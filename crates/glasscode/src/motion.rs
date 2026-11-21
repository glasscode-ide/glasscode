//! Motion: how things come in, and how waiting looks.
//!
//! A GPUI animation draws the whole window for each of its frames (`Workspace::render` runs each
//! time), so motion here is of two kinds only:
//!
//! - **Short and once.** An element fades in, a panel slides in, a ring fills: 140 to 260 ms
//!   when it first shows, then nothing. The cost is a handful of frames.
//! - **Slow and only while waiting.** The skeleton and the spinner repeat at no more than
//!   [`WAIT_FPS`] frames each second, and only for as long as something loads.
//!
//! Nothing here repeats while the app only shows a state (a working session keeps its still dot:
//! `ui::status_dot`). GPUI leaves each animation out when `cx.reduce_motion()` is set: an
//! element is then drawn as it is at its end.
//!
//! An animation is known by its id: it plays when an element with that id first shows, and again
//! after the element was gone for a frame. Give the id of the thing, not of the screen, so that a
//! list does not play again when the screen draws.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::Duration;

use gpui::{Animation, AnimationElement, AnimationExt as _, Div, ElementId, Hsla, PathBuilder, Pixels, canvas, div, point, prelude::*, px};

use crate::theme;

/// A thing that answers a click: a hover, a pressed state, a small part that shows.
pub const FAST: Duration = Duration::from_millis(140);
/// A card, a row, a popover that comes in.
pub const BASE: Duration = Duration::from_millis(200);
/// A panel that slides over the page, a ring that fills.
pub const SLOW: Duration = Duration::from_millis(260);
/// The frames each second of a skeleton or a spinner: enough to read as motion, and a tenth of
/// what the display would draw.
pub const WAIT_FPS: f32 = 12.;

/// Fast at first, then slow to its end: what a thing that comes in does.
pub fn ease_out(t: f32) -> f32 {
    1. - (1. - t.clamp(0., 1.)).powi(3)
}

/// The element fades in when it first shows.
pub fn fade_in<E: Styled + IntoElement + 'static>(id: impl Into<ElementId>, element: E) -> AnimationElement<E> {
    element.with_animation(id, Animation::new(BASE).with_easing(ease_out), |element, t| element.opacity(t))
}

/// As [`fade_in`], after `order` steps of 30 ms: the rows of a list come in one after the other.
/// No more than eight steps, so a long list is there within a quarter of a second.
pub fn fade_in_after<E: Styled + IntoElement + 'static>(id: impl Into<ElementId>, order: usize, element: E) -> AnimationElement<E> {
    let wait = Duration::from_millis(30 * order.min(8) as u64);
    let total = wait + BASE;
    let start = wait.as_secs_f32() / total.as_secs_f32();
    element.with_animation(id, Animation::new(total), move |element, t| element.opacity(ease_out(((t - start) / (1. - start)).max(0.))))
}

/// A panel at the right edge of its (relative) parent slides in from outside it and fades in.
/// The panel must be `absolute()`; `width` is its width on the screen.
pub fn slide_in_right(id: impl Into<ElementId>, width: Pixels, panel: Div) -> AnimationElement<Div> {
    panel.with_animation(id, Animation::new(SLOW).with_easing(ease_out), move |panel, t| panel.right(width * (t - 1.) * 0.25).opacity(t))
}

/// The dark layer under a panel or a dialog fades in.
pub fn scrim_in(id: impl Into<ElementId>, scrim: Div) -> AnimationElement<Div> {
    scrim.with_animation(id, Animation::new(BASE), |scrim, t| scrim.opacity(t))
}

/// A grey block that stands for content that is still loading, and breathes while it waits.
/// Give it the size of what comes: `skeleton("pr-title", 180., 12.)`.
pub fn skeleton(id: impl Into<ElementId>, width: f32, height: f32) -> AnimationElement<Div> {
    skeleton_block(id, div().w(theme::sz(width)).h(theme::sz(height)))
}

/// As [`skeleton`], for a block that takes its size from its parent (`.w_full()`, `.flex_1()`).
pub fn skeleton_block(id: impl Into<ElementId>, block: Div) -> AnimationElement<Div> {
    block
        .flex_none()
        .rounded(theme::sz(6.))
        .bg(theme::hover())
        .with_animation(id, Animation::new(Duration::from_millis(1400)).repeat().with_max_fps(WAIT_FPS), |block, t| block.opacity(0.45 + 0.55 * (0.5 - 0.5 * (t * TAU).cos())))
}

