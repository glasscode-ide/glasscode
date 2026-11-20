//! The manager's brain runtime (M50; D18): one piece of code that the app runs on its own threads
//! on the Mac, and glasscoded on the server (`Host`: `AppHost`, `DaemonHost`). It starts a brain in
//! its exact profile ([`claude`], [`codex`]), checks its tools on its first turn and lets its MCP
//! server serve it only then, keeps its health ([`health`]) and its token ledger ([`budget`]), wraps
//! outside text as data ([`wrap`]), lays out its prompt for caching ([`layout`]) and restarts a
//! process that has grown too big or old ([`restart_due`]).
//!
//! What it does not do: fall back to a weaker profile or a hidden terminal when a brain cannot
//! start (brain-m1, alt-11). Then the health says why, and everything that needs no model keeps
//! working (the no-model mode).

pub mod budget;
pub mod claude;
pub mod codex;
pub mod driver;
pub mod health;
pub mod lanes;
pub mod queue;
pub mod situation;
pub mod wake;
pub mod wrap;

use std::time::Duration;

use crate::caller::{Caller, Lane};
use crate::model::AgentKind;
use crate::ops::{Op, OpHost, OpReply};
use crate::paths::MachineId;

pub use budget::{Blocked, Ledger, Turn, Usage};
pub use driver::{BrainDriver, BrainSpec, Expected, Limits, Outcome, TurnEnd, Update};
pub use health::BrainHealth;

/// What only the app or only glasscoded can do for the runtime (design section 8).
pub trait Host: Send + Sync {
    fn machine(&self) -> &MachineId;
    /// An op this host runs (Window and SessionHost ops here; Core ones anywhere), for `caller`.
    fn run_op(&self, op: &Op, caller: &Caller) -> OpReply;
    fn has(&self, host: OpHost) -> bool;
    /// One notification about `matter` (a later one about the same matter replaces it).
    fn notify(&self, title: &str, body: &str, matter: &str);
    /// The brain of `lane` (and `wake`) passed its tool-list check: its MCP server's calls are
    /// served from now on (brain-m5).
    fn brain_checked(&self, lane: Lane, wake: Option<&str>);
    /// A card for the person: `op` as a wake proposed it, with its reason; the card's number (a
    /// gate "never" refuses it).
    fn propose(&self, op: &Op, reason: &str, lane: Lane, wake: Option<&str>) -> Result<String, String>;
    /// `op`, which a wake decided on under the playbook that fired it (`lanes::under_playbook`):
    /// through the gate as that playbook's wake, not as a card. A host with no playbooks refuses.
    fn run_under(&self, op: &Op, _caller: &Caller, playbook: &str) -> OpReply {
        OpReply::Failed(crate::ops::OpError::new(crate::ops::ErrorCode::Failed, format!("{} under the playbook {playbook}: this host runs no playbook's ops", op.name())))
    }
}

/// The driver for a brain spec.
pub fn driver_for(spec: BrainSpec) -> Box<dyn BrainDriver> {
    match spec.agent {
        AgentKind::Codex => Box::new(codex::CodexDriver::new(spec)),
        _ => Box::new(claude::ClaudeDriver::new(spec)),
    }
}

/// The tools a Claude brain must have, by its init line: Glasscode's manager tools under Claude's
/// prefix, and ToolSearch (`mcp::init_tools_ok`).
pub fn claude_expected(tools: &[String]) -> Result<(), (Vec<String>, Vec<String>)> {
    if crate::mcp::init_tools_ok(tools, crate::mcp::Profile::Manager) {
        return Ok(());
    }
    let ours: Vec<String> = crate::mcp::tools(crate::mcp::Profile::Manager).iter().filter_map(|t| t["name"].as_str().map(|n| format!("{}{n}", crate::mcp::CLAUDE_PREFIX))).collect();
    let extra: Vec<String> = tools.iter().filter(|t| *t != "ToolSearch" && !ours.contains(t)).cloned().collect();
    let missing: Vec<String> = ours.iter().filter(|o| !tools.contains(o)).cloned().collect();
    Err((extra, missing))
}

/// The brain's prompt, laid out for the vendors' prompt caches (brain-16): the static
/// instructions and the always-loaded memory in the system prompt (the same from turn to turn),
/// the situation and the question in the user turn. Long text goes in files and on stdin, never in
/// argv.
pub fn layout(instructions: &str, memory: &str, situation: &str, question: &str) -> (String, String) {
    let system = if memory.trim().is_empty() { instructions.trim().to_string() } else { format!("{}\n\n# What you remember\n\n{}", instructions.trim(), memory.trim()) };
    let user = if situation.trim().is_empty() { question.trim().to_string() } else { format!("{}\n\n{}", situation.trim(), question.trim()) };
    (system, user)
}

