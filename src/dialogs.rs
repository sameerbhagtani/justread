use eframe::egui::{self, Button, Id, Key, Modal, Modifiers, TextEdit};

use crate::commands::{COMMANDS, Command};

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
        let modal = Modal::new(Id::unique("palette")).show(ctx, |ui| {
            ui.set_width(420.0);
            let edit = ui.add(
                TextEdit::singleline(&mut self.query)
                    .hint_text("Type a command…")
                    .desired_width(f32::INFINITY),
            );
            edit.request_focus();
            if edit.changed() {
                self.selected = 0;
            }
            ui.add_space(4.0);
            for (idx, (cmd, name, keys)) in matches.iter().enumerate() {
                let row = Button::selectable(idx == self.selected, *name)
                    .shortcut_text(ctx.format_shortcut(&keys[0]))
                    .min_size(egui::vec2(ui.available_width(), 0.0));
                if ui.add(row).clicked() {
                    clicked = Some(*cmd);
                }
            }
            if matches.is_empty() {
                ui.weak("No matching command");
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
        let modal = Modal::new(Id::unique("goto")).show(ctx, |ui| {
            ui.set_width(220.0);
            ui.add(
                TextEdit::singleline(&mut self.input)
                    .hint_text(format!("Page (1-{page_count})"))
                    .desired_width(f32::INFINITY),
            )
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
