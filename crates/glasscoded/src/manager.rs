//! The manager's copy on the server (`glasscode_core::manager`). When the person chose the
//! server as the manager's home, its rounds run here, so it keeps watching GitHub, follow-ups
//! and messages from teammates, and keeps its memory, while the Mac is off.
//!
//! The Mac calls `manager.sync` every few seconds: its settings, what it sees of its own
//! sessions, its data and the person's questions come in; the merged data, the answers and
//! what only the Mac can do (show something, speak, type into a session there) go back. What
//! waits for the Mac stays queued until the Mac says it has it.
//!
//! Here the manager types into sessions that run on this server (the same way `sessions.send`
//! does), sends the messages the person allowed (through the messenger program they set),
//! drafts, remembers, and notifies paired phones. Nothing runs while the settings say off, or that its home is the Mac.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, mpsc};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use glasscode_core::agents::{self, AgentActivity};
use glasscode_core::github;
use glasscode_core::manager::brain::{self, Brain, BrainConfig};
use glasscode_core::manager::harness::Harness;
use glasscode_core::manager::sync::{self, Answered, Ask, Queue, SyncReply, SyncRequest, TestLine};
use glasscode_core::manager::{
    Action, ChatLine, Data, FollowKind, Home, MsgState, OutMessage, Person, PrInfo, ReviewerInfo, SessionInfo, SessionState, Settings, Snapshot, Tier, Who, gh, memory, messages, messenger, store,
};
use glasscode_core::manager::messenger::MessengerError;
use glasscode_core::model::{Account, AgentKind, Defaults, Project, now};
use glasscode_core::prompt::PendingPrompt;
use glasscode_core::remote::{RemoteSession, ServerState};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::daemon::{Shared, lock, log};

const ROUND_EVERY: Duration = Duration::from_secs(5);
const REVIEWS_EVERY: Duration = Duration::from_secs(300);
const INBOX_EVERY: Duration = Duration::from_secs(120);
/// Who reviewed the pull requests of the sessions here is read this often.
const REVIEWERS_EVERY: Duration = Duration::from_secs(180);
/// The Mac counts as connected this long after its last sync.
const MAC_FRESH: u64 = 45;
/// Who reviewed a pull request that just showed up is read this soon.
const REVIEWERS_SOON: Duration = Duration::from_secs(60);
/// A message for a session that cannot take one waits this long (as on the Mac).
const TELL_KEEPS: u64 = 24 * 3600;
/// At least this long between two messages typed into the same session.
const TELL_GAP: u64 = 20;
/// How much of a session's last reply the manager is shown.
const REPLY_CHARS: usize = 400;
/// An answer the Mac never fetched is dropped after this.
const ANSWER_KEEPS: u64 = 900;
/// The longest `glasscoded manager ask` waits.
const ASK_WITHIN: Duration = Duration::from_secs(600);

/// What stays across a restart of the daemon (`~/.glasscode/manager/server.json`): the Mac may
/// be off when the daemon updates itself, and the manager goes on all the same.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Kept {
    /// The settings the Mac sent last.
    settings: Settings,
    /// What the Mac saw of its own sessions, last.
    snapshot: Option<Snapshot>,
    /// When the Mac last synced.
    mac_seen: u64,
    /// What waits for the Mac.
    queue: Queue,
    /// Projects it does not manage, by name (the server's project ids are its own).
    off_projects: Vec<String>,
    /// The person's Automation settings say nothing may type into a session.
    never_tell: bool,
}

/// An answer that waits for the Mac to fetch it.
struct Ready {
    /// The question, to tell it from another one with the same id (ids restart with the app).
    text: String,
    at: u64,
    answer: Answered,
}

/// Something to type into a session here, once it can take it.
struct Tell {
    session: String,
    text: String,
    since: u64,
}

/// What a round decided that takes time (a network call, typing): done after the round, off
/// its thread.
#[derive(Debug, PartialEq)]
enum Effect {
    Send { id: String },
    Tell { session: String, text: String },
    Push { title: String, body: String, session: Option<String>, badge: usize },
}

#[derive(Default)]
struct Server {
    kept: Kept,
    harness: Harness,
    /// Questions from the Mac being answered.
    asks: Vec<Ask>,
    ready: Vec<Ready>,
    /// The last round here (unix seconds; 0: none since it was switched on).
    last_round: u64,
    /// The person's GitHub login, once known.
    me: Option<String>,
    /// Reviewers of the pull requests of the sessions here: (repo, number) → who and how.
    reviewers: HashMap<(String, u64), Vec<ReviewerInfo>>,
    /// Outbox messages on their way out.
    sending: HashSet<String>,
    telling: Vec<Tell>,
    /// When each session here was last told something.
    told_at: HashMap<String, u64>,
    /// A pull request here waits for its reviewers to be read.
    reviewers_missing: bool,
    /// The messenger's inbox was last read up to here.
    inbox_since: u64,
    kept_dirty: bool,
    kept_saved: u64,
    save_failed: bool,
}

fn kept_file() -> PathBuf {
    store::dir().join("server.json")
}

fn state() -> MutexGuard<'static, Server> {
    static STATE: OnceLock<Mutex<Server>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(Server::load())).lock().unwrap_or_else(|e| e.into_inner())
}

/// Write a file of the manager's folder: into a temp file beside it, then renamed over it.
/// Only the owner can read it (it holds the settings and what the Mac saw of its sessions),
/// as with `data.json`.
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(dir) = path.parent().filter(|d| !d.exists()) {
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    // A temp file left from before may have been readable by others.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Why a question is not answered here.
fn why_not_here(settings: &Settings) -> String {
    if settings.enabled {
        format!("{} runs on the Mac, not on this server (Settings › Manager).", settings.name)
    } else {
        "The manager is switched off (Settings › Manager).".into()
    }
}

/// Whether a message may leave: only one that was allowed (by the person, or as a routine
/// ping when they let those go by themselves: `messages::draft` marks both), never one the
/// manager must not send, and nothing while it is switched off.
fn allowed(message: &OutMessage, settings: &Settings) -> bool {
    settings.enabled && message.state == MsgState::Approved && message.tier != Tier::Never
}

/// Who a message is handed to the messenger for: the teammate as the messenger knows them
/// (`Person::contact`), else their name.
fn address(to: &str, person: Option<&Person>) -> String {
    person.and_then(|p| p.contact.as_deref()).map(str::trim).filter(|c| !c.is_empty()).unwrap_or(to.trim()).to_string()
}

/// Send a message the person allowed, or say why it did not go, in words for the person.
/// With no messenger set that is the plain "No messenger is set up. …".
fn deliver(settings: &Settings, message: &OutMessage, person: Option<&Person>, env: &HashMap<String, String>) -> Result<(), String> {
    messenger::send(settings, &address(&message.to, person), &message.body, message.urgent, env).map_err(|error| match error.downcast_ref::<MessengerError>() {
        // The program is set on the Mac; here it is this server's PATH that counts.
        Some(MessengerError::NotFound(program)) => format!("The messenger program \u{201c}{program}\u{201d} is not on the server. Install it there, or let the manager run on the Mac (Settings › Manager)."),
        _ => format!("{error:#}"),
    })
}

/// The first line of a reason, ending like a sentence.
fn sentence(why: &str) -> String {
    let line = why.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Something went wrong");
    if line.ends_with(['.', '!', '?']) { line.to_string() } else { format!("{line}.") }
}

/// Whether the manager looks at a project here: the Mac names the ones it leaves alone.
fn managed(project: Option<&Project>, settings: &Settings, off: &[String]) -> bool {
    let Some(project) = project else { return true };
    if !settings.manages(&project.id) {
        return false;
    }
    let name = project.name.to_lowercase();
    let folder = project.root.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    !off.iter().map(|o| o.to_lowercase()).any(|o| o == name || o == folder)
}

/// The end of a text, cut at a word, with "…" in front when something is left out (the same
/// the Mac shows the manager of a session's last reply).
fn tail(text: &str, most: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= most {
        return text.to_string();
    }
    let tail: String = text.chars().skip(count - most).collect();
    let tail = match tail.find(char::is_whitespace) {
        Some(at) if at < most / 4 => tail[at..].trim_start(),
        _ => tail.as_str(),
    };
    format!("…{tail}")
}

/// The question on an agent's screen, in a line: "Do you want to proceed? (rm -rf build)".
fn question_text(prompt: &PendingPrompt) -> String {
    match prompt.detail.iter().map(|d| d.trim()).find(|d| !d.is_empty()) {
        Some(detail) => format!("{} ({})", prompt.question.trim(), detail.chars().take(160).collect::<String>()),
        None => prompt.question.trim().to_string(),
    }
}

/// What is typed into a session, in one line, saying who it is from (the same line the Mac
/// types).
fn tell_line(name: &str, text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return text;
    }
    format!("[{name}] {text} (Sent by Glasscode because {name} asked it to.)")
}

/// Whether the daemon itself notifies phones about this session right now (a question on its
/// screen, or a reply that ends in one): the manager's "needs you" would be the same twice.
fn asks_already(session: &RemoteSession) -> bool {
    let question = session.last_reply.as_deref().is_some_and(|r| r.trim().ends_with('?'));
    session.running && (session.prompt.is_some() || (session.activity != Some(AgentActivity::Working) && question))
}

/// Whether a session's agent can be told something now without getting in anyone's way:
/// running, signed in, its turn over, and not asking anything (typing would answer it).
fn can_take(session: &RemoteSession, now: u64) -> bool {
    session.running
        && !session.signed_out
        && session.prompt.is_none()
        && match session.activity {
            Some(AgentActivity::Idle) => true,
            Some(_) => false,
            // Not heard from its agent yet: a conversation quiet for a while means its turn is over.
            None => session.updated_at.is_none_or(|at| now.saturating_sub(at) >= 30),
        }
}

