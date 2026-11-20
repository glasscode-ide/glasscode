//! `glasscode help [topic]`, made from the registry (M33.1): each line of a command is its op's
//! example and summary, so a new op is in the help once it is placed here. What the table keeps is
//! only the help's own layout: its sections and the topics that name them, the order of the lines,
//! the column a summary starts at, the option lines under a command and the notes on words.
//!
//! The text is the one `cli.rs` kept by hand until M33.1, byte for byte (`help_golden`).

use super::{OPS, OpSpec};

pub(super) const INTRO: &str = "glasscode: control Glasscode from a terminal or an agent. It talks to the running app.";

pub(super) const FOOTER: &str = r#"Every command takes --json. <session> is an id, its start, or words from its title ("this" is
the session the command runs in); <task> is an id from `glasscode board`, a ticket (ENG-42) or a
pull request (#958); <p> is a project's name.

What an agent may not do by itself waits until you allow it in Glasscode: up to --timeout seconds
(110 unless said), or not at all with --no-wait. `glasscode wait <id>` gets the result later.
Exit codes: 0 done, 1 failed, 2 wrong usage, 3 Glasscode is not running, 4 waiting for you,
5 not allowed (you said no, or Settings › Permissions)."#;

/// A section of the help: the topics that name it (`glasscode help board`), its title, its lines.
pub(super) struct Section {
    pub topics: &'static [&'static str],
    pub title: &'static str,
    pub lines: &'static [Line],
}

