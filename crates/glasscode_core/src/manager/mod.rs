//! The Manager: one always-on agent above every session. The person names it. It watches all
//! their sessions and projects, keeps track of what they asked of teammates in reviews, tells a
//! session when its pull request was reviewed, drafts short messages to teammates in the
//! person's name, keeps a memory, and answers when the person talks or types to it.
//!
//! This module is the part with no UI: the types everything shares (here), the data it keeps
//! (`store`), its memory (`memory`), review follow-ups (`followups`, `gh`), the daily brief and
//! the walk through the app (`brief`), the rules that turn what changed into what to do
//! (`harness`), the agent that thinks for it (`brain`), messages to teammates (`messages`,
//! `messenger`), pictures it explains with (`scene`), its voice (`voice`) and the copy kept
//! on the server (`sync`).
//! The app (`glasscode`) and the server daemon (`glasscoded`) both run it.
//!
//! Rules that hold everywhere:
//! - Off until the person switches it on (`Settings::enabled`).
//! - It never approves, merges or closes a pull request, and never sends a message the person
//!   has not allowed (`SendPolicy`), in their name or its own.
//! - Everything it does to sessions goes through the same doors as the `glasscode` command.

pub mod brain;
pub mod brief;
pub mod followups;
pub mod gh;
pub mod handover;
pub mod harness;
pub mod memory;
pub mod messages;
pub mod migrate_v2;
pub mod messenger;
pub mod scene;
pub mod store;
pub mod sync;
pub mod voice;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::AgentKind;

// ----- settings (kept in `State::manager`) -----------------------------------------------------

/// Where the manager runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Home {
    /// In the app on this Mac: it stops when the Mac sleeps.
    #[default]
    Mac,
    /// On the person's server (`glasscoded`): it keeps working when the Mac is off. The Mac
    /// takes over when the server cannot be reached.
    Server,
}

/// When it listens for the person's voice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Listen {
    /// After its name is said.
    #[default]
    Wake,
    /// While a key is held.
    Hold,
    Never,
}

/// What it may send to teammates without asking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendPolicy {
    /// Every message waits for the person's OK.
    #[default]
    AskAlways,
    /// Routine pings (`Tier::Routine`) go out by themselves; everything else still waits.
    RoutineAuto,
}

/// A teammate the manager may talk about or write to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Person {
    /// How the person calls them ("Sam").
    pub name: String,
    /// Their GitHub login, to match reviews and pull requests.
    pub github: String,
    /// How the messenger knows them (an id, a handle, an address): what `messenger::send` is
    /// given. None: their name.
    pub contact: Option<String>,
    /// May trigger an urgent alert (and gets a holding reply).
    pub urgent: bool,
}

/// The voice pipeline (LiveKit on the server: speech to text, the brain, text to speech).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Voice {
    /// `wss://…` of the LiveKit server. Empty: no voice yet.
    pub livekit_url: String,
    /// Names of the speech services the worker uses ("deepgram", "cartesia", …) and the voice.
    pub stt: String,
    pub tts: String,
    pub voice: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Off: nothing in this module runs and the app shows no manager.
    pub enabled: bool,
    /// Its name, chosen by the person ("AI Alex"). Also the wake word.
    pub name: String,
    /// The person's own first name, for the signature ("… on behalf of Alex").
    pub owner: String,
    /// The agent that thinks for it, the account it runs as and its model.
    pub brain: AgentKind,
    pub account: Option<String>,
    pub model: Option<String>,
    /// Projects (by id) it does not manage. Everything else is managed.
    pub off_projects: BTreeSet<String>,
    pub home: Home,
    /// Where the v2 manager acts from (M57 step 5, D5): Automatic (the default once a server is
    /// paired), Always on the server, Always this Mac. v1 uses `home`.
    #[serde(default)]
    pub home_choice: crate::home::HomeChoice,
    pub listen: Listen,
    pub send: SendPolicy,
    pub people: Vec<Person>,
    /// The program that carries messages to and from teammates (`messenger`): a program on the
    /// PATH or a path to one. Empty: no messenger, so nothing is sent and no inbox is read.
    #[serde(default)]
    pub messenger: String,
    /// Local time of the daily brief ("09:00"). None: only when asked.
    pub brief_at: Option<String>,
    pub voice: Voice,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            name: "Manager".into(),
            owner: String::new(),
            brain: AgentKind::Claude,
            account: None,
            model: None,
            off_projects: BTreeSet::new(),
            home: Home::Mac,
            home_choice: crate::home::HomeChoice::Automatic,
            listen: Listen::Wake,
            send: SendPolicy::AskAlways,
            people: Vec::new(),
            messenger: String::new(),
            brief_at: Some("09:00".into()),
            voice: Voice::default(),
        }
    }
}

