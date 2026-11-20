//! The Server mode switch's moves (the server-sync plan's phase 2): a worktree moves as one unit
//! with every open session in it, never split between the Mac and the server, under the owner
//! record glasscoded keeps for it (`owners`).
//!
//! A move to the server: `owners.begin` → one push and one `worktrees.receive` for the worktree →
//! for each session its conversation and a held `sessions.create` (its agent waits) →
//! `owners.commit`, which starts them there. Anything failing before the commit sends
//! `owners.abort`, and the unit stays on the Mac. The way back is the same in reverse; there the
//! caller starts the sessions here, then closes the server's copies ([`close_on_server`]).
//!
//! These run on the Mac, in both directions: they need its disk. They are the ops `switch.units`,
//! `switch.check`, `switch.move`, `worktree.move` and `worktree.owner` of the plan, as functions
//! until the manager's registry lands.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agents::AgentActivity;
use crate::model::{AgentKind, Session, State};
use crate::owners::{OwnerRecord, WorktreeKey};
use crate::paths::MachineId;
use crate::remote::{
    MoveTag, OwnersBegin, OwnersReply, RemoteClient, RemoteSession, ServerConfig, ServerState,
};
use crate::sync;

/// Which way a move goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    ToServer,
    ToMac,
}

/// One worktree and every open session in it, on the machine it leaves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    pub key: WorktreeKey,
    pub project_id: String,
    pub worktree: PathBuf,
    /// Session ids on the machine it leaves (none: a worktree moved because it was ticked).
    pub sessions: Vec<String>,
    pub titles: Vec<String>,
    /// A project's main checkout: never a unit with an owner record. Its sessions move one at a
    /// time, as before worktrees moved as units, so the project folder works as it always did.
    #[serde(default)]
    pub main: bool,
}

/// Whether `worktree` is the project's main checkout (links resolved).
pub fn is_main_checkout(project_root: &Path, worktree: &Path) -> bool {
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    real(project_root) == real(worktree)
}

/// A move under way, kept on this Mac's disk (`State::worktrees_moving`), so that a lost reply
/// or a restart finishes it from the owner record instead of leaving it half done.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveNote {
    pub direction: Direction,
    pub unit: Unit,
}

/// What the owner record says about a move to `epoch` towards `target`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    /// The target owns the worktree at that epoch: the move happened.
    Moved,
    /// Still under way at that epoch: commit it again, or give it up.
    Moving,
    /// It did not happen (given up, or replaced).
    NotMoved,
}

/// `worktree.owner` read after a lost reply: did the move to `epoch` happen?
pub fn settle(client: &RemoteClient, key: &WorktreeKey, epoch: u64, target: &MachineId, holder: &MachineId) -> anyhow::Result<Settled> {
    let record = owner(client, key, holder)?;
    Ok(settled(&record, epoch, target))
}

/// The rule of [`settle`], on a record already read.
pub fn settled(record: &OwnerRecord, epoch: u64, target: &MachineId) -> Settled {
    if &record.owner == target && record.epoch == epoch && record.moving.is_none() {
        Settled::Moved
    } else if record.moving.as_ref().is_some_and(|m| m.epoch == epoch && &m.to == target) {
        Settled::Moving
    } else {
        Settled::NotMoved
    }
}

/// The open sessions here, grouped by worktree: the units "Server mode" moves.
pub fn units_to_server(local: &State) -> Vec<Unit> {
    let home = crate::paths::home();
    let mut by_path: BTreeMap<PathBuf, Unit> = BTreeMap::new();
    for session in &local.sessions {
        let Some(project) = local.project(&session.project_id) else {
            continue;
        };
        let unit = by_path
            .entry(session.worktree.clone())
            .or_insert_with(|| Unit {
                key: WorktreeKey::of(
                    &session.worktree,
                    &home,
                    &project.name,
                    Some(&session.branch),
                ),
                project_id: project.id.clone(),
                worktree: session.worktree.clone(),
                sessions: Vec::new(),
                titles: Vec::new(),
                main: is_main_checkout(&project.root, &session.worktree),
            });
        unit.sessions.push(session.id.clone());
        unit.titles.push(session.title.clone());
    }
    by_path.into_values().collect()
}

