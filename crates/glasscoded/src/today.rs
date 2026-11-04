//! Today and the `glasscode` command on the server. The Mac keeps the board and decides what
//! agents may do; the server relays: it keeps each Mac's last Today (sent with its
//! `mirror.sync`) for phones (`today.get`), queues what phones do with it (`today.op`), and
//! passes `glasscode` commands from sessions running here to the Mac, which answers them in its
//! next sync. Without a Mac connected the command says so.
//!
//! Queued actions and commands stay until the Mac says it has them (the next sync names them),
//! so a sync that fails on the way back loses nothing; the Mac does each one once.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use glasscode_core::control::{self, Reply, Request, code};
use glasscode_core::model::now;
use glasscode_core::remote::{RemoteCall, RemoteCallReply};
use serde_json::{Value, json};

use crate::daemon::{Shared, lock, log};

/// A Mac counts as connected this long after its last sync with Today in it.
const MAC_FRESH: u64 = 45;
/// The longest a command waits for the Mac's answer.
const ANSWER_WITHIN: Duration = Duration::from_secs(600);

/// One Mac synced to this server (there can be more than one).
#[derive(Default)]
struct Mac {
    /// Its last Today, as JSON (an item a newer app knows passes through unchanged).
    snapshot: Option<Value>,
    /// When it last sent a Today snapshot (older apps never do, so cannot answer commands).
    seen: u64,
    /// Phone actions (with a `seq`) and commands it has not confirmed having yet.
    ops: Vec<(u64, Value)>,
    calls: Vec<(u64, Value)>,
    next_op: u64,
    next_call: u64,
}

impl Mac {
    fn fresh(&self) -> bool {
        self.snapshot.is_some() && now().saturating_sub(self.seen) <= MAC_FRESH
    }
}

#[derive(Default)]
struct Relay {
    macs: HashMap<String, Mac>,
    /// Commands waiting for an answer: call id → (where the answer goes, which Mac).
    waiting: HashMap<u64, (mpsc::Sender<Reply>, String)>,
    skill_off: Option<bool>,
}

fn relay() -> std::sync::MutexGuard<'static, Relay> {
    static RELAY: OnceLock<Mutex<Relay>> = OnceLock::new();
    RELAY.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    }
    bytes
}

/// An id that does not repeat across daemon restarts (53 bits: exact in any JSON reader), so an
/// answer from before a restart never lands on a new command with the same number.
fn fresh_id(bump: &mut u64) -> u64 {
    *bump += 1;
    (u64::from_le_bytes(random_bytes::<8>()) & (((1u64 << 53) - 1) ^ 0xffff)) | (*bump & 0xffff)
}

/// The key sessions here are signed with. Kept in a file (0600): sessions keep running in tmux
/// when the daemon restarts or updates itself, with the signature they were started with.
fn secret() -> &'static [u8; 32] {
    static SECRET: OnceLock<[u8; 32]> = OnceLock::new();
    SECRET.get_or_init(|| {
        let path = glasscode_core::paths::root().join("control.key");
        if let Ok(saved) = std::fs::read(&path) {
            if let Ok(bytes) = <[u8; 32]>::try_from(saved.as_slice()) {
                return bytes;
            }
        }
        let bytes = random_bytes::<32>();
        let tmp = path.with_extension("key.tmp");
        let written = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).and_then(|mut f| f.write_all(&bytes)).and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            log(format!("could not keep the session key ({e}); sessions started now are recognised until glasscoded restarts"));
        }
        bytes
    })
}

/// The signature a session here gets (`GLASSCODE_SESSION_SIG`).
pub fn signature(session_id: &str) -> String {
    control::sign(secret(), session_id)
}

/// The Mac that synced last and still counts as connected (one server may hold more than one).
fn latest_mac(relay: &Relay) -> Option<String> {
    relay.macs.iter().filter(|(_, m)| m.fresh()).max_by_key(|(_, m)| m.seen).map(|(id, _)| id.clone())
}

/// What a Mac sent with its `mirror.sync`, and what it gets back.
pub struct Exchange {
    pub mac_id: String,
    pub today: Option<Value>,
    pub replies: Vec<RemoteCallReply>,
    pub today_acks: Vec<u64>,
    pub cli_acks: Vec<u64>,
    pub skill_off: bool,
}

