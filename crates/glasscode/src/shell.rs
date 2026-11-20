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
    /// The person's open pull requests and the ones that ask for their review.
    PullRequests,
    Usage,
    /// Settings; its Appearance section is the Themes page.
    Settings,
    /// The first run (`first_run.rs`): no sidebars, no status bar.
    FirstRun,
    /// The manager's page (`manager_ui.rs`): the agent above every session.
    Manager,
}

/// A tab of the centre: a session of the worktree on screen, or a file (`file_view.rs`). The
/// session view draws them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CenterTab {
    Session(String),
    File(std::path::PathBuf),
}

/// A popover over the status bar (`status_bar.rs` draws them).
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

// The widths of the sidebars, their limits and the width of the window in the functions below
// are logical: as at 100 %. `Layout.sidebar`, `right` and `tickets` keep them so, and a sidebar
// shows `theme::scale()` times as wide, as everything in it does: a change of scale keeps what
// fits in a sidebar, and `fit_panels_at` keeps the sidebars inside the window.

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

/// `fit_panels` in a window `width` pixels wide on the screen at `scale`: the logical widths fit
/// in `width / scale`, and show `scale` times as wide.
pub fn fit_panels_at(width: f32, scale: f32, left: Option<(f32, f32)>, right: Option<(f32, f32)>) -> (Option<f32>, Option<f32>) {
    let (left, right) = fit_panels(width / scale, left, right);
    (left.map(|w| w * scale), right.map(|w| w * scale))
}

/// The right panel's tab as `Layout.right_tab` keeps it.
fn right_tab_key(tab: RightTab) -> &'static str {
    match tab {
        RightTab::PullRequest => "pull-request",
        RightTab::Checks => "checks",
        RightTab::Changes => "changes",
        RightTab::Files => "files",
        RightTab::Glasscode => "glasscode",
    }
}

/// The saved right tab; the Glasscode tab only where it exists.
pub fn right_tab_from(key: &str) -> Option<RightTab> {
    match key {
        "pull-request" => Some(RightTab::PullRequest),
        "checks" => Some(RightTab::Checks),
        "changes" => Some(RightTab::Changes),
        "files" => Some(RightTab::Files),
        "glasscode" if crate::glasscode_ui::VOICE => Some(RightTab::Glasscode),
        _ => None,
    }
}

// ----- the title bar at narrow widths -----------------------------------------------------------

/// The title bar's optional items: which show at a width. The wordmark goes first, then the
/// breadcrumb, then the words of the Jump field (its icon and its key stay). Its sizes are
/// logical, as the sidebars' are.
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
/// The window buttons of Windows (window_controls.rs).
const WINDOW_BUTTONS: f32 = 3. * crate::window_controls::BUTTON_WIDTH;
/// The room of the traffic lights of macOS. The system draws them at its own size, so this room
/// is in pixels on the screen at each scale.
const TRAFFIC_LIGHTS: f32 = 84.;
/// The title bar's height.
pub const TITLE_HEIGHT: f32 = 44.;

/// The width of a pill in the title bar with `chars` letters of 12 px text.
pub fn pill_width(chars: usize) -> f32 {
    chars as f32 * 7. + 36.
}

/// What the title bar takes whatever the width: its padding (the traffic lights on macOS), the
/// mark, the four buttons, the pills that show, the window buttons of Windows, and the gaps. At
/// `scale`, the traffic lights take less of the logical width.
pub fn title_chrome(windows: bool, pills: &[f32], scale: f32) -> f32 {
    let padding = if windows { 12. + WINDOW_BUTTONS } else { TRAFFIC_LIGHTS / scale + 12. };
    // Gaps: after the mark, the buttons, the breadcrumb, the Jump field and each pill.
    let gaps = (4 + pills.len()) as f32 * BAR_GAP;
    padding + MARK + 4. * BAR_BUTTON + pills.iter().sum::<f32>() + gaps
}

/// `title_fit` for a title bar `width` pixels wide on the screen at `scale`, with these pills.
pub fn title_fit_at(width: f32, scale: f32, windows: bool, pills: &[f32]) -> TitleFit {
    title_fit(width / scale, title_chrome(windows, pills, scale))
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
        .size(theme::sz(BAR_BUTTON))
        .rounded(theme::sz(6.))
        .when(pressed, |d| d.bg(theme::selected()))
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(theme::hover())))
        .child(crate::icons::icon(glyph, theme::sz(16.), theme::sz(16.), color))
}

