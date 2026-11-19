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

/// What [`Store::load`] does with a damaged file (the "On damage" column of design section 6).
/// Either way a `.broken` copy is kept beside it first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnDamage {
    /// Start from the default; the next save replaces the file.
    Empty,
    /// Fail and leave the file as it is: the feature that needs the store stops and says so, and
    /// stays stopped until the file is repaired. It never starts empty.
    Refuse,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The file could not be read or written: permissions, a full disk, a folder in its place. Not
    /// damage, so nothing starts empty.
    #[error("Glasscode could not read or write {store} ({}): {source}", path.display())]
    Io { store: &'static str, path: PathBuf, source: std::io::Error },
    /// The file is damaged; `kept` is the copy of it, when one could be written.
    #[error("{store} is damaged ({why})")]
    Damaged { store: &'static str, why: String, kept: Option<PathBuf> },
    /// A newer Glasscode wrote the file: this one neither reads nor replaces it.
    #[error("{store} was written by a newer Glasscode (schema {found}; this one reads up to {known})")]
    Newer { store: &'static str, found: u32, known: u32 },
    /// The file has an older schema that could not be brought up to date. It is left as it is.
    #[error("{store} could not be updated from schema {from} ({why})")]
    Migrate { store: &'static str, from: u32, why: String },
    #[error("{store} could not be written ({why})")]
    Encode { store: &'static str, why: String },
}

/// A file of Manager v2 (design section 8). On disk it is `{"store": NAME, "version": VERSION,
/// "data": …}`; a file without that envelope is damaged. Its path has a class (`paths::class_of`)
/// and its row in the data map of design section 6 says its `ON_DAMAGE`.
pub trait Store: Default + Serialize + DeserializeOwned {
    /// "authority/policy".
    const NAME: &'static str;
    /// The schema version; an older file goes through [`Store::migrate`].
    const VERSION: u32;
    const ON_DAMAGE: OnDamage;

    fn path() -> PathBuf;

    /// The store; the default when there is no file. A damaged file is kept as `.broken` and gives
    /// the default ([`OnDamage::Empty`]) or [`StoreError::Damaged`] ([`OnDamage::Refuse`]). A file
    /// of a newer schema, a migration that fails and a read error are errors for every store. It
    /// reads the disk: never on the main thread.
    fn load() -> Result<Self, StoreError> {
        Ok(read_at::<Self>(&Self::path())?.unwrap_or_default())
    }

    /// Into a temporary file beside it, synced, then renamed over it (`MoveFileEx` replacing it on
    /// Windows), so a reader or a crash never sees half a file.
    fn save(&self) -> Result<(), StoreError> {
        save_at(self, &Self::path())
    }

    /// The store from the data of an older schema `from` (1 or more). None by default.
    fn migrate(from: u32, value: serde_json::Value) -> Result<Self, StoreError> {
        let _ = value;
        Err(StoreError::Migrate { store: Self::NAME, from, why: "this store has no older schema".into() })
    }
}

#[derive(Serialize)]
struct Envelope<'a, T> {
    store: &'a str,
    version: u32,
    data: &'a T,
}

/// The store in `path`; None when there is no file.
fn read_at<S: Store>(path: &Path) -> Result<Option<S>, StoreError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(StoreError::Io { store: S::NAME, path: path.to_path_buf(), source }),
    };
    let (version, data) = match open_envelope(S::NAME, &bytes) {
        Ok(opened) => opened,
        Err(why) => return damaged(path, &bytes, why).map(Some),
    };
    if version > S::VERSION {
        return Err(StoreError::Newer { store: S::NAME, found: version, known: S::VERSION });
    }
    if version < S::VERSION {
        return S::migrate(version, data).map(Some).inspect_err(|err| {
            let kept = keep_broken(path, &bytes);
            log::error!("{err}; the file is left as it is{}", kept.map(|k| format!(", and copied to {}", k.display())).unwrap_or_default());
        });
    }
    match serde_json::from_value(data) {
        Ok(value) => Ok(Some(value)),
        Err(err) => damaged(path, &bytes, err.to_string()).map(Some),
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

/// A damaged file: keep a copy, then start empty or refuse, as the store says.
fn damaged<S: Store>(path: &Path, bytes: &[u8], why: String) -> Result<S, StoreError> {
    let kept = keep_broken(path, bytes);
    match (&kept, S::ON_DAMAGE) {
        (Some(copy), OnDamage::Empty) => {
            log::error!("{} is damaged ({why}); it is kept as {} and the store starts empty", S::NAME, copy.display());
            Ok(S::default())
        }
        _ => {
            log::error!("{} is damaged ({why}); what needs it stops until it is repaired", S::NAME);
            Err(StoreError::Damaged { store: S::NAME, why, kept })
        }
    }
}

/// Copy the damaged bytes beside the file as `<file>.broken`; when an earlier, different copy is
/// there, as `<file>.broken.2`, `.broken.3` and so on. The same damage seen again reuses its copy.
/// None when no copy could be written; then not even an [`OnDamage::Empty`] store starts empty, as
/// the next save would lose the file.
fn keep_broken(path: &Path, bytes: &[u8]) -> Option<PathBuf> {
    for n in 1..=50 {
        let mut copy = path.as_os_str().to_owned();
        copy.push(if n == 1 { ".broken".to_string() } else { format!(".broken.{n}") });
        let copy = PathBuf::from(copy);
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
    let bytes = encode(value)?;
    let failed = |source| StoreError::Io { store: S::NAME, path: path.to_path_buf(), source };
    let temp = write_temp(path, &bytes).map_err(failed)?;
    #[cfg(test)]
    tests::pause_before_rename();
    temp.persist(path).map_err(|err| failed(err.error))?;
    sync_folder(path);
    Ok(())
}

/// Write `value` to `path` only when there is no file there yet, in one step: true when this
/// call made it, false when one was there (made by another thread or process meanwhile).
fn create_at<S: Store>(value: &S, path: &Path) -> Result<bool, StoreError> {
    let bytes = encode(value)?;
    let failed = |source| StoreError::Io { store: S::NAME, path: path.to_path_buf(), source };
    let temp = write_temp(path, &bytes).map_err(failed)?;
    match temp.persist_noclobber(path) {
        Ok(_) => {
            sync_folder(path);
            Ok(true)
        }
        Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(err) => Err(failed(err.error)),
    }
}

/// The new bytes in a temporary file beside `path` (only the owner can read it), synced to disk.
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
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    Ok(temp)
}

/// Make the rename itself last through a power cut (macOS and Linux; Windows has no such call).
fn sync_folder(path: &Path) {
    #[cfg(unix)]
    if let Some(folder) = path.parent() {
        let _ = std::fs::File::open(folder).and_then(|folder| folder.sync_all());
    }
    #[cfg(not(unix))]
    let _ = path;
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
    if let Some(file) = read_at::<MachineFile>(path)? {
        return Ok(file.id);
    }
    let new = MachineFile::default();
    if create_at(&new, path)? {
        return Ok(new.id);
    }
    // Another thread or process made it first: theirs it is.
    match read_at::<MachineFile>(path)? {
        Some(file) => Ok(file.id),
        None => Err(StoreError::Io { store: MachineFile::NAME, path: path.to_path_buf(), source: std::io::ErrorKind::NotFound.into() }),
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

        // A newer schema is neither read, nor copied, nor started empty: an older build must not
        // overwrite it.
        let newer = r#"{"store":"test/notes","version":7,"data":{"lines":[]}}"#;
        write(&path, newer);
        assert!(matches!(Notes::load(), Err(StoreError::Newer { found: 7, known: 1, .. })));
        assert_eq!(copies(&path).len(), 2);
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
