//! Projects and agent logins that are already on this computer, for the first run: the folders
//! the person opened in Cursor or VS Code or worked in with Claude Code or Codex, and the logins
//! of the two agents.
//!
//! Where each comes from (the editors' format as Cursor and VS Code wrote it on Windows 11 in
//! 2026-11):
//! - Cursor and VS Code: `<app data>/<Cursor|Code>/User/workspaceStorage/<hash>/workspace.json`,
//!   `{"folder":"file:///c%3A/Users/dev/my%20app"}`. The window of a `.code-workspace` file has
//!   `"workspace"` instead, and a remote window a `vscode-remote://` folder: both are left out.
//! - Claude Code and Codex: the conversation files that `history.rs` reads, whose lines carry the
//!   work folder (`cwd`), with its rules for what a person's own conversation is.
//! - claude-swap: `sequence.json` in its backup folder, which lists the accounts it saved by
//!   e-mail address and alias. Their tokens are in `credentials/` (or the Keychain), which is
//!   never opened, nor even listed.
//!
//! This only looks, and only on this computer. Each file it reads has a size limit, folders are
//! read newest first up to a number, a link inside them is not followed, and a folder on a share
//! is not asked for at all. A login counts by whether its file exists; that file is never opened
//! (Glasscode does not read login tokens, see `agents.rs`).

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

use crate::model::{Account, AgentKind};

/// A program that knows a project folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Cursor,
    VsCode,
    ClaudeCode,
    Codex,
}

/// A folder the person worked in.
#[derive(Clone, Debug, PartialEq)]
pub struct FoundProject {
    pub root: PathBuf,
    /// The name of the folder.
    pub name: String,
    /// Each program that knows it, in the order of `Source`.
    pub sources: Vec<Source>,
    /// The latest use that one of them recorded.
    pub last_used: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginState {
    SignedIn,
    NeedsSignIn,
}

/// An agent login.
#[derive(Clone, Debug, PartialEq)]
pub struct FoundLogin {
    pub agent: AgentKind,
    pub label: String,
    /// Its config folder (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`); `None` for an account that another
    /// tool saved, which signs in again in a Glasscode folder of its own.
    pub dir: Option<PathBuf>,
    pub state: LoginState,
    /// Where it was found: `FROM_SYSTEM`, `FROM_GLASSCODE` or `FROM_CLAUDE_SWAP`.
    pub from: String,
}

/// The CLI's own login, `~/.claude` or `~/.codex`.
pub const FROM_SYSTEM: &str = "system";
/// An account of Glasscode, with its folder in `~/.glasscode/accounts`.
pub const FROM_GLASSCODE: &str = "glasscode";
/// An account that claude-swap saved, known by its name only.
pub const FROM_CLAUDE_SWAP: &str = "claude-swap";

/// How much `projects` reads at most, so that a first run stays quick after years of history.
#[derive(Clone, Copy, Debug)]
struct Limits {
    /// Storage folders of each editor, newest first.
    storage_folders: usize,
    /// Claude Code's project folders, newest first…
    claude_folders: usize,
    /// …and the conversation files tried in each, newest first.
    claude_files: usize,
    /// Codex conversation files, newest first.
    codex_files: usize,
    /// How long all folders together may take to say whether they exist.
    wait: Duration,
}

const LIMITS: Limits = Limits {
    storage_folders: 400,
    claude_folders: 300,
    claude_files: 3,
    codex_files: 500,
    wait: Duration::from_secs(2),
};

/// The most entries read from one folder.
const MOST_ENTRIES: usize = 20_000;
/// The largest `workspace.json` read (it holds one address, about 150 bytes).
const WORKSPACE_FILE: u64 = 64 * 1024;
/// How much of the start of a Claude conversation is read for its work folder.
const CLAUDE_HEAD: u64 = 256 * 1024;
/// The longest first line of a Codex conversation read (it holds Codex's instructions, about
/// 20 KB).
const CODEX_HEAD: u64 = 1024 * 1024;
/// The largest `state.json` of Glasscode and `sequence.json` of claude-swap read.
const INDEX_FILE: u64 = 16 * 1024 * 1024;
/// The threads that ask whether folders exist.
const PROBE_THREADS: usize = 16;

/// Where Cursor and VS Code keep their data, the second argument of `projects`: `%APPDATA%`
/// (`C:\Users\<name>\AppData\Roaming`) on Windows, `~/Library/Application Support` on macOS, and
/// `$XDG_CONFIG_HOME` or `~/.config` on Linux. Both editors use the same folder names on each.
pub fn app_data() -> Option<PathBuf> {
    dirs::config_dir()
}

/// The folders the person worked in, last used first: each folder that exists now, once, with
/// each program that knows it. `home` is the home folder, `app_data` the folder of `app_data()`.
/// It reads files only and can take a few seconds (a drive that does not answer), so call it
/// away from the UI thread.
pub fn projects(home: &Path, app_data: &Path) -> Vec<FoundProject> {
    let seen = gather(home, app_data, &LIMITS, cfg!(windows));
    keep_existing(
        join(seen, home, &crate::paths::root()),
        LIMITS.wait,
        Path::is_dir,
    )
}

/// A work folder as one program recorded it.
#[derive(Debug)]
struct Seen {
    folder: PathBuf,
    source: Source,
    used: Option<SystemTime>,
}

/// What each program recorded, as it recorded it. `windows` says how to read a `file:` address.
fn gather(home: &Path, app_data: &Path, limits: &Limits, windows: bool) -> Vec<Seen> {
    let mut seen = Vec::new();
    for (source, app) in [(Source::Cursor, "Cursor"), (Source::VsCode, "Code")] {
        let storage = app_data.join(app).join("User").join("workspaceStorage");
        editor_folders(&storage, source, limits, windows, &mut seen);
    }
    claude_folders(&home.join(".claude").join("projects"), limits, &mut seen);
    codex_folders(&home.join(".codex").join("sessions"), limits, &mut seen);
    seen
}

/// An entry of a folder, as the folder lists it.
struct Entry {
    path: PathBuf,
    name: String,
    modified: Option<SystemTime>,
}

/// The folders (or the files) in `dir`. A link is neither: it could lead out of the folder.
fn entries(dir: &Path, folders: bool) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.take(MOST_ENTRIES)
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type()
                .is_ok_and(|t| if folders { t.is_dir() } else { t.is_file() })
        })
        .map(|e| Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            modified: e.metadata().and_then(|m| m.modified()).ok(),
            path: e.path(),
        })
        .collect()
}

/// The entries of `entries`, newest first.
fn newest_first(dir: &Path, folders: bool) -> Vec<Entry> {
    let mut list = entries(dir, folders);
    list.sort_by_key(|entry| std::cmp::Reverse(entry.modified));
    list
}

/// The entries of `entries`, by name, the last first (Codex names its folders and files by date).
fn last_name_first(dir: &Path, folders: bool) -> Vec<Entry> {
    let mut list = entries(dir, folders);
    list.sort_by(|a, b| b.name.cmp(&a.name));
    list
}

