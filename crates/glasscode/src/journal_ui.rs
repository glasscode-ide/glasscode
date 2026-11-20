//! The app's part of the journal (M14, `glasscode_core::journal`): what the window sees goes in as
//! events, from the 700 ms tick and the places things happen, and the reconciler runs on start,
//! on wake and every 60 s. Appending does no IO (the journal's writer thread does), and what reads
//! the disk (the start, the git files, the server's journal, the ops' queries) runs in the
//! background.
//!
//! With the engine at v1 nothing here runs (`JournalUi::on` stays false): rule 2.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use gpui::{AppContext as _, Context};
use glasscode_core::agents::{self, AgentActivity};
use glasscode_core::control::{Kind, LogEntry, Reply, code};
use glasscode_core::journal::{self, Cursor, Cursors, Event, Filter, ForgeSeen, GitWatch, HookIntake, Scope, Source, kinds};
use glasscode_core::ops::{Action, Op};
use glasscode_core::paths::ProjectKey;
use glasscode_core::reconcile::{self, Reconciler};
use glasscode_core::store::Store as _;
use glasscode_core::turns::{self, Change, Closed, Signal, TurnMachine};
use serde_json::{Value, json};

use crate::control_ui::Caller;
use crate::ops_run::{failed, usage};
use crate::workspace::Workspace;

/// How often the checkouts' git files are looked at.
const GIT_EVERY: Duration = Duration::from_secs(5);
/// How often the spool cursors are kept on disk.
const CURSORS_EVERY: Duration = Duration::from_secs(10);
/// How often the server's journal is pulled.
const SERVER_EVERY: Duration = Duration::from_secs(30);
/// Failed lookups in a row before GitHub counts as degraded for a checkout.
const DEGRADED_AFTER: u32 = 3;

#[derive(Default)]
pub struct JournalUi {
    /// The journal runs here: a v2 home, decided in the background at the start.
    pub on: bool,
    intakes: HashMap<String, HookIntake>,
    cursors: Cursors,
    cursors_dirty: bool,
    cursors_at: Option<Instant>,
    /// What the journal implies; None until its last days were read at the start.
    reconciler: Option<Reconciler>,
    reconciled_at: Option<Instant>,
    clock: journal::ClockWatch,
    git: Arc<Mutex<GitWatch>>,
    git_at: Option<Instant>,
    git_running: bool,
    /// What the last status round said of each session's pull request.
    forge: HashMap<String, ForgeSeen>,
    /// Checkouts whose GitHub lookups keep failing, said once.
    degraded: HashSet<PathBuf>,
    server_at: Option<Instant>,
    server_running: bool,
    /// The journal's write failure the person was told of (events-m4), until it writes again.
    told_failing: bool,
    /// Each session's turns (M30).
    turns: HashMap<String, SessionTurns>,
    /// When each session last reported on itself (`glasscode report`), for the rate limit.
    self_reported: HashMap<String, Instant>,
    /// Conversations started outside Glasscode, and when each was last said (land-9).
    outside: HashMap<String, SystemTime>,
    outside_at: Option<Instant>,
    outside_running: bool,
}

/// One session's turns in the app (M30): its machine, and what it reads besides the hooks.
#[derive(Default)]
struct SessionTurns {
    machine: TurnMachine,
    /// A Codex session's rollout, read for its turns (Codex runs no Glasscode hook: P06).
    rollout: Option<(PathBuf, agents::RolloutTail)>,
    /// A Claude session's transcript, read from this offset for the interrupt marker.
    transcript: Option<(PathBuf, u64)>,
    /// When the transcript last grew, for the watchdog.
    grew_at: Option<Instant>,
}

/// How often a self-report may come from one session.
const SELF_REPORT_EVERY: Duration = Duration::from_secs(30);
/// How often the conversations outside Glasscode are looked for, and how far back.
const OUTSIDE_EVERY: Duration = Duration::from_secs(60);
const OUTSIDE_WINDOW: Duration = Duration::from_secs(10 * 60);
/// A conversation outside Glasscode is said again after it was quiet this long.
const OUTSIDE_AGAIN: Duration = Duration::from_secs(30 * 60);

/// What the start reads off the main thread.
struct Started {
    on: bool,
    cursors: Cursors,
    reconciler: Reconciler,
    /// Turns the journal leaves open: cut when the app that held their sessions went away.
    open_turns: Vec<(String, turns::OpenTurn)>,
}

