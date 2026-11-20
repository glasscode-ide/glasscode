//! The authority store (D10, trust-4, crit-5): the policy as the person approved it, the phones
//! this Mac paired, and the pause. Only the app writes them, in `authority/`.
//!
//! - `authority/policy.json` is the live policy, which the app writes when the person changes a
//!   project's policy in Settings. At that moment the app also copies it into the approved
//!   snapshot, `authority/approved/policy.json`. The gate reads only the snapshot.
//! - When the live file differs from the snapshot (a hand edit, a program running as the person,
//!   a damaged file), the effective policy is the stricter of the snapshot and Careful: the
//!   approved `never` entries, conditions, model accounts and "stays on this Mac" always survive.
//!   The app shows the difference in plain words ([`diff`]), and a hand edit counts only once the
//!   person imports it there ([`import_live`]).
//! - Sessions cannot write `authority/`: the sandbox and the file-tool deny rules of their profile
//!   (M22) refuse it. A program running as the person outside a sandbox still can, which is why
//!   the gate reads the snapshot. A later step can sign the snapshots with a Keychain key.
//! - Every store here fails closed: a damaged file never falls back to its older generation, which
//!   could hold a looser policy, a phone since removed, or no pause.
//! - The server gets a copy inside the Mac's sync ([`FromMac`]), without the projects that stay
//!   on the Mac, and never sends it back.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{ForgeFacts, Pause, Policy, PolicySet, ProjectPolicy, SessionOrigin};
use crate::paths::{self, MachineId, ProjectKey};
use crate::store::{self, Found, OnDamage, Opened, Store, StoreError};

/// `authority/policy.json`: the live policy, which the app writes with every approved change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PolicyFile(pub Policy);

impl Store for PolicyFile {
    const NAME: &'static str = "authority/policy";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    // An older generation may be looser.
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("policy.json")
    }
}

/// `authority/approved/policy.json`: what the person approved in the app, the only policy the
/// gate reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApprovedPolicy {
    pub policy: Policy,
    pub approved_ms: u64,
    /// [`Policy::hash`] of `policy`, for the server's copy and the doctor.
    pub hash: String,
}

impl Store for ApprovedPolicy {
    const NAME: &'static str = "authority/approved/policy";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("approved").join("policy.json")
    }
}

/// A phone this Mac paired itself (trust-m5): its key is pinned here at pairing, and a decision
/// from any other key is refused, whatever the server lists.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PairedDevice {
    /// Its public key, as its `authorized_keys` line has it.
    pub key: String,
    pub name: String,
    pub paired_ms: u64,
}

/// `authority/devices.json`: the paired phones, by device id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Devices {
    pub phones: BTreeMap<String, PairedDevice>,
}

impl Store for Devices {
    const NAME: &'static str = "authority/devices";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    // An older generation may hold a phone since removed.
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("devices.json")
    }
}

/// `authority/pause.json` holds the [`Pause`]. A damaged file counts as paused.
impl Store for Pause {
    const NAME: &'static str = "authority/pause";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    // An older generation may not be paused.
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("pause.json")
    }
}

/// Who started a session that the person did not, or drove it (sent it text), and when.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Started {
    pub origin: SessionOrigin,
    /// "manager", "playbook:<id>", or the id of the session that started it.
    pub by: String,
    pub at_ms: u64,
}

/// `authority/origins.json`: the sessions a playbook, the manager or another session started, or
/// sent text to. M22 runs them under the automation (or quarantine) profile for their whole life,
/// and the gate reads their rules from the default table. A session it does not list is one the
/// person started. Sessions cannot write it (it is in `authority/`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionOrigins {
    pub sessions: BTreeMap<String, Started>,
}

impl Store for SessionOrigins {
    const NAME: &'static str = "authority/origins";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    // An older generation may not list a session the manager drives.
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("origins.json")
    }
}

impl SessionOrigins {
    /// The origin of a session: listed, or the person's.
    pub fn origin(&self, session: &str) -> SessionOrigin {
        self.sessions.get(session).map(|s| s.origin).unwrap_or(SessionOrigin::Person)
    }

