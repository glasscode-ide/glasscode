//! The daemon: owns the server's projects, accounts and sessions (the same `state.json` model
//! as the Mac app), runs sessions in tmux, follows their activity, brings them back after a
//! reboot, and answers clients on a unix socket.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, anyhow, bail};
use glasscode_core::agents::{self, AgentActivity, EventsTail};
use glasscode_core::git::{self, Worktree};
use glasscode_core::model::{Account, AgentKind, Project, ReviewRef, Session, State, new_id, now};
use glasscode_core::remote::{
    AddProject, Attach, ConversationFile, CreateSession, Hello, MoveTag, PROTOCOL_VERSION, ReceiveWorktree, RemoteSession, Request, Response,
    ServerState, StartFrom, WorktreeSnapshot,
};
use glasscode_core::owners::WorktreeKey;
use glasscode_core::prompt::{self, PendingPrompt};
use glasscode_core::remote::PhoneKey;
use glasscode_core::caller::{self, Caller};
use glasscode_core::journal::{self, Event, Source};
use glasscode_core::paths::ProjectKey;
use glasscode_core::reconcile::{self, Reconciler};
use glasscode_core::turns::{self, Change, Signal};
use glasscode_core::{github, history, paths, store, sync, transcript, versions};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tmux::{self, Tmux};

/// Which sessions should be running, and their tmux names; survives daemon restarts and
/// server reboots (`~/.glasscode/glasscoded.json`).
#[derive(Default, Serialize, Deserialize)]
struct Runtime {
    #[serde(default)]
    running: BTreeSet<String>,
    #[serde(default)]
    names: HashMap<String, String>,
    /// Sessions made for a move that has not committed yet: their agents wait (`owners`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    held: BTreeMap<String, MoveTag>,
    /// Sessions stopped because their worktree is moving away: they start again if the move is
    /// given up (session id → the worktree and the epoch of that move).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    stopped_for_move: BTreeMap<String, (WorktreeKey, u64)>,
    /// Copies of worktrees a Mac left at a switch without moving them, by key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    parked: BTreeMap<WorktreeKey, glasscode_core::remote::ParkedWorktree>,
}

pub struct Daemon {
    state: State,
    runtime: Runtime,
    pub env: HashMap<String, String>,
    tmux: Tmux,
    activity: HashMap<String, AgentActivity>,
    tails: HashMap<String, EventsTail>,
    started: HashMap<String, SystemTime>,
    worktrees: HashMap<String, Vec<Worktree>>,
    worktrees_at: Instant,
    /// Questions on the agents' screens, read every couple of seconds.
    prompts: HashMap<String, PendingPrompt>,
    prompts_at: Instant,
    /// The last reply per session, with the conversation file's time it was read at.
    replies: HashMap<String, (PathBuf, SystemTime, Option<String>)>,
    /// Pull requests by worktree, kept fresh by `pr_loop`.
    prs: HashMap<PathBuf, github::PrSummary>,
    /// Why GitHub is not usable here (no gh, not signed in), if so.
    github: Option<String>,
    /// Each project's GitHub repository, by project id (read with the worktrees).
    repos: HashMap<String, String>,
    /// Needs already sent to phones as notifications.
    announced: crate::push::Announced,
    /// The first screen reading after a start only records what is open (no notifications).
    primed: bool,
    dirty: bool,
    /// Mac sessions synced here (`crate::mirror`).
    pub mirrors: HashMap<String, crate::mirror::Mirror>,
    /// Sessions whose agent lost its login (read off their screens).
    signed_out: BTreeSet<String>,
    /// This server's own journal (M14), in a v2 home only.
    journal: Journal,
}

/// glasscoded's journal (M14): it keeps its own, in its own home, and the Mac pulls it
/// (`journal.since`). Off with the engine at v1: then nothing here runs.
#[derive(Default)]
struct Journal {
    on: bool,
    intakes: HashMap<String, journal::HookIntake>,
    /// Where each session's spool was read up to, kept so a restart reads on from there.
    cursors: journal::Cursors,
    cursors_dirty: bool,
    cursors_at: Option<Instant>,
    reconciler: Reconciler,
    reconciled_at: Option<Instant>,
    clock: journal::ClockWatch,
    /// The activity last written for each session.
    activity: HashMap<String, AgentActivity>,
    /// Project id to its key on this machine.
    keys: BTreeMap<String, ProjectKey>,
    /// Each session's turns (M30), through its hooks and tmux; a Codex session's through its rollout.
    turns: HashMap<String, turns::TurnMachine>,
    rollouts: HashMap<String, (PathBuf, agents::RolloutTail)>,
}

impl Journal {
    /// At the start: whether the journal runs here, where each spool was read up to, and what the
    /// last two days of the journal imply (for the reconciler).
    fn load() -> Journal {
        if !journal::init() {
            return Journal::default();
        }
        use glasscode_core::store::Store as _;
        let since = journal::Filter { since_ms: Some(glasscode_core::clock::now_ms().saturating_sub(2 * 86_400_000)), ..Default::default() };
        let recent = journal::query(&since, &journal::Scope::All, 50_000);
        // Turns the journal leaves open: their agents run on in tmux across a daemon restart.
        let mut machines: HashMap<String, turns::TurnMachine> = HashMap::new();
        for (session, turn) in turns::open_in(&recent) {
            machines.entry(session).or_default().restore(turn);
        }
        Journal { on: true, cursors: journal::Cursors::load().unwrap_or_default(), reconciler: Reconciler::from_events(&recent), keys: project_keys(), turns: machines, ..Journal::default() }
    }
}

/// This machine's bindings of project ids to keys; none when they cannot be read.
fn project_keys() -> BTreeMap<String, ProjectKey> {
    use glasscode_core::store::Store as _;
    let read = || -> Result<_, glasscode_core::store::StoreError> { Ok(glasscode_core::identity::Projects::load()?.bound(&glasscode_core::paths::MachineId::this()?)) };
    read().unwrap_or_default()
}

/// Wakes the pull request loop early (after an action, or when a phone asks).
static PR_KICK: std::sync::OnceLock<std::sync::mpsc::Sender<()>> = std::sync::OnceLock::new();

fn kick_prs() {
    if let Some(kick) = PR_KICK.get() {
        let _ = kick.send(());
    }
}

pub type Shared = Arc<Mutex<Daemon>>;

/// What the archive of a worktree's work needs from this server (M24): its project's key here,
/// its sessions' work and conversations. None when the engine is v1 here, or the project has no
/// key on this machine.
fn archive_input(daemon: &Daemon, project_id: &str, worktree: &git::Worktree) -> Option<glasscode_core::memstore::archive::ArchiveInput> {
    use glasscode_core::memstore::archive::{ArchiveInput, Conversation, Ended};
    use glasscode_core::store::{Engine, EngineSwitch};
    if EngineSwitch::current().engine != Engine::V2 {
        return None;
    }
    let key = daemon.journal.keys.get(project_id)?.clone();
    let sessions: Vec<&glasscode_core::model::Session> = daemon.state.sessions.iter().filter(|s| s.worktree == worktree.path).collect();
    let mut input = ArchiveInput::new(key.clone(), Ended::RemovedOnServer, glasscode_core::clock::now_ms());
    input.worktree = Some(worktree.path.clone());
    input.branch = (!worktree.branch.is_empty()).then(|| worktree.branch.clone());
    input.work = match sessions.first() {
        Some(s) => Some(daemon.state.work_key(&key, s)),
        None => input.branch.clone().map(|id| glasscode_core::identity::WorkKey { project: key.clone(), kind: glasscode_core::identity::WorkKind::Branch, id }),
    };
    for s in &sessions {
        if let Some(path) = &s.transcript_path {
            input.conversations.push(Conversation { agent: s.agent, path: path.clone(), digest: None });
        }
        input.sessions.push(s.id.clone());
    }
    Some(input)
}

pub fn log(message: impl AsRef<str>) {
    let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    eprintln!("[{secs}] {}", message.as_ref());
}

/// The environment sessions start with: the user's login shell environment (PATH with
/// `~/.local/bin`, nvm, …), without anything that belongs to this SSH connection or a parent
/// tmux, plus true color and a UTF-8 locale.
pub fn session_env() -> HashMap<String, String> {
    glasscode_core::env::scrub_process_env();
    let mut env = glasscode_core::env::login_shell_env();
    env.retain(|key, value| {
        !key.starts_with("SSH_") && !key.starts_with("TMUX") && !key.starts_with("BASH_FUNC_") && key != "GLASSCODE_SESSION_ID" && !value.contains('\n')
    });
    env.insert("COLORTERM".into(), "truecolor".into());
    if !env.contains_key("LANG") && !env.contains_key("LC_ALL") {
        env.insert("LANG".into(), "C.UTF-8".into());
    }
    env
}

