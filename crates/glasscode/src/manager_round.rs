//! v2's own round of the manager (M56.2): what v1's `manager_tick.rs` round did for a v2 home,
//! on v2's owners only, so v1's round can go. Every few seconds, with the engine at v2, the manager
//! on and this Mac home:
//!
//! - the take-over (`ci_fix::manager_take_over`: the native built-ins' state);
//! - the review feed (`glasscode_core::manager::review_feed`, its own store): the reviews of the
//!   person's pull requests as `review.received` events, which the native review-tell reads;
//! - the native review-tell's tells, to the sessions that are free now.
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
use gpui::Context;

use crate::workspace::Workspace;

/// How often v2's round runs.
const EVERY: Duration = Duration::from_secs(10);

/// The end of a reply the manager sees.
const REPLY_CHARS: usize = 400;

#[derive(Default)]
pub struct Round {
    at: Option<Instant>,
    /// The review feed's store, read at the first round (`manager/reviews.json`).
    seen: Option<glasscode_core::manager::review_feed::ReviewSeen>,
    /// The first round after a start only records.
    first_done: bool,
    /// The end of each session's last reply or the question it stopped on, from its turn reports.
    pub replies: HashMap<String, String>,
}

impl Workspace {
    /// From the workspace's tick.
    pub(crate) fn v2_round_tick(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 || !self.state.manager.enabled || !self.acts_as_home() || self.round.at.is_some_and(|at| at.elapsed() < EVERY) {
            return;
        }
        self.round.at = Some(Instant::now());
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
                Action::Say { text, .. } => self.chat_post_line(false, text),
                other => log::info!("manager: v2's round has no use for {other:?}"),
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
