//! Telling the person (M42; review ux-10, events-22): what becomes a popup, what only a line in
//! the Inbox, and what waits. Pure: the app offers each notice and does what the answer says.
//!
//! - **Matters.** Every notice has a matter key (a session, an approval, a PR, a ticket) and an
//!   evidence hash (what it says). A matter's state is new, told, seen, snoozed or closed.
//! - **Never because of silence.** While a matter is told and not acknowledged, the same evidence
//!   is never told again, whether or not the person answered; only new evidence is news. Once the
//!   person saw it, or the matter closed (its session went on working), the same words are a new
//!   occurrence. A needs-you matter that stays open is for the brief's "still waiting on you", with
//!   its age ([`Delivery::waiting`]).
//! - **Bundling.** New evidence on a matter told less than [`BUNDLE_MS`] ago, while its
//!   notification is still on screen, updates it (no new sound, no new popup); a more severe one
//!   is a popup of its own. Past [`BURST`] popups in a minute, or [`CAP_PER_HOUR`] in an hour, the
//!   rest are Inbox lines.
//! - **Snooze** holds a matter as Inbox lines until its time; opening or dismissing a
//!   notification acknowledges it ([`Delivery::seen`]).
//! - **Quiet hours** (Settings) hold all but urgent notices; what they held comes back as one
//!   digest when they end ([`Delivery::digest`]).
//! - **Severity** chooses the channel: info is an Inbox line, notice and warn a popup, urgent a
//!   popup with its sound that passes quiet hours.
//! - **Presence.** A notice about what the person is looking at right now is an Inbox line.

use std::collections::{BTreeMap, VecDeque};

use sha2::{Digest, Sha256};

pub use crate::journal::Severity;

/// New evidence on a matter within this long updates the notification on screen.
pub const BUNDLE_MS: u64 = 10 * 60_000;
/// More popups than this within a minute become Inbox lines.
pub const BURST: usize = 3;
/// At most this many popups in an hour.
pub const CAP_PER_HOUR: usize = 12;

/// One thing to tell the person.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Notice {
    /// What it is about: `session:<id>`, `approval:<n>`, `pr:<repo>#<n>`, `ticket:<id>`.
    pub matter: String,
    pub severity: Severity,
    pub title: String,
    pub body: String,
}

impl Notice {
    /// What it says, as a hash: the same words again are no news.
    pub fn evidence(&self) -> String {
        Sha256::digest(format!("{}\0{}", self.title, self.body).as_bytes()).iter().take(12).map(|b| format!("{b:02x}")).collect()
    }
}

/// Where the person is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Presence {
    /// Glasscode's window is in front.
    pub window_active: bool,
    /// What the notice is about is on screen there (that session, its PR).
    pub watching: bool,
    /// A notification of this matter is on screen now.
    pub shown: bool,
}

/// The person's rules (Settings).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rules {
    /// Quiet hours, as minutes after local midnight: from, to (past midnight when `to` is the
    /// smaller).
    pub quiet: Option<(u16, u16)>,
    /// No sound at all.
    pub silent: bool,
}

impl Rules {
    /// Whether a local time (minutes after midnight) is in the quiet hours.
    pub fn quiet_at(&self, minute: u16) -> bool {
        match self.quiet {
            None => false,
            Some((from, to)) if from == to => false,
            Some((from, to)) if from < to => (from..to).contains(&minute),
            Some((from, to)) => minute >= from || minute < to,
        }
    }
}

/// What to do with a notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// A popup; `sound`: with its sound.
    Popup { sound: bool },
    /// Replace the notification about this matter that is on screen, quietly (a bundle of
    /// `count` notices).
    Update { count: u32 },
    /// Only a line in the Inbox, and why.
    Line(Hold),
    /// Kept for the digest at the end of the quiet hours.
    Held,
    /// Said before, word for word: nothing.
    Nothing,
}

/// Why a notice is only a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    Info,
    Watching,
    Snoozed,
    Burst,
    Cap,
}

/// Where a matter is.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum State {
    Told,
    Seen,
    Snoozed { until_ms: u64 },
    Closed,
}

/// One matter, as last told.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Matter {
    pub key: String,
    pub state: State,
    pub evidence: String,
    pub title: String,
    pub first_ms: u64,
    pub told_ms: u64,
    /// Notices bundled into the one on screen.
    pub count: u32,
    pub severity: Severity,
    /// It reached the person as a notification (not only a line).
    pub shown: bool,
}

