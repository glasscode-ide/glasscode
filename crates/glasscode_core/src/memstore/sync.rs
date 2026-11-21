//! Memory entries, both ways between the Mac and the server (M57 step 3; it replaces M20.3's
//! read-only copy for a server that has "memory-sync").
//!
//! - **What travels** is a file's skeleton ([`skeleton`]): its headings, entries and tombstones,
//!   never its prose and never `INDEX.md` (each side's own write makes its index). Each side sends
//!   the files that changed since the commit the other has of it, and an entry that left a file
//!   with no tombstone (a proposal accepted, a work file moved) goes as one, so the other lets it go
//!   too.
//! - **Merging** is the format's rule ([`entry::merge`]): by entry, the later stamp winning, a
//!   tombstone beating any stamp. One commit takes a whole sync in ([`Memory::take_synced`]); a
//!   file with an edit that waits for the person is left out and comes again with the next sync.
//!   Caps do not refuse a merge (the two copies were each within them; tidying trims later).
//! - **What stays on this Mac** ([`LeftOut`]) is an allowlist the other way round: a file goes when
//!   it is not below a left-out folder, and a line of it goes when it names no left-out folder,
//!   project key or employer and is no `org=` or `to=` entry aimed at one. Nothing of a local-only
//!   project, its text, its name or its paths, is in what the server receives.
//! - **No authority from the server.** Every text the Mac takes is scanned first, as for any write
//!   (mem-1). From the server it never takes the person's own edit (`source=person-edit`), and an
//!   entry the server confirmed is only learned here unless the Mac had it confirmed already.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::entry::{self, Doc, Line, Source, Status};
use super::git::Who;
use super::layout::MemFile;
use super::{Memory, MemoryError, Result, Snapshot, git_error, writing};
use crate::paths::MachineId;

/// One file's skeleton, as it travels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncedFile {
    pub rel: String,
    pub text: String,
}

/// What of memory stays on this Mac: the folders [`crate::memory_copy::excluded_paths`] names, and
/// the lines elsewhere that point at them.
#[derive(Clone, Debug, Default)]
pub struct LeftOut {
    paths: Vec<String>,
    orgs: Vec<String>,
    needles: Vec<String>,
}

impl LeftOut {
    /// Nothing stays (the server's side).
    pub fn none() -> LeftOut {
        LeftOut::default()
    }

    /// The folders that stay (`projects/<key>`, `org/<employer>`).
    pub fn of(paths: Vec<String>) -> LeftOut {
        let orgs = paths.iter().filter_map(|path| path.strip_prefix("org/")).map(String::from).collect();
        let mut needles: Vec<String> = paths.clone();
        needles.extend(paths.iter().filter_map(|path| path.rsplit('/').next()).map(String::from));
        LeftOut { paths, orgs, needles }
    }

    pub fn needles(&self) -> &[String] {
        &self.needles
    }

    /// Whether a file of this path goes.
    pub fn keeps_file(&self, rel: &str) -> bool {
        !self.paths.iter().any(|path| rel == path || rel.starts_with(&format!("{path}/")))
    }

    fn names(&self, text: &str) -> bool {
        self.needles.iter().any(|needle| text.contains(needle.as_str()))
    }

    /// Whether a line of a file that goes goes too.
    pub fn keeps_line(&self, line: &Line) -> bool {
        match line {
            Line::Entry(entry) => {
                !entry.header.org.as_ref().is_some_and(|org| self.orgs.iter().any(|o| *o == org.to_string()))
                    && !entry.header.to.as_deref().is_some_and(|to| !self.keeps_file(to))
                    && !self.names(&entry.render())
            }
            Line::Text(text) => !self.names(text),
            // A tombstone of what pointed at what stays here stays too, id and all (ux-m2).
            Line::Tombstone { about, .. } => about.as_deref().is_none_or(|about| self.keeps_file(about) && !self.names(about)),
        }
    }
}

