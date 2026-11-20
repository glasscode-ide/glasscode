//! The effect ledger (M31; design D9, sections 6 and 8; review events-7, alt-6, land-21): one use,
//! one result.
//!
//! Every effect a caller other than the person sets off has a key made from what it is
//! ([`effect_key`]: its origin, its subject, the op and the target). The flow is:
//! 1. look the key up and write a claim, synced to disk before the effect ([`claim`]);
//! 2. act;
//! 3. write the result ([`finish`]).
//!
//! The same key seen again gives the stored record ([`Claim::Already`]), so an event replayed
//! after a restart does not act twice. A failed effect whose op may run again (idempotent or
//! verifiable) can be claimed again after a backoff; an at-most-once one never.
//!
//! **After a crash** a claim may have no result. A claim of an earlier run of the app
//! ([`unsettled`]) is settled by a probe with no model ([`settle`]): the op's own check (for
//! `session.send`, the session's events file, `delivery_session::probe`) says whether it
//! happened. An effect with no probe that may run only once (the messenger) goes to the person as
//! uncertain ([`needs_person`]).
//!
//! **Files**: `effects/<yyyy-mm>.jsonl` (UTC months), JSON lines, append only. A claim line, then
//! a finish line for the same key and attempt. Every claim and finish takes `effects/.lock`,
//! reads the months kept, and appends; it is synced (`fsync`, `FlushFileBuffers` on Windows)
//! before it returns. Nothing is renamed, so the Windows caveat of design section 7 (a rename not
//! proven to last a power cut) does not reach a claim. A line a crash cut off is cut away under
//! the lock before the next line is written, so it never becomes a damaged line in the middle of
//! a file. A damaged line (one that does not read, not at the end) refuses new at-most-once effects
//! until the file is repaired; the others go on. Retention deletes whole month files after a year
//! ([`KEEP_MONTHS`]).
//!
//! Each record carries the home that claimed it and the home's generation (D5); M58 registers
//! outward claims with the server, and until then the generation is 0. The server's records are
//! merged in by key and generation ([`merge_theirs`]).

use std::collections::HashMap;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::ops::Retry;
use crate::paths::{MachineId, ProjectKey};

/// The schema of a line. A line of a newer one is never read (and counts as no damage).
pub const SCHEMA: u16 = 1;

/// How many months of records are kept: a year (design section 6, "1 year (setting)").
pub const KEEP_MONTHS: u32 = 12;

/// How many times one effect is tried at most.
pub const MAX_ATTEMPTS: u32 = 5;

/// The first wait before a failed effect may be tried again; it doubles each time, up to
/// [`MAX_BACKOFF_MS`].
pub const BACKOFF_MS: u64 = 30_000;
pub const MAX_BACKOFF_MS: u64 = 30 * 60_000;

/// How long the Activity view shows autonomous actions (land-21).
pub const ACTIVITY_DAYS: u64 = 30;

const DAY_MS: u64 = 86_400_000;

// ----- keys --------------------------------------------------------------------------------------

/// What an effect is, as 32 bytes: the same effect gives the same key on every machine.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EffectKey(pub [u8; 32]);

impl EffectKey {
    pub fn hex(&self) -> String {
        hex(&self.0)
    }

    /// 64 hex digits.
    pub fn parse(text: &str) -> Option<EffectKey> {
        if text.len() != 64 || !text.is_ascii() {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
        }
        Some(EffectKey(out))
    }
}

impl std::fmt::Display for EffectKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.hex())
    }
}

impl std::fmt::Debug for EffectKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EffectKey({})", &self.hex()[..12])
    }
}

impl Serialize for EffectKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for EffectKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<EffectKey, D::Error> {
        let text = String::deserialize(deserializer)?;
        EffectKey::parse(&text).ok_or_else(|| serde::de::Error::custom("an effect key is 64 hex digits"))
    }
}

/// The key of an effect (events-7): its origin (a playbook's or rule's id, a chat message's id),
/// its subject (`acme/app#42@<head sha>`, a schedule's id and the occurrence), the op, and the
/// target (a session id, a PR). Each part is length-prefixed, so no two different sets of parts
/// give the same text to hash.
pub fn effect_key(origin: &str, subject: &str, op: &str, target: &str) -> EffectKey {
    let mut hasher = Sha256::new();
    hasher.update(b"glasscode-effect-v1");
    for part in [origin, subject, op, target] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    EffectKey(hasher.finalize().into())
}

/// 24 hex digits of the SHA-256 of a text: what a record keeps of an op's words or a message,
/// instead of the text.
pub fn short_hash(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes())[..12])
}

// ----- records -----------------------------------------------------------------------------------

