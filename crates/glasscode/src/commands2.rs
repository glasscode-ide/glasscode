//! The rest of the `glasscode` command's commands: projects, worktrees, accounts, tickets,
//! Accounts and the window (see `commands.rs` for the board, sessions and pull requests).

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, Window};
use glasscode_core::control::{Args, Kind, Reply, code};
use glasscode_core::git::{self, Worktree};
use glasscode_core::github;
use glasscode_core::linear;
use glasscode_core::model::{AgentKind, Project, new_id};
use glasscode_core::sys;
use serde_json::{Value, json};

use crate::control_ui::{Caller, Plan, Responder};
use crate::workspace::Workspace;

type Planned = Result<Plan, Reply>;

fn usage(text: impl Into<String>) -> Reply {
    Reply::fail(code::USAGE, text)
}

fn failed(text: impl Into<String>) -> Reply {
    Reply::fail(code::FAILED, text)
}

fn plan(kind: Kind, what: impl Into<String>, run: impl FnOnce(&mut Workspace, Responder, &mut Window, &mut Context<Workspace>) + 'static) -> Planned {
    Ok(Plan { kind, what: what.into(), run: Box::new(run) })
}

fn answer(reply: Reply) -> Planned {
    plan(Kind::Read, "read", move |ws, r, _, _| ws.reply(r, reply))
}

fn check(args: &Args, known: &[&str]) -> Result<(), Reply> {
    let unknown = args.unknown(known);
    if unknown.is_empty() {
        return Ok(());
    }
    Err(usage(format!("Unknown option {}. This command takes: {}.", unknown.join(", "), known.iter().map(|k| format!("--{k}")).collect::<Vec<_>>().join(", "))))
}

fn home_relative(path: &Path) -> String {
    let text = path.display().to_string();
    let home = glasscode_core::paths::home().display().to_string();
    match text.strip_prefix(&home) {
        Some(rest) => format!("~{rest}"),
        None => text,
    }
}

fn expand(path: &str, base: Option<&Path>) -> PathBuf {
    if path == "~" {
        return glasscode_core::paths::home();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return glasscode_core::paths::home().join(rest);
    }
    let p = PathBuf::from(path);
    // A relative path is from where the command was run (the agent's shell), not the app's folder.
    match base {
        Some(base) if p.is_relative() => base.join(p),
        _ => p,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "14d", "2w", "30" (days).
fn days(text: &str) -> Option<i64> {
    let t = text.trim().to_lowercase();
    if let Some(n) = t.strip_suffix('w') {
        return n.parse::<i64>().ok().map(|n| n * 7);
    }
    t.trim_end_matches('d').trim_end_matches(" days").parse().ok()
}

/// A git URL or `owner/name` to clone, and the folder name it gets.
pub(crate) fn clone_source(text: &str) -> Option<(String, String)> {
    let t = text.trim();
    let name = |url: &str| url.trim_end_matches('/').trim_end_matches(".git").rsplit(['/', ':']).next().unwrap_or("project").to_string();
    if t.contains("://") || t.starts_with("git@") {
        return Some((t.to_string(), name(t)));
    }
    let parts: Vec<&str> = t.split('/').collect();
    if parts.len() == 2 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c))) && !Path::new(t).exists() {
        return Some((format!("https://github.com/{t}.git"), parts[1].to_string()));
    }
    None
}

/// Clones `url` into the new folder `target` (`project import` and the first run). It blocks.
pub(crate) fn git_clone(url: &str, target: &Path, env: &std::collections::HashMap<String, String>) -> anyhow::Result<()> {
    std::fs::create_dir_all(target.parent().unwrap_or(target))?;
    // Never a password prompt nobody can answer: git's credential helper or nothing.
    let out = sys::command("git").arg("clone").arg(url).arg(target).envs(env.iter()).env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null()).output()?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

impl Workspace {
    // ----- projects --------------------------------------------------------------------------

