//! The device link (G03, `docs/mobile-glasses-plan.md` section 4): how a paired phone, with its
//! glasses, talks to Glasscode. One protocol, two carriers:
//!
//! - **to the server**, the lines of `glasscoded rpc` over SSH with the phone's key, through
//!   `glasscoded gate --device` (as before);
//! - **to the Mac**, the same lines over a TCP connection to the app's *device door*
//!   (`crates/glasscode/src/device_door.rs`), which listens only on the Mac's tailnet address.
//!
//! **The door's handshake.** The tailnet (WireGuard) carries confidentiality and peer integrity;
//! the handshake binds the connection to a key this Mac paired:
//!
//! 1. The door speaks first, one [`DoorHello`] line: its own key (P-256, made once and kept in
//!    the Keychain, pinned by the phone at pairing), this run of the app (`launch`), a fresh nonce
//!    and its signature of [`door_message`]. The phone checks it against the key it pinned
//!    ([`check_door`]) before it sends anything, so a stand-in for the Mac learns nothing.
//! 2. The phone answers one [`PhoneHello`] line: its key, a nonce of its own, and its signature of
//!    [`phone_message`], which covers the door's key, the run and both nonces.
//! 3. The door checks it ([`check_phone`]): a key this Mac pinned (`authority/devices.json`), a
//!    good signature over this connection's nonce. It answers one [`Welcome`] line. From then on
//!    the connection is `Caller::Phone { device }`, as an SSH session is.
//!
//! After that, requests are [`crate::remote::Request`] lines and answers
//! [`crate::remote::Response`] lines, as with `glasscoded rpc`; the door also writes [`Event`]
//! lines of its own (an answer as it is written, a card, a place to show). A phone may call only
//! [`crate::caller::PHONE_ALLOWED`], at both ends.
//!
//! Input to a session is still signed one by one (`remote::input_message`, #288), so the Mac
//! takes it the same way whichever carrier brought it.

use std::net::IpAddr;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The door's protocol version (in [`DoorHello::door`]).
pub const DOOR_VERSION: u32 = 1;
/// The port the door listens on, unless `GLASSCODE_DOOR_PORT` says otherwise.
pub const DEFAULT_PORT: u16 = 4519;
/// The longest line either side takes (a transcript page is the largest answer).
pub const MAX_LINE: usize = 1024 * 1024;
/// The longest a phone may take to answer the door's hello.
pub const HANDSHAKE_SECS: u64 = 10;
/// What the Mac's door key is kept as in the Keychain.
const KEYCHAIN_SERVICE: &str = "glasscode-device-door";
const KEYCHAIN_ACCOUNT: &str = "door";

/// The port of this Mac's door.
pub fn port() -> u16 {
    std::env::var("GLASSCODE_DOOR_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT)
}

// ----- the handshake ---------------------------------------------------------------------------

/// The door's first line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DoorHello {
    pub door: u32,
    /// This Mac's name, for the phone's screen.
    pub mac: String,
    /// This Mac's machine id (`authority/machine.json`), for `manager.home`.
    #[serde(default)]
    pub mac_id: String,
    /// The door's public key, OpenSSH form (`ecdsa-sha2-nistp256 AAAA…`).
    pub key: String,
    /// This run of the app: signed input and approvals name it (#288).
    pub launch: String,
    pub nonce: String,
    /// The door's signature of [`door_message`].
    pub sig: String,
    /// What this door can do beyond version 1.
    #[serde(default)]
    pub features: Vec<String>,
}

/// The phone's answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhoneHello {
    /// The phone's public key, OpenSSH form: the one it paired with.
    pub key: String,
    pub nonce: String,
    /// The phone's signature of [`phone_message`].
    pub sig: String,
    /// The phone's name and its app's version, for the title bar's panel.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub app: String,
}

