//! The rest of v1's `data.json`, moved into the v2 stores once (M44; review exec-6). The notes
//! went with memory (M20.3); this is the rest:
//!
//! | v1 (`manager/data.json`) | v2 |
//! |---|---|
//! | `followups` | work items in `manager/work.json` (`WorkItem::from_follow_up`, same PR keys), and the built-in `follow-ups` entry |
//! | `outbox` | `manager/outbox.json`, same ids and states: a sent message stays sent, and its send is a settled effect |
//! | `checks` | schedules in `authority/schedules.json` (`v1-check-<id>`), proposed by the v1 manager: off until the person approves them |
//! | `chat` | the conversation (M43), as "v1, origin unknown" lines that back nothing |
//! | `seen` | settled effects (one per key, [`seen_key`]), and the built-ins' state (`manager/builtins.json`): nothing is said twice after the switch, and a review not yet told is not lost |
//! | `inbox`, `brief`, `heartbeat` | not moved: the inbox is read again from the messenger, the brief is made again, the heartbeat is v1's |
//!
//! Rules:
//! - **Kept in step until v2's manager takes over.** v1's manager still reads and writes
//!   `data.json` in a v2 home, so the app runs this at each start: a run whose `data.json` is the
//!   one the last run read (its hash in `manager/migrated.json`) does nothing, and one after v1
//!   changed it takes the changes by v1's own rule (`store::merge`): the newer version of an outbox
//!   message wins and a sent one stays sent, a follow-up's newer state wins in its work item, new
//!   lines join the conversation and new keys are settled. Every step is idempotent.
//! - **An unreadable input stops it**, without the marker: `data.json` that does not parse, or the
//!   projects' store.
//! - **A backup first.** `data.json` is copied to `data.v1.json` before anything else (never over
//!   an older copy), and `data.json` itself is left as it is: v1's manager still reads it until
//!   v2's takes over, and going back to an older build loses nothing.
//! - **The home only**, and only in a v2 home.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::effects::{self, EffectKey, EffectState, Intent};
use crate::identity::{Aliases, Hlc};
use crate::paths::{MachineId, ProjectKey};
use crate::playbooks::{self, Action, Autonomy, Playbook, ScheduleSpec, Slot};
use crate::schedule::{Misfire, Place, Schedule};
use crate::store::{OnDamage, Store};
use crate::work::{WorkItem, WorkLedger};

use super::{ChatLine, Data, MsgState, OutMessage, Settings};

/// The version of this migration: a newer one may move more.
pub const VERSION: u32 = 1;

/// Who proposed what came from v1 (its checks).
pub const BY: &str = "the v1 manager";

/// What a run moved, and what it could not.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    pub work: usize,
    pub outbox: usize,
    pub schedules: usize,
    pub chat: usize,
    pub settled: usize,
    pub builtins: usize,
    pub skipped: Vec<String>,
}

/// `manager/migrated.json`: that the migration ran, its version, the hash of the `data.json` it
/// read, and what it moved. Damaged, it reads as not run: running again is harmless.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Migrated {
    pub version: u32,
    pub at_ms: u64,
    pub machine: Option<MachineId>,
    /// 24 hex digits of the SHA-256 of the `data.json` the run read.
    pub source: String,
    pub report: Report,
}

impl Store for Migrated {
    const NAME: &'static str = "manager/migrated";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Empty;

    fn path() -> std::path::PathBuf {
        super::store::dir().join("migrated.json")
    }
}

/// The effect key of a v1 `seen` key: what v2 asks before saying it again.
pub fn seen_key(v1_key: &str) -> EffectKey {
    effect_key_of(effects::V1_SEEN, v1_key)
}

fn effect_key_of(origin: &str, subject: &str) -> EffectKey {
    effects::effect_key(origin, subject, "", "")
}

/// What v1 said or did by a `seen` key, as an op (for the record).
fn op_of(v1_key: &str) -> &'static str {
    match v1_key.split('|').next().unwrap_or_default() {
        "tell" => "session.send",
        "needs" | "done" => "app.notify",
        "send" => "message.send",
        _ => "manager.note",
    }
}

/// Everything a run writes, worked out from v1's data without IO.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub work: Vec<WorkItem>,
    pub outbox: Vec<OutMessage>,
    pub schedules: Vec<Schedule>,
    pub chat: Vec<ChatLine>,
    pub settled: Vec<(Intent, EffectState)>,
    pub builtins: Vec<Playbook>,
    pub native: playbooks::native::Native,
    pub skipped: Vec<String>,
}

/// The keys of `Data::seen` that are the follow-ups' (`followups`' module doc).
const FOLLOW_KEYS: [&str; 5] = ["pr|", "prr|", "review|", "ci|", "fixed|"];

