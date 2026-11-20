//! The daily brief and "today's update" as a walk through the app. No agent is needed for
//! these: they are built from what the manager already knows.

use super::followups;
use super::gh::one_line;
use super::{Brief, BriefLine, Data, FollowKind, FollowState, FollowUp, MsgState, PanelTab, PrInfo, ReviewAsk, SessionInfo, SessionState, Settings, Show, Snapshot, TourStop};

/// How long a session that finished stays in the brief.
const DONE_FOR: u64 = 24 * 3600;
/// How many stops a walk has at most, and how many lines of one kind a brief lists.
const MAX_STOPS: usize = 8;
const MAX_ASKS: usize = 5;
const MAX_MESSAGES: usize = 4;

// ----- what the rules noted in `Data::seen` -------------------------------------------------------

/// Where the rules note when a session finished (`Data::seen`: key → unix seconds).
pub fn done_key(session: &str) -> String {
    format!("done|{session}")
}

/// Where the rules note a review a session has to be told about (`Data::seen`: key →
/// "pending:<STATE>", then "told:<STATE>").
pub fn tell_key(session: &str, repo: &str, number: u64, reviewer: &str) -> String {
    format!("tell|{session}|{repo}#{number}|{}", reviewer.to_lowercase())
}

/// A review a session has not been told about yet, because it was busy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTell {
    pub key: String,
    pub session: String,
    pub repo: String,
    pub number: u64,
    pub reviewer: String,
    pub state: String,
}

/// Every review that still waits to be told to its session.
pub fn pending_tells(data: &Data) -> Vec<PendingTell> {
    let mut out = Vec::new();
    for (key, value) in data.seen.range("tell|".to_string().."tell}".to_string()) {
        let Some(state) = value.strip_prefix("pending:") else { continue };
        let Some(rest) = key.strip_prefix("tell|") else { continue };
        // From the right: the reviewer and the pull request have no '|' in them.
        let mut parts = rest.rsplitn(3, '|');
        let (Some(reviewer), Some(pr), Some(session)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let Some((repo, number)) = pr.rsplit_once('#') else { continue };
        let Ok(number) = number.parse::<u64>() else { continue };
        out.push(PendingTell { key: key.clone(), session: session.to_string(), repo: repo.to_string(), number, reviewer: reviewer.to_string(), state: state.to_string() });
    }
    out
}

/// What the brief reads besides the snapshot (M56.2): v1 has it in `Data` ([`BriefData::of_v1`]),
/// v2 in its own stores (the outbox, the native review-tell, its turn reports).
#[derive(Clone, Debug, Default)]
pub struct BriefData {
    pub followups: Vec<FollowUp>,
    pub outbox: Vec<super::OutMessage>,
    /// Reviews that still wait to be told to their sessions.
    pub tells: Vec<PendingTell>,
    /// When each session last finished its turn (Unix seconds).
    pub finished: std::collections::BTreeMap<String, u64>,
    /// The last time anything was seen (for a snapshot with no time).
    pub heartbeat: u64,
}

impl BriefData {
    /// v1's: from `data.json`.
    pub fn of_v1(data: &Data) -> BriefData {
        let finished = data.seen.range("done|".to_string().."done}".to_string()).filter_map(|(k, v)| Some((k.strip_prefix("done|")?.to_string(), v.parse::<u64>().ok()?))).collect();
        BriefData { followups: data.followups.clone(), outbox: data.outbox.clone(), tells: pending_tells(data), finished, heartbeat: data.heartbeat }
    }
}

/// `manager/brief.json` (M56.2): v2's last daily brief, so it is made once a day, and when each
/// session last finished a turn (the brief's "finished" lines). Damaged, it starts empty: at worst
/// today's brief is said twice.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct BriefFile {
    pub last: Option<Brief>,
    /// Session id → when it last finished a turn (Unix seconds), the last day's only.
    pub finished: std::collections::BTreeMap<String, u64>,
}

impl BriefFile {
    /// A session finished a turn at `at`; what finished more than a day before is let go.
    pub fn finished_at(&mut self, session: &str, at: u64) {
        self.finished.insert(session.to_string(), at);
        self.finished.retain(|_, t| at.saturating_sub(*t) <= DONE_FOR);
    }
}

impl crate::store::Store for BriefFile {
    const NAME: &'static str = "manager/brief";
    const VERSION: u32 = 1;
    const ON_DAMAGE: crate::store::OnDamage = crate::store::OnDamage::Empty;

    fn path() -> std::path::PathBuf {
        super::store_dir().join("brief.json")
    }
}

// ----- words ------------------------------------------------------------------------------------

/// "#1111" as it is said: "PR 1111".
pub fn spoken(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' && chars.peek().is_some_and(char::is_ascii_digit) {
            out.push_str("PR ");
        } else {
            out.push(c);
        }
    }
    out
}

/// A teammate as the person calls them, else their login.
pub fn name_of(settings: &Settings, login: &str) -> String {
    settings.person(login).map(|p| p.name.clone()).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| login.trim().to_string())
}

/// A title short enough for a line, without a full stop at its end.
fn short(title: &str, max: usize) -> String {
    let t = one_line(title, max);
    t.trim_end_matches(['.', ':', ',', ';']).to_string()
}

/// A session by its title.
pub(crate) fn called(session: &SessionInfo) -> String {
    let t = short(&session.title, 60);
    if t.is_empty() { "A session".to_string() } else { t }
}

