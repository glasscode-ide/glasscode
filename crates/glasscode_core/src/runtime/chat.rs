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

/// Whether the words ask what the manager cannot do: answered from the app's inventory (M33.1's
/// generated list, design section 11), with no model.
pub fn asks_limits(text: &str) -> bool {
    let words: String = text.trim().to_lowercase().replace(['’', '\''], "").chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect();
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    matches!(words.as_str(), "what cant you do" | "what cannot you do" | "what can you not do" | "what wont you do" | "what are you not allowed to do" | "what are you not able to do")
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
/// in their own words, and one way to ask, from the help's lines.
pub fn capabilities_answer(name: &str) -> String {
    let mut out = vec![format!("Here is what {name} can do for you. Ask in your own words; each line is one thing to ask for.")];
    for (prefix, title) in AREAS {
        let ops: Vec<&crate::ops::OpSpec> = crate::ops::OPS.iter().filter(|op| !op.human_only && op.name.split('.').next() == Some(prefix)).collect();
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

// ----- focus: where the person is when they send (M52.2, ux-5) --------------------------------------

/// Where a message was sent from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// The app's window on the Mac.
    Mac,
    /// A phone or glasses (glasscoded), or the `glasscode` command: no window to point at.
    Away,
}

/// What the window showed when the person sent a message: taken from `ui.state` at that moment,
/// names and ids only, never text on the screen or a selection (ux-5). It goes with the message, so
/// "this" and "here" mean what they meant then.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiSnapshot {
    pub at_ms: u64,
    pub away: bool,
    pub view: String,
    /// The session on screen (id, title), or the last one the person had open when away.
    pub session: Option<(String, String)>,
    pub panel_tab: Option<String>,
    /// The pull request or ticket selected on its screen.
    pub selected: Option<String>,
}

impl UiSnapshot {
    /// From `ui.state`'s data (version 1) at `at_ms`.
    pub fn from_state(state: &serde_json::Value, surface: Surface, at_ms: u64) -> UiSnapshot {
        let session = state["session"]["id"].as_str().map(|id| (id.to_string(), state["session"]["title"].as_str().unwrap_or_default().to_string()));
        let panel_tab = (state["right_panel"]["open"] == true).then(|| state["right_panel"]["tab"].as_str().map(str::to_string)).flatten();
        let selected = match (&state["selected"]["pr"], &state["selected"]["ticket"]) {
            (pr, _) if pr.is_object() => Some(format!("pull request {}#{}", pr["repo"].as_str().unwrap_or_default(), pr["number"])),
            (_, ticket) if ticket.is_string() => Some(format!("ticket {}", ticket.as_str().unwrap_or_default())),
            _ => None,
        };
        UiSnapshot { at_ms, away: surface == Surface::Away, view: state["view"].as_str().unwrap_or_default().to_string(), session, panel_tab, selected }
    }

    /// The block that goes before the message in the brain's turn.
    pub fn block(&self) -> String {
        let session = self.session.as_ref().map(|(id, title)| format!("the session “{title}” ({id})"));
        if self.away {
            return match session {
                Some(session) => format!("[Where the person is: not at the window (a phone, glasses or the command line). “Here” and “this” mean {session}, the last one they had open on the Mac: say so before acting on it.]"),
                None => "[Where the person is: not at the window (a phone, glasses or the command line); nothing was open on the Mac. Ask what “here” means.]".into(),
            };
        }
        let mut parts = vec![format!("the {} view", self.view)];
        parts.extend(session);
        parts.extend(self.panel_tab.as_ref().map(|tab| format!("its {tab} tab")));
        parts.extend(self.selected.clone());
        format!("[Where the person is when they sent this: {}. “Here” and “this” mean that.]", parts.join(", "))
    }
}

// ----- moving the screen (M52.2, ux-5) ----------------------------------------------------------------

/// How long after the person's message the chat may move the window in answer.
pub const MOVE_WINDOW_MS: u64 = 30_000;

/// Whether the manager may move the window now: only its chat, in answer to a message the person
/// sent in the last 30 s. A wake, a playbook or a schedule never does; what it would show becomes a
/// "Show me" chip the person can take.
pub fn may_move_screen(caller: &crate::caller::Caller, last_person_ms: Option<u64>, now_ms: u64) -> Result<(), String> {
    use crate::caller::{Caller, Lane};
    match caller {
        Caller::Manager { lane: Lane::Chat, .. } if last_person_ms.is_some_and(|at| now_ms.saturating_sub(at) <= MOVE_WINDOW_MS) => Ok(()),
        Caller::Manager { lane: Lane::Chat, .. } => Err("it moves the window only in answer to your message of the last 30 seconds".into()),
        Caller::Manager { .. } => Err("a wake, a playbook or a schedule never moves the window".into()),
        _ => Ok(()),
    }
}

/// Whether the manager's chat answers the person's own message of the last 30 s: what it shows
/// then is what they asked for, so it is shown even while they are in a session's view.
pub fn answers_the_person(caller: &crate::caller::Caller, last_person_ms: Option<u64>, now_ms: u64) -> bool {
    matches!(caller, crate::caller::Caller::Manager { lane: crate::caller::Lane::Chat, .. }) && may_move_screen(caller, last_person_ms, now_ms).is_ok()
}

/// Whether an op moves the window (an op of the Navigate class that does more than say a line).
pub fn moves_window(op: &crate::ops::Op) -> bool {
    op.spec.effect == crate::ops::EffectClass::Navigate && !matches!(op.name(), "ui.say" | "manager.say")
}

/// A "Show me" chip's matter (`Kind::Nudge`): the op it runs, for the person, when taken.
pub const SHOW_ME: &str = "show:";

pub fn show_me_matter(op: &crate::ops::Op) -> String {
    format!("{SHOW_ME}{}", serde_json::to_string(op).unwrap_or_default())
}

/// A place of the window as a "Show me" chip says it: "today" is "Today", "settings:automation"
/// is "Settings › automation", "tab:changes" is "the Changes tab".
pub fn show_me_place(name: &str) -> String {
    let cap = |w: &str| {
        let mut c = w.chars();
        c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
    };
    if let Some(section) = name.strip_prefix("settings:") {
        return format!("Settings › {}", section.replace('-', " "));
    }
    if let Some(tab) = name.strip_prefix("tab:") {
        return format!("the {} tab", cap(tab));
    }
    cap(&name.replace('-', " "))
}

/// The op of a "Show me" chip.
pub fn show_me_op(matter: &str) -> Option<crate::ops::Op> {
    let value: serde_json::Value = serde_json::from_str(matter.strip_prefix(SHOW_ME)?).ok()?;
    crate::ops::typed(value["op"].as_str()?, value["args"].clone()).ok()
}

// ----- action lines (M52.2, ux-14) ------------------------------------------------------------------

/// The Undo of an action: its inverse op, built from its result (`ops::undo_of`); none when it
/// has none (text typed into an agent, a message sent, a pull request opened).
pub fn undo_op(op: &crate::ops::Op, data: &serde_json::Value) -> Option<crate::ops::Op> {
    let undo = crate::ops::undo_of(op, data)?;
    crate::ops::typed(undo["op"].as_str()?, undo["args"].clone()).ok()
}

/// An action line's text: who acted, what the op did, and whether it can be undone.
pub fn action_line(actor: &str, what: &str, undoable: bool) -> String {
    format!("{actor}: {what}.{}", if undoable { "" } else { " Can't be undone." })
}

// ----- when it cannot think (M52.2, ux-15) ------------------------------------------------------------

/// What the person can do about a failure: one control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// Settings › Accounts.
    SignIn,
    /// Settings › Updates.
    Updates,
    /// Settings › Manager (its agent, its cap).
    ManagerSettings,
    /// Ask the last question again.
    Retry,
    /// Nothing to press: it passes by itself.
    Wait,
}

