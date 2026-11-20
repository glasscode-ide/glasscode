//! Every key binding of the app and the labels of its shortcuts: one table for each platform.
//!
//! macOS keeps its ⌘ shortcuts. On Windows the app's own shortcuts use Ctrl+Shift, because they
//! work wherever the focus is and Ctrl with a letter (Ctrl+C, Ctrl+N, Ctrl+W…) belongs to the
//! program in the terminal. Text fields keep the usual Windows keys (Ctrl+C, Home, Ctrl+Left).

use gpui::{Action, App, DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate};

use crate::{chat_ui, composer, editor, terminal, text_area, text_input, workspace};

/// One binding: the key text GPUI parses, what it does, and where (`None`: the whole window).
struct Key {
    keys: &'static str,
    action: Box<dyn Action>,
    context: Option<&'static str>,
    /// The one labels show, when an action has several.
    label: bool,
}

impl Key {
    fn labeled(mut self) -> Self {
        self.label = true;
        self
    }

    fn binding(self) -> KeyBinding {
        let context = self.context.map(|c| {
            KeyBindingContextPredicate::parse(c)
                .expect("a key context")
                .into()
        });
        KeyBinding::load(
            self.keys,
            self.action,
            context,
            false,
            None,
            &DummyKeyboardMapper,
        )
        .expect("a key text GPUI parses")
    }
}

fn key(keys: &'static str, action: impl Action, context: Option<&'static str>) -> Key {
    Key {
        keys,
        action: Box::new(action),
        context,
        label: false,
    }
}

/// A key that runs an op (ops-7): the op by name and its arguments as JSON, run for the person
/// as a button runs it (`workspace::RunOp`, `Workspace::run_op`). The window's actions stay GPUI
/// actions; a new shortcut for something an op does binds the op with this. No key is bound to an
/// op yet, so the bindings of v1 stay exactly as they were (rule 2).
#[cfg_attr(not(test), allow(dead_code))]
fn op_key(keys: &'static str, name: &str, args: serde_json::Value, context: Option<&'static str>) -> Key {
    key(keys, workspace::RunOp { name: name.to_string(), args }, context)
}

const INPUT: Option<&str> = Some("TextInput");
const AREA: Option<&str> = Some("TextArea");
const COMPOSER: Option<&str> = Some("Composer");
const CHAT: Option<&str> = Some("ChatView");
const WINDOW: Option<&str> = None;
const PALETTE: Option<&str> = Some("Palette");
const TERMINAL: Option<&str> = Some("Terminal");
/// The editor of a file tab (U20). It binds no key of the window, so Jump, Back, Forward and the
/// panels keep working while it has the keyboard.
const EDITOR: Option<&str> = Some("Editor");

/// The table of this platform.
fn keys() -> Vec<Key> {
    if cfg!(windows) { windows() } else { mac() }
}

/// Binds the keys of one context, in table order (each part of the app binds its own).
pub fn bind(cx: &mut App, context: Option<&str>) {
    cx.bind_keys(
        keys()
            .into_iter()
            .filter(|k| k.context == context)
            .map(Key::binding),
    );
}

/// The shortcut of `action` as people read it: `⌘N` on macOS, `Ctrl+Shift+N` on Windows.
pub fn label(action: &dyn Action) -> String {
    label_in(&keys(), action, cfg!(windows))
}

fn label_in(table: &[Key], action: &dyn Action, windows: bool) -> String {
    let mut all = table
        .iter()
        .filter(|k| k.action.partial_eq(action))
        .peekable();
    let first = all.peek().map(|k| k.keys);
    all.find(|k| k.label)
        .map(|k| k.keys)
        .or(first)
        .map(|keys| text(keys, windows))
        .unwrap_or_default()
}

