//! The agent that thinks for the manager: Claude Code or Codex, run without a terminal, with
//! the manager's instructions, its memory and the state of the sessions. It acts through the
//! `glasscode` command (which has `glasscode manager …` for the manager's own things), so
//! everything it does passes Settings › Automation.
//!
//! Claude is one long-lived process per `Brain` (`claude -p` with JSON lines in and out): a
//! question is one line on its stdin, the answer is the `result` line on its stdout, and it
//! remembers the conversation. Codex is started once per question (`codex exec`), with the last
//! few exchanges in front. Either way a question is answered within two minutes or the agent
//! is stopped.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Data, FollowState, MsgState, SessionInfo, SessionState, Settings, Snapshot};
use crate::model::AgentKind;

/// How to run it: the account's login folder, the environment sessions get, and where the
/// `glasscode` launcher is.
#[derive(Clone, Debug, Default)]
pub struct BrainConfig {
    pub config_dir: Option<PathBuf>,
    pub env: HashMap<String, String>,
    /// The folder it runs in (`~/.glasscode/manager/work`).
    pub cwd: PathBuf,
}

/// How long one question may take.
const ASK_TIMEOUT: Duration = Duration::from_secs(120);

/// Exchanges kept to remind a fresh agent of the conversation.
const HISTORY: usize = 6;

/// The whole `context()` block, in characters.
const CONTEXT_BUDGET: usize = 6000;

/// What Claude may use without anyone being asked. Everything else is denied.
pub const ALLOWED_TOOLS: &str = "Bash(glasscode:*),Bash(gh:*),Bash(git:*),Read,Grep,Glob";

/// The built-in tools it has at all.
const TOOLS: &str = "Bash,Read,Grep,Glob";

/// `gh` and `git` are for reading: what changes a pull request, a repository or a checkout is
/// refused even though `gh` and `git` are allowed (a deny rule wins over an allow rule).
const DENIED_TOOLS: &[&str] = &[
    "gh pr merge", "gh pr review", "gh pr close", "gh pr comment", "gh pr create", "gh pr edit", "gh pr ready", "gh pr reopen", "gh pr lock", "gh pr unlock",
    "gh issue comment", "gh issue create", "gh issue close", "gh issue edit", "gh issue delete", "gh issue reopen", "gh issue transfer", "gh issue lock", "gh issue pin",
    "gh api", "gh release", "gh workflow", "gh run rerun", "gh run cancel", "gh run delete", "gh label", "gh secret", "gh variable", "gh auth", "gh gist", "gh ssh-key", "gh gpg-key",
    "gh repo delete", "gh repo create", "gh repo edit", "gh repo fork", "gh repo rename", "gh repo archive", "gh repo sync", "gh repo deploy-key",
    "git push", "git commit", "git reset", "git checkout", "git switch", "git merge", "git rebase", "git clean", "git stash", "git restore", "git rm", "git mv", "git add", "git tag",
    "git config", "git worktree", "git cherry-pick", "git revert", "git pull", "git apply", "git am",
];

fn denied_tools() -> String {
    DENIED_TOOLS.iter().map(|d| format!("Bash({d}:*)")).collect::<Vec<_>>().join(",")
}

// ----- the agents that are running ------------------------------------------------------------

/// Process ids of the agents thinking for a manager right now (the app tells a `glasscode`
/// command from the manager's own agent by these: see `pids`).
static RUNNING: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// The agents thinking for a manager right now, as process ids. A `glasscode` command that
/// runs under one of them comes from the manager itself. Never blocks on a `Brain`.
pub fn pids() -> Vec<u32> {
    RUNNING.lock().map(|r| r.clone()).unwrap_or_default()
}

fn register(pid: u32) {
    if let Ok(mut running) = RUNNING.lock() {
        running.push(pid);
    }
}

fn unregister(pid: u32) {
    if let Ok(mut running) = RUNNING.lock() {
        running.retain(|p| *p != pid);
    }
}

/// Stop an agent and everything it started (it runs in a process group of its own).
fn kill(child: &mut Child) {
    let pid = child.id();
    // SAFETY: killpg only sends a signal; the group is the one this child was started in.
    #[cfg(unix)]
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
    // Windows has no process groups: end what the agent started, then the agent.
    #[cfg(windows)]
    let _ = crate::sys::end_tree(pid);
    let _ = child.kill();
    let _ = child.wait();
    unregister(pid);
}

// ----- one conversation -----------------------------------------------------------------------

struct Exchange {
    question: String,
    answer: String,
}

/// The long-lived Claude process.
struct Proc {
    child: Child,
    stdin: ChildStdin,
    /// Its stdout, line by line (the channel closes when it exits).
    lines: mpsc::Receiver<String>,
    /// The end of its stderr, for saying why it stopped.
    errors: Arc<Mutex<String>>,
    /// It wrote at least one line: the command line was understood.
    spoke: bool,
}

/// Why a question got no answer.
enum Failure {
    /// The agent answered with an error (not signed in, out of turns): asking again will not help.
    Said(String),
    TimedOut,
    /// The process died or its stream broke. `spoke`: it had been talking before.
    Broken { spoke: bool, why: String },
    /// The program could not be started at all (not installed, no folder to run in).
    NoStart(String),
}

/// One conversation with the agent, kept alive between questions so it answers fast and
/// remembers what was just said.
pub struct Brain {
    agent: AgentKind,
    name: String,
    model: Option<String>,
    prompt: String,
    config: BrainConfig,
    proc: Option<Proc>,
    /// This `claude` does not take JSON lines on stdin: one process per question instead.
    one_shot: bool,
    history: VecDeque<Exchange>,
    timeout: Duration,
    /// The agent answering a one-off question right now.
    running: Option<u32>,
}

impl Brain {
    pub fn new(settings: &Settings, config: BrainConfig) -> Brain {
        Brain {
            agent: settings.brain,
            name: settings.name.clone(),
            model: settings.model.clone().filter(|m| !m.trim().is_empty()),
            prompt: system_prompt(settings),
            config,
            proc: None,
            one_shot: false,
            history: VecDeque::new(),
            timeout: ASK_TIMEOUT,
            running: None,
        }
    }

    /// Ask it something and wait for what it says (blocking: call it off the main thread).
    /// `context` is `context()` below; `for_voice` asks for a short spoken answer.
    pub fn ask(&mut self, question: &str, context: &str, for_voice: bool) -> anyhow::Result<String> {
        let body = message(question, context, for_voice);
        let deadline = Instant::now() + self.timeout;
        let result = match self.agent {
            AgentKind::Claude if !self.one_shot => self.ask_stream(&body, deadline),
            AgentKind::Claude => self.ask_claude_once(&body, deadline),
            AgentKind::Codex => self.ask_codex(&body, deadline),
            AgentKind::Shell => Err(Failure::Said(format!("{} needs Claude or Codex to think. Choose one in Settings › Manager.", self.name))),
        };
        match result {
            Ok(say) => {
                self.history.push_back(Exchange { question: clip(question.trim(), 400), answer: clip(&say, 600) });
                while self.history.len() > HISTORY {
                    self.history.pop_front();
                }
                Ok(say)
            }
            Err(Failure::Said(text)) => anyhow::bail!("{text}"),
            Err(Failure::TimedOut) => {
                self.stop();
                anyhow::bail!("{} took longer than {} to answer, so its agent was stopped. Ask again.", self.name, minutes(self.timeout))
            }
            Err(Failure::Broken { why, .. }) => {
                self.stop();
                anyhow::bail!("{}'s agent ({}) stopped without an answer: {why}", self.name, self.agent.name())
            }
            Err(Failure::NoStart(why)) => anyhow::bail!("{}'s agent ({}) could not be started: {why}", self.name, self.agent.name()),
        }
    }

