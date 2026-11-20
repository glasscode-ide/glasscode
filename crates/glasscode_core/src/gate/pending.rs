//! Pending actions (M21.2; ux-1, ops-11): an op that waits for the person, as data. Each has a
//! stable id, the op with its arguments and their hash, who asked and the rule that asks, what it
//! touched when it asked, an expiry and its state. They are kept in `authority/pending.json`, so
//! they survive a restart; approving one runs exactly the stored op after the gate and the
//! targets are checked again ("The pull request is closed now; nothing was done.").
//!
//! Expiries are long ones (crit-9): a UTC instant from [`crate::clock`]. The store keeps the latest
//! wall time it has seen, so a clock set back never extends one.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::op_hash;
use crate::caller::Caller;
use crate::ops::{Op, Targets};
use crate::paths::{self, MachineId, ProjectKey};
use crate::store::{self, OnDamage, Store, StoreError};

/// Where a pending action is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingState {
    #[default]
    Waiting,
    /// The person approved it; it is running.
    Approved,
    Denied,
    Expired,
    Ran,
    /// It was approved and started, and nobody knows how it ended (the app quit meanwhile).
    Uncertain,
}

impl PendingState {
    pub fn is_open(self) -> bool {
        self == PendingState::Waiting
    }
}

/// How long an action waits by default, and for an answer to a session's prompt (which goes stale
/// fast).
pub const WAIT_MS: u64 = 24 * 60 * 60 * 1000;
pub const ANSWER_WAIT_MS: u64 = 10 * 60 * 1000;
/// How long a settled action is kept (the data map's retention).
pub const KEPT_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// One action that waits for the person (design section 8, with the fields the card and the
/// re-check need).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingAction {
    /// Stable: 32 hex digits, the same from the request to the journal.
    pub id: String,
    /// A short number for `glasscode wait <n>`, the notification and the phone, increasing in the
    /// store (never reused, unlike the per-launch counter it replaces).
    pub number: u64,
    /// `{"op": …, "args": …}`: kept as JSON, so an op this build no longer knows is shown and
    /// refused, not a store that fails to load.
    pub op: Value,
    /// [`op_hash`] of the op: what an approval (and a phone's signature) is bound to.
    pub arg_hash: [u8; 32],
    /// What it touched when it asked, as Rust resolved it.
    pub targets: Targets,
    pub caller: Option<Caller>,
    /// Who asks, in words ("Claude in “Fix login”").
    pub who: String,
    /// The session it came from, when one.
    pub session: Option<String>,
    /// One sentence from the registry ("start a Claude session in app").
    pub what: String,
    /// The rule that asks ("app-3f9a1c is Careful: the manager asks before it starts a session").
    pub reason: String,
    pub project: Option<ProjectKey>,
    /// The command line, when it came as one.
    pub command: String,
    pub created_ms: u64,
    pub expires_ms: u64,
    pub state: PendingState,
    /// What happened, in words, once settled.
    pub outcome: Option<String>,
    pub settled_ms: Option<u64>,
    /// The machine it was asked on, and the only one that runs it (M57); none: this one. The other
    /// home shows it as a card and may take the person's answer to it, never run it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine: Option<MachineId>,
}

impl PendingAction {
    /// A new waiting action for `op`, asked now.
    pub fn new(op: &Op, targets: Targets, caller: Option<Caller>, now_ms: u64) -> PendingAction {
        let ttl = if op.name() == "session.answer" { ANSWER_WAIT_MS } else { WAIT_MS };
        let project = targets.projects.first().cloned();
        PendingAction {
            id: new_id(),
            op: serde_json::to_value(op).unwrap_or(Value::Null),
            arg_hash: op_hash(op),
            targets,
            caller,
            project,
            created_ms: now_ms,
            expires_ms: now_ms.saturating_add(ttl),
            ..PendingAction::default()
        }
    }

    /// The stored op, when this build knows it and its hash still matches.
    pub fn op(&self) -> Option<Op> {
        let op: Op = serde_json::from_value(self.op.clone()).ok()?;
        (op_hash(&op) == self.arg_hash).then_some(op)
    }

    /// The op's name, as stored.
    pub fn op_name(&self) -> &str {
        self.op.get("op").and_then(Value::as_str).unwrap_or("")
    }

    /// The full payload: every argument, as the op holds it, pretty, never clipped (trust-18).
    pub fn payload(&self) -> String {
        serde_json::to_string_pretty(self.op.get("args").unwrap_or(&Value::Null)).unwrap_or_default()
    }

    /// Whether it was asked on another machine than `here` (so it runs there, never here).
    pub fn asked_elsewhere(&self, here: &MachineId) -> bool {
        self.machine.as_ref().is_some_and(|machine| machine != here)
    }

