//! The frame of the window: the title bar, the left sidebar, the centre, the right panel, the
//! status bar and the popovers over them. Where the person is (the view, and the session in
//! Sessions) goes on a back list each time it changes, for the title bar's back and forward.

use gpui::{AnyElement, ClickEvent, Context, FontWeight, Stateful, Window, deferred, div, prelude::*, px};

use crate::theme;
use crate::ui;
use crate::workspace::{NewSession, OpenPalette, OpenSettings, RightTab, Side, Workspace};

/// What the centre shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    /// The day's board.
    Today,
    #[default]
    Sessions,
    /// The tickets and the review requests.
    Board,
    /// Its page comes with U14; until then it shows the Board.
    #[allow(dead_code)]
    PullRequests,
    Usage,
    /// Settings; its Appearance section is the Themes page.
    Settings,
    /// Its flow comes with U18; until then it shows the Sessions view, without sidebars.
    #[allow(dead_code)]
    FirstRun,
}

/// A tab of the centre. The session view (U12) and the file tabs (U13) draw them.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CenterTab {
    Session(String),
    File(std::path::PathBuf),
}

/// A popover over the status bar. The status bar (U17) opens and draws them.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Popover {
    Usage,
    Resources,
    Ports,
}

/// A place back and forward go to: a view, and in Sessions the session on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub view: View,
    pub session: Option<String>,
}

/// The back list and the forward list, the nearest place last.
pub type History = (Vec<Place>, Vec<Place>);

/// The most places the back list keeps.
const HISTORY_LEN: usize = 50;

/// Leaving `from` for `to`: `from` goes on the back list and the forward list empties. Staying
/// in the same place records nothing.
fn record(history: &mut History, from: Place, to: &Place) {
    if from == *to {
        return;
    }
    history.0.push(from);
    if history.0.len() > HISTORY_LEN {
        history.0.remove(0);
    }
    history.1.clear();
}

/// A place as it can be shown now: a session that was closed since leaves its view in Sessions,
/// at the session that is active now.
fn resolve(mut place: Place, active: Option<&str>, exists: &dyn Fn(&str) -> bool) -> Place {
    if place.session.as_deref().is_some_and(|id| !exists(id)) {
        place.session = active.map(str::to_string);
    }
    place
}

/// One step through `from` (the back list, or the forward list) to the nearest place that is not
/// `current`; `current` goes on the other list. None when there is no such place.
fn step(from: &mut Vec<Place>, to: &mut Vec<Place>, current: Place, active: Option<&str>, exists: &dyn Fn(&str) -> bool) -> Option<Place> {
    while let Some(place) = from.pop() {
        let place = resolve(place, active, exists);
        if place != current {
            to.push(current);
            return Some(place);
        }
    }
    None
}

fn back(history: &mut History, current: Place, active: Option<&str>, exists: &dyn Fn(&str) -> bool) -> Option<Place> {
    let (back, forward) = history;
    step(back, forward, current, active, exists)
}

fn forward(history: &mut History, current: Place, active: Option<&str>, exists: &dyn Fn(&str) -> bool) -> Option<Place> {
    let (back, forward) = history;
    step(forward, back, current, active, exists)
}

/// A drag that leaves a sidebar narrower than this hides it.
pub const HIDE_BELOW: f32 = 140.;

/// The width the centre keeps, where it can, while the sidebars narrow for it.
const CENTRE_MIN: f32 = 320.;

/// The widths two sidebars show at in a window `width` wide, each given as (its width, its
/// narrowest): the right one narrows first, then the left one, so that the centre keeps
/// `CENTRE_MIN` where it can. None for a sidebar that does not show.
pub fn fit_panels(width: f32, left: Option<(f32, f32)>, right: Option<(f32, f32)>) -> (Option<f32>, Option<f32>) {
    let mut over = left.map_or(0., |l| l.0) + right.map_or(0., |r| r.0) + CENTRE_MIN - width;
    let mut narrow = |panel: Option<(f32, f32)>| {
        panel.map(|(wanted, min)| {
            let cut = over.clamp(0., (wanted - min).max(0.));
            over -= cut;
            wanted - cut
        })
    };
    let right = narrow(right);
    let left = narrow(left);
    (left, right)
}

/// The right panel's tab as `Layout.right_tab` keeps it.
fn right_tab_key(tab: RightTab) -> &'static str {
    match tab {
        RightTab::PullRequest => "pull-request",
        RightTab::Changes => "changes",
        RightTab::Files => "files",
        RightTab::Glasscode => "glasscode",
    }
}

