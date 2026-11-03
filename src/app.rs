use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Rect, ScrollArea, Sense, Shape, TextureHandle,
    TextureOptions, Theme, Vec2, pos2, vec2,
};

use crate::annot::{Annotations, Stroke};
use crate::commands::{self, Command};
use crate::palette::{GoTo, Outcome, Palette};
use crate::render::{Job, Renderer, Reply};

const PAGE_GAP: f32 = 12.0;
const ZOOM_STEP: f32 = 1.25;
const ZOOM_RANGE: (f32, f32) = (0.25, 5.0);
/// Largest texture side we ask for; GPUs commonly cap at 8192 to 16384.
const MAX_TEXTURE_SIDE: f32 = 8192.0;
const PEN_COLOR: [u8; 4] = [229, 72, 77, 255];
/// In PDF points.
const PEN_WIDTH: f32 = 1.5;
/// In screen points.
const ERASER_RADIUS: f32 = 6.0;

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Hand,
    Pen,
    Eraser,
}

enum Edit {
    Added,
    Erased(usize, Stroke),
}

struct OpenDoc {
    key: String,
    /// Page sizes in PDF points.
    sizes: Vec<Vec2>,
    textures: HashMap<usize, (Job, TextureHandle)>,
    annots: Annotations,
    history: Vec<Edit>,
    drawing: Option<Stroke>,
    current: usize,
}

