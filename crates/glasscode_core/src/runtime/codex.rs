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
    args.extend(crate::mcp::codex_flags(&spec.glasscode, &server_env(spec)));
    // `exec`'s options come before its `resume` (as P02 ran it); the prompt is on stdin.
    if let Some(thread) = thread {
        args.extend(["resume".into(), thread.to_string()]);
    }
    args.push("-".into());
    args
}

/// The brain's MCP server environment as it goes on Codex's command line: its signature not
/// there, where any process of the machine can list it, but in a file only this user reads
/// ([`sig_file`], written before each turn), whose path is.
fn server_env(spec: &BrainSpec) -> Vec<(String, String)> {
    spec.brain_env
        .iter()
        .map(|(key, value)| if key == crate::mcp::BRAIN_SIG { (crate::mcp::BRAIN_SIG_FILE.to_string(), sig_file(spec).display().to_string()) } else { (key.clone(), value.clone()) })
        .collect()
}

/// Where a Codex brain's signature is kept for its MCP server: the run folder (`paths::manager_run_dir`).
fn sig_file(spec: &BrainSpec) -> std::path::PathBuf {
    spec.run_dir.join(crate::mcp::brain_sig_file_name(spec.lane, spec.wake.as_deref()))
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

/// What Settings › Manager says of Codex as the manager's brain, and why choosing it is refused,
/// when this Codex shows the brain tools Glasscode cannot switch off.
pub const BRAIN_BLOCKED: &str = "Codex can't be the manager's brain with this Codex (it has tools Glasscode can't switch off).";

/// The Codex versions whose brain keeps tools that no flag turns off. Codex 0.167.0 keeps its five
/// `multi_agent_v1__*` sub-agent tools under `--disable multi_agent`, `-c features.multi_agent=false`,
/// `--disable multi_agent_v2`, `--disable collaboration_modes` and `--disable
/// deferred_tool_world_state` (`--disable multi_agent_v1` is no feature): the live checks of
/// 2026-11-20 and 21, `ALL_TOOLS` read in a live run.
const BLOCKED_VERSIONS: &[&str] = &["0.167.0"];

/// Whether this Codex (its version as `codex --version` says it, "codex-cli 0.167.0" or "0.167.0")
/// is known to keep tools the brain's check refuses: it fails closed at its first question, so
/// Settings says so before the person picks it.
pub fn brain_blocked(version: Option<&str>) -> bool {
    version.and_then(|v| v.split_whitespace().last()).is_some_and(|v| BLOCKED_VERSIONS.contains(&v))
}

/// The tools Codex's code mode shows the model besides Glasscode's: `exec` (a JavaScript isolate
/// with no file system or network of its own), `wait`, `request_user_input`, and `apply_patch` and
/// `view_image`, which the read-only sandbox keeps from writing (P02); and Codex's readers of MCP
/// resources, there whenever a server is configured and no feature turns them off (P02's table):
/// the brain ignores the account's config, so its only server is Glasscode's, which serves no
/// resource (it answers `resources/list` with "no such method").
pub const CODE_MODE_TOOLS: &[&str] = &["exec", "wait", "request_user_input", "apply_patch", "view_image", "list_mcp_resources", "list_mcp_resource_templates", "read_mcp_resource"];

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
        // The MCP server's signature, in its file (mode 0600) before the turn starts it.
        if let Some((_, sig)) = self.spec.brain_env.iter().find(|(key, _)| key == crate::mcp::BRAIN_SIG) {
            let name = crate::mcp::brain_sig_file_name(self.spec.lane, self.spec.wake.as_deref());
            super::driver::private_file(&self.spec.run_dir, &name, sig)?;
        }
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
            &["--disable", "goals"],
            &["--disable", "image_generation"],
            &["-c", r#"web_search="disabled""#],
            &["-c", "check_for_update_on_startup=false"],
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

    /// The brain's signature is the manager's identity (`mcp::brain_caller`): it never goes on
    /// Codex's command line, which any process of the machine can list (`ps`), but into a file of
    /// the run folder only this user reads, whose path the MCP server reads it from.
    #[test]
    fn the_codex_brains_signature_is_not_on_its_command_line() {
        let spec = spec();
        let sig = spec.brain_env.iter().find(|(k, _)| k == crate::mcp::BRAIN_SIG).map(|(_, v)| v.clone()).unwrap();
        for thread in [None, Some("t-1")] {
            let argv = args(&spec, std::path::Path::new("/g/manager/run/i.md"), thread);
            assert!(!argv.iter().any(|a| a.contains(&sig)), "the signature is on the command line: {argv:?}");
            let file = format!("mcp_servers.glasscode.env.{}={}", crate::mcp::BRAIN_SIG_FILE, serde_json::to_string(&sig_file(&spec).display().to_string()).unwrap());
            assert!(argv.contains(&file), "{argv:?}");
        }
        // The server reads it from there, as it would from its environment.
        let run = tempfile::tempdir().unwrap();
        let spec = BrainSpec { run_dir: run.path().into(), ..spec };
        let path = super::super::driver::private_file(&spec.run_dir, &crate::mcp::brain_sig_file_name(spec.lane, None), &sig).unwrap();
        assert_eq!(path, sig_file(&spec));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let env: std::collections::HashMap<String, String> = server_env(&spec).into_iter().collect();
        let brain = crate::mcp::Brain::from_env(|name| env.get(name).cloned()).unwrap();
        assert_eq!((brain.lane, brain.sig), (Lane::Chat, sig));
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
        // A program that is not there: were the folder let through, nothing real would start.
        let mut driver = CodexDriver::new(BrainSpec { cwd: repo.path().into(), run_dir: run.path().into(), program: run.path().join("no-codex"), ..spec() });
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
    fn a_codex_known_to_keep_agent_tools_cannot_be_the_brain() {
        assert!(brain_blocked(Some("0.167.0")) && brain_blocked(Some("codex-cli 0.167.0")));
        assert!(!brain_blocked(Some("0.171.0")) && !brain_blocked(None), "a version not known to keep them is checked at its first question");
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
        // Codex 0.167's own tools under the brain's flags (the live checks, 2026-11-20): the MCP
        // resource readers pass; its sub-agent tools, which no flag turns off, do not.
        let live: Vec<String> = ["apply_patch", "list_mcp_resource_templates", "list_mcp_resources", "read_mcp_resource", "view_image", "multi_agent_v1__spawn_agent", "multi_agent_v1__send_input"].map(String::from).to_vec();
        let mut with_ours = live.clone();
        with_ours.extend(glasscode.iter().map(|g| format!("mcp__glasscode__{g}")));
        assert_eq!(check_all_tools(&with_ours, &glasscode), Err((vec!["multi_agent_v1__spawn_agent".into(), "multi_agent_v1__send_input".into()], vec![])));
        let without_agents: Vec<String> = with_ours.into_iter().filter(|t| !t.starts_with("multi_agent")).collect();
        assert_eq!(check_all_tools(&without_agents, &glasscode), Ok(()));
    }
}
