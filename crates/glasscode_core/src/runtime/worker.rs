//! The manager's wake lane worker (M51; moved here from the app for M57, so the app on the Mac
//! and glasscoded on the server run the same one): on a thread of its own, it reads the journal
//! from where it started, makes the wakes the events call for (`lanes::wake_for_event`), and runs
//! the queue as the budget lets it: each wake on a fresh brain, its situation holding only its
//! projects, its answer read by Rust. Before a wake runs, what it is about is read again from the
//! events since; a stale one is dropped with a journal line. A line it tells goes into the
//! manager's conversation; a notice is a notification; an op it proposes is a card, unless the
//! playbook that fired it was approved to act. While the person's chat turn runs, the worker
//! holds. The host signs each brain's MCP server for its lane and wake (`sign`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::caller::Lane;
use crate::conversation::{Author, Conversation, Draft, Kind, Stream};
use crate::journal::Event;
use crate::runtime::lanes::{self, Hold, Locks};
use crate::runtime::queue::{Queue, Wake};
use crate::runtime::situation::{Fact, Project};
use crate::runtime::wake::Effect;
use crate::runtime::{Brain, BrainSpec, Host, Ledger};
use crate::store::Store as _;

/// What signs a brain's MCP server for its lane and wake (`control::sign` with the host's secret).
pub type Sign = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// A brain's MCP server environment: its lane and wake, signed by the app.
pub fn brain_env(lane: Lane, wake: Option<&str>, sign: impl Fn(&str) -> String) -> Vec<(String, String)> {
    let text = crate::mcp::brain_text(lane, wake);
    vec![
        (crate::mcp::BRAIN_LANE.to_string(), if lane == Lane::Chat { "chat" } else { "wake" }.to_string()),
        (crate::mcp::BRAIN_WAKE.to_string(), wake.unwrap_or_default().to_string()),
        (crate::mcp::BRAIN_SIG.to_string(), sign(&text)),
    ]
}

/// The wake lane's worker.
pub struct Worker {
    queue: Arc<Mutex<Queue>>,
    hold: Hold,
    locks: Locks,
    stop: Arc<AtomicBool>,
    projects: Arc<Mutex<Vec<Project>>>,
    wakes: Arc<Mutex<HashMap<String, Vec<String>>>>,
    base: BrainSpec,
    sign: Sign,
    /// The last wake brain's health, and when it was read: the next wake starts from it (a
    /// signed-out account is not tried, nor told, on every wake), and tries again after a while.
    health: (crate::runtime::BrainHealth, u64),
}

/// The newest events the worker keeps, for the staleness of its wakes.
const SEEN: usize = 500;
/// How often the worker looks at the journal and the queue when there is nothing to run.
const POLL: Duration = Duration::from_secs(2);
/// How long a brain that cannot think is believed before a wake tries it again (and, if it still
/// cannot, says so once more).
const REPROBE_MS: u64 = 30 * 60 * 1000;

/// The events after a wake's own (all of them when none of its events is kept).
fn after<'a>(seen: &'a [Event], wake: &Wake) -> &'a [Event] {
    match seen.iter().rposition(|e| wake.events.contains(&e.id)) {
        Some(at) => &seen[at + 1..],
        None if wake.events.is_empty() => &[],
        None => seen,
    }
}

/// A nudge for this matter is open in the conversation already: a later wake does not post it
/// again.
fn nudge_open(chat: &Conversation, matter: &str) -> bool {
    chat.items().iter().any(|item| !item.closed && matches!(&item.kind, Kind::Nudge { matter: open } if open == matter))
}

impl Worker {
    /// A worker with the lanes' shared state, the wake brains' spec (`base`) and the signer; it
    /// runs until `stop` is set.
    #[allow(clippy::too_many_arguments)]
    pub fn new(queue: Arc<Mutex<Queue>>, hold: Hold, locks: Locks, stop: Arc<AtomicBool>, projects: Arc<Mutex<Vec<Project>>>, wakes: Arc<Mutex<HashMap<String, Vec<String>>>>, base: BrainSpec, sign: Sign) -> Worker {
        Worker { queue, hold, locks, stop, projects, wakes, base, sign, health: (crate::runtime::BrainHealth::Ready, 0) }
    }

    pub fn run(mut self, host: &dyn Host) {
        let mut cursor = crate::journal::end();
        let mut seen: Vec<Event> = Vec::new();
        while !self.stop.load(Ordering::SeqCst) {
            let now = crate::clock::now_ms();
            let (events, next) = crate::journal::read_from(&cursor, 200);
            cursor = next;
            for event in events {
                if let Some(wake) = lanes::wake_for_event(&event, now) {
                    self.queue.lock().unwrap_or_else(|p| p.into_inner()).push(wake, now);
                }
                seen.push(event);
            }
            if seen.len() > SEEN {
                seen.drain(..seen.len() - SEEN);
            }
            let ledger = Ledger::load().unwrap_or_default();
            let (wake, dropped) = lanes::next_wake(&mut self.queue.lock().unwrap_or_else(|p| p.into_inner()), &self.hold, &ledger, now, &|w| lanes::still_holds(&w.precondition, after(&seen, w)));
            for gone in &dropped {
                crate::journal::append(lanes::dropped_event(gone));
            }
            match wake {
                // One that did not start goes back, and the worker waits before the next look.
                Some(wake) => {
                    if let Some(again) = self.run_wake(host, wake, now) {
                        self.queue.lock().unwrap_or_else(|p| p.into_inner()).push(again, now);
                        std::thread::sleep(POLL);
                    }
                }
                None => std::thread::sleep(POLL),
            }
        }
    }

