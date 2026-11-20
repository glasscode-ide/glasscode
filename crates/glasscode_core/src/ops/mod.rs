//! The registry of ops (design section 8): one table of everything Glasscode can be asked to do,
//! from the `glasscode` command, the manager's typed tools (M34) or the app itself (M33.1).
//!
//! An entry ([`OpSpec`]) names the op, its words on the command line and their other words, its
//! arguments and where each is on the line, what kind of effect it has, where it can run and the
//! frozen v1 kind. [`parse`] turns the command's words into an [`Op`] with no IO; [`json_schema`]
//! describes an op's arguments for MCP. The app's handlers are keyed by op name (`ops_run.rs` in
//! the app); [`targets`] says what an op touches, from ids, for the gate (M21).

mod help;
mod parse;
mod schema;
mod spec;
mod table_app;
mod table_board;
mod table_forge;
mod table_manager;
mod table_memory;
mod table_project;
mod table_read;
mod table_session;
mod table_switch;
mod table_words;
mod world;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::control::code;
use crate::paths::ProjectKey;

pub use help::help;
pub use parse::{ParseCtx, SessionView, find_session, name_of, parse};
pub use schema::{description, json_schema, typed};
pub use spec::{ArgKind, ArgSpec, At, EffectClass, Noun, OpGroup, OpHost, OpSpec, Options, Retry, Undo};
pub use world::{ProjectView, SessionPlace, World, WorktreeView, find_project, find_task, find_worktree, project_for, targets};

const TABLES: &[&[OpSpec]] = &[table_read::OPS, table_board::OPS, table_words::OPS, table_session::OPS, table_forge::OPS, table_project::OPS, table_app::OPS, table_manager::OPS, table_switch::OPS, table_memory::OPS];
const NOUN_TABLES: &[&[Noun]] = &[table_read::NOUNS, table_board::NOUNS, table_words::NOUNS, table_switch::NOUNS, table_memory::NOUNS];

const ALL_OPS: [OpSpec; count(TABLES)] = flatten(TABLES, OpSpec::READ);
const ALL_NOUNS: [Noun; count(NOUN_TABLES)] = flatten(NOUN_TABLES, Noun { path: &[], also: &[], unknown: "" });

/// Every op, from every table.
pub static OPS: &[OpSpec] = &ALL_OPS;

/// Every noun of the command line with its other words (`session`: `sessions`, `s`).
pub static NOUNS: &[Noun] = &ALL_NOUNS;

const fn count<T>(tables: &[&[T]]) -> usize {
    let (mut n, mut i) = (0, 0);
    while i < tables.len() {
        n += tables[i].len();
        i += 1;
    }
    n
}

/// The tables one after the other, at compile time: each table file keeps its own list.
const fn flatten<T: Copy, const N: usize>(tables: &[&[T]], fill: T) -> [T; N] {
    let mut out = [fill; N];
    let (mut t, mut at) = (0, 0);
    while t < tables.len() {
        let mut i = 0;
        while i < tables[t].len() {
            out[at] = tables[t][i];
            at += 1;
            i += 1;
        }
        t += 1;
    }
    out
}

/// A v1 command whose v2 op has another name (exec-8). Sessions keep the words they were taught
/// (`cli::SKILL`, `cli::CODEX_INSTRUCTIONS`) for as long as they live, and glasscoded, the glasses
/// and the voice worker call the same words, so the v1 words stay, with the same `--json`, until
/// the release that deletes v1 (M56): the slice that adds the v2 op makes the v1 words parse to it.
#[derive(Clone, Copy, Debug)]
pub struct Renamed {
    /// The v1 op, whose words stay.
    pub v1: &'static str,
    /// The op it becomes, and for whom.
    pub v2: &'static str,
    /// The slice that adds the v2 op.
    pub by: &'static str,
}

pub const RENAMED: &[Renamed] = &[
    Renamed { v1: "manager.check.list", v2: "schedule.list", by: "M41" },
    Renamed { v1: "manager.check.add", v2: "schedule.add", by: "M41" },
    Renamed { v1: "manager.check.remove", v2: "schedule.remove", by: "M41" },
    // `glasscode log` read agent-log.jsonl, which the journal replaces: the actions in it.
    Renamed { v1: "app.log", v2: "journal.query", by: "M14" },
];

