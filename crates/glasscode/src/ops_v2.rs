//! The handlers of the ops the v2 engine adds (M33.2), keyed by op name like `ops_run.rs`'s: what
//! the window shows (`ui.*`), a session's prompt, keys and mode, the typed reads, the accounts'
//! usage and the settings by name.
//!
//! Who is the person is read from the channel a request came in on (`caller.channel`), never from
//! v1's trust: these ops exist only with the engine at v2. A caller that is not the person reveals
//! a place without taking the keyboard or clearing what needs the person, sends a few keys only,
//! never picks an option that grants something for good, never raises a session's mode, and never
//! changes a setting of the gate. Reads that run git or gh, or read a file, run on the background
//! executor; what they return is data (`reads::as_data`).

use std::path::PathBuf;
use std::time::Duration;

use gpui::{AppContext as _, Context, Window};
use glasscode_core::control::{Kind, Reply, code};
use glasscode_core::model::AgentKind;
use glasscode_core::ops::{ErrorCode, Op, OpError};
use glasscode_core::prompt::{self, PendingPrompt};
use glasscode_core::reads;
use glasscode_core::settings;
use serde_json::{Value, json};

use crate::capabilities::Place;
use crate::control_ui::Caller;
use crate::ops_run::{Handler, Planned, error_reply, failed, plan, usage};
use crate::shell::{CenterTab, View};
use crate::workspace::{RightTab, Side, Workspace};

/// The handlers of the v2 engine's ops. `ops_run::HANDLERS` has v1's; a test checks that each op
/// has one handler in all (or none, while it waits for its slice).
pub(crate) const HANDLERS: &[(&str, Handler)] = &[
    ("ui.state", ui_state),
    ("ui.open", ui_open),
    ("ui.back", ui_back),
    ("ui.forward", ui_forward),
    ("ui.panel", ui_panel),
    ("ui.point", crate::ops_run::manager_show),
    ("ui.say", crate::ops_run::manager_say),
    ("ui.tour", crate::ops_run::manager_tour),
    ("ui.scene", crate::ops_run::manager_scene),
    ("session.answer", session_answer),
    ("session.interrupt", session_interrupt),
    ("session.keys", session_keys),
    ("session.mode", session_mode),
    ("pr.view", pr_view),
    ("pr.checks", pr_checks),
    ("pr.diff", pr_diff),
    ("review.threads", review_threads),
    ("ci.log", ci_log),
    ("git.status", git_status),
    ("git.log", git_log),
    ("git.diff", git_diff),
    ("file.read", file_read),
    ("usage.get", usage_get),
    ("settings.get", settings_get),
    ("settings.set", settings_set),
    ("session.report", session_report),
    ("policy.set", trust_set),
    ("playbook.set", trust_set),
    ("schedule.set", trust_set),
    ("manager.wake", manager_wake),
    ("work.list", work_list),
    ("work.show", work_show),
    ("work.done", work_done),
    ("work.drop", work_drop),
    ("work.reopen", work_reopen),
    ("ops.cancel", ops_cancel),
];

/// `cancel <id>` (ops-10): a long op its caller started stops at its next step, or a request that
/// waits for the person is taken back; the caller's own, or any for the person
/// (`Workspace::control_cancel`).
fn ops_cancel(_: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let id = op.number("id").unwrap_or_default();
    let caller = caller.clone();
    plan(Kind::Read, "read", move |ws, r, _, cx| {
        let reply = ws.control_cancel(id, &caller, cx);
        ws.reply(r, reply);
    })
}

/// `manager wake "<reason>" [--project <p>]` (M51): a wake for the manager's wake lane, from a
/// schedule, a playbook or a command. It queues; the lane runs it when it may (never while the
/// person talks to the manager), as a short look that reads and proposes.
fn manager_wake(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let reason = text(op, "reason").trim().to_string();
    if reason.is_empty() {
        return Err(usage("manager wake \"<reason>\" [--project <p>]"));
    }
    // A wake does not wake the next, nor the chat itself: the manager's lanes have their own way.
    // A schedule's or a playbook's run may (Glasscode's own, approved by the person).
    let playbook = match &caller.channel {
        glasscode_core::caller::Caller::Manager { lane: glasscode_core::caller::Lane::Wake, wake: Some(wake), .. } if !glasscode_core::runtime::lanes::brain_wake(wake) => wake.strip_prefix("playbook:").map(str::to_string),
        glasscode_core::caller::Caller::Manager { .. } => return Err(error(ErrorCode::Denied, "The manager does not wake itself: a wake proposes what is next, and the chat answers now.")),
        _ => None,
    };
    let projects: Vec<String> = glasscode_core::ops::targets(op, &ws.world(caller)).map(|t| t.projects.iter().map(|k| k.as_str().to_string()).collect()).unwrap_or_default();
    let source = if person(caller) { "you asked".to_string() } else { format!("{} asked", caller.describe()) };
    let outside = !person(caller);
    plan(Kind::Board, format!("wake the manager: {reason}"), move |ws, r, _, cx| {
        if !ws.lanes.running() {
            return ws.reply(r, Reply::fail(code::NOT_RUNNING, "The manager's wake lane is not running: the manager is off, or its brain is not Claude."));
        }
        let now = glasscode_core::clock::now_ms();
        let mut wake = glasscode_core::runtime::queue::Wake::new(format!("asked:{}", glasscode_core::effects::effect_key(&reason, &projects.join(","), "manager.wake", "").hex()), glasscode_core::runtime::queue::Priority::Playbook, now);
        wake.projects = projects;
        wake.playbook = playbook;
        wake.notes.push(glasscode_core::runtime::queue::Note { source, text: reason.clone(), outside });
        ws.lanes_wake(wake);
        cx.notify();
        ws.reply(r, Reply::ok(format!("The manager will look: {reason}."), json!({ "queued": true })));
    })
}

/// `glasscode report done|blocked|needs-review "<line>"` (M30, events-23): what a session says of
/// its own work, kept in the journal as a hint with whether its signature checked out. It never
/// counts as evidence; one every 30 s per session.
fn session_report(_ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let words = "report done|blocked|needs-review \"<line>\"";
    let state = text(op, "state").to_lowercase();
    if !glasscode_core::turns::REPORT_STATES.contains(&state.as_str()) {
        return Err(usage(words));
    }
    let line = text(op, "line").trim().to_string();
    if line.is_empty() {
        return Err(usage(words));
    }
    let Some(session) = caller.session.clone() else {
        return Err(failed("Only a session reports on its own work: run it inside one."));
    };
    let signed = matches!(caller.channel, glasscode_core::caller::Caller::Session { .. } | glasscode_core::caller::Caller::ShellTab { .. });
    plan(Kind::Read, "read", move |ws, r, _, _| {
        let reply = match ws.journal_self_report(&session, &state, &line, signed) {
            Ok(()) => Reply::ok("Noted for the manager as a hint; what the session did is the evidence.", json!({ "session": session, "state": state, "hint": true, "signed": signed })),
            Err(why) => failed(why),
        };
        ws.reply(r, reply);
    })
}

