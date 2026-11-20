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
pub mod wrap;

use std::time::Duration;

use crate::caller::{Caller, Lane};
use crate::model::AgentKind;
use crate::ops::{Op, OpHost, OpReply};
use crate::paths::MachineId;

pub use budget::{Blocked, Ledger, Turn, Usage};
pub use driver::{BrainDriver, BrainSpec, Limits, Outcome, TurnEnd, Update};
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

#[cfg(test)]
mod tests {
    use super::*;

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
