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

/// What the app keeps only to be quick.
pub fn cache_dir() -> PathBuf {
    root().join("cache")
}

/// A name that is one safe folder name on every system: 1 to 64 of `a-z`, `0-9`, `.`, `_` and
/// `-`, starting with a letter or a digit (so never `.`, `..` or a hidden name).
fn folder_name(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text.as_bytes()[0].is_ascii_alphanumeric()
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// A project as memory and the agent notes know it (D22): `glasscode-3f9a1c`, a slug and six hex
/// digits. Here it is only a name that is safe as a folder (it comes from synced data too); the
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
/// machines send it, so only a safe folder name is one.
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
/// `headless_profile()` build their deny and allow rules from these classes only, never from a
/// literal path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathClass {
    /// `authority/`, `state.json` and the app's other settings: the app (and glasscoded) only.
    Authority,
    /// The words ledger, `words/<machine>/`.
    Words,
    /// The journal and the effect ledger.
    Ledger,
    /// `memory/`, which briefs come from.
    TrustedMemory,
    /// `agent-notes/<key>/<agent>/`: that project's sessions read and write it.
    AgentNotes,
    /// `worktrees/<project>/<branch>/`.
    Worktree,
    /// The agents' logins: `accounts/<id>/`, `~/.claude`, `~/.codex`.
    Accounts,
    /// What hooks write, and the settings files they are given; hooks run outside the sandbox.
    HookIo,
    /// `manager/work/`, the brain's work folder.
    BrainWork,
    /// What the app keeps to be quick: the GitHub and usage caches, the browser's profile.
    Cache,
    /// The person's credentials, which are not Glasscode's: SSH, AWS, `gh`, `.netrc`, keychains,
    /// and `.env` files anywhere.
    Credentials,
}

/// The entries of the Glasscode folder and their classes. A folder entry is its whole tree. An
/// entry with an extension is a file, and covers its siblings with one more suffix too
/// (`state.json.broken`, `state.json.tmp`).
const LAYOUT: &[(&str, PathClass)] = &[
    ("authority", PathClass::Authority),
    ("state.json", PathClass::Authority),
    ("server.json", PathClass::Authority),
    ("glasscoded.json", PathClass::Authority),
    ("glasscoded.sock", PathClass::Authority),
    ("cli-versions.json", PathClass::Authority),
    ("clis.json", PathClass::Authority),
    ("words", PathClass::Words),
    ("journal", PathClass::Ledger),
    ("effects", PathClass::Ledger),
    ("memory", PathClass::TrustedMemory),
    ("agent-notes", PathClass::AgentNotes),
    // Handoff notes: the next session's agent reads the note it is pointed at, so not a denied
    // class. Not a row of design section 5 yet.
    ("handoff", PathClass::AgentNotes),
    ("worktrees", PathClass::Worktree),
    ("accounts", PathClass::Accounts),
    ("events", PathClass::HookIo),
    ("settings", PathClass::HookIo),
    ("manager/work", PathClass::BrainWork),
    ("cache", PathClass::Cache),
    ("github-cache.json", PathClass::Cache),
    ("usage-cache.json", PathClass::Cache),
    ("browser", PathClass::Cache),
    // Copies of editor text that could not be saved: the app's, for the person. Not a row of
    // design section 5 yet.
    ("unsaved", PathClass::Cache),
];

/// The entries of the home folder that design section 5 names, as [`LAYOUT`].
const HOME_LAYOUT: &[(&str, PathClass)] = &[
    (".claude", PathClass::Accounts),
    (".claude.json", PathClass::Accounts),
    (".codex", PathClass::Accounts),
    (".ssh", PathClass::Credentials),
    (".aws", PathClass::Credentials),
    (".config/gh", PathClass::Credentials),
    (".netrc", PathClass::Credentials),
    (".gnupg", PathClass::Credentials),
    // Keychain files: macOS, the Linux keyring, and Windows' credential and key folders.
    ("Library/Keychains", PathClass::Credentials),
    (".local/share/keyrings", PathClass::Credentials),
    ("AppData/Roaming/Microsoft/Credentials", PathClass::Credentials),
    ("AppData/Local/Microsoft/Credentials", PathClass::Credentials),
    ("AppData/Roaming/Microsoft/Protect", PathClass::Credentials),
];

