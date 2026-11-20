//! Moving work between this Mac and the server ("Server mode" / "Laptop mode").
//!
//! A session moves as three things:
//! - **its worktree**, through git: commits are pushed or fetched, and uncommitted work travels
//!   as a snapshot commit built in a temporary index (the user's own index and stashes are
//!   untouched). The receiving side checks the branch out and lays the snapshot over it, so the
//!   work is uncommitted there too. Small ignored config files (`.env*`) travel alongside;
//!   `node_modules` and build output are rebuilt, never copied.
//! - **its conversation file**, copied into the matching account on the other machine, with
//!   the folder paths inside rewritten to the new location;
//! - **the session**, resumed on the other side (`claude --resume`, `codex resume`).
//!
//! Folders map by their place under the home folder: `~/Projects/myapp` here is
//! `~/Projects/myapp` there. Only one machine runs a conversation at a time; callers move a
//! session only when its agent is idle.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use serde_json::{Value, json};

use crate::agents;
use crate::git;
use crate::model::{Account, AgentKind, Defaults, Project, Session, State, new_id, now};
use crate::remote::{
    ConversationFile, CreateSession, EnvFile, ReceiveWorktree, RemoteClient, RemoteSession, ServerConfig, ServerState, WorktreeSnapshot,
};

pub(crate) fn git_cmd(dir: &Path) -> Command {
    let mut cmd = crate::sys::command("git");
    cmd.arg("-C").arg(dir).env("GIT_TERMINAL_PROMPT", "0").env("GIT_OPTIONAL_LOCKS", "0");
    cmd
}

pub(crate) fn run(mut cmd: Command) -> anyhow::Result<String> {
    let output = cmd.output().context("could not run git")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let mut cmd = git_cmd(dir);
    cmd.args(args);
    run(cmd)
}

fn ok(dir: &Path, args: &[&str]) -> bool {
    git_cmd(dir).args(args).output().is_ok_and(|o| o.status.success())
}

/// A worktree's state: its branch (or detached), commit, and a commit with the uncommitted
/// work on top (`snap == head` when there is none).
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub branch: Option<String>,
    pub head: String,
    pub snap: String,
}

pub fn snapshot(dir: &Path) -> anyhow::Result<Snapshot> {
    let head = git(dir, &["rev-parse", "HEAD"])?;
    let branch = git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok().filter(|b| !b.is_empty());
    let tmp = tempfile::tempdir()?;
    let index = tmp.path().join("index");
    let with_index = |args: &[&str]| -> anyhow::Result<String> {
        let mut cmd = git_cmd(dir);
        cmd.env("GIT_INDEX_FILE", &index).args(args);
        run(cmd)
    };
    // A copy of the worktree's own index knows which files are unchanged (their stat data),
    // so only changed files are read again; the user's index itself is never touched.
    let own_index = git(dir, &["rev-parse", "--path-format=absolute", "--git-path", "index"]).ok().map(PathBuf::from);
    if !own_index.is_some_and(|own| std::fs::copy(&own, &index).is_ok()) {
        with_index(&["read-tree", "HEAD"])?;
    }
    // Tracked changes, deletions and untracked files; ignored files stay out.
    with_index(&["add", "-A", "."])?;
    let tree = with_index(&["write-tree"])?;
    let head_tree = git(dir, &["rev-parse", "HEAD^{tree}"])?;
    let snap = if tree == head_tree {
        head.clone()
    } else {
        let mut cmd = git_cmd(dir);
        cmd.args(["commit-tree", &tree, "-p", &head, "-m", "Glasscode snapshot (uncommitted work)"])
            .env("GIT_AUTHOR_NAME", "Glasscode")
            .env("GIT_AUTHOR_EMAIL", "glasscode@localhost")
            .env("GIT_COMMITTER_NAME", "Glasscode")
            .env("GIT_COMMITTER_EMAIL", "glasscode@localhost");
        run(cmd)?
    };
    Ok(Snapshot { branch, head, snap })
}

/// Key for remembering the snapshot last sent away from a folder.
fn sent_ref(path: &Path) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(path.to_string_lossy().as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("refs/glasscode/sent/{hex}")
}

/// Remember what left this folder, so that receiving it back later can tell "unchanged since
/// it left" (safe to replace) from "edited here meanwhile" (kept in a stash).
pub fn remember_sent(root: &Path, path: &Path, snap: &str) {
    let _ = git(root, &["update-ref", &sent_ref(path), snap]);
}

