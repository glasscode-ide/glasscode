//! The words ledger's debug op (M13.1): what the ledger kept lately, with each record's origin. It
//! is for the person only: the ledger is the person's text, and its folder is denied to sessions.

use super::spec::{ArgKind, ArgSpec, At, Noun, OpSpec};

pub(super) const NOUNS: &[Noun] = &[Noun { path: &["words"], also: &[], unknown: "Unknown words command “{word}”. It has one: words recent [--limit 20]." }];

pub(super) const OPS: &[OpSpec] = &[OpSpec {
    name: "words.recent",
    cli: &["words", "recent"],
    // `glasscode words` alone is the same.
    aliases: &[""],
    summary: "the newest records of the words ledger and where each came from (a debug op, for you only)",
    args: &[ArgSpec { name: "limit", kind: ArgKind::Count, help: "how many of the newest", at: At::Flag(&["limit"]), default: Some("20"), ..ArgSpec::OPTION }],
    examples: &["words recent [--limit 20]"],
    human_only: true,
    ..OpSpec::READ
}];
