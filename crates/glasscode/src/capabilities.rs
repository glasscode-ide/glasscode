//! What the person can do in the app, one row per capability (M33.1; ops-1, ops-2): the table the
//! manager's reach is measured against. Each row is in one class:
//!
//! - `Op`: the control does what a registered op does (`glasscode_core::ops`), so the manager can
//!   do it too;
//! - `Planned`: an op the plan adds later; until then the manager says it cannot do that yet;
//! - `PersonOnly`: only the person does it (a secret, a sign-in, quitting), with the sentence the
//!   manager says and the place it opens for them;
//! - `ViewLocal`: hover, scroll, fold, caret and text editing, which mean nothing to anyone else.
//!
//! The tests below hold the table to the app with no window: every op is in a row and every `Op`
//! row names one; every card of Settings maps to a settings key or has only actions; every view,
//! Settings section and panel tab has a `Place`; every GPUI action and every Jump command has a
//! row; and every row names a function of the app that does it. A baseline of the raw click
//! handlers per file (`capabilities_baseline.txt`) may only go down, unless the same change adds
//! rows here, so a new control comes with its row.
//!
//! Buttons and ops share their handlers today; buttons move to `Workspace::run_op` screen by
//! screen, so the person's own actions reach the journal too.
//!
//! Until M33.2's `ui.open` reads places and the manager reads [`cannot_do`], only the tests read
//! this table.
#![cfg_attr(not(test), allow(dead_code))]

use crate::settings_ui::{CardId, Section};
use crate::shell::View;
use crate::workspace::RightTab;

/// Where the app can take the person (and, from M33.2, `ui.open <place>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    View(View),
    Settings(Section),
    /// A tab of a session's side panel.
    Tab(RightTab),
}

impl Place {
    /// "today", "settings:automation", "tab:changes".
    pub fn name(self) -> String {
        match self {
            Place::View(view) => view_name(view).to_string(),
            Place::Settings(section) => format!("settings:{}", section.slug()),
            Place::Tab(tab) => format!("tab:{}", tab_name(tab)),
        }
    }

    /// A place by its name (as [`Place::name`] writes it).
    pub fn parse(text: &str) -> Option<Place> {
        let text = text.trim().to_lowercase();
        if let Some(section) = text.strip_prefix("settings:") {
            return Section::all().into_iter().find(|s| s.slug() == section).map(Place::Settings);
        }
        if let Some(tab) = text.strip_prefix("tab:") {
            return TABS.into_iter().find(|t| tab_name(*t) == tab).map(Place::Tab);
        }
        VIEWS.into_iter().find(|v| view_name(*v) == text).map(Place::View)
    }
}

/// Every view of the centre.
pub const VIEWS: [View; 8] = [View::Today, View::Sessions, View::Board, View::PullRequests, View::Usage, View::Settings, View::FirstRun, View::Manager];

/// Every tab of a session's side panel.
pub const TABS: [RightTab; 5] = [RightTab::PullRequest, RightTab::Checks, RightTab::Changes, RightTab::Files, RightTab::Glasscode];

/// The name of a view's place; a new view does not compile until it has one. The words are the
/// `glasscode open` ones where it has them ("tickets" is the Board).
fn view_name(view: View) -> &'static str {
    match view {
        View::Today => "today",
        View::Sessions => "sessions",
        View::Board => "tickets",
        View::PullRequests => "pull-requests",
        View::Usage => "usage",
        View::Settings => "settings",
        View::FirstRun => "first-run",
        View::Manager => "manager",
    }
}

/// The name of a tab's place (`manager show --tab`'s words where it has them).
fn tab_name(tab: RightTab) -> &'static str {
    match tab {
        RightTab::PullRequest => "pr",
        RightTab::Checks => "checks",
        RightTab::Changes => "changes",
        RightTab::Files => "files",
        RightTab::Glasscode => "glasscode",
    }
}

/// What a card of Settings changes: a key of the settings table (`settings.get`/`settings.set`,
/// M33.2), or nothing but its actions (buttons, lists, sign-ins), which have rows of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardKey {
    Key(&'static str),
    ActionOnly,
}

