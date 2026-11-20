//! Loop protection (M40 step 5; review events-13), and each playbook's own limit.
//!
//! - **Depth.** An event caused by the manager or a playbook is one deeper than its cause; past
//!   [`MAX_DEPTH`] nothing fires. Events the manager caused fire only the playbooks that opt in.
//! - **Cycles.** An effect whose cause chain already holds its own key does not happen again.
//! - **A bucket per session**: at most [`SENDS_PER_HOUR`] messages typed into one session by
//!   anyone but the person in an hour, and [`SENDS_PER_DAY`] in a day.
//! - **One run at a time** per playbook and subject.
//! - **The breaker** pauses a playbook after [`SAME_SUBJECT_TRIPS`] firings on one subject in a day
//!   (whatever its own limit says), or
//!   [`FAILURES_TRIP`] failures in a row, and tells the person once. A global breaker pauses every
//!   playbook after [`GLOBAL_PER_HOUR`] firings in an hour.
//!
//! The counts are read from the effect ledger ([`LedgerCounts`]): a firing is a claim whose origin
//! is `playbook:<id>`, so they last through a restart with no counter of their own.

use std::collections::HashMap;

use crate::effects::{EffectRecord, EffectState, Ledger};

use super::model::{Limit, Per};

/// The deepest an event may be and still fire a playbook (0: the outside world or the person).
pub const MAX_DEPTH: u8 = 3;
/// Messages typed into one session by callers other than the person, per hour and per day.
pub const SENDS_PER_HOUR: u32 = 4;
pub const SENDS_PER_DAY: u32 = 12;
/// Firings of one playbook on one subject that trip its breaker.
pub const SAME_SUBJECT_TRIPS: u32 = 5;
/// Failures in a row that trip a playbook's breaker.
pub const FAILURES_TRIP: u32 = 3;
/// Firings of all playbooks in an hour that trip the global breaker.
pub const GLOBAL_PER_HOUR: u32 = 60;

pub const HOUR_MS: u64 = 3_600_000;
pub const DAY_MS: u64 = 24 * HOUR_MS;

/// The origin of a playbook's claims in the effect ledger.
pub fn origin_of(playbook: &str) -> String {
    format!("playbook:{playbook}")
}

/// What the limits count. Every count is of what happened at or after `since_ms`.
pub trait Counts {
    /// Firings of a playbook on a subject.
    fn fired(&self, playbook: &str, subject: &str, since_ms: u64) -> u32;
    /// Firings of a playbook on a subject not settled yet.
    fn running(&self, playbook: &str, subject: &str) -> u32;
    /// The last firings of a playbook that failed, in a row (the newest first, up to one that did not).
    fn failures_in_a_row(&self, playbook: &str) -> u32;
    /// Messages typed into a session by callers other than the person.
    fn sends(&self, session: &str, since_ms: u64) -> u32;
    /// Firings of every playbook.
    fn all_fired(&self, since_ms: u64) -> u32;
}

/// The counts of an effect ledger's records.
#[derive(Clone, Debug, Default)]
pub struct LedgerCounts {
    records: Vec<EffectRecord>,
}

impl LedgerCounts {
    pub fn of(ledger: &Ledger) -> LedgerCounts {
        let mut records: Vec<EffectRecord> = ledger.records.values().cloned().collect();
        records.sort_by_key(|record| record.claimed_ms);
        LedgerCounts { records }
    }

    /// A firing not written yet (a dry run counts what it would have done).
    pub fn add(&mut self, record: EffectRecord) {
        self.records.push(record);
    }

    /// Count a playbook only from when the person released its pause: what tripped its breaker
    /// before does not trip it again at once. `None` for the playbook: every playbook (the global
    /// release).
    pub fn after_release(&mut self, playbook: Option<&str>, released_ms: u64) {
        let origin = playbook.map(origin_of);
        self.records.retain(|r| r.claimed_ms >= released_ms || !r.origin.starts_with("playbook:") || origin.as_ref().is_some_and(|o| &r.origin != o));
    }

    fn of_playbook<'a>(&'a self, playbook: &str) -> impl Iterator<Item = &'a EffectRecord> + 'a {
        let origin = origin_of(playbook);
        self.records.iter().filter(move |record| record.origin == origin)
    }
}

impl Counts for LedgerCounts {
    fn fired(&self, playbook: &str, subject: &str, since_ms: u64) -> u32 {
        self.of_playbook(playbook).filter(|r| r.subject == subject && r.claimed_ms >= since_ms).count() as u32
    }

    fn running(&self, playbook: &str, subject: &str) -> u32 {
        self.of_playbook(playbook).filter(|r| r.subject == subject && r.state == EffectState::Claimed).count() as u32
    }

