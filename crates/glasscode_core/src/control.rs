//! How the `glasscode` command talks to the running app.
//!
//! The command sends one JSON line with its words (`["session", "new", "--project", …]`) and
//! who is asking, and reads JSON lines back: perhaps `waiting` (the person has to allow it in
//! Glasscode first), then `done` with an exit code, text for people and data for `--json`.
//!
//! The way in is a Unix socket only this user can open (`~/.glasscode/control.sock`), or on
//! Windows a named pipe with the same rule (`sys::control`). Codex runs commands in a sandbox
//! that may not open sockets but may write in its worktree, so the command falls back to a
//! request file in `<worktree>/.glasscode/cli/`, which the app reads.
//!
//! Who is asking: sessions Glasscode starts get their id and a signature of it
//! (`GLASSCODE_SESSION_ID`, `GLASSCODE_SESSION_SIG`); the app checks the signature with a
//! secret only it knows, so a call can say which session (and which agent) made it. Anything
//! else is a program outside Glasscode, or the person at a terminal, which no request can prove
//! (`caller` decides by the channel).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const VERSION: u32 = 1;

/// Exit codes.
pub mod code {
    pub const OK: i32 = 0;
    pub const FAILED: i32 = 1;
    pub const USAGE: i32 = 2;
    /// Glasscode is not running (or cannot be reached).
    pub const NOT_RUNNING: i32 = 3;
    /// Waiting for the person to allow it.
    pub const WAITING: i32 = 4;
    /// The person said no, or the settings do not allow it.
    pub const DENIED: i32 = 5;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    /// The command's words, without the command's own options (`--json`, `--no-wait`).
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    /// The command says it runs at a terminal (stdin and stdout are one). Any program can say
    /// so: no caller is decided by it (`caller::by_channel`); it only names the caller ("A
    /// terminal outside Glasscode"). v1's rule that took such a terminal for the person (owner
    /// decision O13) went with M56 row 7; the field stays on the wire for older apps.
    #[serde(default)]
    pub interactive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// A typed op instead of words (`{"op": "board.done", "args": {…}}`): what `glasscode mcp`
    /// sends (M34). With it `args` is empty; an older app reads no words and says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<Value>,
    /// The manager's brain this request comes from, as the app signed it when it started the
    /// brain's MCP server (`mcp::brain_env`): attribution through the channel, not a self-report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brain: Option<BrainToken>,
    /// The caller does not wait for a long op (a clone, a push, a question to the manager): the
    /// app answers "started" with an id at once, and `wait <id>` gets the result (M33.2).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub detach: bool,
    /// The caller's own id for this call (ops-10), the idempotency key: a second request from the
    /// same caller with the same id gets the first one's answer and never runs again (`--op-id`,
    /// a typed tool's `op_id`). The v2 engine's; an older app ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_id: Option<String>,
}

/// An op id a caller may give (`Request.op_id`): 1 to 64 letters, digits and `._:-`.
pub fn valid_op_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

/// The lane and wake of a brain's MCP server, and the app's signature of them (M34).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainToken {
    pub lane: crate::caller::Lane,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<String>,
    pub sig: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    /// The person has to allow it first; the request stays open until they do.
    Waiting { approval: u64, summary: String },
    Done {
        code: i32,
        #[serde(default)]
        text: String,
        #[serde(default)]
        data: Value,
    },
}

impl Reply {
    pub fn ok(text: impl Into<String>, data: Value) -> Reply {
        Reply::Done { code: code::OK, text: text.into(), data }
    }

    pub fn fail(code: i32, text: impl Into<String>) -> Reply {
        Reply::Done { code, text: text.into(), data: Value::Null }
    }
}

// The socket code lives with the other operating system code; the app and the daemon still
// find it here.
#[cfg(unix)]
pub use crate::sys::control::peer_pid;
pub use crate::sys::control::{prepare_socket_dir, socket_path};

/// The request folder of a worktree (for commands that cannot open the socket).
pub fn drop_dir(worktree: &Path) -> PathBuf {
    worktree.join(".glasscode").join("cli")
}

/// Whether `dir` is a real folder, not a link to one (nor anything else).
fn real_folder(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir())
}