impl Daemon {
    pub fn load(env: HashMap<String, String>) -> anyhow::Result<Daemon> {
        paths::ensure_dirs()?;
        let mut state = store::load();
        state.bootstrap_accounts(false);
        state.active_session = None;
        let runtime: Runtime = std::fs::read(paths::daemon_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let tmux = Tmux::new(&paths::root());
        tmux.write_config()?;
        let mut daemon = Daemon {
            state,
            runtime,
            env,
            tmux,
            activity: HashMap::new(),
            tails: HashMap::new(),
            started: HashMap::new(),
            worktrees: HashMap::new(),
            worktrees_at: Instant::now() - Duration::from_secs(3600),
            prompts: HashMap::new(),
            prompts_at: Instant::now(),
            replies: HashMap::new(),
            prs: HashMap::new(),
            repos: HashMap::new(),
            github: None,
            announced: Default::default(),
            primed: false,
            mirrors: crate::mirror::load(),
            signed_out: BTreeSet::new(),
            journal: Journal::load(),
            dirty: true,
        };
        daemon.refresh_worktrees();
        daemon.save();
        daemon.note(Event::new(journal::kinds::APP_STARTED, Source::Daemon, format!("glasscoded {} started", env!("CARGO_PKG_VERSION"))).data(json!({ "version": env!("CARGO_PKG_VERSION"), "build": glasscode_core::update::BUILD })));
        Ok(daemon)
    }

    /// Write an event to this server's journal (when it runs), and fold it into what the
    /// reconciler knows.
    fn note(&mut self, event: Event) {
        if self.journal.on {
            self.journal.reconciler.observe(&event);
            journal::append(event);
        }
    }

    /// A session's project key here, for its events.
    fn key_of(&self, session_id: &str) -> Option<ProjectKey> {
        let project = &self.state.session(session_id)?.project_id;
        self.journal.keys.get(project).cloned()
    }

    /// Feed a signal to a session's turns: `turn.started` for one that opens, and the report of
    /// one that closes, read on a thread of its own once the transcript settles.
    fn turn_signal(&mut self, id: &str, signal: Signal, prompt: Option<String>) {
        if !self.journal.on {
            return;
        }
        let now = glasscode_core::clock::now_ms();
        let changes = self.journal.turns.entry(id.to_string()).or_default().feed(now, &signal);
        for change in changes {
            let key = self.key_of(id);
            let Some(session) = self.state.session(id).cloned() else {
                // Its session is gone: the turn still gets its report, from what its end said.
                if let Change::Closed(closed) = change {
                    let gathered = turns::Gathered { partial: true, ..turns::Gathered::default() };
                    let mut event = turns::ended_event(key.as_ref(), &turns::report(id, &closed, &gathered));
                    event.source = Source::Daemon;
                    journal::append(event);
                }
                continue;
            };
            match change {
                Change::Opened(turn) => {
                    let offset = turns::transcript_len(session.transcript_path.as_ref());
                    let head = glasscode_core::git_info::heads(&session.worktree).and_then(|h| h.head);
                    if let Some(machine) = self.journal.turns.get_mut(id) {
                        machine.set_baseline(&turn.id, offset, head.clone());
                    }
                    let turn = turns::OpenTurn { transcript_offset: offset, head, ..turn };
                    let mut event = turns::started_event(id, key.as_ref(), &turn, prompt.as_deref());
                    event.source = Source::Daemon;
                    self.note(event);
                }
                Change::Closed(closed) => {
                    let transcript = self.transcript_file(&session);
                    std::thread::spawn(move || {
                        let gathered = turns::gather(session.agent, transcript.as_deref(), &closed, &session.worktree, true);
                        let mut event = turns::ended_event(key.as_ref(), &turns::report(&session.id, &closed, &gathered));
                        event.source = Source::Daemon;
                        journal::append(event);
                    });
                }
            }
        }
    }

    /// The turns' part of a tick: each running Codex session's rollout.
    fn turns_tick(&mut self) {
        let codex: Vec<(String, PathBuf)> = self
            .state
            .sessions
            .iter()
            .filter(|s| s.agent == AgentKind::Codex && self.runtime.running.contains(&s.id))
            .filter_map(|s| Some((s.id.clone(), s.transcript_path.clone()?)))
            .collect();
        for (id, path) in codex {
            let tail = self.journal.rollouts.entry(id.clone()).or_insert_with(|| (path.clone(), agents::RolloutTail::resumed(&path).keeping_events()));
            if tail.0 != path {
                *tail = (path.clone(), agents::RolloutTail::resumed(&path).keeping_events());
            }
            tail.1.poll();
            for line in tail.1.take_events() {
                self.turn_signal(&id, Signal::Rollout(line), None);
            }
        }
        let running = &self.runtime.running;
        self.journal.rollouts.retain(|id, _| running.contains(id));
    }

    /// A session's tail of its hook spool: from where the journal last read it (so what it wrote
    /// while this daemon was down is read too), else from its end.
    fn tail_for(&self, id: &str) -> EventsTail {
        if self.journal.on { EventsTail::resume(id, self.journal.cursors.spools.get(id).copied()) } else { EventsTail::new(id) }
    }

    /// The journal's part of a tick: hook lines, activity changes, the clocks, the reconciler.
    fn journal_tick(&mut self) {
        if !self.journal.on {
            return;
        }
        let now = glasscode_core::clock::now_ms();
        let woke = self.journal.clock.observe(now, Instant::now());
        let wake = woke.iter().any(|e| e.kind == journal::kinds::APP_WOKE);
        let slept = woke.iter().any(|e| e.kind == journal::kinds::APP_SLEPT);
        for event in woke {
            self.note(event);
        }
        if slept {
            let ids: Vec<String> = self.journal.turns.keys().cloned().collect();
            for id in ids {
                self.turn_signal(&id, Signal::Slept, None);
            }
        }
        self.turns_tick();
        // A turn the journal left open for a session that no longer runs here: aborted.
        let gone: Vec<String> = self.journal.turns.iter().filter(|(id, m)| m.open().is_some() && !self.runtime.running.contains(*id)).map(|(id, _)| id.clone()).collect();
        for id in gone {
            self.turn_signal(&id, Signal::Exited(None), None);
        }
        let state = &self.state;
        self.journal.turns.retain(|id, _| state.session(id).is_some());
        let changed: Vec<(String, Option<AgentActivity>, AgentActivity)> =
            self.activity.iter().filter(|(id, now)| self.journal.activity.get(*id) != Some(now)).map(|(id, now)| (id.clone(), self.journal.activity.get(id).copied(), *now)).collect();
        for (id, previous, now) in changed {
            self.journal.activity.insert(id.clone(), now);
            let mut event = journal::activity(&id, self.key_of(&id).as_ref(), previous, now);
            event.source = Source::Daemon;
            self.note(event);
        }
        self.journal.activity.retain(|id, _| self.activity.contains_key(id));
        if wake || self.journal.reconciled_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(reconcile::EVERY_SECS)) {
            self.journal.reconciled_at = Some(Instant::now());
            let snapshot = self.reconcile_snapshot();
            for event in self.journal.reconciler.run(&snapshot) {
                journal::append(event);
            }
        }
        if self.journal.cursors_dirty && self.journal.cursors_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(10)) {
            use glasscode_core::store::Store as _;
            self.journal.cursors_at = Some(Instant::now());
            self.journal.cursors_dirty = false;
            let state = &self.state;
            self.journal.cursors.spools.retain(|id, _| state.session(id).is_some());
            self.journal.intakes.retain(|id, _| state.session(id).is_some());
            if let Err(err) = self.journal.cursors.save() {
                log(format!("journal: could not keep the spool cursors: {err}"));
            }
        }
    }

    /// What is true now, for the reconciler: this server's sessions, their activity and pull
    /// requests, and the worktrees of its projects.
    fn reconcile_snapshot(&self) -> reconcile::Snapshot {
        let sessions = self
            .state
            .sessions
            .iter()
            .map(|s| reconcile::SessionNow {
                id: s.id.clone(),
                project: self.journal.keys.get(&s.project_id).cloned(),
                running: self.runtime.running.contains(&s.id),
                activity: self.activity.get(&s.id).copied(),
                pr: self.prs.get(&s.worktree).and_then(|pr| Some((self.repos.get(&s.project_id)?.clone(), pr.number, pr.state.clone()))),
            })
            .collect();
        let worktrees = self.worktrees.iter().flat_map(|(project, list)| list.iter().map(move |w| (project.clone(), w.path.clone()))).map(|(project, path)| (self.journal.keys.get(&project).cloned(), path)).collect();
        reconcile::Snapshot { sessions, worktrees }
    }

    fn save(&mut self) {
        if let Err(err) = store::save(&self.state) {
            log(format!("could not save state: {err}"));
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.runtime) {
            let _ = std::fs::write(paths::daemon_file(), bytes);
        }
        self.dirty = false;
    }

    fn refresh_worktrees(&mut self) {
        self.worktrees_at = Instant::now();
        self.worktrees = self
            .state
            .projects
            .iter()
            .map(|p| (p.id.clone(), git::worktrees(&p.root).unwrap_or_default()))
            .collect();
        self.repos = self.state.projects.iter().filter_map(|p| Some((p.id.clone(), github::repo_slug(&p.root)?))).collect();
    }

    fn tmux_name(&mut self, session: &Session) -> String {
        let project = self.state.project(&session.project_id).map(|p| p.name.clone()).unwrap_or_default();
        self.runtime.names.entry(session.id.clone()).or_insert_with(|| tmux::session_name(&project, &session.branch, &session.id)).clone()
    }

    /// Start (or resume) a session's agent in tmux. Already running is fine.
    fn start(&mut self, id: &str) -> anyhow::Result<()> {
        let session = self.state.session(id).cloned().ok_or_else(|| anyhow!("no session {id}"))?;
        if self.runtime.held.contains_key(id) {
            bail!("{} starts when its move to this server finishes", session.title);
        }
        let name = self.tmux_name(&session);
        if self.tmux.has(&name) {
            // Still running from before a daemon restart: follow its activity again.
            if !self.tails.contains_key(id) {
                let tail = self.tail_for(id);
                self.tails.insert(id.into(), tail);
            }
            self.runtime.running.insert(id.into());
            return Ok(());
        }
        // A new agent starts only in a worktree this server owns (unreadable records: none).
        let key = self.worktree_key(&session);
        if !crate::owners::may_run_here(&key) {
            bail!("{} cannot run here: {}", session.title, crate::owners::why_not_here(&key));
        }
        if !session.worktree.is_dir() {
            bail!("folder {} does not exist on the server", session.worktree.display());
        }
        let account = session.account_id.as_deref().and_then(|a| self.state.account(a)).cloned();
        if let Some(dir) = account.as_ref().and_then(Account::config_dir) {
            std::fs::create_dir_all(&dir)?;
        }
        // The same session profiles as on the Mac (M22), with the engine at v2 and "profiles" on.
        let profile = server_profile(&session, &self.state);
        let mut launch = agents::launch_with(&session, account.as_ref(), &self.env, profile.as_ref());
        if let Some(profile) = &profile {
            agents::apply_profile_env(&mut launch, profile);
        }
        // The `glasscode` command: which session it runs in (signed), and where to leave
        // requests when a sandbox keeps it from the socket.
        launch.env.insert("GLASSCODE_SESSION_SIG".into(), crate::today::signature(&session.id));
        launch.env.insert("GLASSCODE_CONTROL_SOCKET".into(), glasscode_core::control::socket_path().to_string_lossy().into_owned());
        launch.env.insert("GLASSCODE_WORKTREE".into(), session.worktree.to_string_lossy().into_owned());
        if session.agent != AgentKind::Shell && !Path::new(&launch.program).is_file() {
            bail!("{} is not installed on the server (or not on the login shell's PATH)", launch.program);
        }
        self.tmux.start(&name, &session.worktree, &launch.env, &launch.program, &launch.args)?;
        let tail = self.tail_for(id);
        self.tails.insert(id.into(), tail);
        let started = Event::new(journal::kinds::SESSION_STARTED, Source::Daemon, format!("{} started in {}", session.agent.name(), session.worktree.display())).session(Some(id)).project(self.key_of(id).as_ref()).data(json!({ "agent": session.agent.name() }));
        self.note(started);
        self.started.insert(id.into(), SystemTime::now());
        self.runtime.running.insert(id.into());
        self.dirty = true;
        log(format!("started {name} ({} in {})", session.agent.name(), session.worktree.display()));
        Ok(())
    }

    fn stop(&mut self, id: &str) {
        if let Some(name) = self.runtime.names.get(id).cloned() {
            self.tmux.kill(&name);
        }
        self.runtime.running.remove(id);
        self.activity.remove(id);
        self.tails.remove(id);
        self.dirty = true;
    }

    /// The owner-record key of a session's worktree on this server.
    pub fn worktree_key(&self, session: &Session) -> WorktreeKey {
        let project = self.state.project(&session.project_id).map(|p| p.name.clone()).unwrap_or_default();
        WorktreeKey::of(&session.worktree, &paths::home(), &project, Some(&session.branch))
    }

    /// The session made earlier for this step of a move, if any (a retry after a lost reply).
    fn held_session(&self, tag: &MoveTag) -> Option<String> {
        self.runtime.held.iter().find(|(_, t)| *t == tag).map(|(id, _)| id.clone())
    }

    /// A move to this server committed: its held sessions start, resuming their conversations.
    /// Returns the titles of the ones that could not start (they start when opened). A parked
    /// copy of the worktree is out of date now, so it goes.
    pub fn start_held(&mut self, key: &WorktreeKey, epoch: u64) -> Vec<String> {
        let ids: Vec<String> = self.runtime.held.iter().filter(|(_, t)| &t.key == key && t.epoch == epoch).map(|(id, _)| id.clone()).collect();
        let mut not_started = Vec::new();
        for id in ids {
            self.runtime.held.remove(&id);
            if let Err(err) = self.start(&id) {
                log(format!("could not start moved session {id}: {err:#}"));
                not_started.push(self.state.session(&id).map(|s| s.title.clone()).unwrap_or(id));
            }
        }
        self.runtime.parked.remove(key);
        self.save();
        not_started
    }

    /// The worktree `key` starts moving away: its sessions stop, and are remembered in case the
    /// move is given up.
    pub fn stop_for_move(&mut self, key: &WorktreeKey, epoch: u64) {
        let ids: Vec<String> = self.state.sessions.iter().filter(|s| &self.worktree_key(s) == key && self.runtime.running.contains(&s.id)).map(|s| s.id.clone()).collect();
        for id in ids {
            self.stop(&id);
            self.runtime.stopped_for_move.insert(id, (key.clone(), epoch));
        }
        self.save();
    }

    /// The move of `key` away from here ended: committed (they stay stopped, until the Mac
    /// closes them) or given up (they start again).
    pub fn after_move_away(&mut self, key: &WorktreeKey, epoch: u64, committed: bool) {
        let ids: Vec<String> = self.runtime.stopped_for_move.iter().filter(|(_, (k, e))| k == key && *e == epoch).map(|(id, _)| id.clone()).collect();
        for id in ids {
            self.runtime.stopped_for_move.remove(&id);
            if !committed {
                if let Err(err) = self.start(&id) {
                    log(format!("could not restart session {id} after a move was given up: {err:#}"));
                }
            }
        }
        self.save();
    }

    /// A move was given up: the sessions made for it go.
    pub fn drop_held(&mut self, key: &WorktreeKey, epoch: u64) {
        let ids: Vec<String> = self.runtime.held.iter().filter(|(_, t)| &t.key == key && t.epoch == epoch).map(|(id, _)| id.clone()).collect();
        for id in &ids {
            self.runtime.held.remove(id);
            self.runtime.names.remove(id);
            self.state.sessions.retain(|s| &s.id != id);
        }
        if !ids.is_empty() {
            self.save();
        }
    }

    /// After a reboot (or a crash of tmux), start every session that was running, resuming
    /// its conversation.
    pub fn restore(&mut self) {
        let ids: Vec<String> = self.runtime.running.iter().cloned().collect();
        for id in ids {
            if self.state.session(&id).is_none() {
                self.runtime.running.remove(&id);
                continue;
            }
            if let Err(err) = self.start(&id) {
                log(format!("could not bring back session {id}: {err}"));
                self.runtime.running.remove(&id);
            }
        }
        self.save();
    }

    /// Once a second: notice ended sessions, read hook events, find Codex conversation ids.
    pub fn tick(&mut self) {
        crate::home::tick();
        for (key, epoch) in crate::owners::expire() {
            log(format!("a move of {} made no progress; it was given up", key.0));
            self.drop_held(&key, epoch);
            self.after_move_away(&key, epoch, false);
        }
        let live: BTreeSet<String> = self.tmux.sessions().into_iter().collect();
        for id in self.runtime.running.clone() {
            let alive = self.runtime.names.get(&id).is_some_and(|n| live.contains(n));
            if !alive {
                log(format!("session {id} ended"));
                self.runtime.running.remove(&id);
                self.activity.remove(&id);
                self.tails.remove(&id);
                self.dirty = true;
                let ended = Event::new(journal::kinds::SESSION_ENDED, Source::Daemon, "its agent ended").session(Some(&id)).project(self.key_of(&id).as_ref()).data(json!({ "reason": "exit" }));
                self.note(ended);
                // Its tmux session is gone: a turn still open aborted (tmux keeps no status).
                self.turn_signal(&id, Signal::Exited(None), None);
            }
        }
        let ids: Vec<String> = self.tails.keys().cloned().collect();
        for id in ids {
            let Some(update) = self.tails.get_mut(&id).and_then(EventsTail::poll) else { continue };
            if self.journal.on {
                let key = self.key_of(&id);
                let events = self.journal.intakes.entry(id.clone()).or_default().events(&id, key.as_ref(), &update.lines);
                for event in events {
                    self.note(event);
                }
                for line in &update.lines {
                    let Ok(payload) = serde_json::from_str::<Value>(line) else { continue };
                    let prompt = (payload.get("hook_event_name").and_then(Value::as_str) == Some("UserPromptSubmit")).then(|| payload.get("prompt").and_then(Value::as_str).map(str::to_string)).flatten();
                    self.turn_signal(&id, Signal::Hook(payload), prompt);
                }
                if let Some(offset) = self.tails.get(&id).map(EventsTail::offset) {
                    self.journal.cursors.spools.insert(id.clone(), offset);
                    self.journal.cursors_dirty = true;
                }
            }
            if let Some(activity) = update.activity {
                self.activity.insert(id.clone(), activity);
            }
            if let Some(session) = self.state.session_mut(&id) {
                if let Some(conversation) = update.conversation_id {
                    if session.agent_session_id.as_deref() != Some(conversation.as_str()) {
                        session.agent_session_id = Some(conversation);
                        self.dirty = true;
                    }
                }
                if let Some(path) = update.transcript_path {
                    session.transcript_path = Some(path);
                    self.dirty = true;
                }
                if update.activity == Some(AgentActivity::Working) && session.initial_prompt.is_some() {
                    session.initial_prompt = None;
                    self.dirty = true;
                }
            }
        }
        // Codex chooses its conversation id itself; find it in the new rollout file.
        let waiting: Vec<Session> = self
            .state
            .sessions
            .iter()
            .filter(|s| s.agent == AgentKind::Codex && s.agent_session_id.is_none() && self.runtime.running.contains(&s.id))
            .cloned()
            .collect();
        for session in waiting {
            let Some(account) = session.account_id.as_deref().and_then(|a| self.state.account(a)).cloned() else { continue };
            let since = self.started.get(&session.id).copied().unwrap_or(SystemTime::UNIX_EPOCH) - Duration::from_secs(5);
            if let Some((conversation, path)) = agents::find_codex_conversation(&account.effective_dir(), &session.worktree, since) {
                if let Some(s) = self.state.session_mut(&session.id) {
                    s.agent_session_id = Some(conversation);
                    s.transcript_path = Some(path);
                    s.initial_prompt = None;
                    self.dirty = true;
                }
            }
        }
        // Codex has no hooks: a conversation file written in the last seconds means it is
        // working (used to wait before moving a session).
        let codex: Vec<(String, Option<PathBuf>)> = self
            .state
            .sessions
            .iter()
            .filter(|s| s.agent == AgentKind::Codex && self.runtime.running.contains(&s.id))
            .map(|s| (s.id.clone(), s.transcript_path.clone()))
            .collect();
        for (id, path) in codex {
            let recent = path
                .and_then(|p| std::fs::metadata(p).ok())
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < Duration::from_secs(6));
            self.activity.insert(id, if recent { AgentActivity::Working } else { AgentActivity::Idle });
        }
        if self.prompts_at.elapsed() >= Duration::from_secs(2) {
            self.prompts_at = Instant::now();
            self.read_screens();
        }
        if self.worktrees_at.elapsed() > Duration::from_secs(15) {
            self.refresh_worktrees();
            if self.journal.on {
                self.journal.keys = project_keys();
            }
        }
        self.journal_tick();
        if self.dirty {
            self.save();
        }
    }

    /// Read the open question off each idle agent's screen, and its last reply.
    fn read_screens(&mut self) {
        let running: Vec<Session> = self.state.sessions.iter().filter(|s| self.runtime.running.contains(&s.id)).cloned().collect();
        for session in running {
            let busy = self.activity.get(&session.id) == Some(&AgentActivity::Working) && session.agent == AgentKind::Claude;
            let screen = match (busy, self.runtime.names.get(&session.id)) {
                (false, Some(name)) if session.agent != AgentKind::Shell => self.tmux.screen(name).ok(),
                _ => None,
            };
            if let Some(screen) = &screen {
                if prompt::signed_out(screen) {
                    self.signed_out.insert(session.id.clone());
                } else {
                    self.signed_out.remove(&session.id);
                }
            }
            let found = screen.as_deref().and_then(prompt::parse);
            match found {
                Some(found) => {
                    // Codex has no hook for this; its screen is how we know it waits.
                    self.activity.insert(session.id.clone(), AgentActivity::NeedsInput);
                    self.prompts.insert(session.id.clone(), found);
                }
                None => {
                    self.prompts.remove(&session.id);
                }
            }
            if let Some(path) = self.transcript_file(&session) {
                let modified = std::fs::metadata(&path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                let fresh = self.replies.get(&session.id).is_some_and(|(p, t, _)| *p == path && *t == modified);
                if !fresh {
                    let reply = transcript::last_reply(session.agent, &path);
                    self.replies.insert(session.id.clone(), (path, modified, reply));
                }
            }
        }
        crate::mirror::refresh_replies(&mut self.mirrors);
        self.announce_needs();
    }

    /// Tell paired phones (through Apple's push service) about each new thing that needs them.
    fn announce_needs(&mut self) {
        let mut notes = Vec::new();
        let mut waiting = 0;
        let running: Vec<Session> = self.state.sessions.iter().filter(|s| self.runtime.running.contains(&s.id)).cloned().collect();
        for session in running {
            let title = display_title(&session);
            let need = if let Some(prompt) = self.prompts.get(&session.id) {
                Some((format!("p:{}:{}", prompt.question, prompt.detail.join(" ")), describe_prompt(session.agent, prompt), "permission"))
            } else {
                let working = self.activity.get(&session.id) == Some(&AgentActivity::Working);
                let reply = self.replies.get(&session.id).and_then(|(_, _, r)| r.clone()).unwrap_or_default();
                let reply = reply.trim();
                (!working && reply.ends_with('?')).then(|| (format!("q:{reply}"), format!("{}: \u{201c}{}\u{201d}", session.agent.name(), last_sentence(reply)), "question"))
            };
            match need {
                Some((key, body, category)) => {
                    waiting += 1;
                    if self.announced.fresh(&session.id, &key) {
                        notes.push((title, body, session.id.clone(), category));
                    }
                }
                None => self.announced.clear(&session.id),
            }
        }
        if !std::mem::replace(&mut self.primed, true) {
            return;
        }
        for (title, body, session_id, category) in notes {
            crate::push::send(crate::push::Note { title, body, session_id, category, badge: waiting });
        }
    }

    /// The agent's conversation file for a session, when it has one yet.
    fn transcript_file(&self, session: &Session) -> Option<PathBuf> {
        if let Some(path) = session.transcript_path.as_ref().filter(|p| p.is_file()) {
            return Some(path.clone());
        }
        let conversation = session.agent_session_id.as_deref()?;
        let account = session.account_id.as_deref().and_then(|a| self.state.account(a))?;
        let dir = account.effective_dir();
        match session.agent {
            AgentKind::Claude => agents::find_claude_transcript(&dir, conversation),
            AgentKind::Codex => agents::find_codex_rollout(&dir, conversation),
            AgentKind::Shell => None,
        }
    }

    /// Whether `path` is one of this server's checkouts (the phone may only touch those).
    fn known_checkout(&self, path: &Path) -> bool {
        self.state.projects.iter().any(|p| p.root == path) || self.worktrees.values().flatten().any(|w| w.path == path)
    }

    pub fn has_session(&self, id: &str) -> bool {
        self.state.session(id).is_some()
    }

    pub fn project(&self, id: &str) -> Option<Project> {
        self.state.project(id).cloned()
    }

    pub fn project_root(&self, id: &str) -> Option<PathBuf> {
        self.state.project(id).map(|p| p.root.clone())
    }

    /// Each project here with a memory key: its key and its main folder (M62's learning).
    pub fn projects_with_keys(&self) -> Vec<(glasscode_core::paths::ProjectKey, PathBuf)> {
        self.state.projects.iter().filter_map(|p| self.journal.keys.get(&p.id).map(|key| (key.clone(), p.root.clone()))).collect()
    }

    pub fn accounts(&self) -> (Vec<glasscode_core::model::Account>, glasscode_core::model::Defaults) {
        (self.state.accounts.clone(), self.state.defaults.clone())
    }

    /// Stop the sessions running in a folder whose contents are about to change.
    pub fn stop_sessions_in(&mut self, path: &Path) {
        let busy: Vec<String> = self.state.sessions.iter().filter(|s| s.worktree == path).map(|s| s.id.clone()).collect();
        for id in busy {
            self.stop(&id);
        }
    }

    pub fn snapshot(&self) -> ServerState {
        let live: BTreeSet<String> = self.tmux.sessions().into_iter().collect();
        let sessions = self
            .state
            .sessions
            .iter()
            .map(|s| {
                let tmux_name = self.runtime.names.get(&s.id).cloned().unwrap_or_default();
                let reply = self.replies.get(&s.id);
                RemoteSession {
                    running: !tmux_name.is_empty() && live.contains(&tmux_name),
                    activity: self.activity.get(&s.id).copied(),
                    tmux_name,
                    session: s.clone(),
                    prompt: self.prompts.get(&s.id).cloned(),
                    last_reply: reply.and_then(|(_, _, r)| r.clone()),
                    updated_at: reply.and_then(|(_, t, _)| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs()),
                    pr: self.prs.get(&s.worktree).cloned(),
                    host: None,
                    synced_at: None,
                    signed_out: self.signed_out.contains(&s.id),
                }
            })
            .collect();
        let signed_in = self.state.accounts.iter().map(|a| (a.id.clone(), signed_in(a))).collect();
        ServerState {
            projects: self.state.projects.clone(),
            worktrees: self.worktrees.clone(),
            accounts: self.state.accounts.clone(),
            signed_in,
            defaults: self.state.defaults.clone(),
            sessions,
            github: self.github.clone(),
            repos: self.repos.clone(),
            mirrors: crate::mirror::listed(&self.mirrors),
            parked: self.runtime.parked.values().cloned().collect(),
        }
    }
}

