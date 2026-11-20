//! Git operations, done by running the `git` command line tool.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, bail};

fn git(dir: &Path) -> Command {
    let mut cmd = crate::sys::command("git");
    cmd.arg("-C").arg(dir);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    cmd
}

/// `git` for something that talks to GitHub (fetch, pull, push): as the repository's account.
fn git_remote(dir: &Path) -> Command {
    let mut cmd = git(dir);
    cmd.envs(crate::github_accounts::env_for_dir(dir));
    cmd
}

fn run(mut cmd: Command) -> anyhow::Result<String> {
    let output = cmd.output().context("could not run git")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The top-level directory of the repository containing `path`.
pub fn repo_root(path: &Path) -> anyhow::Result<PathBuf> {
    let mut cmd = git(path);
    cmd.args(["rev-parse", "--show-toplevel"]);
    Ok(PathBuf::from(run(cmd)?.trim()))
}

/// The main checkout for a path that may be inside a linked worktree.
pub fn main_checkout(path: &Path) -> anyhow::Result<PathBuf> {
    let mut cmd = git(path);
    cmd.args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let common = PathBuf::from(run(cmd)?.trim());
    Ok(common.parent().map(Path::to_path_buf).unwrap_or(common))
}

pub fn current_branch(path: &Path) -> String {
    let mut cmd = git(path);
    cmd.args(["rev-parse", "--abbrev-ref", "HEAD"]);
    run(cmd)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "HEAD".into())
}

pub fn default_branch(root: &Path) -> String {
    let mut cmd = git(root);
    cmd.args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]);
    if let Ok(name) = run(cmd) {
        if let Some(branch) = name.trim().strip_prefix("origin/") {
            return branch.to_string();
        }
    }
    current_branch(root)
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Worktree {
    pub path: PathBuf,
    /// Branch name, or "detached" when HEAD is not on a branch.
    pub branch: String,
    pub is_main: bool,
    /// Commit checked out.
    pub head: String,
    /// Locked with `git worktree lock`; git refuses to remove it without a double force.
    pub locked: bool,
    /// The folder is gone; only git's bookkeeping is left.
    pub prunable: bool,
}

impl Worktree {
    pub fn is_detached(&self) -> bool {
        self.branch == "detached"
    }

    /// A name for lists: the branch, or for a detached HEAD the worktree's folder name
    /// (the parent folder when the last one is just the repository's name).
    pub fn label(&self, repo_name: &str) -> String {
        if !self.is_detached() {
            return self.branch.clone();
        }
        folder_label(&self.path, repo_name)
    }
}

pub fn folder_label(path: &Path, repo_name: &str) -> String {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let last = name(path);
    if last == repo_name {
        if let Some(parent) = path.parent() {
            return name(parent);
        }
    }
    last
}

pub fn worktrees(root: &Path) -> anyhow::Result<Vec<Worktree>> {
    let mut cmd = git(root);
    cmd.args(["worktree", "list", "--porcelain"]);
    let out = run(cmd)?;
    Ok(parse_worktrees(&out))
}

fn parse_worktrees(out: &str) -> Vec<Worktree> {
    let mut list = Vec::new();
    let mut current: Option<Worktree> = None;
    for line in out.lines().chain(std::iter::once("")) {
        if let Some(p) = line.strip_prefix("worktree ") {
            current = Some(Worktree { path: PathBuf::from(p), ..Default::default() });
        } else if let Some(wt) = current.as_mut() {
            if let Some(b) = line.strip_prefix("branch ") {
                wt.branch = b.trim_start_matches("refs/heads/").to_string();
            } else if let Some(h) = line.strip_prefix("HEAD ") {
                wt.head = h.to_string();
            } else if line == "detached" {
                wt.branch = "detached".into();
            } else if line == "locked" || line.starts_with("locked ") {
                wt.locked = true;
            } else if line == "prunable" || line.starts_with("prunable ") {
                wt.prunable = true;
            } else if line.is_empty() {
                let mut wt = current.take().unwrap_or_default();
                wt.is_main = list.is_empty();
                list.push(wt);
            }
        }
    }
    list
}

pub fn branch_exists(root: &Path, branch: &str) -> bool {
    let mut cmd = git(root);
    cmd.args(["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")]);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// A folder name that is safe on disk for a branch name.
pub fn slug(branch: &str) -> String {
    let slug: String = branch
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' })
        .collect();
    slug.trim_matches('-').to_string()
}

/// Create a worktree for `branch` under Glasscode's worktree folder and return its path.
/// The branch is created from `base` when it does not exist yet. The project's
/// `.glasscode/settings.json` is read here, once: its `worktree_folder` names the folder, and its
/// setup script waits for the first session in the worktree (`project_settings::take_new_worktree`).
pub fn add_worktree(root: &Path, project_name: &str, branch: &str, base: &str) -> anyhow::Result<PathBuf> {
    add_worktree_with(root, project_name, branch, base, crate::project_settings::load(root))
}

