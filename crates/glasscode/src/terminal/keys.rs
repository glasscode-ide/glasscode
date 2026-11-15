//! Translate GPUI keystrokes into the byte sequences a terminal program expects (xterm style).

use alacritty_terminal::term::TermMode;
use gpui::{KeyDownEvent, Keystroke};

/// Bytes to send to the PTY for a key press, or `None` when the key is not for the terminal.
pub fn for_key_down(event: &KeyDownEvent, mode: TermMode) -> Option<Vec<u8>> {
    // AltGr on Windows comes as Ctrl+Alt: it types a character of the layout (`@` on a German
    // keyboard), not Ctrl+Alt+Q. macOS never sets the flag.
    if event.prefer_character_input && !event.keystroke.modifiers.platform {
        if let Some(text) = &event.keystroke.key_char {
            return Some(text.clone().into_bytes());
        }
    }
    to_esc(&event.keystroke, mode)
}

/// Bytes to send to the PTY for `keystroke`, or `None` when the key is not for the terminal
/// (see [`for_app`]).
pub fn to_esc(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    if for_app(keystroke, cfg!(windows)) {
        return None;
    }
    let m = &keystroke.modifiers;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let modcode = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.control as u8;

    let cursor_key = |final_byte: char| -> String {
        if modcode > 1 {
            format!("\x1b[1;{modcode}{final_byte}")
        } else if app_cursor {
            format!("\x1bO{final_byte}")
        } else {
            format!("\x1b[{final_byte}")
        }
    };
    let tilde = |code: u8| -> String {
        if modcode > 1 {
            format!("\x1b[{code};{modcode}~")
        } else {
            format!("\x1b[{code}~")
        }
    };
    let ss3 = |final_byte: char| -> String {
        if modcode > 1 {
            format!("\x1b[1;{modcode}{final_byte}")
        } else {
            format!("\x1bO{final_byte}")
        }
    };

    let key = keystroke.key.as_str();
    let text: String = match key {
        // Option/Shift+Enter insert a newline in Claude Code and most agent TUIs.
        "enter" if m.alt || m.shift => "\x1b\r".into(),
        "enter" => "\r".into(),
        "tab" if m.shift => "\x1b[Z".into(),
        "tab" => "\t".into(),
        "escape" => "\x1b".into(),
        "backspace" if m.alt => "\x1b\x7f".into(),
        "backspace" if m.control => "\x08".into(),
        "backspace" => "\x7f".into(),
        "up" => cursor_key('A'),
        "down" => cursor_key('B'),
        "right" => cursor_key('C'),
        "left" => cursor_key('D'),
        "home" => cursor_key('H'),
        "end" => cursor_key('F'),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "delete" => tilde(3),
        "insert" => tilde(2),
        "f1" => ss3('P'),
        "f2" => ss3('Q'),
        "f3" => ss3('R'),
        "f4" => ss3('S'),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        "space" if m.control => "\0".into(),
        "space" if m.alt => "\x1b ".into(),
        "space" => " ".into(),
        _ => {
            let mut chars = key.chars();
            let single = match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c),
                _ => None,
            };
            if m.control {
                let c = single?;
                let byte = match c.to_ascii_lowercase() {
                    c @ 'a'..='z' => (c as u8) & 0x1f,
                    '@' | '2' => 0x00,
                    '[' | '3' => 0x1b,
                    '\\' | '4' => 0x1c,
                    ']' | '5' => 0x1d,
                    '^' | '6' => 0x1e,
                    '_' | '-' | '7' | '/' => 0x1f,
                    '8' | '?' => 0x7f,
                    _ => return None,
                };
                return Some(if m.alt { vec![0x1b, byte] } else { vec![byte] });
            }
            if m.alt {
                // Option acts as Meta: ESC prefix plus the unmodified key.
                if let Some(c) = single {
                    let c = if m.shift { c.to_ascii_uppercase() } else { c };
                    return Some(format!("\x1b{c}").into_bytes());
                }
            }
            keystroke.key_char.clone()?
        }
    };
    Some(text.into_bytes())
}