/// Whether an account has a login on this machine: only the file's presence is checked,
/// never its contents. (On macOS Claude keeps it in the Keychain, so assume yes.)
fn signed_in(account: &Account) -> bool {
    let dir = account.effective_dir();
    match account.agent {
        AgentKind::Claude => cfg!(target_os = "macos") || dir.join(".credentials.json").is_file() || std::env::var_os("CLAUDE_CODE_OAUTH_TOKEN").is_some(),
        AgentKind::Codex => dir.join("auth.json").is_file(),
        AgentKind::Shell => true,
    }
}

/// Where new branches start: the remote's default branch after a fetch, because a server's
/// clone is often months behind. Falls back to the local branch when fetching fails (offline,
/// no credentials on the server).
fn fresh_base(root: &Path) -> String {
    let fresh = git::fresh_base(root);
    if !fresh.fetched {
        log(format!("could not fetch origin/{} in {}; new branches start from {}", fresh.branch, root.display(), fresh.base));
    }
    fresh.base
}

/// A session's name for people: its title, or its branch in words when the title is the default.
fn display_title(session: &Session) -> String {
    if session.title.starts_with(&format!("{} · ", session.agent.name())) {
        let last = session.branch.rsplit('/').next().unwrap_or(&session.branch).replace(['-', '_'], " ");
        let mut chars = last.chars();
        return chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default();
    }
    session.title.clone()
}

/// What a permission is about, in a few words ("Claude wants to push to GitHub").
fn describe_prompt(agent: AgentKind, prompt: &PendingPrompt) -> String {
    let detail = prompt.detail.join(" ");
    let all = format!("{detail} {} {}", prompt.question, prompt.title.clone().unwrap_or_default()).to_lowercase();
    let what = if all.contains("git push") {
        "wants to push to GitHub".to_string()
    } else if all.contains("gh pr create") {
        "wants to open a pull request".to_string()
    } else if all.contains("gh pr merge") {
        "wants to merge the pull request".to_string()
    } else if prompt.title.as_deref().is_some_and(|t| t.to_lowercase().contains("edit")) {
        "wants to edit a file".to_string()
    } else if let Some(command) = prompt.detail.first().filter(|_| all.contains("command")) {
        format!("wants to run {}", command.trim_start_matches(['$', ' ']).chars().take(48).collect::<String>())
    } else {
        format!("asks: {}", prompt.question)
    };
    format!("{} {what}", agent.name())
}

fn last_sentence(text: &str) -> String {
    let flat = text.replace('\n', " ");
    let cut = flat.trim_end_matches('?').rfind(['.', '!', '?', '\n']).map(|i| i + 1).unwrap_or(0);
    let last = flat[cut..].trim();
    if last.chars().count() > 140 { format!("{}…", last.chars().take(139).collect::<String>()) } else { last.to_string() }
}

