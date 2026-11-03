//! Find in document: the search box and the matches found so far. The searching itself
//! happens on the render thread, which sends matches back a page at a time.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Id, Key, Modifiers, Rect, Sense, Shape, Stroke, TextEdit, vec2};

use crate::render::Hit;

/// How long typing must pause before a search starts, so each keystroke doesn't start one.
const DELAY: Duration = Duration::from_millis(200);

pub enum Event {
    None,
    /// Scroll so this match (page, first rect) is in view.
    Reveal(usize, Rect),
    Close,
}

/// Kept while closed, so reopening shows the last query.
#[derive(Default)]
pub struct Find {
    pub open: bool,
    query: String,
    /// Bumped for every search, so matches still arriving from an older one are dropped.
    pub id: u64,
    /// When to start searching for the current query.
    search_at: Option<Instant>,
    hits: BTreeMap<usize, Vec<Hit>>,
    done: bool,
    /// The selected match: page, and index among that page's matches.
    current: Option<(usize, usize)>,
    focus: bool,
}

impl Find {
    pub fn input_id() -> Id {
        Id::unique("find-input")
    }

    pub fn open(&mut self) {
        self.open = true;
        self.focus = true;
        self.search_at = Some(Instant::now());
    }

    /// Returns the id to send the render thread so it stops searching.
    pub fn close(&mut self) -> u64 {
        self.open = false;
        self.search_at = None;
        self.reset()
    }

    /// For a newly opened document: drops the old matches and searches it again.
    pub fn restart(&mut self) {
        self.reset();
        if self.open {
            self.search_at = Some(Instant::now());
        }
    }

    /// The query to search for now, if a search is due, with its id.
    pub fn due(&mut self) -> Option<(u64, String)> {
        if !self.open || self.search_at.is_none_or(|at| at > Instant::now()) {
            return None;
        }
        self.search_at = None;
        let id = self.reset();
        self.done = self.query.is_empty();
        Some((id, self.query.clone()))
    }

    fn reset(&mut self) -> u64 {
        self.id += 1;
        self.hits.clear();
        self.current = None;
        self.done = false;
        self.id
    }

    pub fn found(&mut self, id: u64, page: usize, hits: Vec<Hit>) -> Event {
        if id != self.id {
            return Event::None;
        }
        self.hits.insert(page, hits);
        // The search starts on the page in view, so its first matches are the nearest ones.
        if self.current.is_none() {
            self.current = Some((page, 0));
            return self.reveal();
        }
        Event::None
    }

    pub fn finished(&mut self, id: u64) {
        if id == self.id {
            self.done = true;
        }
    }

    pub fn hits_on(&self, page: usize) -> &[Hit] {
        self.hits.get(&page).map_or(&[], Vec::as_slice)
    }

    pub fn is_current(&self, page: usize, index: usize) -> bool {
        self.current == Some((page, index))
    }

    fn step(&mut self, forward: bool) -> Event {
        let Some((page, index)) = self.current else {
            return Event::None;
        };
        let all = || {
            self.hits
                .iter()
                .flat_map(|(&p, hits)| (0..hits.len()).map(move |i| (p, i)))
        };
        self.current = if forward {
            all().find(|&m| m > (page, index)).or_else(|| all().next())
        } else {
            all()
                .rev()
                .find(|&m| m < (page, index))
                .or_else(|| all().next_back())
        };
        self.reveal()
    }

    fn reveal(&self) -> Event {
        match self.current {
            Some((page, index)) => Event::Reveal(page, self.hits[&page][index][0]),
            None => Event::None,
        }
    }

