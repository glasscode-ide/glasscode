//! Review follow-ups: what the person asked of a teammate in a review, kept until it is done;
//! and reviews teammates left on the person's own pull requests.
//!
//! The rules run on a [`Book`]: the follow-ups and what was told of them, kept in the work ledger
//! (`WorkLedger::follow_ups`, `follow_seen`, M56.2); [`round`] and [`given`] are the whole round.
//!
//! What was already told is kept in the book's `seen` (keys are never removed, values change):
//! - `pr|<repo>#<n>` → when the manager first saw this pull request in a session,
//! - `prr|<repo>#<n>` → "1" once its reviewers were seen (they are read after the pull request),
//! - `review|<repo>#<n>|<login>` → the reviewer's state last seen,
//! - `ci|<repo>#<n>` → "failing" or "passing", as last seen,
//! - `fixed|<follow-up id>` → the head commit a "fixed" was told for.

use std::collections::BTreeMap;

use super::gh::GivenReview;
use super::{Action, Event, FollowKind, FollowState, FollowUp, PanelTab, Person, PrInfo, SessionInfo, Settings, Show, Snapshot, brief};

/// The follow-ups and the keys of what was told of them, wherever they are kept.
pub struct Book<'a> {
    pub followups: &'a mut Vec<FollowUp>,
    pub seen: &'a mut BTreeMap<String, String>,
}

/// A follow-up nobody touched for this long is worth a nudge.
pub const STALE_AFTER: u64 = 48 * 3600;

/// Why a follow-up on a review given was closed by itself (its `note`).
pub const NOTE_APPROVED: &str = "You approved it";
pub const NOTE_MERGED: &str = "Merged";
pub const NOTE_CLOSED: &str = "Closed without merging";
/// Where a review received stands (its `note`).
pub const NOTE_CHANGES: &str = "Asked for changes";
pub const NOTE_COMMENTS: &str = "Left comments";
pub const NOTE_AGAIN: &str = "Asked to look again";

/// A pull request has to be known this long before failing checks on it are news: they
/// arrive a little after the pull request itself.
const SETTLE_SECS: u64 = 90;
/// Who reviewed a pull request is read some minutes after the pull request is found, and
/// until then it looks as if nobody had. So the first reviewers seen on a pull request are
/// only news when it has been known for this long; from then on every change is.
const REVIEWS_SETTLE_SECS: u64 = 600;

/// The id of the follow-up on the review the person gave on a pull request.
pub fn given_id(repo: &str, number: u64) -> String {
    format!("fu-{repo}#{number}")
}

/// The id of the follow-up on a review `login` left on the person's pull request.
pub fn received_id(repo: &str, number: u64, login: &str) -> String {
    format!("rv-{repo}#{number}@{}", login.to_lowercase())
}

fn pr_key(repo: &str, number: u64) -> String {
    format!("{repo}#{number}")
}

/// Why a follow-up on a review received was let go by itself (its `note`).
pub const NOTE_NOT_YOURS: &str = "Not your pull request";

/// Whether a pull request is the person's own: nobody is known to have opened it yet, or
/// they did. One a teammate opened is not, whatever session sits on its branch.
pub fn is_own(pr: &PrInfo, me: &str) -> bool {
    let author = pr.author.trim();
    author.is_empty() || author.eq_ignore_ascii_case(me.trim())
}

/// Accounts that are not people: their reviews are not news for a session.
fn is_bot(login: &str) -> bool {
    let l = login.to_lowercase();
    l.ends_with("[bot]") || l.ends_with("-bot") || l.starts_with("app/") || l.contains("copilot") || l == "github-actions" || l == "dependabot"
}

/// Whether a review state is a review someone actually left.
pub fn is_real_review(state: &str) -> bool {
    matches!(state, "APPROVED" | "CHANGES_REQUESTED" | "COMMENTED")
}

/// What a review given should be now, going by GitHub alone, and why when it is done.
fn target(review: &GivenReview) -> (FollowState, &'static str) {
    if review.state == "merged" {
        return (FollowState::Done, NOTE_MERGED);
    }
    if review.state == "closed" {
        return (FollowState::Done, NOTE_CLOSED);
    }
    if review.my_state == "APPROVED" {
        return (FollowState::Done, NOTE_APPROVED);
    }
    let total = review.threads.len();
    let open = review.threads.iter().filter(|t| !t.resolved).count();
    let pushed = !review.reviewed_head.is_empty() && !review.head.is_empty() && review.head != review.reviewed_head;
    if open == 0 && (pushed || total > 0) {
        return (FollowState::Fixed, "");
    }
    (FollowState::Open, "")
}

/// Whether there is something to follow at all: an open pull request the person asked for
/// changes on, or left threads on that are not all resolved.
fn worth_following(review: &GivenReview) -> bool {
    review.state == "open" && review.my_state != "APPROVED" && (review.my_state == "CHANGES_REQUESTED" || review.threads.iter().any(|t| !t.resolved))
}

/// Bring the "blockers given" follow-ups in line with GitHub: new ones for new reviews, blockers
/// ticked off as their threads are resolved, `Fixed` when the author pushed and nothing is
/// open, `Done` when the pull request was merged or closed. Returns what changed.
pub fn from_reviews(book: &mut Book, settings: &Settings, reviews: &[GivenReview], now: u64) -> Vec<Event> {
    let mut events = Vec::new();
    for review in reviews {
        if review.repo.is_empty() || review.number == 0 {
            continue;
        }
        let id = given_id(&review.repo, review.number);
        let (state, why) = target(review);
        let person = settings.person(&review.author).map(|p| p.name.clone()).unwrap_or_default();
        let fixed_key = format!("fixed|{id}");
        let fixed_mark = if review.head.is_empty() { "resolved".to_string() } else { review.head.clone() };

        let Some(at) = book.followups.iter().position(|f| f.id == id) else {
            if !worth_following(review) {
                continue;
            }
            // New to the manager: recorded as it stands. One that is already fixed shows up
            // as such, but is not announced, so switching the manager on does not ring bells
            // for things that were fixed long ago.
            if state == FollowState::Fixed {
                book.seen.insert(fixed_key, fixed_mark);
            }
            book.followups.push(FollowUp {
                id,
                kind: FollowKind::BlockersGiven,
                state,
                repo: review.repo.clone(),
                number: review.number,
                title: review.title.clone(),
                url: review.url.clone(),
                login: review.author.clone(),
                person,
                blockers: review.threads.clone(),
                head: review.reviewed_head.clone(),
                session: None,
                note: String::new(),
                created: now,
                updated: now,
                nudged: None,
            });
            continue;
        };

        let old = book.followups[at].clone();
        if old.kind != FollowKind::BlockersGiven {
            continue;
        }
        let next_state = match old.state {
            // The person said to stop following it.
            FollowState::Dropped => continue,
            FollowState::Done => {
                // Open again only for a new round: the person reviewed again since, or took
                // their approval back.
                let again = worth_following(review) && (review.reviewed_head != old.head || old.note == NOTE_APPROVED);
                if !again {
                    continue;
                }
                state
            }
            FollowState::Open | FollowState::Fixed => state,
        };
        let mut next = old.clone();
        next.state = next_state;
        next.title = review.title.clone();
        next.url = review.url.clone();
        next.login = review.author.clone();
        if !person.is_empty() {
            next.person = person;
        }
        next.blockers = review.threads.clone();
        next.head = review.reviewed_head.clone();
        next.note = if next_state == FollowState::Done { why.to_string() } else { String::new() };
        if next == old {
            continue;
        }
        next.updated = now;
        let turned_fixed = next.state == FollowState::Fixed && old.state != FollowState::Fixed;
        book.followups[at] = next;
        if turned_fixed && book.seen.get(&fixed_key) != Some(&fixed_mark) {
            book.seen.insert(fixed_key, fixed_mark);
            events.push(Event::FollowUpFixed { id });
        }
    }
    events
}


