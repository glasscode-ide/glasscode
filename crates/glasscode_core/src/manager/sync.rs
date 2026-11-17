//! Keeping the Mac's and the server's copy of the manager in step, and saying which of the
//! two runs it.
//!
//! The Mac calls `manager.sync` every few seconds while the manager's home is the server. It
//! sends its settings, what it sees of its own sessions, the questions the person asked, and
//! its data; the server merges the data, answers with its own, and hands back what the Mac
//! has to do (type into a session there, show something, say something) and the answers that
//! are ready. Nothing is lost when a call fails on the way there or back: the Mac sends a
//! question again until its answer arrives, and the server sends an action again until the
//! Mac names it in `acked`.
//!
//! The data itself only travels when the two copies differ (`fingerprint`): an empty `Data`
//! merges as nothing.

use serde::{Deserialize, Serialize};

use super::{Action, ChatLine, Data, Home, Settings, Snapshot, Who};

/// What the Mac sends the server with each sync (`manager.sync`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncRequest {
    pub settings: Option<Settings>,
    /// The Mac's data, or empty when the server already has the same (`fingerprint`).
    pub data: Data,
    /// The Mac's sessions as the manager sees them (the server adds its own).
    pub snapshot: Option<Snapshot>,
    /// Questions the person asked on the Mac for the server's manager to answer. Each is sent
    /// again with every sync until its answer came back.
    pub asks: Vec<Ask>,
    /// Numbers of the actions the Mac has (from `SyncReply::action_ids`): the server stops
    /// sending them.
    pub acked: Vec<u64>,
    /// Ids of the questions whose answer the Mac has.
    pub answered: Vec<String>,
    /// `fingerprint` of the Mac's data (0: not said).
    pub fingerprint: u64,
    /// The projects the manager does not manage, by name and by folder name: project ids
    /// differ between the Mac and the server.
    pub off_projects: Vec<String>,
    /// Settings › Automation on the Mac says nothing may ever type into a session.
    pub never_tell: bool,
    /// The Mac's clock, in seconds east of UTC (`utc_offset`), so times of day in the settings
    /// ("09:00") mean the person's time on a server that keeps another.
    pub utc_offset: Option<i32>,
}

/// What comes back.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncReply {
    /// The server's data after merging, or empty when the Mac already has the same.
    pub data: Data,
    /// Things for the Mac to do (type into a session there, show something, notify).
    pub actions: Vec<Action>,
    pub answers: Vec<Answered>,
    /// The server's manager is running (its last round, unix seconds on the server's clock;
    /// 0 when it does not run there).
    pub heartbeat: u64,
    /// The number of each action in `actions`, in the same order, to send back in `acked`.
    /// The same action can arrive twice (a lost reply): do each number once.
    pub action_ids: Vec<u64>,
    /// The server's clock when it answered, so the Mac can tell the heartbeat's age without
    /// trusting that both clocks agree.
    pub now: u64,
    /// `fingerprint` of the server's data after merging.
    pub fingerprint: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ask {
    pub id: String,
    pub text: String,
    pub voice: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Answered {
    pub id: String,
    pub say: String,
    /// It could not answer: `say` is why.
    pub failed: bool,
}

/// One line of the server test in Settings › Manager.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TestLine {
    pub what: String,
    pub ok: bool,
    pub detail: String,
}

/// Which copy runs the manager right now: the server when it is the home and its heartbeat
/// is fresh (within `fresh_secs`), else the Mac.
pub fn runs_on_server(settings: &Settings, server_heartbeat: u64, now: u64, fresh_secs: u64) -> bool {
    settings.enabled && settings.home == Home::Server && server_heartbeat > 0 && now.saturating_sub(server_heartbeat) <= fresh_secs
}