/// The worktree that already is exactly `head` (on `branch`) plus the snapshot's changes, if
/// one is: a second session of the same switch arriving in a folder the first one brought.
pub fn matching_worktree(root: &Path, path: &Path, branch: Option<&str>, head: &str, snap: &str) -> Option<PathBuf> {
    let list = git::worktrees(root).ok()?;
    let target = branch.and_then(|b| list.iter().find(|w| w.branch == b)).map(|w| w.path.clone()).unwrap_or_else(|| path.to_path_buf());
    if !target.is_dir() || !list.iter().any(|w| w.path == target) {
        return None;
    }
    let current = snapshot(&target).ok()?;
    let same_tree = git(&target, &["rev-parse", &format!("{}^{{tree}}", current.snap)]).ok()? == git(&target, &["rev-parse", &format!("{snap}^{{tree}}")]).ok()?;
    (current.head == head && current.branch.as_deref() == branch && same_tree).then_some(target)
}

/// Set up `path` (a worktree of `root`) to be `head` on `branch` with the snapshot's
/// uncommitted work on top. The objects must already be in `root`'s repository.
///
/// Nothing is lost: a branch tip that `head` does not contain is kept as
/// `refs/glasscode/backup/<branch>/<time>`, and uncommitted work in the target that differs from
/// what last left it is stashed. Returns the worktree's path (an existing checkout of the
/// branch elsewhere is used instead, since git allows only one).
pub fn receive_worktree(root: &Path, path: &Path, branch: Option<&str>, head: &str, snap: &str) -> anyhow::Result<PathBuf> {
    let list = git::worktrees(root)?;
    let target = branch.and_then(|b| list.iter().find(|w| w.branch == b)).map(|w| w.path.clone()).unwrap_or_else(|| path.to_path_buf());
    let exists = target.is_dir() && list.iter().any(|w| w.path == target);
    let stamp = now();

    if exists && git::is_dirty(&target) {
        let current = snapshot(&target)?;
        let sent = git(root, &["rev-parse", "--verify", "--quiet", &format!("{}^{{tree}}", sent_ref(&target))]).ok();
        let current_tree = git(&target, &["rev-parse", &format!("{}^{{tree}}", current.snap)])?;
        if sent.as_deref() != Some(current_tree.as_str()) {
            git(&target, &["stash", "push", "--include-untracked", "-m", &format!("Glasscode: kept before receiving {} ({stamp})", branch.unwrap_or("detached HEAD"))])?;
        } else {
            git(&target, &["reset", "--hard", "-q"])?;
            git(&target, &["clean", "-fdq"])?;
        }
    }

    match branch {
        Some(b) => {
            let full = format!("refs/heads/{b}");
            if let Ok(tip) = git(root, &["rev-parse", "--verify", "--quiet", &full]) {
                if tip != head && !ok(root, &["merge-base", "--is-ancestor", &tip, head]) {
                    git(root, &["update-ref", &format!("refs/glasscode/backup/{b}/{stamp}"), &tip])?;
                }
            }
            if exists {
                let current = git(&target, &["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_default();
                if current == b {
                    git(&target, &["reset", "--hard", "-q", head])?;
                } else {
                    git(&target, &["checkout", "-q", "-B", b, head])?;
                }
            } else {
                git(root, &["update-ref", &full, head])?;
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut cmd = git_cmd(root);
                cmd.args(["worktree", "add", "-q"]).arg(&target).arg(b);
                run(cmd)?;
            }
        }
        None => {
            if exists {
                git(&target, &["checkout", "-q", "--detach", head])?;
            } else {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut cmd = git_cmd(root);
                cmd.args(["worktree", "add", "-q", "--detach"]).arg(&target).arg(head);
                run(cmd)?;
            }
        }
    }
    if snap != head {
        // Working tree = snapshot, index = HEAD: the work arrives uncommitted, as it left.
        git(&target, &["read-tree", "-u", "--reset", snap])?;
        git(&target, &["reset", "-q"])?;
    }
    remember_sent(root, &target, snap);
    Ok(target)
}

/// Ignored `.env*` files in a worktree (not inside ignored folders like `node_modules`), and a
/// review session's notes (`.glasscode/review/`, kept out of git).
pub fn env_files(dir: &Path) -> Vec<EnvFile> {
    let engine = base64::engine::general_purpose::STANDARD;
    let read = |p: &str| {
        let bytes = std::fs::read(dir.join(p)).ok()?;
        (bytes.len() <= 256 * 1024).then(|| EnvFile { path: p.to_string(), data: engine.encode(bytes) })
    };
    let mut files: Vec<EnvFile> = match git(dir, &["status", "--porcelain", "--untracked-files=normal", "--ignored=traditional"]) {
        Ok(out) => out
            .lines()
            .filter_map(|l| l.strip_prefix("!! "))
            .filter(|p| !p.ends_with('/') && p.rsplit('/').next().is_some_and(|n| n.starts_with(".env")))
            .filter_map(read)
            .collect(),
        Err(_) => Vec::new(),
    };
    files
}

pub fn write_env_files(dir: &Path, files: &[EnvFile]) -> anyhow::Result<()> {
    let engine = base64::engine::general_purpose::STANDARD;
    for file in files {
        let rel = Path::new(&file.path);
        if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            continue;
        }
        let target = dir.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, engine.decode(&file.data)?)?;
        if file.path.starts_with(".glasscode/") {
            crate::git::exclude(dir, "/.glasscode/")?;
        } else if !ok(dir, &["check-ignore", "-q", "--", &file.path]) {
            // Ignored where it came from (maybe only there): ignored here too, so no agent
            // commits the secrets in it.
            crate::git::exclude(dir, &format!("/{}", file.path))?;
        }
    }
    Ok(())
}

