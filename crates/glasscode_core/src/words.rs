//! The person message ledger (design D1): whose words a text is.
//!
//! Every input site of Glasscode records what came in through it, with an origin that Glasscode
//! sets on the input side, never the caller, the hook or the text: the app's own composers, the
//! keys the person typed into a session's terminal view, a phone (glasscoded), and everything
//! Glasscode types into a session itself, recorded before it is typed. A `UserPromptSubmit` hook
//! event or a Codex rollout user message is only *matched* against the ledger ([`match_prompt`]);
//! a prompt no record accounts for is nobody's words.
//!
//! Three tiers decide what a record can back ([`tier_of`]): one (a composer, typed spans only),
//! two (terminal keys, a device, voice), and never (everything else). Only typed spans count:
//! text pasted into a box, or put there by the app, never does ([`WordsRecord::quote`]).
//!
//! Secret spans are replaced before the text is hashed or kept ([`scrub`]), and the matcher replaces
//! them in the hook's text the same way. Each machine writes only its own folder,
//! `words/<machine>/<yyyy-mm>.jsonl` (UTC months), from one writer thread per process that stamps
//! each batch with this machine's clock ([`crate::identity::next_stamps`]) and syncs it to disk.
//!
//! The ledger keeps the person's text, so it runs only in a v2 home ([`Mode::Ledger`]); with the
//! engine at v1 nothing is written (rule 2 of the slice plan). Measure-only mode
//! (`GLASSCODE_WORDS_MEASURE=1`, proof P09) works in any home and keeps counts only: the origin,
//! the session and the lengths of the typed and pasted spans, never the text or its hash.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::caller::Caller;
use crate::identity::Hlc;
use crate::paths::{MachineId, ProjectKey};

// ----- origins and tiers -----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ComposerKind {
    TodayBox,
    ManagerChat,
    ChatView,
}

/// What Glasscode itself typed into a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InjectKind {
    /// `session send`, the manager's own messages.
    SessionSend,
    /// A built-in rule's message, such as CI fix.
    Playbook,
    /// A first prompt Glasscode made (from a ticket, a PR, `session new --message`).
    FirstPrompt,
    Handoff,
    /// Review comments sent to the agent.
    Review,
}

/// Where a text came in, set by Glasscode on the input side.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// One of the app's own message boxes.
    Composer(ComposerKind),
    /// Keys the person typed into a session's terminal view.
    TerminalKeys,
    /// A paired phone or glasses (glasscoded), by device id.
    Device(String),
    Voice,
    /// Pasted into a box or a terminal, or put in a box by the app: a span, never the person's.
    Pasted,
    Injected(InjectKind),
    Schedule,
    /// Another session's agent, by session id.
    Peer(String),
    /// A program outside Glasscode, or anything else.
    Outside,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Confirmed facts, chat grants, "remember this".
    One,
    /// "Mentioned once" entries and "Keep this?" proposals only.
    Two,
    /// Nothing: stored and shown as theirs.
    Never,
}

impl Tier {
    /// Lower is less trusted; when several records could be a prompt's, the least trusted wins.
    fn rank(self) -> u8 {
        match self {
            Tier::Never => 0,
            Tier::Two => 1,
            Tier::One => 2,
        }
    }
}

pub fn tier_of(origin: &Origin) -> Tier {
    match origin {
        Origin::Composer(_) => Tier::One,
        Origin::TerminalKeys | Origin::Device(_) | Origin::Voice => Tier::Two,
        Origin::Pasted | Origin::Injected(_) | Origin::Schedule | Origin::Peer(_) | Origin::Outside => Tier::Never,
    }
}

/// The origin of text that reaches Glasscode as a command's argument (`manager ask`, `session
/// send`), from who sent the command: never a composer. The person's own command in a Glasscode
/// shell tab is keys typed in a terminal view; an agent's is a peer's; a terminal outside Glasscode
/// is outside. The app's own buttons put no person's text in a command (a composer records its
/// own), so `Ui` here is outside too.
pub fn command_origin(caller: &Caller) -> Origin {
    match caller {
        Caller::ShellTab { .. } => Origin::TerminalKeys,
        Caller::Session { id, .. } => Origin::Peer(id.clone()),
        Caller::Manager { .. } => Origin::Injected(InjectKind::SessionSend),
        Caller::Phone { device } => Origin::Device(device.clone()),
        Caller::Ui | Caller::Machine { .. } | Caller::Terminal { .. } => Origin::Outside,
    }
}

impl ComposerKind {
    fn key(self) -> &'static str {
        match self {
            ComposerKind::TodayBox => "today_box",
            ComposerKind::ManagerChat => "manager_chat",
            ComposerKind::ChatView => "chat_view",
        }
    }
}

impl InjectKind {
    fn key(self) -> &'static str {
        match self {
            InjectKind::SessionSend => "session_send",
            InjectKind::Playbook => "playbook",
            InjectKind::FirstPrompt => "first_prompt",
            InjectKind::Handoff => "handoff",
            InjectKind::Review => "review",
        }
    }
}

/// One word in the file: `composer:today_box`, `terminal_keys`, `device:<id>`, `injected:playbook`.
impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Origin::Composer(kind) => write!(f, "composer:{}", kind.key()),
            Origin::TerminalKeys => f.write_str("terminal_keys"),
            Origin::Device(id) => write!(f, "device:{id}"),
            Origin::Voice => f.write_str("voice"),
            Origin::Pasted => f.write_str("pasted"),
            Origin::Injected(kind) => write!(f, "injected:{}", kind.key()),
            Origin::Schedule => f.write_str("schedule"),
            Origin::Peer(id) => write!(f, "peer:{id}"),
            Origin::Outside => f.write_str("outside"),
        }
    }
}

impl std::str::FromStr for Origin {
    type Err = String;

    fn from_str(text: &str) -> Result<Origin, String> {
        let composer = [ComposerKind::TodayBox, ComposerKind::ManagerChat, ComposerKind::ChatView];
        let injected = [InjectKind::SessionSend, InjectKind::Playbook, InjectKind::FirstPrompt, InjectKind::Handoff, InjectKind::Review];
        let origin = match text.split_once(':') {
            None => match text {
                "terminal_keys" => Origin::TerminalKeys,
                "voice" => Origin::Voice,
                "pasted" => Origin::Pasted,
                "schedule" => Origin::Schedule,
                "outside" => Origin::Outside,
                _ => return Err(format!("{text:?} is not an origin")),
            },
            Some(("composer", kind)) => Origin::Composer(composer.into_iter().find(|k| k.key() == kind).ok_or_else(|| format!("{text:?} is not an origin"))?),
            Some(("injected", kind)) => Origin::Injected(injected.into_iter().find(|k| k.key() == kind).ok_or_else(|| format!("{text:?} is not an origin"))?),
            Some(("device", id)) if !id.is_empty() => Origin::Device(id.to_string()),
            Some(("peer", id)) if !id.is_empty() => Origin::Peer(id.to_string()),
            _ => return Err(format!("{text:?} is not an origin")),
        };
        Ok(origin)
    }
}

impl Serialize for Origin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Origin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Origin, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

// ----- records ---------------------------------------------------------------------------------

/// A record's id: 24 hex digits, random.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WordsId(String);

