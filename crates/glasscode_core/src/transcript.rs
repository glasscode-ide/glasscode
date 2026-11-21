//! A session's conversation for the phone's chat view, read from the agent's own files:
//! Claude's transcript (`<config>/projects/<folder>/<id>.jsonl`) and Codex's rollout
//! (`<codex home>/sessions/…/rollout-….jsonl`). Thinking, injected context and sub-agents are
//! left out; tool calls become one-line items the phone folds together.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::AgentKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptItem {
    pub kind: ItemKind,
    /// The message; for a tool, its gist ("bun test services/iam", "src/assign-role.ts").
    pub text: String,
    /// The tool's name (Read, Edit, Bash, exec_command, apply_patch, …).
    #[serde(default)]
    pub tool: Option<String>,
    /// The tool reported an error.
    #[serde(default)]
    pub failed: bool,
    #[serde(default)]
    pub time: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TranscriptPage {
    pub items: Vec<TranscriptItem>,
    /// Where the next page starts (a byte offset in the file).
    pub cursor: u64,
    /// This page starts over (first page, or the file was replaced): drop earlier items.
    pub reset: bool,
}

/// How far back a first page reads.
const FIRST_PAGE_BYTES: u64 = 6 * 1024 * 1024;

/// Items after `after` (a cursor from an earlier page), or the last `max_items` when `None`.
pub fn read(agent: AgentKind, path: &Path, after: Option<u64>, max_items: usize) -> anyhow::Result<TranscriptPage> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let (start, reset) = match after {
        Some(cursor) if cursor <= len => (cursor, false),
        _ => (len.saturating_sub(FIRST_PAGE_BYTES), true),
    };
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(len - start).read_to_end(&mut bytes)?;
    // Whole lines only: a line still being written comes with the next page.
    let end = bytes.iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
    let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if reset && start > 0 {
        if let Some(i) = text.find('\n') {
            text.drain(..=i);
        }
    }
    let mut items = Vec::new();
    let mut tools: HashMap<String, usize> = HashMap::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
        match agent {
            AgentKind::Codex => codex_line(&value, &mut items),
            _ => claude_line(&value, &mut items, &mut tools),
        }
    }
    if items.len() > max_items {
        items.drain(..items.len() - max_items);
    }
    Ok(TranscriptPage { items, cursor: start + end as u64, reset })
}

/// The agent's last reply, for a session list ("Push and update the PR?").
pub fn last_reply(agent: AgentKind, path: &Path) -> Option<String> {
    let len = std::fs::metadata(path).ok()?.len();
    let page = read(agent, path, Some(len.saturating_sub(512 * 1024)), 400).ok()?;
    let last = page.items.iter().rposition(|i| i.kind != ItemKind::Tool)?;
    let item = &page.items[last];
    (item.kind == ItemKind::Assistant).then(|| clip(&item.text, 600))
}

/// What lasts of a conversation once its agent deletes the transcript (M24, P12): the first prompt,
/// the last replies, the tools that failed, within [`DIGEST_BYTES`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Digest {
    pub first_prompt: Option<String>,
    /// The last replies, oldest first, at most [`DIGEST_REPLIES`].
    pub replies: Vec<String>,
    /// The tools that failed ("Bash: bun test services/iam"), at most [`DIGEST_REPLIES`].
    pub failed_tools: Vec<String>,
}

/// The most replies a digest keeps.
pub const DIGEST_REPLIES: usize = 20;
/// The most bytes a digest holds.
pub const DIGEST_BYTES: usize = 20 * 1024;

impl Digest {
    /// Its size in bytes, as it is written.
    pub fn bytes(&self) -> usize {
        self.first_prompt.as_ref().map_or(0, String::len) + self.replies.iter().chain(&self.failed_tools).map(|t| t.len() + 1).sum::<usize>()
    }
}

