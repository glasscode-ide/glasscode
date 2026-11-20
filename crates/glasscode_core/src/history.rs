//! Earlier Claude and Codex conversations of a folder, read from the CLIs' own session files,
//! so a new session can resume one of them.
//!
//! - Claude: `<config>/projects/<cwd with every non-alphanumeric character as '-'>/<id>.jsonl`.
//!   Lines carry `cwd` and `entrypoint`; `ai-title` lines hold the title `/resume` shows.
//! - Codex: `<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-*.jsonl`, whose first line
//!   (`session_meta`) has the id, `cwd` and where it came from; prompts are in `history.jsonl`.
//!
//! Both formats are internal to the CLIs; `docs/architecture.md` lists them in the checklist
//! for approving a new CLI version.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::model::{Account, AgentKind};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PastConversation {
    pub agent: AgentKind,
    /// The Glasscode account whose config folder holds it.
    pub account_id: String,
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    /// The latest prompt, when it differs from the title.
    pub last_prompt: Option<String>,
    pub updated: SystemTime,
}

/// Whether `folder` is the manager's (its work folder, or below it): the manager's own
/// conversations are not the person's to resume, and the first run never offers them (exec-28).
pub fn is_managers(folder: &Path) -> bool {
    let manager = crate::paths::root().join("manager");
    folder.starts_with(&manager) || crate::sys::same_path(folder, &crate::paths::manager_work_dir())
}

/// Conversations of `agent` started in `folder`, from every account, newest first. None for the
/// manager's own folder.
pub fn conversations(agent: AgentKind, accounts: &[Account], folder: &Path) -> Vec<PastConversation> {
    if is_managers(folder) {
        return Vec::new();
    }
    let mut all: Vec<PastConversation> = Vec::new();
    for account in accounts.iter().filter(|a| a.agent == agent) {
        match agent {
            AgentKind::Claude => all.extend(claude_conversations(account, folder)),
            AgentKind::Codex => all.extend(codex_conversations(account, folder)),
            AgentKind::Shell => {}
        }
    }
    // The same conversation can sit in two accounts after an account switch; keep the newest.
    all.sort_by(|a, b| b.updated.cmp(&a.updated));
    let mut seen = std::collections::HashSet::new();
    all.retain(|c| seen.insert(c.id.clone()));
    all
}

/// Conversations in `folders` (each project's root and worktrees) written to since `since` that no
/// Glasscode session has (`known`, the agents' conversation ids): sessions started outside
/// Glasscode (land-9), listed read-only, never acted on. Each with the folder it works in. It reads
/// the CLIs' folders: off the main thread.
pub fn active_outside(accounts: &[Account], folders: &[PathBuf], known: &std::collections::HashSet<String>, since: SystemTime) -> Vec<(PathBuf, PastConversation)> {
    let mut out = Vec::new();
    for folder in folders.iter().filter(|folder| !is_managers(folder)) {
        for conversation in conversations(AgentKind::Claude, accounts, folder) {
            if conversation.updated >= since && !known.contains(&conversation.id) {
                out.push((folder.clone(), conversation));
            }
        }
    }
    // Codex: the rollouts of the days since `since`, matched to a folder by their first line.
    for account in accounts.iter().filter(|a| a.agent == AgentKind::Codex) {
        let mut rollouts = Vec::new();
        crate::agents::collect_rollouts(&account.effective_dir().join("sessions"), since, &mut rollouts);
        for (updated, path) in rollouts {
            let Ok(file) = std::fs::File::open(&path) else { continue };
            let mut first = String::new();
            if BufReader::new(file).read_line(&mut first).is_err() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&first) else { continue };
            let meta = &value["payload"];
            let (Some(id), Some(cwd)) = (meta["id"].as_str(), meta["cwd"].as_str()) else { continue };
            if value["type"] != "session_meta" || known.contains(id) || meta["source"].as_str().is_some_and(|s| !matches!(s, "cli" | "vscode")) {
                continue;
            }
            let Some(folder) = folders.iter().find(|folder| same_folder(cwd, folder)) else { continue };
            if is_managers(Path::new(cwd)) {
                continue;
            }
            let title = codex_first_prompt(&path).map(|p| one_line(&p, 90)).unwrap_or_default();
            out.push((folder.clone(), PastConversation { agent: AgentKind::Codex, account_id: account.id.clone(), id: id.to_string(), path, title, last_prompt: None, updated }));
        }
    }
    out
}

fn same_folder(cwd: &str, folder: &Path) -> bool {
    crate::sys::same_path(Path::new(cwd), folder)
}

