//! The Claude brain (M50 step 2): one long-lived `claude -p` with stream-json in and out, in the
//! profile of D4 and P01 (`profile::headless_profile(BrainChat | BrainWake)`), with no shell and
//! none of the person's own set-up (brain-1, brain-5): `--restricted`, only ToolSearch among the
//! built-in tools, Glasscode's MCP server and no other (`--strict-mcp-config`), its tools allowed
//! and every other refused without asking, no slash commands, no hooks, an explicit model. The MCP
//! config and the settings are files only this user can read (brain-16); the instructions, which
//! are Glasscode's own fixed text with nothing of the person's, go in `--append-system-prompt` as
//! P03 proved it (each launch's, on a resume too); the memory brief and the situation go in the user
//! turn on stdin, never argv. The chat's conversation has its own id and is kept, so a brain that ends is loaded
//! again (`--resume`); a wake's is not kept. Stop is the stream's interrupt request, then SIGINT,
//! then the end of the process (brain-11).

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::budget::Usage;
use super::driver::{BrainDriver, BrainSpec, Proc, TurnEnd, Update};

/// The files a Claude brain is started with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Files {
    pub mcp: std::path::PathBuf,
    pub settings: std::path::PathBuf,
}

/// The brain's own settings (its profile's): no hooks of anyone's run in it, and other sessions
/// cannot send to it.
pub fn settings(spec: &BrainSpec) -> Value {
    super::driver::profile_of(spec).claude_settings
}

/// Write the brain's files (mode 0600) for a lane.
pub fn write_files(spec: &BrainSpec) -> Result<Files, String> {
    let lane = if spec.lane == crate::caller::Lane::Chat { "chat" } else { "wake" };
    let dir = &spec.run_dir;
    let mcp = crate::mcp::claude_config(&spec.glasscode, &spec.brain_env);
    Ok(Files {
        mcp: super::driver::private_file(dir, &format!("claude-{lane}-mcp.json"), &mcp.to_string())?,
        settings: super::driver::private_file(dir, &format!("claude-{lane}-settings.json"), &settings(spec).to_string())?,
    })
}

/// `claude`'s arguments for a brain: its headless profile (the isolation set of D4, P01 and
/// brain-5; a golden test holds every one), then this launch's files, model and conversation.
/// `session`: the chat's conversation id, new (`--session-id`) or loaded (`--resume`); a wake keeps
/// no conversation, so it has none.
pub fn args(spec: &BrainSpec, files: &Files, session: &str, resume: bool) -> Vec<String> {
    let mut args = super::driver::profile_of(spec).claude_args;
    args.extend(["--mcp-config".into(), files.mcp.display().to_string()]);
    args.extend(["--settings".into(), files.settings.display().to_string()]);
    args.extend(["--append-system-prompt".into(), spec.instructions.clone()]);
    args.extend(["--model".into(), spec.model.clone()]);
    if let Some(effort) = &spec.effort {
        args.extend(["--effort".into(), effort.clone()]);
    }
    if let Some(fallback) = &spec.fallback_model {
        args.extend(["--fallback-model".into(), fallback.clone()]);
    }
    if spec.lane == crate::caller::Lane::Chat {
        args.extend([if resume { "--resume" } else { "--session-id" }.to_string(), session.to_string()]);
    }
    args
}

/// A user turn's line.
pub fn user_line(text: &str) -> String {
    json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": text }] } }).to_string()
}

/// The stream's interrupt request.
pub fn interrupt_line(id: &str) -> String {
    json!({ "type": "control_request", "request_id": id, "request": { "subtype": "interrupt" } }).to_string()
}

/// The answer to a permission request: refused, never asked (the brain's tools need none).
pub fn refusal_line(id: &str) -> String {
    let deny = json!({ "behavior": "deny", "message": "Not for the manager: use Glasscode's tools." });
    json!({ "type": "control_response", "response": { "subtype": "success", "request_id": id, "response": deny } }).to_string()
}