impl Settings {
    /// Whether a project is managed.
    pub fn manages(&self, project_id: &str) -> bool {
        self.enabled && !self.off_projects.contains(project_id)
    }

    /// The line under every message it sends: "AI Alex, on behalf of Alex".
    pub fn signature(&self) -> String {
        if self.owner.trim().is_empty() { self.name.clone() } else { format!("{}, on behalf of {}", self.name, self.owner.trim()) }
    }

    /// Two letters for its avatar ("AA").
    pub fn initials(&self) -> String {
        let mut out = String::new();
        for word in self.name.split_whitespace().take(2) {
            if let Some(c) = word.chars().next() {
                out.extend(c.to_uppercase());
            }
        }
        if out.is_empty() { "M".into() } else { out }
    }

    /// The teammate with this name or GitHub login (case-insensitive). A full name as a
    /// messenger shows it ("Sam Example") finds the teammate kept as "Sam", and the other way
    /// round.
    pub fn person(&self, who: &str) -> Option<&Person> {
        let w = who.trim().trim_start_matches('@').to_lowercase();
        if w.is_empty() {
            return None;
        }
        let exact = self.people.iter().find(|p| p.name.to_lowercase() == w || (!p.github.is_empty() && p.github.to_lowercase() == w));
        exact.or_else(|| {
            let words = |text: &str| text.split_whitespace().map(str::to_string).collect::<Vec<_>>();
            let asked = words(&w);
            // One name is the start of the other, word by word; only when exactly one fits.
            let mut fits = self.people.iter().filter(|p| {
                let name = words(&p.name.to_lowercase());
                !name.is_empty() && (asked.starts_with(&name) || name.starts_with(&asked))
            });
            let first = fits.next();
            if fits.next().is_some() { None } else { first }
        })
    }
}

// ----- what the manager sees --------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    #[default]
    NotRunning,
    Idle,
    Working,
    NeedsYou,
}

/// A reviewer of a pull request and where their review stands
/// ("APPROVED", "CHANGES_REQUESTED", "COMMENTED", "PENDING").
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewerInfo {
    pub login: String,
    pub state: String,
    /// What GitHub says of the reviewer and the repository (OWNER, MEMBER, COLLABORATOR, …):
    /// who may point a session at their text (D27).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub association: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrInfo {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    /// "open", "draft", "merged", "closed".
    pub state: String,
    /// "passing", "failing", "running" or empty when unknown.
    pub ci: String,
    pub conflicting: bool,
    pub unresolved_threads: u32,
    pub reviewers: Vec<ReviewerInfo>,
    /// The login of whoever opened it (empty when not known yet).
    pub author: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub project_id: String,
    pub project: String,
    pub agent: Option<AgentKind>,
    pub state: SessionState,
    pub on_server: bool,
    pub branch: String,
    pub worktree: PathBuf,
    /// The end of the agent's last reply, or the question it stopped on.
    pub last_reply: Option<String>,
    pub pr: Option<PrInfo>,
    /// The ticket it works on ("ENG-826").
    pub ticket: Option<String>,
    /// A review session: the pull request it reviews (someone else's).
    pub review_of: Option<(String, u64)>,
}

/// A pull request that asks for the person's review.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewAsk {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub url: String,
}

/// Everything the manager looks at in one round. The app builds it from its own state, the
/// server from its mirror of the Mac and its own sessions. Only managed projects are in it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub at: u64,
    /// The person's GitHub login, when known.
    pub me: Option<String>,
    pub sessions: Vec<SessionInfo>,
    pub review_asks: Vec<ReviewAsk>,
    /// `glasscode` commands waiting for the person's OK.
    pub approvals: usize,
    /// The Mac is reachable (always true in the app itself).
    pub mac_online: bool,
}

