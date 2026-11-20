//! The chat's answers from code (M56.2): the plain questions it answers at once, without its
//! brain, on v2's own stores. They were v1's harness's `quick_answer`; at v2 nothing asks the
//! harness for an answer.
//!
//! - what needs the person, what is blocked, the follow-ups, the status, today's update: from the
//!   snapshot and the brief's v2 inputs ([`BriefData`]: the work ledger's follow-ups, v2's outbox);
//! - "done with #1111", "drop #1111": a follow-up of the work ledger closed ([`close`]);
//! - "remember that …", "forget …", "what do you remember about …": memory (memstore), from the
//!   person's own words ([`remember`], [`forget`], [`recall`]), never v1's notes.
//!
//! [`asked`] says which of them a question is; the rest is the brain's.

use crate::manager::brief::{self, BriefData};
use crate::manager::followups;
use crate::manager::{Action, FollowKind, FollowState, FollowUp, Settings, Show, Snapshot};
use crate::memstore::{MemFile, Memory, Remembered, Status};
use crate::words::WordsRecord;

/// An answer from code: what it says, and what it does in the window (a walk, a place shown).
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub say: String,
    pub actions: Vec<Action>,
}

impl Answer {
    fn said(text: String) -> Answer {
        Answer { say: brief::spoken(&text), actions: Vec::new() }
    }
}

/// Which plain question it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    /// "What do you remember (about billing)?": None for everything.
    Recall(Option<String>),
    /// "Done with #1111" (`drop`: "drop #1111", "stop tracking PR 12").
    Close { number: u64, drop: bool },
    /// "Remember that …": the fact, as asked.
    Remember(String),
    /// "Forget …": what to forget, as asked.
    Forget(String),
    /// Today's update.
    Brief,
    FollowUps,
    NeedsYou,
    Blocked,
    Status,
}

impl Asked {
    /// It reads or writes memory (off the main thread).
    pub fn is_memory(&self) -> bool {
        matches!(self, Asked::Recall(_) | Asked::Remember(_) | Asked::Forget(_))
    }
}

