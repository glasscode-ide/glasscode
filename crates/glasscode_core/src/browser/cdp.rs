//! A small Chrome DevTools Protocol client: one WebSocket to the browser, sessions attached
//! with `flatten`, commands matched to answers by id. Dialogs (`alert`, `confirm`) are answered
//! so a page cannot stall an agent.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

type Answer = Result<Value, String>;

pub struct Cdp {
    out: mpsc::UnboundedSender<String>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Answer>>>>,
    next: AtomicU64,
    /// What happened that the agent should hear about (dialogs).
    pub notes: Arc<Mutex<Vec<String>>>,
    /// The sites each tab session may reach (M23): its requests are paused (`Fetch.enable`) and
    /// let through only to them.
    allowed: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl Cdp {
    pub async fn connect(ws_url: &str) -> anyhow::Result<Arc<Cdp>> {
        let (socket, _) = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(ws_url)).await.context("the Glasscode Browser did not answer")??;
        let (mut sink, mut stream) = socket.split();
        let (out, mut outgoing) = mpsc::unbounded_channel::<String>();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Answer>>>> = Arc::default();
        let notes: Arc<Mutex<Vec<String>>> = Arc::default();
        let allowed: Arc<Mutex<HashMap<String, Vec<String>>>> = Arc::default();
        let blocked: Arc<Mutex<std::collections::HashSet<String>>> = Arc::default();
        tokio::spawn(async move {
            while let Some(text) = outgoing.recv().await {
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });
        let cdp = Arc::new(Cdp { out: out.clone(), pending: pending.clone(), next: AtomicU64::new(1), notes: notes.clone(), allowed: allowed.clone() });
        let next = Arc::new(AtomicU64::new(1_000_000));
        tokio::spawn(async move {
            while let Some(Ok(message)) = stream.next().await {
                let Message::Text(text) = message else { continue };
                let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(id) = value["id"].as_u64() {
                    if let Some(waiter) = pending.lock().unwrap().remove(&id) {
                        let answer = match value.get("error") {
                            Some(error) => Err(error["message"].as_str().unwrap_or("error").to_string()),
                            None => Ok(value["result"].clone()),
                        };
                        let _ = waiter.send(answer);
                    }
                    continue;
                }
                let session = value["sessionId"].clone();
                let params = &value["params"];
                let send = |method: &str, params: Value| {
                    let id = next.fetch_add(1, Ordering::Relaxed);
                    let _ = out.send(json!({ "id": id, "method": method, "params": params, "sessionId": session }).to_string());
                };
                match value["method"].as_str() {
                    Some("Fetch.requestPaused") => {
                        let url = params["request"]["url"].as_str().unwrap_or_default();
                        let hosts = session.as_str().and_then(|s| allowed.lock().unwrap().get(s).cloned()).unwrap_or_default();
                        let (method, answer) = request_answer(&hosts, url, params["requestId"].as_str().unwrap_or_default());
                        if method == "Fetch.failRequest" {
                            let host = url.split("://").nth(1).and_then(|r| r.split(['/', '?', '#']).next()).unwrap_or(url).to_string();
                            if blocked.lock().unwrap().insert(host.clone()) {
                                notes.lock().unwrap().push(format!("A request to {host} was stopped: it is not one of this session's sites."));
                            }
                        }
                        send(method, answer);
                    }
                    Some("Page.javascriptDialogOpening") => {
                        notes.lock().unwrap().push(format!("The page showed a {} \"{}\"; it was accepted.", params["type"].as_str().unwrap_or("dialog"), params["message"].as_str().unwrap_or_default()));
                        send("Page.handleJavaScriptDialog", json!({ "accept": true }));
                    }
                    _ => {}
                }
            }
            for (_, waiter) in pending.lock().unwrap().drain() {
                let _ = waiter.send(Err("the Glasscode Browser closed".into()));
            }
        });
        Ok(cdp)
    }

    pub async fn call(&self, session: Option<&str>, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        self.out.send(message.to_string()).map_err(|_| anyhow!("the Glasscode Browser closed"))?;
        match tokio::time::timeout(Duration::from_secs(45), rx).await {
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                bail!("the browser did not answer {method} in time")
            }
            Ok(Err(_)) => bail!("the Glasscode Browser closed"),
            Ok(Ok(answer)) => answer.map_err(|e| anyhow!("{method}: {e}")),
        }
    }

    /// The sites a tab session may reach from now on (its requests are paused with `Fetch.enable`).
    pub fn allow_only(&self, session: &str, hosts: Vec<String>) {
        self.allowed.lock().unwrap().insert(session.to_string(), hosts);
    }

    pub fn take_notes(&self) -> Vec<String> {
        std::mem::take(&mut *self.notes.lock().unwrap())
    }
}

/// The answer to a paused request of a tab kept to `hosts` (M23): go on to one of them, or stop as
/// blocked by the client.
pub fn request_answer(hosts: &[String], url: &str, request: &str) -> (&'static str, Value) {
    if super::host_allowed(hosts, url) {
        ("Fetch.continueRequest", json!({ "requestId": request }))
    } else {
        ("Fetch.failRequest", json!({ "requestId": request, "errorReason": "BlockedByClient" }))
    }
}

/// One browser-level command, from synchronous code (status checks, quitting).
pub fn once(ws_url: &str, method: &str, params: Value) -> anyhow::Result<Value> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let cdp = Cdp::connect(ws_url).await?;
        cdp.call(None, method, params).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M23: a tab kept to the project's sites lets their requests through and stops the rest.
    #[test]
    fn requests_go_only_to_the_projects_sites() {
        let hosts = vec!["docs.rs".to_string()];
        assert_eq!(request_answer(&hosts, "https://docs.rs/serde", "r1"), ("Fetch.continueRequest", json!({ "requestId": "r1" })));
        assert_eq!(request_answer(&hosts, "https://static.docs.rs/x.css", "r2").0, "Fetch.continueRequest");
        assert_eq!(request_answer(&hosts, "https://evil.example/?d=secret", "r3"), ("Fetch.failRequest", json!({ "requestId": "r3", "errorReason": "BlockedByClient" })));
        assert_eq!(request_answer(&[], "https://docs.rs/", "r4").0, "Fetch.failRequest");
    }
}