/// The saved right tab; the Glasscode tab only where it exists.
pub fn right_tab_from(key: &str) -> Option<RightTab> {
    match key {
        "pull-request" => Some(RightTab::PullRequest),
        "changes" => Some(RightTab::Changes),
        "files" => Some(RightTab::Files),
        "glasscode" if crate::glasscode_ui::VOICE => Some(RightTab::Glasscode),
        _ => None,
    }
}

// ----- the title bar at narrow widths -----------------------------------------------------------

/// The title bar's optional items: which show at a width. The wordmark goes first, then the
/// breadcrumb, then the words of the Jump field (its icon and its key stay).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TitleFit {
    pub wordmark: bool,
    pub breadcrumb: bool,
    pub jump_words: bool,
}

const WORDMARK: f32 = 82.;
/// The breadcrumb narrows down to this, with an ellipsis, before it goes.
const BREADCRUMB: f32 = 150.;
/// The Jump field with its words, and with only its icon and key.
const JUMP_WORDS: f32 = 240.;
const JUMP_SMALL: f32 = 120.;
const BAR_BUTTON: f32 = 28.;
const BAR_GAP: f32 = 8.;
const MARK: f32 = 19.;
/// window_controls.rs: three buttons of 46 px.
const WINDOW_BUTTONS: f32 = 138.;

/// The width of a pill in the title bar with `chars` letters of 12 px text.
pub fn pill_width(chars: usize) -> f32 {
    chars as f32 * 7. + 36.
}

/// What the title bar takes whatever the width: its padding (the traffic lights on macOS), the
/// mark, the four buttons, the pills that show, the window buttons of Windows, and the gaps.
pub fn title_chrome(windows: bool, pills: &[f32]) -> f32 {
    let padding = if windows { 12. + WINDOW_BUTTONS } else { 84. + 12. };
    // Gaps: after the mark, the buttons, the breadcrumb, the Jump field and each pill.
    let gaps = (4 + pills.len()) as f32 * BAR_GAP;
    padding + MARK + 4. * BAR_BUTTON + pills.iter().sum::<f32>() + gaps
}

/// Which optional items fit in a title bar `width` wide, where `chrome` is taken already.
pub fn title_fit(width: f32, chrome: f32) -> TitleFit {
    let mut free = width - chrome - JUMP_SMALL;
    let jump_words = free >= JUMP_WORDS - JUMP_SMALL;
    if jump_words {
        free -= JUMP_WORDS - JUMP_SMALL;
    }
    let breadcrumb = jump_words && free >= BREADCRUMB;
    if breadcrumb {
        free -= BREADCRUMB;
    }
    let wordmark = breadcrumb && free >= WORDMARK;
    TitleFit { wordmark, breadcrumb, jump_words }
}

/// A square button of the title bar with an icon; a button with nothing to do shows faint.
fn bar_button(id: &'static str, glyph: &'static [u8], enabled: bool, pressed: bool) -> Stateful<gpui::Div> {
    let color = if enabled { theme::text_soft() } else { theme::faint() };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(BAR_BUTTON))
        .rounded(px(6.))
        .when(pressed, |d| d.bg(theme::selected()))
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(theme::hover())))
        .child(crate::icons::icon(glyph, px(16.), px(16.), color))
}

