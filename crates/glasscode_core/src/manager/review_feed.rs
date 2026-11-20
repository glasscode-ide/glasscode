//! The review feed (M56.2): who reviewed the pull requests of the person's sessions, as the
//! journal's `review.received` events for the native review-tell (M40), from v2's own store and
//! not v1's harness. It keeps v1's rules to the letter (v1 read them in `followups`): only the person's own pull requests (a teammate's is not news for the session), only once
//! the person's login is known, no bots, only reviews someone left (approved, changes asked,
//! comments), a pull request that is merged or closed tells nothing, the first reviewers seen on a
//! pull request are news only once it has been known for a while (GitHub's reviewers are read a few
//! minutes after the pull request), and a start's first round only records what is there.
//!
//! What it saw is kept in `manager/reviews.json` ([`ReviewSeen`]), so a restart says nothing twice.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{PrInfo, Snapshot};

/// The first reviewers seen on a pull request are news only when it has been known this long.
pub const SETTLE_SECS: u64 = 600;

/// What the feed saw (`manager/reviews.json`). Damaged, it starts empty: the next round is a
/// baseline, so nothing old is said.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewSeen {
    /// When each pull request (`repo#n`) was first seen (Unix seconds).
    pub first_seen: BTreeMap<String, u64>,
    /// Pull requests whose reviewers were seen once: from then on every change is news.
    pub reviewers_seen: BTreeSet<String>,
    /// Each reviewer's last state (`repo#n|login` → `APPROVED`, …).
    pub states: BTreeMap<String, String>,
}

impl crate::store::Store for ReviewSeen {
    const NAME: &'static str = "manager/reviews";
    const VERSION: u32 = 1;
    const ON_DAMAGE: crate::store::OnDamage = crate::store::OnDamage::Empty;

    fn path() -> std::path::PathBuf {
        super::store_dir().join("reviews.json")
    }
}

/// A pull request the person opened (or one whose author is not known yet).
fn is_own(pr: &PrInfo, me: &str) -> bool {
    let author = pr.author.trim();
    author.is_empty() || author.eq_ignore_ascii_case(me.trim())
}

/// Accounts that are not people: their reviews are not news for a session.
fn is_bot(login: &str) -> bool {
    let l = login.to_lowercase();
    l.ends_with("[bot]") || l.ends_with("-bot") || l.starts_with("app/") || l.contains("copilot") || l == "github-actions" || l == "dependabot"
}

/// A review someone actually left.
fn is_real_review(state: &str) -> bool {
    matches!(state, "APPROVED" | "CHANGES_REQUESTED" | "COMMENTED")
}

/// One round: what changed since the last, as `review.received` events (their project is the
/// app's to add). `first_round`: the first after a start, which only records. `now` is in seconds.
pub fn changes(seen: &mut ReviewSeen, first_round: bool, snap: &Snapshot, now: u64) -> Vec<crate::journal::Event> {
    let mut events = Vec::new();
    let me = snap.me.as_deref().unwrap_or_default().trim().to_lowercase();
    // Until the person's own login is known, their own reviews cannot be told from a teammate's.
    let known = !me.is_empty();
    for session in snap.sessions.iter().filter(|s| s.review_of.is_none()) {
        let Some(pr) = &session.pr else { continue };
        if pr.repo.is_empty() || pr.number == 0 || !is_own(pr, &me) {
            continue;
        }
        let key = format!("{}#{}", pr.repo, pr.number);
        let first_seen = *seen.first_seen.entry(key.clone()).or_insert(now);
        let over = matches!(pr.state.as_str(), "merged" | "closed");
        let age = now.saturating_sub(first_seen);
        let tell = !first_round && !over && known && (age >= SETTLE_SECS || seen.reviewers_seen.contains(&key));
        if !pr.reviewers.is_empty() {
            seen.reviewers_seen.insert(key.clone());
        }
        for reviewer in &pr.reviewers {
            let login = reviewer.login.trim();
            let lower = login.to_lowercase();
            if login.is_empty() || lower == me || is_bot(login) {
                continue;
            }
            let state = reviewer.state.trim().to_uppercase();
            let state_key = format!("{key}|{lower}");
            if seen.states.get(&state_key) == Some(&state) {
                continue;
            }
            seen.states.insert(state_key, state.clone());
            if tell && is_real_review(&state) {
                events.push(crate::journal::review_received(&session.id, None, &pr.repo, pr.number, login, reviewer.association.as_deref(), &state));
            }
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::{ReviewerInfo, SessionInfo, SessionState};

    fn snap(at: u64, reviewers: &[(&str, &str)], author: &str) -> Snapshot {
        let pr = PrInfo { repo: "acme/app".into(), number: 12, state: "open".into(), author: author.into(), reviewers: reviewers.iter().map(|(l, s)| ReviewerInfo { login: (*l).into(), state: (*s).into(), ..ReviewerInfo::default() }).collect(), ..PrInfo::default() };
        Snapshot { at, me: Some("robin".into()), sessions: vec![SessionInfo { id: "s1".into(), state: SessionState::Idle, pr: Some(pr), ..SessionInfo::default() }], ..Snapshot::default() }
    }

    #[test]
    fn a_review_is_news_once_and_only_for_the_persons_own_pull_request() {
        let mut seen = ReviewSeen::default();
        // The first round after a start only records.
        assert!(changes(&mut seen, true, &snap(1_000, &[("alex", "COMMENTED")], ""), 1_000).is_empty());
        // The same state again: nothing; a new one: news.
        assert!(changes(&mut seen, false, &snap(1_060, &[("alex", "COMMENTED")], ""), 1_060).is_empty());
        let news = changes(&mut seen, false, &snap(1_120, &[("alex", "CHANGES_REQUESTED"), ("ci-bot", "COMMENTED"), ("robin", "COMMENTED")], ""), 1_120);
        assert_eq!(news.len(), 1, "the bot and the person's own review are not news");
        assert_eq!((news[0].kind.as_str(), news[0].data["reviewer"].clone(), news[0].data["state"].clone()), ("review.received", serde_json::json!("alex"), serde_json::json!("CHANGES_REQUESTED")));
        // A pending reviewer is no review; a teammate's pull request is not the session's news.
        assert!(changes(&mut seen, false, &snap(1_180, &[("alex", "CHANGES_REQUESTED"), ("sam", "PENDING")], ""), 1_180).is_empty());
        let mut theirs = ReviewSeen::default();
        changes(&mut theirs, true, &snap(1_000, &[], "sam"), 1_000);
        assert!(changes(&mut theirs, false, &snap(2_000, &[("alex", "APPROVED")], "sam"), 2_000).is_empty());
        // The first reviewers of a pull request just found wait for it to settle.
        let mut fresh = ReviewSeen::default();
        changes(&mut fresh, true, &snap(1_000, &[], ""), 1_000);
        assert!(changes(&mut fresh, false, &snap(1_100, &[("alex", "APPROVED")], ""), 1_100).is_empty(), "known for less than the settle time");
    }
}
