//! This Mac's sessions, kept in sync with its server while it is connected, so the iPhone
//! sees them and anyone can continue one on the server (`glasscoded`'s mirror module).
//!
//! Every round (a few seconds): a cheap fingerprint of each worktree (HEAD and `git status`);
//! when it changed, a snapshot of the commits and uncommitted work is pushed as git refs
//! (only new objects travel). Then one `mirror.sync` with every session's state, which brings
//! back what phones asked for and which sessions were continued on the server; then whatever
//! part of each conversation the server does not have yet.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use base64::Engine as _;
use serde_json::json;

use crate::agents::AgentActivity;
use crate::model::{AgentKind, Project, State};
use crate::prompt::PendingPrompt;
use crate::remote::{MirrorInput, MirrorMeta, MirrorSync, MirrorSyncReply, RemoteClient, ServerConfig, ServerState};
use crate::sync;

/// What the app sees of a session right now.
#[derive(Clone, Debug, Default)]
pub struct Live {
    pub running: bool,
    pub activity: Option<AgentActivity>,
    pub prompt: Option<PendingPrompt>,
}

#[derive(Default)]
struct Tracked {
    fingerprint: u64,
    pushed: Option<(String, String)>,
    branch: Option<String>,
    env: Option<u64>,
}

/// What a round brought back.
#[derive(Debug, Default)]
pub struct Outcome {
    pub inbox: HashMap<String, Vec<MirrorInput>>,
    pub moved: HashMap<String, String>,
    /// The owner records of this Mac's worktrees that have one: a worktree the server owns now
    /// must not run here.
    pub owners: HashMap<crate::owners::WorktreeKey, crate::owners::OwnerRecord>,
    /// This Mac's machine id, as the round read it.
    pub machine: Option<crate::paths::MachineId>,
    /// The server could not read its owner records: keep what was known.
    pub owners_unknown: bool,
    /// The projects that stay on this Mac, as this round read them (the switch leaves them out).
    pub local_only: std::collections::HashSet<String>,
    /// The server's Today board after it merged this Mac's: merged here in turn.
    pub board: Option<crate::board::Board>,
    /// What phones did on Today (with their `seq`), and `glasscode` commands from server
    /// sessions; and every `seq` and call id that came, to acknowledge with the next sync (also
    /// the ones this Mac could not read, so they are not sent forever).
    pub today_ops: Vec<(u64, crate::remote::TodayOp)>,
    pub cli_calls: Vec<crate::remote::RemoteCall>,
    pub today_seen: Vec<u64>,
    pub calls_seen: Vec<u64>,
    pub synced: usize,
    /// Sessions that could not be synced, and why (a project without a remote, say).
    pub skipped: Vec<(String, String)>,
}

/// What goes with a sync besides the sessions: Today for phones, answers for the server's
/// sessions, and what arrived last time.
#[derive(Default)]
pub struct Extras {
    /// The keys of this Mac's worktrees with no open session, so the server says which of them
    /// it owns now (a parked copy opened there, or one moved without a session).
    pub worktree_keys: Vec<crate::owners::WorktreeKey>,
    /// This Mac's Today board, merged with the server's copy.
    pub board: Option<serde_json::Value>,
    /// The last sync before this Mac sleeps.
    pub asleep: bool,
    pub today: Option<serde_json::Value>,
    pub cli_replies: Vec<crate::remote::RemoteCallReply>,
    pub today_acks: Vec<u64>,
    pub cli_acks: Vec<u64>,
    pub skill_off: bool,
    /// The approved authority for the server (v2 homes, and servers that announce "authority").
    pub authority: Option<crate::gate::FromMac>,
}

#[derive(Default)]
pub struct Syncer {
    sessions: HashMap<String, Tracked>,
    /// Local project id → the server's project.
    projects: HashMap<String, Project>,
    /// Projects that could not be set up on the server, and when to try again.
    failed: HashMap<String, (String, Instant)>,
    projects_at: Option<Instant>,
    /// The memory the server's copy was last made from: (memory head, what it left out), and
    /// the copy's commit (M20.3).
    memory_sent: Option<((String, Vec<String>), String)>,
    /// What of the v2 stores this Mac sent already (M57): its journal, and its effect records by
    /// the time they last changed.
    journal_sent: Option<crate::journal::Cursor>,
    effects_sent_ms: u64,
}

