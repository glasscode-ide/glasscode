//! The owner record of each worktree that has moved (`glasscode_core::owners`, in
//! `authority/owners.json`). This daemon is the records' only writer, and the server's clock is
//! the only one that ends a stale move. The switch on the Mac drives a move through
//! `owners.begin`, then `worktrees.receive` and `sessions.create` with the move's tag (the
//! sessions wait, held), then `owners.commit`, which starts them, or `owners.abort`, which
//! removes them.
//!
//! A damaged file is never read as empty or as its older generation: moves stop (every step
//! fails) until it is repaired, and the sessions already here keep running.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use anyhow::anyhow;
use glasscode_core::owners::{OwnerRecord, Owners, WorktreeKey};
use glasscode_core::paths::MachineId;
use glasscode_core::remote::{OwnersBegin, OwnersReply};
use glasscode_core::store::Store;
use serde_json::Value;

use crate::daemon::{Shared, lock, log};

/// The records, or None when the file is damaged.
static OWNERS: OnceLock<Mutex<Option<Owners>>> = OnceLock::new();
static MACHINE: OnceLock<Option<MachineId>> = OnceLock::new();

/// This server's machine id (`authority/machine.json` here), if it could be read or made.
pub fn machine() -> Option<MachineId> {
    MACHINE
        .get_or_init(|| match MachineId::this() {
            Ok(id) => Some(id),
            Err(err) => {
                log(format!("no machine id, so worktrees cannot move by owner record: {err}"));
                None
            }
        })
        .clone()
}

