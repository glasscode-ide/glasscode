//! The person's agent history from before Glasscode counted: how many Claude Code and Codex
//! conversations they started on each day, for the heat map of Today.
//!
//! `activity.json` counts from the first run of the build that has it, so a new install shows an
//! empty map. The CLIs keep one file per conversation, and those files say when each began:
//!
//! - Claude Code: `<config>/projects/<folder>/<id>.jsonl`; the lines carry a `timestamp`.
//! - Codex: `<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-<date>T<time>-<id>.jsonl`.
//!
//! Only a file's name, its times and the head of the file are read (the day it began, and
//! whether a person started it: the rules of `discover.rs` and `history.rs`). No text of a
//! conversation is kept, sent or written to the log. The scan runs in a background task, is
//! bounded to the days the map shows, and its result is kept in `agent-history.json` so the map
//! is there at once on the next start.
//!
//! The counts are merged into what Today shows only for the days before `activity.json` starts
//! (`Activity::with_history`); the activity file itself is never changed by them.

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::day::{self, Day};

/// How much of the start of a Claude conversation is read for its first time.
const CLAUDE_HEAD: u64 = 64 * 1024;
/// How much of a Codex conversation's first line (`session_meta`) is read.
const CODEX_HEAD: u64 = 1024 * 1024;
/// No more files than this are opened in one scan (the newest folders first).
const MOST_FILES: usize = 20_000;

/// Where the conversations are: each `projects` folder of a Claude config folder, and each
/// `sessions` folder of a Codex one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roots {
    pub claude: Vec<PathBuf>,
    pub codex: Vec<PathBuf>,
}

impl Roots {
    /// The CLIs' own folders in `home` (none: they are left alone, as in a test or a scratch
    /// Glasscode folder), and the folders of the accounts Glasscode keeps in `accounts`.
    pub fn of(home: Option<&Path>, accounts: &Path) -> Roots {
        let mut roots = Roots::default();
        if let Some(home) = home {
            roots.claude.push(home.join(".claude").join("projects"));
            roots.codex.push(home.join(".codex").join("sessions"));
        }
        let mut folders: Vec<PathBuf> = std::fs::read_dir(accounts).into_iter().flatten().flatten().map(|entry| entry.path()).collect();
        folders.sort();
        for folder in folders {
            roots.claude.push(folder.join("projects"));
            roots.codex.push(folder.join("sessions"));
        }
        roots
    }
}

/// The conversations started on each local day (`2026-11-16`), from `first` on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    /// When it was scanned (Unix time); 0: never.
    #[serde(default)]
    pub scanned: u64,
    #[serde(default)]
    pub days: BTreeMap<String, u32>,
}

pub fn file() -> PathBuf {
    crate::paths::root().join("agent-history.json")
}

impl History {
    /// The last scan's result; none when there was no scan or its file cannot be read.
    pub fn load() -> Option<History> {
        serde_json::from_slice(&std::fs::read(file()).ok()?).ok()
    }

    pub fn save(&self) -> std::io::Result<()> {
        crate::paths::ensure_dirs()?;
        let path = file();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, &path)
    }

    pub fn sessions(&self) -> u32 {
        self.days.values().fold(0u32, |sum, n| sum.saturating_add(*n))
    }
}

/// Count the conversations of `roots` that began on `first` or later, by local day. A
/// conversation that sits in two folders (after an account switch) counts once.
pub fn scan(roots: &Roots, first: Day) -> History {
    let mut seen: HashSet<String> = HashSet::new();
    let mut days: BTreeMap<String, u32> = BTreeMap::new();
    let mut left = MOST_FILES;
    for root in &roots.claude {
        scan_claude(root, first, &mut seen, &mut days, &mut left);
    }
    for root in &roots.codex {
        scan_codex(root, first, &mut seen, &mut days, &mut left);
    }
    History { scanned: crate::model::now(), days }
}

fn count(day: Day, days: &mut BTreeMap<String, u32>) {
    let n = days.entry(day::iso(day)).or_default();
    *n = n.saturating_add(1);
}

