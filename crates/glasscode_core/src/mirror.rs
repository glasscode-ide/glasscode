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

use anyhow::{Context as _, anyhow, bail};
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
    /// The server's pending actions changed this Mac's store (M57): the cards are read again.
    pub pending_changed: bool,
    /// The server's own changes to work items (M57): the app merges them into its ledger, which
    /// it keeps in memory.
    pub work: Vec<crate::work::WorkItem>,
    /// The server's own changes to the follow-ups (M56.2): merged into the same ledger.
    pub follow_ups: Vec<crate::work::FollowSync>,
    /// How far this Mac's own journal went now, for the app to keep.
    pub journal_sent: Option<crate::journal::Cursor>,
    /// The server's manager's notices, to show.
    pub notices: Vec<crate::remote::ServerNotice>,
    /// What the phones that reach the server said their glasses do (G08).
    pub presence: HashMap<String, serde_json::Value>,
    /// The window ops the server's manager asked for, new to this Mac: the app runs each once.
    pub window_ops: Vec<crate::remote::QueuedWindowOp>,
    /// This Mac's clock went back (crit-9): its changes wait until this time (ms), held here.
    pub clock_back: Option<u64>,
    /// The server's latest stamp, for "use the server's copy".
    pub server_stamp: Option<crate::identity::Hlc>,
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
    /// How far this Mac's own journal went to this server (kept with the journal's cursors).
    pub journal_sent: Option<crate::journal::Cursor>,
    /// What the server's manager needs from this Mac (v2).
    pub manager: Option<crate::remote::ManagerFromMac>,
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
    /// Memory entries (M57): this Mac's commit the server took in, and the server's commit this
    /// Mac took in.
    memory_from: Option<String>,
    memory_have: Option<String>,
    /// The folders that stayed on this Mac at the last memory sync.
    memory_left: Option<Vec<String>>,
    /// What of the v2 stores this Mac sent already (M57): its journal, and its effect records by
    /// the time they last changed.
    journal_sent: Option<crate::journal::Cursor>,
    effects_sent_ms: u64,
    words_sent_ms: u64,
    conversation_sent: crate::conversation::SyncCursor,
    pending_sent_ms: u64,
    work_sent: Option<crate::identity::Hlc>,
    follow_sent: Option<crate::identity::Hlc>,
    /// How far this Mac has the server's records (the last reply's).
    have: crate::remote::HaveCursors,
    /// The server's window ops this Mac has: acknowledged with each sync until the server stops
    /// sending them, and never handed to the app twice.
    window_seen: std::collections::BTreeSet<u64>,
    /// The server's clock at the last sync (ms).
    server_ms: Option<u64>,
}

pub(crate) const CHUNK: usize = 512 * 1024;

