use std::process::Command;

use glasscode_core::agents::copy_conversation;
use glasscode_core::git;
use glasscode_core::model::{Account, AgentKind};

/// Tests that point GLASSCODE_HOME somewhere run one at a time (it is one per process).
static HOME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn sh(dir: &std::path::Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
fn creates_and_lists_worktrees_and_reports_changes() {
    let _home = HOME.lock().unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    // SAFETY: test-only, single-threaded use of the variable within this test binary.
    unsafe { std::env::set_var("GLASSCODE_HOME", home.path().join("ns")) };
    let repo = home.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "init"]);

    let path = git::add_worktree(&repo, "repo", "ns/test-branch", "main").unwrap();
    assert!(path.join("a.txt").exists());
    assert_eq!(git::current_branch(&path), "ns/test-branch");
    let list = git::worktrees(&repo).unwrap();
    assert_eq!(list.len(), 2);
    assert!(list[0].is_main);
    assert_eq!(git::main_checkout(&path).unwrap().canonicalize().unwrap(), repo.canonicalize().unwrap());

    std::fs::write(path.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(path.join("b.txt"), "new\n").unwrap();
    let changes = git::status(&path).unwrap();
    let a = changes.iter().find(|c| c.path == "a.txt").unwrap();
    assert_eq!((a.status, a.added, a.removed, a.staged), ('M', 1, 0, false));
    let b = changes.iter().find(|c| c.path == "b.txt").unwrap();
    assert_eq!(b.status, '?');

    git::stage(&path, "a.txt").unwrap();
    assert!(git::status(&path).unwrap().iter().any(|c| c.path == "a.txt" && c.staged));
    sh(&path, &["config", "user.name", "t"]);
    sh(&path, &["config", "user.email", "t@example.com"]);
    git::commit(&path, "change a").unwrap();
    assert!(!git::status(&path).unwrap().iter().any(|c| c.path == "a.txt"));
}

/// A new session's worktree: below Glasscode's worktree folder, also when that path has a space
/// (a profile folder such as C:\Users\Ada Lovelace), with a folder name for a branch with `/`.
#[test]
fn a_new_worktree_goes_below_the_worktree_folder() {
    let _home = HOME.lock().unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    // SAFETY: as above.
    unsafe { std::env::set_var("GLASSCODE_HOME", home.path().join("Ada Lovelace").join(".glasscode")) };
    let repo = home.path().join("my app");
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "init"]);

    // The root as the app keeps it for a project: as git gives it (with / on Windows).
    let root = git::main_checkout(&repo).unwrap();
    let path = git::add_worktree(&root, "my app", "feature/login", "main").unwrap();
    assert_eq!(path, glasscode_core::paths::worktrees_dir().join("my-app").join("feature-login"));
    assert!(path.starts_with(home.path().join("Ada Lovelace")), "{}", path.display());
    assert!(path.join("a.txt").is_file());
    assert_eq!(git::current_branch(&path), "feature/login");
    let listed = git::worktrees(&repo).unwrap();
    assert!(listed.iter().any(|w| !w.is_main && w.path.canonicalize().ok() == path.canonicalize().ok()), "{listed:?}");
}

#[test]
fn copies_a_claude_conversation_between_accounts_without_credentials() {
    let _home = HOME.lock().unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("GLASSCODE_HOME", home.path()) };
    let from = Account { id: "claude-a".into(), agent: AgentKind::Claude, label: "A".into(), plan: None, system_default: false };
    let to = Account { id: "claude-b".into(), agent: AgentKind::Claude, label: "B".into(), plan: None, system_default: false };
    let project = from.effective_dir().join("projects/-repo");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("conv-1.jsonl"), "{}\n").unwrap();
    std::fs::write(from.effective_dir().join(".credentials.json"), "secret").unwrap();

    copy_conversation(AgentKind::Claude, &from, &to, "conv-1").unwrap();
    assert!(to.effective_dir().join("projects/-repo/conv-1.jsonl").exists());
    assert!(!to.effective_dir().join(".credentials.json").exists());
}

