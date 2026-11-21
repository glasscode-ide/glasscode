//! Small styled building blocks shared by the workspace and dialogs, in the Glasscode style:
//! 32 px buttons with 8 px corners (the accent fills the primary one: ink in Glasscode), chips,
//! checkboxes, a switch, badges and 14 px cards. For the redesign: a scroll area with its own
//! thin thumb, keycaps, pills, a meter and a state dot.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, DispatchPhase, Div, ElementId, FontWeight, Hitbox, HitboxBehavior, Hsla, MouseButton, MouseDownEvent,
    MouseExitEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollHandle, SharedString, Stateful, Window, canvas, div, fill, point,
    prelude::*, px, relative, size,
};

use crate::sidebar::RowState;
use crate::theme;

const RADIUS: f32 = 8.;
pub const BUTTON_HEIGHT: f32 = 32.;
pub const CARD_RADIUS: f32 = 14.;

fn base_button(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap_1p5()
        .h(theme::sz(BUTTON_HEIGHT))
        .px_3()
        .rounded(theme::sz(RADIUS))
        .text_size(theme::sz(13.))
        .whitespace_nowrap()
}

pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    base_button(id)
        .border_1()
        .border_color(theme::border_strong())
        .bg(theme::raised())
        .text_color(theme::text())
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()).text_color(theme::text_strong()))
        .child(label.into())
}

pub fn primary_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    base_button(id)
        .px_3p5()
        .bg(theme::accent())
        .text_color(theme::on_accent())
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|s| s.opacity(0.88))
        .child(label.into())
}

/// For actions that delete work.
pub fn danger_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    base_button(id)
        .px_3p5()
        .bg(theme::danger())
        .text_color(theme::on_danger())
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|s| s.opacity(0.88))
        .child(label.into())
}

pub fn disabled_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    base_button(id)
        .px_3p5()
        .bg(theme::selected())
        .text_color(theme::faint())
        .font_weight(FontWeight::SEMIBOLD)
        .child(label.into())
}

pub fn icon_button(id: impl Into<ElementId>, glyph: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(theme::sz(26.))
        .rounded(theme::sz(6.))
        .text_color(theme::muted())
        .text_size(theme::sz(14.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()).text_color(theme::text_strong()))
        .child(glyph)
}

pub fn dot(color: Hsla) -> Div {
    div().flex_none().size(theme::sz(7.)).rounded_full().bg(color)
}

pub fn caps_label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(theme::sz(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::muted())
        .child(text.into())
}

/// A selectable card (used for radio-style choices in dialogs).
pub fn choice_card(
    id: impl Into<ElementId>,
    selected: bool,
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .flex_1()
        .min_w(theme::sz(150.))
        .gap_1()
        .px_3p5()
        .py_2p5()
        .rounded(theme::sz(8.))
        .border_1()
        .border_color(if selected { theme::accent() } else { theme::border() })
        .when(selected, |d| d.bg(theme::selected()))
        .cursor_pointer()
        .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
        .child(
            div()
                .text_color(if selected { theme::text_strong() } else { theme::text_soft() })
                .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                .child(title.into()),
        )
        .child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(detail.into()))
}

pub fn chip(id: impl Into<ElementId>, selected: bool, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1p5()
        .h(theme::sz(BUTTON_HEIGHT))
        .px_3()
        .rounded(theme::sz(RADIUS))
        .border_1()
        .border_color(if selected { theme::accent() } else { theme::border_strong() })
        .bg(if selected { theme::selected() } else { theme::raised() })
        .text_color(if selected { theme::text_strong() } else { theme::text() })
        .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
        .text_size(theme::sz(13.))
        .cursor_pointer()
        .hover(|s| s.border_color(theme::faint()))
        .child(label.into())
}

/// A segment of a segmented control (put several in [`segmented`]).
pub fn segment(id: impl Into<ElementId>, selected: bool, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(theme::sz(28.))
        .px_3()
        .rounded(theme::sz(6.))
        .text_size(theme::sz(13.))
        .cursor_pointer()
        .when(selected, |d| d.bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD))
        .when(!selected, |d| d.text_color(theme::muted()).hover(|s| s.text_color(theme::text_strong())))
        .child(label.into())
}

