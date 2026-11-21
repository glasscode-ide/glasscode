//! The person's own devices (G07, `docs/mobile-glasses-plan.md`): what the manager shows on their
//! glasses, and which devices are paired and connected. `device open` puts a place on the lens of
//! the device that asked; only the chat may, in answer to that device's question of the last 30 s
//! (the handler checks it), as `ui open` moves the window only in answer to the person.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, OpGroup, OpHost, OpSpec};
use crate::control::Kind;

pub(super) const NOUNS: &[super::spec::Noun] = &[super::spec::Noun { path: &["device"], also: &["devices"], unknown: "Unknown device command “{word}”. It has: device list and device open." }];

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "device.list",
        cli: &["device", "list"],
        aliases: &[""],
        host: OpHost::Window,
        summary: "the person's paired phones and glasses, and which are connected now",
        examples: &["device list"],
        v2_only: true,
        ..OpSpec::READ
    },
    OpSpec {
        name: "device.open",
        cli: &["device", "open"],
        group: OpGroup::Navigate,
        effect: EffectClass::Navigate,
        host: OpHost::Window,
        kind_v1: Kind::Show,
        summary: "show a place on the person's glasses: a session, a pull request, a project, today or the conversation",
        args: &[ArgSpec {
            name: "place",
            kind: ArgKind::Word,
            required: true,
            help: "a session's id or name, pr:<number>, project:<name>, today or conversation",
            at: At::Word(0),
            missing: "device open <place>",
            default: None,
        }],
        examples: &["device open <session|pr:<number>|project:<name>|today|conversation>"],
        v2_only: true,
        ..OpSpec::READ
    },
];
