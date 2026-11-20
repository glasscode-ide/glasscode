//! The manager's ops that change something or ask it (M12.2). Summaries and examples are the
//! help's (`help.rs`), word for word (a test checks); the words, options and messages are those of
//! `manager_plan` in `manager_cmd.rs`, which these replace. They count as the Today board does in
//! Settings › Permissions, and showing, drawing and speaking as changing what the window shows.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, OpGroup, OpHost, OpSpec, Retry, Undo};
use crate::control::Kind;

const FOLLOW_UP: &str = "manager follow-up list [--all] | add \"<what>\" [--person <name>] | done <which> | drop <which>";
const MESSAGE: &str = "manager message list [--all] | draft --to <person> \"<text>\" [--why \"<why>\"] [--about <ref>] | approve <id> [--text \"<new text>\"] | reject <id>";
const CHECK: &str = "manager check list | add \"<what>\" --every <minutes> (or --in <minutes>, once) | remove <id>";

const fn option(name: &'static str, kind: ArgKind, flags: &'static [&'static str], help: &'static str) -> ArgSpec {
    ArgSpec { name, kind, help, at: At::Flag(flags), ..ArgSpec::OPTION }
}

/// The words after the command, as one text.
const fn text(name: &'static str, help: &'static str, missing: &'static str) -> ArgSpec {
    ArgSpec { name, kind: ArgKind::Text, required: true, help, at: At::Rest(0), missing, default: None }
}

/// What changes the manager's own data: the base of their entries.
const DATA: OpSpec = OpSpec { group: OpGroup::Manager, effect: EffectClass::LocalWrite, kind_v1: Kind::Board, ..OpSpec::READ };

/// What changes what the window shows: the base of their entries.
const SHOW: OpSpec = OpSpec { group: OpGroup::Navigate, effect: EffectClass::Navigate, host: OpHost::Window, kind_v1: Kind::Show, ..OpSpec::READ };

/// The arguments of `manager say`, `manager show` and `manager scene`, which `ui say`, `ui point`
/// and `ui scene` take too (`table_ui`).
pub(super) const SAY_ARGS: &[ArgSpec] = &[text("text", "what it says", "manager say \"<text>\"")];
pub(super) const SHOW_ARGS: &[ArgSpec] = &[
    // Not a `Session`: it may be one the manager sees on the server.
    ArgSpec { name: "session", kind: ArgKind::Text, help: "a session: its id, its start, or words from its title", at: At::Rest(0), ..ArgSpec::OPTION },
    option("tab", ArgKind::Word, &["tab"], "pr, changes or files"),
    option("section", ArgKind::Word, &["section"], "checks, preview, reviews, comments, changes, files or reply"),
    option("say", ArgKind::Text, &["say"], "what it says there"),
    option("view", ArgKind::Word, &["view"], "a screen instead of a session: manager, sessions, today, tickets, usage or settings"),
];
pub(super) const SCENE_ARGS: &[ArgSpec] = &[
    ArgSpec { name: "json", kind: ArgKind::Text, help: "the scene, as JSON", at: At::Rest(0), ..ArgSpec::OPTION },
    option("file", ArgKind::Word, &["file"], "a file with the scene, instead"),
];

pub(super) const OPS: &[OpSpec] = &[
    // A playbook's way to have the manager look (M51): a short wake, with no history, that can
    // only propose. The v2 engine's.
    OpSpec {
        name: "manager.wake",
        cli: &["manager", "wake"],
        retry: Retry::Idempotent,
        summary: "wake the manager about something: a short look that reads and proposes, never acts",
        args: &[text("reason", "what it is about", "manager wake \"<reason>\" [--project <p>]"), option("project", ArgKind::Project, &["project", "p"], "the project it is about")],
        examples: &["manager wake \"<reason>\" [--project <p>]"],
        v2_only: true,
        ..DATA
    },
    OpSpec {
        name: "manager.ask",
        cli: &["manager", "ask"],
        group: OpGroup::Manager,
        // A question to the manager's agent: it changes nothing by itself, so v1 counts it as a read.
        effect: EffectClass::SessionInput,
        retry: Retry::AtMostOnce,
        summary: "ask it and wait for its answer (--voice: short, spoken)",
        args: &[
            ArgSpec {
                name: "question",
                kind: ArgKind::Text,
                required: true,
                help: "the question",
                at: At::RestAfter(&["voice"], 0),
                missing: "manager ask \"<question>\" [--voice]",
                default: None,
            },
            option("voice", ArgKind::Switch, &["voice"], "a short answer, to be spoken"),
        ],
        examples: &["manager ask \"<question>\" [--voice]"],
        ..OpSpec::READ
    },
    OpSpec {
        name: "manager.say",
        cli: &["manager", "say"],
        retry: Retry::AtMostOnce,
        summary: "have it say something to the person",
        args: SAY_ARGS,
        examples: &["manager say \"<text>\""],
        ..SHOW
    },
    OpSpec {
        name: "manager.tour",
        cli: &["manager", "tour"],
        aliases: &["walk"],
        summary: "walk the person through today's update in the window",
        examples: &["manager tour"],
        ..SHOW
    },
    OpSpec {
        name: "manager.remember",
        cli: &["manager", "remember"],
        group: OpGroup::Memory,
        retry: Retry::AtMostOnce,
        undo: Undo::Inverse("manager.forget"),
        summary: "leave it a note it keeps (forget \"<id or words>\" drops one)",
        args: &[text("fact", "the fact it keeps", "manager remember \"<fact>\"")],
        examples: &["manager remember \"<fact>\""],
        ..DATA
    },
    OpSpec {
        name: "manager.forget",
        cli: &["manager", "forget"],
        group: OpGroup::Memory,
        effect: EffectClass::Destructive,
        summary: "drop a note it keeps",
        args: &[text("which", "the note's id, or words from it", "manager forget \"<id or words>\"")],
        examples: &["manager forget \"<id or words>\""],
        ..DATA
    },
    OpSpec {
        name: "manager.show",
        cli: &["manager", "show"],
        summary: "take the person to a session, a part of its panel, or a screen",
        args: SHOW_ARGS,
        examples: &["manager show <session> [--tab pr|changes|files] [--section <part>] [--say \"<text>\"]", "manager show --view manager|sessions|today|tickets|usage|settings [--say \"<text>\"]"],
        ..SHOW
    },
    OpSpec {
        name: "manager.scene",
        cli: &["manager", "scene"],
        aliases: &["draw"],
        summary: "draw a small picture it explains with (or --file <path>)",
        args: SCENE_ARGS,
        examples: &["manager scene '<json>'"],
        ..SHOW
    },
    OpSpec {
        name: "manager.voice_token",
        cli: &["manager", "voice-token"],
        // The Keychain of this Mac has the keys.
        host: OpHost::Window,
        summary: "a token for one of your devices to join its voice room",
        args: &[option("identity", ArgKind::Word, &["identity"], "who joins with it (else a new name)")],
        examples: &["manager voice-token [--identity <id>]"],
        ..OpSpec::READ
    },
    // ----- follow-ups -------------------------------------------------------------------------
    OpSpec {
        name: "manager.follow_up.add",
        cli: &["manager", "follow-up", "add"],
        aliases: &["new"],
        retry: Retry::AtMostOnce,
        undo: Undo::Inverse("manager.follow_up.drop"),
        summary: "have it keep an eye on something",
        args: &[text("what", "what it keeps an eye on", FOLLOW_UP), option("person", ArgKind::Word, &["person"], "who it is about")],
        examples: &[FOLLOW_UP],
        ..DATA
    },
    OpSpec {
        name: "manager.follow_up.done",
        cli: &["manager", "follow-up", "done"],
        aliases: &["close"],
        summary: "close a follow-up: it is done",
        args: &[text("which", "its id, or words from it", FOLLOW_UP)],
        examples: &[FOLLOW_UP],
        ..DATA
    },
    OpSpec {
        name: "manager.follow_up.drop",
        cli: &["manager", "follow-up", "drop"],
        aliases: &["dropped"],
        summary: "drop a follow-up: it is not needed any more",
        args: &[text("which", "its id, or words from it", FOLLOW_UP)],
        examples: &[FOLLOW_UP],
        ..DATA
    },
    // ----- messages to teammates --------------------------------------------------------------
    OpSpec {
        name: "manager.message.draft",
        cli: &["manager", "message", "draft"],
        aliases: &["new"],
        group: OpGroup::Message,
        retry: Retry::AtMostOnce,
        undo: Undo::Inverse("manager.message.reject"),
        summary: "draft a message to a teammate; it waits for the person's OK",
        args: &[
            ArgSpec { name: "to", kind: ArgKind::Word, required: true, help: "a teammate of Settings › Manager", at: At::Flag(&["to"]), missing: MESSAGE, default: None },
            text("text", "the message", MESSAGE),
            option("why", ArgKind::Text, &["why"], "why it is sent"),
            option("about", ArgKind::Word, &["about"], "what it is about: ENG-42 or #958"),
        ],
        examples: &["manager message draft --to <person> \"<text>\" [--why \"<why>\"] [--about <ENG-42|#958>]"],
        ..DATA
    },
    OpSpec {
        name: "manager.message.approve",
        cli: &["manager", "message", "approve"],
        aliases: &["allow", "send"],
        group: OpGroup::Message,
        effect: EffectClass::Outward,
        retry: Retry::AtMostOnce,
        human_only: true,
        summary: "allow a drafted message: it goes out (only you, at a terminal)",
        args: &[
            ArgSpec { name: "message", kind: ArgKind::Word, required: true, help: "its id, its start, or words from it", at: At::Word(0), missing: MESSAGE, default: None },
            option("text", ArgKind::Text, &["text"], "its new text"),
        ],
        examples: &["manager message approve <id> [--text \"<new text>\"] / reject <id>"],
        ..DATA
    },
    OpSpec {
        name: "manager.message.reject",
        cli: &["manager", "message", "reject"],
        aliases: &["deny"],
        group: OpGroup::Message,
        human_only: true,
        summary: "reject a drafted message (only you, at a terminal)",
        args: &[
            ArgSpec { name: "message", kind: ArgKind::Word, required: true, help: "its id, its start, or words from it", at: At::Word(0), missing: MESSAGE, default: None },
            // Taken, and not used.
            option("text", ArgKind::Text, &["text"], "taken, and not used"),
        ],
        examples: &["manager message approve <id> [--text \"<new text>\"] / reject <id>"],
        ..DATA
    },
    // ----- checks -----------------------------------------------------------------------------
    OpSpec {
        name: "manager.check.add",
        cli: &["manager", "check", "add"],
        aliases: &["new"],
        retry: Retry::AtMostOnce,
        undo: Undo::Inverse("manager.check.remove"),
        summary: "have it look at something by itself, on a schedule or once",
        args: &[
            text("what", "what it looks at", CHECK),
            option("every", ArgKind::Word, &["every"], "how often: minutes, or 30m, 2h, 1d (5 minutes at the least)"),
            option("in", ArgKind::Word, &["in"], "once, after so long: minutes, or 30m, 2h, 1d"),
        ],
        examples: &["manager check list | add \"<what>\" --every <minutes> | add \"<what>\" --in <minutes> | remove <id>"],
        ..DATA
    },
    OpSpec {
        name: "manager.check.remove",
        cli: &["manager", "check", "remove"],
        aliases: &["rm", "delete"],
        effect: EffectClass::Destructive,
        summary: "stop a check",
        args: &[text("which", "its id, its start, or words from it", CHECK)],
        examples: &["manager check list | add \"<what>\" --every <minutes> | add \"<what>\" --in <minutes> | remove <id>"],
        ..DATA
    },
];
