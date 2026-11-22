//! Stand-in agents (M02 step 2): what a test runs instead of `claude` and `codex`.
//!
//! A [`Scenario`] is data: the steps a CLI takes when it starts, in each turn (one per prompt), and
//! when its input ends. A step writes one line of the agent's own files (Claude's transcript,
//! Codex's rollout and `history.jsonl`), gives a hook event to the commands the session's
//! `--settings` name, prints a line, waits, exits, or dies as under `kill -9`. Its text is kept as
//! written, with `{{name}}` placeholders for what only the run knows (the session id, the folder,
//! the time, the prompt); a placeholder is filled with its value escaped for a JSON string. So a
//! turn that M01 records from the real CLI becomes a tape by putting placeholders where the ids
//! were, and is replayed byte for byte.
//!
//! Two players replay the same scenario: [`Player`], in this process (simulations, scratch homes),
//! and the scripts `script/test-agents/claude` and `codex` (a real process with arguments, hooks
//! and an exit status). A test checks that both write the same bytes.
//!
//! The scenarios of [`claude`] and [`codex`] are **synthesized** from what Glasscode's readers parse
//! (`transcript.rs`, `history.rs`, `agents.rs`) and the CLIs' documented hook payloads, for
//! claude 2.1.322 and codex 0.167.0. Recorded tapes from M01 replace them as fixtures; until then,
//! the shapes the proofs measure (P05: `StopFailure`, the interrupt marker; P06: Codex turn ends)
//! are this kit's best reading, not facts.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::clock::iso_ms;

pub const CLAUDE_VERSION: &str = "2.1.322";
pub const CODEX_VERSION: &str = "0.167.0";

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// One step of a scenario.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Milliseconds after the start of its part (the start, a turn, the end), in virtual time: it
    /// sets the step's `{{ts}}`, and nothing waits for it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub at: u64,
    #[serde(flatten)]
    pub what: What,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub enum What {
    /// A line of Claude's transcript (`<config>/projects/<folder>/<id>.jsonl`).
    Transcript { text: String },
    /// A line of Codex's rollout (`<CODEX_HOME>/sessions/…/rollout-….jsonl`).
    Rollout { text: String },
    /// A line of Codex's `history.jsonl`.
    History { text: String },
    /// A hook event: `text` goes on the standard input of each command the settings give `event`.
    Hook { event: String, text: String },
    /// A line the CLI prints.
    Stdout { text: String },
    /// A tool call that a `PreToolUse` hook may refuse: `pre` is the hook's payload, and the steps
    /// of `allowed` or of `denied` follow (`{{reason}}` holds the refusal in `denied`).
    Tool { pre: String, allowed: Vec<Step>, denied: Vec<Step> },
    /// A command the tool call runs, for a harness that must tell an effect a hook refused from one
    /// that happened: the scripts run it in the session's folder (its output is not shown), and only
    /// in a scratch home; the player in this process gives it to its sink. The synthesized
    /// scenarios have none, so a stand-in started anywhere else changes nothing.
    Run { command: String },
    /// Real time passes (a transcript that lags behind its hook).
    Sleep { ms: u64 },
    /// The process ends with `code`.
    Exit { code: i32 },
    /// The process dies at once, as under `kill -9`: no hook, no last line.
    Crash,
}

impl Step {
    pub fn new(at: u64, what: What) -> Step {
        Step { at, what }
    }
}

/// What a stand-in does, from start to end.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scenario {
    /// `claude` or `codex`.
    pub agent: String,
    /// The version it acts as; `--version` and `--help` answer from `script/cli-help/`.
    pub version: String,
    /// Where its steps come from: "synthesized (M02)", or the CLI version M01 recorded them with.
    #[serde(default)]
    pub source: String,
    /// Placeholders with a fixed value (a session id, `now_ms`: the start in virtual time).
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub start: Vec<Step>,
    /// One per prompt; the last one again for every prompt after them.
    #[serde(default)]
    pub turns: Vec<Vec<Step>>,
    /// When the input ends.
    #[serde(default)]
    pub end: Vec<Step>,
}

impl Scenario {
    pub fn load(path: &Path) -> anyhow::Result<Scenario> {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// The steps of a tape: one step a line, the form M01's recordings take.
    pub fn read_tape(text: &str) -> anyhow::Result<Vec<Step>> {
        text.lines().filter(|line| !line.trim().is_empty()).map(|line| Ok(serde_json::from_str(line)?)).collect()
    }

    pub fn write_tape(steps: &[Step]) -> String {
        steps.iter().map(|step| serde_json::to_string(step).expect("a step is JSON") + "\n").collect()
    }

    /// The steps of turn `n` (from 1): the last turn for every prompt past them.
    pub fn turn(&self, n: usize) -> &[Step] {
        self.turns.get(n.saturating_sub(1)).or(self.turns.last()).map_or(&[], Vec::as_slice)
    }
}

/// `text` with each `{{name}}` of `vars` replaced by its value, escaped for a JSON string; other
/// text, and a placeholder with no value, stay as they are. The scripts' `fill` does the same
/// (a regular expression `\{\{([a-z0-9_]+)\}\}`, first match first).
pub fn fill(text: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let name = after.find("}}").map(|end| &after[..end]).filter(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
        match name.and_then(|name| vars.get(name).map(|value| (name, value))) {
            Some((name, value)) => {
                out.push_str(&escape(value));
                rest = &after[name.len() + 2..];
            }
            None => {
                // On by one character, as the expression would look again from there.
                out.push('{');
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A value as the inside of a JSON string.
pub fn escape(value: &str) -> String {
    let quoted = serde_json::to_string(value).expect("a string is JSON");
    quoted[1..quoted.len() - 1].to_string()
}

/// Which file of the agent a line goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Out {
    Transcript,
    Rollout,
    History,
}

/// Where a [`Player`] puts what it plays.
pub trait Sink {
    /// The virtual time of the step that follows, in milliseconds.
    fn time(&mut self, _ms: u64) {}
    fn line(&mut self, out: Out, text: &str);
    /// A hook event; for `PreToolUse`, the reason when a hook refuses the tool.
    fn hook(&mut self, event: &str, payload: &str) -> Option<String>;
    fn stdout(&mut self, _text: &str) {}
    fn run(&mut self, _command: &str) {}
    fn sleep(&mut self, _ms: u64) {}
}

/// How a part of a scenario ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Running,
    Exit(i32),
    Crashed,
}

/// Plays a scenario in this process, in virtual time, as the scripts do with `now_ms` set.
pub struct Player {
    pub vars: BTreeMap<String, String>,
    time: u64,
    turns: usize,
}

impl Player {
    /// `vars` has the run's values (`session_id`, `cwd`, `transcript_path`, …); the scenario's own
    /// `vars` come first, the run's over them.
    pub fn new(scenario: &Scenario, vars: BTreeMap<String, String>, start_ms: u64) -> Player {
        let mut all = scenario.vars.clone();
        all.insert("version".into(), scenario.version.clone());
        all.extend(vars);
        all.insert("turn".into(), "0".into());
        Player { vars: all, time: start_ms, turns: 0 }
    }

    /// The time of the last step, in virtual milliseconds.
    pub fn time(&self) -> u64 {
        self.time
    }

    /// Time runs on to `ms` (the agent waits for its next prompt).
    pub fn wait_until(&mut self, ms: u64) {
        self.time = self.time.max(ms);
    }

    pub fn start(&mut self, scenario: &Scenario, sink: &mut dyn Sink) -> End {
        let base = self.time;
        self.play(&scenario.start, base, sink)
    }

    pub fn turn(&mut self, scenario: &Scenario, prompt: &str, sink: &mut dyn Sink) -> End {
        self.turns += 1;
        self.vars.insert("turn".into(), self.turns.to_string());
        self.vars.insert("prompt".into(), prompt.to_string());
        let base = self.time;
        self.play(scenario.turn(self.turns), base, sink)
    }

    pub fn end(&mut self, scenario: &Scenario, sink: &mut dyn Sink) -> End {
        let base = self.time;
        self.play(&scenario.end, base, sink)
    }

    fn play(&mut self, steps: &[Step], base: u64, sink: &mut dyn Sink) -> End {
        for step in steps {
            self.time = self.time.max(base + step.at);
            self.vars.insert("ts".into(), iso_ms(self.time));
            self.vars.insert("unix_s".into(), (self.time / 1000).to_string());
            self.vars.insert("ms".into(), self.time.to_string());
            sink.time(self.time);
            let fill = |text: &str| fill(text, &self.vars);
            match &step.what {
                What::Transcript { text } => sink.line(Out::Transcript, &fill(text)),
                What::Rollout { text } => sink.line(Out::Rollout, &fill(text)),
                What::History { text } => sink.line(Out::History, &fill(text)),
                What::Hook { event, text } => {
                    sink.hook(event, &fill(text));
                }
                What::Stdout { text } => sink.stdout(&fill(text)),
                What::Tool { pre, allowed, denied } => {
                    let end = match sink.hook("PreToolUse", &fill(pre)) {
                        Some(reason) => {
                            self.vars.insert("reason".into(), reason);
                            self.play(denied, base, sink)
                        }
                        None => self.play(allowed, base, sink),
                    };
                    if end != End::Running {
                        return end;
                    }
                }
                What::Run { command } => sink.run(&fill(command)),
                What::Sleep { ms } => sink.sleep(*ms),
                What::Exit { code } => return End::Exit(*code),
                What::Crash => return End::Crashed,
            }
        }
        End::Running
    }
}

/// The whole of a session: its start, a turn for each prompt, and its end unless it stopped first.
pub fn play_session(scenario: &Scenario, vars: BTreeMap<String, String>, start_ms: u64, prompts: &[&str], sink: &mut dyn Sink) -> End {
    let mut player = Player::new(scenario, vars, start_ms);
    let mut end = player.start(scenario, sink);
    for prompt in prompts {
        if end != End::Running {
            return end;
        }
        end = player.turn(scenario, prompt, sink);
    }
    if end == End::Running { player.end(scenario, sink) } else { end }
}

/// A [`Sink`] that writes the agent's files, and the hook events as Glasscode's own hook command
/// writes them (`cat >> "$GLASSCODE_EVENTS"; printf '\n' >> "$GLASSCODE_EVENTS"`).
#[derive(Debug, Default)]
pub struct FileSink {
    pub files: BTreeMap<Out, PathBuf>,
    /// The session's events file (`agents::events_file`), if hooks report to Glasscode.
    pub events: Option<PathBuf>,
    /// The events the settings have hooks for, as the CLI runs only those; `None`: every event.
    pub hook_events: Option<Vec<String>>,
    pub hooks: Vec<(String, String)>,
    pub stdout: String,
    /// The commands the tool calls ran.
    pub ran: Vec<String>,
    /// A `PreToolUse` hook refuses a tool whose payload holds the first text, with the second as
    /// the reason.
    pub refuse: Vec<(String, String)>,
}

pub(crate) fn append(path: &Path, text: &[u8]) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap_or_else(|err| panic!("{}: {err}", dir.display()));
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    file.write_all(text).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
}

impl Sink for FileSink {
    fn line(&mut self, out: Out, text: &str) {
        if let Some(path) = self.files.get(&out) {
            append(path, format!("{text}\n").as_bytes());
        }
    }

    fn hook(&mut self, event: &str, payload: &str) -> Option<String> {
        if self.hook_events.as_ref().is_some_and(|events| !events.iter().any(|e| e == event)) {
            return None;
        }
        self.hooks.push((event.to_string(), payload.to_string()));
        if let Some(events) = &self.events {
            append(events, format!("{payload}\n").as_bytes());
        }
        if event != "PreToolUse" {
            return None;
        }
        self.refuse.iter().find(|(needle, _)| payload.contains(needle.as_str())).map(|(_, reason)| reason.clone())
    }

    fn stdout(&mut self, text: &str) {
        self.stdout.push_str(text);
        self.stdout.push('\n');
    }

    fn run(&mut self, command: &str) {
        self.ran.push(command.to_string());
    }
}

/// Where Claude keeps the transcript of conversation `session` started in `cwd`.
pub fn claude_transcript(config_dir: &Path, cwd: &Path, session: &str) -> PathBuf {
    config_dir.join("projects").join(crate::history::claude_folder_name(cwd)).join(format!("{session}.jsonl"))
}

/// Where Codex keeps the rollout of conversation `session` started at `start_ms` (the stand-ins
/// name it by UTC; Codex by the local time).
pub fn codex_rollout(codex_home: &Path, session: &str, start_ms: u64) -> PathBuf {
    let ts = iso_ms(start_ms);
    let (date, time) = (&ts[..10], &ts[11..19]);
    codex_home.join("sessions").join(&ts[..4]).join(&ts[5..7]).join(&ts[8..10]).join(format!("rollout-{date}T{}-{session}.jsonl", time.replace(':', "-")))
}

// ----- the synthesized scenarios ---------------------------------------------------------------

/// How a turn ends: the six cases of M02 step 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ending {
    /// The work is done and the agent says so.
    Completed,
    /// The person stopped it during a tool call.
    Interrupted,
    /// The API refused (a rate limit).
    ApiError,
    /// The process died in the middle (`kill -9`, a crash).
    Crashed,
    /// It ends on a question to the person.
    Question,
    /// It pushes the branch (a `PreToolUse` hook may refuse the push).
    Pushed,
}

impl Ending {
    pub const ALL: [Ending; 6] = [Ending::Completed, Ending::Interrupted, Ending::ApiError, Ending::Crashed, Ending::Question, Ending::Pushed];
}

/// How the CLI runs: in a terminal, or once for a program (`claude -p --output-format
/// stream-json`, `codex exec --json`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Interactive,
    Print,
}

