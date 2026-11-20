//! Glasscode's own playbooks (M40 step 3; review events-12, exec-16): native, their rules in code.
//!
//! Each has an entry in the playbook table ([`entry`]) for its settings and its pause; its rules
//! are state machines here or in the module they come from. The CI and conflict fix is the
//! first: its engine is `autofix` (once per commit, three tries in a row, reset when CI passes,
//! conflicts first, a wait for the rest of CI), and it is app behaviour (exec-16). It runs with the
//! manager off, needs no approval, and a project's level does not change it. Its switches stay in
//! `state.json`, where older builds read them.
//!
//! The others (needs you, review received, follow-ups, the brief, a stuck session, usage near the
//! limit) are v1's harness rules today; they move here with the journal's events (M14, M30).

use crate::autofix::{self, Ask, Enabled, Plan, Record};
use crate::effects::{EffectKey, Intent, effect_key};
use crate::github::CiStatus;
use crate::ops::Retry;

use super::model::{Action, Autonomy, EventFilter, Limit, Per, Playbook, Trigger};

/// One of Glasscode's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Builtin {
    pub id: &'static str,
    pub name: &'static str,
    /// What it does, in the person's words.
    pub what: &'static str,
    /// The event kind it reacts to.
    pub on: &'static str,
    /// The op it runs.
    pub op: &'static str,
    pub autonomy: Autonomy,
    pub limit: Limit,
    /// It runs only while the manager is on; the CI and conflict fix do not (exec-16).
    pub needs_manager: bool,
    /// The v1 rule it carries on (for the parity tests and M44).
    pub v1: &'static str,
    /// The slice that brings the event kind it reacts to, when the journal's catalogue has no
    /// such kind yet.
    pub planned: Option<&'static str>,
}

/// Every built-in, in the order Settings lists them.
pub const BUILTINS: &[Builtin] = &[
    Builtin {
        id: ci_fix::ID,
        name: "Fix failing CI",
        what: "When CI fails on a session's own PR, ask its agent to fix it: once per commit, three times in a row at most.",
        on: "ci.failed",
        op: "session.send",
        autonomy: Autonomy::Act,
        limit: Limit { count: 1, per: Per::Commit },
        needs_manager: false,
        v1: "ci_fix (autofix, CI)",
        planned: None,
    },
    Builtin {
        id: ci_fix::CONFLICTS,
        name: "Resolve merge conflicts",
        what: "When a session's own PR conflicts with its base, ask its agent to resolve it: once per pair of commits, three times in a row at most.",
        on: "pr.conflict",
        op: "session.send",
        autonomy: Autonomy::Act,
        limit: Limit { count: 1, per: Per::Commit },
        needs_manager: false,
        v1: "ci_fix (autofix, conflicts)",
        planned: None,
    },
    Builtin {
        id: "needs-you",
        name: "Tell me when a session needs me",
        what: "A notification when a session stops to ask something, once per stop.",
        on: "session.needs_input",
        op: "app.notify",
        autonomy: Autonomy::Tell,
        limit: Limit { count: 1, per: Per::Session },
        needs_manager: true,
        v1: "harness: needs|<session>",
        planned: None,
    },
    Builtin {
        id: "review-tell",
        name: "Tell a session about its review",
        what: "When someone reviews a session's PR, tell the session once it is free, each review once (from trusted authors).",
        on: "review.received",
        op: "session.send",
        autonomy: Autonomy::Tell,
        limit: Limit { count: 1, per: Per::Pr },
        needs_manager: true,
        v1: "harness: tell|<session>|<repo>#<n>|<login>",
        planned: Some("M40 part 2 (review events from the status rounds)"),
    },
    Builtin {
        id: "follow-ups",
        name: "Follow-ups",
        what: "Tell me when a teammate fixed what I asked for, and bring it up again after 48 quiet hours.",
        on: "review.given",
        op: "app.notify",
        autonomy: Autonomy::Tell,
        limit: Limit { count: 1, per: Per::Pr },
        needs_manager: true,
        v1: "harness: follow-up keys",
        planned: Some("M40 part 2 (the person\'s given reviews)"),
    },
    Builtin {
        id: "daily-brief",
        name: "The daily brief",
        what: "Today's update at the time set in Settings.",
        on: "clock.brief_due",
        op: "manager.brief",
        autonomy: Autonomy::Tell,
        limit: Limit { count: 1, per: Per::Day },
        needs_manager: true,
        v1: "harness: daily_brief",
        planned: Some("M41 (the scheduler)"),
    },
    Builtin {
        id: "stuck-session",
        name: "A session is stuck",
        what: "Tell me when a session's turn runs long, or it keeps ending on the same error.",
        on: "turn.ended",
        op: "app.notify",
        autonomy: Autonomy::Tell,
        limit: Limit { count: 1, per: Per::Session },
        needs_manager: true,
        v1: "none (new)",
        planned: None,
    },
    Builtin {
        id: "usage-handoff",
        name: "Usage near the limit",
        what: "When an account nears its usage limit, suggest handing its sessions to another.",
        on: "usage.threshold",
        op: "app.notify",
        autonomy: Autonomy::Tell,
        limit: Limit { count: 1, per: Per::Day },
        needs_manager: true,
        v1: "none (new)",
        planned: None,
    },
];