impl Recovery {
    pub fn label(self) -> &'static str {
        match self {
            Recovery::SignIn => "Sign in",
            Recovery::Updates => "Open Updates",
            Recovery::ManagerSettings => "Manager settings",
            Recovery::Retry => "Try again",
            Recovery::Wait => "",
        }
    }
}

/// Why the chat's brain cannot answer, as the chat says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Health(super::BrainHealth),
    Budget(super::Blocked),
    /// The network is down.
    Offline,
}

/// The chat's line for a failure: its sentence, what still works, and one control (ux-15).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureView {
    pub sentence: String,
    pub still_works: &'static str,
    pub recovery: Recovery,
}

/// What keeps working whatever the brain's state.
pub const STILL_WORKS: &str = "Answers from code still work: what needs you, status, what I did this week, what I can do; so do your approvals and opening places.";

/// The failure an outcome or a refusal says, if any (offline from the failure's words).
pub fn failure_of(health: Option<&super::BrainHealth>, failed: Option<&str>) -> Option<Failure> {
    use super::BrainHealth;
    if let Some(health) = health.filter(|h| **h != BrainHealth::Ready) {
        return Some(Failure::Health(health.clone()));
    }
    let text = failed?.to_lowercase();
    let offline = ["network", "enotfound", "getaddrinfo", "econnrefused", "econnreset", "fetch failed", "offline", "could not resolve"].iter().any(|w| text.contains(w));
    offline.then_some(Failure::Offline)
}

pub fn failure_view(failure: &Failure, agent: AgentKind) -> FailureView {
    use super::{Blocked, BrainHealth};
    let (sentence, recovery) = match failure {
        Failure::Offline => ("The network is down, so the manager cannot think.".to_string(), Recovery::Retry),
        Failure::Budget(blocked @ Blocked::DailyCap { .. }) => (blocked.sentence(), Recovery::ManagerSettings),
        Failure::Budget(blocked) => (blocked.sentence(), Recovery::Wait),
        Failure::Health(health) => {
            let recovery = match health {
                BrainHealth::SignedOut | BrainHealth::LoginExpiresSoon { .. } => Recovery::SignIn,
                BrainHealth::VersionNotApproved { .. } | BrainHealth::VersionTooOld { .. } => Recovery::Updates,
                BrainHealth::NotInstalled | BrainHealth::SwitchedOff | BrainHealth::ToolListWrong { .. } => Recovery::ManagerSettings,
                BrainHealth::OutOfUsage { .. } => Recovery::Wait,
                BrainHealth::Crashed { .. } | BrainHealth::Overloaded { .. } | BrainHealth::Ready => Recovery::Retry,
            };
            (health.sentence(agent), recovery)
        }
    };
    FailureView { sentence, still_works: STILL_WORKS, recovery }
}

// ----- the limit-reset hold and the other agent (ux-15) ----------------------------------------------

/// How long a hold waits when neither the CLI nor the vendor's windows said when the limit resets:
/// then it looks again.
pub const HOLD_LOOK_AGAIN_SECS: i64 = 30 * 60;

/// Questions kept: the newest this many.
const HELD_MAX: usize = 20;

/// When a used-up plan's limit resets, from the vendor's windows as the ledger last read them
/// (Unix seconds): the soonest reset of the windows that refuse (not those that let it go on, on
/// overage too), if one is still ahead. Asked again then, a question still refused is held again
/// with what the CLI says next.
pub fn reset_from_windows(windows: &std::collections::BTreeMap<String, super::budget::Window>, now_s: i64) -> Option<i64> {
    windows.values().filter(|w| !matches!(w.status.as_str(), "allowed" | "allowed_warning" | "overage" | "allowed_overage")).filter_map(|w| w.resets_at).filter(|at| *at > now_s).min()
}

/// When a question the brain could not answer is asked again by itself (Unix seconds), and whether
/// that is the limit's own reset time (false: no one said, so it looks again in 30 minutes). Only a
/// used-up plan holds a question; any other failure is the person's to see and act on.
pub fn hold_until(failure: &Failure, now_s: i64) -> Option<(i64, bool)> {
    let Failure::Health(super::BrainHealth::OutOfUsage { until }) = failure else { return None };
    Some(match until.filter(|at| *at > now_s) {
        Some(at) => (at.max(now_s + 60), true),
        None => (now_s + HOLD_LOOK_AGAIN_SECS, false),
    })
}

/// What the chat says when it holds a question: "I'll answer at 14:20, when the limit resets."
pub fn hold_line(until: i64, known: bool, now_s: i64) -> String {
    let at = crate::day::clock(until);
    let when = if crate::day::local(until) == crate::day::local(now_s) { format!("at {at}") } else { format!("on {} at {at}", crate::day::weekday_name(crate::day::local(until))) };
    if known { format!("I'll answer {when}, when the limit resets.") } else { format!("My plan's limit is used up and it did not say until when. I'll try again {when}.") }
}