/// Compare two rounds on a book: a new review on the pull request of one of the person's sessions
/// (`Event::PrReviewed`, and a `ReviewReceived` follow-up), CI that started failing. `prev` is None
/// on the first round (nothing is reported then, only recorded). The follow-ups on reviews
/// received are kept in step.
pub fn snapshot_on(book: &mut Book, prev: Option<&Snapshot>, snap: &Snapshot, now: u64) -> Vec<Event> {
    let mut events = Vec::new();
    let me = snap.me.as_deref().unwrap_or_default().trim().to_lowercase();
    // Until the person's own login is known, their own reviews cannot be told from a
    // teammate's: reviews are only recorded then.
    let known = !me.is_empty();
    for session in &snap.sessions {
        if let Some((repo, number)) = &session.review_of {
            // A review session: the follow-up on that review belongs to it.
            let id = given_id(repo, *number);
            if let Some(f) = book.followups.iter_mut().find(|f| f.id == id && f.session.as_deref() != Some(session.id.as_str())) {
                f.session = Some(session.id.clone());
            }
            continue;
        }
        let Some(pr) = &session.pr else { continue };
        if pr.repo.is_empty() || pr.number == 0 {
            continue;
        }
        // A teammate's pull request in a normal session: reviews on it are not for this
        // session. Nothing is recorded, and what was followed before its author was known
        // is let go.
        if !is_own(pr, &me) {
            for f in book.followups.iter_mut().filter(|f| f.kind == FollowKind::ReviewReceived && f.is_open() && f.number == pr.number && f.repo == pr.repo) {
                f.state = FollowState::Dropped;
                f.note = NOTE_NOT_YOURS.to_string();
                f.updated = now;
            }
            continue;
        }
        let key = pr_key(&pr.repo, pr.number);
        let first_seen = book.seen.get(&format!("pr|{key}")).and_then(|t| t.parse::<u64>().ok());
        if first_seen.is_none() {
            book.seen.insert(format!("pr|{key}"), now.to_string());
        }
        let over = matches!(pr.state.as_str(), "merged" | "closed");
        // News is told only about a pull request the manager has known for a moment, and
        // never on the first round after a start.
        let age = first_seen.map(|t| now.saturating_sub(t));
        let tell = prev.is_some() && !over && age.is_some_and(|a| a >= SETTLE_SECS);
        let reviewers_key = format!("prr|{key}");
        let tell_reviews = prev.is_some() && !over && known && (age.is_some_and(|a| a >= REVIEWS_SETTLE_SECS) || book.seen.contains_key(&reviewers_key));
        if !pr.reviewers.is_empty() && !book.seen.contains_key(&reviewers_key) {
            book.seen.insert(reviewers_key, "1".into());
        }

        for reviewer in &pr.reviewers {
            let login = reviewer.login.trim();
            let lower = login.to_lowercase();
            if login.is_empty() || lower == me || is_bot(login) {
                continue;
            }
            let state = reviewer.state.trim().to_uppercase();
            let seen_key = format!("review|{key}|{lower}");
            if book.seen.get(&seen_key).map(String::as_str) == Some(state.as_str()) {
                continue;
            }
            book.seen.insert(seen_key, state.clone());
            if !known {
                continue;
            }
            if !over {
                note_review(book, session, pr, login, &state, now);
            }
            if tell_reviews && is_real_review(&state) {
                events.push(Event::PrReviewed { session: session.id.clone(), repo: pr.repo.clone(), number: pr.number, reviewer: login.to_string(), state });
            }
        }

        let ci_key = format!("ci|{key}");
        match pr.ci.as_str() {
            "failing" => {
                if book.seen.get(&ci_key).map(String::as_str) != Some("failing") {
                    book.seen.insert(ci_key, "failing".into());
                    if tell {
                        events.push(Event::CiFailed { session: session.id.clone(), repo: pr.repo.clone(), number: pr.number });
                    }
                }
            }
            // The failing streak ends when the checks pass (not while they run again).
            "passing" if book.seen.get(&ci_key).is_some_and(|v| v != "passing") => {
                book.seen.insert(ci_key, "passing".into());
            }
            _ => {}
        }

        settle_received(book, pr, over, now);
    }
    events
}

/// Keep the follow-up on a review received in line with the reviewer's new state.
fn note_review(book: &mut Book, session: &SessionInfo, pr: &PrInfo, login: &str, state: &str, now: u64) {
    let id = received_id(&pr.repo, pr.number, login);
    let at = book.followups.iter().position(|f| f.id == id);
    match state {
        "CHANGES_REQUESTED" | "COMMENTED" => {
            let note = if state == "CHANGES_REQUESTED" { NOTE_CHANGES } else { NOTE_COMMENTS };
            match at {
                Some(at) => {
                    let f = &mut book.followups[at];
                    f.state = FollowState::Open;
                    f.note = note.to_string();
                    f.title = pr.title.clone();
                    f.url = pr.url.clone();
                    f.session = Some(session.id.clone());
                    f.updated = now;
                }
                None => {
                    // Comments that left no open thread ask for nothing.
                    if state == "COMMENTED" && pr.unresolved_threads == 0 {
                        return;
                    }
                    book.followups.push(FollowUp {
                        id,
                        kind: FollowKind::ReviewReceived,
                        state: FollowState::Open,
                        repo: pr.repo.clone(),
                        number: pr.number,
                        title: pr.title.clone(),
                        url: pr.url.clone(),
                        login: login.to_string(),
                        person: String::new(),
                        blockers: Vec::new(),
                        head: String::new(),
                        session: Some(session.id.clone()),
                        note: note.to_string(),
                        created: now,
                        updated: now,
                        nudged: None,
                    });
                }
            }
        }
        "APPROVED" | "DISMISSED" => {
            if let Some(f) = at.map(|at| &mut book.followups[at]).filter(|f| f.is_open()) {
                f.state = FollowState::Done;
                f.note = if state == "APPROVED" { "Approved".to_string() } else { "Review dismissed".to_string() };
                f.updated = now;
            }
        }
        // The review was asked for again: the ball is with the reviewer.
        "PENDING" => {
            if let Some(f) = at.map(|at| &mut book.followups[at]).filter(|f| f.is_open()) {
                f.note = NOTE_AGAIN.to_string();
                f.updated = now;
            }
        }
        _ => {}
    }
}

/// Close the follow-ups on reviews received that have nothing left: the pull request is
/// merged or closed, or the comments' threads are all resolved.
fn settle_received(book: &mut Book, pr: &PrInfo, over: bool, now: u64) {
    for f in book.followups.iter_mut() {
        if f.kind != FollowKind::ReviewReceived || !f.is_open() || f.number != pr.number || f.repo != pr.repo {
            continue;
        }
        if over {
            f.state = FollowState::Done;
            f.note = if pr.state == "merged" { NOTE_MERGED.to_string() } else { NOTE_CLOSED.to_string() };
            f.updated = now;
        } else if f.note == NOTE_COMMENTS && pr.unresolved_threads == 0 {
            f.state = FollowState::Done;
            f.note = "Comments resolved".to_string();
            f.updated = now;
        }
    }
}

/// Put the teammates' names (`Settings::people`) on the follow-ups that only have a login.
pub fn fill_people(followups: &mut [FollowUp], settings: &Settings) {
    for f in followups.iter_mut() {
        if !f.person.is_empty() || f.login.is_empty() {
            continue;
        }
        if let Some(p) = settings.person(&f.login) {
            f.person = p.name.clone();
        }
    }
}

