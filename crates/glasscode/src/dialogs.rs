//! Dialogs: new session, handoff and accounts.

use std::path::PathBuf;

use gpui::{
    AnyElement, ClickEvent, Context, Entity, FontWeight, SharedString, Window, deferred, div,
    prelude::*, px,
};
use glasscode_core::agents;
use glasscode_core::git;
use glasscode_core::handoff::{self, HandoffOptions};
use glasscode_core::history::{self, PastConversation};
use glasscode_core::model::{Account, AgentKind, Session, new_id, now};

use crate::terminal::{SpawnSpec, TerminalView};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

pub enum Modal {
    NewSession(NewSessionForm),
    Handoff(HandoffForm),
    RemoveWorktrees(crate::worktree_dialog::RemoveForm),
    Server(crate::server_ui::ServerForm),
    /// Server mode / Laptop mode; its state is `Workspace::switch` so it keeps going when hidden.
    Switch,
    /// The Glasscode Browser; its state is `Workspace::browser`.
    Browser,
    /// A session's new name.
    Rename(RenameForm),
    /// A session from a Linear ticket, or a review of a pull request that asks for one.
    StartTicket(crate::tickets_ui::StartForm),
    /// "Remove this project from Glasscode?" (its id and name, and how many sessions close).
    ConfirmRemoveProject { project_id: String, name: String, sessions: usize },
    /// Add a CLI on top of the built-in Claude Code and Codex.
    AddCli(AddCliForm),
    /// The policy file was changed outside Glasscode (M21.1): what changed, in plain words, and
    /// the live policy the person may import as it is.
    PolicyChanged { lines: Vec<String>, live: glasscode_core::gate::Policy },
    /// Moving in (M35); its state is `Workspace::movein`.
    MoveIn,
    /// What uninstall would remove (M25), to confirm.
    ConfirmUninstall { lines: Vec<String>, blocked: bool },
    /// "Always allow this in <project>" (M21.2): the policy change in plain words, which the person
    /// confirms a second time; then the card that asked is approved too.
    AlwaysAllow { approval: u64, project: glasscode_core::gate::ProjectKey, op: String, lines: Vec<String>, policy: glasscode_core::gate::Policy },
    /// A change of trust in Settings, previewed: written only with "Confirm" (M53).
    TrustChange(Box<glasscode_core::trust::Pending>),
}

pub struct RenameForm {
    pub session_id: String,
    pub input: Entity<TextInput>,
}

/// The "add a CLI" form: an npm package (quick), or a fully custom install.
pub struct AddCliForm {
    /// false = install from npm; true = custom install + version commands.
    pub custom: bool,
    pub npm_package: Entity<TextInput>,
    pub npm_bin: Entity<TextInput>,
    pub name: Entity<TextInput>,
    pub install: Entity<TextInput>,
    pub version: Entity<TextInput>,
}

pub struct NewSessionForm {
    pub project_id: String,
    pub new_worktree: bool,
    pub existing: Option<PathBuf>,
    /// Show every worktree to pick from; when the dialog was opened from a worktree row, only
    /// that one shows (with "Change").
    pub choose_worktree: bool,
    pub branch: Entity<TextInput>,
    pub name: Entity<TextInput>,
    pub agent: AgentKind,
    pub account_id: Option<String>,
    pub account_open: bool,
    pub busy: bool,
    pub error: Option<String>,
    /// Earlier conversations of the chosen worktree and agent (`None` while looking).
    pub history: Option<Vec<PastConversation>>,
    /// What `history` was loaded for; a change starts a new search.
    pub history_key: Option<(AgentKind, PathBuf)>,
    pub history_all: bool,
    /// The conversation to resume instead of starting a new one.
    pub resume: Option<PastConversation>,
    /// Run on the developer's server instead of this Mac.
    pub on_server: bool,
    /// Reserved for a review-session toggle; not used yet.
    pub review_mode: bool,
    /// Build: start from a pull request's branch instead of the latest main.
    pub from_pr: bool,
    /// The PR search box (number, title, branch or author).
    pub pr_query: Entity<TextInput>,
    /// What the search found (`None` while looking).
    pub pr_hits: Option<Vec<glasscode_core::github::PrHit>>,
    pub pr_error: Option<String>,
    /// The hits are the PRs asking for your review.
    pub pr_asked: bool,
    /// What `pr_hits` was searched for (project and words).
    pub pr_searched: Option<String>,
    pub pr_picked: Option<glasscode_core::github::PrHit>,
    /// The default branch, fetched when the dialog opened (this Mac).
    pub fresh: Option<git::FreshBase>,
    pub fresh_for: Option<PathBuf>,
}

pub struct HandoffForm {
    pub session_id: String,
    pub to_agent: AgentKind,
    pub to_account: Option<String>,
    pub include_terminal: bool,
    pub include_diff: bool,
    pub include_transcript: bool,
    pub new_tab: bool,
    pub note: Entity<TextInput>,
    pub account_open: bool,
}

pub struct AccountsForm {
    pub adding: Option<AgentKind>,
    pub label: Entity<TextInput>,
    pub plan: Entity<TextInput>,
    /// A GitHub account's commit email, while it is being edited.
    pub github_email: Entity<TextInput>,
    pub terminal: Option<(String, Entity<TerminalView>)>,
    /// Opened from Tickets: Linear comes first.
    pub linear_first: bool,
}

fn review_title(pr: &glasscode_core::github::PrHit) -> String {
    format!("#{} {}", pr.number, pr.title.chars().take(56).collect::<String>())
}

fn default_branch_name() -> String {
    format!("glasscode/session-{}", &new_id()[..4])
}

impl Modal {
    pub fn new_session(
        ws: &Workspace,
        project_id: String,
        worktree: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Modal {
        let branch = cx.new(|cx| TextInput::new("Branch name", cx).mono().with_text(default_branch_name()));
        let name = cx.new(|cx| TextInput::new("e.g. Fix flaky login test (optional)", cx));
        let pr_query = cx.new(|cx| TextInput::new("Find a pull request: number, title, branch or author", cx));
        cx.subscribe_in(&pr_query, window, |this: &mut Workspace, _, event, window, cx| match event {
            InputEvent::Submit => {
                let first = match this.modal.as_ref() {
                    Some(Modal::NewSession(f)) if f.pr_picked.is_none() => f.pr_hits.as_ref().and_then(|h| h.first().cloned()),
                    _ => None,
                };
                match first {
                    Some(hit) => this.pick_pr(Some(hit), cx),
                    None => this.create_session_from_form(window, cx),
                }
            }
            InputEvent::Cancel => this.close_modal(window, cx),
            InputEvent::Changed => {
                with_new_session_form(this, |f| f.pr_picked = None);
                this.pr_search_soon(cx);
            }
        })
        .detach();
        for input in [&branch, &name] {
            cx.subscribe_in(input, window, |this: &mut Workspace, _, event, window, cx| match event {
                InputEvent::Submit => this.create_session_from_form(window, cx),
                InputEvent::Cancel => this.close_modal(window, cx),
                InputEvent::Changed => {}
            })
            .detach();
        }
        crate::text_input::focus(&name, window, cx);
        // The project's own default agent (its .glasscode/settings.json), else Claude.
        let agent = ws.default_agent(&project_id, false).0;
        let account_id = ws.state.default_account(agent).map(|a| a.id.clone());
        // A folder without git: the session works in it.
        let plain = !ws.is_git_project(&project_id);
        let worktree = if plain { ws.state.project(&project_id).map(|p| p.root.clone()) } else { worktree };
        // Where the person last chose to run: the server, when it is connected and the project
        // is there too. Only without a worktree picked here (that one is on this Mac).
        let server_target = (worktree.is_none() && ws.state.prefs.starts_on_server(ws.server.connected())).then(|| server_counterpart(ws, &project_id)).flatten();
        let mut modal = Modal::NewSession(NewSessionForm {
            project_id,
            new_worktree: worktree.is_none(),
            choose_worktree: worktree.is_none() && !plain,
            existing: worktree,
            branch,
            name,
            agent,
            account_id,
            account_open: false,
            busy: false,
            error: None,
            history: None,
            history_key: None,
            history_all: false,
            resume: None,
            on_server: false,
            review_mode: false,
            from_pr: false,
            pr_query,
            pr_hits: None,
            pr_error: None,
            pr_asked: false,
            pr_searched: None,
            pr_picked: None,
            fresh: None,
            fresh_for: None,
        });
        if let (Some((project, account)), Modal::NewSession(form)) = (server_target, &mut modal) {
            aim_at_server(form, project, account);
        }
        modal
    }

    pub fn handoff(
        ws: &Workspace,
        session: &Session,
        to_agent: Option<AgentKind>,
        to_account: Option<String>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Modal {
        let to_agent = to_agent.unwrap_or(session.agent.other());
        let to_account = to_account.or_else(|| pick_account(ws, session, to_agent));
        let note = cx.new(|cx| TextInput::new("Anything the next agent should know (optional)", cx));
        cx.subscribe_in(&note, window, |this: &mut Workspace, _, event, window, cx| match event {
            InputEvent::Submit => this.perform_handoff(window, cx),
            InputEvent::Cancel => this.close_modal(window, cx),
            InputEvent::Changed => {}
        })
        .detach();
        crate::text_input::focus(&note, window, cx);
        let _ = ws;
        Modal::Handoff(HandoffForm {
            session_id: session.id.clone(),
            to_agent,
            to_account,
            include_terminal: true,
            include_diff: true,
            include_transcript: true,
            new_tab: false,
            note,
            account_open: false,
        })
    }

    pub fn rename(session_id: &str, current: &str, window: &mut Window, cx: &mut Context<Workspace>) -> Modal {
        let input = cx.new(|cx| TextInput::new("What this session is about", cx));
        input.update(cx, |i, cx| i.set_text(current.to_string(), cx));
        cx.subscribe_in(&input, window, |this: &mut Workspace, _, event, window, cx| match event {
            InputEvent::Submit => this.rename_from_form(window, cx),
            InputEvent::Cancel => this.close_modal(window, cx),
            InputEvent::Changed => {}
        })
        .detach();
        crate::text_input::focus(&input, window, cx);
        Modal::Rename(RenameForm { session_id: session_id.to_string(), input })
    }

    pub fn add_cli(window: &mut Window, cx: &mut Context<Workspace>) -> Modal {
        let npm_package = cx.new(|cx| TextInput::new("npm package, e.g. prettier or @scope/name", cx));
        let npm_bin = cx.new(|cx| TextInput::new("Command it installs (optional, if it differs)", cx));
        let name = cx.new(|cx| TextInput::new("Name, e.g. ripgrep", cx));
        let install = cx.new(|cx| TextInput::new(format!("Install command, e.g. {}", crate::platform::install_example()), cx));
        let version = cx.new(|cx| TextInput::new("Version command, e.g. rg --version", cx));
        for input in [&npm_package, &npm_bin, &name, &install, &version] {
            cx.subscribe_in(input, window, |this: &mut Workspace, _, event, window, cx| match event {
                InputEvent::Submit => this.add_cli_from_form(window, cx),
                InputEvent::Cancel => this.close_modal(window, cx),
                InputEvent::Changed => {}
            })
            .detach();
        }
        crate::text_input::focus(&npm_package, window, cx);
        Modal::AddCli(AddCliForm { custom: false, npm_package, npm_bin, name, install, version })
    }
}

impl AccountsForm {
    /// Settings' Accounts: the inputs for adding an account and a GitHub commit email.
    pub fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let label = cx.new(|cx| TextInput::new("Name, e.g. Work or Client X", cx));
        let plan = cx.new(|cx| TextInput::new("Plan, e.g. Max or Pro (optional)", cx));
        for input in [&label, &plan] {
            cx.subscribe_in(input, window, |this: &mut Workspace, _, event, window, cx| match event {
                InputEvent::Submit => this.add_account_from_form(window, cx),
                InputEvent::Cancel => {
                    if let Some(form) = this.settings.form.as_mut() {
                        form.adding = None;
                    }
                    cx.notify();
                }
                InputEvent::Changed => {}
            })
            .detach();
        }
        let github_email = cx.new(|cx| TextInput::new("Commit email, e.g. you@company.com (empty: your git email)", cx));
        cx.subscribe_in(&github_email, window, |this: &mut Workspace, input, event, _, cx| match event {
            InputEvent::Submit => {
                if let Some(login) = this.github.editing.clone() {
                    let text = input.read(cx).text();
                    this.save_github_email(&login, text, cx);
                }
            }
            InputEvent::Cancel => {
                this.github.editing = None;
                cx.notify();
            }
            InputEvent::Changed => {}
        })
        .detach();
        AccountsForm { adding: None, label, plan, github_email, terminal: None, linear_first: false }
    }
}

impl Modal {

