//! Themes: Glasscode Light and Dark, Glasscode classic, high contrast or following macOS, with an
//! accent, fonts, text size, density and the terminal cursor. A change shows at once, in every
//! window (terminals included), and is saved in `state.json`.

use gpui::{AnyElement, App, ClickEvent, Context, FontWeight, Hsla, SharedString, Window, WindowAppearance, div, prelude::*, px};
use glasscode_core::model::{AccentChoice, Appearance, ChatFont, CodeFont, CursorStyle, Density, SessionView, TextSize, ThemeChoice, UiFont};

use crate::platform;
use crate::theme::{self, Palette};
use crate::ui;
use crate::workspace::Workspace;

#[derive(Default)]
pub struct ThemesUi {
    /// The theme whose window appearance (light/dark chrome) is set.
    applied: Option<ThemeChoice>,
    /// Debug builds: `GLASSCODE_DEBUG_THEMES=1` opened the Themes page once.
    debug_opened: bool,
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
            self.show_view(crate::tickets_ui::View::Themes, cx);
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

    pub fn render_themes(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
            div()
                .id(SharedString::from(format!("theme-{choice:?}")))
                .flex()
                .flex_col()
                .w(px(262.))
                .rounded(px(ui::CARD_RADIUS))
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
                        .gap_2p5()
                        .px_3p5()
                        .py_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(theme_name(choice)))
                                .child(div().text_size(px(12.5)).text_color(theme::muted()).child(note)),
                        )
                        .child(ui::badge(mode, theme::text_soft()).border_color(theme::border()))
                        .when(on, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .size(px(22.))
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(theme::accent())
                                    .text_color(theme::on_accent())
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::BOLD)
                                    .child("✓"),
                            )
                        }),
                )
                .into_any_element()
        }).collect::<Vec<AnyElement>>();

        let current = *theme::palette_for(appearance.theme, dark_now);
        let swatches = [(AccentChoice::Theme, "Ink, as in Glasscode"), (AccentChoice::Indigo, "Indigo"), (AccentChoice::Amber, "Amber, as in Glasscode classic")].into_iter().map(|(choice, label)| {
            let (color, _) = theme::accent_for(choice, &current);
            let on = appearance.accent == choice;
            div()
                .id(SharedString::from(format!("accent-{choice:?}")))
                .size(px(32.))
                .p(px(3.))
                .rounded_full()
                .border_2()
                .border_color(if on { theme::text_strong() } else { theme::border() })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_appearance(|a| a.accent = choice, cx);
                    this.show_toast(format!("Accent: {label}."), cx);
                }))
                .child(div().size_full().rounded_full().bg(theme::c(color)))
                .into_any_element()
        }).collect::<Vec<AnyElement>>();

        let look = [
            look_row(
                "Text size",
                "The terminals, and the interface's base size",
                [("Small", TextSize::Small), ("Medium", TextSize::Medium), ("Large", TextSize::Large)].map(|(l, v)| (l, appearance.text_size == v, setter(move |a| a.text_size = v), None)).into(),
                cx,
            ),
            look_row(
                "Density",
                "Rows in the sidebar",
                [("Comfortable", Density::Comfortable), ("Compact", Density::Compact)].map(|(l, v)| (l, appearance.density == v, setter(move |a| a.density = v), None)).into(),
                cx,
            ),
            look_row(
                "Interface font",
                "Menus, the sidebar, settings",
                [("Geist", UiFont::Geist), ("Inter", UiFont::Inter), ("IBM Plex Sans", UiFont::Plex), (platform::THIS.ui_font.label, UiFont::System)]
                    .map(|(l, v)| (l, appearance.ui_font == v, setter(move |a| a.ui_font = v), Some(theme::ui_font_family(v))))
                    .into(),
                cx,
            ),
            look_row(
                "Code font",
                "Terminals, branches and paths",
                [("Geist Mono", CodeFont::Geist), ("JetBrains Mono", CodeFont::JetBrains), ("IBM Plex Mono", CodeFont::Plex), (theme::system_code_font().label, CodeFont::System)]
                    .map(|(l, v)| (l, appearance.code_font == v, setter(move |a| a.code_font = v), Some(theme::code_font_family(v))))
                    .into(),
                cx,
            ),
            look_row(
                "Sessions open as",
                "Chat reads like the Claude app; Terminal is the CLI itself. Each session has the switch too",
                [("Chat", SessionView::Chat), ("Terminal", SessionView::Terminal)].map(|(l, v)| (l, appearance.session_view == v, setter(move |a| a.session_view = v), None)).into(),
                cx,
            ),
            look_row(
                "Conversation font",
                "What Claude and Codex write in the Chat view",
                [("Same as interface", ChatFont::Ui, theme::ui_font_family(appearance.ui_font)), ("Serif", ChatFont::Serif, platform::THIS.serif_font), (platform::THIS.ui_font.label, ChatFont::System, platform::THIS.ui_font.family)]
                    .map(|(l, v, font)| (l, appearance.chat_font == v, setter(move |a| a.chat_font = v), Some(font)))
                    .into(),
                cx,
            ),
            look_row(
                "Cursor",
                "In every terminal, where the program does not choose its own",
                [("Block", CursorStyle::Block), ("Bar", CursorStyle::Bar), ("Underline", CursorStyle::Underline)].map(|(l, v)| (l, appearance.cursor == v, setter(move |a| a.cursor = v), None)).into(),
                cx,
            ),
        ];

        div()
            .id("themes-page")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .bg(theme::bg())
            .child(
                div()
                    .max_w(px(1200.))
                    .mx_auto()
                    .px(px(28.))
                    .pt(px(26.))
                    .pb(px(40.))
                    .flex()
                    .flex_col()
                    .child(div().text_size(px(25.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child("Themes"))
                    .child(div().mt_1p5().text_size(px(14.)).text_color(theme::muted()).child(format!(
                        "Glasscode Light and Dark are the Glasscode design system itself; Auto follows {}. Picking one changes the whole app at once, terminals included.",
                        platform::os_name()
                    )))
                    .child(div().mt_5().flex().flex_wrap().gap(px(14.)).children(cards))
                    .child(
                        ui::card()
                            .mt(px(22.))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_4()
                                    .min_h(px(60.))
                                    .px_4()
                                    .py_3()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .flex_1()
                                            .min_w(px(240.))
                                            .child(div().font_weight(FontWeight::MEDIUM).text_color(theme::text_strong()).child("Accent"))
                                            .child(div().text_size(px(13.)).text_color(theme::muted()).child("Primary buttons, switches and the cursor. Glasscode uses ink; colour is kept for status.")),
                                    )
                                    .child(div().flex().gap_2().children(swatches)),
                            )
                            .children(look),
                    ),
            )
            .into_any_element()
    }
}

