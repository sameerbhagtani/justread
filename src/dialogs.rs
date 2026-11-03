use eframe::egui::{
    self, Button, Color32, CornerRadius, Frame, Id, Key, Margin, Modal, Modifiers, Separator,
    Shadow, Stroke, TextEdit, vec2,
};

use crate::commands::{COMMANDS, Command};

const ROW_HEIGHT: f32 = 32.0;
const PADDING: i8 = 12;

pub fn set_flat_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        let v = &mut style.visuals;
        v.window_corner_radius = CornerRadius::ZERO;
        v.menu_corner_radius = CornerRadius::ZERO;
        v.window_shadow = Shadow::NONE;
        v.popup_shadow = Shadow::NONE;
        let w = &mut v.widgets;
        for state in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            state.corner_radius = CornerRadius::ZERO;
        }
    });
}

/// No padding, so the input and rows inside run edge to edge.
pub fn box_frame(style: &egui::Style) -> Frame {
    Frame::NONE
        .fill(style.visuals.window_fill)
        .stroke(Stroke::new(
            1.0,
            style.visuals.widgets.noninteractive.bg_stroke.color,
        ))
}

pub fn input<'t>(text: &'t mut String, hint: impl Into<egui::WidgetText>) -> TextEdit<'t> {
    TextEdit::singleline(text)
        .hint_text(hint)
        .frame(Frame::NONE.inner_margin(Margin::symmetric(PADDING, 10)))
        .desired_width(f32::INFINITY)
}

fn modal(id: &str) -> Modal {
    Modal::new(Id::unique(id)).backdrop_color(Color32::TRANSPARENT)
}

pub enum Outcome<T> {
    Open,
    Close,
    Done(T),
}

#[derive(Default)]
pub struct Palette {
    query: String,
    selected: usize,
}

impl Palette {
    pub fn show(&mut self, ctx: &egui::Context) -> Outcome<Command> {
        let query = self.query.to_lowercase();
        let matches: Vec<_> = COMMANDS
            .iter()
            .filter(|(_, name, _)| name.to_lowercase().contains(&query))
            .collect();

        let (up, down, enter) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
            )
        });
        if down {
            self.selected += 1;
        }
        if up {
            self.selected = self.selected.saturating_sub(1);
        }
        self.selected = self.selected.min(matches.len().saturating_sub(1));

        let mut clicked = None;
        let frame = box_frame(&ctx.global_style());
        let modal = modal("palette").frame(frame).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            ui.spacing_mut().button_padding = vec2(f32::from(PADDING), 0.0);
            let edit = ui.add(input(&mut self.query, "Type a command…"));
            edit.request_focus();
            if edit.changed() {
                self.selected = 0;
            }
            ui.add(Separator::default().spacing(0.0));
            for (idx, (cmd, name, keys)) in matches.iter().enumerate() {
                let row = Button::selectable(idx == self.selected, *name)
                    .shortcut_text(ctx.format_shortcut(&keys[0]))
                    .min_size(vec2(ui.available_width(), ROW_HEIGHT));
                if ui.add(row).clicked() {
                    clicked = Some(*cmd);
                }
            }
            if matches.is_empty() {
                Frame::NONE
                    .inner_margin(Margin::symmetric(PADDING, 10))
                    .show(ui, |ui| ui.weak("No matching command"));
            }
        });

        if let Some(cmd) = clicked {
            Outcome::Done(cmd)
        } else if enter && let Some((cmd, _, _)) = matches.get(self.selected) {
            Outcome::Done(*cmd)
        } else if modal.should_close() {
            Outcome::Close
        } else {
            Outcome::Open
        }
    }
}

#[derive(Default)]
pub struct GoTo {
    input: String,
}

impl GoTo {
    /// Returns the chosen 0-based page index.
    pub fn show(&mut self, ctx: &egui::Context, page_count: usize) -> Outcome<usize> {
        let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        let frame = box_frame(&ctx.global_style());
        let modal = modal("goto").frame(frame).show(ctx, |ui| {
            ui.set_width(220.0);
            ui.add(input(&mut self.input, format!("Page (1-{page_count})")))
                .request_focus();
        });
        self.input.retain(|c| c.is_ascii_digit());

        if enter && let Some(page) = parse_page(&self.input, page_count) {
            Outcome::Done(page)
        } else if modal.should_close() || enter {
            Outcome::Close
        } else {
            Outcome::Open
        }
    }
}

/// The user types 1-based page numbers; out-of-range ones go to the nearest page.
fn parse_page(input: &str, page_count: usize) -> Option<usize> {
    let n: usize = input.parse().ok()?;
    Some(n.clamp(1, page_count.max(1)) - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_numbers_are_one_based_and_clamped() {
        assert_eq!(parse_page("1", 10), Some(0));
        assert_eq!(parse_page("5", 10), Some(4));
        assert_eq!(parse_page("10", 10), Some(9));
        assert_eq!(parse_page("0", 10), Some(0));
        assert_eq!(parse_page("999", 10), Some(9));
    }

    #[test]
    fn non_numbers_are_rejected() {
        assert_eq!(parse_page("", 10), None);
        assert_eq!(parse_page("abc", 10), None);
    }
}