/// [`add_worktree`] with the project's file as the caller read it, so that one read names the
/// branch, the folder and the setup.
pub fn add_worktree_with(root: &Path, project_name: &str, branch: &str, base: &str, settings: crate::project_settings::Loaded) -> anyhow::Result<PathBuf> {
    let branch = branch.trim();
    if branch.is_empty() {
        bail!("branch name is empty");
    }
    let worktrees = crate::paths::worktrees_dir();
    let dir = crate::project_settings::worktree_dir(&worktrees, project_name, branch, settings.settings.worktree_folder.as_deref());
    make_parents_below(&worktrees, &dir)?;
    let mut cmd = git(root);
    cmd.arg("worktree").arg("add");
    if branch_exists(root, branch) {
        cmd.arg(&dir).arg(branch);
    } else {
        // Not tracking `origin/main`: the branch's own `git push -u` sets where it goes.
        cmd.arg("--no-track").arg("-b").arg(branch).arg(&dir).arg(base);
    }
    run(cmd)?;
    // What git made: a plain folder, still below the worktree folder.
    check_below(&worktrees, &dir).with_context(|| format!("git made {}, but it is not a plain folder below {}", dir.display(), worktrees.display()))?;
    crate::project_settings::remember_new_worktree(&dir, root, settings);
    Ok(dir)
}

/// Make the folders between `base` (Glasscode's worktree folder) and `dir`'s parent, for a new
/// worktree at `dir`, so that nothing is made outside `base`: a template's folders are plain
/// names, but a link or a junction already inside `base` (made by hand, or left by a checkout)
/// could lead out of it. Each folder that exists is looked at without following it and must be
/// a plain folder; each missing one is made alone and looked at again; then the real path must
/// be below `base`. `dir` itself must not exist in any form. Any error is a refusal. `base`
/// itself is the person's own and may be a link.
pub fn make_parents_below(base: &Path, dir: &Path) -> anyhow::Result<()> {
    let rel = dir.strip_prefix(base).map_err(|_| anyhow::anyhow!("{} is not below {}", dir.display(), base.display()))?;
    if !crate::project_settings::stays_below(rel) {
        bail!("{} leaves {}", dir.display(), base.display());
    }
    std::fs::create_dir_all(base).with_context(|| format!("could not make {}", base.display()))?;
    let parts: Vec<_> = rel.components().collect();
    let mut at = base.to_path_buf();
    for part in &parts[..parts.len() - 1] {
        at.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) => plain_folder(&at, &meta)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&at).with_context(|| format!("could not make {}", at.display()))?;
                let meta = std::fs::symlink_metadata(&at).with_context(|| format!("could not look at {}", at.display()))?;
                plain_folder(&at, &meta)?;
            }
            Err(e) => return Err(anyhow::anyhow!(e).context(format!("could not look at {}", at.display()))),
        }
    }
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => bail!("{} already exists", dir.display()),
        Err(e) => return Err(anyhow::anyhow!(e).context(format!("could not look at {}", dir.display()))),
    }
    let (made, below) = (at.canonicalize()?, base.canonicalize()?);
    if !made.starts_with(&below) {
        bail!("{} leads outside {}", at.display(), base.display());
    }
    Ok(())
}

/// The folder at `dir` and each folder between it and `base` are plain folders (no link, no
/// junction), and its real path is below `base`.
fn check_below(base: &Path, dir: &Path) -> anyhow::Result<()> {
    let rel = dir.strip_prefix(base).map_err(|_| anyhow::anyhow!("{} is not below {}", dir.display(), base.display()))?;
    let mut at = base.to_path_buf();
    for part in rel.components() {
        at.push(part);
        plain_folder(&at, &std::fs::symlink_metadata(&at)?)?;
    }
    if !dir.canonicalize()?.starts_with(base.canonicalize()?) {
        bail!("{} leads outside {}", dir.display(), base.display());
    }
    Ok(())
}

fn plain_folder(path: &Path, meta: &std::fs::Metadata) -> anyhow::Result<()> {
    if crate::sys::is_link(meta) {
        bail!("{} is a link or a junction; a new worktree does not go through one", path.display());
    }
    if !meta.is_dir() {
        bail!("{} is not a folder", path.display());
    }
    Ok(())
}

/// Where a new branch starts: the remote's default branch, fetched just now.
#[derive(Clone, Debug, PartialEq)]
pub struct FreshBase {
    /// What to branch from: `origin/main`, or the local `main` when there is no remote copy.
    pub base: String,
    /// The default branch's name (`main`).
    pub branch: String,
    /// The fetch worked; otherwise `base` is whatever was there before (offline, no access).
    pub fetched: bool,
    /// Unix time of `base`'s newest commit.
    pub newest_commit: Option<i64>,
}

/// Fetch the default branch and say where new work should start. A project folder's own
/// `main` is often days behind, and it may not even be checked out on `main`.
pub fn fresh_base(root: &Path) -> FreshBase {
    let branch = default_branch(root);
    let fetched = fetch_branch(root, &branch);
    let remote = format!("origin/{branch}");
    let base = if rev(root, &format!("refs/remotes/{remote}")).is_some() { remote } else { branch.clone() };
    let newest_commit = commit_time(root, &base);
    FreshBase { base, branch, fetched, newest_commit }
}

/// Where new work starts without fetching (Settings › "Start new work from the latest main"
/// off): the local default branch, else the remote copy last fetched.
pub fn local_base(root: &Path) -> FreshBase {
    let branch = default_branch(root);
    let base = if rev(root, &format!("refs/heads/{branch}")).is_some() { branch.clone() } else { format!("origin/{branch}") };
    let newest_commit = commit_time(root, &base);
    FreshBase { base, branch, fetched: false, newest_commit }
}