/// A row at the top of the left sidebar: a view, or New session.
fn nav_row(id: &'static str, glyph: &'static [u8], label: &'static str, on: bool, count: usize, key: Option<String>) -> Stateful<gpui::Div> {
    let color = if on { theme::text_strong() } else { theme::text_soft() };
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2p5()
        .h(theme::sz(30.))
        .px_2()
        .rounded_md()
        .cursor_pointer()
        .text_color(color)
        .when(on, |d| d.bg(theme::selected()).font_weight(FontWeight::MEDIUM))
        .hover(|s| s.bg(theme::hover()))
        .child(crate::icons::icon(glyph, theme::sz(16.), theme::sz(16.), color))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
        .when(count > 0, |d| {
            d.child(
                div()
                    .flex_none()
                    .min_w(theme::sz(18.))
                    .h(theme::sz(18.))
                    .px(theme::sz(5.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(crate::sidebar::attention())
                    .text_color(theme::on_attention())
                    .text_size(theme::sz(10.5))
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
    /// no window, as the ticks have none. The first run is left only through its own buttons,
    /// which close it first: nothing else (a notification, an agent's command) takes the person
    /// out of it, in the middle of a sign-in.
    pub(crate) fn enter(&mut self, view: View) {
        if self.first_run.is_some() && view != View::FirstRun {
            return;
        }
        let (from, to) = (self.place(), Place { view, session: self.state.active_session.clone().filter(|_| view == View::Sessions) });
        record(&mut self.history, from, &to);
        self.view = view;
    }

    /// The session `id` becomes the active one (`select_session`), and with `open` the view is
    /// Sessions (`open_session`). A change of place puts the place left on the back list; a
    /// session made active while the person is in another view is not one.
    pub(crate) fn enter_session(&mut self, id: &str, open: bool) {
        let view = if open && self.first_run.is_none() { View::Sessions } else { self.view };
        let (from, to) = (self.place(), Place { view, session: Some(id.to_string()).filter(|_| view == View::Sessions) });
        record(&mut self.history, from, &to);
        self.view = view;
    }

    /// Go to a view the way its row does: Board refreshes, Settings opens at its last section.
    pub fn go(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        match view {
            View::Today => self.show_today(cx),
            View::Board => self.show_tickets(cx),
            View::PullRequests => self.show_prs(cx),
            View::Sessions => {
                self.center = crate::center::Center::Session;
                self.show_sessions(window, cx);
            }
            View::FirstRun => self.open_first_run(crate::first_run::Step::Projects, cx),
            View::Usage => self.show_view(View::Usage, cx),
            View::Manager => self.show_manager(window, cx),
            View::Settings => {
                let section = self.settings.section;
                self.open_settings(section, window, cx);
            }
        }
        // The terminal that had the keyboard is gone from the screen: the window takes it, so
        // its keys (back, forward, the panels) keep working, as after a click.
        if self.view != View::Sessions {
            window.focus(&self.focus, cx);
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
                self.open_session(&id, window, cx);
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

    /// The view has a right panel to show: the session's, or Today's Inbox and Activity. The
    /// ticket being read on the Board is a panel over the Board (`tickets_view.rs`), not this one:
    /// it takes no width from the page, and nothing of the window moves when it opens.
    pub(crate) fn view_has_right(&self) -> bool {
        matches!(self.view, View::Sessions | View::Today)
    }

    /// The width a sidebar shows at, in pixels on the screen: as the person left it, at the scale,
    /// or narrower when the window is narrow (`fit_panels_at`). A sidebar that does not show has
    /// the width it was left at. Scaled already: `px(self.sidebar_width())`, never `sz`.
    pub(crate) fn shown_width(&self, side: Side) -> f32 {
        let right_side = self.right_side();
        let limit = |side: Side| (self.saved_width(side), crate::sessions_ui::edge_limits(side).0);
        let left = (self.view_has_left() && !self.state.layout.left_hidden).then(|| limit(Side::Left));
        let right = (self.view_has_right() && self.show_right).then(|| limit(right_side));
        let scale = theme::scale();
        let (left, right) = fit_panels_at(self.window_width, scale, left, right);
        let shown = match side {
            Side::Left => left,
            _ if side == right_side => right,
            _ => None,
        };
        shown.unwrap_or_else(|| self.saved_width(side) * scale)
    }

    /// The right panel's edge: the Board's reader keeps its own width.
    fn right_side(&self) -> Side {
        if self.view == View::Board { Side::Tickets } else { Side::Right }
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
        frames::begin();
        // Glass cards take only the backdrop layer that this frame paints.
        crate::backdrop::begin_frame();
        // GPUI's short forms (`p_2`, `gap_3`, `text_sm`) are in rems: the scale reaches them
        // through the rem, and the `px` sizes through `theme::sz`.
        window.set_rem_size(theme::rem_size());
        self.window_width = f32::from(window.viewport_size().width);
        let modal = self.modal.as_ref().map(|m| m.render(self, window, cx));
        let title_bar = frames::part(frames::Part::Title, || self.render_title_bar(window, cx).into_any_element());
        let has_left = self.view_has_left();
        let left = (has_left && !self.state.layout.left_hidden).then(|| frames::part(frames::Part::Left, || self.render_left(window, cx)));
        let main = frames::part(frames::Part::Main, || self.render_main(window, cx));
        let has_right = self.view_has_right();
        let right = if has_right && self.show_right { frames::part_some(frames::Part::Right, || self.render_right(window, cx)) } else { None };
        let right_side = self.right_side();
        // The first run has no status bar: nothing of it is in use yet.
        let status_bar = (self.view != View::FirstRun).then(|| frames::part(frames::Part::Status, || self.render_status_bar(cx).into_any_element()));
        let popover = self.popover.map(|p| self.render_popover(p, window, cx));
        // The manager's bubble, in the bottom right corner of each view but its own page and
        // the first run, and not while a popover of the status bar takes that corner. A toast
        // goes above it.
        let bubble = (self.view != View::FirstRun && self.popover.is_none()).then(|| self.render_manager_bubble(cx)).flatten();
        let toast_bottom = if bubble.is_some() { 96. } else { 40. };
        let shell = div()
            .size_full()
            .flex()
            .flex_col()
            .children(frames::first_mark())
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
            .children(status_bar)
            .children(popover)
            .children(bubble)
            .when_some(self.render_kit(cx), |d, kit| d.child(kit))
            .when_some(self.render_backdrop_page(window, cx), |d, page| d.child(page))
            .when_some(self.render_row_menu(cx), |d, menu| d.child(menu))
            .when_some(modal, |d, modal| d.child(modal))
            .when_some(self.render_palette(cx), |d, palette| d.child(palette))
            .when_some(self.toast.as_ref().map(|t| t.0.clone()), |d, message| {
                d.child(
                    deferred(
                        div()
                            .absolute()
                            .bottom(theme::sz(toast_bottom))
                            .right(theme::sz(16.))
                            .max_w(theme::sz(460.))
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
            .children(frames::last_mark())
            .into_any_element();
        frames::built();
        shell
    }

    /// The centre of the view.
    fn render_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.view {
            View::Today => self.render_today(window, cx),
            View::Board => self.render_tickets(window, cx),
            View::PullRequests => self.render_prs(window, cx),
            View::Usage => self.render_usage(window, cx),
            View::Settings => self.render_settings(window, cx),
            View::FirstRun => self.render_first_run(window, cx),
            View::Manager => self.render_manager(window, cx),
            View::Sessions => match self.center.clone() {
                crate::center::Center::Session => self.render_center(window, cx).into_any_element(),
                crate::center::Center::Server(id) => self.render_server_center(&id, cx),
            },
        }
    }

    /// The left sidebar: "Workspace" with the sessions that need the person and those at work,
    /// New session, the rows of the views, the tree of projects, worktrees and sessions (the
    /// Server group and "Add project…" at its end), and Settings at the bottom.
    pub fn render_left(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let view = self.view;
        // Today: what is due or late, and what waits for the person's OK; Board: review requests.
        let today_count = self.today_view().due + self.control.approvals.len();
        let board_count = self.tickets.reviews.len();
        let prs_count = self.prs_count();
        // Over all projects, also folded ones: a dot and a number each, as words do not fit at
        // the narrowest width.
        let (needs, working) = self.sidebar_counts();
        let tally = |color: gpui::Hsla, n: usize| div().flex().flex_none().items_center().gap(theme::sz(4.)).child(ui::dot(color)).child(n.to_string());
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(theme::sz(10.))
            .px(theme::sz(14.))
            .pt(theme::sz(10.))
            .pb(theme::sz(6.))
            .text_size(theme::sz(11.5))
            .text_color(theme::muted())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(theme::sz(13.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_strong())
                    .child("Workspace"),
            )
            .when(needs > 0, |d| d.child(tally(theme::attention(), needs)))
            .when(working > 0, |d| d.child(tally(theme::added(), working)));
        // The shortcut takes the room of the words in a narrow sidebar: there it goes.
        let roomy = self.sidebar_width() / theme::scale() >= 260.;
        let new_session = div()
            .id("nav-new-session")
            .flex()
            .flex_none()
            .items_center()
            .gap(theme::sz(8.))
            .h(theme::sz(32.))
            .mx(theme::sz(8.))
            .mb(theme::sz(6.))
            .px(theme::sz(10.))
            .rounded(theme::sz(8.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::raised())
            .text_color(theme::text_soft())
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()).text_color(theme::text()))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_session(None, None, window, cx)))
            .child(crate::icons::icon(crate::icons::PLUS, theme::sz(15.), theme::sz(15.), theme::text_soft()))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child("New session"))
            .when(roomy, |d| d.child(ui::kbd(crate::keymap::label(&NewSession))));
        let views = div()
            .flex()
            .flex_none()
            .flex_col()
            .gap_px()
            .px(theme::sz(6.))
            .pb(theme::sz(6.))
            .border_b_1()
            .border_color(theme::border())
            // The manager first, where the rail had it (only while it is on).
            .children(self.render_manager_nav(cx))
            .child(nav_row("nav-today", crate::icons::TODAY, "Today", view == View::Today, today_count, None).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go(View::Today, window, cx))))
            .child(
                nav_row("nav-board", crate::icons::TICKETS, "Board", view == View::Board, board_count, None)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go(View::Board, window, cx))),
            )
            .child(
                nav_row("nav-prs", crate::icons::PULL_REQUEST, "Pull requests", view == View::PullRequests, prs_count, None)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go(View::PullRequests, window, cx))),
            )
            .child(nav_row("nav-usage", crate::icons::USAGE, "Usage", view == View::Usage, 0, None).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go(View::Usage, window, cx))));
        // The tree keeps where it was scrolled to from frame to frame.
        let scroll = window.use_keyed_state("left-tree-scroll", cx, |_, _| gpui::ScrollHandle::new()).read(cx).clone();
        let tree = frames::part(frames::Part::Tree, || div().flex().flex_none().flex_col().px(theme::sz(6.)).pt(theme::sz(4.)).pb(theme::sz(10.)).children(self.render_tree(cx)).into_any_element());
        let settings = div().flex_none().px(theme::sz(6.)).py(theme::sz(6.)).border_t_1().border_color(theme::border()).child(
            nav_row("nav-settings", crate::icons::GEAR, "Settings", view == View::Settings, 0, Some(crate::keymap::label(&OpenSettings)))
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
            .child(header)
            .child(new_session)
            .child(views)
            .child(ui::scroll_area("left-tree", &scroll, tree))
            .child(settings)
            .into_any_element()
    }

    /// The right panel of the view: the session's (Files, Changes, Checks, Pull request), or
    /// Today's (Inbox, Activity).
    pub fn render_right(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.view {
            View::Sessions => Some(self.render_right_panel(cx)),
            View::Today => Some(self.render_today_right(cx)),
            _ => None,
        }
    }

    /// The status bar's popover: Usage, Resources or Ports (`status_bar.rs`).
    pub fn render_popover(
        &mut self,
        popover: Popover,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.status_popover(popover, window, cx)
    }

    pub(crate) fn render_title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (project, worktree) = match (&self.center, self.active_session()) {
            _ if matches!(self.view, View::Usage | View::Settings) => (
                match self.view {
                    View::Usage => "Usage",
                    _ => "Settings",
                }
                .to_string(),
                String::new(),
            ),
            _ if self.view == View::Today => ("Today".to_string(), String::new()),
            _ if self.view == View::Manager => (self.state.manager.name.clone(), String::new()),
            _ if self.view == View::FirstRun => ("Set up Glasscode".to_string(), String::new()),
            _ if self.view == View::PullRequests => ("Pull requests".to_string(), "GitHub".to_string()),
            _ if self.view == View::Board => (
                "Board".to_string(),
                {
                    // The service the cards on screen came from, not the one a setting names.
                    let service = match self.tickets.fetched_from {
                        glasscode_core::model::Provider::GitHub => "GitHub",
                        glasscode_core::model::Provider::Linear => "Linear",
                    };
                    self.tickets.snapshot.as_ref().filter(|s| !s.org.is_empty()).map(|s| format!("{service} · {}", s.org)).unwrap_or_else(|| service.into())
                },
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
            title_fit_at(f32::from(window.viewport_size().width), theme::scale(), cfg!(windows), &pills)
        };
        // The first run ends only through its own buttons: back, forward and Jump wait for it.
        let flow = self.first_run.is_some();
        let can_back = !self.history.0.is_empty() && !flow;
        let can_forward = !self.history.1.is_empty() && !flow;
        let has_left = self.view_has_left();
        let has_right = self.view_has_right();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            // The drag area of Windows is this bar as drawn, so it is as tall as the bar.
            .h(theme::sz(TITLE_HEIGHT))
            .pl(px(TRAFFIC_LIGHTS))
            .pr_3()
            // Windows: no traffic lights at the left, and the window buttons end the bar at the right.
            .when(cfg!(windows), |d| {
                d.pl(theme::sz(12.))
                    .pr_0()
                    .window_control_area(gpui::WindowControlArea::Drag)
            })
            .bg(theme::chrome())
            .border_b_1()
            .border_color(theme::border())
            // The bar has three parts. The left and the right one share what the Jump field leaves
            // (each grows from nothing), so the field is in the middle of the window on every page
            // and stays there when a side panel opens or the breadcrumb changes. Only when one
            // side needs more than its half does the field move.
            .child(div().flex().flex_1().min_w_0().items_center().gap_2().overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(crate::icons::icon(crate::icons::LOGO, theme::sz(19.), theme::sz(18.), theme::text_strong()))
                    .when(fit.wordmark, |d| d.child(div().font_weight(FontWeight::SEMIBOLD).text_size(theme::sz(14.)).child("Glasscode"))),
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
                        .max_w(theme::sz(380.))
                        .items_center()
                        .gap_1p5()
                        .text_color(theme::muted())
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(project)
                        .when(!worktree.is_empty(), |d| {
                            d.child(div().text_color(theme::faint()).child("/"))
                                .child(div().min_w_0().overflow_hidden().text_ellipsis().font_family(theme::MONO_FONT).text_size(theme::sz(12.)).text_color(theme::text_soft()).child(worktree))
                        }),
                )
            }))
            // Jump: the command palette.
            .when(!flow, |d| d.child(
                div()
                    .id("title-search")
                    // Windows: a press opens the palette, it does not move the window.
                    .when(cfg!(windows), |d| d.occlude())
                    .flex()
                    .when(fit.jump_words, |d| d.w(theme::sz(460.)).flex_shrink(1.).min_w(theme::sz(180.)))
                    .when(!fit.jump_words, |d| d.flex_none())
                    .items_center()
                    .gap_2()
                    .h(theme::sz(30.))
                    .px_2p5()
                    .rounded(theme::sz(8.))
                    .border_1()
                    .border_color(theme::border_strong())
                    .bg(theme::raised())
                    .text_color(theme::muted())
                    .cursor_pointer()
                    .hover(|s| s.border_color(theme::faint()))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_palette(window, cx)))
                    .child(crate::icons::icon(crate::icons::SEARCH, theme::sz(14.), theme::sz(14.), theme::faint()))
                    .when(fit.jump_words, |d| d.child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child("Search sessions, tickets and commands")))
                    .child(ui::kbd(crate::keymap::label(&OpenPalette))),
            ))
            // The pills give way before anything overlaps or the window buttons leave the bar: a
            // long server name first, then (at 130 % in the narrowest window, with a move and an
            // update) the pills before it, from the right.
            .child(div().flex().flex_1().items_center().justify_end().gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_shrink(1.)
                    .min_w_0()
                    .overflow_hidden()
                    .when_some(switch, |d, buttons| d.child(crate::window_controls::clickable(buttons)))
                    .when_some(update, |d, pill| d.child(crate::window_controls::clickable(pill)))
                    .child(div().flex().flex_shrink(1.).min_w_0().overflow_hidden().child(crate::window_controls::clickable(self.server_pill(cx)))),
            )
            .child(crate::window_controls::clickable(
                bar_button("title-right-panel", crate::icons::PANEL_RIGHT, has_right, has_right && self.show_right)
                    .when(has_right, |b| b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_right(cx)))),
            ))
            .when(cfg!(windows), |d| {
                d.child(crate::window_controls::WindowControls)
            }))
    }

    /// The status bar: its left side, which gives way and then clips in a narrow window, and the
    /// counts on the right, which always show.
    pub(crate) fn render_status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let left = self.status_left(cx);
        let right = self.status_right(cx);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_4()
            .h(theme::sz(crate::status_bar::BAR_HEIGHT))
            .px_3()
            .bg(theme::chrome())
            .border_t_1()
            .border_color(theme::border())
            .text_size(theme::sz(11.5))
            .text_color(theme::muted())
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_4()
                    .overflow_hidden()
                    .children(left),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(theme::sz(2.))
                    .children(right),
            )
    }

    /// The left side of the status bar: a usage meter for each account with limits and their
    /// refresh (U17), then the items the bar had: branch, changes, GitHub account, CLI versions,
    /// saved sessions, and the agent with its account. In a narrow window parts give way in the
    /// order of `status_bar::fit_at`: the meters' words and week meters first.
    pub fn status_left(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
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
        let warning = !cli_warning.is_empty();
        let cli = if warning {
            Some(format!("⚠ {cli_warning}"))
        } else {
            Some(cli_ok).filter(|ok| !ok.is_empty())
        };
        let branch = format!(
            "⎇ {}",
            session.map(|s| s.branch.clone()).unwrap_or_default()
        );
        let changes = if self.changes.is_empty() {
            "Clean".to_string()
        } else {
            format!("{} changed", self.changes.len())
        };
        let github =
            session.and_then(|s| crate::github_accounts_ui::status_label(self, &s.project_id));
        let saved = {
            let n = self.state.sessions.len();
            let synced = self.mirror.synced.is_some() && self.mirror.error.is_none();
            format!(
                "{n} {} saved · {}",
                if n == 1 { "session" } else { "sessions" },
                if synced {
                    "in sync on the server"
                } else {
                    "restored on relaunch"
                }
            )
        };
        let usage = self.status_accounts();
        let texts = crate::status_bar::Texts {
            branch: &branch,
            changes: &changes,
            github: github.as_deref(),
            cli: cli.as_deref(),
            saved: &saved,
            account: account.as_deref(),
        };
        let fit = crate::status_bar::fit_at(
            self.window_width,
            theme::scale(),
            &usage,
            &texts,
            self.status_summary().as_ref(),
        );
        // The long items also end in an ellipsis; the branch and the GitHub account keep their
        // room up to a width.
        let item = || {
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
        };
        let kept = |width: f32| item().flex_none().max_w(theme::sz(width));
        let mut items = self.status_usage(&usage, fit, cx);
        items.push(
            kept(180.)
                .font_family(theme::MONO_FONT)
                .child(branch)
                .into_any_element(),
        );
        items.push(div().flex_none().child(changes).into_any_element());
        if let Some(label) = github.filter(|_| fit.github) {
            items.push(kept(200.).child(label).into_any_element());
        }
        if let Some(cli) = cli.filter(|_| fit.cli) {
            items.push(
                item()
                    .when(warning, |d| d.text_color(theme::attention()))
                    .child(cli)
                    .into_any_element(),
            );
        }
        if fit.saved {
            items.push(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .flex_none()
                            .size(theme::sz(6.))
                            .rounded_full()
                            .bg(theme::added()),
                    )
                    .child(item().child(saved))
                    .into_any_element(),
            );
        }
        if let Some(account) = account.filter(|_| fit.account) {
            items.push(item().child(account).into_any_element());
        }
        items
    }

    /// The right side of the status bar: the memory of the sessions' programs, their terminals
    /// with the orphans, and their ports (`status_bar.rs`).
    pub fn status_right(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.status_counts(cx)
    }
}