#[test]
fn checks_and_removes_worktrees_without_losing_work() {
    let home = tempfile::tempdir().unwrap();
    let origin = home.path().join("origin.git");
    let repo = home.path().join("repo");
    sh(home.path(), &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    sh(home.path(), &["clone", "-q", origin.to_str().unwrap(), repo.to_str().unwrap()]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "init"]);
    sh(&repo, &["push", "-q", "origin", "main"]);
    let wt = |name: &str| home.path().join("wt").join(name);
    let s = |p: &std::path::Path| p.to_str().unwrap().to_string();

    // pushed: a branch whose commit is on the remote. local: a branch with an unpushed commit.
    sh(&repo, &["worktree", "add", "-q", "-b", "pushed", &s(&wt("pushed")), "main"]);
    std::fs::write(wt("pushed").join("p.txt"), "p\n").unwrap();
    sh(&wt("pushed"), &["add", "-A"]);
    sh(&wt("pushed"), &["commit", "-qm", "p"]);
    sh(&wt("pushed"), &["push", "-q", "origin", "pushed"]);
    sh(&repo, &["worktree", "add", "-q", "-b", "local", &s(&wt("local")), "main"]);
    std::fs::write(wt("local").join("l.txt"), "l\n").unwrap();
    sh(&wt("local"), &["add", "-A"]);
    sh(&wt("local"), &["commit", "-qm", "l"]);
    // dirty: uncommitted file. detached: a commit no branch has. gone: folder deleted. locked.
    sh(&repo, &["worktree", "add", "-q", "-b", "dirty", &s(&wt("dirty")), "main"]);
    std::fs::write(wt("dirty").join("new.txt"), "x\n").unwrap();
    sh(&repo, &["worktree", "add", "-q", "--detach", &s(&wt("detached").join("repo")), "main"]);
    std::fs::write(wt("detached").join("repo").join("d.txt"), "d\n").unwrap();
    sh(&wt("detached").join("repo"), &["add", "-A"]);
    sh(&wt("detached").join("repo"), &["commit", "-qm", "d"]);
    sh(&repo, &["worktree", "add", "-q", "--detach", &s(&wt("gone")), "main"]);
    std::fs::remove_dir_all(wt("gone")).unwrap();
    sh(&repo, &["worktree", "add", "-q", "--detach", &s(&wt("locked")), "main"]);
    sh(&repo, &["worktree", "lock", &s(&wt("locked"))]);

    let list = git::worktrees(&repo).unwrap();
    let find = |name: &str| list.iter().find(|w| w.path.ends_with(name) || w.path.ends_with(format!("{name}/repo"))).unwrap().clone();
    assert!(list[0].is_main && !list[1..].iter().any(|w| w.is_main));
    let (pushed, local, dirty, detached, gone, locked) =
        (find("pushed"), find("local"), find("dirty"), find("detached"), find("gone"), find("locked"));
    assert_eq!(detached.label("repo"), "detached");
    assert_eq!(pushed.label("repo"), "pushed");
    assert!(gone.prunable && locked.locked && detached.is_detached() && !detached.head.is_empty());

    std::fs::write(repo.join(".gitignore"), ".env\nnode_modules/\n").unwrap();
    std::fs::write(wt("pushed").join(".gitignore"), ".env\nnode_modules/\n").unwrap();
    std::fs::write(wt("pushed").join(".env"), "SECRET=1\n").unwrap();
    std::fs::create_dir_all(wt("pushed").join("node_modules/x")).unwrap();
    std::fs::write(wt("pushed").join("node_modules/x/.env"), "").unwrap();
    let c = git::check_worktree(&pushed);
    assert_eq!(c.env_files, vec![".env".to_string()]);
    assert_eq!(c.changed, 1); // the new .gitignore
    std::fs::remove_file(wt("pushed").join(".gitignore")).unwrap();
    let c = git::check_worktree(&pushed);
    // Without its own .gitignore the ignore rules are gone, so .env shows as untracked.
    assert!(c.changed >= 1);
    sh(&wt("pushed"), &["clean", "-qfdx"]);
    let c = git::check_worktree(&pushed);
    assert!(c.is_clean() && c.branch_only_commits == 0 && c.last_commit.is_some() && c.env_files.is_empty());
    let c = git::check_worktree(&local);
    assert!(c.is_clean() && c.branch_only_commits == 1);
    let c = git::check_worktree(&dirty);
    assert_eq!(c.changed, 1);
    assert!(!c.is_clean());
    let c = git::check_worktree(&detached);
    assert_eq!(c.lost_commits, 1);
    assert!(git::check_worktree(&gone).is_clean());

    // The main checkout is never removed; without force git keeps changes and locks.
    assert!(git::remove_worktree(&repo, &list[0], true).is_err());
    assert!(git::remove_worktree(&repo, &dirty, false).is_err());
    assert!(wt("dirty").join("new.txt").exists());
    assert!(git::remove_worktree(&repo, &locked, false).is_err());

    git::remove_worktree(&repo, &pushed, false).unwrap();
    git::delete_branch(&repo, "pushed").unwrap();
    git::remove_worktree(&repo, &gone, false).unwrap();
    git::remove_worktree(&repo, &dirty, true).unwrap();
    git::remove_worktree(&repo, &locked, true).unwrap();
    git::remove_worktree(&repo, &detached, true).unwrap();
    // The now-empty `detached/` folder around `detached/repo` goes too.
    assert!(!wt("detached").exists());
    let left: Vec<_> = git::worktrees(&repo).unwrap().into_iter().map(|w| w.path).collect();
    assert_eq!(left.len(), 2, "{left:?}"); // main, local
    assert!(!wt("pushed").exists() && !wt("dirty").exists() && !wt("locked").exists());
    // The remote still has the deleted local branch.
    sh(&repo, &["rev-parse", "--verify", "-q", "refs/remotes/origin/pushed"]);
}

