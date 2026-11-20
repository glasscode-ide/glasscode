//! Bringing older memories into memory (slice M20.3): the v1 manager's notes (step 1), the voice
//! copilot's facts (step 2), and where Codex keeps its memory (step 4). Running it again adds
//! nothing: an entry or a proposal with the same text is already there.

use std::path::{Path, PathBuf};

use super::{Entry, Header, Kind, MemFile, Memory, MemoryError, Result, Source, Status, Who, duplicate};

/// The section of `you/profile.md` the v1 manager's notes go to.
pub const FROM_V1: &str = "From the v1 manager";
/// The section the voice copilot's facts are proposed for.
pub const FROM_VOICE: &str = "From voice";

/// What a migration brought in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Migrated {
    /// v1 notes kept as learned entries.
    pub learned: usize,
    /// v1 notes the profile had no room for, and the voice copilot's facts: proposals.
    pub proposed: usize,
    /// Texts the write-time scan refused (an instruction, a secret): left out.
    pub refused: usize,
}

/// Step 1: each of the v1 manager's notes (not forgotten ones) becomes a `learned` entry with
/// `source=v1` in `you/profile.md`; one tick confirms it, as v1 cannot prove where a note came
/// from (mem-17). A note the profile's cap has no room for waits as a proposal instead.
pub fn v1_notes(memory: &Memory, notes: &[crate::manager::Note]) -> Result<Migrated> {
    let mut migrated = Migrated::default();
    let file = MemFile::Profile;
    for note in notes.iter().filter(|n| !n.deleted) {
        let text = super::entry::clean_text(&note.text);
        if text.is_empty() {
            continue;
        }
        let written = memory.write(&Who::Manager, "Bring in a note of the v1 manager", std::slice::from_ref(&text), |tx| {
            tx.scan_with(None, &text, Some(FROM_V1))?;
            if duplicate(tx.doc(&file)?, &text).is_some() {
                return Ok(false);
            }
            let stamp = tx.stamp()?;
            let id = tx.new_id()?;
            tx.doc(&file)?.add(Some(FROM_V1), Entry { text: text.clone(), header: Header::new(id, Status::Learned, Kind::Fact, Source::V1, stamp) });
            Ok(true)
        });
        match written {
            Ok((true, _)) => migrated.learned += 1,
            Ok((false, _)) => {}
            Err(MemoryError::OverCap(_)) => {
                memory.propose(&Who::Manager, &file, Some(FROM_V1), &text, Source::V1)?;
                migrated.proposed += 1;
            }
            Err(MemoryError::Refused(_)) => migrated.refused += 1,
            Err(err) => return Err(err),
        }
    }
    Ok(migrated)
}

/// The voice copilot's file (`glasscode-memory.json` in the Glasscode folder).
pub fn voice_file() -> PathBuf {
    crate::paths::root().join("glasscode-memory.json")
}

/// Step 2: each fact of the voice copilot's file becomes a proposal for `you/profile.md`. The
/// file itself is not changed (the voice plan decides its future). No file: nothing to do.
pub fn voice_facts(memory: &Memory, path: &Path) -> Result<Migrated> {
    let mut migrated = Migrated::default();
    let Ok(bytes) = std::fs::read(path) else { return Ok(migrated) };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Ok(migrated) };
    let facts = value.get("facts").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
    let before = memory.snapshot()?.doc(&MemFile::Proposals)?.entries().count();
    for fact in facts {
        let Some(text) = fact.get("text").and_then(serde_json::Value::as_str).map(super::entry::clean_text).filter(|t| !t.is_empty()) else { continue };
        match memory.propose(&Who::Manager, &MemFile::Profile, Some(FROM_VOICE), &text, Source::Import("glasscode-memory.json".into())) {
            Ok(_) => {}
            Err(MemoryError::Refused(_)) => migrated.refused += 1,
            Err(err) => return Err(err),
        }
    }
    migrated.proposed = memory.snapshot()?.doc(&MemFile::Proposals)?.entries().count().saturating_sub(before);
    Ok(migrated)
}

/// Steps 1 and 2 together, from where v1 and the voice copilot keep them.
pub fn run(memory: &Memory) -> Result<Migrated> {
    let notes = crate::manager::migrate_v2::read_v1().notes;
    let a = v1_notes(memory, &notes)?;
    let b = voice_facts(memory, &voice_file())?;
    Ok(Migrated { learned: a.learned, proposed: a.proposed + b.proposed, refused: a.refused + b.refused })
}

