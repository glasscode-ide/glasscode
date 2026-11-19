//! Load and save `state.json` atomically; and [`Store`], which every new store of Manager v2
//! implements (design sections 6 and 8): a schema version, an atomic save, and a rule for a
//! damaged file. The machine id and the engine switch are its first two stores.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::model::State;
use crate::paths::{self, MachineId};

pub fn load() -> State {
    let path = paths::state_file();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            log::error!("state.json is unreadable ({err}); keeping a backup and starting fresh");
            let _ = std::fs::copy(&path, path.with_extension("json.broken"));
            State::default()
        }),
        Err(_) => State::default(),
    }
}

pub fn save(state: &State) -> anyhow::Result<()> {
    paths::ensure_dirs()?;
    let path = paths::state_file();
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(state)?;
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

// ----- the Store of every new store ------------------------------------------------------------

/// What [`Store::load`] does with a damaged file that has no readable previous generation (the
/// "On damage" column of design section 6). Either way a `.broken` copy of it is kept first.
///
/// A store whose emptiness allows nothing (grants, pending approvals, device keys) may be `Empty`.
/// A store whose emptiness would allow something is `Refuse`, and its callers decide what a
/// refusal means, by its row of the data map: they **fail closed** (policy: every project counts as
/// Careful; pause: paused; playbook approvals: all off), or they **suspend and reconcile** (effect
/// ledger, home generation, tombstones, the trusted memory's commit: nothing acts until the file
/// is repaired and reconciled).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnDamage {
    /// Start from the default; the next save replaces the file.
    Empty,
    /// Fail and leave the file as it is, so it fails again on the next start until it is repaired.
    /// It never starts empty.
    Refuse,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The file could not be read or written: permissions, a full disk, a failed sync or rename, a
    /// folder in its place. Not damage, so nothing starts empty. From a save it means the save may
    /// not have happened, or may not last: what depended on it does not run.
    #[error("Glasscode could not read or write {store} ({}): {source}", path.display())]
    Io { store: &'static str, path: PathBuf, source: std::io::Error },
    /// The file is damaged and has no readable previous generation; `kept` is the copy of it, when
    /// one could be written.
    #[error("{store} is damaged ({why})")]
    Damaged { store: &'static str, why: String, kept: Option<PathBuf> },
    /// A newer Glasscode wrote the file: this build reads it as nothing and never writes it (the
    /// version rule of the data map).
    #[error("{store} was written by a newer Glasscode (schema {found}; this one knows up to {known}), so this one leaves it alone")]
    Newer { store: &'static str, found: u32, known: u32 },
    /// The file has an older schema that could not be brought up to date. It is left as it is.
    #[error("{store} could not be updated from schema {from} ({why})")]
    Migrate { store: &'static str, from: u32, why: String },
    #[error("{store} could not be written ({why})")]
    Encode { store: &'static str, why: String },
}

/// A file of Manager v2 (design section 8). On disk it is `{"store": NAME, "version": VERSION,
/// "data": …}`; a file without that envelope is damaged. Beside it, `<file>.prev` keeps the
/// generation before the last save. Every store has a path class (`paths::class_of`), a row in
/// [`DATA_MAP`] and a line in [`ALL_STORES`].
pub trait Store: Default + Serialize + DeserializeOwned {
    /// "authority/policy".
    const NAME: &'static str;
    /// The schema version; an older file goes through [`Store::migrate`].
    const VERSION: u32;
    const ON_DAMAGE: OnDamage;

    fn path() -> PathBuf;

    /// The store, as [`open`] finds it: the default when there is no file; the previous
    /// generation when the file is damaged; then the default ([`OnDamage::Empty`]) or
    /// [`StoreError::Damaged`] ([`OnDamage::Refuse`]). A newer schema, a failed migration and a
    /// read error are errors for every store. It reads the disk: never on the main thread.
    fn load() -> Result<Self, StoreError> {
        open::<Self>().map(|opened| opened.value)
    }

    /// The previous generation to `<file>.prev`, the new bytes to a temporary file beside it,
    /// synced, then renamed over it (`MoveFileEx` replacing it on Windows) and the folder synced,
    /// so a reader or a crash never sees half a file. A file of a newer schema is never written
    /// over ([`StoreError::Newer`]). Any failed write, sync or rename is an error.
    fn save(&self) -> Result<(), StoreError> {
        save_at(self, &Self::path())
    }

    /// The store from the data of an older schema `from` (1 or more). None by default.
    fn migrate(from: u32, value: serde_json::Value) -> Result<Self, StoreError> {
        let _ = value;
        Err(StoreError::Migrate { store: Self::NAME, from, why: "this store has no older schema".into() })
    }
}

/// Where [`open`] found a store, so its caller can say so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    /// The file.
    File,
    /// No file: the default.
    Missing,
    /// The file is damaged (copied to `broken`); its previous generation, `<file>.prev`, was read.
    Previous { broken: Option<PathBuf> },
    /// The file is damaged (copied to `broken`) and has no readable previous generation: the
    /// default of an [`OnDamage::Empty`] store.
    Empty { broken: PathBuf },
}

/// A store and where it was found.
#[derive(Debug)]
pub struct Opened<S> {
    pub value: S,
    pub found: Found,
}

/// The store `S` and where it was found ([`Store::load`] without the where).
pub fn open<S: Store>() -> Result<Opened<S>, StoreError> {
    open_at(&S::path())
}

#[derive(Serialize)]
struct Envelope<'a, T> {
    store: &'a str,
    version: u32,
    data: &'a T,
}

fn open_at<S: Store>(path: &Path) -> Result<Opened<S>, StoreError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Opened { value: S::default(), found: Found::Missing }),
        Err(source) => return Err(StoreError::Io { store: S::NAME, path: path.to_path_buf(), source }),
    };
    let why = match open_envelope(S::NAME, &bytes) {
        Ok((version, _)) if version > S::VERSION => {
            log::warn!("{} has schema {version}, newer than this build's {}; it is not read and not written", S::NAME, S::VERSION);
            return Err(StoreError::Newer { store: S::NAME, found: version, known: S::VERSION });
        }
        Ok((version, data)) if version < S::VERSION => {
            return S::migrate(version, data).map(|value| Opened { value, found: Found::File }).inspect_err(|err| {
                let kept = keep_copy(path, ".broken", &bytes);
                log::error!("{err}; the file is left as it is{}", kept.map(|k| format!(", and copied to {}", k.display())).unwrap_or_default());
            });
        }
        Ok((_, data)) => match serde_json::from_value(data) {
            Ok(value) => return Ok(Opened { value, found: Found::File }),
            Err(err) => err.to_string(),
        },
        Err(why) => why,
    };
    // Damaged: keep a copy, then the previous generation, then the store's rule.
    let broken = keep_copy(path, ".broken", &bytes);
    let previous = sibling(path, ".prev");
    if let Some(value) = read_generation::<S>(&previous) {
        log::warn!("{} is damaged ({why}); using its previous generation, {}", S::NAME, previous.display());
        return Ok(Opened { value, found: Found::Previous { broken } });
    }
    match (broken, S::ON_DAMAGE) {
        (Some(copy), OnDamage::Empty) => {
            log::error!("{} is damaged ({why}) and has no previous generation; it is kept as {} and the store starts empty", S::NAME, copy.display());
            Ok(Opened { value: S::default(), found: Found::Empty { broken: copy } })
        }
        (kept, _) => {
            log::error!("{} is damaged ({why}) and has no previous generation; what needs it stops until it is repaired", S::NAME);
            Err(StoreError::Damaged { store: S::NAME, why, kept })
        }
    }
}

