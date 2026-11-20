//! The manager's rounds in the app: build what it sees, let the rules decide, carry out what
//! they say; take the person through the app and point at things there. Its thinking is the
//! chat and wake lanes' (`manager_chat.rs`, `manager_lanes.rs`).
//!
//! The thinking is `glasscode_core::manager`; this file is what needs the app: its sessions,
//! its terminals, its window. While the manager is off nothing here starts anything; the tick
//! then only takes finished jobs off the channel, so a message that went out is never sent
//! again and the server test in Settings still gets its answer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use gpui::{AppContext as _, Context};
use glasscode_core::agents::AgentActivity;
use glasscode_core::control::{self, Kind, LogEntry, Reply, Rule, code};
use glasscode_core::github::{CheckState, CiStatus, PrDetails, PrSummary, PullRequest};
use glasscode_core::manager::messenger::{self, MessengerError};
use glasscode_core::manager::{Action, FollowKind, MsgState, OutMessage, PrInfo, ReviewAsk, ReviewerInfo, SessionInfo, SessionState, Show, Snapshot, Tier, Who, gh, memory, messages};
use glasscode_core::model::{AgentKind, Session, now};
use glasscode_core::prompt::{self, PendingPrompt};
use glasscode_core::transcript::{self, ItemKind};

use crate::center::Center;
use crate::commands::NewSpec;
use crate::control_ui::Responder;
use crate::manager_view::Done;
use crate::shell::View;
use crate::workspace::Workspace;

/// How often the rules look at everything.
const ROUND: Duration = Duration::from_secs(3);
/// How often the reviews the person gave are read from GitHub.
const REVIEWS_EVERY: Duration = Duration::from_secs(5 * 60);
/// How often the messenger is asked for new messages.
const INBOX_EVERY: Duration = Duration::from_secs(2 * 60);
/// A message for a session that could not be typed for this long is dropped.
const TELL_KEEP: Duration = Duration::from_secs(24 * 3600);
/// At least this long between two messages typed into the same session.
const TELL_GAP: Duration = Duration::from_secs(20);
/// How much of a session's last reply the manager is shown.
const REPLY_CHARS: usize = 400;


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
    /// Messages for sessions that could not be typed yet (working, a draft in the box).
    pub tells: Vec<Tell>,
    /// When each session was last told something.
    pub told_at: HashMap<String, Instant>,
    /// Outbox messages on their way (being looked up or sent), so none goes twice.
    pub sending: HashSet<String>,
    /// The end of each session's last reply, by session id.
    pub replies: HashMap<String, ReplyEnd>,
    pub replies_running: bool,
    pub later: Vec<Later>,
}

// ----- the rounds -------------------------------------------------------------------------------

