//! The manager's rounds in the app: build what it sees, let the rules decide, carry out what
//! they say; ask the brain; take the person through the app and point at things there.
//!
//! The thinking is `glasscode_core::manager`; this file is what needs the app: its sessions,
//! its terminals, its window. While the manager is off nothing here starts anything; the tick
//! then only takes finished jobs off the channel, so a message that went out is never sent
//! again and the server test in Settings still gets its answer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use gpui::{AnyElement, AppContext as _, Context, Div, FontWeight, IntoElement, ParentElement, Styled, Window, div, px};
use glasscode_core::agents::AgentActivity;
use glasscode_core::control::{self, Kind, LogEntry, Reply, Rule, code};
use glasscode_core::github::{self, CheckState, CiStatus, PrDetails, PrSummary, PreviewConfig, PullRequest};
use glasscode_core::manager::brain::{self, Brain, BrainConfig};
use glasscode_core::manager::harness::Answer;
use glasscode_core::manager::sync::{self, Ask};
use glasscode_core::manager::messenger::{self, MessengerError};
use glasscode_core::manager::{
    Action, ChatLine, FollowKind, MsgState, OutMessage, PanelTab, PrInfo, ReviewAsk, ReviewerInfo, Scene, SessionInfo, SessionState, Show, Snapshot, Tier, TourStop, Who, gh, memory, messages, scene, store,
};
use glasscode_core::model::{AgentKind, Session, now};
use glasscode_core::prompt::{self, PendingPrompt};
use glasscode_core::transcript::{self, ItemKind};

use crate::center::Center;
use crate::commands::NewSpec;
use crate::control_ui::Responder;
use crate::manager_state::{Done, Highlight, ScenePlay, Stage, Tour};
use crate::scenes_ui::{gold, gold_bg};
use crate::tickets_ui::View;
use crate::workspace::{RightTab, Workspace};

/// How often the rules look at everything.
const ROUND: Duration = Duration::from_secs(3);
/// How often the reviews the person gave are read from GitHub.
const REVIEWS_EVERY: Duration = Duration::from_secs(5 * 60);
/// How often the messenger is asked for new messages.
const INBOX_EVERY: Duration = Duration::from_secs(2 * 60);
/// How often the pull requests of managed sessions are read in full (reviewers, threads).
const PRS_EVERY: Duration = Duration::from_secs(3 * 60);
/// At most this many pull requests are read in one go.
const PRS_AT_ONCE: usize = 12;
/// The conversation keeps this many lines.
const CHAT_MAX: usize = 400;
/// How long it points at something when no walk says otherwise.
const POINT_FOR: Duration = Duration::from_secs(8);
/// The caption stays this long after it stopped speaking.
const CAPTION_STAYS: Duration = Duration::from_secs(6);
/// A question nobody answered in this time is given up.
const ASK_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// A message for a session that could not be typed for this long is dropped.
const TELL_KEEP: Duration = Duration::from_secs(24 * 3600);
/// At least this long between two messages typed into the same session.
const TELL_GAP: Duration = Duration::from_secs(20);
/// A walk waits this long at most for the voice to finish a sentence.
const VOICE_WAIT: Duration = Duration::from_secs(20);
/// With the server as home, it does the sending while its last round is at most this old.
const SERVER_SENDS_SECS: u64 = 10 * 60;
/// How much of a session's last reply the manager is shown.
const REPLY_CHARS: usize = 400;

/// Called with the manager's answer to a question, or why there is none.
pub type OnAnswer = Box<dyn FnOnce(&mut Workspace, Result<String, String>, &mut Context<Workspace>)>;

/// A message for a session that waits until it can be typed.
pub struct Tell {
    pub session: String,
    pub text: String,
    pub since: Instant,
}

/// Something a callback without a window leaves for the next tick.
pub enum Later {
    /// A line from the app in the conversation.
    Line(String),
    /// A session the manager started: say so, and show it when the person is on its page.
    Started { id: String, title: String },
}

/// The end of a session's last reply, and the transcript it was read from.
#[derive(Clone, Default)]
pub struct ReplyEnd {
    /// The transcript's size and time when it was read (None: not read yet).
    stamp: Option<(u64, Option<SystemTime>)>,
    text: Option<String>,
    /// The question on its screen while it needs the person.
    question: Option<String>,
}

/// What the rounds keep between ticks (`ManagerUi::tick`).
#[derive(Default)]
pub struct TickUi {
    /// Whoever waits for the answer to a question, by the question's id.
    pub waiting: HashMap<String, OnAnswer>,
    /// Questions out with the brain or the server's copy, and since when.
    pub open_asks: HashMap<String, Instant>,
    /// Messages for sessions that could not be typed yet (working, a draft in the box).
    pub tells: Vec<Tell>,
    /// When each session was last told something.
    pub told_at: HashMap<String, Instant>,
    /// Outbox messages on their way (being looked up or sent), so none goes twice.
    pub sending: HashSet<String>,
    /// The end of each session's last reply, by session id.
    pub replies: HashMap<String, ReplyEnd>,
    pub replies_running: bool,
    /// When the pull requests of managed sessions are read in full next.
    pub next_prs: Option<Instant>,
    pub prs_running: bool,
    /// Where the next read of pull requests starts (they take turns, a few at a time).
    pub prs_from: usize,
    pub later: Vec<Later>,
    /// What the brain was started with (agent, account, model).
    pub brain_key: Option<(AgentKind, Option<String>, Option<String>)>,
    /// Whether it was speaking and had a caption at the last tick (to redraw when that ends).
    pub shown: (bool, bool),
    /// A walk was started while it was still speaking: its first stop comes when that is over.
    pub tour_waits: bool,
}

// ----- the rounds -------------------------------------------------------------------------------

