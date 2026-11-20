//! The app's lifetime and its schedules, in a Manager v2 home (M41; design section 10; review
//! crit-11, events-9). With the engine at v1 nothing here acts.
//!
//! - **Schedules** (`glasscode_core::schedule`) are worked out from the wall clock every 30
//!   seconds, off the main thread. An occurrence is marked started before it acts, its effect is
//!   claimed under the schedule's id and the occurrence's instant, and its op runs through the gate
//!   as the manager's wake. After a start or a wake they wait a minute (the reconciler, the cut turns
//!   and the sessions go first), and late runs go one a minute. Only the home runs them: the Mac is
//!   home while no server is paired, or while it holds the lease (M58, `acts_as_home`); its claims
//!   carry the lease's generation.
//! - **Sleep.** While a turn of an automation session runs, the app holds an idle-sleep assertion
//!   (owner decision O6, on by default). It never blocks lid-close sleep. A wake is seen as a gap
//!   between the wall clock and the monotonic one.
//! - **Quit.** While automation turns run, quitting asks first: wait for them (the app quits when
//!   they end), or quit and leave them interrupted (the journal's `app.quit` names them, and their
//!   turns end as `AppQuit`).
//! - **Restart to update** waits for the automation turns to end, at most 10 minutes.
//!
//! A schedule's autonomy says how far it goes: watch only records that it was due; tell tells the
//! person and runs nothing; ask and act run the op through the gate as the manager's wake, where
//! the project's level decides whether a card asks first (a schedule cannot make the gate looser).

use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, PromptLevel, Window};
use glasscode_core::agents::AgentActivity;
use glasscode_core::effects::{self, Claim, EffectState, Intent};
use glasscode_core::playbooks::Autonomy;
use glasscode_core::schedule::{self, Due, Schedule, Skip};

use crate::workspace::Workspace;

/// How often schedules are worked out.
const EVERY: Duration = Duration::from_secs(30);
/// How long "Restart to update" waits for automation turns.
pub const RESTART_WAIT: Duration = Duration::from_secs(10 * 60);
/// A wall clock ahead of the monotonic one by more than this between two ticks: the Mac slept.
const WOKE_GAP_MS: u64 = 60_000;

#[derive(Default)]
pub struct Lifetime {
    /// When the app started, or the Mac last woke.
    since: Option<Instant>,
    /// The last tick, on both clocks.
    last: Option<(Instant, u64)>,
    /// When a late schedule run last went (wall clock).
    last_late_ms: Option<u64>,
    evaluated: Option<Instant>,
    /// A schedule round in flight.
    busy: bool,
    /// The runs the last quit cut off were cleared in this run.
    cleared: bool,
    /// The idle-sleep assertion, while it is held (or asked for).
    awake: Option<gpui::ActivityGuard>,
    awake_asked: bool,
    /// The person chose to wait: quit once the automation turns end.
    pub quit_when_idle: bool,
    /// The person chose to quit anyway.
    quit_confirmed: bool,
    /// "Restart to update" waits until this time at the latest.
    pub restart_by: Option<Instant>,
    restart_now: bool,
    /// v1's data was moved into the v2 stores in this run (or it was done before) (M44).
    migrated: bool,
    /// ... and the move worked: v2 may take over from v1's harness (`handover`).
    pub migrated_ok: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The schedules that run here now (approved and on), from the last schedule round: a v1
    /// check whose schedule runs is left to it.
    pub scheduled: std::collections::BTreeSet<String>,
    /// When the migration was last looked at (it waits for this Mac to be home).
    migrate_at: Option<Instant>,
}

impl Workspace {
    /// The titles of the sessions an automation turn runs in now: started or driven by a playbook
    /// or the manager (`gate::SessionOrigin`), and working.
    /// While the origins are not read (or could not be), none counts: a quit is never held on a
    /// guess.
    pub fn automation_turns(&self) -> Vec<String> {
        let Some(origins) = self.control.authority.origins.as_ref() else { return Vec::new() };
        self.state
            .sessions
            .iter()
            .filter(|s| self.activity.get(&s.id) == Some(&AgentActivity::Working) && origins.sessions.contains_key(&s.id) && origins.origin(&s.id) != glasscode_core::gate::SessionOrigin::Person)
            .map(|s| self.session_title(s))
            .collect()
    }

    /// Quit, after the question about text not saved in an editor (U20).
    fn quit_now(&mut self, cx: &mut Context<Self>) {
        if !self.ask_before_quitting(false, cx) {
            self.save();
            cx.quit();
        }
    }

