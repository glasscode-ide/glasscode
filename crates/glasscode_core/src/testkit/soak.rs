//! The soak (slice M55, slice plan section 14): the reference load for seven simulated days on the
//! virtual clock, with sleep and wake, network loss, server partitions and restarts at random
//! points, through the code that runs on it: the turns (M30), the effect keys each report is
//! delivered under (M31), the wake queue and its budget (M51), and the journal's retention (M14).
//!
//! It asserts what section 14 asks:
//! - zero duplicate effects;
//! - exactly one report per turn;
//! - disk use bounded by the retention rules (the journal keeps 30 UTC days, whatever the run's length);
//! - no growth in what the runtime holds (one turn machine per session; the queue bounded);
//! - wakes per hour under the D6 cap (`runtime::budget::WAKES_PER_HOUR`, urgent ones apart).
//!
//! `seven_day_soak` is the long one: CI's soak job runs it (`--ignored`). `one_day_soak` runs with
//! the other tests. Neither calls a model, nor starts a thread.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::clock::{HOUR, VirtualClock, Zone, utc_ms};
use super::load::{self, Reference};
use super::sim::{self, Input, Record, Runtime, Script};
use crate::effects::effect_key;
use crate::runtime::budget::{Ledger, WAKES_PER_HOUR};
use crate::runtime::lanes::{Hold, next_wake};
use crate::runtime::queue::{Priority, Queue, Wake};
use crate::turns::{Change, Signal, TurnMachine};

/// What the soak runs: a turn machine per session, a report's effect for each closed turn, and a
/// wake for each turn that ends on a failure (urgent) or every tenth other end (a playbook's).
#[derive(Default)]
struct Soak {
    machines: BTreeMap<String, TurnMachine>,
    queue: Queue,
    ledger: Ledger,
    /// When each wake ran, and whether it was urgent.
    wakes: Vec<(u64, bool)>,
    ends: u64,
    largest_queue: usize,
    /// The server is cut off (a partition): wakes wait, nothing is lost.
    partitioned: bool,
}

impl Soak {
    fn closed(&mut self, now: u64, session: &str, changes: Vec<Change>, record: &mut Record) {
        for change in changes {
            let Change::Closed(closed) = change else { continue };
            record.reports.push((session.to_string(), closed.turn.id.clone(), closed.end.name().to_string()));
            // Each report is delivered once, under its effect key (D9).
            let key = effect_key("turn-report", &format!("{session}:{}", closed.turn.id), "notify", session);
            record.effects.push((now, key.to_string(), "notify".into()));
            self.ends += 1;
            let urgent = matches!(closed.end, crate::turns::TurnEnd::Failed(_) | crate::turns::TurnEnd::Crashed);
            if urgent || self.ends % 10 == 0 {
                let priority = if urgent { Priority::Urgent } else { Priority::Playbook };
                self.queue.push(Wake::new(format!("{session}:{}", closed.turn.id), priority, now), now);
                self.largest_queue = self.largest_queue.max(self.queue.len());
            }
        }
    }

    fn all(&mut self, now: u64, signal: &Signal, record: &mut Record) {
        let sessions: Vec<String> = self.machines.keys().cloned().collect();
        for session in sessions {
            let changes = self.machines.get_mut(&session).unwrap().feed(now, signal);
            self.closed(now, &session, changes, record);
        }
    }
}

impl Runtime for Soak {
    fn input(&mut self, now: u64, input: &Input, record: &mut Record) {
        let signal = match input {
            Input::Hook { payload, .. } => Signal::Hook(serde_json::from_str(payload).unwrap_or(Value::Null)),
            Input::Rollout { line, .. } => Signal::Rollout(serde_json::from_str(line).unwrap_or(Value::Null)),
            Input::Write { text, .. } => Signal::TranscriptLine(text.clone()),
            Input::Kill { .. } => Signal::Killed,
            Input::Restart { crashed } => return self.all(now, if *crashed { &Signal::Killed } else { &Signal::AppQuit }, record),
            Input::Sleep { .. } => return self.all(now, &Signal::Slept, record),
            Input::Other { kind, .. } if kind == "partition" => {
                self.partitioned = !self.partitioned;
                return;
            }
            _ => return,
        };
        let Some(session) = input.session().map(str::to_string) else { return };
        let changes = self.machines.entry(session.clone()).or_default().feed(now, &signal);
        self.closed(now, &session, changes, record);
    }