impl Workspace {
    /// Called from the workspace tick (about every 700 ms). Does nothing while the manager is
    /// off. Reads finished background jobs, does a round every few seconds, moves a walk on.
    pub fn manager_tick(&mut self, cx: &mut Context<Self>) {
        let on = self.state.manager.enabled;
        let mut changed = self.mtick_take_jobs(on, cx);
        if !on {
            if changed {
                cx.notify();
            }
            return;
        }
        let at = Instant::now();
        for later in std::mem::take(&mut self.manager.tick.later) {
            match later {
                Later::Line(text) => {
                    self.mtick_line(Who::System, text, None);
                    changed = true;
                }
                Later::Started { id, title } => {
                    let show = Show { session: Some(id), ..Default::default() };
                    self.manager_say(&format!("Started “{title}”."), Some(show.clone()), cx);
                    // Only where the person asked for it: never pull them out of a session.
                    if self.tickets.view == View::Manager {
                        self.with_main_window(cx, move |ws, window, cx| ws.manager_show(&show, window, cx));
                    }
                }
            }
        }

        // The server's copy stopped answering: the Mac takes its questions over.
        let on_server = self.manager_on_server();
        if !on_server && !self.manager.asks.is_empty() {
            let snap = self.manager_snapshot();
            for ask in std::mem::take(&mut self.manager.asks) {
                if let (true, Ok(id)) = (self.manager.tick.open_asks.contains_key(&ask.id), ask.id.parse::<u64>()) {
                    // The brain gets its full time, however long the server sat on the question.
                    self.manager.tick.open_asks.insert(ask.id.clone(), Instant::now());
                    self.mtick_ask_brain(id, ask.text, ask.voice, &snap, cx);
                }
            }
        }

        if self.manager.last_round.is_none_or(|t| t.elapsed() >= ROUND) {
            self.manager.last_round = Some(at);
            self.mtick_refresh_replies(cx);
            if !on_server {
                let snap = self.manager_snapshot();
                let actions = self.manager.harness.step(&self.state.manager, &snap, now());
                changed |= !actions.is_empty();
                self.manager_apply(actions, cx);
                // What it checks by itself goes to the brain when that is free and no walk is on.
                if self.manager.tick.open_asks.is_empty() && self.manager.tour.is_none() {
                    for check in self.manager.harness.due_checks(&self.state.manager, now()) {
                        self.mtick_run_check(&check.what, &snap, cx);
                        changed = true;
                    }
                }
            }
            self.mtick_flush_tells(cx);
            changed |= self.manager.harness.dirty;
            self.manager.harness.save();
        }
        if !on_server {
            if !self.manager.reviews_running && self.manager.last_reviews.is_none_or(|t| t.elapsed() >= REVIEWS_EVERY) {
                self.mtick_fetch_reviews(cx);
            }
            if !self.manager.inbox_running && self.manager.last_inbox.is_none_or(|t| t.elapsed() >= INBOX_EVERY) {
                self.mtick_fetch_inbox(cx);
            }
            if !self.manager.tick.prs_running && self.manager.tick.next_prs.is_none_or(|t| at >= t) {
                self.mtick_fetch_prs(cx);
            }
        }

        // Questions nobody answered.
        let late: Vec<String> = self.manager.tick.open_asks.iter().filter(|(_, since)| since.elapsed() > ASK_TIMEOUT).map(|(id, _)| id.clone()).collect();
        for id in late {
            self.manager.asks.retain(|a| a.id != id);
            self.manager_answered(&id, Err("No answer came back in ten minutes".into()), false, cx);
        }

        // A walk moves on when its sentence is over (and the voice has finished saying it).
        let voice_busy = self.manager.live && self.manager.voice.state == "speaking";
        let held = |next_at: Instant| at < next_at || (voice_busy && at < next_at + VOICE_WAIT);
        let waits = self.manager.tick.tour_waits;
        if let Some((next_at, next, stops)) = self.manager.tour.as_ref().map(|t| (t.next_at, if waits { t.index } else { t.index + 1 }, t.stops.len())) {
            if !held(next_at) {
                if next < stops {
                    self.manager_tour_go(next, cx);
                } else {
                    self.manager.tour = None;
                    changed = true;
                }
            }
        }
        if self.manager.highlight.as_ref().is_some_and(|h| at >= h.until) {
            self.manager.highlight = None;
            changed = true;
        }
        // A picture moves to its next step the same way.
        let step = self.manager.scene.as_mut().and_then(|play| {
            let next_at = play.next_at?;
            if held(next_at) || play.step + 1 >= play.scene.steps.len() {
                return None;
            }
            play.step += 1;
            Some((play.scene.steps[play.step].say.clone(), play.step + 1 < play.scene.steps.len()))
        });
        if let Some((say, more)) = step {
            let spoken = self.mtick_say(&say, None, cx);
            if let Some(play) = self.manager.scene.as_mut() {
                play.next_at = more.then(|| Instant::now() + stop_time(&say, spoken));
            }
            changed = true;
        }

        // It stopped speaking, or its caption has been up long enough: draw that.
        let shown = (self.manager.speaking_until.is_some_and(|t| at < t), self.manager.caption_until.is_some_and(|t| at < t));
        if shown != self.manager.tick.shown {
            self.manager.tick.shown = shown;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// Take what background jobs handed back. While the manager is off, only what must not be
    /// lost is kept: that a message went out, and the answers meant for the server's copy.
    fn mtick_take_jobs(&mut self, on: bool, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        while let Ok(done) = self.manager.jobs.1.try_recv() {
            match done {
                Done::Answer { id, say, voice } => {
                    let id = id.to_string();
                    // An answer to a question that was given up meanwhile is not said.
                    if on && self.manager.tick.open_asks.contains_key(&id) {
                        self.manager_answered(&id, say, voice, cx);
                    }
                }
                Done::Reviews(result) => {
                    let wanted = std::mem::take(&mut self.manager.reviews_running);
                    match result {
                        Ok(reviews) if on && wanted => {
                            let actions = self.manager.harness.on_reviews(&self.state.manager, &reviews, now());
                            self.manager_apply(actions, cx);
                            changed = true;
                        }
                        Ok(_) => {}
                        Err(why) => log::warn!("manager: could not read the reviews you gave: {why}"),
                    }
                }
                Done::Inbox(result) => {
                    let wanted = std::mem::take(&mut self.manager.inbox_running);
                    match result {
                        Ok(messages) if on && wanted && !messages.is_empty() => {
                            let actions = self.manager.harness.on_inbox(&self.state.manager, messages, now());
                            self.manager_apply(actions, cx);
                            changed = true;
                        }
                        Ok(_) => {}
                        Err(why) => log::warn!("manager: could not read new messages: {why}"),
                    }
                }
                Done::Sent { id, result } => {
                    self.manager.tick.sending.remove(&id);
                    let to = self.manager.harness.data.outbox.iter().find(|m| m.id == id).map(|m| m.to.clone()).filter(|to| !to.is_empty()).unwrap_or_else(|| "your teammate".into());
                    match result {
                        Ok(()) => {
                            messages::mark_sent(&mut self.manager.harness.data, &id, now());
                            self.mtick_line(Who::System, format!("Sent the message to {to}."), None);
                        }
                        Err(why) => {
                            messages::mark_failed(&mut self.manager.harness.data, &id, &why, now());
                            self.mtick_line(Who::System, format!("The message to {to} was not sent. {}", sentence(&why)), None);
                        }
                    }
                    self.manager.harness.dirty = true;
                    if !on {
                        self.manager.harness.save();
                    }
                    changed = true;
                }
                Done::Me(login) => {
                    if login.is_some() {
                        self.manager.me = login;
                    }
                }
                done @ (Done::Server(_) | Done::Test(_)) => self.manager.home_inbox.push(done),
            }
        }
        changed
    }

    /// The reviews the person left on other people's pull requests, read in the background.
    fn mtick_fetch_reviews(&mut self, cx: &mut Context<Self>) {
        self.manager.reviews_running = true;
        self.manager.last_reviews = Some(Instant::now());
        let me = self.manager.me.clone();
        let known: Vec<(String, u64)> = self.manager.harness.data.followups.iter().filter(|f| f.kind == FollowKind::BlockersGiven && f.is_open()).map(|f| (f.repo.clone(), f.number)).collect();
        let env = (*self.env).clone();
        let tx = self.manager.jobs.0.clone();
        cx.background_spawn(async move {
            let me = match me {
                Some(me) => Ok(me),
                None => gh::my_login(&env).inspect(|me| {
                    let _ = tx.try_send(Done::Me(Some(me.clone())));
                }),
            };
            let result = me.and_then(|me| gh::given_reviews(&me, &known, &env)).map_err(|e| format!("{e:#}"));
            let _ = tx.try_send(Done::Reviews(result));
        })
        .detach();
    }

    /// New messages from teammates, read in the background. With no messenger set in
    /// Settings › Manager nothing is started and nothing is read.
    fn mtick_fetch_inbox(&mut self, cx: &mut Context<Self>) {
        self.manager.last_inbox = Some(Instant::now());
        if self.state.manager.messenger.trim().is_empty() {
            return;
        }
        self.manager.inbox_running = true;
        // From the newest one it has; the first time, the last hour.
        let since = self.manager.harness.data.inbox.iter().map(|m| m.at).max().unwrap_or_else(|| now().saturating_sub(3600));
        let settings = self.state.manager.clone();
        let env = (*self.env).clone();
        let tx = self.manager.jobs.0.clone();
        cx.background_spawn(async move {
            let result = if messenger::available(&settings, &env) { messenger::incoming(&settings, since, &env).map_err(|e| messenger_reason(&e)) } else { Ok(Vec::new()) };
            let _ = tx.try_send(Done::Inbox(result));
        })
        .detach();
    }

    /// Who reviewed the pull requests of managed sessions, and what is unresolved: the app
    /// only reads that for the pull request on screen, so the manager keeps the rest fresh
    /// (read-only, into the same cache the Pull request tab uses).
    fn mtick_fetch_prs(&mut self, cx: &mut Context<Self>) {
        let mut open: Vec<(String, u64, PathBuf)> = Vec::new();
        for s in &self.state.sessions {
            if s.agent == AgentKind::Shell || s.review.is_some() || !self.mtick_manages(&s.project_id) {
                continue;
            }
            let Some(status) = self.prs.map.get(&s.id) else { continue };
            let (Some(repo), Some(pr)) = (&status.repo, &status.pr) else { continue };
            let over = |state: &str| matches!(state.to_uppercase().as_str(), "MERGED" | "CLOSED");
            if over(&pr.state) || status.ci.as_ref().is_some_and(|c| over(&c.state)) || open.iter().any(|(r, n, _)| r == repo && *n == pr.number) {
                continue;
            }
            open.push((repo.clone(), pr.number, s.worktree.clone()));
        }
        // A few at a time, taking turns: the next read goes on where this one stops.
        let (from, count) = turn(open.len(), self.manager.tick.prs_from, PRS_AT_ONCE);
        self.manager.tick.prs_from = from + count;
        let jobs: Vec<(String, u64, PathBuf)> = open.iter().cycle().skip(from).take(count).cloned().collect();
        // Nothing yet (the app is still finding the pull requests): look again soon.
        // More than one turn's worth: the rest follows a minute later, not three.
        let wait = if jobs.is_empty() { Duration::from_secs(20) } else if open.len() > PRS_AT_ONCE { Duration::from_secs(60) } else { PRS_EVERY };
        self.manager.tick.next_prs = Some(Instant::now() + wait);
        if jobs.is_empty() {
            return;
        }
        self.manager.tick.prs_running = true;
        let env = (*self.env).clone();
        let task = cx.background_spawn(async move {
            jobs.into_iter()
                .filter_map(|(repo, number, worktree)| {
                    let preview = PreviewConfig::load(&worktree);
                    github::pr_details(&repo, number, &preview, &env).ok().map(|details| ((repo, number), details))
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |ws, _| {
                ws.manager.tick.prs_running = false;
                if ws.state.manager.enabled {
                    ws.gh.details.extend(found);
                }
            });
        })
        .detach();
    }

    /// Keep the end of each managed session's last reply at hand: the question on its screen
    /// while it needs the person, and its transcript's end, read in the background whenever
    /// the file changed.
    fn mtick_refresh_replies(&mut self, cx: &mut Context<Self>) {
        let mut questions: Vec<(String, Option<String>)> = Vec::new();
        let mut jobs: Vec<(String, AgentKind, PathBuf, (u64, Option<SystemTime>))> = Vec::new();
        for s in &self.state.sessions {
            if s.agent == AgentKind::Shell || !self.mtick_manages(&s.project_id) {
                continue;
            }
            let state = self.session_state(s);
            let question = if state == "needs you" { self.terminals.get(&s.id).and_then(|t| prompt::parse(&t.read(cx).text_tail(60))).map(|p| question_text(&p)) } else { None };
            questions.push((s.id.clone(), question));
            let Some(path) = s.transcript_path.clone() else { continue };
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let stamp = (meta.len(), meta.modified().ok());
            // While it works the file grows all the time: it is read again when it stops.
            let known = self.manager.tick.replies.get(&s.id).and_then(|r| r.stamp);
            if known.is_some_and(|k| k == stamp || state == "working") {
                continue;
            }
            jobs.push((s.id.clone(), s.agent, path, stamp));
        }
        let replies = &mut self.manager.tick.replies;
        replies.retain(|id, _| questions.iter().any(|(q, _)| q == id));
        for (id, question) in questions {
            replies.entry(id).or_default().question = question;
        }
        if jobs.is_empty() || self.manager.tick.replies_running {
            return;
        }
        self.manager.tick.replies_running = true;
        let task = cx.background_spawn(async move { jobs.into_iter().map(|(id, agent, path, stamp)| (id, stamp, reply_end(agent, &path))).collect::<Vec<_>>() });
        cx.spawn(async move |this, cx| {
            let read = task.await;
            let _ = this.update(cx, |ws, _| {
                ws.manager.tick.replies_running = false;
                for (id, stamp, text) in read {
                    let entry = ws.manager.tick.replies.entry(id).or_default();
                    entry.stamp = Some(stamp);
                    entry.text = text;
                }
            });
        })
        .detach();
    }

    /// A project the manager looks at: managed, and not switched off in Settings › Projects.
    fn mtick_manages(&self, project_id: &str) -> bool {
        self.state.manager.manages(project_id) && !self.state.is_hidden(project_id)
    }
}

// ----- what it sees -----------------------------------------------------------------------------

impl Workspace {
    /// What the manager sees right now (managed projects only). Built from what the app has
    /// at hand: it reads no file and calls nothing.
    pub fn manager_snapshot(&self) -> Snapshot {
        let mut sessions = Vec::new();
        // Repositories of managed sessions, and of the ones it must not look at.
        let (mut ours, mut others): (HashSet<&str>, HashSet<&str>) = Default::default();
        for s in &self.state.sessions {
            let repo = self.prs.map.get(&s.id).and_then(|p| p.repo.as_deref());
            if !self.mtick_manages(&s.project_id) {
                others.extend(repo);
                continue;
            }
            ours.extend(repo);
            let cached = self.manager.tick.replies.get(&s.id);
            sessions.push(SessionInfo {
                id: s.id.clone(),
                title: self.session_title(s),
                project_id: s.project_id.clone(),
                project: self.state.project(&s.project_id).map(|p| p.name.clone()).unwrap_or_default(),
                agent: Some(s.agent),
                state: state_of(self.session_state(s)),
                on_server: false,
                branch: s.branch.clone(),
                worktree: s.worktree.clone(),
                last_reply: cached.and_then(|r| r.question.clone().or_else(|| r.text.clone())),
                pr: self.mtick_pr(s),
                ticket: self.state.ticket_of.get(&s.id).cloned(),
                review_of: s.review.as_ref().map(|r| (r.repo.clone(), r.number)),
            });
        }
        // The sessions on the server, as the last sync showed them.
        if let Some(server) = self.server.state.as_ref().filter(|_| self.server.connected()) {
            for r in &server.sessions {
                let s = &r.session;
                if self.state.session(&s.id).is_some() {
                    continue;
                }
                // The Mac has its own copy of the project under the same name: its switch counts.
                let name = server.projects.iter().find(|p| p.id == s.project_id).map(|p| p.name.clone()).unwrap_or_default();
                let mac = self.state.projects.iter().find(|p| p.id == s.project_id || (!name.is_empty() && p.name == name));
                if !self.mtick_manages(mac.map(|p| p.id.as_str()).unwrap_or(&s.project_id)) {
                    continue;
                }
                let state = match r.activity {
                    _ if r.prompt.is_some() => SessionState::NeedsYou,
                    Some(AgentActivity::NeedsInput) => SessionState::NeedsYou,
                    Some(AgentActivity::Working) => SessionState::Working,
                    _ if r.running => SessionState::Idle,
                    _ => SessionState::NotRunning,
                };
                let repo = server.repos.get(&s.project_id).cloned().unwrap_or_default();
                sessions.push(SessionInfo {
                    id: s.id.clone(),
                    title: s.title.clone(),
                    project_id: s.project_id.clone(),
                    project: name,
                    agent: Some(s.agent),
                    state,
                    on_server: true,
                    branch: s.branch.clone(),
                    worktree: s.worktree.clone(),
                    last_reply: r.prompt.as_ref().map(question_text).or_else(|| r.last_reply.as_deref().map(|t| tail_of(t, REPLY_CHARS))),
                    pr: r.pr.as_ref().map(|pr| PrInfo { author: s.review.as_ref().map(|r| r.author.clone()).unwrap_or_default(), ..pr_from_summary(repo, pr) }),
                    ticket: self.state.ticket_of.get(&s.id).cloned(),
                    review_of: s.review.as_ref().map(|r| (r.repo.clone(), r.number)),
                });
            }
        }
        let review_asks = self
            .tickets
            .reviews
            .iter()
            .filter(|r| ours.contains(r.repo.as_str()) || !others.contains(r.repo.as_str()))
            .map(|r| ReviewAsk { repo: r.repo.clone(), number: r.hit.number, title: r.hit.title.clone(), author: r.hit.author.clone(), url: r.hit.url.clone() })
            .collect();
        Snapshot { at: now(), me: self.manager.me.clone(), sessions, review_asks, approvals: self.control.approvals.len(), mac_online: true }
    }

    /// A session's pull request, from what the app found for it and what was read of it.
    fn mtick_pr(&self, s: &Session) -> Option<PrInfo> {
        let status = self.prs.map.get(&s.id)?;
        let repo = status.repo.clone()?;
        let pr = status.pr.as_ref()?;
        let details = self.gh.details.get(&(repo.clone(), pr.number));
        let mut info = pr_info(repo, pr, status.ci.as_ref(), details);
        // A review session knows whose pull request it reviews.
        if info.author.is_empty() {
            info.author = s.review.as_ref().map(|r| r.author.clone()).unwrap_or_default();
        }
        Some(info)
    }
}

// ----- talking to it ----------------------------------------------------------------------------

impl Workspace {
    /// The person said or typed something to it. Adds the line to the conversation, answers
    /// at once when the rules can (`Harness::quick_answer`), else asks the brain in the
    /// background (or the server's copy when that runs it).
    pub fn manager_ask(&mut self, text: String, voice: bool, cx: &mut Context<Self>) {
        self.mtick_ask(text, voice, None, cx);
    }

    /// The same, and `on_answer` is called with what it says (or why it could not answer).
    /// `glasscode manager ask` and the voice use this to wait for the answer.
    pub fn manager_ask_then(&mut self, text: String, voice: bool, on_answer: Box<dyn FnOnce(&mut Workspace, Result<String, String>, &mut Context<Workspace>)>, cx: &mut Context<Self>) {
        self.mtick_ask(text, voice, Some(on_answer), cx);
    }

    fn mtick_ask(&mut self, text: String, voice: bool, on_answer: Option<OnAnswer>, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        let refused = if !self.state.manager.enabled {
            Some("The manager is switched off. Settings › Manager switches it on.")
        } else if text.is_empty() {
            Some("There was nothing to answer.")
        } else {
            None
        };
        if let Some(why) = refused {
            if let Some(on_answer) = on_answer {
                on_answer(self, Err(why.to_string()), cx);
            }
            return;
        }
        self.mtick_line(Who::You, text.clone(), None);
        if self.manager.tour.is_some() {
            self.manager_stop_tour(cx);
        }
        let snap = self.manager_snapshot();
        if let Some(Answer { say, actions }) = self.manager.harness.quick_answer(&self.state.manager, &snap, &text, now()) {
            self.manager_say(&say, None, cx);
            // The answer is said once, also when the rules list it among what to do.
            let actions = actions.into_iter().filter(|a| !matches!(a, Action::Say { text, .. } if text.trim() == say.trim())).collect();
            self.manager_apply(actions, cx);
            self.manager.harness.save();
            if let Some(on_answer) = on_answer {
                on_answer(self, Ok(say), cx);
            }
            cx.notify();
            return;
        }
        let id = self.manager.next_ask;
        self.manager.next_ask += 1;
        let key = id.to_string();
        if let Some(on_answer) = on_answer {
            self.manager.tick.waiting.insert(key.clone(), on_answer);
        }
        self.manager.tick.open_asks.insert(key.clone(), Instant::now());
        self.manager.thinking = true;
        if self.manager_on_server() {
            self.manager.asks.push(Ask { id: key, text, voice });
        } else {
            self.mtick_ask_brain(id, text, voice, &snap, cx);
        }
        cx.notify();
    }

    /// Hand a question to the brain, off the main thread; the answer comes back as
    /// `Done::Answer`.
    fn mtick_ask_brain(&mut self, id: u64, text: String, voice: bool, snap: &Snapshot, cx: &mut Context<Self>) {
        let brain = self.mtick_brain(cx);
        let context = brain::context(&self.state.manager, &self.manager.harness.data, snap, &text);
        let tx = self.manager.jobs.0.clone();
        cx.background_spawn(async move {
            // One question at a time: the next waits here until this one is answered.
            let say = brain.lock().unwrap_or_else(|e| e.into_inner()).ask(&text, &context, voice).map_err(|e| format!("{e:#}"));
            let _ = tx.try_send(Done::Answer { id, say, voice });
        })
        .detach();
    }

    /// A scheduled check: the brain is asked in the person's words, and what it finds is said
    /// like any answer. The conversation shows it as the app's line, not as the person's.
    fn mtick_run_check(&mut self, what: &str, snap: &Snapshot, cx: &mut Context<Self>) {
        let what = what.trim();
        self.mtick_line(Who::System, format!("Checking: {what}"), None);
        let id = self.manager.next_ask;
        self.manager.next_ask += 1;
        self.manager.tick.open_asks.insert(id.to_string(), Instant::now());
        self.manager.thinking = true;
        self.mtick_ask_brain(id, check_question(what), false, snap, cx);
    }

    /// The agent that thinks for it, started with the first question (and again after its
    /// agent, account or model changed).
    fn mtick_brain(&mut self, cx: &mut Context<Self>) -> Arc<Mutex<Brain>> {
        let settings = &self.state.manager;
        let key = (settings.brain, settings.account.clone(), settings.model.clone());
        if let Some(brain) = self.manager.brain.as_ref().filter(|_| self.manager.tick.brain_key.as_ref() == Some(&key)) {
            return brain.clone();
        }
        self.mtick_stop_brain(cx);
        let settings = &self.state.manager;
        let account = settings
            .account
            .as_deref()
            .and_then(|wanted| self.state.accounts_for(settings.brain).find(|a| a.id == wanted || a.label.eq_ignore_ascii_case(wanted)))
            .or_else(|| self.state.default_account(settings.brain));
        let mut env = (*self.env).clone();
        // Like a session: its `glasscode` commands reach this app.
        env.insert("GLASSCODE_CONTROL_SOCKET".into(), control::socket_path().to_string_lossy().into_owned());
        let config = BrainConfig { config_dir: account.and_then(|a| a.config_dir()), env, cwd: store::dir().join("work") };
        let brain = Arc::new(Mutex::new(Brain::new(settings, config)));
        self.manager.brain = Some(brain.clone());
        self.manager.tick.brain_key = Some(key);
        brain
    }

    /// Stop the brain (the next question starts a fresh one). It may be in the middle of an
    /// answer, so the stopping waits for that off the main thread.
    fn mtick_stop_brain(&mut self, cx: &mut Context<Self>) {
        self.manager.tick.brain_key = None;
        let Some(brain) = self.manager.brain.take() else { return };
        cx.background_spawn(async move { brain.lock().unwrap_or_else(|e| e.into_inner()).stop() }).detach();
    }

    /// An answer came in for question `id` (from the brain's background job, or from the
    /// server's copy): show it, speak it, and call whoever waits for it.
    pub fn manager_answered(&mut self, id: &str, say: Result<String, String>, _voice: bool, cx: &mut Context<Self>) {
        self.manager.tick.open_asks.remove(id);
        self.manager.thinking = !self.manager.tick.open_asks.is_empty();
        match &say {
            Ok(text) => self.manager_say(text, None, cx),
            Err(why) => {
                let line = cannot_answer(&self.state.manager.name, why);
                self.mtick_line(Who::System, line, None);
            }
        }
        if let Some(on_answer) = self.manager.tick.waiting.remove(id) {
            on_answer(self, say, cx);
        }
        self.manager.harness.save();
        cx.notify();
    }

    /// It says something: the conversation, the caption, and its voice when that is on.
    pub fn manager_say(&mut self, text: &str, show: Option<Show>, cx: &mut Context<Self>) {
        self.mtick_say(text, show, cx);
    }

    /// The same; tells whether its voice speaks it.
    fn mtick_say(&mut self, text: &str, show: Option<Show>, cx: &mut Context<Self>) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        self.mtick_line(Who::Manager, text, show);
        let spoken = self.manager_voice_speak(text, cx);
        let until = Instant::now() + say_time(text, spoken);
        self.manager.caption = text.to_string();
        self.manager.speaking_until = Some(until);
        self.manager.caption_until = Some(until + CAPTION_STAYS);
        cx.notify();
        spoken
    }

    /// Add a line to the conversation (the last `CHAT_MAX` are kept).
    fn mtick_line(&mut self, who: Who, text: impl Into<String>, show: Option<Show>) {
        let chat = &mut self.manager.harness.data.chat;
        chat.push(ChatLine { who, text: text.into(), at: now(), show });
        if chat.len() > CHAT_MAX {
            let extra = chat.len() - CHAT_MAX;
            chat.drain(..extra);
        }
        self.manager.harness.dirty = true;
    }

    /// A line in what agents did (Today lists it), for what the manager did to a session.
    fn mtick_log(&mut self, what: String) {
        let entry = LogEntry { at: now(), who: self.state.manager.name.clone(), what, outcome: "done".into(), ..Default::default() };
        control::append_log(&entry);
        self.control.log.insert(0, entry);
        self.control.log.truncate(200);
    }
}

// ----- carrying out what it decided -------------------------------------------------------------

impl Workspace {
    /// Carry out what the manager decided.
    pub fn manager_apply(&mut self, actions: Vec<Action>, cx: &mut Context<Self>) {
        if !self.state.manager.enabled {
            return;
        }
        for action in actions {
            match action {
                Action::TellSession { session, text } => self.mtick_tell(session, &text, cx),
                Action::Notify { title, body, session } => self.mtick_notify(title, body, session, cx),
                Action::Say { text, show } => self.manager_say(&text, show, cx),
                Action::Draft { to, body, why, about } => self.mtick_draft(&to, &body, &why, about.as_deref(), cx),
                Action::Send { id } => self.mtick_send(&id, cx),
                Action::Remember { text } => {
                    memory::remember(&mut self.manager.harness.data, &text, "manager", now());
                    self.manager.harness.dirty = true;
                }
                Action::Show { show } => self.with_main_window(cx, move |ws, window, cx| ws.manager_show(&show, window, cx)),
                Action::Tour { stops } => self.manager_start_tour(stops, cx),
                Action::Scene { scene } => self.mtick_play(scene, cx),
                Action::StartSession { project, title, message, agent } => self.mtick_start_session(&project, title, message, agent, cx),
            }
        }
    }

    /// Queue a message for a session and type it as soon as the session can take it.
    fn mtick_tell(&mut self, session: String, text: &str, cx: &mut Context<Self>) {
        let line = tell_line(&self.state.manager.name, text);
        if line.is_empty() || self.manager.tick.tells.iter().any(|t| t.session == session && t.text == line) {
            return;
        }
        self.manager.tick.tells.push(Tell { session, text: line, since: Instant::now() });
        self.mtick_flush_tells(cx);
    }

    /// Type the waiting messages into the sessions that can take one now. A session that is
    /// working, asking, not running, or has something typed in its box keeps its messages for
    /// a later round; they go in the order they came.
    fn mtick_flush_tells(&mut self, cx: &mut Context<Self>) {
        if self.manager.tick.tells.is_empty() {
            return;
        }
        // Settings › Automation: "Send a message to another session" set to never.
        let never = self.state.agents.rule(Kind::Send) == Rule::Never;
        let mut keep = Vec::new();
        let mut held: HashSet<String> = HashSet::new();
        for tell in std::mem::take(&mut self.manager.tick.tells) {
            let session = match self.state.session(&tell.session) {
                Some(s) => Some((s.agent, self.session_title(s))),
                None => self.server.session(&tell.session).map(|r| (r.session.agent, r.session.title.clone())),
            };
            // The session is gone, is a shell, or the message is a day old: let it go.
            let Some((agent, title)) = session else { continue };
            if agent == AgentKind::Shell || tell.since.elapsed() > TELL_KEEP {
                continue;
            }
            if never {
                self.mtick_line(Who::System, format!("“{title}” was not told: Settings › Automation says agents may never send a message to another session."), None);
                continue;
            }
            if held.contains(&tell.session) || !self.mtick_can_type(&tell.session, cx) || !self.mtick_type(&tell.session, &tell.text, cx) {
                held.insert(tell.session.clone());
                keep.push(tell);
                continue;
            }
            // One at a time: the next one for this session waits for its answer.
            held.insert(tell.session.clone());
            self.manager.tick.told_at.insert(tell.session.clone(), Instant::now());
            let short: String = tell.text.chars().take(80).collect();
            self.mtick_log(format!("Sent “{short}” to the session “{title}”"));
        }
        if keep.len() > 100 {
            let extra = keep.len() - 100;
            keep.drain(..extra);
        }
        self.manager.tick.tells = keep;
    }

    /// Whether a session's agent can be told something now without getting in anyone's way:
    /// running, its turn over, not asking, its screen quiet, nobody typing in it (the same
    /// test Glasscode uses before it asks an agent to fix CI).
    fn mtick_can_type(&self, id: &str, cx: &mut Context<Self>) -> bool {
        let (terminal, activity) = match self.terminals.get(id) {
            Some(terminal) => (terminal, self.activity.get(id).copied()),
            // A session on the server, when its terminal is open here.
            None => match (self.server.terminals.get(id), self.server.session(id)) {
                (Some(terminal), Some(remote)) => (terminal, remote.activity),
                _ => return false,
            },
        };
        if self.manager.tick.told_at.get(id).is_some_and(|at| at.elapsed() < TELL_GAP) {
            return false;
        }
        let t = terminal.read(cx);
        if t.exit_code.is_some() {
            return false;
        }
        if t.last_typed.is_some_and(|at| at.elapsed() < Duration::from_secs(120)) || t.last_output.elapsed() < Duration::from_secs(8) {
            return false;
        }
        match activity {
            Some(AgentActivity::Working) | Some(AgentActivity::NeedsInput) => return false,
            Some(AgentActivity::Idle) => {}
            // No hooks (Codex): a screen quiet for a while means its turn is over.
            None => {
                if t.last_output.elapsed() < Duration::from_secs(30) {
                    return false;
                }
            }
        }
        prompt::parse(&t.text_tail(60)).is_none() && !crate::ci_fix::has_draft(&t.text_tail_typed(60))
    }

    /// Type a line into a session's agent and submit it; false when it cannot take one.
    fn mtick_type(&mut self, id: &str, text: &str, cx: &mut Context<Self>) -> bool {
        if self.terminals.contains_key(id) {
            return self.type_into_session(id, text, cx);
        }
        let Some(terminal) = self.server.terminals.get(id).cloned() else { return false };
        if prompt::parse(&terminal.read(cx).text_tail(60)).is_some() {
            return false;
        }
        let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        terminal.update(cx, |t, cx| t.write(line.into_bytes(), cx));
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
        })
        .detach();
        true
    }

