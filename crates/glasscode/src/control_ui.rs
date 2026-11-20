//! The `glasscode` command's way into the app: a Unix socket or a named pipe only this user can
//! open (and, for sandboxed agents, request files in their worktree), who is calling, what they
//! may do (Settings › Permissions), the requests that wait for the person's Allow or Deny, and the
//! log of what agents did. The registry parses each command (`glasscode_core::ops`), and
//! `ops_run.rs` has the handlers, keyed by op name.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use gpui::{AppContext as _, Context, Window};
use glasscode_core::caller::{self, Channel, Signed};
use glasscode_core::control::{self, Kind, LogEntry, Reply, Request, Rule, code};
use glasscode_core::gate::{self, Decision};
use glasscode_core::model::AgentKind;
use glasscode_core::ops::{self, Op};
use glasscode_core::sys;
use glasscode_core::words::{self, InjectKind, Origin};

use crate::workspace::Workspace;

/// Where an answer goes.
pub enum Sink {
    Socket(mpsc::Sender<Reply>),
    /// A sandboxed command's answer file (lines, like the socket).
    File(PathBuf),
    /// A session on the server asked (through the server): answers go back with the next sync.
    Remote(u64, Arc<Mutex<Vec<glasscode_core::remote::RemoteCallReply>>>),
}

impl Sink {
    fn send(&self, reply: &Reply) {
        match self {
            Sink::Socket(tx) => {
                let _ = tx.send(reply.clone());
            }
            Sink::File(path) => {
                let Ok(line) = serde_json::to_string(reply) else { return };
                if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                    let _ = writeln!(file, "{line}");
                }
            }
            Sink::Remote(id, out) => {
                if let Ok(mut out) = out.lock() {
                    out.push(glasscode_core::remote::RemoteCallReply { id: *id, reply: reply.clone() });
                }
            }
        }
    }
}

/// How a request reached the app, for deciding who is asking.
#[derive(Clone, Copy, Debug)]
pub enum Source {
    /// A socket connection, and the process on the other end.
    Socket(Option<u32>),
    /// A request file dropped in a worktree (anyone may drop one, so never the person).
    File,
}

/// Which rules of Settings › Permissions apply (v1's three answers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// The person: the app itself, or a `glasscode` command they typed in a Glasscode shell tab
    /// (with the engine at v1, also any terminal that says it is typed at).
    You,
    /// An agent in a session Glasscode started.
    Session,
    /// Anything else: a program Glasscode did not start, a terminal outside Glasscode.
    Outside,
}

#[derive(Clone, Debug)]
pub struct Caller {
    /// "Claude", "Codex", "You", "A program outside Glasscode".
    pub who: String,
    pub trust: Trust,
    /// Who it is by the channel it came in on (`glasscode_core::caller`): what the v2 gate reads.
    pub channel: caller::Caller,
    pub session: Option<String>,
    pub session_title: Option<String>,
    pub agent: Option<AgentKind>,
    pub cwd: Option<PathBuf>,
    /// The project a command's session belongs to (a server session sends it; the Mac has no
    /// such session to look up).
    pub project: Option<String>,
}

impl Caller {
    /// The person, in the app itself (a button, a tick of the app's own).
    pub fn you() -> Caller {
        Caller { who: "You".into(), trust: Trust::You, channel: caller::Caller::Ui, session: None, session_title: None, agent: None, cwd: None, project: None }
    }

    /// "Claude in “API connectors”".
    pub fn describe(&self) -> String {
        match &self.session_title {
            Some(title) if self.trust == Trust::Session => format!("{} in “{title}”", self.who),
            _ => self.who.clone(),
        }
    }

    pub fn origin(&self) -> glasscode_core::board::Origin {
        glasscode_core::board::Origin { who: self.who.clone(), session: self.session.clone(), session_title: self.session_title.clone() }
    }

    /// The manager's own agent: it runs under the app like a session's agent, but in no session
    /// (every other agent caller has one).
    pub fn is_manager(&self) -> bool {
        self.trust == Trust::Session && self.session.is_none()
    }
}

/// What a command does once it may.
pub type Run = Box<dyn FnOnce(&mut Workspace, Responder, &mut Window, &mut Context<Workspace>)>;

pub struct Plan {
    pub kind: Kind,
    /// For an approval and the log: "remove the worktree feat-api-connector".
    pub what: String,
    pub run: Run,
    /// What the plan checks with git, gh or a file before anyone is asked (a branch that exists
    /// already, a project not on GitHub): run on the background executor, never the main thread
    /// (ops-9), and still before the rules or the gate ask the person (`Workspace::after_check`).
    pub check: Option<Check>,
}

/// A plan's check (`Plan::check`): nothing, or the reply that refuses it.
pub type Check = Box<dyn FnOnce() -> Result<(), Reply> + Send>;

/// Answers one request (and logs it).
pub struct Responder {
    sinks: Vec<Sink>,
    pub caller: Caller,
    command: String,
    what: String,
    kind: Kind,
    approval: Option<u64>,
    /// The op it answers, and what it touches (`ops::targets`): its reply says which op it was,
    /// and M14 journals both.
    op: Option<Op>,
    targets: Option<ops::Targets>,
    /// How it came to run: allowed by a rule, or waited and was approved or denied.
    decided: ops::Decided,
    /// For the app's own buttons: what to do with the answer (nothing goes to the agents' log; an
    /// op's reply still leaves its journal record).
    on_done: Option<Box<dyn FnOnce(&mut Workspace, &Reply)>>,
    /// The pending action it runs, once approved (v2): its reply marks it ran.
    pending: Option<String>,
}

impl Responder {
    /// The app runs an op for `caller` (`Workspace::run_op`): `done` hears the answer, and the
    /// reply leaves the journal's record of it, as the command's does.
    pub fn for_op(caller: Caller, op: Op, targets: Option<ops::Targets>, plan: &Plan, done: impl FnOnce(&mut Workspace, &Reply) + 'static) -> Responder {
        Responder {
            sinks: Vec::new(),
            caller,
            command: String::new(),
            what: plan.what.clone(),
            kind: plan.kind,
            approval: None,
            op: Some(op),
            targets,
            decided: ops::Decided::Allowed,
            on_done: Some(Box::new(done)),
            pending: None,
        }
    }

    /// The app itself (a button) runs a command: `done` hears the answer.
    pub fn local(done: impl FnOnce(&mut Workspace, &Reply) + 'static) -> Responder {
        Responder {
            sinks: Vec::new(),
            caller: Caller::you(),
            command: String::new(),
            what: String::new(),
            kind: Kind::Read,
            approval: None,
            op: None,
            targets: None,
            decided: ops::Decided::Allowed,
            on_done: Some(Box::new(done)),
            pending: None,
        }
    }
}

pub struct Approval {
    pub id: u64,
    pub caller: Caller,
    pub kind: Kind,
    pub what: String,
    /// The rule that asks (the v2 gate's words), when there is one.
    pub why: Option<String>,
    pub command: String,
    pub created: u64,
    responder: Option<Responder>,
    run: Option<Run>,
    /// With the v2 engine: the pending action it is (M21.2), kept in `authority/pending.json`.
    /// Approving it runs exactly its stored op, after the gate and its targets are checked again;
    /// it holds no closure.
    pub pending: Option<gate::PendingAction>,
    /// Allow may be pressed outside the card (a popup, a phone): the op is local and reversible.
    /// Anything that leaves the Mac or removes something shows "Review" instead.
    pub allow_here: bool,
    /// "Always allow this in <project>" is offered for this project.
    pub always_in: Option<gate::ProjectKey>,
}

impl Approval {
    /// "Claude in “API connectors” wants to remove the worktree feat-api-connector".
    pub fn summary(&self) -> String {
        format!("{} wants to {}", self.caller.describe(), self.what)
    }
}

