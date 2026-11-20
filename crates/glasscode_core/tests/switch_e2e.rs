//! The Server mode switch against a real glasscoded (`script/test-server/run.sh`, with
//! glasscoded installed in it): a worktree moves to the server with both of its sessions, as one
//! unit under an owner record, and back; a held session made twice is one session, and a move
//! given up leaves nothing behind.
//!
//!   script/test-server/run.sh
//!   (copy target/glasscoded/glasscoded-linux-<arch> to ~/.glasscode/bin/glasscoded there)
//!   GLASSCODE_TEST_SSH_CONFIG=$PWD/target/test-server/ssh_config \
//!     cargo test -p glasscode_core --test switch_e2e -- --ignored --test-threads=1
//!
//! (Each test sets `HOME` for the process, so they run one at a time.)

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
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
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
    // With no session there, it still comes home (Laptop mode lists it), at epoch 2.
    let idle_home = switch::Unit { key: idle[0].key.clone(), project_id: server_project.id.clone(), worktree: idle[0].key.path_under(&server_home), sessions: Vec::new(), titles: Vec::new(), main: false };
    let came = switch::move_unit_to_mac(&client, &config, &local, &idle_home, &mac).unwrap();
    assert_eq!((came.epoch, came.sessions.len()), (2, 0));
    assert_eq!(record(&client, &idle[0].key).unwrap().owner, mac);
    // A move given up leaves no record behind for a worktree that never moved.
    assert!(record(&client, &other).is_none(), "the aborted move left nothing");

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

/// The owner's case: 10 worktrees in one project and 20 in another, two with three sessions,
/// moved four at a time (as the switch does, each on its own connection): every session ends on
/// the server exactly once, under its owner record.
#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn thirty_units_two_projects_four_at_a_time() {
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");
    on_server(&config, "pkill -f '[g]lasscoded serve'; sleep 1; rm -rf ~/.glasscode/authority ~/.glasscode/state.json ~/.glasscode/glasscoded.json ~/.glasscode/mirrors ~/.glasscode/worktrees ~/code; true");
    let client = RemoteClient::connect(&config).unwrap();
    let server_home = client.hello.home.clone();
    let mac = MachineId::this().unwrap();
    let mut local = State::default();
    for (name, count) in [("alpha", 10), ("beta", 20)] {
        on_server(&config, &format!("mkdir -p ~/code && git init -q --bare -b main ~/code/{name}.git && git clone -q ~/code/{name}.git ~/code/seed-{name} && cd ~/code/seed-{name} && echo one > a.txt && git add a.txt && git -c user.name=t -c user.email=t@example.com commit -qm init && git push -q origin HEAD:main"));
        let bare = server_home.join(format!("code/{name}.git"));
        let _: Project = client.call("projects.add", json!({ "url": bare, "path": server_home.join(format!("code/{name}")), "name": name })).unwrap();
        let root = home.path().join(format!("code/{name}"));
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        git(home.path(), &config, &["clone", "-q", &config.git_url(&bare), root.to_str().unwrap()]);
        let id = format!("p-{name}");
        local.projects.push(serde_json::from_value(json!({ "id": id, "name": name, "root": root })).unwrap());
        for i in 0..count {
            let worktree = home.path().join(format!(".glasscode/worktrees/{name}/w{i}"));
            git(&root, &config, &["worktree", "add", "-q", "-b", &format!("w{i}"), worktree.to_str().unwrap()]);
            std::fs::write(worktree.join("work.txt"), format!("{name} {i}\n")).unwrap();
            let sessions = if i == 0 { 3 } else { 1 };
            for n in 0..sessions {
                local.sessions.push(shell(&id, &worktree, &format!("{name} w{i} #{n}")));
            }
        }
    }
    let units = switch::units_to_server(&local);
    assert_eq!(units.len(), 30);
    assert_eq!(local.sessions.len(), 34);

    let started = std::time::Instant::now();
    let queue = std::sync::Mutex::new(units.clone());
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let client = RemoteClient::connect(&config).unwrap();
                loop {
                    let Some(unit) = queue.lock().unwrap().pop() else { break };
                    let result = switch::move_unit_to_server(&client, &config, &local, &unit, &mac).map(|m| m.sessions.len());
                    results.lock().unwrap().push((unit.key.clone(), result.map_err(|e| format!("{e:#}"))));
                }
            });
        }
    });
    let took = started.elapsed();
    let results = results.into_inner().unwrap();
    let failed: Vec<_> = results.iter().filter(|(_, r)| r.is_err()).collect();
    assert!(failed.is_empty(), "moves failed: {failed:#?}");
    eprintln!("30 worktrees, 34 sessions, four at a time: {:.1} s ({:.2} s per worktree)", took.as_secs_f64(), took.as_secs_f64() / 30.0);

    let state: ServerState = client.call("state", json!({})).unwrap();
    assert_eq!(state.sessions.len(), 34, "every session exactly once");
    for unit in &units {
        let record = record(&client, &unit.key).unwrap();
        assert_eq!((record.epoch, record.moving), (1, None), "{}", unit.key.0);
        let there = state.sessions.iter().filter(|r| r.session.worktree == unit.key.path_under(&server_home)).count();
        assert_eq!(there, unit.sessions.len(), "{}: its sessions, together", unit.key.0);
    }
}

