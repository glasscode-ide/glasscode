//! The rest of what the `glasscode` command's handlers (`ops_run.rs`) share: cloning and adding
//! projects, removing and cleaning up worktrees, and the accounts (see `commands.rs` for sessions).

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, Window};
use glasscode_core::control::Reply;
use glasscode_core::git::{self, Worktree};
use glasscode_core::github;
use glasscode_core::model::{AgentKind, Project, new_id};
use glasscode_core::sys;
use serde_json::{Value, json};

use crate::commands::{home_relative, plural};
use crate::control_ui::Responder;
use crate::ops_run::failed;
use crate::workspace::Workspace;

/// "14d", "2w", "30" (days).
pub(crate) fn days(text: &str) -> Option<i64> {
    let t = text.trim().to_lowercase();
    if let Some(n) = t.strip_suffix('w') {
        return n.parse::<i64>().ok().map(|n| n * 7);
    }
    t.trim_end_matches('d').trim_end_matches(" days").parse().ok()
}

/// A git URL or `owner/name` to clone, and the folder name it gets.
pub(crate) fn clone_source(text: &str) -> Option<(String, String)> {
    clone_source_checked(text).ok()
}

/// What can be cloned: `https://host/path`, `ssh://host/path`, `git@host:path`, or `owner/name`
/// on GitHub (when no such folder is here), with the folder name it gets. Anything else is
/// refused with a sentence: other transports (`file://`, `ext::`, `git://`, `http://`), a local
/// path, and text that git could read as an option (a leading `-`) or that has a space or a
/// control character.
pub(crate) fn clone_source_checked(text: &str) -> Result<(String, String), String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("Type owner/name of a GitHub repository, or an https:// or ssh:// address.".into());
    }
    if t.starts_with('-') {
        return Err(format!("{t} starts with -, which git would read as an option; it is not cloned."));
    }
    if t.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("A clone address has no space or control character in it.".into());
    }
    let host_ok = |host: &str| !host.is_empty() && !host.starts_with('-') && host.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:@[]".contains(c));
    for scheme in ["https://", "ssh://"] {
        if let Some(rest) = t.strip_prefix(scheme) {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            if host_ok(host) && !path.trim_matches('/').is_empty() {
                return Ok((t.to_string(), clone_folder_name(path)));
            }
            return Err(format!("{t} has no host and repository path; it is not cloned."));
        }
    }
    if let Some(rest) = t.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            if host_ok(host) && !path.is_empty() && !path.starts_with('-') {
                return Ok((t.to_string(), clone_folder_name(path)));
            }
        }
        return Err(format!("{t} is not git@host:owner/repo; it is not cloned."));
    }
    let parts: Vec<&str> = t.split('/').collect();
    let part_ok = |p: &&str| !p.is_empty() && !p.starts_with(['-', '.']) && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if parts.len() == 2 && parts.iter().all(part_ok) && !Path::new(t).exists() {
        return Ok((format!("https://github.com/{t}.git"), clone_folder_name(parts[1])));
    }
    Err(format!(
        "{t} is not cloned: only https://…, ssh://…, git@host:owner/repo and owner/name on GitHub are; other transports (file://, ext::, git://, http://) and local paths are refused."
    ))
}

/// The folder a clone goes into, from the last part of its address, never taken as it is:
/// letters, digits and `-_.` only, no leading `-` or `.`, no trailing `.`, no `..`, no
/// separators, no device name; `project` when nothing is left.
pub(crate) fn clone_folder_name(text: &str) -> String {
    let last = text.trim_end_matches(['/', '\\']).rsplit(['/', '\\', ':']).next().unwrap_or_default();
    let last = last.strip_suffix(".git").unwrap_or(last);
    let mut name: String = last.chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '-' }).collect();
    while name.contains("..") {
        name = name.replace("..", ".");
    }
    let name = name.trim_start_matches(['-', '.']).trim_end_matches('.').to_string();
    let stem = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
    match name.as_str() {
        "" => "project".into(),
        _ if device => format!("{name}_"),
        _ => name,
    }
}