/// A session's pull request as the manager sees it. The person's own is None until its
/// reviewers were read, so it never shows up first without them and then with them (which
/// would read as "just reviewed"). A review session's is someone else's: nobody is told about
/// reviews on it, so it shows as it is.
fn pr_info(session: &RemoteSession, server: &ServerState, reviewers: &HashMap<(String, u64), Vec<ReviewerInfo>>) -> Option<PrInfo> {
    let pr = session.pr.as_ref()?;
    let repo = session.session.review.as_ref().map(|r| r.repo.clone()).filter(|r| !r.is_empty()).or_else(|| server.repos.get(&session.session.project_id).cloned())?;
    let reviewers = match reviewers.get(&(repo.clone(), pr.number)) {
        Some(known) => known.clone(),
        None if session.session.review.is_some() => Vec::new(),
        None => return None,
    };
    let state = match pr.state.to_uppercase().as_str() {
        "MERGED" => "merged",
        "CLOSED" => "closed",
        _ if pr.draft => "draft",
        _ => "open",
    };
    let ci = match pr.ci.as_str() {
        "passed" => "passing",
        "failed" => "failing",
        "running" => "running",
        _ => "",
    };
    Some(PrInfo {
        repo,
        number: pr.number,
        title: pr.title.clone(),
        url: pr.url.clone(),
        state: state.into(),
        ci: ci.into(),
        conflicting: pr.merge_state == "DIRTY",
        unresolved_threads: pr.unresolved,
        reviewers,
        // Not known here: the summary the daemon keeps has no author.
        author: String::new(),
    })
}

/// The sessions on this server as the manager sees them: agents only (typing into a shell
/// would run what is typed), and only in projects it manages.
fn server_sessions(server: &ServerState, settings: &Settings, off: &[String], reviewers: &HashMap<(String, u64), Vec<ReviewerInfo>>) -> Vec<SessionInfo> {
    server
        .sessions
        .iter()
        .filter(|s| s.session.agent != AgentKind::Shell)
        .filter_map(|s| {
            let project = server.projects.iter().find(|p| p.id == s.session.project_id);
            if !managed(project, settings, off) {
                return None;
            }
            // The same reading the Mac has of a session here, so nothing looks changed when
            // the manager moves between the two.
            let state = match s.activity {
                _ if s.prompt.is_some() => SessionState::NeedsYou,
                Some(AgentActivity::NeedsInput) => SessionState::NeedsYou,
                Some(AgentActivity::Working) => SessionState::Working,
                _ if s.running => SessionState::Idle,
                _ => SessionState::NotRunning,
            };
            Some(SessionInfo {
                id: s.session.id.clone(),
                title: s.session.title.clone(),
                project_id: s.session.project_id.clone(),
                project: project.map(|p| p.name.clone()).unwrap_or_default(),
                agent: Some(s.session.agent),
                state,
                on_server: true,
                branch: s.session.branch.clone(),
                worktree: s.session.worktree.clone(),
                last_reply: s.prompt.as_ref().map(question_text).or_else(|| s.last_reply.as_deref().map(|r| tail(r, REPLY_CHARS))),
                pr: pr_info(s, server, reviewers),
                ticket: None,
                review_of: s.session.review.as_ref().map(|r| (r.repo.clone(), r.number)),
            })
        })
        .collect()
}