// ----- what a draw costs (debug builds) ---------------------------------------------------------

/// Debug builds with `GLASSCODE_DEBUG_FRAMES` set: what the draws of the main window cost, in the
/// log once a second (slice U24). Each draw of the window runs `Workspace::render` first, so a
/// render is a draw. For each render: the time to build the elements (`render`), then layout
/// (to the first prepaint), then prepaint and paint (to the last paint), for each part of the
/// window; and the time between renders, whose rhythm names the tick that asked for them. The
/// time to send the frame to the graphics card is the rest of the process's time. Off, each call
/// is one read of a thread-local flag.
pub(crate) mod frames {
    use std::cell::{Cell, RefCell};
    use std::time::{Duration, Instant};

    use gpui::{AnyElement, App, Bounds, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Window, canvas, div, prelude::*, px};

    /// The parts of the window that are timed. Main holds the start box, the cards and the list.
    #[derive(Clone, Copy)]
    pub enum Part {
        Title,
        Left,
        Tree,
        Main,
        Start,
        Cards,
        List,
        Right,
        Status,
    }
    const NAMES: [&str; 9] = ["title", "left", "tree", "main", "start", "cards", "list", "right", "status"];

    /// Build, then the subtree's request for layout, prepaint and paint.
    #[derive(Default)]
    struct Times([Duration; 4]);

