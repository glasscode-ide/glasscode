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
}

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
    /// For the app's own buttons: what to do with the answer (nothing is logged).
    on_done: Option<Box<dyn FnOnce(&mut Workspace, &Reply)>>,
}

impl Responder {
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
        }
    }
}

pub struct Approval {
    pub id: u64,
    pub caller: Caller,
    pub kind: Kind,
    pub what: String,
    pub command: String,
    pub created: u64,
    responder: Option<Responder>,
    run: Option<Run>,
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
    /// The first prompt of a session that is about to start, when the person typed it in a
    /// composer (the Today box): its origin and what of it was not typed. The launch records it.
    pub first_words: HashMap<String, (Origin, Vec<std::ops::Range<usize>>)>,
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
            first_words: HashMap::new(),
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
        let engine = cx.background_spawn(async { glasscode_core::store::EngineSwitch::current().engine == glasscode_core::store::Engine::V2 });
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
            let v2 = engine.await;
            if this.update(cx, |this, _| this.control.engine_v2 = v2).is_err() {
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
        let found = caller::by_channel(request, &channel, signed, under_brain, |session| v2 && caller::typed_command_recently(session));
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

    fn control_handle(&mut self, request: Request, source: Source, sink: Sink, window: &mut Window, cx: &mut Context<Self>) {
        let caller = self.control_caller(&request, source);
        self.control_handle_as(request, sink, caller, window, cx);
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
        let command = shell_line(&request.args);
        // The registry reads the words first; the op's handler makes the plan.
        let (op, plan) = match self.control_plan(&caller, &request.args) {
            Ok(planned) => planned,
            Err(reply) => {
                sink.send(&reply);
                return;
            }
        };
        let targets = ops::targets(&op, &self.world(&caller)).ok();
        let mut responder = Responder { sinks: vec![sink], caller: caller.clone(), command, what: plan.what.clone(), kind: plan.kind, approval: None, op: Some(op), targets, decided: ops::Decided::Allowed, on_done: None };
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
            Rule::Ask => self.control_ask(plan, responder, cx),
        }
    }

    fn control_ask(&mut self, plan: Plan, mut responder: Responder, cx: &mut Context<Self>) {
        let id = self.control.next_approval;
        self.control.next_approval += 1;
        responder.approval = Some(id);
        let approval = Approval {
            id,
            caller: responder.caller.clone(),
            kind: plan.kind,
            what: plan.what,
            command: responder.command.clone(),
            created: glasscode_core::model::now(),
            responder: None,
            run: Some(plan.run),
        };
        let summary = approval.summary();
        for sink in &responder.sinks {
            sink.send(&Reply::Waiting { approval: id, summary: summary.clone() });
        }
        let agent = approval.caller.agent.unwrap_or(AgentKind::Claude);
        let title = format!("{} asks you", approval.caller.who);
        let body = format!("{}?", capitalize(&approval.what));
        let mut approval = approval;
        approval.responder = Some(responder);
        self.control.approvals.push(approval);
        self.notify_approval(id, agent, title, body, cx);
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
        let Some(mut responder) = approval.responder.take() else { return };
        responder.decided = if allow { ops::Decided::Approved } else { ops::Decided::Denied };
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
        if let Some(done) = responder.on_done.take() {
            done(self, &reply);
            return;
        }
        let reply = crate::ops_run::with_op(reply, responder.op.as_ref().map(Op::name), self.control.engine_v2);
        // The journal's record of it (M14 writes it), made from the op, not from words.
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
        for sink in &responder.sinks {
            sink.send(&reply);
        }
        if let Some(id) = responder.approval {
            self.control.results.insert(id, reply.clone());
        }
        let Reply::Done { code: status, text, .. } = &reply else { return };
        if responder.kind == Kind::Read && responder.approval.is_none() {
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