impl Server {
    fn load() -> Server {
        let kept: Kept = std::fs::read(kept_file()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Server { kept, harness: Harness::load(), ..Server::default() }
    }

    /// The manager runs here: switched on, and the server is its home.
    /// The v1 manager runs here only with the engine at v1: under v2 the wake lane is the server's
    /// manager (M57, `crate::brain`), and two would act on the same events.
    fn active(&self) -> bool {
        self.kept.settings.enabled && self.kept.settings.home == Home::Server && glasscode_core::store::EngineSwitch::current().engine != glasscode_core::store::Engine::V2
    }

    fn mac_online(&self, now: u64) -> bool {
        self.kept.snapshot.is_some() && now.saturating_sub(self.kept.mac_seen) <= MAC_FRESH
    }

    /// Write `server.json` when something in it changed (`snapshot_too`: or every half minute,
    /// for the Mac's last snapshot, which changes with every sync).
    fn persist(&mut self, now: u64, snapshot_too: bool) {
        if !(self.kept_dirty || (snapshot_too && now.saturating_sub(self.kept_saved) >= 30)) {
            return;
        }
        let path = kept_file();
        let written = serde_json::to_vec(&self.kept).map_err(anyhow::Error::from).and_then(|bytes| write_private(&path, &bytes));
        self.kept_saved = now;
        match written {
            Ok(()) => {
                self.kept_dirty = false;
                self.save_failed = false;
            }
            Err(error) => {
                if !std::mem::replace(&mut self.save_failed, true) {
                    log(format!("manager: could not write {}: {error:#}", path.display()));
                }
            }
        }
    }

    /// Queue something only the Mac can do.
    fn queue(&mut self, action: Action, now: u64) {
        self.kept.queue.push(action, now);
        self.kept_dirty = true;
    }

    /// Drop what is too old to hand over. What it wanted to say while the Mac was away becomes
    /// a line of the conversation.
    fn tidy(&mut self, now: u64) {
        let before = self.kept.queue.len();
        let lines = self.kept.queue.tidy(now);
        if self.kept.queue.len() != before {
            self.kept_dirty = true;
        }
        for line in lines {
            self.add_to_chat(line);
        }
        self.ready.retain(|r| now.saturating_sub(r.at) <= ANSWER_KEEPS);
    }

    /// The manager stops running here: nothing waits any more.
    fn stand_down(&mut self) {
        self.asks.clear();
        self.telling.clear();
        self.told_at.clear();
        self.reviewers_missing = false;
        if !self.kept.queue.is_empty() {
            self.kept.queue.items.clear();
            self.kept_dirty = true;
        }
        self.harness.prev = None;
        self.last_round = 0;
    }

    /// `manager.sync`. Returns the reply and the questions that are new (to be answered).
    fn sync(&mut self, request: SyncRequest, now: u64) -> (SyncReply, Vec<Ask>) {
        let was_active = self.active();
        if let Some(settings) = &request.settings {
            // Times of day in them are the person's: read them on this machine's clock.
            let settings = sync::settings_here(settings, request.utc_offset, sync::utc_offset(now));
            if settings != self.kept.settings {
                self.kept.settings = settings;
                self.kept_dirty = true;
            }
        }
        if request.off_projects != self.kept.off_projects || request.never_tell != self.kept.never_tell {
            self.kept.off_projects = request.off_projects.clone();
            self.kept.never_tell = request.never_tell;
            self.kept_dirty = true;
        }
        if let Some(snapshot) = request.snapshot.clone() {
            self.kept.snapshot = Some(snapshot);
        }
        self.kept.mac_seen = now;
        if sync::merge_request(&mut self.harness.data, &request) {
            self.harness.dirty = true;
        }
        // What the Mac has is done with.
        let before = self.kept.queue.len();
        self.kept.queue.ack(&request.acked);
        if self.kept.queue.len() != before {
            self.kept_dirty = true;
        }
        self.ready.retain(|r| !request.answered.contains(&r.answer.id));

        // The Mac repeats a question until its answer arrived: take each once.
        let mut fresh = Vec::new();
        for ask in request.asks {
            let known = self.asks.iter().any(|a| a.id == ask.id && a.text == ask.text) || self.ready.iter().any(|r| r.answer.id == ask.id && r.text == ask.text);
            if known {
                continue;
            }
            if self.active() {
                self.asks.push(ask.clone());
                fresh.push(ask);
            } else {
                self.ready.push(Ready { at: now, answer: Answered { id: ask.id, say: why_not_here(&self.kept.settings), failed: true }, text: ask.text });
            }
        }
        self.tidy(now);

        let fingerprint = sync::fingerprint(&self.harness.data);
        let (action_ids, actions) = self.kept.queue.pending();
        let reply = SyncReply {
            // The Mac holds the same already: nothing to send.
            data: if request.fingerprint == fingerprint { Data::default() } else { self.harness.data.clone() },
            actions,
            action_ids,
            answers: self.ready.iter().map(|r| r.answer.clone()).collect(),
            heartbeat: if self.active() { self.last_round } else { 0 },
            now,
            fingerprint,
        };
        if was_active && !self.active() {
            self.stand_down();
        }
        (reply, fresh)
    }

    /// What the manager looks at in a round: the Mac's last snapshot and the sessions here.
    fn snapshot(&self, server: &ServerState, now: u64) -> Snapshot {
        let mut snap = self.kept.snapshot.clone().unwrap_or_default();
        snap.at = now;
        snap.mac_online = self.mac_online(now);
        if snap.me.is_none() {
            snap.me = self.me.clone();
        }
        let own = server_sessions(server, &self.kept.settings, &self.kept.off_projects, &self.reviewers);
        // The server's sessions as the Mac saw them are older than what is known here.
        snap.sessions.retain(|s| !s.on_server && !server.sessions.iter().any(|o| o.session.id == s.id));
        snap.sessions.extend(own);
        snap
    }

    /// The message with this id when it may leave now (looked at right before it goes), and
    /// the teammate it is for when Settings knows them.
    fn sendable(&self, id: &str) -> Option<(OutMessage, Option<Person>)> {
        if !self.active() {
            return None;
        }
        let settings = &self.kept.settings;
        if !messages::ready(&self.harness.data, settings).iter().any(|ready| ready == id) {
            return None;
        }
        let message = self.harness.data.outbox.iter().find(|m| m.id == id)?.clone();
        if !allowed(&message, settings) {
            return None;
        }
        let person = settings.person(&message.to).cloned();
        Some((message, person))
    }

    /// Carry out what the manager decided: what can be done at once is done, what takes time
    /// comes back, and what only the Mac can do is queued for it.
    fn carry_out(&mut self, actions: Vec<Action>, server: &ServerState, snap: &Snapshot, now: u64) -> Vec<Effect> {
        let settings = self.kept.settings.clone();
        let mut effects = Vec::new();
        for action in actions {
            match action {
                Action::Draft { to, body, why, about } => {
                    messages::draft(&mut self.harness.data, &settings, &to, &body, &why, about.as_deref(), now);
                    self.harness.dirty = true;
                }
                Action::Remember { text } => {
                    memory::remember(&mut self.harness.data, &text, "manager", now);
                    self.harness.dirty = true;
                }
                Action::Send { id } => {
                    if !self.sending.contains(&id) && self.sendable(&id).is_some() {
                        self.sending.insert(id.clone());
                        effects.push(Effect::Send { id });
                    }
                }
                Action::Notify { title, body, session } => {
                    // Phones hear it now (unless the daemon already told them that this session
                    // asks something); the Mac shows it when it is there.
                    let announced = session.as_deref().and_then(|id| server.sessions.iter().find(|s| s.session.id == id)).is_some_and(asks_already);
                    if !announced {
                        effects.push(Effect::Push { title: title.clone(), body: body.clone(), session: session.clone(), badge: self.harness.needs_you(snap) });
                    }
                    self.queue(Action::Notify { title, body, session }, now);
                }
                Action::TellSession { session, text } if server.sessions.iter().any(|s| s.session.id == session) => {
                    let line = tell_line(&settings.name, &text);
                    if self.kept.never_tell {
                        log(format!("manager: session {session} was not told: Settings › Automation on the Mac says agents may never send a message to another session"));
                    } else if !line.is_empty() && !self.telling.iter().any(|t| t.session == session && t.text == line) {
                        self.telling.push(Tell { session, text: line, since: now });
                    }
                }
                // A session on the Mac, the window, its voice: the Mac's.
                other => self.queue(other, now),
            }
        }
        effects.extend(self.tells_due(server, now));
        effects
    }

    /// The messages for sessions here that can be typed now, one per session and round, in the
    /// order they came. A session that is working, asking, signed out or not running keeps its
    /// messages for a later round (a day at most).
    fn tells_due(&mut self, server: &ServerState, now: u64) -> Vec<Effect> {
        let mut due = Vec::new();
        let mut keep = Vec::new();
        let mut held: HashSet<String> = HashSet::new();
        for tell in std::mem::take(&mut self.telling) {
            // The session is gone, is a shell, or the message is a day old: let it go.
            let Some(session) = server.sessions.iter().find(|s| s.session.id == tell.session) else { continue };
            if session.session.agent == AgentKind::Shell || now.saturating_sub(tell.since) > TELL_KEEPS {
                continue;
            }
            let just_told = self.told_at.get(&tell.session).is_some_and(|at| now.saturating_sub(*at) < TELL_GAP);
            if !held.insert(tell.session.clone()) || just_told || !can_take(session, now) {
                keep.push(tell);
                continue;
            }
            self.told_at.insert(tell.session.clone(), now);
            due.push(Effect::Tell { session: tell.session, text: tell.text });
        }
        if keep.len() > 100 {
            let extra = keep.len() - 100;
            keep.drain(..extra);
        }
        self.telling = keep;
        self.told_at.retain(|id, _| server.sessions.iter().any(|s| &s.session.id == id));
        due
    }

    /// Add a line to the conversation (the last few hundred are kept, as on the Mac).
    fn add_to_chat(&mut self, line: ChatLine) {
        let chat = &mut self.harness.data.chat;
        chat.push(line);
        if chat.len() > store::CHAT_KEEP {
            let extra = chat.len() - store::CHAT_KEEP;
            chat.drain(..extra);
        }
        self.harness.dirty = true;
    }

    fn say_in_chat(&mut self, who: Who, text: &str, now: u64) {
        self.add_to_chat(ChatLine { who, text: text.to_string(), at: now, show: None });
    }

    /// After anything changed: tidy, and write what needs writing.
    fn settle(&mut self, now: u64) {
        self.tidy(now);
        self.harness.save();
        self.persist(now, false);
    }
}

// ----- doing what takes time --------------------------------------------------------------------

/// Do these one after the other, off the caller's thread (two messages typed into one
/// session at once would mix).
fn run(shared: &Shared, effects: Vec<Effect>) {
    if effects.is_empty() {
        return;
    }
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let shared = shared.clone();
    std::thread::spawn(move || {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        for effect in effects {
            match effect {
                Effect::Send { id } => send_message(&shared, &id),
                Effect::Tell { session, text } => {
                    // The same door phones and the Mac use to type into a session here.
                    let manager = glasscode_core::caller::Caller::Manager { lane: glasscode_core::caller::Lane::Wake, wake: None, project: None };
                    match crate::daemon::dispatch(&shared, &manager, "sessions.send", json!({ "session_id": session, "text": text, "enter": true })) {
                        Ok(_) => log(format!("manager: told session {session}: {}", text.chars().take(80).collect::<String>())),
                        Err(error) => log(format!("manager: could not type into session {session}: {error:#}")),
                    }
                }
                Effect::Push { title, body, session, badge } => {
                    crate::push::send(crate::push::Note { title, body, session_id: session.unwrap_or_default(), category: "question", badge });
                }
            }
        }
    });
}

/// Hand an outbox message to the messenger, after checking once more that it may go, and
/// note how it went (in the outbox and in the conversation).
fn send_message(shared: &Shared, id: &str) {
    let env = lock(shared).env.clone();
    let (checked, settings) = {
        let state = state();
        (state.sendable(id), state.kept.settings.clone())
    };
    let result = checked.map(|(message, person)| {
        let sent = deliver(&settings, &message, person.as_ref(), &env);
        (message.to, sent)
    });
    let at = now();
    let mut server = state();
    server.sending.remove(id);
    match result {
        Some((to, Ok(()))) => {
            messages::mark_sent(&mut server.harness.data, id, at);
            server.say_in_chat(Who::System, &format!("Sent the message to {to}."), at);
        }
        Some((to, Err(why))) => {
            log(format!("manager: message {id} was not sent: {why}"));
            messages::mark_failed(&mut server.harness.data, id, &why, at);
            server.say_in_chat(Who::System, &format!("The message to {to} was not sent. {}", sentence(&why)), at);
        }
        None => {}
    }
    server.harness.save();
}

// ----- rounds -----------------------------------------------------------------------------------

/// One round: what changed, and what to do about it.
fn round(shared: &Shared) {
    let at = now();
    let server = lock(shared).snapshot();
    let effects = {
        let mut state = state();
        if !state.active() {
            return;
        }
        let settings = state.kept.settings.clone();
        let snap = state.snapshot(&server, at);
        state.reviewers_missing = server.github.is_none() && snap.sessions.iter().any(|s| s.on_server && s.pr.is_none() && server.sessions.iter().any(|r| r.session.id == s.id && r.pr.is_some() && server.repos.contains_key(&r.session.project_id)));
        let actions = state.harness.step(&settings, &snap, at);
        state.last_round = at;
        state.harness.data.heartbeat = at;
        let effects = state.carry_out(actions, &server, &snap, at);
        state.settle(at);
        effects
    };
    run(shared, effects);
}

/// Hand what a background check found to the rules, and carry out what they say.
fn decide(shared: &Shared, found: impl FnOnce(&mut Harness, &Settings, u64) -> Vec<Action>) {
    let at = now();
    let server = lock(shared).snapshot();
    let effects = {
        let mut state = state();
        if !state.active() {
            return;
        }
        let settings = state.kept.settings.clone();
        let snap = state.snapshot(&server, at);
        let actions = found(&mut state.harness, &settings, at);
        let effects = state.carry_out(actions, &server, &snap, at);
        state.settle(at);
        effects
    };
    run(shared, effects);
}

/// The reviews the person left on other people's pull requests, fresh from GitHub.
fn check_reviews(shared: &Shared) {
    let (env, github) = {
        let daemon = lock(shared);
        (daemon.env.clone(), daemon.snapshot().github)
    };
    if github.is_some() {
        // gh is missing or signed out here; `manager.test` says so.
        return;
    }
    let (me, known) = {
        let state = state();
        let me = state.me.clone().or_else(|| state.kept.snapshot.as_ref().and_then(|s| s.me.clone()));
        let known: Vec<(String, u64)> = state.harness.data.followups.iter().filter(|f| f.is_open() && f.kind == FollowKind::BlockersGiven && f.number > 0).map(|f| (f.repo.clone(), f.number)).collect();
        (me, known)
    };
    let Some(me) = me.or_else(|| gh::my_login(&env).ok()).filter(|me| !me.is_empty()) else { return };
    state().me = Some(me.clone());
    match gh::given_reviews(&me, &known, &env) {
        Ok(reviews) => decide(shared, |harness, settings, at| harness.on_reviews(settings, &reviews, at)),
        Err(error) => log(format!("manager: could not read reviews from GitHub: {error:#}")),
    }
}

/// New messages from teammates. With no messenger set (or one that does not answer) the
/// inbox is never read.
fn check_inbox(shared: &Shared) {
    let env = lock(shared).env.clone();
    let settings = state().kept.settings.clone();
    if !messenger::available(&settings, &env) {
        return;
    }
    let started = now();
    let since = {
        let state = state();
        let newest = state.harness.data.inbox.iter().map(|m| m.at).max().unwrap_or(0);
        // The first time: the last day, or from the newest message already known.
        if state.inbox_since > 0 { state.inbox_since } else { newest.max(started.saturating_sub(86_400)) }
    };
    match messenger::incoming(&settings, since, &env) {
        Ok(messages) => {
            // A little overlap: the rules know a message they have seen.
            state().inbox_since = started.saturating_sub(60);
            if !messages.is_empty() {
                decide(shared, |harness, settings, at| harness.on_inbox(settings, messages, at));
            }
        }
        Err(error) => log(format!("manager: could not read new messages: {error:#}")),
    }
}

/// Who reviewed the pull requests of the sessions here (the daemon's own list only knows
/// whether a pull request is approved, not by whom).
fn refresh_reviewers(shared: &Shared) {
    let (env, server) = {
        let daemon = lock(shared);
        (daemon.env.clone(), daemon.snapshot())
    };
    if server.github.is_some() {
        return;
    }
    let (settings, off) = {
        let state = state();
        (state.kept.settings.clone(), state.kept.off_projects.clone())
    };
    let mut wanted: Vec<(String, u64, PathBuf)> = Vec::new();
    for session in server.sessions.iter().filter(|s| s.session.review.is_none() && s.session.agent != AgentKind::Shell) {
        let project = server.projects.iter().find(|p| p.id == session.session.project_id);
        let (Some(pr), Some(repo)) = (&session.pr, server.repos.get(&session.session.project_id)) else { continue };
        if managed(project, &settings, &off) && !wanted.iter().any(|(r, n, _)| r == repo && *n == pr.number) {
            wanted.push((repo.clone(), pr.number, session.session.worktree.clone()));
        }
    }
    let mut found = HashMap::new();
    for (repo, number, worktree) in wanted {
        let key = (repo.clone(), number);
        match github::pr_details(&repo, number, &github::PreviewConfig::load(&worktree), &env) {
            Ok(details) => {
                let reviewers = details
                    .reviewers
                    .iter()
                    .filter(|r| !r.is_bot)
                    .map(|r| ReviewerInfo { login: r.login.clone(), state: if r.state == "REQUESTED" { "PENDING".into() } else { r.state.clone() }, association: r.association.clone() })
                    .collect();
                found.insert(key, reviewers);
            }
            // Offline or rate limited: keep what was known.
            Err(_) => {
                if let Some(old) = state().reviewers.get(&key).cloned() {
                    found.insert(key, old);
                }
            }
        }
    }
    state().reviewers = found;
}

/// Clears a "this is running" flag when the work ends, however it ends.
struct Running(&'static AtomicBool);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Run a check on its own thread, unless the last one of its kind is still going.
fn background(flag: &'static AtomicBool, shared: &Shared, work: fn(&Shared)) {
    if flag.swap(true, Ordering::SeqCst) {
        return;
    }
    let shared = shared.clone();
    std::thread::spawn(move || {
        let _running = Running(flag);
        work(&shared);
    });
}

// ----- questions --------------------------------------------------------------------------------

/// A question for the manager. `reply`: someone here waits for the answer (`glasscoded manager
/// ask`); none: it came from the Mac and the answer goes back with its next sync.
struct Job {
    ask: Ask,
    reply: Option<mpsc::Sender<Result<String, String>>>,
}

static JOBS: OnceLock<mpsc::Sender<Job>> = OnceLock::new();

fn brain_key(settings: &Settings) -> String {
    format!("{:?}|{:?}|{:?}|{}|{}", settings.brain, settings.account, settings.model, settings.name, settings.owner)
}

/// The account the brain runs as here: the one the settings name when the server has it, else
/// the server's default for that agent, else any.
pub(crate) fn brain_account<'a>(settings: &Settings, accounts: &'a [Account], defaults: &Defaults) -> Option<&'a Account> {
    let of_kind = |a: &&Account| a.agent == settings.brain;
    settings
        .account
        .as_deref()
        .and_then(|id| accounts.iter().filter(of_kind).find(|a| a.id == id))
        .or_else(|| defaults.for_agent(settings.brain).and_then(|id| accounts.iter().filter(of_kind).find(|a| &a.id == id)))
        .or_else(|| accounts.iter().find(of_kind))
}