/// How the other person is called: their name when known, else their login.
pub fn who(f: &FollowUp) -> String {
    if !f.person.trim().is_empty() {
        f.person.trim().to_string()
    } else if !f.login.trim().is_empty() {
        f.login.trim().to_string()
    } else {
        "Someone".to_string()
    }
}

/// One follow-up in a short sentence without a full stop ("Sam fixed 3 of 5 blockers on #1111").
pub fn line(f: &FollowUp) -> String {
    let who = who(f);
    let n = f.number;
    match f.kind {
        FollowKind::BlockersGiven => {
            let total = f.blockers.len();
            let open = f.open_blockers();
            let word = if total == 1 { "blocker" } else { "blockers" };
            if f.state == FollowState::Fixed {
                if total == 0 {
                    format!("{who} pushed changes to #{n} after your review: ready for you to look again")
                } else if total == 1 {
                    format!("{who} fixed the blocker on #{n}: ready for you to look again")
                } else {
                    format!("{who} fixed all {total} blockers on #{n}: ready for you to look again")
                }
            } else if total == 0 {
                format!("{who} has not answered your review on #{n} yet")
            } else if open == total {
                format!("{who} has {open} open {word} on #{n}")
            } else {
                format!("{who} fixed {} of {total} blockers on #{n}", total - open)
            }
        }
        FollowKind::ReviewReceived => match f.note.as_str() {
            NOTE_COMMENTS => format!("{who} left comments on your #{n}"),
            NOTE_AGAIN => format!("{who} was asked to look at your #{n} again"),
            _ => format!("{who} asked for changes on your #{n}"),
        },
        FollowKind::Custom => {
            let note = if f.note.trim().is_empty() { f.title.trim() } else { f.note.trim() };
            if f.person.trim().is_empty() { note.to_string() } else { format!("{note} ({})", f.person.trim()) }
        }
    }
}

/// Follow-ups nobody touched for `after_secs` (and not nudged since), as `Event::FollowUpStale`.
pub fn stale(followups: &mut [FollowUp], after_secs: u64, now: u64) -> Vec<Event> {
    stale_where(followups, after_secs, now, |_| true)
}

/// [`stale`] for the follow-ups `which` takes only; the others are left as they are.
pub fn stale_where(followups: &mut [FollowUp], after_secs: u64, now: u64, which: impl Fn(&FollowUp) -> bool) -> Vec<Event> {
    let mut events = Vec::new();
    for f in followups.iter_mut() {
        // One that is fixed waits for the person, not for anyone to be reminded.
        if f.state != FollowState::Open || !which(f) {
            continue;
        }
        let last = f.updated.max(f.nudged.unwrap_or(0)).max(f.created);
        if now.saturating_sub(last) >= after_secs {
            f.nudged = Some(now);
            f.updated = now;
            events.push(Event::FollowUpStale { id: f.id.clone() });
        }
    }
    events
}

/// The ones still open, most pressing first.
pub fn open(followups: &[FollowUp]) -> Vec<&FollowUp> {
    let mut list: Vec<&FollowUp> = followups.iter().filter(|f| f.is_open()).collect();
    list.sort_by(|a, b| {
        let fixed = |f: &FollowUp| f.state != FollowState::Fixed;
        fixed(a).cmp(&fixed(b)).then(b.open_blockers().cmp(&a.open_blockers())).then(a.created.cmp(&b.created)).then(a.id.cmp(&b.id))
    });
    list
}

/// Something else to keep an eye on, in the person's words. Returns its id.
pub fn add_custom(followups: &mut Vec<FollowUp>, note: &str, person: &str, now: u64) -> String {
    let note = note.split_whitespace().collect::<Vec<_>>().join(" ");
    let person = person.trim();
    if let Some(f) = followups.iter().find(|f| f.kind == FollowKind::Custom && f.is_open() && f.note.eq_ignore_ascii_case(&note) && f.person.eq_ignore_ascii_case(person)) {
        return f.id.clone();
    }
    let mut id = format!("fu-c-{now}");
    let mut n = 1;
    while followups.iter().any(|f| f.id == id) {
        n += 1;
        id = format!("fu-c-{now}-{n}");
    }
    followups.push(FollowUp {
        id: id.clone(),
        kind: FollowKind::Custom,
        state: FollowState::Open,
        title: note.clone(),
        person: person.to_string(),
        note,
        created: now,
        updated: now,
        ..FollowUp::default()
    });
    id
}

/// The pull request number in "#1111", "1111" or "done with PR #1111".
fn number_in(which: &str) -> Option<u64> {
    if let Some(rest) = which.split('#').nth(1) {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u64>() {
            return Some(n);
        }
    }
    let words: Vec<&str> = which.split_whitespace().collect();
    match words.as_slice() {
        [one] => one.parse().ok(),
        [pr, n] if pr.eq_ignore_ascii_case("pr") => n.parse().ok(),
        _ => None,
    }
}

/// Words that say nothing about which follow-up is meant.
const FILLER: [&str; 22] = ["the", "a", "an", "with", "done", "on", "for", "about", "pr", "is", "it", "that", "one", "follow", "up", "followup", "follow-up", "review", "of", "my", "to", "from"];

/// Mark one done (or dropped). `which` is its id, or "#1111", or words from its title.
pub fn close(followups: &mut [FollowUp], which: &str, dropped: bool, now: u64) -> Option<FollowUp> {
    let which = which.trim();
    if which.is_empty() {
        return None;
    }
    let state = if dropped { FollowState::Dropped } else { FollowState::Done };
    let mut picked: Vec<usize> = Vec::new();
    if let Some(at) = followups.iter().position(|f| f.id == which) {
        if followups[at].is_open() {
            picked.push(at);
        }
    } else if let Some(number) = number_in(which) {
        // Everything open on that pull request; what the person asked of someone comes first.
        picked = (0..followups.len()).filter(|&i| followups[i].is_open() && followups[i].number == number).collect();
        picked.sort_by_key(|&i| followups[i].kind != FollowKind::BlockersGiven);
    } else {
        let lower = which.to_lowercase();
        let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric() && c != '-').filter(|w| w.len() >= 2 && !FILLER.contains(w)).collect();
        if words.is_empty() {
            return None;
        }
        let mut best: Option<(usize, usize)> = None;
        for f in open(followups) {
            let hay = format!("{} {} {} {} {}", f.title, f.note, f.person, f.login, f.repo).to_lowercase();
            let score = words.iter().filter(|w| hay.contains(**w)).count();
            // At least half of the words have to be there, or it is a guess.
            if score == 0 || score * 2 < words.len() {
                continue;
            }
            if !best.is_none_or(|(_, s)| score > s) {
                continue;
            }
            if let Some(at) = followups.iter().position(|x| x.id == f.id) {
                best = Some((at, score));
            }
        }
        picked.extend(best.map(|(at, _)| at));
    }
    for &at in &picked {
        let f = &mut followups[at];
        f.state = state;
        f.updated = now;
    }
    picked.first().map(|&at| followups[at].clone())
}

/// Where a follow-up is looked at: its session's pull request, else the manager's page.
pub fn show_follow(f: &FollowUp, snap: Option<&Snapshot>) -> Option<Show> {
    match f.session.as_deref().filter(|id| snap.is_none_or(|s| s.session(id).is_some())) {
        Some(id) => brief::show_session(id, Some(PanelTab::PullRequest), Some("reviews")),
        None => brief::show_manager("followups"),
    }
}