    /// Whether it may still run at `now_ms`, given the latest wall time seen (`high_water_ms`).
    pub fn expired(&self, now_ms: u64, high_water_ms: u64) -> bool {
        now_ms.max(high_water_ms) >= self.expires_ms
    }

    /// The person approved it: the op to run, exactly as stored, once it is checked again (ops-11).
    /// `targets_now` is what the op touches now, resolved again from the same arguments (an error:
    /// what it named is gone); `decision_now` is the gate's decision now. Otherwise the plain
    /// sentence the person reads; nothing runs.
    pub fn recheck(&self, targets_now: Result<&Targets, &str>, decision_now: &super::Decision, now_ms: u64, high_water_ms: u64) -> Result<Op, String> {
        if !self.state.is_open() {
            return Err(format!("It is {} already; nothing was done.", format!("{:?}", self.state).to_lowercase()));
        }
        if self.expired(now_ms, high_water_ms) {
            return Err("It waited too long; nothing was done.".into());
        }
        let Some(op) = self.op() else { return Err("This Glasscode does not know that action any more; nothing was done.".into()) };
        let targets = targets_now.map_err(|why| format!("{why} Nothing was done."))?;
        if targets != &self.targets {
            return Err("What it was for has changed since it asked; nothing was done.".into());
        }
        if let super::Decision::Never { why } = decision_now {
            return Err(format!("It is not allowed any more ({why}); nothing was done."));
        }
        Ok(op)
    }
}

/// `authority/pending.json`: the pending actions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pending {
    /// The last [`PendingAction::number`] given.
    pub last_number: u64,
    /// The latest wall time this store has seen: an expiry is measured from the later of it and
    /// now, so a clock set back never extends one.
    pub high_water_ms: u64,
    pub actions: Vec<PendingAction>,
}

impl Store for Pending {
    const NAME: &'static str = "authority/pending";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    // An older generation may hold as waiting an action that has run since.
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("pending.json")
    }
}

impl Pending {
    fn see(&mut self, now_ms: u64) {
        self.high_water_ms = self.high_water_ms.max(now_ms);
    }

    /// Add a waiting action, numbered: with its own number when the app gave it one past the
    /// last (it answers the caller before the store is written), else the next.
    pub fn add(&mut self, mut action: PendingAction, now_ms: u64) -> PendingAction {
        self.see(now_ms);
        if action.number <= self.last_number || self.actions.iter().any(|a| a.number == action.number) {
            action.number = self.last_number + 1;
        }
        self.last_number = action.number;
        self.actions.retain(|a| a.id != action.id);
        self.actions.push(action.clone());
        action
    }

    pub fn get(&self, id: &str) -> Option<&PendingAction> {
        self.actions.iter().find(|a| a.id == id)
    }

    pub fn by_number(&self, number: u64) -> Option<&PendingAction> {
        self.actions.iter().find(|a| a.number == number)
    }

    /// The actions that still wait at `now_ms`.
    pub fn waiting(&self, now_ms: u64) -> Vec<&PendingAction> {
        self.actions.iter().filter(|a| a.state.is_open() && !a.expired(now_ms, self.high_water_ms)).collect()
    }

    /// Mark the waiting actions past their expiry as expired; their ids.
    pub fn expire(&mut self, now_ms: u64) -> Vec<String> {
        self.see(now_ms);
        let high = self.high_water_ms;
        let mut out = Vec::new();
        for action in self.actions.iter_mut().filter(|a| a.state.is_open() && a.expired(now_ms, high)) {
            action.state = PendingState::Expired;
            action.outcome = Some("It waited too long; nothing was done.".into());
            action.settled_ms = Some(high);
            out.push(action.id.clone());
        }
        out
    }

    /// Take a waiting action to `state` (approved, denied). False when it is not waiting or has
    /// expired: then it stays as it is (an expired one is marked so).
    pub fn settle(&mut self, id: &str, state: PendingState, outcome: Option<String>, now_ms: u64) -> Result<PendingAction, String> {
        self.see(now_ms);
        let high = self.high_water_ms;
        let action = self.actions.iter_mut().find(|a| a.id == id).ok_or_else(|| "Nothing waits under that id.".to_string())?;
        match action.state {
            PendingState::Waiting if action.expired(now_ms, high) => {
                action.state = PendingState::Expired;
                action.settled_ms = Some(high);
                action.outcome = Some("It waited too long; nothing was done.".into());
                Err("It waited too long; nothing was done.".into())
            }
            // Approved to ran or uncertain; waiting to anything.
            PendingState::Waiting | PendingState::Approved => {
                action.state = state;
                action.outcome = outcome.or(action.outcome.take());
                action.settled_ms = Some(now_ms);
                Ok(action.clone())
            }
            other => Err(format!("It is {other:?} already; nothing was done.").to_lowercase()),
        }
    }

