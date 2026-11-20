//! The playbooks' ops (M40; review events-11): `glasscode playbook check`, which loads every
//! playbook against the journal's field schema and says which run and why the others do not, and
//! `glasscode playbook test <id> [--since 7d]`, which runs one against the journal's past events
//! without acting (`playbooks::simulate`). Both only read. They are `v2_only`: the journal and the
//! playbooks run in a v2 home only.

use super::spec::{ArgKind, ArgSpec, At, Noun, OpSpec};

pub(super) const NOUNS: &[Noun] = &[Noun { path: &["playbook"], also: &["playbooks"], unknown: "Unknown playbook command “{word}”. They are: check, test <id> [--since 7d]." }];

const READ: OpSpec = OpSpec { v2_only: true, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "playbook.check",
        cli: &["playbook", "check"],
        summary: "every playbook as this machine reads it: which run, and why the others do not",
        examples: &["playbook check"],
        ..READ
    },
    OpSpec {
        name: "playbook.test",
        cli: &["playbook", "test"],
        summary: "run one playbook against the journal's past events without acting: what it would have done",
        args: &[
            ArgSpec { name: "playbook", kind: ArgKind::Word, required: true, help: "the playbook's id", at: At::Word(0), missing: "playbook test <id> [--since 7d]", default: None },
            ArgSpec { name: "since", kind: ArgKind::Word, help: "how far back: 10m, 2h, 3d (7d at most)", at: At::Flag(&["since"]), default: Some("7d"), ..ArgSpec::OPTION },
        ],
        examples: &["playbook test <id> [--since 7d]"],
        ..READ
    },
];
