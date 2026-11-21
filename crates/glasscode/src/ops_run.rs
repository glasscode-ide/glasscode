//! The `glasscode` command's handlers, keyed by op name (M12.2). A command's words are parsed by
//! the registry first (`glasscode_core::ops::parse`), and the op's handler builds today's `Plan`:
//! the kind Settings › Permissions knows it by, what it does in words, and what it then does.
//! Each handler is the branch of the hand-parsed command it replaces, reading its words from the
//! `Op` instead of the command line.
//!
//! The words an op names (a project, a worktree, a task) are looked up in `glasscode_core::ops`'s
//! `World`, which `targets` reads too, so the handler and the gate agree on what they name.

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, Window};
use glasscode_core::board::{self, Draft, List, Section};
use glasscode_core::control::{Kind, Reply, code};
use glasscode_core::day::{self, Day};
use glasscode_core::git;
use glasscode_core::github;
use glasscode_core::linear;
use glasscode_core::manager::{Action, FollowState, FollowUp, MsgState, OutMessage, PanelTab, Show, Tier, brief, followups, scene, voice};
use glasscode_core::model::{AgentKind, new_id, now};
use glasscode_core::ops::{self, ErrorCode, Op, OpError, ParseCtx, SessionView, World};
use glasscode_core::sys;
use serde_json::{Value, json};

use crate::commands::{MoveTo, agent_of, clock, home_relative, plural, pr_status_reply, short};
use crate::commands2::{clone_folder_name, clone_source_checked, days, git_clone};
use crate::control_ui::{Caller, Plan, Responder};
use crate::workspace::Workspace;

pub(crate) type Planned = Result<Plan, Reply>;

/// A handler: the op's plan, or why it cannot run.
pub(crate) type Handler = fn(&mut Workspace, &Caller, &Op) -> Planned;

/// Every op's handler. A test checks that each op of the registry has one (or is `v2_only`), and
/// that each `plan(Kind::…)` here is its op's `kind_v1`.
pub(crate) const HANDLERS: &[(&str, Handler)] = &[
    ("board.list", board_list),
    ("board.show", board_show),
    ("board.add", board_add),
    ("board.done", board_done),
    ("board.undo", board_undo),
    ("board.snooze", board_snooze),
    ("board.unsnooze", board_unsnooze),
    ("board.pin", board_pin),
    ("board.unpin", board_unpin),
    ("board.move", board_move),
    ("board.note", board_note),
    ("board.edit", board_edit),
    ("board.remove", board_remove),
    ("board.recurring", board_recurring),
    ("words.recent", words_recent),
    ("memory.search", memory_search),
    ("memory.show", memory_show),
    ("memory.brief", memory_brief),
    ("memory.note", memory_note),
    ("memory.propose", memory_propose),
    ("memory.accept", memory_accept),
    ("memory.reject", memory_reject),
    ("memory.merge", memory_merge),
    ("memory.let_go", memory_let_go),
    ("memory.rewrite", memory_rewrite),
    ("memory.forget", memory_forget),
    ("memory.pending", memory_pending),
    ("data.export", data_export),
    ("data.import", data_import),
    ("data.schedule", data_schedule),
    ("data.move_out", data_move_out),
    ("data.forget", data_forget),
    ("data.uninstall", data_uninstall),
    ("session.list", session_list),
    ("session.show", session_show),
    ("session.read", session_read),
    ("session.new", session_new),
    ("session.open", session_open),
    ("session.rename", session_rename),
    ("session.close", session_close),
    ("session.restart", session_restart),
    ("session.send", session_send),
    ("session.hand_off", session_hand_off),
    ("session.move", session_move),
    ("pr.review", pr_review),
    ("pr.build", pr_build),
    ("pr.status", pr_status),
    ("pr.list", pr_list),
    ("pr.rerun", pr_rerun),
    ("pr.comment", pr_comment),
    ("pr.label", pr_label),
    ("pr.open_draft", pr_open_draft),
    ("project.list", project_list),
    ("project.import", project_import),
    ("project.remove", project_remove),
    ("project.set_account", project_set_account),
    ("worktree.list", worktree_list),
    ("worktree.new", worktree_new),
    ("worktree.rename", worktree_rename),
    ("worktree.remove", worktree_remove),
    ("worktree.clean", worktree_clean),
    ("worktree.open", worktree_open),
    ("account.list", account_list),
    ("ticket.list", ticket_list),
    ("ticket.show", ticket_show),
    ("ticket.start", ticket_start),
    ("ticket.move", ticket_move),
    ("ticket.comment", ticket_comment),
    ("app.status", app_status),
    ("app.whoami", app_whoami),
    ("app.open", app_open),
    ("app.notify", app_notify),
    ("app.log", app_log),
    ("journal.events", journal_events),
    ("journal.query", journal_query),
    ("playbook.check", playbook_check),
    ("playbook.test", playbook_test),
    ("app.approvals", app_approvals),
    ("app.wait", app_wait),
    ("approvals.list", approvals_list),
    ("approvals.decide", approvals_decide),
    ("browser.read", browser_read),
    ("browser.act", browser_gate),
    ("browser.eval", browser_gate),
    ("manager.status", manager_status),
    ("manager.ask", manager_ask),
    ("manager.say", manager_say),
    ("manager.brief", manager_brief),
    ("manager.tour", manager_tour),
    ("manager.remember", manager_remember),
    ("manager.forget", manager_forget),
    ("manager.memory", manager_memory),
    ("manager.show", manager_show),
    ("manager.scene", manager_scene),
    ("manager.voice_token", manager_voice_token),
    ("manager.follow_up.list", follow_up_list),
    ("manager.follow_up.add", follow_up_add),
    ("manager.follow_up.done", follow_up_done),
    ("manager.follow_up.drop", follow_up_drop),
    ("manager.message.list", message_list),
    ("manager.message.draft", message_draft),
    ("manager.message.approve", message_approve),
    ("manager.message.reject", message_reject),
    // v1's checks are schedules (M44): their words parse to `schedule.*` (`V2_WORDS`), and a typed
    // call by v1's name runs the schedule's handler (the same arguments).
    ("manager.check.list", check_list),
    ("manager.check.add", check_add),
    ("manager.check.remove", check_remove),
];


pub(crate) fn usage(text: impl Into<String>) -> Reply {
    Reply::fail(code::USAGE, text)
}

pub(crate) fn failed(text: impl Into<String>) -> Reply {
    Reply::fail(code::FAILED, text)
}

pub(crate) fn denied(text: impl Into<String>) -> Reply {
    Reply::fail(code::DENIED, text)
}

pub(crate) fn plan(kind: Kind, what: impl Into<String>, run: impl FnOnce(&mut Workspace, Responder, &mut Window, &mut Context<Workspace>) + 'static) -> Planned {
    Ok(Plan { kind, what: what.into(), run: Box::new(run), check: None })
}

/// `data` with what it was before (`before`), for the undo (`ops::undo_of`).
fn with_before(mut data: Value, before: impl Into<Value>) -> Value {
    if data.is_object() {
        data["before"] = before.into();
    }
    data
}

/// `data` saying it made nothing new (an undo would take back what was there before).
fn not_created(mut data: Value) -> Value {
    if data.is_object() {
        data["created"] = Value::Bool(false);
    }
    data
}

/// `planned` with a check off the main thread before anyone is asked (`Plan::check`): what a
/// handler used to ask git, gh or a file while it planned (ops-9).
pub(crate) fn checked(planned: Planned, check: impl FnOnce() -> Result<(), Reply> + Send + 'static) -> Planned {
    planned.map(|mut plan| {
        plan.check = Some(Box::new(check));
        plan
    })
}

/// A read whose answer needs git, gh or a file: made off the main thread.
pub(crate) fn read_later(read: impl FnOnce() -> Reply + Send + 'static) -> Planned {
    plan(Kind::Read, "read", move |_, r, _, cx| {
        let task = cx.background_spawn(async move { read() });
        cx.spawn(async move |this, cx| {
            let reply = task.await;
            let _ = this.update(cx, |ws, _| ws.reply(r, reply));
        })
        .detach();
    })
}

/// A command that only reads: its answer right away.
pub(crate) fn answer(reply: Reply) -> Planned {
    plan(Kind::Read, "read", move |ws, r, _, _| ws.reply(r, reply))
}

/// The class of an error, as `data.error_code` says it.
fn error_code(code: &ErrorCode) -> String {
    serde_json::to_value(code).ok().and_then(|v| v["code"].as_str().map(str::to_string)).unwrap_or_else(|| "failed".into())
}

/// An error of the registry as a reply: its words and exit code, and in `data` its class (and the
/// candidates of an ambiguous word). The `control::Reply` shape stays (no protocol change).
pub(crate) fn error_reply(error: &OpError) -> Reply {
    let mut data = json!({ "error_code": error_code(&error.code) });
    if let ErrorCode::Ambiguous { candidates } = &error.code {
        data["candidates"] = json!(candidates);
    }
    Reply::Done { code: error.exit_code(), text: error.text.clone(), data }
}

/// A reply with the op that made it in `data` (`op`, and `error_code` for a failure). The command
/// prints `data` only for `--json` on success, so failures always carry them; a success's data
/// gains `op` where it is an object.
pub(crate) fn with_op(reply: Reply, op: Option<&str>) -> Reply {
    let Reply::Done { code, text, mut data } = reply else { return reply };
    if code != code::OK {
        if !data.is_object() {
            data = json!({});
        }
        data["op"] = json!(op);
        if data.get("error_code").is_none() {
            let class = match code {
                code::USAGE => "usage",
                code::NOT_RUNNING => "unavailable",
                code::DENIED => "denied",
                _ => "failed",
            };
            data["error_code"] = json!(class);
        }
    } else if let (Some(op), Value::Object(map)) = (op, &mut data) {
        map.insert("op".into(), json!(op));
    }
    Reply::Done { code, text, data }
}

/// A word or text of the op, as given ("" when it is left out: `parse` made sure a required one is
/// there).
fn arg<'a>(op: &'a Op, name: &str) -> &'a str {
    op.text(name).unwrap_or_default()
}

/// What an op does, in the words of an approval and of the log ("add “X” to Today"), from the op
/// and the names its handler looked up. It reads no state, so the golden harness checks it against
/// what v1 said.
#[derive(Clone, Debug, Default)]
pub(crate) struct Names {
    /// The title of the task, session or check it is about.
    pub title: String,
    /// Another name it needs: the manager's, a project's, an account's, a branch, a folder.
    pub other: String,
}

pub(crate) fn what(op: &Op, n: &Names, today: Day) -> String {
    let t = &n.title;
    match op.name() {
        "board.add" => format!("add “{t}” to Today"),
        "board.done" => format!("tick off “{t}”"),
        "board.undo" => format!("put “{t}” back on the board"),
        "board.snooze" => {
            let until = op.day("until").flatten().unwrap_or(today + 1);
            let label = day::relative(until, today);
            format!("snooze “{t}” until {}", if label == "Tomorrow" { "tomorrow".to_string() } else { label })
        }
        "board.unsnooze" => format!("bring “{t}” back to today"),
        "board.pin" => format!("put “{t}” in Focus"),
        "board.unpin" => format!("take “{t}” out of Focus"),
        "board.move" => format!("move “{t}” to {}", Section::parse(arg(op, "to")).map(|s| s.title()).unwrap_or_default()),
        "board.note" => format!("add a note to “{t}”"),
        "board.edit" => format!("change “{t}”"),
        "board.remove" => format!("take “{t}” off the board"),
        "session.open" => format!("show the session “{t}” in the window"),
        // A screen, or a session.
        "app.open" if !t.is_empty() => format!("show the session “{t}” in the window"),
        "app.open" => format!("show {} in the window", arg(op, "place").to_lowercase()),
        "session.rename" => format!("rename the session “{t}” to “{}”", arg(op, "name").trim()),
        "session.close" => format!("close the session “{t}”"),
        "session.restart" => format!("restart the session “{t}”"),
        "session.send" => format!("send “{}” to the session “{t}”", arg(op, "message").trim().chars().take(80).collect::<String>()),
        "session.hand_off" => format!("hand the session “{t}” to {}", n.other),
        "session.move" => format!("move the session “{t}” to {}", n.other),
        "pr.rerun" => format!("re-run the failed checks of #{}", op.number("number").unwrap_or_default()),
        "pr.comment" => format!("comment on #{}: “{}”", op.number("number").unwrap_or_default(), arg(op, "text").trim().chars().take(80).collect::<String>()),
        "pr.label" => match (op.text("add"), op.text("remove")) {
            (Some(label), _) => format!("add the label {label} to #{}", op.number("number").unwrap_or_default()),
            (_, label) => format!("take the label {} off #{}", label.unwrap_or_default(), op.number("number").unwrap_or_default()),
        },
        "pr.open_draft" => format!("push {} and open a draft pull request for “{t}”", n.other),
        "app.notify" => format!("send you a notification: “{}”", arg(op, "message")),
        "ticket.move" => format!("move {} to “{}” in Linear", arg(op, "ticket"), arg(op, "to").trim()),
        "ticket.comment" => format!("comment on {} in Linear: “{}”", arg(op, "ticket"), arg(op, "text").trim().chars().take(80).collect::<String>()),
        "manager.say" | "ui.say" => format!("have {} say “{}”", n.other, clip(arg(op, "text").trim(), 80)),
        "manager.tour" | "ui.tour" => "walk you through today's update".to_string(),
        "manager.remember" => format!("have {} remember “{}”", n.other, clip(arg(op, "fact").trim(), 80)),
        "manager.forget" => format!("have {} forget “{}”", n.other, clip(arg(op, "which").trim(), 80)),
        "manager.follow_up.add" => format!("have {} keep an eye on “{}”", n.other, clip(arg(op, "what").trim(), 80)),
        "manager.follow_up.done" => format!("close the follow-up “{}”", clip(arg(op, "which").trim(), 80)),
        "manager.follow_up.drop" => format!("drop the follow-up “{}”", clip(arg(op, "which").trim(), 80)),
        "manager.check.remove" => format!("stop {} checking “{}”", n.other, clip(t, 80)),
        _ => t.clone(),
    }
}

impl Workspace {

    /// The op the words name, or the reply that says why not. Parsing runs nothing.
    pub fn control_parse(&self, caller: &Caller, words: &[String]) -> Result<Op, Reply> {
        let ctx = self.parse_ctx(caller);
        ops::parse(words, &ctx).map_err(|e| with_op(error_reply(&e), ops::name_of(words, ctx.v2)))
    }

    /// The plan of an op its caller has already named (the command's words, a key, a button, a
    /// typed tool): its handler's, or the reply that says why not. The one place a handler is
    /// called: with the v2 engine the gate has decided first (`control_gated`), and a refused op
    /// never reaches its handler.
    /// With the caller's own op id for the call (`op_id`, ops-10), the effect ledger claims it under
    /// that id, so a call asked again after a restart does not run twice either.
    pub(crate) fn op_plan(&mut self, caller: &Caller, op: &Op, op_id: Option<&str>) -> Result<Plan, Reply> {
        let Some((_, handler)) = HANDLERS.iter().chain(crate::ops_v2::HANDLERS).chain(crate::movein_ops::HANDLERS).find(|(name, _)| *name == op.name()) else {
            let text = format!("“{}” is in the registry, and runs once its handler lands (the server-sync plan).", op.spec.cli.join(" "));
            return Err(with_op(error_reply(&OpError::new(ErrorCode::Failed, text)), Some(op.name())));
        };
        let plan = handler(self, caller, op).map_err(|reply| with_op(reply, Some(op.name())))?;
        // v2 (M31): what a caller other than the person sets off goes through the effect ledger.
        let effect = !matches!(op.spec.effect, glasscode_core::ops::EffectClass::Read | glasscode_core::ops::EffectClass::Navigate);
        Ok(if effect && !self.caller_is_person(caller) && op.name() != "session.send" { claimed(self, caller, op, op_id, plan) } else { plan })
    }

    /// Whether the caller may name things by the start of a name or words from a title (ops-17):
    /// with the v1 engine every caller may, as before; with v2 only the person.
    pub(crate) fn caller_is_person(&self, caller: &Caller) -> bool {
        caller.is_person()
    }

    /// What the caller's words can mean: the day, who calls, the sessions a word may name.
    pub(crate) fn parse_ctx(&self, caller: &Caller) -> ParseCtx {
        let sessions = self
            .state
            .sessions
            .iter()
            .filter(|s| !self.state.session_hidden(s))
            .map(|s| SessionView {
                id: s.id.clone(),
                title: self.session_title(s),
                branch: s.branch.clone(),
                review: s.review.as_ref().map(|r| r.number),
                project: self.state.project(&s.project_id).map(|p| p.name.clone()),
            })
            .collect();
        ParseCtx {
            caller_is_person: self.caller_is_person(caller),
            caller_session: caller.session.clone(),
            today: day::local((glasscode_core::clock::now_ms() / 1000) as i64),
            sessions,
            v2: true,
        }
    }

    /// The projects, worktrees and sessions as they are in memory, for the caller (no git, no
    /// files: the repositories are the background scan's).
    pub(crate) fn world(&self, caller: &Caller) -> World {
        World {
            person: self.caller_is_person(caller),
            caller_session: caller.session.clone(),
            caller_project: caller.project.clone(),
            cwd: caller.cwd.clone(),
            home: glasscode_core::paths::home(),
            projects: self
                .state
                .projects
                .iter()
                .map(|p| ops::ProjectView { id: p.id.clone(), name: p.name.clone(), root: p.root.clone(), hidden: self.state.is_hidden(&p.id), key: self.control.project_keys.get(&p.id).cloned(), repo: self.tickets_repo_of(&p.root) })
                .collect(),
            worktrees: self.worktrees.iter().flat_map(|(project, list)| list.iter().map(move |w| ops::WorktreeView { project: project.clone(), path: w.path.clone(), branch: w.branch.clone() })).collect(),
            sessions: self.state.sessions.iter().map(|s| ops::SessionPlace { id: s.id.clone(), project: s.project_id.clone(), worktree: s.worktree.clone() }).collect(),
        }
    }

    /// A task of the board the op names (ops-17: words from its title for the person only).
    fn board_task(&self, caller: &Caller, op: &Op) -> Result<String, Reply> {
        let outside = self.today_outside();
        ops::find_task(&self.today.board, &outside, arg(op, "task"), self.caller_is_person(caller)).map_err(|e| error_reply(&e))
    }

    fn board_title(&self, key: &str) -> String {
        self.today_view().items().find(|i| i.key == key).map(|i| i.title.clone()).unwrap_or_else(|| key.trim_start_matches("task:").to_string())
    }

    /// The manager's settings when it is on; "it is off" otherwise.
    fn manager_on(&self) -> Result<glasscode_core::manager::Settings, Reply> {
        let settings = self.state.manager.clone();
        if !settings.enabled {
            return Err(failed(OFF));
        }
        Ok(settings)
    }
}

// ----- the board -------------------------------------------------------------------------------

fn board_list(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let mut view = ws.today_view();
    let needs = ws.state.sessions.iter().filter(|s| ws.session_state(s) == "needs you").count();
    if let Some(section) = op.text("section") {
        let wanted = Section::parse(section).ok_or_else(|| usage(format!("No section “{section}”.")))?;
        view.sections.retain(|s| s.section == Some(wanted));
    }
    let text = board::render_text(&view, &view.summary(needs), op.on("all"));
    answer(Reply::ok(text, serde_json::to_value(&view).unwrap_or_default()))
}

fn board_show(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let key = ws.board_task(caller, op)?;
    let view = ws.today_view();
    let found = view.items().find(|i| i.key == key).cloned().ok_or_else(|| failed("That item is not on today's board."))?;
    let mut lines = vec![format!("{} {}", found.id, found.title)];
    if let Some(s) = found.section {
        lines.push(format!("Section: {}", s.title()));
    }
    for (label, value) in [("Due", found.due_label.clone()), ("Person", found.person.clone()), ("From", found.source.clone()), ("Link", found.url.clone()), ("Back", found.snoozed.clone()), ("", found.added_by.clone())] {
        if let Some(v) = value {
            lines.push(if label.is_empty() { v } else { format!("{label}: {v}") });
        }
    }
    for note in &found.notes {
        lines.push(format!("Note: {note}"));
    }
    answer(Reply::ok(lines.join("\n"), serde_json::to_value(&found).unwrap_or_default()))
}

fn board_add(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let today = day::today();
    let text = arg(op, "title").to_string();
    let people = ws.today_people();
    let structured = op.on("as_written");
    let mut draft = if structured { Draft { title: text.clone(), ..Default::default() } } else { board::parse_quick(&text, today, &people) };
    if structured {
        draft.links = board::parse_quick(&text, today, &people).links;
    }
    if let Some(due) = op.day("due") {
        draft.due = due;
    }
    if let Some(name) = op.text("asked_by") {
        draft.person = Some(name.to_string());
        draft.list = List::Asked;
    }
    if let Some(name) = op.text("waiting_on") {
        draft.person = Some(name.to_string());
        draft.list = List::Waiting;
    }
    if let Some(name) = op.text("with") {
        draft.person = Some(name.to_string());
    }
    if let Some(list) = op.text("list") {
        draft.list = List::parse(list).ok_or_else(|| usage(format!("--list is todo, asked, waiting or later (not {list}).")))?;
    }
    if let Some(every) = op.text("every") {
        draft.every = Some(board::Every::parse(every).ok_or_else(|| usage(format!("--every is day, weekday, monday…sunday, month-start or month-end (not {every}).")))?);
    }
    draft.via = op.text("via").map(str::to_string);
    draft.note = op.text("note").map(str::to_string);
    draft.urgent |= op.on("urgent");
    for link in op.texts("links") {
        if !draft.links.contains(&link) {
            draft.links.push(link);
        }
    }
    let origin = caller.origin();
    let what = what(op, &Names { title: draft.title.clone(), ..Names::default() }, today);
    plan(Kind::Board, what, move |ws, r, _, cx| {
        let recurring = draft.every;
        let title = draft.title.clone();
        let id = ws.today.board.add(draft, origin, today);
        ws.today_save(cx);
        let view = ws.today_view();
        let text = match recurring {
            Some(every) => format!("Added the recurring task “{title}” ({}, id {id}).", every.label().to_lowercase()),
            None => {
                let placed = view.items().find(|i| i.id == id);
                let section = placed.and_then(|i| i.section).map(|s| s.title()).unwrap_or("Today");
                let due = placed.and_then(|i| i.due_label.clone()).map(|d| format!(", due {}", if matches!(d.as_str(), "Today" | "Tomorrow" | "Yesterday") { d.to_lowercase() } else { d })).unwrap_or_default();
                format!("Added “{title}” to {section}{due} (id {id}).")
            }
        };
        let item = view.items().find(|i| i.id == id).cloned();
        ws.reply(r, Reply::ok(text, json!({ "id": id, "item": item })));
    })
}

fn board_done(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    board_done_or_undo(ws, caller, op, true)
}

fn board_undo(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    board_done_or_undo(ws, caller, op, false)
}

fn board_done_or_undo(ws: &mut Workspace, caller: &Caller, op: &Op, done: bool) -> Planned {
    let today = day::today();
    let key = ws.board_task(caller, op)?;
    let title = ws.board_title(&key);
    plan(Kind::Board, what(op, &Names { title: title.clone(), ..Names::default() }, today), move |ws, r, _, cx| {
        let result = ws.today.board.set_done(&key, done, today);
        ws.today_save(cx);
        ws.reply(r, match result {
            Ok(()) if done => Reply::ok(format!("Ticked off “{title}”."), json!({ "key": key })),
            Ok(()) => Reply::ok(format!("Put “{title}” back on the board."), json!({ "key": key })),
            Err(e) => failed(e),
        });
    })
}

