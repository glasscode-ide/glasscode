//! The manager's brain on the server (M57 step 2): the wake lane's worker
//! (`glasscode_core::runtime::worker`, the same as the app's) runs here while the server is the
//! manager's home, against `DaemonHost`.
//!
//! - **When.** It runs while the server's engine is v2, the server is home, the Mac's approved
//!   authority is here (older than a day, the gate here asks before everything: it is at most
//!   Careful), the manager is on (as the Mac said last), and the brain's program is
//!   installed here (Claude: no Codex wake lane in release 1). Otherwise it is stopped, and why is
//!   logged once. A change of home (its generation), or of what the Mac said, starts it again.
//! - **Its brain** uses the account signed in here through the vendor's CLI (no credential is ever
//!   copied), the model the Mac's settings name, and Glasscode's MCP server here
//!   (`glasscoded cli mcp`), signed with this server's secret for its lane and wake.
//! - **A brain's calls** come to this daemon (`today::relay_call`): one this server signed is
//!   served here as the manager's (`DaemonHost::run_op`); any other is refused. None goes to the
//!   Mac.
//! - **The chat lane** runs beside it (M57 with M52): the person's newest message that nothing
//!   answered yet (asked on the Mac, the phone or here; it came with the sync) is answered by a
//!   chat brain here, with what happened since they last spoke, in the message's topic. The wake
//!   lane holds while it answers. A message older than ten minutes when the server became home is
//!   not answered late. The brain's own conversation starts fresh here (the Mac's is in its
//!   account); the conversation itself is the same.
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

/// A person's message the chat lane answers, at most this old.
const CHAT_ANSWERS_WITHIN_MS: u64 = 10 * 60 * 1000;

/// The person's newest message when nothing answered it yet (no line of the chat lane after it),
/// and it is not too old to answer now.
pub(crate) fn waiting_question(conversation: &glasscode_core::conversation::Conversation, now_ms: u64) -> Option<glasscode_core::conversation::Item> {
    use glasscode_core::conversation::{Author, Kind};
    let items = conversation.items();
    let question = items.iter().filter(|item| matches!(item.kind, Kind::You { .. })).max_by(|a, b| a.posted.cmp(&b.posted))?;
    let answered = items.iter().any(|item| item.author == Author::Chat && item.posted > question.posted);
    (!answered && now_ms.saturating_sub(question.posted.wall_ms) < CHAT_ANSWERS_WITHIN_MS).then(|| (*question).clone())
}

/// The chat lane here: every two seconds, the person's waiting message is answered (once).
fn chat_thread(host: Arc<DaemonHost>, spec: BrainSpec, hold: glasscode_core::runtime::lanes::Hold, locks: glasscode_core::runtime::lanes::Locks, stop: Arc<AtomicBool>) {
    use glasscode_core::conversation::{Author, Conversation, Draft, Kind, Stream};
    use glasscode_core::runtime::{Ledger, Refusal, TurnEnd};
    use glasscode_core::store::Store as _;
    let mut lane: Option<glasscode_core::runtime::lanes::ChatLane> = None;
    let mut asked = std::collections::HashSet::new();
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(2));
        let now = glasscode_core::clock::now_ms();
        let conversation = Conversation::load();
        let Some(question) = waiting_question(&conversation, now) else { continue };
        if !asked.insert(question.id.clone()) {
            continue;
        }
        let lane = lane.get_or_insert_with(|| glasscode_core::runtime::lanes::ChatLane::new(spec.clone(), hold.clone(), locks.clone()));
        let since = glasscode_core::runtime::chat::since_text(&conversation);
        let before = Ledger::load().unwrap_or_default();
        let mut ledger = before.clone();
        let result = lane.ask(host.as_ref(), &mut ledger, &question.text, &since, now, &mut |_| {});
        if let Err(err) = glasscode_core::store::update::<Ledger, _>(|stored| stored.add_since(&before, &ledger)) {
            log(format!("manager: the token ledger could not be kept: {err}"));
        }
        let (kind, text) = match result {
            Ok((outcome, _)) => match outcome.end {
                TurnEnd::Answered(text) => (Kind::Manager { state: Stream::Done }, text),
                TurnEnd::Stopped => (Kind::Manager { state: Stream::Stopped }, String::new()),
                TurnEnd::Failed(why) => (Kind::Error, why),
            },
            Err(Refusal::Health(health)) => (Kind::Error, health.sentence(AgentKind::Claude)),
            Err(Refusal::Budget(blocked)) => (Kind::Error, blocked.sentence()),
        };
        let author = if kind == Kind::Error { Author::App } else { Author::Chat };
        if let Err(err) = Conversation::load().post(Draft { author, kind, text, refs: question.refs.clone(), topic: Some(question.topic.clone()) }) {
            log(format!("manager: the chat's answer could not be posted: {err}"));
        }
    }
    if let Some(mut lane) = lane {
        lane.brain.stop();
    }
}

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