pub struct ControlUi {
    secret: [u8; 32],
    /// This run of the app: an iPhone's answer names it, so a number from an earlier run is
    /// never taken for a new request with the same number.
    pub launch: String,
    pub approvals: Vec<Approval>,
    next_approval: u64,
    /// Answers to requests that waited (for `glasscode wait`).
    results: HashMap<u64, Reply>,
    /// Ops a caller started without waiting for them (`Request.detach`, M33.2): their id (from the
    /// same count as approvals) and what they do, until their result is in `results`.
    running: HashMap<u64, String>,
    /// "Allow for this session".
    allowed: HashMap<String, HashSet<Kind>>,
    /// What agents did, newest first (the recent part of the log file).
    pub log: Vec<LogEntry>,
    /// The socket is open.
    pub listening: bool,
    pub error: Option<String>,
    tx: Option<async_channel::Sender<(Request, Source, Sink)>>,
    excluded: HashSet<PathBuf>,
    /// Answers for commands from server sessions, sent with the next sync.
    pub remote_out: Arc<Mutex<Vec<glasscode_core::remote::RemoteCallReply>>>,
    /// The engine switch says v2 (read once, off the main thread, when the socket opens, before
    /// any request is answered): callers by channel decide what a terminal may do (O13).
    pub engine_v2: bool,
    /// What the replies of the last ops were, newest first: the journal's records of them, which
    /// M14 writes (ops-21). In memory only until then.
    pub actions: Vec<ops::Action>,
    /// The key each of the app's projects is bound to on this machine (M11's
    /// `authority/projects.json`), for the registry's `World`: read with the engine switch, and
    /// only with the engine at v2 (a v1 home gets no machine id from it).
    pub project_keys: std::collections::BTreeMap<String, glasscode_core::paths::ProjectKey>,
    /// The first prompt of a session that is about to start, when the person typed it in a
    /// composer (the Today box): its origin and what of it was not typed. The launch records it.
    pub first_words: HashMap<String, (Origin, Vec<std::ops::Range<usize>>)>,
    /// The approved authority (`glasscode_core::gate`), read off the main thread with the engine at
    /// v2 and again every half minute: what the gate decides by. At v1 it is never read.
    pub authority: gate::Authority,
    /// The live policy file the import screen last showed (its hash), so it opens once per change.
    pub policy_shown: Option<String>,
    /// The brains whose first tool list the app checked (`control_brain_checked`): their MCP
    /// servers' calls are served (M34; brain-m5). Dropped at quit.
    pub brains_checked: HashSet<(glasscode_core::caller::Lane, Option<String>)>,
    /// "Allow for the rest of the session" (v2): this op on these targets, on the monotonic clock,
    /// dropped on a restart and when the Mac wakes (M21.2).
    pub short: gate::SessionGrants,
    /// Where the monotonic clock of `short` starts.
    pub started: std::time::Instant,
    /// The latest wall time the pending store has seen: an expiry never moves back with the clock.
    pub pending_high: u64,
    /// The cards whose full payload is open.
    pub expanded: HashSet<u64>,
    /// Session profiles (M22): the v2 engine with the "profiles" switch on.
    pub profiles: bool,
    /// The profile each session's agent runs under now (M22): a session the manager sends text to
    /// gets the automation profile at its next start, not while it runs.
    pub profile_running: HashMap<String, gate::SessionOrigin>,
}

impl ControlUi {
    pub fn new() -> Self {
        let mut secret = [0u8; 32];
        glasscode_core::sys::random_bytes(&mut secret);
        ControlUi {
            secret,
            launch: glasscode_core::model::new_id(),
            approvals: Vec::new(),
            next_approval: 1,
            running: HashMap::new(),
            results: HashMap::new(),
            allowed: HashMap::new(),
            log: control::read_log(200),
            listening: false,
            error: None,
            tx: None,
            excluded: HashSet::new(),
            remote_out: Default::default(),
            engine_v2: false,
            actions: Vec::new(),
            project_keys: Default::default(),
            first_words: HashMap::new(),
            authority: gate::Authority { policy: Default::default(), live_differs: true, pause: gate::Pause::everything(), ..Default::default() },
            policy_shown: None,
            brains_checked: HashSet::new(),
            short: gate::SessionGrants::default(),
            started: std::time::Instant::now(),
            pending_high: 0,
            expanded: HashSet::new(),
            profiles: false,
            profile_running: HashMap::new(),
        }
    }

    /// The signature a session gets (`GLASSCODE_SESSION_SIG`).
    pub fn signature(&self, session_id: &str) -> String {
        control::sign(&self.secret, session_id)
    }

    pub fn pending_for(&self, session_id: &str) -> Vec<&Approval> {
        self.approvals.iter().filter(|a| a.caller.session.as_deref() == Some(session_id)).collect()
    }
}

/// Settle a pending action in the store, through the ordered writer.
fn settle_later(id: String, state: gate::PendingState, outcome: Option<String>) {
    pending_write(move || match gate::settle_pending(&id, state, outcome) {
        Ok(Ok(_)) => {}
        Ok(Err(said)) => log::info!("pending {id}: {said}"),
        Err(err) => log::warn!("pending {id}: {err}"),
    });
}

/// An approved pending action that no longer holds: marked so, the person told why, its caller
/// answered. Nothing ran.
fn fail_pending(ws: &mut Workspace, mut responder: Responder, id: String, said: String, cx: &mut Context<Workspace>) {
    settle_later(id, if said.contains("waited too long") { gate::PendingState::Expired } else { gate::PendingState::Denied }, Some(said.clone()));
    responder.decided = ops::Decided::Denied;
    ws.show_toast(said.clone(), cx);
    ws.reply(responder, Reply::fail(code::FAILED, said));
}

/// Writes to the pending store, in the order they were asked for, on one thread of their own: an
/// approval is never settled before it is added, and the main thread never waits on the disk.
fn pending_write(write: impl FnOnce() + Send + 'static) {
    type Write = Box<dyn FnOnce() + Send>;
    static WRITER: std::sync::OnceLock<Mutex<mpsc::Sender<Write>>> = std::sync::OnceLock::new();
    let writer = WRITER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Write>();
        let _ = std::thread::Builder::new().name("glasscode-pending".into()).spawn(move || {
            for write in rx {
                write();
            }
        });
        Mutex::new(tx)
    });
    let sent = writer.lock().map(|tx| tx.send(Box::new(write)).is_ok()).unwrap_or(false);
    if !sent {
        log::warn!("pending: the writer has stopped; a change was not kept");
    }
}

/// The pending store, as a start reads it: an action approved before a quit is uncertain now, and
/// those past their expiry are expired. It reads and writes the disk: never on the main thread.
fn load_pending() -> Result<gate::Pending, String> {
    let now = glasscode_core::clock::now_ms();
    glasscode_core::store::update::<gate::Pending, gate::Pending>(|pending| {
        pending.after_restart(now);
        pending.expire(now);
        pending.prune(now);
        pending.clone()
    })
    .map_err(|err| err.to_string())
}

/// Whether an approval may be allowed outside its card (a popup, a phone, "for the rest of the
/// session"): a local, reversible op. What leaves the Mac or removes something is reviewed on the
/// card, with its full payload.
pub fn allow_here(op: &Op, targets: &ops::Targets) -> bool {
    !targets.leaves_machine && !matches!(op.spec.effect, ops::EffectClass::Outward | ops::EffectClass::Destructive) && !op.spec.human_only
}

/// This machine's bindings of project ids to keys (`identity::Projects::bound`); none when they
/// cannot be read (the registry then names a project by its folder alone). It reads the disk:
/// never on the main thread.
pub fn project_keys() -> std::collections::BTreeMap<String, glasscode_core::paths::ProjectKey> {
    use glasscode_core::store::Store as _;
    let read = || -> Result<_, glasscode_core::store::StoreError> { Ok(glasscode_core::identity::Projects::load()?.bound(&glasscode_core::paths::MachineId::this()?)) };
    read().unwrap_or_else(|err| {
        log::warn!("project keys: {err}");
        Default::default()
    })
}

/// Whether a request file is tracked by git (committed into the repository).
fn tracked(worktree: &std::path::Path, path: &std::path::Path) -> bool {
    glasscode_core::git::is_tracked(worktree, path)
}

/// One connection: a request line in, answer lines out.
fn serve(stream: sys::control::Stream, tx: async_channel::Sender<(Request, Source, Sink)>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let peer = stream.peer_pid();
    let Ok(read) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut out = stream;
    let request: Request = match serde_json::from_str(line.trim()) {
        Ok(r) => r,
        Err(e) => {
            let reply = Reply::fail(code::USAGE, format!("Glasscode could not read the request: {e}"));
            let _ = writeln!(out, "{}", serde_json::to_string(&reply).unwrap_or_default());
            return;
        }
    };
    let (reply_tx, reply_rx) = mpsc::channel::<Reply>();
    if tx.send_blocking((request, Source::Socket(peer), Sink::Socket(reply_tx))).is_err() {
        return;
    }
    while let Ok(reply) = reply_rx.recv() {
        let done = matches!(reply, Reply::Done { .. });
        // The command may have stopped waiting; the answer is kept for `glasscode wait`.
        let _ = writeln!(out, "{}", serde_json::to_string(&reply).unwrap_or_default());
        if done {
            break;
        }
    }
}

