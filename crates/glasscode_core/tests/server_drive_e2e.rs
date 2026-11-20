//! events-13 and intake on a real glasscoded (`script/test-server/run.sh`, with glasscoded
//! installed in it): the Mac's connection tells the server what a message it types descends from
//! (`journal.expect_drive`), a bad marker is refused, and the daemon's intake ticks and says why it
//! does not run (the server is not home).
//!
//!   GLASSCODE_TEST_SERVER_NAME=glasscode-test-t5 GLASSCODE_TEST_SERVER_PORT=2305 script/test-server/run.sh
//!   (copy target/glasscoded/glasscoded-linux-<arch> to ~/.glasscode/bin/glasscoded there)
//!   GLASSCODE_TEST_SSH_CONFIG=$PWD/target/glasscode-test-t5/ssh_config \
//!     cargo test -p glasscode_core --test server_drive_e2e -- --ignored --test-threads=1

use std::process::Command;

use glasscode_core::remote::{RemoteClient, ServerConfig};
use serde_json::{Value, json};

fn on_server(config: &ServerConfig, script: &str) -> String {
    let out = Command::new("ssh").args(&config.ssh_args).arg(&config.ssh_target).arg(script).output().expect("ssh runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
#[ignore = "needs the Docker test server with glasscoded installed (see the top of this file)"]
fn the_server_takes_the_macs_drive_and_ticks_intake() {
    let ssh_config = std::env::var("GLASSCODE_TEST_SSH_CONFIG").expect("GLASSCODE_TEST_SSH_CONFIG names the test server's ssh_config");
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the tests of this file run one at a time (`--test-threads=1`).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("GLASSCODE_HOME", home.path().join(".glasscode"));
    }
    let config = ServerConfig { ssh_target: "glasscode-test".into(), ssh_args: vec!["-F".into(), ssh_config], remote_bin: "~/.glasscode/bin/glasscoded".into(), relay: None };
    assert_eq!(on_server(&config, "test -f ~/.glasscode-test-server && echo yes || echo no"), "yes", "not the Docker test server: nothing is reset");
    // A fresh daemon with an empty log, so its intake tick's line is the only one.
    on_server(&config, "pkill -f '[g]lasscoded serve'; sleep 1; mkdir -p ~/.glasscode && : > ~/.glasscode/glasscoded.log; true");
    let client = RemoteClient::connect(&config).expect("glasscoded answers on the test server");

    // The Mac's connection (a machine) tells the server what a typed message descends from.
    let params = glasscode_core::journal::drive_params("s-e2e", "[gc:1a2b3c4d]", 2, &["ab".repeat(32)]);
    let ok: Value = client.call(glasscode_core::caller::DRIVE_METHOD, params).expect("the drive is taken");
    assert_eq!(ok["ok"], json!(true));
    let bad = client.call::<Value>(glasscode_core::caller::DRIVE_METHOD, json!({ "session": "s-e2e", "marker": "no marker" }));
    assert!(bad.is_err(), "{bad:?}");

    // Its intake ticks on the daemon's ticker and says, once, why it does not run here.
    let mut said = String::new();
    for _ in 0..20 {
        said = on_server(&config, "grep 'intake:' ~/.glasscode/glasscoded.log || true");
        if !said.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert!(said.contains("intake: it does not run here:"), "{said}");
    assert_eq!(said.lines().count(), 1, "said once: {said}");
}
