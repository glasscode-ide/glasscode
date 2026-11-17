//! The manager's data on disk: `~/.glasscode/manager/data.json`, and how two copies of it
//! (the Mac's and the server's) are brought together.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;

use super::{ChatLine, Data, InMessage, MsgState, Who};

/// How many lines of the conversation are kept.
pub const CHAT_KEEP: usize = 400;

/// Key in `Data::seen` that says a check was dropped, so the other copy drops it too.
const DROPPED_CHECK: &str = "check.dropped:";

/// `~/.glasscode/manager`.
pub fn dir() -> PathBuf {
    crate::paths::root().join("manager")
}

/// `~/.glasscode/manager/data.json`.
pub fn file() -> PathBuf {
    dir().join("data.json")
}

/// The data, or empty when there is none yet or it cannot be read.
pub fn load() -> Data {
    load_from(&file())
}

/// Write it (atomically: a temp file, then rename).
pub fn save(data: &Data) -> anyhow::Result<()> {
    save_to(data, &file())
}

/// The data in this file, or empty when there is none or it cannot be read. A file that
/// cannot be read is kept beside it as `data.json.bad`, so the next save does not lose it.
pub fn load_from(path: &Path) -> Data {
    let Ok(bytes) = std::fs::read(path) else { return Data::default() };
    match serde_json::from_slice(&bytes) {
        Ok(data) => data,
        Err(err) => {
            if !bytes.iter().all(u8::is_ascii_whitespace) {
                log::warn!("manager: {} cannot be read ({err}); starting empty", path.display());
                let _ = std::fs::copy(path, path.with_extension("json.bad"));
            }
            Data::default()
        }
    }
}

