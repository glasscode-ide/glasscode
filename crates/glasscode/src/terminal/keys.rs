//! Translate GPUI keystrokes into the byte sequences a terminal program expects (xterm style).

use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

/// Bytes to send to the PTY for `keystroke`, or `None` when the key is not for the terminal
/// (for example Cmd shortcuts, which belong to the app).
pub fn to_esc(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = &keystroke.modifiers;
    if m.platform {
        return None;
    }
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
