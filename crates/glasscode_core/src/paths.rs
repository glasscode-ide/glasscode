//! Where Glasscode keeps its files. Everything lives under `~/.glasscode` so a server
//! can mirror the same layout. Each path has a class (`class_of`, design section 5 of Manager
//! v2), which the session profiles build their rules from.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::model::AgentKind;

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// A test run's own Glasscode folder (see [`use_test_root`]).
static TEST_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Tests: from now on [`root`] is a new temporary folder of this test process, so that a test never
/// reads or writes the person's `~/.glasscode` (a test run once gave its `accounts` folder a new
/// time). The core's own tests have it always; a test of another crate that reaches `root` calls
/// it first. Setting `GLASSCODE_HOME` instead would race with the tests that run beside it.
pub fn use_test_root() -> PathBuf {
    TEST_ROOT
        .get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("glasscode-test-home-{}", std::process::id()));
            if let Err(err) = std::fs::create_dir_all(&dir) {
                panic!("could not make the tests' Glasscode folder {}: {err}", dir.display());
            }
            dir
        })
        .clone()
}

/// Whether this is a test run (the core's own tests, or another crate's that set the test root):
/// what would reach outside the Glasscode folder (the person's own `~/.codex`, the Keychain)
/// stays in the test's folder or is not done.
pub fn testing() -> bool {
    cfg!(test) || TEST_ROOT.get().is_some()
}

pub fn root() -> PathBuf {
    let set = std::env::var("GLASSCODE_HOME").ok().filter(|dir| !dir.is_empty()).map(PathBuf::from);
    if cfg!(test) {
        // The core's tests: one that sets a folder of its own (one at a time) gets it, every
        // other one the run's.
        return set.unwrap_or_else(use_test_root);
    }
    TEST_ROOT.get().cloned().or(set).unwrap_or_else(|| home().join(".glasscode"))
}

pub fn state_file() -> PathBuf {
    root().join("state.json")
}

pub fn accounts_dir() -> PathBuf {
    root().join("accounts")
}

pub fn worktrees_dir() -> PathBuf {
    root().join("worktrees")
}

pub fn events_dir() -> PathBuf {
    root().join("events")
}

pub fn handoff_dir() -> PathBuf {
    root().join("handoff")
}

/// The Claude settings of each session on Windows (`claude --settings <file>`).
pub fn settings_dir() -> PathBuf {
    root().join("settings")
}

/// How the Mac app reaches its server (`ServerConfig`).
pub fn server_file() -> PathBuf {
    root().join("server.json")
}

/// The daemon's socket on the server; `glasscoded rpc` connects SSH sessions to it.
pub fn daemon_socket() -> PathBuf {
    root().join("glasscoded.sock")
}

/// Which sessions the daemon keeps running (restarted after a server reboot).
pub fn daemon_file() -> PathBuf {
    root().join("glasscoded.json")
}

/// Who runs each worktree that has moved, the Mac or the server (`owners`, kept by glasscoded).
pub fn owners_file() -> PathBuf {
    authority_dir().join("owners.json")
}

/// Copies of text the editor could not save or ask about when Glasscode quit (`save::keep_copy`).
pub fn unsaved_dir() -> PathBuf {
    root().join("unsaved")
}

pub fn cli_versions_file() -> PathBuf {
    root().join("cli-versions.json")
}

/// The CLIs the person added themselves (on top of the built-in Claude Code and Codex).
pub fn clis_file() -> PathBuf {
    root().join("clis.json")
}

/// Policy, playbooks, schedules, approvals, the machine id and the engine switch: written by the
/// app only (Manager v2, D10).
pub fn authority_dir() -> PathBuf {
    root().join("authority")
}

/// The trusted memory, a git repository that `glasscode memory …` alone writes.
pub fn memory_dir() -> PathBuf {
    root().join("memory")
}

/// On the server: the read-only copy of the Mac's memory, without its local-only projects
/// (`memory_copy`, M20.3), checked out from [`memory_server_repo`].
pub fn memory_server_dir() -> PathBuf {
    root().join("memory-server")
}

/// On the server: the repository the Mac pushes its memory copy to.
pub fn memory_server_repo() -> PathBuf {
    root().join("memory-server.git")
}

/// One project's notes of one agent, where Claude's auto-memory is redirected (D19).
pub fn agent_notes_dir(key: &ProjectKey, agent: AgentKind) -> PathBuf {
    root().join("agent-notes").join(key.as_str()).join(agent.name().to_ascii_lowercase())
}

/// The words ledger of one machine: each machine writes only its own folder (D1).
pub fn words_dir(machine: &MachineId) -> PathBuf {
    root().join("words").join(machine.as_str())
}

/// The journal: one file of events a day.
pub fn journal_dir() -> PathBuf {
    root().join("journal")
}

/// The effect ledger, which is also the audit record (D9).
pub fn effects_dir() -> PathBuf {
    root().join("effects")
}

/// The brain's work folder, the only one its file tools reach.
pub fn manager_work_dir() -> PathBuf {
    root().join("manager").join("work")
}

/// The brain's files (its instructions, MCP config and settings), readable by this user only.
pub fn manager_run_dir() -> PathBuf {
    root().join("manager").join("run")
}

/// What the app keeps only to be quick.
pub fn cache_dir() -> PathBuf {
    root().join("cache")
}