pub struct App {
    renderer: Renderer,
    doc: Option<OpenDoc>,
    dark: bool,
    zoom: f32,
    tool: Tool,
    palette: Option<Palette>,
    goto: Option<GoTo>,
    error: Option<String>,
    scroll_to: Option<usize>,
    /// Last frame's scroll offset and visible rect, for zooming around the cursor.
    view: Option<(Vec2, Rect)>,
    /// Last list sent to the worker, so we only send again when it changes.
    wanted: Vec<Job>,
    /// Set with `--bench-startup`: when `main` started.
    bench: Option<Instant>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        renderer: Renderer,
        bench: Option<Instant>,
    ) -> Self {
        let ctx = &cc.egui_ctx;
        let _ = renderer.ctx.set(ctx.clone());
        // Ctrl +/-/0 zoom the PDF, not egui's whole UI.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let dark = cc
            .storage
            .and_then(|s| eframe::get_value(s, "dark"))
            .unwrap_or(ctx.system_theme() == Some(Theme::Dark));
        ctx.set_theme(theme(dark));
        Self {
            renderer,
            doc: None,
            dark,
            zoom: 1.0,
            tool: Tool::Hand,
            palette: None,
            goto: None,
            error: None,
            scroll_to: None,
            view: None,
            wanted: Vec::new(),
            bench,
        }
    }

    fn run(&mut self, cmd: Command, ctx: &egui::Context) {
        let current = self.doc.as_ref().map_or(0, |d| d.current);
        let last = self
            .doc
            .as_ref()
            .map_or(0, |d| d.sizes.len().saturating_sub(1));
        match cmd {
            Command::Palette => self.palette = Some(Palette::default()),
            Command::Open => {
                // Waiting on the dialog here would freeze the UI and the OS would flag us as
                // "not responding". rfd wants it created on the main thread, awaited elsewhere.
                let dialog = rfd::AsyncFileDialog::new()
                    .add_filter("PDF", &["pdf"])
                    .pick_file();
                let open = self.renderer.opener();
                std::thread::spawn(move || {
                    if let Some(file) = pollster::block_on(dialog) {
                        open(file.path().to_path_buf());
                    }
                });
            }
            Command::NextPage => self.scroll_to = Some((current + 1).min(last)),
            Command::PrevPage => self.scroll_to = Some(current.saturating_sub(1)),
            Command::FirstPage => self.scroll_to = Some(0),
            Command::LastPage => self.scroll_to = Some(last),
            Command::GoToPage if self.doc.is_some() => self.goto = Some(GoTo::default()),
            Command::GoToPage => {}
            Command::TogglePen => self.toggle_tool(Tool::Pen),
            Command::ToggleEraser => self.toggle_tool(Tool::Eraser),
            Command::Undo => self.undo(),
            Command::ZoomIn => self.set_zoom(self.zoom * ZOOM_STEP),
            Command::ZoomOut => self.set_zoom(self.zoom / ZOOM_STEP),
            Command::ZoomReset => self.set_zoom(1.0),
            Command::ToggleDark => {
                self.dark = !self.dark;
                ctx.set_theme(theme(self.dark));
            }
        }
    }

    fn toggle_tool(&mut self, tool: Tool) {
        self.tool = if self.tool == tool { Tool::Hand } else { tool };
    }

    fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
        // Keep the current page in view after the layout changes.
        self.scroll_to = self.doc.as_ref().map(|d| d.current);
    }

    fn undo(&mut self) {
        let Some(doc) = &mut self.doc else { return };
        match doc.history.pop() {
            Some(Edit::Added) => {
                doc.annots.strokes.pop();
            }
            Some(Edit::Erased(idx, stroke)) => doc.annots.strokes.insert(idx, stroke),
            None => return,
        }
        self.save();
    }

    fn save(&mut self) {
        if let Some(doc) = &self.doc
            && let Err(e) = doc.annots.save(&doc.key)
        {
            self.error = Some(format!("Could not save annotations: {e}"));
        }
    }

    fn poll_renderer(&mut self, ctx: &egui::Context) {
        while let Ok(reply) = self.renderer.rx.try_recv() {
            match reply {
                Reply::Opened { path, key, sizes } => self.opened(ctx, path, key, sizes),
                Reply::Failed(e) => self.error = Some(e),
                Reply::Page(job, image) => {
                    let Some(doc) = &mut self.doc else { continue };
                    let texture = ctx.load_texture(
                        format!("page-{}", job.page),
                        image,
                        TextureOptions::LINEAR,
                    );
                    doc.textures.insert(job.page, (job, texture));
                    if let Some(start) = self.bench.take() {
                        println!(
                            "first page ready in {:.1} ms",
                            start.elapsed().as_secs_f64() * 1000.0
                        );
                    }
                }
            }
        }
    }

    fn opened(&mut self, ctx: &egui::Context, path: PathBuf, key: String, sizes: Vec<Vec2>) {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{name} - justread")));
        self.doc = Some(OpenDoc {
            annots: Annotations::load(&key),
            key,
            sizes,
            textures: HashMap::new(),
            history: Vec::new(),
            drawing: None,
            current: 0,
        });
        self.error = None;
        self.wanted.clear();
        self.scroll_to = Some(0);
    }

    fn pages_ui(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = &mut self.doc else {
            ui.centered_and_justified(|ui| {
                ui.weak("Open a PDF - Ctrl+O  ·  Ctrl+K for commands");
            });
            return;
        };

        let (mut tops, mut content) = layout(&doc.sizes, self.zoom);

        // Ctrl + scroll (or a touchpad pinch) zooms, keeping the spot under the cursor in place.
        let mut new_offset = None;
        let (zoom_delta, hover) = ui.input(|i| (i.zoom_delta(), i.pointer.hover_pos()));
        if zoom_delta != 1.0
            && let (Some(pos), Some((offset, inner))) = (hover, self.view)
            && inner.contains(pos)
        {
            let new_zoom = (self.zoom * zoom_delta).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
            let ratio = new_zoom / self.zoom;
            new_offset = Some(zoom_offset(&tops, offset, pos - inner.min, ratio));
            self.zoom = new_zoom;
            (tops, content) = layout(&doc.sizes, self.zoom);
        }

        let zoom = self.zoom;
        let pixels_per_point = ui.ctx().pixels_per_point();
        let mut area = ScrollArea::both().auto_shrink(false);
        if let Some(page) = self.scroll_to.take().filter(|&p| p < tops.len()) {
            area = area.vertical_scroll_offset(tops[page] - PAGE_GAP);
        } else if let Some(offset) = new_offset {
            area = area.scroll_offset(offset);
        }

        let mut wanted = Vec::new();
        let mut changed = false;
        let output = area.show_viewport(ui, |ui, viewport| {
            ui.set_min_size(content);
            let origin = ui.min_rect().min;
            let center_x = origin.x + content.x.max(viewport.width()) / 2.0;
            let page_fill = if self.dark {
                Color32::BLACK
            } else {
                Color32::WHITE
            };

            doc.current = tops
                .iter()
                .rposition(|&top| top <= viewport.min.y + viewport.height() / 3.0)
                .unwrap_or(0);

            let visible: Vec<usize> = (0..doc.sizes.len())
                .filter(|&i| {
                    tops[i] < viewport.max.y && tops[i] + doc.sizes[i].y * zoom > viewport.min.y
                })
                .collect();
            let (Some(&first), Some(&last)) = (visible.first(), visible.last()) else {
                return;
            };
            // Also render one page either side, so it's ready before it scrolls into view.
            let around = [first.checked_sub(1), Some(last + 1)].into_iter().flatten();

            for i in visible
                .iter()
                .copied()
                .chain(around)
                .filter(|&i| i < doc.sizes.len())
            {
                let size = doc.sizes[i];
                let scale = (zoom * pixels_per_point).min(MAX_TEXTURE_SIDE / size.x.max(size.y));
                let job = Job {
                    page: i,
                    scale,
                    invert: self.dark,
                };
                if doc.textures.get(&i).map(|(j, _)| *j) != Some(job) {
                    wanted.push(job);
                }
            }
            doc.textures.retain(|&i, _| i + 3 >= first && i <= last + 3);

            for &i in &visible {
                let rect = Rect::from_min_size(
                    pos2(center_x - doc.sizes[i].x * zoom / 2.0, origin.y + tops[i]),
                    doc.sizes[i] * zoom,
                );
                let painter = ui.painter_at(rect);
                // A texture at the wrong zoom is fine to show until the sharp one arrives,
                // but one in the wrong colors (light vs dark) would flash.
                match doc
                    .textures
                    .get(&i)
                    .filter(|(job, _)| job.invert == self.dark)
                {
                    Some((_, tex)) => {
                        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                        painter.image(tex.id(), rect, uv, Color32::WHITE);
                    }
                    None => {
                        painter.rect_filled(rect, 0.0, page_fill);
                    }
                }

                changed |= page_input(ui, doc, self.tool, i, rect, zoom);

                let strokes = doc
                    .annots
                    .strokes
                    .iter()
                    .chain(&doc.drawing)
                    .filter(|s| s.page == i);
                for stroke in strokes {
                    paint_stroke(&painter, stroke, rect, zoom);
                }
            }
        });
        self.view = Some((output.state.clamped_offset(), output.inner_rect));

        if changed {
            self.save();
        }
        if wanted != self.wanted {
            self.renderer.want(wanted.clone());
            self.wanted = wanted;
        }
    }

    fn status_ui(&self, ui: &egui::Ui) {
        let rect = ui.max_rect().shrink(8.0);
        let painter = ui.painter();
        let font = FontId::proportional(12.0);
        if let Some(doc) = &self.doc {
            let mut status = format!("{} / {}", doc.current + 1, doc.sizes.len());
            if self.zoom != 1.0 {
                status += &format!("  ·  {:.0}%", self.zoom * 100.0);
            }
            match self.tool {
                Tool::Hand => {}
                Tool::Pen => status += "  ·  pen",
                Tool::Eraser => status += "  ·  eraser",
            }
            painter.text(
                rect.right_bottom(),
                Align2::RIGHT_BOTTOM,
                status,
                font.clone(),
                ui.visuals().weak_text_color(),
            );
        }
        if let Some(error) = &self.error {
            painter.text(
                rect.left_bottom(),
                Align2::LEFT_BOTTOM,
                error,
                font,
                ui.visuals().error_fg_color,
            );
        }
    }
}

