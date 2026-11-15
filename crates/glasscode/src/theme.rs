//! Glasscode's colours, fonts and sizes. Themes: Glasscode Light and Dark (the Glasscode design
//! system), Glasscode classic (deep navy and amber, the look before), High contrast, and Auto
//! (Glasscode Light or Dark, following macOS). An accent can replace the theme's own (ink in
//! Glasscode). Every function here reads what is in effect now, so a change shows everywhere on
//! the next frame; the terminal reads its colours from here too.

use std::cell::RefCell;
use std::sync::OnceLock;

use gpui::{App, Hsla, Rgba, SharedString, rgb};
use glasscode_core::model::{AccentChoice, Appearance, CodeFont, CursorStyle, Density, TextSize, ThemeChoice, UiFont};

use crate::platform::{self, SystemFont};

pub fn c(hex: u32) -> Hsla {
    let color: Rgba = rgb(hex);
    color.into()
}

/// A terminal's colours, as 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TermColors {
    pub background: u32,
    pub foreground: u32,
    pub cursor: u32,
    pub selection: u32,
    /// The 16 ANSI colours, normal then bright.
    pub ansi: [u32; 16],
}

/// One theme's colours, as 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub name: &'static str,
    pub note: &'static str,
    pub light: bool,
    pub bg: u32,
    pub panel: u32,
    pub chrome: u32,
    pub header: u32,
    pub raised: u32,
    pub dialog: u32,
    pub hover: u32,
    pub selected: u32,
    pub border: u32,
    pub border_strong: u32,
    pub text: u32,
    pub text_strong: u32,
    pub text_soft: u32,
    pub muted: u32,
    pub faint: u32,
    pub accent: u32,
    pub on_accent: u32,
    pub claude: u32,
    pub codex: u32,
    pub shell: u32,
    pub added: u32,
    pub removed: u32,
    pub modified: u32,
    /// A text field's inside.
    pub input: u32,
    /// A quiet background for a group of rows.
    pub subtle: u32,
    /// `code` inside text.
    pub code: u32,
    pub link: u32,
    /// "Needs you" (orange) and the text on it.
    pub attention: u32,
    pub on_attention: u32,
    /// Buttons that delete work, and the text on them.
    pub danger: u32,
    pub on_danger: u32,
    pub error_bg: u32,
    pub error_text: u32,
    pub error_border: u32,
    pub success_bg: u32,
    pub success_text: u32,
    pub success_border: u32,
    pub warn_bg: u32,
    pub info_bg: u32,
    pub violet: u32,
    pub violet_bg: u32,
    /// A small tag's background.
    pub tag_bg: u32,
    /// The dimmed backdrop behind a dialog, and how opaque it is.
    pub overlay: u32,
    pub overlay_alpha: f32,
    pub term: TermColors,
}

pub const GLASS_LIGHT: Palette = Palette {
    name: "Glasscode Light",
    note: "Warm linen and ink, as in Glasscode",
    light: true,
    bg: 0xf9f6f2,
    panel: 0xf8f5f0,
    chrome: 0xf9f6f2,
    header: 0xf9f6f2,
    raised: 0xfcfaf6,
    dialog: 0xfcfaf6,
    hover: 0xf2ece2,
    selected: 0xede5d9,
    border: 0xe7e0d6,
    border_strong: 0xdcd3c6,
    text: 0x15110c,
    text_strong: 0x15110c,
    text_soft: 0x3d372f,
    muted: 0x746c60,
    faint: 0x70685c,
    accent: 0x1c1812,
    on_accent: 0xfcfaf6,
    claude: 0xcf5604,
    codex: 0x1c65c8,
    shell: 0x746c60,
    added: 0x0f6a31,
    removed: 0xb00a1d,
    modified: 0x9a3e00,
    input: 0xfefcfa,
    subtle: 0xf4efe8,
    code: 0x8a4b00,
    link: 0x1c65c8,
    attention: 0xc2410c,
    on_attention: 0xffffff,
    danger: 0xb00a1d,
    on_danger: 0xffffff,
    error_bg: 0xfbeceb,
    error_text: 0x9b1c1c,
    error_border: 0xf0c6c4,
    success_bg: 0xe7f3ea,
    success_text: 0x0f6a31,
    success_border: 0xbcdcc5,
    warn_bg: 0xf8eed6,
    info_bg: 0xe6eef9,
    violet: 0x6d28d9,
    violet_bg: 0xf0eafb,
    tag_bg: 0xede5d9,
    overlay: 0x15110c,
    overlay_alpha: 0.32,
    term: TermColors {
        background: 0xfefcfa,
        foreground: 0x2a241c,
        cursor: 0x1c1812,
        selection: 0xe6dccd,
        ansi: [
            0x3d372f, 0xb00a1d, 0x0f6a31, 0x8a5a00, 0x1c65c8, 0x8b3fb5, 0x0e7490, 0x5c554b, 0x746c60, 0xd0312d, 0x1f8a45, 0x9a6b00, 0x2f7ae0, 0xa457cf,
            0x1590a6, 0x15110c,
        ],
    },
};

