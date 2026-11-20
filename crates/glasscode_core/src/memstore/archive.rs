//! The archive (M24, D23): when a piece of work ends, however it ends, what memory had of it and
//! what lasts of its conversations go into `projects/<key>/archive/<yyyy-mm>/work/<name>.md`, in one
//! commit, and its live work file leaves the project. Search still finds it, a little lower.
//!
//! **Ways work ends** ([`Ended`]): its worktree is removed by Glasscode (the command, the clean, the
//! Worktrees dialog); the worktree goes some other way (`git worktree remove` in a terminal, the
//! folder deleted: the reconciler finds it, and its review notes are lost); a project is removed.
//!
//! **What it keeps** ([`ArchiveInput`]): the work file's entries (with their ids), the review notes
//! (`<worktree>/.glasscode/review/`, read before the removal), the pull request and ticket links
//! with their last state, the last reply, the handoff notes, and for each conversation its path
//! and a bounded digest (`transcript::digest`: Claude deletes old transcripts, so the digest is
//! what lasts, P12). Every line from a transcript or a note passes the secret part of the scan; a
//! line that holds a secret is left out and says so.
//!
//! **Idempotent per work.** The archive of a removal is named by the work and the UTC day it ended,
//! and never written over: archiving the same end again finds it and changes nothing. A branch
//! used again for new work, ended on the same day, gets the next free name.

use std::path::{Path, PathBuf};

use super::entry::{self, Doc, Line};
use super::layout::{MemFile, month_of};
use super::scan::{self, Rule};
use super::{Memory, MemoryError, Who, Written};
use crate::identity::WorkKey;
use crate::model::AgentKind;
use crate::paths::ProjectKey;
use crate::transcript::Digest;

/// How a piece of work ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// Glasscode removed its worktree (the command, the clean, the dialog).
    Removed,
    /// Its worktree went outside Glasscode; the reconciler found it. Its review notes are lost.
    RemovedOutside,
    /// Its project was removed from Glasscode.
    ProjectRemoved,
    /// Its pull request merged or closed, and no session of it was open for a week after (M24).
    PrEnded,
    /// Its worktree was removed on the server (glasscoded's own `worktrees.remove`).
    RemovedOnServer,
}

impl Ended {
    fn word(self) -> &'static str {
        match self {
            Ended::Removed => "removed",
            Ended::RemovedOutside => "removed-outside",
            Ended::ProjectRemoved => "project-removed",
            Ended::PrEnded => "pr-ended",
            Ended::RemovedOnServer => "removed-on-server",
        }
    }

    fn sentence(self) -> &'static str {
        match self {
            Ended::Removed => "Its worktree was removed in Glasscode.",
            Ended::RemovedOutside => "Its worktree was removed outside Glasscode; its review notes were lost.",
            Ended::ProjectRemoved => "Its project was removed from Glasscode.",
            Ended::PrEnded => "Its pull request merged or closed, and no session of it was open for a week after.",
            Ended::RemovedOnServer => "Its worktree was removed on the server.",
        }
    }
}

/// A conversation that ran in the work, with what lasts of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Conversation {
    pub agent: AgentKind,
    pub path: PathBuf,
    pub digest: Option<Digest>,
}

/// Everything the archive of one piece of work holds.
#[derive(Clone, Debug)]
pub struct ArchiveInput {
    pub project: ProjectKey,
    /// The work, when its key is known (a session's).
    pub work: Option<WorkKey>,
    /// The worktree it was in: work files that name it are archived too.
    pub worktree: Option<PathBuf>,
    pub branch: Option<String>,
    pub ended: Ended,
    /// When it ended (UTC milliseconds, from `clock`).
    pub at_ms: u64,
    /// The review notes: file name and text.
    pub review_notes: Vec<(String, String)>,
    /// "PR acme/app#42" → "merged", "ticket ENG-42" → "Done".
    pub links: Vec<(String, String)>,
    pub last_reply: Option<String>,
    /// The last turn report of its sessions (M30), as one line.
    pub last_turn: Option<String>,
    /// Its sessions' ids, newest last: what the last turn report is looked up by.
    pub sessions: Vec<String>,
    /// The handoff notes: file and the developer's note in it.
    pub handoff: Vec<(PathBuf, String)>,
    pub conversations: Vec<Conversation>,
}

