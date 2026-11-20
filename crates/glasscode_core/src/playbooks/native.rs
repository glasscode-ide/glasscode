//! The manager's own built-ins as state machines over the journal's events (M40 part 2; review
//! events-12, D27). They say what v1's harness says, from events instead of snapshot rounds, and
//! the parity tests hold them to it at the harness's `react` seam: the same recorded rounds go
//! through v1's harness and, as the events the journal writes of them, through these, and the
//! actions are compared.
//!
//! - **needs-you**: a session stopped to ask the person something (`session.needs_input`): one
//!   notification per stop; the stop ends when the session works again (`session.activity`).
//! - **review-tell**: someone reviewed a session's PR (`review.received`): the person hears it at
//!   once; the session is told once it is free (idle), each review once, and only reviews whose
//!   author passes `trusted_author` (D27) are told to the session at all. An approval is news for
//!   the person only.
//! - **stuck-session**: a turn ended failed or cut off (`turn.ended`) twice in a row on the same
//!   cause: the person is told, once until a turn ends well.
//! - **usage-handoff**: an account crossed its usage threshold (`usage.threshold`): the person is
//!   told once a day per account, with the suggestion to hand its sessions to another.
//!
//! They run only while the manager runs (`builtins::runs`); v1's harness does the same while the
//! engine is v1, and until the v2 manager takes over (M51), so the app does not run both.

use std::collections::{BTreeMap, BTreeSet};

use crate::manager::{Action, SessionInfo, SessionState, Settings};

use super::engine::EventView;

/// What the person and the app know besides the event.
pub struct View<'a> {
    pub settings: &'a Settings,
    pub sessions: &'a [SessionInfo],
    /// The person's own logins.
    pub me: &'a [String],
    pub now_ms: u64,
}

impl View<'_> {
    fn session(&self, id: &str) -> Option<&SessionInfo> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// The teammates the person confirmed in the app (their GitHub logins).
    fn confirmed(&self) -> Vec<String> {
        self.settings.people.iter().map(|p| p.github.clone()).filter(|login| !login.is_empty()).collect()
    }
}

/// A review waiting to be told to its session.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Review {
    session: String,
    repo: String,
    number: u64,
    reviewer: String,
}

/// The built-ins' state: kept by the runner, not on disk (the effect ledger keeps what was done).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Native {
    /// Sessions the person was told about in this stop.
    asked: BTreeSet<String>,
    /// Reviews to tell, with their state; and the state each was last told with.
    pending: BTreeMap<Review, String>,
    told: BTreeMap<Review, String>,
    /// Per session: the cause of the last bad turn end, how many in a row, and whether told.
    turns: BTreeMap<String, (String, u32, bool)>,
    /// Per account: the day it was last told about.
    usage: BTreeMap<String, u64>,
}

fn text(event: &dyn EventView, name: &str) -> Option<String> {
    event.field(name).and_then(|value| value.as_str().map(str::to_string))
}

impl Native {
    /// What one event leads to, at once.
    pub fn on_event(&mut self, event: &dyn EventView, view: &View) -> Vec<Action> {
        match event.kind() {
            "session.needs_input" => self.needs_you(event, view),
            "session.activity" => {
                if let Some(session) = event.session()
                    && text(event, "activity").as_deref() == Some("working")
                {
                    self.asked.remove(session);
                }
                Vec::new()
            }
            "review.received" => self.review(event, view),
            "turn.ended" => self.turn(event, view),
            "usage.threshold" => self.usage(event, view),
            _ => Vec::new(),
        }
    }

    fn needs_you(&mut self, event: &dyn EventView, view: &View) -> Vec<Action> {
        let Some(id) = event.session() else { return Vec::new() };
        let Some(session) = view.session(id) else { return Vec::new() };
        if !self.asked.insert(id.to_string()) {
            return Vec::new();
        }
        vec![Action::Notify { title: format!("{} needs you", crate::manager::brief::called(session)), body: crate::manager::brief::reply_line(session, 160), session: Some(id.to_string()) }]
    }