/// The end of a session's last reply as one line (usually the question it stopped on).
pub(crate) fn reply_line(session: &SessionInfo, max: usize) -> String {
    let Some(reply) = session.last_reply.as_deref() else { return String::new() };
    let last = reply.lines().rev().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("```")).unwrap_or_default();
    one_line(last, max)
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// "a", "a and b", "a, b and c".
pub(super) fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn first_upper(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A sentence with one full stop at its end (a question keeps its question mark).
pub fn sentence(text: &str) -> String {
    let t = text.trim().trim_end_matches(['.', ':', ';', ',']).trim_end();
    if t.ends_with(['?', '!']) { first_upper(t) } else { format!("{}.", first_upper(t)) }
}

// ----- what there is to say ---------------------------------------------------------------------

/// One thing worth telling: a line of the brief, and a stop of the walk when it has a place.
struct Item {
    kind: &'static str,
    /// The line in the brief.
    text: String,
    /// What is said at the stop.
    say: String,
    show: Option<Show>,
}

pub(super) fn show_session(session: &str, tab: Option<PanelTab>, section: Option<&str>) -> Option<Show> {
    Some(Show { session: Some(session.to_string()), view: None, tab, section: section.map(str::to_string) })
}

pub fn show_manager(section: &str) -> Option<Show> {
    Some(Show { session: None, view: Some("manager".to_string()), tab: None, section: Some(section.to_string()) })
}

/// The sessions the manager looks at: not those of projects it was told to leave alone.
fn managed<'a>(settings: &'a Settings, snap: &'a Snapshot) -> impl Iterator<Item = &'a SessionInfo> {
    snap.sessions.iter().filter(|s| !settings.off_projects.contains(&s.project_id))
}

/// The person's own open pull request of a session (not one it reviews, and not a
/// teammate's that the session merely sits on).
fn own_pr<'a>(session: &'a SessionInfo, me: &str) -> Option<&'a PrInfo> {
    if session.review_of.is_some() {
        return None;
    }
    session.pr.as_ref().filter(|pr| pr.number > 0 && !matches!(pr.state.as_str(), "merged" | "closed") && followups::is_own(pr, me))
}

fn pr_label(pr: &PrInfo) -> String {
    let title = short(&pr.title, 48);
    if title.is_empty() { format!("#{}", pr.number) } else { format!("#{} ({title})", pr.number) }
}

/// The pull requests that ask for the person's review, without the ones already followed
/// (those are told as follow-ups).
fn asks_of<'a>(data: &BriefData, snap: &'a Snapshot) -> Vec<&'a ReviewAsk> {
    snap.review_asks
        .iter()
        .filter(|a| a.number > 0)
        .filter(|a| !data.followups.iter().any(|f| f.kind == FollowKind::BlockersGiven && f.is_open() && f.number == a.number && f.repo == a.repo))
        .collect()
}

/// Everything worth telling, in the brief's order.
fn items(settings: &Settings, data: &BriefData, snap: &Snapshot, now: u64) -> Vec<Item> {
    let mut out = Vec::new();
    let me = snap.me.as_deref().unwrap_or_default().trim().to_lowercase();

    // 1. Sessions that wait for the person.
    for s in managed(settings, snap).filter(|s| s.state == SessionState::NeedsYou) {
        let reply = reply_line(s, 110);
        let name = called(s);
        let (text, say) = if reply.is_empty() { (format!("{name} needs you"), format!("{name} stopped and waits for you.")) } else { (format!("{name} needs you: {reply}"), format!("{name} stopped and waits for you. {}", sentence(&reply))) };
        out.push(Item { kind: "needs_you", text, say, show: show_session(&s.id, None, Some("reply")) });
    }
    if snap.approvals > 0 {
        let text = format!("{} for your OK", if snap.approvals == 1 { "1 command waits".to_string() } else { format!("{} commands wait", snap.approvals) });
        out.push(Item { kind: "needs_you", say: sentence(&text), text, show: None });
    }

    // 2. Reviews on the person's pull requests.
    let mut covered: Vec<String> = Vec::new();
    for s in managed(settings, snap) {
        let Some(pr) = own_pr(s, &me) else { continue };
        let label = pr_label(pr);
        let mut approvers = Vec::new();
        let asked = pr.reviewers.iter().any(|r| r.state.eq_ignore_ascii_case("CHANGES_REQUESTED"));
        for r in &pr.reviewers {
            let login = r.login.trim();
            if login.is_empty() || login.to_lowercase() == me {
                continue;
            }
            let id = followups::received_id(&pr.repo, pr.number, login);
            let follow = data.followups.iter().find(|f| f.id == id);
            let who = name_of(settings, login);
            let state = r.state.to_uppercase();
            let verb = match state.as_str() {
                "APPROVED" => {
                    approvers.push(who);
                    continue;
                }
                // Asked for changes: told unless the person closed it themselves.
                "CHANGES_REQUESTED" if follow.is_none_or(|f| f.is_open()) => "asked for changes on",
                // Comments: only while something of them is open.
                "COMMENTED" if follow.is_some_and(|f| f.is_open()) => "left comments on",
                _ => continue,
            };
            covered.push(id);
            out.push(Item { kind: "review", text: format!("{who} {verb} {label}"), say: format!("{who} {verb} #{}. Here is the review.", pr.number), show: show_session(&s.id, Some(PanelTab::PullRequest), Some("reviews")) });
        }
        if !approvers.is_empty() && !asked && pr.state == "open" && pr.ci != "failing" && !pr.conflicting {
            let who = and_list(&approvers);
            out.push(Item { kind: "review", text: format!("{who} approved {label}: ready for you to merge"), say: format!("{who} approved #{}. Merging it is yours to do.", pr.number), show: show_session(&s.id, Some(PanelTab::PullRequest), Some("reviews")) });
        }
    }

    // 3. Failing checks, then conflicts.
    for s in managed(settings, snap) {
        let Some(pr) = own_pr(s, &me) else { continue };
        if pr.ci == "failing" {
            out.push(Item { kind: "blocked", text: format!("Checks fail on {}", pr_label(pr)), say: format!("The checks fail on #{}. Here they are.", pr.number), show: show_session(&s.id, Some(PanelTab::PullRequest), Some("checks")) });
        }
    }
    for s in managed(settings, snap) {
        let Some(pr) = own_pr(s, &me) else { continue };
        if pr.conflicting {
            out.push(Item { kind: "blocked", text: format!("{} has merge conflicts", pr_label(pr)), say: format!("#{} has merge conflicts with its base branch.", pr.number), show: show_session(&s.id, Some(PanelTab::PullRequest), None) });
        }
    }

    // 4. Follow-ups (the reviews received that were told above are not repeated).
    for f in followups::open(&data.followups) {
        if f.kind == FollowKind::ReviewReceived && covered.contains(&f.id) {
            continue;
        }
        let line = followups::line(f);
        let session = f.session.as_deref().filter(|id| managed(settings, snap).any(|s| s.id == *id));
        let show = match session {
            Some(id) => show_session(id, Some(PanelTab::PullRequest), Some("reviews")),
            None => show_manager("followups"),
        };
        out.push(Item { kind: "follow_up", say: sentence(&line), text: line, show });
    }

    // 5. Messages that wait for the person's OK.
    let waiting = super::messages::waiting_in(&data.outbox);
    for m in waiting.iter().take(MAX_MESSAGES) {
        let to = if m.to.trim().is_empty() { "a teammate" } else { m.to.trim() };
        let text = if m.urgent { format!("A holding reply to {to} waits for your OK") } else { format!("A message to {to} waits for your OK") };
        out.push(Item { kind: "message", say: sentence(&text), text, show: show_manager("messages") });
    }
    if waiting.len() > MAX_MESSAGES {
        let text = format!("{} more wait for your OK", count(waiting.len() - MAX_MESSAGES, "message", "messages"));
        out.push(Item { kind: "message", say: sentence(&text), text, show: show_manager("messages") });
    }

    // 6. Pull requests that ask for the person's review (not the ones already followed).
    let asks = asks_of(data, snap);
    for a in asks.iter().take(MAX_ASKS) {
        let who = name_of(settings, &a.author);
        let title = short(&a.title, 48);
        let label = if title.is_empty() { format!("#{}", a.number) } else { format!("#{} ({title})", a.number) };
        let text = if who.is_empty() { format!("{label} asks for your review") } else { format!("{who} asks for your review of {label}") };
        let session = managed(settings, snap).find(|s| s.review_of.as_ref().is_some_and(|(repo, n)| *repo == a.repo && *n == a.number));
        let say = format!("{} The review session is here.", sentence(&if who.is_empty() { format!("#{} asks for your review", a.number) } else { format!("{who} asks for your review of #{}", a.number) }));
        out.push(Item { kind: "review", text, say, show: session.and_then(|s| show_session(&s.id, Some(PanelTab::PullRequest), None)) });
    }
    if asks.len() > MAX_ASKS {
        let text = format!("{} more ask for your review", count(asks.len() - MAX_ASKS, "pull request", "pull requests"));
        out.push(Item { kind: "review", say: sentence(&text), text, show: None });
    }

    // 7. Sessions that finished.
    for s in managed(settings, snap).filter(|s| s.state == SessionState::Idle) {
        let finished = data.finished.get(&s.id).copied();
        if !finished.is_some_and(|t| t <= now && now - t <= DONE_FOR) {
            continue;
        }
        let name = called(s);
        let reply = reply_line(s, 110);
        let (text, say) = if reply.is_empty() { (format!("{name} finished"), format!("{name} finished its work.")) } else { (format!("{name} finished: {reply}"), format!("{name} finished. {}", sentence(&reply))) };
        out.push(Item { kind: "done", text, say, show: show_session(&s.id, None, Some("reply")) });
    }

    out
}