    /// A notification for the person, unless they are looking at that session already.
    fn mtick_notify(&mut self, title: String, body: String, session: Option<String>, cx: &mut Context<Self>) {
        // About no session (or one that is gone): Open leads to the manager's page.
        let Some((id, agent)) = session.as_deref().and_then(|id| self.state.session(id)).map(|s| (s.id.clone(), s.agent)) else {
            self.notify_manager(title, body, cx);
            return;
        };
        let watching = self.window_active && self.tickets.view == View::Sessions && self.center == Center::Session && self.state.active_session.as_deref() == Some(id.as_str());
        if !watching {
            self.notify_session(&id, agent, title, body, true, cx);
        }
    }

    /// Put a message to a teammate in the outbox, and say so when it waits for the person.
    fn mtick_draft(&mut self, to: &str, body: &str, why: &str, about: Option<&str>, cx: &mut Context<Self>) {
        let known: HashSet<String> = self.manager.harness.data.outbox.iter().map(|m| m.id.clone()).collect();
        let message = messages::draft(&mut self.manager.harness.data, &self.state.manager, to, body, why, about, now());
        self.manager.harness.dirty = true;
        // The same text to the same person again is the message it already has.
        if message.id.is_empty() || known.contains(&message.id) || message.state != MsgState::Draft || message.tier == Tier::Never {
            return;
        }
        let text = format!("A message to {} waits for your OK.", message.to);
        self.manager_say(&text, Some(Show { view: Some("manager".into()), ..Default::default() }), cx);
        if !self.window_active {
            let title = format!("{} asks you", self.state.manager.name);
            self.mtick_notify(title, text, None, cx);
        }
    }

