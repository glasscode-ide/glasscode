//! v2's own round of the manager (M56.2): what v1's `manager_tick.rs` round did for a v2 home,
//! on v2's owners only, so v1's round can go. Every few seconds, with the engine at v2, the manager
//! on and this Mac home:
//!
//! - the take-over (`ci_fix::manager_take_over`: the native built-ins' state);
//! - the review feed (`glasscode_core::manager::review_feed`, its own store): the reviews of the
//!   person's pull requests as `review.received` events, which the native review-tell reads;
//! - the native review-tell's tells, to the sessions that are free now;
//! - the outbox (`manager/outbox.json` alone, v2's [`Round::outbox`]): what the person allowed is
//!   sent, each version once (`messages::send_claimed`);
//! - the inbox (`manager/inbox.json`): the messenger's new messages, an urgent one from an allowed
//!   teammate notifying at once with a holding reply in the outbox (`manager::inbox`);
//! - the daily brief (`manager/brief.json`): at the time set in Settings, once a day, from v2's own
//!   inputs ([`Workspace::v2_brief_data`]), said in the conversation.
//!
//! It has v2's own view of what the manager sees ([`Workspace::v2_snapshot`], from what the app has
//! at hand) and its own way to carry out what the built-ins say ([`Workspace::v2_apply`]).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use glasscode_core::agents::AgentActivity;
use glasscode_core::control::{Kind, Rule};
use glasscode_core::github::{CheckState, CiStatus, PrDetails, PrSummary, PullRequest};
use glasscode_core::manager::{Action, PrInfo, ReviewAsk, ReviewerInfo, SessionInfo, SessionState, Snapshot};
use glasscode_core::model::AgentKind;
use glasscode_core::prompt::PendingPrompt;
use gpui::{AppContext as _, Context};

use crate::workspace::Workspace;

/// How often v2's round runs.
const EVERY: Duration = Duration::from_secs(10);

/// How often the messenger is asked for new messages.
const INBOX_EVERY: Duration = Duration::from_secs(120);

/// The end of a reply the manager sees.
const REPLY_CHARS: usize = 400;

/// How long after a failed look-up the person's GitHub login is asked for again.
const ME_AGAIN: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
pub struct Round {
    at: Option<Instant>,
    /// The review feed's store, read at the first round (`manager/reviews.json`).
    seen: Option<glasscode_core::manager::review_feed::ReviewSeen>,
    /// The first round after a start only records.
    first_done: bool,
    /// The end of each session's last reply or the question it stopped on, from its turn reports.
    pub replies: HashMap<String, String>,
    /// v2's outbox (`manager/outbox.json`), read at the take-over; None: not read yet, or it
    /// could not be (then nothing is sent).
    pub outbox: Option<glasscode_core::manager::messages::Outbox>,
    /// Messages being sent now.
    sending: HashSet<String>,
    /// v2's inbox (`manager/inbox.json`), and when the messenger was last asked.
    inbox: Option<glasscode_core::manager::inbox::InboxFile>,
    inbox_at: Option<Instant>,
    inbox_busy: bool,
    /// v2's last daily brief and the sessions that finished (`manager/brief.json`), read once.
    brief: Option<glasscode_core::manager::brief::BriefFile>,
    /// The person's GitHub login is being looked up, and when it last was.
    me_busy: bool,
    me_at: Option<Instant>,
    /// The file was read again after the take-over (which waits for v1's data to move, v1's last
    /// brief with it): what a turn report loaded before that is joined with it once.
    brief_fresh: bool,
}

