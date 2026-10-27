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

/// Conversations of `agent` started in `folder`, from every account, newest first.
pub fn conversations(agent: AgentKind, accounts: &[Account], folder: &Path) -> Vec<PastConversation> {
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

fn same_folder(cwd: &str, folder: &Path) -> bool {
    let cwd = Path::new(cwd);
    cwd == folder || folder.canonicalize().is_ok_and(|real| cwd == real) || cwd.canonicalize().is_ok_and(|real| real == folder)
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

pub fn claude_folder_name(folder: &Path) -> String {
    folder.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
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
    let home = account.effective_dir();
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
        let prompts = history.get_or_insert_with(|| codex_history(&home)).get(&id).cloned();
        let (first_prompt, last) = match prompts {
            Some((first, last)) => (Some(first), Some(last)),
            None => (codex_first_prompt(&path), None),
        };
        let Some(first_prompt) = first_prompt else { continue };
        let title = one_line(&first_prompt, 90);
        let last_prompt = last.map(|p| one_line(&p, 120)).filter(|p| *p != title);
        out.push(PastConversation { agent: AgentKind::Codex, account_id: account.id.clone(), id, path, title, last_prompt, updated });
    }
    out
}