/// A row at the top of the left sidebar: a view, or New session.
fn nav_row(id: &'static str, glyph: &'static [u8], label: &'static str, on: bool, count: usize, key: Option<String>) -> Stateful<gpui::Div> {
    let color = if on { theme::text_strong() } else { theme::text_soft() };
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2p5()
        .h(px(30.))
        .px_2()
        .rounded_md()
        .cursor_pointer()
        .text_color(color)
        .when(on, |d| d.bg(theme::selected()).font_weight(FontWeight::MEDIUM))
        .hover(|s| s.bg(theme::hover()))
        .child(crate::icons::icon(glyph, px(16.), px(16.), color))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
        .when(count > 0, |d| {
            d.child(
                div()
                    .flex_none()
                    .min_w(px(18.))
                    .h(px(18.))
                    .px(px(5.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(crate::sidebar::attention())
                    .text_color(theme::on_attention())
                    .text_size(px(10.5))
                    .font_weight(FontWeight::BOLD)
                    .child(count.to_string()),
            )
        })
        .when_some(key, |d, key| d.child(ui::kbd(key)))
}

impl Workspace {
    // ----- where the person is ----------------------------------------------------------------

    /// The view, and in Sessions the session on screen.
    pub fn place(&self) -> Place {
        Place { view: self.view, session: self.state.active_session.clone().filter(|_| self.view == View::Sessions) }
    }

    /// Show `view`, with the place left on the back list. Each way to a view comes here; it needs
    /// no window, as the ticks have none.
    pub(crate) fn enter(&mut self, view: View) {
        let (from, to) = (self.place(), Place { view, session: self.state.active_session.clone().filter(|_| view == View::Sessions) });
        record(&mut self.history, from, &to);
        self.view = view;
    }

    /// Show the session `id` in Sessions, with the place left on the back list (`select_session`).
    pub(crate) fn enter_session(&mut self, id: &str) {
        let (from, to) = (self.place(), Place { view: View::Sessions, session: Some(id.to_string()) });
        record(&mut self.history, from, &to);
        self.view = View::Sessions;
    }

    /// Go to a view the way its row does: Board refreshes, Settings opens at its last section.
    pub fn go(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        match view {
            View::Today => self.show_today(cx),
            View::Board | View::PullRequests => self.show_tickets(cx),
            View::Sessions | View::FirstRun => {
                self.center = crate::center::Center::Session;
                self.show_sessions(window, cx);
            }
            View::Usage => self.show_view(View::Usage, cx),
            View::Settings => {
                let section = self.settings.section;
                self.open_settings(section, window, cx);
            }
        }
        self.save();
    }

    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (current, active) = (self.place(), self.state.active_session.clone());
        let exists = |id: &str| self.state.session(id).is_some();
        if let Some(place) = back(&mut self.history, current, active.as_deref(), &exists) {
            self.open_place(place, window, cx);
        }
    }

    pub fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (current, active) = (self.place(), self.state.active_session.clone());
        let exists = |id: &str| self.state.session(id).is_some();
        if let Some(place) = forward(&mut self.history, current, active.as_deref(), &exists) {
            self.open_place(place, window, cx);
        }
    }

    /// Show a place from the back or forward list. That records nothing: the lists stay as the
    /// step left them.
    fn open_place(&mut self, place: Place, window: &mut Window, cx: &mut Context<Self>) {
        let history = std::mem::take(&mut self.history);
        match place.session.filter(|_| place.view == View::Sessions) {
            Some(id) => {
                self.center = crate::center::Center::Session;
                self.select_session(&id, window, cx);
            }
            None => self.go(place.view, window, cx),
        }
        self.history = history;
        cx.notify();
    }

    // ----- the panels ---------------------------------------------------------------------------

    /// The left sidebar shows in every view but Settings (it has its own list) and the first run.
    pub(crate) fn view_has_left(&self) -> bool {
        !matches!(self.view, View::Settings | View::FirstRun)
    }

    /// The view has a right panel to show: the session's, or the ticket being read on the Board.
    pub(crate) fn view_has_right(&self) -> bool {
        match self.view {
            View::Sessions => true,
            View::Board | View::PullRequests => self.tickets.selected.is_some() && !self.tickets.expanded,
            _ => false,
        }
    }

    /// The width a sidebar shows at: as the person left it, or narrower when the window is narrow
    /// (`fit_panels`). A sidebar that does not show has the width it was left at.
    pub(crate) fn shown_width(&self, side: Side) -> f32 {
        let right_side = self.right_side();
        let limit = |side: Side| (self.saved_width(side), crate::sessions_ui::edge_limits(side).0);
        let left = (self.view_has_left() && !self.state.layout.left_hidden).then(|| limit(Side::Left));
        let right = (self.view_has_right() && self.show_right).then(|| limit(right_side));
        let (left, right) = fit_panels(self.window_width, left, right);
        let shown = match side {
            Side::Left => left,
            _ if side == right_side => right,
            _ => None,
        };
        shown.unwrap_or_else(|| self.saved_width(side))
    }

    /// The right panel's edge: the Board's reader keeps its own width.
    fn right_side(&self) -> Side {
        if matches!(self.view, View::Board | View::PullRequests) { Side::Tickets } else { Side::Right }
    }

    /// Hide or show a sidebar (the right panel and the Board's reader are one), and keep it so.
    pub(crate) fn set_hidden(&mut self, side: Side, hidden: bool) {
        match side {
            Side::Left => self.state.layout.left_hidden = hidden,
            Side::Right | Side::Tickets => {
                self.state.layout.right_hidden = hidden;
                self.show_right = !hidden;
            }
        }
    }

    pub fn toggle_left(&mut self, cx: &mut Context<Self>) {
        if !self.view_has_left() {
            return;
        }
        self.set_hidden(Side::Left, !self.state.layout.left_hidden);
        self.save();
        cx.notify();
    }

    pub fn toggle_right(&mut self, cx: &mut Context<Self>) {
        if !self.view_has_right() {
            return;
        }
        self.set_hidden(Side::Right, self.show_right);
        self.save();
        cx.notify();
    }

    #[allow(dead_code)] // the status bar (U17) opens its popovers with it
    pub fn toggle_popover(&mut self, popover: Popover, cx: &mut Context<Self>) {
        self.popover = if self.popover == Some(popover) { None } else { Some(popover) };
        cx.notify();
    }

    /// Keep the right panel's tab over a restart. Many places change it, so the tick looks.
    pub(crate) fn remember_right_tab(&mut self) {
        let key = right_tab_key(self.right_tab);
        if self.state.layout.right_tab.as_deref() != Some(key) {
            self.state.layout.right_tab = Some(key.to_string());
            self.save();
        }
    }

    // ----- drawing ------------------------------------------------------------------------------

    /// The window: title bar; left sidebar, centre and right panel with their edges; status bar;
    /// then the popover and the overlays, in this order.
    pub fn render_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.window_width = f32::from(window.viewport_size().width);
        let modal = self.modal.as_ref().map(|m| m.render(self, window, cx));
        let title_bar = self.render_title_bar(window, cx).into_any_element();
        let has_left = self.view_has_left();
        let left = (has_left && !self.state.layout.left_hidden).then(|| self.render_left(window, cx));
        let main = self.render_main(window, cx);
        let has_right = self.view_has_right();
        let right = if has_right && self.show_right { self.render_right(window, cx) } else { None };
        let right_side = self.right_side();
        let status_bar = self.render_status_bar(cx).into_any_element();
        let popover = self.popover.map(|p| self.render_popover(p, window, cx));
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(title_bar)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .children(left)
                    // A hidden sidebar keeps its edge, so a drag brings it back.
                    .when(has_left, |d| d.child(self.resize_handle(Side::Left, cx)))
                    .child(main)
                    .when(has_right, |d| d.child(self.resize_handle(right_side, cx)))
                    .children(right),
            )
            .child(status_bar)
            .children(popover)
            .when_some(self.render_kit(cx), |d, kit| d.child(kit))
            .when_some(self.render_row_menu(cx), |d, menu| d.child(menu))
            .when_some(modal, |d, modal| d.child(modal))
            .when_some(self.render_palette(cx), |d, palette| d.child(palette))
            .when_some(self.toast.as_ref().map(|t| t.0.clone()), |d, message| {
                d.child(
                    deferred(
                        div()
                            .absolute()
                            .bottom(px(40.))
                            .right(px(16.))
                            .max_w(px(460.))
                            .px_4()
                            .py_3()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme::border_strong())
                            .bg(theme::selected())
                            .text_color(theme::text())
                            .shadow_lg()
                            .child(message),
                    )
                    .with_priority(3),
                )
            })
            .into_any_element()
    }

    /// The centre of the view.
    fn render_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.view {
            View::Today => self.render_today(window, cx),
            View::Board | View::PullRequests => self.render_tickets(window, cx),
            View::Usage => self.render_usage(window, cx),
            View::Settings => self.render_settings(window, cx),
            View::Sessions | View::FirstRun => match self.center.clone() {
                crate::center::Center::Session => self.render_center(window, cx).into_any_element(),
                crate::center::Center::Server(id) => self.render_server_center(&id, cx),
            },
        }
    }

    /// The left sidebar: the rows of the views (the old rail), then the projects and sessions.
    pub fn render_left(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let view = self.view;
        // Today: what is due or late, and what waits for the person's OK; Board: review requests.
        let today_count = self.today_view().due + self.control.approvals.len();
        let board_count = self.tickets.reviews.len();
        let rows = div()
            .flex()
            .flex_none()
            .flex_col()
            .gap_px()
            .px_1p5()
            .pt_2()
            .pb_1p5()
            .border_b_1()
            .border_color(theme::border())
            .child(
                nav_row("nav-new-session", crate::icons::PLUS, "New session", false, 0, Some(crate::keymap::label(&NewSession)))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_session(None, None, window, cx))),
            )
            .child(nav_row("nav-today", crate::icons::TODAY, "Today", view == View::Today, today_count, None).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go(View::Today, window, cx))))
            .child(
                nav_row("nav-board", crate::icons::TICKETS, "Board", matches!(view, View::Board | View::PullRequests), board_count, None)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go(View::Board, window, cx))),
            )
            .child(nav_row("nav-usage", crate::icons::USAGE, "Usage", view == View::Usage, 0, None).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go(View::Usage, window, cx))))
            .child(
                nav_row("nav-settings", crate::icons::SETTINGS, "Settings", view == View::Settings, 0, Some(crate::keymap::label(&OpenSettings)))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go(View::Settings, window, cx))),
            );
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.sidebar_width()))
            .h_full()
            .bg(theme::panel())
            .border_r_1()
            .border_color(theme::border())
            .child(rows)
            .child(self.render_sidebar(cx))
            .into_any_element()
    }

    /// The right panel of the view: the session's (Pull request, Changes, Files), or on the Board
    /// the ticket being read.
    pub fn render_right(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.view {
            View::Sessions => Some(self.render_right_panel(cx)),
            View::Board | View::PullRequests => self.render_reader_panel(window, cx),
            _ => None,
        }
    }

    #[allow(dead_code)] // the status bar (U17) draws its popovers here
    pub fn render_popover(&mut self, _popover: Popover, _window: &mut Window, _cx: &mut Context<Self>) -> AnyElement {
        div().into_any_element()
    }

    pub(crate) fn render_title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = self.settings.section == crate::settings_ui::Section::Appearance;
        let (project, worktree) = match (&self.center, self.active_session()) {
            _ if matches!(self.view, View::Usage | View::Settings) => (
                match self.view {
                    View::Usage => "Usage",
                    _ if appearance => "Themes",
                    _ => "Settings",
                }
                .to_string(),
                String::new(),
            ),
            _ if self.view == View::Today => ("Today".to_string(), String::new()),
            _ if matches!(self.view, View::Board | View::PullRequests) => (
                "Board".to_string(),
                self.tickets.snapshot.as_ref().map(|s| format!("Linear · {}", s.org)).unwrap_or_else(|| "Linear".into()),
            ),
            (crate::center::Center::Server(id), _) => (
                self.server.name(),
                self.server.session(id).map(|s| s.session.branch.clone()).unwrap_or_default(),
            ),
            (_, session) => match session {
            Some(session) => (
                self.state
                    .project(&session.project_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default(),
                session.branch.clone(),
            ),
            None => ("No project".into(), String::new()),
            },
        };
        let switch = self.render_switch_buttons(cx);
        let update = self.update_pill(cx);
        let fit = {
            let mut pills = vec![pill_width(if self.server.config.is_none() { 7 } else { self.server.name().chars().count() + if self.server.connected() { 0 } else { 13 } })];
            if update.is_some() {
                pills.push(pill_width("Restart to update".chars().count()));
            }
            if switch.is_some() {
                pills.push(pill_width("Moving 10 of 10…".chars().count()));
            }
            title_fit(f32::from(window.viewport_size().width), title_chrome(cfg!(windows), &pills))
        };
        let can_back = !self.history.0.is_empty();
        let can_forward = !self.history.1.is_empty();
        let has_left = self.view_has_left();
        let has_right = self.view_has_right();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(44.))
            .pl(px(84.))
            .pr_3()
            // Windows: no traffic lights at the left, and the window buttons end the bar at the right.
            .when(cfg!(windows), |d| {
                d.pl(px(12.))
                    .pr_0()
                    .window_control_area(gpui::WindowControlArea::Drag)
            })
            .bg(theme::chrome())
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(crate::icons::icon(crate::icons::LOGO, px(19.), px(18.), theme::text_strong()))
                    .when(fit.wordmark, |d| d.child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).child("Glasscode"))),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_0p5()
                    .child(crate::window_controls::clickable(
                        bar_button("title-left-panel", crate::icons::PANEL_LEFT, has_left, has_left && !self.state.layout.left_hidden)
                            .when(has_left, |b| b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_left(cx)))),
                    ))
                    .child(crate::window_controls::clickable(
                        bar_button("title-back", crate::icons::BACK, can_back, false)
                            .when(can_back, |b| b.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go_back(window, cx)))),
                    ))
                    .child(crate::window_controls::clickable(
                        bar_button("title-forward", crate::icons::FORWARD, can_forward, false)
                            .when(can_forward, |b| b.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go_forward(window, cx)))),
                    )),
            )
            .when(fit.breadcrumb, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_shrink(1.)
                        .min_w_0()
                        .max_w(px(380.))
                        .items_center()
                        .gap_1p5()
                        .text_color(theme::muted())
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(project)
                        .when(!worktree.is_empty(), |d| {
                            d.child(div().text_color(theme::faint()).child("/"))
                                .child(div().min_w_0().overflow_hidden().text_ellipsis().font_family(theme::MONO_FONT).text_size(px(12.)).text_color(theme::text_soft()).child(worktree))
                        }),
                )
            })
            // Jump: the command palette.
            .child(
                div()
                    .id("title-search")
                    // Windows: a press opens the palette, it does not move the window.
                    .when(cfg!(windows), |d| d.occlude())
                    .flex()
                    .when(fit.jump_words, |d| d.flex_1().min_w(px(180.)))
                    .when(!fit.jump_words, |d| d.flex_none())
                    .max_w(px(460.))
                    .mx_auto()
                    .items_center()
                    .gap_2()
                    .h(px(30.))
                    .px_2p5()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme::border_strong())
                    .bg(theme::raised())
                    .text_color(theme::muted())
                    .cursor_pointer()
                    .hover(|s| s.border_color(theme::faint()))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_palette(window, cx)))
                    .child(crate::icons::icon(crate::icons::SEARCH, px(14.), px(14.), theme::faint()))
                    .when(fit.jump_words, |d| d.child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child("Search sessions, tickets and commands")))
                    .child(ui::kbd(crate::keymap::label(&OpenPalette))),
            )
            .when_some(switch, |d, buttons| {
                d.child(crate::window_controls::clickable(buttons))
            })
            .when_some(update, |d, pill| {
                d.child(crate::window_controls::clickable(pill))
            })
            // A long server name gives way before anything overlaps.
            .child(div().flex().flex_shrink(1.).min_w_0().overflow_hidden().child(crate::window_controls::clickable(self.server_pill(cx))))
            .child(crate::window_controls::clickable(
                bar_button("title-right-panel", crate::icons::PANEL_RIGHT, has_right, has_right && self.show_right)
                    .when(has_right, |b| b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_right(cx)))),
            ))
            .when(cfg!(windows), |d| {
                d.child(crate::window_controls::WindowControls)
            })
    }

    /// The status bar: its items on the left, then those on the right.
    pub(crate) fn render_status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let left = self.status_left(cx);
        let right = self.status_right(cx);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_4()
            .h(px(26.))
            .px_3()
            .bg(theme::chrome())
            .border_t_1()
            .border_color(theme::border())
            .text_size(px(11.5))
            .text_color(theme::muted())
            .children(left)
            .children(right)
    }

    /// The status bar as it was: branch, changes and GitHub account; then, at the right end, the
    /// CLI versions, the saved sessions and the agent with its account. U17 puts usage first.
    pub fn status_left(&mut self, _cx: &mut Context<Self>) -> Vec<AnyElement> {
        let session = self.active_session();
        let account = session.and_then(|s| {
            s.account_id
                .as_deref()
                .and_then(|id| self.state.account(id))
                .map(|a| format!("{} · {}", s.agent.name(), a.display()))
        });
        let cli_warning = self
            .cli_status
            .iter()
            .filter(|s| !s.is_ok())
            .map(|s| s.summary())
            .collect::<Vec<_>>()
            .join(" · ");
        let cli_ok = self
            .cli_status
            .iter()
            .filter(|s| s.is_ok())
            .map(|s| s.summary())
            .collect::<Vec<_>>()
            .join(" · ");
        let mut items: Vec<AnyElement> = vec![
            div()
                .font_family(theme::MONO_FONT)
                .child(format!("⎇ {}", session.map(|s| s.branch.clone()).unwrap_or_default()))
                .into_any_element(),
            div()
                .child(if self.changes.is_empty() {
                    "Clean".to_string()
                } else {
                    format!("{} changed", self.changes.len())
                })
                .into_any_element(),
        ];
        if let Some(label) = session.and_then(|s| crate::github_accounts_ui::status_label(self, &s.project_id)) {
            items.push(div().child(label).into_any_element());
        }
        items.push(div().flex_1().into_any_element());
        if !cli_warning.is_empty() {
            items.push(div().text_color(theme::attention()).child(format!("⚠ {cli_warning}")).into_any_element());
        } else if !cli_ok.is_empty() {
            items.push(div().child(cli_ok).into_any_element());
        }
        items.push(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(div().size(px(6.)).rounded_full().bg(theme::added()))
                .child({
                    let n = self.state.sessions.len();
                    let synced = self.mirror.synced.is_some() && self.mirror.error.is_none();
                    format!("{n} {} saved · {}", if n == 1 { "session" } else { "sessions" }, if synced { "in sync on the server" } else { "restored on relaunch" })
                })
                .into_any_element(),
        );
        if let Some(account) = account {
            items.push(div().child(account).into_any_element());
        }
        items
    }

    /// Memory, terminals and ports come here with U17.
    pub fn status_right(&mut self, _cx: &mut Context<Self>) -> Vec<AnyElement> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(view: View, session: Option<&str>) -> Place {
        Place { view, session: session.map(str::to_string) }
    }

    fn all(_: &str) -> bool {
        true
    }

    #[test]
    fn back_and_forward_walk_the_places() {
        let mut history = History::default();
        record(&mut history, at(View::Sessions, Some("a")), &at(View::Today, None));
        record(&mut history, at(View::Today, None), &at(View::Board, None));
        record(&mut history, at(View::Board, None), &at(View::Settings, None));

        assert_eq!(back(&mut history, at(View::Settings, None), Some("a"), &all), Some(at(View::Board, None)));
        assert_eq!(back(&mut history, at(View::Board, None), Some("a"), &all), Some(at(View::Today, None)));
        assert_eq!(back(&mut history, at(View::Today, None), Some("a"), &all), Some(at(View::Sessions, Some("a"))));
        assert_eq!(back(&mut history, at(View::Sessions, Some("a")), Some("a"), &all), None, "nothing before the first place");

        assert_eq!(forward(&mut history, at(View::Sessions, Some("a")), Some("a"), &all), Some(at(View::Today, None)));
        assert_eq!(forward(&mut history, at(View::Today, None), Some("a"), &all), Some(at(View::Board, None)));
        assert_eq!(history.0.last(), Some(&at(View::Today, None)), "forward puts the place left back on the back list");
    }

    #[test]
    fn a_new_place_clears_the_forward_list() {
        let mut history = History::default();
        record(&mut history, at(View::Sessions, Some("a")), &at(View::Today, None));
        assert_eq!(back(&mut history, at(View::Today, None), Some("a"), &all), Some(at(View::Sessions, Some("a"))));
        assert_eq!(history.1, vec![at(View::Today, None)]);

        record(&mut history, at(View::Sessions, Some("a")), &at(View::Usage, None));
        assert!(history.1.is_empty(), "going somewhere new ends the way forward");
        assert_eq!(history.0, vec![at(View::Sessions, Some("a"))]);
        assert_eq!(forward(&mut history, at(View::Usage, None), Some("a"), &all), None);
    }

    #[test]
    fn staying_records_nothing_and_sessions_are_places() {
        let mut history = History::default();
        record(&mut history, at(View::Today, None), &at(View::Today, None));
        assert!(history.0.is_empty());
        record(&mut history, at(View::Sessions, Some("a")), &at(View::Sessions, Some("b")));
        assert_eq!(history.0, vec![at(View::Sessions, Some("a"))], "another session is another place");
    }

    #[test]
    fn a_closed_session_is_its_view_with_the_active_session() {
        let mut history = History::default();
        record(&mut history, at(View::Sessions, Some("gone")), &at(View::Today, None));
        record(&mut history, at(View::Today, None), &at(View::Sessions, Some("b")));
        let exists = |id: &str| id != "gone";
        // Back from b: Today; back again: the closed session's place is Sessions at b, so it shows.
        assert_eq!(back(&mut history, at(View::Sessions, Some("b")), Some("b"), &exists), Some(at(View::Today, None)));
        assert_eq!(back(&mut history, at(View::Today, None), Some("b"), &exists), Some(at(View::Sessions, Some("b"))));

        // A place that resolves to where the person is already is passed over.
        let mut history = History::default();
        record(&mut history, at(View::Today, None), &at(View::Sessions, Some("gone")));
        record(&mut history, at(View::Sessions, Some("gone")), &at(View::Sessions, Some("b")));
        assert_eq!(back(&mut history, at(View::Sessions, Some("b")), Some("b"), &exists), Some(at(View::Today, None)));
    }

    #[test]
    fn the_back_list_is_bounded() {
        let mut history = History::default();
        for i in 0..(HISTORY_LEN + 10) {
            record(&mut history, at(View::Sessions, Some(&i.to_string())), &at(View::Sessions, Some(&(i + 1).to_string())));
        }
        assert_eq!(history.0.len(), HISTORY_LEN);
        assert_eq!(history.0.last(), Some(&at(View::Sessions, Some(&(HISTORY_LEN + 9).to_string()))));
    }

    /// At 900 px the centre keeps 320 px where it can: the right panel narrows first, to its
    /// narrowest, then the left sidebar. A wide window changes nothing.
    #[test]
    fn the_sidebars_narrow_for_the_centre() {
        let left = Some((300., 220.));
        assert_eq!(fit_panels(1440., left, Some((420., 300.))), (Some(300.), Some(420.)));
        assert_eq!(fit_panels(900., left, Some((420., 300.))), (Some(280.), Some(300.)), "Sessions at 900 px");
        assert_eq!(fit_panels(900., left, Some((540., 400.))), (Some(220.), Some(400.)), "the Board's reader at 900 px");
        assert_eq!(fit_panels(900., left, None), (Some(300.), None));
        assert_eq!(fit_panels(900., None, Some((760., 300.))), (None, Some(580.)));
        assert_eq!(fit_panels(600., left, Some((420., 300.))), (Some(220.), Some(300.)), "never below their narrowest");
    }

    #[test]
    fn the_right_tab_goes_round() {
        for tab in [RightTab::PullRequest, RightTab::Changes, RightTab::Files] {
            assert!(right_tab_from(right_tab_key(tab)) == Some(tab));
        }
        assert!(right_tab_from("checks").is_none(), "an unknown tab is the default");
    }

    /// At the smallest window (900 px) nothing overlaps: the optional items go first. A quiet bar
    /// has only the "Server…" pill; then the update pill comes; a busy bar also has a move in
    /// progress and a server with a long name that is connecting.
    #[test]
    fn the_title_bar_fits_at_each_width() {
        let server = pill_width("Server…".chars().count());
        let update = pill_width("Restart to update".len());
        let quiet = [server];
        let updating = [server, update];
        let busy = [pill_width("my-build-server · connecting".chars().count()), update, pill_width("Moving 10 of 10…".chars().count())];
        let all = TitleFit { wordmark: true, breadcrumb: true, jump_words: true };
        let none = TitleFit { wordmark: false, breadcrumb: false, jump_words: false };
        for windows in [true, false] {
            for width in [900., 1100., 1600.] {
                assert_eq!(title_fit(width, title_chrome(windows, &quiet)), all, "a quiet bar at {width} px (Windows: {windows})");
            }
        }
        let fit = |width: f32, pills: &[f32]| title_fit(width, title_chrome(true, pills));
        assert_eq!(fit(900., &updating), TitleFit { wordmark: false, breadcrumb: false, jump_words: true });
        assert_eq!((fit(1100., &updating), fit(1600., &updating)), (all, all));
        assert_eq!((fit(900., &busy), fit(1100., &busy), fit(1600., &busy)), (none, none, all));
        // The rest fits even then: all but the server pill (which clips) and the small Jump field.
        assert!(title_chrome(true, &busy[1..]) + JUMP_SMALL <= 900.);
    }

    #[test]
    fn the_title_bar_drops_the_wordmark_first() {
        let chrome = title_chrome(true, &[pill_width(7)]);
        let roomy = chrome + JUMP_WORDS + BREADCRUMB + WORDMARK;
        assert_eq!(title_fit(roomy, chrome), TitleFit { wordmark: true, breadcrumb: true, jump_words: true });
        assert_eq!(title_fit(roomy - 1., chrome), TitleFit { wordmark: false, breadcrumb: true, jump_words: true });
        assert_eq!(title_fit(chrome + JUMP_WORDS + BREADCRUMB - 1., chrome), TitleFit { wordmark: false, breadcrumb: false, jump_words: true });
        assert_eq!(title_fit(chrome + JUMP_WORDS - 1., chrome), TitleFit { wordmark: false, breadcrumb: false, jump_words: false });
    }
}
