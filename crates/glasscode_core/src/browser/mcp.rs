//! `glasscode browser-mcp`: the MCP server (stdio) each agent on this Mac or PC runs. It drives one
//! tab of the Glasscode Browser for its session: open a page, read it (numbered elements and
//! text), click, type, press keys, and take screenshots. The tab closes when the session ends.
//!
//! The tab stays in the background, whatever the agent does: nothing here brings the browser
//! forward, so five sessions can work in it at once while the person does something else.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;

use super::cdp::Cdp;

const SNAPSHOT_JS: &str = include_str!("snapshot.js");

const INSTRUCTIONS: &str = "Use these tools, not other browser tools, for web pages: they open in this session's own background tab, which other sessions and the person do not disturb (and which does not disturb them). Any site opens. Read a page with snapshot and act on its numbered elements.";

/// What the agent is told about the tools: where the browser is, then `INSTRUCTIONS`.
fn instructions() -> String {
    let computer = if cfg!(windows) { "PC" } else { "Mac" };
    format!("The Glasscode Browser is a {} window on this {computer}. {INSTRUCTIONS}", super::NAME)
}

struct Tab {
    cdp: Arc<Cdp>,
    session: String,
    target: String,
}

struct Server {
    tab: Option<Tab>,
    /// The Glasscode session these tools serve (its tab is labelled with it).
    session: Option<String>,
}

fn text(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }] })
}

fn failure(s: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }], "isError": true })
}

fn tools() -> Value {
    let reference = json!({ "type": "integer", "description": "The element's number from the last snapshot" });
    json!([
        { "name": "open", "description": "Open a URL in this session's tab of the Glasscode Browser and read the page.", "inputSchema": { "type": "object", "properties": { "url": { "type": "string" } }, "required": ["url"] } },
        { "name": "snapshot", "description": "Read the current page: its numbered buttons, links and fields, and its text.", "inputSchema": { "type": "object", "properties": {} } },
        { "name": "click", "description": "Click an element (by its number from the snapshot), then read the page.", "inputSchema": { "type": "object", "properties": { "ref": reference }, "required": ["ref"] } },
        { "name": "type", "description": "Type into a field (by number). Replaces what is in it unless append is true; submit presses Enter after.", "inputSchema": { "type": "object", "properties": { "ref": reference, "text": { "type": "string" }, "submit": { "type": "boolean" }, "append": { "type": "boolean" } }, "required": ["ref", "text"] } },
        { "name": "press", "description": "Press a key in the page: Enter, Tab, Escape, Backspace, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Space.", "inputSchema": { "type": "object", "properties": { "key": { "type": "string" } }, "required": ["key"] } },
        { "name": "select", "description": "Choose an option of a select element (by number) by its value.", "inputSchema": { "type": "object", "properties": { "ref": reference, "value": { "type": "string" } }, "required": ["ref", "value"] } },
        { "name": "back", "description": "Go back one page.", "inputSchema": { "type": "object", "properties": {} } },
        { "name": "wait_for", "description": "Wait until the page shows some text (up to `seconds`, default 10), then read it.", "inputSchema": { "type": "object", "properties": { "text": { "type": "string" }, "seconds": { "type": "number" } } } },
        { "name": "screenshot", "description": "A screenshot of the tab (the visible part, or the whole page).", "inputSchema": { "type": "object", "properties": { "full_page": { "type": "boolean" } } } },
        { "name": "eval", "description": "Run JavaScript in the page and get its result as JSON (an expression; promises are awaited).", "inputSchema": { "type": "object", "properties": { "expression": { "type": "string" } }, "required": ["expression"] } }
    ])
}

impl Server {
    async fn tab(&mut self) -> anyhow::Result<&Tab> {
        if self.tab.is_none() {
            let running = tokio::task::spawn_blocking(super::ensure_running).await??;
            let cdp = Cdp::connect(&running.ws_url).await?;
            // In the background: it does not take the front of the window from another tab.
            let target = cdp.call(None, "Target.createTarget", json!({ "url": "about:blank", "background": true })).await?["targetId"].as_str().context("no tab")?.to_string();
            if let Some(owner) = &self.session {
                super::claim_tab(&target, owner);
            }
            let session = cdp.call(None, "Target.attachToTarget", json!({ "targetId": target, "flatten": true })).await?["sessionId"].as_str().context("no tab session")?.to_string();
            // As if it were in front: it renders, animates, takes screenshots and clicks while
            // other tabs are in front or the window is covered.
            cdp.call(Some(&session), "Emulation.setFocusEmulationEnabled", json!({ "enabled": true })).await?;
            cdp.call(Some(&session), "Page.enable", json!({})).await?;
            self.tab = Some(Tab { cdp, session, target });
        }
        Ok(self.tab.as_ref().unwrap())
    }

    async fn send(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let tab = self.tab().await?;
        let (cdp, session) = (tab.cdp.clone(), tab.session.clone());
        cdp.call(Some(&session), method, params).await
    }

