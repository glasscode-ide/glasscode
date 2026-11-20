//! The `glasscode` command: everything a person can do in Glasscode, from a terminal or an
//! agent. On the Mac it is the app's own binary (`Glasscode cli …`, which `~/.local/bin/glasscode`
//! runs); on Windows it is the console program `glasscode-cli.exe`, which the app copies to
//! `~/.glasscode/bin/glasscode.exe`. The work happens in the running app, this only sends the
//! words and prints the answer.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::control::{self, CallError, Reply, Request, code};

/// `glasscode help [topic]`, made from the registry (`ops::help`).
fn help(topic: Option<&str>) -> String {
    crate::ops::help(topic)
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
        // What Claude Code runs in a session on Windows (`agents::launch`), not words for people.
        Some("hook") => return hook(),
        Some("statusline") => return statusline(),
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

// ----- for Claude Code in a session (Windows) --------------------------------------------------

/// `glasscode hook`, the command of each Claude Code hook in a session on Windows: what the hook
/// gets on stdin, and a line end, go to the session's events file (`GLASSCODE_EVENTS`), where the
/// app reads them. It does not reach the app, and prints nothing: Claude Code would add that to
/// the conversation.
fn hook() -> i32 {
    let Some(events) = std::env::var_os("GLASSCODE_EVENTS").filter(|v| !v.is_empty()).map(PathBuf::from) else {
        eprintln!("glasscode hook: GLASSCODE_EVENTS is not set (Glasscode sets it in its sessions)");
        // Never 2: that would tell Claude Code to block the prompt.
        return code::FAILED;
    };
    match append_event(&mut std::io::stdin().lock(), &events) {
        Ok(()) => code::OK,
        Err(error) => {
            eprintln!("glasscode hook: could not write to {}: {error}", events.display());
            code::FAILED
        }
    }
}

/// Add all of `input` and a line end to the events file in one write, so that the lines of two
/// hooks never mix. Claude Code ends its input with a line end already: then it gets no second.
fn append_event(input: &mut impl std::io::Read, events: &Path) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut event = Vec::new();
    input.read_to_end(&mut event)?;
    if !event.ends_with(b"\n") {
        event.push(b'\n');
    }
    if let Some(dir) = events.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new().create(true).append(true).open(events)?.write_all(&event)
}

/// `glasscode statusline`, the status line command of Claude Code in a session on Windows, as
/// `usage::STATUSLINE_COMMAND` is on the Mac: it keeps the input for Usage (`GLASSCODE_LIMITS`),
/// then shows the person's own status line (`GLASSCODE_STATUSLINE`) with the same input.
fn statusline() -> i32 {
    use std::io::{Read as _, Write as _};
    let mut input = Vec::new();
    if let Err(error) = std::io::stdin().lock().read_to_end(&mut input) {
        eprintln!("glasscode statusline: could not read the status line input: {error}");
        return code::FAILED;
    }
    let limits = std::env::var_os("GLASSCODE_LIMITS").filter(|v| !v.is_empty()).map(PathBuf::from);
    let own = std::env::var("GLASSCODE_STATUSLINE").ok().filter(|c| !c.trim().is_empty());
    let (shown, exit) = status_line(&input, limits.as_deref(), own.as_deref());
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&shown).and_then(|()| stdout.flush()).is_err() {
        return code::FAILED;
    }
    exit
}

/// What `statusline` prints, and its exit code.
fn status_line(input: &[u8], limits: Option<&Path>, own: Option<&str>) -> (Vec<u8>, i32) {
    let mut exit = code::OK;
    if let Some(file) = limits
        && let Err(error) = crate::usage::save_limits(file, input)
    {
        eprintln!("glasscode statusline: could not keep the limits in {}: {error}", file.display());
        exit = code::FAILED;
    }
    let Some(own) = own else { return (Vec::new(), exit) };
    // The person wrote it for Claude Code, so it runs in Claude Code's shell.
    match run_with_input(crate::sys::claude_shell(own), input) {
        Ok(output) if output.status.success() => (output.stdout, exit),
        Ok(output) => (output.stdout, output.status.code().unwrap_or(code::FAILED)),
        Err(error) => {
            eprintln!("glasscode statusline: could not run your status line command: {error}");
            (Vec::new(), code::FAILED)
        }
    }
}

