//! The manager's brain on the server (M57 step 2): the wake lane's worker
//! (`glasscode_core::runtime::worker`, the same as the app's) runs here while the server is the
//! manager's home, against `DaemonHost`.
//!
//! - **When.** It runs while the server's engine is v2, the server is home, the Mac's approved
//!   authority is fresh, the manager is on (as the Mac said last), and the brain's program is
//!   installed here (Claude: no Codex wake lane in release 1). Otherwise it is stopped, and why is
//!   logged once. A change of home (its generation), or of what the Mac said, starts it again.
//! - **Its brain** uses the account signed in here through the vendor's CLI (no credential is ever
//!   copied), the model the Mac's settings name, and Glasscode's MCP server here
//!   (`glasscoded cli mcp`), signed with this server's secret for its lane and wake.
//! - **A brain's calls** come to this daemon (`today::relay_call`): one this server signed is
//!   served here as the manager's (`DaemonHost::run_op`); any other is refused. None goes to the
//!   Mac.
//! - **What the Mac said** (`ManagerFromMac`: on or off, the model, the projects as a wake sees
//!   them) is kept in `manager/from-mac.json`, so the wakes run while the Mac is away.
//! - **Cards the person approved** on the Mac (asked here; the answer comes with the sync) run
//!   here once, checked again first (still waiting, not expired, the same targets, not refused by
//!   the gate now), and are settled with how it ended. One approved before this daemon stopped,
//!   whose end nobody saw, is uncertain.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glasscode_core::caller::Lane;
use glasscode_core::control::{Reply, Request, code};
use glasscode_core::model::AgentKind;
use glasscode_core::ops::OpReply;
use glasscode_core::remote::{ManagerFromMac, ServerNotice};
use glasscode_core::runtime::{BrainSpec, Host as _};

use crate::brain_host::DaemonHost;
use crate::daemon::log;

