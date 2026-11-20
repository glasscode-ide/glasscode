//! Messages to teammates: drafted by the manager, signed, sorted by how much say the person
//! keeps over them, and sent only when allowed.
//!
//! The sorting (`tier`) is a list of plain rules, not a model, so it is the same every time
//! and can be read. It errs one way only: a text is `Routine` when it is recognisably a short
//! fact about work and nothing else. That is checked twice over: nothing in it may sound like
//! a promise, a date, an opinion and so on (`NEEDS_OK`), and every word of it must be a plain
//! word about work (`PLAIN`), a number, a name or a quoted title. Everything else waits for
//! the person, so a routine ping is best written like "The fix for #1111 is pushed".

use super::{Data, MsgState, OutMessage, Settings, Tier};

/// The same text to the same person is not proposed or sent twice within this time.
pub const QUIET_SECS: u64 = 24 * 3600;

/// A routine ping is at most this long.
const MAX_CHARS: usize = 200;
const MAX_WORDS: usize = 32;
const MAX_SENTENCES: usize = 2;

// ----- the rules --------------------------------------------------------------------------------

/// Asking for an approval, or saying there is none, approves nothing. These are taken out
/// before looking for an approval (and make the text wait for the person).
const NOT_APPROVING: &[&str] = &[
    "your approval", "needs approval", "need approval", "waiting for approval", "awaiting approval", "for approval", "not approved", "not yet approved", "not approve", "can you approve", "could you approve", "would you approve", "please approve", "not merge", "not close", "not accept", "not accepted", "acceptance test", "acceptance tests",
    "acceptance criteria",
];

/// Saying one of these approves, merges, closes or accepts in the person's name.
const NEVER: &[&str] = &[
    "lgtm", "looks good", "look good", "looking good", "good to go", "good to merge", "ship it", "shipit", "sign off", "signed off", "signing off", "signoff", "green light", "thumbs up", "stamp it", "stamped", "you have my ok", "you have my go", "request changes", "requesting changes", "you can merge", "you may merge", "can merge", "may merge", "can be merged", "free to merge",
    "go ahead and merge", "go ahead with the merge", "ok to merge", "okay to merge", "fine to merge", "safe to merge", "ready to merge", "ready to be merged", "merge it", "merge this", "merge that", "merge away", "merge now", "merge when", "please merge", "will merge", "i merge", "we merge", "am merging", "are merging", "merging now", "merging it", "merging this", "lets merge",
    "should merge", "you can close", "you may close", "can close", "may close", "can be closed", "free to close", "go ahead and close", "ok to close", "okay to close", "fine to close", "safe to close", "close it", "close this", "close that", "close the", "closing it", "closing this", "closing the", "will close", "am closing", "lets close", "please close", "go ahead and deploy",
    "go ahead and ship", "go ahead and release", "agree to", "agreed to", "it is a deal", "merging", "passes review", "passed review", "passes my review", "passed my review", "review passed", "review passes", "ready to ship", "ready to go", "ready to land", "ready to deploy", "ready to release", "ready to be shipped", "can ship", "can land", "can go in", "can go out",
    "can go live", "can be shipped", "can be deployed", "can be released",
];

/// Words that start like this approve or accept ("approve", "approved", "approval", "accept", …).
const NEVER_STARTS: &[&str] = &["approv", "accept"];

/// A promise, a time, an estimate, an opinion, a decision, an apology, a refusal, a question,
/// money, or something personal: any of these and the person decides.
const NEEDS_OK: &[&str] = &[
    // Promises and plans.
    "will", "shall", "going to", "gonna", "commit to", "committed to", "on it", "working on", "work on", "look into", "looking into", "take care", "get back", "come back", "follow up", "let you know", "keep you posted", "count on", "make sure", "i can", "we can", "plan to", "planning to", "aim to", "try to", "trying to",
    // Dates and times.
    "today", "tomorrow", "tonight", "yesterday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday", "mon", "tue", "tues", "wed", "thu", "thur", "thurs", "fri", "sat", "sun", "january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december", "jan", "feb", "mar", "apr", "jun", "jul", "aug",
    "sep", "sept", "oct", "nov", "dec", "morning", "afternoon", "evening", "noon", "midnight", "eod", "eow", "cob", "day", "days", "week", "weeks", "weekend", "month", "months", "year", "quarter", "sprint", "hour", "hours", "minute", "minutes", "min", "mins", "soon", "later", "shortly", "asap", "right now", "immediately", "deadline", "due", "until", "till", "by then",
    "oclock", "lunch", "next", "heute", "morgen",
    // Estimates.
    "eta", "roughly", "approximately", "approx", "probably", "likely", "unlikely", "maybe", "perhaps", "might", "should", "could", "would", "hopefully", "hope", "guess", "assume", "almost", "nearly", "percent",
    // Opinions.
    "think", "thinks", "believe", "feel", "feels", "opinion", "imo", "imho", "rather", "good", "bad", "great", "nice", "awesome", "terrible", "ugly", "better", "worse", "best", "worst", "wrong", "love", "hate", "like", "dislike", "worried", "worry", "unfortunately", "sadly", "happy", "unhappy", "important", "must", "need to", "needs to", "have to", "has to", "ought",
    "too", "enough", "seems", "seem", "looks", "perfect", "fine", "ok", "okay",
    // Decisions.
    "can", "ready to", "merge", "close", "closing", "owe", "but", "decision", "lets", "let us", "go with", "go ahead", "instead", "plan", "plans", "planned", "choose", "chose", "chosen", "pick", "drop", "skip", "revert", "hold off", "wait", "yes", "no", "nope", "yep", "yeah", "sure", "deal",
    // Apologies.
    "sorry", "my bad", "my fault", "excuse", "regret", "forgive", "pardon",
    // Refusals.
    "can not", "cannot", "do not", "not going", "not able", "unable", "not possible", "impossible", "no way", "never", "not want", "not do", "not happen",
    // Questions and asks.
    "can you", "could you", "would you", "do you", "did you", "are you", "have you", "why", "how", "what", "need you", "you need", "you must", "you should", "hurry", "emergency", "critical",
    // Money.
    "usd", "eur", "euro", "euros", "dollar", "dollars", "budget", "pay", "paid", "payment", "payments", "salary", "bonus", "raise", "quote", "discount", "refund", "bill", "billing", "contract", "offer", "money", "cheap", "fee", "fees",
    // Personal and private.
    "welcome", "sick", "vacation", "holiday", "ooo", "out of office", "away", "busy", "available", "free", "meet", "meeting", "call", "password", "secret", "token", "private", "confidential", "personal", "fired", "hire", "hiring", "quit", "legal", "lawyer",
];

