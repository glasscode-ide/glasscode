//! Playbooks (M40; design D10, D15, section 8; review events-10 to events-13, exec-16): what
//! Glasscode does by itself when something happens, without a model.
//!
//! - **The model** ([`model`]) is typed tables: an event filter with a `where` table, typed
//!   conditions, an op with argument slots, an autonomy (watch, tell, ask, act) and a limit.
//! - **Two kinds** in one table. Glasscode's own ([`builtins`]) are code: their rules are state
//!   machines (`autofix` is the CI one's), and their entry holds only their settings. The person's
//!   (and the manager's proposals) are the declarative form.
//! - **Approval** (D10, events-10) is as the policy's (M21.1): the live file
//!   `authority/playbooks.json` is what the app writes, and each machine keeps its own approved
//!   hashes in `authority/approved/playbooks.json`, which is never synced. A playbook fires only
//!   when its hash now is the one this machine approved ([`model::Playbook::live`]): a file that
//!   arrives by sync, or that a program changed, is not approved. A playbook a wake or a session
//!   wrote is only proposed ([`propose`]); only the person approves it ([`approve`]).
//! - **Pause** (D15): any caller may pause a playbook; only the person releases it ([`pause`],
//!   [`release`]). The pause is not part of what was approved.
//! - **The engine** ([`engine`]) is pure, and so are the limits ([`limits`]) and the dry run
//!   ([`simulate`]).
//!
//! Both stores refuse when damaged and never fall back to an older generation: then nothing runs
//! but the built-ins, which are app behaviour (exec-16) and need neither file.

pub mod builtins;
pub mod engine;
pub mod limits;
pub mod model;
pub mod simulate;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use model::{Action, Autonomy, Cond, EventFilter, FieldType, Limit, Match, Paused, Per, Playbook, ScheduleSpec, Schema, Slot, Trigger};

use crate::paths;
use crate::store::{OnDamage, Store, StoreError};

/// `authority/playbooks.json`: the playbooks as the app wrote them (the person's, the manager's
/// proposals, and the built-ins' settings). Synced from the Mac to the server, where it is not
/// approved on arrival.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybookFile {
    pub playbooks: Vec<Playbook>,
    /// The global breaker: every playbook but the built-ins is paused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_paused: Option<Paused>,
}

impl Store for PlaybookFile {
    const NAME: &'static str = "authority/playbooks";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    // An older generation may hold a playbook since removed.
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("playbooks.json")
    }
}

/// What the person approved on this machine: each playbook's hash, by id. Never synced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApprovedPlaybooks {
    pub approved: BTreeMap<String, Approval>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Approval {
    /// 64 hex digits of [`Playbook::hash`].
    pub hash: String,
    pub approved_ms: u64,
}

impl Store for ApprovedPlaybooks {
    const NAME: &'static str = "authority/approved/playbooks";
    const VERSION: u32 = 1;
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;
    const FALLS_BACK: bool = false;

    fn path() -> PathBuf {
        paths::authority_dir().join("approved").join("playbooks.json")
    }
}

/// The playbooks as this machine reads them: each with the hash it approved, the ones that do not
/// pass [`model::validate`] switched off, and one alert for each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Book {
    pub playbooks: Vec<Playbook>,
    pub all_paused: Option<Paused>,
    /// "“Fix red CI” is off: …", one line for each playbook that did not load.
    pub alerts: Vec<String>,
}