/// When a long-lived brain process starts again (brain-m6): after this many turns, or above this
/// resident memory. It loads its conversation again, so nothing is lost but the cache.
pub const RESTART_AFTER_TURNS: u32 = 200;
pub const RESTART_ABOVE_BYTES: u64 = 800 * 1024 * 1024;
/// An idle lane's process stops after this long, and starts again with its conversation.
pub const IDLE_STOP: Duration = Duration::from_secs(30 * 60);

/// Whether a brain process is due to start again: its turns, and its resident memory now.
pub fn restart_due(turns: u32, rss: Option<u64>) -> bool {
    turns >= RESTART_AFTER_TURNS || rss.is_some_and(|bytes| bytes > RESTART_ABOVE_BYTES)
}

/// The resident memory of a process (bytes), when the system says.
pub fn rss_of(pid: u32) -> Option<u64> {
    let out = crate::sys::command("ps").args(["-o", "rss=", "-p", &pid.to_string()]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().ok().map(|kb| kb * 1024)
}

/// Why a brain did not take a turn.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// It cannot think now (the no-model mode): the chat says so and answers from code.
    Health(BrainHealth),
    /// The budget is spent for now.
    Budget(Blocked),
}

/// One lane's brain: its driver, its health, whether its tools were checked, its turns.
pub struct Brain {
    spec: BrainSpec,
    driver: Box<dyn BrainDriver>,
    pub health: BrainHealth,
    checked: bool,
    turns: u32,
    pub limits: Limits,
}

impl Brain {
    pub fn new(spec: BrainSpec) -> Brain {
        let driver = driver_for(spec.clone());
        Brain { spec, driver, health: BrainHealth::Ready, checked: false, turns: 0, limits: Limits::default() }
    }

    /// For a test: a brain on a driver of the test's own.
    pub fn with_driver(spec: BrainSpec, driver: Box<dyn BrainDriver>) -> Brain {
        Brain { spec, driver, health: BrainHealth::Ready, checked: false, turns: 0, limits: Limits::default() }
    }

    /// The conversation's id, to load it again after a restart of the app.
    pub fn session_id(&self) -> Option<String> {
        self.driver.session_id()
    }

    /// The person's Stop.
    pub fn cancel(&mut self) {
        self.driver.cancel();
    }

    /// End its process (its conversation is kept).
    pub fn stop(&mut self) {
        self.driver.stop();
        self.checked = false;
    }

    /// Set its state, and when it changed, tell the journal and the person once (brain-14).
    pub fn set_health(&mut self, host: &dyn Host, after: BrainHealth) {
        if health::change(&self.health, &after).is_some() {
            crate::journal::append(health::event(&after, self.spec.agent, self.spec.lane));
            let title = if after == BrainHealth::Ready { "The manager thinks again" } else { "The manager cannot think" };
            host.notify(title, &after.sentence(self.spec.agent), "brain-health");
        }
        self.health = after;
    }

    /// One turn: refused when it cannot think or the budget is spent (`ledger`, read by the
    /// caller); a process that has grown too old or big starts again with its conversation; the
    /// tools are checked on the first turn of each process, and only then does `host` serve its MCP
    /// calls. The ledger gets what it used.
    pub fn ask(&mut self, host: &dyn Host, ledger: &mut Ledger, text: &str, turn: Turn, now_ms: u64, on_update: &mut dyn FnMut(&Update)) -> Result<Outcome, Refusal> {
        if !self.health.can_think() && self.health.backoff().is_none() {
            return Err(Refusal::Health(self.health.clone()));
        }
        ledger.allow(turn, now_ms).map_err(Refusal::Budget)?;
        if turn != Turn::Chat {
            ledger.record_wake(now_ms);
        }
        if restart_due(self.turns, self.driver.pid().and_then(rss_of)) {
            self.spec.resume = self.driver.session_id();
            self.driver.stop();
            self.driver = driver_for(self.spec.clone());
            self.checked = false;
            self.turns = 0;
        }
        let (lane, wake) = (self.spec.lane, self.spec.wake.clone());
        if self.spec.agent == AgentKind::Codex && !self.checked {
            // Codex prints no tool list: its whole list is read from ALL_TOOLS, on its first turn.
            if let Err(state) = self.codex_check(host) {
                self.set_health(host, state.clone());
                return Err(Refusal::Health(state));
            }
        }
        let mut checked_now = false;
        let expected: Expected = if self.spec.agent == AgentKind::Codex { &|_| Ok(()) } else { &claude_expected };
        let outcome = driver::run_turn(self.driver.as_mut(), text, self.checked, expected, &mut || checked_now = true, &mut |update| {
            if let Update::RateLimit(event) = update {
                ledger.record_window(event);
            }
            on_update(update);
        }, self.limits);
        if checked_now {
            host.brain_checked(lane, wake.as_deref());
        }
        self.checked = outcome.checked;
        self.turns += 1;
        ledger.record(lane, outcome.usage, now_ms);
        let after = match (&outcome.health, &outcome.end) {
            (Some(state), _) => state.clone(),
            (None, TurnEnd::Failed(why)) if self.driver.pid().is_none() => BrainHealth::Crashed { why: why.clone() },
            _ => BrainHealth::Ready,
        };
        self.set_health(host, after);
        Ok(outcome)
    }