/// How long a window op waits for the Mac: past that, what it would show is stale.
const WINDOW_OP_KEEP_MS: u64 = 6 * 60 * 60 * 1000;
/// At most this many wait; a newer one pushes the oldest out.
const WINDOW_OPS_KEPT: usize = 50;

/// The window ops waiting for the Mac, in `manager/window-queue.json`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct WindowQueue {
    next: u64,
    ops: Vec<glasscode_core::remote::QueuedWindowOp>,
}

static WINDOW_QUEUE: Mutex<()> = Mutex::new(());

fn window_queue_path() -> std::path::PathBuf {
    glasscode_core::paths::root().join("manager").join("window-queue.json")
}

/// Change the window queue on disk (under one lock in this process); what `change` gives back.
fn with_window_queue<R>(change: impl FnOnce(&mut WindowQueue) -> R) -> R {
    let _one = WINDOW_QUEUE.lock().unwrap_or_else(|p| p.into_inner());
    let path = window_queue_path();
    let mut queue: WindowQueue = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let now = glasscode_core::clock::now_ms();
    queue.ops.retain(|q| now.saturating_sub(q.queued_ms) < WINDOW_OP_KEEP_MS);
    let result = change(&mut queue);
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    let temporary = path.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_string(&queue)
        && std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, &path)).is_err()
    {
        log("manager: the window queue could not be kept".to_string());
    }
    result
}

/// A window op of the manager here, kept until the Mac connects and runs it (M57).
pub fn queue_window_op(op: &glasscode_core::ops::Op) -> u64 {
    queue_for_mac(op, None)
}

/// An op for the Mac to run, kept until it connects: a window op, or (`card`) an approved card of
/// an op only the Mac runs.
pub fn queue_for_mac(op: &glasscode_core::ops::Op, card: Option<String>) -> u64 {
    let now = glasscode_core::clock::now_ms();
    with_window_queue(|queue| {
        queue.next += 1;
        let id = queue.next;
        queue.ops.push(glasscode_core::remote::QueuedWindowOp { id, op: serde_json::to_value(op).unwrap_or_default(), queued_ms: now, card });
        let excess = queue.ops.len().saturating_sub(WINDOW_OPS_KEPT);
        queue.ops.drain(..excess);
        id
    })
}

/// The window ops for the Mac: those it acknowledged go; the rest (still in time) are sent again.
pub fn window_ops_for_mac(acked: &[u64]) -> Vec<glasscode_core::remote::QueuedWindowOp> {
    with_window_queue(|queue| {
        queue.ops.retain(|q| !acked.contains(&q.id));
        queue.ops.clone()
    })
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
    // Only a wake that runs here now, whose tool list passed its check (brain-m5): a signature
    // alone (the key is a file on this server) reaches nothing.
    let running = RUNNING.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|r| r.host.clone());
    let allowed = match &caller {
        glasscode_core::caller::Caller::Manager { lane: Lane::Wake, wake: Some(wake), .. } => running.as_ref().is_some_and(|host| host.is_running_wake(wake) && host.is_checked(Lane::Wake, Some(wake))),
        glasscode_core::caller::Caller::Manager { lane: Lane::Chat, wake: None, .. } => running.as_ref().is_some_and(|host| host.is_checked(Lane::Chat, None)),
        _ => false,
    };
    if !allowed {
        return Reply::fail(code::DENIED, "That brain does not run here (or its tools were not checked yet); nothing was done.");
    }
    let Some(op) = request.op.clone().and_then(|op| serde_json::from_value::<glasscode_core::ops::Op>(op).ok()) else {
        return Reply::fail(code::USAGE, "A brain's call names a typed op.");
    };
    let Some(host) = running else {
        return Reply::fail(code::NOT_RUNNING, "The manager does not run on this server now; nothing was done.");
    };
    match host.run_op(&op, &caller) {
        OpReply::Done { text, data } => Reply::Done { code: code::OK, text, data },
        OpReply::Waiting { pending, summary } => Reply::Waiting { approval: pending.parse().unwrap_or(0), summary },
        OpReply::Failed(error) => Reply::fail(error.exit_code(), error.text),
    }
}