/// The updates one line of the stream gives.
pub fn parse_line(line: &str) -> Vec<Update> {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else { return Vec::new() };
    let strings = |v: &Value| -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect()).unwrap_or_default() };
    match value["type"].as_str() {
        Some("system") if value["subtype"] == "init" => {
            let mut out = vec![Update::Init { tools: strings(&value["tools"]), model: value["model"].as_str().unwrap_or_default().into(), version: value["claude_code_version"].as_str().unwrap_or_default().into() }];
            if let Some(id) = value["session_id"].as_str() {
                out.insert(0, Update::Session { id: id.into() });
            }
            out
        }
        // A part of the answer as it streams (`--include-partial-messages`).
        Some("stream_event") if value["parent_tool_use_id"].is_null() => {
            let delta = &value["event"]["delta"];
            match (value["event"]["type"].as_str(), delta["type"].as_str(), delta["text"].as_str()) {
                (Some("content_block_delta"), Some("text_delta"), Some(text)) => vec![Update::Text(text.to_string())],
                _ => Vec::new(),
            }
        }
        // Its own messages only: a sub-agent's carry the tool call they belong to.
        Some("assistant") if value["parent_tool_use_id"].is_null() => value["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| match block["type"].as_str() {
                Some("tool_use") => block["name"].as_str().map(|name| Update::ToolCall { name: name.into() }),
                _ => None,
            })
            .collect(),
        Some("rate_limit_event") => vec![Update::RateLimit(value)],
        Some("control_request") if value["request"]["subtype"] == "can_use_tool" => vec![Update::Permission { id: value["request_id"].as_str().unwrap_or_default().into(), tool: value["request"]["tool_name"].as_str().unwrap_or_default().into() }],
        Some("result") => {
            let usage = Update::Usage(Usage::from_claude(&value["usage"]));
            let subtype = value["subtype"].as_str().unwrap_or_default();
            let text = value["result"].as_str().unwrap_or_default().trim().to_string();
            let error = value["is_error"].as_bool().unwrap_or(false) || subtype.starts_with("error");
            // An interrupted turn ends as an error: the person's Stop, not a failure (brain-11).
            let stopped = value["terminal_reason"] == "aborted_streaming" || subtype == "error_during_execution" && text.contains("interrupt");
            let end = match (error, stopped, text.is_empty()) {
                (_, true, _) => TurnEnd::Stopped,
                (false, _, false) => TurnEnd::Answered(text),
                (false, _, true) => TurnEnd::Failed("it finished without saying anything".into()),
                (true, _, false) => TurnEnd::Failed(text),
                (true, _, true) => TurnEnd::Failed(format!("it stopped with an error ({})", if subtype.is_empty() { "unknown" } else { subtype })),
            };
            vec![usage, Update::Ended(end)]
        }
        _ => Vec::new(),
    }
}

/// The Claude brain driver.
pub struct ClaudeDriver {
    spec: BrainSpec,
    proc: Option<Proc>,
    session: String,
    /// When Stop was asked, and how far it went (0 the request, 1 SIGINT).
    stopping: Option<(Instant, u8)>,
    pending: std::collections::VecDeque<Update>,
    next_request: u64,
}

impl ClaudeDriver {
    pub fn new(spec: BrainSpec) -> ClaudeDriver {
        let session = spec.resume.clone().unwrap_or_else(|| uuid_v4());
        ClaudeDriver { spec, proc: None, session, stopping: None, pending: Default::default(), next_request: 0 }
    }
}

/// A conversation id Claude takes for `--session-id` (a UUID).
fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    crate::sys::random_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", h[0..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..16].concat())
}

impl BrainDriver for ClaudeDriver {
    fn new_session(&mut self) -> Result<(), String> {
        if self.proc.is_some() {
            return Ok(());
        }
        let files = write_files(&self.spec)?;
        let resume = self.spec.resume.as_deref() == Some(self.session.as_str());
        let cmd = super::driver::command(&self.spec, args(&self.spec, &files, &self.session, resume))?;
        self.proc = Some(Proc::spawn(cmd)?);
        // From now on the conversation is one to load again.
        self.spec.resume = Some(self.session.clone());
        Ok(())
    }

    fn prompt(&mut self, text: &str) -> Result<(), String> {
        if self.proc.as_mut().is_some_and(Proc::exited) {
            self.proc = None;
        }
        self.new_session()?;
        self.stopping = None;
        let proc = self.proc.as_mut().ok_or("it did not start")?;
        // What it still wrote after its last answer is not about this turn.
        while proc.lines.try_recv().is_ok() {}
        proc.send(&user_line(text))
    }