/// A branch name from a task ("Fix the flaky retry in the webhook handler" →
/// "fix/flaky-retry-webhook-handler"), made unique in the repository.
fn branch_for_task(task: &str, root: &Path) -> String {
    const SKIP: [&str; 24] = [
        "the", "a", "an", "to", "and", "of", "in", "on", "for", "with", "please", "can", "you", "it", "this", "that", "my", "is", "be", "should", "we", "our", "so", "add",
    ];
    let words: Vec<String> = task
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let fix = words.first().is_some_and(|w| matches!(w.as_str(), "fix" | "bug" | "repair" | "stop"));
    let picked: Vec<&str> = words.iter().skip(usize::from(fix)).map(String::as_str).filter(|w| !SKIP.contains(w)).take(5).collect();
    let stem = if picked.is_empty() { "task".to_string() } else { picked.join("-") };
    let stem: String = stem.chars().take(48).collect::<String>().trim_end_matches('-').to_string();
    let base = format!("{}/{stem}", if fix { "fix" } else { "feat" });
    let exists = |name: &str| {
        std::process::Command::new("git").arg("-C").arg(root).args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")]).output().is_ok_and(|o| o.status.success())
    };
    let mut name = base.clone();
    let mut n = 2;
    while exists(&name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    name
}

/// Keep every session's pull request summary fresh (one minute apart, or right after a kick),
/// without holding the daemon's lock while GitHub answers.
fn pr_loop(shared: Shared, kicks: std::sync::mpsc::Receiver<()>) {
    loop {
        let (env, worktrees) = {
            let daemon = lock(&shared);
            // A review session's PR is the one it reviews, not one found from its branch.
            let worktrees: BTreeMap<PathBuf, Option<u64>> = daemon.state.sessions.iter().map(|s| (s.worktree.clone(), s.review.as_ref().map(|r| r.number))).collect();
            (daemon.env.clone(), worktrees)
        };
        match github::gh_ready(&env) {
            Err(reason) => lock(&shared).github = Some(reason),
            Ok(()) => {
                lock(&shared).github = None;
                for (worktree, reviewing) in worktrees.into_iter().filter(|(w, _)| w.is_dir()) {
                    let Some(repo) = github::repo_slug(&worktree) else { continue };
                    let branch = git::current_branch(&worktree);
                    let found = match reviewing {
                        Some(number) => github::pr_by_number(&repo, number, &env),
                        None => github::pr_for_checkout(&repo, &worktree, &branch, &env),
                    };
                    let summary = match found {
                        Ok(Some(pr)) => match github::pr_details(&repo, pr.number, &github::PreviewConfig::load(&worktree), &env) {
                            Ok(details) => Some(github::PrSummary::from_details(&details)),
                            Err(_) => continue,
                        },
                        Ok(None) => None,
                        // Offline or rate limited: keep what we had.
                        Err(_) => continue,
                    };
                    let mut daemon = lock(&shared);
                    match summary {
                        Some(summary) => daemon.prs.insert(worktree, summary),
                        None => daemon.prs.remove(&worktree),
                    };
                }
            }
        }
        let _ = kicks.recv_timeout(Duration::from_secs(60));
        while kicks.try_recv().is_ok() {}
    }
}

pub(crate) fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `len` bytes into the buffer.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    if !ok {
        return "server".into();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn param<T: for<'de> Deserialize<'de>>(params: &Value, key: &str) -> anyhow::Result<T> {
    serde_json::from_value(params.get(key).cloned().unwrap_or(Value::Null)).with_context(|| format!("missing or invalid `{key}`"))
}

impl Daemon {
    /// A session's title, agent and project name, for saying who ran a command.
    /// The tmux session a session runs in here, and tmux (the manager types into it, M57).
    pub fn running_in(&self, id: &str) -> Option<(Tmux, String)> {
        self.runtime.names.get(id).map(|name| (self.tmux.clone(), name.clone()))
    }

    /// The id of this server's project of a name (the Mac's projects match by name).
    pub fn project_named(&self, name: &str) -> Option<String> {
        self.state.projects.iter().find(|p| p.name.eq_ignore_ascii_case(name)).map(|p| p.id.clone())
    }

    /// The name of the project of a session here.
    pub fn project_name_of(&self, id: &str) -> Option<String> {
        let session = self.state.session(id)?;
        self.project(&session.project_id).map(|p| p.name)
    }

    pub fn session_brief(&self, id: &str) -> Option<(String, AgentKind, Option<String>)> {
        self.state.session(id).map(|s| (s.title.clone(), s.agent, self.state.project(&s.project_id).map(|p| p.name.clone())))
    }

    /// The worktrees of the sessions running here.
    pub fn session_worktrees(&self) -> Vec<PathBuf> {
        let mut list: Vec<PathBuf> = self.state.sessions.iter().filter(|s| self.runtime.running.contains(&s.id)).map(|s| s.worktree.clone()).collect();
        list.sort();
        list.dedup();
        list
    }
}

pub fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Daemon> {
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Start a session (`sessions.create`, and a Mac session continued here).
pub fn create_session(shared: &Shared, create: CreateSession) -> anyhow::Result<RemoteSession> {
    if let Some(tag) = &create.held {
        crate::owners::moving_here(&tag.key, tag.epoch)?;
        let daemon = lock(shared);
        if let Some(id) = daemon.held_session(tag) {
            // Made already, by a try whose answer was lost: the same session, never a second.
            return daemon.snapshot().sessions.into_iter().find(|s| s.session.id == id).ok_or_else(|| anyhow!("session vanished"));
        }
    }
    let project = lock(shared).state.project(&create.project_id).cloned().ok_or_else(|| anyhow!("no project {}", create.project_id))?;
    let worktree = match (&create.start, &create.worktree, &create.new_branch) {
        (Some(StartFrom::Review { number, base }), _, _) => {
            if !base.is_empty() {
                git::fetch_branch(&project.root, base);
            }
            let checkout = git::checkout_review(&project.root, &project.name, *number)?;
            log(format!("review of #{number}: {}", checkout.note));
            checkout.path
        }
        (Some(StartFrom::PullRequest { number, head, same_repo }), _, _) => {
            let checkout = git::checkout_pr_branch(&project.root, &project.name, *number, head, *same_repo)?;
            log(format!("PR #{number}: {}", checkout.note));
            checkout.path
        }
        (_, Some(path), _) => path.clone(),
        (_, None, Some(branch)) => {
            // "auto": named from the task, the way a person would ("fix/flaky-webhook-retry").
            let branch = if branch.trim() == "auto" {
                branch_for_task(create.initial_prompt.as_deref().or(create.title.as_deref()).unwrap_or_default(), &project.root)
            } else {
                branch.trim().to_string()
            };
            let base = fresh_base(&project.root);
            git::add_worktree(&project.root, &project.name, &branch, &base)?
        }
        (_, None, None) => project.root.clone(),
    };
    if !worktree.is_dir() {
        bail!("{} does not exist on the server", worktree.display());
    }
    let branch = git::current_branch(&worktree);
    let mut daemon = lock(shared);
    let account_id = if create.agent.uses_accounts() {
        create.account_id.clone().or_else(|| daemon.state.default_account(create.agent).map(|a| a.id.clone()))
    } else {
        None
    };
    if create.agent.uses_accounts() && account_id.is_none() {
        bail!("add a {} account on the server first", create.agent.name());
    }
    let lineage = vec![daemon.state.lineage_tag(create.agent, account_id.as_deref())];
    let session = Session {
        id: new_id(),
        project_id: project.id.clone(),
        title: create.title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| format!("{} · {}", create.agent.name(), branch)),
        worktree,
        branch,
        agent: create.agent,
        account_id,
        agent_session_id: create.resume.clone().or_else(|| (create.agent == AgentKind::Claude).then(new_id)),
        transcript_path: None,
        lineage,
        initial_prompt: create.initial_prompt.clone(),
        team_task: None,
        model: create.model.clone(),
        mode: create.mode.clone(),
        review: create.review.clone(),
        created_at: now(),
    };
    let id = session.id.clone();
    if let Some(tag) = &create.held {
        // Another try with the same tag got here first while this one worked unlocked.
        if let Some(made) = daemon.held_session(tag) {
            return daemon.snapshot().sessions.into_iter().find(|s| s.session.id == made).ok_or_else(|| anyhow!("session vanished"));
        }
    } else if crate::owners::owner_elsewhere(&daemon.worktree_key(&session)).is_some() {
        let key = daemon.worktree_key(&session);
        bail!("{}", crate::owners::why_not_here(&key));
    }
    daemon.state.sessions.push(session);
    if let Some(tag) = create.held.clone() {
        // Its agent starts when the move commits (`owners.commit`).
        daemon.runtime.held.insert(id.clone(), tag);
    } else if let Err(err) = daemon.start(&id) {
        daemon.state.sessions.retain(|s| s.id != id);
        daemon.save();
        return Err(err);
    }
    daemon.refresh_worktrees();
    daemon.save();
    let snapshot = daemon.snapshot();
    snapshot.sessions.into_iter().find(|s| s.session.id == id).ok_or_else(|| anyhow!("session vanished"))
}

/// A lock per repository (or per place a project is cloned to): git's own bookkeeping is not
/// safe for two checkouts or clones at once, and the switch moves several worktrees at a time.
pub fn project_lock(place: &str) -> Arc<Mutex<()>> {
    static LOCKS: std::sync::OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = std::sync::OnceLock::new();
    let mut locks = LOCKS.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
    locks.entry(place.to_string()).or_default().clone()
}

/// What `caller` may not call here (M13.2), before any handler runs; and a declaration, which
/// only a connection's own read loop answers.
fn refuse(caller: &Caller, method: &str) -> anyhow::Result<()> {
    if let Some(why) = caller::daemon_refusal(caller, method) {
        bail!("{why}");
    }
    if method == caller::DECLARE_METHOD {
        bail!("a connection declares its caller in its own read loop, not here");
    }
    Ok(())
}

/// Answer one request from `caller` (its connection's, M13.2). Slow work (git clones, worktree
/// checks, history scans) runs without holding the daemon's lock, so other clients and the
/// activity loop are not blocked.
pub fn dispatch(shared: &Shared, caller: &Caller, method: &str, params: Value) -> anyhow::Result<Value> {
    refuse(caller, method)?;
    if let Some(answer) = crate::mirror::dispatch(shared, caller, method, &params) {
        return answer;
    }
    if let Some(answer) = crate::today::dispatch(method, &params) {
        return answer;
    }
    if let Some(answer) = crate::manager::dispatch(shared, method, &params) {
        return answer;
    }
    if let Some(answer) = crate::owners::dispatch(shared, caller, method, &params) {
        return answer;
    }
    if let Some(answer) = crate::home::dispatch(caller, method, &params) {
        return answer;
    }
    if let Some(answer) = crate::memory_copy::dispatch(method, &params) {
        return answer;
    }
    match method {
        "hello" => {
            let env = lock(shared).env.clone();
            let hello = Hello {
                version: env!("CARGO_PKG_VERSION").into(),
                build: glasscode_core::update::BUILD,
                protocol: PROTOCOL_VERSION,
                host: hostname(),
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                user: std::env::var("USER").unwrap_or_default(),
                home: paths::home(),
                tmux: Tmux::version(),
                claude: agents::installed_version("claude", &env),
                codex: agents::installed_version("codex", &env),
                approved: versions::load_approved(),
                features: {
                    // "authority": it keeps the Mac's approved authority from `mirror.sync` (M21.1),
                    // and never sends it back.
                    let mut features: Vec<String> = ["phone", "mirror", "today", "manager", caller::DAEMON_FEATURE, "authority"].map(String::from).to_vec();
                    if crate::push::configured() {
                        features.push("push".into());
                    }
                    if crate::owners::machine().is_some() {
                        features.push("owners".into());
                    }
                    // The server's read-only copy of the Mac's memory (M20.3), and memory entries
                    // both ways, which replace it (M57).
                    features.push("memory-copy".into());
                    features.push("memory-sync".into());
                    // The manager's home arbiter (M58): home.get, claim, renew, release, effects.
                    features.push("home".into());
                    // `journal.since` (M14): the events of this server's journal, by cursor.
                    features.push("journal".into());
                    features
                },
                machine: crate::owners::machine(),
            };
            Ok(serde_json::to_value(hello)?)
        }
        "doctor" => {
            let env = lock(shared).env.clone();
            Ok(serde_json::to_value(crate::doctor::run(&env))?)
        }
        "state" => Ok(serde_json::to_value(lock(shared).snapshot())?),
        // The Mac types into this session on behalf of an effect (M31): its events descend from it
        // once the prompt with the marker is in (events-13).
        glasscode_core::caller::DRIVE_METHOD => {
            journal::expect_drive_from(&params).map_err(|why| anyhow::anyhow!(why))?;
            Ok(json!({ "ok": true }))
        }
        "journal.since" => {
            // The events after a cursor, within the caller's scope: the Mac (a machine) sees them
            // all, a phone none that name a project. Read without the daemon's lock.
            let cursor: journal::Cursor = match params.get("cursor") {
                Some(Value::Null) | None => journal::Cursor::default(),
                Some(value) => serde_json::from_value(value.clone())?,
            };
            let max = params.get("max").and_then(Value::as_u64).unwrap_or(500).clamp(1, 5_000) as usize;
            let (mut events, next) = journal::since(&cursor, &journal::Scope::of(caller, &[]), &journal::Filter::default(), max);
            // A Mac's own events came from it (M57): it does not pull them back.
            if let Caller::Machine { id } = caller {
                events.retain(|event| &event.at.machine != id);
            }
            Ok(json!({ "on": journal::known_enabled() == Some(true), "events": events, "cursor": next }))
        }
        "relay.status" => Ok(serde_json::to_value(crate::relay_link::STATUS.lock().unwrap().clone())?),

        "projects.add" => {
            let add: AddProject = serde_json::from_value(params)?;
            // One at a time per place: worktree moves in parallel add a missing project once.
            let place = add.path.as_ref().map(|p| p.display().to_string()).or_else(|| add.url.clone()).unwrap_or_default();
            let lock_for_place = project_lock(&place);
            let _one_at_a_time = lock_for_place.lock().unwrap_or_else(|p| p.into_inner());
            let root = match (&add.url, &add.path) {
                (_, Some(path)) if path.join(".git").exists() => path.clone(),
                (Some(url), path) => {
                    let name = add.name.clone().unwrap_or_else(|| {
                        url.trim_end_matches('/').trim_end_matches(".git").rsplit(['/', ':']).next().unwrap_or("project").to_string()
                    });
                    let target = path.clone().unwrap_or_else(|| paths::home().join("Projects").join(&name));
                    if target.exists() && std::fs::read_dir(&target).map(|mut d| d.next().is_some()).unwrap_or(false) {
                        bail!("{} already exists and is not a git repository", target.display());
                    }
                    std::fs::create_dir_all(target.parent().unwrap_or(&target))?;
                    log(format!("cloning {url} into {}", target.display()));
                    let out = std::process::Command::new("git").arg("clone").arg(url).arg(&target).env("GIT_TERMINAL_PROMPT", "0").output()?;
                    if !out.status.success() {
                        bail!("git clone failed: {}", String::from_utf8_lossy(&out.stderr).trim());
                    }
                    target
                }
                (None, Some(path)) => bail!("{} is not a git repository", path.display()),
                (None, None) => bail!("give a repository URL or a folder"),
            };
            let root = git::main_checkout(&root).unwrap_or(root);
            let mut daemon = lock(shared);
            if let Some(existing) = daemon.state.projects.iter().find(|p| p.root == root) {
                return Ok(serde_json::to_value(existing)?);
            }
            let name = add.name.unwrap_or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            let project = Project { id: new_id(), name, root, expanded: true, github_account: None };
            daemon.state.projects.push(project.clone());
            daemon.refresh_worktrees();
            daemon.save();
            Ok(serde_json::to_value(project)?)
        }
        "projects.remove" => {
            let id: String = param(&params, "project_id")?;
            let mut daemon = lock(shared);
            if daemon.state.sessions.iter().any(|s| s.project_id == id) {
                bail!("close this project's sessions first");
            }
            daemon.state.projects.retain(|p| p.id != id);
            daemon.worktrees.remove(&id);
            daemon.save();
            Ok(Value::Null)
        }

        "worktrees.add" => {
            let project_id: String = param(&params, "project_id")?;
            let branch: String = param(&params, "branch")?;
            let base: Option<String> = param(&params, "base").unwrap_or(None);
            let project = lock(shared).state.project(&project_id).cloned().ok_or_else(|| anyhow!("no project {project_id}"))?;
            let base = base.unwrap_or_else(|| fresh_base(&project.root));
            let path = git::add_worktree(&project.root, &project.name, &branch, &base)?;
            lock(shared).refresh_worktrees();
            Ok(serde_json::to_value(path)?)
        }
        "worktrees.check" => {
            let path: PathBuf = param(&params, "path")?;
            let worktree = lock(shared).worktrees.values().flatten().find(|w| w.path == path).cloned().ok_or_else(|| anyhow!("no worktree {}", path.display()))?;
            Ok(serde_json::to_value(git::check_worktree(&worktree))?)
        }
        "worktrees.remove" => {
            let project_id: String = param(&params, "project_id")?;
            let path: PathBuf = param(&params, "path")?;
            let force: bool = param(&params, "force").unwrap_or(false);
            let delete_branch: bool = param(&params, "delete_branch").unwrap_or(false);
            let (root, worktree, archive) = {
                let mut daemon = lock(shared);
                let root = daemon.state.project(&project_id).map(|p| p.root.clone()).ok_or_else(|| anyhow!("no project {project_id}"))?;
                let worktree = daemon.worktrees.get(&project_id).and_then(|l| l.iter().find(|w| w.path == path)).cloned().ok_or_else(|| anyhow!("no worktree {}", path.display()))?;
                // Its work is archived first, in this server's memory (M24, with M57's entries): read
                // here, before its sessions go.
                let archive = archive_input(&daemon, &project_id, &worktree);
                // Sessions in it end first; their terminals hold the folder open.
                let doomed: Vec<String> = daemon.state.sessions.iter().filter(|s| s.worktree == path).map(|s| s.id.clone()).collect();
                for id in doomed {
                    daemon.stop(&id);
                    daemon.state.sessions.retain(|s| s.id != id);
                }
                daemon.save();
                (root, worktree, archive)
            };
            if let Some(input) = archive
                && let Err(err) = glasscode_core::memstore::archive::before_removal(input)
            {
                log(format!("memory: the work of {} was not archived: {err}", worktree.path.display()));
            }
            git::remove_worktree(&root, &worktree, force)?;
            if delete_branch && !worktree.is_detached() {
                git::delete_branch(&root, &worktree.branch)?;
            }
            lock(shared).refresh_worktrees();
            Ok(Value::Null)
        }

        "accounts.add" => {
            let agent: AgentKind = param(&params, "agent")?;
            let label: String = param(&params, "label")?;
            let plan: Option<String> = param(&params, "plan").unwrap_or(None);
            let mut daemon = lock(shared);
            let id = format!("{}-{}", agent.name().to_lowercase(), &new_id()[..6]);
            let account = Account { id, agent, label, plan, system_default: false };
            if let Some(dir) = account.config_dir() {
                std::fs::create_dir_all(&dir)?;
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
            daemon.state.accounts.push(account.clone());
            if daemon.state.defaults.for_agent(agent).is_none() {
                daemon.state.defaults.set(agent, Some(account.id.clone()));
            }
            daemon.save();
            Ok(serde_json::to_value(account)?)
        }
        "accounts.remove" => {
            let id: String = param(&params, "account_id")?;
            let mut daemon = lock(shared);
            if daemon.state.sessions.iter().any(|s| s.account_id.as_deref() == Some(id.as_str())) {
                bail!("sessions still use this account");
            }
            daemon.state.accounts.retain(|a| a.id != id);
            for agent in [AgentKind::Claude, AgentKind::Codex] {
                if daemon.state.defaults.for_agent(agent) == Some(&id) {
                    let first = daemon.state.accounts_for(agent).next().map(|a| a.id.clone());
                    daemon.state.defaults.set(agent, first);
                }
            }
            daemon.save();
            Ok(Value::Null)
        }
        "accounts.set_default" => {
            let agent: AgentKind = param(&params, "agent")?;
            let id: String = param(&params, "account_id")?;
            let mut daemon = lock(shared);
            daemon.state.defaults.set(agent, Some(id));
            daemon.save();
            Ok(Value::Null)
        }
        "accounts.login" => {
            // The CLI's own sign-in, in a terminal the client attaches to; the login is made
            // and kept by the CLI on this machine, never copied.
            let id: String = param(&params, "account_id")?;
            let daemon = lock(shared);
            let account = daemon.state.account(&id).cloned().ok_or_else(|| anyhow!("no account {id}"))?;
            let mut launch = agents::login_launch(&account, &daemon.env);
            if account.agent == AgentKind::Codex {
                // The browser is on the laptop, so the usual localhost callback cannot reach
                // this machine; the device code flow works from any browser.
                launch.args.push("--device-auth".into());
            }
            let name = format!("login-{}", account.id);
            daemon.tmux.kill(&name);
            daemon.tmux.start(&name, &paths::home(), &launch.env, &launch.program, &launch.args)?;
            Ok(serde_json::to_value(Attach { argv: daemon.tmux.attach_argv(&name) })?)
        }

        "sessions.create" => {
            let create: CreateSession = serde_json::from_value(params)?;
            Ok(serde_json::to_value(create_session(shared, create)?)?)
        }
        "sessions.attach" => {
            let id: String = param(&params, "session_id")?;
            let mut daemon = lock(shared);
            daemon.start(&id)?;
            daemon.save();
            let name = daemon.runtime.names.get(&id).cloned().ok_or_else(|| anyhow!("no session {id}"))?;
            Ok(serde_json::to_value(Attach { argv: daemon.tmux.attach_argv(&name) })?)
        }
        "sessions.stop" => {
            let id: String = param(&params, "session_id")?;
            let mut daemon = lock(shared);
            daemon.stop(&id);
            daemon.save();
            Ok(Value::Null)
        }
        "sessions.close" => {
            let id: String = param(&params, "session_id")?;
            let mut daemon = lock(shared);
            daemon.stop(&id);
            daemon.runtime.names.remove(&id);
            daemon.state.sessions.retain(|s| s.id != id);
            let _ = std::fs::remove_file(agents::events_file(&id));
            daemon.save();
            Ok(Value::Null)
        }
        "sessions.rename" => {
            let id: String = param(&params, "session_id")?;
            let title: String = param(&params, "title")?;
            let mut daemon = lock(shared);
            let session = daemon.state.session_mut(&id).ok_or_else(|| anyhow!("no session {id}"))?;
            session.title = title;
            daemon.save();
            Ok(Value::Null)
        }
        "sessions.peek" => {
            let id: String = param(&params, "session_id")?;
            let lines: u32 = param(&params, "lines").unwrap_or(60);
            let daemon = lock(shared);
            let name = daemon.runtime.names.get(&id).cloned().ok_or_else(|| anyhow!("session {id} is not running"))?;
            Ok(json!(daemon.tmux.capture(&name, lines.min(2000))?))
        }
        "sessions.send" => {
            let id: String = param(&params, "session_id")?;
            let text: String = param(&params, "text")?;
            let enter: bool = param(&params, "enter").unwrap_or(true);
            let (tmux, name) = {
                let daemon = lock(shared);
                let name = daemon.runtime.names.get(&id).cloned().ok_or_else(|| anyhow!("session {id} is not running"))?;
                (daemon.tmux.clone(), name)
            };
            tmux.send(&name, &text, enter)?;
            Ok(Value::Null)
        }

        "sessions.transcript" => {
            let id: String = param(&params, "session_id")?;
            let after: Option<u64> = param(&params, "after").unwrap_or(None);
            let limit: usize = param(&params, "limit").unwrap_or(400);
            let (agent, file) = {
                let daemon = lock(shared);
                let session = daemon.state.session(&id).cloned().ok_or_else(|| anyhow!("no session {id}"))?;
                (session.agent, daemon.transcript_file(&session))
            };
            match file {
                Some(path) => Ok(serde_json::to_value(transcript::read(agent, &path, after, limit.min(2000))?)?),
                // No conversation yet (just started, or a shell): an empty first page.
                None => Ok(serde_json::to_value(transcript::TranscriptPage { items: Vec::new(), cursor: 0, reset: after.is_none() })?),
            }
        }
        "sessions.answer" => {
            // Pick an option of the question on the agent's screen, the way a person would:
            // arrows from the highlighted option, Enter; then the text for "no, do this instead".
            let id: String = param(&params, "session_id")?;
            let option: u32 = param(&params, "option")?;
            let text: Option<String> = param(&params, "text").unwrap_or(None);
            let (tmux, name) = {
                let daemon = lock(shared);
                let name = daemon.runtime.names.get(&id).cloned().ok_or_else(|| anyhow!("session {id} is not running"))?;
                (daemon.tmux.clone(), name)
            };
            let screen = tmux.screen(&name)?;
            let open = prompt::parse(&screen).ok_or_else(|| anyhow!("the question is no longer open"))?;
            let keys = prompt::keys_for(&open, option).ok_or_else(|| anyhow!("no option {option}"))?;
            tmux.keys(&name, &keys)?;
            if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
                std::thread::sleep(Duration::from_millis(500));
                tmux.send(&name, &text, true)?;
            }
            let mut daemon = lock(shared);
            daemon.prompts.remove(&id);
            daemon.activity.insert(id, AgentActivity::Working);
            Ok(Value::Null)
        }
        "sessions.keys" => {
            // Single keys a phone keyboard lacks: Escape (stop), BTab (mode), arrows, Ctrl-C.
            let id: String = param(&params, "session_id")?;
            let keys: Vec<String> = param(&params, "keys")?;
            const ALLOWED: [&str; 14] = ["Escape", "Enter", "Up", "Down", "Left", "Right", "Tab", "BTab", "C-c", "1", "2", "3", "y", "n"];
            if let Some(bad) = keys.iter().find(|k| !ALLOWED.contains(&k.as_str())) {
                bail!("key {bad} is not allowed");
            }
            let (tmux, name) = {
                let daemon = lock(shared);
                let name = daemon.runtime.names.get(&id).cloned().ok_or_else(|| anyhow!("session {id} is not running"))?;
                (daemon.tmux.clone(), name)
            };
            tmux.keys(&name, &keys.iter().map(String::as_str).collect::<Vec<_>>())?;
            Ok(Value::Null)
        }
        // TODO(glasscode): review sessions keep no files on the server yet. These RPCs are
        // no-ops until a review flow is added.
        "reviews.files" => Ok(Value::Null),
        "reviews.write" => Ok(Value::Null),
        "sessions.set_review" => {
            let id: String = param(&params, "session_id")?;
            let review: Option<ReviewRef> = serde_json::from_value(params.get("review").cloned().unwrap_or(Value::Null))?;
            let mut daemon = lock(shared);
            let session = daemon.state.session_mut(&id).ok_or_else(|| anyhow!("no session {id}"))?;
            session.review = review;
            daemon.save();
            Ok(Value::Null)
        }
        "worktrees.changes" => {
            let path: PathBuf = param(&params, "path")?;
            if !lock(shared).known_checkout(&path) {
                bail!("{} is not a checkout on this server", path.display());
            }
            Ok(serde_json::to_value(git::status(&path)?)?)
        }
        "worktrees.diff" => {
            let path: PathBuf = param(&params, "path")?;
            let file: String = param(&params, "file")?;
            if !lock(shared).known_checkout(&path) {
                bail!("{} is not a checkout on this server", path.display());
            }
            let untracked = git::status(&path)?.iter().any(|c| c.path == file && c.status == '?');
            let mut diff = git::diff_file(&path, &file, untracked)?;
            if diff.len() > 400_000 {
                let mut cut = 400_000;
                while !diff.is_char_boundary(cut) {
                    cut -= 1;
                }
                diff.truncate(cut);
                diff.push_str("\n… diff truncated …\n");
            }
            Ok(json!(diff))
        }
        "worktrees.commit" => {
            let path: PathBuf = param(&params, "path")?;
            let message: String = param(&params, "message")?;
            let push: bool = param(&params, "push").unwrap_or(false);
            if !lock(shared).known_checkout(&path) {
                bail!("{} is not a checkout on this server", path.display());
            }
            if message.trim().is_empty() {
                bail!("the commit message is empty");
            }
            git::commit_all(&path, message.trim())?;
            if push {
                let branch = git::current_branch(&path);
                git::push(&path, &branch)?;
            }
            Ok(Value::Null)
        }
        "github.pr" => {
            // The checkout's pull request and its checks, reviews, comments and preview, through
            // the server's own `gh` login.
            let path: PathBuf = param(&params, "path")?;
            let env = {
                let daemon = lock(shared);
                if !daemon.known_checkout(&path) {
                    bail!("{} is not a checkout on this server", path.display());
                }
                daemon.env.clone()
            };
            let repo = github::repo_slug(&path).ok_or_else(|| anyhow!("{} is not a GitHub repository", path.display()))?;
            let branch = git::current_branch(&path);
            let pr = github::pr_for_checkout(&repo, &path, &branch, &env)?;
            let details = match &pr {
                Some(pr) => Some(github::pr_details(&repo, pr.number, &github::PreviewConfig::load(&path), &env)?),
                None => None,
            };
            {
                let mut daemon = lock(shared);
                match &details {
                    Some(d) => daemon.prs.insert(path.clone(), github::PrSummary::from_details(d)),
                    None => daemon.prs.remove(&path),
                };
            }
            Ok(json!({ "repo": repo, "branch": branch, "pr": pr, "details": details }))
        }
        "github.action" => {
            let repo: String = param(&params, "repo")?;
            let number: u64 = param(&params, "number").unwrap_or(0);
            let action: String = param(&params, "action")?;
            let env = lock(shared).env.clone();
            match action.as_str() {
                "comment" => github::comment(&repo, number, &param::<String>(&params, "body")?, &env)?,
                "reply" => github::reply_to_thread(&repo, &param::<String>(&params, "thread")?, &param::<String>(&params, "body")?, &env)?,
                "resolve" => github::resolve_thread(&repo, &param::<String>(&params, "thread")?, &env)?,
                "rerun" => github::rerun_failed(&repo, &param::<Vec<u64>>(&params, "runs")?, &env)?,
                "label" => github::set_label(&repo, number, &param::<String>(&params, "label")?, param(&params, "on")?, &env)?,
                other => bail!("unknown GitHub action {other}"),
            }
            kick_prs();
            Ok(Value::Null)
        }
        "github.refresh" => {
            kick_prs();
            Ok(Value::Null)
        }
        "phones.add" => {
            let phone: PhoneKey = serde_json::from_value(params)?;
            Ok(serde_json::to_value(crate::phones::add(&phone)?)?)
        }
        "phones.list" => Ok(serde_json::to_value(crate::phones::list())?),
        "phones.push_token" => {
            let token: String = param(&params, "token")?;
            let sandbox: bool = param(&params, "sandbox").unwrap_or(false);
            let name: String = param(&params, "name").unwrap_or_else(|_| "iPhone".into());
            crate::push::register(&token, sandbox, &name)?;
            Ok(json!({ "configured": crate::push::configured() }))
        }
        "phones.remove" => {
            let key: String = param(&params, "key")?;
            crate::phones::remove(&key)?;
            Ok(Value::Null)
        }
        "history.list" => {
            let agent: AgentKind = param(&params, "agent")?;
            let folder: PathBuf = param(&params, "folder")?;
            let accounts = lock(shared).state.accounts.clone();
            Ok(serde_json::to_value(history::conversations(agent, &accounts, &folder))?)
        }
        "worktrees.receive" => {
            // A worktree arriving from the laptop: refs were pushed to refs/glasscode/in/<token>.
            let receive: ReceiveWorktree = serde_json::from_value(params)?;
            if let Some((key, epoch)) = &receive.move_epoch {
                crate::owners::moving_here(key, *epoch)?;
            }
            let project = lock(shared).state.project(&receive.project_id).cloned().ok_or_else(|| anyhow!("no project {}", receive.project_id))?;
            let root = &project.root;
            let resolve = |name: &str| -> anyhow::Result<String> {
                let out = std::process::Command::new("git").arg("-C").arg(root).args(["rev-parse", "--verify", "--quiet", &format!("refs/glasscode/in/{}/{name}", receive.token)]).output()?;
                if !out.status.success() {
                    bail!("the pushed {name} ref is missing");
                }
                Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
            };
            let (head, snap) = (resolve("head")?, resolve("snap")?);
            // git's worktree bookkeeping is per repository: one checkout at a time in it.
            let repo_lock = project_lock(&root.display().to_string());
            let _one_at_a_time = repo_lock.lock().unwrap_or_else(|p| p.into_inner());
            let path = match sync::matching_worktree(root, &receive.path, receive.branch.as_deref(), &head, &snap) {
                // Already exactly this (another session of the same switch brought it): leave
                // the folder, and the sessions running in it, alone.
                Some(path) => path,
                None => {
                    // Its contents change: sessions running in that folder stop first.
                    {
                        let mut daemon = lock(shared);
                        let busy: Vec<String> = daemon.state.sessions.iter().filter(|s| s.worktree == receive.path).map(|s| s.id.clone()).collect();
                        for id in busy {
                            daemon.stop(&id);
                        }
                    }
                    sync::receive_worktree(root, &receive.path, receive.branch.as_deref(), &head, &snap)?
                }
            };
            sync::write_env_files(&path, &receive.env)?;
            for name in ["head", "snap"] {
                let _ = std::process::Command::new("git").arg("-C").arg(root).args(["update-ref", "-d", &format!("refs/glasscode/in/{}/{name}", receive.token)]).output();
            }
            lock(shared).refresh_worktrees();
            log(format!("received {} into {}", receive.branch.as_deref().unwrap_or("a detached HEAD"), path.display()));
            Ok(serde_json::to_value(path)?)
        }
        "worktrees.park" => {
            // A Mac's worktree with uncommitted work and no session, left here at a switch: its
            // refs were pushed to refs/glasscode/parked/<token>. Nothing moves yet.
            let parked: glasscode_core::remote::ParkedWorktree = serde_json::from_value(params)?;
            let mut daemon = lock(shared);
            daemon.state.project(&parked.project_id).ok_or_else(|| anyhow!("no project {}", parked.project_id))?;
            daemon.runtime.parked.insert(parked.key.clone(), parked);
            daemon.save();
            Ok(Value::Null)
        }
        "worktrees.open_parked" => {
            // Open a parked worktree here: it is rebuilt from its copy, and this server owns it
            // from now on (the Mac's folder stays as it was; a session there stops).
            let key: WorktreeKey = param(&params, "key")?;
            let parked = lock(shared).runtime.parked.get(&key).cloned().ok_or_else(|| anyhow!("no copy of {} here", key.0))?;
            if crate::owners::machine().is_some_and(|me| crate::owners::get(std::slice::from_ref(&key)).get(&key).is_some_and(|r| r.owner == me)) {
                // It moved here since: the copy is older than what is here now.
                let mut daemon = lock(shared);
                daemon.runtime.parked.remove(&key);
                daemon.save();
                return Ok(serde_json::to_value(key.path_under(&paths::home()))?);
            }
            let root = lock(shared).project_root(&parked.project_id).ok_or_else(|| anyhow!("no project {}", parked.project_id))?;
            let resolve = |name: &str| -> anyhow::Result<String> {
                let out = std::process::Command::new("git").arg("-C").arg(&root).args(["rev-parse", "--verify", "--quiet", &format!("refs/glasscode/parked/{}/{name}", parked.token)]).output()?;
                if !out.status.success() {
                    bail!("the copy of {} is incomplete", key.0);
                }
                Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
            };
            let (head, snap) = (resolve("head")?, resolve("snap")?);
            let repo_lock = project_lock(&root.display().to_string());
            let path = {
                let _one_at_a_time = repo_lock.lock().unwrap_or_else(|p| p.into_inner());
                sync::receive_worktree(&root, &key.path_under(&paths::home()), parked.branch.as_deref(), &head, &snap)?
            };
            if let Some(mac) = &parked.machine {
                crate::owners::taken_here(&key, mac);
            }
            let mut daemon = lock(shared);
            daemon.runtime.parked.remove(&key);
            daemon.refresh_worktrees();
            daemon.save();
            log(format!("opened the parked copy of {} into {}", key.0, path.display()));
            Ok(serde_json::to_value(path)?)
        }
        "worktrees.snapshot" => {
            // A worktree leaving for the laptop: its state, in refs/glasscode/out/<token>.
            let path: PathBuf = param(&params, "path")?;
            let token: String = param(&params, "token")?;
            let root = git::main_checkout(&path)?;
            let snap = sync::snapshot(&path)?;
            for (name, sha) in [("head", &snap.head), ("snap", &snap.snap)] {
                let out = std::process::Command::new("git").arg("-C").arg(&root).args(["update-ref", &format!("refs/glasscode/out/{token}/{name}"), sha]).output()?;
                if !out.status.success() {
                    bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
                }
            }
            sync::remember_sent(&root, &path, &snap.snap);
            Ok(serde_json::to_value(WorktreeSnapshot { root, branch: snap.branch, head: snap.head, snap: snap.snap, env: sync::env_files(&path) })?)
        }
        "refs.clear" => {
            let project_id: String = param(&params, "project_id")?;
            let token: String = param(&params, "token")?;
            let root = lock(shared).state.project(&project_id).map(|p| p.root.clone()).ok_or_else(|| anyhow!("no project {project_id}"))?;
            for name in ["head", "snap"] {
                let _ = std::process::Command::new("git").arg("-C").arg(&root).args(["update-ref", "-d", &format!("refs/glasscode/out/{token}/{name}")]).output();
            }
            Ok(Value::Null)
        }
        "conversations.import_path" => {
            // Where an arriving conversation file goes in an account's folder.
            let account_id: String = param(&params, "account_id")?;
            let rel: PathBuf = param(&params, "rel")?;
            if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                bail!("bad conversation path");
            }
            let account = lock(shared).state.account(&account_id).cloned().ok_or_else(|| anyhow!("no account {account_id}"))?;
            let path = account.effective_dir().join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Ok(serde_json::to_value(path)?)
        }
        "conversations.export" => {
            let id: String = param(&params, "session_id")?;
            let (session, account) = {
                let daemon = lock(shared);
                let session = daemon.state.session(&id).cloned().ok_or_else(|| anyhow!("no session {id}"))?;
                let account = session.account_id.as_deref().and_then(|a| daemon.state.account(a)).cloned().ok_or_else(|| anyhow!("session {id} has no account"))?;
                (session, account)
            };
            let conversation = session.agent_session_id.clone().ok_or_else(|| anyhow!("session {id} has no conversation yet"))?;
            let dir = account.effective_dir();
            let path = match session.agent {
                AgentKind::Claude => agents::find_claude_transcript(&dir, &conversation),
                AgentKind::Codex => session.transcript_path.clone().filter(|p| p.is_file()).or_else(|| agents::find_codex_rollout(&dir, &conversation)),
                AgentKind::Shell => None,
            }
            .ok_or_else(|| anyhow!("no conversation file for session {id}"))?;
            let rel = path.strip_prefix(&dir).map(Path::to_path_buf).unwrap_or_default();
            Ok(serde_json::to_value(ConversationFile { account_id: account.id, path, rel })?)
        }
        "restart" => {
            // After an update: the new binary takes this process's place (same PID, so a
            // service manager does not notice; without one it comes back all the same).
            // Sessions keep running in tmux; clients reconnect.
            log("restarting with the installed binary");
            std::thread::spawn(|| {
                // Long enough for an install over the relay to finish its SSH session.
                std::thread::sleep(Duration::from_millis(1500));
                use std::os::unix::process::CommandExt as _;
                let error = std::process::Command::new(paths::root().join("bin").join("glasscoded")).arg("serve").exec();
                log(format!("could not restart: {error}"));
            });
            Ok(json!({ "ok": true }))
        }
        "shutdown" => {
            // Used after an update when no service manager restarts us: the next connection
            // starts the new binary. Sessions keep running in tmux meanwhile.
            log("shutting down for an update");
            let _ = std::fs::remove_file(paths::daemon_socket());
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(200));
                std::process::exit(0);
            });
            Ok(Value::Null)
        }
        other => bail!("unknown method {other}"),
    }
}