    /// Mark a session as started or driven by `by`. An origin is never lowered: a quarantined
    /// session stays one, and an automation session never becomes the person's again.
    pub fn mark(&mut self, session: &str, origin: SessionOrigin, by: &str, at_ms: u64) -> bool {
        let rank = |o: SessionOrigin| match o {
            SessionOrigin::Person => 0,
            SessionOrigin::Automation => 1,
            SessionOrigin::Quarantine => 2,
        };
        match self.sessions.get(session) {
            Some(known) if rank(known.origin) >= rank(origin) => false,
            _ if origin == SessionOrigin::Person => false,
            _ => {
                self.sessions.insert(session.to_string(), Started { origin, by: by.to_string(), at_ms });
                true
            }
        }
    }

    /// Forget the sessions that are gone.
    pub fn retain(&mut self, alive: impl Fn(&str) -> bool) {
        self.sessions.retain(|id, _| alive(id));
    }
}

/// Mark a session in the store (off the main thread).
pub fn mark_session(session: &str, origin: SessionOrigin, by: &str) -> Result<bool, StoreError> {
    let at = crate::clock::now_ms();
    store::update::<SessionOrigins, bool>(|origins| origins.mark(session, origin, by, at))
}

/// `authority/forge.json`: what each project's forge enforces for its default branch (D29), as
/// read from GitHub at project setup (never changed by Glasscode). The gate allows Autopilot's
/// forge writes, a push among them, only where all three hold.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ForgeStore {
    pub projects: BTreeMap<ProjectKey, ForgeFacts>,
}

impl Store for ForgeStore {
    const NAME: &'static str = "authority/forge";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    // An older generation may say a protection holds that was since removed.
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("forge.json")
    }
}

/// The line a project's Settings shows for its forge (D29).
pub fn forge_line(facts: Option<&ForgeFacts>) -> String {
    match facts {
        Some(f) if f.holds() => "Forge enforces: review before merge, no force-push, no bypass.".to_string(),
        Some(f) => {
            let mut missing = Vec::new();
            if !f.review_required {
                missing.push("a review before merge");
            }
            if !f.no_force_push {
                missing.push("blocking force-push");
            }
            if !f.no_bypass {
                missing.push("no bypass for you");
            }
            format!("Guidance only: the forge does not enforce {}. Autopilot does not push here.", missing.join(", "))
        }
        None => "Guidance only: Glasscode has not read this repository's branch protection. Autopilot does not push here.".to_string(),
    }
}

/// How long the server trusts the Mac's copy as it is: past it, every project is at most Careful
/// there until the Mac syncs again.
pub const MAX_COPY_AGE_MS: u64 = 24 * 60 * 60 * 1000;

/// `authority/from-mac.json`, on the server: the copy of the Mac's approved authority that came
/// with its last sync. glasscoded decides for its own sessions from it, and never sends it back.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FromMac {
    /// The Mac it came from.
    pub from: Option<MachineId>,
    /// When the Mac sent it, by the server's clock when it arrived.
    pub received_ms: u64,
    /// The Mac's approved policy, without the projects that stay on the Mac.
    pub policy: Policy,
    pub hash: String,
    /// The Mac's pause; the server's own pause is kept beside it, and the stricter wins.
    pub pause: Pause,
    /// The phones the Mac paired.
    pub devices: BTreeSet<String>,
}

impl Store for FromMac {
    const NAME: &'static str = "authority/from-mac";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("from-mac.json")
    }
}

impl FromMac {
    /// What the Mac sends inside its sync: the approved snapshot as it is effective now (stricter
    /// when the live file differs), without the projects that stay on the Mac; never the live file.
    pub fn of(authority: &Authority, from: Option<MachineId>) -> FromMac {
        let policy = authority.policy.for_server();
        let hash = policy.hash();
        FromMac { from, received_ms: 0, policy, hash, pause: authority.pause.clone(), devices: authority.devices.clone() }
    }

    /// The policy the server decides by at `now_ms`: the copy as it is while it is fresh, at most
    /// Careful once it is older than [`MAX_COPY_AGE_MS`] or when it never came.
    pub fn effective(&self, now_ms: u64) -> (Policy, bool) {
        let stale = self.from.is_none() || now_ms.saturating_sub(self.received_ms) > MAX_COPY_AGE_MS;
        if stale { (self.policy.at_most_careful(), true) } else { (self.policy.clone(), false) }
    }

    /// The server's gate set: the copy's policy, the stricter of its pause and the server's own,
    /// and the phones the Mac paired.
    pub fn policy_set(&self, own_pause: &Pause, now_ms: u64) -> PolicySet {
        let (policy, stale) = self.effective(now_ms);
        PolicySet { policy, live_differs: stale, pause: self.pause.stricter(own_pause), devices: self.devices.clone(), home: paths::home(), ..PolicySet::default() }
    }

