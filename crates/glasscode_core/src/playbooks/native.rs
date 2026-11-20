//! The manager's own built-ins as state machines over the journal's events (M40 part 2; review
//! events-12, D27). They say what v1's harness says, from events instead of snapshot rounds, and
//! the parity tests hold them to it at the harness's `react` seam: the same recorded rounds go
//! through v1's harness and, as the events the journal writes of them, through these, and the
//! actions are compared.
//!
//! - **needs-you**: a session stopped to ask the person something: its activity became
//!   `needs_input` (what v1's snapshot reads as `NeedsYou`), or it said so (`session.needs_input`,
//!   counted only while it is in that state: Claude's idle reminder is no stop). One notification
//!   per stop; the stop ends when the session works again (`session.activity`).
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
//! engine is v1, and until v2 takes over (`manager::handover`), so the app never runs both: from
//! then on the harness keeps its bookkeeping and leaves these rules to them. Their
//! state ([`Native`]) is what the runner keeps across a restart, as v1 keeps `Data::seen`: it
//! serializes, and the parity tests carry it across their restarts.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::manager::{Action, SessionInfo, SessionState, Settings};

use super::engine::EventView;

/// The kinds of events the built-ins read.
pub const KINDS: &[&str] = &["session.needs_input", "session.activity", "review.received", "turn.ended", "usage.threshold"];

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
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Review {
    session: String,
    repo: String,
    number: u64,
    reviewer: String,
}

/// The built-ins' state, which the runner keeps across a restart (as v1 keeps `Data::seen`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Native {
    /// Sessions the person was told about in this stop.
    asked: BTreeSet<String>,
    /// Reviews to tell, by v1's tell key (`brief::tell_key`), with the state each came with.
    pending: BTreeMap<String, (Review, String)>,
    /// Per session: the cause of the last bad turn end, how many in a row, and whether told.
    turns: BTreeMap<String, (String, u32, bool)>,
    /// Per account: the day it was last told about.
    usage: BTreeMap<String, u64>,
}

/// `manager/builtins.json`: the built-ins' state between runs (the runner keeps it, M51; M44 seeds
/// it from v1's `seen`). Damaged, it starts empty: at worst one notice is said again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeFile {
    pub native: Native,
}

impl crate::store::Store for NativeFile {
    const NAME: &'static str = "manager/builtins";
    const VERSION: u32 = 1;
    const ON_DAMAGE: crate::store::OnDamage = crate::store::OnDamage::Empty;

    fn path() -> std::path::PathBuf {
        crate::manager::store::dir().join("builtins.json")
    }
}

impl Native {
    /// The built-ins' state from v1's `seen` keys (M44): a session that waits for the person was
    /// told so (`needs|<session>` = 1), and a review not yet told to its session (`tell|…` =
    /// `pending:<state>`) still waits to be.
    pub fn from_v1_seen(seen: &BTreeMap<String, String>) -> Native {
        let mut native = Native::default();
        for (key, value) in seen {
            let parts: Vec<&str> = key.split('|').collect();
            match parts.as_slice() {
                ["needs", session] if value == "1" => {
                    native.asked.insert(session.to_string());
                }
                ["tell", session, pr, reviewer] => {
                    let Some(state) = value.strip_prefix("pending:") else { continue };
                    let Some((repo, number)) = pr.rsplit_once('#') else { continue };
                    let Ok(number) = number.parse() else { continue };
                    let review = Review { session: session.to_string(), repo: repo.to_string(), number, reviewer: reviewer.to_string() };
                    native.pending.insert(key.clone(), (review, state.to_string()));
                }
                _ => {}
            }
        }
        native
    }

    /// Whether the person was told that a session waits for them, in this stop.
    pub fn asked(&self, session: &str) -> bool {
        self.asked.contains(session)
    }

    /// How many reviews wait to be told to their sessions.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

fn text(event: &dyn EventView, name: &str) -> Option<String> {
    event.field(name).and_then(|value| value.as_str().map(str::to_string))
}

impl Native {
    /// What one event leads to, at once.
    pub fn on_event(&mut self, event: &dyn EventView, view: &View) -> Vec<Action> {
        match event.kind() {
            "session.needs_input" => self.needs_you(event, view, false),
            "session.activity" => match (event.session(), text(event, "activity").as_deref()) {
                (Some(session), Some("working")) => {
                    self.asked.remove(session);
                    Vec::new()
                }
                (Some(_), Some("needs_input")) => self.needs_you(event, view, true),
                _ => Vec::new(),
            },
            "review.received" => self.review(event, view),
            "turn.ended" => self.turn(event, view),
            "usage.threshold" => self.usage(event, view),
            _ => Vec::new(),
        }
    }