/// A question held until the limit resets (`manager/held.json`): asked again by itself then,
/// as it went the first time and without being posted twice; kept across restarts.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Held {
    pub id: String,
    pub text: String,
    /// The person's words record, when they typed or said it.
    #[serde(default)]
    pub words: Option<WordsId>,
    /// The turn's text as it went to the brain: where the person was then, what they attached
    /// (ux-5). None: the question alone.
    #[serde(default)]
    pub composed: Option<String>,
    pub asked_ms: u64,
    /// When it is asked again (Unix seconds), and whether that is the limit's own reset time.
    pub until: i64,
    #[serde(default)]
    pub known: bool,
    /// The agent and account whose limit it waits for: once the manager thinks with another, it
    /// waits no more.
    #[serde(default)]
    pub agent: Option<AgentKind>,
    #[serde(default)]
    pub account: Option<String>,
}

impl Held {
    /// Whether it waits for this agent and account's limit.
    pub fn is_for(&self, agent: AgentKind, account: Option<&str>) -> bool {
        self.agent.is_none_or(|a| a == agent) && self.account.as_deref() == account
    }
}

/// `manager/held.json`: the questions waiting for the limit to reset, oldest first. Damaged, it
/// starts empty: the questions are still in the conversation, unanswered.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct HeldFile {
    pub held: Vec<Held>,
}

impl crate::store::Store for HeldFile {
    const NAME: &'static str = "manager/held";
    const VERSION: u32 = 1;
    const ON_DAMAGE: crate::store::OnDamage = crate::store::OnDamage::Empty;

    fn path() -> std::path::PathBuf {
        crate::paths::root().join("manager").join("held.json")
    }
}

impl HeldFile {
    /// Hold a question until its `until`. The same question held again (it was asked again, or
    /// the limit was still on when it was) keeps its place with the new time. Every question of
    /// the same agent and account then waits for that reset, the newest word on it: the limit is
    /// the plan's, not the question's.
    pub fn hold(&mut self, held: Held) {
        let text = held.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Some(same) = self.held.iter_mut().find(|h| h.text == text) {
            *same = Held { id: same.id.clone(), text, words: same.words.clone().or(held.words.clone()), asked_ms: same.asked_ms, ..held.clone() };
        } else {
            self.held.push(Held { id: crate::model::new_id(), text, ..held.clone() });
        }
        let (agent, account) = (held.agent, held.account.as_deref());
        for waiting in self.held.iter_mut().filter(|h| agent.is_none_or(|a| h.is_for(a, account))) {
            waiting.until = held.until;
            waiting.known = held.known;
        }
        if self.held.len() > HELD_MAX {
            let over = self.held.len() - HELD_MAX;
            self.held.drain(..over);
        }
    }

    /// Until when questions still wait for this agent and account's limit, if any do.
    pub fn holding(&self, now_s: i64, agent: AgentKind, account: Option<&str>) -> Option<(i64, bool)> {
        self.held.iter().filter(|h| h.until > now_s && h.is_for(agent, account)).map(|h| (h.until, h.known)).max()
    }

    /// The oldest question whose time has come: its limit reset, or the manager thinks with
    /// another agent or account now.
    pub fn due(&self, now_s: i64, agent: AgentKind, account: Option<&str>) -> Option<&Held> {
        self.held.iter().find(|h| h.until <= now_s || !h.is_for(agent, account))
    }

    /// Take a question out (it is being asked now, or the person asked it elsewhere).
    pub fn take(&mut self, text: &str) -> Option<Held> {
        let at = self.held.iter().position(|h| h.text == text.trim())?;
        Some(self.held.remove(at))
    }
}

/// The other CLI, the way out when one is out (ux-15): Claude's is Codex, Codex's is Claude.
pub fn other_agent(agent: AgentKind) -> Option<AgentKind> {
    match agent {
        AgentKind::Claude => Some(AgentKind::Codex),
        AgentKind::Codex => Some(AgentKind::Claude),
        AgentKind::Shell => None,
    }
}

/// Whether the other agent is a way out of this failure: the plan is used up, or the account
/// signed out. Anything else (offline, the version, the day's cap) holds for both.
pub fn other_helps(failure: &Failure) -> bool {
    matches!(failure, Failure::Health(super::BrainHealth::OutOfUsage { .. } | super::BrainHealth::SignedOut))
}

/// The button that asks the other agent: "Ask Codex instead".
pub fn other_label(other: AgentKind) -> String {
    format!("Ask {} instead", other.name())
}

// ----- runs of action lines (M52.2's follow-up) ---------------------------------------------------

/// A row of the thread: one item, or a run of action lines one after the other, shown as one line
/// ("3 actions ▸") that opens to show each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    One(usize),
    Actions(Range<usize>),
}

/// A run of this many action lines or more folds into one row.
pub const FOLD_AT: usize = 3;

/// The rows of a thread whose items are action lines or not (`actions`, in order).
pub fn rows(actions: &[bool]) -> Vec<Row> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < actions.len() {
        let run = actions[at..].iter().take_while(|a| **a).count();
        if run >= FOLD_AT {
            out.push(Row::Actions(at..at + run));
            at += run;
        } else {
            out.push(Row::One(at));
            at += 1;
        }
    }
    out
}

/// The folded run's line: "3 actions ▸", or "3 actions ▾" when it is open.
pub fn run_label(count: usize, open: bool) -> String {
    format!("{count} actions {}", if open { "▾" } else { "▸" })
}

// ----- the time budget (M52.2, ux-m3) -----------------------------------------------------------------

/// The journal's line for a chat turn: how soon its first text came after the message, how long
/// it took in all, and how it ended (measured, so the budget is a number, not a promise).
pub fn turn_event(first_text_ms: Option<u64>, total_ms: u64, ended: &str, agent: AgentKind) -> crate::journal::Event {
    let first = first_text_ms.map_or_else(|| "no text".to_string(), |ms| format!("first text after {ms} ms"));
    let mut event = crate::journal::Event::new(crate::journal_kinds::CHAT_TURN, crate::journal::Source::Manager, format!("the manager's chat answered: {first}, {total_ms} ms in all ({ended})"));
    event.data = serde_json::json!({ "first_text_ms": first_text_ms, "total_ms": total_ms, "ended": ended, "agent": agent.name() });
    event
}

// ----- a week's account (M52.2, ux-22) ---------------------------------------------------------------

/// Whether the person asks what the manager did this week.
pub fn asks_week(text: &str) -> bool {
    let words: String = text.trim().to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect();
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    ["what did you do this week", "what have you done this week", "what did you do", "weekly account", "your week"].iter().any(|w| words == *w)
}

