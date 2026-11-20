//! Your data (M25): export, import, forget and uninstall (`portability`). They are the person's
//! alone (`human_only`) and `v2_only`: with the v1 engine `glasscode data …` stays unknown.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, Noun, OpGroup, OpSpec, Retry, Undo};
use crate::control::Kind;

pub(super) const NOUNS: &[Noun] = &[Noun { path: &["data"], also: &[], unknown: "Unknown data command “{word}”. They are: export, import, forget, uninstall." }];

const fn option(name: &'static str, kind: ArgKind, flags: &'static [&'static str], help: &'static str) -> ArgSpec {
    ArgSpec { name, kind, help, at: At::Flag(flags), ..ArgSpec::OPTION }
}

/// The person's own data: what only they do.
const DATA: OpSpec = OpSpec { group: OpGroup::Settings, effect: EffectClass::LocalWrite, retry: Retry::Verifiable, kind_v1: Kind::Settings, human_only: true, v2_only: true, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "data.export",
        cli: &["data", "export"],
        summary: "export memory and the stores to one tar file (projects that stay on this Mac are left out unless named)",
        args: &[
            ArgSpec { name: "to", kind: ArgKind::Text, help: "the file to write (else a dated file in Downloads)", at: At::FlagOrRest(&["to"], 0), ..ArgSpec::OPTION },
            option("include", ArgKind::Many, &["include"], "a project that stays on this Mac, taken along anyway; may be given more than once"),
        ],
        examples: &["data export [--to <file.tar>] [--include <project>]…"],
        ..DATA
    },
    OpSpec {
        name: "data.import",
        cli: &["data", "import"],
        summary: "import an export: memory merged entry by entry, project keys, cited words; policies set aside to approve again",
        args: &[ArgSpec { name: "from", kind: ArgKind::Text, required: true, help: "the export's tar file", at: At::Rest(0), missing: "data import <file.tar>", default: None }],
        examples: &["data import <file.tar>"],
        ..DATA
    },
    OpSpec {
        name: "data.forget",
        cli: &["data", "forget"],
        summary: "forget a person, a project or an employer everywhere Glasscode keeps it, memory's history included",
        args: &[
            option("person", ArgKind::Text, &["person"], "a person's name"),
            option("project", ArgKind::Project, &["project"], "a project"),
            option("employer", ArgKind::Word, &["employer"], "an employer's folder name"),
        ],
        examples: &["data forget --person \"<name>\" | --project <name> | --employer <name>"],
        effect: EffectClass::Destructive,
        undo: Undo::None,
        ..DATA
    },
    OpSpec {
        name: "data.uninstall",
        cli: &["data", "uninstall"],
        summary: "list everything Glasscode made (--dry-run), or remove it after you confirm in the app",
        args: &[option("dry_run", ArgKind::Switch, &["dry-run"], "only list it")],
        examples: &["data uninstall [--dry-run]"],
        effect: EffectClass::Destructive,
        undo: Undo::None,
        ..DATA
    },
];
