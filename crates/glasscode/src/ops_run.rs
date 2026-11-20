//! The `glasscode` command's handlers, keyed by op name (M12.2). A command's words are parsed by
//! the registry first (`glasscode_core::ops::parse`), and the op's handler builds today's `Plan`:
//! the kind Settings › Permissions knows it by, what it does in words, and what it then does.
//! Each handler is the branch of the hand-parsed command it replaces, reading its words from the
//! `Op` instead of the command line.
//!
//! The words an op names (a project, a worktree, a task) are looked up in `glasscode_core::ops`'s
//! `World`, which `targets` reads too, so the handler and the gate agree on what they name.

use gpui::{AppContext as _, Context, Window};
use glasscode_core::board::{self, Draft, List, Section};
use glasscode_core::control::{Kind, Reply, code};
use glasscode_core::day::{self, Day};
use glasscode_core::git;
use glasscode_core::github;
use glasscode_core::linear;
use glasscode_core::manager::{Action, Check, FollowUp, MsgState, OutMessage, Show, Tier, brief, followups, memory, messages, scene, voice};
use glasscode_core::model::{AgentKind, new_id, now};
use glasscode_core::ops::{self, ErrorCode, Op, OpError, ParseCtx, SessionView, World};
use glasscode_core::sys;
use serde_json::{Value, json};

use crate::commands::{MoveTo, agent_of, clock, home_relative, plural, pr_status_reply, short};
use crate::commands2::{clone_folder_name, clone_source_checked, days, git_clone};
use crate::control_ui::{Caller, Plan, Responder, Trust};
use crate::manager_cmd::{OFF, SECTIONS, TOKEN_SECS, VIEWS, check_line, clip, day_of, every, follow_up_line, message_line, message_state, minutes, pick, source_of, tab_for, tab_of, to_json};
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
    ("app.approvals", app_approvals),
    ("app.wait", app_wait),
    ("approvals.list", approvals_list),
    ("approvals.decide", approvals_decide),
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

fn denied(text: impl Into<String>) -> Reply {
    Reply::fail(code::DENIED, text)
}