/// `policy set`, `playbook set` and `schedule set` (M53): the change's preview, and with
/// `--confirm` the change. The person's only (the ops are `human_only`); what they write is
/// approved as it is.
fn trust_set(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    use glasscode_core::trust::{self, Change};
    if !person(caller) {
        return Err(error(ErrorCode::Denied, "Only you change what the manager may do, in Settings or at your own terminal."));
    }
    let change = match op.name() {
        "policy.set" => {
            let word = text(op, "project");
            let project = ws.state.projects.iter().find(|p| p.id == word || p.name.eq_ignore_ascii_case(word)).ok_or_else(|| error(ErrorCode::NotFound, format!("No project “{word}”. `glasscode project list` lists them.")))?;
            let key = ws.control.project_keys.get(&project.id).cloned().ok_or_else(|| failed(format!("{} has no project key yet, so its policy cannot change.", project.name)))?;
            let given = [op.text("level").is_some(), op.text("condition").is_some(), op.number("remove_condition").is_some()].iter().filter(|g| **g).count();
            if given != 1 {
                return Err(usage("Say one change: --level, --condition or --remove-condition."));
            }
            if let Some(level) = op.text("level") {
                let level = glasscode_core::gate::Level::parse(level).ok_or_else(|| usage(format!("“{level}” is no level: careful, standard or autopilot.")))?;
                Change::Level { project: key, level }
            } else if let Some(condition) = op.text("condition") {
                Change::AddCondition { project: key, condition: trust::parse_condition(condition).map_err(usage)? }
            } else {
                let n = op.number("remove_condition").unwrap_or(0);
                if n == 0 {
                    return Err(usage("Conditions count from 1."));
                }
                Change::RemoveCondition { project: key, index: n as usize - 1 }
            }
        }
        name => {
            let id = text(op, if name == "playbook.set" { "playbook" } else { "schedule" }).to_string();
            let state = text(op, "state").to_lowercase();
            match (name, state.as_str()) {
                ("playbook.set", "on" | "off") => Change::Playbook { id, on: state == "on" },
                ("playbook.set", "approve") => Change::ApprovePlaybook { id },
                (_, "on" | "off") => Change::Schedule { id, on: state == "on" },
                (_, "approve") => Change::ApproveSchedule { id },
                _ => return Err(usage("Say on, off or approve.")),
            }
        }
    };
    let confirm = op.on("confirm");
    let policy = {
        let authority = &ws.control.authority;
        authority.approved.clone().or_else(|| (!authority.live_differs).then(|| authority.policy.clone())).ok_or_else(|| failed("The approved policy could not be read, so nothing can change until it is repaired."))?
    };
    let forge = ws.control_policy_set().forge;
    plan(Kind::Settings, "change what the manager may do", move |_ws, r, _, cx| {
        let task = cx.background_spawn(async move {
            use glasscode_core::store::Store as _;
            let book = glasscode_core::playbooks::load(&glasscode_core::playbooks::model::journal_schema()).ok();
            let schedules = glasscode_core::schedule::ScheduleFile::load().ok();
            let world = trust::World { policy: &policy, book: book.as_ref(), schedules: schedules.as_ref(), forge: &forge };
            let pending = trust::prepare(change, &world)?;
            let done = if confirm { Some(trust::apply(&pending, true)?) } else { None };
            let authority = done.is_some().then(glasscode_core::gate::load);
            Ok::<_, String>((pending, done, authority))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                let reply = match result {
                    Ok((pending, done, authority)) => {
                        let mut text = vec![pending.title.clone()];
                        text.extend(pending.lines.iter().map(|line| format!("  {line}")));
                        text.push(done.clone().unwrap_or_else(|| "Nothing changed yet: give the same command with --confirm to make it.".into()));
                        if let Some(authority) = authority {
                            ws.control_set_authority(authority, cx);
                        }
                        Reply::ok(text.join("\n"), json!({ "change": pending.change, "preview": pending.lines, "changed": done.is_some() }))
                    }
                    Err(why) => failed(why),
                };
                ws.reply(r, reply);
            });
        })
        .detach();
    })
}

/// The person, by the channel: the app itself, or their own command in a Glasscode shell tab.
fn person(caller: &Caller) -> bool {
    caller.channel.is_person()
}

fn error(code: ErrorCode, text: impl Into<String>) -> Reply {
    error_reply(&OpError::new(code, text))
}

fn text<'a>(op: &'a Op, name: &str) -> &'a str {
    op.text(name).unwrap_or_default()
}

// ----- ui.state ------------------------------------------------------------------------------

/// What `ui.state` is made from: names, ids and counts the workspace holds, gathered on the main
/// thread from memory. Text the person typed is kept only as its length, a prompt only as its id.
#[derive(Clone, Debug, Default)]
pub(crate) struct Seen {
    pub view: String,
    pub session: Option<(String, String)>,
    pub center: Option<Value>,
    pub right_panel: (bool, String),
    pub left_sidebar: bool,
    pub popover: Option<String>,
    pub modal: Option<String>,
    pub jump: Option<usize>,
    pub settings_section: Option<String>,
    pub selected: Option<Value>,
    pub commit_draft: usize,
    pub approvals: usize,
    pub toast: bool,
    /// The prompt on the visible session's screen: its session and the prompt as parsed.
    pub prompt: Option<(String, PendingPrompt)>,
    pub window_active: bool,
}

/// `ui.state`'s data (version 1): no text of the screen, nothing the person typed.
pub(crate) fn state_of(seen: &Seen) -> Value {
    json!({
        "v": 1,
        "view": seen.view,
        "session": seen.session.as_ref().map(|(id, title)| json!({ "id": id, "title": title })),
        "center_tab": seen.center,
        "right_panel": { "open": seen.right_panel.0, "tab": seen.right_panel.1 },
        "left_sidebar": seen.left_sidebar,
        "popover": seen.popover,
        "modal": seen.modal,
        "jump": seen.jump.map(|typed| json!({ "open": true, "typed": typed })),
        "settings_section": seen.settings_section,
        "selected": seen.selected,
        "commit_draft_length": seen.commit_draft,
        "approvals": seen.approvals,
        "toast": seen.toast,
        "prompt": seen.prompt.as_ref().map(|(session, prompt)| json!({ "session": session, "prompt_id": prompt::prompt_id(session, prompt) })),
        "presence": { "window_active": seen.window_active, "last_input_ms": Value::Null },
    })
}

fn tab_word(tab: RightTab) -> String {
    Place::Tab(tab).name().trim_start_matches("tab:").to_string()
}

fn modal_name(modal: &crate::dialogs::Modal) -> &'static str {
    use crate::dialogs::Modal;
    match modal {
        Modal::NewSession(_) => "new_session",
        Modal::Handoff(_) => "handoff",
        Modal::RemoveWorktrees(_) => "remove_worktrees",
        Modal::Server(_) => "server",
        Modal::Switch => "switch",
        Modal::Browser => "browser",
        Modal::Rename(_) => "rename",
        Modal::StartTicket(_) => "start_ticket",
        Modal::ConfirmRemoveProject { .. } => "remove_project",
        Modal::AddCli(_) => "add_cli",
        #[allow(unreachable_patterns)]
        _ => "dialog",
    }
}