/// The class of `path` (design section 5), or None for a path the layout does not name: a
/// relative path, the Glasscode folder itself, or one of its entries without a row. Pure: the
/// path is read as written (`..` taken back, letter case ignored), not looked up on disk, so a
/// caller gives a canonical path (links resolved: `/var` is `/private/var` on macOS).
pub fn class_of(path: &Path) -> Option<PathClass> {
    if !path.is_absolute() {
        return None;
    }
    let parts = normal(path);
    let class = under(&parts, &normal(&root()))
        .and_then(|rest| lookup(rest, LAYOUT))
        .or_else(|| under(&parts, &normal(&home())).and_then(|rest| lookup(rest, HOME_LAYOUT)));
    // `**/.env*`: a secret file in a folder sessions may otherwise read.
    let secret = parts.iter().any(|part| part.starts_with(".env"));
    match class {
        None | Some(PathClass::Worktree | PathClass::AgentNotes | PathClass::BrainWork | PathClass::TrustedMemory) if secret => Some(PathClass::Credentials),
        class => class,
    }
}

/// The parts of an absolute path, in lower case, with `.` dropped and `..` taken back.
fn normal(path: &Path) -> Vec<String> {
    let mut parts = Vec::new();
    let mut floor = 0;
    for part in path.components() {
        match part {
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().to_lowercase());
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

/// The rest of `parts` below `base`, when it is below it.
fn under<'a>(parts: &'a [String], base: &[String]) -> Option<&'a [String]> {
    parts.strip_prefix(base)
}

