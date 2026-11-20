//! When a session's pull request fails CI or conflicts with its base, Glasscode asks that
//! session's agent to fix it, by itself (the "Ask agent to fix" button, without the click).
//! What to ask and when is `glasscode_core::autofix`: once per commit for CI, once per pair of
//! head and base commits for conflicts (conflicts first, in one message with CI that failed on
//! the same commit), three asks in a row at most, then it is over to the person. Here:
//!
//! - only when the agent can take it: running, its turn over, not asking anything, its screen
//!   quiet, and nobody typing in it. Until then it waits (a closed session waits to be opened);
//! - never for review sessions (the PR is someone else's), handed-off sessions or shells.
//!
//! What was asked is kept in `~/.glasscode/ci-fix.json`, so a restart does not ask again.
//!
//! In a Manager v2 home this is the built-in playbook `ci-fix` (M40,
//! `glasscode_core::playbooks::builtins`): the same rules, its subject, group and effect key from
//! there. It stays app behaviour: it runs with the manager off, needs no approval, and a project's
//! level does not change it (exec-16). With the engine at v1 it runs as before.
//!
//! In a Manager v2 home (M31) every message Glasscode types into a session goes through the
//! session's delivery queue here (`glasscode_core::delivery_session`): `session send`, this CI fix
//! and the manager's messages. The queue is the one writer of a session's terminal; a delivery
//! waits until the session is idle, is typed with a marker, has its Enter held back when a prompt
//! came up, and counts as delivered when the session's hook event carries the marker. Each one an
//! agent, a rule or the manager set off is claimed in the effect ledger first
//! (`glasscode_core::effects`), and its outcome finishes the claim.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{AppContext as _, Context};
use glasscode_core::agents::AgentActivity;
use glasscode_core::autofix::{self, Enabled, Plan};
use glasscode_core::delivery_session::{self as delivery, Delivery, Inputs, Outcome, Pushed, Queues, Step};
use glasscode_core::effects::{self, Claim, EffectKey, EffectState, Intent};
use glasscode_core::model::AgentKind;

pub use glasscode_core::autofix::Record;

use crate::center::Center;
use crate::workspace::Workspace;

#[derive(Default)]
pub struct CiFix {
    records: HashMap<String, Record>,
    /// When a failure on a commit was first seen (waiting for the rest of CI).
    first_seen: HashMap<(String, String), Instant>,
    /// Closed sessions already flagged for a commit (or a pair of commits, for conflicts).
    flagged: HashSet<(String, String)>,
    /// v2: every session's delivery queue (M31).
    pub deliveries: Queues,
    /// Effects being claimed on a background thread, not to claim one twice from the tick.
    claiming: HashSet<EffectKey>,
    /// The claims earlier runs left without a result were settled in this run.
    reconciled: bool,
    /// v2: CI asks handed to the queue. One counts as asked once it is delivered; until it is over
    /// it is not planned again.
    asks: HashMap<EffectKey, PendingAsk>,
    /// Effects the ledger says may be claimed again only from this time (ms), not to claim them
    /// on every tick.
    not_before: HashMap<EffectKey, u64>,
    /// v2 (M40): the person's playbooks, run from the journal's new events.
    runner: Runner,
}

/// The playbook runner's place in the journal and what it last read.
#[derive(Default)]
struct Runner {
    cursor: Option<glasscode_core::journal::Cursor>,
    /// The playbooks that may fire, as last read (none: nothing to run, and nothing is read).
    live: Vec<glasscode_core::playbooks::Playbook>,
    read_at: Option<Instant>,
    busy: bool,
}

/// A CI ask waiting in the delivery queue (v2).
struct PendingAsk {
    /// `repo#number`, its record's key.
    key: String,
    ci: glasscode_core::github::CiStatus,
    ask: autofix::Ask,
    session: String,
    pr: u64,
}

fn path() -> std::path::PathBuf {
    glasscode_core::paths::root().join("ci-fix.json")
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl CiFix {
    pub fn load() -> CiFix {
        CiFix { records: std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default(), ..Default::default() }
    }

    fn save(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.records) {
            let _ = std::fs::write(path(), bytes);
        }
    }

    pub fn record(&self, key: &str) -> Option<Record> {
        self.records.get(key).cloned()
    }
}

/// What the Checks section says about it.
pub enum Note {
    Off,
    Asked { at: u64, tries: u32 },
    GaveUp,
    WaitingForCi,
    /// CI waits while the branch conflicts: the rebase makes a new commit.
    WaitingForConflicts,
    WaitingForAgent,
    WaitingForSession,
}

impl Workspace {
    fn autofix_enabled(&self) -> Enabled {
        Enabled { ci: !self.state.ci_autofix_off, conflicts: !self.state.conflict_autofix_off, tries: self.state.prefs.fix_tries() }
    }