/// What a turn of each ending says last.
fn last_words(ending: Ending) -> &'static str {
    match ending {
        Ending::Completed => "Done: the install steps point at make setup, and all 12 tests pass.",
        Ending::Question => "The fix is ready. Should I push it and open a pull request?",
        Ending::Pushed => "Pushed {{branch}}.",
        Ending::ApiError => "API Error: 429 {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Rate limited\"}}",
        Ending::Interrupted | Ending::Crashed => "",
    }
}

/// The id of a prompt, as `UserPromptSubmit` and the turn's `Stop` both carry it (P05).
const PROMPT_ID: &str = "{{session_id}}-prompt-{{turn}}";

/// The time from `Stop` to the turn's last lines in the transcript: P05 saw the hook run first, and
/// the assistant lines 111 ms after it.
const TRANSCRIPT_LAG_MS: u64 = 111;

/// Claude's steps of one turn, built line by line. The hook payloads have the fields P05 saw with
/// claude 2.1.328 (M01, docs/manager-v2-proofs.md); `StopFailure`'s are the documented ones, not
/// seen yet.
struct ClaudeTurn {
    mode: Mode,
    steps: Vec<Step>,
    at: u64,
    n: u32,
    parent: Option<String>,
}

impl ClaudeTurn {
    fn new(mode: Mode) -> ClaudeTurn {
        ClaudeTurn { mode, steps: Vec::new(), at: 0, n: 0, parent: None }
    }

    fn push(&mut self, gap: u64, what: What) {
        self.at += gap;
        self.steps.push(Step::new(self.at, what));
    }

    fn id(&mut self, prefix: &str) -> String {
        self.n += 1;
        format!("{prefix}{{{{turn}}}}{:04}", self.n)
    }

    /// A transcript line: the fields Claude writes on each, then the line's own.
    fn line(&mut self, kind: &str, message: Value, extra: Value) -> (String, String) {
        let uuid = self.id("00000000-0000-4000-8000-0000");
        let mut line = json!({
            "parentUuid": self.parent,
            "isSidechain": false,
            "userType": "external",
            "cwd": "{{cwd}}",
            "sessionId": "{{session_id}}",
            "version": "{{version}}",
            "gitBranch": "{{branch}}",
            "entrypoint": "{{entrypoint}}",
            "type": kind,
            "message": message,
        });
        if let Value::Object(extra) = extra {
            line.as_object_mut().expect("an object").extend(extra);
        }
        line["uuid"] = json!(uuid);
        line["timestamp"] = json!("{{ts}}");
        self.parent = Some(uuid.clone());
        (line.to_string(), uuid)
    }

    fn hook(&mut self, gap: u64, event: &str, fields: Value) -> Step {
        let mut payload = json!({"session_id": "{{session_id}}", "transcript_path": "{{transcript_path}}", "cwd": "{{cwd}}"});
        if matches!(event, "UserPromptSubmit" | "Stop" | "PreToolUse" | "PostToolUse") {
            payload["permission_mode"] = json!("{{permission_mode}}");
        }
        payload["hook_event_name"] = json!(event);
        if let Value::Object(fields) = fields {
            payload.as_object_mut().expect("an object").extend(fields);
        }
        self.at += gap;
        Step::new(self.at, What::Hook { event: event.into(), text: payload.to_string() })
    }

    fn add_hook(&mut self, gap: u64, event: &str, fields: Value) {
        let step = self.hook(gap, event, fields);
        self.steps.push(step);
    }

    fn prompt(&mut self) {
        self.add_hook(0, "UserPromptSubmit", json!({"prompt": "{{prompt}}", "prompt_id": PROMPT_ID}));
        let (line, _) = self.line("user", json!({"role": "user", "content": "{{prompt}}"}), json!({}));
        self.push(15, What::Transcript { text: line });
        if self.mode == Mode::Interactive {
            self.push(0, What::Stdout { text: "> {{prompt}}".into() });
        }
    }

    fn assistant(&mut self, content: Value, extra: Value) -> (String, Value) {
        let id = self.id("msg_");
        let message = json!({
            "id": id,
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-5",
            "content": content,
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {"input_tokens": 12, "cache_read_input_tokens": 4096, "output_tokens": 64},
        });
        let mut extra = extra;
        extra.as_object_mut().expect("an object").insert("requestId".into(), json!(self.id("req_")));
        let (line, uuid) = self.line("assistant", message.clone(), extra);
        (line, json!({"type": "assistant", "message": message, "parent_tool_use_id": null, "session_id": "{{session_id}}", "uuid": uuid}))
    }

    /// A sentence of the agent's.
    fn say(&mut self, gap: u64, text: &str) {
        let (line, stream) = self.assistant(json!([{"type": "text", "text": text}]), json!({}));
        self.push(gap, What::Transcript { text: line });
        match self.mode {
            Mode::Interactive => self.push(0, What::Stdout { text: format!("⏺ {text}") }),
            Mode::Print => self.push(0, What::Stdout { text: stream.to_string() }),
        }
    }

    /// The call of a tool and its result: (call steps, result steps).
    fn tool_steps(&mut self, name: &str, input: Value, output: &str, error: bool) -> (Vec<Step>, Vec<Step>, String) {
        let tool_id = self.id("toolu_");
        let at = self.at;
        let (call, stream_call) = self.assistant(json!([{"type": "tool_use", "id": tool_id, "name": name, "input": input}]), json!({}));
        let mut calls = vec![Step::new(at + 400, What::Transcript { text: call })];
        let gist = input.get("command").or(input.get("file_path")).and_then(Value::as_str).unwrap_or_default().to_string();
        let result = json!({"role": "user", "content": [{"tool_use_id": tool_id, "type": "tool_result", "content": output, "is_error": error}]});
        let (line, uuid) = self.line("user", result.clone(), json!({"toolUseResult": {"stdout": if error { "" } else { output }, "stderr": if error { output } else { "" }, "interrupted": false, "isImage": false}}));
        let mut results = vec![Step::new(at + 1400, What::Transcript { text: line })];
        match self.mode {
            Mode::Interactive => {
                calls.push(Step::new(at + 400, What::Stdout { text: format!("⏺ {name}({gist})") }));
                results.push(Step::new(at + 1400, What::Stdout { text: format!("  ⎿  {}", output.lines().next().unwrap_or_default()) }));
            }
            Mode::Print => {
                calls.push(Step::new(at + 400, What::Stdout { text: stream_call.to_string() }));
                results.push(Step::new(at + 1400, What::Stdout { text: json!({"type": "user", "message": result, "parent_tool_use_id": null, "session_id": "{{session_id}}", "uuid": uuid}).to_string() }));
            }
        }
        (calls, results, tool_id)
    }

    fn tool(&mut self, name: &str, input: Value, output: &str, error: bool) {
        let (calls, results, _) = self.tool_steps(name, input, output, error);
        self.at += 1400;
        self.steps.extend(calls);
        self.steps.extend(results);
    }

    /// The `result` line that ends a run of `claude -p --output-format stream-json`.
    fn result(&mut self, gap: u64, error: bool, text: &str) {
        if self.mode == Mode::Print {
            let subtype = if error { "error_during_execution" } else { "success" };
            let line = json!({
                "type": "result",
                "subtype": subtype,
                "is_error": error,
                "duration_ms": self.at + gap,
                "duration_api_ms": self.at,
                "num_turns": 1,
                "result": text,
                "session_id": "{{session_id}}",
                "total_cost_usd": 0,
                "usage": {"input_tokens": 12, "output_tokens": 64},
                "uuid": self.id("00000000-0000-4000-9000-0000"),
            });
            self.push(gap, What::Stdout { text: line.to_string() });
        }
    }

    fn work(&mut self) {
        self.say(900, "I'll look at the install steps and run the tests.");
        self.tool("Read", json!({"file_path": "{{cwd}}/README.md"}), "# app\n\nRun ./setup.sh to install.\n", false);
        self.tool("Edit", json!({"file_path": "{{cwd}}/README.md", "old_string": "Run ./setup.sh", "new_string": "Run make setup"}), "The file has been updated.", false);
        self.tool("Bash", json!({"command": "bun test", "description": "Run the tests"}), "12 pass\n0 fail", false);
    }

    /// The end of a turn that answered: `Stop` with the answer, then (P05) the answer's line in
    /// the transcript, and the `result` line of a program's run.
    fn answered(&mut self, gap: u64, text: &str) {
        let stop = json!({"stop_hook_active": false, "last_assistant_message": text, "prompt_id": PROMPT_ID, "background_tasks": [], "session_crons": [], "effort": "medium"});
        self.add_hook(gap, "Stop", stop);
        self.say(TRANSCRIPT_LAG_MS, text);
        self.result(5, false, text);
    }

    fn turn(mut self, ending: Ending) -> Vec<Step> {
        self.prompt();
        match ending {
            Ending::Completed | Ending::Question => {
                self.work();
                if self.mode == Mode::Interactive && ending == Ending::Completed {
                    // Claude names the conversation; P05 found the title the last line at Stop.
                    self.push(300, What::Transcript { text: json!({"type": "ai-title", "aiTitle": "Point the readme at make setup", "sessionId": "{{session_id}}"}).to_string() });
                }
                self.answered(1200, last_words(ending));
                if ending == Ending::Question && self.mode == Mode::Interactive {
                    // A minute later Claude says it waits.
                    self.add_hook(60_000, "Notification", json!({"message": "Claude is waiting for your input", "notification_type": "idle_prompt"}));
                }
            }
            Ending::Pushed => {
                self.work();
                let pre = self.hook(0, "PreToolUse", json!({"tool_name": "Bash", "tool_input": {"command": "git push origin {{branch}}", "description": "Push the branch"}, "tool_use_id": "toolu_push{{turn}}", "prompt_id": PROMPT_ID}));
                let What::Hook { text: pre, .. } = pre.what else { unreachable!() };
                let start = self.at;
                let (mut allowed, results, _) = self.tool_steps("Bash", json!({"command": "git push origin {{branch}}", "description": "Push the branch"}), "To https://github.com/acme/app.git\n   1a2b3c4..5d6e7f8  {{branch}} -> {{branch}}", false);
                allowed.extend(results);
                self.at = start;
                let (mut denied, refused, _) = self.tool_steps("Bash", json!({"command": "git push origin {{branch}}", "description": "Push the branch"}), "{{reason}}", true);
                denied.extend(refused);
                // How the turn ends after the push, or after the refusal: each with its own answer.
                let before = std::mem::take(&mut self.steps);
                self.at = start + 1400;
                self.answered(600, last_words(Ending::Pushed));
                allowed.extend(std::mem::take(&mut self.steps));
                self.at = start + 1400;
                self.answered(600, "The push was refused: {{reason}}");
                denied.extend(std::mem::take(&mut self.steps));
                self.steps = before;
                self.steps.push(Step::new(start, What::Tool { pre, allowed, denied }));
            }
            Ending::Interrupted => {
                self.say(900, "I'll run the tests first.");
                let (calls, _, tool_id) = self.tool_steps("Bash", json!({"command": "bun test", "description": "Run the tests"}), "", false);
                self.steps.extend(calls);
                self.at += 400;
                // Esc during the tool call: the call is refused, then Claude's interrupt marker; no
                // Stop (the docs; P05 has not run this case yet).
                let refused = json!({"role": "user", "content": [{"tool_use_id": tool_id, "type": "tool_result", "content": "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.", "is_error": true}]});
                let (line, _) = self.line("user", refused, json!({"toolUseResult": "User rejected tool use"}));
                self.push(2_000, What::Transcript { text: line });
                let (marker, _) = self.line("user", json!({"role": "user", "content": [{"type": "text", "text": "[Request interrupted by user for tool use]"}]}), json!({}));
                self.push(1, What::Transcript { text: marker });
                match self.mode {
                    Mode::Interactive => self.push(0, What::Stdout { text: "  ⎿  Interrupted · What should Claude do instead?".into() }),
                    Mode::Print => self.result(0, true, "[Request interrupted by user for tool use]"),
                }
            }
            Ending::ApiError => {
                // A failed turn: StopFailure, not Stop, with the documented fields (P05 has not run
                // this case yet); its line reaches the transcript after the hook, as a Stop's does.
                self.add_hook(2_500, "StopFailure", json!({"error": "rate_limit", "error_details": "429 rate_limit_error: Rate limited", "last_assistant_message": last_words(ending), "prompt_id": PROMPT_ID}));
                let (line, stream) = self.assistant(json!([{"type": "text", "text": last_words(ending)}]), json!({"isApiErrorMessage": true}));
                self.push(TRANSCRIPT_LAG_MS, What::Transcript { text: line });
                match self.mode {
                    Mode::Interactive => self.push(0, What::Stdout { text: format!("  ⎿  {}", last_words(ending)) }),
                    Mode::Print => self.push(0, What::Stdout { text: stream.to_string() }),
                }
                self.result(5, true, last_words(ending));
            }
            Ending::Crashed => {
                self.say(900, "I'll run the tests first.");
                let (calls, _, _) = self.tool_steps("Bash", json!({"command": "bun test", "description": "Run the tests"}), "", false);
                self.steps.extend(calls);
                self.push(800, What::Crash);
            }
        }
        self.steps
    }
}