/// v1's follow-ups and their keys from `data` (M56.2): what the work ledger takes in
/// (`WorkLedger::take_follow_ups`), so v2's follow-ups go on where v1's were.
pub fn follow_ups_of(data: &Data) -> (Vec<super::FollowUp>, BTreeMap<String, String>) {
    let seen = data.seen.iter().filter(|(key, _)| FOLLOW_KEYS.iter().any(|p| key.starts_with(p))).map(|(k, v)| (k.clone(), v.clone())).collect();
    (data.followups.clone(), seen)
}

/// [`follow_ups_of`] the home's `data.json`, read only (none when there is none). It reads the
/// disk: off the main thread.
pub fn v1_follow_ups() -> Result<(Vec<super::FollowUp>, BTreeMap<String, String>), String> {
    let file = super::store::file();
    if !file.exists() {
        return Ok((Vec::new(), BTreeMap::new()));
    }
    let text = std::fs::read_to_string(&file).map_err(|err| format!("{} could not be read ({err})", file.display()))?;
    let data: Data = serde_json::from_str(&text).map_err(|err| format!("{} does not parse ({err})", file.display()))?;
    Ok(follow_ups_of(&data))
}

/// A schedule id from a v1 check's id: `v1-check-` and its safe letters./// A schedule id from a v1 check's id: `v1-check-` and its safe letters.
pub fn check_id(id: &str) -> String {
    let safe: String = id.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let safe = safe.trim_matches('-');
    format!("v1-check-{}", if safe.is_empty() { "x" } else { safe }).chars().take(64).collect::<String>().trim_end_matches('-').to_string()
}

/// The plan of a run (pure): v1's `data` and manager `settings`, the projects this machine knows
/// (for the follow-ups' keys), this machine, and the time.
pub fn plan(data: &Data, settings: &Settings, known: &[(ProjectKey, Aliases)], machine: &MachineId, now_ms: u64) -> Plan {
    let mut plan = Plan::default();
    // Follow-ups: the teammates' pull requests, waiting on them.
    for follow in &data.followups {
        let Some(remote) = crate::github::normalize_remote(&format!("https://github.com/{}", follow.repo.trim())) else {
            plan.skipped.push(format!("follow-up {} names no repository", follow.id));
            continue;
        };
        let project = crate::identity::key_for_remote(&remote, known);
        let at = Hlc { wall_ms: follow.updated.saturating_mul(1000), count: 0, machine: machine.clone() };
        plan.work.push(WorkItem::from_follow_up(follow, &project, at));
    }
    // The outbox, as it is; a sent message's send is a settled effect.
    plan.outbox = data.outbox.clone();
    for message in data.outbox.iter().filter(|m| m.state == MsgState::Sent) {
        plan.settled.push((super::messages::send_intent(message, ""), EffectState::Done(serde_json::json!({ "sent": message.sent }))));
    }
    // Checks: schedules the person approves first.
    for check in &data.checks {
        let spec = if check.once {
            ScheduleSpec::Once { at_ms: check.last.saturating_add(check.every_secs).saturating_mul(1000).max(now_ms) }
        } else {
            ScheduleSpec::Every { secs: check.every_secs.max(crate::schedule::MIN_PERIOD_SECS) }
        };
        let mut args = BTreeMap::new();
        args.insert("question".to_string(), vec![Slot::Text(check.what.clone())]);
        plan.schedules.push(Schedule {
            id: check_id(&check.id),
            name: check.what.chars().take(80).collect(),
            spec,
            then: Action { op: "manager.ask".into(), args },
            autonomy: Autonomy::Ask,
            misfire: Misfire::Once,
            expires_ms: None,
            place: Place::Home,
            enabled: false,
            proposed_by: Some(BY.to_string()),
            made_ms: if check.last > 0 { check.last * 1000 } else { now_ms },
        });
    }
    plan.chat = data.chat.clone();
    // `seen`: each key a settled effect, and the built-ins' state.
    for (key, value) in &data.seen {
        let session = key.split('|').nth(1).unwrap_or_default().to_string();
        let intent = Intent {
            origin: effects::V1_SEEN.into(),
            subject: key.clone(),
            target: session.clone(),
            caller: BY.into(),
            reason: format!("v1 had it as {value}"),
            session: (!session.is_empty()).then_some(session),
            ..Intent::new(seen_key(key), op_of(key), crate::ops::Retry::Verifiable)
        };
        plan.settled.push((intent, EffectState::Done(serde_json::json!({ "v1": value }))));
    }
    plan.native = playbooks::native::Native::from_v1_seen(&data.seen);
    // The manager's built-ins, switched as v1's manager was.
    for builtin in playbooks::builtins::BUILTINS.iter().filter(|b| b.needs_manager) {
        plan.builtins.push(Playbook { enabled: settings.enabled, ..playbooks::builtins::entry(builtin) });
    }
    plan
}