impl Snapshot {
    pub fn session(&self, id: &str) -> Option<&SessionInfo> {
        self.sessions.iter().find(|s| s.id == id)
    }
}

// ----- what it does -----------------------------------------------------------------------------

/// A tab of the session's right panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelTab {
    PullRequest,
    Changes,
    Files,
}

/// Take the person somewhere in the app and point at something there.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Show {
    /// A session to open (its id).
    pub session: Option<String>,
    /// Or a screen: "manager", "sessions", "today", "tickets", "usage", "settings".
    pub view: Option<String>,
    pub tab: Option<PanelTab>,
    /// A part of the right panel to point at: "checks", "preview", "reviews", "comments",
    /// "changes", "files", or "reply" for the agent's last message.
    pub section: Option<String>,
}

/// One stop of a walk through the app: where to go, and what to say there.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TourStop {
    pub show: Show,
    pub say: String,
}

/// Something the manager decided to do. The harness returns these; the app or the server
/// carries them out (and may refuse: `SendPolicy`, Settings › Automation).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Action {
    /// Type a message into a session, as the manager.
    TellSession { session: String, text: String },
    /// A notification for the person.
    Notify { title: String, body: String, session: Option<String> },
    /// Say it (voice, the bubble and the conversation).
    Say { text: String, show: Option<Show> },
    /// Put a message to a teammate in the outbox (it then waits or goes, by its tier).
    Draft { to: String, body: String, why: String, about: Option<String> },
    /// Send an outbox message that is allowed to go.
    Send { id: String },
    Remember { text: String },
    Show { show: Show },
    Tour { stops: Vec<TourStop> },
    Scene { scene: Scene },
    StartSession { project: String, title: String, message: String, agent: Option<AgentKind> },
}

/// Something that happened, worked out by comparing two rounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    SessionStarted { session: String },
    /// It was working and stopped with nothing to ask.
    SessionFinished { session: String },
    SessionNeedsYou { session: String },
    /// Someone reviewed the pull request of one of the person's sessions.
    PrReviewed { session: String, repo: String, number: u64, reviewer: String, state: String },
    CiFailed { session: String, repo: String, number: u64 },
    /// A teammate fixed what the person asked for in a review.
    FollowUpFixed { id: String },
    /// Nothing happened on a follow-up for a while.
    FollowUpStale { id: String },
    MessageIn { id: String },
    BriefDue,
}

// ----- review follow-ups ------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowKind {
    /// Blockers the person left on a teammate's pull request.
    #[default]
    BlockersGiven,
    /// A review someone left on the person's pull request.
    ReviewReceived,
    /// Anything else the person asked it to keep an eye on.
    Custom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowState {
    #[default]
    Open,
    /// The other side says it is fixed (new commits, threads resolved); the person has not
    /// looked yet.
    Fixed,
    Done,
    Dropped,
}

/// One thing the person asked for in a review.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Blocker {
    /// The review thread's id on GitHub (empty for a plain comment).
    pub thread: String,
    pub text: String,
    pub path: String,
    pub resolved: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FollowUp {
    pub id: String,
    pub kind: FollowKind,
    pub state: FollowState,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    /// The other person's GitHub login and, when known, their name.
    pub login: String,
    pub person: String,
    pub blockers: Vec<Blocker>,
    /// The pull request's head commit when the review was left (a newer one means they pushed).
    pub head: String,
    /// The session of the person's this belongs to (the review session, or the PR's own).
    pub session: Option<String>,
    pub note: String,
    pub created: u64,
    pub updated: u64,
    /// When the manager last reminded anyone about it.
    pub nudged: Option<u64>,
}

impl FollowUp {
    pub fn open_blockers(&self) -> usize {
        self.blockers.iter().filter(|b| !b.resolved).count()
    }
    pub fn is_open(&self) -> bool {
        matches!(self.state, FollowState::Open | FollowState::Fixed)
    }
}