fn board_snooze(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let today = day::today();
    let key = ws.board_task(caller, op)?;
    // `parse` read the day (tomorrow unless said) and made sure it is after today.
    let until = op.day("until").flatten().unwrap_or(today + 1);
    let title = ws.board_title(&key);
    let label = day::relative(until, today);
    let label = if label == "Tomorrow" { "tomorrow".to_string() } else { label };
    plan(Kind::Board, what(op, &Names { title: title.clone(), ..Names::default() }, today), move |ws, r, _, cx| {
        let result = ws.today.board.snooze(&key, Some(until));
        ws.today_save(cx);
        ws.reply(r, match result {
            Ok(()) => Reply::ok(format!("Snoozed “{title}”: back {label}."), json!({ "key": key, "until": day::iso(until) })),
            Err(e) => failed(e),
        });
    })
}

fn board_unsnooze(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let key = ws.board_task(caller, op)?;
    let title = ws.board_title(&key);
    plan(Kind::Board, what(op, &Names { title: title.clone(), ..Names::default() }, day::today()), move |ws, r, _, cx| {
        let result = ws.today.board.snooze(&key, None);
        ws.today_save(cx);
        ws.reply(r, match result {
            Ok(()) => Reply::ok(format!("“{title}” is back on today's board."), json!({ "key": key })),
            Err(e) => failed(e),
        });
    })
}

fn board_pin(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    board_focus(ws, caller, op, true)
}

fn board_unpin(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    board_focus(ws, caller, op, false)
}

fn board_focus(ws: &mut Workspace, caller: &Caller, op: &Op, on: bool) -> Planned {
    let key = ws.board_task(caller, op)?;
    let title = ws.board_title(&key);
    plan(Kind::Board, what(op, &Names { title: title.clone(), ..Names::default() }, day::today()), move |ws, r, _, cx| {
        let result = ws.today.board.set_focus(&key, on);
        ws.today_save(cx);
        ws.reply(r, match result {
            Ok(()) if on => Reply::ok(format!("“{title}” is in Focus."), json!({ "key": key })),
            Ok(()) => Reply::ok(format!("Took “{title}” out of Focus."), json!({ "key": key })),
            Err(e) => failed(e),
        });
    })
}

fn board_move(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let key = ws.board_task(caller, op)?;
    let to = arg(op, "to");
    let section = Section::parse(to).filter(|s| s.movable()).ok_or_else(|| usage(format!("Tasks move to asked, todo, waiting or later (not {to}).")))?;
    let title = ws.board_title(&key);
    plan(Kind::Board, what(op, &Names { title: title.clone(), ..Names::default() }, day::today()), move |ws, r, _, cx| {
        let result = ws.today.board.move_to(&key, section);
        ws.today_save(cx);
        ws.reply(r, match result {
            Ok(()) => Reply::ok(format!("Moved “{title}” to {}.", section.title()), json!({ "key": key, "section": section.key() })),
            Err(e) => failed(e),
        });
    })
}

fn board_note(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let key = ws.board_task(caller, op)?;
    let text = arg(op, "text").to_string();
    let title = ws.board_title(&key);
    let origin = caller.origin();
    plan(Kind::Board, what(op, &Names { title: title.clone(), ..Names::default() }, day::today()), move |ws, r, _, cx| {
        let result = ws.today.board.add_note(&key, &text, &origin);
        ws.today_save(cx);
        ws.reply(r, match result {
            Ok(()) => Reply::ok(format!("Added a note to “{title}”."), json!({ "key": key })),
            Err(e) => failed(e),
        });
    })
}

fn board_edit(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let key = ws.board_task(caller, op)?;
    let id = key.strip_prefix("task:").map(str::to_string).ok_or_else(|| failed("Only your own tasks can be edited; items from Linear or GitHub change there."))?;
    let due = op.day("due");
    let list = match op.text("list") {
        Some(l) => Some(List::parse(l).ok_or_else(|| usage(format!("--list is todo, asked, waiting or later (not {l}).")))?),
        None => None,
    };
    let text = |name: &str| op.text(name).map(str::to_string);
    let (title, asked, waiting, with, via) = (text("title"), text("asked_by"), text("waiting_on"), text("with"), text("via"));
    let links = op.texts("links");
    let urgent = if op.on("urgent") {
        Some(true)
    } else if op.on("not_urgent") {
        Some(false)
    } else {
        None
    };
    let name = ws.board_title(&key);
    plan(Kind::Board, what(op, &Names { title: name, ..Names::default() }, day::today()), move |ws, r, _, cx| {
        let Some(task) = ws.today.board.task_mut(&id) else {
            ws.reply(r, failed("That task is gone."));
            return;
        };
        if let Some(t) = title {
            task.title = t;
        }
        if let Some(d) = due {
            task.due = d.map(day::iso);
        }
        if let Some(p) = asked {
            task.person = Some(p);
            task.list = List::Asked;
        }
        if let Some(p) = waiting {
            task.person = Some(p);
            task.list = List::Waiting;
        }
        if let Some(p) = with {
            task.person = Some(p).filter(|p| !p.is_empty() && p != "none");
        }
        if let Some(l) = list {
            task.list = l;
        }
        if via.is_some() {
            task.via = via;
        }
        for link in links {
            if !task.links.contains(&link) {
                task.links.push(link);
            }
        }
        if let Some(u) = urgent {
            task.urgent = u;
        }
        task.updated_at = now();
        let title = task.title.clone();
        ws.today_save(cx);
        ws.reply(r, Reply::ok(format!("Changed “{title}”."), json!({ "key": key })));
    })
}

fn board_remove(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let key = ws.board_task(caller, op)?;
    let title = ws.board_title(&key);
    plan(Kind::Board, what(op, &Names { title, ..Names::default() }, day::today()), move |ws, r, _, cx| {
        let result = ws.today.board.remove(&key);
        ws.today_save(cx);
        ws.reply(r, match result {
            Ok(title) => Reply::ok(format!("Took “{title}” off the board."), json!({ "key": key })),
            Err(e) => failed(e),
        });
    })
}

fn board_recurring(ws: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    let list = &ws.today.board.recurring;
    let text = if list.is_empty() {
        "No recurring tasks. `glasscode board add \"Review Renovate PRs\" --every monday` adds one.".to_string()
    } else {
        list.iter().map(|r| format!("{}  {}  · {}", r.id, r.title, r.every.label())).collect::<Vec<_>>().join("\n")
    };
    answer(Reply::ok(text, serde_json::to_value(list).unwrap_or_default()))
}

// ----- sessions --------------------------------------------------------------------------------

fn session_list(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let project = match op.text("project") {
        Some(p) => Some(ws.find_project_as(caller, p)?),
        None => None,
    };
    let needs = op.on("needs_you");
    let mut rows = Vec::new();
    let mut data = Vec::new();
    for s in &ws.state.sessions {
        // Sessions of a hidden project are invisible to the CLI and Glasscode.
        if ws.state.session_hidden(s) {
            continue;
        }
        if project.as_ref().is_some_and(|p| *p != s.project_id) {
            continue;
        }
        let state = ws.session_state(s);
        if needs && state != "needs you" {
            continue;
        }
        let project_name = ws.state.project(&s.project_id).map(|p| p.name.clone()).unwrap_or_default();
        let pr = ws.prs.map.get(&s.id).and_then(|p| p.pr.as_ref()).map(|p| format!(" · PR #{}", p.number)).unwrap_or_default();
        let here = if caller.session.as_deref() == Some(s.id.as_str()) { "  (this session)" } else { "" };
        rows.push(format!("{}  {:<11} {:<6} {}  · {project_name}/{}{pr}{here}", short(&s.id), state, s.agent.name(), ws.session_title(s), s.branch));
        data.push(ws.session_json(s));
    }
    if op.on("server") {
        for r in ws.server.state.as_ref().map(|s| s.sessions.clone()).unwrap_or_default() {
            let state = match r.activity {
                Some(glasscode_core::agents::AgentActivity::NeedsInput) => "needs you",
                Some(glasscode_core::agents::AgentActivity::Working) => "working",
                _ if r.running => "idle",
                _ => "not running",
            };
            if needs && state != "needs you" {
                continue;
            }
            rows.push(format!("{}  {:<11} {:<6} {}  · on {}/{}", short(&r.session.id), state, r.session.agent.name(), r.session.title, ws.server.name(), r.session.branch));
            data.push(json!({ "id": r.session.id, "title": r.session.title, "agent": r.session.agent.name().to_lowercase(), "branch": r.session.branch, "state": state, "on_server": true, "last_reply": r.last_reply }));
        }
    }
    let text = if rows.is_empty() { "No sessions.".to_string() } else { rows.join("\n") };
    answer(Reply::ok(text, Value::Array(data)))
}

/// The session the op names (`parse` resolved it), as the window holds it.
fn session_of(ws: &Workspace, op: &Op) -> Result<glasscode_core::model::Session, Reply> {
    ws.state.session(arg(op, "session")).cloned().ok_or_else(|| failed("That session is gone."))
}

fn session_show(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let s = session_of(ws, op)?;
    let mut data = ws.session_json(&s);
    let mut lines = vec![
        format!("{} ({})", ws.session_title(&s), s.id),
        format!("{} · {} · {}", s.agent.name(), s.account_id.as_deref().and_then(|a| ws.state.account(a)).map(|a| a.label.clone()).unwrap_or_else(|| "no account".into()), ws.session_state(&s)),
        format!("Project {} · branch {} · {}", ws.state.project(&s.project_id).map(|p| p.name.clone()).unwrap_or_default(), s.branch, home_relative(&s.worktree)),
    ];
    if let Some(pr) = ws.prs.map.get(&s.id).and_then(|p| p.pr.as_ref()) {
        lines.push(format!("Pull request #{} {} {}", pr.number, pr.title, pr.url));
    }
    if let Some(t) = ws.state.ticket_of.get(&s.id) {
        lines.push(format!("Ticket {t}"));
    }
    // The agent's last reply is in its transcript, a file: read off the main thread.
    let (agent, transcript) = (s.agent, s.transcript_path.clone());
    read_later(move || {
        let last = transcript.as_deref().and_then(|p| glasscode_core::transcript::last_reply(agent, p));
        data["last_reply"] = json!(last);
        if let Some(last) = &last {
            lines.push(format!("Last reply:\n{last}"));
        }
        Reply::ok(lines.join("\n"), data)
    })
}

fn session_read(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let s = session_of(ws, op)?;
    let last = op.number("last").unwrap_or(6) as usize;
    let path = s.transcript_path.clone().ok_or_else(|| failed("That session has no conversation yet."))?;
    let title = ws.session_title(&s);
    plan(Kind::Read, "read", move |_ws, r, _, cx| {
        let agent = s.agent;
        let task = cx.background_spawn(async move { glasscode_core::transcript::read(agent, &path, None, 400) });
        cx.spawn(async move |this, cx| {
            let page = task.await;
            let _ = this.update(cx, |ws, _| {
                let reply = match page {
                    Ok(page) => {
                        let items: Vec<_> = page.items.into_iter().filter(|i| i.kind != glasscode_core::transcript::ItemKind::Tool).collect();
                        let items = &items[items.len().saturating_sub(last)..];
                        let text = items
                            .iter()
                            .map(|i| format!("{}: {}", if i.kind == glasscode_core::transcript::ItemKind::User { "Person" } else { agent.name() }, i.text.trim()))
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        Reply::ok(format!("“{title}”, the last {}:\n\n{text}", plural(items.len(), "message", "messages")), serde_json::to_value(items).unwrap_or_default())
                    }
                    Err(e) => failed(format!("Could not read its conversation: {e}")),
                };
                ws.reply(r, reply);
            });
        })
        .detach();
    })
}

fn session_new(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let spec = ws.new_spec(caller, op)?;
    let check = new_branch_free(ws, &spec);
    checked_if(ws.new_session_plan(spec), check)
}

/// The check of a new session's `--branch`: that git has no such branch yet (off the main thread).
fn new_branch_free(ws: &Workspace, spec: &crate::commands::NewSpec) -> Option<Box<dyn FnOnce() -> Result<(), Reply> + Send>> {
    let branch = spec.branch.clone()?;
    let root = ws.state.project(&spec.project_id).map(|p| p.root.clone())?;
    Some(Box::new(move || if git::branch_exists(&root, &branch) { Err(failed(format!("Branch {branch} exists already; use --worktree {branch} to work in it."))) } else { Ok(()) }))
}

/// [`checked`] when there is a check.
fn checked_if(planned: Planned, check: Option<Box<dyn FnOnce() -> Result<(), Reply> + Send>>) -> Planned {
    match check {
        Some(check) => checked(planned, check),
        None => planned,
    }
}

fn session_open(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let id = arg(op, "session").to_string();
    let title = ws.state.session(&id).map(|s| ws.session_title(s)).unwrap_or_default();
    plan(Kind::Show, what(op, &Names { title: title.clone(), ..Names::default() }, day::today()), move |ws, r, window, cx| {
        ws.open_session(&id, window, cx);
        ws.reply(r, Reply::ok(format!("Showing “{title}”."), json!({ "id": id })));
    })
}

fn session_rename(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let id = arg(op, "session").to_string();
    let name = arg(op, "name").trim().to_string();
    let old = ws.state.session(&id).map(|s| ws.session_title(s)).unwrap_or_default();
    plan(Kind::Rename, what(op, &Names { title: old.clone(), ..Names::default() }, day::today()), move |ws, r, _, cx| {
        if let Some(s) = ws.state.session_mut(&id) {
            s.title = name.clone();
            ws.state.custom_titles.insert(id.clone());
        }
        ws.save();
        cx.notify();
        let data = with_before(json!({ "id": id, "title": name }), old.clone());
        ws.reply(r, Reply::ok(format!("Renamed “{old}” to “{name}”."), data));
    })
}

fn session_close(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    session_close_or_restart(ws, op, false)
}

fn session_restart(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    session_close_or_restart(ws, op, true)
}

fn session_close_or_restart(ws: &mut Workspace, op: &Op, restart: bool) -> Planned {
    let id = arg(op, "session").to_string();
    let title = ws.state.session(&id).map(|s| ws.session_title(s)).unwrap_or_default();
    plan(Kind::Close, what(op, &Names { title: title.clone(), ..Names::default() }, day::today()), move |ws, r, window, cx| {
        if restart {
            // Answer first: restarting this very session ends the command's own process.
            ws.reply(r, Reply::ok(format!("Restarting “{title}”; it resumes its conversation."), json!({ "id": id })));
            ws.restart_session(&id, window, cx);
        } else {
            ws.reply(r, Reply::ok(format!("Closed “{title}”. Its worktree stays; a new session there can resume the conversation."), json!({ "id": id })));
            ws.close_session(&id, window, cx);
        }
    })
}

fn session_send(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let id = arg(op, "session").to_string();
    let message = arg(op, "message").trim().to_string();
    if caller.session.as_deref() == Some(id.as_str()) {
        return Err(usage("That is this session; say it in your reply instead."));
    }
    let title = ws.state.session(&id).map(|s| ws.session_title(s)).unwrap_or_default();
    let from = caller.describe();
    session_send_queued(ws, caller, op, id, title, from, message)
}

/// v2 (M31): `session send` is a queued delivery, from every caller: the session's queue types it
/// when the session is idle, with a marker, and its hook event with the marker is the receipt. One
/// an agent or the manager sends is claimed in the effect ledger first, with a subject of its own
/// (a command is never a replay of an earlier one: the ledger records it and dedupes nothing).
fn session_send_queued(ws: &mut Workspace, caller: &Caller, op: &Op, id: String, title: String, from: String, message: String) -> Planned {
    let agent = ws.state.session(&id).map(|s| s.agent);
    if agent == Some(AgentKind::Shell) {
        return Err(failed(format!("“{title}” is a shell, not an agent; there is nothing to send a message to.")));
    }
    let person = ws.caller_is_person(caller);
    let who = caller.describe();
    let origin = caller.session.as_ref().map(|s| format!("session:{s}")).unwrap_or_else(|| caller.who.clone());
    let (project, session, launch) = (caller_key(ws, caller), caller.session.clone(), ws.control.launch.clone());
    // What the target's events will descend from (events-13): a playbook's firing; a session's
    // own drive, passed on (a session the manager drives that asks another to go on is the same
    // chain); the manager itself, one step. The person's own sends drive nothing.
    let drive = if person {
        None
    } else {
        caller
            .drive
            .clone()
            .or_else(|| caller.session.as_deref().and_then(glasscode_core::journal::driven).map(|d| (d.depth, d.causes)))
            .or_else(|| matches!(caller.channel, glasscode_core::caller::Caller::Manager { .. }).then(|| (1, Vec::new())))
    };
    plan(Kind::Send, what(op, &Names { title: title.clone(), ..Names::default() }, day::today()), move |ws, r, window, cx| {
        if !ws.terminals.contains_key(&id) && ws.state.session(&id).is_some() {
            ws.ensure_terminal(&id, window, cx);
        }
        let (depth, causes) = drive.clone().unwrap_or_default();
        let delivery = glasscode_core::delivery_session::Delivery {
            note: Some(format!("(Sent by Glasscode because {from} asked it to.)")),
            depth,
            causes,
            ..glasscode_core::delivery_session::Delivery::new(&id, &message, glasscode_core::words::InjectKind::SessionSend, glasscode_core::clock::now_ms())
        };
        let intent = (!person).then(|| glasscode_core::effects::Intent {
            origin: origin.clone(),
            subject: delivery.id.clone(),
            target: id.clone(),
            caller: who.clone(),
            reason: clip(&message, 80),
            project,
            session,
            launch,
            ..glasscode_core::effects::Intent::new(glasscode_core::effects::effect_key(&origin, &delivery.id, "session.send", &id), "session.send", glasscode_core::ops::Retry::Verifiable)
        });
        let marker = delivery.marker();
        ws.queue_delivery(delivery, intent, cx);
        ws.reply(r, Reply::ok(format!("Queued for “{title}”: it is typed in when the session is idle, and nothing is typed over a question or your own text."), json!({ "id": id, "queued": true, "marker": marker })));
    })
}

/// The project key of a caller's project (its session's, or the one a server session names).
fn caller_key(ws: &Workspace, caller: &Caller) -> Option<glasscode_core::paths::ProjectKey> {
    let project = caller.session.as_deref().and_then(|id| ws.state.session(id)).map(|s| s.project_id.clone()).or_else(|| caller.project.clone())?;
    ws.control.project_keys.get(&project).cloned()
}

/// v2 (M31): an effect of a caller other than the person is claimed in the effect ledger before it
/// runs, off the main thread, and its reply finishes the claim. Its subject is the request's own,
/// so the ledger records every one (the Activity view's autonomous actions) and repeats none.
/// `session send` is not wrapped: its queue finishes its claim. The record keeps a hash of the
/// op's words, its description with secrets replaced, and a short receipt: it is kept a year and
/// exported. A board move keeps what its probe reads back after a restart.
fn claimed(ws: &Workspace, caller: &Caller, op: &Op, op_id: Option<&str>, plan: Plan) -> Plan {
    // Its check (`Plan::check`) stays: it runs before the claim, before anyone is asked.
    let Plan { kind, what, run, check } = plan;
    let target = glasscode_core::effects::short_hash(&op.args.to_string());
    // A call with the caller's own op id (ops-10) is claimed under it, whatever its arguments, so
    // that the same id asked again finds it (and other arguments are refused); else each request
    // is its own subject.
    let (origin, subject, key_target) = match op_id {
        Some(id) => (caller.channel.origin(), format!("op:{id}"), String::new()),
        None => {
            let mut request = [0u8; 12];
            glasscode_core::sys::random_bytes(&mut request);
            let origin = caller.session.as_ref().map(|s| format!("session:{s}")).unwrap_or_else(|| caller.who.clone());
            (origin, request.iter().map(|b| format!("{b:02x}")).collect(), target.clone())
        }
    };
    let by_op_id = op_id.is_some();
    let launch = ws.control.launch.clone();
    let evidence = match op.name() {
        "board.move" => match (ws.board_task(caller, op), Section::parse(arg(op, "to"))) {
            (Ok(key), Some(section)) => json!({ "key": key, "section": section.key() }),
            _ => Value::Null,
        },
        // What a probe after a restart asks GitHub (M31): a pull request whose head is the
        // session's branch.
        "pr.open_draft" => match session_of(ws, op).ok().and_then(|s| ws.state.project(&s.project_id).and_then(|p| ws.tickets_repo_of(&p.root)).map(|repo| (repo, s.branch.clone()))) {
            Some((repo, branch)) => json!({ "repo": repo, "branch": branch }),
            None => Value::Null,
        },
        _ => Value::Null,
    };
    let intent = glasscode_core::effects::Intent {
        origin: origin.clone(),
        subject: subject.clone(),
        target: target.clone(),
        caller: caller.describe(),
        reason: glasscode_core::words::scrub(&what),
        project: caller_key(ws, caller),
        session: caller.session.clone(),
        launch: ws.control.launch.clone(),
        evidence,
        ..glasscode_core::effects::Intent::new(glasscode_core::effects::effect_key(&origin, &subject, op.name(), &key_target), op.name(), op.spec.retry)
    };
    // M58 step 4: with a server paired, an outward or at-most-once effect of the manager is claimed
    // with the server's arbiter at this Mac's lease generation, so an older home's claim never runs
    // twice; a Mac that is not home (the server is, or cannot be reached) runs nothing outward of the
    // manager's. A session's or a program's own effect is not the manager's: where the manager's
    // home is does not decide it (claimed here, as before).
    let manager = matches!(caller.channel, glasscode_core::caller::Caller::Manager { .. });
    let outward = manager && (op.spec.effect == glasscode_core::ops::EffectClass::Outward || op.spec.retry == glasscode_core::ops::Retry::AtMostOnce);
    let arbiter = ws.server.client.clone().filter(|c| ws.server.config.is_some() && c.hello.features.iter().any(|f| f == "home"));
    let registered: Option<Option<(std::sync::Arc<glasscode_core::remote::RemoteClient>, u64)>> = match (outward, arbiter) {
        (true, Some(client)) => Some(ws.home.generation().map(|generation| (client, generation))),
        _ => None,
    };
    let run: crate::control_ui::Run = Box::new(move |ws, r, window, cx| {
        let key = intent.key;
        let registered = match registered {
            Some(None) => {
                let server = ws.server.name();
                ws.reply(r, failed(format!("The manager's home is {server} now (or it cannot be reached), so nothing outward runs from this {}.", crate::platform::computer())));
                return;
            }
            Some(Some(arbiter)) => Some(arbiter),
            None => None,
        };
        let at_server = registered.clone();
        let task = cx.background_spawn(async move {
            if let Some((client, generation)) = &at_server {
                return glasscode_core::home::claim_with_server(client, &intent, *generation).map_err(|err| std::io::Error::other(format!("{err:#}")));
            }
            let home = glasscode_core::paths::MachineId::this().map_err(|err| std::io::Error::other(err.to_string()))?;
            glasscode_core::effects::claim(&intent, &home)
        });
        cx.spawn_in(window, async move |this, cx| {
            let claimed = task.await;
            let _ = this.update_in(cx, |ws, window, cx| match claimed {
                Ok(glasscode_core::effects::Claim::New(_)) => {
                    let caller = r.caller.clone();
                    let job = r.job();
                    let mut local = Responder::local(move |ws, reply| {
                        let first = |text: &str| glasscode_core::words::scrub(&clip(text.lines().next().unwrap_or_default(), 120));
                        let state = match reply {
                            Reply::Done { code: code::OK, text, .. } => glasscode_core::effects::EffectState::Done(json!({ "said": first(text) })),
                            // Stopped when asked (ops-10): a step that ran stays done, one stopped
                            // midway may have taken effect; neither is a failure to run again.
                            Reply::Done { text, data, .. } if data["status"] == "cancelled" => {
                                if !data["midway"].is_null() {
                                    glasscode_core::effects::EffectState::Uncertain
                                } else if data["done"].as_array().is_some_and(|d| !d.is_empty()) {
                                    glasscode_core::effects::EffectState::Done(json!({ "said": first(text), "cancelled_after": data["done"] }))
                                } else {
                                    glasscode_core::effects::EffectState::Failed(first(text))
                                }
                            }
                            Reply::Done { text, .. } => glasscode_core::effects::EffectState::Failed(first(text)),
                            Reply::Waiting { .. } => glasscode_core::effects::EffectState::Uncertain,
                        };
                        ws.reply(r, reply.clone());
                        match registered.clone() {
                            // A registered claim's result goes to the server's ledger.
                            Some((client, _)) => std::mem::drop(std::thread::spawn(move || {
                                if let Err(err) = glasscode_core::home::finish_with_server(&client, &key, &state) {
                                    log::error!("effects: the result of {key} could not be written on the server ({err:#})");
                                }
                            })),
                            None => glasscode_core::effects::finish_later(key, state),
                        }
                    });
                    local.caller = caller;
                    local.set_job(job);
                    run(ws, local, window, cx);
                }
                Ok(glasscode_core::effects::Claim::Already(record)) if by_op_id => {
                    let reply = already(&record, &target, &launch);
                    ws.reply(r, reply)
                }
                Ok(glasscode_core::effects::Claim::Already(record)) => ws.reply(r, failed(format!("This was done before ({:?}); it is not done again.", record.state))),
                Err(err) => ws.reply(r, failed(format!("The effect ledger could not be written ({err}); nothing was done."))),
            });
        })
        .detach();
    });
    Plan { kind, what, run, check }
}