/// Clones `url` into the new folder `target` (`project import` and the first run). It blocks.
/// `--` ends git's options, so the address is never read as one, and only https and ssh are
/// allowed for this clone.
/// [`git_clone`] for an op its caller may stop (ops-10): false when the clone was stopped, and
/// then its half-made folder is removed.
pub(crate) fn git_clone_unless(url: &str, target: &Path, env: &std::collections::HashMap<String, String>, job: Option<&glasscode_core::ops::job::Job>) -> anyhow::Result<bool> {
    let Some(job) = job else { return git_clone(url, target, env).map(|()| true) };
    std::fs::create_dir_all(target.parent().unwrap_or(target))?;
    let mut cmd = sys::command("git");
    cmd.arg("clone").arg("--").arg(url).arg(target).envs(env.iter()).env("GIT_TERMINAL_PROMPT", "0").env("GIT_ALLOW_PROTOCOL", "https:ssh");
    let Some(out) = glasscode_core::ops::job::output_unless(cmd, Some(job))? else {
        // Nothing of it is anyone's yet: the clone made the folder.
        let _ = std::fs::remove_dir_all(target);
        return Ok(false);
    };
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(true)
}

pub(crate) fn git_clone(url: &str, target: &Path, env: &std::collections::HashMap<String, String>) -> anyhow::Result<()> {
    std::fs::create_dir_all(target.parent().unwrap_or(target))?;
    // Never a password prompt nobody can answer: git's credential helper or nothing.
    let out = sys::command("git")
        .arg("clone")
        .arg("--")
        .arg(url)
        .arg(target)
        .envs(env.iter())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", "https:ssh")
        .stdin(std::process::Stdio::null())
        .output()?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

impl Workspace {
    /// Where new clones go: the folder most projects are in, else ~/Projects.
    pub(crate) fn projects_folder(&self) -> PathBuf {
        let mut counts: std::collections::HashMap<PathBuf, usize> = Default::default();
        for p in &self.state.projects {
            if let Some(parent) = p.root.parent() {
                *counts.entry(parent.to_path_buf()).or_default() += 1;
            }
        }
        counts.into_iter().max_by_key(|(_, n)| *n).map(|(p, _)| p).unwrap_or_else(|| glasscode_core::paths::home().join("Projects"))
    }

    /// Add a folder as a project without the New session dialog: (its id, new).
    pub fn add_project_quietly(&mut self, path: PathBuf, name: Option<String>, cx: &mut Context<Self>) -> Result<(String, bool), String> {
        let root = project_root_of(&path)?;
        self.add_project_at_root(root, name, cx)
    }

    /// [`Workspace::add_project_quietly`] for a folder [`project_root_of`] read (off the main
    /// thread, for the `glasscode` command).
    pub(crate) fn add_project_at_root(&mut self, root: PathBuf, name: Option<String>, cx: &mut Context<Self>) -> Result<(String, bool), String> {
        if root.parent().is_none() || root == glasscode_core::paths::home() {
            return Err(format!("{} is too broad to be a project; point at a repository folder.", home_relative(&root)));
        }
        if let Some(existing) = self.state.projects.iter().find(|p| p.root == root) {
            return Ok((existing.id.clone(), false));
        }
        let name = name.unwrap_or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into()));
        let id = new_id();
        self.state.projects.push(Project { id: id.clone(), name, root, expanded: true, github_account: None });
        self.save();
        self.configure_github(cx);
        self.refresh_worktrees(cx);
        cx.notify();
        Ok((id, true))
    }

    #[allow(clippy::too_many_arguments)]
    /// What the archive of a worktree's work needs from the window (M24): its project's memory key,
    /// its sessions' work, pull requests and conversations, read before its sessions close. None
    /// with the engine at v1 (rule 2), and for a project with no memory key yet. No IO.
    pub(crate) fn archive_input(&self, root: &Path, wt: &Worktree) -> Option<glasscode_core::memstore::archive::ArchiveInput> {
        use glasscode_core::memstore::archive::{ArchiveInput, Conversation, Ended};
        if !self.control.engine_v2 {
            return None;
        }
        let project = self.state.projects.iter().find(|p| p.root == root)?;
        let key = self.control.project_keys.get(&project.id)?.clone();
        let sessions: Vec<&glasscode_core::model::Session> = self.state.sessions.iter().filter(|s| s.worktree == wt.path).collect();
        let mut input = ArchiveInput::new(key.clone(), Ended::Removed, glasscode_core::clock::now_ms());
        input.worktree = Some(wt.path.clone());
        input.branch = (!wt.branch.is_empty()).then(|| wt.branch.clone());
        input.work = match sessions.first() {
            Some(s) => Some(glasscode_core::identity::WorkKey::for_session(&key, s, None, None)),
            None => input.branch.clone().map(|id| glasscode_core::identity::WorkKey { project: key.clone(), kind: glasscode_core::identity::WorkKind::Branch, id }),
        };
        for s in &sessions {
            if let Some(pr) = self.prs.map.get(&s.id).and_then(|p| Some((p.repo.clone()?, p.pr.as_ref()?.number, p.pr.as_ref()?.state.clone()))) {
                input.links.push((format!("PR {}#{}", pr.0, pr.1), if pr.2.is_empty() { "open".into() } else { pr.2.to_lowercase() }));
            }
            if let Some(path) = &s.transcript_path {
                input.conversations.push(Conversation { agent: s.agent, path: path.clone(), digest: None });
            }
        }
        input.links.dedup();
        Some(input)
    }

    pub(crate) fn remove_worktree_now(&mut self, root: PathBuf, wt: Worktree, delete_branch: bool, force: bool, r: Responder, window: &mut Window, cx: &mut Context<Self>) {
        let check_wt = wt.clone();
        let own_pid = std::process::id();
        let task = cx.background_spawn(async move {
            let programs = if crate::platform::removal_needs_free_folder() { git::programs_in(&git::process_folders(), &check_wt.path, own_pid) } else { Vec::new() };
            (git::check_worktree(&check_wt), programs)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (check, programs) = task.await;
            let _ = this.update_in(cx, |ws, window, cx| {
                // Before anything is deleted: git would delete the files and its record of the
                // worktree, then fail on the folder that the program holds. --force cannot help.
                if !programs.is_empty() {
                    ws.reply(r, failed(format!("Not removed: {} {} in that folder, so {} cannot delete it. Stop {} first.", programs.join(", "), if programs.len() == 1 { "runs" } else { "run" }, crate::platform::THIS.os_name, if programs.len() == 1 { "it" } else { "them" })));
                    return;
                }
                // Text not saved in Glasscode's editor is only in Glasscode (U20): no --force
                // takes it.
                let unsaved = ws.unsaved_in_worktree(&wt.path);
                if !unsaved.is_empty() {
                    ws.reply(r, failed(format!("Not removed: {} {} changes in Glasscode's editor that are not saved. The person saves or discards them first.", unsaved.join(", "), if unsaved.len() == 1 { "has" } else { "have" })));
                    return;
                }
                let risky = wt.locked || !check.is_clean();
                if risky && !force {
                    let mut why = Vec::new();
                    if check.changed > 0 {
                        why.push(plural(check.changed, "uncommitted file", "uncommitted files"));
                    }
                    if check.lost_commits > 0 {
                        why.push(plural(check.lost_commits, "commit only there", "commits only there"));
                    }
                    if wt.locked {
                        why.push("it is locked".into());
                    }
                    ws.reply(r, failed(format!("Not removed: it has {}. Commit or push first, or say --force to lose them.", why.join(" and "))));
                    return;
                }
                // What memory keeps of its work (M24), read while its sessions are still there.
                let archive = ws.archive_input(&root, &wt);
                // Its sessions end first: their terminals hold the folder open.
                let doomed: Vec<String> = ws.state.sessions.iter().filter(|s| s.worktree == wt.path).map(|s| s.id.clone()).collect();
                for id in doomed {
                    ws.close_session(&id, window, cx);
                }
                ws.removing.insert(wt.path.clone());
                cx.notify();
                let delete = delete_branch && !wt.is_detached() && (force || check.branch_only_commits == 0);
                let kept_reason = (delete_branch && !delete).then(|| format!("its branch has {} not pushed anywhere, so it stays (use --force to delete it anyway)", plural(check.branch_only_commits, "commit", "commits")));
                let job = wt.clone();
                let task = cx.background_spawn(async move {
                    // The archive comes before the removal; when it fails, nothing is removed
                    // unless --force says to (memory is what the person keeps).
                    if let Some(input) = archive
                        && let Err(err) = glasscode_core::memstore::archive::before_removal(input)
                    {
                        if !force {
                            anyhow::bail!("memory could not archive its work ({err}), so nothing was removed; say --force to remove it anyway");
                        }
                        log::warn!("memory: removing {} without its archive: {err}", job.path.display());
                    }
                    git::remove_worktree(&root, &job, force || risky)?;
                    if delete {
                        git::delete_branch(&root, &job.branch).map(|()| true).or_else(|e| Err(anyhow::anyhow!("removed, but the branch stays: {e}")))
                    } else {
                        Ok(false)
                    }
                });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |ws, cx| {
                        ws.removing.remove(&wt.path);
                        ws.refresh_worktrees(cx);
                        let name = if wt.branch.is_empty() { home_relative(&wt.path) } else { wt.branch.clone() };
                        ws.reply(r, match result {
                            Ok(true) => Reply::ok(format!("Removed the worktree {name} and its branch."), json!({ "path": wt.path })),
                            Ok(false) => Reply::ok(format!("Removed the worktree {name}{}.", kept_reason.map(|k| format!("; {k}")).unwrap_or_default()), json!({ "path": wt.path })),
                            Err(e) => failed(format!("Could not remove {name}: {e:#}")),
                        });
                    });
                })
                .detach();
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn clean_worktrees(&mut self, only: Option<String>, merged: bool, older: Option<i64>, dry: bool, keep_branch: bool, r: Responder, cx: &mut Context<Self>) {
        let mut jobs: Vec<(PathBuf, Worktree)> = Vec::new();
        for p in &self.state.projects {
            if only.as_ref().is_some_and(|o| *o != p.id) {
                continue;
            }
            for wt in self.worktrees.get(&p.id).cloned().unwrap_or_default() {
                // A worktree with text not saved in the editor is in use too (U20).
                let busy = self.state.sessions.iter().any(|s| s.worktree == wt.path) || self.removing.contains(&wt.path) || !self.unsaved_in_worktree(&wt.path).is_empty();
                if !wt.is_main && wt.path != p.root && !busy && !wt.locked {
                    jobs.push((p.root.clone(), wt));
                }
            }
        }
        let env = self.env.clone();
        let own_pid = std::process::id();
        // What memory keeps of each one's work (M24), from the window, before anything goes.
        let mut archives: std::collections::HashMap<PathBuf, glasscode_core::memstore::archive::ArchiveInput> = jobs.iter().filter_map(|(root, wt)| self.archive_input(root, wt).map(|input| (wt.path.clone(), input))).collect();
        let task = cx.background_spawn(async move {
            let programs = git::process_folders();
            let now = glasscode_core::model::now() as i64;
            let mut chosen = Vec::new();
            let mut skipped = Vec::new();
            for (root, wt) in jobs {
                let name = if wt.branch.is_empty() || wt.is_detached() { home_relative(&wt.path) } else { wt.branch.clone() };
                if wt.prunable {
                    chosen.push((root, wt, true));
                    continue;
                }
                let check = git::check_worktree(&wt);
                let in_use = programs.iter().any(|p| (p.dir.starts_with(&wt.path)) && !git::descends_from(&programs, p.pid, own_pid));
                let why = if !check.is_clean() {
                    Some("has work that is nowhere else")
                } else if !check.env_files.is_empty() {
                    Some("has .env files")
                } else if in_use {
                    Some("a program runs in it")
                } else if older.is_some_and(|d| check.last_commit.is_some_and(|t| now - t < d * 86_400)) {
                    Some("changed recently")
                } else if merged && !branch_merged(&root, &wt, &env) {
                    Some("not merged")
                } else {
                    None
                };
                match why {
                    Some(why) => skipped.push(format!("{name} ({why})")),
                    None => chosen.push((root, wt, check.branch_only_commits == 0)),
                }
            }
            if dry {
                return (chosen.into_iter().map(|(_, wt, _)| (wt, Ok("would go".to_string()))).collect::<Vec<_>>(), skipped);
            }
            let results = chosen
                .into_iter()
                .map(|(root, wt, branch_safe)| {
                    if let Some(input) = archives.remove(&wt.path)
                        && let Err(err) = glasscode_core::memstore::archive::before_removal(input)
                    {
                        return (wt, Err(format!("not removed: memory could not archive its work ({err})")));
                    }
                    let result = git::remove_worktree(&root, &wt, false).map_err(|e| e.to_string()).map(|()| {
                        if keep_branch || !branch_safe || wt.is_detached() || wt.branch.is_empty() {
                            "removed".to_string()
                        } else {
                            match git::delete_branch(&root, &wt.branch) {
                                Ok(()) => "removed with its branch".to_string(),
                                Err(_) => "removed (branch kept)".to_string(),
                            }
                        }
                    });
                    (wt, result)
                })
                .collect();
            (results, skipped)
        });
        cx.spawn(async move |this, cx| {
            let (results, skipped) = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.refresh_worktrees(cx);
                let name = |wt: &Worktree| if wt.branch.is_empty() || wt.is_detached() { home_relative(&wt.path) } else { wt.branch.clone() };
                let mut lines: Vec<String> = results.iter().map(|(wt, res)| match res {
                    Ok(note) => format!("  {}: {note}", name(wt)),
                    Err(e) => format!("  {}: could not remove ({e})", name(wt)),
                }).collect();
                let head = if dry {
                    format!("{} would go:", plural(results.len(), "worktree", "worktrees"))
                } else {
                    format!("Removed {}:", plural(results.iter().filter(|(_, r)| r.is_ok()).count(), "worktree", "worktrees"))
                };
                if results.is_empty() {
                    lines.push("  none".into());
                }
                if !skipped.is_empty() {
                    lines.push(format!("Kept {}: {}", plural(skipped.len(), "worktree", "worktrees"), skipped.join(", ")));
                }
                let data: Vec<Value> = results.iter().map(|(wt, res)| json!({ "path": wt.path, "branch": wt.branch, "result": res.as_ref().map_err(|e| e.clone()).map(|s| s.clone()).unwrap_or_else(|e| format!("error: {e}")) })).collect();
                ws.reply(r, Reply::ok(format!("{head}\n{}", lines.join("\n")), json!({ "worktrees": data, "kept": skipped })));
            });
        })
        .detach();
    }

    // ----- accounts ----------------------------------------------------------------------------

    pub(crate) fn accounts_reply(&self) -> Reply {
        let mut lines = Vec::new();
        let mut agents = Vec::new();
        for agent in [AgentKind::Claude, AgentKind::Codex] {
            let default = self.state.defaults.for_agent(agent).cloned();
            let list: Vec<String> = self.state.accounts_for(agent).map(|a| {
                agents.push(json!({ "agent": agent.name().to_lowercase(), "id": a.id, "label": a.label, "plan": a.plan, "default": Some(&a.id) == default.as_ref() }));
                format!("{}{}", a.display(), if Some(&a.id) == default.as_ref() { " (default)" } else { "" })
            }).collect();
            lines.push(format!("{}: {}", agent.name(), if list.is_empty() { "no account".to_string() } else { list.join(", ") }));
        }
        let gh: Vec<Value> = self.github.list.clone().unwrap_or_default().iter().map(|a| json!({ "login": a.login, "default": self.state.github.default.as_deref() == Some(a.login.as_str()), "working": a.ok })).collect();
        let gh_text: Vec<String> = self.github.list.clone().unwrap_or_default().iter().map(|a| format!("{}{}{}", a.login, if self.state.github.default.as_deref() == Some(a.login.as_str()) { " (default)" } else { "" }, if a.ok { "" } else { " (signed out)" })).collect();
        lines.push(format!("GitHub: {}", if gh_text.is_empty() { "gh is not signed in".to_string() } else { gh_text.join(", ") }));
        // Linear's own key and fetch: a board on a GitHub repository is set up without them.
        let from_linear = self.tickets.fetched_from == glasscode_core::model::Provider::Linear;
        let linear = self.tickets.snapshot.as_ref().filter(|_| from_linear).map(|s| format!("{} ({})", s.viewer.name, s.org));
        lines.push(format!("Linear: {}", match (&linear, self.tickets.linear_key) { (Some(who), true) => format!("connected as {who}"), (_, true) => "connected".into(), _ => "not connected".into() }));
        Reply::ok(
            lines.join("\n"),
            json!({ "agents": agents, "github": gh, "linear": { "connected": self.tickets.linear_key, "as": linear } }),
        )
    }
}

