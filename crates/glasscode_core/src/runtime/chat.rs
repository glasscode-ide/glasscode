//! The chat surface's pure parts (M52.1; ux-4, ux-9, ux-18, ux-20): what Enter does in the Today
//! box, the person's messages queued while the chat's brain answers, references in an answer that
//! become chips, a request to start a session read with no model turn, "what can you do?" answered
//! from the registry, and what each brain can do on screen. The app draws them (`manager_chat.rs`).

use std::collections::VecDeque;
use std::ops::Range;

use crate::model::AgentKind;
use crate::words::WordsId;

// ----- the Today box -----------------------------------------------------------------------------

/// What the Today box does with Enter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StartMode {
    /// Start a session, as it always did.
    #[default]
    Session,
    /// Ask the manager.
    Ask,
}

/// What Enter does now, and why it cannot ask when the person chose to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Enter {
    pub mode: StartMode,
    /// The person chose Ask and it cannot answer now: one line, shown by the control.
    pub why_not: Option<String>,
    /// The control is shown at all (the manager is on).
    pub choice: bool,
}

/// What Enter does (ux-4): while the manager is off, or cannot answer, the box starts a session
/// exactly as before; once it is on, the person's choice (`chosen`, Session until they pick Ask).
pub fn enter_meaning(manager_on: bool, can_answer: Result<(), String>, chosen: StartMode) -> Enter {
    if !manager_on {
        return Enter { mode: StartMode::Session, why_not: None, choice: false };
    }
    match (chosen, can_answer) {
        (StartMode::Ask, Err(why)) => Enter { mode: StartMode::Session, why_not: Some(why), choice: true },
        (mode, _) => Enter { mode, why_not: None, choice: true },
    }
}

/// The box's placeholder for what Enter does.
pub fn placeholder(mode: StartMode, name: &str) -> String {
    match mode {
        StartMode::Session => "Ask an agent to build, fix or review something".into(),
        StartMode::Ask => format!("Ask {name}: what needs you, what is happening, what to start"),
    }
}

// ----- the queue ----------------------------------------------------------------------------------

/// A message the person sent while the brain was answering: it waits, shown as queued, and goes
/// next (ux-9). It can be edited or removed until then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    pub id: u64,
    pub text: String,
    /// Its words record (the person's typing, D1).
    pub words: Option<WordsId>,
}

/// The person's queued messages, oldest first. Held by the app, not the brain: a brain that starts
/// again loses none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatQueue {
    next: u64,
    items: VecDeque<Queued>,
}

impl ChatQueue {
    pub fn push(&mut self, text: String, words: Option<WordsId>) -> u64 {
        self.next += 1;
        self.items.push_back(Queued { id: self.next, text, words });
        self.next
    }

    pub fn remove(&mut self, id: u64) -> Option<Queued> {
        let at = self.items.iter().position(|q| q.id == id)?;
        self.items.remove(at)
    }

    /// Change a queued message's text (its words record goes: the text is not what was typed).
    pub fn edit(&mut self, id: u64, text: String) -> bool {
        match self.items.iter_mut().find(|q| q.id == id) {
            Some(q) => {
                q.text = text;
                q.words = None;
                true
            }
            None => false,
        }
    }

    /// The next to send, once the brain is free.
    pub fn take_next(&mut self) -> Option<Queued> {
        self.items.pop_front()
    }

    pub fn items(&self) -> impl Iterator<Item = &Queued> {
        self.items.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Where a message goes when the person sends it: to the brain now, or behind its running answer.
pub fn send_or_queue(busy: bool, queue: &mut ChatQueue, text: String, words: Option<WordsId>) -> Option<Queued> {
    if busy || !queue.is_empty() {
        queue.push(text, words);
        return None;
    }
    Some(Queued { id: 0, text, words })
}

// ----- references as chips -------------------------------------------------------------------------

/// What a chip opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChipTarget {
    Session(String),
    Pr { repo: String, number: u64 },
}

/// A reference in a message, drawn as a chip that opens its place (ux-20).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub target: ChipTarget,
    pub label: String,
    /// Where it is in the text.
    pub at: Range<usize>,
}