/// After a trip to the server and back (the record gives the worktree to the Mac at epoch 2),
/// the fallback take-over still continues it, with every Mac session in it; the record moves to
/// the server first. A project's main checkout is never a unit. A move a restart interrupted is
/// finished, or given up, by the owner record. Several moves into a project the server does not
/// have yet add it once.
#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn take_over_main_checkout_recovery_and_a_new_project() {
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");
    on_server(&config, "pkill -f '[g]lasscoded serve'; sleep 1; rm -rf ~/.glasscode/authority ~/.glasscode/state.json ~/.glasscode/glasscoded.json ~/.glasscode/mirrors ~/.glasscode/worktrees ~/code; true");
    let client = RemoteClient::connect(&config).unwrap();
    let server_home = client.hello.home.clone();
    let server_id = client.hello.machine.clone().unwrap();
    let mac = MachineId::this().unwrap();

    // A project on both machines, a worktree with two sessions, one session in the main checkout.
    on_server(&config, "mkdir -p ~/code && git init -q --bare -b main ~/code/tk.git && git clone -q ~/code/tk.git ~/code/seed-tk && cd ~/code/seed-tk && echo one > a.txt && git add a.txt && git -c user.name=t -c user.email=t@example.com commit -qm init && git push -q origin HEAD:main");
    let bare = server_home.join("code/tk.git");
    let server_project: Project = client.call("projects.add", json!({ "url": bare, "path": server_home.join("code/tk"), "name": "tk" })).unwrap();
    let root = home.path().join("code/tk");
    std::fs::create_dir_all(root.parent().unwrap()).unwrap();
    git(home.path(), &config, &["clone", "-q", &config.git_url(&bare), root.to_str().unwrap()]);
    let worktree = home.path().join(".glasscode/worktrees/tk/fix");
    git(&root, &config, &["worktree", "add", "-q", "-b", "fix", worktree.to_str().unwrap()]);
    let mut local = State::default();
    local.projects.push(serde_json::from_value(json!({ "id": "lt", "name": "tk", "root": root })).unwrap());
    local.sessions.push(shell("lt", &worktree, "Shell · a"));
    local.sessions.push(shell("lt", &worktree, "Shell · b"));
    local.sessions.push(shell("lt", &root, "Shell · main"));

    // The main checkout is never a unit with an owner record.
    let units = switch::units_to_server(&local);
    let main = units.iter().find(|u| u.main).expect("the main checkout's unit is marked");
    assert!(switch::move_unit_to_server(&client, &config, &local, main, &mac).is_err(), "it moves session by session");
    let unit = units.iter().find(|u| !u.main).unwrap().clone();

    // There and back: the record gives it to the Mac at epoch 2.
    let there = switch::move_unit_to_server(&client, &config, &local, &unit, &mac).unwrap();
    let state: ServerState = client.call("state", json!({})).unwrap();
    let back_unit = switch::units_to_mac(&state, &server_home).into_iter().find(|u| u.key == unit.key).unwrap();
    let back = switch::move_unit_to_mac(&client, &config, &local, &back_unit, &mac).unwrap();
    switch::close_on_server(&client, &back).unwrap();
    assert_eq!(record(&client, &unit.key).unwrap().epoch, 2);
    let _ = there;

    // A new session in that worktree on the server is refused, with words for the person.
    let create: CreateSession = serde_json::from_value(json!({ "project_id": server_project.id, "worktree": unit.key.path_under(&server_home), "agent": "shell" })).unwrap();
    let refused = client.call::<Value>("sessions.create", serde_json::to_value(&create).unwrap()).unwrap_err();
    assert!(format!("{refused:#}").contains("use Server mode"), "{refused:#}");

    // The Mac syncs its two sessions (as after coming back), then the server continues one: both
    // continue, and the record is the server's at epoch 3.
    let mut home_state = local.clone();
    home_state.sessions = back.sessions.iter().map(|(_, s)| s.clone()).collect();
    let mut syncer = glasscode_core::mirror::Syncer::default();
    let live: std::collections::HashMap<String, glasscode_core::mirror::Live> = home_state.sessions.iter().map(|s| (s.id.clone(), glasscode_core::mirror::Live { running: true, activity: None, prompt: None })).collect();
    let extras = glasscode_core::mirror::Extras { worktree_keys: Vec::new(), board: None, asleep: false, today: None, cli_replies: Vec::new(), today_acks: Vec::new(), cli_acks: Vec::new(), skill_off: false, authority: None, journal_sent: None, manager: None };
    syncer.sync(&client, &config, &home_state, &live, "test-mac", "test-mac-id", extras).unwrap();
    let first = home_state.sessions[0].id.clone();
    let continued: RemoteSession = client.call("mirror.take_over", json!({ "session_id": first })).unwrap();
    assert!(continued.running, "the take-over started it");
    let taken = record(&client, &unit.key).unwrap();
    assert_eq!((taken.owner, taken.epoch), (server_id.clone(), 3));
    let state: ServerState = client.call("state", json!({})).unwrap();
    let continuing = state.sessions.iter().filter(|r| r.session.worktree == unit.key.path_under(&server_home)).count();
    assert_eq!(continuing, 2, "the other session of the worktree continued too");

    // A restart after a commit: the note finishes it from the record.
    let note = switch::MoveNote { direction: switch::Direction::ToServer, unit: unit.clone() };
    match switch::recover(&client, &config, &local, &note, &mac).unwrap() {
        switch::Recovered::ToServer(sessions) => assert_eq!(sessions.len(), 2),
        _ => panic!("the record says it is on the server"),
    }
    // A restart in the middle of a move: given up, and nothing of it stays.
    let mid = WorktreeKey(".glasscode/worktrees/tk/mid".into());
    let _: OwnersReply = client.call("owners.begin", serde_json::to_value(OwnersBegin { key: mid.clone(), from: mac.clone(), to: server_id.clone(), epoch: 0 }).unwrap()).unwrap();
    let mid_note = switch::MoveNote { direction: switch::Direction::ToServer, unit: switch::Unit { key: mid.clone(), project_id: "lt".into(), worktree: home.path().join(".glasscode/worktrees/tk/mid"), sessions: Vec::new(), titles: Vec::new(), main: false } };
    assert!(matches!(switch::recover(&client, &config, &local, &mid_note, &mac).unwrap(), switch::Recovered::NotMoved));
    assert!(record(&client, &mid).is_none());

    // Four worktrees of a project the server does not have yet, moved at once: it is added once.
    on_server(&config, "git init -q --bare -b main ~/code/fresh.git && git clone -q ~/code/fresh.git ~/code/seed-fresh && cd ~/code/seed-fresh && echo one > a.txt && git add a.txt && git -c user.name=t -c user.email=t@example.com commit -qm init && git push -q origin HEAD:main");
    let fresh_root = home.path().join("code/fresh");
    git(home.path(), &config, &["clone", "-q", &config.git_url(&server_home.join("code/fresh.git")), fresh_root.to_str().unwrap()]);
    // Its origin, as the server reaches it.
    git(&fresh_root, &config, &["remote", "set-url", "origin", server_home.join("code/fresh.git").to_str().unwrap()]);
    let mut fresh = State::default();
    fresh.projects.push(serde_json::from_value(json!({ "id": "lf", "name": "fresh", "root": fresh_root })).unwrap());
    for i in 0..4 {
        let w = home.path().join(format!(".glasscode/worktrees/fresh/w{i}"));
        git(&fresh_root, &config, &["worktree", "add", "-q", "-b", &format!("w{i}"), w.to_str().unwrap()]);
        fresh.sessions.push(shell("lf", &w, &format!("fresh w{i}")));
    }
    let fresh_units = switch::units_to_server(&fresh);
    let failures = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for unit in &fresh_units {
            let (config, fresh, mac, failures) = (&config, &fresh, &mac, &failures);
            scope.spawn(move || {
                let client = RemoteClient::connect(config).unwrap();
                if let Err(err) = switch::move_unit_to_server(&client, config, fresh, unit, mac) {
                    failures.lock().unwrap().push(format!("{err:#}"));
                }
            });
        }
    });
    assert!(failures.lock().unwrap().is_empty(), "{:?}", failures.lock().unwrap());
    let state: ServerState = client.call("state", json!({})).unwrap();
    assert_eq!(state.projects.iter().filter(|p| p.name == "fresh").count(), 1, "the project was added once");
}