    /// Keep a copy that came with a sync, stamped with the server's own clock. Only glasscoded
    /// calls it, and nothing ever reads this file to send it on.
    pub fn receive(mut self) -> Result<(), StoreError> {
        self.received_ms = crate::clock::now_ms();
        // A copy whose hash does not match its policy is not kept: the old one stays.
        if self.hash != self.policy.hash() {
            return Err(StoreError::Damaged { store: Self::NAME, why: "the copy's hash does not match its policy".into(), kept: None });
        }
        self.save()
    }
}

/// The authority as the app reads it, off the main thread.
#[derive(Clone, Debug, Default)]
pub struct Authority {
    /// The effective policy ([`effective`]).
    pub policy: Policy,
    pub live_differs: bool,
    /// The approved snapshot, when there is one that loads.
    pub approved: Option<Policy>,
    /// The live file, when it is there and loads: what the import screen offers.
    pub live: Option<Policy>,
    pub pause: Pause,
    pub devices: BTreeSet<String>,
    /// Who started or drives the sessions the person did not start; None when the store cannot be
    /// read, and then every session counts as driven by the manager.
    pub origins: Option<SessionOrigins>,
    /// One line for each store that could not be read, naming what stopped.
    pub problems: Vec<String>,
}

impl Authority {
    /// A session's origin by this authority: listed, else the person's; every session the manager's
    /// when the store could not be read.
    pub fn origin(&self, session: &str) -> SessionOrigin {
        match &self.origins {
            Some(origins) => origins.origin(session),
            None => SessionOrigin::Automation,
        }
    }

    /// The gate's set from this authority; the app adds what it knows of its sessions.
    pub fn policy_set(&self, person_rules: crate::control::AgentSettings) -> PolicySet {
        PolicySet {
            policy: self.policy.clone(),
            live_differs: self.live_differs,
            person_rules,
            pause: self.pause.clone(),
            devices: self.devices.clone(),
            home: paths::home(),
            ..PolicySet::default()
        }
    }
}

/// The effective policy from the approved snapshot and the live file, and whether they differ:
/// - the same: the snapshot;
/// - different, or the live file gone or unreadable (None): the snapshot at most Careful;
/// - no snapshot and no live file: the default, every project Careful;
/// - a live file and no snapshot: nothing is approved, every project Careful.
pub fn effective(approved: Option<&Policy>, live: Option<&Policy>) -> (Policy, bool) {
    match (approved, live) {
        (Some(approved), Some(live)) if approved == live => (approved.clone(), false),
        (Some(approved), _) => (approved.at_most_careful(), true),
        (None, None) => (Policy::default().at_most_careful(), false),
        (None, Some(_)) => (Policy::default().at_most_careful(), true),
    }
}

/// Read the authority. A store that cannot be read fails closed and says so in `problems`: no
/// snapshot, every project Careful; no pause, paused; no devices, no phone trusted. It reads the
/// disk: never on the main thread.
pub fn load() -> Authority {
    let mut problems = Vec::new();
    let approved = match store::open::<ApprovedPolicy>() {
        Ok(Opened { value, found: Found::File }) => Some(value),
        Ok(Opened { found: Found::Missing, .. }) => None,
        Ok(Opened { found, .. }) => {
            problems.push(format!("The approved policy was not read ({found:?}): every project is Careful."));
            None
        }
        Err(err) => {
            problems.push(format!("{err}: every project is Careful."));
            None
        }
    };
    // A snapshot whose hash does not match what it holds was not written by the app.
    let approved = approved.filter(|snapshot| {
        let whole = snapshot.hash == snapshot.policy.hash();
        if !whole {
            problems.push("The approved policy does not match its hash: every project is Careful.".into());
        }
        whole
    });
    let (live, live_missing) = match store::open::<PolicyFile>() {
        Ok(Opened { value, found: Found::File }) => (Some(value.0), false),
        Ok(Opened { found: Found::Missing, .. }) => (None, true),
        Ok(_) => (None, false),
        Err(err) => {
            problems.push(format!("{err}: the approved policy applies at most as Careful."));
            (None, false)
        }
    };
    let (mut policy, mut live_differs) = effective(approved.as_ref().map(|a| &a.policy), live.as_ref());
    if approved.is_none() && !live_missing && live.is_none() {
        // A damaged live file and no snapshot: Careful, and the import screen has nothing to show.
        policy = Policy::default().at_most_careful();
        live_differs = true;
    }
    let pause = match Pause::load() {
        Ok(pause) => pause,
        Err(err) => {
            problems.push(format!("{err}: the manager counts as paused."));
            Pause::everything()
        }
    };
    let devices = match Devices::load() {
        Ok(devices) => devices.phones.into_keys().collect(),
        Err(err) => {
            problems.push(format!("{err}: no phone is trusted until it is repaired."));
            BTreeSet::new()
        }
    };
    let origins = match SessionOrigins::load() {
        Ok(origins) => Some(origins),
        Err(err) => {
            problems.push(format!("{err}: every session counts as one the manager drives."));
            None
        }
    };
    Authority { policy, live_differs, approved: approved.map(|a| a.policy), live, pause, devices, origins, problems }
}