/// The entry of an op, by its name.
pub fn find(name: &str) -> Option<&'static OpSpec> {
    OPS.iter().find(|op| op.name == name)
}

/// An op with its arguments: what `parse` makes of a command line, and what the typed tools
/// send. In JSON: `{"op": "board.done", "args": {"task": "zysg"}}`.
#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub spec: &'static OpSpec,
    pub args: Value,
}

impl Op {
    pub fn name(&self) -> &'static str {
        self.spec.name
    }

    /// A text or word argument, as given; None when it is left out.
    pub fn text(&self, name: &str) -> Option<&str> {
        self.args.get(name).and_then(Value::as_str)
    }

    /// A switch: on, or left out.
    pub fn on(&self, name: &str) -> bool {
        self.args.get(name).and_then(Value::as_bool).unwrap_or(false)
    }

    /// A number argument (a count, a pull request, an id).
    pub fn number(&self, name: &str) -> Option<u64> {
        self.args.get(name).and_then(Value::as_u64)
    }

    /// An option given any number of times, in order.
    pub fn texts(&self, name: &str) -> Vec<String> {
        self.args.get(name).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default()
    }

    /// A day: None when left out, `Some(None)` for "no day", else the day.
    pub fn day(&self, name: &str) -> Option<Option<crate::day::Day>> {
        match self.args.get(name)? {
            Value::Null => Some(None),
            value => Some(value.as_str().and_then(crate::day::parse_iso)),
        }
    }
}

impl Serialize for Op {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Shape<'a> {
            op: &'a str,
            args: &'a Value,
        }
        Shape { op: self.spec.name, args: &self.args }.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Op {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Op, D::Error> {
        #[derive(Deserialize)]
        struct Shape {
            op: String,
            #[serde(default)]
            args: Value,
        }
        let shape = Shape::deserialize(deserializer)?;
        let spec = find(&shape.op).ok_or_else(|| serde::de::Error::custom(format!("no op “{}”", shape.op)))?;
        let args = if shape.args.is_null() { Value::Object(Default::default()) } else { shape.args };
        Ok(Op { spec, args })
    }
}

/// What an op touches, resolved by Rust from ids (M12.2), for the gate (M21).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Targets {
    pub projects: Vec<ProjectKey>,
    pub sessions: Vec<String>,
    pub repos: Vec<String>,
    pub paths: Vec<PathBuf>,
    pub leaves_machine: bool,
}

/// What a reply to an op leaves for the journal (ops-21): the op, its arguments and what it
/// touched, who asked, what was decided and how it ended, as fields rather than a sentence. The
/// app makes one for each reply of the `glasscode` command; M14 writes them as `op.*` events, and
/// Activity's sentence is made from them there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub op: String,
    pub args: Value,
    pub targets: Targets,
    pub effect: EffectClass,
    /// The op that takes it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
    /// Who asked: "you", "session", "manager" or "outside".
    pub caller: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub decision: Decided,
    /// Its reply's exit code (`control::code`).
    pub code: i32,
    pub at_ms: u64,
}

/// How an op came to run, or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decided {
    /// A rule let it run.
    Allowed,
    /// It waited, and the person allowed it.
    Approved,
    /// The person said no.
    Denied,
    /// A rule never lets it run.
    Never,
}

impl Action {
    pub fn of(op: &Op, targets: Targets, caller: &str, session: Option<String>, decision: Decided, code: i32, at_ms: u64) -> Action {
        let undo = match op.spec.undo {
            Undo::Inverse(other) => Some(other.to_string()),
            Undo::None => None,
        };
        Action { op: op.name().to_string(), args: op.args.clone(), targets, effect: op.spec.effect, undo, caller: caller.to_string(), session, decision, code, at_ms }
    }
}

/// An op's answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpReply {
    Done { text: String, data: Value },
    /// The person has to allow it first.
    Waiting { pending: String, summary: String },
    Failed(OpError),
}

/// What went wrong: a class, and words for the caller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpError {
    #[serde(flatten)]
    pub code: ErrorCode,
    pub text: String,
}

/// What `parse` gives for words that are not an op it can take.
pub type ParseError = OpError;

