//! Schedules (M41; design section 10; review events-9, crit-9, crit-11, alt-m6): what runs at a
//! time, without a model.
//!
//! - **Local time.** A daily schedule runs at a wall-clock time of the machine's own time zone,
//!   read through `clock` (`sys` local time); the zone's name is kept for display only. A local
//!   time that does not exist (the spring change) runs at the next valid minute; a repeated hour
//!   (the autumn change) runs once ([`daily_at`]).
//! - **From the wall clock, every tick.** Due-ness is worked out from the wall clock each time
//!   ([`evaluate`]), never from a sleeping timer, so a Mac that slept sees what it missed.
//! - **Missed runs.** Every occurrence missed while asleep or closed collapses into one: the last,
//!   once, when it is still inside its window (6 hours for a daily one, half its period up to 2
//!   hours for a repeating one), else it is skipped with one Activity line ([`Skip::Missed`]).
//! - **Overlap.** An occurrence that comes while the one before still runs is skipped.
//! - **Expiry** is a UTC instant, checked against the latest wall time this machine ever saw
//!   (kept in the state): a clock that went back never extends a schedule (crit-9).
//! - **Only the home runs** a schedule that says so (all do, by default); a machine that is not
//!   home moves its marks on, so it does not run what the home ran when it becomes home.
//! - **Once.** An occurrence is marked started in `authority/schedules-state.json` before it acts,
//!   and its effect is claimed under the schedule's id and the occurrence's UTC instant ([`key`]):
//!   a quit in the middle leaves it uncertain, never run twice.
//!
//! Schedules live in `authority/schedules.json`, approved per machine like playbooks (the approved
//! snapshot holds `schedule:<id>`). Both stores refuse when damaged, and then nothing runs.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::effects::{EffectKey, effect_key};
use crate::paths;
use crate::playbooks::{Action, Autonomy, Playbook, ScheduleSpec, Trigger};
use crate::store::{OnDamage, Store};

const MINUTE_MS: u64 = 60_000;
const HOUR_MS: u64 = 60 * MINUTE_MS;

/// How late a daily (or one-off) run may still go after a sleep.
pub const DAILY_GRACE_MS: u64 = 6 * HOUR_MS;
/// The most a repeating schedule's run may be late (half its period, up to this).
pub const EVERY_GRACE_MS: u64 = 2 * HOUR_MS;
/// How late a run that skips misfires may still go: it is on time, give or take a tick.
pub const ON_TIME_MS: u64 = 2 * MINUTE_MS;
/// The shortest period of a repeating schedule.
pub const MIN_PERIOD_SECS: u64 = 60;
/// After a start or a wake, schedules wait this long (the reconciler and the sessions go first),
/// and late runs go at most one a minute.
pub const SETTLE_MS: u64 = MINUTE_MS;

/// What to do about a run that was missed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Misfire {
    /// Only on time.
    Skip,
    /// The last one missed, once, within its window.
    Once,
}

/// Where it may run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    /// Only on the manager's home (D5).
    #[default]
    Home,
    /// On whichever machine has it.
    Either,
}

fn once() -> Misfire {
    Misfire::Once
}

/// One schedule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub spec: ScheduleSpec,
    pub then: Action,
    pub autonomy: Autonomy,
    #[serde(default = "once")]
    pub misfire: Misfire,
    /// When it ends, in UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<u64>,
    #[serde(default)]
    pub place: Place,
    #[serde(default)]
    pub enabled: bool,
    /// Who made it, when not the person (the v1 brain's checks): it stays off until approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_by: Option<String>,
    /// When it was made: a repeating schedule counts its periods from here.
    pub made_ms: u64,
}

impl Schedule {
    /// What the person approves: the definition and its switch.
    pub fn hash(&self) -> [u8; 32] {
        let definition = serde_json::json!({
            "id": self.id, "name": self.name, "spec": self.spec, "then": self.then, "autonomy": self.autonomy,
            "misfire": self.misfire, "expires_ms": self.expires_ms, "place": self.place, "enabled": self.enabled, "made_ms": self.made_ms,
        });
        Sha256::digest(definition.to_string().as_bytes()).into()
    }

    /// Its key in the approved snapshot of `playbooks`.
    pub fn approval_id(&self) -> String {
        format!("schedule:{}", self.id)
    }

    /// How late a missed run may still go.
    pub fn grace_ms(&self) -> u64 {
        match (self.misfire, &self.spec) {
            (Misfire::Skip, _) => ON_TIME_MS,
            (Misfire::Once, ScheduleSpec::Every { secs }) => (secs.saturating_mul(1000) / 2).clamp(ON_TIME_MS, EVERY_GRACE_MS),
            (Misfire::Once, _) => DAILY_GRACE_MS,
        }
    }
}