    fn review(&mut self, event: &dyn EventView, view: &View) -> Vec<Action> {
        let (Some(session), Some(repo), Some(number), Some(reviewer), Some(state)) = (event.session(), text(event, "repo"), event.field("number").and_then(|n| n.as_u64()), text(event, "reviewer"), text(event, "state")) else {
            return Vec::new();
        };
        let who = crate::manager::brief::name_of(view.settings, &reviewer);
        let verdict = match state.as_str() {
            "APPROVED" => "approved",
            "CHANGES_REQUESTED" => "asked for changes on",
            _ => "left comments on",
        };
        let s = view.session(session);
        let title = s.and_then(|s| s.pr.as_ref()).map(|pr| pr.title.trim().to_string()).filter(|t| !t.is_empty()).or_else(|| s.map(crate::manager::brief::called)).unwrap_or_default();
        let review = Review { session: session.to_string(), repo, number, reviewer: reviewer.clone() };
        // An approval asks nothing of the session; outside text from an untrusted author never
        // reaches it (D27).
        let association = text(event, "association");
        let trusted = crate::gate::trusted_author(&reviewer, association.as_deref(), view.me, &view.confirmed());
        if state == "APPROVED" || !trusted {
            self.pending.remove(&review);
        } else if self.told.get(&review) != Some(&state) {
            self.pending.insert(review, state.clone());
        }
        vec![Action::Notify { title: format!("{who} {verdict} #{number}"), body: title, session: Some(session.to_string()) }]
    }

    fn turn(&mut self, event: &dyn EventView, view: &View) -> Vec<Action> {
        let Some(session) = event.session() else { return Vec::new() };
        let end = text(event, "end").unwrap_or_default();
        let bad = !matches!(end.as_str(), "completed" | "paused" | "");
        if !bad {
            self.turns.remove(session);
            return Vec::new();
        }
        let entry = self.turns.entry(session.to_string()).or_insert_with(|| (end.clone(), 0, false));
        if entry.0 != end {
            *entry = (end.clone(), 0, false);
        }
        entry.1 += 1;
        if entry.1 < 2 || entry.2 {
            return Vec::new();
        }
        entry.2 = true;
        let name = view.session(session).map(crate::manager::brief::called).unwrap_or_else(|| session.to_string());
        vec![Action::Notify { title: format!("{name} seems stuck"), body: format!("Its last turns ended {} the same way ({end}).", entry.1), session: Some(session.to_string()) }]
    }

    fn usage(&mut self, event: &dyn EventView, view: &View) -> Vec<Action> {
        let Some(account) = text(event, "account") else { return Vec::new() };
        let day = view.now_ms / 86_400_000;
        if self.usage.get(&account) == Some(&day) {
            return Vec::new();
        }
        self.usage.insert(account.clone(), day);
        let percent = event.field("percent").and_then(|p| p.as_u64()).unwrap_or(0);
        vec![Action::Notify { title: format!("{account} is at {percent}% of its usage"), body: "Its sessions can be handed to another account before it runs out.".to_string(), session: None }]
    }