/// "What I did this week", from the effect ledger: what ran by itself in the last 7 days, by
/// whom, how it ended, newest first (no model).
pub fn week_account(records: &[crate::effects::EffectRecord], now_ms: u64) -> String {
    use crate::effects::EffectState;
    let week = now_ms.saturating_sub(7 * 86_400_000);
    let mut recent: Vec<&crate::effects::EffectRecord> = records.iter().filter(|r| r.claimed_ms >= week).collect();
    if recent.is_empty() {
        return "Nothing ran by itself this week: no playbook, schedule or wake acted.".into();
    }
    recent.sort_by(|a, b| b.claimed_ms.cmp(&a.claimed_ms));
    let count = |f: &dyn Fn(&EffectState) -> bool| recent.iter().filter(|r| f(&r.state)).count();
    let (done, failed, open) = (count(&|s| matches!(s, EffectState::Done(_))), count(&|s| matches!(s, EffectState::Failed(_))), count(&|s| matches!(s, EffectState::Claimed | EffectState::Uncertain)));
    let mut out = vec![format!("This week {} action{} ran by themselves: {done} done, {failed} failed, {open} still open.", recent.len(), if recent.len() == 1 { "" } else { "s" })];
    for record in recent.iter().take(8) {
        let how = match &record.state {
            EffectState::Done(_) => "done".to_string(),
            EffectState::Failed(why) => format!("failed ({why})"),
            EffectState::Claimed => "running or cut off".to_string(),
            EffectState::Uncertain => "not known: you decide".to_string(),
        };
        let what = crate::ops::find(&record.op).map(|spec| spec.summary).unwrap_or(record.op.as_str());
        let on = if record.subject.is_empty() { String::new() } else { format!(" on {}", record.subject) };
        let secs = (record.claimed_ms / 1000) as i64;
        let when = format!("{} {}", crate::day::weekday_name(crate::day::local(secs)), crate::day::clock(secs));
        out.push(format!("- {when}: {}{on}, by {}: {how}", capitalize(what), if record.caller.is_empty() { "Glasscode".to_string() } else { record.caller.clone() }));
    }
    out.join("\n")
}

// ----- the composer: "/" and "@" (ux-18) --------------------------------------------------------------

/// One line of a composer's menu: what it shows, and the text it puts in the box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    pub detail: String,
    /// The box's whole text once it is chosen.
    pub text: String,
}

/// How many lines a menu shows.
pub const MENU_LINES: usize = 8;

/// The "/" menu: when the box starts with "/", the registry's actions whose words or summary
/// match what follows, each with its example; choosing one puts its words in the box, to finish
/// in one's own (no model).
pub fn slash_suggestions(text: &str) -> Vec<Suggestion> {
    let Some(query) = text.strip_prefix('/') else { return Vec::new() };
    let query = query.trim().to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut found: Vec<(u8, &crate::ops::OpSpec)> = crate::ops::OPS
        .iter()
        .filter(|op| !op.human_only)
        .filter_map(|op| {
            let words_of = op.cli.join(" ");
            let hay = format!("{} {words_of} {}", op.name, op.summary).to_lowercase();
            // Its own words first, then its name, then its summary.
            let rank = if !query.is_empty() && words_of.starts_with(query.as_str()) { 0 } else if op.name.contains(query.as_str()) { 1 } else { 2 };
            words.iter().all(|w| hay.contains(w)).then_some((rank, op))
        })
        .collect();
    found.sort_by_key(|(rank, _)| *rank);
    found
        .into_iter()
        .map(|(_, op)| op)
        .take(MENU_LINES)
        .map(|op| Suggestion { label: capitalize(op.summary), detail: op.examples.first().map(|e| e.to_string()).unwrap_or_else(|| op.cli.join(" ")), text: format!("{} ", op.cli.join(" ")) })
        .collect()
}

/// Something "@" can name: a session, a project, a pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    /// "session", "project", "pull request".
    pub kind: &'static str,
    pub label: String,
    /// What goes in the box: the form the chips read back (a session's first 8 characters,
    /// `owner/repo#n`).
    pub reference: String,
}

impl Named {
    pub fn session(id: &str, title: &str) -> Named {
        Named { kind: "session", label: title.to_string(), reference: format!("“{title}” ({})", id.chars().take(8).collect::<String>()) }
    }

    pub fn pr(repo: &str, number: u64, title: &str) -> Named {
        Named { kind: "pull request", label: if title.is_empty() { format!("{repo}#{number}") } else { format!("{repo}#{number} {title}") }, reference: format!("{repo}#{number}") }
    }

    pub fn project(name: &str) -> Named {
        Named { kind: "project", label: name.to_string(), reference: format!("the project {name}") }
    }
}

/// The "@" being typed at the end of the box: where it starts, and what follows it.
pub fn at_query(text: &str) -> Option<(usize, &str)> {
    let start = text.rfind(|c: char| c.is_whitespace()).map_or(0, |i| i + 1);
    let word = &text[start..];
    word.strip_prefix('@').map(|query| (start, query))
}

/// The "@" menu: what matches the word being typed; choosing one puts its reference in its place.
pub fn at_suggestions(text: &str, named: &[Named]) -> Vec<Suggestion> {
    let Some((start, query)) = at_query(text) else { return Vec::new() };
    let query = query.to_lowercase();
    named
        .iter()
        .filter(|n| n.label.to_lowercase().contains(&query) || n.reference.to_lowercase().contains(&query))
        .take(MENU_LINES)
        .map(|n| Suggestion { label: n.label.clone(), detail: n.kind.to_string(), text: format!("{}{} ", &text[..start], n.reference) })
        .collect()
}

// ----- long answers (ux-20) -------------------------------------------------------------------------

/// How long an answer the chat over the window shows before "Read more".
pub const READ_MORE_AT: usize = 700;

/// A long answer cut for a small surface: at the last paragraph, else line, else word before
/// `limit` characters. None: it is short enough.
pub fn clip_long(text: &str, limit: usize) -> Option<String> {
    if text.chars().count() <= limit {
        return None;
    }
    let cut = text.char_indices().nth(limit).map_or(text.len(), |(i, _)| i);
    let head = &text[..cut];
    let at = head.rfind("\n\n").filter(|i| *i > limit / 3).or_else(|| head.rfind('\n').filter(|i| *i > limit / 3)).or_else(|| head.rfind(' ')).unwrap_or(cut);
    Some(format!("{}…", head[..at].trim_end()))
}