/// A playbook with a schedule trigger, as a schedule (its id is the playbook's).
pub fn from_playbook(playbook: &Playbook, made_ms: u64) -> Option<Schedule> {
    let Trigger::Schedule { schedule } = &playbook.trigger else { return None };
    Some(Schedule {
        id: playbook.id.clone(),
        name: playbook.name.clone(),
        spec: schedule.clone(),
        then: playbook.then.clone(),
        autonomy: playbook.autonomy,
        misfire: Misfire::Once,
        expires_ms: None,
        place: Place::Home,
        enabled: playbook.live(),
        proposed_by: playbook.proposed_by.clone(),
        made_ms,
    })
}

/// `authority/schedules.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleFile {
    pub schedules: Vec<Schedule>,
}

impl Store for ScheduleFile {
    const NAME: &'static str = "authority/schedules";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("schedules.json")
    }
}

/// What a schedule did last.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunState {
    /// The wall time of the last evaluation: occurrences after it are new.
    pub evaluated_ms: u64,
    /// The last occurrence started, or skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fired: Option<u64>,
    /// The occurrence running now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<u64>,
}

/// `authority/schedules-state.json`: each schedule's last run, and the latest wall time seen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleState {
    /// The latest wall time this machine saw: expiries are checked against it.
    pub seen_ms: u64,
    pub runs: BTreeMap<String, RunState>,
}

impl Store for ScheduleState {
    const NAME: &'static str = "authority/schedules-state";
    const VERSION: u32 = 1;
    // An older generation could run an occurrence again.
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("schedules-state.json")
    }
}

// ----- writing and approving --------------------------------------------------------------------

/// Why a change to the schedules was not made.
#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
    #[error("only the person may {0} a schedule")]
    NotThePerson(&'static str),
    #[error("there is no schedule {0}")]
    NotFound(String),
    #[error("{0} is the person's own; a proposal does not replace it")]
    TheirsToKeep(String),
    #[error("{0}")]
    Invalid(String),
}

/// Whether a schedule's time is one: a time of day under 24 hours, weekdays in the low seven bits,
/// a period of a minute or more.
pub fn check(schedule: &Schedule) -> Result<(), ScheduleError> {
    match &schedule.spec {
        ScheduleSpec::At { minutes, days, .. } if *minutes >= 1440 || *days >= 128 => Err(ScheduleError::Invalid(format!("“{}” runs at no time of day", schedule.name))),
        ScheduleSpec::Every { secs } if *secs < MIN_PERIOD_SECS => Err(ScheduleError::Invalid(format!("“{}” would run more than once a minute", schedule.name))),
        _ if schedule.id.is_empty() || !schedule.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') => Err(ScheduleError::Invalid(format!("{:?} is not a schedule id", schedule.id))),
        _ => Ok(()),
    }
}

/// Whether this machine approved a schedule as it is now.
pub fn is_approved(schedule: &Schedule, approved: &crate::playbooks::ApprovedPlaybooks) -> bool {
    let hash: String = schedule.hash().iter().map(|b| format!("{b:02x}")).collect();
    approved.approved.get(&schedule.approval_id()).is_some_and(|approval| approval.hash == hash)
}

/// Write a schedule. The person's is written as it is (and approved with [`approve`]); anyone
/// else's (a wake, the v1 brain's checks) is only proposed: switched off, marked with who, and with
/// no approval. A proposal never replaces the person's own. It writes the disk.
pub fn propose(mut schedule: Schedule, by: Option<&str>) -> Result<(), ScheduleError> {
    check(&schedule)?;
    if let Some(by) = by {
        schedule.enabled = false;
        schedule.proposed_by = Some(by.to_string());
    }
    let (id, proposal) = (schedule.id.clone(), by.is_some());
    let written = crate::store::update::<ScheduleFile, _>(|file| match file.schedules.iter_mut().find(|s| s.id == id) {
        Some(have) if proposal && have.proposed_by.is_none() => false,
        Some(have) => {
            *have = schedule;
            true
        }
        None => {
            file.schedules.push(schedule);
            true
        }
    })?;
    if !written {
        return Err(ScheduleError::TheirsToKeep(id));
    }
    if proposal {
        crate::store::update::<crate::playbooks::ApprovedPlaybooks, _>(|approved| {
            approved.approved.remove(&format!("schedule:{id}"));
        })?;
    }
    Ok(())
}

/// The person approves a schedule as it is now, on this machine. It writes the disk.
pub fn approve(id: &str, person: bool) -> Result<(), ScheduleError> {
    if !person {
        return Err(ScheduleError::NotThePerson("approve"));
    }
    let file = ScheduleFile::load()?;
    let schedule = file.schedules.iter().find(|s| s.id == id).ok_or_else(|| ScheduleError::NotFound(id.to_string()))?;
    let hash: String = schedule.hash().iter().map(|b| format!("{b:02x}")).collect();
    let key = schedule.approval_id();
    crate::store::update::<crate::playbooks::ApprovedPlaybooks, _>(|approved| {
        approved.approved.insert(key, crate::playbooks::Approval { hash, approved_ms: crate::clock::now_ms() });
    })?;
    Ok(())
}

