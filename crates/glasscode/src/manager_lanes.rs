//! The manager's lanes in the app (M51), with the engine at v2 and the manager on: the wake lane's
//! worker, on a thread of its own, and what a lane's brain starts with (the chat lane's is M52's).
//!
//! The worker reads the journal from where it started, makes the wakes the events call for
//! (`runtime::lanes::wake_for_event`), and runs the queue as the budget lets it: each wake on a
//! fresh brain of Claude's cheapest model, its situation holding only its projects, its answer read
//! by Rust. Before a wake runs, what it is about is read again from the events since; a stale one
//! is dropped with a journal line. A line it tells goes into the manager's conversation; a notice
//! is a notification; an op it proposes is a card (posted in the conversation by the gate's ask),
//! unless the playbook that fired it was approved to act. While the person's chat turn runs, the
//! worker holds, and the gate refuses a wake's action.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use glasscode_core::caller::Lane;
use glasscode_core::conversation::{Author, Conversation, Draft, Kind, Stream};
use glasscode_core::journal::Event;
use glasscode_core::model::AgentKind;
use glasscode_core::runtime::lanes::{self, Hold, Locks};
use glasscode_core::runtime::queue::{Queue, Wake};
use glasscode_core::runtime::situation::{Fact, Project};
use glasscode_core::runtime::wake::Effect;
use glasscode_core::runtime::{Brain, BrainSpec, Host, Ledger};
use glasscode_core::store::Store as _;

use crate::workspace::Workspace;

/// The lanes' shared state: the queue, the hold, the claims, and what the worker reads of the app.
#[derive(Default)]
pub struct Lanes {
    pub queue: Arc<Mutex<Queue>>,
    pub hold: Hold,
    pub locks: Locks,
    /// The running worker's stop flag (none: no worker).
    stop: Option<Arc<AtomicBool>>,
    /// The projects as a wake's situation sees them, kept as the policy changes.
    pub projects: Arc<Mutex<Vec<Project>>>,
    /// The projects each running wake holds (by its id), for the gate (trust-9).
    wakes: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl Lanes {
    pub fn running(&self) -> bool {
        self.stop.is_some()
    }

    /// The projects a wake of the brain holds: none for one the worker is not running.
    pub fn wake_projects(&self, wake: &str) -> Vec<String> {
        self.wakes.lock().unwrap_or_else(|p| p.into_inner()).get(wake).cloned().unwrap_or_default()
    }
}

/// The model of a lane's brain when the manager's settings name none (brain-5: always one, never
/// the person's own default).
fn default_model(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Codex => "gpt-5.6-luna",
        _ => "sonnet",
    }
}

/// A brain's MCP server environment: its lane and wake, signed by the app, and this app's home
/// and control socket, so that its calls reach this Glasscode and no other (the brain's own
/// process has neither: `runtime::driver::SESSION_ENV`).
fn brain_env(lane: Lane, wake: Option<&str>, sign: impl Fn(&str) -> String) -> Vec<(String, String)> {
    let text = glasscode_core::mcp::brain_text(lane, wake);
    vec![
        (glasscode_core::mcp::BRAIN_LANE.to_string(), if lane == Lane::Chat { "chat" } else { "wake" }.to_string()),
        (glasscode_core::mcp::BRAIN_WAKE.to_string(), wake.unwrap_or_default().to_string()),
        (glasscode_core::mcp::BRAIN_SIG.to_string(), sign(&text)),
        ("GLASSCODE_HOME".to_string(), glasscode_core::paths::root().to_string_lossy().into_owned()),
        ("GLASSCODE_CONTROL_SOCKET".to_string(), glasscode_core::control::socket_path().to_string_lossy().into_owned()),
    ]
}