/// A file's headings, entries and tombstones that go (no prose). A tombstone under a heading that
/// names what stays here (the inbox's per-file headings) stays with it, though it says nothing of
/// where it pointed (one made before tombstones kept that).
pub fn skeleton(doc: &Doc, left: &LeftOut) -> Doc {
    let mut section_kept = true;
    let mut lines = Vec::new();
    for line in &doc.lines {
        match line {
            Line::Text(text) if text.trim_start().starts_with('#') => {
                section_kept = left.keeps_line(line);
                if section_kept {
                    lines.push(line.clone());
                }
            }
            Line::Text(_) => {}
            Line::Tombstone { .. } if !section_kept => {}
            _ if left.keeps_line(line) => lines.push(line.clone()),
            _ => {}
        }
    }
    Doc { lines, newline: true }
}

/// A file that takes part: memory's, and neither the generated index nor the rejected texts.
fn synced(file: &MemFile) -> bool {
    !matches!(file, MemFile::Index | MemFile::Rejected)
}

/// What to send: the skeletons of the files that changed since `since` (a commit of this memory the
/// other home has; none or unknown: every file), and the commit they are of. `machine` stamps the
/// tombstones of entries that left a file. It reads the disk and runs git.
pub fn outgoing(memory: &Memory, since: Option<&str>, left: &LeftOut, machine: &MachineId) -> Result<(Vec<SyncedFile>, Option<String>)> {
    let snap = memory.snapshot()?;
    let Some(head) = snap.commit().map(String::from) else { return Ok((Vec::new(), None)) };
    if since == Some(head.as_str()) {
        return Ok((Vec::new(), Some(head)));
    }
    let since = since.filter(|commit| snap.git.has_commit(commit));
    let rels = match since {
        Some(from) => snap.git.changed_between(from, &head).map_err(git_error)?,
        None => snap.git.files(&head).map_err(git_error)?,
    };
    let files: Vec<MemFile> = rels.iter().filter_map(|rel| MemFile::parse(rel)).filter(|file| synced(file) && left.keeps_file(&file.rel())).collect();
    let now = snap.texts(&files)?;
    let before = match since {
        Some(from) => Snapshot { git: snap.git.clone(), commit: Some(from.to_string()) }.texts(&files)?,
        None => vec![None; files.len()],
    };
    let mut out = Vec::new();
    for ((file, now), before) in files.iter().zip(now).zip(before) {
        let doc = now.as_deref().map(Doc::parse).unwrap_or_default();
        let mut sent = skeleton(&doc, left);
        // Entries that left the file with no tombstone go as one: only those that went before (an
        // entry about what stays here never did, so neither does its id).
        if let Some(before) = before.as_deref().map(Doc::parse) {
            let still = doc.ids();
            let gone: Vec<String> = before.entries().filter(|e| left.keeps_line(&Line::Entry((*e).clone()))).map(|e| e.header.id.clone()).filter(|id| !still.contains(id)).collect();
            if !gone.is_empty() {
                let stamps = crate::identity::next_stamps(machine, gone.len()).map_err(|err| MemoryError::Git(err.to_string()))?;
                sent.lines.extend(gone.into_iter().zip(stamps).map(|(id, at)| Line::Tombstone { id, at, about: None }));
            }
        }
        if now.is_some() || sent.lines.iter().any(|line| !matches!(line, Line::Text(_))) {
            out.push(SyncedFile { rel: file.rel(), text: sent.render() });
        }
    }
    Ok((out, Some(head)))
}

/// What the server is asked to forget at the next sync (`manager/server-forget.json`): after this
/// Mac forgot a secret, a person, a project or an employer, its history restarts there too, and
/// the folders of a forgotten project or employer go. It holds no forgotten text, only that and
/// which folders. A sync that the server answered clears what it sent.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ServerForget {
    pub restart: bool,
    pub drop: Vec<String>,
    /// Words records this Mac forgot (a secret's, a person's), by id: the server took them in.
    pub words: Vec<String>,
}

impl crate::store::Store for ServerForget {
    const NAME: &'static str = "manager/server-forget";
    const VERSION: u32 = 1;
    const ON_DAMAGE: crate::store::OnDamage = crate::store::OnDamage::Empty;

    fn path() -> std::path::PathBuf {
        crate::paths::root().join("manager").join("server-forget.json")
    }
}

impl ServerForget {
    /// Ask for folders to go too (`projects/<key>`, `org/<employer>`).
    pub fn drop_folders(&mut self, folders: impl IntoIterator<Item = String>) {
        for folder in folders {
            if !self.drop.contains(&folder) {
                self.drop.push(folder);
            }
        }
    }