pub(crate) const CHUNK: usize = 512 * 1024;

fn hash<T: Hash>(value: &T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// HEAD, what `git status` lists, and those files' sizes and times: it changes whenever a
/// snapshot would.
fn fingerprint(dir: &std::path::Path) -> anyhow::Result<u64> {
    let mut cmd = sync::git_cmd(dir);
    cmd.args(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    let status = cmd.output().context("could not run git")?;
    if !status.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&status.stderr).trim());
    }
    let mut cmd = sync::git_cmd(dir);
    cmd.args(["rev-parse", "HEAD"]);
    let head = sync::run(cmd).unwrap_or_default();
    let mut stats = Vec::new();
    for entry in status.stdout.split(|b| *b == 0).filter(|e| e.len() > 3) {
        let path = dir.join(String::from_utf8_lossy(&entry[3..]).as_ref());
        if let Ok(meta) = std::fs::metadata(&path) {
            stats.push((meta.len(), meta.modified().ok()));
        }
    }
    Ok(hash(&(head, &status.stdout, stats)))
}

impl Syncer {
    /// The server's project for a local one, set up there (cloned) the first time.
    fn server_project(&mut self, client: &RemoteClient, state: &mut Option<ServerState>, local: &Project, server_home: &std::path::Path) -> anyhow::Result<Project> {
        if let Some(p) = self.projects.get(&local.id) {
            return Ok(p.clone());
        }
        if let Some((why, at)) = self.failed.get(&local.id) {
            if at.elapsed() < Duration::from_secs(600) {
                return Err(anyhow!("{why}"));
            }
        }
        if state.is_none() || self.projects_at.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
            *state = Some(client.call("state", json!({}))?);
            self.projects_at = Some(Instant::now());
        }
        let root = sync::map_path(&local.root, &crate::paths::home(), server_home, &local.name, None);
        // The same place under home, else the same project by name and folder name.
        let found = state.as_ref().and_then(|s| {
            s.projects.iter().find(|p| p.root == root).or_else(|| s.projects.iter().find(|p| p.name == local.name && p.root.file_name() == local.root.file_name())).cloned()
        });
        let project = match found {
            Some(p) => p,
            None => {
                let result = sync::origin_url(&local.root)
                    .ok_or_else(|| anyhow!("{} has no origin remote to clone on the server", local.name))
                    .and_then(|url| client.call::<Project>("projects.add", json!({ "url": url, "path": root, "name": local.name })));
                match result {
                    Ok(p) => p,
                    Err(error) => {
                        self.failed.insert(local.id.clone(), (format!("{error:#}"), Instant::now()));
                        return Err(error);
                    }
                }
            }
        };
        self.projects.insert(local.id.clone(), project.clone());
        Ok(project)
    }

    /// One round for every local session.
    #[allow(clippy::too_many_arguments)]
    pub fn sync(
        &mut self,
        client: &RemoteClient,
        config: &ServerConfig,
        local: &State,
        live: &HashMap<String, Live>,
        mac: &str,
        mac_id: &str,
        extras: Extras,
    ) -> anyhow::Result<Outcome> {
        let server_home = client.hello.home.clone();
        let mut state: Option<ServerState> = None;
        let mut outcome = Outcome::default();
        let mut metas = Vec::new();
        let mut conversations: Vec<(String, PathBuf)> = Vec::new();

        // A project that stays on this Mac sends nothing: no worktree, no conversation, no state.
        let local_only = crate::switch::local_only_projects(local);
        outcome.local_only = local_only.clone();
        for session in &local.sessions {
            if local_only.contains(&session.project_id) {
                continue;
            }
            let Some(project) = local.project(&session.project_id) else { continue };
            let server_project = match self.server_project(client, &mut state, project, &server_home) {
                Ok(p) => p,
                Err(error) => {
                    outcome.skipped.push((session.title.clone(), format!("{error:#}")));
                    continue;
                }
            };
            let token = format!("m{}", sync::token_for(&session.id));
            let tracked = self.sessions.entry(session.id.clone()).or_default();

            // The worktree, when it changed.
            match fingerprint(&session.worktree) {
                Ok(print) if print != tracked.fingerprint || tracked.pushed.is_none() => match sync::snapshot(&session.worktree) {
                    Ok(snap) => {
                        let pair = (snap.head.clone(), snap.snap.clone());
                        if tracked.pushed.as_ref() != Some(&pair) {
                            let mut push = sync::git_cmd(&session.worktree);
                            push.env("GIT_SSH_COMMAND", config.git_ssh_command()).args([
                                "push",
                                "--quiet",
                                "--force",
                                "--no-verify",
                                &config.git_url(&server_project.root),
                                &format!("{}:refs/glasscode/mirror/{token}/head", snap.head),
                                &format!("{}:refs/glasscode/mirror/{token}/snap", snap.snap),
                            ]);
                            match sync::run(push) {
                                Ok(_) => tracked.pushed = Some(pair),
                                Err(error) => {
                                    outcome.skipped.push((session.title.clone(), format!("could not push its worktree: {error:#}")));
                                    continue;
                                }
                            }
                        }
                        tracked.branch = snap.branch;
                        tracked.fingerprint = print;
                    }
                    Err(error) => outcome.skipped.push((session.title.clone(), format!("{error:#}"))),
                },
                Ok(_) => {}
                Err(error) => outcome.skipped.push((session.title.clone(), format!("{error:#}"))),
            }

            // `.env` files, when they changed.
            let env = sync::env_files(&session.worktree);
            let env_hash = hash(&env.iter().map(|e| (&e.path, &e.data)).collect::<Vec<_>>());
            let env = (tracked.env != Some(env_hash)).then_some(env);
            tracked.env = Some(env_hash);

            let account = session.account_id.as_deref().and_then(|id| local.account(id)).cloned();
            let conversation = account.as_ref().and_then(|a| sync::local_conversation(session, a));
            let conversation_rel = match (session.agent, &conversation, &account) {
                (AgentKind::Codex, Some(path), Some(a)) => path.strip_prefix(a.effective_dir()).ok().map(PathBuf::from),
                _ => None,
            };
            if let Some(path) = &conversation {
                conversations.push((session.id.clone(), path.clone()));
            }
            let now = live.get(&session.id).cloned().unwrap_or_default();
            metas.push(MirrorMeta {
                session: session.clone(),
                project_id: server_project.id.clone(),
                running: now.running,
                activity: now.activity,
                prompt: now.prompt,
                token,
                branch: tracked.branch.clone(),
                head: tracked.pushed.as_ref().map(|p| p.0.clone()),
                snap: tracked.pushed.as_ref().map(|p| p.1.clone()),
                env,
                account,
                conversation_rel,
                // A project's main checkout has no owner record (it is never a unit).
                key: (!crate::switch::is_main_checkout(&project.root, &session.worktree)).then(|| crate::owners::WorktreeKey::of(&session.worktree, &crate::paths::home(), &project.name, Some(&session.branch))),
            });
        }
        let ids: HashSet<String> = local.sessions.iter().map(|s| s.id.clone()).collect();
        self.sessions.retain(|id, _| ids.contains(id));

        let machine = crate::paths::MachineId::this().ok();
        outcome.machine = machine.clone();
        // The v2 stores (M57): this Mac's own journal events since the last sync, and its effect
        // records that changed. Events it pulled from the server are not sent back.
        let v2 = crate::store::EngineSwitch::current().engine == crate::store::Engine::V2;
        let (journal_events, journal_next) = if v2 {
            let cursor = self.journal_sent.clone().unwrap_or_else(crate::journal::end);
            let (events, next) = crate::journal::read_from(&cursor, 1_000);
            (events.into_iter().filter(|e| machine.as_ref() == Some(&e.at.machine)).collect(), Some(next))
        } else {
            (Vec::new(), None)
        };
        let sync_started_ms = crate::clock::now_ms();
        let effects = if v2 { crate::effects::changed_since(self.effects_sent_ms).unwrap_or_default() } else { Vec::new() };
        let sync = MirrorSync {
            mac: mac.to_string(),
            home: crate::paths::home(),
            sessions: metas,
            machine: machine.clone(),
            worktree_keys: extras.worktree_keys,
            board: extras.board,
            asleep: extras.asleep,
            journal: journal_events,
            effects,
            today: extras.today,
            cli_replies: extras.cli_replies,
            today_acks: extras.today_acks,
            cli_acks: extras.cli_acks,
            skill_off: extras.skill_off,
            // Stamped with this Mac's id, which is read here, off the main thread.
            authority: extras.authority.map(|copy| crate::gate::FromMac { from: machine.clone(), ..copy }),
        };
        let mut params = serde_json::to_value(&sync)?;
        params["mac_id"] = json!(mac_id);
        let reply: MirrorSyncReply = client.call("mirror.sync", params)?;
        outcome.synced = sync.sessions.len();

        // The rest of each conversation.
        for (id, path) in conversations {
            let have = reply.conversation_sizes.get(&id).copied().unwrap_or(0);
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let from = if (have as usize) > bytes.len() { 0 } else { have as usize };
            if from == bytes.len() {
                continue;
            }
            for (i, chunk) in bytes[from..].chunks(CHUNK).enumerate() {
                let offset = from + i * CHUNK;
                let sent = client.call::<serde_json::Value>(
                    "mirror.conversation",
                    json!({ "session_id": id, "offset": offset, "data": base64::engine::general_purpose::STANDARD.encode(chunk) }),
                );
                if sent.is_err() {
                    break;
                }
            }
        }
        outcome.inbox = reply.inbox;
        outcome.moved = reply.moved;
        outcome.owners = reply.owners;
        outcome.owners_unknown = reply.owners_unknown;
        outcome.board = reply.board.and_then(|b| serde_json::from_value(b).ok());
        if v2 {
            // Sent: the next sync goes on from here. The server's changes come in.
            if let Some(next) = journal_next {
                self.journal_sent = Some(next);
            }
            self.effects_sent_ms = sync_started_ms;
            if !reply.effects.is_empty() {
                if let Err(err) = crate::effects::merge_theirs(&reply.effects) {
                    log::warn!("effects: the server's records could not be merged: {err}");
                }
            }
        }
        for op in reply.today_ops {
            let Some(seq) = op.get("seq").and_then(serde_json::Value::as_u64) else { continue };
            outcome.today_seen.push(seq);
            match serde_json::from_value::<crate::remote::TodayOp>(op) {
                Ok(op) => outcome.today_ops.push((seq, op)),
                Err(e) => log::warn!("an iPhone action this Mac does not know: {e}"),
            }
        }
        for call in reply.cli_calls {
            if let Some(id) = call.get("id").and_then(serde_json::Value::as_u64) {
                outcome.calls_seen.push(id);
            }
            match serde_json::from_value::<crate::remote::RemoteCall>(call) {
                Ok(call) => outcome.cli_calls.push(call),
                Err(e) => log::warn!("a glasscode command from the server this Mac cannot read: {e}"),
            }
        }
        // The server's read-only copy of the memory (M20.3; engine v2 only).
        if crate::store::EngineSwitch::current().engine == crate::store::Engine::V2 {
            if let Err(err) = self.send_memory(client, config, &local_only) {
                log::warn!("the server's copy of the memory was not sent: {err:#}");
            }
        }
        Ok(outcome)
    }

    /// Send the server its copy of the memory when memory has a newer head (or what stays on this
    /// Mac changed): without the projects that stay here and the employer folders only they use.
    fn send_memory(&mut self, client: &RemoteClient, config: &ServerConfig, local_only: &HashSet<String>) -> anyhow::Result<()> {
        use crate::store::Store;
        if !client.hello.features.iter().any(|f| f == "memory-copy") {
            return Ok(());
        }
        let Some(head) = crate::memstore::MemoryHead::load().map_err(|e| anyhow!("{e}"))?.commit else { return Ok(()) };
        let memory = crate::memstore::Memory::open();
        let snapshot = memory.snapshot().map_err(|e| anyhow!("{e}"))?;
        let machine = crate::paths::MachineId::this().map_err(|e| anyhow!("{e}"))?;
        let keys = crate::identity::Projects::load().unwrap_or_default();
        let local_only_keys: Vec<crate::paths::ProjectKey> = local_only.iter().filter_map(|id| keys.key_of(&machine, id).cloned()).collect();
        let mut projects: Vec<crate::paths::ProjectKey> = snapshot.files().map_err(|e| anyhow!("{e}"))?.iter().filter_map(|f| f.project().cloned()).collect();
        projects.extend(local_only_keys.iter().cloned());
        projects.sort();
        projects.dedup();
        let employers: Vec<(crate::paths::ProjectKey, Option<String>)> = projects.into_iter().map(|key| {
            let employer = snapshot.employer_of(&key).ok().flatten().map(|e| e.as_str().to_string());
            (key, employer)
        }).collect();
        let excluded = crate::memory_copy::excluded_paths(&local_only_keys, &employers);
        let wanted = (head.clone(), excluded.clone());
        if self.memory_sent.as_ref().is_some_and(|(sent, _)| *sent == wanted) {
            return Ok(());
        }
        // The copy's history goes on from its last commit, kept under a ref of its own here.
        let previous = self.memory_sent.as_ref().map(|(_, copy)| copy.clone()).or_else(|| {
            let out = crate::sys::command("git").arg("-C").arg(memory.dir()).args(["rev-parse", "--verify", "--quiet", crate::memory_copy::SERVER_REF]).output().ok()?;
            out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        });
        let sent = crate::memory_copy::send(client, config, memory.dir(), &head, &excluded, previous.as_deref())?;
        let _ = crate::sys::command("git").arg("-C").arg(memory.dir()).args(["update-ref", crate::memory_copy::SERVER_REF, &sent]).output();
        self.memory_sent = Some((wanted, sent));
        Ok(())
    }

    /// Forget what was pushed (after a reconnect to another server).
    pub fn reset(&mut self) {
        *self = Syncer::default();
    }
}

