//! The Server mode switch against a real glasscoded (`script/test-server/run.sh`, with
//! glasscoded installed in it): a worktree moves to the server with both of its sessions, as one
//! unit under an owner record, and back; a held session made twice is one session, and a move
//! given up leaves nothing behind.
//!
//!   script/test-server/run.sh
//!   (copy target/glasscoded/glasscoded-linux-<arch> to ~/.glasscode/bin/glasscoded there)
//!   GLASSCODE_TEST_SSH_CONFIG=$PWD/target/test-server/ssh_config \
//!     cargo test -p glasscode_core --test switch_e2e -- --ignored

use std::path::{Path, PathBuf};
use std::process::Command;

use glasscode_core::model::{AgentKind, Project, Session, State, new_id};
use glasscode_core::owners::{OwnerRecord, WorktreeKey};
use glasscode_core::paths::MachineId;
use glasscode_core::remote::{CreateSession, MoveTag, OwnersBegin, OwnersReply, RemoteClient, RemoteSession, ServerConfig, ServerState};
use glasscode_core::switch;
use serde_json::{Value, json};

fn git(dir: &Path, config: &ServerConfig, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .env("GIT_SSH_COMMAND", config.git_ssh_command())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn on_server(config: &ServerConfig, script: &str) -> String {
    let (program, args) = config.remote_command(false, &["sh".into(), "-c".into(), script.into()]);
    let out = Command::new(program).args(args).output().unwrap();
    assert!(out.status.success(), "{script}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn shell(project: &str, worktree: &Path, title: &str) -> Session {
    serde_json::from_value(json!({
        "id": new_id(), "project_id": project, "worktree": worktree, "branch": "fix/retry",
        "title": title, "agent": AgentKind::Shell, "created_at": 0
    }))
    .unwrap()
}

fn record(client: &RemoteClient, key: &WorktreeKey) -> Option<OwnerRecord> {
    let records: std::collections::HashMap<WorktreeKey, OwnerRecord> = client.call("owners.get", json!({ "keys": [key] })).unwrap();
    records.get(key).cloned()
}

#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn a_worktree_moves_with_all_its_sessions_and_back() {
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    // A clean daemon, only on the throwaway server (its image leaves this file).
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");
    on_server(&config, "pkill -f '[g]lasscoded serve'; sleep 1; rm -rf ~/.glasscode/authority ~/.glasscode/state.json ~/.glasscode/glasscoded.json ~/.glasscode/mirrors; true");
    let client = RemoteClient::connect(&config).expect("glasscoded answers on the test server");
    assert!(client.hello.features.iter().any(|f| f == "owners"), "this glasscoded moves worktrees by owner record");
    let server_home = client.hello.home.clone();

    // A repository there, a project of glasscoded's, and its clone here.
    let bare = server_home.join("code/app-e2e.git");
    on_server(
        &config,
        &format!(
            "rm -rf ~/code/app-e2e.git ~/code/app-e2e ~/code/seed-e2e ~/.glasscode/worktrees/app-e2e && mkdir -p ~/code && git init -q --bare -b main ~/code/app-e2e.git && git clone -q ~/code/app-e2e.git ~/code/seed-e2e && cd ~/code/seed-e2e && echo one > a.txt && git add a.txt && git -c user.name=t -c user.email=t@example.com commit -qm init && git push -q origin HEAD:main"
        ),
    );
    let server_project: Project = client.call("projects.add", json!({ "url": bare, "path": server_home.join("code/app-e2e"), "name": "app-e2e" })).unwrap();
    let root = home.path().join("code/app-e2e");
    std::fs::create_dir_all(root.parent().unwrap()).unwrap();
    git(home.path(), &config, &["clone", "-q", &config.git_url(&bare), root.to_str().unwrap()]);
    let worktree = home.path().join(".glasscode/worktrees/app-e2e/fix-retry");
    git(&root, &config, &["worktree", "add", "-q", "-b", "fix/retry", worktree.to_str().unwrap()]);
    std::fs::write(worktree.join("note.txt"), "not committed yet\n").unwrap();

    let mut local = State::default();
    local.projects.push(serde_json::from_value(json!({ "id": "lp1", "name": "app-e2e", "root": root })).unwrap());
    local.sessions.push(shell("lp1", &worktree, "Shell · one"));
    local.sessions.push(shell("lp1", &worktree, "Shell · two"));
    let mac = MachineId::this().unwrap();

    // 1. The worktree goes with both sessions, at epoch 1.
    let units = switch::units_to_server(&local);
    assert_eq!(units.len(), 1);
    let unit = &units[0];
    assert_eq!(unit.sessions.len(), 2);
    let moved = switch::move_unit_to_server(&client, &config, &local, unit, &mac).unwrap();
    assert_eq!((moved.epoch, moved.sessions.len()), (1, 2));
    let owner = record(&client, &unit.key).unwrap();
    assert_eq!((owner.owner, owner.epoch, owner.moving), (client.hello.machine.clone().unwrap(), 1, None));
    let state: ServerState = client.call("state", json!({})).unwrap();
    let there: Vec<&RemoteSession> = state.sessions.iter().filter(|r| r.session.worktree == unit.key.path_under(&server_home)).collect();
    assert_eq!(there.len(), 2, "both sessions run there, in one worktree");
    assert_eq!(on_server(&config, "cat ~/.glasscode/worktrees/app-e2e/fix-retry/note.txt"), "not committed yet");
    // A step of a move that already committed is refused, never a second session.
    let late = CreateSession {
        held: Some(MoveTag { key: unit.key.clone(), epoch: 1, from: unit.sessions[0].clone() }),
        ..serde_json::from_value(json!({ "project_id": server_project.id, "worktree": unit.key.path_under(&server_home), "agent": "shell" })).unwrap()
    };
    assert!(client.call::<Value>("sessions.create", serde_json::to_value(&late).unwrap()).is_err());

    // 2. A move given up: a held session made twice is one, and it goes on abort.
    let other = WorktreeKey(".glasscode/worktrees/app-e2e/other".into());
    let begun: OwnersReply = client
        .call("owners.begin", serde_json::to_value(OwnersBegin { key: other.clone(), from: mac.clone(), to: client.hello.machine.clone().unwrap(), epoch: 0 }).unwrap())
        .unwrap();
    assert!(matches!(begun, OwnersReply::Ok { .. }));
    let held = CreateSession {
        held: Some(MoveTag { key: other.clone(), epoch: 1, from: "mac-session".into() }),
        ..serde_json::from_value(json!({ "project_id": server_project.id, "worktree": server_project.root, "agent": "shell" })).unwrap()
    };
    let first: RemoteSession = client.call("sessions.create", serde_json::to_value(&held).unwrap()).unwrap();
    let again: RemoteSession = client.call("sessions.create", serde_json::to_value(&held).unwrap()).unwrap();
    assert_eq!(first.session.id, again.session.id, "a retry gives the same session");
    assert!(!first.running, "a held session waits for the commit");
    client.call::<Value>("owners.abort", json!({ "key": other, "epoch": 1 })).unwrap();
    let state: ServerState = client.call("state", json!({})).unwrap();
    assert!(state.sessions.iter().all(|r| r.session.id != first.session.id), "the abort removed it");

    // 2b. A worktree with uncommitted work and no session leaves a copy, without moving; opened
    // there, it moves.
    let idle_path = home.path().join(".glasscode/worktrees/app-e2e/idle");
    git(&root, &config, &["worktree", "add", "-q", "-b", "idle", idle_path.to_str().unwrap()]);
    std::fs::write(idle_path.join("draft.txt"), "a draft\n").unwrap();
    let worktrees: std::collections::HashMap<String, Vec<glasscode_core::git::Worktree>> = [("lp1".to_string(), glasscode_core::git::worktrees(&root).unwrap())].into();
    let idle = switch::idle_worktrees(&local, &worktrees);
    assert_eq!(idle.len(), 1, "only the worktree without a session: {idle:?}");
    switch::park_on_server(&client, &config, &local, &idle[0], &mac).unwrap();
    let state: ServerState = client.call("state", json!({})).unwrap();
    assert!(state.parked.iter().any(|p| p.key == idle[0].key));
    assert!(record(&client, &idle[0].key).is_none(), "parking does not move it");
    let _: PathBuf = client.call("worktrees.open_parked", json!({ "key": idle[0].key })).unwrap();
    assert_eq!(on_server(&config, "cat ~/.glasscode/worktrees/app-e2e/idle/draft.txt"), "a draft");
    let taken = record(&client, &idle[0].key).unwrap();
    assert_eq!((taken.owner, taken.epoch), (client.hello.machine.clone().unwrap(), 1), "opened there, the server owns it");

    // 3. Back to this Mac, at epoch 2: the server's copies stop, then close.
    let back = switch::units_to_mac(&state, &server_home).into_iter().find(|u| u.key == unit.key).unwrap();
    assert_eq!(back.sessions.len(), 2);
    std::fs::write(worktree.join("note.txt"), "changed here meanwhile\n").unwrap();
    on_server(&config, "echo 'written on the server' > ~/.glasscode/worktrees/app-e2e/fix-retry/server.txt");
    let home_again = switch::move_unit_to_mac(&client, &config, &local, &back, &mac).unwrap();
    assert_eq!((home_again.epoch, home_again.sessions.len()), (2, 2));
    assert_eq!(std::fs::read_to_string(worktree.join("server.txt")).unwrap(), "written on the server\n");
    assert_eq!(std::fs::read_to_string(worktree.join("note.txt")).unwrap(), "not committed yet\n", "the server's version is checked out");
    let stash = Command::new("git").arg("-C").arg(&worktree).args(["stash", "list"]).output().unwrap();
    assert!(String::from_utf8_lossy(&stash.stdout).contains("Glasscode: kept"), "the edit made here meanwhile is kept in a stash");
    assert_eq!(record(&client, &unit.key).unwrap().owner, mac);
    switch::close_on_server(&client, &home_again).unwrap();
    let state: ServerState = client.call("state", json!({})).unwrap();
    assert!(state.sessions.iter().all(|r| r.session.worktree != unit.key.path_under(&server_home)), "nothing runs in two places");
    let _: PathBuf = worktree;
}