/// The worktree's request folder, when it and `.glasscode` are folders of the worktree's own. A
/// session writes its worktree (a sandboxed one writes only there), but the app reads, writes and
/// clears this folder outside any sandbox: made a link (to `/opt/homebrew/bin`, to the home
/// folder), it would have the app clear another folder's links, or answer there.
pub fn own_drop_dir(worktree: &Path) -> Option<PathBuf> {
    let dir = drop_dir(worktree);
    (real_folder(&worktree.join(".glasscode")) && real_folder(&dir)).then_some(dir)
}

/// Append one line to a request's answer file (`<name>.out` in a worktree's request folder),
/// never through a link: not at the file (on Unix the open refuses one, `O_NOFOLLOW`, and what it
/// opened must be a plain file with no other name, so not a hard link either), not at its folder
/// or at `.glasscode` (checked first). A session can plant one there between its request
/// and the answer, minutes apart when the person is asked; followed, the app would append the
/// answer, the request's words in it, to any file of the person's (`~/.zshrc`).
pub fn append_answer(path: &Path, line: &str) -> std::io::Result<()> {
    let refused = || std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("{} is not in a request folder of its own", path.display()));
    let dir = path.parent().ok_or_else(refused)?;
    if !real_folder(dir) || !dir.parent().is_some_and(real_folder) {
        return Err(refused());
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // Not a link, and never waiting on a pipe planted there.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).mode(0o600);
    }
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(refused());
    }
    let mut file = options.open(path)?;
    // What was opened is a plain file of this folder only: not a hard link to a file elsewhere,
    // which no flag of the open refuses.
    let opened = file.metadata()?;
    #[cfg(unix)]
    let one_name = {
        use std::os::unix::fs::MetadataExt as _;
        opened.nlink() == 1
    };
    #[cfg(not(unix))]
    let one_name = true;
    if !opened.is_file() || !one_name {
        return Err(refused());
    }
    writeln!(file, "{line}")
}

/// A session's signature: proof the app gave it this id.
pub fn sign(secret: &[u8], session: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(secret);
    hash.update(b":");
    hash.update(session.as_bytes());
    hash.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// Words and `--options` of a command line. `--name value` or `--name=value`; the options in
/// `SWITCHES` take no value. An option can repeat (`--link a --link b`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Args {
    pub words: Vec<String>,
    pub flags: BTreeMap<String, Vec<String>>,
}

/// Options that are on or off (everything else takes a value). `--review` is not one: it takes a
/// pull request in `session new`; `ticket list --review` works because nothing follows it.
pub const SWITCHES: &[&str] = &[
    "all", "open", "force", "delete-branch", "keep-branch", "dry-run", "merged", "no-session", "new-tab", "on-server", "to-server", "to-mac", "to-pc", "urgent", "not-urgent", "done", "needs-you",
    "server", "help", "plain", "quiet", "here", "yes",
];

