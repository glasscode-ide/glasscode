//! The dry run (M40 step 6; review events-11): playbooks run against past events without acting,
//! to show what they would have done. Each would-be firing counts toward the limits of the ones
//! after it, as if it had happened, so the result is what the run would have been. The events come
//! from the journal (the last 7 days, M14) through [`EventView`]; nothing is claimed or run.

use crate::effects::{EffectRecord, EffectState};
use crate::paths::MachineId;

use super::engine::{self, EventView, Facts, Firing, HeldBack};
use super::limits::{LedgerCounts, origin_of};
use super::model::{Playbook, Schema};

/// How far back a dry run reads by default.
pub const DAYS: u64 = 7;

/// What a dry run found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// What would have fired, in the order of the events.
    pub fired: Vec<Firing>,
    /// What matched and was held back, with the event's id.
    pub held: Vec<(String, HeldBack)>,
    /// How many events were read.
    pub events: usize,
}

/// Run `playbooks` against `events` (oldest first), each at its own time (`at_ms` gives it),
/// starting from what the effect ledger already holds (`counts`). Nothing acts.
pub fn dry_run<'a, E: EventView + 'a>(
    playbooks: &[Playbook],
    events: impl IntoIterator<Item = (&'a E, u64)>,
    schema: &Schema,
    facts: &dyn Fn(u64) -> Facts<'a>,
    mut counts: LedgerCounts,
) -> Report {
    let home = MachineId::parse("dry-run").expect("a machine id");
    let mut report = Report::default();
    for (event, at_ms) in events {
        report.events += 1;
        let (fired, held) = engine::fire(playbooks, event, schema, &facts(at_ms), &counts);
        for firing in &fired {
            // As if it happened: it counts for the limits of what comes after.
            for playbook in std::iter::once(&firing.playbook).chain(&firing.merged) {
                counts.add(EffectRecord {
                    v: crate::effects::SCHEMA,
                    key: firing.key,
                    op: firing.op.clone(),
                    retry: crate::ops::Retry::Verifiable,
                    origin: origin_of(playbook),
                    subject: firing.subject.clone(),
                    target: firing.target.clone(),
                    caller: String::new(),
                    reason: String::new(),
                    project: None,
                    session: None,
                    home: home.clone(),
                    generation: 0,
                    launch: String::new(),
                    claimed_ms: at_ms,
                    attempt: 1,
                    evidence: serde_json::Value::Null,
                    state: EffectState::Done(serde_json::Value::Null),
                    finished_ms: Some(at_ms),
                });
            }
        }
        report.fired.extend(fired);
        report.held.extend(held.into_iter().map(|held| (event.id().to_string(), held)));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::Level;
    use crate::paths::ProjectKey;
    use crate::playbooks::engine::tests::{TestEvent, playbook, red, schema};
    use crate::playbooks::limits::Held;
    use crate::playbooks::model::Autonomy;

    #[test]
    fn a_dry_run_shows_what_would_have_fired_and_acts_on_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let before = std::env::var_os("GLASSCODE_HOME");
        // SAFETY: the core tests run one at a time (`--test-threads=1`).
        unsafe { std::env::set_var("GLASSCODE_HOME", dir.path()) };
        let careful = |_: Option<&ProjectKey>| Level::Careful;
        let me = ["me-dev".to_string()];
        let facts = |now_ms: u64| Facts { level: &careful, me: &me, now_ms };
        let events: Vec<TestEvent> = vec![red("e1", "aaa"), red("e2", "aaa"), red("e3", "bbb"), red("e4", "bbb")];
        let at = 1_800_000_000_000u64;
        let report = dry_run(&[playbook("fix", Autonomy::Act)], events.iter().enumerate().map(|(n, e)| (e, at + n as u64 * 60_000)), &schema(), &facts, LedgerCounts::default());
        assert_eq!(report.events, 4);
        // Once per commit: the first event of each head fires, the second is held by the limit.
        assert_eq!(report.fired.iter().map(|f| f.event.as_str()).collect::<Vec<_>>(), ["e1", "e3"]);
        assert_eq!(report.held.iter().map(|(event, held)| (event.as_str(), held.why.clone())).collect::<Vec<_>>(), [("e2", Held::Limit { count: 1 }), ("e4", Held::Limit { count: 1 })]);
        // Nothing was claimed.
        assert!(!crate::paths::effects_dir().exists());
        match before {
            Some(value) => unsafe { std::env::set_var("GLASSCODE_HOME", value) },
            None => unsafe { std::env::remove_var("GLASSCODE_HOME") },
        }
    }
}
