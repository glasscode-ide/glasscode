//! Usage: how many tokens each Claude and Codex account used, by day, project and session, and
//! how close each is to its limits, from what the CLIs write on this Mac.
//!
//! - Claude Code's transcripts (`<config>/projects/**/*.jsonl`) carry each reply's token counts
//!   (input, output, cache writes and reads). A reply streams over several lines with the same
//!   message id; a copied conversation repeats ids in another file: each id counts once.
//! - Codex's session logs (`<home>/sessions/**/rollout-*.jsonl`) carry running token totals and
//!   the rate limits ChatGPT reports (used percent, window, reset time).
//! - Claude's limits come from its status line input (`rate_limits`: the 5-hour and 7-day
//!   windows), which sessions Glasscode starts record with [`STATUSLINE_COMMAND`].
//!
//! No login token or credential file is read. Files are read once: what was read is kept in
//! `~/.glasscode/usage-cache.json` (offsets and per-day counts), so a refresh reads only what
//! the CLIs added since.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::AgentKind;
use crate::paths;

/// Days kept: the longest range the page shows, and a little more.
pub const KEEP_DAYS: i64 = 35;
const CACHE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_write + self.cache_read
    }

    pub fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_write += other.cache_write;
        self.cache_read += other.cache_read;
    }

    /// What `self` has beyond `earlier` (never negative).
    fn since(&self, earlier: &Tokens) -> Tokens {
        Tokens {
            input: self.input.saturating_sub(earlier.input),
            output: self.output.saturating_sub(earlier.output),
            cache_write: self.cache_write.saturating_sub(earlier.cache_write),
            cache_read: self.cache_read.saturating_sub(earlier.cache_read),
        }
    }
}

/// One rate-limit window, as the CLI last reported it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Limit {
    pub window_minutes: u64,
    /// 0 to 100.
    pub used_percent: f64,
    /// Unix time the window resets.
    pub resets_at: Option<i64>,
}

impl Limit {
    pub fn label(&self) -> String {
        match self.window_minutes {
            300 => "Current window (5 hours)".into(),
            10080 => "This week".into(),
            m if m >= 1440 && m % 1440 == 0 => format!("{}-day window", m / 1440),
            m if m >= 60 => format!("{}-hour window", m / 60),
            m => format!("{m}-minute window"),
        }
    }
}

/// An account to count, as Glasscode knows it.
#[derive(Clone, Debug)]
pub struct AccountInput {
    pub id: String,
    pub agent: AgentKind,
    /// Its config directory (`~/.claude`, `~/.codex`, or Glasscode's own for added accounts).
    pub dir: PathBuf,
    /// Glasscode's sessions on this account (their status line records are its Claude limits).
    pub sessions: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionUsage {
    /// The CLI's own session id.
    pub id: String,
    pub cwd: String,
    /// Claude's title for the conversation, when it gave one.
    pub title: Option<String>,
    pub days: BTreeMap<String, Tokens>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountReport {
    pub id: String,
    pub agent: Option<AgentKind>,
    /// Limits whose window has not reset since they were read, shortest window first.
    pub limits: Vec<Limit>,
    /// When the limits were read (Unix time).
    pub limits_seen: Option<i64>,
    /// The plan the CLI reported ("pro", …).
    pub plan: Option<String>,
    pub days: BTreeMap<String, Tokens>,
    pub sessions: Vec<SessionUsage>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub accounts: Vec<AccountReport>,
    pub scanned_at: i64,
    /// Files read this time (for "Reading…" notes and tests).
    pub files_read: usize,
}

// ----- the cache -----------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct FileState {
    offset: u64,
    #[serde(default)]
    session: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    days: BTreeMap<String, Tokens>,
    /// Claude: the reply being streamed when the file was last read, and what was counted.
    #[serde(default)]
    last_id: u64,
    #[serde(default)]
    last_day: String,
    #[serde(default)]
    last_tokens: Tokens,
    /// Codex: the session's running total so far.
    #[serde(default)]
    total: Tokens,
    /// Codex: the newest rate limits in this file: (when, limits, plan).
    #[serde(default)]
    limits: Option<(i64, Vec<Limit>, Option<String>)>,
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    version: u32,
    files: HashMap<String, FileState>,
    /// Claude reply ids counted (a hash), with the day number they were from.
    seen: HashMap<u64, i64>,
}

fn cache_path() -> PathBuf {
    paths::root().join("usage-cache.json")
}

/// Reads and remembers usage; one per process (the app keeps it between refreshes).
#[derive(Default)]
pub struct Scanner {
    cache: Cache,
}

impl Scanner {
    pub fn load() -> Self {
        let cache = std::fs::read(cache_path())
            .ok()
            .and_then(|b| serde_json::from_slice::<Cache>(&b).ok())
            .filter(|c| c.version == CACHE_VERSION)
            .unwrap_or_default();
        Self { cache }
    }

