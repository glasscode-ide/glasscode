//! Mac sessions synced to this server ("mirrors"). A connected Mac sends each of its
//! sessions every few seconds: the state (`mirror.sync`), the worktree as git refs
//! (`refs/glasscode/mirror/<token>/{head,snap}` in this server's clone of the project) and the
//! conversation (`mirror.conversation`, appended as it grows).
//!
//! Phones see mirrors next to the server's own sessions. What they do to one (answer, type,
//! keys) waits in its inbox until the Mac picks it up with its next sync and does it there.
//! "Continue here" (`mirror.take_over`) sets the worktree up from the refs, brings the
//! conversation into the matching account and resumes the session here; the Mac hears about it
//! with its next sync and closes its copy, so one machine runs a conversation at a time.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use glasscode_core::agents::AgentActivity;
use glasscode_core::caller::Caller;
use glasscode_core::model::AgentKind;
use glasscode_core::paths;
use glasscode_core::remote::{CreateSession, MirrorInput, MirrorMeta, MirrorSync, MirrorSyncReply, RemoteSession};
use glasscode_core::{sync, transcript};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::daemon::{Shared, log};

/// A Mac that has not synced for this long is asleep or away: its sessions can be continued
/// here even if they were working when it left.
const AWAY_AFTER: u64 = 90;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mirror {
    pub mac_id: String,
    pub mac: String,
    pub home: PathBuf,
    pub meta: MirrorMeta,
    pub synced_at: u64,
    /// Continued here: the server session it became.
    #[serde(default)]
    pub moved_to: Option<String>,
    /// The Mac's machine id, for the owner record when a session is continued here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<glasscode_core::paths::MachineId>,
    /// Its Mac said it is going to sleep (the last sync before it).
    #[serde(default)]
    pub asleep: bool,
    #[serde(skip)]
    pub inbox: Vec<MirrorInput>,
    /// The last reply, read from the conversation when it changes.
    #[serde(skip)]
    pub reply: Option<(u64, Option<String>, Option<u64>)>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn root() -> PathBuf {
    paths::root().join("mirrors")
}

/// Session ids are the Mac's (UUIDs); anything else never becomes a path.
fn safe(id: &str) -> anyhow::Result<&str> {
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("not a session id: {id}");
    }
    Ok(id)
}

pub fn conversation_path(id: &str) -> PathBuf {
    root().join(id).join("conversation.jsonl")
}

fn meta_path(id: &str) -> PathBuf {
    root().join(id).join("mirror.json")
}

fn save(mirror: &Mirror) {
    let id = &mirror.meta.session.id;
    let _ = std::fs::create_dir_all(root().join(id));
    if let Ok(bytes) = serde_json::to_vec_pretty(mirror) {
        let _ = std::fs::write(meta_path(id), bytes);
    }
}

fn forget(id: &str) {
    let _ = std::fs::remove_dir_all(root().join(id));
}

/// The mirrors kept from before a restart.
pub fn load() -> HashMap<String, Mirror> {
    let mut mirrors = HashMap::new();
    for entry in std::fs::read_dir(root()).into_iter().flatten().flatten() {
        let id = entry.file_name().to_string_lossy().to_string();
        if let Ok(mirror) = std::fs::read(meta_path(&id)).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice::<Mirror>(&b)?)) {
            mirrors.insert(id, mirror);
        }
    }
    mirrors
}

/// Read each mirror's last reply again when its conversation grew (the screen loop calls it).
pub fn refresh_replies(mirrors: &mut HashMap<String, Mirror>) {
    for m in mirrors.values_mut().filter(|m| m.moved_to.is_none()) {
        let path = conversation_path(&m.meta.session.id);
        let Ok(md) = std::fs::metadata(&path) else { continue };
        if m.reply.as_ref().is_none_or(|(size, _, _)| *size != md.len()) {
            let updated = md.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs());
            m.reply = Some((md.len(), transcript::last_reply(m.meta.session.agent, &path), updated));
        }
    }
}