/// What the manager does next by itself, as one sentence without its full stop.
fn next_line(settings: &Settings, data: &BriefData, snap: &Snapshot) -> String {
    let mut plans: Vec<String> = Vec::new();

    let mut busy: Vec<String> = Vec::new();
    for tell in data.tells.iter().cloned() {
        if let Some(s) = managed(settings, snap).find(|s| s.id == tell.session) {
            let name = called(s);
            if !busy.contains(&name) {
                busy.push(name);
            }
        }
    }
    if !busy.is_empty() {
        let what = if busy.len() == 1 { format!("tell {} about its review as soon as it is free", busy[0]) } else { format!("tell {} sessions about their reviews as soon as they are free", busy.len()) };
        plans.push(what);
    }

    let going = data.outbox.iter().filter(|m| m.state == MsgState::Approved).count();
    if going > 0 {
        plans.push(format!("send the {} you allowed", if going == 1 { "message".to_string() } else { format!("{going} messages") }));
    }

    let watched: Vec<String> = followups::open(&data.followups).iter().filter(|f| f.kind == FollowKind::BlockersGiven && f.state == FollowState::Open).take(3).map(|f| format!("#{}", f.number)).collect();
    if !watched.is_empty() {
        plans.push(format!("watch {} for fixes", and_list(&watched)));
    }

    if plans.is_empty() {
        return "Next I keep watching your sessions and pull requests, and tell you when something changes".to_string();
    }
    plans.truncate(2);
    format!("Next I {}", and_list(&plans))
}

/// What waits for the person, counted the way the rail's badge counts it
/// (`Harness::needs_you`): sessions that stopped to ask, commands and messages that wait for
/// an OK, and follow-ups a teammate says are fixed.
struct Waiting<'a> {
    sessions: usize,
    approvals: usize,
    messages: usize,
    fixed: Vec<&'a FollowUp>,
}

impl Waiting<'_> {
    fn of<'a>(settings: &Settings, data: &'a BriefData, snap: &Snapshot) -> Waiting<'a> {
        Waiting {
            sessions: managed(settings, snap).filter(|s| s.state == SessionState::NeedsYou).count(),
            approvals: snap.approvals,
            messages: super::messages::waiting_in(&data.outbox).len(),
            fixed: data.followups.iter().filter(|f| f.state == FollowState::Fixed).collect(),
        }
    }

    fn count(&self) -> usize {
        self.sessions + self.approvals + self.messages + self.fixed.len()
    }
}

/// The first sentence of the spoken brief: everything that waits for the person, so it never
/// says "nothing" while a message or a fix waits. `names` are the sessions that stopped to ask.
fn opening(waiting: &Waiting, names: &[String]) -> String {
    if waiting.count() == 0 {
        return "Nothing needs you right now.".to_string();
    }
    let only_sessions = waiting.sessions == waiting.count();
    let mut parts = Vec::new();
    if only_sessions {
        // Only sessions: say which.
        let who = if names.len() <= 3 { and_list(names) } else { format!("{} and {} more", names[..3].join(", "), names.len() - 3) };
        let head = if waiting.sessions == 1 { "1 session needs you".to_string() } else { format!("{} sessions need you", waiting.sessions) };
        return sentence(&if who.is_empty() { head } else { format!("{head}: {who}") });
    }
    match (waiting.sessions, names) {
        (0, _) => {}
        (1, [name]) => parts.push(format!("{name} waits for an answer")),
        (1, _) => parts.push("1 session waits for an answer".to_string()),
        (n, _) => parts.push(format!("{n} sessions wait for an answer")),
    }
    match waiting.approvals {
        0 => {}
        1 => parts.push("1 command waits for your OK".to_string()),
        n => parts.push(format!("{n} commands wait for your OK")),
    }
    match waiting.messages {
        0 => {}
        1 => parts.push("1 message waits for your OK".to_string()),
        n => parts.push(format!("{n} messages wait for your OK")),
    }
    match waiting.fixed.as_slice() {
        [] => {}
        [f] if f.number > 0 => parts.push(format!("{} says #{} is fixed", followups::who(f), f.number)),
        [_] => parts.push("1 follow-up is marked fixed".to_string()),
        many => parts.push(format!("{} follow-ups are marked fixed", many.len())),
    }
    match parts.as_slice() {
        // One kind of thing: the sentence itself has the number.
        [one] => sentence(one),
        _ => sentence(&format!("{} things need you: {}", waiting.count(), and_list(&parts))),
    }
}