/// What a call with an op id the effect ledger has a record of gets (ops-10): the first call's
/// answer, never a second run. Other arguments under the same id are refused.
fn already(record: &glasscode_core::effects::EffectRecord, target: &str, launch: &str) -> Reply {
    use glasscode_core::effects::EffectState;
    let busy = |text: String| error_reply(&OpError::new(ErrorCode::Busy, text));
    if record.target != target {
        return busy("This op id was used for another call: give this one a new id.".into());
    }
    match &record.state {
        EffectState::Done(value) => Reply::ok(format!("Done before, with this op id: {}", value["said"].as_str().unwrap_or("it ran")), json!({ "status": "done", "repeated": true, "said": value["said"] })),
        EffectState::Failed(why) => Reply::Done { code: code::FAILED, text: format!("It failed before, with this op id, and is not run again yet: {why}"), data: json!({ "status": "failed", "repeated": true }) },
        EffectState::Claimed if record.launch == launch => busy("The first call with this op id is still running: ask again once it answered.".into()),
        EffectState::Claimed | EffectState::Uncertain => Reply::Done {
            code: code::FAILED,
            text: "It was started before Glasscode restarted, and whether it ran is not known: the person decides in Activity. Nothing was run again.".into(),
            data: json!({ "status": "uncertain", "repeated": true, "error_code": "uncertain" }),
        },
    }
}

fn session_hand_off(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let s = session_of(ws, op)?;
    let to = match op.text("to") {
        Some(a) => agent_of(a)?,
        None => s.agent.other(),
    };
    if to == AgentKind::Shell {
        return Err(usage("--to is claude or codex."));
    }
    let account = ws.find_account(to, op.text("account"))?.ok_or_else(|| failed("No account to hand it to."))?;
    let note = arg(op, "note").to_string();
    let new_tab = op.on("new_tab");
    let title = ws.session_title(&s);
    let label = ws.state.lineage_tag(to, Some(&account));
    let id = s.id.clone();
    plan(Kind::Move, what(op, &Names { title: title.clone(), other: label.clone() }, day::today()), move |ws, r, window, cx| {
        ws.reply(r, Reply::ok(format!("Handing “{title}” to {label}."), json!({ "id": id })));
        ws.handoff_now(&id, to, account, note, new_tab, window, cx);
    })
}

fn session_move(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let to = if op.on("to_this_computer") { MoveTo::ThisComputer } else { MoveTo::Server };
    let id = arg(op, "session").to_string();
    let title = ws.state.session(&id).map(|s| ws.session_title(s)).unwrap_or_default();
    if to == MoveTo::ThisComputer {
        let option = crate::platform::computer().to_lowercase();
        return Err(usage(format!("`--to-{option}` brings a server session back; this one runs on {} already.", crate::platform::this_computer())));
    }
    if ws.server.config.is_none() {
        return Err(failed("No server is set up (Settings › Server and devices)."));
    }
    let server = ws.server.name();
    plan(Kind::Move, what(op, &Names { title: title.clone(), other: server.clone() }, day::today()), move |ws, r, _, cx| {
        // A worktree moves with every session in it, never split.
        let with = ws.moves_with(crate::switch_ui::Direction::ToServer, &id);
        ws.move_one(crate::switch_ui::Direction::ToServer, id.clone(), cx);
        let also = match with.len() {
            0 => String::new(),
            _ => format!(" Its worktree moves with it, so these sessions move too: {}.", with.join(", ")),
        };
        ws.reply(r, Reply::ok(format!("Moving “{title}” to {server} once its agents are idle.{also}"), json!({ "id": id, "with": with })));
    })
}

// ----- pull requests ---------------------------------------------------------------------------

fn pr_review(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let n = op.number("number").unwrap_or_default();
    let mut spec = ws.new_spec(caller, op)?;
    spec.review = Some(n);
    // Already reviewing it here: say so instead of starting a second one.
    if let Some(s) = ws.state.sessions.iter().find(|s| s.review.as_ref().is_some_and(|r| r.number == n)) {
        return answer(Reply::ok(format!("Already reviewing #{n} in “{}” ({}).", ws.session_title(s), short(&s.id)), not_created(ws.session_json(s))));
    }
    let check = new_branch_free(ws, &spec);
    checked_if(ws.new_session_plan(spec), check)
}

fn pr_build(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let mut spec = ws.new_spec(caller, op)?;
    spec.pr = op.number("number");
    let check = new_branch_free(ws, &spec);
    checked_if(ws.new_session_plan(spec), check)
}

fn pr_status(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let n = op.number("number").unwrap_or_default();
    let project_id = ws.project_for(caller, op.text("project"))?;
    let root = ws.state.project(&project_id).map(|p| p.root.clone()).unwrap_or_default();
    let env = ws.env.clone();
    plan(Kind::Read, "read", move |_ws, r, _, cx| {
        let task = cx.background_spawn(async move {
            let repo = github::repo_slug(&root).ok_or_else(|| anyhow::anyhow!("this project's origin is not on GitHub"))?;
            let preview = github::PreviewConfig::load(&root);
            github::pr_details(&repo, n, &preview, &env)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, _| {
                ws.reply(r, match result {
                    Ok(d) => pr_status_reply(&d),
                    Err(e) => failed(format!("Could not read #{n}: {e:#}")),
                })
            });
        })
        .detach();
    })
}

fn pr_list(ws: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    let reviews = &ws.tickets.reviews;
    let text = if reviews.is_empty() {
        "No pull requests ask for your review.".to_string()
    } else {
        reviews.iter().map(|r| format!("#{:<6} {}  · {} · by {}", r.hit.number, r.hit.title, r.repo, r.hit.author)).collect::<Vec<_>>().join("\n")
    };
    let data: Vec<Value> = reviews.iter().map(|r| json!({ "repo": r.repo, "number": r.hit.number, "title": r.hit.title, "author": r.hit.author, "url": r.hit.url })).collect();
    answer(Reply::ok(format!("Asking for your review:\n{text}"), Value::Array(data)))
}

/// What a change on GitHub needs: the project's folder and repository, and the environment of
/// its account.
/// The project a GitHub change is for: its folder, its repository when the last scan read it
/// (else git says, off the main thread), and the environment of its GitHub account.
fn github_target(ws: &Workspace, caller: &Caller, op: &Op) -> Result<(std::path::PathBuf, Option<String>, std::collections::HashMap<String, String>), Reply> {
    let project_id = ws.project_for(caller, op.text("project"))?;
    let root = ws.state.project(&project_id).map(|p| p.root.clone()).unwrap_or_default();
    let repo = ws.tickets_repo_of(&root);
    let mut env = (*ws.env).clone();
    env.extend(ws.github_env(&project_id));
    Ok((root, repo, env))
}

const NOT_ON_GITHUB: &str = "This project's origin is not on GitHub.";

type GithubJob = Box<dyn FnOnce(&str, &std::collections::HashMap<String, String>) -> anyhow::Result<()> + Send>;

/// A change on GitHub, in the background; `ok` is what it says when it worked. With no repository
/// from the last scan, git names it: first in the plan's check, before anyone is asked, then for
/// the job.
fn run_gh(root: std::path::PathBuf, repo: Option<String>, env: std::collections::HashMap<String, String>, what: String, ok: String, job: GithubJob) -> Planned {
    let known = repo.is_some();
    let check_root = root.clone();
    let planned = plan(Kind::Github, what, move |_ws, r, _, cx| {
        let task = cx.background_spawn(async move {
            let repo = repo.or_else(|| github::repo_slug(&root)).ok_or_else(|| anyhow::anyhow!("{NOT_ON_GITHUB}"))?;
            job(&repo, &env)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, _| {
                ws.reply(r, match result {
                    Ok(()) => Reply::ok(ok, Value::Null),
                    Err(e) => failed(format!("GitHub said no: {e:#}")),
                })
            });
        })
        .detach();
    });
    if known { planned } else { checked(planned, move || github::repo_slug(&check_root).map(|_| ()).ok_or_else(|| failed(NOT_ON_GITHUB))) }
}

fn pr_rerun(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let n = op.number("number").unwrap_or_default();
    let (root, repo, env) = github_target(ws, caller, op)?;
    let preview_root = root.clone();
    run_gh(root, repo, env, what(op, &Names::default(), day::today()), format!("The failed checks of #{n} run again."), Box::new(move |repo, env| {
        let details = github::pr_details(repo, n, &github::PreviewConfig::load(&preview_root), env)?;
        let mut runs: Vec<u64> = details.checks.iter().filter(|c| c.state() == github::CheckState::Failed).filter_map(|c| c.run_id()).collect();
        runs.sort_unstable();
        runs.dedup();
        if runs.is_empty() {
            anyhow::bail!("#{n} has no failed checks to re-run");
        }
        github::rerun_failed(repo, &runs, env)
    }))
}

fn pr_comment(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let n = op.number("number").unwrap_or_default();
    let (root, repo, env) = github_target(ws, caller, op)?;
    let body = arg(op, "text").trim().to_string();
    run_gh(root, repo, env, what(op, &Names::default(), day::today()), format!("Commented on #{n}."), Box::new(move |repo, env| github::comment(repo, n, &body, env)))
}

fn pr_label(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let n = op.number("number").unwrap_or_default();
    let (root, repo, env) = github_target(ws, caller, op)?;
    let (label, on) = match (op.text("add"), op.text("remove")) {
        (Some(l), None) => (l.to_string(), true),
        (None, Some(l)) => (l.to_string(), false),
        _ => return Err(usage("pr label <number> --add <label> | --remove <label>")),
    };
    let ok = if on { format!("#{n} has the label {label}.") } else { format!("Took {label} off #{n}.") };
    run_gh(root, repo, env, what(op, &Names::default(), day::today()), ok, Box::new(move |repo, env| github::set_label(repo, n, &label, on, env)))
}

fn pr_open_draft(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let s = session_of(ws, op)?;
    // With the v2 engine, a push the manager or a session asks for sends only the commits that
    // session made on its own branch (M23, D13): never a tip it found (P07).
    let own_only = !caller.channel.is_person();
    let root = ws.state.project(&s.project_id).map(|p| p.root.clone()).unwrap_or_default();
    // The default branch is git's to say: read with the push, off the main thread.
    let base = op.text("base").map(str::to_string);
    let mut env = (*ws.env).clone();
    env.extend(ws.github_env(&s.project_id));
    let title = ws.session_title(&s);
    plan(Kind::Github, what(op, &Names { title, other: s.branch.clone() }, day::today()), move |_ws, r, _, cx| {
        let (dir, branch) = (s.worktree.clone(), s.branch.clone());
        let session = s.id.clone();
        // Started without waiting: it says how far it got, and stops when asked (ops-10).
        let job = r.job();
        let task = cx.background_spawn(async move {
            // The commit checked is the commit pushed: the branch may move after the check.
            let tip = if own_only { Some(glasscode_core::profile::own_push_tip(&dir, &branch, &glasscode_core::profile::session_commits(&session))?) } else { None };
            let base = base.unwrap_or_else(|| git::default_branch(&root));
            github::push_and_open_draft_unless(&dir, &branch, &base, &env, job.as_deref(), tip.as_deref())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.pr_refresh(cx);
                ws.reply(r, match result {
                    Ok(()) => Reply::ok(format!("Pushed {} and opened a draft pull request.", s.branch), json!({ "branch": s.branch })),
                    Err(e) => match e.downcast_ref::<glasscode_core::ops::job::Cancelled>() {
                        Some(stop) => glasscode_core::ops::job::cancelled(stop),
                        None => failed(format!("Could not open the draft: {e:#}")),
                    },
                });
            });
        })
        .detach();
    })
}

// ----- projects and worktrees ------------------------------------------------------------------

fn project_list(ws: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    // What the app holds, here; whether each folder is a repository and its GitHub name are the
    // disk's and git's, off the main thread.
    let projects: Vec<_> = ws
        .state
        .visible_projects()
        .map(|p| {
            let sessions = ws.state.sessions.iter().filter(|s| s.project_id == p.id).count();
            let worktrees = ws.worktrees.get(&p.id).map(|w| w.len()).unwrap_or(0);
            (p.id.clone(), p.name.clone(), p.root.clone(), sessions, worktrees, ws.github_account_for(&p.id))
        })
        .collect();
    // Hidden projects (Settings › Git and projects) are left out of the CLI and Glasscode entirely.
    read_later(move || {
        let mut lines = Vec::new();
        let mut data = Vec::new();
        for (id, name, root, sessions, worktrees, account) in projects {
            let git = root.join(".git").exists();
            lines.push(format!(
                "{}  {}  · {} · {}{}",
                name,
                home_relative(&root),
                plural(sessions, "session", "sessions"),
                if git { plural(worktrees, "worktree", "worktrees") } else { "no git".into() },
                account.as_ref().map(|a| format!(" · GitHub {a}")).unwrap_or_default()
            ));
            data.push(json!({ "id": id, "name": name, "root": root, "git": git, "sessions": sessions, "worktrees": worktrees, "github_account": account, "repo": github::repo_slug(&root) }));
        }
        let text = if lines.is_empty() { "No projects yet. `glasscode project import <folder or git URL>` adds one.".to_string() } else { lines.join("\n") };
        Reply::ok(text, Value::Array(data))
    })
}

fn project_import(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let source = arg(op, "source").trim().to_string();
    let folder = crate::commands::expand(&source, caller.cwd.as_deref());
    let name = op.text("name").map(str::to_string);
    if folder.is_dir() {
        let shown = home_relative(&folder);
        return plan(Kind::Import, format!("import the project in {shown}"), move |_, r, _, cx| {
            // Which repository the folder is: git's to say, off the main thread.
            let task = cx.background_spawn(async move { crate::commands2::project_root_of(&folder) });
            cx.spawn(async move |this, cx| {
                let root = task.await;
                let _ = this.update(cx, |ws, cx| {
                    let reply = match root.and_then(|root| ws.add_project_at_root(root, name, cx)) {
                        Ok((id, true)) => Reply::ok(format!("Imported {shown} as {}.", ws.state.project(&id).map(|p| p.name.clone()).unwrap_or_default()), json!({ "id": id })),
                        Ok((id, false)) => Reply::ok(format!("{shown} is a project already ({}).", ws.state.project(&id).map(|p| p.name.clone()).unwrap_or_default()), not_created(json!({ "id": id }))),
                        Err(e) => failed(e),
                    };
                    ws.reply(r, reply);
                });
            })
            .detach();
        });
    }
    let (url, repo_name) = clone_source_checked(&source).map_err(|why| failed(format!("{source} is not a folder here. {why}")))?;
    let into = match op.text("into") {
        Some(dir) => crate::commands::expand(dir, caller.cwd.as_deref()),
        None => ws.projects_folder(),
    };
    // A --name is a folder name too: cleaned the same way, never a path.
    let dest = into.join(name.as_deref().map(clone_folder_name).unwrap_or(repo_name));
    if dest.exists() {
        return Err(failed(format!("{} exists already; import that folder, or choose --name / --into.", home_relative(&dest))));
    }
    let shown = home_relative(&dest);
    let env = ws.env.clone();
    plan(Kind::Import, format!("clone {url} into {shown} and import it"), move |_ws, r, _, cx| {
        let target = dest.clone();
        // Started without waiting: a cancel stops the clone (its half-made folder is removed) or,
        // once it is done, keeps it out of Glasscode (ops-10).
        let job = r.job();
        let stop = job.clone();
        let task = cx.background_spawn(async move {
            if let Some(job) = &job {
                job.step(1, 2, format!("cloning {url}"));
            }
            match crate::commands2::git_clone_unless(&url, &target, &env, job.as_deref()) {
                Ok(true) => {}
                Ok(false) => return Err(None),
                Err(e) => return Err(Some(format!("Could not clone it: {e:#}"))),
            }
            if let Some(job) = &job {
                job.ran(format!("cloning {url} into {}", home_relative(&target)));
                job.step(2, 2, "importing it");
            }
            crate::commands2::project_root_of(&target).map_err(Some)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                let cancelled = |job: &Option<std::sync::Arc<glasscode_core::ops::job::Job>>| job.as_ref().and_then(|j| j.check().err());
                let reply = match result {
                    // Stopped while it cloned: nothing was made.
                    Err(None) => glasscode_core::ops::job::cancelled(&glasscode_core::ops::job::Cancelled { done: Vec::new(), midway: None }),
                    Err(Some(why)) => failed(why),
                    // Cloned, and asked to stop before it was imported: the folder stays, out of Glasscode.
                    Ok(_) if cancelled(&stop).is_some() => glasscode_core::ops::job::cancelled(&cancelled(&stop).expect("checked")),
                    Ok(root) => match ws.add_project_at_root(root, name, cx) {
                        Ok((id, _)) => Reply::ok(format!("Cloned and imported {shown}."), json!({ "id": id })),
                        Err(e) => failed(e),
                    },
                };
                ws.reply(r, reply);
            });
        })
        .detach();
    })
}

fn project_remove(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let id = ws.find_project_as(caller, arg(op, "project"))?;
    let name = ws.state.project(&id).map(|p| p.name.clone()).unwrap_or_default();
    let sessions = ws.state.sessions.iter().filter(|s| s.project_id == id).count();
    plan(Kind::Close, format!("remove the project {name} from Glasscode (closing {}; the folder stays)", plural(sessions, "session", "sessions")), move |ws, r, _, cx| {
        ws.remove_project(&id, cx);
        ws.reply(r, Reply::ok(format!("Removed {name} from Glasscode. Its folder and worktrees stay on disk."), json!({ "id": id })));
    })
}

fn project_set_account(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let id = ws.find_project_as(caller, arg(op, "project"))?;
    let login = arg(op, "github").to_string();
    let known: Vec<String> = ws.github.list.as_ref().map(|l| l.iter().map(|a| a.login.clone()).collect()).unwrap_or_default();
    if login != "default" && !known.is_empty() && !known.iter().any(|k| k.eq_ignore_ascii_case(&login)) {
        return Err(failed(format!("gh is not signed in as {login}. Signed in: {}.", known.join(", "))));
    }
    let name = ws.state.project(&id).map(|p| p.name.clone()).unwrap_or_default();
    plan(Kind::Settings, format!("make {name} use the GitHub account {login}"), move |ws, r, _, cx| {
        let mut before = "default".to_string();
        if let Some(p) = ws.state.projects.iter_mut().find(|p| p.id == id) {
            before = p.github_account.clone().unwrap_or_else(|| "default".into());
            p.github_account = (login != "default").then(|| login.clone());
        }
        ws.save();
        ws.configure_github(cx);
        let data = with_before(json!({ "id": id }), before);
        ws.reply(r, Reply::ok(format!("New sessions in {name} use {login} on GitHub."), data));
    })
}

fn worktree_list(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let only = match op.text("project") {
        Some(p) => Some(ws.find_project_as(caller, p)?),
        None => None,
    };
    let mut lines = Vec::new();
    let mut data = Vec::new();
    for p in ws.state.visible_projects() {
        if only.as_ref().is_some_and(|o| *o != p.id) {
            continue;
        }
        lines.push(format!("{} ({})", p.name, home_relative(&p.root)));
        for (path, branch) in ws.project_worktrees(&p.id) {
            let sessions = ws.state.sessions.iter().filter(|s| s.worktree == path).count();
            let main = path == p.root;
            let removing = ws.removing.contains(&path);
            lines.push(format!(
                "  {:<44} {}{}{}{}",
                if branch.is_empty() { "(no branch)".to_string() } else { branch.clone() },
                home_relative(&path),
                if main { "  · main checkout" } else { "" },
                if sessions > 0 { format!("  · {}", plural(sessions, "session", "sessions")) } else { String::new() },
                if removing { "  · removing…" } else { "" }
            ));
            data.push(json!({ "project": p.name, "path": path, "branch": branch, "main": main, "sessions": sessions }));
        }
    }
    answer(Reply::ok(if lines.is_empty() { "No projects.".to_string() } else { lines.join("\n") }, Value::Array(data)))
}

fn worktree_new(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let project_id = ws.project_for(caller, op.text("project"))?;
    let branch = arg(op, "branch").trim().replace(' ', "-");
    let project = ws.state.project(&project_id).cloned().ok_or_else(|| failed("That project is gone."))?;
    let (check_root, check_branch) = (project.root.clone(), branch.clone());
    let from = op.text("from").map(str::to_string);
    let fetch = !ws.state.prefs.fresh_main_off;
    let planned = plan(Kind::Start, format!("make a worktree for the new branch {branch} in {}", project.name), move |_ws, r, _, cx| {
        let (root, name, b) = (project.root.clone(), project.name.clone(), branch.clone());
        // Started without waiting: it says how far it got, and stops before the worktree is made
        // when asked (ops-10). A fetch that runs finishes; it changes only what is known of origin.
        let job = r.job();
        let task = cx.background_spawn(async move {
            use glasscode_core::ops::job;
            let base = match from {
                Some(f) => f,
                None => {
                    if let Some(job) = &job {
                        job.step(1, 2, if fetch { "fetching the newest main" } else { "finding the base" });
                    }
                    git::start_base(&root, fetch).base
                }
            };
            if let Some(job) = &job {
                job.ran(format!("finding the base ({base})"));
                job.check()?;
                job.step(2, 2, format!("making the worktree for {b}"));
            }
            git::add_worktree(&root, &name, &b, &base).map(|path| (path, base)).map_err(job::Failed::from)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.refresh_worktrees(cx);
                ws.reply(r, match result {
                    Ok((path, base)) => Reply::ok(format!("Made {} on {branch} (from {base}).", home_relative(&path)), json!({ "path": path, "branch": branch, "base": base })),
                    Err(glasscode_core::ops::job::Failed::Cancelled(stop)) => glasscode_core::ops::job::cancelled(&stop),
                    Err(glasscode_core::ops::job::Failed::Error(e)) => failed(format!("Could not make the worktree: {e:#}")),
                });
            });
        })
        .detach();
    });
    checked(planned, move || if git::branch_exists(&check_root, &check_branch) { Err(failed(format!("Branch {check_branch} exists already."))) } else { Ok(()) })
}