/// `--name` alone, not a value: an option's name (lower case, digits, dashes).
fn looks_like_option(arg: &str) -> bool {
    arg.len() > 2 && arg.starts_with("--") && arg[2..].chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl Args {
    pub fn parse(args: &[String]) -> Args {
        let mut out = Args::default();
        let mut i = 0;
        let mut only_words = false;
        while i < args.len() {
            let arg = &args[i];
            i += 1;
            if only_words || !arg.starts_with("--") || arg == "--" {
                if arg == "--" && !only_words {
                    only_words = true;
                    continue;
                }
                out.words.push(arg.clone());
                continue;
            }
            let name = &arg[2..];
            if let Some((name, value)) = name.split_once('=') {
                out.flags.entry(name.to_string()).or_default().push(value.to_string());
                continue;
            }
            // A value may start with dashes ("--message '--force is bad'"); another option's name
            // may not be one.
            if SWITCHES.contains(&name) || i >= args.len() || looks_like_option(&args[i]) {
                out.flags.entry(name.to_string()).or_default().push(String::new());
                continue;
            }
            out.flags.entry(name.to_string()).or_default().push(args[i].clone());
            i += 1;
        }
        out
    }

    pub fn word(&self, index: usize) -> Option<&str> {
        self.words.get(index).map(String::as_str)
    }

    /// The words from `index` on, as one text (a message, a title).
    pub fn rest(&self, index: usize) -> String {
        self.words.get(index..).map(|w| w.join(" ")).unwrap_or_default()
    }

    pub fn flag(&self, name: &str) -> Option<&str> {
        self.flags.get(name).and_then(|v| v.last()).map(String::as_str).filter(|v| !v.is_empty())
    }

    pub fn all(&self, name: &str) -> Vec<String> {
        self.flags.get(name).map(|v| v.iter().filter(|s| !s.is_empty()).cloned().collect()).unwrap_or_default()
    }

    /// Given and not switched off (`--urgent=false`, `=no`, `=0`, `=off`).
    pub fn has(&self, name: &str) -> bool {
        self.flags.get(name).and_then(|v| v.last()).is_some_and(|v| !matches!(v.to_lowercase().as_str(), "false" | "no" | "0" | "off"))
    }

    /// Options this command does not know, for a helpful error.
    pub fn unknown(&self, known: &[&str]) -> Vec<String> {
        self.flags.keys().filter(|k| !known.contains(&k.as_str())).map(|k| format!("--{k}")).collect()
    }
}

// ----- the client ------------------------------------------------------------------------------

#[derive(Debug)]
pub enum CallError {
    /// Glasscode is not running or could not be reached.
    NotRunning(String),
    /// Gave up waiting for the person (the request stays open in Glasscode).
    StillWaiting { approval: u64, summary: String },
    Broken(String),
}

/// Send a request and wait for its answer. `on_waiting` hears when the person has to allow it
/// (return `false` to stop waiting then); `wait_for` is how long to wait for them.
pub fn call(request: &Request, mut on_waiting: impl FnMut(u64, &str) -> bool, wait_for: Duration) -> Result<Reply, CallError> {
    let socket = socket_path();
    let line = serde_json::to_string(request).map_err(|e| CallError::Broken(e.to_string()))?;
    match crate::sys::control::connect(&socket) {
        Ok(stream) => {
            let mut writer = stream.try_clone().map_err(|e| CallError::Broken(e.to_string()))?;
            writer.write_all(format!("{line}\n").as_bytes()).map_err(|e| CallError::NotRunning(e.to_string()))?;
            let started = Instant::now();
            let mut reader = BufReader::new(stream);
            let mut waiting: Option<(u64, String)> = None;
            loop {
                let left = wait_for.saturating_sub(started.elapsed());
                let _ = reader.get_ref().set_read_timeout(Some(if waiting.is_some() { left.max(Duration::from_millis(10)) } else { Duration::from_secs(600) }));
                let mut buf = String::new();
                match reader.read_line(&mut buf) {
                    Ok(0) => return Err(CallError::Broken("Glasscode closed the connection without an answer".into())),
                    Ok(_) => match serde_json::from_str::<Reply>(buf.trim()) {
                        Ok(Reply::Waiting { approval, summary }) => {
                            if !on_waiting(approval, &summary) {
                                return Err(CallError::StillWaiting { approval, summary });
                            }
                            waiting = Some((approval, summary));
                        }
                        Ok(done) => return Ok(done),
                        Err(e) => return Err(CallError::Broken(format!("Glasscode answered something unreadable: {e}"))),
                    },
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                        if let Some((approval, summary)) = waiting {
                            return Err(CallError::StillWaiting { approval, summary });
                        }
                        return Err(CallError::Broken("Glasscode did not answer in time".into()));
                    }
                    Err(e) => return Err(CallError::Broken(e.to_string())),
                }
            }
        }
        // A sandbox (Codex) that may not open sockets or pipes: the request file.
        Err(e) if crate::sys::control::is_denied(&e) || std::env::var_os("GLASSCODE_CLI_FILES").is_some() => {
            let worktree = std::env::var_os("GLASSCODE_WORKTREE").map(PathBuf::from).or_else(|| request.cwd.as_ref().map(PathBuf::from)).ok_or_else(|| CallError::NotRunning("no worktree to leave the request in".into()))?;
            call_by_file(&worktree, &line, on_waiting, wait_for)
        }
        Err(e) => Err(CallError::NotRunning(e.to_string())),
    }
}