/// One line of text for a list: whitespace collapsed, cut at `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max - 1).collect();
    format!("{}…", cut.trim_end())
}

fn modified(path: &Path) -> SystemTime {
    std::fs::metadata(path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The last `bytes` of a file as lines (the first, partial line dropped).
fn tail_lines(path: &Path, bytes: u64) -> Vec<String> {
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    let _ = file.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    lines
}

/// Text a person typed, from a Claude `user` line; `None` for tool results, slash-command
/// wrappers (`<command-name>…`) and meta lines.
fn claude_prompt(line: &Value) -> Option<String> {
    if line["type"] != "user" || line["isMeta"] == true || line["isSidechain"] == true {
        return None;
    }
    let content = &line["message"]["content"];
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p["type"] == "text")
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<') && !text.starts_with("[Request interrupted")).then(|| text.to_string())
}

/// The folder in `<config>/projects` that Claude Code keeps the conversations of a work folder
/// in, named as Claude Code (2.1) names it: each UTF-16 unit of the path that is not an ASCII
/// letter or digit becomes `-` (`C:\Users\dev\app` is `C--Users-dev-app`); a name longer than 200
/// is cut there and gets a hash of the whole path. Windows finds the folder in any case.
pub fn claude_folder_name(folder: &Path) -> String {
    const LONGEST: usize = 200;
    let path = folder.to_string_lossy();
    let mut name = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else {
            name.extend(std::iter::repeat_n('-', c.len_utf16()));
        }
    }
    if name.len() <= LONGEST {
        return name;
    }
    // JavaScript's `(h << 5) - h + unit | 0` over the UTF-16 units, as a positive number in base 36.
    let hash = path.encode_utf16().fold(0i32, |h, unit| h.wrapping_mul(31).wrapping_add(i32::from(unit)));
    let mut rest = i64::from(hash).unsigned_abs();
    let mut digits = Vec::new();
    loop {
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(rest % 36) as usize]);
        rest /= 36;
        if rest == 0 {
            break;
        }
    }
    digits.reverse();
    format!("{}-{}", &name[..LONGEST], String::from_utf8_lossy(&digits))
}

fn claude_conversations(account: &Account, folder: &Path) -> Vec<PastConversation> {
    let dir = account.effective_dir().join("projects").join(claude_folder_name(folder));
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out = Vec::new();
    for path in entries.filter_map(Result::ok).map(|e| e.path()) {
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
        let Ok(file) = std::fs::File::open(&path) else { continue };
        let mut first_prompt = None;
        let mut cwd_ok = None;
        let mut interactive = true;
        for line in BufReader::new(file).lines().map_while(Result::ok).take(800) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
            if cwd_ok.is_none() {
                if let Some(cwd) = value["cwd"].as_str() {
                    cwd_ok = Some(same_folder(cwd, folder));
                    // `claude -p` and SDK runs are scripts, not conversations to resume.
                    interactive = value["entrypoint"].as_str().is_none_or(|e| !e.starts_with("sdk"));
                }
            }
            if first_prompt.is_none() {
                first_prompt = claude_prompt(&value);
            }
            if cwd_ok.is_some() && first_prompt.is_some() {
                break;
            }
        }
        if cwd_ok == Some(false) || !interactive {
            continue;
        }
        let mut title = None;
        let mut last_prompt = None;
        for line in tail_lines(&path, 512 * 1024) {
            if !line.contains("\"ai-title\"") && !line.contains("\"last-prompt\"") {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
            match value["type"].as_str() {
                Some("ai-title") => title = value["aiTitle"].as_str().map(str::to_string).or(title),
                Some("last-prompt") => last_prompt = value["lastPrompt"].as_str().map(str::to_string).or(last_prompt),
                _ => {}
            }
        }
        let Some(title) = title.or_else(|| first_prompt.clone()) else { continue };
        let title = one_line(&title, 90);
        let last_prompt = last_prompt.map(|p| one_line(&p, 120)).filter(|p| *p != title);
        out.push(PastConversation {
            agent: AgentKind::Claude,
            account_id: account.id.clone(),
            id,
            updated: modified(&path),
            path,
            title,
            last_prompt,
        });
    }
    out
}