/// Step 4: the folder Codex keeps its memory in, under its home (an account's `CODEX_HOME`).
/// Its name differs between Codex versions, so the known names are probed (mem-m6).
pub fn codex_memory_dir(codex_home: &Path) -> Option<PathBuf> {
    const NAMES: [&str; 3] = ["memories", "memory", "MEMORY"];
    NAMES.iter().map(|name| codex_home.join(name)).find(|dir| dir.is_dir())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    /// A Glasscode folder of the test's own, and git that reads none of the person's settings.
    struct Scratch {
        dir: tempfile::TempDir,
        before: Vec<(&'static str, Option<OsString>)>,
    }

    impl Scratch {
        fn new() -> Scratch {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("glasscode");
            std::fs::create_dir(&home).unwrap();
            let config = dir.path().join("gitconfig");
            std::fs::write(&config, "").unwrap();
            let set: [(&'static str, OsString); 4] = [("GLASSCODE_HOME", home.into()), ("GIT_CONFIG_GLOBAL", config.into()), ("GIT_CONFIG_NOSYSTEM", "1".into()), ("GIT_CEILING_DIRECTORIES", dir.path().into())];
            let before = set.iter().map(|(name, _)| (*name, std::env::var_os(name))).collect();
            for (name, value) in &set {
                // SAFETY: the core tests run one at a time (`--test-threads=1`).
                unsafe { std::env::set_var(name, value) };
            }
            Scratch { dir, before }
        }
    }

    impl Drop for Scratch {
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

    fn note(id: &str, text: &str, deleted: bool) -> crate::manager::Note {
        serde_json::from_value(serde_json::json!({ "id": id, "text": text, "tags": [], "source": "you", "created": 1, "updated": 1, "deleted": deleted })).unwrap()
    }

    #[test]
    fn v1_notes_become_learned() {
        let _scratch = Scratch::new();
        let memory = Memory::open();
        let notes = [note("n1", "Sam reviews on Fridays.", false), note("n2", "Gone already.", true)];
        let migrated = v1_notes(&memory, &notes).unwrap();
        assert_eq!(migrated.learned, 1);
        let doc = memory.snapshot().unwrap().doc(&MemFile::Profile).unwrap();
        let entry = doc.entries().find(|e| e.text.contains("Sam reviews")).expect("kept");
        assert_eq!((entry.header.status, &entry.header.source), (Status::Learned, &Source::V1));
        assert!(doc.entries().all(|e| !e.text.contains("Gone already")), "a forgotten note stays forgotten");
    }

    #[test]
    fn migration_is_idempotent() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let notes = [note("n1", "Sam reviews on Fridays.", false)];
        let voice = scratch.dir.path().join("glasscode-memory.json");
        std::fs::write(&voice, r#"{"updated": 1, "sessions": [], "facts": [{"text": "Prefers short answers."}], "seen": {}}"#).unwrap();
        let first = (v1_notes(&memory, &notes).unwrap(), voice_facts(&memory, &voice).unwrap());
        assert_eq!((first.0.learned, first.1.proposed), (1, 1));
        let again = (v1_notes(&memory, &notes).unwrap(), voice_facts(&memory, &voice).unwrap());
        assert_eq!((again.0.learned, again.1.proposed), (0, 0), "nothing twice");
        assert!(std::fs::read_to_string(&voice).unwrap().contains("Prefers short answers"), "the voice file is not changed");
        let proposals = memory.snapshot().unwrap().doc(&MemFile::Proposals).unwrap();
        assert_eq!(proposals.entries().filter(|e| e.text.contains("Prefers short answers")).count(), 1);
    }

    /// M20.3's demonstration: after the migration, the Manager page lists a v1 note as
    /// "Learned (v1)", and a voice fact as waiting for the person's tick.
    #[test]
    fn migrated_notes_list_as_learned_from_v1() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let voice = scratch.dir.path().join("glasscode-memory.json");
        std::fs::write(&voice, r#"{"updated": 1, "sessions": [], "facts": [{"text": "Prefers short answers."}], "seen": {}}"#).unwrap();
        v1_notes(&memory, &[note("n1", "Sam reviews on Fridays.", false)]).unwrap();
        voice_facts(&memory, &voice).unwrap();
        let listed = super::super::listing(&memory).unwrap();
        let line = |text: &str| listed.iter().find(|l| l.text.contains(text)).map(|l| (l.standing.as_str(), l.file.as_str()));
        assert_eq!(line("Sam reviews"), Some(("Learned (v1)", "you/profile.md")));
        assert_eq!(line("Prefers short answers"), Some(("Waits for your tick", "you/profile.md")));
        assert_eq!(listed[0].standing, "Waits for your tick", "the inbox first");
    }

    #[test]
    fn codex_memory_is_found_by_its_folder_name() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(codex_memory_dir(dir.path()), None);
        std::fs::create_dir(dir.path().join("memory")).unwrap();
        assert_eq!(codex_memory_dir(dir.path()), Some(dir.path().join("memory")));
        std::fs::create_dir(dir.path().join("memories")).unwrap();
        assert_eq!(codex_memory_dir(dir.path()), Some(dir.path().join("memories")), "the newer name first");
    }
}
