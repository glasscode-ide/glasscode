//! Intake on the server (M60): `glasscode_core::intake` while the server is the manager's home, so
//! an Autopilot project goes on finding work while the Mac is away; the Mac runs it while the Mac is
//! home (`intake_ui.rs`), never both. It runs with the engine at v2 and the manager on (what the Mac
//! said, `manager/from-mac.json`), at most every ten minutes, in a thread of its own.
//!
//! - **What it reads.** The intake rule of each project in the Mac's approved policy (the server's
//!   copy, `daemon::server_gate`), the project's GitHub issues with its label (`gh` in the server's
//!   checkout), and the server's own work and effect ledgers. Linear is the Mac's alone: the server
//!   has no Linear key.
//! - **How loaded.** The project's sessions running here count against its cap (all of them: the
//!   server does not know which the person started). The server has no reading of today's tokens
//!   or of its pull requests' reviews and CI, so those limits are the Mac's while the Mac is home,
//!   and the cap alone holds here.
//! - **Trust.** An issue's author counts only by GitHub's association (OWNER, MEMBER,
//!   COLLABORATOR): the person's logins and confirmed teammates are the Mac's to know.
//! - **Starting.** Each pick's effect is claimed at the server's home generation, its work item
//!   written with its typed done conditions, and `session.new` runs through `DaemonHost::act`, the
//!   gate with the Mac's approved policy: allowed starts, an ask is a card for the person.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glasscode_core::effects::{self, Claim, EffectState};
use glasscode_core::intake::{self, Load};
use glasscode_core::paths::ProjectKey;
use glasscode_core::ops::OpReply;
use glasscode_core::store::Store as _;

use crate::daemon::log;

/// How often the daemon asks whether intake runs here.
const EVERY: Duration = Duration::from_secs(600);

static LOOKED: Mutex<Option<Instant>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static WHY_NOT: Mutex<Option<String>> = Mutex::new(None);

/// Why intake does not run here now (none: it may), and the home's generation when it may.
fn not_here() -> Result<u64, String> {
    let generation = crate::home::server_generation().ok_or("the manager's home is the Mac, which runs intake")?;
    let copy = glasscode_core::gate::FromMac::load().map_err(|err| format!("the Mac's approved authority cannot be read ({err})"))?;
    if copy.from.is_none() {
        return Err("the Mac's approved authority has not come yet".into());
    }
    if !crate::brain::from_mac().is_some_and(|said| said.enabled) {
        return Err("the manager is off".into());
    }
    Ok(generation)
}

/// One project intake looks at here.
struct Here {
    key: ProjectKey,
    /// Its name, as the Mac's projects match the server's.
    name: String,
    root: std::path::PathBuf,
    /// Its sessions running here.
    running: u32,
}

/// From the daemon's tick: at most every ten minutes, a round in its own thread when intake may
/// run here and none runs.
pub fn tick(shared: &crate::daemon::Shared) {
    {
        let mut looked = LOOKED.lock().unwrap_or_else(|p| p.into_inner());
        if looked.is_some_and(|at| at.elapsed() < EVERY) {
            return;
        }
        *looked = Some(Instant::now());
    }
    let generation = match not_here() {
        Ok(generation) => generation,
        Err(why) => {
            let mut said = WHY_NOT.lock().unwrap_or_else(|p| p.into_inner());
            if said.as_deref() != Some(why.as_str()) {
                log(format!("intake: it does not run here: {why}"));
                *said = Some(why);
            }
            return;
        }
    };
    *WHY_NOT.lock().unwrap_or_else(|p| p.into_inner()) = None;
    let policy = crate::daemon::server_gate().policy;
    let names: HashMap<String, String> = crate::brain::from_mac().map(|said| said.projects.into_iter().map(|p| (p.id, p.name)).collect()).unwrap_or_default();
    let (here, env) = {
        let daemon = crate::daemon::lock(shared);
        let here: Vec<Here> = daemon
            .projects_with_keys()
            .into_iter()
            .filter(|(key, _)| policy.of(key).intake.as_ref().is_some_and(|rule| rule.github_label.is_some()))
            .map(|(key, root)| {
                let name = names.get(key.as_str()).cloned().unwrap_or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                let running = daemon.running_in_project(&name);
                Here { key, name, root, running }
            })
            .collect();
        (here, daemon.env.clone())
    };
    if here.is_empty() || RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let shared = shared.clone();
    let _ = std::thread::Builder::new().name("intake".into()).spawn(move || {
        for project in here {
            if let Err(why) = round(&shared, &project, &policy, &env, generation) {
                log(format!("intake: {}: {why}", project.key));
            }
        }
        RUNNING.store(false, Ordering::SeqCst);
    });
}