/// Which plain question `question` is (None: the brain's). Nothing while the manager is off.
pub fn asked(settings: &Settings, question: &str) -> Option<Asked> {
    if !settings.enabled {
        return None;
    }
    let (asked, low) = plain_question(settings, question);
    if low.is_empty() {
        return None;
    }
    let words = low.split_whitespace().count();

    if let Some(topic) = after(&asked, &low, &["what do you remember", "what did you remember", "what have you remembered", "do you remember", "what is in your memory", "what's in your memory", "show me your memory", "show your memory"]) {
        let topic = topic.trim_end_matches(['?', '!', '.']).trim();
        let topic = ["about ", "of ", "on "].iter().find_map(|p| topic.strip_prefix(p)).unwrap_or(topic).trim();
        let general = topic.is_empty() || matches!(topic.to_lowercase().as_str(), "anything" | "everything" | "something" | "so far");
        return Some(Asked::Recall((!general).then(|| topic.to_string())));
    }

    // ("close #12" alone is the brain's: it may mean the pull request, which is not the manager's
    // to close.)
    let closes = ["done with", "i am done with", "i'm done with", "im done with", "finished with", "close the follow", "close follow"];
    let drops = ["drop #", "drop pr ", "drop the follow", "stop tracking", "stop following", "stop watching", "do not follow", "don't follow"];
    let is_close = closes.iter().any(|p| low.starts_with(p)) || low.ends_with(" is done") || low.ends_with(" is finished") || low.ends_with(" as done");
    let is_drop = drops.iter().any(|p| low.starts_with(p));
    if let Some(number) = number_in(&low).filter(|_| (is_close || is_drop) && words <= 9) {
        return Some(Asked::Close { number, drop: is_drop });
    }

    if let Some(fact) = after(&asked, &low, &["remember that ", "remember this: ", "remember", "note that ", "keep in mind that ", "keep in mind", "don't forget that ", "do not forget that "]) {
        let fact = fact.trim();
        if fact.is_empty() || matches!(fact.to_lowercase().trim_end_matches(['.', '!']), "that" | "this" | "it" | "me") || fact.ends_with('?') {
            return None;
        }
        return Some(Asked::Remember(fact.to_string()));
    }
    if let Some(what) = after(&asked, &low, &["forget that ", "forget about ", "forget"]) {
        let what = what.trim().trim_end_matches(['.', '!', '?']).trim();
        if what.is_empty() || matches!(what.to_lowercase().as_str(), "it" | "that" | "this" | "about it" | "everything" | "all" | "all of it" | "me") {
            return None;
        }
        return Some(Asked::Forget(what.to_string()));
    }

    let wants_brief = matches!(low.as_str(), "brief" | "briefing" | "update" | "the update" | "my update")
        || any_in(&low, &["today's update", "todays update", "today update", "daily update", "daily brief", "morning brief", "morning update", "the brief", "brief me", "catch me up", "give me an update", "give me the update", "give me my update", "what's new", "whats new", "what is new", "what did i miss", "walk me through today", "walk me through it", "walk me through everything"]);
    if wants_brief && words <= 12 {
        return Some(Asked::Brief);
    }

    // (Asking for a new follow-up, "follow up with Sam on …", is the brain's.)
    let adds = ["follow up", "follow-up with", "add", "create", "make", "track", "keep an eye", "watch", "start"].iter().any(|p| low.starts_with(p));
    if words <= 10 && !adds && any_in(&low, &["follow-up", "followup", "follow up", "blockers", "what am i waiting", "who am i waiting", "who owes me", "what is open with", "what's open with"]) {
        return Some(Asked::FollowUps);
    }
    if words <= 10 && any_in(&low, &["what needs me", "what needs my", "who needs me", "what do i need to", "what do i have to", "anything need me", "anything needs me", "anything for me", "waiting for me", "waiting on me", "waits for me", "what should i do", "what should i look at", "needs you", "need me now", "what needs you"]) {
        return Some(Asked::NeedsYou);
    }
    if words <= 8 && any_in(&low, &["blocked", "stuck", "what is failing", "what's failing", "what fails", "anything failing", "anything red"]) {
        return Some(Asked::Blocked);
    }
    // Only the plain question, not "the status of the migration".
    let status = low.starts_with("status") || low.ends_with("status") || any_in(&low, &["status update", "status report"]);
    let about_one = any_in(&low, &["status of", "status on", "status for", "status with", "status page"]);
    if words <= 6 && !about_one && (status || any_in(&low, &["how are things", "how is it going", "how's it going", "what's going on", "what is going on", "what are my sessions doing", "what are the sessions doing", "where are we", "where do we stand", "overview"])) {
        return Some(Asked::Status);
    }
    None
}