    pub fn ci_fix_tick(&mut self, cx: &mut Context<Self>) {
        self.delivery_tick(cx);
        self.playbook_tick(cx);
        let enabled = self.autofix_enabled();
        if !enabled.ci && !enabled.conflicts {
            return;
        }
        let candidates: Vec<String> = self
            .state
            .sessions
            .iter()
            .filter(|s| s.agent != AgentKind::Shell && s.review.is_none() && s.team_task.is_none())
            .map(|s| s.id.clone())
            .collect();
        let mut changed = false;
        for id in candidates {
            // Only what GitHub confirmed in this run and still says (see `PrStatus::acts`).
            let Some(status) = self.prs.map.get(&id).filter(|s| s.acts()) else { continue };
            let (Some(repo), Some(pr), Some(ci)) = (status.repo.clone(), status.pr.clone(), status.ci.clone()) else { continue };
            let key = format!("{repo}#{}", pr.number);
            let mut record = self.ci_fix.record(&key).unwrap_or_default();
            if autofix::reset(&mut record, &ci) {
                self.ci_fix.records.insert(key.clone(), record.clone());
                changed = true;
            }
            // CI that failed waits for the rest of its checks, for a while.
            let ci_waited = ci.failed().is_empty() || record.head == ci.head || {
                let seen = *self.ci_fix.first_seen.entry((key.clone(), ci.head.clone())).or_insert_with(Instant::now);
                seen.elapsed() >= self.state.prefs.ci_wait()
            };
            let ask = match autofix::plan(&record, &ci, enabled, ci_waited) {
                Plan::Nothing | Plan::Later => continue,
                Plan::GiveUp { conflicts } => {
                    autofix::gave_up(&mut record, &ci, conflicts);
                    self.ci_fix.records.insert(key.clone(), record);
                    changed = true;
                    let text = if conflicts {
                        format!("PR #{} still conflicts with {} after {} asks to the agent: over to you.", pr.number, if ci.base.is_empty() { "its base" } else { &ci.base }, enabled.tries)
                    } else {
                        format!("CI still fails on PR #{} after {} fixes by the agent: over to you.", pr.number, enabled.tries)
                    };
                    self.autofix_over_to_you(&id, text, cx);
                    continue;
                }
                Plan::Ask(ask) => ask,
            };
            // v2: the delivery queue waits for the agent itself; only a closed session waits here.
            let v2 = self.control.engine_v2;
            let can_take = if v2 { self.terminals.contains_key(&id) } else { self.ci_fix_can_take(&id, cx) };
            if !can_take {
                // A closed session: flag it once, the ask goes out when it is opened.
                let about = if ask.conflicts { ci.merge_key() } else { ci.head.clone() };
                if !self.terminals.contains_key(&id) && self.ci_fix.flagged.insert((id.clone(), about)) {
                    self.attention.insert(id.clone());
                    cx.notify();
                }
                continue;
            }
            let text = autofix::message(&repo, pr.number, &pr.url, &ci, ask);
            if v2 {
                // The built-in playbook (M40): one ask per PR and commit (or pair of commits), its
                // effect key, and a group whose newer commit drops an older ask still waiting.
                use glasscode_core::playbooks::builtins::ci_fix as builtin;
                let subject = builtin::subject(&key, &ci, ask);
                let delivery = Delivery { group: Some(builtin::group(&key, ask)), version: Some(subject.clone()), ..Delivery::new(&id, &text, glasscode_core::words::InjectKind::Playbook, glasscode_core::clock::now_ms()) };
                let effect = builtin::key(&subject, &id);
                let now = glasscode_core::clock::now_ms();
                // Waiting in the queue, being claimed, or failed a moment ago: not again yet.
                if self.ci_fix.asks.contains_key(&effect) || self.ci_fix.claiming.contains(&effect) || self.ci_fix.not_before.get(&effect).is_some_and(|at| now < *at) {
                    continue;
                }
                let intent = builtin::intent(&subject, &id, pr.number, ask.conflicts, &self.control.launch);
                self.ci_fix.asks.insert(effect, PendingAsk { key: key.clone(), ci: ci.clone(), ask, session: id.clone(), pr: pr.number });
                // It counts as asked once it is delivered (`ci_ask_over`).
                self.queue_delivery(delivery, Some(intent), cx);
                continue;
            }
            if !self.type_into_session(&id, &text, glasscode_core::words::InjectKind::Playbook, cx) {
                continue;
            }
            autofix::asked(&mut record, &ci, ask, &id, unix_now());
            self.ci_fix.records.insert(key, record);
            changed = true;
            self.ci_fix_announce(&id, pr.number, ask.conflicts, &ci.base, cx);
        }
        if changed {
            self.ci_fix.save();
            cx.notify();
        }
    }

    /// An ask went to the agent: a toast, and a notification when the person is not looking at
    /// that session.
    fn ci_fix_announce(&mut self, id: &str, pr: u64, conflicts: bool, base: &str, cx: &mut Context<Self>) {
        let title = self.state.session(id).map(|s| s.title.clone()).unwrap_or_default();
        let what = if conflicts {
            format!("PR #{pr} conflicts with {}: asked the agent in \u{201c}{title}\u{201d} to resolve it.", if base.is_empty() { "its base" } else { base })
        } else {
            format!("CI failed on PR #{pr}: asked the agent in \u{201c}{title}\u{201d} to fix it.")
        };
        self.show_toast(what.clone(), cx);
        let watching = self.window_active && self.center == Center::Session && self.state.active_session.as_deref() == Some(id);
        if !watching && !self.state.prefs.notify_ci_off {
            if let Some(session) = self.state.session(id).cloned() {
                let heading = if conflicts { "Merge conflicts" } else { "CI failed" };
                self.notify_session(id, session.agent, heading.to_string(), what, false, cx);
            }
        }
    }

