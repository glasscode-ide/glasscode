//! The wake queue (M51 step 4; events-15): what the wake lane runs next. The chat lane is never in
//! it: the person's messages go to the chat lane at once, and while it answers the wake lane holds
//! (`lanes::Hold`). In the queue, urgent events come first, then playbook wakes, then digests; a
//! wake that has waited gains a level every ten minutes, so nothing starves.
//!
//! - **Coalescing.** Wakes with one key (their subject) are one: their events are merged. A digest
//!   is ready after 20 s with no new event, at most 120 s after its first, or at 50 events.
//! - **Bounds.** At most [`BOUND`] wakes; past that, the oldest digests collapse into one that
//!   counts them.
//! - **Staleness.** Before a wake runs, its precondition is read again (CI still red on the same
//!   head); one that no longer holds is dropped with a line for the journal.
//! - **Cancellation.** A wake can be cancelled by its key; a running one is the lane's to stop.
//! - **Deadlines.** A wake past its deadline is dropped; one that runs has [`WAKE_TIMEOUT`].

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How urgent a wake is, highest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Digest = 0,
    Playbook = 1,
    Urgent = 2,
}

/// What must still be true for a wake to be worth running (read again when it is taken).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Precondition {
    None,
    /// CI is still red on this head of this pull request.
    CiRed { repo: String, number: u64, head: String },
    /// The session still needs the person (it has not been answered meanwhile).
    SessionNeedsYou { session: String },
    /// The approval is still open (or: just resolved, for the lane that asked).
    Approval { id: String },
}

/// What asked for a wake, in words: a playbook's reason, a command's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub source: String,
    pub text: String,
    /// Words from outside the person (a session's command): told as data.
    pub outside: bool,
}

/// One wake.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Wake {
    /// Its subject: one wake per key.
    pub key: String,
    pub priority: Priority,
    pub precondition: Precondition,
    /// The events it is about (their ids), oldest first.
    pub events: Vec<String>,
    /// The projects it is about (their keys or ids): the situation holds only these.
    pub projects: Vec<String>,
    /// When it was made, when its last event came, and when it is no use any more (ms).
    pub made_ms: u64,
    pub last_ms: u64,
    pub deadline_ms: Option<u64>,
    /// Digests folded into it when the queue was full.
    #[serde(default)]
    pub folded: u32,
    #[serde(default)]
    pub notes: Vec<Note>,
}

impl Wake {
    pub fn new(key: impl Into<String>, priority: Priority, now_ms: u64) -> Wake {
        Wake { key: key.into(), priority, precondition: Precondition::None, events: Vec::new(), projects: Vec::new(), made_ms: now_ms, last_ms: now_ms, deadline_ms: None, folded: 0, notes: Vec::new() }
    }

    /// Its level now: its priority, and one more for each ten minutes it waited (at most urgent).
    pub fn level(&self, now_ms: u64) -> u8 {
        let aged = (now_ms.saturating_sub(self.made_ms) / AGE_STEP.as_millis() as u64) as u8;
        (self.priority as u8).saturating_add(aged).min(Priority::Urgent as u8)
    }

    /// Whether it can run now: a digest only once its window closed.
    pub fn ready(&self, now_ms: u64) -> bool {
        self.priority != Priority::Digest
            || now_ms.saturating_sub(self.last_ms) >= DIGEST_QUIET.as_millis() as u64
            || now_ms.saturating_sub(self.made_ms) >= DIGEST_MOST.as_millis() as u64
            || self.events.len() >= DIGEST_EVENTS
    }
}

/// A digest is ready after this long with no new event…
pub const DIGEST_QUIET: Duration = Duration::from_secs(20);
/// …at most this long after its first…
pub const DIGEST_MOST: Duration = Duration::from_secs(120);
/// …or at this many events.
pub const DIGEST_EVENTS: usize = 50;
/// A waiting wake gains a level each time this passes.
pub const AGE_STEP: Duration = Duration::from_secs(10 * 60);
/// The most wakes the queue holds.
pub const BOUND: usize = 100;
/// The longest a wake may run.
pub const WAKE_TIMEOUT: Duration = Duration::from_secs(120);