    pub fn render(&self, ws: &Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
        let heading = match self {
            Modal::StartTicket(form) => Some((crate::tickets_ui::start_title(form), crate::tickets_ui::start_subtitle(ws, form))),
            _ => None,
        };
        let (title, subtitle, body, footer, wide) = match self {
            Modal::StartTicket(form) => ("", "", crate::tickets_ui::render_start(ws, form, cx), crate::tickets_ui::start_footer(form, cx), false),
            Modal::NewSession(form) => (
                "New session",
                "Pick what you are doing, where it runs and who drives it.",
                render_new_session(ws, form, cx),
                new_session_footer(form, cx),
                false,
            ),
            Modal::Handoff(form) => (
                "Continue with another agent or account",
                "Glasscode packs up this session's context and starts the next one with it.",
                render_handoff(ws, form, cx),
                handoff_footer(ws, form, cx),
                false,
            ),
            Modal::RemoveWorktrees(form) => (
                if form.single { "Remove worktree" } else { "Clean up worktrees" },
                if form.single {
                    "Deletes the worktree's folder. Glasscode checks first what would be lost."
                } else {
                    "Select the worktrees you no longer need. Each one is checked for unsaved work."
                },
                crate::worktree_dialog::render_remove(ws, form, cx),
                crate::worktree_dialog::remove_footer(form, cx),
                !form.single,
            ),
            Modal::Switch => match ws.switch.as_ref() {
                Some(form) => (
                    if form.direction == crate::switch_ui::Direction::ToServer { "Server mode" } else { "Laptop mode" },
                    if form.direction == crate::switch_ui::Direction::ToServer {
                        crate::platform::fixed_text!("Move every session to the server, so it keeps running while {} is closed.", crate::platform::this_computer())
                    } else {
                        crate::platform::fixed_text!("Bring every session back from the server to {}.", crate::platform::this_computer())
                    },
                    crate::switch_ui::render_switch(ws, form, cx),
                    crate::switch_ui::switch_footer(ws, form, cx),
                    true,
                ),
                None => ("Server mode", "", div().into_any_element(), div().into_any_element(), false),
            },
            Modal::Rename(form) => (
                "Rename session",
                "A name you give stays: Glasscode does not rename it. Leave it empty to let Glasscode name it again.",
                div().child(form.input.clone()).into_any_element(),
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(ui::button("rename-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx))))
                    .child(ui::primary_button("rename-go", "Rename").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.rename_from_form(window, cx))))
                    .into_any_element(),
                false,
            ),
            Modal::Browser => (
                "Browser",
                crate::platform::fixed_text!("The browser the agents on {} use, signed in once by you.", crate::platform::this_computer()),
                crate::browser_ui::render_browser_dialog(ws, window, cx),
                div()
                    .flex()
                    .justify_end()
                    .child(ui::primary_button("browser-done", "Done").on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.close_modal(window, cx),
                    )))
                    .into_any_element(),
                true,
            ),
            Modal::ConfirmRemoveProject { project_id, name, sessions } => {
                let (id, closing) = (project_id.clone(), *sessions);
                (
                    "Remove project from Glasscode?",
                    "Its folder and worktrees stay on disk; only Glasscode forgets it.",
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_color(theme::text_strong()).child(name.clone()))
                        .when(closing > 0, |d| d.child(div().text_size(theme::sz(13.)).text_color(theme::muted()).child(format!("{} open here will close.", if closing == 1 { "1 session".to_string() } else { format!("{closing} sessions") }))))
                        .into_any_element(),
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(ui::button("remove-project-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx))))
                        .child(ui::danger_button("remove-project-go", "Remove project").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.close_modal(window, cx);
                            this.remove_project(&id, cx);
                            this.show_toast("Removed the project from Glasscode. Its folder stays on disk.", cx);
                        })))
                        .into_any_element(),
                    false,
                )
            }
            Modal::PolicyChanged { lines, live } => {
                let live = live.clone();
                (
                    "The policy was changed outside Glasscode",
                    "Until you import the change, every project counts as Careful at most, and what you approved before still holds.",
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(if lines.is_empty() { vec!["The file differs from what you approved, or could not be read.".to_string()] } else { lines.clone() }.into_iter().map(|line| div().text_size(theme::sz(13.)).text_color(theme::text_strong()).child(line)))
                        .into_any_element(),
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(ui::button("policy-keep", "Keep what I approved").on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.close_modal(window, cx);
                            this.control_answer_policy(None, cx);
                        })))
                        .child(ui::danger_button("policy-import", "Import this change").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.close_modal(window, cx);
                            this.control_answer_policy(Some(live.clone()), cx);
                        })))
                        .into_any_element(),
                    false,
                )
            }
            Modal::AlwaysAllow { approval, project, op, lines, policy } => {
                let (id, policy) = (*approval, policy.clone());
                (
                    "Always allow this?",
                    "This changes the project's policy. Cards for what leaves this Mac or removes work still show.",
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_size(theme::sz(13.)).text_color(theme::text_soft()).child(format!("{op} in {project}, from now on, without asking:")))
                        .children(lines.clone().into_iter().map(|line| div().text_size(theme::sz(13.)).text_color(theme::text_strong()).child(line)))
                        .into_any_element(),
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(ui::button("always-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx))))
                        .child(ui::primary_button("always-confirm", "Allow always").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.close_modal(window, cx);
                            this.control_always_confirmed(id, policy.clone(), window, cx);
                        })))
                        .into_any_element(),
                    false,
                )
            }
            Modal::TrustChange(pending) => {
                let pending = (**pending).clone();
                let lines = pending.lines.clone();
                (
                    "Change trust?",
                    "Nothing changes until you confirm.",
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_size(theme::sz(13.)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(pending.title.clone()))
                        .children(lines.into_iter().map(|line| div().text_size(theme::sz(13.)).text_color(theme::text()).child(line)))
                        .into_any_element(),
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(ui::button("trust-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx))))
                        .child(ui::primary_button("trust-confirm", "Confirm").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.close_modal(window, cx);
                            this.trust_confirm(pending.clone(), cx);
                        })))
                        .into_any_element(),
                    false,
                )
            }
            Modal::MoveIn => match ws.movein.as_ref() {
                Some(form) => {
                    let (title, subtitle) = crate::movein_ui::heading(form);
                    (title, subtitle, crate::movein_ui::render(form, cx), crate::movein_ui::footer(form, cx), true)
                }
                None => ("Move in", "", div().into_any_element(), div().into_any_element(), false),
            },
            Modal::ConfirmUninstall { lines, blocked } => {
                let blocked = *blocked;
                (
                    "Uninstall Glasscode?",
                    if blocked { "A worktree has work that is nowhere else: push or remove it first." } else { "This removes the Glasscode folder: memory, settings, accounts' logins. Export first to keep your memory." },
                    div().flex().flex_col().gap_1().children(lines.clone().into_iter().map(|line| div().text_size(theme::sz(12.5)).text_color(theme::text_strong()).child(line))).into_any_element(),
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(ui::button("uninstall-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx))))
                        .when(!blocked, |d| {
                            d.child(ui::danger_button("uninstall-go", "Uninstall").on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.close_modal(window, cx);
                                this.data_uninstall_now(cx);
                            })))
                        })
                        .into_any_element(),
                    true,
                )
            }
            Modal::AddCli(form) => (
                "Add a CLI",
                "Claude Code and Codex are always here. Add any other CLI on top — from npm, or your own install.",
                render_add_cli(form, cx),
                add_cli_footer(cx),
                false,
            ),
            Modal::Server(form) => (
                "Server",
                crate::platform::fixed_text!("Your always-on machine. Sessions there keep running when {} is closed or asleep.", crate::platform::this_computer()),
                crate::server_ui::render_server_dialog(ws, form, cx),
                div()
                    .flex()
                    .justify_end()
                    .child(ui::primary_button("server-done", "Done").on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.close_modal(window, cx),
                    )))
                    .into_any_element(),
                true,
            ),
        };
        let (title, subtitle): (SharedString, SharedString) = match heading {
            Some((t, s)) => (t.into(), s.into()),
            None => (title.into(), subtitle.into()),
        };
        deferred(
            div()
                .id("modal-overlay")
                .occlude()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .justify_center()
                .items_start()
                .pt(theme::sz(56.))
                .pb(theme::sz(16.))
                .bg(theme::overlay())
                .child(
                    // At most 760 px, and never taller than the window: in a short window the
                    // body scrolls and the footer's buttons stay on screen. Never wider than the
                    // window either (the wide one is 884 px at 130 %).
                    div().flex().flex_col().h_full().max_h(theme::sz(760.)).max_w_full().child(
                    div()
                        .id("modal")
                        .flex()
                        .flex_col()
                        .w(theme::sz(if wide { 680. } else { 620. }))
                        .max_w_full()
                        .min_h_0()
                        .max_h(gpui::relative(1.))
                        .rounded_xl()
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::dialog())
                        .shadow_lg()
                        .child(
                            div()
                                .flex()
                                .items_start()
                                .gap_3()
                                .px_5()
                                .pt_4()
                                .pb_2()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1()
                                        .child(div().text_size(theme::sz(17.)).font_weight(FontWeight::SEMIBOLD).child(title))
                                        .child(div().text_color(theme::muted()).child(subtitle)),
                                )
                                .child(ui::icon_button("modal-close", "×").on_click(cx.listener(
                                    |this, _: &ClickEvent, window, cx| this.close_modal(window, cx),
                                ))),
                        )
                        .child(
                            div()
                                .id("modal-body")
                                .flex()
                                .flex_col()
                                .gap_4()
                                .px_5()
                                .py_3()
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .child(body),
                        )
                        .child(
                            div()
                                .px_5()
                                .py_3p5()
                                .border_t_1()
                                .border_color(theme::border())
                                .child(footer),
                        ),
                    ),
                ),
        )
        .with_priority(5)
        .into_any_element()
    }
}