/// Rewrite the working folder recorded in a conversation file (Claude's `cwd` on each line,
/// Codex's `session_meta.payload.cwd`) from `from` to `to`. Lines that are not JSON stay as
/// they are.
pub fn rewrite_cwd(content: &[u8], from: &Path, to: &Path) -> Vec<u8> {
    let from = from.to_string_lossy().to_string();
    let to = to.to_string_lossy().to_string();
    let swap = |value: &mut Value| {
        if let Some(cwd) = value.as_str() {
            if cwd == from {
                *value = Value::String(to.clone());
            } else if let Some(rest) = cwd.strip_prefix(&format!("{from}/")) {
                *value = Value::String(format!("{to}/{rest}"));
            }
        }
    };
    let text = String::from_utf8_lossy(content);
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, newline) = line.strip_suffix('\n').map(|b| (b, "\n")).unwrap_or((line, ""));
        match serde_json::from_str::<Value>(body) {
            Ok(mut value) if value.is_object() => {
                if let Some(cwd) = value.get_mut("cwd") {
                    swap(cwd);
                }
                if let Some(cwd) = value.get_mut("payload").and_then(|p| p.get_mut("cwd")) {
                    swap(cwd);
                }
                out.push_str(&serde_json::to_string(&value).unwrap_or_else(|_| body.to_string()));
            }
            _ => out.push_str(body),
        }
        out.push_str(newline);
    }
    out.into_bytes()
}

/// `path` under `from_home`, placed under `to_home`; folders outside the home folder use the
/// standard Glasscode worktree place for the project and branch.
pub fn map_path(path: &Path, from_home: &Path, to_home: &Path, project: &str, branch: Option<&str>) -> PathBuf {
    match path.strip_prefix(from_home) {
        Ok(rel) => to_home.join(rel),
        Err(_) => to_home.join(".glasscode/worktrees").join(git::slug(project)).join(git::slug(branch.unwrap_or("detached"))),
    }
}

/// The account on the other machine for a conversation: the same id, else the same agent and
/// name, else that machine's default for the agent.
pub fn map_account(from: Option<&Account>, agent: AgentKind, to_accounts: &[Account], to_defaults: &Defaults) -> Option<String> {
    if let Some(from) = from {
        if to_accounts.iter().any(|a| a.id == from.id && a.agent == agent) {
            return Some(from.id.clone());
        }
        if let Some(same) = to_accounts.iter().find(|a| a.agent == agent && a.label == from.label) {
            return Some(same.id.clone());
        }
    }
    to_defaults.for_agent(agent).cloned().or_else(|| to_accounts.iter().find(|a| a.agent == agent).map(|a| a.id.clone()))
}

