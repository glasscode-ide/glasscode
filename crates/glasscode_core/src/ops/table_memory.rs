//! Memory's ops (M20.2): Glasscode's trusted memory (`memstore`), for the person, the manager and
//! sessions alike. They are `v2_only`: with the v1 engine `glasscode memory …` stays an unknown
//! command, and the v1 manager's notes (`manager remember`) are unchanged (rule 2).
//!
//! Who may do what: the gate decides (`gate::decide`, the Memory group), and these are
//! `human_only`: accepting or rejecting a proposal or a pending edit, forgetting, and listing what
//! is pending (that is text nobody vetted). A session's note goes to its own work file; a lasting
//! fact from anyone but the person is a proposal that waits for their tick.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, Noun, OpGroup, OpSpec, Retry, Undo};
use crate::control::Kind;

pub(super) const NOUNS: &[Noun] = &[Noun { path: &["memory"], also: &["mem"], unknown: "Unknown memory command “{word}”. They are: search, show, note, propose, accept, reject, merge, let-go, rewrite, forget, pending, brief." }];

const fn option(name: &'static str, kind: ArgKind, flags: &'static [&'static str], help: &'static str) -> ArgSpec {
    ArgSpec { name, kind, help, at: At::Flag(flags), ..ArgSpec::OPTION }
}

const fn word(name: &'static str, help: &'static str, missing: &'static str) -> ArgSpec {
    ArgSpec { name, kind: ArgKind::Word, required: true, help, at: At::Word(0), missing, default: None }
}

const PROJECT: ArgSpec = option("project", ArgKind::Project, &["project"], "the project (else the caller's)");

/// What only reads memory: a read, as the gate counts it.
const READ: OpSpec = OpSpec { v2_only: true, ..OpSpec::READ };

/// What changes memory: one commit each.
const WRITE: OpSpec = OpSpec { group: OpGroup::Memory, effect: EffectClass::LocalWrite, retry: Retry::Idempotent, kind_v1: Kind::Board, v2_only: true, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "memory.search",
        cli: &["memory", "search"],
        aliases: &["find"],
        summary: "search memory: the sections that answer, best first, as data (your work, then your project, then the person and the org)",
        args: &[
            ArgSpec { name: "query", kind: ArgKind::Text, required: true, help: "what to look for", at: At::Rest(0), missing: "memory search \"<words>\" [--project <name>] [--limit 5]", default: None },
            PROJECT,
            ArgSpec { name: "limit", kind: ArgKind::Count, help: "how many sections", at: At::Flag(&["limit"]), default: Some("5"), ..ArgSpec::OPTION },
            option("history", ArgKind::Switch, &["history"], "entries let go too"),
        ],
        examples: &["memory search \"<words>\" [--project <name>] [--limit 5]"],
        ..READ
    },
    OpSpec {
        name: "memory.show",
        cli: &["memory", "show"],
        summary: "an entry by its id, or a memory file by its path, as data",
        args: &[word("what", "an entry's id, or a file such as projects/<key>/decisions.md", "memory show <id or file>")],
        examples: &["memory show <id or file>"],
        ..READ
    },
    OpSpec {
        name: "memory.brief",
        cli: &["memory", "brief"],
        summary: "the memory brief a session is given (--hook: as a SessionStart hook's output)",
        args: &[option("session", ArgKind::Session, &["session"], "whose brief (else the caller's)"), option("hook", ArgKind::Switch, &["hook"], "print it as a SessionStart hook's JSON")],
        examples: &["memory brief [--session <id>] [--hook]"],
        ..READ
    },
    OpSpec {
        name: "memory.note",
        cli: &["memory", "note"],
        summary: "note something about the work in its work file (\"decided to keep the v1 API\")",
        args: &[
            ArgSpec { name: "text", kind: ArgKind::Text, required: true, help: "the note", at: At::Rest(0), missing: "memory note \"<text>\" [--session <id>]", default: None },
            option("session", ArgKind::Session, &["session"], "whose work (else the caller's)"),
        ],
        examples: &["memory note \"<text>\" [--session <id>]"],
        ..WRITE
    },
    OpSpec {
        name: "memory.propose",
        cli: &["memory", "propose"],
        summary: "propose a lasting fact or know-how; it waits for the person's tick",
        args: &[
            ArgSpec { name: "text", kind: ArgKind::Text, required: true, help: "the fact", at: At::Rest(0), missing: "memory propose \"<text>\" [--project <name>] [--file project|decisions|policy|profile|knowhow:<topic>] [--section <heading>]", default: None },
            PROJECT,
            option("file", ArgKind::Word, &["file"], "where it goes: project, decisions (the default), policy, profile, people, glossary, or knowhow:<topic>"),
            option("section", ArgKind::Word, &["section"], "the heading it goes under"),
        ],
        examples: &["memory propose \"<text>\" [--project <name>] [--file project|decisions|policy|profile|knowhow:<topic>] [--section <heading>]"],
        ..WRITE
    },
    OpSpec {
        name: "memory.accept",
        cli: &["memory", "accept"],
        summary: "accept a proposal (by its id) or a pending edit (by its file)",
        args: &[word("what", "a proposal's id, or a file with a pending edit", "memory accept <proposal id or file>")],
        examples: &["memory accept <proposal id or file>"],
        human_only: true,
        ..WRITE
    },
    OpSpec {
        name: "memory.reject",
        cli: &["memory", "reject"],
        aliases: &["discard"],
        summary: "reject a proposal (by its id) or discard a pending edit (by its file)",
        args: &[word("what", "a proposal's id, or a file with a pending edit", "memory reject <proposal id or file>")],
        examples: &["memory reject <proposal id or file>"],
        human_only: true,
        ..WRITE
    },
    OpSpec {
        name: "memory.merge",
        cli: &["memory", "merge"],
        summary: "merge entries of one file into one (to make room under its cap)",
        args: &[
            ArgSpec { name: "ids", kind: ArgKind::Text, required: true, help: "the entries' ids", at: At::Rest(0), missing: "memory merge <id> <id>… --text \"<words>\"", default: None },
            ArgSpec { name: "text", kind: ArgKind::Text, required: true, help: "the merged words", at: At::Flag(&["text"]), missing: "memory merge <id> <id>… --text \"<words>\"", default: None },
        ],
        examples: &["memory merge <id> <id>… --text \"<words>\""],
        ..WRITE
    },
    OpSpec {
        name: "memory.let_go",
        cli: &["memory", "let-go"],
        aliases: &["let_go"],
        summary: "let an entry go: out of every brief, archived later",
        args: &[word("id", "the entry's id", "memory let-go <id>")],
        examples: &["memory let-go <id>"],
        ..WRITE
    },
    OpSpec {
        name: "memory.rewrite",
        cli: &["memory", "rewrite"],
        summary: "new words for an entry, keeping its id",
        args: &[
            word("id", "the entry's id", "memory rewrite <id> \"<words>\""),
            ArgSpec { name: "text", kind: ArgKind::Text, required: true, help: "the new words", at: At::Rest(1), missing: "memory rewrite <id> \"<words>\"", default: None },
        ],
        examples: &["memory rewrite <id> \"<words>\""],
        ..WRITE
    },
    OpSpec {
        name: "memory.forget",
        cli: &["memory", "forget"],
        summary: "forget entries for good (--secret: and rebuild memory's history so the text is in no copy)",
        args: &[
            ArgSpec { name: "ids", kind: ArgKind::Text, required: true, help: "the entries' ids", at: At::Rest(0), missing: "memory forget <id>… [--secret]", default: None },
            option("secret", ArgKind::Switch, &["secret"], "rebuild the history too"),
        ],
        examples: &["memory forget <id>… [--secret]"],
        human_only: true,
        effect: EffectClass::Destructive,
        retry: Retry::Verifiable,
        undo: Undo::None,
        ..WRITE
    },
    OpSpec {
        name: "memory.pending",
        cli: &["memory", "pending"],
        summary: "what waits for you: proposals, and edits made outside Glasscode with their diffs",
        examples: &["memory pending"],
        human_only: true,
        ..READ
    },
];
