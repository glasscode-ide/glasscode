//! Your data (M25, crit-8): export, import, forget and uninstall.
//!
//! - **Export** writes one `tar` file (the `tar` program: macOS, Linux and Windows 10 and later
//!   have it) holding memory as of Glasscode's last commit (its working files), the stores the
//!   data map marks for export (the authority store without this machine's own files and device
//!   keys, the effect ledger, the manager's conversation), the words records memory's facts cite,
//!   and a manifest. A project that stays on this Mac is left out unless it is named.
//! - **Import** takes an export into this Glasscode folder: memory merged entry by entry (stamps
//!   and tombstones decide), the project keys' remotes and roots (so a project added on this
//!   machine finds its key again), the cited words records, and the authority files put aside under
//!   `authority/imported/` for the person to approve again (an imported policy carries no
//!   authority by itself, D10).
//! - **Forget** a person (every entry naming them, their words records, the journal's summaries and
//!   the effect ledger's lines naming them), a project (its memory folder, agent notes, key and
//!   journal lines), or an employer (every project that names it, and its folder). Memory's history
//!   is rebuilt each time, so the text is in no object.
//! - **Uninstall** lists everything Glasscode made ([`uninstall_plan`]) and removes it after the
//!   person confirms in the app ([`uninstall`]); a worktree with work that is nowhere else stops it.
//!   Outside its folder it takes only its own: the trust tables of its worktrees in Codex's
//!   `config.toml`, and its keys in the system's keychain.
//! - **Scheduled export** ([`ExportSchedule`]): an export every day or week to a folder, keeping the
//!   newest few. The app asks [`scheduled_export`] each hour.
//! - **Move out** ([`move_out`]): the agent notes go back to each Claude account's own memory
//!   folder of the project, so Claude keeps what it learned here.
//!
//! Everything here reads and writes the disk and runs programs: never on the main thread.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};

use crate::identity::Projects;
use crate::memstore::{self, Doc, Employer, MemFile, Memory, Source};
use crate::paths::{self, ProjectKey};
use crate::store::{DATA_MAP, StoreError};

/// The manifest of an export (`manifest.json` at its top).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The export's own format.
    pub format: u32,
    pub made_ms: u64,
    /// The memory commit it was made from.
    pub memory_commit: Option<String>,
    /// Projects that stay on their Mac, left out.
    pub left_out: Vec<ProjectKey>,
}

pub const FORMAT: u32 = 1;

/// Files of the authority store an export never holds: this machine's own (its id, its clock, its
/// engine switch, memory's head), and the paired devices' keys.
const NOT_EXPORTED: &[&str] = &["authority/machine.json", "authority/clock.json", "authority/engine.json", "authority/memory-head.json", "authority/devices.json", "authority/memory.lock"];

/// What an export wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
    pub path: PathBuf,
    pub memory_files: usize,
    pub other_files: usize,
    pub left_out: Vec<ProjectKey>,
}

/// Export into `to` (a `.tar` file). `local_only` are the projects that stay on this Mac; `include`
/// those of them the person named to take along.
pub fn export(to: &Path, local_only: &[ProjectKey], include: &[ProjectKey]) -> anyhow::Result<Exported> {
    let stage = tempfile::tempdir().context("no temporary folder for the export")?;
    let left_out: Vec<ProjectKey> = local_only.iter().filter(|k| !include.contains(k)).cloned().collect();
    let kept = |file: &MemFile| file.project().is_none_or(|key| !left_out.contains(key));
    // Memory, as of Glasscode's last commit.
    let snap = Memory::open().snapshot()?;
    let files: Vec<MemFile> = snap.files()?.into_iter().filter(|f| kept(f) && !matches!(f, MemFile::Rejected)).collect();
    let texts = snap.texts(&files)?;
    let mut cited = BTreeSet::new();
    for (file, text) in files.iter().zip(&texts) {
        let Some(text) = text else { continue };
        write(&stage.path().join("memory").join(file.rel()), text.as_bytes())?;
        for e in Doc::parse(text).entries() {
            if let Source::Words(id) = &e.header.source {
                cited.insert(id.clone());
            }
        }
    }
    // The stores the data map marks for export.
    let root = paths::root();
    let mut other = 0;
    for row in DATA_MAP.iter().filter(|row| row.export.starts_with("yes") && !row.path.starts_with("memory")) {
        let path = row.path.split('<').next().unwrap_or(row.path).trim_end_matches('/');
        let source = root.join(path);
        other += copy_tree(&root, &source, &stage.path().join("data"), &left_out)?;
    }
    // The authority store, but this machine's own files and the device keys.
    other += copy_tree(&root, &paths::authority_dir(), &stage.path().join("data"), &left_out)?;
    // The words records memory's facts cite.
    let words: Vec<String> = cited_records(&cited);
    if !words.is_empty() {
        write(&stage.path().join("words.jsonl"), (words.join("\n") + "\n").as_bytes())?;
    }
    let manifest = Manifest { format: FORMAT, made_ms: crate::clock::now_ms(), memory_commit: snap.commit().map(str::to_string), left_out: left_out.clone() };
    write(&stage.path().join("manifest.json"), serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    if let Some(folder) = to.parent() {
        std::fs::create_dir_all(folder)?;
    }
    run_tar(&["-cf", &to.to_string_lossy(), "-C", &stage.path().to_string_lossy(), "."])?;
    Ok(Exported { path: to.to_path_buf(), memory_files: files.len(), other_files: other, left_out })
}

/// Copy the files of `source` (a file or a folder below `root`) into `into`, at the same path
/// below it; files of a left-out project's folder (`…/<key>/…`) and [`NOT_EXPORTED`] stay. How
/// many it copied.
fn copy_tree(root: &Path, source: &Path, into: &Path, left_out: &[ProjectKey]) -> anyhow::Result<usize> {
    let mut count = 0;
    let mut stack = vec![source.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        let Ok(rel) = path.strip_prefix(root) else { continue };
        let rel_text = rel.to_string_lossy().replace('\\', "/");
        if NOT_EXPORTED.contains(&rel_text.as_str()) || rel_text.ends_with(".lock") || rel_text.ends_with(".tmp") || left_out.iter().any(|key| rel_text.split('/').any(|part| part == key.as_str())) {
            continue;
        }
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else if meta.is_file() {
            let target = into.join(rel);
            if target.exists() {
                continue;
            }
            if let Some(folder) = target.parent() {
                std::fs::create_dir_all(folder)?;
            }
            std::fs::copy(&path, &target)?;
            count += 1;
        }
    }
    Ok(count)
}

/// The lines of the words ledger, of every machine, whose record `cited` names.
fn cited_records(cited: &BTreeSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for file in jsonl_files(&paths::root().join("words")) {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        for line in text.lines() {
            let id = serde_json::from_str::<serde_json::Value>(line).ok().and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string));
            if id.is_some_and(|id| cited.contains(&id)) {
                out.push(line.to_string());
            }
        }
    }
    out
}

