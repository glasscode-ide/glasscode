//! Icons drawn from SVGs in `assets/icons` (the same shapes as the redesign prototype). GPUI
//! paints an SVG as a mask, so an icon takes the colour given with `text_color`. Brand marks
//! keep their own colours: [`brand`] draws them as images.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui::{AnyElement, App, DevicePixels, Hsla, ImageCacheError, Pixels, RenderImage, Styled, Svg, SvgSize, Window, img, prelude::*, px, size, svg};
use glasscode_core::model::AgentKind;

use crate::theme;

pub const LOGO: &[u8] = include_bytes!("../assets/icons/logo.svg");
pub const SESSIONS: &[u8] = include_bytes!("../assets/icons/sessions.svg");
pub const TICKETS: &[u8] = include_bytes!("../assets/icons/tickets.svg");
pub const USAGE: &[u8] = include_bytes!("../assets/icons/usage.svg");
pub const SETTINGS: &[u8] = include_bytes!("../assets/icons/settings.svg");
pub const THEMES: &[u8] = include_bytes!("../assets/icons/themes.svg");
pub const TODAY: &[u8] = include_bytes!("../assets/icons/today.svg");
// Today's rows.
pub const STAR: &[u8] = include_bytes!("../assets/icons/star.svg");
pub const STAR_FILLED: &[u8] = include_bytes!("../assets/icons/star-filled.svg");
pub const MOON: &[u8] = include_bytes!("../assets/icons/moon.svg");
pub const CHECK: &[u8] = include_bytes!("../assets/icons/check.svg");
pub const ADD: &[u8] = include_bytes!("../assets/icons/plus-circle.svg");
// The Chat view's message box and rows.
pub const MIC: &[u8] = include_bytes!("../assets/icons/chat-mic.svg");
pub const SEND: &[u8] = include_bytes!("../assets/icons/chat-send.svg");
pub const ATTACH: &[u8] = include_bytes!("../assets/icons/chat-attach.svg");
pub const STOP: &[u8] = include_bytes!("../assets/icons/chat-stop.svg");
pub const SPINNER: &[u8] = include_bytes!("../assets/icons/chat-spinner.svg");
pub const CHEVRON: &[u8] = include_bytes!("../assets/icons/chat-chevron.svg");
// The redesign's line icons (a 24 px grid, 1.7 px lines), for the glyphs the interface drew
// as text: ▾ ▸ (turn the chevron), ⎇, +, × ✕, ⌕, ↻, ◫. The microphone is MIC above.
pub const CHEVRON_DOWN: &[u8] = include_bytes!("../assets/icons/chevron-down.svg");
pub const BRANCH: &[u8] = include_bytes!("../assets/icons/branch.svg");
pub const FOLDER: &[u8] = include_bytes!("../assets/icons/folder.svg");
pub const FILE: &[u8] = include_bytes!("../assets/icons/file.svg");
pub const PLUS: &[u8] = include_bytes!("../assets/icons/plus.svg");
pub const CLOSE: &[u8] = include_bytes!("../assets/icons/close.svg");
pub const SEARCH: &[u8] = include_bytes!("../assets/icons/search.svg");
pub const BELL: &[u8] = include_bytes!("../assets/icons/bell.svg");
pub const CALENDAR: &[u8] = include_bytes!("../assets/icons/calendar.svg");
pub const GAUGE: &[u8] = include_bytes!("../assets/icons/gauge.svg");
pub const CHART: &[u8] = include_bytes!("../assets/icons/chart.svg");
pub const GRID: &[u8] = include_bytes!("../assets/icons/grid.svg");
pub const GEAR: &[u8] = include_bytes!("../assets/icons/gear.svg");
pub const PLUG: &[u8] = include_bytes!("../assets/icons/plug.svg");
pub const CPU: &[u8] = include_bytes!("../assets/icons/cpu.svg");
pub const TERMINAL: &[u8] = include_bytes!("../assets/icons/terminal.svg");
pub const REFRESH: &[u8] = include_bytes!("../assets/icons/refresh.svg");
pub const PULL_REQUEST: &[u8] = include_bytes!("../assets/icons/pull-request.svg");
pub const PANEL_LEFT: &[u8] = include_bytes!("../assets/icons/panel-left.svg");
pub const PANEL_RIGHT: &[u8] = include_bytes!("../assets/icons/panel-right.svg");
pub const BACK: &[u8] = include_bytes!("../assets/icons/back.svg");
pub const FORWARD: &[u8] = include_bytes!("../assets/icons/forward.svg");
pub const PLAY: &[u8] = include_bytes!("../assets/icons/play.svg");
pub const SHIELD: &[u8] = include_bytes!("../assets/icons/shield.svg");
/// A keyboard (the prototype's `i-key`), for shortcuts.
pub const KEYBOARD: &[u8] = include_bytes!("../assets/icons/keyboard.svg");

