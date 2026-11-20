//! What the app shows of memory (the Manager page's Memory, v2): what waits for the person (the
//! inbox's proposals and the edits made by hand that are held back), and what memory keeps, file by
//! file, with each entry's standing ("Confirmed", "Learned", "Learned (v1)", "Mentioned once").
//! Read from Glasscode's last commit, as every reader of memory (D20). It reads the disk and runs
//! git: never on the main thread.

use std::collections::BTreeMap;

use super::{Entry, MemFile, Memory, MemoryError, Source, Status};
use crate::paths::ProjectKey;

/// A proposal in the inbox, as the person decides on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub id: String,
    pub text: String,
    /// Where it goes once accepted, in words ("app · PROJECT.md").
    pub to: String,
    pub section: Option<String>,
    /// Where it came from, in words ("from your words", "moved in", "learned").
    pub from: String,
    /// Why it waits, when its writer said (a rewrite of a confirmed entry, a fact to tick).
    pub why: Option<String>,
}

/// An edit made by hand in the memory folder, held back until the person accepts or discards it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub path: String,
    /// "added", "changed" or "deleted".
    pub change: &'static str,
}

/// One entry, as the person reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub text: String,
    pub status: &'static str,
    pub from: String,
}

/// One file of memory, with what it keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileView {
    pub file: MemFile,
    pub label: String,
    pub rows: Vec<Row>,
    /// Entries let go (kept for their history, not shown).
    pub let_go: usize,
}

/// Everything the Memory page shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overview {
    pub proposals: Vec<Proposal>,
    pub held: Vec<Held>,
    pub files: Vec<FileView>,
    /// The commit it was read from.
    pub commit: Option<String>,
}

impl Overview {
    /// How many entries it keeps (let go not counted).
    pub fn entries(&self) -> usize {
        self.files.iter().map(|f| f.rows.len()).sum()
    }

    /// How many things wait for the person.
    pub fn waiting(&self) -> usize {
        self.proposals.len() + self.held.len()
    }
}

/// An entry's standing, in the words the page uses. An entry of v1's manager, migrated (M20.3),
/// is "Learned (v1)" until the person confirms it.
pub fn status_label(entry: &Entry) -> &'static str {
    match (entry.header.status, &entry.header.source) {
        (Status::Confirmed, _) => "Confirmed",
        (Status::Learned, Source::V1) => "Learned (v1)",
        (Status::Learned, _) => "Learned",
        (Status::Mentioned, _) => "Mentioned once",
        (Status::LetGo, _) => "Let go",
    }
}

/// Where an entry or a proposal came from, in words.
pub fn from_label(source: &Source) -> String {
    match source {
        Source::Words(_) => "from your words".into(),
        Source::Session(_) => "from a session".into(),
        Source::Import(_) => "moved in".into(),
        Source::V1 => "from v1's manager".into(),
        Source::PersonEdit => "your edit".into(),
    }
}

/// A file of memory in words: "You", "acme · people", "app · PROJECT.md", "app · know-how: ci".
/// `names`: each project's name by its key (a key with no name is shown as it is).
pub fn file_label(file: &MemFile, names: &BTreeMap<ProjectKey, String>) -> String {
    let name = |key: &ProjectKey| names.get(key).cloned().unwrap_or_else(|| key.as_str().to_string());
    match file {
        MemFile::Profile => "You".into(),
        MemFile::People(employer) => format!("{} · people", employer.as_str()),
        MemFile::Glossary(employer) => format!("{} · glossary", employer.as_str()),
        MemFile::Project(key) => format!("{} · PROJECT.md", name(key)),
        MemFile::Policy(key) => format!("{} · POLICY.md", name(key)),
        MemFile::Decisions(key) => format!("{} · decisions", name(key)),
        MemFile::Knowhow(key, topic) => format!("{} · know-how: {topic}", name(key)),
        MemFile::Work(key, work) => format!("{} · work: {work}", name(key)),
        MemFile::Archive(key, month, work) => format!("{} · archive {month}: {work}", name(key)),
        MemFile::Proposals => "Waiting for you".into(),
        MemFile::Rejected => "Not kept".into(),
        MemFile::Index => "Index".into(),
    }
}

/// The files the page lists: what memory knows, not its work files, archives or own bookkeeping.
fn shown(file: &MemFile) -> bool {
    matches!(file, MemFile::Profile | MemFile::People(_) | MemFile::Glossary(_) | MemFile::Project(_) | MemFile::Policy(_) | MemFile::Decisions(_) | MemFile::Knowhow(..))
}