/// A name that is one safe folder name on every system: 1 to 64 of `a-z`, `0-9`, `.`, `_` and
/// `-`, starting with a letter or a digit (so never `.`, `..` or a hidden name), not ending in a
/// dot, and not a device name of Windows (`con`, `prn`, `aux`, `nul`, `com0`–`com9`,
/// `lpt0`–`lpt9`, with or without an extension).
fn folder_name(text: &str) -> bool {
    let stem = text.split('.').next().unwrap_or(text);
    let device = matches!(stem, "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.as_bytes()[3].is_ascii_digit());
    (1..=64).contains(&text.len())
        && text.as_bytes()[0].is_ascii_alphanumeric()
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
        && !text.ends_with('.')
        && !device
}

/// A project as memory and the agent notes know it (D22): `glasscode-3f9a1c`, a slug and six hex
/// digits. It lives here as a name that is safe as a folder (it comes from synced data too); the
/// identity module makes it from the project's aliases and re-exports it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectKey(String);

impl ProjectKey {
    /// None when `text` is not safe as a folder name.
    pub fn parse(text: &str) -> Option<ProjectKey> {
        folder_name(text).then(|| ProjectKey(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ProjectKey {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        ProjectKey::parse(&text).ok_or_else(|| format!("{text:?} is not a project key"))
    }
}

impl From<ProjectKey> for String {
    fn from(key: ProjectKey) -> String {
        key.0
    }
}

impl std::fmt::Display for ProjectKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One Glasscode machine, for sync and the words ledger (D22): random, made once and kept in
/// `authority/machine.json` (`MachineId::this`, in `store`). It names a folder, and other
/// machines send it, so only a safe folder name is one. The identity module re-exports it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MachineId(String);

impl MachineId {
    /// None when `text` is not safe as a folder name.
    pub fn parse(text: &str) -> Option<MachineId> {
        folder_name(text).then(|| MachineId(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MachineId {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        MachineId::parse(&text).ok_or_else(|| format!("{text:?} is not a machine id"))
    }
}

impl From<MachineId> for String {
    fn from(id: MachineId) -> String {
        id.0
    }
}

impl std::fmt::Display for MachineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a path is to a session (design section 5 of Manager v2). `session_profile()` and
/// `headless_profile()` build their deny and allow rules from these classes and [`layout`] only,
/// never from a literal path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathClass {
    /// `authority/`, `state.json` and the app's and glasscoded's other settings, keys and records:
    /// the app (and glasscoded) only.
    Authority,
    /// The words ledger, `words/<machine>/`.
    Words,
    /// The journal, the effect ledger, the manager's records and the logs.
    Ledger,
    /// `memory/`, which briefs come from.
    TrustedMemory,
    /// `agent-notes/<key>/<agent>/`: that project's sessions read and write it.
    AgentNotes,
    /// `handoff/`: the person's sessions read the note handed to them; automation and quarantine
    /// sessions are denied, as a note can hold another project's diff.
    Handoff,
    /// `worktrees/<project>/<branch>/`.
    Worktree,
    /// The agents' logins: `accounts/<id>/`, `~/.claude`, `~/.codex`.
    Accounts,
    /// What hooks write, and the settings files they are given; hooks run outside the sandbox.
    HookIo,
    /// `manager/work/`, the brain's work folder.
    BrainWork,
    /// What sessions use and never change: the `glasscode` command (`bin/`), Claude's plugin
    /// (`agent/`), the files pasted into a terminal (`pasted/`), and the control socket sessions
    /// connect to.
    SessionTools,
    /// What the app keeps and can make again: the GitHub, usage and Linear caches, the browser's
    /// profile, downloads, the server's mirrors.
    Cache,
    /// The person's credentials, which are not Glasscode's: SSH, AWS, `gh`, `.netrc`, keychains,
    /// and `.env` files anywhere.
    Credentials,
}

/// The folder a [`LayoutRow`] is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base {
    /// The Glasscode folder, [`root`].
    Glasscode,
    /// The person's home folder, [`home`].
    Home,
}

/// One row of the layout (design section 5): an entry of a folder and its class. A folder entry
/// is its whole tree. An entry with an extension is a file, and covers its siblings with one more
/// suffix too (`state.json.broken`, `.prev`, `.lock`, `.tmp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutRow {
    pub base: Base,
    /// `authority`, `manager/work`, `.config/gh`.
    pub entry: &'static str,
    pub class: PathClass,
}

impl LayoutRow {
    /// Where the row is on this machine.
    pub fn path(&self) -> PathBuf {
        let base = match self.base {
            Base::Glasscode => root(),
            Base::Home => home(),
        };
        self.entry.split('/').fold(base, |path, part| path.join(part))
    }
}

const fn row(base: Base, entry: &'static str, class: PathClass) -> LayoutRow {
    LayoutRow { base, entry, class }
}

use Base::{Glasscode as G, Home as H};
use PathClass::*;

/// Every entry of the Glasscode folder, and the entries of the home folder that design section 5
/// names. The longest entry that matches a path decides (`manager/work` before `manager`).
const LAYOUT: &[LayoutRow] = &[
    row(G, "authority", Authority),
    row(G, "state.json", Authority),
    row(G, "server.json", Authority),
    row(G, "glasscoded.json", Authority),
    row(G, "glasscoded.sock", Authority),
    row(G, "cli-versions.json", Authority),
    row(G, "clis.json", Authority),
    // The key glasscoded signs its sessions' requests with.
    row(G, "control.key", Authority),
    row(G, "push.json", Authority),
    row(G, "push-devices.json", Authority),
    row(G, "relay.json", Authority),
    row(G, "auto-review.json", Authority),
    row(G, "ci-fix.json", Authority),
    row(G, "tmux.conf", Authority),
    // It steers auto-review, so sessions do not write it.
    row(G, "review-prompt.md", Authority),
    row(G, "device", Authority),
    row(G, "machine-id", Authority),
    row(G, "board.json", Authority),
    row(G, "activity.json", Authority),
    row(G, "words", Words),
    row(G, "journal", Ledger),
    row(G, "effects", Ledger),
    row(G, "agent-log.jsonl", Ledger),
    row(G, "glasscoded.log", Ledger),
    row(G, "manager", Ledger),
    row(G, "manager/work", BrainWork),
    row(G, "memory", TrustedMemory),
    row(G, "memory-server", TrustedMemory),
    // The voice copilot's own memory (voice/memory.mjs); M20.3 reads its facts once.
    row(G, "glasscode-memory.json", Authority),
    row(G, "memory-server.git", TrustedMemory),
    row(G, "agent-notes", AgentNotes),
    row(G, "handoff", Handoff),
    row(G, "worktrees", Worktree),
    row(G, "accounts", Accounts),
    row(G, "events", HookIo),
    row(G, "settings", HookIo),
    row(G, "bin", SessionTools),
    row(G, "agent", SessionTools),
    row(G, "pasted", SessionTools),
    // Sessions connect to it (the `glasscode` command); a profile allows the connection only.
    row(G, "control.sock", SessionTools),
    row(G, "cache", Cache),
    row(G, "github-cache.json", Cache),
    row(G, "usage-cache.json", Cache),
    row(G, "linear.json", Cache),
    row(G, "browser", Cache),
    row(G, "unsaved", Cache),
    row(G, "mirrors", Cache),
    row(G, "usage", Cache),
    row(G, "updates", Cache),
    row(H, ".claude", Accounts),
    row(H, ".claude.json", Accounts),
    row(H, ".codex", Accounts),
    row(H, ".ssh", Credentials),
    row(H, ".aws", Credentials),
    row(H, ".config/gh", Credentials),
    row(H, ".netrc", Credentials),
    // Its encrypted and other forms, which the same programs read (the review of M10).
    row(H, ".netrc.gpg", Credentials),
    row(H, ".authinfo", Credentials),
    row(H, ".authinfo.gpg", Credentials),
    row(H, ".git-credentials", Credentials),
    row(H, ".gnupg", Credentials),
    // Keychain files: macOS, the Linux keyring, and Windows' credential and key folders.
    row(H, "Library/Keychains", Credentials),
    row(H, ".local/share/keyrings", Credentials),
    row(H, "AppData/Roaming/Microsoft/Credentials", Credentials),
    row(H, "AppData/Local/Microsoft/Credentials", Credentials),
    row(H, "AppData/Roaming/Microsoft/Protect", Credentials),
];

/// The layout, for the session profiles to turn classes into paths ([`LayoutRow::path`]).
pub fn layout() -> &'static [LayoutRow] {
    LAYOUT
}

/// What [`class_of`] says of a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classified {
    /// A path of the layout, or a `.env` file: its class.
    Class(PathClass),
    /// Inside the Glasscode folder (or the folder itself) but in no row: denied to sessions.
    InsideUnlisted,
    /// Outside the Glasscode folder and in no row, or a relative path.
    Outside,
}

impl Classified {
    pub fn class(self) -> Option<PathClass> {
        match self {
            Classified::Class(class) => Some(class),
            _ => None,
        }
    }
}

/// The class of `path` (design section 5). The path is read as written, with letter case
/// ignored, `.` and `..` taken back and Windows' `\\?\` prefixes folded; it matches the Glasscode
/// and home folders both as they are named and with their links resolved (by their nearest
/// folder that exists, when they do not exist yet), so a canonical path (`/private/var/…` on
/// macOS) has its class too. When the Glasscode folder cannot be resolved at all, everything under
/// it is denied. A file (not a folder) whose name starts with `.env` is a credential wherever
/// sessions could otherwise read it. It does not resolve links in `path` itself; it reads the disk
/// to resolve the two folders, and to tell a `.env…` folder from a file.
pub fn class_of(path: &Path) -> Classified {
    if !path.is_absolute() {
        return Classified::Outside;
    }
    let parts = normal(path);
    let (glasscode, home) = (Forms::of(&root()), Forms::of(&home()));
    let classified = classify(&parts, &glasscode, &home);
    // Below its folder only, and never a folder: `.envs/` and `.envoy/` are not secrets.
    let name = parts.last().map(String::as_str).unwrap_or("");
    let secret = name.starts_with(".env")
        && !glasscode.is(&parts)
        && !home.is(&parts)
        && !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir());
    match classified {
        Classified::Outside | Classified::Class(Worktree | AgentNotes | Handoff | BrainWork | TrustedMemory | SessionTools) if secret => Classified::Class(Credentials),
        classified => classified,
    }
}

/// The class of a path's parts by the layout, without the `.env` rule. Pure.
fn classify(parts: &[String], glasscode: &Forms, home: &Forms) -> Classified {
    if let Some(rest) = glasscode.below(parts) {
        // Not even resolved by an ancestor: its canonical paths cannot be told, so deny.
        if glasscode.resolved.is_none() {
            return Classified::InsideUnlisted;
        }
        return longest_row(rest, Base::Glasscode).map_or(Classified::InsideUnlisted, |row| Classified::Class(row.class));
    }
    match home.below(parts).and_then(|rest| longest_row(rest, Base::Home)) {
        Some(row) => Classified::Class(row.class),
        None => Classified::Outside,
    }
}

/// The row of the layout that decides `path`: the longest that matches it. None for a path under
/// a Glasscode folder that cannot be resolved, as [`class_of`] denies it.
pub fn row_of(path: &Path) -> Option<&'static LayoutRow> {
    let parts = normal(path);
    let glasscode = Forms::of(&root());
    if let Some(rest) = glasscode.below(&parts) {
        return glasscode.resolved.as_ref().and_then(|_| longest_row(rest, Base::Glasscode));
    }
    Forms::of(&home()).below(&parts).and_then(|rest| longest_row(rest, Base::Home))
}