/// Why a wake left the queue without running.
#[derive(Clone, Debug, PartialEq)]
pub enum Dropped {
    Stale(Wake),
    PastDeadline(Wake),
    Cancelled(Wake),
}

/// The queue.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Queue {
    wakes: Vec<Wake>,
}

impl Queue {
    pub fn len(&self) -> usize {
        self.wakes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.wakes.is_empty()
    }

    pub fn wakes(&self) -> &[Wake] {
        &self.wakes
    }

    /// Add a wake, merged with the one of its key (its events, the higher priority, the earlier
    /// deadline). Past [`BOUND`] the oldest digests fold into one.
    pub fn push(&mut self, wake: Wake, now_ms: u64) {
        if let Some(have) = self.wakes.iter_mut().find(|w| w.key == wake.key) {
            for event in wake.events {
                if !have.events.contains(&event) {
                    have.events.push(event);
                }
            }
            for project in wake.projects {
                if !have.projects.contains(&project) {
                    have.projects.push(project);
                }
            }
            for note in wake.notes {
                if !have.notes.contains(&note) {
                    have.notes.push(note);
                }
            }
            have.priority = have.priority.max(wake.priority);
            have.last_ms = now_ms;
            have.deadline_ms = match (have.deadline_ms, wake.deadline_ms) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            if wake.precondition != Precondition::None {
                have.precondition = wake.precondition;
            }
            return;
        }
        self.wakes.push(wake);
        if self.wakes.len() > BOUND {
            self.fold_digests(now_ms);
        }
    }

    /// The oldest digests, folded into one that counts them, until the queue fits.
    fn fold_digests(&mut self, now_ms: u64) {
        let mut digests: Vec<usize> = (0..self.wakes.len()).filter(|i| self.wakes[*i].priority == Priority::Digest).collect();
        digests.sort_by_key(|i| self.wakes[*i].made_ms);
        let over = self.wakes.len() - BOUND + 1;
        let take: Vec<usize> = digests.into_iter().take(over.max(2)).collect();
        if take.len() < 2 {
            // Nothing to fold: the oldest wake goes.
            if let Some(oldest) = (0..self.wakes.len()).min_by_key(|i| (self.wakes[*i].priority, self.wakes[*i].made_ms)) {
                self.wakes.remove(oldest);
            }
            return;
        }
        let mut folded = Wake::new("digest:folded", Priority::Digest, now_ms);
        for i in take.iter().rev() {
            let wake = self.wakes.remove(*i);
            folded.folded += 1 + wake.folded;
            folded.made_ms = folded.made_ms.min(wake.made_ms);
            folded.events.extend(wake.events);
            for project in wake.projects {
                if !folded.projects.contains(&project) {
                    folded.projects.push(project);
                }
            }
        }
        folded.events.truncate(DIGEST_EVENTS);
        self.push(folded, now_ms);
    }

    /// Cancel the wake of `key`.
    pub fn cancel(&mut self, key: &str) -> Option<Dropped> {
        let at = self.wakes.iter().position(|w| w.key == key)?;
        Some(Dropped::Cancelled(self.wakes.remove(at)))
    }

