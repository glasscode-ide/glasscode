use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use glasscode_core::agents::find_codex_conversation;
use glasscode_core::model::{AgentKind, Session};

/// Tests that point GLASSCODE_HOME somewhere run one at a time (it is one per process).
static HOME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn use_home(dir: &Path) -> std::sync::MutexGuard<'static, ()> {
    let guard = HOME.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: only the tests that hold the lock read or change the variable.
    unsafe { std::env::set_var("GLASSCODE_HOME", dir) };
    guard
}

/// A rollout whose header (`session_meta`) has `cwd`, in a day folder of `sessions`.
fn write_rollout(codex_home: &Path, id: &str, cwd: &str) -> PathBuf {
    let day = codex_home.join("sessions").join("2026").join("11").join("15");
    std::fs::create_dir_all(&day).unwrap();
    let header = serde_json::json!({ "type": "session_meta", "payload": { "id": id, "cwd": cwd, "source": "cli" } });
    let path = day.join(format!("rollout-2026-11-15T10-00-00-{id}.jsonl"));
    std::fs::write(&path, format!("{header}\n")).unwrap();
    path
}

fn codex_session(worktree: &Path, prompt: Option<&str>) -> Session {
    Session {
        id: "s1234567890".into(),
        project_id: "p".into(),
        worktree: worktree.to_path_buf(),
        branch: "b".into(),
        title: "t".into(),
        agent: AgentKind::Codex,
        account_id: Some("codex-work".into()),
        agent_session_id: None,
        transcript_path: None,
        lineage: vec![],
        initial_prompt: prompt.map(str::to_string),
        team_task: None,
        model: None,
        mode: None,
        review: None,
        created_at: 0,
    }
}

#[test]
fn finds_conversation_by_folder_even_when_year_folder_is_old() {
    let home = tempfile::tempdir().unwrap();
    let day = home.path().join("sessions/2026/10/14");
    std::fs::create_dir_all(&day).unwrap();
    let cwd = home.path().join("repo");
    let header = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": "abc-123", "cwd": cwd }
    });
    std::fs::write(day.join("rollout-2026-10-14T13-12-22-abc-123.jsonl"), format!("{header}\n")).unwrap();
    // Make the year folder look old, like real Codex homes.
    let old = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    filetime::set_file_mtime(home.path().join("sessions/2026"), old).unwrap();

    let since = SystemTime::now() - Duration::from_secs(60);
    let found = find_codex_conversation(home.path(), &cwd, since).expect("found");
    assert_eq!(found.0, "abc-123");
    assert!(find_codex_conversation(home.path(), &home.path().join("other"), since).is_none());
}

/// Codex 0.153 on Windows writes the folder as it was given to it (`C:\Users\dev\app`, in the case
/// of the caller); Glasscode may know the same folder in another spelling.
#[cfg(windows)]
#[test]
fn finds_a_conversation_by_folder_in_any_spelling_on_windows() {
    let home = tempfile::tempdir().unwrap();
    write_rollout(home.path(), "win-1", r"C:\Users\dev\app");
    let since = SystemTime::now() - Duration::from_secs(60);
    for folder in [r"c:\users\dev\app", r"C:\Users\Dev\App\", r"\\?\C:\Users\dev\app"] {
        let found = find_codex_conversation(home.path(), Path::new(folder), since);
        assert_eq!(found.map(|(id, _)| id).as_deref(), Some("win-1"), "{folder}");
    }
    assert!(find_codex_conversation(home.path(), Path::new(r"c:\users\dev\app2"), since).is_none());
}

/// npm's `codex.cmd`, as `npm install -g @openai/codex` writes it.
#[cfg(windows)]
const NPM_CMD: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n";

/// Codex from npm starts as the program its `.cmd` file names, so the long, quoted
/// `-c developer_instructions=…` and a first message with `&`, `%` and `"` reach it unchanged; and
/// the worktree is trusted in the account's config, in TOML that Codex can read.
#[cfg(windows)]
#[test]
fn a_codex_session_from_npm_starts_without_cmd_and_trusts_its_worktree() {
    use glasscode_core::agents;
    use glasscode_core::model::Account;
    use std::collections::HashMap;

    let home = tempfile::tempdir().unwrap();
    let _home = use_home(&home.path().join("glasscode"));
    // The instructions about the `glasscode` command go with the skill.
    glasscode_core::cli::write_plugin().unwrap();
    let npm = home.path().join("npm");
    let script = npm.join("node_modules").join("@openai").join("codex").join("bin").join("codex.js");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, "").unwrap();
    std::fs::write(npm.join("codex.cmd"), NPM_CMD).unwrap();
    std::fs::write(npm.join("node.exe"), "").unwrap();
    let env = HashMap::from([("PATH".to_string(), npm.to_string_lossy().into_owned())]);
    let worktree = home.path().join("app");
    let account = Account { id: "codex-work".into(), agent: AgentKind::Codex, label: "Work".into(), plan: None, system_default: false };
    // cmd.exe would end the argument at `&` and put the value of PATH in place of `%PATH%`.
    let prompt = r#"say "a & b" for 100% of %PATH%"#;

    let launch = agents::launch(&codex_session(&worktree, Some(prompt)), Some(&account), &env);
    assert_eq!(PathBuf::from(&launch.program), npm.join("node.exe"));
    assert_eq!(PathBuf::from(&launch.args[0]), glasscode_core::sys::clean_path(&script));
    assert_eq!(&launch.args[launch.args.len() - 2..], ["--", prompt]);
    let at = launch.args.iter().position(|a| a.starts_with("developer_instructions=")).expect("developer_instructions");
    assert_eq!(launch.args[at - 1], "-c");
    // Codex reads the value as TOML.
    let value: toml::Table = toml::from_str(&launch.args[at].replacen("developer_instructions=", "v = ", 1)).unwrap();
    assert_eq!(value["v"].as_str(), Some(glasscode_core::cli::CODEX_INSTRUCTIONS));
    assert_eq!(launch.env.get("CODEX_HOME").map(PathBuf::from), Some(account.effective_dir()));

    let config: toml::Table = toml::from_str(&std::fs::read_to_string(account.effective_dir().join("config.toml")).unwrap()).unwrap();
    assert_eq!(config["projects"][worktree.to_str().unwrap()]["trust_level"].as_str(), Some("trusted"));
    // A second start (a resume after a restart of the app) adds nothing, and resumes.
    let mut resumed = codex_session(&worktree, None);
    resumed.agent_session_id = Some("019a0000-0000-7000-8000-000000000001".into());
    let launch = agents::launch(&resumed, Some(&account), &env);
    assert_eq!(&launch.args[launch.args.len() - 2..], ["resume", "019a0000-0000-7000-8000-000000000001"]);
    let again: toml::Table = toml::from_str(&std::fs::read_to_string(account.effective_dir().join("config.toml")).unwrap()).unwrap();
    assert_eq!(again, config);
}