impl Workspace {
    /// At the start, in the background: whether the journal runs, the spool cursors and the last
    /// two days of the journal (what the reconciler starts from). Then the spools read on from
    /// where they stopped, and the reconciler runs once.
    pub fn journal_start(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move {
            if !journal::init() {
                return Started { on: false, cursors: Cursors::default(), reconciler: Reconciler::default(), open_turns: Vec::new() };
            }
            let since = Filter { since_ms: Some(glasscode_core::clock::now_ms().saturating_sub(2 * 86_400_000)), ..Filter::default() };
            let recent = journal::query(&since, &Scope::All, 50_000);
            let open_turns = turns::open_in(&recent);
            Started { on: true, cursors: Cursors::load().unwrap_or_default(), reconciler: Reconciler::from_events(&recent), open_turns }
        });
        cx.spawn(async move |this, cx| {
            let started = task.await;
            let _ = this.update(cx, |this, cx| {
                if !started.on {
                    return;
                }
                this.journal.on = true;
                this.journal.cursors = started.cursors;
                this.journal.reconciler = Some(started.reconciler);
                let version = env!("CARGO_PKG_VERSION");
                this.journal_note(Event::new(kinds::APP_STARTED, Source::App, format!("Glasscode {version} started")).data(json!({ "version": version, "build": glasscode_core::update::BUILD })));
                let ids: Vec<String> = this.tails.keys().cloned().collect();
                for id in ids {
                    this.journal_tail_started(&id);
                }
                // A turn still open in the journal was cut: its session's agent ran in the app
                // that is gone (a quit closes them as AppQuit, so this one crashed).
                for (session, turn) in started.open_turns {
                    let entry = this.journal.turns.entry(session.clone()).or_default();
                    entry.machine.restore(turn);
                    this.journal_turn_signal(&session, Signal::Killed, None, cx);
                }
            });
        })
        .detach();
    }

    /// Feed one signal to a session's turns: a turn that opens gets its baseline (where the
    /// transcript is, the checkout's head) and a `turn.started`; one that closes gets its report,
    /// read in the background once the transcript settles.
    pub fn journal_turn_signal(&mut self, id: &str, signal: Signal, prompt: Option<String>, cx: &mut Context<Self>) {
        if !self.journal.on {
            return;
        }
        let now = glasscode_core::clock::now_ms();
        let changes = self.journal.turns.entry(id.to_string()).or_default().machine.feed(now, &signal);
        for change in changes {
            match change {
                Change::Opened(turn) => {
                    let session = self.state.session(id);
                    let offset = turns::transcript_len(session.and_then(|s| s.transcript_path.as_ref()));
                    let head = session.and_then(|s| glasscode_core::git_info::heads(&s.worktree)).and_then(|h| h.head);
                    let entry = self.journal.turns.entry(id.to_string()).or_default();
                    entry.machine.set_baseline(&turn.id, offset, head.clone());
                    entry.grew_at = Some(Instant::now());
                    let turn = turns::OpenTurn { transcript_offset: offset, head, ..turn };
                    let event = turns::started_event(id, self.journal_key(id).as_ref(), &turn, prompt.as_deref());
                    self.journal_note(event);
                }
                Change::Closed(closed) => self.journal_turn_closed(id, closed, Some(&mut *cx)),
            }
        }
    }

    /// A closed turn's report. With a context it reads the transcript (once it settles) and git
    /// in the background; without one (the quit) it is written at once from what the turn's end
    /// said, marked partial.
    fn journal_turn_closed(&mut self, id: &str, closed: Closed, cx: Option<&mut Context<Self>>) {
        let key = self.journal_key(id);
        let session = self.state.session(id).cloned();
        let Some(cx) = cx.filter(|_| session.is_some()) else {
            let gathered = turns::Gathered { partial: true, ..turns::Gathered::default() };
            let report = turns::report(id, &closed, &gathered);
            self.journal_note(turns::ended_event(key.as_ref(), &report));
            return;
        };
        let session = session.expect("checked");
        let id = id.to_string();
        let wait = !matches!(closed.end, turns::TurnEnd::AppQuit | turns::TurnEnd::Slept);
        let task = cx.background_spawn(async move {
            let gathered = turns::gather(session.agent, session.transcript_path.as_deref(), &closed, &session.worktree, wait);
            turns::report(&session.id, &closed, &gathered)
        });
        cx.spawn(async move |this, cx| {
            let report = task.await;
            let _ = this.update(cx, |this, _| {
                let key = this.journal_key(&id).or(key);
                this.journal_note(turns::ended_event(key.as_ref(), &report));
            });
        })
        .detach();
    }

    /// The agent of a session ended (its terminal's process): a turn still open aborted, or, when
    /// a signal killed it (no exit status), crashed.
    pub fn journal_turn_exit(&mut self, id: &str, status: Option<i32>, cx: &mut Context<Self>) {
        let signal = match status {
            Some(code) => Signal::Exited(Some(code)),
            None => Signal::Killed,
        };
        self.journal_turn_signal(id, signal, None, cx);
    }

    /// The turns' part of the tick: Codex rollouts, Claude's interrupt marker, the watchdog.
    fn journal_turns_tick(&mut self, cx: &mut Context<Self>) {
        let sessions: Vec<(String, glasscode_core::model::AgentKind, Option<PathBuf>, bool)> = self
            .state
            .sessions
            .iter()
            .map(|s| (s.id.clone(), s.agent, s.transcript_path.clone(), self.terminals.get(&s.id).is_some_and(|t| t.read(cx).exit_code.is_none())))
            .collect();
        for (id, agent, transcript, alive) in sessions {
            let mut signals: Vec<Signal> = Vec::new();
            let entry = self.journal.turns.entry(id.clone()).or_default();
            match (agent, transcript) {
                (glasscode_core::model::AgentKind::Codex, Some(path)) if alive => {
                    if entry.rollout.as_ref().is_none_or(|(p, _)| *p != path) {
                        entry.rollout = Some((path.clone(), agents::RolloutTail::resumed(&path).keeping_events()));
                    }
                    if let Some((_, tail)) = entry.rollout.as_mut() {
                        tail.poll();
                        signals.extend(tail.take_events().into_iter().map(Signal::Rollout));
                    }
                }
                (glasscode_core::model::AgentKind::Claude, Some(path)) if entry.machine.open().is_some() => {
                    let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let from = match &entry.transcript {
                        Some((p, offset)) if *p == path && *offset <= len => *offset,
                        _ => entry.machine.open().and_then(|t| t.transcript_offset).unwrap_or(len),
                    };
                    if len > from {
                        entry.grew_at = Some(Instant::now());
                        let lines = agents::spool_lines_of(&path, from, len);
                        signals.extend(lines.into_iter().filter(|l| l.contains("[Request interrupted by user")).map(Signal::TranscriptLine));
                    }
                    entry.transcript = Some((path, len));
                }
                _ => {}
            }
            if entry.machine.open().is_some() {
                let quiet_ms = entry.grew_at.map(|at| at.elapsed().as_millis() as u64).unwrap_or(u64::MAX);
                signals.push(Signal::Watchdog { alive, quiet_ms });
            }
            for signal in signals {
                self.journal_turn_signal(&id, signal, None, cx);
            }
        }
        let state = &self.state;
        self.journal.turns.retain(|id, _| state.session(id).is_some());
    }

    /// `glasscode report …` from a session: kept as a hint, at most one every 30 s per session.
    pub fn journal_self_report(&mut self, session: &str, state: &str, line: &str, signed: bool) -> Result<(), String> {
        if !self.journal.on {
            return Err("The journal is off here (it runs with the v2 engine).".into());
        }
        if self.journal.self_reported.get(session).is_some_and(|at| at.elapsed() < SELF_REPORT_EVERY) {
            return Err("One report every 30 seconds per session; this one was not kept.".into());
        }
        self.journal.self_reported.insert(session.to_string(), Instant::now());
        let turn = self.journal.turns.get(session).and_then(|t| t.machine.open()).map(|t| t.id.clone());
        let event = turns::self_report_event(session, self.journal_key(session).as_ref(), turn.as_deref(), state, line, signed);
        self.journal_note(event);
        Ok(())
    }

    /// Conversations started outside Glasscode in a known project (land-9): said once, and again
    /// after they were quiet for 30 minutes; never acted on.
    fn journal_outside(&mut self, cx: &mut Context<Self>) {
        self.journal.outside_at = Some(Instant::now());
        self.journal.outside_running = true;
        let accounts = self.state.accounts.clone();
        let mut folders: Vec<PathBuf> = self.state.projects.iter().map(|p| p.root.clone()).collect();
        folders.extend(self.worktrees.values().flatten().map(|w| w.path.clone()));
        folders.sort();
        folders.dedup();
        let known: HashSet<String> = self.state.sessions.iter().filter_map(|s| s.agent_session_id.clone()).collect();
        let since = SystemTime::now() - OUTSIDE_WINDOW;
        let task = cx.background_spawn(async move { glasscode_core::history::active_outside(&accounts, &folders, &known, since) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |this, _| {
                this.journal.outside_running = false;
                for (folder, conversation) in found {
                    let said = this.journal.outside.get(&conversation.id).copied();
                    let quiet = said.is_none_or(|at| conversation.updated.duration_since(at).unwrap_or_default() >= OUTSIDE_AGAIN);
                    this.journal.outside.insert(conversation.id.clone(), conversation.updated);
                    if !quiet {
                        continue;
                    }
                    let project = this.state.projects.iter().filter(|p| folder.starts_with(&p.root)).max_by_key(|p| p.root.as_os_str().len()).and_then(|p| this.control.project_keys.get(&p.id).cloned());
                    this.journal_note(turns::outside_event(project.as_ref(), conversation.agent, &conversation.id, &folder, &conversation.title));
                }
            });
        })
        .detach();
    }

    /// Write an event, and fold it into what the reconciler knows. Nothing with the journal off.
    pub fn journal_note(&mut self, event: Event) {
        if !self.journal.on {
            return;
        }
        if let Some(reconciler) = self.journal.reconciler.as_mut() {
            reconciler.observe(&event);
        }
        journal::append(event);
    }

    /// A session's project key, for its events.
    fn journal_key(&self, session: &str) -> Option<ProjectKey> {
        let project = &self.state.session(session)?.project_id;
        self.control.project_keys.get(project).cloned()
    }

    /// A tail of a session's spool began (a launch, or the journal's start): the lines written
    /// since the journal last read it, while Glasscode was not running, go in as events, and only
    /// there (they change no activity now: the reconciler compares what is true).
    pub fn journal_tail_started(&mut self, id: &str) {
        if !self.journal.on {
            return;
        }
        let Some(from) = self.journal.cursors.spools.get(id).copied() else { return };
        let Some(to) = self.tails.get(id).map(agents::EventsTail::offset) else { return };
        if from < to {
            let lines = agents::spool_lines(id, from, to);
            self.journal_hook_lines(id, &lines, to);
        }
    }

    /// Hook lines a session's tail read, up to `offset`.
    pub fn journal_hook_lines(&mut self, id: &str, lines: &[String], offset: u64) {
        if !self.journal.on {
            return;
        }
        // The lines are the turns' too (`journal_turn_hooks`), read as they come in the tick.
        let key = self.journal_key(id);
        let events = self.journal.intakes.entry(id.to_string()).or_default().events(id, key.as_ref(), lines);
        for event in events {
            self.journal_note(event);
        }
        self.journal.cursors.spools.insert(id.to_string(), offset);
        self.journal.cursors_dirty = true;
    }

    /// The hook lines of a session, as turn signals (the tick's, not the catch-up after a start).
    pub fn journal_turn_hooks(&mut self, id: &str, lines: &[String], cx: &mut Context<Self>) {
        if !self.journal.on {
            return;
        }
        for line in lines {
            let Ok(payload) = serde_json::from_str::<Value>(line) else { continue };
            let prompt = (payload.get("hook_event_name").and_then(Value::as_str) == Some("UserPromptSubmit")).then(|| payload.get("prompt").and_then(Value::as_str).map(str::to_string)).flatten();
            self.journal_turn_signal(id, Signal::Hook(payload), prompt, cx);
        }
    }

    /// A session's activity changed, from whichever source (hooks, a Codex rollout, its screen).
    pub fn journal_activity(&mut self, id: &str, previous: Option<AgentActivity>, now: AgentActivity) {
        if !self.journal.on || previous == Some(now) {
            return;
        }
        let event = journal::activity(id, self.journal_key(id).as_ref(), previous, now);
        self.journal_note(event);
    }

    /// The reply of an op, with its caller (ops-21): every one, reads too; and the log line of
    /// what it did, which `log --json` keeps the shape of.
    pub fn journal_op(&mut self, action: &Action, log: Option<&LogEntry>) {
        if !self.journal.on {
            return;
        }
        let kind = if action.code == code::OK { kinds::OP_DONE } else { kinds::OP_FAILED };
        let summary = log.map(|l| l.what.clone()).unwrap_or_else(|| format!("{} {}", action.op, if action.code == code::OK { "done" } else { "failed" }));
        let project = action.targets.projects.first().cloned().or_else(|| action.session.as_deref().and_then(|s| self.journal_key(s)));
        let data = json!({ "action": action, "log": log });
        self.journal_note(Event::new(kind, Source::Control, summary).session(action.session.as_deref()).project(project.as_ref()).data(data));
    }

    /// What the manager did to a session (v1's rounds), as its log line says.
    pub fn journal_manager_did(&mut self, log: &LogEntry) {
        if !self.journal.on {
            return;
        }
        let project = log.session.as_deref().and_then(|s| self.journal_key(s));
        self.journal_note(Event::new(kinds::OP_DONE, Source::Manager, log.what.clone()).session(log.session.as_deref()).project(project.as_ref()).data(json!({ "log": log })));
    }

    /// A request waits for the person, or they answered it.
    pub fn journal_approval(&mut self, id: u64, caller: &Caller, what: &str, kind_v1: Kind, answer: Option<bool>) {
        if !self.journal.on {
            return;
        }
        let (kind, summary) = match answer {
            None => (kinds::APPROVAL_ASKED, format!("{} asks to {what}", caller.describe())),
            Some(true) => (kinds::APPROVAL_ALLOWED, format!("you allowed {} to {what}", caller.describe())),
            Some(false) => (kinds::APPROVAL_DENIED, format!("you did not let {} {what}", caller.describe())),
        };
        let project = caller.session.as_deref().and_then(|s| self.journal_key(s));
        let data = json!({ "approval": id, "what": what, "kind": kind_v1.key(), "caller": caller.channel, "who": caller.who });
        self.journal_note(Event::new(kind, Source::Control, summary).session(caller.session.as_deref()).project(project.as_ref()).data(data));
    }

    /// A message of the manager's outbox went, or did not.
    pub fn journal_message(&mut self, id: &str, to: &str, result: &Result<(), String>) {
        if !self.journal.on {
            return;
        }
        let event = match result {
            Ok(()) => Event::new(kinds::MESSAGE_SENT, Source::Manager, format!("sent the message to {to}")).data(json!({ "message": id, "to": to })),
            Err(why) => Event::new(kinds::MESSAGE_FAILED, Source::Manager, format!("the message to {to} was not sent: {why}")).data(json!({ "message": id, "to": to, "why": why })),
        };
        self.journal_note(event);
    }

    /// A status round's answer for a session's pull request: what changed since the last one,
    /// with the round's age. The first answer this run is a baseline.
    pub fn journal_forge(&mut self, id: &str, status: &crate::pr_status::PrStatus, age: Duration) {
        if !self.journal.on || !status.confirmed || status.error.is_some() {
            return;
        }
        let now = ForgeSeen {
            repo: status.repo.clone(),
            number: status.pr.as_ref().map(|pr| pr.number),
            title: status.pr.as_ref().map(|pr| pr.title.clone()).unwrap_or_default(),
            state: status.ci.as_ref().map(|ci| ci.state.clone()).or_else(|| status.pr.as_ref().map(|pr| pr.state.clone())).unwrap_or_default().to_uppercase(),
            draft: status.pr.as_ref().is_some_and(|pr| pr.is_draft),
            ci: status.ci.as_ref().and_then(|ci| Some((ci.head.clone(), journal::ci_overall(ci.checks.iter().map(|c| c.state()))?.to_string()))),
            failed: status.ci.as_ref().map(|ci| ci.failed().iter().map(|c| c.label()).collect()).unwrap_or_default(),
            mergeable: status.ci.as_ref().map(|ci| ci.mergeable.clone()).unwrap_or_default(),
        };
        let key = self.journal_key(id);
        let before = self.journal.forge.insert(id.to_string(), now.clone());
        for event in journal::forge_events(id, key.as_ref(), before.as_ref(), &now, age.as_millis() as u64) {
            self.journal_note(event);
        }
    }

    /// A pull request no longer open on its session merged (found by the merge check).
    pub fn journal_merged(&mut self, id: &str, repo: &str, number: u64) {
        if !self.journal.on {
            return;
        }
        let event = journal::pr_merged(id, self.journal_key(id).as_ref(), repo, number);
        self.journal_note(event);
    }

    /// A checkout's GitHub lookups failed again (`fails` in a row), or answered (`fails` 0): said
    /// once when they keep failing, and once when they work again. A failure is never a change of
    /// the pull request (events-18).
    pub fn journal_forge_health(&mut self, worktree: &std::path::Path, fails: u32) {
        if !self.journal.on {
            return;
        }
        let place = worktree.display().to_string();
        if fails >= DEGRADED_AFTER && self.journal.degraded.insert(worktree.to_path_buf()) {
            self.journal_note(journal::source_degraded("github", &format!("{fails} lookups in a row failed for {place}")).data(json!({ "source": "github", "worktree": place, "fails": fails })));
        } else if fails == 0 && self.journal.degraded.remove(worktree) {
            self.journal_note(journal::source_recovered("github").data(json!({ "source": "github", "worktree": place })));
        }
    }

    /// The quit: the last event, the spool cursors, and every event on disk before the process
    /// ends.
    pub fn journal_quit(&mut self) {
        if !self.journal.on {
            return;
        }
        let running: Vec<String> = self.terminals.keys().cloned().collect();
        // Each open turn is cut by the quit, and reported once as that (no waiting now).
        let now = glasscode_core::clock::now_ms();
        let cut: Vec<(String, Closed)> = self
            .journal
            .turns
            .iter_mut()
            .flat_map(|(id, turns)| turns.machine.feed(now, &Signal::AppQuit).into_iter().filter_map(move |c| if let Change::Closed(closed) = c { Some((id.clone(), closed)) } else { None }))
            .collect();
        for (id, closed) in cut {
            self.journal_turn_closed(&id, closed, None);
        }
        self.journal_note(Event::new(kinds::APP_QUIT, Source::App, "Glasscode quit").data(json!({ "running": running })));
        if let Err(err) = self.journal.cursors.save() {
            log::warn!("journal: could not keep the spool cursors: {err}");
        }
        journal::flush();
    }

    /// The journal's part of the 700 ms tick.
    pub fn journal_tick(&mut self, cx: &mut Context<Self>) {
        if !self.journal.on {
            return;
        }
        let clocks = self.journal.clock.observe(glasscode_core::clock::now_ms(), Instant::now());
        let woke = clocks.iter().any(|e| e.kind == kinds::APP_WOKE);
        let slept = clocks.iter().any(|e| e.kind == kinds::APP_SLEPT);
        for event in clocks {
            self.journal_note(event);
        }
        if slept {
            let ids: Vec<String> = self.journal.turns.keys().cloned().collect();
            for id in ids {
                self.journal_turn_signal(&id, Signal::Slept, None, cx);
            }
        }
        self.journal_turns_tick(cx);
        if !self.journal.outside_running && self.journal.outside_at.is_none_or(|at| at.elapsed() >= OUTSIDE_EVERY) {
            self.journal_outside(cx);
        }
        if woke || self.journal.reconciled_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(reconcile::EVERY_SECS)) {
            self.journal_reconcile();
        }
        if !self.journal.git_running && self.journal.git_at.is_none_or(|at| at.elapsed() >= GIT_EVERY) {
            self.journal_git(cx);
        }
        if self.journal.cursors_dirty && self.journal.cursors_at.is_none_or(|at| at.elapsed() >= CURSORS_EVERY) {
            self.journal.cursors_at = Some(Instant::now());
            self.journal.cursors_dirty = false;
            // Only the sessions there still are.
            let state = &self.state;
            self.journal.cursors.spools.retain(|id, _| state.session(id).is_some());
            self.journal.intakes.retain(|id, _| state.session(id).is_some());
            let cursors = self.journal.cursors.clone();
            cx.background_spawn(async move {
                if let Err(err) = cursors.save() {
                    log::warn!("journal: could not keep the spool cursors: {err}");
                }
            })
            .detach();
        }
        if !self.journal.server_running && self.journal.server_at.is_none_or(|at| at.elapsed() >= SERVER_EVERY) {
            self.journal_server_pull(cx);
        }
        // A journal that cannot be written says so once, without the journal (events-m4).
        match (self.journal_warning(), self.journal.told_failing) {
            (Some(warning), false) => {
                self.journal.told_failing = true;
                self.show_toast(warning, cx);
            }
            (None, true) => self.journal.told_failing = false,
            _ => {}
        }
    }

    /// Compare what is true now with what the journal implies (D24's safety net).
    fn journal_reconcile(&mut self) {
        self.journal.reconciled_at = Some(Instant::now());
        let Some(mut reconciler) = self.journal.reconciler.take() else { return };
        let snapshot = self.journal_snapshot();
        for event in reconciler.run(&snapshot) {
            journal::append(event);
        }
        self.journal.reconciler = Some(reconciler);
    }

    /// What is true now, from what the window holds in memory (no IO).
    fn journal_snapshot(&self) -> reconcile::Snapshot {
        let sessions = self
            .state
            .sessions
            .iter()
            .map(|s| reconcile::SessionNow {
                id: s.id.clone(),
                project: self.control.project_keys.get(&s.project_id).cloned(),
                running: self.terminals.contains_key(&s.id),
                activity: self.activity.get(&s.id).copied(),
                pr: self.prs.map.get(&s.id).filter(|p| p.acts()).and_then(|p| {
                    let pr = p.pr.as_ref()?;
                    let state = p.ci.as_ref().map(|ci| ci.state.clone()).unwrap_or_else(|| pr.state.clone()).to_uppercase();
                    Some((p.repo.clone()?, pr.number, if state.is_empty() { "OPEN".into() } else { state }))
                }),
            })
            .collect();
        let worktrees = self.worktrees.iter().flat_map(|(project, list)| list.iter().map(move |w| (self.control.project_keys.get(project).cloned(), w.path.clone()))).collect();
        reconcile::Snapshot { sessions, worktrees }
    }

    /// The checkouts of the sessions: a commit, a push, another branch (from their git files).
    fn journal_git(&mut self, cx: &mut Context<Self>) {
        self.journal.git_at = Some(Instant::now());
        self.journal.git_running = true;
        let mut checkouts: Vec<(PathBuf, Option<ProjectKey>, Option<String>)> = Vec::new();
        for session in &self.state.sessions {
            if checkouts.iter().all(|(path, _, _)| *path != session.worktree) {
                checkouts.push((session.worktree.clone(), self.control.project_keys.get(&session.project_id).cloned(), Some(session.id.clone())));
            }
        }
        let watch = self.journal.git.clone();
        let task = cx.background_spawn(async move { watch.lock().map(|mut watch| watch.observe(&checkouts)).unwrap_or_default() });
        cx.spawn(async move |this, cx| {
            let events = task.await;
            let _ = this.update(cx, |this, _| {
                this.journal.git_running = false;
                for event in events {
                    this.journal_note(event);
                }
            });
        })
        .detach();
    }

    /// The server's own journal, from where the last pull stopped (`journal.since`), into this
    /// one: its events keep their stamps, which name the server's machine.
    fn journal_server_pull(&mut self, cx: &mut Context<Self>) {
        self.journal.server_at = Some(Instant::now());
        let Some(client) = self.server.client.clone().filter(|c| self.server.connected() && c.hello.features.iter().any(|f| f == "journal")) else { return };
        let Some(target) = self.server.config.as_ref().map(|c| c.ssh_target.clone()) else { return };
        let cursor = self.journal.cursors.servers.get(&target).cloned();
        self.journal.server_running = true;
        let task = cx.background_spawn(async move { client.call_value("journal.since", json!({ "cursor": cursor, "max": 1000 })) });
        cx.spawn(async move |this, cx| {
            let answer = task.await;
            let _ = this.update(cx, |this, _| {
                this.journal.server_running = false;
                let answer = match answer {
                    Ok(answer) => answer,
                    Err(err) => {
                        log::debug!("journal: the server's journal could not be read: {err:#}");
                        return;
                    }
                };
                let events: Vec<Event> = answer.get("events").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
                let Some(next) = answer.get("cursor").cloned().and_then(|v| serde_json::from_value::<Cursor>(v).ok()) else { return };
                let count = events.len();
                for event in events {
                    if let Some(reconciler) = this.journal.reconciler.as_mut() {
                        reconciler.observe(&event);
                    }
                    journal::append_stamped(event);
                }
                if count > 0 {
                    this.journal_note(Event::new(kinds::SERVER_PULLED, Source::App, format!("{count} events from {target}")).data(json!({ "count": count, "server": target, "cursor": next })));
                }
                this.journal.cursors.servers.insert(target, next);
                this.journal.cursors_dirty = true;
            });
        })
        .detach();
    }

    /// The status bar's word on the journal: why it cannot be written, while it cannot.
    pub fn journal_warning(&self) -> Option<String> {
        if !self.journal.on {
            return None;
        }
        journal::health().error.map(|why| format!("The journal cannot be written ({why}); trying again every 30 s."))
    }
}