/// The matters, the popups of the last hour, and what the quiet hours hold. It is kept in
/// `manager/delivery.json` (`DeliveryFile`), so a restart forgets no snooze, no matter already told
/// and nothing the quiet hours hold for the digest.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Delivery {
    matters: BTreeMap<String, Matter>,
    popups: VecDeque<u64>,
    held: Vec<Notice>,
}

/// `manager/delivery.json`: [`Delivery`] between runs. Damaged, it starts empty: at worst a notice
/// is said again.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DeliveryFile {
    pub delivery: Delivery,
}

impl crate::store::Store for DeliveryFile {
    const NAME: &'static str = "manager/delivery";
    const VERSION: u32 = 1;
    const ON_DAMAGE: crate::store::OnDamage = crate::store::OnDamage::Empty;

    fn path() -> std::path::PathBuf {
        crate::manager::store_dir().join("delivery.json")
    }
}

/// Matters kept that long after they were last told; older ones are history.
const KEEP_MS: u64 = 7 * 24 * 3_600_000;

impl Delivery {
    /// What is worth keeping across a restart: matters of the last week (and every snooze still
    /// running), the popups of the last hour, and what the quiet hours hold.
    pub fn kept(&self, now_ms: u64) -> Delivery {
        let mut kept = self.clone();
        kept.matters.retain(|_, m| now_ms.saturating_sub(m.told_ms) < KEEP_MS || matches!(m.state, State::Snoozed { until_ms } if until_ms > now_ms));
        kept.popups.retain(|at| now_ms.saturating_sub(*at) < 3_600_000);
        kept
    }
}

impl Delivery {
    /// What to do with `notice`, at `now_ms` and local `minute` of the day.
    pub fn offer(&mut self, notice: Notice, presence: Presence, rules: &Rules, now_ms: u64, minute: u16) -> Decision {
        let evidence = notice.evidence();
        if let Some(matter) = self.matters.get(&notice.matter)
            && matter.evidence == evidence
            && matches!(matter.state, State::Told | State::Snoozed { .. })
        {
            return Decision::Nothing;
        }
        let urgent = notice.severity == Severity::Urgent;
        if rules.quiet_at(minute) && !urgent {
            if !self.held.iter().any(|held| held.matter == notice.matter && held.evidence() == evidence) {
                self.held.retain(|held| held.matter != notice.matter);
                self.held.push(notice);
            }
            return Decision::Held;
        }
        let previous = self.matters.get(&notice.matter).cloned();
        let matter = Matter {
            key: notice.matter.clone(),
            state: State::Told,
            evidence,
            title: notice.title.clone(),
            first_ms: previous.as_ref().filter(|m| m.state == State::Told).map_or(now_ms, |m| m.first_ms),
            told_ms: now_ms,
            count: 1,
            severity: notice.severity,
            shown: false,
        };
        // Snoozed: a line until its time.
        if let Some(State::Snoozed { until_ms }) = previous.as_ref().map(|m| &m.state)
            && now_ms < *until_ms
            && !urgent
        {
            self.matters.insert(notice.matter.clone(), Matter { state: State::Snoozed { until_ms: *until_ms }, ..matter });
            return Decision::Line(Hold::Snoozed);
        }
        let hold = if notice.severity == Severity::Info {
            Some(Hold::Info)
        } else if presence.window_active && presence.watching && !urgent {
            Some(Hold::Watching)
        } else {
            None
        };
        if let Some(hold) = hold {
            self.matters.insert(notice.matter.clone(), matter);
            return Decision::Line(hold);
        }
        // New evidence on a matter just told, its notification still on screen and no more
        // severe: that one is updated.
        if let Some(previous) = &previous
            && previous.state == State::Told
            && presence.shown
            && notice.severity <= previous.severity
            && now_ms.saturating_sub(previous.told_ms) < BUNDLE_MS
        {
            let count = previous.count + 1;
            self.matters.insert(notice.matter.clone(), Matter { count, told_ms: previous.told_ms, shown: true, ..matter });
            return Decision::Update { count };
        }
        while self.popups.front().is_some_and(|at| now_ms.saturating_sub(*at) >= 3_600_000) {
            self.popups.pop_front();
        }
        let last_minute = self.popups.iter().filter(|at| now_ms.saturating_sub(**at) < 60_000).count();
        if !urgent && last_minute >= BURST {
            self.matters.insert(notice.matter.clone(), matter);
            return Decision::Line(Hold::Burst);
        }
        if !urgent && self.popups.len() >= CAP_PER_HOUR {
            self.matters.insert(notice.matter.clone(), matter);
            return Decision::Line(Hold::Cap);
        }
        self.popups.push_back(now_ms);
        self.matters.insert(notice.matter.clone(), Matter { shown: true, ..matter });
        Decision::Popup { sound: !rules.silent || urgent }
    }