/// The door's last handshake line: the device the connection is from now on, or why not.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What the door signs.
pub fn door_message(key: &str, launch: &str, nonce: &str) -> String {
    format!("glasscode-door\n{DOOR_VERSION}\n{key}\n{launch}\n{nonce}")
}

/// What the phone signs: the door it speaks to, this run, and both nonces.
pub fn phone_message(door_key: &str, launch: &str, door_nonce: &str, phone_nonce: &str) -> String {
    format!("glasscode-phone\n{DOOR_VERSION}\n{door_key}\n{launch}\n{door_nonce}\n{phone_nonce}")
}

/// A fresh nonce: 16 random bytes, hex.
pub fn nonce() -> String {
    let mut bytes = [0u8; 16];
    crate::sys::random_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn nonce_ok(nonce: &str) -> bool {
    (16..=64).contains(&nonce.len()) && nonce.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The door's hello for this connection, signed with `key`.
pub fn hello(key: &DoorKey, mac: &str, mac_id: &str, launch: &str, features: &[&str]) -> DoorHello {
    let (public, nonce) = (key.public_openssh(), nonce());
    let sig = key.sign(&door_message(&public, launch, &nonce));
    DoorHello { door: DOOR_VERSION, mac: mac.to_string(), mac_id: mac_id.to_string(), key: public, launch: launch.to_string(), nonce, sig, features: features.iter().map(|f| f.to_string()).collect() }
}

/// The phone's side: whether `hello` is from the door it pinned at pairing (`pinned`).
pub fn check_door(hello: &DoorHello, pinned: &str) -> Result<(), String> {
    if hello.door != DOOR_VERSION {
        return Err(format!("this Mac's door speaks version {}, not {DOOR_VERSION}", hello.door));
    }
    if key_words(&hello.key) != key_words(pinned) {
        return Err("this is not the Mac you paired".into());
    }
    if !nonce_ok(&hello.nonce) || hello.launch.is_empty() || !crate::remote::verify_phone_signature(pinned, door_message(&hello.key, &hello.launch, &hello.nonce).as_bytes(), &hello.sig) {
        return Err("the Mac's signature is not good".into());
    }
    Ok(())
}

/// The phone's answer to `hello`, signed with its key (for the phone's own code, and tests).
pub fn answer(hello: &DoorHello, phone: &DoorKey, name: &str) -> PhoneHello {
    let (key, nonce) = (phone.public_openssh(), nonce());
    let sig = phone.sign(&phone_message(&hello.key, &hello.launch, &hello.nonce, &nonce));
    PhoneHello { key, nonce, sig, name: name.to_string(), app: String::new() }
}

/// The door's side: whether `phone` answered this connection's `hello` with a key this Mac
/// paired (`paired`, by [`crate::gate::device_id`]). The device id it is, or why not.
pub fn check_phone(hello: &DoorHello, phone: &PhoneHello, paired: &dyn Fn(&str) -> bool) -> Result<String, String> {
    if !phone.key.starts_with("ecdsa-sha2-nistp256 ") || !nonce_ok(&phone.nonce) {
        return Err("that is not a phone's answer".into());
    }
    let device = crate::gate::device_id(&phone.key);
    if !paired(&device) {
        return Err("this phone is not paired with this Mac".into());
    }
    if !crate::remote::verify_phone_signature(&phone.key, phone_message(&hello.key, &hello.launch, &hello.nonce, &phone.nonce).as_bytes(), &phone.sig) {
        return Err("the phone's signature is not good".into());
    }
    Ok(device)
}

/// A key's type and body, without a comment.
fn key_words(key: &str) -> Vec<&str> {
    key.split_whitespace().take(2).collect()
}

// ----- the door's key --------------------------------------------------------------------------

/// A P-256 key: the Mac's door key, or (in tests and on a phone stand-in) a phone's.
pub struct DoorKey {
    signing: p256::ecdsa::SigningKey,
}

impl DoorKey {
    pub fn generate() -> DoorKey {
        loop {
            let mut bytes = [0u8; 32];
            crate::sys::random_bytes(&mut bytes);
            if let Ok(signing) = p256::ecdsa::SigningKey::from_bytes(&bytes.into()) {
                return DoorKey { signing };
            }
        }
    }

    /// From its secret scalar, hex (as the Keychain keeps it).
    pub fn from_hex(hex: &str) -> Option<DoorKey> {
        let hex = hex.trim();
        if hex.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, out) in bytes.iter_mut().enumerate() {
            *out = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        p256::ecdsa::SigningKey::from_bytes(&bytes.into()).ok().map(|signing| DoorKey { signing })
    }

    pub fn to_hex(&self) -> String {
        self.signing.to_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// This Mac's door key: the one in the Keychain, or a new one kept there.
    pub fn this_mac() -> anyhow::Result<DoorKey> {
        if let Some(key) = crate::keychain::read(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).and_then(|hex| DoorKey::from_hex(&hex)) {
            return Ok(key);
        }
        let key = DoorKey::generate();
        crate::keychain::write(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, &key.to_hex())?;
        Ok(key)
    }

    /// The public key, OpenSSH form (`ecdsa-sha2-nistp256 AAAA…`), as phone keys are written.
    pub fn public_openssh(&self) -> String {
        openssh_p256(self.signing.verifying_key())
    }

    /// A signature of `message` (SHA-256), raw r‖s, base64: what
    /// [`crate::remote::verify_phone_signature`] checks, and what CryptoKit's `rawRepresentation`
    /// gives on the phone.
    pub fn sign(&self, message: &str) -> String {
        use p256::ecdsa::signature::Signer as _;
        let signature: p256::ecdsa::Signature = self.signing.sign(message.as_bytes());
        base64::engine::general_purpose::STANDARD.encode(signature.to_bytes())
    }
}

/// A P-256 public key in OpenSSH form.
pub fn openssh_p256(key: &p256::ecdsa::VerifyingKey) -> String {
    let point = key.to_encoded_point(false);
    let mut blob = Vec::new();
    for part in [b"ecdsa-sha2-nistp256".as_slice(), b"nistp256".as_slice(), point.as_bytes()] {
        blob.extend_from_slice(&(part.len() as u32).to_be_bytes());
        blob.extend_from_slice(part);
    }
    format!("ecdsa-sha2-nistp256 {}", base64::engine::general_purpose::STANDARD.encode(blob))
}

// ----- who may connect -------------------------------------------------------------------------

/// Whether a connection from `ip` may reach the door: a tailnet address (Tailscale's IPv4 range
/// 100.64.0.0/10 and its IPv6 range fd7a:115c:a1e0::/48), and loopback only when `loopback` (the
/// tests, a debug build's simulator). Never the local network: there, nothing encrypts the lines.
pub fn peer_allowed(ip: IpAddr, loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            (a == 100 && (64..=127).contains(&b)) || (loopback && v4.is_loopback())
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return peer_allowed(IpAddr::V4(v4), loopback);
            }
            let s = v6.segments();
            (s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0) || (loopback && v6.is_loopback())
        }
    }
}