    #[derive(Default)]
    struct Second {
        since: Option<Instant>,
        renders: u32,
        /// Render start to the first prepaint (build and layout), and to the last paint.
        laid_out: Duration,
        painted: Duration,
        built: Duration,
        parts: [Times; 9],
        /// Milliseconds between renders.
        gaps: Vec<u32>,
        /// The three tick loops (250, 350 and 700 ms): runs and time.
        ticks: [(u32, Duration); 3],
    }

    thread_local! {
        static ON: bool = cfg!(debug_assertions) && std::env::var_os("GLASSCODE_DEBUG_FRAMES").is_some();
        static NOW: RefCell<Second> = RefCell::new(Second::default());
        static BEGUN: Cell<Option<Instant>> = const { Cell::new(None) };
        static LAST: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    pub fn on() -> bool {
        ON.with(|on| *on)
    }

    /// The start of a render.
    pub fn begin() {
        if !on() {
            return;
        }
        let now = Instant::now();
        BEGUN.set(Some(now));
        let gap = LAST.replace(Some(now)).map(|last| now.duration_since(last).as_millis() as u32);
        NOW.with_borrow_mut(|s| {
            s.since.get_or_insert(now);
            s.renders += 1;
            s.gaps.extend(gap);
        });
    }

    /// The end of `render`: the elements are built.
    pub fn built() {
        if let Some(begun) = BEGUN.get().filter(|_| on()) {
            NOW.with_borrow_mut(|s| s.built += begun.elapsed());
        }
    }

    fn add(part: Part, phase: usize, took: Duration) {
        NOW.with_borrow_mut(|s| s.parts[part as usize].0[phase] += took);
    }

    /// Builds a part of the window and, when on, times it in each phase.
    pub fn part(part: Part, build: impl FnOnce() -> AnyElement) -> AnyElement {
        if !on() {
            return build();
        }
        let begun = Instant::now();
        let element = build();
        add(part, 0, begun.elapsed());
        Timed { part, child: element }.into_any_element()
    }

    /// `part` for a part that may not show.
    pub fn part_some(part: Part, build: impl FnOnce() -> Option<AnyElement>) -> Option<AnyElement> {
        if !on() {
            return build();
        }
        let begun = Instant::now();
        let element = build()?;
        add(part, 0, begun.elapsed());
        Some(Timed { part, child: element }.into_any_element())
    }

    /// The time of a tick loop (0: 250 ms, 1: 350 ms, 2: 700 ms). The 700 ms loop also writes the
    /// log, so a second with no render is logged too.
    pub fn tick(which: usize, took: Duration) {
        if !on() {
            return;
        }
        NOW.with_borrow_mut(|s| {
            s.ticks[which].0 += 1;
            s.ticks[which].1 += took;
        });
        if which == 2 {
            log_if_due();
        }
    }

    /// Marks for the first child (its prepaint: layout is done) and the last child (its paint:
    /// paint is done) of the window's root.
    pub fn first_mark() -> Option<AnyElement> {
        on().then(|| {
            mark(
                || {
                    if let Some(begun) = BEGUN.get() {
                        NOW.with_borrow_mut(|s| s.laid_out += begun.elapsed());
                    }
                },
                || (),
            )
        })
    }

    pub fn last_mark() -> Option<AnyElement> {
        on().then(|| {
            mark(
                || (),
                || {
                    if let Some(begun) = BEGUN.take() {
                        NOW.with_borrow_mut(|s| s.painted += begun.elapsed());
                    }
                },
            )
        })
    }

    fn mark(prepaint: impl FnOnce() + 'static, paint: impl FnOnce() + 'static) -> AnyElement {
        div()
            .absolute()
            .top_0()
            .left_0()
            .size(px(1.))
            .child(canvas(move |_, _, _| prepaint(), move |_, _, _, _| paint()).size_full())
            .into_any_element()
    }

    fn log_if_due() {
        let line = NOW.with_borrow_mut(|s| {
            let since = s.since.get_or_insert_with(Instant::now);
            let seconds = since.elapsed().as_secs_f32();
            if seconds < 1. {
                return None;
            }
            let line = line(s, seconds);
            *s = Second { since: Some(Instant::now()), ..Second::default() };
            Some(line)
        });
        if let Some(line) = line {
            log::warn!("frames: {line}");
        }
    }

    fn ms(d: Duration) -> f32 {
        d.as_secs_f32() * 1000.
    }

    fn line(s: &Second, seconds: f32) -> String {
        let n = s.renders.max(1) as f32;
        let parts: Vec<String> = s
            .parts
            .iter()
            .enumerate()
            .filter(|(_, t)| t.0.iter().any(|d| !d.is_zero()))
            .map(|(i, t)| format!("{} {:.2}/{:.2}/{:.2}/{:.2}", NAMES[i], ms(t.0[0]) / n, ms(t.0[1]) / n, ms(t.0[2]) / n, ms(t.0[3]) / n))
            .collect();
        let ticks: Vec<String> = ["250ms", "350ms", "700ms"].iter().zip(s.ticks).map(|(name, (runs, took))| format!("{name} {runs}x {:.2} ms", ms(took))).collect();
        format!(
            "{seconds:.1} s · {} renders ({:.1}/s) · each: build {:.2} ms, to layout {:.2} ms, to last paint {:.2} ms · parts build/layout/prepaint/paint: {} · gaps ms {:?} · ticks {}",
            s.renders,
            s.renders as f32 / seconds,
            ms(s.built) / n,
            ms(s.laid_out) / n,
            ms(s.painted) / n,
            parts.join(", "),
            s.gaps,
            ticks.join(", ")
        )
    }

    /// A part of the window, timed in each phase of the draw.
    struct Timed {
        part: Part,
        child: AnyElement,
    }

    impl IntoElement for Timed {
        type Element = Self;

        fn into_element(self) -> Self {
            self
        }
    }

    impl gpui::Element for Timed {
        type RequestLayoutState = ();
        type PrepaintState = ();

        fn id(&self) -> Option<ElementId> {
            None
        }

        fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
            None
        }

        fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
            let begun = Instant::now();
            let id = self.child.request_layout(window, cx);
            add(self.part, 1, begun.elapsed());
            (id, ())
        }

        fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
            let begun = Instant::now();
            self.child.prepaint(window, cx);
            add(self.part, 2, begun.elapsed());
        }

        fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
            let begun = Instant::now();
            self.child.paint(window, cx);
            add(self.part, 3, begun.elapsed());
        }
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

    /// The scales of `Appearance.scale`, and the window's smallest width (main.rs).
    const SCALES: [f32; 4] = [0.9, 1., 1.15, 1.3];
    const NARROWEST: f32 = 900.;

    fn close(a: Option<f32>, b: f32) -> bool {
        a.is_some_and(|a| (a - b).abs() < 0.01)
    }

    /// The widths kept are at 100 %: a sidebar shows `scale` times as wide in a wide window. In
    /// the narrowest window at 130 % it narrows as at 100 % in a window of 900 / 1.3 = 692 px.
    #[test]
    fn the_sidebars_grow_with_the_scale() {
        let (left, right, reader) = (Some((300., 220.)), Some((420., 300.)), Some((540., 400.)));
        let (l, r) = fit_panels_at(1920., 1.3, left, right);
        assert!(close(l, 390.) && close(r, 546.), "{l:?} {r:?}");
        let (l, r) = fit_panels_at(1440., 0.9, left, right);
        assert!(close(l, 270.) && close(r, 378.), "{l:?} {r:?}");
        // Sessions at 900 px and 130 %: both at their narrowest, the centre 224 px.
        let (l, r) = fit_panels_at(NARROWEST, 1.3, left, right);
        assert!(close(l, 286.) && close(r, 390.), "{l:?} {r:?}");
        // The Board's reader: the centre keeps 94 px, the least of all.
        let (l, r) = fit_panels_at(NARROWEST, 1.3, left, reader);
        assert!(close(l, 286.) && close(r, 520.), "{l:?} {r:?}");
        assert_eq!(fit_panels_at(1440., 1., left, right), fit_panels(1440., left, right), "100 % is as before");
    }

    /// A change of scale never hides a sidebar and never makes the two wider than the window,
    /// at any width the window can have, with the sidebars at any width a drag leaves.
    #[test]
    fn no_scale_hides_a_sidebar_or_overflows_the_window() {
        let (left_min, left_max) = crate::sessions_ui::edge_limits(Side::Left);
        for scale in SCALES {
            for width in [NARROWEST, 1000., 1280., 1440., 2560.] {
                for right_side in [Side::Right, Side::Tickets] {
                    let (right_min, right_max) = crate::sessions_ui::edge_limits(right_side);
                    for (wanted_left, wanted_right) in [(left_min, right_min), (300., 420.), (left_max, right_max)] {
                        let (l, r) = fit_panels_at(width, scale, Some((wanted_left, left_min)), Some((wanted_right, right_min)));
                        let (l, r) = (l.expect("the left sidebar shows"), r.expect("the right panel shows"));
                        assert!(l + r <= width, "{l} + {r} in {width} px at {scale}");
                        assert!(l >= left_min * scale - 0.01 && r >= right_min * scale - 0.01, "never below their narrowest at {scale}");
                    }
                    let (l, r) = fit_panels_at(width, scale, None, Some((right_max, right_min)));
                    assert!(l.is_none() && r.is_some_and(|r| r <= width));
                }
            }
        }
        // A drag hides below this, so a sidebar at its narrowest is not near hiding.
        assert!(left_min > HIDE_BELOW);
    }

    #[test]
    fn the_right_tab_goes_round() {
        for tab in [RightTab::Files, RightTab::Changes, RightTab::Checks, RightTab::PullRequest] {
            assert!(right_tab_from(right_tab_key(tab)) == Some(tab));
        }
        // The names that a state.json of U03 keeps still load.
        assert!(right_tab_from("pull-request") == Some(RightTab::PullRequest));
        assert!(right_tab_from("changes") == Some(RightTab::Changes));
        assert!(right_tab_from("files") == Some(RightTab::Files));
        assert!(right_tab_from("glasscode") == crate::glasscode_ui::VOICE.then_some(RightTab::Glasscode));
        assert!(right_tab_from("ci").is_none(), "an unknown tab is the default");
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
                assert_eq!(title_fit_at(width, 1., windows, &quiet), all, "a quiet bar at {width} px (Windows: {windows})");
            }
        }
        let fit = |width: f32, pills: &[f32]| title_fit_at(width, 1., true, pills);
        assert_eq!(fit(900., &updating), TitleFit { wordmark: false, breadcrumb: false, jump_words: true });
        assert_eq!((fit(1100., &updating), fit(1600., &updating)), (all, all));
        assert_eq!((fit(900., &busy), fit(1100., &busy), fit(1600., &busy)), (none, none, all));
        // The rest fits even then: all but the server pill (which clips) and the small Jump field.
        assert!(title_chrome(true, &busy[1..], 1.) + JUMP_SMALL <= 900.);
    }

    /// At each scale the items the bar shows fit in it. At 130 % in the narrowest window a quiet
    /// bar keeps the Jump field's words, and with the update pill too it all still fits.
    #[test]
    fn the_title_bar_fits_at_each_scale() {
        let server = pill_width("Server…".chars().count());
        let update = pill_width("Restart to update".len());
        let busy = [pill_width("my-build-server · connecting".chars().count()), update, pill_width("Moving 10 of 10…".chars().count())];
        for scale in SCALES {
            for windows in [true, false] {
                for width in [NARROWEST, 1100., 1440., 1600.] {
                    for pills in [&[server][..], &[server, update][..]] {
                        let fit = title_fit_at(width, scale, windows, pills);
                        let shown = title_chrome(windows, pills, scale)
                            + if fit.jump_words { JUMP_WORDS } else { JUMP_SMALL }
                            + if fit.breadcrumb { BREADCRUMB } else { 0. }
                            + if fit.wordmark { WORDMARK } else { 0. };
                        assert!(shown * scale <= width + 0.01, "{shown} at {scale} in {width} px (Windows: {windows}, {} pills)", pills.len());
                    }
                }
                // The busy bar does not fit there at 130 %; the pills give way (they clip, from
                // the right), and all the rest fits without them.
                assert!(title_chrome(windows, &[], scale) + JUMP_SMALL <= NARROWEST / scale, "all but the pills at {scale}");
            }
        }
        let quiet = TitleFit { wordmark: false, breadcrumb: false, jump_words: true };
        let none = TitleFit { wordmark: false, breadcrumb: false, jump_words: false };
        assert_eq!(title_fit_at(NARROWEST, 1.3, true, &[server]), quiet);
        assert_eq!(title_fit_at(NARROWEST, 1.3, false, &[server]), quiet);
        assert_eq!(title_fit_at(NARROWEST, 1.3, true, &[server, update]), none);
        assert_eq!(title_fit_at(NARROWEST, 1.3, true, &busy), none);
        assert_eq!(title_fit_at(1600., 1.3, true, &[server]), TitleFit { wordmark: true, breadcrumb: true, jump_words: true });
    }

    #[test]
    fn the_title_bar_drops_the_wordmark_first() {
        let chrome = title_chrome(true, &[pill_width(7)], 1.);
        let roomy = chrome + JUMP_WORDS + BREADCRUMB + WORDMARK;
        assert_eq!(title_fit(roomy, chrome), TitleFit { wordmark: true, breadcrumb: true, jump_words: true });
        assert_eq!(title_fit(roomy - 1., chrome), TitleFit { wordmark: false, breadcrumb: true, jump_words: true });
        assert_eq!(title_fit(chrome + JUMP_WORDS + BREADCRUMB - 1., chrome), TitleFit { wordmark: false, breadcrumb: false, jump_words: true });
        assert_eq!(title_fit(chrome + JUMP_WORDS - 1., chrome), TitleFit { wordmark: false, breadcrumb: false, jump_words: false });
    }
}