/// The same, for words that start like this ("promise", "promised", …).
const NEEDS_OK_STARTS: &[&str] = &["promis", "guarant", "estimat", "expect", "prefer", "recommend", "suggest", "agree", "disagree", "concern", "disappoint", "frustrat", "decid", "cancel", "postpon", "reschedul", "priorit", "confirm", "apolog", "refus", "declin", "reject", "urgent", "remind", "invoic", "pric", "cost", "expens", "thank", "congrat", "credential", "resign"];

/// A number followed by one of these is a length of time or an amount.
const UNITS: &[&str] = &["am", "pm", "h", "hr", "hrs", "m", "d", "w", "k", "s", "sec", "secs", "second", "seconds"];

/// What a work ping is about …
const WORK: &[&str] = &["pr", "prs", "pull request", "pull requests", "branch", "commit", "commits", "build", "ci", "pipeline", "fix", "fixes", "review", "comment", "comments", "ticket", "issue", "test", "tests", "check", "checks", "patch", "change", "changes", "thread", "threads", "diff"];

/// … and what it says about it.
const STATE: &[&str] = &[
    "pushed", "is ready", "are ready", "ready for", "merged", "updated", "resolved", "fixed", "passing", "passes", "passed", "green", "failing", "fails", "failed", "red", "opened", "open", "reopened", "closed", "rebased", "done", "finished", "addressed", "replied", "answered", "posted", "left", "landed", "reviewed", "re review", "rereview", "another look", "requested", "is up",
    "is in", "is out", "is live", "added", "removed", "conflict", "conflicts", "started", "running", "queued", "deployed", "released", "published", "uploaded", "attached", "linked", "created", "applied", "squashed", "tagged", "assigned", "pending", "draft",
];

/// Every word a routine ping may be made of, besides numbers, names and quoted titles. A
/// word that is not here is not wrong; the text then simply waits for the person.
const PLAIN: &[&str] = &[
    // Small words.
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "has", "have", "had", "am", "on", "in", "for", "of", "to", "at", "by", "with", "from", "into", "as", "so", "and", "or", "it", "its", "this", "that", "these", "those", "there", "here", "now", "again", "also", "all", "both", "each", "your", "my", "our", "their", "i", "we", "you", "they", "them", "me", "us", "not",
    "still", "yet", "already", "just", "up", "out", "hi", "hello", "hey", "fyi", "please", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "first", "second", "third", "some", "more", "another", "other", "new", "latest", "last", "previous", "same", "after", "see", "take", "look", "re",
    // What work is made of.
    "pr", "prs", "pull", "request", "requests", "branch", "branches", "commit", "commits", "build", "builds", "ci", "pipeline", "fix", "fixes", "review", "reviews", "rereview", "comment", "comments", "ticket", "tickets", "issue", "issues", "test", "tests", "check", "checks", "patch", "change", "changes", "thread", "threads", "diff", "main", "master", "develop", "repo",
    "repository", "code", "file", "files", "version", "tag", "conflict", "conflicts", "rebase", "push", "update", "updates", "run", "runs", "job", "jobs", "log", "logs", "link", "description", "title", "label", "labels", "draft", "reviewer", "reviewers", "author", "suggestion", "suggestions", "reply", "replies", "answer", "answers", "note", "notes", "item", "items",
    "point", "points", "round",
    // What can be said about it.
    "pushed", "ready", "merged", "updated", "resolved", "fixed", "passing", "passes", "passed", "green", "failing", "fails", "failed", "red", "opened", "open", "reopened", "closed", "rebased", "done", "finished", "addressed", "replied", "answered", "posted", "left", "landed", "reviewed", "requested", "live", "added", "removed", "started", "running", "queued", "deployed",
    "released", "published", "uploaded", "attached", "linked", "created", "applied", "squashed", "tagged", "assigned", "pending",
];

/// How much say the person keeps over this text. Anything with a promise, a date, an opinion,
/// a decision or a question of judgement needs their OK; approving or merging is never sent.
/// When in doubt: `NeedsOk`.
pub fn tier(body: &str) -> Tier {
    let body = without_signature(body);
    let words = normal(&body);
    // Take out what only asks for an approval, then see whether anything approves.
    let mut rest = words.clone();
    for phrase in NOT_APPROVING {
        rest = rest.replace(&format!(" {phrase} "), "  ");
    }
    let asks = rest != words;
    if says(&rest, NEVER) || starts(&rest, NEVER_STARTS) || body.contains("👍") || plus_one(&body) {
        return Tier::Never;
    }
    if words.trim().is_empty() {
        return Tier::NeedsOk;
    }
    // "A merge conflict" and "a merge request" are things, not the act of merging.
    let words = words.replace(" merge conflict", " conflict").replace(" merge request", " pr");
    if asks || body.contains(['?', '¿']) || says(&words, NEEDS_OK) || starts(&words, NEEDS_OK_STARTS) || has_time_or_amount(&body, &words) || is_long(&body, &words) {
        return Tier::NeedsOk;
    }
    // What is left must be a plain fact about work, in plain words. A quoted title is not
    // part of the sentence: it may say what it likes (it passed the checks above).
    let own = outside_quotes(&body);
    let own_words = normal(&own).replace(" merge conflict", " conflict").replace(" merge request", " pr");
    let about_work = says(&own_words, WORK) || has_reference(&own);
    if about_work && says(&own_words, STATE) && is_plain(&own, &own_words) { Tier::Routine } else { Tier::NeedsOk }
}

