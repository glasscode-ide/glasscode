//! The Glasscode Browser: a Chrome window of its own (its own profile, signed in once by the
//! person) that the agents on this Mac drive through the `glasscode-browser` MCP tools
//! ([`mcp`]). Each session gets its own tab, and may open any site.
//!
//! It is the installed Google Chrome, started as a separate instance with
//! `--user-data-dir` and a local DevTools port (Chrome allows the port only for a profile of
//! its own anyway). The person's own Chrome is not touched.

pub mod cdp;
pub mod mcp;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use serde_json::Value;

use crate::paths;

pub fn dir() -> PathBuf {
    paths::root().join("browser")
}

pub fn profile_dir() -> PathBuf {
    dir().join("profile")
}

/// Where the `browser-mcp` program is (written at Glasscode's start; agents' configs are made
/// from it).
pub fn mcp_config_file() -> PathBuf {
    dir().join("mcp.json")
}

fn agents_off_file() -> PathBuf {
    dir().join("agents-off")
}

/// Which session owns which tab: one file per agent tab, named by its target id.
fn owners_dir() -> PathBuf {
    dir().join("tabs")
}

pub fn port() -> u16 {
    std::env::var("GLASSCODE_BROWSER_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(9333)
}

/// Chrome (or another Chromium) to run.
pub fn chrome() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GLASSCODE_BROWSER_CHROME").map(PathBuf::from) {
        return Some(path);
    }
    let home = paths::home();
    [
        PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
        home.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
        PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
        PathBuf::from("/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// What a running Glasscode Browser says about itself.
#[derive(Clone, Debug)]
pub struct Running {
    pub ws_url: String,
    pub product: String,
}

fn http_json(path: &str) -> Option<Value> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_millis(800)).build();
    let response = agent.get(&format!("http://127.0.0.1:{}{path}", port())).call().ok()?;
    serde_json::from_str(&response.into_string().ok()?).ok()
}

pub fn running() -> Option<Running> {
    let version = http_json("/json/version")?;
    Some(Running { ws_url: version["webSocketDebuggerUrl"].as_str()?.to_string(), product: version["Browser"].as_str().unwrap_or_default().to_string() })
}

/// A tab open in it, and the session it belongs to (none: the person's own).
#[derive(Clone, Debug, Default)]
pub struct Tab {
    pub id: String,
    pub title: String,
    pub url: String,
    pub session: Option<String>,
}

/// The tabs open in it.
pub fn tabs() -> Vec<Tab> {
    let Some(list) = http_json("/json/list").and_then(|v| v.as_array().cloned()) else { return Vec::new() };
    let owners = owners();
    let tabs: Vec<Tab> = list
        .into_iter()
        .filter(|t| t["type"] == "page")
        .map(|t| {
            let id = t["id"].as_str().unwrap_or_default().to_string();
            Tab { session: owners.get(&id).cloned(), id, title: t["title"].as_str().unwrap_or_default().to_string(), url: t["url"].as_str().unwrap_or_default().to_string() }
        })
        .collect();
    forget_closed_tabs(&tabs);
    tabs
}

/// Agent tabs by target id → session id.
fn owners() -> std::collections::HashMap<String, String> {
    std::fs::read_dir(owners_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| Some((entry.file_name().to_str()?.to_string(), std::fs::read_to_string(entry.path()).ok()?.trim().to_string())))
        .collect()
}

/// Each session's tab (session id → target id), for its Browser button. Nothing to ask the
/// browser when no session has a tab.
pub fn session_tabs() -> std::collections::HashMap<String, String> {
    if owners().is_empty() {
        return Default::default();
    }
    tabs().into_iter().filter_map(|t| Some((t.session?, t.id))).collect()
}

/// Note that `target` is `session`'s tab (the Browser dialog shows whose tab is whose).
pub fn claim_tab(target: &str, session: &str) {
    let _ = std::fs::create_dir_all(owners_dir());
    let _ = std::fs::write(owners_dir().join(target), session);
}

pub fn release_tab(target: &str) {
    let _ = std::fs::remove_file(owners_dir().join(target));
}

/// Forget the owners of tabs that are gone (the person closed them, or the browser quit).
fn forget_closed_tabs(open: &[Tab]) {
    for target in owners().into_keys() {
        if !open.iter().any(|t| t.id == target) {
            release_tab(&target);
        }
    }
}

/// Bring a tab and the browser forward, for the person (agents never do this).
pub fn show_tab(target: &str) -> anyhow::Result<()> {
    let r = running().context("the Glasscode Browser is not running")?;
    cdp::once(&r.ws_url, "Target.activateTarget", serde_json::json!({ "targetId": target }))?;
    Ok(())
}