/// The class of an error. Each maps to an exit code of the `glasscode` command
/// ([`ErrorCode::exit_code`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ErrorCode {
    /// It did not work (exit 1).
    Failed,
    /// The words are not right (exit 2).
    Usage,
    /// What they name is not there (exit 1, as "failed" always was).
    NotFound,
    /// What they name could be several things (exit 2), up to six of them.
    Ambiguous { candidates: Vec<Candidate> },
    /// Not allowed: the person said no, or the settings (exit 5).
    Denied,
    /// Its host is not here (exit 3); `queued` when it runs once the host is back.
    Unavailable { host: OpHost, queued: bool },
}

impl ErrorCode {
    pub fn exit_code(&self) -> i32 {
        match self {
            ErrorCode::Failed | ErrorCode::NotFound => code::FAILED,
            ErrorCode::Usage | ErrorCode::Ambiguous { .. } => code::USAGE,
            ErrorCode::Unavailable { .. } => code::NOT_RUNNING,
            ErrorCode::Denied => code::DENIED,
        }
    }
}

/// One of the things an ambiguous word could mean.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

impl OpError {
    pub fn new(code: ErrorCode, text: impl Into<String>) -> OpError {
        OpError { code, text: text.into() }
    }

    pub fn usage(text: impl Into<String>) -> OpError {
        OpError::new(ErrorCode::Usage, text)
    }

    pub fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Kind;

    /// `cli.rs` as written, with every run of white space made one space.
    fn help_text() -> String {
        squeeze(&help(None))
    }

    fn squeeze(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The words that name an op: its own, and every other word for each.
    fn spellings(op: &OpSpec) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = vec![Vec::new()];
        for (i, word) in op.cli.iter().enumerate() {
            let last = i + 1 == op.cli.len();
            let path = &op.cli[..i + 1];
            let mut words: Vec<&str> = vec![word];
            if last {
                words.extend(op.aliases.iter().filter(|a| !a.is_empty() && **a != "*"));
            } else {
                words.extend(NOUNS.iter().find(|n| n.path == path).map(|n| n.also).unwrap_or_default());
            }
            out = out.iter().flat_map(|prefix| words.iter().map(move |w| [prefix.clone(), vec![w.to_string()]].concat())).collect();
        }
        out
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<&str> = OPS.iter().map(|op| op.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "an op name is used twice");
        // No two ops answer to the same words, and no op to the words of a command still to come.
        let mut seen: std::collections::HashMap<Vec<String>, &str> = std::collections::HashMap::new();
        for op in OPS {
            assert!(!op.cli.is_empty() && op.name.split('.').count() >= 2, "{}", op.name);
            for words in spellings(op) {
                if let Some(other) = seen.insert(words.clone(), op.name) {
                    panic!("{words:?} names both {other} and {}", op.name);
                }
            }
            // One op per place takes "no word" or "any word".
            for special in ["", "*"] {
                let twins = OPS.iter().filter(|o| o.cli.len() == op.cli.len() && o.cli[..o.cli.len() - 1] == op.cli[..op.cli.len() - 1] && o.aliases.contains(&special)).count();
                assert!(twins <= 1, "two ops after {:?} take {special:?}", &op.cli[..op.cli.len() - 1]);
            }
            let mut fields: Vec<&str> = op.args.iter().map(|a| a.name).collect();
            fields.sort_unstable();
            let n = fields.len();
            fields.dedup();
            assert_eq!(fields.len(), n, "{} has an argument twice", op.name);
            // An option belongs to one argument that takes its value; a switch may read it as well.
            let mut flags: Vec<&str> = op.args.iter().filter(|a| a.kind != ArgKind::Switch).flat_map(|a| a.at.flags().iter().copied()).collect();
            flags.sort_unstable();
            let n = flags.len();
            flags.dedup();
            assert_eq!(flags.len(), n, "{} has an option twice", op.name);
        }
    }

    #[test]
    fn every_spec_has_example() {
        for op in OPS {
            assert!(!op.examples.is_empty(), "{} has no example", op.name);
            assert!(!op.summary.is_empty(), "{} has no summary", op.name);
            for arg in op.args {
                assert!(!arg.help.is_empty(), "{}: {} has no help", op.name, arg.name);
                assert_eq!(arg.required, !arg.missing.is_empty(), "{}: {} is required exactly when it says what is missing", op.name, arg.name);
                assert!(!(arg.required && arg.default.is_some()), "{}: {} is required and has a default", op.name, arg.name);
                assert!(matches!(arg.at, At::Word(_) | At::Rest(_)) || !arg.at.flags().is_empty(), "{}: {} has no place", op.name, arg.name);
            }
        }
    }

