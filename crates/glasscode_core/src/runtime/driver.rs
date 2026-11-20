//! The brain's driver (M50 step 1; brain-19, land-17), shaped like the Agent Client Protocol: a
//! session is made new or loaded, a prompt is one turn, the turn streams its updates, a
//! permission request is an update the driver answers, and cancel stops the turn. Claude's
//! stream-json process (`claude.rs`) and `codex exec` (`codex.rs`) are its two drivers; an ACP
//! driver can come behind the same trait.
//!
//! [`run_turn`] is the one loop every lane uses: it checks the tool list on the first init line
//! and stops a brain whose tools are not exactly Glasscode's (D4, brain-m5), refuses permission
//! requests, ends a turn that goes quiet for too long or runs past its cap, and keeps the usage.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::budget::Usage;
use super::health::BrainHealth;
use crate::caller::Lane;
use crate::model::AgentKind;

/// What a brain process is started with.
#[derive(Clone, Debug)]
pub struct BrainSpec {
    pub agent: AgentKind,
    pub lane: Lane,
    pub wake: Option<String>,
    /// `claude` or `codex`, as found on the account's PATH.
    pub program: PathBuf,
    /// Glasscode's own program, for `glasscode mcp --profile manager`.
    pub glasscode: PathBuf,
    pub model: String,
    pub effort: Option<String>,
    pub fallback_model: Option<String>,
    /// The brain's working folder (`paths::manager_work_dir`).
    pub cwd: PathBuf,
    /// Where its files go, readable by this user only (`paths::manager_run_dir`).
    pub run_dir: PathBuf,
    /// The account's environment (its login folder, the PATH of the person's shell).
    pub env: HashMap<String, String>,
    /// The static instructions and the always-loaded memory: the system prompt, cached.
    pub instructions: String,
    /// The brain's MCP server identity (`mcp::brain_env`).
    pub brain_env: Vec<(String, String)>,
    /// A conversation to load (ACP's session/load); none for a new one.
    pub resume: Option<String>,
}

/// One update of a turn.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// The conversation's id, to load it again later.
    Session { id: String },
    /// The process's set-up, on its first line (Claude's `system/init`).
    Init { tools: Vec<String>, model: String, version: String },
    /// What it has said so far (a part, or a whole message).
    Text(String),
    /// It calls one of its tools.
    ToolCall { name: String },
    /// It asks may it use a tool: the driver refuses (nothing is the person's to allow here).
    Permission { id: String, tool: String },
    /// A vendor's rate-limit reading.
    RateLimit(Value),
    /// What the turn used.
    Usage(Usage),
    /// The turn is over.
    Ended(TurnEnd),
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnEnd {
    Answered(String),
    /// Stopped by the person (an interrupt, or the process ended on Stop).
    Stopped,
    Failed(String),
}

/// A brain driver: ACP's session/new or /load, /prompt, /update, /cancel.
pub trait BrainDriver: Send {
    /// Start the process for a new conversation, or the one of `BrainSpec::resume`.
    fn new_session(&mut self) -> Result<(), String>;
    /// One user turn.
    fn prompt(&mut self, text: &str) -> Result<(), String>;
    /// The next update, waiting up to `wait`; none when nothing came.
    fn next(&mut self, wait: Duration) -> Option<Update>;
    /// Stop the turn (the person's Stop).
    fn cancel(&mut self);
    /// End the process.
    fn stop(&mut self);
    fn pid(&self) -> Option<u32>;
    /// The conversation's id, once known.
    fn session_id(&self) -> Option<String>;
    /// What the process wrote on stderr last.
    fn stderr_tail(&self) -> String;
}

/// How long a turn may take.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// With no update for this long, the turn is over.
    pub idle: Duration,
    /// The longest a turn may take in all.
    pub turn: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { idle: Duration::from_secs(90), turn: Duration::from_secs(15 * 60) }
    }
}

/// What came of a turn.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub end: TurnEnd,
    pub usage: Usage,
    /// The brain's state the turn showed (signed out, out of usage, the wrong tools…).
    pub health: Option<BrainHealth>,
    /// The init line's tools were checked and are Glasscode's (this turn or before).
    pub checked: bool,
    pub tools_called: Vec<String>,
}

/// The tools a brain must have: none but these (D4). `extra` and `missing` when it differs.
pub type Expected<'a> = &'a dyn Fn(&[String]) -> Result<(), (Vec<String>, Vec<String>)>;

