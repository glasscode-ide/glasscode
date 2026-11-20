//! Finding earlier Claude and Codex conversations of a folder, and resuming them.

use std::collections::HashMap;

use glasscode_core::agents;
use glasscode_core::history::{claude_folder_name, conversations};
use glasscode_core::model::{Account, AgentKind, Session};
use serde_json::json;

fn write_lines(path: &std::path::Path, lines: &[serde_json::Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(path, text).unwrap();
}

#[test]
fn lists_and_resumes_conversations_of_a_folder() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("GLASSCODE_HOME", home.path()) };
    let folder = home.path().join("repo").join("wt");
    std::fs::create_dir_all(&folder).unwrap();
    let cwd = folder.to_string_lossy().to_string();
    let claude = Account { id: "claude-work".into(), agent: AgentKind::Claude, label: "Work".into(), plan: None, system_default: false };
    let codex = Account { id: "codex-work".into(), agent: AgentKind::Codex, label: "Work".into(), plan: None, system_default: false };

    // Claude: a titled conversation, one named by its first real prompt (after a slash
    // command), a `claude -p` run and one from another folder that maps to the same name.
    let projects = claude.effective_dir().join("projects").join(claude_folder_name(&folder));
    let user = |id: &str, text: &str, entry: &str, cwd: &str| json!({"type": "user", "sessionId": id, "cwd": cwd, "entrypoint": entry, "message": {"role": "user", "content": text}});
    write_lines(&projects.join("aaa.jsonl"), &[
        user("aaa", "why is the notice shown twice?", "cli", &cwd),
        json!({"type": "ai-title", "aiTitle": "Stop repeating the notice", "sessionId": "aaa"}),
        json!({"type": "last-prompt", "lastPrompt": "push it", "sessionId": "aaa"}),
    ]);
    write_lines(&projects.join("bbb.jsonl"), &[
        user("bbb", "<command-name>/clear</command-name>", "cli", &cwd),
        user("bbb", "write a test for reverify", "cli", &cwd),
    ]);
    write_lines(&projects.join("ccc.jsonl"), &[user("ccc", "summarize", "sdk-cli", &cwd)]);
    write_lines(&projects.join("ddd.jsonl"), &[user("ddd", "hi", "cli", &format!("{cwd}.x"))]);

    let list = conversations(AgentKind::Claude, &[claude.clone(), codex.clone()], &folder);
    let titles: Vec<&str> = list.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(list.len(), 2, "{titles:?}");
    assert!(titles.contains(&"Stop repeating the notice") && titles.contains(&"write a test for reverify"));
    let titled = list.iter().find(|c| c.id == "aaa").unwrap();
    assert_eq!((titled.account_id.as_str(), titled.last_prompt.as_deref()), ("claude-work", Some("push it")));

    // Codex: an interactive conversation (title from history.jsonl), a `codex exec` run and a
    // sub-agent thread.
    let day = codex.effective_dir().join("sessions/2026/10/13");
    let meta = |id: &str, source: serde_json::Value, thread: &str| json!({"type": "session_meta", "payload": {"id": id, "cwd": cwd, "source": source, "originator": "codex-tui", "thread_source": thread}});
    write_lines(&day.join("rollout-2026-10-13T10-00-00-x1.jsonl"), &[meta("x1", json!("cli"), "user")]);
    write_lines(&day.join("rollout-2026-10-13T10-00-01-x2.jsonl"), &[meta("x2", json!("exec"), "user")]);
    write_lines(&day.join("rollout-2026-10-13T10-00-02-x3.jsonl"), &[meta("x3", json!({"subagent": {}}), "subagent")]);
    write_lines(&codex.effective_dir().join("history.jsonl"), &[
        json!({"session_id": "x1", "text": "review the dedupe logic", "ts": 1}),
        json!({"session_id": "x1", "text": "add a test", "ts": 2}),
    ]);
    let list = conversations(AgentKind::Codex, &[claude.clone(), codex.clone()], &folder);
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].id.as_str(), list[0].title.as_str(), list[0].last_prompt.as_deref()), ("x1", "review the dedupe logic", Some("add a test")));

    // Resuming: a session carrying the conversation id starts `claude --resume` / `codex resume`.
    let session = |agent, id: &str| Session {
        id: "s1".into(),
        project_id: "p".into(),
        worktree: folder.clone(),
        branch: "b".into(),
        title: "t".into(),
        agent,
        account_id: None,
        agent_session_id: Some(id.into()),
        transcript_path: None,
        lineage: vec![],
        initial_prompt: None,
        team_task: None,
        model: None,
        mode: None,
        review: None,
        created_at: 0,
    };
    let env = HashMap::new();
    let launch = agents::launch(&session(AgentKind::Claude, "aaa"), Some(&claude), &env);
    assert_eq!(&launch.args[..2], &["--resume".to_string(), "aaa".to_string()]);
    let launch = agents::launch(&session(AgentKind::Codex, "x1"), Some(&codex), &env);
    // Codex's `resume` comes after its options (the `-c` overrides).
    assert_eq!(&launch.args[launch.args.len() - 2..], &["resume".to_string(), "x1".to_string()]);
    assert!(launch.args[..launch.args.len() - 2].chunks(2).all(|pair| pair[0] == "-c"), "{:?}", launch.args);
}