/// Mirrors as phones list them (continued ones are server sessions now).
pub fn listed(mirrors: &HashMap<String, Mirror>) -> Vec<RemoteSession> {
    let mut list: Vec<RemoteSession> = mirrors
        .values()
        .filter(|m| m.moved_to.is_none())
        .map(|m| {
            let (_, last_reply, updated_at) = m.reply.clone().unwrap_or_default();
            RemoteSession {
                session: m.meta.session.clone(),
                running: m.meta.running,
                activity: m.meta.activity,
                tmux_name: String::new(),
                prompt: m.meta.prompt.clone(),
                last_reply,
                updated_at: updated_at.or(Some(m.synced_at)),
                pr: None,
                host: Some(m.mac.clone()),
                synced_at: Some(m.synced_at),
                signed_out: false,
            }
        })
        .collect();
    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    list
}

/// `mirror.*` requests, and session requests for a mirror's id (phones treat mirrors like
/// sessions). None: not ours, the daemon answers it. What a phone types waits with the device its
/// connection declared, so the Mac records it as that phone's words (M13.2).
pub fn dispatch(shared: &Shared, caller: &Caller, method: &str, params: &Value) -> Option<anyhow::Result<Value>> {
    let device = match caller {
        Caller::Phone { device } => Some(device.clone()),
        _ => None,
    };
    match method {
        "mirror.sync" => Some(sync_from_mac(shared, caller, params)),
        "mirror.conversation" => Some(append_conversation(shared, params)),
        "mirror.conversation_size" => Some((|| {
            let id = safe(params.get("session_id").and_then(Value::as_str).unwrap_or_default())?.to_string();
            if !crate::daemon::lock(shared).mirrors.contains_key(&id) {
                bail!("no synced session {id}");
            }
            Ok(json!(std::fs::metadata(conversation_path(&id)).map(|m| m.len()).unwrap_or(0)))
        })()),
        "mirror.conversation_into" => Some(conversation_into(shared, params)),
        "mirror.take_over" => Some(take_over(shared, params)),
        "sessions.send" | "sessions.answer" | "sessions.keys" | "sessions.transcript" | "sessions.attach" | "sessions.stop" | "sessions.close" | "sessions.rename" | "sessions.peek" => {
            let id = params.get("session_id").and_then(Value::as_str)?.to_string();
            let mac = {
                let daemon = crate::daemon::lock(shared);
                if daemon.has_session(&id) {
                    return None;
                }
                daemon.mirrors.get(&id).filter(|m| m.moved_to.is_none()).map(|m| m.mac.clone())?
            };
            Some(match method {
                "sessions.send" => queue(shared, &id, MirrorInput::Text {
                    text: params.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
                    enter: params.get("enter").and_then(Value::as_bool).unwrap_or(true),
                    device,
                }),
                "sessions.answer" => queue(shared, &id, MirrorInput::Answer {
                    option: params.get("option").and_then(Value::as_u64).unwrap_or(1) as u32,
                    text: params.get("text").and_then(Value::as_str).map(str::to_string),
                    device,
                }),
                "sessions.keys" => queue(shared, &id, MirrorInput::Keys {
                    keys: params.get("keys").and_then(Value::as_array).map(|k| k.iter().filter_map(|k| k.as_str().map(str::to_string)).collect()).unwrap_or_default(),
                }),
                "sessions.transcript" => read_transcript(shared, &id, params),
                _ => Err(anyhow!("this session runs on {mac}; continue it on the server first")),
            })
        }
        _ => None,
    }
}

