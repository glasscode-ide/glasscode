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

/// Last path components, enough to recognise a file on a phone.
pub(crate) fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    parts[parts.len().saturating_sub(3)..].join("/")
}

pub(crate) fn first_line(text: &str) -> String {
    clip(text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default(), 200)
}

// ----- Claude ----------------------------------------------------------------------------------

/// What a person typed, without Claude Code's wrappers; `None` for lines that are not theirs.
pub(crate) fn claude_user_text(text: &str) -> Option<String> {
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