    /// Whether sending is the server's: it is the manager's home and its copy did a round in
    /// the last ten minutes. (This Mac's rounds take over after 30 s of silence; a message
    /// must not go out from both in that time.)
    fn mtick_server_sends(&self) -> bool {
        sync::runs_on_server(&self.state.manager, self.manager.server_heartbeat, now(), SERVER_SENDS_SECS)
    }

    /// The outbox message with this id, when it may go out now.
    fn mtick_sendable(&self, id: &str) -> Option<OutMessage> {
        self.manager.harness.data.outbox.iter().find(|m| m.id == id && m.state == MsgState::Approved && m.tier != Tier::Never).cloned()
    }

    /// Send an outbox message the person allowed, in the background, through the messenger
    /// set in Settings › Manager. It goes to the teammate's contact when Settings has one,
    /// else to their name.
    fn mtick_send(&mut self, id: &str, cx: &mut Context<Self>) {
        // Only the copy at home sends: with the server as home and heard from lately, the
        // message stays allowed and the server sends it, also while this Mac does the rounds.
        if self.manager.tick.sending.contains(id) || self.mtick_server_sends() {
            return;
        }
        // Looked at right before it goes: still allowed, and never one it must not send.
        let Some(message) = self.mtick_sendable(id) else { return };
        let person = self.state.manager.person(&message.to);
        let to = recipient(person.and_then(|p| p.contact.as_deref()), person.map(|p| p.name.as_str()), &message.to);
        let settings = self.state.manager.clone();
        let env = (*self.env).clone();
        let id = id.to_string();
        self.manager.tick.sending.insert(id.clone());
        let tx = self.manager.jobs.0.clone();
        cx.background_spawn(async move {
            let result = messenger::send(&settings, &to, &message.body, message.urgent, &env).map_err(|e| messenger_reason(&e));
            let _ = tx.try_send(Done::Sent { id, result });
        })
        .detach();
    }