fn sync_from_mac(shared: &Shared, caller: &Caller, params: &Value) -> anyhow::Result<Value> {
    let mac_id = params.get("mac_id").and_then(Value::as_str).unwrap_or_default().to_string();
    let mut sync: MirrorSync = serde_json::from_value(params.clone())?;
    // The Mac's approved authority: kept, one way (crit-5), and only from a connection that
    // declared itself the Mac (`Caller::Machine`, M13.2), with the same machine id the copy names.
    // A copy whose hash does not match is not kept, and nothing in the reply carries authority back.
    let primary = match caller {
        Caller::Machine { id } => is_primary(id),
        _ => false,
    };
    if let Some(copy) = sync.authority.take() {
        match caller {
            Caller::Machine { id } if copy.from.as_ref() == Some(id) && !primary => {
                crate::daemon::log(format!("authority from {id}, an observer Mac: not kept (this server keeps its primary Mac's)"));
            }
            Caller::Machine { id } if copy.from.as_ref() == Some(id) => crate::daemon::keep_authority(copy),
            _ => crate::daemon::log("authority in a sync from a connection that is not that Mac's: not kept"),
        }
    }
    // What the server's manager needs from the Mac (M57), from that Mac's own connection only.
    if let Some(said) = sync.manager.take() {
        match caller {
            Caller::Machine { id } if sync.machine.as_ref() == Some(id) && primary => crate::brain::keep_from_mac(said),
            Caller::Machine { .. } if !primary => {}
            _ => crate::daemon::log("the manager's settings in a sync from a connection that is not that Mac's: not kept"),
        }
    }
    // The Mac's engine switch, the same way (M57): this server's journal, words and conversation
    // run as the Mac's do.
    if let Some(engine) = sync.engine.take() {
        match caller {
            Caller::Machine { id } if sync.machine.as_ref() == Some(id) && primary => keep_engine(engine),
            Caller::Machine { .. } if !primary => {}
            _ => crate::daemon::log("an engine switch in a sync from a connection that is not that Mac's: not kept"),
        }
    }
    // Today for phones, and the `glasscode` command's answers and calls (kept until the Mac
    // confirms it has them; it sends back what it got last time).
    let (today_ops, cli_calls, board) = crate::today::exchange(crate::today::Exchange {
        mac_id: mac_id.clone(),
        today: sync.today.take(),
        board: sync.board.take(),
        replies: std::mem::take(&mut sync.cli_replies),
        today_acks: std::mem::take(&mut sync.today_acks),
        cli_acks: std::mem::take(&mut sync.cli_acks),
        skill_off: sync.skill_off,
    });
    let mut reply = MirrorSyncReply { today_ops, cli_calls, board, observer: !primary, ..Default::default() };
    // The v2 stores, before the daemon's lock: they read and write the disk.
    manager_stores(&mut sync, &mac_id, &mut reply);
    let mut daemon = crate::daemon::lock(shared);
    let at = now();
    let mut listed = HashSet::new();
    for meta in sync.sessions {
        let id = safe(&meta.session.id)?.to_string();
        listed.insert(id.clone());
        let mirror = daemon.mirrors.entry(id.clone()).or_insert_with(|| Mirror {
            mac_id: mac_id.clone(),
            mac: sync.mac.clone(),
            home: sync.home.clone(),
            meta: meta.clone(),
            synced_at: at,
            moved_to: None,
            machine: sync.machine.clone(),
            asleep: sync.asleep,
            inbox: Vec::new(),
            reply: None,
        });
        let env = meta.env.clone().or_else(|| mirror.meta.env.clone());
        let changed = serde_json::to_value(&mirror.meta).ok() != serde_json::to_value(&meta).ok() || mirror.mac != sync.mac;
        mirror.meta = meta;
        mirror.meta.env = env;
        mirror.mac = sync.mac.clone();
        mirror.mac_id = mac_id.clone();
        mirror.machine = sync.machine.clone();
        mirror.asleep = sync.asleep;
        mirror.home = sync.home.clone();
        mirror.synced_at = at;
        if let Some(moved) = &mirror.moved_to {
            reply.moved.insert(id.clone(), moved.clone());
        }
        let inbox = std::mem::take(&mut mirror.inbox);
        if !inbox.is_empty() {
            reply.inbox.insert(id.clone(), inbox);
        }
        reply.conversation_sizes.insert(id.clone(), std::fs::metadata(conversation_path(&id)).map(|m| m.len()).unwrap_or(0));
        if changed {
            save(mirror);
        }
    }
    // Who owns the worktrees of this Mac's sessions, where a record says.
    let mut keys: Vec<glasscode_core::owners::WorktreeKey> = daemon.mirrors.values().filter(|m| m.mac_id == mac_id).filter_map(|m| m.meta.key.clone()).collect();
    keys.extend(sync.worktree_keys.iter().cloned());
    reply.owners = crate::owners::get(&keys);
    reply.owners_unknown = !crate::owners::known();
    // This Mac's sessions it no longer has: closed, or moved here and closed there.
    let gone: Vec<String> = daemon.mirrors.iter().filter(|(id, m)| m.mac_id == mac_id && !listed.contains(*id)).map(|(id, _)| id.clone()).collect();
    for id in gone {
        let mirror = daemon.mirrors.remove(&id);
        forget(&id);
        if let Some(mirror) = mirror {
            delete_refs(&daemon, &mirror);
        }
    }
    Ok(serde_json::to_value(reply)?)
}