    fn status(&self) -> String {
        let searching = if self.done { "" } else { "…" };
        match self.current {
            _ if self.query.is_empty() => String::new(),
            None if self.done => "No matches".into(),
            None => "Searching…".into(),
            Some((page, index)) => {
                let before: usize = self.hits.range(..page).map(|(_, h)| h.len()).sum();
                let total: usize = self.hits.values().map(Vec::len).sum();
                format!("{} / {total}{searching}", before + index + 1)
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Event {
        // Handled before the text box sees them: it would give up focus on Enter, and move
        // the cursor to the start or end on the arrows. Esc is handled by the app, since egui
        // unfocuses the box on Esc before we'd see it here.
        let mut event = Event::None;
        if ctx.memory(|m| m.has_focus(Self::input_id())) {
            ctx.input_mut(|i| {
                if i.consume_key(Modifiers::SHIFT, Key::Enter)
                    || i.consume_key(Modifiers::NONE, Key::ArrowUp)
                {
                    event = self.step(false);
                } else if i.consume_key(Modifiers::NONE, Key::Enter)
                    || i.consume_key(Modifiers::NONE, Key::ArrowDown)
                {
                    event = self.step(true);
                }
            });
        }

        egui::Area::new(Id::unique("find"))
            .anchor(Align2::RIGHT_TOP, vec2(-12.0, 12.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let input = ui.add(
                            TextEdit::singleline(&mut self.query)
                                .id(Self::input_id())
                                .hint_text("Find")
                                .desired_width(200.0),
                        );
                        if std::mem::take(&mut self.focus) {
                            input.request_focus();
                        }
                        if input.changed() {
                            self.search_at = Some(Instant::now() + DELAY);
                        }
                        ui.weak(self.status());
                        if icon_button(ui, Icon::Up, "Previous (Shift+Enter or Up)") {
                            event = self.step(false);
                        }
                        if icon_button(ui, Icon::Down, "Next (Enter or Down)") {
                            event = self.step(true);
                        }
                        if icon_button(ui, Icon::Close, "Close (Esc)") {
                            event = Event::Close;
                        }
                    });
                });
            });

        if let Some(at) = self.search_at {
            ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));
        }
        event
    }
}

enum Icon {
    Up,
    Down,
    Close,
}

/// Drawn rather than typed: egui's bundled fonts only have arrow and cross glyphs with its
/// emoji fonts enabled, which would add about 1 MB to the binary.
fn icon_button(ui: &mut egui::Ui, icon: Icon, hint: &str) -> bool {
    let size = ui.spacing().interact_size.y;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let response = response.on_hover_text(hint);
    let visuals = ui.style().interact(&response);
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
    }
    let (c, r) = (rect.center(), size * 0.2);
    let stroke = Stroke::new(1.5, visuals.fg_stroke.color);
    match icon {
        Icon::Up => painter.add(Shape::line(
            vec![
                c + vec2(-r, r / 2.0),
                c + vec2(0.0, -r / 2.0),
                c + vec2(r, r / 2.0),
            ],
            stroke,
        )),
        Icon::Down => painter.add(Shape::line(
            vec![
                c + vec2(-r, -r / 2.0),
                c + vec2(0.0, r / 2.0),
                c + vec2(r, -r / 2.0),
            ],
            stroke,
        )),
        Icon::Close => {
            painter.line_segment([c + vec2(-r, -r), c + vec2(r, r)], stroke);
            painter.line_segment([c + vec2(-r, r), c + vec2(r, -r)], stroke)
        }
    };
    response.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(x: f32) -> Hit {
        vec![Rect::from_min_size(egui::pos2(x, 0.0), vec2(10.0, 10.0))]
    }

    /// A search for "a" that has found two matches on page 3 and one on page 1.
    fn searched() -> Find {
        let mut find = Find {
            open: true,
            query: "a".into(),
            search_at: Some(Instant::now()),
            ..Find::default()
        };
        let (id, _) = find.due().unwrap();
        find.found(id, 3, vec![hit(0.0), hit(50.0)]);
        find.found(id, 1, vec![hit(20.0)]);
        find
    }

    #[test]
    fn first_match_found_is_selected() {
        let find = searched();
        assert!(find.is_current(3, 0));
        assert_eq!(find.status(), "2 / 3…");
    }

    #[test]
    fn next_and_previous_go_in_document_order_and_wrap() {
        let mut find = searched();
        find.step(true);
        assert!(find.is_current(3, 1));
        find.step(true);
        assert!(find.is_current(1, 0), "wraps to the start");
        find.step(false);
        assert!(find.is_current(3, 1), "wraps to the end");
    }

    #[test]
    fn matches_from_an_older_search_are_dropped() {
        let mut find = searched();
        let old = find.id;
        find.search_at = Some(Instant::now());
        let (new, _) = find.due().unwrap();
        find.found(old, 5, vec![hit(0.0)]);
        assert!(find.hits_on(5).is_empty());
        find.finished(new);
        assert_eq!(find.status(), "No matches");
    }
}