/// The Today board on the server (server-sync phase 5): the Mac's board merges into the server's
/// copy at each sync; with the Mac away, `today.get` answers from the server's board and
/// `today.op` changes it there; the Mac's next sync brings the change back.
#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn the_today_board_lives_on_the_server_too() {
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");
    on_server(&config, "pkill -f '[g]lasscoded serve'; sleep 1; rm -rf ~/.glasscode/authority ~/.glasscode/state.json ~/.glasscode/glasscoded.json ~/.glasscode/mirrors ~/.glasscode/board.json*; true");
    let client = RemoteClient::connect(&config).unwrap();

    // The Mac's board, with a task, goes with a sync.
    let mut mac_board = glasscode_core::board::Board::default();
    mac_board.tasks.push(serde_json::from_value(json!({ "id": "abcd", "title": "Send Sam the wording", "created_at": 0, "updated_at": 0 })).unwrap());
    mac_board.save().unwrap();
    let sync = |board: &glasscode_core::board::Board| {
        let extras = glasscode_core::mirror::Extras { worktree_keys: Vec::new(), board: serde_json::to_value(board).ok(), asleep: false, today: None, cli_replies: Vec::new(), today_acks: Vec::new(), cli_acks: Vec::new(), skill_off: false, authority: None, journal_sent: None, manager: None };
        glasscode_core::mirror::Syncer::default().sync(&client, &config, &State::default(), &Default::default(), "test-mac", "test-mac-id", extras).unwrap()
    };
    let outcome = sync(&mac_board);
    assert!(outcome.board.as_ref().unwrap().task("abcd").is_some(), "the server has it now");

    // The Mac away (it sent no Today snapshot): the server answers from its own board.
    let got: Value = client.call("today.get", json!({})).unwrap();
    assert_eq!(got["from"], "server");
    assert!(got["snapshot"].to_string().contains("Send Sam the wording"), "{got}");

    // A change there, with the Mac away: done on the server's copy.
    let applied: Value = client.call("today.op", json!({ "op": "add", "text": "Book the train for Monday" })).unwrap();
    assert_eq!(applied["applied"], true);
    let got: Value = client.call("today.get", json!({})).unwrap();
    assert!(got["snapshot"].to_string().contains("Book the train"), "{got}");

    // The Mac's next sync brings it here, and keeps the Mac's own task.
    let outcome = sync(&mac_board);
    let server_board = outcome.board.unwrap();
    assert!(mac_board.merge(&server_board));
    assert!(mac_board.tasks.iter().any(|t| t.title.contains("Book the train")));
    assert!(mac_board.task("abcd").is_some());
    // An approval is still the Mac's.
    let decide = client.call::<Value>("today.op", json!({ "op": "decide", "approval": 1, "allow": true }));
    assert!(decide.is_err(), "no Mac connected: only the Mac answers an agent's request");
}

