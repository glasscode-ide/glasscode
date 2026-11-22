//! The work ledger (slice M32): one [`WorkItem`] per piece of work (`identity::WorkKey`), linking
//! its board task, ticket, sessions and pull request, with done conditions that code checks, never
//! a model (events-14, land-8).
//!
//! - **Conditions** are typed predicates over facts the journal already holds: the pull request's
//!   state and CI, a review requested or posted, a board task done, a turn that ended without a
//!   question, the person's word. A session gets them at its start from its kind ([`conditions_for`]):
//!   a pull-request session `CiGreen` and `ReviewRequested`, a review session `ReviewPosted`, a ticket
//!   session `PrReadyForReview`; a session started from words has none and is done when its pull
//!   request merges. Only the person adds `Person`.
//! - **Facts** come from journal events ([`WorkLedger::observe`]), after each turn report and each
//!   status round, and the state is evaluated then ([`WorkLedger::evaluate`]): Open, Waiting(who),
//!   Stuck(why), Done or Dropped.
//! - **Stuck** is a rule: working 45 minutes with no turn end, three failed turns in a row, or CI
//!   still red after two fix messages.
//! - **Claimed is not verified.** A session's `glasscode report done` is kept as a claim and never
//!   satisfies a condition; a claim the facts do not back is said once (`work.claim_unverified`).
//! - The vendors' own goal evaluators (Claude's `/goal`, Codex's thread goals) are an option per CLI
//!   version, not used in release 1 (land-8).
//!
//! `manager/work.json` is the ledger's store; each item also has its work file in memory
//! (`projects/<key>/work/<name>.md`), whose "Status" section is generated from the item and whose
//! "Notes from sessions" stay as the sessions wrote them ([`render_work_file`]). A branch rename
//! moves the item to its new key and its file to the new name in one memory commit (exec-12).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::identity::{Hlc, WorkKey, WorkKind};
use crate::journal::{Event, Source, kinds};
use crate::memstore::entry::{self as memory_entry, Doc, Line};
use crate::model::Session;
use crate::paths::{MachineId, ProjectKey};
use crate::store::{OnDamage, Store};

/// How long a turn may run before its work counts as stuck.
pub const LONG_TURN_MS: u64 = 45 * 60 * 1000;
/// Failed turns in a row that make work stuck.
pub const FAILED_TURNS: u32 = 3;
/// CI-fix messages after which CI still red makes work stuck.
pub const FIX_MESSAGES: u32 = 2;
/// How long after its pull request merged or closed, with no session open, a work's memory goes
/// to the archive (M24).
pub const ARCHIVE_AFTER_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// What makes a piece of work done (design section 8), each checked by code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "task", rename_all = "snake_case")]
pub enum DoneCondition {
    PrMerged,
    PrReadyForReview,
    CiGreen,
    ReviewRequested,
    /// A review session posted its review.
    ReviewPosted,
    BoardTaskDone(String),
    /// The last turn ended without a question to the person.
    AnsweredWithoutQuestion,
    /// The person said so.
    Person,
}

impl DoneCondition {
    pub fn words(&self) -> String {
        match self {
            DoneCondition::PrMerged => "the pull request merged".into(),
            DoneCondition::PrReadyForReview => "a pull request ready for review".into(),
            DoneCondition::CiGreen => "CI green".into(),
            DoneCondition::ReviewRequested => "a review requested".into(),
            DoneCondition::ReviewPosted => "the review posted".into(),
            DoneCondition::BoardTaskDone(task) => format!("board task {task} done"),
            DoneCondition::AnsweredWithoutQuestion => "an answer with no question left".into(),
            DoneCondition::Person => "you say it is done".into(),
        }
    }
}

/// Where a piece of work stands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "why", rename_all = "snake_case")]
pub enum WorkState {
    #[default]
    Open,
    /// Waiting on someone or something: "you", "CI", "a review", "the session".
    Waiting(String),
    Stuck(String),
    Done,
    Dropped,
}

impl WorkState {
    pub fn words(&self) -> String {
        match self {
            WorkState::Open => "open".into(),
            WorkState::Waiting(who) => format!("waiting for {who}"),
            WorkState::Stuck(why) => format!("stuck: {why}"),
            WorkState::Done => "done".into(),
            WorkState::Dropped => "dropped".into(),
        }
    }

    fn ended(&self) -> bool {
        matches!(self, WorkState::Done | WorkState::Dropped)
    }
}

/// What the journal said of a piece of work: the facts its conditions read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Facts {
    /// OPEN, MERGED or CLOSED.
    pub pr_state: Option<String>,
    pub pr_draft: bool,
    /// passed, failed or running, and the commit it ran on.
    pub ci: Option<String>,
    pub ci_head: Option<String>,
    pub review_requested: bool,
    pub review_posted: bool,
    pub board_done: BTreeSet<String>,
    pub person_confirmed: bool,
    /// The last turn's end, and whether it left a question.
    pub last_turn: Option<String>,
    pub last_question: bool,
    pub failed_turns: u32,
    /// A turn is open since this time (ms), and which turn: a report that comes late for the turn
    /// before does not close the one that runs now.
    pub open_turn_since: Option<u64>,
    pub open_turn: Option<String>,
    /// CI-fix messages since CI last turned red.
    pub fix_messages: u32,
    /// What a session said of it (`glasscode report`): a claim, never evidence.
    pub claimed: Option<String>,
    /// The claim was said to be unverified already.
    pub claim_told: bool,
    /// `Person` is among the conditions because the person said done (`work done`), not because
    /// the work started with it: reopening takes it out again.
    pub person_added: bool,
    /// When its pull request merged or closed (ms), as the journal or a status round first said.
    pub pr_ended_ms: Option<u64>,
    /// Its memory went to the archive (M24): it is not archived again.
    pub archived: bool,
}

/// One piece of work (design section 8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    pub key: WorkKey,
    pub sessions: Vec<String>,
    pub pr: Option<(String, u64)>,
    pub ticket: Option<String>,
    pub board_task: Option<String>,
    pub done: Vec<DoneCondition>,
    pub state: WorkState,
    pub updated: Hlc,
    /// The ids it had before (a renamed branch), so old references still find it.
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub facts: Facts,
}

/// One line of Today's "Coming up" ([`WorkLedger::coming_up`]): the work, its newest session, its
/// pull request, a short word for the card's time column ("CI", "Review", "Next"), and what it
/// waits for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Next {
    pub key: WorkKey,
    pub session: Option<String>,
    pub pr: Option<(String, u64)>,
    pub when: &'static str,
    pub what: String,
}

/// A session's done conditions at its start, by its kind (step 2).
pub fn conditions_for(key: &WorkKey) -> Vec<DoneCondition> {
    match key.kind {
        WorkKind::Pr => vec![DoneCondition::CiGreen, DoneCondition::ReviewRequested],
        WorkKind::Review => vec![DoneCondition::ReviewPosted],
        WorkKind::Ticket => vec![DoneCondition::PrReadyForReview],
        // Started from words: done when its pull request merges (no condition to hold).
        WorkKind::Branch => Vec::new(),
    }
}

impl WorkItem {
    pub fn new(key: WorkKey, at: Hlc) -> WorkItem {
        let done = conditions_for(&key);
        WorkItem { key, sessions: Vec::new(), pr: None, ticket: None, board_task: None, done, state: WorkState::Open, updated: at, aliases: Vec::new(), facts: Facts::default() }
    }

    /// Whether a condition holds, from the facts alone (a claim never counts).
    pub fn met(&self, condition: &DoneCondition) -> bool {
        let f = &self.facts;
        match condition {
            DoneCondition::PrMerged => f.pr_state.as_deref() == Some("MERGED"),
            DoneCondition::PrReadyForReview => self.pr.is_some() && !f.pr_draft && matches!(f.pr_state.as_deref(), Some("OPEN" | "MERGED")),
            DoneCondition::CiGreen => f.ci.as_deref() == Some("passed"),
            DoneCondition::ReviewRequested => f.review_requested,
            DoneCondition::ReviewPosted => f.review_posted,
            DoneCondition::BoardTaskDone(task) => f.board_done.contains(task),
            DoneCondition::AnsweredWithoutQuestion => f.last_turn.as_deref() == Some("Completed") && !f.last_question,
            DoneCondition::Person => f.person_confirmed,
        }
    }

    /// Whether the facts say it is done.
    pub fn verified_done(&self) -> bool {
        if self.facts.person_confirmed {
            return true;
        }
        if self.done.is_empty() {
            return self.facts.pr_state.as_deref() == Some("MERGED");
        }
        self.done.iter().all(|condition| self.met(condition))
    }

    /// Where it stands at `now_ms`, from its facts.
    pub fn evaluate(&self, now_ms: u64) -> WorkState {
        if self.state.ended() {
            return self.state.clone();
        }
        if self.verified_done() {
            return WorkState::Done;
        }
        let f = &self.facts;
        if f.pr_state.as_deref() == Some("CLOSED") && self.done.iter().any(|c| matches!(c, DoneCondition::PrMerged | DoneCondition::CiGreen | DoneCondition::PrReadyForReview)) {
            return WorkState::Stuck("its pull request was closed without a merge".into());
        }
        if let Some(since) = f.open_turn_since
            && now_ms.saturating_sub(since) >= LONG_TURN_MS
        {
            return WorkState::Stuck("working for 45 minutes with no turn end".into());
        }
        if f.failed_turns >= FAILED_TURNS {
            return WorkState::Stuck(format!("{} failed turns in a row", f.failed_turns));
        }
        if f.ci.as_deref() == Some("failed") && f.fix_messages >= FIX_MESSAGES {
            return WorkState::Stuck(format!("CI is still red after {} fix messages", f.fix_messages));
        }
        if f.last_question && f.open_turn_since.is_none() {
            return WorkState::Waiting("you".into());
        }
        if f.open_turn_since.is_some() {
            return WorkState::Waiting("the session".into());
        }
        if f.ci.as_deref() == Some("running") {
            return WorkState::Waiting("CI".into());
        }
        if f.review_requested && self.done.iter().any(|c| matches!(c, DoneCondition::PrMerged)) {
            return WorkState::Waiting("a review".into());
        }
        WorkState::Open
    }

