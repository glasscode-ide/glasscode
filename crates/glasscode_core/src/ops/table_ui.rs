//! What the window shows, for the manager and other typed callers (M33.2; ops-13): `ui.state`
//! (what is on screen, as names and ids, never its text) and `ui.open` (a place, in the window).
//! A caller that is not the person reveals a place and never moves the keyboard, clears what needs
//! the person, or dismisses a notice; `--go` is the person's own going there. `ui point`, `ui say`,
//! `ui tour` and `ui scene` are the manager's showing, speaking and drawing of v1 under the `ui`
//! words, with its arguments. All are the v2 engine's.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, OpGroup, OpHost, OpSpec, Undo};
use super::table_manager::{SAY_ARGS, SCENE_ARGS, SHOW_ARGS};
use crate::control::Kind;

pub(super) const NOUNS: &[super::spec::Noun] = &[super::spec::Noun { path: &["ui"], also: &[], unknown: "Unknown ui command “{word}”. It has: ui state, open, back, forward, panel, point, say, tour and scene." }];

/// What changes what the window shows: the base of the entries.
const SHOW: OpSpec = OpSpec { group: OpGroup::Navigate, effect: EffectClass::Navigate, host: OpHost::Window, kind_v1: Kind::Show, v2_only: true, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "ui.state",
        cli: &["ui", "state"],
        // `glasscode ui` alone is the same.
        aliases: &[""],
        host: OpHost::Window,
        summary: "what the window shows: the view, the session, the side panel and its tab, what is open, what waits, the prompt on screen; names and ids, never the screen's text",
        examples: &["ui state"],
        v2_only: true,
        ..OpSpec::READ
    },
    OpSpec {
        name: "ui.open",
        cli: &["ui", "open"],
        undo: Undo::Inverse("ui.open"),
        summary: "show a place in the window: a view, a section of Settings, a tab of the side panel, or a session",
        args: &[
            ArgSpec { name: "place", kind: ArgKind::Word, required: true, help: "today, sessions, tickets, pull-requests, usage, settings, manager, settings:<section>, tab:<pr|checks|changes|files|glasscode> or a session's id", at: At::Word(0), missing: "ui open <place>", default: None },
            ArgSpec { name: "go", kind: ArgKind::Switch, help: "the person's own: take the keyboard there and mark it seen", at: At::Flag(&["go"]), ..ArgSpec::OPTION },
        ],
        examples: &["ui open settings:automation [--go]"],
        ..SHOW
    },
    OpSpec { name: "ui.back", cli: &["ui", "back"], undo: Undo::Inverse("ui.forward"), summary: "back to the place before", examples: &["ui back"], ..SHOW },
    OpSpec { name: "ui.forward", cli: &["ui", "forward"], undo: Undo::Inverse("ui.back"), summary: "forward again", examples: &["ui forward"], ..SHOW },
    OpSpec {
        name: "ui.panel",
        cli: &["ui", "panel"],
        undo: Undo::Inverse("ui.panel"),
        summary: "show or hide the left sidebar or the side panel",
        args: &[
            ArgSpec { name: "side", kind: ArgKind::Word, required: true, help: "left or right", at: At::Word(0), missing: "ui panel left|right [--hide]", default: None },
            ArgSpec { name: "hide", kind: ArgKind::Switch, help: "hide it (else show it)", at: At::Flag(&["hide"]), ..ArgSpec::OPTION },
        ],
        examples: &["ui panel left|right [--hide]"],
        ..SHOW
    },
    OpSpec { name: "ui.point", cli: &["ui", "point"], summary: "point the person at a session, a part of its panel, or a screen", args: SHOW_ARGS, examples: &["ui point <session> [--tab pr|changes|files] [--section <part>] [--say \"<text>\"]"], ..SHOW },
    OpSpec { name: "ui.say", cli: &["ui", "say"], retry: super::spec::Retry::AtMostOnce, summary: "say something to the person", args: SAY_ARGS, examples: &["ui say \"<text>\""], ..SHOW },
    OpSpec { name: "ui.tour", cli: &["ui", "tour"], summary: "walk the person through today's update in the window", examples: &["ui tour"], ..SHOW },
    OpSpec { name: "ui.scene", cli: &["ui", "scene"], summary: "draw a small picture to explain with (or --file <path>)", args: SCENE_ARGS, examples: &["ui scene '<json>'"], ..SHOW },
];