    fn failures_in_a_row(&self, playbook: &str) -> u32 {
        let mut mine: Vec<&EffectRecord> = self.of_playbook(playbook).collect();
        mine.sort_by(|a, b| b.claimed_ms.cmp(&a.claimed_ms));
        mine.iter().take_while(|r| matches!(r.state, EffectState::Failed(_))).count() as u32
    }

    /// Only the delivery queue's claims count (they carry the typed marker): a playbook's send
    /// leaves its own claim and the queue's, and is one message.
    fn sends(&self, session: &str, since_ms: u64) -> u32 {
        self.records.iter().filter(|r| r.op == "session.send" && r.target == session && r.claimed_ms >= since_ms && r.evidence.get("marker").is_some()).count() as u32
    }

    fn all_fired(&self, since_ms: u64) -> u32 {
        self.records.iter().filter(|r| r.origin.starts_with("playbook:") && r.claimed_ms >= since_ms).count() as u32
    }
}

/// The subject a limit counts by, from the event's fields: the commit, the PR, the session, or
/// the hour or day of `now_ms`. None: the event has no such field.
pub fn subject(per: Per, field: &dyn Fn(&str) -> Option<serde_json::Value>, now_ms: u64) -> Option<String> {
    let text = |name: &str| field(name).map(|value| value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string()));
    match per {
        Per::Commit => text("head").map(|head| format!("commit:{head}")),
        Per::Pr => Some(format!("pr:{}#{}", text("repo")?, text("number")?)),
        Per::Session => text("session").map(|session| format!("session:{session}")),
        Per::Hour => Some(format!("hour:{}", now_ms / HOUR_MS)),
        Per::Day => Some(format!("day:{}", now_ms / DAY_MS)),
    }
}

/// Why something did not fire, or what tripped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Held {
    TooDeep(u8),
    ByManager,
    Cycle,
    Limit { count: u32 },
    Running,
    SessionBucket { session: String },
    /// The playbook's breaker tripped: it is to be paused, and the person told.
    Breaker(String),
    /// The global breaker tripped: every playbook is to be paused.
    Global,
}

impl Held {
    pub fn words(&self) -> String {
        match self {
            Held::TooDeep(depth) => format!("the event is {depth} steps from anything the person or the world did (at most {MAX_DEPTH})"),
            Held::ByManager => "the manager caused the event, and the playbook does not react to its own kind".to_string(),
            Held::Cycle => "it would answer an effect of its own".to_string(),
            Held::Limit { count } => format!("its limit of {count} is reached"),
            Held::Running => "it is already running for this".to_string(),
            Held::SessionBucket { session } => format!("{session} had {SENDS_PER_HOUR} messages this hour, or {SENDS_PER_DAY} today, from others than you"),
            Held::Breaker(why) => format!("paused by the breaker: {why}"),
            Held::Global => format!("every playbook is paused: {GLOBAL_PER_HOUR} firings in an hour"),
        }
    }
}

/// Whether one firing may go: the limits and breakers, in order. `send_to` is the session a
/// message would be typed into.
#[allow(clippy::too_many_arguments)]
pub fn check(counts: &dyn Counts, playbook: &str, limit: Limit, subject: &str, depth: u8, by_manager: bool, manager_events: bool, send_to: Option<&str>, now_ms: u64) -> Result<(), Held> {
    if depth > MAX_DEPTH {
        return Err(Held::TooDeep(depth));
    }
    if by_manager && !manager_events {
        return Err(Held::ByManager);
    }
    if counts.all_fired(now_ms.saturating_sub(HOUR_MS)) >= GLOBAL_PER_HOUR {
        return Err(Held::Global);
    }
    if counts.failures_in_a_row(playbook) >= FAILURES_TRIP {
        return Err(Held::Breaker(format!("{FAILURES_TRIP} failures in a row")));
    }
    // The same subject again and again in a day, whatever the playbook's own limit: nothing it did
    // changed it.
    if counts.fired(playbook, subject, now_ms.saturating_sub(DAY_MS)) >= SAME_SUBJECT_TRIPS {
        return Err(Held::Breaker(format!("{SAME_SUBJECT_TRIPS} firings on {subject} in a day")));
    }
    if counts.running(playbook, subject) > 0 {
        return Err(Held::Running);
    }
    let since = match limit.per {
        Per::Hour => now_ms.saturating_sub(HOUR_MS),
        Per::Day => now_ms.saturating_sub(DAY_MS),
        Per::Commit | Per::Pr | Per::Session => 0,
    };
    if counts.fired(playbook, subject, since) >= limit.count {
        return Err(Held::Limit { count: limit.count });
    }
    if let Some(session) = send_to
        && (counts.sends(session, now_ms.saturating_sub(HOUR_MS)) >= SENDS_PER_HOUR || counts.sends(session, now_ms.saturating_sub(DAY_MS)) >= SENDS_PER_DAY)
    {
        return Err(Held::SessionBucket { session: session.to_string() });
    }
    Ok(())
}

