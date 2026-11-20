//! The ops that work on pull requests and tickets (M12.2): sessions for them, and changes on
//! GitHub and Linear. Summaries and examples are the help's (`help.rs`, which prints them), word for word (a test checks);
//! the words, options and messages are those of `pr_plan`, `pr_change_plan` and `ticket_plan` in
//! `commands.rs` and `commands2.rs`, which these replace.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, OpGroup, OpHost, OpSpec, Retry, Undo};
use super::table_session::{ACCOUNT, AGENT, BRANCH, MESSAGE, MODE, MODEL, ON_SERVER, OPEN, PROJECT, START, THIS_SESSION, TITLE};
use crate::control::Kind;

const fn option(name: &'static str, kind: ArgKind, flags: &'static [&'static str], help: &'static str) -> ArgSpec {
    ArgSpec { name, kind, help, at: At::Flag(flags), ..ArgSpec::OPTION }
}

/// The pull request a session is started for: 958, #958 or its URL.
const PR: ArgSpec = ArgSpec { name: "number", kind: ArgKind::Pr, required: true, help: "958, #958 or its URL", at: At::Word(0), missing: "pr {verb} <number>", default: None };

/// The pull request a change on GitHub is for, by its number only ("#958" is 958), as these
/// commands always read it.
const NUMBER: ArgSpec = ArgSpec { name: "number", kind: ArgKind::Number, required: true, help: "its number: 958 or #958", at: At::Word(0), missing: "pr {verb} <number> …", default: None };

const TICKET: ArgSpec = ArgSpec { name: "ticket", kind: ArgKind::Ticket, required: true, help: "its ID, such as ENG-42", at: At::Word(0), missing: "ticket {verb} <ID>", default: None };

/// A change on GitHub: the base of their entries.
const GITHUB: OpSpec = OpSpec { group: OpGroup::ForgeWrite, effect: EffectClass::Outward, retry: Retry::AtMostOnce, host: OpHost::Network, kind_v1: Kind::Github, ..OpSpec::READ };

/// A change in Linear: the base of their entries.
const LINEAR: OpSpec = OpSpec { group: OpGroup::Tickets, effect: EffectClass::Outward, retry: Retry::AtMostOnce, host: OpHost::Network, kind_v1: Kind::Tickets, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    // ----- sessions for a pull request ------------------------------------------------------
    OpSpec {
        name: "pr.review",
        cli: &["pr", "review"],
        summary: "a review session in its own worktree",
        args: &[PR, PROJECT, AGENT, ACCOUNT, MESSAGE, OPEN, MODEL, MODE, ON_SERVER, TITLE],
        examples: &["pr review <n> [--project <p>] [--agent claude|codex]"],
        ..START
    },
    OpSpec {
        name: "pr.build",
        cli: &["pr", "build"],
        aliases: &["work"],
        summary: "a session on the PR's own branch",
        args: &[PR, PROJECT, AGENT, ACCOUNT, MESSAGE, OPEN, MODEL, MODE, ON_SERVER, TITLE],
        examples: &["pr build <n> [--project <p>] [--message \"<what to do>\"]"],
        ..START
    },
    // ----- changes on GitHub ------------------------------------------------------------------
    OpSpec {
        name: "pr.rerun",
        cli: &["pr", "rerun"],
        summary: "re-run its failed checks",
        args: &[NUMBER, PROJECT],
        examples: &["pr rerun <n> [--project <p>]"],
        ..GITHUB
    },
    OpSpec {
        name: "pr.comment",
        cli: &["pr", "comment"],
        summary: "comment on it",
        args: &[
            NUMBER,
            ArgSpec { name: "text", kind: ArgKind::Text, required: true, help: "the comment", at: At::Rest(1), missing: "pr comment <number> \"<text>\"", default: None },
            PROJECT,
        ],
        examples: &["pr comment <n> \"<text>\""],
        ..GITHUB
    },
    OpSpec {
        name: "pr.label",
        cli: &["pr", "label"],
        retry: Retry::Idempotent,
        undo: Undo::Inverse("pr.label"),
        summary: "put a label on it, or take one off (e.g. the preview label)",
        args: &[NUMBER, PROJECT, option("add", ArgKind::Word, &["add"], "the label to put on"), option("remove", ArgKind::Word, &["remove"], "the label to take off")],
        examples: &["pr label <n> --add <label> | --remove <label>"],
        ..GITHUB
    },
    OpSpec {
        name: "pr.open_draft",
        cli: &["pr", "open-draft"],
        aliases: &["draft"],
        retry: Retry::Verifiable,
        // It pushes from the session's own worktree.
        host: OpHost::SessionHost,
        summary: "push the session's branch and open a draft pull request",
        args: &[THIS_SESSION, option("base", ArgKind::Word, &["base"], "the branch it is for (else the default branch)")],
        examples: &["pr open-draft [<session>]"],
        ..GITHUB
    },
    // ----- tickets ----------------------------------------------------------------------------
    OpSpec {
        name: "ticket.start",
        cli: &["ticket", "start"],
        aliases: &["work"],
        summary: "a session that works on the ticket, on its own branch",
        args: &[TICKET, PROJECT, AGENT, ACCOUNT, MESSAGE, OPEN, MODEL, MODE, BRANCH, TITLE],
        examples: &["ticket start <ID> [--agent …] [--project <p>] [--message \"<what to do>\"]"],
        ..START
    },
    OpSpec {
        name: "ticket.move",
        cli: &["ticket", "move"],
        aliases: &["status"],
        retry: Retry::Idempotent,
        undo: Undo::Inverse("ticket.move"),
        summary: "give it another status in Linear",
        args: &[
            TICKET,
            ArgSpec {
                name: "to",
                kind: ArgKind::Text,
                required: true,
                help: "the status, as its team names it (\"In Review\")",
                at: At::FlagOrRest(&["to"], 1),
                missing: "ticket move <ID> \"<status>\", e.g. \"In Review\"",
                default: None,
            },
        ],
        examples: &["ticket move <ID> \"<status>\""],
        ..LINEAR
    },
    OpSpec {
        name: "ticket.comment",
        cli: &["ticket", "comment"],
        summary: "comment on it in Linear",
        args: &[TICKET, ArgSpec { name: "text", kind: ArgKind::Text, required: true, help: "the comment", at: At::Rest(1), missing: "ticket comment <ID> \"<text>\"", default: None }],
        examples: &["ticket comment <ID> \"<text>\""],
        ..LINEAR
    },
];