pub(crate) fn origin_url(root: &Path) -> Option<String> {
    git(root, &["remote", "get-url", "origin"]).ok().filter(|u| !u.is_empty())
}

/// The conversation file of a local session, if it has one yet.
pub(crate) fn local_conversation(session: &Session, account: &Account) -> Option<PathBuf> {
    let id = session.agent_session_id.as_deref()?;
    let dir = account.effective_dir();
    match session.agent {
        AgentKind::Claude => agents::find_claude_transcript(&dir, id),
        AgentKind::Codex => session.transcript_path.clone().filter(|p| p.is_file()).or_else(|| agents::find_codex_rollout(&dir, id)),
        AgentKind::Shell => None,
    }
}

pub(crate) fn token_for(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric()).take(12).collect()
}

/// The project on the server for a project here: the one at the same place under home, else the
/// same project by name and folder (one synced there), else a clone of its origin there.
pub(crate) fn server_project(
    client: &RemoteClient,
    state: &ServerState,
    project: &Project,
    local_home: &Path,
    server_home: &Path,
) -> anyhow::Result<Project> {
    let server_root = map_path(&project.root, local_home, server_home, &project.name, None);
    let found = state
        .projects
        .iter()
        .find(|p| p.root == server_root)
        .or_else(|| {
            state
                .projects
                .iter()
                .find(|p| p.name == project.name && p.root.file_name() == project.root.file_name())
        });
    match found {
        Some(p) => Ok(p.clone()),
        None => {
            let url = origin_url(&project.root).ok_or_else(|| {
                anyhow!(
                    "{} has no origin remote to clone on the server",
                    project.name
                )
            })?;
            client.call::<Project>(
                "projects.add",
                json!({ "url": url, "path": server_root, "name": project.name }),
            )
        }
    }
}

/// The project here for a project on the server, found the same way.
pub(crate) fn local_project<'a>(
    local: &'a State,
    server_project: &Project,
    server_home: &Path,
    local_home: &Path,
) -> anyhow::Result<&'a Project> {
    let local_root = map_path(
        &server_project.root,
        server_home,
        local_home,
        &server_project.name,
        None,
    );
    local
        .projects
        .iter()
        .find(|p| p.root == local_root)
        .or_else(|| {
            local.projects.iter().find(|p| {
                p.name == server_project.name
                    && p.root.file_name() == server_project.root.file_name()
            })
        })
        .ok_or_else(|| {
            anyhow!(
                "{} is not a project on this Mac (expected at {}); add it first",
                server_project.name,
                local_root.display()
            )
        })
}

/// Send a worktree to the server: push its HEAD and its uncommitted snapshot, then rebuild it
/// there. Returns where it is on the server.
pub(crate) fn send_worktree(
    client: &RemoteClient,
    config: &ServerConfig,
    project: &Project,
    server_project: &Project,
    worktree: &Path,
    token: &str,
    move_epoch: Option<(crate::owners::WorktreeKey, u64)>,
) -> anyhow::Result<PathBuf> {
    let snap = snapshot(worktree)?;
    let mut push = git_cmd(worktree);
    push.env("GIT_SSH_COMMAND", config.git_ssh_command()).args([
        "push",
        "--quiet",
        "--force",
        "--no-verify",
        &config.git_url(&server_project.root),
        &format!("{}:refs/glasscode/in/{token}/head", snap.head),
        &format!("{}:refs/glasscode/in/{token}/snap", snap.snap),
    ]);
    run(push).context("could not send the worktree to the server")?;
    let server_worktree = map_path(
        worktree,
        &crate::paths::home(),
        &client.hello.home,
        &project.name,
        snap.branch.as_deref(),
    );
    let receive = ReceiveWorktree {
        project_id: server_project.id.clone(),
        path: server_worktree,
        branch: snap.branch.clone(),
        token: token.to_string(),
        env: env_files(worktree),
        move_epoch,
    };
    let server_worktree: PathBuf =
        client.call("worktrees.receive", serde_json::to_value(&receive)?)?;
    let root = git::main_checkout(worktree).unwrap_or(project.root.clone());
    remember_sent(&root, worktree, &snap.snap);
    Ok(server_worktree)
}