/// The brief as it is read out: three to six short sentences, numbers first. `asks` is how
/// many pull requests ask for a review.
fn sentences(found: &[Item], waiting: &Waiting, asks: usize, next: &str) -> Vec<String> {
    let of = |kind: &str| found.iter().filter(|i| i.kind == kind).collect::<Vec<_>>();
    let mut out = Vec::new();

    // What needs the person.
    let names: Vec<String> = found.iter().filter(|i| i.kind == "needs_you" && i.show.is_some()).filter_map(|i| i.text.split(" needs you").next().map(str::to_string)).collect();
    out.push(opening(waiting, &names));

    // The rest, most important first: the number, and the first one when there is room.
    let mut more: Vec<(String, Option<String>)> = Vec::new();
    let mut group = |list: Vec<&Item>, many: &str| match list.as_slice() {
        [] => {}
        [one] => more.push((sentence(&one.text), None)),
        [top, ..] => more.push((format!("{} {many}.", list.len()), Some(format!("First: {}", sentence(&top.text))))),
    };
    group(found.iter().filter(|i| i.kind == "review" && i.show.as_ref().is_some_and(|s| s.section.as_deref() == Some("reviews"))).collect(), "reviews are in on your pull requests");
    group(of("blocked"), "things are blocked");
    // The follow-ups that still wait on someone else (the fixed ones were said first, and so
    // were the messages).
    let fixed: Vec<String> = waiting.fixed.iter().map(|f| followups::line(f)).collect();
    group(of("follow_up").into_iter().filter(|i| !fixed.contains(&i.text)).collect(), "follow-ups are still open with others");
    if asks > 0 {
        more.push((format!("{} for your review.", if asks == 1 { "1 pull request asks".to_string() } else { format!("{asks} pull requests ask") }), None));
    }
    let done = of("done").len();
    if done > 0 {
        more.push((format!("{} finished.", count(done, "session", "sessions")), None));
    }
    if more.is_empty() {
        more.push(("No reviews came in and nothing is blocked.".to_string(), None));
    }

    // Four sentences between the first and the last: one per kind, a "First: …" where one is left.
    more.truncate(4);
    let mut spare = 4 - more.len();
    for (short, long) in more {
        out.push(short);
        if let Some(long) = long.filter(|_| spare > 0) {
            spare -= 1;
            out.push(long);
        }
    }
    out.push(sentence(next));
    out
}

/// The brief for today: what needs the person, reviews in, follow-ups, messages waiting,
/// what is blocked, what finished, what the manager does next.
/// The brief's "still waiting on you" (M42, events-22): each matter told and not yet seen, with
/// how long it has waited, from code. The brain only words the brief when it runs.
pub fn waiting_lines(waiting: &[(String, u64)]) -> Vec<BriefLine> {
    waiting
        .iter()
        .map(|(title, age_ms)| {
            let hours = age_ms / 3_600_000;
            let age = if hours >= 24 { format!("{} days", hours / 24) } else if hours >= 1 { format!("{hours} h") } else { format!("{} min", age_ms / 60_000) };
            BriefLine { kind: "needs_you".into(), text: format!("Still waiting on you: {title} ({age})"), show: None }
        })
        .collect()
}

pub fn build(settings: &Settings, data: &Data, snap: &Snapshot, now: u64) -> Brief {
    build_from(settings, &BriefData::of_v1(data), snap, now)
}

/// [`build`] from what the brief reads, whoever keeps it (v1's `Data`, or v2's stores).
pub fn build_from(settings: &Settings, data: &BriefData, snap: &Snapshot, now: u64) -> Brief {
    let found = items(settings, data, snap, now);
    let next = next_line(settings, data, snap);
    let waiting = Waiting::of(settings, data, snap);
    let asks = asks_of(data, snap).len();
    let text = spoken(&sentences(&found, &waiting, asks, &next).join(" "));
    let mut lines: Vec<BriefLine> = found.into_iter().map(|i| BriefLine { kind: i.kind.to_string(), text: i.text, show: i.show }).collect();
    lines.push(BriefLine { kind: "next".to_string(), text: next, show: None });
    Brief { day: crate::day::iso(crate::day::local(now as i64)), made: now, lines, text }
}

/// The same as stops to walk through: each session that matters (the right tab and section of
/// its panel), then the follow-ups and messages on the manager's page. One or two sentences
/// to say per stop.
pub fn tour(settings: &Settings, data: &Data, snap: &Snapshot) -> Vec<TourStop> {
    tour_from(settings, &BriefData::of_v1(data), snap)
}

/// [`tour`] from what the brief reads, whoever keeps it.
pub fn tour_from(settings: &Settings, data: &BriefData, snap: &Snapshot) -> Vec<TourStop> {
    let now = if snap.at > 0 { snap.at } else { data.heartbeat };
    let found = items(settings, data, snap, now);
    let follow = followups::open(&data.followups).len();
    let messages = super::messages::waiting_in(&data.outbox).len();
    let page = follow > 0 || messages > 0;

    let mut stops: Vec<TourStop> = Vec::new();
    for item in &found {
        let Some(show) = &item.show else { continue };
        let Some(session) = &show.session else { continue };
        // A session that only finished is not visited twice.
        let again = stops.iter().any(|s| s.show.session.as_ref() == Some(session) && (s.show.section == show.section || item.kind == "done"));
        if again {
            continue;
        }
        stops.push(TourStop { show: show.clone(), say: spoken(&item.say) });
    }
    stops.truncate(if page { MAX_STOPS - 1 } else { MAX_STOPS });

    if page {
        let mut parts = Vec::new();
        if follow > 0 {
            parts.push(format!("{} open", if follow == 1 { "1 follow-up is".to_string() } else { format!("{follow} follow-ups are") }));
        }
        if messages > 0 {
            parts.push(format!("{} for your OK", if messages == 1 { "1 message waits".to_string() } else { format!("{messages} messages wait") }));
        }
        let mut say = format!("On my page: {}.", parts.join(" and "));
        if let Some(top) = followups::open(&data.followups).first() {
            say.push(' ');
            say.push_str(&sentence(&followups::line(top)));
        }
        let section = if follow > 0 { "followups" } else { "messages" };
        stops.push(TourStop { show: show_manager(section).unwrap_or_default(), say: spoken(&say) });
    }
    stops
}

