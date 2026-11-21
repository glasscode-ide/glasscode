//! Activity: how many sessions the person started and how many of their pull requests merged
//! (with the lines they changed) on each day, for the heat map and "This month" of Today.
//!
//! It is kept in `~/.glasscode/activity.json` from the first day that something was counted
//! (`since`); there is no history from before. Days are local dates (`2026-11-16`).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What one day had.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Day {
    #[serde(default)]
    pub sessions: u32,
    #[serde(default)]
    pub prs_merged: u32,
    /// The lines the merged pull requests changed (added and removed).
    #[serde(default)]
    pub lines: u32,
}

impl Day {
    fn add(&mut self, other: &Day) {
        self.sessions = self.sessions.saturating_add(other.sessions);
        self.prs_merged = self.prs_merged.saturating_add(other.prs_merged);
        self.lines = self.lines.saturating_add(other.lines);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    /// The first day counted: the days before it have no data (which is not zero activity).
    #[serde(default)]
    pub since: String,
    /// Each day that has a count.
    #[serde(default)]
    pub days: BTreeMap<String, Day>,
    /// The pull requests counted (`owner/name#12`): a merge seen again does not count again.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub merged: BTreeSet<String>,
}

pub fn file() -> PathBuf {
    crate::paths::root().join("activity.json")
}

/// The local date now (`2026-11-16`).
pub fn today() -> String {
    crate::day::iso(crate::day::today())
}

impl Activity {
    /// The file; with none, a broken one (a copy is kept as `activity.json.broken`) or one that
    /// cannot be read now, an empty history that starts today.
    pub fn load() -> Activity {
        read().unwrap_or_else(|| Activity::starting(&today()))
    }

    /// Write the whole file: to a file of its own first, then in place of the old one, so that a
    /// crash leaves the old file or the new one and never a part of one.
    pub fn save(&self) -> io::Result<()> {
        crate::paths::ensure_dirs()?;
        let path = file();
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        {
            let mut out = std::fs::File::create(&tmp)?;
            out.write_all(&bytes)?;
            out.sync_all()?;
        }
        std::fs::rename(&tmp, &path)
    }

    pub fn add_session(&mut self, day: &str) {
        let counts = self.day_mut(day);
        counts.sessions = counts.sessions.saturating_add(1);
    }

    /// A merged pull request, counted once for each `key` (`owner/name#12`).
    pub fn add_merge(&mut self, day: &str, lines: u32, key: &str) {
        if !self.merged.insert(key.to_string()) {
            return;
        }
        let counts = self.day_mut(day);
        counts.prs_merged = counts.prs_merged.saturating_add(1);
        counts.lines = counts.lines.saturating_add(lines);
    }

    /// Sessions for each day of the last `weeks` weeks, the oldest week first. A week runs from
    /// Monday (index 0) to Sunday (index 6), and the last one is the week of `today`; its days
    /// after `today`, and each day without data, are 0.
    pub fn weeks(&self, today: &str, weeks: usize) -> Vec<[u32; 7]> {
        let Some(today) = crate::day::parse_iso(today) else {
            return vec![[0; 7]; weeks];
        };
        let monday = today - i64::from(crate::day::weekday(today));
        let first = monday - 7 * (weeks as i64 - 1);
        (0..weeks as i64)
            .map(|week| {
                std::array::from_fn(|weekday| {
                    let date = first + 7 * week + weekday as i64;
                    if date > today {
                        return 0;
                    }
                    self.days.get(&crate::day::iso(date)).map_or(0, |counts| counts.sessions)
                })
            })
            .collect()
    }

    /// The month of `today` and the month before it, each summed.
    pub fn month(&self, today: &str) -> (Day, Day) {
        let Some(today) = crate::day::parse_iso(today) else {
            return (Day::default(), Day::default());
        };
        let (year, month, _) = crate::day::to_civil(today);
        let (before_year, before_month) = if month == 1 { (year - 1, 12) } else { (year, month - 1) };
        (self.sum(&format!("{year:04}-{month:02}-")), self.sum(&format!("{before_year:04}-{before_month:02}-")))
    }

    /// This history with the conversations the person had in their agents before it starts
    /// (`agent_history.rs`): each day before `since` gets its count, and the history then starts
    /// at the first of them. A day Glasscode counted itself is left as it is, and nothing here is
    /// saved: it is what Today shows.
    pub fn with_history(&self, days: &BTreeMap<String, u32>) -> Activity {
        let mut merged = self.clone();
        let since = self.since.clone();
        for (date, sessions) in days.iter().filter(|(date, n)| **n > 0 && (since.is_empty() || date.as_str() < since.as_str())) {
            let counts = merged.day_mut(date);
            counts.sessions = counts.sessions.saturating_add(*sessions);
        }
        merged
    }

