//! The arbiter of where the manager acts from (M58, design section 4 "Home (D5)"): the home
//! record lives here, only this server's clock ends a Mac's lease, and every change of home
//! raises the generation. Only a Glasscode machine's own connection calls these (the caller is
//! the Mac, `Caller::Machine`); a damaged record refuses every step, so no machine is home by
//! mistake.

use std::sync::{Mutex, MutexGuard, OnceLock};

use anyhow::anyhow;
use glasscode_core::caller::Caller;
use glasscode_core::home::{HomeFile, HomeRecord};
use glasscode_core::paths::MachineId;
use glasscode_core::store::Store;
use serde_json::{Value, json};

use crate::daemon::log;

/// The record, or None when the file is damaged.
static HOME: OnceLock<Mutex<Option<HomeFile>>> = OnceLock::new();

fn file() -> MutexGuard<'static, Option<HomeFile>> {
    HOME.get_or_init(|| {
        Mutex::new(match HomeFile::load() {
            Ok(file) => Some(file),
            Err(err) => {
                log(format!("the manager's home record cannot be read ({err}); no machine is home until {} is repaired", HomeFile::path().display()));
                None
            }
        })
    })
    .lock()
    .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn server() -> anyhow::Result<MachineId> {
    crate::owners::machine().ok_or_else(|| anyhow!("this server has no machine id"))
}

/// Run `f` on the record (the server's at generation 0 when there is none yet), saving a change.
fn with_record<R>(f: impl FnOnce(&mut HomeRecord, &MachineId) -> anyhow::Result<R>) -> anyhow::Result<R> {
    let server = server()?;
    let mut guard = file();
    let home = guard.as_mut().ok_or_else(|| anyhow!("the manager's home record is damaged ({}); repair it first", HomeFile::path().display()))?;
    let mut record = home.record.clone().unwrap_or_else(|| HomeRecord::server(&server));
    let before = record.clone();
    let result = f(&mut record, &server)?;
    if record != before {
        home.record = Some(record);
        home.save().map_err(|err| anyhow!("could not save the manager's home: {err}"))?;
    }
    Ok(result)
}

/// The server's tick: a Mac's lease ran out (and the grace after it), so the server is home.
pub fn tick() {
    let now = glasscode_core::clock::now_ms();
    let taken = with_record(|record, server| Ok(record.expire(server, now, false).then(|| record.generation)));
    if let Ok(Some(generation)) = taken {
        log(format!("the Mac's lease ran out: the manager's home is this server (generation {generation})"));
    }
}

fn mac(caller: &Caller) -> anyhow::Result<MachineId> {
    match caller {
        Caller::Machine { id } => Ok(id.clone()),
        _ => anyhow::bail!("only a Glasscode machine's own connection asks for the manager's home"),
    }
}

fn generation(params: &Value) -> anyhow::Result<u64> {
    params.get("generation").and_then(Value::as_u64).ok_or_else(|| anyhow!("no generation"))
}

pub fn dispatch(caller: &Caller, method: &str, params: &Value) -> Option<anyhow::Result<Value>> {
    let now = glasscode_core::clock::now_ms();
    let answer = match method {
        "home.get" => with_record(|record, _| Ok(serde_json::to_value(&*record)?)),
        "home.renew" => (|| {
            let mac = mac(caller)?;
            let generation = generation(params)?;
            with_record(|record, server| match record.renew(&mac, generation, server, now) {
                Ok(()) => Ok(json!({ "ok": true, "record": record })),
                Err(error) => Ok(json!({ "ok": false, "error": error, "record": record })),
            })
        })(),
        "home.claim" => (|| {
            // M57's server brain finishes its turn before this answers (at most 2 minutes); with
            // no brain running here yet, home is granted at once.
            let mac = mac(caller)?;
            with_record(|record, server| {
                let granted = record.grant(&mac, server, now).clone();
                log(format!("the manager's home is {} (generation {})", mac.as_str(), granted.generation));
                Ok(serde_json::to_value(granted)?)
            })
        })(),
        "home.release" => (|| {
            let mac = mac(caller)?;
            let generation = generation(params)?;
            with_record(|record, server| match record.release(&mac, generation, server) {
                Ok(after) => {
                    log(format!("the Mac handed the manager's home to this server (generation {})", after.generation));
                    Ok(json!({ "ok": true, "record": after }))
                }
                Err(error) => Ok(json!({ "ok": false, "error": error, "record": record })),
            })
        })(),
        _ => return None,
    };
    Some(answer)
}