/// One line for the status bar and the lens ("3 need you · 2 follow-ups open").
pub fn status_line(data: &Data, snap: &Snapshot) -> String {
    status_line_from(&BriefData::of_v1(data), snap)
}

/// [`status_line`] from what the brief reads, whoever keeps it.
pub fn status_line_from(data: &BriefData, snap: &Snapshot) -> String {
    let need = snap.sessions.iter().filter(|s| s.state == SessionState::NeedsYou).count() + snap.approvals;
    let working = snap.sessions.iter().filter(|s| s.state == SessionState::Working).count();
    let follow = followups::open(&data.followups).len();
    let messages = super::messages::waiting_in(&data.outbox).len();
    let mut parts = Vec::new();
    if need > 0 {
        parts.push(if need == 1 { "1 needs you".to_string() } else { format!("{need} need you") });
    }
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if follow > 0 {
        parts.push(format!("{} open", count(follow, "follow-up", "follow-ups")));
    }
    if messages > 0 {
        parts.push(if messages == 1 { "1 message waits".to_string() } else { format!("{messages} messages wait") });
    }
    if parts.is_empty() { "All quiet".to_string() } else { parts.join(" · ") }
}

/// Minutes since midnight of "09:00" (also "9:00", "9").
pub fn parse_clock(text: &str) -> Option<u32> {
    let text = text.trim();
    let (h, m) = text.split_once(':').unwrap_or((text, "0"));
    let (h, m) = (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Whether the daily brief is due (the time in `Settings::brief_at` passed and today's was
/// not made yet). `local_minutes` is minutes since local midnight, `today` the ISO date.
pub fn due(settings: &Settings, data: &Data, today: &str, local_minutes: u32) -> bool {
    due_after(settings, data.brief.as_ref(), today, local_minutes)
}

/// [`due`] after the last brief made (`last`), whoever keeps it.
pub fn due_after(settings: &Settings, last: Option<&Brief>, today: &str, local_minutes: u32) -> bool {
    if !settings.enabled || last.is_some_and(|b| b.day == today) {
        return false;
    }
    settings.brief_at.as_deref().and_then(parse_clock).is_some_and(|at| local_minutes >= at)
}

#[cfg(test)]
mod tests {
    use super::super::{Blocker, OutMessage, Person, ReviewerInfo, Tier};
    use super::*;

    /// v2's inputs give v1's brief (M56.2): the same outbox, reviews waiting to be told and finished
    /// sessions, kept by v2's stores instead of `data.json`, make the same brief, walk and line;
    /// and it is made once a day by the last one kept.
    #[test]
    fn the_brief_is_the_same_from_v2s_inputs() {
        let (settings, data, snap) = busy();
        let now = snap.at;
        let v2 = BriefData {
            followups: data.followups.clone(),
            outbox: data.outbox.clone(),
            tells: pending_tells(&data),
            finished: data.seen.iter().filter_map(|(k, v)| Some((k.strip_prefix("done|")?.to_string(), v.parse().ok()?))).collect(),
            heartbeat: data.heartbeat,
        };
        assert_eq!(build_from(&settings, &v2, &snap, now), build(&settings, &data, &snap, now));
        assert_eq!(tour_from(&settings, &v2, &snap), tour(&settings, &data, &snap));
        assert_eq!(status_line_from(&v2, &snap), status_line(&data, &snap));
        let made = build_from(&settings, &v2, &snap, now);
        let timed = Settings { brief_at: Some("08:00".into()), ..settings.clone() };
        assert!(due_after(&timed, None, &made.day, 9 * 60));
        assert!(!due_after(&timed, Some(&made), &made.day, 9 * 60), "once a day");
        assert!(!due_after(&timed, None, &made.day, 7 * 60), "not before its time");
        let mut file = BriefFile::default();
        file.finished_at("old", 1_000);
        file.finished_at("new", 1_000 + DONE_FOR + 1);
        assert_eq!(file.finished.keys().collect::<Vec<_>>(), ["new"], "a day's only");
    }

    fn settings() -> Settings {
        Settings {
            enabled: true,
            name: "AI Robin".into(),
            owner: "Robin".into(),
            people: vec![Person { name: "Sam".into(), github: "sam-dev".into(), ..Person::default() }, Person { name: "Alex".into(), github: "alex".into(), ..Person::default() }],
            ..Settings::default()
        }
    }

    fn session(id: &str, title: &str, state: SessionState) -> SessionInfo {
        SessionInfo { id: id.into(), title: title.into(), project_id: "p1".into(), project: "app".into(), state, ..SessionInfo::default() }
    }

    fn pr(number: u64, title: &str) -> PrInfo {
        PrInfo { repo: "acme/app".into(), number, title: title.into(), url: format!("https://github.com/acme/app/pull/{number}"), state: "open".into(), ci: "passing".into(), ..PrInfo::default() }
    }

    fn follow(number: u64, resolved: usize, total: usize, state: FollowState) -> FollowUp {
        FollowUp {
            id: followups::given_id("acme/app", number),
            kind: FollowKind::BlockersGiven,
            state,
            repo: "acme/app".into(),
            number,
            title: "Token refresh".into(),
            login: "sam-dev".into(),
            person: "Sam".into(),
            blockers: (0..total).map(|i| Blocker { thread: format!("t{i}"), text: format!("point {i}"), path: "a.rs".into(), resolved: i < resolved }).collect(),
            created: 10,
            updated: 10,
            ..FollowUp::default()
        }
    }

    /// A day with a bit of everything.
    fn busy() -> (Settings, Data, Snapshot) {
        let mut needs = session("s-need", "Quiz import", SessionState::NeedsYou);
        needs.last_reply = Some("I looked at both.\n\nShould I keep the old column or drop it?\n".into());
        let mut reviewed = session("s-rev", "Relay fix", SessionState::Idle);
        let mut p = pr(12, "Fix the relay.");
        p.reviewers = vec![ReviewerInfo { login: "alex".into(), state: "CHANGES_REQUESTED".into(), ..ReviewerInfo::default() }, ReviewerInfo { login: "robin".into(), state: "COMMENTED".into(), ..ReviewerInfo::default() }];
        reviewed.pr = Some(p);
        let mut failing = session("s-ci", "Servers table", SessionState::Working);
        let mut p = pr(13, "Servers table");
        p.ci = "failing".into();
        failing.pr = Some(p);
        let mut conflict = session("s-conf", "Status filter", SessionState::Idle);
        let mut p = pr(14, "Status filter");
        p.conflicting = true;
        conflict.pr = Some(p);
        let done = session("s-done", "Type check", SessionState::Idle);
        let mut hidden = session("s-off", "Secret work", SessionState::NeedsYou);
        hidden.project_id = "p-off".into();

        let mut settings = settings();
        settings.off_projects.insert("p-off".into());
        let mut data = Data::default();
        data.followups.push(follow(1111, 3, 5, FollowState::Open));
        data.seen.insert(done_key("s-done"), "9000".into());
        let snap = Snapshot {
            at: 10_000,
            me: Some("robin".into()),
            sessions: vec![needs, reviewed, failing, conflict, done, hidden],
            review_asks: vec![ReviewAsk { repo: "acme/app".into(), number: 45, title: "Billing export".into(), author: "mira".into(), url: String::new() }],
            approvals: 0,
            mac_online: true,
        };
        (settings, data, snap)
    }

    #[test]
    fn the_brief_lists_everything_in_order() {
        let (settings, data, snap) = busy();
        let brief = build(&settings, &data, &snap, 10_000);
        assert_eq!(brief.made, 10_000);
        assert_eq!(brief.day, crate::day::iso(crate::day::local(10_000)));
        let got: Vec<(&str, &str)> = brief.lines.iter().map(|l| (l.kind.as_str(), l.text.as_str())).collect();
        assert_eq!(
            got,
            vec![
                ("needs_you", "Quiz import needs you: Should I keep the old column or drop it?"),
                ("review", "Alex asked for changes on #12 (Fix the relay)"),
                ("blocked", "Checks fail on #13 (Servers table)"),
                ("blocked", "#14 (Status filter) has merge conflicts"),
                ("follow_up", "Sam fixed 3 of 5 blockers on #1111"),
                ("review", "mira asks for your review of #45 (Billing export)"),
                ("done", "Type check finished"),
                ("next", "Next I watch #1111 for fixes"),
            ]
        );
        // Where each line leads.
        assert_eq!(brief.lines[0].show, Some(Show { session: Some("s-need".into()), view: None, tab: None, section: Some("reply".into()) }));
        assert_eq!(brief.lines[1].show, Some(Show { session: Some("s-rev".into()), view: None, tab: Some(PanelTab::PullRequest), section: Some("reviews".into()) }));
        assert_eq!(brief.lines[2].show, Some(Show { session: Some("s-ci".into()), view: None, tab: Some(PanelTab::PullRequest), section: Some("checks".into()) }));
        assert_eq!(brief.lines[3].show.as_ref().and_then(|s| s.session.as_deref()), Some("s-conf"));
        assert_eq!(brief.lines[4].show, Some(Show { session: None, view: Some("manager".into()), tab: None, section: Some("followups".into()) }));
        assert_eq!(brief.lines[5].show, None);
        // Nothing of the project that is switched off.
        assert!(!brief.lines.iter().any(|l| l.text.contains("Secret")));
        assert!(!brief.text.contains("Secret"));
    }

    #[test]
    fn the_spoken_brief_is_short_and_starts_with_numbers() {
        let (settings, data, snap) = busy();
        let brief = build(&settings, &data, &snap, 10_000);
        assert!(brief.text.starts_with("1 session needs you: Quiz import."), "{}", brief.text);
        assert!(brief.text.contains("Alex asked for changes on PR 12"), "{}", brief.text);
        assert!(brief.text.contains("Sam fixed 3 of 5 blockers on PR 1111."), "{}", brief.text);
        assert!(brief.text.ends_with("Next I watch PR 1111 for fixes."), "{}", brief.text);
        assert!(!brief.text.contains('#'));
        let n = brief.text.matches(". ").count() + 1;
        assert!((3..=6).contains(&n), "{n} sentences: {}", brief.text);
        assert!(brief.text.contains("2 things are blocked."), "{}", brief.text);

        // With room to spare, the first of several is named.
        let mut fewer = snap.clone();
        fewer.sessions.retain(|s| s.id == "s-ci" || s.id == "s-conf");
        fewer.review_asks.clear();
        let text = build(&settings, &Data::default(), &fewer, 10_000).text;
        assert_eq!(text, "Nothing needs you right now. 2 things are blocked. First: Checks fail on PR 13 (Servers table). Next I keep watching your sessions and pull requests, and tell you when something changes.");

        // A quiet day still makes three sentences.
        let quiet = build(&settings, &Data::default(), &Snapshot::default(), 10_000);
        assert_eq!(quiet.text, "Nothing needs you right now. No reviews came in and nothing is blocked. Next I keep watching your sessions and pull requests, and tell you when something changes.");
        assert_eq!(quiet.lines.len(), 1);
        assert_eq!(quiet.lines[0].kind, "next");
    }

    #[test]
    fn many_things_still_fit_in_six_sentences() {
        let (settings, mut data, mut snap) = busy();
        data.followups.push(follow(1120, 0, 2, FollowState::Open));
        let mut second = session("s-rev2", "Other fix", SessionState::NeedsYou);
        let mut p = pr(15, "Other fix");
        p.ci = "failing".into();
        p.reviewers = vec![ReviewerInfo { login: "sam-dev".into(), state: "CHANGES_REQUESTED".into(), ..ReviewerInfo::default() }];
        second.pr = Some(p);
        snap.sessions.push(second);
        snap.approvals = 2;
        let brief = build(&settings, &data, &snap, 10_000);
        assert!(brief.text.starts_with("4 things need you: 2 sessions wait for an answer and 2 commands wait for your OK. "), "{}", brief.text);
        let n = brief.text.matches(". ").count() + 1;
        assert!((3..=6).contains(&n), "{n} sentences: {}", brief.text);
        assert!(brief.text.ends_with("Next I watch PR 1111 and PR 1120 for fixes."), "{}", brief.text);
        assert!(brief.text.contains("3 things are blocked. 2 follow-ups are still open with others. 1 pull request asks for your review."), "{}", brief.text);
    }

    fn waiting_message(id: &str, to: &str) -> OutMessage {
        OutMessage { id: id.into(), to: to.into(), body: format!("A word about the relay for {to}.\nAI Robin, on behalf of Robin"), tier: Tier::NeedsOk, created: 5, updated: 5, ..OutMessage::default() }
    }

    #[test]
    fn the_opening_counts_everything_that_waits_for_the_person() {
        let settings = settings();
        // No session waits, but two messages do, and Sam says one pull request is fixed.
        let mut data = Data::default();
        data.outbox = vec![waiting_message("o1", "Sam"), waiting_message("o2", "Alex")];
        data.followups.push(follow(88, 2, 2, FollowState::Fixed));
        data.followups.push(follow(1120, 0, 2, FollowState::Open));
        let idle = Snapshot { at: 500, me: Some("robin".into()), sessions: vec![session("s1", "Relay fix", SessionState::Idle)], ..Snapshot::default() };
        let text = build(&settings, &data, &idle, 500).text;
        assert_eq!(text, "3 things need you: 2 messages wait for your OK and Sam says PR 88 is fixed. Sam has 2 open blockers on PR 1120. Next I watch PR 1120 for fixes.");
        // The same count as the badge.
        let badge = super::super::harness::Harness { data: data.clone(), prev: None, dirty: false, ..super::super::harness::Harness::default() }.needs_you(&idle);
        assert_eq!(badge, 3);

        // With a session and a command on top, each kind is named once, shortly.
        let mut busy = idle.clone();
        busy.sessions.push(session("s2", "Quiz import", SessionState::NeedsYou));
        busy.approvals = 1;
        let text = build(&settings, &data, &busy, 500).text;
        assert!(text.starts_with("5 things need you: Quiz import waits for an answer, 1 command waits for your OK, 2 messages wait for your OK and Sam says PR 88 is fixed. "), "{text}");
        assert!(!text.contains("Nothing needs you"));

        // One kind only: the sentence says it by itself.
        let mut one = Data::default();
        one.outbox = vec![waiting_message("o1", "Sam")];
        assert!(build(&settings, &one, &idle, 500).text.starts_with("1 message waits for your OK. "));
        let mut one = Data::default();
        one.followups.push(follow(88, 2, 2, FollowState::Fixed));
        one.followups.push(follow(89, 1, 1, FollowState::Fixed));
        let text = build(&settings, &one, &idle, 500).text;
        assert!(text.starts_with("2 follow-ups are marked fixed. "), "{text}");
        // They were said already: not once more as "open".
        assert!(!text.contains("follow-ups are still open"), "{text}");
        let only = Snapshot { approvals: 2, ..Snapshot::default() };
        assert!(build(&settings, &Data::default(), &only, 500).text.starts_with("2 commands wait for your OK. "));

        // Only when nothing at all waits.
        let mut none = Data::default();
        none.followups.push(follow(1120, 0, 2, FollowState::Open));
        assert!(build(&settings, &none, &idle, 500).text.starts_with("Nothing needs you right now. "));
    }

    #[test]
    fn a_fixed_follow_up_and_an_approval_are_told() {
        let settings = settings();
        let mut data = Data::default();
        data.followups.push(follow(1111, 5, 5, FollowState::Fixed));
        let mut s = session("s1", "Relay fix", SessionState::Idle);
        let mut p = pr(12, "Fix the relay");
        p.reviewers = vec![ReviewerInfo { login: "alex".into(), state: "APPROVED".into(), ..ReviewerInfo::default() }, ReviewerInfo { login: "sam-dev".into(), state: "APPROVED".into(), ..ReviewerInfo::default() }];
        s.pr = Some(p);
        let snap = Snapshot { at: 500, me: Some("robin".into()), sessions: vec![s], ..Snapshot::default() };
        let brief = build(&settings, &data, &snap, 500);
        let texts: Vec<&str> = brief.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["Alex and Sam approved #12 (Fix the relay): ready for you to merge", "Sam fixed all 5 blockers on #1111: ready for you to look again", "Next I keep watching your sessions and pull requests, and tell you when something changes"]);

        // Not "ready to merge" while someone asks for changes or the checks fail.
        let mut snap2 = snap.clone();
        if let Some(pr) = snap2.sessions[0].pr.as_mut() {
            pr.ci = "failing".into();
        }
        assert!(!build(&settings, &data, &snap2, 500).lines.iter().any(|l| l.text.contains("ready for you to merge")));
    }

    #[test]
    fn a_teammates_pull_request_in_a_normal_session_is_not_in_the_brief() {
        let (settings, data, mut snap) = busy();
        for s in snap.sessions.iter_mut() {
            if let Some(pr) = s.pr.as_mut() {
                pr.author = "sam-dev".into();
            }
        }
        let brief = build(&settings, &data, &snap, 10_000);
        assert!(!brief.lines.iter().any(|l| l.text.contains("#12") || l.text.contains("#13") || l.text.contains("#14")), "{:?}", brief.lines);
        // The person's own are, with the login in any case.
        for s in snap.sessions.iter_mut() {
            if let Some(pr) = s.pr.as_mut() {
                pr.author = "Robin".into();
            }
        }
        let brief = build(&settings, &data, &snap, 10_000);
        assert!(brief.lines.iter().any(|l| l.text == "Alex asked for changes on #12 (Fix the relay)"));
        assert!(brief.lines.iter().any(|l| l.text == "Checks fail on #13 (Servers table)"));
    }

    #[test]
    fn a_review_the_person_closed_is_not_told_again() {
        let (settings, mut data, snap) = busy();
        data.followups.push(FollowUp { id: followups::received_id("acme/app", 12, "alex"), kind: FollowKind::ReviewReceived, state: FollowState::Done, repo: "acme/app".into(), number: 12, login: "alex".into(), ..FollowUp::default() });
        let brief = build(&settings, &data, &snap, 10_000);
        assert!(!brief.lines.iter().any(|l| l.text.contains("Alex asked for changes")));
    }

    #[test]
    fn a_review_received_without_its_session_is_a_follow_up_line() {
        let settings = settings();
        let mut data = Data::default();
        data.followups.push(FollowUp { id: followups::received_id("acme/app", 12, "alex"), kind: FollowKind::ReviewReceived, state: FollowState::Open, repo: "acme/app".into(), number: 12, login: "alex".into(), person: "Alex".into(), note: followups::NOTE_CHANGES.into(), session: Some("gone".into()), ..FollowUp::default() });
        let brief = build(&settings, &data, &Snapshot::default(), 100);
        assert_eq!(brief.lines[0].kind, "follow_up");
        assert_eq!(brief.lines[0].text, "Alex asked for changes on your #12");
        assert_eq!(brief.lines[0].show.as_ref().and_then(|s| s.view.as_deref()), Some("manager"));
    }

    #[test]
    fn the_walk_visits_what_matters_and_ends_on_the_managers_page() {
        let (settings, data, snap) = busy();
        let stops = tour(&settings, &data, &snap);
        let where_: Vec<(Option<&str>, Option<&str>)> = stops.iter().map(|s| (s.show.session.as_deref().or(s.show.view.as_deref()), s.show.section.as_deref())).collect();
        assert_eq!(where_, vec![(Some("s-need"), Some("reply")), (Some("s-rev"), Some("reviews")), (Some("s-ci"), Some("checks")), (Some("s-conf"), None), (Some("s-done"), Some("reply")), (Some("manager"), Some("followups"))]);
        assert_eq!(stops[1].show.tab, Some(PanelTab::PullRequest));
        assert_eq!(stops[0].say, "Quiz import stopped and waits for you. Should I keep the old column or drop it?");
        assert_eq!(stops[1].say, "Alex asked for changes on PR 12. Here is the review.");
        assert_eq!(stops[5].say, "On my page: 1 follow-up is open. Sam fixed 3 of 5 blockers on PR 1111.");
        for stop in &stops {
            assert!(!stop.say.is_empty() && !stop.say.contains('#'));
        }
    }

    #[test]
    fn the_walk_has_at_most_eight_stops_and_keeps_the_page_last() {
        let (settings, mut data, mut snap) = busy();
        for i in 0..12 {
            snap.sessions.push(session(&format!("n{i}"), &format!("Extra {i}"), SessionState::NeedsYou));
        }
        let stops = tour(&settings, &data, &snap);
        assert_eq!(stops.len(), 8);
        assert_eq!(stops[7].show.view.as_deref(), Some("manager"));
        // Without anything on the page, all eight are sessions.
        data.followups.clear();
        let stops = tour(&settings, &data, &snap);
        assert_eq!(stops.len(), 8);
        assert!(stops.iter().all(|s| s.show.session.is_some()));
        // Nothing going on: no walk.
        assert!(tour(&settings, &Data::default(), &Snapshot::default()).is_empty());
    }

    #[test]
    fn the_status_line_counts() {
        let (_, data, snap) = busy();
        // The app only puts managed projects in a snapshot; the line counts what it is given.
        assert_eq!(status_line(&data, &snap), "2 need you · 1 working · 1 follow-up open");
        assert_eq!(status_line(&Data::default(), &Snapshot::default()), "All quiet");
        let one = Snapshot { approvals: 1, ..Snapshot::default() };
        assert_eq!(status_line(&Data::default(), &one), "1 needs you");
    }

    #[test]
    fn what_still_waits_is_in_the_brief_with_its_age() {
        let lines = waiting_lines(&[("Fix login needs you".into(), 6 * 3_600_000), ("PR #42 reviewed".into(), 50 * 3_600_000), ("Ticket ENG-1".into(), 5 * 60_000)]);
        assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["Still waiting on you: Fix login needs you (6 h)", "Still waiting on you: PR #42 reviewed (2 days)", "Still waiting on you: Ticket ENG-1 (5 min)"]);
        assert!(lines.iter().all(|l| l.kind == "needs_you"));
    }

    #[test]
    fn the_brief_is_due_once_a_day_after_its_time() {
        let mut settings = settings();
        let mut data = Data::default();
        assert!(!due(&settings, &data, "2026-11-16", 8 * 60 + 59));
        assert!(due(&settings, &data, "2026-11-16", 9 * 60));
        assert!(due(&settings, &data, "2026-11-16", 17 * 60));
        data.brief = Some(Brief { day: "2026-11-15".into(), ..Brief::default() });
        assert!(due(&settings, &data, "2026-11-16", 9 * 60), "yesterday's does not count");
        data.brief = Some(Brief { day: "2026-11-16".into(), ..Brief::default() });
        assert!(!due(&settings, &data, "2026-11-16", 9 * 60));
        data.brief = None;
        settings.brief_at = None;
        assert!(!due(&settings, &data, "2026-11-16", 23 * 60));
        settings.brief_at = Some("soon".into());
        assert!(!due(&settings, &data, "2026-11-16", 23 * 60));
        settings.brief_at = Some("7:30".into());
        assert!(due(&settings, &data, "2026-11-16", 7 * 60 + 30));
        settings.enabled = false;
        assert!(!due(&settings, &data, "2026-11-16", 23 * 60));
    }

    #[test]
    fn clock_times() {
        assert_eq!(parse_clock("09:00"), Some(540));
        assert_eq!(parse_clock(" 9 "), Some(540));
        assert_eq!(parse_clock("23:59"), Some(1439));
        assert_eq!(parse_clock("24:00"), None);
        assert_eq!(parse_clock("9:75"), None);
        assert_eq!(parse_clock(""), None);
    }

    #[test]
    fn pending_tells_are_read_back_from_their_keys() {
        let mut data = Data::default();
        data.seen.insert(tell_key("ses|odd:id", "acme/app", 12, "Alex"), "pending:CHANGES_REQUESTED".into());
        data.seen.insert(tell_key("s2", "acme/app", 13, "mira"), "told:COMMENTED".into());
        data.seen.insert("tell|broken".into(), "pending:X".into());
        data.seen.insert("zzz".into(), "pending:X".into());
        let pending = pending_tells(&data);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].session, "ses|odd:id");
        assert_eq!((pending[0].repo.as_str(), pending[0].number, pending[0].reviewer.as_str(), pending[0].state.as_str()), ("acme/app", 12, "alex", "CHANGES_REQUESTED"));
    }

    #[test]
    fn spoken_numbers() {
        assert_eq!(spoken("Sam fixed 3 of 5 blockers on #1111, see #12."), "Sam fixed 3 of 5 blockers on PR 1111, see PR 12.");
        assert_eq!(spoken("C# and #hash"), "C# and #hash");
    }
}
