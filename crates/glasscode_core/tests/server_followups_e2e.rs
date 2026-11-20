//! The follow-ups while the server is the manager's home (M56.2), on a real glasscoded
//! (`script/test-server/run.sh`, with glasscoded installed in it): the Mac's follow-ups go to the
//! server with the sync; there, a quiet one with nobody known is told (a notice and a line in the
//! conversation, both back with the sync) and a teammate's note is left for the Mac; a follow-up
//! the Mac closes is closed on the server. A "stays on this Mac" project's follow-up never goes,
//! nor the name of a teammate who has follow-ups of it alone.
//!
//!   GLASSCODE_TEST_SERVER_NAME=glasscode-test-t2 GLASSCODE_TEST_SERVER_PORT=2302 script/test-server/run.sh
//!   (copy target/glasscoded/glasscoded-linux-<arch> to ~/.glasscode/bin/glasscoded there)
//!   GLASSCODE_TEST_SSH_CONFIG=$PWD/target/glasscode-test-t2/ssh_config \
//!     cargo test -p glasscode_core --test server_followups_e2e -- --ignored --test-threads=1

use std::process::Command;
use std::time::{Duration, Instant};

use glasscode_core::conversation::Conversation;
use glasscode_core::manager::{Blocker, FollowKind, FollowState, FollowUp, followups};
use glasscode_core::model::{Project, State};
use glasscode_core::paths::MachineId;
use glasscode_core::remote::{FollowUpsFromMac, ManagerFromMac, RemoteClient, ServerConfig, Teammate};
use glasscode_core::work::WorkLedger;
use serde_json::Value;