/// The server's heartbeat on this machine's clock (0 when the manager does not run there).
/// The server says how old its last round was by its own clock; that age is taken off `now`.
pub fn heartbeat_here(reply: &SyncReply, now: u64) -> u64 {
    if reply.heartbeat == 0 {
        0
    } else if reply.now == 0 {
        reply.heartbeat
    } else {
        now.saturating_sub(reply.now.saturating_sub(reply.heartbeat))
    }
}

/// Take into the server's data what the Mac sent. Returns whether the server's data changed.
pub fn merge_request(server: &mut Data, request: &SyncRequest) -> bool {
    super::store::merge(server, &request.data)
}

/// A number that is the same for two copies holding the same things, in whatever order, and
/// never 0. The heartbeat is left out: it moves with every round.
pub fn fingerprint(data: &Data) -> u64 {
    let mut data = data.clone();
    data.heartbeat = 0;
    data.followups.sort_by(|a, b| a.id.cmp(&b.id));
    data.outbox.sort_by(|a, b| a.id.cmp(&b.id));
    data.inbox.sort_by(|a, b| a.id.cmp(&b.id));
    data.notes.sort_by(|a, b| a.id.cmp(&b.id));
    data.checks.sort_by(|a, b| a.id.cmp(&b.id));
    data.chat.sort_by(|a, b| (a.at, &a.text).cmp(&(b.at, &b.text)));
    // FNV-1a: the same on the Mac and on the server, whatever built them.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in serde_json::to_vec(&data).unwrap_or_default() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash.max(1)
}

/// This machine's local time, in seconds east of UTC, at a Unix time.
#[cfg(unix)]
pub fn utc_offset(now: u64) -> i32 {
    // SAFETY: localtime_r only writes into the struct we give it.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let t = now as libc::time_t;
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i32
    }
}

/// This machine's local time, in seconds east of UTC, at a Unix time. Windows has no
/// `tm_gmtoff`: the local clock (`sys::local_time`) read as if it were UTC, less the time.
#[cfg(not(unix))]
pub fn utc_offset(now: u64) -> i32 {
    let Some(t) = crate::sys::local_time(now as i64) else {
        return 0;
    };
    let local = crate::day::from_civil(i64::from(t.year), t.month, t.day) * 86_400 + i64::from(t.hour) * 3600 + i64::from(t.minute) * 60;
    // The local clock has no seconds: compare it with the start of the same minute.
    (local - (now as i64).div_euclid(60) * 60) as i32
}

/// A time of day ("09:00") moved by some seconds, around the clock. None when it is not a
/// time of day.
pub fn shift_clock(clock: &str, by_secs: i64) -> Option<String> {
    let (h, m) = clock.trim().split_once(':').unwrap_or((clock.trim(), "0"));
    let (h, m) = (h.trim().parse::<i64>().ok()?, m.trim().parse::<i64>().ok()?);
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    let minutes = (h * 60 + m + by_secs.div_euclid(60)).rem_euclid(24 * 60);
    Some(format!("{:02}:{:02}", minutes / 60, minutes % 60))
}

/// The settings as they hold on a machine whose clock is `here` seconds east of UTC, when
/// the person's is `theirs`: the daily brief keeps its moment (09:00 in Berlin is 07:00 on a
/// server in UTC). The day a brief is filed under is the server's; that only differs for a
/// brief whose time falls on the other side of midnight there.
pub fn settings_here(settings: &Settings, theirs: Option<i32>, here: i32) -> Settings {
    let mut settings = settings.clone();
    if let (Some(theirs), Some(at)) = (theirs, settings.brief_at.as_deref()) {
        if let Some(shifted) = shift_clock(at, i64::from(here) - i64::from(theirs)) {
            settings.brief_at = Some(shifted);
        }
    }
    settings
}

// ----- what waits for the Mac -------------------------------------------------------------------

/// Pointing at something in the app is dropped when the Mac did not fetch it this soon.
const SHOW_KEEPS: u64 = 120;
/// Something to say becomes a line of the conversation instead.
const SAY_KEEPS: u64 = 900;
const NOTIFY_KEEPS: u64 = 12 * 3600;
const NOTIFY_MOST: usize = 10;
const QUEUE_MOST: usize = 200;