fn write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    std::fs::write(path, bytes).with_context(|| format!("could not write {}", path.display()))
}

fn run_tar(args: &[&str]) -> anyhow::Result<()> {
    let output = crate::sys::command("tar").args(args).output().context("could not run tar")?;
    if !output.status.success() {
        bail!("tar: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

/// Every `.jsonl` file below a folder.
fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
    out.sort();
    out
}

// ----- import ----------------------------------------------------------------------------------

/// What an import did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    pub memory_files: usize,
    pub project_keys: usize,
    pub words_records: usize,
    /// Authority files put aside for the person to approve again.
    pub set_aside: usize,
}

/// Import an export (`from`, a `.tar` file) into this Glasscode folder.
pub fn import(from: &Path) -> anyhow::Result<Imported> {
    let stage = tempfile::tempdir().context("no temporary folder for the import")?;
    run_tar(&["-xf", &from.to_string_lossy(), "-C", &stage.path().to_string_lossy()])?;
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(stage.path().join("manifest.json")).context("this is no Glasscode export (no manifest)")?)?;
    if manifest.format > FORMAT {
        bail!("this export was made by a newer Glasscode (format {}); update first", manifest.format);
    }
    // Memory, merged entry by entry.
    let mut docs = Vec::new();
    let memory_dir = stage.path().join("memory");
    let mut stack = vec![memory_dir.clone()];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else if let Ok(rel) = path.strip_prefix(&memory_dir)
            && let Some(file) = MemFile::parse(&rel.to_string_lossy())
            && !matches!(file, MemFile::Index | MemFile::Rejected)
        {
            docs.push((file, Doc::parse(&std::fs::read_to_string(&path)?)));
        }
    }
    docs.sort_by(|a, b| a.0.cmp(&b.0));
    Memory::open().import_docs(&docs)?;
    // The project keys: their remotes and roots, so this machine finds them by its own folders.
    let data = stage.path().join("data");
    let mut keys = 0;
    if let Ok(text) = std::fs::read_to_string(data.join("authority").join("projects.json"))
        && let Ok(envelope) = serde_json::from_str::<serde_json::Value>(&text)
        && let Ok(theirs) = serde_json::from_value::<Projects>(envelope["data"].clone())
    {
        Projects::update(|ours| {
            for (key, entry) in theirs.keys {
                let mine = ours.keys.entry(key).or_default();
                for remote in entry.remotes {
                    if !mine.remotes.contains(&remote) {
                        mine.remotes.push(remote);
                    }
                }
                for root in entry.roots {
                    if !mine.roots.contains(&root) {
                        mine.roots.push(root);
                    }
                }
                keys += 1;
            }
        })?;
    }
    // The cited words records, in their machine's folder, each once.
    let mut records = 0;
    if let Ok(text) = std::fs::read_to_string(stage.path().join("words.jsonl")) {
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(record) = serde_json::from_str::<crate::words::WordsRecord>(line) else { continue };
            if crate::words::find(&record.id).is_some() {
                continue;
            }
            let file = paths::words_dir(&record.at.machine).join(format!("{}.jsonl", month_of(record.at.wall_ms)));
            crate::words::append_lines(&file, &[line.to_string()])?;
            records += 1;
        }
    }
    // The rest of the authority store: put aside, for the person to approve again (D10).
    let mut set_aside = 0;
    let imported = paths::authority_dir().join("imported");
    let theirs = data.join("authority");
    let mut stack = vec![theirs.clone()];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else if let Ok(rel) = path.strip_prefix(&theirs)
            && rel != Path::new("projects.json")
        {
            let target = imported.join(rel);
            if let Some(folder) = target.parent() {
                std::fs::create_dir_all(folder)?;
            }
            std::fs::copy(&path, &target)?;
            set_aside += 1;
        }
    }
    Ok(Imported { memory_files: docs.len(), project_keys: keys, words_records: records, set_aside })
}

fn month_of(at_ms: u64) -> String {
    memstore::layout::month_of(at_ms)
}

// ----- forget ----------------------------------------------------------------------------------

/// What to forget (design section 6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Forget {
    /// Everything that names a person.
    Person(String),
    /// A project: its memory folder, agent notes, key and journal lines.
    Project(ProjectKey),
    /// Every project that names the employer, and its folder.
    Employer(Employer),
}

/// What a forget did, by store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Forgotten {
    pub memory: bool,
    pub words_records: usize,
    pub journal_lines: usize,
    pub effect_lines: usize,
    /// Moving in's studies and questions (`manager/movein.json`).
    pub movein: usize,
    pub projects: Vec<ProjectKey>,
}

/// What a person's name is replaced by in the journal and the effect ledger.
pub const FORGOTTEN: &str = "[forgotten]";