fn worktree_rename(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let new = arg(op, "branch").trim().replace(' ', "-");
    let (project_id, wt) = ws.find_worktree_as(caller, arg(op, "worktree"), None)?;
    if wt.is_detached() {
        return Err(failed("That worktree is not on a branch."));
    }
    let root = ws.state.project(&project_id).map(|p| p.root.clone()).unwrap_or_default();
    let check_new = new.clone();
    let force = op.on("force");
    let old = wt.branch.clone();
    let planned = plan(Kind::Rename, format!("rename the branch {old} to {new}"), move |_ws, r, _, cx| {
        let path = wt.path.clone();
        let (o, n) = (old.clone(), new.clone());
        let task = cx.background_spawn(async move {
            if !force && git::branch_was_pushed(&path, &o) {
                anyhow::bail!("{o} is on GitHub already (a pull request may use it); renaming it there is not done here. Use --force to rename only this copy.");
            }
            git::rename_branch(&path, &o, &n)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                if result.is_ok() {
                    for s in ws.state.sessions.iter_mut().filter(|s| s.worktree == wt.path) {
                        s.branch = new.clone();
                    }
                    ws.save();
                    ws.refresh_worktrees(cx);
                }
                ws.reply(r, match result {
                    Ok(()) => Reply::ok(format!("Renamed {old} to {new} ({}).", home_relative(&wt.path)), with_before(json!({ "path": wt.path, "branch": new }), old.clone())),
                    Err(e) => failed(format!("{e:#}")),
                });
            });
        })
        .detach();
    });
    checked(planned, move || if git::branch_exists(&root, &check_new) { Err(failed(format!("Branch {check_new} exists already."))) } else { Ok(()) })
}

fn worktree_remove(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let (project_id, wt) = ws.find_worktree_as(caller, arg(op, "worktree"), None)?;
    let root = ws.state.project(&project_id).map(|p| p.root.clone()).unwrap_or_default();
    if wt.is_main || wt.path == root {
        return Err(failed("That is the project's own folder (the main checkout); it is not removed."));
    }
    let (delete_branch, force) = (op.on("delete_branch"), op.on("force"));
    let sessions = ws.state.sessions.iter().filter(|s| s.worktree == wt.path).count();
    let mut what = format!("remove the worktree {}", if wt.branch.is_empty() || wt.is_detached() { home_relative(&wt.path) } else { wt.branch.clone() });
    if delete_branch {
        what.push_str(" and its branch");
    }
    if sessions > 0 {
        what.push_str(&format!(" (closing {} there)", plural(sessions, "session", "sessions")));
    }
    plan(Kind::Remove, what, move |ws, r, window, cx| ws.remove_worktree_now(root, wt, delete_branch, force, r, window, cx))
}

fn worktree_clean(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let only = match op.text("project") {
        Some(p) => Some(ws.find_project_as(caller, p)?),
        None => caller.session.as_deref().and_then(|s| ws.state.session(s)).map(|s| s.project_id.clone()),
    };
    let older = match op.text("older") {
        Some(t) => Some(days(t).ok_or_else(|| usage(format!("--older takes days: 14d, 2w (not {t}).")))?),
        None => None,
    };
    let (merged, dry, keep_branch) = (op.on("merged"), op.on("dry_run"), op.on("keep_branch"));
    let scope = only.as_ref().and_then(|id| ws.state.project(id)).map(|p| p.name.clone()).unwrap_or_else(|| "every project".into());
    let mut filter = Vec::new();
    if merged {
        filter.push("merged".to_string());
    }
    if let Some(d) = older {
        filter.push(format!("untouched for {d} days"));
    }
    let what = format!("remove the worktrees in {scope} that have nothing to lose{}", if filter.is_empty() { String::new() } else { format!(" ({})", filter.join(", ")) });
    // A dry run only says which would go: a read.
    plan(if dry { Kind::Read } else { Kind::Remove }, what, move |ws, r, _, cx| ws.clean_worktrees(only, merged, older, dry, keep_branch, r, cx))
}

fn worktree_open(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let (_, wt) = ws.find_worktree_as(caller, arg(op, "worktree"), None)?;
    let shown = home_relative(&wt.path);
    plan(Kind::Show, format!("open {shown} in your editor"), move |ws, r, _, cx| {
        let (program, env) = (ws.preferred_editor().map(|(_, program)| program), ws.env.clone());
        // From a task of its own, as a click's open (`center::start_outside`); the reply waits for it.
        crate::center::start_outside(
            move || match program {
                Some(program) => sys::command(program).arg(&wt.path).envs(env.iter()).spawn().map(|_| ()),
                None => sys::open_path(&wt.path),
            },
            move |ws, result, _| {
                ws.reply(r, match result {
                    Ok(()) => Reply::ok(format!("Opened {shown}."), Value::Null),
                    Err(e) => failed(format!("Could not open an editor: {e}")),
                })
            },
            cx,
        );
    })
}

// ----- accounts and tickets --------------------------------------------------------------------

fn account_list(ws: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    answer(ws.accounts_reply())
}

/// The ticket the op names, among the person's.
fn ticket_of(ws: &Workspace, op: &Op) -> Result<linear::Ticket, Reply> {
    let id = arg(op, "ticket").to_uppercase();
    ws.tickets.ticket(&id).cloned().ok_or_else(|| failed(format!("No ticket {id} among yours in Linear{}.", if ws.tickets.connected { "" } else { " (Linear is not connected: Settings › Integrations)" })))
}

fn ticket_list(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let Some(snapshot) = ws.tickets.snapshot.as_ref() else {
        return Err(failed(if ws.tickets.connected { "Linear has not answered yet; try again in a moment." } else { "Linear is not connected (Settings › Integrations › Linear)." }));
    };
    let list: Vec<&linear::Ticket> = if op.on("review") { ws.tickets.reviewing() } else { snapshot.mine.iter().collect() };
    let open: Vec<&linear::Ticket> = list.into_iter().filter(|t| op.on("all") || !t.is_done()).collect();
    let text = if open.is_empty() {
        "No tickets.".to_string()
    } else {
        open.iter().map(|t| format!("{:<8} {:<12} {}{}", t.identifier, t.status.name, t.title, t.due.as_deref().map(|d| format!("  · due {}", linear::short_date(d))).unwrap_or_default())).collect::<Vec<_>>().join("\n")
    };
    answer(Reply::ok(text, serde_json::to_value(&open).unwrap_or_default()))
}

fn ticket_show(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let ticket = ticket_of(ws, op)?;
    plan(Kind::Read, "read", move |_ws, r, _, cx| {
        let id = ticket.id.clone();
        let task = cx.background_spawn(async move { linear::Client::saved().ok_or_else(|| anyhow::anyhow!("Linear is not connected"))?.detail(&id) });
        cx.spawn(async move |this, cx| {
            let detail = task.await;
            let _ = this.update(cx, |ws, _| {
                let mut text = format!("{} {}\n{} · {}{}\n{}", ticket.identifier, ticket.title, ticket.status.name, ticket.priority_name(), ticket.due.as_deref().map(|d| format!(" · due {}", linear::short_date(d))).unwrap_or_default(), ticket.url);
                let mut data = serde_json::to_value(&ticket).unwrap_or_default();
                match detail {
                    Ok(d) => {
                        if !d.description.trim().is_empty() {
                            text.push_str(&format!("\n\n{}", linear::markdown(&d.description).trim()));
                        }
                        for c in &d.comments {
                            text.push_str(&format!("\n\n{} ({}): {}", c.author, linear::short_date(&c.created_at), c.body.trim()));
                        }
                        data["description"] = json!(d.description);
                        data["comments"] = json!(d.comments.iter().map(|c| json!({ "author": c.author, "body": c.body, "at": c.created_at })).collect::<Vec<_>>());
                    }
                    Err(e) => text.push_str(&format!("\n\n(Its description and comments could not be read: {e:#})")),
                }
                ws.reply(r, Reply::ok(text, data));
            });
        })
        .detach();
    })
}

fn ticket_start(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let ticket = ticket_of(ws, op)?;
    let mut spec = ws.new_spec(caller, op)?;
    spec.ticket = Some(ticket);
    let check = new_branch_free(ws, &spec);
    checked_if(ws.new_session_plan(spec), check)
}

fn ticket_move(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let ticket = ticket_of(ws, op)?;
    let to = arg(op, "to").trim().to_string();
    plan(Kind::Tickets, what(op, &Names::default(), day::today()), move |_ws, r, _, cx| {
        let t = ticket.clone();
        let wanted = to.clone();
        let task = cx.background_spawn(async move {
            let client = linear::Client::saved().ok_or_else(|| anyhow::anyhow!("Linear is not connected"))?;
            let detail = client.detail(&t.id)?;
            let (state_id, status) = detail
                .states
                .iter()
                .find(|(_, st)| st.name.eq_ignore_ascii_case(&wanted))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("its team has no “{wanted}” status (it has: {})", detail.states.iter().map(|(_, s)| s.name.clone()).collect::<Vec<_>>().join(", ")))?;
            client.set_status(&t.id, &state_id)?;
            Ok::<_, anyhow::Error>(status)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                let reply = match result {
                    Ok(status) => {
                        let name = status.name.clone();
                        ws.tickets_set_status(&ticket.identifier, status);
                        ws.tickets.last_fetch = None;
                        ws.tickets_refresh(cx);
                        Reply::ok(format!("{} is {name} in Linear.", ticket.identifier), with_before(json!({ "ticket": ticket.identifier, "status": name }), ticket.status.name.clone()))
                    }
                    Err(e) => failed(format!("Could not move {}: {e:#}", ticket.identifier)),
                };
                ws.reply(r, reply);
            });
        })
        .detach();
    })
}

fn ticket_comment(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let ticket = ticket_of(ws, op)?;
    let body = arg(op, "text").trim().to_string();
    plan(Kind::Tickets, what(op, &Names::default(), day::today()), move |_ws, r, _, cx| {
        let id = ticket.id.clone();
        let text = body.clone();
        let task = cx.background_spawn(async move { linear::Client::saved().ok_or_else(|| anyhow::anyhow!("Linear is not connected"))?.comment(&id, &text) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, _| {
                ws.reply(r, match result {
                    Ok(()) => Reply::ok(format!("Commented on {}.", ticket.identifier), json!({ "ticket": ticket.identifier })),
                    Err(e) => failed(format!("Could not comment: {e:#}")),
                })
            });
        })
        .detach();
    })
}

// ----- the app ---------------------------------------------------------------------------------

fn app_status(ws: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    answer(ws.status_reply())
}

/// `words recent`: the newest records of the words ledger and where each came from. A debug op
/// for the person only: it is their text, and its folder is denied to sessions. It reads the
/// disk, so off the main thread.
fn words_recent(_: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    if !caller.is_person() {
        return Err(denied("Only the person can read the words ledger (it holds what they typed)."));
    }
    let limit = op.number("limit").unwrap_or(20) as usize;
    plan(Kind::Read, "read the words ledger", move |_ws, r, _, cx| {
        let task = cx.background_spawn(async move { (glasscode_core::words::mode(), glasscode_core::words::recent(limit)) });
        cx.spawn(async move |this, cx| {
            let (mode, records) = task.await;
            let _ = this.update(cx, |ws, _| ws.reply(r, crate::commands::words_reply(mode, &records)));
        })
        .detach();
    })
}

// ----- memory (M20.2) ---------------------------------------------------------------------------

use glasscode_core::memstore::{self, MemFile, Memory, MemoryError, Who};

/// Who asks memory, and about what work: read from the window's state, used on a background thread.
#[derive(Clone)]
struct MemoryAsk {
    who: Who,
    /// Every project is the caller's to read: the person (with no project named) and the manager.
    everywhere: bool,
    /// The project: its id, its folder, and its key when bound.
    project: Option<(String, std::path::PathBuf, Option<glasscode_core::paths::ProjectKey>)>,
    /// The session whose work it is.
    session: Option<glasscode_core::model::Session>,
    /// What its work is keyed by beyond the session: the window's state's ticket and pull request
    /// links (`State::work_key`).
    links: glasscode_core::model::State,
    /// The policy header of the project (its level, from the gate).
    policy: Option<String>,
    /// The projects the caller may not read at all: for the manager, those bound to another
    /// account than its own (D17). Its reads that name no project leave them out.
    hidden: Vec<glasscode_core::paths::ProjectKey>,
}

impl MemoryAsk {
    /// The project's key: its binding, else bound now (`memstore::bind_project`: only a folder that
    /// is there). Runs git: background only.
    fn key(&self) -> Result<Option<glasscode_core::paths::ProjectKey>, Reply> {
        let Some((id, root, key)) = &self.project else { return Ok(None) };
        if let Some(key) = key {
            return Ok(Some(key.clone()));
        }
        match memstore::bind_project(id, root) {
            Ok(glasscode_core::identity::KeyMatch::Known(key) | glasscode_core::identity::KeyMatch::New(key)) => Ok(Some(key)),
            Ok(_) => Err(failed("This project's folder may be a fork of another project: say which in Settings › Git and projects first.")),
            Err(e) => Err(failed(format!("This project has no memory key yet: {e:#}"))),
        }
    }

    /// How far a change by id may reach (`memory.merge`, `let_go`, `rewrite`): anywhere for the
    /// person and the manager; a session only its own project's memory (mem-18), so it cannot
    /// change another project's entries by their ids. Runs git: background only.
    fn within(&self) -> Result<Option<glasscode_core::paths::ProjectKey>, Reply> {
        match self.who {
            Who::Session(_) => self.key()?.map(Some).ok_or_else(|| denied("This command runs in no project's session, so it has no project's memory.")),
            Who::You | Who::Manager => Ok(None),
        }
    }

    /// The piece of work of the session, in project `key`.
    fn work(&self, key: &glasscode_core::paths::ProjectKey) -> Option<glasscode_core::identity::WorkKey> {
        self.session.as_ref().map(|s| self.links.work_key(key, s))
    }
}

/// Who a caller is to memory (D1, D2): the person only in the app's own window; the manager by its
/// channel; anyone else (a session, a command typed in a shell tab, a program outside) writes as a
/// session does, and never as the person.
fn memory_who(caller: &Caller) -> Who {
    match &caller.channel {
        glasscode_core::caller::Caller::Ui => Who::You,
        glasscode_core::caller::Caller::Manager { .. } => Who::Manager,
        glasscode_core::caller::Caller::Session { id, .. } | glasscode_core::caller::Caller::ShellTab { session: id, .. } => Who::Session(id.clone()),
        _ => Who::Session(caller.session.clone().unwrap_or_else(|| "outside".into())),
    }
}

/// Which project and session a memory op of this caller is about, or why it may not ask:
/// the person in the app and the manager may name any project or session (with neither, they read
/// every project); anyone else only its own session and its own project, and has to have one.
/// `own` is the caller's own session and its project; `named` what the op names.
fn memory_reach(who: &Who, own: (Option<&str>, Option<&str>), named: (Option<&str>, Option<&str>)) -> Result<Reach, String> {
    let (own_session, own_project) = own;
    let (named_session, named_project) = named;
    if matches!(who, Who::You | Who::Manager) {
        return Ok(if named_session.is_none() && named_project.is_none() { Reach::Everywhere } else { Reach::Named });
    }
    if let Some(named) = named_session
        && Some(named) != own_session
    {
        return Err("A session reads and writes the memory of its own work only.".into());
    }
    if let Some(named) = named_project
        && Some(named) != own_project
    {
        return Err("A session reads and writes the memory of its own project only.".into());
    }
    if own_project.is_none() {
        return Err("This command runs in no project's session, so it has no project's memory.".into());
    }
    Ok(Reach::Own)
}

/// What [`memory_reach`] allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    /// Every project (the person in the app, the manager): naming none.
    Everywhere,
    /// The project or session the person or the manager named.
    Named,
    /// The caller's own project and session.
    Own,
}

/// What a memory op knows of its caller: who it is, its project (`--project`, else its session's)
/// and its session (`--session`, else its own), within [`memory_reach`].
fn memory_ask(ws: &Workspace, caller: &Caller, op: &Op) -> Result<MemoryAsk, Reply> {
    let who = memory_who(caller);
    let world = ws.world(caller);
    let own_session = caller.session.clone().filter(|id| ws.state.session(id).is_some());
    let own_project = match &own_session {
        Some(id) => ws.state.session(id).map(|s| s.project_id.clone()),
        None if who == Who::You || who == Who::Manager => None,
        None => ops::project_for(&world, None).ok(),
    };
    let named_project = match op.text("project") {
        Some(word) => Some(ops::find_project(&world, word).map_err(|e| error_reply(&e))?),
        None => None,
    };
    let named_session = op.text("session").map(str::to_string);
    let reach = memory_reach(&who, (own_session.as_deref(), own_project.as_deref()), (named_session.as_deref(), named_project.as_deref())).map_err(denied)?;
    let session_id = named_session.or(own_session);
    let session = session_id.as_deref().and_then(|id| ws.state.session(id)).cloned();
    let project_id = named_project.or_else(|| session.as_ref().map(|s| s.project_id.clone())).or(own_project);
    let project = project_id.and_then(|id| ws.state.project(&id).map(|p| (p.id.clone(), p.root.clone(), ws.control.project_keys.get(&p.id).cloned())));
    if reach == Reach::Own && project.is_none() {
        return Err(denied("This command runs in no project's session, so it has no project's memory."));
    }
    let everywhere = reach == Reach::Everywhere;
    // D17: the manager reads no project bound to another account, named or not (the gate refuses
    // one an op names; a session named here is about its project too).
    let hidden = if who == Who::Manager {
        let set = ws.control_policy_set();
        set.hidden_from_manager(ws.control.project_keys.values())
    } else {
        Vec::new()
    };
    if let Some((_, _, Some(key))) = &project
        && hidden.contains(key)
    {
        return Err(denied(format!("Only the account the person chose may read {key}, and the manager runs on another.")));
    }
    let policy = project.as_ref().and_then(|(_, _, key)| key.as_ref()).map(|key| {
        let level = ws.control_policy_set().policy.of(key).level;
        format!("Project {key} is at the {level:?} level. Glasscode's gate decides what runs; this line only says so.")
    });
    // Only the session's own links go to the background thread, not the whole state.
    let mut links = glasscode_core::model::State::default();
    if let Some(s) = &session {
        links.ticket_of.extend(ws.state.ticket_of.get(&s.id).map(|t| (s.id.clone(), t.clone())));
        links.pr_of.extend(ws.state.pr_of.get(&s.id).map(|p| (s.id.clone(), p.clone())));
    }
    Ok(MemoryAsk { who, everywhere, project, session, links, policy, hidden })
}

/// A memory error as the command's reply.
fn memory_reply(err: MemoryError) -> Reply {
    glasscode_core::ops::core_reads::memory_reply(err)
}

/// Run `job` off the main thread, and answer with what it gives.
fn memory_plan(kind: Kind, what: impl Into<String>, job: impl FnOnce() -> Reply + Send + 'static) -> Planned {
    plan(kind, what, move |_ws, r, _, cx| {
        let task = cx.background_spawn(async move { job() });
        cx.spawn(async move |this, cx| {
            let reply = task.await;
            let _ = this.update(cx, |ws, _| ws.reply(r, reply));
        })
        .detach();
    })
}

/// The person only (the gate refuses these to anyone else too, `human_only`).
fn person_only(caller: &Caller, what: &str) -> Result<(), Reply> {
    if caller.is_person() { Ok(()) } else { Err(denied(format!("Only the person can {what}, in Glasscode."))) }
}

fn written(written: memstore::Written, done: &str) -> Reply {
    match written {
        memstore::Written::Commit(commit) => Reply::ok(done.to_string(), json!({ "commit": commit })),
        memstore::Written::Unchanged => Reply::ok(format!("{done} (it was already so)"), json!({ "commit": Value::Null })),
    }
}

fn memory_search(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let ask = memory_ask(ws, caller, op)?;
    let query = arg(op, "query").to_string();
    let limit = op.number("limit").unwrap_or(5).clamp(1, 50) as usize;
    let history = op.on("history");
    memory_plan(Kind::Read, "search memory", move || {
        let run = || -> Result<Reply, Reply> {
            let key = if ask.everywhere { None } else { ask.key()? };
            let work = key.as_ref().and_then(|key| ask.work(key));
            let root = ask.project.as_ref().map(|(_, root, _)| root.as_path());
            glasscode_core::ops::core_reads::memory_search(key.as_ref(), work.as_ref(), root, &query, limit, history, &ask.hidden)
        };
        run().unwrap_or_else(|reply| reply)
    })
}

fn memory_show(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let ask = memory_ask(ws, caller, op)?;
    let what = arg(op, "what").trim().to_string();
    memory_plan(Kind::Read, "read memory", move || {
        let run = || -> Result<Reply, Reply> {
            let key = if ask.everywhere { None } else { ask.key()? };
            glasscode_core::ops::core_reads::memory_show(key.as_ref(), &what, &ask.hidden)
        };
        run().unwrap_or_else(|reply| reply)
    })
}

fn memory_brief(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let ask = memory_ask(ws, caller, op)?;
    let hook = op.on("hook");
    let manager_lines: Vec<memstore::brief::ProjectLine> = if ask.session.is_none() && matches!(ask.who, Who::Manager) {
        let set = ws.control_policy_set();
        ws.state
            .projects
            .iter()
            .filter_map(|p| ws.control.project_keys.get(&p.id).map(|key| (p, key)))
            // D17: a project bound to another account is not in the manager's brief.
            .filter(|(_, key)| set.manager_may_read(key))
            .map(|(p, key)| {
                let policy = set.policy.of(key);
                memstore::brief::ProjectLine { key: key.clone(), name: p.name.clone(), level: format!("{:?}", policy.level), local_only: policy.local_only }
            })
            .collect()
    } else {
        Vec::new()
    };
    memory_plan(Kind::Read, "read the memory brief", move || {
        let run = || -> Result<Reply, Reply> {
            let snap = Memory::open().snapshot().map_err(memory_reply)?;
            let key = ask.key()?;
            let work = key.as_ref().and_then(|key| ask.work(key));
            let folder = ask.session.as_ref().map(|s| s.worktree.clone());
            let what = memstore::brief::BriefFor {
                project: if manager_lines.is_empty() { key.as_ref() } else { None },
                work: work.as_ref(),
                folder: folder.as_deref(),
                policy: ask.policy.clone(),
                projects: manager_lines,
                today: glasscode_core::day::iso(glasscode_core::day::local((glasscode_core::clock::now_ms() / 1000) as i64)),
            };
            let root = ask.project.as_ref().map(|(_, root, _)| root.clone());
            let holds = move |code: &memstore::CodeRef| root.as_ref().is_none_or(|root| memstore::code_holds(root, code));
            let brief = memstore::brief::build(&snap, &what, &holds).map_err(memory_reply)?;
            if hook {
                // The fixed instructions first, then this start's brief: within Claude's 10,000.
                let context = format!("{}\n\n{}", glasscode_core::cli::MEMORY_INSTRUCTIONS, brief.text);
                let out = json!({ "hookSpecificOutput": { "hookEventName": "SessionStart", "additionalContext": context } });
                return Ok(Reply::ok(out.to_string(), json!({ "commit": brief.commit })));
            }
            Ok(Reply::ok(brief.text, json!({ "commit": brief.commit })))
        };
        run().unwrap_or_else(|reply| reply)
    })
}

fn memory_note(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let ask = memory_ask(ws, caller, op)?;
    if ask.session.is_none() {
        return Err(usage("Say whose work it is about: `glasscode memory note \"<text>\" --session <id>` (a session's own note needs no --session)."));
    }
    let text = arg(op, "text").to_string();
    memory_plan(Kind::Board, format!("note in memory: {}", short(&text)), move || {
        let run = || -> Result<Reply, Reply> {
            let key = ask.key()?.ok_or_else(|| failed("That session's project has no memory key."))?;
            let work = ask.work(&key).ok_or_else(|| failed("That session has no piece of work."))?;
            // The work file names its worktree, so its archive finds it when the worktree goes (M24).
            let worktree = ask.session.as_ref().map(|s| s.worktree.clone());
            let remembered = Memory::open().note_in(&ask.who, &work, worktree.as_deref(), &text).map_err(memory_reply)?;
            Ok(remembered_reply(remembered, &format!("Noted in the work file of {}", work.id)))
        };
        run().unwrap_or_else(|reply| reply)
    })
}

