//! The ops that start, show, change and end sessions (M12.2). Summaries and examples are the
//! help's (`help.rs`), word for word (a test checks); the words, options and messages are those of
//! `session_plan` in `commands.rs`, which these replace.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, OpGroup, OpHost, OpSpec, Retry, Undo};
use crate::control::Kind;

const fn option(name: &'static str, kind: ArgKind, flags: &'static [&'static str], help: &'static str) -> ArgSpec {
    ArgSpec { name, kind, help, at: At::Flag(flags), ..ArgSpec::OPTION }
}

/// A session the command names first; `missing` is what its absence says.
const fn session(missing: &'static str) -> ArgSpec {
    ArgSpec {
        name: "session",
        kind: ArgKind::Session,
        required: true,
        help: "an id, its start, or words from its title (\"this\" is the session the command runs in)",
        at: At::Word(0),
        missing,
        default: None,
    }
}

/// The session the command runs in unless another is named.
pub(super) const THIS_SESSION: ArgSpec = ArgSpec { default: Some("this"), required: false, missing: "", ..session("") };

/// What a new session is given: `session new`'s options, which `pr review`, `pr build` and
/// `ticket start` take some of (`NewSpec` in the app).
pub(super) const PROJECT: ArgSpec = option("project", ArgKind::Project, &["project"], "a project's name (else the session's own, or the only one)");
pub(super) const AGENT: ArgSpec = option("agent", ArgKind::Word, &["agent"], "claude, codex or shell (else the project's own)");
pub(super) const ACCOUNT: ArgSpec = option("account", ArgKind::Word, &["account"], "the account it runs on, by its name (else the default one)");
pub(super) const TITLE: ArgSpec = option("title", ArgKind::Text, &["title"], "its name");
pub(super) const BRANCH: ArgSpec = option("branch", ArgKind::Word, &["branch"], "a new branch for it, in a worktree of its own");
pub(super) const MODEL: ArgSpec = option("model", ArgKind::Word, &["model"], "the model it uses");
pub(super) const MODE: ArgSpec = option("mode", ArgKind::Word, &["mode"], "default, auto, plan or accept-edits (Codex: full-auto, read-only or default)");
pub(super) const MESSAGE: ArgSpec = option("message", ArgKind::Text, &["message"], "its first message");
pub(super) const OPEN: ArgSpec = option("open", ArgKind::Switch, &["open"], "show it in the window (else it starts in the background)");
pub(super) const ON_SERVER: ArgSpec = option("on_server", ArgKind::Switch, &["on-server"], "start it on the server");

/// What starts sessions: the base of their entries.
pub(super) const START: OpSpec = OpSpec {
    group: OpGroup::SessionStart,
    effect: EffectClass::Create,
    retry: Retry::AtMostOnce,
    undo: Undo::Inverse("session.close"),
    host: OpHost::SessionHost,
    kind_v1: Kind::Start,
    ..OpSpec::READ
};

/// What changes a session that runs: the base of their entries.
const CONTROL: OpSpec = OpSpec { group: OpGroup::SessionControl, effect: EffectClass::LocalWrite, host: OpHost::SessionHost, ..OpSpec::READ };

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "session.child",
        cli: &["session", "child"],
        summary: "start a child session for one part of a session's work: fresh context, the same worktree, no new tools; its summary comes back (M61)",
        args: &[
            ArgSpec { name: "task", kind: ArgKind::Text, required: true, help: "the part it does, in full: it knows nothing else", at: At::Rest(0), missing: "session child \"<the part it does>\" [--session <parent>]", default: None },
            ArgSpec { at: At::Flag(&["session"]), help: "the session it is a part of (else the one the command runs in)", ..THIS_SESSION },
            TITLE,
        ],
        examples: &["session child \"<the part it does>\" [--session <parent>] [--title \"<name>\"]"],
        v2_only: true,
        ..START
    },
    OpSpec {
        name: "session.new",
        cli: &["session", "new"],
        aliases: &["start", "create"],
        summary: "a new session (in the background; --open shows it)",
        args: &[
            PROJECT,
            AGENT,
            ACCOUNT,
            TITLE,
            BRANCH,
            option("worktree", ArgKind::Word, &["worktree"], "a worktree it runs in: its folder or branch"),
            option("ticket", ArgKind::Word, &["ticket"], "a Linear ticket it works on, such as ENG-42"),
            option("pr", ArgKind::Pr, &["pr"], "a pull request it works on, on the PR's own branch"),
            option("review", ArgKind::Pr, &["review"], "a pull request it reviews, in its own worktree"),
            option("from", ArgKind::Word, &["from"], "the branch a new branch starts from"),
            MODEL,
            MODE,
            MESSAGE,
            OPEN,
            ON_SERVER,
        ],
        examples: &["session new"],
        ..START
    },
    OpSpec {
        name: "session.open",
        cli: &["session", "open"],
        aliases: &["focus", "go"],
        group: OpGroup::Navigate,
        effect: EffectClass::Navigate,
        host: OpHost::Window,
        kind_v1: Kind::Show,
        summary: "show it in the window",
        args: &[session("session open <session>")],
        examples: &["session open <session>"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "session.rename",
        cli: &["session", "rename"],
        aliases: &["name", "title"],
        undo: Undo::Inverse("session.rename"),
        kind_v1: Kind::Rename,
        summary: "give it a name of your own",
        args: &[
            session("session rename <session> \"<new name>\""),
            ArgSpec { name: "name", kind: ArgKind::Text, required: true, help: "its new name", at: At::Rest(1), missing: "session rename <session> \"<new name>\"", default: None },
        ],
        examples: &["session rename <session> \"<name>\""],
        ..CONTROL
    },
    OpSpec {
        name: "session.close",
        cli: &["session", "close"],
        effect: EffectClass::Destructive,
        kind_v1: Kind::Close,
        summary: "close it; its worktree stays, and a new session there can resume the conversation",
        args: &[session("session {verb} <session>")],
        examples: &["session close <session> / restart <session>"],
        ..CONTROL
    },
    OpSpec {
        name: "session.restart",
        cli: &["session", "restart"],
        effect: EffectClass::Destructive,
        retry: Retry::AtMostOnce,
        kind_v1: Kind::Close,
        summary: "restart it; it resumes its conversation",
        args: &[session("session {verb} <session>")],
        examples: &["session close <session> / restart <session>"],
        ..CONTROL
    },
    OpSpec {
        name: "session.send",
        cli: &["session", "send"],
        aliases: &["message", "tell"],
        group: OpGroup::SessionInput,
        effect: EffectClass::SessionInput,
        retry: Retry::AtMostOnce,
        kind_v1: Kind::Send,
        summary: "type a message into another session",
        args: &[
            session("session send <session> \"<message>\""),
            ArgSpec { name: "message", kind: ArgKind::Text, required: true, help: "what to type into it", at: At::Rest(1), missing: "session send <session> \"<message>\"", default: None },
            // Taken and not used, as it always was.
            option("no_enter", ArgKind::Switch, &["no-enter"], "taken, and not used: the message is always sent"),
        ],
        examples: &["session send <session> \"<message>\""],
        ..CONTROL
    },
    OpSpec {
        name: "session.hand_off",
        cli: &["session", "hand-off"],
        aliases: &["handoff", "switch"],
        group: OpGroup::SessionStart,
        effect: EffectClass::Create,
        retry: Retry::AtMostOnce,
        kind_v1: Kind::Move,
        summary: "hand it to the other agent, with a note of where it stands",
        args: &[
            THIS_SESSION,
            option("to", ArgKind::Word, &["to"], "claude or codex (else the other one)"),
            ACCOUNT,
            option("note", ArgKind::Text, &["note"], "a note for the agent that takes it over"),
            option("new_tab", ArgKind::Switch, &["new-tab"], "in a tab of its own, beside the old one"),
        ],
        examples: &["session hand-off <session> --to claude|codex [--account <name>] [--note \"<note>\"] [--new-tab]"],
        ..CONTROL
    },
    // The server-sync plan's `session.move`: "as worktree.move".
    OpSpec {
        name: "session.move",
        cli: &["session", "move"],
        group: OpGroup::Worktree,
        effect: EffectClass::Create,
        retry: Retry::Verifiable,
        undo: Undo::Inverse("session.move"),
        kind_v1: Kind::Move,
        summary: "move it to the server, with its worktree, once its agent is idle",
        args: &[
            THIS_SESSION,
            option("to_server", ArgKind::Switch, &["to-server"], "to the server (also when nothing is said)"),
            // `--to-pc` is `--to-mac` for people on Windows.
            option("to_this_computer", ArgKind::Switch, &["to-mac", "to-pc"], "back to this computer"),
        ],
        examples: &["session move <session> --to-server | --to-mac (or --to-pc)"],
        ..CONTROL
    },
];