/// Forget a person, a project or an employer everywhere Glasscode keeps it.
pub fn forget(what: &Forget) -> anyhow::Result<Forgotten> {
    let memory = Memory::open();
    let mut out = Forgotten::default();
    match what {
        Forget::Person(name) => {
            let name = name.trim();
            if name.chars().count() < 2 || name.contains(['"', '\\']) {
                bail!("say the person's name (two letters or more, no quotation marks)");
            }
            out.memory = memory.forget_matching(name)?.history_restarted;
            // The records that name them, by id first: the server took them in too (M57).
            let ids: std::cell::RefCell<Vec<crate::words::WordsId>> = std::cell::RefCell::new(Vec::new());
            out.words_records = rewrite_lines(&paths::root().join("words"), |line| {
                if !contains_folded(line, name) {
                    return Some(line.to_string());
                }
                if let Ok(record) = serde_json::from_str::<crate::words::WordsRecord>(line) {
                    ids.borrow_mut().push(record.id);
                }
                None
            })?;
            memstore::sync::ask_server_to_forget_words(&ids.into_inner());
            // A person's forget leaves no history of them on the server either.
            crate::store::update::<memstore::sync::ServerForget, _>(|request| request.restart = true).map_err(|e| anyhow::anyhow!(e))?;
            out.journal_lines = rewrite_lines(&paths::journal_dir(), |line| Some(replace_folded(line, name, FORGOTTEN)))?;
            out.effect_lines = rewrite_lines(&paths::effects_dir(), |line| Some(replace_folded(line, name, FORGOTTEN)))?;
            out.movein = crate::store::update::<crate::movein::Progress, _>(|progress| progress.forget_person(name)).map_err(|e| anyhow::anyhow!(e))?;
        }
        Forget::Project(key) => {
            out.memory = forget_projects(&memory, std::slice::from_ref(key))?;
            out.movein = forget_movein(std::slice::from_ref(key))?;
            out.journal_lines = drop_project_lines(key)?;
            out.projects.push(key.clone());
        }
        Forget::Employer(employer) => {
            let snap = memory.snapshot()?;
            let mut keys: Vec<ProjectKey> = Vec::new();
            for file in snap.files()? {
                if let MemFile::Project(key) = &file
                    && snap.employer_of(key)?.as_ref() == Some(employer)
                {
                    keys.push(key.clone());
                }
            }
            let employer = employer.clone();
            let org_folder = format!("org/{}", employer.as_str());
            let org = memory.forget_files(move |file| matches!(file, MemFile::People(e) | MemFile::Glossary(e) if *e == employer))?;
            ask_server_to_drop([org_folder])?;
            out.memory = forget_projects(&memory, &keys)? || org.history_restarted;
            out.movein = forget_movein(&keys)?;
            for key in &keys {
                out.journal_lines += drop_project_lines(key)?;
            }
            out.projects = keys;
        }
    }
    Ok(out)
}

/// A project's memory folder, agent notes and key go.
fn forget_projects(memory: &Memory, keys: &[ProjectKey]) -> anyhow::Result<bool> {
    if keys.is_empty() {
        return Ok(false);
    }
    let set: Vec<ProjectKey> = keys.to_vec();
    let forgot = memory.forget_files(move |file| file.project().is_some_and(|key| set.contains(key)))?;
    ask_server_to_drop(keys.iter().map(|key| format!("projects/{}", key.as_str())))?;
    for key in keys {
        let notes = paths::root().join("agent-notes").join(key.as_str());
        if notes.exists() {
            std::fs::remove_dir_all(&notes).with_context(|| format!("could not remove {}", notes.display()))?;
        }
    }
    let keys = keys.to_vec();
    Projects::update(|projects| {
        for key in &keys {
            projects.keys.remove(key);
        }
    })
    .map_err(|e: StoreError| anyhow::anyhow!(e))?;
    Ok(forgot.history_restarted)
}

/// A server with this memory's entries lets these folders go at the next sync (M57).
fn ask_server_to_drop(folders: impl IntoIterator<Item = String>) -> anyhow::Result<()> {
    let folders: Vec<String> = folders.into_iter().collect();
    crate::store::update::<crate::memstore::sync::ServerForget, _>(|request| request.drop_folders(folders)).map_err(|e| anyhow::anyhow!(e))
}

/// Moving in's studies and questions of the projects go. How many.
fn forget_movein(keys: &[ProjectKey]) -> anyhow::Result<usize> {
    if keys.is_empty() {
        return Ok(0);
    }
    crate::store::update::<crate::movein::Progress, _>(|progress| progress.forget_projects(keys)).map_err(|e| anyhow::anyhow!(e))
}

/// The journal's lines of a project go. How many.
fn drop_project_lines(key: &ProjectKey) -> anyhow::Result<usize> {
    let quoted = format!("\"project\":\"{}\"", key.as_str());
    rewrite_lines(&paths::journal_dir(), |line| (!line.replace(' ', "").contains(&quoted)).then(|| line.to_string()))
}

/// Rewrite every JSON-lines file below `dir`: each line kept as `change` gives it, or dropped. Under
/// each file's `.lock` (the writers' lock), through a temporary file. How many lines changed.
fn rewrite_lines(dir: &Path, change: impl Fn(&str) -> Option<String>) -> anyhow::Result<usize> {
    let mut changed = 0;
    for file in jsonl_files(dir) {
        let mut lock_name = file.clone().into_os_string();
        lock_name.push(".lock");
        let _lock = crate::sys::FileLock::take(Path::new(&lock_name))?;
        let text = std::fs::read_to_string(&file)?;
        let mut out = String::with_capacity(text.len());
        let mut here = 0;
        for line in text.lines() {
            match change(line) {
                Some(kept) => {
                    if kept != line {
                        here += 1;
                    }
                    out.push_str(&kept);
                    out.push('\n');
                }
                None => here += 1,
            }
        }
        if here > 0 {
            let temp = file.with_extension("jsonl.forget.tmp");
            std::fs::write(&temp, &out)?;
            std::fs::rename(&temp, &file)?;
            changed += here;
        }
    }
    Ok(changed)
}

fn contains_folded(text: &str, name: &str) -> bool {
    text.to_lowercase().contains(&name.to_lowercase())
}

