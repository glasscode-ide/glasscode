//! The ops that only read (M12.1): sessions, pull requests, projects, worktrees, accounts,
//! tickets, the app and the manager. Summaries and examples are the help's (`help.rs`, which prints them), word for word
//! (a test checks); the words, options and messages are those of the commands in `commands.rs`,
//! `commands2.rs` and `manager_cmd.rs`.

use super::spec::{ArgKind, ArgSpec, At, Noun, OpGroup, OpHost, OpSpec, Options};

const PROJECT: ArgSpec = ArgSpec { name: "project", kind: ArgKind::Project, help: "a project's name", at: At::Flag(&["project"]), ..ArgSpec::OPTION };

const SESSION: ArgSpec = ArgSpec {
    name: "session",
    kind: ArgKind::Session,
    help: "an id, its start, or words from its title (\"this\" is the session the command runs in)",
    at: At::Word(0),
    default: Some("this"),
    ..ArgSpec::OPTION
};

const ALL: ArgSpec = ArgSpec { name: "all", kind: ArgKind::Switch, help: "also the ones that are done", at: At::Flag(&["all"]), ..ArgSpec::OPTION };

pub(super) const NOUNS: &[Noun] = &[
    Noun { path: &["session"], also: &["sessions", "s"], unknown: "Unknown session command “{word}”. `glasscode help session` lists them." },
    Noun { path: &["pr"], also: &["prs", "pull"], unknown: "Unknown pr command “{word}”. `glasscode help pr` lists them." },
    Noun { path: &["project"], also: &["projects"], unknown: "Unknown project command “{word}”. `glasscode help project` lists them." },
    Noun { path: &["worktree"], also: &["worktrees", "wt"], unknown: "Unknown worktree command “{word}”. `glasscode help worktree` lists them." },
    // Any word after it lists the accounts (`account.list` takes "*").
    Noun { path: &["account"], also: &["accounts"], unknown: "" },
    Noun { path: &["ticket"], also: &["tickets"], unknown: "Unknown ticket command “{word}”. `glasscode help ticket` lists them." },
    Noun { path: &["manager"], also: &["mgr"], unknown: "Unknown manager command “{word}”. `glasscode help manager` lists them." },
    Noun {
        path: &["manager", "follow-up"],
        also: &["follow-ups", "followup", "followups", "follow"],
        unknown: "manager follow-up list [--all] | add \"<what>\" [--person <name>] | done <which> | drop <which>",
    },
    Noun {
        path: &["manager", "message"],
        also: &["messages", "msg"],
        unknown: "manager message list [--all] | draft --to <person> \"<text>\" [--why \"<why>\"] [--about <ref>] | approve <id> [--text \"<new text>\"] | reject <id>",
    },
    Noun { path: &["manager", "check"], also: &["checks"], unknown: "manager check list | add \"<what>\" --every <minutes> (or --in <minutes>, once) | remove <id>" },
];