impl ArchiveInput {
    pub fn new(project: ProjectKey, ended: Ended, at_ms: u64) -> ArchiveInput {
        ArchiveInput { project, work: None, worktree: None, branch: None, ended, at_ms, review_notes: Vec::new(), links: Vec::new(), last_reply: None, last_turn: None, sessions: Vec::new(), handoff: Vec::new(), conversations: Vec::new() }
    }

    /// Whether there is anything to keep beyond a work file.
    fn has_more(&self) -> bool {
        !self.review_notes.is_empty() || !self.links.is_empty() || self.last_reply.is_some() || self.last_turn.is_some() || !self.handoff.is_empty() || !self.conversations.is_empty()
    }
}

/// What an archive did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Archived {
    /// The archive file, when one was written or was already there.
    pub file: Option<MemFile>,
    /// The live work files it took out of the project.
    pub moved: Vec<MemFile>,
    pub written: Written,
}

/// The most bytes of a review note or a handoff note that are kept.
pub const NOTE_BYTES: usize = 8 * 1024;

/// The review notes of a worktree (`<worktree>/.glasscode/review/`): each file's name and text,
/// clipped. Read them before the worktree is removed. It reads the disk.
pub fn review_notes(worktree: &Path) -> Vec<(String, String)> {
    let dir = worktree.join(".glasscode").join("review");
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut notes: Vec<(String, String)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| std::fs::read(e.path()).ok().map(|bytes| (e.file_name().to_string_lossy().into_owned(), clip_bytes(&String::from_utf8_lossy(&bytes), NOTE_BYTES))))
        .collect();
    notes.sort();
    notes
}

/// The handoff notes written for sessions of `worktree` (`handoff/*.md` that name it): each
/// file and the developer's note in it. It reads the disk.
pub fn handoff_notes(worktree: &Path) -> Vec<(PathBuf, String)> {
    let Ok(entries) = std::fs::read_dir(crate::paths::handoff_dir()) else { return Vec::new() };
    let named = format!("- Worktree: {}", worktree.display());
    let mut notes: Vec<(PathBuf, String)> = entries
        .flatten()
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            if !text.lines().any(|line| line.trim_end() == named) {
                return None;
            }
            let note = text.split("## Note from the developer").nth(1).map(|rest| rest.split("\n## ").next().unwrap_or("").trim().to_string()).unwrap_or_default();
            Some((e.path(), clip_bytes(&note, NOTE_BYTES)))
        })
        .collect();
    notes.sort();
    notes
}

fn clip_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// A line from a transcript or a note, as the archive keeps it: one line, `-->` neutralized, and
/// left out when the scan finds a secret in it (a token, a URL with a password, a `.env` value,
/// invisible text). Instruction-like words stay: the archive is data.
pub(super) fn kept_line(text: &str, env: &[String]) -> String {
    let line = entry::clean_text(text);
    let secret = scan::scan(&line, env).iter().any(|f| matches!(f.rule, Rule::Token | Rule::CredentialUrl | Rule::EnvValue | Rule::Invisible));
    if secret { "[left out: it looked like a secret]".into() } else { line }
}

/// The archive's name for the work and the day it ended (UTC): `<work file>-<yyyymmdd>`, then
/// `-2`, `-3`… when one of that name holds another end.
fn archive_file(project: &ProjectKey, stem: &str, at_ms: u64, n: usize) -> MemFile {
    let (year, month, day) = crate::day::to_civil((at_ms / 86_400_000) as i64);
    let stem: String = stem.chars().take(44).collect::<String>().trim_end_matches('-').to_string();
    let name = match n {
        0 => format!("work/{stem}-{year:04}{month:02}{day:02}"),
        n => format!("work/{stem}-{year:04}{month:02}{day:02}-{}", n + 1),
    };
    MemFile::Archive(project.clone(), month_of(at_ms), name)
}