    /// Show a picture on its page and say its first step.
    fn mtick_play(&mut self, scene: Scene, cx: &mut Context<Self>) {
        // A picture that cannot be drawn (a step names something that is not there) is not shown.
        if let Err(why) = scene::check(&scene) {
            let line = format!("{} could not draw that. {}", self.state.manager.name, sentence(&format!("{why:#}")));
            self.mtick_line(Who::System, line, None);
            cx.notify();
            return;
        }
        let Some(first) = scene.steps.first().map(|s| s.say.clone()) else { return };
        let more = scene.steps.len() > 1;
        self.manager.scene = Some(ScenePlay { scene, step: 0, next_at: None });
        self.manager.stage = Stage::Scene;
        self.show_view(View::Manager, cx);
        let spoken = self.mtick_say(&first, None, cx);
        if let Some(play) = self.manager.scene.as_mut() {
            play.next_at = more.then(|| Instant::now() + stop_time(&first, spoken));
        }
        cx.notify();
    }

    /// Start a session the way `glasscode session new` does: in the background, without
    /// taking the keyboard. Settings › Automation's "never" for starting sessions holds.
    fn mtick_start_session(&mut self, project: &str, title: String, message: String, agent: Option<AgentKind>, cx: &mut Context<Self>) {
        let name = self.state.manager.name.clone();
        let agent = agent.unwrap_or(AgentKind::Claude);
        let what = if title.trim().is_empty() { "a session".to_string() } else { format!("“{}”", title.trim()) };
        let spec = if self.state.agents.rule(Kind::Start) == Rule::Never {
            Err("Settings › Automation says agents may never start sessions.".to_string())
        } else if agent == AgentKind::Shell {
            Err("A shell has no agent to give the work to.".to_string())
        } else {
            self.find_project(project).and_then(|project_id| Ok((self.find_account(agent, None)?, project_id))).map_err(|reply| reply_text(&reply)).and_then(|(account_id, project_id)| {
                if !self.mtick_manages(&project_id) {
                    return Err(format!("{name} does not manage {project}."));
                }
                Ok(NewSpec {
                    project_id,
                    agent,
                    account_id,
                    title: Some(title.trim().to_string()).filter(|t| !t.is_empty()),
                    branch: None,
                    worktree: None,
                    from: None,
                    ticket: None,
                    pr: None,
                    review: None,
                    model: None,
                    mode: None,
                    message: Some(message.trim().to_string()).filter(|m| !m.is_empty()),
                    open: false,
                    on_server: false,
                    started_by: name.clone(),
                })
            })
        };
        let spec = match spec {
            Ok(spec) => spec,
            Err(why) => {
                self.mtick_line(Who::System, format!("{name} did not start {what}. {}", sentence(&why)), None);
                cx.notify();
                return;
            }
        };
        self.with_main_window(cx, move |ws, window, cx| {
            let responder = Responder::local(move |ws, reply| match reply {
                Reply::Done { code: status, data, text } if *status == code::OK => {
                    let title = data["title"].as_str().unwrap_or_default().to_string();
                    ws.mtick_log(format!("Started “{title}”"));
                    match data["id"].as_str() {
                        Some(id) => ws.manager.tick.later.push(Later::Started { id: id.to_string(), title }),
                        None => ws.manager.tick.later.push(Later::Line(text.clone())),
                    }
                }
                other => ws.manager.tick.later.push(Later::Line(format!("{name} did not start {what}. {}", sentence(&reply_text(other))))),
            });
            ws.start_session(spec, responder, window, cx);
        });
    }

