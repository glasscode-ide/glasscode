//! The work ledger's ops (M32, M33.2): what each piece of work is, where it stands and what makes
//! it done (`work list`, `work show`), and the person's word on it (`work done`, `work drop`,
//! `work reopen`). They are `v2_only`: the ledger runs only with the v2 engine.
//!
//! Done is decided by code from the facts, never by a model (events-14, land-8), so the three
//! that change a piece of work are the person's only (`human_only`): `done` counts it done
//! whatever its conditions say, and `drop` and `reopen` take it off "Needs you" or put it back.
//! The manager reads the ledger, and tells the person what it would mark.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, Noun, OpGroup, OpSpec, Retry, Undo};
use crate::control::Kind;

pub(super) const NOUNS: &[Noun] = &[Noun { path: &["work"], also: &[], unknown: "Unknown work command “{word}”. It has: work list, show, done, drop and reopen." }];

const fn option(name: &'static str, kind: ArgKind, flags: &'static [&'static str], help: &'static str) -> ArgSpec {
    ArgSpec { name, kind, help, at: At::Flag(flags), ..ArgSpec::OPTION }
}

const fn work(missing: &'static str) -> ArgSpec {
    ArgSpec {
        name: "work",
        kind: ArgKind::Word,
        required: true,
        help: "the work: its id (acme/app#42, ENG-42, a branch), <project>:<id> where two projects share one, or a session's id",
        at: At::Word(0),
        missing,
        default: None,
    }
}

const READ: OpSpec = OpSpec { v2_only: true, ..OpSpec::READ };

/// The person's word on a piece of work: the ledger's, on this Mac, taken back by `work reopen`.
const MARK: OpSpec = OpSpec { group: OpGroup::Board, effect: EffectClass::LocalWrite, retry: Retry::Idempotent, undo: Undo::Inverse("work.reopen"), human_only: true, kind_v1: Kind::Board, v2_only: true, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "work.list",
        cli: &["work", "list"],
        aliases: &[""],
        summary: "the work in hand: each piece's state and what makes it done, checked by code from the facts",
        args: &[
            option("all", ArgKind::Switch, &["all"], "work that is done or dropped too"),
            option("project", ArgKind::Project, &["project"], "a project's name: only its work"),
            option("state", ArgKind::Word, &["state"], "only work in this state: open, waiting, stuck, done or dropped"),
        ],
        examples: &["work list [--all] [--project <name>] [--state stuck]"],
        ..READ
    },
    OpSpec {
        name: "work.show",
        cli: &["work", "show"],
        summary: "one piece of work: its state, each done condition and whether it holds, its pull request, ticket and sessions, and what the session says (a claim, not evidence)",
        args: &[ArgSpec { required: false, missing: "", default: Some("this"), ..work("") }],
        examples: &["work show [<work>]"],
        ..READ
    },
    OpSpec {
        name: "work.done",
        cli: &["work", "done"],
        summary: "say a piece of work is done, whatever its conditions say (for you only)",
        args: &[work("Say which: work done <work>.")],
        examples: &["work done <work>"],
        ..MARK
    },
    OpSpec {
        name: "work.drop",
        cli: &["work", "drop"],
        summary: "drop a piece of work: it is not to be done, and leaves Needs you (for you only)",
        args: &[work("Say which: work drop <work>.")],
        examples: &["work drop <work>"],
        ..MARK
    },
    OpSpec {
        name: "work.reopen",
        cli: &["work", "reopen"],
        summary: "take back done or dropped: the work stands where its facts say again (for you only)",
        args: &[work("Say which: work reopen <work>.")],
        examples: &["work reopen <work>"],
        undo: Undo::Inverse("work.done"),
        ..MARK
    },
];
