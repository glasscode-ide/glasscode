//! The Glasscode Browser: a Chrome window of its own (its own profile, signed in once by the
//! person) that the agents on this Mac drive through the `glasscode-browser` MCP tools
//! ([`mcp`]). Each session gets its own tab, and may open any site.
//!
//! It is the installed Google Chrome (on Windows Chrome, else Microsoft Edge), started as a
//! separate instance with `--user-data-dir` and a local DevTools port (Chrome allows the port
//! only for a profile of its own anyway). The person's own Chrome is not touched.

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

/// The DevTools port of the running browser: a new random one each launch (trust-m1), which
/// Chrome writes into its profile folder (`DevToolsActivePort`, with the browser's secret path);
/// that folder is the Cache class, which sessions are denied. `GLASSCODE_BROWSER_PORT` fixes it
/// (tests).
pub fn port() -> Option<u16> {
    if let Some(port) = std::env::var("GLASSCODE_BROWSER_PORT").ok().and_then(|p| p.parse().ok()) {
        return Some(port);
    }
    active_port().map(|(port, _)| port)
}

/// `DevToolsActivePort` of the profile: the port, and the browser's WebSocket path (its secret).
fn active_port() -> Option<(u16, String)> {
    let text = std::fs::read_to_string(profile_dir().join("DevToolsActivePort")).ok()?;
    let mut lines = text.lines();
    let port = lines.next()?.trim().parse().ok()?;
    let path = lines.next().unwrap_or_default().trim().to_string();
    Some((port, path))
}

/// The `--remote-debugging-port` a launch asks for: 0, a random one, unless a test fixes it.
fn launch_port() -> u16 {
    std::env::var("GLASSCODE_BROWSER_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(0)
}

/// The browser it runs on, as the Browser dialog and messages name it.
pub const NAME: &str = if cfg!(windows) { "Chrome or Edge" } else { "Chrome" };

/// The same, in full.
pub const FULL_NAME: &str = if cfg!(windows) { "Google Chrome or Microsoft Edge" } else { "Google Chrome" };

/// Chrome (or another Chromium) to run.
pub fn chrome() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GLASSCODE_BROWSER_CHROME").map(PathBuf::from) {
        return Some(path);
    }
    installed_browsers().into_iter().find(|p| p.is_file())
}

/// Where the browsers that `chrome` takes are installed, the first choice first.
#[cfg(not(windows))]
fn installed_browsers() -> Vec<PathBuf> {
    let home = paths::home();
    vec![
        PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
        home.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
        PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
        PathBuf::from("/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
    ]
}

#[cfg(windows)]
fn installed_browsers() -> Vec<PathBuf> {
    browsers_in(|name| std::env::var_os(name))
}

/// Chrome, then Edge (which each Windows 11 has), then Brave, then Chromium; each installed for
/// all users (64-bit, then 32-bit) or for this user alone, in the folders the variables name.
#[cfg(windows)]
fn browsers_in(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let roots: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"].into_iter().filter_map(&var).map(PathBuf::from).filter(|root| root.is_absolute()).collect();
    [r"Google\Chrome\Application\chrome.exe", r"Microsoft\Edge\Application\msedge.exe", r"BraveSoftware\Brave-Browser\Application\brave.exe", r"Chromium\Application\chrome.exe"]
        .into_iter()
        .flat_map(|browser| roots.iter().map(move |root| root.join(browser)))
        .collect()
}

/// What a running Glasscode Browser says about itself.
#[derive(Clone, Debug)]
pub struct Running {
    pub ws_url: String,
    pub product: String,
}

fn http_json(path: &str) -> Option<Value> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_millis(800)).build();
    let response = agent.get(&format!("http://127.0.0.1:{}{path}", port()?)).call().ok()?;
    serde_json::from_str(&response.into_string().ok()?).ok()
}

