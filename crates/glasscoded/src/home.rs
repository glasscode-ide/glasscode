//! The arbiter of where the manager acts from (M58, design section 4 "Home (D5)"): the home
//! record lives here, only this server's clock ends a Mac's lease, and every change of home
//! raises the generation. Only a Glasscode machine's own connection calls these (the caller is
//! the Mac, `Caller::Machine`); a damaged record refuses every step, so no machine is home by
//! mistake.

use std::sync::{Mutex, MutexGuard};

use anyhow::anyhow;
use glasscode_core::caller::Caller;
use glasscode_core::home::{HomeFile, HomeRecord};
use glasscode_core::paths::MachineId;
use glasscode_core::store::Store;
use serde_json::{Value, json};

use crate::daemon::log;

/// The record, or None when the file is damaged (read again only in another Glasscode folder).
static HOME: Mutex<crate::owners::ByRoot<Option<HomeFile>>> = Mutex::new(crate::owners::ByRoot::new(None));

fn file() -> MutexGuard<'static, crate::owners::ByRoot<Option<HomeFile>>> {
    let mut guard = HOME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.for_root(|| match HomeFile::load() {
        Ok(file) => Some(file),
        Err(err) => {
            log(format!("the manager's home record cannot be read ({err}); no machine is home until {} is repaired", HomeFile::path().display()));
            None
        }
    });
    guard
}

fn server() -> anyhow::Result<MachineId> {
    crate::owners::machine().ok_or_else(|| anyhow!("this server has no machine id"))
}

/// Run `f` on the record (the server's at generation 0 when there is none yet), saving a change.
/// When home moved on, what an older generation claimed without a result becomes Uncertain.
fn with_record<R>(f: impl FnOnce(&mut HomeRecord, &MachineId) -> anyhow::Result<R>) -> anyhow::Result<R> {
    let server = server()?;
    let mut guard = file();
    let home = guard.as_mut().ok_or_else(|| anyhow!("the manager's home record is damaged ({}); repair it first", HomeFile::path().display()))?;
    let mut record = home.record.clone().unwrap_or_else(|| HomeRecord::server(&server));
    let before = record.clone();
    let result = f(&mut record, &server)?;
    if record != before {
        let moved_on = record.generation > before.generation;
        let generation = record.generation;
        home.record = Some(record);
        home.save().map_err(|err| anyhow!("could not save the manager's home: {err}"))?;
        if moved_on {
            match glasscode_core::effects::fence_older(generation) {
                Ok(0) => {}
                Ok(n) => log(format!("{n} effect(s) the old home claimed without a result are uncertain now; they are checked before anything tries them again")),
                Err(err) => log(format!("could not fence the old home's effects: {err}")),
            }
        }
    }
    Ok(result)
}

/// A test's Mac holds the lease (`home.claim` granted): the manager's home is that Mac.
#[cfg(test)]
pub(crate) fn mac_is_home_for_test() {
    let mac = glasscode_core::paths::MachineId::parse("00000000000000aa").unwrap();
    with_record(|record, server| {
        record.grant(&mac, server, glasscode_core::clock::now_ms());
        Ok(())
    })
    .unwrap();
}

