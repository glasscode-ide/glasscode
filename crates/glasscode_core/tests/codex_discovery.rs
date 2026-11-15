use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use glasscode_core::agents::find_codex_conversation;

/// A rollout whose header (`session_meta`) has `cwd`, in a day folder of `sessions`.
fn write_rollout(codex_home: &Path, id: &str, cwd: &str) -> PathBuf {
    let day = codex_home.join("sessions").join("2026").join("11").join("15");
    std::fs::create_dir_all(&day).unwrap();
    let header = serde_json::json!({ "type": "session_meta", "payload": { "id": id, "cwd": cwd, "source": "cli" } });
    let path = day.join(format!("rollout-2026-11-15T10-00-00-{id}.jsonl"));
    std::fs::write(&path, format!("{header}\n")).unwrap();
    path
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
