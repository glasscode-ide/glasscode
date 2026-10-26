use std::time::{Duration, SystemTime};

use glasscode_core::agents::find_codex_conversation;

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
