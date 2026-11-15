//! The console program as a person or an agent runs it.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use glasscode_core::control::{Reply, Request, code};
use glasscode_core::sys;

/// A channel nobody else uses: a socket in the test's folder, or a pipe named after it.
fn channel(home: &tempfile::TempDir) -> PathBuf {
    if cfg!(windows) {
        let name = home.path().file_name().unwrap().to_string_lossy().into_owned();
        PathBuf::from(format!(r"\\.\pipe\glasscode-cli-test-{name}"))
    } else {
        home.path().join("control.sock")
    }
}

/// `glasscode-cli` with a Glasscode folder and a channel of its own, outside any session.
fn glasscode(home: &tempfile::TempDir, args: &[&str]) -> Command {
    let mut command = sys::command(env!("CARGO_BIN_EXE_glasscode-cli"));
    command.args(args).env("GLASSCODE_HOME", home.path()).env("GLASSCODE_CONTROL_SOCKET", channel(home)).stdin(Stdio::null());
    for name in ["GLASSCODE_CLI_FILES", "GLASSCODE_WORKTREE", "GLASSCODE_SESSION_ID", "GLASSCODE_SESSION_SIG"] {
        command.env_remove(name);
    }
    command
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn help_lists_the_commands() {
    let home = tempfile::tempdir().unwrap();
    let Output { status, stdout, .. } = glasscode(&home, &["help"]).output().unwrap();
    assert_eq!(status.code(), Some(code::OK));
    assert!(text(&stdout).contains("board add"), "{}", text(&stdout));
}

#[test]
fn without_the_app_it_exits_with_3() {
    let home = tempfile::tempdir().unwrap();
    let Output { status, stderr, .. } = glasscode(&home, &["board"]).output().unwrap();
    assert_eq!(status.code(), Some(code::NOT_RUNNING), "{}", text(&stderr));
    assert!(text(&stderr).contains("Glasscode is not running"), "{}", text(&stderr));
}

#[test]
fn a_command_reaches_the_app_and_prints_its_answer() {
    let home = tempfile::tempdir().unwrap();
    let listener = sys::control::listen(&channel(&home)).unwrap();
    let child = glasscode(&home, &["board", "--json"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let pid = child.id();
    // The app's side: one request line in, one answer line out.
    let stream = listener.accept().unwrap();
    assert_eq!(stream.peer_pid(), Some(pid));
    let mut line = String::new();
    BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
    let request: Request = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(request.args, ["board"]);
    let reply = Reply::ok("1 task", serde_json::json!({ "tasks": [{ "title": "Test from Windows" }] }));
    let mut out = stream;
    writeln!(out, "{}", serde_json::to_string(&reply).unwrap()).unwrap();
    drop(out);
    let Output { status, stdout, stderr } = child.wait_with_output().unwrap();
    assert_eq!(status.code(), Some(code::OK), "{}", text(&stderr));
    let printed: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(printed["tasks"][0]["title"], "Test from Windows");
}