    /// The commands HELP documents without a line of their own (and the v2 engine's, which HELP
    /// does not list until it is generated, M33.1), and those whose line says only how to call
    /// them or whose words there are no summary: their summary (and for the first, their
    /// example) are written here.
    const UNDOCUMENTED: &[&str] = &["board.undo", "board.unsnooze", "words.recent", "manager.forget", "switch.units", "switch.check", "switch.move", "worktree.move", "worktree.owner"];
    const NO_DESCRIPTION: &[&str] = &[
        "board.show",
        "board.edit",
        "session.list",
        "project.list",
        "worktree.list",
        "manager.follow_up.list",
        "manager.check.list",
        "app.open",
        "app.notify",
        "session.rename",
        "session.close",
        "session.restart",
        "session.hand_off",
        "session.move",
        "pr.label",
        "project.import",
        "project.set_account",
        "worktree.new",
        "worktree.rename",
        "worktree.remove",
        "ticket.start",
        "ticket.move",
        "ticket.comment",
        "manager.show",
        "manager.follow_up.add",
        "manager.follow_up.done",
        "manager.follow_up.drop",
        "manager.message.draft",
        "manager.message.approve",
        "manager.message.reject",
        "manager.check.add",
        "manager.check.remove",
    ];

    #[test]
    fn summaries_and_examples_are_helps_own_words() {
        let help = help_text();
        // The help is v1's: an op of the v2 engine alone is not in it.
        for op in OPS.iter().filter(|op| !op.v2_only && !UNDOCUMENTED.contains(&op.name)) {
            for example in op.examples {
                assert!(help.contains(&squeeze(example)), "{}: “{example}” is not in HELP", op.name);
            }
            if !NO_DESCRIPTION.contains(&op.name) {
                assert!(help.contains(&squeeze(op.summary)), "{}: “{}” is not in HELP", op.name, op.summary);
            }
        }
    }

    /// Each command line of the help is an op's example, and every op the help documents is in it.
    #[test]
    fn every_help_line_is_an_op() {
        let printed = help(None);
        let lines: Vec<&str> = printed.lines().filter(|l| l.starts_with("  ") && l[2..].starts_with(|c: char| c.is_ascii_lowercase())).collect();
        assert!(lines.len() > 60, "{}", lines.len());
        let examples: Vec<String> = OPS.iter().flat_map(|op| op.examples.iter().map(|e| squeeze(e))).collect();
        for line in lines {
            let line = squeeze(line);
            assert!(examples.iter().any(|e| line.starts_with(e.as_str())), "“{line}” is no op's example");
        }
        let placed: Vec<&str> = help::SECTIONS.iter().flat_map(|s| s.lines.iter().filter_map(help::Line::op)).collect();
        for op in OPS.iter().filter(|op| !op.v2_only && !UNDOCUMENTED.contains(&op.name)) {
            assert!(placed.contains(&op.name) || op.examples.iter().any(|e| printed.contains(e)), "{} is not in the help", op.name);
        }
    }

    /// The ops v1 counts as reads (they run for anyone Settings › Permissions lets read) that are not
    /// reads in v2: asking the manager changes nothing by itself, but it is input to its agent.
    const V1_READS: &[&str] = &["manager.ask"];

    #[test]
    fn kinds_and_effects_agree() {
        for op in OPS {
            if op.effect == EffectClass::Read {
                assert_eq!(op.kind_v1, Kind::Read, "{}", op.name);
            }
            assert_eq!(op.kind_v1 == Kind::Read && op.effect != EffectClass::Read, V1_READS.contains(&op.name), "{}", op.name);
            assert_eq!(op.effect == EffectClass::Read, matches!(op.group, OpGroup::Read | OpGroup::ForgeRead), "{}", op.name);
            // Only reads and the switch's moves wait for the v2 engine; the person's own approvals
            // are never offered to anyone else.
            if op.v2_only {
                assert!(op.name.starts_with("switch.") || op.name.starts_with("worktree.") || op.name.starts_with("memory."), "{}", op.name);
            }

            if let Undo::Inverse(other) = op.undo {
                let inverse = find(other).unwrap_or_else(|| panic!("{}: no op {other}", op.name));
                assert_ne!(inverse.effect, EffectClass::Read, "{}", op.name);
            }
        }
    }