    /// The work item a v1 follow-up becomes (the shape M44 migrates them with): the teammate's
    /// pull request, waiting on them while it is open.
    pub fn from_follow_up(follow: &crate::manager::FollowUp, project: &ProjectKey, at: Hlc) -> WorkItem {
        use crate::manager::FollowState;
        let key = WorkKey { project: project.clone(), kind: WorkKind::Pr, id: format!("{}#{}", follow.repo.trim().to_lowercase(), follow.number) };
        let state = match follow.state {
            FollowState::Open => WorkState::Waiting(if follow.person.is_empty() { follow.login.clone() } else { follow.person.clone() }),
            FollowState::Fixed => WorkState::Waiting("you".into()),
            FollowState::Done => WorkState::Done,
            FollowState::Dropped => WorkState::Dropped,
        };
        WorkItem {
            sessions: follow.session.iter().cloned().collect(),
            pr: Some((follow.repo.trim().to_lowercase(), follow.number)),
            done: vec![DoneCondition::Person],
            state,
            ..WorkItem::new(key, at)
        }
    }
}

/// What a change of the ledger asks the memory to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryChange {
    /// Write the item's work file (its state changed).
    Write(WorkKey),
    /// Move the work file from the old key to the new one (a branch rename), in one commit.
    Move { from: WorkKey, to: WorkKey },
}

/// `manager/work.json`: every piece of work, and what the journal said of each.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkLedger {
    pub items: Vec<WorkItem>,
    /// The follow-ups (M56.2, the `follow-ups` built-in): what the person asked of a teammate in a
    /// review, reviews teammates left on the person's pull requests, and what the person asked to
    /// keep an eye on. A list beside the items, not items: one pull request can carry several
    /// (a review from each reviewer) and its session's own work.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub follow_ups: Vec<crate::manager::FollowUp>,
    /// What was told of them (`followups`' keys: `pr|`, `prr|`, `review|`, `ci|`, `fixed|`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub follow_seen: BTreeMap<String, String>,
    /// Each follow-up's last change, by its id: the stamp the sync goes by (M56.2), as an item's
    /// `updated` (a follow-up's own `updated` is its last activity, which the quiet rule reads).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub follow_stamps: BTreeMap<String, Hlc>,
}

/// A follow-up as the sync carries it (M56.2): the record, its "fixed" mark (`fixed|<id>`, so
/// "ready for you to look again" is told once on either home), and the stamp of its last change.
/// The snapshot's keys (`pr|`, `prr|`, `review|`, `ci|`) stay where they were written: only the
/// Mac reads its sessions' pull requests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowSync {
    pub follow: crate::manager::FollowUp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed: Option<String>,
    pub stamp: Hlc,
}

fn fixed_key(id: &str) -> String {
    format!("fixed|{id}")
}

impl WorkLedger {
    /// The follow-ups' rules run on this view of the ledger (`followups::round`, `given`).
    pub fn book(&mut self) -> crate::manager::followups::Book<'_> {
        crate::manager::followups::Book { followups: &mut self.follow_ups, seen: &mut self.follow_seen }
    }

    /// v1's follow-ups and their keys, taken in (M56.2, as M44's migration takes the rest): a new
    /// one added, a known one replaced when v1's is newer (`updated`), and a key the ledger has not
    /// got kept. Running it again changes nothing. True when the ledger changed.
    pub fn take_follow_ups(&mut self, followups: Vec<crate::manager::FollowUp>, seen: BTreeMap<String, String>) -> bool {
        let mut changed = false;
        for follow in followups {
            match self.follow_ups.iter_mut().find(|have| have.id == follow.id) {
                None => {
                    self.follow_ups.push(follow);
                    changed = true;
                }
                Some(have) if follow.updated > have.updated => {
                    *have = follow;
                    changed = true;
                }
                Some(_) => {}
            }
        }
        for (key, value) in seen {
            if !self.follow_seen.contains_key(&key) {
                self.follow_seen.insert(key, value);
                changed = true;
            }
        }
        changed
    }

    /// A change to the follow-ups, on either home: each follow-up whose record or "fixed" mark it
    /// changed, and each one with no stamp yet, gets this machine's next stamp, so the sync
    /// carries it (M56.2). Every change to them goes through here.
    pub fn follow_edit<R>(&mut self, machine: &MachineId, edit: impl FnOnce(&mut WorkLedger) -> R) -> R {
        let before: BTreeMap<String, (crate::manager::FollowUp, Option<String>)> = self.follow_ups.iter().map(|f| (f.id.clone(), (f.clone(), self.follow_seen.get(&fixed_key(&f.id)).cloned()))).collect();
        let out = edit(self);
        let changed: Vec<String> = self
            .follow_ups
            .iter()
            .filter(|f| !self.follow_stamps.contains_key(&f.id) || before.get(&f.id).is_none_or(|(was, mark)| was != *f || *mark != self.follow_seen.get(&fixed_key(&f.id)).cloned()))
            .map(|f| f.id.clone())
            .collect();
        if !changed.is_empty() {
            // Later than the stamps they had (another home's, from a clock ahead of this one):
            // the change wins there too.
            let seen = changed.iter().filter_map(|id| self.follow_stamps.get(id)).max().cloned();
            let stamps = crate::identity::next_stamps_after(machine, changed.len(), seen.as_ref()).unwrap_or_default();
            for (i, id) in changed.into_iter().enumerate() {
                let stamp = stamps.get(i).cloned().unwrap_or_else(|| match &seen {
                    Some(seen) => Hlc::zero(machine.clone()).merge(seen),
                    None => stamp(machine),
                });
                self.follow_stamps.insert(id, stamp);
            }
        }
        out
    }

    /// This machine's own changes to the follow-ups stamped after `after`, oldest first (what a
    /// sync sends). One taken from the other home keeps its stamp, so it never goes back.
    pub fn follow_changed(&self, machine: &MachineId, after: Option<&Hlc>) -> Vec<FollowSync> {
        let mut out: Vec<FollowSync> = self
            .follow_ups
            .iter()
            .filter_map(|f| {
                let stamp = self.follow_stamps.get(&f.id)?;
                (&stamp.machine == machine && after.is_none_or(|after| stamp > after)).then(|| FollowSync { follow: f.clone(), fixed: self.follow_seen.get(&fixed_key(&f.id)).cloned(), stamp: stamp.clone() })
            })
            .collect();
        out.sort_by(|a, b| a.stamp.cmp(&b.stamp));
        out
    }

    /// Take the other home's follow-ups: each one whose stamp is newer than this ledger's (or
    /// that it has not got) replaces this one, with its "fixed" mark. A stamp too far ahead of this
    /// clock is quarantined (not taken, crit-9). True when the ledger changed.
    pub fn follow_merge(&mut self, theirs: &[FollowSync]) -> bool {
        let mut changed = false;
        for sync in theirs {
            if sync.stamp.too_far_ahead() {
                log::warn!("work ledger: a follow-up from the other home is stamped too far ahead; it is not taken");
                crate::identity::quarantined("work", 1, Some(&sync.stamp.machine));
                continue;
            }
            let id = &sync.follow.id;
            if self.follow_stamps.get(id).is_some_and(|ours| ours >= &sync.stamp) {
                continue;
            }
            match self.follow_ups.iter_mut().find(|f| f.id == *id) {
                Some(ours) => *ours = sync.follow.clone(),
                None => self.follow_ups.push(sync.follow.clone()),
            }
            match &sync.fixed {
                Some(mark) => self.follow_seen.insert(fixed_key(id), mark.clone()),
                None => self.follow_seen.remove(&fixed_key(id)),
            };
            self.follow_stamps.insert(id.clone(), sync.stamp.clone());
            changed = true;
        }
        changed
    }
}

impl Store for WorkLedger {
    const NAME: &'static str = "manager/work";
    const VERSION: u32 = 1;
    // Rebuilt from the journal by the reconciler's facts; an older generation is fine.
    const ON_DAMAGE: OnDamage = OnDamage::Refuse;

    fn path() -> PathBuf {
        crate::paths::root().join("manager").join("work.json")
    }
}

fn stamp(machine: &MachineId) -> Hlc {
    crate::identity::next_stamps(machine, 1).ok().and_then(|mut s| s.pop()).unwrap_or_else(|| Hlc::now(&Hlc::zero(machine.clone())))
}

impl WorkLedger {
    /// This machine's own changes stamped after `after` (what a sync sends, M57), oldest first:
    /// items whose newest stamp is this machine's. An item taken from the other home keeps its
    /// stamp, so it never goes back.
    pub fn own_changed(&self, machine: &MachineId, after: Option<&Hlc>) -> Vec<WorkItem> {
        let mut out: Vec<WorkItem> = self.items.iter().filter(|item| &item.updated.machine == machine && after.is_none_or(|after| &item.updated > after)).cloned().collect();
        out.sort_by(|a, b| a.updated.cmp(&b.updated));
        out
    }

    /// Take the other home's items (a sync, M57), each matched by its key or a name it had (a
    /// renamed branch). The newer stamp wins the item; the facts are joined, since each home reads
    /// its own journal: what either saw requested, posted, confirmed or done stays seen. A stamp
    /// too far ahead of this clock is quarantined (not taken, crit-9). Gives the keys that changed.
    pub fn merge_theirs(&mut self, theirs: &[WorkItem]) -> Vec<WorkKey> {
        let same = |ours: &WorkItem, theirs: &WorkItem| {
            ours.key == theirs.key || (ours.key.project == theirs.key.project && ours.key.kind == theirs.key.kind && (theirs.aliases.contains(&ours.key.id) || ours.aliases.contains(&theirs.key.id)))
        };
        let mut changed = Vec::new();
        for item in theirs {
            if item.updated.too_far_ahead() {
                log::warn!("work ledger: an item from the other home is stamped too far ahead; it is not taken");
                crate::identity::quarantined("work", 1, Some(&item.updated.machine));
                continue;
            }
            match self.items.iter_mut().find(|ours| same(ours, item)) {
                Some(ours) if item.updated > ours.updated => {
                    let mut facts = item.facts.clone();
                    facts.review_requested |= ours.facts.review_requested;
                    facts.review_posted |= ours.facts.review_posted;
                    facts.person_confirmed |= ours.facts.person_confirmed;
                    facts.claim_told |= ours.facts.claim_told;
                    facts.board_done.extend(ours.facts.board_done.iter().cloned());
                    let mut sessions = ours.sessions.clone();
                    sessions.extend(item.sessions.iter().filter(|s| !ours.sessions.contains(s)).cloned());
                    let mut aliases = ours.aliases.clone();
                    aliases.extend(item.aliases.iter().filter(|a| !ours.aliases.contains(a)).cloned());
                    if ours.key != item.key && !aliases.contains(&ours.key.id) {
                        aliases.push(ours.key.id.clone());
                    }
                    aliases.retain(|a| a != &item.key.id);
                    *ours = WorkItem { facts, sessions, aliases, ..item.clone() };
                    changed.push(item.key.clone());
                }
                Some(_) => {}
                None => {
                    self.items.push(item.clone());
                    changed.push(item.key.clone());
                }
            }
        }
        changed
    }

