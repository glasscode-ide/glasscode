//! The UI kit on one page, for screen checks in a debug build: the glass colours with their
//! contrast, each icon and brand mark, and each helper of `ui.rs`. `GLASSCODE_DEBUG_OPEN=kit`
//! opens it; `kit:light`, `kit:dark`, `kit:classic` or `kit:contrast` also sets that theme, as
//! the Themes page does (so it is saved).

use gpui::{AnyElement, ClickEvent, Context, Div, FontWeight, Hsla, ScrollHandle, SharedString, div, linear_color_stop, linear_gradient, prelude::*, px};
use glasscode_core::model::{AgentKind, ThemeChoice};

use crate::icons::{self, Brand};
use crate::sidebar::{self, RowState};
use crate::theme;
use crate::ui;
use crate::workspace::{NewSession, OpenPalette, Workspace};

#[derive(Default)]
pub struct Kit {
    page: ScrollHandle,
    list: ScrollHandle,
}

const THEMES: [(ThemeChoice, &str); 4] =
    [(ThemeChoice::Light, "Light"), (ThemeChoice::Dark, "Dark"), (ThemeChoice::Classic, "Classic"), (ThemeChoice::Contrast, "High contrast")];

/// The theme a `GLASSCODE_DEBUG_OPEN` value names: `kit:dark` and so on.
fn theme_of(mode: &str) -> Option<ThemeChoice> {
    match mode.strip_prefix("kit:")? {
        "light" => Some(ThemeChoice::Light),
        "dark" => Some(ThemeChoice::Dark),
        "classic" => Some(ThemeChoice::Classic),
        "contrast" => Some(ThemeChoice::Contrast),
        _ => None,
    }
}