fn remembered_reply(remembered: memstore::Remembered, done: &str) -> Reply {
    match remembered {
        memstore::Remembered::Entry { id, status, written: w } => {
            let mut reply = written(w, done);
            if let Reply::Done { data, .. } = &mut reply {
                data["id"] = json!(id);
                data["status"] = json!(status.word());
            }
            reply
        }
        memstore::Remembered::Proposed { id, why, .. } => Reply::ok(format!("Proposed ({why}); it waits for the person's tick. Proposal {id}."), json!({ "id": id, "proposed": true, "why": why })),
    }
}

/// The file a proposal names (`--file`): decisions by default.
fn memory_file(word: Option<&str>, key: Option<&glasscode_core::paths::ProjectKey>, employer: Option<memstore::Employer>) -> Result<MemFile, Reply> {
    let word = word.unwrap_or("decisions").trim().to_lowercase();
    let need = || key.cloned().ok_or_else(|| usage("Name the project (--project), or propose to the person's profile (--file profile)."));
    let org = || employer.clone().ok_or_else(|| usage("This project names no employer, so it has no people or glossary file."));
    Ok(match word.as_str() {
        "profile" | "you" => MemFile::Profile,
        "project" => MemFile::Project(need()?),
        "policy" => MemFile::Policy(need()?),
        "decisions" | "decision" => MemFile::Decisions(need()?),
        "people" => MemFile::People(org()?),
        "glossary" => MemFile::Glossary(org()?),
        other => match other.strip_prefix("knowhow:").and_then(memstore::layout::topic) {
            Some(topic) => MemFile::Knowhow(need()?, topic),
            None => return Err(usage(format!("--file is project, decisions, policy, profile, people, glossary or knowhow:<topic> (not {other})."))),
        },
    })
}

fn memory_propose(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let ask = memory_ask(ws, caller, op)?;
    let text = arg(op, "text").to_string();
    let file_word = op.text("file").map(str::to_string);
    let section = op.text("section").map(str::to_string);
    memory_plan(Kind::Board, format!("propose to memory: {}", short(&text)), move || {
        let run = || -> Result<Reply, Reply> {
            let memory = Memory::open();
            let key = ask.key()?;
            let employer = match &key {
                Some(key) => memory.snapshot().and_then(|snap| snap.employer_of(key)).map_err(memory_reply)?,
                None => None,
            };
            let file = memory_file(file_word.as_deref(), key.as_ref(), employer)?;
            let source = match &ask.who {
                Who::You => memstore::Source::PersonEdit,
                Who::Manager => memstore::Source::Session("manager".into()),
                Who::Session(id) => memstore::Source::Session(id.clone()),
            };
            let (id, _) = memory.propose(&ask.who, &file, section.as_deref(), &text, source).map_err(memory_reply)?;
            Ok(Reply::ok(format!("Proposed for {}; it waits for the person's tick. Proposal {id}.", file.rel()), json!({ "id": id, "file": file.rel(), "proposed": true })))
        };
        run().unwrap_or_else(|reply| reply)
    })
}

/// A proposal's id, or a file with a pending edit.
fn is_file_word(word: &str) -> bool {
    word.contains('/') || word.ends_with(".md")
}

fn memory_accept(_: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    person_only(caller, "accept what memory keeps")?;
    let what = arg(op, "what").trim().to_string();
    memory_plan(Kind::Board, format!("accept {what} into memory"), move || {
        let memory = Memory::open();
        let done = if is_file_word(&what) { memory.accept(std::slice::from_ref(&what)) } else { memory.accept_proposal(&what) };
        done.map(|w| written(w, &format!("Accepted {what}."))).unwrap_or_else(memory_reply)
    })
}

fn memory_reject(_: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    person_only(caller, "reject what waits for memory")?;
    let what = arg(op, "what").trim().to_string();
    memory_plan(Kind::Board, format!("reject {what}"), move || {
        let memory = Memory::open();
        if is_file_word(&what) {
            return memory.discard(std::slice::from_ref(&what)).map(|()| Reply::ok(format!("Discarded the edit of {what}."), json!({}))).unwrap_or_else(memory_reply);
        }
        memory.reject_proposal(&what).map(|w| written(w, &format!("Rejected proposal {what}."))).unwrap_or_else(memory_reply)
    })
}

fn memory_merge(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let ask = memory_ask(ws, caller, op)?;
    let ids: Vec<String> = arg(op, "ids").split_whitespace().map(str::to_string).collect();
    let text = arg(op, "text").to_string();
    memory_plan(Kind::Board, format!("merge {} entries of memory", ids.len()), move || {
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let within = match ask.within() {
            Ok(within) => within,
            Err(reply) => return reply,
        };
        Memory::open().merge(&ask.who, within.as_ref(), &ids, &text).map(|done| remembered_reply(done, "Merged")).unwrap_or_else(memory_reply)
    })
}

fn memory_let_go(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let ask = memory_ask(ws, caller, op)?;
    let id = arg(op, "id").trim().to_string();
    memory_plan(Kind::Board, format!("let {id} go"), move || match ask.within() {
        Ok(within) => Memory::open().let_go(&ask.who, within.as_ref(), &id).map(|w| written(w, &format!("Let {id} go."))).unwrap_or_else(memory_reply),
        Err(reply) => reply,
    })
}

fn memory_rewrite(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let ask = memory_ask(ws, caller, op)?;
    let id = arg(op, "id").trim().to_string();
    let text = arg(op, "text").to_string();
    memory_plan(Kind::Board, format!("rewrite {id}"), move || match ask.within() {
        Ok(within) => Memory::open().rewrite(&ask.who, within.as_ref(), &id, &text).map(|done| remembered_reply(done, &format!("Rewrote {id}"))).unwrap_or_else(memory_reply),
        Err(reply) => reply,
    })
}

fn memory_forget(_: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    person_only(caller, "forget what memory keeps")?;
    let ids: Vec<String> = arg(op, "ids").split_whitespace().map(str::to_string).collect();
    let secret = op.on("secret");
    memory_plan(Kind::Board, format!("forget {} entries of memory", ids.len()), move || {
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        match Memory::open().forget(&ids, secret) {
            Ok(forgot) if forgot.history_restarted => Reply::ok(format!("Forgot {}. {}", ids.join(", "), memstore::Forgot::HISTORY_GONE), json!({ "history_restarted": true })),
            Ok(_) => Reply::ok(format!("Forgot {}.", ids.join(", ")), json!({ "history_restarted": false })),
            Err(err) => memory_reply(err),
        }
    })
}

fn memory_pending(_: &mut Workspace, caller: &Caller, _: &Op) -> Planned {
    person_only(caller, "see what waits for memory")?;
    memory_plan(Kind::Read, "read what waits for memory", move || {
        let run = || -> Result<Reply, Reply> {
            let memory = Memory::open();
            let pending = memory.pending().map_err(memory_reply)?;
            let proposals = memory.snapshot().and_then(|snap| snap.doc(&MemFile::Proposals)).map_err(memory_reply)?;
            let mut text = String::new();
            let mut listed = Vec::new();
            for e in proposals.entries() {
                text.push_str(&format!("proposal {}: {} → {}\n", e.header.id, e.text, e.header.to.as_deref().unwrap_or("?")));
                listed.push(json!({ "proposal": e.header.id, "text": e.text, "to": e.header.to }));
            }
            for edit in &pending.edits {
                text.push_str(&format!("edit of {} ({:?}):\n{}\n", edit.path, edit.change, edit.diff.trim_end()));
                listed.push(json!({ "edit": edit.path, "change": format!("{:?}", edit.change).to_lowercase(), "diff": edit.diff }));
            }
            if pending.foreign_commits.is_some() {
                text.push_str("Memory also has commits made outside Glasscode; their changes are the edits above.\n");
            }
            if text.is_empty() {
                text = "Nothing waits for you in memory.".into();
            }
            Ok(Reply::ok(text.trim_end().to_string(), json!({ "pending": listed })))
        };
        run().unwrap_or_else(|reply| reply)
    })
}

// ----- your data (M25) ------------------------------------------------------------------------