/// Where an effect is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "is", content = "value", rename_all = "snake_case")]
pub enum EffectState {
    /// Claimed, and no result yet: in flight in this run, or cut off by a crash in an earlier one.
    Claimed,
    /// It happened; what it gave.
    Done(serde_json::Value),
    Failed(String),
    /// Nobody can tell whether it happened: the person decides.
    Uncertain,
}

/// What a probe found out (design section 8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeResult {
    Happened(serde_json::Value),
    DidNotHappen,
    Unknown,
}

/// What a caller is about to do: what [`claim`] writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    pub key: EffectKey,
    /// The op's name: `session.send`.
    pub op: String,
    pub retry: Retry,
    pub origin: String,
    pub subject: String,
    pub target: String,
    /// Who set it off, as the Activity view says it: "the manager", "Claude in “Fix login”".
    pub caller: String,
    /// Why, in a few words: "CI failed on #42".
    pub reason: String,
    pub project: Option<ProjectKey>,
    pub session: Option<String>,
    /// This run of the app (`control.launch`): a claim of an earlier run with no result was cut
    /// off by a crash.
    pub launch: String,
    /// What the probe needs later (for `session.send`, the length of the session's events file
    /// at the claim, and the marker typed with the message).
    pub evidence: serde_json::Value,
}

impl Intent {
    /// The intent of `op` for `key`, every other field empty.
    pub fn new(key: EffectKey, op: &str, retry: Retry) -> Intent {
        Intent {
            key,
            op: op.to_string(),
            retry,
            origin: String::new(),
            subject: String::new(),
            target: String::new(),
            caller: String::new(),
            reason: String::new(),
            project: None,
            session: None,
            launch: String::new(),
            evidence: serde_json::Value::Null,
        }
    }
}

fn schema() -> u16 {
    SCHEMA
}

/// One effect, as its lines fold: the newest claim of its key, and that attempt's result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectRecord {
    #[serde(default = "schema")]
    pub v: u16,
    pub key: EffectKey,
    pub op: String,
    pub retry: Retry,
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub caller: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The home that claimed it (D5), and the home's generation (0 until M58).
    pub home: MachineId,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub launch: String,
    pub claimed_ms: u64,
    /// 1 for the first claim; one more for each claim again after a failure.
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub evidence: serde_json::Value,
    pub state: EffectState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_ms: Option<u64>,
}

impl EffectRecord {
    /// When it may be claimed again: a failed effect of an op that may run again, after its
    /// backoff, while it has tries left. None: never.
    pub fn retry_at(&self) -> Option<u64> {
        let EffectState::Failed(_) = self.state else { return None };
        if self.retry == Retry::AtMostOnce || self.attempt >= MAX_ATTEMPTS {
            return None;
        }
        Some(self.finished_ms.unwrap_or(self.claimed_ms).saturating_add(backoff(self.attempt)))
    }
}

/// The wait after the `attempt`-th try failed.
pub fn backoff(attempt: u32) -> u64 {
    BACKOFF_MS.saturating_mul(1u64 << attempt.saturating_sub(1).min(16)).min(MAX_BACKOFF_MS)
}

/// What a claim found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "claim", content = "value", rename_all = "snake_case")]
pub enum Claim {
    /// Nobody claimed it (or its failure may be tried again): act, then [`finish`].
    New(EffectKey),
    /// Claimed before: its record. Done or failed, it is the result; claimed, it is in flight (or
    /// cut off by a crash, [`unsettled`]); uncertain, it waits for the person.
    Already(EffectRecord),
}

/// One line of a month's file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "line", rename_all = "snake_case")]
enum Line {
    Claim(EffectRecord),
    Finish {
        #[serde(default = "schema")]
        v: u16,
        key: EffectKey,
        attempt: u32,
        state: EffectState,
        ms: u64,
    },
}

impl Line {
    fn v(&self) -> u16 {
        match self {
            Line::Claim(record) => record.v,
            Line::Finish { v, .. } => *v,
        }
    }
}

/// The records of the months kept, as their lines fold, and the files with a damaged line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    pub records: HashMap<EffectKey, EffectRecord>,
    pub damaged: Vec<PathBuf>,
}

impl Ledger {
    /// Take one line in, in the order they were written: a claim replaces an older attempt of its
    /// key; a finish settles the attempt it names.
    fn take(&mut self, line: Line) {
        match line {
            Line::Claim(record) => {
                let newer = self.records.get(&record.key).is_none_or(|have| record.attempt >= have.attempt);
                if newer {
                    self.records.insert(record.key, record);
                }
            }
            Line::Finish { key, attempt, state, ms, .. } => {
                if let Some(record) = self.records.get_mut(&key).filter(|record| record.attempt == attempt) {
                    record.state = state;
                    record.finished_ms = Some(ms);
                }
            }
        }
    }
}

// ----- files -------------------------------------------------------------------------------------