/// Every icon with its name: the kit page shows them, the tests parse them.
pub const ALL: [(&str, &[u8]); 43] = [
    ("logo", LOGO),
    ("sessions", SESSIONS),
    ("tickets", TICKETS),
    ("usage", USAGE),
    ("settings", SETTINGS),
    ("themes", THEMES),
    ("today", TODAY),
    ("star", STAR),
    ("star filled", STAR_FILLED),
    ("moon", MOON),
    ("check", CHECK),
    ("add", ADD),
    ("mic", MIC),
    ("send", SEND),
    ("attach", ATTACH),
    ("stop", STOP),
    ("spinner", SPINNER),
    ("chevron", CHEVRON),
    ("chevron down", CHEVRON_DOWN),
    ("branch", BRANCH),
    ("folder", FOLDER),
    ("file", FILE),
    ("plus", PLUS),
    ("close", CLOSE),
    ("search", SEARCH),
    ("bell", BELL),
    ("calendar", CALENDAR),
    ("gauge", GAUGE),
    ("chart", CHART),
    ("grid", GRID),
    ("gear", GEAR),
    ("plug", PLUG),
    ("cpu", CPU),
    ("terminal", TERMINAL),
    ("refresh", REFRESH),
    ("pull request", PULL_REQUEST),
    ("panel left", PANEL_LEFT),
    ("panel right", PANEL_RIGHT),
    ("back", BACK),
    ("forward", FORWARD),
    ("play", PLAY),
    ("shield", SHIELD),
    ("keyboard", KEYBOARD),
];

/// An icon `w`×`h` in `color`.
pub fn icon(data: &'static [u8], w: Pixels, h: Pixels, color: Hsla) -> Svg {
    svg().data(data).w(w).h(h).flex_none().text_color(color)
}

/// A product's mark (from svgl.app; the shell's is the prototype's own).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Brand {
    Claude,
    Codex,
    Shell,
    Github,
    Linear,
    Cursor,
    VsCode,
}

impl Brand {
    pub const ALL: [Brand; 7] = [Brand::Claude, Brand::Codex, Brand::Shell, Brand::Github, Brand::Linear, Brand::Cursor, Brand::VsCode];

    pub fn name(self) -> &'static str {
        match self {
            Brand::Claude => "Claude",
            Brand::Codex => "Codex",
            Brand::Shell => "Shell",
            Brand::Github => "GitHub",
            Brand::Linear => "Linear",
            Brand::Cursor => "Cursor",
            Brand::VsCode => "VS Code",
        }
    }

    /// The file has no size of its own: [`brand_svg`] gives it one.
    fn svg(self) -> &'static str {
        match self {
            Brand::Claude => include_str!("../assets/icons/brand-claude.svg"),
            Brand::Codex => include_str!("../assets/icons/brand-codex.svg"),
            Brand::Shell => include_str!("../assets/icons/brand-shell.svg"),
            Brand::Github => include_str!("../assets/icons/brand-github.svg"),
            Brand::Linear => include_str!("../assets/icons/brand-linear.svg"),
            Brand::Cursor => include_str!("../assets/icons/brand-cursor.svg"),
            Brand::VsCode => include_str!("../assets/icons/brand-vscode.svg"),
        }
    }

    /// Black and white marks (`currentColor` in their file) take the theme's text colour, so
    /// they show on light and dark; the shell takes its agent colour. The others keep theirs.
    fn ink(self) -> Option<u32> {
        let palette = theme::palette();
        match self {
            Brand::Codex | Brand::Github | Brand::Cursor => Some(palette.text),
            Brand::Shell => Some(palette.shell),
            Brand::Claude | Brand::Linear | Brand::VsCode => None,
        }
    }
}

pub fn agent_brand(agent: AgentKind) -> Brand {
    match agent {
        AgentKind::Claude => Brand::Claude,
        AgentKind::Codex => Brand::Codex,
        AgentKind::Shell => Brand::Shell,
    }
}

