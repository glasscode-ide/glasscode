//! The ops of what waits for the person (M21.2): its list, and its answer. Both are the person's
//! only (`human_only`): the gate refuses them to every caller but the app's window, and a shell
//! tab confirms them there. They come with the v2 engine; with v1, `glasscode approvals` lists
//! what waits, as before.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, Noun, OpGroup, OpHost, OpSpec, Retry, Undo};
use crate::control::Kind;

pub(super) const NOUNS: &[Noun] = &[Noun { path: &["approval"], also: &[], unknown: "Unknown approval command “{word}”. It has: approval list, approval decide <n> allow|deny." }];

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "approvals.list",
        cli: &["approval", "list"],
        aliases: &[""],
        host: OpHost::Window,
        summary: "what waits for you: each action with every argument, who asks and why, and how long it waits (for you only)",
        examples: &["approval list"],
        human_only: true,
        v2_only: true,
        ..OpSpec::READ
    },
    OpSpec {
        name: "approvals.decide",
        cli: &["approval", "decide"],
        group: OpGroup::Settings,
        effect: EffectClass::LocalWrite,
        retry: Retry::Idempotent,
        undo: Undo::None,
        host: OpHost::Window,
        kind_v1: Kind::Settings,
        summary: "approve or deny what waits: it runs once, with exactly its stored arguments, if they still hold (for you only)",
        args: &[
            ArgSpec { name: "id", kind: ArgKind::Word, required: true, help: "its number, or its id", at: At::Word(0), missing: "Say which: approval decide <n> allow|deny.", default: None },
            ArgSpec { name: "answer", kind: ArgKind::Word, required: true, help: "allow or deny", at: At::Word(1), missing: "Say allow or deny: approval decide <n> allow|deny.", default: None },
        ],
        examples: &["approval decide <n> allow|deny"],
        human_only: true,
        v2_only: true,
        ..OpSpec::READ
    },
];