/// A previous generation, when it is whole and of a schema this build reads.
fn read_generation<S: Store>(path: &Path) -> Option<S> {
    let bytes = std::fs::read(path).ok()?;
    let (version, data) = open_envelope(S::NAME, &bytes).ok()?;
    match version.cmp(&S::VERSION) {
        std::cmp::Ordering::Less => S::migrate(version, data).ok(),
        std::cmp::Ordering::Equal => serde_json::from_value(data).ok(),
        std::cmp::Ordering::Greater => None,
    }
}

/// The schema version and the data of a store's file, or why it is damaged.
fn open_envelope(name: &str, bytes: &[u8]) -> Result<(u32, serde_json::Value), String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    let serde_json::Value::Object(mut fields) = value else { return Err("not a store's file".into()) };
    match fields.get("store").and_then(serde_json::Value::as_str) {
        Some(store) if store == name => {}
        Some(store) => return Err(format!("it is the file of {store}")),
        None => return Err("not a store's file".into()),
    }
    let version = fields.get("version").and_then(serde_json::Value::as_u64).and_then(|v| u32::try_from(v).ok());
    match (version, fields.remove("data")) {
        (Some(version), Some(data)) if version > 0 => Ok((version, data)),
        _ => Err("no schema version or no data".into()),
    }
}

/// `<file><suffix>`, beside it.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

/// Copy damaged bytes beside the file as `<file>.broken`; when an earlier, different copy is
/// there, as `<file>.broken.2`, `.broken.3` and so on. The same damage seen again reuses its copy.
/// None when no copy could be written; then not even an [`OnDamage::Empty`] store starts empty, as
/// the next save would lose the file.
fn keep_copy(path: &Path, suffix: &str, bytes: &[u8]) -> Option<PathBuf> {
    for n in 1..=50 {
        let copy = sibling(path, &if n == 1 { suffix.to_string() } else { format!("{suffix}.{n}") });
        match std::fs::read(&copy) {
            Ok(earlier) if earlier == bytes => return Some(copy),
            Ok(_) => continue,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                log::error!("could not keep a copy of a damaged file as {} ({err})", copy.display());
                return None;
            }
        }
        return match write_private(&copy, bytes) {
            Ok(()) => Some(copy),
            Err(err) => {
                log::error!("could not keep a copy of a damaged file as {} ({err})", copy.display());
                None
            }
        };
    }
    log::error!("{} has 50 damaged copies beside it already; no more are kept", path.display());
    None
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn encode<S: Store>(value: &S) -> Result<Vec<u8>, StoreError> {
    let envelope = Envelope { store: S::NAME, version: S::VERSION, data: value };
    serde_json::to_vec_pretty(&envelope).map_err(|err| StoreError::Encode { store: S::NAME, why: err.to_string() })
}

fn save_at<S: Store>(value: &S, path: &Path) -> Result<(), StoreError> {
    let failed = |source| StoreError::Io { store: S::NAME, path: path.to_path_buf(), source };
    // The file there now: never written over when newer, and kept as the previous generation
    // when whole (a damaged one is not: the generation before it stays).
    let now = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(failed(err)),
    };
    let keep = match now.as_deref().map(|bytes| (bytes, open_envelope(S::NAME, bytes))) {
        Some((_, Ok((version, _)))) if version > S::VERSION => {
            return Err(StoreError::Newer { store: S::NAME, found: version, known: S::VERSION });
        }
        Some((bytes, Ok(_))) => Some(bytes),
        _ => None,
    };
    let bytes = encode(value)?;
    let temp = write_temp(path, &bytes).map_err(failed)?;
    if let Some(previous) = keep {
        let generation = sibling(path, ".prev");
        let kept = write_temp(&generation, previous).map_err(failed)?;
        #[cfg(test)]
        tests::fault("prev").map_err(failed)?;
        kept.persist(&generation).map_err(|err| failed(err.error))?;
    }
    #[cfg(test)]
    tests::pause_before_rename();
    #[cfg(test)]
    tests::fault("rename").map_err(failed)?;
    temp.persist(path).map_err(|err| failed(err.error))?;
    sync_folder(path).map_err(failed)
}

