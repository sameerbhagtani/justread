//! A background thread that owns the MuPDF document and renders and searches pages on
//! request, so the UI never blocks on PDF work.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, OnceLock};

use eframe::egui::{self, ColorImage};
use mupdf::text_page::SearchHitResponse;
use mupdf::{Colorspace, Document, Matrix, TextPageFlags};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Job {
    pub page: usize,
    /// Pixels per PDF point.
    pub scale: f32,
    pub invert: bool,
}

/// One search match: the rectangles it covers (several if it wraps onto the next line),
/// in PDF points from the page's top-left.
pub type Hit = Vec<egui::Rect>;

enum Request {
    Open(PathBuf),
    /// Replaces the list of pages the UI wants rendered, most important first.
    /// Ignored if its generation is from an earlier document.
    Want(u64, Vec<Job>),
    /// Replaces any running search with one starting at page `from`. An empty query just
    /// stops it.
    Find {
        generation: u64,
        id: u64,
        query: String,
        from: usize,
    },
}

pub enum Reply {
    Opened {
        /// Goes up by one with each document opened.
        generation: u64,
        path: PathBuf,
        key: String,
        /// Page sizes in PDF points.
        sizes: Vec<egui::Vec2>,
    },
    Failed(String),
    Page(Job, ColorImage),
    /// Matches on one page, in reading order. Pages without any aren't sent.
    Found {
        id: u64,
        page: usize,
        hits: Vec<Hit>,
    },
    /// The search with this id has been through every page.
    FindDone(u64),
}

struct Search {
    id: u64,
    query: String,
    page: usize,
    pages_left: usize,
    page_count: usize,
}

pub struct Renderer {
    tx: Sender<Request>,
    pub rx: Receiver<Reply>,
    /// Set once the UI exists, so the worker can wake it when a reply is ready.
    pub ctx: Arc<OnceLock<egui::Context>>,
}

impl Renderer {
    pub fn spawn() -> Self {
        let (tx, req_rx) = mpsc::channel();
        let (reply_tx, rx) = mpsc::channel();
        let ctx = Arc::new(OnceLock::new());
        let worker_ctx = Arc::clone(&ctx);
        std::thread::spawn(move || worker(req_rx, reply_tx, worker_ctx));
        Self { tx, rx, ctx }
    }

    pub fn open(&self, path: PathBuf) {
        let _ = self.tx.send(Request::Open(path));
    }

    pub fn opener(&self) -> impl FnOnce(PathBuf) + Send + 'static {
        let tx = self.tx.clone();
        move |path| {
            let _ = tx.send(Request::Open(path));
        }
    }

    pub fn want(&self, generation: u64, jobs: Vec<Job>) {
        let _ = self.tx.send(Request::Want(generation, jobs));
    }

    pub fn find(&self, generation: u64, id: u64, query: String, from: usize) {
        let _ = self.tx.send(Request::Find {
            generation,
            id,
            query,
            from,
        });
    }
}