/// How long the primary Mac may be away before another Mac's authority is kept (a Mac replaced).
const PRIMARY_AWAY_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Whether `mac` is this server's primary Mac (M57): the one whose approved authority it keeps.
/// Another Mac is an observer: it reads, shows and answers approvals, and acts on its own sessions
/// only; its authority, engine and manager settings are not kept, and it is never the manager's
/// home. With no authority kept yet, or the primary away for a week, any Mac may be the primary.
pub fn is_primary(mac: &glasscode_core::paths::MachineId) -> bool {
    use glasscode_core::store::Store as _;
    let now = glasscode_core::clock::now_ms();
    match glasscode_core::gate::FromMac::load().ok().and_then(|copy| copy.from.clone().map(|from| (from, copy.received_ms))) {
        Some((primary, received_ms)) => &primary == mac || now.saturating_sub(received_ms) > PRIMARY_AWAY_MS,
        None => true,
    }
}

/// Keep the Mac's engine switch (its features) when it differs from this server's; the journal and
/// the words ledger read the features again.
fn keep_engine(engine: glasscode_core::store::EngineSwitch) {
    use glasscode_core::store::{EngineSwitch, Store as _};
    if EngineSwitch::load().ok().as_ref() == Some(&engine) {
        return;
    }
    match glasscode_core::store::update::<EngineSwitch, ()>(|switch| *switch = engine) {
        Ok(()) => {
            glasscode_core::journal::decide_again();
            glasscode_core::words::decide_again();
            log("the manager's features here are the Mac's now".to_string());
        }
        Err(err) => log(format!("the Mac's engine switch was not kept: {err}")),
    }
}