/// The entries of a folder that are files (or folders), without following a link.
fn entries(dir: &Path, folders: bool) -> Vec<(String, PathBuf, std::fs::Metadata)> {
    let mut out: Vec<(String, PathBuf, std::fs::Metadata)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let meta = std::fs::symlink_metadata(entry.path()).ok()?;
            (if folders { meta.is_dir() } else { meta.is_file() }).then(|| (entry.file_name().to_string_lossy().into_owned(), entry.path(), meta))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

fn modified(meta: &std::fs::Metadata) -> Option<i64> {
    Some(meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
}

fn scan_claude(projects: &Path, first: Day, seen: &mut HashSet<String>, days: &mut BTreeMap<String, u32>, left: &mut usize) {
    for (_, folder, _) in entries(projects, true) {
        for (name, path, meta) in entries(&folder, false) {
            // A sub-agent's file (`agent-<id>.jsonl`) is part of its conversation, not one more.
            let Some(id) = name.strip_suffix(".jsonl").filter(|id| !id.starts_with("agent-")) else { continue };
            let changed = modified(&meta);
            // Last written before the first day: it began before it too.
            if changed.is_some_and(|at| day::local(at) < first) {
                continue;
            }
            if *left == 0 {
                return;
            }
            *left -= 1;
            let Some(began) = claude_began(&path, changed) else { continue };
            if began >= first && seen.insert(format!("claude:{id}")) {
                count(began, days);
            }
        }
    }
}

/// The local day a Claude conversation began: the first `timestamp` in the head of its file,
/// else the time the file was last written. None for `claude -p` and SDK runs (scripts, not a
/// person's work), the manager's own conversations, and a file with no line.
fn claude_began(path: &Path, changed: Option<i64>) -> Option<Day> {
    let file = std::fs::File::open(path).ok()?;
    let mut began: Option<Day> = None;
    let mut lines = 0;
    for line in BufReader::new(file.take(CLAUDE_HEAD)).split(b'\n') {
        let Ok(line) = line else { break };
        if line.is_empty() {
            continue;
        }
        lines += 1;
        let has = |key: &[u8]| line.windows(key.len()).any(|w| w == key);
        let (stamp, cwd) = (began.is_none() && has(b"\"timestamp\""), has(b"\"cwd\""));
        if !stamp && !cwd {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else { continue };
        if value["entrypoint"].as_str().is_some_and(|e| e.starts_with("sdk")) {
            return None;
        }
        if value["cwd"].as_str().is_some_and(|cwd| !cwd.is_empty() && crate::history::is_managers(Path::new(cwd))) {
            return None;
        }
        if began.is_none() {
            began = value["timestamp"].as_str().and_then(unix_of).map(day::local);
        }
        // The first line with the folder also says how it was started: nothing more to learn.
        if cwd && began.is_some() {
            break;
        }
    }
    if lines == 0 {
        return None;
    }
    began.or_else(|| changed.map(day::local))
}

/// The Unix time of `2026-11-16T09:40:12.345Z`.
fn unix_of(stamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(stamp).ok().map(|at| at.timestamp())
}

fn scan_codex(sessions: &Path, first: Day, seen: &mut HashSet<String>, days: &mut BTreeMap<String, u32>, left: &mut usize) {
    let number = |name: &str| name.parse::<u32>().ok();
    for (year, year_path, _) in entries(sessions, true) {
        let Some(y) = number(&year) else { continue };
        for (month, month_path, _) in entries(&year_path, true) {
            let Some(m) = number(&month).filter(|m| (1..=12).contains(m)) else { continue };
            for (date, day_path, _) in entries(&month_path, true) {
                let Some(d) = number(&date).filter(|d| (1..=31).contains(d)) else { continue };
                let began = day::from_civil(i64::from(y), m, d);
                // The folders are read newest first: the rest of this month is older still.
                if began < first {
                    break;
                }
                for (name, path, _) in entries(&day_path, false) {
                    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                        continue;
                    }
                    if *left == 0 {
                        return;
                    }
                    *left -= 1;
                    if codex_is_persons(&path) && seen.insert(format!("codex:{name}")) {
                        count(began, days);
                    }
                }
            }
        }
    }
}

/// Whether a person had this Codex conversation, by the rules of `history.rs`: not a `codex
/// exec` run, a sub-agent, or a run another tool started.
fn codex_is_persons(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else { return false };
    let mut line = Vec::new();
    if BufReader::new(file.take(CODEX_HEAD)).read_until(b'\n', &mut line).is_err() {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<Value>(&line) else { return false };
    let meta = &value["payload"];
    let source = meta["source"].as_str().unwrap_or("");
    let thread_source = meta["thread_source"].as_str().unwrap_or("user");
    let originator = meta["originator"].as_str().unwrap_or("");
    value["type"] == "session_meta" && matches!(source, "cli" | "vscode") && thread_source == "user" && originator != "Claude Code"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn set_time(path: &Path, unix: i64) {
        filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(unix, 0)).unwrap();
    }

    /// Noon of a local day, as a Unix time and as the stamp Claude Code writes.
    fn noon(date: &str) -> (i64, String) {
        let stamp = format!("{date}T12:00:00.000Z");
        (unix_of(&stamp).unwrap(), stamp)
    }

    fn claude(root: &Path, folder: &str, id: &str, date: &str, entrypoint: &str) -> PathBuf {
        let (unix, stamp) = noon(date);
        let path = root.join(folder).join(format!("{id}.jsonl"));
        write(&path, &format!("{{\"type\":\"summary\",\"summary\":\"made up\"}}\n{{\"type\":\"user\",\"cwd\":\"/work/acme/app\",\"entrypoint\":\"{entrypoint}\",\"timestamp\":\"{stamp}\",\"message\":{{\"content\":\"made-up words\"}}}}\n"));
        set_time(&path, unix + 600);
        path
    }

    fn codex(root: &Path, date: &str, id: &str, source: &str) {
        let (y, rest) = date.split_once('-').unwrap();
        let (m, d) = rest.split_once('-').unwrap();
        let path = root.join(y).join(m).join(d).join(format!("rollout-{date}T10-00-00-{id}.jsonl"));
        write(&path, &format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/work/acme/app\",\"source\":\"{source}\",\"originator\":\"codex_cli_rs\"}}}}\n{{\"type\":\"event\"}}\n"));
    }

    /// The local day of noon UTC is the same date in every zone from -11 to +11 hours.
    fn day_of(date: &str) -> Day {
        day::local(noon(date).0)
    }

    #[test]
    fn conversations_are_counted_on_the_day_they_began() {
        let dir = tempfile::tempdir().unwrap();
        let (claude_root, codex_root) = (dir.path().join("home/.claude/projects"), dir.path().join("home/.codex/sessions"));
        claude(&claude_root, "-work-acme-app", "a1", "2026-03-02", "cli");
        claude(&claude_root, "-work-acme-app", "a2", "2026-03-02", "cli");
        claude(&claude_root, "-work-acme-tools", "a3", "2026-03-05", "cli");
        codex(&codex_root, "2026-03-05", "c1", "cli");
        codex(&codex_root, "2026-03-09", "c2", "vscode");
        let roots = Roots { claude: vec![claude_root], codex: vec![codex_root] };
        let history = scan(&roots, day_of("2026-01-01"));
        let count = |date: &str| history.days.get(&day::iso(day_of(date))).copied().unwrap_or(0);
        assert_eq!((count("2026-03-02"), count("2026-03-05"), count("2026-03-09")), (2, 2, 1));
        assert_eq!(history.sessions(), 5);
        assert!(history.scanned > 0);
    }

    #[test]
    fn scripts_sub_agents_and_other_files_are_not_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let (claude_root, codex_root) = (dir.path().join("c/projects"), dir.path().join("x/sessions"));
        claude(&claude_root, "-work-acme-app", "person", "2026-03-02", "cli");
        // `claude -p` and the SDK: a script's run.
        claude(&claude_root, "-work-acme-app", "script", "2026-03-02", "sdk-cli");
        // A sub-agent's file beside its conversation, a note, an empty file, a broken one.
        claude(&claude_root, "-work-acme-app", "agent-77", "2026-03-02", "cli");
        write(&claude_root.join("-work-acme-app").join("notes.txt"), "made up");
        write(&claude_root.join("-work-acme-app").join("empty.jsonl"), "");
        // A sub-agent's folder below a project is not walked.
        claude(&claude_root.join("-work-acme-app"), "person/subagents", "deep", "2026-03-02", "cli");
        codex(&codex_root, "2026-03-02", "exec", "exec");
        codex(&codex_root, "2026-03-02", "person", "cli");
        write(&codex_root.join("2026/03/02/rollout-2026-03-02T11-00-00-broken.jsonl"), "not json\n");
        write(&codex_root.join("2026/03/02/other.jsonl"), "{}\n");
        let history = scan(&Roots { claude: vec![claude_root], codex: vec![codex_root] }, day_of("2026-01-01"));
        assert_eq!(history.days.get(&day::iso(day_of("2026-03-02"))), Some(&2), "{history:?}");
        assert_eq!(history.sessions(), 2);
    }

    #[test]
    fn a_file_with_no_time_takes_the_time_it_was_written_and_broken_lines_are_passed_over() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let path = root.join("-work-acme-app").join("b1.jsonl");
        write(&path, "not json at all\n{\"type\":\"summary\",\"summary\":\"made up\"}\n");
        set_time(&path, noon("2026-04-10").0);
        let history = scan(&Roots { claude: vec![root], codex: Vec::new() }, day_of("2026-01-01"));
        assert_eq!(history.days.get(&day::iso(day_of("2026-04-10"))), Some(&1), "{history:?}");
    }

    #[test]
    fn days_before_the_first_are_left_out_and_a_copy_in_a_second_folder_counts_once() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("one/projects"), dir.path().join("two/projects"));
        let (codex_one, codex_two) = (dir.path().join("one/sessions"), dir.path().join("two/sessions"));
        claude(&one, "-work-acme-app", "old", "2025-06-01", "cli");
        claude(&one, "-work-acme-app", "same", "2026-03-02", "cli");
        claude(&two, "-work-acme-app", "same", "2026-03-02", "cli");
        // Written again later, but begun before the first day.
        let resumed = claude(&one, "-work-acme-app", "resumed", "2025-06-01", "cli");
        set_time(&resumed, noon("2026-03-03").0);
        codex(&codex_one, "2025-06-01", "old", "cli");
        codex(&codex_one, "2026-03-02", "same", "cli");
        codex(&codex_two, "2026-03-02", "same", "cli");
        let roots = Roots { claude: vec![one, two], codex: vec![codex_one, codex_two] };
        let history = scan(&roots, day_of("2026-01-01"));
        assert_eq!(history.days.len(), 1, "{history:?}");
        assert_eq!(history.sessions(), 2);
    }

    #[test]
    fn nothing_there_gives_nothing_and_the_roots_are_the_clis_and_the_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let missing = Roots { claude: vec![dir.path().join("no/projects")], codex: vec![dir.path().join("no/sessions")] };
        assert_eq!(scan(&missing, 0).days, BTreeMap::new());
        let accounts = dir.path().join("accounts");
        std::fs::create_dir_all(accounts.join("claude-test")).unwrap();
        // No home (a test, a scratch folder): only Glasscode's own accounts are read.
        let own = Roots::of(None, &accounts);
        assert_eq!(own.claude, vec![accounts.join("claude-test").join("projects")]);
        assert_eq!(own.codex, vec![accounts.join("claude-test").join("sessions")]);
        let home = dir.path().join("home");
        let all = Roots::of(Some(&home), &accounts);
        assert_eq!(all.claude[0], home.join(".claude").join("projects"));
        assert_eq!(all.codex[0], home.join(".codex").join("sessions"));
        assert_eq!(all.claude.len(), 2);
    }

    #[test]
    fn the_result_is_kept_and_read_again() {
        // The tests' own Glasscode folder (`paths::root` in a test run), never the person's.
        let _ = std::fs::remove_file(file());
        assert!(file().starts_with(crate::paths::use_test_root()) || std::env::var_os("GLASSCODE_HOME").is_some());
        assert_eq!(History::load(), None);
        let history = History { scanned: 7, days: BTreeMap::from([("2026-03-02".to_string(), 3)]) };
        history.save().unwrap();
        assert_eq!(History::load(), Some(history));
        std::fs::remove_file(file()).unwrap();
    }
}