    /// A Codex brain's first turn: its `ALL_TOOLS`, checked against Glasscode's tools.
    fn codex_check(&mut self, host: &dyn Host) -> Result<(), BrainHealth> {
        let outcome = driver::run_turn(self.driver.as_mut(), codex::ALL_TOOLS_PROMPT, true, &|_| Ok(()), &mut || {}, &mut |_| {}, self.limits);
        let TurnEnd::Answered(answer) = outcome.end else { return Err(outcome.health.unwrap_or(BrainHealth::Crashed { why: "it could not list its tools".into() })) };
        let ours: Vec<String> = crate::mcp::tools(crate::mcp::Profile::Manager).iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect();
        let tools = codex::all_tools_of(&answer).ok_or(BrainHealth::ToolListWrong { extra: vec!["(its list could not be read)".into()], missing: vec![] })?;
        codex::check_all_tools(&tools, &ours).map_err(|(extra, missing)| {
            self.driver.stop();
            BrainHealth::ToolListWrong { extra, missing }
        })?;
        self.checked = true;
        host.brain_checked(self.spec.lane, self.spec.wake.as_deref());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host that records what the runtime asks of it.
    #[derive(Default)]
    struct Seen {
        notices: std::sync::Mutex<Vec<String>>,
        checked: std::sync::Mutex<u32>,
        machine: Option<MachineId>,
    }

    impl Host for Seen {
        fn machine(&self) -> &MachineId {
            self.machine.as_ref().unwrap()
        }
        fn run_op(&self, _: &Op, _: &Caller) -> OpReply {
            OpReply::Done { text: String::new(), data: serde_json::Value::Null }
        }
        fn has(&self, _: OpHost) -> bool {
            true
        }
        fn notify(&self, _: &str, body: &str, _: &str) {
            self.notices.lock().unwrap().push(body.into());
        }
        fn brain_checked(&self, _: Lane, _: Option<&str>) {
            *self.checked.lock().unwrap() += 1;
        }
        fn propose(&self, _: &Op, _: &str, _: Lane, _: Option<&str>) -> Result<String, String> {
            Ok("p1".into())
        }
    }

    fn spec(agent: AgentKind) -> BrainSpec {
        BrainSpec { agent, lane: Lane::Chat, wake: None, program: "x".into(), glasscode: "g".into(), model: "m".into(), effort: None, fallback_model: None, cwd: "/w".into(), run_dir: "/r".into(), env: Default::default(), instructions: String::new(), brain_env: vec![], resume: None }
    }

    fn ours() -> Vec<String> {
        crate::mcp::tools(crate::mcp::Profile::Manager).iter().map(|t| format!("{}{}", crate::mcp::CLAUDE_PREFIX, t["name"].as_str().unwrap())).collect()
    }

    #[test]
    fn a_brain_is_served_only_once_checked_and_says_its_health_once() {
        crate::paths::use_test_root();
        let init = Update::Init { tools: ours(), model: "m".into(), version: "2.1.328".into() };
        let played = driver::tests::Played::new(vec![
            vec![init, Update::Usage(Usage { input: 50, ..Usage::default() }), Update::Ended(TurnEnd::Answered("Two run.".into()))],
            vec![Update::Ended(TurnEnd::Failed("Please run /login".into()))],
            vec![Update::Ended(TurnEnd::Failed("Please run /login".into()))],
        ]);
        let mut brain = Brain::with_driver(spec(AgentKind::Claude), Box::new(played));
        let host = Seen { machine: MachineId::parse("0123456789abcdef"), ..Seen::default() };
        let mut ledger = Ledger::default();
        let first = brain.ask(&host, &mut ledger, "what is running?", Turn::Chat, 1_795_000_000_000, &mut |_| {}).unwrap();
        assert_eq!((first.end, *host.checked.lock().unwrap(), ledger.today(1_795_000_000_000)["chat"]), (TurnEnd::Answered("Two run.".into()), 1, 50));
        brain.ask(&host, &mut ledger, "and now?", Turn::Chat, 1_795_000_000_000, &mut |_| {}).unwrap();
        assert_eq!(brain.health, BrainHealth::SignedOut);
        // Signed out is the person's to end: no further turn, and one notice in all.
        assert_eq!(brain.ask(&host, &mut ledger, "again?", Turn::Chat, 1_795_000_000_000, &mut |_| {}), Err(Refusal::Health(BrainHealth::SignedOut)));
        assert_eq!(host.notices.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_codex_brain_lists_its_tools_first() {
        crate::paths::use_test_root();
        let names: Vec<String> = crate::mcp::tools(crate::mcp::Profile::Manager).iter().map(|t| format!("mcp__glasscode__{}", t["name"].as_str().unwrap())).collect();
        let mut all = vec!["exec".to_string(), "wait".to_string()];
        all.extend(names);
        let list = serde_json::to_string(&all).unwrap();
        let played = driver::tests::Played::new(vec![vec![Update::Ended(TurnEnd::Answered(list))], vec![Update::Ended(TurnEnd::Answered("Two run.".into()))]]);
        let mut brain = Brain::with_driver(spec(AgentKind::Codex), Box::new(played));
        let host = Seen { machine: MachineId::parse("0123456789abcdef"), ..Seen::default() };
        let outcome = brain.ask(&host, &mut Ledger::default(), "what is running?", Turn::Chat, 1, &mut |_| {}).unwrap();
        assert_eq!((outcome.end, *host.checked.lock().unwrap()), (TurnEnd::Answered("Two run.".into()), 1));
        let shell = driver::tests::Played::new(vec![vec![Update::Ended(TurnEnd::Answered("[\"exec\",\"exec_command\"]".into()))]]);
        let mut brain = Brain::with_driver(spec(AgentKind::Codex), Box::new(shell));
        assert!(matches!(brain.ask(&host, &mut Ledger::default(), "x", Turn::Chat, 1, &mut |_| {}), Err(Refusal::Health(BrainHealth::ToolListWrong { .. }))));
    }

    #[test]
    fn a_spent_budget_refuses_a_wake() {
        let mut brain = Brain::with_driver(spec(AgentKind::Claude), Box::new(driver::tests::Played::new(vec![])));
        let host = Seen { machine: MachineId::parse("0123456789abcdef"), ..Seen::default() };
        let mut ledger = Ledger { daily_cap: 10, ..Ledger::default() };
        ledger.record(Lane::Wake, Usage { input: 10, ..Usage::default() }, 5);
        ledger.record_window_at(&serde_json::json!({ "rate_limit_info": { "status": "allowed" } }), 5);
        assert!(matches!(brain.ask(&host, &mut ledger, "x", Turn::Wake, 5, &mut |_| {}), Err(Refusal::Budget(Blocked::DailyCap { .. }))));
    }

    #[test]
    fn the_prompt_is_laid_out_for_the_cache() {
        let (system, user) = layout("You are the manager.", "Sam reviews the API.", "Two sessions run.", "What is running?");
        assert_eq!(system, "You are the manager.\n\n# What you remember\n\nSam reviews the API.");
        assert_eq!(user, "Two sessions run.\n\nWhat is running?");
        // The same instructions and memory give the same system prompt, whatever the question.
        assert_eq!(layout("You are the manager.", "Sam reviews the API.", "", "Anything else?").0, system);
        assert_eq!(layout("I", "", "", "Q"), ("I".to_string(), "Q".to_string()));
    }

    #[test]
    fn a_big_or_old_process_restarts() {
        assert!(!restart_due(10, Some(200 * 1024 * 1024)));
        assert!(restart_due(RESTART_AFTER_TURNS, None));
        assert!(restart_due(1, Some(RESTART_ABOVE_BYTES + 1)));
        assert!(rss_of(std::process::id()).is_some_and(|bytes| bytes > 0) || cfg!(windows));
    }

    #[test]
    fn claude_must_have_exactly_glasscodes_tools() {
        let ours: Vec<String> = crate::mcp::tools(crate::mcp::Profile::Manager).iter().map(|t| format!("{}{}", crate::mcp::CLAUDE_PREFIX, t["name"].as_str().unwrap())).collect();
        let mut with_search = ours.clone();
        with_search.push("ToolSearch".into());
        assert_eq!(claude_expected(&with_search), Ok(()));
        let mut with_bash = with_search.clone();
        with_bash.push("Bash".into());
        assert_eq!(claude_expected(&with_bash), Err((vec!["Bash".into()], vec![])));
        let (_, missing) = claude_expected(&with_search[1..]).unwrap_err();
        assert_eq!(missing, vec![ours[0].clone()]);
    }
}