/// `fresh_base` or `local_base`, as Settings says.
pub fn start_base(root: &Path, fetch: bool) -> FreshBase {
    if fetch { fresh_base(root) } else { local_base(root) }
}

/// `git fetch origin <branch>` (one minute at most); false when offline or not allowed.
pub fn fetch_branch(root: &Path, branch: &str) -> bool {
    let mut cmd = git_remote(root);
    cmd.args(["fetch", "--quiet", "origin", branch]);
    crate::github::output_within(cmd, std::time::Duration::from_secs(60)).is_ok_and(|o| o.status.success())
}

fn rev(dir: &Path, name: &str) -> Option<String> {
    let mut cmd = git(dir);
    cmd.args(["rev-parse", "--verify", "--quiet", &format!("{name}^{{commit}}")]);
    run(cmd).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Unix time of a revision's commit.
pub fn commit_time(dir: &Path, name: &str) -> Option<i64> {
    let mut cmd = git(dir);
    cmd.args(["log", "-1", "--format=%ct", name, "--"]);
    run(cmd).ok()?.trim().parse().ok()
}

/// A pull request checked out in a worktree, and what happened to it.
#[derive(Clone, Debug, PartialEq)]
pub struct PrCheckout {
    pub path: PathBuf,
    pub branch: String,
    /// For people: "Updated to the PR's latest commit", "Kept your local changes; …".
    pub note: String,
}

/// Fetch a pull request's head into `refs/glasscode/pr/<n>` (forks included) and return it.
pub fn fetch_pr(root: &Path, number: u64) -> anyhow::Result<String> {
    let mut cmd = git_remote(root);
    cmd.args(["fetch", "--quiet", "origin", &format!("+refs/pull/{number}/head:refs/glasscode/pr/{number}")]);
    let out = crate::github::output_within(cmd, std::time::Duration::from_secs(120))?;
    if !out.status.success() {
        bail!("could not fetch PR #{number}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    rev(root, &format!("refs/glasscode/pr/{number}")).ok_or_else(|| anyhow::anyhow!("PR #{number} has no head"))
}

/// Bring a checkout of `branch` up to `target` without losing anything: a fast-forward, or
/// (when nothing local would be lost) a jump to `target`; otherwise leave it and say so.
fn bring_up_to(dir: &Path, target: &str, previous: Option<&str>, what: &str) -> anyhow::Result<String> {
    let Some(current) = rev(dir, "HEAD") else { bail!("{} has no commit checked out", dir.display()) };
    let target_sha = rev(dir, target).ok_or_else(|| anyhow::anyhow!("{target} is missing"))?;
    if current == target_sha {
        return Ok(format!("Up to date with {what}."));
    }
    let clean = status(dir).map(|c| c.is_empty()).unwrap_or(false);
    let mut ancestor = git(dir);
    ancestor.args(["merge-base", "--is-ancestor", "HEAD", target]);
    let fast_forward = ancestor.status().is_ok_and(|s| s.success());
    if fast_forward && clean {
        let mut cmd = git(dir);
        cmd.args(["merge", "--ff-only", "--quiet", target]);
        run(cmd)?;
        return Ok(format!("Updated to {what}'s latest commit."));
    }
    // Force-pushed, but this checkout still is exactly what we fetched last time: nothing of
    // the person's would be lost by moving it.
    if clean && previous == Some(current.as_str()) {
        let mut cmd = git(dir);
        cmd.args(["reset", "--hard", "--quiet", target]);
        run(cmd)?;
        return Ok(format!("Updated to {what}'s latest commit (it was force-pushed)."));
    }
    Ok(format!("Kept your local changes; {what} has newer commits (git merge {target})."))
}

/// A folder for a new worktree that is not taken yet.
fn free_dir(project_name: &str, name: &str) -> PathBuf {
    let parent = crate::paths::worktrees_dir().join(slug(project_name));
    let mut dir = parent.join(slug(name));
    let mut n = 2;
    while dir.exists() {
        dir = parent.join(format!("{}-{n}", slug(name)));
        n += 1;
    }
    dir
}

/// A pull request's own branch in a worktree, at its latest commit: to carry on with it.
/// Same-repository PRs use their branch (so pushing updates the PR); a fork's PR gets a local
/// `pr/<n>` branch.
pub fn checkout_pr_branch(root: &Path, project_name: &str, number: u64, head: &str, same_repo: bool) -> anyhow::Result<PrCheckout> {
    let (branch, target, previous) = if same_repo {
        let previous = rev(root, &format!("refs/remotes/origin/{head}"));
        let mut cmd = git_remote(root);
        cmd.args(["fetch", "--quiet", "origin", &format!("+refs/heads/{head}:refs/remotes/origin/{head}")]);
        let out = crate::github::output_within(cmd, std::time::Duration::from_secs(120))?;
        if !out.status.success() {
            bail!("could not fetch {head}: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        (head.to_string(), format!("origin/{head}"), previous)
    } else {
        let previous = rev(root, &format!("refs/glasscode/pr/{number}"));
        fetch_pr(root, number)?;
        (format!("pr/{number}"), format!("refs/glasscode/pr/{number}"), previous)
    };
    // Already checked out somewhere (the project folder, an earlier worktree): use that one.
    if let Some(wt) = worktrees(root)?.into_iter().find(|w| w.branch == branch && w.path.is_dir()) {
        let note = bring_up_to(&wt.path, &target, previous.as_deref(), &format!("PR #{number}"))?;
        return Ok(PrCheckout { path: wt.path, branch, note });
    }
    let dir = free_dir(project_name, &branch);
    std::fs::create_dir_all(dir.parent().unwrap_or(&dir))?;
    let mut cmd = git(root);
    cmd.arg("worktree").arg("add").arg("--quiet");
    let existed = branch_exists(root, &branch);
    if existed {
        cmd.arg(&dir).arg(&branch);
    } else if same_repo {
        cmd.args(["--track", "-b", &branch]).arg(&dir).arg(&target);
    } else {
        cmd.args(["-b", &branch]).arg(&dir).arg(&target);
    }
    run(cmd)?;
    let note = if existed {
        bring_up_to(&dir, &target, previous.as_deref(), &format!("PR #{number}"))?
    } else {
        format!("Checked out PR #{number} at its latest commit.")
    };
    Ok(PrCheckout { path: dir, branch, note })
}

/// A pull request to review, at its latest commit, in `review/<n>`. Refreshed when the author
/// pushes again; never pushed from (its push remote does not exist).
pub fn checkout_review(root: &Path, project_name: &str, number: u64) -> anyhow::Result<PrCheckout> {
    let branch = format!("review/{number}");
    let target = format!("refs/glasscode/pr/{number}");
    let previous = rev(root, &target);
    fetch_pr(root, number)?;
    let checkout = match worktrees(root)?.into_iter().find(|w| w.branch == branch && w.path.is_dir()) {
        Some(wt) => {
            let note = bring_up_to(&wt.path, &target, previous.as_deref(), &format!("PR #{number}"))?;
            PrCheckout { path: wt.path, branch: branch.clone(), note }
        }
        None => {
            let dir = free_dir(project_name, &format!("review-{number}"));
            std::fs::create_dir_all(dir.parent().unwrap_or(&dir))?;
            // The branch is Glasscode's own: start it again at the PR's head.
            let mut cmd = git(root);
            cmd.args(["worktree", "add", "--quiet", "-B", &branch]).arg(&dir).arg(&target);
            run(cmd)?;
            PrCheckout { path: dir, branch: branch.clone(), note: format!("Checked out PR #{number} at its latest commit.") }
        }
    };
    let mut cmd = git(root);
    cmd.args(["config", &format!("branch.{branch}.pushRemote"), "review-only-do-not-push"]);
    let _ = run(cmd);
    exclude(&checkout.path, "/.glasscode/")?;
    Ok(checkout)
}

/// Keep a pattern out of git in this repository (`info/exclude`, shared by its worktrees).
/// Whether git tracks `path` (so a request file committed to a repository is never run).
pub fn is_tracked(dir: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(dir) else { return false };
    let mut cmd = git(dir);
    cmd.args(["ls-files", "--error-unmatch", "-z", "--"]).arg(rel);
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

pub fn exclude(dir: &Path, pattern: &str) -> anyhow::Result<()> {
    let mut cmd = git(dir);
    cmd.args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let file = PathBuf::from(run(cmd)?.trim()).join("info").join("exclude");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    if text.lines().any(|l| l.trim() == pattern) {
        return Ok(());
    }
    std::fs::create_dir_all(file.parent().unwrap_or(&file))?;
    let mut text = text;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(pattern);
    text.push('\n');
    std::fs::write(&file, text)?;
    Ok(())
}

/// What removing a worktree would throw away.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorktreeCheck {
    /// Modified, staged and untracked files (ignored files do not count).
    pub changed: usize,
    /// Detached HEAD only: commits no branch, tag or remote has. Lost with the worktree.
    pub lost_commits: usize,
    /// Branch only: commits no other branch, tag or remote has. Lost if the branch is deleted.
    pub branch_only_commits: usize,
    /// Unix time of the last commit.
    pub last_commit: Option<i64>,
    /// Ignored local files such as `.env` that are deleted with the folder.
    pub env_files: Vec<String>,
}

impl WorktreeCheck {
    /// Removing the worktree loses nothing.
    pub fn is_clean(&self) -> bool {
        self.changed == 0 && self.lost_commits == 0
    }
}

fn count(cmd: Command) -> usize {
    run(cmd).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

pub fn check_worktree(wt: &Worktree) -> WorktreeCheck {
    let mut check = WorktreeCheck::default();
    if wt.prunable || !wt.path.is_dir() {
        return check;
    }
    let mut cmd = git(&wt.path);
    // Ignored directories (node_modules, target) are listed once, not walked.
    cmd.args(["status", "--porcelain", "--untracked-files=normal", "--ignored=traditional"]);
    match run(cmd) {
        Ok(out) => {
            for line in out.lines().filter(|l| l.len() > 3) {
                let (code, path) = line.split_at(3);
                if code != "!! " {
                    check.changed += 1;
                } else if !path.ends_with('/') && path.rsplit('/').next().is_some_and(|name| name.starts_with(".env")) {
                    check.env_files.push(path.to_string());
                }
            }
        }
        // A worktree git cannot read counts as changed, so it is never removed silently.
        Err(_) => check.changed = 1,
    }
    let mut cmd = git(&wt.path);
    if wt.is_detached() {
        cmd.args(["rev-list", "--count", "HEAD", "--not", "--branches", "--tags", "--remotes"]);
        check.lost_commits = count(cmd);
    } else {
        // `--exclude` takes the bare branch name when it applies to `--branches`.
        cmd.args(["rev-list", "--count"])
            .arg(format!("refs/heads/{}", wt.branch))
            .args(["--not", &format!("--exclude={}", wt.branch), "--branches", "--tags", "--remotes"]);
        check.branch_only_commits = count(cmd);
    }
    let mut cmd = git(&wt.path);
    cmd.args(["log", "-1", "--format=%ct", "HEAD"]);
    check.last_commit = run(cmd).ok().and_then(|s| s.trim().parse().ok());
    check
}

/// A running program and its working folder (from `lsof`, or the process list on Windows).
#[derive(Clone, Debug)]
pub struct ProcessFolder {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
    pub dir: PathBuf,
}

/// Working folders of running programs. Used to warn before removing a worktree that an
/// agent or a dev server is still running in.
#[cfg(unix)]
pub fn process_folders() -> Vec<ProcessFolder> {
    let Ok(output) = crate::sys::command("/usr/sbin/lsof").args(["-a", "-d", "cwd", "-FpRcn"]).output() else {
        return Vec::new();
    };
    let mut list = Vec::new();
    let mut current = ProcessFolder { pid: 0, parent: 0, name: String::new(), dir: PathBuf::new() };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => current.pid = value.parse().unwrap_or(0),
            "R" => current.parent = value.parse().unwrap_or(0),
            "c" => current.name = value.to_string(),
            "n" => list.push(ProcessFolder { dir: PathBuf::from(value), ..current.clone() }),
            _ => {}
        }
    }
    list
}

/// Working folders of running programs. Used to warn before removing a worktree that an
/// agent or a dev server is still running in.
#[cfg(windows)]
pub fn process_folders() -> Vec<ProcessFolder> {
    crate::sys::processes().into_iter().filter_map(|p| Some(ProcessFolder { dir: p.dir?, pid: p.pid, parent: p.parent, name: p.name })).collect()
}

/// Whether `pid` is `ancestor` or runs under it, following parents through `list`.
pub fn descends_from(list: &[ProcessFolder], mut pid: u32, ancestor: u32) -> bool {
    for _ in 0..64 {
        if pid == ancestor {
            return true;
        }
        match list.iter().find(|p| p.pid == pid) {
            Some(p) if p.parent != pid && p.parent > 1 => pid = p.parent,
            _ => return false,
        }
    }
    false
}

/// The names of the programs from `list` that work in `folder` or below it and do not run
/// under `own_pid` (Glasscode's own sessions end with the removal of a worktree). Sorted, each
/// name one time.
pub fn programs_in(list: &[ProcessFolder], folder: &Path, own_pid: u32) -> Vec<String> {
    let real = folder.canonicalize().unwrap_or_else(|_| folder.to_path_buf());
    let mut names: Vec<String> = list
        .iter()
        .filter(|p| p.dir.starts_with(folder) || p.dir.starts_with(&real))
        .filter(|p| !descends_from(list, p.pid, own_pid))
        .map(|p| p.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Remove a linked worktree. Without `force`, git refuses when there are changes or a lock.
/// A worktree whose folder is already gone is pruned from git's bookkeeping.
pub fn remove_worktree(root: &Path, wt: &Worktree, force: bool) -> anyhow::Result<()> {
    if wt.is_main {
        bail!("the main checkout cannot be removed");
    }
    if wt.prunable || !wt.path.exists() {
        let mut cmd = git(root);
        cmd.args(["worktree", "prune"]);
        run(cmd)?;
        return Ok(());
    }
    let mut cmd = git(root);
    cmd.args(["worktree", "remove"]);
    if force {
        cmd.arg("--force");
        if wt.locked {
            cmd.arg("--force");
        }
    }
    cmd.arg(&wt.path);
    run(cmd)?;
    // Tools that make `<random-name>/<repo>` folders leave the random one behind; drop it
    // when it is now empty (remove_dir never deletes anything that has files).
    let repo_name = root.file_name();
    if repo_name.is_some() && wt.path.file_name() == repo_name {
        if let Some(parent) = wt.path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    Ok(())
}

/// Delete a local branch (`git branch -D`). Callers check `branch_only_commits` first.
pub fn delete_branch(root: &Path, branch: &str) -> anyhow::Result<()> {
    let mut cmd = git(root);
    cmd.args(["branch", "-D"]).arg(branch);
    run(cmd)?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileChange {
    pub path: String,
    /// Single-letter status for display: M, A, D, R, U (conflict) or ? (untracked).
    pub status: char,
    pub staged: bool,
    pub partially_staged: bool,
    pub added: usize,
    pub removed: usize,
}

pub fn status(dir: &Path) -> anyhow::Result<Vec<FileChange>> {
    let mut cmd = git(dir);
    cmd.args(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    let out = run(cmd)?;
    let mut entries = out.split('\0').filter(|s| !s.is_empty());
    let mut changes = Vec::new();
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let bytes = entry.as_bytes();
        let (x, y) = (bytes[0] as char, bytes[1] as char);
        let path = entry[3..].to_string();
        if x == 'R' || x == 'C' {
            // Renames carry the original path as the next entry.
            entries.next();
        }
        let status = match (x, y) {
            ('?', '?') => '?',
            ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => 'U',
            ('A', _) => 'A',
            ('D', _) | (_, 'D') => 'D',
            ('R', _) => 'R',
            _ => 'M',
        };
        let staged = x != ' ' && x != '?';
        let partially_staged = staged && y != ' ';
        changes.push(FileChange {
            path,
            status,
            staged,
            partially_staged,
            added: 0,
            removed: 0,
        });
    }
    // Line counts for staged + unstaged changes against HEAD.
    let mut cmd = git(dir);
    cmd.args(["diff", "HEAD", "--numstat", "-z"]);
    if let Ok(numstat) = run(cmd) {
        for record in numstat.split('\0') {
            let mut parts = record.split('\t');
            let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            if let Some(change) = changes.iter_mut().find(|c| c.path == path.trim()) {
                change.added = added.parse().unwrap_or(0);
                change.removed = removed.parse().unwrap_or(0);
            }
        }
    }
    for change in changes.iter_mut().filter(|c| c.status == '?') {
        if let Ok(text) = std::fs::read_to_string(dir.join(&change.path)) {
            change.added = text.lines().count();
        }
    }
    Ok(changes)
}

/// Unified diff of one file against HEAD (staged and unstaged together).
pub fn diff_file(dir: &Path, path: &str, untracked: bool) -> anyhow::Result<String> {
    let mut cmd = git(dir);
    if untracked {
        cmd.args(["diff", "--no-color", "--no-index", "--", "/dev/null", path]);
        let output = cmd.output()?;
        // `--no-index` exits with 1 when files differ.
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    cmd.args(["diff", "--no-color", "HEAD", "--", path]);
    run(cmd)
}

/// Full diff of the worktree against HEAD, including untracked files (for handoff).
pub fn diff_all(dir: &Path, max_bytes: usize) -> String {
    let mut out = String::new();
    let mut cmd = git(dir);
    cmd.args(["diff", "--no-color", "HEAD"]);
    if let Ok(diff) = run(cmd) {
        out.push_str(&diff);
    }
    if let Ok(changes) = status(dir) {
        for change in changes.iter().filter(|c| c.status == '?') {
            if let Ok(diff) = diff_file(dir, &change.path, true) {
                out.push_str(&diff);
            }
        }
    }
    if out.len() > max_bytes {
        let mut cut = max_bytes;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n… diff truncated …\n");
    }
    out
}

pub fn stage(dir: &Path, path: &str) -> anyhow::Result<()> {
    let mut cmd = git(dir);
    cmd.args(["add", "--", path]);
    run(cmd).map(|_| ())
}

pub fn unstage(dir: &Path, path: &str) -> anyhow::Result<()> {
    let mut cmd = git(dir);
    cmd.args(["restore", "--staged", "--", path]);
    run(cmd).map(|_| ())
}

pub fn commit(dir: &Path, message: &str) -> anyhow::Result<String> {
    let mut cmd = git_remote(dir);
    cmd.args(["commit", "-m", message]);
    run(cmd)
}

pub fn read_file(dir: &Path, path: &str) -> anyhow::Result<String> {
    let bytes = std::fs::read(dir.join(path))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn is_dirty(dir: &Path) -> bool {
    status(dir).map(|changes| !changes.is_empty()).unwrap_or(false)
}

/// Stage everything and commit (used before handing a session off).
pub fn commit_all(dir: &Path, message: &str) -> anyhow::Result<()> {
    let mut cmd = git(dir);
    cmd.args(["add", "-A"]);
    run(cmd)?;
    let mut cmd = git_remote(dir);
    cmd.args(["commit", "-m", message]);
    run(cmd).map(|_| ())
}

/// Files that look like secrets (env files, keys), which a first commit must not take.
fn looks_secret(name: &str) -> bool {
    (name.starts_with(".env") && !name.ends_with(".example") && !name.ends_with(".sample") && !name.ends_with(".template"))
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
}

/// Make a folder a git repository: `git init` on `main` and a first commit of what is in
/// it, so new worktrees start with those files. Files that look like secrets are kept out
/// (in `.git/info/exclude`, nothing committed); they are returned.
pub fn init_repository(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut init = git(dir);
    init.args(["init", "-q", "-b", "main"]);
    if run(init).is_err() {
        let mut init = git(dir);
        init.args(["init", "-q"]);
        run(init)?;
    }
    let mut status = git(dir);
    status.args(["status", "--porcelain", "-uall"]);
    let mut secrets: Vec<String> = run(status)?
        .lines()
        .filter_map(|l| l.get(3..))
        .map(|p| p.trim_matches('"').to_string())
        .filter(|p| p.rsplit('/').next().is_some_and(looks_secret))
        .collect();
    secrets.sort();
    secrets.dedup();
    if !secrets.is_empty() {
        let exclude = dir.join(".git/info/exclude");
        let mut text = std::fs::read_to_string(&exclude).unwrap_or_default();
        if !text.ends_with('\n') && !text.is_empty() {
            text.push('\n');
        }
        text.push_str("# Kept out by Glasscode when it set up git (looked like secrets)\n");
        for path in &secrets {
            text.push_str(&format!("/{path}\n"));
        }
        std::fs::create_dir_all(dir.join(".git/info"))?;
        std::fs::write(&exclude, text)?;
    }
    let mut add = git(dir);
    add.args(["add", "-A"]);
    run(add)?;
    let mut commit = git(dir);
    commit.args(["commit", "-q", "--allow-empty", "-m", "Start"]);
    run(commit)?;
    Ok(secrets)
}

/// Whether the checkout's branch was ever pushed: it tracks an upstream, or `origin` has it.
pub fn branch_was_pushed(dir: &Path, branch: &str) -> bool {
    let mut upstream = git(dir);
    upstream.args(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]);
    if run(upstream).is_ok() {
        return true;
    }
    rev(dir, &format!("refs/remotes/origin/{branch}")).is_some()
}

/// `git branch -m <old> <new>` (the worktree that has `old` checked out follows).
pub fn rename_branch(dir: &Path, old: &str, new: &str) -> anyhow::Result<()> {
    let mut cmd = git(dir);
    cmd.args(["branch", "-m", old, new]);
    run(cmd).map(|_| ())
}

/// The commits of `branch` that no remote branch holds: what a push of it would send, newest first.
pub fn unpushed_commits(dir: &Path, branch: &str) -> anyhow::Result<Vec<String>> {
    let mut cmd = git(dir);
    cmd.args(["rev-list", branch, "--not", "--remotes"]);
    Ok(run(cmd)?.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
}

pub fn push(dir: &Path, branch: &str) -> anyhow::Result<()> {
    let mut cmd = git_remote(dir);
    cmd.args(["push", "-u", "origin", branch]);
    run(cmd).map(|_| ())
}

/// Check out a branch that exists on `origin` (e.g. a teammate's) into a new worktree.
pub fn add_worktree_from_remote(root: &Path, project_name: &str, branch: &str) -> anyhow::Result<PathBuf> {
    let mut cmd = git_remote(root);
    cmd.args(["fetch", "origin", &format!("{branch}:refs/remotes/origin/{branch}")]);
    run(cmd)?;
    let dir = crate::paths::worktrees_dir().join(slug(project_name)).join(slug(branch));
    if dir.exists() {
        // Already checked out earlier: bring it up to date.
        let mut cmd = git_remote(&dir);
        cmd.args(["pull", "--ff-only"]);
        let _ = run(cmd);
        return Ok(dir);
    }
    std::fs::create_dir_all(dir.parent().unwrap_or(&dir))?;
    let mut cmd = git(root);
    cmd.arg("worktree").arg("add");
    if branch_exists(root, branch) {
        cmd.arg(&dir).arg(branch);
    } else {
        cmd.args(["--track", "-b", branch]).arg(&dir).arg(format!("origin/{branch}"));
    }
    run(cmd)?;
    Ok(dir)
}

// ----- the worktree as a tree (the Files tab) -----------------------------------------------

/// One entry of a folder in a worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// Folders first, then files; each by name, case-insensitive (so dot entries come first).
pub fn sort_entries(entries: &mut [DirEntry]) {
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.name.cmp(&b.name)));
}

/// A folder of a worktree (`relative` is "" for its top), without `.git` and `.DS_Store`.
pub fn list_dir(root: &Path, relative: &str) -> std::io::Result<Vec<DirEntry>> {
    let dir = if relative.is_empty() { root.to_path_buf() } else { root.join(relative) };
    let mut entries: Vec<DirEntry> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" || name == ".DS_Store" {
                return None;
            }
            // A symlink to a folder lists as a file: following it could leave the worktree.
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            Some(DirEntry { name, is_dir })
        })
        .collect();
    sort_entries(&mut entries);
    Ok(entries)
}

/// What git ignores in a worktree, as `git status --ignored=matching` names it: folders end
/// with "/" and stand for everything in them.
pub fn ignored_paths(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut cmd = git(dir);
    cmd.args(["status", "--porcelain=v1", "-z", "--ignored=matching", "--untracked-files=normal"]);
    Ok(parse_ignored(&run(cmd)?))
}

/// The "!!" entries of `git status --porcelain=v1 -z --ignored`.
pub fn parse_ignored(out: &str) -> Vec<String> {
    let mut entries = out.split('\0').filter(|s| !s.is_empty());
    let mut ignored = Vec::new();
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let code = &entry[..2];
        if code.starts_with('R') || code.starts_with('C') {
            entries.next();
        }
        if code == "!!" {
            ignored.push(entry[3..].to_string());
        }
    }
    ignored
}

/// Whether `path` is ignored by one of `ignored` (a folder entry covers what is inside it).
pub fn is_ignored(ignored: &[String], path: &str) -> bool {
    ignored.iter().any(|i| match i.strip_suffix('/') {
        Some(folder) => path == folder || path.starts_with(i.as_str()),
        None => path == i,
    })
}

/// Every file of a worktree that git tracks or would add (untracked but not ignored).
pub fn listed_files(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut cmd = git(dir);
    cmd.args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"]);
    let out = run(cmd)?;
    let mut files: Vec<String> = out.split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect();
    files.sort();
    files.dedup();
    Ok(files)
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    #[test]
    fn folders_come_first_by_name() {
        let mut list = vec![
            DirEntry { name: "src".into(), is_dir: true },
            DirEntry { name: "README.md".into(), is_dir: false },
            DirEntry { name: ".github".into(), is_dir: true },
            DirEntry { name: "apps".into(), is_dir: true },
            DirEntry { name: ".gitignore".into(), is_dir: false },
            DirEntry { name: "Cargo.toml".into(), is_dir: false },
        ];
        sort_entries(&mut list);
        let names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, [".github", "apps", "src", ".gitignore", "Cargo.toml", "README.md"]);
    }

    #[test]
    fn reads_ignored_entries() {
        let out = "!! node_modules/\0 M src/lib.rs\0R  new.rs\0old.rs\0!! .env.local\0?? notes.md\0!! dist/\0";
        let ignored = parse_ignored(out);
        assert_eq!(ignored, ["node_modules/", ".env.local", "dist/"]);
        assert!(is_ignored(&ignored, "node_modules"));
        assert!(is_ignored(&ignored, "node_modules/react/index.js"));
        assert!(is_ignored(&ignored, ".env.local"));
        assert!(!is_ignored(&ignored, ".env"));
        assert!(!is_ignored(&ignored, "distribution/x"));
    }

    #[test]
    fn lists_a_real_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sh = |args: &[&str]| assert!(crate::sys::command("git").arg("-C").arg(root).args(args).output().unwrap().status.success(), "git {args:?}");
        sh(&["init", "-q"]);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("target/debug/app"), "x").unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.join("notes.md"), "n").unwrap();
        let top = list_dir(root, "").unwrap();
        assert_eq!(top.iter().map(|e| (e.name.as_str(), e.is_dir)).collect::<Vec<_>>(), [("src", true), ("target", true), (".gitignore", false), ("notes.md", false)]);
        assert_eq!(ignored_paths(root).unwrap(), ["target/"]);
        assert_eq!(listed_files(root).unwrap(), [".gitignore", "notes.md", "src/main.rs"]);
    }
}

