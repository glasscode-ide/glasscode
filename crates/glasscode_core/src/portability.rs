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
            out.words_records = rewrite_lines(&paths::root().join("words"), |line| (!contains_folded(line, name)).then(|| line.to_string()))?;
            out.journal_lines = rewrite_lines(&paths::journal_dir(), |line| Some(replace_folded(line, name, FORGOTTEN)))?;
            out.effect_lines = rewrite_lines(&paths::effects_dir(), |line| Some(replace_folded(line, name, FORGOTTEN)))?;
        }
        Forget::Project(key) => {
            out.memory = forget_projects(&memory, std::slice::from_ref(key))?;
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
            let org = memory.forget_files(move |file| matches!(file, MemFile::People(e) | MemFile::Glossary(e) if *e == employer))?;
            out.memory = forget_projects(&memory, &keys)? || org.history_restarted;
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
}

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
            out.push(Made { path, what: "a worktree".into(), blocks, removed: true });
        }
    }
    out.push(Made { path: root.clone(), what: "the Glasscode folder: memory, the authority store, the journal, accounts' login folders, settings, caches".into(), blocks: None, removed: true });
    let codex = paths::home().join(".codex").join("config.toml");
    if std::fs::read_to_string(&codex).is_ok_and(|text| text.contains("trust_level = \"trusted\"")) {
        out.push(Made { path: codex, what: "trust entries Glasscode added for its worktrees (remove the [projects.…] tables of Glasscode's worktrees by hand)".into(), blocks: None, removed: false });
    }
    for service in ["Glasscode Linear", "glasscode-manager-voice"] {
        out.push(Made { path: PathBuf::from(format!("Keychain: {service}")), what: "a key kept in the system's keychain (remove it in Keychain Access)".into(), blocks: None, removed: false });
    }
    out.push(Made { path: PathBuf::from("Time Machine"), what: "backups made while Glasscode was here keep copies of the Glasscode folder until they expire".into(), blocks: None, removed: false });
    out
}

/// Remove what [`uninstall_plan`] says it removes. Refused while a worktree has work that is
/// nowhere else, unless `force`. The person confirms in the app first.
pub fn uninstall(plan: &[Made], force: bool) -> anyhow::Result<usize> {
    if !force && let Some(blocked) = plan.iter().find(|m| m.blocks.is_some()) {
        bail!("{} has work that is nowhere else ({}): push or remove it first", blocked.path.display(), blocked.blocks.as_deref().unwrap_or(""));
    }
    let root = paths::root();
    let mut removed = 0;
    for made in plan.iter().filter(|m| m.removed) {
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
        let bad = vec![Made { path: outside.path().to_path_buf(), what: "x".into(), blocks: None, removed: true }];
        assert_eq!(uninstall(&bad, false).unwrap(), 0);
        assert!(outside.path().exists());
        assert_eq!(uninstall(&plan, false).unwrap(), 1);
        assert!(!paths::root().exists());
    }
}
