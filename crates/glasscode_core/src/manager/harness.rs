//! The rules: what changed since the last round, and what the manager does about it. Pure:
//! it is handed a `Snapshot` and returns `Action`s; whoever runs it (the app, the server)
//! carries them out.
//!
//! What was already said or done is kept in `Data::seen`, so nothing is said twice, also not
//! after a restart or by the other copy (keys are never removed, values change):
//! - `needs|<session>` → "1" while the session waits for the person (told once per stop),
//! - `done|<session>` → when it last finished (`brief::done_key`),
//! - `tell|<session>|<repo>#<n>|<login>` → "pending:<STATE>" until the session was told about
//!   that review, then "told:<STATE>" (`brief::tell_key`),
//! - `send|<message id>` → "<the message's updated>@<when>": a send was asked for,
//! - and the keys of `followups`.

use super::brief::{self, PendingTell};
use super::gh::{one_line, GivenReview};
use super::handover::{Owned, Owner};
use super::{followups, memory, messages};
use super::{Action, Check, Data, Event, FollowKind, FollowState, FollowUp, InMessage, MsgState, PanelTab, Person, SessionState, Settings, Show, Snapshot, Tier};

/// A follow-up nobody touched for this long is brought up again.
pub const STALE_AFTER: u64 = 48 * 3600;
/// A send that was asked for and never reported back is asked for again after this long.
const RESEND_AFTER: u64 = 15 * 60;
/// The heartbeat alone is written to disk at most this often.
const HEARTBEAT_EVERY: u64 = 15;
/// At most this many scheduled checks are handed out in one go (each is a question to the
/// agent), and none runs more often than this.
const CHECKS_AT_ONCE: usize = 2;
const CHECK_MIN_SECS: u64 = 60;

/// What the manager says to a question it can answer without thinking (status, what is
/// blocked, today's update), and what to do with it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answer {
    pub say: String,
    pub actions: Vec<Action>,
}

/// What one round leads to: the actions, and the events for the journal (v2's review-tell).
#[derive(Clone, Debug, Default)]
pub struct Round {
    pub actions: Vec<Action>,
    pub events: Vec<crate::journal::Event>,
}

#[derive(Default)]
pub struct Harness {
    pub data: Data,
    /// The last round, to compare with.
    pub prev: Option<Snapshot>,
    /// `data` changed since it was last saved.
    pub dirty: bool,
    /// v2 took the outbox over (`Harness::use_v2_outbox`): `data.outbox` is v2's
    /// (`manager/outbox.json`), and this is v1's, kept as v1 left it in `data.json`.
    pub v1_outbox: Option<Vec<super::OutMessage>>,
    /// v2's outbox could not be read: nothing is sent until it is repaired (never v1's instead).
    pub outbox_error: Option<String>,
}

/// Minutes since local midnight at a Unix time.
pub fn local_minutes(now: u64) -> u32 {
    brief::parse_clock(&crate::day::clock(now as i64)).unwrap_or(0)
}

/// The local day of a Unix time, as an ISO date.
pub fn local_day(now: u64) -> String {
    crate::day::iso(crate::day::local(now as i64))
}

/// The snapshot without the sessions of projects the manager was told to leave alone. The
/// app already leaves them out; this makes sure of it.
fn managed(settings: &Settings, snap: &Snapshot) -> Snapshot {
    let mut snap = snap.clone();
    snap.sessions.retain(|s| !settings.off_projects.contains(&s.project_id));
    snap
}

/// What changed about the sessions themselves since the last round: one that stopped to ask
/// (once per stop), one that finished, one that is new.
fn session_events(data: &mut Data, prev: Option<&Snapshot>, snap: &Snapshot, now: u64) -> Vec<Event> {
    let mut events = Vec::new();
    for s in &snap.sessions {
        let key = format!("needs|{}", s.id);
        let waited = data.seen.get(&key).map(String::as_str) == Some("1");
        if s.state == SessionState::NeedsYou {
            if !waited {
                data.seen.insert(key, "1".into());
                if prev.is_some() {
                    events.push(Event::SessionNeedsYou { session: s.id.clone() });
                }
            }
        } else if waited {
            data.seen.insert(key, "0".into());
        }
        let Some(prev) = prev else { continue };
        match prev.session(&s.id) {
            None => {
                if s.state != SessionState::NotRunning {
                    events.push(Event::SessionStarted { session: s.id.clone() });
                }
            }
            Some(before) if before.state == SessionState::Working && s.state == SessionState::Idle => {
                data.seen.insert(brief::done_key(&s.id), now.to_string());
                events.push(Event::SessionFinished { session: s.id.clone() });
            }
            Some(_) => {}
        }
    }
    events
}

/// Where a follow-up is looked at: its session's pull request, else the manager's page.
fn show_follow(f: &FollowUp, snap: Option<&Snapshot>) -> Option<Show> {
    match f.session.as_deref().filter(|id| snap.is_none_or(|s| s.session(id).is_some())) {
        Some(id) => brief::show_session(id, Some(PanelTab::PullRequest), Some("reviews")),
        None => brief::show_manager("followups"),
    }
}

/// A teammate fixed what the person asked for: tell the person, once.
fn fixed_actions(f: &FollowUp, snap: Option<&Snapshot>) -> Vec<Action> {
    let line = brief::sentence(&followups::line(f));
    vec![
        Action::Notify { title: format!("#{} is ready for you to look again", f.number), body: line.clone(), session: f.session.clone() },
        Action::Say { text: brief::spoken(&line), show: show_follow(f, snap) },
    ]
}

/// The reminder to the author of a pull request the person reviewed, in the person's words.
pub fn nudge_text(f: &FollowUp) -> String {
    let total = f.blockers.len();
    let open = f.open_blockers();
    let what = if total == 0 || open == 0 {
        "the changes I asked for in my review are still open".to_string()
    } else if total == 1 {
        "the point from my review is still open".to_string()
    } else if open == 1 {
        format!("1 of the {total} points from my review is still open")
    } else {
        format!("{open} of the {total} points from my review are still open")
    };
    format!("#{}: {what}. Could you take a look?", f.number)
}

/// The teammate a follow-up is about, when the person listed them.
fn person_of<'a>(settings: &'a Settings, f: &FollowUp) -> Option<&'a Person> {
    [f.login.trim(), f.person.trim()].into_iter().filter(|w| !w.is_empty()).find_map(|w| settings.person(w)).filter(|p| !p.name.trim().is_empty())
}

/// The teammate who wrote a message: by name or login, or by how the messenger knows them
/// (the writer or the chat is their `contact`).
fn sender_of<'a>(settings: &'a Settings, msg: &InMessage) -> Option<&'a Person> {
    let from = msg.from.trim();
    let by_name = if from.is_empty() { None } else { settings.person(from) };
    by_name.or_else(|| settings.people.iter().find(|p| (!from.is_empty() && is_contact(p, from)) || (!msg.chat.is_empty() && is_contact(p, &msg.chat))))
}

/// Whether this is how the messenger knows the teammate (letter case does not count).
fn is_contact(person: &Person, what: &str) -> bool {
    person.contact.as_deref().map(str::trim).is_some_and(|c| !c.is_empty() && c.eq_ignore_ascii_case(what.trim()))
}

/// What a session is told about the reviews on its pull request.
pub(crate) fn tell_text(settings: &Settings, tells: &[PendingTell]) -> String {
    let mut numbers: Vec<u64> = Vec::new();
    for t in tells {
        if !numbers.contains(&t.number) {
            numbers.push(t.number);
        }
    }
    let mut parts: Vec<String> = Vec::new();
    for &number in &numbers {
        // Who asked for changes, and who only left comments.
        let names = |changes: bool| tells.iter().filter(|t| t.number == number && (t.state == "CHANGES_REQUESTED") == changes).map(|t| brief::name_of(settings, &t.reviewer)).collect::<Vec<_>>();
        let asked = names(true);
        let commented = names(false);
        let mut what = Vec::new();
        if !asked.is_empty() {
            what.push(format!("{} asked for changes on PR #{number}", brief::and_list(&asked)));
        }
        if !commented.is_empty() {
            what.push(format!("{} left comments on PR #{number}", brief::and_list(&commented)));
        }
        let several = asked.len() + commented.len() > 1;
        parts.push(format!("{}. Read the {} on PR #{number} and address {}.", what.join(", and "), if several { "reviews" } else { "review" }, if several { "them" } else { "it" }));
    }
    let name = settings.name.trim();
    let from = if name.is_empty() { "Your manager".to_string() } else { name.to_string() };
    format!("{from} here: {} Tell me when it is done.", parts.join(" "))
}

/// The question without a greeting or the manager's name in front, as asked (for what is to
/// be remembered) and in lower case without its last punctuation (to see what is asked).
fn plain_question(settings: &Settings, question: &str) -> (String, String) {
    let asked = question.replace(['’', '‘'], "'");
    let mut rest = asked.trim();
    let name = settings.name.to_lowercase();
    let names: Vec<&str> = name.split_whitespace().collect();
    while let Some(word) = rest.split_whitespace().next() {
        let bare = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        let filler = matches!(bare.as_str(), "hey" | "hi" | "hello" | "ok" | "okay" | "please" | "so" | "and") || (!bare.is_empty() && names.contains(&bare.as_str()));
        if !filler || rest.split_whitespace().count() <= 1 {
            break;
        }
        rest = rest[word.len()..].trim_start_matches(|c: char| c.is_whitespace() || c == ',');
    }
    let low = rest.to_lowercase();
    let low = low.trim().trim_end_matches(['?', '!', '.', ',']).trim_end();
    let low = low.strip_suffix(" please").unwrap_or(low).trim_end_matches(',').to_string();
    (rest.to_string(), low)
}

/// What comes after one of these beginnings, in the words as asked.
fn after<'a>(asked: &'a str, low: &str, starts: &[&str]) -> Option<&'a str> {
    for start in starts {
        if low.starts_with(start) && asked.is_char_boundary(start.len()) && asked.len() >= start.len() && asked[..start.len()].eq_ignore_ascii_case(start) {
            let rest = &asked[start.len()..];
            // A whole word, not the start of a longer one ("remembering").
            if rest.is_empty() || rest.starts_with(|c: char| !c.is_alphanumeric()) || start.ends_with(' ') {
                return Some(rest.trim().trim_start_matches([':', ',']).trim());
            }
        }
    }
    None
}

fn any_in(low: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|p| low.contains(p))
}

/// The pull request number in "#1111" or "pr 1111".
fn number_in(low: &str) -> Option<u64> {
    let digits = |text: &str| text.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<u64>().ok();
    if let Some((_, rest)) = low.split_once('#') {
        return digits(rest);
    }
    let words: Vec<&str> = low.split_whitespace().collect();
    words.windows(2).find(|w| matches!(w[0], "pr" | "request" | "number")).and_then(|w| digits(w[1]))
}