/// `text` with every `name` in it (any letter case) replaced.
fn replace_folded(text: &str, name: &str, with: &str) -> String {
    let lower = text.to_lowercase();
    let needle = name.to_lowercase();
    if !lower.contains(&needle) || lower.len() != text.len() {
        return if lower.len() == text.len() { text.to_string() } else { text.replace(name, with) };
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(found) = lower[at..].find(&needle) {
        out.push_str(&text[at..at + found]);
        out.push_str(with);
        at += found + needle.len();
    }
    out.push_str(&text[at..]);
    out
}

// ----- scheduled export --------------------------------------------------------------------------

/// The scheduled export (`manager/export-schedule.json`): how often, where, how many to keep, and
/// how the last one went.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportSchedule {
    /// Days between exports; 0: none.
    pub every_days: u32,
    /// Where they go; none: "Glasscode exports" in Downloads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    /// How many to keep; 0: [`KEEP_EXPORTS`].
    pub keep: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl crate::store::Store for ExportSchedule {
    const NAME: &'static str = "manager/export-schedule";
    const VERSION: u32 = 1;
    const ON_DAMAGE: crate::store::OnDamage = crate::store::OnDamage::Empty;

    fn path() -> PathBuf {
        paths::root().join("manager").join("export-schedule.json")
    }
}

/// How many scheduled exports are kept when the schedule names none.
pub const KEEP_EXPORTS: u32 = 4;

/// The name every scheduled export starts with, so pruning touches only those.
const SCHEDULED_PREFIX: &str = "glasscode-export-";

impl ExportSchedule {
    /// Whether an export is due at `now_ms`.
    pub fn due(&self, now_ms: u64) -> bool {
        self.every_days > 0 && self.last_ms.is_none_or(|last| now_ms >= last + u64::from(self.every_days) * 86_400_000)
    }

    pub fn folder(&self) -> PathBuf {
        self.folder.clone().unwrap_or_else(default_export_folder)
    }

    pub fn keep(&self) -> usize {
        (if self.keep == 0 { KEEP_EXPORTS } else { self.keep }) as usize
    }

    /// One line for Settings and the command.
    pub fn line(&self) -> String {
        let every = match self.every_days {
            0 => return "No scheduled export.".into(),
            1 => "every day".to_string(),
            7 => "every week".to_string(),
            n => format!("every {n} days"),
        };
        let last = match (&self.last_error, &self.last_file) {
            (Some(error), _) => format!(" The last one failed: {error}."),
            (None, Some(file)) => format!(" The last one: {}.", file.display()),
            _ => String::new(),
        };
        format!("An export {every} to {}, keeping the newest {}.{last}", self.folder().display(), self.keep())
    }
}

/// "Glasscode exports" in Downloads (in a test run, in the test's own folder).
pub fn default_export_folder() -> PathBuf {
    person_home().join("Downloads").join("Glasscode exports")
}

/// The export the schedule asks for, when one is due at `now_ms`: written to its folder as
/// `glasscode-export-<date>-<time>.tar` (projects that stay on this Mac left out), then only the
/// newest few of those kept. How it went is kept in the schedule. None: none was due.
pub fn scheduled_export(now_ms: u64, local_only: &[ProjectKey]) -> anyhow::Result<Option<PathBuf>> {
    use crate::store::Store as _;
    let schedule = ExportSchedule::load().map_err(|e| anyhow::anyhow!(e))?;
    if !schedule.due(now_ms) {
        return Ok(None);
    }
    let folder = schedule.folder();
    let secs = (now_ms / 1000) as i64;
    let stamp = format!("{}-{}", crate::day::iso(crate::day::local(secs)), crate::day::clock(secs).replace(':', ""));
    let file = folder.join(format!("{SCHEDULED_PREFIX}{stamp}.tar"));
    let done = std::fs::create_dir_all(&folder).with_context(|| format!("could not make {}", folder.display())).and_then(|()| export(&file, local_only, &[]));
    let (last_file, last_error) = match &done {
        Ok(exported) => (Some(exported.path.clone()), None),
        Err(err) => (None, Some(format!("{err:#}"))),
    };
    crate::store::update::<ExportSchedule, _>(|stored| {
        stored.last_ms = Some(now_ms);
        stored.last_error = last_error;
        if last_file.is_some() {
            stored.last_file = last_file;
        }
    })
    .map_err(|e| anyhow::anyhow!(e))?;
    let exported = done?;
    prune_exports(&folder, schedule.keep())?;
    Ok(Some(exported.path))
}

/// Keep the newest `keep` scheduled exports in `folder` (by their dated names); only files named
/// as the schedule names them are touched. How many went.
fn prune_exports(folder: &Path, keep: usize) -> anyhow::Result<usize> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)?.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(SCHEDULED_PREFIX) && n.ends_with(".tar"))).collect();
    files.sort();
    let gone = files.len().saturating_sub(keep);
    for old in &files[..gone] {
        std::fs::remove_file(old).with_context(|| format!("could not remove {}", old.display()))?;
    }
    Ok(gone)
}

// ----- move out --------------------------------------------------------------------------------

/// What [`move_out`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MovedOut {
    /// Files written, each a note that was not there.
    pub written: Vec<PathBuf>,
    /// Notes already there as they are.
    pub same: usize,
}

/// Move out: each project's Claude notes (`agent-notes/<key>/claude/`, where Glasscode redirects
/// Claude's own memory, D19) go back to the memory folder of that project in each Claude account (`claude_dirs`), so
/// Claude keeps what it learned while Glasscode was here. A note is never written over: one that is
/// there with other text is written beside it as `<name>-from-glasscode.md`. `projects`: each
/// project's key and main folder. `dry_run`: only say what it would write. It reads and writes the
/// disk: never on the main thread.
pub fn move_out(projects: &[(ProjectKey, PathBuf)], claude_dirs: &[PathBuf], dry_run: bool) -> anyhow::Result<MovedOut> {
    let mut out = MovedOut::default();
    for (key, root) in projects {
        let notes = paths::agent_notes_dir(key, crate::model::AgentKind::Claude);
        let Ok(entries) = std::fs::read_dir(&notes) else { continue };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md")).collect();
        files.sort();
        for account in claude_dirs {
            let to = crate::agents::claude_memory_dir(account, root);
            for file in &files {
                let text = std::fs::read_to_string(file).with_context(|| format!("could not read {}", file.display()))?;
                let Some(name) = file.file_name() else { continue };
                let mut target = to.join(name);
                if let Ok(there) = std::fs::read_to_string(&target) {
                    if there == text {
                        out.same += 1;
                        continue;
                    }
                    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("note");
                    target = to.join(format!("{stem}-from-glasscode.md"));
                    if std::fs::read_to_string(&target).is_ok_and(|there| there == text) {
                        out.same += 1;
                        continue;
                    }
                }
                if !dry_run {
                    std::fs::create_dir_all(&to).with_context(|| format!("could not make {}", to.display()))?;
                    std::fs::write(&target, &text).with_context(|| format!("could not write {}", target.display()))?;
                }
                out.written.push(target);
            }
        }
    }
    Ok(out)
}

// ----- uninstall -------------------------------------------------------------------------------