/// `cmd-shift-h` as `⌘⇧H` (macOS), `ctrl-shift-h` as `Ctrl+Shift+H` (Windows).
fn text(keys: &str, windows: bool) -> String {
    let mut parts: Vec<&str> = keys.split('-').collect();
    let key = match parts.pop().unwrap_or_default() {
        "pageup" => "PageUp".to_string(),
        "pagedown" => "PageDown".to_string(),
        key => {
            let mut chars = key.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    };
    let modifiers = parts.into_iter().map(|m| match (m, windows) {
        ("cmd", false) => "⌘",
        ("ctrl", false) => "⌃",
        ("alt", false) => "⌥",
        ("shift", false) => "⇧",
        ("cmd", true) => "Win",
        ("ctrl", true) => "Ctrl",
        ("alt", true) => "Alt",
        ("shift", true) => "Shift",
        (m, _) => m,
    });
    if windows {
        modifiers
            .chain([key.as_str()])
            .collect::<Vec<_>>()
            .join("+")
    } else {
        modifiers.collect::<String>() + &key
    }
}

/// macOS: the bindings from before the Windows port, unchanged (a test keeps them).
fn mac() -> Vec<Key> {
    vec![
        key("backspace", text_input::Backspace, INPUT),
        key("delete", text_input::Delete, INPUT),
        key("left", text_input::Left, INPUT),
        key("right", text_input::Right, INPUT),
        key("alt-left", text_input::WordLeft, INPUT),
        key("alt-right", text_input::WordRight, INPUT),
        key("alt-backspace", text_input::DeleteWordLeft, INPUT),
        key("shift-left", text_input::SelectLeft, INPUT),
        key("shift-right", text_input::SelectRight, INPUT),
        key("cmd-a", text_input::SelectAll, INPUT),
        key("cmd-v", text_input::Paste, INPUT),
        key("cmd-c", text_input::Copy, INPUT),
        key("cmd-x", text_input::Cut, INPUT),
        key("home", text_input::Home, INPUT),
        key("end", text_input::End, INPUT),
        key("cmd-left", text_input::Home, INPUT),
        key("cmd-right", text_input::End, INPUT),
        key("enter", text_input::Submit, INPUT),
        key("escape", text_input::Cancel, INPUT),
        key("backspace", text_area::Backspace, AREA),
        key("delete", text_area::Delete, AREA),
        key("left", text_area::Left, AREA),
        key("right", text_area::Right, AREA),
        key("up", text_area::Up, AREA),
        key("down", text_area::Down, AREA),
        key("shift-left", text_area::SelectLeft, AREA),
        key("shift-right", text_area::SelectRight, AREA),
        key("shift-up", text_area::SelectUp, AREA),
        key("shift-down", text_area::SelectDown, AREA),
        key("cmd-a", text_area::SelectAll, AREA),
        key("cmd-v", text_area::Paste, AREA),
        key("cmd-c", text_area::Copy, AREA),
        key("cmd-x", text_area::Cut, AREA),
        key("cmd-left", text_area::Home, AREA),
        key("cmd-right", text_area::End, AREA),
        key("enter", text_area::Newline, AREA),
        key("cmd-enter", text_area::Submit, AREA),
        key("escape", text_area::Cancel, AREA),
        key("backspace", composer::Backspace, COMPOSER),
        key("delete", composer::Delete, COMPOSER),
        key("left", composer::Left, COMPOSER),
        key("right", composer::Right, COMPOSER),
        key("up", composer::Up, COMPOSER),
        key("down", composer::Down, COMPOSER),
        key("alt-left", composer::WordLeft, COMPOSER),
        key("alt-right", composer::WordRight, COMPOSER),
        key("alt-backspace", composer::DeleteWordLeft, COMPOSER),
        key("cmd-backspace", composer::DeleteToStart, COMPOSER),
        key("shift-left", composer::SelectLeft, COMPOSER),
        key("shift-right", composer::SelectRight, COMPOSER),
        key("shift-up", composer::SelectUp, COMPOSER),
        key("shift-down", composer::SelectDown, COMPOSER),
        key("cmd-a", composer::SelectAll, COMPOSER),
        key("cmd-v", composer::Paste, COMPOSER),
        key("cmd-c", composer::Copy, COMPOSER),
        key("cmd-x", composer::Cut, COMPOSER),
        key("home", composer::Home, COMPOSER),
        key("end", composer::End, COMPOSER),
        key("cmd-left", composer::Home, COMPOSER),
        key("cmd-right", composer::End, COMPOSER),
        key("cmd-up", composer::Home, COMPOSER),
        key("cmd-down", composer::End, COMPOSER),
        key("enter", composer::Submit, COMPOSER),
        key("cmd-enter", composer::SubmitNow, COMPOSER),
        key("shift-enter", composer::Newline, COMPOSER),
        key("alt-enter", composer::Newline, COMPOSER),
        key("escape", composer::Cancel, COMPOSER),
        key("shift-tab", composer::CycleMode, COMPOSER),
        key("cmd-c", chat_ui::CopySelection, CHAT),
        key("shift-tab", chat_ui::CycleMode, CHAT),
        key("cmd-t", workspace::NewSession, WINDOW),
        key("cmd-n", workspace::NewSession, WINDOW).labeled(),
        key("cmd-w", workspace::CloseSession, WINDOW),
        key("ctrl-tab", workspace::NextSession, WINDOW),
        key("cmd-shift-]", workspace::NextSession, WINDOW),
        key("ctrl-shift-tab", workspace::PreviousSession, WINDOW),
        key("cmd-shift-[", workspace::PreviousSession, WINDOW),
        key("cmd-,", workspace::OpenAccounts, WINDOW),
        key("cmd-o", workspace::AddProject, WINDOW),
        key("cmd-shift-h", workspace::Handoff, WINDOW),
        key("cmd-b", workspace::ToggleRightPanel, WINDOW),
        key("cmd-q", workspace::Quit, WINDOW),
        key("cmd-k", workspace::OpenPalette, WINDOW),
        key("cmd-shift-e", workspace::ToggleLeftPanel, WINDOW),
        key("cmd-[", workspace::GoBack, WINDOW),
        key("cmd-]", workspace::GoForward, WINDOW),
        key("cmd-shift-s", workspace::OpenSettings, WINDOW),
        key("up", workspace::PaletteUp, PALETTE),
        key("down", workspace::PaletteDown, PALETTE),
        key("cmd-c", terminal::Copy, TERMINAL),
        key("cmd-v", terminal::Paste, TERMINAL),
        key("cmd-l", terminal::ClearScrollback, TERMINAL),
        key("left", editor::Left, EDITOR),
        key("right", editor::Right, EDITOR),
        key("up", editor::Up, EDITOR),
        key("down", editor::Down, EDITOR),
        key("alt-left", editor::WordLeft, EDITOR),
        key("alt-right", editor::WordRight, EDITOR),
        key("cmd-left", editor::Home, EDITOR),
        key("home", editor::Home, EDITOR),
        key("cmd-right", editor::End, EDITOR),
        key("end", editor::End, EDITOR),
        key("cmd-up", editor::DocStart, EDITOR),
        key("cmd-down", editor::DocEnd, EDITOR),
        key("pageup", editor::PageUp, EDITOR),
        key("pagedown", editor::PageDown, EDITOR),
        key("shift-left", editor::SelectLeft, EDITOR),
        key("shift-right", editor::SelectRight, EDITOR),
        key("shift-up", editor::SelectUp, EDITOR),
        key("shift-down", editor::SelectDown, EDITOR),
        key("alt-shift-left", editor::SelectWordLeft, EDITOR),
        key("alt-shift-right", editor::SelectWordRight, EDITOR),
        key("cmd-shift-left", editor::SelectHome, EDITOR),
        key("shift-home", editor::SelectHome, EDITOR),
        key("cmd-shift-right", editor::SelectEnd, EDITOR),
        key("shift-end", editor::SelectEnd, EDITOR),
        key("cmd-shift-up", editor::SelectDocStart, EDITOR),
        key("cmd-shift-down", editor::SelectDocEnd, EDITOR),
        key("shift-pageup", editor::SelectPageUp, EDITOR),
        key("shift-pagedown", editor::SelectPageDown, EDITOR),
        key("cmd-a", editor::SelectAll, EDITOR),
        key("backspace", editor::Backspace, EDITOR),
        key("delete", editor::Delete, EDITOR),
        key("alt-backspace", editor::DeleteWordLeft, EDITOR),
        key("alt-delete", editor::DeleteWordRight, EDITOR),
        key("enter", editor::Newline, EDITOR),
        key("tab", editor::Tab, EDITOR),
        key("shift-tab", editor::Outdent, EDITOR),
        key("cmd-z", editor::Undo, EDITOR),
        key("cmd-shift-z", editor::Redo, EDITOR),
        key("cmd-x", editor::Cut, EDITOR),
        key("cmd-c", editor::Copy, EDITOR),
        key("cmd-v", editor::Paste, EDITOR),
        key("cmd-s", editor::Save, EDITOR),
    ]
}

/// Windows: Ctrl in text fields and the Chat view, Ctrl+Shift for the window and the terminal.
fn windows() -> Vec<Key> {
    vec![
        key("backspace", text_input::Backspace, INPUT),
        key("delete", text_input::Delete, INPUT),
        key("left", text_input::Left, INPUT),
        key("right", text_input::Right, INPUT),
        key("ctrl-left", text_input::WordLeft, INPUT),
        key("ctrl-right", text_input::WordRight, INPUT),
        key("ctrl-backspace", text_input::DeleteWordLeft, INPUT),
        key("shift-left", text_input::SelectLeft, INPUT),
        key("shift-right", text_input::SelectRight, INPUT),
        key("ctrl-a", text_input::SelectAll, INPUT),
        key("ctrl-v", text_input::Paste, INPUT),
        key("ctrl-c", text_input::Copy, INPUT),
        key("ctrl-x", text_input::Cut, INPUT),
        key("home", text_input::Home, INPUT),
        key("end", text_input::End, INPUT),
        key("enter", text_input::Submit, INPUT),
        key("escape", text_input::Cancel, INPUT),
        key("backspace", text_area::Backspace, AREA),
        key("delete", text_area::Delete, AREA),
        key("left", text_area::Left, AREA),
        key("right", text_area::Right, AREA),
        key("up", text_area::Up, AREA),
        key("down", text_area::Down, AREA),
        key("shift-left", text_area::SelectLeft, AREA),
        key("shift-right", text_area::SelectRight, AREA),
        key("shift-up", text_area::SelectUp, AREA),
        key("shift-down", text_area::SelectDown, AREA),
        key("ctrl-a", text_area::SelectAll, AREA),
        key("ctrl-v", text_area::Paste, AREA),
        key("ctrl-c", text_area::Copy, AREA),
        key("ctrl-x", text_area::Cut, AREA),
        key("home", text_area::Home, AREA),
        key("end", text_area::End, AREA),
        key("enter", text_area::Newline, AREA),
        key("ctrl-enter", text_area::Submit, AREA),
        key("escape", text_area::Cancel, AREA),
        key("backspace", composer::Backspace, COMPOSER),
        key("delete", composer::Delete, COMPOSER),
        key("left", composer::Left, COMPOSER),
        key("right", composer::Right, COMPOSER),
        key("up", composer::Up, COMPOSER),
        key("down", composer::Down, COMPOSER),
        key("ctrl-left", composer::WordLeft, COMPOSER),
        key("ctrl-right", composer::WordRight, COMPOSER),
        key("ctrl-backspace", composer::DeleteWordLeft, COMPOSER),
        key("ctrl-shift-backspace", composer::DeleteToStart, COMPOSER),
        key("shift-left", composer::SelectLeft, COMPOSER),
        key("shift-right", composer::SelectRight, COMPOSER),
        key("shift-up", composer::SelectUp, COMPOSER),
        key("shift-down", composer::SelectDown, COMPOSER),
        key("ctrl-a", composer::SelectAll, COMPOSER),
        key("ctrl-v", composer::Paste, COMPOSER),
        key("ctrl-c", composer::Copy, COMPOSER),
        key("ctrl-x", composer::Cut, COMPOSER),
        key("home", composer::Home, COMPOSER),
        key("end", composer::End, COMPOSER),
        key("ctrl-home", composer::Home, COMPOSER),
        key("ctrl-end", composer::End, COMPOSER),
        key("enter", composer::Submit, COMPOSER),
        key("ctrl-enter", composer::SubmitNow, COMPOSER),
        key("shift-enter", composer::Newline, COMPOSER),
        key("alt-enter", composer::Newline, COMPOSER),
        key("escape", composer::Cancel, COMPOSER),
        key("shift-tab", composer::CycleMode, COMPOSER),
        key("ctrl-c", chat_ui::CopySelection, CHAT),
        key("shift-tab", chat_ui::CycleMode, CHAT),
        key("ctrl-shift-t", workspace::NewSession, WINDOW),
        key("ctrl-shift-n", workspace::NewSession, WINDOW).labeled(),
        key("ctrl-shift-w", workspace::CloseSession, WINDOW),
        key("ctrl-tab", workspace::NextSession, WINDOW),
        key("ctrl-pagedown", workspace::NextSession, WINDOW),
        key("ctrl-shift-tab", workspace::PreviousSession, WINDOW),
        key("ctrl-pageup", workspace::PreviousSession, WINDOW),
        key("ctrl-,", workspace::OpenAccounts, WINDOW),
        key("ctrl-shift-o", workspace::AddProject, WINDOW),
        key("ctrl-alt-h", workspace::Handoff, WINDOW),
        key("ctrl-shift-b", workspace::ToggleRightPanel, WINDOW),
        // Alt+F4 also quits: the system closes the window, and closing it quits (see main.rs).
        key("ctrl-shift-q", workspace::Quit, WINDOW),
        key("ctrl-shift-k", workspace::OpenPalette, WINDOW),
        key("ctrl-shift-e", workspace::ToggleLeftPanel, WINDOW),
        // Ctrl+Shift+, and Ctrl+Shift+. on a US keyboard: Windows gives them as "<" and ">"
        // (gpui_windows folds Shift into the key), and the terminal sends nothing for them, so
        // they take nothing from its program (Alt+Left and Ctrl+[ belong to it).
        key("ctrl-<", workspace::GoBack, WINDOW),
        key("ctrl->", workspace::GoForward, WINDOW),
        key("ctrl-shift-s", workspace::OpenSettings, WINDOW),
        key("up", workspace::PaletteUp, PALETTE),
        key("down", workspace::PaletteDown, PALETTE),
        key("ctrl-shift-c", terminal::Copy, TERMINAL),
        key("ctrl-shift-v", terminal::Paste, TERMINAL),
        key("ctrl-shift-l", terminal::ClearScrollback, TERMINAL),
        // The editor keeps the usual Windows keys, as the text fields do; Ctrl+Tab and
        // Ctrl+PageUp/PageDown stay the window's (the next and the previous session).
        key("left", editor::Left, EDITOR),
        key("right", editor::Right, EDITOR),
        key("up", editor::Up, EDITOR),
        key("down", editor::Down, EDITOR),
        key("ctrl-left", editor::WordLeft, EDITOR),
        key("ctrl-right", editor::WordRight, EDITOR),
        key("home", editor::Home, EDITOR),
        key("end", editor::End, EDITOR),
        key("ctrl-home", editor::DocStart, EDITOR),
        key("ctrl-end", editor::DocEnd, EDITOR),
        key("pageup", editor::PageUp, EDITOR),
        key("pagedown", editor::PageDown, EDITOR),
        key("shift-left", editor::SelectLeft, EDITOR),
        key("shift-right", editor::SelectRight, EDITOR),
        key("shift-up", editor::SelectUp, EDITOR),
        key("shift-down", editor::SelectDown, EDITOR),
        key("ctrl-shift-left", editor::SelectWordLeft, EDITOR),
        key("ctrl-shift-right", editor::SelectWordRight, EDITOR),
        key("shift-home", editor::SelectHome, EDITOR),
        key("shift-end", editor::SelectEnd, EDITOR),
        key("ctrl-shift-home", editor::SelectDocStart, EDITOR),
        key("ctrl-shift-end", editor::SelectDocEnd, EDITOR),
        key("shift-pageup", editor::SelectPageUp, EDITOR),
        key("shift-pagedown", editor::SelectPageDown, EDITOR),
        key("ctrl-a", editor::SelectAll, EDITOR),
        key("backspace", editor::Backspace, EDITOR),
        key("delete", editor::Delete, EDITOR),
        key("ctrl-backspace", editor::DeleteWordLeft, EDITOR),
        key("ctrl-delete", editor::DeleteWordRight, EDITOR),
        key("enter", editor::Newline, EDITOR),
        key("tab", editor::Tab, EDITOR),
        key("shift-tab", editor::Outdent, EDITOR),
        key("ctrl-z", editor::Undo, EDITOR),
        key("ctrl-y", editor::Redo, EDITOR).labeled(),
        key("ctrl-shift-z", editor::Redo, EDITOR),
        key("ctrl-x", editor::Cut, EDITOR),
        key("ctrl-c", editor::Copy, EDITOR),
        key("ctrl-v", editor::Paste, EDITOR),
        key("ctrl-s", editor::Save, EDITOR),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Keystroke;

    /// A key bound to an op builds, its label reads as any other, and it is told apart from a
    /// key bound to another op or other arguments.
    #[test]
    fn a_key_runs_an_op_by_name() {
        let usage = || op_key("cmd-shift-u", "app.open", serde_json::json!({ "place": "usage" }), WINDOW);
        let table = vec![usage(), op_key("cmd-shift-t", "app.open", serde_json::json!({ "place": "today" }), WINDOW)];
        let action = workspace::RunOp { name: "app.open".into(), args: serde_json::json!({ "place": "usage" }) };
        assert_eq!(label_in(&table, &action, false), "⌘⇧U");
        assert_eq!(usage().action.name(), "glasscode::RunOp");
        let _ = usage().binding();
        assert!(glasscode_core::ops::typed(&action.name, action.args.clone()).is_ok(), "the op and its arguments are the registry's");
    }

    fn rows(table: &[Key]) -> Vec<(&'static str, &'static str, Option<&'static str>)> {
        table
            .iter()
            .map(|k| (k.keys, k.action.name(), k.context))
            .collect()
    }

    /// The bindings of macOS as the six files made them before this table, in the same order;
    /// the actions added since (`NEW`) come on top and change none of them.
    #[test]
    fn mac_keeps_the_bindings_from_before() {
        const NEW: [&str; 4] = ["glasscode::ToggleLeftPanel", "glasscode::GoBack", "glasscode::GoForward", "glasscode::OpenSettings"];
        let input = Some("TextInput");
        let area = Some("TextArea");
        let composer = Some("Composer");
        let before = vec![
            ("backspace", "text_input::Backspace", input),
            ("delete", "text_input::Delete", input),
            ("left", "text_input::Left", input),
            ("right", "text_input::Right", input),
            ("alt-left", "text_input::WordLeft", input),
            ("alt-right", "text_input::WordRight", input),
            ("alt-backspace", "text_input::DeleteWordLeft", input),
            ("shift-left", "text_input::SelectLeft", input),
            ("shift-right", "text_input::SelectRight", input),
            ("cmd-a", "text_input::SelectAll", input),
            ("cmd-v", "text_input::Paste", input),
            ("cmd-c", "text_input::Copy", input),
            ("cmd-x", "text_input::Cut", input),
            ("home", "text_input::Home", input),
            ("end", "text_input::End", input),
            ("cmd-left", "text_input::Home", input),
            ("cmd-right", "text_input::End", input),
            ("enter", "text_input::Submit", input),
            ("escape", "text_input::Cancel", input),
            ("backspace", "text_area::Backspace", area),
            ("delete", "text_area::Delete", area),
            ("left", "text_area::Left", area),
            ("right", "text_area::Right", area),
            ("up", "text_area::Up", area),
            ("down", "text_area::Down", area),
            ("shift-left", "text_area::SelectLeft", area),
            ("shift-right", "text_area::SelectRight", area),
            ("shift-up", "text_area::SelectUp", area),
            ("shift-down", "text_area::SelectDown", area),
            ("cmd-a", "text_area::SelectAll", area),
            ("cmd-v", "text_area::Paste", area),
            ("cmd-c", "text_area::Copy", area),
            ("cmd-x", "text_area::Cut", area),
            ("cmd-left", "text_area::Home", area),
            ("cmd-right", "text_area::End", area),
            ("enter", "text_area::Newline", area),
            ("cmd-enter", "text_area::Submit", area),
            ("escape", "text_area::Cancel", area),
            ("backspace", "composer::Backspace", composer),
            ("delete", "composer::Delete", composer),
            ("left", "composer::Left", composer),
            ("right", "composer::Right", composer),
            ("up", "composer::Up", composer),
            ("down", "composer::Down", composer),
            ("alt-left", "composer::WordLeft", composer),
            ("alt-right", "composer::WordRight", composer),
            ("alt-backspace", "composer::DeleteWordLeft", composer),
            ("cmd-backspace", "composer::DeleteToStart", composer),
            ("shift-left", "composer::SelectLeft", composer),
            ("shift-right", "composer::SelectRight", composer),
            ("shift-up", "composer::SelectUp", composer),
            ("shift-down", "composer::SelectDown", composer),
            ("cmd-a", "composer::SelectAll", composer),
            ("cmd-v", "composer::Paste", composer),
            ("cmd-c", "composer::Copy", composer),
            ("cmd-x", "composer::Cut", composer),
            ("home", "composer::Home", composer),
            ("end", "composer::End", composer),
            ("cmd-left", "composer::Home", composer),
            ("cmd-right", "composer::End", composer),
            ("cmd-up", "composer::Home", composer),
            ("cmd-down", "composer::End", composer),
            ("enter", "composer::Submit", composer),
            ("cmd-enter", "composer::SubmitNow", composer),
            ("shift-enter", "composer::Newline", composer),
            ("alt-enter", "composer::Newline", composer),
            ("escape", "composer::Cancel", composer),
            ("shift-tab", "composer::CycleMode", composer),
            ("cmd-c", "chat::CopySelection", Some("ChatView")),
            ("shift-tab", "chat::CycleMode", Some("ChatView")),
            ("cmd-t", "glasscode::NewSession", None),
            ("cmd-n", "glasscode::NewSession", None),
            ("cmd-w", "glasscode::CloseSession", None),
            ("ctrl-tab", "glasscode::NextSession", None),
            ("cmd-shift-]", "glasscode::NextSession", None),
            ("ctrl-shift-tab", "glasscode::PreviousSession", None),
            ("cmd-shift-[", "glasscode::PreviousSession", None),
            ("cmd-,", "glasscode::OpenAccounts", None),
            ("cmd-o", "glasscode::AddProject", None),
            ("cmd-shift-h", "glasscode::Handoff", None),
            ("cmd-b", "glasscode::ToggleRightPanel", None),
            ("cmd-q", "glasscode::Quit", None),
            ("cmd-k", "glasscode::OpenPalette", None),
            ("up", "glasscode::PaletteUp", Some("Palette")),
            ("down", "glasscode::PaletteDown", Some("Palette")),
            ("cmd-c", "terminal::Copy", Some("Terminal")),
            ("cmd-v", "terminal::Paste", Some("Terminal")),
            ("cmd-l", "terminal::ClearScrollback", Some("Terminal")),
        ];
        // The editor (U20) is a context of its own, added after them.
        let old: Vec<_> = rows(&mac()).into_iter().filter(|(_, action, context)| !NEW.contains(action) && *context != EDITOR).collect();
        assert_eq!(old, before);
        let added: Vec<_> = rows(&mac()).into_iter().filter(|(_, action, _)| NEW.contains(action)).map(|(_, action, _)| action).collect();
        assert_eq!(added, NEW, "each new action is bound once");
    }

    /// The editor binds no key that the window binds, so Jump, Back and Forward, the panels,
    /// the sessions and Quit work while it has the keyboard; and it leaves Ctrl+Tab and
    /// Ctrl+PageUp/PageDown (the sessions) alone on Windows.
    #[test]
    fn the_editor_leaves_the_window_its_keys() {
        for table in [mac(), windows()] {
            let window: Vec<Keystroke> = table.iter().filter(|k| k.context.is_none()).map(|k| Keystroke::parse(k.keys).unwrap()).collect();
            let editor: Vec<&Key> = table.iter().filter(|k| k.context == EDITOR).collect();
            assert!(editor.len() > 30);
            for k in editor {
                assert!(!window.contains(&Keystroke::parse(k.keys).unwrap()), "{} is the window's", k.keys);
            }
        }
        for k in windows().iter().filter(|k| k.context == EDITOR) {
            assert!(!matches!(k.keys, "ctrl-tab" | "ctrl-shift-tab" | "ctrl-pageup" | "ctrl-pagedown"), "{}", k.keys);
        }
    }

    /// A key text GPUI cannot parse would stop the app at its start, on the platform of the table.
    #[test]
    fn each_key_text_parses() {
        for k in mac().into_iter().chain(windows()) {
            assert!(Keystroke::parse(k.keys).is_ok(), "{}", k.keys);
            k.binding();
        }
    }

    #[test]
    fn no_two_bindings_share_a_key_and_a_context() {
        for table in [mac(), windows()] {
            let mut seen: Vec<(Keystroke, Option<&str>)> = Vec::new();
            for k in &table {
                let entry = (Keystroke::parse(k.keys).unwrap(), k.context);
                assert!(
                    !seen.contains(&entry),
                    "{} twice in {:?}",
                    k.keys,
                    k.context
                );
                seen.push(entry);
            }
        }
    }

    /// Ctrl+C, Ctrl+N, Ctrl+W… go to the program in the terminal, so the window's own shortcuts
    /// (which apply over the terminal too) never take Ctrl with a letter alone.
    #[test]
    fn windows_leaves_ctrl_and_a_letter_to_the_terminal() {
        for k in windows().iter().filter(|k| k.context.is_none()) {
            let keystroke = Keystroke::parse(k.keys).unwrap();
            let m = keystroke.modifiers;
            let letter =
                keystroke.key.len() == 1 && keystroke.key.chars().all(|c| c.is_ascii_alphabetic());
            assert!(
                !(m.control && letter && !m.shift && !m.alt && !m.platform && !m.function),
                "{}",
                k.keys
            );
        }
    }

    /// Back and forward on Windows are Ctrl with "<" and ">" as Windows reports them (Shift folded
    /// into the key), not Alt with an arrow or Ctrl+[, which programs in the terminal use.
    #[test]
    fn windows_back_and_forward_leave_the_terminal_its_keys() {
        let mut found = 0;
        for k in windows().iter().filter(|k| k.action.partial_eq(&workspace::GoBack) || k.action.partial_eq(&workspace::GoForward)) {
            let keystroke = Keystroke::parse(k.keys).unwrap();
            let m = keystroke.modifiers;
            assert!(m.control && !m.shift && !m.alt, "{}", k.keys);
            assert!(matches!(keystroke.key.as_str(), "<" | ">"), "{}", k.keys);
            found += 1;
        }
        assert_eq!(found, 2);
    }

    #[test]
    fn windows_never_needs_the_windows_key() {
        for k in windows() {
            assert!(
                !Keystroke::parse(k.keys).unwrap().modifiers.platform,
                "{}",
                k.keys
            );
        }
    }

    #[test]
    fn windows_has_each_action_of_mac() {
        let actions = |table: &[Key]| {
            let mut all: Vec<(&str, Option<&str>)> =
                table.iter().map(|k| (k.action.name(), k.context)).collect();
            all.sort();
            all.dedup();
            all
        };
        assert_eq!(actions(&windows()), actions(&mac()));
    }

    #[test]
    fn labels_name_the_keys_of_each_platform() {
        let shown: [(&dyn Action, &str, &str); 21] = [
            (&editor::Save, "⌘S", "Ctrl+S"),
            (&editor::Undo, "⌘Z", "Ctrl+Z"),
            (&editor::Redo, "⌘⇧Z", "Ctrl+Y"),
            (&workspace::ToggleLeftPanel, "⌘⇧E", "Ctrl+Shift+E"),
            (&workspace::GoBack, "⌘[", "Ctrl+<"),
            (&workspace::GoForward, "⌘]", "Ctrl+>"),
            (&workspace::OpenSettings, "⌘⇧S", "Ctrl+Shift+S"),
            (&workspace::OpenPalette, "⌘K", "Ctrl+Shift+K"),
            (&workspace::NewSession, "⌘N", "Ctrl+Shift+N"),
            (&workspace::CloseSession, "⌘W", "Ctrl+Shift+W"),
            (&workspace::NextSession, "⌃Tab", "Ctrl+Tab"),
            (&workspace::PreviousSession, "⌃⇧Tab", "Ctrl+Shift+Tab"),
            (&workspace::OpenAccounts, "⌘,", "Ctrl+,"),
            (&workspace::AddProject, "⌘O", "Ctrl+Shift+O"),
            (&workspace::Handoff, "⌘⇧H", "Ctrl+Alt+H"),
            (&workspace::ToggleRightPanel, "⌘B", "Ctrl+Shift+B"),
            (&workspace::Quit, "⌘Q", "Ctrl+Shift+Q"),
            (&composer::Paste, "⌘V", "Ctrl+V"),
            (&terminal::Copy, "⌘C", "Ctrl+Shift+C"),
            (&terminal::Paste, "⌘V", "Ctrl+Shift+V"),
            (&terminal::ClearScrollback, "⌘L", "Ctrl+Shift+L"),
        ];
        for (action, mac_label, windows_label) in shown {
            assert_eq!(label_in(&mac(), action, false), mac_label);
            assert_eq!(label_in(&windows(), action, true), windows_label);
        }
        assert_eq!(text("ctrl-pagedown", true), "Ctrl+PageDown");
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_its_table() {
        assert_eq!(label(&workspace::NewSession), "Ctrl+Shift+N");
        assert_eq!(rows(&keys()), rows(&windows()));
    }
}