    /// The person allows or rejects a message in the outbox (optionally with a changed text).
    pub fn manager_decide_message(&mut self, id: &str, allow: bool, edited: Option<String>, cx: &mut Context<Self>) {
        let decided = messages::decide(&mut self.manager.harness.data, id, allow, edited.as_deref(), now());
        self.manager.harness.dirty = true;
        // Allowed: it goes out now (from here, unless the server's copy does the sending).
        if let Some(message) = decided.filter(|m| allow && m.state == MsgState::Approved) {
            self.manager_apply(vec![Action::Send { id: message.id }], cx);
        }
        self.manager.harness.save();
        cx.notify();
    }

    /// Switch the manager on or off, or change its settings: save, and start or stop its parts.
    pub fn manager_settings_changed(&mut self, cx: &mut Context<Self>) {
        self.save();
        if self.state.manager.enabled {
            // Another agent, account or model: the next question starts a fresh brain.
            let settings = &self.state.manager;
            let key = (settings.brain, settings.account.clone(), settings.model.clone());
            if self.manager.brain.is_some() && self.manager.tick.brain_key.as_ref() != Some(&key) {
                self.mtick_stop_brain(cx);
            }
            // Its first round comes with the next tick.
            self.manager.last_round = None;
        } else {
            self.mtick_stop_brain(cx);
            if self.manager.live {
                self.manager_voice_toggle(cx);
            }
            let manager = &mut self.manager;
            manager.tour = None;
            manager.tick.tour_waits = false;
            manager.highlight = None;
            manager.scene = None;
            if manager.stage == Stage::Scene {
                manager.stage = Stage::Overview;
            }
            manager.thinking = false;
            manager.caption.clear();
            manager.speaking_until = None;
            manager.caption_until = None;
            manager.asks.clear();
            manager.tick.open_asks.clear();
            manager.tick.tells.clear();
            manager.tick.later.clear();
            manager.harness.save();
            // Whoever waits for an answer hears that none comes.
            let waiting: Vec<OnAnswer> = self.manager.tick.waiting.drain().map(|(_, on_answer)| on_answer).collect();
            for on_answer in waiting {
                on_answer(self, Err("The manager was switched off.".into()), cx);
            }
        }
        cx.notify();
    }
}

// ----- taking the person through the app --------------------------------------------------------

impl Workspace {
    /// Go where `show` says and point at what it names.
    pub fn manager_show(&mut self, show: &Show, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.manager.enabled {
            return;
        }
        match show.view.as_deref().map(|v| v.trim().to_lowercase()).as_deref() {
            Some("manager") => self.show_manager(window, cx),
            Some("sessions" | "session") => self.show_view(View::Sessions, cx),
            Some("today" | "board") => self.show_today(cx),
            Some("tickets") => self.show_tickets(cx),
            Some("usage") => self.show_view(View::Usage, cx),
            Some("themes") => self.show_view(View::Themes, cx),
            Some("settings") => self.open_settings(self.settings.section, window, cx),
            _ => {}
        }
        // On its own page a section is one of the things it holds, not a part of a panel.
        if show.session.is_none() && show.view.as_deref().is_some_and(|v| v.trim().eq_ignore_ascii_case("manager")) {
            if let Some(stage) = show.section.as_deref().and_then(stage_of) {
                self.manager.stage = stage;
            }
            self.manager.highlight = None;
            cx.notify();
            return;
        }
        if let Some(id) = show.session.as_deref() {
            if self.state.session(id).is_some_and(|s| !self.state.session_hidden(s)) {
                self.tickets.view = View::Sessions;
                self.menu_open = false;
                if self.state.active_session.as_deref() != Some(id) || self.center != Center::Session {
                    self.select_session(id, window, cx);
                }
            } else if self.server.session(id).is_some() {
                self.tickets.view = View::Sessions;
                self.menu_open = false;
                self.open_server_session(id, window, cx);
            } else {
                // The session is gone: there is nothing to show.
                self.manager.highlight = None;
                cx.notify();
                return;
            }
        }
        let section = show.section.as_deref().map(section_key).filter(|s| !s.is_empty());
        if let Some(tab) = show.tab.or_else(|| section.as_deref().and_then(tab_of_section)) {
            // The panel belongs to a session: make sure one is on screen.
            if show.view.is_none() && show.session.is_none() {
                self.tickets.view = View::Sessions;
            }
            self.right_tab = match tab {
                PanelTab::PullRequest => RightTab::PullRequest,
                PanelTab::Changes => RightTab::Changes,
                PanelTab::Files => RightTab::Files,
            };
            self.show_right = true;
            if tab == PanelTab::Files {
                self.files_tick(cx);
            }
        }
        self.manager.highlight = section.map(|section| {
            // At a stop of a walk it points for as long as the stop lasts.
            let until = (Instant::now() + POINT_FOR).max(self.manager.tour.as_ref().map(|t| t.next_at).unwrap_or_else(Instant::now));
            Highlight { session: show.session.clone().or_else(|| self.state.active_session.clone()), section, until }
        });
        cx.notify();
    }

    /// Start a walk through the app: go to its first stop.
    pub fn manager_start_tour(&mut self, stops: Vec<TourStop>, cx: &mut Context<Self>) {
        if stops.is_empty() || !self.state.manager.enabled {
            return;
        }
        // It finishes the sentence it is saying (the answer that announced the walk) first.
        let now = Instant::now();
        match self.manager.speaking_until.filter(|until| *until > now) {
            Some(until) => {
                self.manager.tour = Some(Tour { stops, index: 0, next_at: until });
                self.manager.tick.tour_waits = true;
                cx.notify();
            }
            None => {
                self.manager.tour = Some(Tour { stops, index: 0, next_at: now });
                self.manager_tour_go(0, cx);
            }
        }
    }

    /// Go to a stop of the walk: show it and say its sentence. The walk moves on by itself
    /// when the sentence is over, and ends after the last stop.
    pub fn manager_tour_go(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(stop) = self.manager.tour.as_ref().map(|t| t.stops.get(index).cloned()) else { return };
        let Some(stop) = stop else {
            self.manager_stop_tour(cx);
            return;
        };
        self.manager.tick.tour_waits = false;
        let spoken = self.mtick_say(&stop.say, Some(stop.show.clone()), cx);
        if let Some(tour) = self.manager.tour.as_mut() {
            tour.index = index;
            tour.next_at = Instant::now() + stop_time(&stop.say, spoken);
        }
        let show = stop.show;
        self.with_main_window(cx, move |ws, window, cx| ws.manager_show(&show, window, cx));
        cx.notify();
    }

    /// End the walk (the person asked something, clicked, or it is over).
    pub fn manager_stop_tour(&mut self, cx: &mut Context<Self>) {
        self.manager.tick.tour_waits = false;
        if self.manager.tour.take().is_some() {
            self.manager.highlight = None;
            if self.manager.live {
                self.manager_voice_stop(cx);
            }
        }
        cx.notify();
    }

    /// Whether it is pointing at this part of the active session's right panel now
    /// ("checks", "preview", "reviews", "comments", "changes", "files", "reply").
    pub fn manager_points_at(&self, section: &str) -> bool {
        let Some(highlight) = self.manager.highlight.as_ref() else { return false };
        self.state.manager.enabled
            && highlight.section == section
            && highlight.until > Instant::now()
            && self.tickets.view == View::Sessions
            && self.center == Center::Session
            && highlight.session.as_deref().is_none_or(|id| self.state.active_session.as_deref() == Some(id))
    }

    /// Mark a row (a section's heading) while the manager points at `section`: a warm outline,
    /// a tint, and "<name> is pointing here" at its end. Otherwise `el` comes back as it is.
    pub fn manager_mark<E: Styled + ParentElement>(&self, section: &str, el: E) -> E {
        if !self.manager_points_at(section) {
            return el;
        }
        el.mt_2().px_2().pt_1p5().pb_1p5().rounded(px(8.)).border(px(1.5)).border_color(gold()).bg(gold_bg()).child(pointing_tag(&self.state.manager.name))
    }

    /// The same for a whole tab of the panel: the mark goes around it, the caption on top.
    pub fn manager_mark_any(&self, section: &str, el: AnyElement) -> AnyElement {
        if !self.manager_points_at(section) {
            return el;
        }
        pointed(&self.state.manager.name, el, true)
    }
}

