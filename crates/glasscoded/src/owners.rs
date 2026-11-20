//! The owner record of each worktree that has moved (`glasscode_core::owners`). This daemon is
//! the records' only writer, and the server's clock is the only one that ends a stale move. The
//! switch on the Mac drives a move through `owners.begin`, then `worktrees.receive` and
//! `sessions.create` with the move's tag (the sessions wait, held), then `owners.commit`, which
//! starts them, or `owners.abort`, which removes them.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use anyhow::anyhow;
use glasscode_core::owners::{OwnerRecord, Owners, WorktreeKey};
use glasscode_core::paths::MachineId;
use glasscode_core::remote::{OwnersBegin, OwnersReply};
use glasscode_core::store::Store;
use serde_json::Value;

use crate::daemon::{Shared, lock, log};

static OWNERS: OnceLock<Mutex<Owners>> = OnceLock::new();
static MACHINE: OnceLock<Option<MachineId>> = OnceLock::new();

/// This server's machine id (`authority/machine.json` here), if it could be read or made.
pub fn machine() -> Option<MachineId> {
    MACHINE
        .get_or_init(|| match MachineId::this() {
            Ok(id) => Some(id),
            Err(err) => {
                log(format!(
                    "no machine id, so worktrees cannot move by owner record: {err}"
                ));
                None
            }
        })
        .clone()
}

fn records() -> MutexGuard<'static, Owners> {
    OWNERS
        .get_or_init(|| {
            Mutex::new(Owners::load().unwrap_or_else(|err| {
                log(format!("could not read the worktree owners: {err}"));
                Owners::default()
            }))
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn save(owners: &Owners) {
    if let Err(err) = owners.save() {
        log(format!("could not save the worktree owners: {err}"));
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Whether this server may run agents in `key`'s worktree now: it owns it (a worktree here with
/// no record is its own) and no move of it is under way.
pub fn may_run_here(key: &WorktreeKey) -> bool {
    let Some(me) = machine() else { return true };
    records().may_run(key, &me, &me)
}

/// Who owns `key` when it is not this server, for messages.
pub fn owner_elsewhere(key: &WorktreeKey) -> Option<OwnerRecord> {
    let me = machine()?;
    let record = records().get(key, &me);
    (record.owner != me || record.moving.is_some()).then_some(record)
}

/// A step of the move of `key` to `epoch` arrived: it must be a move to this server, and it is
/// progress, so the move does not go stale.
pub fn moving_here(key: &WorktreeKey, epoch: u64) -> anyhow::Result<()> {
    let me = machine().ok_or_else(|| anyhow!("this server has no machine id"))?;
    let mut owners = records();
    if owners.moving_to(key, epoch) != Some(&me) {
        anyhow::bail!(
            "this move is no longer under way (it was given up, or another one replaced it)"
        );
    }
    owners
        .touch(key, epoch, now_ms())
        .map_err(|err| anyhow!("{err}"))?;
    Ok(())
}

/// The records that exist among `keys`.
pub fn get(keys: &[WorktreeKey]) -> HashMap<WorktreeKey, OwnerRecord> {
    let owners = records();
    keys.iter()
        .filter_map(|key| Some((key.clone(), owners.records.get(key)?.clone())))
        .collect()
}

/// Moves that made no progress for too long: given up (with the epoch each would have had).
pub fn expire() -> Vec<(WorktreeKey, u64)> {
    let mut owners = records();
    let gone = owners.expire(now_ms());
    if !gone.is_empty() {
        save(&owners);
    }
    gone
}

/// This server continued a Mac's worktree on its own (`mirror.take_over`): it owns it now, at a
/// new epoch, so the Mac stops its copy when it next syncs.
pub fn taken_here(key: &WorktreeKey, mac: &MachineId) -> Option<OwnerRecord> {
    let me = machine()?;
    let mut owners = records();
    let record = owners.take(key, &me, mac);
    save(&owners);
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
            let keys: Vec<WorktreeKey> = serde_json::from_value(
                params
                    .get("keys")
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new())),
            )?;
            Ok(serde_json::to_value(get(&keys))?)
        })(),
        "owners.begin" => (|| {
            let begin: OwnersBegin = serde_json::from_value(params.clone())?;
            let me = machine().ok_or_else(|| anyhow!("this server has no machine id"))?;
            // Without a record, a worktree belongs to the machine it leaves.
            let holder = if begin.to == me {
                begin.from.clone()
            } else {
                me.clone()
            };
            let mut owners = records();
            let result = owners.begin(
                &begin.key,
                &begin.from,
                &begin.to,
                begin.epoch,
                &holder,
                now_ms(),
            );
            if result.is_ok() {
                save(&owners);
            }
            drop(owners);
            if let Ok(record) = &result {
                if begin.to != me {
                    // Leaving: its agents stop, and nothing may start here while it moves.
                    let epoch = record.moving.as_ref().map_or(record.epoch + 1, |m| m.epoch);
                    lock(shared).stop_for_move(&begin.key, epoch);
                }
                log(format!(
                    "moving {} to {} (epoch {})",
                    begin.key.0,
                    begin.to.as_str(),
                    record.moving.as_ref().map_or(record.epoch, |m| m.epoch)
                ));
            }
            reply(result)
        })(),
        "owners.commit" => (|| {
            let (key, epoch) = key_and_epoch(params)?;
            let me = machine().ok_or_else(|| anyhow!("this server has no machine id"))?;
            let mut owners = records();
            let result = owners.commit(&key, epoch);
            if result.is_ok() {
                save(&owners);
            }
            drop(owners);
            if let Ok(record) = &result {
                let mut daemon = lock(shared);
                if record.owner == me {
                    daemon.start_held(&key, epoch);
                } else {
                    daemon.after_move_away(&key, epoch, true);
                }
                log(format!(
                    "{} now runs on {} (epoch {epoch})",
                    key.0,
                    if record.owner == me {
                        "this server"
                    } else {
                        record.owner.as_str()
                    }
                ));
            }
            reply(result)
        })(),
        "owners.abort" => (|| {
            let (key, epoch) = key_and_epoch(params)?;
            let mut owners = records();
            let record = owners.abort(&key, epoch);
            save(&owners);
            drop(owners);
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
    let key: WorktreeKey = serde_json::from_value(
        params
            .get("key")
            .cloned()
            .ok_or_else(|| anyhow!("no key"))?,
    )?;
    let epoch = params
        .get("epoch")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("no epoch"))?;
    Ok((key, epoch))
}