impl Workspace {
    /// What the window shows now, from memory ([`Seen`]); a hidden project's session is not named.
    pub(crate) fn ui_seen(&self, cx: &gpui::App) -> Seen {
        let visible = |id: &str| self.state.session(id).filter(|s| !self.state.session_hidden(s));
        let session = self.state.active_session.as_deref().and_then(visible).map(|s| (s.id.clone(), self.session_title(s)));
        let center = match self.tabs.get(self.tab) {
            Some(CenterTab::Session(id)) => visible(id).map(|s| json!({ "session": s.id })),
            Some(CenterTab::File(path)) => session.as_ref().map(|_| json!({ "file": path })),
            None => None,
        };
        let prompt = session.as_ref().filter(|_| self.view == View::Sessions).and_then(|(id, _)| {
            let terminal = self.terminals.get(id)?;
            prompt::parse(&terminal.read(cx).text_tail(60)).map(|p| (id.clone(), p))
        });
        let selected = self.tickets.selected.as_ref().map(|key| match key {
            crate::tickets_ui::Key::Ticket(id) => json!({ "ticket": id }),
            crate::tickets_ui::Key::Pr(repo, n) => json!({ "pr": { "repo": repo, "number": n } }),
        });
        Seen {
            view: Place::View(self.view).name(),
            session,
            center,
            right_panel: (self.show_right, tab_word(self.right_tab)),
            left_sidebar: !self.state.layout.left_hidden,
            popover: self.popover.map(|p| format!("{p:?}").to_lowercase()),
            modal: self.modal.as_ref().map(|m| modal_name(m).to_string()),
            jump: self.palette.as_ref().map(|p| p.input.read(cx).text().chars().count()),
            settings_section: (self.view == View::Settings).then(|| self.settings.section.slug()),
            selected,
            commit_draft: self.commit_input.read(cx).text().chars().count(),
            approvals: self.control.approvals.len(),
            toast: self.toast.is_some(),
            prompt,
            window_active: self.window_active,
        }
    }

    /// The place on screen, as `ui.open` names it (for an undo).
    fn ui_place_now(&self) -> String {
        match (self.view, self.state.active_session.as_deref()) {
            (View::Sessions, Some(id)) => id.to_string(),
            (View::Settings, _) => Place::Settings(self.settings.section).name(),
            (view, _) => Place::View(view).name(),
        }
    }

    /// The person is busy in the window (a dialog or Jump is open, or they look at a session and
    /// may be typing in it): a caller that is not the person does not move what they see.
    fn ui_person_busy(&self) -> bool {
        self.window_active && (self.modal.is_some() || self.palette.is_some() || (self.view == View::Sessions && self.state.active_session.is_some()))
    }

    /// Show a session without taking the keyboard, clearing its "needs you" mark or dismissing
    /// its notices (ops-13): only the person's own going there marks it seen.
    fn reveal_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.enter_session(id, true);
        self.center = crate::center::Center::Session;
        self.state.active_session = Some(id.to_string());
        self.ensure_terminal(id, window, cx);
        self.save();
        cx.notify();
    }
}

fn ui_state(_: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    plan(Kind::Read, "read", |ws, r, _, cx| {
        let state = state_of(&ws.ui_seen(cx));
        let session = state["session"]["title"].as_str().map(|t| format!(", the session “{t}”")).unwrap_or_default();
        let text = format!("The window shows {}{session}.", state["view"].as_str().unwrap_or_default());
        ws.reply(r, Reply::ok(text, state));
    })
}

/// Where `ui.open` goes.
enum Target {
    Place(Place),
    Session(String),
}

fn ui_open(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let word = text(op, "place").trim().to_string();
    let go = op.on("go");
    if go && !person(caller) {
        return Err(error(ErrorCode::NeedsPerson, "Only you go somewhere with --go. Without it the place is shown and your keyboard stays where it is."));
    }
    let target = match Place::parse(&word) {
        Some(Place::View(View::FirstRun)) if !person(caller) => return Err(error(ErrorCode::NeedsPerson, "Only you can go through the first run.")),
        Some(place) => Target::Place(place),
        None => Target::Session(ws.find_session(&word, caller)?),
    };
    let name = match &target {
        Target::Place(place) => place.name(),
        Target::Session(id) => ws.state.session(id).map(|s| format!("the session “{}”", ws.session_title(s))).unwrap_or_default(),
    };
    let before = ws.ui_place_now();
    let who = caller.describe();
    let person = person(caller);
    plan(Kind::Show, format!("show {name} in the window"), move |ws, r, window, cx| {
        if !person && ws.ui_person_busy() {
            ws.show_toast(format!("{who} would show you {name}: open Jump or the sidebar to go there."), cx);
            ws.reply(r, Reply::ok(format!("Not shown now: the person is busy in the window, so they were told instead ({name})."), json!({ "place": word, "deferred": true })));
            return;
        }
        match target {
            Target::Session(id) if go => ws.open_session(&id, window, cx),
            Target::Session(id) => ws.reveal_session(&id, window, cx),
            Target::Place(Place::View(view)) if go => ws.go(view, window, cx),
            Target::Place(Place::View(view)) => match view {
                View::Today => ws.show_today(cx),
                View::Board => ws.show_tickets(cx),
                View::PullRequests => ws.show_prs(cx),
                View::Sessions => {
                    ws.center = crate::center::Center::Session;
                    ws.show_view(View::Sessions, cx);
                }
                View::Manager => ws.show_manager(window, cx),
                other => ws.show_view(other, cx),
            },
            Target::Place(Place::Settings(section)) => ws.open_settings(section, window, cx),
            Target::Place(Place::Tab(tab)) => {
                ws.right_tab = tab;
                ws.show_right = true;
                cx.notify();
            }
        }
        ws.reply(r, Reply::ok(format!("Showing {name}."), json!({ "place": word, "deferred": false, "undo": { "op": "ui.open", "args": { "place": before } } })));
    })
}

fn ui_back(_: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    plan(Kind::Show, "go back in the window", |ws, r, window, cx| {
        ws.go_back(window, cx);
        ws.reply(r, Reply::ok("Back.", json!({ "undo": { "op": "ui.forward", "args": {} } })));
    })
}

fn ui_forward(_: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    plan(Kind::Show, "go forward in the window", |ws, r, window, cx| {
        ws.go_forward(window, cx);
        ws.reply(r, Reply::ok("Forward.", json!({ "undo": { "op": "ui.back", "args": {} } })));
    })
}