/// A folder as it is named, and with its links resolved.
struct Forms {
    written: Vec<String>,
    /// None when not even an ancestor of it could be resolved.
    resolved: Option<Vec<String>>,
}

impl Forms {
    fn of(folder: &Path) -> Forms {
        Forms { written: normal(folder), resolved: resolve(folder).map(|resolved| normal(&resolved)) }
    }

    /// The parts of a path below this folder, in either form.
    fn below<'a>(&self, parts: &'a [String]) -> Option<&'a [String]> {
        parts.strip_prefix(self.written.as_slice()).or_else(|| self.resolved.as_ref().and_then(|resolved| parts.strip_prefix(resolved.as_slice())))
    }

    /// Whether the parts are this folder itself.
    fn is(&self, parts: &[String]) -> bool {
        self.below(parts).is_some_and(<[String]>::is_empty)
    }
}

/// `folder` with its links resolved: by itself, or by its nearest ancestor that exists, with the
/// rest of the path after it. None when not even the top of it resolves.
fn resolve(folder: &Path) -> Option<PathBuf> {
    folder.ancestors().find_map(|ancestor| {
        let resolved = std::fs::canonicalize(ancestor).ok()?;
        Some(resolved.join(folder.strip_prefix(ancestor).ok()?))
    })
}

fn longest_row(rest: &[String], base: Base) -> Option<&'static LayoutRow> {
    LAYOUT.iter().filter(|row| row.base == base && matches_entry(rest, row.entry)).max_by_key(|row| row.entry.split('/').count())
}

fn matches_entry(rest: &[String], entry: &str) -> bool {
    let entry: Vec<String> = entry.split('/').map(str::to_lowercase).collect();
    let Some((last, folders)) = entry.split_last() else { return false };
    if rest.len() < entry.len() || rest[..folders.len()] != *folders {
        return false;
    }
    let name = &rest[folders.len()];
    let file = last.get(1..).is_some_and(|tail| tail.contains('.'));
    name == last || (file && name.strip_prefix(last.as_str()).is_some_and(|more| more.starts_with('.')))
}

/// Whether two paths name the same folder: their canonical forms (links resolved), with letter
/// case ignored on macOS and Windows, whose disks ignore it by default. A path that cannot be
/// resolved is compared as written.
pub fn same_folder(a: &Path, b: &Path) -> bool {
    folder_form(a) == folder_form(b)
}

/// A folder in the form [`same_folder`] compares, so that one folder is resolved once when it is
/// compared with many (`identity`): canonical (links resolved), and on macOS and Windows as
/// lower-case parts. A path that cannot be resolved is taken as written. It reads the disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderForm {
    /// macOS and Windows, whose disks ignore letter case by default.
    Folded(Vec<String>),
    Exact(PathBuf),
}

pub fn folder_form(path: &Path) -> FolderForm {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if cfg!(any(target_os = "macos", windows)) { FolderForm::Folded(normal(&resolved)) } else { FolderForm::Exact(resolved) }
}