/// A Claude scenario with one turn per ending: `claude` in a terminal with hooks, or `claude -p
/// --output-format stream-json --verbose`.
pub fn claude(mode: Mode, turns: &[Ending]) -> Scenario {
    let mut start = ClaudeTurn::new(mode);
    start.add_hook(0, "SessionStart", json!({"source": "{{start_source}}", "model": "claude-sonnet-5-5"}));
    match mode {
        Mode::Interactive => start.push(0, What::Stdout { text: "╭ Claude Code {{version}} (stand-in) · {{cwd}}".into() }),
        Mode::Print => {
            let init = json!({
                "type": "system",
                "subtype": "init",
                "cwd": "{{cwd}}",
                "session_id": "{{session_id}}",
                "tools": ["Task", "Bash", "Glob", "Grep", "Read", "Edit", "Write", "WebFetch", "TodoWrite", "WebSearch"],
                "mcp_servers": [],
                "model": "claude-sonnet-5-5",
                "permissionMode": "{{permission_mode}}",
                "slash_commands": [],
                "apiKeySource": "none",
                "claude_code_version": "{{version}}",
                "uuid": "00000000-0000-4000-a000-000000000000",
            });
            start.push(0, What::Stdout { text: init.to_string() });
        }
    }
    let mut end = ClaudeTurn::new(mode);
    if mode == Mode::Interactive {
        end.add_hook(0, "SessionEnd", json!({"reason": "prompt_input_exit"}));
    }
    Scenario {
        agent: "claude".into(),
        version: CLAUDE_VERSION.into(),
        source: "synthesized (M02)".into(),
        vars: BTreeMap::new(),
        start: start.steps,
        turns: turns.iter().map(|ending| ClaudeTurn::new(mode).turn(*ending)).collect(),
        end: end.steps,
    }
}

/// Codex's steps of one turn.
struct CodexTurn {
    mode: Mode,
    steps: Vec<Step>,
    at: u64,
    n: u32,
}

impl CodexTurn {
    fn push(&mut self, gap: u64, what: What) {
        self.at += gap;
        self.steps.push(Step::new(self.at, what));
    }

    fn rollout(&mut self, gap: u64, kind: &str, payload: Value) {
        let line = json!({"timestamp": "{{ts}}", "type": kind, "payload": payload});
        self.push(gap, What::Rollout { text: line.to_string() });
    }

    fn event(&mut self, gap: u64, payload: Value) {
        self.rollout(gap, "event_msg", payload);
    }

    fn item(&mut self, gap: u64, payload: Value) {
        self.rollout(gap, "response_item", payload);
    }

    fn exec(&mut self, line: Value) {
        if self.mode == Mode::Print {
            self.push(0, What::Stdout { text: line.to_string() });
        }
    }

    fn call_id(&mut self) -> String {
        self.n += 1;
        format!("call_{{{{turn}}}}{:03}", self.n)
    }

    fn say(&mut self, gap: u64, text: &str) {
        self.event(gap, json!({"type": "agent_message", "message": text}));
        self.item(1, json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}));
        match self.mode {
            Mode::Interactive => self.push(0, What::Stdout { text: format!("• {text}") }),
            Mode::Print => {
                let id = self.n;
                self.n += 1;
                self.exec(json!({"type": "item.completed", "item": {"id": format!("item_{id}"), "type": "agent_message", "text": text}}));
            }
        }
    }

    fn shell(&mut self, command: &str, output: Option<&str>) {
        let call = self.call_id();
        let arguments = json!({"command": ["bash", "-lc", command]}).to_string();
        self.item(700, json!({"type": "function_call", "name": "shell", "arguments": arguments, "call_id": call}));
        match self.mode {
            Mode::Interactive => self.push(0, What::Stdout { text: format!("• Ran {command}") }),
            Mode::Print => self.exec(json!({"type": "item.started", "item": {"id": call, "type": "command_execution", "command": format!("bash -lc '{command}'"), "aggregated_output": "", "exit_code": null, "status": "in_progress"}})),
        }
        if let Some(output) = output {
            let result = json!({"output": output, "metadata": {"exit_code": 0, "duration_seconds": 1.2}}).to_string();
            self.item(1200, json!({"type": "function_call_output", "call_id": call, "output": result}));
            self.exec(json!({"type": "item.completed", "item": {"id": call, "type": "command_execution", "command": format!("bash -lc '{command}'"), "aggregated_output": output, "exit_code": 0, "status": "completed"}}));
        }
    }

    fn patch(&mut self) {
        let call = self.call_id();
        self.item(900, json!({"type": "custom_tool_call", "status": "completed", "call_id": call, "name": "apply_patch", "input": "*** Begin Patch\n*** Update File: README.md\n@@\n-Run ./setup.sh to install.\n+Run make setup to install.\n*** End Patch\n"}));
        self.item(40, json!({"type": "custom_tool_call_output", "call_id": call, "output": "Success. Updated the following files:\nM README.md\n"}));
        match self.mode {
            Mode::Interactive => self.push(0, What::Stdout { text: "• Edited README.md (+1 -1)".into() }),
            Mode::Print => self.exec(json!({"type": "item.completed", "item": {"id": call, "type": "file_change", "changes": [{"path": "README.md", "kind": "update"}], "status": "completed"}})),
        }
    }

    fn turn(mut self, ending: Ending) -> Vec<Step> {
        let turn_id = "{{session_id}}-{{turn}}";
        self.event(0, json!({"type": "task_started", "turn_id": turn_id, "model_context_window": 258_400}));
        self.exec(json!({"type": "turn.started"}));
        self.rollout(1, "turn_context", json!({"turn_id": turn_id, "cwd": "{{cwd}}", "approval_policy": "on-request", "sandbox_policy": {"type": "workspace-write"}, "model": "gpt-5.5-codex", "summary": "auto"}));
        self.item(1, json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "{{prompt}}"}]}));
        self.event(1, json!({"type": "user_message", "message": "{{prompt}}", "images": []}));
        if self.mode == Mode::Interactive {
            self.push(0, What::History { text: r#"{"session_id":"{{session_id}}","ts":{{unix_s}},"text":"{{prompt}}"}"#.into() });
            self.push(0, What::Stdout { text: "› {{prompt}}".into() });
        }
        let done = |this: &mut CodexTurn, last: Option<&str>| {
            this.event(30, json!({"type": "token_count", "info": {"total_token_usage": {"input_tokens": 4096, "output_tokens": 64}}, "rate_limits": null}));
            this.event(1, json!({"type": "task_complete", "turn_id": turn_id, "last_agent_message": last}));
            this.exec(json!({"type": "turn.completed", "usage": {"input_tokens": 4096, "cached_input_tokens": 3072, "output_tokens": 64}}));
        };
        match ending {
            Ending::Completed | Ending::Question => {
                self.shell("bun test", Some("12 pass\n0 fail\n"));
                self.patch();
                self.say(1100, last_words(ending));
                done(&mut self, Some(last_words(ending)));
            }
            Ending::Pushed => {
                self.shell("bun test", Some("12 pass\n0 fail\n"));
                self.shell("git push origin {{branch}}", Some("To https://github.com/acme/app.git\n   1a2b3c4..5d6e7f8  {{branch}} -> {{branch}}\n"));
                self.say(800, last_words(ending));
                done(&mut self, Some(last_words(ending)));
            }
            Ending::Interrupted => {
                self.shell("bun test", None);
                self.event(2_000, json!({"type": "turn_aborted", "turn_id": turn_id, "reason": "interrupted"}));
                match self.mode {
                    Mode::Interactive => self.push(0, What::Stdout { text: "■ Conversation interrupted - tell the model what to do differently.".into() }),
                    Mode::Print => self.exec(json!({"type": "turn.failed", "error": {"message": "interrupted"}})),
                }
            }
            Ending::ApiError => {
                let message = "exceeded retry limit, last status: 429 Too Many Requests";
                self.event(2_500, json!({"type": "error", "message": message}));
                match self.mode {
                    Mode::Interactive => self.push(0, What::Stdout { text: format!("■ {message}") }),
                    Mode::Print => {
                        self.exec(json!({"type": "error", "message": message}));
                        self.exec(json!({"type": "turn.failed", "error": {"message": message}}));
                    }
                }
                self.event(1, json!({"type": "task_complete", "turn_id": turn_id, "last_agent_message": null}));
            }
            Ending::Crashed => {
                self.shell("bun test", None);
                self.push(800, What::Crash);
            }
        }
        self.steps
    }
}

/// A Codex scenario with one turn per ending: `codex` in a terminal, or `codex exec --json`.
/// Codex runs no hooks for Glasscode yet (P06): its rollout is what reports a turn.
pub fn codex(mode: Mode, turns: &[Ending]) -> Scenario {
    let mut start = CodexTurn { mode, steps: Vec::new(), at: 0, n: 0 };
    start.rollout(
        0,
        "session_meta",
        json!({"id": "{{session_id}}", "timestamp": "{{ts}}", "cwd": "{{cwd}}", "originator": "{{originator}}", "cli_version": "{{version}}", "source": "{{source}}", "thread_source": "user", "model_provider": "openai", "git": {"branch": "{{branch}}"}}),
    );
    match mode {
        Mode::Interactive => start.push(0, What::Stdout { text: "╭ OpenAI Codex (v{{version}}, stand-in) · {{cwd}}".into() }),
        Mode::Print => start.exec(json!({"type": "thread.started", "thread_id": "{{session_id}}"})),
    }
    Scenario {
        agent: "codex".into(),
        version: CODEX_VERSION.into(),
        source: "synthesized (M02)".into(),
        vars: BTreeMap::new(),
        start: start.steps,
        turns: turns.iter().map(|ending| CodexTurn { mode, steps: Vec::new(), at: 0, n: 0 }.turn(*ending)).collect(),
        end: Vec::new(),
    }
}

/// The values a run gives the placeholders of a Claude scenario.
pub fn claude_vars(session: &str, cwd: &Path, transcript: &Path, mode: Mode) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("session_id".into(), session.into()),
        ("cwd".into(), cwd.to_string_lossy().into_owned()),
        ("transcript_path".into(), transcript.to_string_lossy().into_owned()),
        ("branch".into(), "main".into()),
        ("permission_mode".into(), "default".into()),
        ("start_source".into(), "startup".into()),
        ("entrypoint".into(), if mode == Mode::Print { "sdk-cli" } else { "cli" }.into()),
    ])
}

/// The values a run gives the placeholders of a Codex scenario.
pub fn codex_vars(session: &str, cwd: &Path, mode: Mode) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("session_id".into(), session.into()),
        ("cwd".into(), cwd.to_string_lossy().into_owned()),
        ("branch".into(), "main".into()),
        ("originator".into(), if mode == Mode::Print { "codex_exec" } else { "codex_cli_rs" }.into()),
        ("source".into(), if mode == Mode::Print { "exec" } else { "cli" }.into()),
    ])
}

