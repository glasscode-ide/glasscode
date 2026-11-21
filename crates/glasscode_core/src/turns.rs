//! Turns and turn reports (design D24, slice M30): a turn is a state machine with an explicit end
//! cause, closed exactly once.
//!
//! - **Opens** on `UserPromptSubmit` (Claude, keyed by `prompt_id`) or on a rollout's
//!   `task_started` (Codex, keyed by `turn_id`).
//! - **Closes** on one of: `Stop` with no background work → `Completed`; `Stop` with background
//!   tasks or session crons → `Paused`; `StopFailure` (or a Codex `error` before its
//!   `task_complete`) → `Failed`; a Codex `turn_aborted`, Claude's interrupt marker in the
//!   transcript, or a new prompt while a turn is open → `Interrupted`; `SessionEnd` or the agent's
//!   exit → `Aborted(status)`; a killed agent, or a dead process whose transcript stopped growing for
//!   [`CRASH_QUIET_MS`] → `Crashed`; the app quitting → `AppQuit`; the machine asleep → `Slept`.
//! - Repeated `Stop`s of one `prompt_id` (a stop-hook continuation) collapse into the first;
//!   `SubagentStart` and `SubagentStop` belong to the turn that started them, and an internal one
//!   (an empty `agent_type`) is ignored. A `Notification` is read by its `notification_type`.
//!
//! The report ([`TurnReport`]) is built from the turn's own signals first (Stop's
//! `last_assistant_message`, P05), then from the transcript between the turn's bounds once it
//! settles (it lags the hooks), and from git against the head at the turn's start. Codex turns are
//! read from the rollout, which needs no hook (P06); Codex hooks are not installed.
//!
//! What a session says about itself (`glasscode report …`) is a hint, never evidence.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::journal::{Event, Source, kinds};
use crate::model::AgentKind;
use crate::paths::ProjectKey;
use crate::transcript::{ItemKind, TranscriptItem};

/// A dead agent whose transcript has not grown for this long crashed (the watchdog).
pub const CRASH_QUIET_MS: u64 = 120_000;
/// How long a report waits for the transcript to catch up with the hooks (P05: 111 ms seen).
pub const TRANSCRIPT_LAG: Duration = Duration::from_secs(3);
/// How much of the last reply a report keeps.
pub const REPLY_CHARS: usize = 600;
/// The most of a transcript one report reads, from the turn's start.
const REPORT_BYTES: u64 = 8 * 1024 * 1024;
/// The schema of a report.
pub const REPORT_VERSION: u16 = 1;

/// How a turn ended (design section 8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "end", content = "detail", rename_all = "snake_case")]
pub enum TurnEnd {
    Completed,
    /// It stopped with background work still running: reported now, the work stays open.
    Paused,
    Failed(String),
    Interrupted,
    Aborted(Option<i32>),
    Crashed,
    Slept,
    AppQuit,
}

/// A `turn.ended` event's end in words: `completed`, `failed: <why>`, … (its report writes
/// `{"end": "failed", "detail": "…"}`; a plain word reads as itself).
pub fn end_words(end: &Value) -> String {
    let name = end.get("end").and_then(Value::as_str).or(end.as_str()).unwrap_or_default();
    match end.get("detail").and_then(Value::as_str).filter(|d| !d.is_empty()) {
        Some(detail) => format!("{name}: {detail}"),
        None => name.to_string(),
    }
}

impl TurnEnd {
    /// `Completed`, `Failed`, … (what the simulations compare).
    pub fn name(&self) -> &'static str {
        match self {
            TurnEnd::Completed => "Completed",
            TurnEnd::Paused => "Paused",
            TurnEnd::Failed(_) => "Failed",
            TurnEnd::Interrupted => "Interrupted",
            TurnEnd::Aborted(_) => "Aborted",
            TurnEnd::Crashed => "Crashed",
            TurnEnd::Slept => "Slept",
            TurnEnd::AppQuit => "AppQuit",
        }
    }

    /// For a person: "completed", "failed: rate_limit", "aborted (exit 1)".
    pub fn words(&self) -> String {
        match self {
            TurnEnd::Failed(why) => format!("failed: {why}"),
            TurnEnd::Aborted(Some(code)) => format!("aborted (exit {code})"),
            TurnEnd::AppQuit => "cut by Glasscode quitting".into(),
            TurnEnd::Slept => "cut by sleep".into(),
            other => other.name().to_lowercase(),
        }
    }
}

/// A turn that is open.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTurn {
    /// The `prompt_id` (Claude), the `turn_id` (Codex), or one of Glasscode's.
    pub id: String,
    pub started_ms: u64,
    /// The prompt came from an agent (a subagent's hand-back), not from anyone typing: D1 tier
    /// "never".
    #[serde(default)]
    pub by_agent: bool,
    /// Where the transcript was when it opened, and the checkout's head: the report reads from
    /// there.
    #[serde(default)]
    pub transcript_offset: Option<u64>,
    #[serde(default)]
    pub head: Option<String>,
    /// It waits on the person (a permission prompt, a question the agent asked).
    #[serde(default)]
    pub question: Option<String>,
    /// A Codex `error` came in this turn.
    #[serde(default)]
    pub error: Option<String>,
    /// The id is Glasscode's: the prompt carried none, so any `Stop` is this turn's.
    #[serde(default)]
    pub generated_id: bool,
}

/// A turn that closed, with what its end said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Closed {
    pub turn: OpenTurn,
    pub end: TurnEnd,
    pub ended_ms: u64,
    /// The reply its end carried (Stop's `last_assistant_message`, Codex's `last_agent_message`).
    pub reply: Option<String>,
    /// The background tasks and session crons a paused turn left running.
    pub background: Vec<String>,
}

/// What can open or close a turn.
#[derive(Clone, Debug, PartialEq)]
pub enum Signal {
    /// A Claude hook payload.
    Hook(Value),
    /// A Codex rollout line.
    Rollout(Value),
    /// A line added to a Claude transcript.
    TranscriptLine(String),
    /// The agent's process ended, with its status when known.
    Exited(Option<i32>),
    /// The agent was killed (`kill -9`), or the app that held it crashed.
    Killed,
    AppQuit,
    Slept,
    /// The watchdog's look: whether the agent runs, and how long its transcript has not grown.
    Watchdog { alive: bool, quiet_ms: u64 },
}