/// The server's sessions, grouped by worktree: the units "Laptop mode" brings back.
pub fn units_to_mac(server: &ServerState, server_home: &Path) -> Vec<Unit> {
    let mut by_path: BTreeMap<PathBuf, Unit> = BTreeMap::new();
    for remote in &server.sessions {
        let session = &remote.session;
        let Some(project) = server.projects.iter().find(|p| p.id == session.project_id) else {
            continue;
        };
        let unit = by_path
            .entry(session.worktree.clone())
            .or_insert_with(|| Unit {
                key: WorktreeKey::of(
                    &session.worktree,
                    server_home,
                    &project.name,
                    Some(&session.branch),
                ),
                project_id: project.id.clone(),
                worktree: session.worktree.clone(),
                sessions: Vec::new(),
                titles: Vec::new(),
                // Compared as paths on the server (its folders are not here to resolve).
                main: session.worktree == project.root,
            });
        unit.sessions.push(session.id.clone());
        unit.titles.push(session.title.clone());
    }
    by_path.into_values().collect()
}

/// The worktrees here with no open session (not a project's main checkout): the switch lists
/// the ones with uncommitted work, unticked. Ticked, one moves as a unit with no session;
/// unticked, its copy is parked on the server ([`park_on_server`]).
pub fn idle_worktrees(local: &State, worktrees: &std::collections::HashMap<String, Vec<crate::git::Worktree>>) -> Vec<Unit> {
    let home = crate::paths::home();
    // git names a worktree with its links resolved; a session may not.
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let busy: std::collections::HashSet<PathBuf> = local.sessions.iter().map(|s| real(&s.worktree)).collect();
    let mut units = Vec::new();
    for project in &local.projects {
        for worktree in worktrees.get(&project.id).into_iter().flatten() {
            if worktree.is_main || worktree.prunable || !worktree.path.is_dir() || busy.contains(&real(&worktree.path)) {
                continue;
            }
            let branch = (worktree.branch != "detached").then_some(worktree.branch.as_str());
            units.push(Unit {
                key: WorktreeKey::of(&worktree.path, &home, &project.name, branch),
                project_id: project.id.clone(),
                worktree: worktree.path.clone(),
                sessions: Vec::new(),
                titles: Vec::new(),
                main: false,
            });
        }
    }
    units
}

/// Leave a copy of a worktree with no session on the server, without moving it: its HEAD and
/// uncommitted snapshot go to `refs/glasscode/parked/<token>`, so it can be opened there while
/// this Mac sleeps (`worktrees.open_parked`, which moves it).
pub fn park_on_server(client: &RemoteClient, config: &ServerConfig, local: &State, unit: &Unit, mac: &MachineId) -> anyhow::Result<()> {
    let project = local.project(&unit.project_id).ok_or_else(|| anyhow!("no project for {}", unit.worktree.display()))?;
    let state: ServerState = client.call("state", json!({}))?;
    let server_project = sync::server_project(client, &state, project, &crate::paths::home(), &client.hello.home)?;
    let snap = sync::snapshot(&unit.worktree)?;
    let token = format!("p{}", unit.key.token());
    let mut push = sync::git_cmd(&unit.worktree);
    push.env("GIT_SSH_COMMAND", config.git_ssh_command()).args([
        "push",
        "--quiet",
        "--force",
        "--no-verify",
        &config.git_url(&server_project.root),
        &format!("{}:refs/glasscode/parked/{token}/head", snap.head),
        &format!("{}:refs/glasscode/parked/{token}/snap", snap.snap),
    ]);
    sync::run(push)?;
    let parked = crate::remote::ParkedWorktree { key: unit.key.clone(), project_id: server_project.id, branch: snap.branch, token, machine: Some(mac.clone()), at: crate::model::now() };
    client.call::<Value>("worktrees.park", serde_json::to_value(&parked)?)?;
    Ok(())
}

