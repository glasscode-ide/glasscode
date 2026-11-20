//! Memory's learning on the server (M62): `glasscode_core::learning` while the server is the
//! manager's home, so it goes on while the Mac is away; the Mac runs it while the Mac is home
//! (`learning_host.rs`), never both. It runs with the engine at v2 and its `learning` feature on
//! (the Mac's engine switch, which comes with the sync), at most once an hour, in a thread of its
//! own.
//!
//! - **What it reads.** Every machine's words kept here after its cursor (the Mac's come with the
//!   sync), and this server's journal. The projects are this server's checkouts with their memory
//!   keys; a project not checked out here is left alone by the repair, as on the Mac.
//! - **Its model** is the account signed in here of the name the Mac chose for the brain (as the
//!   wake lane's), on Claude's cheapest model; with a Codex brain only the parts with no model run.
//! - **Its state** (`manager/learning.json`) is this machine's: what the Mac learned while it was
//!   home may be read once more here, and proposes nothing twice (the inbox keeps one proposal per
//!   text and file).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use glasscode_core::learning::{self, LearningState, ProjectHere};
use glasscode_core::model::AgentKind;
use glasscode_core::store::Store as _;

use crate::daemon::log;

/// How often the daemon asks whether learning is due here.
const EVERY: Duration = Duration::from_secs(300);

static LOOKED: Mutex<Option<Instant>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static WHY_NOT: Mutex<Option<String>> = Mutex::new(None);

/// Why learning does not run here now (none: it may).
fn not_here() -> Option<String> {
    use glasscode_core::store::EngineSwitch;
    let switch = EngineSwitch::current();
    if !switch.is_on(learning::FEATURE) {
        return Some("learning is off (Settings › Manager › Memory on the Mac)".into());
    }
    if crate::home::server_generation().is_none() {
        return Some("the manager's home is the Mac, which learns".into());
    }
    None
}

/// From the daemon's tick: at most every five minutes, a run in its own thread when learning may
/// run here and none runs (each part keeps its own hour or week).
pub fn tick(shared: &crate::daemon::Shared) {
    {
        let mut looked = LOOKED.lock().unwrap_or_else(|p| p.into_inner());
        if looked.is_some_and(|at| at.elapsed() < EVERY) {
            return;
        }
        *looked = Some(Instant::now());
    }
    if let Some(why) = not_here() {
        let mut said = WHY_NOT.lock().unwrap_or_else(|p| p.into_inner());
        if said.as_deref() != Some(why.as_str()) {
            log(format!("memory: learning does not run here: {why}"));
            *said = Some(why);
        }
        return;
    }
    *WHY_NOT.lock().unwrap_or_else(|p| p.into_inner()) = None;
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let (projects, env, accounts, defaults) = {
        let daemon = crate::daemon::lock(shared);
        let (accounts, defaults) = daemon.accounts();
        (daemon.projects_with_keys(), daemon.env.clone(), accounts, defaults)
    };
    let _ = std::thread::Builder::new().name("memory-learning".into()).spawn(move || {
        match run(projects, env, accounts, defaults) {
            Some(line) => log(format!("memory: learning here: {line}")),
            None => {}
        }
        RUNNING.store(false, Ordering::SeqCst);
    });
}