/// The manager's v2 stores (M57), both ways. The Mac's records come in: its own journal events
/// (each once, by id: a lost reply sends them again), its effect records by key and generation, its
/// words into its folder, its conversation changes by stamp, its pending actions (answers taken,
/// no authority) and its work items by stamp. This server's own go back from where the Mac says it
/// has them (`have`), and the reply says where they stopped: this server keeps no state of it, so
/// a reply lost on the way brings them again.
fn manager_stores(sync: &mut MirrorSync, mac_id: &str, reply: &mut MirrorSyncReply) {
    use glasscode_core::store::Store as _;
    let here = glasscode_core::paths::MachineId::this().ok();
    let mut not_taken: Vec<String> = Vec::new();
    let journal = std::mem::take(&mut sync.journal);
    if !journal.is_empty() {
        let mut seen = JOURNAL_SEEN.lock().unwrap_or_else(|p| p.into_inner());
        let seen = seen.get_or_insert_with(HashMap::new).entry(mac_id.to_string()).or_default();
        if seen.len() > 100_000 {
            // A run this long: the oldest ids are long past any resend.
            seen.clear();
        }
        for event in journal {
            if seen.insert(event.id.clone()) {
                glasscode_core::journal::append_stamped(event);
            }
        }
    }
    if !sync.effects.is_empty() {
        if let Err(err) = glasscode_core::effects::merge_theirs(&sync.effects) {
            log(format!("effects: the Mac's records could not be merged: {err}"));
            not_taken.push("effects".into());
        }
    }
    if let (false, Some(mac)) = (sync.words.is_empty(), sync.machine.as_ref()) {
        if let Err(err) = glasscode_core::words::take_in(mac, &sync.words) {
            log(format!("words: the Mac's records could not be kept: {err}"));
            not_taken.push("words".into());
        }
    }
    if let Err(err) = glasscode_core::conversation::take_from_other_home(std::mem::take(&mut sync.conversation)) {
        log(format!("conversation: the Mac's changes could not be taken: {err}"));
        not_taken.push("conversation".into());
    }
    if let (false, Some(mac), Some(here)) = (sync.pending.is_empty(), sync.machine.as_ref(), here.as_ref()) {
        let now = glasscode_core::clock::now_ms();
        if let Err(err) = glasscode_core::store::update::<glasscode_core::gate::Pending, usize>(|p| p.merge_theirs(&sync.pending, mac, here, true, now)) {
            log(format!("pending: the Mac's actions could not be merged: {err}"));
            not_taken.push("pending".into());
        }
    }
    if !sync.work.is_empty() {
        if let Err(err) = glasscode_core::store::update::<glasscode_core::work::WorkLedger, Vec<glasscode_core::identity::WorkKey>>(|ledger| ledger.merge_theirs(&sync.work)) {
            log(format!("work: the Mac's items could not be merged: {err}"));
            not_taken.push("work".into());
        }
    }
    if !sync.follow_ups.is_empty() {
        if let Err(err) = glasscode_core::store::update::<glasscode_core::work::WorkLedger, bool>(|ledger| ledger.follow_merge(&sync.follow_ups)) {
            log(format!("follow-ups: the Mac's could not be merged: {err}"));
            not_taken.push("follow_ups".into());
        }
    }

    // This server's own, from where the Mac has them.
    let have = &sync.have;
    let mut next = have.clone();
    let read_at = glasscode_core::clock::now_ms();
    reply.effects = glasscode_core::effects::changed_since(have.effects_ms).unwrap_or_default();
    reply.pending = glasscode_core::gate::Pending::load().map(|p| p.changed_since(have.pending_ms)).unwrap_or_default();
    (next.effects_ms, next.pending_ms) = (read_at, read_at);
    reply.words = glasscode_core::words::own_since(have.words_ms);
    if let Some(last) = reply.words.last() {
        next.words_ms = last.at.wall_ms;
    }
    if let Some(here) = here.as_ref() {
        reply.conversation = glasscode_core::conversation::own_changed(&mut next.conversation, here, 2_000);
        let ledger = glasscode_core::work::WorkLedger::load().ok();
        reply.work = ledger.as_ref().map(|ledger| ledger.own_changed(here, have.work.as_ref())).unwrap_or_default();
        if let Some(last) = reply.work.last() {
            next.work = Some(last.updated.clone());
        }
        reply.follow_ups = ledger.as_ref().map(|ledger| ledger.follow_changed(here, have.follow_ups.as_ref())).unwrap_or_default();
        if let Some(last) = reply.follow_ups.last() {
            next.follow_ups = Some(last.stamp.clone());
        }
    }
    reply.have = next;
    reply.not_taken = not_taken;
    // This server's latest stamp (crit-9): the Mac tells by it whether its own clock went back.
    if let Some(here) = here.as_ref() {
        use glasscode_core::store::Store as _;
        let last = glasscode_core::identity::LastStamp::load().ok().and_then(|file| file.last);
        let now = glasscode_core::clock::now_ms();
        reply.server_stamp = Some(match last {
            Some(last) if last.wall_ms > now => glasscode_core::identity::Hlc { machine: here.clone(), ..last },
            _ => glasscode_core::identity::Hlc { wall_ms: now, count: 0, machine: here.clone() },
        });
    }
    reply.notices = crate::brain::take_notices();
    reply.notices.extend(crate::followups::take_notices());
    reply.window_ops = crate::brain::window_ops_for_mac(&sync.window_acks);
}

/// The ids of the journal events each Mac sent this run (a lost reply sends them again).
static JOURNAL_SEEN: std::sync::Mutex<Option<HashMap<String, HashSet<String>>>> = std::sync::Mutex::new(None);

fn delete_refs(daemon: &crate::daemon::Daemon, mirror: &Mirror) {
    if let Some(root) = daemon.project_root(&mirror.meta.project_id) {
        for name in ["head", "snap"] {
            let _ = std::process::Command::new("git").arg("-C").arg(&root).args(["update-ref", "-d", &format!("refs/glasscode/mirror/{}/{name}", mirror.meta.token)]).output();
        }
    }
}

fn append_conversation(shared: &Shared, params: &Value) -> anyhow::Result<Value> {
    let id = safe(params.get("session_id").and_then(Value::as_str).unwrap_or_default())?.to_string();
    let offset = params.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let data = base64::engine::general_purpose::STANDARD.decode(params.get("data").and_then(Value::as_str).unwrap_or_default())?;
    if !crate::daemon::lock(shared).mirrors.contains_key(&id) {
        bail!("no synced session {id}");
    }
    let path = conversation_path(&id);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let mut file = if offset == 0 {
        std::fs::File::create(&path)?
    } else if offset == size {
        std::fs::OpenOptions::new().append(true).open(&path)?
    } else {
        bail!("the server has {size} bytes of this conversation, not {offset}");
    };
    file.write_all(&data)?;
    Ok(json!({ "size": offset + data.len() as u64 }))
}

