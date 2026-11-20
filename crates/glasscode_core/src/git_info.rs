//! What the pull-request lookups need from git about a checkout (its branch, its repository on
//! GitHub, the branches its pull request may be on), kept until the checkout's git files change.
//! Asking git costs several processes per checkout; looking at a few file times costs nothing,
//! so a refresh of twenty sessions runs git only for the checkouts that changed.
//!
//! The same file times tell when a checkout was switched to another branch, pushed or fetched
//! ([`moved`]): the moment its pull request may have changed, and worth asking GitHub about.
//! They are a hint, not the only trigger: what is kept is also asked of git again after
//! [`KEEP_FOR`], and the app still refreshes on its timers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::save::{Stamp, stamp};

/// The longest a checkout's git answers are kept while none of its files changed.
pub const KEEP_FOR: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitInfo {
    /// The branch, or "HEAD" when detached (as `git rev-parse --abbrev-ref HEAD`).
    pub branch: String,
    /// `owner/name` on GitHub (`github::repo_slug`).
    pub repo: Option<String>,
    /// Where its pull request may be, most likely first (`github::pr_branch_candidates`).
    pub candidates: Vec<String>,
}

struct Entry {
    /// The stamps of the files that, when they change, mean: ask git again.
    local: Vec<Stamp>,
    info: GitInfo,
    at: Instant,
}

static KEPT: Mutex<Option<HashMap<PathBuf, Entry>>> = Mutex::new(None);
/// The stamps [`moved`] saw last, per checkout.
static SEEN: Mutex<Option<HashMap<PathBuf, Vec<Stamp>>>> = Mutex::new(None);

/// A checkout's own git folder and the repository's shared one (the same for the main
/// checkout; for a linked worktree, `.git` is a file naming its folder, which names the shared
/// one in `commondir`).
fn git_dirs(worktree: &Path) -> Option<(PathBuf, PathBuf)> {
    let dot = worktree.join(".git");
    let meta = std::fs::metadata(&dot).ok()?;
    let own = if meta.is_dir() {
        dot
    } else {
        let text = std::fs::read_to_string(&dot).ok()?;
        let path = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        if path.is_absolute() { path } else { worktree.join(path) }
    };
    let common = match std::fs::read_to_string(own.join("commondir")) {
        Ok(text) => {
            let path = PathBuf::from(text.trim());
            if path.is_absolute() { path } else { own.join(path) }
        }
        Err(_) => own.clone(),
    };
    Some((own, common))
}

/// The branch HEAD names, or "HEAD" when it names a commit (detached).
fn head_branch(own: &Path) -> Option<String> {
    let head = std::fs::read_to_string(own.join("HEAD")).ok()?;
    Some(head.trim().strip_prefix("ref: refs/heads/").map(str::to_string).unwrap_or_else(|| "HEAD".to_string()))
}

/// Files whose change means the branch, the remote or the candidates may have changed.
fn local_files(own: &Path, common: &Path) -> Vec<PathBuf> {
    vec![own.join("HEAD"), own.join("logs/HEAD"), common.join("config"), common.join("packed-refs"), common.join("FETCH_HEAD")]
}

/// Files whose change means the pull request may have changed: a switch to another branch, a
/// push of this branch (its remote ref), a fetch.
fn moved_files(own: &Path, common: &Path, branch: &str) -> Vec<PathBuf> {
    let mut files = vec![own.join("HEAD"), common.join("packed-refs"), common.join("FETCH_HEAD")];
    if branch != "HEAD" && !branch.is_empty() {
        files.push(common.join("refs/remotes/origin").join(branch));
    }
    files
}

/// A checkout's git answers: kept ones while its files have not changed, else asked of git.
/// A folder without git gives no repository and no candidates. Runs git: call it in the
/// background.
pub fn info(worktree: &Path) -> GitInfo {
    let Some((own, common)) = git_dirs(worktree) else {
        // No `.git` of its own (a folder inside a repository, or none at all): asked of git
        // each time, as before.
        let branch = crate::git::current_branch(worktree);
        let repo = crate::github::repo_slug(worktree);
        let candidates = if repo.is_some() { crate::github::pr_branch_candidates(worktree, &branch) } else { Vec::new() };
        return GitInfo { branch, repo, candidates };
    };
    let local: Vec<Stamp> = local_files(&own, &common).iter().map(|f| stamp(f)).collect();
    {
        let kept = KEPT.lock().unwrap();
        if let Some(entry) = kept.as_ref().and_then(|k| k.get(worktree)) {
            if entry.local == local && entry.at.elapsed() < KEEP_FOR {
                return entry.info.clone();
            }
        }
    }
    let branch = head_branch(&own).unwrap_or_else(|| crate::git::current_branch(worktree));
    let repo = crate::github::repo_slug(worktree);
    let candidates = if repo.is_some() { crate::github::pr_branch_candidates(worktree, &branch) } else { Vec::new() };
    let info = GitInfo { branch, repo, candidates };
    KEPT.lock().unwrap().get_or_insert_with(HashMap::new).insert(worktree.to_path_buf(), Entry { local, info: info.clone(), at: Instant::now() });
    info
}