#[test]
fn finds_the_pr_branch_a_checkout_builds_on() {
    use glasscode_core::github::pr_branch_candidates;
    let home = tempfile::tempdir().unwrap();
    let origin = home.path().join("origin.git");
    let repo = home.path().join("repo");
    sh(home.path(), &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    sh(home.path(), &["clone", "-q", origin.to_str().unwrap(), repo.to_str().unwrap()]);
    let commit = |dir: &std::path::Path, file: &str| {
        std::fs::write(dir.join(file), file).unwrap();
        sh(dir, &["add", "-A"]);
        sh(dir, &["commit", "-qm", file]);
    };
    commit(&repo, "a");
    sh(&repo, &["push", "-q", "origin", "main"]);
    sh(&repo, &["remote", "set-head", "origin", "main"]);
    // A PR branch on the remote, and another one this checkout has nothing to do with.
    sh(&repo, &["checkout", "-q", "-b", "feat/auto-model-routing"]);
    commit(&repo, "b");
    sh(&repo, &["push", "-q", "origin", "feat/auto-model-routing"]);
    sh(&repo, &["checkout", "-q", "-b", "feat/other", "main"]);
    commit(&repo, "c");
    sh(&repo, &["push", "-q", "origin", "feat/other"]);
    // main moves on.
    sh(&repo, &["checkout", "-q", "main"]);
    commit(&repo, "d");
    sh(&repo, &["push", "-q", "origin", "main"]);

    // A local branch with another name made from the PR's commit, no upstream, main merged in.
    sh(&repo, &["checkout", "-q", "-b", "demo/auto-routing", "origin/feat/auto-model-routing"]);
    sh(&repo, &["branch", "--unset-upstream"]);
    sh(&repo, &["merge", "-q", "--no-edit", "origin/main"]);
    assert_eq!(pr_branch_candidates(&repo, "demo/auto-routing"), vec!["demo/auto-routing", "feat/auto-model-routing"]);

    // With an upstream under another name, that name comes first.
    sh(&repo, &["branch", "-u", "origin/feat/auto-model-routing"]);
    assert_eq!(pr_branch_candidates(&repo, "demo/auto-routing")[0], "feat/auto-model-routing");

    // A fresh branch from main builds on nothing; a detached HEAD still finds its PR branch.
    sh(&repo, &["checkout", "-q", "-b", "fresh", "origin/main"]);
    assert_eq!(pr_branch_candidates(&repo, "fresh"), vec!["fresh"]);
    sh(&repo, &["checkout", "-q", "--detach", "origin/feat/other"]);
    assert_eq!(pr_branch_candidates(&repo, "HEAD"), vec!["feat/other"]);
}