/// This Mac's tailnet IPv4 address, from the Tailscale CLI (none: Tailscale is not running).
pub fn tailscale_ipv4() -> Option<std::net::Ipv4Addr> {
    for program in ["/usr/local/bin/tailscale", "/opt/homebrew/bin/tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"] {
        if !std::path::Path::new(program).exists() {
            continue;
        }
        let Ok(out) = crate::sys::command(program).args(["ip", "-4"]).output() else { continue };
        if !out.status.success() {
            continue;
        }
        if let Some(ip) = String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse::<std::net::Ipv4Addr>().ok()).find(|ip| peer_allowed(IpAddr::V4(*ip), false)) {
            return Some(ip);
        }
    }
    None
}

// ----- lines after the handshake ---------------------------------------------------------------

/// Something the door tells the phone without being asked (a line with no `id`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub event: String,
    #[serde(default)]
    pub data: Value,
}

/// The kinds of [`Event`].
pub mod events {
    /// An answer the phone asked for, as it is written: `{ask, text, done, error?}`.
    pub const ANSWER: &str = "manager.answer";
    /// A conversation item was posted: the item.
    pub const CONVERSATION: &str = "manager.item";
    /// What the manager is doing: `{state, line}` ("thinking", "Reading checkout-fix…").
    pub const WORKING: &str = "manager.working";
    /// Show this on the device (`device.open`): `{kind, id, title}`.
    pub const OPEN: &str = "device.open";
    /// Something needs the person: a card, a session's question, a note.
    pub const NOTE: &str = "device.note";
    /// Today changed: a fresh `TodaySnapshot`.
    pub const TODAY: &str = "today";
    /// Where the manager is home changed: `manager.home`'s answer.
    pub const HOME: &str = "manager.home";
}

