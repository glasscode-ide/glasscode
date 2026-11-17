//! A project's `.glasscode/settings.json` names the folder of a new worktree, which stays below
//! Glasscode's worktree folder, and the setup waits for the first session there.

use std::process::Command;

use glasscode_core::{git, project_settings};

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
fn the_project_file_names_the_folder_and_keeps_the_setup_for_the_first_session() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary that sets it.
    unsafe { std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode")) };
    let repo = home.path().join("my app");
    std::fs::create_dir_all(repo.join(".glasscode")).unwrap();
    sh(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "init"]);
    let root = git::main_checkout(&repo).unwrap();
    let worktrees = glasscode_core::paths::worktrees_dir();

    // A template that tries to leave the worktree folder stays below it.
    std::fs::write(
        project_settings::file(&root),
        r#"{ "worktree_folder": "../../{repo}-x/{type}/{slug}", "setup": "echo setup ran", "team_note": "kept" }"#,
    )
    .unwrap();
    let path = git::add_worktree(&root, "my app", "feat/eng-1-login", "main").unwrap();
    assert_eq!(
        path,
        worktrees.join("my-app-x").join("feat").join("eng-1-login")
    );
    assert!(path.join("a.txt").is_file());
    assert_eq!(git::current_branch(&path), "feat/eng-1-login");

    // The setup read for this worktree waits for its first session, once.
    let new = project_settings::take_new_worktree(&path).expect("the new worktree is remembered");
    assert_eq!(new.root, root);
    assert_eq!(new.loaded.settings.setup.as_deref(), Some("echo setup ran"));
    assert!(project_settings::take_new_worktree(&path).is_none());

    // A broken file: the folder as before, and the worktree is still made.
    std::fs::write(project_settings::file(&root), "{ broken").unwrap();
    let path = git::add_worktree(&root, "my app", "fix/two", "main").unwrap();
    assert_eq!(path, worktrees.join("my-app").join("fix-two"));
    let new = project_settings::take_new_worktree(&path).unwrap();
    assert_eq!(
        (
            new.loaded.state,
            new.loaded.settings.setup,
            new.loaded.warnings.len()
        ),
        (project_settings::FileState::Broken, None, 1)
    );
}