    pub fn save(&self) {
        let _ = paths::ensure_dirs();
        if let Ok(bytes) = serde_json::to_vec(&self.cache) {
            let tmp = cache_path().with_extension("json.tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(tmp, cache_path());
            }
        }
    }

    /// Read what the CLIs added since the last scan and report every account.
    pub fn scan(&mut self, accounts: &[AccountInput], now: i64) -> Report {
        self.cache.version = CACHE_VERSION;
        let oldest = now - KEEP_DAYS * 86400;
        let mut files_read = 0;
        let mut reports = Vec::new();
        let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
        for account in accounts {
            let files = match account.agent {
                AgentKind::Claude => jsonl_files(&account.dir.join("projects"), |_| true),
                AgentKind::Codex => jsonl_files(&account.dir.join("sessions"), |name| name.starts_with("rollout-")),
                AgentKind::Shell => Vec::new(),
            };
            let mut mine: Vec<String> = Vec::new();
            for (path, size, mtime) in files {
                let key = path.to_string_lossy().into_owned();
                present.insert(key.clone());
                let known = self.cache.files.contains_key(&key);
                if !known && mtime < oldest {
                    continue;
                }
                let mut state = self.cache.files.remove(&key).unwrap_or_default();
                if size < state.offset {
                    // Rewritten: keep what was counted, read on from its new end.
                    state.offset = size;
                }
                if size > state.offset {
                    files_read += 1;
                    match account.agent {
                        AgentKind::Codex => read_codex(&path, &mut state, oldest),
                        _ => read_claude(&path, &mut state, &mut self.cache.seen, oldest),
                    }
                }
                state.days.retain(|day, _| day_number(day).is_some_and(|d| d >= oldest / 86400 - 1));
                self.cache.files.insert(key.clone(), state);
                mine.push(key);
            }
            reports.push(self.report(account, &mine, now));
        }
        // Forget files that are gone, and old reply ids.
        self.cache.files.retain(|path, _| present.contains(path));
        let first_day = oldest / 86400 - 1;
        self.cache.seen.retain(|_, day| *day >= first_day);
        Report { accounts: reports, scanned_at: now, files_read }
    }

    fn report(&self, account: &AccountInput, files: &[String], now: i64) -> AccountReport {
        let mut days: BTreeMap<String, Tokens> = BTreeMap::new();
        let mut sessions: HashMap<String, SessionUsage> = HashMap::new();
        let mut newest_limits: Option<(i64, Vec<Limit>, Option<String>)> = None;
        for key in files {
            let Some(state) = self.cache.files.get(key) else { continue };
            for (day, tokens) in &state.days {
                days.entry(day.clone()).or_default().add(tokens);
            }
            if !state.days.is_empty() {
                let id = if state.session.is_empty() { Path::new(key).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() } else { state.session.clone() };
                let entry = sessions.entry(id.clone()).or_insert_with(|| SessionUsage { id, ..Default::default() });
                if entry.cwd.is_empty() {
                    entry.cwd = state.cwd.clone();
                }
                if entry.title.is_none() {
                    entry.title = state.title.clone();
                }
                for (day, tokens) in &state.days {
                    entry.days.entry(day.clone()).or_default().add(tokens);
                }
            }
            if let Some(limits) = &state.limits {
                if newest_limits.as_ref().is_none_or(|n| limits.0 > n.0) {
                    newest_limits = Some(limits.clone());
                }
            }
        }
        if account.agent == AgentKind::Claude {
            newest_limits = claude_limits(&account.sessions);
        }
        let (limits_seen, mut limits, plan) = match newest_limits {
            Some((at, limits, plan)) => (Some(at), limits, plan),
            None => (None, Vec::new(), None),
        };
        // A window that has reset since it was read says nothing about now.
        limits.retain(|l| l.resets_at.is_none_or(|r| r > now));
        limits.sort_by_key(|l| l.window_minutes);
        let mut sessions: Vec<SessionUsage> = sessions.into_values().collect();
        sessions.sort_by(|a, b| a.id.cmp(&b.id));
        AccountReport { id: account.id.clone(), agent: Some(account.agent), limits, limits_seen, plan, days, sessions }
    }
}

/// `*.jsonl` files under a folder (any depth), with their size and modification time.
fn jsonl_files(root: &Path, name_ok: impl Fn(&str) -> bool + Copy) -> Vec<(PathBuf, u64, i64)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.ends_with(".jsonl") && name_ok(&name) {
                    if let Ok(meta) = entry.metadata() {
                        let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
                        out.push((path, meta.len(), mtime));
                    }
                }
            }
        }
    }
    out
}