/// A command line again, for the log ("glasscode board add 'Reply to Sam'").
fn shell_line(args: &[String]) -> String {
    let words: Vec<String> = args
        .iter()
        .map(|a| if a.is_empty() || a.contains(|c: char| c.is_whitespace() || "'\"$`\\".contains(c)) { format!("'{}'", a.replace('\'', "'\\''")) } else { a.clone() })
        .collect();
    format!("glasscode {}", words.join(" "))
}

impl Workspace {
    /// Open the socket (a named pipe on Windows), write the `glasscode` command and what tells
    /// agents about it.
    pub fn control_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = control::socket_path();
        // Another Glasscode already answers there (a second copy): leave it be.
        if sys::control::connect(&path).is_ok() {
            self.control.error = Some("Another Glasscode is running; the glasscode command talks to that one.".into());
            return;
        }
        if let Err(e) = control::prepare_socket_dir(&path) {
            self.control.error = Some(format!("The glasscode command cannot reach this app ({e})."));
            log::warn!("control socket folder {}: {e}", path.display());
            return;
        }
        let listener = match sys::control::listen(&path) {
            Ok(l) => l,
            Err(e) => {
                self.control.error = Some(format!("The glasscode command cannot reach this app ({e})."));
                log::warn!("control socket {}: {e}", path.display());
                return;
            }
        };
        let (tx, rx) = async_channel::unbounded::<(Request, Source, Sink)>();
        self.control.tx = Some(tx.clone());
        self.control.listening = true;
        // The engine switch is a file: read on a background thread, before the first request is
        // answered (they wait in the channel meanwhile), so a v2 home never answers by v1's rule.
        // With it, at v2, the projects' keys (the same folder).
        let engine = cx.background_spawn(async {
            let switch = glasscode_core::store::EngineSwitch::current();
            let v2 = switch.engine == glasscode_core::store::Engine::V2;
            // M22: the app's own git calls on worktrees ignore what a session can configure.
            glasscode_core::sys::use_safe_git(v2);
            // And this machine's id, for the brain runtime's host (M50): a v1 home makes none.
            let machine = v2.then(|| glasscode_core::paths::MachineId::this().ok()).flatten();
            (v2, switch.is_on("profiles"), if v2 { project_keys() } else { Default::default() }, v2.then(|| (gate::load(), load_pending())), machine)
        });
        std::thread::Builder::new()
            .name("glasscode-control".into())
            .spawn(move || {
                loop {
                    let stream = match listener.accept() {
                        Ok(stream) => stream,
                        Err(e) => {
                            log::warn!("control socket: {e}");
                            // An error that stays (no more handles) must not keep a core busy.
                            std::thread::sleep(Duration::from_millis(100));
                            continue;
                        }
                    };
                    let tx = tx.clone();
                    let _ = std::thread::Builder::new().name("glasscode-control-conn".into()).spawn(move || serve(stream, tx));
                }
            })
            .ok();
        cx.spawn_in(window, async move |this, cx| {
            let (v2, profiles, keys, authority, machine) = engine.await;
            let started = this.update_in(cx, |this, window, cx| {
                (this.control.engine_v2, this.control.project_keys) = (v2, keys);
                this.control.profiles = v2 && profiles;
                if let Some(machine) = machine {
                    let calls = this.brain_host_start(window, cx);
                    this.brain_host = Some(std::sync::Arc::new(crate::brain_host::AppHost::new(machine, calls)));
                }
                if let Some((authority, pending)) = authority {
                    this.control_set_authority(authority, cx);
                    this.control_restore_pending(pending, cx);
                    this.control_watch_authority(cx);
                }
            });
            if started.is_err() {
                return;
            }
            while let Ok((request, source, sink)) = rx.recv().await {
                if this.update_in(cx, |this, window, cx| this.control_handle(request, source, sink, window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        self.control_write_files();
    }

    /// The launcher on the PATH (not for test copies with their own folder), the skill and
    /// Codex's instructions (unless switched off).
    pub fn control_write_files(&self) {
        let own_folder = std::env::var_os("GLASSCODE_HOME").is_some() || std::env::var_os("GLASSCODE_CONTROL_SOCKET").is_some();
        if let Ok(exe) = std::env::current_exe() {
            let bundled = exe.to_string_lossy().contains(".app/Contents/MacOS/");
            if bundled && !own_folder {
                if let Err(e) = glasscode_core::cli::write_launcher(&exe) {
                    log::warn!("could not write the glasscode launcher: {e:#}");
                }
            }
            // Windows: the command goes into Glasscode's folder, so a test copy with a folder of
            // its own (GLASSCODE_HOME) changes only that one. A copy that has only a channel of
            // its own uses the real folder, and leaves the real command as it is.
            let own_channel_only = std::env::var_os("GLASSCODE_HOME").is_none() && std::env::var_os("GLASSCODE_CONTROL_SOCKET").is_some();
            if cfg!(windows)
                && !own_channel_only
                && let Err(e) = glasscode_core::cli::install_command(&glasscode_core::cli::console_program(&exe))
            {
                log::warn!("could not install the glasscode command: {e:#}");
            }
        }
        if self.state.agents.skill_off {
            let _ = std::fs::remove_dir_all(glasscode_core::cli::plugin_dir());
        } else if let Err(e) = glasscode_core::cli::write_plugin() {
            log::warn!("could not write the glasscode skill: {e:#}");
        }
    }

    /// Sandboxed commands leave their request in `<worktree>/.glasscode/cli/`.
    pub fn control_scan(&mut self) {
        let Some(tx) = self.control.tx.clone() else { return };
        let worktrees: HashSet<PathBuf> = self.state.sessions.iter().filter(|s| self.terminals.contains_key(&s.id)).map(|s| s.worktree.clone()).collect();
        for worktree in worktrees {
            let dir = control::drop_dir(&worktree);
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            if self.control.excluded.insert(worktree.clone()) {
                let path = worktree.clone();
                std::thread::spawn(move || {
                    let _ = glasscode_core::git::exclude(&path, ".glasscode/");
                });
            }
            for entry in entries.flatten() {
                let path = entry.path();
                // A real request we would answer is a plain file nobody committed. A symlink or
                // a tracked file is someone trying to make us read or write somewhere else.
                let plain = entry.file_type().is_ok_and(|t| t.is_file());
                match path.extension().and_then(|e| e.to_str()) {
                    Some("req") if plain && !tracked(&worktree, &path) => {
                        let text = std::fs::read_to_string(&path).unwrap_or_default();
                        let _ = std::fs::remove_file(&path);
                        let out = path.with_extension("out");
                        // Make the answer file ourselves (never follow a symlink or append to a
                        // file that is already there).
                        let _ = std::fs::remove_file(&out);
                        match serde_json::from_str::<Request>(text.trim()) {
                            Ok(request) => {
                                let _ = tx.try_send((request, Source::File, Sink::File(out)));
                            }
                            Err(e) => Sink::File(out).send(&Reply::fail(code::USAGE, format!("Glasscode could not read the request: {e}"))),
                        }
                    }
                    Some("req") if plain => {
                        // A committed or linked request: refuse it, do not run it.
                        let _ = std::fs::remove_file(&path);
                    }
                    // Answers nobody collected.
                    Some("out" | "tmp") if plain => {
                        if entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().unwrap_or_default() > Duration::from_secs(86_400)) {
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                    _ => {
                        // Not a plain file we wrote: get it out of the way.
                        if !plain {
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                }
            }
        }
    }

    /// Who is asking, by the channel (`caller::by_channel`): a valid signature is its session; the
    /// person's own Enter in a shell tab makes a command from that shell theirs; a process under
    /// the manager's brain is the manager (v1's rule, attribution); anything else is a terminal,
    /// which may read and asks for the rest. The request's `interactive` flag decides nothing,
    /// except with the engine at v1, where a terminal that says it is typed at (and does not run
    /// under this app), and a signed shell session that says so, are still the person, as before
    /// (owner decision O13; `glasscode whoami` warns about it).
    pub fn control_caller(&self, request: &Request, source: Source) -> Caller {
        let cwd = request.cwd.as_ref().map(PathBuf::from);
        let channel = match source {
            Source::Socket(pid) => Channel::Socket { pid },
            Source::File => Channel::RequestFile,
        };
        let signed = |id: &str, sig: &str| (sig == self.control.signature(id)).then(|| self.state.session(id)).flatten().map(|session| Signed { shell: session.agent == AgentKind::Shell });
        let under_brain = |pid: u32| {
            let brains = glasscode_core::manager::brain::pids();
            !brains.is_empty() && {
                let parents = control::parents();
                brains.iter().any(|brain| control::descends(&parents, pid, *brain))
            }
        };
        // The shell-tab rule is v2's: at v1 a shell session is the person only as before.
        let v2 = self.control.engine_v2;
        // A brain's own MCP server (`glasscode mcp`, M34) carries the token the app signed for it.
        let found = glasscode_core::mcp::brain_caller(request, |text, sig| sig == self.control.signature(text)).unwrap_or_else(|| caller::by_channel(request, &channel, signed, under_brain, |session| v2 && caller::typed_command_recently(session)));
        let v1_person = !v2
            && request.interactive
            && match (&found, source) {
                (caller::Caller::Session { id, .. }, _) => self.state.session(id).is_some_and(|s| s.agent == AgentKind::Shell),
                (caller::Caller::Terminal { .. }, Source::Socket(Some(pid))) => !self.control_descends_from_us(pid),
                _ => false,
            };
        let you = || Caller { who: "You".into(), trust: Trust::You, channel: found.clone(), session: None, session_title: None, agent: None, cwd: cwd.clone(), project: None };
        let outside = |who: &str| Caller { who: who.into(), trust: Trust::Outside, channel: found.clone(), session: None, session_title: None, agent: None, cwd: cwd.clone(), project: None };
        match &found {
            caller::Caller::Session { id, .. } | caller::Caller::ShellTab { session: id, .. } => {
                // Signed for a session this app has (`signed`); one that went meanwhile is nobody.
                let Some(session) = self.state.session(id) else { return outside("A program outside Glasscode") };
                let title = Some(self.session_title(session));
                let (who, trust) = match session.agent {
                    _ if found.is_person() || v1_person => ("You".to_string(), Trust::You),
                    AgentKind::Shell => ("A program in a shell session".to_string(), Trust::Session),
                    agent => (agent.name().to_string(), Trust::Session),
                };
                Caller { who, trust, channel: found.clone(), session: Some(id.clone()), session_title: title, agent: Some(session.agent), cwd, project: None }
            }
            // The manager's own agent (`glasscode_core::manager::brain`): it is the manager, in no
            // session, and follows Settings › Automation like any agent (never the person).
            caller::Caller::Manager { .. } => Caller { who: self.state.manager.name.clone(), trust: Trust::Session, channel: found.clone(), session: None, session_title: None, agent: Some(self.state.manager.brain), cwd, project: None },
            _ if v1_person => you(),
            // A request file is droppable by anyone (including one committed to a repo), and a
            // terminal is anyone who says so: at most "outside".
            _ => outside(if request.interactive && self.control.engine_v2 { "A terminal outside Glasscode" } else { "A program outside Glasscode" }),
        }
    }

    /// The first prompt of `session`, as the agent is started with it: the person's, when they
    /// typed it in a composer ([`ControlUi::first_words`]), else what Glasscode made (from a
    /// ticket, a PR, a handoff, `session new --message`).
    pub fn control_record_first_prompt(&mut self, session: &glasscode_core::model::Session, prompt: &str) {
        let (origin, untyped) = self.control.first_words.remove(&session.id).unwrap_or_else(|| {
            let kind = if session.review.is_some() {
                InjectKind::Review
            } else if session.lineage.len() > 1 {
                InjectKind::Handoff
            } else {
                InjectKind::FirstPrompt
            };
            (Origin::Injected(kind), Vec::new())
        });
        words::record_with(origin, prompt, &untyped, Some(&session.id), None);
    }

    /// Whether `pid` runs under this app (our own agent and shell sessions do; the person's own
    /// terminal in another app does not).
    fn control_descends_from_us(&self, pid: u32) -> bool {
        let me = std::process::id();
        if pid == me {
            return true;
        }
        control::descends(&control::parents(), pid, me)
    }

    pub fn control_rule(&self, caller: &Caller, kind: Kind) -> Rule {
        let settings = &self.state.agents;
        match caller.trust {
            Trust::You => Rule::Allow,
            Trust::Session => {
                if caller.session.as_ref().and_then(|s| self.control.allowed.get(s)).is_some_and(|k| k.contains(&kind)) {
                    Rule::Allow
                } else {
                    settings.rule(kind)
                }
            }
            Trust::Outside => caller::outside_rule(kind, settings),
        }
    }

    /// What the v2 gate reads: the approved authority, the person's rules of Settings ›
    /// Permissions, and what the app knows of its sessions (their project, who started them, their
    /// branch, mode and account), never what a request says of itself.
    pub fn control_policy_set(&self) -> gate::PolicySet {
        let authority = &self.control.authority;
        let mut set = authority.policy_set(self.state.agents.clone());
        for s in &self.state.sessions {
            let started = authority.origins.as_ref().and_then(|o| o.sessions.get(&s.id));
            set.sessions.insert(
                s.id.clone(),
                gate::SessionFacts {
                    project: self.control.project_keys.get(&s.project_id).cloned(),
                    origin: authority.origin(&s.id),
                    started_by: started.map(|s| s.by.clone()),
                    branch: (!s.branch.is_empty()).then(|| s.branch.clone()),
                    mode: s.mode.clone(),
                    account: s.account_id.clone(),
                },
            );
        }
        // A server session the app has no record of may be in any project the server holds a copy
        // of (all of them, while the server's list is not here): the strictest decides (trust-3).
        let on_server: Vec<gate::ProjectKey> =
            self.state.projects.iter().filter(|p| crate::dialogs::server_counterpart(self, &p.id).is_some()).filter_map(|p| self.control.project_keys.get(&p.id).cloned()).collect();
        set.server_projects = if on_server.is_empty() { self.control.project_keys.values().cloned().collect() } else { on_server };
        set.manager_account = self.state.manager.account.clone();
        set
    }

    /// The v2 gate's decision for an op, with no grant (M21.2 adds them).
    pub fn control_gate(&self, caller: &Caller, op: &Op, targets: &ops::Targets) -> Decision {
        gate::decide(&caller.channel, op, targets, &self.control_policy_set(), None)
    }

    /// Take a newly read authority: and when the live policy file differs from what the person
    /// approved, offer the import screen once for that content.
    pub fn control_set_authority(&mut self, authority: gate::Authority, cx: &mut Context<Self>) {
        for problem in &authority.problems {
            log::warn!("gate: {problem}");
        }
        let offer = match (&authority.live, authority.live_differs) {
            (Some(live), true) => {
                let hash = live.hash();
                (self.control.policy_shown.as_ref() != Some(&hash)).then_some(hash)
            }
            _ => None,
        };
        self.control.authority = authority;
        if let Some(hash) = offer
            && self.modal.is_none()
        {
            self.control.policy_shown = Some(hash);
            let approved = self.control.authority.approved.clone().unwrap_or_default();
            let live = self.control.authority.live.clone().unwrap_or_default();
            let lines = gate::diff(&approved, &live);
            self.modal = Some(crate::dialogs::Modal::PolicyChanged { lines, live });
        }
        cx.notify();
    }

    /// Read the authority again every half minute, off the main thread: a hand edit counts as
    /// Careful as soon as it is seen.
    fn control_watch_authority(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                let authority = cx.background_spawn(async { gate::load() }).await;
                if this.update(cx, |this, cx| {
                    this.control_set_authority(authority, cx);
                    this.control_expire_pending(cx);
                })
                .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// The import screen's answer: import the live file as the person saw it, or keep the approved
    /// policy (written back over the live file). Then read the authority again.
    pub fn control_answer_policy(&mut self, import: Option<gate::Policy>, cx: &mut Context<Self>) {
        let approved = self.control.authority.approved.clone();
        let importing = import.is_some();
        let task = cx.background_spawn(async move {
            let done = match (&import, &approved) {
                (Some(seen), _) => gate::import_live(seen),
                (None, Some(approved)) => gate::approve(approved),
                (None, None) => gate::approve(&gate::Policy::default()),
            };
            (done, gate::load())
        });
        cx.spawn(async move |this, cx| {
            let (done, authority) = task.await;
            let _ = this.update(cx, |this, cx| {
                match done {
                    Ok(()) => this.show_toast(if importing { "Imported the policy." } else { "Kept the approved policy." }, cx),
                    Err(err) => this.show_toast(&format!("The policy was not changed: {err}"), cx),
                }
                this.control_set_authority(authority, cx);
            });
        })
        .detach();
    }

    /// Record that a caller other than the person started a session or sent it text: it runs by
    /// the automation rules from now on (M22: its profile, for its whole life).
    pub(crate) fn control_mark_driven(&mut self, caller: &Caller, sessions: &[String]) {
        if !self.control.engine_v2 || caller.channel.is_person() || matches!(caller.channel, caller::Caller::Phone { .. } | caller::Caller::Machine { .. }) {
            return;
        }
        let by = match &caller.channel {
            caller::Caller::Session { id, .. } => id.clone(),
            caller::Caller::Manager { .. } => "manager".to_string(),
            _ => "outside".to_string(),
        };
        let now = glasscode_core::clock::now_ms();
        let origins = self.control.authority.origins.get_or_insert_with(Default::default);
        let mut changed = Vec::new();
        for id in sessions {
            if origins.mark(id, gate::SessionOrigin::Automation, &by, now) {
                changed.push(id.clone());
            }
        }
        if changed.is_empty() {
            return;
        }
        std::thread::spawn(move || {
            for id in changed {
                if let Err(err) = gate::mark_session(&id, gate::SessionOrigin::Automation, &by) {
                    log::warn!("gate: could not record who drives {id}: {err}");
                }
            }
        });
    }

    /// A pending action for an op that asks (v2): numbered past every number given before, and
    /// written to the store off the main thread.
    fn control_new_pending(&mut self, op: &Op, responder: &Responder, plan: &Plan, why: Option<&str>) -> gate::PendingAction {
        let now = glasscode_core::clock::now_ms();
        let mut action = gate::PendingAction::new(op, responder.targets.clone().unwrap_or_default(), Some(responder.caller.channel.clone()), now);
        action.number = self.control.next_approval;
        self.control.next_approval += 1;
        action.who = responder.caller.describe();
        action.session = responder.caller.session.clone();
        action.what = plan.what.clone();
        action.reason = why.unwrap_or_default().to_string();
        action.command = responder.command.clone();
        let kept = action.clone();
        pending_write(move || {
            if let Err(err) = gate::add_pending(kept) {
                log::warn!("pending: could not keep an approval: {err}");
            }
        });
        action
    }

    /// The pending actions after a start: those that still wait come back as cards (with no caller
    /// waiting on them), and their numbers go on from the highest.
    fn control_restore_pending(&mut self, pending: Result<gate::Pending, String>, cx: &mut Context<Self>) {
        let pending = match pending {
            Ok(pending) => pending,
            Err(err) => {
                self.show_toast(format!("The waiting approvals could not be read: {err}"), cx);
                return;
            }
        };
        self.control.pending_high = pending.high_water_ms;
        self.control.next_approval = self.control.next_approval.max(pending.last_number + 1);
        let now = glasscode_core::clock::now_ms();
        for action in pending.waiting(now) {
            if self.control.approvals.iter().any(|a| a.id == action.number) {
                continue;
            }
            let caller = self.control_caller_of(action);
            let (allow_here, always_in) = match action.op() {
                Some(op) => (allow_here(&op, &action.targets), action.targets.projects.first().filter(|key| gate::can_always_allow(&caller.channel, &op, &self.control_policy_set(), key)).cloned()),
                None => (false, None),
            };
            self.control.approvals.push(Approval {
                id: action.number,
                caller,
                kind: action.op().map(|op| op.spec.kind_v1).unwrap_or(Kind::Settings),
                what: action.what.clone(),
                why: (!action.reason.is_empty()).then(|| action.reason.clone()),
                command: action.command.clone(),
                created: action.created_ms / 1000,
                responder: None,
                run: None,
                pending: Some(action.clone()),
                allow_here,
                always_in,
            });
        }
        cx.notify();
    }

    /// Who asked for a stored action, as the app shows callers.
    fn control_caller_of(&self, action: &gate::PendingAction) -> Caller {
        let channel = action.caller.clone().unwrap_or(caller::Caller::Terminal { pid: None });
        let trust = match &channel {
            caller::Caller::Ui | caller::Caller::ShellTab { .. } => Trust::You,
            caller::Caller::Session { .. } | caller::Caller::Manager { .. } => Trust::Session,
            _ => Trust::Outside,
        };
        let session_title = action.session.as_deref().and_then(|id| self.state.session(id)).map(|s| self.session_title(s));
        let agent = action.session.as_deref().and_then(|id| self.state.session(id)).map(|s| s.agent);
        let who = action.who.split(" in “").next().unwrap_or(&action.who).to_string();
        Caller { who, trust, channel, session: action.session.clone(), session_title, agent, cwd: None, project: None }
    }

    /// The cards whose action waited past its expiry: gone, each told so, and marked in the store.
    fn control_expire_pending(&mut self, cx: &mut Context<Self>) {
        let now = glasscode_core::clock::now_ms();
        let high = self.control.pending_high;
        let expired: Vec<u64> = self.control.approvals.iter().filter(|a| a.pending.as_ref().is_some_and(|p| p.expired(now, high))).map(|a| a.id).collect();
        for id in expired {
            let Some(index) = self.control.approvals.iter().position(|a| a.id == id) else { continue };
            let mut approval = self.control.approvals.remove(index);
            self.dismiss_popups_for(&format!("approval:{id}"), cx);
            if let Some(pending) = &approval.pending {
                let pid = pending.id.clone();
                pending_write(move || {
                    let now = glasscode_core::clock::now_ms();
                    let _ = glasscode_core::store::update::<gate::Pending, Vec<String>>(|p| p.expire(now));
                    log::info!("pending {pid}: expired");
                });
            }
            if let Some(mut responder) = approval.responder.take() {
                responder.decided = ops::Decided::Denied;
                self.reply(responder, Reply::fail(code::DENIED, "It waited too long for the person; nothing was done."));
            }
        }
        cx.notify();
    }

    /// The person answered a pending action (v2). Deny: it is marked denied. Allow: its stored op is
    /// checked again (still waiting, not expired, the same targets, allowed by the gate now) and
    /// planned again by its handler, then run once; else the person reads why nothing was done.
    fn control_decide_pending(&mut self, mut approval: Approval, allow: bool, for_session: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = approval.pending.take() else { return };
        let caller = approval.caller.clone();
        let op = pending.op();
        let mut responder = approval.responder.take().unwrap_or_else(|| Responder {
            sinks: Vec::new(),
            caller: caller.clone(),
            command: pending.command.clone(),
            what: pending.what.clone(),
            kind: approval.kind,
            approval: Some(approval.id),
            op: op.clone(),
            targets: Some(pending.targets.clone()),
            decided: ops::Decided::Allowed,
            on_done: None,
            pending: None,
        });
        let pid = pending.id.clone();
        let settle = |state: gate::PendingState, outcome: Option<String>| settle_later(pid.clone(), state, outcome);
        if !allow {
            settle(gate::PendingState::Denied, Some("You said no.".into()));
            responder.decided = ops::Decided::Denied;
            self.reply(responder, Reply::fail(code::DENIED, "The person said no in Glasscode. Do not try another way around it."));
            return;
        }
        let now = glasscode_core::clock::now_ms();
        let checked = op.as_ref().map(|op| {
            let targets = ops::targets(op, &self.world(&caller));
            let decision = match &targets {
                Ok(targets) => self.control_gate(&caller, op, targets),
                Err(_) => Decision::Allow,
            };
            (targets, decision)
        });
        let rechecked = match &checked {
            Some((targets, decision)) => pending.recheck(targets.as_ref().map_err(|e| e.text.as_str()), decision, now, self.control.pending_high),
            None => pending.recheck(Err("This Glasscode does not know that action."), &Decision::Allow, now, self.control.pending_high),
        };
        let fail = |ws: &mut Workspace, responder: Responder, said: String, cx: &mut Context<Workspace>| fail_pending(ws, responder, pid.clone(), said, cx);
        let op = match rechecked {
            Ok(op) => op,
            Err(said) => return fail(self, responder, said, cx),
        };
        let plan = match self.op_plan(&caller, &op) {
            Ok(plan) => plan,
            Err(Reply::Done { text, .. }) => return fail(self, responder, format!("{}. Nothing was done.", text.lines().next().unwrap_or_default().trim_end_matches('.')), cx),
            Err(_) => return fail(self, responder, "It could not be planned again; nothing was done.".into(), cx),
        };
        // The plan's own check (git, gh, a file) runs off the main thread first, as for any op.
        let allow_here = approval.allow_here;
        let id = pending.id.clone();
        self.after_check(plan, window, cx, move |ws, plan, window, cx| {
            let plan = match plan {
                Ok(plan) => plan,
                Err(Reply::Done { text, .. }) => return fail_pending(ws, responder, id, format!("{}. Nothing was done.", text.lines().next().unwrap_or_default().trim_end_matches('.')), cx),
                Err(_) => return fail_pending(ws, responder, id, "It did not pass its check; nothing was done.".into(), cx),
            };
            settle_later(id.clone(), gate::PendingState::Approved, None);
            if for_session
                && allow_here
                && let Some(session) = &caller.session
            {
                let mono = ws.control.started.elapsed().as_millis() as u64;
                ws.control.short.allow(session, &op, &pending.targets, 12 * 60 * 60 * 1000, now, mono);
            }
            responder.decided = ops::Decided::Approved;
            responder.pending = Some(id);
            responder.what = plan.what.clone();
            (plan.run)(ws, responder, window, cx);
        });
    }

    /// "Always allow this in <project>": the policy preview, which the person confirms a second
    /// time before the change is approved.
    pub fn control_offer_always(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(approval) = self.control.approvals.iter().find(|a| a.id == id) else { return };
        let (Some(key), Some(pending)) = (approval.always_in.clone(), approval.pending.as_ref()) else { return };
        let Some(approved) = self.control.authority.approved.clone().or_else(|| (!self.control.authority.live_differs).then(|| self.control.authority.policy.clone())) else {
            self.show_toast("The approved policy could not be read, so nothing can be allowed always.", cx);
            return;
        };
        let op = pending.op_name().to_string();
        let changed = gate::with_always_allowed(&approved, &key, &op);
        let lines = gate::diff(&approved, &changed);
        self.modal = Some(crate::dialogs::Modal::AlwaysAllow { approval: id, project: key, op, lines, policy: changed });
        cx.notify();
    }

    /// The second confirmation: the changed policy is approved, and the card that asked is approved.
    pub fn control_always_confirmed(&mut self, id: u64, policy: gate::Policy, window: &mut Window, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move { (gate::approve(&policy), gate::load()) });
        cx.spawn(async move |this, cx| {
            let (done, authority) = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(err) = done {
                    this.show_toast(format!("The policy was not changed: {err}"), cx);
                }
                this.control_set_authority(authority, cx);
            });
        })
        .detach();
        self.control_decide(id, true, false, window, cx);
    }

    fn control_handle(&mut self, request: Request, source: Source, sink: Sink, window: &mut Window, cx: &mut Context<Self>) {
        let caller = self.control_caller(&request, source);
        // A brain's calls wait until the app has checked the tools of its first turn (brain-m5).
        if let Some(refusal) = glasscode_core::mcp::unchecked_brain(&caller.channel, request.brain.is_some(), &self.control.brains_checked) {
            sink.send(&refusal);
            return;
        }
        // A typed op (`glasscode mcp`): the v2 engine's, through its gate.
        if let Some(typed) = glasscode_core::mcp::typed_request(&request) {
            if !self.control.engine_v2 {
                sink.send(&Reply::fail(code::USAGE, "Glasscode's tools (glasscode mcp) need Manager v2's engine; use the glasscode command."));
                return;
            }
            match typed {
                Ok(op) => {
                    let command = format!("glasscode mcp: {}", op.name());
                    self.control_gated_as(op, caller, vec![sink], command, None, request.detach, window, cx);
                }
                Err(error) => sink.send(&crate::ops_run::with_op(crate::ops_run::error_reply(&error), request.op.as_ref().and_then(|o| o["op"].as_str()).and_then(ops::find).map(|o| o.name), true)),
            }
            return;
        }
        self.control_handle_as(request, sink, caller, window, cx);
    }

    /// The app has checked the tools of a brain's first turn (`mcp::init_tools_ok`): its MCP
    /// server's calls are served from now on. The brain's launch calls it (M50).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn control_brain_checked(&mut self, lane: glasscode_core::caller::Lane, wake: Option<String>) {
        self.control.brains_checked.insert((lane, wake));
    }

    /// A `glasscode` command from a session on the server (the server checked who it is).
    pub fn control_remote_call(&mut self, call: glasscode_core::remote::RemoteCall, window: &mut Window, cx: &mut Context<Self>) {
        // The server names the op its session's words are (`ops::name_of`); the Mac reads them
        // again, and the two agree unless the builds know other ops.
        let here = ops::name_of(&call.request.args, true);
        if call.op.is_some() && call.op.as_deref() != here {
            log::warn!("a server session's command is {:?} there and {here:?} here", call.op);
        }
        let server = self.server.name();
        let channel = caller::by_channel(&call.request, &Channel::Server { session: call.session.clone() }, |_, _| None, |_| false, |_| false);
        let caller = match &call.session {
            Some(id) => Caller {
                who: call.agent.map(|a| a.name().to_string()).unwrap_or_else(|| "A program".into()),
                trust: Trust::Session,
                channel,
                session: Some(format!("server:{id}")),
                session_title: Some(format!("{} (on {server})", call.session_title.clone().unwrap_or_else(|| "a session".into()))),
                agent: call.agent,
                cwd: None,
                project: call.project.clone(),
            },
            None => Caller { who: format!("A program on {server}"), trust: Trust::Outside, channel, session: None, session_title: None, agent: None, cwd: None, project: None },
        };
        let sink = Sink::Remote(call.id, self.control.remote_out.clone());
        self.control_handle_as(call.request, sink, caller, window, cx);
    }

    /// Run something that needs the window from where there is none (a background task).
    pub fn with_main_window(&self, cx: &mut Context<Self>, run: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static) {
        let Some(main) = self.main_window else { return };
        let me = cx.entity();
        cx.defer(move |cx| {
            let _ = main.update(cx, |_, window, cx| me.update(cx, |ws, cx| run(ws, window, cx)));
        });
    }

    fn control_handle_as(&mut self, request: Request, sink: Sink, caller: Caller, window: &mut Window, cx: &mut Context<Self>) {
        if self.control.engine_v2 {
            self.control_handle_v2(request, sink, caller, window, cx);
            return;
        }
        let command = shell_line(&request.args);
        // The registry reads the words first; the op's handler makes the plan.
        let (op, plan) = match self.control_plan(&caller, &request.args) {
            Ok(planned) => planned,
            Err(reply) => {
                sink.send(&reply);
                return;
            }
        };
        self.after_check(plan, window, cx, move |ws, plan, window, cx| {
            let plan = match plan {
                Ok(plan) => plan,
                Err(reply) => return sink.send(&crate::ops_run::with_op(reply, Some(op.name()), false)),
            };
            let targets = ops::targets(&op, &ws.world(&caller)).ok();
            let responder = Responder { sinks: vec![sink], caller: caller.clone(), command, what: plan.what.clone(), kind: plan.kind, approval: None, op: Some(op), targets, decided: ops::Decided::Allowed, on_done: None, pending: None };
            ws.control_run(plan, responder, window, cx);
        });
    }

    /// A dry run's answer (ops-22): what the op would do, what it touches, whether it would ask the
    /// person, and what would take it back; as a read, so nothing is logged as done.
    fn control_preview(&mut self, plan: Plan, mut responder: Responder, asks: bool) {
        let op = responder.op.take();
        let name = op.as_ref().map(Op::name);
        let undo = op.as_ref().and_then(|op| match op.spec.undo {
            ops::Undo::Inverse(inverse) => Some(inverse),
            ops::Undo::None => None,
        });
        let targets = responder.targets.clone().unwrap_or_default();
        responder.kind = Kind::Read;
        let text = format!("Would {}{}. Nothing was done.", plan.what, if asks { " (after the person's OK)" } else { "" });
        let data = serde_json::json!({ "op": name, "dry_run": true, "would": plan.what, "asks": asks, "targets": targets, "undo": undo });
        self.reply(responder, Reply::ok(text, data));
    }

    /// `then` with the plan once its check (`Plan::check`) passed off the main thread, or with
    /// the check's refusal; at once when it has none.
    pub(crate) fn after_check(&mut self, mut plan: Plan, window: &mut Window, cx: &mut Context<Self>, then: impl FnOnce(&mut Workspace, Result<Plan, Reply>, &mut Window, &mut Context<Workspace>) + 'static) {
        let Some(check) = plan.check.take() else { return then(self, Ok(plan), window, cx) };
        let task = cx.background_spawn(async move { check() });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |ws, window, cx| then(ws, result.map(|()| plan), window, cx));
        })
        .detach();
    }

    /// Run a plan by the rules of Settings › Permissions for its caller: at once, after the
    /// person's OK, or not at all.
    pub(crate) fn control_run(&mut self, plan: Plan, mut responder: Responder, window: &mut Window, cx: &mut Context<Self>) {
        let caller = responder.caller.clone();
        match self.control_rule(&caller, plan.kind) {
            Rule::Allow => (plan.run)(self, responder, window, cx),
            Rule::Never => {
                responder.decided = ops::Decided::Never;
                let text = format!(
                    "Not allowed: Settings › Permissions says agents may never “{}”. Ask the person to do it themselves, or to change that setting.",
                    plan.kind.label().to_lowercase()
                );
                self.reply(responder, Reply::fail(code::DENIED, text));
            }
            Rule::Ask => self.control_ask(plan, responder, None, cx),
        }
    }

    /// The v2 engine's way for the `glasscode` command: the registry reads the words, then
    /// [`Workspace::control_gated`].
    fn control_handle_v2(&mut self, request: Request, sink: Sink, caller: Caller, window: &mut Window, cx: &mut Context<Self>) {
        let command = shell_line(&request.args);
        match self.control_parse(&caller, &request.args) {
            Ok(op) => self.control_gated(op, caller, vec![sink], command, None, window, cx),
            Err(reply) => sink.send(&reply),
        }
    }

    /// An op a caller named, with the v2 engine: Rust resolves what it touches, the gate decides,
    /// and only then does its handler plan it (`op_plan`); a refused op never reaches its handler.
    /// The answer goes to `sinks` (a command), or to `done` (an op the app runs, `run_op`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn control_gated(&mut self, op: Op, caller: Caller, sinks: Vec<Sink>, command: String, done: Option<Box<dyn FnOnce(&mut Workspace, &Reply)>>, window: &mut Window, cx: &mut Context<Self>) {
        self.control_gated_as(op, caller, sinks, command, done, false, window, cx);
    }

