//! What takes an op back (M33.2; ops-18): the inverse op its spec declares (`Undo::Inverse`) and
//! the arguments that call needs, made from the op's own arguments and what its reply said. The
//! app puts it in a reply's `data.undo` with the engine at v2, so the chat's Undo button and the
//! journal have the call to make; nothing here runs it.
//!
//! Only the trivial inverses are made (ops-18): a session or worktree that was made is closed or
//! removed, a task's tick or pin is put back, a name, a status, a label or a setting goes back to
//! what it was. A move between machines (`session.move`, `worktree.move`) is not taken back this
//! way. A reply that made nothing new (`"created": false`) has nothing to take back.

use serde_json::{Value, json};

use super::{Op, Undo};

fn call(op: &str, args: Value) -> Option<Value> {
    Some(json!({ "op": op, "args": args }))
}

/// The inverse call of `op`, whose reply's data is `data`; none when its spec declares none, or
/// the reply does not say what to take back.
pub fn undo_of(op: &Op, data: &Value) -> Option<Value> {
    let Undo::Inverse(inverse) = op.spec.undo else { return None };
    // A handler that knows its inverse best says it (`ui.open`'s place before, `settings.set`'s
    // value before).
    if data.get("undo").is_some_and(Value::is_object) {
        return data.get("undo").cloned();
    }
    if data.get("created") == Some(&Value::Bool(false)) {
        return None;
    }
    let text = |name: &str| op.text(name).map(str::to_string);
    let field = |name: &str| data.get(name).and_then(Value::as_str).map(str::to_string);
    match op.name() {
        "board.done" | "board.undo" | "board.snooze" | "board.pin" | "board.unpin" => call(inverse, json!({ "task": text("task")? })),
        "board.add" => call(inverse, json!({ "task": field("id")? })),
        "session.new" | "session.child" | "pr.review" | "pr.build" | "ticket.start" => call(inverse, json!({ "session": field("id")? })),
        "session.rename" => call(inverse, json!({ "session": text("session")?, "name": field("before")? })),
        "pr.label" => {
            let number = op.number("number")?;
            match (text("add"), text("remove")) {
                (Some(label), _) => call(inverse, json!({ "number": number, "remove": label })),
                (_, Some(label)) => call(inverse, json!({ "number": number, "add": label })),
                _ => None,
            }
        }
        "ticket.move" => call(inverse, json!({ "ticket": text("ticket")?, "to": field("before")? })),
        "project.import" => call(inverse, json!({ "project": field("id")? })),
        "project.set_account" => call(inverse, json!({ "project": text("project")?, "github": field("before")? })),
        "worktree.new" => call(inverse, json!({ "worktree": data.get("path").and_then(Value::as_str)? })),
        "worktree.rename" => call(inverse, json!({ "worktree": text("branch")?, "branch": field("before")? })),
        "manager.remember" | "manager.follow_up.add" | "manager.check.add" => call(inverse, json!({ "which": field("id")? })),
        "manager.message.draft" => call(inverse, json!({ "message": field("id")? })),
        "ui.back" | "ui.forward" => call(inverse, json!({})),
        // The work by the name its reply gave (`<project>:<id>`), which names one piece of work.
        "work.done" | "work.drop" => call(inverse, json!({ "work": field("id")? })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{OPS, typed};

    /// A reply's data for each op that declares an inverse, as its handler answers.
    fn reply_of(op: &str) -> Value {
        match op {
            "board.add" => json!({ "id": "zysg", "item": {} }),
            "session.new" | "session.child" | "pr.review" | "pr.build" | "ticket.start" => json!({ "id": "8765f51a-8ad5-4e73-9df0-27adaa817aa9" }),
            "session.rename" => json!({ "id": "8765f51a-8ad5-4e73-9df0-27adaa817aa9", "title": "New", "before": "Old" }),
            "ticket.move" => json!({ "ticket": "ENG-42", "status": "In Review", "before": "Todo" }),
            "project.import" => json!({ "id": "p-app" }),
            "project.set_account" => json!({ "id": "p-app", "before": "acme-bot" }),
            "worktree.new" => json!({ "path": "/wt/app/feat-x", "branch": "feat/x" }),
            "worktree.rename" => json!({ "path": "/wt/app/feat-x", "branch": "feat/y", "before": "feat/x" }),
            "manager.remember" | "manager.follow_up.add" | "manager.check.add" | "manager.message.draft" => json!({ "id": "n1" }),
            "ui.open" => json!({ "undo": { "op": "ui.open", "args": { "place": "today" } } }),
            "ui.panel" => json!({ "undo": { "op": "ui.panel", "args": { "side": "left", "hide": true } } }),
            "settings.set" => json!({ "undo": { "op": "settings.set", "args": { "key": "appearance.theme", "value": "\"auto\"" } } }),
            "work.done" | "work.drop" => json!({ "id": "app-3f9a1c:ENG-42", "state": { "state": "done" } }),
            "work.reopen" => json!({ "id": "app-3f9a1c:ENG-42", "undo": { "op": "work.done", "args": { "work": "app-3f9a1c:ENG-42" } } }),
            _ => json!({}),
        }
    }

    /// The op's own arguments, as a typed caller sends them.
    fn args_of(op: &str) -> Value {
        match op {
            "board.done" | "board.undo" | "board.snooze" | "board.pin" | "board.unpin" => json!({ "task": "zysg" }),
            "board.add" => json!({ "title": "x" }),
            "session.rename" => json!({ "session": "8765f51a-8ad5-4e73-9df0-27adaa817aa9", "name": "New" }),
            "pr.review" | "pr.build" => json!({ "number": 12 }),
            "ticket.start" => json!({ "ticket": "ENG-42" }),
            "session.child" => json!({ "task": "Find where login fails." }),
            "pr.label" => json!({ "number": 12, "add": "preview" }),
            "ticket.move" => json!({ "ticket": "ENG-42", "to": "In Review" }),
            "project.import" => json!({ "source": "/code/app" }),
            "project.set_account" => json!({ "project": "app", "github": "acme" }),
            "worktree.new" => json!({ "branch": "feat/x" }),
            "worktree.rename" => json!({ "worktree": "feat/x", "branch": "feat/y" }),
            "manager.remember" => json!({ "fact": "x" }),
            "manager.follow_up.add" | "manager.check.add" => json!({ "what": "x" }),
            "manager.message.draft" => json!({ "to": "Sam", "text": "x" }),
            "ui.open" => json!({ "place": "usage" }),
            "ui.panel" => json!({ "side": "left" }),
            "session.move" => json!({ "session": "8765f51a-8ad5-4e73-9df0-27adaa817aa9", "to_server": true }),
            "switch.move" => json!({ "to": "server" }),
            "worktree.move" => json!({ "worktree": "feat/x", "to": "server" }),
            "settings.set" => json!({ "key": "appearance.theme", "value": "dark" }),
            "work.done" | "work.drop" | "work.reopen" => json!({ "work": "ENG-42" }),
            _ => json!({}),
        }
    }

    /// Each op that declares an inverse gives a call to it that the inverse's schema takes, but for
    /// the moves between machines (ops-18).
    #[test]
    fn each_inverse_round_trips_through_its_schema() {
        const NOT_TAKEN_BACK: &[&str] = &["session.move", "switch.move", "worktree.move"];
        for spec in OPS {
            let Undo::Inverse(inverse) = spec.undo else { continue };
            let op = typed(spec.name, args_of(spec.name)).unwrap_or_else(|e| panic!("{}: {}", spec.name, e.text));
            let undo = undo_of(&op, &reply_of(spec.name));
            if NOT_TAKEN_BACK.contains(&spec.name) {
                assert!(undo.is_none(), "{}", spec.name);
                continue;
            }
            let undo = undo.unwrap_or_else(|| panic!("{} gives no undo", spec.name));
            assert_eq!(undo["op"], json!(inverse), "{}", spec.name);
            typed(inverse, undo["args"].clone()).unwrap_or_else(|e| panic!("{}'s undo {undo}: {}", spec.name, e.text));
        }
    }

    #[test]
    fn what_made_nothing_takes_nothing_back() {
        let op = typed("pr.review", json!({ "number": 12 })).unwrap();
        assert_eq!(undo_of(&op, &json!({ "id": "s1", "created": false })), None);
        let read = typed("session.list", json!({})).unwrap();
        assert_eq!(undo_of(&read, &json!({})), None);
        let label = typed("pr.label", json!({ "number": 12, "remove": "preview" })).unwrap();
        assert_eq!(undo_of(&label, &json!(null)), Some(json!({ "op": "pr.label", "args": { "number": 12, "add": "preview" } })));
    }
}