fn pick_account(ws: &Workspace, session: &Session, agent: AgentKind) -> Option<String> {
    let default = ws.state.default_account(agent).map(|a| a.id.clone());
    if agent == session.agent && default == session.account_id {
        ws.state
            .accounts_for(agent)
            .find(|a| Some(&a.id) != session.account_id.as_ref())
            .map(|a| a.id.clone())
            .or(default)
    } else {
        default
    }
}

/// A worktree as the New session dialog names it: its branch, or with no branch known (a folder
/// without git, or one git has not listed yet) its folder's name.
fn worktree_label(path: &std::path::Path, branch: &str) -> String {
    if branch.is_empty() {
        format!("⎇ {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string()))
    } else {
        format!("⎇ {branch}")
    }
}

/// A labelled part of a dialog, the label as a quiet sentence above it (as in the redesign).
fn section(label: &str) -> gpui::Div {
    div().flex().flex_col().gap_2().child(div().text_size(theme::sz(13.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child(label.to_string()))
}

fn account_dropdown(
    ws: &Workspace,
    agent: AgentKind,
    selected: Option<&String>,
    open: bool,
    current: Option<&String>,
    on_toggle: impl Fn(&mut Workspace) + 'static,
    on_pick: impl Fn(&mut Workspace, String) + Clone + 'static,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let selected_account = selected.and_then(|id| ws.state.account(id));
    let label = selected_account
        .map(|a| a.display())
        .unwrap_or_else(|| format!("No {} account — add one", agent.name()));
    let mut options: Vec<AnyElement> = Vec::new();
    if open {
        for account in ws.state.accounts_for(agent) {
            let id = account.id.clone();
            let is_selected = selected == Some(&account.id);
            let is_default = ws.state.defaults.for_agent(agent) == Some(&account.id);
            let is_current = current == Some(&account.id);
            let pick = on_pick.clone();
            options.push(
                div()
                    .id(SharedString::from(format!("opt-{}", account.id)))
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .h(theme::sz(34.))
                    .px_2p5()
                    .rounded_md()
                    .cursor_pointer()
                    .when(is_selected, |d| d.bg(theme::selected()))
                    .hover(|s| s.bg(theme::hover()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        pick(this, id.clone());
                        cx.notify();
                    }))
                    .child(ui::dot(theme::agent_color(agent)))
                    .child(account.label.clone())
                    .child(div().text_size(theme::sz(11.5)).text_color(theme::muted()).child(account.plan.clone().unwrap_or_default()))
                    .when(is_default, |d| d.child(ui::badge("Default", theme::muted())))
                    .when(is_current, |d| d.child(ui::badge("In use here", theme::muted())))
                    .when(is_selected, |d| d.child(div().ml_auto().text_color(theme::accent()).child("✓")))
                    .into_any_element(),
            );
        }
        options.push(div().h(px(1.)).my_1p5().bg(theme::border_strong()).into_any_element());
        options.push(
            div()
                .id("opt-add")
                .flex()
                .items_center()
                .h(theme::sz(34.))
                .px_2p5()
                .rounded_md()
                .cursor_pointer()
                .text_color(theme::text_soft())
                .hover(|s| s.bg(theme::hover()))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_accounts(window, cx);
                    if let Some(form) = this.settings.form.as_mut() {
                        form.adding = Some(agent);
                        crate::text_input::focus(&form.label, window, cx);
                    }
                }))
                .child(format!("+  Add another {} account", agent.name()))
                .into_any_element(),
        );
    }
    div()
        .relative()
        .child(
            div()
                .id(SharedString::from(format!("dropdown-{}", agent.name())))
                .flex()
                .items_center()
                .gap_2p5()
                .h(theme::sz(38.))
                .px_3()
                .rounded_md()
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::input())
                .cursor_pointer()
                .hover(|s| s.border_color(theme::faint()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    on_toggle(this);
                    cx.notify();
                }))
                .child(ui::dot(theme::agent_color(agent)))
                .child(label)
                .child(div().ml_auto().text_color(theme::muted()).child("▾")),
        )
        .when(open, |d| {
            d.child(
                deferred(
                    div()
                        .id("dropdown-list")
                        .occlude()
                        .absolute()
                        .top(theme::sz(42.))
                        .left_0()
                        .right_0()
                        .p_1p5()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::subtle())
                        .shadow_lg()
                        .children(options),
                )
                .with_priority(6),
            )
        })
        .into_any_element()
}

fn with_new_session_form(ws: &mut Workspace, f: impl FnOnce(&mut NewSessionForm)) {
    if let Some(Modal::NewSession(form)) = ws.modal.as_mut() {
        f(form);
    }
}

/// The dialog's project or machine changed: look again for the base and the PRs.
fn after_target_change(this: &mut Workspace, cx: &mut Context<Workspace>) {
    let searching = matches!(this.modal.as_ref(), Some(Modal::NewSession(f)) if f.review_mode || f.from_pr);
    with_new_session_form(this, |f| {
        f.pr_searched = None;
        f.pr_hits = None;
        f.pr_picked = None;
        f.fresh = None;
        f.fresh_for = None;
    });
    this.new_session_fetch_base(cx);
    if searching {
        this.pr_search_now(cx);
    }
    this.load_resume_list(cx);
}

fn with_handoff_form(ws: &mut Workspace, f: impl FnOnce(&mut HandoffForm)) {
    if let Some(Modal::Handoff(form)) = ws.modal.as_mut() {
        f(form);
    }
}