impl WordsId {
    fn new() -> WordsId {
        let mut bytes = [0u8; 12];
        crate::sys::random_bytes(&mut bytes);
        WordsId(hex(&bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WordsId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One line of the ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordsRecord {
    pub id: WordsId,
    /// This machine's stamp when the writer kept it (`wall_ms` is when, for matching).
    pub at: Hlc,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectKey>,
    pub origin: Origin,
    /// With its secret spans replaced ([`scrub`]); empty in measure-only mode.
    #[serde(default)]
    pub text: String,
    /// SHA-256 of `text`; zero in measure-only mode.
    #[serde(with = "hex32")]
    pub hash: [u8; 32],
    /// Byte ranges of `text` and where each came from: the record's origin where the person typed,
    /// [`Origin::Pasted`] where they did not. In measure-only mode, of the text that was not kept.
    #[serde(default)]
    pub spans: Vec<(usize, usize, Origin)>,
    /// Measure-only mode: the text and its hash were not kept.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub measured: bool,
}

impl WordsRecord {
    pub fn tier(&self) -> Tier {
        tier_of(&self.origin)
    }

    /// Where `quote` is in the text, when all of it is in spans the person typed: never in a pasted
    /// span, never in a record whose origin counts for nothing. The first such place.
    pub fn quote(&self, quote: &str) -> Option<(usize, usize)> {
        if quote.is_empty() || self.measured || self.tier() == Tier::Never {
            return None;
        }
        let typed = |at: usize| self.spans.iter().any(|(start, end, origin)| (*start..*end).contains(&at) && tier_of(origin) != Tier::Never);
        self.text.match_indices(quote).map(|(at, _)| (at, at + quote.len())).find(|(start, end)| (*start..*end).all(typed))
    }
}

// ----- secrets ---------------------------------------------------------------------------------

/// What a secret span becomes.
pub const SECRET: &str = "[secret]";

/// The text with its secrets replaced by [`SECRET`]: tokens by their known prefixes (GitHub,
/// Anthropic, OpenAI, Slack, AWS, Linear, GitLab, npm, Google, Hugging Face), JSON web tokens,
/// private key blocks, `Bearer` tokens, the password of a URL, and the value of an assignment whose
/// name says it is a secret (`API_KEY=…`, `password: …`).
pub fn scrub(text: &str) -> String {
    replace(text, &secrets(text))
}

fn replace(text: &str, secrets: &[Range<usize>]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for secret in secrets {
        out.push_str(&text[at..secret.start]);
        out.push_str(SECRET);
        at = secret.end;
    }
    out.push_str(&text[at..]);
    out
}

/// Where a position of the text is once its secrets are replaced; one inside a secret goes to the
/// end of its replacement.
fn moved(at: usize, secrets: &[Range<usize>]) -> usize {
    let mut shift: isize = 0;
    for secret in secrets {
        if at >= secret.end {
            shift += SECRET.len() as isize - secret.len() as isize;
        } else if at > secret.start {
            return (secret.start as isize + shift) as usize + SECRET.len();
        } else {
            break;
        }
    }
    (at as isize + shift) as usize
}

/// Token prefixes, and how many token characters at least must follow.
const PREFIXES: &[(&str, usize)] = &[
    ("github_pat_", 20),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghu_", 20),
    ("ghs_", 20),
    ("ghr_", 20),
    ("sk-", 20),
    ("xoxb-", 10),
    ("xoxp-", 10),
    ("xoxa-", 10),
    ("xoxr-", 10),
    ("xoxs-", 10),
    ("xapp-", 10),
    ("glpat-", 16),
    ("lin_api_", 20),
    ("lin_oauth_", 20),
    ("npm_", 30),
    ("AKIA", 16),
    ("ASIA", 16),
    ("AIza", 30),
    ("ya29.", 20),
    ("hf_", 30),
];

/// Words in names that say the value is a secret.
const SECRET_NAMES: &[&str] = &["TOKEN", "SECRET", "PASSWORD", "PASSWD", "API_KEY", "APIKEY", "ACCESS_KEY", "PRIVATE_KEY", "AUTH_KEY", "CREDENTIAL"];

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

/// The secrets of `text`, sorted and apart.
fn secrets(text: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let starts_word = |at: usize| text[..at].chars().next_back().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
    let run = |from: usize, more: fn(char) -> bool| from + text[from..].find(|c: char| !more(c)).unwrap_or(text.len() - from);
    for (at, _) in text.char_indices() {
        if !starts_word(at) {
            continue;
        }
        let rest = &text[at..];
        // Known prefixes.
        if let Some((prefix, least)) = PREFIXES.iter().find(|(prefix, _)| rest.starts_with(prefix)) {
            let dotted = |c: char| token_char(c) || c == '.';
            let end = run(at + prefix.len(), if *prefix == "ya29." { dotted } else { token_char });
            if end - at - prefix.len() >= *least {
                found.push(at..end);
                continue;
            }
        }
        // JSON web tokens: three base64url parts.
        if rest.starts_with("eyJ") {
            let end = run(at, |c| token_char(c) || c == '.');
            let token = &text[at..end];
            if token.len() >= 30 && token.matches('.').count() == 2 {
                found.push(at..end);
                continue;
            }
        }
        // Private key blocks, to their end line (or the end of the text).
        if rest.starts_with("-----BEGIN ") && rest.lines().next().is_some_and(|line| line.contains("PRIVATE KEY")) {
            let end = rest.find("-----END ").map(|end| at + end + rest[end..].find("KEY-----").map_or(rest.len() - end, |key| key + "KEY-----".len())).unwrap_or(text.len());
            found.push(at..end.min(text.len()));
            continue;
        }
        // Bearer tokens.
        if rest.get(..7).is_some_and(|word| word.eq_ignore_ascii_case("bearer ")) {
            let from = at + 7;
            let end = run(from, |c| token_char(c) || matches!(c, '.' | '~' | '+' | '/' | '='));
            if end - from >= 16 {
                found.push(from..end);
                continue;
            }
        }
        // A name that says it is a secret, then = or :, then its value.
        let name_end = run(at, token_char);
        if name_end > at && SECRET_NAMES.iter().any(|word| text[at..name_end].to_ascii_uppercase().contains(word)) {
            let after = text[name_end..].trim_start_matches([' ', '\t']);
            if let Some(value) = after.strip_prefix(['=', ':']) {
                let value = value.trim_start_matches([' ', '\t']);
                let from = text.len() - value.len();
                let quote = value.chars().next().filter(|c| matches!(c, '"' | '\''));
                let (from, end) = match quote {
                    Some(q) => (from + 1, from + 1 + value[1..].find(q).unwrap_or(value.len() - 1)),
                    None => (from, from + value.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';')).unwrap_or(value.len())),
                };
                let secret = &text[from..end];
                let long_enough = if quote.is_some() { !secret.is_empty() } else { secret.len() >= 6 && !secret.bytes().all(|b| b.is_ascii_digit()) };
                if long_enough && secret != SECRET {
                    found.push(from..end);
                }
            }
        }
    }
    // The password of a URL: scheme://user:password@host.
    let mut from = 0;
    while let Some(found_at) = text[from..].find("://") {
        let start = from + found_at + 3;
        let authority = &text[start..start + text[start..].find(|c: char| c.is_whitespace() || c == '/').unwrap_or(text.len() - start)];
        if let (Some(at), Some(colon)) = (authority.rfind('@'), authority.find(':'))
            && colon + 1 < at
        {
            found.push(start + colon + 1..start + at);
        }
        from = start;
    }
    found.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    let mut apart: Vec<Range<usize>> = Vec::new();
    for range in found {
        match apart.last_mut() {
            Some(last) if range.start < last.end => last.end = last.end.max(range.end),
            _ => apart.push(range),
        }
    }
    apart
}

/// A prompt as the matcher compares it: secrets replaced, white space as single spaces.
fn normal(text: &str) -> String {
    scrub(text).split_whitespace().collect::<Vec<_>>().join(" ")
}

// ----- typed and pasted spans ------------------------------------------------------------------

/// Which bytes of a text box the person did not type in it: pasted, or put there by the app (a
/// draft brought back, dictation). The box calls [`Typed::edit`] for every change of its text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Typed {
    untyped: Vec<Range<usize>>,
}

impl Typed {
    /// `replaced` (byte offsets before the change) became `inserted` bytes, typed by the person or
    /// not.
    pub fn edit(&mut self, replaced: Range<usize>, inserted: usize, typed: bool) {
        // Only for offsets at or after the replaced part.
        let shift = |at: usize| at - replaced.len() + inserted;
        let mut out = Vec::new();
        for range in &self.untyped {
            if range.start < replaced.start {
                out.push(range.start..range.end.min(replaced.start));
            }
            if range.end > replaced.end {
                out.push(shift(range.start.max(replaced.end))..shift(range.end));
            }
        }
        if !typed && inserted > 0 {
            out.push(replaced.start..replaced.start + inserted);
        }
        self.untyped = merged(out);
    }

