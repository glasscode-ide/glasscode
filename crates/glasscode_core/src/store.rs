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
/// is repaired and reconciled). A store that fails closed also says [`Store::FALLS_BACK`] false.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnDamage {
    /// Start from the default; the next save replaces the file.
    Empty,
    /// Fail and leave the file as it is, so it fails again on the next start until it is repaired.
    /// It never starts empty, and a file that is gone while its previous generation is there is a
    /// refusal too.
    Refuse,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The file could not be read or written: permissions, a full disk, a failed sync or rename, a
    /// folder in its place. Not damage, so nothing starts empty. From a save it means the save may
    /// not have happened, or may not last: what depended on it does not run.
    #[error("Glasscode could not read or write {store} ({}): {source}", path.display())]
    Io { store: &'static str, path: PathBuf, source: std::io::Error },
    /// The file is damaged (or gone beside its previous generation) and no generation was taken;
    /// `kept` is the copy of it, or the generation left as the evidence, when there is one.
    #[error("{store} is damaged ({why})")]
    Damaged { store: &'static str, why: String, kept: Option<PathBuf> },
    /// A newer Glasscode wrote the file: this build reads it as nothing and never writes it (the
    /// version rule of the data map).
    #[error("{store} was written by a newer Glasscode (schema {found}; this one knows up to {known}), so this one leaves it alone")]
    Newer { store: &'static str, found: u64, known: u32 },
    /// The file has an older schema that could not be brought up to date. It is left as it is.
    #[error("{store} could not be updated from schema {from} ({why})")]
    Migrate { store: &'static str, from: u32, why: String },
    #[error("{store} could not be written ({why})")]
    Encode { store: &'static str, why: String },
}

/// A file of Manager v2 (design section 8). On disk it is `{"store": NAME, "version": VERSION,
/// "data": …}`; a file without that envelope is damaged. Beside it, `<file>.prev` keeps the
/// generation before the last save (or the first one, for a store made once), and `<file>.lock`
/// keeps two saves apart. Every store has a path class (`paths::class_of`), a row in [`DATA_MAP`]
/// and a line in [`ALL_STORES`].
pub trait Store: Default + Serialize + DeserializeOwned {
    /// "authority/policy".
    const NAME: &'static str;
    /// The schema version; an older file goes through [`Store::migrate`].
    const VERSION: u32;
    const ON_DAMAGE: OnDamage;
    /// Whether a damaged file falls back to its previous generation. A store whose older value
    /// may allow more (the engine switch, a policy) says false, and then no way of reading it
    /// ([`Store::load`], [`open`]) ever gives an older generation: it fails closed instead.
    const FALLS_BACK: bool = true;

    fn path() -> PathBuf;

    /// The store, as [`open`] finds it: the default when there is no file (and, for a refusing
    /// store, no previous generation either); the previous generation when the file is damaged
    /// (unless [`Store::FALLS_BACK`] is false); then the default ([`OnDamage::Empty`]) or
    /// [`StoreError::Damaged`] ([`OnDamage::Refuse`]). A newer schema, a failed migration and a
    /// read error are errors for every store. It reads the disk: never on the main thread.
    fn load() -> Result<Self, StoreError> {
        open::<Self>().map(|opened| opened.value)
    }

    /// Under `<file>.lock`: the file there now to `<file>.prev` when it loads (and to `.broken`
    /// when it does not), the new bytes to a temporary file beside it, synced, renamed over it
    /// (`MoveFileEx` replacing it on Windows), and the folder synced, so a reader or a crash never
    /// sees half a file. A file of a newer schema is never written over ([`StoreError::Newer`]).
    /// Any failed write, sync or rename is an error.
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
    /// No file (and, for a refusing store, no previous generation): the default.
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

/// What a store's file holds: the one reading of a file that loading, the previous generation and
/// saving share.
enum Parsed<S> {
    /// It loads: of this schema, or migrated from an older one.
    Loads(S),
    /// A newer schema: this build neither reads nor writes it, whatever else the file holds.
    Newer(u64),
    /// An older schema that could not be migrated.
    Unmigrated(StoreError),
    /// Damaged, and why.
    Damaged(String),
}

fn parse<S: Store>(bytes: &[u8]) -> Parsed<S> {
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(err) => return Parsed::Damaged(err.to_string()),
    };
    let serde_json::Value::Object(mut fields) = value else { return Parsed::Damaged("not a store's file".into()) };
    match fields.get("store").and_then(serde_json::Value::as_str) {
        Some(store) if store == S::NAME => {}
        Some(store) => return Parsed::Damaged(format!("it is the file of {store}")),
        None => return Parsed::Damaged("not a store's file".into()),
    }
    // The version decides first: a newer file is left alone, with or without its data, and
    // whatever kind of number says it (2.0, 1e30, past u64).
    let Some(serde_json::Value::Number(number)) = fields.get("version") else {
        return Parsed::Damaged("no schema version".into());
    };
    let version = match number.as_u64() {
        Some(version) => version,
        // `as` saturates: a version past u64 is u64::MAX.
        None => match number.as_f64() {
            Some(float) if float > f64::from(S::VERSION) => return Parsed::Newer(float as u64),
            _ => return Parsed::Damaged(format!("schema version {number} is not one")),
        },
    };
    if version > u64::from(S::VERSION) {
        return Parsed::Newer(version);
    }
    let Some(data) = fields.remove("data") else { return Parsed::Damaged("no data".into()) };
    match version as u32 {
        0 => Parsed::Damaged("schema 0".into()),
        older if older < S::VERSION => match S::migrate(older, data) {
            Ok(value) => Parsed::Loads(value),
            Err(err) => Parsed::Unmigrated(err),
        },
        _ => match serde_json::from_value(data) {
            Ok(value) => Parsed::Loads(value),
            Err(err) => Parsed::Damaged(err.to_string()),
        },
    }
}

fn open_at<S: Store>(path: &Path) -> Result<Opened<S>, StoreError> {
    let previous = sibling(path, ".prev");
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Not new: something removed the file of a store that was saved before.
            if S::ON_DAMAGE == OnDamage::Refuse && std::fs::symlink_metadata(&previous).is_ok() {
                log::error!("{} is gone, but its previous generation {} is there; what needs it stops until it is repaired", S::NAME, previous.display());
                return Err(StoreError::Damaged { store: S::NAME, why: "the file is gone, but its previous generation is there".into(), kept: Some(previous) });
            }
            return Ok(Opened { value: S::default(), found: Found::Missing });
        }
        Err(source) => return Err(StoreError::Io { store: S::NAME, path: path.to_path_buf(), source }),
    };
    let why = match parse::<S>(&bytes) {
        Parsed::Loads(value) => return Ok(Opened { value, found: Found::File }),
        Parsed::Newer(found) => {
            log::warn!("{} has schema {found}, newer than this build's {}; it is not read and not written", S::NAME, S::VERSION);
            return Err(StoreError::Newer { store: S::NAME, found, known: S::VERSION });
        }
        Parsed::Unmigrated(err) => {
            let kept = keep_copy(path, ".broken", &bytes);
            log::error!("{err}; the file is left as it is{}", kept.map(|k| format!(", and copied to {}", k.display())).unwrap_or_default());
            return Err(err);
        }
        Parsed::Damaged(why) => why,
    };
    // Damaged: keep a copy, then the previous generation, then the store's rule.
    let broken = keep_copy(path, ".broken", &bytes);
    if let Some(value) = S::FALLS_BACK.then(|| read_generation::<S>(&previous)).flatten() {
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

/// A previous generation, when it loads.
fn read_generation<S: Store>(path: &Path) -> Option<S> {
    match parse::<S>(&std::fs::read(path).ok()?) {
        Parsed::Loads(value) => Some(value),
        _ => None,
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
/// None when no copy could be written; then not even an [`OnDamage::Empty`] store starts empty,
/// and no save replaces the file, as the damage would be lost.
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
    make_folder(path).map_err(failed)?;
    // From the read of the file there now to the rename: another save waits.
    let _lock = lock(path).map_err(failed)?;
    save_locked(value, path)
}

/// Read the store `S`, change it and save it, all under `<file>.lock`: two writers of the same
/// store (threads or processes) never lose each other's change, as a load and a save apart would.
/// It reads as [`Store::load`] does; when that is an error (a refusing store that is damaged, a
/// newer schema), `change` is not called and nothing is written. It reads the disk: never on the
/// main thread.
pub fn update<S: Store, R>(change: impl FnOnce(&mut S) -> R) -> Result<R, StoreError> {
    update_at::<S, R>(&S::path(), change)
}

fn update_at<S: Store, R>(path: &Path, change: impl FnOnce(&mut S) -> R) -> Result<R, StoreError> {
    let failed = |source| StoreError::Io { store: S::NAME, path: path.to_path_buf(), source };
    make_folder(path).map_err(failed)?;
    let _lock = lock(path).map_err(failed)?;
    let mut value = open_at::<S>(path)?.value;
    let result = change(&mut value);
    save_locked(&value, path)?;
    Ok(result)
}

/// [`Store::save`] with `<file>.lock` already held by the caller.
fn save_locked<S: Store>(value: &S, path: &Path) -> Result<(), StoreError> {
    let failed = |source| StoreError::Io { store: S::NAME, path: path.to_path_buf(), source };
    let bytes = encode(value)?;
    sweep_temporary_files(path);
    let now = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(failed(err)),
    };
    // A file that loads becomes the previous generation; a newer one is never written over; any
    // other is kept as `.broken` first, and the generation before it stays.
    let keep = match now.as_deref() {
        None => None,
        Some(current) => match parse::<S>(current) {
            Parsed::Loads(_) => Some(current),
            Parsed::Newer(found) => return Err(StoreError::Newer { store: S::NAME, found, known: S::VERSION }),
            Parsed::Unmigrated(_) | Parsed::Damaged(_) => {
                if keep_copy(path, ".broken", current).is_none() {
                    return Err(failed(std::io::Error::other("the damaged file there could not be copied aside")));
                }
                None
            }
        },
    };
    let temp = write_temp(path, &bytes).map_err(failed)?;
    if let Some(previous) = keep {
        let generation = sibling(path, ".prev");
        let kept = write_temp(&generation, previous).map_err(failed)?;
        #[cfg(test)]
        tests::fault("prev").map_err(failed)?;
        rename_into(kept, &generation, true).map_err(failed)?;
    }
    #[cfg(test)]
    tests::pause_before_rename();
    #[cfg(test)]
    tests::fault("rename").map_err(failed)?;
    rename_into(temp, path, true).map_err(failed)?;
    sync_folder(path).map_err(failed)
}

/// Write `value` to `path` only when there is no file there yet, in one step, and its first
/// generation beside it: true when this call made it, false when one was there (made by another
/// thread or process meanwhile).
fn create_at<S: Store>(value: &S, path: &Path) -> Result<bool, StoreError> {
    let failed = |source| StoreError::Io { store: S::NAME, path: path.to_path_buf(), source };
    let bytes = encode(value)?;
    make_folder(path).map_err(failed)?;
    let _lock = lock(path).map_err(failed)?;
    let temp = write_temp(path, &bytes).map_err(failed)?;
    match rename_into(temp, path, false) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(err) => return Err(failed(err)),
    }
    let generation = sibling(path, ".prev");
    let kept = write_temp(&generation, &bytes).map_err(failed)?;
    rename_into(kept, &generation, true).map_err(failed)?;
    sync_folder(path).map(|()| true).map_err(failed)
}

/// The store's folder; a new one only the person can look into (macOS and Linux).
fn make_folder(path: &Path) -> std::io::Result<()> {
    let Some(folder) = path.parent() else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file's path"));
    };
    if folder.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(folder)
}

/// `<file>.lock`, held until dropped: one save of a store at a time, across processes.
fn lock(path: &Path) -> std::io::Result<crate::sys::FileLock> {
    crate::sys::FileLock::take(&sibling(path, ".lock")).map_err(|err| std::io::Error::other(err.to_string()))
}

/// Remove the temporary files of this store's saves that died: under the lock no save of it is
/// running, and one older than ten minutes is not a slow disk.
fn sweep_temporary_files(path: &Path) {
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else { return };
    let prefix = format!(".{}.", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        if !file.starts_with(&prefix) || !file.ends_with(".tmp") {
            continue;
        }
        // File times are the disk's, so the age is too (not the manager's clock).
        let old = entry.metadata().and_then(|meta| meta.modified()).ok().and_then(|at| at.elapsed().ok()).is_some_and(|age| age > std::time::Duration::from_secs(600));
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The new bytes in a temporary file beside `path` (only the owner can read it), synced to disk.
/// On an error the temporary file is removed.
fn write_temp(path: &Path, bytes: &[u8]) -> std::io::Result<tempfile::NamedTempFile> {
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file's path"));
    };
    let mut temp = tempfile::Builder::new().prefix(&format!(".{}.", name.to_string_lossy())).suffix(".tmp").tempfile_in(folder)?;
    #[cfg(test)]
    tests::fault("write")?;
    temp.write_all(bytes)?;
    #[cfg(test)]
    tests::fault("sync")?;
    temp.as_file().sync_all()?;
    Ok(temp)
}

/// Rename the temporary file to `path` in one step, over a file there when `replace`, else
/// failing with `AlreadyExists`: `rename` (or a rename that does not replace) on macOS and Linux;
/// on Windows `MoveFileExW`, writing through, after `tempfile`'s own `keep` has cleared the file's
/// temporary mark (the moved file would keep it). Microsoft documents the flush of
/// `MOVEFILE_WRITE_THROUGH` for a move done as a copy, and Windows has no folder sync, so there a
/// save is atomic but not proven to last a power cut at the moment it returns (design section 7).
fn rename_into(temp: tempfile::NamedTempFile, path: &Path, replace: bool) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        use windows::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW};
        use windows::core::PCWSTR;
        // Kept: not marked temporary, and no longer removed when dropped.
        let kept = temp.into_temp_path().keep().map_err(|err| err.error)?;
        let wide = |path: &Path| path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
        let (from, to) = (wide(&kept), wide(path));
        let flags = if replace { MOVEFILE_WRITE_THROUGH | MOVEFILE_REPLACE_EXISTING } else { MOVEFILE_WRITE_THROUGH };
        // SAFETY: two NUL-terminated paths that live through the call.
        let moved = unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), flags) }.map_err(|error| {
            // An HRESULT made from a Win32 error number is 0x8007 and the number.
            let code = error.code().0 as u32;
            if code >> 16 == 0x8007 { std::io::Error::from_raw_os_error((code & 0xffff) as i32) } else { std::io::Error::other(error) }
        });
        if moved.is_err() {
            // Nothing else removes a kept file.
            let _ = std::fs::remove_file(&kept);
        }
        moved
    }
    #[cfg(not(windows))]
    {
        let renamed = if replace { temp.persist(path) } else { temp.persist_noclobber(path) };
        renamed.map(drop).map_err(|err| err.error)
    }
}