    /// What each op is, in one line: so that a change to a group, effect, retry class, host, v1 kind or
    /// undo is one a review sees (name, group, effect, retry, host, v1 kind, undo, person only, and
    /// "v2" for an op only the v2 engine knows).
    #[test]
    fn the_table_in_short() {
        let short: Vec<String> = OPS
            .iter()
            .map(|op| {
                let undo = match op.undo {
                    Undo::None => "-",
                    Undo::Inverse(other) => other,
                };
                let line = format!("{} {:?} {:?} {:?} {:?} {} {undo} {}", op.name, op.group, op.effect, op.retry, op.host, op.kind_v1.key(), if op.human_only { "person" } else { "-" });
                if op.v2_only { format!("{line} v2") } else { line }
            })
            .collect();
        let pinned = "
        session.list Read Read Idempotent SessionHost read - -
        session.show Read Read Idempotent SessionHost read - -
        session.read Read Read Idempotent SessionHost read - -
        pr.status ForgeRead Read Idempotent Network read - -
        pr.list ForgeRead Read Idempotent Network read - -
        project.list Read Read Idempotent Core read - -
        worktree.list Read Read Idempotent Core read - -
        account.list Read Read Idempotent Core read - -
        ticket.list Read Read Idempotent Network read - -
        ticket.show Read Read Idempotent Network read - -
        app.status Read Read Idempotent Window read - -
        app.log Read Read Idempotent Core read - -
        app.approvals Read Read Idempotent Window read - -
        app.whoami Read Read Idempotent Core read - -
        manager.status Read Read Idempotent Core read - -
        manager.brief Read Read Idempotent Core read - -
        manager.memory Read Read Idempotent Core read - -
        manager.follow_up.list Read Read Idempotent Core read - -
        manager.message.list Read Read Idempotent Core read - -
        manager.check.list Read Read Idempotent Core read - -
        board.list Read Read Idempotent Core read - -
        board.show Read Read Idempotent Core read - -
        board.add Board LocalWrite AtMostOnce Core board board.remove -
        board.done Board LocalWrite Idempotent Core board board.undo -
        board.undo Board LocalWrite Idempotent Core board board.done -
        board.snooze Board LocalWrite Idempotent Core board board.unsnooze -
        board.unsnooze Board LocalWrite Idempotent Core board - -
        board.pin Board LocalWrite Idempotent Core board board.unpin -
        board.unpin Board LocalWrite Idempotent Core board board.pin -
        board.move Board LocalWrite Idempotent Core board - -
        board.note Board LocalWrite AtMostOnce Core board - -
        board.edit Board LocalWrite Idempotent Core board - -
        board.remove Board Destructive Idempotent Core board - -
        board.recurring Read Read Idempotent Core read - -
        words.recent Read Read Idempotent Core read - person
        session.new SessionStart Create AtMostOnce SessionHost start session.close -
        session.open Navigate Navigate Idempotent Window show - -
        session.rename SessionControl LocalWrite Idempotent SessionHost rename session.rename -
        session.close SessionControl Destructive Idempotent SessionHost close - -
        session.restart SessionControl Destructive AtMostOnce SessionHost close - -
        session.send SessionInput SessionInput AtMostOnce SessionHost send - -
        session.hand_off SessionStart Create AtMostOnce SessionHost move - -
        session.move Worktree Create Verifiable SessionHost move session.move -
        pr.review SessionStart Create AtMostOnce SessionHost start session.close -
        pr.build SessionStart Create AtMostOnce SessionHost start session.close -
        pr.rerun ForgeWrite Outward AtMostOnce Network github - -
        pr.comment ForgeWrite Outward AtMostOnce Network github - -
        pr.label ForgeWrite Outward Idempotent Network github pr.label -
        pr.open_draft ForgeWrite Outward Verifiable SessionHost github - -
        ticket.start SessionStart Create AtMostOnce SessionHost start session.close -
        ticket.move Tickets Outward Idempotent Network tickets ticket.move -
        ticket.comment Tickets Outward AtMostOnce Network tickets - -
        project.import Project Create Verifiable SessionHost import project.remove -
        project.remove Project Destructive Idempotent SessionHost close - -
        project.set_account Settings LocalWrite Idempotent SessionHost settings project.set_account -
        worktree.new Worktree Create Verifiable SessionHost start worktree.remove -
        worktree.rename Worktree LocalWrite Verifiable SessionHost rename worktree.rename -
        worktree.remove Worktree Destructive Idempotent SessionHost remove - -
        worktree.clean Worktree Destructive Idempotent SessionHost remove - -
        worktree.open Navigate Navigate Idempotent Window show - -
        app.open Navigate Navigate Idempotent Window show - -
        app.notify Board LocalWrite AtMostOnce Window board - -
        app.wait Read Read Idempotent Window read - -
        manager.ask Manager SessionInput AtMostOnce Core read - -
        manager.say Navigate Navigate AtMostOnce Window show - -
        manager.tour Navigate Navigate Idempotent Window show - -
        manager.remember Memory LocalWrite AtMostOnce Core board manager.forget -
        manager.forget Memory Destructive Idempotent Core board - -
        manager.show Navigate Navigate Idempotent Window show - -
        manager.scene Navigate Navigate Idempotent Window show - -
        manager.voice_token Read Read Idempotent Window read - -
        manager.follow_up.add Manager LocalWrite AtMostOnce Core board manager.follow_up.drop -
        manager.follow_up.done Manager LocalWrite Idempotent Core board - -
        manager.follow_up.drop Manager LocalWrite Idempotent Core board - -
        manager.message.draft Message LocalWrite AtMostOnce Core board manager.message.reject -
        manager.message.approve Message Outward AtMostOnce Core board - person
        manager.message.reject Message LocalWrite Idempotent Core board - person
        manager.check.add Manager LocalWrite AtMostOnce Core board manager.check.remove -
        manager.check.remove Manager Destructive Idempotent Core board - -
        switch.units Read Read Idempotent SessionHost read - - v2
        switch.check Read Read Idempotent SessionHost read - - v2
        switch.move Worktree Create Verifiable SessionHost move worktree.move - v2
        worktree.move Worktree Create Verifiable SessionHost move worktree.move - v2
        worktree.owner Read Read Idempotent Core read - - v2
        memory.search Read Read Idempotent Core read - - v2
        memory.show Read Read Idempotent Core read - - v2
        memory.brief Read Read Idempotent Core read - - v2
        memory.note Memory LocalWrite Idempotent Core board - - v2
        memory.propose Memory LocalWrite Idempotent Core board - - v2
        memory.accept Memory LocalWrite Idempotent Core board - person v2
        memory.reject Memory LocalWrite Idempotent Core board - person v2
        memory.merge Memory LocalWrite Idempotent Core board - - v2
        memory.let_go Memory LocalWrite Idempotent Core board - - v2
        memory.rewrite Memory LocalWrite Idempotent Core board - - v2
        memory.forget Memory Destructive Verifiable Core board - person v2
        memory.pending Read Read Idempotent Core read - person v2
        ";
        let pinned: Vec<&str> = pinned.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        assert_eq!(short, pinned);
    }