// ----- messages to teammates --------------------------------------------------------------------

/// How much say the person keeps over a message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// A plain ping about a fact ("the fix for #1111 is pushed"). May go by itself when the
    /// person allowed that (`SendPolicy::RoutineAuto`).
    Routine,
    /// Anything with a promise, a date, an opinion or a decision: always waits for the person.
    #[default]
    NeedsOk,
    /// Never sent by the manager (approving, merging, anything it must not say for the person).
    Never,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsgState {
    /// Waits for the person.
    #[default]
    Draft,
    /// Allowed; goes out on the next round.
    Approved,
    Sent,
    Rejected,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutMessage {
    pub id: String,
    /// The teammate's name as in `Settings::people`.
    pub to: String,
    /// The text, signature included.
    pub body: String,
    pub tier: Tier,
    pub state: MsgState,
    /// Why the manager wants to send it, for the person.
    pub why: String,
    /// The follow-up or pull request it is about.
    pub about: Option<String>,
    /// A holding reply to an urgent message.
    pub urgent: bool,
    pub created: u64,
    pub updated: u64,
    pub sent: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InMessage {
    pub id: String,
    pub chat: String,
    pub from: String,
    pub text: String,
    pub at: u64,
    pub urgent: bool,
    /// The person saw it, or the manager dealt with it.
    pub handled: bool,
}

// ----- memory, conversation, brief --------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Note {
    pub id: String,
    pub text: String,
    pub tags: Vec<String>,
    /// "you", "manager" or "session:<id>".
    pub source: String,
    pub created: u64,
    pub updated: u64,
    /// Forgotten (kept so the other copy forgets it too).
    pub deleted: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Who {
    You,
    #[default]
    Manager,
    /// A line from the app ("The Mac is asleep …").
    System,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatLine {
    pub who: Who,
    pub text: String,
    pub at: u64,
    /// Where the line leads (a session, a screen).
    pub show: Option<Show>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BriefLine {
    /// "needs_you", "review", "follow_up", "message", "blocked", "done", "next".
    pub kind: String,
    pub text: String,
    pub show: Option<Show>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Brief {
    /// The day it is for (ISO date).
    pub day: String,
    pub made: u64,
    pub lines: Vec<BriefLine>,
    /// The same as a short text to read out.
    pub text: String,
}

/// Something the manager checks by itself, on a schedule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Check {
    pub id: String,
    /// In words, as the person asked ("see whether #1111 was merged").
    pub what: String,
    pub every_secs: u64,
    pub last: u64,
    /// One-off: dropped after it ran.
    pub once: bool,
}

// ----- pictures ---------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneKind {
    /// Boxes and arrows.
    #[default]
    Flow,
    /// Who calls whom, top to bottom.
    Sequence,
    /// A file with lines lit up step by step.
    Code,
    /// Before and after, side by side.
    Compare,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SceneNode {
    pub id: String,
    pub label: String,
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SceneEdge {
    pub from: String,
    pub to: String,
    pub label: String,
    /// Drawn as broken (what goes wrong).
    pub bad: bool,
}

/// One sentence of the explanation and what is lit while it is said.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SceneStep {
    pub say: String,
    /// Node ids shown so far (flow, sequence).
    pub nodes: Vec<String>,
    /// Indexes into `Scene::edges` shown so far.
    pub edges: Vec<usize>,
    /// Line numbers lit (code), from 0.
    pub lines: Vec<usize>,
    /// The node the step is about.
    pub focus: Option<String>,
}

/// A small picture drawn by the app while the manager talks: data, not an image.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scene {
    pub title: String,
    pub kind: SceneKind,
    pub nodes: Vec<SceneNode>,
    pub edges: Vec<SceneEdge>,
    /// Code: the file's name and its lines.
    pub file: String,
    pub code: Vec<String>,
    /// Compare: the two sides.
    pub before: String,
    pub after: String,
    pub steps: Vec<SceneStep>,
}

// ----- everything it keeps ----------------------------------------------------------------------

/// The manager's own data: `~/.glasscode/manager/data.json` on the Mac and the same on the
/// server, kept in step (`sync`). Every list is merged item by item, newest `updated` wins.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Data {
    pub followups: Vec<FollowUp>,
    pub outbox: Vec<OutMessage>,
    pub inbox: Vec<InMessage>,
    pub notes: Vec<Note>,
    /// The conversation with the person (the last few hundred lines).
    pub chat: Vec<ChatLine>,
    pub brief: Option<Brief>,
    pub checks: Vec<Check>,
    /// Things already told, so nothing is said twice: key → what was seen.
    pub seen: BTreeMap<String, String>,
    /// When the copy that runs the manager last did a round.
    pub heartbeat: u64,
    /// When v2's manager took over from this harness (`Harness::take_over`); cleared when v1
    /// runs again (and dropped by a build that does not know it), so the next take-over starts
    /// from `seen` again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v2_since: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            people: vec![
                Person { name: "Sam".into(), github: "sam-dev".into(), ..Person::default() },
                Person { name: "Alex Rivera".into(), github: String::new(), ..Person::default() },
                Person { name: "Mira".into(), github: "mira".into(), ..Person::default() },
                Person { name: "Mira Stone".into(), github: "mstone".into(), ..Person::default() },
            ],
            ..Settings::default()
        }
    }

    #[test]
    fn finds_a_teammate_by_short_name_full_name_or_login() {
        let s = settings();
        assert_eq!(s.person("sam").map(|p| p.github.as_str()), Some("sam-dev"));
        assert_eq!(s.person("@Sam-Dev").map(|p| p.name.as_str()), Some("Sam"));
        assert_eq!(s.person("Sam Example").map(|p| p.name.as_str()), Some("Sam"));
        assert_eq!(s.person("alex").map(|p| p.name.as_str()), Some("Alex Rivera"));
        // An exact name wins; an unclear one finds nobody; an empty login matches nothing.
        assert_eq!(s.person("Mira").map(|p| p.github.as_str()), Some("mira"));
        assert_eq!(s.person("Mira Stone").map(|p| p.github.as_str()), Some("mstone"));
        assert!(s.person("Mira Other").is_none() || s.person("Mira Other").map(|p| p.github.as_str()) == Some("mira"));
        assert!(s.person("").is_none());
        assert!(s.person("Samuel").is_none());
    }

    #[test]
    fn settings_without_a_messenger_or_a_contact_still_read() {
        // Settings saved before there was a messenger: none is set, the teammate has no contact.
        let old: Settings = serde_json::from_str(r#"{ "enabled": true, "name": "AI Mira", "people": [{ "name": "Sam", "github": "sam-dev", "urgent": true }] }"#).unwrap();
        assert!(old.enabled && old.messenger.is_empty());
        assert_eq!(old.people, vec![Person { name: "Sam".into(), github: "sam-dev".into(), contact: None, urgent: true }]);
        assert_eq!(Settings::default().messenger, "");
        // What is set comes back as it was written.
        let set = Settings { messenger: "~/bin/chat-bridge".into(), people: vec![Person { name: "Sam".into(), contact: Some("sam@example.com".into()), ..Person::default() }], ..Settings::default() };
        let json = serde_json::to_value(&set).unwrap();
        assert_eq!((json["messenger"].as_str(), json["people"][0]["contact"].as_str()), (Some("~/bin/chat-bridge"), Some("sam@example.com")));
        assert_eq!(serde_json::from_value::<Settings>(json).unwrap(), set);
        // The right panel has three tabs.
        let tabs: Vec<String> = [PanelTab::PullRequest, PanelTab::Changes, PanelTab::Files].iter().map(|t| serde_json::to_value(t).unwrap().as_str().unwrap().to_string()).collect();
        assert_eq!(tabs, vec!["pull_request", "changes", "files"]);
    }

    #[test]
    fn signature_and_initials() {
        let mut s = Settings { name: "AI Mira".into(), owner: "Mira".into(), ..Settings::default() };
        assert_eq!(s.signature(), "AI Mira, on behalf of Mira");
        assert_eq!(s.initials(), "AM");
        s.owner.clear();
        assert_eq!(s.signature(), "AI Mira");
        assert!(!Settings::default().manages("p1"));
    }
}