/// A teammate fixed what the person asked for: tell the person, once.
pub fn fixed_actions(f: &FollowUp, snap: Option<&Snapshot>) -> Vec<Action> {
    let line = brief::sentence(&line(f));
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
pub fn person_of<'a>(settings: &'a Settings, f: &FollowUp) -> Option<&'a Person> {
    [f.login.trim(), f.person.trim()].into_iter().filter(|w| !w.is_empty()).find_map(|w| settings.person(w)).filter(|p| !p.name.trim().is_empty())
}

/// What a follow-up that went quiet asks for: a draft to the teammate who has the ball (whether
/// it waits for the person or goes is the outbox's rule), else a notification.
pub fn stale_actions(settings: &Settings, f: &FollowUp) -> Vec<Action> {
    let days = STALE_AFTER / 86_400;
    match drafts_to(settings, f) {
        Some(p) => vec![Action::Draft { to: p.name.clone(), body: nudge_text(f), why: format!("Nothing happened on #{} for {days} days.", f.number), about: Some(f.id.clone()) }],
        None => vec![Action::Notify { title: format!("Quiet for {days} days"), body: brief::sentence(&line(f)), session: f.session.clone() }],
    }
}

/// The teammate a quiet follow-up's note goes to: who has the ball on a review the person gave.
fn drafts_to<'a>(settings: &'a Settings, f: &FollowUp) -> Option<&'a Person> {
    person_of(settings, f).filter(|_| f.kind == FollowKind::BlockersGiven)
}

/// What the follow-ups' events ask for: "fixed" told once, a quiet one nudged. Other events are
/// not the follow-ups' to answer.
fn act(book: &Book, settings: &Settings, snap: Option<&Snapshot>, events: &[Event]) -> Vec<Action> {
    let mut actions = Vec::new();
    for event in events {
        match event {
            Event::FollowUpFixed { id } => actions.extend(book.followups.iter().find(|f| f.id == *id).map(|f| fixed_actions(f, snap)).unwrap_or_default()),
            Event::FollowUpStale { id } => actions.extend(book.followups.iter().find(|f| f.id == *id).map(|f| stale_actions(settings, f)).unwrap_or_default()),
            _ => {}
        }
    }
    actions
}

/// v2's round of the follow-ups (M56.2, the `follow-ups` built-in) on the work ledger's book:
/// reviews received kept in step with the snapshot, the teammates' names, and, after the first
/// round of a run (`prev`), a nudge for the quiet ones. The reviews' own news (`PrReviewed`) is
/// the review feed's, and failing CI the CI fix's: neither is answered here.
pub fn round(book: &mut Book, settings: &Settings, prev: Option<&Snapshot>, snap: &Snapshot, now: u64) -> Vec<Action> {
    snapshot_on(book, prev, snap, now);
    fill_people(book.followups, settings);
    if prev.is_none() {
        return Vec::new();
    }
    let events = stale(book.followups, STALE_AFTER, now);
    act(book, settings, Some(snap), &events)
}

/// The server's round of the follow-ups while it is the manager's home (M56.2): the teammates'
/// names and a nudge for the quiet ones, with no snapshot (the server does not read the person's
/// sessions). A quiet one whose note goes to a teammate is left for the Mac, which drafts it in
/// its outbox when it is home again (the outbox and its sending are the Mac's).
pub fn away(book: &mut Book, settings: &Settings, now: u64) -> Vec<Action> {
    fill_people(book.followups, settings);
    let events = stale_where(book.followups, STALE_AFTER, now, |f| drafts_to(settings, f).is_none());
    act(book, settings, None, &events)
}

/// v2's answer to the reviews the person gave (`gh::given_reviews`): the follow-ups kept in step,
/// and "ready for you to look again", once.
pub fn given(book: &mut Book, settings: &Settings, reviews: &[GivenReview], snap: Option<&Snapshot>, now: u64) -> Vec<Action> {
    let events = from_reviews(book, settings, reviews, now);
    act(book, settings, snap, &events)
}

#[cfg(test)]
mod tests {
    use super::super::{Blocker, Person, PrInfo, ReviewerInfo, SessionInfo, SessionState};
    use super::*;
    // The follow-ups' rules run here on v2's store, the work ledger (M56.2).
    use crate::work::WorkLedger as Data;

    fn settings() -> Settings {
        Settings { enabled: true, name: "AI Robin".into(), owner: "Robin".into(), people: vec![Person { name: "Sam".into(), github: "sam-dev".into(), ..Person::default() }], ..Settings::default() }
    }

    fn blocker(id: &str, resolved: bool) -> Blocker {
        Blocker { thread: id.into(), text: format!("fix {id}"), path: "src/a.rs".into(), resolved }
    }

    fn review(threads: Vec<Blocker>) -> GivenReview {
        GivenReview {
            repo: "acme/app".into(),
            number: 1111,
            title: "Token refresh".into(),
            url: "https://github.com/acme/app/pull/1111".into(),
            author: "sam-dev".into(),
            head: "aaa".into(),
            reviewed_head: "aaa".into(),
            my_state: "CHANGES_REQUESTED".into(),
            threads,
            state: "open".into(),
            updated: 0,
        }
    }

    #[test]
    fn a_review_given_becomes_a_follow_up_with_a_stable_id() {
        let mut data = Data::default();
        let events = from_reviews(&mut data.book(), &settings(), &[review(vec![blocker("t1", false), blocker("t2", false)])], 100);
        assert!(events.is_empty());
        assert_eq!(data.follow_ups.len(), 1);
        let f = &data.follow_ups[0];
        assert_eq!(f.id, "fu-acme/app#1111");
        assert_eq!(f.kind, FollowKind::BlockersGiven);
        assert_eq!(f.state, FollowState::Open);
        assert_eq!(f.person, "Sam");
        assert_eq!(f.login, "sam-dev");
        assert_eq!(f.open_blockers(), 2);
        assert_eq!(f.head, "aaa");
        assert_eq!((f.created, f.updated), (100, 100));

        // The same again changes nothing, not even `updated`.
        let before = data.clone();
        assert!(from_reviews(&mut data.book(), &settings(), &[review(vec![blocker("t1", false), blocker("t2", false)])], 200).is_empty());
        assert_eq!(data, before);
    }

    #[test]
    fn blockers_are_ticked_off_and_fixed_is_told_once() {
        let mut data = Data::default();
        let s = settings();
        from_reviews(&mut data.book(), &s, &[review(vec![blocker("t1", false), blocker("t2", false)])], 100);

        // One resolved: progress, no event.
        assert!(from_reviews(&mut data.book(), &s, &[review(vec![blocker("t1", true), blocker("t2", false)])], 200).is_empty());
        assert_eq!(data.follow_ups[0].open_blockers(), 1);
        assert_eq!(data.follow_ups[0].state, FollowState::Open);
        assert_eq!(data.follow_ups[0].updated, 200);
        assert_eq!(line(&data.follow_ups[0]), "Sam fixed 1 of 2 blockers on #1111");

        // Pushed, but one thread still open: not fixed.
        let mut pushed = review(vec![blocker("t1", true), blocker("t2", false)]);
        pushed.head = "bbb".into();
        assert!(from_reviews(&mut data.book(), &s, &[pushed], 250).is_empty());
        assert_eq!(data.follow_ups[0].state, FollowState::Open);

        // All resolved and pushed: fixed, told once.
        let mut done = review(vec![blocker("t1", true), blocker("t2", true)]);
        done.head = "bbb".into();
        let events = from_reviews(&mut data.book(), &s, &[done.clone()], 300);
        assert_eq!(events, vec![Event::FollowUpFixed { id: "fu-acme/app#1111".into() }]);
        assert_eq!(data.follow_ups[0].state, FollowState::Fixed);
        assert!(from_reviews(&mut data.book(), &s, &[done.clone()], 400).is_empty());

        // Even when another copy of the data lost the state, the same fix is not told again.
        data.follow_ups[0].state = FollowState::Open;
        assert!(from_reviews(&mut data.book(), &s, &[done], 500).is_empty());
        assert_eq!(data.follow_ups[0].state, FollowState::Fixed);
    }