/// This Mac, for the server: a stable local id made once, and its name.
pub fn identity() -> (String, String) {
    // One id for this machine: M10's (made from this older one when it was there, M57 step 1).
    let device: Option<String> = crate::paths::MachineId::this().ok().map(|id| id.as_str().to_string());
    let id = device.unwrap_or_else(|| {
        let path = crate::paths::root().join("machine-id");
        std::fs::read_to_string(&path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| {
            let id = crate::model::new_id();
            let _ = std::fs::write(&path, &id);
            id
        })
    });
    (id, crate::sys::computer_name())
}

/// The bytes a terminal needs for the keys phones send (tmux key names).
pub fn key_bytes(key: &str) -> Option<&'static [u8]> {
    Some(match key {
        "Escape" => b"\x1b",
        "Enter" => b"\r",
        "Up" => b"\x1b[A",
        "Down" => b"\x1b[B",
        "Right" => b"\x1b[C",
        "Left" => b"\x1b[D",
        "Tab" => b"\t",
        "BTab" => b"\x1b[Z",
        "C-c" => b"\x03",
        "1" => b"1",
        "2" => b"2",
        "3" => b"3",
        "y" => b"y",
        "n" => b"n",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_follows_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| assert!(crate::sys::command("git").arg("-C").arg(dir.path()).args(args).output().unwrap().status.success());
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "one").unwrap();
        git(&["add", "."]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "one"]);
        let clean = fingerprint(dir.path()).unwrap();
        assert_eq!(fingerprint(dir.path()).unwrap(), clean, "nothing changed");
        std::fs::write(dir.path().join("a.txt"), "two").unwrap();
        let edited = fingerprint(dir.path()).unwrap();
        assert_ne!(edited, clean);
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.path().join("a.txt"), "three!").unwrap();
        assert_ne!(fingerprint(dir.path()).unwrap(), edited, "edited again: still \" M\", but it changed");
        std::fs::write(dir.path().join("new.txt"), "x").unwrap();
        let snap = sync::snapshot(dir.path()).unwrap();
        assert_ne!(snap.snap, snap.head, "uncommitted work is in the snapshot");
        assert_eq!(key_bytes("Down"), Some(&b"\x1b[B"[..]));
        assert_eq!(key_bytes("rm -rf"), None);
    }
}