    /// [`Workspace::control_gated`], for a caller that may not wait for the op (`detach`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn control_gated_as(&mut self, op: Op, caller: Caller, sinks: Vec<Sink>, command: String, done: Option<Box<dyn FnOnce(&mut Workspace, &Reply)>>, detach: bool, window: &mut Window, cx: &mut Context<Self>) {
        let answer = |ws: &mut Workspace, sinks: &[Sink], done: Option<Box<dyn FnOnce(&mut Workspace, &Reply)>>, reply: Reply| {
            for sink in sinks {
                sink.send(&reply);
            }
            if let Some(done) = done {
                done(ws, &reply);
            }
        };
        // What it touches must be known for the gate to decide: a word that names nothing, or
        // could name several things, is the caller's to fix.
        let targets = match ops::targets(&op, &self.world(&caller)) {
            Ok(targets) => targets,
            Err(error) => return answer(self, &sinks, done, crate::ops_run::with_op(crate::ops_run::error_reply(&error), Some(op.name()), true)),
        };
        let mut decision = self.control_gate(&caller, &op, &targets);
        // "Allow for the rest of the session": this op on these targets from this session.
        if let (Decision::Ask { .. }, Some(session)) = (&decision, &caller.session)
            && allow_here(&op, &targets)
            && self.control.short.covers(session, &op, &targets, glasscode_core::clock::now_ms(), self.control.started.elapsed().as_millis() as u64)
        {
            decision = Decision::Allow;
        }
        // A session that runs under the person's profile, typed into by the manager or a session
        // it drives: it takes the automation profile only at its next start, so this asks (M22).
        if self.control.profiles
            && decision == Decision::Allow
            && op.spec.group == ops::OpGroup::SessionInput
            && !caller.channel.is_person()
            && targets.sessions.iter().any(|id| self.control.profile_running.get(id) == Some(&gate::SessionOrigin::Person))
        {
            decision = Decision::Ask { why: "that session runs under your own profile until it starts again; typing into it from here asks".into() };
        }
        let mut responder = Responder {
            sinks,
            caller: caller.clone(),
            command,
            what: op.spec.summary.to_string(),
            kind: op.spec.kind_v1,
            approval: None,
            op: Some(op.clone()),
            targets: Some(targets),
            decided: ops::Decided::Allowed,
            on_done: done,
            pending: None,
        };
        if let Decision::Never { why } = &decision {
            responder.decided = ops::Decided::Never;
            let text = format!("Not allowed: {why}. Ask the person to do it themselves, or to change the project's policy in Glasscode.");
            self.reply(responder, Reply::fail(code::DENIED, text));
            return;
        }
        let plan = match self.op_plan(&caller, &op) {
            Ok(plan) => plan,
            Err(reply) => return answer(self, &responder.sinks, responder.on_done.take(), reply),
        };
        let name = op.name();
        self.after_check(plan, window, cx, move |ws, plan, window, cx| {
            let plan = match plan {
                Ok(plan) => plan,
                Err(reply) => return answer(ws, &responder.sinks, responder.on_done.take(), crate::ops_run::with_op(reply, Some(name), true)),
            };
            responder.what = plan.what.clone();
            responder.kind = plan.kind;
            // A dry run says what it would do, and runs nothing: no card, no claim, no record of
            // an effect (ops-22). The gate's "never" has answered above.
            if responder.op.as_ref().is_some_and(Op::dry_run) {
                return ws.control_preview(plan, responder, matches!(decision, Decision::Ask { .. }));
            }
            match decision {
                // A caller that does not wait (`glasscode mcp` for a long op) has an id at once,
                // and the result under it (`app.wait <id>`).
                Decision::Allow if detach => {
                    let id = ws.control.next_approval;
                    ws.control.next_approval += 1;
                    ws.control.running.insert(id, plan.what.clone());
                    let started = Reply::ok(format!("Started (#{id}): {}. `wait {id}` gets the result.", plan.what), serde_json::json!({ "status": "started", "id": id }));
                    for sink in std::mem::take(&mut responder.sinks) {
                        sink.send(&started);
                    }
                    responder.approval = Some(id);
                    (plan.run)(ws, responder, window, cx)
                }
                Decision::Allow => (plan.run)(ws, responder, window, cx),
                Decision::Ask { why } => ws.control_ask(plan, responder, Some(why), cx),
                Decision::Never { .. } => unreachable!("answered above"),
            }
        });
    }

    fn control_ask(&mut self, plan: Plan, mut responder: Responder, why: Option<String>, cx: &mut Context<Self>) {
        // With the v2 engine an approval is data, kept across a restart (M21.2).
        let pending = match (&responder.op, self.control.engine_v2) {
            (Some(op), true) => Some(self.control_new_pending(op, &responder, &plan, why.as_deref())),
            _ => None,
        };
        let id = match &pending {
            Some(pending) => pending.number,
            None => {
                let id = self.control.next_approval;
                self.control.next_approval += 1;
                id
            }
        };
        responder.approval = Some(id);
        let targets = responder.targets.clone().unwrap_or_default();
        let (allow_here, always_in) = match (&responder.op, &pending) {
            (Some(op), Some(_)) => (allow_here(op, &targets), targets.projects.first().filter(|key| gate::can_always_allow(&responder.caller.channel, op, &self.control_policy_set(), key)).cloned()),
            _ => (true, None),
        };
        let approval = Approval {
            id,
            caller: responder.caller.clone(),
            kind: plan.kind,
            what: plan.what,
            why,
            command: responder.command.clone(),
            created: glasscode_core::model::now(),
            responder: None,
            // A pending action is planned again from its stored op when it is approved.
            run: if pending.is_some() { None } else { Some(plan.run) },
            pending,
            allow_here,
            always_in,
        };
        let summary = approval.summary();
        for sink in &responder.sinks {
            sink.send(&Reply::Waiting { approval: id, summary: summary.clone() });
        }
        let agent = approval.caller.agent.unwrap_or(AgentKind::Claude);
        let title = format!("{} asks you", approval.caller.who);
        let body = match &approval.why {
            Some(why) => format!("{}? ({})", capitalize(&approval.what), why),
            None => format!("{}?", capitalize(&approval.what)),
        };
        let mut approval = approval;
        approval.responder = Some(responder);
        let review = !approval.allow_here;
        self.journal_approval(id, &approval.caller, &approval.what, approval.kind, None);
        self.control.approvals.push(approval);
        self.notify_approval(id, agent, title, body, review, cx);
        cx.notify();
        // Debug builds' UI tests: answer by itself after a moment (allow, deny, session).
        if let Some(answer) = std::env::var("GLASSCODE_DEBUG_DECIDE").ok().filter(|a| cfg!(debug_assertions) && !a.is_empty()) {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                let _ = this.update(cx, |ws, cx| ws.control_debug_decide(id, &answer, cx));
            })
            .detach();
        }
    }

    /// The debug answer needs a window: the main one.
    fn control_debug_decide(&mut self, id: u64, answer: &str, cx: &mut Context<Self>) {
        let (allow, session) = (answer != "deny", answer == "session");
        self.with_main_window(cx, move |ws, window, cx| ws.control_decide(id, allow, session, window, cx));
    }

    /// Allow or deny a waiting request (`for_session`: and the same kind from that session
    /// from now on, until Glasscode quits).
    pub fn control_decide(&mut self, id: u64, allow: bool, for_session: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.control.approvals.iter().position(|a| a.id == id) else { return };
        let mut approval = self.control.approvals.remove(index);
        self.dismiss_popups_for(&format!("approval:{id}"), cx);
        if approval.pending.is_some() {
            // Every decision goes to the journal (M21.2 step 7), the pending ones too.
            self.journal_approval(id, &approval.caller, &approval.what, approval.kind, Some(allow));
            self.control_decide_pending(approval, allow, for_session, window, cx);
            cx.notify();
            return;
        }
        let Some(mut responder) = approval.responder.take() else { return };
        responder.decided = if allow { ops::Decided::Approved } else { ops::Decided::Denied };
        self.journal_approval(id, &approval.caller, &approval.what, approval.kind, Some(allow));
        if allow {
            if for_session {
                if let Some(session) = &approval.caller.session {
                    self.control.allowed.entry(session.clone()).or_default().insert(approval.kind);
                }
            }
            if let Some(run) = approval.run.take() {
                run(self, responder, window, cx);
            }
        } else {
            self.reply(responder, Reply::fail(code::DENIED, "The person said no in Glasscode. Do not try another way around it."));
        }
        cx.notify();
    }

    /// Send the answer, keep it for `wait`, and log what was done.
    pub fn reply(&mut self, mut responder: Responder, reply: Reply) {
        // A button's own command (no op): its answer, as it came.
        if responder.op.is_none()
            && let Some(done) = responder.on_done.take()
        {
            done(self, &reply);
            return;
        }
        let mut reply = crate::ops_run::with_op(reply, responder.op.as_ref().map(Op::name), self.control.engine_v2);
        // What takes it back (ops-18), with the v2 engine: the inverse op and its arguments.
        if let (true, Some(op), Reply::Done { code: code::OK, data, .. }) = (self.control.engine_v2, &responder.op, &mut reply)
            && let Some(undo) = ops::undo_of(op, data)
            && data.is_object()
        {
            data["undo"] = undo;
        }
        // An approved pending action has run: what came of it, on its record.
        if let (Some(id), Reply::Done { code: status, text, .. }) = (responder.pending.take(), &reply) {
            let outcome = format!("{}{}", if *status == code::OK { "" } else { "It failed: " }, text.lines().next().unwrap_or_default());
            pending_write(move || {
                if let Err(err) = gate::settle_pending(&id, gate::PendingState::Ran, Some(outcome)) {
                    log::warn!("pending {id}: {err}");
                }
            });
        }
        // A session a caller other than the person started, or sent text to, is driven by it.
        if let (Some(op), Reply::Done { code: code::OK, data, .. }) = (&responder.op, &reply) {
            let mut driven: Vec<String> = Vec::new();
            match op.spec.group {
                ops::OpGroup::SessionStart => driven.extend(data.get("id").and_then(serde_json::Value::as_str).map(str::to_string)),
                ops::OpGroup::SessionInput => driven.extend(responder.targets.as_ref().map(|t| t.sessions.clone()).unwrap_or_default()),
                _ => {}
            }
            if !driven.is_empty() {
                let caller = responder.caller.clone();
                self.control_mark_driven(&caller, &driven);
            }
        }
        // The journal's record of it (M14 writes it), made from the op, not from words: a
        // command's, and an op the app ran for the person (`run_op`).
        if let (Some(op), Reply::Done { code: status, .. }) = (&responder.op, &reply) {
            let who = match (responder.caller.trust, responder.caller.is_manager()) {
                (Trust::You, _) => "you",
                (_, true) => "manager",
                (Trust::Session, false) => "session",
                (Trust::Outside, _) => "outside",
            };
            let action = ops::Action::of(op, responder.targets.take().unwrap_or_default(), who, responder.caller.session.clone(), responder.decided, *status, glasscode_core::clock::now_ms());
            self.control.actions.insert(0, action);
            self.control.actions.truncate(200);
        }
        // The journal's op event (M14): with the log line, below; for a read, and for an op the
        // app ran for the person (`run_op`), without one.
        let journaled = self.control.actions.first().filter(|a| responder.op.as_ref().is_some_and(|op| op.name() == a.op)).cloned();
        if let Some(done) = responder.on_done.take() {
            if let Some(action) = &journaled {
                self.journal_op(action, None);
            }
            done(self, &reply);
            return;
        }
        for sink in &responder.sinks {
            sink.send(&reply);
        }
        if let Some(id) = responder.approval {
            self.control.results.insert(id, reply.clone());
            self.control.running.remove(&id);
        }
        let Reply::Done { code: status, text, .. } = &reply else { return };
        if responder.kind == Kind::Read && responder.approval.is_none() {
            if let Some(action) = &journaled {
                self.journal_op(action, None);
            }
            return;
        }
        let first = text.lines().next().unwrap_or_default().trim();
        let (what, outcome) = match *status {
            code::OK => (past_tense(&responder.what), "done"),
            code::DENIED => (format!("Was not allowed to {}", responder.what), "denied"),
            _ => (format!("Could not {}: {}", responder.what, strip_prefixes(first, &responder.what)), "failed"),
        };
        let entry = LogEntry {
            at: glasscode_core::model::now(),
            who: responder.caller.who.clone(),
            session: responder.caller.session.clone(),
            session_title: responder.caller.session_title.clone(),
            what,
            command: responder.command.clone(),
            outcome: outcome.into(),
        };
        control::append_log(&entry);
        if let Some(action) = &journaled {
            self.journal_op(action, Some(&entry));
        }
        self.control.log.insert(0, entry);
        self.control.log.truncate(200);
    }

    /// `glasscode wait <id>`: the answer, or wait along.
    pub fn control_wait(&mut self, id: u64, mut responder: Responder) {
        if let Some(reply) = self.control.results.get(&id).cloned() {
            for sink in &responder.sinks {
                sink.send(&reply);
            }
            return;
        }
        // Started without waiting, and not done yet: ask again later.
        if let Some(what) = self.control.running.get(&id) {
            let still = Reply::Waiting { approval: id, summary: format!("still running: {what}") };
            for sink in &responder.sinks {
                sink.send(&still);
            }
            return;
        }
        let Some(approval) = self.control.approvals.iter_mut().find(|a| a.id == id) else {
            for sink in &responder.sinks {
                sink.send(&Reply::fail(code::FAILED, format!("Nothing is waiting as #{id} (Glasscode forgets answers when it quits).")));
            }
            return;
        };
        let summary = approval.summary();
        for sink in &responder.sinks {
            sink.send(&Reply::Waiting { approval: id, summary: summary.clone() });
        }
        if let Some(waiting) = approval.responder.as_mut() {
            waiting.sinks.append(&mut responder.sinks);
        }
    }

    pub fn control_approvals_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.control
                .approvals
                .iter()
                .map(|a| serde_json::json!({ "id": a.id, "who": a.caller.who, "session": a.caller.session, "session_title": a.caller.session_title, "what": a.what, "command": a.command, "kind": a.kind.key(), "created": a.created }))
                .collect(),
        )
    }
}