impl Workspace {
    /// Called from the workspace tick (about every 700 ms). Does nothing while the manager is
    /// off. Reads finished background jobs, does a round every few seconds, moves a walk on.
    pub fn manager_tick(&mut self, cx: &mut Context<Self>) {
        // "Switch now" (M56 step 1): v1's manager stops at once, so that it and v2 never both act;
        // v2 runs from the restart.
        let on = self.state.manager.enabled && self.lifetime.engine_switch.is_none();
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
                    if self.view == View::Manager {
                        self.with_main_window(cx, move |ws, window, cx| ws.manager_show(&show, window, cx));
                    }
                }
            }
        }

        if self.manager.last_round.is_none_or(|t| t.elapsed() >= ROUND) {
            self.manager.last_round = Some(at);
            self.mtick_refresh_replies(cx);
            let snap = self.manager_snapshot();
            // v2 takes over from this harness once it may (`handover`): from then on the
            // harness leaves what v2's built-ins do to them.
            self.manager_take_over(cx);
            let owned = self.manager_owned();
            let round = self.manager.harness.step_with(&self.state.manager, &snap, now(), &owned);
            for event in round.events {
                let project = event.session.as_deref().and_then(|id| self.journal_key(id));
                glasscode_core::journal::append(if event.project.is_none() { event.project(project.as_ref()) } else { event });
            }
            let mut actions = round.actions;
            actions.extend(self.native_tell_ready(&snap));
            changed |= !actions.is_empty();
            // v1's checks are schedules now (`v1-check-<id>`, M44), which run once the person
            // approves them: v1's tick brain, which asked them, is gone (M56).
            self.manager_apply(actions, cx);
            self.mtick_flush_tells(cx);
            changed |= self.manager.harness.dirty;
            self.manager.harness.save();
        }
        // At v2 the reviews the person gave are `followups_ui`'s to read (M56.2, the work ledger):
        // v1's read would only ask GitHub again.
        if !self.control.engine_v2 && !self.manager.reviews_running && self.manager.last_reviews.is_none_or(|t| t.elapsed() >= REVIEWS_EVERY) {
            self.mtick_fetch_reviews(cx);
        }
        // Once v2 has taken over (its natives run), v2's round reads the inbox into its own
        // store (`v2_inbox`, M56.2); v1's read would tell the person a second time.
        let v2_reads_inbox = self.control.engine_v2 && self.ci_fix.natives.is_some();
        if !v2_reads_inbox && !self.manager.inbox_running && self.manager.last_inbox.is_none_or(|t| t.elapsed() >= INBOX_EVERY) {
            self.mtick_fetch_inbox(cx);
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
                Done::Reviews(result) => {
                    let wanted = std::mem::take(&mut self.manager.reviews_running);
                    match result {
                        Ok(reviews) if on && wanted => {
                            let owned = self.manager_owned();
                            let actions = self.manager.harness.on_reviews_with(&self.state.manager, &reviews, now(), &owned);
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
                            // The inbox notifies only for an urgent message from a teammate allowed
                            // to alarm the person: in a v2 home that passes quiet hours (M42).
                            let (urgent, rest): (Vec<Action>, Vec<Action>) = actions.into_iter().partition(|a| self.control.engine_v2 && matches!(a, Action::Notify { session: None, .. }));
                            for alarm in urgent {
                                if let Action::Notify { title, body, .. } = alarm {
                                    self.notify_manager_urgent(title, body, cx);
                                }
                            }
                            self.manager_apply(rest, cx);
                            changed = true;
                        }
                        Ok(_) => {}
                        Err(why) => log::warn!("manager: could not read new messages: {why}"),
                    }
                }
                Done::Sent { id, result } => {
                    self.manager.tick.sending.remove(&id);
                    let to = self.manager.harness.data.outbox.iter().find(|m| m.id == id).map(|m| m.to.clone()).filter(|to| !to.is_empty()).unwrap_or_else(|| "your teammate".into());
                    self.journal_message(&id, &to, &result);
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


    /// Keep the end of each managed session's last reply at hand: the question on its screen
    /// while it needs the person, and its transcript's end, read in the background whenever
    /// the file changed.
    fn mtick_refresh_replies(&mut self, cx: &mut Context<Self>) {
        let mut questions: Vec<(String, Option<String>)> = Vec::new();
        let mut jobs: Vec<(String, AgentKind, PathBuf, (u64, Option<SystemTime>))> = Vec::new();
        for s in &self.state.sessions {
            if s.agent == AgentKind::Shell || !self.manager_manages(&s.project_id) {
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
            if !self.manager_manages(&s.project_id) {
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
                if !self.manager_manages(mac.map(|p| p.id.as_str()).unwrap_or(&s.project_id)) {
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
        let status = self.prs.map.get(&s.id).filter(|s| s.acts())?;
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







    /// A line in what agents did (Today lists it), for what the manager did to a session.
    fn mtick_log(&mut self, what: String) {
        let entry = LogEntry { at: now(), who: self.state.manager.name.clone(), what, outcome: "done".into(), ..Default::default() };
        control::append_log(&entry);
        self.journal_manager_did(&entry);
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
                Action::Scene { scene } => self.manager_play(scene, cx),
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
        // v2 (M31): the session's delivery queue types it when the session is idle, once: the
        // same message to the same session on the same day is one effect.
        if self.control.engine_v2 {
            let agent = self.state.session(&session).map(|s| s.agent).or_else(|| self.server.session(&session).map(|r| r.session.agent));
            if agent.is_none_or(|agent| agent == AgentKind::Shell) {
                return;
            }
            if self.state.agents.rule(Kind::Send) == Rule::Never {
                self.mtick_line(Who::System, "A session was not told: Settings › Automation says agents may never send a message to another session.".to_string(), None);
                return;
            }
            let now = glasscode_core::clock::now_ms();
            let day = now / 86_400_000;
            // The record keeps a hash of the message, not the message.
            let subject = format!("{day}:{}", glasscode_core::effects::short_hash(&line));
            // One step from what the manager saw (events-13).
            let delivery = glasscode_core::delivery_session::Delivery { depth: 1, ..glasscode_core::delivery_session::Delivery::new(&session, &line, glasscode_core::words::InjectKind::SessionSend, now) };
            let intent = glasscode_core::effects::Intent {
                origin: "manager".into(),
                subject: subject.clone(),
                target: session.clone(),
                caller: self.state.manager.name.clone(),
                reason: glasscode_core::words::scrub(&line.chars().take(80).collect::<String>()),
                session: Some(session.clone()),
                launch: self.control.launch.clone(),
                ..glasscode_core::effects::Intent::new(glasscode_core::effects::effect_key("manager", &subject, "session.send", &session), "session.send", glasscode_core::ops::Retry::Verifiable)
            };
            self.queue_delivery(delivery, Some(intent), cx);
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
            return self.type_into_session(id, text, glasscode_core::words::InjectKind::SessionSend, cx);
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
        let watching = self.window_active && self.view == View::Sessions && self.center == Center::Session && self.state.active_session.as_deref() == Some(id.as_str());
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

    /// The outbox message with this id, when it may go out now.
    fn mtick_sendable(&self, id: &str) -> Option<OutMessage> {
        self.manager.harness.data.outbox.iter().find(|m| m.id == id && m.state == MsgState::Approved && m.tier != Tier::Never).cloned()
    }

    /// Send an outbox message the person allowed, in the background, through the messenger
    /// set in Settings › Manager. It goes to the teammate's contact when Settings has one,
    /// else to their name.
    fn mtick_send(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.manager.tick.sending.contains(id) {
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
        // v2's outbox (`handover`): each version of a message is one effect, claimed before the
        // messenger is asked (at most once: it has no probe).
        let claim = self.manager.harness.v1_outbox.is_some().then(|| (glasscode_core::manager::messages::send_intent(&message, &self.control.launch), self.home_generation()));
        cx.background_spawn(async move {
            let result = match claim {
                None => messenger::send(&settings, &to, &message.body, message.urgent, &env).map_err(|e| messenger_reason(&e)),
                Some((intent, generation)) => messages::send_claimed(&intent, generation, || messenger::send(&settings, &to, &message.body, message.urgent, &env).map_err(|e| messenger_reason(&e))),
            };
            let _ = tx.try_send(Done::Sent { id, result });
        })
        .detach();
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
                if !self.manager_manages(&project_id) {
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
                    message_origin: None,
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

}

// ----- taking the person through the app --------------------------------------------------------

impl Workspace {






}

// ----- pointing ---------------------------------------------------------------------------------



// ----- small things -----------------------------------------------------------------------------




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
            .map(|d| d.reviewers.iter().filter(|r| !r.is_bot).map(|r| ReviewerInfo { login: r.login.clone(), state: if r.state == "REQUESTED" { "PENDING".into() } else { r.state.clone() }, association: r.association.clone() }).collect())
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




/// What is typed into a session: one line that says who speaks, and ends the way
/// Glasscode's own messages do (the Chat view shows those as sent by Glasscode).
fn tell_line(name: &str, text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return text;
    }
    format!("[{name}] {text} (Sent by Glasscode because {name} asked it to.)")
}

/// The first line of a reason, ending like a sentence.
pub(crate) fn sentence(why: &str) -> String {
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
        PullRequest { number: 41, title: "Add the export".into(), url: "https://example.test/pr/41".into(), base_ref_name: "main".into(), head_ref_name: "feat/export".into(), is_draft: false, state: "OPEN".into(), ..Default::default() }
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
        assert_eq!(info.reviewers, vec![ReviewerInfo { login: "sam".into(), state: "CHANGES_REQUESTED".into(), ..ReviewerInfo::default() }, ReviewerInfo { login: "mira".into(), state: "PENDING".into(), ..ReviewerInfo::default() }]);

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
    fn reasons_read_as_sentences() {
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