    /// The person opened or dismissed its notification: acknowledged.
    pub fn seen(&mut self, key: &str) {
        if let Some(matter) = self.matters.get_mut(key) {
            matter.state = State::Seen;
        }
    }

    /// [`Delivery::seen`] for every matter whose key starts with `prefix`.
    pub fn seen_all(&mut self, prefix: &str) {
        for matter in self.matters.values_mut().filter(|m| m.key.starts_with(prefix)) {
            matter.state = State::Seen;
        }
    }

    /// [`Delivery::close`] for every matter whose key starts with `prefix`.
    pub fn close_all(&mut self, prefix: &str) {
        for matter in self.matters.values_mut().filter(|m| m.key.starts_with(prefix)) {
            matter.state = State::Closed;
        }
    }

    /// Held as Inbox lines until `until_ms`.
    pub fn snooze(&mut self, key: &str, until_ms: u64) {
        if let Some(matter) = self.matters.get_mut(key) {
            matter.state = State::Snoozed { until_ms };
        }
    }

    /// [`Delivery::snooze`] for every matter whose key starts with `prefix`.
    pub fn snooze_all(&mut self, prefix: &str, until_ms: u64) {
        for matter in self.matters.values_mut().filter(|m| m.key.starts_with(prefix)) {
            matter.state = State::Snoozed { until_ms };
        }
    }

    /// Done with: the next notice about it is new, even with the same words.
    pub fn close(&mut self, key: &str) {
        if let Some(matter) = self.matters.get_mut(key) {
            matter.state = State::Closed;
        }
    }

    /// What the quiet hours held, once they are over at `minute`: one digest, each matter once.
    pub fn digest(&mut self, rules: &Rules, now_ms: u64, minute: u16) -> Option<Notice> {
        if self.held.is_empty() || rules.quiet_at(minute) {
            return None;
        }
        let held = std::mem::take(&mut self.held);
        for notice in &held {
            let evidence = notice.evidence();
            self.matters.insert(notice.matter.clone(), Matter { key: notice.matter.clone(), state: State::Told, evidence, title: notice.title.clone(), first_ms: now_ms, told_ms: now_ms, count: 1, severity: notice.severity, shown: true });
        }
        let title = if held.len() == 1 { "While it was quiet".to_string() } else { format!("While it was quiet: {} things", held.len()) };
        // Each digest is a matter of its own: the same titles on another morning are told too.
        Some(Notice { matter: format!("digest:{now_ms}"), severity: Severity::Notice, title, body: held.iter().map(|n| n.title.as_str()).collect::<Vec<_>>().join("\n") })
    }

    /// The matters that need the person (warn and above), shown to them and still open (not seen,
    /// not closed), oldest first, with their age: for the brief's "still waiting on you"
    /// (events-22).
    pub fn waiting(&self, now_ms: u64) -> Vec<(&Matter, u64)> {
        let mut out: Vec<(&Matter, u64)> = self.matters.values().filter(|m| m.shown && matches!(m.state, State::Told | State::Snoozed { .. }) && m.severity >= Severity::Warn).map(|m| (m, now_ms.saturating_sub(m.first_ms))).collect();
        out.sort_by_key(|(m, _)| m.first_ms);
        out
    }