    pub fn item(&self, key: &WorkKey) -> Option<&WorkItem> {
        self.items.iter().find(|item| item.key == *key || (item.key.project == key.project && item.key.kind == key.kind && item.aliases.contains(&key.id)))
    }

    fn index_of_session(&self, session: &str) -> Option<usize> {
        self.items.iter().position(|item| !item.state.ended() && item.sessions.iter().any(|s| s == session))
    }

    /// The open work a session belongs to.
    pub fn of_session(&self, session: &str) -> Option<&WorkItem> {
        self.index_of_session(session).map(|at| &self.items[at])
    }

    /// The work of a session that started (made with its kind's conditions when new). True when
    /// the ledger changed.
    pub fn ensure(&mut self, key: WorkKey, session: &Session, ticket: Option<&str>, machine: &MachineId) -> bool {
        let at = match self.items.iter().position(|item| item.key == key) {
            Some(at) => at,
            None => {
                let mut item = WorkItem::new(key.clone(), stamp(machine));
                item.ticket = ticket.map(str::to_string);
                if let Some(review) = &session.review {
                    item.pr = Some((review.repo.to_lowercase(), review.number));
                }
                if key.kind == WorkKind::Pr
                    && let Some((repo, number)) = key.id.split_once('#')
                {
                    item.pr = number.parse().ok().map(|n| (repo.to_string(), n));
                }
                self.items.push(item);
                self.items.len() - 1
            }
        };
        let item = &mut self.items[at];
        if item.sessions.iter().any(|s| *s == session.id) {
            return false;
        }
        item.sessions.push(session.id.clone());
        true
    }

    /// The person adds "done when I say so" to a piece of work, or says it is done.
    pub fn confirm(&mut self, key: &WorkKey) -> bool {
        let Some(item) = self.items.iter_mut().find(|item| item.key == *key) else { return false };
        if !item.done.contains(&DoneCondition::Person) {
            item.done.push(DoneCondition::Person);
            item.facts.person_added = true;
        }
        item.facts.person_confirmed = true;
        true
    }

    /// The work a word names (`work show`, `work done`): `<project key>:<id>`, or an id
    /// (`acme/app#42`, `ENG-42` in any case, `feat/login`), or an old id of a renamed branch. Work
    /// that ended counts too. Every item it could be, newest first: none, one, or several from
    /// different projects.
    pub fn named(&self, word: &str) -> Vec<&WorkItem> {
        let word = word.trim();
        let (project, id) = match word.split_once(':') {
            Some((project, id)) if ProjectKey::parse(project).is_some() && self.items.iter().any(|item| item.key.project.as_str() == project) => (Some(project), id),
            _ => (None, word),
        };
        let same = |item: &WorkItem, id: &str| match item.key.kind {
            // Tickets are named in capitals; pull requests by their lower-case repository.
            WorkKind::Ticket | WorkKind::Pr | WorkKind::Review => item.key.id.eq_ignore_ascii_case(id) || item.aliases.iter().any(|a| a.eq_ignore_ascii_case(id)),
            WorkKind::Branch => item.key.id == id || item.aliases.iter().any(|a| a == id),
        };
        let mut found: Vec<&WorkItem> = self.items.iter().filter(|item| project.is_none_or(|p| item.key.project.as_str() == p) && same(item, id)).collect();
        found.sort_by(|a, b| b.updated.cmp(&a.updated));
        found
    }

    /// The work of a session, whether it ended or not: its open work, else the newest it had.
    pub fn of_session_any(&self, session: &str) -> Option<&WorkItem> {
        self.of_session(session).or_else(|| self.items.iter().filter(|item| item.sessions.iter().any(|s| s == session)).max_by(|a, b| a.updated.cmp(&b.updated)))
    }

    /// The person's word on a piece of work (`work done`, `work drop`, `work reopen`): its state
    /// now, its `work.changed` event, and the state it had. Err: why nothing changed (it is done
    /// already, or open already).
    pub fn mark(&mut self, key: &WorkKey, to: Mark, now_ms: u64, machine: &MachineId) -> Result<(WorkState, Event), String> {
        let at = self.items.iter().position(|item| item.key == *key).ok_or_else(|| format!("no work {}", key.id))?;
        let before = self.items[at].state.clone();
        match to {
            Mark::Done if before == WorkState::Done => return Err(format!("{} is done already", key.id)),
            Mark::Dropped if before == WorkState::Dropped => return Err(format!("{} was dropped already", key.id)),
            Mark::Open if !before.ended() => return Err(format!("{} is not done or dropped: it is {}", key.id, before.words())),
            _ => {}
        }
        if to == Mark::Done {
            self.confirm(key);
        }
        let item = &mut self.items[at];
        item.state = match to {
            Mark::Done => WorkState::Done,
            Mark::Dropped => WorkState::Dropped,
            Mark::Open => {
                // Back to what its facts say, without the person's word.
                item.facts.person_confirmed = false;
                if std::mem::take(&mut item.facts.person_added) {
                    item.done.retain(|c| *c != DoneCondition::Person);
                }
                item.state = WorkState::Open;
                item.evaluate(now_ms)
            }
        };
        item.updated = stamp(machine);
        let session = item.sessions.last().cloned();
        let event = changed_event(item, &before, session.as_deref());
        Ok((before, event))
    }
}

/// What a server session's pull request at a glance says (`RemoteSession::pr`, which the server
/// refreshes every minute), for the Mac's work item of it (M32's Server line): its repository
/// (from its address, else its project's), number, state, draft, CI (no head: the glance has
/// none), and whether someone reviewed it (approved or changes asked: a review was asked for).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerPr {
    pub repo: String,
    pub number: u64,
    pub state: String,
    pub draft: bool,
    pub ci: Option<(String, String)>,
    pub reviewed: bool,
}

impl ServerPr {
    pub fn of(pr: &crate::github::PrSummary, project_repo: Option<&str>) -> Option<ServerPr> {
        let repo = crate::github::repo_of_pr_url(&pr.url).or_else(|| project_repo.map(str::to_string))?.to_lowercase();
        let state = if pr.state.trim().is_empty() { "OPEN".to_string() } else { pr.state.to_uppercase() };
        let ci = matches!(pr.ci.as_str(), "passed" | "failed" | "running").then(|| (String::new(), pr.ci.clone()));
        Some(ServerPr { repo, number: pr.number, state, draft: pr.draft, ci, reviewed: matches!(pr.review.as_str(), "approved" | "changes_requested") })
    }
}

/// The Mac's project for a project of the server: the same id, else the same name (a project
/// copied to the server keeps its name).
pub fn mac_project_of<'a>(mac: &'a [crate::model::Project], server: &[crate::model::Project], server_project: &str) -> Option<&'a crate::model::Project> {
    let theirs = server.iter().find(|p| p.id == server_project)?;
    mac.iter().find(|p| p.id == theirs.id).or_else(|| mac.iter().find(|p| p.name == theirs.name))
}

/// What a status round's pull request says of reviews (M32's `ReviewRequested` and
/// `ReviewPosted`): whether one is asked for (a review is requested of someone, or someone other
/// than its author reviewed it), and whether `mine` (the session's own GitHub login) posted one
/// since `since_secs` (the session's start).
pub fn review_facts(pr: &crate::github::PullRequest, mine: Option<&str>, since_secs: u64) -> (bool, bool) {
    let requested = pr.review_requests > 0 || pr.reviews.iter().any(|r| !r.login.is_empty() && !r.login.eq_ignore_ascii_case(&pr.author));
    let posted = mine.filter(|me| !me.is_empty()).is_some_and(|me| {
        pr.reviews.iter().any(|r| r.login.eq_ignore_ascii_case(me) && r.state != "PENDING" && crate::github::parse_time(&r.submitted_at).is_some_and(|at| at >= since_secs as i64))
    });
    (requested, posted)
}

/// The journal's events for review facts a status round found and the ledger has not yet: a
/// pull-request session's review asked for, a review session's review posted. Each is said once
/// (the ledger keeps the fact).
pub fn review_events(item: Option<&WorkItem>, session: &str, project: Option<&ProjectKey>, repo: &str, number: u64, requested: bool, posted: bool) -> Vec<Event> {
    let Some(item) = item else { return Vec::new() };
    let mut out = Vec::new();
    let pr = format!("{repo}#{number}");
    if item.key.kind == WorkKind::Review {
        if posted && !item.facts.review_posted {
            out.push(Event::new(kinds::REVIEW_POSTED, Source::Forge, format!("{pr}: the review is posted")).session(Some(session)).project(project).data(json!({ "repo": repo, "number": number, "session": session })));
        }
    } else if requested && !item.facts.review_requested {
        out.push(Event::new(kinds::REVIEW_REQUESTED, Source::Forge, format!("{pr}: a review is asked for")).session(Some(session)).project(project).data(json!({ "repo": repo, "number": number })));
    }
    out
}

/// What the person says of a piece of work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Done, whatever its conditions say.
    Done,
    /// Not to be done.
    Dropped,
    /// Not ended after all: its state is what its facts say again.
    Open,
}

impl WorkState {
    /// Its one word (`open`, `waiting`, `stuck`, `done`, `dropped`), as `work list --state` names it.
    pub fn name(&self) -> &'static str {
        match self {
            WorkState::Open => "open",
            WorkState::Waiting(_) => "waiting",
            WorkState::Stuck(_) => "stuck",
            WorkState::Done => "done",
            WorkState::Dropped => "dropped",
        }
    }

    /// Done or dropped: nothing evaluates it again.
    pub fn is_ended(&self) -> bool {
        self.ended()
    }
}