/// The references a message makes: `owner/repo#12` (and `#12` when only one repository is known),
/// and a session by its id or the first 8 characters of it. `sessions`: (id, title); `repos`: the
/// known repositories. In text order, each once.
pub fn chips_from_references(text: &str, sessions: &[(String, String)], repos: &[String]) -> Vec<Chip> {
    let mut out: Vec<Chip> = Vec::new();
    let bytes = text.as_bytes();
    let word_char = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/');
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let digits_end = (i + 1..bytes.len()).find(|j| !bytes[*j].is_ascii_digit()).unwrap_or(bytes.len());
            let number: u64 = text[i + 1..digits_end].parse().unwrap_or(0);
            let start = (0..i).rev().take_while(|j| word_char(bytes[*j])).last().unwrap_or(i);
            let before = &text[start..i];
            let repo = if before.contains('/') && before.split('/').count() == 2 && before.split('/').all(|p| !p.is_empty()) {
                Some((before.to_string(), start))
            } else if before.is_empty() && repos.len() == 1 {
                Some((repos[0].clone(), i))
            } else {
                None
            };
            if let Some((repo, from)) = repo
                && number > 0
            {
                out.push(Chip { label: format!("{repo}#{number}"), target: ChipTarget::Pr { repo, number }, at: from..digits_end });
            }
            i = digits_end;
            continue;
        }
        if bytes[i].is_ascii_hexdigit() && (i == 0 || !word_char(bytes[i - 1])) {
            let end = (i..bytes.len()).find(|j| !(bytes[*j].is_ascii_hexdigit() || bytes[*j] == b'-')).unwrap_or(bytes.len());
            let word = &text[i..end];
            if word.len() >= 8
                && let Some((id, title)) = sessions.iter().find(|(id, _)| id == word || (word.len() == 8 && id.starts_with(word)))
            {
                out.push(Chip { target: ChipTarget::Session(id.clone()), label: title.clone(), at: i..end });
            }
            i = end.max(i + 1);
            continue;
        }
        i += 1;
    }
    let mut seen = Vec::new();
    out.retain(|chip| {
        let fresh = !seen.contains(&chip.target);
        seen.push(chip.target.clone());
        fresh
    });
    out
}

// ----- answers with no model -----------------------------------------------------------------------

/// A request that is plainly "start a session on X": its task, for `session.new`, with no model
/// turn (ux-4).
pub fn start_request(text: &str) -> Option<String> {
    let lower = text.trim().to_lowercase();
    const LEADS: &[&str] = &["start a session to ", "start a session on ", "start a session for ", "start a session: ", "start a new session to ", "start a new session on ", "start a new session for ", "new session: ", "new session to ", "new session on ", "new session for "];
    let lead = LEADS.iter().find(|lead| lower.starts_with(*lead))?;
    let task = text.trim()[lead.len()..].trim().trim_end_matches(['.', '!']).trim();
    (!task.is_empty()).then(|| task.to_string())
}

/// Whether the person asks what the manager can do.
pub fn asks_capabilities(text: &str) -> bool {
    let words: String = text.trim().to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect();
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    matches!(words.as_str(), "help" | "what can you do" | "what can you do here" | "what can you do for me" | "what do you do" | "what are you able to do")
}

/// The areas of the registry, as the answer to "what can you do?" names them, in its order.
const AREAS: &[(&str, &str)] = &[
    ("session", "Sessions"),
    ("pr", "Pull requests"),
    ("review", "Reviews"),
    ("ci", "CI"),
    ("board", "Today's board"),
    ("work", "Work in progress"),
    ("worktree", "Worktrees"),
    ("project", "Projects"),
    ("memory", "Memory"),
    ("playbook", "Playbooks"),
    ("ui", "The screen"),
    ("usage", "Usage"),
    ("settings", "Settings"),
];

/// "What can you do?" answered from the registry with no model (ux-18): each area, what its ops do
/// in their own words, and one way to ask, from the help's lines. `v2`: the v2 engine's ops count.
pub fn capabilities_answer(name: &str, v2: bool) -> String {
    let mut out = vec![format!("Here is what {name} can do for you. Ask in your own words; each line is one thing to ask for.")];
    for (prefix, title) in AREAS {
        let ops: Vec<&crate::ops::OpSpec> = crate::ops::OPS.iter().filter(|op| !op.human_only && (v2 || !op.v2_only) && op.name.split('.').next() == Some(prefix)).collect();
        if ops.is_empty() {
            continue;
        }
        let lines: Vec<String> = ops.iter().take(6).map(|op| format!("- {}", capitalize(op.summary))).collect();
        let example = ops.iter().find_map(|op| op.examples.first()).map(|e| format!("\n  For example: `{e}`")).unwrap_or_default();
        out.push(format!("**{title}**\n{}{example}", lines.join("\n")));
    }
    out.join("\n\n")
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

// ----- the brain's conversation behind the one on screen --------------------------------------------

/// Which brain conversation the chat's next turn runs in (D8, ux-2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// None yet: a fresh one, linked as it starts.
    First,
    /// The newest rollover link's, loaded again.
    Resume(String),
    /// The newest one's, after which a fresh one starts from its summary (a new day, an idle
    /// spell, or past a size).
    RollOver { brain: String, why: crate::conversation::RollReason },
}