pub(crate) fn plan(kind: Kind, what: impl Into<String>, run: impl FnOnce(&mut Workspace, Responder, &mut Window, &mut Context<Workspace>) + 'static) -> Planned {
    Ok(Plan { kind, what: what.into(), run: Box::new(run), check: None })
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
/// gains `op` only with the v2 engine, and only where it is an object, so that v1's `--json` stays
/// as it was.
pub(crate) fn with_op(reply: Reply, op: Option<&str>, v2: bool) -> Reply {
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
    } else if let (true, Some(op), Value::Object(map)) = (v2, op, &mut data) {
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
    /// The op the words name and its plan, or the reply that says why not: v1's way, where the plan's
    /// kind is then looked up in Settings › Permissions (`control_rule`).
    pub fn control_plan(&mut self, caller: &Caller, words: &[String]) -> Result<(Op, Plan), Reply> {
        let op = self.control_parse(caller, words)?;
        let plan = self.op_plan(caller, &op)?;
        Ok((op, plan))
    }

    /// The op the words name, or the reply that says why not. Parsing runs nothing.
    pub fn control_parse(&self, caller: &Caller, words: &[String]) -> Result<Op, Reply> {
        let ctx = self.parse_ctx(caller);
        let v2 = ctx.v2;
        ops::parse(words, &ctx).map_err(|e| with_op(error_reply(&e), ops::name_of(words, v2), v2))
    }

    /// The plan of an op its caller has already named (the command's words, a key, a button, a
    /// typed tool): its handler's, or the reply that says why not. The one place a handler is
    /// called: with the v2 engine the gate has decided first (`control_gated`), and a refused op
    /// never reaches its handler.
    pub(crate) fn op_plan(&mut self, caller: &Caller, op: &Op) -> Result<Plan, Reply> {
        let v2 = self.control.engine_v2;
        let Some((_, handler)) = HANDLERS.iter().chain(crate::ops_v2::HANDLERS).find(|(name, _)| *name == op.name()).filter(|_| v2 || !op.spec.v2_only) else {
            let text = format!("“{}” is in the registry, and runs once its handler lands (the server-sync plan).", op.spec.cli.join(" "));
            return Err(with_op(error_reply(&OpError::new(ErrorCode::Failed, text)), Some(op.name()), v2));
        };
        let plan = handler(self, caller, op).map_err(|reply| with_op(reply, Some(op.name()), v2))?;
        // v2 (M31): what a caller other than the person sets off goes through the effect ledger.
        let effect = !matches!(op.spec.effect, glasscode_core::ops::EffectClass::Read | glasscode_core::ops::EffectClass::Navigate);
        Ok(if v2 && effect && !self.caller_is_person(caller) && op.name() != "session.send" { claimed(self, caller, op, plan) } else { plan })
    }

    /// Whether the caller may name things by the start of a name or words from a title (ops-17):
    /// with the v1 engine every caller may, as before; with v2 only the person.
    pub(crate) fn caller_is_person(&self, caller: &Caller) -> bool {
        !self.control.engine_v2 || caller.trust == Trust::You
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
            v2: self.control.engine_v2,
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
        ws.reply(r, Reply::ok(format!("Renamed “{old}” to “{name}”."), json!({ "id": id, "title": name })));
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
    if ws.control.engine_v2 {
        return session_send_queued(ws, caller, op, id, title, from, message);
    }
    plan(Kind::Send, what(op, &Names { title: title.clone(), ..Names::default() }, day::today()), move |ws, r, window, cx| {
        ws.session_send(&id, &title, &from, message, r, window, cx);
    })
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
    plan(Kind::Send, what(op, &Names { title: title.clone(), ..Names::default() }, day::today()), move |ws, r, window, cx| {
        if !ws.terminals.contains_key(&id) && ws.state.session(&id).is_some() {
            ws.ensure_terminal(&id, window, cx);
        }
        let delivery = glasscode_core::delivery_session::Delivery {
            note: Some(format!("(Sent by Glasscode because {from} asked it to.)")),
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
fn claimed(ws: &Workspace, caller: &Caller, op: &Op, plan: Plan) -> Plan {
    // Its check (`Plan::check`) stays: it runs before the claim, before anyone is asked.
    let Plan { kind, what, run, check } = plan;
    let mut request = [0u8; 12];
    glasscode_core::sys::random_bytes(&mut request);
    let subject: String = request.iter().map(|b| format!("{b:02x}")).collect();
    let origin = caller.session.as_ref().map(|s| format!("session:{s}")).unwrap_or_else(|| caller.who.clone());
    let target = glasscode_core::effects::short_hash(&op.args.to_string());
    let evidence = match op.name() {
        "board.move" => match (ws.board_task(caller, op), Section::parse(arg(op, "to"))) {
            (Ok(key), Some(section)) => json!({ "key": key, "section": section.key() }),
            _ => Value::Null,
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
        ..glasscode_core::effects::Intent::new(glasscode_core::effects::effect_key(&origin, &subject, op.name(), &target), op.name(), op.spec.retry)
    };
    let run: crate::control_ui::Run = Box::new(move |_, r, window, cx| {
        let key = intent.key;
        let task = cx.background_spawn(async move {
            let home = glasscode_core::paths::MachineId::this().map_err(|err| std::io::Error::other(err.to_string()))?;
            glasscode_core::effects::claim(&intent, &home)
        });
        cx.spawn_in(window, async move |this, cx| {
            let claimed = task.await;
            let _ = this.update_in(cx, |ws, window, cx| match claimed {
                Ok(glasscode_core::effects::Claim::New(_)) => {
                    let caller = r.caller.clone();
                    let mut local = Responder::local(move |ws, reply| {
                        let first = |text: &str| glasscode_core::words::scrub(&clip(text.lines().next().unwrap_or_default(), 120));
                        let state = match reply {
                            Reply::Done { code: code::OK, text, .. } => glasscode_core::effects::EffectState::Done(json!({ "said": first(text) })),
                            Reply::Done { text, .. } => glasscode_core::effects::EffectState::Failed(first(text)),
                            Reply::Waiting { .. } => glasscode_core::effects::EffectState::Uncertain,
                        };
                        ws.reply(r, reply.clone());
                        glasscode_core::effects::finish_later(key, state);
                    });
                    local.caller = caller;
                    run(ws, local, window, cx);
                }
                Ok(glasscode_core::effects::Claim::Already(record)) => ws.reply(r, failed(format!("This was done before ({:?}); it is not done again.", record.state))),
                Err(err) => ws.reply(r, failed(format!("The effect ledger could not be written ({err}); nothing was done."))),
            });
        })
        .detach();
    });
    Plan { kind, what, run, check }
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
        return answer(Reply::ok(format!("Already reviewing #{n} in “{}” ({}).", ws.session_title(s), short(&s.id)), ws.session_json(s)));
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

fn pr_open_draft(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let s = session_of(ws, op)?;
    let root = ws.state.project(&s.project_id).map(|p| p.root.clone()).unwrap_or_default();
    // The default branch is git's to say: read with the push, off the main thread.
    let base = op.text("base").map(str::to_string);
    let mut env = (*ws.env).clone();
    env.extend(ws.github_env(&s.project_id));
    let title = ws.session_title(&s);
    plan(Kind::Github, what(op, &Names { title, other: s.branch.clone() }, day::today()), move |_ws, r, _, cx| {
        let (dir, branch) = (s.worktree.clone(), s.branch.clone());
        let task = cx.background_spawn(async move {
            let base = base.unwrap_or_else(|| git::default_branch(&root));
            github::push_and_open_draft(&dir, &branch, &base, &env)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.pr_refresh(cx);
                ws.reply(r, match result {
                    Ok(()) => Reply::ok(format!("Pushed {} and opened a draft pull request.", s.branch), json!({ "branch": s.branch })),
                    Err(e) => failed(format!("Could not open the draft: {e:#}")),
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
                        Ok((id, false)) => Reply::ok(format!("{shown} is a project already ({}).", ws.state.project(&id).map(|p| p.name.clone()).unwrap_or_default()), json!({ "id": id })),
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
        let task = cx.background_spawn(async move { git_clone(&url, &target, &env).map_err(|e| format!("Could not clone it: {e:#}")).and_then(|()| crate::commands2::project_root_of(&target)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                let reply = match result.and_then(|root| ws.add_project_at_root(root, name, cx)) {
                    Ok((id, _)) => Reply::ok(format!("Cloned and imported {shown}."), json!({ "id": id })),
                    Err(e) => failed(e),
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
        if let Some(p) = ws.state.projects.iter_mut().find(|p| p.id == id) {
            p.github_account = (login != "default").then(|| login.clone());
        }
        ws.save();
        ws.configure_github(cx);
        ws.reply(r, Reply::ok(format!("New sessions in {name} use {login} on GitHub."), json!({ "id": id })));
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
        let task = cx.background_spawn(async move {
            let base = match from {
                Some(f) => f,
                None => git::start_base(&root, fetch).base,
            };
            git::add_worktree(&root, &name, &b, &base).map(|path| (path, base))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.refresh_worktrees(cx);
                ws.reply(r, match result {
                    Ok((path, base)) => Reply::ok(format!("Made {} on {branch} (from {base}).", home_relative(&path)), json!({ "path": path, "branch": branch, "base": base })),
                    Err(e) => failed(format!("Could not make the worktree: {e:#}")),
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
                    Ok(()) => Reply::ok(format!("Renamed {old} to {new} ({}).", home_relative(&wt.path)), json!({ "path": wt.path, "branch": new })),
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
                        Reply::ok(format!("{} is {name} in Linear.", ticket.identifier), json!({ "ticket": ticket.identifier, "status": name }))
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
    if caller.trust != Trust::You {
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

fn app_log(_: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let limit = op.number("limit").unwrap_or(20) as usize;
    // The log is a file: read off the main thread.
    read_later(move || {
        let entries = glasscode_core::control::read_log(limit);
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
        Reply::ok(text, serde_json::to_value(&entries).unwrap_or_default())
    })
}

fn app_approvals(ws: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    let list = &ws.control.approvals;
    let text = if list.is_empty() {
        "Nothing waits for the person.".to_string()
    } else {
        list.iter().map(|a| format!("#{}  {}  ({})", a.id, a.summary(), a.command)).collect::<Vec<_>>().join("\n")
    };
    answer(Reply::ok(text, ws.control_approvals_json()))
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
            .map(|p| format!("#{}  {} asks to {}\n    why: {}\n    waits {} min more\n    {}", p.number, p.who, p.what, p.reason, p.expires_ms.saturating_sub(now) / 60_000, p.payload().replace('\n', "\n    ")))
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

fn app_wait(_: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let id = op.number("id").unwrap_or_default();
    plan(Kind::Read, "read", move |ws, r, _, _| ws.control_wait(id, r))
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
    // agent's is a peer's, a terminal's is outside (ux-m1).
    glasscode_core::words::record(glasscode_core::words::command_origin(&caller.channel), &text, caller.session.as_deref(), None);
    // The manager takes a question as the person's. One from a session's agent says so.
    let text = if caller.trust == Trust::Session { format!("(Asked by {}, an agent, not the person.) {text}", caller.describe()) } else { text };
    // Asking changes nothing by itself (what the manager then does passes Settings › Automation as
    // its own), so the person's voice and phone can ask without an OK.
    plan(Kind::Read, format!("ask {name}: “{}”", clip(&text, 80)), move |ws, r, _, cx| {
        ws.manager_ask_then(
            text,
            voice,
            Box::new(move |ws, say, _cx| {
                let reply = match say {
                    Ok(say) => Reply::ok(say.clone(), json!({ "say": say })),
                    Err(why) => failed(why),
                };
                ws.reply(r, reply);
            }),
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
    let made = brief::build(&settings, &ws.manager.harness.data, &ws.manager_snapshot(), now());
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
        let stops = brief::tour(&ws.state.manager, &ws.manager.harness.data, &ws.manager_snapshot());
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
    let settings = ws.manager_on()?;
    let text = arg(op, "fact").trim().to_string();
    let source = source_of(caller);
    plan(Kind::Board, what(op, &Names { other: settings.name, ..Names::default() }, day::today()), move |ws, r, _, cx| {
        let id = memory::remember(&mut ws.manager.harness.data, &text, &source, now());
        ws.manager.harness.dirty = true;
        cx.notify();
        ws.reply(r, Reply::ok(format!("Remembered: {text}"), json!({ "id": id, "text": text })));
    })
}

fn manager_forget(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let settings = ws.manager_on()?;
    let which = arg(op, "which").trim().to_string();
    plan(Kind::Board, what(op, &Names { other: settings.name, ..Names::default() }, day::today()), move |ws, r, _, cx| {
        let reply = match memory::forget(&mut ws.manager.harness.data, &which, now()) {
            Some(text) => {
                ws.manager.harness.dirty = true;
                cx.notify();
                Reply::ok(format!("Forgot: {text}"), json!({ "text": text }))
            }
            None => failed(format!("Nothing in memory matches “{which}”. `glasscode manager memory` lists what is kept.")),
        };
        ws.reply(r, reply);
    })
}

fn manager_memory(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    ws.manager_on()?;
    let query = arg(op, "query").trim().to_string();
    let limit = op.number("limit").unwrap_or(20) as usize;
    let data = &ws.manager.harness.data;
    let notes = if query.is_empty() { memory::all(data) } else { memory::recall(data, &query, limit) };
    let text = match (notes.is_empty(), query.is_empty()) {
        (true, true) => "Nothing remembered yet. `glasscode manager remember \"<fact>\"` keeps one.".to_string(),
        (true, false) => format!("Nothing in memory matches “{query}”."),
        _ => notes.iter().take(limit.max(1)).map(|n| format!("{}  {}  ({}, {})", short(&n.id), n.text, n.source, day_of(n.updated.max(n.created)))).collect::<Vec<_>>().join("\n"),
    };
    answer(Reply::ok(text, Value::Array(notes.iter().take(limit.max(1)).map(|n| to_json(*n)).collect())))
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
        ws.manager_apply(actions, cx);
        let text = crate::control_ui::capitalize(&what.replacen("show", "showing", 1));
        ws.reply(r, Reply::ok(format!("{text}."), json!({ "show": to_json(&show), "say": say })));
    })
}

pub(crate) fn manager_scene(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let settings = ws.manager_on()?;
    // A scene in a file is read, and read again, off the main thread: first by the plan's check,
    // so that a file that is not there or not a scene goes straight back, before anyone is asked.
    if let Some(path) = op.text("file") {
        let path = crate::manager_cmd::expand(path, caller.cwd.as_deref());
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
    ws.manager_apply(vec![Action::Scene { scene }], cx);
    ws.reply(r, Reply::ok(format!("Drawing {title} ({}).", plural(steps, "step", "steps")), data));
}

fn manager_voice_token(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let settings = ws.manager_on()?;
    // A token lets a device listen and speak in the manager's room as the person: for the person
    // and their own devices (the voice server asks for the glasses and the phone), never for an
    // agent in a session or the manager's own.
    if caller.trust == Trust::Session {
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

fn follow_up_list(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    ws.manager_on()?;
    let data = &ws.manager.harness.data;
    let list: Vec<&FollowUp> = if op.on("all") { data.followups.iter().collect() } else { followups::open(data) };
    let text = if list.is_empty() { "No follow-ups are open.".to_string() } else { list.iter().map(|f| follow_up_line(f)).collect::<Vec<_>>().join("\n") };
    answer(Reply::ok(text, Value::Array(list.iter().map(|f| to_json(*f)).collect())))
}

fn follow_up_add(ws: &mut Workspace, caller: &Caller, op: &Op) -> Planned {
    let settings = ws.manager_on()?;
    let what_text = arg(op, "what").trim().to_string();
    let person = arg(op, "person").to_string();
    let from = source_of(caller);
    plan(Kind::Board, what(op, &Names { other: settings.name, ..Names::default() }, day::today()), move |ws, r, _, cx| {
        let id = followups::add_custom(&mut ws.manager.harness.data, &what_text, &person, now());
        ws.manager.harness.dirty = true;
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
        let reply = match followups::close(&mut ws.manager.harness.data, &which, dropped, now()) {
            Some(f) => {
                ws.manager.harness.dirty = true;
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
    let list: Vec<&OutMessage> = ws.manager.harness.data.outbox.iter().filter(|m| all || !matches!(m.state, MsgState::Sent | MsgState::Rejected)).collect();
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
        let message = messages::draft(&mut ws.manager.harness.data, &settings, &to, &body, &why, about.as_deref(), now());
        ws.manager.harness.dirty = true;
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
    if caller.trust != Trust::You {
        return Err(denied(if allow { "Only the person can allow a message. It waits for them in Glasscode." } else { "Only the person can reject a message. It waits for them in Glasscode." }));
    }
    let message = pick(&ws.manager.harness.data.outbox, arg(op, "message"), |m| &m.id, |m| format!("{} {}", m.to, m.body), "message")?.clone();
    if !matches!(message.state, MsgState::Draft | MsgState::Failed) {
        return Err(failed(format!("That message to {} is {} already.", message.to, message_state(&message))));
    }
    if allow && message.tier == Tier::Never {
        return Err(failed(format!("{} never sends a message like this one. Write to {} yourself.", settings.name, message.to)));
    }
    let edited = op.text("text").map(str::to_string).filter(|_| allow);
    plan(Kind::Board, format!("{} the message to {}", if allow { "allow" } else { "reject" }, message.to), move |ws, r, _, cx| {
        ws.manager_decide_message(&message.id, allow, edited, cx);
        let after = ws.manager.harness.data.outbox.iter().find(|m| m.id == message.id).cloned().unwrap_or(message);
        let text = if allow { format!("Allowed. The message to {} goes out.", after.to) } else { format!("Rejected the message to {}.", after.to) };
        ws.reply(r, Reply::ok(text, to_json(&after)));
    })
}

fn check_list(ws: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    let settings = ws.manager_on()?;
    let checks = &ws.manager.harness.data.checks;
    let text = if checks.is_empty() { format!("{} checks nothing on a schedule.", settings.name) } else { checks.iter().map(check_line).collect::<Vec<_>>().join("\n") };
    answer(Reply::ok(text, to_json(checks)))
}

fn check_add(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    const USAGE: &str = "manager check list | add \"<what>\" --every <minutes> (or --in <minutes>, once) | remove <id>";
    let settings = ws.manager_on()?;
    let name = settings.name;
    let what = arg(op, "what").trim().to_string();
    let (once, word) = match (op.text("every"), op.text("in")) {
        (Some(every), None) => (false, every),
        (None, Some(after)) => (true, after),
        _ => return Err(usage(USAGE)),
    };
    let Some(mins) = minutes(word) else { return Err(usage(format!("“{word}” is not a time: a number of minutes, or 30m, 2h, 1d."))) };
    // Each check is a question to the agent: not more often than every five minutes.
    if !once && mins < 5 {
        return Err(usage("A check runs every 5 minutes at the most."));
    }
    if ws.manager.harness.data.checks.len() >= 50 {
        return Err(failed(format!("{name} has 50 checks already. Remove one first (`glasscode manager check list`).")));
    }
    let when = if once { format!("once, in {}", every(mins * 60).trim_start_matches("every ")) } else { every(mins * 60) };
    plan(Kind::Board, format!("have {name} check “{}” {when}", clip(&what, 80)), move |ws, r, _, cx| {
        // `last` is now: the first look is one interval from here.
        let added = Check { id: new_id(), what: what.clone(), every_secs: mins * 60, last: now(), once };
        let data = to_json(&added);
        ws.manager.harness.data.checks.push(added);
        ws.manager.harness.dirty = true;
        cx.notify();
        ws.reply(r, Reply::ok(format!("{} checks {when}: {what}", ws.state.manager.name), data));
    })
}

fn check_remove(ws: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let settings = ws.manager_on()?;
    let which = arg(op, "which").trim().to_string();
    let found = pick(&ws.manager.harness.data.checks, &which, |c| &c.id, |c| c.what.clone(), "check")?.clone();
    plan(Kind::Board, what(op, &Names { title: found.what.clone(), other: settings.name }, day::today()), move |ws, r, _, cx| {
        // Dropped for good, so a sync with the server's copy does not bring it back.
        glasscode_core::manager::store::drop_check(&mut ws.manager.harness.data, &found.id);
        ws.manager.harness.dirty = true;
        cx.notify();
        ws.reply(r, Reply::ok(format!("No longer checked: {}", clip(&found.what, 120)), to_json(&found)));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
    /// `op_plan`. With the v2 engine its callers decide by `decide()` first (`control_gated`, for
    /// the command and for `run_op`), and return on a refusal before it; with v1, the plan is
    /// checked by Settings › Permissions (`control_run`, `control_rule`) before it runs.
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
            ("manager_cmd.rs", include_str!("manager_cmd.rs")),
            ("today_ui.rs", include_str!("today_ui.rs")),
            ("manager_ui.rs", include_str!("manager_ui.rs")),
        ];
        let calls = |what: &str| -> Vec<&str> { app.iter().flat_map(|(file, text)| std::iter::repeat_n(*file, text.matches(what).count())).collect() };
        assert_eq!(calls(".op_plan("), ["ops_run.rs", "control_ui.rs", "control_ui.rs", "workspace.rs"]);
        // An approved pending action (M21.2) is planned again only after its re-check, which asks
        // the gate again.
        let approved = function(control, "control_decide_pending");
        let (gate, recheck, planned) = (approved.find("self.control_gate(").unwrap(), approved.find(".recheck(").unwrap(), approved.find("self.op_plan(").unwrap());
        assert!(gate < recheck && recheck < planned, "decide, re-check, then plan");
        assert_eq!(calls(".control_plan("), ["control_ui.rs"]);
        // v2: the gate, a return on Never, then the handler.
        let gated = function(control, "control_gated");
        let (gate, never, planned) = (gated.find("self.control_gate(").unwrap(), gated.find("Decision::Never { why }").unwrap(), gated.find("self.op_plan(").unwrap());
        assert!(gate < never && never < planned, "decide, refuse, then plan");
        assert!(gated[never..planned].contains("return;"));
        assert!(function(control, "control_gate").contains("gate::decide("));
        // The command at v2 goes there, before v1's way; so does run_op.
        let handle = function(control, "control_handle_as");
        assert!(handle.find("self.control_handle_v2(").unwrap() < handle.find("self.control_plan(").unwrap());
        assert!(function(control, "control_handle_v2").contains("self.control_gated("));
        let run_op = function(workspace, "run_op");
        assert!(run_op.find("self.control_gated(").unwrap() < run_op.find("self.op_plan(").unwrap());
        // v1: the plan's kind through Settings › Permissions.
        assert!(function(control, "control_run").contains("self.control_rule(&caller, plan.kind)"));
        assert_eq!(calls("fn control_handle_as("), ["control_ui.rs"]);
    }

    #[test]
    fn every_op_has_a_handler() {
        for op in ops::OPS {
            let handled = HANDLERS.iter().chain(crate::ops_v2::HANDLERS).filter(|(name, _)| *name == op.name).count();
            // The v2 engine's ops have theirs in `ops_v2`, or none while they wait for their slice.
            assert!(handled == 1 || (handled == 0 && op.v2_only), "{}: {handled} handlers", op.name);
        }
        for (name, _) in HANDLERS.iter().chain(crate::ops_v2::HANDLERS) {
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
        const OTHER_KINDS: &[(&str, &str)] = &[("worktree.clean", "Read")];
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
            // `answer` and `read_later` plan a read.
            let reads = body.contains("answer(") || body.contains("read_later(");
            assert!(!kinds.is_empty() || starts || reads, "{op} plans nothing");
            if kinds.is_empty() && !starts {
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
        for (file, text) in [("commands.rs", include_str!("commands.rs")), ("commands2.rs", include_str!("commands2.rs")), ("manager_cmd.rs", include_str!("manager_cmd.rs")), ("control_ui.rs", include_str!("control_ui.rs"))] {
            let plans: Vec<&str> = text.match_indices("plan(Kind::").map(|(i, _)| &text[i..i + 16]).collect();
            let expected: &[&str] = if file == "commands.rs" { &["plan(Kind::Start"] } else { &[] };
            assert_eq!(plans, expected, "{file}");
        }
    }

    /// A plan's check runs before anyone is asked: on every way a plan runs (the command with
    /// either engine, and `run_op`), `after_check` comes before the rules, the gate's ask or the run.
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
        let v1 = body("control_handle_as");
        assert!(v1.find("self.after_check(").unwrap() < v1.find("ws.control_run(").unwrap());
        let v2 = body("control_gated");
        assert!(v2.find("self.after_check(").unwrap() < v2.find("ws.control_ask(").unwrap());
        let workspace = include_str!("workspace.rs");
        let run_op = &workspace[workspace.find("pub fn run_op(").unwrap()..];
        assert!(run_op.find("self.after_check(").unwrap() < run_op.find("ws.control_run(").unwrap());
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
        let failed_reply = with_op(failed("No."), Some("board.done"), false);
        let Reply::Done { code: 1, data, .. } = &failed_reply else { panic!("{failed_reply:?}") };
        assert_eq!(*data, json!({ "op": "board.done", "error_code": "failed" }));
        // A registry error keeps its own class and candidates.
        let ambiguous = OpError::new(ErrorCode::Ambiguous { candidates: vec![ops::Candidate { id: "a".into(), title: "A".into(), project: None }] }, "which?");
        let Reply::Done { code, data, text } = with_op(error_reply(&ambiguous), Some("session.show"), false) else { panic!() };
        assert_eq!((code, text.as_str()), (2, "which?"));
        assert_eq!(data["error_code"], json!("ambiguous"));
        assert_eq!(data["candidates"][0]["id"], json!("a"));
        assert_eq!(data["op"], json!("session.show"));
        // A success's data stays as it was with v1 (the command prints it for --json).
        for data in [json!({ "id": "x" }), json!([1, 2]), Value::Null] {
            assert_eq!(with_op(Reply::ok("Done.", data.clone()), Some("board.add"), false).clone_data(), data);
        }
        assert_eq!(with_op(Reply::ok("Done.", json!({ "id": "x" })), Some("board.add"), true).clone_data(), json!({ "id": "x", "op": "board.add" }));
        assert_eq!(with_op(Reply::ok("Done.", json!([1])), Some("session.list"), true).clone_data(), json!([1]));
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
                    let Reply::Done { code, text, .. } = with_op(error_reply(&error), None, false) else { unreachable!() };
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
}