/// What a phone types into a session, from one request line of its connection (`sessions.send`,
/// or the text of `sessions.answer`): the session and the text. Other lines carry no words.
pub fn phone_words(line: &str) -> Option<(String, String)> {
    let request: Request = serde_json::from_str(line.trim()).ok()?;
    if !matches!(request.method.as_str(), "sessions.send" | "sessions.answer") {
        return None;
    }
    let session = request.params.get("session_id")?.as_str()?.to_string();
    let text = request.params.get("text")?.as_str()?.trim().to_string();
    (!text.is_empty()).then_some((session, text))
}

/// Record a phone's words before the daemon gets its request (the gate, which only phones' keys
/// run): `device`'s, the id its `authorized_keys` line gives (`unknown` for a phone paired before
/// M13.2), tier two at most (design D1). Every other door to `sessions.send` (the Mac over its
/// `Machine` connection, the manager here, a program on this server) records nothing here: the
/// Mac recorded its own text with its own origin, and the server never upgrades a tier.
pub fn record_phone_input(device: &str, line: &str) {
    if let Some((session, text)) = phone_words(line) {
        glasscode_core::words::record(glasscode_core::words::Origin::Device(device.to_string()), &text, Some(&session), None);
        // On disk before the request goes on: the gate's process ends with the connection.
        glasscode_core::words::flush();
    }
}

