//! The app's own ops that change something (M12.2): showing a place, notifying the person, and
//! waiting for an approval's result. Summaries and examples are `cli.rs`'s `HELP`, word for word
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
];