/// A line of a section.
pub(super) enum Line {
    /// An op's line: its example `example`, then its summary from column `col` (one space after
    /// the example when that is longer).
    Op { op: &'static str, example: usize, col: usize },
    /// The same, with the summary wrapped before `width` characters and the rest under the column.
    Wrapped { op: &'static str, example: usize, col: usize, width: usize },
    /// An op's line with help's own shorter words instead of the summary.
    Said { op: &'static str, example: usize, col: usize, text: &'static str },
    /// An op's example alone.
    Bare { op: &'static str, example: usize },
    /// A line that is no op's own: the options of the command above, a note on words.
    Text(&'static str),
}

impl Line {
    /// The op whose example the line is.
    pub(super) fn op(&self) -> Option<&'static str> {
        match self {
            Line::Op { op, .. } | Line::Wrapped { op, .. } | Line::Said { op, .. } | Line::Bare { op, .. } => Some(op),
            Line::Text(_) => None,
        }
    }

    fn render(&self) -> String {
        let spec = |name: &str| -> &'static OpSpec { OPS.iter().find(|op| op.name == name).unwrap_or_else(|| panic!("help names {name}, which is no op")) };
        let usage = |name: &str, example: usize| format!("  {}", spec(name).examples[example]);
        let at = |usage: String, col: usize| {
            let pad = col.saturating_sub(usage.chars().count()).max(1);
            format!("{usage}{}", " ".repeat(pad))
        };
        match self {
            Line::Op { op, example, col } => format!("{}{}", at(usage(op, *example), *col), spec(op).summary),
            Line::Said { op, example, col, text } => format!("{}{text}", at(usage(op, *example), *col)),
            Line::Bare { op, example } => usage(op, *example),
            Line::Text(text) => text.to_string(),
            Line::Wrapped { op, example, col, width } => {
                let mut lines = vec![at(usage(op, *example), *col)];
                let mut fresh = true;
                for word in spec(op).summary.split(' ') {
                    let last = lines.last_mut().expect("one line at least");
                    if !fresh && last.chars().count() + 1 + word.chars().count() > *width {
                        lines.push(format!("{}{word}", " ".repeat(*col)));
                        continue;
                    }
                    if !fresh {
                        last.push(' ');
                    }
                    last.push_str(word);
                    fresh = false;
                }
                lines.join("\n")
            }
        }
    }
}

impl Section {
    pub(super) fn text(&self) -> String {
        let mut out = vec![self.title.to_string()];
        out.extend(self.lines.iter().map(Line::render));
        out.join("\n")
    }
}

/// `glasscode help [topic]`: one section and the footer for a topic it knows, else everything.
pub fn help(topic: Option<&str>) -> String {
    if let Some(section) = topic.and_then(|topic| SECTIONS.iter().find(|s| s.topics.contains(&topic.to_lowercase().as_str()))) {
        return format!("{}\n\n{FOOTER}", section.text());
    }
    let all: Vec<String> = SECTIONS.iter().map(Section::text).collect();
    format!("{INTRO}\n\n{}\n\n{FOOTER}", all.join("\n\n"))
}

/// The help, in the order it prints.
pub(super) const SECTIONS: &[Section] = &[
    Section {
        topics: &["board", "today", "task", "tasks"],
        title: "Today board",
        lines: &[
            Line::Op { op: "board.list", example: 0, col: 33 },
            Line::Wrapped { op: "board.add", example: 0, col: 33, width: 92 },
            Line::Text("      --due <day>  --asked-by <name>  --waiting-on <name>  --with <name>  --via <where they asked>"),
            Line::Text("      --list todo|asked|waiting|later  --link <ENG-42|#958|url>  --note \"<text>\"  --urgent"),
            Line::Text("      --every day|weekday|monday…sunday|month-start|month-end   (a recurring task)"),
            Line::Op { op: "board.done", example: 0, col: 33 },
            Line::Op { op: "board.snooze", example: 0, col: 40 },
            Line::Op { op: "board.pin", example: 0, col: 40 },
            Line::Op { op: "board.move", example: 0, col: 33 },
            Line::Op { op: "board.note", example: 0, col: 33 },
            Line::Bare { op: "board.edit", example: 0 },
            Line::Op { op: "board.remove", example: 0, col: 33 },
            Line::Bare { op: "board.show", example: 0 },
            Line::Op { op: "board.recurring", example: 0, col: 33 },
            Line::Text("  <day>: today, tomorrow, fri, next monday, in 3 days, 2026-11-03, 30.10."),
        ],
    },
    Section {
        topics: &["session", "sessions"],
        title: "Sessions",
        lines: &[
            Line::Bare { op: "session.list", example: 0 },
            Line::Op { op: "session.show", example: 0, col: 33 },
            Line::Op { op: "session.read", example: 0, col: 40 },
            Line::Op { op: "session.new", example: 0, col: 33 },
            Line::Text("      --project <p>  --agent claude|codex|shell  --account <name>  --title \"<name>\""),
            Line::Text("      --branch <new branch> | --worktree <path or branch> | --ticket ENG-42 | --pr 958 | --review 958"),
            Line::Text("      --from <base branch>  --model <model>  --mode default|auto|plan|accept-edits"),
            Line::Text("      --message \"<first message>\"  --open  --on-server"),
            Line::Op { op: "session.open", example: 0, col: 33 },
            Line::Bare { op: "session.rename", example: 0 },
            Line::Bare { op: "session.close", example: 0 },
            Line::Op { op: "session.send", example: 0, col: 40 },
            Line::Bare { op: "session.hand_off", example: 0 },
            Line::Bare { op: "session.move", example: 0 },
        ],
    },
    Section {
        topics: &["pr", "prs", "pull", "review"],
        title: "Pull requests",
        lines: &[
            Line::Op { op: "pr.review", example: 0, col: 57 },
            Line::Op { op: "pr.build", example: 0, col: 58 },
            Line::Op { op: "pr.status", example: 0, col: 34 },
            Line::Op { op: "pr.list", example: 0, col: 34 },
            Line::Op { op: "pr.rerun", example: 0, col: 34 },
            Line::Op { op: "pr.comment", example: 0, col: 34 },
            Line::Said { op: "pr.label", example: 0, col: 51, text: "e.g. the preview label" },
            Line::Op { op: "pr.open_draft", example: 0, col: 34 },
        ],
    },
    Section {
        topics: &["project", "projects", "worktree", "worktrees"],
        title: "Projects and worktrees",
        lines: &[
            Line::Bare { op: "project.list", example: 0 },
            Line::Bare { op: "project.import", example: 0 },
            Line::Op { op: "project.remove", example: 0, col: 34 },
            Line::Bare { op: "project.set_account", example: 0 },
            Line::Bare { op: "worktree.list", example: 0 },
            Line::Bare { op: "worktree.new", example: 0 },
            Line::Bare { op: "worktree.rename", example: 0 },
            Line::Bare { op: "worktree.remove", example: 0 },
            Line::Op { op: "worktree.clean", example: 0, col: 72 },
            Line::Op { op: "worktree.open", example: 0, col: 34 },
        ],
    },
    Section {
        topics: &["account", "accounts", "ticket", "tickets", "linear"],
        title: "Accounts and tickets",
        lines: &[
            Line::Op { op: "account.list", example: 0, col: 34 },
            Line::Op { op: "ticket.list", example: 0, col: 34 },
            Line::Op { op: "ticket.show", example: 0, col: 34 },
            Line::Bare { op: "ticket.start", example: 0 },
            Line::Said { op: "ticket.move", example: 0, col: 34, text: "e.g. \"In Review\"" },
            Line::Bare { op: "ticket.comment", example: 0 },
        ],
    },
    Section {
        topics: &["manager", "mgr"],
        title: "The manager (the agent above every session; off until switched on in Settings › Manager)",
        lines: &[
            Line::Op { op: "manager.status", example: 0, col: 34 },
            Line::Op { op: "manager.ask", example: 0, col: 42 },
            Line::Op { op: "manager.say", example: 0, col: 34 },
            Line::Op { op: "manager.brief", example: 0, col: 34 },
            Line::Op { op: "manager.tour", example: 0, col: 34 },
            Line::Op { op: "manager.remember", example: 0, col: 34 },
            Line::Op { op: "manager.memory", example: 0, col: 34 },
            Line::Bare { op: "manager.follow_up.add", example: 0 },
            Line::Op { op: "manager.message.list", example: 0, col: 34 },
            Line::Bare { op: "manager.message.draft", example: 0 },
            Line::Said { op: "manager.message.approve", example: 0, col: 72, text: "only you, at a terminal" },
            Line::Bare { op: "manager.show", example: 0 },
            Line::Bare { op: "manager.show", example: 1 },
            Line::Text("      <part>: checks, preview, reviews, comments, changes, files, reply"),
            Line::Op { op: "manager.scene", example: 0, col: 34 },
            Line::Bare { op: "manager.check.add", example: 0 },
            Line::Op { op: "manager.voice_token", example: 0, col: 42 },
        ],
    },
    Section {
        topics: &["app", "open", "notify", "status", "log", "approvals", "wait", "whoami"],
        title: "The app",
        lines: &[
            Line::Op { op: "app.status", example: 0, col: 34 },
            Line::Bare { op: "app.open", example: 0 },
            Line::Bare { op: "app.notify", example: 0 },
            Line::Op { op: "app.log", example: 0, col: 34 },
            Line::Op { op: "app.approvals", example: 0, col: 34 },
            Line::Op { op: "app.wait", example: 0, col: 34 },
            Line::Op { op: "app.whoami", example: 0, col: 34 },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The topics `help` prints a section for, and none.
    fn every_topic() -> Vec<Option<&'static str>> {
        std::iter::once(None).chain(SECTIONS.iter().flat_map(|s| s.topics.iter().map(|t| Some(*t)))).collect()
    }

    /// What `glasscode help` and `glasscode help <topic>` print, as committed: the text kept by
    /// hand in `cli.rs` until M33.1, byte for byte.
    #[test]
    fn help_golden() {
        let golden = include_str!("help.golden.txt");
        let printed: Vec<String> = every_topic().into_iter().map(|topic| format!("== help {}\n{}\n", topic.unwrap_or(""), help(topic))).collect();
        let printed = printed.join("");
        if printed != golden && std::env::var_os("GLASSCODE_BLESS").is_some() {
            std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ops/help.golden.txt"), &printed).unwrap();
            return;
        }
        assert_eq!(printed, golden, "the help changed: check it, then run with GLASSCODE_BLESS=1 to keep it");
    }

    #[test]
    fn an_unknown_topic_prints_everything() {
        assert_eq!(help(Some("nothing-like-it")), help(None));
        assert_eq!(help(Some("BOARD")), help(Some("board")));
    }

    #[test]
    fn topics_are_unique() {
        let topics: Vec<&str> = SECTIONS.iter().flat_map(|s| s.topics.iter().copied()).collect();
        let mut sorted = topics.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), topics.len());
    }

    /// Each line of a command is a registered op's example, and no op is placed twice for the same
    /// example.
    #[test]
    fn every_line_is_an_op_once() {
        let mut seen = Vec::new();
        for line in SECTIONS.iter().flat_map(|s| s.lines.iter()) {
            match line {
                Line::Op { op, example, .. } | Line::Wrapped { op, example, .. } | Line::Said { op, example, .. } | Line::Bare { op, example } => {
                    let spec = OPS.iter().find(|spec| spec.name == *op).unwrap_or_else(|| panic!("{op} is no op"));
                    assert!(*example < spec.examples.len(), "{op} has no example {example}");
                    assert!(!spec.v2_only, "{op}: the help is v1's");
                    assert!(!seen.contains(&(op, example)), "{op} {example} twice");
                    seen.push((op, example));
                }
                Line::Text(text) => assert!(!text.starts_with("  ") || text.starts_with("      ") || text.starts_with("  <"), "“{text}” looks like a command: make it an op's line"),
            }
        }
    }
}