/// Run the migration once, on the home, in a v2 home: a backup of `data.json` first, then each
/// store, each step idempotent, and the marker last. Gives what it moved (or what the run that
/// already did moved). It reads and writes the disk: never on the main thread.
pub fn run(home: bool) -> Result<Report, String> {
    if !home {
        return Err("only the manager's home migrates v1's data".to_string());
    }
    if crate::store::EngineSwitch::current().engine != crate::store::Engine::V2 {
        return Err("v1's data moves only in a v2 home".to_string());
    }
    let data_file = super::store::file();
    // v1's file as it is: one that does not parse stops the migration (no marker).
    let bytes = match std::fs::read(&data_file) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => b"{}".to_vec(),
        Err(err) => return Err(format!("{} could not be read ({err}); nothing was moved", data_file.display())),
    };
    let data: Data = serde_json::from_slice(&bytes).map_err(|err| format!("{} does not read ({err}); nothing was moved, and it runs again once the file is repaired", data_file.display()))?;
    let source = effects::short_hash(&String::from_utf8_lossy(&bytes));
    let marker = Migrated::load().unwrap_or_default();
    if marker.version >= VERSION && marker.source == source {
        return Ok(marker.report);
    }
    let backup = data_file.with_file_name("data.v1.json");
    if data_file.exists() && !backup.exists() {
        std::fs::copy(&data_file, &backup).map_err(|err| format!("the backup data.v1.json could not be written ({err}); nothing was moved"))?;
    }
    let settings = crate::store::load().manager;
    let machine = MachineId::this().map_err(|err| err.to_string())?;
    let known = crate::identity::Projects::load().map(|projects| projects.known(&machine)).map_err(|err| format!("the projects' keys could not be read ({err}); nothing was moved"))?;
    let now = crate::clock::now_ms();
    let plan = plan(&data, &settings, &known, &machine, now);
    let mut report = Report { skipped: plan.skipped.clone(), ..Report::default() };
    // Work items: new ones added, and a follow-up's newer state taken (by its stamp).
    let work = plan.work.clone();
    report.work = crate::store::update::<WorkLedger, _>(|ledger| {
        let mut changed = 0;
        for item in work {
            if !ledger.items.iter().any(|have| have.key == item.key) {
                ledger.items.push(item);
                changed += 1;
            } else {
                changed += ledger.merge_theirs(std::slice::from_ref(&item)).len();
            }
        }
        changed
    })
    .map_err(|err| err.to_string())?;
    // The outbox by v1's rule: by id, the newer version wins, and a sent message stays sent.
    let outbox = plan.outbox.clone();
    report.outbox = crate::store::update::<super::messages::Outbox, _>(|file| {
        let mut changed = 0;
        for message in outbox {
            match file.messages.iter_mut().find(|have| have.id == message.id) {
                None => {
                    file.messages.push(message);
                    changed += 1;
                }
                Some(have) if have.state != MsgState::Sent && (message.state == MsgState::Sent || message.updated > have.updated) && *have != message => {
                    *have = message;
                    changed += 1;
                }
                Some(_) => {}
            }
        }
        changed
    })
    .map_err(|err| err.to_string())?;
    // Checks: proposed, so off until approved.
    let have = crate::schedule::ScheduleFile::load().map_err(|err| err.to_string())?;
    for schedule in plan.schedules {
        if have.schedules.iter().any(|s| s.id == schedule.id) {
            continue;
        }
        match crate::schedule::propose(schedule.clone(), Some(BY)) {
            Ok(()) => report.schedules += 1,
            Err(err) => report.skipped.push(format!("check “{}”: {err}", schedule.name)),
        }
    }
    // The conversation.
    let mut conversation = crate::conversation::Conversation::load();
    report.chat = conversation.import_v1(&plan.chat, &machine).map_err(|err| err.to_string())?;
    // `seen` and the sends: settled effects, in one pass over the ledger.
    report.settled = effects::record_settled_many(&plan.settled, &machine).map_err(|err| err.to_string())?;
    // The built-ins' entries, and their state.
    let builtins = plan.builtins.clone();
    report.builtins = crate::store::update::<playbooks::PlaybookFile, _>(|file| {
        let mut added = 0;
        for entry in builtins {
            if !file.playbooks.iter().any(|p| p.id == entry.id) {
                file.playbooks.push(entry);
                added += 1;
            }
        }
        added
    })
    .map_err(|err| err.to_string())?;
    let native = plan.native.clone();
    crate::store::update::<playbooks::native::NativeFile, _>(|file| {
        if file.native == playbooks::native::Native::default() {
            file.native = native;
        }
    })
    .map_err(|err| err.to_string())?;
    Migrated { version: VERSION, at_ms: now, machine: Some(machine), source, report: report.clone() }.save().map_err(|err| err.to_string())?;
    Ok(report)
}