/// `mirror.sync`: keep what the Mac sent, acknowledge what it did, hand it what still waits.
/// Returns (phone actions, commands), each as JSON with its number, for the Mac to do.
pub fn exchange(x: Exchange) -> (Vec<Value>, Vec<Value>) {
    let mut relay = relay();
    if x.today.is_some() {
        relay.skill_off = Some(x.skill_off);
    }
    // Answers for commands, each to whoever is waiting on that Mac's call.
    for r in x.replies {
        let done = matches!(r.reply, Reply::Done { .. });
        if let Some((tx, mac)) = relay.waiting.get(&r.id) {
            if *mac == x.mac_id {
                let _ = tx.send(r.reply);
                if done {
                    relay.waiting.remove(&r.id);
                }
            }
        }
    }
    let mac = relay.macs.entry(x.mac_id.clone()).or_default();
    if let Some(today) = x.today {
        mac.snapshot = Some(today);
        mac.seen = now();
    }
    mac.ops.retain(|(seq, _)| !x.today_acks.contains(seq));
    mac.calls.retain(|(id, _)| !x.cli_acks.contains(id));
    let ops = mac
        .ops
        .iter()
        .map(|(seq, op)| {
            let mut op = op.clone();
            if let Some(map) = op.as_object_mut() {
                map.insert("seq".into(), (*seq).into());
            }
            op
        })
        .collect();
    let calls = mac.calls.iter().map(|(_, c)| c.clone()).collect();
    // Forget Macs long gone.
    let cutoff = now().saturating_sub(600);
    relay.macs.retain(|id, m| *id == x.mac_id || m.seen > cutoff);
    (ops, calls)
}

/// Phones: `today.get` and `today.op` (through the SSH gate, which only a paired phone passes).
pub fn dispatch(method: &str, params: &Value) -> Option<anyhow::Result<Value>> {
    match method {
        "today.get" => {
            let relay = relay();
            let snapshot = latest_mac(&relay).and_then(|id| relay.macs.get(&id)).and_then(|m| m.snapshot.clone());
            let fresh = snapshot.is_some();
            Some(Ok(json!({ "snapshot": snapshot, "fresh": fresh })))
        }
        "today.op" => Some((|| {
            if params.get("op").and_then(Value::as_str).is_none() {
                anyhow::bail!("not a Today action");
            }
            let mut relay = relay();
            let Some(id) = latest_mac(&relay) else {
                anyhow::bail!("your Mac is not connected to this server right now, so Today cannot change; it updates when the Mac is back");
            };
            let mac = relay.macs.get_mut(&id).unwrap();
            mac.next_op += 1;
            let seq = mac.next_op;
            mac.ops.push((seq, params.clone()));
            Ok(json!({ "queued": true }))
        })()),
        _ => None,
    }
}

/// Pass a command to the Mac and stream its answers to `send` (until Done).
fn relay_call(shared: &Shared, request: Request, mut send: impl FnMut(&Reply)) {
    // Which session here ran it (signed), for the Mac to say who asked and to find the project.
    let session = match (&request.session, &request.sig) {
        (Some(id), Some(sig)) if *sig == signature(id) => Some(id.clone()),
        _ => None,
    };
    let (title, agent, project) = session
        .as_deref()
        .and_then(|id| lock(shared).session_brief(id))
        .map(|(t, a, p)| (Some(t), Some(a), p))
        .unwrap_or((None, None, None));

    let (id, rx) = {
        let mut relay = relay();
        let Some(mac_id) = latest_mac(&relay) else {
            send(&Reply::fail(code::NOT_RUNNING, "Glasscode on your Mac is not connected to this server right now. The glasscode command works through the Mac; try again when it is online."));
            return;
        };
        let (tx, rx) = mpsc::channel();
        let mac = relay.macs.get_mut(&mac_id).unwrap();
        let id = fresh_id(&mut mac.next_call);
        let call = RemoteCall { id, request, session, session_title: title, agent, project };
        mac.calls.push((id, serde_json::to_value(&call).unwrap_or_default()));
        relay.waiting.insert(id, (tx, mac_id.clone()));
        (id, rx)
    };
    let started = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(reply) => {
                let done = matches!(reply, Reply::Done { .. });
                send(&reply);
                if done {
                    relay().waiting.remove(&id);
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let gone = {
                    let relay = relay();
                    relay.waiting.get(&id).map(|(_, mac)| relay.macs.get(mac).is_none_or(|m| !m.fresh())).unwrap_or(true)
                };
                if gone {
                    let mut relay = relay();
                    relay.waiting.remove(&id);
                    for mac in relay.macs.values_mut() {
                        mac.calls.retain(|(cid, _)| *cid != id);
                    }
                    send(&Reply::fail(code::NOT_RUNNING, "Glasscode on your Mac went offline before it answered. Try again when it is back."));
                    return;
                }
                if started.elapsed() > ANSWER_WITHIN {
                    relay().waiting.remove(&id);
                    send(&Reply::fail(code::FAILED, "The Mac did not answer in time."));
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                relay().waiting.remove(&id);
                return;
            }
        }
    }
}

