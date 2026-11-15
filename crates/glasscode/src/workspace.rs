//! The Glasscode window: projects sidebar, session tabs with terminals, and the Git panel.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, Focusable, FontWeight,
    KeyBinding, MouseButton, PathPromptOptions, SharedString, Subscription, Window, actions, deferred, div,
    prelude::*, px,
};
use glasscode_core::agents::{self, AgentActivity, EventsTail};
use glasscode_core::git::{self, FileChange, Worktree};
use glasscode_core::model::{AgentKind, Session, StartupMode, State, new_id, now};
use glasscode_core::{store, versions};

use crate::dialogs::Modal;
use crate::terminal::{SpawnSpec, TerminalEvent, TerminalView};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui;

actions!(
    glasscode,
    [
        NewSession,
        CloseSession,
        NextSession,
        PreviousSession,
        OpenAccounts,
        AddProject,
        Handoff,
        ToggleRightPanel,
        OpenPalette,
        PaletteUp,
        PaletteDown,
        Quit
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-t", NewSession, None),
        KeyBinding::new("cmd-n", NewSession, None),
        KeyBinding::new("cmd-w", CloseSession, None),
        KeyBinding::new("ctrl-tab", NextSession, None),
        KeyBinding::new("cmd-shift-]", NextSession, None),
        KeyBinding::new("ctrl-shift-tab", PreviousSession, None),
        KeyBinding::new("cmd-shift-[", PreviousSession, None),
        KeyBinding::new("cmd-,", OpenAccounts, None),
        KeyBinding::new("cmd-o", AddProject, None),
        KeyBinding::new("cmd-shift-h", Handoff, None),
        KeyBinding::new("cmd-b", ToggleRightPanel, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-k", OpenPalette, None),
        KeyBinding::new("up", PaletteUp, Some("Palette")),
        KeyBinding::new("down", PaletteDown, Some("Palette")),
    ]);
}

/// Which sidebar edge is being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    /// The Tickets view's reading panel.
    Tickets,
}

pub const SIDEBAR_WIDTH: f32 = 300.;
pub const RIGHT_WIDTH: f32 = 420.;

#[derive(Clone, Copy, PartialEq)]
pub enum RightTab {
    /// The session's pull request: CI, reviews, preview and comments.
    PullRequest,
    /// The worktree's changed files and the commit box.
    Changes,
    /// The worktree as a tree, with a file's preview or diff.
    Files,
    /// Glasscode, the voice copilot: the moon character and the sessions she watches.
    Glasscode,
}

pub struct DiffView {
    pub path: String,
    pub lines: Vec<(char, String)>,
}

pub struct Workspace {
    pub state: State,
    pub env: Arc<HashMap<String, String>>,
    pub terminals: HashMap<String, Entity<TerminalView>>,
    tails: HashMap<String, EventsTail>,
    launched_at: HashMap<String, SystemTime>,
    pub activity: HashMap<String, AgentActivity>,
    pub attention: HashSet<String>,
    pub worktrees: HashMap<String, Vec<Worktree>>,
    pub changes: Vec<FileChange>,
    pub changes_for: Option<PathBuf>,
    pub diff: Option<DiffView>,
    pub right_tab: RightTab,
    pub show_right: bool,
    pub commit_input: Entity<TextInput>,
    pub modal: Option<Modal>,
    pub row_menu: Option<crate::worktree_dialog::RowMenu>,
    pub menu_open: bool,
    pub toast: Option<(String, Instant)>,
    pub cli_status: Vec<versions::CliStatus>,
    pub glasscode: crate::glasscode_ui::GlasscodeState,
    pub prs: crate::pr_status::PrStatuses,
    pub center: crate::center::Center,
    pub server: crate::server_ui::ServerUi,
    pub update: crate::update_ui::UpdateUi,
    pub mirror: crate::mirror_ui::MirrorUi,
    pub browser: crate::browser_ui::BrowserUi,
    pub switch: Option<crate::switch_ui::SwitchForm>,
    pub gh: crate::github_ui::GithubUi,
    pub github: crate::github_accounts_ui::GithubAccounts,
    pub ci_fix: crate::ci_fix::CiFix,
    pub auto_review: crate::auto_review::AutoReview,
    /// Worktrees being removed in the background (their sidebar rows say so).
    pub removing: HashSet<PathBuf>,
    /// Projects whose worktrees without a session are unfolded in the sidebar.
    pub idle_worktrees_open: HashSet<String>,
    /// The session last opened in each worktree (its row opens that one).
    pub last_in_worktree: HashMap<PathBuf, String>,
    /// What each session is about, from its transcript (to name it by).
    pub auto_titles: HashMap<String, String>,
    pub(crate) titles_checked: Instant,
    pub(crate) titles_running: bool,
    /// Worktrees whose placeholder branch was found pushed (never renamed).
    pub(crate) rename_skip: HashSet<PathBuf>,
    /// The window is in front (no notification for the session on screen).
    pub window_active: bool,
    /// Notifications on screen, by session.
    pub popups: Vec<(String, gpui::AnyWindowHandle)>,
    pub main_window: Option<gpui::AnyWindowHandle>,
    /// A sidebar edge being dragged: which, where the mouse started, its width then.
    pub(crate) resizing: Option<(Side, f32, f32)>,
    pub pairing: Option<crate::pair_ui::Pairing>,
    pub phones: Option<Vec<glasscode_core::remote::PhoneKey>>,
    pub tickets: crate::tickets_ui::TicketsUi,
    pub settings: crate::settings_ui::SettingsUi,
    /// ⌘W on a working session asked first: (its id, when).
    pub(crate) close_asked: Option<(String, Instant)>,
    /// The command palette, while it is open.
    pub palette: Option<crate::palette_ui::Palette>,
    pub usage: crate::usage_ui::UsageUi,
    pub themes: crate::themes_ui::ThemesUi,
    /// Sessions shown as a chat (the Claude app's look) over their terminal.
    pub chat: crate::chat_ui::ChatUi,
    /// The session header's ⋯ menu is open.
    pub session_more: bool,
    /// The `glasscode` command: its socket, waiting requests, what agents did.
    pub control: crate::control_ui::ControlUi,
    /// The Today board.
    pub today: crate::today_ui::TodayUi,
    pub(crate) focus: FocusHandle,
    last_git_refresh: Instant,
    last_worktree_refresh: Instant,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Workspace {
    pub fn new(env: HashMap<String, String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _ = glasscode_core::paths::ensure_dirs();
        let mut state = store::load();
        state.bootstrap_accounts(true);
        let commit_input = cx.new(|cx| TextInput::new("Commit message", cx));
        let subscription = cx.subscribe(&commit_input, |this: &mut Self, _, event, cx| {
            if matches!(event, InputEvent::Submit) {
                this.commit(cx);
            }
        });
        let env = Arc::new(env);
        let cli_env = env.clone();
        let mut this = Self {
            state,
            env,
            terminals: HashMap::new(),
            tails: HashMap::new(),
            launched_at: HashMap::new(),
            activity: HashMap::new(),
            attention: HashSet::new(),
            worktrees: HashMap::new(),
            changes: Vec::new(),
            changes_for: None,
            diff: None,
            right_tab: RightTab::PullRequest,
            show_right: true,
            commit_input,
            modal: None,
            row_menu: None,
            menu_open: false,
            toast: None,
            cli_status: Vec::new(),
            glasscode: crate::glasscode_ui::GlasscodeState::new(),
            prs: Default::default(),
            center: crate::center::Center::Session,
            server: crate::server_ui::ServerUi::new(),
            update: crate::update_ui::UpdateUi::new(),
            mirror: crate::mirror_ui::MirrorUi::new(),
            browser: crate::browser_ui::BrowserUi::new(),
            switch: None,
            gh: Default::default(),
            github: Default::default(),
            ci_fix: crate::ci_fix::CiFix::load(),
            auto_review: crate::auto_review::AutoReview::load(),
            removing: HashSet::new(),
            idle_worktrees_open: HashSet::new(),
            last_in_worktree: HashMap::new(),
            auto_titles: HashMap::new(),
            titles_checked: Instant::now() - Duration::from_secs(60),
            titles_running: false,
            rename_skip: HashSet::new(),
            window_active: window.is_window_active(),
            popups: Vec::new(),
            main_window: Some(window.window_handle()),
            resizing: None,
            pairing: None,
            phones: None,
            tickets: crate::tickets_ui::TicketsUi::new(cx),
            settings: Default::default(),
            palette: None,
            close_asked: None,
            usage: Default::default(),
            themes: Default::default(),
            chat: Default::default(),
            session_more: false,
            control: crate::control_ui::ControlUi::new(),
            today: crate::today_ui::TodayUi::new(cx),
            focus: cx.focus_handle(),
            last_git_refresh: Instant::now() - Duration::from_secs(60),
            last_worktree_refresh: Instant::now() - Duration::from_secs(60),
            _subscriptions: vec![subscription],
        };

        // Restore: the active session comes back right away; others resume when opened.
        if this.state.active_session.as_ref().and_then(|id| this.state.session(id)).is_none() {
            this.state.active_session = this.state.sessions.first().map(|s| s.id.clone());
        }
        // With a server set up, an empty Mac usually means the work moved there (Server mode).
        let wants_new_claude = this.state.startup == StartupMode::RestoreAndNewClaude
            || (this.state.sessions.is_empty() && this.server.config.is_none());
        if wants_new_claude {
            let project = this
                .active_session()
                .map(|s| s.project_id.clone())
                .or_else(|| this.state.projects.first().map(|p| p.id.clone()));
            if let Some(project_id) = project {
                this.start_default_claude(&project_id, window, cx);
            }
        }
        cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = window.is_window_active();
            if this.window_active {
                this.today.activated_at = Some(Instant::now());
                if let Some(id) = this.state.active_session.clone() {
                    this.dismiss_popups_for(&id, cx);
                }
            }
        })
        .detach();
        // Sessions start as their project's GitHub account (from the first one on).
        this.configure_github(cx);
        this.github_accounts_refresh(cx);
        if let Some(id) = this.state.active_session.clone() {
            this.ensure_terminal(&id, window, cx);
            this.focus_active(window, cx);
        }
        this.save();
        this.server_start(cx);
        this.update_start();
        this.control_start(window, cx);
        // However the app quits (menu, Dock, last window), a waiting update goes in place.
        cx.on_app_quit(|this: &mut Workspace, _| {
            this.update_on_quit();
            async {}
        })
        .detach();