/// The lines of Settings › Manager › Test (M57): where home is, the Mac's approved permissions,
/// and whether the wake lane runs here now (or why not).
pub fn test_lines(shared: &crate::daemon::Shared) -> Vec<glasscode_core::manager::TestLine> {
    use crate::manager::line;
    use glasscode_core::store::Store as _;
    let mut lines = vec![match crate::home::server_generation() {
        Some(generation) => line("Home", true, format!("The manager works from this server now (generation {generation}).")),
        None => line("Home", true, "The manager works from the Mac now; this server takes over when the Mac sleeps or you switch to Server mode."),
    }];
    let now = glasscode_core::clock::now_ms();
    lines.push(match glasscode_core::gate::FromMac::load() {
        Ok(copy) if copy.from.is_some() && !copy.effective(now).1 => line("Permissions", true, format!("The Mac's approved permissions are here (from {} min ago).", now.saturating_sub(copy.received_ms) / 60_000)),
        Ok(copy) if copy.from.is_some() => line("Permissions", true, "The Mac's approved permissions here are more than a day old: the manager here asks before acting until the Mac syncs again."),
        Ok(_) => line("Permissions", false, "The Mac has not sent its approved permissions yet (they come with the next sync)."),
        Err(err) => line("Permissions", false, format!("The Mac's approved permissions cannot be read here ({err}).")),
    });
    lines.push(match wanted(shared) {
        Ok((_, spec, _)) => line("Wake lane", true, format!("It runs here now, with {} ({}).", spec.program.display(), spec.model)),
        Err(why) if crate::home::server_generation().is_none() => line("Wake lane", true, format!("It does not run here now ({why}).")),
        Err(why) => line("Wake lane", false, format!("It cannot run here: {why}.")),
    });
    lines
}