/// `effects/<yyyy-mm>.jsonl` of the UTC month of `at_ms`.
fn month_file(at_ms: u64) -> PathBuf {
    let (year, month) = year_month(at_ms);
    crate::paths::effects_dir().join(format!("{year:04}-{month:02}.jsonl"))
}

/// The UTC year and month of a time (H. Hinnant's `civil_from_days`).
fn year_month(at_ms: u64) -> (i64, u32) {
    let z = (at_ms / DAY_MS) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month as u32)
}

/// A month as one number, to compare: year * 12 + month.
fn month_number((year, month): (i64, u32)) -> i64 {
    year * 12 + i64::from(month)
}

/// The month files, oldest first, with their months.
fn month_files() -> Vec<(i64, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(crate::paths::effects_dir()) else { return Vec::new() };
    let mut files: Vec<(i64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(".jsonl")?;
            let (year, month) = stem.split_once('-')?;
            let (year, month): (i64, u32) = (year.parse().ok()?, month.parse().ok()?);
            (stem.len() == 7 && (1..=12).contains(&month)).then(|| (month_number((year, month)), entry.path()))
        })
        .collect();
    files.sort();
    files
}

/// Whether a line that did not read is a newer schema's (no damage) rather than a damaged one.
fn newer_schema(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line).ok().and_then(|value| value.get("v").and_then(serde_json::Value::as_u64)).is_some_and(|v| v > u64::from(SCHEMA))
}

/// Read the months kept ([`KEEP_MONTHS`] before `now_ms`, and any later). It reads the disk.
fn read_ledger(now_ms: u64) -> std::io::Result<Ledger> {
    let oldest = month_number(year_month(now_ms)) - i64::from(KEEP_MONTHS);
    let mut ledger = Ledger::default();
    for (_, path) in month_files().into_iter().filter(|(month, _)| *month > oldest) {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        let whole = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |at| at + 1);
        let text = String::from_utf8_lossy(&bytes[..whole]);
        let mut damaged = false;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str::<Line>(line) {
                Ok(read) if read.v() <= SCHEMA => ledger.take(read),
                _ if newer_schema(line) => {}
                _ => damaged = true,
            }
        }
        if damaged {
            ledger.damaged.push(path);
        }
    }
    Ok(ledger)
}

/// `effects/.lock`, held until dropped: one claim or finish at a time, across processes.
fn lock() -> std::io::Result<crate::sys::FileLock> {
    let dir = crate::paths::effects_dir();
    make_folder(&dir)?;
    crate::sys::FileLock::take(&dir.join(".lock")).map_err(|err| std::io::Error::other(err.to_string()))
}