fn render_new_session(ws: &Workspace, form: &NewSessionForm, cx: &mut Context<Workspace>) -> AnyElement {
    // Projects, worktrees and accounts come from this Mac or from the server.
    let server = if form.on_server { ws.server.state.as_ref() } else { None };
    let project_list: Vec<glasscode_core::model::Project> = match server {
        Some(state) => state.projects.clone(),
        None => ws.state.projects.clone(),
    };
    let worktree_list: Vec<(PathBuf, String)> = match server {
        Some(state) => {
            let repo = project_list.iter().find(|p| p.id == form.project_id).and_then(|p| p.root.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            state.worktrees.get(&form.project_id).map(|l| l.iter().map(|w| (w.path.clone(), w.label(&repo))).collect()).unwrap_or_default()
        }
        None => ws.project_worktrees(&form.project_id),
    };
    let run_on = ws.server.config.as_ref().map(|_| {
        let name = ws.server.name();
        let connected = ws.server.connected();
        section("Run on").child(
            div()
                .flex()
                .gap_2()
                .child(
                    ui::choice_card("ns-run-mac", !form.on_server, crate::platform::this_computer_label(), format!("Stops while the {} sleeps", crate::platform::computer()))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            // The person chose: the next new session starts here too.
                            if this.state.prefs.choose_run_on(false) {
                                this.save();
                            }
                            let project = this.active_session().map(|s| s.project_id.clone()).or_else(|| this.state.projects.first().map(|p| p.id.clone()));
                            let account = this.state.default_account(AgentKind::Claude).map(|a| a.id.clone());
                            with_new_session_form(this, |f| {
                                f.on_server = false;
                                f.agent = AgentKind::Claude;
                                f.account_id = account;
                                f.existing = None;
                                if let Some(project) = project {
                                    f.project_id = project;
                                }
                            });
                            after_target_change(this, cx);
                            cx.notify();
                        })),
                )
                .child(
                    ui::choice_card(
                        "ns-run-server",
                        form.on_server,
                        name.clone(),
                        if connected { format!("Keeps running when {} is closed", crate::platform::this_computer()) } else { "Not connected".to_string() },
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        if !this.server.connected() {
                            this.open_server_dialog(window, cx);
                            return;
                        }
                        // The server's counterpart of the project picked here, else its first.
                        let picked = match this.modal.as_ref() {
                            Some(Modal::NewSession(f)) => f.project_id.clone(),
                            _ => String::new(),
                        };
                        let target = server_counterpart(this, &picked).or_else(|| {
                            let server = this.server.state.as_ref()?;
                            Some((server.projects.first()?.id.clone(), server.defaults.claude.clone()))
                        });
                        let Some((project, account)) = target else {
                            this.open_server_dialog(window, cx);
                            return;
                        };
                        // The person chose: the next new session starts on the server too.
                        if this.state.prefs.choose_run_on(true) {
                            this.save();
                        }
                        with_new_session_form(this, |f| aim_at_server(f, project, account));
                        after_target_change(this, cx);
                        cx.notify();
                    })),
                ),
        )
    });

    let projects: Vec<AnyElement> = project_list
        .iter()
        .map(|project| {
            let id = project.id.clone();
            ui::chip(
                SharedString::from(format!("ns-project-{}", project.id)),
                project.id == form.project_id,
                project.name.clone(),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let id = id.clone();
                let plain_root = (!this.is_git_project(&id)).then(|| this.state.project(&id).map(|p| p.root.clone())).flatten();
                with_new_session_form(this, |f| {
                    f.project_id = id;
                    f.existing = plain_root.clone();
                    if plain_root.is_some() {
                        f.new_worktree = false;
                        f.from_pr = false;
                        f.choose_worktree = false;
                    }
                });
                after_target_change(this, cx);
                cx.notify();
            }))
            .into_any_element()
        })
        .collect();

    let worktrees: Vec<AnyElement> = worktree_list
        .iter()
        .cloned()
        .map(|(path, branch)| {
            let selected = form.existing.as_ref() == Some(&path);
            ui::chip(SharedString::from(format!("ns-wt-{}", path.display())), selected, worktree_label(&path, &branch))
                .font_family(theme::MONO_FONT)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let path = path.clone();
                    with_new_session_form(this, |f| f.existing = Some(path));
                    this.load_resume_list(cx);
                    cx.notify();
                }))
                .into_any_element()
        })
        .collect();

    // Which GitHub account new work in this project pushes and uses `gh` as. Shown for a local
    // GitHub project when there is more than one account, so it can be set the first time here.
    let github_accounts: Vec<AnyElement> = if form.on_server || !ws.is_git_project(&form.project_id) {
        Vec::new()
    } else if let Some(list) = ws.github.list.as_ref().filter(|l| l.len() > 1) {
        let chosen = ws.github_account_for(&form.project_id);
        let default = ws.state.github.default.clone();
        list.iter()
            .map(|account| {
                let login = account.login.clone();
                let selected = chosen.as_deref() == Some(login.as_str());
                let label = if default.as_deref() == Some(login.as_str()) { format!("{login}  · default") } else { login.clone() };
                let project_id = form.project_id.clone();
                ui::chip(SharedString::from(format!("ns-gh-{login}")), selected, label)
                    .when(!account.ok, |c| c.opacity(0.6))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_project_github(&project_id, Some(login.clone()), cx);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect()
    } else {
        Vec::new()
    };

    let agents: Vec<AnyElement> = [
        (AgentKind::Claude, "Claude Code on one of your Claude plans"),
        (AgentKind::Codex, "Codex CLI on one of your ChatGPT plans"),
        (AgentKind::Shell, "Plain terminal, no agent"),
    ]
    .into_iter()
    .filter(|(agent, _)| !(form.review_mode && *agent == AgentKind::Shell))
    .map(|(agent, detail)| {
        ui::choice_card(
            SharedString::from(format!("ns-agent-{}", agent.name())),
            form.agent == agent,
            agent.name(),
            detail,
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            let on_server = matches!(this.modal.as_ref(), Some(Modal::NewSession(f)) if f.on_server);
            let account = if on_server {
                this.server.state.as_ref().and_then(|s| s.defaults.for_agent(agent).cloned())
            } else {
                this.state.default_account(agent).map(|a| a.id.clone())
            };
            with_new_session_form(this, |f| {
                f.agent = agent;
                f.account_id = account;
                f.account_open = false;
            });
            this.load_resume_list(cx);
            cx.notify();
        }))
        .into_any_element()
    })
    .collect();

    let preview = {
        let project = project_list.iter().find(|p| p.id == form.project_id);
        let mut lines = Vec::new();
        if form.on_server {
            lines.push(format!("ssh {}   # glasscoded runs it in tmux, so it keeps running", ws.server.config.as_ref().map(|c| c.ssh_target.clone()).unwrap_or_default()));
        }
        let folder = project.map(|p| git::slug(&p.name)).unwrap_or_default();
        let default = form.fresh.as_ref().map(|f| f.branch.clone()).unwrap_or_else(|| "main".into());
        if form.review_mode {
            match &form.pr_picked {
                Some(pr) => {
                    lines.push(format!("git fetch origin pull/{n}/head   # the PR's latest commit, forks too", n = pr.number));
                    lines.push(format!("git worktree add ~/.glasscode/worktrees/{folder}/review-{n} review/{n}", n = pr.number));
                }
                None => lines.push("# pick the pull request to review".into()),
            }
        } else if form.from_pr {
            match &form.pr_picked {
                Some(pr) if pr.same_repo => {
                    lines.push(format!("git fetch origin {}", pr.head));
                    lines.push(format!("git worktree add ~/.glasscode/worktrees/{folder}/{} {}", git::slug(&pr.head), pr.head));
                }
                Some(pr) => {
                    lines.push(format!("git fetch origin pull/{n}/head   # a fork's PR", n = pr.number));
                    lines.push(format!("git worktree add ~/.glasscode/worktrees/{folder}/pr-{n} -b pr/{n}", n = pr.number));
                }
                None => lines.push("# pick the pull request to carry on with".into()),
            }
        } else if form.new_worktree {
            let branch = form.branch.read(cx).text();
            let base = if ws.state.prefs.fresh_main_off && !form.on_server {
                form.fresh.as_ref().map(|f| f.base.clone()).unwrap_or_else(|| default.clone())
            } else {
                lines.push(format!("git fetch origin {default}"));
                format!("origin/{default}")
            };
            // The folder the project's settings file names, as `add_worktree` makes it.
            let template = project.and_then(|p| ws.project_file(&p.id)).and_then(|l| l.settings.worktree_folder.as_deref());
            let rel = glasscode_core::project_settings::worktree_dir(std::path::Path::new(""), project.map(|p| p.name.as_str()).unwrap_or_default(), &branch, template);
            lines.push(format!("git worktree add ~/.glasscode/worktrees/{} -b {} {base}", rel.to_string_lossy().replace('\\', "/"), branch));
        } else if let Some(path) = &form.existing {
            lines.push(format!("cd {}", path.display()));
        }
        let account = form.account_id.as_deref().and_then(|id| ws.state.account(id));
        match form.agent {
            // The server starts its own shell there.
            AgentKind::Shell if form.on_server => lines.push("zsh -l".into()),
            AgentKind::Shell => lines.push(shell_line()),
            agent => {
                let var = if agent == AgentKind::Claude { "CLAUDE_CONFIG_DIR" } else { "CODEX_HOME" };
                let program = if agent == AgentKind::Claude { "claude" } else { "codex" };
                let command = match (&form.resume, agent) {
                    (Some(c), AgentKind::Claude) => format!("{program} --resume {}", c.id),
                    (Some(c), _) => format!("{program} resume {}", c.id),
                    (None, _) => program.to_string(),
                };
                match account.and_then(Account::config_dir) {
                    Some(dir) => lines.push(format!("{var}={} {command}", dir.display())),
                    None => lines.push(command),
                }
            }
        }
        lines
    };

    let compact_worktree = (!form.new_worktree && !form.choose_worktree)
        .then(|| form.existing.clone())
        .flatten()
        .map(|path| {
            let label = worktree_list
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(p, branch)| worktree_label(p, branch))
                .unwrap_or_else(|| format!("⎇ {}", path.display()));
            section("Worktree").child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(ui::chip("ns-wt-current", true, label).font_family(theme::MONO_FONT))
                    .child(
                        div()
                            .id("ns-wt-change")
                            .text_size(theme::sz(12.5))
                            .text_color(theme::link())
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                with_new_session_form(this, |f| f.choose_worktree = true);
                                cx.notify();
                            }))
                            .child("Change…"),
                    ),
            )
        });

    let default_name = form.fresh.as_ref().map(|f| f.branch.clone()).unwrap_or_else(|| "main".into());
    let server_name = form.on_server.then(|| ws.server.name());
    let start_from = section("Start from")
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    ui::choice_card(
                        "ns-new-wt",
                        form.new_worktree && !form.from_pr,
                        if ws.state.prefs.fresh_main_off { default_name.clone() } else { format!("Latest {default_name}") },
                        if ws.state.prefs.fresh_main_off { format!("A new branch from it, as it is on {}", crate::platform::this_computer()) } else { "A new branch from it, fetched first".to_string() },
                    ).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        with_new_session_form(this, |f| {
                            f.new_worktree = true;
                            f.from_pr = false;
                        });
                        this.load_resume_list(cx);
                        cx.notify();
                    })),
                )
                .child(
                    ui::choice_card("ns-from-pr", form.from_pr, "A pull request", "Carry on with a PR on its latest code").on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        with_new_session_form(this, |f| {
                            f.from_pr = true;
                            f.new_worktree = true;
                        });
                        if let Some(Modal::NewSession(f)) = this.modal.as_ref() {
                            crate::text_input::focus(&f.pr_query, window, cx);
                        }
                        this.pr_search_now(cx);
                        cx.notify();
                    })),
                )
                .child(
                    ui::choice_card("ns-existing-wt", !form.new_worktree && !form.from_pr, "Existing worktree", "Reuse a folder already checked out").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        with_new_session_form(this, |f| {
                            f.new_worktree = false;
                            f.from_pr = false;
                        });
                        this.load_resume_list(cx);
                        cx.notify();
                    })),
                ),
        )
        .when(form.new_worktree && !form.from_pr, |d| {
            d.child(crate::session_start::base_note(form, server_name.clone(), !ws.state.prefs.fresh_main_off)).child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(div().text_size(theme::sz(12.)).text_color(theme::text_soft()).child("Branch name"))
                    .child(form.branch.clone()),
            )
        })
        .when(form.from_pr, |d| {
            d.child(crate::session_start::render_pr_picker(form, cx)).when_some(form.pr_picked.as_ref(), |d, pr| {
                d.child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(if pr.same_repo {
                    format!("Works on {} at its latest commit; pushing updates the PR. Uncommitted changes in a worktree already on it are kept.", pr.head)
                } else {
                    format!("A fork's PR: checked out as pr/{} at its latest commit (pushing goes to your own branch).", pr.number)
                }))
            })
        })
        .when(!form.new_worktree && !form.from_pr, |d| d.child(div().flex().flex_wrap().gap_1p5().children(worktrees)));
    let review = section("Pull request to review")
        .child(crate::session_start::render_pr_picker(form, cx))
        .when_some(form.pr_picked.as_ref(), |d, pr| {
            d.child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(format!(
                "Checked out as review/{} at the PR's latest commit, and refreshed when the author pushes. The agent never pushes here.",
                pr.number
            )))
        });

    div()
        .flex()
        .flex_col()
        .gap_4()
        .when_some(run_on, |d, run_on| d.child(run_on))
        .child(section("Project").child(div().flex().flex_wrap().gap_1p5().children(projects)))
        .when(!github_accounts.is_empty(), |d| d.child(section("GitHub account").child(div().flex().flex_col().gap_1().child(div().flex().flex_wrap().gap_1p5().children(github_accounts)).child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child("Who this project's sessions push and use gh as. You can change it later in the project's ⋯ menu.")))))
        .when(true, |d| {
            let plain = !ws.is_git_project(&form.project_id);
            let set_up = form.project_id.clone();
            d.when(!plain, |d| d.when_some(compact_worktree, |d, compact| d.child(compact)).when(form.new_worktree || form.from_pr || form.choose_worktree || form.existing.is_none(), |d| d.child(start_from)))
                .when(plain, |d| {
                    d.child(
                        section("Folder").child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(div().flex_1().text_size(theme::sz(12.5)).text_color(theme::muted()).child("This folder has no git, so the session works in the folder itself. Set up git to give each session its own worktree."))
                                .child(ui::button("ns-set-up-git", "Set up git").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.set_up_git(&set_up, cx);
                                    with_new_session_form(this, |f| f.existing = None);
                                    cx.notify();
                                }))),
                        ),
                    )
                })
        })
        .child(section("Who drives it").child(div().flex().gap_2().children(agents)))
        .when(!form.review_mode && !form.new_worktree && !form.from_pr && form.agent.uses_accounts(), |d| d.child(render_resume_list(ws, form, cx)))
        .when(form.agent.uses_accounts() && server.is_some(), |d| {
            let agent = form.agent;
            let state = server.expect("checked");
            let chips: Vec<AnyElement> = state
                .accounts
                .iter()
                .filter(|a| a.agent == agent)
                .map(|account| {
                    let id = account.id.clone();
                    let signed = state.signed_in.get(&account.id).copied().unwrap_or(false);
                    ui::chip(
                        SharedString::from(format!("ns-server-account-{}", account.id)),
                        form.account_id.as_deref() == Some(account.id.as_str()),
                        if signed { account.display() } else { format!("{} (not signed in)", account.display()) },
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let id = id.clone();
                        with_new_session_form(this, |f| f.account_id = Some(id));
                        cx.notify();
                    }))
                    .into_any_element()
                })
                .collect();
            d.child(section(&format!("{} account on {}", agent.name(), ws.server.name())).child(div().flex().flex_wrap().gap_1p5().children(chips)))
        })
        .when(form.agent.uses_accounts() && server.is_none(), |d| {
            let agent = form.agent;
            d.child(section(&format!("{} account", agent.name())).child(account_dropdown(
                ws,
                agent,
                form.account_id.as_ref(),
                form.account_open,
                None,
                |this| with_new_session_form(this, |f| f.account_open = !f.account_open),
                |this, id| {
                    with_new_session_form(this, |f| {
                        f.account_id = Some(id);
                        f.account_open = false;
                    })
                },
                cx,
            )))
        })
        .when(!form.review_mode, |d| {
            d.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(div().text_size(theme::sz(12.)).text_color(theme::text_soft()).child("Session name (optional)"))
                    .child(form.name.clone()),
            )
        })
        .child(
            // In one line: where it runs, who it pushes as and what comes with it.
            div()
                .px_3()
                .py_2p5()
                .rounded_full()
                .bg(theme::hover())
                .text_size(theme::sz(13.))
                .text_color(theme::text_soft())
                .child(new_session_note(ws, form, &project_list)),
        )
        .child(command_preview(preview))
        .when_some(form.error.clone(), |d, error| {
            d.child(div().text_color(theme::removed()).child(error))
        })
        .into_any_element()
}