    /// The server did what `sent` asked, as its answer says (`done`): only what it confirmed is
    /// cleared. A server that does not know the request (an older glasscoded) confirms nothing,
    /// so it is asked again at each sync. Folders go at any server that syncs entries.
    pub fn done(&mut self, sent: &ServerForget, done: &Forgotten) {
        if sent.restart && done.restart {
            self.restart = false;
        }
        self.drop.retain(|folder| !sent.drop.contains(folder));
        if done.words {
            self.words.retain(|id| !sent.words.contains(id));
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.restart && self.drop.is_empty() && self.words.is_empty()
    }
}

/// What the server says it forgot at a sync (`memory.sync`'s `forgot`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Forgotten {
    /// Its history restarted.
    pub restart: bool,
    /// It dropped the words records it was sent (how many it had is its own).
    pub words: bool,
}

/// Ask the server to drop these words records at the next sync (by id; no text).
pub fn ask_server_to_forget_words(ids: &[crate::words::WordsId]) {
    if ids.is_empty() {
        return;
    }
    let ids: Vec<String> = ids.iter().map(|id| id.as_str().to_string()).collect();
    if let Err(err) = crate::store::update::<ServerForget, _>(|request| {
        for id in ids {
            if !request.words.contains(&id) {
                request.words.push(id);
            }
        }
    }) {
        log::warn!("memory: the server will not be asked to forget the words: {err}");
    }
}

/// The server's side of a forget the Mac asks for at a sync: the folders go, the words records go
/// by id, then (after the Mac's files with their tombstones are taken) the history restarts. What
/// it did, for its answer. It reads and writes the disk.
pub fn forget_here(memory: &Memory, request: &ServerForget) -> Result<Forgotten> {
    let mut done = Forgotten::default();
    if !request.drop.is_empty() {
        memory.drop_folders(&request.drop)?;
    }
    if !request.words.is_empty() {
        crate::words::forget(|record| request.words.iter().any(|id| id == record.id.as_str())).map_err(|err| MemoryError::Git(format!("the words ledger: {err}")))?;
        done.words = true;
    }
    if request.restart {
        memory.forget_history()?;
        done.restart = true;
    }
    Ok(done)
}

/// What [`Memory::take_synced`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Taken {
    /// Files changed (in one commit).
    pub changed: usize,
    /// Files with an edit that waits for the person: left as they are, to come again.
    pub skipped: Vec<String>,
    /// Files and entries refused (not memory's, left out, a scan finding, no authority).
    pub refused: usize,
}

/// The other home's skeleton as this one takes it: its texts scanned, and from the server no
/// person's edit and no confirmation this Mac did not have.
fn admit(ours: &Doc, theirs: Doc, from_server: bool, taken: &mut Taken) -> Doc {
    let mut out = theirs;
    out.lines.retain_mut(|line| {
        let Line::Entry(entry) = line else { return true };
        let mine = ours.find(&entry.header.id);
        if mine == Some(&*entry) {
            return true;
        }
        if !super::scan::scan(&entry.text, &[]).is_empty() {
            taken.refused += 1;
            return false;
        }
        if from_server {
            if entry.header.source == Source::PersonEdit {
                taken.refused += 1;
                return false;
            }
            if entry.header.status == Status::Confirmed && !mine.is_some_and(|m| m.header.status == Status::Confirmed && m.text == entry.text) {
                entry.header.status = Status::Learned;
            }
        }
        true
    });
    out
}

impl Memory {
    /// Remove the files below `folders` (a project that stays on the Mac now: the Mac asks the
    /// server to let its folder go, M57), in one commit. Only memory's files are touched. Gives how
    /// many went. It reads and writes the disk and runs git.
    pub fn drop_folders(&self, folders: &[String]) -> Result<usize> {
        let folders: Vec<&String> = folders.iter().filter(|f| (f.starts_with("projects/") || f.starts_with("org/")) && !f.contains("..") && f.matches('/').count() == 1).collect();
        if folders.is_empty() {
            return Ok(0);
        }
        let _writing = writing()?;
        let head = self.ready()?;
        let snap = Snapshot { git: self.git.clone(), commit: Some(head.clone()) };
        let edits: Vec<(MemFile, Option<String>)> = snap.files()?.into_iter().filter(|file| synced(file) && folders.iter().any(|f| file.rel().starts_with(&format!("{f}/")))).map(|file| (file, None)).collect();
        if edits.is_empty() {
            return Ok(0);
        }
        let count = edits.len();
        self.commit(&head, &edits, &Who::Manager, "Stays on the Mac now", &[])?;
        Ok(count)
    }