/// A small text file: a file (not a link) of at most `most` bytes.
fn small_file(path: &Path, most: u64) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > most {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(most)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// The first line of a file, when it ends within `most` bytes.
fn first_line(path: &Path, most: u64) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = Vec::new();
    BufReader::new(file.take(most))
        .read_until(b'\n', &mut line)
        .ok()?;
    line.ends_with(b"\n")
        .then(|| String::from_utf8_lossy(&line).into_owned())
}

/// Cursor and VS Code: the folder of each window, from the storage folder the editor keeps for it.
fn editor_folders(
    storage: &Path,
    source: Source,
    limits: &Limits,
    windows: bool,
    out: &mut Vec<Seen>,
) {
    // The editor writes its state of a window to `state.vscdb` while the window is open (it is
    // only looked at, never opened). The time of the storage folder is no use: Cursor changed it
    // for nearly all of them on one day.
    let mut folders: Vec<(Entry, Option<SystemTime>)> = entries(storage, true)
        .into_iter()
        .map(|entry| {
            let state = std::fs::symlink_metadata(entry.path.join("state.vscdb"))
                .and_then(|m| m.modified())
                .ok();
            let used = state.or(entry.modified);
            (entry, used)
        })
        .collect();
    folders.sort_by_key(|(_, used)| std::cmp::Reverse(*used));
    for (entry, used) in folders.into_iter().take(limits.storage_folders) {
        let Some(text) = small_file(&entry.path.join("workspace.json"), WORKSPACE_FILE) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        // `"workspace"` (a `.code-workspace` file) is left out: its folders are in that file.
        let Some(folder) = value["folder"]
            .as_str()
            .and_then(|uri| file_uri_path(uri, windows))
        else {
            continue;
        };
        out.push(Seen {
            folder: PathBuf::from(folder),
            source,
            used,
        });
    }
}

/// The local folder of a `file:` address as Cursor and VS Code write it, for Windows (`windows`)
/// or for macOS and Linux: `file:///c%3A/Users/dev/my%20app` is `C:\Users\dev\my app`,
/// `file://server/share/app` is `\\server\share\app`, and `file:///Users/dev/app` is
/// `/Users/dev/app`. `None` for another scheme (`vscode-remote://ssh-remote%2B…`), a share on
/// macOS or Linux, and a path without a drive on Windows.
fn file_uri_path(uri: &str, windows: bool) -> Option<String> {
    let rest = uri
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("file://"))
        .and(uri.get(7..))?;
    // A query or a fragment is not part of the path (the editors write neither).
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let (host, path) = (
        urlencoding::decode(host).ok()?,
        urlencoding::decode(path).ok()?,
    );
    if path.contains('\0') || host.contains(['/', '\\', '\0']) {
        return None;
    }
    let host: &str = if host.eq_ignore_ascii_case("localhost") {
        ""
    } else {
        &host
    };
    if !windows {
        return (host.is_empty() && path.starts_with('/')).then(|| without_end(&path, '/'));
    }
    let path = path.replace('/', "\\");
    if !host.is_empty() {
        // A share has a name after its server: `\\server\share`.
        let share = path.trim_start_matches('\\');
        return (!share.is_empty()).then(|| without_end(&format!(r"\\{host}\{share}"), '\\'));
    }
    // `\c:\Users\dev` (the editors write the drive in lower case).
    let drive = path.strip_prefix('\\')?;
    let bytes = drive.as_bytes();
    if bytes.len() < 2
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes.get(2).is_some_and(|&b| b != b'\\')
    {
        return None;
    }
    let folder = without_end(&drive[2..], '\\');
    Some(format!(
        "{}:{}",
        drive[..1].to_ascii_uppercase(),
        if folder == "\\" { "\\" } else { &folder }
    ))
}