    /// `stopped`: the session's activity says it waits; else only its word, which counts while
    /// the person's view of it says it waits too.
    fn needs_you(&mut self, event: &dyn EventView, view: &View, stopped: bool) -> Vec<Action> {
        let Some(id) = event.session() else { return Vec::new() };
        let Some(session) = view.session(id) else { return Vec::new() };
        if !stopped && session.state != SessionState::NeedsYou {
            return Vec::new();
        }
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
        // Each review event is a new review (the status rounds write one when a reviewer's state
        // changes): a second round of changes is told again.
        let key = crate::manager::brief::tell_key(&review.session, &review.repo, review.number, &review.reviewer);
        if state == "APPROVED" || !trusted {
            self.pending.remove(&key);
        } else {
            self.pending.insert(key, (review, state.clone()));
        }
        vec![Action::Notify { title: format!("{who} {verdict} #{number}"), body: title, session: Some(session.to_string()) }]
    }

    fn turn(&mut self, event: &dyn EventView, view: &View) -> Vec<Action> {
        let Some(session) = event.session() else { return Vec::new() };
        // The report writes its end as `{"end": "failed", "detail": "…"}`; a plain word reads too.
        let end = crate::turns::end_words(&event.field("end").unwrap_or_default());
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
        for (review, _) in self.pending.values() {
            if !sessions.contains(&review.session) {
                sessions.push(review.session.clone());
            }
        }
        let mut actions = Vec::new();
        for id in sessions {
            if !view.session(&id).is_some_and(|s| s.state == SessionState::Idle) {
                continue;
            }
            let reviews: Vec<(String, Review, String)> = self.pending.iter().filter(|(_, (r, _))| r.session == id).map(|(key, (r, s))| (key.clone(), r.clone(), s.clone())).collect();
            let tells: Vec<crate::manager::brief::PendingTell> = reviews
                .iter()
                .map(|(key, r, state)| crate::manager::brief::PendingTell { key: key.clone(), session: r.session.clone(), repo: r.repo.clone(), number: r.number, reviewer: r.reviewer.clone(), state: state.clone() })
                .collect();
            for (key, _, _) in reviews {
                self.pending.remove(&key);
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
            reviewers: reviewers.iter().map(|(l, s)| ReviewerInfo { login: (*l).into(), state: (*s).into(), ..ReviewerInfo::default() }).collect(),
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
    /// the tells they give. A round of `None` is a restart: the harness keeps its data, these keep
    /// their state (serialized and read back), and the journal's next round is a baseline.
    fn both(rounds: &[Option<Snapshot>]) -> (Vec<Action>, Vec<Action>) {
        let s = settings();
        let mut harness = Harness::default();
        let mut native = Native::default();
        let mut reader = crate::manager::Data::default();
        let me = ["robin".to_string()];
        let (mut old, mut new) = (Vec::new(), Vec::new());
        let mut prev: Option<Snapshot> = None;
        for round in rounds {
            let Some(round) = round else {
                harness = Harness { data: harness.data.clone(), prev: None, dirty: false, ..Harness::default() };
                native = serde_json::from_str(&serde_json::to_string(&native).unwrap()).unwrap();
                prev = None;
                continue;
            };
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

    /// The same rounds once v2 took over: v1's harness keeps its bookkeeping but leaves needs-you
    /// and review-tell to these built-ins, its reader hands the reviews over as `review.received`
    /// events, and together they say exactly what v1's harness alone says.
    fn through_the_hand_over(rounds: &[Option<Snapshot>]) -> (Vec<Action>, Vec<Action>) {
        use crate::manager::handover::{Mode, Owned, Owner};
        let s = settings();
        let owned = Owned::of(Mode { engine_v2: true, manager_on: true, taken_over: true, v2_before: false }, &|_| true);
        assert_eq!((owned.needs_you, owned.review_tell), (Owner::V2, Owner::V2));
        let (mut v1, mut v2) = (Harness::default(), Harness::default());
        let mut native = Native::default();
        let me = ["robin".to_string()];
        let (mut old, mut new) = (Vec::new(), Vec::new());
        let mut prev: Option<Snapshot> = None;
        let keep = |a: &Action| matches!(a, Action::Notify { .. } | Action::TellSession { .. });
        for round in rounds {
            let Some(round) = round else {
                v1 = Harness { data: v1.data.clone(), prev: None, dirty: false, ..Harness::default() };
                v2 = Harness { data: v2.data.clone(), prev: None, dirty: false, ..Harness::default() };
                native = serde_json::from_str(&serde_json::to_string(&native).unwrap()).unwrap();
                prev = None;
                continue;
            };
            old.extend(v1.step(&s, round, round.at).into_iter().filter(keep));
            let step = v2.step_with(&s, round, round.at, &owned);
            new.extend(step.actions.into_iter().filter(keep));
            let view = View { settings: &s, sessions: &round.sessions, me: &me, now_ms: round.at * 1000 };
            let mut events = events_of(prev.as_ref(), round, &mut crate::manager::Data::default()).into_iter().filter(|e| e.kind.as_str() != "review.received").collect::<Vec<_>>();
            events.extend(step.events);
            for e in events {
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
        let review = |at: u64, state: SessionState, reviewers: &[(&str, &str)]| Some(snap(at, vec![session("s1", state, Some(pr(12, reviewers)))]));
        let scenarios: Vec<Vec<Option<Snapshot>>> = vec![
            // a_busy_session_is_told_when_it_is_free, with its restart in the middle.
            vec![
                review(T0, SessionState::Working, &[("alex", "PENDING")]),
                review(T0 + 300, SessionState::Working, &[("alex", "COMMENTED")]),
                review(T0 + 600, SessionState::NeedsYou, &[("alex", "COMMENTED")]),
                review(T0 + 700, SessionState::NotRunning, &[("alex", "COMMENTED")]),
                None,
                review(T0 + 900, SessionState::Idle, &[("alex", "COMMENTED")]),
                review(T0 + 960, SessionState::Idle, &[("alex", "COMMENTED")]),
                review(T0 + 1020, SessionState::Idle, &[("alex", "COMMENTED")]),
            ],
            // two_reviews_make_one_message_and_a_second_round_of_changes_is_told_again.
            vec![
                review(T0, SessionState::Working, &[("alex", "PENDING"), ("sam-dev", "PENDING")]),
                review(T0 + 300, SessionState::Working, &[("alex", "CHANGES_REQUESTED"), ("sam-dev", "COMMENTED")]),
                review(T0 + 600, SessionState::Idle, &[("alex", "CHANGES_REQUESTED"), ("sam-dev", "COMMENTED")]),
                review(T0 + 900, SessionState::Idle, &[("alex", "PENDING"), ("sam-dev", "COMMENTED")]),
                review(T0 + 1200, SessionState::Idle, &[("alex", "CHANGES_REQUESTED"), ("sam-dev", "COMMENTED")]),
            ],
            // an_approval_is_news_for_the_person_only.
            vec![
                review(T0, SessionState::Idle, &[("alex", "CHANGES_REQUESTED")]),
                review(T0 + 300, SessionState::Idle, &[("alex", "APPROVED")]),
                review(T0 + 600, SessionState::Idle, &[("alex", "APPROVED")]),
            ],
            // the_person_is_told_once_per_stop_that_a_session_needs_them, with its restart.
            vec![
                review(T0, SessionState::Working, &[]),
                review(T0 + 60, SessionState::NeedsYou, &[]),
                review(T0 + 120, SessionState::NeedsYou, &[]),
                None,
                review(T0 + 180, SessionState::NeedsYou, &[]),
                review(T0 + 240, SessionState::NeedsYou, &[]),
                review(T0 + 300, SessionState::Working, &[]),
                review(T0 + 360, SessionState::NeedsYou, &[]),
            ],
        ];
        for (n, rounds) in scenarios.iter().enumerate() {
            let (old, new) = both(rounds);
            assert!(!old.is_empty(), "scenario {n} does something");
            assert_eq!(new, old, "scenario {n}");
            let (old, new) = through_the_hand_over(rounds);
            assert_eq!(new, old, "scenario {n}, through the hand-over");
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

    /// The journal's `turn.ended` as the turns write it (`turns::ended_event`): its end is
    /// `{"end": "failed", "detail": …}`, and two failures on the same cause tell the person.
    #[test]
    fn a_stuck_session_from_real_turn_reports() {
        use crate::turns::{TurnEnd, TurnReport, ended_event};
        let s = settings();
        let me: [String; 0] = [];
        let sessions = vec![session("s1", SessionState::Idle, None)];
        let view = View { settings: &s, sessions: &sessions, me: &me, now_ms: 0 };
        let report = |turn: &str, end: TurnEnd| TurnReport {
            v: 1,
            session: "s1".into(),
            turn: turn.into(),
            started_ms: 1,
            ended_ms: 2,
            end,
            tools: 0,
            failed_tools: Vec::new(),
            commands: Vec::new(),
            files: Vec::new(),
            commits: Vec::new(),
            pushed: false,
            last_reply: String::new(),
            question: None,
            background: Vec::new(),
            by_agent: false,
            partial: false,
        };
        let failed = |turn: &str| ended_event(None, &report(turn, TurnEnd::Failed("the API returned 529".into())));
        let mut native = Native::default();
        assert!(native.on_event(&failed("t1"), &view).is_empty());
        let told = native.on_event(&failed("t2"), &view);
        assert_eq!(told.len(), 1, "two failures on the same cause");
        assert!(matches!(&told[0], Action::Notify { body, .. } if body.contains("failed: the API returned 529")), "{told:?}");
        native.on_event(&ended_event(None, &report("t3", TurnEnd::Completed)), &view);
        assert!(native.on_event(&failed("t4"), &view).is_empty(), "a good turn starts it again");
    }
}