/// A brand's mark in full colour, `size` px square. `svg()` paints one colour only, so the
/// mark becomes an image, drawn at the window's own pixel size so that it stays sharp.
pub fn brand(brand: Brand, size: f32) -> AnyElement {
    let ink = brand.ink();
    img(move |window: &mut Window, cx: &mut App| -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let pixels = (size * window.scale_factor()).round().max(1.) as u32;
        brand_image(brand, pixels, ink, cx).map(Ok)
    })
    .size(px(size))
    .flex_none()
    .into_any_element()
}

thread_local! {
    /// Marks drawn so far, by brand, pixel size and colour. A mark that failed is kept as
    /// None, so that its error is logged once and not on each frame.
    static BRANDS: RefCell<HashMap<(Brand, u32, Option<u32>), Option<Arc<RenderImage>>>> = RefCell::default();
}

fn brand_image(brand: Brand, pixels: u32, ink: Option<u32>, cx: &App) -> Option<Arc<RenderImage>> {
    BRANDS.with(|cache| {
        cache
            .borrow_mut()
            .entry((brand, pixels, ink))
            .or_insert_with(|| {
                let renderer = cx.svg_renderer();
                let side = DevicePixels(pixels as i32);
                renderer
                    .parse_svg(brand_svg(brand, pixels, ink).as_bytes())
                    .and_then(|svg| renderer.render_parsed(&svg, SvgSize::Size(size(side, side))))
                    .inspect_err(|err| log::warn!("could not draw the {} mark at {pixels} px: {err}", brand.name()))
                    .ok()
            })
            .clone()
    })
}

/// A mark's SVG, `pixels` square (a mark that is not square stays centred in it), its one
/// colour, if it has one, set to `ink` (0xRRGGBB).
fn brand_svg(brand: Brand, pixels: u32, ink: Option<u32>) -> String {
    let mut root = format!(r#"<svg width="{pixels}" height="{pixels}""#);
    if let Some(ink) = ink {
        root.push_str(&format!(r##" color="#{ink:06x}""##));
    }
    brand.svg().replacen("<svg", &root, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer() -> gpui::SvgRenderer {
        gpui::SvgRenderer::new(Arc::new(()))
    }

    #[test]
    fn each_icon_parses_as_svg() {
        let renderer = renderer();
        for (name, data) in ALL {
            assert!(renderer.parse_svg(data).is_ok(), "{name} does not parse");
        }
    }

    /// Pixels of a 32 px drawing of a mark, BGRA with the colour multiplied by the alpha.
    fn draw(brand: Brand, ink: Option<u32>) -> Vec<u8> {
        let renderer = renderer();
        let svg = renderer.parse_svg(brand_svg(brand, 32, ink).as_bytes()).unwrap_or_else(|err| panic!("{}: {err}", brand.name()));
        let side = DevicePixels(32);
        let image = renderer.render_parsed(&svg, SvgSize::Size(size(side, side))).unwrap_or_else(|err| panic!("{}: {err}", brand.name()));
        assert_eq!(image.size(0), size(side, side), "{}", brand.name());
        image.as_bytes(0).unwrap().to_vec()
    }

    #[test]
    fn each_brand_draws_at_the_size_asked() {
        for brand in Brand::ALL {
            let pixels = draw(brand, Some(0x123456));
            let painted = pixels.chunks_exact(4).filter(|p| p[3] > 0).count();
            assert!(painted > 32, "{} draws {painted} pixels", brand.name());
        }
    }

    #[test]
    fn one_colour_marks_take_the_colour_given() {
        for brand in [Brand::Codex, Brand::Github, Brand::Cursor, Brand::Shell] {
            let pixels = draw(brand, Some(0x3366cc));
            // A pixel that is fully covered: in BGRA, blue first.
            let solid = pixels.chunks_exact(4).find(|p| p[3] == 255).unwrap_or_else(|| panic!("{} has no solid pixel", brand.name()));
            assert_eq!(&solid[..3], &[0xcc, 0x66, 0x33], "{}", brand.name());
        }
        // The full-colour marks ignore it.
        let claude = draw(Brand::Claude, None);
        let solid = claude.chunks_exact(4).find(|p| p[3] == 255).expect("Claude has a solid pixel");
        assert_eq!(&solid[..3], &[0x57, 0x77, 0xd9]);
    }

    #[test]
    fn each_agent_has_its_mark() {
        assert_eq!(agent_brand(AgentKind::Claude), Brand::Claude);
        assert_eq!(agent_brand(AgentKind::Codex), Brand::Codex);
        assert_eq!(agent_brand(AgentKind::Shell), Brand::Shell);
    }
}
