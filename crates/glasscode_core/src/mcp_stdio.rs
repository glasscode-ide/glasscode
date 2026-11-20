//! The MCP server loop Glasscode's own servers share (M34; ops-15, alt-3): JSON-RPC lines on
//! stdin and stdout, hand-written, with no SDK. It answers `initialize` (with a table of protocol
//! revisions), `notifications/initialized`, `ping`, `tools/list` and `tools/call`; every other
//! method gets -32601, Claude's `server/discover` probe too, which makes Claude fall back to
//! `initialize`. A notification (no id) gets no answer.
//!
//! The Glasscode Browser's server (`browser::mcp`) and `glasscode mcp` (the registry's ops as
//! tools, `crate::mcp`) are both served by it.

use std::future::Future;
use std::io::Write as _;

use serde_json::{Value, json};

/// The protocol revisions it speaks, newest first. A client that asks for one of them gets it;
/// any other gets the newest it knows that most clients take (`FALLBACK`).
pub const REVISIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The revision a client gets when it asks for one this table does not have.
pub const FALLBACK: &str = "2025-03-26";

/// The method has no handler here (JSON-RPC's "method not found").
pub const METHOD_NOT_FOUND: i64 = -32601;

/// What a server tells a client about itself at `initialize`.
#[derive(Clone, Debug)]
pub struct Info {
    pub name: String,
    pub version: String,
    pub instructions: String,
}

/// A server's tools.
pub trait Tools {
    fn info(&self) -> Info;
    /// Every tool, as `tools/list` gives it (`name`, `description`, `inputSchema`, …).
    fn list(&self) -> Vec<Value>;
    /// A tool's result (`{"content": […], "isError"?: …}`) for `tools/call` with these params.
    fn call(&mut self, params: &Value) -> impl Future<Output = Value>;
}

/// The revision a client that asks for `asked` gets.
pub fn revision(asked: &str) -> &'static str {
    REVISIONS.iter().find(|r| **r == asked).copied().unwrap_or(FALLBACK)
}

fn reply(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

/// A tool's result that is text.
pub fn text(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }] })
}

/// A tool's result that says it failed (the call was made; the tool could not do it).
pub fn failure(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }], "isError": true })
}

/// The answer to one message, or None for a notification (or a line that is not JSON-RPC).
pub async fn handle(tools: &mut impl Tools, message: &Value) -> Option<Value> {
    let id = message.get("id").cloned()?;
    let method = message["method"].as_str().unwrap_or_default();
    Some(match method {
        "initialize" => {
            let info = tools.info();
            let version = revision(message["params"]["protocolVersion"].as_str().unwrap_or_default());
            reply(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": info.name, "version": info.version },
                    "instructions": info.instructions,
                }),
            )
        }
        "ping" => reply(id, json!({})),
        "tools/list" => reply(id, json!({ "tools": tools.list() })),
        "tools/call" => {
            let result = tools.call(&message["params"]).await;
            reply(id, result)
        }
        other => error(id, METHOD_NOT_FOUND, format!("no method {other}")),
    })
}

/// Serve on stdin and stdout until the client goes away, on a runtime of the calling thread.
pub fn serve(tools: &mut impl Tools) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(serve_lines(tools))
}

/// The loop of [`serve`], for a server that already runs on a tokio runtime.
pub async fn serve_lines(tools: &mut impl Tools) -> anyhow::Result<()> {
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(answer) = handle(tools, &message).await else { continue };
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{answer}");
        let _ = out.flush();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl Tools for Echo {
        fn info(&self) -> Info {
            Info { name: "echo".into(), version: "1".into(), instructions: "Echo.".into() }
        }
        fn list(&self) -> Vec<Value> {
            vec![json!({ "name": "echo", "inputSchema": { "type": "object" } })]
        }
        async fn call(&mut self, params: &Value) -> Value {
            text(params["arguments"]["say"].as_str().unwrap_or_default())
        }
    }

    fn answer(message: Value) -> Option<Value> {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(handle(&mut Echo, &message))
    }

    #[test]
    fn unknown_method_is_32601() {
        for method in ["server/discover", "resources/list", "prompts/list", "nothing"] {
            let reply = answer(json!({ "jsonrpc": "2.0", "id": 1, "method": method })).unwrap();
            assert_eq!(reply["error"]["code"], json!(METHOD_NOT_FOUND), "{method}");
        }
        assert_eq!(answer(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })), None, "a notification has no answer");
    }

    #[test]
    fn the_revision_is_the_clients_when_it_is_known() {
        for asked in REVISIONS {
            let reply = answer(json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": asked } })).unwrap();
            assert_eq!(reply["result"]["protocolVersion"], json!(asked));
        }
        let reply = answer(json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": "2026-07-28" } })).unwrap();
        assert_eq!((reply["result"]["protocolVersion"].clone(), reply["result"]["serverInfo"]["name"].clone(), reply["result"]["instructions"].clone()), (json!(FALLBACK), json!("echo"), json!("Echo.")));
    }

    #[test]
    fn tools_are_listed_and_called() {
        let list = answer(json!({ "jsonrpc": "2.0", "id": "a", "method": "tools/list" })).unwrap();
        assert_eq!((list["id"].clone(), list["result"]["tools"][0]["name"].clone()), (json!("a"), json!("echo")));
        let call = answer(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "echo", "arguments": { "say": "hi" } } })).unwrap();
        assert_eq!(call["result"]["content"][0]["text"], json!("hi"));
        assert_eq!(answer(json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" })).unwrap()["result"], json!({}));
    }
}
