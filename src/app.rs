use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, ScrollArea, Sense, Shape, TextureHandle,
    TextureOptions, Theme, Vec2, pos2, vec2,
};

use crate::annot::{Annotations, Stroke};
use crate::commands::{self, Command};
use crate::dialogs::{GoTo, Outcome, Palette};
use crate::render::{Job, Renderer, Reply};

const PAGE_GAP: f32 = 12.0;
const ZOOM_STEP: f32 = 1.25;
const ZOOM_RANGE: (f32, f32) = (0.25, 5.0);
/// How long the zoom must stay put before pages are re-rendered at the new scale.
const ZOOM_SETTLE: Duration = Duration::from_millis(150);
const ERROR_DURATION: Duration = Duration::from_secs(5);
/// Largest texture side we ask for, even if the GPU allows more: past this a single page
/// costs hundreds of MB.
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

/// An undone edit, holding what's needed to apply it again.
enum Undone {
    Added(Stroke),
    Erased(usize),
}

struct OpenDoc {
    generation: u64,
    key: String,
    /// Page sizes in PDF points.
    sizes: Vec<Vec2>,
    textures: HashMap<usize, (Job, TextureHandle)>,
    annots: Annotations,
    /// Why its annotations file couldn't be read. Nothing is saved then, since that would
    /// overwrite the file.
    load_error: Option<String>,
    history: Vec<Edit>,
    redo: Vec<Undone>,
    drawing: Option<Stroke>,
    /// Last eraser position, so a fast swipe erases everything along its path.
    erasing: Option<(usize, [f32; 2])>,
    /// Changed during the current drag. Saved once the mouse is released, not on every
    /// stroke an eraser drag removes.
    unsaved: bool,
    current: usize,
}

impl OpenDoc {
    /// A new edit makes the undone ones meaningless, so it clears what redo would apply.
    fn record(&mut self, edit: Edit) {
        self.history.push(edit);
        self.redo.clear();
    }
}