fn handle_connection(stream: UnixStream, shared: Shared) {
    serve_connection(stream, move |caller, method, params| dispatch(&shared, &caller, method, params));
}

/// Answer each request on its own thread, so a slow one (a GitHub lookup) does not hold up
/// the others on the same connection; replies carry the request's id. A caller declaration
/// (M13.2) is answered here in the read loop instead, so it applies to every later line, and
/// each request keeps the caller its connection had when the line was read.
pub(crate) fn serve_connection<F>(stream: UnixStream, answer: F)
where
    F: Fn(Caller, &str, Value) -> anyhow::Result<Value> + Send + Sync + 'static,
{
    let Ok(writer) = stream.try_clone() else { return };
    let writer = Arc::new(Mutex::new(std::io::BufWriter::new(writer)));
    let answer = Arc::new(answer);
    let mut connection = caller::DaemonConnection::default();
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let request = match serde_json::from_str::<Request>(&line) {
            Ok(request) => request,
            Err(err) => {
                write_response(&writer, &Response { id: 0, result: None, error: Some(format!("bad request: {err}")) });
                continue;
            }
        };
        if request.method == caller::DECLARE_METHOD {
            let response = match connection.declare(&request.params) {
                Ok(declared) => Response { id: request.id, result: Some(serde_json::to_value(&declared).unwrap_or(Value::Null)), error: None },
                Err(why) => {
                    log(format!("refused a caller declaration from {:?}: {why}", connection.caller()));
                    Response { id: request.id, result: None, error: Some(why) }
                }
            };
            write_response(&writer, &response);
            continue;
        }
        let (caller, answer, writer) = (connection.caller(), answer.clone(), writer.clone());
        std::thread::spawn(move || {
            let response = match answer(caller, &request.method, request.params) {
                Ok(result) => Response { id: request.id, result: Some(result), error: None },
                Err(err) => Response { id: request.id, result: None, error: Some(format!("{err:#}")) },
            };
            write_response(&writer, &response);
        });
    }
}

