//! When Glasscode asks a session's agent to fix its own pull request by itself: CI that fails,
//! and conflicts with the branch it merges into. The app (`ci_fix` in the glasscode crate)
//! watches the PRs and types the message in; this decides what to ask and when:
//!
//! - CI: once per commit, when CI has finished on it (or after a wait, so the agent hears of
//!   every failing check at once);
//! - conflicts: once per pair of commits (the PR's head and its base's), as soon as GitHub
//!   says the branch conflicts. GitHub says UNKNOWN while it works that out: nothing happens
//!   until it knows. A push, or a new commit on the base that conflicts again, asks again;
//! - conflicts come first: while a PR conflicts, its CI is moot (GitHub runs no workflows on a
//!   conflicting PR, and the rebase makes a new commit anyway), so CI that failed on the same
//!   commit goes in the same message and CI is not asked about on its own;
//! - three asks in a row at most, for each; then it is over to the person, until CI passes
//!   (or GitHub can merge the branch) again.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::github::CiStatus;

pub const MAX_TRIES: u32 = 3;
/// How long to wait for CI's other checks once one has failed.
pub const WAIT_FOR_CI: Duration = Duration::from_secs(600);

/// What Glasscode did about one PR (`<owner/repo>#<n>`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// The commit last asked about (or given up on) for CI.
    pub head: String,
    /// CI asks since CI last passed.
    pub tries: u32,
    /// Unix seconds of the last CI ask.
    pub asked_at: u64,
    pub session: String,
    pub gave_up: bool,
    /// The head and base commits last asked about (or given up on) for conflicts.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub conflict: String,
    /// Conflict asks since the branch last merged cleanly.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub conflict_tries: u32,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub conflict_asked_at: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conflict_gave_up: bool,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// Which of the two Glasscode may ask about (the settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Enabled {
    pub ci: bool,
    pub conflicts: bool,
    /// Asks in a row before it is over to the person (Settings › Automation; 3 unless chosen).
    pub tries: u32,
}

/// One message to the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ask {
    pub conflicts: bool,
    pub ci: bool,
    /// The ask's number (of `of`) for what the message leads with.
    pub try_no: u32,
    pub of: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Nothing to ask (or asked already).
    Nothing,
    /// CI failed but is still running: wait for the rest.
    Later,
    /// Out of asks: over to the person.
    GiveUp { conflicts: bool },
    Ask(Ask),
}

/// What to do about a PR now. `ci_waited` says the wait for CI's other checks is over.
pub fn plan(record: &Record, ci: &CiStatus, enabled: Enabled, ci_waited: bool) -> Plan {
    if ci.state != "OPEN" || ci.head.is_empty() {
        return Plan::Nothing;
    }
    let ci_due = enabled.ci && !ci.failed().is_empty() && record.head != ci.head;
    if enabled.conflicts && ci.conflicting() {
        if record.conflict == ci.merge_key() {
            return Plan::Nothing; // asked (or given up) on this pair; CI waits for the rebase
        }
        if record.conflict_tries >= enabled.tries {
            return Plan::GiveUp { conflicts: true };
        }
        let ci = ci_due && record.tries < enabled.tries;
        return Plan::Ask(Ask { conflicts: true, ci, try_no: record.conflict_tries + 1, of: enabled.tries });
    }
    if !ci_due {
        return Plan::Nothing;
    }
    if ci.running() && !ci_waited {
        return Plan::Later;
    }
    if record.tries >= enabled.tries {
        return Plan::GiveUp { conflicts: false };
    }
    Plan::Ask(Ask { conflicts: false, ci: true, try_no: record.tries + 1, of: enabled.tries })
}

/// Start the counts again once CI passes or the branch merges cleanly. True when it changed.
pub fn reset(record: &mut Record, ci: &CiStatus) -> bool {
    let mut changed = false;
    if !ci.checks.is_empty() && ci.failed().is_empty() && !ci.running() && (record.tries > 0 || record.gave_up) {
        record.tries = 0;
        record.gave_up = false;
        changed = true;
    }
    // Merging cleanly again also forgets the pair asked about: a later conflict is a new one.
    if ci.merges_cleanly() && (record.conflict_tries > 0 || record.conflict_gave_up || !record.conflict.is_empty()) {
        record.conflict_tries = 0;
        record.conflict_gave_up = false;
        record.conflict.clear();
        changed = true;
    }
    changed
}

