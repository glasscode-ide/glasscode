//! The Codex brain (M50 step 3; D4, D14, P02): `codex exec --json` for a new conversation and
//! `codex exec … resume <thread>` for each turn after, one process a turn, the prompt on stdin. Its
//! profile turns off the shell, apps, plugins, multi-agent work, memories, the browser and
//! computer use and the person's skills, runs in the read-only sandbox with the network off,
//! ignores the person's config and rules, and has Glasscode's MCP server approved by Glasscode's
//! own gate and required (brain-m2). `apply_patch` and `view_image` stay visible in code mode and
//! cannot write (the P02 fallback); the brain's whole tool list is read from `ALL_TOOLS` at its
//! first start and checked ([`check_all_tools`]). On exec, Stop can only end the process
//! (brain-11). App-server waits for its conformance suite (D14).

use std::time::Duration;

use serde_json::Value;

use super::budget::Usage;
use super::driver::{BrainDriver, BrainSpec, Proc, TurnEnd, Update};

/// `codex`'s arguments for a brain turn (a golden test holds every one). `thread`: the
/// conversation to resume; none for its first turn.
pub fn args(spec: &BrainSpec, instructions: &std::path::Path, thread: Option<&str>) -> Vec<String> {
    // The headless profile (M23, D4, P02): `exec` and its walls.
    let mut args = super::driver::profile_of(spec).codex_args;
    let mut set = |key: &str, value: String| args.extend(["-c".to_string(), format!("{key}={value}")]);
    set("model_instructions_file", serde_json::to_string(&instructions.display().to_string()).unwrap_or_default());
    if let Some(effort) = &spec.effort {
        set("model_reasoning_effort", serde_json::to_string(effort).unwrap_or_default());
    }
    args.extend(["-m".into(), spec.model.clone()]);
    args.extend(crate::mcp::codex_flags(&spec.glasscode, &spec.brain_env));
    // `exec`'s options come before its `resume` (as P02 ran it); the prompt is on stdin.
    if let Some(thread) = thread {
        args.extend(["resume".into(), thread.to_string()]);
    }
    args.push("-".into());
    args
}

/// The updates one line of `codex exec --json` gives.
pub fn parse_line(line: &str, said: &mut String) -> Vec<Update> {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else { return Vec::new() };
    let item = &value["item"];
    match value["type"].as_str() {
        Some("thread.started") => value["thread_id"].as_str().map(|id| vec![Update::Session { id: id.into() }]).unwrap_or_default(),
        Some("item.completed") if item["type"] == "agent_message" => {
            let text = item["text"].as_str().unwrap_or_default().to_string();
            *said = text.clone();
            vec![Update::Text(text)]
        }
        Some("item.started") if item["type"] == "mcp_tool_call" => {
            let name = format!("{}__{}", item["server"].as_str().unwrap_or_default(), item["tool"].as_str().unwrap_or_default());
            vec![Update::ToolCall { name }]
        }
        Some("item.started") if matches!(item["type"].as_str(), Some("command_execution" | "file_change" | "web_search")) => vec![Update::ToolCall { name: item["type"].as_str().unwrap_or_default().into() }],
        Some("turn.completed") => {
            let text = said.trim().to_string();
            let end = if text.is_empty() { TurnEnd::Failed("it finished without saying anything".into()) } else { TurnEnd::Answered(text) };
            vec![Update::Usage(Usage::from_codex(&value["usage"])), Update::Ended(end)]
        }
        Some("turn.failed") => vec![Update::Ended(TurnEnd::Failed(value["error"]["message"].as_str().unwrap_or("the turn failed").into()))],
        Some("error") => vec![Update::Ended(TurnEnd::Failed(value["message"].as_str().unwrap_or("an error").into()))],
        _ => Vec::new(),
    }
}

/// The tools Codex's code mode shows the model besides Glasscode's: `exec` (a JavaScript isolate
/// with no file system or network of its own), `wait`, `request_user_input`, and `apply_patch` and
/// `view_image`, which the read-only sandbox keeps from writing (P02).
pub const CODE_MODE_TOOLS: &[&str] = &["exec", "wait", "request_user_input", "apply_patch", "view_image"];

/// What the brain's first turn asks, to read its whole tool list (prompt-input leaves out the
/// deferred ones, P02).
pub const ALL_TOOLS_PROMPT: &str = "Run exec once with exactly this code and nothing else: text(JSON.stringify(ALL_TOOLS.map(t => t.name))). Then answer with exactly what it printed, and nothing else.";

/// The tool names in an answer to [`ALL_TOOLS_PROMPT`] (a JSON list, maybe in a code block).
pub fn all_tools_of(answer: &str) -> Option<Vec<String>> {
    let start = answer.find('[')?;
    let end = answer.rfind(']')?;
    serde_json::from_str::<Vec<String>>(&answer[start..=end]).ok()
}