pub const GLASS_DARK: Palette = Palette {
    name: "Glasscode Dark",
    note: "Cool zinc, quiet at night, as in Glasscode",
    light: false,
    bg: 0x09090b,
    panel: 0x18181b,
    chrome: 0x09090b,
    header: 0x09090b,
    raised: 0x111114,
    dialog: 0x18181b,
    hover: 0x27272a,
    selected: 0x2d2d31,
    border: 0x27272a,
    border_strong: 0x52525c,
    text: 0xfafafa,
    text_strong: 0xfafafa,
    text_soft: 0xd0d0d6,
    muted: 0x9f9fa9,
    faint: 0x91919b,
    accent: 0xfafafa,
    on_accent: 0x18181b,
    claude: 0xf0834e,
    codex: 0x70a6f5,
    shell: 0xa1a1aa,
    added: 0x40cc6d,
    removed: 0xfb2c36,
    modified: 0xf3ba25,
    input: 0x0e0e10,
    subtle: 0x131316,
    code: 0xe5c07b,
    link: 0x70a6f5,
    attention: 0xfb923c,
    on_attention: 0x1c0d02,
    danger: 0xdc2626,
    on_danger: 0xffffff,
    error_bg: 0x2a1214,
    error_text: 0xfca5a5,
    error_border: 0x5a1d22,
    success_bg: 0x0f2a18,
    success_text: 0x86efac,
    success_border: 0x1f5a33,
    warn_bg: 0x2a2010,
    info_bg: 0x142036,
    violet: 0xc4b5fd,
    violet_bg: 0x221c33,
    tag_bg: 0x27272a,
    overlay: 0x000000,
    overlay_alpha: 0.6,
    term: TermColors {
        background: 0x0e0e10,
        foreground: 0xe4e4e7,
        cursor: 0xfafafa,
        selection: 0x3f3f46,
        ansi: [
            0x27272a, 0xf87171, 0x4ade80, 0xfacc15, 0x60a5fa, 0xc084fc, 0x22d3ee, 0xd4d4d8, 0x71717a, 0xfca5a5, 0x86efac, 0xfde047, 0x93c5fd, 0xd8b4fe,
            0x67e8f9, 0xfafafa,
        ],
    },
};

