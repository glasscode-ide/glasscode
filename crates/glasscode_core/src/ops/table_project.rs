//! The ops that change projects and worktrees (M12.2). Summaries and examples are `cli.rs`'s
//! `HELP`, word for word (a test checks); the words, options and messages are those of
//! `project_plan` and `worktree_plan` in `commands2.rs`, which these replace.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, OpGroup, OpHost, OpSpec, Retry, Undo};
use crate::control::Kind;

const fn option(name: &'static str, kind: ArgKind, flags: &'static [&'static str], help: &'static str) -> ArgSpec {
    ArgSpec { name, kind, help, at: At::Flag(flags), ..ArgSpec::OPTION }
}

const fn project(missing: &'static str) -> ArgSpec {
    ArgSpec { name: "project", kind: ArgKind::Project, required: true, help: "a project's name, id or folder", at: At::Word(0), missing, default: None }
}

/// A worktree by its folder, its branch or the end of its path (`find_worktree` in the app).
pub(super) const fn worktree(missing: &'static str) -> ArgSpec {
    ArgSpec { name: "worktree", kind: ArgKind::Word, required: true, help: "a worktree: its folder, its branch, or the end of its path", at: At::Word(0), missing, default: None }
}

/// A project of Glasscode's: the base of their entries. They run where the projects are.
const PROJECTS: OpSpec = OpSpec { group: OpGroup::Project, host: OpHost::SessionHost, ..OpSpec::READ };

/// A worktree's change: the base of their entries.
pub(super) const WORKTREES: OpSpec = OpSpec { group: OpGroup::Worktree, host: OpHost::SessionHost, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    // ----- projects ---------------------------------------------------------------------------
    OpSpec {
        name: "project.import",
        cli: &["project", "import"],
        aliases: &["add"],
        effect: EffectClass::Create,
        retry: Retry::Verifiable,
        undo: Undo::Inverse("project.remove"),
        kind_v1: Kind::Import,
        summary: "add a folder as a project, or clone a repository and add it",
        args: &[
            ArgSpec {
                name: "source",
                kind: ArgKind::Text,
                required: true,
                help: "a folder, a git URL, or owner/name on GitHub",
                at: At::Rest(0),
                missing: "project import <folder or git URL> [--name <name>] [--into <folder>]",
                default: None,
            },
            option("name", ArgKind::Word, &["name"], "its name in Glasscode (and the folder a clone goes into)"),
            option("into", ArgKind::Word, &["into"], "the folder a clone goes into (else where most projects are)"),
        ],
        examples: &["project import <folder or git URL> [--name <name>] [--into <folder>]"],
        ..PROJECTS
    },
    OpSpec {
        name: "project.remove",
        cli: &["project", "remove"],
        aliases: &["rm"],
        effect: EffectClass::Destructive,
        kind_v1: Kind::Close,
        summary: "from Glasscode (the folder stays)",
        args: &[project("project remove <project>")],
        examples: &["project remove <p>"],
        ..PROJECTS
    },
    OpSpec {
        name: "project.set_account",
        cli: &["project", "set-account"],
        aliases: &["account"],
        group: OpGroup::Settings,
        effect: EffectClass::LocalWrite,
        undo: Undo::Inverse("project.set_account"),
        kind_v1: Kind::Settings,
        summary: "the GitHub account its new sessions use",
        args: &[
            project("project set-account <project> --github <login>"),
            ArgSpec {
                name: "github",
                kind: ArgKind::Word,
                required: true,
                help: "a login gh is signed in as, or default",
                at: At::Flag(&["github"]),
                missing: "project set-account <project> --github <login> (or --github default)",
                default: None,
            },
        ],
        examples: &["project set-account <p> --github <login>"],
        ..PROJECTS
    },
    // ----- worktrees --------------------------------------------------------------------------
    OpSpec {
        name: "worktree.new",
        cli: &["worktree", "new"],
        aliases: &["add", "create"],
        effect: EffectClass::Create,
        retry: Retry::Verifiable,
        undo: Undo::Inverse("worktree.remove"),
        kind_v1: Kind::Start,
        summary: "a worktree for a new branch",
        args: &[
            option("project", ArgKind::Project, &["project"], "a project's name (else the session's own, or the only one)"),
            ArgSpec {
                name: "branch",
                kind: ArgKind::Word,
                required: true,
                help: "the new branch",
                at: At::FlagOrWord(&["branch"], 0),
                missing: "worktree new --branch <name> [--project <p>] [--from <base>]",
                default: None,
            },
            option("from", ArgKind::Word, &["from"], "the branch it starts from (else the default branch, fetched)"),
        ],
        examples: &["worktree new --branch <name> [--project <p>] [--from <base>]"],
        ..WORKTREES
    },
    OpSpec {
        name: "worktree.rename",
        cli: &["worktree", "rename"],
        effect: EffectClass::LocalWrite,
        retry: Retry::Verifiable,
        undo: Undo::Inverse("worktree.rename"),
        kind_v1: Kind::Rename,
        summary: "rename its branch",
        args: &[
            worktree("worktree rename <worktree> <new branch>"),
            ArgSpec { name: "branch", kind: ArgKind::Word, required: true, help: "its new branch", at: At::Word(1), missing: "worktree rename <worktree> <new branch>", default: None },
            option("force", ArgKind::Switch, &["force"], "also when the branch is on GitHub already (only this copy is renamed)"),
        ],
        examples: &["worktree rename <worktree> <new branch>"],
        ..WORKTREES
    },
    OpSpec {
        name: "worktree.remove",
        cli: &["worktree", "remove"],
        aliases: &["rm", "delete"],
        effect: EffectClass::Destructive,
        kind_v1: Kind::Remove,
        summary: "remove it, and the sessions in it",
        args: &[
            worktree("worktree remove <worktree> [--delete-branch] [--force]"),
            option("delete_branch", ArgKind::Switch, &["delete-branch"], "its branch too, when nothing on it is only there"),
            option("force", ArgKind::Switch, &["force"], "also with uncommitted files or commits only there"),
        ],
        examples: &["worktree remove <worktree> [--delete-branch] [--force]"],
        ..WORKTREES
    },
    OpSpec {
        name: "worktree.clean",
        cli: &["worktree", "clean"],
        aliases: &["cleanup", "clean-up"],
        effect: EffectClass::Destructive,
        kind_v1: Kind::Remove,
        summary: "the ones with nothing to lose",
        args: &[
            option("project", ArgKind::Project, &["project"], "only this project's (else the session's own project, or every project)"),
            option("merged", ArgKind::Switch, &["merged"], "only the ones whose branch is merged"),
            option("older", ArgKind::Word, &["older"], "only the ones untouched for so long: 14d, 2w"),
            option("dry_run", ArgKind::Switch, &["dry-run"], "only say which would go"),
            option("keep_branch", ArgKind::Switch, &["keep-branch"], "keep their branches"),
        ],
        examples: &["worktree clean [--project <p>] [--merged] [--older 14d] [--dry-run]"],
        ..WORKTREES
    },
    OpSpec {
        name: "worktree.open",
        cli: &["worktree", "open"],
        group: OpGroup::Navigate,
        effect: EffectClass::Navigate,
        host: OpHost::Window,
        kind_v1: Kind::Show,
        summary: "in your editor",
        args: &[worktree("worktree open <worktree>")],
        examples: &["worktree open <worktree>"],
        ..OpSpec::READ
    },
];