/// One run here. None when nothing was due.
fn run(projects: Vec<(glasscode_core::paths::ProjectKey, std::path::PathBuf)>, mut env: std::collections::HashMap<String, String>, accounts: Vec<glasscode_core::model::Account>, defaults: glasscode_core::model::Defaults) -> Option<String> {
    use glasscode_core::runtime::budget::Ledger;
    let projects: Vec<ProjectHere> = projects.into_iter().map(|(key, root)| ProjectHere { key, root }).collect();
    let mut state = LearningState::load().unwrap_or_default();
    let now = glasscode_core::clock::now_ms();
    let said = crate::brain::from_mac();
    let agent = said.as_ref().and_then(|s| s.agent).unwrap_or(AgentKind::Claude);
    // The brain's account here, as the wake lane's: its own login, never a copied credential.
    let settings = glasscode_core::manager::Settings { brain: agent, account: said.as_ref().and_then(|s| s.account.clone()), ..Default::default() };
    let config_dir = crate::manager::brain_account(&settings, &accounts, &defaults).and_then(|a| a.config_dir());
    if let Some(dir) = &config_dir {
        env.insert("CLAUDE_CONFIG_DIR".into(), dir.to_string_lossy().into_owned());
    }
    let filter = glasscode_core::journal::Filter { since_ms: Some(state.failures_ms.saturating_add(1)), kind: Some("ci.failed,ci.passed,turn.ended".into()), ..Default::default() };
    let events: Vec<_> = glasscode_core::journal::query(&filter, &glasscode_core::journal::Scope::All, 20_000).into_iter().filter(|e| e.at_ms() > state.failures_ms).collect();
    // The person's name as git knows it here, to strip.
    let mut names = Vec::new();
    if let Ok(out) = std::process::Command::new("git").args(["config", "--global", "user.name"]).output()
        && out.status.success()
    {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            names.push(name);
        }
    }
    let inputs = learning::Inputs { now_ms: now, projects: &projects, names: &names, words: glasscode_core::words::since(state.words_ms), events };
    let before = Ledger::load().unwrap_or_default();
    let mut ledger = before.clone();
    let mut model_run = |input: &str, schema: &serde_json::Value| -> Result<String, String> {
        let dir = glasscode_core::paths::cache_dir().join("learning");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let (schema_file, answer_file) = (dir.join("schema.json"), dir.join("answer.json"));
        let _ = std::fs::remove_file(&answer_file);
        std::fs::write(&schema_file, schema.to_string()).map_err(|e| e.to_string())?;
        let profile = learning::profile(agent, schema, &schema_file, &answer_file);
        glasscode_core::agents::headless_run(agent, config_dir.as_deref(), &profile, input, Some(&answer_file), &env).map_err(|e| format!("{e:#}"))
    };
    let installed = glasscode_core::agents::resolve_program("claude", &std::collections::HashMap::new()).is_some();
    let model: Option<learning::ModelRun> = (agent == AgentKind::Claude && installed).then_some(&mut model_run as learning::ModelRun);
    let due = state.repair_due(now) || state.tidy_due(now) || model.is_some();
    if !due {
        return None;
    }
    let report = learning::run(&mut state, &glasscode_core::memstore::Memory::open(), &mut ledger, inputs, model);
    if let Err(err) = glasscode_core::store::update::<Ledger, _>(|stored| stored.add_since(&before, &ledger)) {
        log(format!("memory: the token ledger could not be kept: {err}"));
    }
    if let Err(err) = state.save() {
        log(format!("memory: learning's place could not be kept: {err}"));
    }
    // What the tidy-up names, a nudge a project, in the conversation (it syncs to the Mac).
    if let Some(tidied) = &report.tidied {
        use glasscode_core::conversation::{Author, Conversation, Draft, Kind, Ref};
        let week = now / (7 * 86_400_000);
        let mut chat = Conversation::load();
        for (project, text) in tidied.messages() {
            let draft = Draft { author: Author::App, kind: Kind::Nudge { matter: format!("learning:tidy:{}:{week}", project.as_str()) }, text, refs: vec![Ref::Project(project)], topic: None };
            if let Err(err) = chat.post(draft) {
                log(format!("memory: the tidy-up's questions could not be posted: {err}"));
            }
        }
    }
    Some(report.line())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learning_runs_here_only_with_its_switch_on_a_server_that_is_home() {
        use glasscode_core::store::EngineSwitch;
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        assert!(not_here().unwrap().contains("off"));
        glasscode_core::store::update::<EngineSwitch, ()>(|s| {
            s.switches.insert(learning::FEATURE.into(), true);
        })
        .unwrap();
        // With no home record, the server is home (generation 0): it learns here.
        assert_eq!(crate::home::server_generation(), Some(0));
        assert_eq!(not_here(), None);
        // A Mac holds the lease: it learns there, not here.
        crate::home::mac_is_home_for_test();
        assert!(not_here().is_some_and(|why| why.contains("home")), "{:?}", not_here());
    }
}