/// The look until the redesign.
pub const CLASSIC: Palette = Palette {
    name: "Glasscode classic",
    note: "Deep navy and amber, the look until now",
    light: false,
    bg: 0x0b0e14,
    panel: 0x10141c,
    chrome: 0x0f131b,
    header: 0x0d1118,
    raised: 0x161b25,
    dialog: 0x121722,
    hover: 0x1a2030,
    selected: 0x222a3b,
    border: 0x222937,
    border_strong: 0x2a3242,
    text: 0xe6e9f0,
    text_strong: 0xf2f4f8,
    text_soft: 0xc9d1de,
    muted: 0x9aa4b6,
    faint: 0x7f8a9d,
    accent: 0xf2b65a,
    on_accent: 0x17120a,
    claude: 0xe08a62,
    codex: 0x8db8ff,
    shell: 0xaab3c3,
    added: 0x7dd3a0,
    removed: 0xf08c8c,
    modified: 0xf2c46d,
    input: 0x0e121a,
    subtle: 0x121824,
    code: 0xe6c48a,
    link: 0x8db8ff,
    attention: 0xf5a142,
    on_attention: 0x1c0d02,
    danger: 0xc2474f,
    on_danger: 0xffffff,
    error_bg: 0x2a1517,
    error_text: 0xf5b8b8,
    error_border: 0x4a2326,
    success_bg: 0x16241c,
    success_text: 0xcdefd9,
    success_border: 0x2e5a44,
    warn_bg: 0x2a2416,
    info_bg: 0x16203a,
    violet: 0xc7a6ff,
    violet_bg: 0x221c33,
    tag_bg: 0x1b2130,
    overlay: 0x04060a,
    overlay_alpha: 0.72,
    term: TermColors {
        background: 0x0b0e14,
        foreground: 0xd5dbe5,
        cursor: 0xf2b65a,
        selection: 0x2a3550,
        ansi: [
            0x1b2130, 0xf07c7c, 0x7dd3a0, 0xf2c46d, 0x7fb2ff, 0xc7a6ff, 0x6fd3d8, 0xc9d1de, 0x5c677a, 0xff9c9c, 0x9be7b8, 0xffd98a, 0xa3c8ff, 0xdbc2ff,
            0x92e6ea, 0xf2f4f8,
        ],
    },
};

pub const CONTRAST: Palette = Palette {
    name: "High contrast",
    note: "Black and white, nothing faint",
    light: false,
    bg: 0x000000,
    panel: 0x0a0a0a,
    chrome: 0x000000,
    header: 0x000000,
    raised: 0x141414,
    dialog: 0x0a0a0a,
    hover: 0x1f1f1f,
    selected: 0x2c2c2c,
    border: 0x5c5c5c,
    border_strong: 0x8c8c8c,
    text: 0xffffff,
    text_strong: 0xffffff,
    text_soft: 0xf0f0f0,
    muted: 0xd8d8d8,
    faint: 0xc4c4c4,
    accent: 0xffffff,
    on_accent: 0x000000,
    claude: 0xffab7a,
    codex: 0x9ccaff,
    shell: 0xe0e0e0,
    added: 0x8cf0b0,
    removed: 0xff9e9e,
    modified: 0xffe07a,
    input: 0x000000,
    subtle: 0x0f0f0f,
    code: 0xffe07a,
    link: 0x9ccaff,
    attention: 0xffb86b,
    on_attention: 0x000000,
    danger: 0xff5c5c,
    on_danger: 0x000000,
    error_bg: 0x2a0000,
    error_text: 0xffb3b3,
    error_border: 0xff7a7a,
    success_bg: 0x002a10,
    success_text: 0xb3ffcc,
    success_border: 0x6ee7a0,
    warn_bg: 0x2a2200,
    info_bg: 0x001a33,
    violet: 0xd8c8ff,
    violet_bg: 0x1f1433,
    tag_bg: 0x2c2c2c,
    overlay: 0x000000,
    overlay_alpha: 0.75,
    term: TermColors {
        background: 0x000000,
        foreground: 0xffffff,
        cursor: 0xffffff,
        selection: 0x4a4a4a,
        ansi: [
            0x2e2e2e, 0xff6b6b, 0x5cf08e, 0xffe066, 0x6cb6ff, 0xe08aff, 0x4ce6f0, 0xf0f0f0, 0x8c8c8c, 0xff9e9e, 0x8cf0b0, 0xffeb99, 0x9ccaff, 0xecb3ff,
            0x8ff2f7, 0xffffff,
        ],
    },
};