/// The check of a Codex brain's tool list (D4): Glasscode's tools (whatever prefix Codex gives
/// them) and the code-mode tools of [`CODE_MODE_TOOLS`]; anything else (a shell, an agent
/// spawner, a connector) is extra. `glasscode`: the names of Glasscode's tools.
pub fn check_all_tools(tools: &[String], glasscode: &[String]) -> Result<(), (Vec<String>, Vec<String>)> {
    let ours = |name: &str| glasscode.iter().any(|g| name == g || name.ends_with(&format!("__{g}")) || name.ends_with(&format!(".{g}")));
    let extra: Vec<String> = tools.iter().filter(|t| !CODE_MODE_TOOLS.contains(&t.as_str()) && !ours(t)).cloned().collect();
    let missing: Vec<String> = glasscode.iter().filter(|g| !tools.iter().any(|t| t == *g || t.ends_with(&format!("__{g}")) || t.ends_with(&format!(".{g}")))).cloned().collect();
    if extra.is_empty() && missing.is_empty() { Ok(()) } else { Err((extra, missing)) }
}

/// The Codex brain driver: a process a turn.
pub struct CodexDriver {
    spec: BrainSpec,
    proc: Option<Proc>,
    thread: Option<String>,
    said: String,
    stopped: bool,
    pending: std::collections::VecDeque<Update>,
}

impl CodexDriver {
    pub fn new(spec: BrainSpec) -> CodexDriver {
        let thread = spec.resume.clone();
        CodexDriver { spec, proc: None, thread, said: String::new(), stopped: false, pending: Default::default() }
    }

    fn instructions(&self) -> Result<std::path::PathBuf, String> {
        let lane = if self.spec.lane == crate::caller::Lane::Chat { "chat" } else { "wake" };
        super::driver::private_file(&self.spec.run_dir, &format!("codex-{lane}-instructions.md"), &self.spec.instructions)
    }
}

impl BrainDriver for CodexDriver {
    fn new_session(&mut self) -> Result<(), String> {
        // A turn is a process: nothing runs between turns.
        Ok(())
    }

    fn prompt(&mut self, text: &str) -> Result<(), String> {
        let instructions = self.instructions()?;
        let cmd = super::driver::command(&self.spec, args(&self.spec, &instructions, self.thread.as_deref()))?;
        let mut proc = Proc::spawn(cmd)?;
        proc.send(text)?;
        // The end of the prompt.
        proc.stdin = None;
        self.proc = Some(proc);
        self.said.clear();
        self.stopped = false;
        Ok(())
    }

    fn next(&mut self, wait: Duration) -> Option<Update> {
        if let Some(update) = self.pending.pop_front() {
            return Some(update);
        }
        let proc = self.proc.as_mut()?;
        match proc.lines.recv_timeout(wait) {
            Ok(line) => {
                for update in parse_line(&line, &mut self.said) {
                    if let Update::Session { id } = &update {
                        self.thread = Some(id.clone());
                    }
                    self.pending.push_back(update);
                }
                self.pending.pop_front()
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let why = proc.stderr_tail();
                self.proc = None;
                Some(Update::Ended(if self.stopped { TurnEnd::Stopped } else { TurnEnd::Failed(format!("it stopped ({})", why.lines().last().unwrap_or("no word"))) }))
            }
        }
    }

    fn cancel(&mut self) {
        // `exec` has no interrupt: Stop ends the process.
        self.stopped = true;
        if let Some(proc) = self.proc.as_mut() {
            proc.end();
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
        self.thread.clone()
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
            agent: AgentKind::Codex,
            lane: Lane::Chat,
            wake: None,
            program: "codex".into(),
            glasscode: "/Applications/Glasscode.app/Contents/MacOS/Glasscode".into(),
            model: "gpt-5.6-luna".into(),
            effort: Some("low".into()),
            fallback_model: None,
            cwd: "/g/manager/work".into(),
            run_dir: "/g/manager/run".into(),
            env: Default::default(),
            instructions: "You are the manager.".into(),
            brain_env: crate::mcp::brain_env(Lane::Chat, None, &[1u8; 32]),
            resume: None,
        }
    }