// ----- pointing ---------------------------------------------------------------------------------

/// "<name> is pointing here", in the manager's own colour.
fn pointing_tag(name: &str) -> Div {
    div().flex_none().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(gold()).child(format!("{name} is pointing here"))
}

/// `el` with the mark around it and "<name> is pointing here" above. `fill`: it takes the
/// room its place in a column gives it (a tab of the panel); else it is as tall as `el`.
pub(crate) fn pointed(name: &str, el: AnyElement, fill: bool) -> AnyElement {
    let frame = div().flex().flex_col().rounded(px(8.)).border(px(1.5)).border_color(gold()).bg(gold_bg());
    let frame = if fill { frame.flex_1().min_h_0() } else { frame.p_2() };
    frame.child(pointing_tag(name).px_2().py_1()).child(el).into_any_element()
}

// ----- small things -----------------------------------------------------------------------------

/// How long it takes to say a sentence: about a second and 22 ms a character on screen (at
/// most six seconds), and the pace of speech when its voice says it.
fn say_time(text: &str, spoken: bool) -> Duration {
    let chars = text.chars().count() as u64;
    if spoken { Duration::from_millis((1000 + 65 * chars).min(90_000)) } else { Duration::from_millis((1000 + 22 * chars).min(6000)) }
}

/// How long a stop of a walk (or a step of a picture) lasts: its sentence, said or read at
/// an easy pace, and two seconds to look.
fn stop_time(text: &str, spoken: bool) -> Duration {
    let chars = text.chars().count() as u64;
    let sentence = if spoken { say_time(text, true) } else { Duration::from_millis((1000 + 45 * chars).clamp(3000, 20_000)) };
    sentence + Duration::from_secs(2)
}

/// Whose turn it is: where to start among `len` things when the last turn stopped at `from`,
/// and how many to take (at most `at_once`, never one twice).
fn turn(len: usize, from: usize, at_once: usize) -> (usize, usize) {
    if len == 0 {
        return (0, 0);
    }
    (from % len, len.min(at_once))
}

/// The app's word for a session's state, as the manager's.
fn state_of(word: &str) -> SessionState {
    match word {
        "needs you" => SessionState::NeedsYou,
        "working" => SessionState::Working,
        "idle" => SessionState::Idle,
        _ => SessionState::NotRunning,
    }
}

/// The end of a text, cut at a word, with "…" in front when something is left out.
fn tail_of(text: &str, max: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let tail: String = text.chars().skip(count - max).collect();
    let tail = match tail.find(char::is_whitespace) {
        Some(at) if at < max / 4 => tail[at..].trim_start(),
        _ => tail.as_str(),
    };
    format!("…{tail}")
}

/// The end of the agent's last reply in a transcript (None while the person spoke last).
fn reply_end(agent: AgentKind, path: &Path) -> Option<String> {
    let len = std::fs::metadata(path).ok()?.len();
    let page = transcript::read(agent, path, Some(len.saturating_sub(512 * 1024)), 400).ok()?;
    let last = page.items.iter().rev().find(|i| i.kind != ItemKind::Tool)?;
    (last.kind == ItemKind::Assistant).then(|| tail_of(&last.text, REPLY_CHARS))
}

/// The question on an agent's screen, in a line: "Do you want to proceed? (rm -rf build)".
fn question_text(prompt: &PendingPrompt) -> String {
    match prompt.detail.iter().map(|d| d.trim()).find(|d| !d.is_empty()) {
        Some(detail) => format!("{} ({})", prompt.question.trim(), detail.chars().take(160).collect::<String>()),
        None => prompt.question.trim().to_string(),
    }
}

/// What the manager is told about a pull request, from the app's own view of it: `pr` as
/// found for the session, `ci` on its newest commit, `details` when it was read in full.
fn pr_info(repo: String, pr: &PullRequest, ci: Option<&CiStatus>, details: Option<&PrDetails>) -> PrInfo {
    let known = |s: &&str| !s.is_empty();
    let state = ci.map(|c| c.state.as_str()).filter(known).or(details.map(|d| d.state.as_str()).filter(known)).unwrap_or(pr.state.as_str());
    let draft = details.map(|d| d.is_draft).unwrap_or(pr.is_draft);
    let checks = ci.map(|c| &c.checks).or(details.map(|d| &d.checks));
    let has = |state: CheckState| checks.is_some_and(|list| list.iter().any(|c| c.state() == state));
    let ci_word = if has(CheckState::Failed) {
        "failing"
    } else if has(CheckState::Running) {
        "running"
    } else if has(CheckState::Passed) {
        "passing"
    } else {
        ""
    };
    PrInfo {
        repo,
        number: pr.number,
        title: details.map(|d| d.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| pr.title.clone()),
        url: pr.url.clone(),
        state: pr_state(state, draft).into(),
        ci: ci_word.into(),
        conflicting: ci.map(CiStatus::conflicting).unwrap_or_else(|| details.is_some_and(|d| d.mergeable == "CONFLICTING")),
        unresolved_threads: details.map(|d| d.threads.iter().filter(|t| !t.resolved).count() as u32).unwrap_or(0),
        // People only: a bot's review is not a teammate's.
        reviewers: details
            .map(|d| d.reviewers.iter().filter(|r| !r.is_bot).map(|r| ReviewerInfo { login: r.login.clone(), state: if r.state == "REQUESTED" { "PENDING".into() } else { r.state.clone() } }).collect())
            .unwrap_or_default(),
        // Known once the pull request was read in full.
        author: details.map(|d| d.author.clone()).unwrap_or_default(),
    }
}

/// The same from the few answers the server keeps about a session's pull request.
fn pr_from_summary(repo: String, pr: &PrSummary) -> PrInfo {
    PrInfo {
        repo,
        number: pr.number,
        title: pr.title.clone(),
        url: pr.url.clone(),
        state: pr_state(&pr.state, pr.draft).into(),
        ci: match pr.ci.as_str() {
            "failed" => "failing",
            "running" => "running",
            "passed" => "passing",
            _ => "",
        }
        .into(),
        conflicting: pr.merge_state == "DIRTY",
        unresolved_threads: pr.unresolved,
        reviewers: Vec::new(),
        author: String::new(),
    }
}

/// "open", "draft", "merged" or "closed", from GitHub's word and whether it is a draft.
fn pr_state(state: &str, draft: bool) -> &'static str {
    match state.to_uppercase().as_str() {
        "MERGED" => "merged",
        "CLOSED" => "closed",
        _ if draft => "draft",
        _ => "open",
    }
}

/// A section of the right panel as the app names it, whatever the manager called it.
fn section_key(word: &str) -> String {
    let word = word.trim().to_lowercase();
    match word.as_str() {
        "check" | "ci" => "checks",
        "review" => "reviews",
        "comment" | "threads" => "comments",
        "change" | "diff" => "changes",
        "file" | "worktree" => "files",
        _ => return word,
    }
    .to_string()
}

/// What the right side of its own page shows for a section named in a `Show`.
fn stage_of(section: &str) -> Option<Stage> {
    match section.trim().to_lowercase().as_str() {
        "overview" | "home" => Some(Stage::Overview),
        "followups" | "follow-ups" | "follow_ups" | "followup" | "follow-up" => Some(Stage::FollowUps),
        "messages" | "message" | "outbox" => Some(Stage::Messages),
        "memory" | "notes" => Some(Stage::Memory),
        _ => None,
    }
}

/// The tab of the right panel a section is on (the last reply is on none).
fn tab_of_section(section: &str) -> Option<PanelTab> {
    match section {
        "checks" | "preview" | "reviews" | "comments" => Some(PanelTab::PullRequest),
        "changes" => Some(PanelTab::Changes),
        "files" => Some(PanelTab::Files),
        _ => None,
    }
}

/// What is typed into a session: one line that says who speaks, and ends the way
/// Glasscode's own messages do (the Chat view shows those as sent by Glasscode).
fn tell_line(name: &str, text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return text;
    }
    format!("[{name}] {text} (Sent by Glasscode because {name} asked it to.)")
}

/// A scheduled check as a question for the brain: it must know nobody asked just now.
fn check_question(what: &str) -> String {
    format!("(A check the person asked you to run on a schedule, not a question from them just now. Say what you found in one or two sentences.) {what}")
}

/// Why there is no answer, as a line for the conversation.
fn cannot_answer(name: &str, why: &str) -> String {
    format!("{name} could not answer. {}", sentence(why))
}

/// The first line of a reason, ending like a sentence.
fn sentence(why: &str) -> String {
    let line = why.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Something went wrong");
    if line.ends_with(['.', '!', '?']) { line.to_string() } else { format!("{line}.") }
}

/// Why the messenger did not do it, in words for the person: what to do about it when they can.
fn messenger_reason(error: &anyhow::Error) -> String {
    match error.downcast_ref::<MessengerError>() {
        Some(MessengerError::NotSet) => "No messenger is set up. Choose one in Settings › Manager.".into(),
        Some(other) => sentence(&other.to_string()),
        None => sentence(&format!("{error:#}")),
    }
}