fn ui_panel(_: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let side = match text(op, "side").to_lowercase().as_str() {
        "left" => Side::Left,
        "right" => Side::Right,
        other => return Err(usage(format!("ui panel left|right [--hide] (not “{other}”)."))),
    };
    let hide = op.on("hide");
    let (word, verb) = (if side == Side::Left { "left" } else { "right" }, if hide { "hide" } else { "show" });
    plan(Kind::Show, format!("{verb} the {word} panel"), move |ws, r, _, cx| {
        match side {
            Side::Right => ws.show_right = !hide,
            _ => ws.set_hidden(side, hide),
        }
        ws.save();
        cx.notify();
        ws.reply(r, Reply::ok(format!("The {word} panel is {}.", if hide { "hidden" } else { "shown" }), json!({ "undo": { "op": "ui.panel", "args": { "side": word, "hide": !hide } } })));
    })
}

// ----- a session's prompt, keys and mode ------------------------------------------------------

/// A running session the op names: its terminal, title and agent.
fn running(ws: &Workspace, op: &Op) -> Result<(String, String, AgentKind, gpui::Entity<crate::terminal::TerminalView>), Reply> {
    let id = text(op, "session").to_string();
    let session = ws.state.session(&id).ok_or_else(|| failed(format!("No session {id}.")))?;
    let title = ws.session_title(session);
    let terminal = ws.terminals.get(&id).cloned().ok_or_else(|| failed(format!("“{title}” is not running.")))?;
    Ok((id, title, session.agent, terminal))
}

/// The terminal's bytes for a key a caller names, and whether only the person may press it.
fn key_of(word: &str) -> Option<(&'static [u8], bool)> {
    let name = match word.to_lowercase().as_str() {
        "esc" | "escape" => "Escape",
        "enter" | "return" => "Enter",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "tab" => "Tab",
        "shift-tab" | "btab" => return glasscode_core::mirror::key_bytes("BTab").map(|b| (b, true)),
        "ctrl-c" | "c-c" => return glasscode_core::mirror::key_bytes("C-c").map(|b| (b, true)),
        _ => return None,
    };
    glasscode_core::mirror::key_bytes(name).map(|b| (b, false))
}

fn session_answer(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let (id, title, _, terminal) = running(ws, op)?;
    let (prompt_id, label) = (text(op, "prompt").to_string(), text(op, "option").trim().to_string());
    let extra = op.text("text").map(str::to_string).filter(|t| !t.trim().is_empty());
    let caller = caller.clone();
    plan(Kind::Send, format!("answer “{label}” in the session “{title}”"), move |ws, r, _, cx| {
        let screen = terminal.read(cx).text_tail(60);
        let (now, option) = match prompt::answer_on(&screen, &id, &prompt_id, &label) {
            Ok(found) => found,
            Err(prompt::Refused::Stale) => return ws.reply(r, error(ErrorCode::StalePrompt, format!("The prompt in “{title}” is not the one you read ({prompt_id}); nothing was sent. Read it again (ui state, session show)."))),
            Err(prompt::Refused::NoOption(labels)) => return ws.reply(r, usage(format!("The prompt in “{title}” has no option “{label}”; it has: {}.", labels.join("; ")))),
        };
        if prompt::grants_for_good(&option) && !person(&caller) {
            return ws.reply(r, error(ErrorCode::NeedsPerson, format!("“{}” allows it for good: only you choose that, in “{title}”.", option.label)));
        }
        if option.wants_text && extra.is_none() {
            return ws.reply(r, usage(format!("“{}” asks what to do instead: give it with --text.", option.label)));
        }
        let Some(keys) = prompt::keys_for(&now, option.number) else { return ws.reply(r, failed("The option cannot be reached with the keys.")) };
        let origin = glasscode_core::words::command_origin(&caller.channel);
        cx.spawn(async move |this, cx| {
            let last = keys.len() - 1;
            for (i, key) in keys.iter().enumerate() {
                // The screen is read again before each key: a changed prompt gets no more keys.
                let still = this.update(cx, |_, cx| prompt::answer_on(&terminal.read(cx).text_tail(60), &id, &prompt_id, &label).is_ok()).unwrap_or(false);
                if !still {
                    let _ = this.update(cx, |ws, _| ws.reply(r, error(ErrorCode::StalePrompt, format!("The prompt in “{title}” changed while it was answered; the keys stopped there."))));
                    return;
                }
                let Some(bytes) = glasscode_core::mirror::key_bytes(key) else { continue };
                let _ = terminal.update(cx, |t, cx| t.write(bytes.to_vec(), cx));
                if i < last {
                    cx.background_executor().timer(Duration::from_millis(80)).await;
                }
            }
            if let Some(extra) = extra {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                glasscode_core::words::record(origin, &extra, Some(&id), None);
                let _ = terminal.update(cx, |t, cx| t.paste_text(&extra, cx));
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
            }
            let _ = this.update(cx, |ws, _| ws.reply(r, Reply::ok(format!("Answered “{}” in “{title}”.", option.label), json!({ "session": id, "prompt_id": prompt_id, "option": option.number }))));
        })
        .detach();
    })
}

fn session_interrupt(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let (id, title, agent, terminal) = running(ws, op)?;
    plan(Kind::Send, format!("stop what “{title}” is doing"), move |ws, r, _, cx| {
        // A shell stops with Ctrl-C; Claude and Codex with Esc.
        let key = if agent == AgentKind::Shell { "C-c" } else { "Escape" };
        if let Some(bytes) = glasscode_core::mirror::key_bytes(key) {
            terminal.update(cx, |t, cx| t.write(bytes.to_vec(), cx));
        }
        ws.reply(r, Reply::ok(format!("Stopped “{title}”."), json!({ "session": id })));
    })
}

fn session_keys(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let (id, title, _, terminal) = running(ws, op)?;
    let words = op.texts("keys");
    let mut keys = Vec::new();
    for word in &words {
        let Some((bytes, persons)) = key_of(word) else { return Err(usage(format!("“{word}” is not a key it presses: esc, enter, up, down, left, right, tab (and for you shift-tab, ctrl-c)."))) };
        if persons && !person(caller) {
            return Err(error(ErrorCode::NeedsPerson, format!("Only you press {word} in a session.")));
        }
        keys.push(bytes);
    }
    let shown = words.join(" ");
    // On an open question, a caller that is not the person may only dismiss it (Esc): an answer
    // names the prompt it was decided on (`session.answer`), and keys alone could choose an option
    // that allows something for good.
    let only_esc = !person(caller) && words.iter().any(|w| !matches!(w.to_lowercase().as_str(), "esc" | "escape"));
    plan(Kind::Send, format!("press {shown} in “{title}”"), move |ws, r, _, cx| {
        if only_esc && prompt::parse(&terminal.read(cx).text_tail(60)).is_some() {
            return ws.reply(r, error(ErrorCode::NeedsPerson, format!("“{title}” is asking something: answer it with session answer and its prompt id, or let the person; only esc is pressed on an open question.")));
        }
        cx.spawn(async move |this, cx| {
            for bytes in keys {
                let _ = terminal.update(cx, |t, cx| t.write(bytes.to_vec(), cx));
                cx.background_executor().timer(Duration::from_millis(80)).await;
            }
            let _ = this.update(cx, |ws, _| ws.reply(r, Reply::ok(format!("Pressed {shown} in “{title}”."), json!({ "session": id }))));
        })
        .detach();
    })
}