/// The built-ins' state from v1's `seen` in `data.json` as it is now, for a take-over after the move
/// failed (`lifetime::Migration`): nothing v1 said is said again, and a review v1 had not told yet
/// is not lost. None when `data.json` is not there or does not read (then they start afresh).
pub fn natives_from_v1() -> Option<playbooks::native::Native> {
    let bytes = std::fs::read(super::store::file()).ok()?;
    let data: Data = serde_json::from_slice(&bytes).ok()?;
    Some(playbooks::native::Native::from_v1_seen(&data.seen))
}

// ----- the switch to v2 (M56 step 1) ----------------------------------------------------------

/// The v2 features a home gets with the switch: each was built behind its own switch (rule 2).
/// `profiles` (M22) is not among them: it changes how every session starts and has not run in a
/// live session yet, so it waits for the owner's live run (a switch in Settings › Manager).
pub const V2_FEATURES: [&str; 2] = ["brief", "turn_hooks"];

/// What the start did with the engine switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtStart {
    /// Nothing to do: v2 already, or a switch file that does not read (it fails closed: v1).
    Keep,
    /// v2 now: every home at v1 (M56 removal: v1's manager goes, so a home whose manager is on
    /// switches too, before its first round).
    Switched,
}

/// What the start does with the switch (`switch` as it reads, None when it does not): v2 for
/// every home still at v1.
pub fn decide_at_start(switch: Option<&crate::store::EngineSwitch>) -> AtStart {
    match switch {
        None => AtStart::Keep,
        Some(switch) if switch.engine == crate::store::Engine::V2 => AtStart::Keep,
        Some(_) => AtStart::Switched,
    }
}

/// `authority/engine.json` at v2, with [`V2_FEATURES`] on (one switched off before stays off).
pub fn write_switch() -> Result<(), String> {
    crate::store::update::<crate::store::EngineSwitch, _>(|switch| {
        switch.engine = crate::store::Engine::V2;
        for feature in V2_FEATURES {
            switch.switches.entry(feature.to_string()).or_insert(true);
        }
    })
    .map_err(|err| format!("the engine switch could not be written ({err})"))
}

/// The start's switch (M56), before anything of this run reads the engine: every home is on v2
/// from now on, a new install as one whose v1 manager ran (nothing of v1's runs before this). v1's
/// data moves into the v2 stores at this start's first ticks, as in any v2 home ([`run`], M20.3's
/// notes), because those stores take writes only in a v2 home. A switch file that does not read
/// stays as it is.
pub fn switch_at_start() -> AtStart {
    let switch = match crate::store::open::<crate::store::EngineSwitch>() {
        Ok(crate::store::Opened { value, found: crate::store::Found::File | crate::store::Found::Missing }) => Some(value),
        _ => None,
    };
    let at = decide_at_start(switch.as_ref());
    if at == AtStart::Switched
        && let Err(why) = write_switch()
    {
        log::warn!("manager: {why}; this run stays on v1");
        return AtStart::Keep;
    }
    at
}

/// What "Switch now" did: v2 is on; v1's notes and data moved, or why not yet (they move again
/// at each start until they have).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switched {
    pub notes: Result<crate::memstore::migrate::Migrated, String>,
    pub data: Result<Report, String>,
}