    /// The whole text was set to `len` bytes.
    pub fn set(&mut self, len: usize, typed: bool) {
        self.untyped = if typed || len == 0 { Vec::new() } else { std::iter::once(0..len).collect() };
    }

    pub fn untyped(&self) -> &[Range<usize>] {
        &self.untyped
    }

    /// The untyped ranges inside `part` of the text, counted from its start: for a text that is
    /// sent trimmed.
    pub fn within(&self, part: Range<usize>) -> Vec<Range<usize>> {
        self.untyped.iter().filter(|r| r.end > part.start && r.start < part.end).map(|r| r.start.max(part.start) - part.start..r.end.min(part.end) - part.start).collect()
    }
}

fn merged(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.retain(|r| r.start < r.end);
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        match out.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => out.push(range),
        }
    }
    out
}

/// The trimmed part of `text` (as a box sends it), and its untyped ranges counted from there.
pub fn trimmed<'a>(text: &'a str, typed: &Typed) -> (&'a str, Vec<Range<usize>>) {
    let start = text.len() - text.trim_start().len();
    let part = text.trim();
    (part, typed.within(start..start + part.len()))
}

/// The line the person is typing into a session's terminal, from the terminal view's own key and
/// paste events only (what Glasscode types never comes here). It follows typing, Backspace, the
/// line-clearing keys and newlines; other keys (arrows, history, completion) change the program's
/// line in ways it cannot see, so its text can differ from what is sent: a record that does not
/// match its prompt is nobody's words.
#[derive(Clone, Debug, Default)]
pub struct InputLine {
    text: String,
    untyped: Vec<Range<usize>>,
}

impl InputLine {
    /// The bytes a key press sent to the program; on Enter, the line (and its pasted ranges),
    /// when it holds more than white space.
    pub fn key(&mut self, bytes: &[u8]) -> Option<(String, Vec<Range<usize>>)> {
        match bytes {
            b"\r" => return self.submit(),
            b"\x1b\r" => self.text.push('\n'),
            b"\x7f" | b"\x08" => {
                self.text.pop();
                self.cut();
            }
            // Ctrl-W: the word before the cursor.
            b"\x17" => {
                let kept = self.text.trim_end().rfind(char::is_whitespace).map_or(0, |at| at + 1);
                self.text.truncate(kept);
                self.cut();
            }
            // Ctrl-U and Ctrl-C clear the line.
            b"\x15" | b"\x03" => self.clear(),
            bytes => {
                if let Ok(text) = std::str::from_utf8(bytes)
                    && !text.chars().any(char::is_control)
                {
                    self.text.push_str(text);
                }
            }
        }
        None
    }

    /// Text the person typed by other means than a key (an input method, dictation).
    pub fn typed(&mut self, text: &str) {
        self.text.push_str(&text.replace("\r\n", "\n"));
    }

    /// Text the person pasted (or dropped) into the terminal.
    pub fn paste(&mut self, text: &str) {
        let start = self.text.len();
        self.text.push_str(&text.replace("\r\n", "\n"));
        self.untyped = merged(self.untyped.drain(..).chain(std::iter::once(start..self.text.len())).collect());
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.untyped.clear();
    }

    fn cut(&mut self) {
        let len = self.text.len();
        self.untyped = merged(self.untyped.drain(..).map(|r| r.start.min(len)..r.end.min(len)).collect());
    }

    fn submit(&mut self) -> Option<(String, Vec<Range<usize>>)> {
        let text = std::mem::take(&mut self.text);
        let untyped = std::mem::take(&mut self.untyped);
        (!text.trim().is_empty()).then_some((text, untyped))
    }
}

// ----- writing ---------------------------------------------------------------------------------

/// What the ledger keeps in this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Nothing (the engine is v1, and no measuring).
    Off,
    /// Counts only, never text (proof P09).
    Measure,
    /// Records with their text (a v2 home).
    Ledger,
}

/// `GLASSCODE_WORDS_MEASURE=1`: measure-only mode, in any home.
pub const MEASURE_VARIABLE: &str = "GLASSCODE_WORDS_MEASURE";

/// The mode, once decided: 0 not yet, then [`Mode`] as 1, 2, 3.
static MODE: AtomicU8 = AtomicU8::new(0);

/// The mode of this process, decided at the first ask: [`MEASURE_VARIABLE`], else a v2 engine,
/// else off. The first ask reads the engine switch: the writer thread asks, never a caller.
pub fn mode() -> Mode {
    if let Some(mode) = known_mode() {
        return mode;
    }
    let mode = if std::env::var(MEASURE_VARIABLE).is_ok_and(|v| v == "1") {
        Mode::Measure
    } else if crate::store::EngineSwitch::current().engine == crate::store::Engine::V2 {
        Mode::Ledger
    } else {
        Mode::Off
    };
    MODE.store(mode as u8 + 1, Ordering::Relaxed);
    mode
}

/// Decide the mode again (the engine switch changed: a server that takes the Mac's, M57).
pub fn decide_again() {
    MODE.store(0, Ordering::Relaxed);
}

fn known_mode() -> Option<Mode> {
    match MODE.load(Ordering::Relaxed) {
        1 => Some(Mode::Off),
        2 => Some(Mode::Measure),
        3 => Some(Mode::Ledger),
        _ => None,
    }
}

/// A record before the writer stamps it.
struct Draft {
    id: WordsId,
    session: Option<String>,
    project: Option<ProjectKey>,
    origin: Origin,
    text: String,
    hash: [u8; 32],
    spans: Vec<(usize, usize, Origin)>,
    measured: bool,
}

enum Job {
    Record(Box<Draft>),
    Flush(mpsc::Sender<()>),
}

/// The writer thread of this process.
static WRITER: Mutex<Option<mpsc::Sender<Job>>> = Mutex::new(None);

/// Record `text`, which came in through `origin` and was typed there as a whole.
pub fn record(origin: Origin, text: &str, session: Option<&str>, project: Option<&ProjectKey>) -> WordsId {
    record_with(origin, text, &[], session, project)
}

/// Record `text`, which came in through `origin`, with the byte ranges of it the person did not
/// type there (pasted, or put in the box by the app). It does no IO: the record goes to this
/// process's writer thread, and the id comes back at once (also when the ledger is off, or cannot
/// be written: then it names nothing). Record before delivering: a prompt that arrives before its
/// record is nobody's words.
pub fn record_with(origin: Origin, text: &str, untyped: &[Range<usize>], session: Option<&str>, project: Option<&ProjectKey>) -> WordsId {
    let id = WordsId::new();
    let mode = known_mode();
    if mode == Some(Mode::Off) || text.is_empty() {
        return id;
    }
    let spans = spans_of(&origin, text, untyped);
    let measured = mode == Some(Mode::Measure) || (mode.is_none() && std::env::var(MEASURE_VARIABLE).is_ok_and(|v| v == "1"));
    let (text, hash, spans) = if measured {
        (String::new(), [0u8; 32], spans)
    } else {
        let secrets = secrets(text);
        let spans = merge_spans(spans.into_iter().map(|(start, end, origin)| (moved(start, &secrets), moved(end, &secrets), origin)).collect());
        let text = replace(text, &secrets);
        let hash = Sha256::digest(text.as_bytes()).into();
        (text, hash, spans)
    };
    let draft = Draft { id: id.clone(), session: session.map(str::to_string), project: project.cloned(), origin, text, hash, spans, measured };
    send(Job::Record(Box::new(draft)));
    id
}

