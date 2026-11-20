//! Memory's learning on the Mac (M62): `glasscode_core::learning` once an hour, in the background,
//! when the engine is v2, its `learning` feature is on, and this Mac is the manager's home (a
//! server that is home does not run it here: only one machine learns). The model's part runs on
//! the manager's brain account with Claude's cheapest model; with a Codex brain only the parts
//! that need no model run (as the wakes, D6).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui::{AppContext as _, Context};
use glasscode_core::learning::{self, LearningState, ProjectHere};
use glasscode_core::store::Store as _;

use crate::workspace::Workspace;

/// How often the app asks learning whether something is due.
const EVERY: std::time::Duration = std::time::Duration::from_secs(3600);
/// After the start, the first ask waits this long (the reconciler and the sessions go first).
const FIRST: std::time::Duration = std::time::Duration::from_secs(300);

/// Learning's part of the window.
#[derive(Default)]
pub struct Learning {
    /// A run goes on now.
    pub running: Arc<AtomicBool>,
    /// The feature is on (as last read), and what the last run did.
    pub on: bool,
    pub line: Option<String>,
}

impl Workspace {
    /// At start (v2): the hourly loop.
    pub fn learning_start(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 {
            return;
        }
        self.learning_read(cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST).await;
            loop {
                if this.update(cx, |ws, cx| ws.learning_tick(cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(EVERY).await;
            }
        })
        .detach();
    }

