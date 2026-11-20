//! Moving in's buttons as ops (`movein.*`), and the chat's topics (`ui.topic`): the button, a
//! command typed in a shell tab and the journeys (`script/journeys.sh`) press the same op. Each
//! op checks that the dialog is where its button is shown, and says so when it is not.

use glasscode_core::control::{Kind, Reply};
use glasscode_core::ops::Op;
use serde_json::json;

use crate::control_ui::Caller;
use crate::movein_ui::Stage;
use crate::ops_run::{Handler, Planned, plan};
use crate::workspace::Workspace;

pub(crate) const HANDLERS: &[(&str, Handler)] = &[
    ("movein.open", movein_open),
    ("movein.read", movein_read),
    ("movein.accept", movein_accept),
    ("movein.drop", movein_drop),
    ("movein.undo", movein_undo),
    ("movein.study", movein_study),
    ("movein.retry", movein_retry),
    ("ui.topic", ui_topic),
];

/// The stage moving in is at, in words.
fn stage_name(ws: &Workspace) -> &'static str {
    match ws.movein.as_ref().map(|f| f.stage) {
        None => "closed",
        Some(Stage::Looking) => "looking",
        Some(Stage::Consent) => "waiting for you to tick its sources",
        Some(Stage::Reading) => "reading",
        Some(Stage::Review) => "showing what it proposes",
        Some(Stage::Study) => "studying",
    }
}

fn not_now(ws: &Workspace, what: &str) -> Reply {
    Reply::fail(glasscode_core::control::code::FAILED, format!("Moving in cannot {what} now: it is {}.", stage_name(ws)))
}

fn movein_open(_: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    plan(Kind::Settings, "open moving in", |ws, r, _, cx| {
        ws.open_move_in(cx);
        ws.reply(r, Reply::ok("Moving in is open: it lists what your agents and editors already know, and reads nothing until you tick it.", json!({ "stage": stage_name(ws) })));
    })
}

fn movein_read(_: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    plan(Kind::Settings, "read the ticked sources of moving in", |ws, r, _, cx| {
        let ticked = ws.movein.as_ref().filter(|f| f.stage == Stage::Consent && !f.busy).map(|f| f.found.iter().filter(|x| x.ticked).count());
        match ticked {
            Some(n) if n > 0 => {
                ws.move_in_read(cx);
                ws.reply(r, Reply::ok(format!("Reading {n} ticked source{}; nothing becomes memory until you accept it.", if n == 1 { "" } else { "s" }), json!({ "sources": n })));
            }
            Some(_) => ws.reply(r, Reply::fail(glasscode_core::control::code::FAILED, "No source is ticked: tick what moving in may read first.")),
            None => {
                let reply = not_now(ws, "read");
                ws.reply(r, reply);
            }
        }
    })
}

fn movein_accept(_: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let asked: Vec<String> = op.text("ids").unwrap_or_default().split_whitespace().map(str::to_string).collect();
    plan(Kind::Settings, "accept what moving in proposed", move |ws, r, _, cx| {
        let Some(form) = ws.movein.as_ref().filter(|f| matches!(f.stage, Stage::Review | Stage::Study)) else {
            let reply = not_now(ws, "accept");
            return ws.reply(r, reply);
        };
        let waiting: Vec<String> = form.waiting.iter().map(|(id, ..)| id.clone()).collect();
        let ids: Vec<String> = if asked.is_empty() { waiting.clone() } else { asked.clone() };
        if let Some(unknown) = ids.iter().find(|id| !waiting.contains(id)) {
            return ws.reply(r, Reply::fail(glasscode_core::control::code::FAILED, format!("{unknown} is no proposal of moving in that waits.")));
        }
        if ids.is_empty() {
            return ws.reply(r, Reply::ok("Nothing of moving in waits to be accepted.", json!({ "accepted": 0 })));
        }
        let n = ids.len();
        ws.move_in_accept(ids, cx);
        ws.reply(r, Reply::ok(format!("Accepting {n} proposal{} of moving in.", if n == 1 { "" } else { "s" }), json!({ "accepted": n })));
    })
}