/// The digest of a conversation: its first prompt (from the start of the file), and its last
/// replies and failed tools (from its end), each reply clipped, the oldest replies dropped until it
/// is within [`DIGEST_BYTES`]. None when the file cannot be read.
pub fn digest(agent: AgentKind, path: &Path) -> Option<Digest> {
    let first_prompt = {
        let mut file = std::fs::File::open(path).ok()?;
        let mut head = Vec::new();
        (&mut file).take(512 * 1024).read_to_end(&mut head).ok()?;
        let end = head.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        let text = String::from_utf8_lossy(&head[..end]).into_owned();
        let mut items = Vec::new();
        let mut tools = HashMap::new();
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
            match agent {
                AgentKind::Codex => codex_line(&value, &mut items),
                _ => claude_line(&value, &mut items, &mut tools),
            }
            if items.iter().any(|i: &TranscriptItem| i.kind == ItemKind::User) {
                break;
            }
        }
        items.into_iter().find(|i| i.kind == ItemKind::User).map(|i| clip(&i.text, 2_000))
    };
    let page = read(agent, path, None, 4_000).ok()?;
    let mut replies: Vec<String> = page.items.iter().filter(|i| i.kind == ItemKind::Assistant).map(|i| clip(&i.text, 1_500)).collect();
    if replies.len() > DIGEST_REPLIES {
        replies.drain(..replies.len() - DIGEST_REPLIES);
    }
    let mut failed_tools: Vec<String> = page.items.iter().filter(|i| i.kind == ItemKind::Tool && i.failed).map(|i| clip(&format!("{}: {}", i.tool.as_deref().unwrap_or("tool"), i.text), 300)).collect();
    if failed_tools.len() > DIGEST_REPLIES {
        failed_tools.drain(..failed_tools.len() - DIGEST_REPLIES);
    }
    let mut out = Digest { first_prompt, replies, failed_tools };
    while out.bytes() > DIGEST_BYTES {
        if !out.replies.is_empty() {
            out.replies.remove(0);
        } else if !out.failed_tools.is_empty() {
            out.failed_tools.remove(0);
        } else {
            out.first_prompt = out.first_prompt.map(|p| clip(&p, DIGEST_BYTES / 2));
            break;
        }
    }
    Some(out)
}

pub(crate) fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max).collect::<String>().trim_end())
}

pub(crate) fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// Last path components, enough to recognise a file on a phone. A Windows path (`C:\a\b.rs`, as
/// Claude writes it there) divides at `\` and keeps it.
pub(crate) fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    let separator = if path.contains('/') || !path.contains('\\') { "/" } else { "\\" };
    parts[parts.len().saturating_sub(3)..].join(separator)
}

pub(crate) fn first_line(text: &str) -> String {
    clip(text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default(), 200)
}

// ----- Claude ----------------------------------------------------------------------------------

/// What a person typed, without Claude Code's wrappers; `None` for lines that are not theirs.
/// A message without the marks Claude Code puts around text that was pasted into its box
/// (`<pasted_content id="ed38">` … `</pasted_content id="ed38">`): the words are the person's, the
/// marks are not. Glasscode's chat sends each message as a paste, so its own messages come back
/// with them.
pub(crate) fn unwrap_pasted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let at = [rest.find("<pasted_content"), rest.find("</pasted_content")].into_iter().flatten().min();
        // A mark ends at its ">" on the same line; anything else is the person's own text.
        let Some((at, end)) = at.and_then(|at| rest[at..].find('>').filter(|end| !rest[at..at + end].contains('\n')).map(|end| (at, at + end + 1))) else {
            out.push_str(rest);
            return out.trim().to_string();
        };
        out.push_str(&rest[..at]);
        rest = &rest[end..];
    }
}

pub(crate) fn claude_user_text(text: &str) -> Option<String> {
    let text = unwrap_pasted(text);
    let t = text.trim();
    if t.is_empty() || t.starts_with("<local-command-") || t.starts_with("<system-reminder>") || t.starts_with("Caveat: ") || t.starts_with("<bash-") {
        return None;
    }
    if let Some(rest) = t.strip_prefix("<command-name>") {
        // A slash command: "/compact focus on the tests".
        let name = rest.split("</command-name>").next().unwrap_or_default().trim();
        let args = t.split("<command-args>").nth(1).and_then(|a| a.split("</command-args>").next()).unwrap_or_default().trim();
        return Some(if args.is_empty() { name.to_string() } else { format!("{name} {args}") });
    }
    Some(clip(t, 6000))
}