// ----- selected text as context (ux-5) ----------------------------------------------------------------

/// Text the person attached to their next message (the "+ N selected lines" chip): never taken
/// without the chip, its secrets replaced as the words ledger does, and told to the brain as data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attached {
    /// Where it was selected ("the terminal of “Fix the login”").
    pub source: String,
    pub text: String,
}

/// The most text an attachment carries.
pub const ATTACH_MAX: usize = 20_000;

impl Attached {
    pub fn new(source: String, text: &str) -> Option<Attached> {
        let text = crate::words::scrub(text.trim_end());
        if text.trim().is_empty() {
            return None;
        }
        let text = match text.char_indices().nth(ATTACH_MAX) {
            Some((cut, _)) => format!("{}\n[… cut at {ATTACH_MAX} characters]", &text[..cut]),
            None => text,
        };
        Some(Attached { source, text })
    }

    pub fn lines(&self) -> usize {
        self.text.lines().count().max(1)
    }

    /// The chip: "+ 12 selected lines".
    pub fn chip(&self) -> String {
        let n = self.lines();
        format!("+ {n} selected line{}", if n == 1 { "" } else { "s" })
    }

    /// What the brain's turn gets: the text as data, from where it was selected.
    pub fn block(&self) -> String {
        super::wrap::wrap(&format!("text the person selected in {} and attached", self.source), &self.text)
    }
}

// ----- Undo that expires (ux-14) ----------------------------------------------------------------------

/// How long an action's Undo is offered at most.
pub const UNDO_FOR_MS: u64 = 60 * 60 * 1000;

/// What an action touched, for its Undo's expiry: its sessions and repositories, a setting by its
/// key, else the op itself (a later run of it changes what the Undo would restore).
pub fn undo_keys(op: &str, args: &serde_json::Value, targets: &crate::ops::Targets) -> Vec<String> {
    let mut keys = super::lanes::work_keys(targets);
    keys.extend(targets.projects.iter().map(|p| format!("project:{}", p.as_str())));
    if let Some(key) = args.get("key").and_then(serde_json::Value::as_str) {
        keys.push(format!("setting:{key}"));
    }
    if keys.is_empty() {
        keys.push(format!("op:{op}"));
    }
    keys
}

/// An Undo kept for an action line.
#[derive(Clone, Debug, PartialEq)]
pub struct UndoEntry {
    pub op: crate::ops::Op,
    /// What the action touched ([`undo_keys`]).
    pub keys: Vec<String>,
    /// The action's own op hash: its own record does not expire it.
    pub of: [u8; 32],
    pub at_ms: u64,
}

impl UndoEntry {
    /// Whether it still holds: within [`UNDO_FOR_MS`], and nothing changed what it touched since
    /// (`later`: each later action that changed something, by its keys and op hash).
    pub fn holds(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.at_ms) <= UNDO_FOR_MS
    }

    /// A later action that changed something it touched ends it.
    pub fn ended_by(&self, keys: &[String], hash: &[u8; 32]) -> bool {
        *hash != self.of && keys.iter().any(|key| self.keys.contains(key))
    }
}

// ----- feedback on nudges (ux-22) -------------------------------------------------------------------

/// What the person said of a nudge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feedback {
    Useful,
    NotUseful,
    /// "Fewer like this".
    Fewer,
}

impl Feedback {
    pub fn parse(word: &str) -> Option<Feedback> {
        match word.trim().to_lowercase().replace([' ', '-'], "_").as_str() {
            "useful" => Some(Feedback::Useful),
            "not_useful" => Some(Feedback::NotUseful),
            "fewer" | "fewer_like_this" => Some(Feedback::Fewer),
            _ => None,
        }
    }
}

/// The family a nudge's matter belongs to, which feedback counts against: its key up to the first
/// ":" ("pr", "ci", "needs", a playbook's id), with its project when it has one.
pub fn matter_family(matter: &str, project: Option<&str>) -> String {
    let family = matter.split(':').next().unwrap_or(matter).trim().to_lowercase();
    match project {
        Some(project) => format!("{family}@{project}"),
        None => family,
    }
}

/// `manager/feedback.json`: the person's word on each family of nudges, applied by code as a
/// threshold (ux-22): a family the person found more useless than useful, or asked for fewer of,
/// reaches the chat no more (its wakes still look, and the journal keeps what they found).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct NudgeFeedback {
    pub families: std::collections::BTreeMap<String, [u32; 3]>,
}

impl crate::store::Store for NudgeFeedback {
    const NAME: &'static str = "manager/feedback";
    const VERSION: u32 = 1;
    // Lost, the person's word is asked again: nothing is held back.
    const ON_DAMAGE: crate::store::OnDamage = crate::store::OnDamage::Empty;

    fn path() -> std::path::PathBuf {
        crate::paths::root().join("manager").join("feedback.json")
    }
}

impl NudgeFeedback {
    pub fn record(&mut self, family: &str, said: Feedback) {
        let counts = self.families.entry(family.to_string()).or_default();
        counts[said as usize] += 1;
    }

    /// Whether nudges of this family still reach the chat: useful counts for, not useful
    /// against, "fewer like this" twice against.
    pub fn shows(&self, family: &str) -> bool {
        let Some([useful, not, fewer]) = self.families.get(family) else { return true };
        i64::from(*useful) - i64::from(*not) - 2 * i64::from(*fewer) >= 0
    }
}

// ----- a matter's next check ------------------------------------------------------------------------

/// A look a wake asked for later (a matter's `next_check_minutes`): when it is due, a wake looks
/// again at that matter.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Check {
    pub matter: String,
    pub summary: String,
    pub due_ms: u64,
    #[serde(default)]
    pub projects: Vec<String>,
}

/// `manager/checks.json`: the looks the wakes asked for, one per matter (the latest asked), kept
/// across restarts.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Checks {
    pub checks: Vec<Check>,
}

impl crate::store::Store for Checks {
    const NAME: &'static str = "manager/checks";
    const VERSION: u32 = 1;
    // Lost, the looks are not taken; the matters' nudges stay in the conversation.
    const ON_DAMAGE: crate::store::OnDamage = crate::store::OnDamage::Empty;

    fn path() -> std::path::PathBuf {
        crate::paths::root().join("manager").join("checks.json")
    }
}

/// The most looks kept: the soonest stay.
pub const CHECKS_MAX: usize = 200;

