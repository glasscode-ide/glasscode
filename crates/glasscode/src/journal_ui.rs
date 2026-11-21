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
    /// The Codex sessions whose conversation this launch found (it began with their first turn):
    /// the turns' tail reads their rollout from its start (`RolloutTail::for_turns`).
    pub found_rollouts: HashSet<String>,
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
    /// The work ledger (M32): None until read at the start, and when its file is damaged.
    work: Option<glasscode_core::work::WorkLedger>,
    work_at: Option<Instant>,
    work_dirty: bool,
    /// This machine's id, for the ledger's stamps (read at the start).
    machine: Option<glasscode_core::paths::MachineId>,
    /// The sessions that run on the server, as its last state said (M32's Server line): their
    /// work is kept here, and they count as live while the server cannot say otherwise.
    server_sessions: std::collections::BTreeSet<String>,
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
    /// Where the session's last turn started in its transcript: the next starts after it.
    last_start: Option<u64>,
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
    work: Option<glasscode_core::work::WorkLedger>,
    machine: Option<glasscode_core::paths::MachineId>,
    /// The log lines of its last op events, newest first (Today › Activity).
    log: Vec<LogEntry>,
}

impl Workspace {
    /// At the start, in the background: whether the journal runs, the spool cursors and the last
    /// two days of the journal (what the reconciler starts from). Then the spools read on from
    /// where they stopped, and the reconciler runs once.
    pub fn journal_start(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move {
            if !journal::init() {
                return Started { on: false, cursors: Cursors::default(), reconciler: Reconciler::default(), open_turns: Vec::new(), work: None, machine: None, log: Vec::new() };
            }
            let since = Filter { since_ms: Some(glasscode_core::clock::now_ms().saturating_sub(2 * 86_400_000)), ..Filter::default() };
            let recent = journal::query(&since, &Scope::All, 50_000);
            let open_turns = turns::open_in(&recent);
            let work = glasscode_core::work::WorkLedger::load().map_err(|err| log::warn!("work ledger: {err}; done conditions are not checked until it is repaired")).ok();
            let machine = glasscode_core::paths::MachineId::this().ok();
            let log = glasscode_core::control::log_lines(&recent, 200);
            Started { on: true, cursors: Cursors::load().unwrap_or_default(), reconciler: Reconciler::from_events(&recent), open_turns, work, machine, log }
        });
        cx.spawn(async move |this, cx| {
            let started = task.await;
            let _ = this.update(cx, |this, cx| {
                if !started.on {
                    return;
                }
                this.journal.on = true;
                this.journal.work = started.work;
                this.journal.machine = started.machine;
                // Today › Activity: what this run did already, then the journal's last lines.
                let mut log = std::mem::take(&mut this.control.log);
                let older: Vec<LogEntry> = started.log.into_iter().filter(|line| !log.contains(line)).collect();
                log.extend(older);
                log.truncate(200);
                this.control.log = log;
                // Codex runs memory's brief hook once it trusts it (M22, P06).
                this.codex_hook_trust(cx);
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
                    // From its prompt's line when the transcript already holds the turn (a short
                    // one, its hooks read together).
                    let after = self.journal.turns.get(id).and_then(|t| t.last_start);
                    let offset = turns::turn_offset(session.and_then(|s| s.transcript_path.as_ref()), prompt.as_deref(), after);
                    let head = session.and_then(|s| glasscode_core::git_info::heads(&s.worktree)).and_then(|h| h.head);
                    let entry = self.journal.turns.entry(id.to_string()).or_default();
                    entry.machine.set_baseline(&turn.id, offset, head.clone());
                    entry.last_start = offset.or(entry.last_start);
                    entry.grew_at = Some(Instant::now());
                    let turn = turns::OpenTurn { transcript_offset: offset, head, ..turn };
                    let event = turns::started_event(id, self.journal_key(id).as_ref(), &turn, prompt.as_deref());
                    self.journal_note(event);
                }
                Change::Closed(closed) => self.journal_turn_closed(id, closed, Some(&mut *cx)),
            }
        }
    }

