//! The manager's lanes in the app (M51), with the engine at v2 and the manager on: what a lane's
//! brain starts with (the chat lane's is M52's), and the wake lane's worker
//! (`glasscode_core::runtime::worker`, shared with the server) started on a thread of its own,
//! signed with the app's secret. While the person's chat turn runs, the worker holds, and the gate
//! refuses a wake's action.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use glasscode_core::caller::Lane;
use glasscode_core::model::AgentKind;
use glasscode_core::runtime::lanes::{self, Hold, Locks};
use glasscode_core::runtime::queue::{Queue, Wake};
use glasscode_core::runtime::situation::Project;
use glasscode_core::runtime::worker::{Worker, brain_env};
use glasscode_core::runtime::BrainSpec;

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
    pub(crate) fn lanes_projects(&self) -> Vec<Project> {
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
        // Only the home runs wakes (M57): this Mac while no server is paired or while it holds the
        // lease; else the server's worker does.
        if !self.control.engine_v2 || !self.state.manager.enabled || !self.home_here() {
            return;
        }
        let Some(host) = self.brain_host.clone() else { return };
        let Some(base) = self.brain_spec(Lane::Wake, None).filter(|s| s.agent == AgentKind::Claude) else { return };
        *self.lanes.projects.lock().unwrap_or_else(|p| p.into_inner()) = self.lanes_projects();
        let stop = Arc::new(AtomicBool::new(false));
        let signer = self.control.signer();
        let worker = Worker::new(self.lanes.queue.clone(), self.lanes.hold.clone(), self.lanes.locks.clone(), stop.clone(), self.lanes.projects.clone(), self.lanes.wakes.clone(), base, Arc::new(move |text: &str| signer.sign(text)));
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

