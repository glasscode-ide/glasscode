//! The manager's voice: a LiveKit room on the person's server. A worker there turns speech
//! into text and text into speech; the brain stays wherever the manager runs. This file is
//! the part every side shares: the room's name, the messages that travel in it, and the
//! access token a client joins with.
//!
//! In the room: the client (the Mac helper, the phone) publishes the microphone. The worker
//! (identity `WORKER`) sends what it heard on topic `HEARD` as `{"text","final","from"}`,
//! speaks whatever arrives on topic `SAY` as `{"id","text"}`, stops speaking on topic `STOP`,
//! and reports `{"state":"idle|listening|thinking|speaking","id"}` on topic `STATE`. A client
//! tells the worker the manager's name and when to listen on topic `CONFIG` as
//! `{"name","listen"}`, where "listen" is "wake" (pass on what starts with the manager's
//! name, or comes within about 20 s after it spoke), "always" (an open microphone: pass on
//! every sentence) or "never". The worker keeps it per client. `from` in `HEARD` is the
//! LiveKit identity of the client that spoke; a client only answers what carries its own.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::bail;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::hmac;

use super::Settings;

pub const WORKER: &str = "manager-voice";
pub const HEARD: &str = "manager.heard";
pub const SAY: &str = "manager.say";
pub const STOP: &str = "manager.stop";
pub const STATE: &str = "manager.state";
/// A client's `{"name","listen"}` for the worker: the wake word and when to listen
/// ("wake", "always" or "never"). Sent after joining and again whenever `WORKER` joins.
pub const CONFIG: &str = "manager.config";

/// Where the LiveKit API key and secret are kept in the Keychain: one item, "key:secret".
const KEYCHAIN_SERVICE: &str = "glasscode-manager-voice";
pub(crate) const KEYCHAIN_ACCOUNT: &str = "livekit";

/// The room every device of the person joins. One person, one manager, one room: the name
/// carries nothing personal (LiveKit writes room names to its logs).
pub fn room(_settings: &Settings) -> String {
    "manager".into()
}

/// The LiveKit API key and secret, from the Keychain (None: voice is not set up).
pub fn keys() -> Option<(String, String)> {
    split_keys(&crate::keychain::read(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)?)
}

/// Keep the LiveKit API key and secret in the Keychain. Two empty strings forget them.
pub fn set_keys(key: &str, secret: &str) -> anyhow::Result<()> {
    let (key, secret) = (key.trim(), secret.trim());
    if key.is_empty() && secret.is_empty() {
        crate::keychain::delete(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT);
        return Ok(());
    }
    if key.is_empty() || secret.is_empty() {
        bail!("the LiveKit key and its secret are both needed");
    }
    if key.contains(':') || key.contains(char::is_whitespace) || secret.contains(char::is_whitespace) {
        bail!("that does not look like a LiveKit key and secret");
    }
    crate::keychain::write(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, &format!("{key}:{secret}"))
}

/// "key:secret" as it is kept, taken apart. The key never holds a colon; the secret may.
fn split_keys(kept: &str) -> Option<(String, String)> {
    let (key, secret) = kept.trim().split_once(':')?;
    (!key.is_empty() && !secret.is_empty()).then(|| (key.to_string(), secret.to_string()))
}

/// An access token for `identity` to join `room` (publish its microphone, receive audio and
/// data), valid for `ttl_secs`. A LiveKit token is a JWT signed with the API secret
/// (HS256); the server only checks it when the client connects.
pub fn token(key: &str, secret: &str, identity: &str, room: &str, ttl_secs: u64) -> anyhow::Result<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    token_at(key, secret, identity, room, ttl_secs, now)
}