/// Whether a worktree's branch is merged: its commits are in the default branch, or its pull
/// request was merged (squash merges leave no common commits).
fn branch_merged(root: &Path, wt: &Worktree, env: &std::collections::HashMap<String, String>) -> bool {
    if wt.is_detached() || wt.branch.is_empty() {
        return false;
    }
    let base = format!("origin/{}", git::default_branch(root));
    let ancestor = sys::command("git").arg("-C").arg(root).args(["merge-base", "--is-ancestor", &wt.branch, &base]).status().is_ok_and(|s| s.success());
    if ancestor {
        return true;
    }
    let Some(repo) = github::repo_slug(root) else { return false };
    let gh = std::env::var("GLASSCODE_GH").unwrap_or_else(|_| "gh".into());
    sys::command(gh)
        .args(["pr", "list", "--repo", &repo, "--head", &wt.branch, "--state", "merged", "--limit", "1", "--json", "number"])
        .envs(env.iter())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains("number"))
}

#[cfg(test)]
mod clone_tests {
    use super::*;

    #[test]
    fn the_addresses_that_are_cloned() {
        for (text, url, folder) in [
            ("https://github.com/acme/app.git", "https://github.com/acme/app.git", "app"),
            ("https://gitlab.example.com/group/sub/app", "https://gitlab.example.com/group/sub/app", "app"),
            ("ssh://git@github.com/acme/app.git", "ssh://git@github.com/acme/app.git", "app"),
            ("git@github.com:acme/app.git", "git@github.com:acme/app.git", "app"),
            ("  acme/app  ", "https://github.com/acme/app.git", "app"),
            ("acme/my.app_2", "https://github.com/acme/my.app_2.git", "my.app_2"),
        ] {
            assert_eq!(clone_source_checked(text), Ok((url.to_string(), folder.to_string())), "{text}");
        }
    }