/// The generation at which this server is the manager's home now; none when a Mac is, or the
/// record is damaged (then no machine is home).
pub fn server_generation() -> Option<u64> {
    let server = server().ok()?;
    let guard = file();
    let record = guard.as_ref()?.record.clone().unwrap_or_else(|| HomeRecord::server(&server));
    record.is_home(&server, &server, glasscode_core::clock::now_ms()).then_some(record.generation)
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

/// An observer Mac is never the manager's home (M57): the primary Mac or this server is.
fn observer_refused(mac: &MachineId) -> anyhow::Result<()> {
    if crate::mirror::is_primary(mac) {
        Ok(())
    } else {
        anyhow::bail!("This Mac watches: another Mac is this server's primary, so the manager works from that Mac or from the server")
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
            observer_refused(&mac)?;
            let generation = generation(params)?;
            let choice: Option<glasscode_core::home::HomeChoice> = params.get("choice").cloned().and_then(|c| serde_json::from_value(c).ok());
            with_record(|record, server| match record.renew(&mac, generation, server, now) {
                Ok(()) if choice.is_some_and(|c| c != record.choice) => {
                    record.choice = choice.unwrap_or_default();
                    Ok(json!({ "ok": true, "record": record }))
                }
                Ok(()) => Ok(json!({ "ok": true, "record": record })),
                Err(error) => Ok(json!({ "ok": false, "error": error, "record": record })),
            })
        })(),
        "home.claim" => (|| {
            // M57's server brain finishes its turn before this answers (at most 2 minutes); with
            // no brain running here yet, home is granted at once.
            let mac = mac(caller)?;
            observer_refused(&mac)?;
            let choice: glasscode_core::home::HomeChoice = params.get("choice").cloned().and_then(|c| serde_json::from_value(c).ok()).unwrap_or_default();
            with_record(|record, server| {
                record.choice = choice;
                let granted = record.grant(&mac, server, now).clone();
                log(format!("the manager's home is {} (generation {})", mac.as_str(), granted.generation));
                Ok(serde_json::to_value(granted)?)
            })
        })(),
        // Registered claims (M58 step 4): while a server is paired, the home claims each outward
        // or at-most-once effect here before it runs, fenced by its generation.
        "home.claim_effect" => (|| {
            let mac = mac(caller)?;
            let generation = generation(params)?;
            let intent: glasscode_core::effects::Intent = serde_json::from_value(params.get("intent").cloned().ok_or_else(|| anyhow!("no intent"))?)?;
            // The check and the claim under the record's lock: home cannot move on between them,
            // so a claim is always of the generation that is home then, and the change of home
            // that follows fences it (`fence_older`).
            with_record(|record, server| {
                if !(record.generation == generation && record.is_home(&mac, server, now)) {
                    anyhow::bail!("not the manager's home at generation {generation} (generation {} is): nothing outward runs from here", record.generation);
                }
                #[cfg(test)]
                tests::between_check_and_claim();
                let claim = glasscode_core::effects::claim_at(&intent, &mac, generation)?;
                Ok(serde_json::to_value(claim)?)
            })
        })(),
        "home.finish_effect" => (|| {
            let key: glasscode_core::effects::EffectKey = serde_json::from_value(params.get("key").cloned().ok_or_else(|| anyhow!("no key"))?)?;
            let state: glasscode_core::effects::EffectState = serde_json::from_value(params.get("state").cloned().ok_or_else(|| anyhow!("no state"))?)?;
            glasscode_core::effects::finish(&key, state)?;
            Ok(Value::Null)
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

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use glasscode_core::caller::Caller;
    use glasscode_core::effects::{self, EffectState, Intent};
    use glasscode_core::home::{HomeRecord, LEASE_MS, TAKEOVER_GRACE_MS};
    use glasscode_core::paths::MachineId;
    use serde_json::json;

    /// What runs between a claim's check and its write, once (a test's change of home).
    static BETWEEN: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>> = std::sync::Mutex::new(None);

    pub(crate) fn between_check_and_claim() {
        let hook = BETWEEN.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(hook) = hook {
            hook();
        }
    }

    struct Fixed(AtomicU64);

    impl glasscode_core::clock::Clock for Fixed {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }

        fn local_minutes(&self, at_ms: u64) -> Option<i64> {
            Some((at_ms / 60_000) as i64)
        }
    }

    /// A claim and a change of home never interleave (M58): while the Mac's claim is between its
    /// check and its write, its lease runs out and the server's tick takes home. The tick waits
    /// for the claim, so the claim is of the old generation and the change of home fences it: it
    /// never runs from the old home.
    #[test]
    fn a_claim_and_a_change_of_home_never_interleave() {
        let (_home, _scratch) = crate::daemon::tests::scratch_home();
        let start = 1_900_000_000_000;
        let clock = Arc::new(Fixed(AtomicU64::new(start)));
        let _guard = glasscode_core::clock::use_test_clock(clock.clone());
        let mac = Caller::Machine { id: MachineId::parse("00000000000000aa").unwrap() };
        let granted: HomeRecord = serde_json::from_value(super::dispatch(&mac, "home.claim", &json!({})).unwrap().unwrap()).unwrap();
        let ticker: Arc<std::sync::Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::default();
        let (later, handle) = (clock.clone(), ticker.clone());
        *BETWEEN.lock().unwrap() = Some(Box::new(move || {
            later.0.store(start + LEASE_MS + TAKEOVER_GRACE_MS + 1, Ordering::SeqCst);
            *handle.lock().unwrap() = Some(std::thread::spawn(super::tick));
            // Time for the tick to take home, if nothing holds it back.
            std::thread::sleep(std::time::Duration::from_millis(300));
        }));
        let intent = Intent::new(effects::effect_key("manager", "pr:app#7", "pr.comment", "app#7"), "pr.comment", glasscode_core::ops::Retry::AtMostOnce);
        super::dispatch(&mac, "home.claim_effect", &json!({ "intent": intent, "generation": granted.generation })).unwrap().unwrap();
        ticker.lock().unwrap().take().unwrap().join().unwrap();
        assert_eq!(super::server_generation(), Some(granted.generation + 1), "the server took home");
        assert_eq!(effects::find(&intent.key).unwrap().unwrap().state, EffectState::Uncertain, "the old home's claim is fenced");
    }
}