/// The words as they were said, with one full stop at the end (nothing is put in capitals:
/// "iOS" stays "iOS").
fn with_stop(text: &str) -> String {
    let t = text.trim().trim_end_matches(['.', ':', ';', ',']).trim_end();
    if t.ends_with(['?', '!']) { t.to_string() } else { format!("{t}.") }
}

/// A few lines read out one after the other, with how many more there are.
fn read_out(lines: &[String], max: usize) -> String {
    let mut out: Vec<String> = lines.iter().take(max).map(|l| brief::sentence(l)).collect();
    if lines.len() > max {
        out.push(format!("And {} more.", lines.len() - max));
    }
    out.join(" ")
}

impl Harness {
    /// With the data from disk.
    pub fn load() -> Harness {
        Harness { data: super::store::load(), ..Harness::default() }
    }

    /// Save if anything changed. With v2's outbox, `data.json` keeps v1's as v1 left it, and the
    /// outbox goes to `manager/outbox.json`.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        if self.v1_outbox.is_none() {
            if super::store::save(&self.data).is_ok() {
                self.dirty = false;
            }
            return;
        }
        use crate::store::Store as _;
        let outbox = super::messages::Outbox { messages: self.data.outbox.clone() };
        let saved = super::store::save(&self.v1_data()).is_ok();
        match outbox.save() {
            Ok(()) if saved => self.dirty = false,
            Ok(()) => {}
            Err(err) => log::warn!("manager: the outbox could not be written ({err})"),
        }
    }

    /// v2 takes the outbox over (M44's hand-over): from now on the messages the manager drafts,
    /// the person allows and the sender sends are `manager/outbox.json`'s, which M44 filled from
    /// v1's. One that could not be read holds every send (`outbox_error`): never v1's instead.
    pub fn use_v2_outbox(&mut self, v2: Result<Vec<super::OutMessage>, String>) {
        if self.v1_outbox.is_some() {
            return;
        }
        match v2 {
            Ok(messages) => {
                self.v1_outbox = Some(std::mem::replace(&mut self.data.outbox, messages));
                self.outbox_error = None;
            }
            Err(why) => self.outbox_error = Some(why),
        }
    }

    /// What `data.json` holds: with v2's outbox, v1's in its place.
    pub fn v1_data(&self) -> Data {
        let mut data = self.data.clone();
        if let Some(v1) = &self.v1_outbox {
            data.outbox = v1.clone();
        }
        data
    }

    /// Take in the other copy's `data.json` (`store::merge`): with v2's outbox, theirs is merged
    /// into v1's, never into v2's.
    pub fn merge_theirs(&mut self, theirs: &Data) -> bool {
        let Some(mut v1) = self.v1_outbox.take() else { return super::store::merge(&mut self.data, theirs) };
        std::mem::swap(&mut self.data.outbox, &mut v1);
        let changed = super::store::merge(&mut self.data, theirs);
        std::mem::swap(&mut self.data.outbox, &mut v1);
        self.v1_outbox = Some(v1);
        changed
    }

    /// v2 takes over (M44): from now on its built-ins say what this harness said. The first time
    /// (or the first after v1 ran again), the built-ins' state comes from `seen` as it is now, so
    /// nothing v1 said is said again and a review not yet told is not lost; after that, theirs
    /// is kept (None).
    pub fn take_over(&mut self, now: u64) -> Option<crate::playbooks::native::Native> {
        if self.data.v2_since.is_some() {
            return None;
        }
        self.data.v2_since = Some(now);
        self.dirty = true;
        Some(crate::playbooks::native::Native::from_v1_seen(&self.data.seen))
    }

    /// One round with everything v1's ([`Harness::step_with`]).
    pub fn step(&mut self, settings: &Settings, snap: &Snapshot, now: u64) -> Vec<Action> {
        self.step_with(settings, snap, now, &Owned::V1).actions
    }

    /// One round: compare with the last, update follow-ups, and say what to do. Among others:
    /// a session whose pull request was reviewed is told so (`Action::TellSession`), the person
    /// is told what needs them, an outbox message that may go is sent (`Action::Send`), the
    /// daily brief is made when due.
    ///
    /// The first round after a start only records what is there: nothing is done about it,
    /// so a restart does not bring old news back.
    ///
    /// `owned` says which rules are this harness's (`handover`): for one that is v2's or
    /// nobody's, the round keeps all its bookkeeping (`seen`, the follow-ups) and only leaves
    /// its actions out; a review for v2's review-tell comes back as a `review.received` event
    /// for the journal instead.
    pub fn step_with(&mut self, settings: &Settings, snap: &Snapshot, now: u64, owned: &Owned) -> Round {
        if !settings.enabled {
            return Round::default();
        }
        // The engine is back at v1: the next take-over starts from `seen` again. (A v2 round
        // before this launch's take-over keeps it: the built-ins' own state is newer.)
        if !owned.engine_v2 && self.data.v2_since.is_some() {
            self.data.v2_since = None;
        }
        let snap = managed(settings, snap);
        let mut before = self.data.clone();
        let first = self.prev.is_none();
        let mut actions = Vec::new();

        let mut events = followups::from_snapshot(&mut self.data, self.prev.as_ref(), &snap, now);
        events.extend(session_events(&mut self.data, self.prev.as_ref(), &snap, now));
        followups::fill_people(&mut self.data, settings);

        let mut journal = Vec::new();
        if !first {
            events.extend(followups::stale(&mut self.data, STALE_AFTER, now));
            for event in &events {
                self.react(settings, &snap, event, owned, &mut actions, &mut journal);
            }
            if owned.review_tell == Owner::V1 {
                self.tell_sessions(settings, &snap, &mut actions);
            }
            if owned.send != Owner::Nobody {
                self.send_ready(settings, now, &mut actions);
            }
            if owned.brief != Owner::Nobody {
                self.daily_brief(settings, &snap, now, &mut actions);
            }
        }

        // The heartbeat moves every round; it alone is worth a write only now and then.
        let beat = self.data.heartbeat;
        self.data.heartbeat = now;
        before.heartbeat = now;
        if self.data != before || beat == 0 || now / HEARTBEAT_EVERY != beat / HEARTBEAT_EVERY {
            self.dirty = true;
        }
        self.prev = Some(snap);
        Round { actions, events: journal }
    }

    /// What one event leads to.
    fn react(&mut self, settings: &Settings, snap: &Snapshot, event: &Event, owned: &Owned, actions: &mut Vec<Action>, journal: &mut Vec<crate::journal::Event>) {
        match event {
            // v2's review-tell hears of it through the journal, with what GitHub says of the
            // reviewer (D27).
            Event::PrReviewed { session, repo, number, reviewer, state } if owned.review_tell == Owner::V2 => {
                let s = snap.session(session);
                let association = s.and_then(|s| s.pr.as_ref()).and_then(|pr| pr.reviewers.iter().find(|r| r.login.eq_ignore_ascii_case(reviewer))).and_then(|r| r.association.clone());
                // The project's key is the app's to add (it knows the session's).
                journal.push(crate::journal::review_received(session, None, repo, *number, reviewer, association.as_deref(), state));
            }
            Event::PrReviewed { .. } if owned.review_tell == Owner::Nobody => {}
            Event::SessionNeedsYou { .. } if owned.needs_you != Owner::V1 => {}
            Event::FollowUpFixed { .. } | Event::FollowUpStale { .. } if owned.follow_ups == Owner::Nobody => {}
            Event::PrReviewed { session, repo, number, reviewer, state } => {
                let who = brief::name_of(settings, reviewer);
                let verdict = match state.as_str() {
                    "APPROVED" => "approved",
                    "CHANGES_REQUESTED" => "asked for changes on",
                    _ => "left comments on",
                };
                let s = snap.session(session);
                let title = s.and_then(|s| s.pr.as_ref()).map(|pr| pr.title.trim().to_string()).filter(|t| !t.is_empty()).or_else(|| s.map(brief::called)).unwrap_or_default();
                actions.push(Action::Notify { title: format!("{who} {verdict} #{number}"), body: title, session: Some(session.clone()) });
                // An approval asks nothing of the session; anything else it has to hear about,
                // as soon as it is free.
                let note = if state == "APPROVED" { format!("none:{state}") } else { format!("pending:{state}") };
                self.data.seen.insert(brief::tell_key(session, repo, *number, reviewer), note);
            }
            Event::SessionNeedsYou { session } => {
                let Some(s) = snap.session(session) else { return };
                actions.push(Action::Notify { title: format!("{} needs you", brief::called(s)), body: brief::reply_line(s, 160), session: Some(session.clone()) });
            }
            Event::FollowUpFixed { id } => {
                if let Some(f) = self.data.followups.iter().find(|f| f.id == *id) {
                    actions.extend(fixed_actions(f, Some(snap)));
                }
            }
            Event::FollowUpStale { id } => {
                let Some(f) = self.data.followups.iter().find(|f| f.id == *id) else { return };
                let days = STALE_AFTER / 86_400;
                match person_of(settings, f).filter(|_| f.kind == FollowKind::BlockersGiven) {
                    // A draft only: whether it waits for the person or goes is the outbox's rule.
                    Some(p) => actions.push(Action::Draft { to: p.name.clone(), body: nudge_text(f), why: format!("Nothing happened on #{} for {days} days.", f.number), about: Some(f.id.clone()) }),
                    None => actions.push(Action::Notify { title: format!("Quiet for {days} days"), body: brief::sentence(&followups::line(f)), session: f.session.clone() }),
                }
            }
            // The app fixes failing checks by itself; the brief names them.
            Event::CiFailed { .. } | Event::SessionStarted { .. } | Event::SessionFinished { .. } | Event::MessageIn { .. } | Event::BriefDue => {}
        }
    }

    /// Tell the sessions that are free about the reviews that wait for them: one message per
    /// session, each review once.
    fn tell_sessions(&mut self, settings: &Settings, snap: &Snapshot, actions: &mut Vec<Action>) {
        let pending = brief::pending_tells(&self.data);
        let mut sessions: Vec<&str> = Vec::new();
        for t in &pending {
            if !sessions.contains(&t.session.as_str()) {
                sessions.push(&t.session);
            }
        }
        for id in sessions {
            // Never typed into a session that works, waits for an answer or is not running.
            if !snap.session(id).is_some_and(|s| s.state == SessionState::Idle) {
                continue;
            }
            let tells: Vec<PendingTell> = pending.iter().filter(|t| t.session == id).cloned().collect();
            for t in &tells {
                self.data.seen.insert(t.key.clone(), format!("told:{}", t.state));
            }
            actions.push(Action::TellSession { session: id.to_string(), text: tell_text(settings, &tells) });
        }
    }

    /// Ask for the messages that may go to be sent, each once (none while v2's outbox cannot be
    /// read).
    fn send_ready(&mut self, settings: &Settings, now: u64, actions: &mut Vec<Action>) {
        if self.outbox_error.is_some() {
            return;
        }
        for id in messages::ready(&self.data, settings) {
            let Some(m) = self.data.outbox.iter().find(|m| m.id == id) else { continue };
            if m.tier == Tier::Never || matches!(m.state, MsgState::Sent | MsgState::Rejected) {
                continue;
            }
            let key = format!("send|{id}");
            let stamp = m.updated.to_string();
            let asked = self.data.seen.get(&key).and_then(|v| v.split_once('@')).map(|(stamp, at)| (stamp.to_string(), at.parse::<u64>().unwrap_or(0)));
            let again = match asked {
                None => true,
                // Allowed again after a change, or the send never reported back.
                Some((old, at)) => (old != stamp && m.state != MsgState::Failed) || now.saturating_sub(at) >= RESEND_AFTER,
            };
            if again {
                self.data.seen.insert(key, format!("{stamp}@{now}"));
                actions.push(Action::Send { id });
            }
        }
    }

    /// Make and say the daily brief when its time has come.
    fn daily_brief(&mut self, settings: &Settings, snap: &Snapshot, now: u64, actions: &mut Vec<Action>) {
        if !brief::due(settings, &self.data, &local_day(now), local_minutes(now)) {
            return;
        }
        let made = brief::build(settings, &self.data, snap, now);
        actions.push(Action::Say { text: made.text.clone(), show: brief::show_manager("overview") });
        self.data.brief = Some(made);
    }

    /// The person's reviews, fresh from GitHub (every few minutes).
    pub fn on_reviews(&mut self, settings: &Settings, reviews: &[GivenReview], now: u64) -> Vec<Action> {
        self.on_reviews_with(settings, reviews, now, &Owned::V1)
    }

    /// [`Harness::on_reviews`], saying nothing when the follow-ups are nobody's (`handover`).
    pub fn on_reviews_with(&mut self, settings: &Settings, reviews: &[GivenReview], now: u64, owned: &Owned) -> Vec<Action> {
        if !settings.enabled {
            return Vec::new();
        }
        let before = self.data.clone();
        let events = followups::from_reviews(&mut self.data, settings, reviews, now);
        let mut actions = Vec::new();
        for event in events.into_iter().filter(|_| owned.follow_ups != Owner::Nobody) {
            let Event::FollowUpFixed { id } = event else { continue };
            if let Some(f) = self.data.followups.iter().find(|f| f.id == id) {
                actions.extend(fixed_actions(f, self.prev.as_ref()));
            }
        }
        if self.data != before {
            self.dirty = true;
        }
        actions
    }

    /// New messages from teammates. An urgent one from someone allowed to (`Person::urgent`)
    /// notifies the person at once and drafts a holding reply; nothing else is answered.
    pub fn on_inbox(&mut self, settings: &Settings, messages: Vec<InMessage>, now: u64) -> Vec<Action> {
        if !settings.enabled {
            return Vec::new();
        }
        let mut actions = Vec::new();
        for msg in messages {
            if msg.id.is_empty() {
                continue;
            }
            if let Some(known) = self.data.inbox.iter_mut().find(|m| m.id == msg.id) {
                // Seen before; the person may have read it in their messenger since.
                if msg.handled && !known.handled {
                    known.handled = true;
                    self.dirty = true;
                }
                continue;
            }
            // What the person has read already is no alarm.
            let sender = sender_of(settings, &msg).filter(|p| p.urgent && msg.urgent && !msg.handled).cloned();
            self.data.inbox.push(msg.clone());
            self.dirty = true;
            let Some(sender) = sender else { continue };
            let who = if sender.name.trim().is_empty() { msg.from.trim().to_string() } else { sender.name.trim().to_string() };
            actions.push(Action::Notify { title: format!("Urgent from {who}"), body: one_line(&msg.text, 160), session: None });
            // The holding reply goes through the outbox like every message: signed, sorted,
            // and sent only when the person's rules allow it.
            messages::hold(&mut self.data, settings, &who, Some(&msg.id), now);
        }
        actions
    }

    /// The scheduled checks (`Data::checks`) whose time has come, at most a few at a time:
    /// each is a question for the agent, in the person's words (`Check::what`). They are
    /// marked as run here: a repeating one waits another interval, a one-off is dropped for
    /// good (`store::drop_check`). Whoever runs the manager asks the agent and tells the
    /// person what came of it.
    pub fn due_checks(&mut self, settings: &Settings, now: u64) -> Vec<Check> {
        self.due_checks_but(settings, now, &|_| false)
    }

    /// [`Harness::due_checks_but`] when the checks are v1's (`handover`): none at all with the
    /// engine at v2, where v1's brain, which asks them, does not run.
    pub fn due_checks_owned(&mut self, settings: &Settings, now: u64, owned: &Owned, scheduled: &dyn Fn(&str) -> bool) -> Vec<Check> {
        if owned.checks != Owner::V1 {
            return Vec::new();
        }
        self.due_checks_but(settings, now, scheduled)
    }

    /// [`Harness::due_checks`] but those `scheduled` says a schedule runs now (v2: the person
    /// approved its `v1-check-<id>` schedule, M44): they are neither handed out nor marked run.
    pub fn due_checks_but(&mut self, settings: &Settings, now: u64, scheduled: &dyn Fn(&str) -> bool) -> Vec<Check> {
        if !settings.enabled {
            return Vec::new();
        }
        let mut due: Vec<Check> = self.data.checks.iter().filter(|c| !c.what.trim().is_empty() && now.saturating_sub(c.last) >= c.every_secs.max(CHECK_MIN_SECS) && !scheduled(&c.id)).cloned().collect();
        // The one that waited longest first.
        due.sort_by_key(|c| c.last);
        due.truncate(CHECKS_AT_ONCE);
        for check in &due {
            if check.once {
                super::store::drop_check(&mut self.data, &check.id);
            } else if let Some(c) = self.data.checks.iter_mut().find(|c| c.id == check.id) {
                c.last = now;
            }
        }
        if !due.is_empty() {
            self.dirty = true;
        }
        due
    }

    /// Answer without the brain when the question is one of the plain ones ("what is
    /// blocked?", "give me today's update", "status", "what needs me?", "remember …",
    /// "forget …"). None: ask the brain.
    pub fn quick_answer(&mut self, settings: &Settings, snap: &Snapshot, question: &str, now: u64) -> Option<Answer> {
        if !settings.enabled {
            return None;
        }
        let snap = managed(settings, snap);
        let (asked, low) = plain_question(settings, question);
        if low.is_empty() {
            return None;
        }
        let words = low.split_whitespace().count();
        let say = |text: String| Some(Answer { say: brief::spoken(&text), actions: Vec::new() });

        // What it remembers.
        if let Some(topic) = after(&asked, &low, &["what do you remember", "what did you remember", "what have you remembered", "do you remember", "what is in your memory", "what's in your memory", "show me your memory", "show your memory"]) {
            let topic = topic.trim_end_matches(['?', '!', '.']).trim();
            let topic = ["about ", "of ", "on "].iter().find_map(|p| topic.strip_prefix(p)).unwrap_or(topic).trim();
            let general = topic.is_empty() || matches!(topic.to_lowercase().as_str(), "anything" | "everything" | "something" | "so far");
            let notes: Vec<String> = if general { memory::all(&self.data).into_iter().take(8).map(|n| n.text.clone()).collect() } else { memory::recall(&self.data, topic, 5).into_iter().map(|n| n.text.clone()).collect() };
            return say(match (notes.is_empty(), general) {
                (true, true) => "I do not remember anything yet.".to_string(),
                (true, false) => format!("I remember nothing about {topic}."),
                (false, true) => format!("I remember {}. {}", if notes.len() == 1 { "1 thing".to_string() } else { format!("{} things", notes.len()) }, read_out(&notes, 8)),
                (false, false) => format!("About {topic} I remember: {}", read_out(&notes, 5)),
            });
        }

        // A follow-up that is done ("done with #1111", "drop #1111").
        // ("close #12" alone is left to the brain: it may mean the pull request, which is not
        // the manager's to close.)
        let closes = ["done with", "i am done with", "i'm done with", "im done with", "finished with", "close the follow", "close follow"];
        let drops = ["drop #", "drop pr ", "drop the follow", "stop tracking", "stop following", "stop watching", "do not follow", "don't follow"];
        let is_close = closes.iter().any(|p| low.starts_with(p)) || low.ends_with(" is done") || low.ends_with(" is finished") || low.ends_with(" as done");
        let is_drop = drops.iter().any(|p| low.starts_with(p));
        if let Some(number) = number_in(&low).filter(|_| (is_close || is_drop) && words <= 9) {
            let closed = followups::close(&mut self.data, &format!("#{number}"), is_drop, now);
            if closed.is_some() {
                self.dirty = true;
            }
            return say(match closed {
                Some(_) if is_drop => format!("I no longer follow #{number}."),
                Some(f) => format!("Done. The follow-up on #{number} is closed{}.", if f.title.trim().is_empty() { String::new() } else { format!(": {}", one_line(&f.title, 60)) }),
                None => format!("I have no open follow-up on #{number}."),
            });
        }

        // Remember and forget.
        if let Some(fact) = after(&asked, &low, &["remember that ", "remember this: ", "remember", "note that ", "keep in mind that ", "keep in mind", "don't forget that ", "do not forget that "]) {
            let fact = fact.trim();
            if fact.is_empty() || matches!(fact.to_lowercase().trim_end_matches(['.', '!']), "that" | "this" | "it" | "me") || fact.ends_with('?') {
                return None;
            }
            let id = memory::remember(&mut self.data, fact, "you", now);
            if id.is_empty() {
                return None;
            }
            self.dirty = true;
            return say(format!("I will remember that: {}", with_stop(fact)));
        }
        if let Some(what) = after(&asked, &low, &["forget that ", "forget about ", "forget"]) {
            let what = what.trim().trim_end_matches(['.', '!', '?']).trim();
            if what.is_empty() || matches!(what.to_lowercase().as_str(), "it" | "that" | "this" | "about it" | "everything" | "all" | "all of it" | "me") {
                return None;
            }
            return say(match memory::forget(&mut self.data, what, now) {
                Some(text) => {
                    self.dirty = true;
                    format!("Forgotten: {}", with_stop(&text))
                }
                None => "I am not sure which note you mean, so I forgot nothing.".to_string(),
            });
        }

        // Today's update: the brief, and a walk through the app.
        let wants_brief = matches!(low.as_str(), "brief" | "briefing" | "update" | "the update" | "my update")
            || any_in(&low, &["today's update", "todays update", "today update", "daily update", "daily brief", "morning brief", "morning update", "the brief", "brief me", "catch me up", "give me an update", "give me the update", "give me my update", "what's new", "whats new", "what is new", "what did i miss", "walk me through today", "walk me through it", "walk me through everything"]);
        if wants_brief && words <= 12 {
            let made = brief::build(settings, &self.data, &snap, now);
            let stops = brief::tour(settings, &self.data, &snap);
            let text = made.text.clone();
            self.data.brief = Some(made);
            self.dirty = true;
            return Some(Answer { say: text, actions: if stops.is_empty() { Vec::new() } else { vec![Action::Tour { stops }] } });
        }

        // Follow-ups.
        // (Asking for a new one, "follow up with Sam on …", is the brain's.)
        let adds = ["follow up", "follow-up with", "add", "create", "make", "track", "keep an eye", "watch", "start"].iter().any(|p| low.starts_with(p));
        if words <= 10 && !adds && any_in(&low, &["follow-up", "followup", "follow up", "blockers", "what am i waiting", "who am i waiting", "who owes me", "what is open with", "what's open with"]) {
            let open = followups::open(&self.data);
            if open.is_empty() {
                return say("No follow-ups are open.".to_string());
            }
            let lines: Vec<String> = open.iter().map(|f| followups::line(f)).collect();
            let head = if lines.len() == 1 { "1 follow-up is open.".to_string() } else { format!("{} follow-ups are open.", lines.len()) };
            return Some(Answer { say: brief::spoken(&format!("{head} {}", read_out(&lines, 5))), actions: brief::show_manager("followups").map(|show| Action::Show { show }).into_iter().collect() });
        }

        // What needs the person.
        if words <= 10 && any_in(&low, &["what needs me", "what needs my", "who needs me", "what do i need to", "what do i have to", "anything need me", "anything needs me", "anything for me", "waiting for me", "waiting on me", "waits for me", "what should i do", "what should i look at", "needs you", "need me now", "what needs you"]) {
            let made = brief::build(settings, &self.data, &snap, now);
            let mut lines: Vec<(String, Option<Show>)> = made.lines.iter().filter(|l| matches!(l.kind.as_str(), "needs_you" | "review" | "message")).map(|l| (l.text.clone(), l.show.clone())).collect();
            for f in followups::open(&self.data).into_iter().filter(|f| f.state == FollowState::Fixed) {
                lines.push((followups::line(f), show_follow(f, Some(&snap))));
            }
            if lines.is_empty() {
                return say("Nothing needs you right now.".to_string());
            }
            let head = if lines.len() == 1 { "1 thing needs you.".to_string() } else { format!("{} things need you.", lines.len()) };
            let texts: Vec<String> = lines.iter().map(|(t, _)| t.clone()).collect();
            let first = lines.iter().find_map(|(_, show)| show.clone());
            return Some(Answer { say: brief::spoken(&format!("{head} {}", read_out(&texts, 5))), actions: first.map(|show| Action::Show { show }).into_iter().collect() });
        }

        // What is blocked.
        if words <= 8 && any_in(&low, &["blocked", "stuck", "what is failing", "what's failing", "what fails", "anything failing", "anything red"]) {
            let made = brief::build(settings, &self.data, &snap, now);
            let blocked: Vec<&super::BriefLine> = made.lines.iter().filter(|l| l.kind == "blocked").collect();
            let waiting: Vec<String> = followups::open(&self.data).into_iter().filter(|f| f.state == FollowState::Open && f.kind != FollowKind::ReviewReceived).map(followups::line).collect();
            if blocked.is_empty() && waiting.is_empty() {
                return say("Nothing is blocked.".to_string());
            }
            let mut parts = Vec::new();
            if blocked.is_empty() {
                parts.push("Nothing of yours is blocked.".to_string());
            } else {
                let texts: Vec<String> = blocked.iter().map(|l| l.text.clone()).collect();
                parts.push(format!("{} blocked. {}", if texts.len() == 1 { "1 thing is".to_string() } else { format!("{} things are", texts.len()) }, read_out(&texts, 4)));
            }
            if !waiting.is_empty() {
                parts.push(format!("You wait on others for {}. {}", if waiting.len() == 1 { "1 thing".to_string() } else { format!("{} things", waiting.len()) }, read_out(&waiting, 3)));
            }
            let first = blocked.iter().find_map(|l| l.show.clone());
            return Some(Answer { say: brief::spoken(&parts.join(" ")), actions: first.map(|show| Action::Show { show }).into_iter().collect() });
        }

        // Status: only the plain question, not "the status of the migration".
        let status = low.starts_with("status") || low.ends_with("status") || any_in(&low, &["status update", "status report"]);
        let about_one = any_in(&low, &["status of", "status on", "status for", "status with", "status page"]);
        if words <= 6 && !about_one && (status || any_in(&low, &["how are things", "how is it going", "how's it going", "what's going on", "what is going on", "what are my sessions doing", "what are the sessions doing", "where are we", "where do we stand", "overview"])) {
            return say(self.status(&snap));
        }
        None
    }

    /// The sessions and what is open, in a few sentences.
    fn status(&self, snap: &Snapshot) -> String {
        let of = |state: SessionState| snap.sessions.iter().filter(|s| s.state == state).count();
        let (working, needs, idle) = (of(SessionState::Working), of(SessionState::NeedsYou), of(SessionState::Idle));
        let running = working + needs + idle;
        let mut out = Vec::new();
        if running == 0 {
            out.push("No session is running.".to_string());
        } else {
            let mut parts = Vec::new();
            if working > 0 {
                parts.push(format!("{working} working"));
            }
            if needs > 0 {
                parts.push(if needs == 1 { "1 needs you".to_string() } else { format!("{needs} need you") });
            }
            if idle > 0 {
                parts.push(format!("{idle} idle"));
            }
            out.push(format!("{}: {}.", if running == 1 { "1 session".to_string() } else { format!("{running} sessions") }, parts.join(", ")));
        }
        if snap.approvals > 0 {
            out.push(format!("{} for your OK.", if snap.approvals == 1 { "1 command waits".to_string() } else { format!("{} commands wait", snap.approvals) }));
        }
        let open = followups::open(&self.data);
        let fixed = open.iter().filter(|f| f.state == FollowState::Fixed).count();
        if !open.is_empty() {
            let ready = if fixed == 0 { String::new() } else { format!(", {fixed} ready for you to look again") };
            out.push(format!("{} open{ready}.", if open.len() == 1 { "1 follow-up is".to_string() } else { format!("{} follow-ups are", open.len()) }));
        }
        let waiting = messages::waiting(&self.data).len();
        if waiting > 0 {
            out.push(format!("{} for your OK.", if waiting == 1 { "1 message waits".to_string() } else { format!("{waiting} messages wait") }));
        }
        if out.len() == 1 && running > 0 && needs == 0 {
            out.push("Nothing needs you right now.".to_string());
        }
        out.join(" ")
    }

    /// How many things wait for the person (the rail's badge).
    pub fn needs_you(&self, snap: &Snapshot) -> usize {
        let sessions = snap.sessions.iter().filter(|s| s.state == SessionState::NeedsYou).count();
        let fixed = self.data.followups.iter().filter(|f| f.state == FollowState::Fixed).count();
        sessions + snap.approvals + fixed + messages::waiting(&self.data).len()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Blocker, OutMessage, PrInfo, ReviewerInfo, SendPolicy, SessionInfo};
    use super::*;

    const T0: u64 = 1_800_000_000;

    fn settings() -> Settings {
        Settings {
            enabled: true,
            name: "AI Robin".into(),
            owner: "Robin".into(),
            // No daily brief unless a test asks for one: its time depends on the clock.
            brief_at: None,
            people: vec![
                Person { name: "Sam".into(), github: "sam-dev".into(), urgent: true, contact: Some("u-sam".into()), ..Person::default() },
                Person { name: "Alex".into(), github: "alex".into(), ..Person::default() },
            ],
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
        SessionInfo { id: id.into(), title: format!("Work {id}"), project_id: "p1".into(), project: "app".into(), state, pr, ..SessionInfo::default() }
    }

    fn snap(at: u64, sessions: Vec<SessionInfo>) -> Snapshot {
        Snapshot { at, me: Some("robin".into()), sessions, mac_online: true, ..Snapshot::default() }
    }

    fn tells(actions: &[Action]) -> Vec<(&str, &str)> {
        actions.iter().filter_map(|a| if let Action::TellSession { session, text } = a { Some((session.as_str(), text.as_str())) } else { None }).collect()
    }

    fn notifies(actions: &[Action]) -> Vec<&str> {
        actions.iter().filter_map(|a| if let Action::Notify { title, .. } = a { Some(title.as_str()) } else { None }).collect()
    }

    fn given(threads: Vec<Blocker>, head: &str) -> GivenReview {
        GivenReview {
            repo: "acme/app".into(),
            number: 1111,
            title: "Token refresh".into(),
            url: "https://github.com/acme/app/pull/1111".into(),
            author: "sam-dev".into(),
            head: head.into(),
            reviewed_head: "aaa".into(),
            my_state: "CHANGES_REQUESTED".into(),
            threads,
            state: "open".into(),
            updated: 0,
        }
    }

    fn blocker(id: &str, resolved: bool) -> Blocker {
        Blocker { thread: id.into(), text: format!("fix {id}"), path: "a.rs".into(), resolved }
    }

    /// v2 takes over once: the built-ins' state comes from `seen` then; a v1 round after it (the
    /// engine back at v1) makes the next take-over start from `seen` again. A rule that is v2's
    /// or nobody's keeps its bookkeeping and says nothing.
    #[test]
    fn the_hand_over_seeds_once_and_v1_takes_it_back() {
        use crate::manager::handover::{Mode, Owned, Owner};
        let s = settings();
        let mut h = Harness::default();
        h.data.seen.insert("needs|s1".into(), "1".into());
        let seeded = h.take_over(T0).expect("the first take-over seeds");
        assert!(seeded.asked("s1") && h.data.v2_since == Some(T0));
        assert!(h.take_over(T0 + 60).is_none(), "after that, the built-ins keep theirs");
        // The next launch, before its take-over: still v2's, and nothing is sent from v1's stale
        // outbox.
        let before = Owned::of(Mode { engine_v2: true, manager_on: true, taken_over: false, v2_before: true }, &|_| true);
        assert_eq!(before.send, Owner::Nobody);
        h.data.outbox.push(OutMessage { id: "o1".into(), to: "Sam".into(), body: "Sent by v2 already.".into(), tier: Tier::Routine, state: MsgState::Approved, created: T0, updated: T0, ..OutMessage::default() });
        h.step_with(&s, &snap(T0, vec![session("s1", SessionState::Idle, None)]), T0, &before);
        let round = h.step_with(&s, &snap(T0 + 60, vec![session("s1", SessionState::Idle, None)]), T0 + 60, &before);
        assert!(!round.actions.iter().any(|a| matches!(a, Action::Send { .. })), "{:?}", round.actions);
        assert!(h.take_over(T0 + 120).is_none(), "the built-ins' state is kept across launches");
        h.data.outbox.clear();
        h.prev = None;
        let v2 = Owned::of(Mode { engine_v2: true, manager_on: true, taken_over: true, v2_before: true }, &|id| id != "needs-you");
        h.step_with(&s, &snap(T0, vec![session("s1", SessionState::Working, None)]), T0, &v2);
        let round = h.step_with(&s, &snap(T0 + 60, vec![session("s1", SessionState::NeedsYou, None)]), T0 + 60, &v2);
        assert!(round.actions.is_empty() && round.events.is_empty(), "needs-you switched off in v2 is nobody's");
        assert_eq!(h.data.seen.get("needs|s1").map(String::as_str), Some("1"), "its bookkeeping goes on");
        assert_eq!(h.data.v2_since, Some(T0));
        h.step(&s, &snap(T0 + 120, vec![session("s1", SessionState::NeedsYou, None)]), T0 + 120);
        assert_eq!(h.data.v2_since, None);
        assert!(h.take_over(T0 + 180).is_some());
    }

    /// v2's outbox: drafts and sends are its own; `data.json` keeps v1's as v1 left it, the other
    /// copy's merges into v1's; one that cannot be read sends nothing.
    #[test]
    fn v2_takes_the_outbox_over() {
        let _home = crate::testkit::home::Home::new();
        let s = settings();
        let mut h = Harness::default();
        let v1 = OutMessage { id: "o1".into(), to: "Sam".into(), body: "v1's".into(), tier: Tier::Routine, state: MsgState::Approved, created: T0, updated: T0, ..OutMessage::default() };
        h.data.outbox.push(v1.clone());
        let mut moved = v1.clone();
        moved.state = MsgState::Sent;
        h.use_v2_outbox(Ok(vec![moved.clone()]));
        let drafted = messages::draft(&mut h.data, &s, "Alex", "v2's own", "A question.", None, T0 + 10);
        h.dirty = true;
        h.save();
        assert_eq!(super::super::store::load().outbox, vec![v1.clone()], "data.json keeps v1's outbox");
        use crate::store::Store as _;
        let file = messages::Outbox::load().unwrap();
        assert_eq!(file.messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["o1", drafted.id.as_str()]);
        assert_eq!(h.v1_data().outbox, vec![v1.clone()]);
        // The other copy's message joins v1's outbox, not v2's.
        let mut theirs = Data::default();
        theirs.outbox.push(OutMessage { id: "o9".into(), to: "Sam".into(), body: "from the server".into(), created: T0, updated: T0 + 5, ..OutMessage::default() });
        assert!(h.merge_theirs(&theirs));
        assert!(h.data.outbox.iter().all(|m| m.id != "o9") && h.v1_outbox.as_ref().unwrap().iter().any(|m| m.id == "o9"));
        // An outbox that cannot be read: nothing is sent, also not v1's.
        let mut broken = Harness::default();
        broken.data.outbox.push(v1);
        broken.use_v2_outbox(Err("manager/outbox.json is damaged".into()));
        let mut actions = Vec::new();
        broken.send_ready(&s, T0 + 60, &mut actions);
        assert!(actions.is_empty() && broken.outbox_error.is_some());
    }

    #[test]
    fn nothing_happens_when_switched_off() {
        let mut h = Harness::default();
        let off = Settings { enabled: false, brief_at: Some("00:00".into()), ..settings() };
        let s = snap(T0, vec![session("s1", SessionState::NeedsYou, Some(pr(12, &[("alex", "CHANGES_REQUESTED")])))]);
        assert!(h.step(&off, &s, T0).is_empty());
        assert!(h.step(&off, &s, T0 + 600).is_empty());
        assert!(h.on_reviews(&off, &[given(vec![blocker("t1", false)], "aaa")], T0).is_empty());
        let urgent = InMessage { id: "m1".into(), from: "Sam".into(), text: "Production is down".into(), urgent: true, at: T0, ..InMessage::default() };
        assert!(h.on_inbox(&off, vec![urgent], T0).is_empty());
        assert_eq!(h.quick_answer(&off, &s, "status", T0), None);
        assert_eq!(h.quick_answer(&off, &s, "remember that the demo is in room 4", T0), None);
        assert_eq!(h.data, Data::default());
        assert!(h.prev.is_none());
        assert!(!h.dirty);
    }

    #[test]
    fn the_first_round_after_a_start_does_nothing_about_old_state() {
        let mut h = Harness::default();
        // An old follow-up that has been quiet for a week, an approved message, a brief that is due.
        h.data.followups.push(FollowUp { id: followups::given_id("acme/app", 1111), repo: "acme/app".into(), number: 1111, login: "sam-dev".into(), person: "Sam".into(), blockers: vec![blocker("t1", false)], created: T0 - 700_000, updated: T0 - 700_000, ..FollowUp::default() });
        h.data.outbox.push(OutMessage { id: "o1".into(), to: "Sam".into(), body: "The fix for #12 is pushed.\nAI Robin, on behalf of Robin".into(), tier: Tier::Routine, state: MsgState::Approved, created: T0 - 50, updated: T0 - 50, ..OutMessage::default() });
        let with_brief = Settings { brief_at: Some("00:00".into()), ..settings() };
        let mut failing = pr(12, &[("alex", "CHANGES_REQUESTED"), ("sam-dev", "COMMENTED")]);
        failing.ci = "failing".into();
        let s = snap(T0, vec![session("s1", SessionState::NeedsYou, Some(failing)), session("s2", SessionState::Idle, None)]);
        let actions = h.step(&with_brief, &s, T0);
        assert_eq!(actions, Vec::new());
        // It did look: the round is kept, the heartbeat is set, what it saw is recorded.
        assert_eq!(h.prev.as_ref().map(|p| p.at), Some(T0));
        assert_eq!(h.data.heartbeat, T0);
        assert!(h.dirty);
        assert!(h.data.followups.iter().any(|f| f.kind == FollowKind::ReviewReceived && f.login == "alex"));

        // The same state on the next round is still not news for the session or the person;
        // what was only held back (the nudge, the send, the brief) comes now.
        let actions = h.step(&with_brief, &snap(T0 + 300, s.sessions.clone()), T0 + 300);
        assert!(tells(&actions).is_empty(), "{actions:?}");
        assert!(notifies(&actions).is_empty(), "{actions:?}");
        assert!(actions.iter().any(|a| matches!(a, Action::Draft { to, .. } if to == "Sam")));
        assert!(actions.contains(&Action::Send { id: "o1".into() }));
        assert!(actions.iter().any(|a| matches!(a, Action::Say { .. })));
    }

    #[test]
    fn a_session_is_told_about_a_review_once_and_never_again() {
        let mut h = Harness::default();
        let s = settings();
        let before = snap(T0, vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "PENDING")])))]);
        assert!(h.step(&s, &before, T0).is_empty());
        let reviewed = vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "CHANGES_REQUESTED")])))];
        let actions = h.step(&s, &snap(T0 + 300, reviewed.clone()), T0 + 300);
        assert_eq!(notifies(&actions), vec!["Alex asked for changes on #12"]);
        assert_eq!(tells(&actions), vec![("s1", "AI Robin here: Alex asked for changes on PR #12. Read the review on PR #12 and address it. Tell me when it is done.")]);
        assert_eq!(actions.len(), 2);
        assert!(matches!(&actions[0], Action::Notify { session: Some(id), body, .. } if id == "s1" && body == "Change 12"));

        // Later rounds, with the session working on it and idle again: nothing more.
        for (i, state) in [SessionState::Working, SessionState::Idle, SessionState::Idle].into_iter().enumerate() {
            let mut sessions = reviewed.clone();
            sessions[0].state = state;
            let at = T0 + 600 + 60 * i as u64;
            assert!(h.step(&s, &snap(at, sessions), at).is_empty());
        }

        // A restart with the same data: still nothing, on the first round and after.
        let mut again = Harness { data: h.data.clone(), prev: None, dirty: false, ..Harness::default() };
        assert!(again.step(&s, &snap(T0 + 5000, reviewed.clone()), T0 + 5000).is_empty());
        assert!(again.step(&s, &snap(T0 + 5300, reviewed.clone()), T0 + 5300).is_empty());

        // The other copy (the server taking over), with the data merged over, says nothing either.
        let mut other = Harness::default();
        assert!(super::super::store::merge(&mut other.data, &again.data));
        assert!(other.step(&s, &snap(T0 + 5400, reviewed.clone()), T0 + 5400).is_empty());
        assert!(other.step(&s, &snap(T0 + 5700, reviewed), T0 + 5700).is_empty());
    }

    #[test]
    fn a_busy_session_is_told_when_it_is_free() {
        let mut h = Harness::default();
        let s = settings();
        let round = |state: SessionState, review: &str| vec![session("s1", state, Some(pr(12, &[("alex", review)])))];
        h.step(&s, &snap(T0, round(SessionState::Working, "PENDING")), T0);
        // The review comes in while it works: the person hears of it, the session not yet.
        let actions = h.step(&s, &snap(T0 + 300, round(SessionState::Working, "COMMENTED")), T0 + 300);
        assert_eq!(notifies(&actions), vec!["Alex left comments on #12"]);
        assert!(tells(&actions).is_empty());
        // Nor while it waits for an answer from the person, or is not running.
        let actions = h.step(&s, &snap(T0 + 600, round(SessionState::NeedsYou, "COMMENTED")), T0 + 600);
        assert!(tells(&actions).is_empty());
        assert!(h.step(&s, &snap(T0 + 700, round(SessionState::NotRunning, "COMMENTED")), T0 + 700).is_empty());

        // A restart in between loses nothing: the first round records, the next one tells.
        let mut h = Harness { data: h.data.clone(), prev: None, dirty: false, ..Harness::default() };
        assert!(h.step(&s, &snap(T0 + 900, round(SessionState::Idle, "COMMENTED")), T0 + 900).is_empty());
        let actions = h.step(&s, &snap(T0 + 960, round(SessionState::Idle, "COMMENTED")), T0 + 960);
        assert_eq!(tells(&actions), vec![("s1", "AI Robin here: Alex left comments on PR #12. Read the review on PR #12 and address it. Tell me when it is done.")]);
        assert_eq!(actions.len(), 1);
        assert!(h.step(&s, &snap(T0 + 1020, round(SessionState::Idle, "COMMENTED")), T0 + 1020).is_empty());
    }

    #[test]
    fn two_reviews_make_one_message_and_a_second_round_of_changes_is_told_again() {
        let mut h = Harness::default();
        let s = settings();
        h.step(&s, &snap(T0, vec![session("s1", SessionState::Working, Some(pr(12, &[("alex", "PENDING"), ("sam-dev", "PENDING")])))]), T0);
        let both = pr(12, &[("alex", "CHANGES_REQUESTED"), ("sam-dev", "COMMENTED")]);
        h.step(&s, &snap(T0 + 300, vec![session("s1", SessionState::Working, Some(both.clone()))]), T0 + 300);
        let actions = h.step(&s, &snap(T0 + 600, vec![session("s1", SessionState::Idle, Some(both.clone()))]), T0 + 600);
        assert_eq!(tells(&actions), vec![("s1", "AI Robin here: Alex asked for changes on PR #12, and Sam left comments on PR #12. Read the reviews on PR #12 and address them. Tell me when it is done.")]);

        // The session pushed and asked again; Alex asks for changes a second time.
        let asked_again = pr(12, &[("alex", "PENDING"), ("sam-dev", "COMMENTED")]);
        assert!(h.step(&s, &snap(T0 + 900, vec![session("s1", SessionState::Idle, Some(asked_again))]), T0 + 900).is_empty());
        let actions = h.step(&s, &snap(T0 + 1200, vec![session("s1", SessionState::Idle, Some(both))]), T0 + 1200);
        assert_eq!(tells(&actions).len(), 1);
        assert!(tells(&actions)[0].1.starts_with("AI Robin here: Alex asked for changes on PR #12. "));
    }

    #[test]
    fn switching_it_on_does_not_tell_sessions_about_reviews_that_were_there_all_along() {
        let mut h = Harness::default();
        let s = settings();
        // The app finds the pull request first and reads who reviewed it a little later.
        h.step(&s, &snap(T0, vec![session("s1", SessionState::Idle, Some(pr(12, &[])))]), T0);
        assert!(h.step(&s, &snap(T0 + 3, vec![session("s1", SessionState::Idle, Some(pr(12, &[])))]), T0 + 3).is_empty());
        let old = pr(12, &[("alex", "CHANGES_REQUESTED"), ("sam-dev", "COMMENTED")]);
        assert!(h.step(&s, &snap(T0 + 200, vec![session("s1", SessionState::Idle, Some(old.clone()))]), T0 + 200).is_empty());
        assert!(h.step(&s, &snap(T0 + 203, vec![session("s1", SessionState::Idle, Some(old))]), T0 + 203).is_empty());
        // The standing request for changes is known all the same.
        assert_eq!(followups::open(&h.data).len(), 1);
        // A review after that is told.
        let newer = pr(12, &[("alex", "CHANGES_REQUESTED"), ("sam-dev", "CHANGES_REQUESTED")]);
        let actions = h.step(&s, &snap(T0 + 400, vec![session("s1", SessionState::Idle, Some(newer))]), T0 + 400);
        assert_eq!(notifies(&actions), vec!["Sam asked for changes on #12"]);
        assert_eq!(tells(&actions).len(), 1);
    }

    #[test]
    fn an_approval_is_news_for_the_person_only() {
        let mut h = Harness::default();
        let s = settings();
        h.step(&s, &snap(T0, vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "CHANGES_REQUESTED")])))]), T0);
        let approved = vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "APPROVED")])))];
        let actions = h.step(&s, &snap(T0 + 300, approved.clone()), T0 + 300);
        assert_eq!(notifies(&actions), vec!["Alex approved #12"]);
        assert_eq!(actions.len(), 1);
        assert!(h.step(&s, &snap(T0 + 600, approved), T0 + 600).is_empty());
        // Nothing here ever approves, merges or posts.
        assert!(followups::open(&h.data).is_empty());
    }

    #[test]
    fn the_person_is_told_once_per_stop_that_a_session_needs_them() {
        let mut h = Harness::default();
        let s = settings();
        let round = |state: SessionState| {
            let mut one = session("s1", state, None);
            one.last_reply = Some("Two ways to do this.\nWhich one do you want?".into());
            vec![one]
        };
        h.step(&s, &snap(T0, round(SessionState::Working)), T0);
        let actions = h.step(&s, &snap(T0 + 60, round(SessionState::NeedsYou)), T0 + 60);
        assert_eq!(actions, vec![Action::Notify { title: "Work s1 needs you".into(), body: "Which one do you want?".into(), session: Some("s1".into()) }]);
        assert!(h.step(&s, &snap(T0 + 120, round(SessionState::NeedsYou)), T0 + 120).is_empty());
        // A restart while it still waits says nothing again.
        let mut h = Harness { data: h.data.clone(), prev: None, dirty: false, ..Harness::default() };
        assert!(h.step(&s, &snap(T0 + 180, round(SessionState::NeedsYou)), T0 + 180).is_empty());
        assert!(h.step(&s, &snap(T0 + 240, round(SessionState::NeedsYou)), T0 + 240).is_empty());
        // It goes on and stops again: that is a new stop.
        assert!(h.step(&s, &snap(T0 + 300, round(SessionState::Working)), T0 + 300).is_empty());
        assert_eq!(notifies(&h.step(&s, &snap(T0 + 360, round(SessionState::NeedsYou)), T0 + 360)), vec!["Work s1 needs you"]);
    }

    #[test]
    fn sessions_of_projects_it_does_not_manage_are_ignored() {
        let mut h = Harness::default();
        let mut s = settings();
        s.off_projects.insert("p-off".into());
        let round = |state: SessionState, review: &str| {
            let mut one = session("s1", state, Some(pr(12, &[("alex", review)])));
            one.project_id = "p-off".into();
            vec![one]
        };
        h.step(&s, &snap(T0, round(SessionState::Working, "PENDING")), T0);
        assert!(h.step(&s, &snap(T0 + 300, round(SessionState::NeedsYou, "CHANGES_REQUESTED")), T0 + 300).is_empty());
        assert!(h.step(&s, &snap(T0 + 600, round(SessionState::Idle, "CHANGES_REQUESTED")), T0 + 600).is_empty());
        assert!(h.data.followups.is_empty());
        assert!(h.prev.as_ref().is_some_and(|p| p.sessions.is_empty()));
        assert_eq!(h.quick_answer(&s, &snap(T0 + 600, round(SessionState::NeedsYou, "CHANGES_REQUESTED")), "what needs me?", T0 + 600).map(|a| a.say), Some("Nothing needs you right now.".to_string()));
    }

    #[test]
    fn failing_checks_are_left_to_the_app_and_a_finished_session_is_in_the_brief() {
        let mut h = Harness::default();
        let s = settings();
        let round = |state: SessionState, ci: &str| {
            let mut p = pr(12, &[]);
            p.ci = ci.into();
            vec![session("s1", state, Some(p))]
        };
        h.step(&s, &snap(T0, round(SessionState::Working, "passing")), T0);
        assert!(h.step(&s, &snap(T0 + 300, round(SessionState::Working, "failing")), T0 + 300).is_empty());
        let made = brief::build(&s, &h.data, &snap(T0 + 300, round(SessionState::Working, "failing")), T0 + 300);
        assert!(made.lines.iter().any(|l| l.kind == "blocked" && l.text == "Checks fail on #12 (Change 12)"));
        // It stops with nothing to ask: finished.
        assert!(h.step(&s, &snap(T0 + 600, round(SessionState::Idle, "passing")), T0 + 600).is_empty());
        let made = brief::build(&s, &h.data, &snap(T0 + 900, round(SessionState::Idle, "passing")), T0 + 900);
        assert!(made.lines.iter().any(|l| l.kind == "done" && l.text == "Work s1 finished"), "{:?}", made.lines);
    }

    #[test]
    fn a_fixed_follow_up_is_announced_once() {
        let mut h = Harness::default();
        let s = settings();
        assert!(h.on_reviews(&s, &[given(vec![blocker("t1", false), blocker("t2", false)], "aaa")], T0).is_empty());
        assert!(h.dirty);
        h.dirty = false;
        assert!(h.on_reviews(&s, &[given(vec![blocker("t1", false), blocker("t2", false)], "aaa")], T0 + 300).is_empty());
        assert!(!h.dirty, "nothing changed, nothing to save");
        assert_eq!(h.needs_you(&Snapshot::default()), 0);

        let fixed = [given(vec![blocker("t1", true), blocker("t2", true)], "bbb")];
        let actions = h.on_reviews(&s, &fixed, T0 + 600);
        assert_eq!(
            actions,
            vec![
                Action::Notify { title: "#1111 is ready for you to look again".into(), body: "Sam fixed all 2 blockers on #1111: ready for you to look again.".into(), session: None },
                Action::Say { text: "Sam fixed all 2 blockers on PR 1111: ready for you to look again.".into(), show: Some(Show { view: Some("manager".into()), section: Some("followups".into()), ..Show::default() }) },
            ]
        );
        assert!(h.dirty);
        assert_eq!(h.needs_you(&Snapshot::default()), 1);
        assert!(h.on_reviews(&s, &fixed, T0 + 900).is_empty());
        // Not after a restart either.
        let mut again = Harness { data: h.data.clone(), prev: None, dirty: false, ..Harness::default() };
        assert!(again.on_reviews(&s, &fixed, T0 + 1200).is_empty());
    }

    #[test]
    fn a_quiet_follow_up_becomes_a_draft_and_never_an_outbox_entry_by_itself() {
        let mut h = Harness::default();
        let s = settings();
        h.on_reviews(&s, &[given(vec![blocker("t1", true), blocker("t2", false), blocker("t3", false)], "aaa")], T0);
        // Someone the person did not list: no message can be drafted, the person is told instead.
        let mut unknown = given(vec![blocker("t1", false)], "aaa");
        unknown.number = 1200;
        unknown.author = "stranger".into();
        h.on_reviews(&s, &[unknown], T0);
        h.step(&s, &Snapshot::default(), T0);
        assert!(h.step(&s, &Snapshot::default(), T0 + STALE_AFTER - 10).is_empty());

        let actions = h.step(&s, &Snapshot::default(), T0 + STALE_AFTER);
        assert_eq!(
            actions,
            vec![
                Action::Draft { to: "Sam".into(), body: "#1111: 2 of the 3 points from my review are still open. Could you take a look?".into(), why: "Nothing happened on #1111 for 2 days.".into(), about: Some("fu-acme/app#1111".into()) },
                Action::Notify { title: "Quiet for 2 days".into(), body: "Stranger has 1 open blocker on #1200.".into(), session: None },
            ]
        );
        // The harness itself put nothing in the outbox: that is `messages::draft`'s to do.
        assert!(h.data.outbox.is_empty());
        // Not again the next round, nor the next day; after two more days, once more.
        assert!(h.step(&s, &Snapshot::default(), T0 + STALE_AFTER + 60).is_empty());
        assert!(h.step(&s, &Snapshot::default(), T0 + STALE_AFTER + 86_400).is_empty());
        assert_eq!(h.step(&s, &Snapshot::default(), T0 + 2 * STALE_AFTER).len(), 2);

        // The draft, carried out the way the app does it, waits for the person and is signed.
        if let Action::Draft { to, body, why, about } = &actions[0] {
            let m = messages::draft(&mut h.data, &s, to, body, why, about.as_deref(), T0 + STALE_AFTER);
            assert_eq!(m.state, MsgState::Draft);
            assert!(m.body.ends_with("AI Robin, on behalf of Robin"), "{}", m.body);
            assert!(h.step(&s, &Snapshot::default(), T0 + STALE_AFTER + 120).is_empty(), "a draft is not sent");
        }
    }

    #[test]
    fn nudge_texts() {
        let f = |resolved: usize, total: usize| FollowUp { number: 7, blockers: (0..total).map(|i| blocker(&format!("t{i}"), i < resolved)).collect(), ..FollowUp::default() };
        assert_eq!(nudge_text(&f(0, 0)), "#7: the changes I asked for in my review are still open. Could you take a look?");
        assert_eq!(nudge_text(&f(0, 1)), "#7: the point from my review is still open. Could you take a look?");
        assert_eq!(nudge_text(&f(4, 5)), "#7: 1 of the 5 points from my review is still open. Could you take a look?");
        assert_eq!(nudge_text(&f(3, 5)), "#7: 2 of the 5 points from my review are still open. Could you take a look?");
        // A nudge is never something only the person may say.
        assert_ne!(messages::tier(&nudge_text(&f(3, 5))), Tier::Never);
    }

    #[test]
    fn an_allowed_message_is_sent_once() {
        let mut h = Harness::default();
        let s = Settings { send: SendPolicy::RoutineAuto, ..settings() };
        h.step(&s, &Snapshot::default(), T0);
        let routine = messages::draft(&mut h.data, &s, "Sam", "The fix for #12 is pushed.", "Sam asked for it.", None, T0);
        assert_eq!(routine.state, MsgState::Approved, "a routine ping goes by itself when the person allowed that");
        let waits = messages::draft(&mut h.data, &s, "Alex", "I think we should drop the old API tomorrow.", "An opinion.", None, T0);
        assert_eq!(waits.state, MsgState::Draft);

        assert_eq!(h.step(&s, &Snapshot::default(), T0 + 60), vec![Action::Send { id: routine.id.clone() }]);
        // While the send is on its way it is not asked for again.
        assert!(h.step(&s, &Snapshot::default(), T0 + 120).is_empty());
        messages::mark_sent(&mut h.data, &routine.id, T0 + 130);
        assert!(h.step(&s, &Snapshot::default(), T0 + 180).is_empty());
        assert!(h.step(&s, &Snapshot::default(), T0 + 180 + RESEND_AFTER).is_empty());

        // The person allows the other one: it goes, once. Rejected ones never do.
        messages::decide(&mut h.data, &waits.id, true, None, T0 + 2000);
        assert_eq!(h.step(&s, &Snapshot::default(), T0 + 2060), vec![Action::Send { id: waits.id.clone() }]);
        messages::mark_failed(&mut h.data, &waits.id, "The messenger said no", T0 + 2070);
        assert!(h.step(&s, &Snapshot::default(), T0 + 2120).is_empty(), "a failed send waits for the person");
        assert_eq!(h.needs_you(&Snapshot::default()), 1);
    }

    #[test]
    fn a_send_that_never_reported_back_is_asked_for_again_later() {
        let mut h = Harness::default();
        let s = settings();
        h.data.outbox.push(OutMessage { id: "o1".into(), to: "Sam".into(), body: "The fix for #12 is pushed.".into(), tier: Tier::Routine, state: MsgState::Approved, created: T0, updated: T0, ..OutMessage::default() });
        h.data.outbox.push(OutMessage { id: "o2".into(), to: "Sam".into(), body: "You can merge it.".into(), tier: Tier::Never, state: MsgState::Approved, created: T0, updated: T0, ..OutMessage::default() });
        h.step(&s, &Snapshot::default(), T0);
        assert_eq!(h.step(&s, &Snapshot::default(), T0 + 60), vec![Action::Send { id: "o1".into() }]);
        assert!(h.step(&s, &Snapshot::default(), T0 + 600).is_empty());
        assert_eq!(h.step(&s, &Snapshot::default(), T0 + 60 + RESEND_AFTER), vec![Action::Send { id: "o1".into() }]);
    }

    #[test]
    fn the_daily_brief_is_said_once_a_day() {
        let mut h = Harness::default();
        // Due from this very minute on.
        let s = Settings { brief_at: Some(crate::day::clock(T0 as i64)), ..settings() };
        let round = |at: u64| snap(at, vec![session("s1", SessionState::NeedsYou, None)]);
        assert!(h.step(&s, &round(T0), T0).is_empty(), "never on the first round");
        let actions = h.step(&s, &round(T0 + 20), T0 + 20);
        assert_eq!(actions.len(), 1);
        let Action::Say { text, show } = &actions[0] else { panic!("not said: {actions:?}") };
        assert!(text.starts_with("1 session needs you: Work s1."), "{text}");
        assert_eq!(show.as_ref().and_then(|s| s.view.as_deref()), Some("manager"));
        assert_eq!(h.data.brief.as_ref().map(|b| b.day.clone()), Some(local_day(T0 + 20)));
        assert!(h.step(&s, &round(T0 + 40), T0 + 40).is_empty());
        // Not after a restart on the same day.
        let mut again = Harness { data: h.data.clone(), prev: None, dirty: false, ..Harness::default() };
        again.step(&s, &round(T0 + 60), T0 + 60);
        assert!(again.step(&s, &round(T0 + 80), T0 + 80).is_empty());
        // Before its time, nothing.
        let mut early = Harness::default();
        let later = Settings { brief_at: Some("23:59".into()), ..settings() };
        if local_minutes(T0) < 23 * 60 + 58 {
            early.step(&later, &round(T0), T0);
            assert!(early.step(&later, &round(T0 + 20), T0 + 20).is_empty());
        }
    }

    #[test]
    fn inbox_messages_are_kept_once_and_only_an_urgent_one_from_the_right_person_alarms() {
        let mut h = Harness::default();
        let s = settings();
        let msg = |id: &str, from: &str, urgent: bool| InMessage { id: id.into(), chat: "c1".into(), from: from.into(), text: "The release is broken,\nplease look".into(), at: T0, urgent, handled: false };

        // Plain messages, and an urgent one from someone who may not alarm: only kept.
        assert!(h.on_inbox(&s, vec![msg("m1", "Sam", false), msg("m2", "Alex", true), msg("m3", "Stranger", true), msg("", "Sam", true)], T0).is_empty());
        assert_eq!(h.data.inbox.len(), 3);
        assert!(h.data.outbox.is_empty());
        assert!(h.dirty);

        // Urgent, from Sam (by how the messenger knows them): the person is told at once, a holding reply is drafted.
        let actions = h.on_inbox(&s, vec![msg("m4", "u-sam", true), msg("m1", "Sam", false)], T0 + 60);
        assert_eq!(actions, vec![Action::Notify { title: "Urgent from Sam".into(), body: "The release is broken,".into(), session: None }]);
        assert_eq!(h.data.inbox.len(), 4);
        assert_eq!(h.data.outbox.len(), 1);
        let hold = h.data.outbox[0].clone();
        assert!(hold.urgent);
        assert_eq!(hold.to, "Sam");
        assert_eq!(hold.about.as_deref(), Some("m4"));
        assert_eq!(hold.state, MsgState::Draft, "it waits for the person: it is not a routine ping");
        assert!(hold.body.contains("automatic reply from AI Robin"));
        assert!(hold.body.ends_with("AI Robin, on behalf of Robin"));

        // The same message again: nothing. Another urgent one: told, but not a second reply.
        assert!(h.on_inbox(&s, vec![msg("m4", "u-sam", true)], T0 + 120).is_empty());
        assert_eq!(notifies(&h.on_inbox(&s, vec![msg("m5", "sam-dev", true)], T0 + 180)), vec!["Urgent from Sam"]);
        assert_eq!(h.data.outbox.len(), 1);
        // And the rounds send nothing of it while the person has not said so.
        h.step(&s, &Snapshot::default(), T0 + 200);
        assert!(h.step(&s, &Snapshot::default(), T0 + 260).is_empty());

        // An urgent message the person has already read in their messenger is no alarm.
        let mut read = msg("c1|m6", "Sam", true);
        read.handled = true;
        assert!(h.on_inbox(&s, vec![read], T0 + 300).is_empty());
        assert_eq!(h.data.outbox.len(), 1);
        // One that was kept unread is marked once the person has read it.
        h.dirty = false;
        let mut read = msg("m1", "Sam", false);
        read.handled = true;
        assert!(h.on_inbox(&s, vec![read], T0 + 360).is_empty());
        assert!(h.data.inbox.iter().find(|m| m.id == "m1").is_some_and(|m| m.handled));
        assert!(h.dirty);
        assert_eq!(h.data.inbox.len(), 6);
    }

    #[test]
    fn scheduled_checks_come_due_a_few_at_a_time_and_one_offs_are_dropped_for_good() {
        let mut h = Harness::default();
        let s = settings();
        let check = |id: &str, every_secs: u64, last: u64, once: bool| Check { id: id.into(), what: format!("see whether {id} is merged"), every_secs, last, once };
        h.data.checks = vec![check("hourly", 3600, T0, false), check("once", 1800, T0, true), check("daily", 86_400, T0 - 86_000, false), check("fast", 0, T0, false)];
        assert!(h.due_checks(&Settings { enabled: false, ..settings() }, T0 + 99_999).is_empty());
        assert!(h.due_checks(&s, T0 + 30).is_empty());
        assert!(!h.dirty);

        // Never more often than once a minute, whatever the check says.
        assert_eq!(h.due_checks(&s, T0 + 60).iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["fast"]);
        assert!(h.dirty);
        assert!(h.due_checks(&s, T0 + 90).is_empty());

        // Three are due: two now, the one that waited longest first; the third on the next call.
        let now = T0 + 3600;
        assert_eq!(h.due_checks(&s, now).iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["daily", "hourly"]);
        assert_eq!(h.due_checks(&s, now + 3).iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["once", "fast"]);
        assert!(h.due_checks(&s, now + 6).is_empty());

        // The one-off is gone, also for the other copy: a sync does not bring it back.
        assert!(!h.data.checks.iter().any(|c| c.id == "once"));
        let mut other = Data { checks: vec![check("once", 1800, T0, true)], ..Data::default() };
        super::super::store::merge(&mut other, &h.data);
        assert!(!other.checks.iter().any(|c| c.id == "once"));
        // The repeating ones wait another interval.
        assert!(h.due_checks(&s, now + 3599).iter().all(|c| c.id == "fast"));
        assert!(h.due_checks(&s, now + 3600).iter().any(|c| c.id == "hourly"));
    }

    fn busy_day() -> (Harness, Settings, Snapshot) {
        let mut h = Harness::default();
        let s = settings();
        let mut needs = session("s1", SessionState::NeedsYou, None);
        needs.last_reply = Some("Which database should I use?".into());
        let mut failing = pr(13, &[]);
        failing.ci = "failing".into();
        let sessions = vec![needs, session("s2", SessionState::Working, Some(failing)), session("s3", SessionState::Idle, Some(pr(12, &[("alex", "CHANGES_REQUESTED")])))];
        h.on_reviews(&s, &[given(vec![blocker("t1", true), blocker("t2", false)], "aaa")], T0);
        let sn = snap(T0, sessions);
        h.step(&s, &sn, T0);
        h.dirty = false;
        (h, s, sn)
    }

    #[test]
    fn plain_questions_are_answered_without_the_brain() {
        let (mut h, s, sn) = busy_day();
        let mut ask = |q: &str| h.quick_answer(&s, &sn, q, T0 + 60);

        let status = ask("Hey Robin, status?").expect("status");
        assert_eq!(status.say, "3 sessions: 1 working, 1 needs you, 1 idle. 2 follow-ups are open.");
        assert!(status.actions.is_empty());
        assert_eq!(ask("how are things").map(|a| a.say), Some(status.say.clone()));

        let needs = ask("What needs me?").expect("needs");
        assert_eq!(needs.say, "2 things need you. Work s1 needs you: Which database should I use? Alex asked for changes on PR 12 (Change 12).");
        assert_eq!(needs.actions, vec![Action::Show { show: Show { session: Some("s1".into()), section: Some("reply".into()), ..Show::default() } }]);

        let blocked = ask("what is blocked?").expect("blocked");
        assert_eq!(blocked.say, "1 thing is blocked. Checks fail on PR 13 (Change 13). You wait on others for 1 thing. Sam fixed 1 of 2 blockers on PR 1111.");
        assert_eq!(blocked.actions, vec![Action::Show { show: Show { session: Some("s2".into()), tab: Some(PanelTab::PullRequest), section: Some("checks".into()), ..Show::default() } }]);

        let follow = ask("any follow-ups?").expect("follow-ups");
        assert_eq!(follow.say, "2 follow-ups are open. Sam fixed 1 of 2 blockers on PR 1111. Alex asked for changes on your PR 12.");
        assert_eq!(ask("show me my blockers").map(|a| a.say), Some(follow.say.clone()));

        // Anything that needs thinking goes to the brain.
        for q in [
            "",
            "   ",
            "Why does the relay drop the first frame?",
            "what is the status of the migration in the billing service?",
            "update the status page copy",
            "start a session for the login bug",
            "forget it",
            "remember that",
            "remember when we talked about the relay?",
            "close #12",
            "follow up with Sam on the staging database",
            "is the quiz import blocked by the schema change or by something else entirely?",
        ] {
            assert_eq!(ask(q), None, "{q:?}");
        }
    }

    #[test]
    fn todays_update_is_the_brief_and_a_walk() {
        let (mut h, s, sn) = busy_day();
        for q in ["Give me today's update", "catch me up please", "AI Robin, brief me", "what’s new?"] {
            let answer = h.quick_answer(&s, &sn, q, T0 + 60).unwrap_or_else(|| panic!("no answer to {q:?}"));
            assert!(answer.say.starts_with("1 session needs you: Work s1."), "{}", answer.say);
            let [Action::Tour { stops }] = answer.actions.as_slice() else { panic!("no walk: {:?}", answer.actions) };
            assert_eq!(stops.first().and_then(|s| s.show.session.as_deref()), Some("s1"));
            assert_eq!(stops.last().and_then(|s| s.show.view.as_deref()), Some("manager"));
            assert!(stops.len() <= 8);
        }
        assert_eq!(h.data.brief.as_ref().map(|b| b.made), Some(T0 + 60));
        assert!(h.dirty);
        // On a day with nothing, there is a brief but no walk.
        let mut quiet = Harness::default();
        let answer = quiet.quick_answer(&s, &Snapshot::default(), "today's update", T0).expect("a brief");
        assert!(answer.say.starts_with("Nothing needs you right now."));
        assert!(answer.actions.is_empty());
    }

    #[test]
    fn follow_ups_are_closed_by_number() {
        let (mut h, s, sn) = busy_day();
        assert_eq!(h.quick_answer(&s, &sn, "done with #1111", T0 + 60).map(|a| a.say), Some("Done. The follow-up on PR 1111 is closed: Token refresh.".to_string()));
        assert!(h.dirty);
        assert!(!followups::open(&h.data).iter().any(|f| f.number == 1111));
        assert_eq!(h.quick_answer(&s, &sn, "Done with #1111.", T0 + 70).map(|a| a.say), Some("I have no open follow-up on PR 1111.".to_string()));
        assert_eq!(h.quick_answer(&s, &sn, "stop tracking PR 12", T0 + 80).map(|a| a.say), Some("I no longer follow PR 12.".to_string()));
        assert_eq!(h.quick_answer(&s, &sn, "drop #12", T0 + 85).map(|a| a.say), Some("I have no open follow-up on PR 12.".to_string()));
        assert_eq!(h.data.followups.iter().find(|f| f.number == 12).map(|f| f.state), Some(FollowState::Dropped));
        assert!(followups::open(&h.data).is_empty());
        assert_eq!(h.quick_answer(&s, &sn, "follow-ups?", T0 + 90).map(|a| a.say), Some("No follow-ups are open.".to_string()));
    }

    #[test]
    fn remember_forget_and_recall() {
        let mut h = Harness::default();
        let s = settings();
        let sn = Snapshot::default();
        let said = h.quick_answer(&s, &sn, "Hey Robin, remember that iOS builds are Mira's", T0).expect("remembered");
        assert_eq!(said.say, "I will remember that: iOS builds are Mira's.");
        assert_eq!(h.quick_answer(&s, &sn, "forget that iOS builds are Mira's", T0).map(|a| a.say), Some("Forgotten: iOS builds are Mira's.".to_string()));
        let said = h.quick_answer(&s, &sn, "please remember that Mira owns the billing export", T0).expect("remembered");
        assert_eq!(said.say, "I will remember that: Mira owns the billing export.");
        assert!(h.dirty);
        h.quick_answer(&s, &sn, "Remember: the staging database is reset every Sunday.", T0 + 1);
        assert_eq!(memory::all(&h.data).len(), 2);
        assert!(memory::all(&h.data).iter().any(|n| n.text.starts_with("Mira owns the billing export") && n.source == "you"));

        let recalled = h.quick_answer(&s, &sn, "what do you remember about billing?", T0 + 2).expect("recalled");
        assert_eq!(recalled.say, "About billing I remember: Mira owns the billing export.");
        let all = h.quick_answer(&s, &sn, "What do you remember?", T0 + 2).expect("everything");
        assert!(all.say.starts_with("I remember 2 things. "), "{}", all.say);
        assert_eq!(h.quick_answer(&s, &sn, "what do you remember about dragons", T0 + 2).map(|a| a.say), Some("I remember nothing about dragons.".to_string()));

        let forgot = h.quick_answer(&s, &sn, "forget the staging database", T0 + 3).expect("forgot");
        assert!(forgot.say.starts_with("Forgotten: the staging database is reset every Sunday"), "{}", forgot.say);
        assert_eq!(memory::all(&h.data).len(), 1);
        assert_eq!(h.quick_answer(&s, &sn, "forget the dragons", T0 + 4).map(|a| a.say), Some("I am not sure which note you mean, so I forgot nothing.".to_string()));
        assert_eq!(memory::all(&h.data).len(), 1);
    }

    #[test]
    fn the_badge_counts_what_waits_for_the_person() {
        let (mut h, s, mut sn) = busy_day();
        assert_eq!(h.needs_you(&sn), 1);
        sn.approvals = 2;
        assert_eq!(h.needs_you(&sn), 3);
        h.on_reviews(&s, &[given(vec![blocker("t1", true), blocker("t2", true)], "bbb")], T0 + 60);
        assert_eq!(h.needs_you(&sn), 4);
        messages::draft(&mut h.data, &s, "Alex", "Can you look at the relay with me?", "A question.", None, T0 + 60);
        assert_eq!(h.needs_you(&sn), 5);
    }

    #[test]
    fn the_data_is_only_marked_for_saving_when_it_changed() {
        let mut h = Harness::default();
        let s = settings();
        let at = T0 - T0 % HEARTBEAT_EVERY;
        let sn = snap(at, vec![session("s1", SessionState::Idle, None)]);
        h.step(&s, &sn, at);
        assert!(h.dirty);
        h.dirty = false;
        h.step(&s, &sn, at + 2);
        assert!(!h.dirty, "nothing changed and the heartbeat was just written");
        assert_eq!(h.data.heartbeat, at + 2);
        h.step(&s, &sn, at + HEARTBEAT_EVERY);
        assert!(h.dirty, "the heartbeat is written now and then");
        assert_eq!(h.prev.as_ref().map(|p| p.at), Some(at));
    }

    #[test]
    fn questions_are_stripped_of_greetings_and_the_name() {
        let s = settings();
        assert_eq!(plain_question(&s, "Hey AI Robin, what's blocked?"), ("what's blocked?".to_string(), "what's blocked".to_string()));
        assert_eq!(plain_question(&s, "  Robin  "), ("Robin".to_string(), "robin".to_string()));
        assert_eq!(plain_question(&s, "ok, remember that Ärger is German"), ("remember that Ärger is German".to_string(), "remember that ärger is german".to_string()));
        assert_eq!(number_in("done with pr 1111"), Some(1111));
        assert_eq!(number_in("close #12 please"), Some(12));
        assert_eq!(number_in("close the door"), None);
    }
}