/// The server's project for a project here, with the server's default account for Claude: the
/// one at the same place under home, else the same name and folder (as the moves find it).
pub fn server_counterpart(ws: &Workspace, project_id: &str) -> Option<(String, Option<String>)> {
    let server = ws.server.state.as_ref()?;
    let here = ws.state.project(project_id)?;
    let server_home = ws.server.client.as_ref()?.hello.home.clone();
    let root = glasscode_core::sync::map_path(&here.root, &glasscode_core::paths::home(), &server_home, &here.name, None);
    let found = server.projects.iter().find(|p| p.root == root).or_else(|| server.projects.iter().find(|p| p.name == here.name && p.root.file_name() == here.root.file_name()))?;
    Some((found.id.clone(), server.defaults.claude.clone()))
}

/// Aim the new-session form at the server: its project, its default account, Claude, and a new
/// or chosen worktree there (a worktree picked on this Mac is not on the server).
pub fn aim_at_server(form: &mut NewSessionForm, project: String, account: Option<String>) {
    form.on_server = true;
    form.agent = AgentKind::Claude;
    form.account_id = account;
    form.project_id = project;
    form.existing = None;
    form.choose_worktree = true;
}

/// "Runs on this Mac and stays in sync on devbox · GitHub: your-account · …"
fn new_session_note(ws: &Workspace, form: &NewSessionForm, projects: &[glasscode_core::model::Project]) -> String {
    let project = projects.iter().find(|p| p.id == form.project_id);
    let mut parts = Vec::new();
    parts.push(match (form.on_server, ws.server.config.is_some()) {
        (true, _) => format!("Runs on {}, and keeps running while {} sleeps", ws.server.name(), crate::platform::this_computer()),
        // The person last chose the server, but it is not connected: this Mac, and it says so.
        (false, true) if ws.state.prefs.new_on_server && !ws.server.connected() => {
            format!("Runs on {}: {} is not connected (new sessions start there again once it is)", crate::platform::this_computer(), ws.server.name())
        }
        (false, true) => format!("Runs on {} and stays in sync on {}", crate::platform::this_computer(), ws.server.name()),
        (false, false) => format!("Runs on {}", crate::platform::this_computer()),
    });
    if let Some(login) = ws.github_account_for(&form.project_id) {
        let whose = match project {
            Some(p) if p.github_account.is_some() => format!("{}'s account", p.name),
            _ => "the default".to_string(),
        };
        parts.push(format!("GitHub: {login} ({whose})"));
    }
    if form.agent != AgentKind::Shell && !form.on_server {
        parts.push("the browser tools come with it".to_string());
    }
    parts.join(" · ")
}

fn ago(time: std::time::SystemTime) -> String {
    let minutes = time.elapsed().map(|d| d.as_secs() / 60).unwrap_or(0);
    match minutes {
        0..=1 => "just now".into(),
        2..=59 => format!("{minutes} min ago"),
        60..=119 => "1 hour ago".into(),
        120..=1439 => format!("{} hours ago", minutes / 60),
        1440..=2879 => "yesterday".into(),
        _ => format!("{} days ago", minutes / 1440),
    }
}

/// Start fresh, or pick an earlier conversation of this worktree to resume.
fn render_resume_list(ws: &Workspace, form: &NewSessionForm, cx: &mut Context<Workspace>) -> AnyElement {
    let agent = form.agent;
    let radio = |selected: bool| {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .mt(theme::sz(2.))
            .size(theme::sz(14.))
            .rounded_full()
            .border_1()
            .border_color(if selected { theme::accent() } else { theme::faint() })
            .when(selected, |d| d.child(div().size(theme::sz(6.)).rounded_full().bg(theme::accent())))
    };
    let row = |id: SharedString, selected: bool| {
        div()
            .id(id)
            .flex()
            .items_start()
            .gap_2p5()
            .px_2p5()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(if selected { theme::accent() } else { theme::border() })
            .bg(if selected { theme::hover() } else { theme::raised() })
            .cursor_pointer()
            .hover(|s| s.border_color(theme::faint()))
            .child(radio(selected))
    };
    let mut rows: Vec<AnyElement> = vec![
        row("resume-new".into(), form.resume.is_none())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                with_new_session_form(this, |f| f.resume = None);
                cx.notify();
            }))
            .child(div().text_color(theme::text_strong()).child("Start a new conversation"))
            .into_any_element(),
    ];
    match &form.history {
        None => rows.push(
            div().px_1().text_size(theme::sz(12.)).text_color(theme::faint()).child(format!("Looking for earlier {} conversations in this worktree…", agent.name())).into_any_element(),
        ),
        Some(list) if list.is_empty() => rows.push(
            div().px_1().text_size(theme::sz(12.)).text_color(theme::faint()).child(format!("No earlier {} conversations in this worktree.", agent.name())).into_any_element(),
        ),
        Some(list) => {
            let shown = if form.history_all { list.len() } else { list.len().min(5) };
            for (index, conversation) in list.iter().take(shown).enumerate() {
                let selected = form.resume.as_ref().is_some_and(|r| r.id == conversation.id);
                let (open_in, account) = if form.on_server {
                    let state = ws.server.state.as_ref();
                    (
                        state.and_then(|s| s.sessions.iter().find(|r| r.session.agent_session_id.as_deref() == Some(conversation.id.as_str()))).map(|r| format!("server:{}", r.session.id)),
                        state.and_then(|s| s.accounts.iter().find(|a| a.id == conversation.account_id)).map(|a| a.label.clone()).unwrap_or_else(|| conversation.account_id.clone()),
                    )
                } else {
                    (
                        ws.state.sessions.iter().find(|s| s.agent_session_id.as_deref() == Some(conversation.id.as_str())).map(|s| s.id.clone()),
                        ws.state.account(&conversation.account_id).map(|a| a.label.clone()).unwrap_or_else(|| conversation.account_id.clone()),
                    )
                };
                let picked = conversation.clone();
                let is_open = open_in.is_some();
                let mut meta = vec![ago(conversation.updated), account];
                if is_open {
                    meta.push("open in a tab".into());
                }
                rows.push(
                    row(SharedString::from(format!("resume-{index}")), selected)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            if let Some(session) = &open_in {
                                // Two terminals on one conversation would write over each other.
                                this.modal = None;
                                match session.strip_prefix("server:") {
                                    Some(id) => this.open_server_session(id, window, cx),
                                    None => this.open_session(session, window, cx),
                                }
                                return;
                            }
                            let picked = picked.clone();
                            with_new_session_form(this, |f| {
                                f.account_id = Some(picked.account_id.clone());
                                f.account_open = false;
                                f.resume = Some(picked);
                            });
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .gap_0p5()
                                .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text_strong()).child(conversation.title.clone()))
                                .when_some(conversation.last_prompt.clone(), |d, last| {
                                    d.child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(12.)).text_color(theme::muted()).child(format!("Last: {last}")))
                                }),
                        )
                        .child(div().flex_none().text_size(theme::sz(11.5)).text_color(if is_open { theme::codex() } else { theme::faint() }).child(meta.join(" · ")))
                        .into_any_element(),
                );
            }
            if list.len() > shown {
                rows.push(
                    div()
                        .id("resume-more")
                        .px_1()
                        .text_size(theme::sz(12.))
                        .text_color(theme::link())
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            with_new_session_form(this, |f| f.history_all = true);
                            cx.notify();
                        }))
                        .child(format!("Show {} more", list.len() - shown))
                        .into_any_element(),
                );
            }
        }
    }
    section("Conversation").child(div().flex().flex_col().gap_1p5().children(rows)).into_any_element()
}

/// What a shell session starts here, as the preview shows it: `zsh -l`, `powershell -NoLogo`.
fn shell_line() -> String {
    // Kept: on Windows the shell is found on PATH, which is too slow for each frame.
    static LINE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let shell = glasscode_core::sys::default_shell();
        let program = std::path::Path::new(&shell.program);
        let name = if program.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) { program.file_stem() } else { program.file_name() };
        let name = name.map_or_else(|| shell.program.clone(), |n| n.to_string_lossy().into_owned());
        std::iter::once(name).chain(shell.session_args).collect::<Vec<_>>().join(" ")
    });
    LINE.clone()
}

fn command_preview(lines: Vec<String>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::bg())
        .child(ui::caps_label("GLASSCODE WILL RUN"))
        .children(lines.into_iter().map(|line| {
            div()
                .font_family(theme::MONO_FONT)
                .text_size(theme::sz(11.5))
                .text_color(theme::text_soft())
                .child(format!("$ {line}"))
        }))
        .into_any_element()
}

fn new_session_footer(form: &NewSessionForm, cx: &mut Context<Workspace>) -> AnyElement {
    let label = if form.busy {
        if form.review_mode || form.from_pr { "Fetching the pull request…".to_string() } else { "Creating worktree…".to_string() }
    } else if form.review_mode {
        match &form.pr_picked {
            Some(pr) => format!("Start review of #{}", pr.number),
            None => "Pick a pull request".to_string(),
        }
    } else if form.from_pr {
        match &form.pr_picked {
            Some(pr) => format!("Start {} on #{}", form.agent.name(), pr.number),
            None => "Pick a pull request".to_string(),
        }
    } else if form.agent == AgentKind::Shell {
        "Open terminal".to_string()
    } else if form.resume.is_some() && !form.new_worktree {
        format!("Resume {} conversation", form.agent.name())
    } else if form.on_server {
        format!("Start {} on the server", form.agent.name())
    } else {
        format!("Start {} session", form.agent.name())
    };
    div()
        .flex()
        .justify_end()
        .gap_2()
        .child(ui::button("ns-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.close_modal(window, cx)
        })))
        .child(ui::primary_button("ns-start", label).on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.create_session_from_form(window, cx)
        })))
        .into_any_element()
}