/// One turn of `driver`: the prompt, then its updates until it ends. On the first init line the
/// tools are checked (`expected`); a brain with others is stopped before its first tool call can
/// be served (the MCP server refuses until `on_checked`). Permission requests are refused, a turn
/// quiet for `limits.idle` or longer than `limits.turn` is cancelled, and `on_update` sees each
/// update as it comes.
pub fn run_turn(driver: &mut dyn BrainDriver, text: &str, checked: bool, expected: Expected, on_checked: &mut dyn FnMut(), on_update: &mut dyn FnMut(&Update), limits: Limits) -> Outcome {
    let mut outcome = Outcome { end: TurnEnd::Failed("it said nothing".into()), usage: Usage::default(), health: None, checked, tools_called: Vec::new() };
    if let Err(why) = driver.prompt(text) {
        outcome.health = Some(BrainHealth::Crashed { why: why.clone() });
        outcome.end = TurnEnd::Failed(why);
        return outcome;
    }
    let started = Instant::now();
    let mut last = Instant::now();
    let mut cancelled = false;
    loop {
        let wait = limits.idle.saturating_sub(last.elapsed()).min(limits.turn.saturating_sub(started.elapsed())).max(Duration::from_millis(10));
        let Some(update) = driver.next(wait) else {
            if (last.elapsed() >= limits.idle || started.elapsed() >= limits.turn) && !cancelled {
                // Quiet too long, or too long in all: stopped, as on the person's Stop.
                driver.cancel();
                cancelled = true;
                last = Instant::now();
                continue;
            }
            if cancelled && last.elapsed() >= Duration::from_secs(10) {
                driver.stop();
                outcome.end = TurnEnd::Failed(format!("it gave no answer in {} s and was stopped", started.elapsed().as_secs()));
                return outcome;
            }
            continue;
        };
        last = Instant::now();
        on_update(&update);
        match &update {
            Update::Init { tools, .. } if !outcome.checked => match expected(tools) {
                Ok(()) => {
                    outcome.checked = true;
                    on_checked();
                }
                Err((extra, missing)) => {
                    driver.stop();
                    outcome.health = Some(BrainHealth::ToolListWrong { extra, missing });
                    outcome.end = TurnEnd::Failed("its tools were not Glasscode's".into());
                    return outcome;
                }
            },
            Update::ToolCall { name } => outcome.tools_called.push(name.clone()),
            Update::RateLimit(event) => {
                if let Some(state) = super::health::from_rate_limit(event) {
                    outcome.health = Some(state);
                }
            }
            Update::Usage(usage) => outcome.usage = *usage,
            Update::Ended(end) => {
                if let TurnEnd::Failed(why) = end
                    && outcome.health.is_none()
                {
                    outcome.health = super::health::from_failure(&format!("{why}\n{}", driver.stderr_tail()), 0);
                }
                outcome.end = if cancelled && !matches!(end, TurnEnd::Answered(_)) { TurnEnd::Stopped } else { end.clone() };
                return outcome;
            }
            _ => {}
        }
    }
}

// ----- the process ---------------------------------------------------------------------------

/// A running brain process: its stdin, its stdout as lines, the end of its stderr.
pub(crate) struct Proc {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub lines: mpsc::Receiver<String>,
    pub errors: Arc<Mutex<String>>,
}

impl Proc {
    /// Start `cmd` with its three pipes read and written on the side.
    pub fn spawn(mut cmd: Command) -> Result<Proc, String> {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let program = cmd.get_program().to_string_lossy().into_owned();
        let mut child = cmd.spawn().map_err(|e| format!("could not start {program} ({e})"))?;
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
            let _ = child.kill();
            return Err(format!("could not talk to {program}"));
        };
        let (tx, lines) = mpsc::channel::<String>();
        let _ = std::thread::Builder::new().name("brain-out".into()).spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let errors = Arc::new(Mutex::new(String::new()));
        let tail = errors.clone();
        let _ = std::thread::Builder::new().name("brain-err".into()).spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if let Ok(mut tail) = tail.lock() {
                    tail.push_str(&line);
                    tail.push('\n');
                    if tail.len() > 4000 {
                        let cut = (tail.len() - 2000..tail.len()).find(|i| tail.is_char_boundary(*i)).unwrap_or(tail.len());
                        tail.drain(..cut);
                    }
                }
            }
        });
        Ok(Proc { child, stdin, lines, errors })
    }

    /// Write one line to its stdin.
    pub fn send(&mut self, line: &str) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("its input is closed")?;
        stdin.write_all(line.as_bytes()).and_then(|()| stdin.write_all(b"\n")).and_then(|()| stdin.flush()).map_err(|e| format!("it stopped reading ({e})"))
    }

    pub fn stderr_tail(&self) -> String {
        self.errors.lock().map(|t| t.clone()).unwrap_or_default()
    }

    /// Ask it to stop what it does (SIGINT to its group on Unix).
    pub fn interrupt(&self) {
        #[cfg(unix)]
        {
            let _ = crate::sys::command("kill").args(["-INT", &format!("-{}", self.child.id())]).status();
        }
    }

    /// End it and what it started: its process group on Unix, its tree on Windows (brain-17).
    pub fn end(&mut self) {
        let pid = self.child.id();
        #[cfg(unix)]
        {
            let _ = crate::sys::command("kill").args(["-TERM", &format!("-{pid}")]).status();
        }
        #[cfg(windows)]
        {
            let _ = crate::sys::end_tree(pid);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// It has ended by itself.
    pub fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if !self.exited() {
            self.end();
        }
    }
}

