//! New branches start from the freshly fetched default branch, and pull requests are checked
//! out at their latest commit, for building on them or for reviewing them.

use std::path::Path;
use std::process::Command;

use glasscode_core::git;

fn sh(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, text: &str, message: &str) -> String {
    std::fs::write(dir.join(file), text).unwrap();
    sh(dir, &["add", "-A"]);
    sh(dir, &["commit", "-qm", message]);
    sh(dir, &["rev-parse", "HEAD"])
}

#[test]
fn starts_from_the_latest_main_and_checks_out_pull_requests() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("GLASSCODE_HOME", home.path().join("home")) };
    let origin = home.path().join("origin.git");
    let author = home.path().join("author");
    let repo = home.path().join("repo");
    std::fs::create_dir_all(&author).unwrap();
    sh(home.path(), &["init", "-q", "--bare", origin.to_str().unwrap()]);
    sh(&author, &["init", "-q", "-b", "main"]);
    sh(&author, &["remote", "add", "origin", origin.to_str().unwrap()]);
    commit(&author, "a.txt", "one\n", "init");
    sh(&author, &["push", "-q", "origin", "main"]);
    sh(home.path(), &["clone", "-q", origin.to_str().unwrap(), repo.to_str().unwrap()]);

    // main moves on after the developer's clone; their folder's main stays behind.
    let newest = commit(&author, "a.txt", "one\ntwo\n", "on main");
    sh(&author, &["push", "-q", "origin", "main"]);
    // With fetching turned off in Settings, new work starts from main as it is in the folder.
    let local = git::start_base(&repo, false);
    assert_eq!(local.base, "main");
    assert!(!local.fetched);
    assert_ne!(sh(&repo, &["rev-parse", "main"]), newest);
    let base = git::start_base(&repo, true);
    assert_eq!(base.base, "origin/main");
    assert!(base.fetched && base.newest_commit.is_some());
    let path = git::add_worktree(&repo, "repo", "feat/new", &base.base).unwrap();
    assert_eq!(sh(&path, &["rev-parse", "HEAD"]), newest);
    // Not tracking origin/main, so its own `git push -u` decides where it goes.
    assert!(Command::new("git").arg("-C").arg(&path).args(["rev-parse", "--abbrev-ref", "@{upstream}"]).output().unwrap().stderr.len() > 0);

    // A pull request: branch fix/x, published as refs/pull/7/head like GitHub does.
    sh(&author, &["checkout", "-q", "-b", "fix/x"]);
    let head1 = commit(&author, "b.txt", "fix\n", "fix");
    sh(&author, &["push", "-q", "origin", "fix/x", "fix/x:refs/pull/7/head"]);

    let review = git::checkout_review(&repo, "repo", 7).unwrap();
    assert_eq!(review.branch, "review/7");
    assert_eq!(sh(&review.path, &["rev-parse", "HEAD"]), head1);
    assert_eq!(sh(&repo, &["config", "branch.review/7.pushRemote"]), "review-only-do-not-push");
    // The review's notes stay out of git (checkout_review excludes /.glasscode/).
    std::fs::create_dir_all(review.path.join(".glasscode/review")).unwrap();
    std::fs::write(review.path.join(".glasscode/review/overview.md"), "## What it does\n").unwrap();
    assert!(git::status(&review.path).unwrap().is_empty());

    // The author pushes again: the review follows.
    let head2 = commit(&author, "b.txt", "fix\nmore\n", "more");
    sh(&author, &["push", "-q", "-f", "origin", "fix/x", "fix/x:refs/pull/7/head"]);
    let again = git::checkout_review(&repo, "repo", 7).unwrap();
    assert_eq!(again.path.canonicalize().unwrap(), review.path.canonicalize().unwrap());
    assert!(again.note.starts_with("Updated"), "{}", again.note);
    assert_eq!(sh(&review.path, &["rev-parse", "HEAD"]), head2);

    // …also after a force-push, since nothing local would be lost.
    sh(&author, &["reset", "-q", "--hard", &head1]);
    let head3 = commit(&author, "b.txt", "fix\nrewritten\n", "rewritten");
    sh(&author, &["push", "-q", "-f", "origin", "fix/x", "fix/x:refs/pull/7/head"]);
    let forced = git::checkout_review(&repo, "repo", 7).unwrap();
    assert!(forced.note.contains("force-pushed"), "{}", forced.note);
    assert_eq!(sh(&review.path, &["rev-parse", "HEAD"]), head3);

    // Local edits are never thrown away.
    std::fs::write(review.path.join("a.txt"), "mine\n").unwrap();
    commit(&author, "c.txt", "c\n", "later");
    sh(&author, &["push", "-q", "-f", "origin", "fix/x", "fix/x:refs/pull/7/head"]);
    let kept = git::checkout_review(&repo, "repo", 7).unwrap();
    assert!(kept.note.starts_with("Kept your local changes"), "{}", kept.note);
    assert_eq!(sh(&review.path, &["rev-parse", "HEAD"]), head3);
    assert_eq!(std::fs::read_to_string(review.path.join("a.txt")).unwrap(), "mine\n");

    // Building on the PR: its own branch, tracking it, so pushing updates the PR.
    let build = git::checkout_pr_branch(&repo, "repo", 7, "fix/x", true).unwrap();
    assert_eq!(build.branch, "fix/x");
    assert_eq!(git::current_branch(&build.path), "fix/x");
    assert_eq!(sh(&build.path, &["rev-parse", "--abbrev-ref", "@{upstream}"]), "origin/fix/x");
    assert_eq!(sh(&build.path, &["rev-parse", "HEAD"]), sh(&author, &["rev-parse", "HEAD"]));
    let same = git::checkout_pr_branch(&repo, "repo", 7, "fix/x", true).unwrap();
    assert_eq!(same.path.canonicalize().unwrap(), build.path.canonicalize().unwrap());
    assert!(same.note.starts_with("Up to date"), "{}", same.note);

    // A fork's PR: a local pr/7 branch from the pull ref.
    let fork = git::checkout_pr_branch(&repo, "repo", 7, "fix/x", false).unwrap();
    assert_eq!(fork.branch, "pr/7");
    assert_eq!(git::current_branch(&fork.path), "pr/7");
}