    /// Every tick: the wake, the idle-sleep assertion, a quit or restart that waited, and the
    /// schedules.
    pub fn lifetime_tick(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 {
            return;
        }
        // M44: v1's data.json moves into the v2 stores, on the home, off the main thread: once a
        // run, and again a minute later when this Mac was not home yet (it may gain the lease).
        if !self.lifetime.migrated && self.lifetime.migrate_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(60)) {
            self.lifetime.migrate_at = Some(Instant::now());
            if self.acts_as_home() {
                self.lifetime.migrated = true;
                let ok = self.lifetime.migrated_ok.clone();
                cx.background_spawn(async move {
                    match glasscode_core::manager::migrate_v2::run(true) {
                        Ok(report) => {
                            log::info!("manager: v1's data is in the v2 stores ({report:?})");
                            ok.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        Err(why) => log::info!("manager: v1's data was not moved: {why}"),
                    }
                    // M20.3: v1's notes and the voice copilot's facts into memory, as "learned
                    // (v1)" entries and proposals. Running it again adds nothing (the same text is
                    // there), so it keeps in step while v1's manager still writes notes.
                    match glasscode_core::memstore::migrate::run(&glasscode_core::memstore::Memory::open()) {
                        Ok(report) if report != Default::default() => log::info!("memory: v1's notes are in memory ({report:?})"),
                        Ok(_) => {}
                        Err(why) => log::info!("memory: v1's notes were not brought in: {why}"),
                    }
                })
                .detach();
            }
        }
        // M42: what the quiet hours held, as one notification when they end.
        self.notify_digest_tick(cx);
        // The Memory page reads memory again while it is shown.
        self.memory_tick(cx);
        let now = Instant::now();
        let wall = glasscode_core::clock::now_ms();
        let life = &mut self.lifetime;
        life.since.get_or_insert(now);
        if let Some((at, wall_at)) = life.last
            && wall.saturating_sub(wall_at) > now.duration_since(at).as_millis() as u64 + WOKE_GAP_MS
        {
            life.since = Some(now);
        }
        life.last = Some((now, wall));
        let turns = self.automation_turns();
        self.hold_awake(!turns.is_empty(), cx);
        if self.lifetime.quit_when_idle && turns.is_empty() {
            self.lifetime.quit_when_idle = false;
            self.lifetime.quit_confirmed = true;
            self.quit_now(cx);
            return;
        }
        if let Some(by) = self.lifetime.restart_by
            && (turns.is_empty() || now >= by)
        {
            self.lifetime.restart_by = None;
            self.lifetime.restart_now = true;
            self.update_restart(cx);
            self.lifetime.restart_now = false;
        }
        if !self.lifetime.busy && self.lifetime.evaluated.is_none_or(|at| at.elapsed() >= EVERY) {
            self.schedule_round(cx);
        }
    }

    /// Hold the idle-sleep assertion while `wanted`, else let it go.
    fn hold_awake(&mut self, wanted: bool, cx: &mut Context<Self>) {
        if !wanted {
            self.lifetime.awake = None;
            self.lifetime.awake_asked = false;
            return;
        }
        if self.lifetime.awake.is_some() || self.lifetime.awake_asked {
            return;
        }
        self.lifetime.awake_asked = true;
        let task = cx.prevent_idle_sleep("Glasscode: an automation turn is running");
        cx.spawn(async move |this, cx| {
            let guard = task.await;
            let _ = this.update(cx, |ws, _| match guard {
                Ok(guard) if ws.lifetime.awake_asked => ws.lifetime.awake = Some(guard),
                Ok(_) => {}
                Err(err) => log::warn!("lifetime: the Mac may sleep during an automation turn ({err:#})"),
            });
        })
        .detach();
    }