    pub(crate) fn project_plan(&mut self, caller: &Caller, args: &Args, verb: Option<&str>) -> Planned {
        match verb {
            None | Some("list" | "ls") => {
                check(args, &[])?;
                let mut lines = Vec::new();
                let mut data = Vec::new();
                // Hidden projects (Settings › Projects) are left out of the CLI and Glasscode entirely.
                for p in self.state.visible_projects() {
                    let sessions = self.state.sessions.iter().filter(|s| s.project_id == p.id).count();
                    let worktrees = self.worktrees.get(&p.id).map(|w| w.len()).unwrap_or(0);
                    let git = p.root.join(".git").exists();
                    let account = self.github_account_for(&p.id);
                    lines.push(format!(
                        "{}  {}  · {} · {}{}",
                        p.name,
                        home_relative(&p.root),
                        plural(sessions, "session", "sessions"),
                        if git { plural(worktrees, "worktree", "worktrees") } else { "no git".into() },
                        account.as_ref().map(|a| format!(" · GitHub {a}")).unwrap_or_default()
                    ));
                    data.push(json!({ "id": p.id, "name": p.name, "root": p.root, "git": git, "sessions": sessions, "worktrees": worktrees, "github_account": account, "repo": github::repo_slug(&p.root) }));
                }
                let text = if lines.is_empty() { "No projects yet. `glasscode project import <folder or git URL>` adds one.".to_string() } else { lines.join("\n") };
                answer(Reply::ok(text, Value::Array(data)))
            }
            Some("import" | "add") => {
                check(args, &["name", "into"])?;
                let source = args.rest(2).trim().to_string();
                if source.is_empty() {
                    return Err(usage("project import <folder or git URL> [--name <name>] [--into <folder>]"));
                }
                let folder = expand(&source, caller.cwd.as_deref());
                let name = args.flag("name").map(str::to_string);
                if folder.is_dir() {
                    let shown = home_relative(&folder);
                    return plan(Kind::Import, format!("import the project in {shown}"), move |ws, r, _, cx| {
                        let reply = match ws.add_project_quietly(folder, name, cx) {
                            Ok((id, true)) => Reply::ok(format!("Imported {shown} as {}.", ws.state.project(&id).map(|p| p.name.clone()).unwrap_or_default()), json!({ "id": id })),
                            Ok((id, false)) => Reply::ok(format!("{shown} is a project already ({}).", ws.state.project(&id).map(|p| p.name.clone()).unwrap_or_default()), json!({ "id": id })),
                            Err(e) => failed(e),
                        };
                        ws.reply(r, reply);
                    });
                }
                let Some((url, repo_name)) = clone_source(&source) else {
                    return Err(failed(format!("{source} is neither a folder nor a git URL (or owner/name on GitHub).")));
                };
                let into = match args.flag("into") {
                    Some(dir) => expand(dir, caller.cwd.as_deref()),
                    None => self.projects_folder(),
                };
                let dest = into.join(name.clone().unwrap_or(repo_name));
                if dest.exists() {
                    return Err(failed(format!("{} exists already; import that folder, or choose --name / --into.", home_relative(&dest))));
                }
                let shown = home_relative(&dest);
                let env = self.env.clone();
                plan(Kind::Import, format!("clone {url} into {shown} and import it"), move |_ws, r, _, cx| {
                    let target = dest.clone();
                    let task = cx.background_spawn(async move { git_clone(&url, &target, &env) });
                    cx.spawn(async move |this, cx| {
                        let result = task.await;
                        let _ = this.update(cx, |ws, cx| {
                            let reply = match result.map_err(|e| format!("Could not clone it: {e:#}")).and_then(|()| ws.add_project_quietly(dest, name, cx)) {
                                Ok((id, _)) => Reply::ok(format!("Cloned and imported {shown}."), json!({ "id": id })),
                                Err(e) => failed(e),
                            };
                            ws.reply(r, reply);
                        });
                    })
                    .detach();
                })
            }
            Some("remove" | "rm") => {
                check(args, &[])?;
                let id = self.find_project(args.word(2).ok_or_else(|| usage("project remove <project>"))?)?;
                let name = self.state.project(&id).map(|p| p.name.clone()).unwrap_or_default();
                let sessions = self.state.sessions.iter().filter(|s| s.project_id == id).count();
                plan(Kind::Close, format!("remove the project {name} from Glasscode (closing {}; the folder stays)", plural(sessions, "session", "sessions")), move |ws, r, _, cx| {
                    ws.remove_project(&id, cx);
                    ws.reply(r, Reply::ok(format!("Removed {name} from Glasscode. Its folder and worktrees stay on disk."), json!({ "id": id })));
                })
            }
            Some("set-account" | "account") => {
                check(args, &["github"])?;
                let id = self.find_project(args.word(2).ok_or_else(|| usage("project set-account <project> --github <login>"))?)?;
                let login = args.flag("github").ok_or_else(|| usage("project set-account <project> --github <login> (or --github default)"))?.to_string();
                let known: Vec<String> = self.github.list.as_ref().map(|l| l.iter().map(|a| a.login.clone()).collect()).unwrap_or_default();
                if login != "default" && !known.is_empty() && !known.iter().any(|k| k.eq_ignore_ascii_case(&login)) {
                    return Err(failed(format!("gh is not signed in as {login}. Signed in: {}.", known.join(", "))));
                }
                let name = self.state.project(&id).map(|p| p.name.clone()).unwrap_or_default();
                plan(Kind::Settings, format!("make {name} use the GitHub account {login}"), move |ws, r, _, cx| {
                    if let Some(p) = ws.state.projects.iter_mut().find(|p| p.id == id) {
                        p.github_account = (login != "default").then(|| login.clone());
                    }
                    ws.save();
                    ws.configure_github(cx);
                    ws.reply(r, Reply::ok(format!("New sessions in {name} use {login} on GitHub."), json!({ "id": id })));
                })
            }
            Some(other) => {
                let _ = caller;
                Err(usage(format!("Unknown project command “{other}”. `glasscode help project` lists them.")))
            }
        }
    }

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
        let root = match git::main_checkout(&path).or_else(|_| git::repo_root(&path)) {
            Ok(root) => root,
            Err(_) if path.is_dir() => path.canonicalize().unwrap_or(path.clone()),
            Err(_) => return Err(format!("{} is not a folder.", path.display())),
        };
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