/// The parts of an absolute path, in lower case, with `.` dropped, `..` taken back and the
/// prefix folded.
fn normal(path: &Path) -> Vec<String> {
    let mut parts = Vec::new();
    let mut floor = 0;
    for part in path.components() {
        match part {
            Component::Prefix(prefix) => {
                parts.push(prefix_text(prefix.kind()));
                floor = parts.len();
            }
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                if parts.len() > floor {
                    parts.pop();
                }
            }
            Component::Normal(name) => parts.push(name.to_string_lossy().to_lowercase()),
        }
    }
    parts
}

/// A Windows prefix in one form: `\\?\C:` is `c:`, and `\\?\UNC\server\share` is
/// `\\server\share`, as `canonicalize` gives the first of each.
fn prefix_text(prefix: std::path::Prefix<'_>) -> String {
    use std::path::Prefix;
    match prefix {
        Prefix::VerbatimDisk(disk) | Prefix::Disk(disk) => format!("{}:", (disk as char).to_ascii_lowercase()),
        Prefix::VerbatimUNC(server, share) | Prefix::UNC(server, share) => format!(r"\\{}\{}", server.to_string_lossy(), share.to_string_lossy()).to_lowercase(),
        Prefix::Verbatim(name) => name.to_string_lossy().to_lowercase(),
        Prefix::DeviceNS(name) => format!(r"\\.\{}", name.to_string_lossy()).to_lowercase(),
    }
}