fn write_response(writer: &Mutex<std::io::BufWriter<UnixStream>>, response: &Response) {
    let Ok(mut out) = serde_json::to_string(response) else { return };
    out.push('\n');
    if let Ok(mut writer) = writer.lock() {
        let _ = writer.write_all(out.as_bytes()).and_then(|_| writer.flush());
    }
}

/// Run the daemon in the foreground until killed.
pub fn serve() -> anyhow::Result<()> {
    paths::ensure_dirs()?;
    let _ = std::fs::set_permissions(paths::root(), std::fs::Permissions::from_mode(0o700));
    let socket = paths::daemon_socket();
    if UnixStream::connect(&socket).is_ok() {
        bail!("glasscoded is already running ({})", socket.display());
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).with_context(|| format!("could not listen on {}", socket.display()))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;

    let env = session_env();
    let daemon = Daemon::load(env)?;
    let shared: Shared = Arc::new(Mutex::new(daemon));
    log(format!("glasscoded {} listening on {}", env!("CARGO_PKG_VERSION"), socket.display()));
    lock(&shared).restore();

    let (kick, kicks) = std::sync::mpsc::channel();
    let _ = PR_KICK.set(kick);
    let prs = shared.clone();
    std::thread::spawn(move || pr_loop(prs, kicks));

    // Reaching this server through a relay is disabled in the open-source build (SSH/Tailscale).
    std::thread::spawn(crate::relay_link::run);
    std::thread::spawn(crate::update::run);

    // The `glasscode` command for sessions here, relayed to the Mac; Today for phones.
    crate::today::start(shared.clone());

    let ticker = shared.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        lock(&ticker).tick();
        // The manager's wake lane here, while the server is home (M57): outside the lock.
        crate::brain::tick(&ticker);
        // Memory's learning here, while the server is home (M62).
        crate::learning::tick(&ticker);
    });

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let shared = shared.clone();
                std::thread::spawn(move || handle_connection(stream, shared));
            }
            Err(err) => log(format!("accept failed: {err}")),
        }
    }
    Ok(())
}

/// A server session's profile (M22): None with the engine at v1 or "profiles" off, and for a
/// shell. Its origin is this machine's record (`authority/origins.json`; a session nobody marked
/// is the person's); its policy the Mac's approved default (the server has no project keys yet).
/// Linux runs Claude's sandbox through bubblewrap.
fn server_profile(session: &Session, state: &State) -> Option<glasscode_core::profile::SessionProfile> {
    use glasscode_core::profile::{self, GitFacts, HookProgram, ProfileFacts};
    use glasscode_core::store::Store as _;
    let switch = glasscode_core::store::EngineSwitch::current();
    if session.agent == AgentKind::Shell || !switch.is_on("profiles") {
        return None;
    }
    let origin = glasscode_core::gate::SessionOrigins::load().map(|o| o.origin(&session.id)).unwrap_or(glasscode_core::gate::SessionOrigin::Automation);
    let policy = glasscode_core::gate::FromMac::load().map(|copy| copy.effective(glasscode_core::clock::now_ms()).0.default).unwrap_or_default();
    let other_worktrees = state.sessions.iter().filter(|s| s.project_id == session.project_id && s.worktree != session.worktree).map(|s| s.worktree.clone()).collect();
    let env_files = std::fs::read_dir(&session.worktree)
        .map(|entries| entries.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".env") && e.file_type().is_ok_and(|t| t.is_file())).map(|e| e.path()).collect())
        .unwrap_or_default();
    let facts = ProfileFacts {
        worktree: session.worktree.clone(),
        other_worktrees,
        git: GitFacts::of(&session.worktree, &session.branch),
        notes: None,
        env_files,
        hook: std::env::current_exe().ok().map(|program| HookProgram { program, prefix: vec!["cli".into()] }),
        events: agents::events_file(&session.id),
        permission_file: paths::settings_dir().join(format!("{}.permission.json", session.id)),
        registries: ["registry.npmjs.org", "crates.io", "index.crates.io", "static.crates.io", "pypi.org", "files.pythonhosted.org"].map(String::from).to_vec(),
        forge: vec!["github.com".into(), "api.github.com".into()],
        default_branch: None,
        socket: glasscode_core::control::socket_path(),
    };
    let made = profile::session_profile(origin, session.agent, &policy, &facts);
    if let Ok(bytes) = serde_json::to_vec(&made.permission) {
        let _ = std::fs::create_dir_all(paths::settings_dir());
        let _ = std::fs::write(&facts.permission_file, bytes);
    }
    Some(made)
}

/// Keep the Mac's approved authority that came with its sync (`authority/from-mac.json`), as
/// one-way cache: glasscoded decides its own sessions by it ([`server_gate`]) and never sends it
/// back to the Mac. A copy that does not match its hash is left out, and the old one stays.
pub fn keep_authority(copy: glasscode_core::gate::FromMac) {
    if let Err(err) = copy.receive() {
        log(format!("authority from the Mac not kept: {err}"));
    }
}