/// `text` without separators at its end; a root keeps its own.
fn without_end(text: &str, separator: char) -> String {
    let trimmed = text.trim_end_matches(separator);
    if trimmed.is_empty() {
        separator.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Claude Code: the work folder of the newest conversations in each of its project folders.
fn claude_folders(projects: &Path, limits: &Limits, out: &mut Vec<Seen>) {
    for folder in newest_first(projects, true)
        .into_iter()
        .take(limits.claude_folders)
    {
        let files = newest_first(&folder.path, false)
            .into_iter()
            .filter(|f| f.name.ends_with(".jsonl"));
        for file in files.take(limits.claude_files) {
            if let Some(cwd) = claude_cwd(&file.path) {
                out.push(Seen {
                    folder: cwd,
                    source: Source::ClaudeCode,
                    used: file.modified,
                });
                break;
            }
        }
    }
}

/// The work folder of a Claude Code conversation, from the first line that has one; `None` for
/// `claude -p` and SDK runs, which are scripts and not a person's work (as in `history.rs`).
fn claude_cwd(file: &Path) -> Option<PathBuf> {
    let file = std::fs::File::open(file).ok()?;
    for line in BufReader::new(file.take(CLAUDE_HEAD)).split(b'\n') {
        let line = line.ok()?;
        // Most lines are messages: parse only those that can have the folder.
        if !line.windows(5).any(|w| w == b"\"cwd\"") {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let Some(cwd) = value["cwd"].as_str() else {
            continue;
        };
        let interactive = value["entrypoint"]
            .as_str()
            .is_none_or(|e| !e.starts_with("sdk"));
        return (interactive && !cwd.is_empty()).then(|| PathBuf::from(cwd));
    }
    None
}

/// Codex: the work folder of its newest conversations, `sessions/YYYY/MM/DD/rollout-*.jsonl`.
fn codex_folders(sessions: &Path, limits: &Limits, out: &mut Vec<Seen>) {
    let mut left = limits.codex_files;
    for year in last_name_first(sessions, true) {
        for month in last_name_first(&year.path, true) {
            for day in last_name_first(&month.path, true) {
                for file in last_name_first(&day.path, false) {
                    if !file.name.starts_with("rollout-") || !file.name.ends_with(".jsonl") {
                        continue;
                    }
                    if left == 0 {
                        return;
                    }
                    left -= 1;
                    if let Some(cwd) = codex_cwd(&file.path) {
                        out.push(Seen {
                            folder: cwd,
                            source: Source::Codex,
                            used: file.modified,
                        });
                    }
                }
            }
        }
    }
}

/// The work folder of a Codex conversation, from its first line (`session_meta`), for the
/// conversations a person had, by the rules of `history.rs`: not `codex exec` runs, sub-agents,
/// or Codex runs that another tool started.
fn codex_cwd(file: &Path) -> Option<PathBuf> {
    let value: Value = serde_json::from_str(&first_line(file, CODEX_HEAD)?).ok()?;
    let meta = &value["payload"];
    if value["type"] != "session_meta" {
        return None;
    }
    let source = meta["source"].as_str().unwrap_or("");
    let thread_source = meta["thread_source"].as_str().unwrap_or("user");
    let originator = meta["originator"].as_str().unwrap_or("");
    if !matches!(source, "cli" | "vscode") || thread_source != "user" || originator == "Claude Code"
    {
        return None;
    }
    meta["cwd"]
        .as_str()
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
}

/// What tells one folder from another: on Windows without case, as its file system does.
fn folder_key(folder: &Path) -> String {
    let clean = crate::sys::clean_path(folder)
        .to_string_lossy()
        .into_owned();
    if cfg!(windows) {
        clean.to_lowercase()
    } else {
        clean
    }
}

/// Whether the folder of `key` is the folder of `parent` or inside it.
fn inside(key: &str, parent: &str) -> bool {
    let separator = std::path::MAIN_SEPARATOR;
    key.strip_prefix(parent).is_some_and(|rest| {
        rest.is_empty() || rest.starts_with(separator) || parent.ends_with(separator)
    })
}

/// One project for each folder, with each program that knows it and the latest use; not yet
/// looked for on the disk. Left out: a relative path, the home folder and the folders above it
/// (a CLI started there), and the folders in Glasscode's own folder `own` (its worktrees).
fn join(seen: Vec<Seen>, home: &Path, own: &Path) -> Vec<FoundProject> {
    let (home, own) = (folder_key(home), folder_key(own));
    let mut found: Vec<FoundProject> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for item in seen {
        if !item.folder.is_absolute() {
            continue;
        }
        let root = crate::sys::clean_path(&item.folder);
        let key = folder_key(&root);
        if inside(&home, &key) || inside(&key, &own) {
            continue;
        }
        match index.get(&key) {
            Some(&i) => {
                let project = &mut found[i];
                if !project.sources.contains(&item.source) {
                    project.sources.push(item.source);
                    project.sources.sort();
                }
                project.last_used = project.last_used.max(item.used);
            }
            None => {
                index.insert(key, found.len());
                let name = root
                    .file_name()
                    .map_or_else(|| root.to_string_lossy(), |name| name.to_string_lossy())
                    .into_owned();
                found.push(FoundProject {
                    root,
                    name,
                    sources: vec![item.source],
                    last_used: item.used,
                });
            }
        }
    }
    found
}

/// The drive a folder is on, asked before its folders: `C:\` on Windows, the first two folders on
/// macOS and Linux (`/Volumes/Backup`, `/home/dev`). `None` for a share (`\\server\share`), which
/// is not asked.
fn volume(folder: &Path) -> Option<PathBuf> {
    let mut parts = folder.components();
    match parts.next()? {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                Some(PathBuf::from(format!("{}:\\", char::from(letter))))
            }
            _ => None,
        },
        Component::RootDir => Some(
            std::iter::once(Component::RootDir)
                .chain(parts.take(2))
                .collect(),
        ),
        _ => None,
    }
}

/// The projects whose folder exists, last used first. The folders are asked on other threads, the
/// drive of each first, and all answers must come within `wait`: a drive that is not connected
/// can keep one call for a long time, and its folders then count as gone. A folder on a share is
/// not asked: that goes on the network (and a WSL share would start WSL).
fn keep_existing(
    projects: Vec<FoundProject>,
    wait: Duration,
    is_dir: fn(&Path) -> bool,
) -> Vec<FoundProject> {
    // Each drive, with the folders on it and their places in `projects`.
    let mut volumes: BTreeMap<PathBuf, Vec<(usize, PathBuf)>> = BTreeMap::new();
    for (i, project) in projects.iter().enumerate() {
        if let Some(volume) = volume(&project.root) {
            volumes
                .entry(volume)
                .or_default()
                .push((i, project.root.clone()));
        }
    }
    let mut groups: Vec<Vec<_>> = (0..PROBE_THREADS).map(|_| Vec::new()).collect();
    for (n, volume) in volumes.into_iter().enumerate() {
        groups[n % PROBE_THREADS].push(volume);
    }
    let (tx, rx) = mpsc::channel();
    for group in groups.into_iter().filter(|g| !g.is_empty()) {
        let tx = tx.clone();
        // Never joined: a thread that waits on a lost drive ends by itself, later.
        let _ = std::thread::Builder::new()
            .name("discover-folders".into())
            .spawn(move || {
                for (volume, folders) in group {
                    if !is_dir(&volume) {
                        continue;
                    }
                    for (i, folder) in folders {
                        if is_dir(&folder) && tx.send(i).is_err() {
                            return;
                        }
                    }
                }
            });
    }
    drop(tx);
    let deadline = Instant::now() + wait;
    let mut present = vec![false; projects.len()];
    while let Ok(i) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        present[i] = true;
    }
    while let Ok(i) = rx.try_recv() {
        present[i] = true;
    }
    let mut kept: Vec<FoundProject> = projects
        .into_iter()
        .zip(present)
        .filter_map(|(project, there)| there.then_some(project))
        .collect();
    kept.sort_by(|a, b| {
        b.last_used
            .cmp(&a.last_used)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    kept
}

/// The agent logins on this computer: the CLIs' own (`~/.claude`, `~/.codex`), Glasscode's
/// accounts, and the accounts that claude-swap saved, by name only (decision P3), which have no
/// folder and need a sign-in. No login file is read.
pub fn logins(home: &Path) -> Vec<FoundLogin> {
    find_logins(
        home,
        &crate::paths::root(),
        &swap_folders(home),
        cfg!(target_os = "macos"),
    )
}

/// Whether the account has a login: whether its login file exists (Claude Code's
/// `.credentials.json`, Codex's `auth.json`). The file is never opened. Claude Code on macOS keeps
/// its login in the Keychain, so there a Claude account counts once its folder exists.
pub fn signed_in(account: &Account) -> bool {
    login_present(
        account.agent,
        &account.effective_dir(),
        cfg!(target_os = "macos"),
    )
}

/// `signed_in` for the folder `dir`, on macOS (`mac`) or another system.
fn login_present(agent: AgentKind, dir: &Path, mac: bool) -> bool {
    match agent {
        // No file to look for, and the Keychain is not asked: as glasscoded does, the login is
        // taken to be there.
        AgentKind::Claude if mac => dir.exists(),
        AgentKind::Claude => dir.join(".credentials.json").exists(),
        AgentKind::Codex => dir.join("auth.json").exists(),
        AgentKind::Shell => true,
    }
}

fn login_state(present: bool) -> LoginState {
    if present {
        LoginState::SignedIn
    } else {
        LoginState::NeedsSignIn
    }
}

/// `logins` with Glasscode's folder `glasscode`, claude-swap's possible folders `swap`, and the
/// system (`mac`).
fn find_logins(home: &Path, glasscode: &Path, swap: &[PathBuf], mac: bool) -> Vec<FoundLogin> {
    let accounts = glasscode_accounts(&glasscode.join("state.json"));
    let mut found = Vec::new();
    for (agent, folder) in [(AgentKind::Claude, ".claude"), (AgentKind::Codex, ".codex")] {
        let dir = home.join(folder);
        let known = accounts
            .iter()
            .find(|a| a.system_default && a.agent == agent);
        // A CLI that never ran here has no folder; Glasscode may know its login before the first
        // sign-in (on a server).
        if known.is_none() && !dir.is_dir() {
            continue;
        }
        let label = known.map_or_else(|| "My login".to_string(), |a| a.label.clone());
        let state = login_state(login_present(agent, &dir, mac));
        found.push(FoundLogin {
            agent,
            label,
            dir: Some(dir),
            state,
            from: FROM_SYSTEM.into(),
        });
    }
    for account in accounts
        .iter()
        .filter(|a| !a.system_default && a.agent.uses_accounts())
    {
        // The id names one folder in `accounts`, and nothing else (`..`) from the file.
        let mut parts = Path::new(&account.id).components();
        if !matches!(
            (parts.next(), parts.next()),
            (Some(Component::Normal(_)), None)
        ) {
            continue;
        }
        let dir = glasscode.join("accounts").join(&account.id);
        let state = login_state(login_present(account.agent, &dir, mac));
        found.push(FoundLogin {
            agent: account.agent,
            label: account.label.clone(),
            dir: Some(dir),
            state,
            from: FROM_GLASSCODE.into(),
        });
    }
    let system_claude = found.iter().any(|l| {
        l.from == FROM_SYSTEM && l.agent == AgentKind::Claude && l.state == LoginState::SignedIn
    });
    for folder in swap {
        for label in swap_accounts(&folder.join("sequence.json"), system_claude) {
            if found
                .iter()
                .any(|l| l.agent == AgentKind::Claude && l.label.eq_ignore_ascii_case(&label))
            {
                continue;
            }
            found.push(FoundLogin {
                agent: AgentKind::Claude,
                label,
                dir: None,
                state: LoginState::NeedsSignIn,
                from: FROM_CLAUDE_SWAP.into(),
            });
        }
    }
    found
}

/// Glasscode's accounts, from its `state.json`: only read (a broken file stays as it is, which
/// `store::load` would not do), and each account on its own, so that one it cannot read does not
/// hide the others.
fn glasscode_accounts(state_file: &Path) -> Vec<Account> {
    let Some(text) = small_file(state_file, INDEX_FILE) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let Some(list) = value["accounts"].as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|account| serde_json::from_value(account.clone()).ok())
        .collect()
}

/// Where claude-swap keeps its data: `~/.claude-swap-backup` on macOS and Windows (and on Linux in
/// older versions), `$XDG_DATA_HOME/claude-swap` (`~/.local/share/claude-swap`) on Linux.
fn swap_folders(home: &Path) -> Vec<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| cfg!(target_os = "linux") && dir.is_absolute())
        .unwrap_or_else(|| home.join(".local").join("share"));
    vec![home.join(".claude-swap-backup"), data.join("claude-swap")]
}