/// The unit a session belongs to, with every other session in its worktree.
pub fn unit_of(units: &[Unit], session: &str) -> Option<Unit> {
    units
        .iter()
        .find(|u| u.sessions.iter().any(|s| s == session))
        .cloned()
}

/// Something that stops a unit from moving, or that the person should know first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Problem {
    /// The server's glasscoded is too old for moves by worktree.
    OldServer,
    /// The project is not on the server and has no origin to clone it from.
    NoProject { project: String },
    /// A session's account has no login on the other machine.
    NotSignedIn { session: String, account: String },
    /// No account for a session's agent on the other machine.
    NoAccount { session: String, agent: String },
    /// A session's agent is working: it moves once the turn ends (or with "Move now").
    Working { session: String },
    /// A Shell session moves as a fresh shell in the same folder; what runs in it stops.
    ShellRestarts { session: String },
}

impl Problem {
    /// Whether the unit cannot move until it is fixed (the others are worth knowing).
    pub fn blocks(&self) -> bool {
        // An older server still takes sessions one at a time, without an owner record.
        matches!(self, Problem::NoProject { .. } | Problem::NotSignedIn { .. } | Problem::NoAccount { .. })
    }

    pub fn describe(&self, title: impl Fn(&str) -> String) -> String {
        match self {
            Problem::OldServer => "the server's glasscoded is old: its sessions move one at a time, without an owner record; update it".into(),
            Problem::NoProject { project } => {
                format!("{project} is not on the server and has no origin remote to clone")
            }
            Problem::NotSignedIn { session, account } => format!(
                "{}: {account} is not signed in on the other machine",
                title(session)
            ),
            Problem::NoAccount { session, agent } => format!(
                "{}: no {agent} account on the other machine",
                title(session)
            ),
            Problem::Working { session } => {
                format!("{} is working; it moves when its turn ends", title(session))
            }
            Problem::ShellRestarts { session } => format!(
                "{} is a shell: it starts fresh there, and what runs in it stops",
                title(session)
            ),
        }
    }
}

/// What a session's agent is doing now, for the check (the caller knows; here, `None` is idle).
pub type ActivityOf<'a> = &'a dyn Fn(&str) -> Option<AgentActivity>;

/// The check before a move to the server: what stops each unit, read from what this Mac and
/// the server already know (no change anywhere).
pub fn check_to_server(
    local: &State,
    server: &ServerState,
    hello_features: &[String],
    unit: &Unit,
    activity: ActivityOf,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    if !hello_features.iter().any(|f| f == "owners") {
        problems.push(Problem::OldServer);
    }
    if let Some(project) = local.project(&unit.project_id) {
        let known = server
            .projects
            .iter()
            .any(|p| p.name == project.name && p.root.file_name() == project.root.file_name());
        if !known && sync::origin_url(&project.root).is_none() {
            problems.push(Problem::NoProject {
                project: project.name.clone(),
            });
        }
    }
    for id in &unit.sessions {
        let Some(session) = local.session(id) else {
            continue;
        };
        account_problem(
            &mut problems,
            session,
            local.account(session.account_id.as_deref().unwrap_or_default()),
            &server.accounts,
            &server.defaults,
            &server.signed_in,
        );
        push_activity(&mut problems, session, activity(id));
    }
    problems
}

