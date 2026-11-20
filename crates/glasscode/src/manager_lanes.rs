//! The manager's lanes in the app (M51), with the engine at v2 and the manager on: the wake lane's
//! worker, on a thread of its own, and what the chat lane (M52) starts its brain with.
//!
//! The worker reads the journal from where it started, makes the wakes the events call for
//! (`runtime::lanes::wake_for_event`), and runs the queue: each wake on a fresh brain of the
//! cheapest model, its situation holding only its projects, its answer read by Rust. What comes of
//! it goes into the manager's conversation (a line, a nudge, a card) and to notifications; an op it
//! proposes is a card, unless a playbook the person approved covers it. While the person's chat
//! turn runs, the worker holds.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{Context, Window};
use glasscode_core::caller::Lane;
use glasscode_core::conversation::{Author, Conversation, Draft, Kind, Stream};
use glasscode_core::model::AgentKind;
use glasscode_core::runtime::lanes::{self, Hold, Locks};
use glasscode_core::runtime::queue::{Precondition, Queue, Wake};
use glasscode_core::runtime::situation::{Fact, Project};
use glasscode_core::runtime::wake::Effect;
use glasscode_core::runtime::{Brain, BrainSpec, Host, Ledger};
use glasscode_core::store::Store as _;

use crate::workspace::Workspace;

/// The lanes' shared state: the queue, the hold, the locks, and what the worker reads of the app.
#[derive(Default)]
pub struct Lanes {
    pub queue: Arc<Mutex<Queue>>,
    pub hold: Hold,
    pub locks: Locks,
    stop: Arc<AtomicBool>,
    pub running: bool,
    /// The projects as the situation sees them, refreshed by the app.
    pub projects: Arc<Mutex<Vec<Project>>>,
    /// The playbooks the person approved, and their ops.
    pub approved: Arc<Mutex<Vec<(String, String)>>>,
}

/// The default model of a lane's brain when the manager's settings name none (brain-5: always
/// one, never the person's own default).
fn default_model(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Codex => "gpt-5.6-luna",
        _ => "sonnet",
    }
}

impl Workspace {
    /// What a lane's brain is started with: the manager's agent and account, its model, its own
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
        let text = glasscode_core::mcp::brain_text(lane, wake);
        let lane_word = if lane == Lane::Chat { "chat" } else { "wake" };
        let brain_env = vec![
            (glasscode_core::mcp::BRAIN_LANE.to_string(), lane_word.to_string()),
            (glasscode_core::mcp::BRAIN_WAKE.to_string(), wake.unwrap_or_default().to_string()),
            (glasscode_core::mcp::BRAIN_SIG.to_string(), self.control.signature(&text)),
        ];
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
            brain_env,
            resume: None,
        })
    }

    /// The projects as a wake's situation sees them: their keys, names, whether they are Careful.
    fn lanes_projects(&self) -> Vec<Project> {
        let policy = self.control_policy_set().policy;
        self.state
            .visible_projects()
            .filter_map(|p| {
                let key = self.control.project_keys.get(&p.id)?;
                Some(Project { id: key.as_str().to_string(), name: p.name.clone(), careful: policy.of(key).level == glasscode_core::gate::Level::Careful, visible: true })
            })
            .collect()
    }

    /// Start the wake lane's worker, with the engine at v2 and the manager on, once.
    pub(crate) fn lanes_start(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        if self.lanes.running || !self.control.engine_v2 || !self.state.manager.enabled {
            return;
        }
        let Some(host) = self.brain_host.clone() else { return };
        // The wake lane is Claude's (D6); with a Codex brain it stays off in release 1.
        let Some(base) = self.brain_spec(Lane::Wake, None).filter(|s| s.agent == AgentKind::Claude) else { return };
        *self.lanes.projects.lock().unwrap_or_else(|p| p.into_inner()) = self.lanes_projects();
        self.lanes.running = true;
        self.lanes.stop.store(false, Ordering::SeqCst);
        let worker = Worker {
            queue: self.lanes.queue.clone(),
            hold: self.lanes.hold.clone(),
            stop: self.lanes.stop.clone(),
            projects: self.lanes.projects.clone(),
            approved: self.lanes.approved.clone(),
            base,
            signer: self.control.signer(),
        };
        let _ = std::thread::Builder::new().name("manager-wakes".into()).spawn(move || worker.run(host.as_ref()));
    }

    /// Stop the wake lane (the manager was switched off, or the app quits).
    pub(crate) fn lanes_stop(&mut self) {
        self.lanes.stop.store(true, Ordering::SeqCst);
        self.lanes.running = false;
    }

    /// A wake asked for by `manager.wake` (a playbook, or a command): into the queue.
    pub(crate) fn lanes_wake(&mut self, wake: Wake) {
        let now = glasscode_core::clock::now_ms();
        self.lanes.queue.lock().unwrap_or_else(|p| p.into_inner()).push(wake, now);
    }
}