    #[test]
    fn other_transports_options_and_odd_text_are_refused() {
        for text in [
            "",
            "--upload-pack=touch /tmp/x",
            "-c",
            "ext::sh -c touch% /tmp/pwned",
            "ext::sh",
            "file:///etc",
            "file://C:/Users",
            "git://github.com/acme/app",
            "http://github.com/acme/app",
            "https://",
            "https:///acme/app",
            "https://-oProxyCommand=x/acme/app",
            "ssh://-oProxyCommand=calc/acme",
            "git@-oProxyCommand=calc:acme/app",
            "git@github.com:-u/app",
            "git@github.com",
            "https://github.com/acme/app\n--x",
            "https://github.com/acme/my app",
            "../app",
            r"C:\work\app",
            "/srv/app",
            "acme/../app",
            "-acme/app",
            ".acme/app",
            "acme/.app",
            "a/b/c",
        ] {
            assert!(clone_source_checked(text).is_err(), "{text:?} should be refused");
            assert_eq!(clone_source(text), None, "{text:?}");
        }
        let why = clone_source_checked("ext::sh -c x").unwrap_err();
        assert!(why.contains("space"), "{why}");
        let why = clone_source_checked("file:///etc").unwrap_err();
        assert!(why.contains("file://") && why.contains("refused"), "{why}");
    }