/// Note an ask that went out.
pub fn asked(record: &mut Record, ci: &CiStatus, ask: Ask, session: &str, now: u64) {
    if ask.conflicts {
        record.conflict = ci.merge_key();
        record.conflict_tries += 1;
        record.conflict_asked_at = now;
        record.conflict_gave_up = false;
    }
    if ask.ci {
        record.head = ci.head.clone();
        record.tries += 1;
        record.asked_at = now;
        record.gave_up = false;
    }
    record.session = session.to_string();
}

/// Note giving up, so it is said once for this commit (or pair of commits).
pub fn gave_up(record: &mut Record, ci: &CiStatus, conflicts: bool) {
    if conflicts {
        record.conflict = ci.merge_key();
        record.conflict_gave_up = true;
    } else {
        record.head = ci.head.clone();
        record.gave_up = true;
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn failed_checks(ci: &CiStatus) -> String {
    ci.failed().iter().map(|c| format!("{} ({})", c.label(), c.url)).collect::<Vec<_>>().join("; ")
}

/// The message the agent gets.
pub fn message(repo: &str, number: u64, url: &str, ci: &CiStatus, ask: Ask) -> String {
    if ask.conflicts {
        let base = if ci.base.is_empty() { "its base branch".to_string() } else { ci.base.clone() };
        let remote = if ci.base.is_empty() { "the base branch from origin".to_string() } else { format!("origin/{}", ci.base) };
        let also = if ask.ci { format!(" CI also failed on this commit: {}. Check it again after the rebase and fix what still fails.", failed_checks(ci)) } else { String::new() };
        return format!(
            "PR #{number} ({url}) has merge conflicts with {base} (head {}). Fetch origin, rebase this branch on {remote} (or merge {remote} into it if others share the branch), resolve the conflicts keeping both sides' intent, run the checks you can, then push (with --force-with-lease after a rebase).{also} (Sent by Glasscode because GitHub reports conflicts; ask {} of {}.)",
            short(&ci.head),
            ask.try_no,
            ask.of
        );
    }
    let mut runs: Vec<u64> = ci.failed().iter().filter_map(|c| c.run_id()).collect();
    runs.sort();
    runs.dedup();
    let logs = if runs.is_empty() {
        "Open the failing checks' links to see why.".to_string()
    } else {
        format!("Read the failing logs with `gh run view <run-id> --log-failed --repo {repo}` (runs: {}).", runs.iter().map(u64::to_string).collect::<Vec<_>>().join(", "))
    };
    format!(
        "CI failed on PR #{number} ({url}) for commit {}: {}. {logs} Fix the cause on this branch, run the same checks locally if you can, then commit and push. If the failure has nothing to do with this branch (a flaky test or CI itself), say so and change nothing. (Sent by Glasscode because CI failed; ask {} of {}.)",
        short(&ci.head),
        failed_checks(ci),
        ask.try_no,
        ask.of
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::{CheckRun, ci_from_json};

    const BOTH: Enabled = Enabled { ci: true, conflicts: true, tries: MAX_TRIES };

    fn status(head: &str, base_head: &str, mergeable: &str, checks: &[(&str, &str)]) -> CiStatus {
        CiStatus {
            head: head.into(),
            state: "OPEN".into(),
            checks: checks
                .iter()
                .map(|(name, conclusion)| CheckRun {
                    name: name.to_string(),
                    workflow: "CI".into(),
                    status: if conclusion.is_empty() { "IN_PROGRESS".into() } else { "COMPLETED".into() },
                    conclusion: conclusion.to_string(),
                    url: "https://github.com/o/r/actions/runs/7/job/1".into(),
                    ..Default::default()
                })
                .collect(),
            mergeable: mergeable.into(),
            base: "main".into(),
            base_head: base_head.into(),
        }
    }

    /// Run the plan and, when it asks, note the ask: what a tick does.
    fn tick(record: &mut Record, ci: &CiStatus) -> Plan {
        reset(record, ci);
        let plan = plan(record, ci, BOTH, true);
        match plan {
            Plan::Ask(ask) => asked(record, ci, ask, "s1", 100),
            Plan::GiveUp { conflicts } => gave_up(record, ci, conflicts),
            _ => {}
        }
        plan
    }

    #[test]
    fn reads_mergeable_states() {
        let json = |m: &str| serde_json::json!({"headRefOid": "abc1234def", "state": "OPEN", "statusCheckRollup": [], "mergeable": m, "baseRefName": "main", "baseRefOid": "base000111"});
        let conflicting = ci_from_json(&json("CONFLICTING"));
        assert!(conflicting.conflicting() && !conflicting.merges_cleanly());
        assert_eq!((conflicting.base.as_str(), conflicting.merge_key().as_str()), ("main", "abc1234def:base000111"));
        let clean = ci_from_json(&json("MERGEABLE"));
        assert!(clean.merges_cleanly() && !clean.conflicting());
        let unknown = ci_from_json(&json("UNKNOWN"));
        assert!(!unknown.conflicting() && !unknown.merges_cleanly());
        // An older gh, or a field missing: neither.
        let none = ci_from_json(&serde_json::json!({"headRefOid": "abc", "state": "OPEN"}));
        assert!(!none.conflicting() && !none.merges_cleanly());
    }

    #[test]
    fn conflicts_are_asked_once_per_head_and_base() {
        let mut r = Record::default();
        // GitHub has not worked it out yet: nothing.
        assert_eq!(tick(&mut r, &status("h1", "b1", "UNKNOWN", &[])), Plan::Nothing);
        // Conflicts: ask 1, and only once for the same pair.
        assert_eq!(tick(&mut r, &status("h1", "b1", "CONFLICTING", &[])), Plan::Ask(Ask { conflicts: true, ci: false, try_no: 1, of: MAX_TRIES }));
        assert_eq!(tick(&mut r, &status("h1", "b1", "CONFLICTING", &[])), Plan::Nothing);
        assert_eq!(tick(&mut r, &status("h1", "b1", "UNKNOWN", &[])), Plan::Nothing);
        // The agent pushed but it still conflicts: ask 2.
        assert_eq!(tick(&mut r, &status("h2", "b1", "CONFLICTING", &[])), Plan::Ask(Ask { conflicts: true, ci: false, try_no: 2, of: MAX_TRIES }));
        // The base moved and it conflicts again with the same head: ask 3.
        assert_eq!(tick(&mut r, &status("h2", "b2", "CONFLICTING", &[])), Plan::Ask(Ask { conflicts: true, ci: false, try_no: 3, of: MAX_TRIES }));
        // Out of asks: over to the person, said once.
        assert_eq!(tick(&mut r, &status("h3", "b2", "CONFLICTING", &[])), Plan::GiveUp { conflicts: true });
        assert_eq!(tick(&mut r, &status("h3", "b2", "CONFLICTING", &[])), Plan::Nothing);
        // It merges cleanly again: the count starts over, and a later conflict is asked about,
        // even on the same head.
        assert_eq!(tick(&mut r, &status("h4", "b2", "MERGEABLE", &[])), Plan::Nothing);
        assert_eq!((r.conflict_tries, r.conflict_gave_up, r.conflict.as_str()), (0, false, ""));
        assert_eq!(tick(&mut r, &status("h4", "b3", "CONFLICTING", &[])), Plan::Ask(Ask { conflicts: true, ci: false, try_no: 1, of: MAX_TRIES }));
    }

    #[test]
    fn conflicts_come_first_and_carry_failed_ci() {
        let mut r = Record::default();
        let ci = status("h1", "b1", "CONFLICTING", &[("Lint", "FAILURE"), ("Test", "SUCCESS")]);
        assert_eq!(tick(&mut r, &ci), Plan::Ask(Ask { conflicts: true, ci: true, try_no: 1, of: MAX_TRIES }));
        // Both are noted: the commit is not asked about again for CI.
        assert_eq!((r.head.as_str(), r.tries, r.conflict_tries), ("h1", 1, 1));
        assert_eq!(tick(&mut r, &ci), Plan::Nothing);
        let text = message("o/r", 9, "https://github.com/o/r/pull/9", &ci, Ask { conflicts: true, ci: true, try_no: 1, of: MAX_TRIES });
        assert!(text.starts_with("PR #9 (https://github.com/o/r/pull/9) has merge conflicts with main (head h1)."), "{text}");
        assert!(text.contains("rebase this branch on origin/main") && text.contains("CI also failed on this commit: CI / Lint"), "{text}");
        assert!(text.ends_with("(Sent by Glasscode because GitHub reports conflicts; ask 1 of 3.)"), "{text}");
        // CI failing on a conflicting PR is not asked about on its own.
        let mut r = Record { conflict: "h2:b1".into(), conflict_tries: 1, ..Default::default() };
        assert_eq!(tick(&mut r, &status("h2", "b1", "CONFLICTING", &[("Lint", "FAILURE")])), Plan::Nothing);
    }

    #[test]
    fn ci_alone_works_as_before() {
        let mut r = Record::default();
        // Still running: wait for the rest (unless the wait is over).
        let running = status("h1", "b1", "MERGEABLE", &[("Lint", "FAILURE"), ("Test", "")]);
        assert_eq!(plan(&r, &running, BOTH, false), Plan::Later);
        assert_eq!(plan(&r, &running, BOTH, true), Plan::Ask(Ask { conflicts: false, ci: true, try_no: 1, of: MAX_TRIES }));
        let failed = status("h1", "b1", "MERGEABLE", &[("Lint", "FAILURE")]);
        assert_eq!(tick(&mut r, &failed), Plan::Ask(Ask { conflicts: false, ci: true, try_no: 1, of: MAX_TRIES }));
        assert_eq!(tick(&mut r, &failed), Plan::Nothing);
        assert_eq!(tick(&mut r, &status("h2", "b1", "UNKNOWN", &[("Lint", "FAILURE")])), Plan::Ask(Ask { conflicts: false, ci: true, try_no: 2, of: MAX_TRIES }));
        // Green: the count starts over.
        tick(&mut r, &status("h3", "b1", "MERGEABLE", &[("Lint", "SUCCESS")]));
        assert_eq!(r.tries, 0);
    }

    #[test]
    fn each_switch_turns_off_its_own() {
        let ci = status("h1", "b1", "CONFLICTING", &[("Lint", "FAILURE")]);
        let r = Record::default();
        // Conflicts off: CI is asked about as before.
        assert_eq!(plan(&r, &ci, Enabled { ci: true, conflicts: false, tries: MAX_TRIES }, true), Plan::Ask(Ask { conflicts: false, ci: true, try_no: 1, of: MAX_TRIES }));
        // CI off: only the conflicts.
        assert_eq!(plan(&r, &ci, Enabled { ci: false, conflicts: true, tries: MAX_TRIES }, true), Plan::Ask(Ask { conflicts: true, ci: false, try_no: 1, of: MAX_TRIES }));
        assert_eq!(plan(&r, &ci, Enabled { ci: false, conflicts: false, tries: MAX_TRIES }, true), Plan::Nothing);
        // A merged or closed PR: nothing.
        assert_eq!(plan(&r, &CiStatus { state: "MERGED".into(), ..ci.clone() }, BOTH, true), Plan::Nothing);
    }

    #[test]
    fn old_records_still_load() {
        let old = r#"{"head":"abc","tries":2,"asked_at":5,"session":"s","gave_up":false}"#;
        let r: Record = serde_json::from_str(old).unwrap();
        assert_eq!((r.tries, r.conflict_tries, r.conflict.as_str()), (2, 0, ""));
        // And a record with no conflicts writes the same fields as before.
        assert_eq!(serde_json::to_string(&r).unwrap(), old);
    }
}