    /// Stop the agent (the next `ask` starts a fresh one).
    pub fn stop(&mut self) {
        if let Some(mut proc) = self.proc.take() {
            kill(&mut proc.child);
        }
    }

    /// The process id of the agent while it runs (see `pids`, which does not need the `Brain`).
    pub fn pid(&self) -> Option<u32> {
        self.proc.as_ref().map(|p| p.child.id()).or(self.running)
    }

    // ----- Claude, kept alive ---------------------------------------------------------------

    fn ask_stream(&mut self, body: &str, deadline: Instant) -> Result<String, Failure> {
        let mut last = (false, String::new());
        // The process may have died since the last question: start a fresh one once.
        for _ in 0..2 {
            match self.stream_turn(body, deadline) {
                Err(Failure::Broken { spoke, why }) => {
                    self.stop();
                    last = (spoke, why);
                }
                other => return other,
            }
        }
        let (spoke, why) = last;
        if spoke {
            return Err(Failure::Broken { spoke, why });
        }
        // It never got as far as one line, twice: most likely a `claude` that does not know
        // these options. One process per question works with every version.
        log::warn!("the manager's agent does not start with JSON input ({why}); asking one question at a time");
        self.one_shot = true;
        self.ask_claude_once(body, deadline)
    }

    /// One question to the running process (started when there is none) and its answer.
    fn stream_turn(&mut self, body: &str, deadline: Instant) -> Result<String, Failure> {
        let fresh = self.proc.is_none();
        if fresh {
            self.proc = Some(self.start_stream().map_err(Failure::NoStart)?);
        }
        // A fresh process knows nothing of what was said before.
        let text = if fresh { with_history(&self.history, body) } else { body.to_string() };
        let Some(proc) = self.proc.as_mut() else { return Err(Failure::NoStart("it did not start".into())) };
        // Whatever it still wrote after its last answer is not about this question.
        while proc.lines.try_recv().is_ok() {}
        send(proc, &user_line(&text))?;
        let mut turn = Turn::default();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Failure::TimedOut);
            }
            match proc.lines.recv_timeout(left) {
                Ok(line) => {
                    proc.spoke = true;
                    match turn.feed(&line) {
                        Step::More => {}
                        Step::Reply(out) => send(proc, &out)?,
                        Step::Done(result) => return result.map_err(Failure::Said),
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return Err(Failure::TimedOut),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Failure::Broken { spoke: proc.spoke, why: why_it_stopped(proc) }),
            }
        }
    }

    fn start_stream(&self) -> Result<Proc, String> {
        let mut cmd = self.command("claude", claude_args(&self.prompt, self.model.as_deref(), true))?;
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("could not start claude ({e})"))?;
        register(child.id());
        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
            kill(&mut child);
            return Err("could not talk to claude".into());
        };
        let (tx, lines) = mpsc::channel::<String>();
        let _ = std::thread::Builder::new().name("manager-brain-out".into()).spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let errors = Arc::new(Mutex::new(String::new()));
        let tail = errors.clone();
        let _ = std::thread::Builder::new().name("manager-brain-err".into()).spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if let Ok(mut tail) = tail.lock() {
                    tail.push_str(&line);
                    tail.push('\n');
                    if tail.len() > 4000 {
                        let cut = tail.len() - 2000;
                        let cut = (cut..tail.len()).find(|i| tail.is_char_boundary(*i)).unwrap_or(tail.len());
                        tail.drain(..cut);
                    }
                }
            }
        });
        Ok(Proc { child, stdin, lines, errors, spoke: false })
    }

    // ----- one process per question ---------------------------------------------------------

    fn ask_claude_once(&mut self, body: &str, deadline: Instant) -> Result<String, Failure> {
        let cmd = self.command("claude", claude_args(&self.prompt, self.model.as_deref(), false)).map_err(Failure::NoStart)?;
        let (ok, out, err) = self.run_once(cmd, &with_history(&self.history, body), deadline)?;
        let out = out.trim();
        if ok && !out.is_empty() {
            return Ok(out.to_string());
        }
        let why = [out, err.trim()].into_iter().find(|t| !t.is_empty()).map(|t| clip(last_lines(t, 3).trim(), 400)).unwrap_or_else(|| "it said nothing".into());
        Err(Failure::Broken { spoke: true, why })
    }

    fn ask_codex(&mut self, body: &str, deadline: Instant) -> Result<String, Failure> {
        // Codex writes its last message to a file, so progress on stdout never mixes in.
        let answer = tempfile::Builder::new().prefix("manager-answer-").suffix(".txt").tempfile().map_err(|e| Failure::NoStart(format!("no temporary file ({e})")))?;
        let args = codex_args(&self.prompt, self.model.as_deref(), &answer.path().to_string_lossy());
        let cmd = self.command("codex", args).map_err(Failure::NoStart)?;
        let (ok, out, err) = self.run_once(cmd, &with_history(&self.history, body), deadline)?;
        let said = std::fs::read_to_string(answer.path()).unwrap_or_default();
        let said = if said.trim().is_empty() { out.trim() } else { said.trim() };
        if ok && !said.is_empty() {
            return Ok(said.to_string());
        }
        let why = [err.trim(), out.trim()].into_iter().find(|t| !t.is_empty()).map(|t| clip(last_lines(t, 3).trim(), 400)).unwrap_or_else(|| "it said nothing".into());
        Err(Failure::Broken { spoke: true, why })
    }

    /// Run a command to its end with `input` on its stdin: (it succeeded, stdout, stderr).
    fn run_once(&mut self, mut cmd: Command, input: &str, deadline: Instant) -> Result<(bool, String, String), Failure> {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let program = cmd.get_program().to_string_lossy().into_owned();
        let mut child = cmd.spawn().map_err(|e| Failure::NoStart(format!("could not start {program} ({e})")))?;
        let pid = child.id();
        register(pid);
        self.running = Some(pid);
        // Its pipes are read and written on the side, so a long question or answer cannot block.
        let stdin = child.stdin.take();
        let text = input.to_string();
        let writer = std::thread::spawn(move || {
            if let Some(mut stdin) = stdin {
                let _ = stdin.write_all(text.as_bytes());
            }
        });
        let read_all = |pipe: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut out = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut out);
                }
                String::from_utf8_lossy(&out).into_owned()
            })
        };
        let out = read_all(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
        let err = read_all(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() >= deadline => break None,
                Ok(None) => std::thread::sleep(Duration::from_millis(40)),
                Err(_) => break None,
            }
        };
        self.running = None;
        let Some(status) = status else {
            kill(&mut child);
            return Err(Failure::TimedOut);
        };
        unregister(pid);
        let _ = writer.join();
        Ok((status.success(), out.join().unwrap_or_default(), err.join().unwrap_or_default()))
    }

    // ----- how it is started ------------------------------------------------------------------

    /// The agent's program with the environment sessions get, as the manager, in its folder.
    fn command(&self, program: &str, args: Vec<String>) -> Result<Command, String> {
        // With a PATH of the person's shell the program has to be on it (never whatever this
        // process would find by itself).
        let path = match self.config.env.get("PATH") {
            Some(_) => crate::agents::resolve_program(program, &self.config.env).ok_or_else(|| format!("{program} is not installed (it is not on the PATH)"))?,
            None => PathBuf::from(program),
        };
        // `sys::command`: on Windows it also starts `.cmd` programs, with no console window.
        let mut cmd = crate::sys::command(path);
        cmd.args(args);
        if !self.config.env.is_empty() {
            cmd.env_clear();
        }
        cmd.envs(child_env(self.agent, &self.config));
        for key in SESSION_ENV {
            cmd.env_remove(key);
        }
        if !self.config.cwd.as_os_str().is_empty() {
            std::fs::create_dir_all(&self.config.cwd).map_err(|e| format!("could not make {} ({e})", self.config.cwd.display()))?;
            cmd.current_dir(&self.config.cwd);
        }
        // A group of its own, so stopping it stops what it started too.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        Ok(cmd)
    }
}