/// One round, under the state's lock: the schedules and this machine's approvals read, what is due
/// worked out, held back by [`pace`] or marked started, and saved before anything acts. `clear`:
/// the first round of a run, which clears what the last quit cut off. It reads and writes the disk.
pub fn round(now_ms: u64, home: bool, clear: bool, since_wake_ms: u64, last_late_ms: Option<u64>) -> Result<(Vec<(Due, Schedule)>, Vec<(Skip, String)>, Vec<(String, u64)>), ScheduleError> {
    let file = ScheduleFile::load()?;
    let approved = crate::playbooks::ApprovedPlaybooks::load()?;
    let (due, skips, cut) = crate::store::update::<ScheduleState, _>(|state| {
        let cut = if clear { after_restart(state) } else { Vec::new() };
        let (due, skips) = evaluate(&file.schedules, &|s| is_approved(s, &approved), state, now_ms, home);
        let due = pace(due, since_wake_ms, last_late_ms, now_ms);
        for d in &due {
            started(state, &d.schedule, d.occurrence_ms);
        }
        (due, skips, cut)
    })?;
    let find = |id: &str| file.schedules.iter().find(|s| s.id == id).cloned();
    let skips = skips
        .into_iter()
        .map(|skip| {
            let id = match &skip {
                Skip::Missed { schedule, .. } | Skip::Overlap { schedule, .. } => schedule.clone(),
            };
            let name = find(&id).map(|s| s.name).unwrap_or(id);
            (skip, name)
        })
        .collect();
    Ok((due.into_iter().filter_map(|d| find(&d.schedule).map(|s| (d, s))).collect(), skips, cut))
}

/// A run ended (however): marked under the state's lock. It writes the disk.
pub fn finish_run(schedule: &str, occurrence_ms: u64) -> Result<(), ScheduleError> {
    crate::store::update::<ScheduleState, _>(|state| finished(state, schedule, occurrence_ms))?;
    Ok(())
}

// ----- when ------------------------------------------------------------------------------------

/// The local day (days since the local epoch) of a UTC instant.
fn local_day(at_ms: u64) -> Option<i64> {
    crate::clock::local_minutes(at_ms).map(|minutes| minutes.div_euclid(1440))
}

/// The UTC instant a daily time `minutes` after local midnight comes on local day `day`: the first
/// minute whose local time is that time or later. In the spring change, a time the clock skips
/// comes at the next valid minute; in the autumn one, a time the clock shows twice comes the first
/// time only.
pub fn daily_at(day: i64, minutes: u16) -> Option<u64> {
    let target = day * 1440 + i64::from(minutes);
    // A first guess from the offset there, then a walk of three hours each way.
    let naive = u64::try_from(target.checked_mul(MINUTE_MS as i64)?).ok()?;
    let offset = crate::clock::local_minutes(naive)? * MINUTE_MS as i64 - naive as i64;
    let guess = naive as i64 - offset;
    let from = (guess - 3 * HOUR_MS as i64).max(0) as u64 / MINUTE_MS * MINUTE_MS;
    (0..=6 * 60).map(|step| from + step * MINUTE_MS).find(|at| crate::clock::local_minutes(*at).is_some_and(|local| local >= target))
}

/// The last occurrence of a schedule after `after_ms` and at or before `until_ms`, and how many
/// there were in between (it included). None: no occurrence there.
pub fn last_occurrence(spec: &ScheduleSpec, made_ms: u64, after_ms: u64, until_ms: u64) -> Option<(u64, u32)> {
    if until_ms <= after_ms {
        return None;
    }
    match spec {
        ScheduleSpec::Once { at_ms } => (after_ms < *at_ms && *at_ms <= until_ms).then_some((*at_ms, 1)),
        ScheduleSpec::Every { secs } => {
            let period = secs.max(&MIN_PERIOD_SECS) * 1000;
            if until_ms < made_ms + period {
                return None;
            }
            let last = made_ms + (until_ms - made_ms) / period * period;
            if last <= after_ms {
                return None;
            }
            let first = if after_ms < made_ms + period { made_ms + period } else { made_ms + ((after_ms - made_ms) / period + 1) * period };
            Some((last, u32::try_from((last - first) / period + 1).unwrap_or(u32::MAX)))
        }
        ScheduleSpec::At { minutes, days, .. } => {
            let (Some(first_day), Some(last_day)) = (local_day(after_ms), local_day(until_ms)) else { return None };
            // At most a year and a bit back: a longer absence counts as that.
            let first_day = first_day.max(last_day - 400);
            let mut last = None;
            let mut count = 0u32;
            for day in first_day..=last_day {
                let weekday = (day + 4).rem_euclid(7) as u32;
                if *days != 0 && days & (1 << weekday) == 0 {
                    continue;
                }
                let Some(at) = daily_at(day, *minutes) else { continue };
                if after_ms < at && at <= until_ms {
                    last = Some(at);
                    count += 1;
                }
            }
            last.map(|at| (at, count))
        }
    }
}