    /// The next wake to run: the highest level that is ready (the oldest among equals). Wakes past
    /// their deadline, and those whose precondition no longer holds (`holds`), are dropped on the
    /// way and returned for the journal.
    pub fn pop(&mut self, now_ms: u64, holds: &dyn Fn(&Precondition) -> bool) -> (Option<Wake>, Vec<Dropped>) {
        let mut dropped = Vec::new();
        self.wakes.retain(|w| {
            if w.deadline_ms.is_some_and(|d| d <= now_ms) {
                dropped.push(Dropped::PastDeadline(w.clone()));
                false
            } else {
                true
            }
        });
        loop {
            let next = (0..self.wakes.len()).filter(|i| self.wakes[*i].ready(now_ms)).max_by_key(|i| (self.wakes[*i].level(now_ms), std::cmp::Reverse(self.wakes[*i].made_ms)));
            let Some(i) = next else { return (None, dropped) };
            let wake = self.wakes.remove(i);
            if holds(&wake.precondition) {
                return (Some(wake), dropped);
            }
            dropped.push(Dropped::Stale(wake));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_000_000;

    fn wake(key: &str, priority: Priority, at: u64) -> Wake {
        Wake::new(key, priority, at)
    }

    #[test]
    fn urgent_first_then_playbooks_then_digests() {
        let mut q = Queue::default();
        q.push(wake("digest:app", Priority::Digest, T - 200_000), T);
        q.push(wake("pb:ci", Priority::Playbook, T), T);
        q.push(wake("urgent:auth", Priority::Urgent, T), T);
        let order: Vec<String> = std::iter::from_fn(|| q.pop(T, &|_| true).0.map(|w| w.key)).collect();
        assert_eq!(order, ["urgent:auth", "pb:ci", "digest:app"]);
    }

    #[test]
    fn a_digest_waits_for_quiet_and_never_forever() {
        let mut q = Queue::default();
        let mut d = wake("digest:app", Priority::Digest, T);
        d.events.push("e1".into());
        q.push(d, T);
        assert!(q.pop(T + 10_000, &|_| true).0.is_none(), "not quiet yet");
        // A steady stream of events: each one restarts the quiet, but 120 s is the most.
        for s in 1..12 {
            let mut more = wake("digest:app", Priority::Digest, T + s * 10_000);
            more.events.push(format!("e{}", s + 1));
            q.push(more, T + s * 10_000);
        }
        assert!(q.pop(T + 115_000, &|_| true).0.is_none());
        let taken = q.pop(T + 120_000, &|_| true).0.unwrap();
        assert_eq!(taken.events.len(), 12, "coalesced into one");
    }

    #[test]
    fn waiting_digests_rise_so_none_starves() {
        let old = wake("digest:old", Priority::Digest, T);
        assert_eq!(old.level(T), 0);
        assert_eq!(old.level(T + 20 * 60_000), 2, "two steps of ten minutes");
        let mut q = Queue::default();
        q.push(old, T);
        q.push(wake("pb:new", Priority::Playbook, T + 20 * 60_000), T + 20 * 60_000);
        assert_eq!(q.pop(T + 20 * 60_000, &|_| true).0.unwrap().key, "digest:old");
    }

    #[test]
    fn stale_wake_dropped() {
        let mut q = Queue::default();
        let mut ci = wake("pr:acme/app#7", Priority::Playbook, T);
        ci.precondition = Precondition::CiRed { repo: "acme/app".into(), number: 7, head: "abc".into() };
        q.push(ci, T);
        q.push(wake("pb:other", Priority::Playbook, T + 1), T + 1);
        let (next, dropped) = q.pop(T + 5, &|p| !matches!(p, Precondition::CiRed { .. }));
        assert_eq!(next.unwrap().key, "pb:other");
        assert!(matches!(&dropped[..], [Dropped::Stale(w)] if w.key == "pr:acme/app#7"));
        let mut late = wake("pb:late", Priority::Playbook, T);
        late.deadline_ms = Some(T + 10);
        q.push(late, T);
        assert!(matches!(&q.pop(T + 10, &|_| true).1[..], [Dropped::PastDeadline(_)]));
        q.push(wake("pb:x", Priority::Playbook, T), T);
        assert!(matches!(q.cancel("pb:x"), Some(Dropped::Cancelled(_))) && q.is_empty());
    }

    #[test]
    fn a_full_queue_folds_its_oldest_digests() {
        let mut q = Queue::default();
        for n in 0..BOUND as u64 + 5 {
            let mut d = wake(&format!("digest:{n}"), Priority::Digest, T + n);
            d.events.push(format!("e{n}"));
            q.push(d, T + n);
        }
        assert!(q.len() <= BOUND, "{}", q.len());
        let folded = q.wakes().iter().find(|w| w.key == "digest:folded").unwrap();
        assert!(folded.folded >= 2 && folded.events.contains(&"e0".to_string()));
    }
}
