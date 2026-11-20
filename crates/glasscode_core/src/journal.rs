//! The journal (design D24, slice M14): one ordered record of what happened, in JSON lines.
//!
//! - **Files.** `journal/<yyyy-mm-dd>.jsonl`, one a UTC day, kept 30 days ([`rotate`]). Each line
//!   is an [`Event`] with its schema version `v`. The folder is private (0700) and its files too
//!   (0600) on macOS and Linux.
//! - **One writer thread per process** ([`append`]) stamps events with this machine's clock
//!   (`identity::next_stamps`, so the persisted `Hlc` advances), writes them in batches under the
//!   file's lock and syncs each batch: at once for an urgent event, else every 100 ms. A failed
//!   write keeps the events and tries again every 30 s; [`health`] says so, without the journal.
//! - **Order** is the stamp's, never the wall clock's: a clock that goes back cannot reorder it.
//! - **Readers** take a [`Cursor`] (a file and an offset) and [`read_from`] it; a damaged line is
//!   skipped and logged once. [`query`] answers `glasscode events` and `journal query`, within the
//!   caller's [`Scope`]: a session sees its own project, the manager the managed ones, the person
//!   everything. That keeps other projects' events out of an agent's context; it is not a wall,
//!   as any program of the person can read the files.
//! - **Redaction at write.** A prompt (or a reply) becomes its hash and a 200-character excerpt
//!   (owner decision O8), with secrets replaced by the words ledger's scrubber; other text is
//!   scrubbed and clipped.
//! - **Rule 2.** The journal runs only in a v2 home: with the engine at v1 nothing is written.
//!
//! Hook events reach it through each session's spool file (`events/<id>.jsonl`), which the app
//! and glasscoded tail ([`HookIntake`]); `glasscode hook` appends each payload there in one write
//! under a lock. The reconciler (`reconcile`) writes what no event explained.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::agents::AgentActivity;
use crate::caller::Caller;
use crate::identity::Hlc;
pub use crate::journal_kinds as kinds;
pub use crate::journal_kinds::{CATALOGUE, EventKind, KindInfo, Severity};
use crate::paths::{MachineId, ProjectKey};
use crate::store::{OnDamage, Store};

/// The schema of a line.
pub const VERSION: u16 = 1;
/// How long a day's file is kept.
pub const KEEP_DAYS: u64 = 30;
/// How many characters of a prompt or a reply are kept (O8).
pub const EXCERPT: usize = 200;
/// How long other text in an event may be.
const CLIP: usize = 2_000;
/// How long the writer gathers events before it syncs a batch.
const BATCH: Duration = Duration::from_millis(100);
/// How often a failed write is tried again.
const RETRY: Duration = Duration::from_secs(30);
/// Events kept while the journal cannot be written; older ones go first.
const BACKLOG: usize = 10_000;

/// Where an event comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The app itself (its window, its timers).
    App,
    /// A session's hook, through its spool file.
    Hook,
    /// glasscoded, on the server.
    Daemon,
    /// A checkout's git files.
    Git,
    /// GitHub, from a status round.
    Forge,
    /// The `glasscode` command and its replies.
    Control,
    Manager,
    /// The reconciler: what no event explained.
    Reconcile,
    /// The clocks: sleep, wake, a jump, a new time zone.
    Clock,
    /// A source this build does not know (a newer build wrote it).
    #[serde(other)]
    Other,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::App => "app",
            Source::Hook => "hook",
            Source::Daemon => "server",
            Source::Git => "git",
            Source::Forge => "github",
            Source::Control => "command",
            Source::Manager => "manager",
            Source::Reconcile => "reconciler",
            Source::Clock => "clock",
            Source::Other => "other",
        }
    }
}

/// One line of the journal (design section 8).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub v: u16,
    pub id: String,
    /// This machine's clock stamp, given by the writer: the order of the journal.
    pub at: Hlc,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub kind: EventKind,
    pub severity: Severity,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
    /// What links the steps of one decision (exec-31): each later step copies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    /// How old what it says was when it was seen: the poll's age for an event a poll found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u64>,
}

/// The stamp an event has until the writer gives it this machine's.
fn unstamped() -> Hlc {
    Hlc::zero(MachineId::parse("unstamped").expect("a folder name"))
}

impl Event {
    /// A new event of `kind`, with the kind's default severity. The writer stamps it.
    pub fn new(kind: EventKind, source: Source, summary: impl Into<String>) -> Event {
        let severity = kind.info().map(|info| info.severity).unwrap_or_default();
        Event { v: VERSION, id: new_id(), at: unstamped(), source, project: None, session: None, kind, severity, summary: summary.into(), data: Value::Null, trace: None, age_ms: None }
    }

    pub fn project(mut self, project: Option<&ProjectKey>) -> Event {
        self.project = project.cloned();
        self
    }

    pub fn session(mut self, session: Option<&str>) -> Event {
        self.session = session.map(str::to_string);
        self
    }

    pub fn data(mut self, data: Value) -> Event {
        self.data = data;
        self
    }

    pub fn severity(mut self, severity: Severity) -> Event {
        self.severity = severity;
        self
    }

    pub fn trace(mut self, trace: Option<&str>) -> Event {
        self.trace = trace.map(str::to_string);
        self
    }

    pub fn age(mut self, age_ms: u64) -> Event {
        self.age_ms = Some(age_ms);
        self
    }

    /// The wall time it was stamped at (milliseconds, UTC).
    pub fn at_ms(&self) -> u64 {
        self.at.wall_ms
    }

    /// When it happened: its stamp, or now for an event the writer has not stamped yet. The app's
    /// observers (the work ledger, the reconciler) see each event before `append` hands it to the
    /// writer, which stamps it; its `at_ms` is still 0 then.
    pub fn at_or_now(&self) -> u64 {
        if self.at.wall_ms == 0 { crate::clock::now_ms() } else { self.at.wall_ms }
    }
}