fn call_by_file(worktree: &Path, line: &str, mut on_waiting: impl FnMut(u64, &str) -> bool, wait_for: Duration) -> Result<Reply, CallError> {
    let dir = drop_dir(worktree);
    std::fs::create_dir_all(&dir).map_err(|e| CallError::NotRunning(format!("cannot reach Glasscode from this sandbox ({e})")))?;
    let id = format!("{}-{}-{:08x}", std::process::id(), crate::model::now(), rand::random::<u32>());
    let tmp = dir.join(format!("{id}.tmp"));
    let req = dir.join(format!("{id}.req"));
    let out = dir.join(format!("{id}.out"));
    std::fs::write(&tmp, line).and_then(|()| std::fs::rename(&tmp, &req)).map_err(|e| CallError::NotRunning(format!("cannot reach Glasscode from this sandbox ({e})")))?;
    let cleanup = || {
        let _ = std::fs::remove_file(&req);
        let _ = std::fs::remove_file(&out);
    };
    let started = Instant::now();
    let mut seen = 0;
    let mut waiting: Option<(u64, String)> = None;
    loop {
        std::thread::sleep(Duration::from_millis(150));
        // Nobody picked it up: Glasscode is not running (or does not know this worktree). Take
        // it back first, so it cannot run after this says it did not.
        if waiting.is_none() && started.elapsed() > Duration::from_secs(8) && req.exists() && !out.exists() && std::fs::remove_file(&req).is_ok() {
            return Err(CallError::NotRunning("Glasscode did not pick up the request".into()));
        }
        if let Some((approval, summary)) = &waiting {
            if started.elapsed() > wait_for {
                return Err(CallError::StillWaiting { approval: *approval, summary: summary.clone() });
            }
        } else if started.elapsed() > Duration::from_secs(600) {
            // Taken, and never answered (Glasscode quit meanwhile).
            cleanup();
            return Err(CallError::Broken("Glasscode took the request but never answered".into()));
        }
        let Ok(text) = std::fs::read_to_string(&out) else { continue };
        let lines: Vec<&str> = text.lines().collect();
        for l in lines.iter().skip(seen) {
            seen += 1;
            match serde_json::from_str::<Reply>(l) {
                Ok(Reply::Waiting { approval, summary }) => {
                    if !on_waiting(approval, &summary) {
                        return Err(CallError::StillWaiting { approval, summary });
                    }
                    waiting = Some((approval, summary));
                }
                Ok(done) => {
                    cleanup();
                    return Ok(done);
                }
                Err(_) => {
                    seen -= 1;
                    break;
                }
            }
        }
    }
}

// ----- what agents may do ------------------------------------------------------------------

/// Kinds of action, each with its own rule in Settings › Permissions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Read,
    Board,
    Start,
    Rename,
    Send,
    Import,
    Close,
    Remove,
    Move,
    Tickets,
    Github,
    Show,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// Just do it.
    Allow,
    /// Ask the person first.
    Ask,
    Never,
}

impl Kind {
    pub const ALL: [Kind; 13] = [Kind::Read, Kind::Board, Kind::Start, Kind::Rename, Kind::Send, Kind::Import, Kind::Close, Kind::Remove, Kind::Move, Kind::Tickets, Kind::Github, Kind::Show, Kind::Settings];

    /// What it covers, for Settings.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Read => "Read sessions, projects, accounts, tickets and pull requests",
            Kind::Board => "Add and update tasks on Today, and send you notifications",
            Kind::Start => "Start new sessions, reviews and worktrees",
            Kind::Rename => "Rename sessions and worktrees",
            Kind::Send => "Send a message to another session",
            Kind::Import => "Import a project",
            Kind::Close => "Close or restart a session, remove a project from Glasscode",
            Kind::Remove => "Remove or clean up worktrees",
            Kind::Move => "Hand off a session, or move it to the server and back",
            Kind::Tickets => "Change Linear or GitHub tickets: move, comment, approve, answer",
            Kind::Github => "Change pull requests: re-run checks, comment, labels, open a draft",
            Kind::Show => "Change what the window shows, open an editor",
            Kind::Settings => "Change settings, such as a project's GitHub account",
        }
    }

    /// The suggested rule.
    pub fn default_rule(self) -> Rule {
        match self {
            Kind::Read | Kind::Board | Kind::Start | Kind::Rename => Rule::Allow,
            Kind::Settings => Rule::Never,
            _ => Rule::Ask,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::Read => "read",
            Kind::Board => "board",
            Kind::Start => "start",
            Kind::Rename => "rename",
            Kind::Send => "send",
            Kind::Import => "import",
            Kind::Close => "close",
            Kind::Remove => "remove",
            Kind::Move => "move",
            Kind::Tickets => "tickets",
            Kind::Github => "github",
            Kind::Show => "show",
            Kind::Settings => "settings",
        }
    }
}