/// The check before bringing a unit back to this Mac.
pub fn check_to_mac(local: &State, server: &ServerState, unit: &Unit) -> Vec<Problem> {
    let mut problems = Vec::new();
    for remote in server
        .sessions
        .iter()
        .filter(|r| unit.sessions.contains(&r.session.id))
    {
        let session = &remote.session;
        let account = session
            .account_id
            .as_deref()
            .and_then(|id| server.accounts.iter().find(|a| a.id == id));
        if session.agent.uses_accounts()
            && sync::map_account(account, session.agent, &local.accounts, &local.defaults).is_none()
        {
            problems.push(Problem::NoAccount {
                session: session.id.clone(),
                agent: session.agent.name().into(),
            });
        }
        push_activity(&mut problems, session, remote.activity);
    }
    problems
}

fn account_problem(
    problems: &mut Vec<Problem>,
    session: &Session,
    account: Option<&crate::model::Account>,
    to_accounts: &[crate::model::Account],
    to_defaults: &crate::model::Defaults,
    signed_in: &std::collections::HashMap<String, bool>,
) {
    if !session.agent.uses_accounts() {
        return;
    }
    match sync::map_account(account, session.agent, to_accounts, to_defaults) {
        None => problems.push(Problem::NoAccount {
            session: session.id.clone(),
            agent: session.agent.name().into(),
        }),
        Some(id) if signed_in.get(&id) == Some(&false) => {
            let label = to_accounts
                .iter()
                .find(|a| a.id == id)
                .map(|a| a.label.clone())
                .unwrap_or(id);
            problems.push(Problem::NotSignedIn {
                session: session.id.clone(),
                account: label,
            });
        }
        Some(_) => {}
    }
}

fn push_activity(problems: &mut Vec<Problem>, session: &Session, activity: Option<AgentActivity>) {
    if matches!(activity, Some(AgentActivity::Working)) {
        problems.push(Problem::Working {
            session: session.id.clone(),
        });
    }
    if session.agent == AgentKind::Shell {
        problems.push(Problem::ShellRestarts {
            session: session.id.clone(),
        });
    }
}

/// `worktree.owner`: the record of a worktree, or the one it has without a record (owned by
/// `holder`, where it is, at epoch 0).
pub fn owner(
    client: &RemoteClient,
    key: &WorktreeKey,
    holder: &MachineId,
) -> anyhow::Result<OwnerRecord> {
    let records: std::collections::HashMap<WorktreeKey, OwnerRecord> =
        client.call("owners.get", json!({ "keys": [key] }))?;
    Ok(records.get(key).cloned().unwrap_or(OwnerRecord {
        owner: holder.clone(),
        epoch: 0,
        moving: None,
    }))
}

fn owners_call(client: &RemoteClient, method: &str, params: Value) -> anyhow::Result<(OwnerRecord, Vec<String>)> {
    match client.call::<OwnersReply>(method, params)? {
        OwnersReply::Ok { record, not_started } => Ok((record, not_started)),
        OwnersReply::Refused { error } => Err(anyhow!("{error}")),
    }
}

fn server_machine(client: &RemoteClient) -> anyhow::Result<MachineId> {
    match (
        &client.hello.machine,
        client.hello.features.iter().any(|f| f == "owners"),
    ) {
        (Some(id), true) => Ok(id.clone()),
        _ => bail!("the server's glasscoded is too old to move worktrees safely; update it"),
    }
}

/// A unit moved to the server: the epoch it has there, and each session as it continues there.
#[derive(Clone, Debug)]
pub struct MovedToServer {
    pub key: WorktreeKey,
    pub epoch: u64,
    /// (the session here, the one continuing it there)
    pub sessions: Vec<(String, RemoteSession)>,
    /// Sessions that moved but whose agent did not start there (`owners.commit` says why in
    /// glasscoded's log): they start when opened there.
    pub not_started: Vec<String>,
}