    /// Quitting while automation turns run asks first; true when it held the quit.
    pub fn lifetime_holds_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.control.engine_v2 || self.lifetime.quit_confirmed {
            return false;
        }
        let turns = self.automation_turns();
        if turns.is_empty() {
            return false;
        }
        let detail = format!("{}\n\nThey are cut off if Glasscode quits now, and each is reported as interrupted.", turns.join("\n"));
        let message = if turns.len() == 1 { "An automation turn is running".to_string() } else { format!("{} automation turns are running", turns.len()) };
        let answer = window.prompt(PromptLevel::Warning, &message, Some(&detail), &["Wait for them, then quit", "Quit and mark them interrupted", "Cancel"], cx);
        cx.spawn(async move |this, cx| {
            let Ok(choice) = answer.await else { return };
            let _ = this.update(cx, |ws, cx| match choice {
                0 => {
                    ws.lifetime.quit_when_idle = true;
                    ws.show_toast("Glasscode quits once the automation turns end.", cx);
                }
                1 => {
                    ws.lifetime.quit_confirmed = true;
                    ws.quit_now(cx);
                }
                _ => {}
            });
        })
        .detach();
        true
    }

    /// "Restart to update" while automation turns run waits for them, at most 10 minutes; true
    /// when it held the restart.
    pub fn lifetime_holds_restart(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.control.engine_v2 || self.lifetime.restart_now {
            return false;
        }
        let turns = self.automation_turns();
        if turns.is_empty() {
            return false;
        }
        if self.lifetime.restart_by.is_none() {
            self.lifetime.restart_by = Some(Instant::now() + RESTART_WAIT);
            self.show_toast("Glasscode restarts to update once the automation turns end (10 minutes at most).", cx);
        }
        true
    }

    /// One round of the schedules, off the main thread: what is due is marked started and saved
    /// before anything acts.
    fn schedule_round(&mut self, cx: &mut Context<Self>) {
        let life = &mut self.lifetime;
        life.busy = true;
        life.evaluated = Some(Instant::now());
        let clear = !std::mem::replace(&mut life.cleared, true);
        let since_wake = life.since.map_or(0, |at| at.elapsed().as_millis() as u64);
        let last_late = life.last_late_ms;
        // Only the home runs schedules (M58 step 9): this Mac while no server is paired, or while it
        // holds the lease; the occurrence keys keep one missed across a switch to one run.
        let home = self.acts_as_home();
        let now = glasscode_core::clock::now_ms();
        let task = cx.background_spawn(async move { (schedule::round(now, home, clear, since_wake, last_late), schedule::running_ids()) });
        cx.spawn(async move |this, cx| {
            let (round, running) = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.lifetime.busy = false;
                if let Ok(running) = running {
                    ws.lifetime.scheduled = running;
                }
                match round {
                    Ok((due, skips, cut)) => {
                        for (id, occurrence) in cut {
                            log::info!("schedules: “{id}” at {occurrence} was cut off by the last quit; its claim says what came of it");
                        }
                        ws.schedule_run(due, skips, cx);
                    }
                    Err(err) => log::warn!("schedules: {err}; nothing runs until it is repaired"),
                }
            });
        })
        .detach();
    }

    /// Run what is due, through the gate, as the manager's wake; the skips go to the journal.
    fn schedule_run(&mut self, due: Vec<(Due, Schedule)>, skips: Vec<(Skip, String)>, cx: &mut Context<Self>) {
        for (skip, name) in skips {
            let words = skip.words(&name);
            glasscode_core::journal::append(glasscode_core::journal::decision("schedule", "skipped", &words, serde_json::json!({ "skip": format!("{skip:?}") }), None));
        }
        let now = glasscode_core::clock::now_ms();
        for (due, schedule) in due {
            if due.late_ms > schedule::ON_TIME_MS || due.missed > 0 {
                self.lifetime.last_late_ms = Some(now);
            }
            match schedule.autonomy {
                // Only noted.
                Autonomy::Watch => {
                    glasscode_core::journal::append(glasscode_core::journal::decision("schedule", "watched", &format!("“{}” was due: {}", schedule.name, schedule.then.op), serde_json::json!({ "schedule": schedule.id, "at": due.occurrence_ms }), None));
                    finish_run(schedule.id.clone(), due.occurrence_ms);
                    continue;
                }
                // The person is told; nothing runs.
                Autonomy::Tell => {
                    let text = format!("“{}” is due ({}).", schedule.name, schedule.then.op);
                    self.show_toast(text.clone(), cx);
                    glasscode_core::journal::append(glasscode_core::journal::decision("schedule", "told", &text, serde_json::json!({ "schedule": schedule.id, "at": due.occurrence_ms }), None));
                    finish_run(schedule.id.clone(), due.occurrence_ms);
                    continue;
                }
                Autonomy::Ask | Autonomy::Act => {}
            }
            if self.main_window.is_none() {
                log::warn!("schedules: no window to run “{}” in; it waits for the next occurrence", schedule.name);
                finish_run(schedule.id.clone(), due.occurrence_ms);
                continue;
            }
            let args = schedule_args(&schedule);
            let op = match glasscode_core::ops::typed(&schedule.then.op, args) {
                Ok(op) => op,
                Err(err) => {
                    log::warn!("schedules: “{}” cannot run {} ({})", schedule.name, schedule.then.op, err.text);
                    finish_run(schedule.id.clone(), due.occurrence_ms);
                    continue;
                }
            };
            let target = op.args.get("session").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let key = schedule::key(&schedule.id, due.occurrence_ms, op.name(), &target);
            let intent = Intent {
                origin: format!("schedule:{}", schedule.id),
                subject: due.occurrence_ms.to_string(),
                target: target.clone(),
                caller: format!("Glasscode's schedule “{}”", schedule.name),
                reason: if due.missed > 0 { format!("{} (and {} missed while away)", schedule.name, due.missed) } else { schedule.name.clone() },
                launch: self.control.launch.clone(),
                ..Intent::new(key, op.name(), op.spec.retry)
            };
            let (id, occurrence) = (schedule.id.clone(), due.occurrence_ms);
            // At the lease's generation: a home that moved on fences it (M58).
            let generation = self.home_generation();
            let claim = cx.background_spawn(async move {
                let home = glasscode_core::paths::MachineId::this().map_err(|err| std::io::Error::other(err.to_string()))?;
                effects::claim_at(&intent, &home, generation)
            });
            let caller = schedule_caller(&schedule);
            cx.spawn(async move |this, cx| {
                let claimed = claim.await;
                let _ = this.update(cx, |ws, cx| match claimed {
                    Ok(Claim::New(key)) => ws.with_main_window(cx, move |ws, window, cx| {
                        ws.run_op(
                            &op,
                            caller,
                            move |_, reply| {
                                match reply {
                                    glasscode_core::control::Reply::Done { code: 0, .. } => effects::finish_later(key, EffectState::Done(serde_json::Value::Null)),
                                    glasscode_core::control::Reply::Done { text, .. } => effects::finish_later(key, EffectState::Failed(text.lines().next().unwrap_or_default().to_string())),
                                    // A card waits for the person: the claim stays open until it is
                                    // answered (a restart settles it).
                                    glasscode_core::control::Reply::Waiting { .. } => {}
                                }
                                // Its op answered (or waits on a card): the next occurrence may go.
                                finish_run(id, occurrence);
                            },
                            window,
                            cx,
                        );
                    }),
                    Ok(Claim::Already(_)) => finish_run(id, occurrence),
                    Err(err) => {
                        log::error!("schedules: the effect ledger could not be written ({err}); “{id}” did not run");
                        finish_run(id, occurrence);
                    }
                });
            })
            .detach();
        }
    }
}