impl Workspace {
    /// What a lane's brain starts with: the manager's agent and account, its model, its own
    /// folders, Glasscode's MCP server signed for the lane. None when the agent is not installed.
    pub(crate) fn brain_spec(&self, lane: Lane, wake: Option<&str>) -> Option<BrainSpec> {
        let settings = &self.state.manager;
        let agent = settings.brain;
        let account = settings
            .account
            .as_deref()
            .and_then(|wanted| self.state.accounts_for(agent).find(|a| a.id == wanted || a.label.eq_ignore_ascii_case(wanted)))
            .or_else(|| self.state.default_account(agent));
        let mut env: HashMap<String, String> = (*self.env).clone();
        if let Some(dir) = account.and_then(|a| a.config_dir()) {
            env.insert(if agent == AgentKind::Codex { "CODEX_HOME" } else { "CLAUDE_CONFIG_DIR" }.into(), dir.to_string_lossy().into_owned());
        }
        let program = glasscode_core::agents::resolve_program(if agent == AgentKind::Codex { "codex" } else { "claude" }, &env)?;
        Some(BrainSpec {
            agent,
            lane,
            wake: wake.map(str::to_string),
            program,
            glasscode: std::env::current_exe().ok()?,
            model: settings.model.clone().filter(|m| !m.trim().is_empty()).unwrap_or_else(|| default_model(agent).to_string()),
            effort: None,
            fallback_model: None,
            cwd: glasscode_core::paths::manager_work_dir(),
            run_dir: glasscode_core::paths::manager_run_dir(),
            env,
            instructions: lanes::CHAT_INSTRUCTIONS.to_string(),
            brain_env: brain_env(lane, wake, |text| self.control.signature(text)),
            resume: None,
        })
    }

    /// The projects as a wake's situation sees them: their keys and names; Careful ones; and
    /// whether the manager's account may read them (D17: a project bound to another account is
    /// none of its business).
    fn lanes_projects(&self) -> Vec<Project> {
        let set = self.control_policy_set();
        self.state
            .visible_projects()
            .filter(|p| !self.state.manager.off_projects.contains(&p.id))
            .filter_map(|p| {
                let key = self.control.project_keys.get(&p.id)?;
                let policy = set.policy.of(key);
                let visible = policy.model_account.as_deref().is_none_or(|only| set.manager_account.as_deref() == Some(only));
                Some(Project { id: key.as_str().to_string(), name: p.name.clone(), careful: policy.level == glasscode_core::gate::Level::Careful, visible })
            })
            .collect()
    }

    /// Keep the projects a wake may be told of as the policy is now.
    pub(crate) fn lanes_projects_refresh(&mut self) {
        if self.lanes.running() {
            *self.lanes.projects.lock().unwrap_or_else(|p| p.into_inner()) = self.lanes_projects();
        }
    }

    /// Start or stop the wake lane's worker as the settings say: it runs with the engine at v2,
    /// the manager on and a Claude brain (D6: no Codex wake lane in release 1). A change of the
    /// manager's settings starts it again, with them.
    pub(crate) fn lanes_refresh(&mut self) {
        // The chat's brain starts again with the settings too, at its next question.
        self.chat_restart();
        if let Some(stop) = self.lanes.stop.take() {
            stop.store(true, Ordering::SeqCst);
        }
        if !self.control.engine_v2 || !self.state.manager.enabled {
            return;
        }
        let Some(host) = self.brain_host.clone() else { return };
        let Some(base) = self.brain_spec(Lane::Wake, None).filter(|s| s.agent == AgentKind::Claude) else { return };
        *self.lanes.projects.lock().unwrap_or_else(|p| p.into_inner()) = self.lanes_projects();
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            queue: self.lanes.queue.clone(),
            hold: self.lanes.hold.clone(),
            locks: self.lanes.locks.clone(),
            stop: stop.clone(),
            projects: self.lanes.projects.clone(),
            wakes: self.lanes.wakes.clone(),
            base,
            signer: self.control.signer(),
            health: (glasscode_core::runtime::BrainHealth::Ready, 0),
        };
        if std::thread::Builder::new().name("manager-wakes".into()).spawn(move || worker.run(host.as_ref())).is_ok() {
            self.lanes.stop = Some(stop);
        }
    }

    /// A wake asked for by `manager.wake` (a schedule, a command) or a playbook's firing: into the
    /// queue.
    pub(crate) fn lanes_wake(&mut self, wake: Wake) {
        let now = glasscode_core::clock::now_ms();
        self.lanes.queue.lock().unwrap_or_else(|p| p.into_inner()).push(wake, now);
    }
}

