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

use super::{Action, Data, Home, Settings, Snapshot};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::{ChatLine, Note, Who};

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
    fn requests_from_an_older_app_still_read() {
        let request: SyncRequest = serde_json::from_str(r#"{"asks":[{"id":"4","text":"status"}]}"#).unwrap();
        assert_eq!(request.asks, vec![Ask { id: "4".into(), text: "status".into(), voice: false }]);
        assert!(request.acked.is_empty() && request.fingerprint == 0 && !request.never_tell);
        let reply: SyncReply = serde_json::from_str(r#"{"heartbeat":7,"answers":[{"id":"4","say":"All quiet."}]}"#).unwrap();
        assert_eq!(reply.answers, vec![Answered { id: "4".into(), say: "All quiet.".into(), failed: false }]);
        assert!(reply.action_ids.is_empty());
    }


}