/// Put a message in the outbox: signed (`Settings::signature`), with its tier, as a draft
/// (or already approved when it is routine and the person allowed routine pings). The same
/// text to the same person twice is one message. Returns it.
///
/// A text that approves, merges, closes or accepts is kept as rejected, with the reason, so
/// the person can see what was held back; it can never be allowed. "Twice" covers a message
/// that waits, one the person rejected and one that went out, for `QUIET_SECS`: a rule that
/// drafts the same ping every round does not ask or send again and again.
pub fn draft(data: &mut Data, settings: &Settings, to: &str, body: &str, why: &str, about: Option<&str>, now: u64) -> OutMessage {
    put(data, settings, to, body, why, about, false, now)
}

/// Put the holding reply to an urgent message in the outbox (`holding_reply`, marked
/// `urgent`). It waits for the person like every message that is not a routine ping.
pub fn hold(data: &mut Data, settings: &Settings, to: &str, about: Option<&str>, now: u64) -> OutMessage {
    put(data, settings, to, &holding_reply(settings), "A holding reply to an urgent message.", about, true, now)
}

#[allow(clippy::too_many_arguments)]
fn put(data: &mut Data, settings: &Settings, to: &str, body: &str, why: &str, about: Option<&str>, urgent: bool, now: u64) -> OutMessage {
    let signature = settings.signature();
    let to = settings.person(to).map(|p| p.name.clone()).unwrap_or_else(|| to.trim().to_string());
    let text = unsigned(body, &signature);
    let refuse = |error: &str| OutMessage { to: to.clone(), body: text.clone(), state: MsgState::Rejected, error: Some(error.into()), created: now, updated: now, ..Default::default() };
    if text.is_empty() {
        return refuse("There is nothing to send.");
    }
    if to.is_empty() {
        return refuse("It does not say who the message is for.");
    }
    let tier = tier(&text);
    let body = signed(&text, &signature);
    if let Some(same) = data.outbox.iter().find(|m| is_repeat(m, &to, &body, now)) {
        return same.clone();
    }
    let (state, error) = match tier {
        Tier::Never => (MsgState::Rejected, Some(NEVER_REASON.to_string())),
        Tier::Routine if settings.send == super::SendPolicy::RoutineAuto => (MsgState::Approved, None),
        _ => (MsgState::Draft, None),
    };
    let message = OutMessage { id: crate::model::new_id(), to, body, tier, state, why: why.trim().to_string(), about: about.map(str::trim).filter(|a| !a.is_empty()).map(str::to_string), urgent, created: now, updated: now, sent: None, error };
    data.outbox.push(message.clone());
    message
}

const NEVER_REASON: &str = "Not sent: this approves, merges, closes or accepts something in your name. Only you can say that.";

/// The person allows or rejects a draft. `edited` replaces its text (the signature is kept).
///
/// A sent message is left as it is. A text that approves or merges stays rejected whatever
/// is asked; when the person rewrites it into something else, it can go.
pub fn decide(data: &mut Data, id: &str, allow: bool, edited: Option<&str>, now: u64) -> Option<OutMessage> {
    let message = data.outbox.iter_mut().find(|m| m.id == id)?;
    if message.state == MsgState::Sent {
        return Some(message.clone());
    }
    if let Some(edited) = edited {
        // The signature is the last line `draft` wrote.
        let signature = message.body.rsplit_once('\n').map(|(_, last)| last.trim().to_string()).unwrap_or_default();
        let text = unsigned(edited, &signature);
        if !text.is_empty() {
            message.tier = tier(&text);
            message.body = signed(&text, &signature);
        }
    }
    if message.tier == Tier::Never {
        message.state = MsgState::Rejected;
        message.error = Some(NEVER_REASON.to_string());
    } else {
        message.state = if allow { MsgState::Approved } else { MsgState::Rejected };
        message.error = None;
    }
    message.updated = now.max(message.updated);
    Some(message.clone())
}

/// The effect of sending an allowed message (M42, M31): one claim per message and version, at
/// most once (the messenger has no probe: a send cut off by a crash is uncertain, for the person).
/// The v2 sender claims it before the messenger's `send` and finishes it after.
pub fn send_intent(message: &OutMessage, launch: &str) -> crate::effects::Intent {
    let subject = format!("{}@{}", message.id, message.updated);
    crate::effects::Intent {
        origin: "outbox".into(),
        subject: subject.clone(),
        target: message.to.clone(),
        caller: "the outbox".into(),
        reason: crate::words::scrub(&message.why),
        launch: launch.to_string(),
        ..crate::effects::Intent::new(crate::effects::effect_key("outbox", &subject, "message.send", &message.to), "message.send", crate::ops::Retry::AtMostOnce)
    }
}

/// Ids of the messages that may go out now, oldest first. Never one of `Tier::Never`, and
/// none at all while the manager is switched off.
pub fn ready(data: &Data, settings: &Settings) -> Vec<String> {
    if !settings.enabled {
        return Vec::new();
    }
    let mut ready: Vec<&OutMessage> = data.outbox.iter().filter(|m| m.state == MsgState::Approved && m.tier != Tier::Never && !m.to.trim().is_empty() && !m.body.trim().is_empty()).collect();
    ready.sort_by_key(|m| m.created);
    ready.into_iter().map(|m| m.id.clone()).collect()
}

pub fn mark_sent(data: &mut Data, id: &str, now: u64) {
    if let Some(message) = data.outbox.iter_mut().find(|m| m.id == id) {
        message.state = MsgState::Sent;
        message.sent = Some(now);
        message.error = None;
        message.updated = now.max(message.updated);
    }
}

/// Sending did not work. The message waits for the person again (`waiting`), with the
/// reason: it is not tried again by itself, since it may have gone out after all.
pub fn mark_failed(data: &mut Data, id: &str, error: &str, now: u64) {
    if let Some(message) = data.outbox.iter_mut().find(|m| m.id == id && m.state != MsgState::Sent) {
        message.state = MsgState::Failed;
        message.error = Some(error.trim().to_string());
        message.updated = now.max(message.updated);
    }
}