// ----- the ops ---------------------------------------------------------------------------------

/// What a caller may read of the journal (events-6): a session its own project, a terminal the
/// project of its folder, the manager the projects it manages, the person everything.
pub(crate) fn scope_of(ws: &Workspace, caller: &Caller) -> Scope {
    use glasscode_core::caller::Caller as Channel;
    let key_of_project = |id: &str| ws.control.project_keys.get(id).cloned();
    match &caller.channel {
        Channel::Session { id, .. } => Scope::Projects(ws.state.session(id).and_then(|s| key_of_project(&s.project_id)).into_iter().collect()),
        Channel::Terminal { .. } => {
            let project = caller.cwd.as_ref().and_then(|cwd| ws.state.projects.iter().filter(|p| cwd.starts_with(&p.root)).max_by_key(|p| p.root.as_os_str().len())).and_then(|p| key_of_project(&p.id));
            Scope::Projects(project.into_iter().collect())
        }
        Channel::Manager { .. } => Scope::Projects(ws.control.project_keys.values().cloned().collect()),
        other => Scope::of(other, &[]),
    }
}

/// `10m`, `2h`, `3d`, `45s`, or milliseconds since 1970: the time from which.
fn since_ms(text: &str, now_ms: u64) -> Option<u64> {
    let text = text.trim();
    if let Ok(at) = text.parse::<u64>() {
        return Some(at);
    }
    let (number, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = number.parse().ok()?;
    let ms = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    Some(now_ms.saturating_sub(n.saturating_mul(ms)))
}

/// The filter an op's arguments ask for.
pub(crate) fn filter_of(ws: &Workspace, op: &Op) -> Result<Filter, Reply> {
    let project = match op.text("project") {
        Some(word) => {
            let found = ws.state.projects.iter().find(|p| p.id == word || p.name.eq_ignore_ascii_case(word)).ok_or_else(|| usage(format!("No project “{word}”. `glasscode project list` lists them.")))?;
            Some(ws.control.project_keys.get(&found.id).cloned().ok_or_else(|| failed(format!("{} has no project key yet, so its events cannot be told apart.", found.name)))?)
        }
        None => None,
    };
    let since = match op.text("since") {
        Some(text) => Some(since_ms(text, glasscode_core::clock::now_ms()).ok_or_else(|| usage(format!("“{text}” is not a time: say 10m, 2h, 3d, or milliseconds since 1970.")))?),
        None => None,
    };
    Ok(Filter { project, session: op.text("session").map(str::to_string), kind: op.text("kind").map(str::to_string), since_ms: since })
}

/// `14:02` today, else `Nov 18 14:02`.
fn when(at_ms: u64) -> String {
    let secs = at_ms / 1000;
    let day = glasscode_core::day::local(secs as i64);
    if day == glasscode_core::day::today() { crate::commands::clock(secs) } else { format!("{} {}", glasscode_core::day::short(day), crate::commands::clock(secs)) }
}

/// One event for a person: `14:02  session.needs_input  [8765f51a]  Claude needs your permission`.
pub(crate) fn event_line(event: &Event) -> String {
    let session = event.session.as_deref().map(|s| format!("  [{}]", s.chars().take(8).collect::<String>())).unwrap_or_default();
    format!("{}  {}{session}  {}", when(event.at_ms()), event.kind, event.summary)
}

/// A row of `journal query`: the fields of `log --json` (`LogEntry`), and the event's own.
pub(crate) fn row(event: &Event) -> Value {
    let log: Option<LogEntry> = event.data.get("log").cloned().and_then(|v| serde_json::from_value(v).ok());
    let at = event.at_ms() / 1000;
    let (who, session_title, what, command, outcome) = match log {
        Some(log) => (log.who, log.session_title, log.what, log.command, log.outcome),
        None => (event.source.name().to_string(), None, event.summary.clone(), String::new(), String::new()),
    };
    json!({
        "at": at, "who": who, "session": event.session, "session_title": session_title, "what": what, "command": command, "outcome": outcome,
        "id": event.id, "kind": event.kind, "project": event.project, "severity": event.severity, "source": event.source, "data": event.data,
    })
}

/// A row for a person, as `log` printed its lines.
pub(crate) fn row_line(row: &Value) -> String {
    let at = row["at"].as_u64().unwrap_or_default();
    let from = row["session_title"].as_str().map(|t| format!(" in “{t}”")).unwrap_or_default();
    format!("{}  {}{from}: {}", when(at * 1000), row["who"].as_str().unwrap_or_default(), row["what"].as_str().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_read_as_ago_or_as_milliseconds() {
        let now = 10_000_000;
        assert_eq!(since_ms("10m", now), Some(now - 600_000));
        assert_eq!(since_ms("2h", now), Some(now - 7_200_000));
        assert_eq!(since_ms("3h", now), Some(0));
        assert_eq!(since_ms("45s", now), Some(now - 45_000));
        assert_eq!(since_ms("1763560000000", now), Some(1_763_560_000_000));
        for bad in ["", "m", "10x", "ten minutes"] {
            assert_eq!(since_ms(bad, now), None, "{bad}");
        }
    }

    #[test]
    fn a_row_keeps_the_fields_of_log_json() {
        let entry = LogEntry { at: 1_763_560_000, who: "Claude".into(), session: Some("s1".into()), session_title: Some("API".into()), what: "added “x” to Today".into(), command: "glasscode board add x".into(), outcome: "done".into() };
        let event = Event::new(kinds::OP_DONE, Source::Control, entry.what.clone()).session(Some("s1")).data(json!({ "log": entry }));
        let row = row(&event);
        let back: LogEntry = serde_json::from_value(row.clone()).unwrap();
        assert_eq!((back.who, back.what, back.command, back.outcome, back.session_title), (entry.who, entry.what, entry.command, entry.outcome, entry.session_title));
        assert_eq!(row["kind"], "op.done");
        // An event that is no action reads as a row too.
        let other = Event::new(kinds::GIT_PUSH, Source::Git, "pushed main");
        assert!(serde_json::from_value::<LogEntry>(row_of(&other)).is_ok());
    }

    fn row_of(event: &Event) -> Value {
        row(event)
    }
}
