//! How the `glasscode` command talks to the running app.
//!
//! The command sends one JSON line with its words (`["session", "new", "--project", …]`) and
//! who is asking, and reads JSON lines back: perhaps `waiting` (the person has to allow it in
//! Glasscode first), then `done` with an exit code, text for people and data for `--json`.
//!
//! The way in is a Unix socket only this user can open (`~/.glasscode/control.sock`). Codex runs
//! commands in a sandbox that may not open sockets but may write in its worktree, so the
//! command falls back to a request file in `<worktree>/.glasscode/cli/`, which the app reads.
//!
//! Who is asking: sessions Glasscode starts get their id and a signature of it
//! (`GLASSCODE_SESSION_ID`, `GLASSCODE_SESSION_SIG`); the app checks the signature with a
//! secret only it knows, so a call can say which session (and which agent) made it. Anything
//! else is a program outside Glasscode, or the person at a terminal.

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
    /// Typed by a person at a terminal (stdin and stdout are a terminal).
    #[serde(default)]
    pub interactive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
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

/// The socket the app listens on: `GLASSCODE_CONTROL_SOCKET`, else `control.sock` in
/// Glasscode's folder (in a folder of this user's own under /tmp when that path would be too
/// long for a socket).
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("GLASSCODE_CONTROL_SOCKET").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let path = crate::paths::root().join("control.sock");
    if path.as_os_str().len() < 100 {
        return path;
    }
    let mut hash = Sha256::new();
    hash.update(path.as_os_str().as_encoded_bytes());
    let digest = hash.finalize();
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/glasscode-{uid}/control-{:02x}{:02x}{:02x}{:02x}.sock", digest[0], digest[1], digest[2], digest[3]))
}

/// Make sure the socket's folder is this user's alone before listening there: the /tmp folder
/// is made 0700, and one that belongs to someone else (or is open to others) is refused.
pub fn prepare_socket_dir(socket: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let Some(dir) = socket.parent() else { return Ok(()) };
    // Only our own auto-made folder under /tmp needs guarding; an explicitly set socket path
    // (a test or dev override) is trusted, and a socket in ~/.glasscode needs nothing.
    let ours = dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("glasscode-")) && dir.parent().is_some_and(|p| p == Path::new("/tmp") || p == Path::new("/private/tmp"));
    if !ours {
        return Ok(());
    }
    if !dir.exists() {
        std::fs::DirBuilder::new().mode(0o700).create(dir)?;
    }
    let meta = std::fs::symlink_metadata(dir)?;
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != uid {
        anyhow::bail!("{} is not this user's own folder", dir.display());
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The process on the other end of a Unix socket.
pub fn peer_pid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "macos")]
    {
        let mut pid: libc::pid_t = 0;
        let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
        // SAFETY: getsockopt writes at most `len` bytes into `pid`.
        let ok = unsafe { libc::getsockopt(fd, libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&mut pid as *mut libc::pid_t).cast(), &mut len) } == 0;
        return (ok && pid > 0).then_some(pid as u32);
    }
    #[cfg(target_os = "linux")]
    {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: getsockopt writes at most `len` bytes into `cred`.
        let ok = unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len) } == 0;
        return (ok && cred.pid > 0).then_some(cred.pid as u32);
    }
    #[allow(unreachable_code)]
    None
}

/// Every process's parent (`ps`), to tell whether a caller runs inside one of our sessions.
pub fn parents() -> std::collections::HashMap<u32, u32> {
    let Ok(out) = std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid="]).output() else { return Default::default() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        })
        .collect()
}

/// Whether `pid` is `ancestor` or runs under it.
pub fn descends(parents: &std::collections::HashMap<u32, u32>, mut pid: u32, ancestor: u32) -> bool {
    for _ in 0..64 {
        if pid == ancestor {
            return true;
        }
        match parents.get(&pid) {
            Some(&parent) if parent != pid && parent > 1 => pid = parent,
            _ => return false,
        }
    }
    false
}

/// The request folder of a worktree (for commands that cannot open the socket).
pub fn drop_dir(worktree: &Path) -> PathBuf {
    worktree.join(".glasscode").join("cli")
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
    "all", "open", "force", "delete-branch", "keep-branch", "dry-run", "merged", "no-session", "new-tab", "on-server", "to-server", "to-mac", "urgent", "not-urgent", "done", "needs-you",
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
    match std::os::unix::net::UnixStream::connect(&socket) {
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
        // A sandbox (Codex) that may not open sockets: the request file.
        Err(e) if e.raw_os_error() == Some(libc::EPERM) || e.raw_os_error() == Some(libc::EACCES) || std::env::var_os("GLASSCODE_CLI_FILES").is_some() => {
            call_by_file(request, &line, on_waiting, wait_for)
        }
        Err(e) => Err(CallError::NotRunning(e.to_string())),
    }
}

fn call_by_file(request: &Request, line: &str, mut on_waiting: impl FnMut(u64, &str) -> bool, wait_for: Duration) -> Result<Reply, CallError> {
    let worktree = std::env::var_os("GLASSCODE_WORKTREE").map(PathBuf::from).or_else(|| request.cwd.as_ref().map(PathBuf::from)).ok_or_else(|| CallError::NotRunning("no worktree to leave the request in".into()))?;
    let dir = drop_dir(&worktree);
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

/// Kinds of action, each with its own rule in Settings › Automation.
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

/// One line of what agents did (`~/.glasscode/agent-log.jsonl`).
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

pub fn log_file() -> PathBuf {
    crate::paths::root().join("agent-log.jsonl")
}

pub fn append_log(entry: &LogEntry) {
    let Ok(line) = serde_json::to_string(entry) else { return };
    let path = log_file();
    // Keep it from growing without end: past 2 MB, the newer half stays (a torn line in it
    // must not stop that).
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 2_000_000) {
        if let Ok(bytes) = std::fs::read(&path) {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().collect();
            let keep = lines[lines.len() / 2..].join("\n");
            let tmp = path.with_extension("jsonl.tmp");
            if std::fs::write(&tmp, format!("{keep}\n")).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

/// The newest entries first.
pub fn read_log(limit: usize) -> Vec<LogEntry> {
    let Ok(bytes) = std::fs::read(log_file()) else { return Vec::new() };
    String::from_utf8_lossy(&bytes).lines().rev().filter_map(|l| serde_json::from_str(l).ok()).take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