    fn next(&mut self, wait: Duration) -> Option<Update> {
        if let Some(update) = self.pending.pop_front() {
            return Some(update);
        }
        let proc = self.proc.as_mut()?;
        // Stop: the request, then SIGINT after 3 s, then the end of the process after 6 s.
        if let Some((at, step)) = self.stopping {
            if step == 0 && at.elapsed() > Duration::from_secs(3) {
                proc.interrupt();
                self.stopping = Some((at, 1));
            } else if at.elapsed() > Duration::from_secs(6) {
                proc.end();
                self.proc = None;
                self.stopping = None;
                return Some(Update::Ended(TurnEnd::Stopped));
            }
        }
        match proc.lines.recv_timeout(wait) {
            Ok(line) => {
                let updates = parse_line(&line);
                // A permission request is answered at once: refused.
                for update in &updates {
                    if let Update::Permission { id, .. } = update {
                        let _ = proc.send(&refusal_line(id));
                    }
                }
                self.pending.extend(updates);
                self.pending.pop_front()
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let why = super::driver::Proc::stderr_tail(proc);
                self.proc = None;
                Some(Update::Ended(if self.stopping.take().is_some() { TurnEnd::Stopped } else { TurnEnd::Failed(format!("it stopped ({})", why.lines().last().unwrap_or("no word"))) }))
            }
        }
    }

    fn cancel(&mut self) {
        if let Some(proc) = self.proc.as_mut() {
            self.next_request += 1;
            let _ = proc.send(&interrupt_line(&format!("stop-{}", self.next_request)));
            self.stopping = Some((Instant::now(), 0));
        }
    }

    fn stop(&mut self) {
        if let Some(mut proc) = self.proc.take() {
            proc.end();
        }
    }

    fn pid(&self) -> Option<u32> {
        self.proc.as_ref().map(|p| p.child.id())
    }

    fn session_id(&self) -> Option<String> {
        Some(self.session.clone())
    }