/// What a running session is about, from its transcript, to name it by: Claude's own title
/// for the conversation (the latest `ai-title`), else the first thing the person typed; for
/// Codex, its first prompt. `None` until there is something.
pub fn session_title(agent: AgentKind, transcript: &Path) -> Option<String> {
    let title = match agent {
        AgentKind::Claude => {
            let mut title = None;
            for line in tail_lines(transcript, 256 * 1024) {
                if !line.contains("\"ai-title\"") {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if value["type"] == "ai-title" {
                        title = value["aiTitle"].as_str().map(str::to_string).or(title);
                    }
                }
            }
            title.or_else(|| {
                let file = std::fs::File::open(transcript).ok()?;
                BufReader::new(file).lines().map_while(Result::ok).take(400).find_map(|line| claude_prompt(&serde_json::from_str::<Value>(&line).ok()?))
            })
        }
        AgentKind::Codex => codex_first_prompt(transcript),
        AgentKind::Shell => None,
    }?;
    Some(one_line(&title, 120)).filter(|t| !t.is_empty())
}

/// Prompts per Codex conversation from `history.jsonl`: (first, last).
fn codex_history(codex_home: &Path) -> HashMap<String, (String, String)> {
    let mut map: HashMap<String, (String, String)> = HashMap::new();
    let Ok(file) = std::fs::File::open(codex_home.join("history.jsonl")) else { return map };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        let (Some(id), Some(text)) = (value["session_id"].as_str(), value["text"].as_str()) else { continue };
        map.entry(id.to_string())
            .and_modify(|e| e.1 = text.to_string())
            .or_insert_with(|| (text.to_string(), text.to_string()));
    }
    map
}

/// The first prompt inside a rollout, for conversations `history.jsonl` does not know.
fn codex_first_prompt(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().map_while(Result::ok).take(400) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        let payload = &value["payload"];
        if value["type"] == "event_msg" && payload["type"] == "user_message" {
            if let Some(text) = payload["message"].as_str().filter(|t| !t.trim().is_empty()) {
                return Some(text.to_string());
            }
        }
        if value["type"] == "response_item" && payload["type"] == "message" && payload["role"] == "user" {
            let text = payload["content"]
                .as_array()
                .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            let text = text.trim();
            // Codex adds its own context as user messages (<environment_context>, AGENTS.md).
            if !text.is_empty() && !text.starts_with('<') && !text.starts_with("# AGENTS.md") {
                return Some(text.to_string());
            }
        }
    }
    None
}

fn codex_conversations(account: &Account, folder: &Path) -> Vec<PastConversation> {
    codex_conversations_in(&account.effective_dir(), &account.id, folder)
}