fn claude_tool_gist(name: &str, input: &Value) -> String {
    let gist = match name {
        "Bash" => first_line(str_of(input, "command")),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => short_path(input["file_path"].as_str().or(input["notebook_path"].as_str()).unwrap_or_default()),
        "Grep" | "Glob" => str_of(input, "pattern").to_string(),
        "WebFetch" => str_of(input, "url").to_string(),
        "WebSearch" => str_of(input, "query").to_string(),
        "Task" | "Agent" => str_of(input, "description").to_string(),
        "TodoWrite" => "the plan".to_string(),
        "Skill" => str_of(input, "skill").to_string(),
        _ => input.as_object().and_then(|o| o.values().find_map(Value::as_str)).map(first_line).unwrap_or_default(),
    };
    clip(&gist, 200)
}

fn claude_line(value: &Value, items: &mut Vec<TranscriptItem>, tools: &mut HashMap<String, usize>) {
    let kind = str_of(value, "type");
    let flag = |key: &str| value[key].as_bool() == Some(true);
    if !matches!(kind, "user" | "assistant") || flag("isMeta") || flag("isSidechain") || flag("isCompactSummary") || flag("isVisibleInTranscriptOnly") {
        return;
    }
    let time = value["timestamp"].as_str().map(str::to_string);
    let push = |items: &mut Vec<TranscriptItem>, kind: ItemKind, text: String, tool: Option<String>| {
        items.push(TranscriptItem { kind, text, tool, failed: false, time: time.clone() });
    };
    let content = &value["message"]["content"];
    if kind == "user" {
        if let Some(text) = content.as_str() {
            if let Some(text) = claude_user_text(text) {
                push(items, ItemKind::User, text, None);
            }
            return;
        }
        for block in content.as_array().into_iter().flatten() {
            match str_of(block, "type") {
                "text" => {
                    if let Some(text) = claude_user_text(str_of(block, "text")) {
                        push(items, ItemKind::User, text, None);
                    }
                }
                "image" => push(items, ItemKind::User, "[image]".into(), None),
                "tool_result" if block["is_error"].as_bool() == Some(true) => {
                    if let Some(&i) = tools.get(str_of(block, "tool_use_id")) {
                        items[i].failed = true;
                    }
                }
                _ => {}
            }
        }
        return;
    }
    for block in content.as_array().into_iter().flatten() {
        match str_of(block, "type") {
            "text" => {
                let text = str_of(block, "text").trim();
                if !text.is_empty() {
                    push(items, ItemKind::Assistant, clip(text, 12000), None);
                }
            }
            "tool_use" => {
                let name = str_of(block, "name");
                tools.insert(str_of(block, "id").to_string(), items.len());
                push(items, ItemKind::Tool, claude_tool_gist(name, &block["input"]), Some(name.to_string()));
            }
            _ => {}
        }
    }
}

// ----- Codex -----------------------------------------------------------------------------------

/// Codex records its own context (environment, AGENTS.md, permissions) as user messages.
pub(crate) fn codex_injected(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("# AGENTS.md") || (t.starts_with('<') && t.lines().next().is_some_and(|l| l.trim_end().ends_with('>')))
}

fn codex_command(arguments: &Value) -> String {
    match &arguments["command"] {
        // ["bash", "-lc", "bun test"]
        Value::Array(parts) => parts.last().and_then(Value::as_str).map(first_line).unwrap_or_default(),
        Value::String(s) => first_line(s),
        _ => arguments["cmd"].as_str().map(first_line).unwrap_or_default(),
    }
}

/// Files an `apply_patch` touches: "*** Update File: src/a.ts".
fn patched_files(patch: &str) -> String {
    let files: Vec<String> = patch
        .lines()
        .filter_map(|l| l.strip_prefix("*** Update File: ").or_else(|| l.strip_prefix("*** Add File: ")).or_else(|| l.strip_prefix("*** Delete File: ")))
        .map(|f| short_path(f.trim()))
        .collect();
    if files.is_empty() { first_line(patch) } else { files.join(", ") }
}