/// What a signal did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Opened(OpenTurn),
    Closed(Closed),
}

/// One session's turns.
#[derive(Clone, Debug, Default)]
pub struct TurnMachine {
    open: Option<OpenTurn>,
    /// The last turns closed, so a repeated `Stop` of one of them collapses into it.
    closed: VecDeque<String>,
    next: u64,
}

impl TurnMachine {
    pub fn open(&self) -> Option<&OpenTurn> {
        self.open.as_ref()
    }

    /// A turn the journal says is still open (after a restart).
    pub fn restore(&mut self, turn: OpenTurn) {
        self.open = Some(turn);
    }

    /// Set where an open turn's report starts reading (read when it opened, off the machine).
    pub fn set_baseline(&mut self, id: &str, transcript_offset: Option<u64>, head: Option<String>) {
        if let Some(open) = self.open.as_mut().filter(|open| open.id == id) {
            open.transcript_offset = transcript_offset.or(open.transcript_offset);
            open.head = head.or(open.head.take());
        }
    }

    fn close(&mut self, now_ms: u64, end: TurnEnd, reply: Option<String>, background: Vec<String>) -> Option<Change> {
        let turn = self.open.take()?;
        self.closed.push_back(turn.id.clone());
        while self.closed.len() > 32 {
            self.closed.pop_front();
        }
        Some(Change::Closed(Closed { turn, end, ended_ms: now_ms, reply, background }))
    }

    fn opened(&mut self, now_ms: u64, id: Option<String>, by_agent: bool) -> Change {
        self.next += 1;
        let given = id.filter(|id| !id.is_empty());
        let generated_id = given.is_none();
        let id = given.unwrap_or_else(|| format!("t{now_ms}-{}", self.next));
        let turn = OpenTurn { id, started_ms: now_ms, by_agent, generated_id, ..OpenTurn::default() };
        self.open = Some(turn.clone());
        Change::Opened(turn)
    }

    /// Feed one signal: the turns it opened and closed, in order.
    pub fn feed(&mut self, now_ms: u64, signal: &Signal) -> Vec<Change> {
        let mut out = Vec::new();
        match signal {
            Signal::Hook(payload) => self.hook(now_ms, payload, &mut out),
            Signal::Rollout(line) => self.rollout(now_ms, line, &mut out),
            Signal::TranscriptLine(line) => {
                if self.open.is_some() && interrupt_marker(line) {
                    out.extend(self.close(now_ms, TurnEnd::Interrupted, None, Vec::new()));
                }
            }
            // A signal (137 = 128 + 9) is a kill; any other exit with a turn open aborts it.
            Signal::Exited(Some(137)) | Signal::Killed => out.extend(self.close(now_ms, TurnEnd::Crashed, None, Vec::new())),
            Signal::Exited(status) => out.extend(self.close(now_ms, TurnEnd::Aborted(*status), None, Vec::new())),
            Signal::AppQuit => out.extend(self.close(now_ms, TurnEnd::AppQuit, None, Vec::new())),
            Signal::Slept => out.extend(self.close(now_ms, TurnEnd::Slept, None, Vec::new())),
            Signal::Watchdog { alive, quiet_ms } => {
                if !alive && *quiet_ms >= CRASH_QUIET_MS {
                    out.extend(self.close(now_ms, TurnEnd::Crashed, None, Vec::new()));
                }
            }
        }
        out
    }

    fn hook(&mut self, now_ms: u64, payload: &Value, out: &mut Vec<Change>) {
        let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string);
        let prompt_id = text("prompt_id");
        match text("hook_event_name").as_deref().unwrap_or_default() {
            "UserPromptSubmit" => {
                // A prompt while a turn is open: the one before ended without its Stop (an
                // interrupt Claude marks with no hook).
                if self.open.as_ref().is_some_and(|open| prompt_id.is_none() || Some(&open.id) != prompt_id.as_ref()) {
                    out.extend(self.close(now_ms, TurnEnd::Interrupted, None, Vec::new()));
                }
                if self.open.is_none() {
                    let by_agent = text("prompt").is_some_and(|p| p.trim_start().starts_with("<agent-message"));
                    out.push(self.opened(now_ms, prompt_id, by_agent));
                }
            }
            "Stop" => {
                if !self.matches(prompt_id.as_deref()) {
                    // A repeated Stop of a closed turn (a continuation), or one for no turn.
                    return;
                }
                let background = background_of(payload);
                let end = if background.is_empty() { TurnEnd::Completed } else { TurnEnd::Paused };
                out.extend(self.close(now_ms, end, text("last_assistant_message"), background));
            }
            "StopFailure" => {
                if self.matches(prompt_id.as_deref()) {
                    let why = text("error").filter(|e| !e.is_empty()).unwrap_or_else(|| "error".into());
                    out.extend(self.close(now_ms, TurnEnd::Failed(why), text("last_assistant_message"), Vec::new()));
                }
            }
            "SessionEnd" => out.extend(self.close(now_ms, TurnEnd::Aborted(None), None, Vec::new())),
            "Notification" => {
                if let Some(open) = self.open.as_mut() {
                    let kind = text("notification_type").unwrap_or_default();
                    if matches!(kind.as_str(), "permission_prompt" | "agent_needs_input" | "elicitation_dialog") {
                        open.question = Some(text("message").unwrap_or_else(|| kind.replace('_', " ")));
                    }
                }
            }
            // A subagent is part of the turn that started it; an internal one (no `agent_type`)
            // is Claude's own helper. Neither opens nor closes anything.
            _ => {}
        }
    }

    /// Whether a `Stop` of `prompt_id` is the open turn's (a missing id is the open one's).
    fn matches(&self, prompt_id: Option<&str>) -> bool {
        match (&self.open, prompt_id) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(open), Some(id)) => open.id == id || (open.generated_id && !self.closed.iter().any(|c| c == id)),
        }
    }

    fn rollout(&mut self, now_ms: u64, line: &Value, out: &mut Vec<Change>) {
        if line.get("type").and_then(Value::as_str) != Some("event_msg") {
            return;
        }
        let payload = &line["payload"];
        let turn_id = payload.get("turn_id").and_then(Value::as_str).map(str::to_string);
        match payload.get("type").and_then(Value::as_str).unwrap_or_default() {
            "task_started" => {
                if self.open.is_some() {
                    out.extend(self.close(now_ms, TurnEnd::Interrupted, None, Vec::new()));
                }
                out.push(self.opened(now_ms, turn_id, false));
            }
            "error" => {
                if let Some(open) = self.open.as_mut() {
                    open.error = Some(payload.get("message").and_then(Value::as_str).unwrap_or("error").to_string());
                }
            }
            "task_complete" => {
                let reply = payload.get("last_agent_message").and_then(Value::as_str).map(str::to_string);
                let end = match self.open.as_ref().and_then(|open| open.error.clone()) {
                    Some(why) => TurnEnd::Failed(why),
                    None => TurnEnd::Completed,
                };
                out.extend(self.close(now_ms, end, reply, Vec::new()));
            }
            "turn_aborted" => out.extend(self.close(now_ms, TurnEnd::Interrupted, None, Vec::new())),
            _ => {}
        }
    }
}