/// The spans of a text: `origin` where the person typed, pasted where not.
fn spans_of(origin: &Origin, text: &str, untyped: &[Range<usize>]) -> Vec<(usize, usize, Origin)> {
    let boundary_down = |mut at: usize| {
        at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let boundary_up = |mut at: usize| {
        at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at += 1;
        }
        at
    };
    let untyped = merged(untyped.iter().map(|r| boundary_down(r.start)..boundary_up(r.end)).collect());
    let mut spans = Vec::new();
    let mut at = 0;
    for range in untyped {
        if range.start > at {
            spans.push((at, range.start, origin.clone()));
        }
        spans.push((range.start, range.end, Origin::Pasted));
        at = range.end;
    }
    if at < text.len() {
        spans.push((at, text.len(), origin.clone()));
    }
    spans
}

fn merge_spans(spans: Vec<(usize, usize, Origin)>) -> Vec<(usize, usize, Origin)> {
    let mut out: Vec<(usize, usize, Origin)> = Vec::new();
    for (start, end, origin) in spans.into_iter().filter(|(start, end, _)| start < end) {
        match out.last_mut() {
            Some(last) if last.1 >= start && last.2 == origin => last.1 = last.1.max(end),
            _ => out.push((start, end, origin)),
        }
    }
    out
}

fn send(job: Job) {
    let mut writer = WRITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let job = match writer.as_ref() {
        Some(tx) => match tx.send(job) {
            Ok(()) => return,
            Err(mpsc::SendError(job)) => job,
        },
        None => job,
    };
    // No writer yet, or it is gone: a new one.
    let (tx, rx) = mpsc::channel();
    let started = std::thread::Builder::new().name("glasscode-words".into()).spawn(move || write_loop(rx));
    if let Err(err) = started {
        log::warn!("words: could not start the writer ({err}); this record is not kept");
        return;
    }
    let _ = tx.send(job);
    *writer = Some(tx);
}

/// Wait until every record this process made so far is on disk (or dropped), for at most 10 s.
pub fn flush() {
    let (tx, rx) = mpsc::channel();
    let started = WRITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref().is_some_and(|writer| writer.send(Job::Flush(tx)).is_ok());
    if started {
        let _ = rx.recv_timeout(Duration::from_secs(10));
    }
}

fn write_loop(rx: mpsc::Receiver<Job>) {
    while let Ok(first) = rx.recv() {
        let mut jobs = vec![first];
        jobs.extend(rx.try_iter());
        let mut drafts = Vec::new();
        let mut flushes = Vec::new();
        for job in jobs {
            match job {
                Job::Record(draft) => drafts.push(*draft),
                Job::Flush(done) => flushes.push(done),
            }
        }
        if !drafts.is_empty() {
            write_batch(drafts);
        }
        for done in flushes {
            let _ = done.send(());
        }
    }
}

/// This machine's id, once read; a failure is logged once.
static MACHINE: Mutex<Option<Result<MachineId, ()>>> = Mutex::new(None);

fn machine() -> Option<MachineId> {
    let mut machine = MACHINE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    machine
        .get_or_insert_with(|| {
            MachineId::this().map_err(|err| {
                log::error!("words: {err}; the words ledger stops until the machine id can be read");
            })
        })
        .clone()
        .ok()
}

fn write_batch(drafts: Vec<Draft>) {
    if mode() == Mode::Off {
        return;
    }
    let Some(machine) = machine() else { return };
    let stamps = match crate::identity::next_stamps(&machine, drafts.len()) {
        Ok(stamps) => stamps,
        Err(err) => {
            log::error!("words: {err}; {} records are not kept", drafts.len());
            return;
        }
    };
    let mut files: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for (draft, at) in drafts.into_iter().zip(stamps) {
        let path = month_file(&machine, at.wall_ms);
        let record = WordsRecord { id: draft.id, at, session: draft.session, project: draft.project, origin: draft.origin, text: draft.text, hash: draft.hash, spans: draft.spans, measured: draft.measured };
        match serde_json::to_string(&record) {
            Ok(line) => files.entry(path).or_default().push(line),
            Err(err) => log::error!("words: a record could not be written ({err})"),
        }
    }
    for (path, lines) in files {
        if let Err(err) = append_lines(&path, &lines) {
            log::error!("words: {} records not kept in {} ({err})", lines.len(), path.display());
        }
    }
}

/// `words/<machine>/<yyyy-mm>.jsonl` of the UTC month of `at_ms`.
fn month_file(machine: &MachineId, at_ms: u64) -> PathBuf {
    let (year, month) = year_month(at_ms);
    crate::paths::words_dir(machine).join(format!("{year:04}-{month:02}.jsonl"))
}

/// The UTC year and month of a time.
fn year_month(at_ms: u64) -> (i64, u32) {
    // Howard Hinnant's civil_from_days.
    let z = (at_ms / 86_400_000) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month as u32)
}

/// Append whole lines to a JSON-lines file and sync them to disk before returning: under
/// `<file>.lock`, so writers in several processes take turns, and after a line break when the file
/// ends in a line a crash cut off (readers skip that line). A new folder is private to the person
/// (0700) and a new file too (0600), on macOS and Linux.
pub fn append_lines(path: &Path, lines: &[String]) -> std::io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let folder = path.parent().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file's path"))?;
    if !folder.is_dir() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(folder)?;
    }
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    let _lock = crate::sys::FileLock::take(Path::new(&lock_name)).map_err(|err| std::io::Error::other(err.to_string()))?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).append(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    let len = file.metadata()?.len();
    let mut text = String::new();
    if len > 0 {
        let mut last = [0u8];
        file.seek(SeekFrom::Start(len - 1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            text.push('\n');
        }
    }
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    file.write_all(text.as_bytes())?;
    file.sync_data()?;
    // A new file: its name in the folder lasts too.
    #[cfg(unix)]
    if len == 0 {
        std::fs::File::open(folder)?.sync_all()?;
    }
    Ok(())
}

// ----- reading ---------------------------------------------------------------------------------

/// The records of a file stamped at `from_ms` or later, read from its end; bad lines are skipped.
fn read_since(path: &Path, from_ms: u64) -> Vec<WordsRecord> {
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let Ok(len) = file.metadata().map(|m| m.len()) else { return Vec::new() };
    let mut tail = 64 * 1024u64;
    loop {
        let start = len.saturating_sub(tail);
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(start)).and_then(|_| file.read_to_end(&mut bytes)).is_err() {
            return Vec::new();
        }
        let text = String::from_utf8_lossy(&bytes);
        // The first line of a part read from the middle may be cut: it is read with the next part.
        let lines = text.lines().skip(usize::from(start > 0));
        let records: Vec<WordsRecord> = lines.filter_map(|line| serde_json::from_str(line).ok()).collect();
        let reached = records.first().is_some_and(|first| first.at.wall_ms < from_ms);
        if start == 0 || reached {
            return records.into_iter().filter(|r| r.at.wall_ms >= from_ms).collect();
        }
        tail *= 4;
    }
}

/// Every record of a file.
fn read_all(path: &Path) -> Vec<WordsRecord> {
    read_since(path, 0)
}

/// This machine's newest records, newest first (this month's and last month's file).
pub fn recent(limit: usize) -> Vec<WordsRecord> {
    flush();
    let Some(machine) = machine() else { return Vec::new() };
    let now = crate::clock::now_ms();
    let mut records: Vec<WordsRecord> = months_between(now.saturating_sub(31 * 86_400_000), now).iter().flat_map(|at| read_all(&month_file(&machine, *at))).collect();
    records.sort_by(|a, b| b.at.cmp(&a.at));
    records.dedup_by(|a, b| a.id == b.id);
    records.truncate(limit);
    records
}