/// What the chat answers from the snapshot and the brief's inputs: today's update (with its walk),
/// the follow-ups, what needs the person, what is blocked, the status. None for the questions that
/// are not about them (memory, closing a follow-up).
pub fn answer(asked: &Asked, settings: &Settings, data: &BriefData, snap: &Snapshot, now: u64) -> Option<Answer> {
    let snap = managed(settings, snap);
    let open = followups::open(&data.followups);
    match asked {
        Asked::Brief => {
            let made = brief::build_from(settings, data, &snap, now);
            let stops = brief::tour_from(settings, data, &snap);
            Some(Answer { say: made.text, actions: if stops.is_empty() { Vec::new() } else { vec![Action::Tour { stops }] } })
        }
        Asked::FollowUps => {
            if open.is_empty() {
                return Some(Answer::said("No follow-ups are open.".into()));
            }
            let lines: Vec<String> = open.iter().map(|f| followups::line(f)).collect();
            let head = if lines.len() == 1 { "1 follow-up is open.".to_string() } else { format!("{} follow-ups are open.", lines.len()) };
            Some(Answer { say: brief::spoken(&format!("{head} {}", read_out(&lines, 5))), actions: brief::show_manager("followups").map(|show| Action::Show { show }).into_iter().collect() })
        }
        Asked::NeedsYou => {
            let made = brief::build_from(settings, data, &snap, now);
            let mut lines: Vec<(String, Option<Show>)> = made.lines.iter().filter(|l| matches!(l.kind.as_str(), "needs_you" | "review" | "message")).map(|l| (l.text.clone(), l.show.clone())).collect();
            for f in open.iter().filter(|f| f.state == FollowState::Fixed) {
                lines.push((followups::line(f), followups::show_follow(f, Some(&snap))));
            }
            if lines.is_empty() {
                return Some(Answer::said("Nothing needs you right now.".into()));
            }
            let head = if lines.len() == 1 { "1 thing needs you.".to_string() } else { format!("{} things need you.", lines.len()) };
            let texts: Vec<String> = lines.iter().map(|(t, _)| t.clone()).collect();
            let first = lines.iter().find_map(|(_, show)| show.clone());
            Some(Answer { say: brief::spoken(&format!("{head} {}", read_out(&texts, 5))), actions: first.map(|show| Action::Show { show }).into_iter().collect() })
        }
        Asked::Blocked => {
            let made = brief::build_from(settings, data, &snap, now);
            let blocked: Vec<&crate::manager::BriefLine> = made.lines.iter().filter(|l| l.kind == "blocked").collect();
            let waiting: Vec<String> = open.iter().filter(|f| f.state == FollowState::Open && f.kind != FollowKind::ReviewReceived).map(|f| followups::line(f)).collect();
            if blocked.is_empty() && waiting.is_empty() {
                return Some(Answer::said("Nothing is blocked.".into()));
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
            Some(Answer { say: brief::spoken(&parts.join(" ")), actions: first.map(|show| Action::Show { show }).into_iter().collect() })
        }
        Asked::Status => Some(Answer::said(status(data, &snap))),
        Asked::Recall(_) | Asked::Close { .. } | Asked::Remember(_) | Asked::Forget(_) => None,
    }
}

/// "Done with #1111" or "drop #1111": the open follow-up of that pull request closed (or dropped)
/// in `follow_ups` (the work ledger's), and what to say.
pub fn close(follow_ups: &mut [FollowUp], number: u64, drop: bool, now: u64) -> String {
    match followups::close(follow_ups, &format!("#{number}"), drop, now) {
        Some(_) if drop => format!("I no longer follow #{number}."),
        Some(f) => format!("Done. The follow-up on #{number} is closed{}.", if f.title.trim().is_empty() { String::new() } else { format!(": {}", crate::manager::gh::one_line(&f.title, 60)) }),
        None => format!("I have no open follow-up on #{number}."),
    }
}

/// "Remember that …", from the person's own words (`record`): a fact in their profile, confirmed
/// when they typed it, else a proposal that waits for their tick (memory's own rule).
pub fn remember(memory: &Memory, record: &WordsRecord, fact: &str) -> String {
    let fact = fact.trim();
    match memory.remember(record, fact, fact, &MemFile::Profile, None) {
        Ok(Remembered::Entry { .. }) => format!("I will remember that: {}", with_stop(fact)),
        Ok(Remembered::Proposed { why, .. }) => format!("I proposed it for memory; it waits for your tick ({why}): {}", with_stop(fact)),
        Err(err) => format!("I could not remember that: {err}"),
    }
}

/// "Forget …": the one entry of memory whose words hold what was said (or that it holds), let go
/// with a tombstone. None or more than one: nothing is forgotten, and it says so.
pub fn forget(memory: &Memory, what: &str) -> String {
    let found = match entries(memory) {
        Ok(found) => found,
        Err(err) => return format!("Memory could not be read, so I forgot nothing: {err}"),
    };
    let low = what.to_lowercase();
    let matches: Vec<&(MemFile, String, String)> = found.iter().filter(|(_, _, text)| {
        let text = text.to_lowercase();
        text.contains(&low) || low.contains(text.trim_end_matches('.'))
    }).collect();
    match matches.as_slice() {
        [] => "I am not sure which note you mean, so I forgot nothing.".to_string(),
        [(_, id, text)] => match memory.forget(&[id.as_str()], false) {
            Ok(_) => format!("Forgotten: {}", with_stop(text)),
            Err(err) => format!("I could not forget it: {err}"),
        },
        many => format!("{} notes hold that, so I forgot nothing; say which: {}", many.len(), read_said(&many.iter().take(3).map(|(_, _, t)| t.clone()).collect::<Vec<_>>(), 3)),
    }
}

/// "What do you remember (about …)?": memory's best matches for the topic (every project's, as the
/// manager reads them), or, for everything, what the person told it (their profile).
pub fn recall(memory: &Memory, topic: Option<&str>) -> String {
    let snap = match memory.snapshot() {
        Ok(snap) => snap,
        Err(err) => return format!("Memory could not be read: {err}"),
    };
    match topic {
        Some(topic) => {
            let scope = crate::memstore::search::Scope { project: None, employer: None, work: None, history: false, hidden: Vec::new() };
            let hits = crate::memstore::search::search(&snap, topic, &scope, 5).unwrap_or_default();
            let texts: Vec<String> = hits.iter().flat_map(|hit| hit.entries.iter().map(|e| e.text.clone())).take(5).collect();
            if texts.is_empty() { format!("I remember nothing about {topic}.") } else { format!("About {topic} I remember: {}", read_said(&texts, 5)) }
        }
        None => {
            let texts: Vec<String> = snap.doc(&MemFile::Profile).map(|doc| doc.entries().filter(|e| e.header.status != Status::LetGo).map(|e| e.text.clone()).collect()).unwrap_or_default();
            match texts.len() {
                0 => "You have not told me anything to remember yet.".to_string(),
                n => format!("You told me {}. {}", if n == 1 { "1 thing".to_string() } else { format!("{n} things") }, read_said(&texts, 8)),
            }
        }
    }
}

/// Every entry of memory that is not let go: its file, its id and its words.
fn entries(memory: &Memory) -> Result<Vec<(MemFile, String, String)>, crate::memstore::MemoryError> {
    let snap = memory.snapshot()?;
    let mut out = Vec::new();
    for file in snap.files()? {
        let Ok(doc) = snap.doc(&file) else { continue };
        for entry in doc.entries().filter(|e| e.header.status != Status::LetGo) {
            out.push((file.clone(), entry.header.id.clone(), entry.text.clone()));
        }
    }
    Ok(out)
}

/// The sessions and what is open, in a few sentences.
fn status(data: &BriefData, snap: &Snapshot) -> String {
    use crate::manager::SessionState;
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
    let open = followups::open(&data.followups);
    let fixed = open.iter().filter(|f| f.state == FollowState::Fixed).count();
    if !open.is_empty() {
        let ready = if fixed == 0 { String::new() } else { format!(", {fixed} ready for you to look again") };
        out.push(format!("{} open{ready}.", if open.len() == 1 { "1 follow-up is".to_string() } else { format!("{} follow-ups are", open.len()) }));
    }
    let waiting = crate::manager::messages::Outbox { messages: data.outbox.clone() }.waiting().len();
    if waiting > 0 {
        out.push(format!("{} for your OK.", if waiting == 1 { "1 message waits".to_string() } else { format!("{waiting} messages wait") }));
    }
    if out.len() == 1 && running > 0 && needs == 0 {
        out.push("Nothing needs you right now.".to_string());
    }
    out.join(" ")
}

/// The snapshot without the sessions of projects the manager was told to leave alone.
fn managed(settings: &Settings, snap: &Snapshot) -> Snapshot {
    let mut snap = snap.clone();
    snap.sessions.retain(|s| !settings.off_projects.contains(&s.project_id));
    snap
}

/// The question without a greeting or the manager's name in front, as asked (for what is to be
/// remembered) and in lower case without its last punctuation (to see what is asked).
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

/// The words as they were said, with one full stop at the end ("iOS" stays "iOS").
fn with_stop(text: &str) -> String {
    let t = text.trim().trim_end_matches(['.', ':', ';', ',']).trim_end();
    if t.ends_with(['?', '!']) { t.to_string() } else { format!("{t}.") }
}

/// A few lines read out one after the other, with how many more there are.
fn read_out(lines: &[String], max: usize) -> String {
    read_with(lines, max, brief::sentence)
}

/// Memory's words read out as they were said ("iOS" is not made "IOS").
fn read_said(lines: &[String], max: usize) -> String {
    read_with(lines, max, with_stop)
}

fn read_with(lines: &[String], max: usize, each: fn(&str) -> String) -> String {
    let mut out: Vec<String> = lines.iter().take(max).map(|l| each(l)).collect();
    if lines.len() > max {
        out.push(format!("And {} more.", lines.len() - max));
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::{FollowUp, SessionInfo, SessionState};

    fn settings() -> Settings {
        Settings { enabled: true, name: "AI Robin".into(), owner: "Robin".into(), brief_at: None, ..Settings::default() }
    }

    /// The plain questions are told apart as v1's harness told them; the rest is the brain's.
    #[test]
    fn the_plain_questions() {
        let s = settings();
        assert_eq!(asked(&s, "Hey Robin, remember that iOS builds are Mira's"), Some(Asked::Remember("iOS builds are Mira's".into())));
        assert_eq!(asked(&s, "remember that"), None);
        assert_eq!(asked(&s, "forget that iOS builds are Mira's"), Some(Asked::Forget("iOS builds are Mira's".into())));
        assert_eq!(asked(&s, "what do you remember about billing?"), Some(Asked::Recall(Some("billing".into()))));
        assert_eq!(asked(&s, "What do you remember?"), Some(Asked::Recall(None)));
        assert_eq!(asked(&s, "done with #1111"), Some(Asked::Close { number: 1111, drop: false }));
        assert_eq!(asked(&s, "stop tracking PR 12"), Some(Asked::Close { number: 12, drop: true }));
        assert_eq!(asked(&s, "close #12"), None, "the pull request may be meant");
        assert_eq!(asked(&s, "today's update"), Some(Asked::Brief));
        assert_eq!(asked(&s, "what am I waiting on?"), Some(Asked::FollowUps));
        assert_eq!(asked(&s, "what needs me?"), Some(Asked::NeedsYou));
        assert_eq!(asked(&s, "what is blocked"), Some(Asked::Blocked));
        assert_eq!(asked(&s, "status"), Some(Asked::Status));
        assert_eq!(asked(&s, "the status of the migration"), None);
        assert_eq!(asked(&s, "go to the session Fix the typo"), None);
        assert_eq!(asked(&Settings { enabled: false, ..settings() }, "status"), None);
    }

    /// The answers read v2's inputs: the follow-ups and the outbox of the brief's data.
    #[test]
    fn answers_from_v2_inputs() {
        let s = settings();
        let snap = Snapshot { sessions: vec![SessionInfo { id: "s1".into(), title: "Fix".into(), state: SessionState::NeedsYou, ..SessionInfo::default() }], ..Snapshot::default() };
        let follow = FollowUp { id: "f1".into(), repo: "acme/app".into(), number: 1111, title: "Token refresh".into(), state: FollowState::Fixed, ..FollowUp::default() };
        let data = BriefData { followups: vec![follow], ..BriefData::default() };
        let status = answer(&Asked::Status, &s, &data, &snap, 1_800_000_000).unwrap().say;
        assert!(status.contains("1 session: 1 needs you.") && status.contains("1 follow-up is open, 1 ready for you to look again."), "{status}");
        let none = answer(&Asked::FollowUps, &s, &BriefData::default(), &snap, 1_800_000_000).unwrap().say;
        assert_eq!(none, "No follow-ups are open.");
        assert_eq!(answer(&Asked::Remember("x".into()), &s, &data, &snap, 0), None, "memory is answered on its own");
    }

    /// "Done with #1111" closes the ledger's follow-up; asked again, there is none open.
    #[test]
    fn a_follow_up_is_closed() {
        let mut list = vec![FollowUp { id: "f1".into(), repo: "acme/app".into(), number: 1111, title: "Token refresh".into(), ..FollowUp::default() }];
        assert_eq!(close(&mut list, 1111, false, 10), "Done. The follow-up on #1111 is closed: Token refresh.");
        assert_eq!(close(&mut list, 1111, false, 11), "I have no open follow-up on #1111.");
    }

    /// "Remember that …" writes memory (the person's profile), "what do you remember" reads it back,
    /// and "forget …" lets it go: memstore, not v1's notes.
    #[test]
    fn remember_recall_forget_in_memory() {
        let _scratch = crate::memstore::tests_support::Scratch::new();
        let memory = Memory::open();
        let record = crate::memstore::tests_support::chat("Remember that iOS builds are Mira's");
        let said = remember(&memory, &record, "iOS builds are Mira's");
        assert_eq!(said, "I will remember that: iOS builds are Mira's.");
        assert!(recall(&memory, None).contains("iOS builds are Mira's"), "{}", recall(&memory, None));
        assert_eq!(forget(&memory, "dragons"), "I am not sure which note you mean, so I forgot nothing.");
        assert_eq!(forget(&memory, "iOS builds are Mira's"), "Forgotten: iOS builds are Mira's.");
        assert!(!recall(&memory, None).contains("iOS builds"));
    }
}