// ----- what is due -----------------------------------------------------------------------------

/// An occurrence to run now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    pub schedule: String,
    pub occurrence_ms: u64,
    /// Earlier occurrences that collapsed into this one.
    pub missed: u32,
    pub late_ms: u64,
}

/// An occurrence that does not run, for one Activity line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skip {
    /// Missed while asleep or closed, and past its window: `count` occurrences, the last at
    /// `occurrence_ms`.
    Missed { schedule: String, occurrence_ms: u64, count: u32 },
    /// The run before still runs.
    Overlap { schedule: String, occurrence_ms: u64 },
}

impl Skip {
    pub fn words(&self, name: &str) -> String {
        match self {
            Skip::Missed { count: 1, .. } => format!("“{name}” did not run while the Mac was away, and it is past its time now."),
            Skip::Missed { count, .. } => format!("“{name}” did not run {count} times while the Mac was away; they are past their time now."),
            Skip::Overlap { .. } => format!("“{name}” did not run: its run before is still going."),
        }
    }
}

/// The effect key of an occurrence: the schedule's id and the occurrence's UTC instant.
pub fn key(schedule: &str, occurrence_ms: u64, op: &str, target: &str) -> EffectKey {
    effect_key(&format!("schedule:{schedule}"), &occurrence_ms.to_string(), op, target)
}

/// What is due now, and what is skipped: pure. `approved` says whether this machine approved a
/// schedule as it is; `home`, whether this machine is the manager's home. Every schedule's mark
/// moves on to `now_ms`, run or not, so nothing it passed comes back later; a clock that went back
/// moves nothing back.
pub fn evaluate(schedules: &[Schedule], approved: &dyn Fn(&Schedule) -> bool, state: &mut ScheduleState, now_ms: u64, home: bool) -> (Vec<Due>, Vec<Skip>) {
    state.seen_ms = state.seen_ms.max(now_ms);
    let seen = state.seen_ms;
    let mut due = Vec::new();
    let mut skipped = Vec::new();
    for schedule in schedules {
        let run = state.runs.entry(schedule.id.clone()).or_insert_with(|| RunState { evaluated_ms: schedule.made_ms, ..RunState::default() });
        let after = run.evaluated_ms;
        // The mark moves on to now; only a due occurrence keeps it just before itself, so one held
        // back (by `pace`) is found again until it is started, or ages into a skip.
        run.evaluated_ms = run.evaluated_ms.max(now_ms);
        let live = schedule.enabled && approved(schedule) && (home || schedule.place == Place::Either);
        if !live || schedule.expires_ms.is_some_and(|ends| ends <= seen) {
            continue;
        }
        let Some((last, count)) = last_occurrence(&schedule.spec, schedule.made_ms, after, now_ms) else { continue };
        if run.fired.is_some_and(|fired| fired >= last) {
            continue;
        }
        if run.running.is_some() {
            run.fired = Some(last);
            skipped.push(Skip::Overlap { schedule: schedule.id.clone(), occurrence_ms: last });
            continue;
        }
        let late = now_ms - last;
        if late > schedule.grace_ms() {
            run.fired = Some(last);
            skipped.push(Skip::Missed { schedule: schedule.id.clone(), occurrence_ms: last, count });
            continue;
        }
        run.evaluated_ms = after.max(last.saturating_sub(1));
        due.push(Due { schedule: schedule.id.clone(), occurrence_ms: last, missed: count - 1, late_ms: late });
    }
    (due, skipped)
}

/// An occurrence starts: marked before it acts, so it never runs twice.
pub fn started(state: &mut ScheduleState, schedule: &str, occurrence_ms: u64) {
    let run = state.runs.entry(schedule.to_string()).or_default();
    run.fired = Some(run.fired.map_or(occurrence_ms, |fired| fired.max(occurrence_ms)));
    run.running = Some(occurrence_ms);
    run.evaluated_ms = run.evaluated_ms.max(occurrence_ms);
}

/// An occurrence ended (however): the next may run.
pub fn finished(state: &mut ScheduleState, schedule: &str, occurrence_ms: u64) {
    if let Some(run) = state.runs.get_mut(schedule)
        && run.running == Some(occurrence_ms)
    {
        run.running = None;
    }
}