impl Drop for Brain {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What makes a `glasscode` command come from one session: the manager's agent is no session.
const SESSION_ENV: &[&str] = &["GLASSCODE_SESSION_ID", "GLASSCODE_SESSION_SIG", "GLASSCODE_EVENTS", "GLASSCODE_WORKTREE", "GLASSCODE_LIMITS", "GLASSCODE_STATUSLINE", "GLASSCODE_CLI_FILES"];

/// The environment the agent runs with: what sessions get, the account's login folder, and
/// `GLASSCODE_MANAGER=1`.
fn child_env(agent: AgentKind, config: &BrainConfig) -> HashMap<String, String> {
    let mut env = config.env.clone();
    for key in SESSION_ENV {
        env.remove(*key);
    }
    // The `glasscode` launcher lives in ~/.local/bin (`cli::launcher_dir`).
    #[cfg(not(windows))]
    {
        let bin = crate::cli::launcher_dir();
        if let Some(path) = env.get("PATH").cloned() {
            if !path.split(':').any(|p| std::path::Path::new(p) == bin) {
                env.insert("PATH".into(), format!("{path}:{}", bin.display()));
            }
        }
    }
    // On Windows the command is ~/.glasscode/bin/glasscode.exe, first on the PATH as for a session.
    #[cfg(windows)]
    crate::cli::add_command_to_path(&mut env);
    if let Some(dir) = &config.config_dir {
        let key = if agent == AgentKind::Codex { "CODEX_HOME" } else { "CLAUDE_CONFIG_DIR" };
        env.insert(key.into(), dir.to_string_lossy().into_owned());
    }
    // Versions are managed by Glasscode's approved-version list.
    env.insert("DISABLE_AUTOUPDATER".into(), "1".into());
    env.insert("GLASSCODE_MANAGER".into(), "1".into());
    env
}

/// `claude`'s arguments. `stream`: the long-lived process with JSON lines in and out; else one
/// question on stdin and the answer as text (only options every version knows).
fn claude_args(prompt: &str, model: Option<&str>, stream: bool) -> Vec<String> {
    let mut args: Vec<String> = vec!["-p".into()];
    if stream {
        args.extend(["--input-format", "stream-json", "--output-format", "stream-json", "--verbose"].map(String::from));
    } else {
        args.extend(["--output-format", "text"].map(String::from));
    }
    args.extend(["--append-system-prompt".to_string(), prompt.to_string()]);
    args.extend(["--allowedTools".to_string(), ALLOWED_TOOLS.to_string()]);
    args.extend(["--disallowedTools".to_string(), denied_tools()]);
    if stream {
        // Only these tools exist for it; what is not allowed above is denied, never asked
        // (whatever the account's own default mode is); no MCP servers; nothing kept on disk.
        args.extend(["--tools", TOOLS, "--permission-mode", "dontAsk", "--permission-prompts", "none", "--strict-mcp-config", "--no-session-persistence"].map(String::from));
    }
    if let Some(model) = model {
        args.extend(["--model".to_string(), model.to_string()]);
    }
    args
}

/// `codex exec`'s arguments: in its sandbox (it may write in its own folder only), with the
/// network on because the `glasscode` command reaches the app through a socket, the manager's
/// instructions as developer instructions, the question on stdin and the answer in `answer`.
fn codex_args(prompt: &str, model: Option<&str>, answer: &str) -> Vec<String> {
    let mut args: Vec<String> = ["exec", "--sandbox", "workspace-write", "-c", "sandbox_workspace_write.network_access=true", "--skip-git-repo-check", "--ephemeral", "--color", "never"].map(String::from).to_vec();
    args.extend(["-c".to_string(), format!("developer_instructions={}", serde_json::to_string(prompt).unwrap_or_default())]);
    if let Some(model) = model {
        args.extend(["-m".to_string(), model.to_string()]);
    }
    args.extend(["-o".to_string(), answer.to_string(), "-".to_string()]);
    args
}

fn send(proc: &mut Proc, line: &str) -> Result<(), Failure> {
    let written = proc.stdin.write_all(line.as_bytes()).and_then(|()| proc.stdin.write_all(b"\n")).and_then(|()| proc.stdin.flush());
    written.map_err(|_| Failure::Broken { spoke: proc.spoke, why: why_it_stopped(proc) })
}

/// The end of what it wrote to stderr, or that it just closed.
fn why_it_stopped(proc: &mut Proc) -> String {
    // Give the reader a moment to take in the last lines.
    std::thread::sleep(Duration::from_millis(60));
    let tail = proc.errors.lock().map(|t| t.trim().to_string()).unwrap_or_default();
    if tail.is_empty() { "it closed without saying why".into() } else { clip(last_lines(&tail, 3).trim(), 400) }
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join(" ")
}

fn minutes(d: Duration) -> String {
    match d.as_secs() {
        s if s >= 120 => format!("{} minutes", s / 60),
        s if s >= 60 => "a minute".into(),
        s => format!("{s} seconds"),
    }
}

/// At most `max` characters, with "…" when something was cut.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ----- what goes in and what comes out (pure) -------------------------------------------------

/// What is sent for one question: what is true right now, then the question.
fn message(question: &str, context: &str, for_voice: bool) -> String {
    let mut out = String::new();
    if !context.trim().is_empty() {
        out.push_str(context.trim_end());
        out.push_str("\n\n---\n");
    }
    out.push_str(question.trim());
    if for_voice {
        out.push_str("\n\nAnswer in at most three short spoken sentences, no markdown, no lists.");
    }
    out
}

/// The last few exchanges in front of a message, for an agent that was not there for them.
fn with_history(history: &VecDeque<Exchange>, body: &str) -> String {
    if history.is_empty() {
        return body.to_string();
    }
    let mut out = String::from("Earlier in this conversation (oldest first):\n");
    for exchange in history {
        out.push_str(&format!("The person: {}\nYou: {}\n", exchange.question, exchange.answer));
    }
    out.push('\n');
    out.push_str(body);
    out
}

/// A user message as one JSON line for `claude --input-format stream-json`.
fn user_line(text: &str) -> String {
    json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": text }] } }).to_string()
}

/// One line of `claude --output-format stream-json`, as far as an answer needs it.
#[derive(Debug, PartialEq)]
enum Line {
    /// The turn is over: what it said (when the line carries it) and whether it failed.
    Result { text: Option<String>, error: bool, subtype: String },
    /// Text of an assistant message (the answer so far).
    Said(String),
    /// It asks its host something (may it use a tool): nobody is there to say yes.
    Control { id: String, subtype: String },
    Other,
}

fn parse_line(line: &str) -> Line {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else { return Line::Other };
    match value["type"].as_str() {
        Some("result") => {
            let subtype = value["subtype"].as_str().unwrap_or_default().to_string();
            let error = value["is_error"].as_bool().unwrap_or(false) || subtype.starts_with("error");
            Line::Result { text: value["result"].as_str().map(str::to_string), error, subtype }
        }
        // Only its own messages: a sub-agent's carry the tool call they belong to.
        Some("assistant") if value["parent_tool_use_id"].is_null() => {
            let text: Vec<&str> = value["message"]["content"].as_array().map(|blocks| blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect()).unwrap_or_default();
            Line::Said(text.join("\n"))
        }
        Some("control_request") => Line::Control { id: value["request_id"].as_str().unwrap_or_default().to_string(), subtype: value["request"]["subtype"].as_str().unwrap_or_default().to_string() },
        _ => Line::Other,
    }
}

/// The answer to something it asks its host: a tool that would need permission is denied,
/// anything else is not supported.
fn control_reply(id: &str, subtype: &str) -> String {
    if subtype == "can_use_tool" {
        let deny = json!({ "behavior": "deny", "message": "Not allowed for the manager. Use the glasscode command." });
        return json!({ "type": "control_response", "response": { "subtype": "success", "request_id": id, "response": deny } }).to_string();
    }
    json!({ "type": "control_response", "response": { "subtype": "error", "request_id": id, "error": "The manager's host does not support this." } }).to_string()
}

/// What to do after a line.
#[derive(Debug, PartialEq)]
enum Step {
    More,
    /// Write this line to its stdin.
    Reply(String),
    /// The answer, or why there is none.
    Done(Result<String, String>),
}

/// One question's answer, put together from the lines as they come.
#[derive(Default)]
struct Turn {
    /// The last thing it said (the answer, when the result line does not carry it).
    said: String,
}

impl Turn {
    fn feed(&mut self, line: &str) -> Step {
        match parse_line(line) {
            Line::Other => Step::More,
            Line::Said(text) => {
                if !text.trim().is_empty() {
                    self.said = text;
                }
                Step::More
            }
            Line::Control { id, subtype } => Step::Reply(control_reply(&id, &subtype)),
            Line::Result { text, error, subtype } => {
                let text = text.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| std::mem::take(&mut self.said)).trim().to_string();
                Step::Done(match (error, text.is_empty()) {
                    (false, false) => Ok(text),
                    (false, true) => Err("The agent finished without saying anything.".into()),
                    (true, false) => Err(text),
                    (true, true) => Err(format!("The agent stopped with an error ({}).", if subtype.is_empty() { "unknown" } else { &subtype })),
                })
            }
        }
    }
}