    #[test]
    fn every_noun_has_its_words_and_every_op_its_noun() {
        for op in OPS {
            for depth in 1..op.cli.len() {
                assert!(NOUNS.iter().any(|n| n.path == &op.cli[..depth]), "{}: no noun {:?}", op.name, &op.cli[..depth]);
            }
        }
        for noun in NOUNS {
            assert!(OPS.iter().any(|op| op.cli.len() > noun.path.len() && op.cli[..noun.path.len()] == *noun.path), "noun {:?} has no op", noun.path);
        }
    }

    /// The words every session was taught still name ops (exec-8): each `glasscode …` of the skill
    /// and of Codex's instructions parses, and so does every bare command in them.
    #[test]
    fn the_words_sessions_were_taught_still_parse() {
        let ctx = ParseCtx { caller_is_person: false, caller_session: Some("d740a312-2980-49ad-96b0-17b1bc7bcb50".into()), ..Default::default() };
        let mut seen = 0;
        for text in [crate::cli::SKILL, crate::cli::CODEX_INSTRUCTIONS] {
            let squeezed = squeeze(text);
            for (i, span) in squeezed.split('`').enumerate() {
                let line = span.trim_start_matches("glasscode ").trim();
                // Inside backticks, a command line; not `--json`, `help` (the command's own) or prose.
                if i % 2 == 0 || line.starts_with("--") || line.starts_with("help") || line == "glasscode" || !line.starts_with(|c: char| c.is_ascii_lowercase()) {
                    continue;
                }
                let line = line.replace("<session>", "this").replace("<id>", "7");
                let words = shell_words(&line);
                assert!(parse(&words, &ctx).is_ok(), "“{line}”: {:?}", parse(&words, &ctx));
                seen += 1;
            }
        }
        assert!(seen >= 15, "{seen}");
        // The ones that get another name in v2 are still v1's ops.
        for renamed in RENAMED {
            assert!(find(renamed.v1).is_some(), "{}", renamed.v1);
            let first = renamed.v2.split([' ', ',']).next().unwrap();
            assert!(find(first).is_none(), "{} exists now: make `{}`'s words parse to it, with the same --json, and drop it here ({})", first, renamed.v1, renamed.by);
        }
    }