fn theme(dark: bool) -> Theme {
    if dark { Theme::Dark } else { Theme::Light }
}

/// Top of each page and the total content size, in screen points.
fn layout(sizes: &[Vec2], zoom: f32) -> (Vec<f32>, Vec2) {
    let mut tops = Vec::with_capacity(sizes.len());
    let mut y = PAGE_GAP;
    for size in sizes {
        tops.push(y);
        y += size.y * zoom + PAGE_GAP;
    }
    let width = sizes.iter().fold(0.0_f32, |w, s| w.max(s.x)) * zoom + 2.0 * PAGE_GAP;
    (tops, vec2(width, y))
}

/// The scroll offset that keeps the content under `cursor` (relative to the view) in place
/// after zooming by `ratio`. `tops` is the layout before the zoom.
fn zoom_offset(tops: &[f32], offset: Vec2, cursor: Vec2, ratio: f32) -> Vec2 {
    let at = offset + cursor;
    // Gaps between pages don't scale, so take the ones above the cursor out first.
    let gaps = tops.iter().filter(|&&top| top <= at.y).count().max(1) as f32 * PAGE_GAP;
    let new_at = vec2(
        PAGE_GAP + (at.x - PAGE_GAP) * ratio,
        gaps + (at.y - gaps) * ratio,
    );
    new_at - cursor
}