    /// Whether the feature is on, and the last run's line, for Settings.
    fn learning_read(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move { (glasscode_core::store::EngineSwitch::current().is_on(learning::FEATURE), LearningState::load().ok().and_then(|s| s.last)) });
        cx.spawn(async move |this, cx| {
            let (on, line) = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.learning.on = on;
                ws.learning.line = line;
                cx.notify();
            });
        })
        .detach();
    }

    /// Settings › Manager › Learning: the person turns it on or off (the engine switch's feature).
    pub(crate) fn learning_set(&mut self, on: bool, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move {
            // Switched on, it reads from a few days back, never the whole history at once.
            if on {
                let now = glasscode_core::clock::now_ms();
                if let Err(err) = glasscode_core::store::update::<LearningState, _>(|state| {
                    let from = now.saturating_sub(learning::START_BACK_MS);
                    state.words_ms = state.words_ms.max(from);
                    state.failures_ms = state.failures_ms.max(from);
                }) {
                    log::warn!("learning: where it starts could not be kept: {err}");
                }
            }
            glasscode_core::store::update::<glasscode_core::store::EngineSwitch, _>(|switch| {
                switch.switches.insert(learning::FEATURE.to_string(), on);
            })
        });
        cx.spawn(async move |this, cx| {
            let done = task.await;
            let _ = this.update(cx, |ws, cx| {
                match done {
                    Ok(()) => {
                        ws.learning.on = on;
                        ws.show_toast(if on { "Memory learns now: once an hour, on the manager's home, within its budget." } else { "Memory's learning is off." }, cx);
                        if on {
                            ws.learning_tick(cx);
                        }
                    }
                    Err(err) => ws.show_toast(format!("The switch could not be changed: {err}"), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// One run, when this Mac is home and none runs: what it needs from the window here, the rest
    /// in the background.
    pub(crate) fn learning_tick(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 || !self.home_here() || self.learning.running.load(Ordering::SeqCst) {
            return;
        }
        let projects: Vec<ProjectHere> = self.state.projects.iter().filter_map(|p| self.control.project_keys.get(&p.id).map(|key| ProjectHere { key: key.clone(), root: p.root.clone() })).collect();
        let names: Vec<String> = self.github.list.as_ref().map(|list| list.iter().map(|a| a.login.clone()).collect()).unwrap_or_default();
        let agent = self.state.manager.brain;
        let config_dir = self.state.default_account(agent).and_then(|a| a.config_dir());
        let env = self.env.clone();
        let running = self.learning.running.clone();
        running.store(true, Ordering::SeqCst);
        let task = cx.background_spawn(async move {
            let result = learning_run(projects, names, agent, config_dir, env);
            running.store(false, Ordering::SeqCst);
            result
        });
        cx.spawn(async move |this, cx| {
            let line = task.await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(line) = line {
                    ws.learning.line = Some(line);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// The run itself, off the main thread. None when learning is off.
fn learning_run(projects: Vec<ProjectHere>, mut names: Vec<String>, agent: glasscode_core::model::AgentKind, config_dir: Option<std::path::PathBuf>, env: Arc<std::collections::HashMap<String, String>>) -> Option<String> {
    use glasscode_core::runtime::budget::Ledger;
    if !glasscode_core::store::EngineSwitch::current().is_on(learning::FEATURE) {
        return None;
    }
    // The person's name as git knows it, to strip with their logins.
    if let Ok(out) = glasscode_core::sys::command("git").args(["config", "--global", "user.name"]).output()
        && out.status.success()
    {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            names.push(name);
        }
    }
    let memory = glasscode_core::memstore::Memory::open();
    let mut state = LearningState::load().unwrap_or_default();
    let filter = glasscode_core::journal::Filter { since_ms: Some(state.failures_ms.saturating_add(1)), kind: Some("ci.failed,ci.passed,turn.ended".into()), ..Default::default() };
    let events: Vec<_> = glasscode_core::journal::query(&filter, &glasscode_core::journal::Scope::All, 20_000).into_iter().filter(|e| e.at_ms() > state.failures_ms).collect();
    let inputs = learning::Inputs { now_ms: glasscode_core::clock::now_ms(), projects: &projects, names: &names, words: glasscode_core::words::own_since(state.words_ms), events };
    let before = Ledger::load().unwrap_or_default();
    let mut ledger = before.clone();
    // The model's part: Claude only (a Codex learning run is off, as its wakes are).
    let mut run = |input: &str, schema: &serde_json::Value| -> Result<String, String> {
        let dir = glasscode_core::paths::cache_dir().join("learning");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let (schema_file, answer_file) = (dir.join("schema.json"), dir.join("answer.json"));
        let _ = std::fs::remove_file(&answer_file);
        std::fs::write(&schema_file, schema.to_string()).map_err(|e| e.to_string())?;
        let profile = learning::profile(agent, schema, &schema_file, &answer_file);
        glasscode_core::agents::headless_run(agent, config_dir.as_deref(), &profile, input, Some(&answer_file), &env).map_err(|e| format!("{e:#}"))
    };
    let model: Option<learning::ModelRun> = (agent == glasscode_core::model::AgentKind::Claude).then_some(&mut run as learning::ModelRun);
    let report = learning::run(&mut state, &memory, &mut ledger, inputs, model);
    if let Err(err) = glasscode_core::store::update::<Ledger, _>(|stored| stored.add_since(&before, &ledger)) {
        log::warn!("learning: the token ledger could not be kept: {err}");
    }
    if let Err(err) = state.save() {
        log::warn!("learning: where it is could not be kept: {err}");
    }
    // What the tidy-up names, for the person, in the conversation: a nudge a project, naming it,
    // so no project's text sits beside another's.
    if let Some(tidied) = &report.tidied {
        use glasscode_core::conversation::{Author, Conversation, Draft, Kind, Ref};
        let week = glasscode_core::clock::now_ms() / (7 * 86_400_000);
        let mut chat = Conversation::load();
        for (project, text) in tidied.messages() {
            let draft = Draft { author: Author::App, kind: Kind::Nudge { matter: format!("learning:tidy:{}:{week}", project.as_str()) }, text, refs: vec![Ref::Project(project)], topic: None };
            if let Err(err) = chat.post(draft) {
                log::warn!("learning: the tidy-up's questions could not be posted: {err}");
            }
        }
    }
    Some(report.line())
}