impl Workspace {
    /// From the workspace's tick.
    pub(crate) fn v2_round_tick(&mut self, cx: &mut Context<Self>) {
        // The person's login, on every Mac whose manager is on: the review feed only records
        // until it is known, and the server's follow-ups get it from here (`ManagerFromMac`).
        if self.control.engine_v2 && self.state.manager.enabled {
            self.v2_look_up_me(cx);
        }
        if !self.control.engine_v2 || !self.state.manager.enabled || !self.acts_as_home() || self.round.at.is_some_and(|at| at.elapsed() < EVERY) {
            return;
        }
        self.round.at = Some(Instant::now());
        // Questions held until the brain's limit reset are asked again (ux-15).
        self.chat_held_tick(cx);
        self.manager_take_over(cx);
        let snap = self.v2_snapshot();
        // The review feed, on its own store.
        let seen = self.round.seen.get_or_insert_with(|| {
            use glasscode_core::store::Store as _;
            glasscode_core::manager::review_feed::ReviewSeen::load().unwrap_or_default()
        });
        let before = seen.clone();
        let events = glasscode_core::manager::review_feed::changes(seen, !self.round.first_done, &snap, glasscode_core::model::now());
        self.round.first_done = true;
        if *seen != before {
            let kept = seen.clone();
            std::thread::spawn(move || {
                use glasscode_core::store::Store as _;
                if let Err(err) = kept.save() {
                    log::warn!("manager: what the review feed saw could not be kept ({err})");
                }
            });
        }
        for event in events {
            let project = event.session.as_deref().and_then(|id| self.journal_key(id));
            glasscode_core::journal::append(event.project(project.as_ref()));
        }
        // The reviews whose sessions are free now.
        let tells = self.native_tell_ready(&snap);
        self.v2_apply(tells, cx);
        if self.ci_fix.natives.is_some() {
            self.v2_send(cx);
            self.v2_inbox(cx);
            self.v2_daily_brief(&snap, cx);
        }
    }

    /// What the brief reads, from v2's own stores: the outbox, the reviews that wait to be told
    /// (the native review-tell), when sessions finished (`manager/brief.json`, from their turn
    /// reports), and the follow-ups (the work ledger's, [`Workspace::follow_ups`]).
    pub(crate) fn v2_brief_data(&self) -> glasscode_core::manager::brief::BriefData {
        glasscode_core::manager::brief::BriefData {
            followups: self.follow_ups().to_vec(),
            outbox: self.round.outbox.as_ref().map(|o| o.messages.clone()).unwrap_or_default(),
            tells: self.ci_fix.natives.as_ref().map(|n| n.pending_tells()).unwrap_or_default(),
            finished: self.round.brief.as_ref().map(|b| b.finished.clone()).unwrap_or_default(),
            heartbeat: glasscode_core::model::now(),
        }
    }

    /// v2's brief file, read the first time it is needed.
    fn v2_brief_file(&mut self) -> &mut glasscode_core::manager::brief::BriefFile {
        self.round.brief.get_or_insert_with(|| {
            use glasscode_core::store::Store as _;
            glasscode_core::manager::brief::BriefFile::load().unwrap_or_default()
        })
    }

    /// Kept by joining what is on disk (the migration writes v1's last brief there too): the later
    /// brief and each session's later finish win, never an older copy.
    fn v2_keep_brief_file(&self) {
        let Some(kept) = self.round.brief.clone() else { return };
        std::thread::spawn(move || {
            if let Err(err) = glasscode_core::store::update::<glasscode_core::manager::brief::BriefFile, _>(|file| file.join(&kept)) {
                log::warn!("manager: the brief could not be kept ({err})");
            }
        });
    }

    /// The daily brief built-in (v2): at the time set in Settings, once a day.
    fn v2_daily_brief(&mut self, snap: &Snapshot, cx: &mut Context<Self>) {
        use glasscode_core::manager::handover::{self, Owner, Rule};
        if handover::owner(Rule::Brief, self.manager_mode(), &|id| self.builtin_on(id)) != Owner::V2 {
            return;
        }
        if !self.round.brief_fresh {
            use glasscode_core::store::Store as _;
            let disk = glasscode_core::manager::brief::BriefFile::load().unwrap_or_default();
            self.v2_brief_file().join(&disk);
            self.round.brief_fresh = true;
        }
        let now = glasscode_core::model::now();
        let last = self.v2_brief_file().last.clone();
        let today = glasscode_core::day::iso(glasscode_core::day::local(now as i64));
        let minutes = glasscode_core::manager::brief::parse_clock(&glasscode_core::day::clock(now as i64)).unwrap_or(0);
        if !glasscode_core::manager::brief::due_after(&self.state.manager, last.as_ref(), &today, minutes) {
            return;
        }
        // As v1's `daily_brief`: said (and spoken), nothing when there is nothing to say; made once.
        let made = glasscode_core::manager::brief::build_from(&self.state.manager, &self.v2_brief_data(), snap, now);
        let text = made.text.trim().to_string();
        self.v2_brief_file().last = Some(made);
        self.v2_keep_brief_file();
        if !text.is_empty() {
            self.chat_say_from_code(text, true, cx);
        }
    }

