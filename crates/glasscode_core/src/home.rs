//! Where the manager acts from: the home record and its lease (design section 4, "Home (D5)";
//! slice M58). glasscoded keeps the record and is its only writer, and only the server's clock
//! decides when a lease ends. The Mac is home only while it holds an unexpired lease, renewed
//! with each sync; otherwise the server is. Every change of home raises the generation, which
//! fences what the old home started (effect claims carry it).
//!
//! Pure rules here; the arbiter is glasscoded's `home.*` methods.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths::{self, MachineId};
use crate::store::{OnDamage, Store};

/// How long a lease lasts after each renewal (server clock).
pub const LEASE_MS: u64 = 30_000;
/// After a lease ran out with no word from the Mac, the server waits this much more before it
/// takes home (an unplanned loss: a closed lid with no network, a crash).
pub const TAKEOVER_GRACE_MS: u64 = 15_000;
/// Under Automatic, the Mac asks for home only after the person was present this long (input
/// recently, not a dark wake), so home does not flap.
pub const PRESENT_FOR_MS: u64 = 60_000;

/// Whether the home acts fully, or only reads, follows rules, notifies and queues (nothing
/// outward) because its effect ledger may be behind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Full,
    Degraded,
}

/// Settings › Manager › Home.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeChoice {
    /// The Mac while it is awake, reachable and in use; else the server (the default once a
    /// server is paired).
    #[default]
    Automatic,
    AlwaysServer,
    AlwaysThisMac,
}

/// The home record (`authority/home.json` on the server).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeRecord {
    pub home: MachineId,
    pub generation: u64,
    /// When the Mac's lease ends (milliseconds since the Unix epoch, server clock). Meaningless
    /// while the server is home.
    #[serde(default)]
    pub lease_until_ms: u64,
    #[serde(default)]
    pub mode: Mode,
    /// What the person chose on the Mac: with "Always this Mac" the server never takes home when
    /// the lease runs out (the manager waits for the Mac).
    #[serde(default)]
    pub choice: HomeChoice,
}

/// Why the arbiter refused a step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum HomeError {
    /// The caller is not home at that generation (it lost its lease, or home moved on).
    NotHome { home: MachineId, generation: u64 },
}

impl std::fmt::Display for HomeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HomeError::NotHome { generation, .. } => write!(f, "this machine is no longer the manager's home (generation {generation} is)"),
        }
    }
}

impl std::error::Error for HomeError {}

/// The record kept on the server; none yet means the server is home at generation 0.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeFile {
    #[serde(default)]
    pub record: Option<HomeRecord>,
}

impl Store for HomeFile {
    const NAME: &'static str = "authority/home";
    const VERSION: u32 = 1;
    // It says who may act for the manager: never an older generation, never read as empty.
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("home.json")
    }
}

impl HomeRecord {
    /// The server is home, at generation 0 (no record yet).
    pub fn server(server: &MachineId) -> HomeRecord {
        HomeRecord { home: server.clone(), generation: 0, lease_until_ms: 0, mode: Mode::Full, choice: HomeChoice::Automatic }
    }

    /// Whether `machine` is home now. A Mac is home only within its lease (server clock).
    pub fn is_home(&self, machine: &MachineId, server: &MachineId, now_ms: u64) -> bool {
        &self.home == machine && (machine == server || now_ms < self.lease_until_ms)
    }

    /// The Mac renews its lease (with each sync): only while it is home at `generation` and the
    /// lease has not run out.
    pub fn renew(&mut self, mac: &MachineId, generation: u64, server: &MachineId, now_ms: u64) -> Result<(), HomeError> {
        if !(self.generation == generation && self.is_home(mac, server, now_ms)) {
            return Err(self.not_home());
        }
        self.lease_until_ms = now_ms + LEASE_MS;
        Ok(())
    }

    /// The Mac asks for home (awake, present for a minute, the server's brain turn over): it is
    /// home at the next generation, with a fresh lease. Asking while already home renews.
    pub fn grant(&mut self, mac: &MachineId, server: &MachineId, now_ms: u64) -> &HomeRecord {
        if !self.is_home(mac, server, now_ms) {
            self.home = mac.clone();
            self.generation += 1;
            self.mode = Mode::Full;
        }
        self.lease_until_ms = now_ms + LEASE_MS;
        self
    }

