//! The centre of the Sessions view: a tab for each session of the worktree on screen and for each
//! file open in it (`file_view.rs`), the Chat and Terminal control, and the session in a column
//! with a short header, over the backdrop. With no session, the empty state.

use gpui::{AnyElement, ClickEvent, Context, Focusable, FontWeight, ScrollHandle, SharedString, Stateful, Window, deferred, div, prelude::*, px};
use glasscode_core::agents::AgentActivity;
use glasscode_core::model::{AgentKind, Session, SessionView};

use crate::backdrop;
use crate::icons;
use crate::shell::CenterTab;
use crate::sidebar;
use crate::theme;
use crate::ui;
use crate::workspace::{Side, Workspace};

/// Behind a session the backdrop is dimmer than on Today (U04).
const BACKDROP_STRENGTH: f32 = 0.6;
/// The room around the column, over the backdrop; less in the narrowest centre.
const MARGIN: f32 = 12.;
const MARGIN_NARROW: f32 = 6.;
/// The corners of the column.
const RADIUS: f32 = 14.;
/// The narrowest a tab gets: its mark and its state dot.
const TAB_MIN: f32 = 42.;
/// The narrowest a file tab gets: its mark, the start of its name and its × (padding 14, mark
/// 14, gaps 10, × 20, and about six letters). With more tabs than fit, the strip scrolls.
const FILE_TAB_MIN: f32 = 104.;
/// Room in the strip for three tabs at their narrowest, "+" and the Chat and Terminal control.
const STRIP_ROOM: f32 = 360.;
/// Room in the header for the Chat and Terminal control on a line of its own.
const HEADER_ROOM: f32 = 260.;
/// Room in the header for its buttons' words beside a project and branch that are not cut.
const WORDS_ROOM: f32 = 460.;

// ----- the tabs ------------------------------------------------------------------------------------

/// The tabs of the centre while `active` is on screen: one for each session of its worktree, in
/// the order of `sessions`, then the file tabs of `open` in their order (the files open in that
/// worktree, `FileTabs::paths_in`).
pub fn tab_list(sessions: &[Session], active: &Session, open: &[CenterTab]) -> Vec<CenterTab> {
    sessions
        .iter()
        .filter(|s| s.project_id == active.project_id && s.worktree == active.worktree)
        .map(|s| CenterTab::Session(s.id.clone()))
        .chain(open.iter().filter(|t| matches!(t, CenterTab::File(_))).cloned())
        .collect()
}

/// The tab in front: the file tab that was in front while it stays open, else the tab of the
/// session on screen.
pub fn front_tab(tabs: &[CenterTab], was: Option<&CenterTab>, active: &str) -> usize {
    was.filter(|t| matches!(t, CenterTab::File(_)))
        .and_then(|was| tabs.iter().position(|t| t == was))
        .or_else(|| tabs.iter().position(|t| matches!(t, CenterTab::Session(id) if id == active)))
        .unwrap_or(0)
}

/// The tab that comes to the front when the tab at `closing` closes: the one after it, else the
/// one before it. None for the only tab.
pub fn neighbour(tabs: &[CenterTab], closing: usize) -> Option<&CenterTab> {
    if closing >= tabs.len() {
        return None;
    }
    tabs.get(closing + 1).or_else(|| closing.checked_sub(1).and_then(|i| tabs.get(i)))
}

// ----- what fits -----------------------------------------------------------------------------------

/// Where the Chat and Terminal control goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewControl {
    /// At the right of the tab strip.
    Strip,
    /// In the header, which wraps.
    Header,
    /// The narrowest centre: one item of the "⋯" menu, so that the column keeps room for the
    /// conversation.
    Menu,
}

/// What fits in a centre `width` wide (logical, as at 100 %). The header wraps, so at no width
/// is anything in it cut; the strip keeps its room for the tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CentreFit {
    pub view: ViewControl,
    /// The header's buttons show their words, the account its plan, and the state shows; else
    /// the icons and the account's name only (the tab above has the state's dot).
    pub words: bool,
}

pub fn centre_fit(width: f32) -> CentreFit {
    let view = if width >= STRIP_ROOM {
        ViewControl::Strip
    } else if width >= HEADER_ROOM {
        ViewControl::Header
    } else {
        ViewControl::Menu
    };
    CentreFit { view, words: width >= WORDS_ROOM }
}

/// A button of the header: its icon, and its words when they fit.
fn header_button(id: &'static str, glyph: &'static [u8], label: &'static str, words: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(theme::sz(6.))
        .h(theme::sz(26.))
        .min_w(theme::sz(26.))
        .when(words, |d| d.px(theme::sz(8.)))
        .rounded(theme::sz(6.))
        .text_size(theme::sz(12.5))
        .text_color(theme::text_soft())
        .cursor_pointer()
        .hover(|s| s.bg(theme::tint()).text_color(theme::text()))
        .child(icons::icon(glyph, theme::sz(15.), theme::sz(15.), theme::text_soft()))
        .when(words, |d| d.child(label))
}