impl WorkItem {
    /// Its name in a reply: `ENG-42`, with its project when `with_project` (`app-3f9a1c:ENG-42`,
    /// what `work show` takes back).
    pub fn label(&self, with_project: bool) -> String {
        if with_project { format!("{}:{}", self.key.project, self.key.id) } else { self.key.id.clone() }
    }

    /// One line of `work list`: `ENG-42 (ticket): waiting for CI · done when a pull request ready
    /// for review (not yet)`.
    pub fn line(&self, with_project: bool) -> String {
        let kind = match self.key.kind {
            WorkKind::Branch => "branch",
            WorkKind::Ticket => "ticket",
            WorkKind::Pr => "pull request",
            WorkKind::Review => "review",
        };
        let done = if self.done.is_empty() {
            "done when its pull request merges".to_string()
        } else {
            let each: Vec<String> = self.done.iter().map(|c| format!("{} ({})", c.words(), if self.met(c) { "yes" } else { "not yet" })).collect();
            format!("done when {}", each.join(", "))
        };
        format!("{} ({kind}): {} · {done}", self.label(with_project), self.state.words())
    }
}

impl WorkLedger {

    /// Fold one journal event into the facts of the work it is about. The memory changes it asks
    /// for come back (a branch rename moves the work file).
    pub fn observe(&mut self, event: &Event, machine: &MachineId) -> Vec<MemoryChange> {
        let mut out = Vec::new();
        let Some(session) = event.session.as_deref() else { return out };
        let Some(at) = self.index_of_session(session) else { return out };
        let data = &event.data;
        let text = |key: &str| data.get(key).and_then(Value::as_str).map(str::to_string);
        let item = &mut self.items[at];
        let f = &mut item.facts;
        match event.kind.as_str() {
            "turn.started" => {
                // The app observes a turn's start before the writer stamps it: its time is now then.
                f.open_turn_since = Some(event.at_or_now());
                f.open_turn = text("turn");
            }
            "turn.ended" => {
                // Reports are written after their transcript settles: one of the turn before may
                // come after the next turn started.
                let turn = text("turn");
                if f.open_turn.is_none() || turn.is_none() || f.open_turn == turn {
                    f.open_turn_since = None;
                    f.open_turn = None;
                }
                let end = data.get("end").and_then(|e| e.get("end")).and_then(Value::as_str).unwrap_or_default().to_string();
                f.failed_turns = if end == "failed" { f.failed_turns + 1 } else { 0 };
                f.last_question = data.get("question").is_some_and(|q| !q.is_null());
                f.last_turn = Some(match end.as_str() {
                    "completed" => "Completed".to_string(),
                    other => other.to_string(),
                });
            }
            "pr.opened" => {
                if let (Some(repo), Some(number)) = (text("repo"), data.get("number").and_then(Value::as_u64)) {
                    item.pr = Some((repo, number));
                }
                f.pr_state = Some("OPEN".into());
                f.pr_draft = data.get("draft").and_then(Value::as_bool).unwrap_or(false);
                f.ci = None;
            }
            "pr.changed" | "reconciled.pr" => {
                if let Some(state) = text("state") {
                    f.pr_state = Some(state.to_uppercase());
                }
                if let Some(draft) = data.get("draft").and_then(Value::as_bool) {
                    f.pr_draft = draft;
                }
            }
            "pr.merged" => {
                f.pr_state = Some("MERGED".into());
                f.pr_ended_ms.get_or_insert(event.at_or_now());
            }
            "pr.closed" => {
                f.pr_state = Some("CLOSED".into());
                f.pr_ended_ms.get_or_insert(event.at_or_now());
            }
            "ci.passed" | "ci.failed" | "ci.running" => {
                let now = event.kind.as_str().trim_start_matches("ci.").to_string();
                if now == "failed" && f.ci.as_deref() != Some("failed") {
                    f.fix_messages = 0;
                }
                f.ci = Some(now);
                f.ci_head = text("head");
            }
            "review.requested" => f.review_requested = true,
            "review.posted" => f.review_posted = true,
            "decision.made" if text("step").as_deref() == Some("ci_fix") && text("outcome").as_deref() == Some("sent") => f.fix_messages += 1,
            // The app's finish report: a turn that ends on a question waits for the person, one
            // that is done or goes on by itself does not (also for turns no hook opened).
            "session.finished" => f.last_question = data.get("outcome").and_then(Value::as_str) == Some("needs_you"),
            "session.reported" => {
                f.claimed = text("state");
                f.claim_told = false;
            }
            "op.done" => {
                let action = &data["action"];
                if action["op"].as_str() == Some("board.done")
                    && let Some(task) = action["args"]["task"].as_str()
                {
                    f.board_done.insert(task.to_string());
                }
            }
            "git.branch" if item.key.kind == WorkKind::Branch => {
                if let Some(branch) = text("branch").filter(|b| *b != item.key.id && b != "HEAD") {
                    let from = item.key.clone();
                    item.aliases.push(from.id.clone());
                    item.key = WorkKey { id: branch, ..from.clone() };
                    item.updated = stamp(machine);
                    out.push(MemoryChange::Move { from, to: item.key.clone() });
                }
            }
            _ => {}
        }
        out
    }

    /// What the last status round confirmed of a session's pull request (the change events miss
    /// one that existed before this run, or merged while Glasscode was closed): its number, state,
    /// draft and CI. True when a fact changed.
    pub fn set_pr_facts(&mut self, session: &str, repo: &str, number: u64, state: &str, draft: bool, ci: Option<(String, String)>) -> bool {
        let Some(at) = self.index_of_session(session) else { return false };
        let item = &mut self.items[at];
        let before = (item.pr.clone(), item.facts.pr_state.clone(), item.facts.pr_draft, item.facts.ci.clone(), item.facts.ci_head.clone());
        item.pr = Some((repo.to_lowercase(), number));
        item.facts.pr_state = Some(state.to_uppercase());
        if matches!(item.facts.pr_state.as_deref(), Some("MERGED" | "CLOSED")) {
            item.facts.pr_ended_ms.get_or_insert_with(crate::clock::now_ms);
        } else {
            item.facts.pr_ended_ms = None;
        }
        item.facts.pr_draft = draft;
        if let Some((head, overall)) = ci {
            if overall == "failed" && item.facts.ci.as_deref() != Some("failed") {
                item.facts.fix_messages = 0;
            }
            item.facts.ci = Some(overall);
            item.facts.ci_head = Some(head);
        }
        before != (item.pr.clone(), item.facts.pr_state.clone(), item.facts.pr_draft, item.facts.ci.clone(), item.facts.ci_head.clone())
    }

    /// What is next for each piece of open work: Today's "Coming up" (M32). First the work that
    /// waits on CI or a review, then the work whose done conditions are not all met, with the ones
    /// it still waits for (work with none waits for its pull request to merge, when it has one).
    /// Stuck work is "Needs you"'s, and so is work that waits for the person's answer; work its
    /// session is busy with shows as working in the sidebar.
    pub fn coming_up(&self) -> Vec<Next> {
        let mut waiting = Vec::new();
        let mut next = Vec::new();
        for item in &self.items {
            let session = item.sessions.last().cloned();
            match &item.state {
                WorkState::Waiting(who) if who == "CI" || who == "a review" => {
                    waiting.push(Next { key: item.key.clone(), session, pr: item.pr.clone(), when: if who == "CI" { "CI" } else { "Review" }, what: format!("waiting for {who}") });
                }
                WorkState::Open => {
                    let missing: Vec<String> = item.done.iter().filter(|c| !item.met(c)).map(DoneCondition::words).collect();
                    let what = match (missing.is_empty(), &item.pr) {
                        (false, _) => format!("next: {}", missing.join(", ")),
                        (true, Some(_)) if item.done.is_empty() => "next: its pull request merges".to_string(),
                        _ => continue,
                    };
                    next.push(Next { key: item.key.clone(), session, pr: item.pr.clone(), when: "Next", what });
                }
                _ => {}
            }
        }
        waiting.extend(next);
        waiting
    }

    /// The work whose memory goes to the archive (M24 step 2): its pull request merged or closed
    /// a week ago or more, and none of its sessions is open (`live`). Each once.
    pub fn due_for_archive(&self, now_ms: u64, live: &BTreeSet<String>) -> Vec<WorkKey> {
        self.items
            .iter()
            .filter(|item| !item.facts.archived && item.facts.pr_ended_ms.is_some_and(|at| now_ms.saturating_sub(at) >= ARCHIVE_AFTER_MS))
            .filter(|item| !item.sessions.iter().any(|s| live.contains(s)))
            .map(|item| item.key.clone())
            .collect()
    }

    /// The work's memory went to the archive (or did not: `false` lets it be tried again).
    pub fn set_archived(&mut self, key: &WorkKey, archived: bool) -> bool {
        let Some(item) = self.items.iter_mut().find(|item| item.key == *key) else { return false };
        let changed = item.facts.archived != archived;
        item.facts.archived = archived;
        changed
    }

    /// Work whose sessions are all gone (closed by the person) and is not done: dropped, with its
    /// event.
    pub fn drop_gone(&mut self, live: &BTreeSet<String>, machine: &MachineId) -> (Vec<Event>, Vec<MemoryChange>) {
        let mut events = Vec::new();
        let mut writes = Vec::new();
        for item in &mut self.items {
            // An item the other home changed last is its to drop (M57): its sessions may run there.
            if item.state.ended() || item.sessions.is_empty() || item.sessions.iter().any(|s| live.contains(s)) || &item.updated.machine != machine {
                continue;
            }
            let before = std::mem::replace(&mut item.state, WorkState::Dropped);
            item.updated = stamp(machine);
            events.push(changed_event(item, &before, item.sessions.last().map(String::as_str)));
            writes.push(MemoryChange::Write(item.key.clone()));
        }
        (events, writes)
    }