fn lookup(rest: &[String], layout: &[(&str, PathClass)]) -> Option<PathClass> {
    layout.iter().find(|(entry, _)| matches_entry(rest, entry)).map(|(_, class)| *class)
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

#[cfg(test)]
mod layout {
    use std::collections::BTreeSet;

    use super::*;

    /// The name of a path function of this file, from one line of it: `pub fn name(…) -> PathBuf`
    /// at the start of the line, other than the Glasscode and home folders themselves.
    fn path_function(line: &str) -> Option<&str> {
        let rest = line.strip_prefix("pub fn ")?;
        let name = &rest[..rest.find(['(', '<'])?];
        (line.contains("-> PathBuf") && !matches!(name, "home" | "root" | "use_test_root")).then_some(name)
    }

    /// Each folder (and file) of this module has a class (design section 5). A new `pub fn …_dir`
    /// fails here until it is listed below and `class_of` knows it.
    #[test]
    fn every_folder_has_a_class() {
        use PathClass::*;
        let key = ProjectKey::parse("glasscode-3f9a1c").unwrap();
        let machine = MachineId::parse("0123456789abcdef").unwrap();
        let layout = [
            ("state_file", state_file(), Authority),
            ("accounts_dir", accounts_dir(), Accounts),
            ("worktrees_dir", worktrees_dir(), Worktree),
            ("events_dir", events_dir(), HookIo),
            ("handoff_dir", handoff_dir(), AgentNotes),
            ("settings_dir", settings_dir(), HookIo),
            ("server_file", server_file(), Authority),
            ("daemon_socket", daemon_socket(), Authority),
            ("daemon_file", daemon_file(), Authority),
            ("unsaved_dir", unsaved_dir(), Cache),
            ("cli_versions_file", cli_versions_file(), Authority),
            ("clis_file", clis_file(), Authority),
            ("authority_dir", authority_dir(), Authority),
            ("memory_dir", memory_dir(), TrustedMemory),
            ("agent_notes_dir", agent_notes_dir(&key, AgentKind::Claude), AgentNotes),
            ("words_dir", words_dir(&machine), Words),
            ("journal_dir", journal_dir(), Ledger),
            ("effects_dir", effects_dir(), Ledger),
            ("manager_work_dir", manager_work_dir(), BrainWork),
            ("cache_dir", cache_dir(), Cache),
        ];
        let declared: BTreeSet<&str> = include_str!("paths.rs").lines().filter_map(path_function).collect();
        let listed: BTreeSet<&str> = layout.iter().map(|(name, ..)| *name).collect();
        assert_eq!(declared, listed, "a path function of paths.rs without a class, or a listed one that is gone");
        for (name, path, class) in &layout {
            assert!(path.starts_with(root()), "{name}: {}", path.display());
            assert_eq!(class_of(path), Some(*class), "{name}: {}", path.display());
            assert_eq!(class_of(&path.join("inside")), Some(*class), "{name}: what is in it");
        }
    }

    #[test]
    fn class_of_reads_the_layout_of_design_section_5() {
        use PathClass::*;
        let root = root();
        let home = home();
        let cases = [
            // The Glasscode folder: by its entries, whole names only.
            (root.clone(), None),
            (root.join("board.json"), None),
            (root.join("worktrees-old").join("x"), None),
            (root.join("manager").join("data.json"), None),
            (root.join("manager").join("work").join("notes.md"), Some(BrainWork)),
            (root.join("state.json.broken"), Some(Authority)),
            (root.join("state.json.tmp"), Some(Authority)),
            (root.join("state.jsonx"), None),
            (root.join("authority").join("policy.json"), Some(Authority)),
            (root.join("github-cache.json"), Some(Cache)),
            (root.join("browser").join("profile"), Some(Cache)),
            (root.join("accounts").join("a1").join(".claude.json"), Some(Accounts)),
            // Letter case, `.` and `..` do not get around it.
            (root.join("Authority").join("policy.json"), Some(Authority)),
            (root.join("worktrees").join("..").join("authority").join("engine.json"), Some(Authority)),
            (root.join(".").join("memory").join("acme"), Some(TrustedMemory)),
            // `.env*` anywhere a session may read is a credential; a denied class stays as it is.
            (root.join("worktrees").join("app").join("main").join(".env.local"), Some(Credentials)),
            (root.join("worktrees").join("app").join("main").join("src").join("lib.rs"), Some(Worktree)),
            (root.join("memory").join("acme").join(".env"), Some(Credentials)),
            (root.join("authority").join(".env"), Some(Authority)),
            // The home folder's rows.
            (home.join(".claude").join("projects"), Some(Accounts)),
            (home.join(".claude.json"), Some(Accounts)),
            (home.join(".codex").join("auth.json"), Some(Accounts)),
            (home.join(".ssh").join("id_ed25519"), Some(Credentials)),
            (home.join(".aws").join("credentials"), Some(Credentials)),
            (home.join(".config").join("gh").join("hosts.yml"), Some(Credentials)),
            (home.join(".config").join("other"), None),
            (home.join(".netrc"), Some(Credentials)),
            (home.join("Library").join("Keychains").join("login.keychain-db"), Some(Credentials)),
            (home.join("Projects").join("app").join(".envrc"), Some(Credentials)),
            (home.join("Projects").join("app").join("README.md"), None),
            // Nothing for a relative path: it is not known what it is relative to.
            (PathBuf::from("worktrees").join("x"), None),
            (PathBuf::from(".ssh"), None),
        ];
        for (path, class) in &cases {
            assert_eq!(class_of(path), *class, "{}", path.display());
        }
    }

    #[test]
    fn keys_and_machine_ids_are_safe_folder_names() {
        for good in ["glasscode-3f9a1c", "next.js-0a1b2c", "my_app-ffffff", "0123456789abcdef"] {
            assert_eq!(ProjectKey::parse(good).map(|k| k.to_string()), Some(good.to_string()));
            assert_eq!(MachineId::parse(good).map(|m| m.to_string()), Some(good.to_string()));
        }
        let long = "a".repeat(65);
        for bad in ["", ".", "..", "../x", "a/b", r"a\b", ".hidden", "-x", "Upper", "a b", "ü", long.as_str()] {
            assert!(ProjectKey::parse(bad).is_none() && MachineId::parse(bad).is_none(), "{bad:?}");
        }
        // Read from a file or another machine, the same rule holds.
        assert!(serde_json::from_str::<MachineId>(r#""../../.ssh""#).is_err());
        assert_eq!(serde_json::from_str::<ProjectKey>(r#""acme-012345""#).unwrap().as_str(), "acme-012345");
        assert_eq!(serde_json::to_string(&MachineId::parse("abc").unwrap()).unwrap(), r#""abc""#);
        let key = ProjectKey::parse("acme-012345").unwrap();
        assert_eq!(agent_notes_dir(&key, AgentKind::Codex), root().join("agent-notes").join("acme-012345").join("codex"));
    }
}