/// Copy a session's conversation into the matching account on the server. Returns that account
/// and the conversation to resume there.
pub(crate) fn send_conversation(
    client: &RemoteClient,
    config: &ServerConfig,
    local: &State,
    state: &ServerState,
    session: &Session,
    server_worktree: &Path,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    let account = session
        .account_id
        .as_deref()
        .and_then(|id| local.account(id));
    let server_account = if session.agent.uses_accounts() {
        map_account(account, session.agent, &state.accounts, &state.defaults)
    } else {
        None
    };
    let mut resume = None;
    if let (Some(account), Some(server_account), Some(id)) = (
        account,
        server_account.as_ref(),
        session.agent_session_id.as_ref(),
    ) {
        match local_conversation(session, account) {
            Some(file) => {
                let content =
                    rewrite_cwd(&std::fs::read(&file)?, &session.worktree, server_worktree);
                let rel = match session.agent {
                    AgentKind::Claude => PathBuf::from("projects")
                        .join(crate::history::claude_folder_name(server_worktree))
                        .join(format!("{id}.jsonl")),
                    _ => file
                        .strip_prefix(account.effective_dir())
                        .map(Path::to_path_buf)
                        .map_err(|_| anyhow!("unexpected conversation location"))?,
                };
                let target: PathBuf = client.call(
                    "conversations.import_path",
                    json!({ "account_id": server_account, "rel": rel }),
                )?;
                config.upload(&content, &target)?;
                resume = Some(id.clone());
            }
            // A Claude session without a transcript yet keeps its id (`--session-id`).
            None if session.agent == AgentKind::Claude => resume = Some(id.clone()),
            None => {}
        }
    }
    Ok((server_account, resume))
}

/// The request that resumes a session on the server.
pub(crate) fn create_on_server(
    session: &Session,
    server_project: &Project,
    server_worktree: &Path,
    account_id: Option<String>,
    resume: Option<String>,
    held: Option<crate::remote::MoveTag>,
) -> CreateSession {
    CreateSession {
        project_id: server_project.id.clone(),
        worktree: Some(server_worktree.to_path_buf()),
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
        held,
    }
}

/// Move a session from this Mac to the server and resume it there. The caller closes the local
/// session afterwards (its worktree stays as it is).
pub fn move_to_server(
    client: &RemoteClient,
    config: &ServerConfig,
    local: &State,
    session: &Session,
) -> anyhow::Result<RemoteSession> {
    let project = local
        .project(&session.project_id)
        .ok_or_else(|| anyhow!("no project for {}", session.title))?;
    let state: ServerState = client.call("state", json!({}))?;
    let server_project = server_project(
        client,
        &state,
        project,
        &crate::paths::home(),
        &client.hello.home,
    )?;
    let server_worktree = send_worktree(
        client,
        config,
        project,
        &server_project,
        &session.worktree,
        &token_for(&session.id),
        None,
    )?;
    let (account_id, resume) =
        send_conversation(client, config, local, &state, session, &server_worktree)?;
    let create = create_on_server(
        session,
        &server_project,
        &server_worktree,
        account_id,
        resume,
        None,
    );
    client.call("sessions.create", serde_json::to_value(&create)?)
}

/// Fetch a worktree from the server and rebuild it here (changes made here meanwhile go to a
/// stash and a backup ref, never lost). Returns where it is here.
pub(crate) fn fetch_worktree(
    client: &RemoteClient,
    config: &ServerConfig,
    project: &Project,
    server_project: &Project,
    server_worktree: &Path,
    token: &str,
) -> anyhow::Result<PathBuf> {
    let snap: WorktreeSnapshot = client.call(
        "worktrees.snapshot",
        json!({ "path": server_worktree, "token": token }),
    )?;
    let mut fetch = git_cmd(&project.root);
    fetch
        .env("GIT_SSH_COMMAND", config.git_ssh_command())
        .args([
            "fetch",
            "--quiet",
            "--force",
            "--no-tags",
            &config.git_url(&snap.root),
            &format!("refs/glasscode/out/{token}/head:refs/glasscode/in/{token}/head"),
            &format!("refs/glasscode/out/{token}/snap:refs/glasscode/in/{token}/snap"),
        ]);
    run(fetch).context("could not fetch the worktree from the server")?;
    let local_worktree = map_path(
        server_worktree,
        &client.hello.home,
        &crate::paths::home(),
        &project.name,
        snap.branch.as_deref(),
    );
    let worktree = receive_worktree(
        &project.root,
        &local_worktree,
        snap.branch.as_deref(),
        &snap.head,
        &snap.snap,
    )?;
    write_env_files(&worktree, &snap.env)?;
    let _ = git(
        &project.root,
        &[
            "update-ref",
            "-d",
            &format!("refs/glasscode/in/{token}/head"),
        ],
    );
    let _ = git(
        &project.root,
        &[
            "update-ref",
            "-d",
            &format!("refs/glasscode/in/{token}/snap"),
        ],
    );
    let _ = client.call::<Value>(
        "refs.clear",
        json!({ "project_id": server_project.id, "token": token }),
    );
    Ok(worktree)
}