fn records() -> MutexGuard<'static, Option<Owners>> {
    OWNERS
        .get_or_init(|| {
            Mutex::new(match Owners::load() {
                Ok(owners) => Some(owners),
                Err(err) => {
                    log(format!("the worktree owners cannot be read ({err}); worktrees cannot move until {} is repaired", Owners::path().display()));
                    None
                }
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn damaged() -> anyhow::Error {
    anyhow!("the server's worktree owner records are damaged ({}); worktrees cannot move until it is repaired", Owners::path().display())
}

fn save(owners: &Owners) -> anyhow::Result<()> {
    owners.save().map_err(|err| anyhow!("could not save the worktree owners: {err}"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Whether this server may run agents in `key`'s worktree now: it owns it (a worktree here with
/// no record is its own) and no move of it is under way.
pub fn may_run_here(key: &WorktreeKey) -> bool {
    let Some(me) = machine() else { return true };
    match records().as_ref() {
        Some(owners) => owners.may_run(key, &me, &me),
        // Moves are stopped: what runs here keeps running.
        None => true,
    }
}

/// Who owns `key` when it is not this server, for messages.
pub fn owner_elsewhere(key: &WorktreeKey) -> Option<OwnerRecord> {
    let me = machine()?;
    let record = records().as_ref()?.get(key, &me);
    (record.owner != me || record.moving.is_some()).then_some(record)
}

/// A step of the move of `key` to `epoch` arrived: it must be a move to this server, and it is
/// progress, so the move does not go stale.
pub fn moving_here(key: &WorktreeKey, epoch: u64) -> anyhow::Result<()> {
    let me = machine().ok_or_else(|| anyhow!("this server has no machine id"))?;
    let mut guard = records();
    let owners = guard.as_mut().ok_or_else(damaged)?;
    if owners.moving_to(key, epoch) != Some(&me) {
        anyhow::bail!("this move is no longer under way (it was given up, or another one replaced it)");
    }
    owners.touch(key, epoch, now_ms()).map_err(|err| anyhow!("{err}"))
}

/// The records that exist among `keys` (none while the file is damaged).
pub fn get(keys: &[WorktreeKey]) -> HashMap<WorktreeKey, OwnerRecord> {
    let guard = records();
    let Some(owners) = guard.as_ref() else { return HashMap::new() };
    keys.iter().filter_map(|key| Some((key.clone(), owners.records.get(key)?.clone()))).collect()
}

/// Moves that made no progress for too long: given up (with the epoch each would have had).
pub fn expire() -> Vec<(WorktreeKey, u64)> {
    let mut guard = records();
    let Some(owners) = guard.as_mut() else { return Vec::new() };
    let gone = owners.expire(now_ms());
    if !gone.is_empty() {
        if let Err(err) = save(owners) {
            log(format!("{err:#}"));
        }
    }
    gone
}

/// This server continued a Mac's worktree on its own (`mirror.take_over`, a parked copy opened
/// here): it owns it now, at a new epoch, so the Mac stops its copy when it next syncs.
pub fn taken_here(key: &WorktreeKey, mac: &MachineId) -> Option<OwnerRecord> {
    let me = machine()?;
    let mut guard = records();
    let owners = guard.as_mut()?;
    let record = owners.take(key, &me, mac);
    if let Err(err) = save(owners) {
        log(format!("{err:#}"));
    }
    Some(record)
}

fn reply(result: Result<OwnerRecord, glasscode_core::owners::OwnerError>) -> anyhow::Result<Value> {
    Ok(serde_json::to_value(match result {
        Ok(record) => OwnersReply::Ok { record },
        Err(error) => OwnersReply::Refused { error },
    })?)
}

pub fn dispatch(shared: &Shared, method: &str, params: &Value) -> Option<anyhow::Result<Value>> {
    let answer = match method {
        "owners.get" => (|| {
            let keys: Vec<WorktreeKey> = serde_json::from_value(params.get("keys").cloned().unwrap_or(Value::Array(Vec::new())))?;
            if records().is_none() {
                return Err(damaged());
            }
            Ok(serde_json::to_value(get(&keys))?)
        })(),
        "owners.begin" => (|| {
            let begin: OwnersBegin = serde_json::from_value(params.clone())?;
            let me = machine().ok_or_else(|| anyhow!("this server has no machine id"))?;
            // Without a record, a worktree belongs to the machine it leaves.
            let holder = if begin.to == me { begin.from.clone() } else { me.clone() };
            let result = {
                let mut guard = records();
                let owners = guard.as_mut().ok_or_else(damaged)?;
                let result = owners.begin(&begin.key, &begin.from, &begin.to, begin.epoch, &holder, now_ms());
                if result.is_ok() {
                    save(owners)?;
                }
                result
            };
            if let Ok(record) = &result {
                let epoch = record.moving.as_ref().map_or(record.epoch + 1, |m| m.epoch);
                if begin.to != me {
                    // Leaving: its agents stop, and nothing may start here while it moves.
                    lock(shared).stop_for_move(&begin.key, epoch);
                }
                log(format!("moving {} to {} (epoch {epoch})", begin.key.0, begin.to.as_str()));
            }
            reply(result)
        })(),
        "owners.commit" => (|| {
            let (key, epoch) = key_and_epoch(params)?;
            let me = machine().ok_or_else(|| anyhow!("this server has no machine id"))?;
            let result = {
                let mut guard = records();
                let owners = guard.as_mut().ok_or_else(damaged)?;
                let result = owners.commit(&key, epoch);
                if result.is_ok() {
                    // Nothing starts or stops before the new owner is on disk.
                    save(owners)?;
                }
                result
            };
            if let Ok(record) = &result {
                let mut daemon = lock(shared);
                if record.owner == me {
                    daemon.start_held(&key, epoch);
                } else {
                    daemon.after_move_away(&key, epoch, true);
                }
                log(format!("{} now runs on {} (epoch {epoch})", key.0, if record.owner == me { "this server" } else { record.owner.as_str() }));
            }
            reply(result)
        })(),
        "owners.abort" => (|| {
            let (key, epoch) = key_and_epoch(params)?;
            let record = {
                let mut guard = records();
                let owners = guard.as_mut().ok_or_else(damaged)?;
                let record = owners.abort(&key, epoch);
                save(owners)?;
                record
            };
            {
                let mut daemon = lock(shared);
                daemon.drop_held(&key, epoch);
                daemon.after_move_away(&key, epoch, false);
            }
            log(format!("gave up moving {} (epoch {epoch})", key.0));
            match record {
                Some(record) => reply(Ok(record)),
                None => Ok(Value::Null),
            }
        })(),
        _ => return None,
    };
    Some(answer)
}

fn key_and_epoch(params: &Value) -> anyhow::Result<(WorktreeKey, u64)> {
    let key: WorktreeKey = serde_json::from_value(params.get("key").cloned().ok_or_else(|| anyhow!("no key"))?)?;
    let epoch = params.get("epoch").and_then(Value::as_u64).ok_or_else(|| anyhow!("no epoch"))?;
    Ok((key, epoch))
}
