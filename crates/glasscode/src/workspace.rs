//! The Glasscode window: projects sidebar, session tabs with terminals, and the Git panel.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    App, Context, Entity, FocusHandle, Focusable,
    MouseButton, PathPromptOptions, Subscription, Window, actions, div,
    prelude::*, px,
};
use glasscode_core::agents::{self, AgentActivity, EventsTail, RolloutTail};
use glasscode_core::git::{self, FileChange, Worktree};
use glasscode_core::model::{AgentKind, Session, StartupMode, State, new_id, now};
use glasscode_core::{store, versions};

use crate::dialogs::Modal;
use crate::terminal::{SpawnSpec, TerminalEvent, TerminalView};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;

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
        Quit,
        ToggleLeftPanel,
        GoBack,
        GoForward,
        OpenSettings
    ]
);

/// A key bound to an op (ops-7): `keymap.rs` binds op names with it, and it runs the op for the
/// person, as a button does (`Workspace::run_op`). `args` are the op's arguments as JSON.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = glasscode, no_json)]
pub struct RunOp {
    pub name: String,
    pub args: serde_json::Value,
}

pub fn bind_keys(cx: &mut App) {
    crate::keymap::bind(cx, None);
    crate::keymap::bind(cx, Some("Palette"));
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

/// Debug builds: `GLASSCODE_DEBUG_NO_AGENTS=1` keeps a screen check from starting Claude or Codex
/// for a session (rule 15 of the redesign plan: a check never runs an agent with a real login).
/// The session shows a line instead; shells start as always. Release builds ignore it.
pub fn no_agents() -> bool {
    cfg!(debug_assertions) && std::env::var("GLASSCODE_DEBUG_NO_AGENTS").is_ok_and(|v| v.trim() == "1")
}

/// Whether a session's program may start: a shell always, Claude and Codex unless `no_agents`.
pub fn may_start(agent: AgentKind, no_agents: bool) -> bool {
    agent == AgentKind::Shell || !no_agents
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightTab {
    /// The session's pull request: its state, reviews, preview and comments.
    PullRequest,
    /// The checks (CI) of the session's pull request, and what Glasscode does when they fail.
    Checks,
    /// The worktree's changed files and the commit box.
    Changes,
    /// The worktree as a tree; a click opens a file in a tab of the centre.
    Files,
    /// Glasscode, the voice copilot (macOS): the mirror of the glasses' lens.
    Glasscode,
}

pub struct Workspace {
    pub state: State,
    /// What the centre shows.
    pub view: crate::shell::View,
    /// The tabs of the centre and the one in front (`session_view` makes them).
    pub tabs: Vec<crate::shell::CenterTab>,
    pub tab: usize,
    /// The places back and forward go to.
    pub history: crate::shell::History,
    /// The status bar's popover that is open.
    pub popover: Option<crate::shell::Popover>,
    /// The window's width at the last frame: the sidebars narrow in a narrow window.
    pub(crate) window_width: f32,
    /// The focus of each sidebar edge (`Side` order), which the arrow keys move.
    pub(crate) edge_focus: [FocusHandle; 3],
    pub env: Arc<HashMap<String, String>>,
    pub terminals: HashMap<String, Entity<TerminalView>>,
    pub(crate) tails: HashMap<String, EventsTail>,
    /// Codex sessions' turns, from their rollouts (`platform::chat_status_fixes`).
    rollouts: HashMap<String, RolloutTail>,
    /// Claude sessions whose hooks have reported since their launch (`platform::chat_status_fixes`).
    hooked: HashSet<String>,
    /// Claude sessions that wait on a question before Claude starts: their NeedsInput is ours.
    startup_waits: HashSet<String>,
    launched_at: HashMap<String, SystemTime>,
    pub activity: HashMap<String, AgentActivity>,
    pub attention: HashSet<String>,
    pub worktrees: HashMap<String, Vec<Worktree>>,
    pub changes: Vec<FileChange>,
    pub changes_for: Option<PathBuf>,
    pub right_tab: RightTab,
    /// The right panel shows (kept as `Layout.right_hidden`; `shell::set_hidden` changes both).
    pub show_right: bool,
    pub commit_input: Entity<TextInput>,
    pub modal: Option<Modal>,
    pub row_menu: Option<crate::worktree_dialog::RowMenu>,
    pub menu_open: bool,
    pub toast: Option<(String, Instant)>,
    pub cli_status: Vec<versions::CliStatus>,
    pub glasscode: crate::glasscode_ui::GlasscodeState,
    /// The manager: the agent above every session (its page, bubble and rounds).
    pub manager: crate::manager_state::ManagerUi,
    pub prs: crate::pr_status::PrStatuses,
    pub center: crate::center::Center,
    pub server: crate::server_ui::ServerUi,
    pub update: crate::update_ui::UpdateUi,
    pub mirror: crate::mirror_ui::MirrorUi,
    /// The journal's part of the app (M14).
    pub journal: crate::journal_ui::JournalUi,
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
    /// Each project's `.glasscode/settings.json` as last read, and new worktrees' setup scripts.
    pub project_files: crate::project_setup::ProjectFiles,
    /// Closing a working session asked first: (its id, when).
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
    /// The UI kit page (debug hook `kit`).
    pub kit: Option<crate::kit::Kit>,
    /// The backdrop behind the screens, and its debug page (debug hook `backdrop`).
    pub backdrop: crate::backdrop::Backdrops,
    /// The first run, while it shows.
    pub first_run: Option<crate::first_run::FirstRun>,
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
        // The first run shows (no project, never finished or skipped, or its debug hook): the
        // person ticks the logins to use there, so none is taken by itself and no session is made
        // or started until it ends. After it, the logins are what the person chose (skipping takes
        // the system logins, see `first_run::close_state`). A state from before the first run
        // takes them as it did. Another debug hook opens its own screen instead of the flow, and
        // takes no login either.
        let debug_open = std::env::var("GLASSCODE_DEBUG_OPEN").ok().filter(|_| cfg!(debug_assertions));
        let hook_first_run = debug_open.as_deref().is_some_and(|v| v.starts_with("firstrun"));
        let first_run_due = crate::first_run::wanted(&state);
        let first_run = hook_first_run || (first_run_due && debug_open.is_none());
        if !state.first_run_done && !first_run_due && !hook_first_run {
            state.bootstrap_accounts(true);
        }
        let commit_input = cx.new(|cx| TextInput::new("Commit message", cx));
        let subscription = cx.subscribe(&commit_input, |this: &mut Self, _, event, cx| {
            if matches!(event, InputEvent::Submit) {
                this.commit(cx);
            }
        });
        let env = Arc::new(env);
        let cli_env = env.clone();
        // The right panel as the person left it.
        let right_tab = state.layout.right_tab.as_deref().and_then(crate::shell::right_tab_from).unwrap_or(RightTab::PullRequest);
        let show_right = !state.layout.right_hidden;
        let mut this = Self {
            state,
            view: crate::shell::View::default(),
            tabs: Vec::new(),
            tab: 0,
            history: Default::default(),
            popover: None,
            window_width: f32::from(window.viewport_size().width),
            edge_focus: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()],
            env,
            terminals: HashMap::new(),
            tails: HashMap::new(),
            rollouts: HashMap::new(),
            hooked: HashSet::new(),
            startup_waits: HashSet::new(),
            launched_at: HashMap::new(),
            activity: HashMap::new(),
            attention: HashSet::new(),
            worktrees: HashMap::new(),
            changes: Vec::new(),
            changes_for: None,
            right_tab,
            show_right,
            commit_input,
            modal: None,
            row_menu: None,
            menu_open: false,
            toast: None,
            cli_status: Vec::new(),
            glasscode: crate::glasscode_ui::GlasscodeState::new(),
            manager: crate::manager_state::ManagerUi::new(),
            prs: Default::default(),
            center: crate::center::Center::Session,
            server: crate::server_ui::ServerUi::new(),
            update: crate::update_ui::UpdateUi::new(),
            mirror: crate::mirror_ui::MirrorUi::new(),
            journal: Default::default(),
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
            project_files: Default::default(),
            palette: None,
            close_asked: None,
            usage: Default::default(),
            themes: Default::default(),
            chat: Default::default(),
            session_more: false,
            control: crate::control_ui::ControlUi::new(),
            today: crate::today_ui::TodayUi::new(cx),
            kit: None,
            backdrop: Default::default(),
            first_run: None,
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
        let wants_new_claude = !first_run
            && (this.state.startup == StartupMode::RestoreAndNewClaude || (this.state.sessions.is_empty() && this.server.config.is_none()));
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
        // Each project's own settings file, read in the background.
        this.project_files_refresh(cx);
        if let Some(id) = this.state.active_session.clone().filter(|_| !first_run) {
            this.ensure_terminal(&id, window, cx);
            this.focus_active(window, cx);
        }
        if first_run && !hook_first_run {
            this.open_first_run(crate::first_run::Step::Projects, cx);
        }
        this.save();
        this.server_start(cx);
        this.update_start();
        this.control_start(window, cx);
        this.journal_start(cx);
        // Going to sleep: the server hears it in one last sync (best effort).
        let weak = cx.entity().downgrade();
        let sleep = cx.on_system_sleep(move |cx| {
            let _ = weak.update(cx, |this, cx| this.mirror_before_sleep(cx));
        });
        this.keep_subscription(sleep);
        // However the app quits (menu, Dock, last window), a waiting update goes in place.
        cx.on_app_quit(|this: &mut Workspace, cx| {
            this.journal_quit();
            this.update_on_quit();
            // A quit that could not ask (U20) leaves a copy of each unsaved editor's text.
            this.keep_unsaved_text(cx);
            async {}
        })
        .detach();
        // The window's close button (and Alt+F4) asks first about text not saved in an editor;
        // it closes once the person answered (`answer_unsaved`).
        let workspace = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| workspace.update(cx, |this, cx| !this.ask_before_quitting(false, cx)).unwrap_or(true));

        // The Chat view on screen follows its session file closely.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(350)).await;
                if this
                    .update(cx, |this, cx| {
                        let begun = Instant::now();
                        this.control_scan();
                        this.chat_tick(cx);
                        crate::shell::frames::tick(1, begun.elapsed());
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
                let ticked = this.update(cx, |this, cx| {
                    let begun = Instant::now();
                    this.tick(cx);
                    crate::shell::frames::tick(2, begun.elapsed());
                });
                if ticked.is_err() {
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
                let ticked = this.update(cx, |this, cx| {
                    let begun = Instant::now();
                    this.glasscode_tick(cx);
                    crate::shell::frames::tick(0, begun.elapsed());
                });
                if ticked.is_err() {
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
                    this.open_row_menu(id, None, gpui::point(theme::sz(240.), theme::sz(120.)), cx);
                }
            }
            Some(mode) if mode.starts_with("palette") => this.open_palette_hook(mode, window, cx),
            Some("usage") => this.show_view(crate::shell::View::Usage, cx),
            Some("today") => this.show_today(cx),
            Some(mode) if mode.starts_with("firstrun") => this.open_first_run_hook(mode, cx),
            Some("themes") => this.open_appearance(cx),
            Some(mode) if mode.starts_with("kit") => this.open_kit(mode, cx),
            Some(mode) if mode.starts_with("backdrop") => this.open_backdrop_page(mode, cx),
            Some(mode) if mode.starts_with("voice") => this.open_voice_hook(mode, cx),
            Some("manager") => this.show_manager(window, cx),
            Some("manager-chat") => {
                this.manager.mode = crate::manager_state::Mode::Chat;
                this.show_manager(window, cx);
            }
            Some(mode) if mode.starts_with("settings") => {
                // Any of the 13 sections by its name, or a name from before (`Section::named`).
                let section = crate::settings_ui::Section::named(mode.strip_prefix("settings:").unwrap_or("")).unwrap_or_default();
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
                                            held: None,
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
                // The first changed file of the active session, in a tab of the centre (its diff
                // shows there), and in the Files tree. git runs in the background.
                this.right_tab = RightTab::Files;
                if let Some(worktree) = this.active_session().map(|s| s.worktree.clone()) {
                    let status = cx.background_spawn(async move { git::status(&worktree) });
                    cx.spawn(async move |this, cx| {
                        let Ok(changes) = status.await else { return };
                        let _ = this.update(cx, |this, cx| {
                            if let Some(path) = changes.first().map(|c| c.path.clone()) {
                                this.changes = changes;
                                this.gh.files.reveal(&path);
                                cx.notify();
                            }
                        });
                    })
                    .detach();
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
        let watching = self.modal.is_none() && self.palette.is_none() && self.view == crate::shell::View::Sessions && self.center == crate::center::Center::Session;
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
        // Its worktree is being moved: nothing starts until the move ends.
        if self.switch_moving(session_id) {
            return;
        }
        // Its worktree runs on the server now: it comes back first, then the session starts.
        if self.runs_elsewhere(session_id) {
            self.bring_worktree_home(session_id.to_string(), cx);
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
        // A new worktree's setup script comes first: it asks or runs, and the agent waits.
        if self.setup_first(&session, window, cx) {
            return;
        }
        if !may_start(session.agent, no_agents()) {
            // The session view says why (`session_view.rs`); nothing starts.
            log::warn!("GLASSCODE_DEBUG_NO_AGENTS: {} not started for session {}", session.agent.name(), session.id);
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
        // reach this app, where to leave requests when a sandbox keeps it from the socket, and
        // (on Windows) the folder of the command itself.
        launch.env.insert("GLASSCODE_SESSION_SIG".into(), self.control.signature(&session.id));
        launch.env.insert("GLASSCODE_CONTROL_SOCKET".into(), glasscode_core::control::socket_path().to_string_lossy().into_owned());
        launch.env.insert("GLASSCODE_WORKTREE".into(), session.worktree.to_string_lossy().into_owned());
        glasscode_core::cli::add_command_to_path(&mut launch.env);
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
        // A first prompt reaches the agent as an argument: its record comes first, so the
        // prompt's hook event finds whose words it is (the person's composer, else Glasscode's).
        if let Some(prompt) = session.initial_prompt.as_deref().filter(|prompt| launch.args.iter().any(|arg| arg == prompt)) {
            self.control_record_first_prompt(&session, prompt);
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
        // The person's keys here are this session's: an agent's prompts for the words ledger, a
        // shell's `glasscode` lines for who is calling.
        terminal.update(cx, |t, _| t.watch_input(session.id.clone(), session.agent == AgentKind::Shell));
        let id = session_id.to_string();
        let sub = cx.subscribe_in(&terminal, window, move |this, _, event, _window, cx| {
            match event {
                TerminalEvent::Bell => {
                    if this.state.active_session.as_deref() != Some(&id) {
                        this.attention.insert(id.clone());
                    }
                }
                TerminalEvent::Exited(status) => {
                    this.activity.remove(&id);
                    this.journal_turn_exit(&id, *status, cx);
                }
                TerminalEvent::TitleChanged => {}
                TerminalEvent::Notice(message) => this.show_toast(message.clone(), cx),
            }
            cx.notify();
        });
        self._subscriptions.push(sub);
        self.tails.insert(session_id.to_string(), EventsTail::new(session_id));
        self.journal_tail_started(session_id);
        self.launched_at.insert(session_id.to_string(), SystemTime::now());
        if crate::platform::chat_status_fixes() {
            // A new launch: Claude's first hook event is still to come.
            self.hooked.remove(session_id);
            self.startup_waits.remove(session_id);
            // A resumed Codex conversation is followed from here; a new one once it is found (`tick`).
            if session.agent == AgentKind::Codex {
                match session.transcript_path.as_deref().filter(|p| session.agent_session_id.is_some() && p.is_file()) {
                    Some(path) => self.rollouts.insert(session_id.to_string(), RolloutTail::resumed(path)),
                    None => self.rollouts.remove(session_id),
                };
            }
        }
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
        agents::remove_session_files(session_id);
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
        self.setup_forget(session_id);
        self.tails.remove(session_id);
        self.activity.remove(session_id);
        if crate::platform::chat_status_fixes() {
            self.rollouts.remove(session_id);
            self.hooked.remove(session_id);
            self.startup_waits.remove(session_id);
        }
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

    /// Make a session the active one. The view stays: something done in the background (an
    /// agent's command, the glasses) never takes the person out of Today or Settings. In
    /// Sessions, the session left goes on the back list.
    pub fn select_session(&mut self, session_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.show_session(session_id, false, window, cx);
    }

    /// The person goes to a session (its row, a notification, the palette): Sessions shows it,
    /// and the place left goes on the back list.
    pub fn open_session(&mut self, session_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.show_session(session_id, true, window, cx);
    }

    fn show_session(&mut self, session_id: &str, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.enter_session(session_id, open);
        self.today.navigated_at = Some(Instant::now());
        self.center = crate::center::Center::Session;
        // Its pull request is asked about now unless GitHub answered for it a moment ago.
        self.pr_session_opened(session_id, cx);
        self.state.active_session = Some(session_id.to_string());
        self.attention.remove(session_id);
        self.menu_open = false;
        if let Some(worktree) = self.state.session(session_id).map(|s| s.worktree.clone()) {
            self.last_in_worktree.insert(worktree, session_id.to_string());
        }
        self.dismiss_popups_for(session_id, cx);
        // The person goes to it: its project and worktree open in the sidebar, so that its row
        // shows. Something done in the background leaves the folds as they are.
        if open {
            if let Some((project_id, worktree)) = self.state.session(session_id).map(|s| (s.project_id.clone(), s.worktree.clone())) {
                crate::sidebar::reveal(&mut self.state.layout.folded, &project_id, &worktree);
                if let Some(project) = self.state.projects.iter_mut().find(|p| p.id == project_id) {
                    project.expanded = true;
                }
            }
        }
        self.ensure_terminal(session_id, window, cx);
        // Not while Today, a dialog or the palette is in front: the keyboard stays there.
        self.focus_active_if_watching(window, cx);
        if self.changes_for.as_ref() != self.active_session().map(|s| &s.worktree) {
            self.changes.clear();
            self.changes_for = None;
        }
        self.last_git_refresh = Instant::now() - Duration::from_secs(60);
        self.save();
        cx.notify();
    }

    /// A new session, made the active one; the view stays (see `select_session`).
    pub fn add_session(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        let id = session.id.clone();
        self.state.sessions.push(session);
        glasscode_core::activity::record_session();
        self.select_session(&id, window, cx);
    }

    /// A new session the person started (the New session dialog, a ticket): Sessions shows it.
    pub fn add_and_open_session(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        let id = session.id.clone();
        self.state.sessions.push(session);
        glasscode_core::activity::record_session();
        self.open_session(&id, window, cx);
    }

    pub fn close_session(&mut self, session_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        // The last session of a worktree whose files have unsaved changes asks first (U20); the
        // answer closes it.
        if self.ask_before_closing_session(session_id, cx) {
            return;
        }
        let Some(index) = self.state.sessions.iter().position(|s| s.id == session_id) else {
            return;
        };
        let closed = self.state.sessions.remove(index);
        self.stop_terminal(session_id);
        agents::remove_session_files(session_id);
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
        glasscode_core::activity::record_session();
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
        // The person asked for the next session: show it, from Today too.
        self.open_session(&id, window, cx);
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
        // Its files with unsaved changes come first (U20); this question follows the answer.
        if self.ask_before_removing_project(&project_id, true, cx) {
            return;
        }
        let Some(name) = self.state.project(&project_id).map(|p| p.name.clone()) else { return };
        let sessions = self.state.sessions.iter().filter(|s| s.project_id == project_id).count();
        self.modal = Some(crate::dialogs::Modal::ConfirmRemoveProject { project_id, name, sessions });
        let _ = window;
        cx.notify();
    }

    pub fn remove_project(&mut self, project_id: &str, cx: &mut Context<Self>) {
        // An agent's `project remove` with files not saved in an editor waits for the person (U20).
        if self.ask_before_removing_project(project_id, false, cx) {
            return;
        }
        let ids: Vec<String> = self
            .state
            .sessions
            .iter()
            .filter(|s| s.project_id == project_id)
            .map(|s| s.id.clone())
            .collect();
        for id in &ids {
            self.stop_terminal(id);
            glasscode_core::agents::remove_session_files(id);
            self.state.ticket_of.remove(id);
            self.state.custom_titles.remove(id);
        }
        self.state.sessions.retain(|s| s.project_id != project_id);
        self.state.projects.retain(|p| p.id != project_id);
        // Its folds in the sidebar go with it.
        let projects: Vec<&str> = self.state.projects.iter().map(|p| p.id.as_str()).collect();
        crate::sidebar::prune_folded(&mut self.state.layout.folded, &projects);
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
    /// It asks git nothing (the New session dialog draws it): the list is the last background
    /// read (`refresh_worktrees`); a folder without git, or one not read yet, is its root with the
    /// branch a session there knows, else none.
    pub fn project_worktrees(&self, project_id: &str) -> Vec<(PathBuf, String)> {
        let mut list: Vec<(PathBuf, String)> = self
            .worktrees
            .get(project_id)
            .map(|w| w.iter().map(|w| (w.path.clone(), w.branch.clone())).collect())
            .unwrap_or_default();
        if list.is_empty() {
            if let Some(project) = self.state.project(project_id) {
                let known = self.state.sessions.iter().find(|s| s.worktree == project.root).map(|s| s.branch.clone()).unwrap_or_default();
                list.push((project.root.clone(), known));
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
        self.journal_tick(cx);
        self.remember_right_tab();
        let mut dirty = false;
        let mut redraw = false;
        let ids: Vec<String> = self.tails.keys().cloned().collect();
        for id in ids {
            let Some(update) = self.tails.get_mut(&id).and_then(EventsTail::poll) else { continue };
            if self.journal.on {
                let offset = self.tails.get(&id).map(EventsTail::offset).unwrap_or_default();
                self.journal_hook_lines(&id, &update.lines, offset);
                self.journal_turn_hooks(&id, &update.lines, cx);
            }
            if crate::platform::chat_status_fixes() {
                // Its hooks run: Claude has started, and its activity is theirs from here.
                self.hooked.insert(id.clone());
                if self.startup_waits.remove(&id) && update.activity.is_none() && self.activity.get(&id) == Some(&AgentActivity::NeedsInput) {
                    self.activity.remove(&id);
                    redraw = true;
                }
            }
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
                        if crate::platform::chat_status_fixes() {
                            self.rollouts.insert(id.clone(), RolloutTail::new(&path));
                        }
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

        if crate::platform::chat_status_fixes() {
            redraw |= self.codex_turns_tick(cx);
            redraw |= self.claude_start_tick(cx);
        }

        if dirty {
            self.save();
            redraw = true;
        }
        if self.last_git_refresh.elapsed() > Duration::from_secs(3) {
            self.refresh_git(cx);
        }
        self.glasscode_tick(cx);
        self.manager_tick(cx);
        self.manager_home_tick(cx);
        self.manager_voice_tick(cx);
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

    /// Codex has no hooks: its sessions' activity comes from the turns in their rollouts, and
    /// tells the person what Claude's hook events tell. True when one changed.
    fn codex_turns_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        let ids: Vec<String> = self.rollouts.keys().cloned().collect();
        for id in ids {
            let running = self.terminals.get(&id).is_some_and(|t| t.read(cx).exit_code.is_none());
            let Some(rollout) = self.rollouts.get_mut(&id) else { continue };
            rollout.poll();
            let Some(now) = agents::codex_activity(rollout.turn, running) else { continue };
            // Only a change counts, so each turn tells "finished" once.
            let previous = self.activity.get(&id).copied();
            if previous != Some(now) {
                self.activity.insert(id.clone(), now);
                self.activity_changed(&id, previous, now, cx);
                changed = true;
            }
        }
        changed
    }

    /// A Claude session that waits on a question before Claude starts (the Chat view cannot show
    /// it) needs the person, until its first hook event. True when one changed.
    fn claude_start_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        let ids: Vec<String> = self.state.sessions.iter().filter(|s| s.agent == AgentKind::Claude && !self.hooked.contains(&s.id)).map(|s| s.id.clone()).collect();
        for id in ids {
            let Some((running, drawn, message_box)) = self.terminals.get(&id).map(|terminal| {
                let t = terminal.read(cx);
                let (drawn, message_box) = crate::chat_ui::claude_start_screen(&t.text_tail(60), &t.text_tail_typed(60));
                (t.exit_code.is_none(), drawn, message_box)
            }) else {
                continue;
            };
            let since_launch = self.launched_at.get(&id).and_then(|at| at.elapsed().ok()).unwrap_or_default();
            let waits = agents::ClaudeStart { since_launch, running, hooked: false, screen_drawn: drawn, message_box, waiting: self.startup_waits.contains(&id) }.waits();
            // Once each time: the "needs you" notification too.
            if waits && self.startup_waits.insert(id.clone()) {
                if self.state.active_session.as_deref() != Some(id.as_str()) {
                    self.attention.insert(id.clone());
                }
                let previous = self.activity.insert(id.clone(), AgentActivity::NeedsInput);
                self.activity_changed(&id, previous, AgentActivity::NeedsInput, cx);
                changed = true;
            } else if !waits && self.startup_waits.remove(&id) {
                // Started with hooks that do not report, or gone: no activity, as before.
                if self.activity.get(&id) == Some(&AgentActivity::NeedsInput) {
                    self.activity.remove(&id);
                }
                changed = true;
            }
        }
        changed
    }

    /// The session's Claude waits on a question before it starts (see `claude_start_tick`).
    pub(crate) fn waits_to_start(&self, id: &str) -> bool {
        crate::platform::chat_status_fixes() && self.startup_waits.contains(id)
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
        let (program, env) = (self.preferred_editor().map(|(_, program)| program), self.env.clone());
        // Out of the click handler that asked (`center::start_outside`).
        crate::center::start_or_toast(
            "Could not open editor",
            move || match program {
                Some(program) => glasscode_core::sys::command(program).arg(&worktree).arg(&target).envs(env.iter()).spawn().map(|_| ()),
                // No editor: the file in the program the system has for its type.
                None => glasscode_core::sys::open_path(&target),
            },
            cx,
        );
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

    /// Settings › Agents and accounts (the Accounts button).
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
            // A working agent: the first close only asks (unless that is switched off).
            let working = self.state.session(&id).is_some_and(|s| self.row_state(s) == crate::sidebar::RowState::Working);
            let confirmed = self.close_asked.take().is_some_and(|(asked, at)| asked == id && at.elapsed() < Duration::from_secs(5));
            if working && !confirmed && !self.state.prefs.confirm_close_off {
                self.close_asked = Some((id, Instant::now()));
                self.show_toast(format!("Its agent is working. Press {} again to close it anyway.", crate::keymap::label(&CloseSession)), cx);
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

    fn on_run_op(&mut self, action: &RunOp, window: &mut Window, cx: &mut Context<Self>) {
        self.run_op_named(&action.name, action.args.clone(), window, cx);
    }

    /// Run an op from inside the app (a button, a key bound to an op, a Jump command) for
    /// `caller`, which there is the person (`Caller::you()`): its handler's plan, by the same
    /// rules of Settings › Permissions as the `glasscode` command (the person may do everything),
    /// and its reply leaves the journal's record of the person's action (ops-21). `done` hears the
    /// reply.
    pub fn run_op(&mut self, op: &glasscode_core::ops::Op, caller: crate::control_ui::Caller, done: impl FnOnce(&mut Workspace, &glasscode_core::control::Reply) + 'static, window: &mut Window, cx: &mut Context<Self>) {
        // With the v2 engine the gate decides first, for every caller (M21.1).
        if self.control.engine_v2 {
            return self.control_gated(op.clone(), caller, Vec::new(), String::new(), Some(Box::new(done)), window, cx);
        }
        let plan = match self.op_plan(&caller, op) {
            Ok(plan) => plan,
            Err(reply) => {
                done(self, &reply);
                return;
            }
        };
        let op = op.clone();
        self.after_check(plan, window, cx, move |ws, plan, window, cx| {
            let plan = match plan {
                Ok(plan) => plan,
                Err(reply) => return done(ws, &crate::ops_run::with_op(reply, Some(op.name()), false)),
            };
            // A dry run is the v2 engine's: with v1 nothing that asks for one runs.
            if op.dry_run() {
                return done(ws, &glasscode_core::control::Reply::fail(glasscode_core::control::code::USAGE, "A dry run needs Manager v2's engine."));
            }
            let targets = glasscode_core::ops::targets(&op, &ws.world(&caller)).ok();
            let responder = crate::control_ui::Responder::for_op(caller, op, targets, &plan, done);
            ws.control_run(plan, responder, window, cx);
        });
    }

    /// [`Workspace::run_op`] for the person, by the op's name and its arguments as JSON (checked
    /// against its schema); what failed shows as a toast.
    pub fn run_op_named(&mut self, name: &str, args: serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        let op = match glasscode_core::ops::typed(name, args) {
            Ok(op) => op,
            Err(error) => return self.show_toast(error.text, cx),
        };
        self.run_op(
            &op,
            crate::control_ui::Caller::you(),
            |ws, reply| {
                if let glasscode_core::control::Reply::Done { code, text, .. } = reply
                    && *code != glasscode_core::control::code::OK
                {
                    ws.toast = Some((text.clone(), Instant::now()));
                }
            },
            window,
            cx,
        );
        cx.notify();
    }

    fn on_handoff(&mut self, _: &Handoff, window: &mut Window, cx: &mut Context<Self>) {
        self.open_handoff(None, None, window, cx);
    }

    fn on_toggle_right(&mut self, _: &ToggleRightPanel, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_right(cx);
    }

    fn on_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        // Text not saved in an editor is asked about first (U20); the answer quits.
        if self.ask_before_quitting(false, cx) {
            return;
        }
        self.save();
        cx.quit();
    }
}

// ----- rendering ---------------------------------------------------------------------

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The glasses navigated to a session (desk mode) → open it here. Deferred from the
        // poll tick, which has no Window; render does. Guarded, so it runs once per change.
        if let Some(id) = self.glasscode.pending_open.take() {
            if self.state.active_session.as_deref() != Some(id.as_str()) {
                self.select_session(&id, window, cx);
            }
        }
        let shell = self.render_shell(window, cx);
        // "Save the changes?" (U20) over every view, as it can come from Quit or a session's ×.
        let unsaved = self.render_unsaved_question(window, cx);
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            // Esc ends the voice screen from anywhere in the window while it shows.
            .when(self.voice_shows(), |d| d.on_key_down(cx.listener(Self::voice_escape)))
            .on_action(cx.listener(Self::on_new_session))
            .on_action(cx.listener(Self::on_close_session))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_previous))
            .on_action(cx.listener(Self::on_accounts))
            .on_action(cx.listener(Self::on_add_project))
            .on_action(cx.listener(Self::on_handoff))
            .on_action(cx.listener(Self::on_run_op))
            .on_action(cx.listener(Self::on_toggle_right))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(|this, _: &OpenPalette, window, cx| this.open_palette(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleLeftPanel, _, cx| this.toggle_left(cx)))
            .on_action(cx.listener(|this, _: &GoBack, window, cx| this.go_back(window, cx)))
            .on_action(cx.listener(|this, _: &GoForward, window, cx| this.go_forward(window, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.go(crate::shell::View::Settings, window, cx)))
            .when(self.first_run.is_some(), crate::first_run::hold_keys)
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| this.resize_move(event, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| this.resize_end(cx)))
            .size_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family(theme::UI_FONT)
            .text_size(px(theme::ui_text_size()))
            .child(shell)
            .children(unsaved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `GLASSCODE_DEBUG_NO_AGENTS` stops Claude and Codex, never a shell.
    #[test]
    fn the_debug_switch_stops_agents_and_not_shells() {
        assert!(may_start(AgentKind::Shell, true));
        assert!(!may_start(AgentKind::Claude, true) && !may_start(AgentKind::Codex, true));
        assert!(may_start(AgentKind::Claude, false) && may_start(AgentKind::Codex, false) && may_start(AgentKind::Shell, false));
    }
}