/// An event as one line.
pub fn event_line(event: &str, data: Value) -> String {
    serde_json::to_string(&Event { event: event.to_string(), data }).unwrap_or_default()
}

/// A request line from the phone, after the handshake: refused when too long or not a request.
pub fn parse_request(line: &str) -> Result<crate::remote::Request, String> {
    if line.len() > MAX_LINE {
        return Err("that line is too long".into());
    }
    serde_json::from_str(line.trim()).map_err(|e| format!("not a request: {e}"))
}

/// Whether a phone may call `method` at all: the one list both ends keep.
pub fn allowed(method: &str) -> bool {
    crate::caller::PHONE_ALLOWED.contains(&method)
}

/// The QR code's text at pairing: the token, the addresses to try (tailnet first), the name.
pub fn pairing_link(token: &str, addresses: &[String], name: &str) -> String {
    let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '-' }).collect();
    format!("glasscode://pair?v=2&t={token}&a={}&n={name}", addresses.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn pair() -> (DoorKey, DoorKey) {
        (DoorKey::generate(), DoorKey::generate())
    }

    #[test]
    fn handshake_needs_a_paired_key() {
        let (door, phone) = pair();
        let stranger = DoorKey::generate();
        let paired_id = crate::gate::device_id(&phone.public_openssh());
        let paired = |id: &str| id == paired_id;
        let hello = hello(&door, "mac", "0123456789abcdef", "launch-1", &[]);
        assert_eq!(check_phone(&hello, &answer(&hello, &phone, "iPhone"), &paired), Ok(paired_id.clone()));
        assert!(check_phone(&hello, &answer(&hello, &stranger, "iPhone"), &paired).is_err(), "a key this Mac did not pair");
        // The paired key, with a signature by another key.
        let mut forged = answer(&hello, &stranger, "iPhone");
        forged.key = phone.public_openssh();
        assert!(check_phone(&hello, &forged, &paired).is_err());
    }

    #[test]
    fn handshake_replayed_is_refused() {
        let (door, phone) = pair();
        let id = crate::gate::device_id(&phone.public_openssh());
        let paired = |d: &str| d == id;
        let first = hello(&door, "mac", "", "launch-1", &[]);
        let recorded = answer(&first, &phone, "iPhone");
        // The same answer on a later connection (a new nonce) is not good.
        let second = hello(&door, "mac", "", "launch-1", &[]);
        assert_ne!(first.nonce, second.nonce);
        assert!(check_phone(&second, &recorded, &paired).is_err());
    }

    #[test]
    fn another_launch_is_refused() {
        let (door, phone) = pair();
        let id = crate::gate::device_id(&phone.public_openssh());
        let paired = |d: &str| d == id;
        let first = hello(&door, "mac", "", "launch-1", &[]);
        let answered = answer(&first, &phone, "iPhone");
        let mut later = first.clone();
        later.launch = "launch-2".into();
        assert!(check_phone(&later, &answered, &paired).is_err());
    }

    #[test]
    fn the_phone_checks_the_door_it_pinned() {
        let door = DoorKey::generate();
        let other = DoorKey::generate();
        let hello = hello(&door, "mac", "", "launch-1", &["manager"]);
        assert_eq!(check_door(&hello, &door.public_openssh()), Ok(()));
        assert!(check_door(&hello, &other.public_openssh()).is_err(), "not the pinned Mac");
        // A stand-in that copies the door's key but cannot sign.
        let mut fake = super::hello(&other, "mac", "", "launch-1", &[]);
        fake.key = door.public_openssh();
        assert!(check_door(&fake, &door.public_openssh()).is_err());
    }

    #[test]
    fn a_line_before_the_handshake_is_refused() {
        let line = r#"{"id":1,"method":"state","params":{}}"#;
        assert!(serde_json::from_str::<PhoneHello>(line).is_err(), "a request is not a phone's answer");
    }

    #[test]
    fn the_door_key_round_trips() {
        let key = DoorKey::generate();
        let back = DoorKey::from_hex(&key.to_hex()).unwrap();
        assert_eq!(key.public_openssh(), back.public_openssh());
        assert!(DoorKey::from_hex("zz").is_none());
        let sig = key.sign("hello");
        assert!(crate::remote::verify_phone_signature(&key.public_openssh(), b"hello", &sig));
        assert!(!crate::remote::verify_phone_signature(&key.public_openssh(), b"other", &sig));
    }

    #[test]
    fn only_the_tailnet_reaches_the_door() {
        assert!(peer_allowed(IpAddr::V4(Ipv4Addr::new(100, 101, 1, 2)), false));
        assert!(peer_allowed(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)), false));
        assert!(!peer_allowed(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 1)), false));
        assert!(!peer_allowed(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)), false), "the local network");
        assert!(!peer_allowed(IpAddr::V4(Ipv4Addr::LOCALHOST), false));
        assert!(peer_allowed(IpAddr::V4(Ipv4Addr::LOCALHOST), true));
        assert!(peer_allowed(IpAddr::V6("fd7a:115c:a1e0::1".parse::<Ipv6Addr>().unwrap()), false));
        assert!(!peer_allowed(IpAddr::V6("fe80::1".parse::<Ipv6Addr>().unwrap()), false));
        assert!(peer_allowed(IpAddr::V6(Ipv4Addr::new(100, 90, 0, 1).to_ipv6_mapped()), false));
    }

    #[test]
    fn the_lists_agree_on_both_ends() {
        for method in ["manager.ask", "manager.home", "manager.conversation", "manager.stop", "device.presence", "today.op", "sessions.send"] {
            assert!(allowed(method), "{method}");
        }
        for method in ["sessions.create", "phones.add", "caller.declare", "owners.begin", "session.new", "approvals.decide"] {
            assert!(!allowed(method), "{method}");
        }
    }

    #[test]
    fn requests_and_events_are_lines() {
        let request = parse_request(r#"{"id":4,"method":"manager.home"}"#).unwrap();
        assert_eq!((request.id, request.method.as_str()), (4, "manager.home"));
        assert!(parse_request(&"x".repeat(MAX_LINE + 1)).is_err());
        let line = event_line(events::OPEN, serde_json::json!({"kind": "session", "id": "s1"}));
        assert!(!line.contains('\n'));
        let event: Event = serde_json::from_str(&line).unwrap();
        assert_eq!(event.event, "device.open");
    }

    #[test]
    fn the_pairing_link_carries_no_spaces() {
        let link = pairing_link("abc", &["100.70.0.2:51000".into()], "Sam's Mac");
        assert_eq!(link, "glasscode://pair?v=2&t=abc&a=100.70.0.2:51000&n=Sam-s-Mac");
    }
}