/// Claude's interrupt marker: a user record `[Request interrupted by user…]`.
pub fn interrupt_marker(line: &str) -> bool {
    if !line.contains("[Request interrupted by user") {
        return false;
    }
    serde_json::from_str::<Value>(line).is_ok_and(|value| value.get("type").and_then(Value::as_str) == Some("user"))
}

/// The background tasks and session crons a `Stop` lists (events-25), each in a few words.
fn background_of(payload: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for task in payload.get("background_tasks").and_then(Value::as_array).into_iter().flatten() {
        let running = task.get("status").and_then(Value::as_str).is_none_or(|s| s == "running" || s == "pending");
        if running {
            let what = task.get("description").or_else(|| task.get("agent_type")).or_else(|| task.get("type")).and_then(Value::as_str).unwrap_or("a background task");
            out.push(what.to_string());
        }
    }
    for cron in payload.get("session_crons").and_then(Value::as_array).into_iter().flatten() {
        let what = cron.get("prompt").or_else(|| cron.get("description")).or_else(|| cron.get("schedule")).and_then(Value::as_str).unwrap_or("a scheduled prompt");
        out.push(format!("cron: {what}"));
    }
    out
}

// ----- the report ------------------------------------------------------------------------------

/// A file a turn changed, from `git diff --numstat`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDelta {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

/// What a turn did (design section 8), with what could not be known marked so.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnReport {
    pub v: u16,
    pub session: String,
    pub turn: String,
    pub started_ms: u64,
    pub ended_ms: u64,
    pub end: TurnEnd,
    pub tools: u32,
    pub failed_tools: Vec<String>,
    /// The commands it ran, in order.
    pub commands: Vec<String>,
    pub files: Vec<FileDelta>,
    /// The commits made in the turn (`<short sha> <subject>`), and whether they were pushed.
    pub commits: Vec<String>,
    pub pushed: bool,
    pub last_reply: String,
    pub question: Option<String>,
    /// What a paused turn left running.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background: Vec<String>,
    /// The prompt came from an agent, not from anyone typing.
    #[serde(default)]
    pub by_agent: bool,
    /// The transcript's part of it is missing or was cut: what it says is what could be read.
    #[serde(default)]
    pub partial: bool,
}

/// What the report reads off the main thread: the transcript between the turn's bounds, and git.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gathered {
    pub items: Vec<TranscriptItem>,
    pub files: Vec<FileDelta>,
    pub commits: Vec<String>,
    pub pushed: bool,
    /// The transcript could not be read, or was cut at the size cap.
    pub partial: bool,
}

/// Read what a closed turn's report needs: wait for the transcript to settle (at most
/// [`TRANSCRIPT_LAG`], when `wait`), read it from the turn's start, and ask git what changed since
/// the head at the turn's start. It runs git and reads files: off the main thread.
pub fn gather(agent: AgentKind, transcript: Option<&Path>, closed: &Closed, worktree: &Path, wait: bool) -> Gathered {
    let mut out = Gathered::default();
    if let Some(path) = transcript {
        if wait {
            settle(path);
        }
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let from = closed.turn.transcript_offset.unwrap_or(len);
        let from = if len.saturating_sub(from) > REPORT_BYTES {
            out.partial = true;
            len - REPORT_BYTES
        } else {
            from
        };
        match crate::transcript::read(agent, path, Some(from.min(len)), usize::MAX) {
            Ok(page) => out.items = page.items,
            Err(_) => out.partial = true,
        }
    } else if agent == AgentKind::Claude {
        out.partial = true;
    }
    if let Some(head) = closed.turn.head.as_deref().filter(|h| h.len() >= 7) {
        let git = |args: &[&str]| -> Option<String> {
            let output = crate::sys::command("git").arg("-C").arg(worktree).args(args).output().ok()?;
            output.status.success().then(|| String::from_utf8_lossy(&output.stdout).to_string())
        };
        if let Some(numstat) = git(&["diff", "--numstat", head]) {
            out.files = numstat
                .lines()
                .filter_map(|line| {
                    let mut parts = line.split('\t');
                    let (added, removed, path) = (parts.next()?, parts.next()?, parts.next()?);
                    Some(FileDelta { path: path.to_string(), added: added.parse().unwrap_or(0), removed: removed.parse().unwrap_or(0) })
                })
                .collect();
        }
        if let Some(log) = git(&["log", "--format=%h %ct %s", &format!("{head}..HEAD")]) {
            out.commits = made_in_turn(&log, closed.turn.started_ms);
        }
        if !out.commits.is_empty() {
            // Pushed: the branch's upstream has the turn's last commit.
            let now = git(&["rev-parse", "HEAD"]).map(|s| s.trim().to_string());
            let upstream = git(&["rev-parse", "@{upstream}"]).map(|s| s.trim().to_string());
            out.pushed = now.is_some() && now == upstream;
        }
    }
    out
}