    /// v2: a CI ask in the queue is over. Delivered (or entered with no receipt), it counts as
    /// asked, as v1 counts one typed; otherwise it may be planned again (the ledger's backoff says
    /// when).
    fn ci_ask_over(&mut self, effect: &EffectKey, asked: bool, cx: &mut Context<Self>) {
        let Some(pending) = self.ci_fix.asks.remove(effect) else { return };
        if !asked {
            return;
        }
        let mut record = self.ci_fix.record(&pending.key).unwrap_or_default();
        autofix::asked(&mut record, &pending.ci, pending.ask, &pending.session, unix_now());
        self.ci_fix.records.insert(pending.key, record);
        self.ci_fix.save();
        self.ci_fix_announce(&pending.session, pending.pr, pending.ask.conflicts, &pending.ci.base, cx);
        cx.notify();
    }

    /// The agent has had its asks: say so in the sidebar and a toast, and as a notification
    /// when the person is not looking at that session.
    fn autofix_over_to_you(&mut self, id: &str, text: String, cx: &mut Context<Self>) {
        self.attention.insert(id.to_string());
        self.show_toast(text.clone(), cx);
        let watching = self.window_active && self.center == Center::Session && self.state.active_session.as_deref() == Some(id);
        if !watching && !self.state.notify.needs_input_off && !self.state.prefs.notify_ci_off {
            if let Some(session) = self.state.session(id).cloned() {
                self.notify_session(id, session.agent, "Over to you".to_string(), text, true, cx);
            }
        }
    }

    /// Whether the session's agent can be asked now without getting in anyone's way.
    fn ci_fix_can_take(&self, id: &str, cx: &mut Context<Self>) -> bool {
        let Some(terminal) = self.terminals.get(id) else { return false };
        let t = terminal.read(cx);
        if t.exit_code.is_some() {
            return false;
        }
        if t.last_typed.is_some_and(|at| at.elapsed() < Duration::from_secs(120)) || t.last_output.elapsed() < Duration::from_secs(8) {
            return false;
        }
        match self.activity.get(id) {
            Some(AgentActivity::Working) | Some(AgentActivity::NeedsInput) => return false,
            Some(AgentActivity::Idle) => {}
            // No hooks (Codex): a screen quiet for a while means its turn is over.
            None => {
                if t.last_output.elapsed() < Duration::from_secs(30) {
                    return false;
                }
            }
        }
        let tail = t.text_tail(60);
        if glasscode_core::prompt::parse(&tail).is_some() || has_draft(&t.text_tail_typed(60)) {
            return false;
        }
        true
    }

    /// The Checks section's line about CI, for a PR shown in the panel.
    pub fn ci_fix_note(&self, session_id: &str) -> Option<Note> {
        if self.state.ci_autofix_off {
            return Some(Note::Off);
        }
        let (record, ci) = self.autofix_subject(session_id)?;
        if ci.failed().is_empty() {
            return None;
        }
        Some(if record.head == ci.head {
            if record.gave_up { Note::GaveUp } else { Note::Asked { at: record.asked_at, tries: record.tries } }
        } else if !self.state.conflict_autofix_off && ci.conflicting() {
            Note::WaitingForConflicts
        } else if record.tries >= self.state.prefs.fix_tries() {
            Note::GaveUp
        } else if ci.running() {
            Note::WaitingForCi
        } else if !self.terminals.contains_key(session_id) {
            Note::WaitingForSession
        } else {
            Note::WaitingForAgent
        })
    }

    /// The Checks section's line about merge conflicts, for a PR GitHub says conflicts.
    pub fn conflict_fix_note(&self, session_id: &str) -> Option<Note> {
        if self.state.conflict_autofix_off {
            return Some(Note::Off);
        }
        let (record, ci) = self.autofix_subject(session_id)?;
        if !ci.conflicting() {
            return None;
        }
        Some(if record.conflict == ci.merge_key() {
            if record.conflict_gave_up { Note::GaveUp } else { Note::Asked { at: record.conflict_asked_at, tries: record.conflict_tries } }
        } else if record.conflict_tries >= self.state.prefs.fix_tries() {
            Note::GaveUp
        } else if !self.terminals.contains_key(session_id) {
            Note::WaitingForSession
        } else {
            Note::WaitingForAgent
        })
    }

    /// A session Glasscode may ask about its PR, with what it did so far and the PR's state.
    fn autofix_subject(&self, session_id: &str) -> Option<(Record, glasscode_core::github::CiStatus)> {
        let session = self.state.session(session_id)?;
        if session.agent == AgentKind::Shell || session.review.is_some() || session.team_task.is_some() {
            return None;
        }
        let status = self.prs.map.get(session_id).filter(|s| s.acts())?;
        let (repo, pr, ci) = (status.repo.clone()?, status.pr.clone()?, status.ci.clone()?);
        Some((self.ci_fix.record(&format!("{repo}#{}", pr.number)).unwrap_or_default(), ci))
    }