/// An action waiting for the Mac, with its number and when it was decided.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Queued {
    pub id: u64,
    pub at: u64,
    pub action: Action,
}

/// What the server's manager decided that only the Mac can do. Each action stays until the
/// Mac says it has it, so a sync that fails on the way back loses nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Queue {
    pub items: Vec<Queued>,
    next: u64,
}

impl Queue {
    /// Add an action; returns its number. Numbers start from the clock, so they do not repeat
    /// when the queue's file is lost.
    pub fn push(&mut self, action: Action, now: u64) -> u64 {
        self.next = self.next.max(now.saturating_mul(1 << 16)) + 1;
        self.items.push(Queued { id: self.next, at: now, action });
        self.next
    }

    /// The Mac has these.
    pub fn ack(&mut self, ids: &[u64]) {
        self.items.retain(|q| !ids.contains(&q.id));
    }

    /// Drop what is too old to be worth doing when the Mac comes back. What it wanted to say
    /// comes back as lines for the conversation, so the person still reads it.
    pub fn tidy(&mut self, now: u64) -> Vec<ChatLine> {
        let mut lines = Vec::new();
        let age = |q: &Queued| now.saturating_sub(q.at);
        self.items.retain(|q| match &q.action {
            Action::Show { .. } | Action::Tour { .. } | Action::Scene { .. } => age(q) <= SHOW_KEEPS,
            Action::Notify { .. } => age(q) <= NOTIFY_KEEPS,
            Action::Say { text, show } => {
                let keep = age(q) <= SAY_KEEPS;
                if !keep {
                    lines.push(ChatLine { who: Who::Manager, text: text.clone(), at: q.at, show: show.clone() });
                }
                keep
            }
            _ => true,
        });
        // A Mac that was away for days gets the last few notifications, not all of them.
        let notes = self.items.iter().filter(|q| matches!(q.action, Action::Notify { .. })).count();
        let mut extra = notes.saturating_sub(NOTIFY_MOST);
        self.items.retain(|q| {
            if extra > 0 && matches!(q.action, Action::Notify { .. }) {
                extra -= 1;
                return false;
            }
            true
        });
        if self.items.len() > QUEUE_MOST {
            let cut = self.items.len() - QUEUE_MOST;
            self.items.drain(..cut);
        }
        lines
    }