/// The commits a turn made, from `git log --format="%h %ct %s" <head>..HEAD`: those committed
/// since the second it started (`<short sha> <subject>`). A commit the branch only reached in the
/// turn (a merge of another branch, a reset or a fast-forward onto one) was committed before it,
/// so it is not the turn's: a brokered push sends only what the session made (D13, P07).
fn made_in_turn(log: &str, started_ms: u64) -> Vec<String> {
    let since = started_ms / 1000;
    log.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            let (sha, at, subject) = (parts.next()?, parts.next()?.parse::<u64>().ok()?, parts.next().unwrap_or_default());
            (at >= since).then(|| format!("{sha} {subject}"))
        })
        .collect()
}

/// Wait until a file stops growing (300 ms without a change), for at most [`TRANSCRIPT_LAG`].
fn settle(path: &Path) {
    let start = Instant::now();
    let mut last = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut still = Instant::now();
    while start.elapsed() < TRANSCRIPT_LAG {
        std::thread::sleep(Duration::from_millis(100));
        let now = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if now != last {
            last = now;
            still = Instant::now();
        } else if still.elapsed() >= Duration::from_millis(300) && start.elapsed() >= Duration::from_millis(300) {
            return;
        }
    }
}

/// The tool names that run a command.
const COMMAND_TOOLS: [&str; 5] = ["Bash", "exec_command", "shell", "local_shell", "unified_exec"];

/// The report of a closed turn, from its end, its transcript items and git (no IO).
/// The report of a turn the app's quit cuts, read at once (there is no time to wait for its
/// transcript to settle): its tools, commands and reply from what the transcript holds now, its
/// files and commits, as any turn's ([`gather`] without the wait). Before, a quit reported none of
/// it ("cut by Glasscode quitting: 0 tools" with a command running, the live checks).
pub fn report_now(session: &str, agent: AgentKind, transcript: Option<&Path>, closed: &Closed, worktree: &Path) -> TurnReport {
    report(session, closed, &gather(agent, transcript, closed, worktree, false))
}

pub fn report(session: &str, closed: &Closed, gathered: &Gathered) -> TurnReport {
    let tools: Vec<&TranscriptItem> = gathered.items.iter().filter(|item| item.kind == ItemKind::Tool).collect();
    let failed_tools = tools.iter().filter(|item| item.failed).map(|item| item.tool.clone().unwrap_or_else(|| item.text.clone())).collect();
    let commands = tools.iter().filter(|item| item.tool.as_deref().is_some_and(|tool| COMMAND_TOOLS.contains(&tool))).map(|item| item.text.clone()).collect();
    let from_transcript = gathered.items.iter().rev().find(|item| item.kind == ItemKind::Assistant).map(|item| item.text.clone());
    let reply = closed.reply.clone().filter(|r| !r.trim().is_empty()).or(from_transcript).unwrap_or_default();
    let reply = crate::transcript::clip(&reply, REPLY_CHARS);
    // Signals first (a permission prompt, a question it raised), then the text.
    let question = closed.turn.question.clone().or_else(|| {
        let last = reply.trim_end().trim_end_matches('…');
        last.ends_with('?').then(|| last.rsplit(['.', '!', '\n']).next().unwrap_or(last).trim().to_string())
    });
    let partial = gathered.partial || (reply.is_empty() && matches!(closed.end, TurnEnd::Completed | TurnEnd::Paused));
    TurnReport {
        v: REPORT_VERSION,
        session: session.to_string(),
        turn: closed.turn.id.clone(),
        started_ms: closed.turn.started_ms,
        ended_ms: closed.ended_ms,
        end: closed.end.clone(),
        tools: tools.len() as u32,
        failed_tools,
        commands,
        files: gathered.files.clone(),
        commits: gathered.commits.clone(),
        pushed: gathered.pushed,
        last_reply: reply,
        question,
        background: closed.background.clone(),
        by_agent: closed.turn.by_agent,
        partial,
    }
}

impl TurnReport {
    /// One line for a person: "completed in 42 s: 3 tools, 2 files, 1 commit (pushed)".
    pub fn line(&self) -> String {
        let secs = self.ended_ms.saturating_sub(self.started_ms) / 1000;
        let mut parts = vec![format!("{} tools", self.tools)];
        if !self.files.is_empty() {
            parts.push(format!("{} files", self.files.len()));
        }
        if !self.commits.is_empty() {
            parts.push(format!("{} commits{}", self.commits.len(), if self.pushed { " (pushed)" } else { "" }));
        }
        if !self.failed_tools.is_empty() {
            parts.push(format!("{} failed", self.failed_tools.len()));
        }
        if !self.background.is_empty() {
            parts.push(format!("still running: {}", self.background.join(", ")));
        }
        format!("{} in {secs} s: {}", self.end.words(), parts.join(", "))
    }
}

// ----- journal events --------------------------------------------------------------------------

/// A turn opened.
pub fn started_event(session: &str, project: Option<&ProjectKey>, turn: &OpenTurn, prompt: Option<&str>) -> Event {
    let mut data = json!({ "turn": turn.id, "by_agent": turn.by_agent, "head": turn.head });
    if let Some(prompt) = prompt {
        // The journal keeps it as a hash and an excerpt.
        data["prompt"] = Value::String(prompt.to_string());
    }
    Event::new(kinds::TURN_STARTED, Source::App, format!("turn {}", turn.id)).session(Some(session)).project(project).data(data)
}

/// A turn's report, back from its `turn.ended` event (whose reply is kept as `reply`).
pub fn report_of(event: &Event) -> Option<TurnReport> {
    let mut data = event.data.clone();
    let map = data.as_object_mut()?;
    let reply = map.remove("reply").map(|r| match r {
        Value::String(text) => text,
        other => other["text"].as_str().or_else(|| other["excerpt"].as_str()).unwrap_or_default().to_string(),
    });
    map.insert("last_reply".into(), Value::String(reply.unwrap_or_default()));
    serde_json::from_value(data).ok()
}