fn worker(rx: Receiver<Request>, tx: Sender<Reply>, ctx: Arc<OnceLock<egui::Context>>) {
    let mut doc: Option<Document> = None;
    let mut generation = 0;
    let mut queue: VecDeque<Job> = VecDeque::new();
    let mut search: Option<Search> = None;

    let send = |reply: Reply| {
        let _ = tx.send(reply);
        if let Some(ctx) = ctx.get() {
            ctx.request_repaint();
        }
    };

    loop {
        // Block when idle; otherwise just pick up whatever arrived since the last page.
        let next = if queue.is_empty() && search.is_none() {
            match rx.recv() {
                Ok(req) => Some(req),
                Err(_) => return, // UI is gone
            }
        } else {
            match rx.try_recv() {
                Ok(req) => Some(req),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        };
        match next {
            Some(Request::Open(path)) => {
                // On failure keep the old document: the UI is still showing it.
                match open(&path) {
                    Ok((d, key, sizes)) => {
                        queue.clear();
                        search = None;
                        doc = Some(d);
                        generation += 1;
                        send(Reply::Opened {
                            generation,
                            path,
                            key,
                            sizes,
                        });
                        // Head start while the UI is still starting: render page 1 at a guessed
                        // scale. Even if the guess is off, MuPDF's font/image caches are now warm.
                        queue.push_back(Job {
                            page: 0,
                            scale: 1.0,
                            invert: false,
                        });
                    }
                    Err(e) => send(Reply::Failed(e)),
                }
                continue;
            }
            Some(Request::Want(for_generation, jobs)) => {
                if for_generation == generation {
                    queue = jobs.into();
                }
                continue;
            }
            Some(Request::Find {
                generation: for_generation,
                id,
                query,
                from,
            }) => {
                if for_generation == generation {
                    let page_count = doc.as_ref().map_or(0, |d| d.page_count().unwrap_or(0));
                    search = (!query.is_empty() && page_count > 0).then(|| Search {
                        id,
                        query,
                        page: from % page_count as usize,
                        pages_left: page_count as usize,
                        page_count: page_count as usize,
                    });
                }
                continue;
            }
            None => {}
        }

        let Some(doc) = &doc else {
            queue.clear();
            search = None;
            continue;
        };
        // Rendering comes first, so searching never delays the pages on screen. Searching
        // goes one page at a time, so new requests are picked up between pages.
        if let Some(job) = queue.pop_front() {
            match render(doc, job) {
                Ok(image) => send(Reply::Page(job, image)),
                Err(e) => send(Reply::Failed(format!("page {}: {e}", job.page + 1))),
            }
        } else if let Some(s) = &mut search {
            // A page that fails to load just has no matches; an error per page would be noise.
            if let Ok(hits) = find(doc, s.page, &s.query)
                && !hits.is_empty()
            {
                send(Reply::Found {
                    id: s.id,
                    page: s.page,
                    hits,
                });
            }
            s.page = (s.page + 1) % s.page_count;
            s.pages_left -= 1;
            if s.pages_left == 0 {
                send(Reply::FindDone(s.id));
                search = None;
            }
        }
    }
}

fn open(path: &Path) -> Result<(Document, String, Vec<egui::Vec2>), String> {
    let is_pdf = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
    if !is_pdf {
        return Err("Only PDF files are supported".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let doc = Document::from_bytes(&bytes, "application/pdf").map_err(|e| e.to_string())?;
    if doc.needs_password().unwrap_or(false) {
        return Err("Password-protected PDFs are not supported".into());
    }
    let count = doc.page_count().map_err(|e| e.to_string())?;
    let sizes = (0..count)
        .map(|i| {
            let b = doc.load_page(i)?.bounds()?;
            Ok(egui::vec2(b.width(), b.height()))
        })
        .collect::<Result<_, mupdf::Error>>()
        .map_err(|e| e.to_string())?;
    Ok((doc, crate::annot::key(&bytes), sizes))
}

fn render(doc: &Document, job: Job) -> Result<ColorImage, mupdf::Error> {
    let page = doc.load_page(job.page as i32)?;
    let matrix = Matrix::new_scale(job.scale, job.scale);
    let mut pixmap = page.to_pixmap(&matrix, &Colorspace::device_rgb(), false, false)?;
    if job.invert {
        pixmap.invert()?;
    }
    let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);
    let stride = pixmap.stride() as usize;
    let samples = pixmap.samples();
    // Rows may be padded; copy just the RGB bytes of each row.
    let mut rgb = Vec::with_capacity(w * h * 3);
    for row in samples.chunks(stride).take(h) {
        rgb.extend_from_slice(&row[..w * 3]);
    }
    Ok(ColorImage::from_rgb([w, h], &rgb))
}

fn find(doc: &Document, page: usize, query: &str) -> Result<Vec<Hit>, mupdf::Error> {
    let page = doc.load_page(page as i32)?;
    let bounds = page.bounds()?;
    let origin = egui::vec2(bounds.x0, bounds.y0);
    let mut hits = Vec::new();
    let text = page.to_text_page(TextPageFlags::empty())?;
    text.search_cb(query, &mut hits, |hits, quads| {
        let rects = quads.iter().map(|q| {
            let corners = [&q.ul, &q.ur, &q.ll, &q.lr].map(|p| egui::pos2(p.x, p.y) - origin);
            egui::Rect::from_points(&corners)
        });
        hits.push(rects.collect());
        SearchHitResponse::ContinueSearch
    })?;
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Page 1 is 612x792 with a black 100x100 square in its bottom-left corner,
    /// page 2 is 300x400 and blank.
    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/two-pages.pdf")
    }

    /// Page 1 is 300x200 with "Hello world, hello." on a baseline 150pt from its bottom.
    /// Page 2 is 300x200 with its MediaBox offset by (100, 100), and "say hello" 20pt from
    /// its left and on a baseline 150pt from its bottom.
    fn text_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/text.pdf")
    }

    fn pixel(image: &ColorImage, x: usize, y: usize) -> [u8; 3] {
        let c = image.pixels[y * image.width() + x];
        [c.r(), c.g(), c.b()]
    }

    #[test]
    fn opens_pdf_with_page_sizes() {
        let (_, key, sizes) = open(&fixture()).unwrap();
        assert_eq!(
            sizes,
            vec![egui::vec2(612.0, 792.0), egui::vec2(300.0, 400.0)]
        );
        assert_eq!(key, crate::annot::key(&std::fs::read(fixture()).unwrap()));
    }

    #[test]
    fn rejects_non_pdf_files() {
        let err = open(Path::new("notes.txt")).unwrap_err();
        assert!(err.contains("Only PDF"), "{err}");
        assert!(open(Path::new("missing.pdf")).is_err());
    }

    #[test]
    fn renders_at_the_requested_scale() {
        let (doc, _, _) = open(&fixture()).unwrap();
        let job = Job {
            page: 0,
            scale: 0.5,
            invert: false,
        };
        let image = render(&doc, job).unwrap();
        assert_eq!(image.size, [306, 396]);
        assert_eq!(pixel(&image, 5, 5), [255, 255, 255]);
        assert_eq!(pixel(&image, 5, 390), [0, 0, 0]);
    }

    #[test]
    fn invert_flips_colors_for_dark_mode() {
        let (doc, _, _) = open(&fixture()).unwrap();
        let job = Job {
            page: 0,
            scale: 0.5,
            invert: true,
        };
        let image = render(&doc, job).unwrap();
        assert_eq!(pixel(&image, 5, 5), [0, 0, 0]);
        assert_eq!(pixel(&image, 5, 390), [255, 255, 255]);
    }

    #[test]
    fn worker_opens_and_renders_requested_pages() {
        let renderer = Renderer::spawn();
        let recv = || renderer.rx.recv_timeout(Duration::from_secs(10)).unwrap();

        renderer.open(fixture());
        let Reply::Opened {
            generation, sizes, ..
        } = recv()
        else {
            panic!("expected Opened")
        };
        assert_eq!(sizes.len(), 2);

        let wanted = Job {
            page: 1,
            scale: 1.0,
            invert: false,
        };
        renderer.want(generation, vec![wanted]);
        // The worker may still send its head-start render of page 1 first.
        loop {
            match recv() {
                Reply::Page(job, image) if job == wanted => {
                    assert_eq!(image.size, [300, 400]);
                    break;
                }
                Reply::Page(..) => continue,
                Reply::Failed(e) => panic!("{e}"),
                _ => panic!("unexpected reply"),
            }
        }
    }

    #[test]
    fn failed_open_keeps_the_current_document() {
        let renderer = Renderer::spawn();
        let recv = || renderer.rx.recv_timeout(Duration::from_secs(10)).unwrap();

        renderer.open(fixture());
        let Reply::Opened { generation, .. } = recv() else {
            panic!("expected Opened")
        };
        renderer.open(PathBuf::from("notes.txt"));

        let wanted = Job {
            page: 1,
            scale: 1.0,
            invert: false,
        };
        renderer.want(generation, vec![wanted]);
        let mut failed = false;
        loop {
            match recv() {
                Reply::Page(job, _) if job == wanted => break,
                Reply::Page(..) => continue,
                Reply::Failed(_) => failed = true,
                _ => panic!("unexpected reply"),
            }
        }
        assert!(failed, "the bad file should still report an error");
    }

    #[test]
    fn find_returns_matches_in_page_coordinates() {
        let (doc, _, _) = open(&text_fixture()).unwrap();

        let hits = find(&doc, 0, "hello").unwrap();
        assert_eq!(hits.len(), 2, "case-insensitive: Hello and hello");
        let first = hits[0][0];
        assert!(first.min.x > 15.0 && first.min.x < 25.0, "{first:?}");
        // The baseline is 50pt from the top, so the word sits just above it.
        assert!(first.min.y > 35.0 && first.max.y < 55.0, "{first:?}");
        assert!(hits[1][0].min.x > first.max.x, "in reading order");

        // Same place on page 2 despite its offset MediaBox.
        let hit = find(&doc, 1, "hello").unwrap()[0][0];
        assert!(
            hit.min.x > 35.0 && hit.min.y > 35.0 && hit.max.y < 55.0,
            "{hit:?}"
        );

        assert!(find(&doc, 0, "absent").unwrap().is_empty());
    }

    #[test]
    fn worker_searches_every_page_starting_from_the_given_one() {
        let renderer = Renderer::spawn();
        let recv = || renderer.rx.recv_timeout(Duration::from_secs(10)).unwrap();
        renderer.open(text_fixture());
        let Reply::Opened { generation, .. } = recv() else {
            panic!("expected Opened")
        };

        renderer.find(generation, 7, "hello".into(), 1);
        let mut pages = Vec::new();
        loop {
            match recv() {
                Reply::Found { id, page, hits } => {
                    assert_eq!(id, 7);
                    pages.push((page, hits.len()));
                }
                Reply::FindDone(id) => {
                    assert_eq!(id, 7);
                    break;
                }
                Reply::Page(..) => {}
                Reply::Failed(e) => panic!("{e}"),
                Reply::Opened { .. } => panic!("opened twice"),
            }
        }
        assert_eq!(pages, vec![(1, 1), (0, 2)], "starts at page 2, then wraps");
    }

    #[test]
    fn requests_for_an_older_document_are_ignored() {
        let renderer = Renderer::spawn();
        renderer.open(fixture());
        renderer.open(fixture());

        // Meant for the first document, but it arrives after the second opened.
        let stale = Job {
            page: 1,
            scale: 0.25,
            invert: false,
        };
        renderer.want(1, vec![stale]);

        let mut generations = Vec::new();
        while let Ok(reply) = renderer.rx.recv_timeout(Duration::from_millis(500)) {
            match reply {
                Reply::Opened { generation, .. } => generations.push(generation),
                Reply::Page(job, _) => assert_ne!(job, stale, "rendered a stale request"),
                Reply::Failed(e) => panic!("{e}"),
                _ => panic!("unexpected reply"),
            }
        }
        assert_eq!(generations, vec![1, 2]);
    }
}
