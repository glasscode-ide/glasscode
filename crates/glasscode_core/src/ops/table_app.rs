//! The app's own ops that change something (M12.2): showing a place, notifying the person, and
//! waiting for an approval's result. Summaries and examples are the help's (`help.rs`, which prints them), word for word
//! (a test checks); the words, options and messages are those of `control_plan` in `commands.rs`
//! and `open_plan` in `commands2.rs`, which these replace.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, OpGroup, OpHost, OpSpec, Options, Retry};
use crate::control::Kind;

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "app.open",
        cli: &["open"],
        aliases: &["show"],
        group: OpGroup::Navigate,
        effect: EffectClass::Navigate,
        host: OpHost::Window,
        kind_v1: Kind::Show,
        summary: "show a screen, or a session, in the window",
        args: &[ArgSpec {
            name: "place",
            kind: ArgKind::Word,
            required: true,
            help: "today, tickets, sessions, usage, settings, themes, or a session (its id, its start, or words from its title)",
            at: At::Word(0),
            missing: "open today|tickets|sessions|usage|settings|themes|<session>",
            default: None,
        }],
        examples: &["open today|tickets|sessions|usage|settings|<session>"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "app.notify",
        cli: &["notify"],
        group: OpGroup::Board,
        effect: EffectClass::LocalWrite,
        retry: Retry::AtMostOnce,
        host: OpHost::Window,
        kind_v1: Kind::Board,
        summary: "send the person a notification",
        args: &[
            ArgSpec { name: "message", kind: ArgKind::Text, required: true, help: "what it says", at: At::Rest(0), missing: "notify \"<message>\" [--title \"<title>\"]", default: None },
            ArgSpec { name: "title", kind: ArgKind::Text, help: "its title (else who sent it)", at: At::Flag(&["title"]), ..ArgSpec::OPTION },
        ],
        examples: &["notify \"<message>\" [--title \"<title>\"]"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "app.wait",
        cli: &["wait"],
        host: OpHost::Window,
        summary: "the result of something that waited for you",
        args: &[ArgSpec {
            name: "id",
            kind: ArgKind::Number,
            required: true,
            help: "the number the waiting command printed",
            at: At::Word(0),
            missing: "wait <id>: the number the waiting command printed.",
            default: None,
        }],
        examples: &["wait <id>"],
        // It never looked at its options.
        options: Options::Ignore,
        ..OpSpec::READ
    },
    // M33.2 (ops-10), the v2 engine's: a long op that answered "started" stops when asked, and a
    // request that waits for the person is taken back. Its caller's own, or any for the person; it
    // changes nothing but that, so it is a read for the gate, and the handler says who may.
    OpSpec {
        name: "ops.cancel",
        cli: &["cancel"],
        host: OpHost::Window,
        summary: "stop a long op you started (its id from “started”), or take back your request that waits for the person",
        args: &[ArgSpec { name: "id", kind: ArgKind::Number, required: true, help: "the id it answered with", at: At::Word(0), missing: "cancel <id>: the id the op answered with.", default: None }],
        examples: &["cancel <id>"],
        v2_only: true,
        ..OpSpec::READ
    },
    // M33.2 (ops-18), the v2 engine's: the inverse an op's answer gave (`data.undo`), run as the
    // caller's own call under its own rule. Itself a read for the gate: the inverse is gated.
    OpSpec {
        name: "ops.undo",
        cli: &["undo"],
        host: OpHost::Window,
        summary: "take back an op you made (its undo_id; none: your last) under your own rule, if what it changed has not changed since",
        args: &[ArgSpec { name: "id", kind: ArgKind::Count, required: false, help: "the undo_id its answer gave; none: your last", at: At::Word(0), missing: "", default: None }],
        examples: &["undo [<id>]"],
        v2_only: true,
        ..OpSpec::READ
    },
];