    /// v2 (M61): a child session ended its turn: its summary is kept with the child
    /// (`manager/children.json`) until it went back to the parent ([`Workspace::children_tick`]),
    /// so a parent that is closed, or a quit, loses nothing.
    fn children_turn_ended(&mut self, report: turns::TurnReport, cx: &mut Context<Self>) {
        // Its summary, once it finished or failed: a turn cut off (a sleep, a quit, an interrupt,
        // a pause) is not its answer yet.
        if !matches!(report.end, turns::TurnEnd::Completed | turns::TurnEnd::Failed(_)) {
            return;
        }
        let child_title = self.state.session(&report.session).map(|s| self.session_title(s)).unwrap_or_else(|| report.session.clone());
        cx.background_spawn(async move {
            use glasscode_core::store::Store as _;
            let Some(entry) = glasscode_core::children::Children::load().ok().and_then(|file| file.waiting(&report.session).cloned()) else { return };
            let text = glasscode_core::children::summary_message(&child_title, &entry.task, &report);
            if let Err(err) = glasscode_core::store::update::<glasscode_core::children::Children, _>(|file| file.summary(&report.session, &report.turn, &text)) {
                log::warn!("children: the summary of {} could not be kept ({err})", report.session);
            }
        })
        .detach();
    }

    /// v2 (M61), now and then: the children's summaries that wait go to their parents through the
    /// delivery queue, once each (the effect ledger keeps it to once per child turn and run); one
    /// that went is marked, and one whose parent is gone is let go.
    pub(crate) fn children_tick(&mut self, cx: &mut Context<Self>) {
        if self.children_at.is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(30)) {
            return;
        }
        self.children_at = Some(std::time::Instant::now());
        let launch = self.control.launch.clone();
        let key_of = move |child: &str, turn: &str, parent: &str| glasscode_core::effects::effect_key("child", &format!("{child}@{turn}@{launch}"), "session.send", parent);
        let key_task = key_of.clone();
        let task = cx.background_spawn(async move {
            use glasscode_core::store::Store as _;
            let file = glasscode_core::children::Children::load().unwrap_or_default();
            let ledger = glasscode_core::effects::load().ok();
            let mut went = Vec::new();
            let mut waiting = Vec::new();
            for child in file.due() {
                let Some((turn, text)) = child.pending.clone() else { continue };
                let key = key_task(&child.child, &turn, &child.parent);
                let state = ledger.as_ref().and_then(|l| l.records.get(&key)).map(|r| r.state.clone());
                match state {
                    Some(glasscode_core::effects::EffectState::Done(_)) => went.push(child.child.clone()),
                    _ => waiting.push((child.clone(), turn, text)),
                }
            }
            if !went.is_empty() {
                let now = glasscode_core::clock::now_ms();
                let _ = glasscode_core::store::update::<glasscode_core::children::Children, _>(|file| {
                    for child in &went {
                        file.reported(child, now);
                    }
                });
            }
            waiting
        });
        cx.spawn(async move |this, cx| {
            let waiting = task.await;
            let _ = this.update(cx, |ws, cx| {
                for (child, turn, text) in waiting {
                    // A parent that is gone hears nothing; its child is done all the same.
                    if ws.state.session(&child.parent).is_none() {
                        let gone = child.child.clone();
                        std::thread::spawn(move || {
                            let _ = glasscode_core::store::update::<glasscode_core::children::Children, _>(|file| file.reported(&gone, glasscode_core::clock::now_ms()));
                        });
                        continue;
                    }
                    // It waits for its parent to be open (the queue types only into a running one).
                    if !ws.terminals.contains_key(&child.parent) {
                        continue;
                    }
                    let child_title = ws.state.session(&child.child).map(|s| ws.session_title(s)).unwrap_or_else(|| child.child.clone());
                    // What the child's turn descended from, passed on (events-13).
                    let (depth, causes) = glasscode_core::journal::driven(&child.child).map(|d| (d.depth, d.causes)).unwrap_or_default();
                    let now = glasscode_core::clock::now_ms();
                    let delivery = glasscode_core::delivery_session::Delivery {
                        note: Some(format!("(Sent by Glasscode: what the child session “{child_title}” said.)")),
                        depth,
                        causes,
                        ..glasscode_core::delivery_session::Delivery::new(&child.parent, &text, glasscode_core::words::InjectKind::SessionSend, now)
                    };
                    let key = key_of(&child.child, &turn, &child.parent);
                    let intent = glasscode_core::effects::Intent {
                        origin: "child".into(),
                        subject: format!("{}@{turn}", child.child),
                        target: child.parent.clone(),
                        caller: format!("the child session “{child_title}”"),
                        reason: glasscode_core::words::scrub(&child.task.chars().take(80).collect::<String>()),
                        session: Some(child.parent.clone()),
                        launch: ws.control.launch.clone(),
                        ..glasscode_core::effects::Intent::new(key, "session.send", glasscode_core::ops::Retry::Verifiable)
                    };
                    ws.queue_delivery(delivery, Some(intent), cx);
                }
            });
        })
        .detach();
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
            let _ = this.update(cx, |this, cx| {
                let key = this.journal_key(&id).or(key);
                this.journal_note(turns::ended_event(key.as_ref(), &report));
                this.v2_turn_ended(&report);
                this.children_turn_ended(report, cx);
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
                        entry.rollout = Some((path.clone(), agents::RolloutTail::for_turns(&path, self.journal.found_rollouts.remove(&id))));
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
        self.journal_work_observe(&event);
        journal::append(event);
    }

    /// The facts of the work an event is about (M32); a renamed branch moves its work file.
    fn journal_work_observe(&mut self, event: &Event) {
        // A session whose first turn ends before the work round (every 5 s) made its work: its
        // work is made now, so the turn's facts are not lost (found in the journey run).
        let new_work = event.session.as_deref().and_then(|id| {
            let ledger = self.journal.work.as_ref()?;
            if ledger.of_session(id).is_some() {
                return None;
            }
            let session = self.state.session(id).filter(|s| s.agent != glasscode_core::model::AgentKind::Shell)?;
            let project = self.control.project_keys.get(&session.project_id)?;
            Some((self.state.work_key(project, session), session.clone(), self.state.ticket_of.get(id).cloned()))
        });
        let (Some(ledger), Some(machine)) = (self.journal.work.as_mut(), self.journal.machine.as_ref()) else { return };
        if let Some((key, session, ticket)) = new_work {
            self.journal.work_dirty |= ledger.ensure(key, &session, ticket.as_deref(), machine);
        }
        if event.session.as_deref().is_some_and(|session| ledger.of_session(session).is_some()) {
            self.journal.work_dirty = true;
        }
        let changes = ledger.observe(event, machine);
        for change in changes {
            if let glasscode_core::work::MemoryChange::Move { from, to } = change
                && let Some(item) = ledger.item(&to).cloned()
            {
                std::thread::spawn(move || {
                    if let Err(err) = glasscode_core::memstore::Memory::open().move_work(&from, &item) {
                        log::warn!("work ledger: could not move the work file of {}: {err}", from.id);
                    }
                });
            }
        }
    }

    /// The work ledger's part of the tick (every 5 s): each session's work (made with its kind's
    /// done conditions), the states evaluated from the facts, their events, the work files of the
    /// ones that changed, and the ledger kept on disk.
    fn journal_work_tick(&mut self, cx: &mut Context<Self>) {
        if self.journal.work_at.is_some_and(|at| at.elapsed() < Duration::from_secs(5)) {
            return;
        }
        self.journal.work_at = Some(Instant::now());
        let Some(machine) = self.journal.machine.clone() else { return };
        let sessions: Vec<(glasscode_core::identity::WorkKey, glasscode_core::model::Session, Option<String>)> = self
            .state
            .sessions
            .iter()
            .filter(|s| s.agent != glasscode_core::model::AgentKind::Shell)
            .filter_map(|s| {
                let project = self.control.project_keys.get(&s.project_id)?;
                let ticket = self.state.ticket_of.get(&s.id).cloned();
                Some((self.state.work_key(project, s), s.clone(), ticket))
            })
            .collect();
        // What the last status round confirmed: a pull request that existed before this run, or
        // merged while Glasscode was closed, has no change event.
        let rounds: Vec<(String, String, u64, String, bool, Option<(String, String)>)> = self
            .state
            .sessions
            .iter()
            .filter_map(|s| {
                let status = self.prs.map.get(&s.id).filter(|p| p.acts())?;
                let pr = status.pr.as_ref()?;
                let state = status.ci.as_ref().map(|ci| ci.state.clone()).filter(|st| !st.is_empty()).unwrap_or_else(|| if pr.state.is_empty() { "OPEN".into() } else { pr.state.clone() });
                let ci = status.ci.as_ref().and_then(|ci| Some((ci.head.clone(), journal::ci_overall(ci.checks.iter().map(|c| c.state()))?.to_string())));
                Some((s.id.clone(), status.repo.clone()?, pr.number, state, pr.is_draft, ci))
            })
            .collect();
        // What the round says of reviews (M32): a review asked for on a session's pull request, and
        // a review session's own review posted since it started, each said once.
        let mine = self.github.list.as_ref().and_then(|list| list.iter().find(|a| a.active).map(|a| a.login.clone()));
        let reviews: Vec<Event> = self
            .state
            .sessions
            .iter()
            .filter_map(|s| {
                let status = self.prs.map.get(&s.id).filter(|p| p.acts())?;
                let pr = status.pr.as_ref()?;
                let repo = status.repo.clone()?;
                let login = self.github_account_for(&s.project_id).or_else(|| mine.clone());
                let (requested, posted) = glasscode_core::work::review_facts(pr, login.as_deref(), s.created_at);
                let item = self.journal.work.as_ref().and_then(|ledger| ledger.of_session(&s.id));
                Some(glasscode_core::work::review_events(item, &s.id, self.control.project_keys.get(&s.project_id), &repo, pr.number, requested, posted))
            })
            .flatten()
            .collect();
        for event in reviews {
            self.journal_note(event);
        }
        // The sessions that run on the server (M32's Server line): their work is kept here, by
        // the Mac's key for the same project, with the facts of their pull request's glance and
        // the turns of the server's journal (`journal_server_pull`).
        let (server_sessions, server_prs) = self.journal_server_work();
        for (id, pr) in &server_prs {
            let item = self.journal.work.as_ref().and_then(|ledger| ledger.of_session(id));
            let project = item.map(|item| item.key.project.clone());
            for event in glasscode_core::work::review_events(item, id, project.as_ref(), &pr.repo, pr.number, pr.reviewed, false) {
                self.journal_note(event);
            }
        }
        let mut live: std::collections::BTreeSet<String> = self.state.sessions.iter().map(|s| s.id.clone()).collect();
        live.extend(self.journal.server_sessions.iter().cloned());
        let Some(ledger) = self.journal.work.as_mut() else { return };
        for (key, session, ticket) in sessions.into_iter().chain(server_sessions) {
            if ledger.of_session(&session.id).is_none() {
                self.journal.work_dirty |= ledger.ensure(key, &session, ticket.as_deref(), &machine);
            }
        }
        for (session, repo, number, state, draft, ci) in rounds {
            self.journal.work_dirty |= ledger.set_pr_facts(&session, &repo, number, &state, draft, ci);
        }
        for (session, pr) in server_prs {
            self.journal.work_dirty |= ledger.set_pr_facts(&session, &pr.repo, pr.number, &pr.state, pr.draft, pr.ci);
        }
        let (mut events, mut writes) = ledger.drop_gone(&live, &machine);
        let (evaluated, written) = ledger.evaluate(glasscode_core::clock::now_ms(), &machine);
        events.extend(evaluated);
        writes.extend(written);
        let items: Vec<glasscode_core::work::WorkItem> = writes
            .iter()
            .filter_map(|change| match change {
                glasscode_core::work::MemoryChange::Write(key) => ledger.item(key).cloned(),
                _ => None,
            })
            .collect();
        // The work whose pull request ended a week ago with no session open: its memory goes to
        // the archive (M24), once, after the work files of this tick are written.
        let now = glasscode_core::clock::now_ms();
        let archives: Vec<glasscode_core::memstore::archive::ArchiveInput> = ledger
            .due_for_archive(now, &live)
            .into_iter()
            .filter_map(|key| {
                let item = ledger.item(&key)?.clone();
                ledger.set_archived(&key, true);
                let mut input = glasscode_core::memstore::archive::ArchiveInput::new(key.project.clone(), glasscode_core::memstore::archive::Ended::PrEnded, now);
                input.branch = (key.kind == glasscode_core::identity::WorkKind::Branch).then(|| key.id.clone());
                input.work = Some(key);
                if let Some((repo, number)) = &item.pr {
                    input.links.push((format!("PR {repo}#{number}"), item.facts.pr_state.clone().unwrap_or_default().to_lowercase()));
                }
                if let Some(ticket) = &item.ticket {
                    input.links.push((format!("ticket {ticket}"), item.state.words()));
                }
                Some(input)
            })
            .collect();
        if !events.is_empty() || !archives.is_empty() {
            self.journal.work_dirty = true;
        }
        for event in events {
            journal::append(event);
        }
        if !items.is_empty() || !archives.is_empty() {
            let task = cx.background_spawn(async move {
                let memory = glasscode_core::memstore::Memory::open();
                for item in items {
                    if let Err(err) = memory.write_work(&item) {
                        log::warn!("work ledger: could not write the work file of {}: {err}", item.key.id);
                    }
                }
                let mut failed = Vec::new();
                for input in archives {
                    if let Err(err) = memory.archive(&input) {
                        log::warn!("work ledger: the memory of {} did not go to the archive: {err}", input.work.as_ref().map(|k| k.id.as_str()).unwrap_or_default());
                        failed.extend(input.work);
                    }
                }
                failed
            });
            cx.spawn(async move |this, cx| {
                let failed = task.await;
                if failed.is_empty() {
                    return;
                }
                // Tried again at a later tick.
                let _ = this.update(cx, |this, _| {
                    if let Some(ledger) = this.journal.work.as_mut() {
                        for key in &failed {
                            this.journal.work_dirty |= ledger.set_archived(key, false);
                        }
                    }
                });
            })
            .detach();
        }
        if std::mem::take(&mut self.journal.work_dirty)
            && let Some(ledger) = self.journal.work.clone()
        {
            cx.background_spawn(async move {
                if let Err(err) = ledger.save() {
                    log::warn!("work ledger: could not be kept: {err}");
                }
            })
            .detach();
        }
    }

    /// The server's own sessions, from its last state while it is connected (the Mac's mirrors on
    /// the server are this Mac's sessions, not these): each one's work key, by the Mac's project of
    /// the same id or name and its key, and its pull request's facts. While the server is not
    /// connected the sessions last seen stay live, with nothing new.
    fn journal_server_work(&mut self) -> (Vec<(glasscode_core::identity::WorkKey, glasscode_core::model::Session, Option<String>)>, Vec<(String, glasscode_core::work::ServerPr)>) {
        let Some(server) = self.server.state.as_ref().filter(|_| self.server.connected()) else { return (Vec::new(), Vec::new()) };
        let mut keys = Vec::new();
        let mut prs = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for remote in server.sessions.iter().filter(|r| r.session.agent != glasscode_core::model::AgentKind::Shell) {
            let session = &remote.session;
            seen.insert(session.id.clone());
            let Some(project) = glasscode_core::work::mac_project_of(&self.state.projects, &server.projects, &session.project_id) else { continue };
            let Some(key) = self.control.project_keys.get(&project.id) else { continue };
            // A session moved from this Mac keeps its links here.
            keys.push((self.state.work_key(key, session), session.clone(), self.state.ticket_of.get(&session.id).cloned()));
            if let Some(pr) = remote.pr.as_ref().and_then(|pr| glasscode_core::work::ServerPr::of(pr, server.repos.get(&session.project_id).map(String::as_str))) {
                prs.push((session.id.clone(), pr));
            }
        }
        self.journal.server_sessions = seen;
        (keys, prs)
    }

    /// The work ledger, while it runs (M32): what `work list` and `work show` read.
    pub(crate) fn work_ledger(&self) -> Option<&glasscode_core::work::WorkLedger> {
        self.journal.work.as_ref()
    }

    /// A change to the work ledger from outside the tick (the follow-ups, M56.2): made in memory,
    /// and kept by the next tick (the tick is the ledger's one writer). Each follow-up it changes
    /// is stamped with this Mac's next stamp, so the sync takes it to the server
    /// (`WorkLedger::follow_edit`). None while the ledger is not running here.
    pub(crate) fn work_ledger_edit<R>(&mut self, edit: impl FnOnce(&mut glasscode_core::work::WorkLedger) -> R) -> Option<R> {
        let machine = self.journal.machine.clone();
        let ledger = self.journal.work.as_mut()?;
        let out = match &machine {
            Some(machine) => ledger.follow_edit(machine, edit),
            None => edit(ledger),
        };
        self.journal.work_dirty = true;
        Some(out)
    }

    /// The server's changes to the follow-ups (M56.2), from the sync: merged by stamp into the
    /// ledger, which the next tick keeps.
    pub fn journal_follow_ups_take_theirs(&mut self, theirs: &[glasscode_core::work::FollowSync]) {
        let Some(ledger) = self.journal.work.as_mut() else { return };
        if ledger.follow_merge(theirs) {
            self.journal.work_dirty = true;
        }
    }

    /// The person's word on a piece of work (`work done`, `drop`, `reopen`): marked in the ledger,
    /// its `work.changed` in the journal, its work file written in the background, and the ledger
    /// kept by the next tick (the tick is the ledger's one writer). The state it had, and the item.
    pub(crate) fn work_mark(&mut self, key: &glasscode_core::identity::WorkKey, mark: glasscode_core::work::Mark) -> Result<(glasscode_core::work::WorkState, glasscode_core::work::WorkItem), String> {
        let machine = self.journal.machine.clone().ok_or("this Mac's id could not be read, so the ledger cannot be changed")?;
        let ledger = self.journal.work.as_mut().ok_or("the work ledger is not running here")?;
        let (before, event) = ledger.mark(key, mark, glasscode_core::clock::now_ms(), &machine)?;
        let item = ledger.item(key).cloned().ok_or("the work went from the ledger")?;
        self.journal.work_dirty = true;
        // Kept at the next tick, not five seconds on.
        self.journal.work_at = None;
        self.journal_note(event);
        let written = item.clone();
        std::mem::drop(std::thread::spawn(move || {
            if let Err(err) = glasscode_core::memstore::Memory::open().write_work(&written) {
                log::warn!("work ledger: could not write the work file of {}: {err}", written.key.id);
            }
        }));
        Ok((before, item))
    }

    /// The server's changes to work items (a sync, M57), merged into the ledger here; the work
    /// files of those that changed are written, and the ledger kept with the next tick.
    pub fn journal_work_take_theirs(&mut self, theirs: &[glasscode_core::work::WorkItem], cx: &mut Context<Self>) {
        let Some(ledger) = self.journal.work.as_mut() else { return };
        let changed = ledger.merge_theirs(theirs);
        if changed.is_empty() {
            return;
        }
        self.journal.work_dirty = true;
        let items: Vec<glasscode_core::work::WorkItem> = changed.iter().filter_map(|key| ledger.item(key).cloned()).collect();
        cx.background_spawn(async move {
            let memory = glasscode_core::memstore::Memory::open();
            for item in items {
                if let Err(err) = memory.write_work(&item) {
                    log::warn!("work ledger: could not write the work file of {}: {err}", item.key.id);
                }
            }
        })
        .detach();
    }

    /// How far this Mac's own journal went to the paired server with the mirror sync (M57).
    pub fn journal_sent(&self) -> Option<journal::Cursor> {
        let target = self.server.config.as_ref()?.ssh_target.clone();
        self.journal.cursors.sent.get(&target).cloned()
    }

    /// The mirror sync sent this Mac's journal up to `cursor`: kept with the other cursors.
    pub fn journal_set_sent(&mut self, cursor: journal::Cursor) {
        let Some(target) = self.server.config.as_ref().map(|c| c.ssh_target.clone()) else { return };
        if self.journal.cursors.sent.get(&target) != Some(&cursor) {
            self.journal.cursors.sent.insert(target, cursor);
            self.journal.cursors_dirty = true;
        }
    }

    /// Claude is about to compact a session's conversation (its `PreCompact` hook, land-18): its
    /// last turns, from their reports in the journal, go into its work file's "Before compaction"
    /// section, which the brief after the compaction carries. In the background; no model.
    fn journal_before_compaction(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(key) = self.journal.work.as_ref().and_then(|ledger| ledger.of_session(id)).map(|item| item.key.clone()) else { return };
        let session = id.to_string();
        cx.background_spawn(async move {
            let filter = Filter { session: Some(session), kind: Some(kinds::TURN_ENDED.as_str().to_string()), ..Filter::default() };
            let reports: Vec<turns::TurnReport> = journal::query(&filter, &Scope::All, 10).iter().filter_map(turns::report_of).collect();
            if reports.is_empty() {
                return;
            }
            let digest = turns::compaction_digest(&reports, 2000);
            if let Err(err) = glasscode_core::memstore::Memory::open().write_turns(&key, &digest) {
                log::warn!("memory: the last turns of {} were not written before a compaction: {err}", key.id);
            }
        })
        .detach();
    }

    /// Each Codex account's `hooks.json` gets memory's brief hook, and its `config.toml` the trust of
    /// that hook from `codex app-server`'s `hooks/list` (M22, P06), so a Codex session gets its
    /// brief from the hook instead of asking for it. In the background, with the brief on; a
    /// session that starts before it is done asks, as before.
    pub(crate) fn codex_hook_trust(&mut self, cx: &mut Context<Self>) {
        if !glasscode_core::journal::feature_on("brief") {
            return;
        }
        let Some(command) = glasscode_core::agents::brief_hook_command() else { return };
        let mut homes: Vec<std::path::PathBuf> = self.state.accounts.iter().filter(|a| a.agent == glasscode_core::model::AgentKind::Codex).map(glasscode_core::model::Account::effective_dir).collect();
        let default = glasscode_core::paths::home().join(".codex");
        if default.is_dir() {
            homes.push(default);
        }
        homes.sort();
        homes.dedup();
        let env = (*self.env).clone();
        cx.background_spawn(async move {
            let (program, first) = glasscode_core::agents::codex_program(&env);
            for home in homes {
                if let Err(err) = glasscode_core::codex_hooks::trust_brief_hook(&program, &first, &home, &home, &command, &env) {
                    log::warn!("codex: memory's brief hook is not trusted in {}: {err:#}; its sessions ask for their brief", home.display());
                }
            }
        })
        .detach();
    }

    /// The sessions whose work is stuck, with why (Today's "Needs you").
    pub(crate) fn journal_stuck(&self) -> Vec<(String, String)> {
        let Some(ledger) = self.journal.work.as_ref() else { return Vec::new() };
        ledger
            .items
            .iter()
            .filter_map(|item| match &item.state {
                glasscode_core::work::WorkState::Stuck(why) => Some((item.sessions.last()?.clone(), why.clone())),
                _ => None,
            })
            .collect()
    }

    /// A session's project key, for its events.
    pub(crate) fn journal_key(&self, session: &str) -> Option<ProjectKey> {
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
            if payload.get("hook_event_name").and_then(Value::as_str) == Some("PreCompact") {
                self.journal_before_compaction(id, cx);
            }
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
        // An action changes what an earlier Undo would restore: that Undo ends (M52, ux-14).
        self.chat_expire_undos(action);
        // "Keep the log of what agents did" off (`agents.log_off`): no record of ops either.
        if !self.journal.on || self.state.agents.log_off {
            return;
        }
        let kind = if action.code == code::OK { kinds::OP_DONE } else { kinds::OP_FAILED };
        let summary = log.map(|l| l.what.clone()).unwrap_or_else(|| format!("{} {}", action.op, if action.code == code::OK { "done" } else { "failed" }));
        let project = action.targets.projects.first().cloned().or_else(|| action.session.as_deref().and_then(|s| self.journal_key(s)));
        let data = json!({ "action": action, "log": log });
        self.journal_note(Event::new(kind, Source::Control, summary).session(action.session.as_deref()).project(project.as_ref()).data(data));
    }


    /// A request waits for the person, or they answered it.
    pub fn journal_approval(&mut self, id: u64, caller: &Caller, what: &str, kind_v1: Kind, answer: Option<bool>) {
        if !self.journal.on {
            return;
        }
        let (kind, summary) = match answer {
            None => (kinds::APPROVAL_ASKED, format!("{} to {what}", glasscode_core::caller::who_does(&caller.describe(), "ask"))),
            // The person's own command, confirmed in the app (a shell tab's human-only op).
            Some(true) if caller.describe() == "You" => (kinds::APPROVAL_ALLOWED, format!("you confirmed: {what}")),
            Some(false) if caller.describe() == "You" => (kinds::APPROVAL_DENIED, format!("you did not confirm: {what}")),
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
        // A new time zone is said once (M41 step 1): schedules follow it.
        if let Some(zone) = clocks.iter().find(|e| e.kind == kinds::CLOCK_ZONE) {
            let (before, now) = (zone.data["before"].as_i64().unwrap_or_default(), zone.data["now"].as_i64().unwrap_or_default());
            self.show_toast(glasscode_core::clock::zone_note(before, now), cx);
        }
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
        self.control_keys_tick(cx);
        self.journal_work_tick(cx);
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
        let mut gone = Vec::new();
        for event in reconciler.run(&snapshot) {
            if event.kind == journal::kinds::RECONCILED_WORKTREE_GONE
                && let Some(path) = event.data.get("worktree").and_then(|v| v.as_str())
            {
                gone.push(std::path::PathBuf::from(path));
            }
            journal::append(event);
        }
        self.journal.reconciler = Some(reconciler);
        // A worktree that went outside Glasscode: memory archives the work that names it (M24).
        if !gone.is_empty() {
            std::thread::spawn(move || {
                for path in gone {
                    if let Err(err) = glasscode_core::memstore::Memory::open().archive_gone(&path, glasscode_core::clock::now_ms()) {
                        log::warn!("memory: could not archive the work of {}: {err}", path.display());
                    }
                }
            });
        }
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
        // Each project's list as git last read it: one that is not read waits alone.
        let worktrees = Some(
            self.state
                .projects
                .iter()
                .map(|p| reconcile::ProjectWorktrees {
                    project: self.control.project_keys.get(&p.id).cloned(),
                    name: p.name.clone(),
                    list: match self.worktree_reads.get(&p.id) {
                        Some(Ok(())) => Ok(self.worktrees.get(&p.id).map(|list| list.iter().map(|w| w.path.clone()).collect()).unwrap_or_default()),
                        Some(Err(why)) => Err(Some(why.clone())),
                        None => Err(None),
                    },
                })
                .collect(),
        );
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
                let here = this.journal.machine.clone();
                for event in events {
                    if let Some(reconciler) = this.journal.reconciler.as_mut() {
                        reconciler.observe(&event);
                    }
                    // The server's own sessions' turns and changes give their work's facts here
                    // (M32's Server line); what this Mac sent the server is not taken twice.
                    if event.session.as_ref().is_some_and(|s| this.journal.server_sessions.contains(s)) && here.as_ref() != Some(&event.at.machine) {
                        this.journal_work_observe(&event);
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
        // D17: not a project bound to another account than the manager's.
        Channel::Manager { .. } => {
            let set = ws.control_policy_set();
            Scope::Projects(ws.control.project_keys.values().filter(|key| set.manager_may_read(key)).cloned().collect())
        }
        other => Scope::of(other, &[]),
    }
}

/// `10m`, `2h`, `3d`, `45s`, or milliseconds since 1970: the time from which.
fn since_ms(text: &str, now_ms: u64) -> Option<u64> {
    glasscode_core::ops::core_reads::since_ms(text, now_ms)
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
    glasscode_core::ops::core_reads::journal_filter(project, op.text("since"), op.text("session"), op.text("kind"))
}

/// `14:02` today, else `Nov 18 14:02`.
fn when(at_ms: u64) -> String {
    let secs = at_ms / 1000;
    let day = glasscode_core::day::local(secs as i64);
    if day == glasscode_core::day::today() { crate::commands::clock(secs) } else { format!("{} {}", glasscode_core::day::short(day), crate::commands::clock(secs)) }
}

/// One event for a person: `14:02  session.needs_input  [8765f51a]  Claude needs your permission`.
pub(crate) fn event_line(event: &Event) -> String {
    glasscode_core::ops::core_reads::event_line(event)
}

/// A row of `journal query`: the fields of `log --json` (`LogEntry`), and the event's own.
pub(crate) fn row(event: &Event) -> Value {
    glasscode_core::ops::core_reads::row(event)
}

/// A row for a person, as `log` printed its lines.
pub(crate) fn row_line(row: &Value) -> String {
    glasscode_core::ops::core_reads::row_line(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An event of a session with no work yet makes its work before it is observed: a first turn
    /// that ends within the work round's 5 s keeps its facts (found in the journey run).
    #[test]
    fn a_first_turn_reaches_its_work() {
        let source = include_str!("journal_ui.rs");
        let body = &source[source.find("fn journal_work_observe(").unwrap()..];
        let body = &body[..body.find("\n    }\n").unwrap()];
        assert!(body.find("ledger.ensure(").unwrap() < body.find("ledger.observe(").unwrap());
    }

    /// A turn that starts and ends in one read of its hook lines (a short first turn) is reported
    /// from its transcript: the transcript's path from those lines is stored before the turn hooks
    /// are read (found in the journey run: such a turn was "0 tools", partial).
    #[test]
    fn a_first_turn_knows_its_transcript() {
        let workspace = include_str!("workspace.rs");
        let at = workspace.find("EventsTail::poll").unwrap();
        let after = &workspace[at..];
        let stored = after.find("session.transcript_path = Some(path)").unwrap();
        let hooks = after.find("self.journal_turn_hooks(").unwrap();
        assert!(stored < hooks, "the transcript's path, then the turn hooks");
    }

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