    /// Run one wake; the wake back when it did not start.
    fn run_wake(&mut self, host: &dyn Host, wake: Wake, now: u64) -> Option<Wake> {
        let id = crate::model::new_id();
        let mut spec = self.base.clone();
        spec.brain_env = brain_env(Lane::Wake, Some(&id), |text| (self.sign)(text));
        let spec = lanes::wake_spec(&spec, &id)?;
        // What its events said, kept with it, and what is open, from the work ledger: the wake
        // carries no history (M51 step 2).
        let mut facts: Vec<Fact> = wake.facts.clone();
        match crate::work::WorkLedger::load() {
            Ok(ledger) => facts.extend(crate::runtime::situation::work_facts(&ledger)),
            Err(err) => log::warn!("manager: the work ledger could not be read for a wake: {err}"),
        }
        let projects = self.projects.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let situation = crate::runtime::situation::for_wake(&wake, &facts, &projects);
        // What its tools may reach: the projects its situation holds (trust-9, in the gate).
        let own: Vec<String> = crate::runtime::situation::projects_of(&wake, &projects).iter().map(|p| p.id.clone()).collect();
        self.wakes.lock().unwrap_or_else(|p| p.into_inner()).insert(id.clone(), own);
        // It acts only under the playbook that fired it, while that is approved to act.
        let under = match crate::playbooks::load(&crate::playbooks::model::journal_schema()) {
            Ok(book) => lanes::under_playbook(&wake, &book),
            Err(err) => {
                log::warn!("manager: the playbooks could not be read, so the wake only proposes: {err}");
                None
            }
        };
        // The lane's own copy of the ledger; what it records joins the stored one after.
        let before = Ledger::load().unwrap_or_default();
        let mut ledger = before.clone();
        let mut brain = Brain::new(spec);
        let believed = !self.health.0.can_think() && now.saturating_sub(self.health.1) < REPROBE_MS;
        if believed {
            brain.health = self.health.0.clone();
        }
        let run = lanes::run_wake(host, &mut ledger, &mut brain, wake, &situation, under.as_deref(), now);
        if !believed || brain.health != self.health.0 {
            self.health = (brain.health.clone(), now);
        }
        if let Err(err) = crate::store::update::<Ledger, _>(|stored| stored.add_since(&before, &ledger)) {
            log::warn!("manager: the token ledger could not be kept: {err}");
        }
        if let Some(why) = &run.why_not {
            log::info!("manager: the wake {} did nothing: {why}", run.wake.key);
        }
        if run.retry {
            self.wakes.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
            return Some(run.wake);
        }
        let mut chat = Conversation::load();
        let post = |chat: &mut Conversation, kind: Kind, text: String| {
            if let Err(err) = chat.post(Draft { author: Author::Wake(id.clone()), kind, text, refs: Vec::new(), topic: None }) {
                log::warn!("manager: a wake's line could not be posted: {err}");
            }
        };
        for effect in run.effects {
            match effect {
                Effect::Tell(text) => post(&mut chat, Kind::Manager { state: Stream::Done }, text),
                Effect::Notify(notice) => host.notify(&notice.title, &notice.body, &run.wake.key),
                Effect::Matter(matter) => {
                    if !nudge_open(&chat, &matter.key) {
                        post(&mut chat, Kind::Nudge { matter: matter.key }, matter.summary);
                    }
                }
                // The gate's ask posts the card; a refusal says why here.
                Effect::Card { op, reason } => {
                    if let Err(why) = host.propose(&op, &reason, Lane::Wake, Some(&id)) {
                        post(&mut chat, Kind::System, format!("A proposal to {} was not made: {why}. ({reason})", op.spec.summary));
                    }
                }
                Effect::Run { op, playbook, reason } => {
                    let caller = crate::caller::Caller::Manager { lane: Lane::Wake, wake: Some(id.clone()), project: None };
                    let outcome = match host.run_under(&op, &caller, &playbook) {
                        crate::ops::OpReply::Done { text, .. } => text,
                        crate::ops::OpReply::Waiting { summary, .. } => format!("waiting for you: {summary}"),
                        crate::ops::OpReply::Failed(error) => error.text,
                    };
                    post(&mut chat, Kind::System, format!("Under the playbook {playbook}, {}: {reason}. {outcome}", op.spec.summary));
                }
                Effect::Refused { op, why } => log::info!("manager: a wake proposed {op}, which does nothing: {why}"),
            }
        }
        // The wake's turn ended: what it worked on is free for the chat, and its tools reach
        // nothing more.
        self.locks.release(Lane::Wake);
        self.wakes.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Staleness reads the events after a wake's own: all kept ones when its own are gone, none
    /// when it has none.
    #[test]
    fn a_wake_is_judged_by_what_came_after_it() {
        let event = |id: &str| {
            let mut e = Event::new(crate::journal_kinds::CI_PASSED, crate::journal::Source::Forge, "x");
            e.id = id.into();
            e
        };
        let seen = vec![event("a"), event("b"), event("c")];
        let mut wake = Wake::new("ci:x#1", crate::runtime::queue::Priority::Urgent, 0);
        assert!(after(&seen, &wake).is_empty());
        wake.events.push("b".into());
        assert_eq!(after(&seen, &wake).iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["c"]);
        wake.events = vec!["gone".into()];
        assert_eq!(after(&seen, &wake).len(), 3);
    }
}
