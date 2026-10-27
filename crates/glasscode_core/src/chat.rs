//! A session's conversation for the desktop Chat view, read from the agent's own files while
//! the official CLI keeps running in its terminal: Claude's transcript and Codex's rollout (see
//! `transcript`, which serves the phone a simpler version of the same thing).
//!
//! Each line becomes chat entries: what the person (or Glasscode) sent, the agent's replies,
//! its tool calls with their output, diffs and screenshots, its plan, and quiet notices
//! ("Conversation compacted", "Worked for 2m 14s"). Thinking and injected context are left out.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::model::AgentKind;
use crate::transcript::{claude_user_text, clip, codex_injected, first_line, short_path, str_of};

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// A message to the agent. `from_glasscode` is why Glasscode sent it ("CI failed; ask 1 of 3").
    User { text: String, images: Vec<Image>, from_glasscode: Option<String> },
    /// The agent's reply, in Markdown.
    Text(String),
    Tool(Tool),
    /// The agent's plan (Claude's to-do list); a later one replaces it.
    Todos { items: Vec<Todo>, superseded: bool },
    /// A plan the agent asks to carry out (Claude's plan mode).
    Plan(String),
    /// Something that happened between turns, shown small.
    Notice(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub id: String,
    /// What it did, in words ("Ran", "Edited", "Read") ...
    pub verb: String,
    /// ... and on what ("pnpm test", "src/auth.ts"), shown in the code font when `code`.
    pub subject: String,
    pub code: bool,
    /// A short result next to it ("+38 −12", "14 lines", "failed").
    pub meta: Option<String>,
    pub failed: bool,
    pub done: bool,
    /// Its output or diff, shown when the row is opened.
    pub detail: Vec<Line>,
    /// Screenshots and other images it returned, shown under the row.
    pub images: Vec<Image>,
    /// Rows of the same kind next to each other fold into one ("Read 3 files").
    pub group: Option<Group>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    Read,
    Search,
    List,
    /// Commands run one after another ("Ran 4 commands").
    Run,
}

impl Group {
    /// Reading, searching and listing fold together; commands only with commands.
    fn joins(self, other: Group) -> bool {
        (self == Group::Run) == (other == Group::Run)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: String,
    pub kind: LineKind,
    /// Its number in the file, for diffs.
    pub number: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Plain,
    Added,
    Removed,
    Hunk,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
    pub text: String,
    pub state: TodoState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoState {
    Pending,
    Active,
    Done,
}

/// An image as the agent stored it (base64), decoded by whoever draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub media_type: String,
    pub base64: Arc<String>,
}

impl Image {
    /// The image file's bytes.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.decode(self.base64.as_bytes()).ok()
    }
}

/// How Claude's voice mode listens, when it is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceMode {
    /// Hold the key while talking (the default).
    Hold,
    /// Tap to start, tap again to stop and send.
    Tap,
}

/// Claude's voice mode in a config folder's `settings.json` (`/voice` writes it there), or
/// `None` when it is off. Only that setting is read.
pub fn claude_voice(config_dir: &Path) -> Option<VoiceMode> {
    let settings: Value = serde_json::from_slice(&std::fs::read(config_dir.join("settings.json")).ok()?).ok()?;
    let on = settings["voice"]["enabled"].as_bool().or_else(|| settings["voiceEnabled"].as_bool()).unwrap_or(false);
    on.then(|| if settings["voice"]["mode"].as_str() == Some("tap") { VoiceMode::Tap } else { VoiceMode::Hold })
}

/// What the Chat view draws: an entry, or several tool rows folded into one.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    One(usize),
    Group { group: Group, entries: Vec<usize>, label: String },
}

/// How far back the first read goes; a longer conversation starts with a notice.
const FIRST_BYTES: u64 = 4 * 1024 * 1024;
const DETAIL_LINES: usize = 60;
const DETAIL_LINE_CHARS: usize = 400;

/// A session's conversation, kept up to date by `poll`.
pub struct ChatLog {
    pub agent: AgentKind,
    pub path: PathBuf,
    offset: u64,
    pub entries: Vec<Entry>,
    /// Tool calls by id, for their results.
    tools: HashMap<String, usize>,
    /// The latest to-do list, replaced by the next.
    todos: Option<usize>,
    /// Earlier conversation was left out (the file is long).
    pub truncated: bool,
    /// The model of the latest reply ("claude-opus-5-5", "gpt-6-astra").
    pub model: Option<String>,
    /// Codex: how full the context is (0-100), from its token counts.
    pub context_percent: Option<f64>,
    /// Messages sent while Claude works, waiting in its queue (it reads them between steps).
    pub queued: Vec<String>,
    /// Bumped on every change, so a view knows when to redraw.
    pub generation: u64,
}

impl ChatLog {
    pub fn open(agent: AgentKind, path: &Path) -> Self {
        let mut log = ChatLog {
            agent,
            path: path.to_path_buf(),
            offset: 0,
            entries: Vec::new(),
            tools: HashMap::new(),
            todos: None,
            truncated: false,
            model: None,
            context_percent: None,
            queued: Vec::new(),
            generation: 0,
        };
        log.poll();
        log
    }