/// Whole lines from `offset` on; the last line is left for next time if it has no newline yet.
fn read_lines(path: &Path, state: &mut FileState, mut each: impl FnMut(&str, &mut FileState)) {
    let Ok(mut file) = std::fs::File::open(path) else { return };
    if file.seek(SeekFrom::Start(state.offset)).is_err() {
        return;
    }
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut line: Vec<u8> = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if line.last() != Some(&b'\n') {
                    break;
                }
                state.offset += n as u64;
                if let Ok(text) = std::str::from_utf8(&line) {
                    each(text, state);
                }
            }
        }
    }
}

fn read_claude(path: &Path, state: &mut FileState, seen: &mut HashMap<u64, i64>, oldest: i64) {
    read_lines(path, state, |line, state| {
        if line.contains("\"ai-title\"") {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(title) = v["aiTitle"].as_str().filter(|t| !t.trim().is_empty()) {
                    state.title = Some(title.trim().to_string());
                }
            }
            return;
        }
        if !(line.contains("\"usage\"") && line.contains("\"assistant\"")) {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        if v["type"].as_str() != Some("assistant") {
            return;
        }
        let usage = &v["message"]["usage"];
        if usage.is_null() {
            return;
        }
        let Some(at) = v["timestamp"].as_str().and_then(crate::github::parse_time) else { return };
        if at < oldest {
            return;
        }
        if let Some(session) = v["sessionId"].as_str() {
            if state.session.is_empty() {
                state.session = session.to_string();
            }
        }
        if state.cwd.is_empty() {
            if let Some(cwd) = v["cwd"].as_str() {
                state.cwd = cwd.to_string();
            }
        }
        let n = |k: &str| usage[k].as_u64().unwrap_or(0);
        let tokens = Tokens { input: n("input_tokens"), output: n("output_tokens"), cache_write: n("cache_creation_input_tokens"), cache_read: n("cache_read_input_tokens") };
        let id = v["message"]["id"].as_str().or_else(|| v["requestId"].as_str()).or_else(|| v["uuid"].as_str()).unwrap_or_default();
        let hash = fnv(id);
        if hash == state.last_id && !state.last_day.is_empty() {
            // The same reply, further along: count what it added.
            let more = tokens.since(&state.last_tokens);
            state.days.entry(state.last_day.clone()).or_default().add(&more);
            state.last_tokens = tokens;
            return;
        }
        if seen.contains_key(&hash) {
            return;
        }
        let day = local_day(at);
        seen.insert(hash, at.div_euclid(86400));
        state.days.entry(day.clone()).or_default().add(&tokens);
        state.last_id = hash;
        state.last_day = day;
        state.last_tokens = tokens;
    });
}