/// The text of a holding reply to an urgent message: it says who is writing, that the person
/// has been told, and promises nothing.
pub fn holding_reply(settings: &Settings) -> String {
    let name = settings.name.trim();
    let name = if name.is_empty() { "the manager" } else { name };
    let owner = settings.owner.trim();
    if owner.is_empty() {
        format!("This is an automatic reply from {name}. Your message has been passed on, and you get a personal answer to it.")
    } else {
        format!("This is an automatic reply from {name}, on behalf of {owner}. {owner} has been told about your message and answers it personally.")
    }
}

/// Messages waiting for the person's OK, oldest first: drafts, and those that failed to send
/// (allowing one of those again sends it again).
pub fn waiting(data: &Data) -> Vec<&OutMessage> {
    let mut waiting: Vec<&OutMessage> = data.outbox.iter().filter(|m| matches!(m.state, MsgState::Draft | MsgState::Failed) && m.tier != Tier::Never).collect();
    waiting.sort_by_key(|m| m.created);
    waiting
}

// ----- signing and repeats ----------------------------------------------------------------------

/// The text without the signature at its end, if it has one.
fn unsigned(body: &str, signature: &str) -> String {
    let body = body.trim();
    let signature = signature.trim();
    if signature.is_empty() {
        return body.to_string();
    }
    match body.rsplit_once('\n') {
        Some((text, last)) if is_signature(last, signature) => text.trim().to_string(),
        None if is_signature(body, signature) => String::new(),
        _ => body.to_string(),
    }
}

fn is_signature(line: &str, signature: &str) -> bool {
    line.trim().trim_start_matches(['-', '—', '–', ' ']).eq_ignore_ascii_case(signature)
}

/// The text with the signature on a line of its own.
fn signed(text: &str, signature: &str) -> String {
    let signature = signature.trim();
    if signature.is_empty() { text.to_string() } else { format!("{text}\n{signature}") }
}

fn squeezed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Whether this message already says the same to the same person, and is recent enough to
/// stand for it: one that still waits or failed always, a rejected or sent one for a day,
/// and one that can never be sent for good.
fn is_repeat(message: &OutMessage, to: &str, body: &str, now: u64) -> bool {
    if !message.to.eq_ignore_ascii_case(to) || squeezed(&message.body) != squeezed(body) {
        return false;
    }
    let recent = |at: u64| now.saturating_sub(at) < QUIET_SECS;
    match message.state {
        MsgState::Draft | MsgState::Approved | MsgState::Failed => true,
        MsgState::Rejected => message.tier == Tier::Never || recent(message.updated),
        MsgState::Sent => recent(message.sent.unwrap_or(message.updated)),
    }
}

// ----- reading a text ---------------------------------------------------------------------------

/// The text without a last line that is a signature ("…, on behalf of …").
fn without_signature(body: &str) -> String {
    let body = body.trim();
    match body.rsplit_once('\n') {
        Some((text, last)) if last.to_lowercase().contains(" on behalf of ") => text.trim().to_string(),
        _ => body.to_string(),
    }
}

/// The text as plain words between single spaces, with a space at both ends, so `" word "`
/// finds whole words. Short forms are written out ("I'll" → "i will", "can't" → "can not").
fn normal(body: &str) -> String {
    let low = body.to_lowercase().replace(['’', '`'], "'");
    let low = low.replace("won't", "will not").replace("can't", "can not").replace("let's", "lets").replace("n't", " not").replace("'ll", " will").replace("'re", " are").replace("'ve", " have").replace("'m", " am").replace("'d", " would").replace("it's", "it is").replace("'s", "").replace("o'clock", "oclock");
    let mut out = String::from(" ");
    for c in low.chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

/// Whether one of these words or phrases is in the text, as whole words.
fn says(words: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|p| words.contains(&format!(" {p} ")))
}

/// Whether a word of the text starts like one of these.
fn starts(words: &str, stems: &[&str]) -> bool {
    stems.iter().any(|s| words.contains(&format!(" {s}")))
}

/// "+1": an approval in two characters.
fn plus_one(body: &str) -> bool {
    body.match_indices("+1").any(|(i, _)| !body[i + 2..].starts_with(|c: char| c.is_ascii_digit()) && !body[..i].ends_with(|c: char| c.is_alphanumeric()))
}

/// A pull request or issue number (`#42`) or a ticket id (`ENG-826`).
fn has_reference(body: &str) -> bool {
    let chars: Vec<char> = body.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let digit_next = chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
        if c == '#' && digit_next {
            return true;
        }
        if c == '-' && digit_next && i >= 2 && chars[i - 1].is_ascii_alphabetic() && chars[i - 2].is_ascii_alphabetic() {
            return true;
        }
    }
    false
}

/// A time of day, a date, a length of time, an amount of money or a share: "14:30", "3pm",
/// "12.11.", "2026-11-15", "2 hours", "the 15th", "500 €", "80%".
fn has_time_or_amount(body: &str, words: &str) -> bool {
    if body.contains(['$', '€', '£', '¥', '%']) {
        return true;
    }
    let chars: Vec<char> = body.chars().collect();
    let digit = |i: usize| chars.get(i).is_some_and(|c| c.is_ascii_digit());
    for (i, &c) in chars.iter().enumerate() {
        // 14:30, 12.11, 3/4 and 2026-11 all have a digit on both sides of the sign.
        if i > 0 && digit(i - 1) && digit(i + 1) && (matches!(c, ':' | '.' | '/') || (c == '-' && i >= 4 && (i - 4..i).all(digit))) {
            return true;
        }
    }
    let list: Vec<&str> = words.split(' ').filter(|w| !w.is_empty()).collect();
    for (i, word) in list.iter().enumerate() {
        let digits = word.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            continue;
        }
        let tail = &word[digits..];
        // "3pm", "2h", "5k", "15th"; or a number and then a unit as the next word.
        if !tail.is_empty() && (UNITS.contains(&tail) || ["st", "nd", "rd", "th"].contains(&tail)) {
            return true;
        }
        if tail.is_empty() && list.get(i + 1).is_some_and(|next| UNITS.contains(next)) {
            return true;
        }
    }
    false
}

