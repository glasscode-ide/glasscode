//! Small styled building blocks shared by the workspace and dialogs, in the Glasscode style:
//! 32 px buttons with 8 px corners (the accent fills the primary one: ink in Glasscode), chips,
//! checkboxes, a switch, badges and 14 px cards.

use gpui::{Div, ElementId, FontWeight, Hsla, SharedString, Stateful, div, prelude::*, px};

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
        .h(px(BUTTON_HEIGHT))
        .px_3()
        .rounded(px(RADIUS))
        .text_size(px(13.))
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
        .size(px(26.))
        .rounded(px(6.))
        .text_color(theme::muted())
        .text_size(px(14.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()).text_color(theme::text_strong()))
        .child(glyph)
}

pub fn dot(color: Hsla) -> Div {
    div().flex_none().size(px(7.)).rounded_full().bg(color)
}

pub fn caps_label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(11.))
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
        .min_w(px(150.))
        .gap_1()
        .px_3p5()
        .py_2p5()
        .rounded(px(8.))
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
        .child(div().text_size(px(12.)).text_color(theme::muted()).child(detail.into()))
}

pub fn chip(id: impl Into<ElementId>, selected: bool, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1p5()
        .h(px(BUTTON_HEIGHT))
        .px_3()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(if selected { theme::accent() } else { theme::border_strong() })
        .bg(if selected { theme::selected() } else { theme::raised() })
        .text_color(if selected { theme::text_strong() } else { theme::text() })
        .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
        .text_size(px(13.))
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
        .h(px(28.))
        .px_3()
        .rounded(px(6.))
        .text_size(px(13.))
        .cursor_pointer()
        .when(selected, |d| d.bg(theme::selected()).text_color(theme::text_strong()).font_weight(FontWeight::SEMIBOLD))
        .when(!selected, |d| d.text_color(theme::muted()).hover(|s| s.text_color(theme::text_strong())))
        .child(label.into())
}

/// The frame around [`segment`]s.
pub fn segmented() -> Div {
    div().flex().flex_wrap().items_center().gap_0p5().p_0p5().rounded(px(RADIUS)).border_1().border_color(theme::border()).bg(theme::raised())
}

pub fn checkbox(id: impl Into<ElementId>, checked: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .rounded(px(4.))
        .border_1()
        .border_color(if checked { theme::accent() } else { theme::border_strong() })
        .bg(if checked { theme::accent() } else { theme::input() })
        .text_color(theme::on_accent())
        .text_size(px(10.5))
        .font_weight(FontWeight::BOLD)
        .cursor_pointer()
        .when(checked, |d| d.child("✓"))
}

/// An on/off switch, as in Settings.
pub fn switch(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .when(on, |d| d.justify_end())
        .w(px(36.))
        .h(px(20.))
        .p(px(2.))
        .rounded_full()
        .bg(if on { theme::accent() } else { theme::selected() })
        .border_1()
        .border_color(if on { theme::accent() } else { theme::border_strong() })
        .cursor_pointer()
        .child(div().size(px(14.)).rounded_full().bg(if on { theme::on_accent() } else { theme::muted() }))
}

pub fn badge(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex_none()
        .px_2()
        .rounded_full()
        .border_1()
        .border_color(color)
        .text_color(color)
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

/// A card: rows or content on the raised surface, with 14 px corners.
pub fn card() -> Div {
    div().flex().flex_col().rounded(px(CARD_RADIUS)).border_1().border_color(theme::border()).bg(theme::raised()).shadow_sm()
}
