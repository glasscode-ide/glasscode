//! glasscode-relay: a WebSocket switchboard between your own Glasscode devices.
//!
//! Every device dials out to it over one WebSocket, so a phone on mobile data reaches a Mac
//! behind NAT without a VPN and without opening a port on the Mac.
//!
//! - A **host** (the Mac, a build server) keeps a *listener* socket open (`/v1/listen`).
//! - A **client** (the iPhone, the Mac) opens `/v1/connect?to=<device>`. The relay tells that
//!   device's listener, the host opens a `/v1/accept` socket, and the relay joins the two and
//!   passes binary frames through until either side closes.
//!
//! Authorisation is a single shared secret you set (`RELAY_SECRET`): every device that
//! presents it belongs to one group and can reach the others. There is no account server and
//! nothing hosted — the relay is yours to run. Rotate the secret to lock every device out.

pub mod auth;
pub mod switchboard;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;

/// The relay's configuration: just the shared secret.
#[derive(Clone, Debug)]
pub struct Config {
    pub secret: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Config> {
        let secret = std::env::var("RELAY_SECRET").map_err(|_| anyhow::anyhow!("RELAY_SECRET must be set (a long random string shared by your devices)"))?;
        if secret.len() < 16 {
            anyhow::bail!("RELAY_SECRET is too short; use at least 16 random characters");
        }
        Ok(Config { secret })
    }
}

pub fn router(state: Arc<switchboard::Switchboard>) -> Router {
    Router::new()
        .route("/v1/listen", get(switchboard::listen))
        .route("/v1/connect", get(switchboard::connect))
        .route("/v1/accept", get(switchboard::accept))
        .route("/v1/online", get(switchboard::online))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state)
}