/// The frame around [`segment`]s.
pub fn segmented() -> Div {
    div().flex().flex_wrap().items_center().gap_0p5().p_0p5().rounded(theme::sz(RADIUS)).border_1().border_color(theme::border()).bg(theme::raised())
}

pub fn checkbox(id: impl Into<ElementId>, checked: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(theme::sz(16.))
        .rounded(theme::sz(4.))
        .border_1()
        .border_color(if checked { theme::accent() } else { theme::border_strong() })
        .bg(if checked { theme::accent() } else { theme::input() })
        .text_color(theme::on_accent())
        .text_size(theme::sz(10.5))
        .font_weight(FontWeight::BOLD)
        .cursor_pointer()
        .when(checked, |d| d.child("✓"))
}

/// An on/off switch, as in Settings. The knob slides when the switch changes: a spring that
/// starts at its end when the switch first shows, so a page of switches stands still as it opens.
pub fn switch(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    use gpui::AnimationExt as _;
    // From the left end to the right one: the track without its padding, its edges and the knob.
    let travel = theme::sz(36. - 2. * 2. - 14.) - px(2.);
    let knob = div().flex_none().size(theme::sz(14.)).rounded_full().bg(if on { theme::on_accent() } else { theme::muted() });
    let spring = gpui::SpringAnimation::new(gpui::SpringConfig::new(600., 42., 1.)).to(if on { 1.0_f32 } else { 0. });
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .w(theme::sz(36.))
        .h(theme::sz(20.))
        .p(theme::sz(2.))
        .rounded_full()
        .bg(if on { theme::accent() } else { theme::selected() })
        .border_1()
        .border_color(if on { theme::accent() } else { theme::border_strong() })
        .cursor_pointer()
        .child(knob.with_spring("knob", spring, move |knob, at: f32| knob.ml(travel * at.clamp(0., 1.))))
}