/// The settings key of each card; a new card does not compile until it has one.
pub fn card_key(card: CardId) -> CardKey {
    use CardKey::{ActionOnly, Key};
    match card {
        CardId::Startup => Key("general.startup"),
        CardId::NotifyNeeds => Key("notify.needs"),
        CardId::NotifyFinished => Key("notify.finished"),
        CardId::NotifyCi => Key("notify.ci"),
        CardId::NotifyTickets => Key("notify.tickets"),
        CardId::NotifySound => Key("notify.sound"),
        CardId::QuietHours => Key("notify.quiet"),
        CardId::AutoNames => Key("sessions.auto_names"),
        CardId::ConfirmClose => Key("sessions.confirm_close"),
        CardId::Editor => Key("general.editor"),
        // The manager's section draws its own groups (`manager_settings.rs`): its switch is a
        // key, its policy the person's alone (ops-3).
        CardId::Manager => Key("manager.enabled"),
        CardId::Theme => Key("appearance.theme"),
        CardId::Accent => Key("appearance.accent"),
        CardId::Backdrop => Key("appearance.backdrop"),
        CardId::Finish => Key("appearance.finish"),
        CardId::BackdropInSessions => Key("appearance.backdrop_in_sessions"),
        CardId::Scale => Key("appearance.scale"),
        CardId::TextSize => Key("appearance.text_size"),
        CardId::Density => Key("appearance.density"),
        CardId::UiFont => Key("appearance.ui_font"),
        CardId::CodeFont => Key("appearance.code_font"),
        CardId::ChatFont => Key("appearance.chat_font"),
        CardId::SessionView => Key("appearance.session_view"),
        CardId::Cursor => Key("appearance.cursor"),
        CardId::AgentAccounts => ActionOnly,
        CardId::GithubAccounts => ActionOnly,
        CardId::RuleRead => Key("permissions.read"),
        CardId::RuleBoard => Key("permissions.board"),
        CardId::RuleStart => Key("permissions.start"),
        CardId::RuleRename => Key("permissions.rename"),
        CardId::RuleSend => Key("permissions.send"),
        CardId::RuleImport => Key("permissions.import"),
        CardId::RuleClose => Key("permissions.close"),
        CardId::RuleRemove => Key("permissions.remove"),
        CardId::RuleMove => Key("permissions.move"),
        CardId::RuleTickets => Key("permissions.tickets"),
        CardId::RuleGithub => Key("permissions.github"),
        CardId::RuleShow => Key("permissions.show"),
        CardId::RuleSettings => Key("permissions.settings"),
        CardId::AgentsSkill => Key("agents.skill"),
        CardId::AgentsOutside => Key("agents.outside_allowed"),
        CardId::ControlStatus => ActionOnly,
        CardId::Projects => ActionOnly,
        CardId::FreshMain => Key("projects.fresh_main"),
        CardId::DeleteBranch => Key("projects.delete_branch"),
        // A project's own file (`.glasscode/settings.json`), set there.
        CardId::ProjectSetup => ActionOnly,
        CardId::ProjectBranch => ActionOnly,
        CardId::ProjectFolder => ActionOnly,
        CardId::ProjectAgent => ActionOnly,
        CardId::Linear => ActionOnly,
        CardId::FixCi => Key("automation.fix_ci"),
        CardId::FixConflicts => Key("automation.fix_conflicts"),
        CardId::FixTries => Key("automation.fix_tries"),
        CardId::CiWait => Key("automation.ci_wait"),
        CardId::AutoReview => Key("automation.auto_review"),
        CardId::MoveTicket => Key("automation.move_ticket"),
        CardId::AgentsLog => Key("automation.agents_log"),
        CardId::Morning => Key("automation.morning"),
        CardId::BrowserState => ActionOnly,
        // A file of the browser's own (`browser::set_agents_enabled`), the person's alone.
        CardId::BrowserAgents => ActionOnly,
        CardId::BrowserReset => ActionOnly,
        CardId::Server => ActionOnly,
        CardId::Phones => ActionOnly,
        CardId::Voice => ActionOnly,
        CardId::UpdateStatus => ActionOnly,
        CardId::Channel => Key("updates.channel"),
        CardId::InstallOnQuit => Key("updates.install_on_quit"),
        CardId::CliVersions => ActionOnly,
        CardId::Shortcuts => ActionOnly,
        CardId::GlasscodeFolder => ActionOnly,
        CardId::UnsavedCopies => ActionOnly,
        CardId::FirstRun => ActionOnly,
    }
}