/// Keys that stay with the app or the system: Cmd shortcuts (the Windows key on Windows). On
/// Windows also Ctrl+Shift with a letter, the app's shortcuts there while Ctrl with a letter
/// goes to the program, and Alt+F4, with which the system closes the window.
fn for_app(keystroke: &Keystroke, windows: bool) -> bool {
    let m = &keystroke.modifiers;
    if m.platform {
        return true;
    }
    let letter = keystroke.key.len() == 1 && keystroke.key.bytes().all(|b| b.is_ascii_alphabetic());
    windows
        && ((m.control && m.shift && !m.alt && letter)
            || (m.alt && !m.control && keystroke.key == "f4"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    /// A key press as the platform reports it; `key_char` is None for Ctrl+letter, as Windows
    /// and macOS give no printable character for it.
    fn press(key: &str, modifiers: Modifiers, key_char: Option<&str>) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke {
                modifiers,
                key: key.into(),
                key_char: key_char.map(str::to_string),
            },
            is_held: false,
            prefer_character_input: false,
        }
    }

    fn bytes(event: &KeyDownEvent) -> Option<Vec<u8>> {
        for_key_down(event, TermMode::empty())
    }

    #[test]
    fn the_keys_of_a_shell_give_their_bytes() {
        let none = Modifiers::default();
        assert_eq!(bytes(&press("a", none, Some("a"))), Some(b"a".to_vec()));
        assert_eq!(
            bytes(&press("a", Modifiers::shift(), Some("A"))),
            Some(b"A".to_vec())
        );
        assert_eq!(bytes(&press("enter", none, None)), Some(b"\r".to_vec()));
        // DEL, as xterm sends it; ConPTY reads it as Backspace.
        assert_eq!(
            bytes(&press("backspace", none, None)),
            Some(b"\x7f".to_vec())
        );
        assert_eq!(bytes(&press("tab", none, None)), Some(b"\t".to_vec()));
        assert_eq!(
            bytes(&press("tab", Modifiers::shift(), None)),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(bytes(&press("up", none, None)), Some(b"\x1b[A".to_vec()));
        assert_eq!(bytes(&press("down", none, None)), Some(b"\x1b[B".to_vec()));
        assert_eq!(bytes(&press("right", none, None)), Some(b"\x1b[C".to_vec()));
        assert_eq!(bytes(&press("left", none, None)), Some(b"\x1b[D".to_vec()));
        assert_eq!(
            for_key_down(&press("up", none, None), TermMode::APP_CURSOR),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            bytes(&press("c", Modifiers::control(), None)),
            Some(vec![0x03])
        );
        assert_eq!(
            bytes(&press("d", Modifiers::control(), None)),
            Some(vec![0x04])
        );
        assert_eq!(
            bytes(&press("l", Modifiers::control(), None)),
            Some(vec![0x0c])
        );
    }

    #[test]
    fn altgr_types_its_character() {
        let ctrl_alt = Modifiers {
            control: true,
            alt: true,
            ..Modifiers::default()
        };
        // A German keyboard: AltGr+Q is @.
        let altgr = KeyDownEvent {
            prefer_character_input: true,
            ..press("q", ctrl_alt, Some("@"))
        };
        assert_eq!(bytes(&altgr), Some(b"@".to_vec()));
        // Ctrl+Alt+Q of a layout without a character there stays Meta+Ctrl+Q.
        assert_eq!(bytes(&press("q", ctrl_alt, None)), Some(vec![0x1b, 0x11]));
    }

    #[test]
    fn app_shortcuts_are_not_for_the_terminal() {
        assert_eq!(bytes(&press("c", Modifiers::command(), Some("c"))), None);
    }

    fn keystroke(key: &str, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: None,
        }
    }

    #[test]
    fn ctrl_c_is_byte_3() {
        assert_eq!(
            to_esc(&keystroke("c", Modifiers::control()), TermMode::empty()),
            Some(vec![0x03])
        );
    }

    /// Both platforms' rules, whichever this one is.
    #[test]
    fn the_app_keeps_its_keys_from_the_program() {
        let ctrl_shift = Modifiers::control_shift();
        let ctrl_alt = Modifiers {
            control: true,
            alt: true,
            ..Modifiers::default()
        };
        for windows in [false, true] {
            assert!(for_app(&keystroke("c", Modifiers::command()), windows));
            assert!(!for_app(&keystroke("c", Modifiers::control()), windows));
            assert!(!for_app(&keystroke("q", ctrl_alt), windows));
            assert!(!for_app(&keystroke("tab", ctrl_shift), windows));
            // Ctrl+@ is Ctrl+Shift+2 on a US keyboard; it stays NUL for the program.
            assert!(!for_app(&keystroke("@", Modifiers::control()), windows));
            assert_eq!(for_app(&keystroke("c", ctrl_shift), windows), windows);
            assert_eq!(
                for_app(&keystroke("f4", Modifiers::alt()), windows),
                windows
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_leaves_ctrl_shift_and_alt_f4_to_the_app() {
        assert_eq!(bytes(&press("c", Modifiers::control_shift(), None)), None);
        assert_eq!(bytes(&press("v", Modifiers::control_shift(), None)), None);
        assert_eq!(bytes(&press("f4", Modifiers::alt(), None)), None);
        assert_eq!(
            bytes(&press("c", Modifiers::control(), None)),
            Some(vec![0x03])
        );
    }
}