/// Move one unit to the server (`worktree.move --to-server`). Its agents here must be stopped or
/// idle first, and the caller closes its sessions here when this returns. On an error the unit
/// stays here, at its old epoch.
pub fn move_unit_to_server(client: &RemoteClient, config: &ServerConfig, local: &State, unit: &Unit, mac: &MachineId) -> anyhow::Result<MovedToServer> {
    if unit.main {
        bail!("a project's main checkout moves session by session");
    }
    let server = server_machine(client)?;
    let seen = owner(client, &unit.key, mac)?;
    if seen.owner != *mac {
        bail!("this worktree already runs on the server");
    }
    // A lost reply to begin: trying again begins the same move (the server allows it).
    let begun = owners_call(client, "owners.begin", serde_json::to_value(OwnersBegin { key: unit.key.clone(), from: mac.clone(), to: server.clone(), epoch: seen.epoch })?)?;
    let epoch = begun.0.moving.as_ref().map_or(seen.epoch + 1, |m| m.epoch);
    let result = send_unit(client, config, local, unit, epoch).and_then(|sessions| commit_or_settle(client, &unit.key, epoch, &server, mac).map(|not_started| (sessions, not_started)));
    match result {
        Ok((sessions, not_started)) => Ok(MovedToServer { key: unit.key.clone(), epoch, sessions, not_started }),
        Err(err) => {
            give_up(client, &unit.key, epoch);
            Err(err)
        }
    }
}

/// Commit a move; a lost reply is settled by the owner record: moved is done, still under way
/// commits once more, anything else fails. Returns the sessions that did not start there.
fn commit_or_settle(client: &RemoteClient, key: &WorktreeKey, epoch: u64, target: &MachineId, holder: &MachineId) -> anyhow::Result<Vec<String>> {
    let commit = || owners_call(client, "owners.commit", json!({ "key": key, "epoch": epoch })).map(|(_, not_started)| not_started);
    match commit() {
        Ok(not_started) => Ok(not_started),
        Err(err) => match settle(client, key, epoch, target, holder) {
            Ok(Settled::Moved) => Ok(Vec::new()),
            Ok(Settled::Moving) => commit(),
            _ => Err(err),
        },
    }
}

/// A move that failed: given up, unless it committed meanwhile (then the abort changes nothing).
fn give_up(client: &RemoteClient, key: &WorktreeKey, epoch: u64) {
    let _ = client.call::<Value>("owners.abort", json!({ "key": key, "epoch": epoch }));
}

fn send_unit(
    client: &RemoteClient,
    config: &ServerConfig,
    local: &State,
    unit: &Unit,
    epoch: u64,
) -> anyhow::Result<Vec<(String, RemoteSession)>> {
    let project = local
        .project(&unit.project_id)
        .ok_or_else(|| anyhow!("no project for {}", unit.worktree.display()))?;
    let state: ServerState = client.call("state", json!({}))?;
    let server_project = sync::server_project(
        client,
        &state,
        project,
        &crate::paths::home(),
        &client.hello.home,
    )?;
    let token = format!("u{}e{epoch}", unit.key.token());
    let server_worktree = sync::send_worktree(
        client,
        config,
        project,
        &server_project,
        &unit.worktree,
        &token,
        Some((unit.key.clone(), epoch)),
    )?;
    let mut moved = Vec::new();
    for id in &unit.sessions {
        let session = local
            .session(id)
            .ok_or_else(|| anyhow!("session {id} is gone"))?;
        let (account_id, resume) =
            sync::send_conversation(client, config, local, &state, session, &server_worktree)?;
        let held = MoveTag {
            key: unit.key.clone(),
            epoch,
            from: id.clone(),
        };
        let create = sync::create_on_server(
            session,
            &server_project,
            &server_worktree,
            account_id,
            resume,
            Some(held),
        );
        let remote: RemoteSession =
            client.call("sessions.create", serde_json::to_value(&create)?)?;
        moved.push((id.clone(), remote));
    }
    Ok(moved)
}

/// A unit brought back: each session to add here and start (in order), and the server sessions
/// to close once they run ([`close_on_server`]).
#[derive(Clone, Debug)]
pub struct MovedToMac {
    pub key: WorktreeKey,
    pub epoch: u64,
    /// (the server session, the one continuing it here)
    pub sessions: Vec<(String, Session)>,
}