/// How much a Claude mode lets the agent do without asking: plan 0, default 1, accept edits 2,
/// auto 3, bypass 4.
fn mode_rank(mode: &str) -> Option<u8> {
    match mode.to_lowercase().replace([' ', '_'], "-").as_str() {
        "plan" | "plan-mode" => Some(0),
        "default" => Some(1),
        "accept-edits" => Some(2),
        "auto" | "auto-mode" => Some(3),
        "bypass" | "bypass-permissions" => Some(4),
        _ => None,
    }
}

fn session_mode(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let (id, title, agent, terminal) = running(ws, op)?;
    if agent != AgentKind::Claude {
        return Err(failed(format!("“{title}” is not a Claude session: only Claude's mode can be set.")));
    }
    let target = text(op, "mode").to_lowercase();
    let wanted = match mode_rank(&target) {
        Some(rank @ 0..=2) => rank,
        _ => return Err(usage("session mode <session> plan|default|accept-edits")),
    };
    plan(Kind::Send, format!("put “{title}” in {target} mode"), move |ws, r, _, cx| {
        let now = crate::chat_ui::mode_line(&terminal.read(cx).text_tail(12)).and_then(|m| mode_rank(&m));
        let Some(now) = now else { return ws.reply(r, failed(format!("The mode of “{title}” cannot be read from its screen; nothing was pressed."))) };
        if wanted > now {
            return ws.reply(r, error(ErrorCode::Denied, format!("{target} asks less than “{title}” does now: a session's mode is never raised. Only you can, in the session.")));
        }
        cx.spawn(async move |this, cx| {
            // Shift-Tab cycles the modes; at most a full round.
            for _ in 0..5 {
                let at = this.update(cx, |_, cx| crate::chat_ui::mode_line(&terminal.read(cx).text_tail(12)).and_then(|m| mode_rank(&m))).ok().flatten();
                if at == Some(wanted) {
                    let _ = this.update(cx, |ws, _| ws.reply(r, Reply::ok(format!("“{title}” is in {target} mode."), json!({ "session": id, "mode": target }))));
                    return;
                }
                if let Some(bytes) = glasscode_core::mirror::key_bytes("BTab") {
                    let _ = terminal.update(cx, |t, cx| t.write(bytes.to_vec(), cx));
                }
                cx.background_executor().timer(Duration::from_millis(300)).await;
            }
            let _ = this.update(cx, |ws, _| ws.reply(r, failed(format!("“{title}” did not reach {target} mode."))));
        })
        .detach();
    })
}

// ----- typed reads -----------------------------------------------------------------------------

/// The repository an op names, or its project's.
fn repo_for(ws: &Workspace, caller: &Caller, op: &Op) -> Result<String, Reply> {
    if let Some(repo) = op.text("repo").map(str::trim).filter(|r| r.split('/').count() == 2) {
        return Ok(repo.to_string());
    }
    let project = ws.project_for(caller, op.text("project"))?;
    let root = ws.state.project(&project).map(|p| p.root.clone()).unwrap_or_default();
    ws.tickets_repo_of(&root).ok_or_else(|| failed("Its project has no GitHub repository Glasscode knows; give --repo owner/name."))
}

/// A read that runs off the main thread and answers with its data.
fn read_off_main(what: String, work: impl FnOnce() -> Result<(String, Value), String> + Send + 'static) -> Planned {
    plan(Kind::Read, what, move |_, r, _, cx| {
        let task = cx.background_spawn(async move { work() });
        cx.spawn(async move |this, cx| {
            let reply = match task.await {
                Ok((text, data)) => Reply::ok(text, data),
                Err(why) => failed(why),
            };
            let _ = this.update(cx, |ws, _| ws.reply(r, reply));
        })
        .detach();
    })
}

fn pr_read(ws: &mut Workspace, caller: &Caller, op: &Op, kind: &'static str) -> Planned {
    let repo = repo_for(ws, caller, op)?;
    let number = op.number("pr").unwrap_or_default();
    let env = (*ws.env).clone();
    read_off_main("read".into(), move || {
        let source = format!("{kind} of {repo}#{number}");
        match kind {
            "The pull request" => reads::pr_view(&repo, number, &env).map(|v| (reads::as_data(&source, &serde_json::to_string_pretty(&v).unwrap_or_default()), v)),
            "The checks" => reads::pr_checks(&repo, number, &env).map(|v| (reads::as_data(&source, &serde_json::to_string_pretty(&v).unwrap_or_default()), v)),
            "The review threads" => reads::review_threads(&repo, number, &env).map(|v| (reads::as_data(&source, &serde_json::to_string_pretty(&v).unwrap_or_default()), json!({ "threads": v }))),
            _ => reads::pr_diff(&repo, number, &env).map(|diff| (reads::as_data(&source, &diff), json!({ "repo": repo, "pr": number, "diff": diff }))),
        }
        .map_err(|e| format!("{source}: {e:#}"))
    })
}

fn pr_view(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    pr_read(ws, caller, op, "The pull request")
}

fn pr_checks(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    pr_read(ws, caller, op, "The checks")
}

fn pr_diff(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    pr_read(ws, caller, op, "The diff")
}

fn review_threads(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    pr_read(ws, caller, op, "The review threads")
}

fn ci_log(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let repo = repo_for(ws, caller, op)?;
    let run = op.number("run").unwrap_or_default();
    let env = (*ws.env).clone();
    read_off_main("read".into(), move || {
        let source = format!("The failed log of run {run} of {repo}");
        reads::ci_log(&repo, run, &env).map(|log| (reads::as_data(&source, &log), json!({ "repo": repo, "run": run, "log": log }))).map_err(|e| format!("{source}: {e:#}"))
    })
}

/// The folder of the worktree an op names.
fn worktree_dir(ws: &Workspace, caller: &Caller, op: &Op) -> Result<(PathBuf, String), Reply> {
    let project = op.text("project").map(|p| ws.project_for(caller, Some(p))).transpose()?;
    let (_, wt) = ws.find_worktree_as(caller, text(op, "worktree"), project.as_deref())?;
    let label = if wt.branch.is_empty() { wt.path.display().to_string() } else { wt.branch.clone() };
    Ok((wt.path, label))
}

fn git_status(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let (dir, label) = worktree_dir(ws, caller, op)?;
    read_off_main("read".into(), move || {
        let status = reads::git_status(&dir).map_err(|e| format!("{label}: {e:#}"))?;
        let files = status["files"].as_array().map_or(0, Vec::len);
        let text = format!("{label}: {} changed {}; ahead {}, behind {}.", files, if files == 1 { "file" } else { "files" }, status["ahead"], status["behind"]);
        Ok((text, status))
    })
}