/// Run `command` with `input` on its stdin, and give its output.
fn run_with_input(mut command: std::process::Command, input: &[u8]) -> std::io::Result<std::process::Output> {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()?;
    let stdin = child.stdin.take();
    let input = input.to_vec();
    // From a thread of its own: a command that prints a lot before it reads would wait for this
    // one, and this one for it.
    let writer = std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(&input);
        }
    });
    let output = child.wait_with_output();
    let _ = writer.join();
    output
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

/// The modes of Glasscode's programs for other programs: `browser-mcp`, the Glasscode Browser's
/// MCP server that each agent starts, and `relay-connect`, an SSH ProxyCommand. The app's own
/// binary has them on the Mac, the console program on Windows. None for any other word.
pub fn run_mode(mode: &str, args: &[String]) -> Option<i32> {
    match mode {
        "browser-mcp" => Some(match crate::browser::mcp::serve(args) {
            Ok(()) => code::OK,
            Err(error) => {
                eprintln!("Glasscode browser: {error:#}");
                code::FAILED
            }
        }),
        "relay-connect" => {
            // TODO(glasscode): no relay ProxyCommand yet. The self-hosted relay is optional; reach
            // servers over SSH or Tailscale instead.
            eprintln!("Glasscode relay: a relay is not configured in this build (use SSH or Tailscale).");
            Some(255)
        }
        _ => None,
    }
}

// ----- the console program (Windows) -----------------------------------------------------------

/// The folder of the `glasscode` command on Windows. Each session has it first on its PATH.
pub fn command_dir() -> PathBuf {
    crate::paths::root().join("bin")
}

/// The `glasscode` command in `command_dir`.
pub fn command_file() -> PathBuf {
    command_dir().join(format!("glasscode{}", std::env::consts::EXE_SUFFIX))
}

/// The console program next to the app's own program: cargo builds the two into one folder, and
/// the installer puts them into one folder.
pub fn console_program(app: &Path) -> PathBuf {
    app.with_file_name(format!("glasscode-cli{}", std::env::consts::EXE_SUFFIX))
}

/// Copy the console program to `command_file` when it is not there or is different (another
/// build of the app). A window program cannot write to a console, so on Windows the command is a
/// program of its own.
pub fn install_command(program: &Path) -> anyhow::Result<PathBuf> {
    let target = command_file();
    copy_if_changed(program, &target)?;
    Ok(target)
}