/// Bring one unit back to this Mac (`worktree.move --to-mac`). The server stops its agents in
/// the worktree when the move begins; the caller adds and starts the returned sessions, then
/// calls [`close_on_server`]. On an error the unit stays on the server.
pub fn move_unit_to_mac(client: &RemoteClient, config: &ServerConfig, local: &State, unit: &Unit, mac: &MachineId) -> anyhow::Result<MovedToMac> {
    if unit.main {
        bail!("a project's main checkout moves session by session");
    }
    let server = server_machine(client)?;
    let seen = owner(client, &unit.key, &server)?;
    if seen.owner != server {
        bail!("this worktree already runs on this Mac");
    }
    let begun = owners_call(client, "owners.begin", serde_json::to_value(OwnersBegin { key: unit.key.clone(), from: server.clone(), to: mac.clone(), epoch: seen.epoch })?)?;
    let epoch = begun.0.moving.as_ref().map_or(seen.epoch + 1, |m| m.epoch);
    let result = fetch_unit(client, config, local, unit, epoch).and_then(|sessions| commit_or_settle(client, &unit.key, epoch, mac, &server).map(|_| sessions));
    match result {
        Ok(sessions) => Ok(MovedToMac { key: unit.key.clone(), epoch, sessions }),
        Err(err) => {
            give_up(client, &unit.key, epoch);
            Err(err)
        }
    }
}

/// Before moves to the server run in parallel: each of these projects (ids here) is on the
/// server, added (cloned there) once if missing.
pub fn ensure_projects(client: &RemoteClient, local: &State, project_ids: &[String]) -> anyhow::Result<()> {
    let mut state: ServerState = client.call("state", json!({}))?;
    for id in project_ids {
        let Some(project) = local.project(id) else { continue };
        let found = sync::server_project(client, &state, project, &crate::paths::home(), &client.hello.home)?;
        if !state.projects.iter().any(|p| p.id == found.id) {
            state.projects.push(found);
        }
    }
    Ok(())
}

/// How a move a restart interrupted ends, by what the owner record says now.
pub enum Recovered {
    /// It reached the server: these sessions continue it there (the ones here end).
    ToServer(Vec<RemoteSession>),
    /// It came back here: these sessions continue it (then the server's close).
    ToMac(MovedToMac),
    /// It did not happen, or was given up now: the unit stays where it was.
    NotMoved,
}

/// Finish a move from its note ([`MoveNote`]) after a restart or a lost reply: the owner record
/// decides. A move still under way is given up (nothing of it runs yet).
pub fn recover(client: &RemoteClient, config: &ServerConfig, local: &State, note: &MoveNote, mac: &MachineId) -> anyhow::Result<Recovered> {
    let server = server_machine(client)?;
    let holder = match note.direction {
        Direction::ToServer => mac,
        Direction::ToMac => &server,
    };
    let record = owner(client, &note.unit.key, holder)?;
    if let Some(moving) = &record.moving {
        give_up(client, &note.unit.key, moving.epoch);
        return Ok(Recovered::NotMoved);
    }
    let state: ServerState = client.call("state", json!({}))?;
    let there = note.unit.key.path_under(&client.hello.home);
    let sessions: Vec<RemoteSession> = state.sessions.into_iter().filter(|r| r.session.worktree == there).collect();
    match note.direction {
        Direction::ToServer if record.owner == server => Ok(Recovered::ToServer(sessions)),
        Direction::ToMac if &record.owner == mac => {
            // The files came before the commit; the sessions here are made again from the
            // server's (stopped) ones.
            let unit = Unit { sessions: sessions.iter().map(|r| r.session.id.clone()).collect(), titles: sessions.iter().map(|r| r.session.title.clone()).collect(), ..note.unit.clone() };
            let moved = fetch_unit(client, config, local, &unit, record.epoch)?;
            Ok(Recovered::ToMac(MovedToMac { key: unit.key.clone(), epoch: record.epoch, sessions: moved }))
        }
        _ => Ok(Recovered::NotMoved),
    }
}

