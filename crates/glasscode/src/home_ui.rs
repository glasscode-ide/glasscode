//! M58 on the Mac: the manager's home lease (design section 4, "Home (D5)"), engine v2 only.
//!
//! Under Automatic, the Mac asks the server's arbiter for home once you were present for a minute
//! (its window active, which a dark wake never is) and the server is reachable; it renews the
//! lease every few seconds while it holds it, drops it on every wake (it is a client until home
//! is granted again), and hands it back before sleep and when the switch moves everything to the
//! server. With no server paired the Mac is home and nothing switches. While the server is
//! unreachable the Mac is not home: nothing outward runs from it (registered claims, `ops_run`).

use std::time::{Duration, Instant};

use glasscode_core::home::{self, HomeChoice, HomeRecord, MacLease};
use glasscode_core::paths::MachineId;
use gpui::{AppContext as _, Context};
use serde_json::{Value, json};

use crate::workspace::Workspace;

/// How often the lease is renewed (it lasts `home::LEASE_MS`).
const RENEW_EVERY: Duration = Duration::from_secs(10);
/// How long after a failed claim it asks again.
const CLAIM_AGAIN: Duration = Duration::from_secs(15);

/// Where the manager acts from, as this Mac sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HomeStatus {
    /// Engine v1, or no server paired: nothing switches.
    #[default]
    Off,
    /// This Mac holds the lease at this generation.
    ThisMac(u64),
    /// The server is home.
    Server,
    /// The server cannot be reached: this Mac is not home, and nothing outward runs from it.
    Unreachable,
}

#[derive(Default)]
pub struct HomeUi {
    pub lease: MacLease,
    pub status: HomeStatus,
    mac: Option<MachineId>,
    present_since: Option<Instant>,
    busy: bool,
    renewed_at: Option<Instant>,
    tried_at: Option<Instant>,
}

impl HomeUi {
    /// The generation this Mac may act at now (registered claims), if it is home.
    pub fn generation(&self) -> Option<u64> {
        self.lease.holds(glasscode_core::clock::now_ms())
    }
}

fn arbiter(ws: &Workspace) -> Option<std::sync::Arc<glasscode_core::remote::RemoteClient>> {
    ws.server.client.clone().filter(|c| ws.server.connected() && c.hello.features.iter().any(|f| f == "home"))
}

impl Workspace {
    /// Once a second (from the workspace tick): claim, renew or note where home is.
    pub fn home_tick(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 || self.server.config.is_none() {
            self.home.status = HomeStatus::Off;
            return;
        }
        if self.window_active {
            self.home.present_since.get_or_insert_with(Instant::now);
        } else {
            self.home.present_since = None;
        }
        let Some(client) = arbiter(self) else {
            self.home.lease.woke();
            self.home.status = HomeStatus::Unreachable;
            return;
        };
        if self.home.busy {
            return;
        }
        if self.home.mac.is_none() {
            self.home.busy = true;
            let task = cx.background_spawn(async { MachineId::this().ok() });
            cx.spawn(async move |this, cx| {
                let mac = task.await;
                let _ = this.update(cx, |this, _| {
                    this.home.mac = mac;
                    this.home.busy = false;
                });
            })
            .detach();
            return;
        }
        let now = glasscode_core::clock::now_ms();
        match self.home.lease.holds(now) {
            Some(generation) => {
                if self.home.renewed_at.is_none_or(|at| at.elapsed() >= RENEW_EVERY) {
                    self.home_call(client, "home.renew", json!({ "generation": generation }), cx);
                }
            }
            None => {
                let present = self.home.present_since.map_or(0, |at| at.elapsed().as_millis() as u64);
                if home::mac_should_be_home(HomeChoice::Automatic, true, true, present) {
                    if self.home.tried_at.is_none_or(|at| at.elapsed() >= CLAIM_AGAIN) {
                        self.home.tried_at = Some(Instant::now());
                        self.home_call(client, "home.claim", json!({}), cx);
                    }
                } else if matches!(self.home.status, HomeStatus::Off | HomeStatus::Unreachable) {
                    self.home.status = HomeStatus::Server;
                }
            }
        }
    }

    /// The system woke up: whatever lease this Mac held is gone (P17: a clock may have stopped).
    pub fn home_woke(&mut self) {
        self.home.lease.woke();
        self.home.present_since = None;
        if self.home.status != HomeStatus::Off {
            self.home.status = HomeStatus::Server;
        }
    }

    /// Hand home to the server (before sleep, or after the switch moved everything there): it takes
    /// it at once. Best effort; otherwise the lease runs out and the server takes it after the grace.
    pub fn home_release(&mut self, cx: &mut Context<Self>) {
        let Some(generation) = self.home.generation() else { return };
        self.home.lease.woke();
        self.home.status = HomeStatus::Server;
        if let Some(client) = arbiter(self) {
            self.home_call(client, "home.release", json!({ "generation": generation }), cx);
        }
    }

    fn home_call(&mut self, client: std::sync::Arc<glasscode_core::remote::RemoteClient>, method: &'static str, params: Value, cx: &mut Context<Self>) {
        self.home.busy = true;
        let task = cx.background_spawn(async move { client.call_value(method, params) });
        cx.spawn(async move |this, cx| {
            let answer = task.await;
            let _ = this.update(cx, |this, cx| {
                this.home.busy = false;
                let now = glasscode_core::clock::now_ms();
                let mac = this.home.mac.clone();
                // `home.claim` answers the record; `renew` and `release` say ok with the record.
                let record: Option<HomeRecord> = answer.as_ref().ok().and_then(|v| serde_json::from_value(v.get("record").cloned().unwrap_or_else(|| v.clone())).ok());
                let ok = answer.as_ref().ok().is_some_and(|v| v.get("ok").and_then(Value::as_bool).unwrap_or(true));
                match (method, record, ok) {
                    ("home.release", _, _) => {}
                    (_, Some(record), true) if Some(&record.home) == mac.as_ref() => {
                        this.home.lease.renewed(record.generation, now);
                        this.home.renewed_at = Some(Instant::now());
                        this.home.status = HomeStatus::ThisMac(record.generation);
                    }
                    (_, _, _) => {
                        if let Err(err) = &answer {
                            log::warn!("the manager's home: {method} failed: {err:#}");
                        }
                        this.home.lease.woke();
                        this.home.status = HomeStatus::Server;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// For the title bar: where the manager acts from (v2 with a server only).
    pub fn home_label(&self) -> Option<String> {
        match self.home.status {
            HomeStatus::Off => None,
            HomeStatus::ThisMac(_) => Some(format!("Manager: this {}", crate::platform::computer())),
            HomeStatus::Server => Some(format!("Manager: {}", self.server.name())),
            HomeStatus::Unreachable => Some(format!("Manager: {} unreachable", self.server.name())),
        }
    }
}