    /// The P02 flags, D4's additions and the MCP server's, on the first turn and on resume.
    #[test]
    fn codex_flags_in_probe() {
        let first = args(&spec(), std::path::Path::new("/g/manager/run/i.md"), None);
        let has = |args: &[String], pair: &[&str]| args.windows(pair.len()).any(|w| w.iter().zip(pair).all(|(a, b)| a == b));
        for pair in [
            &["exec", "--json"][..],
            &["-s", "read-only"],
            &["--ignore-user-config"],
            &["--ignore-rules"],
            &["--disable", "shell_tool"],
            &["--disable", "apps"],
            &["--disable", "plugins"],
            &["--disable", "multi_agent"],
            &["--disable", "memories"],
            &["--disable", "browser_use"],
            &["--disable", "computer_use"],
            &["-c", "skills.include_instructions=false"],
            &["-c", "skills.bundled.enabled=false"],
            &["-c", "sandbox_workspace_write.network_access=false"],
            &["-c", "notify=[]"],
            &["-c", "mcp_servers.glasscode.required=true"],
            &["-c", r#"mcp_servers.glasscode.default_tools_approval_mode="approve""#],
            &["-m", "gpt-5.6-luna"],
        ] {
            assert!(has(&first, pair), "{pair:?} in {first:?}");
        }
        assert_eq!(first.last().map(String::as_str), Some("-"), "the prompt on stdin");
        assert!(!first.iter().any(|a| a.contains("You are the manager.") || a.contains("workspace-write") || a.contains("danger")));
        // `exec`'s options first, then `resume <thread>` and the prompt (P02's order).
        let resumed = args(&spec(), std::path::Path::new("/g/manager/run/i.md"), Some("t-1"));
        assert_eq!((resumed[0].as_str(), &resumed[resumed.len() - 3..]), ("exec", &["resume".to_string(), "t-1".to_string(), "-".to_string()][..]));
        assert_eq!(resumed.iter().filter(|a| *a == "resume").count(), 1);
        assert!(!first.contains(&"--ephemeral".to_string()), "the chat's thread is kept, to resume");
    }

    /// exec-24 for Codex: every turn's process runs in the manager's work folder, read-only, and a
    /// repository as its folder is refused before it starts.
    #[test]
    fn the_codex_brain_spawns_only_in_its_work_folder() {
        crate::paths::use_test_root();
        let work = crate::paths::manager_work_dir();
        let at_work = BrainSpec { cwd: work.clone(), ..spec() };
        let cmd = super::super::driver::command(&at_work, args(&at_work, std::path::Path::new("/g/manager/run/i.md"), None)).unwrap();
        assert_eq!(cmd.get_current_dir(), Some(work.as_path()));
        let argv: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(argv.windows(2).any(|w| w == ["-s", "read-only"]) && argv.iter().any(|a| a == "--ignore-user-config"), "{argv:?}");

        let repo = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let mut driver = CodexDriver::new(BrainSpec { cwd: repo.path().into(), run_dir: run.path().into(), ..spec() });
        let refused = driver.prompt("hello").unwrap_err();
        assert!(refused.contains("its own folder"), "{refused}");
        assert!(driver.pid().is_none());
    }

    #[test]
    fn exec_lines_become_updates() {
        let mut said = String::new();
        assert_eq!(parse_line(r#"{"type":"thread.started","thread_id":"t-1"}"#, &mut said), vec![Update::Session { id: "t-1".into() }]);
        assert_eq!(parse_line(r#"{"type":"item.started","item":{"type":"mcp_tool_call","server":"glasscode","tool":"session_list"}}"#, &mut said), vec![Update::ToolCall { name: "glasscode__session_list".into() }]);
        parse_line(r#"{"type":"item.completed","item":{"type":"agent_message","text":"Two sessions run."}}"#, &mut said);
        let end = parse_line(r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":60,"output_tokens":7}}"#, &mut said);
        assert_eq!(end, vec![Update::Usage(Usage { input: 40, output: 7, cache_read: 60, cache_write: 0 }), Update::Ended(TurnEnd::Answered("Two sessions run.".into()))]);
        assert_eq!(parse_line(r#"{"type":"turn.failed","error":{"message":"usage limit reached"}}"#, &mut said), vec![Update::Ended(TurnEnd::Failed("usage limit reached".into()))]);
    }

    #[test]
    fn all_tools_are_checked() {
        let glasscode: Vec<String> = ["ui_state", "session_list"].map(String::from).to_vec();
        let answer = "```json\n[\"exec\",\"wait\",\"apply_patch\",\"view_image\",\"mcp__glasscode__ui_state\",\"mcp__glasscode__session_list\"]\n```";
        let tools = all_tools_of(answer).unwrap();
        assert_eq!(check_all_tools(&tools, &glasscode), Ok(()));
        let mut with_shell = tools.clone();
        with_shell.push("exec_command".into());
        with_shell.push("spawn_agent".into());
        assert_eq!(check_all_tools(&with_shell, &glasscode), Err((vec!["exec_command".into(), "spawn_agent".into()], vec![])));
        assert_eq!(check_all_tools(&tools[..4], &glasscode), Err((vec![], glasscode.clone())), "Glasscode's tools missing");
        assert!(all_tools_of("no list").is_none());
    }
}
