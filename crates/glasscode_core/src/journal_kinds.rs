//! The catalogue of journal events (design D24, events-19): every kind an event can be, by
//! family, with its default severity, the file that writes it and what reads it. A kind is a
//! dotted name (`session.started`); [`EventKind`] keeps it as text, so an event a newer build
//! wrote reads as a kind this build does not know instead of a damaged line.
//!
//! A test checks that every kind of the catalogue has a constant here and that every kind whose
//! producer is a file is written in that file; a kind still to come names its slice
//! (`planned:M30`).

use std::borrow::Cow;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How much an event matters, for the reader: an urgent one is synced to disk at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Info,
    Notice,
    Warn,
    Urgent,
}

/// What an event is: a family and a name, `session.started`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventKind(Cow<'static, str>);

impl EventKind {
    /// A kind of the catalogue (the constants below).
    pub const fn of(name: &'static str) -> EventKind {
        EventKind(Cow::Borrowed(name))
    }

    /// A kind by its name, as read: also one this build does not know.
    pub fn named(name: &str) -> EventKind {
        match CATALOGUE.iter().find(|info| info.kind == name) {
            Some(info) => EventKind(Cow::Borrowed(info.kind)),
            None => EventKind(Cow::Owned(name.to_string())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `session` of `session.started`.
    pub fn family(&self) -> &str {
        self.0.split('.').next().unwrap_or_default()
    }

    /// Its row of the catalogue; None for a kind this build does not know.
    pub fn info(&self) -> Option<&'static KindInfo> {
        CATALOGUE.iter().find(|info| info.kind == self.as_str())
    }

    /// Whether `filter` names it: its whole name, its family (`session`), or a prefix ending in a
    /// dot (`reconciled.`).
    pub fn matches(&self, filter: &str) -> bool {
        let name = self.as_str();
        name == filter || (filter.ends_with('.') && name.starts_with(filter)) || self.family() == filter
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for EventKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for EventKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<EventKind, D::Error> {
        let name = String::deserialize(deserializer)?;
        if name.is_empty() || name.len() > 80 {
            return Err(serde::de::Error::custom("not an event kind"));
        }
        Ok(EventKind::named(&name))
    }
}

/// One row of the catalogue.
#[derive(Clone, Copy, Debug)]
pub struct KindInfo {
    pub kind: &'static str,
    pub severity: Severity,
    /// The file that writes it (from the repository's root), or `planned:<slice>`.
    pub producer: &'static str,
    /// What its `data` holds.
    pub data: &'static str,
    /// What reads it (a rule, a playbook, the reconciler), or "Activity" when only people do.
    pub consumer: &'static str,
}

macro_rules! kinds {
    ($($name:ident = $kind:literal, $severity:ident, $producer:literal, $data:literal, $consumer:literal;)*) => {
        $(pub const $name: EventKind = EventKind::of($kind);)*
        /// Every kind, by family.
        pub const CATALOGUE: &[KindInfo] = &[$(KindInfo { kind: $kind, severity: Severity::$severity, producer: $producer, data: $data, consumer: $consumer }),*];
        /// The name of each kind's constant, for the coverage test.
        #[cfg(test)]
        pub(crate) const CONSTANTS: &[(&str, &str)] = &[$((stringify!($name), $kind)),*];
    };
}

const APP: &str = "crates/glasscode/src/journal_ui.rs";
const DAEMON: &str = "crates/glasscoded/src/daemon.rs";
const CORE: &str = "crates/glasscode_core/src/journal.rs";
const BRAIN: &str = "crates/glasscode_core/src/runtime/health.rs";
const RECONCILE: &str = "crates/glasscode_core/src/reconcile.rs";
const TURNS: &str = "crates/glasscode_core/src/turns.rs";
const LANES: &str = "crates/glasscode_core/src/runtime/lanes.rs";
const CHAT: &str = "crates/glasscode_core/src/runtime/chat.rs";
const WORK: &str = "crates/glasscode_core/src/work.rs";
const IDENTITY: &str = "crates/glasscode_core/src/identity.rs";

kinds! {
    // ----- sessions: from their hooks, the app and glasscoded
    SESSION_STARTED = "session.started", Info, "crates/glasscode_core/src/journal.rs", "agent, hook source (startup, resume, clear)", "reconciler";
    SESSION_ACTIVITY = "session.activity", Info, "crates/glasscode_core/src/journal.rs", "activity (working, idle, needs_input), previous", "reconciler, Activity";
    SESSION_NEEDS_INPUT = "session.needs_input", Notice, "crates/glasscode_core/src/journal.rs", "notification_type (permission_prompt, idle_prompt, …), message", "Activity; M40's needs-you rule";
    SESSION_ENDED = "session.ended", Info, "crates/glasscode_core/src/journal.rs", "reason (clear, resume, logout, prompt_input_exit, other), or exit", "reconciler";
    SESSION_HOOK = "session.hook", Info, "crates/glasscode_core/src/journal.rs", "the hook's name and its fields, prompts hashed with an excerpt", "M30 (turns)";
    // ----- turns: M30
    TURN_STARTED = "turn.started", Info, "crates/glasscode_core/src/turns.rs", "turn id, prompt hash and excerpt, the head it started at, whether an agent sent it", "M32, the reconciler";
    TURN_ENDED = "turn.ended", Info, "crates/glasscode_core/src/turns.rs", "the turn report (end cause, tools, commands, files, commits, reply excerpt, question)", "M32, M40, Activity";
    SESSION_REPORTED = "session.reported", Info, "crates/glasscode_core/src/turns.rs", "state (done, blocked, needs-review), text, signed; a hint, never evidence", "M32";
    SESSION_OUTSIDE = "session.outside", Info, "crates/glasscode_core/src/turns.rs", "agent, conversation, folder, title", "Activity";
    // ----- git: from the checkouts' files
    GIT_COMMIT = "git.commit", Info, "crates/glasscode_core/src/journal.rs", "worktree, branch, head, before", "M32, Activity";
    GIT_PUSH = "git.push", Info, "crates/glasscode_core/src/journal.rs", "worktree, branch, head", "M32, Activity";
    GIT_BRANCH = "git.branch", Info, "crates/glasscode_core/src/journal.rs", "worktree, branch, before", "M32, Activity";
    // ----- the forge: from the status rounds, with the poll's age
    PR_OPENED = "pr.opened", Notice, "crates/glasscode_core/src/journal.rs", "repo, number, title, draft", "M32";
    PR_CHANGED = "pr.changed", Info, "crates/glasscode_core/src/journal.rs", "repo, number, state, draft, before", "M32";
    PR_MERGED = "pr.merged", Notice, "crates/glasscode_core/src/journal.rs", "repo, number", "M32, M40";
    PR_CLOSED = "pr.closed", Notice, "crates/glasscode_core/src/journal.rs", "repo, number", "M32";
    CI_RUNNING = "ci.running", Info, "crates/glasscode_core/src/journal.rs", "repo, number, head", "M40 (CI fix)";
    CI_PASSED = "ci.passed", Info, "crates/glasscode_core/src/journal.rs", "repo, number, head", "M32 (CI green)";
    CI_FAILED = "ci.failed", Warn, "crates/glasscode_core/src/journal.rs", "repo, number, head, failed checks", "M40 (CI fix)";
    PR_CONFLICT = "pr.conflict", Warn, "crates/glasscode_core/src/journal.rs", "repo, number, mergeable", "M40";
    REVIEW_REQUESTED = "review.requested", Notice, "crates/glasscode_core/src/work.rs", "repo, number", "M32";
    REVIEW_RECEIVED = "review.received", Notice, "crates/glasscode_core/src/journal.rs", "repo, number, reviewer, association, state (v1's reader of the status rounds: once per reviewer state change, never in a start's first round)", "M40's review-tell";
    REVIEW_POSTED = "review.posted", Notice, "crates/glasscode_core/src/work.rs", "repo, number, the review session", "M32 (ReviewPosted)";
    // ----- the work ledger (M32)
    WORK_CHANGED = "work.changed", Info, "crates/glasscode_core/src/work.rs", "work key, state and the one before, its done conditions", "Today (Needs you), M40";
    WORK_CLAIM_UNVERIFIED = "work.claim_unverified", Notice, "crates/glasscode_core/src/work.rs", "work key, the conditions not yet met", "M51 (a wake for the disagreement)";
    // ----- what was asked and done: every reply of an op, with its caller
    OP_DONE = "op.done", Info, "crates/glasscode/src/journal_ui.rs", "the action (op, args, targets, caller, decision, code) and its log line", "Activity, M31";
    OP_FAILED = "op.failed", Notice, "crates/glasscode/src/journal_ui.rs", "as op.done", "Activity";
    APPROVAL_ASKED = "approval.asked", Notice, "crates/glasscode/src/journal_ui.rs", "approval id, what, caller", "Activity";
    APPROVAL_ALLOWED = "approval.allowed", Info, "crates/glasscode/src/journal_ui.rs", "approval id, what, caller", "Activity";
    APPROVAL_DENIED = "approval.denied", Info, "crates/glasscode/src/journal_ui.rs", "approval id, what, caller", "Activity";
    // ----- messages, tickets, usage
    MESSAGE_SENT = "message.sent", Notice, "crates/glasscode/src/journal_ui.rs", "message id, to, channel", "Activity";
    MESSAGE_FAILED = "message.failed", Warn, "crates/glasscode/src/journal_ui.rs", "message id, why", "Activity";
    TICKET_CHANGED = "ticket.changed", Info, "planned:M33.2", "ticket, state, before", "M32";
    USAGE_THRESHOLD = "usage.threshold", Warn, "crates/glasscode_core/src/journal.rs", "account, window, percent", "M41";
    // ----- the app and the machine
    APP_STARTED = "app.started", Info, "crates/glasscode/src/journal_ui.rs", "version, build", "reconciler";
    APP_QUIT = "app.quit", Info, "crates/glasscode/src/journal_ui.rs", "running sessions", "M30 (AppQuit turns), the reconciler";
    APP_SLEPT = "app.slept", Info, "crates/glasscode_core/src/journal.rs", "from, to (wall clock ms)", "M30 (Slept turns), reconciler";
    APP_WOKE = "app.woke", Info, "crates/glasscode_core/src/journal.rs", "asleep for (ms)", "reconciler";
    // ----- the manager's brain: M50
    MANAGER_BRAIN_DOWN = "brain.down", Warn, "crates/glasscode_core/src/runtime/health.rs", "agent, lane, health (state and its words)", "the chat, Activity";
    MANAGER_BRAIN_UP = "brain.up", Info, "crates/glasscode_core/src/runtime/health.rs", "agent, lane, health", "the chat";
    // ----- the manager's wakes: M51
    CHAT_TURN = "chat.turn", Info, "crates/glasscode_core/src/runtime/chat.rs", "first text (ms after the message), whole turn (ms), how it ended, the brain", "the time budget (ux-m3)";
    WAKE_DROPPED = "wake.dropped", Info, "crates/glasscode_core/src/runtime/lanes.rs", "key, why (stale, past its deadline, cancelled), events", "Activity";
    LINK_LOST = "link.lost", Warn, "crates/glasscode_core/src/journal.rs", "the server, since", "M58";
    LINK_BACK = "link.back", Info, "crates/glasscode_core/src/journal.rs", "the server, after (ms)", "M58";
    CLOCK_JUMPED = "clock.jumped", Warn, "crates/glasscode_core/src/journal.rs", "by (ms)", "M41";
    CLOCK_ZONE = "clock.zone", Notice, "crates/glasscode_core/src/journal.rs", "offset before and after (minutes)", "M41";
    // ----- the reconciler (D24's safety net): what no event explained
    RECONCILED_BASELINE = "reconciled.baseline", Info, "crates/glasscode_core/src/reconcile.rs", "sessions and worktrees first seen", "Activity";
    RECONCILED_MISSED_STOP = "reconciled.missed_stop", Notice, "crates/glasscode_core/src/reconcile.rs", "activity the journal implied, and now", "M30";
    RECONCILED_ACTIVITY = "reconciled.activity", Info, "crates/glasscode_core/src/reconcile.rs", "activity the journal implied, and now", "M30";
    RECONCILED_SESSION_GONE = "reconciled.session_gone", Notice, "crates/glasscode_core/src/reconcile.rs", "the session's agent is not running", "M30";
    RECONCILED_SESSION_RUNNING = "reconciled.session_running", Info, "crates/glasscode_core/src/reconcile.rs", "running with no start in the journal", "M30";
    RECONCILED_PR = "reconciled.pr", Info, "crates/glasscode_core/src/reconcile.rs", "pull request the journal implied, and now", "M32";
    RECONCILED_WORKTREE_ADDED = "reconciled.worktree_added", Info, "crates/glasscode_core/src/reconcile.rs", "worktree", "M24";
    RECONCILED_WORKTREE_GONE = "reconciled.worktree_gone", Info, "crates/glasscode_core/src/reconcile.rs", "worktree", "M24 (archive)";
    // ----- the sources themselves (events-18, events-20)
    SOURCE_DEGRADED = "source.degraded", Warn, "crates/glasscode_core/src/journal.rs", "source, why (unreadable hook lines, lost events, failed lookups)", "Settings, the reconciler";
    SOURCE_RECOVERED = "source.recovered", Info, "crates/glasscode_core/src/journal.rs", "source", "Settings";
    // ----- the trace of a decision (exec-31)
    DECISION = "decision.made", Info, "crates/glasscode_core/src/journal.rs", "step, outcome, reason, inputs", "`journal why` (M33.2)";
    // ----- the server's events, pulled by the Mac (journal.since)
    SERVER_PULLED = "server.pulled", Info, "crates/glasscode/src/journal_ui.rs", "count, cursor", "Activity";
    // ----- the sync's stamps (crit-9)
    SYNC_QUARANTINED = "sync.quarantined", Warn, "crates/glasscode_core/src/identity.rs", "store, count, machine", "Activity";
}

/// The files that write the kinds of the catalogue, for the reader of the table.
pub const PRODUCERS: &[&str] = &[APP, DAEMON, CORE, RECONCILE, TURNS, WORK, BRAIN, LANES, CHAT, IDENTITY];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_producer() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut names = std::collections::BTreeSet::new();
        for info in CATALOGUE {
            assert!(names.insert(info.kind), "{} twice", info.kind);
            let parts: Vec<&str> = info.kind.split('.').collect();
            assert!(parts.len() == 2 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')), "{}", info.kind);
            let (constant, _) = CONSTANTS.iter().find(|(_, kind)| *kind == info.kind).unwrap();
            if let Some(slice) = info.producer.strip_prefix("planned:") {
                assert!(slice.starts_with('M'), "{}: {}", info.kind, info.producer);
                continue;
            }
            assert!(PRODUCERS.contains(&info.producer), "{}: {} is not a producer file", info.kind, info.producer);
            let source = std::fs::read_to_string(root.join(info.producer)).unwrap_or_else(|err| panic!("{}: {err}", info.producer));
            // The kind's constant is written there (beyond a `use` line).
            let written = source.lines().filter(|line| !line.trim_start().starts_with("use ") && !line.trim_start().starts_with("//")).any(|line| line.contains(constant));
            assert!(written, "{} ({constant}) is never written by {}", info.kind, info.producer);
        }
    }

    #[test]
    fn a_kind_reads_as_its_name_and_a_new_one_too() {
        assert_eq!(serde_json::to_string(&SESSION_STARTED).unwrap(), "\"session.started\"");
        assert_eq!(serde_json::from_str::<EventKind>("\"session.started\"").unwrap(), SESSION_STARTED);
        let newer: EventKind = serde_json::from_str("\"playbook.ran\"").unwrap();
        assert_eq!((newer.as_str(), newer.family(), newer.info().is_none()), ("playbook.ran", "playbook", true));
        assert!(serde_json::from_str::<EventKind>("\"\"").is_err());
        assert!(RECONCILED_MISSED_STOP.matches("reconciled") && RECONCILED_MISSED_STOP.matches("reconciled.") && RECONCILED_MISSED_STOP.matches("reconciled.missed_stop"));
        assert!(!RECONCILED_MISSED_STOP.matches("reconciled.missed") && !SESSION_STARTED.matches("sess"));
    }
}