/// What a session's last turns did, in a few lines and no model (land-18): written into its work
/// file before Claude compacts the conversation, so that after it the session can still say what
/// it was doing. Newest last; within `max_chars`, the oldest go first.
pub fn compaction_digest(reports: &[TurnReport], max_chars: usize) -> String {
    let clip = |text: &str, n: usize| {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.chars().count() <= n { text } else { format!("{}…", text.chars().take(n).collect::<String>()) }
    };
    let mut lines: Vec<String> = reports
        .iter()
        .rev()
        .take(5)
        .rev()
        .map(|r| {
            let mut parts = vec![r.end.words()];
            if !r.commands.is_empty() {
                let last: Vec<String> = r.commands.iter().rev().take(3).rev().map(|c| format!("`{}`", clip(c, 80))).collect();
                parts.push(format!("ran {}", last.join(", ")));
            }
            if !r.failed_tools.is_empty() {
                parts.push(format!("failed: {}", r.failed_tools.iter().map(|t| clip(t, 80)).collect::<Vec<_>>().join(", ")));
            }
            if !r.files.is_empty() {
                let files: Vec<&str> = r.files.iter().take(5).map(|f| f.path.as_str()).collect();
                parts.push(format!("changed {}{}", files.join(", "), if r.files.len() > 5 { format!(" and {} more", r.files.len() - 5) } else { String::new() }));
            }
            if !r.commits.is_empty() {
                parts.push(format!("committed {}", r.commits.iter().take(3).map(|c| clip(c, 60)).collect::<Vec<_>>().join(", ")));
            }
            if let Some(question) = &r.question {
                parts.push(format!("asked: {}", clip(question, 120)));
            } else if !r.last_reply.is_empty() {
                parts.push(format!("said: {}", clip(&r.last_reply, 160)));
            }
            format!("- {}", parts.join("; "))
        })
        .collect();
    while lines.len() > 1 && lines.iter().map(|l| l.len() + 1).sum::<usize>() > max_chars {
        lines.remove(0);
    }
    lines.join("\n")
}

/// A turn ended, with its report.
pub fn ended_event(project: Option<&ProjectKey>, report: &TurnReport) -> Event {
    let severity = match report.end {
        TurnEnd::Failed(_) | TurnEnd::Crashed => crate::journal::Severity::Warn,
        TurnEnd::Completed | TurnEnd::Paused if report.question.is_some() => crate::journal::Severity::Notice,
        _ => crate::journal::Severity::Info,
    };
    let mut data = serde_json::to_value(report).unwrap_or(Value::Null);
    // The reply is the agent's text: kept as an excerpt with its hash.
    if let Some(map) = data.as_object_mut() {
        let reply = map.remove("last_reply").unwrap_or(Value::Null);
        map.insert("reply".into(), reply);
    }
    Event::new(kinds::TURN_ENDED, Source::App, report.line()).session(Some(&report.session)).project(project).data(data).severity(severity)
}

/// What a session said about its own work (`glasscode report`, events-23): a hint, at most 200
/// characters, never evidence; `signed` says whether the session's signature checked out.
pub fn self_report_event(session: &str, project: Option<&ProjectKey>, turn: Option<&str>, state: &str, line: &str, signed: bool) -> Event {
    let line: String = line.chars().take(200).collect();
    Event::new(kinds::SESSION_REPORTED, Source::Control, format!("says {state}: {line}"))
        .session(Some(session))
        .project(project)
        .data(json!({ "state": state, "text": line, "turn": turn, "signed": signed, "hint": true, "evidence": false }))
}

/// A session started outside Glasscode works in a known project (land-9): listed, never acted on.
pub fn outside_event(project: Option<&ProjectKey>, agent: AgentKind, conversation: &str, folder: &Path, title: &str) -> Event {
    Event::new(kinds::SESSION_OUTSIDE, Source::App, format!("{} works outside Glasscode in {}: {title}", agent.name(), folder.display()))
        .project(project)
        .data(json!({ "agent": agent.name(), "conversation": conversation, "folder": folder.display().to_string(), "title": title }))
}

/// The self-report states `glasscode report` takes.
pub const REPORT_STATES: [&str; 3] = ["done", "blocked", "needs-review"];

/// The open turns a stretch of the journal leaves, by session: started, and not ended.
pub fn open_in(events: &[Event]) -> Vec<(String, OpenTurn)> {
    let mut open: std::collections::BTreeMap<String, OpenTurn> = std::collections::BTreeMap::new();
    for event in events {
        let Some(session) = event.session.clone() else { continue };
        let turn = event.data.get("turn").and_then(Value::as_str).unwrap_or_default().to_string();
        if event.kind == kinds::TURN_STARTED {
            let head = event.data.get("head").and_then(Value::as_str).map(str::to_string);
            let by_agent = event.data.get("by_agent").and_then(Value::as_bool).unwrap_or(false);
            open.insert(session, OpenTurn { id: turn, started_ms: event.at_ms(), by_agent, head, ..OpenTurn::default() });
        } else if event.kind == kinds::TURN_ENDED && open.get(&session).is_some_and(|o| o.id == turn) {
            open.remove(&session);
        }
    }
    open.into_iter().collect()
}

/// Where a session's transcript is now, for a turn's baseline.
pub fn transcript_len(path: Option<&PathBuf>) -> Option<u64> {
    std::fs::metadata(path?).ok().map(|m| m.len())
}

/// How far back a turn's start looks for its prompt in the transcript.
const PROMPT_LOOK_BACK: u64 = 64 * 1024;