/// Start it if it is not running, and wait until it answers. In the background: it does not
/// take the front from whatever the person is doing. Sessions starting together start it once.
pub fn ensure_running() -> anyhow::Result<Running> {
    if let Some(r) = running() {
        return Ok(r);
    }
    let _lock = crate::net::FileLock::take(&dir().join("start.lock"))?;
    if let Some(r) = running() {
        return Ok(r);
    }
    forget_closed_tabs(&[]);
    let chrome = chrome().context("Google Chrome is not installed (the Glasscode Browser runs on it)")?;
    std::fs::create_dir_all(profile_dir().join("Default"))?;
    // A name and colour of its own, so its window is not mistaken for the person's Chrome.
    let preferences = profile_dir().join("Default").join("Preferences");
    if !preferences.exists() {
        let _ = std::fs::write(&preferences, serde_json::json!({ "profile": { "name": "Glasscode" }, "browser": { "theme": { "user_color": -1002662 } } }).to_string());
    }
    let mut args = vec![
        format!("--user-data-dir={}", profile_dir().display()),
        format!("--remote-debugging-port={}", port()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-features=ChromeWhatsNewUI".to_string(),
        // No window of its own at start (a window would take the front); the first tab makes one.
        "--no-startup-window".to_string(),
        // Tabs behind others, and covered windows, keep running at full speed.
        "--disable-background-timer-throttling".to_string(),
        "--disable-backgrounding-occluded-windows".to_string(),
        "--disable-renderer-backgrounding".to_string(),
    ];
    let headless = std::env::var_os("GLASSCODE_BROWSER_HEADLESS").is_some();
    if headless {
        args.push("--headless=new".into());
    }
    // A separate app instance (`open -n`), so it lives on when Glasscode quits, started behind
    // the person's work (`-g`); headless runs as a plain background process.
    let app = chrome.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(PathBuf::from);
    let started = match (&app, headless) {
        (Some(app), false) => crate::sys::command("/usr/bin/open").arg("-n").arg("-g").arg("-a").arg(app).arg("--args").args(&args).status().map(|s| s.success()),
        _ => crate::sys::command(&chrome).args(&args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map(|_| true),
    }
    .context("could not start Chrome")?;
    if !started {
        bail!("Chrome did not start");
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(r) = running() {
            return Ok(r);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!("the Glasscode Browser did not come up on port {}", port())
}

/// Quit it (its profile, and so its sign-ins, stay).
pub fn stop() {
    if let Some(r) = running() {
        let _ = cdp::once(&r.ws_url, "Browser.close", serde_json::json!({}));
    }
}

/// Quit it and forget everything it knew (sign-ins, history).
pub fn reset() -> anyhow::Result<()> {
    stop();
    std::thread::sleep(Duration::from_millis(800));
    if profile_dir().exists() {
        std::fs::remove_dir_all(profile_dir())?;
    }
    Ok(())
}

// ----- the agents' side ----------------------------------------------------------------------

pub fn agents_enabled() -> bool {
    !agents_off_file().exists()
}

pub fn set_agents_enabled(on: bool) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir())?;
    if on {
        let _ = std::fs::remove_file(agents_off_file());
    } else {
        std::fs::write(agents_off_file(), "")?;
    }
    Ok(())
}

/// The MCP server entry for Claude, pointing at this program's `browser-mcp` mode.
pub fn write_mcp_config(exe: &std::path::Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir())?;
    let config = serde_json::json!({
        "mcpServers": { "glasscode-browser": { "type": "stdio", "command": exe, "args": ["browser-mcp"] } }
    });
    let path = mcp_config_file();
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
    Ok(path)
}

/// Extra arguments for an agent on this Mac: the browser tools, usable without asking, told
/// which session they serve (its tab is labelled with it).
/// Nothing when it is switched off or not set up.
pub fn agent_args(agent: crate::model::AgentKind, session: &str) -> Vec<String> {
    let config = mcp_config_file();
    if !cfg!(target_os = "macos") || !agents_enabled() {
        return Vec::new();
    }
    let Some(command) = std::fs::read_to_string(&config).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["mcpServers"]["glasscode-browser"]["command"].as_str().map(str::to_string)) else {
        return Vec::new();
    };
    let args = ["browser-mcp", "--session", session];
    match agent {
        crate::model::AgentKind::Claude => {
            let config = serde_json::json!({ "mcpServers": { "glasscode-browser": { "type": "stdio", "command": command, "args": args } } });
            vec!["--mcp-config".into(), config.to_string(), "--allowedTools".into(), "mcp__glasscode-browser".into()]
        }
        crate::model::AgentKind::Codex => vec![
            "-c".into(),
            format!("mcp_servers.glasscode-browser.command={}", serde_json::to_string(&command).unwrap_or_default()),
            "-c".into(),
            format!("mcp_servers.glasscode-browser.args={}", serde_json::to_string(&args).unwrap_or_default()),
        ],
        crate::model::AgentKind::Shell => Vec::new(),
    }
}