// ----- the scripts ------------------------------------------------------------------------------

/// The folder of the stand-in scripts (`script/test-agents/` of the source tree).
/// The stand-in manager's brain (`claude -p` with stream-json both ways, M50): each turn starts
/// with the init line of exactly the manager's tools (so the runtime's tool check passes) and
/// answers `answer`. Its file is the scenarios folder's `claude-print.json`: the brain has no
/// session id, so the stand-in plays that one. A wake parses the answer as its JSON shape; the chat
/// shows it as it is.
pub fn brain(answer: &str) -> Scenario {
    let tools: Vec<String> = crate::mcp::tools(crate::mcp::Profile::Manager).iter().filter_map(|t| t["name"].as_str().map(|n| format!("{}{n}", crate::mcp::CLAUDE_PREFIX))).chain(["ToolSearch".to_string()]).collect();
    let line = |value: serde_json::Value| Step::new(0, What::Stdout { text: value.to_string() });
    let turn = vec![
        line(serde_json::json!({ "type": "system", "subtype": "init", "session_id": "{{session_id}}", "tools": tools, "model": "claude-haiku-5-5", "claude_code_version": CLAUDE_VERSION })),
        line(serde_json::json!({ "type": "stream_event", "parent_tool_use_id": null, "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": answer } } })),
        line(serde_json::json!({ "type": "result", "subtype": "success", "is_error": false, "result": answer, "usage": { "input_tokens": 400, "output_tokens": 40 } })),
    ];
    Scenario { agent: "claude".into(), version: CLAUDE_VERSION.into(), source: "synthesized: the manager's brain".into(), turns: vec![turn], ..Scenario::default() }
}

pub fn scripts_dir() -> PathBuf {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    crate_dir.parent().and_then(Path::parent).expect("the crate is in crates/ of the repository").join("script").join("test-agents")
}

/// The environment variable that names a scenario file for a stand-in.
pub const SCENARIO_VARIABLE: &str = "GLASSCODE_TEST_SCENARIO";
/// A folder of scenarios: `<session id>.json`, else `<agent>.json` (`claude-print.json`,
/// `codex-exec.json` for the programs' runs).
pub const SCENARIOS_VARIABLE: &str = "GLASSCODE_TEST_SCENARIOS";
/// The start of virtual time for a stand-in: with it, `{{ts}}` comes from the steps' `at`.
pub const NOW_VARIABLE: &str = "GLASSCODE_TEST_NOW_MS";
/// A fixed conversation id for a stand-in that would make one up.
pub const SESSION_VARIABLE: &str = "GLASSCODE_TEST_SESSION_ID";

#[cfg(test)]
mod tests {
    use super::*;