/// The palette a choice stands for; Auto depends on whether macOS is dark.
pub fn palette_for(theme: ThemeChoice, system_dark: bool) -> &'static Palette {
    match theme {
        ThemeChoice::Auto if system_dark => &GLASS_DARK,
        ThemeChoice::Auto | ThemeChoice::Light => &GLASS_LIGHT,
        ThemeChoice::Dark => &GLASS_DARK,
        ThemeChoice::Classic => &CLASSIC,
        ThemeChoice::Contrast => &CONTRAST,
    }
}

/// The accent colour and the text on it, for a palette.
pub fn accent_for(choice: AccentChoice, palette: &Palette) -> (u32, u32) {
    match choice {
        AccentChoice::Theme => (palette.accent, palette.on_accent),
        AccentChoice::Indigo => (if palette.light { 0x4948e5 } else { 0x4f4ae6 }, 0xffffff),
        AccentChoice::Amber if palette.light => (0x9a3e00, 0xffffff),
        AccentChoice::Amber => (0xf2b65a, 0x17120a),
    }
}

pub fn ui_font_family(font: UiFont) -> &'static str {
    match font {
        UiFont::Geist => "Geist",
        UiFont::Inter => "Inter",
        UiFont::Plex => "IBM Plex Sans",
        UiFont::System => platform::THIS.ui_font.family,
    }
}

pub fn code_font_family(font: CodeFont) -> &'static str {
    match font {
        CodeFont::Geist => "Geist Mono",
        CodeFont::JetBrains => "JetBrains Mono",
        CodeFont::Plex => "IBM Plex Mono",
        CodeFont::System => system_code_font().family,
    }
}

static SYSTEM_CODE_FONT: OnceLock<SystemFont> = OnceLock::new();

/// Choose the system code font this computer has, before the first frame. macOS offers one
/// font only, so nothing is looked up there.
pub fn find_system_fonts(cx: &App) {
    SYSTEM_CODE_FONT.get_or_init(|| {
        let fonts = platform::THIS.code_fonts;
        if fonts.len() < 2 {
            return fonts[0];
        }
        let installed = cx.text_system().all_font_names();
        let font = fonts.iter().find(|f| installed.iter().any(|name| name == f.family)).unwrap_or(&fonts[fonts.len() - 1]);
        *font
    });
}

/// The "System" code font: Menlo on macOS; Cascadia Mono, or Consolas where Windows has no Cascadia.
pub fn system_code_font() -> SystemFont {
    let fonts = platform::THIS.code_fonts;
    SYSTEM_CODE_FONT.get().copied().unwrap_or(fonts[fonts.len() - 1])
}

/// What is in effect now.
struct Current {
    palette: Palette,
    appearance: Appearance,
    system_dark: bool,
    applied: bool,
}

thread_local! {
    static CURRENT: RefCell<Current> = RefCell::new(Current { palette: GLASS_LIGHT, appearance: Appearance::default(), system_dark: false, applied: false });
}

/// Put an appearance in effect (and say whether anything changed).
pub fn apply(appearance: Appearance, system_dark: bool) -> bool {
    CURRENT.with(|current| {
        let mut current = current.borrow_mut();
        if current.applied && current.appearance == appearance && current.system_dark == system_dark {
            return false;
        }
        let mut palette = *palette_for(appearance.theme, system_dark);
        let (accent, on_accent) = accent_for(appearance.accent, &palette);
        palette.accent = accent;
        palette.on_accent = on_accent;
        palette.term.cursor = if appearance.accent == AccentChoice::Theme { palette.term.cursor } else { accent };
        *current = Current { palette, appearance, system_dark, applied: true };
        true
    })
}

pub fn appearance() -> Appearance {
    CURRENT.with(|c| c.borrow().appearance)
}

pub fn palette() -> Palette {
    CURRENT.with(|c| c.borrow().palette)
}

fn get(f: impl Fn(&Palette) -> u32) -> Hsla {
    CURRENT.with(|current| c(f(&current.borrow().palette)))
}

pub fn is_light() -> bool {
    CURRENT.with(|c| c.borrow().palette.light)
}