// ----- its instructions ------------------------------------------------------------------------

/// The manager's standing instructions: who it is, what it may and may not do, how to act
/// (the `glasscode` commands), how to speak.
pub fn system_prompt(settings: &Settings) -> String {
    let name = if settings.name.trim().is_empty() { "the manager" } else { settings.name.trim() };
    let owner = if settings.owner.trim().is_empty() { "the person" } else { settings.owner.trim() };
    let guide = super::scene::guide().trim();
    format!(
        r#"You are {name}, the manager above all of {owner}'s coding sessions in Glasscode, the Mac app in which {owner} runs many Claude Code and Codex sessions. You work for {owner} and for nobody else.

# How you act
You act ONLY through the `glasscode` command in Bash. `glasscode help` lists everything (sessions, pull requests, tickets, the Today board); add --json to parse an answer. `gh` and `git` are for reading only. Your own commands:
- `glasscode manager status`: how you are set up and what waits.
- `glasscode manager brief`: today's update as text. `glasscode manager tour` walks {owner} through it in the app.
- `glasscode manager remember "<fact>"`, `forget "<id or words>"`, `memory [<query>]`: your memory.
- `glasscode manager follow-up list | add "<what>" [--person <name>] | done <which> | drop <which>`: things you keep an eye on until they are done.
- `glasscode manager message list | draft --to <person> "<text>" [--why "<why>"] [--about <ref>]`: messages to teammates. You only draft.
- `glasscode manager show <session> [--tab pr|changes|files] [--section checks|preview|reviews|comments|changes|files|reply] [--say "<text>"]`, or `show --view manager|sessions|today|tickets|usage|settings`: take {owner} there and point at it.
- `glasscode manager scene '<json>'`: draw a small picture while you explain.
- `glasscode manager say "<text>"`: say something to {owner} now.
- `glasscode manager check list | add "<what>" --every <minutes> | remove <id>`: things you look at by yourself on a schedule.
- `glasscode session send <session> "<message>"`: tell a session something.
Every question starts with what is true right now (sessions, follow-ups, messages, memory). Trust it over what you remember, and look closer with `glasscode session show <id>`, `glasscode session read <id>` or `glasscode pr status <n>`.

# Hard limits
- Never approve, merge, close or comment on a pull request, and never post anything on GitHub.
- Never send a message to anyone yourself. You may only DRAFT one with `glasscode manager message draft`; {owner} or their send setting decides whether it goes.
- A draft goes by itself only when it is a routine ping: short and plain, and it names the work by number or with the title in double quotes, for example `PR #42 "Fix the login redirect" is ready for another look`. Anything with a promise, a date, an opinion or a decision always waits for {owner}; when you draft one, say that it waits.
- Never touch a project you do not manage. The sessions you are shown are the ones you manage.
- A command that exits with 4 waits for {owner} to allow it in Glasscode: say that it waits, do not retry it in a loop and do not look for another way. Exit 5 means not allowed: stop and say so.
- Do not change code or files yourself. Work goes to a session (`glasscode session send`, `glasscode session new`, `glasscode pr build`).

# How you speak
Plain, short sentences. Lead with the answer. Then say what you did, and what waits for {owner}. No filler, no headings. Name things exactly (pull request 1111, the session's title). When you do not know, say so and say how you would find out.

# Showing and drawing
When you talk about one session or {owner} asks where something is, take them there with `glasscode manager show …` while you explain. For today's update use `glasscode manager tour`. When an explanation has parts that connect (a flow, who calls whom, a file's lines, before and after), draw it with `glasscode manager scene`. If the command answers with an error, fix the JSON and run it again.
{guide}

# Memory
When you learn something that will still matter next week (a preference of {owner}, who owns what, a decision, how a teammate works), keep it: `glasscode manager remember "<one fact in one sentence>"`. Do not keep what the list of sessions already shows.
"#
    )
}

// ----- what it should know for a question -----------------------------------------------------