/// Copy `program` to `target`, unless `target` has the same bytes already.
fn copy_if_changed(program: &Path, target: &Path) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let new = std::fs::read(program).with_context(|| format!("could not read {}", program.display()))?;
    if std::fs::read(target).is_ok_and(|old| old == new) {
        return Ok(());
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = target.with_extension("tmp");
    std::fs::write(&tmp, &new)?;
    if std::fs::rename(&tmp, target).is_err() {
        // A command that runs now keeps its file open: Windows lets it be renamed, not replaced.
        // The old file goes the next time, when nothing runs it.
        let old = target.with_extension("old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(target, &old).with_context(|| format!("could not replace {}", target.display()))?;
        std::fs::rename(&tmp, target)?;
    }
    Ok(())
}

/// Put `command_dir` first on the PATH of a session on Windows, so that `glasscode` works in
/// PowerShell, cmd.exe and Git Bash alike. On the Mac the launcher is in ~/.local/bin, which the
/// person's own PATH has.
pub fn add_command_to_path(env: &mut HashMap<String, String>) {
    if !cfg!(windows) {
        return;
    }
    let dir = command_dir();
    let old = env.get("PATH").cloned().unwrap_or_default();
    let rest: Vec<PathBuf> = if old.is_empty() { Vec::new() } else { std::env::split_paths(&old).filter(|p| *p != dir).collect() };
    match std::env::join_paths(std::iter::once(dir.clone()).chain(rest)) {
        Ok(path) => {
            env.insert("PATH".into(), path.to_string_lossy().into_owned());
        }
        Err(e) => log::warn!("could not put {} on the PATH of sessions: {e}", dir.display()),
    }
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

    #[test]
    fn a_hook_adds_its_input_and_one_line_end() {
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("events").join("s1.jsonl");
        append_event(&mut &br#"{"a":1}"#[..], &events).unwrap();
        assert_eq!(std::fs::read_to_string(&events).unwrap(), "{\"a\":1}\n");
        append_event(&mut &br#"{"a":1}"#[..], &events).unwrap();
        assert_eq!(std::fs::read_to_string(&events).unwrap(), "{\"a\":1}\n{\"a\":1}\n");
        // As Claude Code gives it, with its own line end.
        append_event(&mut &b"{\"b\":2}\n"[..], &events).unwrap();
        assert_eq!(std::fs::read_to_string(&events).unwrap(), "{\"a\":1}\n{\"a\":1}\n{\"b\":2}\n");
    }

    #[test]
    fn the_status_line_keeps_the_limits_and_shows_nothing_without_ones_own() {
        let dir = tempfile::tempdir().unwrap();
        let limits = dir.path().join("usage").join("limits").join("s1.json");
        let first = br#"{"rate_limits":{"five_hour":{"used_percentage":12}}}"#;
        assert_eq!(status_line(first, Some(&limits), None), (Vec::new(), code::OK));
        assert_eq!(std::fs::read(&limits).unwrap(), first);
        // The next update takes its place, and no file of the way is left.
        assert_eq!(status_line(b"{}", Some(&limits), None), (Vec::new(), code::OK));
        assert_eq!(std::fs::read(&limits).unwrap(), b"{}");
        assert_eq!(std::fs::read_dir(limits.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn the_status_line_shows_ones_own() {
        let (shown, exit) = status_line(b"{}", None, Some("echo mine"));
        assert_eq!((String::from_utf8_lossy(&shown).trim(), exit), ("mine", code::OK));
    }

    #[test]
    fn other_words_are_no_mode() {
        assert_eq!(run_mode("board", &[]), None);
        assert_eq!(run_mode("relay-connect", &[]), Some(255));
    }

    #[test]
    fn the_console_program_is_next_to_the_app() {
        let app = Path::new("target").join("debug").join(format!("glasscode{}", std::env::consts::EXE_SUFFIX));
        assert_eq!(console_program(&app), Path::new("target").join("debug").join(format!("glasscode-cli{}", std::env::consts::EXE_SUFFIX)));
    }

    #[test]
    fn the_command_is_copied_only_when_it_changed() {
        let dir = tempfile::tempdir().unwrap();
        let (program, target) = (dir.path().join("glasscode-cli.exe"), dir.path().join("bin").join("glasscode.exe"));
        std::fs::write(&program, "one").unwrap();
        copy_if_changed(&program, &target).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "one");
        // The same bytes: the file stays as it is (a new copy would have a new time).
        let long_ago = filetime::FileTime::from_unix_time(1_000_000_000, 0);
        filetime::set_file_mtime(&target, long_ago).unwrap();
        copy_if_changed(&program, &target).unwrap();
        assert_eq!(filetime::FileTime::from_last_modification_time(&std::fs::metadata(&target).unwrap()), long_ago);
        std::fs::write(&program, "two").unwrap();
        copy_if_changed(&program, &target).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "two");
        assert!(copy_if_changed(&dir.path().join("missing.exe"), &target).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn the_command_is_replaced_while_it_runs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("bin").join("glasscode.exe");
        let ping = Path::new(&std::env::var_os("SystemRoot").unwrap()).join(r"System32\PING.EXE");
        copy_if_changed(&ping, &target).unwrap();
        // A command that runs keeps its file open.
        let mut running = crate::sys::command(&target).args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let program = dir.path().join("glasscode-cli.exe");
        std::fs::write(&program, "new build").unwrap();
        let replaced = copy_if_changed(&program, &target);
        let _ = running.kill();
        let _ = running.wait();
        replaced.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new build");
    }

    #[cfg(windows)]
    #[test]
    fn sessions_find_the_command_first() {
        let mut env = HashMap::from([("PATH".to_string(), format!(r"C:\Tools;{};C:\Windows", command_dir().display()))]);
        add_command_to_path(&mut env);
        assert_eq!(env["PATH"], format!(r"{};C:\Tools;C:\Windows", command_dir().display()));
        let mut empty = HashMap::new();
        add_command_to_path(&mut empty);
        assert_eq!(empty["PATH"], command_dir().display().to_string());
    }
}
