//! A session's prompt, keys and mode (M33.2; ops-5): answering the question on its screen by the
//! prompt's id and the option's label, stopping the agent, a few keys, and its mode. All are the
//! v2 engine's.
//!
//! An answer names the prompt it was decided on (`prompt_id`, from `ui.state` or `session.show`);
//! the screen is read again before each key, and any change fails with `stale_prompt`. Options that
//! grant something for good ("don't ask again", "always") are the person's alone. A caller that is
//! not the person sends Esc, Enter and the arrows only, and a mode is never raised.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, OpGroup, OpHost, OpSpec, Retry};
use crate::control::Kind;

const SESSION: ArgSpec = ArgSpec { name: "session", kind: ArgKind::Session, required: true, help: "the session", at: At::Word(0), missing: "Say which session.", default: None };

/// What types into a session: the base of the entries.
const INPUT: OpSpec = OpSpec { group: OpGroup::SessionInput, effect: EffectClass::SessionInput, retry: Retry::AtMostOnce, host: OpHost::SessionHost, kind_v1: Kind::Send, v2_only: true, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "session.answer",
        cli: &["session", "answer"],
        summary: "answer the question on a session's screen: the prompt's id and the option's label",
        args: &[
            SESSION,
            ArgSpec { name: "prompt", kind: ArgKind::Word, required: true, help: "the prompt's id, from ui state or session show", at: At::Flag(&["prompt"]), missing: "session answer <session> --prompt <id> \"<option>\"", default: None },
            ArgSpec { name: "option", kind: ArgKind::Text, required: true, help: "the option's label, as the prompt shows it", at: At::Rest(1), missing: "session answer <session> --prompt <id> \"<option>\"", default: None },
            ArgSpec { name: "text", kind: ArgKind::Text, help: "what to tell the agent, for an option that asks for it", at: At::Flag(&["text"]), ..ArgSpec::OPTION },
        ],
        examples: &["session answer <session> --prompt <id> \"<option>\" [--text \"<what instead>\"]"],
        ..INPUT
    },
    OpSpec {
        name: "session.interrupt",
        cli: &["session", "interrupt"],
        aliases: &["stop"],
        group: OpGroup::SessionControl,
        retry: Retry::Idempotent,
        summary: "stop what the agent is doing (its Esc)",
        args: &[SESSION],
        examples: &["session interrupt <session>"],
        ..INPUT
    },
    OpSpec {
        name: "session.keys",
        cli: &["session", "keys"],
        summary: "press keys in a session: esc, enter, up, down, left, right, tab (and, for you, any text)",
        args: &[SESSION, ArgSpec { name: "keys", kind: ArgKind::Many, required: true, help: "the keys, in order", at: At::Rest(1), missing: "session keys <session> <key>…", default: None }],
        examples: &["session keys <session> esc|enter|up|down…"],
        ..INPUT
    },
    OpSpec {
        name: "session.mode",
        cli: &["session", "mode"],
        group: OpGroup::SessionControl,
        effect: EffectClass::LocalWrite,
        retry: Retry::Verifiable,
        summary: "a Claude session's mode: plan, default or accept-edits; never one that asks less than now",
        args: &[SESSION, ArgSpec { name: "mode", kind: ArgKind::Word, required: true, help: "plan, default or accept-edits", at: At::Word(1), missing: "session mode <session> plan|default|accept-edits", default: None }],
        examples: &["session mode <session> plan|default|accept-edits"],
        ..INPUT
    },
];