/// The conversations in one `CODEX_HOME`, of the Glasscode account `account_id`.
fn codex_conversations_in(home: &Path, account_id: &str, folder: &Path) -> Vec<PastConversation> {
    let mut rollouts = Vec::new();
    crate::agents::collect_rollouts(&home.join("sessions"), SystemTime::UNIX_EPOCH, &mut rollouts);
    let mut history: Option<HashMap<String, (String, String)>> = None;
    let mut out = Vec::new();
    for (updated, path) in rollouts {
        let Ok(file) = std::fs::File::open(&path) else { continue };
        let mut first = String::new();
        if BufReader::new(file).read_line(&mut first).is_err() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&first) else { continue };
        let meta = &value["payload"];
        if value["type"] != "session_meta" || !meta["cwd"].as_str().is_some_and(|c| same_folder(c, folder)) {
            continue;
        }
        // Only conversations a person had: not `codex exec` runs, sub-agents, or Codex runs
        // that another tool (e.g. Claude Code's Codex plugin) started.
        let source = meta["source"].as_str().unwrap_or("");
        let thread_source = meta["thread_source"].as_str().unwrap_or("user");
        let originator = meta["originator"].as_str().unwrap_or("");
        if !matches!(source, "cli" | "vscode") || thread_source != "user" || originator == "Claude Code" {
            continue;
        }
        let Some(id) = meta["id"].as_str().map(str::to_string) else { continue };
        let prompts = history.get_or_insert_with(|| codex_history(home)).get(&id).cloned();
        let (first_prompt, last) = match prompts {
            Some((first, last)) => (Some(first), Some(last)),
            None => (codex_first_prompt(&path), None),
        };
        let Some(first_prompt) = first_prompt else { continue };
        let title = one_line(&first_prompt, 90);
        let last_prompt = last.map(|p| one_line(&p, 120)).filter(|p| *p != title);
        out.push(PastConversation { agent: AgentKind::Codex, account_id: account_id.to_string(), id, path, title, last_prompt, updated });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manager's own conversations are not offered to resume, even one that looks like the
    /// person's (exec-28); a project's are.
    #[test]
    fn manager_conversations_hidden() {
        let root = crate::paths::use_test_root();
        let account = Account { id: "claude-test".into(), agent: AgentKind::Claude, label: "test".into(), plan: None, system_default: false };
        let work = crate::paths::manager_work_dir();
        let project = root.join("code").join("app");
        for (folder, id) in [(&work, "m1"), (&project, "p1")] {
            let dir = account.effective_dir().join("projects").join(claude_folder_name(folder));
            std::fs::create_dir_all(&dir).unwrap();
            let line = serde_json::json!({ "type": "user", "cwd": folder, "entrypoint": "cli", "message": { "role": "user", "content": "what is running?" } });
            std::fs::write(dir.join(format!("{id}.jsonl")), format!("{line}\n")).unwrap();
        }
        assert!(conversations(AgentKind::Claude, std::slice::from_ref(&account), &work).is_empty());
        assert_eq!(conversations(AgentKind::Claude, std::slice::from_ref(&account), &project).iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["p1"]);
        assert!(is_managers(&work) && is_managers(&work.join("x")) && is_managers(&crate::paths::manager_run_dir()) && !is_managers(&project));
        let outside = active_outside(std::slice::from_ref(&account), &[work.clone(), project.clone()], &Default::default(), SystemTime::UNIX_EPOCH);
        assert_eq!(outside.iter().map(|(_, c)| c.id.as_str()).collect::<Vec<_>>(), ["p1"]);
    }

    #[test]
    fn claude_folder_names_are_the_ones_claude_code_makes() {
        // The names that Claude Code 2.1.325 made on Windows for these work folders.
        assert_eq!(claude_folder_name(Path::new(r"C:\Users\dev\app")), "C--Users-dev-app");
        assert_eq!(claude_folder_name(Path::new(r"Q:\Dev App.v2")), "Q--Dev-App-v2");
        // One character outside the BMP is two UTF-16 units, so two dashes.
        assert_eq!(claude_folder_name(Path::new("Q:\\caf\u{e9}-\u{1F600}")), "Q--caf----");
        let long = format!(r"Q:\worktrees\feature-{}", "x".repeat(200));
        assert_eq!(claude_folder_name(Path::new(&long)), format!("Q--worktrees-feature-{}-4o1jfi", "x".repeat(179)));
        // As on the Mac.
        assert_eq!(claude_folder_name(Path::new("/Users/dev/app/.glasscode/worktrees/feat-x")), "-Users-dev-app--glasscode-worktrees-feat-x");
    }

    #[cfg(windows)]
    #[test]
    fn a_conversation_is_of_the_folder_in_any_spelling_on_windows() {
        // Claude keeps the work folder as it was given; a person may have typed it otherwise.
        assert!(same_folder(r"c:\users\dev\app", Path::new(r"C:\Users\Dev\App")));
        assert!(same_folder(r"C:\Users\Dev\App", Path::new(r"\\?\C:\Users\Dev\App")));
        assert!(!same_folder(r"C:\Users\Dev\App.x", Path::new(r"C:\Users\Dev\App")));
    }

    #[cfg(windows)]
    #[test]
    fn a_codex_conversation_of_the_folder_is_listed_in_any_spelling_on_windows() {
        // A header as Codex 0.153 writes it on Windows: the folder as it was given to Codex.
        let home = tempfile::tempdir().unwrap();
        let day = home.path().join("sessions").join("2026").join("11").join("15");
        std::fs::create_dir_all(&day).unwrap();
        let meta = serde_json::json!({"type": "session_meta", "payload": {"id": "w1", "cwd": r"C:\Users\Dev\App", "source": "cli", "originator": "codex-tui", "thread_source": "user"}});
        let prompt = serde_json::json!({"type": "event_msg", "payload": {"type": "user_message", "message": "fix the build on Windows"}});
        std::fs::write(day.join("rollout-2026-11-15T10-00-00-w1.jsonl"), format!("{meta}\n{prompt}\n")).unwrap();
        for folder in [r"c:\users\dev\app", r"\\?\C:\Users\Dev\App\", "C:/Users/Dev/App"] {
            let list = codex_conversations_in(home.path(), "codex-work", Path::new(folder));
            let found: Vec<(&str, &str)> = list.iter().map(|c| (c.id.as_str(), c.title.as_str())).collect();
            assert_eq!(found, [("w1", "fix the build on Windows")], "{folder}");
        }
        assert!(codex_conversations_in(home.path(), "codex-work", Path::new(r"C:\Users\Dev\App2")).is_empty());
    }
}
