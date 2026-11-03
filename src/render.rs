//! A background thread that owns the MuPDF document and renders pages on request,
//! so the UI never blocks on PDF work.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, OnceLock};

use eframe::egui::{self, ColorImage};
use mupdf::{Colorspace, Document, Matrix};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Job {
    pub page: usize,
    /// Pixels per PDF point.
    pub scale: f32,
    pub invert: bool,
}

enum Request {
    Open(PathBuf),
    /// Replaces the list of pages the UI wants rendered, most important first.
    Want(Vec<Job>),
}

pub enum Reply {
    Opened {
        path: PathBuf,
        key: String,
        /// Page sizes in PDF points.
        sizes: Vec<egui::Vec2>,
    },
    Failed(String),
    Page(Job, ColorImage),
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

    pub fn want(&self, jobs: Vec<Job>) {
        let _ = self.tx.send(Request::Want(jobs));
    }
}

fn worker(rx: Receiver<Request>, tx: Sender<Reply>, ctx: Arc<OnceLock<egui::Context>>) {
    let mut doc: Option<Document> = None;
    let mut queue: VecDeque<Job> = VecDeque::new();

    let send = |reply: Reply| {
        let _ = tx.send(reply);
        if let Some(ctx) = ctx.get() {
            ctx.request_repaint();
        }
    };

    loop {
        // Block when idle; otherwise just pick up whatever arrived since the last page.
        let next = if queue.is_empty() {
            rx.recv().ok()
        } else {
            match rx.try_recv() {
                Ok(req) => Some(req),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        };
        match next {
            Some(Request::Open(path)) => {
                queue.clear();
                doc = None;
                match open(&path) {
                    Ok((d, key, sizes)) => {
                        doc = Some(d);
                        send(Reply::Opened { path, key, sizes });
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
            Some(Request::Want(jobs)) => {
                queue = jobs.into();
                continue;
            }
            None if queue.is_empty() => return, // UI is gone
            None => {}
        }

        let (Some(job), Some(doc)) = (queue.pop_front(), &doc) else {
            continue;
        };
        match render(doc, job) {
            Ok(image) => send(Reply::Page(job, image)),
            Err(e) => send(Reply::Failed(format!("page {}: {e}", job.page + 1))),
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Page 1 is 612x792 with a black 100x100 square in its bottom-left corner,
    /// page 2 is 300x400 and blank.
    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/two-pages.pdf")
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
        let Reply::Opened { sizes, .. } = recv() else {
            panic!("expected Opened")
        };
        assert_eq!(sizes.len(), 2);

        let wanted = Job {
            page: 1,
            scale: 1.0,
            invert: false,
        };
        renderer.want(vec![wanted]);
        // The worker may still send its head-start render of page 1 first.
        loop {
            match recv() {
                Reply::Page(job, image) if job == wanted => {
                    assert_eq!(image.size, [300, 400]);
                    break;
                }
                Reply::Page(..) => continue,
                Reply::Failed(e) => panic!("{e}"),
                Reply::Opened { .. } => panic!("opened twice"),
            }
        }
    }
}