/// The names of the accounts in claude-swap's `sequence.json`, by slot: the alias the person gave,
/// else the e-mail address. The file has no tokens (claude-swap's `docs/architecture.md`, "State on
/// disk"). Left out: an API key, and a token saved without an address (claude-swap names it
/// `…@token.local`), which a sign-in in the browser cannot make again; and with `skip_active`
/// (the CLI's own login is there, and it is that account), the active slot.
fn swap_accounts(file: &Path, skip_active: bool) -> Vec<String> {
    let Some(text) = small_file(file, INDEX_FILE) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let Some(accounts) = value["accounts"].as_object() else {
        return Vec::new();
    };
    let active = match &value["activeAccountNumber"] {
        Value::Number(number) => Some(number.to_string()),
        Value::String(slot) => Some(slot.clone()),
        _ => None,
    };
    let mut slots: Vec<(&String, &Value)> = accounts.iter().collect();
    slots.sort_by_key(|(slot, _)| (slot.parse::<u64>().unwrap_or(u64::MAX), slot.to_string()));
    let mut names: Vec<String> = Vec::new();
    for (slot, record) in slots {
        if skip_active && active.as_deref() == Some(slot.as_str()) {
            continue;
        }
        let email = record["email"].as_str().unwrap_or_default().trim();
        if record["kind"] == "api_key" || email.ends_with("@token.local") {
            continue;
        }
        let alias = record["alias"].as_str().unwrap_or_default().trim();
        let name = if alias.is_empty() { email } else { alias };
        if !name.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            names.push(name.to_string());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Limits = Limits {
        storage_folders: 100,
        claude_folders: 100,
        claude_files: 3,
        codex_files: 100,
        wait: Duration::from_secs(10),
    };

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn set_time(path: &Path, seconds: u64) {
        filetime::set_file_mtime(path, filetime::FileTime::from_system_time(at(seconds))).unwrap();
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A folder's `file:` address as the editors write it: `file:///c%3A/Users/dev/my%20app`.
    fn uri(folder: &Path) -> String {
        let text = folder.to_string_lossy().replace('\\', "/");
        let path = text
            .split('/')
            .map(|part| urlencoding::encode(part).into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if cfg!(windows) {
            format!("file:///{}{}", path[..1].to_ascii_lowercase(), &path[1..])
        } else {
            format!("file://{path}")
        }
    }

    /// A storage folder of an editor, last used at `seconds`. The folder itself has a later time,
    /// as Cursor gave nearly all of its folders on one day.
    fn editor_window(app_data: &Path, app: &str, hash: &str, workspace_json: &str, seconds: u64) {
        let dir = app_data
            .join(app)
            .join("User")
            .join("workspaceStorage")
            .join(hash);
        write(&dir.join("workspace.json"), workspace_json);
        write(&dir.join("state.vscdb"), "");
        set_time(&dir.join("state.vscdb"), seconds);
        set_time(&dir, 9_000_000);
    }

    fn folder_json(folder: &Path) -> String {
        serde_json::json!({ "folder": uri(folder) }).to_string()
    }

    /// A Claude Code conversation in `folder`, last written at `seconds`.
    fn claude_conversation(home: &Path, folder: &Path, id: &str, entrypoint: &str, seconds: u64) {
        let dir = home
            .join(".claude")
            .join("projects")
            .join(crate::history::claude_folder_name(folder));
        let summary = serde_json::json!({ "type": "summary", "summary": "Fix the build" });
        let user = serde_json::json!({ "type": "user", "cwd": folder, "entrypoint": entrypoint, "message": { "role": "user", "content": "fix the build" } });
        let file = dir.join(format!("{id}.jsonl"));
        write(&file, &format!("{summary}\n{user}\n"));
        set_time(&file, seconds);
        set_time(&dir, seconds);
    }

    /// A Codex conversation in `folder` of the day `day` (`2026/11/15`), last written at `seconds`.
    fn codex_conversation(
        home: &Path,
        folder: &Path,
        day: &str,
        id: &str,
        source: &str,
        seconds: u64,
    ) {
        let meta = serde_json::json!({ "type": "session_meta", "payload": { "id": id, "cwd": folder, "source": source, "originator": "codex_cli_rs" } });
        let file = home.join(".codex").join("sessions").join(day).join(format!(
            "rollout-{}T10-00-00-{id}.jsonl",
            day.replace('/', "-")
        ));
        write(&file, &format!("{meta}\n"));
        set_time(&file, seconds);
    }

    fn find(home: &Path, app_data: &Path, limits: &Limits) -> Vec<FoundProject> {
        let seen = gather(home, app_data, limits, cfg!(windows));
        keep_existing(
            join(seen, home, &home.join(".glasscode")),
            limits.wait,
            Path::is_dir,
        )
    }

    fn names(found: &[FoundProject]) -> Vec<&str> {
        found.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn editor_addresses_become_local_folders() {
        let windows = [
            (
                "file:///c%3A/Users/dev/my%20app",
                Some(r"C:\Users\dev\my app"),
            ),
            (
                "file:///d%3A/Work%20%28old%29/caf%C3%A9%2B1/",
                Some(r"D:\Work (old)\café+1"),
            ),
            ("FILE:///C:/Users/dev/app", Some(r"C:\Users\dev\app")),
            ("file:///c%3A/", Some(r"C:\")),
            ("file:///c%3A", Some(r"C:\")),
            ("file://localhost/c%3A/app", Some(r"C:\app")),
            ("file://server/share/app/", Some(r"\\server\share\app")),
            (
                "file://wsl.localhost/Ubuntu/home/dev/app",
                Some(r"\\wsl.localhost\Ubuntu\home\dev\app"),
            ),
            (
                "file://wsl%24/Ubuntu/home/dev/app",
                Some(r"\\wsl$\Ubuntu\home\dev\app"),
            ),
            ("file://server/", None),
            ("file:///home/dev/app", None),
            ("file:///cc%3A/app", None),
        ];
        for (uri, folder) in windows {
            assert_eq!(file_uri_path(uri, true).as_deref(), folder, "{uri}");
        }
        let unix = [
            ("file:///Users/dev/my%20app/", Some("/Users/dev/my app")),
            ("file:///home/dev/caf%C3%A9?x=1#y", Some("/home/dev/café")),
            ("file://localhost/home/dev/app", Some("/home/dev/app")),
            ("file:///", Some("/")),
            ("file://server/share/app", None),
        ];
        for (uri, folder) in unix {
            assert_eq!(file_uri_path(uri, false).as_deref(), folder, "{uri}");
        }
        // Remote windows, other schemes, and addresses that are not text.
        for uri in [
            "vscode-remote://ssh-remote%2Bbox/home/dev/app",
            "vscode-vfs://github/acme/app",
            "",
            "file:",
            "file:/c%3A/app",
            "file:///c%3A/a%00b",
            "file:///c%3A/%FF",
        ] {
            assert_eq!(file_uri_path(uri, true), None, "{uri}");
            assert_eq!(file_uri_path(uri, false), None, "{uri}");
        }
    }

    #[test]
    fn editor_windows_give_their_folders_and_broken_ones_are_left_out() {
        let (home, app_data, work) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let spaces = work.path().join("my app (v2)");
        let other = work.path().join("other");
        std::fs::create_dir_all(&spaces).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        editor_window(
            app_data.path(),
            "Cursor",
            "a1",
            &folder_json(&spaces),
            1_000,
        );
        editor_window(app_data.path(), "Code", "b1", &folder_json(&other), 2_000);
        // A folder that is gone, broken files, a workspace file, a remote window: none of them.
        editor_window(
            app_data.path(),
            "Code",
            "b2",
            &folder_json(&work.path().join("gone")),
            3_000,
        );
        editor_window(
            app_data.path(),
            "Code",
            "b3",
            "{\"folder\": \"file:///c%3A/br",
            3_000,
        );
        editor_window(
            app_data.path(),
            "Code",
            "b4",
            "\u{0}\u{1}not json at all",
            3_000,
        );
        editor_window(
            app_data.path(),
            "Code",
            "b5",
            &serde_json::json!({ "workspace": uri(&work.path().join("all.code-workspace")) })
                .to_string(),
            3_000,
        );
        editor_window(
            app_data.path(),
            "Code",
            "b6",
            "{\"folder\": \"vscode-remote://ssh-remote%2Bbox/home/dev/app\"}",
            3_000,
        );
        editor_window(app_data.path(), "Code", "b7", "[1, 2, 3]", 3_000);
        editor_window(
            app_data.path(),
            "Code",
            "b8",
            &format!(
                "{{\"folder\": \"{}\", \"pad\": \"{}\"}}",
                uri(&other),
                "x".repeat(WORKSPACE_FILE as usize)
            ),
            3_000,
        );
        std::fs::write(work.path().join("all.code-workspace"), "{}").unwrap();
        // A storage folder without its file, and one whose file is a folder.
        std::fs::create_dir_all(app_data.path().join("Code/User/workspaceStorage/b9")).unwrap();
        std::fs::create_dir_all(
            app_data
                .path()
                .join("Code/User/workspaceStorage/b10/workspace.json"),
        )
        .unwrap();
        // Without `state.vscdb`, the time of the storage folder.
        let plain = work.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let dir = app_data.path().join("Cursor/User/workspaceStorage/a2");
        write(&dir.join("workspace.json"), &folder_json(&plain));
        set_time(&dir, 1_500);
        let found = find(home.path(), app_data.path(), &ALL);
        assert_eq!(names(&found), ["other", "plain", "my app (v2)"]);
        assert_eq!(found[0].sources, [Source::VsCode]);
        assert_eq!(found[0].last_used, Some(at(2_000)));
        assert_eq!(found[1].last_used, Some(at(1_500)));
        assert!(
            crate::sys::same_path(&found[2].root, &spaces),
            "{}",
            found[2].root.display()
        );
        assert_eq!(
            (found[2].sources.as_slice(), found[2].last_used),
            ([Source::Cursor].as_slice(), Some(at(1_000)))
        );
    }

    /// A link at `link` to the folder `target`: a junction on Windows (no privilege needed), a
    /// symbolic link elsewhere.
    fn link_folder(target: &Path, link: &Path) {
        #[cfg(windows)]
        {
            // With `\` only: cmd.exe takes `/User` for a switch.
            let status = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(crate::sys::clean_path(link))
                .arg(crate::sys::clean_path(target))
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "mklink /J failed");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        assert!(link.join(".").is_dir(), "{} leads nowhere", link.display());
    }

    #[test]
    fn links_out_of_the_folders_that_are_read_are_not_followed() {
        let (home, app_data, work, elsewhere) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let (linked, inside) = (work.path().join("linked"), work.path().join("inside"));
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::create_dir_all(&inside).unwrap();
        // Outside the folders that are read: a storage folder and a Claude project folder.
        let outside = elsewhere.path().join("window");
        write(&outside.join("workspace.json"), &folder_json(&linked));
        claude_conversation(elsewhere.path(), &linked, "c1", "cli", 1_000);
        // Inside them, links to those.
        let storage = app_data.path().join("Code/User/workspaceStorage");
        std::fs::create_dir_all(&storage).unwrap();
        link_folder(&outside, &storage.join("b1"));
        let projects = home.path().join(".claude/projects");
        std::fs::create_dir_all(&projects).unwrap();
        let claude_folder = crate::history::claude_folder_name(&linked);
        link_folder(
            &elsewhere
                .path()
                .join(".claude/projects")
                .join(&claude_folder),
            &projects.join(&claude_folder),
        );
        // And a real one, which is found.
        editor_window(app_data.path(), "Code", "b2", &folder_json(&inside), 1_000);
        assert_eq!(names(&find(home.path(), app_data.path(), &ALL)), ["inside"]);
    }

    #[test]
    fn the_same_folder_from_each_program_is_one_project_and_the_newest_use_comes_first() {
        let (home, app_data, work) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let (app, api, site) = (
            work.path().join("app"),
            work.path().join("api"),
            work.path().join("site"),
        );
        for folder in [&app, &api, &site] {
            std::fs::create_dir_all(folder).unwrap();
        }
        editor_window(app_data.path(), "Cursor", "a1", &folder_json(&app), 1_000);
        editor_window(app_data.path(), "Code", "b1", &folder_json(&app), 1_500);
        // Claude Code and Codex keep the folder as it was given: another spelling of the same one.
        let spelled = if cfg!(windows) {
            PathBuf::from(app.to_string_lossy().to_uppercase() + "\\")
        } else {
            PathBuf::from(format!("{}/", app.display()))
        };
        claude_conversation(home.path(), &spelled, "c1", "cli", 4_000);
        codex_conversation(home.path(), &app, "2026/11/15", "x1", "cli", 2_000);
        codex_conversation(home.path(), &api, "2026/11/14", "x2", "vscode", 5_000);
        claude_conversation(home.path(), &site, "c2", "cli", 3_000);
        let found = find(home.path(), app_data.path(), &ALL);
        assert_eq!(names(&found), ["api", "app", "site"]);
        assert_eq!(
            found[1].sources,
            [
                Source::Cursor,
                Source::VsCode,
                Source::ClaudeCode,
                Source::Codex
            ]
        );
        assert_eq!(found[1].last_used, Some(at(4_000)));
        assert_eq!(
            (found[0].sources.as_slice(), found[0].last_used),
            ([Source::Codex].as_slice(), Some(at(5_000)))
        );
        assert_eq!(
            (found[2].sources.as_slice(), found[2].last_used),
            ([Source::ClaudeCode].as_slice(), Some(at(3_000)))
        );
    }

    #[test]
    fn scripts_runs_of_other_tools_and_glasscodes_own_folders_are_no_projects() {
        let (home, app_data, work) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let (script, exec, worktree) = (
            work.path().join("script"),
            work.path().join("exec"),
            home.path()
                .join(".glasscode")
                .join("worktrees")
                .join("app")
                .join("feat"),
        );
        for folder in [&script, &exec, &worktree] {
            std::fs::create_dir_all(folder).unwrap();
        }
        claude_conversation(home.path(), &script, "c1", "sdk-cli", 1_000);
        codex_conversation(home.path(), &exec, "2026/11/15", "x1", "exec", 1_000);
        claude_conversation(home.path(), &worktree, "c2", "cli", 1_000);
        // A CLI started in the home folder.
        codex_conversation(home.path(), home.path(), "2026/11/15", "x2", "cli", 1_000);
        // Another tool's Codex run, and a sub-agent.
        let file = home
            .path()
            .join(".codex/sessions/2026/11/15/rollout-2026-11-15T11-00-00-x3.jsonl");
        let meta = serde_json::json!({ "type": "session_meta", "payload": { "id": "x3", "cwd": exec, "source": "cli", "originator": "Claude Code" } });
        let child = serde_json::json!({ "type": "session_meta", "payload": { "id": "x4", "cwd": exec, "source": "cli", "thread_source": "subagent" } });
        write(&file, &format!("{meta}\n"));
        write(
            &file.with_file_name("rollout-2026-11-15T12-00-00-x4.jsonl"),
            &format!("{child}\n"),
        );
        assert_eq!(
            names(&find(home.path(), app_data.path(), &ALL)),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn broken_and_huge_conversation_files_are_left_out() {
        let (home, app_data, work) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let (good, late) = (work.path().join("good"), work.path().join("late"));
        std::fs::create_dir_all(&good).unwrap();
        std::fs::create_dir_all(&late).unwrap();
        // Claude: lines that are not JSON, then the folder.
        let dir = home.path().join(".claude/projects/broken");
        write(
            &dir.join("a.jsonl"),
            &format!(
                "not json\n{{\"cwd\": 3}}\n\u{0}\u{1}\n{}\n",
                serde_json::json!({ "type": "user", "cwd": good })
            ),
        );
        // The folder only after the part that is read: not found.
        let filler = format!(
            "{{\"type\":\"summary\",\"summary\":\"{}\"}}\n",
            "x".repeat(1000)
        );
        let late_line = serde_json::json!({ "type": "user", "cwd": late });
        write(
            &home.path().join(".claude/projects/late/b.jsonl"),
            &(filler.repeat(CLAUDE_HEAD as usize / 1000 + 10) + &format!("{late_line}\n")),
        );
        // Codex: a first line longer than the part read, one cut off, and one that is not JSON.
        let codex = home.path().join(".codex/sessions/2026/11/15");
        let long = serde_json::json!({ "type": "session_meta", "payload": { "cwd": late, "source": "cli", "base_instructions": "x".repeat(CODEX_HEAD as usize) } });
        write(
            &codex.join("rollout-2026-11-15T10-00-00-x1.jsonl"),
            &format!("{long}\n"),
        );
        write(
            &codex.join("rollout-2026-11-15T10-00-01-x2.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":",
        );
        write(
            &codex.join("rollout-2026-11-15T10-00-02-x3.jsonl"),
            "\u{0}\n",
        );
        std::fs::write(codex.join("notes.txt"), "").unwrap();
        // A file where a folder of years should be.
        write(&home.path().join(".codex/sessions/2027"), "");
        let found = find(home.path(), app_data.path(), &ALL);
        assert_eq!(names(&found), ["good"]);
    }

    #[test]
    fn only_the_newest_storage_folders_and_conversations_are_read() {
        let (home, app_data, work) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let folders: Vec<PathBuf> = (0..9).map(|n| work.path().join(format!("p{n}"))).collect();
        for folder in &folders {
            std::fs::create_dir_all(folder).unwrap();
        }
        for n in 0..3 {
            editor_window(
                app_data.path(),
                "Code",
                &format!("h{n}"),
                &folder_json(&folders[n]),
                1_000 + n as u64,
            );
            claude_conversation(
                home.path(),
                &folders[3 + n],
                &format!("c{n}"),
                "cli",
                1_000 + n as u64,
            );
            codex_conversation(
                home.path(),
                &folders[6 + n],
                &format!("2026/11/1{n}"),
                &format!("x{n}"),
                "cli",
                1_000 + n as u64,
            );
        }
        let limits = Limits {
            storage_folders: 2,
            claude_folders: 1,
            claude_files: 1,
            codex_files: 2,
            wait: ALL.wait,
        };
        let mut found = names(&find(home.path(), app_data.path(), &limits)).join(" ");
        // Used at the same time: by name.
        assert_eq!(found, "p2 p5 p8 p1 p7");
        found = names(&find(home.path(), app_data.path(), &ALL)).join(" ");
        assert_eq!(found, "p2 p5 p8 p1 p4 p7 p0 p3 p6");
    }

    #[test]
    fn folders_with_spaces_and_on_another_drive_are_found() {
        // The build folder, which on Windows is often on another drive than the temporary folder
        // (W: and C: here, D: and C: in the automatic build).
        let build = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target");
        std::fs::create_dir_all(&build).unwrap();
        let build = crate::sys::clean_path(&build.canonicalize().unwrap());
        let (home, app_data, work, there) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir_in(&build).unwrap(),
        );
        let (near, far) = (work.path().join("near app"), there.path().join("far app"));
        std::fs::create_dir_all(&near).unwrap();
        std::fs::create_dir_all(&far).unwrap();
        editor_window(app_data.path(), "Cursor", "a1", &folder_json(&near), 2_000);
        editor_window(app_data.path(), "Code", "b1", &folder_json(&far), 1_000);
        claude_conversation(home.path(), &far, "c1", "cli", 1_500);
        let found = find(home.path(), app_data.path(), &ALL);
        assert_eq!(names(&found), ["near app", "far app"]);
        assert_eq!(found[1].sources, [Source::VsCode, Source::ClaudeCode]);
        assert!(
            crate::sys::same_path(&found[1].root, &far),
            "{}",
            found[1].root.display()
        );
    }

    #[test]
    fn nothing_there_gives_nothing() {
        let (home, app_data) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        assert!(find(home.path(), app_data.path(), &ALL).is_empty());
        assert!(
            find(
                &home.path().join("missing"),
                &app_data.path().join("missing"),
                &ALL
            )
            .is_empty()
        );
    }

    fn project(root: &str, seconds: u64) -> FoundProject {
        FoundProject {
            root: PathBuf::from(root),
            name: root.rsplit(['/', '\\']).next().unwrap_or(root).into(),
            sources: vec![Source::Cursor],
            last_used: Some(at(seconds)),
        }
    }

    /// The paths that `slow_probe` was asked for.
    static ASKED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

    /// A drive that does not answer (`slow`), and folders that are all there.
    fn slow_probe(path: &Path) -> bool {
        ASKED.lock().unwrap().push(path.to_path_buf());
        if path.to_string_lossy().contains("slow") {
            std::thread::sleep(Duration::from_secs(30));
        }
        true
    }

    #[test]
    fn a_drive_that_does_not_answer_does_not_hold_the_list() {
        let list = if cfg!(windows) {
            vec![
                project(r"S:\slow\app", 3),
                project(r"F:\fast\app", 2),
                project(r"\\server\share\app", 1),
                project(r"F:\fast\api", 4),
                project(r"T:\a\slow", 5),
            ]
        } else {
            vec![
                project("/slow/disk/app", 3),
                project("/fast/disk/app", 2),
                project("/fast/disk/api", 4),
                project("/media/disk/slow", 5),
            ]
        };
        let started = Instant::now();
        let kept = keep_existing(list, Duration::from_secs(2), slow_probe);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(names(&kept), ["api", "app"]);
        // A share is never asked: that would go on the network.
        assert!(
            !ASKED
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.to_string_lossy().starts_with(r"\\"))
        );
    }

    #[test]
    fn volumes_are_drives_or_the_first_two_folders() {
        if cfg!(windows) {
            assert_eq!(
                volume(Path::new(r"C:\Users\dev\app")),
                Some(PathBuf::from(r"C:\"))
            );
            assert_eq!(
                volume(Path::new(r"\\?\D:\work")),
                Some(PathBuf::from(r"D:\"))
            );
            assert_eq!(volume(Path::new(r"\\server\share\app")), None);
            assert_eq!(volume(Path::new(r"\\?\UNC\server\share\app")), None);
        } else {
            assert_eq!(
                volume(Path::new("/Volumes/Backup/work/app")),
                Some(PathBuf::from("/Volumes/Backup"))
            );
            assert_eq!(volume(Path::new("/srv")), Some(PathBuf::from("/srv")));
        }
        assert_eq!(volume(Path::new("relative/app")), None);
    }

    // ----- logins -----

    fn login(
        agent: AgentKind,
        label: &str,
        dir: Option<PathBuf>,
        state: LoginState,
        from: &str,
    ) -> FoundLogin {
        FoundLogin {
            agent,
            label: label.into(),
            dir,
            state,
            from: from.into(),
        }
    }

    #[test]
    fn a_login_is_its_file_and_the_file_is_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let (claude, codex) = (dir.path().join("claude"), dir.path().join("codex"));
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::create_dir_all(&codex).unwrap();
        // An empty folder has no login.
        assert!(!login_present(AgentKind::Claude, &claude, false));
        assert!(!login_present(AgentKind::Codex, &codex, false));
        assert!(!login_present(
            AgentKind::Codex,
            &dir.path().join("missing"),
            false
        ));
        // Whatever the entry holds, it counts: text that is no login, a folder of that name.
        std::fs::write(claude.join(".credentials.json"), "not a login").unwrap();
        std::fs::create_dir_all(codex.join("auth.json")).unwrap();
        assert!(login_present(AgentKind::Claude, &claude, false));
        assert!(login_present(AgentKind::Codex, &codex, false));
        // And a file that cannot be opened at all.
        let locked = dir.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        #[cfg(windows)]
        let _open = {
            use std::os::windows::fs::OpenOptionsExt as _;
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .share_mode(0)
                .open(locked.join("auth.json"))
                .unwrap();
            assert!(
                std::fs::read(locked.join("auth.json")).is_err(),
                "Windows let the file be read"
            );
            file
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(locked.join("auth.json"), "").unwrap();
            std::fs::set_permissions(
                locked.join("auth.json"),
                std::fs::Permissions::from_mode(0o000),
            )
            .unwrap();
        }
        assert!(login_present(AgentKind::Codex, &locked, false));
        // On macOS Claude's login is in the Keychain: its folder counts.
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(login_present(AgentKind::Claude, &empty, true));
        assert!(!login_present(
            AgentKind::Claude,
            &dir.path().join("missing"),
            true
        ));
        assert!(!login_present(AgentKind::Codex, &empty, true));
    }

    /// claude-swap's `sequence.json` as its version of 2026-11 writes it (made-up accounts).
    const SEQUENCE: &str = r#"{
        "activeAccountNumber": 1,
        "lastUpdated": "2026-11-15T10:00:00Z",
        "sequence": [1, 2, 3, 4, 5, 10],
        "accounts": {
            "10": {"email": "sam@example.org", "uuid": "", "organizationUuid": "", "organizationName": "", "added": "2026-11-15T10:00:00Z"},
            "1": {"email": "alex@example.com", "uuid": "u1", "organizationUuid": "", "organizationName": "", "added": "2026-11-01T10:00:00Z"},
            "2": {"email": "alex@acme.example", "uuid": "u2", "organizationUuid": "o2", "organizationName": "Acme", "added": "2026-11-02T10:00:00Z", "alias": "Work"},
            "3": {"email": "api-key-3@token.local", "uuid": "", "organizationUuid": "", "organizationName": "", "added": "2026-11-03T10:00:00Z", "kind": "api_key"},
            "4": {"email": "setup-token-4@token.local", "uuid": "", "organizationUuid": "", "organizationName": "", "added": "2026-11-04T10:00:00Z"},
            "5": {"email": "side@example.com", "uuid": "u5", "organizationUuid": "", "organizationName": "", "added": "2026-11-05T10:00:00Z", "alias": "Side project", "disabled": true}
        }
    }"#;

    #[test]
    fn logins_are_the_clis_glasscodes_accounts_and_claude_swaps_names() {
        let (home, glasscode) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (home, own) = (home.path(), glasscode.path());
        // The CLIs' own logins: Claude signed in, Codex not.
        write(&home.join(".claude").join(".credentials.json"), "{");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        // Glasscode's accounts: a renamed system login, two folders of its own, and a broken one.
        let state = serde_json::json!({
            "projects": [],
            "accounts": [
                { "id": "claude-default", "agent": "claude", "label": "Personal", "system_default": true },
                { "id": "claude-1a2b3c4d", "agent": "claude", "label": "Side project" },
                { "id": "codex-5e6f7a8b", "agent": "codex", "label": "Codex work" },
                { "id": "../escape", "agent": "claude", "label": "Escape" },
                { "id": "x", "agent": "gemini", "label": "Not an agent" },
            ],
        });
        write(&own.join("state.json"), &state.to_string());
        std::fs::create_dir_all(
            own.join("accounts")
                .join("claude-1a2b3c4d")
                .join(".credentials.json"),
        )
        .unwrap();
        std::fs::create_dir_all(own.join("accounts").join("codex-5e6f7a8b")).unwrap();
        // claude-swap: the names, next to folders of tokens that are never opened.
        let swap = home.join(".claude-swap-backup");
        write(&swap.join("sequence.json"), SEQUENCE);
        write(
            &swap
                .join("credentials")
                .join(".claude-credentials-1-alex@example.com.json"),
            "secret",
        );
        write(
            &swap
                .join("configs")
                .join(".claude-config-1-alex@example.com.json"),
            "{}",
        );
        let swap_folders = [swap, home.join(".local").join("share").join("claude-swap")];
        let found = find_logins(home, own, &swap_folders, false);
        assert_eq!(
            found,
            [
                login(
                    AgentKind::Claude,
                    "Personal",
                    Some(home.join(".claude")),
                    LoginState::SignedIn,
                    FROM_SYSTEM
                ),
                login(
                    AgentKind::Codex,
                    "My login",
                    Some(home.join(".codex")),
                    LoginState::NeedsSignIn,
                    FROM_SYSTEM
                ),
                login(
                    AgentKind::Claude,
                    "Side project",
                    Some(own.join("accounts").join("claude-1a2b3c4d")),
                    LoginState::SignedIn,
                    FROM_GLASSCODE
                ),
                login(
                    AgentKind::Codex,
                    "Codex work",
                    Some(own.join("accounts").join("codex-5e6f7a8b")),
                    LoginState::NeedsSignIn,
                    FROM_GLASSCODE
                ),
                // Slot 1 is the login of `~/.claude`; 3 and 4 are tokens; 5 is a Glasscode account.
                login(
                    AgentKind::Claude,
                    "Work",
                    None,
                    LoginState::NeedsSignIn,
                    FROM_CLAUDE_SWAP
                ),
                login(
                    AgentKind::Claude,
                    "sam@example.org",
                    None,
                    LoginState::NeedsSignIn,
                    FROM_CLAUDE_SWAP
                ),
            ]
        );
        // Without a login in `~/.claude`, the active slot is one to sign in to again.
        std::fs::remove_file(home.join(".claude").join(".credentials.json")).unwrap();
        let labels: Vec<String> = find_logins(home, own, &swap_folders, false)
            .into_iter()
            .filter(|l| l.from == FROM_CLAUDE_SWAP)
            .map(|l| l.label)
            .collect();
        assert_eq!(labels, ["alex@example.com", "Work", "sam@example.org"]);
    }

    #[test]
    fn no_login_anywhere_gives_an_empty_list() {
        let (home, glasscode) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let swap = [home.path().join(".claude-swap-backup")];
        assert!(find_logins(home.path(), glasscode.path(), &swap, false).is_empty());
        // Broken files of Glasscode and claude-swap give nothing, and stay as they were.
        write(&glasscode.path().join("state.json"), "{\"accounts\": [");
        write(&swap[0].join("sequence.json"), "{\"accounts\": {\"1\": ");
        assert!(find_logins(home.path(), glasscode.path(), &swap, false).is_empty());
        assert_eq!(
            std::fs::read_to_string(glasscode.path().join("state.json")).unwrap(),
            "{\"accounts\": ["
        );
        assert!(!glasscode.path().join("state.json.broken").exists());
        // A Codex folder alone: the CLI ran here and has not signed in.
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        let found = find_logins(home.path(), glasscode.path(), &swap, false);
        assert_eq!(
            found,
            [login(
                AgentKind::Codex,
                "My login",
                Some(home.path().join(".codex")),
                LoginState::NeedsSignIn,
                FROM_SYSTEM
            )]
        );
    }

    #[test]
    fn claude_swap_on_linux_is_in_the_data_folder() {
        let home = tempfile::tempdir().unwrap();
        let folders = swap_folders(home.path());
        assert_eq!(folders[0], home.path().join(".claude-swap-backup"));
        assert!(
            folders[1].ends_with("claude-swap"),
            "{}",
            folders[1].display()
        );
        if !cfg!(target_os = "linux") {
            assert_eq!(
                folders[1],
                home.path().join(".local").join("share").join("claude-swap")
            );
        }
    }

    /// What `projects` and `logins` find on this computer, as counts only (folder names and
    /// accounts are the person's own): `cargo test -p glasscode_core discover -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore = "reads the folders of this computer; run by hand"]
    fn what_this_computer_has() {
        let home = crate::paths::home();
        let app_data = app_data().expect("this system has a folder for app data");
        let started = Instant::now();
        let seen = gather(&home, &app_data, &LIMITS, cfg!(windows));
        let read = started.elapsed();
        let count = |source: Source| seen.iter().filter(|s| s.source == source).count();
        println!(
            "recorded: Cursor {}, VS Code {}, Claude Code {}, Codex {} (read in {read:?})",
            count(Source::Cursor),
            count(Source::VsCode),
            count(Source::ClaudeCode),
            count(Source::Codex)
        );
        let joined = join(seen, &home, &crate::paths::root());
        let shares = joined.iter().filter(|p| volume(&p.root).is_none()).count();
        println!(
            "folders after joining: {} ({shares} on a share, not asked)",
            joined.len()
        );
        let started = Instant::now();
        let found = projects(&home, &app_data);
        println!("projects(): {} in {:?}", found.len(), started.elapsed());
        let count = |source: Source| found.iter().filter(|p| p.sources.contains(&source)).count();
        println!(
            "  known to Cursor {}, VS Code {}, Claude Code {}, Codex {}",
            count(Source::Cursor),
            count(Source::VsCode),
            count(Source::ClaudeCode),
            count(Source::Codex)
        );
        println!(
            "  known to more than one program: {}",
            found.iter().filter(|p| p.sources.len() > 1).count()
        );
        println!(
            "  linked worktrees (.git is a file): {}",
            found
                .iter()
                .filter(|p| p.root.join(".git").is_file())
                .count()
        );
        println!(
            "  git repositories (.git is a folder): {}",
            found
                .iter()
                .filter(|p| p.root.join(".git").is_dir())
                .count()
        );
        println!(
            "  without a time of use: {}",
            found.iter().filter(|p| p.last_used.is_none()).count()
        );
        println!(
            "  last used first: {}",
            found.windows(2).all(|w| w[0].last_used >= w[1].last_used)
        );
        let logins = logins(&home);
        for from in [FROM_SYSTEM, FROM_GLASSCODE, FROM_CLAUDE_SWAP] {
            let of: Vec<&FoundLogin> = logins.iter().filter(|l| l.from == from).collect();
            let signed = of
                .iter()
                .filter(|l| l.state == LoginState::SignedIn)
                .count();
            println!("logins from {from}: {} ({signed} signed in)", of.len());
        }
    }
}