fn movein_drop(_: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let id = op.text("id").unwrap_or_default().trim().to_string();
    plan(Kind::Settings, format!("drop {id} of moving in"), move |ws, r, _, cx| {
        let waits = ws.movein.as_ref().is_some_and(|f| f.waiting.iter().any(|(w, ..)| *w == id));
        if !waits {
            return ws.reply(r, Reply::fail(glasscode_core::control::code::FAILED, format!("{id} is no proposal of moving in that waits.")));
        }
        ws.move_in_drop(id.clone(), cx);
        ws.reply(r, Reply::ok(format!("Dropped {id}."), json!({ "dropped": id })));
    })
}

fn movein_undo(_: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    plan(Kind::Settings, "undo this move-in", |ws, r, _, cx| {
        if !ws.movein.as_ref().is_some_and(|f| matches!(f.stage, Stage::Review | Stage::Study)) {
            let reply = not_now(ws, "undo");
            return ws.reply(r, reply);
        }
        ws.move_in_undo(cx);
        ws.reply(r, Reply::ok("This move-in is undone: what waited left, what was accepted is forgotten, and its study and questions went.", json!({ "undone": true })));
    })
}

fn movein_study(_: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    plan(Kind::Settings, "study the projects as part of moving in", |ws, r, _, cx| {
        let ready = ws.movein.as_ref().is_some_and(|f| f.stage == Stage::Review && !f.projects.is_empty());
        if !ready {
            let reply = not_now(ws, "study");
            return ws.reply(r, reply);
        }
        ws.move_in_study_start(cx);
        ws.reply(r, Reply::ok("Studying your projects: their levels are asked first, in the chat's topic “Moving in”.", json!({ "studying": true })));
    })
}

fn movein_retry(_: &mut Workspace, _: &Caller, _: &Op) -> Planned {
    plan(Kind::Settings, "try a paused study again", |ws, r, _, cx| {
        let paused = ws.movein.as_ref().is_some_and(|f| f.stage == Stage::Study && f.progress.as_ref().is_some_and(|p| p.paused.is_some()));
        if !paused {
            return ws.reply(r, Reply::fail(glasscode_core::control::code::FAILED, "No study of moving in is paused."));
        }
        ws.move_in_study_step(cx);
        ws.reply(r, Reply::ok("The study goes on.", json!({ "studying": true })));
    })
}

/// `ui topic <name>|all`: the conversation on the Manager page shows that topic (by its name, in
/// any case, or its id), or all of them, as its chip does.
fn ui_topic(_: &mut Workspace, _: &Caller, op: &Op) -> Planned {
    let asked = op.text("topic").unwrap_or_default().trim().to_string();
    plan(Kind::Show, format!("show the topic {asked}"), move |ws, r, _, cx| {
        if asked.eq_ignore_ascii_case("all") {
            ws.chat_show_topic(None, cx);
            return ws.reply(r, Reply::ok("The conversation shows all its topics.", json!({ "topic": null })));
        }
        let topics = ws.mchat.conversation.topics();
        let found = topics.iter().find(|t| t.id.as_str() == asked || t.title.eq_ignore_ascii_case(&asked)).map(|t| (t.id.clone(), t.title.clone()));
        match found {
            Some((id, title)) => {
                ws.chat_show_topic(Some(id.clone()), cx);
                ws.reply(r, Reply::ok(format!("The conversation shows the topic “{title}”."), json!({ "topic": id.as_str(), "title": title })));
            }
            None => {
                let names: Vec<String> = topics.iter().map(|t| t.title.clone()).collect();
                ws.reply(r, Reply::fail(glasscode_core::control::code::FAILED, format!("No topic “{asked}”. The topics are: {}.", if names.is_empty() { "none yet".to_string() } else { names.join(", ") })));
            }
        }
    })
}