pub fn bg() -> Hsla {
    get(|p| p.bg)
}
pub fn panel() -> Hsla {
    get(|p| p.panel)
}
pub fn chrome() -> Hsla {
    get(|p| p.chrome)
}
pub fn header() -> Hsla {
    get(|p| p.header)
}
pub fn raised() -> Hsla {
    get(|p| p.raised)
}
pub fn dialog() -> Hsla {
    get(|p| p.dialog)
}
pub fn hover() -> Hsla {
    get(|p| p.hover)
}
pub fn selected() -> Hsla {
    get(|p| p.selected)
}
pub fn border() -> Hsla {
    get(|p| p.border)
}
pub fn border_strong() -> Hsla {
    get(|p| p.border_strong)
}
pub fn text() -> Hsla {
    get(|p| p.text)
}
pub fn text_strong() -> Hsla {
    get(|p| p.text_strong)
}
pub fn text_soft() -> Hsla {
    get(|p| p.text_soft)
}
pub fn muted() -> Hsla {
    get(|p| p.muted)
}
pub fn faint() -> Hsla {
    get(|p| p.faint)
}
pub fn accent() -> Hsla {
    get(|p| p.accent)
}
pub fn on_accent() -> Hsla {
    get(|p| p.on_accent)
}
pub fn claude() -> Hsla {
    get(|p| p.claude)
}
pub fn codex() -> Hsla {
    get(|p| p.codex)
}
pub fn shell() -> Hsla {
    get(|p| p.shell)
}
pub fn added() -> Hsla {
    get(|p| p.added)
}
pub fn removed() -> Hsla {
    get(|p| p.removed)
}
pub fn modified() -> Hsla {
    get(|p| p.modified)
}
/// A text field's inside.
pub fn input() -> Hsla {
    get(|p| p.input)
}
/// A quiet background for a group of rows.
pub fn subtle() -> Hsla {
    get(|p| p.subtle)
}
/// `code` inside text.
pub fn code() -> Hsla {
    get(|p| p.code)
}
pub fn link() -> Hsla {
    get(|p| p.link)
}
/// "Needs you" orange, and the text on it.
pub fn attention() -> Hsla {
    get(|p| p.attention)
}
pub fn on_attention() -> Hsla {
    get(|p| p.on_attention)
}
/// A button that deletes work, and the text on it.
pub fn danger() -> Hsla {
    get(|p| p.danger)
}
pub fn on_danger() -> Hsla {
    get(|p| p.on_danger)
}
pub fn error_bg() -> Hsla {
    get(|p| p.error_bg)
}
pub fn error_text() -> Hsla {
    get(|p| p.error_text)
}
pub fn error_border() -> Hsla {
    get(|p| p.error_border)
}
pub fn success_bg() -> Hsla {
    get(|p| p.success_bg)
}
pub fn success_text() -> Hsla {
    get(|p| p.success_text)
}
pub fn success_border() -> Hsla {
    get(|p| p.success_border)
}
pub fn warn_bg() -> Hsla {
    get(|p| p.warn_bg)
}
pub fn info_bg() -> Hsla {
    get(|p| p.info_bg)
}
pub fn violet() -> Hsla {
    get(|p| p.violet)
}
pub fn violet_bg() -> Hsla {
    get(|p| p.violet_bg)
}
pub fn tag_bg() -> Hsla {
    get(|p| p.tag_bg)
}
pub fn overlay() -> Hsla {
    CURRENT.with(|current| {
        let p = &current.borrow().palette;
        let mut color = c(p.overlay);
        color.a = p.overlay_alpha;
        color
    })
}

/// The terminal's colours now.
pub fn term() -> TermColors {
    CURRENT.with(|c| c.borrow().palette.term)
}

// ----- fonts and sizes ---------------------------------------------------------------------

/// The interface or the code font in effect; use it wherever a font family goes
/// (`.font_family(theme::MONO_FONT)`, `SharedString::from(theme::UI_FONT)`).
#[derive(Clone, Copy, Debug)]
pub struct FontName {
    mono: bool,
}