/// A move to this server: the mirrored copy of a Mac session's conversation (the Mac sent only
/// what it lacked) goes into an account here, with its folder paths rewritten to the worktree
/// here. Returns where it is.
fn conversation_into(shared: &Shared, params: &Value) -> anyhow::Result<Value> {
    let id = safe(params.get("session_id").and_then(Value::as_str).unwrap_or_default())?.to_string();
    let account_id = params.get("account_id").and_then(Value::as_str).unwrap_or_default().to_string();
    let rel = PathBuf::from(params.get("rel").and_then(Value::as_str).unwrap_or_default());
    if rel.as_os_str().is_empty() || rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        bail!("bad conversation path");
    }
    let from = PathBuf::from(params.get("from").and_then(Value::as_str).unwrap_or_default());
    let to = PathBuf::from(params.get("to").and_then(Value::as_str).unwrap_or_default());
    let account = {
        let daemon = crate::daemon::lock(shared);
        if !daemon.mirrors.contains_key(&id) {
            bail!("no synced session {id}");
        }
        daemon.accounts().0.into_iter().find(|a| a.id == account_id).ok_or_else(|| anyhow!("no account {account_id}"))?
    };
    let content = sync::rewrite_cwd(&std::fs::read(conversation_path(&id))?, &from, &to);
    let target = account.effective_dir().join(rel);
    std::fs::create_dir_all(target.parent().unwrap_or(Path::new("/")))?;
    std::fs::write(&target, content)?;
    Ok(serde_json::to_value(target)?)
}

fn queue(shared: &Shared, id: &str, input: MirrorInput) -> anyhow::Result<Value> {
    let mut daemon = crate::daemon::lock(shared);
    let mirror = daemon.mirrors.get_mut(id).ok_or_else(|| anyhow!("no synced session {id}"))?;
    if matches!(input, MirrorInput::Answer { .. }) {
        // Answered: not open any more for phones, until the Mac says otherwise.
        mirror.meta.prompt = None;
        mirror.meta.activity = Some(AgentActivity::Working);
    }
    mirror.inbox.push(input);
    Ok(Value::Null)
}

fn read_transcript(shared: &Shared, id: &str, params: &Value) -> anyhow::Result<Value> {
    let agent = crate::daemon::lock(shared).mirrors.get(id).map(|m| m.meta.session.agent).ok_or_else(|| anyhow!("no synced session {id}"))?;
    let after = params.get("after").and_then(Value::as_u64);
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(400).min(2000) as usize;
    let path = conversation_path(id);
    if !path.is_file() {
        return Ok(serde_json::to_value(transcript::TranscriptPage { items: Vec::new(), cursor: 0, reset: after.is_none() })?);
    }
    Ok(serde_json::to_value(transcript::read(agent, &path, after, limit)?)?)
}

