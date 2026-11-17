//! Themes: Glasscode Light and Dark, Glasscode classic, high contrast or following macOS, with an
//! accent, the backdrop and its finish, the scale, fonts, text size, density and the terminal
//! cursor. A change shows at once, in every window (terminals included), and is saved in
//! `state.json`. Settings › Appearance draws the controls of this file in its cards.

use gpui::{AnyElement, App, ClickEvent, Context, FontWeight, Hsla, SharedString, WindowAppearance, div, prelude::*};
use glasscode_core::backdrop::Finish;
use glasscode_core::model::{AccentChoice, Appearance, BackdropChoice, ChatFont, CodeFont, CursorStyle, Density, SessionView, TextSize, ThemeChoice, UiFont};

use crate::platform;
use crate::settings_ui::CardId;
use crate::theme::{self, Palette};
use crate::ui;
use crate::workspace::Workspace;

#[derive(Default)]
pub struct ThemesUi {
    /// The theme whose window appearance (light/dark chrome) is set.
    applied: Option<ThemeChoice>,
    /// Debug builds: `GLASSCODE_DEBUG_THEMES=1` opened Settings, Appearance once.
    debug_opened: bool,
    /// Debug builds: `GLASSCODE_DEBUG_SCALE=90|100|115|130` was read once, so that the page and
    /// the UI kit can change the scale after it.
    debug_scaled: bool,
    /// Whether the terminals were last told the theme is light.
    told_light: Option<bool>,
}