/// The settings for agents using the `glasscode` command (in `state.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentSettings {
    /// Rules changed from the suggested ones.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rules: BTreeMap<Kind, Rule>,
    /// Programs Glasscode did not start follow the same rules (off: they can only read, and
    /// everything else asks).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub outside_allowed: bool,
    /// Off: Today does not list what agents did.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub log_off: bool,
    /// Off: sessions are not told about the `glasscode` command (no skill, no instructions).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skill_off: bool,
    /// Off: Glasscode does not open Today by itself the first time each morning.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub morning_off: bool,
}

impl AgentSettings {
    pub fn rule(&self, kind: Kind) -> Rule {
        self.rules.get(&kind).copied().unwrap_or(kind.default_rule())
    }

    pub fn set(&mut self, kind: Kind, rule: Rule) {
        if rule == kind.default_rule() {
            self.rules.remove(&kind);
        } else {
            self.rules.insert(kind, rule);
        }
    }
}

/// The line of what an op did, as the journal's op event keeps it (`data.log`, ops-21): what a
/// row of `journal query` (and `log --json`) shows, and Today › Activity lists. (Until M56 row 7 it
/// was also a line of `~/.glasscode/agent-log.jsonl`, which is no longer written or read; an old
/// home's file stays where it is.)
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub at: u64,
    /// "Claude", "Codex", "You", "A program outside Glasscode".
    pub who: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    /// What it did, in words: "added “Send Sönke the wording” to Today".
    pub what: String,
    /// The command.
    pub command: String,
    /// done, failed, denied, waiting.
    pub outcome: String,
}

