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

use std::collections::BTreeSet;
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
        }
        item.facts.person_confirmed = true;
        true
    }

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
                f.open_turn_since = Some(event.at_ms());
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
            "pr.merged" => f.pr_state = Some("MERGED".into()),
            "pr.closed" => f.pr_state = Some("CLOSED".into()),
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

    /// Work whose sessions are all gone (closed by the person) and is not done: dropped, with its
    /// event.
    pub fn drop_gone(&mut self, live: &BTreeSet<String>, machine: &MachineId) -> (Vec<Event>, Vec<MemoryChange>) {
        let mut events = Vec::new();
        let mut writes = Vec::new();
        for item in &mut self.items {
            if item.state.ended() || item.sessions.is_empty() || item.sessions.iter().any(|s| live.contains(s)) {
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
        let mut ledger = WorkLedger { items: vec![ours] };
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