fn render_handoff(ws: &Workspace, form: &HandoffForm, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(session) = ws.state.session(&form.session_id) else {
        return div().child("This session is gone.").into_any_element();
    };
    let from_account = session.account_id.as_deref().and_then(|id| ws.state.account(id));
    let same_agent = form.to_agent == session.agent;
    let agents: Vec<AnyElement> = [AgentKind::Claude, AgentKind::Codex]
        .into_iter()
        .map(|agent| {
            let detail = if agent == session.agent {
                "Same agent, another account"
            } else if agent == AgentKind::Codex {
                "Hand the work to Codex"
            } else {
                "Hand the work to Claude"
            };
            ui::choice_card(
                SharedString::from(format!("ho-agent-{}", agent.name())),
                form.to_agent == agent,
                agent.name(),
                detail,
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let account = this
                    .active_session()
                    .cloned()
                    .and_then(|s| pick_account(this, &s, agent));
                with_handoff_form(this, |f| {
                    f.to_agent = agent;
                    f.to_account = account;
                    f.account_open = false;
                });
                cx.notify();
            }))
            .into_any_element()
        })
        .collect();
    fn check(
        id: &'static str,
        checked: bool,
        title: &str,
        detail: String,
        toggle: fn(&mut HandoffForm),
        cx: &mut Context<Workspace>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex()
            .items_start()
            .gap_2p5()
            .py_1p5()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                with_handoff_form(this, toggle);
                cx.notify();
            }))
            .child(ui::checkbox(SharedString::from(format!("{id}-box")), checked).mt_0p5())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().text_color(theme::text()).child(title.to_string()))
                    .child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(detail)),
            )
    }
    let changes = ws.changes.len();
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2p5()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::input())
                .child(ui::caps_label("FROM"))
                .child(ui::dot(theme::agent_color(session.agent)))
                .child(
                    div().font_weight(FontWeight::SEMIBOLD).child(format!(
                        "{} · {}",
                        session.agent.name(),
                        from_account.map(|a| a.display()).unwrap_or_default()
                    )),
                )
                .child(
                    div()
                        .font_family(theme::MONO_FONT)
                        .text_size(theme::sz(11.5))
                        .text_color(theme::muted())
                        .child(format!("⎇ {}", session.branch)),
                ),
        )
        .child(section("Continue with").child(div().flex().gap_2().children(agents)))
        .child(section(&format!("{} account", form.to_agent.name())).child(account_dropdown(
            ws,
            form.to_agent,
            form.to_account.as_ref(),
            form.account_open,
            session.account_id.as_ref(),
            |this| with_handoff_form(this, |f| f.account_open = !f.account_open),
            |this, id| {
                with_handoff_form(this, |f| {
                    f.to_account = Some(id);
                    f.account_open = false;
                })
            },
            cx,
        )))
        .when(same_agent, |d| {
            d.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(theme::input())
                    .text_color(theme::text_soft())
                    .child("Same agent: the whole conversation moves to the other account as it is and resumes there."),
            )
        })
        .when(!same_agent, |d| {
            d.child(
                section("Carry over")
                    .child(check(
                        "ho-transcript",
                        form.include_transcript,
                        "Previous conversation",
                        "The next agent gets the transcript file and reads what it needs".into(),
                        |f| f.include_transcript = !f.include_transcript,
                        cx,
                    ))
                    .child(check(
                        "ho-diff",
                        form.include_diff,
                        "Uncommitted changes",
                        format!("{changes} changed file(s) in this worktree"),
                        |f| f.include_diff = !f.include_diff,
                        cx,
                    ))
                    .child(check(
                        "ho-terminal",
                        form.include_terminal,
                        "Last screen of the session",
                        "Recent output, so it sees where things stopped".into(),
                        |f| f.include_terminal = !f.include_terminal,
                        cx,
                    ))
                    .child(form.note.clone()),
            )
            .child(
                section("Open in").child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            ui::choice_card("ho-same-tab", !form.new_tab, "This tab", "The tab switches over and keeps its history")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    with_handoff_form(this, |f| f.new_tab = false);
                                    cx.notify();
                                })),
                        )
                        .child(
                            ui::choice_card("ho-new-tab", form.new_tab, "New tab", "Keep the original session open beside it")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    with_handoff_form(this, |f| f.new_tab = true);
                                    cx.notify();
                                })),
                        ),
                ),
            )
        })
        .into_any_element()
}

fn handoff_footer(ws: &Workspace, form: &HandoffForm, cx: &mut Context<Workspace>) -> AnyElement {
    let session = ws.state.session(&form.session_id);
    let same_account = session.is_some_and(|s| s.account_id == form.to_account && s.agent == form.to_agent);
    let missing = form.to_account.is_none();
    let label = if same_account {
        "Already on this account".to_string()
    } else if missing {
        format!("Add a {} account first", form.to_agent.name())
    } else if session.is_some_and(|s| s.agent == form.to_agent) {
        "Switch account".to_string()
    } else {
        format!("Hand off to {}", form.to_agent.name())
    };
    div()
        .flex()
        .justify_end()
        .gap_2()
        .child(ui::button("ho-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.close_modal(window, cx)
        })))
        .child(if same_account || missing {
            ui::disabled_button("ho-go", label)
        } else {
            ui::primary_button("ho-go", label).on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.perform_handoff(window, cx)
            }))
        })
        .into_any_element()
}

/// The Claude and Codex accounts, one card each, with adding another.
pub(crate) fn account_groups(ws: &Workspace, form: &AccountsForm, cx: &mut Context<Workspace>) -> Vec<AnyElement> {
    use crate::settings_ui::{group, row, row_with};
    let mut groups: Vec<AnyElement> = Vec::new();
    for agent in [AgentKind::Claude, AgentKind::Codex] {
        let accounts: Vec<Account> = ws.state.accounts_for(agent).cloned().collect();
        let mut rows: Vec<gpui::Div> = Vec::new();
        for account in &accounts {
            let is_default = ws.state.defaults.for_agent(agent) == Some(&account.id);
            let used = ws
                .state
                .sessions
                .iter()
                .filter(|s| s.account_id.as_deref() == Some(account.id.as_str()))
                .count();
            let folder = account.effective_dir();
            let default_id = account.id.clone();
            let login_id = account.id.clone();
            let remove_id = account.id.clone();
            let title = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(div().font_weight(FontWeight::MEDIUM).text_color(theme::text()).child(account.label.clone()))
                .when_some(account.plan.clone(), |d, plan| d.child(ui::badge(plan, theme::text_soft())))
                // On glass a badge's words are `text`, not the accent (Amber on light glass is faint).
                .when(is_default, |d| d.child(ui::badge("Default for new sessions", theme::text())));
            let used_text = match used {
                0 => "Not used".to_string(),
                1 => "Used by 1 session".to_string(),
                n => format!("Used by {n} sessions"),
            };
            let buttons = div()
                .flex()
                .flex_wrap()
                .gap_2()
                .when(!is_default, |d| {
                    d.child(ui::button(SharedString::from(format!("default-{default_id}")), "Make default").on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.state.defaults.set(agent, Some(default_id.clone()));
                            this.save();
                            cx.notify();
                        }),
                    ))
                })
                .child(ui::button(SharedString::from(format!("login-{login_id}")), "Sign in…").on_click(
                    cx.listener(move |this, _: &ClickEvent, window, cx| this.start_login(&login_id, window, cx)),
                ))
                .when(used == 0, |d| {
                    d.child(ui::button(SharedString::from(format!("remove-{remove_id}")), "Remove").on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.remove_account(&remove_id, cx)),
                    ))
                })
                .into_any_element();
            rows.push(row_with(title, format!("{used_text} · {}", crate::settings_ui::tilde(&folder)), Some(buttons)));
        }
        if form.adding == Some(agent) {
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .child(form.label.clone())
                    .child(form.plan.clone())
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(ui::primary_button("add-confirm", format!("Add and sign in with {}", agent.name())).on_click(
                                cx.listener(|this, _: &ClickEvent, window, cx| this.add_account_from_form(window, cx)),
                            ))
                            .child(ui::button("add-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(form) = this.settings.form.as_mut() {
                                    form.adding = None;
                                }
                                cx.notify();
                            }))),
                    ),
            );
        } else {
            rows.push(row(
                format!("Add another {} account", agent.name()),
                "Signs in with the official CLI, in a terminal here",
                Some(
                    ui::button(SharedString::from(format!("add-{}", agent.name())), "Add…")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            if let Some(form) = this.settings.form.as_mut() {
                                form.adding = Some(agent);
                                form.label.update(cx, |i, cx| i.set_text("", cx));
                                form.plan.update(cx, |i, cx| i.set_text("", cx));
                                crate::text_input::focus(&form.label, window, cx);
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                ),
            ));
        }
        let signed_in = match accounts.len() {
            1 => "1 account; each session runs on one, new sessions on the default".to_string(),
            n => format!("{n} accounts; each session runs on one, new sessions on the default"),
        };
        groups.push(group(&format!("{} accounts", agent.name()), &signed_in, rows));
    }

    groups
}

/// The terminal an account signs in in (or an install runs in), while it is open.
pub(crate) fn accounts_terminal(form: &AccountsForm, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    form.terminal.as_ref().map(|(title, terminal)| {
        crate::settings_ui::card(title.clone(), "This runs in the official CLI. Glasscode never sees your password or tokens.")
            .control(ui::button("close-login", "Close terminal").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                if let Some(form) = this.settings.form.as_mut() {
                    form.terminal = None;
                }
                cx.notify();
            })))
            .body(div().h(theme::sz(320.)).rounded_md().border_1().border_color(theme::border_strong()).overflow_hidden().child(terminal.clone()))
            .render()
    })
}

/// The approved CLI versions, each with its install button when it is missing or off.
fn add_cli_field(label: &str, hint: &str, input: Entity<TextInput>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_size(theme::sz(13.)).text_color(theme::text_strong()).child(label.to_string()))
        .child(input)
        .when(!hint.is_empty(), |d| d.child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(hint.to_string())))
        .into_any_element()
}

pub(crate) fn render_add_cli(form: &AddCliForm, cx: &mut Context<Workspace>) -> AnyElement {
    let custom = form.custom;
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            ui::segmented()
                .child(
                    ui::segment("add-cli-npm", !custom, "From npm")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_add_cli_mode(false, cx))),
                )
                .child(
                    ui::segment("add-cli-custom", custom, "Custom")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_add_cli_mode(true, cx))),
                ),
        )
        .map(|d| {
            if custom {
                d.child(add_cli_field("Name", "What to call it in Glasscode.", form.name.clone()))
                    .child(add_cli_field("Install command", "Run to install or update it.", form.install.clone()))
                    .child(add_cli_field(
                        "Version command",
                        "Prints its version, so Glasscode can show it.",
                        form.version.clone(),
                    ))
            } else {
                d.child(add_cli_field("npm package", "Installed with npm install -g.", form.npm_package.clone()))
                    .child(add_cli_field(
                        "Command (optional)",
                        "Only if the command differs from the package name.",
                        form.npm_bin.clone(),
                    ))
            }
        })
        .into_any_element()
}

fn add_cli_footer(cx: &mut Context<Workspace>) -> AnyElement {
    div()
        .flex()
        .justify_end()
        .gap_2()
        .child(ui::button("add-cli-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx))))
        .child(ui::primary_button("add-cli-go", "Add CLI").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.add_cli_from_form(window, cx))))
        .into_any_element()
}