/// The running worker: its stop flag, its host, and what it was started with.
struct Running {
    stop: Arc<AtomicBool>,
    host: Arc<DaemonHost>,
    started_with: String,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
/// Why it is not running, as last logged (said once).
static WHY_NOT: Mutex<Option<String>> = Mutex::new(None);
/// When the last tick looked.
static LOOKED: Mutex<Option<Instant>> = Mutex::new(None);

fn from_mac_path() -> std::path::PathBuf {
    glasscode_core::paths::root().join("manager").join("from-mac.json")
}

/// What the Mac said last.
pub fn from_mac() -> Option<ManagerFromMac> {
    let text = std::fs::read_to_string(from_mac_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Keep what the Mac says (from its own connection only; the caller checks), when it changed.
pub fn keep_from_mac(said: ManagerFromMac) {
    if from_mac().as_ref() == Some(&said) {
        return;
    }
    let path = from_mac_path();
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    match serde_json::to_string_pretty(&said) {
        Ok(text) => {
            let temporary = path.with_extension("json.tmp");
            let written = std::fs::write(&temporary, text.as_bytes()).and_then(|()| std::fs::rename(&temporary, &path));
            if let Err(err) = written {
                log(format!("manager: what the Mac said could not be kept: {err}"));
            }
        }
        Err(err) => log(format!("manager: what the Mac said could not be written: {err}")),
    }
}

/// The notices of the running brain since the last sync.
pub fn take_notices() -> Vec<ServerNotice> {
    RUNNING.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|r| r.host.take_notices()).unwrap_or_default()
}

/// A brain's call: served here when this server signed its lane and wake, refused otherwise.
pub fn serve(request: &Request) -> Reply {
    let verify = |text: &str, sig: &str| crate::today::sign_text(text) == sig;
    let Some(caller) = glasscode_core::mcp::brain_caller(request, verify) else {
        return Reply::fail(code::DENIED, "This brain's channel is not this server's; nothing was done.");
    };
    let Some(op) = request.op.clone().and_then(|op| serde_json::from_value::<glasscode_core::ops::Op>(op).ok()) else {
        return Reply::fail(code::USAGE, "A brain's call names a typed op.");
    };
    let running = RUNNING.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|r| r.host.clone());
    let Some(host) = running else {
        return Reply::fail(code::NOT_RUNNING, "The manager does not run on this server now; nothing was done.");
    };
    match host.run_op(&op, &caller) {
        OpReply::Done { text, data } => Reply::Done { code: code::OK, text, data },
        OpReply::Waiting { pending, summary } => Reply::Waiting { approval: pending.parse().unwrap_or(0), summary },
        OpReply::Failed(error) => Reply::fail(error.exit_code(), error.text),
    }
}

/// What the worker would start with now, or why it does not run.
fn wanted() -> Result<(u64, BrainSpec, Vec<glasscode_core::runtime::situation::Project>), String> {
    use glasscode_core::store::{Engine, EngineSwitch, Store as _};
    if EngineSwitch::current().engine != Engine::V2 {
        return Err("the engine here is v1".into());
    }
    let generation = crate::home::server_generation().ok_or("the manager's home is the Mac")?;
    let copy = glasscode_core::gate::FromMac::load().map_err(|err| format!("the Mac's approved authority cannot be read ({err})"))?;
    if copy.effective(glasscode_core::clock::now_ms()).1 {
        return Err("the Mac's approved authority is more than a day old".into());
    }
    let said = from_mac().ok_or("the Mac has not said how the manager runs")?;
    if !said.enabled {
        return Err("the manager is off".into());
    }
    if said.agent.is_some_and(|agent| agent != AgentKind::Claude) {
        return Err("the manager's brain is Codex, which has no wake lane in release 1".into());
    }
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.insert("GLASSCODE_CONTROL_SOCKET".into(), glasscode_core::control::socket_path().to_string_lossy().into_owned());
    let program = glasscode_core::agents::resolve_program("claude", &env).ok_or("Claude is not installed on this server")?;
    let spec = BrainSpec {
        agent: AgentKind::Claude,
        lane: Lane::Wake,
        wake: None,
        program,
        glasscode: std::env::current_exe().map_err(|err| format!("glasscoded's own path is unknown ({err})"))?,
        model: said.model.clone().filter(|m| !m.trim().is_empty()).unwrap_or_else(|| "sonnet".into()),
        effort: None,
        fallback_model: None,
        cwd: glasscode_core::paths::manager_work_dir(),
        run_dir: glasscode_core::paths::manager_run_dir(),
        env,
        instructions: glasscode_core::runtime::lanes::CHAT_INSTRUCTIONS.to_string(),
        brain_env: Vec::new(),
        resume: None,
    };
    Ok((generation, spec, said.projects))
}

/// The approved cards asked here and taken up already (by id), so none runs twice.
static STARTED: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

/// Run the cards asked here that the person approved on the Mac: each once, checked again first,
/// then settled (ran, or why nothing was done).
fn run_approved(host: &DaemonHost) {
    use glasscode_core::gate::{Pending, PendingState};
    use glasscode_core::store::Store as _;
    let Ok(pending) = Pending::load() else { return };
    let now = glasscode_core::clock::now_ms();
    for action in pending.actions.iter().filter(|a| a.state == PendingState::Approved && a.machine.is_none()) {
        if !STARTED.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert_with(Default::default).insert(action.id.clone()) {
            continue;
        }
        let caller = action.caller.clone().unwrap_or(glasscode_core::caller::Caller::Manager { lane: Lane::Wake, wake: None, project: None });
        let (state, outcome) = match action.op() {
            None => (PendingState::Denied, "This server does not know that action any more; nothing was done.".to_string()),
            Some(op) => {
                let targets = host.targets_here(&op);
                let decision = match &targets {
                    Ok(targets) => glasscode_core::gate::decide(&caller, &op, targets, &crate::daemon::server_gate(), None),
                    Err(_) => glasscode_core::gate::Decision::Allow,
                };
                // Checked again as the ask it was (its answer came approved with the sync).
                let asked = glasscode_core::gate::PendingAction { state: PendingState::Waiting, ..action.clone() };
                match asked.recheck(targets.as_ref().map_err(String::as_str), &decision, now, pending.high_water_ms) {
                    Err(said) => (if said.contains("waited too long") { PendingState::Expired } else { PendingState::Denied }, said),
                    Ok(op) => match host.act(&op, &caller, true) {
                        Some(OpReply::Done { text, .. }) => (PendingState::Ran, text),
                        Some(OpReply::Failed(error)) => (PendingState::Ran, format!("It failed: {}", error.text)),
                        Some(OpReply::Waiting { .. }) | None => (PendingState::Denied, format!("This server cannot {} itself; nothing was done.", op.spec.summary)),
                    },
                }
            }
        };
        log(format!("manager: the card {} you approved: {outcome}", action.number));
        if let Err(err) = glasscode_core::gate::settle_pending(&action.id, state, Some(outcome)) {
            log(format!("manager: the card {} could not be settled: {err}", action.number));
        }
    }
}

/// Every few seconds (from the daemon's tick): start, keep or stop the worker as `wanted` says,
/// and run the cards the person approved.
pub fn tick(shared: &crate::daemon::Shared) {
    // Once a run: an approved card whose end nobody saw (this daemon stopped) is uncertain.
    static RESTARTED: std::sync::Once = std::sync::Once::new();
    RESTARTED.call_once(|| {
        let now = glasscode_core::clock::now_ms();
        if let Ok(ids) = glasscode_core::store::update::<glasscode_core::gate::Pending, Vec<String>>(|p| p.after_restart(now))
            && !ids.is_empty()
        {
            log(format!("manager: {} approved card(s) were running when glasscoded stopped; they are uncertain", ids.len()));
        }
    });
    if let Some(host) = RUNNING.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|r| r.host.clone()) {
        run_approved(&host);
    }
    {
        let mut looked = LOOKED.lock().unwrap_or_else(|p| p.into_inner());
        if looked.is_some_and(|at| at.elapsed() < Duration::from_secs(5)) {
            return;
        }
        *looked = Some(Instant::now());
    }
    let wanted = wanted();
    let mut running = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
    let key = wanted.as_ref().ok().map(|(generation, spec, projects)| format!("{generation}:{}:{}:{projects:?}", spec.model, spec.program.display()));
    if running.as_ref().is_some_and(|r| Some(&r.started_with) == key.as_ref()) {
        return;
    }
    if let Some(old) = running.take() {
        old.stop.store(true, Ordering::SeqCst);
        log("manager: the wake lane here stopped".to_string());
    }
    let (generation, base, projects) = match wanted {
        Ok(wanted) => wanted,
        Err(why) => {
            let mut said = WHY_NOT.lock().unwrap_or_else(|p| p.into_inner());
            if said.as_deref() != Some(why.as_str()) {
                log(format!("manager: the wake lane does not run here: {why}"));
                *said = Some(why);
            }
            return;
        }
    };
    *WHY_NOT.lock().unwrap_or_else(|p| p.into_inner()) = None;
    let Some(machine) = crate::owners::machine() else { return };
    let wakes: Arc<Mutex<HashMap<String, Vec<String>>>> = Arc::default();
    let host = Arc::new(DaemonHost::at_home(machine, generation, wakes.clone(), projects.clone(), Some(shared.clone())));
    let stop = Arc::new(AtomicBool::new(false));
    let worker = glasscode_core::runtime::worker::Worker::new(
        Default::default(),
        Default::default(),
        Default::default(),
        stop.clone(),
        Arc::new(Mutex::new(projects)),
        wakes,
        base,
        Arc::new(|text: &str| crate::today::sign_text(text)),
    );
    let for_worker = host.clone();
    if std::thread::Builder::new().name("manager-wakes".into()).spawn(move || worker.run(for_worker.as_ref())).is_ok() {
        log(format!("manager: the wake lane runs here (home at generation {generation})"));
        *running = Some(Running { stop, host, started_with: key.unwrap_or_default() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glasscode_core::caller::Lane;
    use glasscode_core::control::BrainToken;

    fn request(sig: String) -> Request {
        let op = glasscode_core::ops::typed("work.list", serde_json::Value::Null).unwrap();
        let mut request = glasscode_core::mcp::request_for(&op, None, None, None);
        request.brain = Some(BrainToken { lane: Lane::Wake, wake: Some("w1".into()), sig });
        request
    }

    /// A brain's call this server did not sign is refused (and never goes to the Mac); one it
    /// signed is the manager's, answered here: with no worker running, nothing is done.
    #[test]
    fn a_brains_call_is_served_here_only_when_this_server_signed_it() {
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        let forged = serve(&request("0".repeat(32)));
        assert!(matches!(&forged, Reply::Done { code: code::DENIED, .. }), "{forged:?}");
        let signed = serve(&request(crate::today::sign_text(&glasscode_core::mcp::brain_text(Lane::Wake, Some("w1")))));
        assert!(matches!(&signed, Reply::Done { code: code::NOT_RUNNING, .. }), "{signed:?}");
    }

    /// The server types into its own sessions only: a session that is not here is not found, a
    /// wake reaches no session outside its projects, and a card the person approved that no longer
    /// holds is settled with why nothing was done (M57).
    #[test]
    fn the_server_acts_on_its_own_sessions_and_settles_approved_cards() {
        use glasscode_core::gate::{Pending, PendingAction, PendingState};
        use glasscode_core::runtime::Host as _;
        use glasscode_core::store::Store as _;
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        let shared: crate::daemon::Shared = Arc::new(Mutex::new(crate::daemon::Daemon::load(HashMap::new()).unwrap()));
        let machine = crate::owners::machine().unwrap();
        let generation = crate::home::server_generation().unwrap();
        let wakes: Arc<Mutex<HashMap<String, Vec<String>>>> = Arc::new(Mutex::new(HashMap::from([("w1".to_string(), vec!["app-3f9a1c".to_string()])])));
        let host = DaemonHost::at_home(machine, generation, wakes, Vec::new(), Some(shared));
        let wake = glasscode_core::caller::Caller::Manager { lane: Lane::Wake, wake: Some("w1".into()), project: None };
        let send = glasscode_core::ops::typed("session.send", serde_json::json!({ "session": "s-gone", "message": "please rebase" })).unwrap();
        assert!(matches!(host.run_op(&send, &wake), OpReply::Failed(glasscode_core::ops::OpError { code: glasscode_core::ops::ErrorCode::NotFound, .. })));

        // A card for it, approved on the Mac: checked again, nothing done, settled with why.
        let now = glasscode_core::clock::now_ms();
        let mut action = PendingAction::new(&send, Default::default(), Some(wake), now);
        action.state = PendingState::Approved;
        let kept = glasscode_core::store::update::<Pending, PendingAction>(|p| p.add(action, now)).unwrap();
        run_approved(&host);
        let settled = Pending::load().unwrap().get(&kept.id).unwrap().clone();
        assert_eq!(settled.state, PendingState::Denied);
        assert!(settled.outcome.unwrap_or_default().contains("Nothing was done"));
        run_approved(&host);
        assert_eq!(Pending::load().unwrap().get(&kept.id).unwrap().state, PendingState::Denied, "once");
    }

    /// The worker runs only when everything says so; each missing piece is the reason.
    #[test]
    fn the_wake_lane_runs_here_only_when_the_server_is_home_and_the_mac_said_so() {
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        assert_eq!(wanted().err().as_deref(), Some("the engine here is v1"));
        keep_from_mac(ManagerFromMac { enabled: true, ..ManagerFromMac::default() });
        assert_eq!(from_mac().map(|m| m.enabled), Some(true));
    }
}