/// One thing Glasscode made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Made {
    pub path: PathBuf,
    pub what: String,
    /// Why removing it waits for the person (work that is nowhere else).
    pub blocks: Option<String>,
    /// Whether `uninstall` removes it (false: it is listed for the person to remove by hand).
    pub removed: bool,
    pub kind: MadeKind,
}

/// What kind of thing Glasscode made, so uninstall knows how to take it away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MadeKind {
    /// A folder inside the Glasscode folder (or the folder itself).
    Folder,
    /// A Codex `config.toml` outside it, with trust tables for Glasscode's worktrees: only those go.
    CodexTrust,
    /// A key in the system's keychain, by its service.
    Keychain(&'static str),
    /// Only said (Time Machine).
    Note,
}

/// The person's home as uninstall and the scheduled export reach it: in a test run, the test's
/// own Glasscode folder, so a test never touches the person's `~/.codex` or Downloads.
fn person_home() -> PathBuf {
    if paths::testing() { paths::root() } else { paths::home() }
}

/// The Codex folders outside the Glasscode folder whose `config.toml` may hold trust tables for
/// Glasscode's worktrees: the person's own `~/.codex` (or `CODEX_HOME`), and each Codex account's
/// folder of their own choosing.
fn codex_homes() -> Vec<PathBuf> {
    let root = paths::root();
    let mut homes = vec![std::env::var_os("CODEX_HOME").filter(|_| !paths::testing()).map(PathBuf::from).unwrap_or_else(|| person_home().join(".codex"))];
    if let Ok(text) = std::fs::read_to_string(paths::state_file())
        && let Ok(state) = serde_json::from_str::<crate::model::State>(&text)
    {
        for account in state.accounts.iter().filter(|a| a.agent == crate::model::AgentKind::Codex) {
            let dir = account.effective_dir();
            if !dir.starts_with(&root) && !homes.contains(&dir) {
                homes.push(dir);
            }
        }
    }
    homes
}

/// The keys Glasscode keeps in the system's keychain, by service.
const KEYCHAIN: [&str; 2] = ["Glasscode Linear", "glasscode-manager-voice"];

/// Everything Glasscode made on this computer, and what uninstall would do with each.
pub fn uninstall_plan() -> Vec<Made> {
    let root = paths::root();
    let mut out = Vec::new();
    // Worktrees first: one with work that is nowhere else stops the uninstall.
    for project in std::fs::read_dir(paths::worktrees_dir()).into_iter().flatten().flatten() {
        for worktree in std::fs::read_dir(project.path()).into_iter().flatten().flatten() {
            let path = worktree.path();
            if !path.join(".git").exists() {
                continue;
            }
            let wt = crate::git::Worktree { path: path.clone(), branch: crate::git::current_branch(&path), head: String::new(), is_main: false, locked: false, prunable: false };
            let check = crate::git::check_worktree(&wt);
            let blocks = (!check.is_clean()).then(|| format!("{} uncommitted, {} commits only here", check.changed, check.lost_commits));
            out.push(Made { path, what: "a worktree".into(), blocks, removed: true, kind: MadeKind::Folder });
        }
    }
    out.push(Made { path: root.clone(), what: "the Glasscode folder: memory, the authority store, the journal, accounts' login folders, settings, caches".into(), blocks: None, removed: true, kind: MadeKind::Folder });
    for home in codex_homes() {
        let config = home.join("config.toml");
        let trusted = std::fs::read_to_string(&config).map(|text| crate::agents::without_codex_trust(&text, &paths::worktrees_dir()).1).unwrap_or(0);
        if trusted > 0 {
            out.push(Made { path: config, what: format!("{trusted} trust entries Glasscode added for its worktrees (only those go; the rest of the file stays)"), blocks: None, removed: true, kind: MadeKind::CodexTrust });
        }
    }
    for service in KEYCHAIN {
        if paths::testing() || crate::keychain::read(service, keychain_account(service)).is_some() {
            out.push(Made { path: PathBuf::from(format!("{}: {service}", crate::keychain::NAME)), what: "a key Glasscode kept in the system's keychain".into(), blocks: None, removed: true, kind: MadeKind::Keychain(service) });
        }
    }
    out.push(Made { path: PathBuf::from("Time Machine"), what: "backups made while Glasscode was here keep copies of the Glasscode folder until they expire".into(), blocks: None, removed: false, kind: MadeKind::Note });
    out
}

/// The account a keychain service's key is kept under.
fn keychain_account(service: &str) -> &'static str {
    match service {
        "Glasscode Linear" => crate::linear::KEYCHAIN_ACCOUNT,
        _ => crate::manager::voice::KEYCHAIN_ACCOUNT,
    }
}