pub struct App {
    renderer: Renderer,
    doc: Option<OpenDoc>,
    dark: bool,
    zoom: f32,
    tool: Tool,
    palette: Option<Palette>,
    goto: Option<GoTo>,
    /// Shown until `ERROR_DURATION` after the given time.
    error: Option<(String, Instant)>,
    scroll_to: Option<usize>,
    scroll_offset: Option<Vec2>,
    /// When the zoom last changed. Cleared once it settles.
    zoomed_at: Option<Instant>,
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
            scroll_offset: None,
            zoomed_at: None,
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
            Command::Redo => self.redo(),
            Command::ZoomIn => self.zoom_at(self.zoom * ZOOM_STEP, None),
            Command::ZoomOut => self.zoom_at(self.zoom / ZOOM_STEP, None),
            Command::ZoomReset => self.zoom_at(1.0, None),
            Command::ToggleDark => {
                self.dark = !self.dark;
                ctx.set_theme(theme(self.dark));
            }
        }
    }

    fn toggle_tool(&mut self, tool: Tool) {
        self.tool = if self.tool == tool { Tool::Hand } else { tool };
    }

    /// Zooms keeping the content under `anchor` (default: the middle of the view) in place.
    fn zoom_at(&mut self, zoom: f32, anchor: Option<Pos2>) {
        let zoom = zoom.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
        if zoom == self.zoom {
            return;
        }
        self.zoomed_at = Some(Instant::now());
        if let (Some(doc), Some((offset, inner))) = (&self.doc, self.view) {
            let cursor = anchor.unwrap_or(inner.center()) - inner.min;
            let zooms = (self.zoom, zoom);
            self.scroll_offset = Some(zoom_offset(
                &doc.sizes,
                zooms,
                offset,
                cursor,
                inner.width(),
            ));
        }
        self.zoom = zoom;
    }

    fn undo(&mut self) {
        let Some(doc) = &mut self.doc else { return };
        let undone = match doc.history.pop() {
            Some(Edit::Added) => match doc.annots.strokes.pop() {
                Some(stroke) => Undone::Added(stroke),
                None => return,
            },
            Some(Edit::Erased(idx, stroke)) => {
                doc.annots.strokes.insert(idx, stroke);
                Undone::Erased(idx)
            }
            None => return,
        };
        doc.redo.push(undone);
        self.save();
    }

    fn redo(&mut self) {
        let Some(doc) = &mut self.doc else { return };
        let edit = match doc.redo.pop() {
            Some(Undone::Added(stroke)) => {
                doc.annots.strokes.push(stroke);
                Edit::Added
            }
            Some(Undone::Erased(idx)) => Edit::Erased(idx, doc.annots.strokes.remove(idx)),
            None => return,
        };
        doc.history.push(edit);
        self.save();
    }

    fn save(&mut self) {
        if let Some(doc) = &self.doc
            && doc.load_error.is_none()
            && let Err(e) = doc.annots.save(&doc.key)
        {
            self.show_error(format!("Could not save annotations: {e}"));
        }
    }

    fn show_error(&mut self, error: String) {
        self.error = Some((error, Instant::now()));
    }

    fn poll_renderer(&mut self, ctx: &egui::Context) {
        while let Ok(reply) = self.renderer.rx.try_recv() {
            match reply {
                Reply::Opened {
                    generation,
                    path,
                    key,
                    sizes,
                } => self.opened(ctx, generation, path, key, sizes),
                Reply::Failed(e) => self.show_error(e),
                Reply::Page(job, image) => {
                    let Some(doc) = &mut self.doc else { continue };
                    let texture = ctx.load_texture(
                        format!("page-{}", job.page),
                        image,
                        TextureOptions::LINEAR,
                    );
                    doc.textures.insert(job.page, (job, texture));
                }
            }
        }
    }

    fn opened(
        &mut self,
        ctx: &egui::Context,
        generation: u64,
        path: PathBuf,
        key: String,
        sizes: Vec<Vec2>,
    ) {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{name} - justread")));
        self.error = None;
        let (annots, load_error) = match Annotations::load(&key) {
            Ok(annots) => (annots, None),
            Err(e) => (
                Annotations::default(),
                Some(format!(
                    "Could not read annotations, new ones won't be saved: {e}"
                )),
            ),
        };
        self.doc = Some(OpenDoc {
            generation,
            annots,
            load_error,
            key,
            sizes,
            textures: HashMap::new(),
            history: Vec::new(),
            redo: Vec::new(),
            drawing: None,
            erasing: None,
            unsaved: false,
            current: 0,
        });
        self.wanted.clear();
        self.scroll_to = Some(0);
    }

    fn pages_ui(&mut self, ui: &mut egui::Ui) {
        // Ctrl + scroll (or a touchpad pinch) zooms around the cursor.
        let (zoom_delta, hover) = ui.input(|i| (i.zoom_delta(), i.pointer.hover_pos()));
        if zoom_delta != 1.0
            && let (Some(pos), Some((_, inner))) = (hover, self.view)
            && inner.contains(pos)
        {
            self.zoom_at(self.zoom * zoom_delta, Some(pos));
        }

        let Some(doc) = &mut self.doc else {
            ui.centered_and_justified(|ui| {
                ui.weak("Open a PDF - Ctrl+O  ·  Ctrl+K for commands");
            });
            return;
        };

        let zoom = self.zoom;
        let generation = doc.generation;
        let (tops, content) = layout(&doc.sizes, zoom);
        let pixels_per_point = ui.ctx().pixels_per_point();
        let max_side = MAX_TEXTURE_SIDE.min(ui.input(|i| i.max_texture_side) as f32);
        let settle_left = self
            .zoomed_at
            .map(|t| ZOOM_SETTLE.saturating_sub(t.elapsed()))
            .filter(|left| !left.is_zero());
        match settle_left {
            Some(left) => ui.ctx().request_repaint_after(left),
            None => self.zoomed_at = None,
        }
        let zooming = settle_left.is_some();
        let mut area = ScrollArea::both().auto_shrink(false);
        let scroll_offset = self.scroll_offset.take();
        if let Some(page) = self.scroll_to.take().filter(|&p| p < tops.len()) {
            area = area.vertical_scroll_offset(tops[page] - PAGE_GAP);
        } else if let Some(offset) = scroll_offset {
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

            doc.current = current_page(&tops, viewport, content.y);

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
                let scale = (zoom * pixels_per_point).min(max_side / size.x.max(size.y));
                let job = Job {
                    page: i,
                    scale,
                    invert: self.dark,
                };
                let have = doc.textures.get(&i).map(|(j, _)| *j);
                if needs_render(have, job, zooming) {
                    wanted.push(job);
                }
                // Not just any texture: the worker's head start can be at the wrong scale
                // or colors.
                if i == first
                    && have == Some(job)
                    && let Some(start) = self.bench.take()
                {
                    println!(
                        "first page shown in {:.1} ms",
                        start.elapsed().as_secs_f64() * 1000.0
                    );
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

        // The release can happen where page_input doesn't see it: after switching tools
        // mid-stroke, or with the page scrolled out of view.
        let released = !ui.input(|i| i.pointer.primary_down());
        if released {
            changed |= finish_stroke(doc);
            doc.erasing = None;
        }
        doc.unsaved |= changed;

        if released && std::mem::take(&mut doc.unsaved) {
            self.save();
        }
        if wanted != self.wanted {
            self.renderer.want(generation, wanted.clone());
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
        let error = self
            .error
            .as_ref()
            .map(|(e, _)| e)
            .or(self.doc.as_ref().and_then(|d| d.load_error.as_ref()));
        if let Some(error) = error {
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
/// when zooming from `old` to `new`.
fn zoom_offset(
    sizes: &[Vec2],
    (old, new): (f32, f32),
    offset: Vec2,
    cursor: Vec2,
    view_width: f32,
) -> Vec2 {
    let ratio = new / old;
    let (tops, old_content) = layout(sizes, old);
    let (_, new_content) = layout(sizes, new);
    let at = offset + cursor;
    // Gaps between pages don't scale, so take the ones above the cursor out first.
    let gaps = tops.iter().filter(|&&top| top <= at.y).count().max(1) as f32 * PAGE_GAP;
    // Pages are centred, so horizontally it's the distance from the centre that scales.
    let old_center = old_content.x.max(view_width) / 2.0;
    let new_center = new_content.x.max(view_width) / 2.0;
    let new_at = vec2(
        new_center + (at.x - old_center) * ratio,
        gaps + (at.y - gaps) * ratio,
    );
    new_at - cursor
}

/// The page whose top is past the top third of the view. At the bottom of the document
/// it's the last page, since short last pages can never scroll up that far.
fn current_page(tops: &[f32], viewport: Rect, content_height: f32) -> usize {
    let scrollable = content_height > viewport.height();
    if scrollable && viewport.max.y >= content_height - 1.0 {
        return tops.len().saturating_sub(1);
    }
    tops.iter()
        .rposition(|&top| top <= viewport.min.y + viewport.height() / 3.0)
        .unwrap_or(0)
}

/// While zooming, pages that have a texture show it stretched instead of re-rendering at
/// every in-between scale, since a render can't be cancelled once started.
fn needs_render(have: Option<Job>, want: Job, zooming: bool) -> bool {
    match have {
        None => true,
        Some(have) if have.invert != want.invert => true,
        Some(have) => !zooming && have != want,
    }
}

/// Returns true if a stroke was added.
fn finish_stroke(doc: &mut OpenDoc) -> bool {
    let Some(stroke) = doc.drawing.take() else {
        return false;
    };
    doc.annots.strokes.push(stroke);
    doc.record(Edit::Added);
    true
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
            response.drag_stopped() && finish_stroke(doc)
        }
        Tool::Eraser => {
            if !(response.is_pointer_button_down_on() || response.dragged()) {
                return false;
            }
            let Some(pos) = response.interact_pointer_pos() else {
                return false;
            };
            let to = to_page(pos);
            let from = match doc.erasing {
                Some((p, from)) if p == page => from,
                _ => to,
            };
            doc.erasing = Some((page, to));
            let mut erased = false;
            while let Some(idx) = doc.annots.hit(page, from, to, ERASER_RADIUS / zoom) {
                let stroke = doc.annots.strokes.remove(idx);
                doc.record(Edit::Erased(idx, stroke));
                erased = true;
            }
            erased
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

        if let Some((_, shown_at)) = self.error {
            let left = ERROR_DURATION.saturating_sub(shown_at.elapsed());
            if left.is_zero() {
                self.error = None;
            } else {
                ctx.request_repaint_after(left);
            }
        }

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

    /// Where `local` (in PDF points on `page`) ends up in content coordinates.
    fn content_pos(sizes: &[Vec2], zoom: f32, view_width: f32, page: usize, local: Vec2) -> Vec2 {
        let (tops, content) = layout(sizes, zoom);
        let left = content.x.max(view_width) / 2.0 - sizes[page].x * zoom / 2.0;
        vec2(left, tops[page]) + local * zoom
    }

    fn assert_zoom_keeps_spot(sizes: &[Vec2], view_width: f32, zooms: (f32, f32)) {
        // Cursor over page 2, 476pt down and 188pt across it.
        let (page, local) = (1, vec2(188.0, 476.0));
        let cursor = vec2(200.0, 300.0);
        let offset = content_pos(sizes, zooms.0, view_width, page, local) - cursor;

        let new_offset = zoom_offset(sizes, zooms, offset, cursor, view_width);

        let expected = content_pos(sizes, zooms.1, view_width, page, local);
        let actual = new_offset + cursor;
        assert!(
            (actual - expected).length() < 0.01,
            "{actual:?} != {expected:?}"
        );
    }

    #[test]
    fn zoom_keeps_the_spot_under_the_cursor() {
        let sizes = [vec2(600.0, 800.0); 3];
        assert_zoom_keeps_spot(&sizes, 400.0, (1.0, 1.5));
    }

    #[test]
    fn zoom_keeps_the_spot_when_pages_are_narrower_than_the_view() {
        let sizes = [vec2(600.0, 800.0), vec2(300.0, 800.0), vec2(500.0, 800.0)];
        assert_zoom_keeps_spot(&sizes, 1200.0, (1.0, 1.5));
        assert_zoom_keeps_spot(&sizes, 1200.0, (1.0, 3.0));
        assert_zoom_keeps_spot(&sizes, 1200.0, (3.0, 1.0));
    }

    #[test]
    fn zooming_reuses_stretched_textures_until_it_settles() {
        let job = |scale, invert| Job {
            page: 0,
            scale,
            invert,
        };
        let want = job(2.0, false);
        assert!(needs_render(None, want, true), "nothing to stretch");
        assert!(
            needs_render(Some(job(2.0, true)), want, true),
            "wrong colors"
        );
        assert!(!needs_render(Some(job(1.0, false)), want, true));
        assert!(needs_render(Some(job(1.0, false)), want, false));
        assert!(!needs_render(Some(want), want, false));
    }

    #[test]
    fn current_page_follows_the_top_third_of_the_view() {
        let sizes = [vec2(600.0, 800.0); 4];
        let (tops, content) = layout(&sizes, 1.0);
        let view = |top: f32| Rect::from_min_size(pos2(0.0, top), vec2(600.0, 900.0));
        assert_eq!(current_page(&tops, view(0.0), content.y), 0);
        assert_eq!(current_page(&tops, view(tops[1] - 200.0), content.y), 1);
        assert_eq!(current_page(&tops, view(tops[1] - 400.0), content.y), 0);
    }

    #[test]
    fn current_page_is_the_last_at_the_bottom() {
        // Short last pages never reach the top third of a tall view.
        let sizes = [vec2(600.0, 800.0), vec2(600.0, 200.0), vec2(600.0, 200.0)];
        let (tops, content) = layout(&sizes, 1.0);
        let bottom = Rect::from_min_max(pos2(0.0, content.y - 1000.0), pos2(600.0, content.y));
        assert_eq!(current_page(&tops, bottom, content.y), 2);

        // Everything fits: nothing to scroll, so go by the top third as usual.
        let all = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 2000.0));
        assert_eq!(current_page(&tops, all, content.y), 0);
    }
}