fn git_log(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let (dir, label) = worktree_dir(ws, caller, op)?;
    let limit = op.number("limit").unwrap_or(20) as usize;
    read_off_main("read".into(), move || {
        let log = reads::git_log(&dir, limit).map_err(|e| format!("{label}: {e:#}"))?;
        let lines: Vec<String> = log.as_array().into_iter().flatten().map(|c| format!("{} {} ({})", c["hash"].as_str().unwrap_or_default().chars().take(9).collect::<String>(), c["subject"].as_str().unwrap_or_default(), c["author"].as_str().unwrap_or_default())).collect();
        Ok((reads::as_data(&format!("The last commits of {label}"), &lines.join("\n")), json!({ "commits": log })))
    })
}

fn git_diff(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let (dir, label) = worktree_dir(ws, caller, op)?;
    read_off_main("read".into(), move || {
        let diff = reads::git_diff(&dir);
        Ok((reads::as_data(&format!("The changes of {label}"), &diff), json!({ "diff": diff })))
    })
}

fn file_read(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let given = text(op, "path").trim().to_string();
    let path = ws.world(caller).expand(&given, caller.cwd.as_deref());
    // The managed projects' folders and their worktrees; a hidden project's are not.
    let mut roots: Vec<PathBuf> = ws.state.projects.iter().filter(|p| !ws.state.is_hidden(&p.id)).map(|p| p.root.clone()).collect();
    roots.extend(ws.worktrees.iter().filter(|(project, _)| !ws.state.is_hidden(project)).flat_map(|(_, list)| list.iter().map(|w| w.path.clone())));
    read_off_main("read".into(), move || {
        let text = reads::file_read(&path, &roots)?;
        Ok((reads::as_data(&format!("The file {}", path.display()), &text), json!({ "path": path, "text": text })))
    })
}

// ----- usage and settings ----------------------------------------------------------------------

fn usage_get(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let agent = op.text("agent").map(str::to_lowercase);
    if agent.as_deref().is_some_and(|a| !matches!(a, "claude" | "codex")) {
        return Err(usage("usage get [--agent claude|codex]"));
    }
    let report = ws.usage.report.as_ref();
    let accounts: Vec<Value> = ws
        .state
        .accounts
        .iter()
        .filter(|a| a.agent != AgentKind::Shell && agent.as_deref().is_none_or(|want| a.agent.name().eq_ignore_ascii_case(want)))
        .map(|a| {
            let found = report.and_then(|r| r.accounts.iter().find(|x| x.id == a.id));
            let limits: Vec<Value> = found.map(|f| f.limits.iter().map(|l| json!({ "window_minutes": l.window_minutes, "used_percent": l.used_percent, "resets_at": l.resets_at })).collect()).unwrap_or_default();
            json!({ "agent": a.agent.name().to_lowercase(), "account": a.label, "plan": a.plan.clone().or_else(|| found.and_then(|f| f.plan.clone())), "limits": limits })
        })
        .collect();
    let lines: Vec<String> = accounts
        .iter()
        .map(|a| {
            let limits: Vec<String> = a["limits"].as_array().into_iter().flatten().map(|l| format!("{:.0}% of {}h", l["used_percent"].as_f64().unwrap_or_default(), l["window_minutes"].as_u64().unwrap_or_default() / 60)).collect();
            format!("{} · {}: {}", a["agent"].as_str().unwrap_or_default(), a["account"].as_str().unwrap_or_default(), if limits.is_empty() { "not read yet".to_string() } else { limits.join(", ") })
        })
        .collect();
    let text = if lines.is_empty() { "No Claude or Codex account.".to_string() } else { lines.join("\n") };
    crate::ops_run::answer(Reply::ok(text, json!({ "accounts": accounts, "scanned_at": report.map(|r| r.scanned_at) })))
}

fn settings_get(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    match op.text("key") {
        Some(key) => {
            let setting = settings::find(key).ok_or_else(|| error(ErrorCode::NotFound, format!("No setting {key}; `settings get` lists them.")))?;
            let value = setting.get(&ws.state);
            crate::ops_run::answer(Reply::ok(format!("{}: {value} ({})", setting.key, setting.help), json!({ "key": setting.key, "value": value, "tier": setting.tier, "takes": setting.takes(), "section": setting.section })))
        }
        None => {
            let all = settings::all(&ws.state);
            let lines: Vec<String> = settings::SETTINGS.iter().map(|s| format!("{} = {}", s.key, all[s.key])).collect();
            crate::ops_run::answer(Reply::ok(lines.join("\n"), all))
        }
    }
}

/// A setting's value as the command line gives it: JSON when it reads as JSON, else the word.
fn value_of(text: &str) -> Value {
    serde_json::from_str(text.trim()).unwrap_or_else(|_| Value::String(text.trim().to_string()))
}

fn settings_set(_: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let key = text(op, "key").to_string();
    let setting = settings::find(&key).ok_or_else(|| error(ErrorCode::NotFound, format!("No setting {key}; `settings get` lists them.")))?;
    settings::may_set(setting, person(caller)).map_err(|why| error(ErrorCode::NeedsPerson, why))?;
    let value = value_of(text(op, "value"));
    setting.check(&value).map_err(usage)?;
    plan(Kind::Settings, format!("set {key} to {value}"), move |ws, r, _, cx| {
        let before = match setting.set(&mut ws.state, &value) {
            Ok(before) => before,
            Err(why) => return ws.reply(r, usage(why)),
        };
        if setting.key.starts_with("appearance.") {
            ws.set_appearance(|_| {}, cx);
        } else {
            ws.save();
        }
        cx.notify();
        let reply = Reply::Done { code: code::OK, text: format!("{key} is {value} now (it was {before})."), data: json!({ "key": key, "value": value, "before": before, "undo": { "op": "settings.set", "args": { "key": key, "value": before.to_string() } } }) };
        ws.reply(r, reply);
    })
}

// ----- the work ledger (M32) ------------------------------------------------------------------

/// The work this caller may see: its scope's projects (`journal::Scope`: a session its own, the
/// manager the ones it manages), never a hidden project's.
fn work_seen(ws: &Workspace, caller: &Caller) -> impl Fn(&glasscode_core::work::WorkItem) -> bool + use<> {
    let scope = crate::journal_ui::scope_of(ws, caller);
    let hidden: Vec<glasscode_core::paths::ProjectKey> = ws.state.hidden_projects.iter().filter_map(|id| ws.control.project_keys.get(id).cloned()).collect();
    move |item| {
        !hidden.contains(&item.key.project)
            && match &scope {
                glasscode_core::journal::Scope::All => true,
                glasscode_core::journal::Scope::Projects(keys) => keys.contains(&item.key.project),
            }
    }
}

fn work_ledger(ws: &Workspace) -> Result<&glasscode_core::work::WorkLedger, Reply> {
    ws.work_ledger().ok_or_else(|| failed("The work ledger is not running here: it runs with the v2 engine, once its file could be read."))
}