type Setter = Box<dyn Fn(&mut Appearance)>;

fn setter(f: impl Fn(&mut Appearance) + 'static) -> Setter {
    Box::new(f)
}

/// A row of the look settings: what it is, and a segmented choice.
fn look_row(label: &'static str, detail: &'static str, options: Vec<(&'static str, bool, Setter, Option<&'static str>)>, cx: &mut Context<Workspace>) -> AnyElement {
    let segments: Vec<AnyElement> = options
        .into_iter()
        .map(|(text, on, set, font)| {
            let set = std::rc::Rc::new(set);
            ui::segment(SharedString::from(format!("look-{label}-{text}")), on, text)
                .when_some(font, |d, f| d.font_family(f))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let set = set.clone();
                    this.set_appearance(move |a| set(a), cx);
                }))
                .into_any_element()
        })
        .collect();
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_4()
        .min_h(px(60.))
        .px_4()
        .py_3()
        .border_t_1()
        .border_color(theme::border())
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(240.))
                .child(div().font_weight(FontWeight::MEDIUM).text_color(theme::text_strong()).child(label))
                .child(div().text_size(px(13.)).text_color(theme::muted()).child(detail)),
        )
        .child(ui::segmented().children(segments))
        .into_any_element()
}

/// A small picture of a theme: a sidebar, a title bar and a terminal.
fn preview(p: &Palette, accent: u32) -> AnyElement {
    let c = theme::c;
    let bar = |w: f32, color: Hsla| div().h(px(5.)).w(gpui::relative(w)).rounded(px(3.)).bg(color);
    div()
        .flex()
        .h(px(150.))
        .bg(c(p.bg))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .w(gpui::relative(0.26))
                .px_2()
                .py(px(10.))
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
                        .gap(px(6.))
                        .h(px(22.))
                        .px_2()
                        .bg(c(p.chrome))
                        .border_b_1()
                        .border_color(c(p.border))
                        .child(div().size(px(6.)).rounded_full().bg(c(p.claude)))
                        .child(div().h(px(5.)).w(px(40.)).rounded(px(3.)).bg(c(p.muted)))
                        .child(div().flex_1())
                        .child(div().h(px(10.)).w(px(28.)).rounded(px(3.)).bg(c(accent))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .gap(px(6.))
                        .px(px(10.))
                        .py(px(9.))
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