impl Book {
    /// The playbooks that may fire now.
    pub fn live(&self) -> Vec<Playbook> {
        if self.all_paused.is_some() {
            return Vec::new();
        }
        self.playbooks.iter().filter(|p| p.live()).cloned().collect()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<[u8; 32]> {
    crate::effects::EffectKey::parse(text).map(|key| key.0)
}

/// Put the file and this machine's approvals together: each playbook gets the hash this machine
/// approved; one that does not validate is switched off, with an alert, and the rest still load.
pub fn book(file: PlaybookFile, approved: &ApprovedPlaybooks, schema: &Schema) -> Book {
    let mut alerts = Vec::new();
    let playbooks = file
        .playbooks
        .into_iter()
        .map(|mut playbook| {
            playbook.approved_hash = approved.approved.get(&playbook.id).and_then(|approval| unhex(&approval.hash));
            if let Err(problems) = model::validate(&playbook, schema) {
                alerts.push(format!("“{}” is off: {}.", playbook.name, problems.join("; ")));
                playbook.enabled = false;
                playbook.approved_hash = None;
            }
            playbook
        })
        .collect();
    Book { playbooks, all_paused: file.all_paused, alerts }
}

/// Read both stores, and check the playbooks against `schema`. A damaged store is an error:
/// nothing but the built-ins runs then. It reads the disk.
pub fn load(schema: &Schema) -> Result<Book, StoreError> {
    Ok(book(PlaybookFile::load()?, &ApprovedPlaybooks::load()?, schema))
}

/// Why a change to the playbooks was not made.
#[derive(Debug, thiserror::Error)]
pub enum BookError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("only the person may {0} a playbook")]
    NotThePerson(&'static str),
    #[error("there is no playbook {0}")]
    NotFound(String),
    #[error("{0} is the person's own; a proposal does not replace it")]
    TheirsToKeep(String),
}

/// Whether a caller is the person (only the person approves and releases).
fn person_only(person: bool, what: &'static str) -> Result<(), BookError> {
    if person { Ok(()) } else { Err(BookError::NotThePerson(what)) }
}

/// The person approves a playbook as it is now, on this machine. It writes the disk.
pub fn approve(id: &str, person: bool) -> Result<(), BookError> {
    person_only(person, "approve")?;
    let file = PlaybookFile::load()?;
    let playbook = file.playbooks.iter().find(|p| p.id == id).ok_or_else(|| BookError::NotFound(id.to_string()))?;
    let hash = hex(&playbook.hash());
    crate::store::update::<ApprovedPlaybooks, _>(|approved| {
        approved.approved.insert(id.to_string(), Approval { hash, approved_ms: crate::clock::now_ms() });
    })?;
    Ok(())
}

/// Write a playbook. The person's is written as it is (and approved with [`approve`]); anyone
/// else's is only proposed: switched off, marked with who proposed it, and with no approval. A
/// proposal never replaces the person's own playbook of the same id. It writes the disk.
pub fn propose(mut playbook: Playbook, by: Option<&str>) -> Result<(), BookError> {
    if let Some(by) = by {
        playbook.enabled = false;
        playbook.proposed_by = Some(by.to_string());
    }
    playbook.approved_hash = None;
    playbook.paused = None;
    let id = playbook.id.clone();
    let proposal = by.is_some();
    let written = crate::store::update::<PlaybookFile, _>(|file| match file.playbooks.iter_mut().find(|p| p.id == id) {
        Some(have) if proposal && have.proposed_by.is_none() => false,
        Some(have) => {
            *have = playbook;
            true
        }
        None => {
            file.playbooks.push(playbook);
            true
        }
    })?;
    if !written {
        return Err(BookError::TheirsToKeep(id));
    }
    if proposal {
        // Whatever was approved under this id no longer is.
        crate::store::update::<ApprovedPlaybooks, _>(|approved| {
            approved.approved.remove(&id);
        })?;
    }
    Ok(())
}

/// Pause a playbook (any caller, D15), or every one with `id` None (the global breaker). It
/// writes the disk.
pub fn pause(id: Option<&str>, by: &str, why: &str) -> Result<(), BookError> {
    let paused = Paused { by: by.to_string(), since_ms: crate::clock::now_ms(), why: why.to_string() };
    crate::store::update::<PlaybookFile, _>(|file| match id {
        None => file.all_paused = Some(paused),
        Some(id) => {
            if let Some(playbook) = file.playbooks.iter_mut().find(|p| p.id == id) {
                playbook.paused = Some(paused);
            }
        }
    })?;
    Ok(())
}

/// Release a pause: the person only (D15). It writes the disk.
pub fn release(id: Option<&str>, person: bool) -> Result<(), BookError> {
    person_only(person, "release")?;
    crate::store::update::<PlaybookFile, _>(|file| match id {
        None => file.all_paused = None,
        Some(id) => {
            if let Some(playbook) = file.playbooks.iter_mut().find(|p| p.id == id) {
                playbook.paused = None;
            }
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::playbooks::engine::tests::{playbook, schema};

    struct Home {
        _dir: tempfile::TempDir,
        before: Option<OsString>,
    }

    impl Home {
        fn new() -> Home {
            let dir = tempfile::tempdir().unwrap();
            let before = std::env::var_os("GLASSCODE_HOME");
            // SAFETY: the core tests run one at a time (`--test-threads=1`).
            unsafe { std::env::set_var("GLASSCODE_HOME", dir.path()) };
            Home { _dir: dir, before }
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

    fn mine() -> Playbook {
        Playbook { approved_hash: None, ..playbook("fix", Autonomy::Act) }
    }

    #[test]
    fn synced_playbook_is_not_approved() {
        let _home = Home::new();
        propose(mine(), None).unwrap();
        // Written by the person, not approved yet: it does not fire.
        assert!(load(&schema()).unwrap().live().is_empty());
        approve("fix", true).unwrap();
        assert_eq!(load(&schema()).unwrap().live().len(), 1);
        // The same file on another machine (the server's copy of the Mac's sync) has no approval
        // of its own: nothing fires there.
        let file = PlaybookFile::load().unwrap();
        assert!(book(file.clone(), &ApprovedPlaybooks::default(), &schema()).live().is_empty());
        // A program edits the file: the hash this machine approved no longer holds.
        let mut edited = file;
        edited.playbooks[0].autonomy = Autonomy::Act;
        edited.playbooks[0].limit.count = 99;
        edited.save().unwrap();
        assert!(load(&schema()).unwrap().live().is_empty());
        // A damaged file refuses (nothing runs), and never falls back to the older generation.
        std::fs::write(PlaybookFile::path(), "not json").unwrap();
        assert!(load(&schema()).is_err());
        assert!(!PlaybookFile::FALLS_BACK && !ApprovedPlaybooks::FALLS_BACK);
        assert_eq!(paths::class_of(&ApprovedPlaybooks::path()).class(), Some(paths::PathClass::Authority));
    }

    #[test]
    fn wake_cannot_create_act_playbook() {
        let _home = Home::new();
        // The wake lane writes a playbook that would act by itself: it is only proposed.
        propose(mine(), Some("the manager's wake lane")).unwrap();
        let book = load(&schema()).unwrap();
        assert_eq!(book.playbooks[0].proposed_by.as_deref(), Some("the manager's wake lane"));
        assert!(!book.playbooks[0].enabled);
        assert!(book.live().is_empty());
        // It cannot approve it either; the person can, and what they approve is what is there.
        assert!(approve("fix", false).is_err());
        approve("fix", true).unwrap();
        assert!(load(&schema()).unwrap().live().is_empty(), "approved switched off: still nothing");
        // A proposal never replaces the person's own playbook.
        propose(mine(), None).unwrap();
        approve("fix", true).unwrap();
        assert_eq!(load(&schema()).unwrap().live().len(), 1);
        assert!(propose(Playbook { name: "sneaky".into(), ..mine() }, Some("a session")).is_err());
        assert_eq!(load(&schema()).unwrap().live()[0].name, "Fix red CI");
    }

    #[test]
    fn any_caller_pauses_only_the_person_releases() {
        let _home = Home::new();
        propose(mine(), None).unwrap();
        approve("fix", true).unwrap();
        pause(Some("fix"), "a session", "it keeps answering").unwrap();
        let book = load(&schema()).unwrap();
        assert!(book.live().is_empty());
        assert!(book.playbooks[0].approved(), "a pause does not take the approval away");
        assert!(release(Some("fix"), false).is_err());
        release(Some("fix"), true).unwrap();
        assert_eq!(load(&schema()).unwrap().live().len(), 1);
        pause(None, "the global breaker", "60 firings in an hour").unwrap();
        assert!(load(&schema()).unwrap().live().is_empty());
        release(None, true).unwrap();
        assert_eq!(load(&schema()).unwrap().live().len(), 1);
    }

    #[test]
    fn an_invalid_playbook_is_off_with_one_alert_and_the_rest_load() {
        let _home = Home::new();
        propose(mine(), None).unwrap();
        let mut broken = Playbook { id: "broken".into(), name: "Broken".into(), ..mine() };
        broken.then.op = "no.such_op".into();
        propose(broken, None).unwrap();
        approve("fix", true).unwrap();
        approve("broken", true).unwrap();
        let book = load(&schema()).unwrap();
        assert_eq!(book.alerts.len(), 1);
        assert!(book.alerts[0].starts_with("“Broken” is off"), "{}", book.alerts[0]);
        assert_eq!(book.live().iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["fix"]);
    }
}