    /// Read what was added since last time; true when something changed.
    pub fn poll(&mut self) -> bool {
        let Ok(mut file) = std::fs::File::open(&self.path) else { return false };
        let Ok(len) = file.metadata().map(|m| m.len()) else { return false };
        if len < self.offset {
            // Replaced (or truncated): start over.
            *self = ChatLog { generation: self.generation + 1, ..ChatLog::open_empty(self.agent, &self.path) };
        }
        if len == self.offset {
            return false;
        }
        let first = self.offset == 0 && self.entries.is_empty();
        let start = if first && len > FIRST_BYTES { len - FIRST_BYTES } else { self.offset };
        if file.seek(SeekFrom::Start(start)).is_err() {
            return false;
        }
        let mut bytes = Vec::new();
        if file.take(len - start).read_to_end(&mut bytes).is_err() {
            return false;
        }
        // Whole lines only: a line still being written comes next time.
        let Some(end) = bytes.iter().rposition(|&b| b == b'\n').map(|i| i + 1) else { return false };
        let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
        if start > self.offset {
            self.truncated = true;
            if let Some(i) = text.find('\n') {
                text.drain(..=i);
            }
        }
        self.offset = start + end as u64;
        let mut changed = false;
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
            match self.agent {
                AgentKind::Codex => self.codex_line(&value),
                _ => self.claude_line(&value),
            }
            changed = true;
        }
        if changed {
            self.generation += 1;
        }
        changed
    }

    fn open_empty(agent: AgentKind, path: &Path) -> Self {
        ChatLog { agent, path: path.to_path_buf(), offset: 0, entries: Vec::new(), tools: HashMap::new(), todos: None, truncated: false, model: None, context_percent: None, queued: Vec::new(), generation: 0 }
    }

    /// The tool still running (asked, no result yet), latest first: what a permission
    /// question is about.
    pub fn pending_tool(&self) -> Option<&Tool> {
        self.entries.iter().rev().find_map(|e| match e {
            Entry::Tool(t) if !t.done => Some(t),
            _ => None,
        })
    }

    fn push_tool(&mut self, tool: Tool) {
        self.tools.insert(tool.id.clone(), self.entries.len());
        self.entries.push(Entry::Tool(tool));
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut Tool> {
        let i = *self.tools.get(id)?;
        match self.entries.get_mut(i) {
            Some(Entry::Tool(t)) => Some(t),
            _ => None,
        }
    }

    fn push_todos(&mut self, items: Vec<Todo>) {
        if let Some(Entry::Todos { superseded, .. }) = self.todos.and_then(|i| self.entries.get_mut(i)) {
            *superseded = true;
        }
        self.todos = Some(self.entries.len());
        self.entries.push(Entry::Todos { items, superseded: false });
    }

    fn push_user(&mut self, text: &str, images: Vec<Image>) {
        let text = text.trim();
        if text.is_empty() && images.is_empty() {
            return;
        }
        let (text, from_glasscode) = split_glasscode(text);
        self.entries.push(Entry::User { text, images, from_glasscode });
    }

    // ----- Claude ------------------------------------------------------------------------------

    fn claude_line(&mut self, value: &Value) {
        let kind = str_of(value, "type");
        let flag = |key: &str| value[key].as_bool() == Some(true);
        if flag("isSidechain") {
            return;
        }
        // A message sent while Claude works: queued first, then taken in between two steps
        // (as a `queued_command` attachment, not a user message).
        if kind == "queue-operation" {
            match str_of(value, "operation") {
                "enqueue" => {
                    if let Some(text) = value["content"].as_str().filter(|t| !t.trim_start().starts_with('<')) {
                        self.queued.push(text.to_string());
                    }
                }
                "remove" => {
                    let text = str_of(value, "content");
                    if let Some(i) = self.queued.iter().position(|q| q == text) {
                        self.queued.remove(i);
                    }
                }
                "dequeue" => {
                    if !self.queued.is_empty() {
                        self.queued.remove(0);
                    }
                }
                "popAll" => self.queued.clear(),
                _ => {}
            }
            return;
        }
        if kind == "attachment" {
            let attachment = &value["attachment"];
            if str_of(attachment, "type") == "queued_command" {
                let prompt = &attachment["prompt"];
                match str_of(attachment, "commandMode") {
                    "task-notification" => {
                        let text = prompt.as_str().unwrap_or_default();
                        self.entries.push(Entry::Notice(clip(&tag(text, "summary").unwrap_or_else(|| "A background task finished".into()), 300)));
                    }
                    _ => match prompt {
                        Value::String(text) => self.claude_user_text(text, Vec::new()),
                        Value::Array(blocks) => {
                            let texts: Vec<String> = blocks.iter().filter(|b| str_of(b, "type") == "text").map(|b| str_of(b, "text").to_string()).collect();
                            let images = blocks.iter().filter_map(image_of).collect();
                            self.claude_user_text(&texts.join("\n\n"), images);
                        }
                        _ => {}
                    },
                }
            }
            return;
        }
        if kind == "system" {
            match str_of(value, "subtype") {
                "compact_boundary" => self.entries.push(Entry::Notice("Conversation compacted".into())),
                "turn_duration" => {
                    if let Some(ms) = value["durationMs"].as_u64().filter(|ms| *ms >= 1000) {
                        self.entries.push(Entry::Notice(format!("Worked for {}", duration(ms))));
                    }
                }
                _ => {}
            }
            return;
        }
        if !matches!(kind, "user" | "assistant") || flag("isMeta") || flag("isCompactSummary") || flag("isVisibleInTranscriptOnly") {
            return;
        }
        let message = &value["message"];
        let content = &message["content"];
        if kind == "assistant" {
            if let Some(model) = message["model"].as_str().filter(|m| !m.starts_with('<')) {
                self.model = Some(model.to_string());
            }
            for block in content.as_array().into_iter().flatten() {
                match str_of(block, "type") {
                    "text" => {
                        let text = str_of(block, "text").trim();
                        if !text.is_empty() && text != "(no content)" {
                            self.entries.push(Entry::Text(text.to_string()));
                        }
                    }
                    "tool_use" => self.claude_tool_use(block),
                    _ => {}
                }
            }
            return;
        }
        // The person's message, Claude Code's notes about commands, or tool results.
        if let Some(text) = content.as_str() {
            self.claude_user_text(text, Vec::new());
            return;
        }
        let mut texts = Vec::new();
        let mut images = Vec::new();
        for block in content.as_array().into_iter().flatten() {
            match str_of(block, "type") {
                "text" => texts.push(str_of(block, "text").to_string()),
                "image" => images.extend(image_of(block)),
                "tool_result" => self.claude_tool_result(block, &value["toolUseResult"]),
                _ => {}
            }
        }
        if !texts.is_empty() || !images.is_empty() {
            self.claude_user_text(&texts.join("\n\n"), images);
        }
    }

    fn claude_user_text(&mut self, text: &str, images: Vec<Image>) {
        let t = text.trim();
        if t.starts_with("[Request interrupted by user") {
            self.entries.push(Entry::Notice("Interrupted".into()));
            return;
        }
        if let Some(rest) = t.strip_prefix("<local-command-stdout>") {
            let out = strip_ansi(rest.split("</local-command-stdout>").next().unwrap_or_default()).trim().to_string();
            if !out.is_empty() {
                self.entries.push(Entry::Notice(clip(&out, 300)));
            }
            return;
        }
        if t.starts_with("<task-notification>") {
            let summary = tag(t, "summary").unwrap_or_else(|| "A background task finished".into());
            self.entries.push(Entry::Notice(clip(&summary, 300)));
            return;
        }
        if t.starts_with("<command-name>") {
            if let Some(command) = claude_user_text(t) {
                self.entries.push(Entry::Notice(format!("You ran {command}")));
            }
            return;
        }
        match claude_user_text(t) {
            Some(text) => self.push_user(&text, images),
            None if !images.is_empty() => self.push_user("", images),
            None => {}
        }
    }

    fn claude_tool_use(&mut self, block: &Value) {
        let id = str_of(block, "id").to_string();
        let name = str_of(block, "name");
        let input = &block["input"];
        let file = || short_path(input["file_path"].as_str().or(input["notebook_path"].as_str()).unwrap_or_default());
        let tool = |verb: &str, subject: String, code: bool| Tool {
            id: id.clone(),
            verb: verb.to_string(),
            subject: clip(&subject, 160),
            code,
            meta: None,
            failed: false,
            done: false,
            detail: Vec::new(),
            images: Vec::new(),
            group: None,
        };
        let made = match name {
            "TodoWrite" => {
                let items = input["todos"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|t| Todo {
                        text: str_of(t, "content").to_string(),
                        state: match str_of(t, "status") {
                            "completed" => TodoState::Done,
                            "in_progress" => TodoState::Active,
                            _ => TodoState::Pending,
                        },
                    })
                    .collect();
                self.push_todos(items);
                // Its result ("Todos have been modified") is not worth a row.
                return;
            }
            "ExitPlanMode" => {
                let plan = str_of(input, "plan").trim();
                if !plan.is_empty() {
                    self.entries.push(Entry::Plan(plan.to_string()));
                }
                tool("Asked to start on the plan", String::new(), false)
            }
            "Bash" => {
                let mut t = Tool { group: Some(Group::Run), ..tool("Ran", first_line(str_of(input, "command")), true) };
                t.detail = command_lines(str_of(input, "command"));
                t
            }
            "Read" => Tool { group: Some(Group::Read), ..tool("Read", file(), true) },
            "Edit" | "MultiEdit" => tool("Edited", file(), true),
            "Write" => tool("Wrote", file(), true),
            "NotebookEdit" => tool("Edited", file(), true),
            "Grep" => Tool { group: Some(Group::Search), ..tool("Searched for", str_of(input, "pattern").to_string(), true) },
            "Glob" => Tool { group: Some(Group::List), ..tool("Found files", str_of(input, "pattern").to_string(), true) },
            "LS" => Tool { group: Some(Group::List), ..tool("Listed", short_path(str_of(input, "path")), true) },
            "WebFetch" => tool("Fetched", str_of(input, "url").to_string(), false),
            "WebSearch" => tool("Searched the web for", str_of(input, "query").to_string(), false),
            "Task" | "Agent" => tool("Asked an agent to", str_of(input, "description").to_lowercase(), false),
            "Skill" => tool("Used the skill", str_of(input, "skill").to_string(), false),
            "AskUserQuestion" => {
                let questions: Vec<String> = input["questions"].as_array().into_iter().flatten().map(|q| str_of(q, "question").to_string()).collect();
                tool("Asked you", questions.join(" · "), false)
            }
            _ => match name.strip_prefix("mcp__").and_then(|rest| rest.rsplit_once("__")) {
                Some((server, action)) => {
                    let mut t = tool(&format!("Used {}:", server.replace('_', " ").trim()), action.replace('_', " "), false);
                    t.detail = input_lines(input);
                    t
                }
                None => {
                    let mut t = tool("Used", name.to_string(), false);
                    t.detail = input_lines(input);
                    t
                }
            },
        };
        self.push_tool(made);
    }

    fn claude_tool_result(&mut self, block: &Value, result: &Value) {
        let id = str_of(block, "tool_use_id").to_string();
        let failed = block["is_error"].as_bool() == Some(true);
        let mut texts = Vec::new();
        let mut images = Vec::new();
        match &block["content"] {
            Value::String(s) => texts.push(s.clone()),
            Value::Array(parts) => {
                for part in parts {
                    match str_of(part, "type") {
                        "text" => texts.push(str_of(part, "text").to_string()),
                        "image" => images.extend(image_of(part)),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        let text = texts.join("\n");
        let Some(tool) = self.tool_mut(&id) else { return };
        tool.done = true;
        tool.failed = failed;
        tool.images = images;
        let interrupted = text.contains("[Request interrupted by user") || result["interrupted"].as_bool() == Some(true);
        // Diffs for edits, line counts for new files, output for the rest.
        if let Some(hunks) = result["structuredPatch"].as_array().filter(|h| !h.is_empty()) {
            let (lines, added, removed) = patch_lines(hunks);
            tool.meta = Some(format!("+{added} −{removed}"));
            tool.detail = lines;
        } else if str_of(result, "type") == "create" {
            let content = str_of(result, "content");
            tool.verb = "Created".into();
            tool.meta = Some(plural(content.lines().count(), "line"));
            tool.detail = content.lines().take(DETAIL_LINES).enumerate().map(|(i, l)| Line { text: clip_line(l), kind: LineKind::Added, number: Some(i as u32 + 1) }).collect();
        } else if result.get("stdout").is_some() {
            let mut lines = tool.detail.clone();
            if !lines.is_empty() {
                lines.push(Line { text: String::new(), kind: LineKind::Plain, number: None });
            }
            lines.extend(output_lines(str_of(result, "stdout"), LineKind::Plain));
            lines.extend(output_lines(str_of(result, "stderr"), LineKind::Error));
            tool.detail = lines;
        } else if !text.trim().is_empty() && tool.verb != "Read" {
            let mut lines = tool.detail.clone();
            if !lines.is_empty() {
                lines.push(Line { text: String::new(), kind: LineKind::Plain, number: None });
            }
            lines.extend(output_lines(&text, if failed { LineKind::Error } else { LineKind::Plain }));
            tool.detail = lines;
        } else if tool.verb == "Read" {
            tool.meta = result["file"]["numLines"].as_u64().map(|n| plural(n as usize, "line"));
        }
        if interrupted {
            tool.meta = Some("interrupted".into());
        } else if failed {
            tool.meta = Some("failed".into());
            if tool.detail.is_empty() {
                tool.detail = output_lines(&text, LineKind::Error);
            }
        }
    }

    // ----- Codex -------------------------------------------------------------------------------

    fn codex_line(&mut self, value: &Value) {
        let payload = &value["payload"];
        match (str_of(value, "type"), str_of(payload, "type")) {
            ("turn_context", _) => {
                if let Some(model) = payload["model"].as_str() {
                    self.model = Some(model.to_string());
                }
            }
            ("event_msg", "item_completed") => self.codex_item(&payload["item"]),
            ("event_msg", "task_complete") => {
                if let Some(ms) = payload["duration_ms"].as_u64().filter(|ms| *ms >= 1000) {
                    self.entries.push(Entry::Notice(format!("Worked for {}", duration(ms))));
                }
            }
            ("event_msg", "token_count") => {
                let info = &payload["info"];
                if let (Some(used), Some(window)) = (info["last_token_usage"]["input_tokens"].as_f64(), info["model_context_window"].as_f64()) {
                    if window > 0. {
                        self.context_percent = Some((used / window * 100.).clamp(0., 100.));
                    }
                }
            }
            _ => {}
        }
    }

    fn codex_item(&mut self, item: &Value) {
        let id = str_of(item, "id").to_string();
        let blank = |verb: &str, subject: String, code: bool| Tool {
            id: id.clone(),
            verb: verb.to_string(),
            subject: clip(&subject, 160),
            code,
            meta: None,
            failed: false,
            done: true,
            detail: Vec::new(),
            images: Vec::new(),
            group: None,
        };
        let texts = |key: &str| -> String {
            item[key]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| str_of(c, "text"))
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        };
        match str_of(item, "type") {
            "UserMessage" => {
                let text = texts("content");
                if !codex_injected(&text) {
                    self.push_user(&text, Vec::new());
                }
            }
            "AgentMessage" => {
                let text = texts("content");
                if !text.trim().is_empty() {
                    self.entries.push(Entry::Text(text.trim().to_string()));
                }
            }
            "CommandExecution" => {
                let parsed = item["parsed_cmd"].as_array().cloned().unwrap_or_default();
                let first = parsed.first();
                let command = first.map(|p| str_of(p, "cmd").to_string()).unwrap_or_else(|| {
                    item["command"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default()
                });
                let mut tool = match first.map(|p| str_of(p, "type")) {
                    Some("read") if parsed.len() == 1 => Tool { group: Some(Group::Read), ..blank("Read", short_path(first.map(|p| str_of(p, "path")).unwrap_or_default()), true) },
                    Some("search") if parsed.len() == 1 => Tool { group: Some(Group::Search), ..blank("Searched for", first.map(|p| str_of(p, "query").to_string()).unwrap_or_default(), true) },
                    Some("list_files") if parsed.len() == 1 => Tool { group: Some(Group::List), ..blank("Listed", short_path(first.map(|p| str_of(p, "path")).unwrap_or_default()), true) },
                    _ => Tool { group: Some(Group::Run), ..blank("Ran", first_line(&command), true) },
                };
                let exit = item["exit_code"].as_i64().unwrap_or(0);
                tool.failed = exit != 0 || str_of(item, "status") == "failed";
                if tool.failed {
                    tool.meta = Some(format!("exit {exit}"));
                }
                if tool.group == Some(Group::Run) {
                    tool.detail = command_lines(&command);
                    let out = str_of(item, "aggregated_output");
                    if !out.trim().is_empty() {
                        tool.detail.push(Line { text: String::new(), kind: LineKind::Plain, number: None });
                        tool.detail.extend(output_lines(out, if tool.failed { LineKind::Error } else { LineKind::Plain }));
                    }
                }
                self.push_tool(tool);
            }
            "FileChange" => {
                let Some(changes) = item["changes"].as_object() else { return };
                for (path, change) in changes {
                    let mut tool = Tool { id: format!("{id}:{path}"), ..blank("Edited", short_path(path), true) };
                    match str_of(change, "type") {
                        "add" => {
                            let content = str_of(change, "content");
                            tool.verb = "Created".into();
                            tool.meta = Some(plural(content.lines().count(), "line"));
                            tool.detail = content.lines().take(DETAIL_LINES).enumerate().map(|(i, l)| Line { text: clip_line(l), kind: LineKind::Added, number: Some(i as u32 + 1) }).collect();
                        }
                        "delete" => tool.verb = "Deleted".into(),
                        _ => {
                            let (lines, added, removed) = diff_lines(str_of(change, "unified_diff"));
                            tool.meta = Some(format!("+{added} −{removed}"));
                            tool.detail = lines;
                        }
                    }
                    tool.failed = str_of(item, "status") == "failed";
                    self.push_tool(tool);
                }
            }
            "McpToolCall" => {
                let mut tool = blank(&format!("Used {}:", str_of(item, "server").replace('_', " ")), str_of(item, "tool").replace('_', " "), false);
                tool.detail = input_lines(&item["arguments"]);
                let result = &item["result"];
                let out: Vec<String> = result["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str().map(str::to_string)).collect();
                tool.images = result["content"].as_array().into_iter().flatten().filter_map(codex_image).collect();
                tool.failed = result["isError"].as_bool() == Some(true) || str_of(item, "status") == "failed";
                if !out.is_empty() {
                    tool.detail.push(Line { text: String::new(), kind: LineKind::Plain, number: None });
                    tool.detail.extend(output_lines(&out.join("\n"), if tool.failed { LineKind::Error } else { LineKind::Plain }));
                }
                if tool.failed {
                    tool.meta = Some("failed".into());
                }
                self.push_tool(tool);
            }
            "Extension" if str_of(item, "kind") == "web.search" => {
                let mut tool = blank("Searched the web for", str_of(item, "query").to_string(), false);
                tool.detail = item["results"].as_array().into_iter().flatten().take(8).map(|r| Line { text: format!("{} · {}", str_of(r, "title"), str_of(r, "url")), kind: LineKind::Plain, number: None }).collect();
                self.push_tool(tool);
            }
            "ContextCompaction" => self.entries.push(Entry::Notice("Conversation compacted".into())),
            _ => {}
        }
    }
}

/// Entries as the Chat view shows them: rows of the same kind next to each other (reading
/// files, searching) fold into one.
pub fn rows(entries: &[Entry]) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        if let Entry::Todos { superseded: true, .. } = entry {
            continue;
        }
        let group = match entry {
            Entry::Tool(t) if t.done && !t.failed && t.images.is_empty() => t.group,
            _ => None,
        };
        let Some(group) = group else {
            rows.push(Row::One(i));
            continue;
        };
        // Rows that fold together, while nothing else comes between.
        let foldable = |e: &Entry| match e {
            Entry::Tool(t) if t.done && !t.failed && t.images.is_empty() => t.group.filter(|g| g.joins(group)),
            _ => None,
        };
        match rows.last_mut() {
            Some(Row::Group { group: g, entries: list, .. }) if g.joins(group) => list.push(i),
            Some(Row::One(prev)) if foldable(&entries[*prev]).is_some() => {
                let prev = *prev;
                rows.pop();
                rows.push(Row::Group { group, entries: vec![prev, i], label: String::new() });
            }
            _ => rows.push(Row::One(i)),
        }
    }
    for row in &mut rows {
        if let Row::Group { entries: list, label, group } = row {
            let count = |g: Group| list.iter().filter(|&&i| matches!(&entries[i], Entry::Tool(t) if t.group == Some(g))).count();
            let mut parts = Vec::new();
            let (read, search, listed, run) = (count(Group::Read), count(Group::Search), count(Group::List), count(Group::Run));
            if run > 0 {
                parts.push(format!("Ran {}", plural(run, "command")));
            }
            if read > 0 {
                parts.push(format!("Read {}", plural(read, "file")));
            }
            if search > 0 {
                parts.push(format!("{} {}", if parts.is_empty() { "Searched" } else { "searched" }, times(search)));
            }
            if listed > 0 {
                parts.push(format!("{} {}", if parts.is_empty() { "Listed" } else { "listed" }, plural(listed, "folder")));
            }
            *label = parts.join(", ");
            *group = if run > 0 { Group::Run } else if read > 0 { Group::Read } else if search > 0 { Group::Search } else { Group::List };
        }
    }
    rows
}

/// Where a session's conversation is: Claude's transcript (from its hooks, or found by the
/// conversation id in the account's folder) or Codex's rollout.
pub fn locate(agent: AgentKind, config_dir: Option<&Path>, conversation: Option<&str>, reported: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = reported.filter(|p| p.is_file()) {
        return Some(path.to_path_buf());
    }
    let (dir, id) = (config_dir?, conversation?);
    match agent {
        AgentKind::Claude => crate::agents::find_claude_transcript(dir, id),
        AgentKind::Codex => crate::agents::find_codex_rollout(dir, id),
        AgentKind::Shell => None,
    }
}

/// Glasscode's own messages end "(Sent by Glasscode because CI failed; ask 1 of 3.)".
fn split_glasscode(text: &str) -> (String, Option<String>) {
    let marker = "(Sent by Glasscode because ";
    if let Some(at) = text.rfind(marker) {
        let why = text[at + marker.len()..].trim_end().trim_end_matches(')').trim_end_matches('.').to_string();
        if text[at..].trim_end().ends_with(')') {
            return (text[..at].trim_end().to_string(), Some(why));
        }
    }
    (text.to_string(), None)
}

fn image_of(block: &Value) -> Option<Image> {
    let source = &block["source"];
    (str_of(source, "type") == "base64").then(|| Image { media_type: str_of(source, "media_type").to_string(), base64: Arc::new(str_of(source, "data").to_string()) })
}

fn codex_image(content: &Value) -> Option<Image> {
    (str_of(content, "type") == "image").then(|| Image { media_type: content["mimeType"].as_str().unwrap_or("image/png").to_string(), base64: Arc::new(str_of(content, "data").to_string()) }).filter(|i| !i.base64.is_empty())
}

fn tag(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim().to_string())
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn clip_line(line: &str) -> String {
    clip(line.trim_end(), DETAIL_LINE_CHARS)
}

fn output_lines(text: &str, kind: LineKind) -> Vec<Line> {
    let text = strip_ansi(text);
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let mut out: Vec<Line> = lines.iter().take(DETAIL_LINES).map(|l| Line { text: clip_line(l), kind, number: None }).collect();
    if lines.len() > DETAIL_LINES {
        out.push(Line { text: format!("… {} more", plural(lines.len() - DETAIL_LINES, "line")), kind: LineKind::Hunk, number: None });
    }
    out
}

fn command_lines(command: &str) -> Vec<Line> {
    command.trim().lines().take(12).enumerate().map(|(i, l)| Line { text: format!("{}{}", if i == 0 { "$ " } else { "  " }, clip_line(l)), kind: LineKind::Hunk, number: None }).collect()
}

/// A tool's input, a line per field ("url: https://…").
fn input_lines(input: &Value) -> Vec<Line> {
    let Some(fields) = input.as_object() else { return Vec::new() };
    fields
        .iter()
        .take(8)
        .map(|(key, value)| {
            let shown = match value {
                Value::String(s) => first_line(s),
                other => clip(&other.to_string(), 200),
            };
            Line { text: format!("{key}: {shown}"), kind: LineKind::Hunk, number: None }
        })
        .collect()
}

/// Claude's `structuredPatch` hunks as diff lines, with the lines added and removed.
fn patch_lines(hunks: &[Value]) -> (Vec<Line>, usize, usize) {
    let mut lines = Vec::new();
    let (mut added, mut removed) = (0, 0);
    for (i, hunk) in hunks.iter().enumerate() {
        if i > 0 {
            lines.push(Line { text: "⋯".into(), kind: LineKind::Hunk, number: None });
        }
        let (mut old, mut new) = (hunk["oldStart"].as_u64().unwrap_or(1) as u32, hunk["newStart"].as_u64().unwrap_or(1) as u32);
        for line in hunk["lines"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            let (kind, number) = match line.chars().next() {
                Some('+') => {
                    added += 1;
                    new += 1;
                    (LineKind::Added, new - 1)
                }
                Some('-') => {
                    removed += 1;
                    old += 1;
                    (LineKind::Removed, old - 1)
                }
                _ => {
                    old += 1;
                    new += 1;
                    (LineKind::Plain, new - 1)
                }
            };
            lines.push(Line { text: clip_line(line), kind, number: Some(number) });
        }
    }
    if lines.len() > DETAIL_LINES * 2 {
        let more = lines.len() - DETAIL_LINES * 2;
        lines.truncate(DETAIL_LINES * 2);
        lines.push(Line { text: format!("… {} more", plural(more, "line")), kind: LineKind::Hunk, number: None });
    }
    (lines, added, removed)
}

/// A unified diff as lines, with the lines added and removed.
fn diff_lines(diff: &str) -> (Vec<Line>, usize, usize) {
    let (mut added, mut removed) = (0, 0);
    let mut lines = Vec::new();
    let (mut old, mut new) = (1u32, 1u32);
    for line in diff.lines() {
        let (kind, number) = if let Some(header) = line.strip_prefix("@@") {
            // "@@ -10,2 +10,15 @@": where the next lines are.
            let start = |sign: char| header.split_whitespace().find_map(|p| p.strip_prefix(sign)).and_then(|p| p.split(',').next()).and_then(|n| n.parse().ok()).unwrap_or(1);
            (old, new) = (start('-'), start('+'));
            if !lines.is_empty() && lines.len() < DETAIL_LINES * 2 {
                lines.push(Line { text: "⋯".into(), kind: LineKind::Hunk, number: None });
            }
            continue;
        } else if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
            new += 1;
            (LineKind::Added, Some(new - 1))
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
            old += 1;
            (LineKind::Removed, Some(old - 1))
        } else {
            old += 1;
            new += 1;
            (LineKind::Plain, Some(new - 1))
        };
        if lines.len() < DETAIL_LINES * 2 {
            lines.push(Line { text: clip_line(line), kind, number });
        }
    }
    (lines, added, removed)
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

fn times(n: usize) -> String {
    if n == 1 { "once".into() } else { format!("{n} times") }
}

/// "41s", "2m 14s", "1h 3m".
pub fn duration(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

/// A friendlier model name: "claude-opus-5-5" → "Opus 5.5", "gpt-6-astra" → "GPT-6-Astra".
pub fn model_label(model: &str) -> String {
    let m = model.trim_start_matches("claude-");
    let mut parts = m.split('-');
    if let Some(family @ ("opus" | "sonnet" | "haiku" | "fable")) = parts.next() {
        let version: Vec<&str> = parts.take_while(|p| p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit())).collect();
        let mut name = family[..1].to_uppercase() + &family[1..];
        if !version.is_empty() {
            name.push(' ');
            name.push_str(&version.join("."));
        }
        return name;
    }
    model
        .split('-')
        .map(|p| if p.eq_ignore_ascii_case("gpt") { "GPT".to_string() } else { p[..1].to_uppercase() + &p[1..] })
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn file(lines: &[Value]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for line in lines {
            use std::io::Write;
            writeln!(file, "{line}").unwrap();
        }
        file
    }

    fn append(file: &tempfile::NamedTempFile, lines: &[Value]) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(file.path()).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    fn assistant(blocks: Value) -> Value {
        json!({"type": "assistant", "message": {"role": "assistant", "model": "claude-opus-5-5", "content": blocks}})
    }

    fn result(id: &str, content: Value, tool_result: Value) -> Value {
        json!({"type": "user", "toolUseResult": tool_result, "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": content}]}})
    }

    #[test]
    fn reads_a_claude_conversation() {
        let f = file(&[
            json!({"type": "user", "message": {"role": "user", "content": "fix the token refresh"}}),
            json!({"type": "user", "isMeta": true, "message": {"role": "user", "content": "injected"}}),
            assistant(json!([{"type": "thinking", "thinking": "hmm"}, {"type": "text", "text": "Looking at `mcp-oauth.ts`."}])),
            assistant(json!([{"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/w/services/iam/src/mcp-oauth.ts"}}])),
            result("t1", json!("…"), json!({"file": {"numLines": 120}})),
            assistant(json!([{"type": "tool_use", "id": "t2", "name": "Edit", "input": {"file_path": "/w/services/iam/src/mcp-oauth.ts"}}])),
            result("t2", json!("ok"), json!({"structuredPatch": [{"oldStart": 10, "oldLines": 2, "newStart": 10, "newLines": 3, "lines": [" a", "-b", "+c", "+d"]}]})),
            assistant(json!([{"type": "tool_use", "id": "t3", "name": "Bash", "input": {"command": "pnpm test"}}])),
            result("t3", json!("14 passed"), json!({"stdout": "14 passed", "stderr": ""})),
            json!({"type": "system", "subtype": "turn_duration", "durationMs": 134000}),
        ]);
        let log = ChatLog::open(AgentKind::Claude, f.path());
        assert_eq!(log.model.as_deref(), Some("claude-opus-5-5"));
        assert!(matches!(&log.entries[0], Entry::User { text, .. } if text == "fix the token refresh"));
        assert!(matches!(&log.entries[1], Entry::Text(t) if t == "Looking at `mcp-oauth.ts`."));
        let Entry::Tool(read) = &log.entries[2] else { panic!() };
        assert_eq!((read.verb.as_str(), read.subject.as_str(), read.meta.as_deref()), ("Read", "iam/src/mcp-oauth.ts", Some("120 lines")));
        let Entry::Tool(edit) = &log.entries[3] else { panic!() };
        assert_eq!(edit.meta.as_deref(), Some("+2 −1"));
        assert_eq!(edit.detail.iter().filter(|l| l.kind == LineKind::Added).count(), 2);
        assert_eq!(edit.detail.iter().map(|l| l.number).collect::<Vec<_>>(), vec![Some(10), Some(11), Some(11), Some(12)]);
        let Entry::Tool(bash) = &log.entries[4] else { panic!() };
        assert_eq!((bash.verb.as_str(), bash.subject.as_str(), bash.done), ("Ran", "pnpm test", true));
        assert_eq!(bash.detail.last().unwrap().text, "14 passed");
        assert_eq!(log.entries[5], Entry::Notice("Worked for 2m 14s".into()));
    }

    #[test]
    fn follows_a_growing_file_and_knows_the_running_tool() {
        let f = file(&[assistant(json!([{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "gh pr review 656 --comment"}}]))]);
        let mut log = ChatLog::open(AgentKind::Claude, f.path());
        assert_eq!(log.pending_tool().map(|t| t.subject.as_str()), Some("gh pr review 656 --comment"));
        assert!(!log.poll());
        append(&f, &[result("t1", json!("Error: denied"), json!(null)), json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "x", "is_error": true, "content": "x"}]}})]);
        assert!(log.poll());
        assert!(log.pending_tool().is_none());
    }

    #[test]
    fn marks_what_glasscode_sent_and_command_notes() {
        let f = file(&[
            json!({"type": "user", "message": {"role": "user", "content": "PR #947 has merge conflicts with main. Rebase it. (Sent by Glasscode because GitHub reports conflicts; ask 1 of 3.)"}}),
            json!({"type": "user", "message": {"role": "user", "content": "<command-name>/model</command-name>\n<command-args>opus</command-args>"}}),
            json!({"type": "user", "message": {"role": "user", "content": "<local-command-stdout>Set model to \u{1b}[1mOpus 5.5\u{1b}[22m</local-command-stdout>"}}),
            json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": "[Request interrupted by user]"}]}}),
            json!({"type": "system", "subtype": "compact_boundary"}),
        ]);
        let log = ChatLog::open(AgentKind::Claude, f.path());
        assert_eq!(log.entries[0], Entry::User { text: "PR #947 has merge conflicts with main. Rebase it.".into(), images: vec![], from_glasscode: Some("GitHub reports conflicts; ask 1 of 3".into()) });
        assert_eq!(log.entries[1], Entry::Notice("You ran /model opus".into()));
        assert_eq!(log.entries[2], Entry::Notice("Set model to Opus 5.5".into()));
        assert_eq!(log.entries[3], Entry::Notice("Interrupted".into()));
        assert_eq!(log.entries[4], Entry::Notice("Conversation compacted".into()));
    }

    #[test]
    fn shows_messages_sent_while_claude_works() {
        let f = file(&[
            json!({"type": "user", "message": {"role": "user", "content": "review the PR"}}),
            json!({"type": "queue-operation", "operation": "enqueue", "content": "also check for a fix branch"}),
        ]);
        let mut log = ChatLog::open(AgentKind::Claude, f.path());
        assert_eq!(log.queued, vec!["also check for a fix branch".to_string()]);
        append(&f, &[
            json!({"type": "queue-operation", "operation": "remove", "content": "also check for a fix branch"}),
            json!({"type": "attachment", "attachment": {"type": "queued_command", "commandMode": "prompt", "prompt": "also check for a fix branch"}}),
            json!({"type": "attachment", "attachment": {"type": "queued_command", "commandMode": "task-notification", "prompt": "<task-notification><summary>Agent \"Audit\" finished</summary></task-notification>"}}),
        ]);
        log.poll();
        assert!(log.queued.is_empty());
        assert!(matches!(&log.entries[1], Entry::User { text, .. } if text == "also check for a fix branch"));
        assert_eq!(log.entries[2], Entry::Notice("Agent \"Audit\" finished".into()));
    }

    #[test]
    fn keeps_screenshots_and_the_latest_plan() {
        let f = file(&[
            assistant(json!([{"type": "tool_use", "id": "p1", "name": "TodoWrite", "input": {"todos": [{"content": "Fix refresh", "status": "in_progress"}]}}])),
            assistant(json!([{"type": "tool_use", "id": "s1", "name": "mcp__glasscode-browser__screenshot", "input": {"url": "https://x"}}])),
            result("s1", json!([{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBOR"}}]), json!(null)),
            assistant(json!([{"type": "tool_use", "id": "p2", "name": "TodoWrite", "input": {"todos": [{"content": "Fix refresh", "status": "completed"}, {"content": "Test it", "status": "pending"}]}}])),
        ]);
        let log = ChatLog::open(AgentKind::Claude, f.path());
        assert!(matches!(&log.entries[0], Entry::Todos { superseded: true, .. }));
        let Entry::Tool(shot) = &log.entries[1] else { panic!() };
        assert_eq!((shot.verb.as_str(), shot.subject.as_str(), shot.images.len()), ("Used glasscode-browser:", "screenshot", 1));
        let Entry::Todos { items, superseded: false } = &log.entries[2] else { panic!() };
        assert_eq!(items[0].state, TodoState::Done);
        assert_eq!(rows(&log.entries), vec![Row::One(1), Row::One(2)]);
    }

    #[test]
    fn folds_reading_and_searching() {
        let f = file(&[
            assistant(json!([{"type": "tool_use", "id": "a", "name": "Read", "input": {"file_path": "/a.rs"}}, {"type": "tool_use", "id": "b", "name": "Read", "input": {"file_path": "/b.rs"}}, {"type": "tool_use", "id": "c", "name": "Grep", "input": {"pattern": "offline_access"}}])),
            result("a", json!("x"), json!(null)),
            result("b", json!("x"), json!(null)),
            result("c", json!("x"), json!(null)),
            assistant(json!([{"type": "text", "text": "Found it."}])),
        ]);
        let log = ChatLog::open(AgentKind::Claude, f.path());
        assert_eq!(rows(&log.entries), vec![Row::Group { group: Group::Read, entries: vec![0, 1, 2], label: "Read 2 files, searched once".into() }, Row::One(3)]);
        let f = file(&[
            assistant(json!([{"type": "tool_use", "id": "a", "name": "Bash", "input": {"command": "git fetch"}}, {"type": "tool_use", "id": "b", "name": "Bash", "input": {"command": "git rebase origin/main"}}, {"type": "tool_use", "id": "c", "name": "Read", "input": {"file_path": "/a.rs"}}])),
            result("a", json!("x"), json!({"stdout": "", "stderr": ""})),
            result("b", json!("x"), json!({"stdout": "", "stderr": ""})),
            result("c", json!("x"), json!(null)),
        ]);
        let log = ChatLog::open(AgentKind::Claude, f.path());
        assert_eq!(rows(&log.entries), vec![Row::Group { group: Group::Run, entries: vec![0, 1], label: "Ran 2 commands".into() }, Row::One(2)]);
    }

    #[test]
    fn reads_a_codex_rollout() {
        let item = |item: Value| json!({"type": "event_msg", "payload": {"type": "item_completed", "item": item}});
        let f = file(&[
            json!({"type": "turn_context", "payload": {"model": "gpt-6-astra"}}),
            item(json!({"type": "UserMessage", "id": "u", "content": [{"type": "text", "text": "add a search page"}]})),
            item(json!({"type": "UserMessage", "id": "u0", "content": [{"type": "text", "text": "<environment_context>\n</environment_context>"}]})),
            item(json!({"type": "CommandExecution", "id": "c1", "command": ["/bin/zsh"], "parsed_cmd": [{"type": "read", "cmd": "cat src/a.ts", "path": "src/a.ts"}], "status": "completed", "aggregated_output": "…", "exit_code": 0})),
            item(json!({"type": "CommandExecution", "id": "c2", "command": ["/bin/zsh"], "parsed_cmd": [{"type": "unknown", "cmd": "npm test"}], "status": "completed", "aggregated_output": "1 failing", "exit_code": 1})),
            item(json!({"type": "FileChange", "id": "f", "changes": {"/w/src/pages/search.tsx": {"type": "add", "content": "a\nb\n"}, "/w/src/lib/index.ts": {"type": "update", "unified_diff": "@@ -1 +1,2 @@\n-a\n+b\n+c"}}, "status": "completed"})),
            item(json!({"type": "AgentMessage", "id": "m", "content": [{"type": "Text", "text": "Added the page."}], "phase": "final_answer"})),
            json!({"type": "event_msg", "payload": {"type": "token_count", "info": {"last_token_usage": {"input_tokens": 25840}, "model_context_window": 258400}}}),
            json!({"type": "event_msg", "payload": {"type": "task_complete", "duration_ms": 24000}}),
        ]);
        let log = ChatLog::open(AgentKind::Codex, f.path());
        assert_eq!(log.model.as_deref(), Some("gpt-6-astra"));
        assert!(matches!(&log.entries[0], Entry::User { text, .. } if text == "add a search page"));
        let Entry::Tool(read) = &log.entries[1] else { panic!() };
        assert_eq!((read.verb.as_str(), read.group), ("Read", Some(Group::Read)));
        let Entry::Tool(test) = &log.entries[2] else { panic!() };
        assert_eq!((test.subject.as_str(), test.failed, test.meta.as_deref()), ("npm test", true, Some("exit 1")));
        let Entry::Tool(created) = &log.entries[3] else { panic!() };
        assert_eq!((created.verb.as_str(), created.meta.as_deref()), ("Created", Some("2 lines")));
        let Entry::Tool(edited) = &log.entries[4] else { panic!() };
        assert_eq!(edited.meta.as_deref(), Some("+2 −1"));
        assert_eq!(log.entries[5], Entry::Text("Added the page.".into()));
        assert_eq!(log.entries[6], Entry::Notice("Worked for 24s".into()));
        assert_eq!(log.context_percent.map(|p| p.round()), Some(10.));
    }

    #[test]
    fn reads_the_voice_setting() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(claude_voice(dir.path()), None);
        std::fs::write(dir.path().join("settings.json"), r#"{"voiceEnabled": true}"#).unwrap();
        assert_eq!(claude_voice(dir.path()), Some(VoiceMode::Hold));
        std::fs::write(dir.path().join("settings.json"), r#"{"voice": {"enabled": true, "mode": "tap"}}"#).unwrap();
        assert_eq!(claude_voice(dir.path()), Some(VoiceMode::Tap));
        std::fs::write(dir.path().join("settings.json"), r#"{"voice": {"enabled": false}, "voiceEnabled": true}"#).unwrap();
        assert_eq!(claude_voice(dir.path()), None);
    }

    #[test]
    fn names_models() {
        assert_eq!(model_label("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_label("claude-sonnet-5"), "Sonnet 5");
        assert_eq!(model_label("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(model_label("gpt-6-astra"), "GPT-6-Astra");
        assert_eq!(duration(41_000), "41s");
    }
}