    /// Type a message into a session's agent and submit it; false when it cannot take one. It is
    /// recorded first as Glasscode's (`kind`), never the person's words (D1).
    pub fn type_into_session(&mut self, session_id: &str, message: &str, kind: glasscode_core::words::InjectKind, cx: &mut Context<Self>) -> bool {
        let Some(terminal) = self.terminals.get(session_id).cloned() else { return false };
        if glasscode_core::prompt::parse(&terminal.read(cx).text_tail(60)).is_some() {
            return false;
        }
        let line = message.split_whitespace().collect::<Vec<_>>().join(" ");
        glasscode_core::words::record(glasscode_core::words::Origin::Injected(kind), &line, Some(session_id), None);
        terminal.update(cx, |t, cx| t.write(line.into_bytes(), cx));
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
        })
        .detach();
        true
    }
}

// ----- v2: the delivery queue (M31) ----------------------------------------------------------

/// What an outcome leaves in the effect ledger.
fn state_of(outcome: &Outcome) -> EffectState {
    match outcome {
        Outcome::Delivered(value) => EffectState::Done(value.clone()),
        Outcome::NotSent(why) | Outcome::NotEntered(why) | Outcome::Dropped(why) => EffectState::Failed(why.clone()),
        Outcome::Uncertain(_) => EffectState::Uncertain,
    }
}

/// Finish a claim off the main thread; a failure is logged.
fn finish_claim(key: EffectKey, state: EffectState, cx: &mut Context<Workspace>) {
    cx.background_spawn(async move {
        if let Err(err) = effects::finish(&key, state) {
            log::error!("effects: the result of {key} could not be written ({err})");
        }
    })
    .detach();
}