pub const UI_FONT: FontName = FontName { mono: false };
pub const MONO_FONT: FontName = FontName { mono: true };

impl FontName {
    pub fn family(self) -> &'static str {
        let a = appearance();
        if self.mono { code_font_family(a.code_font) } else { ui_font_family(a.ui_font) }
    }
}

impl From<FontName> for SharedString {
    fn from(font: FontName) -> Self {
        SharedString::new_static(font.family())
    }
}

/// The interface's base text size (where an element does not set its own).
pub fn ui_text_size() -> f32 {
    match appearance().text_size {
        TextSize::Small => 12.,
        TextSize::Medium => 13.,
        TextSize::Large => 14.5,
    }
}

/// The Chat view's reading font: the interface font, a serif like the Claude app's replies
/// (Charter, which macOS has; Georgia on Windows), or the system's own.
pub fn chat_font_family() -> &'static str {
    let a = appearance();
    match a.chat_font {
        glasscode_core::model::ChatFont::Ui => ui_font_family(a.ui_font),
        glasscode_core::model::ChatFont::Serif => platform::THIS.serif_font,
        glasscode_core::model::ChatFont::System => platform::THIS.ui_font.family,
    }
}

/// The Chat view's reading size, a little larger than the interface.
pub fn chat_text_size() -> f32 {
    ui_text_size() + 2.
}

/// Code inside a reply: red-brown on a faint tint, as in the Claude app.
pub fn chat_code() -> Hsla {
    if is_light() { c(0x8a2424) } else { c(0xfe8181) }
}

pub fn chat_code_bg() -> Hsla {
    let mut tint = text_strong();
    tint.a = if is_light() { 0.055 } else { 0.1 };
    tint
}

/// Diff rows in the Chat view: a light tint of green or red behind the line.
pub fn added_bg() -> Hsla {
    let mut tint = added();
    tint.a = if is_light() { 0.13 } else { 0.2 };
    tint
}

pub fn removed_bg() -> Hsla {
    let mut tint = removed();
    tint.a = if is_light() { 0.12 } else { 0.2 };
    tint
}

/// The terminals' font size.
pub fn terminal_font_size() -> f32 {
    match appearance().text_size {
        TextSize::Small => 12.,
        TextSize::Medium => 13.,
        TextSize::Large => 14.5,
    }
}

/// Sidebar rows closer together.
pub fn compact() -> bool {
    appearance().density == Density::Compact
}

/// The cursor terminals draw where a program asks for the default (block) one.
pub fn cursor_style() -> CursorStyle {
    appearance().cursor
}

pub fn agent_color(agent: glasscode_core::model::AgentKind) -> Hsla {
    use glasscode_core::model::AgentKind;
    match agent {
        AgentKind::Claude => claude(),
        AgentKind::Codex => codex(),
        AgentKind::Shell => shell(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_takes_effect() {
        assert!(apply(Appearance::default(), false));
        assert!(is_light());
        assert!(!apply(Appearance::default(), false));
        assert!(apply(Appearance::default(), true));
        assert!(!is_light());
        assert_eq!(palette().name, "Glasscode Dark");
        let amber = Appearance { theme: ThemeChoice::Light, accent: AccentChoice::Amber, ..Appearance::default() };
        apply(amber, true);
        assert_eq!((palette().accent, palette().on_accent), (0x9a3e00, 0xffffff));
        assert_eq!(MONO_FONT.family(), "Geist Mono");
        let classic = Appearance { theme: ThemeChoice::Classic, code_font: CodeFont::JetBrains, ..Appearance::default() };
        apply(classic, false);
        assert_eq!((palette().bg, MONO_FONT.family()), (0x0b0e14, "JetBrains Mono"));
    }

    #[test]
    fn system_fonts_are_the_platforms() {
        assert_eq!(ui_font_family(UiFont::System), platform::THIS.ui_font.family);
        assert!(platform::THIS.code_fonts.contains(&system_code_font()));
        assert_eq!(code_font_family(CodeFont::System), system_code_font().family);
    }
}