fn fetch_unit(
    client: &RemoteClient,
    config: &ServerConfig,
    local: &State,
    unit: &Unit,
    epoch: u64,
) -> anyhow::Result<Vec<(String, Session)>> {
    let state: ServerState = client.call("state", json!({}))?;
    let server_project = state
        .projects
        .iter()
        .find(|p| p.id == unit.project_id)
        .ok_or_else(|| anyhow!("no project for {}", unit.worktree.display()))?;
    let project = sync::local_project(
        local,
        server_project,
        &client.hello.home,
        &crate::paths::home(),
    )?;
    let token = format!("u{}e{epoch}", unit.key.token());
    let worktree = sync::fetch_worktree(
        client,
        config,
        project,
        server_project,
        &unit.worktree,
        &token,
    )?;
    // Progress, so the server does not give a long move up (its two-minute rule).
    let touch = || {
        let _ = client.call::<Value>("owners.touch", json!({ "key": unit.key, "epoch": epoch }));
    };
    touch();
    let mut moved = Vec::new();
    for id in &unit.sessions {
        touch();
        let remote = state
            .sessions
            .iter()
            .find(|r| &r.session.id == id)
            .ok_or_else(|| anyhow!("server session {id} is gone"))?;
        let (account_id, conversation) =
            sync::fetch_conversation(client, config, local, &state, &remote.session, &worktree)?;
        moved.push((
            id.clone(),
            sync::local_session(
                local,
                &remote.session,
                project,
                worktree.clone(),
                account_id,
                conversation,
            ),
        ));
    }
    Ok(moved)
}

/// After a unit came back and its sessions run here: the server's copies end.
pub fn close_on_server(client: &RemoteClient, moved: &MovedToMac) -> anyhow::Result<()> {
    for (server_id, _) in &moved.sessions {
        client.call::<Value>("sessions.close", json!({ "session_id": server_id }))?;
    }
    Ok(())
}

