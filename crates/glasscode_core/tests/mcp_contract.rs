//! The contract of `glasscode mcp` with each approved CLI (M34): its handshake, replayed against
//! the server, and what each step must answer. `tests/mcp_contract/<cli>.jsonl` holds one message
//! a line with the parts of the answer that matter, written from the proofs' runs (P01 for Claude
//! Code 2.1.328, P02 for Codex 0.167); a new approved version adds its own file. No call here
//! reaches the app: each one is refused by the server first (a missing argument, a tool the
//! profile does not have), so the test touches no Glasscode folder.

use glasscode_core::caller::Lane;
use glasscode_core::mcp::{Brain, Profile, Server};
use glasscode_core::mcp_stdio::handle;
use serde_json::Value;

/// The value at a dotted path (`result.tools.0.name`).
fn at<'a>(value: &'a Value, path: &str) -> &'a Value {
    path.split('.').fold(value, |v, key| match key.parse::<usize>() {
        Ok(i) => &v[i],
        Err(_) => &v[key],
    })
}

fn replay(file: &str) -> usize {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("mcp_contract").join(file);
    let text = std::fs::read_to_string(&path).unwrap();
    let mut server = Server { profile: Profile::Manager, brain: Some(Brain { lane: Lane::Chat, wake: None, sig: "test".into() }), session: None, cwd: None };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut steps = 0;
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let step: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{file}:{}: {e}", n + 1));
        let Some(send) = step.get("send") else { continue };
        let answer = runtime.block_on(handle(&mut server, send));
        match &step["expect"] {
            Value::Null => assert!(answer.is_none(), "{file}:{}: a notification got {answer:?}", n + 1),
            Value::Object(expect) => {
                let answer = answer.unwrap_or_else(|| panic!("{file}:{}: no answer", n + 1));
                assert_eq!(answer["id"], send["id"], "{file}:{}", n + 1);
                for (path, wanted) in expect {
                    assert_eq!(at(&answer, path), wanted, "{file}:{}: {path} in {answer}", n + 1);
                }
            }
            other => panic!("{file}:{}: expect is {other}", n + 1),
        }
        steps += 1;
    }
    steps
}

#[test]
fn claude_code_handshake() {
    assert!(replay("claude.jsonl") >= 6);
}

#[test]
fn codex_handshake() {
    assert!(replay("codex.jsonl") >= 4);
}
