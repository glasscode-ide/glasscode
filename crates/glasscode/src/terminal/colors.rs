//! Terminal palette: maps ANSI colors from the emulator onto Glasscode's theme (each theme has
//! its own terminal colours, light ones for the light themes).
//!
//! A program does not know the theme: Claude Code and others paint with 24-bit and 256-colour
//! values chosen for a dark terminal (light grey hints, white text), which cannot be read on a
//! light ground. So the colour of a cell's text is held to a least contrast with the cell's own
//! background ([`text_on`]) wherever the theme gave one side of the pair (the default ground or
//! foreground, one of the 16 colours, the selection). A pair the program chose in full (its own
//! foreground on its own background) is drawn as it asked.

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

pub(crate) const fn rgb(hex: u32) -> Rgb {
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

/// The least contrast of text with its cell's background (WCAG AA).
pub const TEXT_FLOOR: f32 = 4.5;
/// The least contrast of faint (SGR 2) text, and of the lines of a box.
pub const FAINT_FLOOR: f32 = 3.0;
/// The least contrast of a block, a shade or a braille dot on the default ground: enough to see
/// that it is there, as the faint track of a bar is meant to be faint.
pub const SHAPE_FLOOR: f32 = 1.5;

/// WCAG relative luminance.
fn luminance(color: Rgb) -> f32 {
    let linear = |v: u8| {
        let v = v as f32 / 255.;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

/// The WCAG contrast ratio of two colours (1 to 21).
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn mix(from: Rgb, to: Rgb, t: f32) -> Rgb {
    let part = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round().clamp(0., 255.) as u8;
    Rgb { r: part(from.r, to.r), g: part(from.g, to.g), b: part(from.b, to.b) }
}

/// A stronger floor for text that has to change sides (see [`readable`]).
const CROSSED_FLOOR: f32 = 7.0;

/// `fg` as it is when it reads on `bg` at `floor` or more; else moved toward black or white
/// (the one that is further from `bg`) by as little as reaches the floor, so it keeps what it can
/// of its hue. A floor of 4.5 or less can be reached on every background.
///
/// Light text on a light colour (white on yellow) cannot get lighter: it has to become dark,
/// through the greys that are as light as the background. Nothing of its colour is left to keep
/// then, and a grey at just the floor looks washed out on a strong colour, so text that changes
/// sides goes on to [`CROSSED_FLOOR`] (or as far as black or white reaches).
pub fn readable(fg: Rgb, bg: Rgb, floor: f32) -> Rgb {
    if contrast(fg, bg) >= floor {
        return fg;
    }
    let (black, white) = (Rgb { r: 0, g: 0, b: 0 }, Rgb { r: 255, g: 255, b: 255 });
    let end = if contrast(black, bg) >= contrast(white, bg) { black } else { white };
    let crosses = (luminance(fg) > luminance(bg)) != (end == white);
    // Only text at its full floor: a faint word stays faint, and a block stays near its colour.
    let floor = if crosses && floor >= TEXT_FLOOR { CROSSED_FLOOR.min(contrast(end, bg)).max(floor) } else { floor };
    let (mut low, mut high) = (0f32, 1f32);
    for _ in 0..8 {
        let middle = (low + high) / 2.;
        if contrast(mix(fg, end, middle), bg) >= floor {
            high = middle
        } else {
            low = middle
        }
    }
    mix(fg, end, high)
}

/// Faint text (SGR 2) on `bg`: two thirds of the way from the background to the colour, and
/// never under [`FAINT_FLOOR`].
pub fn dimmed(color: Rgb, bg: Rgb) -> Rgb {
    readable(mix(bg, color, 0.66), bg, FAINT_FLOOR)
}

fn dim(color: Rgb) -> Rgb {
    dimmed(color, background())
}

/// Whether the theme, and not the program, gave this colour: the default foreground or
/// background, or one of the 16 colours. (A 24-bit or 256-colour value is the program's own.)
pub fn from_theme(color: AnsiColor) -> bool {
    match color {
        AnsiColor::Named(_) => true,
        AnsiColor::Indexed(index) => index < 16,
        AnsiColor::Spec(_) => false,
    }
}

/// What a cell's character is, for the least contrast it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Text,
    /// A line of a box (U+2500 to U+257F).
    Line,
    /// A block, a shade, a braille dot, a powerline shape: a picture more than a letter.
    Shape,
}

pub fn glyph(c: char) -> Glyph {
    match c as u32 {
        0x2500..=0x257f => Glyph::Line,
        0x2580..=0x259f | 0x2800..=0x28ff | 0xe0b0..=0xe0bf | 0x1fb00..=0x1fbff => Glyph::Shape,
        _ => Glyph::Text,
    }
}

/// The colour a cell's character is drawn in: `fg` (already faint when `faint`) on the cell's
/// background `bg`, held to the least contrast of its kind.
///
/// `ours` says the theme gave the foreground or the background (see [`from_theme`]; the
/// selection counts too). Faint text is always held to its floor, as the faint colour is one
/// this terminal made. A shape is only held on the default ground (`on_ground`): in a picture
/// made of half blocks, two cells of nearly the same colour are meant to be so.
pub fn text_on(fg: Rgb, bg: Rgb, glyph: Glyph, faint: bool, ours: bool, on_ground: bool) -> Rgb {
    let floor = match glyph {
        Glyph::Shape if on_ground && (ours || faint) => SHAPE_FLOOR,
        Glyph::Shape => return fg,
        Glyph::Line if ours || faint => FAINT_FLOOR,
        Glyph::Text if faint => FAINT_FLOOR,
        Glyph::Text if ours => TEXT_FLOOR,
        _ => return fg,
    };
    readable(fg, bg, floor)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{CLASSIC, CONTRAST, GLASS_DARK, GLASS_LIGHT, Palette};

    const PALETTES: [&Palette; 4] = [&GLASS_LIGHT, &GLASS_DARK, &CLASSIC, &CONTRAST];

    fn grey(v: u8) -> Rgb {
        Rgb { r: v, g: v, b: v }
    }

    #[test]
    fn a_colour_that_reads_is_kept_and_one_that_does_not_is_moved() {
        let (white, black) = (grey(255), grey(0));
        assert!((contrast(white, black) - 21.).abs() < 0.01);
        // Claude Code's hint grey, chosen for a dark terminal, on a light ground and a dark one.
        let hint = grey(153);
        assert_eq!(readable(hint, grey(14), TEXT_FLOOR), hint);
        let moved = readable(hint, rgb(GLASS_LIGHT.term.background), TEXT_FLOOR);
        assert!(moved.r < hint.r && contrast(moved, rgb(GLASS_LIGHT.term.background)) >= TEXT_FLOOR, "{moved:?}");
        // No further than needed: it stays a grey, not black.
        assert!(contrast(moved, rgb(GLASS_LIGHT.term.background)) < TEXT_FLOOR + 0.5, "{moved:?}");
        // Text that has to change sides is not left a washed-out grey: white on a light red.
        let salmon = rgb(0xf87171);
        let crossed = readable(white, salmon, TEXT_FLOOR);
        assert!(contrast(crossed, salmon) >= 7.0 && crossed.r < 40, "{crossed:?}");
        // A hue stays a hue.
        let orange = readable(rgb(0xffb86b), white, TEXT_FLOOR);
        assert!(orange.r > orange.g && orange.g > orange.b, "{orange:?}");
        // Every background has an end that reaches the floor, the middle greys too.
        for v in (0..=255).step_by(5) {
            let on = grey(v as u8);
            assert!(contrast(readable(on, on, TEXT_FLOOR), on) >= TEXT_FLOOR, "on grey {v}");
        }
    }

    /// As drawn, in each palette: each of the 16 colours, each of the 256 and a ramp of 24-bit
    /// greys is text of 4.5 or more on the terminal's ground, on the selection, and on each of
    /// the 16 colours as a background; faint, it is 3 or more.
    #[test]
    fn text_reads_on_each_ground_the_theme_gives() {
        for p in PALETTES {
            let term = p.term;
            let sixteen: Vec<Rgb> = term.ansi.iter().map(|c| rgb(*c)).collect();
            let mut texts = sixteen.clone();
            texts.push(rgb(term.foreground));
            texts.extend((16..=255).map(|i| indexed(i, &Colors::default())));
            texts.extend((0..=255).step_by(15).map(|v| grey(v as u8)));
            let mut grounds = vec![rgb(term.background), rgb(term.selection)];
            grounds.extend(sixteen.iter().copied());
            for (g, ground) in grounds.iter().enumerate() {
                for text in &texts {
                    let on_ground = g == 0;
                    let drawn = text_on(*text, *ground, Glyph::Text, false, true, on_ground);
                    let ratio = contrast(drawn, *ground);
                    assert!(ratio >= TEXT_FLOOR - 0.01, "{}: {text:?} on {ground:?} is drawn {drawn:?}, {ratio:.2}", p.name);
                    let faint = text_on(dimmed(*text, *ground), *ground, Glyph::Text, true, true, on_ground);
                    let ratio = contrast(faint, *ground);
                    assert!(ratio >= FAINT_FLOOR - 0.01, "{}: faint {text:?} on {ground:?} is drawn {faint:?}, {ratio:.2}", p.name);
                    let line = text_on(*text, *ground, Glyph::Line, false, true, on_ground);
                    assert!(contrast(line, *ground) >= FAINT_FLOOR - 0.01, "{}: a box line {text:?} on {ground:?}", p.name);
                }
            }
            // The theme's own text is drawn as the theme has it: nothing is moved.
            let ground = rgb(term.background);
            for (i, text) in sixteen.iter().enumerate().filter(|(i, _)| p.light || *i != 0) {
                assert_eq!(text_on(*text, ground, Glyph::Text, false, true, true), *text, "{}: colour {i}", p.name);
            }
            let faint = dimmed(rgb(term.foreground), ground);
            assert!(contrast(faint, ground) >= TEXT_FLOOR, "{}: faint text is {:.2}", p.name, contrast(faint, ground));
            assert!(contrast(faint, ground) < contrast(rgb(term.foreground), ground), "{}: faint text is fainter", p.name);
        }
    }

    /// A program's own foreground on its own background is drawn as it asked, in each theme:
    /// light text on a dark box stays light on the light theme. Faint text in the box goes
    /// toward the box's colour, not the terminal's.
    #[test]
    fn a_pair_the_program_chose_is_kept() {
        let (box_bg, light, dark) = (grey(55), grey(255), grey(70));
        assert_eq!(text_on(light, box_bg, Glyph::Text, false, false, false), light);
        assert_eq!(text_on(dark, box_bg, Glyph::Text, false, false, false), dark, "low contrast, but the program's choice");
        let faint = dimmed(light, box_bg);
        assert!(faint.r < light.r && faint.r > box_bg.r && contrast(faint, box_bg) >= FAINT_FLOOR, "{faint:?}");
        // The theme's text on the program's dark box: lifted, as the theme gave one side.
        let ink = rgb(GLASS_LIGHT.term.foreground);
        let drawn = text_on(ink, box_bg, Glyph::Text, false, true, false);
        assert!(contrast(drawn, box_bg) >= TEXT_FLOOR && drawn.r > ink.r, "{drawn:?}");
        assert!(from_theme(AnsiColor::Named(NamedColor::Foreground)) && from_theme(AnsiColor::Indexed(15)));
        assert!(!from_theme(AnsiColor::Indexed(16)) && !from_theme(AnsiColor::Spec(light)));
    }

    /// A picture of blocks keeps its colours on a painted background; on the terminal's ground
    /// a block is only kept from being the ground's own colour.
    #[test]
    fn shapes_are_held_less_than_text() {
        assert_eq!((glyph('a'), glyph('\u{2500}'), glyph('\u{2588}'), glyph('\u{2591}'), glyph('\u{28ff}')), (Glyph::Text, Glyph::Line, Glyph::Shape, Glyph::Shape, Glyph::Shape));
        let ground = rgb(GLASS_LIGHT.term.background);
        let white = grey(255);
        let block = text_on(white, ground, Glyph::Shape, false, true, true);
        let ratio = contrast(block, ground);
        assert!((SHAPE_FLOOR - 0.01..2.0).contains(&ratio), "{ratio:.2}");
        assert_eq!(text_on(grey(40), grey(44), Glyph::Shape, false, true, false), grey(40));
    }
}