/// Returns true if the annotations changed.
fn page_input(
    ui: &mut egui::Ui,
    doc: &mut OpenDoc,
    tool: Tool,
    page: usize,
    rect: Rect,
    zoom: f32,
) -> bool {
    if tool == Tool::Hand {
        return false;
    }
    let response = ui
        .interact(
            rect,
            ui.scope_id().with(("page", page)),
            Sense::click_and_drag(),
        )
        .on_hover_cursor(CursorIcon::Crosshair);
    let to_page = |pos: egui::Pos2| {
        let p = (pos - rect.min) / zoom;
        [p.x, p.y]
    };

    match tool {
        Tool::Pen => {
            if response.drag_started() {
                let origin = ui.input(|i| i.pointer.press_origin());
                doc.drawing = Some(Stroke {
                    page,
                    color: PEN_COLOR,
                    width: PEN_WIDTH,
                    points: origin.into_iter().map(to_page).collect(),
                });
            }
            if let Some(stroke) = &mut doc.drawing
                && stroke.page == page
                && response.dragged()
            {
                // Use every pointer event, not just one per frame, for smooth lines.
                ui.input(|i| {
                    for event in &i.events {
                        if let egui::Event::PointerMoved(pos) = event {
                            stroke.points.push(to_page(*pos));
                        }
                    }
                });
            }
            if response.drag_stopped()
                && let Some(stroke) = doc.drawing.take()
            {
                doc.annots.strokes.push(stroke);
                doc.history.push(Edit::Added);
                return true;
            }
            false
        }
        Tool::Eraser => {
            if !(response.is_pointer_button_down_on() || response.dragged()) {
                return false;
            }
            let Some(pos) = response.interact_pointer_pos() else {
                return false;
            };
            let Some(idx) = doc.annots.hit(page, to_page(pos), ERASER_RADIUS / zoom) else {
                return false;
            };
            let stroke = doc.annots.strokes.remove(idx);
            doc.history.push(Edit::Erased(idx, stroke));
            true
        }
        Tool::Hand => false,
    }
}

fn paint_stroke(painter: &egui::Painter, stroke: &Stroke, rect: Rect, zoom: f32) {
    let [r, g, b, a] = stroke.color;
    let color = Color32::from_rgba_unmultiplied(r, g, b, a);
    let points: Vec<_> = stroke
        .points
        .iter()
        .map(|p| rect.min + vec2(p[0], p[1]) * zoom)
        .collect();
    match points.as_slice() {
        [] => {}
        [dot] => {
            painter.circle_filled(*dot, stroke.width * zoom / 2.0, color);
        }
        _ => {
            painter.add(Shape::line(
                points,
                egui::Stroke::new(stroke.width * zoom, color),
            ));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_renderer(&ctx);

        if let Some(path) =
            ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()))
        {
            self.renderer.open(path);
        }

        if let Some(palette) = &mut self.palette {
            match palette.show(&ctx) {
                Outcome::Open => {}
                Outcome::Close => self.palette = None,
                Outcome::Done(cmd) => {
                    self.palette = None;
                    self.run(cmd, &ctx);
                }
            }
        } else if let Some(goto) = &mut self.goto {
            let pages = self.doc.as_ref().map_or(0, |d| d.sizes.len());
            match goto.show(&ctx, pages) {
                Outcome::Open => {}
                Outcome::Close => self.goto = None,
                Outcome::Done(page) => {
                    self.goto = None;
                    self.scroll_to = Some(page);
                }
            }
        } else if let Some(cmd) = commands::from_keyboard(&ctx) {
            self.run(cmd, &ctx);
        } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.tool = Tool::Hand;
        }

        let background = if self.dark {
            Color32::from_gray(24)
        } else {
            Color32::from_gray(228)
        };
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::NONE.fill(background))
            .show(ui, |ui| {
                self.pages_ui(ui);
                self.status_ui(ui);
            });
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "dark", &self.dark);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_stacks_pages_with_gaps() {
        let sizes = [vec2(100.0, 200.0), vec2(300.0, 100.0)];
        let (tops, content) = layout(&sizes, 2.0);
        assert_eq!(tops, vec![PAGE_GAP, PAGE_GAP + 400.0 + PAGE_GAP]);
        assert_eq!(content.x, 300.0 * 2.0 + 2.0 * PAGE_GAP);
        assert_eq!(content.y, tops[1] + 200.0 + PAGE_GAP);
    }

    #[test]
    fn zoom_keeps_the_spot_under_the_cursor() {
        let sizes = [vec2(600.0, 800.0); 3];
        let (old_zoom, new_zoom) = (1.0, 1.5);
        let (old_tops, _) = layout(&sizes, old_zoom);
        let (new_tops, _) = layout(&sizes, new_zoom);

        // Cursor over page 2, 476pt down and 188pt across it.
        let (page, local) = (1, vec2(188.0, 476.0));
        let cursor = vec2(200.0, 300.0);
        let offset = vec2(PAGE_GAP, old_tops[page]) + local * old_zoom - cursor;

        let new_offset = zoom_offset(&old_tops, offset, cursor, new_zoom / old_zoom);

        let expected = vec2(PAGE_GAP, new_tops[page]) + local * new_zoom;
        let actual = new_offset + cursor;
        assert!(
            (actual - expected).length() < 0.01,
            "{actual:?} != {expected:?}"
        );
    }
}