    /// The Mac hands home to the server (the planned hand-over before sleep, or the switch):
    /// the server is home at the next generation, at once, in Full mode.
    pub fn release(&mut self, mac: &MachineId, generation: u64, server: &MachineId) -> Result<&HomeRecord, HomeError> {
        if !(&self.home == mac && self.generation == generation) {
            return Err(self.not_home());
        }
        self.home = server.clone();
        self.generation += 1;
        self.lease_until_ms = 0;
        self.mode = Mode::Full;
        Ok(self)
    }

    /// The server's tick: a Mac's lease ran out and the grace after it passed with no word, so
    /// the server takes home at the next generation. `degraded`: the Mac's claims could not all
    /// be registered, so the server acts only within Degraded until its ledger is complete.
    pub fn expire(&mut self, server: &MachineId, now_ms: u64, degraded: bool) -> bool {
        if &self.home == server || self.choice == HomeChoice::AlwaysThisMac || now_ms < self.lease_until_ms + TAKEOVER_GRACE_MS {
            return false;
        }
        self.home = server.clone();
        self.generation += 1;
        self.lease_until_ms = 0;
        self.mode = if degraded { Mode::Degraded } else { Mode::Full };
        true
    }

    fn not_home(&self) -> HomeError {
        HomeError::NotHome { home: self.home.clone(), generation: self.generation }
    }
}

/// The Mac's own view of its lease. It counts the lease as lost after every wake and whenever no
/// renewal answer came in time, and measures with the wall clock, which runs on through sleep (a
/// monotonic clock may stop while the Mac sleeps, proof P17).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MacLease {
    /// The generation it is home at, and when the server last answered a renewal (wall clock).
    pub held: Option<(u64, u64)>,
}

impl MacLease {
    /// A renewal (or a grant) answered.
    pub fn renewed(&mut self, generation: u64, now_wall_ms: u64) {
        self.held = Some((generation, now_wall_ms));
    }

    /// The Mac woke up: whatever it held is gone; it is a client until home is granted again.
    pub fn woke(&mut self) {
        self.held = None;
    }

    /// The generation it may act at now, if any: within the lease since the last answer, with a
    /// margin for the time the answer took.
    pub fn holds(&self, now_wall_ms: u64) -> Option<u64> {
        let (generation, at) = self.held?;
        (now_wall_ms >= at && now_wall_ms - at < LEASE_MS - 5_000).then_some(generation)
    }
}

/// Register an effect with the server's arbiter before it runs (M58 step 4): the claim is made
/// there, by this Mac at `generation`, so an older home's claim never runs twice. The answer is
/// the server's [`crate::effects::Claim`]; an error (not home there, or no server) means nothing
/// outward runs.
pub fn claim_with_server(client: &crate::remote::RemoteClient, intent: &crate::effects::Intent, generation: u64) -> anyhow::Result<crate::effects::Claim> {
    client.call("home.claim_effect", serde_json::json!({ "intent": intent, "generation": generation }))
}

/// The result of a registered effect, on the server's ledger.
pub fn finish_with_server(client: &crate::remote::RemoteClient, key: &crate::effects::EffectKey, state: &crate::effects::EffectState) -> anyhow::Result<()> {
    client.call::<serde_json::Value>("home.finish_effect", serde_json::json!({ "key": key, "state": state })).map(|_| ())
}

/// Under Automatic, whether the Mac should be home: it reaches the server and the person was
/// present for [`PRESENT_FOR_MS`] (awake, not a dark wake). With no server paired the Mac is home
/// and nothing switches; "Always" choices decide by themselves.
/// Whether the Mac acts as the manager's home now: while no server is paired, or while it holds
/// the lease (`generation`). What only the home does (schedules, playbooks, moving v1's data) asks
/// this, never the pairing alone.
pub fn mac_acts_as_home(server_paired: bool, generation: Option<u64>) -> bool {
    !server_paired || generation.is_some()
}

