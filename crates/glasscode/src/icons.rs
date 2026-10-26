//! Icons drawn from SVGs in `assets/icons` (the same shapes as the redesign prototype). GPUI
//! paints an SVG as a mask, so an icon takes the colour given with `text_color`.

use gpui::{Hsla, Pixels, Styled, Svg, svg};

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

/// An icon `w`×`h` in `color`.
pub fn icon(data: &'static [u8], w: Pixels, h: Pixels, color: Hsla) -> Svg {
    svg().data(data).w(w).h(h).flex_none().text_color(color)
}