/// The last sync before the Mac sleeps says so: a session that was working can be continued on
/// the server at once, not only after the usual wait.
#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn a_working_session_continues_at_once_when_the_mac_said_it_sleeps() {
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");
    on_server(&config, "pkill -f '[g]lasscoded serve'; sleep 1; rm -rf ~/.glasscode/authority ~/.glasscode/state.json ~/.glasscode/glasscoded.json ~/.glasscode/mirrors ~/.glasscode/worktrees ~/code; true");
    let client = RemoteClient::connect(&config).unwrap();
    let server_home = client.hello.home.clone();
    on_server(&config, "mkdir -p ~/code && git init -q --bare -b main ~/code/zz.git && git clone -q ~/code/zz.git ~/code/seed-zz && cd ~/code/seed-zz && echo one > a.txt && git add a.txt && git -c user.name=t -c user.email=t@example.com commit -qm init && git push -q origin HEAD:main");
    let bare = server_home.join("code/zz.git");
    let _: Project = client.call("projects.add", json!({ "url": bare, "path": server_home.join("code/zz"), "name": "zz" })).unwrap();
    let root = home.path().join("code/zz");
    std::fs::create_dir_all(root.parent().unwrap()).unwrap();
    git(home.path(), &config, &["clone", "-q", &config.git_url(&bare), root.to_str().unwrap()]);
    let worktree = home.path().join(".glasscode/worktrees/zz/w");
    git(&root, &config, &["worktree", "add", "-q", "-b", "w", worktree.to_str().unwrap()]);
    let mut local = State::default();
    local.projects.push(serde_json::from_value(json!({ "id": "lz", "name": "zz", "root": root })).unwrap());
    local.sessions.push(shell("lz", &worktree, "Shell · working"));
    let id = local.sessions[0].id.clone();
    let live: std::collections::HashMap<String, glasscode_core::mirror::Live> = [(id.clone(), glasscode_core::mirror::Live { running: true, activity: Some(glasscode_core::agents::AgentActivity::Working), prompt: None })].into();
    let mut syncer = glasscode_core::mirror::Syncer::default();
    let extras = |asleep| glasscode_core::mirror::Extras { worktree_keys: Vec::new(), board: None, asleep, today: None, cli_replies: Vec::new(), today_acks: Vec::new(), cli_acks: Vec::new(), skill_off: false, authority: None, journal_sent: None, manager: None };
    syncer.sync(&client, &config, &local, &live, "test-mac", "test-mac-id", extras(false)).unwrap();
    let refused = client.call::<Value>("mirror.take_over", json!({ "session_id": id })).unwrap_err();
    assert!(format!("{refused:#}").contains("working"), "the Mac is awake and it works: {refused:#}");
    syncer.sync(&client, &config, &local, &live, "test-mac", "test-mac-id", extras(true)).unwrap();
    let continued: RemoteSession = client.call("mirror.take_over", json!({ "session_id": id })).unwrap();
    assert!(continued.running);
}

/// A move sends only what the server's mirror of a conversation lacks: the mirrored copy goes
/// into an account there, with its folder paths rewritten to the server's worktree.
#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn a_conversation_moves_as_the_mirrors_missing_tail() {
    use base64::Engine as _;
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");
    on_server(&config, "pkill -f '[g]lasscoded serve'; sleep 1; rm -rf ~/.glasscode/authority ~/.glasscode/state.json ~/.glasscode/glasscoded.json ~/.glasscode/mirrors ~/.glasscode/worktrees ~/code; true");
    let client = RemoteClient::connect(&config).unwrap();
    let server_home = client.hello.home.clone();
    on_server(&config, "mkdir -p ~/code && git init -q --bare -b main ~/code/cv.git && git clone -q ~/code/cv.git ~/code/seed-cv && cd ~/code/seed-cv && echo one > a.txt && git add a.txt && git -c user.name=t -c user.email=t@example.com commit -qm init && git push -q origin HEAD:main");
    let bare = server_home.join("code/cv.git");
    let _: Project = client.call("projects.add", json!({ "url": bare, "path": server_home.join("code/cv"), "name": "cv" })).unwrap();
    let root = home.path().join("code/cv");
    std::fs::create_dir_all(root.parent().unwrap()).unwrap();
    git(home.path(), &config, &["clone", "-q", &config.git_url(&bare), root.to_str().unwrap()]);
    let worktree = home.path().join(".glasscode/worktrees/cv/w");
    git(&root, &config, &["worktree", "add", "-q", "-b", "w", worktree.to_str().unwrap()]);
    let mut local = State::default();
    local.projects.push(serde_json::from_value(json!({ "id": "lc", "name": "cv", "root": root })).unwrap());
    local.sessions.push(shell("lc", &worktree, "Shell · talk"));
    let id = local.sessions[0].id.clone();
    let live: std::collections::HashMap<String, glasscode_core::mirror::Live> = [(id.clone(), glasscode_core::mirror::Live { running: true, activity: None, prompt: None })].into();
    let extras = glasscode_core::mirror::Extras { worktree_keys: Vec::new(), board: None, asleep: false, today: None, cli_replies: Vec::new(), today_acks: Vec::new(), cli_acks: Vec::new(), skill_off: false, authority: None, journal_sent: None, manager: None };
    glasscode_core::mirror::Syncer::default().sync(&client, &config, &local, &live, "test-mac", "test-mac-id", extras).unwrap();

    // The mirror has the first line; the move sends the second.
    let line = |n: u32| format!("{{\"cwd\":\"{}\",\"n\":{n}}}\n", worktree.display());
    let first = line(1);
    let _: Value = client.call("mirror.conversation", json!({ "session_id": id, "offset": 0, "data": base64::engine::general_purpose::STANDARD.encode(&first) })).unwrap();
    let have: u64 = client.call("mirror.conversation_size", json!({ "session_id": id })).unwrap();
    assert_eq!(have as usize, first.len());
    let _: Value = client.call("mirror.conversation", json!({ "session_id": id, "offset": have, "data": base64::engine::general_purpose::STANDARD.encode(line(2)) })).unwrap();
    let account: Value = client.call("accounts.add", json!({ "agent": "claude", "label": "Test" })).unwrap();
    let to = server_home.join(".glasscode/worktrees/cv/w");
    let target: PathBuf = client
        .call("mirror.conversation_into", json!({ "session_id": id, "account_id": account["id"], "rel": "projects/x/talk.jsonl", "from": worktree, "to": to }))
        .unwrap();
    let written = on_server(&config, &format!("cat {}", target.display()));
    assert_eq!(written.lines().count(), 2, "{written}");
    assert!(written.contains(&to.display().to_string()) && !written.contains(&worktree.display().to_string()), "folder paths rewritten: {written}");
    // A path out of the account is refused.
    assert!(client.call::<Value>("mirror.conversation_into", json!({ "session_id": id, "account_id": account["id"], "rel": "../x", "from": worktree, "to": to })).is_err());
}