    pub fn matter(&self, key: &str) -> Option<&Matter> {
        self.matters.get(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What was told survives a restart (the file), bar what is history: a snooze still holds, the
    /// quiet hours' notices still wait for the digest, a week-old matter is gone.
    #[test]
    fn what_was_told_is_kept_across_a_restart() {
        let rules = Rules { quiet: Some((22 * 60, 7 * 60)), silent: false };
        let mut d = Delivery::default();
        d.offer(notice("session:s1#needs", "asks", Severity::Warn), away(), &Rules::default(), NOW, NOON);
        d.snooze_all("session:s1#", NOW + 3_600_000);
        d.offer(notice("session:s2#done", "done", Severity::Notice), away(), &rules, NOW, 23 * 60);
        d.offer(notice("session:old#done", "long ago", Severity::Notice), away(), &Rules::default(), NOW - 8 * 24 * 3_600_000, NOON);
        let file = DeliveryFile { delivery: d.kept(NOW + 60_000) };
        let mut again = serde_json::from_str::<DeliveryFile>(&serde_json::to_string(&file).unwrap()).unwrap().delivery;
        assert_eq!(again.offer(notice("session:s1#needs", "asks again", Severity::Warn), away(), &Rules::default(), NOW + 120_000, NOON), Decision::Line(Hold::Snoozed));
        assert!(again.digest(&rules, NOW + 8 * 3_600_000, 8 * 60).is_some(), "the quiet hours' notice still waits");
        assert!(!again.matters.contains_key("session:old#done"));
    }

    const NOW: u64 = 1_800_000_000_000;
    const NOON: u16 = 12 * 60;

    fn notice(matter: &str, title: &str, severity: Severity) -> Notice {
        Notice { matter: matter.into(), severity, title: title.into(), body: String::new() }
    }

    fn away() -> Presence {
        Presence::default()
    }

    fn on_screen() -> Presence {
        Presence { shown: true, ..Presence::default() }
    }

    #[test]
    fn bundles_same_matter() {
        // Five events about one PR in a few minutes: one popup, the rest update it quietly.
        let mut d = Delivery::default();
        let rules = Rules::default();
        let decisions: Vec<Decision> = (0..5).map(|n| d.offer(notice("pr:acme/app#42", &format!("PR #42: event {n}"), Severity::Notice), if n == 0 { away() } else { on_screen() }, &rules, NOW + n * 20_000, NOON)).collect();
        assert_eq!(decisions[0], Decision::Popup { sound: true });
        assert_eq!(decisions[1..], [Decision::Update { count: 2 }, Decision::Update { count: 3 }, Decision::Update { count: 4 }, Decision::Update { count: 5 }]);
        // Its notification gone from the screen, new evidence is a popup again; so is a more
        // severe notice while it is on screen.
        assert!(matches!(d.offer(notice("pr:acme/app#42", "PR #42: event 6", Severity::Notice), away(), &rules, NOW + 120_000, NOON), Decision::Popup { .. }));
        assert!(matches!(d.offer(notice("pr:acme/app#42", "PR #42: conflicts", Severity::Warn), on_screen(), &rules, NOW + 130_000, NOON), Decision::Popup { .. }));
        // Past ten minutes it is a popup of its own again.
        assert_eq!(d.offer(notice("pr:acme/app#42", "PR #42: later", Severity::Notice), on_screen(), &rules, NOW + 20 * 60_000, NOON), Decision::Popup { sound: true });
        // Many matters at once: the fourth in a minute is a line.
        let mut d = Delivery::default();
        for n in 0..BURST {
            assert!(matches!(d.offer(notice(&format!("s{n}"), "x", Severity::Notice), away(), &rules, NOW, NOON), Decision::Popup { .. }));
        }
        assert_eq!(d.offer(notice("s9", "x", Severity::Notice), away(), &rules, NOW + 1, NOON), Decision::Line(Hold::Burst));
        // Urgent always reaches the person.
        assert!(matches!(d.offer(notice("prod", "prod is down", Severity::Urgent), away(), &rules, NOW + 2, NOON), Decision::Popup { .. }));
    }

    #[test]
    fn no_repeat_after_ignore() {
        let mut d = Delivery::default();
        let rules = Rules::default();
        let blocked = notice("session:s1", "Fix login needs you", Severity::Warn);
        assert!(matches!(d.offer(blocked.clone(), away(), &rules, NOW, NOON), Decision::Popup { .. }));
        // Hours of silence from the person: the same words are never said again.
        for hour in 1..6 {
            assert_eq!(d.offer(blocked.clone(), away(), &rules, NOW + hour * 3_600_000, NOON), Decision::Nothing);
        }
        // It is still waiting, for the brief, with its age.
        let waiting = d.waiting(NOW + 6 * 3_600_000);
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].1, 6 * 3_600_000);
        // Seen: no longer waiting; the same words later are a new occurrence (a new stop).
        d.seen("session:s1");
        assert!(d.waiting(NOW + 7 * 3_600_000).is_empty());
        assert!(matches!(d.offer(blocked.clone(), away(), &rules, NOW + 7 * 3_600_000 + 1, NOON), Decision::Popup { .. }));
        // A line (the person was looking at it) never waits in the brief.
        let mut lined = Delivery::default();
        lined.offer(notice("session:s7", "asks", Severity::Warn), Presence { window_active: true, watching: true, shown: false }, &rules, NOW, NOON);
        assert!(lined.waiting(NOW + 3_600_000).is_empty());
        assert_eq!(Delivery::default().offer(notice("x", "finished", Severity::Notice), away(), &rules, NOW, NOON), Decision::Popup { sound: true });
        assert!(matches!(d.offer(notice("session:s1", "Fix login asks again", Severity::Warn), away(), &rules, NOW + 8 * 3_600_000, NOON), Decision::Popup { .. }));
        // Closed: the same words are a new matter.
        d.close("session:s1");
        assert!(matches!(d.offer(notice("session:s1", "Fix login asks again", Severity::Warn), away(), &rules, NOW + 9 * 3_600_000, NOON), Decision::Popup { .. }));
        // Snoozed: lines until its time.
        d.snooze("session:s1", NOW + 12 * 3_600_000);
        assert_eq!(d.offer(notice("session:s1", "and again", Severity::Warn), away(), &rules, NOW + 10 * 3_600_000, NOON), Decision::Line(Hold::Snoozed));
        assert!(matches!(d.offer(notice("session:s1", "and once more", Severity::Warn), away(), &rules, NOW + 13 * 3_600_000, NOON), Decision::Popup { .. }));
        // A popup's snooze covers every matter it stands for.
        d.offer(notice("session:s5#needs", "asks", Severity::Warn), away(), &rules, NOW, NOON);
        d.offer(notice("session:s5#done", "done", Severity::Notice), away(), &rules, NOW + 1, NOON);
        d.snooze_all("session:s5#", NOW + 3_600_000);
        assert_eq!(d.offer(notice("session:s5#needs", "asks again", Severity::Warn), away(), &rules, NOW + 2, NOON), Decision::Line(Hold::Snoozed));
        assert_eq!(d.offer(notice("session:s5#done", "done again", Severity::Notice), away(), &rules, NOW + 3, NOON), Decision::Line(Hold::Snoozed));
        // Info and what is on screen are lines.
        assert_eq!(d.offer(notice("s2", "done", Severity::Info), away(), &rules, NOW, NOON), Decision::Line(Hold::Info));
        assert_eq!(d.offer(notice("s3", "asks", Severity::Warn), Presence { window_active: true, watching: true, shown: false }, &rules, NOW, NOON), Decision::Line(Hold::Watching));
    }