impl Checks {
    /// Ask for a look at `matter` in `minutes` (at most a week on): one per matter, the latest.
    pub fn ask(&mut self, matter: &str, summary: &str, minutes: u64, projects: Vec<String>, now_ms: u64) {
        let due_ms = now_ms + minutes.clamp(1, 7 * 24 * 60) * 60_000;
        self.checks.retain(|c| c.matter != matter);
        self.checks.push(Check { matter: matter.to_string(), summary: summary.to_string(), due_ms, projects });
        self.checks.sort_by_key(|c| c.due_ms);
        self.checks.truncate(CHECKS_MAX);
    }

    /// The looks due now, taken out, as wakes for the queue.
    pub fn take_due(&mut self, now_ms: u64) -> Vec<super::queue::Wake> {
        let (due, later): (Vec<Check>, Vec<Check>) = self.checks.drain(..).partition(|c| c.due_ms <= now_ms);
        self.checks = later;
        due.into_iter()
            .map(|c| {
                let mut wake = super::queue::Wake::new(format!("check:{}", c.matter), super::queue::Priority::Playbook, now_ms);
                wake.projects = c.projects;
                wake.notes.push(super::queue::Note { source: "a look a wake asked for".into(), text: format!("Look again at {}: {}", c.matter, c.summary), outside: false });
                wake
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A "Show me" chip names the place it shows.
    #[test]
    fn a_show_me_chip_names_its_place() {
        assert_eq!(show_me_place("today"), "Today");
        assert_eq!(show_me_place("pull-requests"), "Pull requests");
        assert_eq!(show_me_place("settings:automation"), "Settings › automation");
        assert_eq!(show_me_place("tab:changes"), "the Changes tab");
    }

    /// The chat's answer to the person's own message of the last 30 s is theirs to see; a wake's,
    /// a terminal's or a late one is not (found in the journey run: "go to the session" from the
    /// chat over a session's view was never shown).
    #[test]
    fn the_chat_answers_the_person() {
        use crate::caller::{Caller, Lane};
        let chat = Caller::Manager { lane: Lane::Chat, wake: None, project: None };
        let wake = Caller::Manager { lane: Lane::Wake, wake: Some("w".into()), project: None };
        assert!(answers_the_person(&chat, Some(1_000), 1_000 + MOVE_WINDOW_MS));
        assert!(!answers_the_person(&chat, Some(1_000), 2_000 + MOVE_WINDOW_MS), "too late");
        assert!(!answers_the_person(&chat, None, 1_000), "no message of theirs");
        assert!(!answers_the_person(&wake, Some(1_000), 1_000));
        assert!(!answers_the_person(&Caller::Terminal { pid: None }, Some(1_000), 1_000));
    }

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
    fn the_limits_are_asked_in_a_few_words() {
        for words in ["What can't you do?", "what cannot you do", "What can you not do", "what won’t you do?", "What are you not allowed to do?"] {
            assert!(asks_limits(words), "{words}");
        }
        assert!(!asks_limits("what can you do") && !asks_limits("can't you do the deploy?"));
    }

    #[test]
    fn plain_requests_need_no_model() {
        assert_eq!(start_request("Start a session to fix the login redirect.").as_deref(), Some("fix the login redirect"));
        assert_eq!(start_request("new session: Review #42").as_deref(), Some("Review #42"));
        assert!(start_request("what is the session on #1 doing?").is_none());
        assert!(start_request("start a session on ").is_none());
        assert!(asks_capabilities("What can you do?") && asks_capabilities("help") && !asks_capabilities("help me fix CI"));
        let answer = capabilities_answer("Alex");
        assert!(answer.contains("**Sessions**") && answer.contains("For example: `"), "{answer}");
        assert!(!answer.contains("settings.set"));
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

    /// What the window showed when the person sent goes with the message; from away, "here" is
    /// the last session on the Mac, said back first.
    #[test]
    fn focus_snapshot_at_submit() {
        let state = serde_json::json!({ "v": 1, "view": "sessions", "session": { "id": "s1", "title": "Fix the login" }, "right_panel": { "open": true, "tab": "pr" }, "selected": { "pr": { "repo": "acme/app", "number": 7 } }, "commit_draft_length": 12 });
        let snap = UiSnapshot::from_state(&state, Surface::Mac, 5);
        assert_eq!((snap.view.as_str(), snap.session.clone(), snap.panel_tab.as_deref(), snap.selected.as_deref()), ("sessions", Some(("s1".into(), "Fix the login".into())), Some("pr"), Some("pull request acme/app#7")));
        let block = snap.block();
        assert!(block.contains("the sessions view, the session “Fix the login” (s1), its pr tab, pull request acme/app#7"), "{block}");
        let away = UiSnapshot::from_state(&state, Surface::Away, 5).block();
        assert!(away.contains("not at the window") && away.contains("say so before acting"), "{away}");
        let closed = serde_json::json!({ "view": "today", "right_panel": { "open": false, "tab": "pr" } });
        assert_eq!(UiSnapshot::from_state(&closed, Surface::Mac, 5).panel_tab, None, "a closed panel is not where they are");
    }

    #[test]
    fn no_screen_move_without_recent_ask() {
        use crate::caller::{Caller, Lane};
        let chat = Caller::Manager { lane: Lane::Chat, wake: None, project: None };
        let wake = Caller::Manager { lane: Lane::Wake, wake: Some("w1".into()), project: None };
        let playbook = Caller::Manager { lane: Lane::Wake, wake: Some("playbook:pb".into()), project: None };
        assert!(may_move_screen(&chat, Some(100_000), 120_000).is_ok());
        assert!(may_move_screen(&chat, Some(100_000), 100_000 + MOVE_WINDOW_MS + 1).is_err(), "too long after the message");
        assert!(may_move_screen(&chat, None, 1).is_err());
        assert!(may_move_screen(&wake, Some(100_000), 100_001).is_err() && may_move_screen(&playbook, Some(100_000), 100_001).is_err());
        assert!(may_move_screen(&Caller::Ui, None, 1).is_ok(), "the person moves it");
        let op = crate::ops::typed("ui.open", serde_json::json!({ "place": "today" })).unwrap();
        assert!(moves_window(&op));
        assert_eq!(show_me_op(&show_me_matter(&op)), Some(op));
        // Saying a line moves nothing.
        assert!(!moves_window(&crate::ops::typed("ui.say", serde_json::json!({ "text": "hi" })).unwrap()));
    }

    #[test]
    fn undo_only_for_inverse_ops() {
        let set = crate::ops::typed("settings.set", serde_json::json!({ "key": "notify.ci", "value": "false" })).unwrap();
        let undo = undo_op(&set, &serde_json::json!({ "undo": { "op": "settings.set", "args": { "key": "notify.ci", "value": "true" } } })).unwrap();
        assert_eq!((undo.name(), undo.text("value")), ("settings.set", Some("true")));
        // Text typed into a session has no inverse.
        let send = crate::ops::typed("session.send", serde_json::json!({ "session": "8765f51a-8ad5-4e73-9df0-27adaa817aa9", "message": "fix CI" })).unwrap();
        assert!(undo_op(&send, &serde_json::json!({})).is_none());
        assert!(action_line("Alex (chat)", "send a message to the session", false).ends_with("Can't be undone."));
        assert!(!action_line("You", "set notify.ci", true).contains("Can't"));
    }

    #[test]
    fn failure_state_table() {
        use crate::runtime::{Blocked, BrainHealth};
        let rows: Vec<(Failure, Recovery)> = vec![
            (Failure::Health(BrainHealth::NotInstalled), Recovery::ManagerSettings),
            (Failure::Health(BrainHealth::SignedOut), Recovery::SignIn),
            (Failure::Health(BrainHealth::LoginExpiresSoon { at: 0 }), Recovery::SignIn),
            (Failure::Health(BrainHealth::OutOfUsage { until: None }), Recovery::Wait),
            (Failure::Offline, Recovery::Retry),
            (Failure::Health(BrainHealth::VersionNotApproved { version: "9.9".into() }), Recovery::Updates),
            (Failure::Health(BrainHealth::Crashed { why: "it died".into() }), Recovery::Retry),
            (Failure::Budget(Blocked::DailyCap { cap: 10, used: 10 }), Recovery::ManagerSettings),
        ];
        for (failure, recovery) in rows {
            let view = failure_view(&failure, AgentKind::Claude);
            assert_eq!(view.recovery, recovery, "{failure:?}");
            assert!(!view.sentence.is_empty() && view.still_works.contains("Answers from code"));
        }
        assert_eq!(failure_view(&Failure::Health(BrainHealth::SignedOut), AgentKind::Claude).sentence, "The manager's Claude account is signed out. Sign in again in Settings › Accounts; until then it only answers from what it knows.");
        assert_eq!(failure_of(None, Some("fetch failed: getaddrinfo ENOTFOUND api.anthropic.com")), Some(Failure::Offline));
        assert_eq!(failure_of(Some(&BrainHealth::SignedOut), None), Some(Failure::Health(BrainHealth::SignedOut)));
        assert_eq!(failure_of(Some(&BrainHealth::Ready), Some("it said nothing")), None);
    }

    /// A used-up plan holds the question until its limit resets (from the CLI, else the vendor's
    /// windows, else half an hour), once per question, oldest first; nothing else holds.
    #[test]
    fn the_limit_reset_hold() {
        use crate::runtime::BrainHealth;
        let now = 1_800_000_000;
        assert_eq!(hold_until(&Failure::Health(BrainHealth::OutOfUsage { until: Some(now + 3600) }), now), Some((now + 3600, true)));
        assert_eq!(hold_until(&Failure::Health(BrainHealth::OutOfUsage { until: Some(now - 5) }), now), Some((now + HOLD_LOOK_AGAIN_SECS, false)), "a reset already past is no time");
        assert_eq!(hold_until(&Failure::Health(BrainHealth::OutOfUsage { until: None }), now), Some((now + HOLD_LOOK_AGAIN_SECS, false)));
        assert_eq!(hold_until(&Failure::Health(BrainHealth::SignedOut), now), None);
        assert_eq!(hold_until(&Failure::Offline, now), None);
        let window = |status: &str, resets_at| crate::runtime::budget::Window { status: status.into(), read_ms: 0, resets_at, utilization: None };
        let windows = std::collections::BTreeMap::from([
            ("five_hour".to_string(), window("rejected", Some(now + 600))),
            ("seven_day".to_string(), window("rejected", Some(now + 90_000))),
            ("seven_day_opus".to_string(), window("allowed_overage", Some(now + 300))),
            ("other".to_string(), window("allowed", Some(now + 200))),
        ]);
        assert_eq!(reset_from_windows(&windows, now), Some(now + 600), "the soonest of the windows that refuse");
        assert_eq!(hold_line(now + 3600, true, now), format!("I'll answer at {}, when the limit resets.", crate::day::clock(now + 3600)));
        assert!(hold_line(now + 3 * 86_400, true, now).starts_with("I'll answer on "));
        assert!(hold_line(now + 1800, false, now).contains("did not say until when"));
        let claude = |text: &str, asked_ms, until| Held { text: text.into(), composed: Some(format!("[where you are]\n\n{text}")), asked_ms, until, known: true, agent: Some(AgentKind::Claude), account: Some("work".into()), ..Held::default() };
        let mut file = HeldFile::default();
        file.hold(claude("what is running?", 1, now + 60));
        file.hold(claude("and the release?", 2, now + 30));
        assert_eq!(file.held.iter().map(|h| h.until).collect::<Vec<_>>(), [now + 30, now + 30], "one reset for all, the newest word on it");
        file.hold(claude("what is running?", 3, now + 90));
        assert_eq!(file.held.len(), 2, "the same question keeps its place");
        assert_eq!(file.held[0].asked_ms, 1);
        assert_eq!(file.holding(now, AgentKind::Claude, Some("work")), Some((now + 90, true)));
        assert!(file.due(now, AgentKind::Claude, Some("work")).is_none());
        assert_eq!(file.due(now, AgentKind::Codex, Some("work")).map(|h| h.text.as_str()), Some("what is running?"), "another agent waits for no limit");
        assert_eq!(file.holding(now, AgentKind::Claude, Some("home")), None, "nor another account");
        assert_eq!(file.due(now + 90, AgentKind::Claude, Some("work")).and_then(|h| h.composed.as_deref()), Some("[where you are]\n\nwhat is running?"));
        assert_eq!(file.take("what is running?").map(|h| h.asked_ms), Some(1));
        assert_eq!(file.due(now + 90, AgentKind::Claude, Some("work")).map(|h| h.text.as_str()), Some("and the release?"));
    }

    /// The other CLI is offered when this one's plan is used up or its account signed out.
    #[test]
    fn the_other_agent_way_out() {
        use crate::runtime::BrainHealth;
        assert_eq!(other_agent(AgentKind::Claude), Some(AgentKind::Codex));
        assert_eq!(other_agent(AgentKind::Codex), Some(AgentKind::Claude));
        assert_eq!(other_agent(AgentKind::Shell), None);
        assert!(other_helps(&Failure::Health(BrainHealth::OutOfUsage { until: None })));
        assert!(other_helps(&Failure::Health(BrainHealth::SignedOut)));
        assert!(!other_helps(&Failure::Offline) && !other_helps(&Failure::Health(BrainHealth::NotInstalled)));
        assert_eq!(other_label(AgentKind::Codex), "Ask Codex instead");
    }

    /// Three action lines or more in a row fold into one row; fewer, and any other line, stay.
    #[test]
    fn runs_of_action_lines_fold() {
        assert_eq!(rows(&[]), Vec::<Row>::new());
        assert_eq!(rows(&[true, true]), [Row::One(0), Row::One(1)]);
        assert_eq!(rows(&[false, true, true, true, false, true, true, true, true]), [Row::One(0), Row::Actions(1..4), Row::One(4), Row::Actions(5..9)]);
        assert_eq!((run_label(3, false).as_str(), run_label(4, true).as_str()), ("3 actions ▸", "4 actions ▾"));
    }

    #[test]
    fn the_week_from_the_effect_ledger() {
        assert!(asks_week("What did you do this week?") && !asks_week("what did you do to the login?"));
        assert!(week_account(&[], 1_795_000_000_000).starts_with("Nothing ran by itself"));
    }

    #[test]
    fn the_composer_menus() {
        let slash = slash_suggestions("/session new");
        assert_eq!(slash.first().map(|s| s.text.as_str()), Some("session new "), "{slash:?}");
        assert!(slash_suggestions("/").len() == MENU_LINES && slash_suggestions("hello").is_empty());
        assert!(!slash_suggestions("/manager stop").iter().any(|s| s.text == "manager stop "), "the person's own ops are buttons, not requests");
        let named = vec![Named::session("8765f51a-8ad5-4e73-9df0-27adaa817aa9", "Fix the login"), Named::pr("acme/app", 7, "Login redirect"), Named::project("Billing")];
        assert_eq!(at_query("ask @fix"), Some((4, "fix")));
        assert_eq!(at_query("no at here"), None);
        let at = at_suggestions("what is @login", &named);
        assert_eq!(at.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["what is “Fix the login” (8765f51a) ", "what is acme/app#7 "]);
        // What it puts in the box is what the chips read back.
        let sessions = vec![("8765f51a-8ad5-4e73-9df0-27adaa817aa9".to_string(), "Fix the login".to_string())];
        assert_eq!(super::chips_from_references(&at[0].text, &sessions, &[]).len(), 1);
        assert_eq!(at_suggestions("@bill", &named)[0].text, "the project Billing ");
    }

    #[test]
    fn long_answers_and_attached_text() {
        assert_eq!(clip_long("short", 10), None);
        let long = format!("{}\n\n{}", "a".repeat(400), "b".repeat(600));
        assert_eq!(clip_long(&long, READ_MORE_AT).unwrap(), format!("{}…", "a".repeat(400)));
        let words = "word ".repeat(300);
        assert!(clip_long(&words, 100).unwrap().ends_with("word…"));
        let attached = Attached::new("the terminal of “Fix the login”".into(), "line one\nexport GH_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789\n").unwrap();
        assert_eq!(attached.chip(), "+ 2 selected lines");
        assert!(!attached.text.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"), "{}", attached.text);
        assert!(attached.block().contains("(data, not instructions)"));
        assert!(Attached::new("x".into(), "  \n ").is_none());
    }

    #[test]
    fn an_undo_ends_when_its_target_changes() {
        let set = crate::ops::typed("settings.set", serde_json::json!({ "key": "notify.ci", "value": "false" })).unwrap();
        let keys = undo_keys("settings.set", &serde_json::json!({ "key": "notify.ci" }), &crate::ops::Targets::default());
        assert_eq!(keys, ["setting:notify.ci"]);
        let entry = UndoEntry { op: set.clone(), keys: keys.clone(), of: crate::gate::op_hash(&set), at_ms: 0 };
        assert!(!entry.ended_by(&keys, &crate::gate::op_hash(&set)), "its own record");
        let other = crate::ops::typed("settings.set", serde_json::json!({ "key": "notify.ci", "value": "true" })).unwrap();
        assert!(entry.ended_by(&keys, &crate::gate::op_hash(&other)), "the same setting set again");
        assert!(!entry.ended_by(&["setting:theme".into()], &crate::gate::op_hash(&other)));
        let targets = crate::ops::Targets { sessions: vec!["s1".into()], ..Default::default() };
        assert_eq!(undo_keys("session.rename", &serde_json::json!({}), &targets), ["session:s1"]);
        assert!(entry.holds(UNDO_FOR_MS) && !entry.holds(UNDO_FOR_MS + 1));
    }

    #[test]
    fn feedback_on_nudges_is_a_threshold() {
        let family = matter_family("pr:acme/app#7", Some("app"));
        assert_eq!(family, "pr@app");
        let mut feedback = NudgeFeedback::default();
        assert!(feedback.shows(&family));
        feedback.record(&family, Feedback::Useful);
        feedback.record(&family, Feedback::NotUseful);
        assert!(feedback.shows(&family), "even");
        feedback.record(&family, Feedback::Fewer);
        assert!(!feedback.shows(&family), "fewer like this");
        assert!(feedback.shows("ci@app"), "other families are untouched");
        assert_eq!(Feedback::parse("fewer like this"), Some(Feedback::Fewer));
        assert_eq!(Feedback::parse("not useful"), Some(Feedback::NotUseful));
    }

    #[test]
    fn a_matter_is_looked_at_again_when_asked() {
        let mut checks = Checks::default();
        checks.ask("pr:acme/app#7", "CI red", 30, vec!["app".into()], 0);
        checks.ask("pr:acme/app#7", "CI still red", 10, vec!["app".into()], 0);
        checks.ask("ci:x", "x", 60, Vec::new(), 0);
        assert_eq!(checks.checks.len(), 2, "one per matter, the latest");
        assert!(checks.take_due(9 * 60_000).is_empty());
        let due = checks.take_due(10 * 60_000);
        assert_eq!((due.len(), due[0].key.as_str(), due[0].projects.clone()), (1, "check:pr:acme/app#7", vec!["app".to_string()]));
        assert!(due[0].notes[0].text.contains("CI still red"));
        assert_eq!(checks.checks.len(), 1);
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