impl Memory {
    /// Archive one piece of work (M24): its work files (the work's own, and any naming its
    /// worktree) and what `input` holds, into one archive file, in one commit; the work files leave
    /// the project. Archiving the same end again changes nothing.
    pub fn archive(&self, input: &ArchiveInput) -> Result<Archived, MemoryError> {
        let snap = self.snapshot()?;
        // The live work files of this work.
        let mut work_files: Vec<MemFile> = Vec::new();
        if let Some(work) = &input.work
            && let Some(file) = snap.work_file(work)?
        {
            work_files.push(file);
        }
        if let Some(worktree) = &input.worktree {
            let folder = worktree.to_string_lossy().into_owned();
            for file in snap.files()?.into_iter().filter(|f| matches!(f, MemFile::Work(key, _) if *key == input.project)) {
                if work_files.contains(&file) {
                    continue;
                }
                if snap.doc(&file)?.marker("work").is_some_and(|fields| fields.get("worktree") == Some(&folder)) {
                    work_files.push(file);
                }
            }
        }
        let stem = match (&input.work, work_files.first(), &input.worktree) {
            (Some(work), _, _) => work.file_name(),
            (None, Some(MemFile::Work(_, stem)), _) => stem.clone(),
            (None, _, Some(worktree)) => crate::git::slug(&worktree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "worktree".into())),
            _ => "work".into(),
        };
        let stem = super::layout::topic(&stem).unwrap_or_else(|| "work".into());
        // Its name: the first free one, or the one already holding this end.
        let mut name = None;
        for n in 0..16 {
            let file = archive_file(&input.project, &stem, input.at_ms, n);
            match snap.text(&file)? {
                None => {
                    name = Some(file);
                    break;
                }
                Some(_) if work_files.is_empty() => return Ok(Archived { file: Some(file), moved: Vec::new(), written: Written::Unchanged }),
                Some(_) => continue,
            }
        }
        let Some(file) = name else { return Err(MemoryError::NotAllowed("too many archives of this work on one day".into())) };
        if work_files.is_empty() && !input.has_more() {
            return Ok(Archived { file: None, moved: Vec::new(), written: Written::Unchanged });
        }
        let env = scan::env_values(&super::project_folders(Some(&input.project))?);
        let docs: Vec<(MemFile, Doc)> = work_files.iter().map(|f| Ok((f.clone(), snap.doc(f)?))).collect::<Result<_, MemoryError>>()?;
        let archive = render(input, &docs, &env);
        let moved = work_files.clone();
        let (_, written) = self.write(&Who::Manager, "Archive work", &[], |tx| {
            *tx.doc(&file)? = archive;
            for work in &moved {
                tx.remove(work);
            }
            Ok(())
        })?;
        super::search::forget_cache();
        Ok(Archived { file: Some(file), moved: work_files, written })
    }
}

impl Memory {
    /// The archive of a worktree that went outside Glasscode (the reconciler's
    /// `reconciled.worktree_gone`): every work file that names it, by project, archived as
    /// [`Ended::RemovedOutside`]. Its review notes are lost; what memory had stays.
    pub fn archive_gone(&self, worktree: &Path, at_ms: u64) -> Result<Vec<Archived>, MemoryError> {
        let snap = self.snapshot()?;
        let folder = worktree.to_string_lossy().into_owned();
        let mut projects: Vec<ProjectKey> = Vec::new();
        for file in snap.files()? {
            if let MemFile::Work(project, _) = &file
                && !projects.contains(project)
                && snap.doc(&file)?.marker("work").is_some_and(|fields| fields.get("worktree") == Some(&folder))
            {
                projects.push(project.clone());
            }
        }
        projects
            .into_iter()
            .map(|project| {
                let mut input = ArchiveInput::new(project, Ended::RemovedOutside, at_ms);
                input.worktree = Some(worktree.to_path_buf());
                self.archive(&input)
            })
            .collect()
    }
}