    /// Each round: tell the sessions that are free about the reviews waiting for them, one message
    /// per session, each review once. Never typed into a session that works, waits for the
    /// person, or is not running.
    pub fn tell_ready(&mut self, view: &View) -> Vec<Action> {
        let mut sessions: Vec<String> = Vec::new();
        for review in self.pending.keys() {
            if !sessions.contains(&review.session) {
                sessions.push(review.session.clone());
            }
        }
        let mut actions = Vec::new();
        for id in sessions {
            if !view.session(&id).is_some_and(|s| s.state == SessionState::Idle) {
                continue;
            }
            let reviews: Vec<(Review, String)> = self.pending.iter().filter(|(r, _)| r.session == id).map(|(r, s)| (r.clone(), s.clone())).collect();
            let tells: Vec<crate::manager::brief::PendingTell> = reviews
                .iter()
                .map(|(r, state)| crate::manager::brief::PendingTell { key: String::new(), session: r.session.clone(), repo: r.repo.clone(), number: r.number, reviewer: r.reviewer.clone(), state: state.clone() })
                .collect();
            for (review, state) in reviews {
                self.pending.remove(&review);
                self.told.insert(review, state);
            }
            actions.push(Action::TellSession { session: id.clone(), text: crate::manager::harness::tell_text(view.settings, &tells) });
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Event, EventKind, Source};
    use crate::manager::harness::Harness;
    use crate::manager::{Person, PrInfo, ReviewerInfo, Snapshot};

    const T0: u64 = 1_800_000_000;

    fn settings() -> Settings {
        Settings {
            enabled: true,
            name: "AI Robin".into(),
            owner: "Robin".into(),
            brief_at: None,
            people: vec![Person { name: "Sam".into(), github: "sam-dev".into(), ..Person::default() }, Person { name: "Alex".into(), github: "alex".into(), ..Person::default() }],
            ..Settings::default()
        }
    }

    fn pr(number: u64, reviewers: &[(&str, &str)]) -> PrInfo {
        PrInfo {
            repo: "acme/app".into(),
            number,
            title: format!("Change {number}"),
            url: format!("https://github.com/acme/app/pull/{number}"),
            state: "open".into(),
            ci: "passing".into(),
            reviewers: reviewers.iter().map(|(l, s)| ReviewerInfo { login: (*l).into(), state: (*s).into() }).collect(),
            ..PrInfo::default()
        }
    }

    fn session(id: &str, state: SessionState, pr: Option<PrInfo>) -> SessionInfo {
        SessionInfo { id: id.into(), title: format!("Work {id}"), project_id: "p1".into(), project: "app".into(), state, pr, last_reply: Some("Two ways to do this.\nWhich one do you want?".into()), ..SessionInfo::default() }
    }

    fn snap(at: u64, sessions: Vec<SessionInfo>) -> Snapshot {
        Snapshot { at, me: Some("robin".into()), sessions, mac_online: true, ..Snapshot::default() }
    }

    fn event(kind: &str, session: &str, data: serde_json::Value) -> Event {
        let mut event = Event::new(EventKind::named(kind), Source::App, kind).session(Some(session)).data(data);
        event.id = format!("{kind}-{session}");
        event
    }

    /// The events the journal writes of a round: a session's activity when it changes (its hook
    /// events), a stop to ask (`session.needs_input`), and each review v1's own reader finds in
    /// the round (the status rounds' `review.received`). The first round of a run is a baseline.
    fn events_of(prev: Option<&Snapshot>, now: &Snapshot, reader: &mut crate::manager::Data) -> Vec<Event> {
        let mut out = Vec::new();
        let reviews = crate::manager::followups::from_snapshot(reader, prev, now, now.at);
        let Some(prev) = prev else { return out };
        for s in &now.sessions {
            let before = prev.session(&s.id).map(|b| b.state);
            if before != Some(s.state) {
                let activity = match s.state {
                    SessionState::Working => "working",
                    SessionState::NeedsYou => "needs_input",
                    _ => "idle",
                };
                out.push(event("session.activity", &s.id, serde_json::json!({ "activity": activity })));
                if s.state == SessionState::NeedsYou {
                    out.push(event("session.needs_input", &s.id, serde_json::json!({ "notification_type": "permission_prompt", "message": "asks" })));
                }
            }
        }
        for review in reviews {
            if let crate::manager::Event::PrReviewed { session, repo, number, reviewer, state } = review {
                out.push(event("review.received", &session, serde_json::json!({ "repo": repo, "number": number, "reviewer": reviewer, "state": state })));
            }
        }
        out
    }

    /// The same rounds through v1's harness and through these built-ins: the notifications and
    /// the tells they give.
    fn both(rounds: &[Snapshot]) -> (Vec<Action>, Vec<Action>) {
        let s = settings();
        let mut harness = Harness::default();
        let mut native = Native::default();
        let mut reader = crate::manager::Data::default();
        let me = ["robin".to_string()];
        let (mut old, mut new) = (Vec::new(), Vec::new());
        let mut prev: Option<Snapshot> = None;
        for round in rounds {
            old.extend(harness.step(&s, round, round.at).into_iter().filter(|a| matches!(a, Action::Notify { .. } | Action::TellSession { .. })));
            let view = View { settings: &s, sessions: &round.sessions, me: &me, now_ms: round.at * 1000 };
            for e in events_of(prev.as_ref(), round, &mut reader) {
                new.extend(native.on_event(&e, &view));
            }
            if prev.is_some() {
                new.extend(native.tell_ready(&view));
            }
            prev = Some(round.clone());
        }
        (old, new)
    }

    #[test]
    fn parity_with_harness_step() {
        // A review that comes in while the session works, told when it is free; a second reviewer;
        // an approval for the person only; a stop to ask, once per stop.
        let review = |state: SessionState, reviewers: &[(&str, &str)]| vec![session("s1", state, Some(pr(12, reviewers)))];
        let scenarios: Vec<Vec<Snapshot>> = vec![
            vec![
                snap(T0, review(SessionState::Working, &[("alex", "PENDING")])),
                snap(T0 + 300, review(SessionState::Working, &[("alex", "COMMENTED")])),
                snap(T0 + 600, review(SessionState::NeedsYou, &[("alex", "COMMENTED")])),
                snap(T0 + 700, review(SessionState::NeedsYou, &[("alex", "COMMENTED")])),
                snap(T0 + 900, review(SessionState::Idle, &[("alex", "COMMENTED")])),
                snap(T0 + 960, review(SessionState::Idle, &[("alex", "COMMENTED")])),
            ],
            vec![
                snap(T0, review(SessionState::Working, &[("alex", "PENDING"), ("sam-dev", "PENDING")])),
                snap(T0 + 300, review(SessionState::Working, &[("alex", "CHANGES_REQUESTED"), ("sam-dev", "COMMENTED")])),
                snap(T0 + 600, review(SessionState::Idle, &[("alex", "CHANGES_REQUESTED"), ("sam-dev", "COMMENTED")])),
                snap(T0 + 900, review(SessionState::Idle, &[("alex", "APPROVED"), ("sam-dev", "COMMENTED")])),
            ],
            vec![
                snap(T0, review(SessionState::Working, &[])),
                snap(T0 + 60, review(SessionState::NeedsYou, &[])),
                snap(T0 + 120, review(SessionState::NeedsYou, &[])),
                snap(T0 + 300, review(SessionState::Working, &[])),
                snap(T0 + 360, review(SessionState::NeedsYou, &[])),
            ],
        ];
        for (n, rounds) in scenarios.iter().enumerate() {
            let (old, new) = both(rounds);
            assert!(!old.is_empty(), "scenario {n} does something");
            assert_eq!(new, old, "scenario {n}");
        }
    }

    #[test]
    fn needs_you_once_per_stop() {
        let s = settings();
        let me: [String; 0] = [];
        let sessions = vec![session("s1", SessionState::NeedsYou, None)];
        let view = View { settings: &s, sessions: &sessions, me: &me, now_ms: 0 };
        let mut native = Native::default();
        let asks = event("session.needs_input", "s1", serde_json::json!({ "notification_type": "idle_prompt", "message": "waiting" }));
        assert_eq!(native.on_event(&asks, &view).len(), 1);
        // Claude says it again while it waits: no second notification.
        assert!(native.on_event(&asks, &view).is_empty());
        native.on_event(&event("session.activity", "s1", serde_json::json!({ "activity": "working" })), &view);
        assert_eq!(native.on_event(&asks, &view).len(), 1, "a new stop");
        assert!(native.on_event(&event("session.needs_input", "gone", serde_json::json!({})), &view).is_empty(), "a session not there");
    }

    #[test]
    fn untrusted_review_is_for_the_person_only() {
        let s = settings();
        let me = ["robin".to_string()];
        let sessions = vec![session("s1", SessionState::Idle, Some(pr(12, &[])))];
        let view = View { settings: &s, sessions: &sessions, me: &me, now_ms: 0 };
        let mut native = Native::default();
        // A stranger's review: the person hears it; the session is never told.
        let stranger = event("review.received", "s1", serde_json::json!({ "repo": "acme/app", "number": 12, "reviewer": "drive-by", "state": "CHANGES_REQUESTED", "association": "NONE" }));
        assert_eq!(native.on_event(&stranger, &view).len(), 1);
        assert!(native.tell_ready(&view).is_empty());
        // A member of the organisation is trusted, as GitHub says.
        let member = event("review.received", "s1", serde_json::json!({ "repo": "acme/app", "number": 12, "reviewer": "kim", "state": "COMMENTED", "association": "MEMBER" }));
        native.on_event(&member, &view);
        assert_eq!(native.tell_ready(&view).len(), 1);
    }

    #[test]
    fn stuck_and_usage_tell_once() {
        let s = settings();
        let me: [String; 0] = [];
        let sessions = vec![session("s1", SessionState::Idle, None)];
        let view = View { settings: &s, sessions: &sessions, me: &me, now_ms: 86_400_000 * 3 };
        let mut native = Native::default();
        let failed = event("turn.ended", "s1", serde_json::json!({ "end": "failed" }));
        assert!(native.on_event(&failed, &view).is_empty(), "once is no pattern");
        assert_eq!(native.on_event(&failed, &view).len(), 1);
        assert!(native.on_event(&failed, &view).is_empty(), "told once");
        native.on_event(&event("turn.ended", "s1", serde_json::json!({ "end": "completed" })), &view);
        native.on_event(&failed, &view);
        assert_eq!(native.on_event(&failed, &view).len(), 1, "a good turn in between starts it again");
        let usage = event("usage.threshold", "-", serde_json::json!({ "account": "Claude (work)", "percent": 90 }));
        assert_eq!(native.on_event(&usage, &view).len(), 1);
        assert!(native.on_event(&usage, &view).is_empty());
    }
}