    #[test]
    fn changes_requested_without_threads_is_fixed_by_a_push() {
        let mut data = Data::default();
        let s = settings();
        from_reviews(&mut data.book(), &s, &[review(Vec::new())], 100);
        assert_eq!(data.follow_ups[0].state, FollowState::Open);
        let mut pushed = review(Vec::new());
        pushed.head = "bbb".into();
        assert_eq!(from_reviews(&mut data.book(), &s, &[pushed], 200), vec![Event::FollowUpFixed { id: "fu-acme/app#1111".into() }]);
    }

    #[test]
    fn merged_closed_and_approved_are_done() {
        for (state, my_state, note) in [("merged", "CHANGES_REQUESTED", NOTE_MERGED), ("closed", "CHANGES_REQUESTED", NOTE_CLOSED), ("open", "APPROVED", NOTE_APPROVED)] {
            let mut data = Data::default();
            let s = settings();
            from_reviews(&mut data.book(), &s, &[review(vec![blocker("t1", false)])], 100);
            let mut r = review(vec![blocker("t1", false)]);
            r.state = state.into();
            r.my_state = my_state.into();
            assert!(from_reviews(&mut data.book(), &s, &[r.clone()], 200).is_empty());
            assert_eq!(data.follow_ups[0].state, FollowState::Done, "{state}/{my_state}");
            assert_eq!(data.follow_ups[0].note, note);
            // And it stays that way.
            let before = data.clone();
            from_reviews(&mut data.book(), &s, &[r], 300);
            assert_eq!(data, before);
        }
    }

    #[test]
    fn nothing_new_is_made_for_what_has_nothing_open() {
        let mut data = Data::default();
        let mut merged = review(vec![blocker("t1", false)]);
        merged.state = "merged".into();
        let mut approved = review(Vec::new());
        approved.my_state = "APPROVED".into();
        let mut commented = review(vec![blocker("t1", true)]);
        commented.my_state = "COMMENTED".into();
        from_reviews(&mut data.book(), &settings(), &[merged, approved, commented], 100);
        assert!(data.follow_ups.is_empty());
    }

    #[test]
    fn one_already_fixed_when_first_seen_is_recorded_but_not_announced() {
        let mut data = Data::default();
        let mut r = review(vec![blocker("t1", true)]);
        r.head = "bbb".into();
        assert!(from_reviews(&mut data.book(), &settings(), &[r.clone()], 100).is_empty());
        assert_eq!(data.follow_ups[0].state, FollowState::Fixed);
        assert!(from_reviews(&mut data.book(), &settings(), &[r], 200).is_empty());
    }

    #[test]
    fn a_fixed_one_opens_again_when_a_thread_is_reopened_and_is_told_again_after_a_new_push() {
        let mut data = Data::default();
        let s = settings();
        from_reviews(&mut data.book(), &s, &[review(vec![blocker("t1", false)])], 100);
        let mut fixed = review(vec![blocker("t1", true)]);
        fixed.head = "bbb".into();
        assert_eq!(from_reviews(&mut data.book(), &s, &[fixed], 200).len(), 1);
        // The person looked, was not happy, and reviewed again on the new head.
        let mut again = review(vec![blocker("t1", true), blocker("t2", false)]);
        again.head = "bbb".into();
        again.reviewed_head = "bbb".into();
        assert!(from_reviews(&mut data.book(), &s, &[again], 300).is_empty());
        assert_eq!(data.follow_ups[0].state, FollowState::Open);
        let mut fixed_again = review(vec![blocker("t1", true), blocker("t2", true)]);
        fixed_again.head = "ccc".into();
        fixed_again.reviewed_head = "bbb".into();
        assert_eq!(from_reviews(&mut data.book(), &s, &[fixed_again], 400), vec![Event::FollowUpFixed { id: "fu-acme/app#1111".into() }]);
    }

    #[test]
    fn done_by_the_person_stays_done_until_a_new_review() {
        let mut data = Data::default();
        let s = settings();
        let r = review(vec![blocker("t1", false)]);
        from_reviews(&mut data.book(), &s, std::slice::from_ref(&r), 100);
        assert!(close(&mut data.follow_ups, "#1111", false, 150).is_some());
        from_reviews(&mut data.book(), &s, std::slice::from_ref(&r), 200);
        assert_eq!(data.follow_ups[0].state, FollowState::Done);
        // Dropped ones never come back.
        let mut newer = r.clone();
        newer.head = "bbb".into();
        newer.reviewed_head = "bbb".into();
        from_reviews(&mut data.book(), &s, &[newer.clone()], 300);
        assert_eq!(data.follow_ups[0].state, FollowState::Open, "a new review opens it again");
        close(&mut data.follow_ups, "fu-acme/app#1111", true, 350);
        newer.reviewed_head = "ccc".into();
        from_reviews(&mut data.book(), &s, &[newer], 400);
        assert_eq!(data.follow_ups[0].state, FollowState::Dropped);
    }

    #[test]
    fn approval_taken_back_opens_it_again() {
        let mut data = Data::default();
        let s = settings();
        from_reviews(&mut data.book(), &s, &[review(vec![blocker("t1", false)])], 100);
        let mut approved = review(vec![blocker("t1", true)]);
        approved.my_state = "APPROVED".into();
        from_reviews(&mut data.book(), &s, &[approved], 200);
        assert_eq!(data.follow_ups[0].state, FollowState::Done);
        from_reviews(&mut data.book(), &s, &[review(vec![blocker("t1", true), blocker("t2", false)])], 300);
        assert_eq!(data.follow_ups[0].state, FollowState::Open);
        assert_eq!(data.follow_ups[0].note, "");
    }

    fn session(id: &str, state: SessionState, pr: Option<PrInfo>) -> SessionInfo {
        SessionInfo { id: id.into(), title: format!("Session {id}"), project_id: "p1".into(), project: "app".into(), state, pr, ..SessionInfo::default() }
    }

    fn pr(number: u64, reviewers: &[(&str, &str)]) -> PrInfo {
        PrInfo {
            repo: "acme/app".into(),
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/acme/app/pull/{number}"),
            state: "open".into(),
            ci: "passing".into(),
            reviewers: reviewers.iter().map(|(l, s)| ReviewerInfo { login: (*l).into(), state: (*s).into(), ..ReviewerInfo::default() }).collect(),
            ..PrInfo::default()
        }
    }

    fn snap(at: u64, sessions: Vec<SessionInfo>) -> Snapshot {
        Snapshot { at, me: Some("robin".into()), sessions, mac_online: true, ..Snapshot::default() }
    }

    #[test]
    fn the_first_round_only_records() {
        let mut data = Data::default();
        let mut failing = pr(12, &[("alex", "CHANGES_REQUESTED"), ("mira", "APPROVED")]);
        failing.ci = "failing".into();
        let s1 = snap(1000, vec![session("s1", SessionState::Idle, Some(failing))]);
        assert!(snapshot_on(&mut data.book(), None, &s1, 1000).is_empty());
        // What is there is known: a follow-up for the changes asked for, none for the approval.
        assert_eq!(data.follow_ups.len(), 1);
        assert_eq!(data.follow_ups[0].id, "rv-acme/app#12@alex");
        assert_eq!(data.follow_ups[0].kind, FollowKind::ReviewReceived);
        assert_eq!(data.follow_ups[0].session.as_deref(), Some("s1"));
        // The same on a later round is still old news.
        let s2 = snap(2000, s1.sessions.clone());
        assert!(snapshot_on(&mut data.book(), Some(&s1), &s2, 2000).is_empty());
    }