/// The text without what stands in double quotes or backticks (a pull request's title, a
/// branch name). With a quote left open, nothing is taken out.
fn outside_quotes(body: &str) -> String {
    let mut out = String::new();
    let mut closing: Option<char> = None;
    for c in body.chars() {
        match closing {
            Some(end) if c == end => {
                closing = None;
                out.push(' ');
            }
            Some(_) => {}
            None => match c {
                '"' | '`' => closing = Some(c),
                '“' | '„' => closing = Some('”'),
                '«' => closing = Some('»'),
                _ => out.push(c),
            },
        }
    }
    if closing.is_some() { body.to_string() } else { out }
}

/// Whether every word is a plain word about work, a number, a ticket's prefix or a name. A
/// name is a word with a capital letter inside a sentence ("Hi Sam"), or one that owns
/// something ("Mira's comments"); at the start of a sentence a capital letter says nothing.
fn is_plain(own: &str, own_words: &str) -> bool {
    let mut known: Vec<String> = Vec::new();
    let mut starts_sentence = true;
    for raw in own.split_whitespace() {
        let first = starts_sentence;
        starts_sentence = raw.ends_with(['.', '!', ':', '…']);
        let word = raw.trim_matches(|c: char| !c.is_alphanumeric());
        let (word, owns) = match word.strip_suffix("'s").or_else(|| word.strip_suffix("’s")) {
            Some(owner) => (owner, true),
            None => (word, false),
        };
        // "ENG-826": the letters before the number.
        if let Some((team, number)) = word.split_once('-')
            && !team.is_empty()
            && team.chars().all(|c| c.is_ascii_alphabetic())
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
        {
            known.push(team.to_lowercase());
            continue;
        }
        let capital = word.chars().next().is_some_and(char::is_uppercase) && word.chars().count() >= 2 && word.chars().skip(1).all(char::is_lowercase);
        if capital && (owns || !first) {
            known.push(word.to_lowercase());
        }
    }
    own_words.split(' ').filter(|w| !w.is_empty()).all(|w| w.chars().all(|c| c.is_ascii_digit()) || PLAIN.contains(&w) || known.iter().any(|k| k == w))
}

/// More than a couple of sentences, or simply a lot of text.
fn is_long(body: &str, words: &str) -> bool {
    if body.chars().count() > MAX_CHARS || words.split(' ').filter(|w| !w.is_empty()).count() > MAX_WORDS {
        return true;
    }
    let chars: Vec<char> = body.chars().collect();
    let mut sentences = 0;
    let mut has_words = false;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_alphanumeric() {
            has_words = true;
        }
        let ends = c == '\n' || (matches!(c, '.' | '!' | '?' | '…') && chars.get(i + 1).is_none_or(|n| n.is_whitespace()));
        if ends && has_words {
            sentences += 1;
            has_words = false;
        }
    }
    sentences + usize::from(has_words) > MAX_SENTENCES
}

#[cfg(test)]
mod tests {
    use super::super::{Person, SendPolicy};
    use super::*;

    fn settings(send: SendPolicy) -> Settings {
        Settings { enabled: true, name: "Robo".into(), owner: "Alex".into(), send, people: vec![Person { name: "Sam".into(), github: "sam-dev".into(), ..Default::default() }, Person { name: "Mira".into(), github: "mira".into(), urgent: true, ..Default::default() }], ..Default::default() }
    }

    const SIG: &str = "Robo, on behalf of Alex";

    #[test]
    fn ask_always_never_sends() {
        // The default is to ask, always.
        assert_eq!(Settings::default().send, SendPolicy::AskAlways);
        let s = settings(SendPolicy::AskAlways);
        let mut data = Data::default();
        // Even a routine ping waits for the person.
        let routine = draft(&mut data, &s, "Sam", "The fix for #1111 is pushed", "news", None, 10);
        assert_eq!(routine.tier, Tier::Routine);
        assert_eq!(routine.state, MsgState::Draft);
        let promise = draft(&mut data, &s, "Sam", "I will have it done by Friday", "a date", None, 11);
        assert_eq!(promise.state, MsgState::Draft);
        assert!(ready(&data, &s).is_empty(), "nothing goes before the person says so");
        // The person allows one: only that one goes, once its claim is made.
        decide(&mut data, &routine.id, true, None, 20);
        assert_eq!(ready(&data, &s), [routine.id.clone()]);
        let allowed = data.outbox.iter().find(|m| m.id == routine.id).unwrap().clone();
        let intent = send_intent(&allowed, "run-1");
        assert_eq!(intent.retry, crate::ops::Retry::AtMostOnce);
        assert_eq!(intent.op, "message.send");
        assert_eq!(intent.key, send_intent(&allowed, "run-2").key, "the same message is the same effect");
        // The routine tier stays advised: with RoutineAuto it may go by itself, nothing else.
        let auto = settings(SendPolicy::RoutineAuto);
        let mut data = Data::default();
        assert_eq!(draft(&mut data, &auto, "Sam", "The fix for #1111 is pushed", "news", None, 10).state, MsgState::Approved);
        assert_eq!(draft(&mut data, &auto, "Sam", "I will have it done by Friday", "a date", None, 11).state, MsgState::Draft);
    }