fn make_folder(dir: &Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Append one line to a month's file and sync it before returning. The caller holds the lock. A
/// line a crash cut off at the end is cut away first.
fn append(path: &Path, line: &Line) -> std::io::Result<()> {
    let text = serde_json::to_string(line).map_err(std::io::Error::other)? + "\n";
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    let len = file.metadata()?.len();
    let mut end = len;
    if len > 0 {
        let mut last = [0u8];
        file.seek(SeekFrom::Start(len - 1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            // Back to the end of the last whole line.
            let mut bytes = Vec::new();
            file.seek(SeekFrom::Start(0))?;
            file.read_to_end(&mut bytes)?;
            end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |at| at + 1) as u64;
            file.set_len(end)?;
        }
    }
    #[cfg(test)]
    tests::fault("before_write")?;
    file.seek(SeekFrom::Start(end))?;
    #[cfg(test)]
    if tests::fault("torn").is_err() {
        file.write_all(&text.as_bytes()[..text.len() / 2])?;
        return Err(std::io::Error::other("fault: torn"));
    }
    file.write_all(text.as_bytes())?;
    file.sync_data()?;
    #[cfg(unix)]
    if len == 0 {
        std::fs::File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()?;
    }
    Ok(())
}

// ----- claims ------------------------------------------------------------------------------------

/// Claim an effect before acting: the stored record when its key was claimed before (and may not
/// be tried again yet), else a claim synced to disk ([`Claim::New`]). An at-most-once effect is
/// refused while a file of the ledger is damaged. It reads and writes the disk: never on the main
/// thread.
pub fn claim(intent: &Intent, home: &MachineId) -> std::io::Result<Claim> {
    claim_at(intent, home, 0)
}

/// [`claim`] by the home at `generation` (M58): the arbiter on the server registers a Mac's
/// outward effects this way, and fences what an older generation claimed ([`fence_older`]).
pub fn claim_at(intent: &Intent, home: &MachineId, generation: u64) -> std::io::Result<Claim> {
    let _lock = lock()?;
    let now = crate::clock::now_ms();
    let ledger = read_ledger(now)?;
    let have = ledger.records.get(&intent.key);
    if let Some(have) = have
        && have.retry_at().is_none_or(|at| now < at)
    {
        return Ok(Claim::Already(have.clone()));
    }
    if intent.retry == Retry::AtMostOnce && !ledger.damaged.is_empty() {
        let files: Vec<String> = ledger.damaged.iter().map(|path| path.display().to_string()).collect();
        return Err(std::io::Error::other(format!("the effect ledger is damaged ({}); effects that may run only once wait until it is repaired", files.join(", "))));
    }
    let record = EffectRecord {
        v: SCHEMA,
        key: intent.key,
        op: intent.op.clone(),
        retry: intent.retry,
        origin: intent.origin.clone(),
        subject: intent.subject.clone(),
        target: intent.target.clone(),
        caller: intent.caller.clone(),
        reason: intent.reason.clone(),
        project: intent.project.clone(),
        session: intent.session.clone(),
        home: home.clone(),
        generation,
        launch: intent.launch.clone(),
        claimed_ms: now,
        attempt: have.map_or(1, |have| have.attempt + 1),
        evidence: intent.evidence.clone(),
        state: EffectState::Claimed,
        finished_ms: None,
    };
    append(&month_file(now), &Line::Claim(record))?;
    Ok(Claim::New(intent.key))
}

/// Home moved on to `generation` (M58): every claim of an older generation that has no result
/// is `Uncertain` now, so the new home checks it before anything tries it again, and it never
/// runs from the old home. Returns how many. It reads and writes the disk.
pub fn fence_older(generation: u64) -> std::io::Result<usize> {
    let older: Vec<EffectKey> = load()?.records.values().filter(|r| r.state == EffectState::Claimed && r.generation < generation).map(|r| r.key).collect();
    for key in &older {
        finish(key, EffectState::Uncertain)?;
    }
    Ok(older.len())
}

/// The result of an effect claimed before: the newest attempt of its key. It reads and writes the
/// disk.
pub fn finish(key: &EffectKey, state: EffectState) -> std::io::Result<()> {
    let _lock = lock()?;
    let now = crate::clock::now_ms();
    let ledger = read_ledger(now)?;
    let record = ledger.records.get(key).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("no claim of {key}")))?;
    append(&month_file(now), &Line::Finish { v: SCHEMA, key: *key, attempt: record.attempt, state, ms: now })
}

/// [`finish`] on a thread of its own, for a caller on the main thread with no executor at hand;
/// a failure is logged.
pub fn finish_later(key: EffectKey, state: EffectState) {
    let started = std::thread::Builder::new().name("glasscode-effects".into()).spawn(move || {
        if let Err(err) = finish(&key, state) {
            log::error!("effects: the result of {key} could not be written ({err})");
        }
    });
    if let Err(err) = started {
        log::error!("effects: the result of {key} could not be written ({err})");
    }
}

/// Write an effect that is already settled (M44: v1's "seen" keys become effect keys, so nothing
/// is said twice after the switch). Nothing is written when its key is there.
pub fn record_settled(intent: &Intent, home: &MachineId, state: EffectState) -> std::io::Result<bool> {
    match claim(intent, home)? {
        Claim::New(key) => finish(&key, state).map(|()| true),
        Claim::Already(_) => Ok(false),
    }
}

/// The record of a key, if it was claimed in the months kept. It reads the disk.
pub fn find(key: &EffectKey) -> std::io::Result<Option<EffectRecord>> {
    Ok(read_ledger(crate::clock::now_ms())?.records.remove(key))
}

/// The ledger as it is: every record of the months kept, and the damaged files. It reads the disk.
pub fn load() -> std::io::Result<Ledger> {
    read_ledger(crate::clock::now_ms())
}

/// The claims of earlier runs of the app (not `launch`) that have no result: cut off by a crash or
/// a quit. Each is settled by its probe ([`settle`]). It reads the disk.
pub fn unsettled(launch: &str) -> std::io::Result<Vec<EffectRecord>> {
    let mut out: Vec<EffectRecord> = load()?.records.into_values().filter(|record| record.state == EffectState::Claimed && record.launch != launch).collect();
    out.sort_by_key(|record| record.claimed_ms);
    Ok(out)
}

/// What a claim cut off by a crash comes to, by its probe (no model): it happened, it did not
/// (it may be claimed again, as a failure), or nobody can tell. An at-most-once effect without a
/// probe (`None`) is uncertain: the person decides.
pub fn settle(record: &EffectRecord, probe: Option<ProbeResult>) -> EffectState {
    match probe {
        Some(ProbeResult::Happened(value)) => EffectState::Done(value),
        Some(ProbeResult::DidNotHappen) => EffectState::Failed("it did not happen (the probe found no sign of it after a restart)".to_string()),
        Some(ProbeResult::Unknown) => EffectState::Uncertain,
        None if record.retry == Retry::Idempotent => EffectState::Failed("cut off by a restart; it may run again".to_string()),
        None => EffectState::Uncertain,
    }
}

