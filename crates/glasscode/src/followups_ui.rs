//! v2's follow-ups in the app (M56.2, the `follow-ups` built-in). They are kept in the work
//! ledger (`WorkLedger::follow_ups`), which the journal tick holds and saves; v1's harness no
//! longer touches them with the engine at v2.
//!
//! On the home, with the engine at v2, the manager on and the built-in on:
//! - every 30 s a round of `followups::round` on the ledger (reviews received kept in step with
//!   the snapshot, the teammates' names, the quiet ones nudged);
//! - every 5 min the reviews the person gave (`gh::given_reviews`, off the main thread), then
//!   `followups::given` ("ready for you to look again", once);
//! - once a run, v1's follow-ups in `data.json` taken in (`migrate_v2::v1_follow_ups`), so a
//!   home that just switched goes on where v1 was.
//!
//! The snapshot is v2's own (`v2_snapshot`), the notices and lines go through v2's apply
//! (`v2_apply`), and a draft to a teammate into v2's outbox (`v2_draft_message`). The
//! `manager follow-up …` ops and the Manager page read and write the same list
//! ([`Workspace::follow_ups`], [`Workspace::follow_ups_edit`]).

use std::time::{Duration, Instant};

use glasscode_core::manager::handover::Owner;
use glasscode_core::manager::{Action, Data, FollowKind, FollowUp, MsgState, Show, Snapshot, Tier, followups, gh};
use gpui::{AppContext as _, Context};

use crate::workspace::Workspace;

/// How often the round looks, and how often the reviews the person gave are read.
const ROUND_EVERY: Duration = Duration::from_secs(30);
const REVIEWS_EVERY: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
pub struct FollowUpsUi {
    at: Option<Instant>,
    /// The last round's snapshot: the next round's "before" (none: the run's first round).
    prev: Option<Snapshot>,
    reviews_at: Option<Instant>,
    reviews_running: bool,
    /// v1's follow-ups, taken in once a run before the first round (see [`Take`]).
    take: Take,
}

/// v1's follow-ups in `data.json`, taken in before the first round and the first reviews read,
/// so neither acts on a ledger that has not got them yet. Read again at each start: nothing
/// removes a follow-up or its keys, and the newer state wins (`WorkLedger::take_follow_ups`), so a
/// follow-up closed at v2 stays closed, and one v1 changed after a switch back comes over.
#[derive(Default, PartialEq)]
enum Take {
    #[default]
    Unread,
    Reading,
    Done,
}

impl Workspace {
    /// The follow-ups as the engine keeps them: the work ledger's at v2, v1's `Data` otherwise.
    pub(crate) fn follow_ups(&self) -> &[FollowUp] {
        if self.control.engine_v2 {
            return self.work_ledger().map(|ledger| ledger.follow_ups.as_slice()).unwrap_or_default();
        }
        &self.manager.harness.data.followups
    }

    /// Change the follow-ups where the engine keeps them; the store is written by its own
    /// writer (the journal tick at v2, the harness's save at v1).
    pub(crate) fn follow_ups_edit<R>(&mut self, edit: impl FnOnce(&mut Vec<FollowUp>) -> R) -> Option<R> {
        if self.control.engine_v2 {
            return self.work_ledger_edit(|ledger| edit(&mut ledger.follow_ups));
        }
        let out = edit(&mut self.manager.harness.data.followups);
        self.manager.harness.dirty = true;
        Some(out)
    }