fn hash<T: Hash>(value: &T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// HEAD, what `git status` lists, and those files' sizes and times: it changes whenever a
/// snapshot would.
/// What of a "stays on this Mac" project a sync keeps back (M21.1, M57): its project keys, its
/// sessions, and the conversation topics made from them.
#[derive(Default)]
struct KeptBack {
    /// The local-only projects' keys could not be read: everything is kept back (fail closed).
    all: bool,
    projects: HashSet<crate::paths::ProjectKey>,
    sessions: HashSet<String>,
    topics: HashSet<crate::conversation::TopicId>,
    /// Their GitHub repositories (`owner/name`, lower case): a follow-up names its pull request's
    /// repository, not a project.
    repos: HashSet<String>,
    /// The people of memory's employers (M56.2): a teammate named only by an employer whose
    /// projects all stay here stays here too, with their follow-ups.
    people: KeptPeople,
}

/// The `people.md` of memory's employers, lower case: of those whose projects all stay on this
/// Mac (`memory_copy::excluded_paths`), and of the others. `unknown`: that could not be read (the
/// projects' keys, or memory), and no follow-up or teammate goes (fail closed).
#[derive(Default)]
struct KeptPeople {
    unknown: bool,
    kept: Vec<String>,
    shared: Vec<String>,
}

impl KeptPeople {
    /// The people of memory's employers, split by `excluded` (the folders that stay here).
    fn of(memory: &crate::memstore::Memory, excluded: &[String]) -> anyhow::Result<KeptPeople> {
        use crate::memstore::layout::MemFile;
        let snapshot = memory.snapshot().map_err(|e| anyhow!("{e}"))?;
        let files: Vec<MemFile> = snapshot.files().map_err(|e| anyhow!("{e}"))?.into_iter().filter(|f| matches!(f, MemFile::People(_))).collect();
        let texts = snapshot.texts(&files).map_err(|e| anyhow!("{e}"))?;
        let mut people = KeptPeople::default();
        for (file, text) in files.iter().zip(texts) {
            let (MemFile::People(employer), Some(text)) = (file, text) else { continue };
            let side = if excluded.iter().any(|path| *path == format!("org/{}", employer.as_str())) { &mut people.kept } else { &mut people.shared };
            side.push(text.to_lowercase());
        }
        Ok(people)
    }

    /// Whether `who` (a name or a GitHub login) is named only by employers that stay here.
    fn keeps(&self, who: &str) -> bool {
        let who = who.trim().trim_start_matches('@').to_lowercase();
        let named = |texts: &[String]| {
            !who.is_empty()
                && texts.iter().any(|text| {
                    text.match_indices(who.as_str()).any(|(at, _)| {
                        let word = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
                        !text[..at].chars().next_back().is_some_and(word) && !text[at + who.len()..].chars().next().is_some_and(word)
                    })
                })
        };
        named(&self.kept) && !named(&self.shared)
    }
}

impl KeptBack {
    fn of(local: &State, local_only: &HashSet<String>, machine: Option<&crate::paths::MachineId>) -> KeptBack {
        if local_only.is_empty() {
            return KeptBack::default();
        }
        let sessions: HashSet<String> = local.sessions.iter().filter(|s| local_only.contains(&s.project_id)).map(|s| s.id.clone()).collect();
        let keys = {
            use crate::store::Store as _;
            crate::identity::Projects::load()
        };
        let (Some(machine), Ok(keys)) = (machine, keys) else {
            log::warn!("sync: the keys of the projects that stay on this Mac could not be read; the manager's records wait");
            return KeptBack { all: true, sessions, ..KeptBack::default() };
        };
        let projects: HashSet<crate::paths::ProjectKey> = local_only.iter().filter_map(|id| keys.key_of(machine, id).cloned()).collect();
        let topics = projects.iter().map(|key| crate::conversation::Ref::Project(key.clone()).topic_id()).chain(sessions.iter().map(|id| crate::conversation::Ref::Session(id.clone()).topic_id())).collect();
        // (A project with no GitHub repository has no pull requests, so no follow-ups to keep.)
        let repos = local.projects.iter().filter(|p| local_only.contains(&p.id)).filter_map(|p| crate::github::repo_slug(&p.root)).map(|slug| slug.to_lowercase()).collect();
        KeptBack { all: false, projects, sessions, topics, repos, people: KeptPeople::default() }
    }

    /// What the server's follow-ups may know of the teammates (M56.2): each one, except those
    /// whose follow-ups are all of "stays on this Mac" projects (as memory keeps back an employer
    /// whose projects all stay, `memory_copy::excluded_paths`). None (they do not run there) when
    /// that cannot be told: the projects' keys could not be read, or the follow-ups are not read
    /// while some project stays here.
    fn follow_ups_for_server(&self, mut said: crate::remote::FollowUpsFromMac, follow_ups: Option<&[crate::manager::FollowUp]>, none_stay: bool) -> Option<crate::remote::FollowUpsFromMac> {
        if none_stay {
            return Some(said);
        }
        let follow_ups = follow_ups.filter(|_| !self.all && !self.people.unknown)?;
        let theirs = |t: &crate::remote::Teammate, f: &crate::manager::FollowUp| (!t.github.trim().is_empty() && t.github.trim().trim_start_matches('@').eq_ignore_ascii_case(f.login.trim())) || (!t.name.trim().is_empty() && t.name.trim().eq_ignore_ascii_case(f.person.trim()));
        said.people.retain(|t| {
            let (mut kept, mut shared) = (false, false);
            for f in follow_ups.iter().filter(|f| theirs(t, f)) {
                if self.hides_follow_up(f) { kept = true } else { shared = true }
            }
            (shared || !kept) && !self.people.keeps(&t.name) && !self.people.keeps(&t.github)
        });
        Some(said)
    }

    /// A follow-up of a "stays on this Mac" project: by its session, or its pull request's
    /// repository; and one with a teammate named only by an employer that stays here.
    fn hides_follow_up(&self, follow: &crate::manager::FollowUp) -> bool {
        self.all
            || self.people.unknown
            || follow.session.as_deref().is_some_and(|s| self.sessions.contains(s))
            || (!follow.repo.is_empty() && self.repos.contains(&follow.repo.to_lowercase()))
            || self.people.keeps(&follow.person)
            || self.people.keeps(&follow.login)
    }

    fn hides(&self, project: Option<&crate::paths::ProjectKey>, session: Option<&str>) -> bool {
        self.all || project.is_some_and(|p| self.projects.contains(p)) || session.is_some_and(|s| self.sessions.contains(s))
    }

    fn hides_record(&self, record: &crate::conversation::Record) -> bool {
        use crate::conversation::Ref;
        self.all
            || self.topics.contains(record.topic())
            || record.refs().into_iter().any(|r| match r {
                Ref::Project(key) => self.projects.contains(key),
                Ref::Session(id) => self.sessions.contains(id),
                Ref::Pr { .. } => false,
            })
    }
}

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
        let v2 = true /*V2?*/;
        let (journal_events, journal_next) = if v2 {
            let cursor = extras.journal_sent.clone().or_else(|| self.journal_sent.clone()).unwrap_or_else(crate::journal::end);
            let (events, next) = crate::journal::read_from(&cursor, 1_000);
            (events.into_iter().filter(|e| machine.as_ref() == Some(&e.at.machine)).collect(), Some(next))
        } else {
            (Vec::new(), None)
        };
        let sync_started_ms = crate::clock::now_ms();
        // This Mac's clock went back (crit-9): what it stamped would be quarantined, so its stamped
        // records wait here (their cursors stay) until the clocks agree.
        let held = v2 && crate::identity::clock_went_back(self.server_ms).is_some();
        let effects = if v2 { crate::effects::changed_since(self.effects_sent_ms).unwrap_or_default() } else { Vec::new() };
        let words = if v2 && !held { crate::words::own_since(self.words_sent_ms) } else { Vec::new() };
        let words_next = words.last().map(|r| r.at.wall_ms);
        let mut conversation_cursor = self.conversation_sent.clone();
        let conversation = match (v2 && !held, machine.as_ref()) {
            (true, Some(machine)) => crate::conversation::own_changed(&mut conversation_cursor, machine, 2_000),
            _ => Vec::new(),
        };
        // "Stays on this Mac" projects (M21.1): nothing of theirs goes, by project or session.
        let mut kept = KeptBack::of(local, &local_only, machine.as_ref());
        // Nor the people only their employers know (memory's rule), for the follow-ups.
        if v2 && !local_only.is_empty() {
            let memory = crate::memstore::Memory::open();
            kept.people = Self::memory_left_out(&memory, &local_only).and_then(|excluded| KeptPeople::of(&memory, &excluded)).unwrap_or_else(|err| {
                log::warn!("sync: the people of the employers that stay on this Mac could not be read ({err:#}); the follow-ups wait");
                KeptPeople { unknown: true, ..KeptPeople::default() }
            });
        }
        let journal_events: Vec<crate::journal::Event> = journal_events.into_iter().filter(|e| !kept.hides(e.project.as_ref(), e.session.as_deref())).collect();
        let effects: Vec<crate::effects::EffectRecord> = effects.into_iter().filter(|r| !kept.hides(r.project.as_ref(), r.session.as_deref())).collect();
        let words: Vec<crate::words::WordsRecord> = words.into_iter().filter(|r| !kept.hides(r.project.as_ref(), r.session.as_deref())).collect();
        let conversation: Vec<crate::conversation::Record> = conversation.into_iter().filter(|r| !kept.hides_record(r)).collect();
        let ledger = match (v2 && !held, machine.as_ref()) {
            (true, Some(_)) => {
                use crate::store::Store as _;
                crate::work::WorkLedger::load().ok()
            }
            _ => None,
        };
        let work: Vec<crate::work::WorkItem> = match (&ledger, machine.as_ref()) {
            (Some(ledger), Some(machine)) => ledger.own_changed(machine, self.work_sent.as_ref()),
            _ => Vec::new(),
        };
        let work_next = work.last().map(|item| item.updated.clone());
        let work: Vec<crate::work::WorkItem> = work.into_iter().filter(|item| !kept.hides(Some(&item.key.project), None) && !item.sessions.iter().any(|s| kept.hides(None, Some(s)))).collect();
        // The follow-ups (M56.2), by their own stamps; a local-only project's stay here.
        let follow_ups: Vec<crate::work::FollowSync> = match (&ledger, machine.as_ref()) {
            (Some(ledger), Some(machine)) => ledger.follow_changed(machine, self.follow_sent.as_ref()),
            _ => Vec::new(),
        };
        let follow_next = follow_ups.last().map(|f| f.stamp.clone());
        let follow_ups: Vec<crate::work::FollowSync> = follow_ups.into_iter().filter(|f| !kept.hides_follow_up(&f.follow)).collect();
        // Nor the teammates of a "stays on this Mac" project (as memory keeps back an employer
        // whose projects all stay): what the server's follow-ups need, without them.
        let mut manager = extras.manager;
        if let Some(said) = manager.as_mut() {
            said.follow_ups = said.follow_ups.take().and_then(|follow| kept.follow_ups_for_server(follow, ledger.as_ref().map(|l| l.follow_ups.as_slice()), local_only.is_empty()));
        }
        let pending: Vec<crate::gate::PendingAction> = if v2 {
            use crate::store::Store as _;
            crate::gate::Pending::load()
                .map(|p| p.changed_since(self.pending_sent_ms))
                .unwrap_or_default()
                .into_iter()
                .filter(|a| !kept.hides(a.project.as_ref(), a.session.as_deref()) && !a.targets.projects.iter().any(|p| kept.hides(Some(p), None)) && !a.targets.sessions.iter().any(|s| kept.hides(None, Some(s))))
                .collect()
        } else {
            Vec::new()
        };
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
            words,
            conversation,
            pending,
            work,
            follow_ups,
            have: self.have.clone(),
            engine: Some(crate::store::EngineSwitch::current()),
            manager,
            window_acks: self.window_seen.iter().copied().collect(),
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
            // Sent: the next sync goes on from here (unless the records waited: then nothing went).
            // A kind the server could not take in goes again.
            let taken = |kind: &str| !kept.all && !reply.not_taken.iter().any(|k| k == kind);
            if taken("journal") {
                if let Some(next) = journal_next {
                    self.journal_sent = Some(next.clone());
                    outcome.journal_sent = Some(next);
                }
            }
            if taken("effects") {
                self.effects_sent_ms = sync_started_ms;
            }
            if taken("words") {
                if let Some(next) = words_next {
                    self.words_sent_ms = next;
                }
            }
            if taken("conversation") {
                self.conversation_sent = conversation_cursor;
            }
            if taken("pending") {
                self.pending_sent_ms = sync_started_ms;
            }
            if taken("work") {
                if let Some(next) = work_next {
                    self.work_sent = Some(next);
                }
            }
            if taken("follow_ups") && !kept.people.unknown {
                if let Some(next) = follow_next {
                    self.follow_sent = Some(next);
                }
            }
            // The server's changes come in. How far this Mac has them: where the reply stopped,
            // except for a kind it could not take in, which comes again.
            let mut have = reply.have.clone();
            outcome.work = reply.work;
            outcome.follow_ups = reply.follow_ups;
            outcome.notices = reply.notices;
            outcome.presence = reply.presence;
            // Told against the server's clock as well as this one.
            self.server_ms = reply.server_stamp.as_ref().map(|stamp| stamp.wall_ms);
            outcome.clock_back = crate::identity::clock_went_back(self.server_ms);
            outcome.server_stamp = reply.server_stamp.clone();
            // The server sends a window op until it is acknowledged: what it still sends is kept
            // in the acks; only a new one goes to the app.
            let sent: std::collections::BTreeSet<u64> = reply.window_ops.iter().map(|q| q.id).collect();
            for queued in reply.window_ops {
                if self.window_seen.insert(queued.id) {
                    outcome.window_ops.push(queued);
                }
            }
            self.window_seen.retain(|id| sent.contains(id) || outcome.window_ops.iter().any(|q| q.id == *id));
            // The server's asks come as cards here (answered here, run there); nothing it says
            // answers or adds an ask of this Mac's.
            if let (false, Some(server), Some(here)) = (reply.pending.is_empty(), client.hello.machine.as_ref(), machine.as_ref()) {
                let now = crate::clock::now_ms();
                match crate::store::update::<crate::gate::Pending, usize>(|p| p.merge_theirs(&reply.pending, server, here, false, now)) {
                    Ok(0) => {}
                    Ok(_) => outcome.pending_changed = true,
                    Err(err) => {
                        log::warn!("pending: the server's actions could not be merged: {err}");
                        have.pending_ms = self.have.pending_ms;
                    }
                }
            }
            match crate::conversation::take_from_other_home(reply.conversation) {
                Ok(0) => {}
                Ok(count) => log::info!("conversation: {count} changes from the server"),
                Err(err) => {
                    log::warn!("conversation: the server's changes could not be taken: {err}");
                    have.conversation = self.have.conversation.clone();
                }
            }
            // The server's words, never tier one from there (`words::from_server`).
            let theirs: Vec<crate::words::WordsRecord> = reply.words.into_iter().filter_map(crate::words::from_server).collect();
            if let (false, Some(server)) = (theirs.is_empty(), client.hello.machine.as_ref()) {
                if let Err(err) = crate::words::take_in(server, &theirs) {
                    log::warn!("words: the server's records could not be kept: {err}");
                    have.words_ms = self.have.words_ms;
                }
            }
            if !reply.effects.is_empty() {
                if let Err(err) = crate::effects::merge_theirs(&reply.effects) {
                    log::warn!("effects: the server's records could not be merged: {err}");
                    have.effects_ms = self.have.effects_ms;
                }
            }
            self.have = have;
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
        // Memory entries both ways (M57), or the server's read-only copy for an older server
        // (M20.3); engine v2 only.
        if client.hello.features.iter().any(|f| f == "memory-sync") {
            if let Err(err) = self.sync_memory(client, &local_only) {
                log::warn!("memory: the entries were not synced: {err:#}");
            }
        } else if let Err(err) = self.send_memory(client, config, &local_only) {
            log::warn!("the server's copy of the memory was not sent: {err:#}");
        }
        Ok(outcome)
    }

    /// The folders of memory that stay on this Mac: each local-only project's, and each employer's
    /// whose projects all are. It fails (nothing goes) when the projects' keys cannot be read.
    fn memory_left_out(memory: &crate::memstore::Memory, local_only: &HashSet<String>) -> anyhow::Result<Vec<String>> {
        use crate::store::Store;
        let snapshot = memory.snapshot().map_err(|e| anyhow!("{e}"))?;
        let machine = crate::paths::MachineId::this().map_err(|e| anyhow!("{e}"))?;
        let keys = if local_only.is_empty() { crate::identity::Projects::load().unwrap_or_default() } else { crate::identity::Projects::load().map_err(|e| anyhow!("the projects' keys could not be read, so memory waits: {e}"))? };
        let local_only_keys: Vec<crate::paths::ProjectKey> = local_only.iter().filter_map(|id| keys.key_of(&machine, id).cloned()).collect();
        if local_only_keys.len() < local_only.len() {
            bail!("a project that stays on this Mac has no key yet, so memory waits");
        }
        let mut projects: Vec<crate::paths::ProjectKey> = snapshot.files().map_err(|e| anyhow!("{e}"))?.iter().filter_map(|f| f.project().cloned()).collect();
        projects.extend(local_only_keys.iter().cloned());
        projects.sort();
        projects.dedup();
        let employers: Vec<(crate::paths::ProjectKey, Option<String>)> = projects.into_iter().map(|key| {
            let employer = snapshot.employer_of(&key).ok().flatten().map(|e| e.as_str().to_string());
            (key, employer)
        }).collect();
        Ok(crate::memory_copy::excluded_paths(&local_only_keys, &employers))
    }

    /// Memory entries both ways (M57): this Mac's files that changed since the commit the server
    /// took, without what stays here; the server's that changed since the commit this Mac took,
    /// merged here with no authority. A file with an edit that waits for the person comes again.
    fn sync_memory(&mut self, client: &RemoteClient, local_only: &HashSet<String>) -> anyhow::Result<()> {
        use crate::memstore::sync::{LeftOut, SyncedFile, outgoing};
        // This Mac's clock went back (crit-9): its entries wait here; the server's still come.
        let held = crate::identity::clock_went_back(self.server_ms).is_some();
        let memory = crate::memstore::Memory::open();
        let paths = Self::memory_left_out(&memory, local_only)?;
        // What stays here changed: everything goes again (a project open now is sent), and the
        // server lets the folders of one that stays here now go.
        let mut drop: Vec<String> = Vec::new();
        if let Some(before) = self.memory_left.as_ref().filter(|before| **before != paths) {
            drop = paths.iter().filter(|p| !before.contains(p)).cloned().collect();
            self.memory_from = None;
        }
        // What this Mac forgot, the server forgets too (M25): its history restarts, and a forgotten
        // project's or employer's folders go.
        let forget = {
            use crate::store::Store as _;
            crate::memstore::sync::ServerForget::load().unwrap_or_default()
        };
        for folder in &forget.drop {
            if !drop.contains(folder) {
                drop.push(folder.clone());
            }
        }
        let left = LeftOut::of(paths.clone());
        let machine = crate::paths::MachineId::this().map_err(|e| anyhow!("{e}"))?;
        let (files, head) = if held { (Vec::new(), self.memory_from.clone()) } else { outgoing(&memory, self.memory_from.as_deref(), &left, &machine).map_err(|e| anyhow!("{e}"))? };
        let reply: serde_json::Value = client.call("memory.sync", memory_sync_request(&files, self.memory_have.as_deref(), &drop, &forget, held))?;
        if !forget.is_empty() {
            // Only what the server says it did is cleared: an older server asked again next time.
            let done: crate::memstore::sync::Forgotten = reply.get("forgot").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
            if let Err(err) = crate::store::update::<crate::memstore::sync::ServerForget, _>(|request| request.done(&forget, &done)) {
                log::warn!("memory: the server forgot, but this Mac could not note it: {err}");
            }
            if forget.restart && !done.restart {
                log::info!("memory: the server did not say it restarted its history; it is asked again at the next sync");
            }
            // The server's history restarted: the commit this Mac had of it is in no history now.
            if done.restart {
                self.memory_have = None;
            }
        }
        self.memory_from = head;
        self.memory_left = Some(paths);
        let theirs: Vec<SyncedFile> = reply.get("files").cloned().map(serde_json::from_value).transpose()?.unwrap_or_default();
        let taken = memory.take_synced(&theirs, true, &left).map_err(|e| anyhow!("{e}"))?;
        if !taken.skipped.is_empty() {
            log::info!("memory: {} files wait for your edits to be accepted or discarded; they sync after", taken.skipped.len());
        } else {
            self.memory_have = reply.get("head").and_then(serde_json::Value::as_str).map(String::from);
        }
        Ok(())
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
        let excluded = Self::memory_left_out(&memory, local_only)?;
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

/// The `memory.sync` request (M57). The server restarts its history only when this Mac's files
/// go with the request: while this Mac's clock went back (`held`) they wait, and a history
/// restarted before the tombstones of what was forgotten arrive keeps the forgotten text in its
/// fresh commit. The restart stays asked for until a sync sends it with the files.
fn memory_sync_request(files: &[crate::memstore::sync::SyncedFile], have: Option<&str>, drop: &[String], forget: &crate::memstore::sync::ServerForget, held: bool) -> serde_json::Value {
    serde_json::json!({ "files": files, "have": have, "drop": drop, "forget": { "restart": forget.restart && !held, "words": forget.words } })
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
    fn a_local_only_projects_follow_ups_stay_here() {
        use crate::manager::FollowUp;
        let kept = KeptBack { sessions: HashSet::from(["s-local".to_string()]), repos: HashSet::from(["acme/secret".to_string()]), ..KeptBack::default() };
        let follow = |repo: &str, session: Option<&str>| FollowUp { repo: repo.into(), session: session.map(str::to_string), ..FollowUp::default() };
        assert!(kept.hides_follow_up(&follow("Acme/Secret", None)), "by its repository, in any case");
        assert!(kept.hides_follow_up(&follow("acme/app", Some("s-local"))), "by its session");
        assert!(!kept.hides_follow_up(&follow("acme/app", Some("s-other"))));
        assert!(!kept.hides_follow_up(&follow("", None)), "a follow-up of the person's own words names no repository");
        assert!(KeptBack { all: true, ..KeptBack::default() }.hides_follow_up(&follow("acme/app", None)), "fails closed");
    }

    /// M56.2, as memory's privacy test (`memory_copy`): with the real memory writer, a teammate
    /// named only in the people of an employer whose projects all stay on this Mac (Pat, of
    /// acmecorp) is not sent to the server, nor a follow-up of theirs, even on an open repository;
    /// one named by an open employer too goes; a damaged memory sends none (fail closed).
    #[test]
    fn a_local_only_employers_people_stay_here() {
        use crate::manager::FollowUp;
        use crate::memstore::Memory;
        use crate::memstore::git::Who;
        use crate::memstore::layout::{Employer, MemFile};
        use crate::memstore::tests_support::{Scratch, chat, key, private_memory};
        use crate::remote::{FollowUpsFromMac, Teammate};
        let _scratch = Scratch::new();
        let memory = Memory::open();
        let private = private_memory(&memory);
        // An open employer that also knows Robin, as acmecorp does.
        let globex = Employer::parse("globex").unwrap();
        memory.set_employer(&Who::You, &key("open-222222"), &globex).unwrap();
        memory.remember(&chat("Remember: Robin-dev reviews the API."), "Robin-dev reviews the API.", "Robin-dev reviews the API.", &MemFile::People(globex), None).unwrap();
        memory.remember(&chat("Remember: Robin-dev pairs with Pat."), "Robin-dev pairs with Pat.", "Robin-dev pairs with Pat.", &MemFile::People(Employer::parse("acmecorp").unwrap()), None).unwrap();
        let people = KeptPeople::of(&memory, &private.excluded).unwrap();
        let kept = KeptBack { repos: HashSet::from(["acme/secret".to_string()]), people, ..KeptBack::default() };
        assert!(kept.people.keeps("Pat") && kept.people.keeps("pat"), "named only by acmecorp");
        assert!(!kept.people.keeps("Robin-dev"), "named by globex too");
        assert!(!kept.people.keeps("Pa") && !kept.people.keeps("Robin"), "a whole word only");
        let mate = |name: &str, github: &str| Teammate { name: name.into(), github: github.into() };
        let said = FollowUpsFromMac { me: Some("me".into()), people: vec![mate("Pat", "pat-gh"), mate("Robin", "robin-dev"), mate("Lee", "lee")] };
        let open = |login: &str, person: &str| FollowUp { repo: "acme/app".into(), login: login.into(), person: person.into(), ..FollowUp::default() };
        let follow_ups = [open("pat-gh", "Pat"), open("robin-dev", "Robin")];
        let names: Vec<String> = kept.follow_ups_for_server(said.clone(), Some(&follow_ups), false).unwrap().people.into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["Robin", "Lee"]);
        assert!(kept.hides_follow_up(&follow_ups[0]), "Pat's follow-up on an open repository stays too");
        assert!(!kept.hides_follow_up(&follow_ups[1]));
        // Memory cannot be read: nothing of the follow-ups goes.
        let unknown = KeptBack { people: KeptPeople { unknown: true, ..KeptPeople::default() }, ..KeptBack::default() };
        assert!(unknown.hides_follow_up(&follow_ups[1]));
        assert_eq!(unknown.follow_ups_for_server(said, Some(&follow_ups), false), None);
    }

    /// A teammate the server's follow-ups know of: not one whose follow-ups are all of projects
    /// that stay here; with nothing staying, all of them; fail closed when it cannot be told.
    #[test]
    fn a_local_only_projects_teammates_stay_here() {
        use crate::manager::FollowUp;
        use crate::remote::{FollowUpsFromMac, Teammate};
        let kept = KeptBack { repos: HashSet::from(["acme/secret".to_string()]), ..KeptBack::default() };
        let mate = |name: &str, github: &str| Teammate { name: name.into(), github: github.into() };
        let said = FollowUpsFromMac { me: Some("robin".into()), people: vec![mate("Sam", "sam-dev"), mate("Kim", "kim"), mate("Lee", "lee"), mate("Ana", "")] };
        let follow = |repo: &str, login: &str, person: &str| FollowUp { repo: repo.into(), login: login.into(), person: person.into(), ..FollowUp::default() };
        let follow_ups = [follow("acme/secret", "Sam-Dev", ""), follow("acme/secret", "kim", ""), follow("acme/app", "kim", ""), follow("acme/secret", "", "Ana")];
        let names = |s: Option<FollowUpsFromMac>| s.map(|s| s.people.into_iter().map(|t| t.name).collect::<Vec<_>>());
        assert_eq!(names(kept.follow_ups_for_server(said.clone(), Some(&follow_ups), false)), Some(vec!["Kim".to_string(), "Lee".to_string()]), "Sam and Ana only on the secret project; Kim on both; Lee on none");
        assert_eq!(names(kept.follow_ups_for_server(said.clone(), None, true)).map(|n| n.len()), Some(4), "nothing stays here");
        assert_eq!(kept.follow_ups_for_server(said.clone(), None, false), None, "the follow-ups not read: they do not run there");
        assert_eq!(KeptBack { all: true, ..KeptBack::default() }.follow_ups_for_server(said, Some(&follow_ups), false), None, "fails closed");
    }

    /// mem-13 with M57: a forgotten secret's restart of the server's history goes only with this
    /// Mac's files, which carry its tombstones; while the clock went back (the files wait), it
    /// waits too.
    #[test]
    fn the_servers_history_restarts_only_with_the_tombstones() {
        let forget = crate::memstore::sync::ServerForget { restart: true, drop: Vec::new(), words: vec!["w1".into()] };
        let held = memory_sync_request(&[], None, &[], &forget, true);
        assert_eq!(held["forget"]["restart"], false, "{held}");
        assert_eq!(held["forget"]["words"], serde_json::json!(["w1"]));
        let file = crate::memstore::sync::SyncedFile { rel: "you/profile.md".into(), text: "<!-- forgotten id=0badc0de at=1.0@0123456789abcdef -->\n".into() };
        let sent = memory_sync_request(std::slice::from_ref(&file), Some("abc"), &[], &forget, false);
        assert_eq!(sent["forget"]["restart"], true);
        assert_eq!(sent["files"][0]["rel"], "you/profile.md");
        // A restart the server was not asked for (or did not do) is asked again at the next sync.
        let mut asked = forget.clone();
        asked.done(&forget, &crate::memstore::sync::Forgotten { restart: false, words: true });
        assert!(asked.restart, "a restart the server did not do is asked again");
    }

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