/// Write `value` to `path` only when there is no file there yet, in one step: true when this
/// call made it, false when one was there (made by another thread or process meanwhile).
fn create_at<S: Store>(value: &S, path: &Path) -> Result<bool, StoreError> {
    let bytes = encode(value)?;
    let failed = |source| StoreError::Io { store: S::NAME, path: path.to_path_buf(), source };
    let temp = write_temp(path, &bytes).map_err(failed)?;
    match temp.persist_noclobber(path) {
        Ok(_) => sync_folder(path).map(|()| true).map_err(failed),
        Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(err) => Err(failed(err.error)),
    }
}

/// The new bytes in a temporary file beside `path` (only the owner can read it), synced to disk.
/// On an error the temporary file is removed.
fn write_temp(path: &Path, bytes: &[u8]) -> std::io::Result<tempfile::NamedTempFile> {
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file's path"));
    };
    if !folder.is_dir() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        // Authority data: only the person can look into its folder.
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(folder)?;
    }
    let mut temp = tempfile::Builder::new().prefix(&format!(".{}.", name.to_string_lossy())).suffix(".tmp").tempfile_in(folder)?;
    #[cfg(test)]
    tests::fault("write")?;
    temp.write_all(bytes)?;
    #[cfg(test)]
    tests::fault("sync")?;
    temp.as_file().sync_all()?;
    Ok(temp)
}