fn heading(text: &'static str) -> Div {
    div().pt_4().pb_1p5().text_size(theme::sz(11.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child(text)
}

fn label(text: impl Into<SharedString>) -> Div {
    div().text_size(theme::sz(11.)).text_color(theme::muted()).child(text.into())
}

/// A glass card as the screens will draw it until U04 adds the blur: the colour, its edge and
/// the light along its top.
fn glass_card(fill: Hsla) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .gap_1()
        .w(theme::sz(260.))
        .p_3()
        .rounded(theme::sz(12.))
        .bg(fill)
        .border_1()
        .border_color(theme::glass_border())
        .child(div().absolute().top_0().left(theme::sz(12.)).right(theme::sz(12.)).h(px(1.)).bg(theme::glass_highlight()))
}

impl Workspace {
    pub fn open_kit(&mut self, mode: &str, cx: &mut Context<Self>) {
        self.kit = Some(Kit::default());
        if let Some(choice) = theme_of(mode) {
            self.set_appearance(|a| a.theme = choice, cx);
        }
        cx.notify();
    }

    /// The page, over everything below the title bar, while it is open.
    pub fn render_kit(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let kit = self.kit.as_ref()?;
        let (page, list) = (kit.page.clone(), kit.list.clone());
        let current = self.state.appearance.theme;

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(theme::border())
            .child(div().text_size(theme::sz(15.)).font_weight(FontWeight::SEMIBOLD).child("UI kit"))
            .child(label(theme::palette().name))
            .child(div().flex_1())
            .child(ui::segmented().children(THEMES.into_iter().map(|(choice, name)| {
                ui::segment(SharedString::from(format!("kit-theme-{name}")), current == choice, name)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_appearance(|a| a.theme = choice, cx)))
            })))
            .child(ui::button("kit-close", "Close").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.kit = None;
                cx.notify();
            })));

        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .px_5()
            .pb_8()
            .child(heading("GLASS"))
            .child(Self::kit_glass(cx))
            .child(heading("COLOURS"))
            .child(kit_colours())
            .child(heading("ICONS"))
            .child(div().flex().flex_wrap().gap_2().children(icons::ALL.into_iter().map(|(name, data)| {
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1p5()
                    .w(theme::sz(84.))
                    .py_2()
                    .rounded(theme::sz(8.))
                    .bg(theme::raised())
                    .child(icons::icon(data, theme::sz(18.), theme::sz(18.), theme::text()))
                    .child(label(name))
            })))
            .child(heading("BRANDS"))
            .child(div().flex().flex_wrap().gap_2().children(Brand::ALL.into_iter().map(|brand| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .w(theme::sz(120.))
                    .p_2()
                    .rounded(theme::sz(8.))
                    .bg(theme::raised())
                    .child(div().flex().items_end().gap_2().children([14., 18., 24.].map(|size| icons::brand(brand, size))))
                    .child(label(brand.name()))
            })))
            .child(
                div().flex().items_center().gap_3().children([AgentKind::Claude, AgentKind::Codex, AgentKind::Shell].map(|agent| {
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(sidebar::agent_icon(agent, 22., true))
                        .child(sidebar::agent_icon(agent, 22., false))
                        .child(label(format!("agent_icon: {}, running and not", agent.name())))
                })),
            )
            .child(heading("HELPERS"))
            .child(Self::kit_helpers(cx))
            .child(heading("SCROLL AREA"))
            .child(label("The page and this list: a thumb shows while the pointer is over them or they scroll, and can be dragged."))
            .child(
                div().flex().h(theme::sz(160.)).w(theme::sz(320.)).rounded(theme::sz(8.)).border_1().border_color(theme::border()).child(ui::scroll_area(
                    "kit-list",
                    &list,
                    div().flex().flex_col().children((1..=40).map(|n| div().px_3().py_1().text_color(theme::text_soft()).child(format!("Row {n}")))),
                )),
            );

        Some(
            div()
                .id("kit")
                .absolute()
                // Below the title bar, which stays in use.
                .top(theme::sz(44.))
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .flex_col()
                .bg(theme::bg())
                .occlude()
                .child(header)
                .child(ui::scroll_area("kit-page", &page, body))
                .into_any_element(),
        )
    }

    /// Glass cards on the plain background and on a band of the theme's colours (the backdrop
    /// comes with U04), and the contrast of their text.
    fn kit_glass(cx: &mut Context<Self>) -> Div {
        let cards = |strong: bool, cx: &mut Context<Self>| {
            let fill = if strong { theme::glass_strong() } else { theme::glass() };
            let key = if strong { "strong" } else { "glass" };
            glass_card(fill)
                .child(div().text_color(theme::text()).font_weight(FontWeight::MEDIUM).child(if strong { "glass_strong" } else { "glass" }))
                .child(div().text_size(theme::sz(12.)).text_color(theme::card_soft_text()).child("card_soft_text: the second line"))
                .child(
                    div()
                        .id(SharedString::from(format!("kit-tint-{key}")))
                        .flex()
                        .items_center()
                        .gap_2()
                        .mt_1()
                        .px_1p5()
                        .py_1()
                        .rounded(theme::sz(6.))
                        .hover(|s| s.bg(theme::tint()))
                        .child(div().flex_1().text_size(theme::sz(12.)).text_color(theme::text_soft()).child("A row: tint on hover"))
                        .child(ui::action_pill(SharedString::from(format!("kit-answer-{key}")), "Answer").on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.show_toast("action_pill pressed", cx)),
                        )),
                )
        };
        let band = linear_gradient(90., linear_color_stop(theme::claude(), 0.), linear_color_stop(theme::codex(), 1.));
        let over = |surface: Hsla| theme::bg().blend(surface);
        let ratios = [
            ("text on glass", theme::contrast(theme::text(), over(theme::glass()))),
            ("card_soft_text on glass", theme::contrast(theme::card_soft_text(), over(theme::glass()))),
            ("text on glass_strong", theme::contrast(theme::text(), over(theme::glass_strong()))),
            ("card_soft_text on glass_strong", theme::contrast(theme::card_soft_text(), over(theme::glass_strong()))),
            ("on_action on action", theme::contrast(theme::on_action(), theme::action())),
        ];
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().flex().flex_wrap().gap_4().child(cards(false, cx)).child(cards(true, cx)))
            .child(div().flex().flex_wrap().gap_4().p_4().rounded(theme::sz(14.)).bg(band).child(cards(false, cx)).child(cards(true, cx)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x_5()
                    .gap_y_1()
                    .children(ratios.map(|(what, ratio)| label(format!("{what}: {ratio:.1}")))),
            )
    }

    fn kit_helpers(cx: &mut Context<Self>) -> Div {
        let row = |name: &'static str| div().flex().flex_wrap().items_center().gap_3().child(div().w(theme::sz(110.)).child(label(name)));
        let meter = |fraction: f32, hot: bool| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .w(theme::sz(170.))
                .child(ui::meter(fraction, hot))
                .child(div().w(theme::sz(36.)).text_size(theme::sz(12.)).text_color(theme::text_soft()).child(format!("{:.0}%", fraction * 100.)))
        };
        let dot = |state: RowState, done: bool, name: &'static str| div().flex().items_center().gap_1p5().child(ui::status_dot(state, done)).child(label(name));
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(row("kbd").child(ui::kbd(crate::keymap::label(&OpenPalette))).child(ui::kbd(crate::keymap::label(&NewSession))))
            .child(
                row("pill")
                    .child(ui::pill("kit-pill", "glasscode").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.show_toast("pill pressed", cx))))
                    .child(
                        ui::pill("kit-pill-more", "Claude · Personal")
                            .child(icons::icon(icons::CHEVRON_DOWN, theme::sz(13.), theme::sz(13.), theme::text_soft()))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.show_toast("pill pressed", cx))),
                    ),
            )
            .child(
                row("action_pill")
                    .child(ui::action_pill("kit-fix", "Fix with agent").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.show_toast("action_pill pressed", cx)))),
            )
            .child(row("meter").child(meter(0., false)).child(meter(0.22, false)).child(meter(0.84, true)).child(meter(1., true)))
            .child(
                row("status_dot")
                    .child(dot(RowState::NeedsYou, false, "needs you"))
                    .child(dot(RowState::Working, false, "working"))
                    .child(dot(RowState::Idle, false, "idle"))
                    .child(dot(RowState::Idle, true, "done")),
            )
    }
}

/// Each new colour of the palette, as a swatch on the background.
fn kit_colours() -> Div {
    let colours = [
        ("glass", theme::glass()),
        ("glass_strong", theme::glass_strong()),
        ("glass_border", theme::glass_border()),
        ("glass_highlight", theme::glass_highlight()),
        ("card_soft_text", theme::card_soft_text()),
        ("tint", theme::tint()),
        ("action", theme::action()),
        ("on_action", theme::on_action()),
    ];
    div().flex().flex_wrap().gap_3().children(colours.map(|(name, colour)| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .w(theme::sz(170.))
            .child(div().size(theme::sz(26.)).rounded(theme::sz(6.)).border_1().border_color(theme::border()).bg(colour))
            .child(label(name))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hook_names_a_theme() {
        assert_eq!(theme_of("kit"), None);
        assert_eq!(theme_of("kit:dark"), Some(ThemeChoice::Dark));
        assert_eq!(theme_of("kit:contrast"), Some(ThemeChoice::Contrast));
        assert_eq!(theme_of("kit:pink"), None);
    }
}
