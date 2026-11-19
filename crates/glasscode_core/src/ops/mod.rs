//! The registry of ops (design section 8): one table of everything Glasscode can be asked to do,
//! from the `glasscode` command, the manager's typed tools (M34) or the app itself (M33.1).
//!
//! An entry ([`OpSpec`]) names the op, its words on the command line and their other words, its
//! arguments and where each is on the line, what kind of effect it has, where it can run and the
//! frozen v1 kind. [`parse`] turns the command's words into an [`Op`] with no IO; [`json_schema`]
//! describes an op's arguments for MCP. The handlers come with M12.2; until then the commands M12.2
//! adds are listed in [`NOT_YET`], and `parse` says so for them.

mod parse;
mod schema;
mod spec;
mod table_board;
mod table_read;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::control::code;
use crate::paths::ProjectKey;

pub use parse::{ParseCtx, SessionView, parse};
pub use schema::json_schema;
pub use spec::{ArgKind, ArgSpec, At, EffectClass, Noun, OpGroup, OpHost, OpSpec, Options, Retry, Undo};

const TABLES: &[&[OpSpec]] = &[table_read::OPS, table_board::OPS];
const NOUN_TABLES: &[&[Noun]] = &[table_read::NOUNS, table_board::NOUNS];

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

/// The commands of `commands.rs`, `commands2.rs` and `manager_cmd.rs` that M12.2 puts in the
/// table, by the words before them and their words with every other word for them. `parse` says
/// "not in the registry yet" for them; M12.2 empties this list (a test keeps it apart from
/// [`OPS`]).
pub const NOT_YET: &[(&[&str], &[&str])] = &[
    (&[], &["open", "show", "notify", "wait"]),
    (&["session"], &["new", "start", "create", "open", "focus", "go", "rename", "name", "title", "close", "restart", "send", "message", "tell", "hand-off", "handoff", "switch", "move"]),
    (&["pr"], &["review", "build", "work", "rerun", "comment", "label", "open-draft", "draft"]),
    (&["project"], &["import", "add", "remove", "rm", "set-account", "account"]),
    (&["worktree"], &["new", "add", "create", "rename", "remove", "rm", "delete", "clean", "cleanup", "clean-up", "open"]),
    (&["ticket"], &["start", "work", "move", "status", "comment"]),
    (&["manager"], &["ask", "say", "tour", "walk", "remember", "forget", "show", "scene", "draw", "voice-token"]),
    (&["manager", "follow-up"], &["add", "new", "done", "close", "drop", "dropped"]),
    (&["manager", "message"], &["draft", "new", "approve", "allow", "send", "reject", "deny"]),
    (&["manager", "check"], &["add", "new", "remove", "rm", "delete"]),
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
        include_str!("../cli.rs").split_whitespace().collect::<Vec<_>>().join(" ")
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
                let (path, last) = words.split_at(words.len() - 1);
                assert!(!NOT_YET.iter().any(|(p, w)| p.iter().copied().eq(path.iter().map(String::as_str)) && w.contains(&last[0].as_str())), "{words:?} is in NOT_YET and in OPS");
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
            let mut flags = op.flags();
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

    /// The commands HELP documents without a line of their own, and those whose line says only
    /// how to call them: their summary (and for the first, their example) are written here.
    const UNDOCUMENTED: &[&str] = &["board.undo", "board.unsnooze"];
    const NO_DESCRIPTION: &[&str] = &["board.show", "board.edit", "session.list", "project.list", "worktree.list", "manager.follow_up.list", "manager.check.list"];

    #[test]
    fn summaries_and_examples_are_helps_own_words() {
        let help = help_text();
        for op in OPS.iter().filter(|op| !UNDOCUMENTED.contains(&op.name)) {
            for example in op.examples {
                assert!(help.contains(&squeeze(example)), "{}: “{example}” is not in HELP", op.name);
            }
            if !NO_DESCRIPTION.contains(&op.name) {
                assert!(help.contains(&squeeze(op.summary)), "{}: “{}” is not in HELP", op.name, op.summary);
            }
        }
    }

    /// Each command line of HELP is an op's example, or a command M12.2 adds.
    #[test]
    fn every_help_line_is_an_op_or_not_yet() {
        let source = include_str!("../cli.rs");
        let help = &source[source.find("const HELP").unwrap()..source.find("const FOOTER").unwrap()];
        let lines: Vec<&str> = help.lines().filter(|l| l.starts_with("  ") && l[2..].starts_with(|c: char| c.is_ascii_lowercase())).collect();
        assert!(lines.len() > 60, "{}", lines.len());
        let examples: Vec<String> = OPS.iter().flat_map(|op| op.examples.iter().map(|e| squeeze(e))).collect();
        for line in lines {
            let line = squeeze(line);
            let words: Vec<&str> = line.split(' ').collect();
            let later = NOT_YET.iter().any(|(path, verbs)| words.len() > path.len() && words[..path.len()] == path[..] && verbs.contains(&words[path.len()]));
            assert!(later || examples.iter().any(|e| line.starts_with(e.as_str())), "“{line}” is neither an op's example nor in NOT_YET");
        }
    }

    #[test]
    fn kinds_and_effects_agree() {
        for op in OPS {
            assert_eq!(op.effect == EffectClass::Read, op.kind_v1 == Kind::Read, "{}", op.name);
            assert_eq!(op.effect == EffectClass::Read, matches!(op.group, OpGroup::Read | OpGroup::ForgeRead), "{}", op.name);
            if let Undo::Inverse(other) = op.undo {
                let inverse = find(other).unwrap_or_else(|| panic!("{}: no op {other}", op.name));
                assert_ne!(inverse.effect, EffectClass::Read, "{}", op.name);
            }
        }
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