/// "add “X” to Today" → "Added “X” to Today"; "remove the worktree x" → "Removed the worktree x".
fn past_tense(what: &str) -> String {
    let (first, rest) = match what.split_once(' ') {
        Some((a, b)) => (a, format!(" {b}")),
        None => (what, String::new()),
    };
    let verb = match first {
        "add" => "Added",
        "remove" => "Removed",
        "close" => "Closed",
        "restart" => "Restarted",
        "rename" => "Renamed",
        "move" => "Moved",
        "start" => "Started",
        "send" => "Sent",
        "snooze" => "Snoozed",
        "comment" => "Commented",
        "import" => "Imported",
        "clone" => "Cloned",
        "make" => "Made",
        "open" => "Opened",
        "re-run" => "Re-ran",
        "hand" => "Handed",
        "change" => "Changed",
        "tick" => "Ticked",
        "put" => "Put",
        "approve" => "Approved",
        "answer" => "Answered",
        "pause" => "Paused",
        "resume" => "Resumed",
        "cancel" => "Cancelled",
        "show" => return format!("Opened {}", rest.trim_start().trim_start_matches("the ")),
        "notify" => return "Sent you a notification".to_string(),
        other => return capitalize(&format!("{other}{rest}")),
    };
    format!("{verb}{rest}")
}

/// Drop a leading "Could not …:" the command already put on its error, so the log is not doubled.
fn strip_prefixes<'a>(text: &'a str, _what: &str) -> &'a str {
    text.split_once(": ").map(|(head, tail)| if head.to_lowercase().starts_with("could not") { tail } else { text }).unwrap_or(text)
}

pub fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