/// After a restart nothing of the run before is still running: a run marked running then was cut
/// off (its claim says what came of it). Gives the occurrences cleared.
pub fn after_restart(state: &mut ScheduleState) -> Vec<(String, u64)> {
    let mut cut = Vec::new();
    for (id, run) in &mut state.runs {
        if let Some(occurrence) = run.running.take() {
            cut.push((id.clone(), occurrence));
        }
    }
    cut
}

/// Of the runs due, the ones to start now: on time, all; late (after a sleep or a start), one at a
/// time, the oldest first, and none in the first minute after a wake or a start.
pub fn pace(mut due: Vec<Due>, since_wake_ms: u64, last_late_ms: Option<u64>, now_ms: u64) -> Vec<Due> {
    let on_time = |d: &Due| d.late_ms <= ON_TIME_MS && d.missed == 0;
    if since_wake_ms < SETTLE_MS {
        due.retain(on_time);
        return due;
    }
    due.sort_by_key(|d| d.occurrence_ms);
    let mut late_room = last_late_ms.is_none_or(|at| now_ms.saturating_sub(at) >= MINUTE_MS);
    due.retain(|d| {
        if on_time(d) {
            return true;
        }
        std::mem::replace(&mut late_room, false)
    });
    due
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::effects::{self, Claim, EffectState, Intent};
    use crate::ops::Retry;
    use crate::testkit::clock::{DAY, HOUR, MINUTE, VirtualClock, Zone, utc_ms};

    fn at(minutes: u16) -> ScheduleSpec {
        ScheduleSpec::At { minutes, days: 0, tz: "Europe/Berlin".into() }
    }

    fn schedule(id: &str, spec: ScheduleSpec, made_ms: u64) -> Schedule {
        Schedule {
            id: id.into(),
            name: "Morning brief".into(),
            spec,
            then: Action { op: "manager.brief".into(), args: Default::default() },
            autonomy: Autonomy::Tell,
            misfire: Misfire::Once,
            expires_ms: None,
            place: Place::Home,
            enabled: true,
            proposed_by: None,
            made_ms,
        }
    }

    fn yes(_: &Schedule) -> bool {
        true
    }

    /// Evaluate every `step` from the clock's time to `until`, starting what is due; gives the
    /// local times (hour, minute) of the runs, and every skip.
    fn run_through(clock: &VirtualClock, schedules: &[Schedule], state: &mut ScheduleState, until: u64, step: u64) -> (Vec<(u64, (u32, u32))>, Vec<Skip>) {
        let mut runs = Vec::new();
        let mut skips = Vec::new();
        while clock.now() < until {
            clock.advance(step);
            let (due, skipped) = evaluate(schedules, &yes, state, clock.now(), true);
            for d in due {
                started(state, &d.schedule, d.occurrence_ms);
                finished(state, &d.schedule, d.occurrence_ms);
                let local = crate::clock::local_minutes(d.occurrence_ms).unwrap();
                runs.push((d.occurrence_ms, (((local.rem_euclid(1440)) / 60) as u32, (local.rem_euclid(60)) as u32)));
            }
            skips.extend(skipped);
        }
        (runs, skips)
    }

    #[test]
    fn dst_gap_runs_next_minute() {
        // 29 March 2026: Berlin's clocks go from 02:00 to 03:00. A 02:30 schedule runs at 03:00.
        let clock = VirtualClock::new(utc_ms(2026, 3, 28, 12, 0), Zone::berlin());
        let _guard = clock.install();
        let s = schedule("night", at(150), clock.now());
        let mut state = ScheduleState::default();
        let (runs, skips) = run_through(&clock, &[s], &mut state, utc_ms(2026, 3, 30, 12, 0), 5 * MINUTE);
        assert!(skips.is_empty(), "{skips:?}");
        assert_eq!(runs.iter().map(|(_, local)| *local).collect::<Vec<_>>(), [(3, 0), (2, 30)]);
        assert_eq!(runs[0].0, utc_ms(2026, 3, 29, 1, 0));
    }

    #[test]
    fn dst_overlap_runs_once() {
        // 25 October 2026: Berlin's clocks go from 03:00 back to 02:00; 02:30 comes twice.
        let clock = VirtualClock::new(utc_ms(2026, 10, 24, 12, 0), Zone::berlin());
        let _guard = clock.install();
        let s = schedule("night", at(150), clock.now());
        let mut state = ScheduleState::default();
        let (runs, _) = run_through(&clock, &[s], &mut state, utc_ms(2026, 10, 25, 12, 0), 5 * MINUTE);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, utc_ms(2026, 10, 25, 0, 30), "the first 02:30 (summer time)");
    }

    #[test]
    fn sleep_catch_up_runs_once() {
        // 09:00 set; the Mac sleeps from 08:00 to 10:00: it runs once at 10:00.
        let clock = VirtualClock::new(utc_ms(2026, 11, 20, 8, 0), Zone::utc());
        let _guard = clock.install();
        let s = schedule("brief", at(9 * 60), utc_ms(2026, 11, 19, 12, 0));
        let mut state = ScheduleState::default();
        assert!(evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true).0.is_empty());
        clock.sleep(2 * HOUR);
        let (due, skips) = evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true);
        assert!(skips.is_empty());
        assert_eq!(due, [Due { schedule: "brief".into(), occurrence_ms: utc_ms(2026, 11, 20, 9, 0), missed: 0, late_ms: HOUR }]);
        started(&mut state, "brief", due[0].occurrence_ms);
        finished(&mut state, "brief", due[0].occurrence_ms);
        assert!(evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now() + MINUTE, true).0.is_empty(), "once");
        // A late run waits for the first minute after the wake, then goes alone.
        assert!(pace(due.clone(), 10_000, None, clock.now()).is_empty());
        assert_eq!(pace(due, SETTLE_MS, None, clock.now()).len(), 1);
    }

    #[test]
    fn a_run_held_back_by_pace_comes_again() {
        let clock = VirtualClock::new(utc_ms(2026, 11, 20, 8, 0), Zone::utc());
        let _guard = clock.install();
        let s = schedule("brief", at(9 * 60), utc_ms(2026, 11, 19, 12, 0));
        let mut state = ScheduleState::default();
        evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true);
        clock.sleep(2 * HOUR);
        // Just after the wake: due, held back by pace, not started.
        let (due, _) = evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true);
        assert!(pace(due, 5_000, None, clock.now()).is_empty());
        // A minute later it is due again, and goes.
        clock.advance(MINUTE);
        let (due, _) = evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true);
        let due = pace(due, SETTLE_MS, None, clock.now());
        assert_eq!(due.len(), 1);
        started(&mut state, "brief", due[0].occurrence_ms);
        clock.advance(MINUTE);
        assert!(evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true).0.is_empty());
        // Held back past its window, it is skipped instead.
        let mut state = ScheduleState::default();
        clock.jump_to(utc_ms(2026, 11, 21, 9, 30));
        let (due, _) = evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true);
        assert_eq!(due.len(), 1);
        clock.advance(7 * HOUR);
        let (due, skips) = evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true);
        assert!(due.is_empty());
        assert!(matches!(skips[..], [Skip::Missed { .. }]));
    }

    #[test]
    fn a_schedule_runs_only_once_the_person_approved_it() {
        let dir = tempfile::tempdir().unwrap();
        let before: Option<OsString> = std::env::var_os("GLASSCODE_HOME");
        // SAFETY: the core tests run one at a time (`--test-threads=1`).
        unsafe { std::env::set_var("GLASSCODE_HOME", dir.path()) };
        let clock = VirtualClock::new(utc_ms(2026, 11, 20, 8, 0), Zone::utc());
        let _guard = clock.install();
        let s = schedule("brief", at(9 * 60), utc_ms(2026, 11, 19, 12, 0));
        // The v1 brain's check comes in as a proposal: off, and not approved.
        propose(s.clone(), Some("the v1 manager")).unwrap();
        clock.advance(30 * MINUTE);
        assert!(round(clock.now(), true, true, SETTLE_MS, None).unwrap().0.is_empty());
        assert!(approve("brief", false).is_err());
        approve("brief", true).unwrap();
        clock.advance(31 * MINUTE);
        assert!(round(clock.now(), true, false, SETTLE_MS, None).unwrap().0.is_empty(), "approved as proposed: still switched off");
        // The person's own, approved: it runs, once.
        clock.jump_to(utc_ms(2026, 11, 21, 8, 30));
        propose(s.clone(), None).unwrap();
        approve("brief", true).unwrap();
        clock.advance(31 * MINUTE);
        let (due, _, _) = round(clock.now(), true, false, SETTLE_MS, None).unwrap();
        assert_eq!(due.len(), 1);
        assert!(round(clock.now() + MINUTE, true, false, SETTLE_MS, None).unwrap().0.is_empty());
        finish_run("brief", due[0].0.occurrence_ms).unwrap();
        assert_eq!(ScheduleState::load().unwrap().runs["brief"].running, None);
        // A proposal never replaces the person's own; an edit outside the app is not approved.
        assert!(matches!(propose(Schedule { name: "x".into(), ..s.clone() }, Some("a session")), Err(ScheduleError::TheirsToKeep(_))));
        let mut file = ScheduleFile::load().unwrap();
        file.schedules[0].then.op = "session.send".into();
        file.save().unwrap();
        assert!(!is_approved(&ScheduleFile::load().unwrap().schedules[0], &crate::playbooks::ApprovedPlaybooks::load().unwrap()));
        // A time that is none is refused.
        assert!(propose(Schedule { spec: ScheduleSpec::At { minutes: 1500, days: 0, tz: String::new() }, ..s.clone() }, None).is_err());
        assert!(propose(Schedule { spec: ScheduleSpec::Every { secs: 5 }, ..s }, None).is_err());
        assert_eq!(serde_json::to_value(Misfire::Once).unwrap(), "once");
        match before {
            Some(value) => unsafe { std::env::set_var("GLASSCODE_HOME", value) },
            None => unsafe { std::env::remove_var("GLASSCODE_HOME") },
        }
    }

    #[test]
    fn old_missed_run_skipped() {
        // Three days asleep: the runs collapse into one line, and none runs.
        let clock = VirtualClock::new(utc_ms(2026, 11, 20, 8, 0), Zone::utc());
        let _guard = clock.install();
        let s = schedule("brief", at(9 * 60), utc_ms(2026, 11, 19, 12, 0));
        let mut state = ScheduleState::default();
        evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true);
        clock.sleep(3 * DAY - 2 * HOUR);
        let (due, skips) = evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true);
        assert!(due.is_empty());
        assert_eq!(skips, [Skip::Missed { schedule: "brief".into(), occurrence_ms: utc_ms(2026, 11, 22, 9, 0), count: 3 }]);
        assert!(skips[0].words("Morning brief").contains("3 times"));
        // Nothing of it comes back.
        assert!(evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now() + MINUTE, true) == (Vec::new(), Vec::new()));
        // A repeating schedule's window is half its period: every 10 minutes, five minutes late is
        // too late; a skip-misfire one is only ever on time.
        let every = Schedule { spec: ScheduleSpec::Every { secs: 600 }, ..schedule("poll", at(0), clock.now()) };
        assert_eq!(every.grace_ms(), 5 * MINUTE);
        assert_eq!(Schedule { misfire: Misfire::Skip, ..every.clone() }.grace_ms(), ON_TIME_MS);
        assert_eq!(last_occurrence(&every.spec, every.made_ms, every.made_ms, every.made_ms + 35 * MINUTE), Some((every.made_ms + 30 * MINUTE, 3)));
    }

    #[test]
    fn overlap_skipped() {
        let clock = VirtualClock::new(utc_ms(2026, 11, 20, 8, 0), Zone::utc());
        let _guard = clock.install();
        let s = Schedule { spec: ScheduleSpec::Every { secs: 600 }, ..schedule("poll", at(0), clock.now()) };
        let mut state = ScheduleState::default();
        clock.advance(10 * MINUTE);
        let (due, _) = evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true);
        started(&mut state, "poll", due[0].occurrence_ms);
        // Still running at the next one: skipped, with a line, and not run later.
        clock.advance(10 * MINUTE);
        let (due, skips) = evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true);
        assert!(due.is_empty());
        assert_eq!(skips, [Skip::Overlap { schedule: "poll".into(), occurrence_ms: clock.now() }]);
        finished(&mut state, "poll", utc_ms(2026, 11, 20, 8, 10));
        assert!(evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now() + MINUTE, true).0.is_empty());
        clock.advance(10 * MINUTE);
        assert_eq!(evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true).0.len(), 1);
    }

    #[test]
    fn only_home_runs() {
        let clock = VirtualClock::new(utc_ms(2026, 11, 20, 8, 0), Zone::utc());
        let _guard = clock.install();
        let s = schedule("brief", at(9 * 60), utc_ms(2026, 11, 19, 12, 0));
        let mut state = ScheduleState::default();
        clock.advance(HOUR + MINUTE);
        // Not home: nothing, and the mark moves on.
        assert!(evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), false).0.is_empty());
        // Home again a little later: what the home ran meanwhile is not run here.
        clock.advance(MINUTE);
        assert!(evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true).0.is_empty());
        // One that may run anywhere runs where it is; one not approved here runs nowhere.
        let anywhere = Schedule { id: "anywhere".into(), place: Place::Either, ..s.clone() };
        clock.advance(DAY);
        let mut state = ScheduleState::default();
        assert_eq!(evaluate(std::slice::from_ref(&anywhere), &yes, &mut state, clock.now(), false).0.len(), 1);
        let mut state = ScheduleState::default();
        assert!(evaluate(std::slice::from_ref(&anywhere), &|_| false, &mut state, clock.now(), true).0.is_empty());
    }

    #[test]
    fn clock_back_never_extends_schedule_expiry() {
        let clock = VirtualClock::new(utc_ms(2026, 11, 20, 8, 0), Zone::utc());
        let _guard = clock.install();
        let ends = utc_ms(2026, 11, 21, 12, 0);
        let s = Schedule { expires_ms: Some(ends), ..schedule("brief", at(9 * 60), utc_ms(2026, 11, 19, 12, 0)) };
        let mut state = ScheduleState::default();
        clock.advance(HOUR + MINUTE);
        assert_eq!(evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true).0.len(), 1);
        // Past its end, then the clock is set back a day: it has still ended.
        clock.jump_to(ends + HOUR);
        evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true);
        clock.jump_to(utc_ms(2026, 11, 21, 9, 1));
        assert!(evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true).0.is_empty());
        assert_eq!(state.seen_ms, ends + HOUR);
        // And an occurrence already run does not run again when the clock goes back past it.
        let daily = schedule("daily", at(9 * 60), utc_ms(2026, 11, 19, 12, 0));
        let mut state = ScheduleState::default();
        clock.jump_to(utc_ms(2026, 11, 22, 9, 1));
        let (due, _) = evaluate(std::slice::from_ref(&daily), &yes, &mut state, clock.now(), true);
        started(&mut state, "daily", due[0].occurrence_ms);
        finished(&mut state, "daily", due[0].occurrence_ms);
        clock.jump_to(utc_ms(2026, 11, 22, 8, 0));
        clock.advance(HOUR + 2 * MINUTE);
        assert!(evaluate(std::slice::from_ref(&daily), &yes, &mut state, clock.now(), true).0.is_empty());
    }

    #[test]
    fn quit_during_wake_marks_uncertain_not_repeated() {
        let dir = tempfile::tempdir().unwrap();
        let before: Option<OsString> = std::env::var_os("GLASSCODE_HOME");
        // SAFETY: the core tests run one at a time (`--test-threads=1`).
        unsafe { std::env::set_var("GLASSCODE_HOME", dir.path()) };
        let clock = VirtualClock::new(utc_ms(2026, 11, 20, 9, 0), Zone::utc());
        let _guard = clock.install();
        let s = schedule("brief", at(9 * 60), utc_ms(2026, 11, 19, 12, 0));
        let mut state = ScheduleState::default();
        clock.advance(MINUTE);
        let (due, _) = evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true);
        // Marked started and claimed before it acts; the app quits in the middle.
        started(&mut state, "brief", due[0].occurrence_ms);
        state.save().unwrap();
        let k = key("brief", due[0].occurrence_ms, "manager.brief", "");
        let home = paths::MachineId::parse("00000000000000aa").unwrap();
        let intent = Intent { origin: "schedule:brief".into(), launch: "run-1".into(), ..Intent::new(k, "manager.brief", Retry::AtMostOnce) };
        assert!(matches!(effects::claim(&intent, &home).unwrap(), Claim::New(_)));
        // The next run: nothing is running any more, the claim with no result is uncertain, and
        // the occurrence is not run again.
        let mut state = ScheduleState::load().unwrap();
        assert_eq!(after_restart(&mut state), [("brief".to_string(), due[0].occurrence_ms)]);
        let cut = effects::unsettled("run-2").unwrap();
        assert_eq!(effects::settle(&cut[0], None), EffectState::Uncertain);
        clock.advance(MINUTE);
        assert!(evaluate(std::slice::from_ref(&s), &yes, &mut state, clock.now(), true).0.is_empty());
        assert!(matches!(effects::claim(&intent, &home).unwrap(), Claim::Already(_)));
        match before {
            Some(value) => unsafe { std::env::set_var("GLASSCODE_HOME", value) },
            None => unsafe { std::env::remove_var("GLASSCODE_HOME") },
        }
    }

    #[test]
    fn weekdays_and_a_week_of_virtual_days() {
        // Weekdays only (Monday to Friday: bits 1 to 5), over seven virtual days in New York.
        let clock = VirtualClock::new(utc_ms(2026, 11, 16, 0, 0), Zone::new_york());
        let _guard = clock.install();
        let weekdays = schedule("standup", ScheduleSpec::At { minutes: 9 * 60 + 30, days: 0b0011_1110, tz: "America/New_York".into() }, clock.now());
        let mut state = ScheduleState::default();
        let (runs, skips) = run_through(&clock, &[weekdays], &mut state, utc_ms(2026, 11, 23, 0, 0), 10 * MINUTE);
        assert!(skips.is_empty());
        // 16 November 2026 is a Monday: five runs, each at 09:30 local.
        assert_eq!(runs.len(), 5);
        assert!(runs.iter().all(|(_, local)| *local == (9, 30)));
        // A playbook with a schedule trigger is a schedule of the same id.
        let p = Playbook { trigger: Trigger::Schedule { schedule: at(60) }, ..crate::playbooks::builtins::entry(&crate::playbooks::builtins::BUILTINS[0]) };
        assert_eq!(from_playbook(&p, 5).map(|s| (s.id, s.made_ms)), Some(("ci-fix".to_string(), 5)));
    }
}