pub fn mac_should_be_home(choice: HomeChoice, server_paired: bool, reaches_server: bool, present_for_ms: u64) -> bool {
    if !server_paired {
        return true;
    }
    match choice {
        HomeChoice::AlwaysThisMac => true,
        HomeChoice::AlwaysServer => false,
        HomeChoice::Automatic => reaches_server && present_for_ms >= PRESENT_FOR_MS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> MachineId {
        MachineId::parse(text).unwrap()
    }

    #[test]
    fn the_mac_is_home_unpaired_or_with_the_lease() {
        assert!(mac_acts_as_home(false, None));
        assert!(mac_acts_as_home(true, Some(3)));
        assert!(!mac_acts_as_home(true, None), "paired, and the server is home (or cannot be reached)");
    }

    #[test]
    fn the_server_is_home_until_the_mac_is_granted_it() {
        let (mac, server) = (id("mac"), id("server"));
        let mut record = HomeRecord::server(&server);
        assert!(record.is_home(&server, &server, 0));
        assert!(!record.is_home(&mac, &server, 0));
        let granted = record.grant(&mac, &server, 1_000).clone();
        assert_eq!((granted.home.clone(), granted.generation), (mac.clone(), 1));
        assert!(record.is_home(&mac, &server, 1_000 + LEASE_MS - 1));
        assert!(!record.is_home(&mac, &server, 1_000 + LEASE_MS), "the lease ran out (server clock)");
    }

    #[test]
    fn only_the_home_at_its_generation_renews() {
        let (mac, server, other) = (id("mac"), id("server"), id("other-mac"));
        let mut record = HomeRecord::server(&server);
        record.grant(&mac, &server, 0);
        record.renew(&mac, 1, &server, 20_000).unwrap();
        assert_eq!(record.lease_until_ms, 20_000 + LEASE_MS);
        assert!(record.renew(&mac, 0, &server, 21_000).is_err(), "an older generation");
        assert!(record.renew(&other, 1, &server, 21_000).is_err(), "another Mac");
        assert!(record.renew(&mac, 1, &server, 20_000 + LEASE_MS).is_err(), "too late: the lease is gone");
    }

    #[test]
    fn a_release_hands_home_to_the_server_at_once() {
        let (mac, server) = (id("mac"), id("server"));
        let mut record = HomeRecord::server(&server);
        record.grant(&mac, &server, 0);
        assert!(record.release(&mac, 0, &server).is_err(), "not at its generation");
        let after = record.release(&mac, 1, &server).unwrap().clone();
        assert_eq!((after.home, after.generation, after.mode), (server.clone(), 2, Mode::Full));
    }

    #[test]
    fn an_unplanned_loss_waits_for_the_lease_and_the_grace() {
        let (mac, server) = (id("mac"), id("server"));
        let mut record = HomeRecord::server(&server);
        record.grant(&mac, &server, 0);
        assert!(!record.expire(&server, LEASE_MS, false));
        assert!(!record.expire(&server, LEASE_MS + TAKEOVER_GRACE_MS - 1, false));
        assert!(record.expire(&server, LEASE_MS + TAKEOVER_GRACE_MS, true));
        assert_eq!((record.home.clone(), record.generation, record.mode), (server.clone(), 2, Mode::Degraded));
        assert!(!record.expire(&server, u64::MAX, false), "the server is home already");
        // "Always this Mac": the server never takes it; the manager waits for the Mac.
        let mut pinned = HomeRecord::server(&server);
        pinned.grant(&mac, &server, 0);
        pinned.choice = HomeChoice::AlwaysThisMac;
        assert!(!pinned.expire(&server, u64::MAX, false));
    }

    #[test]
    fn the_mac_counts_its_lease_lost_after_a_wake_or_a_late_answer() {
        let mut lease = MacLease::default();
        assert_eq!(lease.holds(0), None);
        lease.renewed(3, 100_000);
        assert_eq!(lease.holds(110_000), Some(3));
        assert_eq!(lease.holds(100_000 + LEASE_MS), None, "no renewal answer in time");
        lease.renewed(3, 200_000);
        lease.woke();
        assert_eq!(lease.holds(200_001), None, "a wake drops it");
        lease.renewed(4, 300_000);
        assert_eq!(lease.holds(299_000), None, "a clock that went back holds nothing");
    }

    #[test]
    fn automatic_home_needs_the_server_and_a_present_person() {
        assert!(mac_should_be_home(HomeChoice::Automatic, false, false, 0), "no server paired: the Mac");
        assert!(!mac_should_be_home(HomeChoice::Automatic, true, true, PRESENT_FOR_MS - 1));
        assert!(mac_should_be_home(HomeChoice::Automatic, true, true, PRESENT_FOR_MS));
        assert!(!mac_should_be_home(HomeChoice::Automatic, true, false, PRESENT_FOR_MS), "server unreachable: Degraded, not home");
        assert!(!mac_should_be_home(HomeChoice::AlwaysServer, true, true, u64::MAX));
        assert!(mac_should_be_home(HomeChoice::AlwaysThisMac, true, false, 0));
    }
}