/// The log lines of the journal's op events in `events` (oldest first, as a query gives them),
/// newest first: what Today › Activity starts from (M56 row 7).
pub fn log_lines(events: &[crate::journal::Event], limit: usize) -> Vec<LogEntry> {
    events.iter().rev().filter(|e| e.kind == crate::journal_kinds::OP_DONE || e.kind == crate::journal_kinds::OP_FAILED).filter_map(|e| e.data.get("log").cloned().and_then(|v| serde_json::from_value(v).ok())).take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request folder and its answer files are the worktree's own: a link at the answer, at the
    /// folder or at `.glasscode` is never followed, so the app writes nothing outside the worktree
    /// for a session (which a sandboxed one could not write itself).
    #[cfg(unix)]
    #[test]
    fn answers_go_only_into_a_request_folder_of_its_own() {
        let scratch = tempfile::tempdir().unwrap();
        let worktree = scratch.path().join("wt");
        let dir = drop_dir(&worktree);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(own_drop_dir(&worktree), Some(dir.clone()));
        append_answer(&dir.join("a.out"), "one").unwrap();
        append_answer(&dir.join("a.out"), "two").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.out")).unwrap(), "one\ntwo\n");
        // A link at the answer file.
        let rc = scratch.path().join("zshrc");
        std::fs::write(&rc, "export A=1\n").unwrap();
        std::os::unix::fs::symlink(&rc, dir.join("b.out")).unwrap();
        assert!(append_answer(&dir.join("b.out"), "$(touch pwned)").is_err());
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "export A=1\n");
        // A hard link to it, which no flag of the open refuses.
        std::fs::hard_link(&rc, dir.join("h.out")).unwrap();
        assert!(append_answer(&dir.join("h.out"), "$(touch pwned)").is_err());
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "export A=1\n");
        // A pipe there does not hold the app's thread.
        assert!(std::process::Command::new("mkfifo").arg(dir.join("p.out")).status().unwrap().success());
        assert!(append_answer(&dir.join("p.out"), "x").is_err());
        // A link at the folder, or at `.glasscode`.
        let elsewhere = scratch.path().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("cli")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("cli"), &dir).unwrap();
        assert_eq!(own_drop_dir(&worktree), None);
        assert!(append_answer(&dir.join("c.out"), "x").is_err());
        std::fs::remove_dir_all(worktree.join(".glasscode")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, worktree.join(".glasscode")).unwrap();
        assert_eq!(own_drop_dir(&worktree), None);
        assert!(append_answer(&dir.join("d.out"), "x").is_err());
        assert!(std::fs::read_dir(elsewhere.join("cli")).unwrap().next().is_none(), "nothing was written elsewhere");
    }

    /// Today › Activity starts from the journal's op events (M56 row 7): their log lines, newest
    /// first; other events, and an op event with no line (a read), are not lines.
    #[test]
    fn log_lines_come_from_the_journals_op_events() {
        use crate::journal::{Event, Source};
        use crate::journal_kinds as kinds;
        let line = |what: &str| LogEntry { at: 1, who: "Claude".into(), what: what.into(), command: "glasscode board add x".into(), outcome: "done".into(), ..LogEntry::default() };
        let op = |kind, what: &str| Event::new(kind, Source::Control, what).data(serde_json::json!({ "action": {}, "log": line(what) }));
        let events = vec![
            op(kinds::OP_DONE, "added “a” to Today"),
            Event::new(kinds::OP_DONE, Source::Control, "app.status done").data(serde_json::json!({ "action": {}, "log": null })),
            Event::new(kinds::SESSION_STARTED, Source::App, "started"),
            op(kinds::OP_FAILED, "Could not add “b” to Today: no"),
            op(kinds::OP_DONE, "added “c” to Today"),
        ];
        let lines: Vec<String> = log_lines(&events, 200).into_iter().map(|l| l.what).collect();
        assert_eq!(lines, ["added “c” to Today", "Could not add “b” to Today: no", "added “a” to Today"]);
        assert_eq!(log_lines(&events, 1).len(), 1);
    }

    #[test]
    fn parses_words_and_options() {
        let args: Vec<String> = ["board", "add", "Ask Jordan", "--due", "fri", "--urgent", "--link", "ENG-1", "--link=#9", "--", "--not-an-option"].iter().map(|s| s.to_string()).collect();
        let a = Args::parse(&args);
        assert_eq!(a.words, vec!["board", "add", "Ask Jordan", "--not-an-option"]);
        assert_eq!(a.flag("due"), Some("fri"));
        assert!(a.has("urgent") && a.flag("urgent").is_none());
        assert_eq!(a.all("link"), vec!["ENG-1", "#9"]);
        assert_eq!(a.unknown(&["due", "urgent"]), vec!["--link"]);
        assert_eq!(sign(b"secret", "s1"), sign(b"secret", "s1"));
        assert_ne!(sign(b"secret", "s1"), sign(b"other", "s1"));
    }

    #[test]
    fn settings_keep_only_changes() {
        let mut s = AgentSettings::default();
        assert_eq!(s.rule(Kind::Remove), Rule::Ask);
        s.set(Kind::Remove, Rule::Allow);
        s.set(Kind::Board, Rule::Allow);
        assert_eq!((s.rule(Kind::Remove), s.rules.len()), (Rule::Allow, 1));
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"rules":{"remove":"allow"}}"#);
    }

    #[test]
    fn a_request_file_gets_the_answer_of_the_app() {
        let worktree = tempfile::tempdir().unwrap();
        let request = Request { v: VERSION, args: vec!["board".into()], cwd: Some(r"C:\Users\dev\app".into()), ..Default::default() };
        let line = serde_json::to_string(&request).unwrap();
        // The app's side, as it reads the folder of a worktree: it takes the request, and answers
        // one line at a time, each in a file it opens again.
        let dir = drop_dir(worktree.path());
        let app = std::thread::spawn(move || {
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(20) {
                let found = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "req"));
                if let Some(path) = found {
                    let text = std::fs::read_to_string(&path).unwrap();
                    std::fs::remove_file(&path).unwrap();
                    let out = path.with_extension("out");
                    for (i, reply) in [Reply::Waiting { approval: 7, summary: "Add a task".into() }, Reply::ok("Added", Value::Null)].iter().enumerate() {
                        if i > 0 {
                            std::thread::sleep(Duration::from_millis(300));
                        }
                        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&out).unwrap();
                        writeln!(file, "{}", serde_json::to_string(reply).unwrap()).unwrap();
                    }
                    return Some(text);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            None
        });
        let mut asked = Vec::new();
        let reply = call_by_file(
            worktree.path(),
            &line,
            |approval, summary| {
                asked.push((approval, summary.to_string()));
                true
            },
            Duration::from_secs(30),
        );
        assert_eq!(app.join().unwrap().as_deref(), Some(line.as_str()));
        assert_eq!(asked, [(7, "Add a task".to_string())]);
        assert!(matches!(&reply, Ok(Reply::Done { code: code::OK, text, .. }) if text == "Added"), "{reply:?}");
        // The command took its files away.
        let left: Vec<PathBuf> = std::fs::read_dir(drop_dir(worktree.path())).unwrap().flatten().map(|e| e.path()).collect();
        assert!(left.is_empty(), "{left:?}");
    }
}