impl Workspace {
    /// The width of the centre, as at 100 %: the window less the sidebars that show.
    pub(crate) fn centre_width(&self) -> f32 {
        let mut centre = self.window_width;
        if self.view_has_left() && !self.state.layout.left_hidden {
            centre -= self.shown_width(Side::Left);
        }
        if self.view_has_right() && self.show_right {
            centre -= self.shown_width(Side::Right);
        }
        centre / theme::scale()
    }

    /// `Workspace.tabs`, made again from the sessions of the worktree on screen (sessions come
    /// and go in many places) and the files open in it. A file tab stays in front until a
    /// session is shown (`FileTabs::front`; `show_session` notes when in `today.navigated_at`).
    fn sync_tabs(&mut self, active: &Session) {
        let files: Vec<CenterTab> = self.gh.files.tabs.paths_in(&active.worktree).into_iter().map(CenterTab::File).collect();
        self.tabs = tab_list(&self.state.sessions, active, &files);
        let front = self.gh.files.tabs.front(&active.id, self.today.navigated_at).map(CenterTab::File);
        self.tab = front_tab(&self.tabs, front.as_ref(), &active.id);
    }

    /// A click on a tab: a session tab shows its session, as its row in the sidebar does (a
    /// session change, so it goes on the back list); a file tab comes to the front.
    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.get(index).cloned() {
            Some(CenterTab::Session(id)) => {
                self.tab = index;
                self.gh.files.tabs.session_to_front();
                self.open_session(&id, window, cx);
            }
            Some(CenterTab::File(path)) => {
                self.tab = index;
                // A file in Edit takes the keyboard again.
                let editor = self.gh.files.tabs.get(&path).filter(|t| t.editing).and_then(|t| t.editor.clone());
                self.show_file_tab(path, cx);
                if let Some(editor) = editor {
                    window.focus(&editor.read(cx).focus_handle(cx), cx);
                }
            }
            None => {}
        }
    }

    /// The tab's ×. A session closes as with the sidebar's ×; when it is the one on screen, its
    /// neighbour comes to the front first, as a click on that tab would. A file tab in front
    /// gives the front to its neighbour too.
    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index).cloned() else { return };
        let next = neighbour(&self.tabs, index).cloned();
        match tab {
            CenterTab::Session(id) => {
                if self.state.active_session.as_deref() == Some(id.as_str()) {
                    // A file next to it leaves the choice to `close_session`.
                    if let Some(CenterTab::Session(next)) = next {
                        self.open_session(&next, window, cx);
                    }
                }
                self.close_session(&id, window, cx);
            }
            CenterTab::File(path) => {
                // Unsaved changes in its editor: "Save the changes?" first (U20); the answer
                // closes it.
                if !self.ask_before_closing_tab(&path, cx) {
                    self.close_file_tab(&path, window, cx);
                }
            }
        }
    }

    /// The file tab goes, and its neighbour comes to the front if it was in front.
    pub(crate) fn close_file_tab(&mut self, path: &std::path::Path, window: &mut Window, cx: &mut Context<Self>) {
        self.gh.files.tabs.close(path);
        let Some(index) = self.tabs.iter().position(|t| matches!(t, CenterTab::File(p) if p == path)) else {
            return cx.notify();
        };
        let next = neighbour(&self.tabs, index).cloned();
        let in_front = index == self.tab;
        self.tabs.remove(index);
        if in_front {
            self.tab = next.as_ref().and_then(|n| self.tabs.iter().position(|t| t == n)).unwrap_or(0);
            match next {
                Some(CenterTab::File(next)) => self.show_file_tab(next, cx),
                Some(CenterTab::Session(next)) if self.state.active_session.as_deref() != Some(next.as_str()) => self.open_session(&next, window, cx),
                // The session on screen comes back: so does the keyboard, which the window
                // held while the file was in front (`keep_keys_on_screen`).
                Some(CenterTab::Session(_)) => self.focus_active_if_watching(window, cx),
                None => {}
            }
        } else if index < self.tab {
            self.tab -= 1;
        }
        cx.notify();
    }

    pub(crate) fn render_center(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active_session().cloned();
        let fit = centre_fit(self.centre_width());
        // The backdrop shows behind sessions unless Appearance says otherwise; without it the
        // column's glass lies on the plain ground.
        let layer = self.state.appearance.backdrop_in_sessions.then(|| self.backdrop_layer(BACKDROP_STRENGTH, window, cx));
        let (strip, body) = match active {
            Some(session) => {
                self.sync_tabs(&session);
                let strip = self.render_tab_strip(&session, fit, window, cx);
                let body = match self.tabs.get(self.tab).cloned() {
                    Some(CenterTab::File(path)) => {
                        self.keep_keys_on_screen(&session, window, cx);
                        self.render_file_view(&path, fit.view == ViewControl::Strip, cx)
                    }
                    _ => self.render_session(session, fit, window, cx),
                };
                (Some(strip), body)
            }
            None => (None, self.render_empty_state(cx)),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::bg())
            .children(strip)
            .child(div().relative().flex().flex_col().flex_1().min_h_0().p(theme::sz(if fit.view == ViewControl::Menu { MARGIN_NARROW } else { MARGIN })).children(layer).child(body))
    }

    /// With a file in front, the session's terminal or message box is not drawn, and a key sent
    /// to it would go nowhere (Jump, opening a session, Mention in session give it the keyboard).
    /// The window takes the keyboard, as after a click, so that its keys keep working.
    fn keep_keys_on_screen(&mut self, session: &Session, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = self.terminals.get(&session.id).map(|t| t.read(cx).focus_handle_clone());
        // The same choice as `focus_active`, which made the message box only for the Chat view.
        let composer = (self.session_view(session) == SessionView::Chat && session.agent != AgentKind::Shell).then(|| self.chat_composer(&session.id, window, cx).read(cx).focus_handle_clone());
        if terminal.into_iter().chain(composer).any(|handle| handle.is_focused(window)) {
            window.focus(&self.focus, cx);
        }
    }

    /// The tabs of the worktree, "+" for one more session there, and (where it fits) the Chat
    /// and Terminal control.
    fn render_tab_strip(&mut self, session: &Session, fit: CentreFit, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // The tab that comes to the front scrolls into view; otherwise the strip stays where
        // the person scrolled it.
        let front = self.tabs.get(self.tab).cloned();
        let index = self.tab;
        let kept = window.use_keyed_state("center-tabs", cx, |_, _| (ScrollHandle::new(), None::<CenterTab>));
        let scroll = kept.update(cx, |(handle, shown), _| {
            if *shown != front {
                *shown = front.clone();
                handle.scroll_to_item(index);
            }
            handle.clone()
        });
        let mut tabs: Vec<AnyElement> = Vec::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            let on = i == self.tab;
            let close = sidebar::row_button(SharedString::from(format!("center-tab-close-{i}")), icons::CLOSE, Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| this.close_tab(i, window, cx))));
            // A session tab has its state's dot, which the × covers while the pointer is on the
            // tab; a file tab has no state, and its × shows.
            let (mark, title, dot, close, min): (AnyElement, String, AnyElement, Option<AnyElement>, f32) = match tab {
                CenterTab::Session(id) => {
                    let Some(s) = self.state.session(id) else { continue };
                    let dot = match self.row_state(s) {
                        sidebar::RowState::NeedsYou => ui::dot(theme::attention()),
                        sidebar::RowState::Working => ui::dot(theme::added()),
                        sidebar::RowState::Idle => ui::dot(theme::border_strong()),
                    };
                    (sidebar::agent_icon(s.agent, 14. * theme::scale(), self.terminals.contains_key(id)).into_any_element(), self.session_title(s), dot.into_any_element(), Some(close), TAB_MIN)
                }
                // A file with unsaved changes has a dot in the place of its ×, which shows on
                // hover as on a session tab.
                CenterTab::File(path) if self.gh.files.tabs.get(path).is_some_and(|t| t.dirty) => (
                    icons::icon(icons::FILE, theme::sz(14.), theme::sz(14.), theme::muted()).into_any_element(),
                    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    ui::dot(theme::attention()).into_any_element(),
                    Some(close),
                    FILE_TAB_MIN,
                ),
                CenterTab::File(path) => (
                    icons::icon(icons::FILE, theme::sz(14.), theme::sz(14.), theme::muted()).into_any_element(),
                    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    close,
                    None,
                    FILE_TAB_MIN,
                ),
            };
            let group = SharedString::from(format!("center-tab-{i}"));
            tabs.push(
                div()
                    .id(group.clone())
                    .group(group.clone())
                    .relative()
                    .flex()
                    .items_center()
                    .gap(theme::sz(5.))
                    .min_w(theme::sz(min))
                    .max_w(theme::sz(220.))
                    .h(theme::sz(30.))
                    .px(theme::sz(7.))
                    .rounded(theme::sz(7.))
                    .text_size(theme::sz(12.5))
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme::raised()).text_color(theme::text()).font_weight(FontWeight::MEDIUM))
                    .when(!on, |d| d.text_color(theme::muted()).hover(|s| s.bg(theme::hover()).text_color(theme::text())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.select_tab(i, window, cx)))
                    .child(mark)
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
                    .child(dot)
                    .when_some(close, |d, close| {
                        d.child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .right(theme::sz(3.))
                                .flex()
                                .items_center()
                                .rounded(theme::sz(5.))
                                .bg(if on { theme::raised() } else { theme::hover() })
                                .invisible()
                                .group_hover(group, |s| s.visible())
                                .child(close),
                        )
                    })
                    .into_any_element(),
            );
        }
        // With a file in front, the Chat and Terminal control would change a view that is not on
        // screen; it shows again with the session. The file's Edit and Preview take its place.
        let file_in_front = matches!(self.tabs.get(self.tab), Some(CenterTab::File(_)));
        let edit_control = match self.tabs.get(self.tab) {
            Some(CenterTab::File(path)) if fit.view == ViewControl::Strip => Some(self.edit_control(path, cx)),
            _ => None,
        };
        let (project, worktree) = (session.project_id.clone(), session.worktree.clone());
        let add = div()
            .id("center-tab-add")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(theme::sz(28.))
            .rounded(theme::sz(6.))
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_new_session(Some(project.clone()), Some(worktree.clone()), window, cx)))
            .child(icons::icon(icons::PLUS, theme::sz(15.), theme::sz(15.), theme::text_soft()));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(theme::sz(8.))
            .h(theme::sz(42.))
            .px(theme::sz(8.))
            .bg(theme::bg())
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .id("center-tab-list")
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(theme::sz(4.))
                    .overflow_x_scroll()
                    .track_scroll(&scroll)
                    .children(tabs),
            )
            // Outside the tabs' scroll, so that it shows however many tabs there are.
            .child(add)
            .when(fit.view == ViewControl::Strip && session.agent != AgentKind::Shell && !file_in_front, |d| d.child(self.view_control(session, cx)))
            .children(edit_control)
            .into_any_element()
    }

    /// Read a Claude or Codex session as a chat, or see its own terminal.
    fn view_control(&self, session: &Session, cx: &mut Context<Self>) -> AnyElement {
        let view = self.session_view(session);
        let (chat_id, term_id) = (session.id.clone(), session.id.clone());
        ui::segmented()
            .flex_none()
            .child(ui::segment("view-chat", view == SessionView::Chat, "Chat").h(theme::sz(24.)).px_2p5().on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.set_session_view(&chat_id, SessionView::Chat, window, cx)
            })))
            .child(ui::segment("view-terminal", view == SessionView::Terminal, "Terminal").h(theme::sz(24.)).px_2p5().on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.set_session_view(&term_id, SessionView::Terminal, window, cx)
            })))
            .into_any_element()
    }

    fn render_empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let has_projects = !self.state.projects.is_empty();
        div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .child(
                backdrop::glass_strong(theme::sz(RADIUS))
                    .when(theme::is_light(), |d| d.shadow_md())
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_3()
                    .max_w(theme::sz(420.))
                    .p(theme::sz(24.))
                    .child(
                        div()
                            .text_size(theme::sz(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(if has_projects { "No open sessions" } else { "Welcome to Glasscode" }),
                    )
                    .child(div().text_color(theme::card_soft_text()).child(if has_projects {
                        "Start a session on any project. New sessions open Claude with your default account."
                    } else {
                        "Add a git repository. Each session gets its own worktree, agent and account."
                    }))
                    .child(if has_projects {
                        ui::primary_button("empty-new", "New session").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_session(None, None, window, cx)))
                    } else {
                        ui::primary_button("empty-add", "Add project…").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.prompt_add_project(window, cx)))
                    }),
            )
            .into_any_element()
    }

    /// The session's column: its header, then the chat, the terminal, or the way to resume it.
    fn render_session(&mut self, session: Session, fit: CentreFit, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut top = vec![self.render_session_header(&session, fit, cx)];
        if session.lineage.len() > 1 {
            top.push(
                div()
                    .px(theme::sz(14.))
                    .py(theme::sz(6.))
                    .border_b_1()
                    .border_color(theme::border())
                    .text_size(theme::sz(11.5))
                    .text_color(theme::card_soft_text())
                    .child(format!("Session history: {}", session.lineage.join("  →  ")))
                    .into_any_element(),
            );
        }
        let column = || backdrop::glass_strong(theme::sz(RADIUS)).when(theme::is_light(), |d| d.shadow_md()).flex().flex_col().flex_1().min_h_0();
        if !self.terminals.contains_key(&session.id) && session.worktree.is_dir() {
            // A restored session that has not been resumed yet.
            let id = session.id.clone();
            return column()
                .children(top)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .p(theme::sz(16.))
                        .child(div().text_size(theme::sz(16.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(session.title.clone()))
                        .child(div().text_color(theme::card_soft_text()).child(match &session.agent_session_id {
                            Some(conversation) => format!(
                                "Saved {} conversation {} — resume it with the same account.",
                                session.agent.name(),
                                &conversation[..8.min(conversation.len())]
                            ),
                            None => format!("{} session in {}", session.agent.name(), session.branch),
                        }))
                        .child(ui::primary_button("resume", "Resume session").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.restart_session(&id, window, cx)))),
                )
                .into_any_element();
        }
        let terminal = self.terminals.get(&session.id).cloned();
        let exited = terminal
            .as_ref()
            .and_then(|t| t.read(cx).exit_code)
            .map(|code| code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()));
        let ended = exited.map(|code| {
            let (restart_id, close_id) = (session.id.clone(), session.id.clone());
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_3()
                .px(theme::sz(14.))
                .py(theme::sz(8.))
                .border_t_1()
                .border_color(theme::border())
                .child(div().flex_1().min_w(theme::sz(120.)).text_color(theme::card_soft_text()).child(format!("{} exited ({code}).", session.agent.name())))
                .child(ui::primary_button("restart", "Resume").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.restart_session(&restart_id, window, cx))))
                .child(ui::button("close-ended", "Close tab").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.close_session(&close_id, window, cx))))
                .into_any_element()
        });
        // Claude and Codex read as a chat, or show their terminal (the Chat and Terminal control).
        if self.session_view(&session) == SessionView::Chat {
            let top = div().flex().flex_col().flex_none().children(top).into_any_element();
            return self.render_chat(&session, top, ended, window, cx);
        }
        // The terminal keeps its own opaque ground, inside the column with a margin.
        let asks: Vec<AnyElement> = self.control.pending_for(&session.id).into_iter().map(|a| crate::today_ui::render_approval(a, true, cx)).collect();
        column()
            .children(top)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(theme::sz(80.))
                    .m(theme::sz(8.))
                    .p(theme::sz(6.))
                    .rounded(theme::sz(10.))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::c(theme::term().background))
                    .child(div().flex_1().min_h_0().when_some(terminal, |d, t| d.child(t))),
            )
            // In the Terminal view, what the agent asks Glasscode to do shows under it, in at most
            // half the column: in a small window the terminal keeps its room.
            .when(!asks.is_empty(), |d| {
                d.child(div().id("terminal-asks").flex().flex_none().flex_col().gap_2().max_h(gpui::relative(0.5)).overflow_y_scroll().px(theme::sz(8.)).pb(theme::sz(8.)).children(asks))
            })
            .children(ended)
            .into_any_element()
    }

    /// The column's short header: the agent, the project and branch, the account (its menu),
    /// the state, the ticket and review tags; then Browser, Hand off and "⋯" at the right end of
    /// the line they land on. It is one row that wraps, so nothing in it is cut at any width.
    fn render_session_header(&self, session: &Session, fit: CentreFit, cx: &mut Context<Self>) -> AnyElement {
        let project = self.state.project(&session.project_id).map(|p| p.name.clone()).unwrap_or_default();
        let account = session.account_id.as_deref().and_then(|id| self.state.account(id)).cloned();
        let menu = (self.menu_open && account.is_some()).then(|| self.render_account_menu(session, cx));
        let status = self.activity.get(&session.id).cloned();
        let other = session.agent.other();
        // In the narrowest centre the "⋯" menu switches between the chat and the terminal.
        let switch_to = (fit.view == ViewControl::Menu && session.agent != AgentKind::Shell).then(|| match self.session_view(session) {
            SessionView::Chat => SessionView::Terminal,
            SessionView::Terminal => SessionView::Chat,
        });
        // The project and branch wrap rather than end in "…" when the line is short.
        let place = div()
            .flex()
            .min_w_0()
            .items_center()
            .gap(theme::sz(8.))
            .child(icons::brand(icons::agent_brand(session.agent), 16. * theme::scale()))
            .child(div().min_w_0().font_family(theme::MONO_FONT).text_size(theme::sz(12.5)).text_color(theme::text()).child(format!("{project} / {}", session.branch)));
        let control = (fit.view == ViewControl::Header && session.agent != AgentKind::Shell).then(|| self.view_control(session, cx));
        let actions = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(theme::sz(if fit.words { 4. } else { 2. }))
            // The session's tab in the Glasscode Browser, brought to the front for the person.
            .when(self.browser.session_tabs.contains_key(&session.id), |d| {
                let id = session.id.clone();
                d.child(header_button("session-browser", icons::OPEN_OUTSIDE, "Browser", fit.words).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.show_session_tab(&id, cx))))
            })
            .child(header_button("handoff", icons::HANDOFF, "Hand off…", fit.words).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_handoff(Some(other), None, window, cx))))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(header_button("session-more", icons::MORE, "More", false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.session_more = !this.session_more;
                        cx.notify();
                    }))),
            );
        let more = self.session_more.then(|| self.render_session_more(session, switch_to, fit.words, cx));
        div()
            .relative()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap_x(theme::sz(if fit.words { 10. } else { 6. }))
            .gap_y(theme::sz(6.))
            .px(theme::sz(if fit.words { 14. } else { 10. }))
            .py(theme::sz(8.))
            .border_b_1()
            .border_color(theme::border())
            .child(place)
            .when_some(account, |d, account| {
                d.child(
                    div()
                        .relative()
                        .flex_none()
                        .child(
                            div()
                                .id("account-menu")
                                .flex()
                                .items_center()
                                .gap(theme::sz(4.))
                                .max_w(theme::sz(220.))
                                .h(theme::sz(24.))
                                .px(theme::sz(if fit.words { 6. } else { 4. }))
                                .rounded(theme::sz(6.))
                                .text_size(theme::sz(12.5))
                                .text_color(theme::text_soft())
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::tint()).text_color(theme::text()))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.menu_open = !this.menu_open;
                                    cx.notify();
                                }))
                                // The plan only where the buttons have their words too.
                                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(if fit.words { account.display() } else { account.label.clone() }))
                                .child(icons::icon(icons::CHEVRON_DOWN, theme::sz(11.), theme::sz(11.), theme::card_soft_text())),
                        ),
                )
            })
            // A narrow header leaves the state to the dot of the tab right above it.
            .when_some(status.filter(|_| fit.words), |d, status| {
                let (color, word) = match status {
                    AgentActivity::Working => (theme::added(), "working"),
                    AgentActivity::NeedsInput => (theme::attention(), "needs your input"),
                    AgentActivity::Idle => (theme::border_strong(), "idle"),
                };
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(theme::sz(6.))
                        .whitespace_nowrap()
                        .text_size(theme::sz(12.))
                        .text_color(theme::card_soft_text())
                        .child(ui::dot(color))
                        .child(word),
                )
            })
            // The Linear ticket this session works on: opens it on the Board.
            .when_some(self.ticket_for_session(session).map(|t| (t.identifier.clone(), t.status.name.clone())), |d, (id, status)| {
                let key = crate::tickets_ui::Key::Ticket(id.clone());
                d.child(
                    div()
                        .id("session-ticket")
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1p5()
                        .h(theme::sz(24.))
                        .px_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::border_strong())
                        .text_size(theme::sz(12.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::tint()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_ticket(key.clone(), cx)))
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(id))
                        .child(div().text_color(theme::card_soft_text()).child(status)),
                )
            })
            .when(session.review.is_some(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .px_1p5()
                        .py_0p5()
                        .rounded(theme::sz(5.))
                        .bg(theme::tag_bg())
                        .text_size(theme::sz(11.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text_soft())
                        .child("Review"),
                )
            })
            .children(control)
            .child(actions.ml_auto())
            // The menus hang from the header and keep inside its width: in a narrow centre one
            // that reached over the left sidebar did not get the clicks on that part.
            .when_some(menu, |d, menu| d.child(menu))
            .children(more)
            .into_any_element()
    }

    /// The header's ⋯ menu: what is done with a session less often.
    fn render_session_more(&self, session: &Session, switch_to: Option<SessionView>, words: bool, cx: &mut Context<Self>) -> AnyElement {
        // One line each: the menu is no wider than the header, which is narrow in a small window.
        let item = |id: &'static str, label: String| {
            div()
                .id(id)
                .px_2p5()
                .py_2()
                .rounded_md()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(theme::sz(13.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(label)
        };
        let (move_id, close_id, view_id) = (session.id.clone(), session.id.clone(), session.id.clone());
        let conversation = session.agent_session_id.clone();
        deferred(
            div()
                .id("session-more-menu")
                .occlude()
                .absolute()
                .top(gpui::relative(1.))
                .right_0()
                .w(theme::sz(240.))
                .max_w(gpui::relative(1.))
                .p_1()
                .flex()
                .flex_col()
                .rounded_lg()
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::dialog())
                .shadow_lg()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.session_more = false;
                    cx.notify();
                }))
                .when_some(switch_to, |d, view| {
                    let label = if view == SessionView::Terminal { "Show the terminal" } else { "Show the chat" };
                    d.child(item("more-view", label.to_string()).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.session_more = false;
                        this.set_session_view(&view_id, view, window, cx)
                    })))
                })
                .when(self.server.connected(), |d| {
                    d.child(item("more-move", format!("⇡ Move to {}", self.server.name())).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.session_more = false;
                        this.move_one(crate::switch_ui::Direction::ToServer, move_id.clone(), cx)
                    })))
                })
                .child(item("more-editor", format!("Open in {}", self.preferred_editor().map(|(name, _)| name).unwrap_or(crate::platform::file_manager()))).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.session_more = false;
                    this.open_in_editor(None, cx)
                })))
                .when_some(conversation, |d, id| {
                    let label = if words { format!("Copy conversation id ({})", &id[..8.min(id.len())]) } else { "Copy conversation id".to_string() };
                    d.child(item("more-copy-id", label).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.session_more = false;
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(id.clone()));
                        this.show_toast("Conversation id copied".to_string(), cx);
                    })))
                })
                .child(div().h(px(1.)).my_1().bg(theme::border()))
                .child(item("more-close", "Close session…".to_string()).text_color(theme::removed()).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.session_more = false;
                    this.close_session(&close_id, window, cx)
                }))),
        )
        .with_priority(5)
        .into_any_element()
    }

    fn render_account_menu(&self, session: &Session, cx: &mut Context<Self>) -> AnyElement {
        let mut items: Vec<AnyElement> = Vec::new();
        for agent in [AgentKind::Claude, AgentKind::Codex] {
            items.push(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .child(ui::caps_label(format!("{} ACCOUNTS", agent.name().to_uppercase())))
                    .into_any_element(),
            );
            let accounts: Vec<_> = self.state.accounts_for(agent).cloned().collect();
            if accounts.is_empty() {
                items.push(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(theme::sz(12.))
                        .text_color(theme::faint())
                        .child("None yet")
                        .into_any_element(),
                );
            }
            for account in accounts {
                let current = session.account_id.as_deref() == Some(account.id.as_str());
                let same_agent = agent == session.agent;
                let hint = if current {
                    "Current"
                } else if same_agent {
                    "Switch"
                } else {
                    "Hand off →"
                };
                let account_id = account.id.clone();
                items.push(
                    div()
                        .id(SharedString::from(format!("menu-{}", account.id)))
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(theme::sz(32.))
                        .px_2()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.menu_open = false;
                            if current {
                                cx.notify();
                            } else if same_agent {
                                this.switch_account(&account_id, window, cx);
                            } else {
                                this.open_handoff(Some(agent), Some(account_id.clone()), window, cx);
                            }
                        }))
                        .child(ui::dot(theme::agent_color(agent)))
                        .child(account.label.clone())
                        .child(
                            div()
                                .text_size(theme::sz(11.))
                                .text_color(theme::muted())
                                .child(account.plan.clone().unwrap_or_default()),
                        )
                        .child(
                            div()
                                .ml_auto()
                                .text_size(theme::sz(11.))
                                .text_color(if current { theme::accent() } else { theme::muted() })
                                .child(hint),
                        )
                        .into_any_element(),
                );
            }
        }
        items.push(div().h(px(1.)).my_1p5().bg(theme::border_strong()).into_any_element());
        items.push(
            div()
                .id("menu-manage")
                .flex()
                .items_center()
                .h(theme::sz(32.))
                .px_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .text_color(theme::text_soft())
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_accounts(window, cx)))
                .child("Manage accounts…")
                .into_any_element(),
        );
        deferred(
            div()
                .id("account-menu-popover")
                .occlude()
                .absolute()
                .top(gpui::relative(1.))
                .left_0()
                .w(theme::sz(320.))
                .max_w(gpui::relative(1.))
                .p_1p5()
                .rounded_lg()
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::subtle())
                .shadow_lg()
                .children(items),
        )
        .with_priority(2)
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session(id: &str, project: &str, worktree: &str) -> Session {
        Session {
            id: id.into(),
            project_id: project.into(),
            worktree: PathBuf::from(worktree),
            branch: "main".into(),
            title: format!("title {id}"),
            agent: AgentKind::Shell,
            account_id: None,
            agent_session_id: None,
            transcript_path: None,
            lineage: Vec::new(),
            initial_prompt: None,
            team_task: None,
            review: None,
            model: None,
            mode: None,
            created_at: 0,
        }
    }

    fn tab(id: &str) -> CenterTab {
        CenterTab::Session(id.into())
    }

    fn file(path: &str) -> CenterTab {
        CenterTab::File(PathBuf::from(path))
    }

    /// Sessions in other worktrees and other projects get no tab, also those that share a folder
    /// name with this worktree in another project.
    #[test]
    fn a_worktree_with_one_session_has_one_tab() {
        let sessions = [session("a", "p", "/w/app"), session("b", "p", "/w/app-login"), session("c", "q", "/w/app")];
        assert_eq!(tab_list(&sessions, &sessions[0], &[]), vec![tab("a")]);
        assert_eq!(front_tab(&tab_list(&sessions, &sessions[0], &[]), None, "a"), 0);
    }

    #[test]
    fn a_worktree_with_four_sessions_has_four_tabs_in_order() {
        let sessions = [
            session("a", "p", "/w/app"),
            session("x", "p", "/w/other"),
            session("b", "p", "/w/app"),
            session("c", "p", "/w/app"),
            session("d", "p", "/w/app"),
        ];
        let tabs = tab_list(&sessions, &sessions[3], &[]);
        assert_eq!(tabs, vec![tab("a"), tab("b"), tab("c"), tab("d")]);
        assert_eq!(front_tab(&tabs, None, "c"), 2);
        // A session that closed elsewhere leaves the strip the next time it is made.
        let left = [sessions[0].clone(), sessions[2].clone(), sessions[4].clone()];
        assert_eq!(tab_list(&left, &left[0], &tabs), vec![tab("a"), tab("b"), tab("d")]);
    }

    /// File tabs (U13) stay after the session tabs, and stay in front while open.
    #[test]
    fn file_tabs_are_kept_after_the_sessions() {
        let sessions = [session("a", "p", "/w/app"), session("b", "p", "/w/app")];
        let open = [tab("old"), file("/w/app/src/main.rs"), file("/w/app/README.md")];
        let tabs = tab_list(&sessions, &sessions[1], &open);
        assert_eq!(tabs, vec![tab("a"), tab("b"), file("/w/app/src/main.rs"), file("/w/app/README.md")]);
        assert_eq!(front_tab(&tabs, Some(&file("/w/app/README.md")), "b"), 3, "the file stays in front");
        assert_eq!(front_tab(&tabs, Some(&file("/w/app/gone.rs")), "b"), 1, "a closed file gives way to the session");
        assert_eq!(front_tab(&tabs, Some(&tab("a")), "b"), 1, "a session tab follows the active session");
    }

    #[test]
    fn closing_a_tab_brings_its_neighbour_to_the_front() {
        let tabs = vec![tab("a"), tab("b"), tab("c"), file("/w/app/x.rs")];
        assert_eq!(neighbour(&tabs, 1), Some(&tab("c")), "the next one");
        assert_eq!(neighbour(&tabs, 0), Some(&tab("b")));
        assert_eq!(neighbour(&tabs, 2), Some(&file("/w/app/x.rs")), "a file tab is a neighbour too");
        assert_eq!(neighbour(&tabs, 3), Some(&tab("c")), "the last one: the one before");
        assert_eq!(neighbour(&[tab("a")], 0), None, "the only tab");
        assert_eq!(neighbour(&tabs, 9), None);
        // Between file tabs: the next file, else the one before.
        let tabs = vec![tab("a"), file("/w/app/x.rs"), file("/w/app/y.rs"), file("/w/app/z.rs")];
        assert_eq!(neighbour(&tabs, 2), Some(&file("/w/app/z.rs")));
        assert_eq!(neighbour(&tabs, 3), Some(&file("/w/app/y.rs")));
    }

    /// The tab model with the files of `file_view`: the files of each worktree in its own strip;
    /// the same file again selects its tab; closing the file in front brings its neighbour.
    #[test]
    fn file_tabs_open_select_and_close_in_their_worktree() {
        use crate::file_view::FileTabs;
        let sessions = [session("a", "p", "/w/app"), session("b", "p", "/w/app"), session("c", "p", "/w/app-login")];
        let (app, login) = (std::path::Path::new("/w/app"), std::path::Path::new("/w/app-login"));
        let strip = |files: &mut FileTabs, active: &Session| {
            let open: Vec<CenterTab> = files.paths_in(&active.worktree).into_iter().map(CenterTab::File).collect();
            let tabs = tab_list(&sessions, active, &open);
            let front = files.front(&active.id, None).map(CenterTab::File);
            let index = front_tab(&tabs, front.as_ref(), &active.id);
            (tabs, index)
        };
        let mut files = FileTabs::default();
        let (main_rs, _) = files.open(app, "src/main.rs");
        let (readme, _) = files.open(app, "README.md");
        files.open(login, "src/login.rs");
        files.bring_to_front(readme.clone(), "b");
        let (tabs, index) = strip(&mut files, &sessions[1]);
        assert_eq!(tabs, vec![tab("a"), tab("b"), CenterTab::File(main_rs.clone()), CenterTab::File(readme.clone())]);
        assert_eq!(index, 3, "the file opened last is in front");
        // The same file again: no second tab, and it comes to the front.
        assert_eq!(files.open(app, "src/main.rs"), (main_rs.clone(), false));
        files.bring_to_front(main_rs.clone(), "b");
        assert_eq!(strip(&mut files, &sessions[1]).1, 2);
        // The file in front closes: its neighbour (the one before, as it was last) comes forward.
        files.bring_to_front(readme.clone(), "b");
        let (tabs, _) = strip(&mut files, &sessions[1]);
        let next = neighbour(&tabs, 3).cloned();
        assert_eq!(next, Some(CenterTab::File(main_rs.clone())));
        files.close(&readme);
        files.bring_to_front(main_rs.clone(), "b");
        let (tabs, index) = strip(&mut files, &sessions[1]);
        assert_eq!((tabs.len(), index), (3, 2));
        // Another worktree's strip has only its own file; back in this worktree, the files are
        // there, and the session that is on screen is in front.
        let (tabs, index) = strip(&mut files, &sessions[2]);
        assert_eq!(tabs, vec![tab("c"), file("/w/app-login/src/login.rs")]);
        assert_eq!(index, 0);
        let (tabs, index) = strip(&mut files, &sessions[1]);
        assert_eq!(tabs, vec![tab("a"), tab("b"), CenterTab::File(main_rs)]);
        assert_eq!(index, 1, "the session on screen, not the file of before");
    }

    /// The centre at the narrowest window (900 px) and at a wide one, at 100 % and 130 %, with
    /// both sidebars at the widths `shell::fit_panels_at` gives them.
    #[test]
    fn the_strip_and_the_header_fit_the_centre() {
        let centre = |window: f32, scale: f32| {
            let (left, right) = crate::shell::fit_panels_at(window, scale, Some((300., 220.)), Some((420., 300.)));
            (window - left.unwrap() - right.unwrap()) / scale
        };
        let fit = |view, words| CentreFit { view, words };
        // 900 px at 130 %: 224 px on the screen. The control goes into the "⋯" menu and the
        // buttons are icons.
        assert_eq!(centre_fit(centre(900., 1.3)), fit(ViewControl::Menu, false));
        assert_eq!(centre_fit(centre(900., 1.)), fit(ViewControl::Header, false));
        assert_eq!(centre_fit(centre(1280., 1.)), fit(ViewControl::Strip, true));
        assert_eq!(centre_fit(centre(1440., 1.3)), fit(ViewControl::Strip, false));
        assert_eq!(centre_fit(centre(1920., 1.)), fit(ViewControl::Strip, true));
        // A line of the header for the control: an account's name such as "Personal" with its
        // chevron (about 81), the state's dot and the control (about 128), with the header's
        // padding and gaps.
        assert!(HEADER_ROOM - 2. * 10. >= 81. + 10. + 7. + 10. + 128.);
        // Three tabs at their narrowest, "+" (28), the control (about 128) and the gaps fit.
        assert!(3. * TAB_MIN + 2. * 4. + 4. + 28. + 8. + 128. + 16. <= STRIP_ROOM);
    }
}