/// What it should know for this question: the sessions now, open follow-ups, messages
/// waiting, and what memory has on the question.
pub fn context(settings: &Settings, data: &Data, snap: &Snapshot, question: &str) -> String {
    let owner = if settings.owner.trim().is_empty() { "the person" } else { settings.owner.trim() };
    let at = if snap.at > 0 { snap.at } else { crate::model::now() };
    let mut head = format!("What is true right now, from Glasscode:\nNow: {}, {}.", crate::day::long(crate::day::local(at as i64)), crate::day::clock(at as i64));
    if let Some(me) = snap.me.as_deref().filter(|m| !m.is_empty()) {
        head.push_str(&format!(" {owner} is @{me} on GitHub."));
    }
    if snap.approvals > 0 {
        head.push_str(&format!("\nWaiting for {owner} to allow in Glasscode: {}.", count(snap.approvals, "command", "commands")));
    }
    if !snap.mac_online {
        head.push_str("\nThe Mac cannot be reached: its sessions are as last seen.");
    }

    // The smaller parts first, each within its share; the sessions get what is left.
    let follow_ups: Vec<String> = {
        let mut open: Vec<&super::FollowUp> = data.followups.iter().filter(|f| f.is_open()).collect();
        open.sort_by_key(|f| (f.state != FollowState::Fixed, std::cmp::Reverse(f.updated)));
        open.iter().map(|f| follow_up_line(f)).collect()
    };
    let drafts: Vec<String> = data.outbox.iter().filter(|m| m.state == MsgState::Draft).map(|m| format!("- to {}: “{}”{} · id {}", m.to, clip(&one_line(&m.body), 140), if m.why.trim().is_empty() { String::new() } else { format!(" (why: {})", clip(&one_line(&m.why), 80)) }, short_id(&m.id))).collect();
    let inbox: Vec<String> = data.inbox.iter().filter(|m| !m.handled).map(|m| format!("- {}{}: “{}”", m.from, if m.urgent { " (urgent)" } else { "" }, clip(&one_line(&m.text), 160))).collect();
    let asks: Vec<String> = snap.review_asks.iter().map(|a| format!("- {}#{} “{}” by {}", a.repo, a.number, clip(&a.title, 70), a.author)).collect();
    let rest = [
        section(&format!("Open follow-ups ({})", follow_ups.len()), &follow_ups, 1000, "glasscode manager follow-up list"),
        section(&format!("Messages you drafted that wait for {owner}'s OK ({})", drafts.len()), &drafts, 700, "glasscode manager message list"),
        section(&format!("Messages from teammates not dealt with ({})", inbox.len()), &inbox, 700, ""),
        section(&format!("Pull requests that ask for {owner}'s review ({})", asks.len()), &asks, 500, "glasscode pr list"),
    ];
    let memory = super::memory::context(data, question, 1500);
    let memory = if memory.trim().is_empty() { String::new() } else { format!("What you remember:\n{}", clip(memory.trim(), 1500)) };

    let used: usize = head.chars().count() + rest.iter().chain([&memory]).filter(|s| !s.is_empty()).map(|s| s.chars().count() + 2).sum::<usize>();
    let mut sessions: Vec<&SessionInfo> = snap.sessions.iter().collect();
    sessions.sort_by_key(|s| match s.state {
        SessionState::NeedsYou => 0,
        SessionState::Working => 1,
        SessionState::Idle => 2,
        SessionState::NotRunning => 3,
    });
    let lines: Vec<String> = sessions.iter().map(|s| session_line(s)).collect();
    let sessions = match lines.is_empty() {
        true => "Sessions: none.".to_string(),
        false => section(&format!("Sessions ({})", lines.len()), &lines, CONTEXT_BUDGET.saturating_sub(used + 2).max(400), "glasscode session list"),
    };

    let mut out = head;
    for part in [&sessions, &rest[0], &rest[1], &rest[2], &rest[3], &memory] {
        if !part.is_empty() {
            out.push_str("\n\n");
            out.push_str(part);
        }
    }
    clip(&out, CONTEXT_BUDGET)
}

/// A titled list within `budget` characters; what does not fit is counted, with where to see it.
fn section(title: &str, lines: &[String], budget: usize, more: &str) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = format!("{title}:");
    let mut used = out.chars().count();
    for (i, line) in lines.iter().enumerate() {
        let len = line.chars().count() + 1;
        if used + len > budget.saturating_sub(60) && i > 0 {
            let left = lines.len() - i;
            out.push_str(&match more.is_empty() {
                true => format!("\n… and {left} more."),
                false => format!("\n… and {left} more (`{more}`)."),
            });
            return out;
        }
        out.push('\n');
        out.push_str(line);
        used += len;
    }
    out
}

/// "- [needs you] “Title” · project · branch x · PR #12 open, CI failing, … · id 1a2b3c4d" and
/// the first line of its last reply.
fn session_line(s: &SessionInfo) -> String {
    let state = match s.state {
        SessionState::NeedsYou => "needs you",
        SessionState::Working => "working",
        SessionState::Idle => "idle",
        SessionState::NotRunning => "not running",
    };
    let mut line = format!("- [{state}] “{}” · {}", clip(&s.title, 60), s.project);
    if !s.branch.is_empty() {
        line.push_str(&format!(" · branch {}", clip(&s.branch, 50)));
    }
    if let Some(pr) = &s.pr {
        line.push_str(&format!(" · PR #{} {}", pr.number, if pr.state.is_empty() { "open" } else { &pr.state }));
        if !pr.ci.is_empty() {
            line.push_str(&format!(", CI {}", pr.ci));
        }
        if !pr.reviewers.is_empty() {
            let reviews: Vec<String> = pr.reviewers.iter().take(5).map(|r| format!("{} {}", r.login, r.state.to_lowercase().replace('_', " "))).collect();
            line.push_str(&format!(", reviews: {}", reviews.join(", ")));
        }
        if pr.unresolved_threads > 0 {
            line.push_str(&format!(", {}", count(pr.unresolved_threads as usize, "unresolved thread", "unresolved threads")));
        }
        if pr.conflicting {
            line.push_str(", conflicts");
        }
    }
    if let Some((repo, number)) = &s.review_of {
        line.push_str(&format!(" · reviews {repo}#{number}"));
    }
    if let Some(ticket) = s.ticket.as_deref().filter(|t| !t.is_empty()) {
        line.push_str(&format!(" · {ticket}"));
    }
    if s.on_server {
        line.push_str(" · on the server");
    }
    line.push_str(&format!(" · id {}", short_id(&s.id)));
    if let Some(first) = s.last_reply.as_deref().and_then(|r| r.lines().map(str::trim).find(|l| !l.is_empty())) {
        line.push_str(&format!("\n    last: {}", clip(first, 160)));
    }
    line
}