/// Make the rename itself last through a power cut (macOS and Linux; Windows has no such call,
/// design section 7). A file system that cannot sync a folder at all is not an error.
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

/// `authority/machine.json`. It refuses when damaged and has no previous generation, and when it
/// is gone beside one: an id that changed by itself would make this machine a stranger to its
/// server and start a second words folder. It is made once, with its first generation beside it.
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
        machine_id_at(&MachineFile::path(), Some(&paths::root().join("machine-id")))
    }
}

/// The machine id kept at `path`, made at the first ask: from `legacy` (v1's `machine-id`, which
/// the mirror named this machine by) when it holds one, so the machine keeps a single id; else
/// a new random one (M57 step 1).
fn machine_id_at(path: &Path, legacy: Option<&Path>) -> Result<MachineId, StoreError> {
    let opened = open_at::<MachineFile>(path)?;
    if opened.found != Found::Missing {
        return Ok(opened.value.id);
    }
    let mut new = opened.value;
    if let Some(id) = legacy.and_then(|p| std::fs::read_to_string(p).ok()).and_then(|text| MachineId::parse(text.trim())) {
        new.id = id;
    }
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
/// file, and `state.json` does not change with it. It fails closed: when it is damaged or gone
/// beside its previous generation, [`EngineSwitch::current`] is v1 with every feature off, never
/// the previous generation (an older switch may have more on).
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
    // An older switch may have more on.
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("engine.json")
    }
}