fn new_id() -> String {
    let mut bytes = [0u8; 8];
    crate::sys::random_bytes(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("e{hex}")
}

// ----- redaction -------------------------------------------------------------------------------

/// The fields that hold a prompt or a reply: kept as a hash and an excerpt only.
const PROMPT_KEYS: [&str; 4] = ["prompt", "last_assistant_message", "reply", "text"];

/// A prompt as the journal keeps it (O8): the hash of its scrubbed text, and its first
/// [`EXCERPT`] characters.
pub fn prompt_value(text: &str) -> Value {
    let scrubbed = crate::words::scrub(text);
    let hash: String = Sha256::digest(scrubbed.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    json!({ "hash": hash, "excerpt": clip(&scrubbed, EXCERPT), "chars": scrubbed.chars().count() })
}

fn clip(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// What the writer keeps of an event: secrets replaced, prompts hashed, long text clipped.
fn redact(event: &mut Event) {
    event.summary = clip(&crate::words::scrub(&event.summary), EXCERPT);
    redact_value(&mut event.data, None);
}

fn redact_value(value: &mut Value, key: Option<&str>) {
    match value {
        Value::String(text) if key.is_some_and(|key| PROMPT_KEYS.contains(&key)) => *value = prompt_value(text),
        Value::String(text) => *text = clip(&crate::words::scrub(text), CLIP),
        Value::Array(items) => items.iter_mut().for_each(|item| redact_value(item, key)),
        Value::Object(map) => map.iter_mut().for_each(|(key, item)| redact_value(item, Some(key))),
        _ => {}
    }
}

// ----- the features ------------------------------------------------------------------------------

/// The features were read.
static DECIDED: AtomicBool = AtomicBool::new(false);
/// The v2 features switched on, read with the engine switch.
static FEATURES: Mutex<Option<BTreeSet<String>>> = Mutex::new(None);

/// Whether a v2 feature (`EngineSwitch::is_on`, such as "turn_hooks") is on, as read when the
/// journal started ([`enabled`]); false before. No disk read.
pub fn feature_on(name: &str) -> bool {
    FEATURES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref().is_some_and(|on| on.contains(name))
}

/// The journal runs in every home (M56: Manager v2 everywhere). The first ask reads the engine
/// switch's features ([`feature_on`]): call [`init`] on a background thread at the start. True.
pub fn enabled() -> bool {
    if DECIDED.load(Ordering::Relaxed) {
        return true;
    }
    let switch = crate::store::EngineSwitch::current();
    let features: BTreeSet<String> = switch.switches.keys().filter(|name| switch.is_on(name)).cloned().collect();
    *FEATURES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(features);
    DECIDED.store(true, Ordering::Relaxed);
    true
}

/// Read the features again (the engine switch changed: a server that takes the Mac's, M57). It
/// reads the disk.
pub fn decide_again() -> bool {
    DECIDED.store(false, Ordering::Relaxed);
    *FEATURES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    init()
}

/// At the start, off the main thread: read the features, and drop days older than [`KEEP_DAYS`].
/// True.
pub fn init() -> bool {
    enabled();
    rotate(crate::clock::now_ms());
    true
}

// ----- the writer ------------------------------------------------------------------------------

enum Job {
    /// An event, and whether it keeps its stamp (one another machine wrote).
    Event(Box<Event>, bool),
    Flush(mpsc::Sender<()>),
}

static WRITER: Mutex<Option<mpsc::Sender<Job>>> = Mutex::new(None);

/// Add an event to the journal. It does no IO: the event goes to this process's writer thread,
/// which stamps, redacts and writes it (nothing, with the journal off).
pub fn append(mut event: Event) {
    // A hook's events were stamped as they were read, in the order of the spool.
    if event.source != Source::Hook {
        stamp(&mut event);
    }
    send(Job::Event(Box::new(event), false));
}

// ----- what a session's events descend from (events-13) ---------------------------------------

/// How long a session's events count as descending from a message Glasscode typed into it, at
/// most (a session whose CLI reports no prompts, Codex, would otherwise stay driven).
pub const DRIVEN_FOR_MS: u64 = 2 * 3_600_000;

/// What a session's events descend from: a manager's or a playbook's message typed into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Driven {
    /// The depth of the events it causes (the firing's, M40).
    pub depth: u8,
    /// The lineage keys of the effects they descend from (`playbooks::engine::lineage`), as hex.
    pub causes: Vec<String>,
    pub since_ms: u64,
}

/// Messages typed into a session, each waiting for the prompt that carries its marker, and what a
/// session's events descend from now.
static PENDING: Mutex<Option<HashMap<String, Vec<(String, Driven)>>>> = Mutex::new(None);
static DRIVEN: Mutex<Option<HashMap<String, Driven>>> = Mutex::new(None);

/// A message Glasscode types into `session` with `marker`, on behalf of an effect at `depth` with
/// these causes (lineage keys, hex): once the session's prompt carries the marker (the message
/// reached it), its events descend from it, until the person types to it.
pub fn expect_drive(session: &str, marker: &str, depth: u8, causes: &[String]) {
    let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
    let now = crate::clock::now_ms();
    let list = pending.get_or_insert_with(HashMap::new).entry(session.to_string()).or_default();
    list.retain(|(m, d)| m != marker && now.saturating_sub(d.since_ms) < DRIVEN_FOR_MS);
    list.push((marker.to_string(), Driven { depth, causes: causes.to_vec(), since_ms: now }));
}

/// The parameters of `caller::DRIVE_METHOD` (the Mac to the server): [`expect_drive`] there.
pub fn drive_params(session: &str, marker: &str, depth: u8, causes: &[String]) -> Value {
    json!({ "session": session, "marker": marker, "depth": depth, "causes": causes })
}

/// The server's side of `caller::DRIVE_METHOD`: the session's next prompt with the marker makes
/// its events descend from it, as on the Mac.
pub fn expect_drive_from(params: &Value) -> Result<(), String> {
    let text = |name: &str| params.get(name).and_then(Value::as_str).filter(|t| !t.is_empty()).ok_or_else(|| format!("{name} is missing"));
    let (session, marker) = (text("session")?, text("marker")?);
    if !marker.starts_with("[gc:") {
        return Err("a marker is [gc:…]".into());
    }
    let depth = params.get("depth").and_then(Value::as_u64).unwrap_or(1).min(u64::from(u8::MAX)) as u8;
    let causes: Vec<String> = params.get("causes").and_then(Value::as_array).into_iter().flatten().filter_map(|c| c.as_str().map(str::to_string)).collect();
    expect_drive(session, marker, depth, &causes);
    Ok(())
}

/// What `session`'s events descend from now, if anything.
pub fn driven(session: &str) -> Option<Driven> {
    let mut driven = DRIVEN.lock().unwrap_or_else(|p| p.into_inner());
    let map = driven.get_or_insert_with(HashMap::new);
    let now = crate::clock::now_ms();
    map.retain(|_, d| now.saturating_sub(d.since_ms) < DRIVEN_FOR_MS);
    map.get(session).cloned()
}

/// A prompt reached `session`: with the marker of a message Glasscode typed, its events descend
/// from that message from now on; with no marker at all, the person typed it, and its events are
/// theirs again (depth 0). A marker Glasscode did not note (v1's, or an expired one) changes
/// nothing.
fn prompt_reached(session: &str, prompt: &str) {
    let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
    let mut driven = DRIVEN.lock().unwrap_or_else(|p| p.into_inner());
    let pending = pending.get_or_insert_with(HashMap::new);
    let driven = driven.get_or_insert_with(HashMap::new);
    let list = pending.entry(session.to_string()).or_default();
    if let Some(at) = list.iter().position(|(marker, _)| prompt.contains(marker.as_str())) {
        let (_, mut next) = list.remove(at);
        next.since_ms = crate::clock::now_ms();
        driven.insert(session.to_string(), next);
    } else if !prompt.contains("[gc:") {
        driven.remove(session);
    }
}

/// Give an event of a driven session its depth and causes (in `data`), unless it has them.
pub fn stamp(event: &mut Event) {
    let Some(session) = event.session.clone() else { return };
    let Some(driven) = driven(&session) else { return };
    if event.data.get("depth").is_some() {
        return;
    }
    if !event.data.is_object() {
        event.data = if event.data.is_null() { json!({}) } else { json!({ "value": event.data.take() }) };
    }
    if let Some(map) = event.data.as_object_mut() {
        map.insert("depth".into(), json!(driven.depth));
        map.insert("causes".into(), json!(driven.causes));
    }
}

/// Tests: forget who drives which session.
#[cfg(any(test, feature = "testkit"))]
pub fn reset_driven() {
    *PENDING.lock().unwrap_or_else(|p| p.into_inner()) = None;
    *DRIVEN.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// Add an event another machine wrote and stamped (the server's, pulled by the Mac).
pub fn append_stamped(event: Event) {
    send(Job::Event(Box::new(event), true));
}

fn send(job: Job) {
    let mut writer = WRITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let job = match writer.as_ref() {
        Some(tx) => match tx.send(job) {
            Ok(()) => return,
            Err(mpsc::SendError(job)) => job,
        },
        None => job,
    };
    let (tx, rx) = mpsc::channel();
    let started = std::thread::Builder::new().name("glasscode-journal".into()).spawn(move || write_loop(rx));
    if let Err(err) = started {
        log::warn!("journal: could not start the writer ({err}); this event is not kept");
        return;
    }
    let _ = tx.send(job);
    *writer = Some(tx);
}

/// Wait until every event this process appended so far is on disk (or given up), for at most
/// 10 s: before the process ends, and in tests.
pub fn flush() {
    let (tx, rx) = mpsc::channel();
    let sent = WRITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref().is_some_and(|writer| writer.send(Job::Flush(tx)).is_ok());
    if sent {
        let _ = rx.recv_timeout(Duration::from_secs(10));
    }
}

/// How the journal's writing goes, without the journal (events-m4): what the status bar warns of.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Health {
    /// The last batch written (wall clock ms).
    pub last_write_ms: Option<u64>,
    /// Why the last write failed, while it fails, and since when.
    pub error: Option<String>,
    pub failing_since_ms: Option<u64>,
    /// Events waiting to be written.
    pub waiting: usize,
}

static HEALTH: Mutex<Health> = Mutex::new(Health { last_write_ms: None, error: None, failing_since_ms: None, waiting: 0 });

pub fn health() -> Health {
    HEALTH.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

fn write_loop(rx: mpsc::Receiver<Job>) {
    let mut backlog: Vec<(Event, bool)> = Vec::new();
    let mut last_try = Instant::now();
    let mut rotated_day = None;
    loop {
        let first = if backlog.is_empty() {
            match rx.recv() {
                Ok(job) => Some(job),
                Err(_) => return,
            }
        } else {
            match rx.recv_timeout(RETRY.saturating_sub(last_try.elapsed())) {
                Ok(job) => Some(job),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        };
        let mut flushes = Vec::new();
        // Whether a job asks for the batch now: an urgent event.
        let take = |job: Job, backlog: &mut Vec<(Event, bool)>, flushes: &mut Vec<mpsc::Sender<()>>| match job {
            Job::Event(event, stamped) => {
                let urgent = event.severity == Severity::Urgent;
                backlog.push((*event, stamped));
                urgent
            }
            Job::Flush(done) => {
                flushes.push(done);
                false
            }
        };
        if let Some(job) = first {
            let mut urgent = take(job, &mut backlog, &mut flushes);
            let deadline = Instant::now() + BATCH;
            while !urgent && flushes.is_empty() {
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(job) => urgent = take(job, &mut backlog, &mut flushes),
                    Err(_) => break,
                }
            }
        }
        if !backlog.is_empty() && enabled() {
            last_try = Instant::now();
            let today = day_number(crate::clock::now_ms());
            if rotated_day != Some(today) {
                rotated_day = Some(today);
                rotate(crate::clock::now_ms());
            }
            match write_batch(&mut backlog) {
                Ok(()) => {
                    backlog.clear();
                    let mut health = HEALTH.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    *health = Health { last_write_ms: Some(crate::clock::now_ms()), ..Health::default() };
                }
                Err(err) => {
                    let excess = backlog.len().saturating_sub(BACKLOG);
                    backlog.drain(..excess);
                    let mut health = HEALTH.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if health.error.is_none() {
                        log::error!("journal: could not be written ({err}); trying again every 30 s");
                        health.failing_since_ms = Some(crate::clock::now_ms());
                    }
                    health.error = Some(err);
                    health.waiting = backlog.len();
                }
            }
        } else {
            backlog.clear();
        }
        for done in flushes {
            let _ = done.send(());
        }
    }
}

/// This machine's id, once read.
static MACHINE: Mutex<Option<MachineId>> = Mutex::new(None);

fn machine() -> Result<MachineId, String> {
    let mut machine = MACHINE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(id) = machine.as_ref() {
        return Ok(id.clone());
    }
    let id = MachineId::this().map_err(|err| err.to_string())?;
    *machine = Some(id.clone());
    Ok(id)
}

/// Stamp, redact and write a batch, each day's lines in one append. The events keep their stamps
/// when this fails, so a retry writes them in their order.
fn write_batch(batch: &mut [(Event, bool)]) -> Result<(), String> {
    let machine = machine()?;
    let fresh = batch.iter().filter(|(event, stamped)| !*stamped && event.at.machine.as_str() == "unstamped").count();
    let mut stamps = crate::identity::next_stamps(&machine, fresh).map_err(|err| err.to_string())?.into_iter();
    let mut days: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for (event, stamped) in batch.iter_mut() {
        if !*stamped && event.at.machine.as_str() == "unstamped" {
            event.at = stamps.next().ok_or("too few clock stamps")?;
            redact(event);
        }
        let line = serde_json::to_string(event).map_err(|err| err.to_string())?;
        days.entry(day_file(event.at.wall_ms)).or_default().push(line);
    }
    for (path, lines) in days {
        crate::words::append_lines(&path, &lines).map_err(|err| format!("{}: {err}", path.display()))?;
    }
    Ok(())
}

// ----- days ------------------------------------------------------------------------------------

/// Days since 1970-01-01 (UTC).
fn day_number(at_ms: u64) -> i64 {
    (at_ms / 86_400_000) as i64
}

/// The UTC date of a day number (Howard Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// The day number of a date (days_from_civil).
fn days_from(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-11-20.jsonl`.
fn day_name(days: i64) -> String {
    let (year, month, day) = civil(days);
    format!("{year:04}-{month:02}-{day:02}.jsonl")
}

/// The day of a journal file's name; None for any other file of the folder.
fn day_of_name(name: &str) -> Option<i64> {
    let stem = name.strip_suffix(".jsonl")?;
    let mut parts = stem.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let (year, month, day): (i64, u32, u32) = (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    ((1..=12).contains(&month) && (1..=31).contains(&day)).then(|| days_from(year, month, day))
}

fn day_file(at_ms: u64) -> PathBuf {
    crate::paths::journal_dir().join(day_name(day_number(at_ms)))
}

/// The journal's day files, oldest first.
fn day_files() -> Vec<(i64, String)> {
    let mut files: Vec<(i64, String)> = std::fs::read_dir(crate::paths::journal_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            Some((day_of_name(&name)?, name))
        })
        .collect();
    files.sort();
    files
}

/// Remove the days older than [`KEEP_DAYS`] (today and the 29 before it stay), with their locks.
pub fn rotate(now_ms: u64) {
    let oldest = day_number(now_ms) - KEEP_DAYS as i64 + 1;
    for (day, name) in day_files() {
        if day < oldest {
            let path = crate::paths::journal_dir().join(&name);
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(path.with_extension("jsonl.lock"));
        }
    }
}

// ----- reading ---------------------------------------------------------------------------------

/// Where a reader is: a day's file and the offset after the last line it read. The start of the
/// journal is the empty name.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Cursor {
    pub file: String,
    pub offset: u64,
}

impl Cursor {
    /// `2026-11-20.jsonl:1234`, for a command line.
    pub fn text(&self) -> String {
        format!("{}:{}", self.file, self.offset)
    }

    pub fn parse(text: &str) -> Option<Cursor> {
        let (file, offset) = text.rsplit_once(':')?;
        (file.is_empty() || day_of_name(file).is_some()).then_some(())?;
        Some(Cursor { file: file.to_string(), offset: offset.parse().ok()? })
    }
}

/// The cursor after the journal's last line now: what a follower that wants only new events
/// starts from.
pub fn end() -> Cursor {
    match day_files().pop() {
        Some((_, name)) => {
            let len = std::fs::metadata(crate::paths::journal_dir().join(&name)).map(|m| m.len()).unwrap_or(0);
            Cursor { file: name, offset: len }
        }
        None => Cursor::default(),
    }
}

/// The damaged lines logged already (file, offset), so each is logged once.
static DAMAGED: Mutex<Option<BTreeSet<(String, u64)>>> = Mutex::new(None);

fn damaged_once(file: &str, offset: u64, err: &serde_json::Error) {
    let mut seen = DAMAGED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if seen.get_or_insert_with(BTreeSet::new).insert((file.to_string(), offset)) {
        log::warn!("journal: a damaged line in {file} at {offset} is skipped ({err})");
    }
}

/// Up to `max` events after `cursor`, in the journal's order, and the cursor after the last one
/// read. Only whole lines count (a line being written is read next time); a damaged line is
/// skipped, and logged once.
pub fn read_from(cursor: &Cursor, max: usize) -> (Vec<Event>, Cursor) {
    let mut out = Vec::new();
    let mut at = cursor.clone();
    for (_, name) in day_files().into_iter().filter(|(_, name)| *name >= cursor.file) {
        let start = if name == cursor.file { cursor.offset } else { 0 };
        let path = crate::paths::journal_dir().join(&name);
        let Ok(mut file) = std::fs::File::open(&path) else { continue };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if start >= len {
            if name != cursor.file {
                at = Cursor { file: name.clone(), offset: len.min(start) };
            }
            continue;
        }
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(start)).and_then(|_| file.take(len - start).read_to_end(&mut bytes)).is_err() {
            continue;
        }
        let mut offset = start;
        let mut rest = &bytes[..];
        while let Some(end) = rest.iter().position(|b| *b == b'\n') {
            let line = &rest[..end];
            let line_at = offset;
            offset += end as u64 + 1;
            rest = &rest[end + 1..];
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<Event>(line) {
                Ok(event) => out.push(event),
                Err(err) => damaged_once(&name, line_at, &err),
            }
            at = Cursor { file: name.clone(), offset };
            if out.len() >= max {
                return (out, at);
            }
        }
        at = Cursor { file: name.clone(), offset };
    }
    (out, at)
}

/// What a reader may see (events-6): everything, or the events of some projects only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Projects(Vec<ProjectKey>),
}

impl Scope {
    /// The person (the app, a Glasscode shell tab) and another Glasscode machine see everything;
    /// the manager the projects it manages; a session its own project; anyone else nothing that
    /// names a project.
    pub fn of(caller: &Caller, managed: &[ProjectKey]) -> Scope {
        match caller {
            Caller::Ui | Caller::ShellTab { .. } | Caller::Machine { .. } => Scope::All,
            Caller::Manager { .. } => Scope::Projects(managed.to_vec()),
            Caller::Session { project: Some(project), .. } => Scope::Projects(vec![project.clone()]),
            Caller::Session { project: None, .. } | Caller::Phone { .. } | Caller::Terminal { .. } => Scope::Projects(Vec::new()),
        }
    }

    pub fn sees(&self, event: &Event) -> bool {
        match self {
            Scope::All => true,
            Scope::Projects(projects) => event.project.as_ref().is_some_and(|project| projects.contains(project)),
        }
    }
}

/// What `glasscode events` and `journal query` ask for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub project: Option<ProjectKey>,
    pub session: Option<String>,
    /// A kind, a family (`session`) or a prefix (`reconciled.`).
    pub kind: Option<String>,
    pub since_ms: Option<u64>,
}

impl Filter {
    pub fn keeps(&self, event: &Event) -> bool {
        self.project.as_ref().is_none_or(|project| event.project.as_ref() == Some(project))
            && self.session.as_ref().is_none_or(|session| event.session.as_ref().is_some_and(|s| s == session || s.starts_with(session.as_str())))
            && self.kind.as_ref().is_none_or(|kind| kind.split(',').any(|kind| event.kind.matches(kind.trim())))
            && self.since_ms.is_none_or(|since| event.at.wall_ms >= since)
    }
}

/// The newest `limit` events that `filter` keeps and `scope` shows, oldest first.
pub fn query(filter: &Filter, scope: &Scope, limit: usize) -> Vec<Event> {
    let since_day = filter.since_ms.map(day_number);
    let mut out: Vec<Event> = Vec::new();
    for (day, name) in day_files().into_iter().rev() {
        if since_day.is_some_and(|since| day < since) {
            break;
        }
        let (events, _) = read_from(&Cursor { file: name, offset: 0 }, usize::MAX);
        let kept: Vec<Event> = events.into_iter().filter(|event| filter.keeps(event) && scope.sees(event)).collect();
        // This day's (older) events go before the newer days' already taken.
        let room = limit.saturating_sub(out.len());
        let skip = kept.len().saturating_sub(room);
        out.splice(0..0, kept.into_iter().skip(skip));
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// The events after `cursor` that `scope` shows, at most `max`, and the cursor to ask from next
/// (glasscoded's `journal.since`, `glasscode events --follow`).
pub fn since(cursor: &Cursor, scope: &Scope, filter: &Filter, max: usize) -> (Vec<Event>, Cursor) {
    let (events, next) = read_from(cursor, max);
    (events.into_iter().filter(|event| scope.sees(event) && filter.keeps(event)).collect(), next)
}

/// One line for a person: `14:02  session.needs_input  Claude needs your permission`.
pub fn line(event: &Event) -> String {
    let session = event.session.as_deref().map(|s| format!(" [{}]", s.chars().take(8).collect::<String>())).unwrap_or_default();
    format!("{}  {}{session}  {}", event.at.wall_ms, event.kind, event.summary)
}

// ----- spool cursors (events-4) ----------------------------------------------------------------

/// `journal/cursors.json`: where each reader of a spool stopped, so a restart reads on from there
/// instead of skipping what was written meanwhile: each session's hook spool (by session id), and
/// the server's journal for the Mac (by server).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cursors {
    pub spools: BTreeMap<String, u64>,
    pub servers: BTreeMap<String, Cursor>,
    /// How far this Mac's own journal went to each server with the mirror sync (M57), so events
    /// written before the first sync after a start go too.
    pub sent: BTreeMap<String, Cursor>,
}

impl Store for Cursors {
    const NAME: &'static str = "journal/cursors";
    const VERSION: u32 = 1;
    // Lost: each spool starts again from its end, as before M14 (the reconciler fills the gap).
    const ON_DAMAGE: OnDamage = OnDamage::Empty;

    fn path() -> PathBuf {
        crate::paths::journal_dir().join("cursors.json")
    }
}

// ----- hook events -----------------------------------------------------------------------------

/// The fields of a hook payload the journal keeps (the docs' fields of each hook; prompts and
/// replies hashed by the writer).
const HOOK_FIELDS: [&str; 15] = ["hook_event_name", "session_id", "source", "notification_type", "message", "prompt", "reason", "stop_hook_active", "last_assistant_message", "error", "tool_name", "prompt_id", "glasscode_at", "glasscode_seq", "glasscode_decision"];

/// A session's hook lines as events: each line itself (`session.hook`), and what it says about
/// the session (started, needs input, ended). Turns are M30's.
pub fn hook_events(session: &str, project: Option<&ProjectKey>, payload: &Value) -> Vec<Event> {
    let name = payload.get("hook_event_name").and_then(Value::as_str).unwrap_or("unknown");
    let mut kept = serde_json::Map::new();
    for field in HOOK_FIELDS {
        if let Some(value) = payload.get(field) {
            kept.insert(field.to_string(), value.clone());
        }
    }
    let text = |field: &str| payload.get(field).and_then(Value::as_str).unwrap_or_default().to_string();
    let mut out = vec![Event::new(kinds::SESSION_HOOK, Source::Hook, format!("hook {name}")).data(Value::Object(kept))];
    // Who typed this turn's prompt decides what its events descend from (events-13).
    if name == "UserPromptSubmit" {
        prompt_reached(session, &text("prompt"));
    }
    match name {
        "SessionStart" => out.push(Event::new(kinds::SESSION_STARTED, Source::Hook, format!("session started ({})", text("source"))).data(json!({ "source": text("source") }))),
        "Notification" => {
            let kind = text("notification_type");
            // Kinds that are news, not a wait: a sign-in that worked, an agent that finished.
            if !matches!(kind.as_str(), "auth_success" | "agent_completed") {
                let message = text("message");
                let summary = if message.is_empty() { "needs you".to_string() } else { message.clone() };
                out.push(Event::new(kinds::SESSION_NEEDS_INPUT, Source::Hook, summary).data(json!({ "notification_type": kind, "message": message })));
            }
        }
        "SessionEnd" => out.push(Event::new(kinds::SESSION_ENDED, Source::Hook, format!("session ended ({})", text("reason"))).data(json!({ "reason": text("reason") }))),
        _ => {}
    }
    // When the hook ran, as `glasscode hook` noted it: how old the line was when it was read.
    let age = payload.get("glasscode_at").and_then(Value::as_u64).map(|at| crate::clock::now_ms().saturating_sub(at));
    out.into_iter()
        .map(|event| {
            let mut event = event.session(Some(session)).project(project);
            stamp(&mut event);
            match age {
                Some(age) => event.age(age),
                None => event,
            }
        })
        .collect()
}

/// What reads one session's spool: its lines as events, and the lines it could not read or that
/// are missing (each hook line carries a counter), as `source.degraded` (events-20).
#[derive(Clone, Debug, Default)]
pub struct HookIntake {
    last_seq: Option<u64>,
}

impl HookIntake {
    pub fn events(&mut self, session: &str, project: Option<&ProjectKey>, lines: &[String]) -> Vec<Event> {
        let mut out = Vec::new();
        let (mut unreadable, mut lost) = (0u64, 0u64);
        for line in lines.iter().filter(|line| !line.trim().is_empty()) {
            let Ok(payload) = serde_json::from_str::<Value>(line) else {
                unreadable += 1;
                continue;
            };
            if let Some(seq) = payload.get("glasscode_seq").and_then(Value::as_u64) {
                if let Some(last) = self.last_seq
                    && seq > last + 1
                {
                    lost += seq - last - 1;
                }
                self.last_seq = Some(seq);
            }
            out.extend(hook_events(session, project, &payload));
        }
        if unreadable > 0 || lost > 0 {
            let why = match (unreadable, lost) {
                (0, lost) => format!("{lost} hook events are missing"),
                (unreadable, 0) => format!("{unreadable} hook lines could not be read"),
                (unreadable, lost) => format!("{unreadable} hook lines could not be read and {lost} are missing"),
            };
            out.push(source_degraded("hooks", &why).session(Some(session)).project(project).data(json!({ "source": "hooks", "unreadable": unreadable, "missing": lost })));
        }
        out
    }
}

// ----- helpers for the producers ---------------------------------------------------------------

/// Someone reviewed a session's pull request (M40's review-tell): v1's reader of the status
/// rounds found the reviewer's state changed (`followups::from_snapshot`).
pub fn review_received(session: &str, project: Option<&ProjectKey>, repo: &str, number: u64, reviewer: &str, association: Option<&str>, state: &str) -> Event {
    Event::new(kinds::REVIEW_RECEIVED, Source::App, format!("{reviewer} reviewed {repo}#{number}: {state}"))
        .session(Some(session))
        .project(project)
        .data(json!({ "repo": repo, "number": number, "reviewer": reviewer, "association": association, "state": state }))
}

/// A session's activity changed (the app's and glasscoded's view of it).
pub fn activity(session: &str, project: Option<&ProjectKey>, previous: Option<AgentActivity>, now: AgentActivity) -> Event {
    let name = |a: AgentActivity| match a {
        AgentActivity::Working => "working",
        AgentActivity::Idle => "idle",
        AgentActivity::NeedsInput => "needs_input",
    };
    Event::new(kinds::SESSION_ACTIVITY, Source::App, name(now)).session(Some(session)).project(project).data(json!({ "activity": name(now), "previous": previous.map(name) }))
}

/// A source stopped working (a poller failing, a hook spool losing lines).
pub fn source_degraded(source: &str, why: &str) -> Event {
    Event::new(kinds::SOURCE_DEGRADED, Source::App, format!("{source}: {why}")).data(json!({ "source": source, "why": why }))
}

/// A source that was degraded works again.
pub fn source_recovered(source: &str) -> Event {
    Event::new(kinds::SOURCE_RECOVERED, Source::App, format!("{source} works again")).data(json!({ "source": source }))
}

/// One step of a decision and why (step 8, exec-31): later slices write one for each step, with
/// the trace of what started it.
pub fn decision(step: &str, outcome: &str, reason: &str, inputs: Value, trace: Option<&str>) -> Event {
    Event::new(kinds::DECISION, Source::Manager, format!("{step}: {outcome} ({reason})")).data(json!({ "step": step, "outcome": outcome, "reason": reason, "inputs": inputs })).trace(trace)
}

// ----- git (from the checkouts' files) ---------------------------------------------------------

/// What a checkout's git files said last, per checkout: a commit, a push or another branch is the
/// difference to now. A checkout seen for the first time is a baseline.
#[derive(Default)]
pub struct GitWatch {
    seen: HashMap<PathBuf, crate::git_info::Heads>,
}

impl GitWatch {
    /// The events of `checkouts` (folder, project, a session there) since the last look. Reads
    /// files only, no git: in the background.
    pub fn observe(&mut self, checkouts: &[(PathBuf, Option<ProjectKey>, Option<String>)]) -> Vec<Event> {
        let mut out = Vec::new();
        for (worktree, project, session) in checkouts {
            let Some(now) = crate::git_info::heads(worktree) else { continue };
            let Some(before) = self.seen.insert(worktree.clone(), now.clone()) else { continue };
            let place = worktree.display().to_string();
            let event = |kind: EventKind, summary: String, data: Value| Event::new(kind, Source::Git, summary).project(project.as_ref()).session(session.as_deref()).data(data);
            if before.branch != now.branch {
                out.push(event(kinds::GIT_BRANCH, format!("on {} now (was {})", now.branch, before.branch), json!({ "worktree": place, "branch": now.branch, "before": before.branch })));
            } else if now.head.is_some() && before.head != now.head {
                out.push(event(kinds::GIT_COMMIT, format!("{} at {}", now.branch, short(now.head.as_deref())), json!({ "worktree": place, "branch": now.branch, "head": now.head, "before": before.head })));
            }
            if now.remote.is_some() && before.remote != now.remote && now.remote == now.head {
                out.push(event(kinds::GIT_PUSH, format!("pushed {} at {}", now.branch, short(now.head.as_deref())), json!({ "worktree": place, "branch": now.branch, "head": now.head })));
            }
        }
        let asked: BTreeSet<&PathBuf> = checkouts.iter().map(|(path, _, _)| path).collect();
        self.seen.retain(|path, _| asked.contains(path));
        out
    }
}

fn short(sha: Option<&str>) -> String {
    sha.map(|s| s.chars().take(8).collect()).unwrap_or_default()
}

// ----- the forge (from the status rounds) ------------------------------------------------------

/// What a status round knew of a session's pull request and its CI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForgeSeen {
    pub repo: Option<String>,
    pub number: Option<u64>,
    pub title: String,
    /// OPEN, MERGED or CLOSED.
    pub state: String,
    pub draft: bool,
    /// The commit the checks ran on, and how they stand: running, passed, failed (none: no checks).
    pub ci: Option<(String, String)>,
    pub failed: Vec<String>,
    /// MERGEABLE, CONFLICTING or UNKNOWN.
    pub mergeable: String,
}

/// How a list of checks stands: failed when one failed, running while one runs, else passed.
pub fn ci_overall(states: impl IntoIterator<Item = crate::github::CheckState>) -> Option<&'static str> {
    use crate::github::CheckState;
    let states: Vec<CheckState> = states.into_iter().collect();
    if states.is_empty() {
        return None;
    }
    Some(if states.contains(&CheckState::Failed) {
        "failed"
    } else if states.contains(&CheckState::Running) {
        "running"
    } else {
        "passed"
    })
}

/// A pull request that is no longer open on its session merged (a merge check found it).
pub fn pr_merged(session: &str, project: Option<&ProjectKey>, repo: &str, number: u64) -> Event {
    Event::new(kinds::PR_MERGED, Source::Forge, format!("{repo}#{number} merged")).session(Some(session)).project(project).data(json!({ "repo": repo, "number": number }))
}

/// What changed between two status rounds of a session's pull request. The first round this run
/// is a baseline (`before` None): it says nothing, and the reconciler reports what the journal
/// missed. Each event carries the round's age.
pub fn forge_events(session: &str, project: Option<&ProjectKey>, before: Option<&ForgeSeen>, now: &ForgeSeen, age_ms: u64) -> Vec<Event> {
    let Some(before) = before else { return Vec::new() };
    let (Some(repo), Some(number)) = (now.repo.as_deref(), now.number) else { return Vec::new() };
    let pr = format!("{repo}#{number}");
    let mut out = Vec::new();
    let event = |kind: EventKind, summary: String, data: Value| Event::new(kind, Source::Forge, summary).session(Some(session)).project(project).data(data).age(age_ms);
    let same_pr = before.repo == now.repo && before.number == now.number;
    if !same_pr {
        out.push(event(kinds::PR_OPENED, format!("{pr} {}", now.title), json!({ "repo": repo, "number": number, "title": now.title, "draft": now.draft })));
    } else if before.state != now.state {
        match now.state.as_str() {
            "MERGED" => out.push(event(kinds::PR_MERGED, format!("{pr} merged"), json!({ "repo": repo, "number": number }))),
            "CLOSED" => out.push(event(kinds::PR_CLOSED, format!("{pr} closed"), json!({ "repo": repo, "number": number }))),
            state => out.push(event(kinds::PR_CHANGED, format!("{pr} {}", state.to_lowercase()), json!({ "repo": repo, "number": number, "state": state, "draft": now.draft, "before": before.state }))),
        }
    } else if before.draft != now.draft {
        let summary = if now.draft { format!("{pr} is a draft again") } else { format!("{pr} is ready for review") };
        out.push(event(kinds::PR_CHANGED, summary, json!({ "repo": repo, "number": number, "state": now.state, "draft": now.draft, "before": before.state })));
    }
    if let Some((head, overall)) = &now.ci
        && (!same_pr || before.ci.as_ref() != Some(&(head.clone(), overall.clone())))
    {
        let data = json!({ "repo": repo, "number": number, "head": head, "failed": now.failed });
        match overall.as_str() {
            "failed" => out.push(event(kinds::CI_FAILED, format!("{pr}: CI failed ({})", now.failed.join(", ")), data)),
            "running" => out.push(event(kinds::CI_RUNNING, format!("{pr}: CI running"), data)),
            _ => out.push(event(kinds::CI_PASSED, format!("{pr}: CI passed"), data)),
        }
    }
    if now.mergeable == "CONFLICTING" && (before.mergeable != "CONFLICTING" || !same_pr) {
        out.push(event(kinds::PR_CONFLICT, format!("{pr} has conflicts"), json!({ "repo": repo, "number": number, "mergeable": now.mergeable })));
    }
    out
}

// ----- Today's Activity (M30's line per turn) ----------------------------------------------------

/// The kinds Today's Activity shows from the journal: a line per turn, and what happened to the
/// work around it (its pull request, CI, reviews, its state), sessions outside Glasscode, and the
/// manager's messages.
pub const ACTIVITY_KINDS: [&str; 11] = ["turn.ended", "pr.opened", "pr.merged", "pr.closed", "ci.failed", "ci.passed", "review.requested", "review.posted", "work.changed", "session.outside", "message.sent"];

/// One line of Today's Activity from the journal: when (ms), the session it is about, whether it
/// is a turn's report, and what it says.
#[derive(Clone, Debug, PartialEq)]
pub struct ActivityLine {
    pub at_ms: u64,
    pub session: Option<String>,
    pub turn: bool,
    pub text: String,
}

/// The journal's part of Today's Activity: the newest `limit` events of [`ACTIVITY_KINDS`] since
/// `since_ms`, newest first ([`activity_lines_of`]). Reads the day files: off the main thread.
pub fn activity_lines(since_ms: u64, limit: usize) -> Vec<ActivityLine> {
    let filter = Filter { kind: Some(ACTIVITY_KINDS.join(",")), since_ms: Some(since_ms), ..Filter::default() };
    activity_lines_of(&query(&filter, &Scope::All, limit))
}

/// Every event the person may see since `since_ms`, newest first, as Activity lines: what
/// `glasscode events` lists, each with its kind before its summary (M14's demonstration: Activity
/// shows the same events). Reads the day files: off the main thread.
pub fn event_lines(since_ms: u64, limit: usize) -> Vec<ActivityLine> {
    let filter = Filter { since_ms: Some(since_ms), ..Filter::default() };
    every_line_of(&query(&filter, &Scope::All, limit))
}

/// [`event_lines`] of these events.
pub fn every_line_of(events: &[Event]) -> Vec<ActivityLine> {
    events.iter().rev().map(|event| ActivityLine { at_ms: event.at_ms(), session: event.session.clone(), turn: event.kind == kinds::TURN_ENDED, text: format!("{}  {}", event.kind, event.summary) }).collect()
}

/// Events as Activity lines, newest first: each its summary with a capital letter (a turn's is
/// its report's line: "Completed in 42 s: 2 tools, 1 files"). Other kinds are left out.
pub fn activity_lines_of(events: &[Event]) -> Vec<ActivityLine> {
    events
        .iter()
        .rev()
        .filter(|event| ACTIVITY_KINDS.contains(&event.kind.as_str()))
        .map(|event| {
            let mut chars = event.summary.chars();
            let text = chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default();
            ActivityLine { at_ms: event.at_ms(), session: event.session.clone(), turn: event.kind == kinds::TURN_ENDED, text }
        })
        .collect()
}

// ----- usage and the server link (M41, M58) ------------------------------------------------------

/// The share of a usage window at which an account counts as near its limit (the usage-handoff
/// built-in's `usage.threshold`).
pub const USAGE_NEAR: f64 = 85.0;

/// The windows of an account that crossed [`USAGE_NEAR`] since the last report: `usage.threshold`
/// once per crossing. `near` keeps which (account id, window) are over it; one that falls below is
/// said again when it crosses again. `label` is the account's name as the person reads it.
pub fn usage_events(near: &mut BTreeSet<(String, u64)>, account: &str, label: &str, limits: &[crate::usage::Limit]) -> Vec<Event> {
    let mut out = Vec::new();
    for limit in limits {
        let key = (account.to_string(), limit.window_minutes);
        if limit.used_percent < USAGE_NEAR {
            near.remove(&key);
            continue;
        }
        if near.insert(key) {
            let percent = limit.used_percent.round() as u64;
            out.push(Event::new(kinds::USAGE_THRESHOLD, Source::App, format!("{label} is at {percent}% of {}", limit.label().to_lowercase())).severity(Severity::Warn).data(json!({ "account": label, "account_id": account, "window": limit.label(), "window_minutes": limit.window_minutes, "percent": percent, "resets_at": limit.resets_at })));
        }
    }
    out
}

/// The link to the server went (it answered before, and does not now), at `since_ms`.
pub fn link_lost(server: &str, since_ms: u64) -> Event {
    Event::new(kinds::LINK_LOST, Source::App, format!("the link to {server} is lost")).severity(Severity::Warn).data(json!({ "server": server, "since": since_ms }))
}

/// The link to the server is back, `after_ms` after it went.
pub fn link_back(server: &str, after_ms: u64) -> Event {
    Event::new(kinds::LINK_BACK, Source::App, format!("the link to {server} is back after {} s", after_ms / 1000)).data(json!({ "server": server, "after": after_ms }))
}

// ----- the clocks ------------------------------------------------------------------------------

/// The wall clock and the monotonic clock at the last look: a gap between the two is a sleep
/// (macOS and Linux stop the monotonic clock while asleep), a wall clock that went back is a
/// jump, and a new offset is a new time zone. Native sleep and wake notices come later (P17).
#[derive(Default)]
pub struct ClockWatch {
    last: Option<(u64, Instant, Option<i64>)>,
}

/// A gap this long between the two clocks is a sleep, not a correction.
const SLEEP_GAP_MS: i64 = 60_000;

impl ClockWatch {
    pub fn observe(&mut self, wall_ms: u64, mono: Instant) -> Vec<Event> {
        let offset = crate::clock::local_minutes(wall_ms).map(|local| local - (wall_ms / 60_000) as i64);
        let Some((last_wall, last_mono, last_offset)) = self.last.replace((wall_ms, mono, offset)) else { return Vec::new() };
        let mut out = Vec::new();
        let wall_gap = wall_ms as i64 - last_wall as i64;
        let mono_gap = mono.saturating_duration_since(last_mono).as_millis() as i64;
        if wall_gap - mono_gap >= SLEEP_GAP_MS || mono_gap >= SLEEP_GAP_MS {
            let asleep = (wall_gap - mono_gap).max(mono_gap);
            let from = last_wall + mono_gap.min(wall_gap).max(0) as u64;
            out.push(Event::new(kinds::APP_SLEPT, Source::Clock, format!("asleep for {} s", asleep / 1000)).data(json!({ "from": from, "to": wall_ms })));
            out.push(Event::new(kinds::APP_WOKE, Source::Clock, "awake again").data(json!({ "asleep_ms": asleep })));
        } else if wall_gap - mono_gap <= -SLEEP_GAP_MS {
            out.push(Event::new(kinds::CLOCK_JUMPED, Source::Clock, format!("the clock went back {} s", (mono_gap - wall_gap) / 1000)).data(json!({ "by_ms": wall_gap - mono_gap })));
        }
        if let (Some(before), Some(now)) = (last_offset, offset)
            && before != now
        {
            out.push(Event::new(kinds::CLOCK_ZONE, Source::Clock, format!("time zone {} (was {})", crate::clock::offset_words(now), crate::clock::offset_words(before))).data(json!({ "before": before, "now": now })));
        }
        out
    }
}

/// Forget what this process decided and read (tests: a new home).
pub fn reset() {
    flush();
    DECIDED.store(false, Ordering::Relaxed);
    *FEATURES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    *MACHINE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    *HEALTH.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Health::default();
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    /// A server session the Mac types into (M31): the Mac's call names the marker, and the server's
    /// own hook reader stamps what the session does next; a phone may not make the call.
    #[test]
    fn a_server_session_is_stamped_by_the_macs_call() {
        reset_driven();
        let params = drive_params("s9", "[gc:9a8b7c6d]", 2, &["ab".repeat(32)]);
        assert!(expect_drive_from(&json!({ "session": "s9", "marker": "nope" })).is_err());
        expect_drive_from(&params).unwrap();
        hook_events("s9", None, &json!({ "hook_event_name": "UserPromptSubmit", "prompt": "Fix the lint. [gc:9a8b7c6d]" }));
        let asks = hook_events("s9", None, &json!({ "hook_event_name": "Notification", "notification_type": "permission_prompt", "message": "May I?" })).pop().unwrap();
        assert_eq!((asks.data["depth"].clone(), asks.data["causes"][0].clone()), (json!(2), json!("ab".repeat(32))));
        let phone = crate::caller::Caller::Phone { device: "p1".into() };
        assert!(crate::caller::daemon_refusal(&phone, crate::caller::DRIVE_METHOD).is_some());
        reset_driven();
    }

    /// An account's window over the threshold says so once, and again only after it fell below;
    /// the server link's loss and return are events of their own.
    #[test]
    fn usage_crossings_and_the_link_are_said_once() {
        let limit = |percent: f64| crate::usage::Limit { window_minutes: 300, used_percent: percent, resets_at: Some(1_800_000_000) };
        let mut near = BTreeSet::new();
        assert!(usage_events(&mut near, "a1", "Claude (work)", &[limit(40.0)]).is_empty());
        let crossed = usage_events(&mut near, "a1", "Claude (work)", &[limit(91.4)]);
        assert_eq!(crossed.len(), 1);
        assert_eq!((crossed[0].kind.as_str(), crossed[0].data["account"].clone(), crossed[0].data["percent"].clone()), ("usage.threshold", json!("Claude (work)"), json!(91)));
        assert!(usage_events(&mut near, "a1", "Claude (work)", &[limit(95.0)]).is_empty(), "once while over");
        assert!(usage_events(&mut near, "a1", "Claude (work)", &[limit(10.0)]).is_empty());
        assert_eq!(usage_events(&mut near, "a1", "Claude (work)", &[limit(88.0)]).len(), 1, "again after it fell below");
        assert_eq!(link_lost("devbox", 5).kind.as_str(), "link.lost");
        assert_eq!(link_back("devbox", 61_000).data["after"], 61_000);
    }

    /// Activity's lines: a line per turn (its report's), the work's events, newest first, and
    /// nothing of the other kinds.
    #[test]
    fn activity_shows_a_line_per_turn_and_the_works_events() {
        let turn = Event::new(kinds::TURN_ENDED, Source::App, "completed in 42 s: 2 tools, 1 files").session(Some("s1"));
        let hook = Event::new(kinds::SESSION_HOOK, Source::Hook, "Stop").session(Some("s1"));
        let ci = Event::new(kinds::CI_FAILED, Source::Forge, "acme/app#7: CI failed (test)").session(Some("s1"));
        let lines = activity_lines_of(&[turn, hook, ci]);
        assert_eq!(lines.iter().map(|l| (l.text.as_str(), l.turn)).collect::<Vec<_>>(), [("Acme/app#7: CI failed (test)", false), ("Completed in 42 s: 2 tools, 1 files", true)]);
        assert_eq!(lines[1].session.as_deref(), Some("s1"));
        assert!(ACTIVITY_KINDS.iter().all(|kind| EventKind::named(kind).info().is_some()), "every kind is in the catalogue");
        // Every event, as `glasscode events` lists them: the hook too, with its kind.
        let turn = Event::new(kinds::TURN_ENDED, Source::App, "completed in 42 s").session(Some("s1"));
        let hook = Event::new(kinds::SESSION_HOOK, Source::Hook, "hook Stop").session(Some("s1"));
        let every = every_line_of(&[turn, hook]);
        assert_eq!(every.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["session.hook  hook Stop", "turn.ended  completed in 42 s"]);
    }

    /// A scratch home of the test's own, until dropped.
    pub(crate) struct Home {
        _dir: tempfile::TempDir,
        before: Vec<(&'static str, Option<OsString>)>,
    }

    impl Home {
        pub fn new() -> Home {
            let dir = tempfile::tempdir().unwrap();
            let names = ["GLASSCODE_HOME"];
            let before = names.map(|name| (name, std::env::var_os(name))).to_vec();
            // SAFETY: the core tests run one at a time (`--test-threads=1`).
            unsafe {
                std::env::set_var("GLASSCODE_HOME", dir.path());
            }
            reset();
            Home { _dir: dir, before }
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            reset();
            for (name, value) in self.before.drain(..) {
                // SAFETY: as in `new`.
                match value {
                    Some(value) => unsafe { std::env::set_var(name, value) },
                    None => unsafe { std::env::remove_var(name) },
                }
            }
        }
    }

    fn all() -> Vec<Event> {
        read_from(&Cursor::default(), usize::MAX).0
    }

    fn key(name: &str) -> ProjectKey {
        ProjectKey::parse(name).unwrap()
    }

    #[test]
    fn append_and_read_back_in_order() {
        let _home = Home::new();
        for n in 0..50 {
            append(Event::new(kinds::SESSION_ACTIVITY, Source::App, format!("event {n}")).session(Some("s1")));
        }
        flush();
        let events = all();
        assert_eq!(events.len(), 50);
        assert!(events.iter().enumerate().all(|(n, e)| e.summary == format!("event {n}") && e.v == VERSION));
        // Stamped by this machine, each later than the one before.
        assert!(events.windows(2).all(|pair| pair[0].at < pair[1].at));
        assert_eq!(events[0].at.machine, MachineId::this().unwrap());
        // Read a few at a time, from where the last read stopped.
        let (first, cursor) = read_from(&Cursor::default(), 20);
        let (rest, end_cursor) = read_from(&cursor, usize::MAX);
        assert_eq!(first.len() + rest.len(), 50);
        assert_eq!(rest[0].summary, "event 20");
        assert_eq!(end_cursor, end());
        assert!(health().error.is_none() && health().last_write_ms.is_some());
    }


    #[test]
    fn damaged_line_skipped() {
        let _home = Home::new();
        append(Event::new(kinds::APP_STARTED, Source::App, "one"));
        flush();
        let (_, name) = day_files().pop().unwrap();
        let path = crate::paths::journal_dir().join(&name);
        // A torn line (a crash in the middle of a write), then more events.
        std::fs::OpenOptions::new().append(true).open(&path).and_then(|mut f| std::io::Write::write_all(&mut f, b"{\"v\":1,\"id\":\"half")).unwrap();
        append(Event::new(kinds::APP_STARTED, Source::App, "two"));
        flush();
        let summaries: Vec<String> = all().into_iter().map(|e| e.summary).collect();
        assert_eq!(summaries, ["one", "two"]);
        // Read twice: logged once (the set), skipped both times.
        assert_eq!(all().len(), 2);
        assert_eq!(DAMAGED.lock().unwrap().as_ref().map(|seen| seen.iter().filter(|(file, _)| *file == name).count()), Some(1));
    }

    #[test]
    fn rotation_keeps_30_days() {
        let _home = Home::new();
        let now = days_from(2026, 11, 20) as u64 * 86_400_000 + 3_600_000;
        let dir = crate::paths::journal_dir();
        std::fs::create_dir_all(&dir).unwrap();
        for back in 0..45 {
            std::fs::write(dir.join(day_name(day_number(now) - back)), "").unwrap();
        }
        std::fs::write(dir.join("cursors.json"), "{}").unwrap();
        rotate(now);
        let left: Vec<String> = day_files().into_iter().map(|(_, name)| name).collect();
        assert_eq!(left.len(), 30);
        assert_eq!(left.first().map(String::as_str), Some("2026-10-22.jsonl"));
        assert_eq!(left.last().map(String::as_str), Some("2026-11-20.jsonl"));
        // Only day files go.
        assert!(dir.join("cursors.json").exists());
        assert_eq!(day_name(days_from(2024, 2, 29)), "2024-02-29.jsonl");
        assert_eq!(day_of_name("2024-02-29.jsonl"), Some(days_from(2024, 2, 29)));
        assert_eq!(day_of_name("cursors.json"), None);
    }

    #[test]
    fn session_scope_hides_other_projects() {
        let _home = Home::new();
        let (mine, theirs) = (key("app-3f9a1c"), key("billing-0a1b2c"));
        append(Event::new(kinds::SESSION_STARTED, Source::Hook, "mine").project(Some(&mine)).session(Some("s1")));
        append(Event::new(kinds::SESSION_STARTED, Source::Hook, "theirs").project(Some(&theirs)).session(Some("s2")));
        append(Event::new(kinds::APP_STARTED, Source::App, "nobody's"));
        flush();
        let seen = |caller: &Caller, managed: &[ProjectKey]| -> Vec<String> { query(&Filter::default(), &Scope::of(caller, managed), 100).into_iter().map(|e| e.summary).collect() };
        assert_eq!(seen(&Caller::Session { id: "s1".into(), project: Some(mine.clone()) }, &[]), ["mine"]);
        assert_eq!(seen(&Caller::Session { id: "s9".into(), project: None }, &[]), Vec::<String>::new());
        assert_eq!(seen(&Caller::Manager { lane: crate::caller::Lane::Wake, wake: None, project: None }, std::slice::from_ref(&theirs)), ["theirs"]);
        assert_eq!(seen(&Caller::Terminal { pid: None }, &[]), Vec::<String>::new());
        assert_eq!(seen(&Caller::Ui, &[]), ["mine", "theirs", "nobody's"]);
        // A filter within a scope.
        let only = Filter { session: Some("s2".into()), ..Filter::default() };
        assert_eq!(query(&only, &Scope::All, 10).len(), 1);
        let kinds = Filter { kind: Some("app".into()), ..Filter::default() };
        assert_eq!(query(&kinds, &Scope::All, 10).len(), 1);
        // The newest, when there are more.
        assert_eq!(query(&Filter::default(), &Scope::All, 2).into_iter().map(|e| e.summary).collect::<Vec<_>>(), ["theirs", "nobody's"]);
    }

    #[test]
    fn prompt_is_hashed_with_excerpt() {
        let _home = Home::new();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let prompt = format!("deploy with {secret} and then {}", "write the release notes ".repeat(20));
        append(Event::new(kinds::SESSION_HOOK, Source::Hook, format!("asked {secret}")).data(json!({ "prompt": prompt, "message": format!("token {secret}") })));
        flush();
        let event = all().pop().unwrap();
        let text = serde_json::to_string(&event).unwrap();
        assert!(!text.contains(secret), "{text}");
        let kept = &event.data["prompt"];
        let scrubbed = crate::words::scrub(&prompt);
        let hash: String = Sha256::digest(scrubbed.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(kept["hash"], hash);
        let excerpt = kept["excerpt"].as_str().unwrap();
        assert_eq!(excerpt.trim_end_matches('…').chars().count(), EXCERPT);
        assert!(scrubbed.starts_with(excerpt.trim_end_matches('…')));
        assert!(event.data["message"].as_str().unwrap().starts_with("token "));
    }

    #[test]
    fn restart_resumes_cursor() {
        let _home = Home::new();
        append(Event::new(kinds::APP_STARTED, Source::App, "before"));
        flush();
        // A reader keeps its cursor in the cursors store, and the process ends.
        let (_, cursor) = read_from(&Cursor::default(), usize::MAX);
        let mut cursors = Cursors::default();
        cursors.servers.insert("devbox".into(), cursor.clone());
        cursors.save().unwrap();
        assert_eq!(Cursor::parse(&cursor.text()), Some(cursor.clone()));
        reset();
        append(Event::new(kinds::APP_STARTED, Source::App, "after"));
        flush();
        let saved = Cursors::load().unwrap().servers["devbox"].clone();
        let (events, _) = read_from(&saved, usize::MAX);
        assert_eq!(events.into_iter().map(|e| e.summary).collect::<Vec<_>>(), ["after"]);
    }

    #[test]
    fn hook_lines_become_session_events() {
        let mut intake = HookIntake::default();
        let lines = [
            r#"{"hook_event_name":"SessionStart","session_id":"c1","source":"startup","glasscode_seq":1}"#.to_string(),
            r#"{"hook_event_name":"UserPromptSubmit","prompt":"fix the login test","glasscode_seq":2,"cwd":"/somewhere"}"#.to_string(),
            r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash","glasscode_seq":3}"#.to_string(),
            r#"{"hook_event_name":"Notification","notification_type":"auth_success","glasscode_seq":4}"#.to_string(),
            "not json".to_string(),
            r#"{"hook_event_name":"SessionEnd","reason":"prompt_input_exit","glasscode_seq":7}"#.to_string(),
        ];
        let events = intake.events("s1", None, &lines);
        let names: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(names, ["session.hook", "session.started", "session.hook", "session.hook", "session.needs_input", "session.hook", "session.hook", "session.ended", "source.degraded"]);
        assert!(events.iter().all(|e| e.session.as_deref() == Some("s1")));
        // Only the payload's known fields: not the folder.
        assert!(events[2].data.get("cwd").is_none() && events[2].data["prompt"] == "fix the login test");
        assert_eq!(events[4].data["notification_type"], "permission_prompt");
        assert_eq!(events.last().unwrap().data, json!({ "source": "hooks", "unreadable": 1, "missing": 2 }));
    }

    #[test]
    fn concurrent_appends_from_threads_keep_their_lines() {
        let _home = Home::new();
        let threads: Vec<_> = (0..8).map(|t| std::thread::spawn(move || (0..200).for_each(|n| append(Event::new(kinds::SESSION_HOOK, Source::Hook, format!("{t}-{n}")))))).collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        flush();
        let events = all();
        assert_eq!(events.len(), 1600);
        let unique: BTreeSet<&str> = events.iter().map(|e| e.summary.as_str()).collect();
        assert_eq!(unique.len(), 1600);
    }

    #[test]
    fn append_p95_is_within_budget() {
        let _home = Home::new();
        append(Event::new(kinds::APP_STARTED, Source::App, "warm"));
        flush();
        let mut took: Vec<Duration> = (0..2000)
            .map(|n| {
                let event = Event::new(kinds::SESSION_ACTIVITY, Source::App, format!("{n}")).data(json!({ "activity": "working" }));
                let start = Instant::now();
                append(event);
                start.elapsed()
            })
            .collect();
        flush();
        took.sort();
        // Design section 9: append ≤ 1 ms p95.
        assert!(took[took.len() * 95 / 100] <= Duration::from_millis(1), "{:?}", took[took.len() * 95 / 100]);
    }

    #[test]
    fn forge_rounds_say_what_changed() {
        let open = ForgeSeen { repo: Some("acme/app".into()), number: Some(7), title: "Fix login".into(), state: "OPEN".into(), draft: true, ci: Some(("abc".into(), "running".into())), failed: Vec::new(), mergeable: "MERGEABLE".into() };
        // The first round this run is a baseline.
        assert!(forge_events("s1", None, None, &open, 0).is_empty());
        assert!(forge_events("s1", None, Some(&open), &open, 0).is_empty());
        let failed = ForgeSeen { ci: Some(("abc".into(), "failed".into())), failed: vec!["test".into()], ..open.clone() };
        let events = forge_events("s1", None, Some(&open), &failed, 4_000);
        assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["ci.failed"]);
        assert_eq!(events[0].age_ms, Some(4_000));
        let ready = ForgeSeen { draft: false, mergeable: "CONFLICTING".into(), ..failed.clone() };
        assert_eq!(forge_events("s1", None, Some(&failed), &ready, 0).iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["pr.changed", "pr.conflict"]);
        let merged = ForgeSeen { state: "MERGED".into(), ..ready.clone() };
        assert_eq!(forge_events("s1", None, Some(&ready), &merged, 0).iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["pr.merged"]);
        let none = ForgeSeen::default();
        assert_eq!(forge_events("s1", None, Some(&none), &open, 0).iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["pr.opened", "ci.running"]);
    }

    #[test]
    fn a_gap_between_the_clocks_is_a_sleep() {
        let mut watch = ClockWatch::default();
        let start = Instant::now();
        assert!(watch.observe(1_000_000, start).is_empty());
        assert!(watch.observe(1_000_700, start + Duration::from_millis(700)).is_empty());
        // The wall clock moved on ten minutes, the monotonic one a second: asleep.
        let events = watch.observe(1_600_700, start + Duration::from_millis(1_700));
        assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["app.slept", "app.woke"]);
        assert_eq!(events[1].data["asleep_ms"], 599_000);
        // The wall clock went back five minutes.
        let events = watch.observe(1_300_700, start + Duration::from_millis(2_400));
        assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["clock.jumped"]);
    }

    #[test]
    fn git_files_say_commit_push_and_branch() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(dir.path()).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap().status.success(), "{args:?}");
        git(&["init", "-q", "-b", "main"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "one"]);
        let checkouts = vec![(dir.path().to_path_buf(), None, Some("s1".to_string()))];
        let mut watch = GitWatch::default();
        assert!(watch.observe(&checkouts).is_empty(), "the first look is a baseline");
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "two"]);
        assert_eq!(watch.observe(&checkouts).iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["git.commit"]);
        // A push: the remote branch now names the head.
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        assert_eq!(watch.observe(&checkouts).iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["git.push"]);
        git(&["checkout", "-q", "-b", "fix/login"]);
        let events = watch.observe(&checkouts);
        assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["git.branch"]);
        assert_eq!(events[0].data["branch"], "fix/login");
    }
}

#[cfg(test)]
mod scrub_keeps_ids {
    /// What the journal reads back (a turn's head, a session's id) is not taken for a secret.
    #[test]
    fn a_sha_and_a_uuid_are_kept() {
        for text in ["head 1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b", "session 8765f51a-8ad5-4e73-9df0-27adaa817aa9", "prompt_id=4f3c2b1a-0000-4000-8000-123456789abc"] {
            assert_eq!(crate::words::scrub(text), text, "{text}");
        }
    }
}
