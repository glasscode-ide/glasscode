//! The Today board's ops (M12.1). Summaries and examples are `cli.rs`'s `HELP`, word for word
//! (a test checks); the words, options and messages are those of `board_plan` in `commands.rs`.

use super::spec::{ArgKind, ArgSpec, At, EffectClass, Noun, OpGroup, OpSpec, Retry, Undo};
use crate::control::Kind;

pub(super) const NOUNS: &[Noun] = &[Noun { path: &["board"], also: &["today", "task", "tasks", "todo"], unknown: "Unknown board command “{word}”. `glasscode help board` lists them." }];

/// What changes the board: the base of its entries that write.
const WRITE: OpSpec = OpSpec { group: OpGroup::Board, effect: EffectClass::LocalWrite, kind_v1: Kind::Board, ..OpSpec::READ };

const TASK: ArgSpec = ArgSpec {
    name: "task",
    kind: ArgKind::Task,
    required: true,
    help: "an id from `glasscode board`, a ticket (ENG-42) or a pull request (#958)",
    at: At::Word(0),
    missing: "Say which task: `glasscode board {verb} <id>` (ids are in `glasscode board`).",
    default: None,
};

const fn option(name: &'static str, kind: ArgKind, flags: &'static [&'static str], help: &'static str) -> ArgSpec {
    ArgSpec { name, kind, help, at: At::Flag(flags), ..ArgSpec::OPTION }
}

const DUE: ArgSpec = option("due", ArgKind::Day, &["due"], "the day it is due (none: no day)");
const ASKED_BY: ArgSpec = option("asked_by", ArgKind::Word, &["asked-by"], "who asked for it (it goes to Asked of me)");
const WAITING_ON: ArgSpec = option("waiting_on", ArgKind::Word, &["waiting-on"], "who it waits for (it goes to Waiting on others)");
const WITH: ArgSpec = option("with", ArgKind::Word, &["with"], "who else it is about");
const VIA: ArgSpec = option("via", ArgKind::Word, &["via"], "where they asked");
const LINKS: ArgSpec = option("links", ArgKind::Many, &["link"], "ENG-42, #958 or a URL; may be given more than once");
const URGENT: ArgSpec = option("urgent", ArgKind::Switch, &["urgent"], "it is urgent");

pub(super) const OPS: &[OpSpec] = &[
    OpSpec {
        name: "board.list",
        cli: &["board", "list"],
        aliases: &["", "today", "ls"],
        summary: "today's board (--all also lists Later, Recurring and Done)",
        args: &[option("all", ArgKind::Switch, &["all"], "also Later, Recurring and Done"), option("section", ArgKind::Section, &["section"], "only this section")],
        examples: &["board"],
        ..OpSpec::READ
    },
    OpSpec { name: "board.show", cli: &["board", "show"], summary: "one task or item, with its notes", args: &[TASK], examples: &["board show <task>"], ..OpSpec::READ },
    OpSpec {
        name: "board.add",
        cli: &["board", "add"],
        aliases: &["new", "+"],
        retry: Retry::AtMostOnce,
        undo: Undo::Inverse("board.remove"),
        summary: "add a task. Words in it are read: \"by Friday\", \"tomorrow\", \"Sam asked …\", \"waiting on Alex …\", \"every Monday …\"",
        args: &[
            ArgSpec {
                name: "title",
                kind: ArgKind::Text,
                required: true,
                help: "the task; its words are read for a day, a person and how often (unless a list, a day, a person or --every is given)",
                at: At::Rest(0),
                missing: "board add \"<what to do>\" [--due fri] [--asked-by <name>] … (`glasscode help board`)",
                default: None,
            },
            DUE,
            ASKED_BY,
            WAITING_ON,
            WITH,
            VIA,
            option("list", ArgKind::List, &["list", "section"], "todo, asked, waiting or later"),
            LINKS,
            option("note", ArgKind::Text, &["note"], "a note on it"),
            URGENT,
            option("every", ArgKind::Every, &["every"], "day, weekday, monday…sunday, month-start or month-end (a recurring task)"),
        ],
        examples: &["board add \"<task>\""],
        ..WRITE
    },
    OpSpec {
        name: "board.done",
        cli: &["board", "done"],
        aliases: &["check", "tick"],
        undo: Undo::Inverse("board.undo"),
        summary: "tick it off (undo <task> puts it back)",
        args: &[TASK],
        examples: &["board done <task>"],
        ..WRITE
    },
    OpSpec {
        name: "board.undo",
        cli: &["board", "undo"],
        aliases: &["undone", "reopen"],
        undo: Undo::Inverse("board.done"),
        summary: "put a ticked-off task back on the board",
        args: &[TASK],
        examples: &["board undo <task>"],
        ..WRITE
    },
    OpSpec {
        name: "board.snooze",
        cli: &["board", "snooze"],
        aliases: &["later", "tomorrow"],
        undo: Undo::Inverse("board.unsnooze"),
        summary: "out of sight until then (tomorrow unless said)",
        args: &[TASK, ArgSpec { name: "until", kind: ArgKind::Until, help: "the day it comes back", at: At::FlagOrWord(&["until"], 1), default: Some("tomorrow"), ..ArgSpec::OPTION }],
        examples: &["board snooze <task> [--until <day>]"],
        ..WRITE
    },
    OpSpec { name: "board.unsnooze", cli: &["board", "unsnooze"], aliases: &["back"], summary: "bring a snoozed task back to today", args: &[TASK], examples: &["board unsnooze <task>"], ..WRITE },
    OpSpec {
        name: "board.pin",
        cli: &["board", "pin"],
        aliases: &["focus"],
        undo: Undo::Inverse("board.unpin"),
        summary: "Focus: three at most",
        args: &[TASK],
        examples: &["board pin <task> / unpin <task>"],
        ..WRITE
    },
    OpSpec {
        name: "board.unpin",
        cli: &["board", "unpin"],
        aliases: &["unfocus"],
        undo: Undo::Inverse("board.pin"),
        summary: "Focus: three at most",
        args: &[TASK],
        examples: &["board pin <task> / unpin <task>"],
        ..WRITE
    },
    OpSpec {
        name: "board.move",
        cli: &["board", "move"],
        summary: "asked, todo, waiting or later",
        args: &[
            TASK,
            ArgSpec {
                name: "to",
                kind: ArgKind::Movable,
                required: true,
                help: "asked, todo, waiting or later",
                at: At::FlagOrWord(&["to", "list", "section"], 1),
                missing: "board move <task> <asked|todo|waiting|later>",
                default: None,
            },
        ],
        examples: &["board move <task> <list>"],
        ..WRITE
    },
    OpSpec {
        name: "board.note",
        cli: &["board", "note"],
        retry: Retry::AtMostOnce,
        summary: "add a note",
        args: &[TASK, ArgSpec { name: "text", kind: ArgKind::Text, required: true, help: "the note", at: At::Rest(1), missing: "board note <task> \"<text>\"", default: None }],
        examples: &["board note <task> \"<text>\""],
        ..WRITE
    },
    OpSpec {
        name: "board.edit",
        cli: &["board", "edit"],
        summary: "change a task of your own: its title, day, people, links or urgency",
        args: &[
            TASK,
            option("title", ArgKind::Text, &["title"], "its new title"),
            DUE,
            ASKED_BY,
            WAITING_ON,
            ArgSpec { help: "who else it is about (none: nobody)", ..WITH },
            VIA,
            LINKS,
            URGENT,
            option("not_urgent", ArgKind::Switch, &["not-urgent"], "it is not urgent any more"),
            option("list", ArgKind::List, &["list"], "todo, asked, waiting or later"),
        ],
        examples: &["board edit <task> [--title …] [--due …|--due none] [--asked-by …] [--waiting-on …] [--urgent|--not-urgent]"],
        ..WRITE
    },
    OpSpec {
        name: "board.remove",
        cli: &["board", "remove"],
        aliases: &["rm", "delete"],
        effect: EffectClass::Destructive,
        summary: "take it off for good",
        args: &[TASK],
        examples: &["board remove <task>"],
        ..WRITE
    },
    OpSpec {
        name: "board.recurring",
        cli: &["board", "recurring"],
        aliases: &["repeat"],
        summary: "the recurring tasks (remove one with `board remove <id>`)",
        examples: &["board recurring"],
        ..OpSpec::READ
    },
];