/// What a row of the inventory is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// A registered op does it.
    Op(&'static str),
    /// An op the plan adds later (M33.2 and after).
    Planned(&'static str),
    /// Only the person: what the manager says, and where it takes them.
    PersonOnly { say: &'static str, place: Place },
    /// Hover, scroll, fold, caret, text editing.
    ViewLocal,
}

/// One capability of the app.
#[derive(Clone, Copy, Debug)]
pub struct Capability {
    /// The screen of `architecture.md`'s table it is on.
    pub screen: &'static str,
    /// The control: words on screen, `key <namespace>::<Action>` for a GPUI action (`::*` for the
    /// rest of a namespace), `jump <label>` for a Jump command.
    pub control: &'static str,
    /// The function of the app that does it.
    pub handler: &'static str,
    pub class: Class,
}

const fn op(screen: &'static str, control: &'static str, handler: &'static str, name: &'static str) -> Capability {
    Capability { screen, control, handler, class: Class::Op(name) }
}

const fn planned(screen: &'static str, control: &'static str, handler: &'static str, name: &'static str) -> Capability {
    Capability { screen, control, handler, class: Class::Planned(name) }
}

const fn person(screen: &'static str, control: &'static str, handler: &'static str, say: &'static str, place: Place) -> Capability {
    Capability { screen, control, handler, class: Class::PersonOnly { say, place } }
}

const fn local(screen: &'static str, control: &'static str, handler: &'static str) -> Capability {
    Capability { screen, control, handler, class: Class::ViewLocal }
}

const SETTINGS: Place = Place::View(View::Settings);
const ACCOUNTS: Place = Place::Settings(Section::Accounts);
const SERVER: Place = Place::Settings(Section::Server);

/// Every capability, by screen.
pub const INVENTORY: &[Capability] = &[
    // ----- shell, history, panels, title bar
    op("Shell", "key glasscode::NewSession", "open_new_session", "session.new"),
    op("Shell", "key glasscode::CloseSession", "close_session", "session.close"),
    op("Shell", "key glasscode::NextSession", "select_session", "ui.open"),
    op("Shell", "key glasscode::PreviousSession", "select_session", "ui.open"),
    op("Shell", "key glasscode::OpenAccounts", "open_accounts", "ui.open"),
    op("Shell", "key glasscode::AddProject", "prompt_add_project", "project.import"),
    op("Shell", "key glasscode::Handoff", "open_handoff", "session.hand_off"),
    op("Shell", "key glasscode::ToggleRightPanel", "toggle_right", "ui.panel"),
    op("Shell", "key glasscode::ToggleLeftPanel", "toggle_left", "ui.panel"),
    op("Shell", "key glasscode::ToggleManagerChat", "chat_overlay_set", "ui.panel"),
    local("Shell", "key glasscode::OpenPalette", "open_palette"),
    local("Shell", "key glasscode::PaletteUp", "palette_move"),
    local("Shell", "key glasscode::PaletteDown", "palette_move"),
    person("Shell", "key glasscode::Quit", "ask_before_quitting", "Only you can quit Glasscode.", Place::View(View::Sessions)),
    op("Shell", "key glasscode::GoBack", "go_back", "ui.back"),
    op("Shell", "key glasscode::GoForward", "go_forward", "ui.forward"),
    op("Shell", "key glasscode::OpenSettings", "go", "app.open"),
    // A key bound to an op name: it runs that op, whose own row is elsewhere.
    local("Shell", "key glasscode::RunOp", "run_op"),
    op("Shell", "the Server mode switch", "open_switch", "switch.move"),
    op("Shell", "what the switch would move", "open_switch", "switch.units"),
    op("Shell", "what stops a unit from moving", "open_switch", "switch.check"),
    op("Shell", "bring a worktree home", "bring_worktree_home", "worktree.move"),
    op("Shell", "who owns a worktree now", "runs_elsewhere", "worktree.owner"),
    local("Shell", "resize a sidebar", "resize_move"),
    // ----- colours, backdrop, scale: how the app looks, set in Settings › Appearance
    local("Backdrop and glass", "the moving backdrop", "apply_appearance"),
    // ----- tree sidebar
    op("Tree sidebar", "a session's row", "select_session", "session.open"),
    op("Tree sidebar", "rename a session", "open_rename", "session.rename"),
    op("Tree sidebar", "a worktree's menu: remove", "open_remove_worktrees", "worktree.remove"),
    op("Tree sidebar", "clean up worktrees", "open_remove_worktrees", "worktree.clean"),
    op("Tree sidebar", "a worktree's menu: open in the editor", "open_in_editor", "worktree.open"),
    planned("Tree sidebar", "pin a session", "toggle_pin", "worktree.pin"),
    local("Tree sidebar", "fold a project", "set_folded"),
    op("Tree sidebar", "the session list", "render_tree", "session.list"),
    op("Tree sidebar", "the worktrees of a project", "render_tree", "worktree.list"),
    // ----- Today
    op("Today", "the board", "render_today", "board.list"),
    op("Today", "a task's card", "today_open", "board.show"),
    op("Today", "the start box: a task", "today_send", "board.add"),
    op("Today", "Session or Ask: what Enter does in the start box", "chat_set_today_mode", "settings.set"),
    op("Today", "ask the manager from the start box", "today_enter", "manager.ask"),
    op("Today", "open the full conversation", "show_manager", "manager.show"),
    local("Today", "hide the answer under the start box", "today_start_box"),
    local("Today", "Activity: every event, or the turns and the work's only", "today_journal"),
    op("Today", "tick a task off", "today_apply", "board.done"),
    op("Today", "untick a task", "today_apply", "board.undo"),
    op("Today", "snooze", "today_apply", "board.snooze"),
    op("Today", "bring a snoozed task back", "today_apply", "board.unsnooze"),
    op("Today", "put a task in Focus", "today_apply", "board.pin"),
    op("Today", "take a task out of Focus", "today_apply", "board.unpin"),
    op("Today", "move a task to another list", "today_apply", "board.move"),
    op("Today", "a task's note", "today_apply", "board.note"),
    op("Today", "edit a task", "today_apply", "board.edit"),
    op("Today", "remove a task", "today_apply", "board.remove"),
    op("Today", "the recurring tasks", "render_today_right", "board.recurring"),
    op("Today", "the start box: a session", "today_send", "session.new"),
    op("Today", "what waits for you", "render_approval", "app.approvals"),
    op("Today", "the status of the app", "today_snapshot", "app.status"),
    op("Today", "the manager's brief", "today_snapshot", "manager.brief"),
    // ----- session view and tabs
    op("Session view", "the conversation", "render_chat", "session.read"),
    op("Session view", "the session's details", "render_right_panel", "session.show"),
    op("Session view", "the message box: send", "chat_composer", "session.send"),
    planned("Session view", "Chat or Terminal", "set_session_view", "session.view"),
    planned("Session view", "the model", "chat_composer", "session.model"),
    planned("Session view", "attach files", "chat_composer", "composer.attach"),
    op("Session view", "answer the agent's question", "chat_composer", "session.answer"),
    op("Session view", "stop the agent", "chat_composer", "session.interrupt"),
    op("Session view", "restart the session", "restart_session", "session.restart"),
    op("Session view", "move the session to the server", "move_one", "session.move"),
    op("Session view", "push and open a draft pull request", "render_right_panel", "pr.open_draft"),
    op("Session view", "switch the account", "switch_account", "session.hand_off"),
    op("Session view", "key composer::Submit", "chat_composer", "session.send"),
    op("Session view", "key composer::SubmitNow", "chat_composer", "session.send"),
    op("Session view", "key composer::CycleMode", "chat_composer", "session.mode"),
    op("Session view", "key chat::CycleMode", "chat_composer", "session.mode"),
    local("Session view", "key chat::CopySelection", "render_chat"),
    local("Session view", "key composer::*", "chat_composer"),
    local("Session view", "key terminal::*", "ensure_terminal"),
    op("Session view", "keys typed into the terminal", "ensure_terminal", "session.keys"),
    op("Session view", "tab: pull request", "render_pr_section", "pr.status"),
    op("Session view", "tab: checks, re-run", "render_checks_section", "pr.rerun"),
    op("Session view", "comment on the pull request", "render_pr_section", "pr.comment"),
    planned("Session view", "reply to a review thread", "render_pr_section", "pr.reply"),
    op("Session view", "the preview label", "render_pr_section", "pr.label"),
    planned("Session view", "tab: changes, stage a file", "toggle_stage", "git.stage"),
    planned("Session view", "tab: changes, unstage a file", "toggle_stage", "git.unstage"),
    planned("Session view", "tab: changes, stage all", "stage_all", "git.stage"),
    planned("Session view", "tab: changes, commit", "commit", "git.commit"),
    planned("Session view", "tab: files, open a file", "files_select", "file.open"),
    planned("Session view", "tab: files, mention a file", "files_mention", "composer.attach"),
    local("Session view", "tab: files, copy a path", "files_copy"),
    person("Session view", "tab: files, reveal in Finder", "reveal", "Only you can show a file in Finder.", Place::Tab(RightTab::Files)),
    op("Session view", "tab: Glasscode", "glasscode_toggle", "ui.panel"),
    // ----- file tabs and the editor
    planned("File tabs", "open a file at a line", "open_file_tab", "file.open"),
    planned("Editor", "key editor::Save", "file_tab_edit", "file.save"),
    local("Editor", "key editor::*", "file_tab_edit"),
    local("Text fields", "key text_input::*", "focus_handle_clone"),
    local("Text fields", "key text_area::*", "focus_handle_clone"),
    // ----- Board (tickets)
    op("Board", "the tickets", "render_tickets", "ticket.list"),
    op("Board", "a ticket", "open_ticket", "ticket.show"),
    op("Board", "work on a ticket", "work_on_ticket", "ticket.start"),
    op("Board", "a ticket's status", "ticket_move", "ticket.move"),
    op("Board", "comment on a ticket", "ticket_comment", "ticket.comment"),
    op("Board", "review a pull request", "work_on_pr", "pr.review"),
    person("Board", "connect Linear", "linear_connect", "Only you can connect Linear: it needs your API key.", Place::Settings(Section::Integrations)),
    person("Board", "disconnect Linear", "linear_disconnect", "Only you can disconnect Linear.", Place::Settings(Section::Integrations)),
    // ----- pull requests
    op("Pull requests", "the pull requests asking for your review", "render_prs", "pr.list"),
    op("Pull requests", "build on a pull request", "work_on_pr", "pr.build"),
    // ----- Jump
    op("Jump", "jump New session…", "open_new_session", "session.new"),
    op("Jump", "jump Go to Today", "open_today", "app.open"),
    op("Jump", "jump Go to the Board", "show_tickets", "app.open"),
    op("Jump", "jump Go to Pull requests", "show_prs", "ui.open"),
    op("Jump", "jump Go to Usage", "show_view", "app.open"),
    op("Jump", "jump Go back", "go_back", "ui.back"),
    op("Jump", "jump Go forward", "go_forward", "ui.forward"),
    op("Jump", "jump Open Settings", "go", "app.open"),
    op("Jump", "jump Change the theme", "open_appearance", "ui.open"),
    op("Jump", "jump Accounts", "open_settings", "ui.open"),
    op("Jump", "jump Linear settings", "open_linear_settings", "ui.open"),
    op("Jump", "jump GitHub account for a project", "open_settings", "ui.open"),
    op("Jump", "jump Add a project…", "prompt_add_project", "project.import"),
    op("Jump", "jump Hand off this session…", "open_handoff", "session.hand_off"),
    op("Jump", "jump Show or hide the left sidebar", "toggle_left", "ui.panel"),
    op("Jump", "jump Show or hide the side panel", "toggle_right", "ui.panel"),
    op("Jump", "jump Browser", "open_settings", "ui.open"),
    planned("Jump", "jump Server…", "open_server_dialog", "server.status"),
    planned("Jump", "jump Check for CLI updates", "open_settings", "update.check"),
    op("Jump", "jump Manager settings", "open_settings", "ui.open"),
    op("Jump", "jump Go to the manager", "go", "manager.show"),
    op("Jump", "jump Open or close the chat with the manager", "chat_overlay_set", "ui.panel"),
    op("Jump", "jump Stop the manager's answer", "chat_stop", "manager.stop"),
    op("Jump", "jump Today box: what Enter does", "chat_set_today_mode", "settings.set"),
    op("Jump", "a session, worktree, project or ticket found", "palette_run", "app.open"),
    // ----- Settings
    op("Settings", "a setting's switch or choice", "settings_card", "settings.set"),
    op("Settings", "a setting's value", "settings_card", "settings.get"),
    planned("Settings", "a theme, accent or backdrop", "set_appearance", "appearance.set"),
    planned("Settings", "hide or show a project", "set_project_hidden", "project.hide"),
    op("Settings", "a project's GitHub account", "set_project_github", "project.set_account"),
    op("Settings", "remove a project", "remove_project", "project.remove"),
    op("Settings", "the accounts", "render_settings", "account.list"),
    op("Settings", "what the glasscode command may do", "render_settings", "app.whoami"),
    op("Settings", "what agents did", "render_settings", "app.log"),
    op("Today", "an approval card: Approve, Deny", "control_decide", "approvals.decide"),
    op("Browser", "a session's tab: open, read, screenshot", "browser_read", "browser.read"),
    op("Browser", "a session's tab: click, type, press, select", "browser_gate", "browser.act"),
    op("Browser", "a session's tab: run script", "browser_gate", "browser.eval"),
    op("Today", "the approval cards", "render_approval", "approvals.list"),
    person("Today", "an approval card: Always allow this in <project>", "control_offer_always", "Only you can allow something always in a project.", Place::View(View::Today)),
    person("Today", "always allow? Allow always", "control_always_confirmed", "Only you can change a project's policy.", Place::View(View::Today)),
    local("Today", "always allow? Cancel", "close_modal"),
    local("Today", "an approval card: Show all lines", "render_approval"),
    op("Notification", "an approval: Review", "open_today", "ui.open"),
    person("Settings", "the policy changed: Import this change", "control_answer_policy", "Only you can import a change to a project's policy.", Place::Settings(Section::Projects)),
    person("Settings", "the policy changed: Keep what I approved", "control_answer_policy", "Only you can decide about a change to a project's policy.", Place::Settings(Section::Projects)),
    person("Settings", "add a Claude or Codex account", "add_account_from_form", "Only you can add an account: it signs in with your password.", ACCOUNTS),
    person("Settings", "sign in to an account", "start_login", "Only you can sign in to an account.", ACCOUNTS),
    person("Settings", "remove an account", "remove_account", "Only you can remove an account.", ACCOUNTS),
    person("Settings", "the default GitHub account", "set_github_default", "Only you can choose the default GitHub account.", ACCOUNTS),
    person("Settings", "the email a GitHub account commits with", "save_github_email", "Only you can change the email your commits carry.", ACCOUNTS),
    person("Settings", "approve a project's setup script", "setup_answer", "Only you can allow a setup script to run on this computer.", Place::Settings(Section::Projects)),
    person("Settings", "reset the browser", "reset_control", "Only you can reset the browser: it signs every site out.", Place::Settings(Section::Browser)),
    person("Settings", "connect to the server", "server_connect", "Only you can connect a server: it needs its address and key.", SERVER),
    person("Settings", "install the server", "server_install", "Only you can install Glasscode on a server.", SERVER),
    person("Settings", "sign in on the server", "server_login", "Only you can sign in on the server.", SERVER),
    planned("Settings", "disconnect from the server", "server_disconnect", "server.status"),
    planned("Settings", "copy a project to the server", "server_copy_project", "server.status"),
    person("Settings", "pair a phone", "start_pairing", "Only you can pair a phone: it shows a code to scan.", SERVER),
    person("Settings", "remove a phone", "phone_remove", "Only you can remove a paired phone.", SERVER),
    planned("Settings", "check for updates", "update_check_now", "update.check"),
    person("Settings", "restart to update", "update_restart", "Only you can restart Glasscode to update it.", Place::Settings(Section::Updates)),
    planned("Settings", "the update's status", "update_rows", "update.status"),
    person("Settings", "add a CLI version", "open_add_cli", "Only you can add a CLI version: the team approves each one.", Place::Settings(Section::Updates)),
    person("Settings", "run the first run again", "open_first_run", "Only you can run the first run again.", SETTINGS),
    op("Settings", "the manager's status", "settings_manager", "manager.status"),
    // ----- status bar, usage, resources, ports
    op("Status bar", "the usage of each account", "status_usage", "usage.get"),
    op("Usage", "the usage page", "render_usage", "app.open"),
    planned("Status bar", "processes and ports", "status_popover", "process.list"),
    planned("Status bar", "end a process", "end_question", "process.end"),
    planned("Status bar", "end the orphans", "end_orphans_question", "process.end"),
    // ----- notifications
    op("Notifications", "send the person a notification", "notify_session", "app.notify"),
    planned("Notifications", "the notifications", "notify_session", "notification.list"),
    planned("Notifications", "dismiss a notification", "dismiss_popups_for", "notification.dismiss"),
    local("Notifications", "a notification: snooze it for an hour (v2)", "snooze"),
    person("Notifications", "allow or deny a request", "control_decide", "Only you can allow what waits for you.", Place::View(View::Today)),
    op("Notifications", "the result of a request that waited", "control_decide", "app.wait"),
    op("Command", "glasscode cancel <id>", "control_cancel", "ops.cancel"),
    op("Command", "glasscode undo [<id>]", "control_undo", "ops.undo"),
    // ----- first run
    person("First run", "sign in and pick projects", "open_first_run", "Only you can go through the first run.", Place::View(View::FirstRun)),
    op("Manager", "a project's trust level and conditions, previewed then confirmed", "trust_change", "policy.set"),
    op("Manager", "a playbook on, off or approved, previewed then confirmed", "trust_change", "playbook.set"),
    op("Manager", "a schedule on, off or approved, previewed then confirmed", "trust_change", "schedule.set"),
    person("Manager", "delegate a decision to the manager in a project", "trust_change", "Only you delegate a decision: it is previewed, then counts once you confirm it.", Place::Settings(Section::Manager)),
    person("Manager", "take a delegated decision back", "trust_change", "Only you take a delegated decision back.", Place::Settings(Section::Manager)),
    person("Manager", "add a site a project's sessions may open", "trust_change", "Only you add a site sessions may open.", Place::Settings(Section::Manager)),
    person("Manager", "remove a site a project's sessions may open", "trust_change", "Only you remove a site sessions may open.", Place::Settings(Section::Manager)),
    op("Manager", "try a playbook on the last 7 days", "trust_try", "playbook.test"),
    person("Manager", "move in what agents already know", "open_move_in", "Only you can move in: it reads only what you tick, and you accept what it proposes.", Place::Settings(Section::Manager)),
    person("Manager", "try moving the old manager's data again", "migration_retry", "Only you can run the move of the old manager's data again: Settings › Manager has it while it fails.", Place::Settings(Section::Manager)),
    person("Manager", "use the server's copy when this Mac's clock went back", "clock_use_server", "Only you can choose the server's copy: what this Mac changed while its clock was wrong stays here.", Place::Settings(Section::Manager)),
    person("Manager", "read the ticked sources", "move_in_read", "Only you can move in.", Place::Settings(Section::Manager)),
    person("Manager", "accept or drop what moving in proposes", "move_in_accept", "Only you accept what moving in proposes.", Place::Settings(Section::Manager)),
    person("Manager", "undo a move-in", "move_in_undo", "Only you can undo a move-in.", Place::Settings(Section::Manager)),
    person("Manager", "the new manager's parts: memory at each session start", "engine_feature_set", "Only you switch the new manager's parts on or off.", Place::Settings(Section::Manager)),
    person("Manager", "the new manager's parts: session profiles", "engine_feature_set", "Only you switch the new manager's parts on or off.", Place::Settings(Section::Manager)),
    person("Manager", "the new manager's parts: every turn hook", "engine_feature_set", "Only you switch the new manager's parts on or off.", Place::Settings(Section::Manager)),
    person("Manager", "study the projects as part of moving in", "move_in_study_start", "Only you start the study: it runs a model on your plan.", Place::Settings(Section::Manager)),
    person("Manager", "move in from a Glasscode export", "move_in_from_export", "Only you choose an export to move in from.", Place::Settings(Section::Manager)),
    person("Manager", "try a paused study again", "move_in_study_step", "Only you can start the study again.", Place::Settings(Section::Manager)),
    person("Manager", "answer or skip a question of moving in", "move_in_answer", "Only you can answer what moving in asks.", Place::Settings(Section::Manager)),
    person("Manager", "export your data to Downloads", "data_export_now", "Only you can export your data.", Place::Settings(Section::Manager)),
    person("Manager", "import an export", "data_import_pick", "Only you can import an export.", Place::Settings(Section::Manager)),
    person("Manager", "see and confirm what uninstall removes", "data_uninstall_ask", "Only you can uninstall Glasscode.", Place::Settings(Section::Manager)),
    person("Manager", "set how often your data is exported by itself", "data_schedule_set", "Only you choose where your data goes.", Place::Settings(Section::Manager)),
    person("Manager", "give Claude its notes back (move out)", "data_move_out_now", "Only you move your notes out.", Place::Settings(Section::Manager)),
    person("Manager", "turn memory's learning on or off", "learning_set", "Only you decide whether memory learns from what you type.", Place::Settings(Section::Manager)),
    person("Manager", "open memory: what it keeps and what waits for you", "memory_open", "Only you open your memory page.", Place::Settings(Section::Manager)),
    person("Manager", "accept or reject a proposal for memory", "memory_change", "Only you decide what memory keeps.", Place::View(View::Manager)),
    person("Manager", "accept every proposal waiting for memory", "memory_change", "Only you decide what memory keeps.", Place::View(View::Manager)),
    person("Manager", "keep or discard an edit made by hand in the memory folder", "memory_change", "Only you decide what memory keeps.", Place::View(View::Manager)),
    person("Manager", "forget an entry of memory", "memory_change", "Only you decide what memory forgets.", Place::View(View::Manager)),
    // ----- voice
    person("Voice", "talk to the manager", "voice_open", "Only you can start a voice conversation: it uses your microphone.", Place::View(View::Manager)),
    person("Voice", "the manager's voice on or off", "manager_voice_toggle", "Only you can switch my voice on or off.", Place::View(View::Manager)),
    // ----- programs outside Glasscode
    person("Programs outside", "open a link or a folder", "start_outside", "Only you can open programs outside Glasscode.", Place::View(View::Sessions)),
    // ----- the manager
    op("Manager", "the manager's page", "show_manager", "manager.show"),
    op("Manager", "ask the manager", "manager_ask_as", "manager.ask"),
    // The v2 chat (M52.1): one thread, on the Manager page and under the Today box.
    op("Manager", "a chip in the chat that opens a session or a pull request", "chat_open", "ui.open"),
    op("Manager", "Stop the answer being written", "chat_stop", "manager.stop"),
    op("Manager", "the bubble: open or close the chat over the window", "render_manager_bubble", "ui.panel"),
    op("Manager", "close the chat over the window", "render_chat_overlay", "ui.panel"),
    op("Manager", "open the full conversation from the chat", "show_manager", "manager.show"),
    op("Manager", "send in the chat over the window", "chat_overlay_send", "manager.ask"),
    // M52.2: cards, Show me, Undo, the failure line's way out, and the About chip.
    op("Manager", "Allow, on a card in the chat", "render_chat_item", "approvals.decide"),
    op("Manager", "Deny, on a card in the chat", "render_chat_item", "approvals.decide"),
    op("Manager", "Show me: what the manager would have shown", "render_chat_item", "ui.open"),
    local("Manager", "Undo an action line (its inverse op, run as you)", "render_chat_item"),
    op("Manager", "Sign in, when the manager's account is signed out", "chat_recover", "ui.open"),
    op("Manager", "Open Updates, when its CLI version is not approved", "chat_recover", "ui.open"),
    op("Manager", "Manager settings, when its agent is missing or its cap is reached", "chat_recover", "ui.open"),
    op("Manager", "Try again, when an answer did not come", "chat_recover", "manager.ask"),
    local("Manager", "send the next message without where you are (the About chip)", "render_chat_overlay"),
    local("Manager", "the chat's line of why it cannot think", "render_chat_failure"),
    // ux-15's leftovers: the limit-reset hold, and the other agent as the way out.
    local("Manager", "Ask the other agent instead (its account), when the manager's own is out of usage or signed out", "chat_ask_other"),
    local("Manager", "a question held until the limit resets, asked again by itself then", "chat_held_tick"),
    // The chat's boxes (M52 leftovers): menus, attached text, editing a queued message, long answers.
    local("Manager", "a line of the “/” menu of a chat box: an action's words", "chat_menu_take"),
    local("Manager", "a line of the “@” menu of a chat box: a session, a pull request or a project", "chat_menu_take"),
    local("Manager", "key glasscode::ChatMenuUp", "chat_menu_move"),
    local("Manager", "key glasscode::ChatMenuDown", "chat_menu_move"),
    local("Manager", "Enter on the menu line Up and Down are on: its words in the box", "chat_menu_enter"),
    local("Manager", "open or close a run of action lines (“3 actions ▸”)", "chat_toggle_run"),
    local("Manager", "the “+ N selected lines” chip of a chat box", "render_chat_box_extras"),
    local("Manager", "send without the attached selected text", "render_chat_box_extras"),
    local("Manager", "Edit a queued message in the box", "chat_edit_queued"),
    local("Manager", "Esc in a chat box: its menu, or the edit, goes", "chat_box_cancel"),
    local("Manager", "Read more of a long answer in the chat over the window", "render_chat_item"),
    local("Manager", "Show less of a long answer", "render_chat_item"),
    op("Manager", "Useful, on a nudge in the chat", "render_chat_item", "manager.feedback"),
    op("Manager", "Not useful, on a nudge in the chat", "render_chat_item", "manager.feedback"),
    op("Manager", "Fewer like this, on a nudge in the chat", "render_chat_item", "manager.feedback"),
    local("Manager", "a topic of the conversation on the Manager page", "render_chat_topics"),
    person("Manager", "remove a queued message", "render_chat_thread", "Only you can take back a message you queued.", Place::View(View::Manager)),
    op("Manager", "what the manager says", "manager_voice_speak", "manager.say"),
    op("Manager", "the walk through today's update", "render_manager", "manager.tour"),
    op("Manager", "what it remembers", "render_manager", "manager.memory"),
    op("Manager", "leave it a note", "render_manager", "manager.remember"),
    op("Manager", "drop a note", "render_manager", "manager.forget"),
    op("Manager", "a scene it draws", "render_scene", "manager.scene"),
    op("Manager", "follow-ups", "render_manager", "manager.follow_up.list"),
    op("Manager", "add a follow-up", "render_manager", "manager.follow_up.add"),
    op("Manager", "close a follow-up", "render_manager", "manager.follow_up.done"),
    op("Manager", "drop a follow-up", "render_manager", "manager.follow_up.drop"),
    op("Manager", "the messages it drafted", "render_manager", "manager.message.list"),
    op("Manager", "draft a message", "render_manager", "manager.message.draft"),
    op("Manager", "allow a message", "render_manager", "manager.message.approve"),
    op("Manager", "reject a message", "render_manager", "manager.message.reject"),
    op("Manager", "the checks it runs", "render_manager", "manager.check.list"),
    op("Manager", "add a check", "render_manager", "manager.check.add"),
    op("Manager", "stop a check", "render_manager", "manager.check.remove"),
    op("Manager", "a token for a device's voice", "manager_voice_keys_check", "manager.voice_token"),
    // ----- what the manager reads and shows (the v2 engine's ops, M33.2)
    op("Session view", "the side panel's tab", "ui_seen", "ui.state"),
    op("Manager", "point at a session or a screen", "manager_show", "ui.point"),
    op("Manager", "say something", "manager_say", "ui.say"),
    op("Manager", "walk through today's update", "manager_tour", "ui.tour"),
    op("Manager", "draw a scene", "manager_scene", "ui.scene"),
    op("Session view", "tab: pull request, its details", "render_pr_section", "pr.view"),
    op("Session view", "tab: checks", "render_checks_section", "pr.checks"),
    op("Session view", "tab: pull request, its diff", "render_pr_section", "pr.diff"),
    op("Session view", "tab: pull request, its review threads", "render_pr_section", "review.threads"),
    op("Session view", "tab: checks, a failed run's log", "render_checks_section", "ci.log"),
    op("Session view", "tab: changes, the files", "render_right_panel", "git.status"),
    op("Session view", "tab: changes, the commits", "render_right_panel", "git.log"),
    op("Session view", "tab: changes, a file's diff", "render_right_panel", "git.diff"),
    op("File tabs", "a file's text", "open_file_tab", "file.read"),
    person("Settings", "agents may use the browser", "agents_switch", "Only you choose whether agents use the browser.", Place::Settings(Section::Browser)),
    // ----- the glasscode command only (no control of its own)
    op("Command", "glasscode project list", "control_plan", "project.list"),
    op("Command", "glasscode worktree new", "control_plan", "worktree.new"),
    op("Command", "glasscode worktree rename", "control_plan", "worktree.rename"),
    op("Command", "glasscode open <session>", "control_plan", "session.open"),
    op("Command", "glasscode words recent", "control_plan", "words.recent"),
    op("Command", "glasscode events [--follow]", "control_plan", "journal.events"),
    op("Command", "glasscode journal query", "control_plan", "journal.query"),
    op("Command", "glasscode report done|blocked|needs-review", "control_plan", "session.report"),
    op("Command", "glasscode manager wake", "control_plan", "manager.wake"),
    op("Command", "glasscode playbook check", "control_plan", "playbook.check"),
    op("Command", "glasscode pr ready", "control_plan", "pr.ready"),
    op("Command", "glasscode schedule list", "control_plan", "schedule.list"),
    op("Command", "glasscode schedule add", "control_plan", "schedule.add"),
    op("Command", "glasscode schedule remove", "control_plan", "schedule.remove"),
    person("Today", "work intake found: Start", "intake_start_suggestion", "Only you start work intake left for you.", Place::View(View::Today)),
    op("Command", "glasscode session child", "control_plan", "session.child"),
    op("Command", "glasscode playbook test <id>", "control_plan", "playbook.test"),
    op("Command", "glasscode memory search", "control_plan", "memory.search"),
    op("Command", "glasscode memory show", "control_plan", "memory.show"),
    op("Command", "glasscode memory brief", "control_plan", "memory.brief"),
    op("Command", "glasscode memory note", "control_plan", "memory.note"),
    op("Command", "glasscode memory propose", "control_plan", "memory.propose"),
    op("Command", "glasscode memory accept", "control_plan", "memory.accept"),
    op("Command", "glasscode memory reject", "control_plan", "memory.reject"),
    op("Command", "glasscode memory merge", "control_plan", "memory.merge"),
    op("Command", "glasscode memory let-go", "control_plan", "memory.let_go"),
    op("Command", "glasscode memory rewrite", "control_plan", "memory.rewrite"),
    op("Command", "glasscode memory forget", "control_plan", "memory.forget"),
    op("Command", "glasscode memory pending", "control_plan", "memory.pending"),
    op("Command", "glasscode work list", "control_plan", "work.list"),
    op("Command", "glasscode work show", "control_plan", "work.show"),
    op("Command", "glasscode work done (for you only)", "control_plan", "work.done"),
    op("Command", "glasscode work drop (for you only)", "control_plan", "work.drop"),
    op("Command", "glasscode work reopen (for you only)", "control_plan", "work.reopen"),
    op("Command", "glasscode data export", "control_plan", "data.export"),
    op("Command", "glasscode data import", "control_plan", "data.import"),
    op("Command", "glasscode data schedule", "control_plan", "data.schedule"),
    op("Command", "glasscode data move-out", "control_plan", "data.move_out"),
    op("Command", "glasscode data forget", "control_plan", "data.forget"),
    op("Command", "glasscode data uninstall", "control_plan", "data.uninstall"),
];

/// What the manager cannot do, in its own words, by screen (design section 11): the person-only
/// rows, then the ones that wait for an op.
pub fn cannot_do() -> String {
    let mut lines = Vec::new();
    for row in INVENTORY {
        match row.class {
            Class::PersonOnly { say, place } => lines.push(format!("{}: {} ({say} It opens {}.)", row.screen, row.control, place.name())),
            Class::Planned(name) => lines.push(format!("{}: {} (not yet: it waits for {name})", row.screen, row.control)),
            Class::Op(_) | Class::ViewLocal => {}
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use glasscode_core::ops::{self, OPS};

    use super::*;

    /// The app's source files, by name relative to `src/`, but this one (it names what it scans for).
    fn sources() -> BTreeMap<String, String> {
        fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut BTreeMap<String, String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, base, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let name = path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                    if name != "capabilities.rs" {
                        out.insert(name, std::fs::read_to_string(&path).unwrap());
                    }
                }
            }
        }
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = BTreeMap::new();
        walk(&base, &base, &mut out);
        out
    }

    /// Every op is in a row, and every `Op` row names a registered op; a planned op is not
    /// registered yet (once it is, its rows become `Op` rows).
    #[test]
    fn every_op_has_a_row_and_every_row_an_op() {
        let named: BTreeSet<&str> = INVENTORY.iter().filter_map(|row| if let Class::Op(name) = row.class { Some(name) } else { None }).collect();
        for name in &named {
            assert!(ops::find(name).is_some(), "the inventory names {name}, which is no op");
        }
        for op in OPS {
            assert!(named.contains(op.name), "{} has no row in the inventory", op.name);
        }
        for row in INVENTORY {
            if let Class::Planned(name) = row.class {
                assert!(ops::find(name).is_none(), "{name} is registered now: make “{}” an Op row", row.control);
                assert!(name.contains('.') && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'), "{name}");
            }
        }
    }

    /// Every card of Settings has a settings key or only actions, and no two cards share a key.
    #[test]
    fn every_card_has_a_key_or_only_actions() {
        let mut keys = BTreeSet::new();
        for card in crate::settings_ui::catalog() {
            if let CardKey::Key(key) = card_key(card.card) {
                assert!(keys.insert(key), "{key} is two cards'");
                assert!(key.contains('.') && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'), "{key}");
            }
        }
        assert!(keys.len() > 45, "{}", keys.len());
        for key in &keys {
            assert!(glasscode_core::settings::find(key).is_some(), "{key} is no key of the settings table");
        }
    }

    /// Every view, Settings section and side-panel tab has a place, and its name finds it again.
    #[test]
    fn every_view_section_and_tab_has_a_place() {
        let places: Vec<Place> = VIEWS.into_iter().map(Place::View).chain(Section::all().into_iter().map(Place::Settings)).chain(TABS.into_iter().map(Place::Tab)).collect();
        assert_eq!(places.len(), 8 + 13 + 5);
        let mut names = BTreeSet::new();
        for place in places {
            let name = place.name();
            assert!(names.insert(name.clone()), "{name} twice");
            assert_eq!(Place::parse(&name), Some(place), "{name}");
        }
        assert_eq!(Place::parse("settings:automation"), Some(Place::Settings(Section::Automation)));
        assert_eq!(Place::parse("nowhere"), None);
    }

    /// Every GPUI action (`actions!` and `#[derive(Action)]` in the app's source) and every Jump
    /// command has a row.
    #[test]
    fn every_action_and_jump_command_has_a_row() {
        let controls: BTreeSet<&str> = INVENTORY.iter().map(|row| row.control).collect();
        let all = sources();
        let mut actions = 0;
        for (file, text) in &all {
            let mut rest = text.as_str();
            while let Some(at) = rest.find("actions!(") {
                rest = &rest[at + "actions!(".len()..];
                let body = &rest[..rest.find(')').unwrap()];
                let (namespace, list) = body.split_once(',').unwrap();
                let namespace = namespace.trim();
                let list = list.trim().trim_start_matches('[').trim_end_matches(']');
                for action in list.split(',').map(str::trim).filter(|a| !a.is_empty()) {
                    actions += 1;
                    let own = format!("key {namespace}::{action}");
                    let rest_of = format!("key {namespace}::*");
                    assert!(controls.contains(own.as_str()) || controls.contains(rest_of.as_str()), "{file}: {own} has no row");
                }
            }
            // An action with data: `#[action(namespace = n)]` before its `pub struct Name`.
            let mut rest = text.as_str();
            while let Some(at) = rest.find("#[action(namespace = ") {
                rest = &rest[at + "#[action(namespace = ".len()..];
                let namespace = rest[..rest.find(')').unwrap()].split(',').next().unwrap_or_default().trim();
                let after = &rest[rest.find("pub struct ").unwrap() + "pub struct ".len()..];
                let name: String = after.chars().take_while(|c| c.is_alphanumeric()).collect();
                actions += 1;
                assert!(controls.contains(format!("key {namespace}::{name}").as_str()), "{file}: key {namespace}::{name} has no row");
            }
        }
        assert!(actions > 100, "{actions} actions");
        let mut jumps = 0;
        let mut rest = all["palette_ui.rs"].as_str();
        while let Some(at) = rest.find("command(\"") {
            rest = &rest[at + "command(\"".len()..];
            let label = &rest[..rest.find('"').unwrap()];
            jumps += 1;
            assert!(controls.contains(format!("jump {label}").as_str()), "the Jump command “{label}” has no row");
        }
        assert!(jumps > 15, "{jumps} Jump commands");
        // A wildcard row is for text editing and terminal keys only: they are view-local.
        for row in INVENTORY.iter().filter(|row| row.control.ends_with("::*")) {
            assert_eq!(row.class, Class::ViewLocal, "{}", row.control);
        }
    }

    /// Each row names a function of the app that does it, and no row is there twice.
    #[test]
    fn every_row_names_a_handler_of_the_app() {
        let all: String = sources().into_values().collect();
        let mut seen = BTreeSet::new();
        for row in INVENTORY {
            assert!(all.contains(&format!("fn {}(", row.handler)) || all.contains(&format!("fn {}<", row.handler)), "{}: no fn {}", row.control, row.handler);
            assert!(seen.insert((row.screen, row.control)), "{} {} twice", row.screen, row.control);
            if let Class::PersonOnly { say, .. } = row.class {
                assert!(say.starts_with("Only you") && say.ends_with('.'), "{say}");
            }
        }
    }

    /// The source-scan baseline: the raw click handlers (`.on_click(`, `cx.listener(`) of each file
    /// may only go down, unless the same change adds rows to the inventory (as many as it adds
    /// handlers). Lower the baseline when a screen moves to `run_op`.
    #[test]
    fn baseline_only_goes_down() {
        let baseline = include_str!("capabilities_baseline.txt");
        let mut lines = baseline.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#'));
        let rows_then: usize = lines.next().and_then(|l| l.strip_prefix("rows ")).and_then(|n| n.trim().parse().ok()).expect("the first line is “rows <n>”");
        let then: BTreeMap<&str, usize> = lines.map(|l| l.rsplit_once(' ').map(|(file, n)| (file, n.parse().expect("a count"))).expect("“<file> <count>”")).collect();
        let mut grown = 0;
        let mut now_all = BTreeMap::new();
        for (file, text) in sources() {
            let count = text.matches(".on_click(").count() + text.matches("cx.listener(").count();
            if count > 0 {
                now_all.insert(file.clone(), count);
            }
            grown += count.saturating_sub(then.get(file.as_str()).copied().unwrap_or(0));
        }
        let added = INVENTORY.len().saturating_sub(rows_then);
        let report: String = now_all.iter().map(|(file, n)| format!("{file} {n}\n")).collect();
        assert!(grown <= added, "{grown} new click handlers and {added} new inventory rows: add a row for each new control (and then this baseline):\nrows {}\n{report}", INVENTORY.len());
    }

    #[test]
    fn the_manager_says_what_it_cannot_do() {
        let text = cannot_do();
        assert!(text.contains("Only you can quit Glasscode.") && text.contains("It opens sessions."), "{text}");
        assert!(text.contains("not yet: it waits for git.commit"), "{text}");
        assert!(!text.contains("session.new"));
    }
}