impl Workspace {
    /// v2: queue a message for a session. With an intent (an agent, a rule or the manager set it
    /// off), its effect is claimed first, off the main thread: a key claimed before is not queued
    /// again.
    pub(crate) fn queue_delivery(&mut self, delivery: Delivery, intent: Option<Intent>, cx: &mut Context<Self>) {
        let Some(mut intent) = intent else {
            self.push_delivery(delivery, cx);
            return;
        };
        let key = intent.key;
        if !self.ci_fix.claiming.insert(key) {
            return;
        }
        let events = glasscode_core::agents::events_file(&delivery.session);
        let marker = delivery.marker();
        let task = cx.background_spawn(async move {
            // The receipt and the probe look after this length of the session's events file.
            let len = std::fs::metadata(&events).map(|m| m.len()).unwrap_or(0);
            intent.evidence = delivery::evidence(len, &marker);
            let home = glasscode_core::paths::MachineId::this().map_err(|err| std::io::Error::other(err.to_string()))?;
            effects::claim(&intent, &home)
        });
        cx.spawn(async move |this, cx| {
            let claimed = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.ci_fix.claiming.remove(&key);
                let now = glasscode_core::clock::now_ms();
                match claimed {
                    Ok(Claim::New(_)) => ws.push_delivery(Delivery { effect: Some(key), ..delivery }, cx),
                    Ok(Claim::Already(record)) => {
                        log::info!("effects: {} for {} was claimed before ({:?}); not typed again", record.op, record.target, record.state);
                        // Done or uncertain: it went (or the person decides). Failed: not before the
                        // ledger's backoff. Claimed by an earlier run: once that is settled.
                        let asked = match &record.state {
                            EffectState::Done(_) | EffectState::Uncertain => true,
                            EffectState::Failed(_) => match record.retry_at() {
                                Some(at) => {
                                    ws.ci_fix.not_before.insert(key, at);
                                    false
                                }
                                None => true,
                            },
                            EffectState::Claimed => {
                                ws.ci_fix.not_before.insert(key, now + 30_000);
                                false
                            }
                        };
                        ws.ci_ask_over(&key, asked, cx);
                    }
                    Err(err) => {
                        log::error!("effects: a message for {} was not queued: the effect ledger could not be written ({err})", delivery.session);
                        ws.ci_fix.not_before.insert(key, now + 60_000);
                        ws.ci_ask_over(&key, false, cx);
                    }
                }
            });
        })
        .detach();
    }

    fn push_delivery(&mut self, delivery: Delivery, cx: &mut Context<Self>) {
        match self.ci_fix.deliveries.push(delivery.clone()) {
            Pushed::Queued => {}
            Pushed::Same(first) => {
                if let Some(key) = delivery.effect {
                    finish_claim(key, EffectState::Done(serde_json::json!({ "same_as": first })), cx);
                    // The first one counts when it is delivered.
                    self.ci_ask_over(&key, false, cx);
                }
            }
            Pushed::Superseded(older) => {
                for old in older {
                    if let Some(key) = old.effect {
                        finish_claim(key, EffectState::Failed("a newer one took its place".into()), cx);
                        self.ci_ask_over(&key, false, cx);
                    }
                }
            }
        }
    }

    /// Every tick in a v2 home: settle what earlier runs left claimed (once), drop deliveries
    /// that waited too long, and type the next one of each session that can take it.
    fn delivery_tick(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 {
            return;
        }
        if !self.ci_fix.reconciled {
            self.ci_fix.reconciled = true;
            let launch = self.control.launch.clone();
            cx.background_spawn(async move { reconcile(&launch) }).detach();
        }
        for gone in self.ci_fix.deliveries.expire(glasscode_core::clock::now_ms()) {
            if let Some(key) = gone.effect {
                finish_claim(key, EffectState::Failed("it waited too long for the session".into()), cx);
                self.ci_ask_over(&key, false, cx);
            }
        }
        for session in self.ci_fix.deliveries.sessions() {
            let inputs = self.delivery_inputs(&session, cx);
            if let Step::Send(next) = self.ci_fix.deliveries.next(&session, &inputs) {
                self.deliver(next, cx);
            }
        }
    }

    /// What a session shows now, for its queue: no IO.
    fn delivery_inputs(&self, id: &str, cx: &mut Context<Self>) -> Inputs {
        let (terminal, activity) = match self.terminals.get(id) {
            Some(terminal) => (terminal, self.activity.get(id).copied()),
            None => match (self.server.terminals.get(id), self.server.session(id)) {
                (Some(terminal), Some(remote)) => (terminal, remote.activity),
                _ => return Inputs::default(),
            },
        };
        let t = terminal.read(cx);
        let quiet = t.last_output.elapsed();
        let busy = match activity {
            Some(AgentActivity::Working) | Some(AgentActivity::NeedsInput) => true,
            Some(AgentActivity::Idle) => quiet < Duration::from_secs(2),
            // No hooks (Codex): a screen quiet for a while means its turn is over.
            None => quiet < Duration::from_secs(30),
        };
        Inputs {
            running: t.exit_code.is_none(),
            busy,
            asking: glasscode_core::prompt::parse(&t.text_tail(60)).is_some(),
            draft: has_draft(&t.text_tail_typed(60)),
            typed_ms_ago: t.last_typed.map(|at| at.elapsed().as_millis() as u64),
        }
    }

    /// Type one delivery: check for a prompt again right before the paste and before the Enter,
    /// then wait for the session's hook event with the marker.
    fn deliver(&mut self, d: Delivery, cx: &mut Context<Self>) {
        let local = self.terminals.get(&d.session).cloned();
        let Some(terminal) = local.clone().or_else(|| self.server.terminals.get(&d.session).cloned()) else {
            self.delivery_over(d, Outcome::NotSent("the session is not open".into()), cx);
            return;
        };
        let agent = self.state.session(&d.session).map(|s| s.agent).or_else(|| self.server.session(&d.session).map(|r| r.session.agent)).unwrap_or(AgentKind::Claude);
        // `session send` to Claude is pasted with its line breaks, as v1 pasted it; everything else
        // (the CI fix, the manager's messages, Codex) is typed as one line, as v1 typed it.
        let paste = agent == AgentKind::Claude && d.note.is_some();
        let typed = if paste { d.typed() } else { d.typed().split_whitespace().collect::<Vec<_>>().join(" ") };
        // Only a session of this Mac has its events file here.
        let events = local.is_some().then(|| glasscode_core::agents::events_file(&d.session));
        cx.spawn(async move |this, cx| {
            let events_len = match events.clone() {
                Some(path) => cx.background_spawn(async move { std::fs::metadata(&path).map(|m| m.len()).ok() }).await,
                None => None,
            };
            let asking = this.update(cx, |_, cx| glasscode_core::prompt::parse(&terminal.read(cx).text_tail(60)).is_some()).unwrap_or(true);
            if asking {
                let _ = this.update(cx, |ws, cx| ws.delivery_over(d, Outcome::NotSent("a question came up on its screen".into()), cx));
                return;
            }
            // Glasscode's words, exactly as typed, recorded before they are (D1).
            glasscode_core::words::record(glasscode_core::words::Origin::Injected(d.kind), &typed, Some(&d.session), None);
            let pasted = typed.clone();
            let _ = terminal.update(cx, |t, cx| if paste { t.paste_text(&pasted, cx) } else { t.write(pasted.into_bytes(), cx) });
            cx.background_executor().timer(Duration::from_millis(250)).await;
            // land-m3: a prompt that came up after the paste, or a box holding something else, gets
            // no Enter.
            let may = this
                .update(cx, |_, cx| {
                    let t = terminal.read(cx);
                    let asking = glasscode_core::prompt::parse(&t.text_tail(60)).is_some();
                    // Claude's box must hold the text; an empty box, or none on screen, does not.
                    let held = (agent == AgentKind::Claude).then(|| prompt_box(&t.text_tail_typed(60)).unwrap_or_default());
                    delivery::may_press_enter(held.as_deref(), asking, &typed)
                })
                .unwrap_or(false);
            if !may {
                let _ = this.update(cx, |ws, cx| ws.delivery_over(d, Outcome::NotEntered("a question came up before the Enter, or its box did not hold the message; what was typed was left there".into()), cx));
                return;
            }
            let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
            // The receipt: the hook event with the marker. An idle Claude can lose an Enter, so it
            // is pressed again while the text still sits in its box.
            let marker = d.marker();
            let started = std::time::Instant::now();
            let mut presses = 0;
            let outcome = loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                if let (Some(path), Some(from)) = (events.clone(), events_len) {
                    let marker = marker.clone();
                    let found = cx
                        .background_spawn(async move {
                            let bytes = std::fs::read(&path).unwrap_or_default();
                            let from = usize::try_from(from).unwrap_or(usize::MAX).min(bytes.len());
                            delivery::probe(&marker, &String::from_utf8_lossy(&bytes[from..]))
                        })
                        .await;
                    if let glasscode_core::effects::ProbeResult::Happened(receipt) = found {
                        break Outcome::Delivered(receipt);
                    }
                }
                let elapsed = started.elapsed();
                if agent == AgentKind::Claude && presses < 3 && elapsed >= Duration::from_millis(600 * (presses + 1)) {
                    let still = this.update(cx, |_, cx| delivery::squeezed(&crate::chat_ui::claude_box(terminal.read(cx))) == delivery::squeezed(&typed)).unwrap_or(false);
                    if still {
                        let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
                    }
                    presses += 1;
                }
                if elapsed >= Duration::from_millis(delivery::RECEIPT_WAIT_MS) {
                    break match events_len {
                        // Hooks report, and none carried the marker.
                        Some(_) => Outcome::Uncertain(format!("no receipt in {} s", delivery::RECEIPT_WAIT_MS / 1000)),
                        // No hook events for it here (Codex, or a session on the server): typed and
                        // entered is all there is to know.
                        None => Outcome::Delivered(serde_json::json!({ "receipt": "none: typed and entered" })),
                    };
                }
            };
            let _ = this.update(cx, |ws, cx| ws.delivery_over(d, outcome, cx));
        })
        .detach();
    }

    /// A delivery is over: the next of its session may go. One not sent goes back to the front of
    /// its queue a few times; the rest finish their claims.
    fn delivery_over(&mut self, d: Delivery, outcome: Outcome, cx: &mut Context<Self>) {
        if let Outcome::NotSent(why) = &outcome {
            log::info!("delivery: not sent to {} ({why})", d.session);
            // Nothing was typed: it goes back to the front of its queue, a few times.
            if let Some(over) = self.ci_fix.deliveries.not_sent(&d.session, &d.id)
                && let Some(key) = over.effect
            {
                finish_claim(key, state_of(&outcome), cx);
                self.ci_ask_over(&key, false, cx);
            }
            return;
        }
        self.ci_fix.deliveries.finish(&d.session, &d.id);
        if let Some(key) = d.effect {
            finish_claim(key, state_of(&outcome), cx);
            self.ci_ask_over(&key, matches!(outcome, Outcome::Delivered(_) | Outcome::Uncertain(_)), cx);
        }
        cx.notify();
    }
}