/// What the page shows, read from Glasscode's last commit, with what waits (the inbox and the held
/// edits).
pub fn overview(memory: &Memory, names: &BTreeMap<ProjectKey, String>) -> Result<Overview, MemoryError> {
    let snap = memory.snapshot()?;
    let mut out = Overview { commit: snap.commit().map(String::from), ..Overview::default() };
    let files = snap.files()?;
    if files.contains(&MemFile::Proposals) {
        let inbox = snap.doc(&MemFile::Proposals)?;
        for entry in inbox.entries() {
            let to = entry.header.to.as_deref().and_then(MemFile::parse).map(|file| file_label(&file, names)).unwrap_or_else(|| entry.header.to.clone().unwrap_or_default());
            let more = |key: &str| entry.header.more.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
            out.proposals.push(Proposal { id: entry.header.id.clone(), text: entry.text.clone(), to, section: more("section"), from: from_label(&entry.header.source), why: more("why") });
        }
    }
    for edit in memory.pending()?.edits {
        let change = match edit.change {
            super::Change::Added => "added",
            super::Change::Modified => "changed",
            super::Change::Deleted => "deleted",
        };
        out.held.push(Held { path: edit.path, change });
    }
    let mut views: Vec<FileView> = Vec::new();
    for file in files.into_iter().filter(shown) {
        let doc = snap.doc(&file)?;
        let mut view = FileView { label: file_label(&file, names), file, rows: Vec::new(), let_go: 0 };
        for entry in doc.entries() {
            if entry.header.status == Status::LetGo {
                view.let_go += 1;
                continue;
            }
            view.rows.push(Row { id: entry.header.id.clone(), text: entry.text.clone(), status: status_label(entry), from: from_label(&entry.header.source) });
        }
        if !view.rows.is_empty() || view.let_go > 0 {
            views.push(view);
        }
    }
    // The person first, then each project's files together, by name.
    views.sort_by_key(|v| (v.file != MemFile::Profile, v.label.to_lowercase()));
    out.files = views;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memstore::tests_support::{Scratch, chat, key};
    use crate::memstore::{Who, entry::Header};

    #[test]
    fn the_page_shows_what_waits_and_what_is_kept_with_its_standing() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let app = key("app-3f9a1c");
        let names = BTreeMap::from([(app.clone(), "app".to_string())]);
        memory.remember(&chat("Remember that releases go out on Tuesdays."), "releases go out on Tuesdays", "Releases go out on Tuesdays.", &MemFile::Project(app.clone()), None).unwrap();
        memory.learn(&Who::Session("s-1".into()), &MemFile::Knowhow(app.clone(), "ci".into()), None, "Run make test before pushing.", None).unwrap();
        let (proposed, _) = memory.propose(&Who::Manager, &MemFile::Decisions(app.clone()), None, "Deploys go through the release train.", Source::Session("s-2".into())).unwrap();
        // A note of v1's manager, migrated.
        let mut v1 = Entry { text: "The person prefers short answers.".into(), header: Header::new("v1note".into(), Status::Learned, crate::memstore::Kind::Fact, Source::V1, crate::identity::Hlc::zero(crate::paths::MachineId::parse("0123456789abcdef").unwrap())) };
        v1.header.status = Status::Learned;
        assert_eq!(status_label(&v1), "Learned (v1)");
        // An edit made by hand waits.
        std::fs::create_dir_all(memory.dir().join("you")).unwrap();
        std::fs::write(memory.dir().join("you/profile.md"), "# Profile\n\n- I like tea.\n").unwrap();
        let page = overview(&memory, &names).unwrap();
        assert_eq!(page.proposals.len(), 1);
        assert_eq!(page.proposals[0].id, proposed);
        assert_eq!(page.proposals[0].to, "app · decisions");
        assert_eq!(page.proposals[0].from, "from a session");
        assert_eq!(page.held, [Held { path: "you/profile.md".into(), change: "added" }]);
        let project = page.files.iter().find(|f| f.label == "app · PROJECT.md").unwrap();
        assert_eq!((project.rows[0].status, project.rows[0].from.as_str()), ("Confirmed", "from your words"));
        let knowhow = page.files.iter().find(|f| f.label == "app · know-how: ci").unwrap();
        assert_eq!(knowhow.rows[0].status, "Learned");
        assert_eq!(page.entries(), 2);
        assert_eq!(page.waiting(), 2);
        drop(scratch);
    }
}