/// Continue a Mac session here: its worktree from the synced refs, its conversation in the
/// matching account, and the session resumed. The Mac closes its copy when it next syncs.
fn take_over(shared: &Shared, params: &Value) -> anyhow::Result<Value> {
    let id = safe(params.get("session_id").and_then(Value::as_str).unwrap_or_default())?.to_string();
    let (mirror, project, server_home, accounts, defaults) = {
        let daemon = crate::daemon::lock(shared);
        let mirror = daemon.mirrors.get(&id).cloned().ok_or_else(|| anyhow!("no synced session {id}"))?;
        if let Some(moved) = &mirror.moved_to {
            if let Some(session) = daemon.snapshot().sessions.into_iter().find(|s| &s.session.id == moved) {
                return Ok(serde_json::to_value(session)?);
            }
        }
        let project = daemon.project(&mirror.meta.project_id).ok_or_else(|| anyhow!("the project of {} is not on this server", mirror.meta.session.title))?;
        let (accounts, defaults) = daemon.accounts();
        (mirror, project, paths::home(), accounts, defaults)
    };
    // Asleep (its last sync said so) or silent for a while: a working session continues here.
    let away = mirror.asleep || now().saturating_sub(mirror.synced_at) > AWAY_AFTER;
    if mirror.meta.activity == Some(AgentActivity::Working) && !away {
        bail!("{} is working on {} right now; continue it here when it finishes", mirror.meta.session.title, mirror.mac);
    }
    let session = &mirror.meta.session;
    let resolve = |name: &str| -> anyhow::Result<String> {
        let out = std::process::Command::new("git").arg("-C").arg(&project.root).args(["rev-parse", "--verify", "--quiet", &format!("refs/glasscode/mirror/{}/{name}", mirror.meta.token)]).output()?;
        if !out.status.success() {
            bail!("{} has not synced its worktree yet", session.title);
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let (head, snap) = (resolve("head")?, resolve("snap")?);

    // 1. The worktree, where it belongs on this machine.
    let path = sync::map_path(&session.worktree, &mirror.home, &server_home, &project.name, mirror.meta.branch.as_deref());
    let worktree = match sync::matching_worktree(&project.root, &path, mirror.meta.branch.as_deref(), &head, &snap) {
        Some(path) => path,
        None => {
            crate::daemon::lock(shared).stop_sessions_in(&path);
            let repo_lock = crate::daemon::project_lock(&project.root.display().to_string());
            let _one_at_a_time = repo_lock.lock().unwrap_or_else(|p| p.into_inner());
            sync::receive_worktree(&project.root, &path, mirror.meta.branch.as_deref(), &head, &snap)?
        }
    };
    if let Some(env) = &mirror.meta.env {
        sync::write_env_files(&worktree, env)?;
    }

    // 2. The conversation, in the matching account here.
    let account_id = if session.agent.uses_accounts() { sync::map_account(mirror.meta.account.as_ref(), session.agent, &accounts, &defaults) } else { None };
    let mut resume = None;
    if let (Some(account_id), Some(conversation)) = (account_id.as_ref(), session.agent_session_id.as_ref()) {
        let account = accounts.iter().find(|a| &a.id == account_id).context("the matching account is gone")?;
        let file = conversation_path(&id);
        if file.is_file() {
            let rel = match session.agent {
                AgentKind::Claude => PathBuf::from("projects").join(glasscode_core::history::claude_folder_name(&worktree)).join(format!("{conversation}.jsonl")),
                _ => mirror.meta.conversation_rel.clone().filter(|r| r.is_relative() && !r.components().any(|c| matches!(c, std::path::Component::ParentDir))).context("where the conversation goes is unknown")?,
            };
            let target = account.effective_dir().join(rel);
            std::fs::create_dir_all(target.parent().unwrap_or(Path::new("/")))?;
            std::fs::write(&target, sync::rewrite_cwd(&std::fs::read(&file)?, &session.worktree, &worktree))?;
            resume = Some(conversation.clone());
        } else if session.agent == AgentKind::Claude {
            resume = Some(conversation.clone());
        }
    }

    // 3. The session, resumed.
    let create = CreateSession {
        project_id: project.id.clone(),
        worktree: Some(worktree),
        new_branch: None,
        agent: session.agent,
        account_id,
        title: Some(session.title.clone()),
        resume,
        initial_prompt: None,
        start: None,
        review: session.review.clone(),
        review_files: Default::default(),
        model: session.model.clone(),
        mode: session.mode.clone(),
        held: None,
    };
    // The worktree is this server's from now on (a new epoch), before anything starts here, so
    // a worktree the record gives to the Mac can be continued too; the Mac's copy stops when it
    // next syncs.
    if let (Some(key), Some(mac)) = (&mirror.meta.key, &mirror.machine) {
        crate::owners::taken_here(key, mac);
    }
    let created = crate::daemon::create_session(shared, create)?;
    {
        let mut daemon = crate::daemon::lock(shared);
        if let Some(m) = daemon.mirrors.get_mut(&id) {
            m.moved_to = Some(created.session.id.clone());
            m.inbox.clear();
            save(m);
        }
    }
    log(format!("continued {} from {} here", session.title, mirror.mac));
    // A worktree moves as one: the Mac's other sessions in it continue here too.
    if params.get("alone").and_then(Value::as_bool) != Some(true) {
        if let Some(key) = &mirror.meta.key {
            let siblings: Vec<String> = crate::daemon::lock(shared)
                .mirrors
                .iter()
                .filter(|(other, m)| **other != id && m.mac_id == mirror.mac_id && m.meta.key.as_ref() == Some(key) && m.moved_to.is_none())
                .map(|(other, _)| other.clone())
                .collect();
            for sibling in siblings {
                if let Err(err) = take_over(shared, &json!({ "session_id": sibling, "alone": true })) {
                    log(format!("could not continue {sibling} with its worktree: {err:#}"));
                }
            }
        }
    }
    Ok(serde_json::to_value(created)?)
}

#[cfg(test)]
mod tests {
    use glasscode_core::gate::{Pending, PendingAction};
    use glasscode_core::identity::{Hlc, WorkKey, WorkKind};
    use glasscode_core::paths::{MachineId, ProjectKey};
    use glasscode_core::work::{WorkItem, WorkLedger};

    use super::*;

    /// M57: the server takes the Mac's conversation into its manager stores, and keeps the Mac's
    /// engine switch: its features are the server's then.
    #[test]
    fn the_server_takes_the_macs_switch() {
        use glasscode_core::store::{Engine, EngineSwitch, Store as _};
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        let machine = MachineId::parse("00000000000000aa").unwrap();
        let record = glasscode_core::conversation::Record::Topic(serde_json::from_value(json!({ "id": "general", "at": { "wall_ms": 1, "count": 0, "machine": machine }, "title": "General" })).unwrap());
        let mut sync: MirrorSync = serde_json::from_value(json!({ "mac": "mac", "home": "/home/mac", "sessions": [] })).unwrap();
        sync.conversation = vec![record];
        let mut reply = MirrorSyncReply::default();
        manager_stores(&mut sync, "mac-1", &mut reply);
        assert!(reply.not_taken.is_empty(), "{:?}", reply.not_taken);

        keep_engine(EngineSwitch { engine: Engine::V2, switches: std::collections::BTreeMap::from([("turn_hooks".to_string(), true)]) });
        assert!(EngineSwitch::load().unwrap().is_on("turn_hooks"));
        assert!(glasscode_core::journal::feature_on("turn_hooks"));
        // Back to every feature off before the scratch home goes.
        keep_engine(EngineSwitch::default());
        assert!(!glasscode_core::journal::feature_on("turn_hooks"));
    }

    /// M57: the server keeps no record of what it sent a Mac. Its records go from where the Mac
    /// says it has them, so a reply lost on the way brings them again; the reply's `have`, sent
    /// back, brings nothing more. A journal event sent twice is kept once.
    #[test]
    fn a_lost_reply_brings_the_servers_records_again() {
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        let here = MachineId::this().unwrap();
        let now = glasscode_core::clock::now_ms();
        glasscode_core::store::update::<Pending, PendingAction>(|p| p.add(PendingAction { id: "a".repeat(32), created_ms: now - 1_000, expires_ms: now + 60_000, ..PendingAction::default() }, now)).unwrap();
        let key = WorkKey { project: ProjectKey::parse("app-3f9a1c").unwrap(), kind: WorkKind::Branch, id: "feat/x".into() };
        glasscode_core::store::update::<WorkLedger, ()>(|l| l.items.push(WorkItem::new(key, Hlc { wall_ms: now - 1_000, count: 0, machine: here.clone() }))).unwrap();

        let ask = |have: glasscode_core::remote::HaveCursors| {
            let mut sync: MirrorSync = serde_json::from_value(json!({ "mac": "mac", "home": "/home/mac", "sessions": [], "have": have })).unwrap();
            let mut reply = MirrorSyncReply::default();
            manager_stores(&mut sync, "mac-1", &mut reply);
            reply
        };
        let first = ask(Default::default());
        assert_eq!((first.pending.len(), first.work.len()), (1, 1));
        let lost = ask(Default::default());
        assert_eq!((lost.pending.len(), lost.work.len()), (1, 1), "the reply was lost: again");
        let next = ask(first.have.clone());
        assert_eq!((next.pending.len(), next.work.len()), (0, 0), "taken in: nothing more");
    }
}
