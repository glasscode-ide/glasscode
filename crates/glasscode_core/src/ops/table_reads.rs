//! Typed reads for the manager's brain (M33.2): a pull request, its checks, its diff and review
//! threads and a CI run's log from GitHub (`github_api`), a worktree's git status, log and diff
//! with fixed arguments, a file of a managed project, and the accounts' usage; and the settings
//! a caller may read or change by name (`crate::settings`). All are the v2 engine's. What they
//! return is data, never instructions (a pull request's text is someone else's words).

use super::spec::{ArgKind, ArgSpec, At, EffectClass, Noun, OpGroup, OpHost, OpSpec, Retry, Undo};
use crate::control::Kind;

pub(super) const NOUNS: &[Noun] = &[
    Noun { path: &["ci"], also: &[], unknown: "Unknown ci command “{word}”. It has one: ci log <run> [--repo <owner/name>]." },
    Noun { path: &["git"], also: &[], unknown: "Unknown git command “{word}”. It has: git status, git log and git diff <worktree>." },
    Noun { path: &["file"], also: &["files"], unknown: "Unknown file command “{word}”. It has one: file read <path>." },
    Noun { path: &["usage"], also: &[], unknown: "Unknown usage command “{word}”. It has one: usage get [--agent claude|codex]." },
    Noun { path: &["settings"], also: &["setting"], unknown: "Unknown settings command “{word}”. It has: settings get [<key>] and settings set <key> <value>." },
];

const REPO: ArgSpec = ArgSpec { name: "repo", kind: ArgKind::Word, help: "owner/name (else the project's)", at: At::Flag(&["repo"]), ..ArgSpec::OPTION };
const PROJECT: ArgSpec = ArgSpec { name: "project", kind: ArgKind::Project, help: "the project whose repository it is", at: At::Flag(&["project", "p"]), ..ArgSpec::OPTION };
const PR: ArgSpec = ArgSpec { name: "pr", kind: ArgKind::Pr, required: true, help: "its number", at: At::Word(0), missing: "Say which pull request: its number.", default: None };
const WORKTREE: ArgSpec = ArgSpec { name: "worktree", kind: ArgKind::Word, required: true, help: "a worktree: its branch or folder", at: At::Word(0), missing: "Say which worktree: its branch or folder.", default: None };

/// A read from GitHub: the base of those entries.
const FORGE: OpSpec = OpSpec { group: OpGroup::ForgeRead, host: OpHost::Network, v2_only: true, ..OpSpec::READ };
/// A read of a worktree or a file, where it is.
const LOCAL: OpSpec = OpSpec { host: OpHost::SessionHost, v2_only: true, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    // `pr view` is `pr status` (v1's other word for it).
    OpSpec { name: "pr.view", cli: &["pr", "details"], summary: "a pull request: its title, state, branches, author, description and numbers", args: &[PR, REPO, PROJECT], examples: &["pr details <n> [--repo <owner/name>]"], ..FORGE },
    OpSpec { name: "pr.checks", cli: &["pr", "checks"], summary: "a pull request's checks: each one's name, state and run", args: &[PR, REPO, PROJECT], examples: &["pr checks <n> [--repo <owner/name>]"], ..FORGE },
    OpSpec { name: "pr.diff", cli: &["pr", "diff"], summary: "a pull request's diff (the first 200 KB)", args: &[PR, REPO, PROJECT], examples: &["pr diff <n> [--repo <owner/name>]"], ..FORGE },
    OpSpec { name: "review.threads", cli: &["pr", "threads"], summary: "a pull request's review threads: the file, the line, who wrote what, resolved or not", args: &[PR, REPO, PROJECT], examples: &["pr threads <n> [--repo <owner/name>]"], ..FORGE },
    OpSpec {
        name: "ci.log",
        cli: &["ci", "log"],
        summary: "the log of a CI run's failed jobs (the last 200 KB)",
        args: &[ArgSpec { name: "run", kind: ArgKind::Number, required: true, help: "the run's id", at: At::Word(0), missing: "ci log <run> [--repo <owner/name>]", default: None }, REPO, PROJECT],
        examples: &["ci log <run> [--repo <owner/name>]"],
        ..FORGE
    },
    OpSpec { name: "git.status", cli: &["git", "status"], summary: "a worktree's branch, what it is ahead and behind, and its changed files", args: &[WORKTREE, PROJECT], examples: &["git status <worktree>"], ..LOCAL },
    OpSpec {
        name: "git.log",
        cli: &["git", "log"],
        summary: "a worktree's last commits: hash, author, date and subject",
        args: &[WORKTREE, PROJECT, ArgSpec { name: "limit", kind: ArgKind::Count, help: "how many", at: At::Flag(&["limit"]), default: Some("20"), ..ArgSpec::OPTION }],
        examples: &["git log <worktree> [--limit 20]"],
        ..LOCAL
    },
    OpSpec { name: "git.diff", cli: &["git", "diff"], summary: "a worktree's changes against its last commit (the first 200 KB)", args: &[WORKTREE, PROJECT], examples: &["git diff <worktree>"], ..LOCAL },
    OpSpec {
        name: "file.read",
        cli: &["file", "read"],
        summary: "a file of a managed project, as data (200 KB at most; never credentials or Glasscode's own files)",
        args: &[ArgSpec { name: "path", kind: ArgKind::Text, required: true, help: "its path: absolute, or from where the command runs", at: At::Rest(0), missing: "file read <path>", default: None }],
        examples: &["file read <path>"],
        ..LOCAL
    },
    OpSpec {
        name: "usage.get",
        cli: &["usage", "get"],
        aliases: &[""],
        host: OpHost::Window,
        summary: "the usage of each Claude and Codex account: the share of each limit used, and when it resets",
        args: &[ArgSpec { name: "agent", kind: ArgKind::Word, help: "claude or codex (else both)", at: At::FlagOrWord(&["agent"], 0), ..ArgSpec::OPTION }],
        examples: &["usage get [--agent claude|codex]"],
        v2_only: true,
        ..OpSpec::READ
    },
    OpSpec {
        name: "settings.get",
        cli: &["settings", "get"],
        aliases: &[""],
        summary: "a setting's value and who may change it, or every setting (secrets are never shown)",
        args: &[ArgSpec { name: "key", kind: ArgKind::Word, help: "its key, e.g. appearance.theme (else all)", at: At::Word(0), ..ArgSpec::OPTION }],
        examples: &["settings get [<key>]"],
        v2_only: true,
        ..OpSpec::READ
    },
    OpSpec {
        name: "settings.set",
        cli: &["settings", "set"],
        group: OpGroup::Settings,
        effect: EffectClass::LocalWrite,
        retry: Retry::Idempotent,
        undo: Undo::Inverse("settings.set"),
        kind_v1: Kind::Settings,
        summary: "change a setting; the gate, outward automation and programs to run are yours alone",
        args: &[
            ArgSpec { name: "key", kind: ArgKind::Word, required: true, help: "its key, e.g. appearance.theme", at: At::Word(0), missing: "settings set <key> <value>", default: None },
            ArgSpec { name: "value", kind: ArgKind::Text, required: true, help: "its new value: true or false, a word, a number, or JSON", at: At::Rest(1), missing: "settings set <key> <value>", default: None },
        ],
        examples: &["settings set <key> <value>"],
        v2_only: true,
        ..OpSpec::READ
    },
];