/// Rows of skeletons for a list that loads: a mark, a title and a line under it, `rows` times.
pub fn skeleton_rows(id: &'static str, rows: usize) -> Div {
    div().flex().flex_col().children((0..rows).map(|row| {
        div()
            .flex()
            .items_center()
            .gap(theme::sz(12.))
            .px(theme::sz(16.))
            .py(theme::sz(12.))
            .child(skeleton((id, row * 3), 16., 16.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::sz(7.))
                    .child(skeleton((id, row * 3 + 1), 260. - 40. * (row % 3) as f32, 11.))
                    .child(skeleton((id, row * 3 + 2), 150. + 30. * (row % 2) as f32, 9.)),
            )
    }))
}

/// A turning arc for work whose end is not known: a refresh, a sign-in, a download.
pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> AnimationElement<Div> {
    let size = theme::sz(size);
    div().flex_none().size(size).with_animation(id, Animation::new(Duration::from_millis(900)).repeat().with_max_fps(WAIT_FPS), move |holder, t| {
        holder.child(arc(size, (size / 8.).max(px(1.5)), t * TAU, 0.7, color, None))
    })
}

/// A ring filled to `fraction` (0 to 1), as a radial chart: `size` across, `thickness` of the
/// line, both at 100 %. The fill starts at the top and goes clockwise; the rest of the ring is
/// the track.
pub fn ring(fraction: f32, size: f32, thickness: f32, color: Hsla) -> Div {
    let (size, thickness) = (theme::sz(size), theme::sz(thickness));
    div().flex_none().size(size).child(arc(size, thickness, 0., clean(fraction), color, Some(theme::hover())))
}

/// As [`ring`], and the fill grows from nothing when the ring first shows.
pub fn ring_in(id: impl Into<ElementId>, fraction: f32, size: f32, thickness: f32, color: Hsla) -> AnimationElement<Div> {
    let (size, thickness, fraction) = (theme::sz(size), theme::sz(thickness), clean(fraction));
    div().flex_none().size(size).with_animation(id, Animation::new(SLOW * 2).with_easing(ease_out), move |holder, t| {
        holder.child(arc(size, thickness, 0., fraction * t, color, Some(theme::hover())))
    })
}

fn clean(fraction: f32) -> f32 {
    if fraction.is_nan() { 0. } else { fraction.clamp(0., 1.) }
}

/// An arc of a circle `size` across: from `start` (radians, clockwise from the top) over `part`
/// of the circle. `track` draws the whole circle under it.
fn arc(size: Pixels, thickness: Pixels, start: f32, part: f32, color: Hsla, track: Option<Hsla>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let radius = (bounds.size.width.min(bounds.size.height) - thickness) / 2.;
            let center = bounds.center();
            let at = |angle: f32| {
                let angle = angle - FRAC_PI_2;
                point(center.x + radius * angle.cos(), center.y + radius * angle.sin())
            };
            // An arc is drawn as short lines: one for each 4 degrees is round at these sizes, and
            // has no case that an arc of a whole circle would have.
            let mut stroke = |from: f32, part: f32, color: Hsla| {
                if part <= 0. {
                    return;
                }
                let steps = ((part * 90.).ceil() as usize).max(2);
                let mut path = PathBuilder::stroke(thickness);
                path.move_to(at(from));
                for step in 1..=steps {
                    path.line_to(at(from + part * TAU * step as f32 / steps as f32));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            if let Some(track) = track {
                stroke(0., 1., track);
            }
            stroke(start, part, color);
        },
    )
    .size(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_element_that_comes_in_ends_where_it_stays() {
        assert_eq!(ease_out(0.), 0.);
        assert_eq!(ease_out(1.), 1.);
        assert_eq!(ease_out(2.), 1.);
        assert!(ease_out(0.5) > 0.5, "fast at first");
        assert_eq!(clean(f32::NAN), 0.);
        assert_eq!(clean(1.4), 1.);
        assert_eq!(clean(-0.2), 0.);
    }
}