pub(crate) fn cli_rows(ws: &Workspace, cx: &mut Context<Workspace>) -> Vec<AnyElement> {
    let mut rows: Vec<AnyElement> = ws
        .cli_status
        .iter()
        .map(|status| {
            let ok = status.is_ok();
            let name = status.name.clone();
            let installed = status.installed.is_some();
            let update = status.update_available().then(|| status.latest.clone()).flatten();
            // Built-ins: offer the pinned version when it differs. Nothing installed: an Install.
            let install_pinned = status.install_command().filter(|_| !ok && update.is_none());
            let install_fresh = (!installed && update.is_none()).then(|| status.install_latest_command());
            let latest_note = status
                .latest
                .as_ref()
                .map(|l| if status.installed.as_deref() == Some(l.as_str()) { format!(" · latest {l} ✓") } else { format!(" · latest {l}") })
                .unwrap_or_default();
            let builtin = status.builtin;
            let update_name = name.clone();
            let install_name = name.clone();
            let remove_name = name.clone();
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                // On glass the words stay `text`; a CLI that is not right gets an orange dot.
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_1()
                        .min_w(theme::sz(220.))
                        .text_color(theme::text())
                        .when(!ok, |d| d.child(ui::dot(theme::attention())))
                        .child(format!("{}{latest_note}", status.summary())),
                )
                .when_some(update, |d, version| {
                    let v = version.clone();
                    let name = update_name.clone();
                    d.child(
                        ui::primary_button(SharedString::from(format!("update-{name}")), format!("Update to {version}"))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.cli_install(name.clone(), Some(v.clone()), window, cx)
                            })),
                    )
                })
                .when_some(install_pinned, |d, _command| {
                    let name = install_name.clone();
                    d.child(
                        ui::button(SharedString::from(format!("install-{name}")), "Install pinned version").on_click(
                            cx.listener(move |this, _: &ClickEvent, window, cx| {
                                let pin = this.cli_status.iter().find(|s| s.name == name).and_then(|s| s.approved.clone());
                                this.cli_install(name.clone(), pin, window, cx)
                            }),
                        ),
                    )
                })
                .when_some(install_fresh, |d, _command| {
                    let name = install_name.clone();
                    d.child(
                        ui::button(SharedString::from(format!("install-fresh-{name}")), "Install").on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| this.cli_install(name.clone(), None, window, cx),
                        )),
                    )
                })
                .when(!builtin, |d| {
                    let name = remove_name.clone();
                    d.child(ui::button(SharedString::from(format!("remove-{name}")), "Remove").on_click(cx.listener(
                        move |this, _: &ClickEvent, _window, cx| this.remove_custom_cli(name.clone(), cx),
                    )))
                })
                .into_any_element()
        })
        .collect();
    rows.push(
        div()
            .flex()
            .child(
                ui::button(SharedString::from("add-cli"), "+ Add a CLI")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_add_cli(window, cx))),
            )
            .into_any_element(),
    );
    rows
}

// ----- workspace operations driven by dialogs ----------------------------------------

