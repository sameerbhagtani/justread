//! One table drives both the keyboard handling and the command palette, so they can't drift apart.

use eframe::egui::{Context, Key, KeyboardShortcut, Modifiers};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Open,
    NextPage,
    PrevPage,
    FirstPage,
    LastPage,
    GoToPage,
    Find,
    TogglePen,
    ToggleEraser,
    Undo,
    Redo,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    RotateClockwise,
    RotateCounterClockwise,
    ToggleDark,
    Palette,
}

const fn key(modifiers: Modifiers, key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(modifiers, key)
}

const NONE: Modifiers = Modifiers::NONE;
const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);

/// (command, name shown in the palette, shortcuts). The palette displays the first shortcut.
pub const COMMANDS: &[(Command, &str, &[KeyboardShortcut])] = &[
    (
        Command::Palette,
        "Command palette",
        &[key(CMD, Key::K), key(CMD_SHIFT, Key::P)],
    ),
    (Command::Open, "Open PDF…", &[key(CMD, Key::O)]),
    (
        Command::NextPage,
        "Next page",
        &[key(NONE, Key::PageDown), key(NONE, Key::ArrowRight)],
    ),
    (
        Command::PrevPage,
        "Previous page",
        &[key(NONE, Key::PageUp), key(NONE, Key::ArrowLeft)],
    ),
    (Command::FirstPage, "First page", &[key(NONE, Key::Home)]),
    (Command::LastPage, "Last page", &[key(NONE, Key::End)]),
    (Command::GoToPage, "Go to page…", &[key(CMD, Key::G)]),
    (Command::Find, "Find…", &[key(CMD, Key::F)]),
    (Command::TogglePen, "Toggle pen", &[key(NONE, Key::P)]),
    (Command::ToggleEraser, "Toggle eraser", &[key(NONE, Key::E)]),
    // Before Undo: egui ignores extra Shift when matching, so Undo's Ctrl+Z would also
    // catch Ctrl+Shift+Z.
    (
        Command::Redo,
        "Redo annotation",
        &[key(CMD_SHIFT, Key::Z), key(CMD, Key::Y)],
    ),
    (Command::Undo, "Undo annotation", &[key(CMD, Key::Z)]),
    (
        Command::ZoomIn,
        "Zoom in",
        &[key(CMD, Key::Equals), key(CMD, Key::Plus)],
    ),
    (Command::ZoomOut, "Zoom out", &[key(CMD, Key::Minus)]),
    (Command::ZoomReset, "Reset zoom", &[key(CMD, Key::Num0)]),
    (
        Command::RotateCounterClockwise,
        "Rotate page counter-clockwise",
        &[key(CMD_SHIFT, Key::R)],
    ),
    (
        Command::RotateClockwise,
        "Rotate page clockwise",
        &[key(CMD, Key::R)],
    ),
    (Command::ToggleDark, "Toggle dark mode", &[key(CMD, Key::D)]),
];

/// While typing in a text box, plain keys like P are text, and Ctrl+Z/Y undo the typing.
pub fn from_keyboard(ctx: &Context, typing: bool) -> Option<Command> {
    ctx.input_mut(|i| {
        COMMANDS
            .iter()
            .filter(|(cmd, _, _)| !typing || available_while_typing(*cmd))
            .find(|(_, _, keys)| {
                keys.iter()
                    .filter(|k| !typing || k.modifiers.command)
                    .any(|k| i.consume_shortcut(k))
            })
            .map(|(cmd, _, _)| *cmd)
    })
}

fn available_while_typing(cmd: Command) -> bool {
    !matches!(cmd, Command::Undo | Command::Redo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_listed_once_with_a_name_and_shortcut() {
        for (i, (cmd, name, keys)) in COMMANDS.iter().enumerate() {
            assert!(!name.is_empty(), "{cmd:?} has no name");
            assert!(!keys.is_empty(), "{cmd:?} has no shortcut");
            for (other, other_name, _) in &COMMANDS[i + 1..] {
                assert_ne!(cmd, other, "{cmd:?} is listed twice");
                assert_ne!(name, other_name, "two commands are named {name:?}");
            }
        }
    }

    #[test]
    fn no_shortcut_is_caught_by_an_earlier_one() {
        let all: Vec<_> = COMMANDS
            .iter()
            .flat_map(|(cmd, _, keys)| keys.iter().map(move |k| (cmd, k)))
            .collect();
        for (i, (cmd, key)) in all.iter().enumerate() {
            for (earlier, earlier_key) in &all[..i] {
                let caught = earlier_key.logical_key == key.logical_key
                    && key.modifiers.matches_logically(earlier_key.modifiers);
                assert!(!caught, "{cmd:?}'s shortcut also triggers {earlier:?}");
            }
        }
    }

    #[test]
    fn typing_keeps_ctrl_shortcuts_but_not_text_keys_or_undo() {
        let typing = |cmd: Command| {
            let (_, _, keys) = COMMANDS.iter().find(|(c, _, _)| *c == cmd).unwrap();
            available_while_typing(cmd) && keys.iter().any(|k| k.modifiers.command)
        };
        assert!(typing(Command::Open));
        assert!(typing(Command::ToggleDark));
        assert!(!typing(Command::TogglePen), "P is text");
        assert!(!typing(Command::NextPage), "arrows move the cursor");
        assert!(!typing(Command::Undo), "undoes the typing instead");
        assert!(!typing(Command::Redo));
    }

    #[test]
    fn no_shortcut_is_bound_twice() {
        let all: Vec<_> = COMMANDS
            .iter()
            .flat_map(|(cmd, _, keys)| keys.iter().map(move |k| (cmd, k)))
            .collect();
        for (i, (cmd, key)) in all.iter().enumerate() {
            for (other, other_key) in &all[i + 1..] {
                assert_ne!(key, other_key, "{cmd:?} and {other:?} share a shortcut");
            }
        }
    }
}
