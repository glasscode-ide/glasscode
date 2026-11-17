//! A Claude session on Windows: its settings in a file of the session, and `claude` from npm
//! started as the program its `.cmd` file names.
#![cfg(windows)]

use std::collections::HashMap;
use std::path::PathBuf;

use glasscode_core::agents;
use glasscode_core::model::{AgentKind, Session};

/// npm's `claude.cmd` for a script, as `npm install -g` writes it.
const NPM_CMD: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n";

#[test]
fn a_claude_session_has_a_settings_file_and_starts_without_cmd() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("GLASSCODE_HOME", home.path()) };
    let npm = home.path().join("npm");
    let script = npm.join("node_modules").join("@anthropic-ai").join("claude-code").join("cli.js");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, "").unwrap();
    std::fs::write(npm.join("claude.cmd"), NPM_CMD).unwrap();
    std::fs::write(npm.join("node.exe"), "").unwrap();
    let env = HashMap::from([("PATH".to_string(), npm.to_string_lossy().into_owned())]);
    // cmd.exe would end the argument at `&` and put the value of PATH in place of `%PATH%`.
    let prompt = r#"say "a & b" for 100% of %PATH%"#;
    let session = Session {
        id: "s1".into(),
        project_id: "p".into(),
        worktree: home.path().join("app"),
        branch: "b".into(),
        title: "t".into(),
        agent: AgentKind::Claude,
        account_id: None,
        agent_session_id: None,
        transcript_path: None,
        lineage: vec![],
        initial_prompt: Some(prompt.into()),
        team_task: None,
        model: None,
        mode: None,
        review: None,
        created_at: 0,
    };
    let launch = agents::launch(&session, None, &env);
    assert_eq!(PathBuf::from(&launch.program), npm.join("node.exe"));
    assert_eq!(PathBuf::from(&launch.args[0]), glasscode_core::sys::clean_path(&script));
    assert_eq!(&launch.args[launch.args.len() - 2..], ["--", prompt]);

    let at = launch.args.iter().position(|a| a == "--settings").expect("--settings");
    let file = PathBuf::from(&launch.args[at + 1]);
    assert_eq!(file, agents::settings_file("s1"));
    assert!(file.starts_with(home.path()), "{}", file.display());
    let settings: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let hooks = settings["hooks"].as_object().unwrap();
    assert_eq!(hooks.len(), 4);
    for event in ["SessionStart", "UserPromptSubmit", "Stop", "Notification"] {
        assert_eq!(hooks[event][0]["hooks"][0]["command"], "glasscode hook", "{event}");
    }
    assert_eq!(settings["statusLine"]["command"], "glasscode statusline");

    // The file goes with the session.
    agents::remove_session_files("s1");
    assert!(!file.exists());
}