pub fn badge(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex_none()
        .px_2()
        .rounded_full()
        .border_1()
        .border_color(color)
        .text_color(color)
        .text_size(theme::sz(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

/// A card: rows or content on the raised surface, with 14 px corners.
pub fn card() -> Div {
    div().flex().flex_col().rounded(theme::sz(CARD_RADIUS)).border_1().border_color(theme::border()).bg(theme::raised()).shadow_sm()
}

/// A key or a shortcut as a keycap. Take the text from `keymap::label`.
pub fn kbd(label: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .px(theme::sz(5.))
        .rounded(theme::sz(4.))
        .border_1()
        .border_b_2()
        .border_color(theme::border())
        .bg(theme::panel())
        .font_family(theme::MONO_FONT)
        .text_size(theme::sz(11.))
        .text_color(theme::text_soft())
        .whitespace_nowrap()
        .child(label.into())
}

/// A small choice on glass (the agent, the project, the start point): a quiet edge and a tint
/// on hover. Add an icon or a chevron as more children.
pub fn pill(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(theme::sz(6.))
        .px(theme::sz(9.))
        .py(theme::sz(4.))
        .rounded(theme::sz(7.))
        .border_1()
        .border_color(theme::border())
        .text_size(theme::sz(12.))
        .text_color(theme::text_soft())
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(|s| s.bg(theme::tint()).text_color(theme::text()))
        .child(label.into())
}

/// The pill of the one thing to do now ("Answer", "Fix with agent").
pub fn action_pill(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .px(theme::sz(8.))
        .py(theme::sz(2.))
        .rounded(theme::sz(5.))
        .bg(theme::action())
        .text_color(theme::on_action())
        .text_size(theme::sz(11.5))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(|s| s.opacity(0.88))
        .child(label.into())
}

/// A thin bar filled to `fraction` (0 to 1); `hot` makes the fill orange, for a limit that is
/// near. It grows in a row; give it a width to stop that.
pub fn meter(fraction: f32, hot: bool) -> Div {
    div()
        .flex_1()
        .min_w(theme::sz(24.))
        .h(theme::sz(4.))
        .rounded_full()
        .bg(theme::hover())
        .overflow_hidden()
        .child(div().h_full().w(relative(meter_fill(fraction))).rounded_full().bg(if hot { theme::attention() } else { theme::text_soft() }))
}

fn meter_fill(fraction: f32) -> f32 {
    if fraction.is_nan() { 0. } else { fraction.clamp(0., 1.) }
}

/// A session's state as a dot: orange when it needs you, green while it works, grey when idle.
/// `done` draws an empty ring instead.
///
/// The green dot stands still (the owner, 2026-11-17): a pulse is a GPUI animation, which draws
/// the whole window on each display frame, about one core for as long as a session works (U24).
pub fn status_dot(state: RowState, done: bool) -> AnyElement {
    let dot = div().flex_none().size(theme::sz(7.)).rounded_full();
    if done {
        return dot.border(px(1.5)).border_color(theme::faint()).into_any_element();
    }
    match state {
        RowState::NeedsYou => dot.bg(theme::attention()).into_any_element(),
        RowState::Working => dot.bg(theme::added()).into_any_element(),
        RowState::Idle => dot.bg(theme::border_strong()).into_any_element(),
    }
}

// ----- tooltip -----------------------------------------------------------------------------

/// The words that show when the pointer rests on a control: `.tooltip(ui::tip("Board"))`. For a
/// control that shows an icon without its word, so that the word is one rest of the pointer away.
pub fn tip(words: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    let words: SharedString = words.into();
    move |_, cx| cx.new(|_| Tip(words.clone())).into()
}

struct Tip(SharedString);

impl gpui::Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .px(theme::sz(8.))
            .py(theme::sz(4.))
            .rounded(theme::sz(6.))
            .border_1()
            .border_color(theme::border_strong())
            .bg(theme::raised())
            .shadow_md()
            .text_size(theme::sz(12.))
            .text_color(theme::text())
            .child(self.0.clone())
    }
}

// ----- scroll area -------------------------------------------------------------------------

/// How long the thumb stays after the area scrolls or the pointer leaves it.
pub(crate) const THUMB_STAYS: Duration = Duration::from_millis(900);
const THUMB_WIDTH: f32 = 5.;
/// From the area's right edge to the thumb.
const THUMB_GAP: f32 = 3.;
const THUMB_MIN: f32 = 24.;

/// A scrolling column with no scroll bar of its own: a 5 px thumb shows while the pointer is
/// over it or it scrolls, goes 900 ms later, and can be dragged. The area fills its parent (it
/// grows in a flex parent and can shrink below its content); the content is laid out as a
/// column at the area's width.
pub fn scroll_area(id: impl Into<ElementId>, handle: &ScrollHandle, content: impl IntoElement) -> AnyElement {
    let handle = handle.clone();
    div()
        .id(id)
        .relative()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .size_full()
        .child(div().id("scroll-area-content").size_full().flex().flex_col().overflow_y_scroll().track_scroll(&handle).child(content))
        .child(
            canvas(|bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal), move |bounds, hitbox, window, cx| {
                paint_thumb(bounds, hitbox, handle, window, cx)
            })
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .into_any_element()
}

/// Where the thumb goes, as in the prototype: its top (from the top of the area) and its
/// length, or None when the content fits. `viewport` is the area's height, `max_scroll` how
/// far the content can scroll and `scrolled` how far it has, all on the screen; the least length
/// follows the scale.
pub fn thumb(viewport: f32, max_scroll: f32, scrolled: f32) -> Option<(f32, f32)> {
    thumb_at_least(viewport, max_scroll, scrolled, f32::from(theme::sz(THUMB_MIN)))
}

fn thumb_at_least(viewport: f32, max_scroll: f32, scrolled: f32, least: f32) -> Option<(f32, f32)> {
    if viewport <= 0. || max_scroll <= 1. {
        return None;
    }
    let length = (viewport * viewport / (viewport + max_scroll)).max(least).min(viewport);
    let top = (viewport - length) * (scrolled / max_scroll).clamp(0., 1.);
    Some((top, length))
}

/// How far to scroll so that the thumb's top is at `top`: the drag of the thumb.
pub fn scrolled_for_thumb(viewport: f32, max_scroll: f32, top: f32) -> f32 {
    match thumb(viewport, max_scroll, 0.) {
        Some((_, length)) if viewport > length => (top / (viewport - length)).clamp(0., 1.) * max_scroll,
        _ => 0.,
    }
}

/// The thumb's place at the right edge of `area` (in the window) for content that can scroll
/// `max_scroll` and has scrolled `scrolled`, on the screen. The editor draws its thumb with it.
pub fn thumb_place(area: Bounds<Pixels>, max_scroll: f32, scrolled: f32) -> Option<Bounds<Pixels>> {
    let (top, length) = thumb(f32::from(area.size.height), max_scroll, scrolled)?;
    Some(Bounds::new(point(area.right() - theme::sz(THUMB_WIDTH + THUMB_GAP), area.top() + px(top)), size(theme::sz(THUMB_WIDTH), px(length))))
}

/// The thumb's place in the window for the scroll offset the handle has now.
fn thumb_bounds(area: Bounds<Pixels>, handle: &ScrollHandle) -> Option<Bounds<Pixels>> {
    thumb_place(area, f32::from(handle.max_offset().y), -f32::from(handle.offset().y))
}

/// Where in the thumb a press at `position` takes it, or None when the press is not on it. The
/// thumb is a little wider to take than drawn, as 5 px is hard to hit.
pub(crate) fn grab_at(thumb: Bounds<Pixels>, position: Point<Pixels>) -> Option<Pixels> {
    thumb.dilate(theme::sz(THUMB_GAP)).contains(&position).then(|| position.y - thumb.top())
}

/// The sideways thumb's place along the foot of `area`, for text that is wider than the area;
/// None when the text fits.
pub(crate) fn thumb_place_across(area: Bounds<Pixels>, max_scroll: f32, scrolled: f32) -> Option<Bounds<Pixels>> {
    let (left, length) = thumb(f32::from(area.size.width), max_scroll, scrolled)?;
    Some(Bounds::new(point(area.left() + px(left), area.bottom() - theme::sz(THUMB_WIDTH + THUMB_GAP)), size(px(length), theme::sz(THUMB_WIDTH))))
}

/// As [`grab_at`], for the sideways thumb: where in it, from its left, a press takes it.
pub(crate) fn grab_across(thumb: Bounds<Pixels>, position: Point<Pixels>) -> Option<Pixels> {
    thumb.dilate(theme::sz(THUMB_GAP)).contains(&position).then(|| position.x - thumb.left())
}

/// A sideways thumb for a list whose lines do not wrap (a file's lines): it shows for as long as
/// a line is wider than the list, so that it is plain there is more text, and it can be dragged.
/// The wheel has no sideways turn on most mice (Shift with the wheel does it). Put it after the
/// list, in a `relative()` parent: it takes a press on the thumb before the lines do. `grab`
/// keeps a drag from frame to frame.
pub fn across_bar(handle: ScrollHandle, grab: std::rc::Rc<std::cell::Cell<Option<Pixels>>>) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let area = handle.bounds();
            let max = f32::from(handle.max_offset().x);
            let Some(thumb) = thumb_place_across(area, max, -f32::from(handle.offset().x)) else {
                grab.set(None);
                return;
            };
            paint_thumb_quad(thumb, window);
            let taken = grab.clone();
            window.on_mouse_event(move |event: &gpui::MouseDownEvent, phase, _, cx| {
                if phase != gpui::DispatchPhase::Bubble || event.button != gpui::MouseButton::Left {
                    return;
                }
                if let Some(at) = grab_across(thumb, event.position) {
                    taken.set(Some(at));
                    cx.stop_propagation();
                }
            });
            let (taken, moved) = (grab.clone(), handle.clone());
            window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, window, _| {
                let Some(at) = taken.get().filter(|_| phase == gpui::DispatchPhase::Bubble) else { return };
                // A drag that ended outside the window.
                if event.pressed_button != Some(gpui::MouseButton::Left) {
                    taken.set(None);
                    return;
                }
                let mut offset = moved.offset();
                offset.x = -px(scrolled_for_thumb(f32::from(area.size.width), max, f32::from(event.position.x - area.left() - at)));
                moved.set_offset(offset);
                window.refresh();
            });
            let taken = grab.clone();
            window.on_mouse_event(move |event: &gpui::MouseUpEvent, phase, _, _| {
                if phase == gpui::DispatchPhase::Bubble && event.button == gpui::MouseButton::Left {
                    taken.set(None);
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

/// The thumb as it is drawn.
pub(crate) fn paint_thumb_quad(bounds: Bounds<Pixels>, window: &mut Window) {
    let mut color = theme::border_strong();
    color.a = 0.85;
    window.paint_quad(fill(bounds, color).corner_radii(theme::sz(3.)));
}

/// What a scroll area remembers between frames.
#[derive(Default)]
struct ThumbState {
    hovered: bool,
    /// When the area last scrolled, or the pointer left it.
    active_at: Option<Instant>,
    /// The scroll offset at the last paint, to see a scroll of any cause (wheel, keys, code).
    offset: Option<Pixels>,
    /// A drag of the thumb: where in the thumb the pointer took it.
    grab: Option<Pixels>,
    /// A repaint is asked for then, to take the thumb away.
    wake_at: Option<Instant>,
}

impl ThumbState {
    fn shown(&self, now: Instant) -> bool {
        self.hovered || self.grab.is_some() || self.active_at.is_some_and(|at| now.duration_since(at) < THUMB_STAYS)
    }
}

fn paint_thumb(area: Bounds<Pixels>, hitbox: Hitbox, handle: ScrollHandle, window: &mut Window, cx: &mut App) {
    let state = window.use_keyed_state("scroll-thumb", cx, |_, _| ThumbState::default());
    let now = Instant::now();
    let viewport = f32::from(area.size.height);
    let offset = handle.offset().y;
    let (shown, wake) = state.update(cx, |s, _| {
        if s.offset.is_some_and(|last| last != offset) {
            s.active_at = Some(now);
        }
        s.offset = Some(offset);
        let shown = s.shown(now);
        // Without a pointer or a drag to keep it, the thumb goes when its time is up.
        let hide_at = s.active_at.filter(|_| shown && !s.hovered && s.grab.is_none()).map(|at| at + THUMB_STAYS);
        let wake = hide_at.filter(|at| s.wake_at.is_none_or(|w| w < *at));
        if wake.is_some() {
            s.wake_at = wake;
        }
        (shown, wake)
    });
    if let Some(at) = wake {
        let weak = state.downgrade();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(at.saturating_duration_since(Instant::now())).await;
            let _ = weak.update(cx, |s, cx| {
                s.wake_at = None;
                cx.notify();
            });
        })
        .detach();
    }

    if let Some(bounds) = thumb_bounds(area, &handle).filter(|_| shown) {
        paint_thumb_quad(bounds, window);
    }

    // Take the thumb. The handlers read the thumb's place from the handle when the event comes,
    // not from this paint: a press can come before the frame that shows the thumb (the pointer
    // arrived and pressed at once), and the hover shows it anyway. Not through a popover that
    // lies over it: the area must be what the pointer is on.
    window.on_mouse_event({
        let (state, hitbox, handle) = (state.clone(), hitbox.clone(), handle.clone());
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !hitbox.is_hovered(window) {
                return;
            }
            if let Some(grab) = thumb_bounds(area, &handle).and_then(|thumb| grab_at(thumb, event.position)) {
                state.update(cx, |s, cx| {
                    s.grab = Some(grab);
                    cx.notify();
                });
                cx.stop_propagation();
            }
        }
    });
    window.on_mouse_event({
        let state = state.clone();
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let grab = state.read(cx).grab;
            if let Some(grab) = grab.filter(|_| event.pressed_button == Some(MouseButton::Left)) {
                let top = f32::from(event.position.y - area.top() - grab);
                let scrolled = scrolled_for_thumb(viewport, f32::from(handle.max_offset().y), top);
                handle.set_offset(point(handle.offset().x, px(-scrolled)));
                state.update(cx, |s, cx| {
                    s.active_at = Some(Instant::now());
                    cx.notify();
                });
                cx.stop_propagation();
                return;
            }
            let over = hitbox.is_hovered(window);
            state.update(cx, |s, cx| {
                // A drag that ended outside the window.
                let ended = s.grab.take().is_some();
                if over != s.hovered || ended {
                    s.hovered = over;
                    s.active_at = Some(Instant::now());
                    cx.notify();
                }
            });
        }
    });
    window.on_mouse_event({
        let state = state.clone();
        move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                state.update(cx, |s, cx| {
                    if s.grab.take().is_some() {
                        s.active_at = Some(Instant::now());
                        cx.notify();
                    }
                });
            }
        }
    });
    // The pointer can leave the window with no last move over the area (one at the window's
    // edge, as the sidebar), and the thumb must still go.
    window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble {
            state.update(cx, |s, cx| {
                if s.hovered {
                    s.hovered = false;
                    s.active_at = Some(Instant::now());
                    cx.notify();
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thumb_moves_from_top_to_end() {
        // 200 px of 800: a quarter as long, 150 px free to move in.
        assert_eq!(thumb(200., 600., 0.), Some((0., 50.)));
        assert_eq!(thumb(200., 600., 300.), Some((75., 50.)));
        assert_eq!(thumb(200., 600., 600.), Some((150., 50.)));
        // Scrolled past an end (a bounce) stays at that end.
        assert_eq!(thumb(200., 600., 700.), Some((150., 50.)));
        assert_eq!(thumb(200., 600., -20.), Some((0., 50.)));
    }

    #[test]
    fn a_long_content_keeps_a_thumb_to_take() {
        let (top, length) = thumb_at_least(200., 99_800., 99_800., THUMB_MIN).unwrap();
        assert_eq!((top, length), (200. - THUMB_MIN, THUMB_MIN));
        // At 130 % the least length is 130 % too.
        let least = THUMB_MIN * 1.3;
        assert_eq!(thumb_at_least(200., 99_800., 99_800., least), Some((200. - least, least)));
        // An area shorter than the least length gives its whole height.
        assert_eq!(thumb_at_least(10., 100., 0., THUMB_MIN), Some((0., 10.)));
    }

    #[test]
    fn no_thumb_when_the_content_fits() {
        assert_eq!(thumb(200., 0., 0.), None);
        assert_eq!(thumb(200., 1., 0.), None);
        assert_eq!(thumb(0., 600., 0.), None);
    }

    #[test]
    fn dragging_the_thumb_scrolls_back_to_where_it_is() {
        for scrolled in [0., 150., 300., 600.] {
            let (top, _) = thumb(200., 600., scrolled).unwrap();
            assert_eq!(scrolled_for_thumb(200., 600., top), scrolled);
        }
        assert_eq!(scrolled_for_thumb(200., 600., -40.), 0.);
        assert_eq!(scrolled_for_thumb(200., 600., 500.), 600.);
        assert_eq!(scrolled_for_thumb(200., 0., 50.), 0.);
    }

    #[test]
    fn a_press_takes_the_thumb_where_it_lands() {
        let thumb = Bounds::new(point(px(1432.), px(102.)), size(px(5.), px(474.)));
        // Held at the point where it was taken.
        assert_eq!(grab_at(thumb, point(px(1434.), px(200.))), Some(px(98.)));
        // A little beside the 5 px still takes it, up to its ends.
        assert_eq!(grab_at(thumb, point(px(1430.), px(103.))), Some(px(1.)));
        assert_eq!(grab_at(thumb, point(px(1439.), px(575.))), Some(px(473.)));
        // The track below it, and the content to its left, are not the thumb.
        assert_eq!(grab_at(thumb, point(px(1434.), px(700.))), None);
        assert_eq!(grab_at(thumb, point(px(1420.), px(200.))), None);
    }

    #[test]
    fn a_meter_fills_between_empty_and_full() {
        assert_eq!(meter_fill(0.42), 0.42);
        assert_eq!(meter_fill(1.7), 1.);
        assert_eq!(meter_fill(-0.2), 0.);
        assert_eq!(meter_fill(f32::NAN), 0.);
    }
}