/// The command a brain's program runs as: the account's environment and nothing of a session's,
/// in the brain's folder, in a process group of its own. No `.cmd` shim on Windows, so that long
/// arguments arrive as written.
/// The headless profile of a lane's brain (M23, D4): `profile::headless_profile(BrainChat |
/// BrainWake)`.
pub fn profile_of(spec: &BrainSpec) -> crate::profile::SessionProfile {
    let kind = if spec.lane == crate::caller::Lane::Chat { crate::profile::HeadlessKind::BrainChat } else { crate::profile::HeadlessKind::BrainWake };
    crate::profile::headless_profile(kind, spec.agent, &serde_json::Value::Null, std::path::Path::new(""), std::path::Path::new(""))
}

pub(crate) fn command(spec: &BrainSpec, args: Vec<String>) -> Result<Command, String> {
    let search = spec.env.get("PATH").map(String::as_str).unwrap_or_default();
    let (path, first) = crate::sys::script_target(&spec.program, search.as_ref()).unwrap_or((spec.program.clone(), Vec::new()));
    let mut cmd = crate::sys::command(path);
    cmd.args(first).args(args);
    if !spec.env.is_empty() {
        cmd.env_clear();
        cmd.envs(&spec.env);
    }
    for key in SESSION_ENV {
        cmd.env_remove(key);
    }
    // The profile's own: no forge token (the forge is Glasscode's ops), and a wake's limits.
    let profile = profile_of(spec);
    for key in &profile.env_remove {
        cmd.env_remove(key);
    }
    cmd.envs(profile.env_set.iter().map(|(k, v)| (k, v)));
    cmd.env("DISABLE_AUTOUPDATER", "1").env("GLASSCODE_MANAGER", "1");
    std::fs::create_dir_all(&spec.cwd).map_err(|e| format!("could not make {} ({e})", spec.cwd.display()))?;
    cmd.current_dir(&spec.cwd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    Ok(cmd)
}

/// What makes a program one session's: the brain is no session, and never the person's own.
pub const SESSION_ENV: &[&str] = &["GLASSCODE_SESSION_ID", "GLASSCODE_SESSION_SIG", "GLASSCODE_EVENTS", "GLASSCODE_WORKTREE", "GLASSCODE_LIMITS", "GLASSCODE_STATUSLINE", "GLASSCODE_CLI_FILES", "GLASSCODE_CONTROL_SOCKET"];

/// Write `text` to a file of the run folder that only this user can read (brain-16).
pub(crate) fn private_file(dir: &std::path::Path, name: &str, text: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not make {} ({e})", dir.display()))?;
    let path = dir.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|e| format!("could not write {} ({e})", path.display()))?;
    file.write_all(text.as_bytes()).map_err(|e| format!("could not write {} ({e})", path.display()))?;
    Ok(path)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A driver that plays updates as given: one list per prompt.
    pub struct Played {
        pub turns: Vec<Vec<Update>>,
        pub now: std::collections::VecDeque<Update>,
        pub stopped: bool,
        pub cancelled: bool,
        pub prompts: Vec<String>,
    }

    impl Played {
        pub fn new(turns: Vec<Vec<Update>>) -> Played {
            Played { turns, now: Default::default(), stopped: false, cancelled: false, prompts: Vec::new() }
        }
    }

    impl BrainDriver for Played {
        fn new_session(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn prompt(&mut self, text: &str) -> Result<(), String> {
            self.prompts.push(text.into());
            if self.turns.is_empty() {
                return Err("no more turns".into());
            }
            self.now = self.turns.remove(0).into();
            Ok(())
        }
        fn next(&mut self, wait: Duration) -> Option<Update> {
            match self.now.pop_front() {
                Some(update) => Some(update),
                None => {
                    std::thread::sleep(wait.min(Duration::from_millis(5)));
                    if self.cancelled {
                        return Some(Update::Ended(TurnEnd::Failed("aborted".into())));
                    }
                    None
                }
            }
        }
        fn cancel(&mut self) {
            self.cancelled = true;
        }
        fn stop(&mut self) {
            self.stopped = true;
        }
        fn pid(&self) -> Option<u32> {
            None
        }
        fn session_id(&self) -> Option<String> {
            None
        }
        fn stderr_tail(&self) -> String {
            String::new()
        }
    }

    pub fn only_glasscode(tools: &[String]) -> Result<(), (Vec<String>, Vec<String>)> {
        let extra: Vec<String> = tools.iter().filter(|t| !t.starts_with("mcp__glasscode__") && *t != "ToolSearch").cloned().collect();
        if extra.is_empty() { Ok(()) } else { Err((extra, Vec::new())) }
    }

    fn init(tools: &[&str]) -> Update {
        Update::Init { tools: tools.iter().map(|t| t.to_string()).collect(), model: "haiku".into(), version: "2.1.328".into() }
    }

    #[test]
    fn tool_list_mismatch_stops_brain() {
        let mut driver = Played::new(vec![vec![init(&["Bash", "mcp__glasscode__ui_state"]), Update::ToolCall { name: "Bash".into() }, Update::Ended(TurnEnd::Answered("pushed".into()))]]);
        let mut checked = false;
        let outcome = run_turn(&mut driver, "what is running?", false, &only_glasscode, &mut || checked = true, &mut |_| {}, Limits::default());
        assert!(driver.stopped && !checked && !outcome.checked);
        assert_eq!(outcome.health, Some(BrainHealth::ToolListWrong { extra: vec!["Bash".into()], missing: vec![] }));
        assert!(outcome.tools_called.is_empty(), "stopped before its first tool call");
    }

    #[test]
    fn a_checked_brain_answers_and_its_usage_is_kept() {
        let usage = Usage { input: 100, output: 20, ..Usage::default() };
        let mut driver = Played::new(vec![
            vec![init(&["ToolSearch", "mcp__glasscode__session_list"]), Update::ToolCall { name: "mcp__glasscode__session_list".into() }, Update::Text("Two sessions".into()), Update::Usage(usage), Update::Ended(TurnEnd::Answered("Two sessions run.".into()))],
            vec![Update::Ended(TurnEnd::Answered("Still two.".into()))],
        ]);
        let mut checks = 0;
        let mut seen = 0;
        let first = run_turn(&mut driver, "what is running?", false, &only_glasscode, &mut || checks += 1, &mut |_| seen += 1, Limits::default());
        assert_eq!((first.end.clone(), first.usage, first.checked, checks, seen), (TurnEnd::Answered("Two sessions run.".into()), usage, true, 1, 5));
        let second = run_turn(&mut driver, "and now?", true, &only_glasscode, &mut || checks += 1, &mut |_| {}, Limits::default());
        assert_eq!((second.end, checks), (TurnEnd::Answered("Still two.".into()), 1), "checked once per process");
    }

    #[test]
    fn a_quiet_turn_is_stopped() {
        let mut driver = Played::new(vec![vec![init(&["mcp__glasscode__ui_state"])]]);
        let outcome = run_turn(&mut driver, "x", false, &only_glasscode, &mut || {}, &mut |_| {}, Limits { idle: Duration::from_millis(30), turn: Duration::from_secs(5) });
        assert!(driver.cancelled);
        assert_eq!(outcome.end, TurnEnd::Stopped);
    }

    #[test]
    fn a_failure_says_the_state() {
        let mut driver = Played::new(vec![vec![Update::RateLimit(serde_json::json!({ "rate_limit_info": { "status": "rejected", "rateLimitType": "seven_day" } })), Update::Ended(TurnEnd::Failed("limit".into()))]]);
        let outcome = run_turn(&mut driver, "x", true, &only_glasscode, &mut || {}, &mut |_| {}, Limits::default());
        assert_eq!(outcome.health, Some(BrainHealth::OutOfUsage { until: None }));
        let mut gone = Played::new(vec![]);
        assert!(matches!(run_turn(&mut gone, "x", true, &only_glasscode, &mut || {}, &mut |_| {}, Limits::default()).health, Some(BrainHealth::Crashed { .. })));
    }
}