    #[test]
    fn quiet_hours_hold_non_urgent() {
        let mut d = Delivery::default();
        let rules = Rules { quiet: Some((22 * 60, 7 * 60)), silent: false };
        assert!(rules.quiet_at(23 * 60) && rules.quiet_at(60) && !rules.quiet_at(7 * 60) && !rules.quiet_at(NOON));
        assert_eq!(d.offer(notice("session:s1", "Fix login needs you", Severity::Warn), away(), &rules, NOW, 23 * 60), Decision::Held);
        assert_eq!(d.offer(notice("pr:a#1", "PR #1 reviewed", Severity::Notice), away(), &rules, NOW + 1, 23 * 60 + 5), Decision::Held);
        assert_eq!(d.offer(notice("pr:a#1", "PR #1 reviewed", Severity::Notice), away(), &rules, NOW + 2, 23 * 60 + 6), Decision::Held, "held once");
        assert!(matches!(d.offer(notice("prod", "prod is down", Severity::Urgent), away(), &rules, NOW + 3, 2 * 60), Decision::Popup { sound: true }));
        assert_eq!(d.digest(&rules, NOW + 4, 3 * 60), None, "still quiet");
        let digest = d.digest(&rules, NOW + 5, 7 * 60).unwrap();
        assert_eq!(digest.title, "While it was quiet: 2 things");
        assert_eq!(digest.body, "Fix login needs you\nPR #1 reviewed");
        assert_eq!(d.digest(&rules, NOW + 6, 8 * 60), None, "one digest");
        assert!(digest.matter.starts_with("digest:"), "each digest is its own matter");
        // What the digest told is not told again.
        assert_eq!(d.offer(notice("pr:a#1", "PR #1 reviewed", Severity::Notice), away(), &rules, NOW + 7, 8 * 60), Decision::Nothing);
        // A silent person gets no sound but for urgent.
        let silent = Rules { quiet: None, silent: true };
        assert_eq!(Delivery::default().offer(notice("x", "y", Severity::Warn), away(), &silent, NOW, NOON), Decision::Popup { sound: false });
    }
}