/// Make the rename itself last through a power cut (macOS and Linux; Windows has no such call). A
/// file system that cannot sync a folder at all is not an error.
fn sync_folder(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    tests::fault("folder")?;
    #[cfg(unix)]
    if let Some(folder) = path.parent() {
        match std::fs::File::open(folder).and_then(|folder| folder.sync_all()) {
            Err(err) if !matches!(err.kind(), std::io::ErrorKind::InvalidInput | std::io::ErrorKind::Unsupported) => return Err(err),
            _ => {}
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

// ----- the machine id --------------------------------------------------------------------------

/// `authority/machine.json`. It refuses when damaged: an id that changed by itself would make this
/// machine a stranger to its server and start a second words folder.
#[derive(Serialize, Deserialize)]
struct MachineFile {
    id: MachineId,
}

/// A new machine's file, with a new random id.
impl Default for MachineFile {
    fn default() -> Self {
        let mut bytes = [0u8; 8];
        crate::sys::random_bytes(&mut bytes);
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        MachineFile { id: MachineId::parse(&hex).expect("hex digits are a folder name") }
    }
}

impl Store for MachineFile {
    const NAME: &'static str = "authority/machine";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;

    fn path() -> PathBuf {
        paths::authority_dir().join("machine.json")
    }
}

impl MachineId {
    /// This machine's id: made at the first ask and kept in `authority/machine.json` of its
    /// Glasscode folder, so the Mac and the server (glasscoded) each have their own. It reads the
    /// disk: never on the main thread.
    pub fn this() -> Result<MachineId, StoreError> {
        machine_id_at(&MachineFile::path())
    }
}

fn machine_id_at(path: &Path) -> Result<MachineId, StoreError> {
    let opened = open_at::<MachineFile>(path)?;
    if opened.found != Found::Missing {
        return Ok(opened.value.id);
    }
    let new = opened.value;
    if create_at(&new, path)? {
        return Ok(new.id);
    }
    // Another thread or process made it first: theirs it is.
    match open_at::<MachineFile>(path)? {
        Opened { found: Found::Missing, .. } => Err(StoreError::Io { store: MachineFile::NAME, path: path.to_path_buf(), source: std::io::ErrorKind::NotFound.into() }),
        Opened { value, .. } => Ok(value.id),
    }
}

// ----- the engine switch -----------------------------------------------------------------------

/// Which manager carries out actions: v1 (until slice M56) or v2. Never both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    #[default]
    V1,
    V2,
}

/// `authority/engine.json` (rule 2 of the slice plan): the engine, and the v2 features that change
/// what a person sees, each off until it is switched on. Older builds never read or write this
/// file, and `state.json` does not change with it. It refuses when damaged, and then
/// [`EngineSwitch::current`] is v1 with every feature off.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineSwitch {
    pub engine: Engine,
    /// The v2 features by name, such as "brief" (M20.2), "profiles" (M22) and "turn_hooks" (M30).
    /// A feature that is not here is off, and none counts while the engine is v1.
    pub switches: BTreeMap<String, bool>,
}

impl Store for EngineSwitch {
    const NAME: &'static str = "authority/engine";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;

    fn path() -> PathBuf {
        paths::authority_dir().join("engine.json")
    }
}

/// `GLASSCODE_MANAGER_ENGINE=v2` runs v2 without the file, in a scratch home only (any Glasscode
/// folder but the person's own `~/.glasscode`); the file is not written.
pub const ENGINE_VARIABLE: &str = "GLASSCODE_MANAGER_ENGINE";

impl EngineSwitch {
    /// The switch in effect: the file's, or v1 with every feature off when there is none or it
    /// cannot be read; then [`ENGINE_VARIABLE`]. It reads the disk: never on the main thread.
    pub fn current() -> EngineSwitch {
        let file = EngineSwitch::load().unwrap_or_else(|err| {
            log::warn!("manager: {err}; running v1 with every v2 feature off");
            EngineSwitch::default()
        });
        let scratch = paths::root() != paths::home().join(".glasscode");
        file.with_variable(std::env::var(ENGINE_VARIABLE).ok().as_deref(), scratch)
    }

    fn with_variable(mut self, value: Option<&str>, scratch: bool) -> EngineSwitch {
        if scratch && value == Some("v2") {
            self.engine = Engine::V2;
        }
        self
    }

    /// Whether a v2 feature is on: the engine is v2 and the feature is switched on.
    pub fn is_on(&self, feature: &str) -> bool {
        self.engine == Engine::V2 && self.switches.get(feature).copied().unwrap_or(false)
    }
}

// ----- the data map ----------------------------------------------------------------------------

/// One row of the data map (design section 6): what a store is, who writes it and what happens
/// to it. `glasscode manager doctor` and the tests read it.
#[derive(Clone, Copy, Debug)]
pub struct DataRow {
    /// The file or folder, under the Glasscode folder.
    pub path: &'static str,
    /// Its [`Store::NAME`], when it is a [`Store`].
    pub store: Option<&'static str>,
    pub module: &'static str,
    pub writer: &'static str,
    /// The format and, for a [`Store`], its schema version (`schema v1`).
    pub format: &'static str,
    pub retention: &'static str,
    /// For a [`Store`] it starts with "refuse" or "empty", as its [`OnDamage`].
    pub on_damage: &'static str,
    /// Whether an export (and a backup) has it.
    pub export: &'static str,
    pub local_only: &'static str,
    pub forget: &'static str,
    pub sync: &'static str,
}

/// The data map. Every [`Store`] has a row (a test fails without one); a slice that adds a store
/// adds its row, and design section 6 follows this table.
pub const DATA_MAP: &[DataRow] = &[
    DataRow {
        path: "state.json",
        store: None,
        module: "store",
        writer: "the app and glasscoded",
        format: "JSON, no envelope (older builds read it)",
        retention: "lasting",
        on_damage: "today's rule (a .broken copy and a fresh start); the manager part refuses",
        export: "yes",
        local_only: "—",
        forget: "—",
        sync: "through `mirror`",
    },
    DataRow {
        path: "authority/machine.json",
        store: Some("authority/machine"),
        module: "store",
        writer: "the app or glasscoded, once, on its own machine",
        format: "JSON, schema v1",
        retention: "lasting",
        on_damage: "refuse; what needs the machine id (sync, the words ledger) stops",
        export: "no",
        local_only: "yes: each machine has its own",
        forget: "—",
        sync: "never",
    },
    DataRow {
        path: "authority/engine.json",
        store: Some("authority/engine"),
        module: "store",
        writer: "the app only (Settings)",
        format: "JSON, schema v1",
        retention: "lasting, until M56 removes v1",
        on_damage: "refuse; v1 runs with every v2 feature off",
        export: "no",
        local_only: "yes",
        forget: "—",
        sync: "no",
    },
];

/// A [`Store`] for the data map, the doctor and the tests, without its type.
#[derive(Clone, Copy)]
pub struct StoreInfo {
    pub name: &'static str,
    pub version: u32,
    pub on_damage: OnDamage,
    pub path: fn() -> PathBuf,
    /// Open it as [`Store::load`] does, and say where it was found.
    pub check: fn() -> Result<Found, StoreError>,
    /// Save its default (the fault tests).
    #[cfg(test)]
    reset: fn() -> Result<(), StoreError>,
}

impl StoreInfo {
    pub const fn of<S: Store>() -> StoreInfo {
        StoreInfo {
            name: S::NAME,
            version: S::VERSION,
            on_damage: S::ON_DAMAGE,
            path: S::path,
            check: check::<S>,
            #[cfg(test)]
            reset: reset::<S>,
        }
    }
}

fn check<S: Store>() -> Result<Found, StoreError> {
    open::<S>().map(|opened| opened.found)
}

#[cfg(test)]
fn reset<S: Store>() -> Result<(), StoreError> {
    S::default().save()
}

/// Every [`Store`]: a slice that adds one adds its line here (a test fails without it).
pub const ALL_STORES: &[StoreInfo] = &[StoreInfo::of::<MachineFile>(), StoreInfo::of::<EngineSwitch>()];

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::time::{Duration, Instant};

    use super::*;

    /// A Glasscode folder of the test's own, with no engine variable, until dropped.
    struct Home {
        dir: tempfile::TempDir,
        before: Vec<(&'static str, Option<OsString>)>,
    }

    impl Home {
        fn new() -> Home {
            let dir = tempfile::tempdir().unwrap();
            let before = ["GLASSCODE_HOME", ENGINE_VARIABLE].map(|name| (name, std::env::var_os(name))).to_vec();
            // SAFETY: the core tests run one at a time (`--test-threads=1`).
            unsafe {
                std::env::set_var("GLASSCODE_HOME", dir.path());
                std::env::remove_var(ENGINE_VARIABLE);
            }
            assert_eq!(paths::root(), dir.path());
            Home { dir, before }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            for (name, value) in self.before.drain(..) {
                // SAFETY: as in `new`.
                match value {
                    Some(value) => unsafe { std::env::set_var(name, value) },
                    None => unsafe { std::env::remove_var(name) },
                }
            }
        }
    }

    /// A store that starts empty when damaged.
    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Notes {
        lines: Vec<String>,
    }

    impl Store for Notes {
        const NAME: &'static str = "test/notes";
        const VERSION: u32 = 1;
        const ON_DAMAGE: OnDamage = OnDamage::Empty;

        fn path() -> PathBuf {
            paths::root().join("test").join("notes.json")
        }
    }

    /// A refusing store at its second schema; the first had `n` for `count`.
    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Counter {
        count: u64,
        pad: String,
    }

    impl Store for Counter {
        const NAME: &'static str = "test/counter";
        const VERSION: u32 = 2;
        const ON_DAMAGE: OnDamage = OnDamage::Refuse;

        fn path() -> PathBuf {
            paths::root().join("test").join("counter.json")
        }

        fn migrate(from: u32, value: serde_json::Value) -> Result<Self, StoreError> {
            match (from, value["n"].as_u64()) {
                (1, Some(n)) => Ok(Counter { count: n, pad: String::new() }),
                _ => Err(StoreError::Migrate { store: Self::NAME, from, why: "no n".into() }),
            }
        }
    }

    fn broken(path: &Path) -> PathBuf {
        let mut copy = path.as_os_str().to_owned();
        copy.push(".broken");
        copy.into()
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn copies(path: &Path) -> Vec<String> {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut found: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|file| file.starts_with(&format!("{name}.broken")))
            .collect();
        found.sort();
        found
    }

    #[test]
    fn refuse_store_keeps_broken_copy_and_errs() {
        let _home = Home::new();
        // The machine id: a file cut short.
        let path = MachineFile::path();
        let damaged_text = r#"{"store":"authority/machine","version":1,"data":{"id":"#;
        write(&path, damaged_text);
        for _ in 0..2 {
            match MachineId::this() {
                Err(StoreError::Damaged { store: "authority/machine", kept: Some(kept), .. }) => {
                    assert_eq!(kept, broken(&path));
                    assert_eq!(std::fs::read_to_string(&kept).unwrap(), damaged_text);
                }
                other => panic!("{other:?}"),
            }
            // Left in place, so it refuses again rather than starting empty; one copy.
            assert_eq!(std::fs::read_to_string(&path).unwrap(), damaged_text);
            assert_eq!(copies(&path), ["machine.json.broken"]);
        }
        // An id that is no folder name, and another store's file, are damage too.
        for text in [r#"{"store":"authority/machine","version":1,"data":{"id":"../../x"}}"#, r#"{"store":"authority/engine","version":1,"data":{"id":"abc"}}"#] {
            write(&path, text);
            assert!(matches!(MachineId::this(), Err(StoreError::Damaged { kept: Some(_), .. })), "{text}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        // An earlier, different copy is not overwritten.
        assert_eq!(copies(&path).len(), 3, "{:?}", copies(&path));
        assert_eq!(std::fs::read_to_string(broken(&path)).unwrap(), damaged_text);

        // The engine switch refuses too: v1 with every feature off, and the file as it was.
        let engine = EngineSwitch::path();
        write(&engine, "{ not json");
        assert!(matches!(EngineSwitch::load(), Err(StoreError::Damaged { store: "authority/engine", .. })));
        assert_eq!(EngineSwitch::current(), EngineSwitch::default());
        assert_eq!(std::fs::read_to_string(&engine).unwrap(), "{ not json");
        assert_eq!(std::fs::read_to_string(broken(&engine)).unwrap(), "{ not json");

        // A refusing store of a test: damaged data under a good envelope.
        write(&Counter::path(), r#"{"store":"test/counter","version":2,"data":{"count":"many"}}"#);
        assert!(matches!(Counter::load(), Err(StoreError::Damaged { .. })));
        assert!(broken(&Counter::path()).exists());
    }

    #[test]
    fn empty_store_starts_fresh_on_damage() {
        let _home = Home::new();
        let path = Notes::path();
        write(&path, "half a fi");
        assert_eq!(Notes::load().unwrap(), Notes::default());
        assert_eq!(std::fs::read_to_string(broken(&path)).unwrap(), "half a fi");
        // The next save replaces it, and the copy stays.
        let notes = Notes { lines: vec!["one".into()] };
        notes.save().unwrap();
        assert_eq!(Notes::load().unwrap(), notes);
        assert_eq!(std::fs::read_to_string(broken(&path)).unwrap(), "half a fi");
        // Without the envelope it is damaged too; the first copy is kept beside the second.
        write(&path, r#"{"lines":["two"]}"#);
        assert_eq!(Notes::load().unwrap(), Notes::default());
        assert_eq!(copies(&path).len(), 2, "{:?}", copies(&path));

        // Not damage: no file is the default, and a file that cannot be read is an error.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(Notes::load().unwrap(), Notes::default());
        std::fs::create_dir(&path).unwrap();
        assert!(matches!(Notes::load(), Err(StoreError::Io { .. })), "a folder where the file goes");
        std::fs::remove_dir(&path).unwrap();

        // A newer schema is neither read, nor copied, nor started empty, nor written over: an
        // older build leaves it alone.
        let newer = r#"{"store":"test/notes","version":7,"data":{"lines":[]}}"#;
        write(&path, newer);
        assert!(matches!(Notes::load(), Err(StoreError::Newer { found: 7, known: 1, .. })));
        assert!(matches!(Notes::default().save(), Err(StoreError::Newer { found: 7, .. })));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
        assert_eq!(copies(&path).len(), 2);
    }

    /// The damage of the fault tests: cut short, empty, and not JSON.
    fn damages(good: &[u8]) -> [(&'static str, Vec<u8>); 3] {
        [("truncated", good[..good.len() / 2].to_vec()), ("zero-length", Vec::new()), ("invalid JSON", b"{ \"store\": nope".to_vec())]
    }

    #[test]
    fn a_damaged_file_falls_back_to_its_previous_generation() {
        let _home = Home::new();
        let path = Notes::path();
        let first = Notes { lines: vec!["first".into()] };
        let second = Notes { lines: vec!["second".into()] };
        first.save().unwrap();
        assert!(!sibling(&path, ".prev").exists(), "nothing before the first save");
        second.save().unwrap();
        assert_eq!(read_generation::<Notes>(&sibling(&path, ".prev")), Some(Notes { lines: vec!["first".into()] }));
        let good = std::fs::read(&path).unwrap();
        for (damage, bytes) in damages(&good) {
            std::fs::write(&path, &bytes).unwrap();
            let opened = open::<Notes>().unwrap();
            assert_eq!(opened.value, first, "{damage}");
            assert!(matches!(&opened.found, Found::Previous { broken: Some(copy) } if std::fs::read(copy).unwrap() == bytes), "{damage}: {:?}", opened.found);
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "{damage}: the file is left as it is");
        }
        // A save over a damaged file does not make the damage the previous generation.
        let third = Notes { lines: vec!["third".into()] };
        third.save().unwrap();
        assert_eq!((Notes::load().unwrap(), read_generation::<Notes>(&sibling(&path, ".prev"))), (third, Some(first)));

        // The same for a refusing store: the previous generation, not a refusal.
        Counter { count: 1, pad: String::new() }.save().unwrap();
        Counter { count: 2, pad: String::new() }.save().unwrap();
        std::fs::write(Counter::path(), "").unwrap();
        assert_eq!(Counter::load().unwrap().count, 1);
    }

    /// The point of a save where the next one fails (`fault`), until dropped.
    static FAIL_AT: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

    struct FailAt;

    impl FailAt {
        fn new(point: &'static str) -> FailAt {
            *FAIL_AT.lock().unwrap() = Some(point);
            FailAt
        }
    }

    impl Drop for FailAt {
        fn drop(&mut self) {
            *FAIL_AT.lock().unwrap() = None;
        }
    }

    /// In `save_at`, `write_temp` and `sync_folder`: the injected failure of this point, if any.
    /// "write" fails as a full disk does.
    pub(super) fn fault(point: &'static str) -> std::io::Result<()> {
        if *FAIL_AT.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) != Some(point) {
            return Ok(());
        }
        let kind = if point == "write" { std::io::ErrorKind::StorageFull } else { std::io::ErrorKind::Other };
        Err(std::io::Error::new(kind, format!("injected failure at {point}")))
    }

    /// The temporary files of a save of `path` left in its folder.
    fn temporary_files(path: &Path) -> Vec<String> {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut found: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|file| file.starts_with(&format!(".{name}.")) && file.ends_with(".tmp"))
            .collect();
        found.sort();
        found
    }

    /// Fault injection for every store of `ALL_STORES`, and for the two test stores (so both
    /// rules on damage are run): a damaged file in three ways, with and without its previous
    /// generation; a newer schema; and a save that fails at each step.
    #[test]
    fn fault_injection_for_every_store() {
        let _home = Home::new();
        let stores = ALL_STORES.iter().copied().chain([StoreInfo::of::<Notes>(), StoreInfo::of::<Counter>()]);
        for store in stores {
            let name = store.name;
            let path = (store.path)();
            let previous = sibling(&path, ".prev");
            let aside = sibling(&path, ".prev.aside");
            (store.reset)().unwrap();
            (store.reset)().unwrap();
            assert!(previous.is_file(), "{name}: the previous generation");
            let good = std::fs::read(&path).unwrap();

            for (damage, bytes) in damages(&good) {
                std::fs::write(&path, &bytes).unwrap();
                assert!(matches!((store.check)(), Ok(Found::Previous { .. })), "{name}, {damage}: {:?}", (store.check)());
                // Without the previous generation, the store's own rule.
                std::fs::rename(&previous, &aside).unwrap();
                match ((store.check)(), store.on_damage) {
                    (Ok(Found::Empty { broken }), OnDamage::Empty) => assert_eq!(std::fs::read(broken).unwrap(), bytes, "{name}, {damage}"),
                    (Err(StoreError::Damaged { kept: Some(copy), .. }), OnDamage::Refuse) => assert_eq!(std::fs::read(copy).unwrap(), bytes, "{name}, {damage}"),
                    (other, rule) => panic!("{name}, {damage}, {rule:?}: {other:?}"),
                }
                assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name}, {damage}: left as it is");
                std::fs::rename(&aside, &previous).unwrap();
            }

            // A newer schema: not read, not written over, and no generation made of it.
            let newer = format!(r#"{{"store":"{name}","version":{},"data":{{}}}}"#, store.version + 1);
            std::fs::write(&path, &newer).unwrap();
            let generation = std::fs::read(&previous).unwrap();
            assert!(matches!((store.check)(), Err(StoreError::Newer { .. })), "{name}");
            assert!(matches!((store.reset)(), Err(StoreError::Newer { .. })), "{name}");
            assert_eq!((std::fs::read_to_string(&path).unwrap(), std::fs::read(&previous).unwrap()), (newer, generation), "{name}");

            // A save that fails at each step (a full disk, a failed sync or rename): an error,
            // the file as it was when the failure is before the rename, no temporary file left.
            std::fs::remove_file(&path).unwrap();
            (store.reset)().unwrap();
            for point in ["write", "sync", "prev", "rename", "folder"] {
                let before = std::fs::read(&path).unwrap();
                let failed = {
                    let _fail = FailAt::new(point);
                    (store.reset)()
                };
                assert!(matches!(&failed, Err(StoreError::Io { .. })), "{name}, {point}: {failed:?}");
                if point != "folder" {
                    assert_eq!(std::fs::read(&path).unwrap(), before, "{name}, {point}");
                }
                assert!(temporary_files(&path).is_empty(), "{name}, {point}: {:?}", temporary_files(&path));
                assert!(matches!((store.check)(), Ok(Found::File)), "{name}, {point}");
            }
        }
        assert!(fault("write").is_ok(), "no failure once its guard is gone");
    }

    /// The `NAME` of each `impl … Store for …` in the crate's source, but the tests' own stores.
    fn stores_in_the_source() -> std::collections::BTreeSet<String> {
        fn walk(dir: &Path, found: &mut std::collections::BTreeSet<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, found);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let mut at = 0;
                for line in text.split_inclusive('\n') {
                    let start = at;
                    at += line.len();
                    let line = line.trim();
                    if !(line.starts_with("impl") && (line.contains(" Store for ") || line.contains("::Store for "))) {
                        continue;
                    }
                    let marker = "const NAME: &'static str = \"";
                    let after = &text[start..];
                    let from = after.find(marker).unwrap_or_else(|| panic!("{}: a Store without NAME: {line}", path.display())) + marker.len();
                    let name = &after[from..from + after[from..].find('"').unwrap()];
                    if !name.starts_with("test/") {
                        found.insert(name.to_string());
                    }
                }
            }
        }
        let mut found = std::collections::BTreeSet::new();
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut found);
        found
    }

    #[test]
    fn every_store_has_a_data_map_row() {
        use std::collections::BTreeSet;
        let _home = Home::new();
        // Every store of the source is in ALL_STORES, and only those.
        let listed: BTreeSet<String> = ALL_STORES.iter().map(|store| store.name.to_string()).collect();
        assert_eq!(stores_in_the_source(), listed, "a Store missing from ALL_STORES, or one listed that is gone");
        for store in ALL_STORES {
            let row = DATA_MAP.iter().find(|row| row.store == Some(store.name)).unwrap_or_else(|| panic!("{} has no row in DATA_MAP", store.name));
            assert_eq!(paths::root().join(row.path), (store.path)(), "{}: the row's path", store.name);
            assert!(row.format.contains(&format!("schema v{}", store.version)), "{}: {}", store.name, row.format);
            let rule = match store.on_damage {
                OnDamage::Empty => "empty",
                OnDamage::Refuse => "refuse",
            };
            assert!(row.on_damage.starts_with(rule), "{}: {}", store.name, row.on_damage);
            assert!(paths::class_of(&(store.path)()).is_some(), "{} has no path class", store.name);
        }
        // No row of a store that is gone, one row per path, and state.json has its row.
        for row in DATA_MAP {
            assert!(row.store.is_none_or(|name| listed.contains(name)), "{}: no such store", row.path);
            assert_eq!(DATA_MAP.iter().filter(|other| other.path == row.path).count(), 1, "{}", row.path);
        }
        assert!(DATA_MAP.iter().any(|row| row.path == "state.json" && row.store.is_none()));
    }

    #[test]
    fn an_older_schema_is_migrated_and_a_failed_migration_refuses() {
        let _home = Home::new();
        let path = Counter::path();
        write(&path, r#"{"store":"test/counter","version":1,"data":{"n":5}}"#);
        assert_eq!(Counter::load().unwrap(), Counter { count: 5, pad: String::new() });
        Counter::load().unwrap().save().unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved, serde_json::json!({ "store": "test/counter", "version": 2, "data": { "count": 5, "pad": "" } }));
        // A migration that fails leaves the file as it is, with a copy.
        let old = r#"{"store":"test/counter","version":1,"data":{}}"#;
        write(&path, old);
        assert!(matches!(Counter::load(), Err(StoreError::Migrate { from: 1, .. })));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
        assert_eq!(std::fs::read_to_string(broken(&path)).unwrap(), old);
        write(&Notes::path(), r#"{"store":"test/notes","version":0,"data":{}}"#);
        assert!(Notes::load().is_ok(), "version 0 is no schema: damaged, so empty");
    }

    /// The test program, started again by `save_is_atomic_under_kill`: it saves to the file it is
    /// given until it is killed. Without the variable (a person runs `--ignored`) it does nothing.
    #[test]
    #[ignore = "started by save_is_atomic_under_kill"]
    fn store_child_saves_until_killed() {
        let Some(mode) = std::env::var_os("GLASSCODE_TEST_STORE_CHILD") else { return };
        let ready = paths::root().join("child-ready");
        let mut round = 0u64;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            // Big enough that a write takes a while.
            let counter = Counter { count: round % 2, pad: (if round.is_multiple_of(2) { "a" } else { "b" }).repeat(400_000) };
            if mode == "pause" {
                // Stops in `save_at` after the temporary file is synced and before the rename.
                // SAFETY: this program runs this test only.
                unsafe { std::env::set_var("GLASSCODE_TEST_STORE_PAUSE", &ready) };
            }
            counter.save().unwrap();
            if round == 0 {
                std::fs::write(&ready, "").unwrap();
            }
            round += 1;
        }
        // Not killed in time: end without saving more, so the parent sees it.
        std::process::exit(3);
    }

    /// In `save_at`, between the synced temporary file and the rename: a child of
    /// `save_is_atomic_under_kill` says so in the file the variable names, and waits to be killed.
    pub(super) fn pause_before_rename() {
        if let Some(ready) = std::env::var_os("GLASSCODE_TEST_STORE_PAUSE") {
            std::fs::write(ready, "").unwrap();
            std::thread::sleep(Duration::from_secs(30));
            std::process::exit(3);
        }
    }

    fn start_child(home: &Path, mode: &str) -> std::process::Child {
        let _ = std::fs::remove_file(home.join("child-ready"));
        let mut child = crate::sys::command(std::env::current_exe().unwrap());
        child.args(["store::tests::store_child_saves_until_killed", "--exact", "--ignored", "--test-threads=1"]);
        child.env("GLASSCODE_TEST_STORE_CHILD", mode).env("GLASSCODE_HOME", home).env_remove("GLASSCODE_TEST_STORE_PAUSE");
        child.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let mut child = child.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while !home.join("child-ready").exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("the child ended first: {status}");
            }
            assert!(Instant::now() < deadline, "the child never got ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        child
    }

    #[test]
    fn save_is_atomic_under_kill() {
        let home = Home::new();
        let path = Counter::path();
        let before = Counter { count: 41, pad: "before".into() };
        before.save().unwrap();

        // Killed between the synced temporary file and the rename: the file is the old one.
        let mut child = start_child(home.path(), "pause");
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(Counter::load().unwrap(), before);
        let left: Vec<String> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".tmp")).collect();
        assert_eq!(left.len(), 1, "the child's temporary file: {left:?}");

        // Killed at any moment of saves one after another: always one whole save or the other.
        for wait in [0, 3, 7, 13, 23, 37] {
            let mut child = start_child(home.path(), "loop");
            std::thread::sleep(Duration::from_millis(wait));
            child.kill().unwrap();
            child.wait().unwrap();
            let counter = Counter::load().unwrap();
            let whole = match counter.count {
                0 => "a".repeat(400_000),
                1 => "b".repeat(400_000),
                other => panic!("count {other}"),
            };
            assert!(counter.pad == whole, "a save cut short (killed after {wait} ms)");
        }
        // A later save still works over what the children left.
        before.save().unwrap();
        assert_eq!(Counter::load().unwrap(), before);
    }

    #[test]
    fn machine_id_is_stable() {
        let home = Home::new();
        let id = MachineId::this().unwrap();
        assert_eq!(MachineId::parse(id.as_str()), Some(id.clone()));
        assert_eq!(id.as_str().len(), 16);
        let file = home.path().join("authority").join("machine.json");
        assert!(file.is_file());
        assert_eq!(paths::class_of(&file), Some(paths::PathClass::Authority));
        assert_eq!(paths::words_dir(&id), home.path().join("words").join(id.as_str()));
        // Asked again, it is read back, not made again.
        for _ in 0..3 {
            assert_eq!(MachineId::this().unwrap(), id);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(file.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        drop(home);

        // Asked by many at once on a new machine: one id for all.
        let home = Home::new();
        let path = MachineFile::path();
        let ids: Vec<MachineId> = std::thread::scope(|scope| {
            let asks: Vec<_> = (0..8).map(|_| scope.spawn(|| machine_id_at(&path).unwrap())).collect();
            asks.into_iter().map(|ask| ask.join().unwrap()).collect()
        });
        assert!(ids.iter().all(|other| *other == ids[0]), "{ids:?}");
        assert_eq!(MachineId::this().unwrap(), ids[0]);
        // Another Glasscode folder (another machine) has its own.
        assert_ne!(ids[0], id);
        drop(home);
    }

    #[test]
    fn engine_defaults_to_v1() {
        let home = Home::new();
        assert_eq!(EngineSwitch::load().unwrap(), EngineSwitch::default());
        let current = EngineSwitch::current();
        assert_eq!((current.engine, current.switches.len()), (Engine::V1, 0));
        for feature in ["brief", "profiles", "turn_hooks"] {
            assert!(!current.is_on(feature), "{feature}");
        }
        assert!(!home.path().join("authority").exists(), "reading writes nothing");

        // A switch counts only under v2.
        let mut switch = EngineSwitch { engine: Engine::V1, switches: BTreeMap::from([("brief".to_string(), true)]) };
        assert!(!switch.is_on("brief"));
        switch.engine = Engine::V2;
        assert!(switch.is_on("brief") && !switch.is_on("profiles"));

        // The variable: exactly "v2", in a scratch home only, and the file is not written.
        let v1 = EngineSwitch::default();
        assert_eq!(v1.clone().with_variable(Some("v2"), true).engine, Engine::V2);
        assert_eq!(v1.clone().with_variable(Some("v2"), false).engine, Engine::V1, "the person's own home");
        assert_eq!(v1.clone().with_variable(Some("V2"), true).engine, Engine::V1);
        assert_eq!(v1.with_variable(None, true).engine, Engine::V1);
        // SAFETY: the core tests run one at a time; `Home` puts it back.
        unsafe { std::env::set_var(ENGINE_VARIABLE, "v2") };
        assert_eq!(EngineSwitch::current().engine, Engine::V2);
        assert!(!EngineSwitch::path().exists());

        let saved: serde_json::Value = {
            EngineSwitch::default().save().unwrap();
            serde_json::from_slice(&std::fs::read(EngineSwitch::path()).unwrap()).unwrap()
        };
        assert_eq!(saved, serde_json::json!({ "store": "authority/engine", "version": 1, "data": { "engine": "v1", "switches": {} } }));
    }

    #[test]
    fn older_state_json_untouched_by_engine_switch() {
        let home = Home::new();
        // A state.json as a build before Manager v2 writes it, with the v1 manager on.
        let older = r#"{
  "projects": [
    { "id": "p1", "name": "app", "root": "/work/app", "expanded": true }
  ],
  "accounts": [],
  "sessions": [],
  "manager": { "enabled": true, "name": "Manager", "brain": "claude" },
  "first_run_done": true
}"#;
        let state_file = paths::state_file();
        write(&state_file, older);

        let mut switch = EngineSwitch::current();
        assert_eq!(switch.engine, Engine::V1);
        switch.engine = Engine::V2;
        switch.switches.insert("brief".into(), true);
        switch.save().unwrap();
        assert!(EngineSwitch::current().is_on("brief"));

        // state.json is as it was, byte for byte, and reads as before.
        assert_eq!(std::fs::read_to_string(&state_file).unwrap(), older);
        let state = load();
        assert_eq!((state.projects.len(), state.manager.enabled, state.first_run_done), (1, true, true));
        // Nothing of the switch is in it, and saving it keeps the switch where it is.
        let value = serde_json::to_value(&state).unwrap();
        assert!(value.get("engine").is_none() && value["manager"].get("engine").is_none(), "{value}");
        save(&state).unwrap();
        assert!(!std::fs::read_to_string(&state_file).unwrap().contains("engine"));
        assert!(EngineSwitch::current().is_on("brief"));
        assert_eq!(EngineSwitch::path(), home.path().join("authority").join("engine.json"));
        assert_eq!(paths::class_of(&EngineSwitch::path()), Some(paths::PathClass::Authority));
    }
}