/// One project's round: what it finds, what it takes, and its starts.
fn round(shared: &crate::daemon::Shared, project: &Here, policy: &glasscode_core::gate::Policy, env: &HashMap<String, String>, generation: u64) -> Result<(), String> {
    let rules = policy.of(&project.key).clone();
    let Some(rule) = rules.intake.as_ref() else { return Ok(()) };
    let Some(label) = rule.github_label.as_deref() else { return Ok(()) };
    let repo = glasscode_core::github::repo_slug(&project.root).ok_or("its checkout here names no GitHub repository")?;
    let repo = repo.as_str();
    let found = intake::labeled_issues(repo, label, env).map_err(|err| format!("its issues labelled “{label}” could not be read ({err})"))?;
    let ledger = effects::load().map_err(|err| format!("the effect ledger could not be read ({err})"))?;
    let work = glasscode_core::work::WorkLedger::load().unwrap_or_default();
    let now = glasscode_core::clock::now_ms();
    let taken = |key: &glasscode_core::identity::WorkKey| work.items.iter().any(|item| item.key == *key) || found.iter().any(|f| intake::work_key(&project.key, f) == *key && intake::picked_before(ledger.records.get(&intake::effect(&project.key, f)), now));
    let load = Load { automation_sessions: project.running, ..Load::default() };
    let plan = intake::plan(&project.key, &rules, &found, &load, &taken, &[], &[]);
    if let Some(why) = &plan.blocked {
        log(format!("intake: {}: nothing taken: {}", project.key, why.words()));
    }
    if plan.picks.is_empty() {
        return Ok(());
    }
    let machine = crate::owners::machine().ok_or("this server has no machine id")?;
    let projects = crate::brain::from_mac().map(|said| said.projects).unwrap_or_default();
    // The intake's own wake holds this project alone (trust-9).
    let wakes: Arc<Mutex<HashMap<String, Vec<String>>>> = Arc::new(Mutex::new(HashMap::from([("intake".to_string(), vec![project.key.as_str().to_string()])])));
    let host = crate::brain_host::DaemonHost::at_home(machine.clone(), generation, wakes, projects, Some(shared.clone()));
    let caller = glasscode_core::caller::Caller::Manager { lane: glasscode_core::caller::Lane::Wake, wake: Some("intake".into()), project: None };
    for pick in plan.picks {
        let args = serde_json::json!({ "project": project.name, "branch": intake::branch(&pick.found), "title": glasscode_core::linear::short_title(&pick.found.title, 60), "message": pick.message });
        let op = glasscode_core::ops::typed("session.new", args).map_err(|e| e.text)?;
        let intent = intake::intent(&pick, "session.new", "server");
        let Claim::New(effect) = effects::claim_at(&intent, &machine, generation).map_err(|err| err.to_string())? else { continue };
        let stamp = glasscode_core::identity::next_stamps(&machine, 1).ok().and_then(|mut s| s.pop()).unwrap_or_else(|| glasscode_core::identity::Hlc::zero(machine.clone()));
        let added = glasscode_core::store::update::<glasscode_core::work::WorkLedger, _>(|ledger| {
            if ledger.items.iter().any(|item| item.key == pick.key) {
                return false;
            }
            let mut item = glasscode_core::work::WorkItem::new(pick.key.clone(), stamp);
            item.done = pick.done.clone();
            ledger.items.push(item);
            true
        })
        .map_err(|err| err.to_string())?;
        let id = pick.found.id();
        match host.act(&op, &caller, false) {
            Some(OpReply::Done { text, .. }) => {
                log(format!("intake: {}: took {id}: {text}", project.key));
                effects::finish(&effect, EffectState::Done(serde_json::Value::Null)).map_err(|err| err.to_string())?;
            }
            // A card for the person: the claim stays, and the sync shows the card on the Mac.
            Some(OpReply::Waiting { .. }) => log(format!("intake: {}: {id} waits for the person (a card)", project.key)),
            other => {
                let why = match other {
                    Some(OpReply::Failed(err)) => err.text,
                    _ => "the server cannot start it".to_string(),
                };
                log(format!("intake: {}: {id} was not started: {why}", project.key));
                effects::finish(&effect, EffectState::Failed(why)).map_err(|err| err.to_string())?;
                if added {
                    let _ = glasscode_core::store::update::<glasscode_core::work::WorkLedger, _>(|ledger| ledger.items.retain(|item| !(item.key == pick.key && item.sessions.is_empty())));
                }
            }
        }
    }
    Ok(())
}