/// Approve a policy the person set in the app: the live file, then the snapshot (a crash between
/// the two leaves them different, which counts as Careful). A policy that names something that
/// is neither an op nor a group is refused (ops-12). Only the app calls it, from the person's own
/// click.
pub fn approve(policy: &Policy) -> Result<(), StoreError> {
    let unknown = policy.unknown_names();
    if !unknown.is_empty() {
        return Err(StoreError::Encode { store: ApprovedPolicy::NAME, why: format!("it names what is neither an op nor a group: {}", unknown.join(", ")) });
    }
    PolicyFile(policy.clone()).save()?;
    ApprovedPolicy { policy: policy.clone(), approved_ms: crate::clock::now_ms(), hash: policy.hash() }.save()
}

/// The person imports the live file in the app, after seeing [`diff`] of exactly `seen`: it is
/// approved only if the file still holds what they saw.
pub fn import_live(seen: &Policy) -> Result<(), StoreError> {
    let live = PolicyFile::load()?.0;
    if &live != seen {
        return Err(StoreError::Damaged { store: PolicyFile::NAME, why: "it changed again after you looked at it".into(), kept: None });
    }
    approve(&live)
}

/// Pause or release, under the store's lock. A damaged file is not written over: it counts as
/// paused until it is repaired, and nothing releases it.
pub fn set_pause(change: impl FnOnce(&mut Pause)) -> Result<Pause, StoreError> {
    store::update::<Pause, Pause>(|pause| {
        change(pause);
        pause.clone()
    })
}