    #[test]
    fn a_new_review_is_told_once() {
        let mut data = Data::default();
        let s1 = snap(1000, vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "PENDING")])))]);
        assert!(snapshot_on(&mut data.book(), None, &s1, 1000).is_empty());
        let s2 = snap(2000, vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "CHANGES_REQUESTED")])))]);
        let events = snapshot_on(&mut data.book(), Some(&s1), &s2, 2000);
        assert_eq!(events, vec![Event::PrReviewed { session: "s1".into(), repo: "acme/app".into(), number: 12, reviewer: "alex".into(), state: "CHANGES_REQUESTED".into() }]);
        assert_eq!(data.follow_ups.len(), 1);
        assert_eq!(data.follow_ups[0].state, FollowState::Open);
        assert_eq!(data.follow_ups[0].note, NOTE_CHANGES);
        // Not again on the next round, and not after a restart (no previous round, same data).
        assert!(snapshot_on(&mut data.book(), Some(&s2), &snap(3000, s2.sessions.clone()), 3000).is_empty());
        assert!(snapshot_on(&mut data.book(), None, &snap(4000, s2.sessions.clone()), 4000).is_empty());

        // Review asked again, then changes asked for a second time: that is a new review.
        let s3 = snap(5000, vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "PENDING")])))]);
        assert!(snapshot_on(&mut data.book(), Some(&s2), &s3, 5000).is_empty());
        assert_eq!(data.follow_ups[0].note, NOTE_AGAIN);
        let s4 = snap(6000, s2.sessions.clone());
        assert_eq!(snapshot_on(&mut data.book(), Some(&s3), &s4, 6000).len(), 1);

        // Approved: told, and the follow-up is done.
        let s5 = snap(7000, vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "APPROVED")])))]);
        let events = snapshot_on(&mut data.book(), Some(&s4), &s5, 7000);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::PrReviewed { state, .. } if state == "APPROVED"));
        assert_eq!(data.follow_ups[0].state, FollowState::Done);
    }

    #[test]
    fn my_own_reviews_bots_and_review_sessions_are_not_news() {
        let mut data = Data::default();
        let s1 = snap(1000, vec![session("s1", SessionState::Idle, Some(pr(12, &[]))), session("s2", SessionState::Idle, Some(pr(40, &[])))]);
        snapshot_on(&mut data.book(), None, &s1, 1000);
        let mut reviewing = session("s2", SessionState::Idle, Some(pr(40, &[("alex", "CHANGES_REQUESTED")])));
        reviewing.review_of = Some(("acme/app".into(), 40));
        let s2 = snap(2000, vec![session("s1", SessionState::Idle, Some(pr(12, &[("Robin", "COMMENTED"), ("review-helper[bot]", "CHANGES_REQUESTED"), ("copilot-pull-request-reviewer", "COMMENTED")]))), reviewing]);
        assert!(snapshot_on(&mut data.book(), Some(&s1), &s2, 2000).is_empty());
        assert!(data.follow_ups.is_empty());
    }

    #[test]
    fn without_the_persons_login_reviews_are_only_recorded() {
        let mut data = Data::default();
        let mut s1 = snap(1000, vec![session("s1", SessionState::Idle, Some(pr(12, &[])))]);
        s1.me = None;
        snapshot_on(&mut data.book(), None, &s1, 1000);
        // It could be the person's own comment: nobody is told, nothing is followed.
        let mut s2 = snap(2000, vec![session("s1", SessionState::Idle, Some(pr(12, &[("robin", "COMMENTED"), ("alex", "CHANGES_REQUESTED")])))]);
        s2.me = None;
        assert!(snapshot_on(&mut data.book(), Some(&s1), &s2, 2000).is_empty());
        assert!(data.follow_ups.is_empty());
        // Once the login is known, what was there is not brought up as news.
        let s3 = snap(3000, s2.sessions.clone());
        assert!(snapshot_on(&mut data.book(), Some(&s2), &s3, 3000).is_empty());
        // And a new review is told as usual.
        let s4 = snap(4000, vec![session("s1", SessionState::Idle, Some(pr(12, &[("robin", "COMMENTED"), ("alex", "CHANGES_REQUESTED"), ("mira", "COMMENTED")])))]);
        assert_eq!(snapshot_on(&mut data.book(), Some(&s3), &s4, 4000).len(), 1);
    }

    #[test]
    fn a_teammates_pull_request_in_a_normal_session_is_left_alone() {
        let mut data = Data::default();
        let theirs = |reviewers: &[(&str, &str)], ci: &str| {
            let mut p = pr(12, reviewers);
            p.author = "Sam-Dev".into();
            p.ci = ci.into();
            vec![session("s1", SessionState::Idle, Some(p))]
        };
        let s1 = snap(1000, theirs(&[("alex", "PENDING")], "passing"));
        assert!(snapshot_on(&mut data.book(), None, &s1, 1000).is_empty());
        let s2 = snap(2000, theirs(&[("alex", "CHANGES_REQUESTED"), ("mira", "COMMENTED")], "failing"));
        assert!(snapshot_on(&mut data.book(), Some(&s1), &s2, 2000).is_empty());
        // Nothing was recorded either: no follow-up, nothing in `seen`.
        assert!(data.follow_ups.is_empty());
        assert!(data.follow_seen.is_empty(), "{:?}", data.follow_seen);

        // The person's own (the login in another case), and one whose author is not known yet, are theirs.
        let mine = |author: &str, reviewers: &[(&str, &str)]| {
            let mut p = pr(13, reviewers);
            p.author = author.into();
            vec![session("s2", SessionState::Idle, Some(p))]
        };
        for author in ["ROBIN", ""] {
            let mut data = Data::default();
            let s1 = snap(1000, mine(author, &[("alex", "PENDING")]));
            snapshot_on(&mut data.book(), None, &s1, 1000);
            let s2 = snap(2000, mine(author, &[("alex", "CHANGES_REQUESTED")]));
            assert_eq!(snapshot_on(&mut data.book(), Some(&s1), &s2, 2000).len(), 1, "author {author:?}");
        }
        // While the person's login is not known, a pull request with an author is not taken for theirs.
        let mut data = Data::default();
        let mut s1 = snap(1000, mine("robin", &[("alex", "CHANGES_REQUESTED")]));
        s1.me = None;
        snapshot_on(&mut data.book(), None, &s1, 1000);
        assert!(data.follow_seen.is_empty() && data.follow_ups.is_empty());
    }

    #[test]
    fn a_follow_up_made_before_the_author_was_known_is_let_go() {
        let mut data = Data::default();
        // First the pull request has no author yet, and a request for changes is followed.
        let s1 = snap(1000, vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "CHANGES_REQUESTED")])))]);
        snapshot_on(&mut data.book(), None, &s1, 1000);
        assert_eq!(open(&data.follow_ups).len(), 1);
        // Then it turns out a teammate opened it.
        let mut theirs = pr(12, &[("alex", "CHANGES_REQUESTED"), ("mira", "COMMENTED")]);
        theirs.author = "sam-dev".into();
        let s2 = snap(2000, vec![session("s1", SessionState::Idle, Some(theirs))]);
        assert!(snapshot_on(&mut data.book(), Some(&s1), &s2, 2000).is_empty());
        assert!(open(&data.follow_ups).is_empty());
        assert_eq!((data.follow_ups[0].state, data.follow_ups[0].note.as_str()), (FollowState::Dropped, NOTE_NOT_YOURS));
    }

    #[test]
    fn a_pull_request_seen_for_the_first_time_is_recorded_silently() {
        let mut data = Data::default();
        // The first round has the session, but its pull request is not found yet.
        let s1 = snap(1000, vec![session("s1", SessionState::Idle, None)]);
        snapshot_on(&mut data.book(), None, &s1, 1000);
        // Then the pull request, without its reviewers: the app reads those later.
        let s2 = snap(1010, vec![session("s1", SessionState::Idle, Some(pr(12, &[])))]);
        assert!(snapshot_on(&mut data.book(), Some(&s1), &s2, 1010).is_empty());
        // Minutes later the reviews that were there all along arrive: not news.
        let s3 = snap(1200, vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "CHANGES_REQUESTED"), ("mira", "COMMENTED")])))]);
        assert!(snapshot_on(&mut data.book(), Some(&s2), &s3, 1200).is_empty());
        assert_eq!(open(&data.follow_ups).len(), 1, "but the request for changes is followed");
        // From then on, a review is.
        let s4 = snap(1260, vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "CHANGES_REQUESTED"), ("mira", "APPROVED")])))]);
        assert_eq!(snapshot_on(&mut data.book(), Some(&s3), &s4, 1260).len(), 1);
    }

    #[test]
    fn the_first_review_on_a_pull_request_nobody_was_asked_to_review_is_told() {
        let mut data = Data::default();
        let s1 = snap(1000, vec![session("s1", SessionState::Idle, Some(pr(12, &[])))]);
        snapshot_on(&mut data.book(), None, &s1, 1000);
        // Known for long enough that "no reviewers" means there were none.
        let s2 = snap(1000 + REVIEWS_SETTLE_SECS, vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "COMMENTED")])))]);
        assert_eq!(snapshot_on(&mut data.book(), Some(&s1), &s2, 1000 + REVIEWS_SETTLE_SECS).len(), 1);
    }

    #[test]
    fn failing_ci_is_told_once_per_streak() {
        let mut data = Data::default();
        let ci = |state: &str| {
            let mut p = pr(12, &[]);
            p.ci = state.into();
            vec![session("s1", SessionState::Working, Some(p))]
        };
        let s1 = snap(1000, ci("passing"));
        snapshot_on(&mut data.book(), None, &s1, 1000);
        let failed = Event::CiFailed { session: "s1".into(), repo: "acme/app".into(), number: 12 };
        assert_eq!(snapshot_on(&mut data.book(), Some(&s1), &snap(2000, ci("failing")), 2000), vec![failed.clone()]);
        assert!(snapshot_on(&mut data.book(), Some(&s1), &snap(2100, ci("failing")), 2100).is_empty());
        // Running again and failing again is the same streak.
        assert!(snapshot_on(&mut data.book(), Some(&s1), &snap(2200, ci("running")), 2200).is_empty());
        assert!(snapshot_on(&mut data.book(), Some(&s1), &snap(2300, ci("failing")), 2300).is_empty());
        // Passing ends it.
        assert!(snapshot_on(&mut data.book(), Some(&s1), &snap(2400, ci("passing")), 2400).is_empty());
        assert_eq!(snapshot_on(&mut data.book(), Some(&s1), &snap(2500, ci("failing")), 2500), vec![failed]);
        // A restart while it fails does not tell it again.
        assert!(snapshot_on(&mut data.book(), None, &snap(2600, ci("failing")), 2600).is_empty());
    }

    #[test]
    fn reviews_received_close_when_merged_or_resolved() {
        let mut data = Data::default();
        let s1 = snap(1000, vec![session("s1", SessionState::Idle, Some(pr(12, &[])))]);
        snapshot_on(&mut data.book(), None, &s1, 1000);
        let mut commented = pr(12, &[("alex", "COMMENTED"), ("mira", "CHANGES_REQUESTED")]);
        commented.unresolved_threads = 2;
        let s2 = snap(2000, vec![session("s1", SessionState::Idle, Some(commented.clone()))]);
        assert_eq!(snapshot_on(&mut data.book(), Some(&s1), &s2, 2000).len(), 2);
        assert_eq!(open(&data.follow_ups).len(), 2);
        // The comment threads were resolved: only the request for changes is left.
        commented.unresolved_threads = 0;
        let s3 = snap(3000, vec![session("s1", SessionState::Idle, Some(commented.clone()))]);
        assert!(snapshot_on(&mut data.book(), Some(&s2), &s3, 3000).is_empty());
        assert_eq!(open(&data.follow_ups).iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), vec!["rv-acme/app#12@mira"]);
        // Merged: nothing is left.
        commented.state = "merged".into();
        let s4 = snap(4000, vec![session("s1", SessionState::Idle, Some(commented))]);
        assert!(snapshot_on(&mut data.book(), Some(&s3), &s4, 4000).is_empty());
        assert!(open(&data.follow_ups).is_empty());
        assert_eq!(data.follow_ups.iter().find(|f| f.id == "rv-acme/app#12@mira").map(|f| f.note.as_str()), Some(NOTE_MERGED));
    }

    #[test]
    fn a_review_session_is_linked_to_its_follow_up() {
        let mut data = Data::default();
        from_reviews(&mut data.book(), &settings(), &[review(vec![blocker("t1", false)])], 100);
        let mut s = session("rev1", SessionState::Idle, None);
        s.review_of = Some(("acme/app".into(), 1111));
        snapshot_on(&mut data.book(), None, &snap(200, vec![s]), 200);
        assert_eq!(data.follow_ups[0].session.as_deref(), Some("rev1"));
        assert_eq!(data.follow_ups[0].updated, 100);
    }

    #[test]
    fn people_get_their_names() {
        let mut data = Data::default();
        let s1 = snap(1000, vec![session("s1", SessionState::Idle, Some(pr(12, &[("sam-dev", "CHANGES_REQUESTED")])))]);
        snapshot_on(&mut data.book(), None, &s1, 1000);
        assert_eq!(who(&data.follow_ups[0]), "sam-dev");
        fill_people(&mut data.follow_ups, &settings());
        assert_eq!(who(&data.follow_ups[0]), "Sam");
        assert_eq!(line(&data.follow_ups[0]), "Sam asked for changes on your #12");
    }

    #[test]
    fn stale_follow_ups_are_named_once_per_period() {
        let mut data = Data::default();
        from_reviews(&mut data.book(), &settings(), &[review(vec![blocker("t1", false)])], 1000);
        let day = 24 * 3600;
        assert!(stale(&mut data.follow_ups, 2 * day, 1000 + day).is_empty());
        assert_eq!(stale(&mut data.follow_ups, 2 * day, 1000 + 2 * day), vec![Event::FollowUpStale { id: "fu-acme/app#1111".into() }]);
        assert_eq!(data.follow_ups[0].nudged, Some(1000 + 2 * day));
        assert!(stale(&mut data.follow_ups, 2 * day, 1000 + 3 * day).is_empty());
        assert_eq!(stale(&mut data.follow_ups, 2 * day, 1000 + 4 * day).len(), 1);
        // A fixed or closed one is never stale.
        data.follow_ups[0].state = FollowState::Fixed;
        assert!(stale(&mut data.follow_ups, 2 * day, 1000 + 40 * day).is_empty());
        data.follow_ups[0].state = FollowState::Done;
        assert!(stale(&mut data.follow_ups, 2 * day, 1000 + 40 * day).is_empty());
    }

    #[test]
    fn open_ones_are_ordered_fixed_first_then_most_blockers_then_oldest() {
        let mut data = Data::default();
        let mk = |id: &str, state: FollowState, open: usize, created: u64| FollowUp { id: id.into(), state, blockers: (0..open).map(|i| blocker(&format!("t{i}"), false)).collect(), created, ..FollowUp::default() };
        data.follow_ups = vec![mk("old", FollowState::Open, 1, 10), mk("big", FollowState::Open, 5, 30), mk("fixed", FollowState::Fixed, 0, 40), mk("older", FollowState::Open, 1, 5), mk("done", FollowState::Done, 9, 1), mk("dropped", FollowState::Dropped, 9, 1)];
        assert_eq!(open(&data.follow_ups).iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), vec!["fixed", "big", "older", "old"]);
    }

    #[test]
    fn custom_follow_ups_and_closing() {
        let mut data = Data::default();
        let id = add_custom(&mut data.follow_ups, "  Ask Mira about  the staging database ", "Mira", 100);
        assert!(!id.is_empty());
        assert_eq!(add_custom(&mut data.follow_ups, "ask mira about the staging database", "mira", 200), id, "the same twice is one");
        let other = add_custom(&mut data.follow_ups, "Check the billing export", "", 100);
        assert_ne!(other, id);
        assert_eq!(data.follow_ups.len(), 2);
        assert_eq!(line(&data.follow_ups[0]), "Ask Mira about the staging database (Mira)");
        from_reviews(&mut data.book(), &settings(), &[review(vec![blocker("t1", false)])], 300);

        // By words.
        assert!(close(&mut data.follow_ups, "something unrelated entirely", false, 400).is_none());
        let closed = close(&mut data.follow_ups, "the staging database", false, 400).expect("found by words");
        assert_eq!(closed.id, id);
        assert_eq!(closed.state, FollowState::Done);
        assert_eq!(closed.updated, 400);
        assert!(close(&mut data.follow_ups, &id, false, 500).is_none(), "already closed");
        // By number, in a sentence.
        let closed = close(&mut data.follow_ups, "done with #1111", true, 600).expect("found by number");
        assert_eq!(closed.id, "fu-acme/app#1111");
        assert_eq!(closed.state, FollowState::Dropped);
        // By id.
        assert_eq!(close(&mut data.follow_ups, &other, false, 700).map(|f| f.state), Some(FollowState::Done));
        assert!(open(&data.follow_ups).is_empty());
        assert!(close(&mut data.follow_ups, "", false, 800).is_none());
    }

    #[test]
    fn closing_by_number_closes_everything_on_that_pull_request() {
        let mut data = Data::default();
        let s1 = snap(1000, vec![session("s1", SessionState::Idle, Some(pr(12, &[("alex", "CHANGES_REQUESTED"), ("mira", "CHANGES_REQUESTED")])))]);
        snapshot_on(&mut data.book(), None, &s1, 1000);
        assert_eq!(open(&data.follow_ups).len(), 2);
        assert!(close(&mut data.follow_ups, "12", false, 2000).is_some());
        assert!(open(&data.follow_ups).is_empty());
        // The reviewers have not changed their mind, so it stays closed.
        snapshot_on(&mut data.book(), Some(&s1), &snap(3000, s1.sessions.clone()), 3000);
        assert!(open(&data.follow_ups).is_empty());
    }

    /// v2's entry points on the ledger alone (M56.2), with what they say written out: a quiet
    /// follow-up is a draft to the teammate (a notification when the teammate is not known), not
    /// again until it is quiet again; "fixed" is told once, and not after a restart either.
    #[test]
    fn the_v2_round_on_the_ledger_nudges_and_tells_fixed_once() {
        const T0: u64 = 1_800_000_000;
        let s = settings();
        let mut ledger = Data::default();
        let mut stranger = review(vec![blocker("t1", false)]);
        stranger.number = 1200;
        stranger.author = "stranger".into();
        let quiet = Snapshot::default();
        assert!(given(&mut ledger.book(), &s, &[review(vec![blocker("t1", true), blocker("t2", false), blocker("t3", false)]), stranger], None, T0).is_empty());
        assert!(round(&mut ledger.book(), &s, None, &quiet, T0).is_empty(), "a start's first round only records");
        assert!(round(&mut ledger.book(), &s, Some(&quiet), &quiet, T0 + STALE_AFTER - 10).is_empty());
        let nudges = vec![
            Action::Draft { to: "Sam".into(), body: "#1111: 2 of the 3 points from my review are still open. Could you take a look?".into(), why: "Nothing happened on #1111 for 2 days.".into(), about: Some("fu-acme/app#1111".into()) },
            Action::Notify { title: "Quiet for 2 days".into(), body: "Stranger has 1 open blocker on #1200.".into(), session: None },
        ];
        assert_eq!(round(&mut ledger.book(), &s, Some(&quiet), &quiet, T0 + STALE_AFTER), nudges);
        assert!(round(&mut ledger.book(), &s, Some(&quiet), &quiet, T0 + STALE_AFTER + 60).is_empty(), "nudged once");
        assert_eq!(round(&mut ledger.book(), &s, Some(&quiet), &quiet, T0 + 2 * STALE_AFTER), nudges, "quiet again");

        let fixed = [review(vec![blocker("t1", true), blocker("t2", true), blocker("t3", true)])].map(|mut r| {
            r.head = "bbb".into();
            r
        });
        assert_eq!(
            given(&mut ledger.book(), &s, &fixed, None, T0 + 2 * STALE_AFTER + 100),
            vec![
                Action::Notify { title: "#1111 is ready for you to look again".into(), body: "Sam fixed all 3 blockers on #1111: ready for you to look again.".into(), session: None },
                Action::Say { text: "Sam fixed all 3 blockers on PR 1111: ready for you to look again.".into(), show: Some(Show { view: Some("manager".into()), section: Some("followups".into()), ..Show::default() }) },
            ]
        );
        assert!(given(&mut ledger.book(), &s, &fixed, None, T0 + 2 * STALE_AFTER + 200).is_empty());
        // The ledger as it is kept, read back: still told.
        let mut again: Data = serde_json::from_value(serde_json::to_value(&ledger).unwrap()).unwrap();
        assert!(given(&mut again.book(), &s, &fixed, None, T0 + 2 * STALE_AFTER + 300).is_empty());
        assert_eq!(again, ledger);
        // A ledger kept before the follow-ups came over reads as one without them.
        let old: Data = serde_json::from_str(r#"{"items":[]}"#).unwrap();
        assert!(old.follow_ups.is_empty() && old.follow_seen.is_empty());
    }

    /// The server's round (M56.2): no snapshot; a quiet one with a teammate is left for the Mac
    /// to draft (not marked), one with nobody known is told; "fixed" from the reviews as on the Mac.
    #[test]
    fn away_leaves_a_teammates_note_to_the_mac() {
        const T0: u64 = 1_800_000_000;
        let s = settings();
        let mut ledger = Data::default();
        let mut stranger = review(vec![blocker("t1", false)]);
        stranger.number = 1200;
        stranger.author = "stranger".into();
        given(&mut ledger.book(), &s, &[review(vec![blocker("t1", false)]), stranger], None, T0);
        assert!(away(&mut ledger.book(), &s, T0 + 60).is_empty());
        assert_eq!(away(&mut ledger.book(), &s, T0 + STALE_AFTER), vec![Action::Notify { title: "Quiet for 2 days".into(), body: "Stranger has 1 open blocker on #1200.".into(), session: None }]);
        let sams = ledger.follow_ups.iter().find(|f| f.number == 1111).unwrap();
        assert_eq!(sams.nudged, None, "Sam's note waits for the Mac");
        assert!(away(&mut ledger.book(), &s, T0 + STALE_AFTER + 60).is_empty());
        // Home again, the Mac's round drafts it at once.
        let quiet = Snapshot::default();
        assert!(matches!(round(&mut ledger.book(), &s, Some(&quiet), &quiet, T0 + STALE_AFTER + 120).as_slice(), [Action::Draft { to, .. }] if to == "Sam"));
    }
}