fn read_codex(path: &Path, state: &mut FileState, oldest: i64) {
    read_lines(path, state, |line, state| {
        let meta = line.contains("\"session_meta\"");
        let context = !meta && state.cwd.is_empty() && line.contains("\"turn_context\"");
        let count = line.contains("\"token_count\"");
        if !(meta || context || count) {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        let payload = &v["payload"];
        match v["type"].as_str() {
            Some("session_meta") => {
                if let Some(id) = payload["id"].as_str() {
                    state.session = id.to_string();
                }
                if let Some(cwd) = payload["cwd"].as_str() {
                    state.cwd = cwd.to_string();
                }
            }
            Some("turn_context") => {
                if let Some(cwd) = payload["cwd"].as_str() {
                    state.cwd = cwd.to_string();
                }
            }
            Some("event_msg") if payload["type"].as_str() == Some("token_count") => {
                let at = v["timestamp"].as_str().and_then(crate::github::parse_time).unwrap_or(0);
                let total = &payload["info"]["total_token_usage"];
                if !total.is_null() {
                    let n = |k: &str| total[k].as_u64().unwrap_or(0);
                    let (input, cached, written) = (n("input_tokens"), n("cached_input_tokens"), n("cache_write_input_tokens"));
                    let now = Tokens { input: input.saturating_sub(cached + written), output: n("output_tokens"), cache_write: written, cache_read: cached };
                    // Totals only grow within a session; a smaller one starts over.
                    let more = if now.total() < state.total.total() { now } else { now.since(&state.total) };
                    state.total = now;
                    if at >= oldest && more.total() > 0 {
                        state.days.entry(local_day(at)).or_default().add(&more);
                    }
                }
                let limits = &payload["rate_limits"];
                if limits.is_object() {
                    let read = |w: &Value| -> Option<Limit> {
                        Some(Limit { window_minutes: w["window_minutes"].as_u64()?, used_percent: w["used_percent"].as_f64()?, resets_at: w["resets_at"].as_i64() })
                    };
                    let list: Vec<Limit> = [&limits["primary"], &limits["secondary"]].into_iter().filter_map(read).collect();
                    if !list.is_empty() && state.limits.as_ref().is_none_or(|(when, _, _)| at >= *when) {
                        state.limits = Some((at, list, limits["plan_type"].as_str().map(str::to_string)));
                    }
                }
            }
            _ => {}
        }
    });
}

// ----- Claude's limits, from its status line ----------------------------------------------

/// Where a session's status line input is kept (the newest only).
pub fn limits_dir() -> PathBuf {
    paths::root().join("usage").join("limits")
}

pub fn limits_file(session_id: &str) -> PathBuf {
    limits_dir().join(format!("{session_id}.json"))
}

/// For `claude --settings` as `statusLine.command` (with `GLASSCODE_LIMITS` set to
/// [`limits_file`] in the session's environment): keeps what Claude Code tells its status line,
/// which includes `rate_limits` for Claude.ai plans, and then runs the person's own status line
/// command (`GLASSCODE_STATUSLINE`, from [`user_statusline`]) with the same input, so their
/// status line stays as it was. With none, it shows nothing. On Windows, `glasscode statusline`
/// does the same (with [`save_limits`]).
pub const STATUSLINE_COMMAND: &str = r#"i=$(cat); if [ -n "$GLASSCODE_LIMITS" ]; then mkdir -p "$(dirname "$GLASSCODE_LIMITS")" && printf '%s' "$i" > "$GLASSCODE_LIMITS.tmp" && mv -f "$GLASSCODE_LIMITS.tmp" "$GLASSCODE_LIMITS"; fi; if [ -n "$GLASSCODE_STATUSLINE" ]; then printf '%s' "$i" | sh -c "$GLASSCODE_STATUSLINE"; fi"#;

/// Keep a status line input in a session's limits file. It goes to a file of its own first, so
/// that Usage never reads half of it.
pub fn save_limits(file: &Path, input: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    std::fs::write(&tmp, input)?;
    std::fs::rename(&tmp, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The person's own status line command for a Claude config directory, if they set one.
pub fn user_statusline(config_dir: &Path) -> Option<String> {
    let settings: Value = serde_json::from_slice(&std::fs::read(config_dir.join("settings.json")).ok()?).ok()?;
    settings["statusLine"]["command"].as_str().map(str::to_string).filter(|c| !c.trim().is_empty())
}

/// The newest Claude limits any of these sessions' status lines recorded.
fn claude_limits(sessions: &[String]) -> Option<(i64, Vec<Limit>, Option<String>)> {
    let mut best: Option<(i64, Vec<Limit>, Option<String>)> = None;
    for id in sessions {
        let path = limits_file(id);
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let when = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
        if best.as_ref().is_some_and(|b| b.0 >= when) {
            continue;
        }
        let Some(limits) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).map(|v| parse_claude_limits(&v)) else { continue };
        if !limits.is_empty() {
            best = Some((when, limits, None));
        }
    }
    best
}

/// `rate_limits.five_hour` / `seven_day` (`used_percentage`, `resets_at`) of a status line input.
pub fn parse_claude_limits(input: &Value) -> Vec<Limit> {
    let rate = &input["rate_limits"];
    [("five_hour", 300), ("seven_day", 10080)]
        .into_iter()
        .filter_map(|(key, minutes)| {
            let w = &rate[key];
            let used = w["used_percentage"].as_f64().or_else(|| w["used_percent"].as_f64())?;
            let resets_at = match &w["resets_at"] {
                Value::Number(n) => n.as_i64().map(|s| if s > 100_000_000_000 { s / 1000 } else { s }),
                Value::String(s) => crate::github::parse_time(s),
                _ => None,
            };
            Some(Limit { window_minutes: minutes, used_percent: used, resets_at })
        })
        .collect()
}

// ----- small helpers ---------------------------------------------------------------------

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// "2026-10-20" for a Unix time, in this Mac's time zone.
pub fn local_day(secs: i64) -> String {
    let (y, m, d, _, _, _) = local_parts(secs);
    format!("{y:04}-{m:02}-{d:02}")
}

/// (year, month, day, hour, minute, weekday 0 = Sunday) in this Mac's time zone.
pub fn local_parts(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let t = crate::sys::local_time(secs).unwrap_or_default();
    (t.year, t.month, t.day, t.hour, t.minute, t.weekday)
}

/// Days since 1970 of a "2026-10-20" day (for keeping only recent ones).
fn day_number(day: &str) -> Option<i64> {
    crate::github::parse_time(&format!("{day}T12:00:00Z")).map(|s| s.div_euclid(86400))
}

/// The last `n` days up to today, oldest first ("2026-10-07" … "2026-10-20").
pub fn last_days(n: u32, now: i64) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut t = now;
    while out.len() < n as usize {
        let day = local_day(t);
        if out.last() != Some(&day) {
            out.push(day);
        }
        t -= 86400;
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, lines: &[String]) -> PathBuf {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        path
    }

    fn claude_line(id: &str, at: &str, input: u64, output: u64) -> String {
        serde_json::json!({ "type": "assistant", "sessionId": "s1", "cwd": "/w/app", "timestamp": at,
            "message": { "id": id, "usage": { "input_tokens": input, "output_tokens": output, "cache_creation_input_tokens": 10, "cache_read_input_tokens": 100 } } }).to_string()
    }

    #[test]
    fn counts_claude_replies_once() {
        let tmp = tempfile::tempdir().unwrap();
        let now = crate::github::parse_time("2026-10-20T15:00:00Z").unwrap();
        let day = local_day(crate::github::parse_time("2026-10-20T09:00:00Z").unwrap());
        // A streamed reply (same id twice, growing), another reply, an old one, and a title.
        let path = write(tmp.path(), "projects/-w-app/s1.jsonl", &[
            claude_line("m1", "2026-10-20T09:00:00Z", 5, 1),
            claude_line("m1", "2026-10-20T09:00:01Z", 5, 40),
            claude_line("m2", "2026-10-20T09:05:00Z", 2, 3),
            claude_line("m0", "2026-08-01T09:00:00Z", 999, 999),
            serde_json::json!({ "type": "ai-title", "aiTitle": "Munich session search", "sessionId": "s1" }).to_string(),
        ]);
        // The same conversation copied to another account's folder is not counted again.
        write(tmp.path(), "copy/projects/-w-app/s1.jsonl", &[claude_line("m2", "2026-10-20T09:05:00Z", 2, 3)]);
        let accounts = vec![
            AccountInput { id: "a".into(), agent: AgentKind::Claude, dir: tmp.path().to_path_buf(), sessions: vec![] },
            AccountInput { id: "b".into(), agent: AgentKind::Claude, dir: tmp.path().join("copy"), sessions: vec![] },
        ];
        let mut scanner = Scanner::default();
        let report = scanner.scan(&accounts, now);
        let a = &report.accounts[0];
        let t = a.days[&day];
        assert_eq!((t.input, t.output), (7, 43));
        assert_eq!(t.cache_read, 200);
        assert_eq!(a.sessions.iter().find_map(|s| s.title.clone()).as_deref(), Some("Munich session search"));
        assert!(report.accounts[1].days.values().all(|t| t.total() == 0));
        // Nothing new: nothing read; an appended reply is read alone.
        assert_eq!(scanner.scan(&accounts, now).files_read, 0);
        let mut lines = std::fs::read_to_string(&path).unwrap();
        lines.push_str(&format!("{}\n", claude_line("m3", "2026-10-20T10:00:00Z", 1, 1)));
        std::fs::write(&path, lines).unwrap();
        let again = scanner.scan(&accounts, now);
        assert_eq!(again.files_read, 1);
        assert_eq!(again.accounts[0].days[&day].input, 8);
    }

    #[test]
    fn reads_codex_totals_and_limits() {
        let tmp = tempfile::tempdir().unwrap();
        let now = crate::github::parse_time("2026-10-20T15:00:00Z").unwrap();
        let count = |at: &str, input: u64, cached: u64, output: u64, used: f64| {
            serde_json::json!({ "timestamp": at, "type": "event_msg", "payload": { "type": "token_count",
                "info": { "total_token_usage": { "input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": 0, "output_tokens": output, "total_tokens": input + output } },
                "rate_limits": { "primary": { "used_percent": used, "window_minutes": 10080, "resets_at": now + 3600 }, "secondary": null, "plan_type": "pro" } } }).to_string()
        };
        write(tmp.path(), "sessions/2026/10/20/rollout-x.jsonl", &[
            serde_json::json!({ "type": "session_meta", "payload": { "id": "c1", "cwd": "/w/notes" } }).to_string(),
            count("2026-10-20T09:00:00Z", 1000, 600, 50, 30.0),
            count("2026-10-20T09:00:05Z", 1000, 600, 50, 30.0),
            count("2026-10-20T10:00:00Z", 1500, 800, 90, 38.0),
        ]);
        let accounts = vec![AccountInput { id: "x".into(), agent: AgentKind::Codex, dir: tmp.path().to_path_buf(), sessions: vec![] }];
        let report = Scanner::default().scan(&accounts, now);
        let a = &report.accounts[0];
        let day = local_day(crate::github::parse_time("2026-10-20T09:00:00Z").unwrap());
        let t = a.days[&day];
        assert_eq!((t.input, t.cache_read, t.output, t.total()), (700, 800, 90, 1590));
        assert_eq!(a.limits.len(), 1);
        assert_eq!((a.limits[0].used_percent, a.limits[0].label().as_str()), (38.0, "This week"));
        assert_eq!(a.plan.as_deref(), Some("pro"));
        assert_eq!(a.sessions[0].cwd, "/w/notes");
        // A window that has reset is not shown.
        let later = Scanner::default().scan(&accounts, now + 7200);
        assert!(later.accounts[0].limits.is_empty());
    }

    #[test]
    fn reads_claude_status_line_limits() {
        let input = serde_json::json!({ "rate_limits": {
            "five_hour": { "used_percentage": 62.5, "resets_at": 1792461600 },
            "seven_day": { "used_percentage": 41, "resets_at": "2026-10-23T09:00:00Z" } } });
        let limits = parse_claude_limits(&input);
        assert_eq!(limits.len(), 2);
        assert_eq!((limits[0].window_minutes, limits[0].used_percent, limits[0].resets_at), (300, 62.5, Some(1792461600)));
        assert_eq!(limits[1].label(), "This week");
        assert!(limits[1].resets_at.is_some());
        assert!(parse_claude_limits(&serde_json::json!({ "model": {} })).is_empty());
        assert!(STATUSLINE_COMMAND.contains("GLASSCODE_LIMITS") && STATUSLINE_COMMAND.contains("GLASSCODE_STATUSLINE"));
    }

    /// `USAGE_REAL=1 cargo test -p glasscode_core --lib usage::tests::real -- --ignored --nocapture`:
    /// a first scan of this Mac's ~/.claude and ~/.codex (counts only).
    #[test]
    #[ignore]
    fn real() {
        if std::env::var_os("USAGE_REAL").is_none() {
            return;
        }
        let home = crate::paths::home();
        let accounts = vec![
            AccountInput { id: "claude".into(), agent: AgentKind::Claude, dir: home.join(".claude"), sessions: vec![] },
            AccountInput { id: "codex".into(), agent: AgentKind::Codex, dir: home.join(".codex"), sessions: vec![] },
        ];
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let mut scanner = Scanner::default();
        let started = std::time::Instant::now();
        let report = scanner.scan(&accounts, now);
        println!("first scan: {} files in {:?}", report.files_read, started.elapsed());
        for a in &report.accounts {
            let total: u64 = a.days.values().map(Tokens::total).sum();
            println!("{}: {} days, {} sessions, {} tokens, limits {:?}", a.id, a.days.len(), a.sessions.len(), total, a.limits);
        }
        let started = std::time::Instant::now();
        let again = scanner.scan(&accounts, now);
        println!("second scan: {} files in {:?}; cache {} bytes", again.files_read, started.elapsed(), serde_json::to_vec(&scanner.cache).unwrap().len());
    }

    #[test]
    fn days() {
        let now = crate::github::parse_time("2026-10-20T12:00:00Z").unwrap();
        let days = last_days(7, now);
        assert_eq!(days.len(), 7);
        assert_eq!(days.last().map(String::as_str), Some(local_day(now).as_str()));
        assert!(days.windows(2).all(|w| w[0] < w[1]));
    }
}