fn codex_line(value: &Value, items: &mut Vec<TranscriptItem>) {
    if str_of(value, "type") != "response_item" {
        return;
    }
    let payload = &value["payload"];
    let time = value["timestamp"].as_str().map(str::to_string);
    let mut push = |kind: ItemKind, text: String, tool: Option<String>| {
        if !text.trim().is_empty() {
            items.push(TranscriptItem { kind, text, tool, failed: false, time: time.clone() });
        }
    };
    let joined = |kinds: &[&str]| -> String {
        payload["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| kinds.contains(&str_of(c, "type")))
            .map(|c| str_of(c, "text"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match str_of(payload, "type") {
        "message" => match str_of(payload, "role") {
            "user" => {
                let text = joined(&["input_text", "text"]);
                if !codex_injected(&text) {
                    push(ItemKind::User, clip(&text, 6000), None);
                }
            }
            "assistant" => push(ItemKind::Assistant, clip(&joined(&["output_text", "text"]), 12000), None),
            _ => {}
        },
        "function_call" => {
            let name = str_of(payload, "name");
            let arguments: Value = serde_json::from_str(str_of(payload, "arguments")).unwrap_or(Value::Null);
            let gist = match name {
                "apply_patch" => patched_files(arguments["input"].as_str().unwrap_or_default()),
                _ => codex_command(&arguments),
            };
            push(ItemKind::Tool, if gist.is_empty() { name.to_string() } else { gist }, Some(name.to_string()));
        }
        "custom_tool_call" => {
            let name = str_of(payload, "name");
            let input = str_of(payload, "input");
            let gist = if name == "apply_patch" || input.contains("*** Begin Patch") { patched_files(input) } else { first_line(input) };
            push(ItemKind::Tool, gist, Some(name.to_string()));
        }
        "local_shell_call" => {
            let command = payload["action"]["command"].as_array().and_then(|c| c.last()).and_then(Value::as_str).map(first_line).unwrap_or_default();
            push(ItemKind::Tool, command, Some("shell".into()));
        }
        _ => {}
    }
}

#[cfg(test)]
mod digest_tests {
    use super::*;

    #[test]
    fn digest_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut lines = vec![serde_json::json!({"type": "user", "message": {"role": "user", "content": "Fix the CSV export timeouts"}}).to_string()];
        for n in 0..200 {
            lines.push(serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": format!("reply {n} {}", "x".repeat(3_000))}]}}).to_string());
        }
        lines.push(serde_json::json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "bun test"}}]}}).to_string());
        lines.push(serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1", "is_error": true, "content": "1 fail"}]}}).to_string());
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let kept = digest(AgentKind::Claude, &path).unwrap();
        assert_eq!(kept.first_prompt.as_deref(), Some("Fix the CSV export timeouts"));
        assert!(kept.bytes() <= DIGEST_BYTES, "{}", kept.bytes());
        assert!(kept.replies.len() <= DIGEST_REPLIES && !kept.replies.is_empty());
        assert!(kept.replies.last().unwrap().starts_with("reply 199"), "the newest replies stay");
        assert_eq!(kept.failed_tools.len(), 1, "{:?}", kept.failed_tools);
        assert!(digest(AgentKind::Claude, &dir.path().join("gone.jsonl")).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(lines: &[Value]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for line in lines {
            use std::io::Write;
            writeln!(file, "{line}").unwrap();
        }
        file
    }

    #[test]
    fn reads_a_claude_transcript() {
        let file = write(&[
            serde_json::json!({"type": "user", "timestamp": "2026-10-16T09:00:00Z", "message": {"role": "user", "content": "Fix nested group roles"}}),
            serde_json::json!({"type": "user", "isMeta": true, "message": {"role": "user", "content": [{"type": "text", "text": "injected"}]}}),
            serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "Looking at the sync."},
                {"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/home/dev/app/services/iam/sync-groups.ts"}},
                {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "bun test services/iam\necho done"}}
            ]}}),
            serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t2", "is_error": true, "content": "1 fail"}]}}),
            serde_json::json!({"type": "user", "message": {"content": "<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args>keep tests</command-args>"}}),
            serde_json::json!({"type": "assistant", "isSidechain": true, "message": {"content": [{"type": "text", "text": "sub-agent"}]}}),
            serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "Ready to push?"}]}}),
        ]);
        let page = read(AgentKind::Claude, file.path(), None, 100).unwrap();
        let summary: Vec<(ItemKind, &str, bool)> = page.items.iter().map(|i| (i.kind, i.text.as_str(), i.failed)).collect();
        assert_eq!(
            summary,
            vec![
                (ItemKind::User, "Fix nested group roles", false),
                (ItemKind::Assistant, "Looking at the sync.", false),
                (ItemKind::Tool, "services/iam/sync-groups.ts", false),
                (ItemKind::Tool, "bun test services/iam", true),
                (ItemKind::User, "/compact keep tests", false),
                (ItemKind::Assistant, "Ready to push?", false),
            ]
        );
        assert!(page.reset);
        assert_eq!(last_reply(AgentKind::Claude, file.path()).as_deref(), Some("Ready to push?"));
        // Nothing new after the cursor.
        let next = read(AgentKind::Claude, file.path(), Some(page.cursor), 100).unwrap();
        assert!(next.items.is_empty() && !next.reset && next.cursor == page.cursor);
    }

    #[test]
    fn reads_a_codex_rollout() {
        let file = write(&[
            serde_json::json!({"type": "session_meta", "payload": {"id": "x"}}),
            serde_json::json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>\n  <cwd>/x</cwd>\n</environment_context>"}]}}),
            serde_json::json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Give the icons a safe zone"}]}}),
            serde_json::json!({"type": "response_item", "payload": {"type": "function_call", "name": "shell", "arguments": "{\"command\":[\"bash\",\"-lc\",\"bun test\"]}"}}),
            serde_json::json!({"type": "response_item", "payload": {"type": "custom_tool_call", "name": "apply_patch", "input": "*** Begin Patch\n*** Update File: apps/web/public/manifest.json\n@@\n*** End Patch"}}),
            serde_json::json!({"type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Push and update the PR?"}]}}),
        ]);
        let page = read(AgentKind::Codex, file.path(), None, 100).unwrap();
        let texts: Vec<&str> = page.items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, vec!["Give the icons a safe zone", "bun test", "web/public/manifest.json", "Push and update the PR?"]);
        assert_eq!(page.items[1].tool.as_deref(), Some("shell"));
    }

    #[test]
    fn a_short_path_is_the_last_three_parts_with_either_separator() {
        assert_eq!(short_path(r"C:\a\b\c\d.rs"), r"b\c\d.rs");
        assert_eq!(short_path("/home/dev/app/services/iam/sync-groups.ts"), "services/iam/sync-groups.ts");
        assert_eq!(short_path("C:/a/b/c/d.rs"), "b/c/d.rs");
        assert_eq!(short_path(r"src\main.rs"), r"src\main.rs");
        assert_eq!(short_path(""), "");
    }
}