fn brain_config(shared: &Shared, settings: &Settings) -> BrainConfig {
    let (accounts, defaults) = lock(shared).accounts();
    let mut env = lock(shared).env.clone();
    // The `glasscode` command it acts through reaches the Mac from here.
    env.insert("GLASSCODE_CONTROL_SOCKET".into(), glasscode_core::control::socket_path().to_string_lossy().into_owned());
    let cwd = store::dir().join("work");
    let _ = std::fs::create_dir_all(&cwd);
    BrainConfig { config_dir: brain_account(settings, &accounts, &defaults).and_then(Account::config_dir), env, cwd }
}

/// Answer one question: the rules when they can, else the brain (which may take a while).
/// `local`: asked here, so the question and the answer are added to the conversation (the Mac
/// adds its own).
fn answer(shared: &Shared, brain: &mut Option<(String, Brain)>, ask: &Ask, local: bool) -> Result<String, String> {
    let at = now();
    let server = lock(shared).snapshot();
    let (settings, context) = {
        let mut state = state();
        if !state.active() {
            return Err(why_not_here(&state.kept.settings));
        }
        let settings = state.kept.settings.clone();
        let snap = state.snapshot(&server, at);
        if local {
            state.say_in_chat(Who::You, &ask.text, at);
        }
        if let Some(quick) = state.harness.quick_answer(&settings, &snap, &ask.text, at) {
            // It may have remembered or forgotten something.
            state.harness.dirty = true;
            let effects = state.carry_out(quick.actions, &server, &snap, at);
            if local {
                state.say_in_chat(Who::Manager, &quick.say, at);
            }
            state.settle(at);
            drop(state);
            run(shared, effects);
            return Ok(quick.say);
        }
        state.settle(at);
        let context = brain::context(&settings, &state.harness.data, &snap, &ask.text);
        (settings, context)
    };
    let key = brain_key(&settings);
    if brain.as_ref().is_none_or(|(running, _)| *running != key) {
        if let Some((_, mut old)) = brain.take() {
            old.stop();
        }
        *brain = Some((key, Brain::new(&settings, brain_config(shared, &settings))));
    }
    let Some((_, thinker)) = brain.as_mut() else { return Err("The manager's agent could not be started.".into()) };
    let said = thinker.ask(&ask.text, &context, ask.voice).map_err(|error| format!("{error:#}"));
    if let (true, Ok(say)) = (local, &said) {
        let mut state = state();
        state.say_in_chat(Who::Manager, say, now());
        state.harness.save();
    }
    said
}