/// `token` for a given "now" (unix seconds).
fn token_at(key: &str, secret: &str, identity: &str, room: &str, ttl_secs: u64, now: u64) -> anyhow::Result<String> {
    if key.trim().is_empty() || secret.is_empty() {
        bail!("the LiveKit key and secret are missing");
    }
    if identity.trim().is_empty() || room.trim().is_empty() {
        bail!("a token needs an identity and a room");
    }
    let header = serde_json::json!({ "alg": "HS256", "typ": "JWT" });
    let claims = serde_json::json!({
        "iss": key.trim(),
        "sub": identity,
        // A little in the past, so a server whose clock runs behind still lets the client in.
        "nbf": now.saturating_sub(10),
        "exp": now.saturating_add(ttl_secs.max(60)),
        "video": {
            "room": room,
            "roomJoin": true,
            "canPublish": true,
            "canSubscribe": true,
            "canPublishData": true,
        },
    });
    let signed = format!("{}.{}", URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header)?), URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?));
    let mac = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes()), signed.as_bytes());
    Ok(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(mac.as_ref())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(token: &str, index: usize) -> serde_json::Value {
        let raw = URL_SAFE_NO_PAD.decode(token.split('.').nth(index).expect("part")).expect("base64");
        serde_json::from_slice(&raw).expect("json")
    }

    #[test]
    fn token_has_three_parts_and_verifies_with_the_secret() {
        let token = token_at("APIkey123", "a-secret", "mac-ab12", "manager", 3600, 1_700_000_000).expect("token");
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| !p.is_empty() && !p.contains('=') && !p.contains('+') && !p.contains('/')));

        let signed = format!("{}.{}", parts[0], parts[1]);
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).expect("signature");
        let good = hmac::Key::new(hmac::HMAC_SHA256, b"a-secret");
        assert!(hmac::verify(&good, signed.as_bytes(), &signature).is_ok());
        let bad = hmac::Key::new(hmac::HMAC_SHA256, b"another-secret");
        assert!(hmac::verify(&bad, signed.as_bytes(), &signature).is_err());
    }

    #[test]
    fn token_carries_the_claims() {
        let token = token_at("APIkey123", "a-secret", "mac-ab12", "manager", 3600, 1_700_000_000).expect("token");
        assert_eq!(part(&token, 0), serde_json::json!({ "alg": "HS256", "typ": "JWT" }));
        let claims = part(&token, 1);
        assert_eq!(claims["iss"], "APIkey123");
        assert_eq!(claims["sub"], "mac-ab12");
        assert_eq!(claims["nbf"], 1_700_000_000u64 - 10);
        assert_eq!(claims["exp"], 1_700_000_000u64 + 3600);
        assert_eq!(
            claims["video"],
            serde_json::json!({ "room": "manager", "roomJoin": true, "canPublish": true, "canSubscribe": true, "canPublishData": true })
        );
    }

    #[test]
    fn token_of_now_is_valid_now() {
        let token = token("APIkey123", "a-secret", "mac-ab12", "manager", 600).expect("token");
        let claims = part(&token, 1);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs();
        assert!(claims["nbf"].as_u64().expect("nbf") <= now);
        assert!(claims["exp"].as_u64().expect("exp") > now + 500);
    }

    #[test]
    fn token_needs_everything() {
        assert!(token_at("", "s", "mac-1", "manager", 60, 1).is_err());
        assert!(token_at("k", "", "mac-1", "manager", 60, 1).is_err());
        assert!(token_at("k", "s", " ", "manager", 60, 1).is_err());
        assert!(token_at("k", "s", "mac-1", "", 60, 1).is_err());
    }

    #[test]
    fn kept_keys_come_apart() {
        assert_eq!(split_keys("APIkey123:se:cret\n"), Some(("APIkey123".into(), "se:cret".into())));
        assert_eq!(split_keys("APIkey123"), None);
        assert_eq!(split_keys(":secret"), None);
        assert_eq!(split_keys("APIkey123:"), None);
    }

    #[test]
    fn the_room_is_the_same_for_every_device() {
        assert_eq!(room(&Settings::default()), "manager");
        let named = Settings { name: "AI Alex".into(), ..Settings::default() };
        assert_eq!(room(&named), "manager");
    }
}