#[cfg(test)]
mod real_files {
    /// `TRANSCRIPT_CLAUDE=… TRANSCRIPT_CODEX=… cargo test -p glasscode_core real_files -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn reads_real_files() {
        for (var, agent) in [("TRANSCRIPT_CLAUDE", crate::model::AgentKind::Claude), ("TRANSCRIPT_CODEX", crate::model::AgentKind::Codex)] {
            let Ok(path) = std::env::var(var) else { continue };
            let started = std::time::Instant::now();
            let page = super::read(agent, std::path::Path::new(&path), None, 100_000).unwrap();
            let count = |k| page.items.iter().filter(|i| i.kind == k).count();
            let tools: std::collections::BTreeSet<_> = page.items.iter().filter_map(|i| i.tool.clone()).collect();
            println!(
                "{var}: {} items (user {}, assistant {}, tool {}, failed {}) in {:?}; tools {:?}; last reply {} chars",
                page.items.len(),
                count(super::ItemKind::User),
                count(super::ItemKind::Assistant),
                count(super::ItemKind::Tool),
                page.items.iter().filter(|i| i.failed).count(),
                started.elapsed(),
                tools,
                super::last_reply(agent, std::path::Path::new(&path)).map(|s| s.len()).unwrap_or(0)
            );
        }
    }
}
