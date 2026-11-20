//! Moving in (M35, M54) as ops: the dialog's buttons, so the button, a command typed in a shell tab
//! and a test all press the same op. The person's alone (`human_only`) and `v2_only`: moving in
//! fills v2's memory, and with the v1 engine `glasscode move-in …` stays unknown.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, Noun, OpGroup, OpHost, OpSpec, Retry, Undo};
use crate::control::Kind;

pub(super) const NOUNS: &[Noun] = &[Noun { path: &["move-in"], also: &[], unknown: "Unknown move-in command “{word}”. They are: open, read, accept, drop, undo, study, retry." }];

/// The person's moving in: what only they do.
const MOVEIN: OpSpec = OpSpec { group: OpGroup::Manager, effect: EffectClass::LocalWrite, retry: Retry::Verifiable, host: OpHost::Window, kind_v1: Kind::Settings, human_only: true, v2_only: true, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "movein.open",
        cli: &["move-in", "open"],
        retry: Retry::Idempotent,
        summary: "open moving in: it lists what your agents and editors already know, and reads nothing until you tick it",
        examples: &["move-in open"],
        ..MOVEIN
    },
    OpSpec {
        name: "movein.read",
        cli: &["move-in", "read"],
        summary: "read the ticked sources of moving in and propose what they hold to memory",
        examples: &["move-in read"],
        ..MOVEIN
    },
    OpSpec {
        name: "movein.accept",
        cli: &["move-in", "accept"],
        retry: Retry::Idempotent,
        summary: "accept what moving in proposed: these proposals, or all that wait",
        args: &[ArgSpec { name: "ids", kind: ArgKind::Text, help: "the proposals' ids (none: all that wait)", at: At::Rest(0), ..ArgSpec::OPTION }],
        examples: &["move-in accept [<id>…]"],
        ..MOVEIN
    },
    OpSpec {
        name: "movein.drop",
        cli: &["move-in", "drop"],
        retry: Retry::Idempotent,
        summary: "drop one proposal of moving in",
        args: &[ArgSpec { name: "id", kind: ArgKind::Word, required: true, help: "the proposal's id", at: At::Word(0), missing: "move-in drop <id>", default: None }],
        examples: &["move-in drop <id>"],
        ..MOVEIN
    },
    OpSpec {
        name: "movein.undo",
        cli: &["move-in", "undo"],
        summary: "undo this move-in: what waits leaves, what was accepted is forgotten, and its study and questions go",
        effect: EffectClass::Destructive,
        undo: Undo::None,
        examples: &["move-in undo"],
        ..MOVEIN
    },
    OpSpec {
        name: "movein.study",
        cli: &["move-in", "study"],
        summary: "study the projects: their levels asked first, then one study a project, within the budget",
        examples: &["move-in study"],
        ..MOVEIN
    },
    OpSpec {
        name: "movein.retry",
        cli: &["move-in", "retry"],
        summary: "try a paused study of moving in again now",
        examples: &["move-in retry"],
        ..MOVEIN
    },
];