/// Answers questions one at a time, in the order they came (it is one conversation), and
/// stops the agent when the manager no longer runs here.
fn ask_loop(shared: Shared, jobs: mpsc::Receiver<Job>) {
    let mut brain: Option<(String, Brain)> = None;
    loop {
        match jobs.recv_timeout(Duration::from_secs(5)) {
            Ok(job) => {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| answer(&shared, &mut brain, &job.ask, job.reply.is_some())))
                    .unwrap_or_else(|_| Err("The manager ran into an error while answering.".into()));
                match job.reply {
                    Some(reply) => {
                        let _ = reply.send(result);
                    }
                    None => {
                        let mut state = state();
                        state.asks.retain(|a| a.id != job.ask.id);
                        let (say, failed) = match result {
                            Ok(say) => (say, false),
                            Err(why) => (why, true),
                        };
                        state.ready.push(Ready { text: job.ask.text, at: now(), answer: Answered { id: job.ask.id, say, failed } });
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if brain.is_some() && !state().active() {
                    if let Some((_, mut idle)) = brain.take() {
                        idle.stop();
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

// ----- the daemon's side ------------------------------------------------------------------------

/// Start the manager's loops. They rest (no round, no process, no network) until the Mac's
/// settings say the manager is on and the server is its home.
pub fn start(shared: Shared) {
    let (jobs, inbox) = mpsc::channel();
    if JOBS.set(jobs).is_err() {
        return;
    }
    let asker = shared.clone();
    std::thread::spawn(move || ask_loop(asker, inbox));
    std::thread::spawn(move || {
        static REVIEWS: AtomicBool = AtomicBool::new(false);
        static INBOX: AtomicBool = AtomicBool::new(false);
        static REVIEWERS: AtomicBool = AtomicBool::new(false);
        let (mut round_at, mut reviews_at, mut inbox_at, mut reviewers_at): (Option<Instant>, Option<Instant>, Option<Instant>, Option<Instant>) = (None, None, None, None);
        let due = |last: &Option<Instant>, every: Duration| last.is_none_or(|at| at.elapsed() >= every);
        let mut was_active = false;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let active = state().active();
            if active != was_active {
                was_active = active;
                log(if active { "manager: running here" } else { "manager: not running here any more" });
            }
            if !active {
                (round_at, reviews_at, inbox_at, reviewers_at) = (None, None, None, None);
                continue;
            }
            if due(&reviewers_at, REVIEWERS_EVERY) || (due(&reviewers_at, REVIEWERS_SOON) && state().reviewers_missing) {
                reviewers_at = Some(Instant::now());
                background(&REVIEWERS, &shared, refresh_reviewers);
            }
            if due(&round_at, ROUND_EVERY) {
                round_at = Some(Instant::now());
                // A fault in a round must not end the rounds.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| round(&shared))).is_err() {
                    log("manager: a round failed");
                }
            }
            if due(&reviews_at, REVIEWS_EVERY) {
                reviews_at = Some(Instant::now());
                background(&REVIEWS, &shared, check_reviews);
            }
            if due(&inbox_at, INBOX_EVERY) {
                inbox_at = Some(Instant::now());
                background(&INBOX, &shared, check_inbox);
            }
        }
    });
}

/// `manager.*` requests. None: not ours.
pub fn dispatch(shared: &Shared, method: &str, params: &Value) -> Option<anyhow::Result<Value>> {
    match method {
        "manager.sync" => Some(sync_from_mac(params)),
        "manager.test" => Some(test(shared, params)),
        "manager.ask" => Some(ask(params)),
        "manager.status" => Some(Ok(status())),
        _ => None,
    }
}

fn sync_from_mac(params: &Value) -> anyhow::Result<Value> {
    let request: SyncRequest = serde_json::from_value(params.clone())?;
    let at = now();
    let (reply, fresh) = {
        let mut state = state();
        let out = state.sync(request, at);
        state.harness.save();
        state.persist(at, true);
        out
    };
    for ask in fresh {
        let sent = JOBS.get().is_some_and(|jobs| jobs.send(Job { ask: ask.clone(), reply: None }).is_ok());
        if !sent {
            let mut state = state();
            state.asks.retain(|a| a.id != ask.id);
            state.ready.push(Ready { text: ask.text, at, answer: Answered { id: ask.id, say: "The manager on the server cannot take questions right now.".into(), failed: true } });
        }
    }
    Ok(serde_json::to_value(reply)?)
}

/// `manager.ask` (`glasscoded manager ask`, the voice worker): waits for the answer.
fn ask(params: &Value) -> anyhow::Result<Value> {
    let text = params.get("text").and_then(Value::as_str).unwrap_or_default().trim().to_string();
    let voice = params.get("voice").and_then(Value::as_bool).unwrap_or(false);
    if text.is_empty() {
        bail!("nothing was asked");
    }
    {
        let state = state();
        if !state.active() {
            bail!("{}", why_not_here(&state.kept.settings));
        }
    }
    let (reply, answer) = mpsc::channel();
    let jobs = JOBS.get().ok_or_else(|| anyhow!("the manager is not started in this glasscoded"))?;
    jobs.send(Job { ask: Ask { id: String::new(), text, voice }, reply: Some(reply) }).map_err(|_| anyhow!("the manager cannot take questions right now"))?;
    match answer.recv_timeout(ASK_WITHIN) {
        Ok(Ok(say)) => Ok(json!({ "say": say })),
        Ok(Err(why)) => bail!("{why}"),
        Err(_) => bail!("the manager did not answer in time"),
    }
}

/// `manager.status`: what `glasscoded manager status` prints.
fn status() -> Value {
    let at = now();
    let state = state();
    let settings = &state.kept.settings;
    let data = &state.harness.data;
    json!({
        "enabled": settings.enabled,
        "name": settings.name,
        "home": settings.home,
        "running": state.active(),
        "round_ago": (state.active() && state.last_round > 0).then(|| at.saturating_sub(state.last_round)),
        "mac_online": state.mac_online(at),
        "mac_ago": (state.kept.mac_seen > 0).then(|| at.saturating_sub(state.kept.mac_seen)),
        "queued": state.kept.queue.len(),
        "asking": state.asks.len(),
        "followups": data.followups.iter().filter(|f| f.is_open()).count(),
        "waiting": messages::waiting(data).len(),
        "notes": data.notes.iter().filter(|n| !n.deleted).count(),
        "sessions": state.kept.snapshot.as_ref().map(|s| s.sessions.len()).unwrap_or(0),
    })
}

// ----- the test in Settings › Manager -----------------------------------------------------------

pub(crate) fn line(what: &str, ok: bool, detail: impl Into<String>) -> TestLine {
    TestLine { what: what.into(), ok, detail: detail.into() }
}

fn ago(secs: u64) -> String {
    match secs {
        0..=119 => format!("{secs} s"),
        120..=7199 => format!("{} min", secs / 60),
        7200..=172_799 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86_400),
    }
}

fn folder_line() -> TestLine {
    let dir = store::dir();
    let probe = dir.join(".write-test");
    let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&probe, b"ok")).and_then(|()| std::fs::remove_file(&probe));
    match written {
        Ok(()) => line("Its memory's folder", true, format!("{} can be written", dir.display())),
        Err(error) => line("Its memory's folder", false, format!("{} cannot be written: {error}", dir.display())),
    }
}

fn github_line(env: &HashMap<String, String>) -> TestLine {
    match github::gh_ready(env) {
        Ok(()) => match gh::my_login(env) {
            Ok(login) if !login.is_empty() => line("GitHub", true, format!("gh is signed in as {login}")),
            _ => line("GitHub", true, "gh is signed in"),
        },
        Err(why) => line("GitHub", false, why),
    }
}

fn brain_line(settings: &Settings, server: &ServerState, env: &HashMap<String, String>) -> TestLine {
    let what = "Its agent";
    let program = match settings.brain {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
        AgentKind::Shell => return line(what, false, "Choose Claude or Codex as the agent that thinks for it."),
    };
    let name = settings.brain.name();
    let Some(version) = agents::installed_version(program, env) else {
        return line(what, false, format!("{program} is not installed on the server (or not on the login shell's PATH)."));
    };
    match brain_account(settings, &server.accounts, &server.defaults) {
        None => line(what, false, format!("{name} {version} is installed, but the server has no {name} account. Add one in the Server dialog.")),
        Some(account) if !server.signed_in.get(&account.id).copied().unwrap_or(false) => {
            line(what, false, format!("{name} {version} is installed, but \u{201c}{}\u{201d} is not signed in on the server. Sign in from the Server dialog.", account.label))
        }
        Some(account) => line(what, true, format!("{name} {version}, signed in as \u{201c}{}\u{201d}", account.label)),
    }
}

/// Whether messages can leave from here. No messenger at all is not a fault: the manager
/// then only drafts.
fn messenger_line(settings: &Settings, env: &HashMap<String, String>) -> TestLine {
    let what = "Messages";
    let program = settings.messenger.trim();
    if program.is_empty() {
        return line(what, true, "No messenger is set up, so it drafts messages and sends none.");
    }
    match messenger::me(settings, env) {
        Ok(me) => {
            let who = [me.name, me.email, me.id].into_iter().find(|w| !w.is_empty());
            line(what, true, match who {
                Some(who) => format!("\u{201c}{program}\u{201d} answers, signed in as {who}"),
                None => format!("\u{201c}{program}\u{201d} answers"),
            })
        }
        Err(error) => match error.downcast_ref::<MessengerError>() {
            Some(MessengerError::NotFound(_)) => line(what, false, format!("\u{201c}{program}\u{201d} is not on the server (or not on the login shell's PATH). Messages it may send fail here until it is.")),
            _ => line(what, false, format!("\u{201c}{program}\u{201d} is here, but: {}", sentence(&format!("{error:#}")))),
        },
    }
}

fn memory_line(settings: &Settings, mac_seen: u64, now: u64) -> TestLine {
    let what = "Memory in sync";
    if mac_seen == 0 {
        // Not a fault before the server is its home: there is nothing to sync yet.
        return line(what, settings.home != Home::Server || !settings.enabled, "Nothing synced yet. It starts when the server is its home.");
    }
    let age = now.saturating_sub(mac_seen);
    line(what, age <= MAC_FRESH || settings.home != Home::Server || !settings.enabled, format!("Last synced with the Mac {} ago", ago(age)))
}

fn sessions_line(server: &ServerState) -> TestLine {
    let what = "Sessions here";
    if crate::tmux::Tmux::version().is_none() {
        return line(what, false, "tmux is not installed on the server.");
    }
    let signed_in = server.accounts.iter().filter(|a| a.agent.uses_accounts() && server.signed_in.get(&a.id).copied().unwrap_or(false)).count();
    match (signed_in, server.accounts.iter().filter(|a| a.agent.uses_accounts()).count()) {
        (0, 0) => line(what, false, "The server has no Claude or Codex account yet. Add one in the Server dialog."),
        (0, _) => line(what, false, "No account is signed in on the server. Sign in from the Server dialog."),
        (n, _) => line(what, true, format!("It can start sessions here ({n} account{} signed in, {} project{})", if n == 1 { "" } else { "s" }, server.projects.len(), if server.projects.len() == 1 { "" } else { "s" })),
    }
}

/// `manager.test`: is this server ready to be the manager's home? `settings` in the request
/// are the Mac's (so the test works before anything was synced).
fn test(shared: &Shared, params: &Value) -> anyhow::Result<Value> {
    let asked: Option<Settings> = params.get("settings").cloned().and_then(|v| serde_json::from_value(v).ok());
    let (settings, mac_seen) = {
        let state = state();
        (asked.unwrap_or_else(|| state.kept.settings.clone()), state.kept.mac_seen)
    };
    let (env, server) = {
        let daemon = lock(shared);
        (daemon.env.clone(), daemon.snapshot())
    };
    // The slow ones (they start programs) side by side, so the Mac is not kept waiting.
    let lines = std::thread::scope(|scope| {
        let github = scope.spawn(|| github_line(&env));
        let brain = scope.spawn(|| brain_line(&settings, &server, &env));
        let messages = scope.spawn(|| messenger_line(&settings, &env));
        let done = |handle: std::thread::ScopedJoinHandle<'_, TestLine>, what: &str| handle.join().unwrap_or_else(|_| line(what, false, "This check failed."));
        vec![folder_line(), done(github, "GitHub"), done(brain, "Its agent"), done(messages, "Messages"), memory_line(&settings, mac_seen, now()), sessions_line(&server)]
    });
    // The v2 manager here (M57): what decides whether its wake lane runs on this server.
    let mut lines = lines;
    lines.extend(crate::brain::test_lines(shared));
    Ok(serde_json::to_value(lines)?)
}

// ----- `glasscoded manager …` -------------------------------------------------------------------

/// `glasscoded manager ask [--voice] "<text>"` and `glasscoded manager status`, through the
/// daemon's socket.
pub fn command(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("ask") => {
            let voice = args[1..].iter().any(|a| a == "--voice");
            let text = args[1..].iter().filter(|a| *a != "--voice").cloned().collect::<Vec<_>>().join(" ");
            if text.trim().is_empty() {
                bail!("usage: glasscoded manager ask [--voice] \"<question>\"");
            }
            let answer = crate::request("manager.ask", json!({ "text": text, "voice": voice }))?;
            println!("{}", answer.get("say").and_then(Value::as_str).unwrap_or_default());
            Ok(())
        }
        Some("status") => {
            let s = crate::request("manager.status", Value::Null)?;
            let number = |key: &str| s.get(key).and_then(Value::as_u64);
            let name = s.get("name").and_then(Value::as_str).unwrap_or("Manager");
            if s.get("enabled").and_then(Value::as_bool) != Some(true) {
                println!("Manager: off (switch it on in Glasscode on the Mac, Settings › Manager)");
            } else if s.get("running").and_then(Value::as_bool) != Some(true) {
                println!("{name}: runs on the Mac (its home is not this server)");
            } else {
                match number("round_ago") {
                    Some(age) => println!("{name}: runs here, last round {} ago", ago(age)),
                    None => println!("{name}: runs here, first round not done yet"),
                }
            }
            match (s.get("mac_online").and_then(Value::as_bool), number("mac_ago")) {
                (Some(true), Some(age)) => println!("Mac: connected (synced {} ago)", ago(age)),
                (_, Some(age)) => println!("Mac: away (last synced {} ago)", ago(age)),
                _ => println!("Mac: never synced the manager here"),
            }
            println!("Waiting for the Mac: {} · Questions being answered: {}", number("queued").unwrap_or(0), number("asking").unwrap_or(0));
            println!(
                "Follow-ups open: {} · Messages waiting for your OK: {} · Notes: {}",
                number("followups").unwrap_or(0),
                number("waiting").unwrap_or(0),
                number("notes").unwrap_or(0)
            );
            Ok(())
        }
        _ => bail!("usage: glasscoded manager ask [--voice] \"<question>\" | glasscoded manager status"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glasscode_core::manager::Show;
    use glasscode_core::model::Session;

    fn settings(home: Home) -> Settings {
        Settings { enabled: true, home, name: "AI Sam".into(), owner: "Sam".into(), ..Settings::default() }
    }

    fn remote(id: &str, project: &str, agent: AgentKind) -> RemoteSession {
        RemoteSession {
            session: Session {
                id: id.into(),
                project_id: project.into(),
                worktree: PathBuf::from(format!("/srv/{id}")),
                branch: format!("feat/{id}"),
                title: format!("Session {id}"),
                agent,
                account_id: None,
                agent_session_id: None,
                transcript_path: None,
                lineage: Vec::new(),
                initial_prompt: None,
                team_task: None,
                review: None,
                model: None,
                mode: None,
                created_at: 1,
            },
            running: true,
            activity: Some(AgentActivity::Idle),
            tmux_name: id.into(),
            prompt: None,
            last_reply: None,
            updated_at: None,
            pr: None,
            host: None,
            synced_at: None,
            signed_out: false,
        }
    }

    fn project(id: &str, name: &str) -> Project {
        Project { id: id.into(), name: name.into(), root: PathBuf::from(format!("/srv/{}", name.to_lowercase())), expanded: true, github_account: None }
    }

    fn question() -> PendingPrompt {
        PendingPrompt { title: None, question: "Do you want to proceed?".into(), detail: vec!["rm -rf build".into()], options: Vec::new(), selected: 0 }
    }

    fn mac_session(id: &str, on_server: bool) -> SessionInfo {
        SessionInfo { id: id.into(), title: format!("Mac {id}"), on_server, ..SessionInfo::default() }
    }

    fn request(home: Home) -> SyncRequest {
        SyncRequest { settings: Some(settings(home)), snapshot: Some(Snapshot { sessions: vec![mac_session("m1", false)], mac_online: true, ..Snapshot::default() }), ..SyncRequest::default() }
    }

    #[test]
    fn what_waits_for_the_mac_stays_until_the_mac_has_it() {
        let mut server = Server::default();
        let (reply, _) = server.sync(request(Home::Server), 1_000);
        assert!(server.active());
        assert!(reply.actions.is_empty());
        assert_eq!(reply.now, 1_000);
        // No round yet: the Mac goes on by itself until there was one.
        assert_eq!(reply.heartbeat, 0);

        server.last_round = 1_003;
        server.queue(Action::TellSession { session: "m1".into(), text: "Mira approved your pull request.".into() }, 1_003);
        server.queue(Action::Say { text: "One thing needs you.".into(), show: None }, 1_003);
        let (first, _) = server.sync(request(Home::Server), 1_004);
        assert_eq!(first.heartbeat, 1_003);
        assert_eq!(first.actions.len(), 2);
        assert_eq!(first.action_ids.len(), 2);
        // That reply never arrived: the same two come again.
        let (again, _) = server.sync(request(Home::Server), 1_008);
        assert_eq!(again.action_ids, first.action_ids);
        // The Mac has them now.
        let (after, _) = server.sync(SyncRequest { acked: first.action_ids.clone(), ..request(Home::Server) }, 1_012);
        assert!(after.actions.is_empty() && after.action_ids.is_empty());
        assert!(server.kept_dirty);
    }

    #[test]
    fn a_question_is_taken_once_and_its_answer_stays_until_fetched() {
        let mut server = Server::default();
        let ask = Ask { id: "7".into(), text: "what is blocked?".into(), voice: false };
        let (_, fresh) = server.sync(SyncRequest { asks: vec![ask.clone()], ..request(Home::Server) }, 1_000);
        assert_eq!(fresh, vec![ask.clone()]);
        // The Mac repeats it until the answer is there: it is not asked twice.
        let (reply, fresh) = server.sync(SyncRequest { asks: vec![ask.clone()], ..request(Home::Server) }, 1_004);
        assert!(fresh.is_empty() && reply.answers.is_empty());

        // The worker answered.
        server.asks.clear();
        server.ready.push(Ready { text: ask.text.clone(), at: 1_006, answer: Answered { id: "7".into(), say: "Nothing is blocked.".into(), failed: false } });
        let (reply, fresh) = server.sync(SyncRequest { asks: vec![ask.clone()], ..request(Home::Server) }, 1_008);
        assert!(fresh.is_empty());
        assert_eq!(reply.answers, vec![Answered { id: "7".into(), say: "Nothing is blocked.".into(), failed: false }]);
        // Lost on the way: it comes again, until the Mac says it has it.
        let (reply, _) = server.sync(SyncRequest { asks: vec![ask.clone()], ..request(Home::Server) }, 1_012);
        assert_eq!(reply.answers.len(), 1);
        let (reply, _) = server.sync(SyncRequest { answered: vec!["7".into()], ..request(Home::Server) }, 1_016);
        assert!(reply.answers.is_empty());

        // The app restarted and counts from the start again: the same id, another question.
        let other = Ask { id: "7".into(), text: "status".into(), voice: true };
        let (_, fresh) = server.sync(SyncRequest { asks: vec![other.clone()], ..request(Home::Server) }, 1_020);
        assert_eq!(fresh, vec![other]);
    }

    #[test]
    fn nothing_runs_here_when_its_home_is_the_mac_or_it_is_off() {
        for request_settings in [settings(Home::Mac), Settings { enabled: false, ..settings(Home::Server) }] {
            let mut server = Server::default();
            server.last_round = 990;
            let ask = Ask { id: "1".into(), text: "status".into(), voice: false };
            let (reply, fresh) = server.sync(SyncRequest { settings: Some(request_settings), asks: vec![ask], ..SyncRequest::default() }, 1_000);
            assert!(!server.active());
            assert!(fresh.is_empty());
            assert_eq!(reply.heartbeat, 0);
            assert_eq!(reply.answers.len(), 1);
            assert!(reply.answers[0].failed);
        }
    }

    #[test]
    fn switching_its_home_back_to_the_mac_stops_it_here() {
        let mut server = Server::default();
        server.sync(request(Home::Server), 1_000);
        server.last_round = 1_001;
        server.queue(Action::Say { text: "Two things need you.".into(), show: None }, 1_001);
        server.telling.push(Tell { session: "s1".into(), text: "x".into(), since: 1_001 });
        let (reply, _) = server.sync(SyncRequest { settings: Some(settings(Home::Mac)), ..SyncRequest::default() }, 1_004);
        // The Mac gets what still waited, and hears that nothing runs here.
        assert_eq!(reply.actions.len(), 1);
        assert_eq!(reply.heartbeat, 0);
        assert!(!server.active());
        assert!(server.kept.queue.is_empty() && server.telling.is_empty());
        assert_eq!(server.last_round, 0);
    }

    #[test]
    fn data_only_travels_when_the_copies_differ() {
        let mut server = Server::default();
        server.harness.data.chat.push(ChatLine { who: Who::Manager, text: "Sam fixed what you asked for.".into(), at: 900, show: None });
        // The Mac does not say what it has: it gets everything.
        let (reply, _) = server.sync(request(Home::Server), 1_000);
        assert_eq!(reply.data.chat.len(), 1);
        assert_eq!(reply.fingerprint, sync::fingerprint(&server.harness.data));
        // The Mac holds the same: nothing is sent.
        let (reply, _) = server.sync(SyncRequest { fingerprint: reply.fingerprint, ..request(Home::Server) }, 1_004);
        assert_eq!(reply.data, Data::default());
        // Something new here: it travels again.
        server.harness.data.chat.push(ChatLine { who: Who::Manager, text: "Mira asked about the release.".into(), at: 1_005, show: None });
        let (later, _) = server.sync(SyncRequest { fingerprint: reply.fingerprint, ..request(Home::Server) }, 1_008);
        assert_eq!(later.data.chat.len(), 2);
    }

    #[test]
    fn what_it_wanted_to_say_while_the_mac_was_away_lands_in_the_conversation() {
        let mut server = Server::default();
        server.sync(request(Home::Server), 1_000);
        server.queue(Action::Say { text: "Alex pushed the fix.".into(), show: None }, 1_010);
        server.queue(Action::Show { show: Show { view: Some("today".into()), ..Show::default() } }, 1_010);
        // Hours later the Mac is back.
        let (reply, _) = server.sync(request(Home::Server), 9_000);
        assert!(reply.actions.is_empty());
        assert!(reply.data.chat.iter().any(|l| l.text == "Alex pushed the fix." && l.who == Who::Manager && l.at == 1_010));
        assert!(server.harness.dirty);
    }

    #[test]
    fn a_round_sees_the_macs_sessions_and_the_servers_own() {
        let mut server = Server::default();
        let mut sent = request(Home::Server);
        // The Mac also listed a server session, as it saw it a while ago.
        if let Some(snapshot) = sent.snapshot.as_mut() {
            snapshot.sessions.push(mac_session("s1", true));
            snapshot.me = Some("sam-example".into());
        }
        sent.off_projects = vec!["Playground".into()];
        server.sync(sent, 1_000);

        let mut asking = remote("s2", "p1", AgentKind::Claude);
        asking.prompt = Some(question());
        asking.activity = Some(AgentActivity::Working);
        let mut working = remote("s3", "p1", AgentKind::Codex);
        working.activity = Some(AgentActivity::Working);
        let mut stopped = remote("s4", "p1", AgentKind::Claude);
        stopped.running = false;
        stopped.activity = None;
        let mut wondering = remote("s5", "p1", AgentKind::Claude);
        wondering.last_reply = Some("Should I also update the docs?".into());
        let state = ServerState {
            projects: vec![project("p1", "Shop"), project("p2", "Playground")],
            sessions: vec![remote("s1", "p1", AgentKind::Claude), asking, working, stopped, wondering, remote("s6", "p1", AgentKind::Shell), remote("s7", "p2", AgentKind::Claude)],
            ..ServerState::default()
        };

        let snap = server.snapshot(&state, 1_010);
        assert!(snap.mac_online);
        assert_eq!(snap.at, 1_010);
        assert_eq!(snap.me.as_deref(), Some("sam-example"));
        let ids: Vec<&str> = snap.sessions.iter().map(|s| s.id.as_str()).collect();
        // The Mac's own, then the server's: each once, no shell, nothing from a project it
        // does not manage.
        assert_eq!(ids, vec!["m1", "s1", "s2", "s3", "s4", "s5"]);
        let of = |id: &str| snap.session(id).unwrap();
        assert!(!of("m1").on_server && of("s1").on_server);
        assert_eq!(of("s1").title, "Session s1");
        assert_eq!(of("s1").project, "Shop");
        assert_eq!(of("s1").state, SessionState::Idle);
        assert_eq!(of("s2").state, SessionState::NeedsYou);
        assert_eq!(of("s2").last_reply.as_deref(), Some("Do you want to proceed? (rm -rf build)"));
        assert_eq!(of("s3").state, SessionState::Working);
        assert_eq!(of("s4").state, SessionState::NotRunning);
        assert_eq!(of("s5").state, SessionState::Idle);
        assert_eq!(of("s5").last_reply.as_deref(), Some("Should I also update the docs?"));

        // The Mac has been quiet for too long: it counts as away.
        assert!(!server.snapshot(&state, 1_000 + MAC_FRESH + 1).mac_online);
    }

    #[test]
    fn a_pull_request_shows_once_its_reviewers_are_known() {
        let mut session = remote("s1", "p1", AgentKind::Claude);
        session.pr = Some(github::PrSummary { number: 12, title: "Faster search".into(), state: "OPEN".into(), draft: true, ci: "failed".into(), merge_state: "DIRTY".into(), unresolved: 2, ..Default::default() });
        let mut state = ServerState { projects: vec![project("p1", "Shop")], sessions: vec![session], ..ServerState::default() };
        state.repos.insert("p1".into(), "acme/shop".into());
        let mut reviewers = HashMap::new();
        assert!(pr_info(&state.sessions[0], &state, &reviewers).is_none());
        reviewers.insert(("acme/shop".to_string(), 12), vec![ReviewerInfo { login: "mira-example".into(), state: "CHANGES_REQUESTED".into(), ..ReviewerInfo::default() }]);
        let pr = pr_info(&state.sessions[0], &state, &reviewers).unwrap();
        assert_eq!((pr.repo.as_str(), pr.number, pr.state.as_str(), pr.ci.as_str()), ("acme/shop", 12, "draft", "failing"));
        assert!(pr.conflicting);
        assert_eq!(pr.unresolved_threads, 2);
        assert_eq!(pr.reviewers.len(), 1);
        // A review session's pull request is someone else's: it shows as it is, and
        // `review_of` says whose review it is.
        state.sessions[0].session.review = Some(glasscode_core::model::ReviewRef { repo: "acme/shop".into(), number: 12, ..Default::default() });
        assert!(pr_info(&state.sessions[0], &state, &HashMap::new()).is_some_and(|pr| pr.reviewers.is_empty()));
        let seen = server_sessions(&state, &settings(Home::Server), &[], &reviewers);
        assert_eq!(seen[0].review_of, Some(("acme/shop".to_string(), 12)));
    }

    #[test]
    fn a_message_only_leaves_when_the_person_allowed_it() {
        let on = settings(Home::Server);
        let message = |tier, state| OutMessage { id: "m".into(), to: "Mira".into(), body: "The fix is pushed.".into(), tier, state, created: 900, updated: 900, ..OutMessage::default() };
        // A draft waits for the person, whatever it says.
        assert!(!allowed(&message(Tier::Routine, MsgState::Draft), &on));
        assert!(!allowed(&message(Tier::NeedsOk, MsgState::Draft), &on));
        // Allowed.
        assert!(allowed(&message(Tier::NeedsOk, MsgState::Approved), &on));
        assert!(allowed(&message(Tier::Routine, MsgState::Approved), &on));
        // Never: approving, merging, speaking for the person.
        assert!(!allowed(&message(Tier::Never, MsgState::Approved), &on));
        // Not twice, not after a no, not by itself after it failed.
        for state in [MsgState::Sent, MsgState::Rejected, MsgState::Failed] {
            assert!(!allowed(&message(Tier::Routine, state), &on));
        }
        // Not when it is switched off.
        assert!(!allowed(&message(Tier::NeedsOk, MsgState::Approved), &Settings { enabled: false, ..on.clone() }));

        // In the outbox: a draft the person has not seen is not handed to the messenger.
        let mut server = Server::default();
        server.sync(request(Home::Server), 1_000);
        server.harness.data.outbox.push(message(Tier::NeedsOk, MsgState::Draft));
        assert!(server.sendable("m").is_none());
        let state = ServerState::default();
        let snap = server.snapshot(&state, 1_001);
        assert!(server.carry_out(vec![Action::Send { id: "m".into() }], &state, &snap, 1_001).is_empty());
        assert!(server.sending.is_empty());

        // The person allowed it: it goes, once, however often it is asked for meanwhile.
        server.harness.data.outbox[0].state = MsgState::Approved;
        let send = || vec![Action::Send { id: "m".into() }];
        assert_eq!(server.carry_out(send(), &state, &snap, 1_002), vec![Effect::Send { id: "m".into() }]);
        assert!(server.carry_out(send(), &state, &snap, 1_003).is_empty());
        // Its home moved to the Mac before it left: it stays.
        server.sync(SyncRequest { settings: Some(settings(Home::Mac)), ..SyncRequest::default() }, 1_004);
        assert!(server.sendable("m").is_none());
    }

    #[test]
    fn a_message_goes_to_the_contact_or_else_the_name() {
        let mira = Person { name: "Mira".into(), contact: Some(" mira@example.com ".into()), ..Person::default() };
        assert_eq!(address("Mira", Some(&mira)), "mira@example.com");
        assert_eq!(address(" Mira ", Some(&Person { name: "Mira".into(), contact: Some("  ".into()), ..Person::default() })), "Mira");
        assert_eq!(address("Mira", Some(&Person { name: "Mira".into(), ..Person::default() })), "Mira");
        assert_eq!(address("Someone New", None), "Someone New");
    }

    #[test]
    fn without_a_messenger_an_allowed_message_fails_with_a_plain_reason() {
        let message = OutMessage { id: "m".into(), to: "Mira".into(), body: "The fix is pushed.".into(), tier: Tier::Routine, state: MsgState::Approved, ..OutMessage::default() };
        let env = HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]);
        assert_eq!(deliver(&settings(Home::Server), &message, None, &env), Err("No messenger is set up. Choose one in Settings › Manager.".to_string()));
        // One that is set on the Mac but missing here says where it is missing.
        let missing = Settings { messenger: "no-such-messenger".into(), ..settings(Home::Server) };
        let why = deliver(&missing, &message, None, &env).unwrap_err();
        assert!(why.contains("\u{201c}no-such-messenger\u{201d} is not on the server"), "{why}");
        // The test in Settings says the same, and no messenger at all is not a fault.
        assert!(messenger_line(&settings(Home::Server), &env).ok);
        let line = messenger_line(&missing, &env);
        assert!(!line.ok && line.detail.contains("is not on the server"), "{}", line.detail);
    }

    #[test]
    fn an_allowed_message_is_handed_to_the_messenger_program() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // A stand-in messenger: says who the person is and keeps what it is asked to send.
        let script = "#!/bin/sh\ndir=$(dirname \"$0\")\ncase \"$1\" in\n  whoami) echo '{\"id\":\"u-sam\",\"name\":\"Sam Example\",\"email\":\"\"}' ;;\n  send) shift; printf '%s\\n' \"$*\" > \"$dir/args.txt\"; cat > \"$dir/sent.txt\" ;;\n  *) echo '[]' ;;\nesac\n";
        let program = dir.path().join("chat-bridge");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = HashMap::from([("PATH".to_string(), format!("{}:/usr/bin:/bin", dir.path().display()))]);
        let on = Settings { messenger: "chat-bridge".into(), ..settings(Home::Server) };
        let mira = Person { name: "Mira".into(), contact: Some("mira@example.com".into()), ..Person::default() };
        let message = OutMessage { id: "m".into(), to: "Mira".into(), body: "The fix for #12 is pushed.\nAI Sam, on behalf of Sam".into(), urgent: true, tier: Tier::Routine, state: MsgState::Approved, ..OutMessage::default() };
        assert_eq!(deliver(&on, &message, Some(&mira), &env), Ok(()));
        assert_eq!(std::fs::read_to_string(dir.path().join("args.txt")).unwrap().trim(), "--to mira@example.com --urgent");
        assert_eq!(std::fs::read_to_string(dir.path().join("sent.txt")).unwrap(), "The fix for #12 is pushed.\nAI Sam, on behalf of Sam");
        let line = messenger_line(&on, &env);
        assert!(line.ok && line.detail.contains("signed in as Sam Example"), "{}", line.detail);
    }

    #[test]
    fn it_types_into_sessions_here_and_leaves_the_macs_to_the_mac() {
        let mut server = Server::default();
        server.sync(request(Home::Server), 1_000);
        let mut asking = remote("s2", "p1", AgentKind::Claude);
        asking.prompt = Some(question());
        let mut working = remote("s4", "p1", AgentKind::Claude);
        working.activity = Some(AgentActivity::Working);
        let mut state = ServerState { projects: vec![project("p1", "Shop")], sessions: vec![remote("s1", "p1", AgentKind::Claude), asking, remote("s3", "p1", AgentKind::Shell), working], ..ServerState::default() };
        let snap = server.snapshot(&state, 1_001);
        let tell = |session: &str| Action::TellSession { session: session.into(), text: "Mira approved\nyour pull request.".into() };
        let typed = "[AI Sam] Mira approved your pull request. (Sent by Glasscode because AI Sam asked it to.)";
        let effects = server.carry_out(vec![tell("s1"), tell("s1"), tell("s2"), tell("s3"), tell("s4"), tell("m1"), Action::Show { show: Show::default() }], &state, &snap, 1_001);
        // Typed here, once, in one line that says who it is from: the free session only. The
        // one showing a question and the one at work wait; a shell is never typed into.
        assert_eq!(effects, vec![Effect::Tell { session: "s1".into(), text: typed.into() }]);
        let waiting: Vec<&str> = server.telling.iter().map(|t| t.session.as_str()).collect();
        assert_eq!(waiting, vec!["s2", "s4"]);
        // For the Mac: its own session (as the manager said it), and the window.
        let (_, queued) = server.kept.queue.pending();
        assert_eq!(queued, vec![tell("m1"), Action::Show { show: Show::default() }]);

        // The question was answered and the work is done: now they are typed.
        state.sessions[1].prompt = None;
        state.sessions[3].activity = Some(AgentActivity::Idle);
        let effects = server.tells_due(&state, 1_010);
        assert_eq!(effects.len(), 2);
        assert!(server.telling.is_empty());

        // Two for one session go one after the other, with a pause between them.
        let more = |text: &str| Action::TellSession { session: "s2".into(), text: text.into() };
        let effects = server.carry_out(vec![more("Alex asked for changes."), more("CI failed.")], &state, &snap, 1_011);
        assert!(effects.is_empty());
        assert_eq!(server.tells_due(&state, 1_010 + TELL_GAP).len(), 1);
        assert_eq!(server.telling.len(), 1);
        assert!(server.tells_due(&state, 1_011 + TELL_GAP).is_empty());
        assert_eq!(server.tells_due(&state, 1_010 + 2 * TELL_GAP).len(), 1);

        // A session that never becomes free keeps its message for a day, then it is let go.
        state.sessions[0].running = false;
        server.telling.push(Tell { session: "s1".into(), text: "x".into(), since: 2_000 });
        assert!(server.tells_due(&state, 2_100).is_empty());
        assert_eq!(server.telling.len(), 1);
        assert!(server.tells_due(&state, 2_000 + TELL_KEEPS + 1).is_empty());
        assert!(server.telling.is_empty());
        // One whose session was closed goes at once.
        server.telling.push(Tell { session: "gone".into(), text: "x".into(), since: 2_000 });
        assert!(server.tells_due(&state, 2_001).is_empty());
        assert!(server.telling.is_empty());

        // The person's Automation settings say never: nothing is typed.
        server.kept.never_tell = true;
        state.sessions[0].running = true;
        assert!(server.carry_out(vec![tell("s1")], &state, &snap, 90_000).is_empty());
        assert!(server.telling.is_empty());
    }

    #[test]
    fn a_notification_goes_to_phones_and_waits_for_the_mac() {
        let mut server = Server::default();
        server.sync(request(Home::Server), 1_000);
        let state = ServerState::default();
        let snap = server.snapshot(&state, 1_001);
        let effects = server.carry_out(vec![Action::Notify { title: "Review".into(), body: "Alex asked for changes.".into(), session: Some("m1".into()) }], &state, &snap, 1_001);
        assert!(matches!(effects.as_slice(), [Effect::Push { title, session: Some(session), .. }] if title == "Review" && session == "m1"));
        assert_eq!(server.kept.queue.len(), 1);

        // A session here that shows a question: the daemon told the phones already, so only
        // the Mac is left to tell.
        let mut asking = remote("s1", "p1", AgentKind::Claude);
        asking.prompt = Some(question());
        let state = ServerState { projects: vec![project("p1", "Shop")], sessions: vec![asking], ..ServerState::default() };
        let effects = server.carry_out(vec![Action::Notify { title: "Session s1 needs you".into(), body: "Do you want to proceed?".into(), session: Some("s1".into()) }], &state, &snap, 1_002);
        assert!(effects.is_empty());
        assert_eq!(server.kept.queue.len(), 2);
    }

    #[test]
    fn the_brain_runs_as_the_servers_account() {
        let account = |id: &str, agent| Account { id: id.into(), agent, label: id.into(), plan: None, system_default: false };
        let accounts = vec![account("codex-1", AgentKind::Codex), account("claude-1", AgentKind::Claude), account("claude-2", AgentKind::Claude)];
        let defaults = Defaults { claude: Some("claude-2".into()), codex: None };
        let pick = |settings: &Settings| brain_account(settings, &accounts, &defaults).map(|a| a.id.clone());
        // The one the settings name, when the server has it.
        assert_eq!(pick(&Settings { account: Some("claude-1".into()), ..settings(Home::Server) }).as_deref(), Some("claude-1"));
        // The Mac's account is not here: the server's default.
        assert_eq!(pick(&Settings { account: Some("mac-only".into()), ..settings(Home::Server) }).as_deref(), Some("claude-2"));
        // No default: any account of that agent.
        assert_eq!(pick(&Settings { brain: AgentKind::Codex, ..settings(Home::Server) }).as_deref(), Some("codex-1"));
        assert_eq!(brain_account(&settings(Home::Server), &[], &defaults).map(|a| a.id.clone()), None);
    }

    #[test]
    fn what_is_kept_on_disk_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("manager").join("server.json");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        write_private(&path, b"{\"mac_seen\":1}").unwrap();
        assert_eq!((mode(&path), mode(path.parent().unwrap())), (0o600, 0o700));
        // Written again over what is there, and nothing is left beside it.
        write_private(&path, b"{\"mac_seen\":2}").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"mac_seen\":2}");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        // It reads back as what the daemon keeps.
        let kept: Kept = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(kept.mac_seen, 2);
    }

    #[test]
    fn the_test_says_whether_memory_is_in_sync() {
        let home = settings(Home::Server);
        // Its home is the server and the Mac synced just now.
        assert!(memory_line(&home, 1_000, 1_010).ok);
        // Its home is the server and the Mac has been quiet.
        assert!(!memory_line(&home, 1_000, 2_000).ok);
        assert!(!memory_line(&home, 0, 2_000).ok);
        // Before the server is its home there is nothing to sync: not a fault.
        assert!(memory_line(&settings(Home::Mac), 0, 2_000).ok);
        assert_eq!(ago(45), "45 s");
        assert_eq!(ago(600), "10 min");
        assert_eq!(ago(3 * 3600 * 24), "3 d");
    }
}
