//! Who a socket is, in the open-source relay: any device that presents the relay's shared
//! secret. There is no account server and no per-device registry — one secret admits all of
//! your devices, and they can all reach each other. Rotate the secret to lock everyone out.
//!
//! A device names itself with headers (`x-device-id`, `x-device-kind`, `x-device-name`); the
//! secret is what authorises it, the id is only a label so two sockets can tell each other
//! apart. Every device that knows the secret shares one group, so keep the secret long,
//! random and private: anyone who has it can reach every device on this relay.

use axum::http::HeaderMap;

/// The single group every authorised device belongs to on this relay.
const GROUP: &str = "shared";

/// A socket's identity after the secret checks out.
#[derive(Clone, Debug)]
pub struct Identity {
    /// Always [`GROUP`] here: one relay, one secret, one group of devices.
    pub user: String,
    /// The device's own id (a label it chooses; must be unique among your devices).
    pub device: String,
    /// "mac", "phone", "server", … — shown in presence and the device list.
    pub kind: String,
    /// A human name for the device, shown in the list.
    pub name: String,
}

pub struct Auth {
    secret: String,
}

impl Auth {
    pub fn new(secret: String) -> Auth {
        Auth { secret }
    }

    /// Check the `Authorization: Bearer <secret>` header against the relay secret, then read
    /// the device's self-description from its headers.
    pub fn identify(&self, headers: &HeaderMap) -> Result<Identity, (u16, String)> {
        let bearer = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")))
            .map(str::trim)
            .ok_or((401, "set Authorization: Bearer <relay secret>".to_string()))?;
        if !self.secret_ok(bearer) {
            return Err((401, "wrong relay secret".to_string()));
        }
        let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let device = header("x-device-id").ok_or((400, "set x-device-id to this device's id".to_string()))?;
        let kind = header("x-device-kind").unwrap_or_else(|| "device".to_string());
        let name = header("x-device-name").unwrap_or_else(|| device.clone());
        Ok(Identity { user: GROUP.to_string(), device, kind, name })
    }

    /// Constant-time comparison over the secret's length (never short-circuits on a mismatch).
    fn secret_ok(&self, given: &str) -> bool {
        let expected = self.secret.as_bytes();
        let given = given.as_bytes();
        given.len() == expected.len() && given.iter().zip(expected).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
    }
}