fn follow_up_line(f: &super::FollowUp) -> String {
    let state = if f.state == FollowState::Fixed { "they say fixed, not looked at yet" } else { "open" };
    let who = match (f.person.trim(), f.login.trim()) {
        ("", "") => String::new(),
        ("", login) => format!(" · @{login}"),
        (person, "") => format!(" · {person}"),
        (person, login) => format!(" · {person} (@{login})"),
    };
    let what = if f.number > 0 { format!("{}#{} “{}”", f.repo, f.number, clip(&f.title, 60)) } else { format!("“{}”", clip(&one_line(if f.title.is_empty() { &f.note } else { &f.title }), 100)) };
    let blockers = if f.blockers.is_empty() { String::new() } else { format!(" · {} of {} still open", f.open_blockers(), f.blockers.len()) };
    let note = if f.note.trim().is_empty() || f.number == 0 { String::new() } else { format!(" · {}", clip(&one_line(&f.note), 80)) };
    format!("- [{state}] {what}{who}{blockers}{note} · id {}", short_id(&f.id))
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The start of an id, enough for the `glasscode` command to find it.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::{FollowUp, InMessage, OutMessage, PrInfo, ReviewerInfo};

    fn settings() -> Settings {
        Settings { enabled: true, name: "AI Alex".into(), owner: "Alex".into(), ..Settings::default() }
    }

    fn session(id: &str, title: &str, state: SessionState) -> SessionInfo {
        SessionInfo { id: id.into(), title: title.into(), project: "shop".into(), project_id: "p1".into(), state, branch: format!("feat/{id}"), ..SessionInfo::default() }
    }

    #[test]
    fn prompt_says_who_it_is_and_what_it_must_not_do() {
        let prompt = system_prompt(&settings());
        assert!(prompt.starts_with("You are AI Alex, the manager above all of Alex's coding sessions in Glasscode"));
        assert!(prompt.contains("Never approve, merge, close or comment on a pull request"));
        assert!(prompt.contains("Never send a message to anyone yourself"));
        assert!(prompt.contains("glasscode manager message draft"));
        assert!(prompt.contains("PR #42 \"Fix the login redirect\" is ready for another look"));
        assert!(prompt.contains("a promise, a date, an opinion or a decision always waits for Alex; when you draft one, say that it waits"));
        assert!(prompt.contains("Never touch a project you do not manage"));
        assert!(prompt.contains("exits with 4 waits for Alex"));
        assert!(prompt.contains("Exit 5 means not allowed"));
        assert!(prompt.contains("ONLY through the `glasscode` command"));
        assert!(prompt.contains("`glasscode help` lists everything"));
        for verb in ["manager status", "manager brief", "manager tour", "manager remember", "manager follow-up", "manager show", "manager scene", "manager say", "manager check"] {
            assert!(prompt.contains(&format!("glasscode {verb}")), "{verb} is missing");
        }
        assert!(prompt.contains(crate::manager::scene::guide().trim()));
        // Tight: its own text (without the scene guide) stays well under 5000 characters.
        assert!(prompt.chars().count() - crate::manager::scene::guide().trim().chars().count() < 5000);
        // Never told to approve a message itself.
        assert!(!prompt.contains("message approve"));
    }

    #[test]
    fn prompt_without_names_still_reads() {
        let prompt = system_prompt(&Settings { name: " ".into(), owner: String::new(), ..Settings::default() });
        assert!(prompt.starts_with("You are the manager, the manager above all of the person's coding sessions"));
    }

    #[test]
    fn context_lists_what_matters_in_order() {
        let mut snap = Snapshot { at: 1_763_200_000, mac_online: true, approvals: 2, me: Some("alex-dev".into()), ..Snapshot::default() };
        let mut idle = session("aaaaaaaa-1111", "Tidy the docs", SessionState::Idle);
        idle.last_reply = Some("\n  All done, the docs build.\nSecond line".into());
        let mut needs = session("bbbbbbbb-2222", "Checkout retries", SessionState::NeedsYou);
        needs.pr = Some(PrInfo {
            repo: "acme/shop".into(),
            number: 1111,
            state: "open".into(),
            ci: "failing".into(),
            unresolved_threads: 2,
            conflicting: true,
            reviewers: vec![ReviewerInfo { login: "sam".into(), state: "CHANGES_REQUESTED".into() }],
            ..PrInfo::default()
        });
        snap.sessions = vec![idle, needs];
        let mut data = Data::default();
        data.followups.push(FollowUp { id: "f1f1f1f1-9".into(), repo: "acme/shop".into(), number: 1200, title: "Retry queue".into(), person: "Sam".into(), login: "sam".into(), ..FollowUp::default() });
        data.followups.push(FollowUp { id: "done".into(), state: FollowState::Done, title: "Old one".into(), ..FollowUp::default() });
        data.outbox.push(OutMessage { id: "m1m1m1m1-9".into(), to: "Mira".into(), body: "The fix for 1111 is pushed.".into(), why: "she asked".into(), ..OutMessage::default() });
        data.outbox.push(OutMessage { id: "sent".into(), to: "Sam".into(), body: "Already gone".into(), state: MsgState::Sent, ..OutMessage::default() });
        data.inbox.push(InMessage { id: "i1".into(), from: "Sam".into(), text: "Can you look at 1200?".into(), urgent: true, ..InMessage::default() });
        data.inbox.push(InMessage { id: "i2".into(), from: "Mira".into(), text: "Seen already".into(), handled: true, ..InMessage::default() });

        let text = context(&settings(), &data, &snap, "what is blocked?");
        let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("“{needle}” is missing in:\n{text}"));
        assert!(text.starts_with("What is true right now, from Glasscode:\nNow: "));
        assert!(text.contains("Alex is @alex-dev on GitHub."));
        assert!(text.contains("Waiting for Alex to allow in Glasscode: 2 commands."));
        // The session that needs the person comes first, with its pull request.
        assert!(at("[needs you] “Checkout retries”") < at("[idle] “Tidy the docs”"));
        assert!(text.contains("PR #1111 open, CI failing, reviews: sam changes requested, 2 unresolved threads, conflicts"));
        assert!(text.contains("conflicts · id bbbbbbbb"));
        assert!(text.contains("    last: All done, the docs build."));
        assert!(!text.contains("Second line"));
        // Then follow-ups, drafts, the inbox.
        assert!(at("Sessions (2):") < at("Open follow-ups (1):"));
        assert!(at("Open follow-ups (1):") < at("wait for Alex's OK (1):"));
        assert!(at("wait for Alex's OK (1):") < at("Messages from teammates not dealt with (1):"));
        assert!(text.contains("acme/shop#1200 “Retry queue” · Sam (@sam)"));
        assert!(text.contains("- to Mira: “The fix for 1111 is pushed.” (why: she asked) · id m1m1m1m1"));
        assert!(text.contains("- Sam (urgent): “Can you look at 1200?”"));
        assert!(!text.contains("Old one") && !text.contains("Already gone") && !text.contains("Seen already"));
    }

    #[test]
    fn context_stays_within_its_budget() {
        let mut snap = Snapshot { at: 1_763_200_000, mac_online: false, ..Snapshot::default() };
        for i in 0..300 {
            let mut s = session(&format!("{i:08}-x"), &format!("Session number {i} with a fairly long title to fill the line"), SessionState::Working);
            s.last_reply = Some("A long last reply ".repeat(30));
            snap.sessions.push(s);
        }
        snap.sessions.push(session("needsme1-x", "The one that waits", SessionState::NeedsYou));
        let mut data = Data::default();
        for i in 0..80 {
            data.followups.push(FollowUp { id: format!("f{i:07}-x"), repo: "acme/shop".into(), number: 1000 + i, title: "Something to follow up on with a long title".into(), updated: i, ..FollowUp::default() });
            data.outbox.push(OutMessage { id: format!("m{i:07}-x"), to: "Sam".into(), body: "A draft ".repeat(40), ..OutMessage::default() });
        }
        let text = context(&settings(), &data, &snap, "status");
        assert!(text.chars().count() <= CONTEXT_BUDGET, "{} characters", text.chars().count());
        // The most important survives the cut, and what was cut is counted.
        assert!(text.contains("The one that waits"));
        assert!(text.contains("more (`glasscode session list`)"));
        assert!(text.contains("Open follow-ups (80):"));
        assert!(text.contains("more (`glasscode manager follow-up list`)"));
        assert!(text.contains("The Mac cannot be reached"));
        assert!(context(&settings(), &Data::default(), &Snapshot::default(), "").contains("Sessions: none."));
    }

    #[test]
    fn a_question_is_one_json_line() {
        let line = user_line("What is blocked?\nAnd \"why\"?");
        assert!(!line.contains('\n'));
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["type"], "user");
        assert_eq!(value["message"]["role"], "user");
        assert_eq!(value["message"]["content"][0]["type"], "text");
        assert_eq!(value["message"]["content"][0]["text"], "What is blocked?\nAnd \"why\"?");
    }

    #[test]
    fn message_has_context_question_and_the_voice_rule() {
        assert_eq!(message(" What now? ", "", false), "What now?");
        let text = message("What now?", "Sessions: none.\n", true);
        assert_eq!(text, "Sessions: none.\n\n---\nWhat now?\n\nAnswer in at most three short spoken sentences, no markdown, no lists.");
        let mut history = VecDeque::new();
        assert_eq!(with_history(&history, "Q"), "Q");
        history.push_back(Exchange { question: "What is blocked?".into(), answer: "Nothing.".into() });
        assert_eq!(with_history(&history, "And now?"), "Earlier in this conversation (oldest first):\nThe person: What is blocked?\nYou: Nothing.\n\nAnd now?");
    }

    #[test]
    fn a_turn_ends_with_the_result_line() {
        let mut turn = Turn::default();
        assert_eq!(turn.feed(r#"{"type":"system","subtype":"init","session_id":"s1","tools":["Bash"]}"#), Step::More);
        assert_eq!(turn.feed(r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Let me look."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"glasscode session list"}}]},"parent_tool_use_id":null}"#), Step::More);
        assert_eq!(turn.feed(r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"two sessions"}]}}"#), Step::More);
        assert_eq!(turn.feed("not json at all"), Step::More);
        assert_eq!(turn.feed(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Two sessions run."}]}}"#), Step::More);
        let done = turn.feed(r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1200,"result":"Two sessions run. Nothing waits for you.","session_id":"s1"}"#);
        assert_eq!(done, Step::Done(Ok("Two sessions run. Nothing waits for you.".into())));
    }

    #[test]
    fn a_result_without_text_uses_what_it_said_last() {
        let mut turn = Turn::default();
        turn.feed(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"First."}]}}"#);
        // A sub-agent's text is not the answer.
        turn.feed(r#"{"type":"assistant","parent_tool_use_id":"t9","message":{"content":[{"type":"text","text":"Inner."}]}}"#);
        assert_eq!(turn.feed(r#"{"type":"result","subtype":"success","is_error":false}"#), Step::Done(Ok("First.".into())));
        assert_eq!(Turn::default().feed(r#"{"type":"result","subtype":"success","is_error":false,"result":""}"#), Step::Done(Err("The agent finished without saying anything.".into())));
    }

    #[test]
    fn an_error_result_is_an_error() {
        let said = Turn::default().feed(r#"{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login"}"#);
        assert_eq!(said, Step::Done(Err("Invalid API key · Please run /login".into())));
        let silent = Turn::default().feed(r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#);
        assert_eq!(silent, Step::Done(Err("The agent stopped with an error (error_max_turns).".into())));
        assert_eq!(parse_line(r#"{"type":"result","subtype":"error_during_execution"}"#), Line::Result { text: None, error: true, subtype: "error_during_execution".into() });
    }

    #[test]
    fn a_tool_that_would_ask_is_denied() {
        let step = Turn::default().feed(r#"{"type":"control_request","request_id":"req_7","request":{"subtype":"can_use_tool","tool_name":"Write","input":{}}}"#);
        let Step::Reply(line) = step else { panic!("expected a reply") };
        assert!(!line.contains('\n'));
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["type"], "control_response");
        assert_eq!(value["response"]["subtype"], "success");
        assert_eq!(value["response"]["request_id"], "req_7");
        assert_eq!(value["response"]["response"]["behavior"], "deny");
        let other: Value = serde_json::from_str(&control_reply("req_8", "hook_callback")).unwrap();
        assert_eq!(other["response"]["subtype"], "error");
        assert_eq!(other["response"]["request_id"], "req_8");
    }

    #[test]
    fn claude_is_started_with_only_what_it_may_use() {
        let args = claude_args("PROMPT", Some("opus"), true);
        let after = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| panic!("{flag} is missing"));
        assert_eq!(args[0], "-p");
        assert_eq!(after("--input-format"), "stream-json");
        assert_eq!(after("--output-format"), "stream-json");
        assert!(args.contains(&"--verbose".to_string()));
        assert_eq!(after("--append-system-prompt"), "PROMPT");
        assert_eq!(after("--allowedTools"), "Bash(glasscode:*),Bash(gh:*),Bash(git:*),Read,Grep,Glob");
        assert_eq!(after("--tools"), "Bash,Read,Grep,Glob");
        assert_eq!(after("--permission-mode"), "dontAsk");
        assert_eq!(after("--permission-prompts"), "none");
        assert_eq!(after("--model"), "opus");
        let denied = after("--disallowedTools");
        for must in ["Bash(gh pr merge:*)", "Bash(gh pr review:*)", "Bash(gh pr close:*)", "Bash(gh pr comment:*)", "Bash(git push:*)"] {
            assert!(denied.contains(must), "{must} is not denied");
        }
        // No model: Claude's own default. One question at a time: plain text, no JSON options.
        let once = claude_args("PROMPT", None, false);
        assert!(!once.contains(&"--model".to_string()) && !once.contains(&"--input-format".to_string()) && !once.contains(&"--permission-prompts".to_string()));
        assert!(once.contains(&"--allowedTools".to_string()) && once.contains(&"--disallowedTools".to_string()));
    }

    #[test]
    fn codex_gets_the_instructions_and_an_answer_file() {
        let args = codex_args("Line one\n\"two\"", Some("gpt-x"), "/tmp/answer.txt");
        assert_eq!(args[0], "exec");
        assert!(args.windows(2).any(|w| w[0] == "--sandbox" && w[1] == "workspace-write"));
        assert!(args.windows(2).any(|w| w[0] == "-m" && w[1] == "gpt-x"));
        assert!(args.windows(2).any(|w| w[0] == "-o" && w[1] == "/tmp/answer.txt"));
        assert!(args.contains(&r#"developer_instructions="Line one\n\"two\"""#.to_string()));
        assert_eq!(args.last().map(String::as_str), Some("-"));
    }

    #[test]
    fn the_agent_runs_as_the_manager_not_as_a_session() {
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
        env.insert("GLASSCODE_SESSION_ID".to_string(), "s1".to_string());
        env.insert("GLASSCODE_SESSION_SIG".to_string(), "sig".to_string());
        let config = BrainConfig { config_dir: Some(PathBuf::from("/accounts/work")), env, cwd: PathBuf::new() };
        let claude = child_env(AgentKind::Claude, &config);
        assert_eq!(claude.get("GLASSCODE_MANAGER").map(String::as_str), Some("1"));
        assert_eq!(claude.get("DISABLE_AUTOUPDATER").map(String::as_str), Some("1"));
        assert_eq!(claude.get("CLAUDE_CONFIG_DIR").map(String::as_str), Some("/accounts/work"));
        assert!(!claude.contains_key("GLASSCODE_SESSION_ID") && !claude.contains_key("GLASSCODE_SESSION_SIG") && !claude.contains_key("CODEX_HOME"));
        // The person's PATH first, then where the `glasscode` launcher is, and nothing else.
        #[cfg(not(windows))]
        assert_eq!(claude["PATH"], format!("/usr/bin:/bin:{}", crate::cli::launcher_dir().display()));
        // On Windows the folder of the `glasscode` command comes first, as for a session.
        #[cfg(windows)]
        assert!(claude["PATH"].starts_with(&crate::cli::command_dir().display().to_string()));
        assert!(!ALLOWED_TOOLS.split(',').any(|tool| tool.starts_with("Bash(") && !["Bash(glasscode:*)", "Bash(gh:*)", "Bash(git:*)"].contains(&tool)));
        let codex = child_env(AgentKind::Codex, &config);
        assert_eq!(codex.get("CODEX_HOME").map(String::as_str), Some("/accounts/work"));
        assert!(!codex.contains_key("CLAUDE_CONFIG_DIR"));
    }

    // ----- with a stand-in for the agent (a shell script named `claude` or `codex`) ----------

    /// A folder with a script as the agent, and a `Brain` that finds only that.
    /// The script runs only on Unix; on Windows the folder has a file the PATH search does not
    /// take as a program, which is enough for the test of an agent that is not installed.
    fn brain_with(program: &str, script: &str, agent: AgentKind) -> (tempfile::TempDir, Brain) {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let path = bin.join(program);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut env = HashMap::new();
        let path = std::env::join_paths([bin.clone(), PathBuf::from("/usr/bin"), PathBuf::from("/bin")]).unwrap();
        env.insert("PATH".to_string(), path.to_string_lossy().into_owned());
        env.insert("HOME".to_string(), dir.path().display().to_string());
        let config = BrainConfig { config_dir: None, env, cwd: dir.path().join("work") };
        let brain = Brain::new(&Settings { brain: agent, ..settings() }, config);
        (dir, brain)
    }

    /// Answers every line on stdin with "answer <n>", like `claude` with JSON lines.
    #[cfg(unix)]
    const TALKS: &str = r#"echo '{"type":"system","subtype":"init"}'
n=0
while IFS= read -r line; do
  n=$((n+1))
  echo '{"type":"assistant","message":{"content":[{"type":"text","text":"thinking"}]}}'
  echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"answer $n in $(basename "$PWD") as $GLASSCODE_MANAGER\"}"
  [ -n "$ONCE" ] && exit 0
done"#;

    // A shell script stands in for the agent.
    #[cfg(unix)]
    #[test]
    fn one_process_answers_question_after_question() {
        let (_dir, mut brain) = brain_with("claude", TALKS, AgentKind::Claude);
        assert_eq!(brain.ask("first", "ctx", false).unwrap(), "answer 1 in work as 1");
        let pid = brain.pid().expect("it keeps running");
        assert!(pids().contains(&pid));
        assert_eq!(brain.ask("second", "ctx", true).unwrap(), "answer 2 in work as 1");
        assert_eq!(brain.pid(), Some(pid));
        // Stopped: the next question goes to a fresh one.
        brain.stop();
        assert!(brain.pid().is_none() && !pids().contains(&pid));
        assert_eq!(brain.ask("third", "", false).unwrap(), "answer 1 in work as 1");
        assert_eq!(brain.history.len(), 3);
    }

    // A shell script stands in for the agent.
    #[cfg(unix)]
    #[test]
    fn a_dead_process_is_started_again_once() {
        let (_dir, mut brain) = brain_with("claude", &format!("ONCE=1\n{TALKS}"), AgentKind::Claude);
        assert_eq!(brain.ask("first", "", false).unwrap(), "answer 1 in work as 1");
        // It exited after its answer; the next question finds that out and starts a fresh one.
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(brain.ask("second", "", false).unwrap(), "answer 1 in work as 1");
        assert!(!brain.one_shot);
    }

    // A shell script stands in for the agent.
    #[cfg(unix)]
    #[test]
    fn a_claude_without_json_input_is_asked_one_question_at_a_time() {
        let script = r#"case "$*" in *--input-format*) echo "error: unknown option '--input-format'" >&2; exit 1;; esac
question=$(cat)
echo "plain answer to: $(printf '%s' "$question" | tail -n 1)""#;
        let (_dir, mut brain) = brain_with("claude", script, AgentKind::Claude);
        assert_eq!(brain.ask("what now?", "ctx", false).unwrap(), "plain answer to: what now?");
        assert!(brain.one_shot);
        // The next one carries the conversation with it.
        assert_eq!(brain.ask("and then?", "", false).unwrap(), "plain answer to: and then?");
    }

    // A shell script stands in for the agent.
    #[cfg(unix)]
    #[test]
    fn an_answer_that_takes_too_long_stops_the_agent() {
        let (_dir, mut brain) = brain_with("claude", "sleep 30", AgentKind::Claude);
        brain.timeout = Duration::from_millis(400);
        let started = Instant::now();
        let error = brain.ask("anything", "", false).unwrap_err().to_string();
        assert!(error.contains("took longer than"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(brain.pid().is_none());
    }

    #[test]
    fn an_agent_that_is_not_installed_says_so() {
        let (dir, mut brain) = brain_with("something-else", "exit 0", AgentKind::Claude);
        let error = brain.ask("anything", "", false).unwrap_err().to_string();
        assert_eq!(error, "AI Alex's agent (Claude) could not be started: claude is not installed (it is not on the PATH)");
        // Not a reason to give up on the long-lived process.
        assert!(!brain.one_shot);
        drop(dir);
        let mut shell = Brain::new(&Settings { brain: AgentKind::Shell, ..settings() }, BrainConfig::default());
        assert!(shell.ask("anything", "", false).unwrap_err().to_string().contains("needs Claude or Codex"));
    }

    #[test]
    fn a_brain_can_think_on_another_thread() {
        fn sendable<T: Send>() {}
        sendable::<Brain>();
    }

    // A shell script stands in for the agent.
    #[cfg(unix)]
    #[test]
    fn codex_answers_through_its_answer_file() {
        // Finds the file after -o and writes the answer there, like `codex exec`.
        let script = r#"out=""
while [ $# -gt 0 ]; do
  if [ "$1" = "-o" ]; then out="$2"; fi
  shift
done
question=$(cat)
echo "progress that is not the answer"
printf 'codex says: %s (%s)' "$(printf '%s' "$question" | tail -n 1)" "${CODEX_HOME:-no home}" > "$out""#;
        let (dir, mut brain) = brain_with("codex", script, AgentKind::Codex);
        brain.config.config_dir = Some(dir.path().join("codex-home"));
        let said = brain.ask("is it merged?", "ctx", false).unwrap();
        assert_eq!(said, format!("codex says: is it merged? ({})", dir.path().join("codex-home").display()));
        assert!(brain.pid().is_none());
    }
}
