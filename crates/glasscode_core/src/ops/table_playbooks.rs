//! The playbooks' ops (M40; review events-11): `glasscode playbook check`, which loads every
//! playbook against the journal's field schema and says which run and why the others do not, and
//! `glasscode playbook test <id> [--since 7d]`, which runs one against the journal's past events
//! without acting (`playbooks::simulate`). Both only read. They are `v2_only`: the journal and the
//! playbooks run in a v2 home only.

use super::spec::{ArgKind, ArgSpec, At, Noun, OpSpec};

pub(super) const NOUNS: &[Noun] = &[
    Noun { path: &["playbook"], also: &["playbooks"], unknown: "Unknown playbook command “{word}”. They are: check, test <id> [--since 7d], set <id> on|off|approve [--confirm]." },
    Noun { path: &["policy"], also: &[], unknown: "Unknown policy command “{word}”. It has one: policy set <project> [--level careful|standard|autopilot] [--condition \"<condition>\"] [--remove-condition <n>] [--confirm]." },
    Noun { path: &["schedule"], also: &["schedules"], unknown: "Unknown schedule command “{word}”. They are: list, add \"<what>\" --every <minutes> | --in <minutes> | --at 09:00, remove <id>, set <id> on|off|approve [--confirm]." },
];

/// The person's changes of trust (M53): each answers with its preview, and changes nothing until
/// it is given again with `--confirm`. Only the person may make them.
const CHANGE: OpSpec = OpSpec {
    group: super::spec::OpGroup::Settings,
    effect: super::spec::EffectClass::LocalWrite,
    kind_v1: crate::control::Kind::Settings,
    human_only: true,
    v2_only: true,
    ..OpSpec::READ
};

const CONFIRM: ArgSpec = ArgSpec { name: "confirm", kind: ArgKind::Switch, help: "make the change the preview showed (without it, only the preview)", at: At::Flag(&["confirm"]), ..ArgSpec::OPTION };
const STATE: ArgSpec = ArgSpec { name: "state", kind: ArgKind::Word, required: true, help: "on, off or approve", at: At::Word(1), missing: "Say on, off or approve.", default: None };

const READ: OpSpec = OpSpec { v2_only: true, ..OpSpec::READ };

/// The schedules' own changes (M41): the manager's group, as v1's checks were; the person's are
/// written as they are, anyone else's only proposed (off until the person approves them).
const SCHEDULE: OpSpec = OpSpec { group: super::spec::OpGroup::Manager, effect: super::spec::EffectClass::LocalWrite, kind_v1: crate::control::Kind::Board, v2_only: true, ..OpSpec::READ };
const WHEN: &str = "schedule add \"<what>\" --every <minutes> | --in <minutes> | --at 09:00";

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
    OpSpec {
        name: "playbook.set",
        cli: &["playbook", "set"],
        summary: "switch a playbook on or off, or approve it as it is: shows what it does, and changes it with --confirm (you only)",
        args: &[ArgSpec { name: "playbook", kind: ArgKind::Word, required: true, help: "the playbook's id", at: At::Word(0), missing: "playbook set <id> on|off|approve [--confirm]", default: None }, STATE, CONFIRM],
        examples: &["playbook set <id> on|off|approve [--confirm]"],
        ..CHANGE
    },
    OpSpec {
        name: "policy.set",
        cli: &["policy", "set"],
        summary: "a project's trust level or conditions: shows what the change does, and makes it with --confirm (you only)",
        args: &[
            ArgSpec { name: "project", kind: ArgKind::Project, required: true, help: "the project", at: At::Word(0), missing: "policy set <project> [--level careful|standard|autopilot] [--condition \"<condition>\"] [--remove-condition <n>] [--confirm]", default: None },
            ArgSpec { name: "level", kind: ArgKind::Word, help: "careful, standard or autopilot", at: At::Flag(&["level"]), ..ArgSpec::OPTION },
            ArgSpec { name: "condition", kind: ArgKind::Text, help: "a condition to add: not on main, never secrets/**, hours 09:00-18:00 weekdays, only Sam", at: At::Flag(&["condition"]), ..ArgSpec::OPTION },
            ArgSpec { name: "remove_condition", kind: ArgKind::Number, help: "the number of a condition to remove (1 is the first)", at: At::Flag(&["remove-condition"]), ..ArgSpec::OPTION },
            CONFIRM,
        ],
        examples: &["policy set <project> [--level careful|standard|autopilot] [--condition \"<condition>\"] [--remove-condition <n>] [--confirm]"],
        ..CHANGE
    },
    OpSpec {
        name: "schedule.list",
        cli: &["schedule", "list"],
        summary: "the schedules: when each runs, whether it runs here, what it does (v1's manager check list)",
        args: &[],
        examples: &["schedule list"],
        ..READ
    },
    OpSpec {
        name: "schedule.add",
        cli: &["schedule", "add"],
        aliases: &["new"],
        retry: super::spec::Retry::AtMostOnce,
        undo: super::spec::Undo::Inverse("schedule.remove"),
        summary: "have the manager look at something on a schedule, once, or each day at a time; from anyone but you it is proposed",
        args: &[
            ArgSpec { name: "what", kind: ArgKind::Text, required: true, help: "what it looks at", at: At::Rest(0), missing: WHEN, default: None },
            ArgSpec { name: "every", kind: ArgKind::Word, help: "how often: minutes, or 30m, 2h, 1d (5 minutes at the least)", at: At::Flag(&["every"]), ..ArgSpec::OPTION },
            ArgSpec { name: "in", kind: ArgKind::Word, help: "once, after so long: minutes, or 30m, 2h, 1d", at: At::Flag(&["in"]), ..ArgSpec::OPTION },
            ArgSpec { name: "at", kind: ArgKind::Word, help: "each day at this time (09:00), in this Mac's zone", at: At::Flag(&["at"]), ..ArgSpec::OPTION },
        ],
        examples: &["schedule add \"<what>\" --every <minutes> | --in <minutes> | --at 09:00"],
        ..SCHEDULE
    },
    OpSpec {
        name: "schedule.remove",
        cli: &["schedule", "remove"],
        aliases: &["delete", "rm"],
        retry: super::spec::Retry::Idempotent,
        effect: super::spec::EffectClass::Destructive,
        summary: "take a schedule away: yours any, anyone else's only one it proposed",
        args: &[ArgSpec { name: "which", kind: ArgKind::Word, required: true, help: "the schedule's id", at: At::Word(0), missing: "schedule remove <id>", default: None }],
        examples: &["schedule remove <id>"],
        ..SCHEDULE
    },
    OpSpec {
        name: "schedule.set",
        cli: &["schedule", "set"],
        summary: "switch a schedule on or off, or approve it as it is: shows what it does, and changes it with --confirm (you only)",
        args: &[ArgSpec { name: "schedule", kind: ArgKind::Word, required: true, help: "the schedule's id", at: At::Word(0), missing: "schedule set <id> on|off|approve [--confirm]", default: None }, STATE, CONFIRM],
        examples: &["schedule set <id> on|off|approve [--confirm]"],
        ..CHANGE
    },
];