pub(super) const OPS: &[OpSpec] = &[
    // ----- sessions -------------------------------------------------------------------------
    OpSpec {
        name: "session.list",
        cli: &["session", "list"],
        aliases: &["", "ls"],
        host: OpHost::SessionHost,
        summary: "the sessions: what each does, its project, branch and pull request",
        args: &[
            PROJECT,
            ArgSpec { name: "needs_you", kind: ArgKind::Switch, help: "only the ones that need you", at: At::Flag(&["needs-you"]), ..ArgSpec::OPTION },
            ArgSpec { name: "server", kind: ArgKind::Switch, help: "also the sessions on the server", at: At::Flag(&["server", "all"]), ..ArgSpec::OPTION },
        ],
        examples: &["session list [--project <p>] [--needs-you] [--server]"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "session.show",
        cli: &["session", "show"],
        aliases: &["info"],
        host: OpHost::SessionHost,
        summary: "details, its pull request, and the agent's last reply",
        args: &[SESSION],
        examples: &["session show <session>"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "session.read",
        cli: &["session", "read"],
        aliases: &["transcript", "log"],
        host: OpHost::SessionHost,
        summary: "the conversation's last messages",
        args: &[SESSION, ArgSpec { name: "last", kind: ArgKind::Count, help: "how many of its last messages", at: At::Flag(&["last"]), default: Some("6"), ..ArgSpec::OPTION }],
        examples: &["session read <session> [--last 5]"],
        ..OpSpec::READ
    },
    // ----- pull requests --------------------------------------------------------------------
    OpSpec {
        name: "pr.status",
        cli: &["pr", "status"],
        aliases: &["show", "view"],
        group: OpGroup::ForgeRead,
        host: OpHost::Network,
        summary: "CI, reviews, conflicts, the preview, the review verdict",
        args: &[ArgSpec { name: "number", kind: ArgKind::Pr, required: true, help: "958, #958 or its URL", at: At::Word(0), missing: "pr status <number>", ..ArgSpec::OPTION }, PROJECT],
        examples: &["pr status <n> [--project <p>]"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "pr.list",
        cli: &["pr", "list"],
        aliases: &["", "ls"],
        group: OpGroup::ForgeRead,
        host: OpHost::Network,
        summary: "pull requests asking for your review",
        // Taken and not used: the list has every project's.
        args: &[ArgSpec { name: "project", kind: ArgKind::Word, help: "a project's name (the list has every project's)", at: At::Flag(&["project"]), ..ArgSpec::OPTION }],
        examples: &["pr list [--project <p>]"],
        ..OpSpec::READ
    },
    // ----- projects and worktrees -----------------------------------------------------------
    OpSpec {
        name: "project.list",
        cli: &["project", "list"],
        aliases: &["", "ls"],
        summary: "the projects: their folders, sessions, worktrees and GitHub accounts",
        examples: &["project list"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "worktree.list",
        cli: &["worktree", "list"],
        aliases: &["", "ls"],
        summary: "each project's worktrees: branch, folder and sessions",
        args: &[PROJECT],
        examples: &["worktree list [--project <p>]"],
        ..OpSpec::READ
    },
    // ----- accounts and tickets -------------------------------------------------------------
    OpSpec {
        name: "account.list",
        cli: &["account", "list"],
        aliases: &["", "*"],
        summary: "Claude, Codex, GitHub and Linear (never secrets)",
        examples: &["account list"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "ticket.list",
        cli: &["ticket", "list"],
        aliases: &["", "ls"],
        host: OpHost::Network,
        summary: "your Linear tickets",
        args: &[
            ArgSpec { name: "review", kind: ArgKind::Switch, help: "the ones you review", at: At::Flag(&["review"]), ..ArgSpec::OPTION },
            ALL,
            ArgSpec { name: "mine", kind: ArgKind::Switch, help: "your own (they are listed anyway)", at: At::Flag(&["mine"]), ..ArgSpec::OPTION },
        ],
        examples: &["ticket list [--review] [--all]"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "ticket.show",
        cli: &["ticket", "show"],
        aliases: &["read", "view"],
        host: OpHost::Network,
        summary: "the ticket, its description and comments",
        args: &[ArgSpec { name: "ticket", kind: ArgKind::Ticket, required: true, help: "its ID, such as ENG-42", at: At::Word(0), missing: "ticket {verb} <ID>", ..ArgSpec::OPTION }],
        examples: &["ticket show <ID>"],
        ..OpSpec::READ
    },
    // ----- the app --------------------------------------------------------------------------
    OpSpec {
        name: "app.status",
        cli: &["status"],
        host: OpHost::Window,
        summary: "the app, the server, what needs you",
        examples: &["status"],
        options: Options::Ignore,
        ..OpSpec::READ
    },
    OpSpec {
        name: "app.log",
        cli: &["log"],
        summary: "what agents did",
        args: &[ArgSpec { name: "limit", kind: ArgKind::Count, help: "how many of the newest", at: At::Flag(&["limit"]), default: Some("20"), ..ArgSpec::OPTION }],
        examples: &["log [--limit 20]"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "app.approvals",
        cli: &["approvals"],
        aliases: &["pending"],
        host: OpHost::Window,
        summary: "what waits for you to allow it",
        examples: &["approvals"],
        options: Options::Ignore,
        ..OpSpec::READ
    },
    OpSpec {
        name: "app.whoami",
        cli: &["whoami"],
        summary: "who Glasscode sees calling, and what it may do",
        examples: &["whoami"],
        options: Options::Ignore,
        ..OpSpec::READ
    },
    // ----- the manager ----------------------------------------------------------------------
    OpSpec {
        name: "manager.status",
        cli: &["manager", "status"],
        aliases: &[""],
        summary: "whether it is on, what it does now, what waits",
        examples: &["manager status"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "manager.brief",
        cli: &["manager", "brief"],
        aliases: &["update"],
        summary: "today's update as text",
        examples: &["manager brief"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "manager.memory",
        cli: &["manager", "memory"],
        aliases: &["notes"],
        summary: "what it remembers",
        args: &[
            ArgSpec { name: "query", kind: ArgKind::Text, help: "words to look for (nothing: all of it)", at: At::Rest(0), ..ArgSpec::OPTION },
            ArgSpec { name: "limit", kind: ArgKind::Count, help: "how many at most", at: At::Flag(&["limit"]), default: Some("20"), ..ArgSpec::OPTION },
        ],
        examples: &["manager memory [<query>]"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "manager.follow_up.list",
        cli: &["manager", "follow-up", "list"],
        aliases: &["", "ls"],
        summary: "what it keeps an eye on",
        args: &[ArgSpec { help: "also the closed and dropped ones", ..ALL }],
        examples: &["manager follow-up list [--all]"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "manager.message.list",
        cli: &["manager", "message", "list"],
        aliases: &["", "ls"],
        summary: "the messages to teammates it drafted",
        args: &[ArgSpec { help: "also the sent and rejected ones", ..ALL }],
        examples: &["manager message list [--all]"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "manager.check.list",
        cli: &["manager", "check", "list"],
        aliases: &["", "ls"],
        summary: "what it looks at by itself, on a schedule",
        examples: &["manager check list"],
        ..OpSpec::READ
    },
];