/// A board move cut off by a restart, read back: the task is in the section it was moved to.
/// Only a task's move can be read back (an item's place is the board's marks).
fn probe_board_move(evidence: &serde_json::Value) -> glasscode_core::effects::ProbeResult {
    use glasscode_core::effects::ProbeResult;
    let (Some(key), Some(section)) = (evidence.get("key").and_then(|v| v.as_str()), evidence.get("section").and_then(|v| v.as_str())) else { return ProbeResult::Unknown };
    let Some(id) = key.strip_prefix("task:") else { return ProbeResult::Unknown };
    match glasscode_core::board::load().task(id) {
        Some(task) if task.list.section().key() == section => ProbeResult::Happened(serde_json::json!({ "read_back": section })),
        Some(_) => ProbeResult::DidNotHappen,
        None => ProbeResult::Unknown,
    }
}

/// Settle the claims earlier runs of the app left without a result: a message for a session by
/// its events file, anything else by its retry class (`effects::settle`). It reads and writes the
/// disk.
fn reconcile(launch: &str) {
    let cut = match effects::unsettled(launch) {
        Ok(cut) => cut,
        Err(err) => {
            log::error!("effects: the ledger could not be read ({err}); claims cut off by a restart stay open");
            return;
        }
    };
    for record in cut {
        let probe = match record.op.as_str() {
            "session.send" => record.session.as_deref().and_then(|session| delivery::probe_claim(session, &record.evidence)),
            "board.move" => Some(probe_board_move(&record.evidence)),
            _ => None,
        };
        let state = effects::settle(&record, probe);
        if let Err(err) = effects::finish(&record.key, state) {
            log::error!("effects: {} could not be settled ({err})", record.key);
        }
    }
}

// ----- v2: the person's playbooks (M40) -------------------------------------------------------

/// How often the runner reads the journal's new events, and the playbooks.
const RUNNER_EVERY: Duration = Duration::from_secs(2);
const BOOK_EVERY: Duration = Duration::from_secs(30);