/// `GLASSCODE_MANAGER_ENGINE=v2` runs v2 without the file, in a scratch home only (any Glasscode
/// folder but the person's own `~/.glasscode`, links and letter case seen through); the file is
/// not written.
pub const ENGINE_VARIABLE: &str = "GLASSCODE_MANAGER_ENGINE";

impl EngineSwitch {
    /// The switch in effect: the file's when it is there and whole; v1 with every feature off
    /// when there is none, or anything else (the store does not fall back, and this takes only
    /// [`Found::File`] and [`Found::Missing`] as well); then [`ENGINE_VARIABLE`]. It reads the
    /// disk: never on the main thread.
    pub fn current() -> EngineSwitch {
        let file = match open::<EngineSwitch>() {
            Ok(Opened { value, found: Found::File | Found::Missing }) => value,
            Ok(Opened { found, .. }) => {
                log::warn!("manager: the engine switch is damaged ({found:?}); running v1 with every v2 feature off");
                EngineSwitch::default()
            }
            Err(err) => {
                log::warn!("manager: {err}; running v1 with every v2 feature off");
                EngineSwitch::default()
            }
        };
        file.with_variable(std::env::var(ENGINE_VARIABLE).ok().as_deref(), scratch_home())
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

/// Whether the Glasscode folder is a scratch home.
fn scratch_home() -> bool {
    is_scratch(&paths::root(), &paths::home().join(".glasscode"))
}

/// Whether `root` is a scratch home: not `own` (the person's `~/.glasscode`) under another name (a
/// link, other letter case). A root that cannot be resolved is not one.
fn is_scratch(root: &Path, own: &Path) -> bool {
    std::fs::canonicalize(root).is_ok() && !paths::same_folder(root, own)
}

// ----- the data map ----------------------------------------------------------------------------

/// One row of the data map (design section 6): what a file or folder of the Glasscode folder is,
/// who writes it and what happens to it. `glasscode manager doctor` and the tests read it.
#[derive(Clone, Copy, Debug)]
pub struct DataRow {
    /// The file or folder, under the Glasscode folder.
    pub path: &'static str,
    /// Its [`Store::NAME`], when it is (or will be) a [`Store`].
    pub store: Option<&'static str>,
    /// The slice that makes it, while it is only planned ("M21.1"); None for what exists.
    pub planned: Option<&'static str>,
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

/// A cell nobody has checked or decided yet: M25 (export, backup, forget) goes through each.
const NOT_DECIDED: &str = "not decided (M25)";

/// A file of today that is not a [`Store`] yet: its module reads it as it always has. Every other
/// cell is [`NOT_DECIDED`] until a row says what was read in its module.
const TODAY: DataRow = DataRow {
    path: "",
    store: None,
    planned: None,
    module: "",
    writer: NOT_DECIDED,
    format: NOT_DECIDED,
    retention: NOT_DECIDED,
    on_damage: "its module's own reading (not a Store yet)",
    export: NOT_DECIDED,
    local_only: NOT_DECIDED,
    forget: NOT_DECIDED,
    sync: NOT_DECIDED,
};

/// The data map: a row for every [`Store`] and for every entry of the layout that is not a cache
/// (tests fail without one). A slice that adds a store adds its row, or takes "planned" off the
/// row it had; design section 6 follows this table.
pub const DATA_MAP: &[DataRow] = &[
    // Stores of today.
    DataRow {
        path: "state.json",
        module: "store",
        writer: "the app and glasscoded",
        format: "JSON, no envelope (older builds read it)",
        on_damage: "today's rule (a .broken copy and a fresh start); the manager part refuses",
        retention: "lasting",
        export: "yes",
        local_only: "—",
        forget: "—",
        sync: "through `mirror`",
        ..TODAY
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
        ..TODAY
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
        ..TODAY
    },
    DataRow {
        path: "authority/projects.json",
        store: Some("authority/projects"),
        module: "identity",
        writer: "the app only",
        format: "JSON, schema v1",
        retention: "lasting: a removed project keeps its key, and adding it again finds it",
        on_damage: "refuse; no project key is made or looked up, so memory and agent notes stop until it is repaired",
        export: "yes",
        local_only: "per project: a \"stays on this Mac\" project's key is never sent",
        forget: "project",
        sync: "Mac to server (M57), without \"stays on this Mac\" projects; the server reads its own machine's folders and ids",
        ..TODAY
    },
    DataRow {
        path: "authority/clock.json",
        store: Some("authority/clock"),
        module: "identity",
        writer: "the app and glasscoded, each on its own machine, before it gives out stamps",
        format: "JSON, schema v1 (this machine's last clock stamp)",
        retention: "lasting",
        on_damage: "empty: stamps start again from the clock, so their order holds unless the clock also went back",
        export: "no",
        local_only: "yes: each machine has its own",
        forget: "—",
        sync: "never",
        ..TODAY
    },
    // The words ledger is JSON lines, not a Store: one writer thread per process appends, under
    // a lock beside each file (`words`).
    DataRow {
        path: "words/<machine>/<yyyy-mm>.jsonl",
        module: "words",
        writer: "the app, and glasscoded's gate for phones; each machine in its own folder (D1); only in a v2 home, or counts only in measure mode",
        format: "JSON lines",
        retention: "while cited, else 90 days (pruned once facts cite records, M20.1)",
        on_damage: "skip bad lines",
        export: "with the facts that cite them",
        local_only: "per project",
        forget: "person, project",
        sync: "both ways (M57); the Mac never accepts tier 1 from the server",
        ..TODAY
    },
    // The journal is JSON lines, not a Store: one writer thread per process appends, under a lock
    // beside each day's file (`journal`); only in a v2 home.
    DataRow {
        path: "journal/<yyyy-mm-dd>.jsonl",
        module: "journal",
        writer: "the app, and glasscoded on the server; each in its own home; only in a v2 home",
        format: "JSON lines, schema v1 per line (`v`)",
        retention: "30 days (UTC days)",
        on_damage: "skip bad lines, log once",
        export: "no",
        local_only: "—",
        forget: "person (redact), project",
        sync: "the Mac pulls the server's by cursor (`journal.since`); both ways in M57",
        ..TODAY
    },
    DataRow {
        path: "journal/cursors.json",
        store: Some("journal/cursors"),
        module: "journal",
        writer: "the app or glasscoded, each in its own home",
        format: "JSON, schema v1 (each hook spool's offset, each server's journal cursor)",
        retention: "lasting",
        on_damage: "empty; each spool is read from its end again, and the reconciler fills the gap",
        export: "no",
        local_only: "yes",
        forget: "—",
        sync: "never",
        ..TODAY
    },
    // The manager's conversation is JSON lines, not a Store: whole versions of items and topics,
    // newest stamp wins, appended under a lock beside each file (`conversation`).
    DataRow {
        path: "manager/conversation/",
        module: "conversation",
        writer: "the app (the chat lane, wakes, the person's composer), and glasscoded when the server is home (M57); only in a v2 home",
        format: "JSON lines per topic (`<topic>.jsonl`), each line a whole version of an item or a topic with its HLC stamp",
        retention: "90 days of text, summaries and rollover links lasting",
        on_damage: "skip bad lines (and lines of a newer schema), which retention leaves in place",
        export: "yes",
        local_only: "—",
        forget: "person",
        sync: "changes both ways (M57): the newest stamp of each item and topic wins",
        ..TODAY
    },
    // The effect ledger is JSON lines, not a Store: a claim line before each effect and a finish
    // line after it, appended and synced under `effects/.lock` (`effects`).
    DataRow {
        path: "effects/<yyyy-mm>.jsonl",
        module: "effects",
        writer: "the app (claims of non-person callers, the CI fix, the manager's messages), and glasscoded when the server is home (M57)",
        format: "JSON lines (a claim, then a finish, by key and attempt)",
        retention: "1 year (setting): whole month files are deleted",
        on_damage: "refuse new at-most-once effects until repaired; a line a crash cut off is cut away before the next is written",
        export: "yes",
        local_only: "—",
        forget: "redact payloads",
        sync: "merged by key and generation; outward claims registered with the server (M58)",
        ..TODAY
    },
    // Stores that slices to come make: the cells of design section 6.
    DataRow { path: "authority/policy.json", store: Some("authority/policy"), planned: None, module: "gate", writer: "the app only", format: "JSON, schema v1 (the live policy: what the app last wrote)", retention: "lasting", on_damage: "refuse, with no fallback to `.prev`; the approved snapshot applies at most as Careful", export: "yes", local_only: "—", forget: "project", sync: "never: the server gets the approved snapshot" },
    DataRow { path: "authority/approved/policy.json", store: Some("authority/approved/policy"), planned: None, module: "gate", writer: "the app only, when the person approves", format: "JSON, schema v1 (the approved snapshot and its hash: the only policy the gate reads)", retention: "lasting", on_damage: "refuse, with no fallback to `.prev`; every project counts as Careful", export: "yes", local_only: "—", forget: "project", sync: "Mac to server inside `mirror.sync`, without \"stays on this Mac\" projects; never back" },
    DataRow { path: "authority/devices.json", store: Some("authority/devices"), planned: None, module: "gate", writer: "the app only, at pairing", format: "JSON, schema v1 (the phones this Mac paired: id, key, name)", retention: "until the phone is removed", on_damage: "refuse, with no fallback to `.prev`; no phone is trusted until it is repaired", export: "no", local_only: "yes: each Mac pins its own", forget: "—", sync: "the ids go to the server with the policy; never back" },
    DataRow { path: "authority/pause.json", store: Some("authority/pause"), planned: None, module: "gate", writer: "the app (any caller pauses; only the person releases)", format: "JSON, schema v1", retention: "until released", on_damage: "refuse, with no fallback to `.prev`; the manager counts as paused", export: "no", local_only: "—", forget: "—", sync: "both ways; the stricter wins (crit-5)" },
    DataRow { path: "authority/origins.json", store: Some("authority/origins"), planned: None, module: "gate", writer: "the app only, when a non-person caller starts a session or sends it text", format: "JSON, schema v1 (session id → automation or quarantine, by whom, when)", retention: "while the session exists", on_damage: "refuse, with no fallback to `.prev`; every session counts as one the manager drives", export: "no", local_only: "—", forget: "—", sync: "never: each machine keeps its own sessions' origins" },
    DataRow { path: "authority/from-mac.json", store: Some("authority/from-mac"), planned: None, module: "gate", writer: "glasscoded, from the Mac's sync", format: "JSON, schema v1 (the Mac's approved policy, pause and paired phones)", retention: "until the next sync", on_damage: "refuse, with no fallback to `.prev`; the server's sessions count as Careful and the manager there as paused", export: "no", local_only: "—", forget: "project", sync: "Mac to server only; never sent back" },
    DataRow { path: "authority/playbooks.json", store: Some("authority/playbooks"), planned: None, module: "playbooks", writer: "the app only", format: "JSON, schema v1 (the person's playbooks, the manager's proposals and the built-ins' settings and pauses)", retention: "lasting", on_damage: "refuse; nothing runs", export: "yes", local_only: "—", forget: "—", sync: "Mac to server" },
    DataRow { path: "authority/approved/playbooks.json", store: Some("authority/approved/playbooks"), planned: None, module: "playbooks", writer: "the app only, when the person approves", format: "JSON, schema v1 (the hash this machine approved, by playbook id)", retention: "lasting", on_damage: "refuse, with no fallback to `.prev`; no playbook but the built-ins runs", export: "yes", local_only: "yes: each machine approves for itself", forget: "—", sync: "never: a playbook that arrives by sync is not approved there" },
    DataRow { path: "authority/schedules.json", store: Some("authority/schedules"), planned: None, module: "schedule", writer: "the app only", format: "JSON, schema v1 (schedules: spec in local time, action, misfire, expiry in UTC, where it runs)", retention: "lasting", on_damage: "refuse, with no fallback to `.prev`; nothing runs", export: "yes", local_only: "—", forget: "—", sync: "Mac to server; approved per machine (`schedule:<id>` in the approved playbooks)" },
    DataRow { path: "authority/schedules-state.json", store: Some("authority/schedules-state"), planned: None, module: "schedule", writer: "the app only, before a run starts", format: "JSON, schema v1 (each schedule's last evaluation, last run started and run going; the latest wall time seen)", retention: "lasting", on_damage: "refuse, with no fallback to `.prev`; no schedule runs until it is repaired", export: "no", local_only: "yes: each machine's own runs", forget: "—", sync: "never" },
    DataRow { path: "authority/pending.json", store: Some("authority/pending"), planned: None, module: "gate", writer: "the app only", format: "JSON, schema v1 (each action: stable id, op and arguments with their hash, targets, who asks and why, expiry, state)", retention: "until done or expired, plus 7 days", on_damage: "refuse, with no fallback to `.prev`; cards show \"could not load\" and nothing waiting runs", export: "no", local_only: "—", forget: "—", sync: "Mac to server and back (M57); carries no authority" },
    DataRow { path: "authority/memory-head.json", store: Some("authority/memory-head"), module: "memstore", writer: "the app and glasscoded (memory's one writer per machine)", format: "JSON, schema v1 (Glasscode's last memory commit, and the commit that last changed each scope: you, org/<employer>, projects/<key>)", retention: "lasting", on_damage: "refuse, with no fallback to `.prev`; briefs empty and the person is told", export: "no", local_only: "—", forget: "—", sync: "Mac to server", ..TODAY },
    DataRow { path: "glasscode-memory.json", module: "the voice copilot (voice/memory.mjs)", writer: "the voice copilot", format: "JSON", retention: "the voice plan decides", sync: "none; M20.3 proposes its facts to memory once and leaves the file as it is", ..TODAY },
    DataRow { path: "memory-server/", module: "memory_copy (M20.3)", writer: "glasscoded, from the Mac's push; read-only on disk", format: "a checkout of the copy's commit", retention: "replaced at each update", on_damage: "checked out again at the next update", export: "no: a copy of the Mac's", local_only: "never holds a \"stays on this Mac\" project or an employer folder only such projects use", forget: "follows the Mac's memory", sync: "Mac to server only: a filtered tree on its own history", ..TODAY },
    DataRow { path: "memory-server.git/", module: "memory_copy (M20.3)", writer: "the Mac's push (refs/glasscode/memory-server)", format: "git, bare", retention: "the copy's own history", on_damage: "made again; the Mac pushes again", export: "no", local_only: "no local-only text in any object", forget: "follows the Mac's memory", sync: "Mac to server only", ..TODAY },
    DataRow { path: "memory/", module: "memstore", writer: "`glasscode memory …` only (memstore, one writer per machine)", format: "git, Markdown with entry headers", retention: "lasting; archive kept", on_damage: "`git fsck`; refuse writes and keep reads from the last good commit", export: "yes", local_only: "per project", forget: "person, project, employer", sync: "per entry, by HLC stamp and tombstone (M57); \"stays on this Mac\" projects never sent", ..TODAY },
    DataRow { path: "agent-notes/", planned: None, module: "memory", writer: "sessions, and Claude's redirected auto-memory", format: "Markdown (the vendor's own)", retention: "lasting", on_damage: "not used for briefs", export: "yes", local_only: "per project", forget: "project", sync: "per project policy", ..TODAY },
    DataRow { path: "manager/work.json", store: Some("manager/work"), planned: Some("M32"), module: "work", format: "JSON", retention: "until done plus 30 days", on_damage: "refuse; rebuilt by the reconciler", export: "no", local_only: "—", forget: "—", sync: "both ways (M57)", ..TODAY },
    DataRow { path: "manager/work/", planned: Some("M50"), module: "runtime", writer: "the brain", ..TODAY },
    // Today's files and folders, which are not stores yet: only what was read in their modules.
    DataRow { path: "manager/data.json", module: "manager::store (v1)", format: "JSON", retention: "read once by migration (M44), then kept as `data.v1.json`", on_damage: "a .bad copy and an empty start", ..TODAY },
    DataRow { path: "server.json", module: "remote", ..TODAY },
    DataRow { path: "glasscoded.json", module: "glasscoded daemon", writer: "glasscoded", ..TODAY },
    DataRow {
        path: "authority/home.json",
        store: Some("authority/home"),
        module: "home (kept by glasscoded, M58)",
        writer: "glasscoded, its only writer",
        format: "JSON, schema v1",
        retention: "lasting: the manager's home, its generation and the Mac's lease",
        on_damage: "refuse, with no older generation: no machine is home until it is repaired",
        export: "no: it describes this server",
        local_only: "—",
        forget: "—",
        sync: "the Mac reads and renews it through home.*, never writes it",
        ..TODAY
    },
    DataRow {
        path: "authority/owners.json",
        store: Some("authority/owners"),
        module: "owners (kept by glasscoded)",
        writer: "glasscoded, its only writer",
        format: "JSON, schema v1",
        retention: "lasting: one record per worktree that has moved",
        on_damage: "refuse, with no older generation: moves stop until it is repaired; the server keeps running its own sessions",
        export: "no: it describes the server's worktrees",
        local_only: "—",
        forget: "—",
        sync: "the Mac reads it through `owners.get` and `mirror.sync`, never writes it",
        ..TODAY
    },
    DataRow { path: "glasscoded.sock", module: "glasscoded daemon", writer: "glasscoded", format: "a Unix socket", on_damage: "removed and made again when glasscoded starts", ..TODAY },
    DataRow { path: "cli-versions.json", module: "versions", writer: "the person, and the app when it sets up a server", ..TODAY },
    DataRow { path: "clis.json", module: "versions", ..TODAY },
    DataRow { path: "control.key", module: "glasscoded today", writer: "glasscoded", format: "the key glasscoded signs its sessions' requests with", ..TODAY },
    DataRow { path: "push.json", module: "glasscoded push", writer: "not Glasscode: glasscoded only reads it", ..TODAY },
    DataRow { path: "push-devices.json", module: "glasscoded push", writer: "glasscoded", format: "JSON", ..TODAY },
    DataRow { path: "relay.json", module: "glasscoded relay_link", writer: "glasscoded", format: "JSON", ..TODAY },
    DataRow { path: "auto-review.json", module: "auto_review (app)", writer: "the app", ..TODAY },
    DataRow { path: "ci-fix.json", module: "ci_fix (app)", writer: "the app", ..TODAY },
    DataRow { path: "tmux.conf", module: "glasscoded tmux", writer: "glasscoded", format: "tmux configuration", on_damage: "written again when it differs", ..TODAY },
    DataRow { path: "review-prompt.md", module: "auto_review (app)", writer: "the person", format: "Markdown", ..TODAY },
    DataRow { path: "device/", module: "remote", format: "ssh files for the relay (known hosts)", ..TODAY },
    DataRow { path: "machine-id", module: "mirror", writer: "the app, once", format: "text", sync: "this Mac's id for its server (v1)", ..TODAY },
    DataRow {
        path: "board.json",
        store: Some("board"),
        module: "board",
        writer: "the app, and glasscoded for the server's copy",
        format: "JSON, schema v1 (a plain board from older builds is read once)",
        retention: "lasting; done tasks for a few days; removal markers for 60 days",
        on_damage: "refuse: the file is kept (as .broken at the next save) and Today starts from its previous generation, else empty",
        sync: "the Mac's and the server's copies merge entry by entry with M11 stamps, at each mirror.sync",
        ..TODAY
    },
    DataRow { path: "activity.json", module: "activity", on_damage: "a .broken copy and a fresh start", ..TODAY },
    DataRow { path: "agent-log.jsonl", module: "control", format: "JSON lines", retention: "until the journal (M14) takes its place", ..TODAY },
    DataRow { path: "glasscoded.log", module: "glasscoded main", writer: "glasscoded", format: "text", ..TODAY },
    DataRow { path: "handoff/", module: "handoff", writer: "the app", format: "Markdown, one note per handoff", ..TODAY },
    DataRow { path: "worktrees/", module: "git", writer: "sessions and the app", format: "git worktrees", retention: "until the worktree is removed", ..TODAY },
    DataRow { path: "accounts/", module: "model", writer: "the vendor CLIs", format: "the vendor's own", sync: "never: no credential is ever copied (D31)", ..TODAY },
    DataRow { path: "events/", module: "agents", writer: "hooks", format: "JSON lines, one file per session", ..TODAY },
    DataRow { path: "settings/", module: "agents", format: "Claude's settings, one file per session (Windows)", ..TODAY },
    DataRow { path: "bin/", module: "cli", format: "programs", ..TODAY },
    DataRow { path: "agent/", module: "cli", format: "Claude's plugin folder", ..TODAY },
    DataRow { path: "pasted/", module: "terminal (app)", format: "files copied in through a terminal", ..TODAY },
    DataRow { path: "control.sock", module: "sys", writer: "the app", format: "a Unix socket (a named pipe on Windows; under /tmp when the path is too long)", ..TODAY },
    // A cache row is not needed, but this one is the person's text, not a cache.
    DataRow { path: "unsaved/", module: "save", writer: "the app", format: "copies of editor text", ..TODAY },
];

/// A [`Store`] for the data map, the doctor and the tests, without its type.
#[derive(Clone, Copy)]
pub struct StoreInfo {
    pub name: &'static str,
    pub version: u32,
    pub on_damage: OnDamage,
    pub falls_back: bool,
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
            falls_back: S::FALLS_BACK,
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
pub const ALL_STORES: &[StoreInfo] = &[
    StoreInfo::of::<MachineFile>(),
    StoreInfo::of::<EngineSwitch>(),
    StoreInfo::of::<crate::identity::Projects>(),
    StoreInfo::of::<crate::identity::LastStamp>(),
    StoreInfo::of::<crate::owners::Owners>(),
    StoreInfo::of::<crate::board::Board>(),
    StoreInfo::of::<crate::home::HomeFile>(),
    StoreInfo::of::<crate::gate::PolicyFile>(),
    StoreInfo::of::<crate::gate::ApprovedPolicy>(),
    StoreInfo::of::<crate::gate::Devices>(),
    StoreInfo::of::<crate::gate::Pause>(),
    StoreInfo::of::<crate::gate::FromMac>(),
    StoreInfo::of::<crate::playbooks::PlaybookFile>(),
    StoreInfo::of::<crate::playbooks::ApprovedPlaybooks>(),
    StoreInfo::of::<crate::schedule::ScheduleFile>(),
    StoreInfo::of::<crate::schedule::ScheduleState>(),
    StoreInfo::of::<crate::gate::SessionOrigins>(),
    StoreInfo::of::<crate::memstore::MemoryHead>(),
    StoreInfo::of::<crate::journal::Cursors>(),
    StoreInfo::of::<crate::gate::Pending>(),
];

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

    /// Only a file that loads becomes the previous generation; a save over any other keeps it as
    /// `.broken` first, loaded or not (review of PR 78, E10 and E3).
    #[test]
    fn a_save_keeps_only_a_file_that_loads_as_the_previous_generation() {
        let _home = Home::new();
        let path = Counter::path();
        let previous = sibling(&path, ".prev");
        Counter { count: 1, pad: String::new() }.save().unwrap();
        Counter { count: 2, pad: String::new() }.save().unwrap();
        // A whole envelope whose data does not load.
        let bad = r#"{"store":"test/counter","version":2,"data":{"count":"many"}}"#;
        std::fs::write(&path, bad).unwrap();
        Counter { count: 3, pad: String::new() }.save().unwrap();
        assert_eq!(read_generation::<Counter>(&previous).map(|c| c.count), Some(1), "the generation before the bad file stays");
        assert_eq!(std::fs::read_to_string(broken(&path)).unwrap(), bad);
        assert_eq!(Counter::load().unwrap().count, 3);

        // A damaged refusing store saved over without a load before, and with no generation.
        let _ = std::fs::remove_file(&previous);
        let _ = std::fs::remove_file(broken(&path));
        std::fs::write(&path, "garbage").unwrap();
        Counter { count: 4, pad: String::new() }.save().unwrap();
        assert_eq!(std::fs::read_to_string(broken(&path)).unwrap(), "garbage");
        assert!(!previous.exists(), "garbage is no generation");
        // An older schema that does not migrate is kept the same way.
        let old = r#"{"store":"test/counter","version":1,"data":{}}"#;
        std::fs::write(&path, old).unwrap();
        Counter { count: 5, pad: String::new() }.save().unwrap();
        assert!(copies(&path).iter().any(|copy| std::fs::read_to_string(path.with_file_name(copy)).unwrap() == old), "{:?}", copies(&path));
    }

    /// A refusing store whose file is gone while its previous generation is there refuses, and
    /// keeps the generation; only a store with neither is new (review of PR 78, E4 and E11).
    #[test]
    fn a_refusing_store_gone_beside_its_previous_generation_refuses() {
        let home = Home::new();
        let id = MachineId::this().unwrap();
        let path = MachineFile::path();
        let previous = sibling(&path, ".prev");
        assert_eq!(read_generation::<MachineFile>(&previous).map(|f| f.id), Some(id.clone()), "made with its generation");
        // Damaged: the same id, from the generation.
        std::fs::write(&path, "").unwrap();
        assert_eq!(MachineId::this().unwrap(), id);
        // Gone: refused, never a new id, and the generation is left.
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(MachineId::this(), Err(StoreError::Damaged { store: "authority/machine", .. })));
        assert!(!path.exists());
        assert_eq!(read_generation::<MachineFile>(&previous).map(|f| f.id), Some(id));
        // A store that starts empty is new without its file, whatever is beside it.
        Notes { lines: vec!["a".into()] }.save().unwrap();
        Notes { lines: vec!["b".into()] }.save().unwrap();
        std::fs::remove_file(Notes::path()).unwrap();
        assert_eq!(open::<Notes>().unwrap().found, Found::Missing);
        drop(home);
    }

    /// Two processes saving one store: the second waits for the first's lock (review of PR 78).
    #[test]
    fn a_save_waits_for_another_process_saving_the_same_store() {
        let home = Home::new();
        let path = Counter::path();
        let first = Counter { count: 41, pad: "first".into() };
        first.save().unwrap();
        // The child holds the lock, paused before its rename.
        let mut child = start_child(home.path(), "pause");
        let (done, waited) = std::sync::mpsc::channel();
        let saver = std::thread::spawn(move || {
            let result = Counter { count: 42, pad: "second".into() }.save();
            done.send(()).unwrap();
            result
        });
        assert!(waited.recv_timeout(Duration::from_millis(500)).is_err(), "the save did not wait for the lock");
        child.kill().unwrap();
        child.wait().unwrap();
        waited.recv_timeout(Duration::from_secs(30)).expect("the save went on once the child was gone");
        saver.join().unwrap().unwrap();
        assert_eq!(Counter::load().unwrap().count, 42);
        // The child never renamed: the generation before is the first save.
        assert_eq!(read_generation::<Counter>(&sibling(&path, ".prev")).map(|c| c.count), Some(41));
    }

    /// `update` reads and saves under one lock: writers that each add one change lose none of
    /// them, and a store that refuses is neither changed nor written.
    #[test]
    fn an_update_keeps_every_writers_change() {
        let _home = Home::new();
        let writers: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..10 {
                        update::<Counter, _>(|counter| counter.count += 1).unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        assert_eq!(Counter::load().unwrap().count, 40);
        assert_eq!(update::<Counter, _>(|counter| counter.count).unwrap(), 40);
        // A newer file: the change is not made and nothing is written.
        let path = Counter::path();
        let newer = r#"{"store":"test/counter","version":9,"data":{}}"#;
        write(&path, newer);
        let mut called = false;
        assert!(matches!(update::<Counter, _>(|_| called = true), Err(StoreError::Newer { .. })));
        assert!(!called);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
    }

    /// A save sweeps the temporary files of saves that died more than ten minutes ago.
    #[test]
    fn a_save_sweeps_old_temporary_files() {
        let _home = Home::new();
        let path = Notes::path();
        Notes::default().save().unwrap();
        let folder = path.parent().unwrap();
        let old = folder.join(".notes.json.Ab12Cd.tmp");
        let old_generation = folder.join(".notes.json.prev.Ef34Gh.tmp");
        let fresh = folder.join(".notes.json.Ij56Kl.tmp");
        let other = folder.join(".other.json.Mn78Op.tmp");
        for file in [&old, &old_generation, &fresh, &other] {
            std::fs::write(file, "half").unwrap();
        }
        let eleven_minutes_ago = filetime::FileTime::from_system_time(std::time::SystemTime::now() - Duration::from_secs(11 * 60));
        for file in [&old, &old_generation, &other] {
            filetime::set_file_mtime(file, eleven_minutes_ago).unwrap();
        }
        Notes { lines: vec!["x".into()] }.save().unwrap();
        assert!(!old.exists() && !old_generation.exists(), "old temporary files of this store are gone");
        assert!(fresh.exists(), "a recent one may be a save that is still running");
        assert!(other.exists(), "another store's are its own");
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
                match ((store.check)(), store.falls_back) {
                    (Ok(Found::Previous { .. }), true) | (Err(StoreError::Damaged { .. }), false) => {}
                    (other, falls_back) => panic!("{name}, {damage}, falls back {falls_back}: {other:?}"),
                }
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

            // A newer schema: not read, not written over, and no generation made of it. The
            // version decides before anything else of the file (review of PR 78, E2): without
            // data, past u32, or with its data under another key.
            let next = store.version + 1;
            let newer_files = [
                format!(r#"{{"store":"{name}","version":{next},"data":{{}}}}"#),
                format!(r#"{{"store":"{name}","version":{next}}}"#),
                format!(r#"{{"store":"{name}","version":4294967296,"data":{{}}}}"#),
                format!(r#"{{"store":"{name}","version":{next},"payload":{{"lines":["newer"]}}}}"#),
                // Numbers that are not a u64: a float, and past u64 (review of PR 78, second round).
                format!(r#"{{"store":"{name}","version":{next}.0,"data":{{}}}}"#),
                format!(r#"{{"store":"{name}","version":1e30,"data":{{}}}}"#),
                format!(r#"{{"store":"{name}","version":18446744073709551616,"data":{{}}}}"#),
            ];
            for newer in newer_files {
                std::fs::write(&path, &newer).unwrap();
                let generation = std::fs::read(&previous).unwrap();
                assert!(matches!((store.check)(), Err(StoreError::Newer { .. })), "{name}: {newer}: {:?}", (store.check)());
                assert!(matches!((store.reset)(), Err(StoreError::Newer { .. })), "{name}: {newer}");
                assert_eq!((std::fs::read_to_string(&path).unwrap(), std::fs::read(&previous).unwrap()), (newer.clone(), generation), "{name}: {newer}");
            }

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

    /// Every `impl … Store for …` of every crate's source (a renamed `Store`, a signature over
    /// lines, a macro: review of PR 78), with the test modules and tests/ folders left out by file,
    /// not by name. One whose `NAME` the reader cannot see as a literal is an error.
    fn stores_in_the_source() -> std::collections::BTreeSet<String> {
        let sources = paths::source::crate_sources();
        let mut names = vec!["Store".to_string()];
        for (_, text) in &sources {
            names.extend(paths::source::store_aliases(text));
        }
        let mut found = std::collections::BTreeSet::new();
        let mut errors = Vec::new();
        for (file, text) in &sources {
            for store in paths::source::store_impls(&file.display().to_string(), text, &names) {
                match store {
                    Ok(name) => {
                        found.insert(name);
                    }
                    Err(err) => errors.push(err),
                }
            }
        }
        assert!(errors.is_empty(), "{}", errors.join("\n"));
        found
    }

    /// A machine with v1's `machine-id` keeps that id as its machine id: one id for the mirror,
    /// the owner records and the words ledger (M57 step 1).
    #[test]
    fn the_machine_id_is_the_older_one_when_there_was_one() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("machine-id");
        std::fs::write(&legacy, "3f2b9c1e-7a6d-4e2f-9b8a-1c2d3e4f5a6b\n").unwrap();
        let path = dir.path().join("authority").join("machine.json");
        let id = machine_id_at(&path, Some(&legacy)).unwrap();
        assert_eq!(id.as_str(), "3f2b9c1e-7a6d-4e2f-9b8a-1c2d3e4f5a6b");
        assert_eq!(machine_id_at(&path, None).unwrap(), id, "kept");
        // No older id, or one that is not a folder name: a new one.
        let other = dir.path().join("other").join("machine.json");
        std::fs::write(&legacy, "../no").unwrap();
        assert_ne!(machine_id_at(&other, Some(&legacy)).unwrap().as_str(), "../no");
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
            assert!(row.planned.is_none(), "{} exists: its row is not planned any more", store.name);
            assert_eq!(paths::root().join(row.path), (store.path)(), "{}: the row's path", store.name);
            assert!(row.format.contains(&format!("schema v{}", store.version)), "{}: {}", store.name, row.format);
            let rule = match store.on_damage {
                OnDamage::Empty => "empty",
                OnDamage::Refuse => "refuse",
            };
            assert!(row.on_damage.starts_with(rule), "{}: {}", store.name, row.on_damage);
            assert!(paths::class_of(&(store.path)()).class().is_some(), "{} has no path class", store.name);
        }
        // A row of a store is of one that exists, or of one a slice will make; one row per path;
        // every row is inside an entry of the layout.
        for row in DATA_MAP {
            if let Some(name) = row.store {
                assert!(listed.contains(name) || row.planned.is_some(), "{}: no such store, and not planned", row.path);
            }
            assert_eq!(DATA_MAP.iter().filter(|other| other.path == row.path).count(), 1, "{}", row.path);
            assert!(paths::row_of(&paths::root().join(row.path)).is_some_and(|entry| entry.base == paths::Base::Glasscode), "{}: in no entry of the layout", row.path);
        }
        // Every entry of the Glasscode folder that is not a cache has a row (review of PR 78).
        for entry in paths::layout().iter().filter(|entry| entry.base == paths::Base::Glasscode && entry.class != paths::PathClass::Cache) {
            let covered = DATA_MAP.iter().any(|row| paths::row_of(&paths::root().join(row.path)) == Some(entry));
            assert!(covered, "the layout's {} ({:?}) has no row in DATA_MAP", entry.entry, entry.class);
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
        assert_eq!(paths::class_of(&file), paths::Classified::Class(paths::PathClass::Authority));
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
            let asks: Vec<_> = (0..8).map(|_| scope.spawn(|| machine_id_at(&path, None).unwrap())).collect();
            asks.into_iter().map(|ask| ask.join().unwrap()).collect()
        });
        assert!(ids.iter().all(|other| *other == ids[0]), "{ids:?}");
        assert_eq!(MachineId::this().unwrap(), ids[0]);
        // Another Glasscode folder (another machine) has its own.
        assert_ne!(ids[0], id);
        drop(home);
    }

    /// The engine switch fails closed: a damaged or missing file never brings back an older
    /// generation (review of PR 78, E1).
    #[test]
    fn damaged_engine_switch_is_v1_all_off() {
        let _home = Home::new();
        let v2 = EngineSwitch { engine: Engine::V2, switches: BTreeMap::from([("brief".to_string(), true)]) };
        v2.save().unwrap();
        EngineSwitch::default().save().unwrap();
        assert_eq!(EngineSwitch::current(), EngineSwitch::default());
        // The file damaged, with v2 as the previous generation beside it.
        for damage in ["", "{ not json", r#"{"store":"authority/engine","version":1,"data":{"engine":"v"#] {
            std::fs::write(EngineSwitch::path(), damage).unwrap();
            assert_eq!(EngineSwitch::current(), EngineSwitch::default(), "{damage:?}");
        }
        // The file gone, with v2 beside it.
        v2.save().unwrap();
        v2.save().unwrap();
        std::fs::remove_file(EngineSwitch::path()).unwrap();
        assert!(sibling(&EngineSwitch::path(), ".prev").exists());
        assert_eq!(EngineSwitch::current(), EngineSwitch::default());
    }

    /// No way of reading the engine switch takes an older generation: `load`, `open` and
    /// `current` all fail closed (review of PR 78, second round).
    #[test]
    fn no_reading_of_the_engine_switch_takes_an_older_generation() {
        let _home = Home::new();
        EngineSwitch { engine: Engine::V2, switches: BTreeMap::from([("brief".to_string(), true)]) }.save().unwrap();
        EngineSwitch::default().save().unwrap();
        std::fs::write(EngineSwitch::path(), "").unwrap();
        assert!(matches!(EngineSwitch::load(), Err(StoreError::Damaged { .. })), "{:?}", EngineSwitch::load());
        assert!(matches!(open::<EngineSwitch>(), Err(StoreError::Damaged { .. })));
        assert_eq!(EngineSwitch::current(), EngineSwitch::default());
        // The machine id may take its own older generation: it is the same id.
        let id = MachineId::this().unwrap();
        std::fs::write(MachineFile::path(), "").unwrap();
        assert_eq!(MachineId::this().unwrap(), id);
    }

    /// `create_at` never replaces a file that is there (the no-clobber rename).
    #[test]
    fn create_at_never_replaces_a_file() {
        let _home = Home::new();
        let path = MachineFile::path();
        let first = MachineFile::default();
        assert!(create_at(&first, &path).unwrap());
        let before = std::fs::read(&path).unwrap();
        assert!(!create_at(&MachineFile::default(), &path).unwrap(), "a file is there");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(MachineId::this().unwrap(), first.id);
    }

    /// The person's own Glasscode folder under another name is no scratch home, so the engine
    /// variable does nothing there (review of PR 78).
    #[test]
    fn the_persons_own_folder_by_another_name_is_no_scratch_home() {
        let dir = tempfile::tempdir().unwrap();
        let own = dir.path().join("own");
        let scratch = dir.path().join("scratch");
        std::fs::create_dir(&own).unwrap();
        std::fs::create_dir(&scratch).unwrap();
        assert!(is_scratch(&scratch, &own));
        assert!(!is_scratch(&own, &own));
        assert!(!is_scratch(&dir.path().join("gone"), &own), "a root that cannot be resolved");
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&own, &link).unwrap();
            assert!(!is_scratch(&link, &own), "a link to the person's own folder");
        }
        if cfg!(any(target_os = "macos", windows)) {
            let other_case = PathBuf::from(own.display().to_string().to_uppercase());
            assert!(!is_scratch(&other_case, &own), "other letter case");
        }
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
        assert_eq!(paths::class_of(&EngineSwitch::path()), paths::Classified::Class(paths::PathClass::Authority));
    }
}