/// A phone's device id: the first 16 hex digits of the SHA-256 of its key's type and data (the
/// comment of an OpenSSH line does not count), so the same key is the same device everywhere.
pub fn device_id(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let words: Vec<&str> = key.split_whitespace().take(2).collect();
    let digest = Sha256::digest(words.join(" ").as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Pin a phone's key at pairing (the app, with the person), under its [`device_id`].
pub fn pin_phone(key: &str, name: &str) -> Result<String, StoreError> {
    let id = device_id(key);
    pin_device(&id, key, name)?;
    Ok(id)
}

/// Pin a phone at pairing (the app, with the person).
pub fn pin_device(id: &str, key: &str, name: &str) -> Result<(), StoreError> {
    store::update::<Devices, ()>(|devices| {
        devices.phones.insert(id.to_string(), PairedDevice { key: key.to_string(), name: name.to_string(), paired_ms: crate::clock::now_ms() });
    })
}

/// Unpin a phone the person removed.
pub fn unpin_device(id: &str) -> Result<(), StoreError> {
    store::update::<Devices, ()>(|devices| {
        devices.phones.remove(id);
    })
}

/// What changed from `from` to `to`, one line each, in plain words ("acme-3f9a1c: Trust level:
/// Careful to Autopilot"), for the import screen.
pub fn diff(from: &Policy, to: &Policy) -> Vec<String> {
    let mut out = Vec::new();
    project_diff("Every other project", &from.default, &to.default, &mut out);
    let keys: BTreeSet<&ProjectKey> = from.projects.keys().chain(to.projects.keys()).collect();
    for key in keys {
        let (a, b) = (from.projects.get(key), to.projects.get(key));
        match (a, b) {
            (None, Some(b)) => {
                out.push(format!("{key}: added, {}", b.level.label()));
                project_diff(key.as_str(), &from.default, b, &mut out);
            }
            (Some(_), None) => out.push(format!("{key}: removed (it follows every other project's policy: {})", to.default.level.label())),
            (Some(a), Some(b)) => project_diff(key.as_str(), a, b, &mut out),
            (None, None) => {}
        }
    }
    out
}

fn project_diff(at: &str, a: &ProjectPolicy, b: &ProjectPolicy, out: &mut Vec<String>) {
    if a.level != b.level {
        out.push(format!("{at}: Trust level: {} to {}", a.level.label(), b.level.label()));
    }
    let lists = [("never", &a.never, &b.never), ("always asks", &a.ask, &b.ask), ("always allowed", &a.allow, &b.allow), ("allowed commands", &a.allow_commands, &b.allow_commands)];
    for (name, old, new) in lists {
        for gone in old.iter().filter(|x| !new.contains(x)) {
            out.push(format!("{at}: {name}: no longer {gone}"));
        }
        for added in new.iter().filter(|x| !old.contains(x)) {
            out.push(format!("{at}: {name}: now {added}"));
        }
    }
    for gone in a.conditions.iter().filter(|c| !b.conditions.contains(c)) {
        out.push(format!("{at}: no longer: {}", gone.describe()));
    }
    for added in b.conditions.iter().filter(|c| !a.conditions.contains(c)) {
        out.push(format!("{at}: now: {}", added.describe()));
    }
    if a.model_account != b.model_account {
        let name = |account: &Option<String>| account.clone().unwrap_or_else(|| "any of your accounts".into());
        out.push(format!("{at}: read by {} to {}", name(&a.model_account), name(&b.model_account)));
    }
    if a.local_only != b.local_only {
        out.push(format!("{at}: {}", if b.local_only { "now stays on this Mac" } else { "no longer stays on this Mac" }));
    }
}

/// The generated, read-only header of a project's `POLICY.md` in memory: the level and the rules
/// in words. The prose under it is guidance for agents, told to them and not enforced (D10).
pub fn policy_header(key: &ProjectKey, policy: &ProjectPolicy) -> String {
    let mut lines = vec![
        "<!-- Generated by Glasscode from the approved policy. Do not edit: change it in Settings › Projects. -->".to_string(),
        format!("# Policy of {key}"),
        String::new(),
        format!("- Trust level: {}. {}.", policy.level.label(), capital(policy.level.describe())),
    ];
    if !policy.never.is_empty() {
        lines.push(format!("- Never, for anyone but you: {}.", policy.never.join(", ")));
    }
    if !policy.ask.is_empty() {
        lines.push(format!("- Always asks: {}.", policy.ask.join(", ")));
    }
    if !policy.allow.is_empty() {
        lines.push(format!("- Always allowed: {}.", policy.allow.join(", ")));
    }
    for condition in &policy.conditions {
        lines.push(format!("- {}", condition.describe()));
    }
    if !policy.allow_commands.is_empty() {
        lines.push(format!("- Sessions may run without asking: {}.", policy.allow_commands.join(", ")));
    }
    if let Some(account) = &policy.model_account {
        lines.push(format!("- Only the account {account} may read this project."));
    }
    if policy.local_only {
        lines.push("- Stays on this Mac: nothing of it goes to the server.".into());
    }
    lines.push(String::new());
    lines.push("What follows is guidance for agents: told to them, not enforced.".into());
    lines.push("<!-- End of the generated header. -->".into());
    lines.join("\n") + "\n"
}

/// The first line of [`policy_header`], and its last.
const HEADER_START: &str = "<!-- Generated by Glasscode from the approved policy.";
const HEADER_END: &str = "<!-- End of the generated header. -->";

/// A project's `POLICY.md` with `header` as its generated header (M21.1 step 6): the header that
/// was there is replaced, the prose under it kept as it was; a file with no header gets it first.
pub fn with_policy_header(existing: Option<&str>, header: &str) -> String {
    let existing = existing.unwrap_or_default();
    let header = header.trim_end_matches('\n');
    let start = existing.find(HEADER_START);
    let end = start.and_then(|at| existing[at..].find(HEADER_END).map(|end| at + end + HEADER_END.len()));
    let prose = match (start, end) {
        (Some(start), Some(end)) => format!("{}{}", &existing[..start], existing[end..].trim_start_matches('\n')),
        _ => existing.to_string(),
    };
    let prose = prose.trim_start_matches('\n');
    if prose.is_empty() { format!("{header}\n") } else { format!("{header}\n\n{prose}") }
}

fn capital(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