    async fn eval(&mut self, expression: &str) -> anyhow::Result<Value> {
        let result = self.send("Runtime.evaluate", json!({ "expression": expression, "awaitPromise": true, "returnByValue": true, "userGesture": true })).await?;
        if let Some(details) = result.get("exceptionDetails") {
            let message = details["exception"]["description"].as_str().or(details["text"].as_str()).unwrap_or("error");
            bail!("{}", message.lines().next().unwrap_or(message));
        }
        Ok(result["result"]["value"].clone())
    }

    /// Wait for the page to finish loading (after a navigation or an action).
    async fn settle(&mut self, within: Duration) {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.eval("document.readyState").await.ok().as_ref().and_then(Value::as_str) == Some("complete") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    fn notes(&self) -> String {
        let notes = self.tab.as_ref().map(|t| t.cdp.take_notes()).unwrap_or_default();
        if notes.is_empty() { String::new() } else { format!("{}\n\n", notes.join("\n")) }
    }

    async fn read(&mut self, text_limit: usize) -> anyhow::Result<String> {
        let page = self.eval(SNAPSHOT_JS).await?;
        let mut text: String = page["text"].as_str().unwrap_or_default().to_string();
        if text.chars().count() > text_limit {
            text = text.chars().take(text_limit).collect::<String>() + "…";
        }
        let elements: Vec<&str> = page["elements"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        Ok(format!(
            "{}Page: {} — {}\n\nElements:\n{}\n\nText:\n{}",
            self.notes(),
            page["title"].as_str().unwrap_or_default(),
            page["url"].as_str().unwrap_or_default(),
            if elements.is_empty() { "(none)".to_string() } else { elements.join("\n") },
            text
        ))
    }

    async fn navigate(&mut self, url: &str) -> anyhow::Result<()> {
        let result = self.send("Page.navigate", json!({ "url": url })).await?;
        if let Some(error) = result["errorText"].as_str().filter(|e| !e.is_empty()) {
            bail!("{url} did not open: {error}");
        }
        self.settle(Duration::from_secs(30)).await;
        Ok(())
    }

    /// The center of an element, scrolled into view.
    async fn point(&mut self, reference: u64) -> anyhow::Result<(f64, f64)> {
        let at = self
            .eval(&format!(
                "(() => {{ const el = document.querySelector('[data-hb-ref=\"{reference}\"]'); if (!el) return null; el.scrollIntoView({{ block: 'center', inline: 'center' }}); const r = el.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; }})()"
            ))
            .await?;
        match (at[0].as_f64(), at[1].as_f64()) {
            (Some(x), Some(y)) => Ok((x, y)),
            _ => bail!("no element {reference} on the page any more; take a new snapshot"),
        }
    }

    async fn key(&mut self, key: &str) -> anyhow::Result<()> {
        let (code, vk, text) = match key {
            "Enter" => ("Enter", 13, "\r"),
            "Tab" => ("Tab", 9, ""),
            "Escape" => ("Escape", 27, ""),
            "Backspace" => ("Backspace", 8, ""),
            "Space" | " " => ("Space", 32, " "),
            "ArrowUp" => ("ArrowUp", 38, ""),
            "ArrowDown" => ("ArrowDown", 40, ""),
            "ArrowLeft" => ("ArrowLeft", 37, ""),
            "ArrowRight" => ("ArrowRight", 39, ""),
            other => bail!("unknown key {other}"),
        };
        let key = if code == "Space" { " " } else { code };
        let mut down = json!({ "type": "keyDown", "key": key, "code": code, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk });
        if !text.is_empty() {
            down["text"] = json!(text);
        }
        self.send("Input.dispatchKeyEvent", down).await?;
        self.send("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": key, "code": code, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk })).await?;
        Ok(())
    }

    async fn call(&mut self, params: &Value) -> Value {
        let name = params["name"].as_str().unwrap_or_default().to_string();
        let args = params["arguments"].clone();
        let mut result = self.run(&name, &args).await;
        // A tab the person closed, or a browser they quit: start over once.
        if result.as_ref().err().is_some_and(|e| {
            let e = e.to_string();
            e.contains("closed") || e.contains("No target") || e.contains("not found") || e.contains("Session with given id")
        }) {
            if let Some(tab) = self.tab.take() {
                super::release_tab(&tab.target);
            }
            result = self.run(&name, &args).await;
        }
        match result {
            Ok(value) => value,
            Err(error) => failure(format!("{}{error:#}", self.notes())),
        }
    }