    #[test]
    fn sorts_texts_into_the_three_tiers() {
        use Tier::*;
        let table: &[(&str, Tier)] = &[
            // Short facts about work: the only texts that may go by themselves.
            ("the fix for #1111 is pushed", Routine),
            ("PR #42 is ready for another look", Routine),
            ("I left 3 comments on #7", Routine),
            ("Hi Sam, #42 is merged.", Routine),
            ("CI is green on #1111 again.", Routine),
            ("Please re-review #42, the comments are addressed.", Routine),
            ("ENG-826: the branch is rebased and the tests are passing.", Routine),
            ("Ready for re-review: #88.", Routine),
            ("All four threads on #7 are resolved.", Routine),
            ("There is a merge conflict on #42.", Routine),
            ("The build failed on #12. A fix is pushed.", Routine),
            ("the fix for #1111 is pushed\nRobo, on behalf of Alex", Routine),
            ("PR #42 \"Fix the login redirect\" is ready for another look", Routine),
            ("The fix for #42 is pushed on `fix/login-redirect`.", Routine),
            ("Mira's comments on #42 are addressed.", Routine),
            ("Hey Sam, I replied to your comments on WEB-12.", Routine),
            ("Merge request #5 is updated", Routine),
            // Approving, merging, closing, accepting: never.
            ("approved", Never),
            ("Approved, thanks!", Never),
            ("LGTM", Never),
            ("lgtm 🚀", Never),
            ("you can merge", Never),
            ("I approve", Never),
            ("I approve of #42.", Never),
            ("Looks good to me, ship it!", Never),
            ("Feel free to merge #42.", Never),
            ("PR #42 is ready to merge", Never),
            ("I'll merge it after lunch", Never),
            ("Go ahead and close the ticket.", Never),
            ("You can close #7.", Never),
            ("I accept the proposal.", Never),
            ("Accepted.", Never),
            ("+1 from me", Never),
            ("👍", Never),
            ("Sam's approval stands, merge away", Never),
            ("#42 is not approved yet, but LGTM from my side", Never),
            ("I am requesting changes on #9", Never),
            ("#42 passes review", Never),
            ("#42 is ready to ship", Never),
            ("All comments are resolved, merging", Never),
            ("PR #42 \"Small fix\" is approved", Never),
            // Everything else waits for the person.
            ("I'll have it done by Friday.", NeedsOk),
            ("The fix will be ready tomorrow.", NeedsOk),
            ("It should take about 2 hours.", NeedsOk),
            ("The fix for #1111 is pushed, deploy is at 15:00", NeedsOk),
            ("I think we should use Postgres.", NeedsOk),
            ("Let's go with option B.", NeedsOk),
            ("We decided to drop the feature.", NeedsOk),
            ("Sorry for the delay on #42.", NeedsOk),
            ("No, I can't take this one.", NeedsOk),
            ("I won't review #7.", NeedsOk),
            ("Can you look at #42?", NeedsOk),
            ("Is #42 merged?", NeedsOk),
            ("That costs 500 € per month.", NeedsOk),
            ("The budget for #42 is updated", NeedsOk),
            ("Please approve #42 when you get to it", NeedsOk),
            ("#42 is not approved yet", NeedsOk),
            ("Don't merge #42 yet.", NeedsOk),
            ("Meeting at 14:30.", NeedsOk),
            ("I'm on it.", NeedsOk),
            ("Thanks for the review!", NeedsOk),
            ("I disagree with the comment on #7.", NeedsOk),
            ("ETA for #42 is next week", NeedsOk),
            ("#42 is 80% done", NeedsOk),
            ("The fix for #7 is pushed on the 15th", NeedsOk),
            ("#7 is fixed, it took 3h", NeedsOk),
            ("Alex is on vacation.", NeedsOk),
            ("hello", NeedsOk),
            ("The weather is nice.", NeedsOk),
            ("#42 is pushed. The tests are passing. The comments are addressed.", NeedsOk),
            ("", NeedsOk),
            ("   \n ", NeedsOk),
            // Words that are not plainly about work: it may be fine, but it is not sure.
            ("The fix for the login bug is pushed", NeedsOk),
            ("The fix for your sloppy code is pushed", NeedsOk),
            ("Sloppy code in #42 is fixed", NeedsOk),
            ("#42 is unblocked", NeedsOk),
            ("Merge #42, it is ready", NeedsOk),
            ("#42 is ready to review", NeedsOk),
            ("#42 is fixed, you owe me", NeedsOk),
            ("#42 is pushed but the tests are red", NeedsOk),
            ("PR #42 \"Pay the invoice\" is merged", NeedsOk),
            ("The fix for \"login is pushed", NeedsOk),
        ];
        assert!(table.len() >= 25);
        let wrong: Vec<String> = table.iter().filter(|(text, want)| tier(text) != *want).map(|(text, want)| format!("{text:?}: want {want:?}, got {:?}", tier(text))).collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn a_long_text_always_waits() {
        let long = "The fix for #1111 is pushed and the branch is rebased and the build is green and the comments are addressed and the threads are resolved and the tests are passing and the checks are done and the review is requested and the diff is updated";
        assert!(long.chars().count() > MAX_CHARS);
        assert_eq!(tier(long), Tier::NeedsOk);
        assert_eq!(tier("#1 is pushed\n#2 is pushed\n#3 is pushed"), Tier::NeedsOk);
        // Two short sentences are still a ping; a version number is not a sentence end.
        assert_eq!(tier("#1 is pushed. #2 is merged."), Tier::Routine);
    }

    #[test]
    fn drafts_are_signed_once_on_a_line_of_their_own() {
        let mut data = Data::default();
        let s = settings(SendPolicy::AskAlways);
        let m = draft(&mut data, &s, "sam-dev", "  The fix for #1111 is pushed.  ", "Sam asked for it in his review.", Some("f1"), 100);
        assert_eq!(m.body, format!("The fix for #1111 is pushed.\n{SIG}"));
        assert_eq!(m.to, "Sam", "the name as in the settings, also when given the GitHub login");
        assert_eq!((m.tier, m.state), (Tier::Routine, MsgState::Draft), "routine still waits while the person wants to be asked");
        assert_eq!((m.why.as_str(), m.about.as_deref(), m.created, m.updated, m.sent, m.urgent), ("Sam asked for it in his review.", Some("f1"), 100, 100, None, false));
        assert!(!m.id.is_empty());
        assert_eq!(data.outbox, vec![m]);

        // Already signed, in several spellings: not signed twice.
        for body in [format!("PR #42 is ready for another look\n{SIG}"), "PR #42 is ready for another look\n\n— robo, on behalf of alex ".to_string()] {
            let mut data = Data::default();
            let m = draft(&mut data, &s, "Sam", &body, "", None, 100);
            assert_eq!(m.body, format!("PR #42 is ready for another look\n{SIG}"));
            assert_eq!(m.tier, Tier::Routine, "the signature does not count as text");
        }

        // No owner set: the signature is the manager's name alone.
        let mut data = Data::default();
        let m = draft(&mut data, &Settings { owner: String::new(), ..s.clone() }, "Sam", "#42 is merged", "", None, 100);
        assert_eq!(m.body, "#42 is merged\nRobo");
    }

    #[test]
    fn routine_goes_by_itself_only_when_the_person_allowed_it() {
        let mut data = Data::default();
        let auto = settings(SendPolicy::RoutineAuto);
        let ping = draft(&mut data, &auto, "Sam", "The fix for #1111 is pushed", "", None, 100);
        assert_eq!((ping.tier, ping.state), (Tier::Routine, MsgState::Approved));
        let promise = draft(&mut data, &auto, "Sam", "I'll fix #1111 by Friday", "", None, 100);
        assert_eq!((promise.tier, promise.state), (Tier::NeedsOk, MsgState::Draft), "anything else still waits");
        assert_eq!(ready(&data, &auto), vec![ping.id.clone()]);
        assert_eq!(waiting(&data).iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec![promise.id.as_str()]);

        // Switched off: nothing may go.
        assert!(ready(&data, &Settings { enabled: false, ..auto.clone() }).is_empty());
    }

    #[test]
    fn an_approval_is_never_a_draft_and_cannot_be_allowed() {
        let mut data = Data::default();
        let auto = settings(SendPolicy::RoutineAuto);
        let m = draft(&mut data, &auto, "Sam", "LGTM, you can merge #42", "Sam asked whether it can go in.", None, 100);
        assert_eq!((m.tier, m.state), (Tier::Never, MsgState::Rejected));
        assert!(m.error.as_deref().is_some_and(|e| e.contains("in your name")));
        assert!(ready(&data, &auto).is_empty());
        assert!(waiting(&data).is_empty());

        // Allowing it changes nothing.
        let after = decide(&mut data, &m.id, true, None, 110).unwrap();
        assert_eq!((after.tier, after.state), (Tier::Never, MsgState::Rejected));
        assert!(ready(&data, &auto).is_empty());

        // Editing a draft into an approval rejects it as well.
        let ping = draft(&mut data, &auto, "Sam", "I looked at #42, see my comments", "", None, 120);
        assert_eq!(ping.state, MsgState::Draft);
        let after = decide(&mut data, &ping.id, true, Some("Approved!"), 130).unwrap();
        assert_eq!((after.tier, after.state), (Tier::Never, MsgState::Rejected));
        assert!(after.error.is_some());
        assert!(ready(&data, &auto).is_empty());

        // Even a message that claims to be approved is not sent when its tier is Never.
        data.outbox[0].state = MsgState::Approved;
        assert!(ready(&data, &auto).is_empty());

        // The same approval drafted again is the one already held back, not a second entry.
        let count = data.outbox.len();
        let again = draft(&mut data, &auto, "Sam", "LGTM, you can merge #42", "", None, 100 + 30 * QUIET_SECS);
        assert_eq!(again.id, m.id);
        assert_eq!(data.outbox.len(), count);
    }

    #[test]
    fn the_same_text_to_the_same_person_is_one_message() {
        let mut data = Data::default();
        let s = settings(SendPolicy::AskAlways);
        let first = draft(&mut data, &s, "Sam", "PR #42 is ready for another look", "", None, 100);
        let again = draft(&mut data, &s, "sam", "  pr #42 is ready   for another look\n", "another reason", None, 200);
        assert_eq!(again, first);
        assert_eq!(data.outbox.len(), 1);

        // Another person, or another text, is another message.
        draft(&mut data, &s, "Mira", "PR #42 is ready for another look", "", None, 200);
        draft(&mut data, &s, "Sam", "PR #43 is ready for another look", "", None, 200);
        assert_eq!(data.outbox.len(), 3);

        // Allowed and waiting to go: still the same message.
        decide(&mut data, &first.id, true, None, 300);
        assert_eq!(draft(&mut data, &s, "Sam", "PR #42 is ready for another look", "", None, 400).id, first.id);

        // Sent: not again within a day, but after that it is a new message.
        mark_sent(&mut data, &first.id, 500);
        assert_eq!(draft(&mut data, &s, "Sam", "PR #42 is ready for another look", "", None, 500 + QUIET_SECS - 1).id, first.id);
        assert_eq!(data.outbox.len(), 3);
        let later = draft(&mut data, &s, "Sam", "PR #42 is ready for another look", "", None, 500 + QUIET_SECS);
        assert_ne!(later.id, first.id);
        assert_eq!(later.state, MsgState::Draft);
        assert_eq!(data.outbox.len(), 4);
    }

    #[test]
    fn a_rejected_text_is_not_proposed_again_the_same_day() {
        let mut data = Data::default();
        let s = settings(SendPolicy::RoutineAuto);
        let first = draft(&mut data, &s, "Sam", "I think #42 needs more tests", "", None, 100);
        decide(&mut data, &first.id, false, None, 200);
        let again = draft(&mut data, &s, "Sam", "I think #42 needs more tests", "", None, 300);
        assert_eq!((again.id.as_str(), again.state), (first.id.as_str(), MsgState::Rejected));
        assert!(waiting(&data).is_empty());
        let next_day = draft(&mut data, &s, "Sam", "I think #42 needs more tests", "", None, 200 + QUIET_SECS);
        assert_ne!(next_day.id, first.id);
        assert_eq!(next_day.state, MsgState::Draft);
    }

    #[test]
    fn nothing_to_say_or_nobody_to_say_it_to_is_not_stored() {
        let mut data = Data::default();
        let s = settings(SendPolicy::RoutineAuto);
        for (to, body) in [("Sam", "   "), ("Sam", SIG), ("  ", "The fix for #1 is pushed")] {
            let m = draft(&mut data, &s, to, body, "", None, 100);
            assert_eq!(m.state, MsgState::Rejected, "{to:?} {body:?}");
            assert!(m.error.is_some());
        }
        assert!(data.outbox.is_empty());
    }

    #[test]
    fn the_person_decides_and_may_edit() {
        let mut data = Data::default();
        let s = settings(SendPolicy::AskAlways);
        let m = draft(&mut data, &s, "Sam", "I'll look at #42 tomorrow", "", None, 100);
        assert!(ready(&data, &s).is_empty());

        let edited = decide(&mut data, &m.id, true, Some("  I'll look at #42 on Monday. "), 200).unwrap();
        assert_eq!(edited.body, format!("I'll look at #42 on Monday.\n{SIG}"), "the signature stays");
        assert_eq!((edited.state, edited.tier, edited.updated), (MsgState::Approved, Tier::NeedsOk, 200));
        assert_eq!(ready(&data, &s), vec![m.id.clone()]);

        // Editing with the signature typed in does not double it.
        let edited = decide(&mut data, &m.id, true, Some(&format!("#42 is reviewed\n{SIG}")), 210).unwrap();
        assert_eq!(edited.body, format!("#42 is reviewed\n{SIG}"));
        assert_eq!(edited.tier, Tier::Routine);

        // Rejecting takes it out; an empty edit leaves the text alone.
        let rejected = decide(&mut data, &m.id, false, Some("  "), 220).unwrap();
        assert_eq!((rejected.state, rejected.body.as_str()), (MsgState::Rejected, edited.body.as_str()));
        assert!(ready(&data, &s).is_empty());
        assert!(waiting(&data).is_empty());

        // The person may change their mind.
        assert_eq!(decide(&mut data, &m.id, true, None, 230).map(|m| m.state), Some(MsgState::Approved));
        assert_eq!(decide(&mut data, "no-such-id", true, None, 230), None);
    }

    #[test]
    fn sent_and_failed_are_recorded() {
        let mut data = Data::default();
        let s = settings(SendPolicy::RoutineAuto);
        let a = draft(&mut data, &s, "Sam", "The fix for #1 is pushed", "", None, 100);
        let b = draft(&mut data, &s, "Mira", "The fix for #2 is pushed", "", None, 90);
        assert_eq!(ready(&data, &s), vec![b.id.clone(), a.id.clone()], "oldest first");

        mark_sent(&mut data, &a.id, 150);
        let sent = &data.outbox[0];
        assert_eq!((sent.state, sent.sent, sent.updated), (MsgState::Sent, Some(150), 150));
        assert_eq!(ready(&data, &s), vec![b.id.clone()]);

        // A sent message is not changed any more.
        assert_eq!(decide(&mut data, &a.id, false, Some("something else"), 160).map(|m| (m.state, m.body)), Some((MsgState::Sent, format!("The fix for #1 is pushed\n{SIG}"))));
        mark_failed(&mut data, &a.id, "late error", 170);
        assert_eq!(data.outbox[0].state, MsgState::Sent);

        // A failure waits for the person; allowing it again makes it ready again.
        mark_failed(&mut data, &b.id, "  The messenger is not signed in. ", 180);
        let failed = &data.outbox[1];
        assert_eq!((failed.state, failed.error.as_deref(), failed.updated), (MsgState::Failed, Some("The messenger is not signed in."), 180));
        assert!(ready(&data, &s).is_empty(), "not tried again by itself");
        assert_eq!(waiting(&data).iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec![b.id.as_str()]);
        decide(&mut data, &b.id, true, None, 190);
        assert_eq!(ready(&data, &s), vec![b.id.clone()]);
        assert_eq!(data.outbox[1].error, None);

        mark_sent(&mut data, "no-such-id", 200);
        mark_failed(&mut data, "no-such-id", "x", 200);
    }

    #[test]
    fn the_holding_reply_says_who_writes_and_promises_nothing() {
        let s = settings(SendPolicy::RoutineAuto);
        let text = holding_reply(&s);
        assert_eq!(text, "This is an automatic reply from Robo, on behalf of Alex. Alex has been told about your message and answers it personally.");
        let low = format!(" {} ", text.to_lowercase());
        for word in [" will ", " soon ", " shortly ", " today ", " minutes ", " hour ", " asap ", " promise ", " sorry "] {
            assert!(!low.contains(word), "{word:?} in the holding reply");
        }
        assert_eq!(tier(&text), Tier::NeedsOk, "a reply in the person's place is not a routine ping");
        let nameless = holding_reply(&Settings { owner: " ".into(), ..s.clone() });
        assert_eq!(nameless, "This is an automatic reply from Robo. Your message has been passed on, and you get a personal answer to it.");
        assert!(holding_reply(&Settings { name: String::new(), owner: String::new(), ..s }).contains("from the manager."));
    }

    #[test]
    fn a_holding_reply_waits_for_the_person_and_is_marked_urgent() {
        let mut data = Data::default();
        let s = settings(SendPolicy::RoutineAuto);
        let m = hold(&mut data, &s, "Mira", Some("in-7"), 100);
        assert_eq!((m.state, m.tier, m.urgent, m.about.as_deref()), (MsgState::Draft, Tier::NeedsOk, true, Some("in-7")));
        assert_eq!(m.body, format!("{}\n{SIG}", holding_reply(&s)));
        assert!(ready(&data, &s).is_empty(), "also for a teammate marked urgent, and with routine pings allowed");
        // A second urgent message from the same person does not make a second reply.
        assert_eq!(hold(&mut data, &s, "Mira", Some("in-8"), 200).id, m.id);
        assert_eq!(data.outbox.len(), 1);
    }
}