/// Remove what [`uninstall_plan`] says it removes. Refused while a worktree has work that is
/// nowhere else, unless `force`. The person confirms in the app first.
pub fn uninstall(plan: &[Made], force: bool) -> anyhow::Result<usize> {
    if !force && let Some(blocked) = plan.iter().find(|m| m.blocks.is_some()) {
        bail!("{} has work that is nowhere else ({}): push or remove it first", blocked.path.display(), blocked.blocks.as_deref().unwrap_or(""));
    }
    let root = paths::root();
    let worktrees = paths::worktrees_dir();
    let mut removed = 0;
    // What is outside the Glasscode folder first: the trust tables of its worktrees and its keys.
    for made in plan.iter().filter(|m| m.removed) {
        match made.kind {
            MadeKind::CodexTrust => {
                // Only a Codex folder this computer's uninstall would name, whatever the plan says.
                if !codex_homes().iter().any(|home| home.join("config.toml") == made.path) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&made.path) else { continue };
                let (kept, gone) = crate::agents::without_codex_trust(&text, &worktrees);
                if gone > 0 {
                    let temp = made.path.with_extension("toml.glasscode.tmp");
                    std::fs::write(&temp, kept).with_context(|| format!("could not write {}", temp.display()))?;
                    std::fs::rename(&temp, &made.path).with_context(|| format!("could not change {}", made.path.display()))?;
                    removed += 1;
                }
            }
            MadeKind::Keychain(service) if KEYCHAIN.contains(&service) => {
                // A test never reaches the person's keychain.
                if !paths::testing() {
                    crate::keychain::delete(service, keychain_account(service));
                }
                removed += 1;
            }
            _ => {}
        }
    }
    for made in plan.iter().filter(|m| m.removed && m.kind == MadeKind::Folder) {
        // Only inside the Glasscode folder, whatever the plan says.
        if !made.path.starts_with(&root) || !made.path.exists() {
            continue;
        }
        std::fs::remove_dir_all(&made.path).with_context(|| format!("could not remove {}", made.path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memstore::tests_support::{Scratch, chat, key};
    use crate::memstore::{Remembered, Who, search};
    use crate::store::Store as _;

    fn remember(memory: &Memory, file: &MemFile, fact: &str) -> String {
        let Remembered::Entry { id, .. } = memory.remember(&chat(&format!("Remember that {fact}.")), fact, fact, file, None).unwrap() else { panic!("{fact}") };
        id
    }

    #[test]
    fn export_import_round_trip() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let app = key("app-3f9a1c");
        let secret = key("secret-111111");
        remember(&memory, &MemFile::Decisions(app.clone()), "app releases go out on Tuesdays");
        remember(&memory, &MemFile::Decisions(secret.clone()), "the secret project ships in May");
        memory.note("s-1", &crate::identity::WorkKey { project: app.clone(), kind: crate::identity::WorkKind::Branch, id: "feat/x".into() }, "the login form uses useAuthForm").unwrap();
        let file = scratch.at("out/export.tar");
        let exported = export(&file, std::slice::from_ref(&secret), &[]).unwrap();
        assert_eq!(exported.left_out, [secret.clone()]);
        assert!(file.is_file());
        // Into an empty home.
        let _second = Scratch::new();
        let imported = import(&file).unwrap();
        assert!(imported.memory_files >= 2, "{imported:?}");
        let snap = Memory::open().snapshot().unwrap();
        let all: String = snap.texts(&snap.files().unwrap()).unwrap().into_iter().flatten().collect();
        assert!(all.contains("app releases go out on Tuesdays") && all.contains("useAuthForm"));
        assert!(!all.contains("secret project"), "a project that stays on its Mac is left out");
        // Twice changes nothing.
        let again = import(&file).unwrap();
        assert_eq!(again.words_records, 0);
        assert_eq!(Memory::open().snapshot().unwrap().commit(), snap.commit());
    }

    #[test]
    fn restore_after_damage() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let app = key("app-3f9a1c");
        remember(&memory, &MemFile::Decisions(app.clone()), "the staging database runs on port 5432");
        let file = scratch.at("export.tar");
        export(&file, &[], &[]).unwrap();
        // Memory is lost: its folder and its head record.
        std::fs::remove_dir_all(memory.dir()).unwrap();
        std::fs::remove_file(crate::memstore::MemoryHead::path()).unwrap();
        let _ = std::fs::remove_file(crate::memstore::MemoryHead::path().with_extension("json.prev"));
        import(&file).unwrap();
        let snap = Memory::open().snapshot().unwrap();
        let scope = search::scope_for(&snap, Some(&app), None).unwrap();
        let hits = search::search(&snap, "Which port does the staging database use?", &scope, 5).unwrap();
        assert!(hits.iter().any(|h| h.entries.iter().any(|e| e.text.contains("5432"))), "{hits:?}");
    }

    #[test]
    fn forget_person_removes_everywhere() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let app = key("app-3f9a1c");
        remember(&memory, &MemFile::Decisions(app.clone()), "Priya Sharma owns the billing service");
        remember(&memory, &MemFile::Decisions(app.clone()), "releases go out on Tuesdays");
        // Her name in the words ledger, the journal and the effect ledger.
        let machine = crate::paths::MachineId::this().unwrap();
        let words = paths::words_dir(&machine).join("2026-11.jsonl");
        std::fs::create_dir_all(words.parent().unwrap()).unwrap();
        std::fs::write(&words, "{\"id\":\"w1\",\"text\":\"ask Priya Sharma\"}\n{\"id\":\"w2\",\"text\":\"ship it\"}\n").unwrap();
        std::fs::create_dir_all(paths::journal_dir()).unwrap();
        std::fs::write(paths::journal_dir().join("2026-11-20.jsonl"), "{\"summary\":\"Priya Sharma reviewed #42\"}\n").unwrap();
        std::fs::create_dir_all(paths::effects_dir()).unwrap();
        std::fs::write(paths::effects_dir().join("2026-11.jsonl"), "{\"reason\":\"message to priya sharma\"}\n").unwrap();
        let forgotten = forget(&Forget::Person("Priya Sharma".into())).unwrap();
        assert!(forgotten.memory);
        assert_eq!((forgotten.words_records, forgotten.journal_lines, forgotten.effect_lines), (1, 1, 1));
        let dir = memory.dir().to_path_buf();
        let objects = scratch.git(&dir, &["cat-file", "--batch-all-objects", "--batch"]);
        assert!(!objects.to_lowercase().contains("priya"), "in no object of memory");
        for file in [words, paths::journal_dir().join("2026-11-20.jsonl"), paths::effects_dir().join("2026-11.jsonl")] {
            assert!(!std::fs::read_to_string(&file).unwrap().to_lowercase().contains("priya"), "{}", file.display());
        }
        assert!(std::fs::read_to_string(paths::journal_dir().join("2026-11-20.jsonl")).unwrap().contains("[forgotten] reviewed #42"));
        let snap = Memory::open().snapshot().unwrap();
        assert!(snap.doc(&MemFile::Decisions(app)).unwrap().entries().any(|e| e.text.contains("Tuesdays")), "the rest stays");
    }

    #[test]
    fn forget_project() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let (app, web) = (key("app-3f9a1c"), key("web-8b2d10"));
        remember(&memory, &MemFile::Decisions(app.clone()), "the app deploys from main");
        remember(&memory, &MemFile::Decisions(web.clone()), "the web deploys on Fridays");
        let notes = paths::root().join("agent-notes").join(app.as_str()).join("claude");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("MEMORY.md"), "x").unwrap();
        std::fs::create_dir_all(paths::journal_dir()).unwrap();
        std::fs::write(paths::journal_dir().join("2026-11-20.jsonl"), format!("{{\"project\":\"{app}\",\"summary\":\"a\"}}\n{{\"project\":\"{web}\",\"summary\":\"b\"}}\n")).unwrap();
        let forgotten = forget(&Forget::Project(app.clone())).unwrap();
        assert_eq!(forgotten.journal_lines, 1);
        assert!(!notes.exists());
        let snap = Memory::open().snapshot().unwrap();
        assert!(snap.files().unwrap().iter().all(|f| f.project() != Some(&app)));
        assert!(snap.doc(&MemFile::Decisions(web.clone())).unwrap().entries().count() == 1);
        let objects = scratch.git(&memory.dir().to_path_buf(), &["cat-file", "--batch-all-objects", "--batch"]);
        assert!(!objects.contains("the app deploys from main"));
        assert!(std::fs::read_to_string(paths::journal_dir().join("2026-11-20.jsonl")).unwrap().contains(web.as_str()));
        // An employer: every project that names it, and its folder.
        memory.set_employer(&Who::You, &web, &Employer::parse("acme").unwrap()).unwrap();
        remember(&memory, &MemFile::People(Employer::parse("acme").unwrap()), "Sam leads acme payments");
        let forgotten = forget(&Forget::Employer(Employer::parse("acme").unwrap())).unwrap();
        assert_eq!(forgotten.projects, [web]);
        assert!(Memory::open().snapshot().unwrap().files().unwrap().iter().all(|f| matches!(f, MemFile::Index | MemFile::Proposals)));
    }

    #[test]
    fn uninstall_lists_and_removes_only_its_own() {
        let _scratch = Scratch::new();
        Memory::open().remember(&chat("Remember that Sam owns billing."), "Sam owns billing", "Sam owns billing", &MemFile::Profile, None).unwrap();
        let plan = uninstall_plan();
        assert!(plan.iter().any(|m| m.path == paths::root() && m.removed));
        assert!(plan.iter().any(|m| m.what.contains("Time Machine") || m.path == Path::new("Time Machine")));
        // Never outside the Glasscode folder, whatever a plan says.
        let outside = tempfile::tempdir().unwrap();
        let bad = vec![Made { path: outside.path().to_path_buf(), what: "x".into(), blocks: None, removed: true, kind: MadeKind::Folder }, Made { path: outside.path().join("config.toml"), what: "x".into(), blocks: None, removed: true, kind: MadeKind::CodexTrust }];
        assert_eq!(uninstall(&bad, false).unwrap(), 0);
        assert!(outside.path().exists());
        // The folder, and the two keys (a test never reaches the keychain itself).
        assert_eq!(uninstall(&plan, false).unwrap(), 3);
        assert!(!paths::root().exists());
    }

    #[test]
    fn a_scheduled_export_comes_when_due_and_keeps_the_newest() {
        let scratch = Scratch::new();
        use crate::store::Store as _;
        remember(&Memory::open(), &MemFile::Profile, "releases go out on Tuesdays");
        let day = 86_400_000u64;
        let start = 1_795_000_000_000u64;
        assert_eq!(scheduled_export(start, &[]).unwrap(), None, "no schedule, no export");
        let folder = scratch.at("backups");
        ExportSchedule { every_days: 1, folder: Some(folder.clone()), keep: 2, ..ExportSchedule::default() }.save().unwrap();
        let first = scheduled_export(start, &[]).unwrap().unwrap();
        assert!(first.is_file() && first.starts_with(&folder));
        assert_eq!(scheduled_export(start + day / 2, &[]).unwrap(), None, "not due yet");
        // A file of the person's own in the folder is never touched.
        std::fs::write(folder.join("notes.tar"), "mine").unwrap();
        for n in 1..=3 {
            scheduled_export(start + n * day, &[]).unwrap().unwrap();
        }
        let mut names: Vec<String> = std::fs::read_dir(&folder).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.contains(&"notes.tar".to_string()) && !folder.join(first.file_name().unwrap()).exists());
        let schedule = ExportSchedule::load().unwrap();
        assert!(schedule.last_error.is_none() && schedule.line().contains("every day") && schedule.line().contains("newest 2"));
    }

    #[test]
    fn move_out_gives_claude_its_notes_back_and_writes_over_nothing() {
        let scratch = Scratch::new();
        let app = key("app-3f9a1c");
        let root = scratch.at("code/app");
        let notes = paths::agent_notes_dir(&app, crate::model::AgentKind::Claude);
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("MEMORY.md"), "- [Build](build.md)\n").unwrap();
        std::fs::write(notes.join("build.md"), "Run make setup first.\n").unwrap();
        let account = scratch.at("claude-account");
        let theirs = crate::agents::claude_memory_dir(&account, &root);
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(theirs.join("MEMORY.md"), "- [Own](own.md)\n").unwrap();
        let projects = [(app.clone(), root.clone())];
        let dry = move_out(&projects, std::slice::from_ref(&account), true).unwrap();
        assert_eq!(dry.written.len(), 2);
        assert!(!theirs.join("build.md").exists(), "a dry run writes nothing");
        let done = move_out(&projects, std::slice::from_ref(&account), false).unwrap();
        assert_eq!(std::fs::read_to_string(theirs.join("build.md")).unwrap(), "Run make setup first.\n");
        assert_eq!(std::fs::read_to_string(theirs.join("MEMORY.md")).unwrap(), "- [Own](own.md)\n", "Claude's own index stays");
        assert_eq!(std::fs::read_to_string(theirs.join("MEMORY-from-glasscode.md")).unwrap(), "- [Build](build.md)\n");
        assert_eq!(done.written.len(), 2);
        // Twice: nothing new.
        let again = move_out(&projects, std::slice::from_ref(&account), false).unwrap();
        assert!(again.written.is_empty() && again.same == 2);
    }

    #[test]
    fn uninstall_takes_only_its_own_codex_trust() {
        let _scratch = Scratch::new();
        let codex = person_home().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        let worktree = paths::worktrees_dir().join("app").join("feat-x");
        let config = format!("model = \"x\"\n\n[projects.\"/work/own\"]\ntrust_level = \"trusted\"\n\n[projects.{}]\ntrust_level = \"trusted\"\n", serde_json::to_string(worktree.to_str().unwrap()).unwrap());
        std::fs::write(codex.join("config.toml"), &config).unwrap();
        let trust: Vec<Made> = uninstall_plan().into_iter().filter(|m| m.kind == MadeKind::CodexTrust).collect();
        assert_eq!(trust.len(), 1, "{trust:?}");
        assert!(trust[0].removed && trust[0].what.starts_with("1 trust entries"));
        assert_eq!(uninstall(&trust, false).unwrap(), 1);
        let after = std::fs::read_to_string(codex.join("config.toml")).unwrap();
        assert_eq!(after, "model = \"x\"\n\n[projects.\"/work/own\"]\ntrust_level = \"trusted\"\n");
        // Twice: nothing more.
        assert!(uninstall_plan().iter().all(|m| m.kind != MadeKind::CodexTrust));
    }

    #[test]
    fn a_forgotten_secret_leaves_the_words_ledger_and_the_server() {
        let _scratch = Scratch::new();
        // The words ledger keeps text in a v2 home only.
        struct V2;
        impl Drop for V2 {
            fn drop(&mut self) {
                // SAFETY: the core tests run one at a time (`--test-threads=1`).
                unsafe { std::env::remove_var(crate::store::ENGINE_VARIABLE) };
                crate::words::reset();
            }
        }
        // SAFETY: as above.
        unsafe { std::env::set_var(crate::store::ENGINE_VARIABLE, "v2") };
        crate::words::reset();
        let _v2 = V2;
        let memory = Memory::open();
        let said = "Remember that the deploy token is tok_live_8f3a2c91";
        let id = crate::words::record(crate::words::Origin::Composer(crate::words::ComposerKind::ManagerChat), said, None, None);
        let other = crate::words::record(crate::words::Origin::Composer(crate::words::ComposerKind::ManagerChat), "Remember that releases go out on Tuesdays", None, None);
        let record = crate::words::find(&id).unwrap();
        let Remembered::Entry { id: entry, .. } = memory.remember(&record, "the deploy token is tok_live_8f3a2c91", "the deploy token is tok_live_8f3a2c91", &MemFile::Profile, None).unwrap() else { panic!() };
        use crate::store::Store as _;
        assert!(crate::memstore::sync::ServerForget::load().unwrap().is_empty());
        memory.forget(&[entry.as_str()], true).unwrap();
        assert!(crate::words::find(&id).is_none(), "the words it came from go");
        assert!(crate::words::find(&other).is_some(), "other words stay");
        // The server is asked to restart its history and drop the record it took, by id.
        use crate::memstore::sync::{Forgotten, ServerForget};
        let asked = ServerForget::load().unwrap();
        assert!(asked.restart && asked.drop.is_empty());
        assert_eq!(asked.words, [id.as_str().to_string()]);
        // A server that says nothing of it (an older glasscoded) is asked again.
        crate::store::update::<ServerForget, _>(|r| r.done(&asked, &Forgotten::default())).unwrap();
        assert!(ServerForget::load().unwrap().restart && !ServerForget::load().unwrap().words.is_empty());
        // Once it says it did, nothing is asked.
        crate::store::update::<ServerForget, _>(|r| r.done(&asked, &Forgotten { restart: true, words: true })).unwrap();
        assert!(ServerForget::load().unwrap().is_empty());
        // A person forgotten: their records, by id, and a restart there too.
        let named = crate::words::record(crate::words::Origin::Composer(crate::words::ComposerKind::ManagerChat), "Ask Priya Sharma about the rollout", None, None);
        crate::words::flush();
        forget(&Forget::Person("Priya Sharma".into())).unwrap();
        let asked = ServerForget::load().unwrap();
        assert!(asked.restart && asked.words.contains(&named.as_str().to_string()), "{asked:?}");
        crate::store::update::<ServerForget, _>(|r| r.done(&asked, &Forgotten { restart: true, words: true })).unwrap();
        // A forgotten project's folder goes from the server too.
        let app = key("app-3f9a1c");
        remember(&memory, &MemFile::Decisions(app.clone()), "the app deploys from main");
        forget(&Forget::Project(app.clone())).unwrap();
        let asked = crate::memstore::sync::ServerForget::load().unwrap();
        assert!(asked.restart && asked.drop == [format!("projects/{}", app.as_str())]);
    }

    #[test]
    fn the_server_forgets_what_the_mac_asks() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        remember(&memory, &MemFile::Profile, "the staging password is hunter22");
        let before = scratch.git(&memory.dir().to_path_buf(), &["rev-list", "--count", "HEAD"]);
        assert!(before.trim().parse::<u32>().unwrap() > 1);
        let done = crate::memstore::sync::forget_here(&memory, &crate::memstore::sync::ServerForget { restart: true, ..Default::default() }).unwrap();
        assert!(done.restart && !done.words);
        assert_eq!(scratch.git(&memory.dir().to_path_buf(), &["rev-list", "--count", "HEAD"]).trim(), "1");
        // The server's own restart asks no one.
        use crate::store::Store as _;
        assert!(crate::memstore::sync::ServerForget::load().unwrap().is_empty());
    }

    #[test]
    fn the_server_drops_the_words_the_mac_forgot_by_id() {
        let _scratch = Scratch::new();
        struct V2;
        impl Drop for V2 {
            fn drop(&mut self) {
                // SAFETY: the core tests run one at a time (`--test-threads=1`).
                unsafe { std::env::remove_var(crate::store::ENGINE_VARIABLE) };
                crate::words::reset();
            }
        }
        // SAFETY: as above.
        unsafe { std::env::set_var(crate::store::ENGINE_VARIABLE, "v2") };
        crate::words::reset();
        let _v2 = V2;
        // The Mac's records, as a server keeps them in the Mac's folder.
        let gone = crate::words::record(crate::words::Origin::Composer(crate::words::ComposerKind::ManagerChat), "Ask Priya Sharma about the rollout", None, None);
        let kept = crate::words::record(crate::words::Origin::Composer(crate::words::ComposerKind::ManagerChat), "Releases go out on Tuesdays", None, None);
        crate::words::flush();
        let mac = crate::paths::MachineId::parse("fedcba9876543210").unwrap();
        let records: Vec<crate::words::WordsRecord> = [&gone, &kept].iter().map(|id| crate::words::find(id).unwrap()).collect();
        crate::words::take_in(&mac, &records).unwrap();
        let request = crate::memstore::sync::ServerForget { words: vec![gone.as_str().to_string()], ..Default::default() };
        let done = crate::memstore::sync::forget_here(&Memory::open(), &request).unwrap();
        assert!(done.words && !done.restart);
        assert!(crate::words::find(&gone).is_none(), "in no machine's folder");
        assert!(crate::words::find(&kept).is_some());
    }
}