    /// Evaluate every open item at `now_ms`: the events of the states that changed (and of claims
    /// the facts do not back), and the work files to write.
    pub fn evaluate(&mut self, now_ms: u64, machine: &MachineId) -> (Vec<Event>, Vec<MemoryChange>) {
        let mut events = Vec::new();
        let mut writes = Vec::new();
        for item in &mut self.items {
            if item.state.ended() {
                continue;
            }
            let now = item.evaluate(now_ms);
            let session = item.sessions.last().cloned();
            if now != item.state {
                let before = std::mem::replace(&mut item.state, now.clone());
                item.updated = stamp(machine);
                events.push(changed_event(item, &before, session.as_deref()));
                writes.push(MemoryChange::Write(item.key.clone()));
            }
            // Claimed done, not verified: said once (the manager is woken for a disagreement only).
            if item.facts.claimed.as_deref() == Some("done") && !item.verified_done() && !item.facts.claim_told {
                item.facts.claim_told = true;
                let open: Vec<String> = item.done.iter().filter(|c| !item.met(c)).map(DoneCondition::words).collect();
                events.push(
                    Event::new(kinds::WORK_CLAIM_UNVERIFIED, Source::Manager, format!("{} says done; not yet: {}", item.key.id, if open.is_empty() { "its pull request has not merged".to_string() } else { open.join(", ") }))
                        .session(session.as_deref())
                        .project(Some(&item.key.project))
                        .data(json!({ "work": item.key, "open": open })),
                );
            }
        }
        (events, writes)
    }
}

fn changed_event(item: &WorkItem, before: &WorkState, session: Option<&str>) -> Event {
    let severity = match item.state {
        WorkState::Stuck(_) => crate::journal::Severity::Warn,
        WorkState::Waiting(ref who) if who == "you" => crate::journal::Severity::Notice,
        _ => crate::journal::Severity::Info,
    };
    Event::new(kinds::WORK_CHANGED, Source::Manager, format!("{}: {}", item.key.id, item.state.words()))
        .session(session)
        .project(Some(&item.key.project))
        .data(json!({ "work": item.key, "state": item.state, "before": before, "done": item.done }))
        .severity(severity)
}

// ----- the work file ---------------------------------------------------------------------------

/// The section of a work file Glasscode writes from the item.
pub const STATUS: &str = "Status";

/// The section of a work file Glasscode writes before Claude compacts a session's conversation:
/// its last turns, in a few lines (land-18).
pub const BEFORE_COMPACTION: &str = "Before compaction";

/// A work file with its "Before compaction" section written from `digest` (replaced each time),
/// the rest kept as it is; a work file is begun for a work that has none.
pub fn with_turns_section(existing: Option<&str>, key: &WorkKey, digest: &str) -> String {
    let mut doc = existing.map(Doc::parse).filter(|doc| memory_entry::is_work_of(doc, key)).unwrap_or_else(|| memory_entry::work_doc(key));
    let mut section = vec![Line::Text(format!("## {BEFORE_COMPACTION}")), Line::Text(String::new()), Line::Text("The last turns before the conversation was compacted, newest last:".into())];
    section.extend(digest.lines().map(|l| Line::Text(l.to_string())));
    section.push(Line::Text(String::new()));
    let heading = |line: &Line, name: &str| matches!(line, Line::Text(text) if text.trim() == format!("## {name}"));
    match doc.lines.iter().position(|line| heading(line, BEFORE_COMPACTION)) {
        Some(start) => {
            let end = doc.lines[start + 1..].iter().position(|line| matches!(line, Line::Text(text) if text.starts_with("## "))).map(|n| start + 1 + n).unwrap_or(doc.lines.len());
            doc.lines.splice(start..end, section);
        }
        None => {
            let at = doc.lines.iter().position(|line| heading(line, memory_entry::NOTES_FROM_SESSIONS)).unwrap_or(doc.lines.len());
            doc.lines.splice(at..at, section);
        }
    }
    doc.newline = true;
    doc.render()
}

/// The text of the sections Glasscode writes into a work file ("Status", "Before compaction"),
/// for a session's brief: where its work stands, as Glasscode sees it.
pub fn generated_sections(doc: &Doc) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in &doc.lines {
        if let Line::Text(text) = line {
            if let Some(name) = text.strip_prefix("## ") {
                inside = [STATUS, BEFORE_COMPACTION].contains(&name.trim());
                if inside {
                    out.push_str(&format!("\n### {}\n\n", name.trim()));
                }
                continue;
            }
            if inside && !text.trim().is_empty() {
                out.push_str(text);
                out.push('\n');
            }
        }
    }
    out
}