/// Fill what an archive reads from the disk (the review notes and handoff notes of its worktree,
/// each conversation's digest, the last reply), then archive: what a removal runs before
/// `git worktree remove`. It reads the disk and runs git: never on the main thread.
pub fn before_removal(mut input: ArchiveInput) -> Result<Archived, MemoryError> {
    if let Some(worktree) = &input.worktree {
        input.review_notes = review_notes(worktree);
        input.handoff = handoff_notes(worktree);
    }
    for conversation in &mut input.conversations {
        conversation.digest = crate::transcript::digest(conversation.agent, &conversation.path);
    }
    if input.last_reply.is_none() {
        input.last_reply = input.conversations.iter().rev().find_map(|c| crate::transcript::last_reply(c.agent, &c.path));
    }
    if input.last_turn.is_none() {
        input.last_turn = last_turn(&input.sessions);
    }
    Memory::open().archive(&input)
}

/// The newest turn report of these sessions (M30's `turn.ended`), as its one line. It reads the
/// journal.
pub fn last_turn(sessions: &[String]) -> Option<String> {
    sessions.iter().rev().find_map(|session| {
        let filter = crate::journal::Filter { session: Some(session.clone()), kind: Some(crate::journal::kinds::TURN_ENDED.as_str().to_string()), ..Default::default() };
        crate::journal::query(&filter, &crate::journal::Scope::All, 1).pop().map(|event| event.summary)
    })
}