fn on_server(config: &ServerConfig, script: &str) -> String {
    let out = Command::new("ssh").args(&config.ssh_args).arg(&config.ssh_target).arg(script).output().expect("ssh runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn given(number: u64, login: &str, at: u64) -> FollowUp {
    FollowUp {
        id: followups::given_id("acme/app", number),
        kind: FollowKind::BlockersGiven,
        repo: "acme/app".into(),
        number,
        title: format!("Change {number}"),
        login: login.into(),
        blockers: vec![Blocker { thread: "t1".into(), text: "fix it".into(), path: "src/a.rs".into(), resolved: false }],
        created: at,
        updated: at,
        ..FollowUp::default()
    }
}

#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn follow_ups_go_on_while_the_server_is_home() {
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");
    // A fresh daemon, home (no home record), with no primary Mac (this run's is a new machine),
    // and no follow-ups or conversation yet.
    on_server(&config, "pkill -f '[g]lasscoded serve'; sleep 1; mkdir -p ~/.glasscode && : > ~/.glasscode/glasscoded.log; rm -rf ~/.glasscode/authority/home.json* ~/.glasscode/authority/from-mac.json* ~/.glasscode/manager/from-mac.json ~/.glasscode/manager/work.json* ~/.glasscode/manager/conversation; true");

    // The Mac: v2, two follow-ups quiet for three days (Sam's, and one of someone it does not
    // know), stamped as the app's edits are.
    glasscode_core::store::update::<glasscode_core::store::EngineSwitch, ()>(|switch| switch.engine = glasscode_core::store::Engine::V2).unwrap();
    glasscode_core::journal::decide_again();
    let mac = MachineId::this().unwrap();
    let quiet_since = glasscode_core::model::now() - 3 * 86_400;
    // A project that stays on this Mac (every project does, by the approved default here), its
    // checkout's repository acme/secret, with Kim's review on it.
    let secret = home.path().join("code/secret");
    std::fs::create_dir_all(&secret).unwrap();
    for args in [&["init", "-q"][..], &["config", "glasscode.repo", "acme/secret"], &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "--allow-empty", "-m", "start"]] {
        assert!(Command::new("git").arg("-C").arg(&secret).args(args).status().unwrap().success());
    }
    // Its key, as the app binds it (memory's privacy rule waits for one).
    glasscode_core::memstore::bind_project("p-secret", &secret).expect("the project gets a key");
    let local = State { projects: vec![Project { id: "p-secret".into(), name: "secret".into(), root: secret, expanded: true, github_account: None }], ..State::default() };
    let policy = glasscode_core::gate::Policy { default: glasscode_core::gate::ProjectPolicy { local_only: true, ..glasscode_core::gate::ProjectPolicy::default() }, ..glasscode_core::gate::Policy::default() };
    glasscode_core::gate::approve(&policy).unwrap();
    let mut kims = given(700, "kim-dev", quiet_since);
    kims.repo = "acme/secret".into();
    kims.id = followups::given_id("acme/secret", 700);
    glasscode_core::store::update::<WorkLedger, ()>(|ledger| ledger.follow_edit(&mac, |ledger| ledger.follow_ups.extend([given(1111, "sam-dev", quiet_since), given(1200, "stranger", quiet_since), kims]))).unwrap();
    let client = RemoteClient::connect(&config).expect("glasscoded answers on the test server");
    let people = vec![Teammate { name: "Sam".into(), github: "sam-dev".into() }, Teammate { name: "Kim".into(), github: "kim-dev".into() }];
    let manager = ManagerFromMac { enabled: true, follow_ups: Some(FollowUpsFromMac { me: Some("robin".into()), people }), ..ManagerFromMac::default() };
    let authority = glasscode_core::gate::FromMac::of(&glasscode_core::gate::Authority::default(), None);
    let extras = || glasscode_core::mirror::Extras { worktree_keys: Vec::new(), board: None, asleep: false, today: None, cli_replies: Vec::new(), today_acks: Vec::new(), cli_acks: Vec::new(), skill_off: false, authority: Some(authority.clone()), journal_sent: None, manager: Some(manager.clone()) };
    let mut syncer = glasscode_core::mirror::Syncer::default();
    let sync = |syncer: &mut glasscode_core::mirror::Syncer| {
        let outcome = syncer.sync(&client, &config, &local, &Default::default(), "test-mac", "test-mac-id", extras()).unwrap_or_else(|err| panic!("the sync failed ({err:#}); the server said: {}", on_server(&config, "tail -8 ~/.glasscode/glasscoded.log")));
        // As the app does: the server's follow-ups into the Mac's ledger.
        if !outcome.follow_ups.is_empty() {
            glasscode_core::store::update::<WorkLedger, bool>(|ledger| ledger.follow_merge(&outcome.follow_ups)).unwrap();
        }
        outcome
    };

    // The server tells of the stranger's; Sam's note is left for the Mac.
    let started = Instant::now();
    let notice = loop {
        let outcome = sync(&mut syncer);
        if let Some(notice) = outcome.notices.into_iter().find(|n| n.title == "Quiet for 2 days") {
            break notice;
        }
        assert!(started.elapsed() < Duration::from_secs(120), "no notice came: {}", on_server(&config, "grep 'follow-ups' ~/.glasscode/glasscoded.log | tail -5"));
        std::thread::sleep(Duration::from_secs(3));
    };
    assert_eq!(notice.body, "Stranger has 1 open blocker on #1200.");
    let started = Instant::now();
    loop {
        sync(&mut syncer);
        let ledger: WorkLedger = glasscode_core::store::Store::load().unwrap();
        let told = Conversation::load().items().iter().any(|item| item.text == "Quiet for 2 days. Stranger has 1 open blocker on #1200.");
        if told && ledger.follow_ups.iter().any(|f| f.number == 1200 && f.nudged.is_some()) {
            assert!(ledger.follow_ups.iter().find(|f| f.number == 1111).is_some_and(|f| f.nudged.is_none() && f.person == "Sam"), "Sam's note waits for the Mac: {:?}", ledger.follow_ups);
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "the line or the server's nudge did not come back");
        std::thread::sleep(Duration::from_secs(2));
    }
    assert_eq!(on_server(&config, "grep -c 'follow-ups: Quiet for 2 days' ~/.glasscode/glasscoded.log"), "1", "told once");
    // Nothing of the project that stays here went: not Kim's follow-up, not Kim's name.
    let there: Value = serde_json::from_str(&on_server(&config, "cat ~/.glasscode/manager/work.json")).expect("the server's ledger");
    let numbers: Vec<u64> = there["data"]["follow_ups"].as_array().into_iter().flatten().filter_map(|f| f["number"].as_u64()).collect();
    assert_eq!(numbers, [1111, 1200], "{there}");
    let said: Value = serde_json::from_str(&on_server(&config, "cat ~/.glasscode/manager/from-mac.json")).expect("what the Mac said");
    let names: Vec<&str> = said["follow_ups"]["people"].as_array().into_iter().flatten().filter_map(|p| p["name"].as_str()).collect();
    assert_eq!(names, ["Sam"], "{said}");
    assert!(!on_server(&config, "grep -rlE 'kim-dev|Kim' ~/.glasscode/manager/ 2>/dev/null || true").contains("manager"), "Kim's name is nowhere on the server");

    // The Mac closes Sam's: it is closed on the server.
    glasscode_core::store::update::<WorkLedger, ()>(|ledger| {
        ledger.follow_edit(&mac, |ledger| {
            followups::close(&mut ledger.follow_ups, "#1111", false, glasscode_core::model::now());
        })
    })
    .unwrap();
    sync(&mut syncer);
    let there: Value = serde_json::from_str(&on_server(&config, "cat ~/.glasscode/manager/work.json")).expect("the server's ledger");
    let state = there["data"]["follow_ups"].as_array().and_then(|list| list.iter().find(|f| f["number"] == 1111)).map(|f| f["state"].clone());
    assert_eq!(state, Some(serde_json::to_value(FollowState::Done).unwrap()));
}