/// Counts kept in memory: for a dry run and the tests.
#[derive(Clone, Debug, Default)]
pub struct MemoryCounts {
    /// (playbook, subject, when, settled, failed)
    pub firings: Vec<(String, String, u64, bool, bool)>,
    /// (session, when)
    pub sends: Vec<(String, u64)>,
}

impl MemoryCounts {
    pub fn fire(&mut self, playbook: &str, subject: &str, at_ms: u64, send_to: Option<&str>) {
        self.firings.push((playbook.to_string(), subject.to_string(), at_ms, true, false));
        if let Some(session) = send_to {
            self.sends.push((session.to_string(), at_ms));
        }
    }
}

impl Counts for MemoryCounts {
    fn fired(&self, playbook: &str, subject: &str, since_ms: u64) -> u32 {
        self.firings.iter().filter(|(p, s, at, ..)| p == playbook && s == subject && *at >= since_ms).count() as u32
    }

    fn running(&self, playbook: &str, subject: &str) -> u32 {
        self.firings.iter().filter(|(p, s, _, settled, _)| p == playbook && s == subject && !settled).count() as u32
    }

    fn failures_in_a_row(&self, playbook: &str) -> u32 {
        let mut mine: Vec<_> = self.firings.iter().filter(|(p, ..)| p == playbook).collect();
        mine.sort_by(|a, b| b.2.cmp(&a.2));
        mine.iter().take_while(|(.., failed)| *failed).count() as u32
    }

    fn sends(&self, session: &str, since_ms: u64) -> u32 {
        self.sends.iter().filter(|(s, at)| s == session && *at >= since_ms).count() as u32
    }

    fn all_fired(&self, since_ms: u64) -> u32 {
        self.firings.iter().filter(|(_, _, at, ..)| *at >= since_ms).count() as u32
    }
}