/// What the worker would start with now, or why it does not run.
fn wanted(shared: &crate::daemon::Shared) -> Result<(u64, BrainSpec, Vec<glasscode_core::runtime::situation::Project>), String> {
    use glasscode_core::store::Store as _;
    let generation = crate::home::server_generation().ok_or("the manager's home is the Mac")?;
    // A copy older than a day still counts: the gate here is at most Careful with it
    // (`daemon::server_gate`), so a Mac closed over a weekend keeps its manager, asking first.
    let copy = glasscode_core::gate::FromMac::load().map_err(|err| format!("the Mac's approved authority cannot be read ({err})"))?;
    if copy.from.is_none() {
        return Err("the Mac's approved authority has not come yet (it comes with the next sync)".into());
    }
    let said = from_mac().ok_or("the Mac has not said how the manager runs")?;
    if !said.enabled {
        return Err("the manager is off".into());
    }
    if said.agent.is_some_and(|agent| agent != AgentKind::Claude) {
        return Err("the manager's brain is Codex, which has no wake lane in release 1".into());
    }
    let mut env: HashMap<String, String> = crate::daemon::lock(shared).env.clone();
    env.insert("GLASSCODE_CONTROL_SOCKET".into(), glasscode_core::control::socket_path().to_string_lossy().into_owned());
    // The account signed in here of the name the Mac chose, else this server's default one: its
    // own login, never a copied credential.
    let (accounts, defaults) = crate::daemon::lock(shared).accounts();
    let settings = glasscode_core::manager::Settings { brain: AgentKind::Claude, account: said.account.clone(), ..Default::default() };
    if let Some(dir) = crate::manager::brain_account(&settings, &accounts, &defaults).and_then(|a| a.config_dir()) {
        env.insert("CLAUDE_CONFIG_DIR".into(), dir.to_string_lossy().into_owned());
    }
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
/// A host whose home moved on (the Mac took it before this worker stopped) runs none: they wait,
/// approved, until this server is home again (M58).
pub(crate) fn run_approved(host: &DaemonHost) {
    use glasscode_core::gate::{Pending, PendingState};
    use glasscode_core::store::Store as _;
    let Ok(pending) = Pending::load() else { return };
    let now = glasscode_core::clock::now_ms();
    for action in pending.actions.iter().filter(|a| a.state == PendingState::Approved && a.machine.is_none()) {
        if host.not_home().is_some() {
            return;
        }
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
                        Some(OpReply::Waiting { .. }) | None => {
                            // Only the Mac runs it: handed there with the card, which stays
                            // approved here until the Mac says how it ended.
                            let queued = queue_for_mac(&op, Some(action.id.clone()));
                            log(format!("manager: the card {} you approved runs on the Mac (queued as {queued})", action.number));
                            continue;
                        }
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
    let wanted = wanted(shared);
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
    // One hold and one set of locks for both lanes: a wake waits while the chat answers.
    let (hold, locks) = (glasscode_core::runtime::lanes::Hold::default(), glasscode_core::runtime::lanes::Locks::default());
    let mut chat_spec = base.clone();
    chat_spec.lane = Lane::Chat;
    chat_spec.brain_env = glasscode_core::runtime::worker::brain_env(Lane::Chat, None, |text| crate::today::sign_text(text));
    let worker = glasscode_core::runtime::worker::Worker::new(
        Default::default(),
        hold.clone(),
        locks.clone(),
        stop.clone(),
        Arc::new(Mutex::new(projects)),
        wakes,
        base,
        Arc::new(|text: &str| crate::today::sign_text(text)),
    );
    let (for_chat, chat_stop) = (host.clone(), stop.clone());
    let _ = std::thread::Builder::new().name("manager-chat".into()).spawn(move || chat_thread(for_chat, chat_spec, hold, locks, chat_stop));
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
        assert!(matches!(&signed, Reply::Done { code: code::DENIED, .. }), "signed, but no such wake runs here: {signed:?}");
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

        // A card for an op only the Mac runs: handed to the Mac with the card, approved here
        // until the Mac says how it ended.
        let board = glasscode_core::ops::typed("board.add", serde_json::json!({ "title": "Check the release notes" })).unwrap();
        let mut action = PendingAction::new(&board, Default::default(), None, now);
        action.state = PendingState::Approved;
        let card = glasscode_core::store::update::<Pending, PendingAction>(|p| p.add(action, now)).unwrap();
        run_approved(&host);
        assert_eq!(Pending::load().unwrap().get(&card.id).unwrap().state, PendingState::Approved);
        let queued = window_ops_for_mac(&[]);
        assert!(queued.iter().any(|q| q.card.as_deref() == Some(card.id.as_str()) && q.op["op"] == "board.add"), "{queued:?}");
    }

    /// Settings › Manager › Test (M57): the home, the Mac's permissions and the wake lane, each
    /// with what to do; with the server home and no permissions from the Mac yet, the wake lane
    /// says why it cannot run.
    #[test]
    fn the_server_test_says_why_the_manager_does_not_run_here() {
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        let shared: crate::daemon::Shared = Arc::new(Mutex::new(crate::daemon::Daemon::load(HashMap::new()).unwrap()));
        let lines = test_lines(&shared);
        let by = |what: &str| lines.iter().find(|l| l.what == what).cloned().unwrap_or_else(|| panic!("no {what} line: {lines:?}"));
        assert!(by("Home").ok && by("Home").detail.contains("this server now"));
        assert!(!by("Permissions").ok);
        let lane = by("Wake lane");
        assert!(!lane.ok && lane.detail.contains("approved authority"), "{lane:?}");
    }

    /// A window op of the manager here waits for the Mac: it goes with every sync until the Mac
    /// acknowledges it (M57).
    #[test]
    fn a_window_op_waits_for_the_mac() {
        use glasscode_core::runtime::Host as _;
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        let machine = crate::owners::machine().unwrap();
        let generation = crate::home::server_generation().unwrap();
        let host = DaemonHost::at_home(machine, generation, Default::default(), Vec::new(), None);
        let say = glasscode_core::ops::typed("ui.say", serde_json::json!({ "text": "CI is green on the login fix." })).unwrap();
        let caller = glasscode_core::caller::Caller::Manager { lane: Lane::Wake, wake: Some("w1".into()), project: None };
        assert!(matches!(host.run_op(&say, &caller), OpReply::Failed(glasscode_core::ops::OpError { code: glasscode_core::ops::ErrorCode::Unavailable { queued: true, .. }, .. })));
        let first = window_ops_for_mac(&[]);
        assert_eq!(first.len(), 1);
        assert_eq!(window_ops_for_mac(&[]).len(), 1, "sent again until acknowledged");
        assert!(window_ops_for_mac(&[first[0].id]).is_empty());
    }

    /// The chat lane answers the person's newest message once nothing answered it, and not one
    /// asked long before (M57).
    #[test]
    fn the_chat_lane_answers_the_persons_newest_waiting_message() {
        use glasscode_core::conversation::{Author, Conversation, Draft, Kind, Stream};
        use glasscode_core::words::{ComposerKind, Origin};
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        glasscode_core::words::decide_again();
        let mut chat = Conversation::load();
        let you = |text: &str| {
            let words = glasscode_core::words::record(Origin::Composer(ComposerKind::ManagerChat), text, None, None);
            Draft { author: Author::Person, kind: Kind::You { words, reply_to: None }, text: text.into(), refs: Vec::new(), topic: None }
        };
        let now = glasscode_core::clock::now_ms();
        assert!(waiting_question(&chat, now).is_none(), "nothing asked");
        let first = chat.post(you("What is running?")).unwrap();
        assert_eq!(waiting_question(&chat, now).map(|q| q.id), Some(first.id.clone()));
        chat.post(Draft { author: Author::Chat, kind: Kind::Manager { state: Stream::Done }, text: "Two sessions.".into(), refs: Vec::new(), topic: None }).unwrap();
        assert!(waiting_question(&chat, now).is_none(), "answered");
        let second = chat.post(you("And the CI?")).unwrap();
        assert_eq!(waiting_question(&chat, now).map(|q| q.id), Some(second.id.clone()));
        assert!(waiting_question(&chat, second.posted.wall_ms + CHAT_ANSWERS_WITHIN_MS + 1).is_none(), "too old to answer now");
        glasscode_core::words::decide_again();
    }

    /// The worker runs only when everything says so; each missing piece is the reason.
    #[test]
    fn the_wake_lane_runs_here_only_when_the_server_is_home_and_the_mac_said_so() {
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        let shared: crate::daemon::Shared = Arc::new(Mutex::new(crate::daemon::Daemon::load(HashMap::new()).unwrap()));
        // The server is home (no record yet), but the Mac has not sent its approved authority.
        assert_eq!(crate::home::server_generation(), Some(0));
        assert_eq!(wanted(&shared).err().as_deref(), Some("the Mac's approved authority has not come yet (it comes with the next sync)"));
        keep_from_mac(ManagerFromMac { enabled: true, ..ManagerFromMac::default() });
        assert_eq!(from_mac().map(|m| m.enabled), Some(true));
        // A Mac holds the lease: the manager's home is the Mac, so the lane does not run here.
        crate::home::mac_is_home_for_test();
        assert_eq!(wanted(&shared).err().as_deref(), Some("the manager's home is the Mac"));
        assert!(test_lines(&shared).iter().any(|l| l.what == "Home" && l.detail.contains("from the Mac now")));
    }
}