    #[test]
    fn the_folder_name_is_cleaned_never_taken_raw() {
        for (text, folder) in [
            ("app.git", "app"),
            ("acme/app/", "app"),
            ("..", "project"),
            ("...git", "project"),
            ("-rf", "rf"),
            ("--upload-pack", "upload-pack"),
            (".hidden", "hidden"),
            ("a..b", "a.b"),
            ("name.", "name"),
            ("my app", "my-app"),
            (r"a\b", "b"),
            ("x:y", "y"),
            ("con", "con_"),
            ("COM1.git", "COM1_"),
            ("ünï", "ünï"),
            ("", "project"),
        ] {
            assert_eq!(clone_folder_name(text), folder, "{text:?}");
        }
    }
}
/// The folder a project is for: its repository's main checkout (a linked worktree or a subfolder
/// is that repository), else the folder itself. It runs git: never on the main thread from a
/// command.
pub(crate) fn project_root_of(path: &std::path::Path) -> Result<PathBuf, String> {
    match git::main_checkout(path).or_else(|_| git::repo_root(path)) {
        Ok(root) => Ok(root),
        Err(_) if path.is_dir() => Ok(path.canonicalize().unwrap_or_else(|_| path.to_path_buf())),
        Err(_) => Err(format!("{} is not a folder.", path.display())),
    }
}