/// How many firings each playbook would have had, from a list of (playbook, subject) firings:
/// for the simulation's summary.
pub fn tally(firings: &[(String, String)]) -> HashMap<String, u32> {
    let mut out = HashMap::new();
    for (playbook, _) in firings {
        *out.entry(playbook.clone()).or_insert(0) += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{EffectRecord, EffectState};

    const NOW: u64 = 1_800_000_000_000;

    #[test]
    fn loop_breaker_trips() {
        let mut counts = MemoryCounts::default();
        let limit = Limit { count: 10, per: Per::Session };
        // A session ends on a question, the manager answers, it ends on a question again: each
        // round is one deeper, and the fourth does not fire.
        for depth in 0..=MAX_DEPTH {
            assert_eq!(check(&counts, "answer", limit, "session:s1", depth, false, true, None, NOW), Ok(()), "depth {depth}");
        }
        assert_eq!(check(&counts, "answer", limit, "session:s1", MAX_DEPTH + 1, false, true, None, NOW), Err(Held::TooDeep(MAX_DEPTH + 1)));
        // Events the manager caused fire only a playbook that opts in.
        assert_eq!(check(&counts, "answer", limit, "session:s1", 1, true, false, None, NOW), Err(Held::ByManager));
        // Two playbooks feeding each other on one subject: the breaker trips at five.
        for n in 0..SAME_SUBJECT_TRIPS {
            counts.fire("answer", "session:s1", NOW - 1000 + u64::from(n), None);
        }
        assert!(matches!(check(&counts, "answer", limit, "session:s1", 0, false, true, None, NOW), Err(Held::Breaker(_))));
        // Three failures in a row trip it too.
        let mut failing = MemoryCounts::default();
        for n in 0..FAILURES_TRIP {
            failing.firings.push(("fix".into(), format!("commit:{n}"), NOW - 10 + u64::from(n), true, true));
        }
        assert!(matches!(check(&failing, "fix", Limit::default(), "commit:x", 0, false, false, None, NOW), Err(Held::Breaker(_))));
        // And everything stops past the global count.
        let mut busy = MemoryCounts::default();
        for n in 0..GLOBAL_PER_HOUR {
            busy.fire(&format!("p{n}"), "x", NOW - 1000, None);
        }
        assert_eq!(check(&busy, "other", Limit::default(), "y", 0, false, false, None, NOW), Err(Held::Global));
    }

    #[test]
    fn limits_and_the_session_bucket() {
        let mut counts = MemoryCounts::default();
        let once = Limit { count: 1, per: Per::Commit };
        assert_eq!(check(&counts, "fix", once, "commit:a", 0, false, false, Some("s1"), NOW), Ok(()));
        counts.fire("fix", "commit:a", NOW, Some("s1"));
        assert_eq!(check(&counts, "fix", once, "commit:a", 0, false, false, Some("s1"), NOW), Err(Held::Limit { count: 1 }));
        assert_eq!(check(&counts, "fix", once, "commit:b", 0, false, false, Some("s1"), NOW), Ok(()));
        // Four messages into one session in an hour from others than the person: the fifth waits.
        for n in 0..3 {
            counts.sends.push(("s1".into(), NOW - 60_000 * n));
        }
        assert!(matches!(check(&counts, "fix", once, "commit:c", 0, false, false, Some("s1"), NOW), Err(Held::SessionBucket { .. })));
        assert_eq!(check(&counts, "fix", once, "commit:c", 0, false, false, Some("s2"), NOW), Ok(()));
        // An hourly limit counts the last hour only.
        let hourly = Limit { count: 1, per: Per::Hour };
        counts.fire("digest", "hour:1", NOW - 2 * HOUR_MS, None);
        assert_eq!(check(&counts, "digest", hourly, "hour:1", 0, false, false, None, NOW), Ok(()));
        // One running is one too many.
        counts.firings.push(("slow".into(), "pr:a#1".into(), NOW, false, false));
        assert_eq!(check(&counts, "slow", Limit { count: 5, per: Per::Pr }, "pr:a#1", 0, false, false, None, NOW), Err(Held::Running));
    }

    #[test]
    fn ledger_counts_from_a_release_and_count_one_send_once() {
        let home = crate::paths::MachineId::parse("00000000000000aa").unwrap();
        let record = |origin: &str, op: &str, target: &str, at: u64, marker: bool, state: EffectState| EffectRecord {
            v: crate::effects::SCHEMA,
            key: crate::effects::effect_key(origin, &at.to_string(), op, target),
            op: op.into(),
            retry: crate::ops::Retry::Verifiable,
            origin: origin.into(),
            subject: "session:s1".into(),
            target: target.into(),
            caller: String::new(),
            reason: String::new(),
            project: None,
            session: None,
            home: home.clone(),
            generation: 0,
            launch: String::new(),
            claimed_ms: at,
            attempt: 1,
            evidence: if marker { serde_json::json!({ "marker": "[gc:1]" }) } else { serde_json::Value::Null },
            state,
            finished_ms: None,
        };
        let mut counts = LedgerCounts::default();
        // A playbook's send: its own claim, and the delivery queue's (with the marker): one message.
        counts.add(record("playbook:answer", "session.send", "s1", NOW - 10, false, EffectState::Done(serde_json::Value::Null)));
        counts.add(record("session:s9", "session.send", "s1", NOW - 9, true, EffectState::Done(serde_json::Value::Null)));
        assert_eq!(counts.sends("s1", 0), 1);
        // Three failures in a row trip it; released afterwards, it counts from the release.
        for n in 0..3 {
            counts.add(record("playbook:fix", "pr.comment", "acme/app#1", NOW - 5 + n, false, EffectState::Failed("no".into())));
        }
        assert_eq!(counts.failures_in_a_row("fix"), 3);
        counts.after_release(Some("fix"), NOW);
        assert_eq!(counts.failures_in_a_row("fix"), 0);
        assert_eq!(counts.fired("answer", "session:s1", 0), 1, "another playbook's records stay");
        counts.after_release(None, NOW);
        assert_eq!(counts.all_fired(0), 0);
        assert_eq!(counts.sends("s1", 0), 1, "what is not a playbook's stays");
    }

    #[test]
    fn subjects_come_from_typed_fields() {
        let fields = |name: &str| match name {
            "head" => Some(serde_json::json!("abc123")),
            "repo" => Some(serde_json::json!("acme/app")),
            "number" => Some(serde_json::json!(42)),
            "session" => Some(serde_json::json!("s1")),
            _ => None,
        };
        assert_eq!(subject(Per::Commit, &fields, NOW).as_deref(), Some("commit:abc123"));
        assert_eq!(subject(Per::Pr, &fields, NOW).as_deref(), Some("pr:acme/app#42"));
        assert_eq!(subject(Per::Session, &fields, NOW).as_deref(), Some("session:s1"));
        assert_eq!(subject(Per::Day, &fields, NOW), Some(format!("day:{}", NOW / DAY_MS)));
        assert_eq!(subject(Per::Commit, &|_| None, NOW), None);
    }
}