/// The archive file of `input`, with the work files' entries kept as they were (ids and headers).
fn render(input: &ArchiveInput, work_docs: &[(MemFile, Doc)], env: &[String]) -> Doc {
    let title = input.work.as_ref().map(|w| w.id.clone()).or_else(|| input.branch.clone()).or_else(|| input.worktree.as_ref().map(|w| w.display().to_string())).unwrap_or_else(|| "work".into());
    let mut doc = Doc::new(&format!("{} (archived)", entry::clean_text(&title)), &[]);
    let mut fields: Vec<(String, String)> = vec![("project".into(), input.project.as_str().to_string()), ("ended".into(), input.ended.word().into()), ("at".into(), crate::day::iso((input.at_ms / 86_400_000) as i64))];
    if let Some(work) = &input.work {
        fields.push(("kind".into(), entry::kind_word(work.kind).into()));
        fields.push(("id".into(), work.id.clone()));
    }
    if let Some(worktree) = &input.worktree {
        fields.push(("worktree".into(), worktree.display().to_string()));
    }
    if let Some(branch) = &input.branch {
        fields.push(("branch".into(), branch.clone()));
    }
    let pairs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    doc.set_marker("archived", &pairs);
    doc.lines.push(Line::Text(String::new()));
    doc.lines.push(Line::Text(input.ended.sentence().into()));
    // The work files' entries, in their sections, as they were.
    for (_, work) in work_docs {
        let mut section: Option<String> = None;
        for line in &work.lines {
            match line {
                Line::Text(text) if text.starts_with("## ") => section = Some(text[3..].trim().to_string()),
                Line::Entry(e) => {
                    let mut kept = e.clone();
                    kept.header.more.retain(|(k, _)| k != "archived");
                    doc.add(section.as_deref().or(Some(entry::NOTES_FROM_SESSIONS)), kept);
                }
                _ => {}
            }
        }
    }
    let bullets = |doc: &mut Doc, heading: &str, lines: Vec<String>| {
        if lines.is_empty() {
            return;
        }
        doc.lines.push(Line::Text(String::new()));
        doc.lines.push(Line::Text(format!("## {heading}")));
        doc.lines.push(Line::Text(String::new()));
        for line in lines {
            doc.lines.push(Line::Text(format!("- {line}")));
        }
    };
    bullets(&mut doc, "Review notes", input.review_notes.iter().flat_map(|(name, text)| text.lines().filter(|l| !l.trim().is_empty()).map(move |l| format!("{name}: {}", l.trim()))).map(|l| kept_line(&l, env)).collect());
    bullets(&mut doc, "Links", input.links.iter().map(|(what, state)| kept_line(&format!("{what}: {state}"), env)).collect());
    bullets(&mut doc, "Last turn", input.last_turn.iter().map(|r| kept_line(r, env)).collect());
    bullets(&mut doc, "Last reply", input.last_reply.iter().map(|r| kept_line(r, env)).collect());
    bullets(&mut doc, "Handoff notes", input.handoff.iter().map(|(path, note)| kept_line(&format!("{}: {}", path.display(), if note.is_empty() { "(no note)" } else { note }), env)).collect());
    for conversation in &input.conversations {
        let mut lines = vec![kept_line(&format!("{} conversation: {}", conversation.agent.name(), conversation.path.display()), env)];
        match &conversation.digest {
            Some(digest) => {
                if let Some(first) = &digest.first_prompt {
                    lines.push(kept_line(&format!("First prompt: {first}"), env));
                }
                lines.extend(digest.replies.iter().map(|r| kept_line(&format!("Reply: {r}"), env)));
                lines.extend(digest.failed_tools.iter().map(|t| kept_line(&format!("Failed: {t}"), env)));
            }
            None => lines.push("Its transcript was gone when the work ended.".into()),
        }
        bullets(&mut doc, &format!("Conversation {}", conversation.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()), lines);
    }
    doc.newline = true;
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::WorkKind;
    use crate::memstore::tests_support::{Scratch, key};
    use crate::memstore::{Remembered, search};

    fn work(id: &str) -> WorkKey {
        WorkKey { project: key("app-3f9a1c"), kind: WorkKind::Branch, id: id.into() }
    }

    const DAY: u64 = 86_400_000;

    #[test]
    fn archive_idempotent() {
        let _scratch = Scratch::new();
        let memory = Memory::open();
        let w = work("fix/csv-export");
        let worktree = PathBuf::from("/tmp/wt/fix-csv-export");
        memory.note_in(&Who::Session("s-d".into()), &w, Some(&worktree), "the CSV export streams rows to avoid timeouts").unwrap();
        let live = memory.snapshot().unwrap().work_file(&w).unwrap().unwrap();
        let mut input = ArchiveInput::new(key("app-3f9a1c"), Ended::Removed, 20_400 * DAY);
        input.work = Some(w.clone());
        input.worktree = Some(worktree.clone());
        let first = memory.archive(&input).unwrap();
        assert!(matches!(first.written, Written::Commit(_)));
        assert_eq!(first.moved, [live.clone()]);
        let snap = memory.snapshot().unwrap();
        assert!(snap.text(&live).unwrap().is_none(), "the work file left the project");
        let file = first.file.clone().unwrap();
        let text = snap.text(&file).unwrap().unwrap();
        assert!(text.contains("the CSV export streams rows") && text.contains("<!-- archived project=app-3f9a1c ended=removed"), "{text}");
        assert!(matches!(&file, MemFile::Archive(_, month, name) if month == &month_of(20_400 * DAY) && name.starts_with("work/")));
        // Again: nothing changes.
        let again = memory.archive(&input).unwrap();
        assert_eq!((again.written, again.file), (Written::Unchanged, Some(file.clone())));
        // The branch used again for new work, ended the same day: a second archive, never over the first.
        memory.note_in(&Who::Session("s-e".into()), &w, Some(&worktree), "the new export keeps a header row").unwrap();
        let second = memory.archive(&input).unwrap();
        assert_ne!(second.file, Some(file.clone()));
        assert!(memory.snapshot().unwrap().text(&file).unwrap().unwrap().contains("streams rows"));
        // Work found by its worktree alone (a session of another work key in the same folder).
        let review = WorkKey { kind: WorkKind::Review, id: "acme/app#7".into(), ..w.clone() };
        memory.note_in(&Who::Session("s-f".into()), &review, Some(&worktree), "the review asked for a test").unwrap();
        let mut by_folder = ArchiveInput::new(key("app-3f9a1c"), Ended::RemovedOutside, 20_401 * DAY);
        by_folder.worktree = Some(worktree);
        let found = memory.archive(&by_folder).unwrap();
        assert_eq!(found.moved.len(), 1);
        assert!(memory.snapshot().unwrap().text(&found.file.unwrap()).unwrap().unwrap().contains("review notes were lost"));
    }

    #[test]
    fn review_notes_kept() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        let worktree = scratch.at("wt");
        std::fs::create_dir_all(worktree.join(".glasscode/review")).unwrap();
        std::fs::write(worktree.join(".glasscode/review/notes.md"), "Check the retry path.\nThe token is ghp_0123456789abcdefghijABCDEFGHIJ\n").unwrap();
        let mut input = ArchiveInput::new(key("app-3f9a1c"), Ended::Removed, 20_400 * DAY);
        input.work = Some(work("feat/retry"));
        input.review_notes = review_notes(&worktree);
        input.links = vec![("PR acme/app#42".into(), "merged".into())];
        input.last_reply = Some("Done: the retry path has a test.".into());
        let archived = memory.archive(&input).unwrap();
        let text = memory.snapshot().unwrap().text(&archived.file.unwrap()).unwrap().unwrap();
        assert!(text.contains("## Review notes\n\n- notes.md: Check the retry path."), "{text}");
        assert!(text.contains("[left out: it looked like a secret]") && !text.contains("ghp_0123"), "{text}");
        assert!(text.contains("- PR acme/app#42: merged") && text.contains("Done: the retry path has a test."));
    }

    #[test]
    fn outside_removal_archived_by_reconciler() {
        use crate::reconcile::{Reconciler, Snapshot};
        let scratch = Scratch::new();
        let memory = Memory::open();
        let worktree = scratch.at("worktrees/app/fix-upload");
        memory.note_in(&Who::Session("s-1".into()), &work("fix/upload"), Some(&worktree), "uploads over 10 MB go to the bucket directly").unwrap();
        // The reconciler knows the worktree, then it is gone (removed in a terminal).
        let mut reconciler = Reconciler::default();
        reconciler.run(&Snapshot { sessions: Vec::new(), worktrees: vec![(Some(key("app-3f9a1c")), worktree.clone())] });
        let events = reconciler.run(&Snapshot::default());
        let gone = events.iter().find(|e| e.kind == crate::journal::kinds::RECONCILED_WORKTREE_GONE).expect("a gone event");
        assert_eq!(gone.project, Some(key("app-3f9a1c")));
        let path = PathBuf::from(gone.data["worktree"].as_str().unwrap());
        let archived = memory.archive_gone(&path, crate::clock::now_ms()).unwrap();
        assert_eq!(archived.len(), 1);
        let text = memory.snapshot().unwrap().text(archived[0].file.as_ref().unwrap()).unwrap().unwrap();
        assert!(text.contains("uploads over 10 MB") && text.contains("ended=removed-outside"), "{text}");
        assert!(memory.archive_gone(&path, crate::clock::now_ms()).unwrap().is_empty(), "once");
    }

    #[test]
    fn remove_archives_before_delete() {
        let scratch = Scratch::new();
        let memory = Memory::open();
        // A worktree with a conversation, review notes and a work file.
        let worktree = scratch.at("wt-login");
        std::fs::create_dir_all(worktree.join(".glasscode/review")).unwrap();
        std::fs::write(worktree.join(".glasscode/review/review.md"), "Ask Sam about the session timeout.\n").unwrap();
        let transcript = scratch.at("t.jsonl");
        let lines = [
            serde_json::json!({"type": "user", "message": {"role": "user", "content": "Build the login form"}}).to_string(),
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "The login form uses useAuthForm now."}]}}).to_string(),
        ];
        std::fs::write(&transcript, lines.join("\n") + "\n").unwrap();
        let w = work("feat/login");
        memory.note_in(&Who::Session("s-1".into()), &w, Some(&worktree), "the reset link is still missing").unwrap();
        let mut input = ArchiveInput::new(key("app-3f9a1c"), Ended::Removed, crate::clock::now_ms());
        input.work = Some(w);
        input.worktree = Some(worktree.clone());
        input.conversations = vec![Conversation { agent: AgentKind::Claude, path: transcript.clone(), digest: None }];
        let archived = before_removal(input).unwrap();
        // Then the folder and the transcript go: the archive keeps what they held.
        std::fs::remove_dir_all(&worktree).unwrap();
        std::fs::remove_file(&transcript).unwrap();
        let text = memory.snapshot().unwrap().text(archived.file.as_ref().unwrap()).unwrap().unwrap();
        for kept in ["the reset link is still missing", "review.md: Ask Sam about the session timeout.", "First prompt: Build the login form", "Reply: The login form uses useAuthForm now.", "Last reply"] {
            assert!(text.contains(kept), "{kept:?} not in:\n{text}");
        }
    }

    #[test]
    fn the_archive_keeps_the_last_turn_report() {
        let scratch = Scratch::new();
        // The journal runs in a v2 home only.
        struct V2;
        impl Drop for V2 {
            fn drop(&mut self) {
                crate::journal::reset();
            }
        }
        crate::journal::reset();
        let _v2 = V2;
        assert!(crate::journal::init());
        let turn = crate::journal::Event::new(crate::journal::kinds::TURN_ENDED, crate::journal::Source::App, "completed in 42 s: 2 tools, 1 files").session(Some("s-9"));
        crate::journal::append(turn);
        crate::journal::flush();
        let memory = Memory::open();
        let worktree = scratch.at("wt-retry");
        std::fs::create_dir_all(&worktree).unwrap();
        let w = work("fix/retry");
        memory.note_in(&Who::Session("s-9".into()), &w, Some(&worktree), "the retry waits 30 s").unwrap();
        let mut input = ArchiveInput::new(key("app-3f9a1c"), Ended::RemovedOnServer, crate::clock::now_ms());
        input.work = Some(w);
        input.worktree = Some(worktree);
        input.sessions = vec!["s-9".into()];
        let archived = before_removal(input).unwrap();
        let text = memory.snapshot().unwrap().text(archived.file.as_ref().unwrap()).unwrap().unwrap();
        for kept in ["## Last turn", "completed in 42 s: 2 tools, 1 files", "removed on the server", "ended=removed-on-server"] {
            assert!(text.contains(kept), "{kept:?} not in:\n{text}");
        }
    }

    #[test]
    fn archived_work_found_by_search() {
        let _scratch = Scratch::new();
        let memory = Memory::open();
        let w = work("fix/csv-export");
        let Remembered::Entry { .. } = memory.note("s-d", &w, "the CSV export streams rows to avoid timeouts").unwrap() else { panic!() };
        let mut input = ArchiveInput::new(key("app-3f9a1c"), Ended::Removed, crate::clock::now_ms());
        input.work = Some(w);
        memory.archive(&input).unwrap();
        // Another session of the project, on other work, finds it.
        let snap = memory.snapshot().unwrap();
        let scope = search::scope_for(&snap, Some(&key("app-3f9a1c")), Some(&work("feat/other"))).unwrap();
        let hits = search::search(&snap, "How did we fix the CSV export timeouts?", &scope, 5).unwrap();
        assert!(hits.first().is_some_and(|h| matches!(h.file, MemFile::Archive(..)) && h.entries.iter().any(|e| e.text.contains("streams rows"))), "{hits:?}");
    }
}