/// Whether the person has to say what became of it: uncertain.
pub fn needs_person(record: &EffectRecord) -> bool {
    record.state == EffectState::Uncertain
}

/// The records claimed in the last `days` days, newest first: the Activity view's autonomous
/// actions (land-21). It reads the disk.
pub fn recent(days: u64) -> std::io::Result<Vec<EffectRecord>> {
    let since = crate::clock::now_ms().saturating_sub(days.saturating_mul(DAY_MS));
    let mut out: Vec<EffectRecord> = load()?.records.into_values().filter(|record| record.claimed_ms >= since).collect();
    out.sort_by(|a, b| b.claimed_ms.cmp(&a.claimed_ms).then_with(|| a.key.cmp(&b.key)));
    Ok(out)
}

/// Take in the other home's records (M57; the server's claims merged by key on the Mac): a key not
/// here is written as it is; for a key here, theirs wins when its generation is newer, or when it
/// is the same attempt and settled where ours is only claimed. Gives how many changed here.
pub fn merge_theirs(records: &[EffectRecord]) -> std::io::Result<usize> {
    let _lock = lock()?;
    let now = crate::clock::now_ms();
    let ledger = read_ledger(now)?;
    let mut changed = 0;
    for theirs in records {
        let take = match ledger.records.get(&theirs.key) {
            None => true,
            Some(ours) => theirs.generation > ours.generation || (theirs.generation == ours.generation && theirs.attempt > ours.attempt),
        };
        if take {
            append(&month_file(now), &Line::Claim(EffectRecord { state: EffectState::Claimed, finished_ms: None, ..theirs.clone() }))?;
            if theirs.state != EffectState::Claimed {
                append(&month_file(now), &Line::Finish { v: SCHEMA, key: theirs.key, attempt: theirs.attempt, state: theirs.state.clone(), ms: theirs.finished_ms.unwrap_or(now) })?;
            }
            changed += 1;
            continue;
        }
        let ours = &ledger.records[&theirs.key];
        if ours.attempt == theirs.attempt && ours.state == EffectState::Claimed && theirs.state != EffectState::Claimed {
            append(&month_file(now), &Line::Finish { v: SCHEMA, key: theirs.key, attempt: theirs.attempt, state: theirs.state.clone(), ms: theirs.finished_ms.unwrap_or(now) })?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// Retention: delete the month files older than [`KEEP_MONTHS`]. Gives how many went.
pub fn prune() -> std::io::Result<usize> {
    let _lock = lock()?;
    let oldest = month_number(year_month(crate::clock::now_ms())) - i64::from(KEEP_MONTHS);
    let mut went = 0;
    for (month, path) in month_files() {
        if month <= oldest {
            std::fs::remove_file(&path)?;
            went += 1;
        }
    }
    Ok(went)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;
    use std::ffi::OsString;

    use super::*;
    use crate::testkit::clock::{DAY, VirtualClock, Zone, utc_ms};

    thread_local! {
        static FAULT: RefCell<Option<&'static str>> = const { RefCell::new(None) };
    }

    /// Fail at `step` of the next append ("before_write", "torn"), once.
    pub(crate) fn fault(step: &'static str) -> std::io::Result<()> {
        FAULT.with(|fault| {
            if *fault.borrow() == Some(step) {
                *fault.borrow_mut() = None;
                return Err(std::io::Error::other(format!("fault: {step}")));
            }
            Ok(())
        })
    }

    fn inject(step: &'static str) {
        FAULT.with(|fault| *fault.borrow_mut() = Some(step));
    }

    pub(crate) struct Home {
        _dir: tempfile::TempDir,
        pub clock: std::sync::Arc<VirtualClock>,
        _guard: crate::clock::TestClockGuard,
        before: Option<OsString>,
    }

    impl Home {
        pub(crate) fn new() -> Home {
            let dir = tempfile::tempdir().unwrap();
            let before = std::env::var_os("GLASSCODE_HOME");
            // SAFETY: the core tests run one at a time (`--test-threads=1`).
            unsafe { std::env::set_var("GLASSCODE_HOME", dir.path()) };
            let clock = VirtualClock::new(utc_ms(2026, 11, 20, 9, 0), Zone::utc());
            let guard = clock.install();
            Home { _dir: dir, clock, _guard: guard, before }
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            // SAFETY: as in `new`.
            match self.before.take() {
                Some(value) => unsafe { std::env::set_var("GLASSCODE_HOME", value) },
                None => unsafe { std::env::remove_var("GLASSCODE_HOME") },
            }
        }
    }

    fn mac() -> MachineId {
        MachineId::parse("00000000000000aa").unwrap()
    }

    fn intent(subject: &str, retry: Retry) -> Intent {
        Intent {
            origin: "ci-fix".into(),
            subject: subject.into(),
            target: "s-1".into(),
            caller: "the CI fix rule".into(),
            reason: "CI failed on #42".into(),
            launch: "run-1".into(),
            ..Intent::new(effect_key("ci-fix", subject, "session.send", "s-1"), "session.send", retry)
        }
    }

    /// `older_generation_claim_never_runs` (M58): when home moves on, a claim of an older
    /// generation with no result is Uncertain (checked before any retry); a newer one is left.
    #[test]
    fn older_generation_claim_never_runs() {
        let _home = Home::new();
        let old = intent("acme/app#1@a", Retry::AtMostOnce);
        let newer = intent("acme/app#2@b", Retry::AtMostOnce);
        assert!(matches!(claim_at(&old, &mac(), 1).unwrap(), Claim::New(_)));
        assert!(matches!(claim_at(&newer, &mac(), 2).unwrap(), Claim::New(_)));
        assert_eq!(fence_older(2).unwrap(), 1);
        assert_eq!(find(&old.key).unwrap().unwrap().state, EffectState::Uncertain);
        assert_eq!(find(&newer.key).unwrap().unwrap().state, EffectState::Claimed);
        assert!(matches!(claim(&old, &mac()).unwrap(), Claim::Already(r) if r.state == EffectState::Uncertain), "it does not run again");
        // The intent and the claim cross the wire to the arbiter and back.
        let back: Intent = serde_json::from_value(serde_json::to_value(&old).unwrap()).unwrap();
        assert_eq!(back, old);
    }

    #[test]
    fn keys_are_the_same_for_the_same_effect_only() {
        let key = effect_key("ci-fix", "acme/app#42@abc", "session.send", "s-1");
        assert_eq!(key, effect_key("ci-fix", "acme/app#42@abc", "session.send", "s-1"));
        assert_ne!(key, effect_key("ci-fix", "acme/app#42@abd", "session.send", "s-1"));
        // Parts are kept apart: moving a letter from one to the next is another key.
        assert_ne!(effect_key("ab", "c", "op", "t"), effect_key("a", "bc", "op", "t"));
        assert_eq!(EffectKey::parse(&key.hex()), Some(key));
        assert_eq!(serde_json::from_str::<EffectKey>(&serde_json::to_string(&key).unwrap()).unwrap(), key);
        assert!(EffectKey::parse("abc").is_none());
    }

    #[test]
    fn replayed_event_does_not_repeat() {
        let _home = Home::new();
        let first = intent("acme/app#42@abc", Retry::Verifiable);
        assert_eq!(claim(&first, &mac()).unwrap(), Claim::New(first.key));
        // The same event again while it runs: in flight.
        let Claim::Already(record) = claim(&first, &mac()).unwrap() else { panic!("claimed twice") };
        assert_eq!(record.state, EffectState::Claimed);
        finish(&first.key, EffectState::Done(serde_json::json!({ "receipt": "hook" }))).unwrap();
        // Replayed after it ran (a restart reads the event again): the stored result.
        let Claim::Already(record) = claim(&first, &mac()).unwrap() else { panic!("ran twice") };
        assert_eq!(record.state, EffectState::Done(serde_json::json!({ "receipt": "hook" })));
        assert_eq!(record.home, mac());
        assert_eq!(record.generation, 0);
        assert_eq!(record.attempt, 1);
        // A new head is another effect.
        assert!(matches!(claim(&intent("acme/app#42@def", Retry::Verifiable), &mac()).unwrap(), Claim::New(_)));
    }

    #[test]
    fn a_failure_is_tried_again_after_its_backoff_unless_it_may_run_once() {
        let home = Home::new();
        let send = intent("acme/app#1@a", Retry::Verifiable);
        claim(&send, &mac()).unwrap();
        finish(&send.key, EffectState::Failed("the session was asking something".into())).unwrap();
        assert!(matches!(claim(&send, &mac()).unwrap(), Claim::Already(_)), "too soon");
        home.clock.advance(BACKOFF_MS);
        assert_eq!(claim(&send, &mac()).unwrap(), Claim::New(send.key));
        assert_eq!(find(&send.key).unwrap().unwrap().attempt, 2);
        assert_eq!(backoff(2), 2 * BACKOFF_MS);
        assert_eq!(backoff(40), MAX_BACKOFF_MS);
        // A message to a teammate is never sent again by itself.
        let message = Intent { op: "message.send".into(), ..intent("to sam", Retry::AtMostOnce) };
        claim(&message, &mac()).unwrap();
        finish(&message.key, EffectState::Failed("network".into())).unwrap();
        home.clock.advance(DAY);
        assert!(matches!(claim(&message, &mac()).unwrap(), Claim::Already(_)));
        // Past its tries, nothing either.
        let mut tries = 2;
        loop {
            finish(&send.key, EffectState::Failed("again".into())).unwrap();
            home.clock.advance(MAX_BACKOFF_MS);
            match claim(&send, &mac()).unwrap() {
                Claim::New(_) => tries += 1,
                Claim::Already(record) => {
                    assert_eq!(record.attempt, MAX_ATTEMPTS);
                    break;
                }
            }
        }
        assert_eq!(tries, MAX_ATTEMPTS);
    }

    #[test]
    fn crash_between_act_and_result_resolved_by_probe() {
        let _home = Home::new();
        let send = Intent { evidence: serde_json::json!({ "events_len": 0, "marker": "[gc:12345678]" }), ..intent("acme/app#42@abc", Retry::Verifiable) };
        claim(&send, &mac()).unwrap();
        // The app quits before the result. Its next run finds the claim of the run before.
        assert!(unsettled("run-1").unwrap().is_empty(), "this run's own claim is in flight");
        let cut = unsettled("run-2").unwrap();
        assert_eq!(cut.len(), 1);
        // The probe found the message in the session's events: it happened.
        let state = settle(&cut[0], Some(ProbeResult::Happened(serde_json::json!({ "receipt": "hook" }))));
        finish(&cut[0].key, state).unwrap();
        assert!(unsettled("run-2").unwrap().is_empty());
        let Claim::Already(record) = claim(&Intent { launch: "run-2".into(), ..send.clone() }, &mac()).unwrap() else { panic!("sent twice") };
        assert!(matches!(record.state, EffectState::Done(_)));
        // One the probe cannot settle is uncertain; one it says did not happen may run again.
        let other = intent("acme/app#43@abc", Retry::Verifiable);
        claim(&other, &mac()).unwrap();
        assert_eq!(settle(&unsettled("run-2").unwrap()[0], Some(ProbeResult::Unknown)), EffectState::Uncertain);
        assert!(matches!(settle(&unsettled("run-2").unwrap()[0], Some(ProbeResult::DidNotHappen)), EffectState::Failed(_)));
    }

    #[test]
    fn messenger_uncertain_goes_to_person() {
        let _home = Home::new();
        let message = Intent { op: "message.send".into(), ..intent("to sam: the release", Retry::AtMostOnce) };
        claim(&message, &mac()).unwrap();
        let cut = unsettled("run-2").unwrap();
        // No probe: nobody can tell whether the messenger sent it.
        let state = settle(&cut[0], None);
        assert_eq!(state, EffectState::Uncertain);
        finish(&message.key, state).unwrap();
        let record = find(&message.key).unwrap().unwrap();
        assert!(needs_person(&record));
        // It is never sent again by itself.
        assert!(matches!(claim(&message, &mac()).unwrap(), Claim::Already(_)));
        // An idempotent op cut off may simply run again.
        let read = intent("read", Retry::Idempotent);
        claim(&read, &mac()).unwrap();
        assert!(matches!(settle(&find(&read.key).unwrap().unwrap(), None), EffectState::Failed(_)));
    }

    /// Kill the process at each step of claim, act and finish: nothing acts twice, nothing is
    /// lost that was synced, and the ledger is never damaged by it.
    #[test]
    fn fault_injection_at_each_step() {
        let _home = Home::new();
        let send = intent("acme/app#42@abc", Retry::Verifiable);
        // A crash before the claim's line is written: no claim; claiming again works.
        inject("before_write");
        assert!(claim(&send, &mac()).is_err());
        assert!(find(&send.key).unwrap().is_none());
        // A crash halfway through the line: the cut line is not a claim, and is no damage.
        inject("torn");
        assert!(claim(&send, &mac()).is_err());
        let ledger = load().unwrap();
        assert!(ledger.records.is_empty() && ledger.damaged.is_empty(), "{ledger:?}");
        assert_eq!(claim(&send, &mac()).unwrap(), Claim::New(send.key));
        let text = std::fs::read_to_string(month_file(crate::clock::now_ms())).unwrap();
        assert_eq!(text.lines().count(), 1, "the cut line went before the claim was written: {text}");
        // A crash after the claim, before the act or after it: the next run finds it unsettled.
        assert_eq!(unsettled("run-2").unwrap().len(), 1);
        // A crash halfway through the finish line: still unsettled, then settled.
        inject("torn");
        assert!(finish(&send.key, EffectState::Done(serde_json::Value::Null)).is_err());
        assert_eq!(unsettled("run-2").unwrap().len(), 1);
        finish(&send.key, EffectState::Done(serde_json::Value::Null)).unwrap();
        assert!(unsettled("run-2").unwrap().is_empty());
        assert!(load().unwrap().damaged.is_empty());
        // Every cut of the file at any byte reads without damage, and never as more than it was.
        let path = month_file(crate::clock::now_ms());
        let whole = std::fs::read(&path).unwrap();
        for at in 0..whole.len() {
            std::fs::write(&path, &whole[..at]).unwrap();
            let ledger = load().unwrap();
            assert!(ledger.damaged.is_empty(), "cut at {at}");
            assert!(ledger.records.len() <= 1);
            if let Some(record) = ledger.records.get(&send.key) {
                assert!(matches!(record.state, EffectState::Claimed | EffectState::Done(_)));
            }
        }
        // A claim after any cut never acts twice once the claim was whole.
        std::fs::write(&path, &whole).unwrap();
        assert!(matches!(claim(&send, &mac()).unwrap(), Claim::Already(_)));
    }

    #[test]
    fn a_damaged_line_refuses_only_effects_that_may_run_once() {
        let _home = Home::new();
        let send = intent("a", Retry::Verifiable);
        claim(&send, &mac()).unwrap();
        let path = month_file(crate::clock::now_ms());
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("not a line\n{text}")).unwrap();
        assert_eq!(load().unwrap().damaged, [path.clone()]);
        assert!(find(&send.key).unwrap().is_some(), "the lines that read still count");
        assert!(matches!(claim(&intent("b", Retry::Verifiable), &mac()).unwrap(), Claim::New(_)));
        let message = Intent { op: "message.send".into(), ..intent("c", Retry::AtMostOnce) };
        assert!(claim(&message, &mac()).is_err());
        // A line of a newer schema is no damage.
        std::fs::write(&path, format!("{{\"line\":\"claim\",\"v\":9}}\n{text}")).unwrap();
        assert!(load().unwrap().damaged.is_empty());
        assert!(matches!(claim(&message, &mac()).unwrap(), Claim::New(_)));
    }

    #[test]
    fn records_are_kept_a_year_and_shown_for_thirty_days() {
        let home = Home::new();
        claim(&intent("old", Retry::Verifiable), &mac()).unwrap();
        home.clock.advance(40 * DAY);
        claim(&intent("new", Retry::Verifiable), &mac()).unwrap();
        let shown = recent(ACTIVITY_DAYS).unwrap();
        assert_eq!(shown.iter().map(|record| record.subject.as_str()).collect::<Vec<_>>(), ["new"]);
        assert_eq!(recent(60).unwrap().len(), 2);
        assert_eq!(prune().unwrap(), 0);
        // Eleven months on, the first month is past a year; the second is not.
        home.clock.advance(335 * DAY);
        assert_eq!(prune().unwrap(), 1);
        assert_eq!(load().unwrap().records.values().map(|record| record.subject.as_str()).collect::<Vec<_>>(), ["new"]);
    }

    #[test]
    fn the_servers_records_merge_by_key_and_generation() {
        let _home = Home::new();
        let server = MachineId::parse("00000000000000bb").unwrap();
        let ours = intent("acme/app#42@abc", Retry::Verifiable);
        claim(&ours, &mac()).unwrap();
        let theirs = EffectRecord { home: server.clone(), state: EffectState::Done(serde_json::json!("typed on the server")), finished_ms: Some(1), ..find(&ours.key).unwrap().unwrap() };
        // The same attempt, settled there: taken.
        assert_eq!(merge_theirs(std::slice::from_ref(&theirs)).unwrap(), 1);
        assert!(matches!(find(&ours.key).unwrap().unwrap().state, EffectState::Done(_)));
        assert_eq!(merge_theirs(std::slice::from_ref(&theirs)).unwrap(), 0, "nothing new");
        // A key only the server has, and one of a newer generation.
        let only = EffectRecord { key: effect_key("x", "y", "z", "w"), ..theirs.clone() };
        let newer = EffectRecord { generation: 1, state: EffectState::Failed("moved".into()), ..theirs.clone() };
        assert_eq!(merge_theirs(&[only.clone(), newer]).unwrap(), 2);
        assert!(find(&only.key).unwrap().is_some());
        let record = find(&ours.key).unwrap().unwrap();
        assert_eq!((record.generation, record.home), (1, server));
        assert!(matches!(record.state, EffectState::Failed(_)));
    }

    #[test]
    fn a_settled_record_is_written_once() {
        let _home = Home::new();
        let seen = intent("tell|s-1|acme/app#42|sam", Retry::Verifiable);
        assert!(record_settled(&seen, &mac(), EffectState::Done(serde_json::Value::Null)).unwrap());
        assert!(!record_settled(&seen, &mac(), EffectState::Done(serde_json::Value::Null)).unwrap());
        assert!(matches!(claim(&seen, &mac()).unwrap(), Claim::Already(_)));
        assert_eq!(crate::paths::class_of(&crate::paths::effects_dir()).class(), Some(crate::paths::PathClass::Ledger));
    }
}