pub fn running() -> Option<Running> {
    let version = http_json("/json/version")?;
    let ws_url = version["webSocketDebuggerUrl"].as_str()?.to_string();
    // The browser this launch wrote down, at its secret path; not whatever answers on a port.
    if std::env::var_os("GLASSCODE_BROWSER_PORT").is_none() {
        let (port, path) = active_port()?;
        if !path.is_empty() && ws_url != format!("ws://127.0.0.1:{port}{path}") {
            return None;
        }
    }
    Some(Running { ws_url, product: version["Browser"].as_str().unwrap_or_default().to_string() })
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
    // The last launch's port and secret: a new launch writes its own.
    let _ = std::fs::remove_file(profile_dir().join("DevToolsActivePort"));
    let chrome = chrome().with_context(|| format!("{FULL_NAME} is not installed (the Glasscode Browser runs on it)"))?;
    std::fs::create_dir_all(profile_dir().join("Default"))?;
    // A name and colour of its own, so its window is not mistaken for the person's Chrome.
    let preferences = profile_dir().join("Default").join("Preferences");
    if !preferences.exists() {
        let _ = std::fs::write(&preferences, serde_json::json!({ "profile": { "name": "Glasscode" }, "browser": { "theme": { "user_color": -1002662 } } }).to_string());
    }
    let mut args = vec![
        format!("--user-data-dir={}", profile_dir().display()),
        format!("--remote-debugging-port={}", launch_port()),
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
    // the person's work (`-g`); headless runs as a plain background process. Windows has no
    // `open`: a process of its own does the same there.
    let app = chrome.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(PathBuf::from);
    let started = match (&app, headless) {
        (Some(app), false) => crate::sys::command("/usr/bin/open").arg("-n").arg("-g").arg("-a").arg(app).arg("--args").args(&args).status().map(|s| s.success()),
        _ => crate::sys::spawn_detached(crate::sys::command(&chrome).args(&args)).map(|_| true),
    }
    .with_context(|| format!("could not start {NAME}"))?;
    if !started {
        bail!("{NAME} did not start");
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(r) = running() {
            return Ok(r);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!("the Glasscode Browser did not come up (no DevTools port in {})", profile_dir().display())
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
        crate::sys::remove_dir_all(&profile_dir())?;
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

/// The MCP server entry for Claude, pointing at this program's `browser-mcp` mode. On Windows the
/// app is a window program, which cannot talk on standard input and output: the `glasscode`
/// command (the console program) has the mode there.
pub fn write_mcp_config(exe: &std::path::Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir())?;
    let command = if cfg!(windows) { crate::cli::command_file() } else { exe.to_path_buf() };
    let config = serde_json::json!({
        "mcpServers": { "glasscode-browser": { "type": "stdio", "command": command, "args": ["browser-mcp"] } }
    });
    let path = mcp_config_file();
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
    Ok(path)
}

/// Extra arguments for an agent on this Mac or PC: the browser tools, usable without asking,
/// told which session they serve (its tab is labelled with it).
/// Nothing when it is switched off or not set up.
pub fn agent_args(agent: crate::model::AgentKind, session: &str) -> Vec<String> {
    let config = mcp_config_file();
    if !cfg!(any(target_os = "macos", windows)) || !agents_enabled() {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::path::Path;

    /// An environment variable set for one test, and as it was again at its end.
    struct SetVar(&'static str, Option<std::ffi::OsString>);

    impl SetVar {
        fn new(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> SetVar {
            let before = std::env::var_os(name);
            // SAFETY: the core tests run one at a time (`--test-threads=1`).
            unsafe { std::env::set_var(name, value) };
            SetVar(name, before)
        }
    }

    impl Drop for SetVar {
        fn drop(&mut self) {
            // SAFETY: as in `new`.
            match self.1.take() {
                Some(value) => unsafe { std::env::set_var(self.0, value) },
                None => unsafe { std::env::remove_var(self.0) },
            }
        }
    }

    #[test]
    fn chrome_is_the_browser_the_variable_names() {
        let dir = tempfile::tempdir().unwrap();
        let browser = dir.path().join("browser");
        let _chrome = SetVar::new("GLASSCODE_BROWSER_CHROME", &browser);
        assert_eq!(chrome(), Some(browser));
    }

    #[cfg(windows)]
    #[test]
    fn chrome_comes_first_then_edge_brave_and_chromium_on_windows() {
        let places = browsers_in(|name| match name {
            "ProgramFiles" => Some(r"C:\Program Files".into()),
            "ProgramFiles(x86)" => Some(r"C:\Program Files (x86)".into()),
            "LOCALAPPDATA" => Some(r"C:\Users\alex\AppData\Local".into()),
            _ => None,
        });
        assert_eq!(places.len(), 12, "{places:?}");
        assert_eq!(places[0], Path::new(r"C:\Program Files\Google\Chrome\Application\chrome.exe"));
        assert_eq!(places[2], Path::new(r"C:\Users\alex\AppData\Local\Google\Chrome\Application\chrome.exe"));
        assert_eq!(places[3], Path::new(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"));
        assert_eq!(places[4], Path::new(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"));
        assert!(places[6].ends_with(r"BraveSoftware\Brave-Browser\Application\brave.exe"), "{places:?}");
        assert_eq!(places[11], Path::new(r"C:\Users\alex\AppData\Local\Chromium\Application\chrome.exe"));
        // A folder that is not set, or not a full path, is not looked in.
        let places = browsers_in(|name| (name == "LOCALAPPDATA").then(|| "AppData".into()));
        assert!(places.is_empty(), "{places:?}");
    }

    #[cfg(windows)]
    #[test]
    fn claude_gets_the_browser_tools_on_windows_when_the_config_file_exists() {
        use crate::model::AgentKind;
        let home = tempfile::tempdir().unwrap();
        let _root = SetVar::new("GLASSCODE_HOME", home.path());
        assert!(agent_args(AgentKind::Claude, "s1").is_empty(), "no config file yet");
        write_mcp_config(Path::new(r"C:\Apps\Glasscode\Glasscode.exe")).unwrap();
        let args = agent_args(AgentKind::Claude, "s1");
        let at = args.iter().position(|a| a == "--mcp-config").expect("--mcp-config");
        let config: Value = serde_json::from_str(&args[at + 1]).unwrap();
        let server = &config["mcpServers"]["glasscode-browser"];
        // The console program: the app is a window program, which has no standard input.
        assert_eq!(server["command"].as_str().map(PathBuf::from), Some(crate::cli::command_file()));
        assert_eq!(server["args"], serde_json::json!(["browser-mcp", "--session", "s1"]));
        assert!(args.windows(2).any(|pair| pair == ["--allowedTools", "mcp__glasscode-browser"]), "{args:?}");
        // Switched off in the Browser dialog.
        set_agents_enabled(false).unwrap();
        assert!(agent_args(AgentKind::Claude, "s1").is_empty());
    }

    /// A real browser, headless and with a profile in a new temporary folder; by hand only, as it
    /// needs Chrome or Edge: `cargo test -p glasscode_core headless_browser -- --ignored`.
    #[test]
    #[ignore = "starts Chrome or Edge (headless)"]
    fn headless_browser_answers_on_its_port_and_reset_removes_its_profile() {
        let home = tempfile::tempdir().unwrap();
        let _root = SetVar::new("GLASSCODE_HOME", home.path());
        let _headless = SetVar::new("GLASSCODE_BROWSER_HEADLESS", "1");
        assert!(running().is_none(), "something answers on port {:?} already; this test starts a browser of its own", port());
        let started = Instant::now();
        let browser = ensure_running().unwrap();
        let took = started.elapsed();
        assert!(took < Duration::from_secs(20), "{took:?}");
        assert!(browser.ws_url.starts_with(&format!("ws://127.0.0.1:{}/", port().unwrap())), "{}", browser.ws_url);
        assert!(profile_dir().starts_with(home.path()) && profile_dir().is_dir());
        // A page, as an agent's tab opens.
        cdp::once(&browser.ws_url, "Target.createTarget", serde_json::json!({ "url": "about:blank", "background": true })).unwrap();
        assert!(!tabs().is_empty());
        reset().unwrap();
        assert!(running().is_none(), "it still answers after reset");
        assert!(!profile_dir().exists(), "its profile is still there");
    }
}

#[cfg(test)]
mod secret_tests {
    use super::*;

    /// trust-m1: the DevTools port is a new random one each launch, written only into the
    /// profile folder with the browser's secret path; whatever answers on another port, or at
    /// another path, is not taken for the Glasscode Browser.
    #[test]
    fn devtools_requires_secret() {
        let _home = crate::testkit::home::Home::new();
        // SAFETY: the core tests run one at a time.
        unsafe { std::env::remove_var("GLASSCODE_BROWSER_PORT") };
        assert_eq!(launch_port(), 0, "a random port, not a fixed open one");
        assert_eq!(port(), None, "no launch wrote one");
        std::fs::create_dir_all(profile_dir()).unwrap();
        std::fs::write(profile_dir().join("DevToolsActivePort"), "54321\n/devtools/browser/0b1c-secret\n").unwrap();
        assert_eq!(port(), Some(54321));
        assert_eq!(active_port().unwrap().1, "/devtools/browser/0b1c-secret");
        // Nothing listens there: not running, and no guess at 9333.
        assert!(running().is_none());
        assert!(crate::paths::class_of(&profile_dir().join("DevToolsActivePort")).class() == Some(crate::paths::PathClass::Cache), "sessions are denied it");
    }
}