/// This machine's own records kept after `after_ms` (their stamps' time), oldest first: what a
/// sync sends (M57). It reads the disk.
pub fn own_since(after_ms: u64) -> Vec<WordsRecord> {
    flush();
    let Some(machine) = machine() else { return Vec::new() };
    let now = crate::clock::now_ms();
    let mut records: Vec<WordsRecord> = months_between(after_ms.min(now), now).iter().flat_map(|at| read_all(&month_file(&machine, *at))).filter(|r| r.at.wall_ms > after_ms).collect();
    records.sort_by(|a, b| a.at.cmp(&b.at));
    records.dedup_by(|a, b| a.id == b.id);
    records
}

/// Every machine's records kept after `after_ms` (their stamps' time), oldest first: what the
/// manager's home learns from (M62). On the server, the Mac's records are in its folder there
/// (taken in with each sync). It reads the disk.
pub fn since(after_ms: u64) -> Vec<WordsRecord> {
    flush();
    let Ok(folders) = std::fs::read_dir(crate::paths::root().join("words")) else { return Vec::new() };
    let now = crate::clock::now_ms();
    let months = months_between(after_ms.min(now), now);
    let mut records: Vec<WordsRecord> = Vec::new();
    for folder in folders.flatten().filter(|f| f.path().is_dir()) {
        let Some(machine) = folder.file_name().to_str().and_then(MachineId::parse) else { continue };
        records.extend(months.iter().flat_map(|at| read_all(&month_file(&machine, *at))).filter(|r| r.at.wall_ms > after_ms));
    }
    records.sort_by(|a, b| a.at.cmp(&b.at));
    records.dedup_by(|a, b| a.id == b.id);
    records
}

/// Keep another machine's records (a sync, M57) in its own folder, `words/<machine>/`, filed by
/// their month; a record already there is not written again. The Mac passes the server's through
/// [`from_server`] first. Returns how many were new. It reads and writes the disk.
pub fn take_in(machine: &MachineId, records: &[WordsRecord]) -> std::io::Result<usize> {
    let mut by_file: std::collections::BTreeMap<PathBuf, Vec<&WordsRecord>> = std::collections::BTreeMap::new();
    for record in records {
        by_file.entry(month_file(machine, record.at.wall_ms)).or_default().push(record);
    }
    let mut new = 0;
    for (path, records) in by_file {
        let have: std::collections::HashSet<WordsId> = read_all(&path).into_iter().map(|r| r.id).collect();
        let lines: Vec<String> = records.iter().filter(|r| !have.contains(&r.id)).filter_map(|r| serde_json::to_string(r).ok()).collect();
        new += lines.len();
        append_lines(&path, &lines)?;
    }
    Ok(new)
}

/// A record by its id, in any machine's folder.
pub fn find(id: &WordsId) -> Option<WordsRecord> {
    flush();
    let folders = std::fs::read_dir(crate::paths::root().join("words")).ok()?;
    let mut files: Vec<PathBuf> = folders.flatten().filter_map(|folder| std::fs::read_dir(folder.path()).ok()).flatten().flatten().map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|e| e == "jsonl")).collect();
    // Newest months first.
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    files.iter().find_map(|path| read_all(path).into_iter().find(|record| &record.id == id))
}

/// Drop the records `drop` names from every machine's folder (a secret or a person forgotten:
/// the words go too, mem-13). Each file is rewritten under its writers' lock through a temporary
/// file; a line that is no record stays. The ids of those that went. It reads and writes the disk.
pub fn forget(drop: impl Fn(&WordsRecord) -> bool) -> std::io::Result<Vec<WordsId>> {
    flush();
    let Ok(folders) = std::fs::read_dir(crate::paths::root().join("words")) else { return Ok(Vec::new()) };
    let files: Vec<PathBuf> = folders.flatten().filter_map(|folder| std::fs::read_dir(folder.path()).ok()).flatten().flatten().map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|e| e == "jsonl")).collect();
    let mut gone = Vec::new();
    for path in files {
        let mut lock_name = path.as_os_str().to_owned();
        lock_name.push(".lock");
        let _lock = crate::sys::FileLock::take(Path::new(&lock_name)).map_err(|err| std::io::Error::other(err.to_string()))?;
        let text = std::fs::read_to_string(&path)?;
        let mut kept = String::with_capacity(text.len());
        let mut here = Vec::new();
        for line in text.lines() {
            if let Ok(record) = serde_json::from_str::<WordsRecord>(line)
                && drop(&record)
            {
                here.push(record.id);
                continue;
            }
            kept.push_str(line);
            kept.push('\n');
        }
        if !here.is_empty() {
            let temp = path.with_extension("jsonl.forget.tmp");
            std::fs::write(&temp, &kept)?;
            std::fs::rename(&temp, &path)?;
            gone.extend(here);
        }
    }
    Ok(gone)
}

/// Where `quote` is in record `id`, when the person typed all of it ([`WordsRecord::quote`]).
pub fn quote_in(id: &WordsId, quote: &str) -> Option<(usize, usize)> {
    find(id)?.quote(quote)
}

/// A time in each UTC month from `from` to `to`.
fn months_between(from: u64, to: u64) -> Vec<u64> {
    let mut out = vec![from];
    let mut at = from;
    while year_month(at) != year_month(to) {
        at += 86_400_000;
        if year_month(at) != year_month(*out.last().unwrap_or(&from)) {
            out.push(at);
        }
    }
    out
}

/// How long before its hook event a record can be its prompt: a message typed while the agent
/// works waits in its queue until the turn ends.
pub const MATCH_WINDOW_MS: u64 = 30 * 60 * 1000;

/// How long after the event a record still counts (the event's clock and the stamp differ a
/// little).
const SKEW_MS: u64 = 5_000;

/// Records this process matched already, with when they were made: each is one prompt's.
static MATCHED: Mutex<Option<HashMap<WordsId, u64>>> = Mutex::new(None);

/// The record a session's prompt is (a `UserPromptSubmit` event, a Codex rollout user message at
/// `at_ms`): one of this session, made in the [`MATCH_WINDOW_MS`] before it, whose text is the
/// whole prompt (secrets replaced and white space aside), and not matched before. A record that is
/// only part of the prompt is not it. When several are, the least trusted wins ([`Tier`]), then the
/// oldest. None: no record accounts for the prompt, and it is nobody's words. It reads the disk.
pub fn match_prompt(session: &str, text: &str, at_ms: u64) -> Option<WordsId> {
    if mode() != Mode::Ledger {
        return None;
    }
    flush();
    let machine = machine()?;
    let (from, to) = (at_ms.saturating_sub(MATCH_WINDOW_MS), at_ms.saturating_add(SKEW_MS));
    let wanted = normal(text);
    if wanted.is_empty() {
        return None;
    }
    let mut matched = MATCHED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let matched = matched.get_or_insert_with(HashMap::new);
    // A record made before this window's start is no candidate any more.
    matched.retain(|_, made| *made + 2 * MATCH_WINDOW_MS >= from);
    let best = months_between(from, to)
        .iter()
        .flat_map(|at| read_since(&month_file(&machine, *at), from))
        .filter(|r| !r.measured && r.session.as_deref() == Some(session) && r.at.wall_ms <= to && !matched.contains_key(&r.id) && normal(&r.text) == wanted)
        .min_by_key(|r| (r.tier().rank(), r.at.clone()))?;
    matched.insert(best.id.clone(), best.at.wall_ms);
    Some(best.id)
}