#[cfg(test)]
mod programs_in_tests {
    use super::*;

    #[test]
    fn programs_in_a_folder_leave_out_the_ones_of_this_app() {
        let folder = Path::new("/gc-no-such-root/wt/a");
        let at = |pid, parent, name: &str, dir: &str| ProcessFolder { pid, parent, name: name.into(), dir: PathBuf::from(dir) };
        let list = vec![
            at(10, 2, "glasscode", "/gc-no-such-root"),
            at(11, 10, "pwsh", "/gc-no-such-root/wt/a"),
            at(12, 11, "ping", "/gc-no-such-root/wt/a"),
            at(20, 2, "ping", "/gc-no-such-root/wt/a/src"),
            at(21, 2, "ping", "/gc-no-such-root/wt/a"),
            at(22, 2, "node", "/gc-no-such-root/wt/a"),
            at(23, 2, "vite", "/gc-no-such-root/wt/ab"),
        ];
        assert_eq!(programs_in(&list, folder, 10), vec!["node".to_string(), "ping".to_string()]);
        assert!(programs_in(&list, Path::new("/gc-no-such-root/wt/none"), 10).is_empty());
    }
}

#[cfg(test)]
mod worktree_folder_tests {
    use super::*;

    /// A link inside the worktree folder that leads outside it: a symbolic link on Unix, a
    /// junction on Windows. `mklink /J` needs no administrator and no Developer Mode, so on
    /// Windows (where the build job runs these tests) it must work: the test fails, never skips.
    fn link_out(link: &Path, target: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).expect("a symbolic link");
        #[cfg(windows)]
        {
            let out = crate::sys::command("cmd").arg("/c").arg("mklink").arg("/J").arg(link).arg(target).output().expect("cmd runs");
            assert!(out.status.success(), "mklink /J failed: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        }
    }

    #[test]
    fn a_nested_folder_is_made_below_the_worktree_folder() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("worktrees");
        let dir = base.join("app-wt").join("feat").join("eng-1-login");
        make_parents_below(&base, &dir).unwrap();
        assert!(base.join("app-wt").join("feat").is_dir());
        assert!(!dir.exists(), "git makes the worktree's own folder");
        // Again: the folders are there, and the same worktree folder still is not.
        make_parents_below(&base, &dir).unwrap();
        std::fs::create_dir(&dir).unwrap();
        assert!(make_parents_below(&base, &dir).unwrap_err().to_string().contains("already exists"));
        assert!(make_parents_below(&base, &home.path().join("elsewhere").join("x")).is_err());
        // A file where a folder should be.
        std::fs::write(base.join("file"), "x").unwrap();
        assert!(make_parents_below(&base, &base.join("file").join("x")).is_err());
    }

    #[test]
    fn a_link_inside_the_worktree_folder_is_refused_and_nothing_is_made_outside() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("worktrees");
        let outside = home.path().join("outside");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let link = base.join("app-wt");
        link_out(&link, &outside);
        assert!(crate::sys::is_link(&std::fs::symlink_metadata(&link).unwrap()), "the link is seen as one");
        // Through the link to a folder that would be made outside.
        let err = make_parents_below(&base, &link.join("feat").join("eng-1")).unwrap_err();
        assert!(err.to_string().contains("link or a junction"), "{err:#}");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "nothing was made outside");
        // The link itself as the worktree's folder.
        assert!(make_parents_below(&base, &link).is_err());
        // A dangling link where the worktree goes is refused too.
        let gone = home.path().join("gone");
        std::fs::create_dir(&gone).unwrap();
        let dangling = base.join("dangling");
        link_out(&dangling, &gone);
        std::fs::remove_dir(&gone).unwrap();
        assert!(make_parents_below(&base, &dangling).is_err());
        assert!(!gone.exists());
    }
}