/// The manager's home (M58): the server is home until the Mac claims it; the Mac renews at its
/// generation; an older generation is refused; a release hands home back at once.
#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn the_manager_home_moves_by_claim_and_release() {
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");
    // A clean server: no home record, and no primary Mac yet (this test's Mac becomes it).
    on_server(&config, "pkill -f '[g]lasscoded serve'; sleep 1; rm -rf ~/.glasscode/authority/home.json* ~/.glasscode/authority/from-mac.json*; true");
    let client = RemoteClient::connect(&config).unwrap();
    let server_id = client.hello.machine.clone().unwrap();
    let mac = MachineId::this().unwrap();
    let record: glasscode_core::home::HomeRecord = client.call("home.get", json!({})).unwrap();
    assert_eq!((record.home.clone(), record.generation), (server_id.clone(), 0));
    let granted: glasscode_core::home::HomeRecord = client.call("home.claim", json!({})).unwrap();
    assert_eq!((granted.home.clone(), granted.generation), (mac.clone(), 1));
    let renewed: Value = client.call("home.renew", json!({ "generation": 1 })).unwrap();
    assert_eq!(renewed["ok"], true);
    let stale: Value = client.call("home.renew", json!({ "generation": 0 })).unwrap();
    assert_eq!(stale["ok"], false, "an older generation is refused");
    let released: Value = client.call("home.release", json!({ "generation": 1 })).unwrap();
    assert_eq!(released["ok"], true);
    let record: glasscode_core::home::HomeRecord = client.call("home.get", json!({})).unwrap();
    assert_eq!((record.home, record.generation), (server_id, 2), "the server is home at once");
}

/// The server's read-only copy of the memory (M20.3): a project that stays on the Mac, and an
/// employer folder only it uses, are in no object the server receives.
#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn the_servers_memory_copy_leaves_local_only_projects_out() {
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");
    on_server(&config, "chmod -R u+w ~/.glasscode/memory-server 2>/dev/null; rm -rf ~/.glasscode/memory-server ~/.glasscode/memory-server.git; true");
    let client = RemoteClient::connect(&config).unwrap();
    let repo = home.path().join("memory");
    for (path, text) in [("projects/secret-111111/PROJECT.md", "- The codename is BLUEHERON.\n"), ("org/acme/people.md", "- Pat runs ACMEPAYROLL.\n"), ("projects/open-222222/PROJECT.md", "- Builds with cargo.\n")] {
        std::fs::create_dir_all(repo.join(path).parent().unwrap()).unwrap();
        std::fs::write(repo.join(path), text).unwrap();
    }
    git(home.path(), &config, &["init", "-q", "-b", "main", repo.to_str().unwrap()]);
    git(&repo, &config, &["add", "-A"]);
    git(&repo, &config, &["commit", "-qm", "memory"]);
    let head = String::from_utf8(Command::new("git").arg("-C").arg(&repo).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap().trim().to_string();
    let key = |k: &str| glasscode_core::paths::ProjectKey::parse(k).unwrap();
    let excluded = glasscode_core::memory_copy::excluded_paths(&[key("secret-111111")], &[(key("secret-111111"), Some("acme".into())), (key("open-222222"), None)]);
    let sent = glasscode_core::memory_copy::send(&client, &config, &repo, &head, &excluded, None).unwrap();
    let got: Value = client.call("memory.copy.get", json!({})).unwrap();
    assert_eq!(got["commit"], sent);
    assert_eq!(on_server(&config, "cat ~/.glasscode/memory-server/projects/open-222222/PROJECT.md"), "- Builds with cargo.");
    assert_eq!(on_server(&config, "test -e ~/.glasscode/memory-server/projects/secret-111111 && echo there || echo gone"), "gone");
    let objects = on_server(&config, "git -C ~/.glasscode/memory-server.git cat-file --batch-all-objects --batch");
    assert!(!objects.contains("BLUEHERON") && !objects.contains("ACMEPAYROLL"), "no local-only text in any object on the server");
    assert_eq!(on_server(&config, "echo x > ~/.glasscode/memory-server/projects/open-222222/PROJECT.md 2>/dev/null && echo wrote || echo refused"), "refused", "read-only");
}