    /// The numbers and the actions, in step, for `SyncReply`.
    pub fn pending(&self) -> (Vec<u64>, Vec<Action>) {
        (self.items.iter().map(|q| q.id).collect(), self.items.iter().map(|q| q.action.clone()).collect())
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::{Note, Show};

    fn on_server() -> Settings {
        Settings { enabled: true, home: Home::Server, ..Settings::default() }
    }

    fn note(id: &str, text: &str, updated: u64) -> Note {
        Note { id: id.into(), text: text.into(), source: "you".into(), created: updated, updated, ..Note::default() }
    }

    #[test]
    fn the_server_runs_it_only_when_it_is_home_and_alive() {
        let settings = on_server();
        assert!(runs_on_server(&settings, 1_000, 1_010, 30));
        assert!(runs_on_server(&settings, 1_000, 1_030, 30));
        // Stale: the Mac takes over.
        assert!(!runs_on_server(&settings, 1_000, 1_031, 30));
        // Never heard from.
        assert!(!runs_on_server(&settings, 0, 10, 30));
        // A heartbeat a little ahead of this clock still counts.
        assert!(runs_on_server(&settings, 1_005, 1_000, 30));
        assert!(!runs_on_server(&Settings { home: Home::Mac, ..on_server() }, 1_000, 1_001, 30));
        assert!(!runs_on_server(&Settings { enabled: false, ..on_server() }, 1_000, 1_001, 30));
    }

    #[test]
    fn the_heartbeat_is_read_on_this_clock() {
        // The server's clock is an hour ahead; its last round was 7 s before it answered.
        let reply = SyncReply { heartbeat: 5_000, now: 5_007, ..SyncReply::default() };
        assert_eq!(heartbeat_here(&reply, 1_400), 1_393);
        assert!(runs_on_server(&on_server(), heartbeat_here(&reply, 1_400), 1_400, 30));
        // Not running there.
        assert_eq!(heartbeat_here(&SyncReply { heartbeat: 0, now: 5_007, ..SyncReply::default() }, 1_400), 0);
        // A server that does not say its clock.
        assert_eq!(heartbeat_here(&SyncReply { heartbeat: 1_390, ..SyncReply::default() }, 1_400), 1_390);
    }

    #[test]
    fn a_time_of_day_keeps_its_moment_on_another_clock() {
        assert_eq!(shift_clock("09:00", 0).as_deref(), Some("09:00"));
        // The person is two hours east of the server.
        assert_eq!(shift_clock("09:00", -7_200).as_deref(), Some("07:00"));
        // Half-hour zones, and around midnight both ways.
        assert_eq!(shift_clock("9", 19_800).as_deref(), Some("14:30"));
        assert_eq!(shift_clock("07:15", -36_000).as_deref(), Some("21:15"));
        assert_eq!(shift_clock("23:30", 3_600).as_deref(), Some("00:30"));
        assert_eq!(shift_clock("soon", 3_600), None);
        assert_eq!(shift_clock("25:00", 0), None);

        let mine = Settings { brief_at: Some("09:00".into()), ..on_server() };
        // Berlin in summer (+2 h), a server in UTC.
        assert_eq!(settings_here(&mine, Some(7_200), 0).brief_at.as_deref(), Some("07:00"));
        // The same clock on both, or a Mac that does not say: as it is.
        assert_eq!(settings_here(&mine, Some(7_200), 7_200), mine);
        assert_eq!(settings_here(&mine, None, 0), mine);
        // No daily brief: nothing to move.
        let none = Settings { brief_at: None, ..on_server() };
        assert_eq!(settings_here(&none, Some(7_200), 0), none);
        // Whatever this machine's clock is, it is a real offset.
        assert!(utc_offset(1_700_000_000).abs() <= 14 * 3600);
    }

    #[test]
    fn the_fingerprint_ignores_order_and_the_heartbeat() {
        let mut a = Data::default();
        a.notes = vec![note("n1", "Alex reviews on Mondays", 10), note("n2", "Sam owns billing", 20)];
        a.chat = vec![ChatLine { who: Who::You, text: "status".into(), at: 5, show: None }, ChatLine { who: Who::Manager, text: "All quiet.".into(), at: 6, show: None }];
        a.heartbeat = 100;
        let mut b = a.clone();
        b.notes.reverse();
        b.chat.reverse();
        b.heartbeat = 900;
        assert_eq!(fingerprint(&a), fingerprint(&b));
        b.notes[0].text.push('!');
        assert_ne!(fingerprint(&a), fingerprint(&b));
        assert_ne!(fingerprint(&Data::default()), 0);
    }

    #[test]
    fn a_request_brings_the_macs_news_and_an_empty_one_brings_nothing() {
        let mut server = Data::default();
        server.notes = vec![note("n1", "Alex reviews on Mondays", 10)];
        let before = server.clone();
        // Nothing sent (the copies were the same): nothing changes.
        assert!(!merge_request(&mut server, &SyncRequest::default()));
        assert_eq!(server, before);

        let mut mac = Data::default();
        mac.notes = vec![note("n1", "Alex reviews on Tuesdays", 30), note("n2", "Sam owns billing", 20)];
        let request = SyncRequest { data: mac.clone(), fingerprint: fingerprint(&mac), ..SyncRequest::default() };
        assert!(merge_request(&mut server, &request));
        assert_eq!(server.notes.len(), 2);
        assert_eq!(server.notes.iter().find(|n| n.id == "n1").map(|n| n.text.as_str()), Some("Alex reviews on Tuesdays"));
        // Both hold the same now, and saying it again changes nothing.
        assert_eq!(fingerprint(&server), fingerprint(&mac));
        assert!(!merge_request(&mut server, &request));
    }

    #[test]
    fn requests_from_an_older_app_still_read() {
        let request: SyncRequest = serde_json::from_str(r#"{"asks":[{"id":"4","text":"status"}]}"#).unwrap();
        assert_eq!(request.asks, vec![Ask { id: "4".into(), text: "status".into(), voice: false }]);
        assert!(request.acked.is_empty() && request.fingerprint == 0 && !request.never_tell);
        let reply: SyncReply = serde_json::from_str(r#"{"heartbeat":7,"answers":[{"id":"4","say":"All quiet."}]}"#).unwrap();
        assert_eq!(reply.answers, vec![Answered { id: "4".into(), say: "All quiet.".into(), failed: false }]);
        assert!(reply.action_ids.is_empty());
    }

    #[test]
    fn actions_stay_queued_until_the_mac_has_them() {
        let mut queue = Queue::default();
        let tell = queue.push(Action::TellSession { session: "s1".into(), text: "Mira approved your pull request.".into() }, 1_000);
        let say = queue.push(Action::Say { text: "Two things need you.".into(), show: None }, 1_000);
        assert_ne!(tell, say);
        // The reply was lost: the same two go out again.
        let (ids, actions) = queue.pending();
        assert_eq!(ids, vec![tell, say]);
        assert_eq!(actions.len(), 2);
        assert_eq!(queue.pending().0, ids);
        queue.ack(&[tell]);
        assert_eq!(queue.pending().0, vec![say]);
        queue.ack(&[say, 77]);
        assert!(queue.is_empty());
        // Numbers keep rising, also across a queue that lost its file.
        let later = Queue::default().push(Action::Remember { text: "x".into() }, 1_001);
        assert!(later > say);
    }

    #[test]
    fn old_things_are_not_replayed_when_the_mac_comes_back() {
        let mut queue = Queue::default();
        queue.push(Action::Show { show: Show { view: Some("today".into()), ..Show::default() } }, 1_000);
        queue.push(Action::Say { text: "Sam fixed what you asked for.".into(), show: None }, 1_000);
        queue.push(Action::TellSession { session: "s1".into(), text: "Mira approved your pull request.".into() }, 1_000);
        for i in 0..14 {
            queue.push(Action::Notify { title: format!("Note {i}"), body: String::new(), session: None }, 1_000 + i);
        }
        // Soon after: everything is still there.
        assert!(queue.clone().tidy(1_060).is_empty());
        assert_eq!(queue.clone().len(), 17);

        // An hour later: the pointer is gone, the sentence is a line of the conversation, the
        // message for the session waits, and only the last ten notifications are left.
        let lines = queue.tidy(4_700);
        assert_eq!(lines, vec![ChatLine { who: Who::Manager, text: "Sam fixed what you asked for.".into(), at: 1_000, show: None }]);
        assert!(queue.items.iter().any(|q| matches!(q.action, Action::TellSession { .. })));
        assert!(!queue.items.iter().any(|q| matches!(q.action, Action::Show { .. } | Action::Say { .. })));
        let notes: Vec<&str> = queue.items.iter().filter_map(|q| if let Action::Notify { title, .. } = &q.action { Some(title.as_str()) } else { None }).collect();
        assert_eq!(notes.len(), 10);
        assert_eq!(notes.first(), Some(&"Note 4"));
        // Half a day later the notifications are gone too.
        queue.tidy(1_000 + 13 * 3600);
        assert_eq!(queue.len(), 1);
    }
}
