//! The journal's ops (M14): `glasscode events`, which lists the journal's events and with
//! `--follow` goes on printing new ones, and `journal query`, the same read for typed callers. Both
//! answer within the caller's scope (`journal::Scope`): a session its own project's events, the
//! person everything. They are `v2_only`: the journal runs only in a v2 home, so with the v1
//! engine their words stay unknown, as they always were.
//!
//! `glasscode report done|blocked|needs-review "<line>"` (M30) is what a session says about its
//! own work: kept in the journal as a hint, never as evidence (events-23).
//!
//! `glasscode log` read agent-log.jsonl, which the journal takes the place of: with the v2 engine
//! its words parse to `journal.query` of the actions ([`V2_WORDS`]), and the rows keep the fields
//! of `log --json`.

use super::spec::{ArgKind, ArgSpec, At, Noun, OpSpec};

pub(super) const NOUNS: &[Noun] = &[Noun { path: &["journal"], also: &[], unknown: "Unknown journal command “{word}”. It has one: journal query [--kind <kind>] [--project <p>] [--session <s>] [--since <time>] [--limit 50]." }];

const fn option(name: &'static str, kind: ArgKind, flags: &'static [&'static str], help: &'static str) -> ArgSpec {
    ArgSpec { name, kind, help, at: At::Flag(flags), ..ArgSpec::OPTION }
}

const PROJECT: ArgSpec = option("project", ArgKind::Project, &["project"], "a project's name: only its events");
const SESSION: ArgSpec = option("session", ArgKind::Word, &["session"], "a session's id, or its start: only its events");
const KIND: ArgSpec = option("kind", ArgKind::Word, &["kind"], "a kind (session.started), a family (session) or a prefix (reconciled.); several with commas");
const SINCE: ArgSpec = option("since", ArgKind::Word, &["since"], "from when: 10m, 2h, 3d, or a time in milliseconds since 1970");
const LIMIT: ArgSpec = ArgSpec { name: "limit", kind: ArgKind::Count, help: "how many of the newest", at: At::Flag(&["limit"]), default: Some("50"), ..ArgSpec::OPTION };

const READ: OpSpec = OpSpec { v2_only: true, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "journal.events",
        cli: &["events"],
        summary: "the journal's newest events; with --follow, the new ones as they come",
        args: &[
            option("follow", ArgKind::Switch, &["follow"], "go on printing new events until stopped (Ctrl-C)"),
            PROJECT,
            SESSION,
            KIND,
            SINCE,
            LIMIT,
            option("after", ArgKind::Word, &["after"], "the cursor a reply gave (`data.cursor`): only the events after it"),
        ],
        examples: &["events [--follow] [--kind <kind>] [--project <p>] [--session <s>] [--since 1h] [--limit 50]"],
        ..READ
    },
    OpSpec {
        name: "journal.query",
        cli: &["journal", "query"],
        aliases: &[""],
        summary: "the journal's events that match, newest last, as rows a program reads; with --actions, what Glasscode did for anyone but you, kept a year",
        args: &[
            KIND,
            PROJECT,
            SESSION,
            SINCE,
            LIMIT,
            option("actions", ArgKind::Switch, &["actions"], "the actions Glasscode did for a caller other than you (the effect ledger, kept a year; land-21)"),
            option("pr", ArgKind::Pr, &["pr"], "with --actions: only those about this pull request"),
        ],
        examples: &["journal query [--kind <kind>] [--project <p>] [--session <s>] [--since 1h] [--limit 50]", "journal query --actions [--pr 42] [--project <p>] [--since 30d]"],
        ..READ
    },
    OpSpec {
        name: "session.report",
        cli: &["report"],
        summary: "say how the session's work stands (done, blocked, needs-review): a hint for the manager, never evidence",
        args: &[
            ArgSpec { name: "state", kind: ArgKind::Word, required: true, help: "done, blocked or needs-review", at: At::Word(0), missing: "report done|blocked|needs-review \"<line>\"", ..ArgSpec::OPTION },
            ArgSpec { name: "line", kind: ArgKind::Text, required: true, help: "what is done, or what blocks it, in a line (200 characters are kept)", at: At::Rest(1), missing: "report done|blocked|needs-review \"<line>\"", ..ArgSpec::OPTION },
        ],
        examples: &["report done|blocked|needs-review \"<line>\""],
        ..READ
    },
];

/// v1 ops whose words parse to a v2 op with the v2 engine, and the arguments they then get (the
/// slice that adds the v2 op adds its line here, and drops the op's line of `RENAMED`).
pub(super) const V2_WORDS: &[(&str, &str, &[(&str, &str)])] = &[("app.log", "journal.query", &[("kind", "op,approval")])];
