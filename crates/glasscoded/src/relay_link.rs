//! Reaching this server through the self-hosted Glasscode relay (see `relay/` and
//! docs/CONNECTIVITY.md). With a relay configured, the daemon keeps a `listen` socket open so
//! the Mac and phone reach it from any network, with no inbound port here. Without one, nothing
//! runs in this module and devices reach the server over SSH or Tailscale directly.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use glasscode_core::paths;
use glasscode_core::relay::{self, Device, Event};

/// What `glasscoded status` shows about the relay.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RelayStatus {
    pub signed_in: bool,
    pub connected: bool,
    pub devices_allowed: usize,
    pub online: Vec<String>,
    pub error: Option<String>,
}

pub static STATUS: Mutex<RelayStatus> = Mutex::new(RelayStatus {
    signed_in: false,
    connected: false,
    devices_allowed: 0,
    online: Vec::new(),
    error: None,
});

fn set_status(f: impl FnOnce(&mut RelayStatus)) {
    if let Ok(mut s) = STATUS.lock() {
        f(&mut s);
    }
}

/// The relay this server dials out to, kept in `~/.glasscode/relay.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayConfig {
    /// The relay's base URL, e.g. `wss://relay.example.com` (or `ws://host:8080`).
    pub url: String,
    /// The shared secret every device of yours presents.
    pub secret: String,
    /// This machine's device id (a unique label the phone connects `to`).
    #[serde(default)]
    pub device_id: String,
}

fn config_path() -> PathBuf {
    paths::root().join("relay.json")
}

/// A stable, safe device id from this machine's hostname.
fn default_device_id() -> String {
    let host = crate::daemon::hostname();
    let cleaned: String = host.trim().chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    if cleaned.is_empty() { "glasscode-server".to_string() } else { cleaned }
}

pub fn load_config() -> Option<RelayConfig> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    let config: RelayConfig = serde_json::from_str(&text).ok()?;
    (!config.url.is_empty() && !config.secret.is_empty()).then_some(config)
}

/// Whether this server is set up to reach a relay.
pub fn load() -> Option<()> {
    load_config().map(|_| ())
}

/// Write the relay config (filling in a device id if none is given) and return it.
pub fn configure(url: &str, secret: &str) -> anyhow::Result<RelayConfig> {
    if url.is_empty() || secret.is_empty() {
        anyhow::bail!("usage: glasscoded relay <url> <secret>");
    }
    let existing = load_config();
    let device_id = existing.as_ref().map(|c| c.device_id.clone()).filter(|d| !d.is_empty()).unwrap_or_else(default_device_id);
    let config = RelayConfig { url: url.trim_end_matches('/').to_string(), secret: secret.to_string(), device_id };
    std::fs::create_dir_all(paths::root())?;
    std::fs::write(config_path(), serde_json::to_string_pretty(&config)?)?;
    Ok(config)
}

/// Forget the relay config. Returns whether there was one.
pub fn disable() -> anyhow::Result<bool> {
    let path = config_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
        return Ok(true);
    }
    Ok(false)
}

/// The Mac's voice control server (`control` channel). Port from `GLASSCODE_CONTROL_PORT`,
/// default 4517.
fn voice_addr() -> SocketAddr {
    let port = std::env::var("GLASSCODE_CONTROL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(4517u16);
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Where each channel is joined locally: `control` → the voice server, `ssh` → the sshd.
fn route(channel: &str) -> Option<SocketAddr> {
    match channel {
        "control" | "" => Some(voice_addr()),
        "ssh" => Some(SocketAddr::from(([127, 0, 0, 1], 22))),
        _ => None,
    }
}

/// The daemon's relay loop: when configured, keep a listener up (reconnecting), else do nothing.
pub fn run() {
    let Some(config) = load_config() else { return };
    relay::install_crypto();
    let me = Device {
        id: if config.device_id.is_empty() { default_device_id() } else { config.device_id.clone() },
        kind: "mac".to_string(),
        name: crate::daemon::hostname(),
    };
    set_status(|s| {
        s.signed_in = true;
        s.error = None;
    });

    let secret = config.secret.clone();
    let token: Arc<dyn Fn() -> anyhow::Result<String> + Send + Sync> = Arc::new(move || Ok(secret.clone()));
    let route_fn: Arc<dyn Fn(&str) -> Option<SocketAddr> + Send + Sync> = Arc::new(route);
    let on_event: Arc<dyn Fn(&Event) + Send + Sync> = Arc::new(|event| match event {
        Event::Hello { online, .. } => {
            let names: Vec<String> = online.iter().filter_map(|v| v.get("device").and_then(|d| d.as_str()).map(String::from)).collect();
            set_status(|s| {
                s.connected = true;
                s.online = names;
            });
        }
        Event::Presence { device, online, .. } => set_status(|s| {
            s.online.retain(|d| d != device);
            if *online {
                s.online.push(device.clone());
            }
        }),
        Event::Incoming { from_kind, from_name, .. } => {
            crate::daemon::log(format!("relay: {from_kind} {from_name} connected"));
        }
    });
    let stop = Arc::new(AtomicBool::new(false));

    loop {
        set_status(|s| s.connected = false);
        match relay::listen(&config.url, me.clone(), token.clone(), route_fn.clone(), on_event.clone(), stop.clone()) {
            Ok(()) => set_status(|s| s.error = None),
            Err(error) => {
                crate::daemon::log(format!("relay: {error:#}"));
                set_status(|s| s.error = Some(format!("{error:#}")));
            }
        }
        set_status(|s| {
            s.connected = false;
            s.online.clear();
        });
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_channels_to_local_ports() {
        assert_eq!(route("control"), Some(voice_addr()));
        assert_eq!(route(""), Some(voice_addr()));
        assert_eq!(route("ssh"), Some(SocketAddr::from(([127, 0, 0, 1], 22))));
        assert_eq!(route("anything-else"), None);
        // control defaults to 4517 when the env var is unset.
        assert_eq!(voice_addr().port(), std::env::var("GLASSCODE_CONTROL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(4517u16));
    }

    #[test]
    fn relay_config_round_trips_as_json() {
        let config = RelayConfig { url: "wss://relay.example.com".into(), secret: "s3cr3t-value-here".into(), device_id: "alexs-mac".into() };
        let json = serde_json::to_string_pretty(&config).unwrap();
        let back: RelayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.url, config.url);
        assert_eq!(back.secret, config.secret);
        assert_eq!(back.device_id, config.device_id);
        // device_id is optional on read (older files / hand-written config).
        let minimal: RelayConfig = serde_json::from_str(r#"{"url":"ws://host:8080","secret":"abc"}"#).unwrap();
        assert_eq!(minimal.device_id, "");
    }

    #[test]
    fn default_device_id_is_safe() {
        let id = default_device_id();
        assert!(!id.is_empty());
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