impl Workspace {
    /// Every tick in a v2 home: the journal's new events through the engine, off the main thread;
    /// each firing claimed in the effect ledger and run through the gate as the manager's wake.
    /// The built-ins are not run here: the CI fix is the code above, and the manager's own run with
    /// the manager (v1's harness until M51).
    fn playbook_tick(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 || !self.journal.on || self.ci_fix.runner.busy {
            return;
        }
        let runner = &mut self.ci_fix.runner;
        let reread = runner.read_at.is_none_or(|at| at.elapsed() >= BOOK_EVERY);
        if !reread && runner.read_at.is_some_and(|at| at.elapsed() < RUNNER_EVERY) && runner.cursor.is_some() {
            return;
        }
        runner.busy = true;
        let cursor = runner.cursor.clone();
        let live = runner.live.clone();
        let policy = self.control.authority.policy.clone();
        let confirmed: Vec<String> = self.state.manager.people.iter().map(|p| p.github.clone()).filter(|login| !login.is_empty()).collect();
        let task = cx.background_spawn(async move {
            use glasscode_core::playbooks::{self, engine, limits::LedgerCounts, model};
            let schema = model::journal_schema();
            let live = if reread {
                match playbooks::load(&schema) {
                    Ok(book) => book.live(),
                    Err(err) => {
                        log::warn!("playbooks: {err}; the person's playbooks do not run until it is repaired");
                        Vec::new()
                    }
                }
            } else {
                live
            };
            // The first round starts at the journal's end: old events are not acted on.
            let Some(cursor) = cursor else { return (live, Some(glasscode_core::journal::end()), Vec::new()) };
            let (events, next) = glasscode_core::journal::read_from(&cursor, 500);
            if live.is_empty() || events.is_empty() {
                return (live, Some(next), Vec::new());
            }
            let mut counts = glasscode_core::effects::load().map(|ledger| LedgerCounts::of(&ledger)).unwrap_or_default();
            let level = |key: Option<&glasscode_core::paths::ProjectKey>| key.map_or(policy.default.level, |key| policy.of(key).level);
            let mut firings = Vec::new();
            for event in &events {
                let facts = engine::Facts { level: &level, me: &[], confirmed: &confirmed, now_ms: glasscode_core::clock::now_ms() };
                let (fired, held) = engine::fire(&live, event, &schema, &facts, &counts);
                for h in held {
                    glasscode_core::journal::append(glasscode_core::journal::decision("playbook", "held", &h.why.words(), serde_json::json!({ "playbook": h.playbook, "event": event.id }), event.trace.as_deref()));
                }
                for firing in fired {
                    // A firing counts for the limits of the rest of this round too.
                    counts.add(glasscode_core::effects::EffectRecord {
                        v: glasscode_core::effects::SCHEMA,
                        key: firing.key,
                        op: firing.op.clone(),
                        retry: glasscode_core::ops::Retry::Verifiable,
                        origin: playbooks::limits::origin_of(&firing.playbook),
                        subject: firing.subject.clone(),
                        target: firing.target.clone(),
                        caller: String::new(),
                        reason: String::new(),
                        project: None,
                        session: None,
                        home: glasscode_core::paths::MachineId::parse("this").expect("an id"),
                        generation: 0,
                        launch: String::new(),
                        claimed_ms: glasscode_core::clock::now_ms(),
                        attempt: 1,
                        evidence: serde_json::Value::Null,
                        state: EffectState::Claimed,
                        finished_ms: None,
                    });
                    firings.push(firing);
                }
            }
            (live, Some(next), firings)
        });
        cx.spawn(async move |this, cx| {
            let (live, cursor, firings) = task.await;
            let _ = this.update(cx, |ws, cx| {
                let runner = &mut ws.ci_fix.runner;
                runner.busy = false;
                if reread || runner.read_at.is_none() {
                    runner.live = live;
                }
                runner.read_at = Some(Instant::now());
                runner.cursor = cursor;
                for firing in firings {
                    ws.playbook_fire(firing, cx);
                }
            });
        })
        .detach();
    }