/// [`Next`] from the conversation as it is now.
pub fn next_conversation(conversation: &crate::conversation::Conversation, now_ms: u64) -> Next {
    let Some(link) = conversation.last_rollover() else { return Next::First };
    let crate::conversation::Kind::Rollover { brain, .. } = &link.kind else { return Next::First };
    let last = conversation.items().iter().map(|item| item.posted.wall_ms).max().unwrap_or(link.posted.wall_ms);
    match crate::conversation::rollover_due(link.posted.wall_ms, last, now_ms, conversation.items_since_rollover()) {
        Some(why) => Next::RollOver { brain: brain.clone(), why },
        None => Next::Resume(brain.clone()),
    }
}

/// What happened since the person last spoke, for the chat's next turn: the wakes' lines, what
/// Glasscode posted, oldest first (`Conversation::since_last_spoke`).
pub fn since_text(conversation: &crate::conversation::Conversation) -> String {
    use crate::conversation::{Author, Kind};
    conversation
        .since_last_spoke()
        .iter()
        .map(|item| {
            let what = match &item.kind {
                Kind::Card { .. } => "a card for you",
                Kind::Nudge { .. } => "something to look at",
                Kind::Action { .. } => "done",
                Kind::Error => "an error",
                _ => "said",
            };
            let who = match &item.author {
                Author::Wake(_) => "a wake",
                _ => "Glasscode",
            };
            format!("- {who}, {what}: {}", item.text.lines().next().unwrap_or_default())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ----- what a brain can do on screen ---------------------------------------------------------------

/// What the chat's brain can do, by trial (M01, P01, P02; ux-9): the screen follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// Its answer comes in as it is written.
    pub streams_text: bool,
    /// Its tool calls show as they happen.
    pub streams_tool_events: bool,
    /// Stop ends the answer and keeps the conversation.
    pub interrupt: bool,
    /// A message can join a running answer.
    pub steer: bool,
    /// Its conversation is loaded again after a restart.
    pub resume: bool,
}

impl Caps {
    /// Claude's stream-json streams text and tool calls and takes an interrupt; Codex on exec
    /// streams no text (its answer comes at the end), shows its tool calls, and Stop there ends
    /// the process.
    pub fn of(agent: AgentKind) -> Caps {
        match agent {
            AgentKind::Codex => Caps { streams_text: false, streams_tool_events: true, interrupt: false, steer: false, resume: true },
            _ => Caps { streams_text: true, streams_tool_events: true, interrupt: true, steer: false, resume: true },
        }
    }

    /// What Stop says it does.
    pub fn stop_words(&self) -> &'static str {
        if self.interrupt { "Stop" } else { "Stop (ends this answer)" }
    }
}

/// The activity line for a tool call (ux-9: visible progress for both brains).
pub fn activity_line(tool: &str) -> String {
    let op = tool.strip_prefix(crate::mcp::CLAUDE_PREFIX).unwrap_or(tool).replace('_', ".");
    match crate::ops::find(&op) {
        Some(spec) => format!("{}…", capitalize(spec.summary)),
        None if tool == "ToolSearch" => "Looking for the right tool…".into(),
        None => format!("Using {tool}…"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_meaning_by_state() {
        // Off: a session, as always, and no control.
        assert_eq!(enter_meaning(false, Ok(()), StartMode::Ask), Enter { mode: StartMode::Session, why_not: None, choice: false });
        // On: the person's choice, Session until they pick Ask.
        assert_eq!(enter_meaning(true, Ok(()), StartMode::default()).mode, StartMode::Session);
        assert_eq!(enter_meaning(true, Ok(()), StartMode::Ask), Enter { mode: StartMode::Ask, why_not: None, choice: true });
        // Ask chosen and it cannot answer: a session, and the control says why.
        let out = enter_meaning(true, Err("Claude is signed out".into()), StartMode::Ask);
        assert_eq!((out.mode, out.why_not.as_deref()), (StartMode::Session, Some("Claude is signed out")));
        assert!(placeholder(StartMode::Ask, "Alex").starts_with("Ask Alex"));
        assert_eq!(placeholder(StartMode::Session, "Alex"), "Ask an agent to build, fix or review something");
    }

    #[test]
    fn queued_while_busy() {
        let mut queue = ChatQueue::default();
        assert!(send_or_queue(false, &mut queue, "first".into(), None).is_some(), "free: it goes now");
        assert!(send_or_queue(true, &mut queue, "second".into(), None).is_none());
        assert!(send_or_queue(false, &mut queue, "third".into(), None).is_none(), "behind the queued one, in order");
        let ids: Vec<u64> = queue.items().map(|q| q.id).collect();
        assert!(queue.edit(ids[1], "third, changed".into()));
        assert_eq!(queue.remove(ids[0]).unwrap().text, "second");
        assert_eq!(queue.take_next().unwrap().text, "third, changed");
        assert!(queue.take_next().is_none());
    }

    #[test]
    fn chips_from_references() {
        let sessions = vec![("8765f51a-8ad5-4e73-9df0-27adaa817aa9".to_string(), "Fix the login".to_string())];
        let text = "acme/app#12 is green; #7 still fails. Session 8765f51a is on it (8765f51a-8ad5-4e73-9df0-27adaa817aa9). Not #0 or abc/#3.";
        let chips = super::chips_from_references(text, &sessions, &["acme/app".to_string()]);
        let targets: Vec<&ChipTarget> = chips.iter().map(|c| &c.target).collect();
        assert_eq!(targets, [&ChipTarget::Pr { repo: "acme/app".into(), number: 12 }, &ChipTarget::Pr { repo: "acme/app".into(), number: 7 }, &ChipTarget::Session(sessions[0].0.clone())]);
        assert_eq!(&text[chips[0].at.clone()], "acme/app#12");
        assert_eq!(chips[2].label, "Fix the login");
        // With two repositories known, a bare #7 names none.
        let two = super::chips_from_references("#7", &[], &["a/b".into(), "c/d".into()]);
        assert!(two.is_empty());
    }

    #[test]
    fn plain_requests_need_no_model() {
        assert_eq!(start_request("Start a session to fix the login redirect.").as_deref(), Some("fix the login redirect"));
        assert_eq!(start_request("new session: Review #42").as_deref(), Some("Review #42"));
        assert!(start_request("what is the session on #1 doing?").is_none());
        assert!(start_request("start a session on ").is_none());
        assert!(asks_capabilities("What can you do?") && asks_capabilities("help") && !asks_capabilities("help me fix CI"));
        let answer = capabilities_answer("Alex", true);
        assert!(answer.contains("**Sessions**") && answer.contains("For example: `"), "{answer}");
        assert!(!answer.contains("settings.set") && !capabilities_answer("Alex", false).contains("**Work in progress**"));
    }

    /// The chat resumes its brain conversation, and starts a fresh one from a summary when one is
    /// due: none yet, the same day, a new day.
    #[test]
    fn the_brain_conversation_behind_the_screen() {
        use crate::conversation::{Author, Conversation, Kind, Record};
        let machine = crate::paths::MachineId::parse("0123456789abcdef").unwrap();
        let mut conversation = Conversation::new(0);
        let noon = crate::testkit::clock::utc_ms(2026, 11, 20, 12, 0);
        assert_eq!(next_conversation(&conversation, noon), Next::First);
        let stamp = |ms: u64| crate::identity::Hlc { wall_ms: ms, count: 0, machine: machine.clone() };
        let item = |id: &str, kind: Kind, author: Author, at: u64, text: &str| crate::conversation::Item { v: 1, id: serde_json::from_value(serde_json::json!(id)).unwrap(), topic: crate::conversation::TopicId::general(), posted: stamp(at), at: stamp(at), author, kind, text: text.into(), refs: Vec::new(), closed: false };
        conversation.take(Record::Item(item("00000000000000000000000000000001", Kind::Rollover { brain: "b1".into(), summary: None }, Author::Chat, noon - 60_000, "")));
        conversation.take(Record::Item(item("00000000000000000000000000000002", Kind::Manager { state: crate::conversation::Stream::Done }, Author::Chat, noon - 30_000, "Two run.")));
        assert_eq!(next_conversation(&conversation, noon), Next::Resume("b1".into()));
        assert!(matches!(next_conversation(&conversation, noon + 86_400_000), Next::RollOver { brain, why: crate::conversation::RollReason::NewDay } if brain == "b1"));
        // What a wake said since the person last spoke goes with the next turn.
        conversation.take(Record::Item(item("00000000000000000000000000000003", Kind::Manager { state: crate::conversation::Stream::Done }, Author::Wake("w1".into()), noon - 10_000, "CI broke on #7.")));
        assert_eq!(since_text(&conversation), "- a wake, said: CI broke on #7.");
    }

    #[test]
    fn the_screen_follows_what_the_brain_can_do() {
        let (claude, codex) = (Caps::of(AgentKind::Claude), Caps::of(AgentKind::Codex));
        assert!(claude.streams_text && claude.interrupt && !codex.streams_text && !codex.interrupt);
        assert_eq!(codex.stop_words(), "Stop (ends this answer)");
        assert_eq!(activity_line("mcp__glasscode__session_list"), format!("{}…", capitalize(crate::ops::find("session.list").unwrap().summary)));
        assert_eq!(activity_line("ToolSearch"), "Looking for the right tool…");
    }
}