/// The piece of work an op names (`work`): "this" is the caller's session's; a session's id is
/// that session's (the person may give its start); anything else is the work's own id.
fn work_named(ws: &Workspace, caller: &Caller, op: &Op) -> Result<glasscode_core::work::WorkItem, Reply> {
    let ledger = work_ledger(ws)?;
    let seen = work_seen(ws, caller);
    let word = text(op, "work").trim();
    let session = match word {
        "this" | "" => Some(caller.session.clone().ok_or_else(|| usage("Say which work: work show <work> (this command is not run in a session)."))?),
        _ => ws.state.sessions.iter().find(|s| s.id == word || (person(caller) && word.len() >= 4 && s.id.starts_with(word))).map(|s| s.id.clone()),
    };
    if let Some(session) = session {
        return ledger.of_session_any(&session).filter(|item| seen(item)).cloned().ok_or_else(|| error(ErrorCode::NotFound, "That session has no work in the ledger yet (it is made within a few seconds of its start)."));
    }
    let found: Vec<&glasscode_core::work::WorkItem> = ledger.named(word).into_iter().filter(|item| seen(item)).collect();
    match found.as_slice() {
        [] => Err(error(ErrorCode::NotFound, format!("No work “{word}”. `work list --all` lists it."))),
        [one] => Ok((*one).clone()),
        several => {
            let candidates = several.iter().take(6).map(|item| glasscode_core::ops::Candidate { id: item.label(true), title: item.state.words(), project: Some(item.key.project.to_string()) }).collect();
            Err(error_reply(&OpError::new(ErrorCode::Ambiguous { candidates }, format!("“{word}” is work in {} projects: name it as <project>:<id>.", several.len()))))
        }
    }
}

/// One item as a reply's data: the item, its name to give back, its state in words, and each
/// condition with whether it holds.
fn work_data(item: &glasscode_core::work::WorkItem) -> Value {
    let conditions: Vec<Value> = item.done.iter().map(|c| json!({ "condition": c, "words": c.words(), "met": item.met(c) })).collect();
    json!({ "id": item.label(true), "item": item, "state_words": item.state.words(), "conditions": conditions, "done_by_facts": item.verified_done() })
}

fn work_list(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let ledger = work_ledger(ws)?;
    let seen = work_seen(ws, caller);
    let state = op.text("state").map(|s| s.trim().to_lowercase());
    if let Some(state) = &state
        && !["open", "waiting", "stuck", "done", "dropped"].contains(&state.as_str())
    {
        return Err(usage(format!("“{state}” is no state of work: open, waiting, stuck, done or dropped.")));
    }
    let project = match op.text("project") {
        Some(word) => {
            let found = ws.state.visible_projects().find(|p| p.id == word || p.name.eq_ignore_ascii_case(word)).ok_or_else(|| usage(format!("No project “{word}”. `glasscode project list` lists them.")))?;
            Some(ws.control.project_keys.get(&found.id).cloned().ok_or_else(|| failed(format!("{} has no project key yet, so its work cannot be told apart.", found.name)))?)
        }
        None => None,
    };
    let all = op.on("all") || matches!(state.as_deref(), Some("done" | "dropped"));
    let items: Vec<&glasscode_core::work::WorkItem> = ledger
        .items
        .iter()
        .filter(|item| seen(item) && (all || !item.state.is_ended()))
        .filter(|item| project.as_ref().is_none_or(|p| item.key.project == *p))
        .filter(|item| state.as_deref().is_none_or(|s| item.state.name() == s))
        .collect();
    // Two projects with the same id are told apart by their project.
    let shared = |item: &glasscode_core::work::WorkItem| items.iter().filter(|other| other.key.id == item.key.id).count() > 1;
    let text = if items.is_empty() {
        if all { "No work in the ledger.".to_string() } else { "No work in hand (`work list --all` lists what is done or dropped too).".to_string() }
    } else {
        items.iter().map(|item| item.line(shared(item))).collect::<Vec<_>>().join("\n")
    };
    let data: Vec<Value> = items.iter().map(|item| work_data(item)).collect();
    crate::ops_run::answer(Reply::ok(text, Value::Array(data)))
}

fn work_show(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let item = work_named(ws, caller, op)?;
    let mut lines = vec![item.line(true)];
    if let Some((repo, number)) = &item.pr {
        lines.push(format!("Pull request: {repo}#{number}."));
    }
    if let Some(ticket) = &item.ticket {
        lines.push(format!("Ticket: {ticket}."));
    }
    if !item.sessions.is_empty() {
        lines.push(format!("Sessions: {}.", item.sessions.join(", ")));
    }
    if let Some(claimed) = &item.facts.claimed {
        lines.push(format!("The session says: {claimed} (a claim, not evidence)."));
    }
    crate::ops_run::answer(Reply::ok(lines.join("\n"), work_data(&item)))
}

/// What `work done`, `work drop` or `work reopen` names, and its words for a card; refused before
/// anyone is asked when there is nothing to change. The ops are `human_only`: the gate refuses
/// them to anyone but the person.
fn work_to_mark(ws: &Workspace, caller: &Caller, op: &Op, mark: glasscode_core::work::Mark) -> Result<(glasscode_core::identity::WorkKey, String, String), Reply> {
    use glasscode_core::work::{Mark, WorkState};
    let item = work_named(ws, caller, op)?;
    let what = match mark {
        Mark::Done => format!("say {} is done", item.key.id),
        Mark::Dropped => format!("drop {}", item.key.id),
        Mark::Open => format!("reopen {}", item.key.id),
    };
    match (mark, &item.state) {
        (Mark::Done, WorkState::Done) => Err(failed(format!("{} is done already.", item.key.id))),
        (Mark::Dropped, WorkState::Dropped) => Err(failed(format!("{} was dropped already.", item.key.id))),
        (Mark::Open, state) if !state.is_ended() => Err(failed(format!("{} is not done or dropped: it is {}.", item.key.id, state.words()))),
        // Work whose sessions are all closed is dropped again at the next tick (M32).
        (Mark::Open, _) if !item.sessions.iter().any(|id| ws.state.session(id).is_some()) => Err(failed(format!("{}'s sessions are closed, so it would be dropped again: start a session for it instead.", item.key.id))),
        _ => Ok((item.key.clone(), item.label(true), what)),
    }
}

/// The mark made, and its reply: the item, what it was, and for a reopen what takes it back.
fn work_marked(ws: &mut Workspace, r: crate::control_ui::Responder, key: &glasscode_core::identity::WorkKey, mark: glasscode_core::work::Mark, name: String, cx: &mut Context<Workspace>) {
    use glasscode_core::work::{Mark, WorkState};
    let reply = match ws.work_mark(key, mark) {
        Ok((before, item)) => {
            let mut data = work_data(&item);
            data["before"] = json!(before);
            // Reopened: back to what it was, done or dropped.
            if mark == Mark::Open {
                let again = if before == WorkState::Dropped { "work.drop" } else { "work.done" };
                data["undo"] = json!({ "op": again, "args": { "work": name } });
            }
            Reply::ok(format!("{}: {} now (it was {}).", item.key.id, item.state.words(), before.words()), data)
        }
        Err(why) => failed(format!("Nothing changed: {why}.")),
    };
    cx.notify();
    ws.reply(r, reply);
}