    /// Take the other home's files in (`from_server`: the Mac takes the server's), merged by entry in
    /// one commit. A file that is not memory's, or stays on this Mac (`left`), is refused; a file with
    /// a pending edit is left out (in [`Taken::skipped`]). It reads and writes the disk and runs git.
    pub fn take_synced(&self, files: &[SyncedFile], from_server: bool, left: &LeftOut) -> Result<Taken> {
        let mut taken = Taken::default();
        if files.is_empty() {
            return Ok(taken);
        }
        let _writing = writing()?;
        let head = self.ready()?;
        let snap = Snapshot { git: self.git.clone(), commit: Some(head.clone()) };
        let mut edits: Vec<(MemFile, Option<String>)> = Vec::new();
        let mut seen = BTreeSet::new();
        for file in files {
            let Some(target) = MemFile::parse(&file.rel).filter(|f| synced(f) && left.keeps_file(&f.rel()) && f.rel() == file.rel) else {
                taken.refused += 1;
                continue;
            };
            if !seen.insert(target.rel()) {
                continue;
            }
            let ours = snap.doc(&target)?;
            let theirs = admit(&ours, skeleton(&Doc::parse(&file.text), left), from_server, &mut taken);
            let merged = entry::merge(&ours, &theirs);
            if merged != ours {
                edits.push((target, Some(merged.render())));
            }
        }
        let subject = if from_server { "From the server" } else { "From the Mac" };
        loop {
            if edits.is_empty() {
                return Ok(taken);
            }
            match self.commit(&head, &edits, &Who::Manager, subject, &[]) {
                Ok(_) => {
                    taken.changed = edits.len();
                    return Ok(taken);
                }
                Err(MemoryError::Pending(rel)) => {
                    edits.retain(|(file, _)| file.rel() != rel);
                    taken.skipped.push(rel);
                }
                Err(err) => return Err(err),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::memstore::Remembered;
    use crate::memstore::layout::MemFile;
    use crate::memstore::tests_support::{Scratch, chat, key, private_memory};

    /// Two homes in one test: GLASSCODE_HOME switches between them (the scratch puts it back).
    fn at_home(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        // SAFETY: the core tests run one at a time (`--test-threads=1`).
        unsafe { std::env::set_var("GLASSCODE_HOME", dir) };
        crate::words::reset();
    }

    fn remembered(memory: &Memory, text: &str, file: &MemFile) -> String {
        match memory.remember(&chat(&format!("Remember: {text}")), text, text, file, None).unwrap() {
            Remembered::Entry { id, .. } | Remembered::Proposed { id, .. } => id,
        }
    }

    /// M57 with "stays on this Mac": nothing of a local-only project is in what the Mac sends, nor
    /// in any object of the server's memory after it took it in, nor in its index.
    #[test]
    fn local_only_never_in_what_the_server_receives() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let private = private_memory(&memory);
        let machine = MachineId::this().unwrap();
        let (files, head) = outgoing(&memory, None, &LeftOut::of(private.excluded.clone()), &machine).unwrap();
        assert!(head.is_some());
        let payload = serde_json::to_string(&files).unwrap();
        for word in &private.secret_words {
            assert!(!payload.contains(word.as_str()), "{word} is in what goes");
        }
        assert!(!files.iter().any(|f| f.rel == "INDEX.md"), "the index stays");

        at_home(&scratch.at("server"));
        let server = Memory::open();
        let taken = server.take_synced(&files, false, &LeftOut::none()).unwrap();
        assert!(taken.changed > 0 && taken.skipped.is_empty());
        let all = scratch.git(server.dir(), &["cat-file", "--batch-all-objects", "--batch"]);
        let index = std::fs::read_to_string(server.dir().join("INDEX.md")).unwrap();
        for word in &private.secret_words {
            assert!(!all.contains(word.as_str()) && !index.contains(word.as_str()), "{word} reached the server");
        }
        for word in &private.open_words {
            assert!(all.contains(word.as_str()), "{word} goes");
        }

        // Later syncs from a cursor: the proposal for the local-only project is accepted (it
        // leaves the inbox), and no tombstone of it goes, as it never did.
        at_home(&scratch.at("glasscode"));
        let memory = Memory::open();
        memory.accept_proposal(&private.proposal).unwrap();
        let (later, _) = outgoing(&memory, head.as_deref(), &LeftOut::of(private.excluded.clone()), &machine).unwrap();
        let payload = serde_json::to_string(&later).unwrap();
        assert!(!payload.contains(&private.proposal), "the id of what stays here");
        for word in &private.secret_words {
            assert!(!payload.contains(word.as_str()), "{word} is in what goes later");
        }

        // A project that stays here now: the server lets its folder go.
        at_home(&scratch.at("server"));
        let server = Memory::open();
        assert!(server.snapshot().unwrap().files().unwrap().iter().any(|f| f.rel().starts_with("projects/open-222222/")));
        assert!(server.drop_folders(&["projects/open-222222".into(), "../etc".into()]).unwrap() > 0);
        assert!(!server.snapshot().unwrap().files().unwrap().iter().any(|f| f.rel().starts_with("projects/open-222222/")));
    }

    /// mem-13 with M57: a secret forgotten on the Mac leaves the server too, and does not come back
    /// from it: every copy the sweep took (a proposal that held it, too) goes as a tombstone, as the
    /// rebuilt history has no older commit to tell what left.
    #[test]
    fn a_forgotten_secret_does_not_come_back_from_the_server() {
        let scratch = Scratch::new();
        let (mac_home, server_home) = (scratch.at("mac"), scratch.at("server"));
        let app = key("app-3f9a1c");
        let secret = "the staging password is orchid lamp seven";
        at_home(&mac_home);
        let mac = Memory::open();
        let machine = MachineId::this().unwrap();
        let id = remembered(&mac, secret, &MemFile::Profile);
        mac.propose(&Who::Session("s1".into()), &MemFile::Project(app.clone()), None, &format!("Note that {secret}."), Source::Session("s1".into())).unwrap();
        remembered(&mac, "Builds with cargo.", &MemFile::Project(app.clone()));
        let (to_server, sent_at) = outgoing(&mac, None, &LeftOut::none(), &machine).unwrap();
        at_home(&server_home);
        let server = Memory::open();
        server.take_synced(&to_server, false, &LeftOut::none()).unwrap();

        at_home(&mac_home);
        let mac = Memory::open();
        mac.forget(&[id.as_str()], true).unwrap();
        let (to_server, _) = outgoing(&mac, sent_at.as_deref(), &LeftOut::none(), &machine).unwrap();
        at_home(&server_home);
        let server = Memory::open();
        server.take_synced(&to_server, false, &LeftOut::none()).unwrap();
        forget_here(&server, &ServerForget { restart: true, ..ServerForget::default() }).unwrap();
        let objects = scratch.git(server.dir(), &["cat-file", "--batch-all-objects", "--batch"]);
        assert!(!objects.contains("orchid lamp"), "the server still holds the secret");
        let server_machine = MachineId::this().unwrap();
        let (to_mac, _) = outgoing(&server, None, &LeftOut::none(), &server_machine).unwrap();

        at_home(&mac_home);
        let mac = Memory::open();
        mac.take_synced(&to_mac, true, &LeftOut::none()).unwrap();
        let objects = scratch.git(mac.dir(), &["cat-file", "--batch-all-objects", "--batch"]);
        assert!(!objects.contains("orchid lamp"), "the secret came back from the server");
        assert!(mac.snapshot().unwrap().doc(&MemFile::Project(app)).unwrap().entries().any(|e| e.text == "Builds with cargo."));
    }

    /// ux-m2 with forgetting: nothing about what stays on this Mac reaches the server, not even the
    /// id of a forgotten entry: a proposal for a local-only project, forgotten alone or with its
    /// project's secret, and a profile entry kept to an employer whose projects all stay.
    #[test]
    fn a_forgotten_local_only_entry_sends_not_even_its_id() {
        let _scratch = Scratch::new();
        let memory = Memory::open();
        let private = private_memory(&memory);
        let machine = MachineId::this().unwrap();
        let secret = key("secret-111111");
        let (second, _) = memory.propose(&Who::Session("s1".into()), &MemFile::Decisions(secret.clone()), None, "Note: the vault code is COPPERKITE.", Source::Session("s1".into())).unwrap();
        memory.forget(&[private.proposal.as_str(), "0aa0f00d"], false).unwrap();
        let fact = remembered(&memory, "The vault code is COPPERKITE.", &MemFile::Profile);
        memory.forget(&[fact.as_str()], true).unwrap();
        let (files, _) = outgoing(&memory, None, &LeftOut::of(private.excluded.clone()), &machine).unwrap();
        let payload = serde_json::to_string(&files).unwrap();
        for id in [private.proposal.as_str(), "0aa0f00d", second.as_str()] {
            assert!(!payload.contains(id), "the id {id} of what stays here goes to the server");
        }
        assert!(payload.contains(&format!("id={fact}")), "the profile's own tombstone goes");
    }

    /// M57: entries both ways by stamp and tombstone. A forgotten entry goes as its tombstone, an
    /// accepted proposal leaves the other inbox too, and the server's copy carries no authority: a
    /// person's edit from there is refused, a confirmation there is only learned here.
    #[test]
    fn memory_entries_both_ways() {
        let scratch = Scratch::new();
        let (mac_home, server_home) = (scratch.at("mac"), scratch.at("server"));
        let app = key("app-3f9a1c");
        let file = MemFile::Project(app.clone());
        at_home(&mac_home);
        let mac = Memory::open();
        let machine = MachineId::this().unwrap();
        let kept = remembered(&mac, "Builds with cargo.", &file);
        let dropped = remembered(&mac, "Deploys on Fridays.", &file);
        let (proposal, _) = mac.propose(&Who::Session("s1".into()), &file, None, "Run the tests with nextest.", Source::Session("s1".into())).unwrap();
        let (to_server, sent_at) = outgoing(&mac, None, &LeftOut::none(), &machine).unwrap();

        at_home(&server_home);
        let server = Memory::open();
        server.take_synced(&to_server, false, &LeftOut::none()).unwrap();
        let before = server.snapshot().unwrap().commit().map(String::from);
        server.forget(&[dropped.as_str()], false).unwrap();
        let added = remembered(&server, "Releases need two reviews.", &file);
        // The person's own edit, as the server's copy says: never taken by the Mac.
        let path = server.dir().join(file.rel());
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("- Ship without review. <!-- id=0bb0f00d status=confirmed kind=fact source=person-edit -->\n");
        std::fs::write(&path, text).unwrap();
        server.accept(&[file.rel()]).unwrap();
        let server_machine = MachineId::this().unwrap();
        let (to_mac, _) = outgoing(&server, before.as_deref(), &LeftOut::none(), &server_machine).unwrap();

        at_home(&mac_home);
        let mac = Memory::open();
        let taken = mac.take_synced(&to_mac, true, &LeftOut::none()).unwrap();
        assert!(taken.refused >= 1, "the person's edit from the server");
        let snap = mac.snapshot().unwrap();
        let doc = snap.doc(&file).unwrap();
        assert!(doc.find(&kept).is_some());
        assert!(doc.find(&dropped).is_none() && doc.tombstones().any(|(id, _)| id == dropped), "forgotten there, forgotten here");
        let new = doc.find(&added).expect("the server's new entry");
        assert_ne!(new.header.status, Status::Confirmed, "confirmed there is not confirmed here");
        assert!(doc.find("0bb0f00d").is_none());
        // Taken once: the same again changes nothing.
        assert_eq!(mac.take_synced(&to_mac, true, &LeftOut::none()).unwrap().changed, 0);

        // The proposal is accepted on the Mac: it leaves the server's inbox too.
        mac.accept_proposal(&proposal).unwrap();
        let (after, _) = outgoing(&mac, sent_at.as_deref(), &LeftOut::none(), &machine).unwrap();
        at_home(&server_home);
        let server = Memory::open();
        server.take_synced(&after, false, &LeftOut::none()).unwrap();
        let inbox = server.snapshot().unwrap().doc(&MemFile::Proposals).unwrap();
        assert!(inbox.find(&proposal).is_none(), "accepted on the Mac, gone from the server's inbox");
        assert!(server.snapshot().unwrap().find(&proposal).unwrap().is_some_and(|(file, _)| file == MemFile::Project(app.clone())), "and where it went");
    }
}