    fn stderr_tail(&self) -> String {
        self.proc.as_ref().map(Proc::stderr_tail).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caller::Lane;
    use crate::model::AgentKind;

    fn spec() -> BrainSpec {
        BrainSpec {
            agent: AgentKind::Claude,
            lane: Lane::Chat,
            wake: None,
            program: "claude".into(),
            glasscode: "/Applications/Glasscode.app/Contents/MacOS/Glasscode".into(),
            model: "claude-haiku-5-5".into(),
            effort: Some("low".into()),
            fallback_model: None,
            cwd: "/g/manager/work".into(),
            run_dir: "/g/manager/run".into(),
            env: Default::default(),
            instructions: "You are the manager.".into(),
            brain_env: vec![],
            resume: None,
        }
    }

    fn files() -> Files {
        Files { mcp: "/g/manager/run/mcp.json".into(), settings: "/g/manager/run/s.json".into() }
    }

    /// Every argument that keeps the brain to Glasscode's tools is there (D4, P01, brain-5), and
    /// none that opens a shell or the person's set-up.
    #[test]
    fn the_brain_profile_is_the_isolation_set() {
        let args = args(&spec(), &files(), "abc", false);
        let has = |pair: &[&str]| args.windows(pair.len()).any(|w| w.iter().zip(pair).all(|(a, b)| a == b));
        for pair in [&["--restricted"][..], &["--tools", "ToolSearch"], &["--strict-mcp-config"], &["--allowedTools", "mcp__glasscode__*"], &["--permission-mode", "dontAsk"], &["--permission-prompts", "none"], &["--disable-slash-commands"], &["--mcp-config", "/g/manager/run/mcp.json"], &["--settings", "/g/manager/run/s.json"], &["--append-system-prompt", "You are the manager."], &["--model", "claude-haiku-5-5"], &["--session-id", "abc"], &["--include-partial-messages"]] {
            assert!(has(pair), "{pair:?} in {args:?}");
        }
        // Every flag is one `claude --help` lists (the probe's check, alt-m4): no file form it does not.
        for never in ["Bash", "--dangerously-skip-permissions", "--no-session-persistence", "--append-system-prompt-file"] {
            assert!(!args.iter().any(|a| a.contains(never)), "{never}");
        }
        assert!(super::super::driver::tests::only_glasscode(&["ToolSearch".into()]).is_ok());
        let resumed = super::args(&spec(), &files(), "abc", true);
        assert!(resumed.windows(2).any(|w| w == ["--resume", "abc"]));
        assert_eq!((settings(&spec())["disableAllHooks"].clone(), settings(&spec())["crossSessionInbound"].clone()), (json!(true), json!("refuse")));
        // A wake keeps no conversation: no id, nothing to load again.
        let wake = BrainSpec { lane: crate::caller::Lane::Wake, wake: Some("w1".into()), ..spec() };
        let args = super::args(&wake, &files(), "abc", false);
        assert!(args.contains(&"--no-session-persistence".to_string()) && !args.iter().any(|a| a == "--session-id" || a == "--resume"), "{args:?}");
    }

    /// exec-24: the brain's process runs in the manager's work folder, with the flags that keep
    /// it to Glasscode's tools and set its permission mode itself; a repository as its folder is
    /// refused before anything starts (its `.claude/settings.json` hooks never run).
    #[test]
    fn the_brain_spawns_only_in_its_work_folder() {
        crate::paths::use_test_root();
        let work = crate::paths::manager_work_dir();
        let at_work = BrainSpec { cwd: work.clone(), ..spec() };
        let cmd = super::super::driver::command(&at_work, args(&at_work, &files(), "abc", false)).unwrap();
        assert_eq!(cmd.get_current_dir(), Some(work.as_path()));
        let argv: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let has = |pair: &[&str]| argv.windows(pair.len()).any(|w| w.iter().zip(pair).all(|(a, b)| a == b));
        for pair in [&["--permission-mode", "dontAsk"][..], &["--strict-mcp-config"], &["--settings", "/g/manager/run/s.json"], &["--tools", "ToolSearch"]] {
            assert!(has(pair), "{pair:?} in {argv:?}");
        }

        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".claude")).unwrap();
        std::fs::write(repo.path().join(".claude/settings.json"), r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"touch ran"}]}]}}"#).unwrap();
        let run = tempfile::tempdir().unwrap();
        // A program that is not there: were the folder let through, nothing real would start.
        let mut driver = ClaudeDriver::new(BrainSpec { cwd: repo.path().into(), run_dir: run.path().into(), program: run.path().join("no-claude"), ..spec() });
        let refused = driver.new_session().unwrap_err();
        assert!(refused.contains("its own folder"), "{refused}");
        assert!(driver.pid().is_none());
        // A folder inside the work folder is not the work folder either.
        assert!(super::super::driver::brain_folder(&work.join("repo")).is_err());
    }

    /// P01's init lines: only Glasscode's tools (and ToolSearch) is a pass; a built-in tool is not.
    #[test]
    fn brain_tool_list_is_exactly_glasscode() {
        let recorded = r#"{"type":"system","subtype":"init","cwd":"/g/manager/work","session_id":"5d1c","tools":["ToolSearch","mcp__glasscode__note","mcp__glasscode__status"],"mcp_servers":[{"name":"glasscode","status":"connected"}],"model":"claude-haiku-5-5","permissionMode":"dontAsk","slash_commands":[],"apiKeySource":"none","claude_code_version":"2.1.328"}"#;
        let updates = parse_line(recorded);
        assert_eq!(updates[0], Update::Session { id: "5d1c".into() });
        let Update::Init { tools, version, .. } = &updates[1] else { panic!("{updates:?}") };
        assert_eq!(version, "2.1.328");
        assert!(super::super::driver::tests::only_glasscode(tools).is_ok());
        let ordinary = r#"{"type":"system","subtype":"init","tools":["Task","Bash","Read","mcp__glasscode__status"]}"#;
        let Update::Init { tools, .. } = &parse_line(ordinary)[0] else { panic!() };
        assert_eq!(super::super::driver::tests::only_glasscode(tools), Err((vec!["Task".into(), "Bash".into(), "Read".into()], vec![])));
    }

    #[test]
    fn stream_lines_become_updates() {
        assert_eq!(parse_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Two "}},"parent_tool_use_id":null}"#), vec![Update::Text("Two ".into())]);
        assert_eq!(parse_line(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__glasscode__session_list","input":{}}]},"parent_tool_use_id":null}"#), vec![Update::ToolCall { name: "mcp__glasscode__session_list".into() }]);
        let result = parse_line(r#"{"type":"result","subtype":"success","is_error":false,"result":"Two sessions run.","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100}}"#);
        assert_eq!(result, vec![Update::Usage(Usage { input: 10, output: 5, cache_read: 100, cache_write: 0 }), Update::Ended(TurnEnd::Answered("Two sessions run.".into()))]);
        let stopped = parse_line(r#"{"type":"result","subtype":"error_during_execution","is_error":true,"terminal_reason":"aborted_streaming","result":""}"#);
        assert_eq!(stopped[1], Update::Ended(TurnEnd::Stopped), "an interrupt is the person's Stop");
        assert_eq!(parse_line(r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash"}}"#), vec![Update::Permission { id: "r1".into(), tool: "Bash".into() }]);
        assert!(refusal_line("r1").contains("\"behavior\":\"deny\""));
        assert!(interrupt_line("s1").contains("\"subtype\":\"interrupt\""));
        assert!(matches!(&parse_line(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#)[0], Update::RateLimit(_)));
        assert!(parse_line("not json").is_empty());
    }

    #[test]
    fn a_conversation_id_is_a_uuid() {
        let id = uuid_v4();
        assert_eq!((id.len(), id.matches('-').count(), &id[14..15]), (36, 4, "4"));
    }
}