/// The wake lane's worker.
struct Worker {
    queue: Arc<Mutex<Queue>>,
    hold: Hold,
    locks: Locks,
    stop: Arc<AtomicBool>,
    projects: Arc<Mutex<Vec<Project>>>,
    wakes: Arc<Mutex<HashMap<String, Vec<String>>>>,
    base: BrainSpec,
    signer: crate::control_ui::Signer,
    /// The last wake brain's health, and when it was read: the next wake starts from it (a
    /// signed-out account is not tried, nor told, on every wake), and tries again after a while.
    health: (glasscode_core::runtime::BrainHealth, u64),
}

/// The newest events the worker keeps, for the staleness of its wakes.
const SEEN: usize = 500;
/// How often the worker looks at the journal and the queue when there is nothing to run.
const POLL: Duration = Duration::from_secs(2);
/// How long a brain that cannot think is believed before a wake tries it again (and, if it still
/// cannot, says so once more).
const REPROBE_MS: u64 = 30 * 60 * 1000;

/// The events after a wake's own (all of them when none of its events is kept).
fn after<'a>(seen: &'a [Event], wake: &Wake) -> &'a [Event] {
    match seen.iter().rposition(|e| wake.events.contains(&e.id)) {
        Some(at) => &seen[at + 1..],
        None if wake.events.is_empty() => &[],
        None => seen,
    }
}

/// A nudge for this matter is open in the conversation already: a later wake does not post it
/// again.
fn nudge_open(chat: &Conversation, matter: &str) -> bool {
    chat.items().iter().any(|item| !item.closed && matches!(&item.kind, Kind::Nudge { matter: open } if open == matter))
}

impl Worker {
    fn run(mut self, host: &dyn Host) {
        let mut cursor = glasscode_core::journal::end();
        let mut seen: Vec<Event> = Vec::new();
        while !self.stop.load(Ordering::SeqCst) {
            let now = glasscode_core::clock::now_ms();
            let (events, next) = glasscode_core::journal::read_from(&cursor, 200);
            cursor = next;
            for event in events {
                if let Some(wake) = lanes::wake_for_event(&event, now) {
                    self.queue.lock().unwrap_or_else(|p| p.into_inner()).push(wake, now);
                }
                seen.push(event);
            }
            if seen.len() > SEEN {
                seen.drain(..seen.len() - SEEN);
            }
            let ledger = Ledger::load().unwrap_or_default();
            let (wake, dropped) = lanes::next_wake(&mut self.queue.lock().unwrap_or_else(|p| p.into_inner()), &self.hold, &ledger, now, &|w| lanes::still_holds(&w.precondition, after(&seen, w)));
            for gone in &dropped {
                glasscode_core::journal::append(lanes::dropped_event(gone));
            }
            match wake {
                // One that did not start goes back, and the worker waits before the next look.
                Some(wake) => {
                    if let Some(again) = self.run_wake(host, wake, now) {
                        self.queue.lock().unwrap_or_else(|p| p.into_inner()).push(again, now);
                        std::thread::sleep(POLL);
                    }
                }
                None => std::thread::sleep(POLL),
            }
        }
    }