        // The Chat view on screen follows its session file closely.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(350)).await;
                if this
                    .update(cx, |this, cx| {
                        this.control_scan();
                        this.chat_tick(cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // Background: hook events, Codex id discovery, git and worktree refresh.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(700)).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Fast mirror poll: the glasses post every navigation instantly, so poll the voice
        // state ~4×/s (independent of the 700ms tick) to keep the Glasscode mirror snappy.
        // `glasscode_tick` no-ops when the Glasscode tab is closed and Glasscode isn't live, so it's cheap.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                if this.update(cx, |this, cx| this.glasscode_tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Developer aid for screenshots and UI tests: open a dialog at startup.
        let debug_open = std::env::var("GLASSCODE_DEBUG_OPEN").ok().filter(|_| cfg!(debug_assertions));
        match debug_open.as_deref() {
            Some("new") => this.open_new_session(None, None, window, cx),
            Some("accounts") => this.open_accounts(window, cx),
            Some("browser") => this.open_browser_dialog(cx),
            Some("handoff") => this.open_handoff(None, None, window, cx),
            Some("handoff-go") => {
                this.open_handoff(None, None, window, cx);
                this.perform_handoff(window, cx);
            }
            Some("menu") => this.menu_open = true,
            Some("project-menu") => {
                if let Some(id) = this.state.projects.first().map(|p| p.id.clone()) {
                    this.open_row_menu(id, None, gpui::point(px(240.), px(120.)), cx);
                }
            }
            Some("palette") => this.open_palette(window, cx),
            Some("usage") => this.show_view(crate::tickets_ui::View::Usage, cx),
            Some("today") => this.show_today(cx),
            Some("themes") => this.show_view(crate::tickets_ui::View::Themes, cx),
            Some(mode) if mode.starts_with("settings") => {
                let section = match mode.strip_prefix("settings:").unwrap_or("") {
                    "accounts" => crate::settings_ui::Section::Accounts,
                    "projects" => crate::settings_ui::Section::Projects,
                    "automation" => crate::settings_ui::Section::Automation,
                    "browser" => crate::settings_ui::Section::Browser,
                    "server" => crate::settings_ui::Section::Server,
                    "updates" => crate::settings_ui::Section::Updates,
                    "shortcuts" => crate::settings_ui::Section::Shortcuts,
                    _ => crate::settings_ui::Section::General,
                };
                this.open_settings(section, window, cx);
            }
            Some("linear") => {
                this.show_tickets(cx);
                this.open_linear_settings(window, cx);
            }
            Some(mode) if mode.starts_with("tickets") => {
                // tickets[-list|-expand|-start|-done|-review]:<ENG-787 | owner/name#784>
                let (how, key) = mode.split_once(':').unwrap_or((mode, ""));
                let key = match key.rsplit_once('#') {
                    Some((repo, n)) => n.parse().ok().map(|n| crate::tickets_ui::Key::Pr(repo.to_string(), n)),
                    None => (!key.is_empty()).then(|| crate::tickets_ui::Key::Ticket(key.to_string())),
                };
                match how {
                    "tickets-list" => this.tickets.layout = crate::tickets_ui::Layout::List,
                    "tickets-expand" => this.tickets.expanded = true,
                    "tickets-done" => this.tickets.done_open = true,
                    "tickets-review" => this.tickets.scope = crate::tickets_ui::Scope::Review,
                    _ => {}
                }
                match &key {
                    Some(key) => this.open_ticket(key.clone(), cx),
                    None => this.show_tickets(cx),
                }
                if let Some(key) = key.filter(|_| how == "tickets-start" || how == "tickets-go" || how == "tickets-comment") {
                    // Once the board is in: open the start dialog (and start), or post a comment.
                    let go = how == "tickets-go";
                    let comment = how == "tickets-comment";
                    cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor().timer(Duration::from_secs(3)).await;
                        let _ = this.update_in(cx, |this, window, cx| {
                            if comment {
                                this.tickets.comment.update(cx, |c, cx| c.set_text("Tested on a Workspace site with a visitor account: the button is hidden.", cx));
                                this.ticket_comment(cx);
                            } else {
                                this.open_start_ticket(key, None, window, cx);
                            }
                        });
                        if go {
                            cx.background_executor().timer(Duration::from_secs(2)).await;
                            let _ = this.update_in(cx, |this, window, cx| this.start_from_ticket(window, cx));
                        }
                    })
                    .detach();
                }
            }
            Some(mode) if mode.starts_with("gh-expand") => this.gh.expand = Some(mode.strip_prefix("gh-expand:").unwrap_or("all").to_string()),
            Some(mode @ ("switch-to-server" | "switch-to-laptop")) => {
                // Wait for the server connection, then run the switch.
                let to_server = mode == "switch-to-server";
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(4)).await;
                    let _ = this.update(cx, |this, cx| {
                        let direction = if to_server { crate::switch_ui::Direction::ToServer } else { crate::switch_ui::Direction::ToLaptop };
                        this.open_switch(direction, cx);
                        if let Some(form) = this.switch.as_mut() {
                            form.running = true;
                        }
                    });
                })
                .detach();
            }
            Some("pair") => {
                // Wait for the server connection, then open the Server dialog with a pairing code.
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(4)).await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.open_server_dialog(window, cx);
                        this.start_pairing(cx);
                    });
                })
                .detach();
            }
            Some(mode @ ("bring-first" | "move-first")) => {
                // Move one session by itself (the session headers' Move buttons), once connected.
                let to_mac = mode == "bring-first";
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(6)).await;
                    let _ = this.update_in(cx, |this, _, cx| {
                        let (direction, id) = if to_mac {
                            (crate::switch_ui::Direction::ToLaptop, this.server.state.as_ref().and_then(|s| s.sessions.first().map(|r| r.session.id.clone())))
                        } else {
                            (crate::switch_ui::Direction::ToServer, this.state.sessions.first().map(|s| s.id.clone()))
                        };
                        if let Some(id) = id {
                            this.move_one(direction, id, cx);
                        }
                    });
                })
                .detach();
            }
            Some(mode @ ("server" | "server-new" | "server-session")) => {
                // Wait for the server connection, then open the dialog or start a session.
                let mode = mode.to_string();
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(4)).await;
                    let _ = this.update_in(cx, |this, window, cx| match mode.as_str() {
                        "server" => this.open_server_dialog(window, cx),
                        "server-new" => this.open_new_server_session(None, None, window, cx),
                        _ => {
                            let project = this.server.state.as_ref().and_then(|s| s.projects.first().map(|p| p.id.clone()));
                            match this.server.state.as_ref().and_then(|s| s.sessions.first().map(|r| r.session.id.clone())) {
                                Some(id) => this.open_server_session(&id, window, cx),
                                None => {
                                    if let Some(project_id) = project {
                                        let create = glasscode_core::remote::CreateSession {
                                            project_id,
                                            worktree: None,
                                            new_branch: Some("feat/on-server".into()),
                                            agent: AgentKind::Claude,
                                            account_id: None,
                                            title: Some("Runs on the server".into()),
                                            resume: None,
                                            initial_prompt: None,
                                            start: None,
                                            review: None,
                                            review_files: Default::default(),
                                            model: None,
                                            mode: None,
                                        };
                                        this.create_server_session(create, window, cx);
                                    }
                                }
                            }
                        }
                    });
                })
                .detach();
            }
            Some(other) if other.starts_with("build-pr:") => {
                // "build-pr:<words>": New session → Build → A pull request, searching for <words>.
                let words = other.trim_start_matches("build-pr:").to_string();
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.open_new_session(None, None, window, cx);
                        if let Some(Modal::NewSession(form)) = this.modal.as_mut() {
                            form.from_pr = true;
                            form.pr_query.update(cx, |q, cx| q.set_text(words.clone(), cx));
                        }
                        this.pr_search_now(cx);
                    });
                })
                .detach();
            }
            Some(other) if other.starts_with("review-new") => {
                // "review-new[-server][:<number>]": the New session dialog in Review mode; with a
                // number it also picks that PR and starts the review.
                let on_server = other.starts_with("review-new-server");
                let number: Option<u64> = other.split(':').nth(1).and_then(|n| n.parse().ok());
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(if on_server { 4 } else { 1 })).await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        if on_server {
                            this.open_new_server_session(None, None, window, cx);
                        } else {
                            this.open_new_session(None, None, window, cx);
                        }
                        if let Some(Modal::NewSession(form)) = this.modal.as_mut() {
                            form.review_mode = true;
                        }
                        this.pr_search_now(cx);
                    });
                    let Some(number) = number else { return };
                    cx.background_executor().timer(Duration::from_secs(3)).await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        let hit = match this.modal.as_ref() {
                            Some(Modal::NewSession(form)) => form.pr_hits.as_ref().and_then(|h| h.iter().find(|p| p.number == number).cloned()),
                            _ => None,
                        };
                        this.pick_pr(hit, cx);
                        this.create_session_from_form(window, cx);
                    });
                })
                .detach();
            }
            Some(other) if other.starts_with("new-in:") => {
                // "new-in:<path end>:<agent>[:pick]": the New session dialog for that worktree.
                let parts: Vec<String> = other.trim_start_matches("new-in:").split(':').map(str::to_string).collect();
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(2)).await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        let Some(session) = this.state.sessions.iter().find(|s| s.worktree.to_string_lossy().ends_with(&parts[0])).cloned() else { return };
                        this.open_new_session(Some(session.project_id.clone()), Some(session.worktree.clone()), window, cx);
                        if parts.get(1).map(String::as_str) == Some("codex") {
                            let account = this.state.default_account(AgentKind::Codex).map(|a| a.id.clone());
                            if let Some(Modal::NewSession(form)) = this.modal.as_mut() {
                                form.agent = AgentKind::Codex;
                                form.account_id = account;
                            }
                            this.load_resume_list(cx);
                        }
                    });
                    cx.background_executor().timer(Duration::from_secs(2)).await;
                    let _ = this.update(cx, |this, cx| {
                        if parts.get(2).map(String::as_str) == Some("pick") {
                            if let Some(Modal::NewSession(form)) = this.modal.as_mut() {
                                if let Some(first) = form.history.as_ref().and_then(|h| h.first()).cloned() {
                                    form.account_id = Some(first.account_id.clone());
                                    form.resume = Some(first);
                                }
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            Some(other) if other.starts_with("wt-") => {
                // "wt-menu:<path end>", "wt-remove:<path end>", "wt-remove-go:<path end>",
                // "wt-cleanup", "wt-cleanup-go": run once the worktree lists have loaded.
                let mode = other.to_string();
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(Duration::from_secs(2)).await;
                    let _ = this.update_in(cx, |this, window, cx| this.debug_worktrees(&mode, 0, window, cx));
                    cx.background_executor().timer(Duration::from_secs(3)).await;
                    let _ = this.update_in(cx, |this, window, cx| this.debug_worktrees(&mode, 1, window, cx));
                })
                .detach();
            }
            Some("code") => {
                this.right_tab = RightTab::Files;
                if let Some(worktree) = this.active_session().map(|s| s.worktree.clone()) {
                    if let Ok(changes) = git::status(&worktree) {
                        if let Some(first) = changes.first() {
                            let path = first.path.clone();
                            this.changes = changes;
                            this.open_diff(path, cx);
                        }
                    }
                }
            }
            _ => {}
        }

        // Developer aid: type into the active terminal, e.g. "3000:\\r|8000:hello\\r".
        if let Some(script) = std::env::var("GLASSCODE_DEBUG_TYPE").ok().filter(|_| cfg!(debug_assertions)) {
            let steps: Vec<(u64, String)> = script
                .split('|')
                .filter_map(|step| step.split_once(':'))
                .map(|(ms, text)| (ms.parse().unwrap_or(0), text.replace("\\r", "\r").replace("\\e", "\x1b")))
                .collect();
            cx.spawn(async move |this, cx| {
                let mut elapsed = 0;
                for (at, text) in steps {
                    cx.background_executor().timer(Duration::from_millis(at.saturating_sub(elapsed))).await;
                    elapsed = at;
                    let _ = this.update(cx, |this, cx| {
                        let terminal = match &this.center {
                            crate::center::Center::Server(id) => this.server.terminals.get(id).cloned(),
                            _ => this.state.active_session.as_ref().and_then(|id| this.terminals.get(id)).cloned(),
                        };
                        if let Some(terminal) = terminal {
                            terminal.update(cx, |t, cx| t.write(text.into_bytes(), cx));
                        }
                    });
                }
            })
            .detach();
        }

        // Developer aid: paste an image file as if copied ("<ms>:image:<file>"), drop files
        // ("<ms>:drop:<path>") into the active terminal, save what it shows ("<ms>:dump:<file>")
        // or the app's state ("<ms>:state:<file>"); "|" separates steps.
        if let Some(script) = std::env::var("GLASSCODE_DEBUG_FILES").ok().filter(|_| cfg!(debug_assertions)) {
            let steps: Vec<(u64, String, String)> = script
                .split('|')
                .filter_map(|step| {
                    let (ms, rest) = step.split_once(':')?;
                    let (kind, path) = rest.split_once(':')?;
                    Some((ms.parse().unwrap_or(0), kind.to_string(), path.to_string()))
                })
                .collect();
            cx.spawn(async move |this, cx| {
                let mut elapsed = 0;
                for (at, kind, path) in steps {
                    cx.background_executor().timer(Duration::from_millis(at.saturating_sub(elapsed))).await;
                    elapsed = at;
                    let _ = this.update(cx, |this, cx| {
                        if kind == "state" {
                            let state = serde_json::json!({
                                "dialog_open": this.modal.is_some(),
                                "removing": this.removing.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                                "toast": this.toast.as_ref().map(|(t, _)| t.clone()),
                            });
                            let _ = std::fs::write(&path, state.to_string());
                            return;
                        }
                        let Some(terminal) = this.state.active_session.as_ref().and_then(|id| this.terminals.get(id)).cloned() else { return };
                        if kind == "dump" {
                            let _ = std::fs::write(&path, terminal.read(cx).text_tail(300));
                            return;
                        }
                        let incoming = match kind.as_str() {
                            "image" => {
                                let Ok(bytes) = std::fs::read(&path) else { return };
                                let format = if path.ends_with(".tiff") { gpui::ImageFormat::Tiff } else { gpui::ImageFormat::Png };
                                crate::terminal::files::Incoming::Image(gpui::Image::from_bytes(format, bytes))
                            }
                            _ => crate::terminal::files::Incoming::Path(PathBuf::from(&path)),
                        };
                        terminal.update(cx, |t, cx| t.paste_files(vec![incoming], cx));
                    });
                }
            })
            .detach();
        }

        // Developer aid: show sessions as busy ("<id prefix>:needs|working,…") to see the sidebar.
        if let Some(spec) = std::env::var("GLASSCODE_DEBUG_ACTIVITY").ok().filter(|_| cfg!(debug_assertions)) {
            for item in spec.split(',') {
                let Some((prefix, what)) = item.split_once(':') else { continue };
                let Some(id) = this.state.sessions.iter().find(|s| s.id.starts_with(prefix)).map(|s| s.id.clone()) else { continue };
                this.activity.insert(id, if what == "needs" { AgentActivity::NeedsInput } else { AgentActivity::Working });
            }
        }

        // Developer aid: "<ms>:<session id prefix>:finished|needs" shows that notification.
        if let Some(script) = std::env::var("GLASSCODE_DEBUG_NOTIFY").ok().filter(|_| cfg!(debug_assertions)) {
            let steps: Vec<(u64, String, String)> = script
                .split('|')
                .filter_map(|step| {
                    let mut parts = step.splitn(3, ':');
                    Some((parts.next()?.parse().ok()?, parts.next()?.to_string(), parts.next()?.to_string()))
                })
                .collect();
            cx.spawn(async move |this, cx| {
                let mut elapsed = 0;
                for (at, prefix, what) in steps {
                    cx.background_executor().timer(Duration::from_millis(at.saturating_sub(elapsed))).await;
                    elapsed = at;
                    let _ = this.update(cx, |this, cx| {
                        let Some(id) = this.state.sessions.iter().find(|s| s.id.starts_with(&prefix)).map(|s| s.id.clone()) else { return };
                        this.window_active = false;
                        if what == "needs" {
                            this.activity_changed(&id, Some(AgentActivity::Working), AgentActivity::NeedsInput, cx);
                        } else {
                            this.activity_changed(&id, Some(AgentActivity::Working), AgentActivity::Idle, cx);
                        }
                    });
                }
            })
            .detach();
        }

        // The installed/approved versions first (local, instant), then the newest published
        // version in the background (a network call that must not hold up the rest).
        let check_env = cli_env.clone();
        let task = cx.background_spawn(async move { versions::check(&check_env) });
        cx.spawn(async move |this, cx| {
            let status = task.await;
            // Only npm-backed CLIs have a "newest published" version to look up.
            let packages: Vec<(String, String)> =
                status.iter().filter_map(|s| s.npm_package().map(|p| (s.name.clone(), p))).collect();
            let _ = this.update(cx, |this, cx| {
                this.cli_status = status;
                cx.notify();
            });
            for (name, package) in packages {
                let latest = cx.background_spawn(async move { versions::latest_published(&package) }).await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(s) = this.cli_status.iter_mut().find(|s| s.name == name) {
                        s.latest = latest;
                    }
                    cx.notify();
                });
            }
        })
        .detach();

        this
    }

    /// Re-check installed CLI versions (and their newest published versions), e.g.
    /// after an in-app update finishes, so the rows refresh without a restart.
    pub fn refresh_cli_status(&mut self, cx: &mut Context<Self>) {
        let check_env = (*self.env).clone();
        let task = cx.background_spawn(async move { versions::check(&check_env) });
        cx.spawn(async move |this, cx| {
            let mut status = task.await;
            // Keep any newest-published versions already known, then refresh them.
            let _ = this.update(cx, |this, cx| {
                for s in status.iter_mut() {
                    if let Some(prev) = this.cli_status.iter().find(|p| p.name == s.name) {
                        s.latest = prev.latest.clone();
                    }
                }
                this.cli_status = status.clone();
                cx.notify();
            });
            let packages: Vec<(String, String)> =
                status.iter().filter_map(|s| s.npm_package().map(|p| (s.name.clone(), p))).collect();
            for (name, package) in packages {
                let latest = cx.background_spawn(async move { versions::latest_published(&package) }).await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(s) = this.cli_status.iter_mut().find(|s| s.name == name) {
                        s.latest = latest;
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    // ----- state helpers -------------------------------------------------------------

    pub fn save(&self) {
        if let Err(err) = store::save(&self.state) {
            log::error!("could not save state: {err}");
        }
    }

    pub fn active_session(&self) -> Option<&Session> {
        self.state.active_session.as_ref().and_then(|id| self.state.session(id))
    }

    pub fn show_toast(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.toast = Some((message.into(), Instant::now()));
        cx.notify();
    }

    pub fn focus_handle_ref(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn focus_active_pub(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_active(window, cx);
    }

    /// Move the keyboard to the active session only when the person is looking at it: never while
    /// a dialog or the palette is open, or another view (Today, Tickets) is up. So a command
    /// (an agent) that closes or restarts a session never pulls focus from what the person is
    /// doing.
    pub(crate) fn focus_active_if_watching(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let watching = self.modal.is_none() && self.palette.is_none() && self.tickets.view == crate::tickets_ui::View::Sessions && self.center == crate::center::Center::Session;
        if watching {
            self.focus_active(window, cx);
        }
    }

    pub(crate) fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // In the Chat view, typing goes to its message box.
        if let Some(session) = self.active_session().cloned() {
            if self.session_view(&session) == glasscode_core::model::SessionView::Chat {
                let composer = self.chat_composer(&session.id, window, cx);
                let handle = composer.read(cx).focus_handle_clone();
                window.focus(&handle, cx);
                return;
            }
        }
        let terminal = self
            .state
            .active_session
            .as_ref()
            .and_then(|id| self.terminals.get(id))
            .cloned();
        match terminal {
            Some(terminal) => {
                let handle = terminal.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            None => window.focus(&self.focus, cx),
        }
    }

    // ----- terminals -----------------------------------------------------------------

    pub fn ensure_terminal(&mut self, session_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminals.contains_key(session_id) {
            return;
        }
        let Some(session) = self.state.session(session_id).cloned() else { return };
        if !session.worktree.is_dir() {
            self.show_toast(
                format!("Folder {} no longer exists", session.worktree.display()),
                cx,
            );
            return;
        }
        let account = session
            .account_id
            .as_deref()
            .and_then(|id| self.state.account(id))
            .cloned();
        let mut launch = agents::launch(&session, account.as_ref(), &self.env);
        // Its project's GitHub account: pushes and gh act as it (the token is never stored).
        launch.env.extend(self.github_env(&session.project_id));
        // Light or dark, for programs that do not ask the terminal (and for tmux on the server).
        launch.env.insert("COLORFGBG".into(), if crate::theme::is_light() { "0;15" } else { "15;0" }.into());
        // The `glasscode` command: which session it runs in (signed, so the app can tell), how to
        // reach this app, and where to leave requests when a sandbox keeps it from the socket.
        launch.env.insert("GLASSCODE_SESSION_SIG".into(), self.control.signature(&session.id));
        launch.env.insert("GLASSCODE_CONTROL_SOCKET".into(), glasscode_core::control::socket_path().to_string_lossy().into_owned());
        launch.env.insert("GLASSCODE_WORKTREE".into(), session.worktree.to_string_lossy().into_owned());
        if session.agent != AgentKind::Shell && !Path::new(&launch.program).is_file() {
            self.show_toast(
                format!(
                    "{} is not installed or not on your PATH. Install it, then reopen this session.",
                    launch.program
                ),
                cx,
            );
            return;
        }
        let spec = SpawnSpec {
            program: launch.program,
            args: launch.args,
            cwd: session.worktree.clone(),
            env: launch.env,
        };
        let terminal = match TerminalView::spawn(spec, cx) {
            Ok(terminal) => terminal,
            Err(err) => {
                self.show_toast(format!("Could not start session: {err}"), cx);
                return;
            }
        };
        let id = session_id.to_string();
        let sub = cx.subscribe_in(&terminal, window, move |this, _, event, _window, cx| {
            match event {
                TerminalEvent::Bell => {
                    if this.state.active_session.as_deref() != Some(&id) {
                        this.attention.insert(id.clone());
                    }
                }
                TerminalEvent::Exited(_) => {
                    this.activity.remove(&id);
                }
                TerminalEvent::TitleChanged => {}
                TerminalEvent::Notice(message) => this.show_toast(message.clone(), cx),
            }
            cx.notify();
        });
        self._subscriptions.push(sub);
        self.tails.insert(session_id.to_string(), EventsTail::new(session_id));
        self.launched_at.insert(session_id.to_string(), SystemTime::now());
        self.terminals.insert(session_id.to_string(), terminal);
        self.save();
        let _ = window;
    }

    /// Keep a subscription alive as long as the workspace.
    pub fn keep_subscription(&mut self, sub: Subscription) {
        self._subscriptions.push(sub);
    }

    /// Remove a session without a toast or starting another terminal (it moved elsewhere).
    pub fn remove_session_quietly(&mut self, session_id: &str) {
        let Some(index) = self.state.sessions.iter().position(|s| s.id == session_id) else { return };
        let removed = self.state.sessions.remove(index);
        self.stop_terminal(session_id);
        let _ = std::fs::remove_file(agents::events_file(session_id));
        if self.state.active_session.as_deref() == Some(session_id) {
            self.state.active_session = self
                .state
                .sessions
                .iter()
                .find(|s| s.project_id == removed.project_id)
                .or_else(|| self.state.sessions.first())
                .map(|s| s.id.clone());
        }
        self.save();
    }

    pub fn stop_terminal(&mut self, session_id: &str) {
        self.terminals.remove(session_id);
        self.tails.remove(session_id);
        self.activity.remove(session_id);
    }

    pub fn restart_session(&mut self, session_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_terminal(session_id);
        self.ensure_terminal(session_id, window, cx);
        if self.state.active_session.as_deref() == Some(session_id) {
            self.focus_active_if_watching(window, cx);
        }
        cx.notify();
    }

    // ----- sessions ------------------------------------------------------------------

    pub fn select_session(&mut self, session_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.today.navigated_at = Some(Instant::now());
        self.center = crate::center::Center::Session;
        if !self.prs.map.contains_key(session_id) {
            self.pr_refresh(cx);
        }
        self.state.active_session = Some(session_id.to_string());
        self.attention.remove(session_id);
        self.menu_open = false;
        if let Some(worktree) = self.state.session(session_id).map(|s| s.worktree.clone()) {
            self.last_in_worktree.insert(worktree, session_id.to_string());
        }
        self.dismiss_popups_for(session_id, cx);
        if let Some(project_id) = self.state.session(session_id).map(|s| s.project_id.clone()) {
            if let Some(project) = self.state.projects.iter_mut().find(|p| p.id == project_id) {
                project.expanded = true;
            }
        }
        self.ensure_terminal(session_id, window, cx);
        self.focus_active(window, cx);
        self.diff = None;
        if self.changes_for.as_ref() != self.active_session().map(|s| &s.worktree) {
            self.changes.clear();
            self.changes_for = None;
        }
        self.last_git_refresh = Instant::now() - Duration::from_secs(60);
        self.save();
        cx.notify();
    }

    pub fn add_session(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        let id = session.id.clone();
        self.state.sessions.push(session);
        self.select_session(&id, window, cx);
    }

    pub fn close_session(&mut self, session_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.state.sessions.iter().position(|s| s.id == session_id) else {
            return;
        };
        let closed = self.state.sessions.remove(index);
        self.stop_terminal(session_id);
        let _ = std::fs::remove_file(agents::events_file(session_id));
        if self.state.active_session.as_deref() == Some(session_id) {
            let next = self
                .state
                .sessions
                .iter()
                .find(|s| s.project_id == closed.project_id)
                .or_else(|| self.state.sessions.first())
                .map(|s| s.id.clone());
            self.state.active_session = next.clone();
            if let Some(next) = next {
                self.ensure_terminal(&next, window, cx);
            }
            self.focus_active_if_watching(window, cx);
        }
        self.save();
        self.show_toast(
            if closed.agent == AgentKind::Shell {
                format!("Closed “{}”. Its worktree stays on disk.", closed.title)
            } else {
                format!("Closed “{}”. The worktree stays; New session there can resume the conversation.", closed.title)
            },
            cx,
        );
    }

    pub fn new_session_id_for(agent: AgentKind) -> Option<String> {
        // Claude accepts an id up front, so the session is resumable from the first second.
        (agent == AgentKind::Claude).then(new_id)
    }

    pub fn start_default_claude(&mut self, project_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.state.project(project_id).cloned() else { return };
        let Some(account) = self.state.default_account(AgentKind::Claude).cloned() else {
            return;
        };
        let branch = git::current_branch(&project.root);
        let session = Session {
            id: new_id(),
            project_id: project.id.clone(),
            worktree: project.root.clone(),
            branch: branch.clone(),
            title: format!("Claude · {branch}"),
            agent: AgentKind::Claude,
            account_id: Some(account.id.clone()),
            agent_session_id: Self::new_session_id_for(AgentKind::Claude),
            transcript_path: None,
            lineage: vec![self.state.lineage_tag(AgentKind::Claude, Some(&account.id))],
            initial_prompt: None,
            team_task: None,
            model: None,
            mode: None,
            review: None,
            created_at: now(),
        };
        self.state.active_session = Some(session.id.clone());
        self.state.sessions.push(session);
        let _ = (window, cx);
    }

    fn cycle_session(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.state.sessions.iter().map(|s| s.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let current = self
            .state
            .active_session
            .as_ref()
            .and_then(|id| ids.iter().position(|i| i == id))
            .unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(ids.len() as isize) as usize;
        let id = ids[next].clone();
        self.select_session(&id, window, cx);
    }

    // ----- projects ------------------------------------------------------------------

    pub fn prompt_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update_in(cx, |this, window, cx| this.add_project(path, window, cx));
                }
            }
        })
        .detach();
    }

    pub fn add_project(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // A folder without git is fine too: its sessions work in the folder itself, until
        // "Set up git" (the project's ⋯ menu) gives them worktrees of their own.
        let (root, plain) = match git::main_checkout(&path).or_else(|_| git::repo_root(&path)) {
            Ok(root) => (root, false),
            Err(_) if path.is_dir() => (path.canonicalize().unwrap_or(path.clone()), true),
            Err(_) => {
                self.show_toast(format!("{} is not a folder.", path.display()), cx);
                return;
            }
        };
        if plain {
            self.show_toast("Added without git: sessions work in the folder itself. Set up git from the project's ⋯ menu to give each session its own worktree.", cx);
        }
        if let Some(existing) = self.state.projects.iter().find(|p| p.root == root) {
            let id = existing.id.clone();
            self.show_toast(format!("{} is already in your projects", existing.name), cx);
            self.open_new_session(Some(id), None, window, cx);
            return;
        }
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".into());
        let id = new_id();
        self.state.projects.push(glasscode_core::model::Project {
            id: id.clone(),
            name,
            root,
            expanded: true,
            github_account: None,
        });
        self.save();
        self.configure_github(cx);
        self.refresh_worktrees(cx);
        self.open_new_session(Some(id), None, window, cx);
    }

    /// Ask before removing a project from Glasscode (the ⋯ menu's "Remove project").
    pub fn confirm_remove_project(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.state.project(&project_id).map(|p| p.name.clone()) else { return };
        let sessions = self.state.sessions.iter().filter(|s| s.project_id == project_id).count();
        self.modal = Some(crate::dialogs::Modal::ConfirmRemoveProject { project_id, name, sessions });
        let _ = window;
        cx.notify();
    }

    pub fn remove_project(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .state
            .sessions
            .iter()
            .filter(|s| s.project_id == project_id)
            .map(|s| s.id.clone())
            .collect();
        for id in &ids {
            self.stop_terminal(id);
            let _ = std::fs::remove_file(glasscode_core::agents::events_file(id));
            self.state.ticket_of.remove(id);
            self.state.custom_titles.remove(id);
        }
        self.state.sessions.retain(|s| s.project_id != project_id);
        self.state.projects.retain(|p| p.id != project_id);
        if self.active_session().is_none() {
            self.state.active_session = self.state.sessions.first().map(|s| s.id.clone());
        }
        self.save();
        cx.notify();
    }

    pub fn refresh_worktrees(&mut self, cx: &mut Context<Self>) {
        self.last_worktree_refresh = Instant::now();
        let projects: Vec<(String, PathBuf)> = self
            .state
            .projects
            .iter()
            .map(|p| (p.id.clone(), p.root.clone()))
            .collect();
        let task = cx.background_spawn(async move {
            projects
                .into_iter()
                .map(|(id, root)| (id, git::worktrees(&root).unwrap_or_default()))
                .collect::<HashMap<_, _>>()
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                // A branch switched or renamed inside a worktree shows up in its sessions too.
                let mut changed = false;
                for session in this.state.sessions.iter_mut() {
                    let current = result
                        .get(&session.project_id)
                        .and_then(|list| list.iter().find(|w| w.path == session.worktree))
                        .map(|w| w.branch.clone());
                    if let Some(branch) = current.filter(|b| !b.is_empty() && b != "detached" && *b != session.branch) {
                        session.branch = branch;
                        changed = true;
                    }
                }
                this.worktrees = result;
                if changed {
                    this.save();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Worktrees to show for a project: git's list plus any session folders git no longer reports.
    pub fn project_worktrees(&self, project_id: &str) -> Vec<(PathBuf, String)> {
        let mut list: Vec<(PathBuf, String)> = self
            .worktrees
            .get(project_id)
            .map(|w| w.iter().map(|w| (w.path.clone(), w.branch.clone())).collect())
            .unwrap_or_default();
        if list.is_empty() {
            if let Some(project) = self.state.project(project_id) {
                list.push((project.root.clone(), git::current_branch(&project.root)));
            }
        }
        for session in self.state.sessions.iter().filter(|s| s.project_id == project_id) {
            if !list.iter().any(|(p, _)| *p == session.worktree) {
                list.push((session.worktree.clone(), session.branch.clone()));
            }
        }
        list
    }

    // ----- background tick -----------------------------------------------------------

    fn tick(&mut self, cx: &mut Context<Self>) {
        self.server_tick(cx);
        self.pr_tick(cx);
        self.update_tick(cx);
        self.mirror_tick(cx);
        self.browser_tick(cx);
        self.switch_tick(cx);
        self.gh_tick(cx);
        self.ci_fix_tick(cx);
        self.auto_review_tick(cx);
        self.titles_tick(cx);
        self.pairing_tick(cx);
        self.tickets_tick(cx);
        self.usage_tick(cx);
        self.themes_tick(cx);
        self.today_tick(cx);
        let mut dirty = false;
        let mut redraw = false;
        let ids: Vec<String> = self.tails.keys().cloned().collect();
        for id in ids {
            let Some(update) = self.tails.get_mut(&id).and_then(EventsTail::poll) else { continue };
            if let Some(session) = self.state.session_mut(&id) {
                if let Some(conversation) = update.conversation_id {
                    if session.agent_session_id.as_deref() != Some(conversation.as_str()) {
                        session.agent_session_id = Some(conversation);
                        dirty = true;
                    }
                }
                if let Some(path) = update.transcript_path {
                    if session.transcript_path.as_ref() != Some(&path) {
                        session.transcript_path = Some(path);
                        dirty = true;
                    }
                }
            }
            if update.activity == Some(AgentActivity::Working) {
                if let Some(session) = self.state.session_mut(&id) {
                    if session.initial_prompt.take().is_some() {
                        dirty = true;
                    }
                }
            }
            if let Some(activity) = update.activity {
                if activity == AgentActivity::NeedsInput
                    && self.state.active_session.as_deref() != Some(id.as_str())
                {
                    self.attention.insert(id.clone());
                }
                let previous = self.activity.insert(id.clone(), activity);
                self.activity_changed(&id, previous, activity, cx);
                redraw = true;
            }
        }

        // Codex has no `--session-id`: find its conversation by folder once it starts.
        let pending: Vec<(String, PathBuf, Option<String>)> = self
            .state
            .sessions
            .iter()
            .filter(|s| {
                s.agent == AgentKind::Codex
                    && s.agent_session_id.is_none()
                    && self.terminals.contains_key(&s.id)
            })
            .map(|s| (s.id.clone(), s.worktree.clone(), s.account_id.clone()))
            .collect();
        for (id, worktree, account_id) in pending {
            let home = account_id
                .as_deref()
                .and_then(|a| self.state.account(a))
                .map(|a| a.effective_dir());
            let since = self
                .launched_at
                .get(&id)
                .copied()
                .unwrap_or(SystemTime::UNIX_EPOCH)
                - Duration::from_secs(5);
            if let Some(home) = home {
                if let Some((conversation, path)) = agents::find_codex_conversation(&home, &worktree, since) {
                    let taken = self
                        .state
                        .sessions
                        .iter()
                        .any(|s| s.agent_session_id.as_deref() == Some(conversation.as_str()));
                    if !taken {
                        if let Some(session) = self.state.session_mut(&id) {
                            session.agent_session_id = Some(conversation);
                            session.transcript_path = Some(path);
                            session.initial_prompt = None;
                            dirty = true;
                        }
                    }
                }
            }
        }

        if dirty {
            self.save();
            redraw = true;
        }
        if self.last_git_refresh.elapsed() > Duration::from_secs(3) {
            self.refresh_git(cx);
        }
        self.glasscode_tick(cx);
        if self.last_worktree_refresh.elapsed() > Duration::from_secs(15) {
            self.refresh_worktrees(cx);
        }
        if let Some((_, at)) = &self.toast {
            if at.elapsed() > Duration::from_secs(5) {
                self.toast = None;
                redraw = true;
            }
        }
        if redraw {
            cx.notify();
        }
    }

    // ----- git -----------------------------------------------------------------------

    pub fn refresh_git(&mut self, cx: &mut Context<Self>) {
        self.last_git_refresh = Instant::now();
        let Some(worktree) = self.active_session().map(|s| s.worktree.clone()) else {
            self.changes.clear();
            self.changes_for = None;
            return;
        };
        let path = worktree.clone();
        let task = cx.background_spawn(async move { git::status(&path) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let still_active = this.active_session().map(|s| &s.worktree) == Some(&worktree);
                if !still_active {
                    return;
                }
                let changes = result.unwrap_or_default();
                if changes != this.changes || this.changes_for.as_ref() != Some(&worktree) {
                    this.changes = changes;
                    this.changes_for = Some(worktree);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn open_diff(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(worktree) = self.active_session().map(|s| s.worktree.clone()) else { return };
        let untracked = self
            .changes
            .iter()
            .any(|c| c.path == path && c.status == '?');
        self.right_tab = RightTab::Files;
        self.gh.files.reveal(&path);
        let file = path.clone();
        let task = cx.background_spawn(async move {
            let diff = git::diff_file(&worktree, &file, untracked).unwrap_or_default();
            if diff.trim().is_empty() {
                git::read_file(&worktree, &file)
                    .map(|text| text.lines().map(|l| (' ', l.to_string())).collect())
                    .unwrap_or_default()
            } else {
                diff.lines()
                    .skip_while(|l| !l.starts_with("@@"))
                    .map(|l| {
                        let kind = l.chars().next().unwrap_or(' ');
                        let text = if kind == '@' { l.to_string() } else { l.get(1..).unwrap_or("").to_string() };
                        (kind, text)
                    })
                    .collect::<Vec<_>>()
            }
        });
        cx.spawn(async move |this, cx| {
            let lines = task.await;
            let _ = this.update(cx, |this, cx| {
                this.diff = Some(DiffView { path, lines });
                cx.notify();
            });
        })
        .detach();
    }

    pub fn toggle_stage(&mut self, change: FileChange, cx: &mut Context<Self>) {
        let Some(worktree) = self.active_session().map(|s| s.worktree.clone()) else { return };
        let result = if change.staged {
            git::unstage(&worktree, &change.path)
        } else {
            git::stage(&worktree, &change.path)
        };
        if let Err(err) = result {
            self.show_toast(format!("git: {err}"), cx);
        }
        self.refresh_git(cx);
    }

    pub fn stage_all(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active_session().map(|s| s.worktree.clone()) else { return };
        for change in self.changes.clone().iter().filter(|c| !c.staged) {
            let _ = git::stage(&worktree, &change.path);
        }
        self.refresh_git(cx);
    }

    pub fn commit(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self.active_session().map(|s| s.worktree.clone()) else { return };
        let staged = self.changes.iter().filter(|c| c.staged).count();
        if staged == 0 {
            self.show_toast("Stage at least one file first.", cx);
            return;
        }
        let message = self.commit_input.read(cx).text();
        if message.trim().is_empty() {
            self.show_toast("Write a commit message first.", cx);
            return;
        }
        match git::commit(&worktree, message.trim()) {
            Ok(_) => {
                self.commit_input.update(cx, |input, cx| input.set_text("", cx));
                self.show_toast(format!("Committed {staged} file(s): {}", message.trim()), cx);
            }
            Err(err) => self.show_toast(format!("Commit failed: {err}"), cx),
        }
        self.refresh_git(cx);
    }

    /// The editor "Open in" uses, with its name: the one chosen in Settings when it is
    /// installed, else the first one installed.
    pub fn preferred_editor(&self) -> Option<(&'static str, PathBuf)> {
        let installed = |id: &str| {
            glasscode_core::prefs::EDITORS
                .iter()
                .find(|(e, _, _)| *e == id)
                .and_then(|(_, name, command)| agents::resolve_program(command, &self.env).map(|program| (*name, program)))
        };
        self.state.prefs.editor.as_deref().and_then(installed).or_else(|| ["cursor", "code", "zed"].into_iter().find_map(installed))
    }

    pub fn open_in_editor(&mut self, relative: Option<String>, cx: &mut Context<Self>) {
        let Some(worktree) = self.active_session().map(|s| s.worktree.clone()) else { return };
        let target = relative.map(|r| worktree.join(r)).unwrap_or(worktree.clone());
        let result = match self.preferred_editor().map(|(_, program)| program) {
            Some(program) => glasscode_core::sys::command(program)
                .arg(&worktree)
                .arg(&target)
                .envs(self.env.iter())
                .spawn()
                .map(|_| ()),
            None => glasscode_core::sys::command("open").arg(&target).spawn().map(|_| ()),
        };
        if let Err(err) = result {
            self.show_toast(format!("Could not open editor: {err}"), cx);
        }
    }

    // ----- dialogs -------------------------------------------------------------------

    pub fn open_new_session(
        &mut self,
        project_id: Option<String>,
        worktree: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project_id = project_id
            .or_else(|| self.active_session().map(|s| s.project_id.clone()))
            .or_else(|| self.state.projects.first().map(|p| p.id.clone()));
        let Some(project_id) = project_id else {
            self.prompt_add_project(window, cx);
            return;
        };
        self.menu_open = false;
        self.modal = Some(Modal::new_session(self, project_id, worktree, window, cx));
        self.load_resume_list(cx);
        self.new_session_fetch_base(cx);
        cx.notify();
    }

    /// Settings › Accounts (⌘, and the Accounts button).
    pub fn open_accounts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(crate::settings_ui::Section::Accounts, window, cx);
    }

    pub fn open_handoff(
        &mut self,
        to_agent: Option<AgentKind>,
        to_account: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.active_session().cloned() else { return };
        self.menu_open = false;
        let modal = Modal::handoff(self, &session, to_agent, to_account, window, cx);
        self.modal = Some(modal);
        cx.notify();
    }

    pub fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Same agent, other account: copy the conversation to the new account and resume it there.
    pub fn switch_account(&mut self, account_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.active_session().cloned() else { return };
        let (Some(from), Some(to)) = (
            session.account_id.as_deref().and_then(|id| self.state.account(id)).cloned(),
            self.state.account(account_id).cloned(),
        ) else {
            return;
        };
        if from.id == to.id {
            return;
        }
        if let Some(conversation) = &session.agent_session_id {
            if let Err(err) = agents::copy_conversation(session.agent, &from, &to, conversation) {
                log::warn!("copy conversation: {err}");
            }
        }
        let tag = self.state.lineage_tag(session.agent, Some(&to.id));
        if let Some(s) = self.state.session_mut(&session.id) {
            s.account_id = Some(to.id.clone());
            s.lineage.push(tag);
        }
        self.save();
        self.restart_session(&session.id, window, cx);
        self.show_toast(
            format!(
                "Switched to {} · {} — same conversation.",
                session.agent.name(),
                to.label
            ),
            cx,
        );
    }

    // ----- actions -------------------------------------------------------------------

    fn on_new_session(&mut self, _: &NewSession, window: &mut Window, cx: &mut Context<Self>) {
        self.open_new_session(None, None, window, cx);
    }

    fn on_close_session(&mut self, _: &CloseSession, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal.is_some() {
            self.close_modal(window, cx);
            return;
        }
        if let Some(id) = self.state.active_session.clone() {
            // A working agent: the first ⌘W only asks (unless that is switched off).
            let working = self.state.session(&id).is_some_and(|s| self.row_state(s) == crate::sidebar::RowState::Working);
            let confirmed = self.close_asked.take().is_some_and(|(asked, at)| asked == id && at.elapsed() < Duration::from_secs(5));
            if working && !confirmed && !self.state.prefs.confirm_close_off {
                self.close_asked = Some((id, Instant::now()));
                self.show_toast("Its agent is working. Press ⌘W again to close it anyway.", cx);
                return;
            }
            self.close_session(&id, window, cx);
        }
    }

    fn on_next(&mut self, _: &NextSession, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_session(1, window, cx);
    }

    fn on_previous(&mut self, _: &PreviousSession, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_session(-1, window, cx);
    }

    fn on_accounts(&mut self, _: &OpenAccounts, window: &mut Window, cx: &mut Context<Self>) {
        self.open_accounts(window, cx);
    }

    fn on_add_project(&mut self, _: &AddProject, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_add_project(window, cx);
    }

    fn on_handoff(&mut self, _: &Handoff, window: &mut Window, cx: &mut Context<Self>) {
        self.open_handoff(None, None, window, cx);
    }

    fn on_toggle_right(&mut self, _: &ToggleRightPanel, _: &mut Window, cx: &mut Context<Self>) {
        self.show_right = !self.show_right;
        cx.notify();
    }

    fn on_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        self.save();
        cx.quit();
    }
}

// ----- rendering ---------------------------------------------------------------------

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let modal = self.modal.as_ref().map(|m| m.render(self, window, cx));

        // The glasses navigated to a session (desk mode) → open it here. Deferred from the
        // poll tick, which has no Window; render does. Guarded, so it runs once per change.
        if let Some(id) = self.glasscode.pending_open.take() {
            if self.state.active_session.as_deref() != Some(id.as_str()) {
                self.select_session(&id, window, cx);
            }
        }
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_new_session))
            .on_action(cx.listener(Self::on_close_session))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_previous))
            .on_action(cx.listener(Self::on_accounts))
            .on_action(cx.listener(Self::on_add_project))
            .on_action(cx.listener(Self::on_handoff))
            .on_action(cx.listener(Self::on_toggle_right))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(|this, _: &OpenPalette, window, cx| this.open_palette(window, cx)))
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| this.resize_move(event, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| this.resize_end(cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family(theme::UI_FONT)
            .text_size(px(theme::ui_text_size()))
            .child(self.render_title_bar(cx))
            .child({
                let row = div().flex().flex_1().min_h_0().child(self.render_rail(cx));
                if self.tickets.view == crate::tickets_ui::View::Today {
                    row.child(self.render_today(window, cx))
                } else if self.tickets.view == crate::tickets_ui::View::Tickets {
                    row.child(self.render_tickets(window, cx))
                } else if self.tickets.view == crate::tickets_ui::View::Usage {
                    row.child(self.render_usage(window, cx))
                } else if self.tickets.view == crate::tickets_ui::View::Settings {
                    row.child(self.render_settings(window, cx))
                } else if self.tickets.view == crate::tickets_ui::View::Themes {
                    row.child(self.render_themes(window, cx))
                } else {
                    row.child(self.render_sidebar(cx))
                        .child(self.resize_handle(Side::Left, cx))
                        .child(match self.center.clone() {
                            crate::center::Center::Session => self.render_center(window, cx).into_any_element(),
                            crate::center::Center::Server(id) => self.render_server_center(&id, cx),
                        })
                        .when(self.show_right, |d| d.child(self.resize_handle(Side::Right, cx)).child(self.render_right_panel(cx)))
                }
            })
            .child(self.render_status_bar(cx))
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
    }
}

impl Workspace {
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (project, worktree) = match (&self.center, self.active_session()) {
            _ if matches!(self.tickets.view, crate::tickets_ui::View::Usage | crate::tickets_ui::View::Settings | crate::tickets_ui::View::Themes) => (
                match self.tickets.view {
                    crate::tickets_ui::View::Usage => "Usage",
                    crate::tickets_ui::View::Themes => "Themes",
                    _ => "Settings",
                }
                .to_string(),
                String::new(),
            ),
            _ if self.tickets.view == crate::tickets_ui::View::Today => ("Today".to_string(), String::new()),
            _ if self.tickets.view == crate::tickets_ui::View::Tickets => (
                "Tickets".to_string(),
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
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_4()
            .h(px(44.))
            .pl(px(84.))
            .pr_3()
            .bg(theme::chrome())
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(crate::icons::icon(crate::icons::LOGO, px(19.), px(18.), theme::text_strong()))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).child("Glasscode")),
            )
            .child(
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
            // Search: the command palette.
            .child(
                div()
                    .id("title-search")
                    .flex()
                    .flex_1()
                    .min_w(px(180.))
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
                    .child(div().text_color(theme::faint()).child("⌕"))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child("Search sessions, tickets and commands"))
                    .child(div().px_1p5().rounded(px(4.)).border_1().border_color(theme::border()).text_size(px(11.)).text_color(theme::faint()).child("⌘K")),
            )
            .when_some(self.render_switch_buttons(cx), |d, buttons| d.child(buttons))
            .when_some(self.update_pill(cx), |d, pill| d.child(pill))
            .child(self.server_pill(cx))
            .child(
                ui::primary_button("new-session", "+  New session").on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| this.open_new_session(None, None, window, cx),
                )),
            )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut projects: Vec<AnyElement> = Vec::new();
        for project in &self.state.projects {
            // Projects switched off in Settings › Projects drop out of the sidebar entirely.
            if self.state.is_hidden(&project.id) {
                continue;
            }
            let project_id = project.id.clone();
            let count = self
                .state
                .sessions
                .iter()
                .filter(|s| s.project_id == project.id)
                .count();
            let is_current = self
                .active_session()
                .is_some_and(|s| s.project_id == project.id);
            let toggle_id = project_id.clone();
            let add_id = project_id.clone();
            let header = div()
                .flex()
                .items_center()
                .gap_0p5()
                .child(
                    div()
                        .id(SharedString::from(format!("project-{}", project.id)))
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_1p5()
                        .h(px(30.))
                        .px_1p5()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .text_color(if is_current { theme::text_strong() } else { theme::text_soft() })
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(p) = this.state.projects.iter_mut().find(|p| p.id == toggle_id) {
                                p.expanded = !p.expanded;
                            }
                            this.save();
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_color(theme::faint())
                                .text_size(px(10.))
                                .w(px(10.))
                                .child(if project.expanded { "▾" } else { "▸" }),
                        )
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(project.name.clone()))
                        .child(div().flex_1())
                        // The GitHub account this project's sessions use; a click opens Projects & Git.
                        .when_some(self.github_account_for(&project.id), |d, login| {
                            d.child(
                                div()
                                    .id(SharedString::from(format!("project-gh-{}", project.id)))
                                    .flex_none()
                                    .max_w(px(110.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .h(px(20.))
                                    .px_1p5()
                                    .flex()
                                    .items_center()
                                    .rounded(px(10.))
                                    .border_1()
                                    .border_color(theme::border())
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(theme::muted())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::selected()).text_color(theme::text_strong()))
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.open_settings(crate::settings_ui::Section::Projects, window, cx);
                                    }))
                                    .child(format!("@{login}")),
                            )
                        })
                        .when(count > 0, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(theme::muted())
                                    .child(count.to_string()),
                            )
                        }),
                )
                .child({
                    let menu_id = project_id.clone();
                    ui::icon_button(SharedString::from(format!("project-menu-{}", project.id)), "⋯").on_click(cx.listener(
                        move |this, event: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_row_menu(menu_id.clone(), None, event.position(), cx);
                        },
                    ))
                })
                .child(
                    ui::icon_button(SharedString::from(format!("project-add-{}", project.id)), "+")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_new_session(Some(add_id.clone()), None, window, cx)
                        })),
                );

            let mut body: Vec<AnyElement> = Vec::new();
            let repo_name = project.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if project.expanded {
                body.extend(self.project_rows(&project.id, &repo_name, cx));
                body.extend(self.idle_worktree_rows(&project.id, &repo_name, cx));
            }
            projects.push(
                div()
                    .mb_1p5()
                    .child(header)
                    .children(body)
                    .into_any_element(),
            );
        }

        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.sidebar_width()))
            .h_full()
            .bg(theme::panel())
            .border_r_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3p5()
                    .pt_3()
                    .pb_1p5()
                    .child(ui::caps_label("PROJECTS"))
                    .child({
                        let (needs, working) = self.sidebar_counts();
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .text_size(px(11.))
                            .text_color(theme::muted())
                            .child(div().flex().items_center().gap_1().child(div().size(px(7.)).rounded_full().bg(crate::sidebar::attention())).child(format!("{needs} need you")))
                            .child(div().flex().items_center().gap_1().child(div().size(px(7.)).rounded_full().bg(theme::added())).child(format!("{working} working")))
                    }),
            )
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_1p5()
                    .pb_2()
                    .children(projects)
                    .when(self.state.visible_projects().next().is_none(), |d| {
                        d.child(
                            div()
                                .p_3()
                                .text_color(theme::muted())
                                .child(if self.state.projects.is_empty() {
                                    "No projects yet. Add a git repository to start."
                                } else {
                                    "All projects are switched off in Settings › Projects."
                                }),
                        )
                    })
                    .when_some(self.render_server_sidebar(cx), |d, server| d.child(server)),
            )
            .child(
                div().p_2().border_t_1().border_color(theme::border()).child(
                    div()
                        .id("add-project")
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(px(32.))
                        .px_2()
                        .rounded_md()
                        .cursor_pointer()
                        .text_color(theme::text_soft())
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.prompt_add_project(window, cx)
                        }))
                        .child("+   Add project…"),
                ),
            )
    }

    fn render_center(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Sessions are switched and closed from the sidebar; the center shows just the active one.
        let active = self.active_session().cloned();
        let body: AnyElement = match active {
            Some(session) => self.render_session(session, window, cx).into_any_element(),
            None => self.render_empty_state(cx).into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::bg())
            .child(body)
    }

    fn render_empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_projects = !self.state.projects.is_empty();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if has_projects { "No open sessions" } else { "Welcome to Glasscode" }),
            )
            .child(div().max_w(px(380.)).text_color(theme::muted()).child(if has_projects {
                "Start a session on any project. New sessions open Claude with your default account."
            } else {
                "Add a git repository. Each session gets its own worktree, agent and account."
            }))
            .child(if has_projects {
                ui::primary_button("empty-new", "New session")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_new_session(None, None, window, cx)
                    }))
            } else {
                ui::primary_button("empty-add", "Add project…")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.prompt_add_project(window, cx)))
            })
    }

    fn render_session(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.terminals.contains_key(&session.id) && session.worktree.is_dir() {
            // A restored session that has not been resumed yet.
            let id = session.id.clone();
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(session.title.clone()))
                .child(div().text_color(theme::muted()).child(match &session.agent_session_id {
                    Some(conversation) => format!(
                        "Saved {} conversation {} — resume it with the same account.",
                        session.agent.name(),
                        &conversation[..8.min(conversation.len())]
                    ),
                    None => format!("{} session in {}", session.agent.name(), session.branch),
                }))
                .child(ui::primary_button("resume", "Resume session").on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| this.restart_session(&id, window, cx),
                )))
                .into_any_element();
        }
        let terminal = self.terminals.get(&session.id).cloned();
        let exited = terminal
            .as_ref()
            .and_then(|t| t.read(cx).exit_code)
            .map(|code| code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()));
        let account = session
            .account_id
            .as_deref()
            .and_then(|id| self.state.account(id))
            .cloned();
        let tabs = self.worktree_tabs(&session, cx);
        // Claude and Codex read as a chat, or show their terminal (the header's switch).
        let chat = if self.session_view(&session) == glasscode_core::model::SessionView::Chat { Some(self.render_chat(&session, window, cx)) } else { None };
        let chat_view = chat.is_some();
        let header = self.render_session_header(&session, account.as_ref(), window, cx);
        let restart_id = session.id.clone();
        let close_id = session.id.clone();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(header)
            .children(tabs)
            .when(session.lineage.len() > 1, |d| {
                d.child(
                    div()
                        .px_3p5()
                        .py_1p5()
                        .bg(theme::header())
                        .border_b_1()
                        .border_color(theme::border())
                        .text_size(px(11.5))
                        .text_color(theme::muted())
                        .child(format!("Session history: {}", session.lineage.join("  →  "))),
                )
            })
            .child(match chat {
                Some(chat) => div().flex().flex_col().flex_1().min_h_0().child(chat),
                None => div().flex_1().min_h_0().when_some(terminal, |d, t| d.child(t)),
            })
            // In the Terminal view, what the agent asks Glasscode to do shows under it.
            .when(!chat_view, |d| {
                let asks: Vec<AnyElement> = self.control.pending_for(&session.id).into_iter().map(|a| crate::today_ui::render_approval(a, true, cx)).collect();
                d.when(!asks.is_empty(), |d| d.child(div().flex().flex_col().gap_2().p_3().bg(theme::header()).border_t_1().border_color(theme::border()).children(asks)))
            })
            .when_some(exited, |d, code| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3p5()
                        .py_2()
                        .bg(theme::header())
                        .border_t_1()
                        .border_color(theme::border())
                        .child(
                            div()
                                .flex_1()
                                .text_color(theme::muted())
                                .child(format!("{} exited ({code}).", session.agent.name())),
                        )
                        .child(ui::primary_button("restart", "Resume").on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| this.restart_session(&restart_id, window, cx),
                        )))
                        .child(ui::button("close-ended", "Close tab").on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| this.close_session(&close_id, window, cx),
                        ))),
                )
            })
            .into_any_element()
    }

    fn render_session_header(
        &self,
        session: &Session,
        account: Option<&glasscode_core::model::Account>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let status = self.activity.get(&session.id).cloned();
        let menu = if self.menu_open && account.is_some() {
            Some(self.render_account_menu(session, cx))
        } else {
            None
        };
        let other = session.agent.other();
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap_x_3()
            .gap_y_2()
            .px_3p5()
            .py_2()
            .bg(theme::header())
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(ui::dot(theme::agent_color(session.agent)))
                    .child(session.agent.name()),
            )
            .when_some(account, |d, account| {
                d.child(
                    div()
                        .relative()
                        .flex_none()
                        .child(
                            ui::button("account-menu", format!("{}  ▾", account.display())).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.menu_open = !this.menu_open;
                                    cx.notify();
                                }),
                            ),
                        )
                        .when_some(menu, |d, menu| d.child(menu)),
                )
            })
            .child(
                div()
                    .min_w(px(60.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_family(theme::MONO_FONT)
                    .text_size(px(12.))
                    .text_color(theme::text_soft())
                    .child(format!("⎇ {}", session.branch)),
            )
            // The Linear ticket this session works on: opens it in Tickets.
            .when_some(self.ticket_for_session(session).map(|t| (t.identifier.clone(), t.status.name.clone())), |d, (id, status)| {
                let key = crate::tickets_ui::Key::Ticket(id.clone());
                d.child(
                    div()
                        .id("session-ticket")
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1p5()
                        .h(px(24.))
                        .px_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::border_strong())
                        .text_size(px(12.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_ticket(key.clone(), cx)))
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(id))
                        .child(div().text_color(theme::muted()).child(status)),
                )
            })
            .when(session.review.is_some(), |d| {
                d.child(div().flex_none().px_1p5().py_0p5().rounded(px(5.)).bg(theme::tag_bg()).text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_soft()).child("Review"))
            })
            // Read it as a chat (the Claude app's look) or see the agent's own terminal.
            .when(session.agent != AgentKind::Shell, |d| {
                let view = self.session_view(session);
                let (chat_id, term_id) = (session.id.clone(), session.id.clone());
                d.child(
                    ui::segmented()
                        .flex_none()
                        .child(ui::segment("view-chat", view == glasscode_core::model::SessionView::Chat, "Chat").h(px(24.)).px_2p5().on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.set_session_view(&chat_id, glasscode_core::model::SessionView::Chat, window, cx)
                        })))
                        .child(ui::segment("view-terminal", view == glasscode_core::model::SessionView::Terminal, "Terminal").h(px(24.)).px_2p5().on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.set_session_view(&term_id, glasscode_core::model::SessionView::Terminal, window, cx)
                        }))),
                )
            })
            .when_some(status, |d, status| {
                d.child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_size(px(11.5))
                        .text_color(match status {
                            AgentActivity::Working => theme::added(),
                            AgentActivity::NeedsInput => theme::attention(),
                            AgentActivity::Idle => theme::muted(),
                        })
                        .child(match status {
                            AgentActivity::Working => "● working",
                            AgentActivity::NeedsInput => "● needs your input",
                            AgentActivity::Idle => "● idle",
                        }),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    // The session's tab in the Glasscode Browser, brought to the front for the person.
                    .when(self.browser.session_tabs.contains_key(&session.id), |d| {
                        let id = session.id.clone();
                        d.child(
                            ui::button("session-browser", "◍ Browser")
                                .flex_none()
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.show_session_tab(&id, cx))),
                        )
                    })
                    .child(
                        ui::button("handoff", "⇄  Hand off…").flex_none().on_click(
                            cx.listener(move |this, _: &ClickEvent, window, cx| this.open_handoff(Some(other), None, window, cx)),
                        ),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .child(ui::button("session-more", "⋯").w(px(34.)).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.session_more = !this.session_more;
                                cx.notify();
                            })))
                            .when(self.session_more, |d| d.child(self.render_session_more(session, cx))),
                    )
                    .child(
                        ui::button("toggle-right", if self.show_right { "▥" } else { "▢" })
                            .w(px(34.))
                            .flex_none()
                            .when(self.show_right, |b| b.bg(theme::selected()))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.show_right = !this.show_right;
                                cx.notify();
                            })),
                    ),
            )
    }

    /// The header's ⋯ menu: what is done with a session less often.
    fn render_session_more(&self, session: &Session, cx: &mut Context<Self>) -> AnyElement {
        let item = |id: &'static str, label: String| {
            div()
                .id(id)
                .px_2p5()
                .py_2()
                .rounded_md()
                .text_size(px(13.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(label)
        };
        let (move_id, close_id) = (session.id.clone(), session.id.clone());
        let conversation = session.agent_session_id.clone();
        deferred(
            div()
                .id("session-more-menu")
                .occlude()
                .absolute()
                .top(px(36.))
                .right_0()
                .w(px(240.))
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
                .when(self.server.connected(), |d| {
                    d.child(item("more-move", format!("⇡ Move to {}", self.server.name())).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.session_more = false;
                        this.move_one(crate::switch_ui::Direction::ToServer, move_id.clone(), cx)
                    })))
                })
                .child(item("more-editor", format!("Open in {}", self.preferred_editor().map(|(name, _)| name).unwrap_or("Finder"))).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.session_more = false;
                    this.open_in_editor(None, cx)
                })))
                .when_some(conversation, |d, id| {
                    d.child(item("more-copy-id", format!("Copy conversation id ({})", &id[..8.min(id.len())])).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
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
                        .text_size(px(12.))
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
                        .h(px(32.))
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
                                .text_size(px(11.))
                                .text_color(theme::muted())
                                .child(account.plan.clone().unwrap_or_default()),
                        )
                        .child(
                            div()
                                .ml_auto()
                                .text_size(px(11.))
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
                .h(px(32.))
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
                .top(px(36.))
                .left_0()
                .w(px(320.))
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

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .child(format!("⎇ {}", session.map(|s| s.branch.clone()).unwrap_or_default())),
            )
            .child(if self.changes.is_empty() {
                "Clean".to_string()
            } else {
                format!("{} changed", self.changes.len())
            })
            .when_some(session.and_then(|s| crate::github_accounts_ui::status_label(self, &s.project_id)), |d, label| d.child(label))
            .child(div().flex_1())
            .when(!cli_warning.is_empty(), |d| {
                d.child(div().text_color(theme::attention()).child(format!("⚠ {cli_warning}")))
            })
            .when(cli_warning.is_empty() && !cli_ok.is_empty(), |d| d.child(cli_ok))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(div().size(px(6.)).rounded_full().bg(theme::added()))
                    .child({
                        let n = self.state.sessions.len();
                        let synced = self.mirror.synced.is_some() && self.mirror.error.is_none();
                        format!("{n} {} saved · {}", if n == 1 { "session" } else { "sessions" }, if synced { "in sync on the server" } else { "restored on relaunch" })
                    }),
            )
            .when_some(account, |d, account| d.child(account))
    }
}