    fn tick(&mut self, now: u64, _record: &mut Record) {
        if self.partitioned {
            return;
        }
        let hold = Hold::default();
        // A few a tick at most: the budget, not the tick, is what holds wakes back.
        for _ in 0..4 {
            let (wake, _) = next_wake(&mut self.queue, &hold, &self.ledger, now, &|_| true);
            let Some(wake) = wake else { break };
            let urgent = wake.priority == Priority::Urgent;
            if !urgent {
                self.ledger.record_wake(now);
            }
            self.wakes.push((now, urgent));
        }
    }
}

/// The reference load for `days`, with sleeps, restarts, crashes and partitions at random points.
fn script(start: u64, days: usize, seed: u64) -> Script {
    let mut script = Script { items: load::events(Reference::small(), start, days, seed).collect() };
    let mut rng = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut next = |below: u64| {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (rng >> 33) % below.max(1)
    };
    let end = start + days as u64 * 24 * HOUR;
    for _ in 0..days * 3 {
        let at = start + next(end - start);
        let input = match next(4) {
            0 => Input::Sleep { ms: (1 + next(8)) * HOUR },
            1 => Input::Restart { crashed: false },
            2 => Input::Restart { crashed: true },
            _ => Input::Other { kind: "partition".into(), data: Value::Null },
        };
        script.items.push((at, input));
    }
    script
}

fn soak(days: usize, seed: u64) {
    let start = utc_ms(2026, 11, 19, 0, 0);
    let script = script(start, days, seed);
    let clock = VirtualClock::new(start, Zone::utc());
    let mut runtime = Soak::default();
    let record = sim::run(&clock, &script, &mut runtime, 1_000);

    // Zero duplicate effects; exactly one report per turn.
    assert_eq!(sim::no_duplicate_effects(&record), Ok(()));
    assert_eq!(sim::one_report_per_turn(&script, &record), Ok(()));
    assert!(record.reports.len() as u64 >= 10_000 * days as u64 / 2, "{} reports", record.reports.len());

    // What the runtime holds does not grow with time: one machine per session, a bounded queue.
    assert!(runtime.machines.len() <= Reference::small().sessions, "{} machines", runtime.machines.len());
    assert!(runtime.largest_queue <= 4096, "the queue held {}", runtime.largest_queue);

    // Wakes per hour under the cap (routine ones; urgent ones are not held by the hour's cap).
    let routine: Vec<u64> = runtime.wakes.iter().filter(|(_, urgent)| !urgent).map(|(at, _)| *at).collect();
    for (n, at) in routine.iter().enumerate() {
        let in_hour = routine[n..].iter().take_while(|later| **later < at + HOUR).count();
        assert!(in_hour <= WAKES_PER_HOUR as usize, "{in_hour} wakes in the hour from {at}");
    }
    assert!(!routine.is_empty(), "the soak ran no wake");

    // Disk bounded by retention: a journal file a day, rotated each day, keeps 30 of them.
    let dir = tempfile::tempdir().unwrap();
    let before = std::env::var_os("GLASSCODE_HOME");
    // SAFETY: the core tests run one at a time (`--test-threads=1`).
    unsafe { std::env::set_var("GLASSCODE_HOME", dir.path()) };
    let journal = crate::paths::journal_dir();
    std::fs::create_dir_all(&journal).unwrap();
    let mut kept = BTreeSet::new();
    for day in 0..(days as u64 + 40) {
        let now = start + day * 24 * HOUR;
        let name = crate::day::iso((now / 86_400_000) as i64) + ".jsonl";
        std::fs::write(journal.join(&name), "{}\n").unwrap();
        crate::journal::rotate(now);
        kept = std::fs::read_dir(&journal).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert!(kept.len() <= crate::journal::KEEP_DAYS as usize, "day {day}: {} files", kept.len());
    }
    assert_eq!(kept.len(), crate::journal::KEEP_DAYS as usize);
    match before {
        Some(value) => unsafe { std::env::set_var("GLASSCODE_HOME", value) },
        None => unsafe { std::env::remove_var("GLASSCODE_HOME") },
    }
}

#[test]
fn one_day_soak() {
    soak(1, 3);
}

/// Section 14's seven days: CI's soak job runs it (`cargo test … seven_day_soak -- --ignored`).
#[test]
#[ignore = "the soak: CI's soak job runs it"]
fn seven_day_soak() {
    for seed in [1, 2, 3] {
        soak(7, seed);
    }
}