fn serve_connection(stream: UnixStream, shared: Shared) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(read) = stream.try_clone() else { return };
    let mut line = String::new();
    if BufReader::new(read).read_line(&mut line).is_err() {
        return;
    }
    let mut out = stream;
    let mut write = |reply: &Reply| {
        let _ = writeln!(out, "{}", serde_json::to_string(reply).unwrap_or_default());
    };
    match serde_json::from_str::<Request>(line.trim()) {
        Ok(request) => relay_call(&shared, request, write),
        Err(e) => write(&Reply::fail(code::USAGE, format!("could not read the request: {e}"))),
    }
}

/// The `glasscode` command's socket here, request files in sessions' worktrees (for sandboxed
/// agents), the launcher on the PATH and the skill sessions get.
pub fn start(shared: Shared) {
    if let Ok(exe) = std::env::current_exe() {
        if let Err(e) = glasscode_core::cli::write_launcher(&exe) {
            log(format!("could not write the glasscode launcher: {e:#}"));
        }
    }
    if relay().skill_off != Some(true) {
        if let Err(e) = glasscode_core::cli::write_plugin() {
            log(format!("could not write the glasscode skill: {e:#}"));
        }
    }
    let path = control::socket_path();
    if let Err(e) = control::prepare_socket_dir(&path) {
        log(format!("control socket folder {}: {e}", path.display()));
        return;
    }
    if UnixStream::connect(&path).is_ok() {
        log(format!("another glasscoded answers {}", path.display()));
        return;
    }
    let _ = std::fs::remove_file(&path);
    match UnixListener::bind(&path) {
        Ok(listener) => {
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            let accept = shared.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let shared = accept.clone();
                    std::thread::spawn(move || serve_connection(stream, shared));
                }
            });
        }
        Err(e) => log(format!("glasscode command socket {}: {e}", path.display())),
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(400));
        let worktrees: Vec<PathBuf> = lock(&shared).session_worktrees();
        for worktree in worktrees {
            // Keep .glasscode/ out of the repository on the server too.
            let _ = glasscode_core::git::exclude(&worktree, ".glasscode/");
            let Ok(entries) = std::fs::read_dir(control::drop_dir(&worktree)) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                let plain = entry.file_type().is_ok_and(|t| t.is_file());
                match path.extension().and_then(|e| e.to_str()) {
                    Some("req") if plain && !glasscode_core::git::is_tracked(&worktree, &path) => {
                        let text = std::fs::read_to_string(&path).unwrap_or_default();
                        let _ = std::fs::remove_file(&path);
                        let out = path.with_extension("out");
                        let _ = std::fs::remove_file(&out);
                        let shared = shared.clone();
                        std::thread::spawn(move || {
                            let write = |reply: &Reply| {
                                if let Ok(mut file) = std::fs::OpenOptions::new().create_new(true).append(true).open(&out).or_else(|_| std::fs::OpenOptions::new().append(true).open(&out)) {
                                    let _ = writeln!(file, "{}", serde_json::to_string(reply).unwrap_or_default());
                                }
                            };
                            match serde_json::from_str::<Request>(text.trim()) {
                                Ok(request) => relay_call(&shared, request, write),
                                Err(e) => write(&Reply::fail(code::USAGE, format!("could not read the request: {e}"))),
                            }
                        });
                    }
                    Some("req") if plain => {
                        let _ = std::fs::remove_file(&path);
                    }
                    Some("out" | "tmp") if plain => {
                        if entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().unwrap_or_default() > Duration::from_secs(86_400)) {
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                    _ => {
                        if !plain {
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                }
            }
        }
    });
}