    /// Run one wake; the wake back when it did not start.
    fn run_wake(&mut self, host: &dyn Host, wake: Wake, now: u64) -> Option<Wake> {
        let id = glasscode_core::model::new_id();
        let mut spec = self.base.clone();
        spec.brain_env = brain_env(Lane::Wake, Some(&id), |text| self.signer.sign(text));
        let spec = lanes::wake_spec(&spec, &id)?;
        // What its events said, kept with it, and what is open, from the work ledger: the wake
        // carries no history (M51 step 2).
        let mut facts: Vec<Fact> = wake.facts.clone();
        match glasscode_core::work::WorkLedger::load() {
            Ok(ledger) => facts.extend(glasscode_core::runtime::situation::work_facts(&ledger)),
            Err(err) => log::warn!("manager: the work ledger could not be read for a wake: {err}"),
        }
        let projects = self.projects.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let situation = glasscode_core::runtime::situation::for_wake(&wake, &facts, &projects);
        // What its tools may reach: the projects its situation holds (trust-9, in the gate).
        let own: Vec<String> = glasscode_core::runtime::situation::projects_of(&wake, &projects).iter().map(|p| p.id.clone()).collect();
        self.wakes.lock().unwrap_or_else(|p| p.into_inner()).insert(id.clone(), own);
        // It acts only under the playbook that fired it, while that is approved to act.
        let under = match glasscode_core::playbooks::load(&glasscode_core::playbooks::model::journal_schema()) {
            Ok(book) => lanes::under_playbook(&wake, &book),
            Err(err) => {
                log::warn!("manager: the playbooks could not be read, so the wake only proposes: {err}");
                None
            }
        };
        // The lane's own copy of the ledger; what it records joins the stored one after.
        let before = Ledger::load().unwrap_or_default();
        let mut ledger = before.clone();
        let mut brain = Brain::new(spec);
        let believed = !self.health.0.can_think() && now.saturating_sub(self.health.1) < REPROBE_MS;
        if believed {
            brain.health = self.health.0.clone();
        }
        let run = lanes::run_wake(host, &mut ledger, &mut brain, wake, &situation, under.as_deref(), now);
        if !believed || brain.health != self.health.0 {
            self.health = (brain.health.clone(), now);
        }
        if let Err(err) = glasscode_core::store::update::<Ledger, _>(|stored| stored.add_since(&before, &ledger)) {
            log::warn!("manager: the token ledger could not be kept: {err}");
        }
        if let Some(why) = &run.why_not {
            log::info!("manager: the wake {} did nothing: {why}", run.wake.key);
        }
        if run.retry {
            self.wakes.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
            return Some(run.wake);
        }
        let mut chat = Conversation::load();
        let post = |chat: &mut Conversation, kind: Kind, text: String| {
            if let Err(err) = chat.post(Draft { author: Author::Wake(id.clone()), kind, text, refs: Vec::new(), topic: None }) {
                log::warn!("manager: a wake's line could not be posted: {err}");
            }
        };
        for effect in run.effects {
            match effect {
                Effect::Tell(text) => post(&mut chat, Kind::Manager { state: Stream::Done }, text),
                Effect::Notify(notice) => host.notify(&notice.title, &notice.body, &run.wake.key),
                Effect::Matter(matter) => {
                    if !nudge_open(&chat, &matter.key) {
                        post(&mut chat, Kind::Nudge { matter: matter.key }, matter.summary);
                    }
                }
                // The gate's ask posts the card; a refusal says why here.
                Effect::Card { op, reason } => {
                    if let Err(why) = host.propose(&op, &reason, Lane::Wake, Some(&id)) {
                        post(&mut chat, Kind::System, format!("A proposal to {} was not made: {why}. ({reason})", op.spec.summary));
                    }
                }
                Effect::Run { op, playbook, reason } => {
                    let caller = glasscode_core::caller::Caller::Manager { lane: Lane::Wake, wake: Some(id.clone()), project: None };
                    let outcome = match host.run_under(&op, &caller, &playbook) {
                        glasscode_core::ops::OpReply::Done { text, .. } => text,
                        glasscode_core::ops::OpReply::Waiting { summary, .. } => format!("waiting for you: {summary}"),
                        glasscode_core::ops::OpReply::Failed(error) => error.text,
                    };
                    post(&mut chat, Kind::System, format!("Under the playbook {playbook}, {}: {reason}. {outcome}", op.spec.summary));
                }
                Effect::Refused { op, why } => log::info!("manager: a wake proposed {op}, which does nothing: {why}"),
            }
        }
        // The wake's turn ended: what it worked on is free for the chat, and its tools reach
        // nothing more.
        self.locks.release(Lane::Wake);
        self.wakes.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Staleness reads the events after a wake's own: all kept ones when its own are gone, none
    /// when it has none.
    #[test]
    fn a_wake_is_judged_by_what_came_after_it() {
        let event = |id: &str| {
            let mut e = Event::new(glasscode_core::journal_kinds::CI_PASSED, glasscode_core::journal::Source::Forge, "x");
            e.id = id.into();
            e
        };
        let seen = vec![event("a"), event("b"), event("c")];
        let mut wake = Wake::new("ci:x#1", glasscode_core::runtime::queue::Priority::Urgent, 0);
        assert!(after(&seen, &wake).is_empty());
        wake.events.push("b".into());
        assert_eq!(after(&seen, &wake).iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["c"]);
        wake.events = vec!["gone".into()];
        assert_eq!(after(&seen, &wake).len(), 3);
    }
}