    /// After a restart: an action approved before the quit, whose end nobody saw, is uncertain. One
    /// asked on another machine runs there (M57): its end is that machine's to say.
    pub fn after_restart(&mut self, now_ms: u64) -> Vec<String> {
        self.see(now_ms);
        let mut out = Vec::new();
        for action in self.actions.iter_mut().filter(|a| a.state == PendingState::Approved && a.machine.is_none()) {
            action.state = PendingState::Uncertain;
            action.outcome = Some("Glasscode quit while it ran: check whether it happened.".into());
            out.push(action.id.clone());
        }
        out
    }

    /// The actions asked or settled after `since_ms` (what a sync sends, M57).
    pub fn changed_since(&self, since_ms: u64) -> Vec<PendingAction> {
        self.actions.iter().filter(|a| a.created_ms > since_ms || a.settled_ms.is_some_and(|at| at > since_ms)).cloned().collect()
    }

    /// Take the other home's actions (a sync, M57; they carry no authority). `from` sent them and
    /// `here` is this machine.
    /// - One asked there (its machine, or none: the sender's own) is kept as it is there: new here,
    ///   with the next number when its own is taken, or further along there (an action only goes
    ///   forward: waiting, approved, then settled).
    /// - One asked here takes only the person's answer to it while it waits here, and only when
    ///   `answers_from_there` (the server takes the Mac's answers; the Mac never takes one from the
    ///   server, which has no person to answer).
    /// - Anything else (a third machine's) is not taken.
    ///
    /// Gives how many changed.
    pub fn merge_theirs(&mut self, theirs: &[PendingAction], from: &MachineId, here: &MachineId, answers_from_there: bool, now_ms: u64) -> usize {
        fn rank(state: PendingState) -> u8 {
            match state {
                PendingState::Waiting => 0,
                PendingState::Approved => 1,
                _ => 2,
            }
        }
        self.see(now_ms);
        let mut changed = 0;
        for action in theirs {
            let asked_on = action.machine.clone().unwrap_or_else(|| from.clone());
            if &asked_on == here {
                let Some(ours) = self.actions.iter_mut().find(|a| a.id == action.id) else { continue };
                let answered = matches!(action.state, PendingState::Approved | PendingState::Denied);
                if answers_from_there && ours.state == PendingState::Waiting && answered && ours.arg_hash == action.arg_hash {
                    ours.state = action.state;
                    ours.outcome = action.outcome.clone();
                    ours.settled_ms = action.settled_ms;
                    changed += 1;
                }
                // Approved here and handed to the other machine to run (an op only the Mac runs):
                // how it ended there is taken.
                let ended = matches!(action.state, PendingState::Ran | PendingState::Uncertain);
                if answers_from_there && ours.state == PendingState::Approved && ended && ours.arg_hash == action.arg_hash {
                    ours.state = action.state;
                    ours.outcome = action.outcome.clone();
                    ours.settled_ms = action.settled_ms;
                    changed += 1;
                }
                continue;
            }
            if &asked_on != from {
                continue;
            }
            match self.actions.iter_mut().find(|a| a.id == action.id) {
                Some(ours) => {
                    if rank(action.state) > rank(ours.state) {
                        ours.state = action.state;
                        ours.outcome = action.outcome.clone();
                        ours.settled_ms = action.settled_ms;
                        changed += 1;
                    }
                }
                None => {
                    let mut new = action.clone();
                    new.machine = Some(asked_on);
                    if new.number == 0 || self.actions.iter().any(|a| a.number == new.number) {
                        new.number = self.last_number + 1;
                    }
                    self.last_number = self.last_number.max(new.number);
                    self.actions.push(new);
                    changed += 1;
                }
            }
        }
        changed
    }

    /// Drop what was settled (or expired) more than [`KEPT_MS`] ago.
    pub fn prune(&mut self, now_ms: u64) {
        self.see(now_ms);
        let high = self.high_water_ms;
        self.actions.retain(|a| a.state.is_open() || a.settled_ms.unwrap_or(a.expires_ms).saturating_add(KEPT_MS) > high);
    }
}

/// A new stable id: 32 hex digits, random.
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    crate::sys::random_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Add a waiting action to the store (under its lock). It reads the disk: never on the main thread.
pub fn add_pending(action: PendingAction) -> Result<PendingAction, StoreError> {
    let now = crate::clock::now_ms();
    store::update::<Pending, PendingAction>(|pending| {
        pending.prune(now);
        pending.add(action, now)
    })
}

/// Settle a waiting action in the store.
pub fn settle_pending(id: &str, state: PendingState, outcome: Option<String>) -> Result<Result<PendingAction, String>, StoreError> {
    let now = crate::clock::now_ms();
    store::update::<Pending, Result<PendingAction, String>>(|pending| pending.settle(id, state, outcome, now))
}
