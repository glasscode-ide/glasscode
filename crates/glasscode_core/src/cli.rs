//! The `glasscode` command: everything a person can do in Glasscode, from a terminal or an
//! agent. It is the app's own binary (`Glasscode cli …`, which `~/.local/bin/glasscode` runs);
//! the work happens in the running app, this only sends the words and prints the answer.

use std::io::IsTerminal;
use std::time::Duration;

use crate::control::{self, CallError, Reply, Request, code};

const INTRO: &str = "glasscode: control Glasscode from a terminal or an agent. It talks to the running app.";

/// (topics, text) for `glasscode help [topic]`.
const HELP: &[(&[&str], &str)] = &[
    (
        &["board", "today", "task", "tasks"],
        r#"Today board
  board                          today's board (--all also lists Later, Recurring and Done)
  board add "<task>"             add a task. Words in it are read: "by Friday", "tomorrow",
                                 "Sam asked …", "waiting on Alex …", "every Monday …"
      --due <day>  --asked-by <name>  --waiting-on <name>  --with <name>  --via <where they asked>
      --list todo|asked|waiting|later  --link <ENG-42|#958|url>  --note "<text>"  --urgent
      --every day|weekday|monday…sunday|month-start|month-end   (a recurring task)
  board done <task>              tick it off (undo <task> puts it back)
  board snooze <task> [--until <day>]   out of sight until then (tomorrow unless said)
  board pin <task> / unpin <task>       Focus: three at most
  board move <task> <list>       asked, todo, waiting or later
  board note <task> "<text>"     add a note
  board edit <task> [--title …] [--due …|--due none] [--asked-by …] [--waiting-on …] [--urgent|--not-urgent]
  board remove <task>            take it off for good
  board show <task>
  board recurring                the recurring tasks (remove one with `board remove <id>`)
  <day>: today, tomorrow, fri, next monday, in 3 days, 2026-11-03, 30.10."#,
    ),
    (
        &["session", "sessions"],
        r#"Sessions
  session list [--project <p>] [--needs-you] [--server]
  session show <session>         details, its pull request, and the agent's last reply
  session read <session> [--last 5]     the conversation's last messages
  session new                    a new session (in the background; --open shows it)
      --project <p>  --agent claude|codex|shell  --account <name>  --title "<name>"
      --branch <new branch> | --worktree <path or branch> | --ticket ENG-42 | --pr 958 | --review 958
      --from <base branch>  --model <model>  --mode default|auto|plan|accept-edits
      --message "<first message>"  --open  --on-server
  session open <session>         show it in the window
  session rename <session> "<name>"
  session close <session> / restart <session>
  session send <session> "<message>"    type a message into another session
  session hand-off <session> --to claude|codex [--account <name>] [--note "<note>"] [--new-tab]
  session move <session> --to-server | --to-mac"#,
    ),
    (
        &["pr", "prs", "pull", "review"],
        r#"Pull requests
  pr review <n> [--project <p>] [--agent claude|codex]   a review session in its own worktree
  pr build <n> [--project <p>] [--message "<what to do>"] a session on the PR's own branch
  pr status <n> [--project <p>]   CI, reviews, conflicts, the preview, the review verdict
  pr list [--project <p>]         pull requests asking for your review
  pr rerun <n> [--project <p>]    re-run its failed checks
  pr comment <n> "<text>"         comment on it
  pr label <n> --add <label> | --remove <label>    e.g. the preview label
  pr open-draft [<session>]       push the session's branch and open a draft pull request"#,
    ),
    (
        &["project", "projects", "worktree", "worktrees"],
        r#"Projects and worktrees
  project list
  project import <folder or git URL> [--name <name>] [--into <folder>]
  project remove <p>              from Glasscode (the folder stays)
  project set-account <p> --github <login>
  worktree list [--project <p>]
  worktree new --branch <name> [--project <p>] [--from <base>]
  worktree rename <worktree> <new branch>
  worktree remove <worktree> [--delete-branch] [--force]
  worktree clean [--project <p>] [--merged] [--older 14d] [--dry-run]   the ones with nothing to lose
  worktree open <worktree>        in your editor"#,
    ),
    (
        &["account", "accounts", "ticket", "tickets", "linear"],
        r#"Accounts and tickets
  account list                    Claude, Codex, GitHub and Linear (never secrets)
  ticket list [--review] [--all]  your Linear tickets
  ticket show <ID>                the ticket, its description and comments
  ticket start <ID> [--agent …] [--project <p>] [--message "<what to do>"]
  ticket move <ID> "<status>"     e.g. "In Review"
  ticket comment <ID> "<text>""#,
    ),
    (
        &["manager", "mgr"],
        r#"The manager (the agent above every session; off until switched on in Settings › Manager)
  manager status                  whether it is on, what it does now, what waits
  manager ask "<question>" [--voice]      ask it and wait for its answer (--voice: short, spoken)
  manager say "<text>"            have it say something to the person
  manager brief                   today's update as text
  manager tour                    walk the person through today's update in the window
  manager remember "<fact>"       leave it a note it keeps (forget "<id or words>" drops one)
  manager memory [<query>]        what it remembers
  manager follow-up list [--all] | add "<what>" [--person <name>] | done <which> | drop <which>
  manager message list [--all]    the messages to teammates it drafted
  manager message draft --to <person> "<text>" [--why "<why>"] [--about <ENG-42|#958>]
  manager message approve <id> [--text "<new text>"] / reject <id>      only you, at a terminal
  manager show <session> [--tab pr|changes|files] [--section <part>] [--say "<text>"]
  manager show --view manager|sessions|today|tickets|usage|settings [--say "<text>"]
      <part>: checks, preview, reviews, comments, changes, files, reply
  manager scene '<json>'          draw a small picture it explains with (or --file <path>)
  manager check list | add "<what>" --every <minutes> | add "<what>" --in <minutes> | remove <id>
  manager voice-token [--identity <id>]   a token for one of your devices to join its voice room"#,
    ),
    (
        &["app", "open", "notify", "status", "log", "approvals", "wait", "whoami"],
        r#"The app
  status                          the app, the server, what needs you
  open today|tickets|sessions|usage|settings|<session>
  notify "<message>" [--title "<title>"]
  log [--limit 20]                what agents did
  approvals                       what waits for you to allow it
  wait <id>                       the result of something that waited for you
  whoami                          who Glasscode sees calling, and what it may do"#,
    ),
];

const FOOTER: &str = r#"Every command takes --json. <session> is an id, its start, or words from its title ("this" is
the session the command runs in); <task> is an id from `glasscode board`, a ticket (ENG-42) or a
pull request (#958); <p> is a project's name.

What an agent may not do by itself waits until you allow it in Glasscode: up to --timeout seconds
(110 unless said), or not at all with --no-wait. `glasscode wait <id>` gets the result later.
Exit codes: 0 done, 1 failed, 2 wrong usage, 3 Glasscode is not running, 4 waiting for you,
5 not allowed (you said no, or Settings › Automation)."#;

fn help(topic: Option<&str>) -> String {
    if let Some(topic) = topic {
        if let Some((_, text)) = HELP.iter().find(|(names, _)| names.contains(&topic.to_lowercase().as_str())) {
            return format!("{text}\n\n{FOOTER}");
        }
    }
    let all: Vec<&str> = HELP.iter().map(|(_, t)| *t).collect();
    format!("{INTRO}\n\n{}\n\n{FOOTER}", all.join("\n\n"))
}

/// Run the command; returns its exit code.
pub fn main(args: Vec<String>) -> i32 {
    let mut json = false;
    let mut wait = true;
    let mut timeout = Duration::from_secs(110);
    let mut rest = Vec::new();
    let mut iter = args.into_iter();
    // Everything after `--` is words for the command, never this program's own options.
    let mut words_only = false;
    while let Some(arg) = iter.next() {
        if words_only {
            rest.push(arg);
            continue;
        }
        let timeout_value = match arg.as_str() {
            "--timeout" => Some(iter.next()),
            other => other.strip_prefix("--timeout=").map(|v| Some(v.to_string())),
        };
        if let Some(value) = timeout_value {
            match value.and_then(|t| t.parse::<u64>().ok()) {
                Some(secs) => timeout = Duration::from_secs(secs),
                None => {
                    eprintln!("--timeout takes a number of seconds");
                    return code::USAGE;
                }
            }
            continue;
        }
        match arg.as_str() {
            "--json" => json = true,
            "--no-wait" => wait = false,
            "--" => {
                words_only = true;
                rest.push(arg);
            }
            _ => rest.push(arg),
        }
    }
    let first = rest.first().map(String::as_str);
    match first {
        None | Some("help" | "--help" | "-h") => {
            println!("{}", help(rest.get(1).map(String::as_str)));
            return code::OK;
        }
        Some("version" | "--version" | "-V") => {
            println!("glasscode {}", if crate::update::BUILD == 0 { "development build".to_string() } else { crate::update::BUILD.to_string() });
            return code::OK;
        }
        _ => {}
    }
    if rest.iter().take_while(|a| *a != "--").any(|a| a == "--help" || a == "-h") {
        println!("{}", help(first));
        return code::OK;
    }
    let request = Request {
        v: control::VERSION,
        args: rest,
        session: std::env::var("GLASSCODE_SESSION_ID").ok().filter(|s| !s.is_empty()),
        sig: std::env::var("GLASSCODE_SESSION_SIG").ok().filter(|s| !s.is_empty()),
        interactive: std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        cwd: std::env::current_dir().ok().map(|d| d.to_string_lossy().into_owned()),
    };
    let result = control::call(
        &request,
        |approval, summary| {
            eprintln!("Waiting for you to allow this in Glasscode (#{approval}): {summary}");
            wait
        },
        timeout,
    );
    match result {
        Ok(Reply::Done { code, text, data }) => {
            if json && code == code::OK {
                let value = if data.is_null() { serde_json::json!({ "text": text }) } else { data };
                println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
            } else if code == code::OK {
                if !text.is_empty() {
                    println!("{text}");
                }
            } else {
                eprintln!("{text}");
            }
            code
        }
        Ok(Reply::Waiting { .. }) => code::WAITING,
        Err(CallError::StillWaiting { approval, summary }) => {
            if json {
                println!("{}", serde_json::json!({ "waiting": approval, "summary": summary }));
            }
            eprintln!("Still waiting for you to allow it in Glasscode. Run `glasscode wait {approval}` for the result.");
            code::WAITING
        }
        Err(CallError::NotRunning(why)) => {
            eprintln!("Glasscode is not running, so there is nothing to talk to ({why}). Open Glasscode and try again.");
            code::NOT_RUNNING
        }
        Err(CallError::Broken(why)) => {
            eprintln!("Could not talk to Glasscode: {why}");
            code::FAILED
        }
    }
}

/// The skill Claude sessions get (and the instructions Codex sessions get): what the command
/// is for and when to use it.
pub const SKILL: &str = r#"---
name: glasscode
description: Control Glasscode, the app this session runs in, with the `glasscode` command. Use it to read and update the person's Today board (their tasks, who asked what, deadlines), to start new sessions (for a pull request, a ticket, a review) in their own worktrees, to see other sessions, accounts, projects, tickets and pull requests, and to rename, clean up or remove worktrees.
---

# The `glasscode` command

This session runs inside Glasscode. `glasscode` controls the app the way the person does with
the mouse: run `glasscode help` (or `glasscode help board`) for every command.

- **Today board.** The person reads it every morning. When they ask you to remember something,
  note a promise, or track who asked for what, add it: `glasscode board add "Send Sam the
  wording for the sign-up page" --asked-by Sam --via Slack --due fri`. Waiting on someone:
  `--waiting-on Alex`. `glasscode board` shows the board; `board done <id>` ticks a task off.
  Put tasks here when the person asks you to remember or track something.
- **Other work in parallel.** To build a pull request or a ticket in its own session instead of
  this one: `glasscode pr build 958`, `glasscode ticket start ENG-42`, or
  `glasscode session new --project my-app --branch fix/retry --message "…"`. New sessions start
  in the background; tell the person what you started.
- **Looking around.** `glasscode session list`, `session read <session>`, `pr status 958`,
  `account list`, `worktree list`, `ticket show ENG-42`. Add `--json` to parse the answer.
- **Cleaning up.** `glasscode worktree clean --merged --dry-run` first, then without `--dry-run`.
- **The manager.** When the person has switched on their manager (the agent above all their
  sessions), leave it a note with `glasscode manager remember "…"` (a fact it should keep) or
  `glasscode manager follow-up add "…" --person Sam` (something it should keep an eye on).

Some commands (closing sessions, removing worktrees, messaging other sessions, moving tickets)
wait until the person allows them in Glasscode; the command says so and exits with code 4 if
they have not answered in time. Do not retry those in a loop: tell the person it waits for them,
and use `glasscode wait <id>` later. Exit code 5 means they said no or their settings do not
allow it; do not try another way around it.
"#;

/// The short version for Codex's instructions.
pub const CODEX_INSTRUCTIONS: &str = "This session runs inside Glasscode, the app the person uses to run their agents. The `glasscode` command controls it (run `glasscode help`): the person's Today board (`glasscode board`, `glasscode board add \"…\" --asked-by Name --due fri` when they ask you to remember or track something), new sessions for other work (`glasscode pr build 958`, `glasscode ticket start ENG-42`, `glasscode session new …`), and the state of sessions, projects, worktrees, accounts, tickets and pull requests (`--json` to parse). When the person has switched on their manager (the agent above all their sessions), leave it a note with `glasscode manager remember \"…\"` (a fact it should keep) or `glasscode manager follow-up add \"…\"` (something it should keep an eye on). Commands that need the person's OK wait for them in Glasscode and exit with code 4 if they have not answered: tell the person and do not retry in a loop. Exit code 5 means not allowed.";

/// Where Claude sessions find the skill: a plugin folder Glasscode writes (`--plugin-dir`).
pub fn plugin_dir() -> std::path::PathBuf {
    crate::paths::root().join("agent").join("claude-plugin")
}

/// Write the plugin (once per version of the text).
pub fn write_plugin() -> anyhow::Result<std::path::PathBuf> {
    let dir = plugin_dir();
    let skill = dir.join("skills").join("glasscode").join("SKILL.md");
    if std::fs::read_to_string(&skill).ok().as_deref() != Some(SKILL) || !dir.join(".claude-plugin").join("plugin.json").is_file() {
        std::fs::create_dir_all(skill.parent().unwrap_or(&dir))?;
        std::fs::create_dir_all(dir.join(".claude-plugin"))?;
        let manifest = serde_json::json!({
            "name": "glasscode",
            "description": "The glasscode command: control Glasscode (the Today board, sessions, pull requests, worktrees) from this session.",
            "version": "1.0.0",
            "author": { "name": "Glasscode" },
        });
        std::fs::write(dir.join(".claude-plugin").join("plugin.json"), serde_json::to_vec_pretty(&manifest)?)?;
        std::fs::write(&skill, SKILL)?;
    }
    Ok(dir)
}

/// The folder the `glasscode` launcher is written to: `~/.local/bin`.
pub fn launcher_dir() -> std::path::PathBuf {
    crate::paths::home().join(".local/bin")
}

/// `~/.local/bin/glasscode`: runs this program's `cli`. Written by the app each time it starts
/// (an update moves nothing, but a copy of the app somewhere else should be the one it runs).
pub fn write_launcher(app_binary: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let path = launcher_dir().join("glasscode");
    let quoted = format!("'{}'", app_binary.to_string_lossy().replace('\'', r"'\''"));
    let script = format!("#!/bin/sh\n# Glasscode's command line, written by Glasscode. It talks to the running app.\nexec {quoted} cli \"$@\"\n");
    if std::fs::read_to_string(&path).ok().as_deref() == Some(script.as_str()) {
        return Ok(path);
    }
    std::fs::create_dir_all(path.parent().unwrap_or(&path))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_has_every_topic() {
        assert!(help(None).contains("board add"));
        assert!(help(Some("worktree")).contains("worktree clean"));
        assert!(!help(Some("pr")).contains("board add"));
        assert!(help(Some("manager")).contains("manager message draft") && !help(Some("manager")).contains("board add"));
        assert!(help(None).contains("manager follow-up list"));
        assert!(SKILL.contains("glasscode manager remember") && CODEX_INSTRUCTIONS.contains("glasscode manager follow-up add"));
        assert!(SKILL.starts_with("---\nname: glasscode\n"));
    }
}