    /// The stand-in brain's init line names exactly the manager's tools, so the runtime's tool
    /// check passes; its answer is the turn's result.
    #[test]
    fn the_stand_in_brain_passes_the_tool_check() {
        let scenario = brain("{\"tell_person\":\"hi\"}");
        let lines: Vec<serde_json::Value> = scenario.turn(1).iter().filter_map(|step| match &step.what {
            What::Stdout { text } => serde_json::from_str(text).ok(),
            _ => None,
        }).collect();
        let tools: Vec<String> = lines[0]["tools"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
        assert!(crate::runtime::claude_expected(&tools).is_ok());
        assert_eq!(lines.last().unwrap()["result"], "{\"tell_person\":\"hi\"}");
    }
    use crate::model::AgentKind;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn placeholders_are_filled_as_json_text() {
        let v = vars(&[("cwd", r#"C:\Users\dev\"app""#), ("prompt", "line one\nline two\ttab é 😀 \u{1}"), ("n", "42")]);
        assert_eq!(fill(r#"{"cwd":"{{cwd}}","n":{{n}}}"#, &v), r#"{"cwd":"C:\\Users\\dev\\\"app\"","n":42}"#);
        assert_eq!(fill("{{prompt}}", &v), "line one\\nline two\\ttab é 😀 \\u0001");
        // No value, not a name, or broken: left as it is; a placeholder after a stray brace is found.
        assert_eq!(fill("{{nope}} {{Cwd}} {{}} {{n", &v), "{{nope}} {{Cwd}} {{}} {{n");
        assert_eq!(fill("{{{n}}} {{nope}}{{n}}", &v), "{42} {{nope}}42");
        // The result is the JSON the agent would have written.
        let line = fill(r#"{"text":"{{prompt}}"}"#, &v);
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["text"], v["prompt"]);
    }

    #[test]
    fn scenarios_and_tapes_are_plain_json() {
        let scenario = claude(Mode::Interactive, &Ending::ALL);
        let text = serde_json::to_string(&scenario).unwrap();
        assert_eq!(serde_json::from_str::<Scenario>(&text).unwrap(), scenario);
        let tape = Scenario::write_tape(&scenario.turns[0]);
        assert_eq!(Scenario::read_tape(&tape).unwrap(), scenario.turns[0]);
        let first: Value = serde_json::from_str(tape.lines().next().unwrap()).unwrap();
        assert_eq!((first["to"].as_str(), first["event"].as_str()), (Some("hook"), Some("UserPromptSubmit")));
        assert!(first.get("at").is_none(), "an at of 0 is left out");
        // Past the last turn, the last turn again.
        assert_eq!(scenario.turn(9), scenario.turns[5].as_slice());
        // Every step's text is JSON once filled in (the stdout of a terminal run is text).
        let all = vars(&[("cwd", "/w"), ("session_id", "s"), ("transcript_path", "/t"), ("prompt", "p"), ("branch", "b"), ("permission_mode", "default"), ("start_source", "startup"), ("entrypoint", "cli"), ("version", "1"), ("turn", "1"), ("ts", "t"), ("unix_s", "1"), ("reason", "r")]);
        fn check(steps: &[Step], all: &BTreeMap<String, String>) {
            for step in steps {
                match &step.what {
                    What::Transcript { text } | What::Rollout { text } | What::History { text } | What::Hook { text, .. } => {
                        serde_json::from_str::<Value>(&fill(text, all)).unwrap_or_else(|err| panic!("{err}: {text}"));
                    }
                    What::Tool { pre, allowed, denied } => {
                        serde_json::from_str::<Value>(&fill(pre, all)).unwrap();
                        check(allowed, all);
                        check(denied, all);
                    }
                    _ => {}
                }
            }
        }
        for scenario in [claude(Mode::Interactive, &Ending::ALL), claude(Mode::Print, &Ending::ALL), codex(Mode::Interactive, &Ending::ALL), codex(Mode::Print, &Ending::ALL)] {
            check(&scenario.start, &all);
            for turn in &scenario.turns {
                check(turn, &all);
            }
        }
        // A run of a program prints JSON lines only.
        for scenario in [claude(Mode::Print, &Ending::ALL), codex(Mode::Print, &Ending::ALL)] {
            let mut sink = FileSink::default();
            play_session(&scenario, all.clone(), 0, &["a", "b", "c", "d", "e", "f"], &mut sink);
            for line in sink.stdout.lines() {
                serde_json::from_str::<Value>(line).unwrap_or_else(|err| panic!("{err}: {line}"));
            }
        }
    }

    /// Steps run in virtual time: each turn starts where the last one stopped, and a step's
    /// `{{ts}}` is its turn's start plus its `at`.
    #[test]
    fn the_player_keeps_virtual_time() {
        let step = |at, text: &str| Step::new(at, What::Transcript { text: text.into() });
        let scenario = Scenario { agent: "claude".into(), version: "1".into(), turns: vec![vec![step(0, "{{ts}} {{turn}} {{prompt}}"), step(1500, "{{ts}} {{unix_s}}")]], ..Scenario::default() };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut sink = FileSink { files: BTreeMap::from([(Out::Transcript, path.clone())]), ..FileSink::default() };
        let start = super::super::clock::utc_ms(2026, 11, 19, 9, 0);
        assert_eq!(play_session(&scenario, BTreeMap::new(), start, &["one", "two"], &mut sink), End::Running);
        let s = start / 1000;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("2026-11-19T09:00:00.000Z 1 one\n2026-11-19T09:00:01.500Z {}\n2026-11-19T09:00:01.500Z 2 two\n2026-11-19T09:00:03.000Z {}\n", s + 1, s + 3));
    }

    #[test]
    fn a_refused_tool_plays_its_denied_steps() {
        let scenario = claude(Mode::Interactive, &[Ending::Pushed]);
        let v = claude_vars("s1", Path::new("/w"), Path::new("/t"), Mode::Interactive);
        let mut allowed = FileSink::default();
        play_session(&scenario, v.clone(), 0, &["push it"], &mut allowed);
        assert!(allowed.stdout.contains("⏺ Pushed main.") && !allowed.stdout.contains("refused"), "{}", allowed.stdout);
        let mut refused = FileSink { refuse: vec![("git push".into(), "pushes wait for the person".into())], ..FileSink::default() };
        play_session(&scenario, v, 0, &["push it"], &mut refused);
        assert!(refused.stdout.contains("The push was refused: pushes wait for the person"), "{}", refused.stdout);
        // The synthesized scenarios run nothing for real.
        assert!(allowed.ran.is_empty() && refused.ran.is_empty());
        let run = Step::new(0, What::Run { command: "gh pr merge 1".into() });
        let tool = Scenario { turns: vec![vec![Step::new(0, What::Tool { pre: "{}".into(), allowed: vec![run], denied: vec![] })]], ..Scenario::default() };
        let mut ran = FileSink::default();
        play_session(&tool, BTreeMap::new(), 0, &["x"], &mut ran);
        let mut kept = FileSink { refuse: vec![(String::new(), "no".into())], ..FileSink::default() };
        play_session(&tool, BTreeMap::new(), 0, &["x"], &mut kept);
        assert_eq!((ran.ran, kept.ran), (vec!["gh pr merge 1".to_string()], vec![]), "a run step of an allowed tool goes to the sink; of a refused one, not");
        let events: Vec<&str> = refused.hooks.iter().map(|(event, _)| event.as_str()).collect();
        assert_eq!(events, ["SessionStart", "UserPromptSubmit", "PreToolUse", "Stop", "SessionEnd"]);
    }

    #[test]
    fn a_crash_stops_the_session_without_its_hooks() {
        let scenario = claude(Mode::Interactive, &[Ending::Completed, Ending::Crashed]);
        let mut sink = FileSink::default();
        let v = claude_vars("s1", Path::new("/w"), Path::new("/t"), Mode::Interactive);
        assert_eq!(play_session(&scenario, v, 0, &["one", "two", "three"], &mut sink), End::Crashed);
        let events: Vec<&str> = sink.hooks.iter().map(|(event, _)| event.as_str()).collect();
        assert_eq!(events, ["SessionStart", "UserPromptSubmit", "Stop", "UserPromptSubmit"], "no Stop, no third turn, no SessionEnd");
    }

    /// What Glasscode reads of each ending, with its own readers.
    #[test]
    fn claude_endings_read_back_with_glasscode_readers() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("app");
        let transcript = claude_transcript(&dir.path().join("config"), &cwd, "c1");
        let events = dir.path().join("events.jsonl");
        let mut sink = FileSink { files: BTreeMap::from([(Out::Transcript, transcript.clone())]), events: Some(events.clone()), ..FileSink::default() };
        let scenario = claude(Mode::Interactive, &[Ending::Completed, Ending::Question, Ending::Interrupted, Ending::ApiError, Ending::Pushed]);
        let prompts = ["fix the readme", "and the docs?", "run the tests", "again", "push it"];
        play_session(&scenario, claude_vars("c1", &cwd, &transcript, Mode::Interactive), 0, &prompts, &mut sink);

        let page = crate::transcript::read(AgentKind::Claude, &transcript, None, 1000).unwrap();
        let users: Vec<&str> = page.items.iter().filter(|i| i.kind == crate::transcript::ItemKind::User).map(|i| i.text.as_str()).collect();
        // The interrupt marker is a user line, after the prompt it stopped.
        assert_eq!(users, ["fix the readme", "and the docs?", "run the tests", "[Request interrupted by user for tool use]", "again", "push it"]);
        let tools: Vec<(&str, &str, bool)> = page.items.iter().filter_map(|i| Some((i.tool.as_deref()?, i.text.as_str(), i.failed))).collect();
        assert!(tools.contains(&("Bash", "bun test", false)) && tools.iter().any(|(tool, text, _)| *tool == "Read" && text.ends_with("/app/README.md")), "{tools:?}");
        assert!(tools.contains(&("Bash", "git push origin main", false)), "{tools:?}");
        assert!(tools.iter().any(|(_, text, failed)| *text == "bun test" && *failed), "the interrupted call failed: {tools:?}");
        assert_eq!(crate::transcript::last_reply(AgentKind::Claude, &transcript).as_deref(), Some("Pushed main."));
        assert_eq!(crate::history::session_title(AgentKind::Claude, &transcript).as_deref(), Some("Point the readme at make setup"));

        // The hook events, as the app's tail reads them.
        let lines: Vec<Value> = std::fs::read_to_string(&events).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let names: Vec<&str> = lines.iter().map(|l| l["hook_event_name"].as_str().unwrap()).collect();
        assert_eq!(names, ["SessionStart", "UserPromptSubmit", "Stop", "UserPromptSubmit", "Stop", "Notification", "UserPromptSubmit", "UserPromptSubmit", "StopFailure", "UserPromptSubmit", "PreToolUse", "Stop", "SessionEnd"]);
        assert!(lines.iter().all(|l| l["session_id"] == "c1" && l["transcript_path"] == transcript.to_string_lossy().as_ref()));
        assert_eq!(lines[1]["prompt"], "fix the readme");
        assert_eq!(lines[5]["notification_type"], "idle_prompt");
    }

    #[test]
    fn codex_endings_read_back_with_glasscode_readers() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("app");
        let codex_home = dir.path().join("codex");
        let start = super::super::clock::utc_ms(2026, 11, 19, 9, 0);
        let rollout = codex_rollout(&codex_home, "x1", start);
        assert!(rollout.ends_with("sessions/2026/11/19/rollout-2026-11-19T09-00-00-x1.jsonl"), "{}", rollout.display());
        let mut sink = FileSink { files: BTreeMap::from([(Out::Rollout, rollout.clone()), (Out::History, codex_home.join("history.jsonl"))]), ..FileSink::default() };
        let scenario = codex(Mode::Interactive, &[Ending::Completed, Ending::Interrupted, Ending::Pushed]);
        let mut player = Player::new(&scenario, codex_vars("x1", &cwd, Mode::Interactive), start);
        player.start(&scenario, &mut sink);

        // Turn by turn, as the app's tail follows it.
        let mut tail = crate::agents::RolloutTail::new(&rollout);
        tail.poll();
        assert_eq!(tail.turn, crate::agents::CodexTurn::Unknown);
        let mut ends = Vec::new();
        for prompt in ["fix the readme", "run the tests", "push it"] {
            player.turn(&scenario, prompt, &mut sink);
            tail.poll();
            ends.push(tail.turn);
        }
        assert_eq!(ends, [crate::agents::CodexTurn::Ended; 3], "a turn that was stopped ends too");

        let page = crate::transcript::read(AgentKind::Codex, &rollout, None, 1000).unwrap();
        let texts: Vec<&str> = page.items.iter().map(|i| i.text.as_str()).collect();
        assert!(texts.starts_with(&["fix the readme", "bun test", "README.md", last_words(Ending::Completed), "run the tests", "bun test", "push it"]), "{texts:?}");
        assert_eq!(crate::transcript::last_reply(AgentKind::Codex, &rollout).as_deref(), Some("Pushed main."));
        // The conversation is found by its folder, and named by its first prompt.
        let found = crate::agents::find_codex_conversation(&codex_home, &cwd, std::time::SystemTime::UNIX_EPOCH);
        assert_eq!(found, Some(("x1".to_string(), rollout.clone())));
        assert_eq!(crate::history::session_title(AgentKind::Codex, &rollout).as_deref(), Some("fix the readme"));
        let history = std::fs::read_to_string(codex_home.join("history.jsonl")).unwrap();
        assert_eq!(history.lines().count(), 3);
        assert!(history.starts_with(&format!(r#"{{"session_id":"x1","ts":{},"text":"fix the readme"}}"#, (start / 1000))), "{history}");
    }
}

/// The scripts against the player: the same scenario, the same bytes; and the scripts as the CLIs
/// Glasscode starts. They run where `python3` and `sh` are, as the stand-ins do (the Windows
/// `.cmd` twins are not run by a test).
#[cfg(all(test, unix))]
mod scripts {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Stdio};

    use super::*;
    use crate::model::{AgentKind, Session};
    use crate::testkit::home::{Home, HomeSpec};

    /// Glasscode's own hook command on Unix (`agents::claude_hook_settings`).
    const HOOK: &str = r#"cat >> "$GLASSCODE_EVENTS"; printf '\n' >> "$GLASSCODE_EVENTS""#;
    const START: u64 = 1_795_078_800_000;
    const EVENTS: [&str; 7] = ["SessionStart", "UserPromptSubmit", "Stop", "Notification", "StopFailure", "PreToolUse", "SessionEnd"];

    fn settings(events: &[&str], extra: &[(&str, Value)]) -> String {
        let mut hooks = serde_json::Map::new();
        for event in events {
            hooks.insert(event.to_string(), json!([{"hooks": [{"type": "command", "command": HOOK}]}]));
        }
        for (event, entry) in extra {
            hooks.entry(event.to_string()).or_insert(json!([])).as_array_mut().unwrap().push(entry.clone());
        }
        json!({"hooks": hooks}).to_string()
    }

    struct Ran {
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: String,
    }

    /// Runs a stand-in in `cwd` with the home's environment, `scenario` and virtual time.
    fn run(home: &Home, agent: &str, args: &[&str], scenario: Option<&Scenario>, cwd: &Path, stdin: &str, env: &[(&str, String)]) -> Ran {
        let mut command = home.command(scripts_dir().join(agent));
        command.args(args).current_dir(cwd).env(NOW_VARIABLE, START.to_string());
        if let Some(scenario) = scenario {
            let file = home.top().join(format!("scenario-{}.json", rand::random::<u32>()));
            scenario.save(&file).unwrap();
            command.env(SCENARIO_VARIABLE, file);
        }
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap_or_else(|err| panic!("{agent}: {err} (python3 on PATH?)"));
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        Ran { status: out.status, stdout: out.stdout, stderr: String::from_utf8_lossy(&out.stderr).into_owned() }
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
    }

    #[test]
    fn stand_ins_replay_claude_byte_for_byte_in_both_modes() {
        let home = Home::with(&HomeSpec::bare());
        let cwd = home.project("app").root.clone();
        let endings = [Ending::Completed, Ending::Interrupted, Ending::ApiError, Ending::Question, Ending::Pushed];
        let prompts = ["fix the readme", "run the tests", "again", "and the docs?", "push it"];
        for (n, mode) in [Mode::Interactive, Mode::Print].into_iter().enumerate() {
            let scenario = claude(mode, &endings);
            let sid = format!("00000000-0000-4000-8000-00000000000{n}");
            let transcript = claude_transcript(&home.claude_config_dir(), &cwd, &sid);
            let events = home.top().join(format!("events-{n}.jsonl"));
            let settings = settings(&EVENTS, &[]);
            let mut args = vec!["--session-id", sid.as_str(), "--settings", settings.as_str()];
            let stdin = match mode {
                Mode::Interactive => prompts.iter().map(|p| format!("{p}\n")).collect::<String>(),
                Mode::Print => {
                    args.extend(["-p", "--verbose", "--output-format", "stream-json", "--input-format", "stream-json"]);
                    prompts.iter().map(|p| json!({"type": "user", "message": {"role": "user", "content": p}}).to_string() + "\n").collect()
                }
            };
            let ran = run(&home, "claude", &args, Some(&scenario), &cwd, &stdin, &[("GLASSCODE_EVENTS", events.to_string_lossy().into_owned())]);
            assert!(ran.status.success(), "{mode:?}: {}", ran.stderr);
            // The player, with the same values, into files of its own.
            let expected = home.top().join(format!("expected-{n}"));
            let mut sink = FileSink { files: BTreeMap::from([(Out::Transcript, expected.join("transcript.jsonl"))]), events: Some(expected.join("events.jsonl")), hook_events: Some(EVENTS.iter().map(|e| e.to_string()).collect()), ..FileSink::default() };
            assert_eq!(play_session(&scenario, claude_vars(&sid, &cwd, &transcript, mode), START, &prompts, &mut sink), End::Running);
            assert_eq!(read(&transcript), read(&expected.join("transcript.jsonl")), "{mode:?}: the transcript");
            assert_eq!(read(&events), read(&expected.join("events.jsonl")), "{mode:?}: the hook events");
            assert_eq!(String::from_utf8(ran.stdout).unwrap(), sink.stdout, "{mode:?}: what it printed");
        }
    }

    #[test]
    fn stand_ins_replay_codex_byte_for_byte_in_both_modes() {
        let home = Home::with(&HomeSpec::bare());
        let cwd = home.project("app").root.clone();
        let endings = [Ending::Completed, Ending::Interrupted, Ending::ApiError, Ending::Question, Ending::Pushed];
        let prompts = ["fix the readme", "run the tests", "again", "and the docs?", "push it"];
        // In a terminal: one conversation, a turn per line.
        let scenario = codex(Mode::Interactive, &endings);
        let sid = "019a0000-0000-7000-8000-0000000000a1";
        let stdin: String = prompts.iter().map(|p| format!("{p}\n")).collect();
        let ran = run(&home, "codex", &["-c", "model=\"x\"", "--sandbox", "workspace-write", "--ask-for-approval", "on-request"], Some(&scenario), &cwd, &stdin, &[("GLASSCODE_TEST_SESSION_ID", sid.into())]);
        assert!(ran.status.success(), "{}", ran.stderr);
        let rollout = codex_rollout(&home.codex_home(), sid, START);
        let expected = home.top().join("expected");
        let mut sink = FileSink { files: BTreeMap::from([(Out::Rollout, expected.join("rollout.jsonl")), (Out::History, expected.join("history.jsonl"))]), ..FileSink::default() };
        play_session(&scenario, codex_vars(sid, &cwd, Mode::Interactive), START, &prompts, &mut sink);
        assert_eq!(read(&rollout), read(&expected.join("rollout.jsonl")));
        assert_eq!(read(&home.codex_home().join("history.jsonl")), read(&expected.join("history.jsonl")));
        assert_eq!(String::from_utf8(ran.stdout).unwrap(), sink.stdout);
        // `codex exec --json`: one turn a run, each ending.
        for (n, ending) in endings.iter().enumerate() {
            let scenario = codex(Mode::Print, &[*ending]);
            let sid = format!("019a0000-0000-7000-8000-0000000000b{n}");
            let ran = run(&home, "codex", &["exec", "--json", "--sandbox", "workspace-write", "--", prompts[n]], Some(&scenario), &cwd, "", &[("GLASSCODE_TEST_SESSION_ID", sid.clone())]);
            assert!(ran.status.success(), "{ending:?}: {}", ran.stderr);
            let mut sink = FileSink { files: BTreeMap::from([(Out::Rollout, expected.join(format!("exec-{n}.jsonl")))]), ..FileSink::default() };
            play_session(&scenario, codex_vars(&sid, &cwd, Mode::Print), START, &[prompts[n]], &mut sink);
            assert_eq!(read(&codex_rollout(&home.codex_home(), &sid, START)), read(&expected.join(format!("exec-{n}.jsonl"))), "{ending:?}");
            assert_eq!(String::from_utf8(ran.stdout).unwrap(), sink.stdout, "{ending:?}");
        }
        // `codex exec resume <id>` goes on in the same rollout, without a second header.
        let scenario = codex(Mode::Print, &[Ending::Completed]);
        let sid = "019a0000-0000-7000-8000-0000000000b0";
        let before = read(&codex_rollout(&home.codex_home(), sid, START)).lines().count();
        let ran = run(&home, "codex", &["exec", "--json", "resume", sid, "and again"], Some(&scenario), &cwd, "", &[]);
        assert!(ran.status.success(), "{}", ran.stderr);
        let after = read(&codex_rollout(&home.codex_home(), sid, START));
        assert!(after.lines().count() > before && after.matches("session_meta").count() == 1, "{after}");
        assert!(run(&home, "codex", &["resume", "no-such-id"], Some(&scenario), &cwd, "", &[]).stderr.contains("No saved session"));
    }

    /// What a tape's steps write, by sink, as their own text: right for a tape with no placeholder.
    fn written(steps: &[Step]) -> BTreeMap<&'static str, String> {
        let mut out: BTreeMap<&'static str, String> = BTreeMap::new();
        for step in steps {
            let (sink, text) = match &step.what {
                What::Transcript { text } => ("transcript", text),
                What::Rollout { text } => ("rollout", text),
                What::History { text } => ("history", text),
                What::Hook { text, .. } => ("hook", text),
                What::Stdout { text } => ("stdout", text),
                _ => continue,
            };
            out.entry(sink).or_default().push_str(&format!("{text}\n"));
        }
        out
    }

    /// Plays `steps` as one turn of `claude -p --output-format stream-json` or `codex exec --json`,
    /// through the stand-in and through the player: (what the stand-in wrote, what the player
    /// wrote), by sink. The player only fills placeholders, so the two are equal only if the
    /// stand-in keeps every other byte as the tape has it.
    fn replay(home: &Home, agent: &str, steps: &[Step], sid: &str) -> (BTreeMap<&'static str, String>, BTreeMap<&'static str, String>) {
        let cwd = home.project("app").root.clone();
        let scenario = Scenario { agent: agent.into(), version: if agent == "claude" { CLAUDE_VERSION } else { CODEX_VERSION }.into(), source: "a tape".into(), turns: vec![steps.to_vec()], ..Scenario::default() };
        let expected = home.top().join(format!("expected-{sid}"));
        let events: Vec<String> = steps.iter().filter_map(|step| if let What::Hook { event, .. } = &step.what { Some(event.clone()) } else { None }).collect();
        let event_names: Vec<&str> = events.iter().map(String::as_str).collect();
        let prompt = "the prompt";
        let mut got = BTreeMap::new();
        let mut sink = FileSink::default();
        if agent == "claude" {
            let transcript = claude_transcript(&home.claude_config_dir(), &cwd, sid);
            let hooks = home.top().join(format!("events-{sid}.jsonl"));
            let settings = settings(&event_names, &[]);
            let ran = run(home, "claude", &["--session-id", sid, "--settings", &settings, "-p", "--output-format", "stream-json", prompt], Some(&scenario), &cwd, "", &[("GLASSCODE_EVENTS", hooks.to_string_lossy().into_owned())]);
            assert!(ran.status.success(), "{}", ran.stderr);
            got.insert("transcript", std::fs::read_to_string(&transcript).unwrap_or_default());
            got.insert("hook", std::fs::read_to_string(&hooks).unwrap_or_default());
            got.insert("stdout", String::from_utf8(ran.stdout).unwrap());
            sink = FileSink { files: BTreeMap::from([(Out::Transcript, expected.join("transcript"))]), events: Some(expected.join("hook")), hook_events: Some(events.clone()), ..sink };
            play_session(&scenario, claude_vars(sid, &cwd, &transcript, Mode::Print), START, &[prompt], &mut sink);
        } else {
            let ran = run(home, "codex", &["exec", "--json", "--", prompt], Some(&scenario), &cwd, "", &[(SESSION_VARIABLE, sid.into())]);
            assert!(ran.status.success(), "{}", ran.stderr);
            got.insert("rollout", std::fs::read_to_string(codex_rollout(&home.codex_home(), sid, START)).unwrap_or_default());
            got.insert("history", std::fs::read_to_string(home.codex_home().join("history.jsonl")).unwrap_or_default());
            got.insert("stdout", String::from_utf8(ran.stdout).unwrap());
            sink = FileSink { files: BTreeMap::from([(Out::Rollout, expected.join("rollout")), (Out::History, expected.join("history"))]), ..sink };
            play_session(&scenario, codex_vars(sid, &cwd, Mode::Print), START, &[prompt], &mut sink);
        }
        let mut want = BTreeMap::new();
        for name in got.keys() {
            let text = if *name == "stdout" { sink.stdout.clone() } else { std::fs::read_to_string(expected.join(name)).unwrap_or_default() };
            want.insert(*name, text);
        }
        (got, want)
    }

    /// Tapes with no placeholder, in bytes a JSON writer would not give back (spaces after the
    /// colons, `\u00e9`, `\/`, `\u2028`): the stand-ins write them as they are, to every sink:
    /// Claude's transcript, hook events and stdout; Codex's rollout, history and stdout. (Literal
    /// tapes written for the test; the recorded ones are M01's, below.)
    #[test]
    fn literal_tapes_are_replayed_byte_for_byte() {
        let home = Home::with(&HomeSpec::bare());
        let tapes = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/evals/fixtures/tapes");
        for (agent, file, sid, sinks) in [("claude", "claude-literal.tape.jsonl", "lit-0001", ["hook", "stdout", "transcript"]), ("codex", "codex-literal.tape.jsonl", "lit-x1", ["history", "rollout", "stdout"])] {
            let steps = Scenario::read_tape(&read(&tapes.join(file))).unwrap();
            let (got, want) = replay(&home, agent, &steps, sid);
            assert_eq!(got, want, "{file}: the stand-in and the player");
            let own = written(&steps);
            assert_eq!(own.keys().copied().collect::<Vec<_>>(), sinks, "{file}: a line for every sink");
            for (sink, text) in &own {
                assert_eq!(&got[sink], text, "{file}: {sink} as the tape has it");
            }
            assert!(got["stdout"].contains(concat!(r"caf\u", r"00e9")) || got["stdout"].contains(concat!(r"\u", "2028")), "{file}: the escapes stay");
        }
    }

    /// M01's recorded turns, once they are in `tests/evals/fixtures/tapes/recorded/`: each is
    /// replayed by the stand-in, every byte but the placeholders as the CLI wrote it.
    #[test]
    #[ignore = "needs M01: recorded tapes in tests/evals/fixtures/tapes/recorded"]
    fn recorded_turns_are_replayed_byte_for_byte() {
        let home = Home::with(&HomeSpec::bare());
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/evals/fixtures/tapes/recorded");
        let mut tapes: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with(".tape.jsonl")).collect();
        tapes.sort();
        for agent in ["claude", "codex"] {
            assert!(tapes.iter().any(|t| t.file_name().unwrap().to_string_lossy().starts_with(&format!("{agent}-"))), "needs M01: no recorded {agent} tape in {}", dir.display());
        }
        for (n, tape) in tapes.iter().enumerate() {
            let name = tape.file_name().unwrap().to_string_lossy().into_owned();
            let agent = if name.starts_with("codex-") { "codex" } else { "claude" };
            let steps = Scenario::read_tape(&read(tape)).unwrap();
            let (got, want) = replay(&home, agent, &steps, &format!("00000000-0000-4000-8000-0000000{n:05}"));
            assert_eq!(got, want, "{name}");
        }
    }

    #[test]
    fn a_crashed_stand_in_dies_as_under_kill_9() {
        let home = Home::with(&HomeSpec::bare());
        let cwd = home.project("app").root.clone();
        let events = home.top().join("events.jsonl");
        for (agent, scenario) in [("claude", claude(Mode::Interactive, &[Ending::Crashed])), ("codex", codex(Mode::Interactive, &[Ending::Crashed]))] {
            let settings = settings(&EVENTS, &[]);
            let args: Vec<&str> = if agent == "claude" { vec!["--settings", &settings] } else { vec![] };
            let ran = run(&home, agent, &args, Some(&scenario), &cwd, "run the tests\nnever read\n", &[("GLASSCODE_EVENTS", events.to_string_lossy().into_owned())]);
            assert_eq!(ran.status.signal(), Some(9), "{agent}: {:?} {}", ran.status, ran.stderr);
            assert!(!String::from_utf8_lossy(&ran.stdout).contains("never read"));
        }
        let names: Vec<String> = read(&events).lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["hook_event_name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["SessionStart", "UserPromptSubmit"], "no Stop and no SessionEnd after a crash");
    }

    /// A `PreToolUse` hook refuses the push, by exit code 2 or by its answer, and the stand-in
    /// goes on as Claude does: the call fails with the hook's reason.
    #[test]
    fn a_pre_tool_use_hook_refuses_a_push() {
        let home = Home::with(&HomeSpec::bare());
        let cwd = home.project("app").root.clone();
        let scenario = claude(Mode::Interactive, &[Ending::Pushed]);
        let by_exit = json!({"matcher": "Bash", "hooks": [{"type": "command", "command": "echo 'pushes wait for the person' >&2; exit 2"}]});
        let by_answer = json!({"matcher": "Bash", "hooks": [{"type": "command", "command": r#"echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"not on the default branch"}}'"#}]});
        let elsewhere = json!({"matcher": "Edit|Write", "hooks": [{"type": "command", "command": "exit 2"}]});
        for (n, (entry, reason)) in [(by_exit, Some("pushes wait for the person")), (by_answer, Some("not on the default branch")), (elsewhere, None)].into_iter().enumerate() {
            let sid = format!("00000000-0000-4000-8000-0000000000c{n}");
            let settings = settings(&[], &[("PreToolUse", entry)]);
            let ran = run(&home, "claude", &["--session-id", &sid, "--settings", &settings], Some(&scenario), &cwd, "push it\n", &[]);
            assert!(ran.status.success(), "{}", ran.stderr);
            let transcript = claude_transcript(&home.claude_config_dir(), &cwd, &sid);
            let reply = crate::transcript::last_reply(AgentKind::Claude, &transcript).unwrap();
            match reason {
                Some(reason) => assert_eq!(reply, format!("The push was refused: {reason}")),
                None => assert_eq!(reply, "Pushed main."),
            }
        }
    }

    /// The argument lists Glasscode builds reach the stand-ins, which read them as the recorded
    /// help does; `--version` and `--help` after any of them answer (what M22's argument contract
    /// runs); an option the help does not have is refused; the person's folders are refused.
    #[test]
    fn stand_ins_read_glasscodes_arguments_and_refuse_the_rest() {
        let home = Home::with(&HomeSpec::bare());
        let cwd = home.project("app").root.clone();
        let session = |agent: AgentKind, conversation: Option<&str>, mode: Option<&str>| Session {
            id: "s-args".into(),
            project_id: "p-app".into(),
            worktree: cwd.clone(),
            branch: "main".into(),
            title: "t".into(),
            agent,
            account_id: None,
            agent_session_id: conversation.map(str::to_string),
            transcript_path: None,
            lineage: vec![],
            initial_prompt: Some("- a prompt that starts like an option".into()),
            team_task: None,
            review: None,
            model: Some("a-model".into()),
            mode: mode.map(str::to_string),
            created_at: 0,
        };
        let env = home.agent_env();
        for (agent, mode) in [(AgentKind::Claude, Some("plan")), (AgentKind::Claude, None), (AgentKind::Codex, Some("full-auto")), (AgentKind::Codex, Some("read-only"))] {
            for conversation in [None, Some("019a0000-0000-7000-8000-0000000000d1")] {
                let launch = crate::agents::launch(&session(agent, conversation, mode), Some(home.account(agent)), &env);
                assert!(launch.program.starts_with(&*scripts_dir().to_string_lossy()), "{}", launch.program);
                for last in ["--version", "--help"] {
                    let mut args: Vec<String> = launch.args.clone();
                    args.retain(|a| a != "--");
                    args.retain(|a| !a.starts_with("- a prompt"));
                    args.push(last.into());
                    let args: Vec<&str> = args.iter().map(String::as_str).collect();
                    let ran = run(&home, agent.name().to_lowercase().as_str(), &args, None, &cwd, "", &[]);
                    assert!(ran.status.success(), "{agent:?} {args:?}: {}", ran.stderr);
                }
            }
        }
        let version = run(&home, "claude", &["--version"], None, &cwd, "", &[]);
        assert_eq!(String::from_utf8(version.stdout).unwrap(), "2.1.322 (Claude Code)\n");
        assert_eq!(String::from_utf8(run(&home, "codex", &["--version"], None, &cwd, "", &[]).stdout).unwrap(), "codex-cli 0.167.0\n");
        assert_eq!(run(&home, "claude", &["--no-such-option"], None, &cwd, "", &[]).status.code(), Some(1));
        // A stand-in copied without the recorded help says so, and finds it through a variable.
        let copy = home.top().join("copied");
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::copy(scripts_dir().join("claude"), copy.join("claude")).unwrap();
        let copied = |help: Option<&Path>| {
            let mut command = home.command("python3");
            command.arg(copy.join("claude")).arg("--version").current_dir(&cwd);
            if let Some(help) = help {
                command.env("GLASSCODE_TEST_CLI_HELP", help);
            }
            command.output().unwrap()
        };
        let lost = copied(None);
        assert_eq!(lost.status.code(), Some(3));
        assert!(String::from_utf8_lossy(&lost.stderr).contains("no recorded help"), "{}", String::from_utf8_lossy(&lost.stderr));
        let found = copied(Some(&scripts_dir().parent().unwrap().join("cli-help")));
        assert_eq!(String::from_utf8_lossy(&found.stdout), "2.1.322 (Claude Code)\n");
        assert_eq!(run(&home, "codex", &["exec", "--no-such-option", "x"], None, &cwd, "", &[]).status.code(), Some(2));
        // Without a scratch folder nothing runs.
        let scenario = claude(Mode::Print, &[Ending::Completed]);
        let ran = run(&home, "claude", &["-p", "hi"], Some(&scenario), &cwd, "", &[("CLAUDE_CONFIG_DIR", String::new())]);
        assert_eq!(ran.status.code(), Some(3), "{}", ran.stderr);
        assert!(!home.claude_config_dir().join("projects").exists(), "nothing was written");
        // The person's own folders are refused. The real ones are only checked
        // (GLASSCODE_TEST_CHECK_ONLY stops before anything is written, so a check that went wrong
        // could not write there); a home the kit names as the person's is refused as well.
        let check = |agent: &str, variable: &str, value: String, person: Option<&Path>| {
            let mut env = vec![(variable, value), ("GLASSCODE_TEST_CHECK_ONLY", "1".to_string())];
            if let Some(person) = person {
                env.push(("GLASSCODE_TEST_PERSON_HOME", person.to_string_lossy().into_owned()));
            }
            run(&home, agent, &["-p", "hi"], None, &cwd, "", &env)
        };
        let real = crate::paths::home();
        for (agent, variable, folder) in [("claude", "CLAUDE_CONFIG_DIR", ".claude"), ("codex", "CODEX_HOME", ".codex")] {
            let ran = check(agent, variable, real.join(folder).to_string_lossy().into_owned(), None);
            assert_eq!(ran.status.code(), Some(3), "{agent}: the real {folder}: {}", ran.stderr);
            let ran = check(agent, variable, real.join(folder).join("projects").to_string_lossy().into_owned(), None);
            assert_eq!(ran.status.code(), Some(3), "{agent}: inside the real {folder}: {}", ran.stderr);
        }
        let person = home.top().join("person");
        let ran = check("claude", "CLAUDE_CONFIG_DIR", person.join(".claude").to_string_lossy().into_owned(), Some(&person));
        assert_eq!(ran.status.code(), Some(3), "{}", ran.stderr);
        let ran = check("claude", "CLAUDE_CONFIG_DIR", home.claude_config_dir().to_string_lossy().into_owned(), None);
        assert!(ran.status.success() && String::from_utf8_lossy(&ran.stdout).starts_with("scratch: "), "{}", ran.stderr);
        assert!(!home.claude_config_dir().join("projects").exists(), "a check writes nothing");
    }

    /// Glasscode starts a stand-in as it starts Claude and Codex, with its own hook settings; its
    /// own readers follow the session: the hook events, the transcript, the rollout.
    #[test]
    fn glasscode_follows_a_stand_in_session_it_started() {
        let home = Home::with(&HomeSpec::bare());
        // The hooks run `<program> cli hook --stamp`; the test binary is no `glasscode`, so a
        // script of the test's own stands in: each event onto the session's events file.
        let hook = home.top().join("hook");
        std::fs::write(&hook, "#!/bin/sh\ncat >> \"$GLASSCODE_EVENTS\"; printf '\\n' >> \"$GLASSCODE_EVENTS\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // SAFETY: the core tests run one at a time (`--test-threads=1`); put back below.
        unsafe { std::env::set_var("GLASSCODE_TEST_HOOK_PROGRAM", &hook) };
        struct Unset;
        impl Drop for Unset {
            fn drop(&mut self) {
                // SAFETY: as above.
                unsafe { std::env::remove_var("GLASSCODE_TEST_HOOK_PROGRAM") };
            }
        }
        let _unset = Unset;
        let worktree = home.project("app").worktrees[0].clone();
        let mut session = Session {
            id: "s-live".into(),
            project_id: "p-app".into(),
            worktree: worktree.clone(),
            branch: "feat/login".into(),
            title: "t".into(),
            agent: AgentKind::Claude,
            account_id: Some(crate::testkit::home::CLAUDE_ACCOUNT.into()),
            agent_session_id: Some("00000000-0000-4000-8000-0000000000e1".into()),
            transcript_path: None,
            lineage: vec![],
            initial_prompt: Some("fix the readme".into()),
            team_task: None,
            review: None,
            model: None,
            mode: None,
            created_at: 0,
        };
        let start = |session: &Session, stdin: &str| {
            let launch = crate::agents::launch(session, Some(home.account(session.agent)), &home.agent_env());
            let mut command = home.command(&launch.program);
            command.args(&launch.args).envs(&launch.env).current_dir(&worktree).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
            let mut child = command.spawn().unwrap();
            child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        };
        let mut tail = crate::agents::EventsTail::new(&session.id);
        start(&session, "and the setup doc?\n");
        let update = tail.poll().expect("the hooks reported");
        assert_eq!(update.conversation_id.as_deref(), Some("00000000-0000-4000-8000-0000000000e1"));
        // The second turn ended on a question, and Claude said it waits (Glasscode's settings have
        // no SessionEnd hook).
        assert_eq!(update.activity, Some(crate::agents::AgentActivity::NeedsInput));
        // The app's reading of the same lines: the idle reminder a minute after the turn is no
        // wait (the app tells the finish itself, `finish_ui`), and the turn's end is what is left.
        let app = crate::agents::EventsTail::resume(&session.id, Some(0)).reporting_finishes().poll().expect("the same lines");
        assert_eq!(app.activity, Some(crate::agents::AgentActivity::Idle));
        let transcript = update.transcript_path.unwrap();
        assert_eq!(crate::agents::find_claude_transcript(&home.claude_config_dir(), "00000000-0000-4000-8000-0000000000e1"), Some(transcript.clone()));
        let page = crate::transcript::read(AgentKind::Claude, &transcript, None, 100).unwrap();
        let users: Vec<&str> = page.items.iter().filter(|i| i.kind == crate::transcript::ItemKind::User).map(|i| i.text.as_str()).collect();
        assert_eq!(users, ["fix the readme", "and the setup doc?"]);
        assert!(crate::history::conversations(AgentKind::Claude, &home.built().accounts, &worktree).iter().any(|c| c.id == "00000000-0000-4000-8000-0000000000e1"));

        // Codex: found by its folder, followed by its rollout.
        session.agent = AgentKind::Codex;
        session.account_id = Some(crate::testkit::home::CODEX_ACCOUNT.into());
        session.agent_session_id = None;
        let since = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
        start(&session, "");
        let (id, rollout) = crate::agents::find_codex_conversation(&home.codex_home(), &worktree, since).expect("the rollout");
        let mut follow = crate::agents::RolloutTail::new(&rollout);
        follow.poll();
        assert_eq!(follow.turn, crate::agents::CodexTurn::Ended);
        assert_eq!(crate::history::session_title(AgentKind::Codex, &rollout).as_deref(), Some("fix the readme"));
        assert!(crate::history::conversations(AgentKind::Codex, &home.built().accounts, &worktree).iter().any(|c| c.id == id));
    }

    /// A hook that reads none of its payload (`true`), and a reader that goes away (`| head`): the
    /// stand-in goes on, or ends quietly, as the CLI does.
    #[test]
    fn hooks_that_read_nothing_and_readers_that_go_away() {
        let home = Home::with(&HomeSpec::bare());
        let cwd = home.project("app").root.clone();
        let scenario = claude(Mode::Print, &[Ending::Completed]);
        let settings = json!({"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": "true"}]}]}}).to_string();
        let prompt = "fix the readme ".repeat(14_000);
        let ran = run(&home, "claude", &["--settings", &settings, "-p", "--output-format", "stream-json"], Some(&scenario), &cwd, &prompt, &[]);
        assert!(ran.status.success(), "{:?}: {}", ran.status, ran.stderr);
        assert!(String::from_utf8_lossy(&ran.stdout).contains(r#""subtype":"success""#));
        // A hook past its timeout is an error that blocks nothing, as in the CLI.
        let slow = json!({"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": "sleep 5", "timeout": 1}]}]}}).to_string();
        let ran = run(&home, "claude", &["--settings", &slow, "-p", "hi"], Some(&scenario), &cwd, "", &[]);
        let stderr = ran.stderr.clone();
        assert!(ran.status.success() && stderr.contains("past its timeout") && !stderr.contains("Traceback"), "{stderr}");
        // stdout closed after the first line: no trace, no hang.
        let mut command = home.command(scripts_dir().join("claude"));
        command.args(["-p", "hi"]).current_dir(&cwd).env(SCENARIO_VARIABLE, home.built().scenarios.join("claude-print.json")).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        drop(child.stdout.take());
        let out = child.wait_with_output().unwrap();
        assert!(!String::from_utf8_lossy(&out.stderr).contains("Traceback"), "{}", String::from_utf8_lossy(&out.stderr));
        // A run step outside a scratch home is refused.
        let outside = tempfile::tempdir().unwrap();
        let step = Scenario { agent: "claude".into(), version: CLAUDE_VERSION.into(), turns: vec![vec![Step::new(0, What::Run { command: "touch ran".into() })]], ..Scenario::default() };
        let ran = run(&home, "claude", &["-p", "x"], Some(&step), outside.path(), "", &[]);
        assert_eq!(ran.status.code(), Some(3), "{}", ran.stderr);
        assert!(!outside.path().join("ran").exists());
        let ran = run(&home, "claude", &["-p", "x"], Some(&step), &cwd, "", &[]);
        assert!(ran.status.success() && cwd.join("ran").exists(), "{}", ran.stderr);
        // A step the player does not know stops it.
        let mut odd = serde_json::to_value(&step).unwrap();
        odd["turns"][0][0] = json!({"to": "teleport"});
        let file = home.top().join("odd.json");
        std::fs::write(&file, odd.to_string()).unwrap();
        let ran = run(&home, "claude", &["-p", "x"], None, &cwd, "", &[(SCENARIO_VARIABLE, file.to_string_lossy().into_owned())]);
        assert_eq!(ran.status.code(), Some(3), "{}", ran.stderr);
    }

    /// The brain's way (M50's driver): one user line on stdin, the answer's
    /// `result` line on stdout while stdin stays open, then the next; a control request is answered,
    /// not played as a turn; the end of input ends the run.
    #[test]
    fn a_brain_gets_each_answer_before_it_asks_again() {
        use std::io::{BufRead, BufReader};
        use std::sync::mpsc;
        use std::time::Duration;

        let home = Home::with(&HomeSpec::bare());
        let file = home.top().join("brain.json");
        claude(Mode::Print, &[Ending::Completed, Ending::Question]).save(&file).unwrap();
        let mut child = home
            .command(scripts_dir().join("claude"))
            .args(["-p", "--verbose", "--input-format", "stream-json", "--output-format", "stream-json", "--session-id", "brain-1"])
            .current_dir(&home.project("app").root)
            .env(SCENARIO_VARIABLE, &file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (lines, rx) = mpsc::channel::<Value>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if lines.send(serde_json::from_str(&line).unwrap_or(Value::Null)).is_err() {
                    break;
                }
            }
        });
        // It owns stdin: dropping it ends the input.
        let mut send = move |value: Value| {
            stdin.write_all(format!("{value}\n").as_bytes()).unwrap();
            stdin.flush().unwrap();
        };
        let until_result = |rx: &mpsc::Receiver<Value>| -> Vec<Value> {
            let mut seen = Vec::new();
            loop {
                let line = rx.recv_timeout(Duration::from_secs(20)).expect("the answer comes while stdin is open");
                seen.push(line.clone());
                if line["type"] == "result" {
                    return seen;
                }
            }
        };
        send(json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": "fix the readme"}]}}));
        let first = until_result(&rx);
        assert_eq!(first[0]["subtype"], "init");
        assert_eq!(first.last().unwrap()["result"], last_words(Ending::Completed));
        send(json!({"type": "control_request", "request_id": "req_1", "request": {"subtype": "interrupt"}}));
        let answer = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!((answer["type"].as_str(), answer["response"]["request_id"].as_str()), (Some("control_response"), Some("req_1")));
        send(json!({"type": "user", "message": {"role": "user", "content": "and the docs?"}}));
        let second = until_result(&rx);
        assert_eq!(second.last().unwrap()["result"], last_words(Ending::Question));
        assert!(second.iter().all(|line| line["type"] != "system"), "one init, at the start");
        drop(send);
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_err(), "nothing after the end of input");
    }

    /// Started with the brain's flags (P01), the stand-in's init line lists what a real Claude
    /// lists: the built-in tools `--tools` names and the tools its `--mcp-config` servers serve,
    /// as `mcp__<server>__<tool>`, so the brain's tool check accepts it. A server that does not
    /// start is "failed" and serves nothing; a scenario that names MCP tools keeps its own line.
    #[cfg(unix)]
    #[test]
    fn the_brains_flags_give_the_init_line_its_mcp_tools() {
        let home = Home::with(&HomeSpec::bare());
        let cwd = home.project("app").root.clone();
        let names: Vec<String> = crate::mcp::tools(crate::mcp::Profile::Manager).iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect();
        let listed = home.top().join("tools.json");
        std::fs::write(&listed, serde_json::to_string(&names).unwrap()).unwrap();
        let server = home.top().join("mcp-server.py");
        std::fs::write(
            &server,
            "import json, sys\nnames = json.load(open(sys.argv[1]))\nfor line in sys.stdin:\n    m = json.loads(line)\n    if m.get('id') == 1:\n        print(json.dumps({'jsonrpc': '2.0', 'id': 1, 'result': {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'glasscode', 'version': '1'}}}), flush=True)\n    elif m.get('id') == 2:\n        print(json.dumps({'jsonrpc': '2.0', 'id': 2, 'result': {'tools': [{'name': n} for n in names]}}), flush=True)\n",
        )
        .unwrap();
        let config = |command: &str| json!({ "mcpServers": { "glasscode": { "type": "stdio", "command": command, "args": [server.to_string_lossy(), listed.to_string_lossy()], "env": {} } } }).to_string();
        let init = |config: String, scenario: Option<&Scenario>| -> Value {
            // The prompt first: `--mcp-config` takes every word after it, as Claude's parser does.
            let args = ["-p", "hello", "--output-format", "stream-json", "--verbose", "--tools", "ToolSearch", "--strict-mcp-config", "--mcp-config", config.as_str()];
            let ran = run(&home, "claude", &args, scenario, &cwd, "", &[]);
            assert!(ran.status.success(), "{}", ran.stderr);
            let text = String::from_utf8(ran.stdout).unwrap();
            text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find(|v| v["subtype"] == "init").expect("an init line")
        };
        let scenario = claude(Mode::Print, &[Ending::Completed]);
        let line = init(config("python3"), Some(&scenario));
        let tools: Vec<String> = line["tools"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
        assert_eq!(tools[0], "ToolSearch");
        assert!(crate::mcp::init_tools_ok(&tools, crate::mcp::Profile::Manager), "{tools:?}");
        assert!(crate::runtime::claude_expected(&tools).is_ok());
        assert_eq!(line["mcp_servers"], json!([{ "name": "glasscode", "status": "connected" }]));
        // A server that cannot start: failed, and only the built-in tool.
        let failed = init(config("glasscode-no-such-server"), Some(&scenario));
        assert_eq!((failed["tools"].clone(), failed["mcp_servers"][0]["status"].clone()), (json!(["ToolSearch"]), json!("failed")));
        // A scenario that names its MCP tools keeps them.
        let mut own = scenario.clone();
        for step in own.start.iter_mut() {
            if let What::Stdout { text } = &mut step.what
                && text.contains("\"init\"")
            {
                let mut value: Value = serde_json::from_str(text).unwrap();
                value["tools"] = json!(["mcp__glasscode__status"]);
                *text = value.to_string();
            }
        }
        assert_eq!(init(config("python3"), Some(&own))["tools"], json!(["mcp__glasscode__status"]));
    }

    /// `claude -p` prints the answer's text, `--output-format json` the result line; `codex exec`
    /// prints the last message, `--json` every event (what the one-process brain and auto-review
    /// read).
    #[test]
    fn a_program_run_prints_its_answer_as_asked() {
        let home = Home::with(&HomeSpec::bare());
        let cwd = home.project("app").root.clone();
        let stdout = |agent: &str, args: &[&str]| {
            let ran = run(&home, agent, args, None, &cwd, "", &[]);
            assert!(ran.status.success(), "{agent} {args:?}: {}", ran.stderr);
            String::from_utf8(ran.stdout).unwrap()
        };
        assert_eq!(stdout("claude", &["-p", "fix the readme"]), format!("{}\n", last_words(Ending::Completed)));
        let json_line = stdout("claude", &["-p", "--output-format", "json", "fix the readme"]);
        assert_eq!(json_line.lines().count(), 1);
        assert_eq!(serde_json::from_str::<Value>(&json_line).unwrap()["type"], "result");
        assert!(stdout("claude", &["-p", "--output-format", "stream-json", "--verbose", "fix the readme"]).lines().count() > 5);
        assert_eq!(stdout("codex", &["exec", "fix the readme"]), format!("{}\n", last_words(Ending::Completed)));
        assert!(stdout("codex", &["exec", "--json", "fix the readme"]).lines().all(|line| serde_json::from_str::<Value>(line).is_ok()));
    }

    /// A home whose Glasscode folder is outside its top folder (a feature build's test root): a
    /// `run` step runs in a worktree, which is in the Glasscode folder, as in the main checkout
    /// (review of PR 85, round 2).
    #[test]
    fn a_run_step_runs_in_a_worktree_of_a_home_outside_its_top() {
        let (top, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let built = crate::testkit::home::build_into(&top.path().join("home"), &outside.path().join("glasscode"), &HomeSpec::bare());
        assert!(!built.glasscode.starts_with(&built.top) && built.glasscode.join(crate::testkit::home::MARKER).is_file());
        let scenario = Scenario { agent: "claude".into(), version: CLAUDE_VERSION.into(), turns: vec![vec![Step::new(0, What::Run { command: "git status --porcelain > ran.txt".into() })]], ..Scenario::default() };
        let file = built.top.join("run.json");
        scenario.save(&file).unwrap();
        for cwd in [built.projects[0].worktrees[0].clone(), built.projects[0].root.clone()] {
            let mut command = crate::sys::command(scripts_dir().join("claude"));
            crate::testkit::home::clear_credentials(&mut command);
            let out = command.args(["-p", "x"]).envs(crate::testkit::home::agent_env(&built)).env(SCENARIO_VARIABLE, &file).current_dir(&cwd).output().unwrap();
            assert!(out.status.success(), "{}: {}", cwd.display(), String::from_utf8_lossy(&out.stderr));
            assert!(cwd.join("ran.txt").is_file(), "{}", cwd.display());
        }
    }

    #[test]
    fn the_transcript_folder_is_claudes_for_any_folder_name() {
        let home = Home::with(&HomeSpec::bare());
        let odd = home.top().join(format!("dir é 😀 {}", "x".repeat(190)));
        std::fs::create_dir_all(&odd).unwrap();
        let scenario = claude(Mode::Print, &[Ending::Completed]);
        let ran = run(&home, "claude", &["--session-id", "f1", "-p", "hi"], Some(&scenario), &odd, "", &[]);
        assert!(ran.status.success(), "{}", ran.stderr);
        let expected = claude_transcript(&home.claude_config_dir(), &odd, "f1");
        assert!(expected.parent().unwrap().file_name().unwrap().len() > 200, "a long name, cut and hashed");
        assert!(expected.is_file(), "{}", expected.display());
    }

    #[test]
    fn gh_answers_from_its_fixture_and_writes_down_every_call() {
        let home = Home::with(&HomeSpec::bare());
        let cwd = home.project("app").root.clone();
        let fixture = home.top().join("gh.json");
        std::fs::write(&fixture, json!({"calls": [{"args": ["pr", "view", "*"], "stdout": {"number": 7, "title": "Fix the readme"}}, {"args": ["api", "graphql"], "exact": true, "stdout": "{}", "exit": 4}]}).to_string()).unwrap();
        let gh = |args: &[&str]| run(&home, "gh", args, None, &cwd, "", &[("GLASSCODE_TEST_GH", fixture.to_string_lossy().into_owned())]);
        let view = gh(&["pr", "view", "7", "--json", "number,title"]);
        assert_eq!(serde_json::from_slice::<Value>(&view.stdout).unwrap()["title"], "Fix the readme");
        assert_eq!(gh(&["api", "graphql"]).status.code(), Some(4));
        assert!(gh(&["pr", "merge", "7", "--squash"]).status.success(), "a write succeeds, as gh's does");
        assert_eq!(gh(&["repo", "delete"]).status.code(), Some(1));
        let calls: Vec<Value> = read(&home.built().gh_log).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let merges = calls.iter().filter(|c| c["args"][0] == "pr" && c["args"][1] == "merge").count();
        assert_eq!((calls.len(), merges), (4, 1));
        assert!(!crate::testkit::home::is_real(&home.built().gh_log));
    }
}

/// The `.cmd` twins on Windows, through the kit's own command: the version, one turn with a hook
/// given in a settings file (as Glasscode gives it on Windows), and a refused folder, the person's
/// profile found by the system while the kit has moved USERPROFILE.
#[cfg(all(test, windows))]
mod windows_scripts {
    use std::process::Stdio;

    use super::*;
    use crate::testkit::home::{Home, HomeSpec};

    fn run(home: &Home, name: &str, args: &[&str], env: &[(&str, String)]) -> std::process::Output {
        let mut command = home.command(scripts_dir().join(name));
        command.args(args).current_dir(&home.project("app").root).stdin(Stdio::null());
        for (variable, value) in env {
            command.env(variable, value);
        }
        command.output().unwrap_or_else(|err| panic!("{name}.cmd: {err}"))
    }

    #[test]
    fn the_cmd_twins_run_on_windows() {
        let home = Home::with(&HomeSpec::bare());
        let text = |out: &std::process::Output| String::from_utf8_lossy(&out.stdout).into_owned();
        let version = run(&home, "claude", &["--version"], &[]);
        assert!(version.status.success(), "{}", String::from_utf8_lossy(&version.stderr));
        assert_eq!(text(&version), "2.1.322 (Claude Code)\n");
        assert_eq!(text(&run(&home, "codex", &["--version"], &[])), "codex-cli 0.167.0\n");
        assert!(text(&run(&home, "gh", &["--version"], &[])).starts_with("gh version"));

        // A turn, with a hook in a settings file that writes each payload to a file.
        let events = home.top().join("events.jsonl");
        let hook = home.top().join("hook.py");
        std::fs::write(&hook, "import sys\nopen(sys.argv[1], 'ab').write(sys.stdin.buffer.read() + b'\\n')\n").unwrap();
        let command = format!("python {} {}", hook.display(), events.display());
        let settings = home.top().join("settings.json");
        let entry = json!([{"hooks": [{"type": "command", "command": command}]}]);
        std::fs::write(&settings, json!({"hooks": {"UserPromptSubmit": entry, "Stop": entry}}).to_string()).unwrap();
        let scenario = home.top().join("scenario.json");
        claude(Mode::Print, &[Ending::Completed]).save(&scenario).unwrap();
        let settings_arg = settings.to_string_lossy().into_owned();
        let turn = run(&home, "claude", &["--session-id", "win-1", "--settings", &settings_arg, "-p", "fix the readme"], &[(SCENARIO_VARIABLE, scenario.to_string_lossy().into_owned())]);
        assert!(turn.status.success(), "{}", String::from_utf8_lossy(&turn.stderr));
        assert_eq!(text(&turn), format!("{}\n", last_words(Ending::Completed)));
        let transcript = crate::agents::find_claude_transcript(&home.claude_config_dir(), "win-1").expect("the transcript");
        let page = crate::transcript::read(crate::model::AgentKind::Claude, &transcript, None, 100).unwrap();
        assert_eq!(page.items.first().map(|item| item.text.as_str()), Some("fix the readme"));
        let hooks: Vec<Value> = std::fs::read_to_string(&events).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        let names: Vec<&str> = hooks.iter().map(|hook| hook["hook_event_name"].as_str().unwrap()).collect();
        assert_eq!(names, ["UserPromptSubmit", "Stop"]);
        assert!(hooks.iter().all(|hook| hook["session_id"] == "win-1"));

        // The person's own folders, found by Windows (USERPROFILE is the kit's scratch user here):
        // refused, and only checked, so nothing could be written there.
        let profile = crate::paths::home();
        let moved: String = home.agent_env()["USERPROFILE"].clone();
        assert_ne!(profile.to_string_lossy().into_owned(), moved, "the kit moved USERPROFILE");
        for (name, variable, folder) in [("claude", "CLAUDE_CONFIG_DIR", ".claude"), ("codex", "CODEX_HOME", ".codex")] {
            let env = [(variable, profile.join(folder).to_string_lossy().into_owned()), ("GLASSCODE_TEST_CHECK_ONLY", "1".to_string()), ("GLASSCODE_TEST_PERSON_HOME", String::new())];
            let refused = run(&home, name, &["-p", "hi"], &env);
            assert_eq!(refused.status.code(), Some(3), "{name}: {}", String::from_utf8_lossy(&refused.stderr));
        }
    }
}