impl Workspace {
    /// Look up earlier conversations for the dialog's worktree and agent (when they changed).
    pub fn load_resume_list(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::NewSession(form)) = self.modal.as_ref() else { return };
        let on_server = form.on_server;
        let root = if on_server {
            self.server.state.as_ref().and_then(|s| s.projects.iter().find(|p| p.id == form.project_id)).map(|p| p.root.clone())
        } else {
            self.state.project(&form.project_id).map(|p| p.root.clone())
        };
        let folder = if form.new_worktree { None } else { form.existing.clone().or(root) };
        let key = folder.filter(|_| form.agent.uses_accounts()).map(|f| (form.agent, f));
        if key == form.history_key {
            return;
        }
        let accounts = self.state.accounts.clone();
        with_new_session_form(self, |f| {
            f.history_key = key.clone();
            f.history = None;
            f.history_all = false;
            f.resume = None;
        });
        let Some((agent, folder)) = key else { return };
        let wanted = Some((agent, folder.clone()));
        let client = if on_server { self.server.client.clone() } else { None };
        let task = cx.background_spawn(async move {
            match client {
                // The server's own conversations, read by glasscoded there.
                Some(client) => client
                    .call::<Vec<PastConversation>>("history.list", serde_json::json!({ "agent": agent, "folder": folder }))
                    .unwrap_or_default(),
                None => history::conversations(agent, &accounts, &folder),
            }
        });
        cx.spawn(async move |this, cx| {
            let list = task.await;
            let _ = this.update(cx, |this, cx| {
                with_new_session_form(this, |f| {
                    if f.history_key == wanted {
                        f.history = Some(list);
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub fn create_session_from_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::NewSession(form)) = self.modal.as_mut() else { return };
        if form.busy {
            return;
        }
        let from_pr = form.from_pr;
        let picked = form.pr_picked.clone();
        if from_pr && picked.is_none() {
            form.error = Some("Pick the pull request first.".into());
            cx.notify();
            return;
        }
        let Some(Modal::NewSession(form)) = self.modal.as_mut() else { return };
        if form.on_server {
            let name = form.name.read(cx).text();
            let new_worktree = form.new_worktree && !from_pr;
            let resume = if new_worktree || from_pr { None } else { form.resume.clone() };
            let branch = form.branch.read(cx).text().trim().replace(' ', "-");
            let repo = self.server.state.as_ref().and_then(|s| s.repos.get(&form.project_id).cloned());
            let _ = &repo;
            let mut create = glasscode_core::remote::CreateSession {
                project_id: form.project_id.clone(),
                worktree: if new_worktree || from_pr { None } else { form.existing.clone() },
                new_branch: new_worktree.then_some(branch),
                agent: form.agent,
                account_id: form.agent.uses_accounts().then(|| form.account_id.clone()).flatten(),
                title: Some(name.trim().to_string()).filter(|n| !n.is_empty()).or_else(|| resume.as_ref().map(|c| c.title.chars().take(48).collect())),
                resume: resume.map(|c| c.id),
                initial_prompt: None,
                start: new_worktree.then_some(glasscode_core::remote::StartFrom::Main),
                review: None,
                review_files: Default::default(),
                model: None,
                mode: None,
                held: None,
            };
            form.busy = true;
            form.error = None;
            if let (true, Some(pr)) = (from_pr, picked) {
                create.title = create.title.or_else(|| Some(pr.title.chars().take(60).collect()));
                create.start = Some(glasscode_core::remote::StartFrom::PullRequest { number: pr.number, head: pr.head.clone(), same_repo: pr.same_repo });
            }
            self.create_server_session(create, window, cx);
            cx.notify();
            return;
        }
        let Some(project) = self.state.project(&form.project_id).cloned() else { return };
        let agent = form.agent;
        let account_id = if agent.uses_accounts() { form.account_id.clone() } else { None };
        if agent.uses_accounts() && account_id.is_none() {
            form.error = Some(format!("Add a {} account first (Accounts → Add).", agent.name()));
            cx.notify();
            return;
        }
        let name = form.name.read(cx).text();
        let plain = !project.root.join(".git").exists();
        let new_worktree = form.new_worktree && !from_pr && !plain;
        let resume = if new_worktree || from_pr { None } else { form.resume.clone() };

        let branch = form.branch.read(cx).text().trim().replace(' ', "-");
        let existing = form.existing.clone().unwrap_or(project.root.clone());
        form.busy = true;
        form.error = None;
        cx.notify();

        let root = project.root.clone();
        let project_name = project.name.clone();
        let env = self.env.clone();
        // Resuming on another account: copy the conversation over first, as a switch does.
        let copy = resume.as_ref().filter(|c| Some(&c.account_id) != account_id.as_ref()).and_then(|c| {
            let from = self.state.account(&c.account_id).cloned()?;
            let to = self.state.account(account_id.as_deref()?).cloned()?;
            Some((from, to, c.id.clone()))
        });
        let fetch = !self.state.prefs.fresh_main_off;
        let task = cx.background_spawn(async move {
            if let Some((from, to, id)) = &copy {
                agents::copy_conversation(agent, from, to, id)
                    .map_err(|e| anyhow::anyhow!("could not copy the conversation to {}: {e}", to.label))?;
            }
            let _ = &env;
            let no_review: Option<glasscode_core::model::ReviewRef> = None;
            match (from_pr, picked) {
                (true, Some(pr)) => {
                    let checkout = git::checkout_pr_branch(&root, &project_name, pr.number, &pr.head, pr.same_repo)?;
                    Ok::<_, anyhow::Error>((checkout.path, checkout.branch, Some(checkout.note), no_review, Some(pr)))
                }
                _ if new_worktree => {
                    let fresh = git::start_base(&root, fetch);
                    let note = (fetch && !fresh.fetched).then(|| format!("Could not fetch origin (offline?); the branch starts from {} as it is on {}.", fresh.base, crate::platform::this_computer()));
                    let path = git::add_worktree(&root, &project_name, &branch, &fresh.base)?;
                    Ok((path, branch, note, no_review, None))
                }
                _ => {
                    let branch = if git::repo_root(&existing).is_ok() { git::current_branch(&existing) } else { String::new() };
                    Ok((existing, branch, None, no_review, None))
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok((worktree, branch, note, review, pr)) => {
                    let title = match (name.trim(), &resume, &review, &pr) {
                        (_, _, Some(_), Some(pr)) => review_title(pr),
                        ("", _, None, Some(pr)) => pr.title.chars().take(60).collect(),
                        ("", Some(c), _, _) => c.title.chars().take(48).collect::<String>(),
                        ("", None, _, _) if branch.is_empty() => format!("{} · {}", agent.name(), worktree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
                        ("", None, _, _) => format!("{} · {}", agent.name(), branch),
                        (name, _, _, _) => name.to_string(),
                    };
                    let session = Session {
                        id: if name.trim().is_empty() { new_id() } else { this.claim_custom_title(new_id()) },
                        project_id: project.id.clone(),
                        worktree,
                        branch,
                        title,
                        agent,
                        account_id: account_id.clone(),
                        agent_session_id: resume.as_ref().map(|c| c.id.clone()).or_else(|| Workspace::new_session_id_for(agent)),
                        transcript_path: None,
                        lineage: vec![this.state.lineage_tag(agent, account_id.as_deref())],
                        initial_prompt: None,
                        team_task: None,
                        model: None,
                        mode: None,
                        review: review.clone(),
                        created_at: now(),
                    };
                    // Started to carry on with a pull request: its work is that pull request's (M32).
                    if let (true, None, Some(pr)) = (this.control.engine_v2, &review, &pr)
                        && let Some(repo) = glasscode_core::github::repo_of_pr_url(&pr.url)
                    {
                        this.state.pr_of.insert(session.id.clone(), glasscode_core::model::pr_ref(&repo, pr.number));
                    }
                    this.modal = None;
                    this.add_and_open_session(session, window, cx);
                    this.refresh_worktrees(cx);
                    if let Some(note) = note {
                        this.show_toast(note, cx);
                    }
                }
                Err(err) => {
                    with_new_session_form(this, |f| {
                        f.busy = false;
                        f.error = Some(if f.review_mode || f.from_pr {
                            format!("Could not check out the pull request: {err}")
                        } else if f.new_worktree {
                            format!("Could not create the worktree: {err}")
                        } else {
                            format!("Could not start the session: {err}")
                        });
                    });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn perform_handoff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Handoff(form)) = self.modal.as_ref() else { return };
        let Some(session) = self.state.session(&form.session_id).cloned() else { return };
        let Some(to_account) = form.to_account.clone() else { return };
        if session.agent == form.to_agent {
            if session.account_id.as_deref() == Some(to_account.as_str()) {
                return;
            }
            self.modal = None;
            self.switch_account(&to_account, window, cx);
            return;
        }
        let to_agent = form.to_agent;
        let new_tab = form.new_tab;
        let options = HandoffOptions {
            include_terminal: form.include_terminal,
            include_diff: form.include_diff,
            include_transcript: form.include_transcript,
            note: form.note.read(cx).text(),
        };
        self.modal = None;
        self.handoff_with(&session, to_agent, to_account, options, new_tab, window, cx);
    }

    /// Hand a session to another agent or account without the dialog (the `glasscode` command):
    /// the notes include its screen, diff and transcript.
    #[allow(clippy::too_many_arguments)]
    pub fn handoff_now(&mut self, session_id: &str, to_agent: AgentKind, to_account: String, note: String, new_tab: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.state.session(session_id).cloned() else { return };
        if session.agent == to_agent {
            if session.account_id.as_deref() != Some(to_account.as_str()) {
                if self.state.active_session.as_deref() != Some(session_id) {
                    self.select_session(session_id, window, cx);
                }
                self.switch_account(&to_account, window, cx);
            }
            return;
        }
        let options = HandoffOptions { include_terminal: true, include_diff: true, include_transcript: true, note };
        self.handoff_with(&session, to_agent, to_account, options, new_tab, window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn handoff_with(&mut self, session: &Session, to_agent: AgentKind, to_account: String, options: HandoffOptions, new_tab: bool, window: &mut Window, cx: &mut Context<Self>) {
        let session = session.clone();
        let from_label = self.state.lineage_tag(session.agent, session.account_id.as_deref());
        let to_label = self.state.lineage_tag(to_agent, Some(&to_account));
        let tail = self
            .terminals
            .get(&session.id)
            .map(|t| t.read(cx).text_tail(60))
            .unwrap_or_default();
        let packet = match handoff::write_packet(&session, &from_label, &to_label, &tail, &options) {
            Ok(packet) => packet,
            Err(err) => {
                self.show_toast(format!("Handoff failed: {err}"), cx);
                return;
            }
        };
        if new_tab {
            let mut lineage = session.lineage.clone();
            lineage.push(to_label.clone());
            let created = Session {
                id: new_id(),
                project_id: session.project_id.clone(),
                worktree: session.worktree.clone(),
                branch: session.branch.clone(),
                title: format!("{} · {}", session.title, to_agent.name()),
                agent: to_agent,
                account_id: Some(to_account),
                agent_session_id: Workspace::new_session_id_for(to_agent),
                transcript_path: None,
                lineage,
                initial_prompt: Some(packet.prompt),
                team_task: None,
                model: None,
                mode: None,
                review: None,
                created_at: now(),
            };
            self.add_session(created, window, cx);
        } else {
            if let Some(s) = self.state.session_mut(&session.id) {
                s.hand_to(to_agent, to_account, Workspace::new_session_id_for(to_agent), packet.prompt, to_label.clone());
            }
            self.chat_agent_changed(&session.id, cx);
            self.save();
            self.restart_session(&session.id, window, cx);
        }
        self.show_toast(
            format!("Handed off to {to_label}. Notes: {}", packet.path.display()),
            cx,
        );
    }

    pub fn add_account_from_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.settings.form.as_mut() else { return };
        let Some(agent) = form.adding else { return };
        let label = form.label.read(cx).text();
        let plan = form.plan.read(cx).text();
        let label = if label.trim().is_empty() {
            format!("{} account {}", agent.name(), self.state.accounts_for(agent).count() + 1)
        } else {
            label.trim().to_string()
        };
        let account = Account {
            id: format!("{}-{}", agent.name().to_lowercase(), &new_id()[..8]),
            agent,
            label,
            plan: (!plan.trim().is_empty()).then(|| plan.trim().to_string()),
            system_default: false,
        };
        let id = account.id.clone();
        self.state.accounts.push(account);
        if self.state.defaults.for_agent(agent).is_none() {
            self.state.defaults.set(agent, Some(id.clone()));
        }
        if let Some(form) = self.settings.form.as_mut() {
            form.adding = None;
        }
        self.save();
        self.start_login(&id, window, cx);
    }

    pub fn start_login(&mut self, account_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.state.account(account_id).cloned() else { return };
        if self.view != crate::shell::View::Settings || self.settings.section != crate::settings_ui::Section::Accounts {
            self.open_settings(crate::settings_ui::Section::Accounts, window, cx);
        }
        let launch = agents::login_launch(&account, &self.env);
        let spec = SpawnSpec {
            program: launch.program,
            args: launch.args,
            cwd: glasscode_core::paths::home(),
            env: launch.env,
        };
        match TerminalView::spawn(spec, cx) {
            Ok(terminal) => {
                let handle = terminal.read(cx).focus_handle_clone();
                window.focus(&handle, cx);
                let hint = match account.agent {
                    AgentKind::Claude => "type /login if it doesn't ask you to sign in",
                    _ => "a browser window opens to sign in",
                };
                if let Some(form) = self.settings.form.as_mut() {
                    form.terminal = Some((
                        format!("Sign in: {} · {} ({hint})", account.agent.name(), account.label),
                        terminal,
                    ));
                }
            }
            Err(err) => self.show_toast(format!("Could not start sign-in: {err}"), cx),
        }
        cx.notify();
    }

    /// Install a CLI the person chose. `version` picks a specific version (built-ins pin to it so
    /// Glasscode stops flagging it); `None` installs the newest / runs the custom install command.
    /// New sessions use it once they start (reopen the app, or start a fresh session). Running
    /// agents keep the version they launched with.
    pub fn cli_install(&mut self, name: String, version: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(status) = self.cli_status.iter().find(|s| s.name == name) else { return };
        let command = match &version {
            Some(v) if Some(v) == status.latest.as_ref() => status.install_latest_command(),
            Some(v) => status.install_command_for(v),
            None => status.install_latest_command(),
        };
        // Built-ins carry a pin so the version check matches what was just installed; custom
        // CLIs are never pinned.
        if status.builtin {
            if let Some(v) = &version {
                let mut approved = glasscode_core::versions::load_approved();
                match name.as_str() {
                    "Claude Code" => approved.claude = Some(v.clone()),
                    _ => approved.codex = Some(v.clone()),
                }
                if let Err(e) = glasscode_core::versions::save_approved(&approved) {
                    self.show_toast(format!("Could not save the version choice: {e}"), cx);
                }
            }
        }
        if let (Some(v), Some(s)) = (&version, self.cli_status.iter_mut().find(|s| s.name == name)) {
            if s.builtin {
                s.approved = Some(v.clone());
            }
        }
        self.run_in_accounts_terminal(&command, window, cx);
    }

    /// Open the "add a CLI" modal.
    pub fn open_add_cli(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = Some(Modal::add_cli(window, cx));
        cx.notify();
    }

    /// Switch the add-CLI modal between npm and custom modes.
    pub fn set_add_cli_mode(&mut self, custom: bool, cx: &mut Context<Self>) {
        if let Some(Modal::AddCli(form)) = self.modal.as_mut() {
            form.custom = custom;
            cx.notify();
        }
    }

    /// Save the CLI described in the add-CLI modal and re-check versions.
    pub fn add_cli_from_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use glasscode_core::versions::{CliSource, CustomCli};
        let Some(Modal::AddCli(form)) = self.modal.as_ref() else { return };
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_string();
        let cli = if form.custom {
            let name = text(&form.name);
            let install = text(&form.install);
            let version = text(&form.version);
            if name.is_empty() || install.is_empty() || version.is_empty() {
                self.show_toast("Give the CLI a name, an install command and a version command.", cx);
                return;
            }
            CustomCli { name, source: CliSource::Command { install, version } }
        } else {
            let package = text(&form.npm_package);
            if package.is_empty() {
                self.show_toast("Give the npm package name (e.g. prettier or @scope/name).", cx);
                return;
            }
            let bin = text(&form.npm_bin);
            CustomCli {
                name: package.clone(),
                source: CliSource::Npm { package, bin: (!bin.is_empty()).then_some(bin) },
            }
        };
        let name = cli.name.clone();
        if name == "Claude Code" || name == "Codex" {
            self.show_toast(format!("{name} is already built in."), cx);
            return;
        }
        if let Err(e) = glasscode_core::versions::add_cli(cli) {
            self.show_toast(format!("Could not save the CLI: {e}"), cx);
            return;
        }
        self.close_modal(window, cx);
        self.refresh_cli_status(cx);
        self.show_toast(format!("Added {name}."), cx);
    }

    /// Remove a custom CLI (the installed binary itself is left alone).
    pub fn remove_custom_cli(&mut self, name: String, cx: &mut Context<Self>) {
        if let Err(e) = glasscode_core::versions::remove_cli(&name) {
            self.show_toast(format!("Could not remove {name}: {e}"), cx);
            return;
        }
        self.cli_status.retain(|s| s.builtin || s.name != name);
        self.show_toast(format!("Removed {name}. The installed binary was left in place."), cx);
        cx.notify();
    }

    pub fn run_in_accounts_terminal(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (program, args) = glasscode_core::versions::terminal_command(command);
        let spec = SpawnSpec {
            program,
            args,
            cwd: glasscode_core::paths::home(),
            env: (*self.env).clone(),
        };
        if let Ok(terminal) = TerminalView::spawn(spec, cx) {
            let handle = terminal.read(cx).focus_handle_clone();
            window.focus(&handle, cx);
            // When the install finishes, re-check the installed versions so the row
            // updates (and the "Update to…" button clears) without a restart.
            cx.subscribe(&terminal, |this, _, event, cx| {
                if matches!(event, crate::terminal::TerminalEvent::Exited(_)) {
                    this.refresh_cli_status(cx);
                }
            })
            .detach();
            // The terminal lives on the accounts form; create it if the person came
            // straight to this page, otherwise the spawned install would be dropped.
            if self.settings.form.is_none() {
                self.settings.form = Some(AccountsForm::new(window, cx));
            }
            if let Some(form) = self.settings.form.as_mut() {
                form.terminal = Some((format!("Running: {command}"), terminal));
            }
        }
        cx.notify();
    }

    pub fn remove_account(&mut self, account_id: &str, cx: &mut Context<Self>) {
        let Some(account) = self.state.account(account_id).cloned() else { return };
        self.state.accounts.retain(|a| a.id != account_id);
        if self.state.defaults.for_agent(account.agent) == Some(&account.id) {
            let next = self.state.accounts_for(account.agent).next().map(|a| a.id.clone());
            self.state.defaults.set(account.agent, next);
        }
        self.save();
        self.show_toast(
            format!(
                "Removed {} from Glasscode. Its folder {} was kept.",
                account.label,
                account.effective_dir().display()
            ),
            cx,
        );
    }
}