/// Whether this Mac may run a session in `key`'s worktree, by the records the server sent
/// (`MirrorSyncReply::owners`): not when the server owns it now or a move of it is under way
/// that this Mac did not start.
pub fn may_run_here(
    records: &std::collections::HashMap<WorktreeKey, OwnerRecord>,
    key: &WorktreeKey,
    mac: &MachineId,
) -> bool {
    match records.get(key) {
        None => true,
        Some(record) => &record.owner == mac,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Project, new_id};

    fn session(project: &str, worktree: &str, title: &str, agent: AgentKind) -> Session {
        Session {
            id: new_id(),
            project_id: project.into(),
            worktree: PathBuf::from(worktree),
            branch: "fix/retry".into(),
            title: title.into(),
            agent,
            account_id: None,
            agent_session_id: None,
            transcript_path: None,
            lineage: Vec::new(),
            initial_prompt: None,
            team_task: None,
            model: None,
            mode: None,
            review: None,
            created_at: 0,
        }
    }

    fn state() -> State {
        let mut state = State::default();
        let project: Project = serde_json::from_value(
            json!({"id": "p1", "name": "app", "root": "/Users/alex/code/app"}),
        )
        .unwrap();
        state.projects.push(project);
        state.sessions.push(session(
            "p1",
            "/Users/alex/.glasscode/worktrees/app/fix-retry",
            "Claude · fix",
            AgentKind::Claude,
        ));
        state.sessions.push(session(
            "p1",
            "/Users/alex/.glasscode/worktrees/app/fix-retry",
            "Codex · fix",
            AgentKind::Codex,
        ));
        state.sessions.push(session(
            "p1",
            "/Users/alex/.glasscode/worktrees/app/docs",
            "Claude · docs",
            AgentKind::Claude,
        ));
        state
    }

    #[test]
    fn sessions_in_one_worktree_are_one_unit() {
        let state = state();
        let units = units_to_server(&state);
        assert_eq!(units.len(), 2);
        let fix = units
            .iter()
            .find(|u| u.worktree.ends_with("fix-retry"))
            .unwrap();
        assert_eq!(
            fix.sessions.len(),
            2,
            "both sessions of the shared worktree move together"
        );
        assert_eq!(fix.titles, ["Claude · fix", "Codex · fix"]);
        // Moving either session moves the whole worktree.
        let codex = &state.sessions[1].id;
        assert_eq!(unit_of(&units, codex).unwrap().sessions, fix.sessions);
    }

    #[test]
    fn a_working_session_and_a_shell_are_reported() {
        let mut state = state();
        state.sessions[2].agent = AgentKind::Shell;
        let server: ServerState = serde_json::from_value(json!({"projects": [], "worktrees": {}, "accounts": [], "signed_in": {}, "defaults": {}, "sessions": []})).unwrap();
        let units = units_to_server(&state);
        let working = state.sessions[0].id.clone();
        let activity = move |id: &str| (id == working).then_some(AgentActivity::Working);
        let fix = units
            .iter()
            .find(|u| u.worktree.ends_with("fix-retry"))
            .unwrap();
        let problems = check_to_server(&state, &server, &["owners".into()], fix, &activity);
        assert!(problems.contains(&Problem::Working {
            session: state.sessions[0].id.clone()
        }));
        assert!(
            problems.iter().all(|p| !p.blocks()
                || matches!(p, Problem::NoProject { .. } | Problem::NoAccount { .. }))
        );
        let docs = units.iter().find(|u| u.worktree.ends_with("docs")).unwrap();
        let problems = check_to_server(&state, &server, &[], docs, &activity);
        assert!(problems.contains(&Problem::OldServer));
        assert!(problems.contains(&Problem::ShellRestarts {
            session: state.sessions[2].id.clone()
        }));
    }

    #[test]
    fn a_lost_reply_is_settled_by_the_owner_record() {
        let (mac, server) = (MachineId::parse("mac").unwrap(), MachineId::parse("server").unwrap());
        let moving = |to: &MachineId, epoch| Some(crate::owners::Moving { to: to.clone(), epoch, since_ms: 0 });
        let record = |owner: &MachineId, epoch, m| OwnerRecord { owner: owner.clone(), epoch, moving: m };
        assert_eq!(settled(&record(&server, 3, None), 3, &server), Settled::Moved);
        assert_eq!(settled(&record(&mac, 2, moving(&server, 3)), 3, &server), Settled::Moving);
        assert_eq!(settled(&record(&mac, 2, None), 3, &server), Settled::NotMoved, "given up");
        assert_eq!(settled(&record(&server, 4, None), 3, &server), Settled::NotMoved, "a later move, not this one");
        assert_eq!(settled(&record(&mac, 2, moving(&mac, 3)), 3, &server), Settled::NotMoved);
    }

    #[test]
    fn this_mac_stops_a_worktree_the_server_owns() {
        let mac = MachineId::parse("mac").unwrap();
        let server = MachineId::parse("server").unwrap();
        let key = WorktreeKey(".glasscode/worktrees/app/fix-retry".into());
        let mut records = std::collections::HashMap::new();
        assert!(may_run_here(&records, &key, &mac));
        records.insert(
            key.clone(),
            OwnerRecord {
                owner: server,
                epoch: 2,
                moving: None,
            },
        );
        assert!(!may_run_here(&records, &key, &mac));
        records.insert(
            key.clone(),
            OwnerRecord {
                owner: mac.clone(),
                epoch: 3,
                moving: None,
            },
        );
        assert!(may_run_here(&records, &key, &mac));
    }
}