fn system_dark(cx: &App) -> bool {
    matches!(cx.window_appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// Put an appearance in effect: the window chrome (light or dark, or following macOS for
/// Auto) and every colour and font. Says whether anything changed.
pub fn apply_appearance(appearance: Appearance, cx: &mut App) -> bool {
    theme::find_system_fonts(cx);
    set_chrome(appearance.theme, cx);
    theme::apply(appearance, system_dark(cx))
}

fn set_chrome(choice: ThemeChoice, cx: &mut App) {
    let forced = match choice {
        ThemeChoice::Auto => None,
        other if theme::palette_for(other, false).light => Some(WindowAppearance::Light),
        _ => Some(WindowAppearance::Dark),
    };
    cx.set_window_appearance(forced);
}

const THEMES: [ThemeChoice; 5] = [ThemeChoice::Auto, ThemeChoice::Light, ThemeChoice::Dark, ThemeChoice::Classic, ThemeChoice::Contrast];

fn theme_name(choice: ThemeChoice) -> &'static str {
    match choice {
        ThemeChoice::Auto => "Auto",
        other => theme::palette_for(other, false).name,
    }
}

impl Workspace {
    /// Follow macOS while on Auto (and anything else that changed the appearance).
    pub fn themes_tick(&mut self, cx: &mut Context<Self>) {
        if cfg!(debug_assertions) && !self.themes.debug_opened && std::env::var_os("GLASSCODE_DEBUG_THEMES").is_some() {
            self.themes.debug_opened = true;
            self.open_appearance(cx);
        }
        if cfg!(debug_assertions) && !self.themes.debug_scaled {
            self.themes.debug_scaled = true;
            if let Some(scale) = std::env::var("GLASSCODE_DEBUG_SCALE").ok().and_then(|v| theme::scale_named(&v)) {
                self.state.appearance.scale = scale;
                self.save();
            }
        }
        let appearance = self.state.appearance;
        if self.themes.applied != Some(appearance.theme) {
            set_chrome(appearance.theme, cx);
            self.themes.applied = Some(appearance.theme);
        }
        if theme::apply(appearance, system_dark(cx)) {
            cx.refresh_windows();
        }
        let light = theme::is_light();
        if self.themes.told_light.is_some_and(|told| told != light) {
            self.tell_claude_the_theme(light, cx);
        }
        self.themes.told_light = Some(light);
    }

    /// Claude (on `"theme": "auto"`) asks the terminal for its background when it starts; this
    /// report (the colour-scheme update terminals send) makes it look again after a change.
    /// Only Claude gets it: a shell would print it.
    fn tell_claude_the_theme(&mut self, light: bool, cx: &mut Context<Self>) {
        let report: &'static [u8] = if light { b"\x1b[?997;2n" } else { b"\x1b[?997;1n" };
        let claude: Vec<_> = self
            .terminals
            .iter()
            .filter(|(id, _)| self.state.session(id).is_some_and(|s| s.agent == glasscode_core::model::AgentKind::Claude))
            .map(|(_, t)| t.clone())
            .collect();
        for terminal in claude {
            terminal.update(cx, |t, cx| t.write(report, cx));
        }
    }

    /// Change the appearance, show it everywhere and save it.
    pub fn set_appearance(&mut self, change: impl FnOnce(&mut Appearance), cx: &mut Context<Self>) {
        change(&mut self.state.appearance);
        self.save();
        self.themes_tick(cx);
        cx.notify();
    }

    /// The Theme card of Settings › Appearance: a small picture of each theme. A click changes the
    /// whole app at once, terminals included.
    pub(crate) fn theme_choices(&self, cx: &mut Context<Self>) -> AnyElement {
        let appearance = self.state.appearance;
        let dark_now = system_dark(cx);
        let cards = THEMES.into_iter().map(|choice| {
            let palette = *theme::palette_for(choice, dark_now);
            let (accent, _) = theme::accent_for(appearance.accent, &palette);
            let on = appearance.theme == choice;
            let note = match choice {
                ThemeChoice::Auto => platform::fixed_text!("Glasscode Light by day, Dark at night: follows {}", platform::os_name()),
                _ => palette.note,
            };
            let mode = match choice {
                ThemeChoice::Auto => "Auto",
                _ if palette.light => "Light",
                _ => "Dark",
            };
            // The picture and its caption sit on the theme's own panel colour, not on glass.
            div()
                .id(SharedString::from(format!("theme-{choice:?}")))
                .flex()
                .flex_col()
                .w(theme::sz(216.))
                .rounded(theme::sz(12.))
                .border_2()
                .border_color(if on { theme::accent() } else { theme::border() })
                .bg(theme::panel())
                .overflow_hidden()
                .cursor_pointer()
                .hover(|s| s.border_color(theme::border_strong()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_appearance(|a| a.theme = choice, cx);
                    this.show_toast(format!("{} is on. Everything changed at once.", theme_name(choice)), cx);
                }))
                .child(preview(&palette, accent))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px(theme::sz(12.))
                        .py(theme::sz(10.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(theme_name(choice)))
                                .child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(note)),
                        )
                        .child(ui::badge(mode, theme::text_soft()).border_color(theme::border()))
                        .when(on, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(theme::sz(22.))
                                    .rounded_full()
                                    .bg(theme::accent())
                                    .child(crate::icons::icon(crate::icons::CHECK, theme::sz(13.), theme::sz(13.), theme::on_accent())),
                            )
                        }),
                )
                .into_any_element()
        });
        div().flex().flex_wrap().gap(theme::sz(12.)).children(cards).into_any_element()
    }

    /// The Accent card's swatches.
    pub(crate) fn accent_choices(&self, cx: &mut Context<Self>) -> AnyElement {
        let appearance = self.state.appearance;
        let current = *theme::palette_for(appearance.theme, system_dark(cx));
        let swatches = [(AccentChoice::Theme, "Ink, as in Glasscode"), (AccentChoice::Indigo, "Indigo"), (AccentChoice::Amber, "Amber, as in Glasscode classic")].map(|(choice, label)| {
            let (color, _) = theme::accent_for(choice, &current);
            let on = appearance.accent == choice;
            div()
                .id(SharedString::from(format!("accent-{choice:?}")))
                .size(theme::sz(32.))
                .p(theme::sz(3.))
                .rounded_full()
                .border_2()
                .border_color(if on { theme::text() } else { theme::border_strong() })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_appearance(|a| a.accent = choice, cx);
                    this.show_toast(format!("Accent: {label}."), cx);
                }))
                .child(div().size_full().rounded_full().bg(theme::c(color)))
        });
        div().flex().gap_2().children(swatches).into_any_element()
    }

    /// The control of the other cards of Settings › Appearance: a segmented choice, or the switch
    /// of the backdrop behind sessions. A click changes the appearance at once.
    pub(crate) fn appearance_control(&self, card: CardId, cx: &mut Context<Self>) -> AnyElement {
        let a = self.state.appearance;
        let options: Vec<Choice> = match card {
            CardId::Backdrop => BACKDROPS.map(|(v, l)| choice(l, a.backdrop == v, move |a| a.backdrop = v, None)).into(),
            CardId::Finish => [("Smooth", Finish::Smooth), ("Dither", Finish::Dither)].map(|(l, v)| choice(l, a.finish == v, move |a| a.finish = v, None)).into(),
            CardId::BackdropInSessions => {
                let on = a.backdrop_in_sessions;
                return ui::switch("look-backdrop-in-sessions", on)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_appearance(move |a| a.backdrop_in_sessions = !on, cx)))
                    .into_any_element();
            }
            CardId::Scale => theme::SCALES.map(|(v, l)| choice(l, a.scale == v, move |a| a.scale = v, None)).into(),
            CardId::TextSize => [("Small", TextSize::Small), ("Medium", TextSize::Medium), ("Large", TextSize::Large)].map(|(l, v)| choice(l, a.text_size == v, move |a| a.text_size = v, None)).into(),
            CardId::Density => [("Comfortable", Density::Comfortable), ("Compact", Density::Compact)].map(|(l, v)| choice(l, a.density == v, move |a| a.density = v, None)).into(),
            CardId::UiFont => [("Geist", UiFont::Geist), ("Inter", UiFont::Inter), ("IBM Plex Sans", UiFont::Plex), (platform::THIS.ui_font.label, UiFont::System)]
                .map(|(l, v)| choice(l, a.ui_font == v, move |a| a.ui_font = v, Some(theme::ui_font_family(v))))
                .into(),
            CardId::CodeFont => [("Geist Mono", CodeFont::Geist), ("JetBrains Mono", CodeFont::JetBrains), ("IBM Plex Mono", CodeFont::Plex), (theme::system_code_font().label, CodeFont::System)]
                .map(|(l, v)| choice(l, a.code_font == v, move |a| a.code_font = v, Some(theme::code_font_family(v))))
                .into(),
            CardId::ChatFont => [("Same as interface", ChatFont::Ui, theme::ui_font_family(a.ui_font)), ("Serif", ChatFont::Serif, platform::THIS.serif_font), (platform::THIS.ui_font.label, ChatFont::System, platform::THIS.ui_font.family)]
                .map(|(l, v, font)| choice(l, a.chat_font == v, move |a| a.chat_font = v, Some(font)))
                .into(),
            CardId::SessionView => [("Chat", SessionView::Chat), ("Terminal", SessionView::Terminal)].map(|(l, v)| choice(l, a.session_view == v, move |a| a.session_view = v, None)).into(),
            CardId::Cursor => [("Block", CursorStyle::Block), ("Bar", CursorStyle::Bar), ("Underline", CursorStyle::Underline)].map(|(l, v)| choice(l, a.cursor == v, move |a| a.cursor = v, None)).into(),
            // Settings draws only the cards above with this control.
            _ => Vec::new(),
        };
        let id = format!("{card:?}");
        ui::segmented()
            .children(options.into_iter().map(|(text, on, set, font)| {
                let set = std::rc::Rc::new(set);
                ui::segment(SharedString::from(format!("look-{id}-{text}")), on, text).when_some(font, |d, f| d.font_family(f)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let set = set.clone();
                    this.set_appearance(move |a| set(a), cx);
                }))
            }))
            .into_any_element()
    }
}