/// "Switch now" in Settings › Manager (M56 step 1), for a home whose v1 manager runs: v1's notes
/// into memory (M20.3) and the rest of `data.json` into the v2 stores (M44), and the switch, all or
/// nothing. The switch is written first, because those stores take writes only in a v2 home; when
/// a move fails, the switch goes back to what it was and nothing switches. On a Mac that is not the
/// manager's home (a server is), only the switch is written: v1's data moves when this Mac is home
/// again (the start's ticks). v2 runs from the next start. It writes the disk and memory's git:
/// never on the main thread.
pub fn switch_now(home: bool) -> Result<Switched, String> {
    let before = match crate::store::open::<crate::store::EngineSwitch>() {
        Ok(crate::store::Opened { value, found: crate::store::Found::File | crate::store::Found::Missing }) => value,
        _ => return Err("the engine switch does not read; it stays as it is".into()),
    };
    write_switch()?;
    if !home {
        return Ok(Switched { notes: Err("this Mac is not the manager's home: v1's notes move when it is".into()), data: Err("this Mac is not the manager's home: v1's data moves when it is".into()) });
    }
    let back = |why: String| {
        use crate::store::Store as _;
        match before.save() {
            Ok(()) => format!("{why}; nothing switched"),
            Err(err) => format!("{why}; and the switch could not be put back ({err})"),
        }
    };
    let notes = crate::memstore::migrate::run(&crate::memstore::Memory::open()).map_err(|err| back(format!("v1's notes did not move into memory ({err})")))?;
    let data = run(true).map_err(back)?;
    Ok(Switched { notes: Ok(notes), data: Ok(data) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::{Check, FollowKind, FollowState, FollowUp, Tier, Who};
    use crate::testkit::clock::{VirtualClock, Zone, utc_ms};

    /// A v1 data.json as a v1 round left it: a follow-up, two messages (one sent), two checks, a
    /// conversation and its `seen` keys.
    fn recorded() -> Data {
        let mut data = Data::default();
        data.followups.push(FollowUp {
            id: "fu-1".into(),
            kind: FollowKind::BlockersGiven,
            state: FollowState::Open,
            repo: "acme/app".into(),
            number: 1111,
            title: "Token refresh".into(),
            url: "https://github.com/acme/app/pull/1111".into(),
            login: "sam-dev".into(),
            person: "Sam".into(),
            created: 1_763_000_000,
            updated: 1_763_100_000,
            ..FollowUp::default()
        });
        data.outbox.push(OutMessage { id: "m-sent".into(), to: "Sam".into(), body: "The fix for #1111 is pushed".into(), tier: Tier::Routine, state: MsgState::Sent, created: 1_763_100_000, updated: 1_763_100_010, sent: Some(1_763_100_020), ..OutMessage::default() });
        data.outbox.push(OutMessage { id: "m-draft".into(), to: "Sam".into(), body: "I will have it done by Friday".into(), tier: Tier::NeedsOk, state: MsgState::Draft, created: 1_763_100_100, updated: 1_763_100_100, ..OutMessage::default() });
        data.checks.push(Check { id: "c1".into(), what: "Is the release branch green?".into(), every_secs: 3600, last: 1_763_100_000, once: false, ..Check::default() });
        data.checks.push(Check { id: "C 2!".into(), what: "Remind me about the demo".into(), every_secs: 600, last: 1_763_100_000, once: true, ..Check::default() });
        data.chat.push(ChatLine { who: Who::You, text: "what is new?".into(), at: 1_763_500_000, show: None });
        data.chat.push(ChatLine { who: Who::Manager, text: "Two reviews came in.".into(), at: 1_763_500_001, show: None });
        data.seen.insert("needs|s1".into(), "1".into());
        data.seen.insert("tell|s2|acme/app#12|alex".into(), "pending:CHANGES_REQUESTED".into());
        data.seen.insert("tell|s2|acme/app#11|alex".into(), "told:COMMENTED".into());
        data.seen.insert("send|m-sent".into(), "1763100010@1763100020".into());
        data
    }

    struct Home {
        _home: crate::testkit::Home,
        _clock: std::sync::Arc<VirtualClock>,
        _guard: crate::clock::TestClockGuard,
    }

    fn v2_home() -> Home {
        let home = crate::testkit::Home::with(&crate::testkit::HomeSpec::bare());
        // SAFETY: the core tests run one at a time (`--test-threads=1`).
        unsafe { std::env::set_var(crate::store::ENGINE_VARIABLE, "v2") };
        let clock = VirtualClock::new(utc_ms(2025, 11, 20, 9, 0), Zone::utc());
        let guard = clock.install();
        Home { _home: home, _clock: clock, _guard: guard }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            // SAFETY: as in `v2_home`.
            unsafe { std::env::remove_var(crate::store::ENGINE_VARIABLE) };
        }
    }

    /// M56: the decision at the start. Every home at v1 goes to v2, also one whose manager runs;
    /// v2 stays; a switch that does not read stays as it is (it fails closed).
    /// M56.2: v1's follow-ups and only their keys come over to the work ledger; again, nothing
    /// changes; v1's newer state wins, the ledger's newer one stays.
    #[test]
    fn follow_ups_come_over_to_the_ledger_once() {
        let mut data = Data::default();
        data.followups.push(FollowUp { id: "fu-acme/app#7".into(), repo: "acme/app".into(), number: 7, updated: 10, ..FollowUp::default() });
        data.seen.insert("fixed|fu-acme/app#7".into(), "abc".into());
        data.seen.insert("send|m1".into(), "1".into());
        let (followups, seen) = follow_ups_of(&data);
        assert_eq!(seen.keys().collect::<Vec<_>>(), ["fixed|fu-acme/app#7"], "only the follow-ups' keys");
        let mut ledger = WorkLedger::default();
        assert!(ledger.take_follow_ups(followups.clone(), seen.clone()));
        assert!(!ledger.take_follow_ups(followups.clone(), seen.clone()), "again, nothing");
        ledger.follow_ups[0].updated = 20;
        ledger.follow_ups[0].note = "the ledger's".into();
        assert!(!ledger.take_follow_ups(followups, seen), "the ledger's newer state stays");
        data.followups[0].updated = 30;
        data.followups[0].note = "v1's".into();
        let (newer, seen) = follow_ups_of(&data);
        assert!(ledger.take_follow_ups(newer, seen));
        assert_eq!(ledger.follow_ups[0].note, "v1's");
    }

    #[test]
    fn the_start_switches_every_home() {
        use crate::store::{Engine, EngineSwitch};
        let v1 = EngineSwitch::default();
        let v2 = EngineSwitch { engine: Engine::V2, ..EngineSwitch::default() };
        assert_eq!(decide_at_start(Some(&v1)), AtStart::Switched);
        assert_eq!(decide_at_start(Some(&v2)), AtStart::Keep);
        assert_eq!(decide_at_start(None), AtStart::Keep);
    }

    /// M56 step 1: a v1 home whose manager runs switches with "Switch now". The engine is v2 with
    /// its features on (one switched off stays off); v1's data is in the v2 stores; its notes are in
    /// memory; `data.v1.json` is kept; nothing is said twice; its checks wait for approval.
    #[test]
    fn v1_state_switches_cleanly() {
        use crate::store::{Engine, EngineSwitch, Store as _};
        let _home = crate::testkit::Home::with(&crate::testkit::HomeSpec::bare());
        let clock = VirtualClock::new(utc_ms(2025, 11, 20, 9, 0), Zone::utc());
        let _guard = clock.install();
        // A v1 home: its manager on, its data and a note of its own, and a feature switched off.
        let mut state = crate::store::load();
        state.manager.enabled = true;
        crate::store::save(&state).unwrap();
        let mut data = recorded();
        data.notes.push(crate::manager::Note { id: "n1".into(), text: "The release train leaves on Thursdays.".into(), source: "you".into(), created: 1_763_000_000, updated: 1_763_000_000, ..Default::default() });
        super::super::store::save(&data).unwrap();
        EngineSwitch { engine: Engine::V1, switches: BTreeMap::from([("profiles".to_string(), false)]) }.save().unwrap();
        // All or nothing: a data.json that does not read switches nothing.
        let good = std::fs::read(super::super::store::file()).unwrap();
        std::fs::write(super::super::store::file(), "{ not json").unwrap();
        assert!(switch_now(true).unwrap_err().ends_with("nothing switched"));
        assert_eq!(EngineSwitch::current().engine, Engine::V1);
        assert!(!EngineSwitch::current().switches.get("profiles").copied().unwrap_or(true), "the switch is what it was");
        std::fs::write(super::super::store::file(), good).unwrap();
        let switched = switch_now(true).unwrap();
        let switch = EngineSwitch::current();
        assert_eq!(switch.engine, Engine::V2);
        assert_eq!((switch.is_on("brief"), switch.is_on("turn_hooks"), switch.is_on("profiles")), (true, true, false), "{switch:?}");
        let report = switched.data.unwrap();
        assert_eq!((report.work, report.outbox, report.schedules), (1, 2, 2), "{report:?}");
        let notes = switched.notes.unwrap();
        assert_eq!(notes.learned + notes.proposed, 1, "v1's note is in memory: {notes:?}");
        assert!(super::super::store::file().with_file_name("data.v1.json").exists());
        let schedules = crate::schedule::ScheduleFile::load().unwrap();
        assert_eq!(schedules.schedules.iter().filter(|s| s.id.starts_with("v1-check-")).count(), 2);
        // The told reviews are settled: nothing is said twice.
        assert!(effects::find(&seen_key("tell|s2|acme/app#11|alex")).unwrap().is_some());
        // The next start keeps v2.
        assert_eq!(switch_at_start(), AtStart::Keep);
        // A new home switches at its first start (here: the switch file goes, as in a new home).
        for file in std::fs::read_dir(crate::paths::authority_dir()).unwrap().flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("engine.json"))) {
            std::fs::remove_file(file).unwrap();
        }
        assert_eq!(EngineSwitch::current().engine, Engine::V1);
        assert_eq!(switch_at_start(), AtStart::Switched);
        let fresh = EngineSwitch::current();
        assert_eq!((fresh.engine, fresh.is_on("brief"), fresh.is_on("turn_hooks")), (Engine::V2, true, true));
        assert!(!fresh.is_on("profiles"), "profiles waits for the owner's live run");
    }

    /// M56: a take-over after the move failed seeds the built-ins from `data.json`'s `seen` as the
    /// move would have; with no `data.json`, or one that does not read, they start afresh.
    #[test]
    fn natives_come_from_v1_seen_when_the_move_failed() {
        let _home = v2_home();
        assert!(natives_from_v1().is_none(), "no data.json");
        super::super::store::save(&recorded()).unwrap();
        assert_eq!(natives_from_v1().unwrap().pending(), playbooks::native::Native::from_v1_seen(&recorded().seen).pending());
        assert_eq!(natives_from_v1().unwrap().pending(), 1, "the review v1 had not told yet");
        std::fs::write(super::super::store::file(), "{ not json").unwrap();
        assert!(natives_from_v1().is_none());
    }

    #[test]
    fn migration_round_trip_on_recorded_data() {
        let _home = v2_home();
        let data = recorded();
        super::super::store::save(&data).unwrap();
        let before = std::fs::read(super::super::store::file()).unwrap();
        let report = run(true).unwrap();
        assert_eq!((report.work, report.outbox, report.schedules, report.chat, report.builtins), (1, 2, 2, 3, 6), "{report:?}");
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        // The backup is v1's file as it was, and v1's file is still there, untouched.
        let backup = super::super::store::file().with_file_name("data.v1.json");
        assert_eq!(std::fs::read(&backup).unwrap(), before);
        assert_eq!(std::fs::read(super::super::store::file()).unwrap(), before);
        // Each part is where v2 reads it.
        let ledger = WorkLedger::load().unwrap();
        assert_eq!(ledger.items.len(), 1);
        assert_eq!(ledger.items[0].pr, Some(("acme/app".to_string(), 1111)));
        assert!(matches!(&ledger.items[0].state, crate::work::WorkState::Waiting(who) if who == "Sam"));
        let outbox = super::super::messages::Outbox::load().unwrap();
        assert_eq!(outbox.messages.iter().map(|m| (m.id.as_str(), m.state)).collect::<Vec<_>>(), [("m-sent", MsgState::Sent), ("m-draft", MsgState::Draft)]);
        let conversation = crate::conversation::Conversation::load();
        assert_eq!(conversation.items().len(), 2);
        assert!(conversation.items().iter().all(|item| item.words().is_none()), "v1 lines back nothing");
        assert_eq!(playbooks::native::NativeFile::load().unwrap().native.pending(), 1);
        // A second run of the same data.json changes nothing.
        assert_eq!(run(true).unwrap(), report);
        std::fs::remove_file(Migrated::path()).unwrap();
        let again = run(true).unwrap();
        assert_eq!((again.work, again.outbox, again.schedules, again.chat, again.settled, again.builtins), (0, 0, 0, 0, 0, 0), "each step is idempotent besides");
        assert_eq!(WorkLedger::load().unwrap().items.len(), 1);
        // Only the home, only in a v2 home.
        assert!(run(false).is_err());
    }

    #[test]
    fn v1_keeps_writing_and_the_next_run_takes_it() {
        let _home = v2_home();
        let mut data = recorded();
        super::super::store::save(&data).unwrap();
        run(true).unwrap();
        // v1's manager goes on in the v2 home: it sends the draft, the follow-up is done, and the
        // person says something more.
        super::super::messages::decide(&mut data, "m-draft", true, None, 1_763_200_000);
        super::super::messages::mark_sent(&mut data, "m-draft", 1_763_200_010);
        data.followups[0].state = FollowState::Done;
        data.followups[0].updated = 1_763_300_000;
        data.chat.push(ChatLine { who: Who::You, text: "thanks".into(), at: 1_763_500_100, show: None });
        data.seen.insert("send|m-draft".into(), "1763200010@1763200010".into());
        super::super::store::save(&data).unwrap();
        let report = run(true).unwrap();
        assert_eq!((report.outbox, report.work, report.chat), (1, 1, 1), "{report:?}");
        let outbox = super::super::messages::Outbox::load().unwrap();
        assert_eq!(outbox.messages.iter().find(|m| m.id == "m-draft").map(|m| m.state), Some(MsgState::Sent));
        assert_eq!(WorkLedger::load().unwrap().items[0].state, crate::work::WorkState::Done);
        // The draft's send v1 made is a settled effect now: v2 never sends it again.
        let sent = data.outbox.iter().find(|m| m.id == "m-draft").unwrap();
        assert!(matches!(effects::claim(&super::super::messages::send_intent(sent, "x"), &MachineId::this().unwrap()).unwrap(), effects::Claim::Already(_)));
        // A sent message never goes back, whatever an older copy says; Activity lists none of v1's
        // memory.
        let mut older = data.clone();
        older.outbox.iter_mut().for_each(|m| m.state = MsgState::Draft);
        older.chat.push(ChatLine { who: Who::System, text: "older copy".into(), at: 1_763_500_200, show: None });
        super::super::store::save(&older).unwrap();
        run(true).unwrap();
        assert!(super::super::messages::Outbox::load().unwrap().messages.iter().all(|m| m.state == MsgState::Sent));
        assert!(effects::recent(30).unwrap().iter().all(|r| r.origin != effects::V1_SEEN));
    }

    #[test]
    fn an_unreadable_data_json_stops_it() {
        let _home = v2_home();
        std::fs::create_dir_all(super::super::store::dir()).unwrap();
        std::fs::write(super::super::store::file(), "{ not json").unwrap();
        assert!(run(true).unwrap_err().contains("does not read"));
        assert!(!Migrated::path().exists(), "no marker: it runs again once repaired");
        assert!(!super::super::store::file().with_file_name("data.v1.json").exists());
        super::super::store::save(&recorded()).unwrap();
        assert!(run(true).is_ok());
    }

    #[test]
    fn checks_start_disabled() {
        let _home = v2_home();
        super::super::store::save(&recorded()).unwrap();
        run(true).unwrap();
        let file = crate::schedule::ScheduleFile::load().unwrap();
        assert_eq!(file.schedules.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["v1-check-c1", "v1-check-c-2"]);
        assert!(file.schedules.iter().all(|s| !s.enabled && s.proposed_by.as_deref() == Some(BY)));
        assert!(matches!(file.schedules[0].spec, ScheduleSpec::Every { secs: 3600 }));
        assert!(matches!(file.schedules[1].spec, ScheduleSpec::Once { .. }));
        // Nothing of them runs: not on, not approved.
        let approved = playbooks::ApprovedPlaybooks::load().unwrap();
        assert!(file.schedules.iter().all(|s| !crate::schedule::is_approved(s, &approved)));
        let mut state = crate::schedule::ScheduleState::default();
        for hour in 0..48u64 {
            let now = crate::clock::now_ms() + hour * 3_600_000;
            assert!(crate::schedule::evaluate(&file.schedules, &|s| crate::schedule::is_approved(s, &approved), &mut state, now, true).0.is_empty());
        }
    }

    #[test]
    fn seen_keys_prevent_repeat() {
        let _home = v2_home();
        let data = recorded();
        super::super::store::save(&data).unwrap();
        run(true).unwrap();
        let home = MachineId::this().unwrap();
        // The message v1 sent is a settled send: v2's sender never sends it again.
        let sent = data.outbox.iter().find(|m| m.id == "m-sent").unwrap();
        let intent = super::super::messages::send_intent(sent, "run-1");
        assert!(matches!(effects::claim(&intent, &home).unwrap(), effects::Claim::Already(record) if matches!(record.state, EffectState::Done(_))));
        // The draft was never sent: its send is new.
        let draft = data.outbox.iter().find(|m| m.id == "m-draft").unwrap();
        assert!(matches!(effects::claim(&super::super::messages::send_intent(draft, "run-1"), &home).unwrap(), effects::Claim::New(_)));
        // Every seen key is a settled effect.
        for key in data.seen.keys() {
            assert!(effects::find(&seen_key(key)).unwrap().is_some_and(|record| matches!(record.state, EffectState::Done(_))), "{key}");
        }
        // The built-ins carry on: the session v1 told about is not told again in this stop, and the
        // review v1 had not told yet is told when its session is free.
        let mut native = playbooks::native::NativeFile::load().unwrap().native;
        assert!(native.asked("s1"));
        let settings = Settings { enabled: true, name: "AI Robin".into(), ..Settings::default() };
        let sessions = vec![crate::manager::SessionInfo { id: "s1".into(), title: "Work s1".into(), state: crate::manager::SessionState::NeedsYou, ..Default::default() }, crate::manager::SessionInfo { id: "s2".into(), title: "Work s2".into(), state: crate::manager::SessionState::Idle, ..Default::default() }];
        let view = playbooks::native::View { settings: &settings, sessions: &sessions, me: &[], now_ms: 0 };
        let mut asks = crate::journal::Event::new(crate::journal::kinds::SESSION_NEEDS_INPUT, crate::journal::Source::Hook, "asks").session(Some("s1"));
        asks.id = "e1".into();
        assert!(native.on_event(&asks, &view).is_empty(), "said once, before the switch");
        let tells = native.tell_ready(&view);
        assert_eq!(tells.len(), 1);
        assert!(matches!(&tells[0], crate::manager::Action::TellSession { session, text } if session == "s2" && text.contains("PR #12")));
    }
}