/// With the v2 engine and a server that has "memory-sync", the Mac's sync sends memory entries
/// (M57): they are merged into the server's own memory, a new head goes with the next sync, and
/// the read-only copy of M20.3 is gone from the server.
#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn the_mac_syncs_memory_entries_with_the_sync() {
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    let gitconfig = home.path().join("gitconfig");
    std::fs::write(&gitconfig, "").unwrap();
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
        std::env::set_var("GIT_CONFIG_GLOBAL", &gitconfig);
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");
    on_server(&config, "chmod -R u+w ~/.glasscode/memory-server 2>/dev/null; rm -rf ~/.glasscode/memory ~/.glasscode/authority/memory-head.json; mkdir -p ~/.glasscode/memory-server; true");
    let client = RemoteClient::connect(&config).unwrap();
    assert!(client.hello.features.iter().any(|f| f == "memory-sync"));
    // The call itself answers (an empty sync), before the syncer makes it.
    let empty: Value = client.call("memory.sync", json!({ "files": [], "have": null })).unwrap();
    assert!(empty.get("files").is_some(), "{empty}");

    let memory = glasscode_core::memstore::Memory::open();
    let notes: Vec<glasscode_core::manager::Note> = vec![serde_json::from_value(json!({ "id": "n1", "text": "Sam reviews on Fridays.", "tags": [], "source": "you", "created": 1, "updated": 1, "deleted": false })).unwrap()];
    glasscode_core::memstore::migrate::v1_notes(&memory, &notes).unwrap();
    // A v2 home, as the person's is once they switched: its engine switch on disk.
    glasscode_core::store::update::<glasscode_core::store::EngineSwitch, ()>(|switch| switch.engine = glasscode_core::store::Engine::V2).unwrap();
    let mut syncer = glasscode_core::mirror::Syncer::default();
    let extras = || glasscode_core::mirror::Extras { worktree_keys: Vec::new(), board: None, asleep: false, today: None, cli_replies: Vec::new(), today_acks: Vec::new(), cli_acks: Vec::new(), skill_off: false, authority: None, journal_sent: None, manager: None };
    syncer.sync(&client, &config, &State::default(), &Default::default(), "test-mac", "test-mac-id", extras()).unwrap();
    assert!(on_server(&config, "cat ~/.glasscode/memory/you/profile.md").contains("Sam reviews on Fridays"));
    assert_eq!(on_server(&config, "test -e ~/.glasscode/memory-server && echo there || echo gone"), "gone", "the copy is replaced");

    // A new head goes with the next sync, merged by entry.
    let more: Vec<glasscode_core::manager::Note> = vec![serde_json::from_value(json!({ "id": "n2", "text": "The release train leaves on Tuesdays.", "tags": [], "source": "you", "created": 2, "updated": 2, "deleted": false })).unwrap()];
    glasscode_core::memstore::migrate::v1_notes(&memory, &more).unwrap();
    syncer.sync(&client, &config, &State::default(), &Default::default(), "test-mac", "test-mac-id", extras()).unwrap();
    let profile = on_server(&config, "cat ~/.glasscode/memory/you/profile.md");
    assert!(profile.contains("release train") && profile.contains("Sam reviews on Fridays"));
}

/// M57 against a real glasscoded: with the v2 engine, one sync gives the server the Mac's engine,
/// its conversation change and its pending action; a change of the server's own comes back to the
/// Mac with the next sync, and nothing goes round.
#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn the_manager_stores_sync_both_ways_with_a_real_server() {
    use glasscode_core::conversation::{Author, Conversation, Draft, Kind, Stream};
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    let gitconfig = home.path().join("gitconfig");
    std::fs::write(&gitconfig, "").unwrap();
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
        std::env::set_var("GIT_CONFIG_GLOBAL", &gitconfig);
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");
    on_server(&config, "rm -rf ~/.glasscode/manager/conversation ~/.glasscode/authority/pending.json; true");
    glasscode_core::store::update::<glasscode_core::store::EngineSwitch, ()>(|switch| switch.engine = glasscode_core::store::Engine::V2).unwrap();
    let client = RemoteClient::connect(&config).unwrap();
    let server = client.hello.machine.clone().expect("the server says its machine id");

    let mut chat = Conversation::load();
    let mine = chat.post(Draft { author: Author::Chat, kind: Kind::Manager { state: Stream::Done }, text: "The e2e build is green.".into(), refs: Vec::new(), topic: None }).unwrap();
    let op = glasscode_core::ops::typed("board.add", json!({ "title": "Check the e2e card" })).unwrap();
    glasscode_core::gate::add_pending(glasscode_core::gate::PendingAction::new(&op, Default::default(), None, glasscode_core::clock::now_ms())).unwrap();
    let mut syncer = glasscode_core::mirror::Syncer::default();
    let extras = || glasscode_core::mirror::Extras { worktree_keys: Vec::new(), board: None, asleep: false, today: None, cli_replies: Vec::new(), today_acks: Vec::new(), cli_acks: Vec::new(), skill_off: false, authority: None, journal_sent: None, manager: None };
    syncer.sync(&client, &config, &State::default(), &Default::default(), "test-mac", "test-mac-id", extras()).unwrap();
    assert!(on_server(&config, "cat ~/.glasscode/authority/engine.json").contains("v2"), "the server runs as the Mac's engine says");
    assert!(on_server(&config, "cat ~/.glasscode/manager/conversation/*.jsonl").contains("The e2e build is green."));
    assert!(on_server(&config, "cat ~/.glasscode/authority/pending.json").contains("Check the e2e card"));
    // Settings › Manager › Test, v2: the server says what decides whether its manager runs there.
    let lines: Vec<Value> = client.call("manager.test", json!({})).unwrap();
    for what in ["Engine", "Home", "Permissions", "Wake lane"] {
        assert!(lines.iter().any(|l| l["what"] == what), "no {what} line: {lines:?}");
    }

    // A change of the server's own (a line its manager wrote): it comes back with the next sync.
    let mut theirs = serde_json::to_value(glasscode_core::conversation::Record::Item(mine.clone())).unwrap();
    let now = glasscode_core::clock::now_ms() + 1_000;
    theirs["id"] = json!(format!("{:024x}", now));
    theirs["text"] = json!("The server saw CI pass too.");
    for stamp in ["at", "posted"] {
        theirs[stamp] = json!({ "wall_ms": now, "count": 0, "machine": server });
    }
    let topic = theirs["topic"].as_str().unwrap().to_string();
    on_server(&config, &format!("printf '%s\\n' '{}' >> ~/.glasscode/manager/conversation/{topic}.jsonl", serde_json::to_string(&theirs).unwrap().replace('\'', "'\\''")));
    syncer.sync(&client, &config, &State::default(), &Default::default(), "test-mac", "test-mac-id", extras()).unwrap();
    let texts: Vec<String> = Conversation::load().items().iter().map(|item| item.text.clone()).collect();
    assert!(texts.contains(&"The server saw CI pass too.".to_string()), "{texts:?}");
    // A window op the server's manager queued while the Mac was away: it comes once, and leaves
    // the server's queue when the Mac acknowledged it.
    let queued = json!({ "next": 7, "ops": [{ "id": 7, "op": { "op": "ui.say", "args": { "text": "From the server." } }, "queued_ms": glasscode_core::clock::now_ms() }] });
    on_server(&config, &format!("mkdir -p ~/.glasscode/manager && printf '%s' '{queued}' > ~/.glasscode/manager/window-queue.json"));
    let first = syncer.sync(&client, &config, &State::default(), &Default::default(), "test-mac", "test-mac-id", extras()).unwrap();
    assert_eq!(first.window_ops.iter().map(|q| q.id).collect::<Vec<_>>(), [7]);
    let second = syncer.sync(&client, &config, &State::default(), &Default::default(), "test-mac", "test-mac-id", extras()).unwrap();
    assert!(second.window_ops.is_empty(), "handed to the app once");
    syncer.sync(&client, &config, &State::default(), &Default::default(), "test-mac", "test-mac-id", extras()).unwrap();
    assert!(!on_server(&config, "cat ~/.glasscode/manager/window-queue.json").contains("From the server."), "acknowledged: gone from the queue");
    // Nothing went round: the Mac's line is on the server once.
    let lines = on_server(&config, "cat ~/.glasscode/manager/conversation/*.jsonl | grep -c 'The e2e build is green.' || true");
    assert_eq!(lines, "1");
}