    /// One firing: watch records it, tell tells the person, ask and act claim the effect and run
    /// the op through the gate as the manager's wake (the project's level decides whether a card
    /// asks first). The playbooks that gave way to it are recorded as fired too, for their limits.
    fn playbook_fire(&mut self, firing: glasscode_core::playbooks::engine::Firing, cx: &mut Context<Self>) {
        use glasscode_core::playbooks::Autonomy;
        let what = format!("{} {}", firing.op, firing.args);
        match firing.autonomy {
            Autonomy::Watch => {
                glasscode_core::journal::append(glasscode_core::journal::decision("playbook", "watched", &what, serde_json::json!({ "playbook": firing.playbook, "event": firing.event }), None));
                return;
            }
            Autonomy::Tell => {
                self.show_toast(format!("Playbook “{}”: {what}", firing.playbook), cx);
                glasscode_core::journal::append(glasscode_core::journal::decision("playbook", "told", &what, serde_json::json!({ "playbook": firing.playbook, "event": firing.event }), None));
                return;
            }
            Autonomy::Ask | Autonomy::Act => {}
        }
        let op = match glasscode_core::ops::typed(&firing.op, firing.args.clone()) {
            Ok(op) => op,
            Err(err) => {
                log::warn!("playbooks: “{}” cannot run {} ({})", firing.playbook, firing.op, err.text);
                return;
            }
        };
        let intent = Intent {
            origin: glasscode_core::playbooks::limits::origin_of(&firing.playbook),
            subject: firing.subject.clone(),
            target: firing.target.clone(),
            caller: format!("Playbook “{}”", firing.playbook),
            reason: format!("event {}", firing.event),
            launch: self.control.launch.clone(),
            ..Intent::new(firing.key, op.name(), op.spec.retry)
        };
        let merged: Vec<Intent> = firing
            .merged
            .iter()
            .map(|other| Intent {
                origin: glasscode_core::playbooks::limits::origin_of(other),
                subject: firing.subject.clone(),
                target: firing.target.clone(),
                caller: format!("Playbook “{other}”"),
                reason: format!("gave way to “{}” on event {}", firing.playbook, firing.event),
                launch: self.control.launch.clone(),
                ..Intent::new(effects::effect_key("gave-way", other, &firing.subject, &firing.event), op.name(), op.spec.retry)
            })
            .collect();
        let caller = crate::control_ui::Caller {
            who: format!("Playbook “{}”", firing.playbook),
            trust: crate::control_ui::Trust::Session,
            channel: glasscode_core::caller::Caller::Manager { lane: glasscode_core::caller::Lane::Wake, wake: Some(format!("playbook:{}", firing.playbook)), project: None },
            session: None,
            session_title: None,
            agent: None,
            cwd: None,
            project: None,
        };
        let task = cx.background_spawn(async move {
            let home = glasscode_core::paths::MachineId::this().map_err(|err| std::io::Error::other(err.to_string()))?;
            for other in &merged {
                let _ = effects::record_settled(other, &home, EffectState::Done(serde_json::json!({ "same_as": intent.key.hex() })));
            }
            effects::claim(&intent, &home)
        });
        cx.spawn(async move |this, cx| {
            let claimed = task.await;
            let _ = this.update(cx, |ws, cx| match claimed {
                Ok(Claim::New(key)) if ws.main_window.is_some() => ws.with_main_window(cx, move |ws, window, cx| {
                    ws.run_op(
                        &op,
                        caller,
                        move |_, reply| match reply {
                            glasscode_core::control::Reply::Done { code: 0, .. } => effects::finish_later(key, EffectState::Done(serde_json::Value::Null)),
                            glasscode_core::control::Reply::Done { text, .. } => effects::finish_later(key, EffectState::Failed(text.lines().next().unwrap_or_default().to_string())),
                            // A card waits for the person; a restart settles it.
                            glasscode_core::control::Reply::Waiting { .. } => {}
                        },
                        window,
                        cx,
                    );
                }),
                Ok(Claim::New(key)) => finish_claim(key, EffectState::Failed("no window to run it in".into()), cx),
                Ok(Claim::Already(_)) => {}
                Err(err) => log::error!("playbooks: the effect ledger could not be written ({err}); nothing ran"),
            });
        })
        .detach();
    }
}

/// Something typed into Claude's prompt box and not sent yet (Glasscode's message would join
/// it).
pub(crate) fn has_draft(tail: &str) -> bool {
    prompt_box(tail).is_some_and(|text| !text.is_empty())
}

/// What is in Claude's prompt box now ("" when empty, `None` when no box is on screen). The
/// box is the last stretch between two rules; earlier `❯` lines are sent messages. Wrapped
/// lines come back joined by spaces.
pub(crate) fn prompt_box(tail: &str) -> Option<String> {
    let lines: Vec<&str> = tail.lines().collect();
    let rules: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| l.chars().filter(|c| *c == '─').count() >= 20).map(|(i, _)| i).collect();
    let [.., top, bottom] = rules[..] else { return None };
    let mut text: Vec<String> = Vec::new();
    let mut started = false;
    for line in &lines[top + 1..bottom] {
        let line = line.trim_start_matches(['│', '|', ' ']).trim_end_matches(['│', '|', ' ']);
        if !started {
            let Some(rest) = line.strip_prefix('❯').or_else(|| line.strip_prefix('>')) else { continue };
            started = true;
            let rest = rest.trim();
            if rest.starts_with("Try \"") {
                return Some(String::new());
            }
            text.push(rest.to_string());
        } else if !line.is_empty() {
            text.push(line.to_string());
        }
    }
    started.then(|| text.join(" ").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_unsent_text() {
        let rule = "─".repeat(40);
        assert!(!has_draft(&format!("❯ try again\n\nworking on it\n{rule}\n❯ \n{rule}\n  ⏵⏵ auto mode on")));
        assert!(!has_draft(&format!("{rule}\n❯ Try \"refactor the parser\"\n{rule}")));
        assert!(has_draft(&format!("{rule}\n❯ half a sentence\n{rule}")));
        assert!(!has_draft("❯ a sent message, no prompt box on screen"));
        assert_eq!(prompt_box(&format!("{rule}\n❯ fix the token refresh and\n  run the tests\n{rule}\n  ⏵⏵ auto mode on")).as_deref(), Some("fix the token refresh and run the tests"));
        assert_eq!(prompt_box(&format!("{rule}\n❯ \n{rule}")).as_deref(), Some(""));
    }
}