pub fn ensure_dirs() -> std::io::Result<()> {
    for dir in [root(), accounts_dir(), worktrees_dir(), events_dir(), handoff_dir()] {
        std::fs::create_dir_all(&dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Account folders hold login tokens: keep them private.
        let _ = std::fs::set_permissions(accounts_dir(), std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    // `.glasscode` goes in the user's profile folder (C:\Users\<name>), the home folder of Windows.
    #[test]
    fn home_is_the_user_profile_folder() {
        let profile = std::env::var_os("USERPROFILE").expect("Windows sets USERPROFILE");
        let home = super::home();
        assert!(home.as_os_str().eq_ignore_ascii_case(&profile), "{} is not {}", home.display(), profile.display());
    }
}

#[cfg(test)]
mod test_root {
    use super::*;

    /// A test never reaches the person's `~/.glasscode`: without a folder of its own it gets the
    /// run's temporary one, which is one folder for the whole run.
    #[test]
    fn a_test_gets_a_temporary_glasscode_folder() {
        let first = use_test_root();
        assert_eq!(use_test_root(), first);
        assert!(first.is_dir() && first.starts_with(std::env::temp_dir()), "{}", first.display());
        assert_ne!(first, home().join(".glasscode"));
        if std::env::var_os("GLASSCODE_HOME").is_none() {
            assert_eq!((root(), accounts_dir()), (first.clone(), first.join("accounts")));
        }
    }
}


/// The tests' reading of the crates' own source: which paths they join to the Glasscode folder,
/// which functions of this file name a path, and which types implement `store::Store`. Each reader
/// is a pure function of a file's text; anything it cannot read for sure is an error, not a skip.
/// Its blind spot: the Glasscode folder held in a variable (`let root = paths::root();
/// root.join("x")`, glasscoded's `tmux.conf`), which the layout lists by hand.
#[cfg(test)]
pub(crate) mod source {
    use std::path::{Path, PathBuf};

    fn ident(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'_'
    }

    /// `text` with the insides of comments, strings and character literals made spaces (the
    /// quotes stay, so does every line break), so that braces and names are found only in code.
    pub fn mask(text: &str) -> String {
        let b = text.as_bytes();
        let mut out = b.to_vec();
        let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
            for byte in &mut out[from..to.min(b.len())] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
        };
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    let end = b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |n| i + n);
                    blank(&mut out, i, end);
                    i = end;
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    let (mut depth, mut j) = (1, i + 2);
                    while j < b.len() && depth > 0 {
                        if b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
                            depth += 1;
                            j += 2;
                        } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
                            depth -= 1;
                            j += 2;
                        } else {
                            j += 1;
                        }
                    }
                    blank(&mut out, i, j);
                    i = j;
                }
                b'r' if (i == 0 || !ident(b[i - 1]) || (b[i - 1] == b'b' && (i < 2 || !ident(b[i - 2])))) && matches!(b.get(i + 1), Some(b'#' | b'"')) => {
                    let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
                    let open = i + 1 + hashes;
                    if b.get(open) != Some(&b'"') {
                        i += 1;
                        continue;
                    }
                    let close: Vec<u8> = std::iter::once(b'"').chain(std::iter::repeat_n(b'#', hashes)).collect();
                    let end = b[open + 1..].windows(close.len()).position(|w| w == close.as_slice()).map_or(b.len(), |n| open + 1 + n);
                    blank(&mut out, open + 1, end);
                    i = end + close.len();
                }
                b'"' => {
                    let mut j = i + 1;
                    while j < b.len() && b[j] != b'"' {
                        j += if b[j] == b'\\' { 2 } else { 1 };
                    }
                    blank(&mut out, i + 1, j);
                    i = j + 1;
                }
                b'\'' => {
                    // A character literal, or a lifetime (which stays).
                    let end = if b.get(i + 1) == Some(&b'\\') {
                        b.get(i + 3..).and_then(|tail| tail.iter().position(|&c| c == b'\'')).map(|n| i + 3 + n)
                    } else {
                        let width = text[i + 1..].chars().next().map_or(1, char::len_utf8);
                        (b.get(i + 1 + width) == Some(&b'\'')).then_some(i + 1 + width)
                    };
                    match end {
                        Some(end) => {
                            blank(&mut out, i + 1, end);
                            i = end + 1;
                        }
                        None => i += 1,
                    }
                }
                _ => i += 1,
            }
        }
        String::from_utf8(out).expect("only whole characters were made spaces")
    }

    /// The index just past the brace that closes the one at `open`.
    fn closing(masked: &str, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (at, byte) in masked.bytes().enumerate().skip(open) {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(at + 1);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// A masked text without its test modules (`#[cfg(test)]` and `#[cfg(all(test, …))]`
    /// `mod … { … }`), which are made spaces.
    pub fn without_test_modules(masked: &str) -> String {
        let mut out = masked.as_bytes().to_vec();
        let mut from = 0;
        while let Some(found) = masked[from..].find("#[cfg(") {
            let at = from + found;
            from = at + 1;
            let Some(end) = masked[at..].find(']').map(|n| at + n + 1) else { break };
            let predicate: String = masked[at + 6..end - 2].chars().filter(|c| !c.is_whitespace()).collect();
            if predicate != "test" && !predicate.starts_with("all(test,") {
                continue;
            }
            // The item after it, past more attributes and a visibility.
            let mut rest = end;
            loop {
                let next = &masked[rest..];
                let skipped = next.len() - next.trim_start().len();
                rest += skipped;
                if masked[rest..].starts_with("#[") {
                    rest += masked[rest..].find(']').map_or(0, |n| n + 1);
                } else {
                    break;
                }
            }
            let item = masked[rest..].trim_start_matches("pub(crate)").trim_start_matches("pub").trim_start();
            if !item.starts_with("mod ") {
                continue;
            }
            let Some(open) = masked[rest..].find(['{', ';']).map(|n| rest + n) else { break };
            if masked.as_bytes()[open] == b';' {
                continue;
            }
            let Some(close) = closing(masked, open) else { break };
            for byte in &mut out[at..close] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
            from = close;
        }
        String::from_utf8(out).expect("only whole characters were made spaces")
    }

    /// Code ready to read: masked and without test modules.
    pub fn code(text: &str) -> String {
        without_test_modules(&mask(text))
    }

    fn line_of(text: &str, at: usize) -> usize {
        text[..at].matches('\n').count() + 1
    }

    /// The literal string that starts at `quote` (a `"` of the masked text), read from `text`.
    fn literal(text: &str, masked: &str, quote: usize) -> Option<String> {
        (masked.as_bytes().get(quote) == Some(&b'"')).then(|| {
            let end = masked[quote + 1..].find('"').map_or(text.len(), |n| quote + 1 + n);
            text[quote + 1..end].to_string()
        })
    }

    /// The first part of each name joined to the Glasscode folder: `paths::root().join("x")` in
    /// any file, and `root().join("x")` in paths.rs itself, over any whitespace. A join whose
    /// argument is not a literal is an error.
    pub fn root_joins(file: &str, text: &str) -> Vec<Result<String, String>> {
        let masked = code(text);
        let bare = file.ends_with("paths.rs");
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(n) = masked[from..].find("root()") {
            let at = from + n;
            from = at + 6;
            let before = &masked[..at];
            let named = before.ends_with("paths::");
            let plain = bare && !before.ends_with("fn ") && before.bytes().last().is_none_or(|c| !ident(c) && c != b':' && c != b'.');
            if !named && !plain {
                continue;
            }
            let after = masked[from..].trim_start();
            let Some(argument) = after.strip_prefix(".join(") else { continue };
            let quote = masked.len() - argument.trim_start().len();
            match literal(text, &masked, quote) {
                Some(name) => found.push(Ok(name.split('/').next().unwrap_or_default().to_string())),
                None => found.push(Err(format!("{file}:{}: a name joined to the Glasscode folder that is not a literal", line_of(text, at)))),
            }
        }
        found
    }

    /// The functions of a file (any visibility, any layout of the signature) whose return type
    /// names `PathBuf`.
    pub fn path_functions(text: &str) -> Vec<String> {
        let masked = code(text);
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(n) = masked[from..].find("fn ") {
            let at = from + n;
            from = at + 3;
            if at > 0 && ident(masked.as_bytes()[at - 1]) {
                continue;
            }
            let name: String = masked[at + 3..].trim_start().chars().take_while(|&c| c.is_alphanumeric() || c == '_').collect();
            let Some(end) = masked[at..].find(['{', ';']).map(|n| at + n) else { continue };
            let signature = &masked[at..end];
            if !name.is_empty() && signature.split_once("->").is_some_and(|(_, returns)| returns.contains("PathBuf")) {
                found.push(name);
            }
        }
        found
    }

    /// The names a file gives `Store` (`use crate::store::{Store as Base}`, over any whitespace),
    /// besides `Store`.
    pub fn store_aliases(text: &str) -> Vec<String> {
        let words = code(text).split_whitespace().collect::<Vec<_>>().join(" ");
        words.match_indices("Store as ").map(|(at, found)| words[at + found.len()..].chars().take_while(|&c| c.is_alphanumeric() || c == '_').collect()).collect()
    }

    /// The `NAME` of each `impl … Store for …` of a file (with any name of `Store` in `names`, a
    /// signature over several lines, and test modules left out), and of each impl whose body
    /// declares `ON_DAMAGE` whatever its trait is called (a macro's `impl $tr for $t`). A store
    /// whose `NAME` is not a literal (from a macro) is an error.
    pub fn store_impls(file: &str, text: &str, names: &[String]) -> Vec<Result<String, String>> {
        let masked = code(text);
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(n) = masked[from..].find("impl") {
            let at = from + n;
            from = at + 4;
            let next = masked.as_bytes().get(at + 4).copied().unwrap_or(b' ');
            if (at > 0 && ident(masked.as_bytes()[at - 1])) || !(next.is_ascii_whitespace() || next == b'<') {
                continue;
            }
            let Some(open) = masked[at..].find(['{', ';']).map(|n| at + n) else { continue };
            let header: String = masked[at + 4..open].split_whitespace().collect::<Vec<_>>().join(" ");
            // Past the impl's own generics.
            let mut depth = 0;
            let header = header.trim_start();
            let start = if header.starts_with('<') {
                header.char_indices().find_map(|(i, c)| {
                    match c {
                        '<' => depth += 1,
                        '>' => depth -= 1,
                        _ => {}
                    }
                    (depth == 0).then_some(i + 1)
                })
            } else {
                Some(0)
            };
            let header = header[start.unwrap_or(0)..].trim();
            let Some((trait_path, _)) = header.split_once(" for ") else { continue };
            let trait_name = trait_path.split('<').next().unwrap_or("").rsplit("::").next().unwrap_or("").trim();
            let body_end = closing(&masked, open).unwrap_or(masked.len());
            let body = &masked[open..body_end];
            if !names.iter().any(|name| name == trait_name) && !body.split_whitespace().collect::<Vec<_>>().join(" ").contains("const ON_DAMAGE") {
                continue;
            }
            let name = body.find("const NAME").and_then(|n| {
                let equals = body[n..].find('=')? + n + 1;
                let quote = open + equals + (body[equals..].len() - body[equals..].trim_start().len());
                literal(text, &masked, quote)
            });
            match name {
                Some(name) => found.push(Ok(name)),
                None => found.push(Err(format!("{file}:{}: an impl of Store without a literal NAME", line_of(text, at)))),
            }
            from = body_end;
        }
        found
    }

    /// Every `.rs` file of every crate's `src/` (tests/ folders are tests), with its text.
    pub fn crate_sources() -> Vec<(PathBuf, String)> {
        fn walk(dir: &Path, files: &mut Vec<(PathBuf, String)>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, files);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    files.push((path.clone(), std::fs::read_to_string(&path).unwrap()));
                }
            }
        }
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("the crates folder").to_path_buf();
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&crates).unwrap().flatten() {
            walk(&entry.path().join("src"), &mut files);
        }
        assert!(files.len() > 100, "the crates' sources were not found under {}", crates.display());
        files
    }
}

