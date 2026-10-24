//! Terminal palette: maps ANSI colors from the emulator onto Glasscode's theme (each theme has
//! its own terminal colours, light ones for the light themes).

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};
use gpui::{Hsla, Rgba};

use crate::theme;

pub fn background() -> Rgb {
    rgb(theme::term().background)
}

pub fn foreground() -> Rgb {
    rgb(theme::term().foreground)
}

pub fn cursor() -> Rgb {
    rgb(theme::term().cursor)
}

pub fn selection() -> Rgb {
    rgb(theme::term().selection)
}

fn ansi(index: usize) -> Rgb {
    rgb(theme::term().ansi[index])
}

const fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: ((hex >> 16) & 0xff) as u8,
        g: ((hex >> 8) & 0xff) as u8,
        b: (hex & 0xff) as u8,
    }
}

pub fn to_hsla(color: Rgb) -> Hsla {
    Rgba {
        r: color.r as f32 / 255.,
        g: color.g as f32 / 255.,
        b: color.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

/// Faint text: two thirds of the way from the background to the colour.
fn dim(color: Rgb) -> Rgb {
    let bg = background();
    let mix = |c: u8, b: u8| (b as f32 + (c as f32 - b as f32) * 0.66).round().clamp(0., 255.) as u8;
    Rgb { r: mix(color.r, bg.r), g: mix(color.g, bg.g), b: mix(color.b, bg.b) }
}

/// Color for a palette index (0..=255), honoring overrides the program set via OSC 4.
pub fn indexed(index: usize, overrides: &Colors) -> Rgb {
    if let Some(color) = overrides[index] {
        return color;
    }
    match index {
        0..=15 => ansi(index),
        16..=231 => {
            let i = index - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            Rgb {
                r: level(i / 36),
                g: level((i / 6) % 6),
                b: level(i % 6),
            }
        }
        232..=255 => {
            let v = (8 + (index - 232) * 10) as u8;
            Rgb { r: v, g: v, b: v }
        }
        _ => foreground(),
    }
}

pub fn named(color: NamedColor, overrides: &Colors) -> Rgb {
    if let Some(color) = overrides[color] {
        return color;
    }
    match color {
        NamedColor::Foreground | NamedColor::BrightForeground => foreground(),
        NamedColor::Background => background(),
        NamedColor::Cursor => cursor(),
        NamedColor::DimForeground => dim(foreground()),
        NamedColor::DimBlack => dim(ansi(0)),
        NamedColor::DimRed => dim(ansi(1)),
        NamedColor::DimGreen => dim(ansi(2)),
        NamedColor::DimYellow => dim(ansi(3)),
        NamedColor::DimBlue => dim(ansi(4)),
        NamedColor::DimMagenta => dim(ansi(5)),
        NamedColor::DimCyan => dim(ansi(6)),
        NamedColor::DimWhite => dim(ansi(7)),
        other => {
            let index = other as usize;
            if index < 16 { ansi(index) } else { foreground() }
        }
    }
}

pub fn resolve(color: AnsiColor, overrides: &Colors) -> Rgb {
    match color {
        AnsiColor::Spec(rgb) => rgb,
        AnsiColor::Named(named_color) => named(named_color, overrides),
        AnsiColor::Indexed(index) => indexed(index as usize, overrides),
    }
}

/// Bold text in the first 8 colors is shown in the bright variant, like most terminals.
pub fn brighten(color: AnsiColor) -> AnsiColor {
    match color {
        AnsiColor::Named(named_color) => AnsiColor::Named(named_color.to_bright()),
        AnsiColor::Indexed(index) if index < 8 => AnsiColor::Indexed(index + 8),
        other => other,
    }
}

pub fn dimmed(color: Rgb) -> Rgb {
    dim(color)
}

/// Answer an OSC 4/10/11/12 color query.
pub fn for_request(index: usize, overrides: &Colors) -> Rgb {
    match index {
        0..=255 => indexed(index, overrides),
        256 => named(NamedColor::Foreground, overrides),
        257 => named(NamedColor::Background, overrides),
        258 => named(NamedColor::Cursor, overrides),
        _ => foreground(),
    }
}