/// Whom the messenger is told to write to: the teammate's contact from Settings › Manager,
/// else their name there, else the name on the message.
fn recipient(contact: Option<&str>, name: Option<&str>, to: &str) -> String {
    [contact, name].into_iter().flatten().map(str::trim).find(|s| !s.is_empty()).unwrap_or(to.trim()).to_string()
}

fn reply_text(reply: &Reply) -> String {
    match reply {
        Reply::Done { text, .. } => text.clone(),
        Reply::Waiting { summary, .. } => summary.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glasscode_core::github::{CheckRun, ReviewThread, Reviewer};

    fn check(conclusion: &str, status: &str) -> CheckRun {
        CheckRun { name: "build".into(), status: status.into(), conclusion: conclusion.into(), ..Default::default() }
    }

    fn pull() -> PullRequest {
        PullRequest { number: 41, title: "Add the export".into(), url: "https://example.test/pr/41".into(), base_ref_name: "main".into(), head_ref_name: "feat/export".into(), is_draft: false, state: "OPEN".into() }
    }

    #[test]
    fn a_caption_is_short_and_speech_takes_its_time() {
        assert_eq!(say_time("Hi", false), Duration::from_millis(1044));
        assert_eq!(say_time(&"a".repeat(1000), false), Duration::from_secs(6));
        assert!(say_time(&"a".repeat(200), true) > Duration::from_secs(12));
        // A stop lasts as long as its sentence and two seconds more, never a blink.
        assert_eq!(stop_time("Hi", false), Duration::from_secs(5));
        assert_eq!(stop_time(&"a".repeat(200), true), say_time(&"a".repeat(200), true) + Duration::from_secs(2));
        assert_eq!(stop_time(&"a".repeat(5000), false), Duration::from_secs(22));
    }

    #[test]
    fn keeps_the_end_of_a_reply() {
        assert_eq!(tail_of("  Pushed. Shall I open the pull request?  ", 400), "Pushed. Shall I open the pull request?");
        let long = format!("{} Shall I open the pull request?", "word ".repeat(200));
        let tail = tail_of(&long, 60);
        assert!(tail.starts_with('…') && tail.ends_with("Shall I open the pull request?"));
        assert!(tail.chars().count() <= 61);
        // Cut at a word, not in the middle of one.
        assert!(tail.starts_with("…word") || tail.starts_with("…Shall"));
        // Characters, not bytes.
        assert_eq!(tail_of(&"ä".repeat(50), 10).chars().count(), 11);
    }

    #[test]
    fn states_and_sections_get_the_manager_s_words() {
        assert_eq!(state_of("needs you"), SessionState::NeedsYou);
        assert_eq!(state_of("working"), SessionState::Working);
        assert_eq!(state_of("idle"), SessionState::Idle);
        assert_eq!(state_of("not running"), SessionState::NotRunning);
        assert_eq!(section_key(" Checks "), "checks");
        assert_eq!(section_key("diff"), "changes");
        assert_eq!(tab_of_section("reviews"), Some(PanelTab::PullRequest));
        assert_eq!(tab_of_section("changes"), Some(PanelTab::Changes));
        assert_eq!(tab_of_section("files"), Some(PanelTab::Files));
        assert_eq!(tab_of_section("reply"), None);
        assert_eq!(stage_of("followups"), Some(Stage::FollowUps));
        assert_eq!(stage_of("Messages"), Some(Stage::Messages));
        assert_eq!(stage_of("memory"), Some(Stage::Memory));
        assert_eq!(stage_of("checks"), None);
    }

    #[test]
    fn a_pull_request_as_the_manager_sees_it() {
        let ci = CiStatus { head: "abc".into(), state: "OPEN".into(), checks: vec![check("SUCCESS", "COMPLETED"), check("FAILURE", "COMPLETED")], mergeable: "CONFLICTING".into(), ..Default::default() };
        let details = PrDetails {
            number: 41,
            state: "OPEN".into(),
            author: "alex".into(),
            threads: vec![ReviewThread { resolved: false, ..Default::default() }, ReviewThread { resolved: true, ..Default::default() }],
            reviewers: vec![
                Reviewer { login: "sam".into(), state: "CHANGES_REQUESTED".into(), ..Default::default() },
                Reviewer { login: "mira".into(), state: "REQUESTED".into(), ..Default::default() },
                Reviewer { login: "review-bot".into(), state: "COMMENTED".into(), is_bot: true, ..Default::default() },
            ],
            ..Default::default()
        };
        let info = pr_info("acme/app".into(), &pull(), Some(&ci), Some(&details));
        assert_eq!((info.repo.as_str(), info.number, info.state.as_str(), info.ci.as_str()), ("acme/app", 41, "open", "failing"));
        assert!(info.conflicting);
        assert_eq!(info.unresolved_threads, 1);
        assert_eq!(info.author, "alex");
        assert_eq!(info.reviewers, vec![ReviewerInfo { login: "sam".into(), state: "CHANGES_REQUESTED".into() }, ReviewerInfo { login: "mira".into(), state: "PENDING".into() }]);

        // Only what the app found for the session: no reviewers yet, CI unknown.
        let bare = pr_info("acme/app".into(), &PullRequest { is_draft: true, ..pull() }, None, None);
        assert_eq!((bare.state.as_str(), bare.ci.as_str(), bare.conflicting, bare.reviewers.len()), ("draft", "", false, 0));
        let running = CiStatus { state: "MERGED".into(), checks: vec![check("", "IN_PROGRESS"), check("SUCCESS", "COMPLETED")], ..Default::default() };
        let merged = pr_info("acme/app".into(), &pull(), Some(&running), None);
        assert_eq!((merged.state.as_str(), merged.ci.as_str()), ("merged", "running"));

        let summary = PrSummary { number: 7, state: "OPEN".into(), draft: true, ci: "passed".into(), merge_state: "DIRTY".into(), unresolved: 2, ..Default::default() };
        let info = pr_from_summary("acme/app".into(), &summary);
        assert_eq!((info.state.as_str(), info.ci.as_str(), info.conflicting, info.unresolved_threads), ("draft", "passing", true, 2));
    }

    #[test]
    fn a_message_for_a_session_says_who_speaks() {
        assert_eq!(tell_line("AI Alex", "Sam reviewed your pull request:\n  two comments."), "[AI Alex] Sam reviewed your pull request: two comments. (Sent by Glasscode because AI Alex asked it to.)");
        assert_eq!(tell_line("AI Alex", "  \n "), "");
    }

    #[test]
    fn a_check_tells_the_brain_nobody_asked_just_now() {
        let question = check_question("see whether #41 was merged");
        assert!(question.starts_with("(A check the person asked you to run on a schedule") && question.ends_with(") see whether #41 was merged"));
    }

    #[test]
    fn pull_requests_take_turns() {
        // 30 open ones, 12 at a time: every one is read within three turns.
        let mut seen = std::collections::HashSet::new();
        let mut from = 0;
        for _ in 0..3 {
            let (start, count) = turn(30, from, 12);
            seen.extend((0..count).map(|k| (start + k) % 30));
            from = start + count;
        }
        assert_eq!(seen.len(), 30);
        // Fewer than a turn takes: each once, not twice.
        assert_eq!(turn(5, 7, 12), (2, 5));
        assert_eq!(turn(0, 3, 12), (0, 0));
    }

    #[test]
    fn reasons_read_as_sentences() {
        assert_eq!(cannot_answer("AI Alex", "claude is not signed in\nmore detail"), "AI Alex could not answer. claude is not signed in.");
        assert_eq!(sentence("No messenger is set up"), "No messenger is set up.");
        assert_eq!(messenger_reason(&anyhow::Error::new(MessengerError::NotSet)), "No messenger is set up. Choose one in Settings › Manager.");
        assert_eq!(messenger_reason(&anyhow::anyhow!("the program stopped\nwith more")), "the program stopped.");
        assert_eq!(sentence("  "), "Something went wrong.");
    }

    #[test]
    fn a_message_goes_to_the_contact_else_the_name() {
        assert_eq!(recipient(Some("sam@example.test"), Some("Sam Example"), "sam"), "sam@example.test");
        assert_eq!(recipient(Some("  "), Some("Sam Example"), "sam"), "Sam Example");
        assert_eq!(recipient(None, Some("Sam Example"), "sam"), "Sam Example");
        // Nobody by that name in Settings: the name on the message.
        assert_eq!(recipient(None, None, " Sam "), "Sam");
    }

    #[test]
    fn a_question_on_screen_in_one_line() {
        let prompt = PendingPrompt { title: None, question: "Do you want to proceed?".into(), detail: vec![" ".into(), "rm -rf build".into()], options: Vec::new(), selected: 0 };
        assert_eq!(question_text(&prompt), "Do you want to proceed? (rm -rf build)");
        assert_eq!(question_text(&PendingPrompt { detail: Vec::new(), ..prompt }), "Do you want to proceed?");
    }
}