#[cfg(test)]
mod layout {
    use std::collections::BTreeSet;

    use super::*;

    /// An environment variable set for one test, and as it was again at its end.
    struct SetVar(&'static str, Option<std::ffi::OsString>);

    impl SetVar {
        fn new(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> SetVar {
            let before = std::env::var_os(name);
            // SAFETY: the core tests run one at a time (`--test-threads=1`).
            unsafe { std::env::set_var(name, value) };
            SetVar(name, before)
        }
    }

    impl Drop for SetVar {
        fn drop(&mut self) {
            // SAFETY: as in `new`.
            match self.1.take() {
                Some(value) => unsafe { std::env::set_var(self.0, value) },
                None => unsafe { std::env::remove_var(self.0) },
            }
        }
    }

    /// Each folder (and file) function of this module has a class (design section 5). A new
    /// function that returns a path, of any visibility and signature, fails here until it is
    /// listed below and `class_of` knows it.
    #[test]
    fn every_folder_has_a_class() {
        let key = ProjectKey::parse("glasscode-3f9a1c").unwrap();
        let machine = MachineId::parse("0123456789abcdef").unwrap();
        let listed = [
            ("state_file", state_file(), Authority),
            ("accounts_dir", accounts_dir(), Accounts),
            ("worktrees_dir", worktrees_dir(), Worktree),
            ("events_dir", events_dir(), HookIo),
            ("handoff_dir", handoff_dir(), Handoff),
            ("settings_dir", settings_dir(), HookIo),
            ("server_file", server_file(), Authority),
            ("daemon_socket", daemon_socket(), Authority),
            ("daemon_file", daemon_file(), Authority),
            ("owners_file", owners_file(), Authority),
            ("unsaved_dir", unsaved_dir(), Cache),
            ("cli_versions_file", cli_versions_file(), Authority),
            ("clis_file", clis_file(), Authority),
            ("authority_dir", authority_dir(), Authority),
            ("memory_dir", memory_dir(), TrustedMemory),
            ("memory_server_dir", memory_server_dir(), TrustedMemory),
            ("memory_server_repo", memory_server_repo(), TrustedMemory),
            ("agent_notes_dir", agent_notes_dir(&key, AgentKind::Claude), AgentNotes),
            ("words_dir", words_dir(&machine), Words),
            ("journal_dir", journal_dir(), Ledger),
            ("effects_dir", effects_dir(), Ledger),
            ("manager_work_dir", manager_work_dir(), BrainWork),
            ("manager_run_dir", manager_run_dir(), Ledger),
            ("cache_dir", cache_dir(), Cache),
        ];
        // The Glasscode and home folders themselves, the test root, a row's own path, and a
        // folder's resolved form: none names a folder of its own.
        let exempt = ["home", "root", "use_test_root", "path", "resolve"];
        let declared: BTreeSet<String> = source::path_functions(include_str!("paths.rs")).into_iter().filter(|name| !exempt.contains(&name.as_str())).collect();
        let names: BTreeSet<String> = listed.iter().map(|(name, ..)| name.to_string()).collect();
        assert_eq!(declared, names, "a path function of paths.rs without a class, or a listed one that is gone");
        for (name, path, class) in &listed {
            assert!(path.starts_with(root()), "{name}: {}", path.display());
            assert_eq!(class_of(path), Classified::Class(*class), "{name}: {}", path.display());
            assert_eq!(class_of(&path.join("inside")), Classified::Class(*class), "{name}: what is in it");
        }
    }

    /// Every name any crate joins to the Glasscode folder has a row of the layout (review of PR
    /// 78: `control.key`, `bin/`, `pasted/` and others had none).
    #[test]
    fn every_name_joined_to_the_glasscode_folder_is_in_the_layout() {
        let entries: BTreeSet<String> = layout().iter().filter(|row| row.base == Base::Glasscode).map(|row| row.entry.split('/').next().unwrap().to_lowercase()).collect();
        let mut seen = 0;
        let mut missing = Vec::new();
        for (file, text) in source::crate_sources() {
            for join in source::root_joins(&file.display().to_string(), &text) {
                seen += 1;
                match join {
                    Ok(name) if entries.contains(&name.to_lowercase()) => {}
                    Ok(name) => missing.push(format!("{}: {name}", file.display())),
                    Err(err) => missing.push(err),
                }
            }
        }
        assert!(seen > 30, "only {seen} joins were found: the reader is broken");
        assert!(missing.is_empty(), "names joined to the Glasscode folder without a row of the layout:\n{}", missing.join("\n"));
    }

    #[test]
    fn the_source_readers_find_what_is_hidden() {
        // Path functions of any visibility, over several lines, returning any form of a path.
        let functions = source::path_functions(
            "pub fn a_dir() -> PathBuf { root() }\npub(crate) fn b_dir() -> PathBuf { root() }\nfn c_file(\n    x: u8,\n) -> std::path::PathBuf {\n    root()\n}\npub fn d_dir() -> Option<PathBuf> { None }\npub fn e(x: &Path) -> bool { true }\n// pub fn f_dir() -> PathBuf\n#[cfg(test)]\nmod tests {\n    fn g_dir() -> PathBuf { root() }\n}\n",
        );
        assert_eq!(functions, ["a_dir", "b_dir", "c_file", "d_dir"]);
        // Joins: named, over lines, inside paths.rs bare, and not a literal.
        let joins = source::root_joins("other.rs", "let a = crate::paths::root().join(\"alpha/x\");\nlet b = paths::root()\n    .join(\n        \"beta\");\nlet c = paths::root().join(name);\nlet d = root().join(\"gamma\");\nlet e = \"paths::root().join(\\\"delta\\\")\";\n");
        assert_eq!(joins[..2], [Ok("alpha".to_string()), Ok("beta".to_string())]);
        assert!(joins[2].is_err() && joins.len() == 3, "{joins:?}");
        assert_eq!(source::root_joins("src/paths.rs", "pub fn root() -> PathBuf { x }\nfn a() { root().join(\"gamma\") }\nfn b() { use_test_root().join(\"no\") }"), [Ok("gamma".to_string())]);
        // Comments, strings with braces, character literals and raw strings do not confuse it.
        let masked = source::mask("a /* { */ b // }\n\"{\" '{' r#\"}\"# 'a: { }");
        assert_eq!(masked.matches('{').count(), 1, "{masked}");
        assert_eq!(masked.len(), "a /* { */ b // }\n\"{\" '{' r#\"}\"# 'a: { }".len());
    }

    /// The four stores the review hid from the first reader: a renamed `Store`, a signature over
    /// several lines, a macro and a `test/` name. The first, second and fourth count; the macro's
    /// `NAME` is not a literal, so it is an error.
    #[test]
    fn the_store_reader_finds_renamed_split_and_macro_stores() {
        let fixture = r#"
use crate::store::{OnDamage, Store as Base};
pub struct Hidden1;
impl Base for Hidden1 {
    const NAME: &'static str = "authority/hidden1";
}
impl crate::store::Store
    for Hidden2
{
    const NAME: &'static str = "authority/hidden2";
}
macro_rules! store {
    ($t:ident, $name:literal) => {
        impl crate::store::Store for $t {
            const NAME: &'static str = $name;
        }
    };
}
impl crate::store::Store for Hidden4 {
    const NAME: &'static str = "test/../authority/hidden4";
}
impl<T: Store> Wrapper<T> {}
impl std::fmt::Display for Hidden1 {}
use crate::store::{Store
    as
  Renamed};
impl Renamed for HiddenR {
    const NAME: &'static str = "authority/renamed";
}
macro_rules! by_trait {
    ($tr:path, $t:ident) => {
        impl $tr for $t {
            const NAME: &'static str = $name;
            const ON_DAMAGE: OnDamage = OnDamage::Empty;
        }
    };
}
#[cfg(test)]
mod tests {
    impl Store for OnlyInTests {
        const NAME: &'static str = "test/notes";
    }
}
"#;
        let mut names = vec!["Store".to_string()];
        names.extend(source::store_aliases(fixture));
        // A name given over lines, and a macro that takes the trait as a parameter (its body
        // declares ON_DAMAGE, so it is a store) are found too (review of PR 78, second round).
        assert_eq!(names, ["Store", "Base", "Renamed"]);
        let found = source::store_impls("review_stores.rs", fixture, &names);
        let ok: Vec<&String> = found.iter().filter_map(|f| f.as_ref().ok()).collect();
        assert_eq!(ok, ["authority/hidden1", "authority/hidden2", "test/../authority/hidden4", "authority/renamed"]);
        let errors: Vec<&String> = found.iter().filter_map(|f| f.as_ref().err()).collect();
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("review_stores.rs:14") && errors[0].contains("literal NAME"), "{errors:?}");
        assert!(errors[1].contains("review_stores.rs:32"), "{errors:?}");
    }

    #[test]
    fn class_of_reads_the_layout_of_design_section_5() {
        let root = root();
        let home = home();
        let class = Classified::Class;
        let cases = [
            // The Glasscode folder: by its entries, whole names only; in no row, denied.
            (root.clone(), Classified::InsideUnlisted),
            (root.join("brand-new.json"), Classified::InsideUnlisted),
            (root.join("worktrees-old").join("x"), Classified::InsideUnlisted),
            (root.join("board.json"), class(Authority)),
            (root.join("control.key"), class(Authority)),
            (root.join("device").join("id_ed25519"), class(Authority)),
            (root.join("state.json.broken"), class(Authority)),
            (root.join("state.json.tmp"), class(Authority)),
            (root.join("state.jsonx"), Classified::InsideUnlisted),
            (root.join("authority").join("policy.json"), class(Authority)),
            (root.join("authority").join("engine.json.lock"), class(Authority)),
            (root.join("agent-log.jsonl"), class(Ledger)),
            // The longest entry decides: the brain's folder, and the manager's other files.
            (root.join("manager").join("work").join("notes.md"), class(BrainWork)),
            (root.join("manager").join("work.json"), class(Ledger)),
            (root.join("manager").join("data.json"), class(Ledger)),
            (root.join("handoff").join("a1b2c3d4-1.md"), class(Handoff)),
            (root.join("bin").join("glasscode"), class(SessionTools)),
            (root.join("agent").join("claude-plugin").join("skills").join("SKILL.md"), class(SessionTools)),
            (root.join("pasted").join("image.png"), class(SessionTools)),
            (root.join("control.sock"), class(SessionTools)),
            (root.join("github-cache.json"), class(Cache)),
            (root.join("mirrors").join("s1").join("mirror.json"), class(Cache)),
            (root.join("accounts").join("a1").join(".claude.json"), class(Accounts)),
            // Letter case, `.` and `..` do not get around it.
            (root.join("Authority").join("policy.json"), class(Authority)),
            (root.join("worktrees").join("..").join("authority").join("engine.json"), class(Authority)),
            (root.join(".").join("memory").join("acme"), class(TrustedMemory)),
            // `.env` and `.env.*` files where sessions may read; a denied class stays as it is.
            (root.join("worktrees").join("app").join("main").join(".env"), class(Credentials)),
            (root.join("worktrees").join("app").join("main").join(".env.local"), class(Credentials)),
            (root.join("memory").join("acme").join(".env"), class(Credentials)),
            (root.join("authority").join(".env"), class(Authority)),
            (root.join("worktrees").join("app").join("main").join("src").join("lib.rs"), class(Worktree)),
            // Folders named like it, and names that only end like it, are not (review of PR 78, E7).
            (root.join("worktrees").join("app").join("main").join(".envoy").join("config.yaml"), class(Worktree)),
            (root.join("worktrees").join("app").join("main").join("config").join("prod.env"), class(Worktree)),
            (home.join("Projects").join(".envs").join("app").join("main.rs"), Classified::Outside),
            (home.join("Projects").join("app").join(".env").join("bin").join("python"), Classified::Outside),
            (home.join("Projects").join("app").join(".env"), class(Credentials)),
            (home.join("Projects").join("app").join("README.md"), Classified::Outside),
            // The home folder's rows.
            (home.join(".claude").join("projects"), class(Accounts)),
            (home.join(".claude.json"), class(Accounts)),
            (home.join(".codex").join("auth.json"), class(Accounts)),
            (home.join(".ssh").join("id_ed25519"), class(Credentials)),
            (home.join(".aws").join("credentials"), class(Credentials)),
            (home.join(".config").join("gh").join("hosts.yml"), class(Credentials)),
            (home.join(".config").join("other"), Classified::Outside),
            (home.join(".netrc"), class(Credentials)),
            (home.join(".netrc.gpg"), class(Credentials)),
            (home.join(".authinfo.gpg"), class(Credentials)),
            (home.join(".git-credentials"), class(Credentials)),
            (home.join("Library").join("Keychains").join("login.keychain-db"), class(Credentials)),
            // A relative path is not known.
            (PathBuf::from("worktrees").join("x"), Classified::Outside),
            (PathBuf::from(".ssh"), Classified::Outside),
        ];
        for (path, expected) in &cases {
            assert_eq!(class_of(path), *expected, "{}", path.display());
        }
        // Every row's own path has its class, and `row_of` names the row.
        for row in layout() {
            assert_eq!(class_of(&row.path()), Classified::Class(row.class), "{}", row.entry);
            assert_eq!(row_of(&row.path().join("x")), Some(row), "{}", row.entry);
        }
    }

    /// A Glasscode folder reached through a link: its canonical paths have their class (review of
    /// PR 78, E6).
    #[cfg(unix)]
    #[test]
    fn a_canonical_path_has_its_class() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let _root = SetVar::new("GLASSCODE_HOME", &link);
        assert_eq!(root(), link);
        let canonical = std::fs::canonicalize(&real).unwrap();
        for path in [link.join("authority").join("policy.json"), canonical.join("authority").join("policy.json")] {
            assert_eq!(class_of(&path), Classified::Class(Authority), "{}", path.display());
        }
        assert_eq!(class_of(&canonical.join("unlisted")), Classified::InsideUnlisted);
        assert!(same_folder(&link, &real) && !same_folder(&link, dir.path()));
        if cfg!(target_os = "macos") {
            assert!(same_folder(&PathBuf::from(real.display().to_string().to_uppercase()), &link), "letter case on macOS");
        }
    }