/// A record that arrived from the server (sync, M57), decided by the way it came in, not by the
/// machine it names: the Mac never takes tier one from the server, so a record with a composer
/// origin, or a span of one, is refused. The server's phone input is tier two at most.
pub fn from_server(record: WordsRecord) -> Option<WordsRecord> {
    let tier_one = record.tier() == Tier::One || record.spans.iter().any(|(_, _, origin)| tier_of(origin) == Tier::One);
    (!tier_one).then_some(record)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `[u8; 32]` as 64 hex digits.
mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::hex(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(deserializer)?;
        let mut out = [0u8; 32];
        if text.len() != 64 || !text.is_ascii() {
            return Err(serde::de::Error::custom("a hash is 64 hex digits"));
        }
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(serde::de::Error::custom)?;
        }
        Ok(out)
    }
}

/// Tests and the test kit: forget the mode, the machine id and what was matched (a test switches
/// homes, or the engine).
#[cfg(any(test, feature = "testkit"))]
pub fn reset() {
    flush();
    MODE.store(0, Ordering::Relaxed);
    *MACHINE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    *MATCHED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

#[cfg(test)]
// Lists of byte ranges, one long or more.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    /// A v2 scratch home of the test's own (or a v1 one, or one measuring), until dropped.
    struct Home {
        dir: tempfile::TempDir,
        before: Vec<(&'static str, Option<OsString>)>,
    }

    impl Home {
        fn new(engine: Option<&str>, measure: bool) -> Home {
            let dir = tempfile::tempdir().unwrap();
            let names = ["GLASSCODE_HOME", crate::store::ENGINE_VARIABLE, MEASURE_VARIABLE];
            let before = names.map(|name| (name, std::env::var_os(name))).to_vec();
            // SAFETY: the core tests run one at a time (`--test-threads=1`).
            unsafe {
                std::env::set_var("GLASSCODE_HOME", dir.path());
                match engine {
                    Some(engine) => std::env::set_var(crate::store::ENGINE_VARIABLE, engine),
                    None => std::env::remove_var(crate::store::ENGINE_VARIABLE),
                }
                if measure {
                    std::env::set_var(MEASURE_VARIABLE, "1");
                } else {
                    std::env::remove_var(MEASURE_VARIABLE);
                }
            }
            reset();
            Home { dir, before }
        }

        fn v2() -> Home {
            Home::new(Some("v2"), false)
        }

        fn files(&self) -> Vec<PathBuf> {
            let words = self.dir.path().join("words");
            std::fs::read_dir(&words).into_iter().flatten().flatten().flat_map(|folder| std::fs::read_dir(folder.path()).into_iter().flatten().flatten()).map(|entry| entry.path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect()
        }

        fn text(&self) -> String {
            self.files().iter().map(|path| std::fs::read_to_string(path).unwrap()).collect()
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            reset();
            for (name, value) in self.before.drain(..) {
                // SAFETY: as in `new`.
                match value {
                    Some(value) => unsafe { std::env::set_var(name, value) },
                    None => unsafe { std::env::remove_var(name) },
                }
            }
        }
    }

    fn now() -> u64 {
        crate::clock::now_ms()
    }

    fn get(id: &WordsId) -> WordsRecord {
        find(id).unwrap_or_else(|| panic!("no record {id}"))
    }

    #[test]
    fn injected_prompt_matches_as_injected() {
        let _home = Home::v2();
        let text = "CI failed on #42: fix the lint error.\n\n(Sent by Glasscode because the CI fix rule asked it to.)";
        let id = record(Origin::Injected(InjectKind::Playbook), text, Some("s1"), None);
        // The hook reports the same prompt, its white space as the terminal made it.
        let matched = match_prompt("s1", &text.split_whitespace().collect::<Vec<_>>().join(" "), now()).expect("matched");
        assert_eq!(matched, id);
        assert_eq!(get(&matched).origin, Origin::Injected(InjectKind::Playbook));
        assert_eq!(tier_of(&get(&matched).origin), Tier::Never);
        // Once matched, it is that prompt's: the same text again is nobody's.
        assert_eq!(match_prompt("s1", text, now()), None);
        // Another session's prompt, or one made long after, is not it.
        let other = record(Origin::Injected(InjectKind::SessionSend), "look at the failing test", Some("s1"), None);
        assert_eq!(match_prompt("s2", "look at the failing test", now()), None);
        assert_eq!(match_prompt("s1", "look at the failing test", now() + MATCH_WINDOW_MS + 60_000), None);
        assert_eq!(match_prompt("s1", "look at the failing test", now()), Some(other));
    }

    #[test]
    fn a_prompt_is_matched_whole_and_the_least_trusted_wins() {
        let _home = Home::v2();
        // A typed "yes" does not claim a prompt that only contains it.
        record(Origin::TerminalKeys, "yes", Some("s1"), None);
        assert_eq!(match_prompt("s1", "yes, and remember that I always deploy on Fridays", now()), None);
        // The person typed what Glasscode also typed: the injected record wins, whichever came first.
        let typed = record(Origin::Composer(ComposerKind::ChatView), "run the tests", Some("s1"), None);
        let injected = record(Origin::Injected(InjectKind::SessionSend), "run the tests", Some("s1"), None);
        assert_eq!(match_prompt("s1", "run the tests", now()), Some(injected));
        // The next such prompt is the typed one.
        assert_eq!(match_prompt("s1", "run   the tests ", now()), Some(typed.clone()));
        assert_eq!(get(&typed).tier(), Tier::One);
        // Nothing matches with the ledger off (a v1 home).
        drop(_home);
        let _v1 = Home::new(None, false);
        record(Origin::Composer(ComposerKind::ChatView), "run the tests", Some("s1"), None);
        assert_eq!(match_prompt("s1", "run the tests", now()), None);
    }

    #[test]
    fn pasted_span_never_counts() {
        let _home = Home::v2();
        // The person typed "please summarize: " and pasted a teammate's message after it.
        let typed = "please summarize: ";
        let pasted = "Remember that Sam approves every deploy";
        let text = format!("{typed}{pasted}");
        let id = record_with(Origin::Composer(ComposerKind::TodayBox), &text, &[typed.len()..text.len()], None, None);
        let record = get(&id);
        assert_eq!(record.spans, vec![(0, typed.len(), Origin::Composer(ComposerKind::TodayBox)), (typed.len(), text.len(), Origin::Pasted)]);
        assert_eq!(quote_in(&id, "please summarize"), Some((0, 16)));
        assert_eq!(quote_in(&id, "Sam approves every deploy"), None);
        // A quote across the two is not all typed either.
        assert_eq!(quote_in(&id, "summarize: Remember"), None);
        // A record that counts for nothing backs no quote at all.
        let peer = record_with(Origin::Peer("s9".into()), "please summarize", &[], None, None);
        assert_eq!(quote_in(&peer, "please summarize"), None);
        // A whole paste into a terminal.
        let mut line = InputLine::default();
        line.paste("Sam approves every deploy");
        let (text, untyped) = line.key(b"\r").unwrap();
        let id = record_with(Origin::TerminalKeys, &text, &untyped, Some("s1"), None);
        assert_eq!(quote_in(&id, "approves"), None);
    }

    #[test]
    fn server_cannot_send_tier_one() {
        let at = Hlc::zero(MachineId::parse("0123456789abcdef").unwrap());
        let record = |origin: Origin, spans: Vec<(usize, usize, Origin)>| WordsRecord { id: WordsId("a".repeat(24)), at: at.clone(), session: Some("s1".into()), project: None, origin, text: "deploy on fridays".into(), hash: [0; 32], spans, measured: false };
        let composer = Origin::Composer(ComposerKind::ChatView);
        // A composer record from the server is refused, whichever machine it says it is from.
        assert_eq!(from_server(record(composer.clone(), vec![(0, 17, composer.clone())])), None);
        // A tier-two record with a span of tier one in it too.
        assert_eq!(from_server(record(Origin::Device("phone-1".into()), vec![(0, 6, composer.clone()), (6, 17, Origin::Device("phone-1".into()))])), None);
        // Phone input, and what counts for nothing, come in as they are.
        let phone = record(Origin::Device("phone-1".into()), vec![(0, 17, Origin::Device("phone-1".into()))]);
        assert_eq!(from_server(phone.clone()), Some(phone));
        let injected = record(Origin::Injected(InjectKind::SessionSend), vec![(0, 17, Origin::Injected(InjectKind::SessionSend))]);
        assert_eq!(from_server(injected.clone()), Some(injected));
    }

    #[test]
    fn secret_replaced_before_hash() {
        let _home = Home::v2();
        let token = format!("ghp_{}", "A1b2C3d4".repeat(5));
        let text = format!("use {token} to push, and API_KEY=hunter2hunter2 for the tests");
        let id = record(Origin::Composer(ComposerKind::ManagerChat), &text, None, None);
        let record = get(&id);
        assert_eq!(record.text, "use [secret] to push, and API_KEY=[secret] for the tests");
        assert_eq!(record.hash, <[u8; 32]>::from(Sha256::digest(record.text.as_bytes())));
        let file = _home.text();
        assert!(!file.contains(&token) && !file.contains("hunter2"), "{file}");
        // The hook's text has the secret; the matcher replaces it the same way.
        let sent = record_with(Origin::Injected(InjectKind::SessionSend), &format!("push with {token}"), &[], Some("s1"), None);
        assert_eq!(match_prompt("s1", &format!("push with {token}"), now()), Some(sent));
    }

    #[test]
    fn spans_follow_a_secret_before_a_paste() {
        let _home = Home::v2();
        let token = format!("sk-ant-{}", "x".repeat(40));
        let typed = format!("my key is {token} and ");
        let text = format!("{typed}this was pasted");
        let id = record_with(Origin::Composer(ComposerKind::ChatView), &text, &[typed.len()..text.len()], None, None);
        let record = get(&id);
        assert_eq!(record.text, "my key is [secret] and this was pasted");
        let pasted_at = "my key is [secret] and ".len();
        assert_eq!(record.spans, vec![(0, pasted_at, Origin::Composer(ComposerKind::ChatView)), (pasted_at, record.text.len(), Origin::Pasted)]);
        assert_eq!(record.quote("this was pasted"), None);
        assert_eq!(record.quote("and"), Some((19, 22)));
        // A paste that starts inside a secret: the replacement takes the earlier span.
        let id = record_with(Origin::Composer(ComposerKind::ChatView), &format!("key {token} end"), &[10..12], None, None);
        let record = get(&id);
        assert_eq!(record.text, "key [secret] end");
        assert!(record.spans.iter().all(|(start, end, _)| start <= end && *end <= record.text.len()), "{:?}", record.spans);
    }

    #[test]
    fn finds_the_secrets_it_knows() {
        let jwt = format!("eyJ{}.eyJ{}.{}", "a".repeat(20), "b".repeat(20), "c".repeat(20));
        for (text, kept) in [
            (format!("token ghp_{} here", "a".repeat(36)), "token [secret] here".to_string()),
            (format!("github_pat_{}", "Z9_".repeat(20)), SECRET.to_string()),
            (format!("AKIA{}", "ABCDEFGHIJKLMNOP"), SECRET.to_string()),
            (format!("Authorization: Bearer {}", "t0k3n".repeat(5)), "Authorization: Bearer [secret]".to_string()),
            (jwt, SECRET.to_string()),
            ("export DB_PASSWORD=\"correct horse\"".to_string(), "export DB_PASSWORD=\"[secret]\"".to_string()),
            ("password: s3cr3tpass".to_string(), "password: [secret]".to_string()),
            ("open https://alex:pa55word@example.com/repo".to_string(), "open https://alex:[secret]@example.com/repo".to_string()),
            ("-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----\nafter".to_string(), "[secret]\nafter".to_string()),
        ] {
            assert_eq!(scrub(&text), kept, "{text}");
        }
        // Words that only look like it stay.
        for text in ["ask-the-team", "max_tokens=2048", "the token is in the vault", "sk-short", "https://example.com/a:b@c", "risk-assessment-for-the-new-api-gateway-rollout"] {
            assert_eq!(scrub(text), text);
        }
        // Replacing twice changes nothing more.
        let once = scrub("API_KEY=abcdef123456 and ghp_aaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(scrub(&once), once);
    }

    #[test]
    fn a_text_box_knows_what_was_typed() {
        let mut typed = Typed::default();
        // "hello " typed, "WORLD" pasted, "!" typed.
        typed.edit(0..0, 6, true);
        typed.edit(6..6, 5, false);
        typed.edit(11..11, 1, true);
        assert_eq!(typed.untyped(), [6..11]);
        // Typing inside the paste splits it; the typed byte is the person's.
        typed.edit(8..8, 1, true);
        assert_eq!(typed.untyped(), [6..8, 9..12]);
        // Deleting across the start of the paste.
        typed.edit(4..7, 0, true);
        assert_eq!(typed.untyped(), [4..5, 6..9]);
        // Replacing a pasted part by typing.
        typed.edit(6..9, 2, true);
        assert_eq!(typed.untyped(), [4..5]);
        // The app sets the whole text (a draft brought back): none of it typed.
        typed.set(12, false);
        assert_eq!(typed.untyped(), [0..12]);
        typed.set(0, false);
        assert!(typed.untyped().is_empty());
        // Sent trimmed: the ranges count from the trimmed text ("  ab " typed, "cdef" pasted).
        let mut typed = Typed::default();
        typed.edit(0..0, 5, true);
        typed.edit(5..5, 4, false);
        typed.edit(9..9, 1, true);
        assert_eq!(trimmed("  ab cdef ", &typed), ("ab cdef", vec![3..7]));
    }

    #[test]
    fn a_terminal_line_follows_the_keys() {
        let mut line = InputLine::default();
        for key in [&b"f"[..], b"i", b"x", b"x", b"\x7f", b" "] {
            assert_eq!(line.key(key), None);
        }
        line.paste("the bug\r\nin login");
        line.key(b"\x1b\r");
        line.key("é".as_bytes());
        // Arrows and other escapes do not add text.
        line.key(b"\x1b[A");
        let (text, untyped) = line.key(b"\r").unwrap();
        assert_eq!(text, "fix the bug\nin login\né");
        assert_eq!(untyped, [4..20]);
        // Empty again; Enter on nothing is no line.
        assert_eq!(line.key(b"\r"), None);
        line.key(b"abc");
        line.key(b"\x15");
        assert_eq!(line.key(b"\r"), None);
        // Backspace into a paste cuts it.
        line.paste("xyz");
        line.key(b"\x7f");
        line.key(b"Q");
        assert_eq!(line.key(b"\r"), Some(("xyQ".to_string(), vec![0..2])));
        // Ctrl-W takes the last word.
        line.key(b"one two");
        line.key(b"\x17");
        assert_eq!(line.key(b"\r"), Some(("one ".to_string(), vec![])));
    }

    #[test]
    fn measure_only_mode_keeps_no_text_even_in_a_v1_home() {
        let home = Home::new(None, true);
        let id = record_with(Origin::TerminalKeys, "deploy with API_KEY=abcdef123456 please", &[7..10], Some("s1"), None);
        record(Origin::Composer(ComposerKind::TodayBox), "fix the login bug", None, None);
        flush();
        let text = home.text();
        assert!(!text.contains("deploy") && !text.contains("login") && !text.contains("abcdef"), "{text}");
        let records = recent(10);
        assert_eq!(records.len(), 2);
        let measured = records.iter().find(|r| r.id == id).unwrap();
        assert!(measured.measured && measured.text.is_empty() && measured.hash == [0; 32]);
        assert_eq!(measured.spans, vec![(0, 7, Origin::TerminalKeys), (7, 10, Origin::Pasted), (10, 39, Origin::TerminalKeys)]);
        // Nothing matches and nothing can be quoted.
        assert_eq!(match_prompt("s1", "deploy with API_KEY=abcdef123456 please", now()), None);
        assert_eq!(measured.quote("deploy"), None);
    }

    #[test]
    fn a_v1_home_writes_nothing() {
        let home = Home::new(None, false);
        record(Origin::Composer(ComposerKind::TodayBox), "fix the login bug", None, None);
        flush();
        assert!(home.files().is_empty(), "{:?}", home.files());
        assert_eq!(mode(), Mode::Off);
    }

    #[test]
    fn bad_and_torn_lines_are_skipped() {
        let home = Home::v2();
        let first = record(Origin::TerminalKeys, "first", Some("s1"), None);
        flush();
        let file = home.files().pop().unwrap();
        // A crash cut a line off; another writer goes on after it on a line of its own.
        let mut torn = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
        torn.write_all(br#"{"id":"cut","at":{"wall"#).unwrap();
        let second = record(Origin::TerminalKeys, "second", Some("s1"), None);
        flush();
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text.lines().count(), 3, "{text}");
        let ids: Vec<WordsId> = recent(10).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, [second, first]);
        // The folder and the file are the person's alone.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(file.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        }
        assert_eq!(crate::paths::class_of(&file), crate::paths::Classified::Class(crate::paths::PathClass::Words));
    }

    #[test]
    fn records_are_stamped_and_filed_by_month() {
        let home = Home::v2();
        let a = record(Origin::TerminalKeys, "one", Some("s1"), None);
        let b = record(Origin::TerminalKeys, "two", Some("s1"), None);
        let (a, b) = (get(&a), get(&b));
        assert!(a.at < b.at);
        let machine = MachineId::this().unwrap();
        assert_eq!(a.at.machine, machine);
        let (year, month) = year_month(a.at.wall_ms);
        assert_eq!(home.files(), [crate::paths::words_dir(&machine).join(format!("{year:04}-{month:02}.jsonl"))]);
        assert_eq!(year_month(1_763_510_400_000), (2025, 11));
        assert_eq!(year_month(1_767_225_599_999), (2025, 12));
        assert_eq!(year_month(1_767_225_600_000), (2026, 1));
        assert_eq!(year_month(951_782_400_000), (2000, 2));
        assert_eq!(months_between(1_767_225_599_999 - 86_400_000, 1_767_225_600_000 + 1).len(), 2);
    }

    /// M57: a sync sends this machine's own records since the last one; another machine's records
    /// are kept in its folder once; the Mac refuses tier one from the server.
    #[test]
    fn words_sync_between_machines() {
        let _home = Home::v2();
        let a = get(&record(Origin::TerminalKeys, "one", Some("s1"), None));
        let mark = a.at.wall_ms;
        let b = get(&record(Origin::TerminalKeys, "two", Some("s1"), None));
        let sent: Vec<WordsId> = own_since(mark).into_iter().map(|r| r.id).collect();
        assert!(sent.contains(&b.id) && !sent.contains(&a.id), "only what came after the last sync");
        let server = MachineId::parse("5e7e700000000000").unwrap();
        assert_eq!(take_in(&server, &[a.clone(), b.clone()]).unwrap(), 2);
        assert_eq!(take_in(&server, &[a.clone(), b.clone()]).unwrap(), 0, "kept once");
        assert!(own_since(0).iter().all(|r| r.at.machine != server), "another machine's records are not this one's to send");
        let typed_here = get(&record(Origin::Composer(ComposerKind::TodayBox), "tier one", None, None));
        assert!(from_server(typed_here).is_none(), "the Mac never takes tier one from the server");
        assert!(from_server(b.clone()).is_some());
        // Every machine's records, for the manager's home that learns (M62): the other folder's too.
        let other = MachineId::parse("0a0a0a0a0a0a0a0a").unwrap();
        let mut theirs = b.clone();
        theirs.id = WordsId::new();
        theirs.at.machine = other.clone();
        theirs.at.wall_ms = b.at.wall_ms + 1;
        take_in(&other, std::slice::from_ref(&theirs)).unwrap();
        let all: Vec<WordsId> = since(mark).into_iter().map(|r| r.id).collect();
        assert!(all.contains(&b.id) && all.contains(&theirs.id) && !all.contains(&a.id), "{all:?}");
    }

    #[test]
    fn origins_round_trip() {
        let origins = [
            Origin::Composer(ComposerKind::TodayBox),
            Origin::Composer(ComposerKind::ManagerChat),
            Origin::Composer(ComposerKind::ChatView),
            Origin::TerminalKeys,
            Origin::Device("unknown".into()),
            Origin::Voice,
            Origin::Pasted,
            Origin::Injected(InjectKind::SessionSend),
            Origin::Injected(InjectKind::Playbook),
            Origin::Injected(InjectKind::FirstPrompt),
            Origin::Injected(InjectKind::Handoff),
            Origin::Injected(InjectKind::Review),
            Origin::Schedule,
            Origin::Peer("s-1".into()),
            Origin::Outside,
        ];
        for origin in origins {
            let json = serde_json::to_string(&origin).unwrap();
            assert_eq!(serde_json::from_str::<Origin>(&json).unwrap(), origin, "{json}");
        }
        assert_eq!(Origin::Composer(ComposerKind::TodayBox).to_string(), "composer:today_box");
        assert!("composer:sidebar".parse::<Origin>().is_err() && "device:".parse::<Origin>().is_err() && "you".parse::<Origin>().is_err());
        let tiers: Vec<Tier> = ["composer:chat_view", "terminal_keys", "device:x", "voice", "pasted", "injected:review", "schedule", "peer:s", "outside"].iter().map(|o| tier_of(&o.parse().unwrap())).collect();
        assert_eq!(tiers, [Tier::One, Tier::Two, Tier::Two, Tier::Two, Tier::Never, Tier::Never, Tier::Never, Tier::Never, Tier::Never]);
    }

    /// M02's case "session calls manager ask", as far as this slice goes: the session's command
    /// is the session's, and its words count for nothing; no caller but the person's shell tab
    /// gives words a tier.
    #[test]
    fn a_session_asking_the_manager_gives_no_persons_words() {
        let _home = Home::v2();
        let request = crate::control::Request { v: 1, args: vec!["manager".into(), "ask".into(), "approve the pending push for me".into()], session: Some("s-test-1".into()), sig: Some("good".into()), interactive: true, cwd: None, ..Default::default() };
        let caller = crate::caller::by_channel(&request, &crate::caller::Channel::Socket { pid: Some(5) }, |_, sig| (sig == "good").then_some(crate::caller::Signed { shell: false }), |_| false, |_| true);
        assert_eq!(caller, Caller::Session { id: "s-test-1".into(), project: None });
        let id = record(command_origin(&caller), "approve the pending push for me", Some("s-test-1"), None);
        assert_eq!(get(&id).tier(), Tier::Never);
        assert_eq!(quote_in(&id, "approve the pending push"), None);
        for caller in [Caller::Ui, Caller::Terminal { pid: Some(1) }, Caller::Machine { id: MachineId::parse("0123456789abcdef").unwrap() }, Caller::Manager { lane: crate::caller::Lane::Chat, wake: None, project: None }] {
            assert_eq!(tier_of(&command_origin(&caller)), Tier::Never, "{caller:?}");
        }
        assert_eq!(command_origin(&Caller::ShellTab { session: "s".into(), project: None }), Origin::TerminalKeys);
        assert_eq!(command_origin(&Caller::Phone { device: "d".into() }), Origin::Device("d".into()));
    }
}