/// The wake lane's worker.
struct Worker {
    queue: Arc<Mutex<Queue>>,
    hold: Hold,
    stop: Arc<AtomicBool>,
    projects: Arc<Mutex<Vec<Project>>>,
    approved: Arc<Mutex<Vec<(String, String)>>>,
    base: BrainSpec,
    signer: crate::control_ui::Signer,
}

/// The newest events the worker keeps, for the situations of its wakes.
const SEEN: usize = 500;

impl Worker {
    fn run(self, host: &dyn Host) {
        let mut cursor = glasscode_core::journal::end();
        let mut seen: VecDeque<glasscode_core::journal::Event> = VecDeque::new();
        while !self.stop.load(Ordering::SeqCst) {
            let now = glasscode_core::clock::now_ms();
            let (events, next) = glasscode_core::journal::read_from(&cursor, 200);
            cursor = next;
            for event in events {
                if let Some(wake) = lanes::wake_for_event(&event, now) {
                    self.queue.lock().unwrap_or_else(|p| p.into_inner()).push(wake, now);
                }
                seen.push_back(event);
                if seen.len() > SEEN {
                    seen.pop_front();
                }
            }
            // An approval stays worth a look; what else held when the wake was made is taken as
            // still holding (the situation tells the brain what changed since).
            let (wake, dropped) = lanes::next_wake(&mut self.queue.lock().unwrap_or_else(|p| p.into_inner()), &self.hold, now, &|_: &Precondition| true);
            for gone in dropped {
                log::info!("manager: a wake was dropped ({gone:?})");
            }
            match wake {
                Some(wake) => self.run_wake(host, wake, &seen, now),
                None => std::thread::sleep(Duration::from_secs(2)),
            }
        }
    }

    fn run_wake(&self, host: &dyn Host, wake: Wake, seen: &VecDeque<glasscode_core::journal::Event>, now: u64) {
        let id = glasscode_core::model::new_id();
        let mut spec = self.base.clone();
        let text = glasscode_core::mcp::brain_text(Lane::Wake, Some(&id));
        spec.brain_env = vec![
            (glasscode_core::mcp::BRAIN_LANE.to_string(), "wake".to_string()),
            (glasscode_core::mcp::BRAIN_WAKE.to_string(), id.clone()),
            (glasscode_core::mcp::BRAIN_SIG.to_string(), self.signer.sign(&text)),
        ];
        let Some(spec) = lanes::wake_spec(&spec, &id) else { return };
        let facts: Vec<Fact> = seen
            .iter()
            .filter(|e| wake.events.contains(&e.id))
            .map(|e| Fact { project: e.project.as_ref().map(|p| p.as_str().to_string()), source: e.kind.as_str().to_string(), text: e.summary.clone(), outside: !matches!(e.source, glasscode_core::journal::Source::App | glasscode_core::journal::Source::Control), order: 9 })
            .collect();
        let projects = self.projects.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let situation = glasscode_core::runtime::situation::for_wake(&wake, &facts, &projects);
        let approved = self.approved.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let mut ledger = Ledger::load().unwrap_or_default();
        let mut brain = Brain::new(spec);
        let run = lanes::run_wake(host, &mut ledger, &mut brain, wake, &situation, &approved, now);
        let _ = glasscode_core::store::update::<Ledger, _>(|stored| *stored = ledger.clone());
        if let Some(why) = &run.why_not {
            log::info!("manager: a wake ({}) did nothing: {why}", run.wake.key);
        }
        let mut chat = Conversation::load();
        let post = |chat: &mut Conversation, kind: Kind, text: String| {
            let _ = chat.post(Draft { author: Author::Wake(id.clone()), kind, text, refs: Vec::new(), topic: None });
        };
        for effect in run.effects {
            match effect {
                Effect::Tell(text) => post(&mut chat, Kind::Manager { state: Stream::Done }, text),
                Effect::Notify(notice) => host.notify(&notice.title, &notice.body, &run.wake.key),
                Effect::Matter(matter) => post(&mut chat, Kind::Nudge { matter: matter.key.clone() }, matter.summary),
                Effect::Card { op, reason } => match host.propose(&op, &reason, Lane::Wake, Some(&id)) {
                    Ok(pending) => post(&mut chat, Kind::Card { pending }, reason),
                    Err(why) => post(&mut chat, Kind::System, format!("A proposal was refused ({why}): {reason}")),
                },
                Effect::Run { op, playbook, reason } => {
                    let caller = glasscode_core::caller::Caller::Manager { lane: Lane::Wake, wake: Some(id.clone()), project: None };
                    let reply = host.run_op(&op, &caller);
                    post(&mut chat, Kind::System, format!("Under the playbook {playbook}, {}: {reason} ({})", op.name(), match reply {
                        glasscode_core::ops::OpReply::Done { text, .. } => text,
                        glasscode_core::ops::OpReply::Waiting { summary, .. } => format!("waiting for you: {summary}"),
                        glasscode_core::ops::OpReply::Failed(error) => error.text,
                    }));
                }
                Effect::Refused { op, why } => log::info!("manager: a wake proposed {op}, refused: {why}"),
            }
        }
    }
}