    /// Look up the person's GitHub login (`gh api user`) off the main thread, once it is needed and
    /// not known; again after [`ME_AGAIN`] when it could not be read.
    fn v2_look_up_me(&mut self, cx: &mut Context<Self>) {
        if self.manager.me.is_some() || self.round.me_busy || self.round.me_at.is_some_and(|at| at.elapsed() < ME_AGAIN) {
            return;
        }
        self.round.me_busy = true;
        self.round.me_at = Some(Instant::now());
        let env = (*self.env).clone();
        let task = cx.background_spawn(async move { glasscode_core::manager::gh::my_login(&env).map_err(|e| format!("{e:#}")) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |ws, _| {
                ws.round.me_busy = false;
                match found {
                    Ok(me) => {
                        ws.manager.me.get_or_insert(me);
                    }
                    Err(why) => log::info!("manager: the person's GitHub login is not known yet ({why})"),
                }
            });
        })
        .detach();
    }

    /// v2's outbox changed: kept, off the main thread. `data.json`'s outbox stays v1's as v1 left
    /// it (nothing at v2 sends from it, `Owned::for_harness`).
    pub(crate) fn v2_outbox_changed(&mut self) {
        let Some(outbox) = self.round.outbox.clone() else { return };
        std::thread::spawn(move || {
            use glasscode_core::store::Store as _;
            if let Err(err) = outbox.save() {
                log::warn!("manager: the outbox could not be kept ({err})");
            }
        });
    }

    /// The person allowed or rejected a message (v2): in v2's outbox; an allowed one goes with the
    /// next round.
    pub(crate) fn v2_decide_message(&mut self, id: &str, allow: bool, edited: Option<String>) -> Option<glasscode_core::manager::OutMessage> {
        let decided = self.round.outbox.as_mut()?.decide(id, allow, edited.as_deref(), glasscode_core::model::now());
        self.v2_outbox_changed();
        decided
    }

    /// A message to a teammate into v2's outbox (it waits for the person, or goes by itself when
    /// it is a routine ping the person's send setting allows).
    pub(crate) fn v2_draft_message(&mut self, to: &str, body: &str, why: &str, about: Option<&str>) -> Option<glasscode_core::manager::OutMessage> {
        let settings = self.state.manager.clone();
        let drafted = self.round.outbox.as_mut()?.draft(&settings, to, body, why, about, glasscode_core::model::now());
        self.v2_outbox_changed();
        Some(drafted)
    }

    /// The messages to teammates where the engine keeps them: v2's outbox (empty until it is read),
    /// v1's in `data.json` otherwise.
    pub(crate) fn manager_outbox(&self) -> &[glasscode_core::manager::OutMessage] {
        if !self.control.engine_v2 {
            return &self.manager.harness.data.outbox;
        }
        self.round.outbox.as_ref().map(|o| o.messages.as_slice()).unwrap_or_default()
    }

    /// The teammates' messages where the engine keeps them: v2's inbox (`manager/inbox.json`), v1's
    /// in `data.json` otherwise.
    pub(crate) fn manager_inbox(&self) -> &[glasscode_core::manager::InMessage] {
        if !self.control.engine_v2 {
            return &self.manager.harness.data.inbox;
        }
        self.round.inbox.as_ref().map(|i| i.messages.as_slice()).unwrap_or_default()
    }

    /// Send what the person allowed, each version once, through the messenger.
    fn v2_send(&mut self, cx: &mut Context<Self>) {
        let settings = self.state.manager.clone();
        let Some(outbox) = self.round.outbox.as_ref() else { return };
        let ready: Vec<glasscode_core::manager::OutMessage> = outbox.ready(&settings).into_iter().filter(|id| !self.round.sending.contains(id)).filter_map(|id| outbox.get(&id).cloned()).collect();
        for message in ready {
            self.round.sending.insert(message.id.clone());
            let person = settings.person(&message.to);
            let to = [person.and_then(|p| p.contact.as_deref()), person.map(|p| p.name.as_str())].into_iter().flatten().map(str::trim).find(|s| !s.is_empty()).unwrap_or(message.to.trim()).to_string();
            let intent = glasscode_core::manager::messages::send_intent(&message, &self.control.launch);
            let generation = self.home_generation();
            let (settings, env) = (settings.clone(), (*self.env).clone());
            let task = cx.background_spawn(async move {
                glasscode_core::manager::messages::send_claimed(&intent, generation, || {
                    glasscode_core::manager::messenger::send(&settings, &to, &message.body, message.urgent, &env).map_err(|err| match err.downcast_ref::<glasscode_core::manager::messenger::MessengerError>() {
                        Some(glasscode_core::manager::messenger::MessengerError::NotSet) => "No messenger is set up. Choose one in Settings › Manager.".to_string(),
                        _ => format!("{err:#}"),
                    })
                })
                .map(|()| message.clone())
                .map_err(|why| (message, why))
            });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |ws, cx| {
                    let now = glasscode_core::model::now();
                    let (id, to, outcome) = match &result {
                        Ok(m) => (m.id.clone(), m.to.clone(), Ok(())),
                        Err((m, why)) => (m.id.clone(), m.to.clone(), Err(why.clone())),
                    };
                    ws.round.sending.remove(&id);
                    ws.journal_message(&id, &to, &outcome);
                    if let Some(outbox) = ws.round.outbox.as_mut() {
                        match &outcome {
                            Ok(()) => outbox.mark_sent(&id, now),
                            Err(why) => outbox.mark_failed(&id, why, now),
                        }
                    }
                    ws.v2_outbox_changed();
                    ws.chat_post_line(true, match &outcome {
                        Ok(()) => format!("Sent the message to {to}."),
                        Err(why) => format!("The message to {to} was not sent. {why}"),
                    });
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Ask the messenger for new messages, now and then: an urgent one from a teammate the person
    /// allows to alarm them notifies at once (passing quiet hours) and holds a reply in the outbox.
    fn v2_inbox(&mut self, cx: &mut Context<Self>) {
        if self.round.inbox_busy || self.state.manager.messenger.trim().is_empty() || self.round.inbox_at.is_some_and(|at| at.elapsed() < INBOX_EVERY) {
            return;
        }
        self.round.inbox_at = Some(Instant::now());
        self.round.inbox_busy = true;
        let inbox = self.round.inbox.get_or_insert_with(|| {
            use glasscode_core::store::Store as _;
            glasscode_core::manager::inbox::InboxFile::load().unwrap_or_default()
        });
        let since = inbox.since(glasscode_core::model::now());
        let (settings, env) = (self.state.manager.clone(), (*self.env).clone());
        let task = cx.background_spawn(async move {
            if !glasscode_core::manager::messenger::available(&settings, &env) {
                return Ok(Vec::new());
            }
            glasscode_core::manager::messenger::incoming(&settings, since, &env).map_err(|err| format!("{err:#}"))
        });
        cx.spawn(async move |this, cx| {
            let read = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.round.inbox_busy = false;
                let messages = match read {
                    Ok(messages) => messages,
                    Err(why) => return log::warn!("manager: could not read new messages: {why}"),
                };
                if messages.is_empty() {
                    return;
                }
                let settings = ws.state.manager.clone();
                let Some(inbox) = ws.round.inbox.as_mut() else { return };
                let alarms = inbox.take(&settings, messages);
                let kept = inbox.clone();
                std::thread::spawn(move || {
                    use glasscode_core::store::Store as _;
                    if let Err(err) = kept.save() {
                        log::warn!("manager: the inbox could not be kept ({err})");
                    }
                });
                for alarm in alarms {
                    ws.notify_manager_urgent(alarm.title, alarm.body, cx);
                    // The holding reply goes through the outbox like every message.
                    if let Some(outbox) = ws.round.outbox.as_mut() {
                        outbox.hold(&settings, &alarm.to, Some(&alarm.about), glasscode_core::model::now());
                    }
                }
                ws.v2_outbox_changed();
                cx.notify();
            });
        })
        .detach();
    }

    /// Carry out what the built-ins say (v2): a notification, a message to a session through its
    /// delivery queue, a line said in the conversation.
    pub(crate) fn v2_apply(&mut self, actions: Vec<Action>, cx: &mut Context<Self>) {
        if !self.state.manager.enabled {
            return;
        }
        for action in actions {
            match action {
                Action::Notify { title, body, session } => {
                    // About no session (or one that is gone): the manager's own notice.
                    let Some((id, agent)) = session.as_deref().and_then(|id| self.state.session(id)).map(|s| (s.id.clone(), s.agent)) else {
                        self.notify_manager(title, body, cx);
                        continue;
                    };
                    let watching = self.window_active && self.view == crate::shell::View::Sessions && self.center == crate::center::Center::Session && self.state.active_session.as_deref() == Some(id.as_str());
                    if !watching {
                        self.notify_session(&id, agent, title, body, true, cx);
                    }
                }
                Action::TellSession { session, text } => self.v2_tell(session, &text, cx),
                // Said and spoken, as v1's `mtick_say` did at v2.
                Action::Say { text, .. } if !text.trim().is_empty() => self.chat_say_from_code(text.trim().to_string(), true, cx),
                Action::Say { .. } => {}
                // Where it takes the person: the same walks and pictures as the page's.
                Action::Show { show } => self.with_main_window(cx, move |ws, window, cx| ws.manager_show(&show, window, cx)),
                Action::Tour { stops } => self.manager_start_tour(stops, cx),
                Action::Scene { scene } => self.manager_play(scene, cx),
                // A draft waits in v2's outbox for the person's OK (or goes by its tier).
                Action::Draft { to, body, why, about } => {
                    if self.v2_draft_message(&to, &body, &why, about.as_deref()).is_none() {
                        log::warn!("manager: the outbox is not read yet; the draft to {to} was dropped");
                    }
                }
                // A fact is proposed to memory, as `manager remember` does (D1: it waits for the
                // person's tick).
                Action::Remember { text } => {
                    let text = text.trim().to_string();
                    if !text.is_empty() {
                        cx.background_spawn(async move {
                            use glasscode_core::memstore::{MemFile, Memory, Source, Who};
                            if let Err(err) = Memory::open().propose(&Who::Manager, &MemFile::Profile, None, &text, Source::Session("manager".into())) {
                                log::warn!("manager: the fact was not proposed to memory ({err})");
                            }
                        })
                        .detach();
                    }
                }
                // v1's: its brain started sessions and its harness asked for sends; at v2 sessions
                // start through `session.new` and the round sends (`v2_send`).
                other @ (Action::StartSession { .. } | Action::Send { .. }) => log::info!("manager: v2 has no use for {other:?}"),
            }
        }
    }

    /// A message of the manager's to a session, through its delivery queue: typed when the session
    /// is idle, once (the same message to the same session on the same day is one effect).
    fn v2_tell(&mut self, session: String, text: &str, cx: &mut Context<Self>) {
        let name = self.state.manager.name.clone();
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            return;
        }
        let line = format!("[{name}] {text} (Sent by Glasscode because {name} asked it to.)");
        let agent = self.state.session(&session).map(|s| s.agent).or_else(|| self.server.session(&session).map(|r| r.session.agent));
        if agent.is_none_or(|agent| agent == AgentKind::Shell) {
            return;
        }
        if self.state.agents.rule(Kind::Send) == Rule::Never {
            self.chat_post_line(true, "A session was not told: Settings › Automation says agents may never send a message to another session.".to_string());
            return;
        }
        let now = glasscode_core::clock::now_ms();
        let subject = format!("{}:{}", now / 86_400_000, glasscode_core::effects::short_hash(&line));
        // One step from what the manager saw (events-13).
        let delivery = glasscode_core::delivery_session::Delivery { depth: 1, ..glasscode_core::delivery_session::Delivery::new(&session, &line, glasscode_core::words::InjectKind::SessionSend, now) };
        let intent = glasscode_core::effects::Intent {
            origin: "manager".into(),
            subject: subject.clone(),
            target: session.clone(),
            caller: name,
            reason: glasscode_core::words::scrub(&line.chars().take(80).collect::<String>()),
            session: Some(session.clone()),
            launch: self.control.launch.clone(),
            ..glasscode_core::effects::Intent::new(glasscode_core::effects::effect_key("manager", &subject, "session.send", &session), "session.send", glasscode_core::ops::Retry::Verifiable)
        };
        self.queue_delivery(delivery, Some(intent), cx);
    }

    /// What the manager sees right now (managed projects only), v2's own: built from what the app
    /// has at hand; it reads no file and calls nothing.
    pub(crate) fn v2_snapshot(&self) -> Snapshot {
        let manages = |project_id: &str| self.state.manager.manages(project_id) && !self.state.is_hidden(project_id);
        let mut sessions = Vec::new();
        let (mut ours, mut others): (HashSet<&str>, HashSet<&str>) = Default::default();
        for s in &self.state.sessions {
            let repo = self.prs.map.get(&s.id).and_then(|p| p.repo.as_deref());
            if !manages(&s.project_id) {
                others.extend(repo);
                continue;
            }
            ours.extend(repo);
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
                last_reply: self.round.replies.get(&s.id).cloned(),
                pr: self.v2_pr(s),
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
                let name = server.projects.iter().find(|p| p.id == s.project_id).map(|p| p.name.clone()).unwrap_or_default();
                let mac = self.state.projects.iter().find(|p| p.id == s.project_id || (!name.is_empty() && p.name == name));
                if !manages(mac.map(|p| p.id.as_str()).unwrap_or(&s.project_id)) {
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
        Snapshot { at: glasscode_core::model::now(), me: self.manager.me.clone(), sessions, review_asks, approvals: self.control.approvals.len(), mac_online: true }
    }

    /// A session's pull request, from what the app found for it and what was read of it.
    fn v2_pr(&self, s: &glasscode_core::model::Session) -> Option<PrInfo> {
        let status = self.prs.map.get(&s.id).filter(|s| s.acts())?;
        let repo = status.repo.clone()?;
        let pr = status.pr.as_ref()?;
        let details = self.gh.details.get(&(repo.clone(), pr.number));
        let mut info = pr_info(repo, pr, status.ci.as_ref(), details);
        if info.author.is_empty() {
            info.author = s.review.as_ref().map(|r| r.author.clone()).unwrap_or_default();
        }
        Some(info)
    }

    /// A session's turn ended (its report): the end of what it said, for what the manager sees.
    pub(crate) fn v2_turn_ended(&mut self, report: &glasscode_core::turns::TurnReport) {
        if self.control.engine_v2 && report.end == glasscode_core::turns::TurnEnd::Completed {
            self.v2_brief_file().finished_at(&report.session, report.ended_ms / 1000);
            self.v2_keep_brief_file();
        }
        let said = report.question.clone().filter(|q| !q.trim().is_empty()).unwrap_or_else(|| tail_of(&report.last_reply, REPLY_CHARS));
        if !said.trim().is_empty() {
            self.round.replies.insert(report.session.clone(), said);
        }
    }
}

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

/// The question on an agent's screen, in a line.
fn question_text(prompt: &PendingPrompt) -> String {
    match prompt.detail.iter().map(|d| d.trim()).find(|d| !d.is_empty()) {
        Some(detail) => format!("{} ({})", prompt.question.trim(), detail.chars().take(160).collect::<String>()),
        None => prompt.question.trim().to_string(),
    }
}

fn pr_state(state: &str, draft: bool) -> &'static str {
    match state.to_uppercase().as_str() {
        "MERGED" => "merged",
        "CLOSED" => "closed",
        _ if draft => "draft",
        _ => "open",
    }
}

/// What the manager is told about a pull request, from the app's own view of it.
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
