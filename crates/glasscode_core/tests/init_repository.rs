//! Setting up git in a folder that has none: everything is committed except what looks
//! like a secret, which is kept out locally.

use std::process::Command;

#[test]
fn a_folder_becomes_a_repository_without_its_secrets() {
    let dir = std::env::temp_dir().join(format!("glasscode-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(dir.join("README.md"), "hello").unwrap();
    std::fs::write(dir.join(".env"), "TOKEN=1").unwrap();
    std::fs::write(dir.join(".env.example"), "TOKEN=").unwrap();
    std::fs::write(dir.join("config/server.key"), "key").unwrap();
    // A machine without a git identity still gets its first commit (CI).
    unsafe {
        std::env::set_var("GIT_AUTHOR_NAME", "t");
        std::env::set_var("GIT_AUTHOR_EMAIL", "t@t");
        std::env::set_var("GIT_COMMITTER_NAME", "t");
        std::env::set_var("GIT_COMMITTER_EMAIL", "t@t");
    }
    let kept_out = glasscode_core::git::init_repository(&dir).unwrap();
    assert_eq!(kept_out, vec![".env".to_string(), "config/server.key".to_string()]);
    let files = Command::new("git").arg("-C").arg(&dir).args(["ls-files"]).output().unwrap();
    let files = String::from_utf8_lossy(&files.stdout);
    assert!(files.contains("README.md") && files.contains(".env.example"), "{files}");
    assert!(!files.lines().any(|f| f == ".env" || f == "config/server.key"), "{files}");
    assert_eq!(glasscode_core::git::current_branch(&dir), "main");
    let status = Command::new("git").arg("-C").arg(&dir).args(["status", "--porcelain"]).output().unwrap();
    assert!(String::from_utf8_lossy(&status.stdout).trim().is_empty(), "the secrets stay out of git status too");
    let _ = std::fs::remove_dir_all(&dir);
}