/// Write the data to this file: into a temp file beside it, then renamed over it, so a
/// reader never sees half a file. Only the owner can read it.
pub fn save_to(data: &Data, path: &Path) -> anyhow::Result<()> {
    use std::io::Write;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    if !dir.exists() {
        std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let bytes = serde_json::to_vec_pretty(data)?;
    let mut tmp = tempfile::Builder::new().prefix(".data.").suffix(".tmp").tempfile_in(&dir).with_context(|| format!("could not write in {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file().set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    tmp.write_all(&bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|err| err.error).with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

/// Drop a check for good: it is removed here, and `merge` removes it from the other copy
/// too instead of bringing it back. Returns whether there was one.
pub fn drop_check(data: &mut Data, id: &str) -> bool {
    let before = data.checks.len();
    data.checks.retain(|c| c.id != id);
    data.seen.insert(format!("{DROPPED_CHECK}{id}"), "1".into());
    data.checks.len() != before
}

/// Take what `theirs` has that is newer, item by item (by id, newest `updated` wins; chat lines
/// and `seen` are unioned; the newer brief and heartbeat win). Used to keep the Mac's and the
/// server's copies in step. Returns whether `mine` changed.
///
/// Three things are stricter than "newest wins", so that nothing happens twice: a message
/// that was sent stays sent, a message that was handled stays handled, and a check that was
/// dropped (`drop_check`) stays dropped. Where two different versions carry the same time,
/// both copies pick the same one.
pub fn merge(mine: &mut Data, theirs: &Data) -> bool {
    let before = mine.clone();
    // The copy that did the later round is the one whose `seen` is up to date.
    let they_ran_later = theirs.heartbeat > mine.heartbeat;

    merge_items(&mut mine.followups, &theirs.followups, |f| &f.id, |a, b| a.updated.cmp(&b.updated));
    merge_items(&mut mine.outbox, &theirs.outbox, |m| &m.id, |a, b| (a.state == MsgState::Sent).cmp(&(b.state == MsgState::Sent)).then(a.updated.cmp(&b.updated)));
    merge_items(&mut mine.notes, &theirs.notes, |n| &n.id, |a, b| a.updated.cmp(&b.updated).then(a.deleted.cmp(&b.deleted)));
    merge_items(&mut mine.checks, &theirs.checks, |c| &c.id, |a, b| a.last.cmp(&b.last));
    merge_inbox(&mut mine.inbox, &theirs.inbox);
    merge_chat(&mut mine.chat, &theirs.chat);

    for (key, value) in &theirs.seen {
        match mine.seen.get(key) {
            None => {
                mine.seen.insert(key.clone(), value.clone());
            }
            Some(current) if current != value && they_ran_later => {
                mine.seen.insert(key.clone(), value.clone());
            }
            Some(_) => {}
        }
    }
    let seen = &mine.seen;
    mine.checks.retain(|c| !seen.contains_key(&format!("{DROPPED_CHECK}{}", c.id)));

    if let Some(brief) = &theirs.brief {
        let take = match &mine.brief {
            None => true,
            Some(current) => wins(brief, current, brief.made.cmp(&current.made)),
        };
        if take {
            mine.brief = Some(brief.clone());
        }
    }
    mine.heartbeat = mine.heartbeat.max(theirs.heartbeat);
    *mine != before
}

/// Whether their version replaces mine. With the same age and different content, the one
/// that sorts later as JSON wins: an arbitrary rule, but both copies apply it alike.
fn wins<T: PartialEq + Serialize>(theirs: &T, mine: &T, age: Ordering) -> bool {
    match age {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => theirs != mine && serde_json::to_string(theirs).unwrap_or_default() > serde_json::to_string(mine).unwrap_or_default(),
    }
}

/// Items by id: theirs are added when new, and replace mine when `newer` says so.
fn merge_items<T: Clone + PartialEq + Serialize>(mine: &mut Vec<T>, theirs: &[T], id: fn(&T) -> &String, newer: fn(&T, &T) -> Ordering) {
    let mut at: HashMap<String, usize> = mine.iter().enumerate().map(|(i, item)| (id(item).clone(), i)).collect();
    for item in theirs {
        match at.get(id(item)) {
            Some(&i) => {
                if wins(item, &mine[i], newer(item, &mine[i])) {
                    mine[i] = item.clone();
                }
            }
            None => {
                at.insert(id(item).clone(), mine.len());
                mine.push(item.clone());
            }
        }
    }
}

/// Messages by id. What arrived does not change; only `handled` does, and it stays true.
fn merge_inbox(mine: &mut Vec<InMessage>, theirs: &[InMessage]) {
    let mut at: HashMap<String, usize> = mine.iter().enumerate().map(|(i, m)| (m.id.clone(), i)).collect();
    for message in theirs {
        match at.get(&message.id) {
            Some(&i) => mine[i].handled |= message.handled,
            None => {
                at.insert(message.id.clone(), mine.len());
                mine.push(message.clone());
            }
        }
    }
}

fn who_rank(who: Who) -> u8 {
    match who {
        Who::You => 0,
        Who::Manager => 1,
        Who::System => 2,
    }
}

/// Both conversations as one: every line once (same time, speaker and text), in time order,
/// the last `CHAT_KEEP` kept. Lines of the same second keep the order they had.
fn merge_chat(mine: &mut Vec<ChatLine>, theirs: &[ChatLine]) {
    let mut all: Vec<ChatLine> = Vec::with_capacity(mine.len() + theirs.len());
    {
        let mut have: HashSet<(u64, u8, &str)> = HashSet::new();
        for line in mine.iter().chain(theirs) {
            if have.insert((line.at, who_rank(line.who), line.text.as_str())) {
                all.push(line.clone());
            }
        }
    }
    all.sort_by_key(|line| line.at);
    if all.len() > CHAT_KEEP {
        all.drain(..all.len() - CHAT_KEEP);
    }
    *mine = all;
}

#[cfg(test)]
mod tests {
    use super::super::{Brief, Check, FollowState, FollowUp, Note, OutMessage, Tier};
    use super::*;

    fn follow(id: &str, note: &str, updated: u64) -> FollowUp {
        FollowUp { id: id.into(), note: note.into(), updated, ..Default::default() }
    }

    fn out(id: &str, state: MsgState, updated: u64) -> OutMessage {
        OutMessage { id: id.into(), to: "Sam".into(), body: "The fix for #1 is pushed\nRobo, on behalf of Alex".into(), tier: Tier::Routine, state, updated, ..Default::default() }
    }

    fn line(who: Who, text: &str, at: u64) -> ChatLine {
        ChatLine { who, text: text.into(), at, show: None }
    }

    fn incoming(id: &str, handled: bool) -> InMessage {
        InMessage { id: id.into(), chat: "c1".into(), from: "Mira".into(), text: "hello".into(), at: 5, urgent: false, handled }
    }

    #[test]
    fn saves_and_loads_privately_and_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("manager").join("data.json");
        assert_eq!(load_from(&path), Data::default(), "no file yet reads as empty");

        let mut data = Data::default();
        data.followups.push(follow("f1", "watch the login fix", 10));
        data.chat.push(line(Who::You, "what is new?", 11));
        data.seen.insert("pr:1".into(), "APPROVED".into());
        data.heartbeat = 12;
        save_to(&data, &path).unwrap();
        assert_eq!(load_from(&path), data);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        }

        // Saving again replaces the file and leaves no temp file behind.
        data.heartbeat = 99;
        save_to(&data, &path).unwrap();
        assert_eq!(load_from(&path).heartbeat, 99);
        let names: Vec<String> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["data.json".to_string()]);
    }

    #[test]
    fn a_broken_file_reads_as_empty_and_is_kept_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("data.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load_from(&path), Data::default());
        assert_eq!(std::fs::read(tmp.path().join("data.json.bad")).unwrap(), b"{ not json");

        // An empty file is nothing worth keeping.
        let empty = tmp.path().join("empty.json");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(load_from(&empty), Data::default());
        assert!(!tmp.path().join("empty.json.bad").exists());
    }

    #[test]
    fn older_files_with_missing_fields_still_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("data.json");
        std::fs::write(&path, br#"{"heartbeat": 7, "notes": [{"id": "n1", "text": "Sam reviews on Fridays"}], "later_field": true}"#).unwrap();
        let data = load_from(&path);
        assert_eq!(data.heartbeat, 7);
        assert_eq!(data.notes[0].text, "Sam reviews on Fridays");
    }

    #[test]
    fn the_newer_item_wins_in_both_directions() {
        let mut mine = Data { followups: vec![follow("f1", "old", 10), follow("f2", "mine is newer", 30)], ..Default::default() };
        let theirs = Data { followups: vec![follow("f1", "new", 20), follow("f2", "theirs is older", 25), follow("f3", "only theirs", 5)], ..Default::default() };
        assert!(merge(&mut mine, &theirs));
        assert_eq!(mine.followups.len(), 3);
        assert_eq!(mine.followups[0].note, "new");
        assert_eq!(mine.followups[1].note, "mine is newer");
        assert_eq!(mine.followups[2].id, "f3");
        // Nothing left to take.
        assert!(!merge(&mut mine, &theirs));

        // The other copy ends up with the same.
        let mut other = theirs.clone();
        assert!(merge(&mut other, &mine));
        let by_id = |d: &Data| {
            let mut v = d.followups.clone();
            v.sort_by(|a, b| a.id.cmp(&b.id));
            v
        };
        assert_eq!(by_id(&other), by_id(&mine));
    }

    #[test]
    fn the_same_data_changes_nothing() {
        let mut mine = Data {
            followups: vec![follow("f1", "a", 1)],
            outbox: vec![out("o1", MsgState::Draft, 2)],
            inbox: vec![incoming("i1", false)],
            chat: vec![line(Who::You, "hi", 1), line(Who::Manager, "hello", 1)],
            heartbeat: 4,
            ..Default::default()
        };
        let theirs = mine.clone();
        assert!(!merge(&mut mine, &theirs));
        assert_eq!(mine, theirs);
        assert!(!merge(&mut mine, &Data::default()));
    }

    #[test]
    fn two_versions_of_the_same_age_end_up_the_same_on_both_sides() {
        let a = Data { followups: vec![follow("f1", "apple", 10)], ..Default::default() };
        let b = Data { followups: vec![follow("f1", "pear", 10)], ..Default::default() };
        let (mut left, mut right) = (a.clone(), b.clone());
        merge(&mut left, &b);
        merge(&mut right, &a);
        assert_eq!(left.followups, right.followups);
    }

    #[test]
    fn a_follow_up_closed_on_one_side_closes_on_the_other() {
        let mut open = follow("f1", "", 10);
        open.state = FollowState::Open;
        let mut done = follow("f1", "", 20);
        done.state = FollowState::Done;
        let mut mine = Data { followups: vec![open], ..Default::default() };
        assert!(merge(&mut mine, &Data { followups: vec![done], ..Default::default() }));
        assert_eq!(mine.followups[0].state, FollowState::Done);
    }

    #[test]
    fn a_sent_message_stays_sent_whatever_the_times_say() {
        // Theirs was sent, mine carries a later change: it must not go out a second time.
        let mut sent = out("o1", MsgState::Sent, 10);
        sent.sent = Some(10);
        let mut mine = Data { outbox: vec![out("o1", MsgState::Approved, 50)], ..Default::default() };
        assert!(merge(&mut mine, &Data { outbox: vec![sent.clone()], ..Default::default() }));
        assert_eq!(mine.outbox[0].state, MsgState::Sent);
        assert_eq!(mine.outbox[0].sent, Some(10));

        // The other way round: mine was sent, theirs is a later approval.
        let mut mine = Data { outbox: vec![sent], ..Default::default() };
        assert!(!merge(&mut mine, &Data { outbox: vec![out("o1", MsgState::Approved, 50)], ..Default::default() }));
        assert_eq!(mine.outbox[0].state, MsgState::Sent);
    }

    #[test]
    fn the_persons_later_decision_on_a_draft_carries_over() {
        let mut mine = Data { outbox: vec![out("o1", MsgState::Draft, 10)], ..Default::default() };
        assert!(merge(&mut mine, &Data { outbox: vec![out("o1", MsgState::Rejected, 20)], ..Default::default() }));
        assert_eq!(mine.outbox[0].state, MsgState::Rejected);
        // An older draft from the other copy does not undo it.
        assert!(!merge(&mut mine, &Data { outbox: vec![out("o1", MsgState::Draft, 10)], ..Default::default() }));
        assert_eq!(mine.outbox[0].state, MsgState::Rejected);
    }

    #[test]
    fn a_forgotten_note_is_forgotten_on_the_other_copy() {
        let note = |deleted: bool, updated: u64| Note { id: "n1".into(), text: "Sam is away in May".into(), deleted, updated, ..Default::default() };
        let mut mine = Data { notes: vec![note(false, 10)], ..Default::default() };
        assert!(merge(&mut mine, &Data { notes: vec![note(true, 20)], ..Default::default() }));
        assert!(mine.notes[0].deleted);
        // An older live version does not bring it back, a newer one does.
        assert!(!merge(&mut mine, &Data { notes: vec![note(false, 10)], ..Default::default() }));
        assert!(mine.notes[0].deleted);
        assert!(merge(&mut mine, &Data { notes: vec![note(false, 30)], ..Default::default() }));
        assert!(!mine.notes[0].deleted);
        // Same second: forgetting wins.
        assert!(merge(&mut mine, &Data { notes: vec![note(true, 30)], ..Default::default() }));
        assert!(mine.notes[0].deleted);
    }

    #[test]
    fn handled_stays_true_in_both_directions() {
        let mut mine = Data { inbox: vec![incoming("i1", false)], ..Default::default() };
        assert!(merge(&mut mine, &Data { inbox: vec![incoming("i1", true), incoming("i2", false)], ..Default::default() }));
        assert!(mine.inbox[0].handled);
        assert_eq!(mine.inbox.len(), 2);

        let mut mine = Data { inbox: vec![incoming("i1", true)], ..Default::default() };
        assert!(!merge(&mut mine, &Data { inbox: vec![incoming("i1", false)], ..Default::default() }));
        assert!(mine.inbox[0].handled);
    }

    #[test]
    fn the_conversation_is_joined_once_and_in_time_order() {
        let mut mine = Data { chat: vec![line(Who::You, "what is new?", 10), line(Who::Manager, "Two reviews came in.", 10), line(Who::You, "show me", 30)], ..Default::default() };
        let theirs = Data { chat: vec![line(Who::Manager, "Two reviews came in.", 10), line(Who::System, "The Mac is asleep.", 20), line(Who::Manager, "what is new?", 10), line(Who::Manager, "Done.", 40)], ..Default::default() };
        assert!(merge(&mut mine, &theirs));
        let texts: Vec<(&str, u64)> = mine.chat.iter().map(|l| (l.text.as_str(), l.at)).collect();
        assert_eq!(texts, vec![("what is new?", 10), ("Two reviews came in.", 10), ("what is new?", 10), ("The Mac is asleep.", 20), ("show me", 30), ("Done.", 40)]);
        // The same words from someone else at the same second are a line of their own.
        assert_eq!(mine.chat[0].who, Who::You);
        assert_eq!(mine.chat[2].who, Who::Manager);
        assert!(!merge(&mut mine, &theirs));
    }

    #[test]
    fn the_conversation_keeps_the_last_400_lines() {
        let mut mine = Data { chat: (0..300).map(|i| line(Who::You, &format!("mine {i}"), 1000 + i)).collect(), ..Default::default() };
        let theirs = Data { chat: (0..300).map(|i| line(Who::Manager, &format!("theirs {i}"), 1150 + i)).collect(), ..Default::default() };
        assert!(merge(&mut mine, &theirs));
        assert_eq!(mine.chat.len(), CHAT_KEEP);
        assert_eq!(mine.chat.last().map(|l| l.text.as_str()), Some("theirs 299"));
        assert!(mine.chat.windows(2).all(|w| w[0].at <= w[1].at));
        // The oldest 200 of the 600 lines went: 150 of mine alone, then 25 seconds of both.
        assert_eq!(mine.chat[0].at, 1175);
        // Older lines offered again do not come back, and nothing changes.
        assert!(!merge(&mut mine, &theirs));
        let old = Data { chat: vec![line(Who::You, "mine 0", 1000)], ..Default::default() };
        assert!(!merge(&mut mine, &old));
    }

    #[test]
    fn seen_is_joined_and_the_copy_that_ran_later_wins_a_disagreement() {
        let seen = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let mut mine = Data { seen: seen(&[("pr:1", "COMMENTED"), ("pr:2", "APPROVED")]), heartbeat: 100, ..Default::default() };
        let theirs = Data { seen: seen(&[("pr:1", "APPROVED"), ("pr:3", "PENDING")]), heartbeat: 200, ..Default::default() };
        assert!(merge(&mut mine, &theirs));
        assert_eq!(mine.seen, seen(&[("pr:1", "APPROVED"), ("pr:2", "APPROVED"), ("pr:3", "PENDING")]));
        assert_eq!(mine.heartbeat, 200);

        // The copy that ran later keeps its own value and only learns new keys.
        let mut mine = Data { seen: seen(&[("pr:1", "APPROVED")]), heartbeat: 200, ..Default::default() };
        let theirs = Data { seen: seen(&[("pr:1", "COMMENTED"), ("pr:9", "x")]), heartbeat: 100, ..Default::default() };
        assert!(merge(&mut mine, &theirs));
        assert_eq!(mine.seen, seen(&[("pr:1", "APPROVED"), ("pr:9", "x")]));
        assert_eq!(mine.heartbeat, 200);
    }

    #[test]
    fn the_newer_brief_and_the_larger_heartbeat_win() {
        let brief = |made: u64, text: &str| Some(Brief { day: "2026-11-15".into(), made, text: text.into(), lines: Vec::new() });
        let mut mine = Data { brief: None, heartbeat: 5, ..Default::default() };
        assert!(merge(&mut mine, &Data { brief: brief(10, "first"), heartbeat: 3, ..Default::default() }));
        assert_eq!(mine.brief.as_ref().map(|b| b.text.as_str()), Some("first"));
        assert_eq!(mine.heartbeat, 5);

        assert!(merge(&mut mine, &Data { brief: brief(20, "second"), heartbeat: 9, ..Default::default() }));
        assert_eq!(mine.brief.as_ref().map(|b| b.text.as_str()), Some("second"));
        assert_eq!(mine.heartbeat, 9);

        assert!(!merge(&mut mine, &Data { brief: brief(15, "older"), heartbeat: 1, ..Default::default() }));
        assert!(!merge(&mut mine, &Data { brief: None, ..Default::default() }));
        assert_eq!(mine.brief.as_ref().map(|b| b.text.as_str()), Some("second"));
    }

    #[test]
    fn checks_keep_the_later_run_and_a_dropped_check_stays_dropped() {
        let check = |id: &str, last: u64| Check { id: id.into(), what: "see whether #1111 was merged".into(), every_secs: 600, last, once: false };
        let mut mine = Data { checks: vec![check("c1", 100), check("c2", 500)], ..Default::default() };
        let theirs = Data { checks: vec![check("c1", 300), check("c2", 200), check("c3", 0)], ..Default::default() };
        assert!(merge(&mut mine, &theirs));
        assert_eq!(mine.checks.iter().map(|c| (c.id.as_str(), c.last)).collect::<Vec<_>>(), vec![("c1", 300), ("c2", 500), ("c3", 0)]);

        // Dropped here: the other copy still has it, and it must not come back.
        assert!(drop_check(&mut mine, "c3"));
        assert!(!drop_check(&mut mine, "c3"));
        merge(&mut mine, &theirs);
        assert!(mine.checks.iter().all(|c| c.id != "c3"));
        // And the other copy drops it when it hears of it.
        let mut other = theirs.clone();
        assert!(merge(&mut other, &mine));
        assert!(other.checks.iter().all(|c| c.id != "c3"));
        assert_eq!(other.checks.len(), 2);
    }
}