    /// A Glasscode folder that does not exist yet, reached through a link: its canonical paths
    /// have their class too, by its nearest folder that exists (review of PR 78, second round).
    #[cfg(unix)]
    #[test]
    fn a_canonical_path_has_its_class_before_the_folder_exists() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let _root = SetVar::new("GLASSCODE_HOME", link.join("not-yet"));
        assert!(!root().exists());
        let canonical = std::fs::canonicalize(&real).unwrap().join("not-yet");
        assert_eq!(class_of(&canonical.join("authority").join("policy.json")), Classified::Class(Authority));
        assert_eq!(class_of(&canonical.join("unlisted")), Classified::InsideUnlisted);
        assert_eq!(class_of(&link.join("not-yet").join("worktrees").join("a")), Classified::Class(Worktree));
    }

    /// When no form of the Glasscode folder can be resolved, what is under it as written is
    /// denied, whatever its row.
    #[test]
    fn an_unresolved_glasscode_folder_denies_what_is_under_it() {
        let written = Forms { written: normal(Path::new("/nowhere/home")), resolved: None };
        let home = Forms { written: normal(Path::new("/users/x")), resolved: None };
        let path = normal(Path::new("/nowhere/home/worktrees/app/main/src/lib.rs"));
        assert_eq!(classify(&path, &written, &home), Classified::InsideUnlisted);
        let resolved = Forms { resolved: Some(normal(Path::new("/real/home"))), ..written };
        assert_eq!(classify(&path, &resolved, &home), Classified::Class(Worktree));
        assert_eq!(classify(&normal(Path::new("/real/home/worktrees/a")), &resolved, &home), Classified::Class(Worktree));
    }

    /// Every file whose name starts with `.env` is a credential; a folder never is (review of PR
    /// 78, second round).
    #[test]
    fn env_files_are_credentials_and_env_folders_are_not() {
        let worktree = root().join("worktrees").join("env-test").join("main");
        let _ = std::fs::remove_dir_all(&worktree);
        for folder in [".envs", ".envoy", ".env.d"] {
            std::fs::create_dir_all(worktree.join(folder)).unwrap();
        }
        for file in [".env", ".env.local", ".env-prod", ".env_local", ".envrc"] {
            std::fs::write(worktree.join(file), "SECRET=1").unwrap();
            assert_eq!(class_of(&worktree.join(file)), Classified::Class(Credentials), "{file}");
        }
        // Not there yet: counted as a file.
        assert_eq!(class_of(&worktree.join(".env.new")), Classified::Class(Credentials));
        for folder in [".envs", ".envoy", ".env.d"] {
            assert_eq!(class_of(&worktree.join(folder)), Classified::Class(Worktree), "{folder}");
            assert_eq!(class_of(&worktree.join(folder).join("config.yaml")), Classified::Class(Worktree), "{folder}/config.yaml");
        }
        for file in ["prod.env", "secrets.env.local", "my.envrc"] {
            assert_eq!(class_of(&worktree.join(file)), Classified::Class(Worktree), "{file}");
        }
        std::fs::remove_dir_all(&worktree).unwrap();
    }

    #[test]
    fn windows_prefixes_fold_to_one_form() {
        use std::ffi::OsStr;
        use std::path::Prefix;
        assert_eq!(prefix_text(Prefix::VerbatimDisk(b'C')), prefix_text(Prefix::Disk(b'c')));
        assert_eq!(prefix_text(Prefix::Disk(b'C')), "c:");
        assert_eq!(prefix_text(Prefix::VerbatimUNC(OsStr::new("Server"), OsStr::new("Share"))), prefix_text(Prefix::UNC(OsStr::new("server"), OsStr::new("share"))));
    }

    #[test]
    fn keys_and_machine_ids_are_safe_folder_names() {
        for good in ["glasscode-3f9a1c", "next.js-0a1b2c", "my_app-ffffff", "0123456789abcdef", "console", "com10", "lpt", "auxiliary-1"] {
            assert_eq!(ProjectKey::parse(good).map(|k| k.to_string()), Some(good.to_string()), "{good}");
            assert_eq!(MachineId::parse(good).map(|m| m.to_string()), Some(good.to_string()), "{good}");
        }
        let long = "a".repeat(65);
        let bad = ["", ".", "..", "../x", "a/b", r"a\b", ".hidden", "-x", "Upper", "a b", "ü", long.as_str()];
        // Windows' device names, with or without an extension, and a trailing dot (review of PR 78).
        let windows = ["con", "prn", "aux", "nul", "com1", "com9", "lpt1", "lpt9", "con.txt", "nul.json", "com1.tar.gz", "a.", "a.."];
        for bad in bad.into_iter().chain(windows) {
            assert!(ProjectKey::parse(bad).is_none() && MachineId::parse(bad).is_none(), "{bad:?}");
        }
        // Read from a file or another machine, the same rule holds.
        assert!(serde_json::from_str::<MachineId>(r#""../../.ssh""#).is_err());
        assert!(serde_json::from_str::<ProjectKey>(r#""con""#).is_err());
        assert_eq!(serde_json::from_str::<ProjectKey>(r#""acme-012345""#).unwrap().as_str(), "acme-012345");
        assert_eq!(serde_json::to_string(&MachineId::parse("abc").unwrap()).unwrap(), r#""abc""#);
        let key = ProjectKey::parse("acme-012345").unwrap();
        assert_eq!(agent_notes_dir(&key, AgentKind::Codex), root().join("agent-notes").join("acme-012345").join("codex"));
    }
}