/// The looks of the backdrop, in the order of the choice.
const BACKDROPS: [(BackdropChoice, &str); 4] = [(BackdropChoice::Prism, "Prism"), (BackdropChoice::Aurora, "Aurora"), (BackdropChoice::Dusk, "Dusk"), (BackdropChoice::None, "None")];

/// One segment of an appearance choice: its label, whether it is the current value, what a click
/// sets, and the font its label shows in (the font choices show themselves).
type Choice = (&'static str, bool, Box<dyn Fn(&mut Appearance)>, Option<&'static str>);

fn choice(label: &'static str, on: bool, set: impl Fn(&mut Appearance) + 'static, font: Option<&'static str>) -> Choice {
    (label, on, Box::new(set), font)
}

/// A small picture of a theme: a sidebar, a title bar and a terminal.
fn preview(p: &Palette, accent: u32) -> AnyElement {
    let c = theme::c;
    let bar = |w: f32, color: Hsla| div().h(theme::sz(5.)).w(gpui::relative(w)).rounded(theme::sz(3.)).bg(color);
    div()
        .flex()
        .h(theme::sz(118.))
        .bg(c(p.bg))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(theme::sz(6.))
                .w(gpui::relative(0.26))
                .px_2()
                .py(theme::sz(10.))
                .bg(c(p.panel))
                .border_r_1()
                .border_color(c(p.border))
                .child(bar(0.7, c(p.muted)))
                .child(bar(0.9, c(p.selected)))
                .child(bar(0.6, c(p.border)))
                .child(bar(0.8, c(p.border))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme::sz(6.))
                        .h(theme::sz(22.))
                        .px_2()
                        .bg(c(p.chrome))
                        .border_b_1()
                        .border_color(c(p.border))
                        .child(div().size(theme::sz(6.)).rounded_full().bg(c(p.claude)))
                        .child(div().h(theme::sz(5.)).w(theme::sz(40.)).rounded(theme::sz(3.)).bg(c(p.muted)))
                        .child(div().flex_1())
                        .child(div().h(theme::sz(10.)).w(theme::sz(28.)).rounded(theme::sz(3.)).bg(c(accent))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .gap(theme::sz(6.))
                        .px(theme::sz(10.))
                        .py(theme::sz(9.))
                        .bg(c(p.term.background))
                        .child(bar(0.78, c(p.term.foreground)))
                        .child(bar(0.52, c(p.added)))
                        .child(bar(0.66, c(p.muted)))
                        .child(bar(0.4, c(p.codex)))
                        .child(bar(0.58, c(p.removed))),
                ),
        )
        .into_any_element()
}