/// M57's demonstration, against a real glasscoded: while the server is the manager's home, an
/// event wakes the manager there, a brain runs (the test kit's stand-in Claude, playing a wake's
/// answer), and what it tells the person shows in the Mac's conversation with the next sync.
#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn the_manager_wakes_on_the_server_while_it_is_home() {
    use glasscode_core::conversation::Conversation;
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    let gitconfig = home.path().join("gitconfig");
    std::fs::write(&gitconfig, "").unwrap();
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
        std::env::set_var("GIT_CONFIG_GLOBAL", &gitconfig);
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config.clone()], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");

    // The stand-in brain: the test kit's Claude, first on the login shell's PATH, playing a wake.
    let tell = "CI failed on acme/e2e#7: the server's manager saw it.";
    let tools: Vec<String> = glasscode_core::mcp::tools(glasscode_core::mcp::Profile::Manager).iter().map(|t| format!("{}{}", glasscode_core::mcp::CLAUDE_PREFIX, t["name"].as_str().unwrap())).chain(["ToolSearch".to_string()]).collect();
    let answer = json!({ "tell_person": tell, "matters": [], "proposals": [], "notify": [] }).to_string();
    let stdout = |value: Value| json!({ "to": "stdout", "text": value.to_string() });
    let turn = json!([
        stdout(json!({ "type": "system", "subtype": "init", "session_id": "{{session_id}}", "tools": tools, "model": "claude-sonnet-5-5", "claude_code_version": "2.1.322" })),
        stdout(json!({ "type": "result", "subtype": "success", "is_error": false, "result": answer, "usage": { "input_tokens": 40, "output_tokens": 5 } })),
    ]);
    let scenario = json!({ "agent": "claude", "version": "2.1.322", "turns": [turn, turn, turn] });
    let local = home.path().join("wake.json");
    std::fs::write(&local, scenario.to_string()).unwrap();
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../script/test-agents/claude");
    on_server(&config, "mkdir -p ~/standin");
    let help = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../script/cli-help");
    for (from, to) in [(local.as_path(), "standin/wake.json"), (repo.as_path(), "standin/claude-stand-in"), (help.as_path(), "standin/cli-help")] {
        let ok = Command::new("scp").args(["-q", "-r", "-F", &ssh_config]).arg(from).arg(format!("glasscode-test:{to}")).status().unwrap().success();
        assert!(ok, "scp {}", from.display());
    }
    struct Undo(ServerConfig);
    impl Drop for Undo {
        fn drop(&mut self) {
            // GLASSCODE_E2E_KEEP=1 leaves the server as the test left it, to look at.
            if std::env::var("GLASSCODE_E2E_KEEP").is_ok_and(|v| v == "1") {
                return;
            }
            on_server(&self.0, "sed -i '/glasscode-e2e-wake/d' ~/.profile; rm -rf ~/standin; pkill -f '[g]lasscoded serve'; true");
        }
    }
    let _undo = Undo(config.clone());
    // The brain runs as the account (its folder is in the server's own .glasscode, where a stand-in
    // never writes): the wrapper gives the stand-in a scratch folder of its own instead.
    on_server(&config, "mkdir -p ~/standin/claude-config && printf '%s\\n' '#!/bin/sh' 'export CLAUDE_CONFIG_DIR=\"$HOME/standin/claude-config\"' 'exec \"$HOME/standin/claude-stand-in\" \"$@\"' > ~/standin/claude && chmod +x ~/standin/claude ~/standin/claude-stand-in");
    on_server(&config, "chmod +x ~/standin/claude; sed -i '/glasscode-e2e-wake/d' ~/.profile; printf '%s\\n' 'export PATH=\"$HOME/standin:$PATH\" # glasscode-e2e-wake' 'export GLASSCODE_TEST_SCENARIO=\"$HOME/standin/wake.json\" # glasscode-e2e-wake' 'export GLASSCODE_TEST_CLI_HELP=\"$HOME/standin/cli-help\" # glasscode-e2e-wake' >> ~/.profile; pkill -f '[g]lasscoded serve'; sleep 1; rm -rf ~/.glasscode/authority/home.json* ~/.glasscode/authority/from-mac.json* ~/.glasscode/manager/conversation ~/.glasscode/manager/from-mac.json; true");

    // The Mac: v2, its approved authority, the manager on, about one project; the server home.
    glasscode_core::store::update::<glasscode_core::store::EngineSwitch, ()>(|switch| switch.engine = glasscode_core::store::Engine::V2).unwrap();
    glasscode_core::journal::decide_again();
    let client = RemoteClient::connect(&config).unwrap();
    // The server's own Claude account for the manager, with a folder of its own (the person's
    // second login, as it were): the brain runs as it, and the stand-in writes only there.
    let account: Value = client.call("accounts.add", json!({ "agent": "claude", "label": "Manager e2e" })).unwrap();
    let account_id = account["id"].as_str().map(str::to_string).or_else(|| account["account"]["id"].as_str().map(str::to_string)).expect("the new account's id");
    let key = glasscode_core::paths::ProjectKey::parse("e2e-3f9a1c").unwrap();
    let project = glasscode_core::runtime::situation::Project { id: key.to_string(), name: "e2e".into(), careful: false, visible: true };
    let manager = glasscode_core::remote::ManagerFromMac { enabled: true, agent: Some(AgentKind::Claude), model: None, account: Some(account_id), projects: vec![project], follow_ups: None };
    let authority = glasscode_core::gate::FromMac::of(&glasscode_core::gate::Authority::default(), None);
    let extras = || glasscode_core::mirror::Extras { worktree_keys: Vec::new(), board: None, asleep: false, today: None, cli_replies: Vec::new(), today_acks: Vec::new(), cli_acks: Vec::new(), skill_off: false, authority: Some(authority.clone()), journal_sent: None, manager: Some(manager.clone()) };
    let mut syncer = glasscode_core::mirror::Syncer::default();
    // Only what the server logs from here on counts.
    let mark: usize = on_server(&config, "wc -l < ~/.glasscode/glasscoded.log").trim().parse().unwrap_or(0);
    let since = format!("tail -n +{} ~/.glasscode/glasscoded.log", mark + 1);
    syncer.sync(&client, &config, &State::default(), &Default::default(), "test-mac", "test-mac-id", extras()).unwrap();
    let started = std::time::Instant::now();
    while !on_server(&config, &format!("{since} | grep -c 'the wake lane runs here' || true")).starts_with(|c: char| c != '0') {
        assert!(started.elapsed() < std::time::Duration::from_secs(30), "the wake lane did not start: {}", on_server(&config, &format!("{since} | grep 'manager:' | tail -3")));
        std::thread::sleep(std::time::Duration::from_secs(2));
    }

    // CI fails (an event of the Mac's, synced): the manager on the server wakes, and tells.
    let mut event = glasscode_core::journal::Event::new(glasscode_core::journal_kinds::CI_FAILED, glasscode_core::journal::Source::Forge, "CI failed on acme/e2e#7").data(json!({ "repo": "acme/e2e", "number": 7, "head": "abc123" }));
    event.project = Some(key);
    glasscode_core::journal::append(event);
    glasscode_core::journal::flush();
    let started = std::time::Instant::now();
    loop {
        if let Err(err) = syncer.sync(&client, &config, &State::default(), &Default::default(), "test-mac", "test-mac-id", extras()) {
            panic!("the sync failed ({err:#}); the server said: {}", on_server(&config, &format!("sleep 1; {since} | tail -12; pgrep -fa '[g]lasscoded serve' | head -2; true")));
        }
        if Conversation::load().items().iter().any(|item| item.text == tell) {
            break;
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(90), "no line of the server's manager came: {}", on_server(&config, &format!("{since} | grep -E 'manager|wake' | tail -8")));
        std::thread::sleep(std::time::Duration::from_secs(3));
    }

    // The person asks on the Mac (the server is home): the chat lane there answers, and the
    // answer comes back with the sync.
    use glasscode_core::conversation::{Author, Draft, Kind};
    glasscode_core::words::decide_again();
    let words = glasscode_core::words::record(glasscode_core::words::Origin::Composer(glasscode_core::words::ComposerKind::ManagerChat), "What failed?", None, None);
    Conversation::load().post(Draft { author: Author::Person, kind: Kind::You { words, reply_to: None }, text: "What failed?".into(), refs: Vec::new(), topic: None }).unwrap();
    let started = std::time::Instant::now();
    loop {
        if let Err(err) = syncer.sync(&client, &config, &State::default(), &Default::default(), "test-mac", "test-mac-id", extras()) {
            panic!("the sync failed ({err:#})");
        }
        if Conversation::load().items().iter().any(|item| item.author == Author::Chat) {
            break;
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(90), "the server's chat lane did not answer: {}", on_server(&config, &format!("{since} | grep -E 'manager|chat' | tail -8")));
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}
