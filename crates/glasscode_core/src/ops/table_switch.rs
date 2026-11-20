//! The server-sync plan's switch ops (`docs/server-sync-plan.md`, "Op names"), which M58 calls to
//! move worktrees and their sessions between the Mac and the server. They are specs only: their
//! handlers come with that plan, and `AppHost` alone implements them (`DaemonHost` refuses them
//! with `Unavailable { host: SessionHost, queued: false }`). They are `v2_only`, so with the v1
//! engine their words stay unknown, as they always were.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, Noun, OpHost, OpSpec, Retry, Undo};
use super::table_project::{WORKTREES, worktree};
use crate::control::Kind;

pub(super) const NOUNS: &[Noun] = &[Noun { path: &["switch"], also: &[], unknown: "Unknown switch command “{word}”. `glasscode help` lists them." }];

const fn option(name: &'static str, kind: ArgKind, flags: &'static [&'static str], help: &'static str) -> ArgSpec {
    ArgSpec { name, kind, help, at: At::Flag(flags), ..ArgSpec::OPTION }
}

const TO: ArgSpec = option("to", ArgKind::Word, &["to"], "server or mac: which way (else away from where the manager's home is)");

const MOVE: OpSpec = OpSpec { effect: EffectClass::Create, retry: Retry::Verifiable, undo: Undo::Inverse("worktree.move"), kind_v1: Kind::Move, v2_only: true, ..WORKTREES };

const READ: OpSpec = OpSpec { host: OpHost::SessionHost, v2_only: true, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "switch.units",
        cli: &["switch", "units"],
        summary: "the units a switch moves: each worktree with its sessions, its owner and epoch, and the worktrees with no session",
        args: &[TO],
        examples: &["switch units [--to server|mac]"],
        ..READ
    },
    OpSpec {
        name: "switch.check",
        cli: &["switch", "check"],
        summary: "what stops each unit from moving: an old server, a missing project, an account not signed in, a working agent, a shell that restarts",
        args: &[TO],
        examples: &["switch check [--to server|mac]"],
        ..READ
    },
    OpSpec {
        name: "switch.move",
        cli: &["switch", "move"],
        summary: "move every unit with an open session, and the ticked ones, several at a time",
        args: &[
            ArgSpec { required: true, missing: "switch move --to server|mac [--unit <worktree>]… [--parallel 4] [--now]", ..TO },
            option("units", ArgKind::Many, &["unit"], "a worktree that moves too, though it has no open session; may be given more than once"),
            ArgSpec { name: "parallel", kind: ArgKind::Count, help: "how many move at a time", at: At::Flag(&["parallel"]), default: Some("4"), ..ArgSpec::OPTION },
            option("now", ArgKind::Switch, &["now"], "interrupt the sessions that are slow to reach a safe point"),
        ],
        examples: &["switch move --to server|mac [--unit <worktree>]… [--parallel 4] [--now]"],
        ..MOVE
    },
    OpSpec {
        name: "worktree.move",
        cli: &["worktree", "move"],
        summary: "move one worktree, with all its sessions, under its owner record",
        args: &[worktree("worktree move <worktree> --to server|mac"), ArgSpec { required: true, missing: "worktree move <worktree> --to server|mac", ..TO }],
        examples: &["worktree move <worktree> --to server|mac"],
        ..MOVE
    },
    OpSpec {
        name: "worktree.owner",
        cli: &["worktree", "owner"],
        summary: "who owns a worktree now: { owner, epoch, moving }; the probe for a move that may have happened",
        args: &[worktree("worktree owner <worktree>")],
        examples: &["worktree owner <worktree>"],
        v2_only: true,
        ..OpSpec::READ
    },
];
