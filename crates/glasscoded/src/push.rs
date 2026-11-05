//! Notifications to paired iPhones through Apple's push service, so "Claude wants to push"
//! reaches the Lock Screen while the app is closed.
//!
//! Off until the server has the team's APNs key: `~/.glasscode/push.json`
//! `{"key_id": "ABC123DEFG", "team_id": "TEAM123456", "key_path": "~/.glasscode/AuthKey_ABC123DEFG.p8"}`
//! (optionally `"topic"`, the app's bundle id). Signing uses `openssl` and sending uses `curl`
//! (HTTP/2), which servers have; no key or token ever leaves this machine except to Apple.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use glasscode_core::paths;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Config {
    key_id: String,
    team_id: String,
    key_path: String,
    #[serde(default = "default_topic")]
    topic: String,
}

fn default_topic() -> String {
    "dev.glasscode".into()
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Device {
    token: String,
    /// Development builds get their pushes from Apple's sandbox.
    sandbox: bool,
    name: String,
}

fn config() -> Option<Config> {
    let text = std::fs::read_to_string(paths::root().join("push.json")).ok()?;
    let mut config: Config = serde_json::from_str(&text).ok()?;
    if let Some(rest) = config.key_path.strip_prefix("~/") {
        config.key_path = paths::home().join(rest).display().to_string();
    }
    Some(config)
}

pub fn configured() -> bool {
    config().is_some()
}

fn devices_file() -> std::path::PathBuf {
    paths::root().join("push-devices.json")
}

fn devices() -> Vec<Device> {
    std::fs::read(devices_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_devices(devices: &[Device]) {
    if let Ok(bytes) = serde_json::to_vec_pretty(devices) {
        let _ = std::fs::write(devices_file(), bytes);
    }
}

/// A phone's push token (from `phones.push_token`), replacing its earlier one.
pub fn register(token: &str, sandbox: bool, name: &str) -> anyhow::Result<()> {
    if token.len() < 32 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("not a push token");
    }
    let mut list: Vec<Device> = devices().into_iter().filter(|d| d.token != token && d.name != name).collect();
    list.push(Device { token: token.to_lowercase(), sandbox, name: name.to_string() });
    save_devices(&list);
    Ok(())
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        let take = chunk.len() + 1;
        for i in 0..take {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// An ECDSA signature from openssl (DER) as JWT wants it: r and s, 32 bytes each.
fn der_to_raw(der: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut at = 0;
    let mut next = |expect: u8| -> anyhow::Result<Vec<u8>> {
        if der.get(at) != Some(&expect) {
            bail!("unexpected signature encoding");
        }
        let len = *der.get(at + 1).context("short signature")? as usize;
        let start = at + 2;
        at = if expect == 0x30 { start } else { start + len };
        der.get(start..start + len).map(<[u8]>::to_vec).context("short signature")
    };
    next(0x30)?;
    let mut raw = Vec::with_capacity(64);
    for _ in 0..2 {
        let int = next(0x02)?;
        let int: Vec<u8> = int.into_iter().skip_while(|&b| b == 0).collect();
        if int.len() > 32 {
            bail!("unexpected signature length");
        }
        raw.extend(std::iter::repeat_n(0u8, 32 - int.len()));
        raw.extend(int);
    }
    Ok(raw)
}

static TOKEN: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// The provider token (a JWT signed with the team's key), reused for 40 minutes.
fn provider_token(config: &Config) -> anyhow::Result<String> {
    if let Some((token, made)) = TOKEN.lock().ok().and_then(|t| t.clone()) {
        if made.elapsed() < Duration::from_secs(40 * 60) {
            return Ok(token);
        }
    }
    let header = base64url(format!(r#"{{"alg":"ES256","kid":"{}"}}"#, config.key_id).as_bytes());
    let iat = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let claims = base64url(format!(r#"{{"iss":"{}","iat":{iat}}}"#, config.team_id).as_bytes());
    let input = format!("{header}.{claims}");
    let mut openssl = Command::new("openssl")
        .args(["dgst", "-sha256", "-sign", &config.key_path])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("openssl is not installed")?;
    {
        use std::io::Write;
        openssl.stdin.take().context("no stdin")?.write_all(input.as_bytes())?;
    }
    let out = openssl.wait_with_output()?;
    if !out.status.success() {
        bail!("could not sign with the APNs key: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let token = format!("{input}.{}", base64url(&der_to_raw(&out.stdout)?));
    if let Ok(mut slot) = TOKEN.lock() {
        *slot = Some((token.clone(), Instant::now()));
    }
    Ok(token)
}

/// What a notification says and where tapping it goes.
pub struct Note {
    pub title: String,
    pub body: String,
    pub session_id: String,
    /// "permission" (Allow / Open) or "question" (Open).
    pub category: &'static str,
    pub badge: usize,
}

/// Send to every registered phone, in the background. Tokens Apple says are gone are dropped.
pub fn send(note: Note) {
    let Some(config) = config() else { return };
    let list = devices();
    if list.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let jwt = match provider_token(&config) {
            Ok(jwt) => jwt,
            Err(err) => return crate::daemon::log(format!("push: {err:#}")),
        };
        let payload = serde_json::json!({
            "aps": {
                "alert": { "title": note.title, "body": note.body },
                "sound": "default",
                "badge": note.badge,
                "category": note.category,
                "thread-id": note.session_id,
            },
            "session": note.session_id,
        })
        .to_string();
        let mut gone: Vec<String> = Vec::new();
        for device in &list {
            let host = if device.sandbox { "api.sandbox.push.apple.com" } else { "api.push.apple.com" };
            let out = Command::new("curl")
                .args(["--http2", "-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "15", "-X", "POST"])
                .args(["-H", &format!("authorization: bearer {jwt}")])
                .args(["-H", &format!("apns-topic: {}", config.topic)])
                .args(["-H", "apns-push-type: alert", "-H", "apns-priority: 10"])
                .args(["-d", &payload, &format!("https://{host}/3/device/{}", device.token)])
                .output();
            match out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()) {
                Ok(code) if code == "200" => {}
                Ok(code) if code == "410" || code == "400" => gone.push(device.token.clone()),
                Ok(code) => crate::daemon::log(format!("push to {}: HTTP {code}", device.name)),
                Err(err) => crate::daemon::log(format!("push: curl: {err}")),
            }
        }
        if !gone.is_empty() {
            save_devices(&devices().into_iter().filter(|d| !gone.contains(&d.token)).collect::<Vec<_>>());
        }
    });
}

/// Remember which needs were already announced, so each is sent once.
#[derive(Default)]
pub struct Announced(HashMap<String, String>);

impl Announced {
    /// True when `key` (what is needed) is new for this session.
    pub fn fresh(&mut self, session: &str, key: &str) -> bool {
        if self.0.get(session).is_some_and(|k| k == key) {
            return false;
        }
        self.0.insert(session.to_string(), key.to_string());
        true
    }

    pub fn clear(&mut self, session: &str) {
        self.0.remove(session);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes_and_converts_signatures() {
        assert_eq!(super::base64url(b"{\"alg\":\"ES256\"}"), "eyJhbGciOiJFUzI1NiJ9");
        assert_eq!(super::base64url(&[0xfb, 0xff]), "-_8");
        // SEQUENCE { INTEGER (33 bytes, leading 0), INTEGER (31 bytes) }
        let mut der = vec![0x30, 68, 0x02, 33, 0x00];
        der.extend([0xAA; 32]);
        der.extend([0x02, 31]);
        der.extend([0xBB; 31]);
        let raw = super::der_to_raw(&der).unwrap();
        assert_eq!(raw.len(), 64);
        assert_eq!(&raw[..32], &[0xAA; 32]);
        assert_eq!(raw[32], 0);
        assert_eq!(&raw[33..], &[0xBB; 31]);
    }
}
