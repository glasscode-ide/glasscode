//! The Glasscode Browser's ops (M23, D28): what a session's browser tool does, asked of the gate
//! by the browser's MCP server before each call (`browser::mcp`). Reading a page, acting in it
//! (click, type, press, select), and running script in it; acting and script on a site the browser
//! is signed in to ask the person. They come with the v2 engine; with v1 the browser works as
//! before.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, Noun, OpGroup, OpHost, OpSpec, Retry, Undo};
use crate::control::Kind;

pub(super) const NOUNS: &[Noun] = &[Noun { path: &["browser"], also: &[], unknown: "Unknown browser command “{word}”. It has: browser read, act and eval." }];

const URL: ArgSpec = ArgSpec { name: "url", kind: ArgKind::Word, required: true, help: "the page", at: At::Flag(&["url"]), missing: "Say which page: browser <read|act|eval> --url <url> [--signed-in].", default: None };
const SIGNED_IN: ArgSpec = ArgSpec { name: "signed_in", kind: ArgKind::Switch, help: "the browser holds the person's sign-in for the site", at: At::Flag(&["signed-in"]), ..ArgSpec::OPTION };

/// Acting and script. Their v1 kind is Board's, which Settings › Permissions allows by default, so
/// that a session the person started works its tab as it always did; a signed-in site asks (the
/// gate), and the manager's sessions have no browser at all.
const ACT: OpSpec = OpSpec { group: OpGroup::Browser, effect: EffectClass::Outward, retry: Retry::AtMostOnce, undo: Undo::None, host: OpHost::Window, kind_v1: Kind::Board, v2_only: true, args: &[URL, SIGNED_IN], ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "browser.read",
        cli: &["browser", "read"],
        summary: "may this session read a page in its tab of the Glasscode Browser",
        args: &[URL, SIGNED_IN],
        examples: &["browser read --url <url> [--signed-in]"],
        host: OpHost::Window,
        v2_only: true,
        ..OpSpec::READ
    },
    OpSpec { name: "browser.act", cli: &["browser", "act"], summary: "may this session click, type or press keys in a page of its tab", examples: &["browser act --url <url> [--signed-in]"], ..ACT },
    OpSpec { name: "browser.eval", cli: &["browser", "eval"], summary: "may this session run script in a page of its tab", examples: &["browser eval --url <url> [--signed-in]"], ..ACT },
];