/// A work file from its item: the existing file's title, marker and "Notes from sessions" kept,
/// and its "Status" section written from the item (the state, what makes it done and whether each
/// holds, its pull request, ticket, board task and sessions). Not bullets, so a hand edit stays
/// apart from what Glasscode writes.
pub fn render_work_file(item: &WorkItem, existing: Option<&str>) -> String {
    let mut doc = existing.map(Doc::parse).filter(|doc| memory_entry::is_work_of(doc, &item.key)).unwrap_or_else(|| memory_entry::work_doc(&item.key));
    let mut status = vec![Line::Text(format!("## {STATUS}")), Line::Text(String::new()), Line::Text(format!("State: {}.", item.state.words()))];
    if item.done.is_empty() {
        status.push(Line::Text("Done when: its pull request merges.".into()));
    } else {
        let each: Vec<String> = item.done.iter().map(|c| format!("{} ({})", c.words(), if item.met(c) { "yes" } else { "not yet" })).collect();
        status.push(Line::Text(format!("Done when: {}.", each.join(", "))));
    }
    if let Some((repo, number)) = &item.pr {
        status.push(Line::Text(format!("Pull request: {repo}#{number}.")));
    }
    if let Some(ticket) = &item.ticket {
        status.push(Line::Text(format!("Ticket: {ticket}.")));
    }
    if let Some(task) = &item.board_task {
        status.push(Line::Text(format!("Board task: {task}.")));
    }
    if let Some(claimed) = &item.facts.claimed {
        status.push(Line::Text(format!("The session says: {claimed} (not evidence).")));
    }
    if !item.sessions.is_empty() {
        status.push(Line::Text(format!("Sessions: {}.", item.sessions.join(", "))));
    }
    if !item.aliases.is_empty() {
        status.push(Line::Text(format!("Was: {}.", item.aliases.join(", "))));
    }
    // The item's values (its links, names and states, from this Mac's ledger or the server's
    // through a sync) are written as one line each that never reads as an entry or a marker.
    for line in status.iter_mut().skip(1) {
        if let Line::Text(text) = line {
            *text = memory_entry::clean_text(text);
        }
    }
    status.push(Line::Text(String::new()));
    let heading = |line: &Line, name: &str| matches!(line, Line::Text(text) if text.trim() == format!("## {name}"));
    let start = doc.lines.iter().position(|line| heading(line, STATUS));
    match start {
        Some(start) => {
            let end = doc.lines[start + 1..].iter().position(|line| matches!(line, Line::Text(text) if text.starts_with("## "))).map(|n| start + 1 + n).unwrap_or(doc.lines.len());
            doc.lines.splice(start..end, status);
        }
        None => {
            let at = doc.lines.iter().position(|line| heading(line, memory_entry::NOTES_FROM_SESSIONS)).unwrap_or(doc.lines.len());
            doc.lines.splice(at..at, status);
        }
    }
    doc.newline = true;
    doc.render()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> MachineId {
        MachineId::parse("0123456789abcdef").unwrap()
    }

    fn at() -> Hlc {
        Hlc { wall_ms: 1, count: 0, machine: machine() }
    }

    fn project() -> ProjectKey {
        ProjectKey::parse("app-3f9a1c").unwrap()
    }

    fn session(id: &str, branch: &str) -> Session {
        serde_json::from_value(json!({ "id": id, "project_id": "p1", "worktree": "/w", "branch": branch, "title": "t", "agent": "claude", "created_at": 0 })).unwrap()
    }

    fn event(kind: crate::journal::EventKind, session: &str, data: Value) -> Event {
        Event::new(kind, Source::App, "x").session(Some(session)).data(data)
    }

    fn ledger_with(kind: WorkKind, id: &str) -> (WorkLedger, WorkKey) {
        let key = WorkKey { project: project(), kind, id: id.into() };
        let mut ledger = WorkLedger::default();
        assert!(ledger.ensure(key.clone(), &session("s1", "feat/login"), None, &machine()));
        (ledger, key)
    }

    /// Coming up: work waiting on CI or a review first, then what open work still waits for; not
    /// stuck work, nor work waiting for the person's answer or busy in its session.
    #[test]
    fn a_finish_report_that_asks_waits_for_the_person() {
        let (mut ledger, key) = ledger_with(WorkKind::Pr, "acme/app#42");
        ledger.observe(&event(kinds::SESSION_FINISHED, "s1", json!({ "outcome": "needs_you", "question": "Push it?" })), &machine());
        ledger.evaluate(0, &machine());
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Waiting("you".into()));
        ledger.observe(&event(kinds::SESSION_FINISHED, "s1", json!({ "outcome": "done", "question": "" })), &machine());
        ledger.evaluate(0, &machine());
        assert_ne!(ledger.item(&key).unwrap().state, WorkState::Waiting("you".into()));
    }

    #[test]
    fn coming_up_says_what_each_piece_of_work_waits_for() {
        let (mut ledger, key) = ledger_with(WorkKind::Pr, "acme/app#42");
        ledger.evaluate(0, &machine());
        let next = ledger.coming_up();
        assert_eq!(next.len(), 1);
        assert_eq!((next[0].key.clone(), next[0].session.as_deref(), next[0].when, next[0].what.as_str()), (key.clone(), Some("s1"), "Next", "next: CI green, a review requested"));
        ledger.observe(&event(kinds::CI_RUNNING, "s1", json!({ "head": "abc" })), &machine());
        ledger.evaluate(0, &machine());
        assert_eq!(ledger.coming_up()[0].what, "waiting for CI");
        assert_eq!(ledger.coming_up()[0].when, "CI");
        ledger.observe(&event(kinds::TURN_STARTED, "s1", json!({ "turn": "t1" })), &machine());
        ledger.evaluate(0, &machine());
        assert!(ledger.coming_up().is_empty(), "busy in its session");
        ledger.observe(&event(kinds::TURN_ENDED, "s1", json!({ "turn": "t1", "end": { "end": "completed" }, "question": "Push it?" })), &machine());
        ledger.evaluate(0, &machine());
        assert!(ledger.coming_up().is_empty(), "waits for the person: Needs you's");
        // A branch's work with no pull request has nothing to say yet.
        let (mut branch, _) = ledger_with(WorkKind::Branch, "feat/login");
        branch.evaluate(0, &machine());
        assert!(branch.coming_up().is_empty());
    }

    #[test]
    fn pr_session_done_when_ci_green_and_review_requested() {
        let (mut ledger, key) = ledger_with(WorkKind::Pr, "acme/app#42");
        assert_eq!(ledger.item(&key).unwrap().done, [DoneCondition::CiGreen, DoneCondition::ReviewRequested]);
        assert_eq!(ledger.item(&key).unwrap().pr, Some(("acme/app".to_string(), 42)));
        ledger.observe(&event(kinds::CI_RUNNING, "s1", json!({ "head": "abc" })), &machine());
        let (events, _) = ledger.evaluate(0, &machine());
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Waiting("CI".into()));
        assert_eq!(events.len(), 1);
        ledger.observe(&event(kinds::CI_PASSED, "s1", json!({ "head": "abc" })), &machine());
        ledger.evaluate(0, &machine());
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Open, "CI green, no review yet");
        // The session says it is done: a claim, not evidence; said once.
        ledger.observe(&event(kinds::SESSION_REPORTED, "s1", json!({ "state": "done" })), &machine());
        let (events, _) = ledger.evaluate(0, &machine());
        assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["work.claim_unverified"]);
        assert!(ledger.evaluate(0, &machine()).0.is_empty());
        ledger.observe(&Event::new(crate::journal::EventKind::named("review.requested"), Source::Forge, "x").session(Some("s1")), &machine());
        let (events, writes) = ledger.evaluate(0, &machine());
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Done);
        assert_eq!((events[0].kind.as_str(), writes), ("work.changed", vec![MemoryChange::Write(key.clone())]));
        // Done stays done; the session's later work is another item's.
        assert!(ledger.of_session("s1").is_none());
    }

    /// M56.2: the follow-ups sync both ways by their own stamps. A change through `follow_edit`
    /// is stamped (and one with no stamp yet); what came from the other home does not go back; the
    /// newer stamp wins, with its "fixed" mark; an older or too-far-ahead one is not taken.
    #[test]
    fn follow_ups_both_ways() {
        use crate::manager::{FollowState, FollowUp};
        let (mac, server) = (machine(), MachineId::parse("00000000000000bb").unwrap());
        let follow = |id: &str| FollowUp { id: id.into(), repo: "acme/app".into(), number: 7, ..FollowUp::default() };
        let mut ledger = WorkLedger { follow_ups: vec![follow("fu-a")], ..WorkLedger::default() };
        assert!(ledger.follow_changed(&mac, None).is_empty(), "nothing stamped yet");
        ledger.follow_edit(&mac, |l| l.follow_ups.push(follow("fu-b")));
        let sent = ledger.follow_changed(&mac, None);
        assert_eq!(sent.iter().map(|f| f.follow.id.as_str()).collect::<Vec<_>>(), ["fu-a", "fu-b"], "the new one, and the one with no stamp");
        let last = sent.last().unwrap().stamp.clone();
        assert!(ledger.follow_changed(&mac, Some(&last)).is_empty(), "sent already");
        ledger.follow_edit(&mac, |_| ());
        assert!(ledger.follow_changed(&mac, Some(&last)).is_empty(), "no change, no stamp");

        // The server told "fixed" on fu-a: its newer record wins, with the mark.
        let mut on_server = WorkLedger::default();
        assert!(on_server.follow_merge(&sent));
        assert!(!on_server.follow_merge(&sent), "again, nothing");
        on_server.follow_edit(&server, |l| {
            l.follow_ups[0].state = FollowState::Fixed;
            l.follow_seen.insert("fixed|fu-a".into(), "bbb".into());
            l.follow_seen.insert("pr|acme/app#7".into(), "1".into());
        });
        let back = on_server.follow_changed(&server, None);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].fixed.as_deref(), Some("bbb"));
        assert!(ledger.follow_merge(&back));
        assert_eq!(ledger.follow_ups.iter().find(|f| f.id == "fu-a").map(|f| f.state), Some(FollowState::Fixed));
        assert_eq!(ledger.follow_seen.get("fixed|fu-a").map(String::as_str), Some("bbb"));
        assert!(!ledger.follow_seen.contains_key("pr|acme/app#7"), "the snapshot's keys stay where they were written");
        assert!(ledger.follow_changed(&mac, Some(&last)).is_empty(), "the server's change does not go back");

        // The Mac closes it later: the Mac's wins on the server; an older copy is not taken.
        ledger.follow_edit(&mac, |l| l.follow_ups[0].state = FollowState::Done);
        let closed = ledger.follow_changed(&mac, Some(&last));
        assert!(on_server.follow_merge(&closed));
        assert_eq!(on_server.follow_ups[0].state, FollowState::Done);
        assert!(!on_server.follow_merge(&back), "older");
        let mut ahead = back[0].clone();
        ahead.stamp = Hlc { wall_ms: crate::clock::now_ms() + 3 * 60 * 60 * 1000, count: 0, machine: server.clone() };
        assert!(!on_server.follow_merge(&[ahead]), "too far ahead");
        assert_eq!(on_server.follow_ups[0].state, FollowState::Done);

        // The server's clock is a minute ahead: its change comes stamped then. The Mac's next
        // change to it is stamped later still, so the server takes it; the Mac's stamps go on up.
        let ahead_by = Hlc { wall_ms: crate::clock::now_ms() + 60_000, count: 3, machine: server.clone() };
        let mut reopened = back[0].clone();
        reopened.follow.state = FollowState::Open;
        reopened.stamp = ahead_by.clone();
        assert!(ledger.follow_merge(std::slice::from_ref(&reopened)));
        let before = ledger.follow_changed(&mac, None).into_iter().map(|f| f.stamp).max().unwrap();
        ledger.follow_edit(&mac, |l| l.follow_ups.iter_mut().find(|f| f.id == "fu-a").unwrap().state = FollowState::Dropped);
        let after = ledger.follow_changed(&mac, Some(&before));
        assert_eq!(after.len(), 1);
        assert!(after[0].stamp > ahead_by, "{:?} after {:?}", after[0].stamp, ahead_by);
        assert!(on_server.follow_merge(std::slice::from_ref(&reopened)));
        assert!(on_server.follow_merge(&after), "the Mac's later change wins on the server");
        assert_eq!(on_server.follow_ups[0].state, FollowState::Dropped);
        ledger.follow_edit(&mac, |l| l.follow_ups.push(follow("fu-c")));
        assert!(ledger.follow_changed(&mac, Some(&after[0].stamp)).iter().any(|f| f.follow.id == "fu-c"), "a later change of the Mac's is still sent");
    }

    /// M57: work items sync both ways. The newer stamp wins an item, the facts join, a renamed
    /// branch's item is the same item, what came from the other home is not sent back, and a stamp
    /// too far ahead is quarantined.
    #[test]
    fn work_items_both_ways() {
        let (mac, server) = (machine(), MachineId::parse("00000000000000bb").unwrap());
        let stamp = |wall_ms: u64, machine: &MachineId| Hlc { wall_ms, count: 0, machine: machine.clone() };
        let key = WorkKey { project: project(), kind: WorkKind::Branch, id: "feat/login".into() };
        let mut ours = WorkItem::new(key.clone(), stamp(10, &mac));
        ours.sessions = vec!["s1".into()];
        ours.facts.review_requested = true;
        let mut ledger = WorkLedger { items: vec![ours], ..WorkLedger::default() };
        assert_eq!(ledger.own_changed(&mac, None).len(), 1);
        assert!(ledger.own_changed(&mac, Some(&stamp(10, &mac))).is_empty(), "sent already");

        // The server renamed the branch and saw the work done: its newer item wins, facts joined.
        let mut theirs = WorkItem::new(WorkKey { id: "feat/sign-in".into(), ..key.clone() }, stamp(20, &server));
        theirs.aliases = vec!["feat/login".into()];
        theirs.sessions = vec!["s9".into()];
        theirs.state = WorkState::Done;
        assert_eq!(ledger.merge_theirs(std::slice::from_ref(&theirs)).len(), 1);
        assert_eq!(ledger.items.len(), 1, "the same item under its new name");
        let item = ledger.item(&key).expect("still found by its old name");
        assert_eq!((item.key.id.as_str(), &item.state), ("feat/sign-in", &WorkState::Done));
        assert!(item.facts.review_requested, "what the Mac saw stays seen");
        assert_eq!(item.sessions, ["s1", "s9"]);
        assert!(ledger.own_changed(&mac, None).is_empty(), "the server's change does not go back");
        // Its sessions are not live here, and the Mac does not drop it: the server changed it last.
        let mut open = ledger.clone();
        open.items[0].state = WorkState::Open;
        assert!(open.drop_gone(&BTreeSet::new(), &mac).0.is_empty());

        // An older version, or one stamped too far ahead, is not taken.
        let mut stale = theirs.clone();
        stale.updated = stamp(15, &server);
        stale.state = WorkState::Open;
        assert!(ledger.merge_theirs(&[stale]).is_empty());
        let mut ahead = theirs.clone();
        ahead.updated = stamp(crate::clock::now_ms() + 3 * 60 * 60 * 1000, &server);
        ahead.state = WorkState::Dropped;
        assert!(ledger.merge_theirs(&[ahead]).is_empty());
        assert_eq!(ledger.items[0].state, WorkState::Done);
    }

    #[test]
    fn stuck_after_three_failures() {
        let (mut ledger, key) = ledger_with(WorkKind::Branch, "feat/login");
        let failed = json!({ "end": { "end": "failed", "detail": "rate_limit" }, "question": null });
        for n in 0..3 {
            ledger.observe(&event(kinds::TURN_STARTED, "s1", json!({})), &machine());
            ledger.observe(&event(kinds::TURN_ENDED, "s1", failed.clone()), &machine());
            ledger.evaluate(0, &machine());
            let stuck = matches!(ledger.item(&key).unwrap().state, WorkState::Stuck(_));
            assert_eq!(stuck, n == 2, "{n}");
        }
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Stuck("3 failed turns in a row".into()));
    }

    #[test]
    fn stuck_on_a_long_turn_and_on_red_ci_after_two_fixes() {
        let (mut ledger, key) = ledger_with(WorkKind::Pr, "acme/app#7");
        ledger.observe(&event(kinds::TURN_STARTED, "s1", json!({})), &machine());
        let started = ledger.item(&key).unwrap().facts.open_turn_since.unwrap();
        ledger.evaluate(started + LONG_TURN_MS - 1, &machine());
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Waiting("the session".into()));
        ledger.evaluate(started + LONG_TURN_MS, &machine());
        assert!(matches!(ledger.item(&key).unwrap().state, WorkState::Stuck(ref why) if why.contains("45 minutes")));

        let (mut ledger, key) = ledger_with(WorkKind::Pr, "acme/app#8");
        ledger.observe(&event(kinds::CI_FAILED, "s1", json!({ "head": "a" })), &machine());
        for _ in 0..2 {
            ledger.observe(&crate::journal::decision("ci_fix", "sent", "CI failed", json!({}), None).session(Some("s1")), &machine());
        }
        ledger.evaluate(0, &machine());
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Stuck("CI is still red after 2 fix messages".into()));
    }

    /// The app's work ledger sees each event before the journal's writer stamps it (`journal_note`):
    /// a turn that just started is not 45 minutes old, and a pull request that just merged did
    /// not end in 1970 (its work's memory would go to the archive at once). The live checks saw
    /// "stuck: working for 45 minutes with no turn end" seconds into every turn.
    #[test]
    fn an_unstamped_event_happened_now() {
        let (mut ledger, key) = ledger_with(WorkKind::Branch, "feat/login");
        let started = event(kinds::TURN_STARTED, "s1", json!({ "turn": "p1" }));
        assert_eq!(started.at_ms(), 0, "not stamped yet");
        let before = crate::clock::now_ms();
        ledger.observe(&started, &machine());
        assert!(ledger.item(&key).unwrap().facts.open_turn_since.is_some_and(|since| since >= before));
        ledger.evaluate(crate::clock::now_ms(), &machine());
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Waiting("the session".into()));
        ledger.observe(&event(kinds::PR_MERGED, "s1", json!({ "repo": "acme/app", "number": 7 })), &machine());
        assert!(ledger.item(&key).unwrap().facts.pr_ended_ms.is_some_and(|at| at >= before));
    }

    #[test]
    fn a_late_report_of_the_turn_before_does_not_end_the_next() {
        let (mut ledger, key) = ledger_with(WorkKind::Branch, "feat/login");
        ledger.observe(&event(kinds::TURN_STARTED, "s1", json!({ "turn": "p1" })), &machine());
        // A new prompt interrupted p1; p2 runs, and p1's report comes after it settled.
        ledger.observe(&event(kinds::TURN_STARTED, "s1", json!({ "turn": "p2" })), &machine());
        ledger.observe(&event(kinds::TURN_ENDED, "s1", json!({ "turn": "p1", "end": { "end": "interrupted" } })), &machine());
        assert_eq!(ledger.item(&key).unwrap().facts.open_turn.as_deref(), Some("p2"));
        assert!(ledger.item(&key).unwrap().facts.open_turn_since.is_some());
        ledger.observe(&event(kinds::TURN_ENDED, "s1", json!({ "turn": "p2", "end": { "end": "completed" } })), &machine());
        assert!(ledger.item(&key).unwrap().facts.open_turn_since.is_none());
    }

    #[test]
    fn the_status_round_sets_the_facts_the_events_missed() {
        let (mut ledger, key) = ledger_with(WorkKind::Branch, "feat/login");
        // A pull request merged while Glasscode was closed: no pr.merged event this run.
        assert!(ledger.set_pr_facts("s1", "Acme/App", 9, "merged", false, Some(("abc".into(), "passed".into()))));
        assert!(!ledger.set_pr_facts("s1", "Acme/App", 9, "MERGED", false, Some(("abc".into(), "passed".into()))));
        ledger.evaluate(0, &machine());
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Done);
    }

    #[test]
    fn work_of_closed_sessions_is_dropped() {
        let (mut ledger, key) = ledger_with(WorkKind::Branch, "feat/login");
        let live: BTreeSet<String> = ["s1".to_string()].into();
        assert!(ledger.drop_gone(&live, &machine()).0.is_empty());
        let (events, writes) = ledger.drop_gone(&BTreeSet::new(), &machine());
        assert_eq!((events.len(), writes), (1, vec![MemoryChange::Write(key.clone())]));
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Dropped);
        assert!(ledger.drop_gone(&BTreeSet::new(), &machine()).0.is_empty());
    }

    #[test]
    fn a_session_from_words_is_done_when_its_pr_merges() {
        let (mut ledger, key) = ledger_with(WorkKind::Branch, "feat/login");
        assert!(ledger.item(&key).unwrap().done.is_empty());
        ledger.observe(&event(kinds::PR_OPENED, "s1", json!({ "repo": "acme/app", "number": 9, "draft": false })), &machine());
        ledger.observe(&event(kinds::PR_MERGED, "s1", json!({ "repo": "acme/app", "number": 9 })), &machine());
        ledger.evaluate(0, &machine());
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Done);
    }

    #[test]
    fn work_file_generated_from_item() {
        let (mut ledger, key) = ledger_with(WorkKind::Pr, "acme/app#42");
        ledger.observe(&event(kinds::CI_PASSED, "s1", json!({ "head": "abc" })), &machine());
        ledger.evaluate(0, &machine());
        let item = ledger.item(&key).unwrap().clone();
        let first = render_work_file(&item, None);
        assert!(first.starts_with("# acme/app#42\n"), "{first}");
        assert!(first.contains("## Status\n\nState: open.\nDone when: CI green (yes), a review requested (not yet).\nPull request: acme/app#42.\nSessions: s1."), "{first}");
        // A session's note stays; the status is written again in its place.
        let noted = first.replace("## Notes from sessions\n", "## Notes from sessions\n\n- Use make setup. <!-- id=n1 status=learned kind=knowhow source=session:s1 -->\n");
        let mut later = item.clone();
        later.state = WorkState::Waiting("a review".into());
        let second = render_work_file(&later, Some(&noted));
        assert!(second.contains("State: waiting for a review.") && !second.contains("State: open."), "{second}");
        assert!(second.contains("- Use make setup. <!-- id=n1"), "{second}");
        assert_eq!(second.matches("## Status").count(), 1);
        assert!(memory_entry::is_work_of(&Doc::parse(&second), &key));
        // Another work's file is not taken over.
        let other = render_work_file(&later, Some("# x\n<!-- work project=other-000000 kind=pr id=x -->\n"));
        assert!(memory_entry::is_work_of(&Doc::parse(&other), &key));
    }

    #[test]
    fn follow_up_maps_to_work_item() {
        let follow = crate::manager::FollowUp { id: "f1".into(), repo: "Acme/App".into(), number: 12, login: "sam".into(), person: "Sam".into(), session: Some("s9".into()), ..Default::default() };
        let item = WorkItem::from_follow_up(&follow, &project(), at());
        assert_eq!(item.key, WorkKey { project: project(), kind: WorkKind::Pr, id: "acme/app#12".into() });
        assert_eq!((item.state.clone(), item.pr.clone(), item.sessions.clone(), item.done.clone()), (WorkState::Waiting("Sam".into()), Some(("acme/app".to_string(), 12)), vec!["s9".to_string()], vec![DoneCondition::Person]));
        let done = crate::manager::FollowUp { state: crate::manager::FollowState::Done, ..follow };
        assert_eq!(WorkItem::from_follow_up(&done, &project(), at()).state, WorkState::Done);
    }

    /// The person's word (`work done`, `drop`, `reopen`): done whatever the conditions say, and
    /// reopened back to what the facts say, without the condition the word added.
    #[test]
    fn the_persons_word_marks_work_and_reopen_takes_it_back() {
        let (mut ledger, key) = ledger_with(WorkKind::Pr, "acme/app#42");
        let (before, changed) = ledger.mark(&key, Mark::Done, 0, &machine()).unwrap();
        assert_eq!((before, ledger.item(&key).unwrap().state.clone(), changed.kind.as_str()), (WorkState::Open, WorkState::Done, "work.changed"));
        assert!(ledger.item(&key).unwrap().done.contains(&DoneCondition::Person));
        assert!(ledger.mark(&key, Mark::Done, 0, &machine()).is_err(), "done already");
        let (before, _) = ledger.mark(&key, Mark::Open, 0, &machine()).unwrap();
        let item = ledger.item(&key).unwrap();
        assert_eq!((before, item.state.clone()), (WorkState::Done, WorkState::Open));
        assert_eq!(item.done, [DoneCondition::CiGreen, DoneCondition::ReviewRequested], "the word's condition went with it");
        assert!(!item.facts.person_confirmed);
        assert!(ledger.mark(&key, Mark::Open, 0, &machine()).is_err(), "open already");
        // Dropped, and reopened to what its facts say: CI ran meanwhile.
        ledger.observe(&event(kinds::CI_RUNNING, "s1", json!({ "head": "a" })), &machine());
        ledger.mark(&key, Mark::Dropped, 0, &machine()).unwrap();
        assert!(ledger.of_session("s1").is_none(), "dropped work is no session's open work");
        assert_eq!(ledger.of_session_any("s1").unwrap().key, key);
        ledger.mark(&key, Mark::Open, 0, &machine()).unwrap();
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Waiting("CI".into()));
        // A condition the work started with stays when it is reopened.
        let follow = crate::manager::FollowUp { id: "f1".into(), repo: "acme/app".into(), number: 3, ..Default::default() };
        let mut ledger = WorkLedger { items: vec![WorkItem::from_follow_up(&follow, &project(), at())], ..WorkLedger::default() };
        let key = ledger.items[0].key.clone();
        ledger.mark(&key, Mark::Done, 0, &machine()).unwrap();
        ledger.mark(&key, Mark::Open, 0, &machine()).unwrap();
        assert_eq!(ledger.item(&key).unwrap().done, [DoneCondition::Person]);
    }

    /// `work show <word>`: an id in any case for tickets and pull requests, an old branch name,
    /// and `<project>:<id>` where two projects have the same id.
    #[test]
    fn work_is_named_by_its_id() {
        let (mut ledger, key) = ledger_with(WorkKind::Ticket, "ENG-42");
        assert_eq!(ledger.named("eng-42").iter().map(|i| &i.key).collect::<Vec<_>>(), [&key]);
        let other = WorkKey { project: ProjectKey::parse("web-001122").unwrap(), kind: WorkKind::Ticket, id: "ENG-42".into() };
        ledger.ensure(other.clone(), &session("s2", "x"), None, &machine());
        assert_eq!(ledger.named("ENG-42").len(), 2);
        assert_eq!(ledger.named("web-001122:ENG-42").iter().map(|i| &i.key).collect::<Vec<_>>(), [&other]);
        assert_eq!(ledger.named(&ledger.item(&key).unwrap().label(true)).len(), 1);
        assert!(ledger.named("ENG-43").is_empty());
        let (mut branches, branch) = ledger_with(WorkKind::Branch, "feat/login");
        branches.observe(&event(kinds::GIT_BRANCH, "s1", json!({ "branch": "feat/sign-in" })), &machine());
        assert_eq!(branches.named("feat/login")[0].key.id, "feat/sign-in");
        assert!(branches.named("FEAT/LOGIN").is_empty(), "branches are named exactly");
        assert!(branches.item(&branch).unwrap().line(false).starts_with("feat/sign-in (branch): open · done when its pull request merges"));
    }

    /// M32's review facts from a status round: asked for by a request or another's review, and a
    /// review session's own review posted since it started; each said once.
    #[test]
    fn reviews_asked_for_and_posted_come_from_the_status_round() {
        let gh: crate::github::PullRequest = serde_json::from_value(json!({
            "number": 42, "title": "x", "url": "u", "author": { "login": "dev" },
            "reviewRequests": [{ "login": "sam" }], "latestReviews": [{ "author": { "login": "dev" }, "state": "COMMENTED", "submittedAt": "2026-11-20T10:00:00Z" }]
        }))
        .unwrap();
        assert_eq!((gh.author.as_str(), gh.review_requests, gh.reviews.len()), ("dev", 1, 1));
        assert_eq!(review_facts(&gh, None, 0), (true, false));
        // GitHub's own shape, and the shape kept on disk, read alike.
        let graphql: crate::github::PullRequest = serde_json::from_value(json!({
            "number": 42, "title": "x", "url": "u", "author": { "login": "dev" }, "reviewRequests": { "totalCount": 0 },
            "latestReviews": { "nodes": [{ "author": { "login": "me" }, "state": "APPROVED", "submittedAt": "2026-11-20T10:00:00Z" }] }
        }))
        .unwrap();
        let kept: crate::github::PullRequest = serde_json::from_value(serde_json::to_value(&graphql).unwrap()).unwrap();
        assert_eq!(kept, graphql);
        let at = crate::github::parse_time("2026-11-20T10:00:00Z").unwrap() as u64;
        assert_eq!(review_facts(&graphql, Some("ME"), at), (true, true), "another's review is a review asked for; mine counts since the start");
        assert_eq!(review_facts(&graphql, Some("me"), at + 1), (true, false), "a review from before the session is not its review");
        let alone = crate::github::PullRequest { author: "dev".into(), ..Default::default() };
        assert_eq!(review_facts(&alone, Some("me"), 0), (false, false));
        // Said once: the ledger keeps the fact the event sets.
        let (mut ledger, key) = ledger_with(WorkKind::Pr, "acme/app#42");
        let events = review_events(ledger.item(&key), "s1", None, "acme/app", 42, true, true);
        assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["review.requested"]);
        ledger.observe(&events[0], &machine());
        assert!(review_events(ledger.item(&key), "s1", None, "acme/app", 42, true, true).is_empty());
        let (reviews, review) = ledger_with(WorkKind::Review, "acme/app#7");
        assert_eq!(review_events(reviews.item(&review), "s1", None, "acme/app", 7, true, true).iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["review.posted"]);
    }

    /// land-18: the last turns go in their own section, written again in its place; the notes of
    /// sessions stay; the brief gets the sections Glasscode writes.
    #[test]
    fn the_turns_before_compaction_have_their_section() {
        let (_, key) = ledger_with(WorkKind::Branch, "feat/login");
        let first = with_turns_section(None, &key, "- completed; ran `cargo fmt`");
        assert!(first.contains("## Before compaction\n\nThe last turns before the conversation was compacted, newest last:\n- completed; ran `cargo fmt`\n"), "{first}");
        let noted = first.replace("## Notes from sessions\n", "## Notes from sessions\n\n- Use make setup. <!-- id=n1 status=learned kind=knowhow source=session:s1 -->\n");
        let second = with_turns_section(Some(&noted), &key, "- failed; ran `cargo test`");
        assert!(second.contains("ran `cargo test`") && !second.contains("cargo fmt"), "{second}");
        assert!(second.contains("- Use make setup. <!-- id=n1"));
        assert_eq!(second.matches("## Before compaction").count(), 1);
        let with_status = render_work_file(&WorkItem::new(key.clone(), at()), Some(&second));
        let sections = generated_sections(&Doc::parse(&with_status));
        assert!(sections.contains("### Status") && sections.contains("State: open.") && sections.contains("### Before compaction") && sections.contains("cargo test"), "{sections}");
        assert!(!sections.contains("make setup"), "a session's note is not Glasscode's");
    }

    /// M24 step 2: a merged or closed pull request with no open session for a week sends the
    /// work's memory to the archive, once; an open session or a reopened pull request holds it.
    #[test]
    fn work_of_a_pr_ended_a_week_ago_goes_to_the_archive() {
        let (mut ledger, key) = ledger_with(WorkKind::Branch, "feat/login");
        let merged = event(kinds::PR_MERGED, "s1", json!({ "repo": "acme/app", "number": 9 }));
        ledger.observe(&merged, &machine());
        let at = ledger.item(&key).unwrap().facts.pr_ended_ms.unwrap();
        let none: BTreeSet<String> = BTreeSet::new();
        let open: BTreeSet<String> = ["s1".to_string()].into();
        assert!(ledger.due_for_archive(at + ARCHIVE_AFTER_MS - 1, &none).is_empty(), "not a week yet");
        assert!(ledger.due_for_archive(at + ARCHIVE_AFTER_MS, &open).is_empty(), "its session is open");
        assert_eq!(ledger.due_for_archive(at + ARCHIVE_AFTER_MS, &none), [key.clone()]);
        assert!(ledger.set_archived(&key, true));
        assert!(ledger.due_for_archive(at + 2 * ARCHIVE_AFTER_MS, &none).is_empty(), "once");
        // A status round that finds the pull request open again forgets when it ended.
        let (mut ledger, key) = ledger_with(WorkKind::Branch, "feat/x");
        ledger.set_pr_facts("s1", "acme/app", 3, "CLOSED", false, None);
        assert!(ledger.item(&key).unwrap().facts.pr_ended_ms.is_some());
        ledger.set_pr_facts("s1", "acme/app", 3, "OPEN", false, None);
        assert!(ledger.item(&key).unwrap().facts.pr_ended_ms.is_none());
    }

    /// A server session's work, on the Mac: its project is the Mac's of the same id or name, its
    /// pull request's glance gives the facts (CI with no head), and a review given counts as one
    /// asked for.
    #[test]
    fn a_server_sessions_pull_request_gives_its_facts() {
        let project = |id: &str, name: &str| -> crate::model::Project { serde_json::from_value(json!({ "id": id, "name": name, "root": format!("/w/{name}") })).unwrap() };
        let mac = [project("p1", "app"), project("p2", "web")];
        let server = [project("s9", "web"), project("p1", "app")];
        assert_eq!(mac_project_of(&mac, &server, "s9").map(|p| p.id.as_str()), Some("p2"), "by name");
        assert_eq!(mac_project_of(&mac, &server, "p1").map(|p| p.id.as_str()), Some("p1"), "by id");
        assert!(mac_project_of(&mac, &server, "nope").is_none());
        let glance = crate::github::PrSummary { number: 42, url: "https://github.com/Acme/App/pull/42".into(), state: "OPEN".into(), ci: "passed".into(), review: "approved".into(), ..Default::default() };
        let pr = ServerPr::of(&glance, None).unwrap();
        assert_eq!(pr, ServerPr { repo: "acme/app".into(), number: 42, state: "OPEN".into(), draft: false, ci: Some((String::new(), "passed".into())), reviewed: true });
        let (mut ledger, key) = ledger_with(WorkKind::Pr, "acme/app#42");
        assert!(ledger.set_pr_facts("s1", &pr.repo, pr.number, &pr.state, pr.draft, pr.ci.clone()));
        for event in review_events(ledger.item(&key), "s1", None, &pr.repo, pr.number, pr.reviewed, false) {
            ledger.observe(&event, &machine());
        }
        ledger.evaluate(0, &machine());
        assert_eq!(ledger.item(&key).unwrap().state, WorkState::Done);
        let none = crate::github::PrSummary { number: 7, url: String::new(), ci: "none".into(), review: "waiting".into(), ..Default::default() };
        assert_eq!(ServerPr::of(&none, Some("acme/web")).map(|p| (p.repo, p.state, p.ci, p.reviewed)), Some(("acme/web".into(), "OPEN".into(), None, false)));
        assert!(ServerPr::of(&none, None).is_none(), "no repository to name");
    }

    #[test]
    fn a_branch_rename_moves_the_item() {
        let (mut ledger, key) = ledger_with(WorkKind::Branch, "feat/login");
        let changes = ledger.observe(&event(kinds::GIT_BRANCH, "s1", json!({ "branch": "feat/sign-in", "before": "feat/login" })), &machine());
        let moved = WorkKey { id: "feat/sign-in".into(), ..key.clone() };
        assert_eq!(changes, [MemoryChange::Move { from: key.clone(), to: moved.clone() }]);
        assert_eq!(ledger.item(&moved).unwrap().aliases, ["feat/login"]);
        // The old id still finds it.
        assert_eq!(ledger.item(&key).unwrap().key, moved);
        assert_ne!(key.file_name(), moved.file_name());
        // A ticket's key does not change with its branch.
        let (mut tickets, ticket) = ledger_with(WorkKind::Ticket, "ENG-42");
        assert!(tickets.observe(&event(kinds::GIT_BRANCH, "s1", json!({ "branch": "other" })), &machine()).is_empty());
        assert!(tickets.item(&ticket).is_some());
    }
}
