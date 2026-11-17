//! The window buttons of Windows at the right end of the title bar: minimize, maximize or
//! restore, and close. macOS draws its own traffic lights, so only Windows adds these.
//!
//! Each button is a window control area: Windows itself minimizes, maximizes and closes the
//! window, and it shows the snap layouts while the pointer rests on maximize (Windows 11).

use std::sync::OnceLock;

use gpui::{
    AnyElement, App, Hsla, IntoElement, Window, WindowControlArea, deferred, div, prelude::*,
    rgb, white,
};

use crate::theme;

/// The size of the buttons of Windows 11 title bars.
const BUTTON_WIDTH: f32 = 46.;
const GLYPH_SIZE: f32 = 10.;
/// Above the dialogs, the palette and the toasts: the window can still be minimized or closed
/// while one of them covers the title bar.
const PRIORITY: usize = 10;

#[derive(Clone, Copy)]
enum Button {
    Minimize,
    Maximize,
    Restore,
    Close,
}

impl Button {
    fn id(self) -> &'static str {
        match self {
            Button::Minimize => "window-minimize",
            Button::Maximize | Button::Restore => "window-maximize",
            Button::Close => "window-close",
        }
    }

    /// The same code points in Segoe Fluent Icons and Segoe MDL2 Assets.
    fn glyph(self) -> &'static str {
        match self {
            Button::Minimize => "\u{e921}",
            Button::Maximize => "\u{e922}",
            Button::Restore => "\u{e923}",
            Button::Close => "\u{e8bb}",
        }
    }

    fn area(self) -> WindowControlArea {
        match self {
            Button::Minimize => WindowControlArea::Min,
            Button::Maximize | Button::Restore => WindowControlArea::Max,
            Button::Close => WindowControlArea::Close,
        }
    }
}

/// The three buttons, as the last child of the title bar (which must be the drag area).
#[derive(IntoElement)]
pub struct WindowControls;

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let maximize = if window.is_maximized() {
            Button::Restore
        } else {
            Button::Maximize
        };
        // Windows dims the glyphs of a window that is not in front.
        let color = if window.is_window_active() {
            theme::text()
        } else {
            theme::faint()
        };
        deferred(
            div()
                .flex()
                .flex_none()
                .h_full()
                .font_family(icon_font(cx))
                .text_size(theme::sz(GLYPH_SIZE))
                .text_color(color)
                .children([Button::Minimize, maximize, Button::Close].map(button)),
        )
        .with_priority(PRIORITY)
    }
}

fn button(kind: Button) -> impl IntoElement {
    // The red of the close button of Windows 11, with a white glyph.
    let red = Hsla::from(rgb(0xc42b1c));
    div()
        .id(kind.id())
        .flex()
        .items_center()
        .justify_center()
        .w(theme::sz(BUTTON_WIDTH))
        .h_full()
        // The pointer is on the button only, not on the drag area of the bar under it, so the
        // hit test gives this button's area.
        .occlude()
        .window_control_area(kind.area())
        .map(|d| match kind {
            Button::Close => d
                .hover(|s| s.bg(red).text_color(white()))
                .active(|s| s.bg(red.opacity(0.9)).text_color(white())),
            _ => d
                .hover(|s| s.bg(theme::hover()).text_color(theme::text_strong()))
                .active(|s| s.bg(theme::selected())),
        })
        .child(kind.glyph())
}

/// A control in the title bar. On Windows the bar is the drag area, and a press on it moves the
/// window: the control covers the drag area under it, so a press clicks the control.
pub fn clickable(control: impl IntoElement) -> AnyElement {
    if cfg!(windows) {
        div().flex().occlude().child(control).into_any_element()
    } else {
        control.into_any_element()
    }
}

/// Segoe Fluent Icons comes with Windows 11; Windows 10 has only Segoe MDL2 Assets.
fn icon_font(cx: &App) -> &'static str {
    static FONT: OnceLock<&'static str> = OnceLock::new();
    FONT.get_or_init(|| {
        let fluent = cx
            .text_system()
            .all_font_names()
            .iter()
            .any(|name| name == "Segoe Fluent Icons");
        if fluent {
            "Segoe Fluent Icons"
        } else {
            "Segoe MDL2 Assets"
        }
    })
}