/// A run ended: the next occurrence may go (written on a thread of its own, off the main one).
fn finish_run(id: String, occurrence: u64) {
    let started = std::thread::Builder::new().name("glasscode-schedules".into()).spawn(move || {
        if let Err(err) = schedule::finish_run(&id, occurrence) {
            log::warn!("schedules: “{id}” could not be marked ended ({err})");
        }
    });
    if let Err(err) = started {
        log::warn!("schedules: a run could not be marked ended ({err})");
    }
}

/// A schedule's arguments: the text the person wrote into each (a whole number as a number,
/// `true` and `false` as switches).
fn schedule_args(schedule: &Schedule) -> serde_json::Value {
    let mut args = serde_json::Map::new();
    for (name, slots) in &schedule.then.args {
        let text: String = slots
            .iter()
            .filter_map(|slot| match slot {
                glasscode_core::playbooks::Slot::Text(text) => Some(text.as_str()),
                glasscode_core::playbooks::Slot::Field(_) => None,
            })
            .collect();
        let value = match text.as_str() {
            "true" => serde_json::Value::Bool(true),
            "false" => serde_json::Value::Bool(false),
            _ => text.parse::<u64>().map(serde_json::Value::from).unwrap_or(serde_json::Value::String(text)),
        };
        args.insert(name.clone(), value);
    }
    serde_json::Value::Object(args)
}

/// Who a schedule's op runs as: the manager's wake lane, by the schedule's id.
fn schedule_caller(schedule: &Schedule) -> crate::control_ui::Caller {
    crate::control_ui::Caller {
        who: format!("Glasscode's schedule “{}”", schedule.name),
        trust: crate::control_ui::Trust::Session,
        channel: glasscode_core::caller::Caller::Manager { lane: glasscode_core::caller::Lane::Wake, wake: Some(format!("schedule:{}", schedule.id)), project: None },
        session: None,
        session_title: None,
        agent: None,
        cwd: None,
        project: None,
        drive: None,
    }
}