    fn sum(&self, prefix: &str) -> Day {
        let mut total = Day::default();
        for (_, counts) in self.days.range(prefix.to_string()..).take_while(|(date, _)| date.starts_with(prefix)) {
            total.add(counts);
        }
        total
    }

    fn day_mut(&mut self, day: &str) -> &mut Day {
        if self.since.is_empty() || day < self.since.as_str() {
            self.since = day.to_string();
        }
        self.days.entry(day.to_string()).or_default()
    }

    fn starting(day: &str) -> Activity {
        Activity { since: day.to_string(), ..Activity::default() }
    }
}

/// Count a session started now: a new one, not one restored at launch or moved from the server.
pub fn record_session() {
    record(|activity| activity.add_session(&today()));
}

/// Count a pull request merged at `merged_at` (Unix time), once for its `key` (`owner/name#12`).
pub fn record_merge(merged_at: i64, lines: u32, key: &str) {
    let day = crate::day::iso(crate::day::local(merged_at));
    record(|activity| activity.add_merge(&day, lines, key));
}

/// Read, change and save the file. A file that cannot be read now is left as it is: an empty
/// history saved over it would lose its days.
fn record(change: impl FnOnce(&mut Activity)) {
    let Some(mut activity) = read() else { return };
    change(&mut activity);
    if let Err(err) = activity.save() {
        log::error!("could not save {}: {err}", file().display());
    }
}

/// The file as it is; with none, an empty history from today; with a broken one, the same after
/// a copy as `activity.json.broken`. `None` when it cannot be read now or the copy fails.
fn read() -> Option<Activity> {
    let path = file();
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(activity) => Some(activity),
            Err(err) => {
                log::error!("activity.json is unreadable ({err}); keeping a copy and starting empty");
                match std::fs::copy(&path, path.with_extension("json.broken")) {
                    Ok(_) => Some(Activity::starting(&today())),
                    Err(err) => {
                        log::error!("could not keep a copy of the broken activity.json ({err}); it stays as it is");
                        None
                    }
                }
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => Some(Activity::starting(&today())),
        Err(err) => {
            log::error!("could not read {} ({err})", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Glasscode's folder in a new temporary folder until dropped: a test leaves the person's own
    /// `~/.glasscode` alone.
    struct TemporaryRoot {
        dir: tempfile::TempDir,
        before: Option<std::ffi::OsString>,
    }

    impl TemporaryRoot {
        fn new() -> TemporaryRoot {
            let dir = tempfile::tempdir().unwrap();
            let before = std::env::var_os("GLASSCODE_HOME");
            // SAFETY: the core tests run one at a time (`--test-threads=1`).
            unsafe { std::env::set_var("GLASSCODE_HOME", dir.path()) };
            TemporaryRoot { dir, before }
        }
    }

    impl Drop for TemporaryRoot {
        fn drop(&mut self) {
            // SAFETY: as in `new`.
            match self.before.take() {
                Some(value) => unsafe { std::env::set_var("GLASSCODE_HOME", value) },
                None => unsafe { std::env::remove_var("GLASSCODE_HOME") },
            }
        }
    }

    #[test]
    fn a_merge_counts_once_for_its_key() {
        let mut activity = Activity::default();
        activity.add_merge("2026-11-16", 120, "acme/app#7");
        // Seen again the next day (another session on the same branch, or after a restart).
        activity.add_merge("2026-11-17", 120, "acme/app#7");
        activity.add_merge("2026-11-17", 30, "acme/app#8");
        activity.add_session("2026-11-17");
        assert_eq!(activity.days["2026-11-16"], Day { sessions: 0, prs_merged: 1, lines: 120 });
        assert_eq!(activity.days["2026-11-17"], Day { sessions: 1, prs_merged: 1, lines: 30 });
        assert_eq!(activity.since, "2026-11-16");
        // An earlier day moves the start back.
        activity.add_session("2026-11-10");
        assert_eq!(activity.since, "2026-11-10");
    }

    #[test]
    fn weeks_have_seven_days_from_monday_and_zero_without_data() {
        assert_eq!(crate::day::weekday(crate::day::parse_iso("2026-11-16").unwrap()), 0, "2026-11-16 is a Monday");
        let mut activity = Activity::default();
        for (day, sessions) in [("2026-11-01", 4), ("2026-11-02", 1), ("2026-11-15", 2), ("2026-11-18", 3), ("2026-11-19", 5)] {
            for _ in 0..sessions {
                activity.add_session(day);
            }
        }
        // Today is Wednesday 2026-11-18: three weeks from Monday 2026-11-02. The Sunday before
        // (11-01) is outside, and Thursday (11-19) is after today.
        let weeks = activity.weeks("2026-11-18", 3);
        assert_eq!(weeks, vec![[1, 0, 0, 0, 0, 0, 0], [0, 0, 0, 0, 0, 0, 2], [0, 0, 3, 0, 0, 0, 0]]);
        assert!(Activity::default().weeks("2026-11-18", 26).iter().all(|week| *week == [0; 7]));
        assert_eq!(Activity::default().weeks("2026-11-18", 26).len(), 26);
        assert!(activity.weeks("2026-11-18", 0).is_empty());
        assert_eq!(activity.weeks("not a day", 2), vec![[0; 7]; 2]);
    }

    #[test]
    fn month_sums_this_month_and_the_one_before_at_a_month_change() {
        let mut activity = Activity::default();
        activity.add_session("2026-11-30");
        activity.add_merge("2026-11-30", 40, "acme/app#1");
        activity.add_session("2026-12-01");
        activity.add_session("2026-12-31");
        activity.add_merge("2026-12-31", 2, "acme/app#2");
        activity.add_session("2027-01-01");
        activity.add_merge("2027-01-01", 5, "acme/app#3");
        // The first day of a month: the month before is the full one.
        assert_eq!(activity.month("2026-12-01"), (Day { sessions: 2, prs_merged: 1, lines: 2 }, Day { sessions: 1, prs_merged: 1, lines: 40 }));
        // January: the month before is December of the year before.
        assert_eq!(activity.month("2027-01-01"), (Day { sessions: 1, prs_merged: 1, lines: 5 }, Day { sessions: 2, prs_merged: 1, lines: 2 }));
        assert_eq!(activity.month("2027-03-15"), (Day::default(), Day::default()));
    }

    #[test]
    fn a_broken_file_gives_an_empty_history_and_keeps_a_copy() {
        let _root = TemporaryRoot::new();
        crate::paths::ensure_dirs().unwrap();
        std::fs::write(file(), b"{ \"since\": \"2026-11-16\", \"days\": { ").unwrap();
        let activity = Activity::load();
        assert!(activity.days.is_empty());
        assert_eq!(activity.since, today());
        assert_eq!(std::fs::read(file().with_extension("json.broken")).unwrap(), b"{ \"since\": \"2026-11-16\", \"days\": { ");
        // The next count writes a good file; the copy stays.
        record_session();
        assert_eq!(Activity::load().days[&today()].sessions, 1);
        assert!(file().with_extension("json.broken").is_file());
    }

    #[test]
    fn the_agents_history_fills_only_the_days_before_the_record_starts() {
        let mut activity = Activity::starting("2026-11-16");
        activity.add_session("2026-11-16");
        activity.add_session("2026-11-18");
        let history = BTreeMap::from([("2026-09-01".to_string(), 4), ("2026-11-15".to_string(), 2), ("2026-11-16".to_string(), 9), ("2026-11-17".to_string(), 5), ("2026-10-01".to_string(), 0)]);
        let merged = activity.with_history(&history);
        assert_eq!(merged.since, "2026-09-01");
        assert_eq!(merged.days["2026-09-01"].sessions, 4);
        assert_eq!(merged.days["2026-11-15"].sessions, 2);
        // From the first day Glasscode counted, its own count stands (a session it started is in
        // the agents' files too).
        assert_eq!(merged.days["2026-11-16"].sessions, 1);
        assert!(!merged.days.contains_key("2026-11-17") && !merged.days.contains_key("2026-10-01"));
        // The record itself is as it was.
        assert_eq!((activity.since.as_str(), activity.days.len()), ("2026-11-16", 2));
        assert_eq!(activity.with_history(&BTreeMap::new()), activity);
    }

    #[test]
    fn saves_in_place_and_reads_back() {
        let root = TemporaryRoot::new();
        // No file yet: an empty history from today.
        let empty = Activity::load();
        assert_eq!((empty.since.as_str(), empty.days.len()), (today().as_str(), 0));
        record_session();
        record_session();
        record_merge(crate::model::now() as i64, 12, "acme/app#4");
        record_merge(crate::model::now() as i64, 12, "acme/app#4");
        let activity = Activity::load();
        assert_eq!(activity.days[&today()], Day { sessions: 2, prs_merged: 1, lines: 12 });
        assert!(activity.merged.contains("acme/app#4"));
        // The file of its own is gone after the rename.
        let left: Vec<_> = std::fs::read_dir(root.dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".tmp")).collect();
        assert!(left.is_empty(), "{left:?}");
        // An older file without the key list reads.
        std::fs::write(file(), br#"{ "since": "2026-11-16", "days": { "2026-11-16": { "sessions": 3 } } }"#).unwrap();
        let old = Activity::load();
        assert_eq!((old.days["2026-11-16"], old.merged.len()), (Day { sessions: 3, prs_merged: 0, lines: 0 }, 0));
    }
}
