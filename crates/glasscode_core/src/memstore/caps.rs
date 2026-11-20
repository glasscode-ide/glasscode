//! Caps (M20.1 step 7; the table is in design section 4, "Memory caps"). A file holds at most so
//! many characters of what a brief can carry: the text of its entries that are not let go. Headers,
//! headings, tombstones and prose do not count (mem-9). A write that would take a file over its cap,
//! or further over it, fails with "Merge or let go first", and lists the entries with their ids and
//! sizes so the caller can consolidate (`memory.merge`, `memory.let_go`, `memory.rewrite`). A
//! monorepo is one project key (M11), so its one PROJECT.md takes the same cap.

use super::entry::{Doc, Status};
use super::layout::{FileKind, MemFile};

/// The cap of a file in characters, or None for a file that is never put in a brief whole.
pub fn cap_of(file: &MemFile) -> Option<usize> {
    Some(match file.kind() {
        FileKind::Profile => 1_500,
        FileKind::People => 2_000,
        FileKind::Glossary => 2_000,
        FileKind::Project => 3_000,
        FileKind::Policy => 1_500,
        FileKind::Decisions => 4_000,
        FileKind::Knowhow => 4_000,
        FileKind::Work => 3_000,
        // What waits for the person: a session cannot fill the inbox without end.
        FileKind::Proposals => 20_000,
        FileKind::Archive | FileKind::Rejected | FileKind::Index => return None,
    })
}

/// The characters of a file that count against its cap.
pub fn used(doc: &Doc) -> usize {
    doc.entries().filter(|entry| entry.header.status != Status::LetGo).map(|entry| entry.text.chars().count()).sum()
}

/// A write that would take a file over its cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverCap {
    pub file: String,
    pub cap: usize,
    /// What the file would hold after the write.
    pub would_hold: usize,
    /// Every entry that counts: id, characters, and the start of its text.
    pub entries: Vec<(String, usize, String)>,
}

impl std::fmt::Display for OverCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Merge or let go first: {} would hold {} of its {} characters.", self.file, self.would_hold, self.cap)?;
        for (id, size, start) in &self.entries {
            writeln!(f, "  {id} ({size}): {start}")?;
        }
        Ok(())
    }
}

impl std::error::Error for OverCap {}

/// Whether `after` may replace `before` in `file`: within the cap, or no larger than it was (so a
/// file over its cap, as an import may leave it, can always be made smaller).
pub fn check(file: &MemFile, before: &Doc, after: &Doc) -> Result<(), OverCap> {
    let Some(cap) = cap_of(file) else { return Ok(()) };
    let would_hold = used(after);
    if would_hold <= cap || would_hold <= used(before) {
        return Ok(());
    }
    let entries = after
        .entries()
        .filter(|entry| entry.header.status != Status::LetGo)
        .map(|entry| {
            let start: String = entry.text.chars().take(60).collect();
            (entry.header.id.clone(), entry.text.chars().count(), start)
        })
        .collect();
    Err(OverCap { file: file.rel(), cap, would_hold, entries })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Hlc;
    use crate::memstore::entry::{Entry, Header, Kind, Source};
    use crate::paths::{MachineId, ProjectKey};

    fn entry(id: &str, size: usize, status: Status) -> Entry {
        let at = Hlc::zero(MachineId::parse("0123456789abcdef").unwrap());
        Entry { text: "x".repeat(size), header: Header::new(id.into(), status, Kind::Fact, Source::V1, at) }
    }

    #[test]
    fn over_cap_write_fails() {
        let file = MemFile::Profile;
        let mut doc = Doc::new("profile", &["Working hours"]);
        doc.add(Some("Working hours"), entry("a", 1_400, Status::Confirmed));
        let mut more = doc.clone();
        more.add(Some("Working hours"), entry("b", 99, Status::Confirmed));
        assert_eq!(check(&file, &doc, &more), Ok(()), "1,499 of 1,500");
        let mut over = more.clone();
        over.add(Some("Working hours"), entry("c", 2, Status::Mentioned));
        let err = check(&file, &more, &over).unwrap_err();
        assert_eq!((err.cap, err.would_hold, err.entries.len()), (1_500, 1_501, 3));
        assert!(err.to_string().starts_with("Merge or let go first: you/profile.md would hold 1501 of its 1500 characters."), "{err}");
        assert!(err.to_string().contains("  a (1400): "));
        // Let-go entries do not count; a file over its cap may still shrink.
        let mut let_go = over.clone();
        let mut c = let_go.find("a").unwrap().clone();
        c.header.status = Status::LetGo;
        let_go.replace("a", c);
        assert_eq!(check(&file, &over, &let_go), Ok(()));
        let mut smaller = over.clone();
        smaller.take("c");
        assert_eq!(check(&file, &over, &smaller), Ok(()), "1,501 to 1,499");
        let archive = MemFile::Archive(ProjectKey::parse("app-3f9a1c").unwrap(), "2026-11".into(), "x".into());
        assert_eq!(check(&archive, &doc, &over), Ok(()), "the archive has no cap");
    }
}
