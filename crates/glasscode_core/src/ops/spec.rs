//! What an op is (design section 8): its name and its words on the command line, the kind of
//! effect it has, where it can run, and what it takes. The tables (`table_*.rs`) are made of
//! these, and `parse` and `json_schema` read nothing else.

use serde::{Deserialize, Serialize};

use crate::control::Kind;

/// What policy groups an op with: `authority/policy.json` names groups as well as ops (M21).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpGroup {
    Read,
    Navigate,
    Board,
    Memory,
    SessionStart,
    SessionInput,
    SessionControl,
    Worktree,
    Project,
    Settings,
    Tickets,
    ForgeRead,
    ForgeWrite,
    Message,
    Browser,
    Manager,
}

/// What running it does to the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    Read,
    Navigate,
    LocalWrite,
    SessionInput,
    Create,
    Destructive,
    Outward,
}

/// Whether it may run again when nobody knows if it ran (D9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retry {
    /// Running it twice leaves what running it once left.
    Idempotent,
    /// Whether it ran can be looked up before it runs again.
    Verifiable,
    AtMostOnce,
}

/// Where it can run (ops-8). Core: pure data, wherever the runtime is. Window: the Mac's UI
/// only. SessionHost: the app for local sessions, glasscoded for tmux sessions. Network: whoever
/// holds the login. An absent host answers `ErrorCode::Unavailable`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpHost {
    Core,
    Window,
    SessionHost,
    Network,
}

/// How it is taken back (D16): not at all, or by another op.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Undo {
    None,
    Inverse(&'static str),
}

/// What an argument holds, and what `parse` makes of the words for it. In the op's JSON each
/// is a string unless said.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgKind {
    /// One word, as given.
    Word,
    /// Words as one text, as given: a title, a note, a question.
    Text,
    /// On or off (`--urgent`). In the op: `true`, or left out. A switch may also read options of
    /// other arguments: it is on when any of them is given, with a value or without one, as v1's
    /// `Args::has` read them (`board add`'s `as_written`).
    Switch,
    /// A whole number. A word that is not one gives the default, as the commands always did.
    Count,
    /// A day in the words of `day.rs` ("fri", "next monday", "in 3 days", "2026-11-03"); "none",
    /// "no", "never" or "-" clears it. In the op: `"2026-11-20"`, or null for none.
    Day,
    /// The day a snoozed task comes back: after today; "none" (or nothing) is tomorrow.
    Until,
    /// A session (ops-17). The person at a terminal may name one by the start of its id or
    /// words from its title; any other caller gives its whole id. "this" is the caller's own.
    /// In the op: the whole id.
    Session,
    /// A project's name, id or folder, as given. Resolved by its handler (M12.2).
    Project,
    /// A task or item of the board (an id, ENG-42, #958), as given. Resolved by the board.
    Task,
    /// A pull request: 958, #958 or its URL. In the op: the number.
    Pr,
    /// A ticket such as ENG-42. In the op: in capitals. Looked up by its handler.
    Ticket,
    /// One of the board's lists: todo, asked, waiting or later (`board::List`).
    List,
    /// A section of the board (`board::Section`), by its key.
    Section,
    /// A section a task can be moved to, by its key.
    Movable,
    /// When a recurring task comes back (`board::Every`'s words: monday, weekday, month-end).
    Every,
    /// An option given any number of times. In the op: its values, in order.
    Many,
}

/// Where an argument is on the command line (ops-17). Word places count from the first word
/// after the op's own words (`board done <task>`: the task is word 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum At {
    Word(u8),
    /// Every word from that place on, as one text.
    Rest(u8),
    /// An option: `--name value`, `--name=value`, or `--name` alone for a switch. The first name
    /// is its own; the others are older or shorter ones, and the first given wins.
    Flag(&'static [&'static str]),
    /// An option, else the word at that place (`board move <task> <list>` or `--to <list>`).
    FlagOrWord(&'static [&'static str], u8),
}

impl At {
    /// The option names it answers to.
    pub fn flags(self) -> &'static [&'static str] {
        match self {
            At::Flag(names) | At::FlagOrWord(names, _) => names,
            At::Word(_) | At::Rest(_) => &[],
        }
    }
}

/// What a command does with options it does not know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Options {
    /// "Unknown option --x. This command takes: …" (exit 2).
    Refuse,
    /// Lets them through, as `status`, `whoami` and `approvals` always did.
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ArgSpec {
    /// Its field in the op's JSON.
    pub name: &'static str,
    pub kind: ArgKind,
    pub required: bool,
    pub help: &'static str,
    pub at: At,
    /// What a missing required one says (exit 2); `{verb}` is the word that named the command.
    pub missing: &'static str,
    /// Its words when none are given, read like given ones ("this", "20", "tomorrow").
    pub default: Option<&'static str>,
}

impl ArgSpec {
    /// An optional option with no default, for a table entry to start from.
    pub const OPTION: ArgSpec = ArgSpec { name: "", kind: ArgKind::Word, required: false, help: "", at: At::Flag(&[]), missing: "", default: None };
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct OpSpec {
    /// "session.open".
    pub name: &'static str,
    /// Its own words: `["session", "open"]`.
    pub cli: &'static [&'static str],
    /// Other words for the last word of `cli` (ops-17). `""`: it may be left out (`glasscode
    /// board` is `board list`); `"*"`: any other word stands for it (`account <anything>`).
    pub aliases: &'static [&'static str],
    pub group: OpGroup,
    pub effect: EffectClass,
    pub retry: Retry,
    pub undo: Undo,
    /// Never offered to a caller that is not the person.
    pub human_only: bool,
    /// Where it can run (ops-8).
    pub host: OpHost,
    /// The frozen kind of `state.json` (Settings › Permissions in v1).
    #[serde(serialize_with = "kind_key")]
    pub kind_v1: Kind,
    /// From `cli.rs`'s `HELP`, word for word, where it says what the command does.
    pub summary: &'static str,
    pub args: &'static [ArgSpec],
    /// Its lines of `HELP`, word for word, where it has one.
    pub examples: &'static [&'static str],
    pub options: Options,
}

fn kind_key<S: serde::Serializer>(kind: &Kind, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(kind.key())
}

impl OpSpec {
    /// A read that runs wherever the runtime is, takes nothing and refuses unknown options: what
    /// most entries of a table start from.
    pub const READ: OpSpec = OpSpec {
        name: "",
        cli: &[],
        aliases: &[],
        group: OpGroup::Read,
        effect: EffectClass::Read,
        retry: Retry::Idempotent,
        undo: Undo::None,
        human_only: false,
        host: OpHost::Core,
        kind_v1: Kind::Read,
        summary: "",
        args: &[],
        examples: &[],
        options: Options::Refuse,
    };

    /// The option names it knows, each once, in the order its error lists them.
    pub fn flags(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for name in self.args.iter().flat_map(|a| a.at.flags().iter().copied()) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }
}

/// The words before an op's last word (`session`, `manager follow-up`), with their other words
/// and what a word it does not know after it says (exit 2; `{word}` is that word).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Noun {
    pub path: &'static [&'static str],
    pub also: &'static [&'static str],
    pub unknown: &'static str,
}