/// The checkouts among `worktrees` that were switched, pushed or fetched since the last call
/// (only file times are read: no git). A checkout seen for the first time has not moved.
pub fn moved(worktrees: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = SEEN.lock().unwrap();
    let seen = seen.get_or_insert_with(HashMap::new);
    let mut out = Vec::new();
    for worktree in worktrees {
        let Some((own, common)) = git_dirs(worktree) else { continue };
        let branch = head_branch(&own).unwrap_or_default();
        let now: Vec<Stamp> = moved_files(&own, &common, &branch).iter().map(|f| stamp(f)).collect();
        match seen.insert(worktree.clone(), now.clone()) {
            Some(before) if before != now => out.push(worktree.clone()),
            _ => {}
        }
    }
    // Checkouts no longer asked about are not kept.
    seen.retain(|path, _| worktrees.contains(path));
    out
}

/// Where a checkout stands, from its git files alone (no git process): its branch, the commit
/// the branch names, and the commit its `origin` branch names. What the journal compares to tell
/// a commit, a push or another branch (`journal::GitWatch`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Heads {
    pub branch: String,
    pub head: Option<String>,
    pub remote: Option<String>,
}

/// A ref's commit: its loose file, else its line in `packed-refs`.
fn read_ref(common: &Path, name: &str) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(common.join(name)) {
        let sha = text.trim();
        if sha.len() >= 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(sha.to_string());
        }
    }
    let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (sha, reference) = line.split_once(' ')?;
        (reference == name && sha.bytes().all(|b| b.is_ascii_hexdigit())).then(|| sha.to_string())
    })
}

/// A checkout's [`Heads`]; None when it has no git folder of its own.
pub fn heads(worktree: &Path) -> Option<Heads> {
    let (own, common) = git_dirs(worktree)?;
    let head = std::fs::read_to_string(own.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: refs/heads/") {
        Some(branch) => Some(Heads {
            branch: branch.to_string(),
            head: read_ref(&common, &format!("refs/heads/{branch}")),
            remote: read_ref(&common, &format!("refs/remotes/origin/{branch}")),
        }),
        None => Some(Heads { branch: "HEAD".into(), head: Some(head.to_string()), remote: None }),
    }
}

/// Forget a checkout's kept answers (it was removed, or its remote was changed by Glasscode).
pub fn forget(worktree: &Path) {
    if let Some(kept) = KEPT.lock().unwrap().as_mut() {
        kept.remove(worktree);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// git in `dir`, with a name and email of its own (as `mirror`'s tests).
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main"]).args(args).output().unwrap();
        assert!(status.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&status.stderr));
    }

    /// A repository with one commit and an origin on GitHub, and a linked worktree of it.
    fn repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("app");
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["remote", "add", "origin", "https://github.com/acme/app.git"]);
        std::fs::write(main.join("a"), "a").unwrap();
        git(&main, &["add", "a"]);
        git(&main, &["commit", "-q", "-m", "one"]);
        let linked = dir.path().join("wt");
        git(&main, &["worktree", "add", "-q", "-b", "feat/x", linked.to_str().unwrap()]);
        (dir, main, linked)
    }

    #[test]
    fn a_checkout_reads_its_branch_and_repository_and_keeps_them() {
        let (_dir, main, linked) = repo();
        let info = info(&linked);
        assert_eq!(info.branch, "feat/x");
        assert_eq!(info.repo.as_deref(), Some("acme/app"));
        assert!(info.candidates.contains(&"feat/x".to_string()));
        assert_eq!(super::info(&main).branch, "main");
        // Kept: a change git would see but the watched files do not (a remote set in a way that
        // touches no file is impossible, so look at the entry itself).
        let kept_at = KEPT.lock().unwrap().as_ref().unwrap().get(&linked).unwrap().at;
        let _ = super::info(&linked);
        assert_eq!(KEPT.lock().unwrap().as_ref().unwrap().get(&linked).unwrap().at, kept_at);
    }

    #[test]
    fn a_switch_of_branch_is_seen_as_a_move_and_read_again() {
        let (_dir, _main, linked) = repo();
        let worktrees = vec![linked.clone()];
        assert!(moved(&worktrees).is_empty(), "seen for the first time");
        assert!(moved(&worktrees).is_empty(), "nothing changed");
        assert_eq!(info(&linked).branch, "feat/x");
        // File times have a resolution: make the change land in another tick.
        std::thread::sleep(Duration::from_millis(20));
        git(&linked, &["switch", "-q", "-c", "feat/y"]);
        assert_eq!(moved(&worktrees), worktrees);
        assert_eq!(info(&linked).branch, "feat/y");
        assert!(moved(&worktrees).is_empty());
    }

    #[test]
    fn a_detached_checkout_and_a_folder_without_git() {
        let (_dir, main, linked) = repo();
        git(&linked, &["switch", "-q", "--detach", "main"]);
        assert_eq!(info(&linked).branch, "HEAD");
        let plain = main.parent().unwrap().join("plain");
        std::fs::create_dir(&plain).unwrap();
        let none = info(&plain);
        assert_eq!((none.repo, none.candidates.len()), (None, 0));
        assert!(moved(&[plain]).is_empty());
    }
}