/// Copy a server session's conversation into the matching account here. Returns that account
/// and the conversation to resume here.
pub(crate) fn fetch_conversation(
    client: &RemoteClient,
    config: &ServerConfig,
    local: &State,
    state: &ServerState,
    session: &Session,
    worktree: &Path,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    let server_account = session
        .account_id
        .as_deref()
        .and_then(|id| state.accounts.iter().find(|a| a.id == id));
    let account_id = if session.agent.uses_accounts() {
        map_account(
            server_account,
            session.agent,
            &local.accounts,
            &local.defaults,
        )
    } else {
        None
    };
    let mut conversation = None;
    if let (Some(account_id), Some(id)) = (account_id.as_ref(), session.agent_session_id.as_ref()) {
        let account = local
            .account(account_id)
            .ok_or_else(|| anyhow!("no account {account_id} on this Mac"))?;
        match client
            .call::<ConversationFile>("conversations.export", json!({ "session_id": session.id }))
        {
            Ok(file) => {
                let content =
                    rewrite_cwd(&config.download(&file.path)?, &session.worktree, worktree);
                let target = match session.agent {
                    AgentKind::Claude => account
                        .effective_dir()
                        .join("projects")
                        .join(crate::history::claude_folder_name(worktree))
                        .join(format!("{id}.jsonl")),
                    _ => account.effective_dir().join(&file.rel),
                };
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target, content)?;
                conversation = Some(id.clone());
            }
            Err(_) if session.agent == AgentKind::Claude => conversation = Some(id.clone()),
            Err(_) => {}
        }
    }
    Ok((account_id, conversation))
}

/// The session here that continues a server session.
pub(crate) fn local_session(
    local: &State,
    session: &Session,
    project: &Project,
    worktree: PathBuf,
    account_id: Option<String>,
    conversation: Option<String>,
) -> Session {
    let branch = git::current_branch(&worktree);
    let mut lineage = session.lineage.clone();
    lineage.push(local.lineage_tag(session.agent, account_id.as_deref()));
    Session {
        id: new_id(),
        project_id: project.id.clone(),
        worktree,
        branch,
        title: session.title.clone(),
        agent: session.agent,
        account_id,
        agent_session_id: conversation,
        transcript_path: None,
        lineage,
        initial_prompt: None,
        team_task: session.team_task.clone(),
        model: session.model.clone(),
        mode: session.mode.clone(),
        review: session.review.clone(),
        created_at: now(),
    }
}

/// Move a server session to this Mac: fetch its worktree, copy its conversation, close it on
/// the server. Returns the local session to add and start (it resumes the conversation).
pub fn move_to_laptop(
    client: &RemoteClient,
    config: &ServerConfig,
    local: &State,
    remote: &RemoteSession,
) -> anyhow::Result<Session> {
    let session = &remote.session;
    let state: ServerState = client.call("state", json!({}))?;
    let server_project = state
        .projects
        .iter()
        .find(|p| p.id == session.project_id)
        .ok_or_else(|| anyhow!("no project for {}", session.title))?;
    let project = local_project(
        local,
        server_project,
        &client.hello.home,
        &crate::paths::home(),
    )?;
    let worktree = fetch_worktree(
        client,
        config,
        project,
        server_project,
        &session.worktree,
        &token_for(&session.id),
    )?;
    let (account_id, conversation) =
        fetch_conversation(client, config, local, &state, session, &worktree)?;
    // Only one machine runs a conversation: the server session ends here.
    client.call::<Value>("sessions.close", json!({ "session_id": session.id }))?;
    Ok(local_session(
        local,
        session,
        project,
        worktree,
        account_id,
        conversation,
    ))
}