/// The gate's set on the server: the Mac's copy, at most Careful once it is a day old, with the
/// stricter of the Mac's pause and the server's own. A copy that cannot be read is Careful, and
/// the manager there is paused.
pub fn server_gate() -> glasscode_core::gate::PolicySet {
    use glasscode_core::store::Store as _;
    let now = glasscode_core::clock::now_ms();
    let own = glasscode_core::gate::Pause::load().unwrap_or_else(|_| glasscode_core::gate::Pause::everything());
    match glasscode_core::gate::FromMac::load() {
        Ok(copy) => copy.policy_set(&own, now),
        Err(err) => {
            log(format!("authority from the Mac: {err}"));
            glasscode_core::gate::PolicySet::fail_closed()
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use glasscode_core::owners::{OwnerRecord, Owners};
    use glasscode_core::paths::MachineId;
    use glasscode_core::store::Store;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    use glasscode_core::caller::{Caller, DECLARE_METHOD};
    use glasscode_core::remote::Response;
    use serde_json::{Value, json};

    /// One end of a connection the daemon serves as it serves its socket, with `refuse` in front
    /// of a handler that answers every request with its caller.
    fn served() -> (UnixStream, BufReader<UnixStream>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            super::serve_connection(theirs, |caller, method, _| {
                super::refuse(&caller, method)?;
                Ok(serde_json::to_value(caller)?)
            })
        });
        let reader = BufReader::new(ours.try_clone().unwrap());
        (ours, reader)
    }

    fn send(stream: &mut UnixStream, id: u64, method: &str, params: Value) {
        let mut line = json!({ "id": id, "method": method, "params": params }).to_string();
        line.push('\n');
        stream.write_all(line.as_bytes()).unwrap();
    }

    fn read(reader: &mut BufReader<UnixStream>) -> Response {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn ask(stream: &mut UnixStream, reader: &mut BufReader<UnixStream>, id: u64, method: &str, params: Value) -> Result<Value, String> {
        send(stream, id, method, params);
        let response = read(reader);
        assert_eq!(response.id, id);
        match response.error {
            Some(error) => Err(error),
            None => Ok(response.result.unwrap_or(Value::Null)),
        }
    }

    fn machine() -> Value {
        json!({ "kind": "machine", "id": "0123456789abcdef" })
    }

    #[test]
    fn a_connection_that_declares_nothing_is_ui() {
        let (mut stream, mut reader) = served();
        assert_eq!(ask(&mut stream, &mut reader, 1, "state", Value::Null), Ok(json!({ "kind": "ui" })));
        // As before M13.2, the app's connection keeps everything but the moves' owner records.
        assert_eq!(ask(&mut stream, &mut reader, 2, "restart", Value::Null), Ok(json!({ "kind": "ui" })));
        assert!(ask(&mut stream, &mut reader, 3, "owners.begin", json!({})).is_err());
    }

    #[test]
    fn the_declaration_comes_before_every_later_line() {
        let (mut stream, mut reader) = served();
        // Sent together, without waiting: the request after the declaration is the Mac's.
        send(&mut stream, 1, DECLARE_METHOD, machine());
        send(&mut stream, 2, "owners.begin", json!({ "key": "Projects/app" }));
        let mut responses = [read(&mut reader), read(&mut reader)];
        responses.sort_by_key(|r| r.id);
        assert_eq!((responses[0].result.clone(), responses[0].error.clone()), (Some(machine()), None));
        assert_eq!((responses[1].result.clone(), responses[1].error.clone()), (Some(machine()), None));
        // Once: the Mac cannot become anyone else on this connection.
        assert!(ask(&mut stream, &mut reader, 3, DECLARE_METHOD, json!({ "kind": "ui" })).is_err());
        assert_eq!(ask(&mut stream, &mut reader, 4, "state", Value::Null), Ok(machine()));
    }

    #[test]
    fn a_phone_that_declares_is_refused() {
        let (mut stream, mut reader) = served();
        // The gate names the phone and reads the reply before it forwards anything.
        crate::declare_on(&stream, &Caller::Phone { device: "phone-1a2b".into() }).unwrap();
        let phone = json!({ "kind": "phone", "device": "phone-1a2b" });
        for (id, said) in [(1, json!({ "kind": "phone", "device": "phone-other" })), (2, json!({ "kind": "ui" }))] {
            assert_eq!(ask(&mut stream, &mut reader, id, DECLARE_METHOD, said), Err("a phone cannot declare a caller".into()));
        }
        assert_eq!(ask(&mut stream, &mut reader, 3, "sessions.send", json!({ "session_id": "s", "text": "hi" })), Ok(phone));
    }

    #[test]
    fn a_machine_declaration_from_a_phone_key_is_refused() {
        let (mut stream, mut reader) = served();
        crate::declare_on(&stream, &Caller::Phone { device: "unknown".into() }).unwrap();
        assert!(ask(&mut stream, &mut reader, 1, DECLARE_METHOD, machine()).is_err());
        // Still the phone: the moves' owner records stay out of reach.
        assert!(ask(&mut stream, &mut reader, 2, "owners.commit", json!({})).is_err());
        assert_eq!(ask(&mut stream, &mut reader, 3, "state", Value::Null), Ok(json!({ "kind": "phone", "device": "unknown" })));
    }

    #[test]
    fn restart_and_shutdown_from_a_phone_are_refused() {
        let (mut stream, mut reader) = served();
        crate::declare_on(&stream, &Caller::Phone { device: "phone-1a2b".into() }).unwrap();
        for (id, method) in [(1, "restart"), (2, "shutdown"), (3, "phones.add"), (4, "owners.begin"), (5, "owners.abort")] {
            assert!(ask(&mut stream, &mut reader, id, method, json!({})).is_err(), "{method}");
        }
        // Everything else stays open to a phone until phase 0b.
        for (id, method) in [(6, "state"), (7, "sessions.answer"), (8, "phones.remove"), (9, "today.op")] {
            assert!(ask(&mut stream, &mut reader, id, method, json!({})).is_ok(), "{method}");
        }
    }

    #[test]
    fn owners_only_from_a_machine() {
        for declared in [None, Some(json!({ "kind": "ui" })), Some(json!({ "kind": "phone", "device": "p" })), Some(machine())] {
            let (mut stream, mut reader) = served();
            let is_machine = declared.as_ref() == Some(&machine());
            if let Some(declared) = declared {
                ask(&mut stream, &mut reader, 1, DECLARE_METHOD, declared).unwrap();
            }
            for (id, method) in [(2, "owners.begin"), (3, "owners.commit"), (4, "owners.abort"), (5, "owners.get")] {
                assert_eq!(ask(&mut stream, &mut reader, id, method, json!({})).is_ok(), is_machine, "{method}");
            }
        }
    }

    #[test]
    fn old_authorized_keys_lines_still_work() {
        let args = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        // `command="… glasscoded gate"`, written before M13.2: the device nobody named.
        assert_eq!(crate::gate_device(&args(&[])).unwrap(), "unknown");
        assert_eq!(crate::gate_device(&args(&["--device", "phone-0123456789abcdef"])).unwrap(), "phone-0123456789abcdef");
        for bad in [&["--device"][..], &["--device", "a\"b"], &["--device", ""], &["rpc"], &["--device", "p", "x"]] {
            assert!(crate::gate_device(&args(bad)).is_err(), "{bad:?}");
        }
    }

    /// The tests that set the process's folders (`HOME`, `GLASSCODE_HOME`) run one at a time, even
    /// when the others run in parallel, and put them back when they end, so no test reads
    /// another's folder (a session that should be refused then went on to start tmux).
    pub(crate) struct Scratch {
        before: Vec<(&'static str, Option<std::ffi::OsString>)>,
        _one_at_a_time: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            for (name, value) in self.before.drain(..) {
                // SAFETY: as in `scratch_home`.
                match value {
                    Some(value) => unsafe { std::env::set_var(name, value) },
                    None => unsafe { std::env::remove_var(name) },
                }
            }
        }
    }

    pub(crate) fn scratch_home() -> (tempfile::TempDir, Scratch) {
        static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
        let lock = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().unwrap();
        let before = ["HOME", "GLASSCODE_HOME"].iter().map(|name| (*name, std::env::var_os(name))).collect();
        // SAFETY: the tests that change these hold `ONE_AT_A_TIME`; the others do not read them.
        unsafe {
            std::env::set_var("HOME", home.path());
            std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
        }
        (home, Scratch { before, _one_at_a_time: lock })
    }

    /// A second Mac is an observer (M57): the server keeps its primary Mac's authority; the other
    /// Mac is never the manager's home, and takes back from the server only what came from it.
    #[test]
    fn a_second_mac_is_an_observer() {
        let (home, _scratch) = scratch_home();
        let (a, b) = (MachineId::parse("00000000000000aa").unwrap(), MachineId::parse("00000000000000bb").unwrap());
        let now = glasscode_core::clock::now_ms();
        glasscode_core::store::update::<glasscode_core::gate::FromMac, ()>(|copy| {
            copy.from = Some(a.clone());
            copy.received_ms = now;
        })
        .unwrap();
        assert!(crate::mirror::is_primary(&a) && !crate::mirror::is_primary(&b));
        let (on_a, on_b) = (Caller::Machine { id: a.clone() }, Caller::Machine { id: b.clone() });
        let claim_b = crate::home::dispatch(&on_b, "home.claim", &json!({})).unwrap();
        assert!(claim_b.unwrap_err().to_string().contains("watches"));
        assert!(crate::home::dispatch(&on_a, "home.claim", &json!({})).unwrap().is_ok());

        // A worktree that came from A to the server: A takes it back, B does not.
        let server = crate::owners::machine().unwrap();
        let key = WorktreeKey::of(&home.path().join(".glasscode/worktrees/app/fix"), home.path(), "app", Some("fix"));
        let mut owners = Owners::default();
        owners.records.insert(key.clone(), OwnerRecord { owner: server.clone(), epoch: 1, moving: None });
        owners.came_from.insert(key.clone(), a.clone());
        owners.save().unwrap();
        let shared: Shared = Arc::new(Mutex::new(Daemon::load(HashMap::new()).unwrap()));
        let begin = |to: &MachineId| json!({ "key": key, "from": server, "to": to, "epoch": 1 });
        let by_b = crate::owners::dispatch(&shared, &on_b, "owners.begin", &begin(&b)).unwrap();
        assert!(by_b.unwrap_err().to_string().contains("another Mac's"));
        assert!(crate::owners::dispatch(&shared, &on_a, "owners.begin", &begin(&a)).unwrap().is_ok());
    }

    /// The manager on the server acts on this server's sessions (M57): a card the person approved
    /// renames and closes one through the daemon's own handlers; a session that is not running is
    /// not typed into; a wake reaches no session outside its projects.
    #[test]
    fn the_manager_acts_on_the_sessions_here() {
        use glasscode_core::ops::{ErrorCode, OpError, OpReply};
        use glasscode_core::runtime::Host as _;
        let (home, _scratch) = scratch_home();
        let mut daemon = Daemon::load(HashMap::new()).unwrap();
        daemon.state.projects.push(serde_json::from_value(json!({ "id": "p1", "name": "app", "root": home.path().join("app") })).unwrap());
        let session: Session = serde_json::from_value(json!({ "id": "s-here", "project_id": "p1", "worktree": home.path().join("app"), "branch": "fix", "title": "Fix the login", "agent": "claude", "created_at": 0 })).unwrap();
        daemon.state.sessions.push(session);
        let shared: Shared = Arc::new(Mutex::new(daemon));
        let machine = crate::owners::machine().unwrap();
        let generation = crate::home::server_generation().unwrap();
        let projects = vec![glasscode_core::runtime::situation::Project { id: "app-3f9a1c".into(), name: "app".into(), ..Default::default() }];
        let wakes = Arc::new(Mutex::new(HashMap::from([("w1".to_string(), vec!["web-8b2d10".to_string()])])));
        let host = crate::brain_host::DaemonHost::at_home(machine, generation, wakes, projects, Some(shared.clone()));
        let wake = Caller::Manager { lane: glasscode_core::caller::Lane::Wake, wake: Some("w1".into()), project: None };
        let op = |name: &str, args: Value| glasscode_core::ops::typed(name, args).unwrap();

        // The targets: the session and its project, by the project's name.
        let targets = host.targets_here(&op("session.rename", json!({ "session": "s-here", "name": "x" }))).unwrap();
        assert_eq!(targets.sessions, ["s-here"]);
        assert_eq!(targets.projects.iter().map(|k| k.to_string()).collect::<Vec<_>>(), ["app-3f9a1c"]);
        // A wake about another project reaches nothing here.
        assert!(matches!(host.run_op(&op("session.interrupt", json!({ "session": "s-here" })), &wake), OpReply::Failed(OpError { code: ErrorCode::Denied, .. })));
        // Approved by the person: renamed, then closed, through the daemon's handlers.
        assert!(matches!(host.act(&op("session.rename", json!({ "session": "s-here", "name": "Fix the sign-in" })), &wake, true), Some(OpReply::Done { .. })));
        assert_eq!(lock(&shared).state.session("s-here").unwrap().title, "Fix the sign-in");
        assert!(matches!(host.act(&op("session.interrupt", json!({ "session": "s-here" })), &wake, true), Some(OpReply::Failed(OpError { code: ErrorCode::NotFound, .. }))), "not running: nothing typed");
        assert!(matches!(host.act(&op("session.close", json!({ "session": "s-here" })), &wake, true), Some(OpReply::Done { .. })));
        assert!(lock(&shared).state.session("s-here").is_none());
        // A new session needs a project of this server's.
        assert!(host.targets_here(&op("session.new", json!({ "project": "nowhere" }))).is_err());
    }

    /// A new agent starts only where this server may run it: never in a worktree another machine
    /// owns (and a new session there is refused with words for the person), never while held for
    /// a move, and a step of a move that is not under way is refused. (The owner records are
    /// read once per process, so this is the only test here that uses them.)
    #[test]
    fn a_session_starts_only_in_a_worktree_this_server_owns() {
        let (home, _scratch) = scratch_home();
        let worktree = home.path().join(".glasscode/worktrees/app/fix");
        std::fs::create_dir_all(&worktree).unwrap();
        let key = WorktreeKey::of(&worktree, home.path(), "app", Some("fix"));
        let other = MachineId::parse("0123456789abcdef").unwrap();
        let mut owners = Owners::default();
        owners.records.insert(key.clone(), OwnerRecord { owner: other, epoch: 2, moving: None });
        owners.save().unwrap();

        let mut daemon = Daemon::load(HashMap::new()).unwrap();
        daemon.state.projects.push(serde_json::from_value(json!({ "id": "p1", "name": "app", "root": home.path().join("app") })).unwrap());
        let session: Session = serde_json::from_value(json!({ "id": "s1", "project_id": "p1", "worktree": worktree, "branch": "fix", "title": "Shell · fix", "agent": "shell", "created_at": 0 })).unwrap();
        daemon.state.sessions.push(session);
        let refused = daemon.start("s1").unwrap_err();
        assert!(format!("{refused:#}").contains("cannot run here"), "{refused:#}");
        assert!(crate::owners::why_not_here(&key).contains("on the Mac; use Server mode"));
        assert!(!crate::owners::may_run_here(&key));

        daemon.runtime.held.insert("s1".into(), MoveTag { key: key.clone(), epoch: 3, from: "mac-session".into() });
        let held = daemon.start("s1").unwrap_err();
        assert!(format!("{held:#}").contains("starts when its move"), "{held:#}");
        assert!(crate::owners::moving_here(&key, 3).is_err(), "no move is under way");
    }

    /// crit-5: the Mac's approved authority comes with its sync and is kept on the server, one way:
    /// no reply carries authority back, a copy that does not match its hash (one a server session
    /// raised to Autopilot) is not kept, and what the copy never allows is refused on the server.
    #[test]
    fn server_never_sends_authority() {
        use glasscode_core::gate::{FromMac, Level, Policy, ProjectPolicy};
        let (home, _scratch) = scratch_home();
        let mut policy = Policy::default();
        policy.projects.insert(glasscode_core::paths::ProjectKey::parse("app-3f9a1c").unwrap(), ProjectPolicy { level: Level::Standard, ..ProjectPolicy::default() });
        let copy = FromMac { from: MachineId::parse("0123456789abcdef"), hash: policy.hash(), policy: policy.clone(), ..FromMac::default() };
        let shared: Shared = Arc::new(Mutex::new(Daemon::load(HashMap::new()).unwrap()));
        let mac = glasscode_core::caller::Caller::Machine { id: copy.from.clone().unwrap() };
        // Not from a connection that is not that Mac: a phone, an undeclared one, another machine.
        let params = json!({ "mac": "Mac", "home": home.path(), "sessions": [], "authority": copy, "mac_id": "m1" });
        let others = [glasscode_core::caller::Caller::Phone { device: "p1".into() }, glasscode_core::caller::Caller::Ui, glasscode_core::caller::Caller::Machine { id: MachineId::parse("fedcba9876543210").unwrap() }];
        for other in &others {
            crate::mirror::dispatch(&shared, other, "mirror.sync", &params).unwrap().unwrap();
            assert!(FromMac::load().unwrap().from.is_none(), "{other:?}");
        }
        let reply = crate::mirror::dispatch(&shared, &mac, "mirror.sync", &params).unwrap().unwrap();
        let text = reply.to_string();
        for word in ["authority", "policy", "app-3f9a1c", "standard", "devices", "pause"] {
            assert!(!text.contains(word), "the reply carries {word}: {text}");
        }
        let kept = FromMac::load().unwrap();
        assert_eq!((kept.policy.clone(), kept.from.clone()), (policy.clone(), copy.from.clone()));
        assert!(kept.received_ms > 0);
        // A copy edited on its way (or on the server) is not kept; the approved one stays.
        let mut forged = copy.clone();
        forged.policy.default.level = Level::Autopilot;
        let params = json!({ "mac": "Mac", "home": home.path(), "sessions": [], "authority": forged, "mac_id": "m1" });
        crate::mirror::dispatch(&shared, &mac, "mirror.sync", &params).unwrap().unwrap();
        assert_eq!(FromMac::load().unwrap().policy, policy);
        // The Mac reads no authority from a reply, whatever it says.
        let back: glasscode_core::remote::MirrorSyncReply = serde_json::from_value(json!({ "authority": serde_json::to_value(&forged).unwrap(), "policy": {} })).unwrap();
        assert!(!serde_json::to_string(&back).unwrap().contains("autopilot"));
        // What the copy never allows a server session, the server refuses itself.
        use glasscode_core::control;
        let approve = control::Request { v: control::VERSION, args: ["manager", "message", "approve", "m1"].map(String::from).to_vec(), ..Default::default() };
        let why = crate::today::refused_here(&approve, Some("s1")).expect("a person-only op from a server session");
        assert!(why.contains("only you"), "{why}");
        let read = control::Request { v: control::VERSION, args: ["board", "list"].map(String::from).to_vec(), ..Default::default() };
        assert_eq!(crate::today::refused_here(&read, Some("s1")), None);
    }

    #[test]
    fn a_phones_words_are_what_it_types_into_a_session() {
        use super::phone_words;
        let send = r#"{"id":4,"method":"sessions.send","params":{"session_id":"s-1","text":"  fix the login test ","enter":true}}"#;
        assert_eq!(phone_words(send), Some(("s-1".to_string(), "fix the login test".to_string())));
        let answer = r#"{"id":5,"method":"sessions.answer","params":{"session_id":"s-1","option":3,"text":"use the staging database"}}"#;
        assert_eq!(phone_words(answer), Some(("s-1".to_string(), "use the staging database".to_string())));
        // An answer without words, a key, a read, a line that is not a request: nothing.
        for line in [r#"{"id":6,"method":"sessions.answer","params":{"session_id":"s-1","option":1,"text":null}}"#, r#"{"id":7,"method":"sessions.keys","params":{"session_id":"s-1","keys":["Enter"]}}"#, r#"{"id":8,"method":"state","params":null}"#, "not json", ""] {
            assert_eq!(phone_words(line), None, "{line}");
        }
    }

    /// M58: the arbiter registers a home's outward effects only at its generation, and when home
    /// moves on, what the old home claimed without a result is Uncertain (it never runs twice).
    #[test]
    fn the_arbiter_fences_effects_by_generation() {
        let (_home, _scratch) = scratch_home();
        let mac = glasscode_core::caller::Caller::Machine { id: MachineId::parse("00000000000000aa").unwrap() };
        let call = |method: &str, params: Value| crate::home::dispatch(&mac, method, &params).unwrap();
        let granted: glasscode_core::home::HomeRecord = serde_json::from_value(call("home.claim", json!({})).unwrap()).unwrap();
        assert_eq!(granted.generation, 1);
        let intent = glasscode_core::effects::Intent::new(glasscode_core::effects::effect_key("manager", "pr#1", "github.comment", "acme/app#1"), "github.comment", glasscode_core::ops::Retry::AtMostOnce);
        let claim = call("home.claim_effect", json!({ "intent": intent, "generation": 1 })).unwrap();
        assert!(matches!(serde_json::from_value::<glasscode_core::effects::Claim>(claim).unwrap(), glasscode_core::effects::Claim::New(_)));
        let again = call("home.claim_effect", json!({ "intent": intent, "generation": 1 })).unwrap();
        assert!(matches!(serde_json::from_value::<glasscode_core::effects::Claim>(again).unwrap(), glasscode_core::effects::Claim::Already(_)), "the same effect is not claimed twice");
        assert!(call("home.release", json!({ "generation": 1 })).unwrap()["ok"].as_bool().unwrap());
        assert!(call("home.claim_effect", json!({ "intent": intent, "generation": 1 })).is_err(), "an older generation runs nothing outward");
        let record = glasscode_core::effects::find(&intent.key).unwrap().unwrap();
        assert_eq!(record.state, glasscode_core::effects::EffectState::Uncertain, "the old home's unfinished claim is checked before anything tries it again");
        // Only a machine's own connection asks.
        assert!(dispatch(&Arc::new(Mutex::new(Daemon::load(HashMap::new()).unwrap())), &glasscode_core::caller::Caller::Ui, "home.claim", json!({})).is_err());
    }

    #[test]
    fn names_branches_from_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(dir.path()).args(args).output().unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "init"]);
        assert_eq!(branch_for_task("Fix the flaky retry in the webhook handler", dir.path()), "fix/flaky-retry-webhook-handler");
        assert_eq!(branch_for_task("Add an export button to chat history", dir.path()), "feat/export-button-chat-history");
        git(&["branch", "fix/flaky-retry-webhook-handler"]);
        assert_eq!(branch_for_task("fix flaky retry webhook handler", dir.path()), "fix/flaky-retry-webhook-handler-2");
        assert_eq!(branch_for_task("!!", dir.path()), "feat/task");
    }
}