/// Where a turn starts in its transcript: at the line of its prompt (Claude's user message with
/// that text), when the transcript already holds it (a short turn whose hooks were read together,
/// after the agent wrote it all); else at the transcript's end, as before. Only a line after
/// `after` (where the session's turn before started) counts: a prompt said again ("continue")
/// whose line is not written yet is not the turn before's. Reads at most the last
/// [`PROMPT_LOOK_BACK`] bytes.
pub fn turn_offset(path: Option<&PathBuf>, prompt: Option<&str>, after: Option<u64>) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let path = path?;
    let len = std::fs::metadata(path).ok()?.len();
    let Some(prompt) = prompt.map(str::trim).filter(|p| !p.is_empty()) else { return Some(len) };
    let start = len.saturating_sub(PROMPT_LOOK_BACK);
    let mut text = Vec::new();
    let read = std::fs::File::open(path).and_then(|mut file| {
        file.seek(SeekFrom::Start(start))?;
        file.take(len - start).read_to_end(&mut text)
    });
    if read.is_err() {
        return Some(len);
    }
    let mut found = None;
    let mut at = start;
    for (i, line) in text.split(|b| *b == b'\n').enumerate() {
        let here = at;
        at += line.len() as u64 + 1;
        // A first line cut by the look-back is no line.
        if i == 0 && start > 0 {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else { continue };
        if value["type"] != "user" {
            continue;
        }
        let content = &value["message"]["content"];
        let said = match content {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(""),
            _ => continue,
        };
        if said.trim() == prompt && after.is_none_or(|after| here > after) {
            found = Some(here);
        }
    }
    Some(found.unwrap_or(len))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A short turn whose hooks are read after the agent wrote it all starts at its prompt's line,
    /// so its report has its tools (found in the journey run: "0 tools").
    /// A turn the quit cuts while its command runs reports that command, read from the transcript
    /// at once (the live checks: "cut by Glasscode quitting in 19 s: 0 tools").
    #[test]
    fn a_turn_cut_by_the_quit_reports_its_tools() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let earlier = r#"{"type":"user","message":{"role":"user","content":"Earlier"}}"#;
        let prompt = r#"{"type":"user","message":{"role":"user","content":"Run sh wait40.sh"}}"#;
        let tool = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"sh wait40.sh"}}]}}"#;
        std::fs::write(&path, format!("{earlier}\n{prompt}\n{tool}\n")).unwrap();
        let offset = turn_offset(Some(&path), Some("Run sh wait40.sh"), None);
        let closed = Closed { turn: OpenTurn { id: "p1".into(), started_ms: 1_000, transcript_offset: offset, ..OpenTurn::default() }, end: TurnEnd::AppQuit, ended_ms: 20_000, reply: None, background: Vec::new() };
        let report = report_now("s1", AgentKind::Claude, Some(&path), &closed, dir.path());
        assert_eq!(report.tools, 1, "{report:?}");
        assert_eq!(report.commands, ["sh wait40.sh"]);
        assert_eq!(report.end, TurnEnd::AppQuit);
    }

    #[test]
    fn a_short_turn_starts_at_its_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let earlier = r#"{"type":"user","message":{"role":"user","content":"Earlier"}}"#;
        let prompt = r#"{"type":"user","message":{"role":"user","content":"Fix the typo"}}"#;
        let tool = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"README.md"}}]}}"#;
        std::fs::write(&path, format!("{earlier}\n{prompt}\n{tool}\n")).unwrap();
        let offset = turn_offset(Some(&path), Some("Fix the typo"), None).unwrap();
        assert_eq!(offset, earlier.len() as u64 + 1, "the prompt's line");
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(turn_offset(Some(&path), Some("Not there"), None), Some(len), "else the end, as before");
        assert_eq!(turn_offset(Some(&path), None, None), Some(len));
        assert_eq!(turn_offset(None, Some("Fix the typo"), None), None);
        // The same prompt again, its line not written yet: not the turn before's line.
        assert_eq!(turn_offset(Some(&path), Some("Fix the typo"), Some(offset)), Some(len), "said again");
        assert_eq!(turn_offset(Some(&path), Some("Fix the typo"), Some(offset - 1)), Some(offset));
    }
    use crate::testkit::clock::{VirtualClock, Zone};
    use crate::testkit::sim::{self, Input, Record, Runtime};

    fn hook(name: &str, more: Value) -> Signal {
        let mut payload = json!({ "hook_event_name": name });
        if let (Some(map), Value::Object(more)) = (payload.as_object_mut(), more) {
            map.extend(more);
        }
        Signal::Hook(payload)
    }

    fn rollout(kind: &str, more: Value) -> Signal {
        let mut payload = json!({ "type": kind });
        if let (Some(map), Value::Object(more)) = (payload.as_object_mut(), more) {
            map.extend(more);
        }
        Signal::Rollout(json!({ "type": "event_msg", "payload": payload }))
    }

    fn ends(changes: &[Change]) -> Vec<String> {
        changes.iter().filter_map(|c| if let Change::Closed(closed) = c { Some(closed.end.name().to_string()) } else { None }).collect()
    }

    fn run(machine: &mut TurnMachine, signals: &[Signal]) -> Vec<Change> {
        signals.iter().enumerate().flat_map(|(n, s)| machine.feed(1_000 + n as u64 * 100, s)).collect()
    }

    #[test]
    fn a_stop_hook_continuation_is_one_turn() {
        // P05: the first Stop (blocked by a stop hook), a subagent, then the continuation's Stop.
        let mut machine = TurnMachine::default();
        let changes = run(
            &mut machine,
            &[
                hook("UserPromptSubmit", json!({ "prompt_id": "p1", "prompt": "check the readme" })),
                hook("CwdChanged", json!({ "prompt_id": "p1", "old_cwd": "/a", "new_cwd": "/b" })),
                hook("SubagentStart", json!({ "agent_id": "a1", "agent_type": "Explore", "prompt_id": "p1" })),
                hook("Stop", json!({ "prompt_id": "p1", "stop_hook_active": false, "last_assistant_message": "first", "background_tasks": [] })),
                hook("Stop", json!({ "prompt_id": "p1", "stop_hook_active": true, "last_assistant_message": "CONTINUED-58", "background_tasks": [] })),
                hook("SubagentStop", json!({ "agent_id": "a1", "agent_type": "Explore", "prompt_id": "p1" })),
            ],
        );
        assert_eq!(ends(&changes), ["Completed"]);
        assert_eq!(changes.iter().filter(|c| matches!(c, Change::Opened(_))).count(), 1);
    }

    #[test]
    fn subagent_internal_ignored() {
        let mut machine = TurnMachine::default();
        let changes = run(
            &mut machine,
            &[
                hook("UserPromptSubmit", json!({ "prompt_id": "p1" })),
                hook("Stop", json!({ "prompt_id": "p1", "last_assistant_message": "done", "background_tasks": [] })),
                // Claude's own helper, 1.7 s after the Stop (P05): no turn, no second report.
                hook("SubagentStop", json!({ "agent_id": "x", "agent_type": "", "prompt_id": "p1", "last_assistant_message": "{\"title\":\"Fix\"}" })),
                hook("Notification", json!({ "notification_type": "idle_prompt", "message": "Claude is waiting for your input" })),
            ],
        );
        assert_eq!(ends(&changes), ["Completed"]);
        assert!(machine.open().is_none());
    }

    #[test]
    fn a_turn_with_background_tasks_is_paused_and_names_them() {
        let mut machine = TurnMachine::default();
        let changes = run(
            &mut machine,
            &[
                hook("UserPromptSubmit", json!({ "prompt_id": "p1" })),
                hook("Stop", json!({ "prompt_id": "p1", "last_assistant_message": "Started the explorer", "background_tasks": [{ "id": "b1", "type": "subagent", "status": "running", "description": "map the repo", "agent_type": "Explore" }], "session_crons": [] })),
                // The hand-back starts a turn nobody typed.
                hook("UserPromptSubmit", json!({ "prompt_id": "p2", "prompt": "<agent-message from=\"a1\">[Subagent hand-back] done" })),
            ],
        );
        let Change::Closed(closed) = &changes[1] else { panic!("{changes:?}") };
        assert_eq!((closed.end.clone(), closed.background.clone()), (TurnEnd::Paused, vec!["map the repo".to_string()]));
        let Change::Opened(next) = &changes[2] else { panic!() };
        assert!(next.by_agent);
        let report = report("s1", closed, &Gathered::default());
        assert!(report.line().contains("still running: map the repo"), "{}", report.line());
    }

    #[test]
    fn every_end_closes_once() {
        let cases: Vec<(Vec<Signal>, &str)> = vec![
            (vec![hook("StopFailure", json!({ "prompt_id": "p1", "error": "rate_limit", "last_assistant_message": "API Error: 429" }))], "Failed"),
            (vec![hook("SessionEnd", json!({ "reason": "other" }))], "Aborted"),
            (vec![Signal::Exited(Some(1))], "Aborted"),
            (vec![Signal::Exited(Some(137))], "Crashed"),
            (vec![Signal::Killed], "Crashed"),
            (vec![Signal::Watchdog { alive: true, quiet_ms: 600_000 }, Signal::Watchdog { alive: false, quiet_ms: 10_000 }, Signal::Watchdog { alive: false, quiet_ms: CRASH_QUIET_MS }], "Crashed"),
            (vec![Signal::AppQuit], "AppQuit"),
            (vec![Signal::Slept], "Slept"),
            (vec![Signal::TranscriptLine(r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#.into())], "Interrupted"),
            // A new prompt while the turn is open: the one before was interrupted.
            (vec![hook("UserPromptSubmit", json!({ "prompt_id": "p2" }))], "Interrupted"),
        ];
        for (signals, end) in cases {
            let mut machine = TurnMachine::default();
            let mut all = vec![hook("UserPromptSubmit", json!({ "prompt_id": "p1" }))];
            all.extend(signals);
            // Each signal again: nothing more closes.
            let again = all[1..].to_vec();
            all.extend(again);
            let changes = run(&mut machine, &all);
            assert_eq!(ends(&changes), [end], "{changes:?}");
        }
    }

    #[test]
    fn codex_turns_come_from_the_rollout() {
        let mut machine = TurnMachine::default();
        let changes = run(
            &mut machine,
            &[
                rollout("task_started", json!({ "turn_id": "x1" })),
                rollout("task_complete", json!({ "turn_id": "x1", "last_agent_message": "dev=DEV-V1" })),
                rollout("task_started", json!({ "turn_id": "x2" })),
                rollout("error", json!({ "message": "exceeded retry limit, last status: 429 Too Many Requests" })),
                rollout("task_complete", json!({ "turn_id": "x2", "last_agent_message": null })),
                rollout("task_started", json!({ "turn_id": "x3" })),
                rollout("turn_aborted", json!({ "turn_id": "x3", "reason": "interrupted" })),
            ],
        );
        assert_eq!(ends(&changes), ["Completed", "Failed", "Interrupted"]);
        let Change::Closed(first) = &changes[1] else { panic!() };
        assert_eq!(first.reply.as_deref(), Some("dev=DEV-V1"));
    }

    #[test]
    fn self_report_is_hint() {
        let event = self_report_event("s1", None, Some("p1"), "done", &"x".repeat(500), false);
        assert_eq!(event.kind, kinds::SESSION_REPORTED);
        assert_eq!((event.data["hint"].as_bool(), event.data["evidence"].as_bool(), event.data["signed"].as_bool()), (Some(true), Some(false), Some(false)));
        assert_eq!(event.data["text"].as_str().unwrap().chars().count(), 200);
    }

    #[test]
    fn a_report_reads_its_turn() {
        let closed = Closed {
            turn: OpenTurn { id: "p1".into(), started_ms: 1_000, question: None, ..OpenTurn::default() },
            end: TurnEnd::Completed,
            ended_ms: 43_000,
            reply: Some("The fix is ready. Should I push it and open a pull request?".into()),
            background: Vec::new(),
        };
        let item = |kind: ItemKind, text: &str, tool: Option<&str>, failed: bool| TranscriptItem { kind, text: text.into(), tool: tool.map(str::to_string), failed, time: None };
        let gathered = Gathered {
            items: vec![item(ItemKind::User, "fix it", None, false), item(ItemKind::Tool, "bun test", Some("Bash"), true), item(ItemKind::Tool, "src/a.ts", Some("Edit"), false), item(ItemKind::Assistant, "older text", None, false)],
            files: vec![FileDelta { path: "src/a.ts".into(), added: 3, removed: 1 }],
            commits: vec!["1a2b3c4 Fix the login".into()],
            pushed: true,
            partial: false,
        };
        let report = report("s1", &closed, &gathered);
        assert_eq!((report.tools, report.failed_tools.clone(), report.commands.clone()), (2, vec!["Bash".to_string()], vec!["bun test".to_string()]));
        // The Stop's reply wins over the transcript's, which may lag.
        assert!(report.last_reply.starts_with("The fix is ready"));
        assert_eq!(report.question.as_deref(), Some("Should I push it and open a pull request?"));
        assert_eq!(report.line(), "completed in 42 s: 2 tools, 1 files, 1 commits (pushed), 1 failed");
        let event = ended_event(None, &report);
        assert_eq!(event.kind, kinds::TURN_ENDED);
        assert!(event.data.get("last_reply").is_none() && event.data["reply"].is_string());
    }

    #[test]
    fn open_turns_survive_in_the_journal() {
        let turn = OpenTurn { id: "p1".into(), started_ms: 5, ..OpenTurn::default() };
        let started = started_event("s1", None, &turn, Some("hi"));
        let ended = ended_event(None, &report("s1", &Closed { turn: turn.clone(), end: TurnEnd::Completed, ended_ms: 9, reply: None, background: Vec::new() }, &Gathered::default()));
        let other = started_event("s2", None, &OpenTurn { id: "q1".into(), ..OpenTurn::default() }, None);
        let open = open_in(&[started.clone(), other.clone(), ended]);
        assert_eq!(open.iter().map(|(s, t)| (s.as_str(), t.id.as_str())).collect::<Vec<_>>(), [("s2", "q1")]);
        assert_eq!(open_in(&[started]).len(), 1);
    }

    /// The runtime of the simulations: a machine per session, fed what the stand-ins do.
    #[derive(Default)]
    struct Turns {
        machines: std::collections::BTreeMap<String, TurnMachine>,
    }

    impl Turns {
        fn closed(&self, session: &str, changes: Vec<Change>, record: &mut Record) {
            for change in changes {
                if let Change::Closed(closed) = change {
                    record.reports.push((session.to_string(), closed.turn.id.clone(), closed.end.name().to_string()));
                }
            }
        }
    }

    impl Runtime for Turns {
        fn input(&mut self, now: u64, input: &Input, record: &mut Record) {
            let signal = match input {
                Input::Hook { payload, .. } => Signal::Hook(serde_json::from_str(payload).unwrap()),
                Input::Rollout { line, .. } => Signal::Rollout(serde_json::from_str(line).unwrap()),
                Input::Write { text, .. } => Signal::TranscriptLine(text.clone()),
                Input::Kill { .. } => Signal::Killed,
                Input::Restart { crashed } => {
                    let signal = if *crashed { Signal::Killed } else { Signal::AppQuit };
                    let sessions: Vec<String> = self.machines.keys().cloned().collect();
                    for session in sessions {
                        let changes = self.machines.get_mut(&session).unwrap().feed(now, &signal);
                        self.closed(&session, changes, record);
                    }
                    return;
                }
                Input::Sleep { .. } => {
                    let sessions: Vec<String> = self.machines.keys().cloned().collect();
                    for session in sessions {
                        let changes = self.machines.get_mut(&session).unwrap().feed(now, &Signal::Slept);
                        self.closed(&session, changes, record);
                    }
                    return;
                }
                _ => return,
            };
            let Some(session) = input.session().map(str::to_string) else { return };
            let changes = self.machines.entry(session.clone()).or_default().feed(now, &signal);
            self.closed(&session, changes, record);
        }
    }

    /// Both agents, with the stand-ins: a completed turn, an interrupt, a rate-limit failure, a
    /// `kill -9`, an app restart mid-turn and sleep mid-turn each give exactly one report, with
    /// the right cause.
    #[test]
    fn the_stand_ins_give_one_report_per_turn_with_its_cause() {
        let dir = tempfile::tempdir().unwrap();
        let start = 1_000_000;
        for case in sim::cases(dir.path(), start) {
            let mut runtime = Turns::default();
            let record = sim::run(&VirtualClock::new(start, Zone::utc()), &case.script, &mut runtime, 1_000);
            assert_eq!(sim::one_report_per_turn(&case.script, &record), Ok(()), "{}", case.name);
            let ends: Vec<(&str, &str)> = record.reports.iter().map(|(s, _, e)| (s.as_str(), e.as_str())).collect();
            assert_eq!(ends, case.ends, "{}", case.name);
        }
    }

    /// M30's Verify line: one report per turn over the reference load.
    #[test]
    fn one_report_per_turn_over_the_reference_load() {
        let start = crate::testkit::clock::utc_ms(2026, 11, 19, 0, 0);
        let script = sim::Script { items: crate::testkit::load::events(crate::testkit::load::Reference::small(), start, 1, 7).collect() };
        let record = sim::run(&VirtualClock::new(start, Zone::utc()), &script, &mut Turns::default(), 0);
        assert!(script.turns_opened().values().sum::<usize>() > 20_000);
        assert_eq!(sim::one_report_per_turn(&script, &record), Ok(()));
    }

    /// land-18: the last turns, in a few lines with no model, from their journal events; the
    /// failing command and what the session asked are in it.
    #[test]
    fn the_last_turns_are_digested_before_a_compaction() {
        let report = |end: TurnEnd, command: &str, question: Option<&str>| TurnReport {
            v: 1,
            session: "s1".into(),
            turn: "t".into(),
            started_ms: 0,
            ended_ms: 1000,
            end,
            tools: 3,
            failed_tools: if command.contains("test") { vec![format!("Bash: {command}")] } else { Vec::new() },
            commands: vec!["git status".into(), command.into()],
            files: vec![FileDelta { path: "src/login.rs".into(), added: 3, removed: 1 }],
            commits: Vec::new(),
            pushed: false,
            last_reply: "The login test still fails on the token check.".into(),
            question: question.map(str::to_string),
            background: Vec::new(),
            by_agent: false,
            partial: false,
        };
        let first = report(TurnEnd::Completed, "cargo fmt", None);
        let event = ended_event(None, &first);
        assert_eq!(report_of(&event).as_ref(), Some(&first), "the report comes back from its event");
        let second = report(TurnEnd::Failed("tests".into()), "cargo test -p login", Some("Shall I change the token check?"));
        let digest = compaction_digest(&[first.clone(), second.clone()], 2000);
        let lines: Vec<&str> = digest.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("`cargo test -p login`") && lines[1].contains("failed: Bash: cargo test -p login") && lines[1].contains("asked: Shall I change"), "{digest}");
        assert!(lines[0].contains("changed src/login.rs") && lines[0].contains("said: The login test"), "{digest}");
        let short = compaction_digest(&[first, second], 200);
        assert_eq!(short.lines().count(), 1, "the oldest goes first: {short}");
        assert!(short.contains("cargo test -p login"));
    }

}