    async fn run(&mut self, name: &str, args: &Value) -> anyhow::Result<Value> {
        let reference = || args["ref"].as_u64().ok_or_else(|| anyhow!("give the element's number (ref) from the snapshot"));
        match name {
            "open" => {
                let url = args["url"].as_str().context("give a url")?;
                self.navigate(url).await?;
                Ok(text(self.read(4000).await?))
            }
            "snapshot" => Ok(text(self.read(6000).await?)),
            "click" => {
                let (x, y) = self.point(reference()?).await?;
                for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
                    self.send("Input.dispatchMouseEvent", json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 })).await?;
                }
                self.settle(Duration::from_secs(15)).await;
                Ok(text(self.read(3000).await?))
            }
            "type" => {
                let r = reference()?;
                let value = args["text"].as_str().context("give the text")?;
                let append = args["append"].as_bool().unwrap_or(false);
                let found = self
                    .eval(&format!(
                        "(() => {{ const el = document.querySelector('[data-hb-ref=\"{r}\"]'); if (!el) return false; el.scrollIntoView({{ block: 'center' }}); el.focus(); if (!{append}) {{ if (el.select) el.select(); else document.execCommand('selectAll'); }} return true; }})()"
                    ))
                    .await?;
                if found != json!(true) {
                    bail!("no element {r} on the page any more; take a new snapshot");
                }
                self.send("Input.insertText", json!({ "text": value })).await?;
                if args["submit"].as_bool().unwrap_or(false) {
                    self.key("Enter").await?;
                    self.settle(Duration::from_secs(15)).await;
                }
                Ok(text(self.read(3000).await?))
            }
            "press" => {
                self.key(args["key"].as_str().context("give a key")?).await?;
                self.settle(Duration::from_secs(10)).await;
                Ok(text(self.read(3000).await?))
            }
            "select" => {
                let r = reference()?;
                let value = serde_json::to_string(args["value"].as_str().context("give a value")?)?;
                let done = self
                    .eval(&format!(
                        "(() => {{ const el = document.querySelector('[data-hb-ref=\"{r}\"]'); if (!el) return false; el.value = {value}; el.dispatchEvent(new Event('input', {{ bubbles: true }})); el.dispatchEvent(new Event('change', {{ bubbles: true }})); return el.value === {value}; }})()"
                    ))
                    .await?;
                if done != json!(true) {
                    bail!("could not choose {value} in element {r}");
                }
                Ok(text(self.read(3000).await?))
            }
            "back" => {
                self.eval("history.back()").await?;
                self.settle(Duration::from_secs(15)).await;
                Ok(text(self.read(3000).await?))
            }
            "wait_for" => {
                let seconds = args["seconds"].as_f64().unwrap_or(10.0).clamp(0.5, 60.0);
                let deadline = Instant::now() + Duration::from_secs_f64(seconds);
                if let Some(wanted) = args["text"].as_str() {
                    let probe = format!("(document.body ? document.body.innerText : '').includes({})", serde_json::to_string(wanted)?);
                    loop {
                        if self.eval(&probe).await.ok() == Some(json!(true)) {
                            break;
                        }
                        if Instant::now() > deadline {
                            return Ok(failure(format!("\"{wanted}\" did not show within {seconds} s.\n\n{}", self.read(3000).await?)));
                        }
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                } else {
                    tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
                }
                Ok(text(self.read(3000).await?))
            }
            "screenshot" => {
                let full = args["full_page"].as_bool().unwrap_or(false);
                let shot = self.send("Page.captureScreenshot", json!({ "format": "png", "captureBeyondViewport": full })).await?;
                let data = shot["data"].as_str().context("no screenshot")?;
                Ok(json!({ "content": [{ "type": "image", "data": data, "mimeType": "image/png" }] }))
            }
            "eval" => {
                let expression = args["expression"].as_str().context("give an expression")?;
                let value = self.eval(expression).await?;
                let mut out = serde_json::to_string_pretty(&value)?;
                if out.len() > 20000 {
                    out.truncate(20000);
                    out.push('…');
                }
                Ok(text(format!("{}{out}", self.notes())))
            }
            other => Ok(failure(format!("no tool {other}"))),
        }
    }

}

/// Serve MCP on stdin/stdout until the agent goes away; then close the session's tab.
pub fn serve(args: &[String]) -> anyhow::Result<()> {
    let session = args.iter().position(|a| a == "--session").and_then(|i| args.get(i + 1).cloned()).or_else(|| std::env::var("GLASSCODE_SESSION_ID").ok()).filter(|s| !s.is_empty());
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let mut server = Server { tab: None, session };
        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
            let Some(id) = message.get("id").cloned() else { continue };
            let method = message["method"].as_str().unwrap_or_default();
            let reply = match method {
                "initialize" => {
                    let asked = message["params"]["protocolVersion"].as_str().unwrap_or_default();
                    let version = if ["2025-06-18", "2025-03-26", "2024-11-05"].contains(&asked) { asked } else { "2025-03-26" };
                    json!({ "jsonrpc": "2.0", "id": id, "result": {
                        "protocolVersion": version,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "glasscode-browser", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": instructions(),
                    } })
                }
                "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
                "tools/list" => json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tools() } }),
                "tools/call" => {
                    let result = server.call(&message["params"]).await;
                    json!({ "jsonrpc": "2.0", "id": id, "result": result })
                }
                _ => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("no method {method}") } }),
            };
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{reply}");
            let _ = out.flush();
        }
        if let Some(tab) = server.tab.take() {
            let _ = tab.cdp.call(None, "Target.closeTarget", json!({ "targetId": tab.target })).await;
            super::release_tab(&tab.target);
        }
        Ok::<_, anyhow::Error>(())
    })?;
    // stdin's reader thread would keep the process alive.
    std::process::exit(0);
}