    // ----- worktrees -------------------------------------------------------------------------

    pub(crate) fn worktree_plan(&mut self, caller: &Caller, args: &Args, verb: Option<&str>) -> Planned {
        match verb {
            None | Some("list" | "ls") => {
                check(args, &["project"])?;
                let only = match args.flag("project") {
                    Some(p) => Some(self.find_project(p)?),
                    None => None,
                };
                let mut lines = Vec::new();
                let mut data = Vec::new();
                for p in self.state.visible_projects() {
                    if only.as_ref().is_some_and(|o| *o != p.id) {
                        continue;
                    }
                    lines.push(format!("{} ({})", p.name, home_relative(&p.root)));
                    for (path, branch) in self.project_worktrees(&p.id) {
                        let sessions = self.state.sessions.iter().filter(|s| s.worktree == path).count();
                        let main = path == p.root;
                        let removing = self.removing.contains(&path);
                        lines.push(format!(
                            "  {:<44} {}{}{}{}",
                            if branch.is_empty() { "(no branch)".to_string() } else { branch.clone() },
                            home_relative(&path),
                            if main { "  · main checkout" } else { "" },
                            if sessions > 0 { format!("  · {}", plural(sessions, "session", "sessions")) } else { String::new() },
                            if removing { "  · removing…" } else { "" }
                        ));
                        data.push(json!({ "project": p.name, "path": path, "branch": branch, "main": main, "sessions": sessions }));
                    }
                }
                answer(Reply::ok(if lines.is_empty() { "No projects.".to_string() } else { lines.join("\n") }, Value::Array(data)))
            }
            Some("new" | "add" | "create") => {
                check(args, &["project", "branch", "from"])?;
                let project_id = self.project_for(args, caller)?;
                let branch = args.flag("branch").or(args.word(2)).ok_or_else(|| usage("worktree new --branch <name> [--project <p>] [--from <base>]"))?.trim().replace(' ', "-");
                let project = self.state.project(&project_id).cloned().ok_or_else(|| failed("That project is gone."))?;
                if git::branch_exists(&project.root, &branch) {
                    return Err(failed(format!("Branch {branch} exists already.")));
                }
                let from = args.flag("from").map(str::to_string);
                let fetch = !self.state.prefs.fresh_main_off;
                plan(Kind::Start, format!("make a worktree for the new branch {branch} in {}", project.name), move |_ws, r, _, cx| {
                    let (root, name, b) = (project.root.clone(), project.name.clone(), branch.clone());
                    let task = cx.background_spawn(async move {
                        let base = match from {
                            Some(f) => f,
                            None => git::start_base(&root, fetch).base,
                        };
                        git::add_worktree(&root, &name, &b, &base).map(|path| (path, base))
                    });
                    cx.spawn(async move |this, cx| {
                        let result = task.await;
                        let _ = this.update(cx, |ws, cx| {
                            ws.refresh_worktrees(cx);
                            ws.reply(r, match result {
                                Ok((path, base)) => Reply::ok(format!("Made {} on {branch} (from {base}).", home_relative(&path)), json!({ "path": path, "branch": branch, "base": base })),
                                Err(e) => failed(format!("Could not make the worktree: {e:#}")),
                            });
                        });
                    })
                    .detach();
                })
            }
            Some("rename") => {
                check(args, &["force"])?;
                let word = args.word(2).ok_or_else(|| usage("worktree rename <worktree> <new branch>"))?;
                let new = args.word(3).ok_or_else(|| usage("worktree rename <worktree> <new branch>"))?.trim().replace(' ', "-");
                let (project_id, wt) = self.find_worktree(word, None, caller.cwd.as_deref())?;
                if wt.is_detached() {
                    return Err(failed("That worktree is not on a branch."));
                }
                let root = self.state.project(&project_id).map(|p| p.root.clone()).unwrap_or_default();
                if git::branch_exists(&root, &new) {
                    return Err(failed(format!("Branch {new} exists already.")));
                }
                let force = args.has("force");
                let old = wt.branch.clone();
                plan(Kind::Rename, format!("rename the branch {old} to {new}"), move |_ws, r, _, cx| {
                    let path = wt.path.clone();
                    let (o, n) = (old.clone(), new.clone());
                    let task = cx.background_spawn(async move {
                        if !force && git::branch_was_pushed(&path, &o) {
                            anyhow::bail!("{o} is on GitHub already (a pull request may use it); renaming it there is not done here. Use --force to rename only this copy.");
                        }
                        git::rename_branch(&path, &o, &n)
                    });
                    cx.spawn(async move |this, cx| {
                        let result = task.await;
                        let _ = this.update(cx, |ws, cx| {
                            if result.is_ok() {
                                for s in ws.state.sessions.iter_mut().filter(|s| s.worktree == wt.path) {
                                    s.branch = new.clone();
                                }
                                ws.save();
                                ws.refresh_worktrees(cx);
                            }
                            ws.reply(r, match result {
                                Ok(()) => Reply::ok(format!("Renamed {old} to {new} ({}).", home_relative(&wt.path)), json!({ "path": wt.path, "branch": new })),
                                Err(e) => failed(format!("{e:#}")),
                            });
                        });
                    })
                    .detach();
                })
            }
            Some("remove" | "rm" | "delete") => {
                check(args, &["delete-branch", "force"])?;
                let (project_id, wt) = self.find_worktree(args.word(2).ok_or_else(|| usage("worktree remove <worktree> [--delete-branch] [--force]"))?, None, caller.cwd.as_deref())?;
                let root = self.state.project(&project_id).map(|p| p.root.clone()).unwrap_or_default();
                if wt.is_main || wt.path == root {
                    return Err(failed("That is the project's own folder (the main checkout); it is not removed."));
                }
                let (delete_branch, force) = (args.has("delete-branch"), args.has("force"));
                let sessions = self.state.sessions.iter().filter(|s| s.worktree == wt.path).count();
                let mut what = format!("remove the worktree {}", if wt.branch.is_empty() || wt.is_detached() { home_relative(&wt.path) } else { wt.branch.clone() });
                if delete_branch {
                    what.push_str(" and its branch");
                }
                if sessions > 0 {
                    what.push_str(&format!(" (closing {} there)", plural(sessions, "session", "sessions")));
                }
                plan(Kind::Remove, what, move |ws, r, window, cx| ws.remove_worktree_now(root, wt, delete_branch, force, r, window, cx))
            }
            Some("clean" | "cleanup" | "clean-up") => {
                check(args, &["project", "merged", "older", "dry-run", "keep-branch"])?;
                let only = match args.flag("project") {
                    Some(p) => Some(self.find_project(p)?),
                    None => caller.session.as_deref().and_then(|s| self.state.session(s)).map(|s| s.project_id.clone()),
                };
                let older = match args.flag("older") {
                    Some(t) => Some(days(t).ok_or_else(|| usage(format!("--older takes days: 14d, 2w (not {t}).")))?),
                    None => None,
                };
                let (merged, dry, keep_branch) = (args.has("merged"), args.has("dry-run"), args.has("keep-branch"));
                let scope = only.as_ref().and_then(|id| self.state.project(id)).map(|p| p.name.clone()).unwrap_or_else(|| "every project".into());
                let mut filter = Vec::new();
                if merged {
                    filter.push("merged".to_string());
                }
                if let Some(d) = older {
                    filter.push(format!("untouched for {d} days"));
                }
                let what = format!("remove the worktrees in {scope} that have nothing to lose{}", if filter.is_empty() { String::new() } else { format!(" ({})", filter.join(", ")) });
                plan(if dry { Kind::Read } else { Kind::Remove }, what, move |ws, r, _, cx| ws.clean_worktrees(only, merged, older, dry, keep_branch, r, cx))
            }
            Some("open") => {
                check(args, &[])?;
                let (_, wt) = self.find_worktree(args.word(2).ok_or_else(|| usage("worktree open <worktree>"))?, None, caller.cwd.as_deref())?;
                let shown = home_relative(&wt.path);
                plan(Kind::Show, format!("open {shown} in your editor"), move |ws, r, _, _| {
                    let result = match ws.preferred_editor().map(|(_, program)| program) {
                        Some(program) => sys::command(program).arg(&wt.path).envs(ws.env.iter()).spawn().map(|_| ()),
                        None => sys::open_path(&wt.path),
                    };
                    ws.reply(r, match result {
                        Ok(()) => Reply::ok(format!("Opened {shown}."), Value::Null),
                        Err(e) => failed(format!("Could not open an editor: {e}")),
                    });
                })
            }
            Some(other) => Err(usage(format!("Unknown worktree command “{other}”. `glasscode help worktree` lists them."))),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn remove_worktree_now(&mut self, root: PathBuf, wt: Worktree, delete_branch: bool, force: bool, r: Responder, window: &mut Window, cx: &mut Context<Self>) {
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
    fn clean_worktrees(&mut self, only: Option<String>, merged: bool, older: Option<i64>, dry: bool, keep_branch: bool, r: Responder, cx: &mut Context<Self>) {
        let mut jobs: Vec<(PathBuf, Worktree)> = Vec::new();
        for p in &self.state.projects {
            if only.as_ref().is_some_and(|o| *o != p.id) {
                continue;
            }
            for wt in self.worktrees.get(&p.id).cloned().unwrap_or_default() {
                let busy = self.state.sessions.iter().any(|s| s.worktree == wt.path) || self.removing.contains(&wt.path);
                if !wt.is_main && wt.path != p.root && !busy && !wt.locked {
                    jobs.push((p.root.clone(), wt));
                }
            }
        }
        let env = self.env.clone();
        let own_pid = std::process::id();
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

    // ----- accounts and tickets ---------------------------------------------------------------------

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
        let linear = self.tickets.snapshot.as_ref().map(|s| format!("{} ({})", s.viewer.name, s.org));
        lines.push(format!("Linear: {}", match (&linear, self.tickets.connected) { (Some(who), true) => format!("connected as {who}"), (_, true) => "connected".into(), _ => "not connected".into() }));
        Reply::ok(
            lines.join("\n"),
            json!({ "agents": agents, "github": gh, "linear": { "connected": self.tickets.connected, "as": linear } }),
        )
    }

    pub(crate) fn ticket_plan(&mut self, caller: &Caller, args: &Args, verb: Option<&str>) -> Planned {
        let ticket_arg = |index: usize| -> Result<linear::Ticket, Reply> {
            let id = args.word(index).ok_or_else(|| usage(format!("ticket {} <ID>", verb.unwrap_or("show"))))?.to_uppercase();
            self.tickets.ticket(&id).cloned().ok_or_else(|| failed(format!("No ticket {id} among yours in Linear{}.", if self.tickets.connected { "" } else { " (Linear is not connected: Settings › Accounts)" })))
        };
        match verb {
            None | Some("list" | "ls") => {
                check(args, &["review", "all", "mine"])?;
                let Some(snapshot) = self.tickets.snapshot.as_ref() else {
                    return Err(failed(if self.tickets.connected { "Linear has not answered yet; try again in a moment." } else { "Linear is not connected (Settings › Accounts › Linear)." }));
                };
                let list: Vec<&linear::Ticket> = if args.has("review") { self.tickets.reviewing() } else { snapshot.mine.iter().collect() };
                let open: Vec<&linear::Ticket> = list.into_iter().filter(|t| args.has("all") || !t.is_done()).collect();
                let text = if open.is_empty() {
                    "No tickets.".to_string()
                } else {
                    open.iter().map(|t| format!("{:<8} {:<12} {}{}", t.identifier, t.status.name, t.title, t.due.as_deref().map(|d| format!("  · due {}", linear::short_date(d))).unwrap_or_default())).collect::<Vec<_>>().join("\n")
                };
                answer(Reply::ok(text, serde_json::to_value(&open).unwrap_or_default()))
            }
            Some("show" | "read" | "view") => {
                check(args, &[])?;
                let ticket = ticket_arg(2)?;
                plan(Kind::Read, "read", move |_ws, r, _, cx| {
                    let id = ticket.id.clone();
                    let task = cx.background_spawn(async move { linear::Client::saved().ok_or_else(|| anyhow::anyhow!("Linear is not connected"))?.detail(&id) });
                    cx.spawn(async move |this, cx| {
                        let detail = task.await;
                        let _ = this.update(cx, |ws, _| {
                            let mut text = format!("{} {}\n{} · {}{}\n{}", ticket.identifier, ticket.title, ticket.status.name, ticket.priority_name(), ticket.due.as_deref().map(|d| format!(" · due {}", linear::short_date(d))).unwrap_or_default(), ticket.url);
                            let mut data = serde_json::to_value(&ticket).unwrap_or_default();
                            match detail {
                                Ok(d) => {
                                    if !d.description.trim().is_empty() {
                                        text.push_str(&format!("\n\n{}", linear::markdown(&d.description).trim()));
                                    }
                                    for c in &d.comments {
                                        text.push_str(&format!("\n\n{} ({}): {}", c.author, linear::short_date(&c.created_at), c.body.trim()));
                                    }
                                    data["description"] = json!(d.description);
                                    data["comments"] = json!(d.comments.iter().map(|c| json!({ "author": c.author, "body": c.body, "at": c.created_at })).collect::<Vec<_>>());
                                }
                                Err(e) => text.push_str(&format!("\n\n(Its description and comments could not be read: {e:#})")),
                            }
                            ws.reply(r, Reply::ok(text, data));
                        });
                    })
                    .detach();
                })
            }
            Some("start" | "work") => {
                check(args, &["project", "agent", "account", "message", "open", "model", "mode", "branch", "title"])?;
                let ticket = ticket_arg(2)?;
                let mut spec = self.new_spec(caller, args)?;
                spec.ticket = Some(ticket);
                self.new_session_plan(spec)
            }
            Some("move" | "status") => {
                check(args, &["to"])?;
                let ticket = ticket_arg(2)?;
                let to = args.flag("to").map(str::to_string).unwrap_or_else(|| args.rest(3)).trim().to_string();
                if to.is_empty() {
                    return Err(usage("ticket move <ID> \"<status>\", e.g. \"In Review\""));
                }
                plan(Kind::Tickets, format!("move {} to “{to}” in Linear", ticket.identifier), move |_ws, r, _, cx| {
                    let t = ticket.clone();
                    let wanted = to.clone();
                    let task = cx.background_spawn(async move {
                        let client = linear::Client::saved().ok_or_else(|| anyhow::anyhow!("Linear is not connected"))?;
                        let detail = client.detail(&t.id)?;
                        let (state_id, status) = detail.states.iter().find(|(_, st)| st.name.eq_ignore_ascii_case(&wanted)).cloned().ok_or_else(|| {
                            anyhow::anyhow!("its team has no “{wanted}” status (it has: {})", detail.states.iter().map(|(_, s)| s.name.clone()).collect::<Vec<_>>().join(", "))
                        })?;
                        client.set_status(&t.id, &state_id)?;
                        Ok::<_, anyhow::Error>(status)
                    });
                    cx.spawn(async move |this, cx| {
                        let result = task.await;
                        let _ = this.update(cx, |ws, cx| {
                            let reply = match result {
                                Ok(status) => {
                                    let name = status.name.clone();
                                    ws.tickets_set_status(&ticket.identifier, status);
                                    ws.tickets.last_fetch = None;
                                    ws.tickets_refresh(cx);
                                    Reply::ok(format!("{} is {name} in Linear.", ticket.identifier), json!({ "ticket": ticket.identifier, "status": name }))
                                }
                                Err(e) => failed(format!("Could not move {}: {e:#}", ticket.identifier)),
                            };
                            ws.reply(r, reply);
                        });
                    })
                    .detach();
                })
            }
            Some("comment") => {
                check(args, &[])?;
                let ticket = ticket_arg(2)?;
                let body = args.rest(3).trim().to_string();
                if body.is_empty() {
                    return Err(usage("ticket comment <ID> \"<text>\""));
                }
                let preview: String = body.chars().take(80).collect();
                plan(Kind::Tickets, format!("comment on {} in Linear: “{preview}”", ticket.identifier), move |_ws, r, _, cx| {
                    let id = ticket.id.clone();
                    let text = body.clone();
                    let task = cx.background_spawn(async move { linear::Client::saved().ok_or_else(|| anyhow::anyhow!("Linear is not connected"))?.comment(&id, &text) });
                    cx.spawn(async move |this, cx| {
                        let result = task.await;
                        let _ = this.update(cx, |ws, _| ws.reply(r, match result {
                            Ok(()) => Reply::ok(format!("Commented on {}.", ticket.identifier), json!({ "ticket": ticket.identifier })),
                            Err(e) => failed(format!("Could not comment: {e:#}")),
                        }));
                    })
                    .detach();
                })
            }
            Some(other) => Err(usage(format!("Unknown ticket command “{other}”. `glasscode help ticket` lists them."))),
        }
    }

    pub(crate) fn open_plan(&mut self, caller: &Caller, args: &Args) -> Planned {
        check(args, &[])?;
        let what = args.word(1).ok_or_else(|| usage("open today|tickets|sessions|usage|settings|themes|<session>"))?.to_lowercase();
        use crate::shell::View;
        // The words of the command stay as they were: "board" is the Today board, "themes" the
        // Appearance section of Settings.
        let view = match what.as_str() {
            "today" | "board" => Some((View::Today, false)),
            "tickets" | "linear" => Some((View::Board, false)),
            "sessions" => Some((View::Sessions, false)),
            "usage" => Some((View::Usage, false)),
            "settings" => Some((View::Settings, false)),
            "themes" => Some((View::Settings, true)),
            _ => None,
        };
        match view {
            Some((view, themes)) => plan(Kind::Show, format!("show {what} in the window"), move |ws, r, window, cx| {
                match view {
                    View::Settings if themes => ws.open_appearance(cx),
                    View::Board => ws.show_tickets(cx),
                    View::Sessions => ws.show_sessions(window, cx),
                    View::Settings => ws.open_settings(crate::settings_ui::Section::General, window, cx),
                    View::Today => ws.show_today(cx),
                    other => ws.show_view(other, cx),
                }
                ws.reply(r, Reply::ok(format!("Showing {what}."), Value::Null));
            }),
            None => {
                let id = self.find_session(&what, caller)?;
                let title = self.state.session(&id).map(|s| self.session_title(s)).unwrap_or_default();
                plan(Kind::Show, format!("show the session “{title}” in the window"), move |ws, r, window, cx| {
                    ws.open_session(&id, window, cx);
                    ws.reply(r, Reply::ok(format!("Showing “{title}”."), json!({ "id": id })));
                })
            }
        }
    }
}

impl Workspace {
    /// Changing a pull request on GitHub: re-run checks, comment, labels, a draft PR.
    pub(crate) fn pr_change_plan(&mut self, caller: &Caller, args: &Args, verb: &str) -> Planned {
        if matches!(verb, "open-draft" | "draft") {
            check(args, &["base"])?;
            let id = self.find_session(args.word(2).unwrap_or("this"), caller)?;
            let s = self.state.session(&id).cloned().ok_or_else(|| failed("That session is gone."))?;
            let root = self.state.project(&s.project_id).map(|p| p.root.clone()).unwrap_or_default();
            let base = args.flag("base").map(str::to_string).unwrap_or_else(|| git::default_branch(&root));
            let mut env = (*self.env).clone();
            env.extend(self.github_env(&s.project_id));
            let title = self.session_title(&s);
            return plan(Kind::Github, format!("push {} and open a draft pull request for “{title}”", s.branch), move |_ws, r, _, cx| {
                let (dir, branch) = (s.worktree.clone(), s.branch.clone());
                let task = cx.background_spawn(async move { github::push_and_open_draft(&dir, &branch, &base, &env) });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |ws, cx| {
                        ws.pr_refresh(cx);
                        ws.reply(r, match result {
                            Ok(()) => Reply::ok(format!("Pushed {} and opened a draft pull request.", s.branch), json!({ "branch": s.branch })),
                            Err(e) => failed(format!("Could not open the draft: {e:#}")),
                        });
                    });
                })
                .detach();
            });
        }
        let n: u64 = args.word(2).and_then(|w| w.trim_start_matches('#').parse().ok()).ok_or_else(|| usage(format!("pr {verb} <number> …")))?;
        let project_id = self.project_for(args, caller)?;
        let root = self.state.project(&project_id).map(|p| p.root.clone()).unwrap_or_default();
        let repo = github::repo_slug(&root).ok_or_else(|| failed("This project's origin is not on GitHub."))?;
        let mut env = (*self.env).clone();
        env.extend(self.github_env(&project_id));
        let run_gh = |what: String, ok: String, job: Box<dyn FnOnce(&str, &std::collections::HashMap<String, String>) -> anyhow::Result<()> + Send>| {
            let (repo, env) = (repo.clone(), env.clone());
            plan(Kind::Github, what, move |_ws, r, _, cx| {
                let task = cx.background_spawn(async move { job(&repo, &env) });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |ws, _| ws.reply(r, match result {
                        Ok(()) => Reply::ok(ok, Value::Null),
                        Err(e) => failed(format!("GitHub said no: {e:#}")),
                    }));
                })
                .detach();
            })
        };
        match verb {
            "rerun" => {
                check(args, &["project"])?;
                let root = root.clone();
                run_gh(format!("re-run the failed checks of #{n}"), format!("The failed checks of #{n} run again."), Box::new(move |repo, env| {
                    let details = github::pr_details(repo, n, &github::PreviewConfig::load(&root), env)?;
                    let mut runs: Vec<u64> = details.checks.iter().filter(|c| c.state() == github::CheckState::Failed).filter_map(|c| c.run_id()).collect();
                    runs.sort_unstable();
                    runs.dedup();
                    if runs.is_empty() {
                        anyhow::bail!("#{n} has no failed checks to re-run");
                    }
                    github::rerun_failed(repo, &runs, env)
                }))
            }
            "comment" => {
                check(args, &["project"])?;
                let body = args.rest(3).trim().to_string();
                if body.is_empty() {
                    return Err(usage("pr comment <number> \"<text>\""));
                }
                let preview: String = body.chars().take(80).collect();
                run_gh(format!("comment on #{n}: “{preview}”"), format!("Commented on #{n}."), Box::new(move |repo, env| github::comment(repo, n, &body, env)))
            }
            _ => {
                check(args, &["project", "add", "remove"])?;
                let (label, on) = match (args.flag("add"), args.flag("remove")) {
                    (Some(l), None) => (l.to_string(), true),
                    (None, Some(l)) => (l.to_string(), false),
                    _ => return Err(usage("pr label <number> --add <label> | --remove <label>")),
                };
                let what = if on { format!("add the label {label} to #{n}") } else { format!("take the label {label} off #{n}") };
                let ok = if on { format!("#{n} has the label {label}.") } else { format!("Took {label} off #{n}.") };
                run_gh(what, ok, Box::new(move |repo, env| github::set_label(repo, n, &label, on, env)))
            }
        }
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