    /// v1's `Data` with the follow-ups where the engine keeps them, for what still reads them on
    /// `Data` (the brief and the tour, until they read v2's stores: M56.2 t5, part 3).
    pub(crate) fn data_with_follow_ups(&self) -> std::borrow::Cow<'_, Data> {
        if !self.control.engine_v2 {
            return std::borrow::Cow::Borrowed(&self.manager.harness.data);
        }
        let mut data = self.manager.harness.data.clone();
        data.followups = self.follow_ups().to_vec();
        std::borrow::Cow::Owned(data)
    }

    /// v1's chat rules ("what am I waiting on?", "done with #1111") on the follow-ups where the
    /// engine keeps them: at v2 the ledger's are lent to the harness for the call, and what the
    /// call changed goes back to the ledger (v1's copy is left as it was).
    pub(crate) fn with_follow_ups_lent<R>(&mut self, call: impl FnOnce(&mut Self) -> R) -> R {
        if !self.control.engine_v2 {
            return call(self);
        }
        let lent = self.follow_ups().to_vec();
        let v1 = std::mem::replace(&mut self.manager.harness.data.followups, lent.clone());
        let out = call(self);
        let back = std::mem::replace(&mut self.manager.harness.data.followups, v1);
        if back != lent {
            self.follow_ups_edit(|list| *list = back);
        }
        out
    }

    /// The round, from the workspace's tick.
    pub fn followups_tick(&mut self, cx: &mut Context<Self>) {
        if !self.control.engine_v2 || !self.state.manager.enabled || !self.acts_as_home() || self.work_ledger().is_none() {
            return;
        }
        match self.followups.take {
            Take::Done => {}
            Take::Reading => return,
            Take::Unread => {
                self.followups.take = Take::Reading;
                let task = cx.background_spawn(async move { glasscode_core::manager::migrate_v2::v1_follow_ups() });
                cx.spawn(async move |this, cx| {
                    let found = task.await;
                    let _ = this.update(cx, |ws, _| {
                        ws.followups.take = Take::Done;
                        match found {
                            Ok((list, seen)) if !list.is_empty() || !seen.is_empty() => {
                                ws.work_ledger_edit(|ledger| ledger.take_follow_ups(list, seen));
                            }
                            Ok(_) => {}
                            // The round goes on with what the ledger has.
                            Err(why) => log::warn!("follow-ups: v1's could not be read: {why}"),
                        }
                    });
                })
                .detach();
                return;
            }
        }
        if self.manager_owned_v2().follow_ups != Owner::V2 {
            return;
        }
        if self.followups.at.is_none_or(|at| at.elapsed() >= ROUND_EVERY) {
            self.followups.at = Some(Instant::now());
            let snap = self.v2_snapshot();
            let settings = self.state.manager.clone();
            let prev = self.followups.prev.take();
            let now = glasscode_core::model::now();
            let actions = self.work_ledger_edit(|ledger| followups::round(&mut ledger.book(), &settings, prev.as_ref(), &snap, now)).unwrap_or_default();
            self.followups.prev = Some(snap);
            self.followups_apply(actions, cx);
        }
        if !self.followups.reviews_running && self.followups.reviews_at.is_none_or(|at| at.elapsed() >= REVIEWS_EVERY) {
            self.followups_fetch_reviews(cx);
        }
    }

    /// What the follow-ups say: notifications and lines said through v2's apply; a draft to a
    /// teammate into v2's outbox, and the person told it waits for their OK (once: the same text
    /// to the same person again is the message the outbox already has).
    fn followups_apply(&mut self, actions: Vec<Action>, cx: &mut Context<Self>) {
        let mut rest = Vec::new();
        for action in actions {
            let Action::Draft { to, body, why, about } = action else {
                rest.push(action);
                continue;
            };
            // The outbox signs the text, so the same one again is told by its id.
            let known: Vec<String> = self.round.outbox.as_ref().map(|outbox| outbox.messages.iter().map(|m| m.id.clone()).collect()).unwrap_or_default();
            let Some(message) = self.v2_draft_message(&to, &body, &why, about.as_deref()) else {
                log::warn!("follow-ups: v2's outbox is not read yet; the note to {to} waits for the next quiet spell");
                continue;
            };
            if known.contains(&message.id) || message.id.is_empty() || message.state != MsgState::Draft || message.tier == Tier::Never {
                continue;
            }
            let text = format!("A message to {} waits for your OK.", message.to);
            rest.push(Action::Say { text: text.clone(), show: Some(Show { view: Some("manager".into()), ..Show::default() }) });
            if !self.window_active {
                rest.push(Action::Notify { title: format!("{} asks you", self.state.manager.name), body: text, session: None });
            }
        }
        self.v2_apply(rest, cx);
    }

    /// The reviews the person gave, read off the main thread, then the follow-ups kept in step.
    fn followups_fetch_reviews(&mut self, cx: &mut Context<Self>) {
        self.followups.reviews_running = true;
        self.followups.reviews_at = Some(Instant::now());
        let me = self.manager.me.clone();
        let known: Vec<(String, u64)> = self.follow_ups().iter().filter(|f| f.kind == FollowKind::BlockersGiven && f.is_open()).map(|f| (f.repo.clone(), f.number)).collect();
        let env = (*self.env).clone();
        let task = cx.background_spawn(async move {
            let me = match me {
                Some(me) => Ok(me),
                None => gh::my_login(&env),
            };
            me.and_then(|me| Ok((gh::given_reviews(&me, &known, &env)?, me))).map_err(|e| format!("{e:#}"))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                ws.followups.reviews_running = false;
                let reviews = match result {
                    Ok((reviews, me)) => {
                        // Kept: the server follows the same person's reviews while it is home.
                        ws.manager.me.get_or_insert(me);
                        reviews
                    }
                    Err(why) => {
                        log::warn!("follow-ups: could not read the reviews you gave: {why}");
                        return;
                    }
                };
                // The server may have become home while they were read: then they are its.
                if !ws.control.engine_v2 || !ws.state.manager.enabled || !ws.acts_as_home() {
                    return;
                }
                let settings = ws.state.manager.clone();
                let prev = ws.followups.prev.clone();
                let now = glasscode_core::model::now();
                let actions = ws.work_ledger_edit(|ledger| followups::given(&mut ledger.book(), &settings, &reviews, prev.as_ref(), now)).unwrap_or_default();
                ws.followups_apply(actions, cx);
                cx.notify();
            });
        })
        .detach();
    }
}