fn work_done(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let mark = glasscode_core::work::Mark::Done;
    let (key, name, what) = work_to_mark(ws, caller, op, mark)?;
    plan(Kind::Board, what, move |ws, r, _, cx| work_marked(ws, r, &key, mark, name, cx))
}

fn work_drop(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let mark = glasscode_core::work::Mark::Dropped;
    let (key, name, what) = work_to_mark(ws, caller, op, mark)?;
    plan(Kind::Board, what, move |ws, r, _, cx| work_marked(ws, r, &key, mark, name, cx))
}

fn work_reopen(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let mark = glasscode_core::work::Mark::Open;
    let (key, name, what) = work_to_mark(ws, caller, op, mark)?;
    plan(Kind::Board, what, move |ws, r, _, cx| work_marked(ws, r, &key, mark, name, cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: &str = "\
╭──────────────────────────────────────╮
│ Bash command                         │
│   rm -rf ./secret-folder             │
│ Do you want to proceed?              │
│ ❯ 1. Yes                             │
│   2. No, and tell Claude (esc)       │
╰──────────────────────────────────────╯
";

    /// `ui.state` names what is on screen by ids and counts: the prompt's text, the person's
    /// drafts and Jump's query are never in it.
    #[test]
    fn ui_state_has_no_screen_text() {
        let prompt = prompt::parse(SCREEN).expect("a prompt");
        let seen = Seen {
            view: "sessions".into(),
            session: Some(("s1".into(), "API".into())),
            jump: Some(12),
            commit_draft: 30,
            prompt: Some(("s1".into(), prompt.clone())),
            window_active: true,
            ..Seen::default()
        };
        let state = state_of(&seen).to_string();
        for text in ["rm -rf", "secret-folder", "Do you want", "Yes"] {
            assert!(!state.contains(text), "{text} is in {state}");
        }
        let value = state_of(&seen);
        assert_eq!(value["prompt"]["prompt_id"], json!(prompt::prompt_id("s1", &prompt)));
        assert_eq!((value["jump"]["typed"].clone(), value["commit_draft_length"].clone(), value["v"].clone()), (json!(12), json!(30), json!(1)));
    }

    /// A `ui.open` from a caller that is not the person reveals: neither its own arms nor what they
    /// call move the keyboard, clear a session's "needs you" mark or dismiss its notices (ops-13).
    #[test]
    fn ui_open_does_not_steal_focus_for_manager() {
        let files = [include_str!("ops_v2.rs"), include_str!("today_ui.rs"), include_str!("tickets_ui.rs"), include_str!("prs_view.rs"), include_str!("manager_ui.rs"), include_str!("settings_ui.rs")];
        let body = |name: &str| -> String {
            let file = files.iter().find(|f| f.contains(&format!("fn {name}("))).unwrap_or_else(|| panic!("no fn {name}"));
            let at = file.find(&format!("fn {name}(")).unwrap();
            let rest = &file[at..];
            // A function at the top ends at a `}` of its own line; a method at one indented once.
            let line_start = file[..at].rfind('\n').map_or(0, |i| i + 1);
            let end = if file[line_start..at].starts_with(' ') { "\n    }\n" } else { "\n}\n" };
            rest[..rest.find(end).unwrap()].to_string()
        };
        let open = body("ui_open");
        // The arms for a caller that is not the person: from the deferral to the reply.
        let reveal = &open[open.find("match target {").unwrap()..];
        assert!(!reveal.replace("if go => ws.open_session", "").replace("if go => ws.go(", "").contains("open_session(") && !reveal.contains("focus"), "{reveal}");
        for called in ["reveal_session", "show_today", "show_tickets", "show_prs", "show_view", "show_manager", "open_settings"] {
            let text = body(called);
            for taker in ["focus(", "focus_active", "attention.remove", "dismiss_popups_for"] {
                assert!(!text.contains(taker), "{called} calls {taker}");
            }
        }
    }

    #[test]
    fn a_caller_that_is_not_the_person_presses_few_keys() {
        for key in ["esc", "Enter", "up", "down", "left", "right", "tab"] {
            assert_eq!(key_of(key).map(|(_, persons)| persons), Some(false), "{key}");
        }
        for key in ["ctrl-c", "shift-tab"] {
            assert_eq!(key_of(key).map(|(_, persons)| persons), Some(true), "{key}");
        }
        assert!(key_of("rm -rf /").is_none() && key_of("y").is_none());
    }

    #[test]
    fn a_mode_is_ranked_by_what_it_lets_the_agent_do() {
        assert!(mode_rank("plan") < mode_rank("Default") && mode_rank("default") < mode_rank("Accept edits") && mode_rank("accept-edits") < mode_rank("Auto mode") && mode_rank("Auto mode") < mode_rank("bypass"));
        assert_eq!(mode_rank("Plan mode"), Some(0));
        assert_eq!(mode_rank("yolo"), None);
    }

    #[test]
    fn a_value_reads_as_json_or_as_its_word() {
        assert_eq!(value_of("true"), json!(true));
        assert_eq!(value_of(" 3 "), json!(3));
        assert_eq!(value_of("dark"), json!("dark"));
        assert_eq!(value_of("[\"acme/app\"]"), json!(["acme/app"]));
    }

    /// Each handler here plans its op's v1 kind (Settings › Permissions asks about it as about any
    /// command of that kind), and each op of this table is one of the v2 engine's.
    #[test]
    fn handlers_are_the_v2_engines_and_plan_their_kind() {
        let source = include_str!("ops_v2.rs");
        let source = &source[..source.find("#[cfg(test)]\nmod tests").unwrap()];
        for (name, _) in HANDLERS {
            let spec = glasscode_core::ops::find(name).unwrap_or_else(|| panic!("{name} is no op"));
            assert!(spec.v2_only, "{name}");
            let function = source[source.find(&format!("(\"{name}\", ")).unwrap()..].split_once(", ").unwrap().1.split(')').next().unwrap().to_string();
            if function.starts_with("crate::") {
                continue;
            }
            let body = &source[source.find(&format!("\nfn {function}(")).unwrap_or_else(|| panic!("no fn {function}"))..];
            let body = &body[..body[1..].find("\nfn ").map_or(body.len(), |i| i + 1)];
            let kinds: Vec<&str> = body.match_indices("plan(Kind::").map(|(i, _)| body[i + 11..].split(|c: char| !c.is_alphanumeric()).next().unwrap()).collect();
            let delegated = body.contains("pr_read(") || body.contains("read_off_main(") || body.contains("answer(");
            assert!(!kinds.is_empty() || delegated, "{name} plans nothing");
            for kind in kinds {
                assert_eq!(kind, format!("{:?}", spec.kind_v1), "{name}");
            }
        }
    }
}