pub fn find(id: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.id == id)
}

/// A built-in's entry in the playbook table: its settings, switched on.
pub fn entry(builtin: &Builtin) -> Playbook {
    Playbook {
        id: builtin.id.to_string(),
        name: builtin.name.to_string(),
        trigger: Trigger::Event(EventFilter { kind: builtin.on.to_string(), fields: Default::default() }),
        when: Vec::new(),
        then: Action { op: builtin.op.to_string(), args: Default::default() },
        autonomy: builtin.autonomy,
        limit: builtin.limit,
        builtin: true,
        enabled: true,
        manager_events: false,
        proposed_by: None,
        paused: None,
        released_ms: None,
        approved_hash: None,
    }
}

/// Whether a built-in runs now: switched on, and the manager on when it needs it. The CI and
/// conflict fix need no manager, no approval and no level (exec-16).
pub fn runs(builtin: &Builtin, switched_on: bool, manager_on: bool) -> bool {
    switched_on && (manager_on || !builtin.needs_manager)
}

/// The CI and conflict fix: `autofix` is its engine.
pub mod ci_fix {
    use super::*;

    pub const ID: &str = "ci-fix";
    pub const CONFLICTS: &str = "conflict-fix";

    /// What one round decides about one PR.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Step {
        /// Nothing to ask, or not yet (the rest of CI is still running).
        Nothing,
        /// Three asks in a row: over to the person.
        GiveUp { conflicts: bool },
        Ask(Ask),
    }

    /// One round for one PR, as v1's tick does it: the record is reset when CI passed or the
    /// branch merges again (true: it changed), then `autofix` plans. `ci_waited`: CI that failed
    /// has finished, or waited [`autofix::WAIT_FOR_CI`].
    pub fn step(record: &mut Record, ci: &CiStatus, enabled: Enabled, ci_waited: bool) -> (Step, bool) {
        let changed = autofix::reset(record, ci);
        let step = match autofix::plan(record, ci, enabled, ci_waited) {
            Plan::Nothing | Plan::Later => Step::Nothing,
            Plan::GiveUp { conflicts } => Step::GiveUp { conflicts },
            Plan::Ask(ask) => Step::Ask(ask),
        };
        (step, changed)
    }

    /// What an ask is about: `acme/app#42@<head>`, or the pair of commits for conflicts. This is
    /// the effect key's subject since M31; it must not change, or every open head is asked again.
    pub fn subject(pr_key: &str, ci: &CiStatus, ask: Ask) -> String {
        format!("{pr_key}@{}", if ask.conflicts { ci.merge_key() } else { ci.head.clone() })
    }

    /// The delivery group whose newer commit drops an older ask still waiting.
    pub fn group(pr_key: &str, ask: Ask) -> String {
        format!("ci-fix:{pr_key}:{}", if ask.conflicts { "conflicts" } else { "ci" })
    }

    /// The effect key of an ask to a session.
    pub fn key(subject: &str, session: &str) -> EffectKey {
        effect_key("ci-fix", subject, "session.send", session)
    }

    /// The intent of an ask, for the effect ledger.
    pub fn intent(subject: &str, session: &str, pr: u64, conflicts: bool, launch: &str) -> Intent {
        let reason = if conflicts { format!("PR #{pr} conflicts with its base") } else { format!("CI failed on PR #{pr}") };
        Intent {
            origin: "ci-fix".into(),
            subject: subject.to_string(),
            target: session.to_string(),
            caller: "Glasscode's CI fix".into(),
            reason,
            session: Some(session.to_string()),
            launch: launch.to_string(),
            ..Intent::new(key(subject, session), "session.send", Retry::Verifiable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ci_fix::Step;
    use super::*;
    use crate::github::CheckRun;

    fn ci(head: &str, failed: bool, running: bool, mergeable: &str, base_head: &str) -> CiStatus {
        let check = |name: &str, status: &str, conclusion: &str| CheckRun { name: name.into(), status: status.into(), conclusion: conclusion.into(), ..CheckRun::default() };
        let mut checks = Vec::new();
        if failed {
            checks.push(check("test", "COMPLETED", "FAILURE"));
        }
        if running {
            checks.push(check("lint", "IN_PROGRESS", ""));
        }
        if checks.is_empty() {
            checks.push(check("test", "COMPLETED", "SUCCESS"));
        }
        CiStatus { head: head.into(), state: "OPEN".into(), checks, mergeable: mergeable.into(), base: "main".into(), base_head: base_head.into() }
    }

    /// v1's tick, as `ci_fix_tick` runs it, on one PR's rounds: what it asks or gives up on.
    fn v1(rounds: &[CiStatus], enabled: Enabled) -> Vec<String> {
        let mut record = Record::default();
        let mut out = Vec::new();
        for (n, ci) in rounds.iter().enumerate() {
            autofix::reset(&mut record, ci);
            let waited = ci.failed().is_empty() || record.head == ci.head || !ci.running();
            match autofix::plan(&record, ci, enabled, waited) {
                Plan::Nothing | Plan::Later => {}
                Plan::GiveUp { conflicts } => {
                    autofix::gave_up(&mut record, ci, conflicts);
                    out.push(format!("{n}: give up {conflicts}"));
                }
                Plan::Ask(ask) => {
                    autofix::asked(&mut record, ci, ask, "s1", n as u64);
                    out.push(format!("{n}: ask {} {}", ask.conflicts, ask.ci));
                }
            }
        }
        out
    }

    /// The built-in on the same rounds.
    fn builtin(rounds: &[CiStatus], enabled: Enabled) -> Vec<String> {
        let mut record = Record::default();
        let mut out = Vec::new();
        for (n, ci) in rounds.iter().enumerate() {
            let waited = ci.failed().is_empty() || record.head == ci.head || !ci.running();
            match ci_fix::step(&mut record, ci, enabled, waited).0 {
                Step::Nothing => {}
                Step::GiveUp { conflicts } => {
                    autofix::gave_up(&mut record, ci, conflicts);
                    out.push(format!("{n}: give up {conflicts}"));
                }
                Step::Ask(ask) => {
                    autofix::asked(&mut record, ci, ask, "s1", n as u64);
                    out.push(format!("{n}: ask {} {}", ask.conflicts, ask.ci));
                }
            }
        }
        out
    }

    #[test]
    fn parity_with_autofix() {
        let on = Enabled { ci: true, conflicts: true, tries: autofix::MAX_TRIES };
        // Red three times on three commits, a fourth: over to the person. Green again resets it.
        let scenarios: Vec<Vec<CiStatus>> = vec![
            vec![ci("a", true, false, "MERGEABLE", "m1"), ci("a", true, false, "MERGEABLE", "m1"), ci("b", true, false, "MERGEABLE", "m1"), ci("c", true, false, "MERGEABLE", "m1"), ci("d", true, false, "MERGEABLE", "m1"), ci("e", false, false, "MERGEABLE", "m1"), ci("f", true, false, "MERGEABLE", "m1")],
            // Still running: wait for the rest.
            vec![ci("a", true, true, "MERGEABLE", "m1"), ci("a", true, false, "MERGEABLE", "m1")],
            // Conflicts come first, and carry the CI that failed on the same commit.
            vec![ci("a", true, false, "CONFLICTING", "m1"), ci("a", true, false, "CONFLICTING", "m1"), ci("a", true, false, "CONFLICTING", "m2"), ci("b", false, false, "MERGEABLE", "m2")],
            // GitHub has not decided yet.
            vec![ci("a", false, false, "UNKNOWN", "m1"), ci("a", false, false, "CONFLICTING", "m1")],
        ];
        for (n, rounds) in scenarios.iter().enumerate() {
            let old = v1(rounds, on);
            assert_eq!(builtin(rounds, on), old, "scenario {n}");
            assert!(!old.is_empty(), "scenario {n} asks something");
        }
        // Switched off in Settings, neither asks.
        let off = Enabled { ci: false, conflicts: false, tries: 3 };
        assert!(builtin(&scenarios[0], off).is_empty() && v1(&scenarios[0], off).is_empty());
        // The key is M31's, to the letter: an ask after this lands is not a new effect.
        let ask = Ask { conflicts: false, ci: true, try_no: 1, of: 3 };
        let status = ci("abc", true, false, "MERGEABLE", "m1");
        let subject = ci_fix::subject("acme/app#42", &status, ask);
        assert_eq!(subject, "acme/app#42@abc");
        assert_eq!(ci_fix::key(&subject, "s1"), effect_key("ci-fix", "acme/app#42@abc", "session.send", "s1"));
        assert_eq!(ci_fix::group("acme/app#42", ask), "ci-fix:acme/app#42:ci");
        assert_eq!(ci_fix::intent(&subject, "s1", 42, false, "run").key, ci_fix::key(&subject, "s1"));
    }

    #[test]
    fn ci_fix_runs_with_manager_off() {
        let fix = find(ci_fix::ID).unwrap();
        assert!(runs(fix, true, false), "the manager is off: it still runs");
        assert!(!runs(fix, false, true), "its own switch in Settings › Automation stops it");
        assert!(runs(find(ci_fix::CONFLICTS).unwrap(), true, false));
        // The manager's own need the manager.
        assert!(!runs(find("review-tell").unwrap(), true, false));
        assert!(runs(find("review-tell").unwrap(), true, true));
        // Every built-in's entry is a valid built-in playbook, with an op the registry has, and
        // its event kind is in the journal's catalogue or names the slice that brings it.
        for builtin in BUILTINS {
            let known = crate::journal::CATALOGUE.iter().any(|info| info.kind == builtin.on);
            assert!(known != builtin.planned.is_some(), "{}: {} is {} the catalogue", builtin.id, builtin.on, if known { "in" } else { "not in" });
            let entry = entry(builtin);
            assert_eq!(super::super::model::validate(&entry, &Default::default()), Ok(()), "{}", builtin.id);
            assert!(crate::ops::find(builtin.op).is_some(), "{}: {}", builtin.id, builtin.op);
        }
    }
}