impl Workspace {
    /// Settings › Manager › Your data: export to a dated file in Downloads, in the background.
    pub(crate) fn data_export_now(&mut self, cx: &mut Context<Self>) {
        let local_only = self.local_only_keys();
        let to = default_export_file();
        let task = cx.background_spawn(async move { glasscode_core::portability::export(&to, &local_only, &[]) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| match result {
                Ok(done) => ws.show_toast(format!("Exported your data to {}.", home_relative(&done.path)), cx),
                Err(err) => ws.show_toast(format!("Could not export: {err:#}"), cx),
            });
        })
        .detach();
    }

    /// Settings › Manager › Your data: an export picked in the file dialog, imported as
    /// `glasscode data import <file>` does (the person's own click).
    pub(crate) fn data_import_pick(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Import".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |ws, cx| {
                ws.show_toast(format!("Importing {}…", home_relative(&path)), cx);
                let shown = path.clone();
                let task = cx.background_spawn(async move { glasscode_core::portability::import(&path, &[]) });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |ws, cx| match result {
                        Ok(done) => ws.show_toast(
                            format!("Imported {} memory files, {} project keys and {} words records. {} policy files wait in Settings for you to approve again.{}{}", done.memory_files, done.project_keys, done.words_records, done.set_aside, import_refused(done.refused), import_held(&done.held, &shown)),
                            cx,
                        ),
                        Err(err) => ws.show_toast(format!("Could not import: {err:#}"), cx),
                    });
                })
                .detach();
            });
        })
        .detach();
    }

    /// Settings › Manager › Your data: what uninstall would do, to confirm.
    pub(crate) fn data_uninstall_ask(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move { glasscode_core::portability::uninstall_plan() });
        cx.spawn(async move |this, cx| {
            let plan = task.await;
            let _ = this.update(cx, |ws, cx| {
                let blocked = plan.iter().any(|m| m.blocks.is_some());
                ws.modal = Some(crate::dialogs::Modal::ConfirmUninstall { lines: uninstall_lines(&plan), blocked });
                cx.notify();
            });
        })
        .detach();
    }

    /// The person confirmed: remove what Glasscode made.
    pub(crate) fn data_uninstall_now(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move {
            let plan = glasscode_core::portability::uninstall_plan();
            glasscode_core::portability::uninstall(&plan, false)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| match result {
                Ok(_) => ws.show_toast("Glasscode removed its folder. Quit Glasscode now.", cx),
                Err(err) => ws.show_toast(format!("Not uninstalled: {err:#}"), cx),
            });
        })
        .detach();
    }

    /// What moving out needs: each project's key and main folder, and the Claude accounts' folders.
    pub(crate) fn move_out_inputs(&self) -> (Vec<(glasscode_core::paths::ProjectKey, std::path::PathBuf)>, Vec<std::path::PathBuf>) {
        let projects = self.state.projects.iter().filter_map(|p| self.control.project_keys.get(&p.id).map(|key| (key.clone(), p.root.clone()))).collect();
        let claude = self.state.accounts.iter().filter(|a| a.agent == glasscode_core::model::AgentKind::Claude).map(|a| a.effective_dir()).collect();
        (projects, claude)
    }

    /// Settings › Manager › Your data: give Claude its notes back.
    pub(crate) fn data_move_out_now(&mut self, cx: &mut Context<Self>) {
        let (projects, claude) = self.move_out_inputs();
        let task = cx.background_spawn(async move { glasscode_core::portability::move_out(&projects, &claude, false) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| match result {
                Ok(done) => ws.show_toast(format!("Wrote {} notes into Claude's memory folders; {} were there already.", done.written.len(), done.same), cx),
                Err(err) => ws.show_toast(format!("Could not move out: {err:#}"), cx),
            });
        })
        .detach();
    }

    /// Settings › Manager › Your data: how often exports run by themselves (0: none).
    pub(crate) fn data_schedule_set(&mut self, every_days: u32, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move { glasscode_core::store::update::<glasscode_core::portability::ExportSchedule, _>(|s| { s.every_days = every_days; s.clone() }) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                match result {
                    Ok(schedule) => {
                        ws.show_toast(schedule.line(), cx);
                        ws.export_schedule = Some(schedule);
                    }
                    Err(err) => ws.show_toast(format!("Could not keep the schedule: {err}"), cx),
                }
                ws.data_export_due(cx);
            });
        })
        .detach();
    }

    /// At start, and each hour after (v2): run the scheduled export when it is due.
    pub(crate) fn data_export_schedule_start(&mut self, cx: &mut Context<Self>) {
        self.data_export_due(cx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(3600)).await;
                if this.update(cx, |ws, cx| ws.data_export_due(cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// The scheduled export, when it is due: in the background; a failure is said once.
    fn data_export_due(&mut self, cx: &mut Context<Self>) {
        let local_only = self.local_only_keys();
        let task = cx.background_spawn(async move {
            use glasscode_core::store::Store as _;
            let done = glasscode_core::portability::scheduled_export(glasscode_core::clock::now_ms(), &local_only);
            (done, glasscode_core::portability::ExportSchedule::load().ok())
        });
        cx.spawn(async move |this, cx| {
            let (done, schedule) = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.export_schedule = schedule;
                if let Err(err) = done {
                    ws.show_toast(format!("The scheduled export did not work: {err:#}"), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The projects that stay on this Mac (the gate's policy), as memory keys.
    pub(crate) fn local_only_keys(&self) -> Vec<glasscode_core::paths::ProjectKey> {
        self.control_policy_set().policy.projects.iter().filter(|(_, p)| p.local_only).map(|(k, _)| k.clone()).collect()
    }
}

/// Where an export goes when no file is named: a dated file in Downloads.
pub(crate) fn default_export_file() -> std::path::PathBuf {
    let today = glasscode_core::day::iso(glasscode_core::day::local((glasscode_core::clock::now_ms() / 1000) as i64));
    glasscode_core::paths::home().join("Downloads").join(format!("glasscode-export-{today}.tar"))
}

fn data_export(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    person_only(caller, "export your data")?;
    // A relative path is from where the command was typed, not the app's own folder (found in the
    // journey run: `--to export.tar` from a shell tab was written in the app's working folder).
    let to = op.text("to").map(|to| crate::commands::expand(to.trim(), caller.cwd.as_deref())).unwrap_or_else(default_export_file);
    let world = ws.world(caller);
    let mut include = Vec::new();
    for word in op.texts("include") {
        let id = ops::find_project(&world, &word).map_err(|e| error_reply(&e))?;
        include.extend(ws.control.project_keys.get(&id).cloned());
    }
    let local_only = ws.local_only_keys();
    memory_plan(Kind::Settings, format!("export your data to {}", home_relative(&to)), move || match glasscode_core::portability::export(&to, &local_only, &include) {
        Ok(done) => {
            let left = if done.left_out.is_empty() { String::new() } else { format!(" Left out, as they stay on this Mac: {}.", done.left_out.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")) };
            Reply::ok(format!("Exported {} memory files and {} other files to {}.{left}", done.memory_files, done.other_files, done.path.display()), json!({ "path": done.path, "memory_files": done.memory_files, "other_files": done.other_files }))
        }
        Err(err) => failed(format!("Could not export: {err:#}")),
    })
}

fn data_schedule(_: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    use glasscode_core::portability::ExportSchedule;
    use glasscode_core::store::Store as _;
    person_only(caller, "schedule exports")?;
    let every = match op.text("every").map(|w| w.trim().to_lowercase()) {
        None => None,
        Some(word) => Some(match word.as_str() {
            "off" | "none" | "never" | "0" => 0,
            "day" | "daily" | "1" => 1,
            "week" | "weekly" => 7,
            other => other.parse::<u32>().ok().filter(|n| *n <= 365).ok_or_else(|| usage(format!("“{other}” is no interval: say day, week, a number of days, or off.")))?,
        }),
    };
    // Kept and used by later exports: a relative folder is from where the command was typed, as
    // `data export --to` (#244), never the app's own working folder.
    let to = op.text("to").map(|t| crate::commands::expand(t.trim(), caller.cwd.as_deref()));
    let keep = op.number("keep").map(|n| n.clamp(1, 100) as u32);
    memory_plan(Kind::Settings, "schedule exports", move || {
        let changed = every.is_some() || to.is_some();
        let result = if changed {
            glasscode_core::store::update::<ExportSchedule, _>(|s| {
                if let Some(every) = every {
                    s.every_days = every;
                }
                if to.is_some() {
                    s.folder = to;
                }
                if let Some(keep) = keep {
                    s.keep = keep;
                }
                s.clone()
            })
        } else {
            ExportSchedule::load()
        };
        match result {
            Ok(schedule) => Reply::ok(schedule.line(), json!({ "every_days": schedule.every_days, "folder": schedule.folder(), "keep": schedule.keep(), "last_file": schedule.last_file, "last_error": schedule.last_error })),
            Err(err) => failed(format!("Could not keep the schedule: {err}")),
        }
    })
}

fn data_move_out(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    person_only(caller, "move your notes out")?;
    let dry = op.on("dry_run");
    let (projects, claude) = ws.move_out_inputs();
    memory_plan(Kind::Settings, if dry { "list what moving out would write" } else { "move your notes out to Claude" }, move || match glasscode_core::portability::move_out(&projects, &claude, dry) {
        Ok(done) => {
            let verb = if dry { "would write" } else { "wrote" };
            Reply::ok(
                format!("Moving out {verb} {} notes into Claude's memory folders; {} were there already.", done.written.len(), done.same),
                json!({ "written": done.written, "same": done.same, "dry_run": dry }),
            )
        }
        Err(err) => failed(format!("Could not move out: {err:#}")),
    })
}

/// What an import's reply says of the entries the scan left out.
fn import_refused(refused: usize) -> String {
    match refused {
        0 => String::new(),
        1 => " 1 entry was left out: memory's scan refused it (an instruction or a secret).".into(),
        n => format!(" {n} entries were left out: memory's scan refused them (instructions or secrets)."),
    }
}

/// What an import's reply says of the project keys it left unchanged: what the export would add
/// to each, and how to let it.
pub(crate) fn import_held(held: &[glasscode_core::portability::KeyAdditions], from: &std::path::Path) -> String {
    if held.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = held.iter().map(|add| add.line()).collect();
    format!(
        " It left {} project keys you have unchanged; the export would add {}. To add them to a key, run `glasscode data import {} --confirm <key>`.",
        held.len(),
        lines.join(" and "),
        from.display()
    )
}

fn data_import(_: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    person_only(caller, "import data")?;
    let from = crate::commands::expand(arg(op, "from").trim(), caller.cwd.as_deref());
    let mut confirm = Vec::new();
    for word in op.texts("confirm") {
        confirm.push(glasscode_core::paths::ProjectKey::parse(word.trim()).ok_or_else(|| usage(format!("“{word}” is not a project key (such as app-3f9a1c); `glasscode data import <file> --dry-run` lists them.")))?);
    }
    if op.on("dry_run") {
        return memory_plan(Kind::Read, format!("read {}", home_relative(&from)), move || match glasscode_core::portability::preview(&from) {
            Ok(seen) => {
                let keys = if seen.new_keys.is_empty() { "no new project keys".to_string() } else { format!("new project keys {}", seen.new_keys.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")) };
                let adds = if seen.additions.is_empty() {
                    " It adds nothing to the project keys you have.".to_string()
                } else {
                    format!(" To project keys you have it would add, only with --confirm <key>: {}.", seen.additions.iter().map(|a| a.line()).collect::<Vec<_>>().join("; "))
                };
                Reply::ok(
                    format!("This import would bring {} memory files and {keys}.{adds} Nothing was changed.", seen.memory_files),
                    json!({ "memory_files": seen.memory_files, "new_keys": seen.new_keys, "additions": seen.additions }),
                )
            }
            Err(err) => failed(format!("Could not read the export: {err:#}")),
        });
    }
    memory_plan(Kind::Settings, format!("import {}", home_relative(&from)), move || match glasscode_core::portability::import(&from, &confirm) {
        Ok(done) => Reply::ok(
            format!(
                "Imported {} memory files, {} project keys and {} words records. {} authority files are set aside in authority/imported for you to approve again.{}{}",
                done.memory_files,
                done.project_keys,
                done.words_records,
                done.set_aside,
                import_refused(done.refused),
                import_held(&done.held, &from)
            ),
            json!({ "memory_files": done.memory_files, "project_keys": done.project_keys, "words_records": done.words_records, "set_aside": done.set_aside, "refused": done.refused, "held": done.held }),
        ),
        Err(err) => failed(format!("Could not import: {err:#}")),
    })
}

fn data_forget(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    use glasscode_core::portability::Forget;
    person_only(caller, "forget data")?;
    // The card says what goes (found in the journey run: it said "forget data").
    let (what, named) = match (op.text("person"), op.text("project"), op.text("employer")) {
        (Some(name), None, None) => (Forget::Person(name.to_string()), format!("the person “{}”", name.trim())),
        (None, Some(word), None) => {
            let id = ops::find_project(&ws.world(caller), word).map_err(|e| error_reply(&e))?;
            let name = ws.state.projects.iter().find(|p| p.id == id).map(|p| p.name.clone()).unwrap_or_else(|| word.to_string());
            (Forget::Project(ws.control.project_keys.get(&id).cloned().ok_or_else(|| failed("That project has no memory key, so memory holds nothing of it."))?), format!("the project {name}"))
        }
        (None, None, Some(word)) => (Forget::Employer(glasscode_core::memstore::Employer::parse(&word.to_lowercase()).ok_or_else(|| usage(format!("“{word}” is no employer's folder name.")))?), format!("the employer {word}")),
        _ => return Err(usage("data forget --person \"<name>\" | --project <name> | --employer <name> (one of them)")),
    };
    memory_plan(Kind::Settings, format!("forget {named} everywhere Glasscode keeps it, memory's history included"), move || match glasscode_core::portability::forget(&what) {
        Ok(done) => Reply::ok(
            format!(
                "Forgotten: {}{} words records, {} journal lines, {} effect ledger lines and {} of moving in's questions and studies changed. Memory's history before this point is gone.",
                if done.projects.is_empty() { String::new() } else { format!("{}; ", done.projects.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")) },
                done.words_records,
                done.journal_lines,
                done.effect_lines,
                done.movein
            ),
            json!({ "memory": done.memory, "words_records": done.words_records, "journal_lines": done.journal_lines, "effect_lines": done.effect_lines, "movein": done.movein }),
        ),
        Err(err) => failed(format!("Could not forget: {err:#}")),
    })
}

/// The lines of what uninstall would do, for the person.
pub(crate) fn uninstall_lines(plan: &[glasscode_core::portability::Made]) -> Vec<String> {
    plan.iter()
        .map(|m| {
            let verb = if m.removed { "removes" } else { "leaves, for you" };
            let block = m.blocks.as_ref().map(|b| format!(" (stops it: {b})")).unwrap_or_default();
            format!("{verb}: {} — {}{block}", m.path.display(), m.what)
        })
        .collect()
}

fn data_uninstall(_: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    person_only(caller, "uninstall Glasscode")?;
    let dry = op.on("dry_run");
    let from_app = matches!(caller.channel, glasscode_core::caller::Caller::Ui);
    memory_plan(Kind::Settings, "uninstall Glasscode", move || {
        let plan = glasscode_core::portability::uninstall_plan();
        let lines = uninstall_lines(&plan);
        if dry || !from_app {
            let tail = if dry { "" } else { "\nTo remove them, confirm in Glasscode: Settings › Manager › Your data." };
            return Reply::ok(format!("Uninstall {}:\n{}{tail}", if dry { "would do this" } else { "does this" }, lines.join("\n")), json!({ "plan": lines }));
        }
        match glasscode_core::portability::uninstall(&plan, false) {
            Ok(n) => Reply::ok(format!("Removed {n} of Glasscode's folders. Quit Glasscode now; what it left for you is listed above."), json!({ "removed": n })),
            Err(err) => failed(format!("Not uninstalled: {err:#}")),
        }
    })
}

fn app_whoami(ws: &mut Workspace, caller: &Caller, _: &Op) -> Planned {
    answer(ws.whoami_reply(caller))
}

fn app_open(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let what_word = arg(op, "place").to_lowercase();
    use crate::shell::View;
    // The words of the command stay as they were: "board" is the Today board, "themes" the
    // Appearance section of Settings.
    let view = match what_word.as_str() {
        "today" | "board" => Some((View::Today, false)),
        "tickets" | "linear" => Some((View::Board, false)),
        "sessions" => Some((View::Sessions, false)),
        "usage" => Some((View::Usage, false)),
        "settings" => Some((View::Settings, false)),
        "themes" => Some((View::Settings, true)),
        _ => None,
    };
    match view {
        Some((view, themes)) => plan(Kind::Show, what(op, &Names::default(), day::today()), move |ws, r, window, cx| {
            match view {
                View::Settings if themes => ws.open_appearance(cx),
                View::Board => ws.show_tickets(cx),
                View::Sessions => ws.show_sessions(window, cx),
                View::Settings => ws.open_settings(crate::settings_ui::Section::General, window, cx),
                View::Today => ws.show_today(cx),
                other => ws.show_view(other, cx),
            }
            ws.reply(r, Reply::ok(format!("Showing {what_word}."), Value::Null));
        }),
        None => {
            let id = ws.find_session(&what_word, caller)?;
            let title = ws.state.session(&id).map(|s| ws.session_title(s)).unwrap_or_default();
            plan(Kind::Show, what(op, &Names { title: title.clone(), ..Names::default() }, day::today()), move |ws, r, window, cx| {
                ws.open_session(&id, window, cx);
                ws.reply(r, Reply::ok(format!("Showing “{title}”."), json!({ "id": id })));
            })
        }
    }
}

fn app_notify(_: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let message = arg(op, "message").to_string();
    let title = op.text("title").map(str::to_string).unwrap_or_else(|| format!("{} says", caller.who));
    let session = caller.session.clone();
    let agent = caller.agent.unwrap_or(AgentKind::Claude);
    plan(Kind::Board, what(op, &Names::default(), day::today()), move |ws, r, _, cx| {
        ws.notify_session(session.as_deref().unwrap_or("glasscode-notify"), agent, title, message.clone(), true, cx);
        ws.reply(r, Reply::ok(format!("Notified the person: “{message}”."), Value::Null));
    })
}

fn app_log(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let limit = op.number("limit").unwrap_or(20) as usize;
    // The lines Today › Activity has (the journal's op events; agent-log.jsonl is no longer
    // kept, M56 row 7). With the v2 engine `log` is `journal.query`.
    let entries: Vec<glasscode_core::control::LogEntry> = ws.control.log.iter().take(limit).cloned().collect();
    {
        let text = if entries.is_empty() {
            "Nothing yet.".to_string()
        } else {
            entries
                .iter()
                .map(|e| {
                    let at = day::local(e.at as i64);
                    let when = if at == day::today() { clock(e.at) } else { format!("{} {}", day::short(at), clock(e.at)) };
                    let from = e.session_title.as_ref().map(|t| format!(" in “{t}”")).unwrap_or_default();
                    format!("{when}  {}{from}: {}", e.who, e.what)
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        answer(Reply::ok(text, serde_json::to_value(&entries).unwrap_or_default()))
    }
}

fn app_approvals(ws: &mut Workspace, caller: &Caller, _: &Op) -> Planned {
    let list: Vec<&crate::control_ui::Approval> = ws.control.approvals.iter().filter(|a| crate::control_ui::sees_approval(&caller.channel, a)).collect();
    let text = if list.is_empty() {
        "Nothing waits for the person.".to_string()
    } else {
        list.iter().map(|a| format!("#{}  {}  ({})", a.id, a.summary(), a.command)).collect::<Vec<_>>().join("\n")
    };
    answer(Reply::ok(text, ws.control_approvals_json(&caller.channel)))
}

/// What waits for the person (v2): each pending action with every argument, who asks and why, and
/// its expiry.
fn approvals_list(ws: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    let now = glasscode_core::clock::now_ms();
    let pending: Vec<&glasscode_core::gate::PendingAction> = ws.control.approvals.iter().filter_map(|a| a.pending.as_ref()).collect();
    let text = if pending.is_empty() {
        "Nothing waits for you.".to_string()
    } else {
        pending
            .iter()
            .map(|p| format!("#{}  {} to {}\n    why: {}\n    waits {} min more\n    {}", p.number, glasscode_core::caller::who_does(&p.who, "ask"), p.what, p.reason, p.expires_ms.saturating_sub(now) / 60_000, p.payload().replace('\n', "\n    ")))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let data = json!(pending.iter().map(|p| json!({ "number": p.number, "id": p.id, "op": p.op, "who": p.who, "what": p.what, "why": p.reason, "targets": p.targets, "expires_ms": p.expires_ms })).collect::<Vec<_>>());
    answer(Reply::ok(text, data))
}

/// Approve or deny what waits (v2): the card's own buttons run this op, so the person's answer is
/// journalled like any op.
fn approvals_decide(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let word = arg(op, "id").trim_start_matches('#').to_string();
    let allow = match arg(op, "answer").to_lowercase().as_str() {
        "allow" | "approve" | "yes" => true,
        "deny" | "no" => false,
        other => return Err(usage(format!("Say allow or deny, not “{other}”."))),
    };
    let found = ws.control.approvals.iter().find(|a| a.pending.as_ref().is_some_and(|p| p.id == word) || a.id.to_string() == word);
    let Some(approval) = found else { return Err(failed(format!("Nothing waits as {word}."))) };
    let (id, what) = (approval.id, approval.what.clone());
    let verb = if allow { "approve" } else { "deny" };
    plan(Kind::Settings, format!("{verb} “{what}”"), move |ws, r, window, cx| {
        ws.control_decide(id, allow, false, window, cx);
        ws.reply(r, Reply::ok(format!("{} #{id}: {what}.", if allow { "Approved" } else { "Denied" }), json!({ "number": id, "allow": allow })));
    })
}

/// A browser tool call the gate let through (M23, D28): the browser's MCP server asked before it
/// acts, and the answer is only that it may. Reading is a read; acting and script are Board's v1
/// kind, so a session the person started works its tab as before, and a signed-in site asked.
fn browser_gate(_: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let site = arg(op, "url").to_string();
    plan(Kind::Board, format!("act in the browser on {site}"), move |ws, r, _, _| ws.reply(r, Reply::ok(format!("May act on {site}."), json!({ "allowed": true }))))
}

/// Reading a page in a session's tab: a read the gate let through.
fn browser_read(_: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    answer(Reply::ok(format!("May read {}.", arg(op, "url")), json!({ "allowed": true })))
}

fn app_wait(_: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let id = op.number("id").unwrap_or_default();
    plan(Kind::Read, "read", move |ws, r, _, _| ws.control_wait(id, r))
}

// ----- the journal (M14) -----------------------------------------------------------------------

/// `glasscode events`: the newest events, within the caller's scope; with `--follow` (the command
/// asks again every second, with `--after` the cursor of the last reply) only the ones after it.
fn journal_events(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    if !ws.journal.on {
        return Err(failed("The journal is off here (it runs with the v2 engine)."));
    }
    let filter = crate::journal_ui::filter_of(ws, op)?;
    let scope = crate::journal_ui::scope_of(ws, caller);
    let limit = op.number("limit").unwrap_or(50) as usize;
    let after = match op.text("after") {
        Some(text) => Some(glasscode_core::journal::Cursor::parse(text).ok_or_else(|| usage(format!("“{text}” is not a cursor of the journal.")))?),
        None => None,
    };
    let follow = op.on("follow");
    plan(Kind::Read, "read", move |_ws, r, _, cx| {
        let task = cx.background_spawn(async move { glasscode_core::ops::core_reads::journal_events(&filter, &scope, limit, after.as_ref(), follow) });
        cx.spawn(async move |this, cx| {
            let reply = task.await;
            let _ = this.update(cx, |ws, _| ws.reply(r, reply));
        })
        .detach();
    })
}

/// `journal query`, and `log` with the v2 engine: the events that match, as rows a program
/// reads; for `log`, the actions, with the fields `log --json` always had.
fn journal_query(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    if !ws.journal.on {
        return Err(failed("The journal is off here (it runs with the v2 engine)."));
    }
    let filter = crate::journal_ui::filter_of(ws, op)?;
    let scope = crate::journal_ui::scope_of(ws, caller);
    let limit = op.number("limit").unwrap_or(50) as usize;
    // `--actions` (land-21): from the effect ledger, kept a year.
    let actions = op.on("actions").then(|| op.number("pr"));
    plan(Kind::Read, "read", move |_ws, r, _, cx| {
        let task = cx.background_spawn(async move {
            use glasscode_core::ops::core_reads;
            match actions {
                Some(pr) => match glasscode_core::effects::recent(core_reads::ACTIONS_DAYS) {
                    Ok(records) => core_reads::actions_query(&records, &scope, filter.project.as_ref(), pr, filter.since_ms, limit),
                    Err(err) => Reply::fail(code::FAILED, format!("The record of actions could not be read: {err}")),
                },
                None => core_reads::journal_query(&filter, &scope, limit),
            }
        });
        cx.spawn(async move |this, cx| {
            let reply = task.await;
            let _ = this.update(cx, |ws, _| ws.reply(r, reply));
        })
        .detach();
    })
}

// ----- playbooks (M40) ---------------------------------------------------------------------------

/// What a playbook is to this machine now, in a word or two.
fn playbook_state(p: &glasscode_core::playbooks::Playbook, all_paused: bool) -> String {
    if let Some(by) = &p.proposed_by {
        return format!("proposed by {by}");
    }
    if let Some(paused) = &p.paused {
        return format!("paused by {}: {}", paused.by, paused.why);
    }
    if all_paused && !p.builtin {
        return "paused with every playbook".to_string();
    }
    match (p.enabled, p.builtin || p.approved()) {
        (false, _) => "off".to_string(),
        (true, false) => "not approved on this machine".to_string(),
        (true, true) => "runs".to_string(),
    }
}

/// `playbook check`: every playbook as this machine reads it, the built-ins first, and why the
/// ones that do not run do not.
fn playbook_check(_: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    plan(Kind::Read, "read", move |_ws, r, _, cx| {
        let task = cx.background_spawn(async move { glasscode_core::playbooks::load(&glasscode_core::playbooks::model::journal_schema()) });
        cx.spawn(async move |this, cx| {
            let book = task.await;
            let _ = this.update(cx, |ws, _| {
                let reply = match book {
                    Err(err) => failed(format!("The playbooks could not be read ({err}): none of the person's runs until it is repaired; the built-ins go on.")),
                    Ok(book) => {
                        let all_paused = book.all_paused.is_some();
                        let mut lines: Vec<String> = glasscode_core::playbooks::builtins::BUILTINS.iter().map(|b| format!("{:<16} {} (built in{})", b.id, b.name, if b.needs_manager { ", with the manager" } else { "" })).collect();
                        let mut rows: Vec<Value> = glasscode_core::playbooks::builtins::BUILTINS.iter().map(|b| json!({ "id": b.id, "name": b.name, "builtin": true, "needs_manager": b.needs_manager })).collect();
                        for p in book.playbooks.iter().filter(|p| !p.builtin) {
                            let state = playbook_state(p, all_paused);
                            lines.push(format!("{:<16} {}: {state}", p.id, p.name));
                            rows.push(json!({ "id": p.id, "name": p.name, "state": state, "autonomy": p.autonomy, "on": p.trigger.filter().map(|f| f.kind.clone()) }));
                        }
                        lines.extend(book.alerts.iter().cloned());
                        Reply::ok(lines.join("\n"), json!({ "playbooks": rows, "alerts": book.alerts }))
                    }
                };
                ws.reply(r, reply);
            });
        })
        .detach();
    })
}

/// `playbook test <id> [--since 7d]`: the playbook run against the journal's past events as if it
/// were on and approved, acting on nothing: what it would have done, and what held it back.
fn playbook_test(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    if !ws.journal.on {
        return Err(failed("The journal is off here (it runs with the v2 engine)."));
    }
    let id = arg(op, "playbook").to_string();
    let mut filter = crate::journal_ui::filter_of(ws, op)?;
    let now = glasscode_core::clock::now_ms();
    let week = now.saturating_sub(glasscode_core::playbooks::simulate::DAYS * 86_400_000);
    filter.since_ms = Some(filter.since_ms.unwrap_or(week).max(week));
    let scope = crate::journal_ui::scope_of(ws, caller);
    let policy = ws.control.authority.policy.clone();
    let confirmed: Vec<String> = ws.state.manager.people.iter().map(|p| p.github.clone()).filter(|login| !login.is_empty()).collect();
    plan(Kind::Read, "read", move |_ws, r, _, cx| {
        let task = cx.background_spawn(async move {
            use glasscode_core::playbooks::{self, engine::Facts, limits::LedgerCounts, model, simulate};
            let book = playbooks::load(&model::journal_schema()).map_err(|err| format!("The playbooks could not be read ({err})."))?;
            let mut playbook = book.playbooks.into_iter().find(|p| p.id == id && !p.builtin).ok_or_else(|| format!("No playbook “{id}” (built-ins are code; `playbook check` lists them)."))?;
            // One that does not load is off for a reason: that is the answer, not a run.
            if let Err(problems) = model::validate(&playbook, &model::journal_schema()) {
                return Err(format!("“{}” does not load: {}.", playbook.name, problems.join("; ")));
            }
            // As if on and approved here: the question is what it would do.
            playbook.enabled = true;
            playbook.paused = None;
            playbook.approved_hash = Some(playbook.hash());
            let events = glasscode_core::journal::query(&filter, &scope, usize::MAX);
            let counts = glasscode_core::effects::load().map(|ledger| LedgerCounts::of(&ledger)).unwrap_or_default();
            let level = |key: Option<&glasscode_core::paths::ProjectKey>| key.map_or(policy.default.level, |key| policy.of(key).level);
            let facts = |now_ms: u64| Facts { level: &level, me: &[], confirmed: &confirmed, now_ms };
            let timed: Vec<(&glasscode_core::journal::Event, u64)> = events.iter().map(|event| (event, event.at.wall_ms)).collect();
            Ok::<_, String>(simulate::dry_run(std::slice::from_ref(&playbook), timed, &model::journal_schema(), &facts, counts))
        });
        cx.spawn(async move |this, cx| {
            let report = task.await;
            let _ = this.update(cx, |ws, _| {
                let reply = match report {
                    Err(text) => failed(text),
                    Ok(report) => {
                        let mut lines = vec![format!("{} events read; it would have fired {} times, and held back {}.", report.events, report.fired.len(), report.held.len())];
                        lines.extend(report.fired.iter().map(|f| format!("  {} on {}: {} {} ({:?})", f.event, f.target, f.op, f.args, f.autonomy)));
                        lines.extend(report.held.iter().map(|(event, held)| format!("  held on {event}: {}", held.why.words())));
                        let fired: Vec<Value> = report.fired.iter().map(|f| json!({ "event": f.event, "op": f.op, "args": f.args, "target": f.target, "autonomy": f.autonomy, "subject": f.subject })).collect();
                        let held: Vec<Value> = report.held.iter().map(|(event, held)| json!({ "event": event, "why": held.why.words() })).collect();
                        Reply::ok(lines.join("\n"), json!({ "events": report.events, "fired": fired, "held": held }))
                    }
                };
                ws.reply(r, reply);
            });
        })
        .detach();
    })
}

// ----- the manager -----------------------------------------------------------------------------

fn manager_status(ws: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    answer(ws.manager_status_reply())
}

fn manager_ask(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let settings = ws.manager_on()?;
    let name = settings.name.clone();
    if caller.is_manager() {
        return Err(denied(format!("{name} cannot ask itself. Answer from what you know, or look with the glasscode command.")));
    }
    let text = arg(op, "question").trim().to_string();
    let voice = op.on("voice");
    // Whose words the question is comes from who sent the command, never a composer's (D1): an
    // agent's is a peer's, a terminal's is outside (ux-m1). Recorded when it is asked, not when it
    // is planned (a dry run plans and asks nothing).
    let (origin, asked, session) = (glasscode_core::words::command_origin(&caller.channel), text.clone(), caller.session.clone());
    // The manager takes a question as the person's. One from a session's agent says so.
    let text = if caller.is_agent() { format!("(Asked by {}, an agent, not the person.) {text}", caller.describe()) } else { text };
    // Asking changes nothing by itself (what the manager then does passes Settings › Automation as
    // its own), so the person's voice and phone can ask without an OK.
    // With the v2 engine the conversation keeps whose question it is (M52.1): the person's, from
    // their own shell tab, or another's, named.
    let asker = (!caller.channel.is_person()).then(|| caller.describe());
    plan(Kind::Read, format!("ask {name}: “{}”", clip(&text, 80)), move |ws, r, _, cx| {
        let words = glasscode_core::words::record(origin, &asked, session.as_deref(), None);
        ws.manager_ask_as(
            text,
            voice,
            asker.is_none().then_some(words),
            asker,
            Some(Box::new(move |ws, say, _cx| {
                let reply = match say {
                    Ok(say) => Reply::ok(say.clone(), json!({ "say": say })),
                    Err(why) => failed(why),
                };
                ws.reply(r, reply);
            })),
            cx,
        );
    })
}

pub(crate) fn manager_say(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let settings = ws.manager_on()?;
    let text = arg(op, "text").trim().to_string();
    plan(Kind::Show, what(op, &Names { other: settings.name, ..Names::default() }, day::today()), move |ws, r, _, cx| {
        ws.manager_say(&text, None, cx);
        ws.reply(r, Reply::ok(format!("{} said it.", ws.state.manager.name), json!({ "say": text })));
    })
}

fn manager_brief(ws: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    let settings = ws.manager_on()?;
    // v2: from v2's own stores (M56.2); v1: data.json.
    let mut made = brief::build_from(&settings, &ws.v2_brief_data(), &ws.v2_snapshot(), now());
    // v2 (M42): what was told and still waits on the person, with its age, from code.
    {
        let waiting = brief::waiting_lines(&crate::notify_ui::still_waiting(glasscode_core::clock::now_ms()));
        if !waiting.is_empty() && !made.text.trim().is_empty() {
            made.text = format!("{} {}", made.text.trim(), waiting.iter().map(|l| format!("{}.", l.text)).collect::<Vec<_>>().join(" "));
        }
        made.lines.extend(waiting);
    }
    let text = match (made.text.trim().is_empty(), made.lines.is_empty()) {
        (false, _) => made.text.clone(),
        (true, false) => made.lines.iter().map(|l| format!("- {}", l.text)).collect::<Vec<_>>().join("\n"),
        (true, true) => "Nothing to report today.".to_string(),
    };
    answer(Reply::ok(text, to_json(&made)))
}

pub(crate) fn manager_tour(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    ws.manager_on()?;
    plan(Kind::Show, what(op, &Names::default(), day::today()), move |ws, r, _, cx| {
        let stops = brief::tour_from(&ws.state.manager, &ws.v2_brief_data(), &ws.v2_snapshot());
        if stops.is_empty() {
            ws.reply(r, Reply::ok("Nothing to walk through right now.", json!({ "stops": [] })));
            return;
        }
        let (count, data) = (stops.len(), to_json(&stops));
        ws.manager_start_tour(stops, cx);
        ws.reply(r, Reply::ok(format!("Started the walk through today's update: {}.", plural(count, "stop", "stops")), json!({ "stops": data })));
    })
}

fn manager_remember(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    // v2: the words of `manager remember` propose to memory (D1: no one's command makes a fact).
    let ask = memory_ask(ws, caller, op)?;
    let text = arg(op, "fact").trim().to_string();
    memory_plan(Kind::Board, format!("propose to memory: {}", short(&text)), move || {
        let run = || -> Result<Reply, Reply> {
            let memory = Memory::open();
            let key = ask.key()?;
            let file = key.map_or(MemFile::Profile, MemFile::Decisions);
            let source = match &ask.who {
                Who::Session(id) => memstore::Source::Session(id.clone()),
                Who::Manager => memstore::Source::Session("manager".into()),
                Who::You => memstore::Source::PersonEdit,
            };
            let (id, _) = memory.propose(&ask.who, &file, None, &text, source).map_err(memory_reply)?;
            Ok(Reply::ok(format!("Proposed for memory; it waits for the person's tick. Proposal {id}."), json!({ "id": id, "text": text, "proposed": true })))
        };
        run().unwrap_or_else(|reply| reply)
    })
}

fn manager_forget(_: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    person_only(caller, "forget what memory keeps")?;
    let which = arg(op, "which").trim().to_string();
    memory_plan(Kind::Board, format!("forget {which}"), move || Memory::open().forget(&[which.as_str()], false).map(|_| Reply::ok(format!("Forgot {which}."), json!({ "id": which }))).unwrap_or_else(memory_reply))
}

fn manager_memory(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    // v2: what memory holds, by search (the v1 notes are migrated into it, M20.3).
    let ask = memory_ask(ws, caller, op)?;
    let query = arg(op, "query").trim().to_string();
    let limit = op.number("limit").unwrap_or(20).clamp(1, 50) as usize;
    memory_plan(Kind::Read, "search memory", move || {
        let run = || -> Result<Reply, Reply> {
            let snap = Memory::open().snapshot().map_err(memory_reply)?;
            let key = if ask.everywhere { None } else { ask.key()? };
            let mut scope = memstore::search::scope_for(&snap, key.as_ref(), None).map_err(memory_reply)?;
            scope.hidden = ask.hidden.clone();
            let hits = memstore::search::search(&snap, if query.is_empty() { "*" } else { &query }, &scope, limit).map_err(memory_reply)?;
            Ok(Reply::ok(memstore::search::as_data(&hits, snap.commit()), json!({ "commit": snap.commit() })))
        };
        run().unwrap_or_else(|reply| reply)
    })
}

/// `manager show <session> […]` and `manager show --view <screen>`: take the person there.
pub(crate) fn manager_show(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    const USAGE: &str = "manager show <session> [--tab pr|changes|files] [--section <name>] [--say \"<text>\"]  |  manager show --view manager|sessions|today|tickets|usage|settings [--say \"<text>\"]";
    let settings = ws.manager_on()?;
    let word = arg(op, "session").trim().to_string();
    let say = op.text("say").map(str::to_string);
    let section = match op.text("section").map(str::to_lowercase) {
        Some(s) if SECTIONS.contains(&s.as_str()) => Some(s),
        Some(s) => return Err(usage(format!("“{s}” is not a part of the panel: {}.", SECTIONS.join(", ")))),
        None => None,
    };
    let tab = match op.text("tab") {
        Some(t) => Some(tab_of(t)?),
        None => section.as_deref().and_then(tab_for),
    };
    let (show, what) = match (op.text("view"), word.is_empty()) {
        (Some(view), true) => {
            let view = view.to_lowercase();
            if !VIEWS.contains(&view.as_str()) {
                return Err(usage(format!("“{view}” is not a screen: {}.", VIEWS.join(", "))));
            }
            if tab.is_some() || section.is_some() {
                return Err(usage("--tab and --section point into a session's panel: name a session, not --view."));
            }
            let what = format!("show you {}", if view == "manager" { settings.name.clone() } else { crate::control_ui::capitalize(&view) });
            (Show { session: None, view: Some(view), tab: None, section: None }, what)
        }
        (None, false) => {
            let (id, title) = ws.manager_find_session(&word, caller, &settings)?;
            (Show { session: Some(id), view: None, tab, section }, format!("show the session “{}”", clip(&title, 60)))
        }
        _ => return Err(usage(USAGE)),
    };
    plan(Kind::Show, what.clone(), move |ws, r, _, cx| {
        let mut actions = vec![Action::Show { show: show.clone() }];
        if let Some(text) = say.clone().filter(|t| !t.trim().is_empty()) {
            actions.push(Action::Say { text, show: Some(show.clone()) });
        }
        ws.v2_apply(actions, cx);
        let text = crate::control_ui::capitalize(&what.replacen("show", "showing", 1));
        ws.reply(r, Reply::ok(format!("{text}."), json!({ "show": to_json(&show), "say": say })));
    })
}

pub(crate) fn manager_scene(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let settings = ws.manager_on()?;
    // A scene in a file is read, and read again, off the main thread: first by the plan's check,
    // so that a file that is not there or not a scene goes straight back, before anyone is asked.
    if let Some(path) = op.text("file") {
        let path = expand(path, caller.cwd.as_deref());
        let shown = path.display().to_string();
        let check_path = path.clone();
        let planned = plan(Kind::Show, format!("show the picture in {shown}, drawn by {}", settings.name), move |_, r, _, cx| {
            let task = cx.background_spawn(async move { scene_file(&path) });
            cx.spawn(async move |this, cx| {
                let scene = task.await;
                let _ = this.update(cx, |ws, cx| match scene {
                    Ok(scene) => draw_scene(ws, r, scene, cx),
                    Err(reply) => ws.reply(r, reply),
                });
            })
            .detach();
        });
        return checked(planned, move || scene_file(&check_path).map(|_| ()));
    }
    let text = arg(op, "json").to_string();
    if text.trim().is_empty() {
        return Err(usage("manager scene '<json>'  (or --file <path>)"));
    }
    // Read here, before anyone is asked: a mistake in the JSON goes straight back.
    let scene = scene::parse(&text).map_err(|e| usage(format!("That scene cannot be drawn: {e:#}")))?;
    let title = if scene.title.trim().is_empty() { "a picture".to_string() } else { format!("“{}”", clip(&scene.title, 60)) };
    plan(Kind::Show, format!("show {title}, drawn by {}", settings.name), move |ws, r, _, cx| draw_scene(ws, r, scene, cx))
}

/// A scene from a file: at most 512 KB, and one that can be drawn.
fn scene_file(path: &std::path::Path) -> Result<glasscode_core::manager::Scene, Reply> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > 512 * 1024) {
        return Err(failed(format!("{} is too large for a scene.", path.display())));
    }
    let text = std::fs::read_to_string(path).map_err(|e| failed(format!("Could not read {}: {e}", path.display())))?;
    if text.trim().is_empty() {
        return Err(usage("manager scene '<json>'  (or --file <path>)"));
    }
    scene::parse(&text).map_err(|e| usage(format!("That scene cannot be drawn: {e:#}")))
}

fn draw_scene(ws: &mut Workspace, r: Responder, scene: glasscode_core::manager::Scene, cx: &mut Context<Workspace>) {
    let title = if scene.title.trim().is_empty() { "a picture".to_string() } else { format!("“{}”", clip(&scene.title, 60)) };
    let (steps, data) = (scene.steps.len(), to_json(&scene));
    ws.v2_apply(vec![Action::Scene { scene }], cx);
    ws.reply(r, Reply::ok(format!("Drawing {title} ({}).", plural(steps, "step", "steps")), data));
}

fn manager_voice_token(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let settings = ws.manager_on()?;
    // A token lets a device listen and speak in the manager's room as the person: for the person
    // and their own devices (the voice server asks for the glasses and the phone), never for an
    // agent in a session or the manager's own.
    if caller.is_agent() {
        return Err(denied("A voice token is for the person's own devices, not for an agent."));
    }
    let url = settings.voice.livekit_url.trim().to_string();
    if url.is_empty() {
        return Err(failed("Voice is not set up: there is no LiveKit server in Settings › Manager."));
    }
    let room = voice::room(&settings);
    let identity = op.text("identity").map(str::to_string).unwrap_or_else(|| format!("cli-{}", short(&new_id())));
    plan(Kind::Read, "read", move |_ws, r, _, cx| {
        // The Keychain may take a moment: not on the main thread.
        let (room_for, identity_for) = (room.clone(), identity.clone());
        let task = cx.background_spawn(async move {
            let (key, secret) = voice::keys().ok_or_else(|| "Voice is not set up: the LiveKit key and secret are missing in Settings › Manager.".to_string())?;
            voice::token(&key, &secret, &identity_for, &room_for, TOKEN_SECS).map_err(|e| format!("Could not make a voice token: {e:#}"))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, _| {
                let reply = match result {
                    Ok(token) => Reply::ok(format!("Room {room} at {url} as {identity}, good for an hour:\n{token}"), json!({ "url": url, "token": token, "room": room, "identity": identity })),
                    Err(why) => failed(why),
                };
                ws.reply(r, reply);
            });
        })
        .detach();
    })
}

/// At v2 the follow-ups are the work ledger's: while it is not running here (the journal is off,
/// or its file needs repair), nothing about them can be kept.
const NO_LEDGER: &str = "The work ledger is not running here (the journal is off, or manager/work.json needs repair), so nothing was changed.";
const NO_LEDGER_LIST: &str = "The work ledger is not running here (the journal is off, or manager/work.json needs repair), so the follow-ups cannot be read.";

fn follow_up_list(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    ws.manager_on()?;
    // With the engine at v2, the work ledger's (M56.2).
    if ws.work_ledger().is_none() {
        return Err(failed(NO_LEDGER_LIST));
    }
    let all = ws.follow_ups();
    let list: Vec<&FollowUp> = if op.on("all") { all.iter().collect() } else { followups::open(all) };
    let text = if list.is_empty() { "No follow-ups are open.".to_string() } else { list.iter().map(|f| follow_up_line(f)).collect::<Vec<_>>().join("\n") };
    answer(Reply::ok(text, Value::Array(list.iter().map(|f| to_json(*f)).collect())))
}

fn follow_up_add(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let settings = ws.manager_on()?;
    let what_text = arg(op, "what").trim().to_string();
    let person = arg(op, "person").to_string();
    let from = source_of(caller);
    plan(Kind::Board, what(op, &Names { other: settings.name, ..Names::default() }, day::today()), move |ws, r, _, cx| {
        let Some(id) = ws.follow_ups_edit(|list| followups::add_custom(list, &what_text, &person, now())) else {
            return ws.reply(r, failed(NO_LEDGER));
        };
        cx.notify();
        ws.reply(r, Reply::ok(format!("{} keeps an eye on it: {what_text}", ws.state.manager.name), json!({ "id": id, "what": what_text, "person": person, "from": from })));
    })
}

fn follow_up_done(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    follow_up_close(ws, op, false)
}

fn follow_up_drop(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    follow_up_close(ws, op, true)
}

fn follow_up_close(ws: &mut Workspace, op: &Op, dropped: bool) -> Planned {
    ws.manager_on()?;
    let which = arg(op, "which").trim().to_string();
    plan(Kind::Board, what(op, &Names::default(), day::today()), move |ws, r, _, cx| {
        let Some(closed) = ws.follow_ups_edit(|list| followups::close(list, &which, dropped, now())) else {
            return ws.reply(r, failed(NO_LEDGER));
        };
        let reply = match closed {
            Some(f) => {
                cx.notify();
                let what = if f.number > 0 {
                    format!("{}#{} {}", f.repo, f.number, f.title)
                } else if f.title.is_empty() {
                    f.note.clone()
                } else {
                    f.title.clone()
                };
                Reply::ok(format!("{}: {}", if dropped { "Dropped" } else { "Done" }, clip(&what, 120)), to_json(&f))
            }
            None => failed(format!("No open follow-up matches “{which}”. `glasscode manager follow-up list` shows them.")),
        };
        ws.reply(r, reply);
    })
}

fn message_list(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    ws.manager_on()?;
    let all = op.on("all");
    // Its own outbox (M56.2), empty until it is read.
    let none = Vec::new();
    let messages = ws.round.outbox.as_ref().map_or(&none, |outbox| &outbox.messages);
    let list: Vec<&OutMessage> = messages.iter().filter(|m| all || !matches!(m.state, MsgState::Sent | MsgState::Rejected)).collect();
    let text = if list.is_empty() { "No messages wait.".to_string() } else { list.iter().map(|m| message_line(m)).collect::<Vec<_>>().join("\n") };
    answer(Reply::ok(text, Value::Array(list.iter().map(|m| to_json(*m)).collect())))
}

/// Drafting is all an agent can do: whether a message goes is the person's (or their send
/// setting's) to decide.
fn message_draft(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let settings = ws.manager_on()?;
    let body = arg(op, "text").trim().to_string();
    let to = arg(op, "to");
    let Some(person) = settings.person(to) else {
        let known: Vec<&str> = settings.people.iter().map(|p| p.name.as_str()).collect();
        return Err(failed(format!("{to} is not one of the teammates in Settings › Manager{}.", if known.is_empty() { " (there are none yet)".to_string() } else { format!(": {}", known.join(", ")) })));
    };
    let to = person.name.clone();
    let why = arg(op, "why").to_string();
    let about = op.text("about").map(str::to_string);
    plan(Kind::Board, format!("draft a message to {to}: “{}”", clip(&body, 80)), move |ws, r, _, cx| {
        let settings = ws.state.manager.clone();
        let message = {
            match ws.v2_draft_message(&to, &body, &why, about.as_deref()) {
                Some(message) => message,
                None => return ws.reply(r, failed("The manager's outbox is not read yet (or could not be): nothing was drafted.")),
            }
        };
        cx.notify();
        let text = match (message.state, message.tier) {
            (_, Tier::Never) => format!("Kept as a draft for {to}, but {} never sends a message like this one. The person has to write it themselves.", settings.name),
            (MsgState::Approved | MsgState::Sent, _) => format!("Drafted for {to}. It is a routine ping, so it goes out by itself."),
            _ => format!("Drafted for {to}. It waits for the person's OK in Glasscode ({}).", short(&message.id)),
        };
        ws.reply(r, Reply::ok(text, to_json(&message)));
    })
}

fn message_approve(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    message_decide(ws, caller, op, true)
}

fn message_reject(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    message_decide(ws, caller, op, false)
}

fn message_decide(ws: &mut Workspace, caller: &Caller, op: &Op, allow: bool) -> Planned {
    let settings = ws.manager_on()?;
    // Never an agent, never the manager's own brain: the person at a terminal.
    if !caller.is_person() {
        return Err(denied(if allow { "Only the person can allow a message. It waits for them in Glasscode." } else { "Only the person can reject a message. It waits for them in Glasscode." }));
    }
    // Its own outbox (M56.2); one not read holds every decision.
    let Some(outbox) = ws.round.outbox.as_ref().map(|outbox| &outbox.messages) else {
        return Err(failed("The manager's outbox is not read yet (or could not be): nothing was decided."));
    };
    let message = pick(outbox, arg(op, "message"), |m| &m.id, |m| format!("{} {}", m.to, m.body), "message")?.clone();
    if !matches!(message.state, MsgState::Draft | MsgState::Failed) {
        return Err(failed(format!("That message to {} is {} already.", message.to, message_state(&message))));
    }
    if allow && message.tier == Tier::Never {
        return Err(failed(format!("{} never sends a message like this one. Write to {} yourself.", settings.name, message.to)));
    }
    let edited = op.text("text").map(str::to_string).filter(|_| allow);
    plan(Kind::Board, format!("{} the message to {}", if allow { "allow" } else { "reject" }, message.to), move |ws, r, _, _| {
        let after = {
            match ws.v2_decide_message(&message.id, allow, edited) {
                Some(after) => after,
                None => return ws.reply(r, failed(format!("The message to {} is no longer in the outbox: nothing was decided.", message.to))),
            }
        };
        let text = if allow { format!("Allowed. The message to {} goes out.", after.to) } else { format!("Rejected the message to {}.", after.to) };
        ws.reply(r, Reply::ok(text, to_json(&after)));
    })
}






/// v1's `manager check list`, typed by its v1 name: the schedules' list.
fn check_list(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    crate::ops_v2::schedule_list(ws, caller, op)
}

/// v1's `manager check add`, typed by its v1 name: a schedule, off until the person approves it.
fn check_add(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    crate::ops_v2::schedule_add(ws, caller, op)
}

/// v1's `manager check remove`, typed by its v1 name: the schedule goes.
fn check_remove(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    crate::ops_v2::schedule_remove(ws, caller, op)
}

// ----- the words of the manager's ops (were manager_cmd.rs) -------------------------------------

pub(crate) const OFF: &str = "The manager is off. Switch it on in Settings › Manager.";

/// A voice token is good for this long.
pub(crate) const TOKEN_SECS: u64 = 3600;

/// The screens `show --view` knows.
pub(crate) const VIEWS: &[&str] = &["manager", "sessions", "today", "tickets", "usage", "settings"];

/// The parts of a session's right panel `show --section` can point at.
pub(crate) const SECTIONS: &[&str] = &["checks", "preview", "reviews", "comments", "changes", "files", "reply"];





fn id_start(id: &str) -> String {
    id.chars().take(8).collect()
}

/// At most `max` characters of one line, with "…" when something was cut.
pub(crate) fn clip(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub(crate) fn to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_default()
}


/// A relative path is from where the command was run.
pub(crate) fn expand(path: &str, base: Option<&Path>) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        return glasscode_core::paths::home().join(rest);
    }
    let p = PathBuf::from(path);
    match base {
        Some(base) if p.is_relative() => base.join(p),
        _ => p,
    }
}

/// "30", "30m", "2h", "1d" as minutes.
pub(crate) fn minutes(text: &str) -> Option<u64> {
    let t = text.trim().to_lowercase();
    let (number, times) = match t.chars().last()? {
        'm' => (&t[..t.len() - 1], 1),
        'h' => (&t[..t.len() - 1], 60),
        'd' => (&t[..t.len() - 1], 1440),
        _ => (t.as_str(), 1),
    };
    number.trim().parse::<u64>().ok().filter(|n| *n > 0).map(|n| n * times)
}

/// "every 30 minutes", "every 2 hours".
pub(crate) fn every(secs: u64) -> String {
    match secs / 60 {
        m if m % 1440 == 0 => format!("every {}", plural((m / 1440) as usize, "day", "days")),
        m if m % 60 == 0 => format!("every {}", plural((m / 60) as usize, "hour", "hours")),
        m => format!("every {}", plural(m as usize, "minute", "minutes")),
    }
}

/// Who a note or a follow-up comes from: "you", "manager" or "session:<id>".
pub(crate) fn source_of(caller: &Caller) -> String {
    match &caller.session {
        _ if caller.is_person() => "you".into(),
        Some(id) => format!("session:{id}"),
        None if caller.is_manager() => "manager".into(),
        None => "outside".into(),
    }
}

pub(crate) fn tab_of(word: &str) -> Result<PanelTab, Reply> {
    match word.to_lowercase().replace(['-', '_', ' '], "").as_str() {
        "pr" | "pullrequest" | "pull" => Ok(PanelTab::PullRequest),
        "changes" | "diff" => Ok(PanelTab::Changes),
        "files" => Ok(PanelTab::Files),
        _ => Err(usage(format!("“{word}” is not a tab: pr, changes or files."))),
    }
}

/// The tab a part of the panel is on.
pub(crate) fn tab_for(section: &str) -> Option<PanelTab> {
    match section {
        "checks" | "preview" | "reviews" | "comments" => Some(PanelTab::PullRequest),
        "changes" => Some(PanelTab::Changes),
        "files" => Some(PanelTab::Files),
        _ => None,
    }
}

pub(crate) fn follow_up_line(f: &FollowUp) -> String {
    let state = match f.state {
        FollowState::Open => "open",
        FollowState::Fixed => "fixed?",
        FollowState::Done => "done",
        FollowState::Dropped => "dropped",
    };
    let what = if f.number > 0 { format!("{}#{} “{}”", f.repo, f.number, clip(&f.title, 60)) } else { format!("“{}”", clip(if f.title.is_empty() { &f.note } else { &f.title }, 90)) };
    let who = match (f.person.trim(), f.login.trim()) {
        ("", "") => String::new(),
        ("", login) => format!("  · @{login}"),
        (person, _) => format!("  · {person}"),
    };
    let blockers = if f.blockers.is_empty() { String::new() } else { format!("  · {} of {} still open", f.open_blockers(), f.blockers.len()) };
    format!("{}  [{state}] {what}{who}{blockers}", id_start(&f.id))
}

pub(crate) fn message_state(m: &OutMessage) -> &'static str {
    match (m.state, m.tier) {
        (MsgState::Draft, Tier::Never) => "never sent",
        (MsgState::Draft, _) => "waits for the person",
        (MsgState::Approved, _) => "allowed, going out",
        (MsgState::Sent, _) => "sent",
        (MsgState::Rejected, _) => "rejected",
        (MsgState::Failed, _) => "failed",
    }
}

pub(crate) fn message_line(m: &OutMessage) -> String {
    let why = if m.why.trim().is_empty() { String::new() } else { format!("  (why: {})", clip(&m.why, 70)) };
    let error = m.error.as_deref().filter(|e| !e.is_empty()).map(|e| format!("  ({})", clip(e, 70))).unwrap_or_default();
    format!("{}  [{}] to {}: “{}”{why}{error}", id_start(&m.id), message_state(m), m.to, clip(&m.body, 110))
}


/// The one thing in a list that `which` names: its id, the start of its id, or words from
/// its text. `Err` says why not (nothing, or several).
pub(crate) fn pick<'a, T>(items: &'a [T], which: &str, id: impl Fn(&T) -> &str, text: impl Fn(&T) -> String, noun: &str) -> Result<&'a T, Reply> {
    let w = which.trim();
    if let Some(exact) = items.iter().find(|i| id(i) == w) {
        return Ok(exact);
    }
    let starts: Vec<&T> = items.iter().filter(|i| w.len() >= 4 && id(i).starts_with(w)).collect();
    if let [one] = starts.as_slice() {
        return Ok(one);
    }
    let words: Vec<String> = w.to_lowercase().split_whitespace().map(str::to_string).collect();
    let hits: Vec<&T> = items
        .iter()
        .filter(|i| {
            let t = text(i).to_lowercase();
            !words.is_empty() && words.iter().all(|x| t.contains(x))
        })
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(failed(format!("No {noun} “{w}”."))),
        many => Err(usage(format!("“{w}” matches {} of them: {}. Use its id.", many.len(), many.iter().take(6).map(|i| id_start(id(i))).collect::<Vec<_>>().join(", ")))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The card of `data forget` says what it forgets (found in the journey run: "forget data").
    #[test]
    fn the_forget_card_names_what_goes() {
        let source = include_str!("ops_run.rs");
        let body = &source[source.find("fn data_forget(").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        assert!(body.contains("format!(\"forget {named} everywhere"));
        assert!(!body.contains("memory_plan(Kind::Settings, \"forget data\""));
    }

    /// An export's or an import's relative path is from where the command was typed, not the
    /// app's own folder (found in the journey run).
    #[test]
    fn a_data_path_is_from_the_callers_folder() {
        assert_eq!(crate::commands::expand("export.tar", Some(std::path::Path::new("/w"))), std::path::PathBuf::from("/w/export.tar"));
        assert_eq!(crate::commands::expand("/x/export.tar", Some(std::path::Path::new("/w"))), std::path::PathBuf::from("/x/export.tar"));
        let source = include_str!("ops_run.rs");
        for handler in ["fn data_export(", "fn data_import(", "fn data_schedule("] {
            let body = &source[source.find(handler).unwrap()..];
            let body = &body[..body.find("\n}\n").unwrap()];
            assert!(body.contains("crate::commands::expand(") && body.contains("caller.cwd"), "{handler}");
        }
    }
    use glasscode_core::control::Rule;

    /// This file, for the scans below.
    const SOURCE: &str = include_str!("ops_run.rs");

    /// The body of each handler of `HANDLERS`, by op name: from its `fn` to the next item.
    fn bodies() -> Vec<(&'static str, String)> {
        // A handler `ops_v2` shares is `pub(crate)`: read it as any other.
        let source = SOURCE[..SOURCE.find("#[cfg(test)]\nmod tests").unwrap()].replace("\npub(crate) fn ", "\nfn ");
        let source = source.as_str();
        let functions: Vec<(String, String)> = source
            .split("\nfn ")
            .skip(1)
            .map(|chunk| {
                let name = chunk[..chunk.find('(').unwrap()].to_string();
                (name, chunk.to_string())
            })
            .collect();
        let body_of = |name: &str| functions.iter().find(|(n, _)| n == name).map(|(_, b)| b.clone()).unwrap_or_else(|| panic!("no fn {name}"));
        // A handler's helpers are part of it: `board_done` is `board_done_or_undo` too.
        let helpers = ["board_done_or_undo", "board_focus", "session_close_or_restart", "follow_up_close", "message_decide", "github_target", "run_gh"];
        // Each entry of HANDLERS is written `("op.name", function),`.
        let table = &source[source.find("pub(crate) const HANDLERS").unwrap()..];
        let table = &table[..table.find("];").unwrap()];
        table
            .lines()
            .filter_map(|l| l.trim().strip_prefix("(\"")?.strip_suffix("),")?.split_once("\", "))
            .map(|(op, function)| {
                let op = HANDLERS.iter().find(|(name, _)| *name == op).map(|(name, _)| *name).unwrap();
                let mut body = body_of(function);
                for helper in helpers {
                    if body.contains(&format!("{helper}(")) {
                        body.push_str(&body_of(helper));
                    }
                }
                (op, body)
            })
            .collect()
    }

    /// Every source file of the app (`src/`, its folders too), each without its tests: what a scan
    /// must cover so no call site is left out.
    fn app_sources() -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let code = text.find("#[cfg(test)]\nmod tests").map_or(text.as_str(), |at| &text[..at]).to_string();
                    let name = path.strip_prefix(concat!(env!("CARGO_MANIFEST_DIR"), "/src/")).unwrap().to_string_lossy().into_owned();
                    out.push((name, code));
                }
            }
        }
        let mut out = Vec::new();
        walk(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")), &mut out);
        out.sort();
        assert!(out.len() > 60, "{}", out.len());
        out
    }

    /// The body of a function of `source`, from `fn name(` to the next item at its own depth.
    fn function<'a>(source: &'a str, name: &str) -> &'a str {
        let start = source.find(&format!("fn {name}(")).unwrap_or_else(|| panic!("no fn {name}"));
        let rest = &source[start..];
        let open = rest.find('{').unwrap();
        let mut depth = 0;
        for (i, c) in rest[open..].char_indices() {
            depth += match c {
                '{' => 1,
                '}' => -1,
                _ => 0,
            };
            if depth == 0 {
                return &rest[..open + i + 1];
            }
        }
        rest
    }

    /// Every handler is reached through the gate (M21.1): a handler is called in one place only,
    /// `op_plan`, and its callers decide by `decide()` first (`control_gated`, for the command and
    /// for `run_op`) and return on a refusal before it.
    #[test]
    fn every_handler_goes_through_decide() {
        let source = &SOURCE[..SOURCE.find("#[cfg(test)]\nmod tests").unwrap()];
        let control = include_str!("control_ui.rs");
        let control = &control[..control.find("#[cfg(test)]").unwrap_or(control.len())];
        let workspace = include_str!("workspace.rs");
        // One call of a handler, in `op_plan`.
        assert_eq!(source.matches("HANDLERS.iter()").count(), 1);
        assert!(function(source, "op_plan").contains("HANDLERS.iter()") && function(source, "op_plan").contains("handler(self, caller, op)"));
        let app: Vec<(&str, &str)> = vec![
            ("ops_run.rs", source),
            ("control_ui.rs", control),
            ("workspace.rs", workspace),
            ("commands.rs", include_str!("commands.rs")),
            ("commands2.rs", include_str!("commands2.rs")),
            ("manager_view.rs", include_str!("manager_view.rs")),
            ("today_ui.rs", include_str!("today_ui.rs")),
            ("manager_ui.rs", include_str!("manager_ui.rs")),
        ];
        let calls = |what: &str| -> Vec<&str> { app.iter().flat_map(|(file, text)| std::iter::repeat_n(*file, text.matches(what).count())).collect() };
        // Every plan of a handler, in every file of the app, only after the gate: an op it let
        // through and an approved pending action (`control_ui`, below), and a child's start inside
        // `session.child`, which the gate decided first (M61). v1's two (`control_plan` and
        // `run_op`'s, which Settings › Permissions checked) went with M56's rows 1 and 7.
        let all = app_sources();
        let sites: Vec<&str> = all.iter().flat_map(|(file, text)| std::iter::repeat_n(file.as_str(), text.matches(".op_plan(").count())).collect();
        assert_eq!(sites, ["control_ui.rs", "control_ui.rs", "ops_v2.rs"]);
        assert_eq!(calls(".op_plan("), ["control_ui.rs", "control_ui.rs"]);
        let v2 = &all.iter().find(|(file, _)| file == "ops_v2.rs").unwrap().1;
        assert!(function(v2, "session_child").contains("ws.op_plan(&caller, &start, None)"), "the child's start is planned inside its gated op");
        // An approved pending action (M21.2) is planned again only after its re-check, which asks
        // the gate again.
        // (`control_decide_pending` hands every answer to `control_decide_pending_as`.)
        let approved = function(control, "control_decide_pending_as");
        let (gate, recheck, planned) = (approved.find("self.control_gate(").unwrap(), approved.find(".recheck(").unwrap(), approved.find("self.op_plan(").unwrap());
        assert!(gate < recheck && recheck < planned, "decide, re-check, then plan");
        // v1's runner (`control_plan`, `control_run` by Settings › Permissions' kinds) went with M56
        // row 7: a command's plan is made only after the gate.
        assert!(calls(".control_plan(").is_empty() && calls("fn control_run(").is_empty() && calls("fn control_rule(").is_empty());
        // The gate, a return on Never, then the handler.
        let gated = function(control, "control_gated_as");
        let (gate, never, planned) = (gated.find("self.control_gate(").unwrap(), gated.find("Decision::Never { why }").unwrap(), gated.find("self.op_plan(").unwrap());
        assert!(gate < never && never < planned, "decide, refuse, then plan");
        assert!(gated[never..planned].contains("return;"));
        assert!(function(control, "control_gate").contains("gate::decide("));
        // The command goes there (`control_gated_as`, which carries the caller's op id, ops-10),
        // and so does run_op; neither makes a plan of its own.
        let handle = function(control, "control_handle_as");
        assert!(handle.contains("self.control_gated_as(") && !handle.contains("op_plan("));
        let run_op = function(workspace, "run_op");
        assert!(run_op.contains("self.control_gated(") && !run_op.contains("op_plan("));
        assert_eq!(calls("fn control_handle_as("), ["control_ui.rs"]);
    }

    #[test]
    fn memory_is_reached_only_as_far_as_the_caller_may() {
        let session = Who::Session("s-1".into());
        // A session: its own work and project only, and it must have one.
        assert_eq!(memory_reach(&session, (Some("s-1"), Some("p-app")), (None, None)), Ok(Reach::Own));
        assert_eq!(memory_reach(&session, (Some("s-1"), Some("p-app")), (Some("s-1"), Some("p-app"))), Ok(Reach::Own));
        assert!(memory_reach(&session, (Some("s-1"), Some("p-app")), (None, Some("p-web"))).is_err(), "another project");
        assert!(memory_reach(&session, (Some("s-1"), Some("p-app")), (Some("s-2"), None)).is_err(), "another session");
        assert!(memory_reach(&session, (None, None), (None, None)).is_err(), "no project: no memory, never every project's");
        // The person in the app, and the manager, may name any; with none, every project.
        for who in [Who::You, Who::Manager] {
            assert_eq!(memory_reach(&who, (None, None), (None, None)), Ok(Reach::Everywhere));
            assert_eq!(memory_reach(&who, (None, None), (None, Some("p-web"))), Ok(Reach::Named));
        }
        // Only the app's own window is the person: a command typed in a shell tab writes as a session.
        let mut caller = Caller::you();
        assert_eq!(memory_who(&caller), Who::You);
        caller.channel = glasscode_core::caller::Caller::ShellTab { session: "s-1".into(), project: None };
        assert_eq!(memory_who(&caller), Who::Session("s-1".into()));
        caller.channel = glasscode_core::caller::Caller::Terminal { pid: None };
        assert!(matches!(memory_who(&caller), Who::Session(_)));
    }

    #[test]
    fn every_op_has_a_handler() {
        for op in ops::OPS {
            let handled = HANDLERS.iter().chain(crate::ops_v2::HANDLERS).chain(crate::movein_ops::HANDLERS).filter(|(name, _)| *name == op.name).count();
            // The v2 engine's ops have theirs in `ops_v2`, or none while they wait for their slice.
            assert!(handled == 1 || (handled == 0 && op.v2_only), "{}: {handled} handlers", op.name);
        }
        for (name, _) in HANDLERS.iter().chain(crate::ops_v2::HANDLERS).chain(crate::movein_ops::HANDLERS) {
            assert!(ops::find(name).is_some(), "{name} is no op");
        }
    }

    /// Every `plan(Kind::…)` of the command is a handler's, and is its op's v1 kind (a source scan
    /// against the table): Settings › Permissions asks about exactly what it asked about before.
    /// The handlers, their old hand-parsed sites (49 of them) and the three `answer` helpers are
    /// in this file; the other files keep only `new_session_plan`'s.
    #[test]
    fn every_plan_site_has_an_op() {
        // The plans whose kind is not their op's: a dry run only reads.
        const OTHER_KINDS: &[(&str, &str)] = &[("worktree.clean", "Read"), ("data.import", "Read")];
        let mut sites = 0;
        for (op, body) in bodies() {
            let spec = ops::find(op).unwrap();
            // `Kind::` on its own (not `ItemKind::`).
            let kinds: Vec<&str> = body
                .match_indices("Kind::")
                .filter(|(i, _)| !body[..*i].ends_with(|c: char| c.is_alphanumeric() || c == '_'))
                .map(|(i, _)| body[i + 6..].split(|c: char| !c.is_alphanumeric()).next().unwrap())
                .collect();
            let starts = body.contains("new_session_plan(");
            // `answer` and `read_later` plan a read; a send is a queued delivery
            // (`session_send_queued`, whose plan is `Kind::Send`).
            let reads = body.contains("answer(") || body.contains("read_later(");
            let queued = body.contains("session_send_queued(");
            // v1's checks, typed by v1's name, are the schedules' handlers (their plans are those
            // ops' own, checked in `ops_v2`).
            if body.contains("crate::ops_v2::schedule_") {
                continue;
            }
            if queued {
                assert_eq!(spec.kind_v1, Kind::Send, "{op}");
            }
            assert!(!kinds.is_empty() || starts || reads || queued, "{op} plans nothing");
            if kinds.is_empty() && !starts && !queued {
                assert_eq!(spec.kind_v1, Kind::Read, "{op} reads, its v1 kind is {:?}", spec.kind_v1);
            }
            for kind in &kinds {
                let own = format!("{:?}", spec.kind_v1);
                assert!(*kind == own || OTHER_KINDS.contains(&(op, *kind)), "{op} plans Kind::{kind}, its v1 kind is {own}");
                sites += 1;
            }
            if starts {
                assert_eq!(spec.kind_v1, Kind::Start, "{op}");
            }
        }
        assert!(sites >= 45, "{sites}");
        // No other file of the app plans a command, but for the sessions every start makes.
        for (file, text) in [("commands.rs", include_str!("commands.rs")), ("commands2.rs", include_str!("commands2.rs")), ("manager_view.rs", include_str!("manager_view.rs")), ("control_ui.rs", include_str!("control_ui.rs"))] {
            let plans: Vec<&str> = text.match_indices("plan(Kind::").map(|(i, _)| &text[i..i + 16]).collect();
            let expected: &[&str] = if file == "commands.rs" { &["plan(Kind::Start"] } else { &[] };
            assert_eq!(plans, expected, "{file}");
        }
    }

    /// A plan's check runs before anyone is asked: on every way a plan runs (the command, and
    /// `run_op`, both through the gate), `after_check` comes before the gate's ask or the run.
    #[test]
    fn checks_come_before_the_person_is_asked() {
        let planned = checked(answer(Reply::ok("x", Value::Null)), || Err(failed("Branch x exists already.")));
        let check = planned.ok().and_then(|plan| plan.check).expect("a check");
        assert!(matches!(check(), Err(Reply::Done { code: code::FAILED, .. })));
        let control = include_str!("control_ui.rs");
        let body = |name: &str| -> &str {
            let at = control.find(&format!("fn {name}(")).unwrap_or_else(|| panic!("no fn {name}"));
            &control[at..at + control[at..].find("\n    }\n").unwrap()]
        };
        assert!(body("control_handle_as").contains("self.control_gated_as("));
        let gated = body("control_gated_as");
        assert!(gated.find("self.after_check(").unwrap() < gated.find("ws.control_ask(").unwrap());
        let workspace = include_str!("workspace.rs");
        let run_op = &workspace[workspace.find("pub fn run_op(").unwrap()..];
        assert!(run_op[..run_op.find("\n    }\n").unwrap()].contains("self.control_gated("));
        // The person is asked only from the gate's way, after the check (above); v1's `control_run`
        // asked without the check and went with M56. A child's start (M61) runs its check before
        // it runs.
        let all = app_sources();
        let asks: Vec<String> = all.iter().flat_map(|(file, text)| text.match_indices(".control_ask(").map(move |(at, _)| format!("{file}:{}", text[..at].rfind("fn ").map(|f| text[f + 3..].split('(').next().unwrap_or_default()).unwrap_or_default()))).collect();
        assert_eq!(asks, ["control_ui.rs:control_gated_as"], "the one place the person is asked");
        let v2 = &all.iter().find(|(file, _)| file == "ops_v2.rs").unwrap().1;
        let child = function(v2, "session_child");
        assert!(child.find("ws.after_check(").unwrap() < child.find("(plan.run)(").unwrap(), "the child's check, then its run");
    }

    /// No handler runs git or gh on the main thread, or reads a file there (ops-9): the blocking
    /// list of M12.2 is empty.
    #[test]
    fn blocking_list_empty() {
        // What blocks when it is called on the main thread, here or in a helper of another file
        // (ops-9). Each handler now asks git, gh and the disk off the main thread: in a background
        // task, in its plan's check (`checked`), or as a read made later (`read_later`).
        const CALLS: &[&str] = &["git::branch_exists", "git::default_branch", "github::repo_slug", "github::search_prs", "add_project_quietly(", "project_root_of(", "last_reply(", "read_log(", "read_to_string(", "scene_file("];
        const OFF_MAIN: &[&str] = &["background_spawn(", "checked(", "read_later(", "read_off_main("];
        let main_thread = |body: &str| -> String {
            let mut main = String::new();
            let mut rest = body;
            while let Some((at, call)) = OFF_MAIN.iter().filter_map(|call| rest.find(call).map(|at| (at, *call))).min() {
                main.push_str(&rest[..at]);
                let after = &rest[at + call.len()..];
                let mut depth = 1;
                let end = after.char_indices().find(|(_, c)| {
                    depth += match c {
                        '(' => 1,
                        ')' => -1,
                        _ => 0,
                    };
                    depth == 0
                });
                rest = &after[end.map(|(i, _)| i + 1).unwrap_or(after.len())..];
            }
            main.push_str(rest);
            main
        };
        let mut found = Vec::new();
        for (op, body) in bodies() {
            let main = main_thread(&body);
            found.extend(CALLS.iter().filter(|call| main.contains(**call)).map(|call| format!("{op}: {call}")));
        }
        assert!(found.is_empty(), "these handlers block the main thread: {found:?}");
        // The v2 engine's handlers read only off the main thread too.
        let v2 = include_str!("ops_v2.rs");
        let v2 = main_thread(&v2[..v2.find("#[cfg(test)]\nmod tests").unwrap()]);
        for read in ["reads::git_", "reads::file_", "reads::pr_", "reads::ci_", "reads::review_"] {
            assert!(!v2.contains(read), "ops_v2 calls {read} on the main thread");
        }
        // And the scan sees what it looks for: a call outside those is found.
        assert!(main_thread("checked(p, || git::branch_exists(a, b)); github::repo_slug(r)").contains("github::repo_slug") && !main_thread("checked(p, || git::branch_exists(a, b))").contains("branch_exists"));
    }

    #[test]
    fn replies_say_their_op() {
        let failed_reply = with_op(failed("No."), Some("board.done"));
        let Reply::Done { code: 1, data, .. } = &failed_reply else { panic!("{failed_reply:?}") };
        assert_eq!(*data, json!({ "op": "board.done", "error_code": "failed" }));
        // A registry error keeps its own class and candidates.
        let ambiguous = OpError::new(ErrorCode::Ambiguous { candidates: vec![ops::Candidate { id: "a".into(), title: "A".into(), project: None }] }, "which?");
        let Reply::Done { code, data, text } = with_op(error_reply(&ambiguous), Some("session.show")) else { panic!() };
        assert_eq!((code, text.as_str()), (2, "which?"));
        assert_eq!(data["error_code"], json!("ambiguous"));
        assert_eq!(data["candidates"][0]["id"], json!("a"));
        assert_eq!(data["op"], json!("session.show"));
        // A success's data gains its op where it is an object, and is as it was otherwise.
        for data in [json!([1, 2]), Value::Null] {
            assert_eq!(with_op(Reply::ok("Done.", data.clone()), Some("board.add")).clone_data(), data);
        }
        assert_eq!(with_op(Reply::ok("Done.", json!({ "id": "x" })), Some("board.add")).clone_data(), json!({ "id": "x", "op": "board.add" }));
        assert_eq!(with_op(Reply::ok("Done.", json!([1])), Some("session.list")).clone_data(), json!([1]));
    }

    trait Data {
        fn clone_data(&self) -> Value;
    }

    impl Data for Reply {
        fn clone_data(&self) -> Value {
            match self {
                Reply::Done { data, .. } => data.clone(),
                Reply::Waiting { .. } => Value::Null,
            }
        }
    }

    #[test]
    fn to_pc_and_to_mac_are_the_same_move() {
        let ctx = ParseCtx { caller_is_person: true, sessions: vec![SessionView { id: "abcd1234".into(), title: "x".into(), ..Default::default() }], ..Default::default() };
        let op = |line: &[&str]| ops::parse(&line.iter().map(|w| w.to_string()).collect::<Vec<_>>(), &ctx);
        // A switch: the session after it stays a word.
        let (mac, pc) = (op(&["session", "move", "--to-mac", "abcd1234"]).unwrap(), op(&["session", "move", "--to-pc", "abcd1234"]).unwrap());
        assert_eq!(mac.args, pc.args);
        assert!(mac.on("to_this_computer") && arg(&mac, "session") == "abcd1234");
        assert!(!op(&["session", "move", "abcd1234", "--to-server"]).unwrap().on("to_this_computer"));
        assert!(op(&["session", "move", "abcd1234", "--to-laptop"]).is_err());
    }

    // ----- the golden cases --------------------------------------------------------------------

    /// The board and the manager's data as they were when the goldens were recorded.
    const TASKS: &[(&str, &str)] = &[("zysg", "Send Sam the wording for the sign-up page"), ("rrkh", "Review the retry queue change"), ("pgdw", "Renew the certificate"), ("vkn6", "Review Renovate PRs")];
    const CHECKS: &[(&str, &str)] = &[("deploy", "See whether the deploy went out"), ("c0ffee00", "See whether the deploy went out")];

    /// The ops whose approval sentence `what` makes from the op and the names it is given.
    const PURE: &[&str] = &[
        "board.add",
        "board.done",
        "board.undo",
        "board.snooze",
        "board.unsnooze",
        "board.pin",
        "board.unpin",
        "board.move",
        "board.note",
        "board.edit",
        "board.remove",
        "session.open",
        "session.rename",
        "session.close",
        "session.restart",
        "session.send",
        "session.hand_off",
        "pr.rerun",
        "pr.comment",
        "pr.label",
        "pr.open_draft",
        "app.open",
        "app.notify",
        "manager.say",
        "manager.tour",
        "manager.remember",
        "manager.forget",
        "manager.follow_up.add",
        "manager.follow_up.done",
        "manager.follow_up.drop",
        "manager.check.remove",
    ];

    /// The ops whose sentence needs the app's state, and what of it (the A/B replay against v1
    /// checks them).
    const NOT_PURE: &[(&str, &str)] = &[
        ("session.new", "the default agent, its account, the worktree's folder"),
        ("pr.review", "the default agent, its account"),
        ("pr.build", "the default agent, its account"),
        ("project.import", "whether the folder is there, where clones go"),
        ("project.remove", "how many sessions the project has"),
        ("worktree.new", "the project's name"),
        ("worktree.rename", "the worktree's branch"),
        ("worktree.remove", "the worktree's branch and sessions"),
        ("worktree.clean", "the project's name"),
        ("worktree.open", "the worktree's folder"),
        ("manager.show", "which sessions the manager sees"),
        ("manager.message.draft", "the teammate's name in Settings › Manager"),
        ("manager.check.add", "the manager's name"),
    ];

    fn goldens() -> Vec<(String, Value)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../glasscode_core/tests/ops_golden");
        let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
        files.sort();
        files.into_iter().map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap())).collect()
    }

    /// What the app's handlers say for each golden case, where that needs no window: a parse
    /// error is the reply v1 gave (its words and exit code), and a change an outside caller waits
    /// for is announced with the sentence v1 used.
    #[test]
    fn same_words_same_reply() {
        let (mut replies, mut sentences, mut stateful) = (0, 0, 0);
        let mut failures = Vec::new();
        for (file, golden) in goldens() {
            let ctx = &golden["ctx"];
            let today = day::parse_iso(ctx["today"].as_str().unwrap()).unwrap();
            let sessions: Vec<SessionView> = serde_json::from_value(ctx["sessions"].clone()).unwrap();
            for case in golden["cases"].as_array().unwrap() {
                let Some(v1) = case.get("v1") else { continue };
                if case.get("note").is_some() || case.get("differs").is_some() {
                    continue;
                }
                let at = format!("{file} {}", case["words"]);
                if let Some(error) = case.get("error") {
                    let error: OpError = serde_json::from_value(error.clone()).unwrap();
                    let Reply::Done { code, text, .. } = with_op(error_reply(&error), None) else { unreachable!() };
                    if (code, text.as_str()) != (v1["exit"].as_i64().unwrap() as i32, v1["says"].as_str().unwrap_or_default()) {
                        failures.push(format!("{at}: the app replies {code} {text:?}"));
                    }
                    replies += 1;
                    continue;
                }
                if v1["exit"] != json!(4) || case.get("caller").is_some() {
                    continue;
                }
                let op: Op = serde_json::from_value(case["op"].clone()).unwrap();
                let name = op.name();
                if !PURE.contains(&name) {
                    assert!(NOT_PURE.iter().any(|(n, _)| *n == name), "{at}: {name} is in neither list");
                    stateful += 1;
                    continue;
                }
                let session = op.text("session").or_else(|| op.text("place")).and_then(|id| sessions.iter().find(|s| s.id == id));
                let title = match name {
                    "board.add" => {
                        let text = arg(&op, "title");
                        if op.on("as_written") { text.to_string() } else { board::parse_quick(text, today, &[]).title }
                    }
                    "manager.check.remove" => CHECKS.iter().find(|(w, _)| *w == arg(&op, "which")).map(|(_, t)| t.to_string()).unwrap(),
                    _ if name.starts_with("board.") => TASKS.iter().find(|(id, _)| *id == arg(&op, "task")).map(|(_, t)| t.to_string()).unwrap(),
                    _ => session.map(|s| s.title.clone()).unwrap_or_default(),
                };
                let other = match name {
                    "session.hand_off" => "Codex · My login".to_string(),
                    "pr.open_draft" => session.map(|s| s.branch.clone()).unwrap_or_default(),
                    _ if name.starts_with("manager.") => "Manager".to_string(),
                    _ => String::new(),
                };
                let said = format!("A program outside Glasscode wants to {}", what(&op, &Names { title, other }, today));
                if said != v1["says"].as_str().unwrap() {
                    failures.push(format!("{at}\n  v1:  {}\n  now: {said}", v1["says"]));
                }
                sentences += 1;
            }
        }
        assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
        eprintln!("same words, same reply: {replies} parse errors, {sentences} approval sentences; {stateful} sentences need the app's state");
        assert!(replies >= 80 && sentences >= 80, "{replies} replies, {sentences} sentences, {stateful} need the app's state");
        // The rules of Settings › Permissions the goldens' exit codes rest on.
        assert_eq!(Kind::Settings.default_rule(), Rule::Never);
    }

    // The words of the manager's ops (were manager_cmd.rs's tests).

    fn caller(channel: glasscode_core::caller::Caller, session: Option<&str>) -> Caller {
        Caller { who: "x".into(), channel, session: session.map(str::to_string), session_title: None, agent: None, cwd: None, project: None, drive: None }
    }

    #[test]
    fn times_read_as_minutes() {
        assert_eq!(minutes("30"), Some(30));
        assert_eq!(minutes("45m"), Some(45));
        assert_eq!(minutes("2h"), Some(120));
        assert_eq!(minutes("1d"), Some(1440));
        assert_eq!(minutes("0"), None);
        assert_eq!(minutes("soon"), None);
        assert_eq!(every(30 * 60), "every 30 minutes");
        assert_eq!(every(3600), "every 1 hour");
        assert_eq!(every(2 * 86_400), "every 2 days");
    }

    #[test]
    fn who_a_note_comes_from() {
        use glasscode_core::caller::{Caller as Channel, Lane};
        let (you, session, manager, outside) = (Channel::Ui, Channel::Session { id: "s1".into(), project: None }, Channel::Manager { lane: Lane::Wake, wake: None, project: None }, Channel::Terminal { pid: None });
        assert_eq!(source_of(&caller(you.clone(), None)), "you");
        assert_eq!(source_of(&caller(session.clone(), Some("s1"))), "session:s1");
        assert_eq!(source_of(&caller(manager.clone(), None)), "manager");
        assert_eq!(source_of(&caller(outside.clone(), None)), "outside");
        assert!(caller(manager, None).is_manager());
        assert!(!caller(session, Some("s1")).is_manager() && !caller(you, None).is_manager() && !caller(outside, None).is_manager());
    }

    #[test]
    fn tabs_and_sections() {
        assert_eq!(tab_of("pr").ok(), Some(PanelTab::PullRequest));
        assert_eq!(tab_of("Pull-Request").ok(), Some(PanelTab::PullRequest));
        assert_eq!(tab_of("diff").ok(), Some(PanelTab::Changes));
        assert!(tab_of("terminal").is_err());
        assert_eq!(tab_for("checks"), Some(PanelTab::PullRequest));
        assert_eq!(tab_for("files"), Some(PanelTab::Files));
        assert_eq!(tab_for("reply"), None);
    }

    #[test]
    fn picking_by_id_start_or_words() {
        let items = [("aaaa1111-x", "see whether 1111 was merged"), ("bbbb2222-x", "see whether the deploy went out")];
        let by = |which: &str| pick(&items, which, |c| c.0, |c| c.1.to_string(), "item").map(|c| c.0.to_string());
        assert_eq!(by("aaaa1111-x").ok().as_deref(), Some("aaaa1111-x"));
        assert_eq!(by("bbbb").ok().as_deref(), Some("bbbb2222-x"));
        assert_eq!(by("deploy").ok().as_deref(), Some("bbbb2222-x"));
        assert!(by("see whether").is_err());
        assert!(by("nothing like it").is_err());
    }

    #[test]
    fn lines_say_where_things_stand() {
        let draft = OutMessage { id: "m1m1m1m1-x".into(), to: "Sam".into(), body: "The fix is pushed.".into(), why: "he asked".into(), ..OutMessage::default() };
        assert_eq!(message_line(&draft), "m1m1m1m1  [waits for the person] to Sam: “The fix is pushed.”  (why: he asked)");
        assert_eq!(message_state(&OutMessage { tier: Tier::Never, ..OutMessage::default() }), "never sent");
        let follow = FollowUp { id: "f1f1f1f1-x".into(), repo: "acme/shop".into(), number: 12, title: "Retry queue".into(), person: "Mira".into(), ..FollowUp::default() };
        assert_eq!(follow_up_line(&follow), "f1f1f1f1  [open] acme/shop#12 “Retry queue”  · Mira");
    }
}