    /// The memory commands v2 teaches sessions (M20.2) parse with the v2 engine.
    #[test]
    fn the_memory_words_sessions_are_taught_parse_in_v2() {
        let ctx = ParseCtx { caller_is_person: false, caller_session: Some("d740a312-2980-49ad-96b0-17b1bc7bcb50".into()), v2: true, ..Default::default() };
        let mut seen = 0;
        for text in [crate::cli::MEMORY_INSTRUCTIONS, crate::cli::MEMORY_BRIEF_ASK] {
            for (i, span) in squeeze(text).split('`').enumerate() {
                if i % 2 == 0 || !span.starts_with("glasscode memory") {
                    continue;
                }
                let words = shell_words(span.trim_start_matches("glasscode "));
                assert!(parse(&words, &ctx).is_ok(), "“{span}”: {:?}", parse(&words, &ctx));
                seen += 1;
            }
        }
        assert_eq!(seen, 4);
        let v1 = ParseCtx { v2: false, ..ctx };
        assert!(parse(&["memory".into(), "search".into(), "x".into()], &v1).is_err(), "unknown with v1, as before");
    }

    /// Words as a shell splits a simple line: spaces, and "…" as one word.
    fn shell_words(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut word = String::new();
        let mut quoted = false;
        for c in line.chars() {
            match c {
                '"' => quoted = !quoted,
                ' ' if !quoted => {
                    if !word.is_empty() {
                        out.push(std::mem::take(&mut word));
                    }
                }
                c => word.push(c),
            }
        }
        if !word.is_empty() {
            out.push(word);
        }
        out
    }

    #[test]
    fn an_action_is_made_from_its_op() {
        let op = Op { spec: find("board.done").unwrap(), args: serde_json::json!({ "task": "zysg" }) };
        let action = Action::of(&op, Targets::default(), "session", Some("s1".into()), Decided::Approved, 0, 7);
        assert_eq!((action.op.as_str(), action.effect, action.undo.as_deref()), ("board.done", EffectClass::LocalWrite, Some("board.undo")));
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["args"], serde_json::json!({ "task": "zysg" }));
        assert_eq!(json["decision"], serde_json::json!("approved"));
        assert_eq!(serde_json::from_value::<Action>(json).unwrap(), action);
    }

    #[test]
    fn an_op_reads_back_from_its_json() {
        let op = Op { spec: find("board.done").unwrap(), args: serde_json::json!({ "task": "zysg" }) };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(json, serde_json::json!({ "op": "board.done", "args": { "task": "zysg" } }));
        assert_eq!(serde_json::from_value::<Op>(json).unwrap(), op);
        assert!(serde_json::from_value::<Op>(serde_json::json!({ "op": "board.merge" })).is_err());
        let error = OpError::new(ErrorCode::Ambiguous { candidates: vec![Candidate { id: "a".into(), title: "A".into(), project: None }] }, "which?");
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json, serde_json::json!({ "code": "ambiguous", "candidates": [{ "id": "a", "title": "A" }], "text": "which?" }));
        assert_eq!(serde_json::from_value::<OpError>(json).unwrap(), error);
        let failed = OpReply::Failed(error.clone());
        assert_eq!(serde_json::to_value(&failed).unwrap()["kind"], serde_json::json!("failed"));
        assert_eq!(serde_json::from_value::<OpReply>(serde_json::to_value(&failed).unwrap()).unwrap(), failed);
        assert_eq!(error.exit_code(), code::USAGE);
        assert_eq!(OpError::new(ErrorCode::NotFound, "").exit_code(), code::FAILED);
        assert_eq!(OpError::new(ErrorCode::Unavailable { host: OpHost::Window, queued: true }, "").exit_code(), code::NOT_RUNNING);
    }
}
