//! Each finished turn, told at once (the finish report, `glasscode_core::finish`).
//!
//! A session's turn ends (Claude's `Stop`, the end of a Codex turn in its rollout): its activity
//! goes from working to idle, and its last message says what it left. The report is decided then:
//! by rule when the ending is clear, by one small model run on the session's own account when it is
//! not (`finish::check`). It is told after [`HOLD`], unless work starts again first:
//!
//! - the icon: orange when it asks the person something, the "finished" mark when it is done and
//!   the person has not looked at it since (`row_state`);
//! - a notification: "needs you" with its question, or "finished" for work a prompt began (the
//!   person's, or a message Glasscode typed in); a turn another session's message began is told by
//!   its icon only, so that a busy team of sessions does not fill the screen;
//! - the manager: the session's state word and report (`session_state`, `session.show`), its last
//!   reply and finish time in what the manager sees (`v2_session_finished`), the journal's
//!   `session.finished` (a question wakes the manager); and, when the manager sent the session a
//!   message and waits to hear back, it says the outcome in the chat and the voice.
//!
//! Claude Code's idle reminder, which used to stand for all of this sixty seconds late, is no
//! "needs you" any more in the app (`agents::notification_waits`). What starts work again without
//! a hook (another session's message, a Stop hook's feedback, a tool after an approval) is read
//! from the transcript (`agents::TranscriptWatch`).
//!
//! What waits to hear back lives in memory: a restart forgets it, and then nothing is said aloud.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use glasscode_core::agents::{AgentActivity, TranscriptWatch};
use glasscode_core::finish::{self, Finish, Outcome, Read};
use glasscode_core::model::AgentKind;
use gpui::{AppContext as _, Context};
use serde_json::{Value, json};

use crate::center::Center;
use crate::sidebar::RowState;
use crate::workspace::Workspace;

/// How long a finish waits before it is told: a Stop hook's feedback starts the turn again within
/// 6.6 s in nine cases out of ten (1.6 s for half of them; the person's Mac, 2026-11-22).
pub(crate) const HOLD: Duration = Duration::from_secs(7);

/// How long a session still shows "working" after its turn ended: a Stop hook's feedback goes on
/// within this in about half the cases, and the dot does not blink green, blue, green. The check
/// of an unclear ending starts then too, so a turn that goes on costs no model run.
pub(crate) const SETTLE: Duration = Duration::from_secs(2);

/// How long a finish waits for the model's check at most; then it is told as done.
const CHECK_WAIT: Duration = Duration::from_secs(25);

/// How many ticks in a row a prompt that was on a session's screen must be gone before the session
/// counts as going on (the person answered it, here or from the phone, the glasses, the manager).
const PROMPT_GONE_TICKS: u8 = 3;

/// How long after that a prompt back on the screen means it still waits (it was misread).
const PROMPT_RECHECK: Duration = Duration::from_secs(10);

/// How long a turn that said its work goes on by itself shows "working": nine in ten went on
/// within 30 minutes on the person's Mac (half within 20 s; 2 of 21 never did, 2026-11-22).
const STILL_GOING_FOR_MS: u64 = 30 * 60_000;

/// What the screen of a session that waits on a prompt said, look after look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PromptLooks {
    seen: bool,
    gone: u8,
}

impl PromptLooks {
    /// One look: true when the prompt it saw has been gone long enough to count as answered. A
    /// prompt never seen (the screen did not read as one) says nothing: the transcript decides.
    pub(crate) fn look(&mut self, on_screen: bool) -> bool {
        if on_screen {
            *self = PromptLooks { seen: true, gone: 0 };
            return false;
        }
        if self.seen {
            self.gone = self.gone.saturating_add(1);
        }
        self.seen && self.gone >= PROMPT_GONE_TICKS
    }
}

/// Each session's finishes: those being decided and the last one told.
#[derive(Default)]
pub(crate) struct Finishes {
    /// The last message of the turn that ended last in each session, and when (`note_stop`).
    stops: HashMap<String, (Option<String>, Option<u64>)>,
    /// Finishes not told yet.
    pending: HashMap<String, Pending>,
    /// What each session's last told finish left, until it works again.
    reports: HashMap<String, Report>,
    /// Claude transcripts followed while a session is idle or waits, to see work start with no hook.
    watches: HashMap<String, TranscriptWatch>,
    /// When the manager sent each session a message it waits to hear back about (ms).
    awaiting: HashMap<String, u64>,
    /// When each session's current or last turn began (ms).
    began: HashMap<String, u64>,
    /// Sessions whose current work a prompt began (the person's, or a message Glasscode typed).
    by_prompt: HashSet<String>,
    /// The work that is starting now was seen in the transcript, not told by a hook.
    from_transcript: bool,
    /// The number of the last check started, so that a late answer of an older one is dropped.
    checks: u64,
    /// Sessions whose state this launch has from their hooks or rollout (a bell of theirs says
    /// nothing new).
    followed: HashSet<String>,
    /// Claude sessions that wait on a prompt: what their screen said ([`PromptLooks`]).
    prompts: HashMap<String, PromptLooks>,
    /// Claude sessions that went on because their prompt left the screen, and when.
    answered: HashMap<String, Instant>,
    /// How many turns of each Codex session's rollout had ended at the last look.
    codex_ended: HashMap<String, u64>,
}

struct Pending {
    since: Instant,
    reply: String,
    /// The report, once a rule or the check decided it.
    read: Option<Finish>,
    /// The check that decides it, while it runs.
    check: Option<u64>,
    /// The check is to start once the finish settled ([`SETTLE`]).
    check_wanted: bool,
    /// [`SETTLE`] is over: the dot shows what the turn left.
    settled: bool,
    began_ms: u64,
    by_prompt: bool,
}

/// The last told finish of a session.
#[derive(Clone, Debug)]
pub(crate) struct Report {
    pub finish: Finish,
    pub at_ms: u64,
    /// The person looked at the session since (a "done" mark goes; a question stays).
    pub seen: bool,
}

impl Finishes {
    /// A turn of `session` ended with this last message (Claude's `Stop`, a Codex turn's end).
    /// The latest end counts, also one that said nothing: an older message is not this turn's.
    pub(crate) fn note_stop(&mut self, session: &str, reply: Option<String>, at_ms: Option<u64>) {
        self.stops.insert(session.to_string(), (reply, at_ms));
    }

    /// The session's terminal stopped: nothing of it is told, and its last report is over.
    pub(crate) fn forget(&mut self, session: &str) {
        self.stops.remove(session);
        self.pending.remove(session);
        self.reports.remove(session);
        self.watches.remove(session);
        self.by_prompt.remove(session);
        self.followed.remove(session);
        self.prompts.remove(session);
        self.answered.remove(session);
        self.codex_ended.remove(session);
    }

    /// The session's hooks (Claude) or rollout (Codex) reported this launch.
    pub(crate) fn followed(&mut self, session: &str) {
        if !self.followed.contains(session) {
            self.followed.insert(session.to_string());
        }
    }

    /// Glasscode has the session's state from its hooks or rollout: its bell is no news.
    pub(crate) fn follows(&self, session: &str) -> bool {
        self.followed.contains(session)
    }

    /// A turn of the session ended: its transcript is followed from its end now, for lines written
    /// after `since_ms` (a watch from before would take the ended turn's lines for new work).
    pub(crate) fn rebase_watch(&mut self, session: &str, path: &std::path::Path, since_ms: u64) {
        self.watches.insert(session.to_string(), TranscriptWatch::new(path, since_ms));
    }

    /// More of the Codex session's turns ended than at the last look. A rollout is read from this
    /// launch on (`RolloutTail::resumed`), or from its start when this launch began it: the turns
    /// the first look finds ended are this launch's too (a new conversation is often found only
    /// once its first turn is over).
    pub(crate) fn codex_ended(&mut self, session: &str, ended: u64) -> bool {
        let before = self.codex_ended.insert(session.to_string(), ended);
        ended > before.unwrap_or(0)
    }

    pub(crate) fn report(&self, session: &str) -> Option<&Report> {
        self.reports.get(session)
    }
}

/// The icon of a session's last finish, when it shows one: orange while its question waits, the
/// "finished" mark until the person looked at it, and "working" while its work goes on by itself
/// (a build, a review, another session it waits for, and then it reports back).
pub(crate) fn row_state_of(report: Option<&Report>, now_ms: u64) -> Option<RowState> {
    let report = report?;
    match report.finish.outcome {
        Outcome::NeedsYou => Some(RowState::NeedsYou),
        Outcome::StillGoing if now_ms.saturating_sub(report.at_ms) < STILL_GOING_FOR_MS => Some(RowState::Working),
        Outcome::StillGoing => None,
        Outcome::Done if !report.seen => Some(RowState::Finished),
        Outcome::Done => None,
    }
}

/// The state word of a session whose last finish was told (`session_state`).
pub(crate) fn state_word_of(report: Option<&Report>) -> Option<&'static str> {
    Some(match report?.finish.outcome {
        Outcome::NeedsYou => "needs you",
        Outcome::Done => "finished",
        Outcome::StillGoing => "goes on by itself",
    })
}

/// The notification of a finish (title, body, whether it needs the person), when it gets one: a
/// question always; "finished" for work a prompt began; work that goes on by itself never.
pub(crate) fn popup_of(agent: &str, title: &str, finish: &Finish, by_prompt: bool) -> Option<(String, String, bool)> {
    let with = |line: &str| if line.trim().is_empty() { title.to_string() } else { format!("{title}: {line}") };
    match finish.outcome {
        Outcome::NeedsYou => Some((format!("{agent} needs you"), with(&finish.question), true)),
        Outcome::Done if by_prompt => Some((format!("{agent} finished"), with(&finish.summary), false)),
        _ => None,
    }
}

/// What the manager says of a finish the person waits to hear about; none while it goes on.
pub(crate) fn spoken_of(title: &str, finish: &Finish) -> Option<String> {
    let end = |line: &str| {
        let line = line.trim();
        if line.ends_with(['.', '!', '?', '…']) { line.to_string() } else { format!("{line}.") }
    };
    match finish.outcome {
        Outcome::NeedsYou if !finish.question.trim().is_empty() => Some(format!("“{title}” asks: {}", end(&finish.question))),
        Outcome::NeedsYou => Some(format!("“{title}” finished and waits for you.")),
        Outcome::Done if !finish.summary.trim().is_empty() => Some(format!("“{title}” finished: {}", end(&finish.summary))),
        Outcome::Done => Some(format!("“{title}” finished.")),
        Outcome::StillGoing => None,
    }
}

impl Workspace {
    /// The person is looking at the session at the front of the window.
    fn watching_session(&self, id: &str) -> bool {
        self.window_active && self.center == Center::Session && self.state.active_session.as_deref() == Some(id)
    }

    /// Work starts in a session (`activity_changed`): a finish still being decided is not one,
    /// and its last report is over.
    pub(crate) fn finish_working(&mut self, id: &str, previous: Option<AgentActivity>) {
        // A prompt it waited on was answered (here or elsewhere): it no longer needs the person.
        self.attention.remove(id);
        let f = &mut self.finishes;
        f.pending.remove(id);
        f.reports.remove(id);
        f.watches.remove(id);
        f.prompts.remove(id);
        // Going on after an approval is the same turn.
        if previous != Some(AgentActivity::NeedsInput) {
            f.answered.remove(id);
        }
        if previous != Some(AgentActivity::NeedsInput) {
            f.began.insert(id.to_string(), glasscode_core::clock::now_ms());
            // A hook's prompt began it; a Stop hook's feedback goes on with what began the turn,
            // and another session's message begins work of no prompt.
            if !f.from_transcript {
                f.by_prompt.insert(id.to_string());
            }
        }
    }

    /// A session waits on a prompt mid-turn: its transcript says when it goes on (a tool runs).
    pub(crate) fn finish_watch(&mut self, id: &str) {
        let Some(path) = self.state.session(id).filter(|s| s.agent == AgentKind::Claude).and_then(|s| s.transcript_path.clone()) else { return };
        self.finishes.watches.insert(id.to_string(), TranscriptWatch::new(&path, glasscode_core::clock::now_ms()).mid_turn());
    }

    /// A session's turn ended (`activity_changed`): decide what it left, now by rule or by the
    /// check in the background, and tell it once [`HOLD`] is over (`finish_tick`).
    pub(crate) fn finish_begin(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.state.session(id).cloned() else { return };
        let now_ms = glasscode_core::clock::now_ms();
        // Only a turn's end is a finish: a `SessionStart` after work (`/clear`, a resume, a start)
        // makes the session idle, and ends nothing.
        let Some((reply, stopped_ms)) = self.finishes.stops.remove(id) else { return };
        let reply = reply.unwrap_or_default();
        if session.agent == AgentKind::Claude
            && let Some(path) = session.transcript_path.as_deref()
        {
            self.finishes.watches.insert(id.to_string(), TranscriptWatch::new(path, stopped_ms.unwrap_or(now_ms)));
        }
        let began_ms = self.finishes.began.get(id).copied().unwrap_or(0);
        // The person waits to hear: the model reads even an ending the rules call done, which may
        // be work that goes on ("waiting on the review now").
        let awaited = self.finishes.awaiting.get(id).is_some_and(|since| began_ms >= *since);
        let read = match finish::by_rule(&reply) {
            Read::Clear(finish) if !(awaited && finish.outcome == Outcome::Done && !reply.trim().is_empty()) => Some(finish),
            _ => None,
        };
        let by_prompt = self.finishes.by_prompt.contains(id);
        // What the dot shows once the finish settled: what the rules say, else "finished" until
        // the check answers.
        let shown = read.clone().unwrap_or_else(|| Finish::done(finish::headline(&reply)));
        self.finishes.reports.insert(id.to_string(), Report { finish: shown, at_ms: now_ms, seen: self.watching_session(id) });
        let check_wanted = read.is_none();
        self.finishes.pending.insert(id.to_string(), Pending { since: Instant::now(), reply, read, check: None, check_wanted, settled: false, began_ms, by_prompt });
        let _ = cx;
    }

    /// The model's check of an unclear ending, off the main thread, on the session's account.
    fn finish_check(&mut self, id: &str, session: &glasscode_core::model::Session, reply: String, number: u64, cx: &mut Context<Self>) {
        let agent = session.agent;
        let config_dir = session.account_id.as_deref().and_then(|a| self.state.account(a)).or_else(|| self.state.default_account(agent)).and_then(|a| a.config_dir());
        let env = self.env.clone();
        let task = cx.background_spawn(async move { finish::check(agent, config_dir.as_deref(), &reply, &env) });
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                let Some(pending) = ws.finishes.pending.get_mut(&id).filter(|p| p.check == Some(number)) else { return };
                pending.check = None;
                pending.read = Some(match result {
                    Ok(finish) => finish,
                    Err(why) => {
                        log::warn!("finish: the check of session {id} did not answer ({why:#}); it is told as done");
                        Finish::done(finish::headline(&pending.reply))
                    }
                });
                // The icon follows the check at once; the notification waits for the hold.
                if let (Some(read), Some(report)) = (pending.read.clone(), ws.finishes.reports.get_mut(&id)) {
                    report.finish = read;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Each tick: work that starts again with no hook, and the finishes whose report is due. True
    /// when something changed.
    pub(crate) fn finish_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        // Every Claude session that does not work is followed in its transcript (not only after a
        // finish: one that started idle, or resumed, can be begun by another session's message),
        // and one that waits on a prompt on its screen goes on once the prompt is gone.
        let claude: Vec<(String, Option<std::path::PathBuf>, Option<AgentActivity>, bool)> = self
            .state
            .sessions
            .iter()
            .filter(|s| s.agent == AgentKind::Claude && self.terminals.contains_key(&s.id) && !self.waits_to_start(&s.id))
            .filter_map(|s| {
                let activity = self.activity.get(&s.id).copied();
                let recheck = activity == Some(AgentActivity::Working) && self.finishes.answered.get(&s.id).is_some_and(|at| at.elapsed() < PROMPT_RECHECK);
                if !(matches!(activity, Some(AgentActivity::Idle | AgentActivity::NeedsInput)) || recheck) {
                    return None;
                }
                let looked = activity == Some(AgentActivity::NeedsInput) || recheck;
                let asking = looked && self.terminals.get(&s.id).is_some_and(|t| glasscode_core::prompt::parse(&t.read(cx).text_tail(60)).is_some());
                Some((s.id.clone(), s.transcript_path.clone(), activity, asking))
            })
            .collect();
        for (id, path, activity, asking) in claude {
            if activity == Some(AgentActivity::Working) {
                // Just answered by its prompt leaving the screen: a prompt back there still waits.
                if asking {
                    self.finishes.answered.remove(&id);
                    let previous = self.activity.insert(id.clone(), AgentActivity::NeedsInput);
                    self.activity_changed(&id, previous, AgentActivity::NeedsInput, cx);
                    changed = true;
                }
                continue;
            }
            let waits = activity == Some(AgentActivity::NeedsInput);
            if let Some(path) = path {
                let fits = self.finishes.watches.get(&id).is_some_and(|w| w.path() == path.as_path() && w.is_mid_turn() == waits);
                if !fits {
                    let watch = TranscriptWatch::new(&path, glasscode_core::clock::now_ms());
                    self.finishes.watches.insert(id.clone(), if waits { watch.mid_turn() } else { watch });
                }
            }
            if !waits {
                self.finishes.prompts.remove(&id);
                continue;
            }
            if self.finishes.prompts.entry(id.clone()).or_default().look(asking) {
                // Answered: the same turn goes on.
                self.finishes.prompts.remove(&id);
                let previous = self.activity.insert(id.clone(), AgentActivity::Working);
                self.activity_changed(&id, previous, AgentActivity::Working, cx);
                self.finishes.answered.insert(id.clone(), Instant::now());
                changed = true;
            }
        }
        let ids: Vec<String> = self.finishes.watches.keys().cloned().collect();
        for id in ids {
            let waiting = matches!(self.activity.get(&id), Some(AgentActivity::Idle | AgentActivity::NeedsInput));
            let started = waiting && self.finishes.watches.get_mut(&id).is_some_and(TranscriptWatch::poll);
            if !waiting {
                self.finishes.watches.remove(&id);
            } else if started {
                self.finishes.from_transcript = true;
                let previous = self.activity.insert(id.clone(), AgentActivity::Working);
                self.activity_changed(&id, previous, AgentActivity::Working, cx);
                self.finishes.from_transcript = false;
                changed = true;
            }
        }
        // A finish settles: the dot shows what it left, and the check of an unclear one starts.
        let settling: Vec<String> = self.finishes.pending.iter().filter(|(_, p)| !p.settled && p.since.elapsed() >= SETTLE).map(|(id, _)| id.clone()).collect();
        for id in settling {
            let Some(pending) = self.finishes.pending.get_mut(&id) else { continue };
            pending.settled = true;
            changed = true;
            if std::mem::take(&mut pending.check_wanted) {
                self.finishes.checks += 1;
                let number = self.finishes.checks;
                let reply = pending.reply.clone();
                pending.check = Some(number);
                if let Some(session) = self.state.session(&id).cloned() {
                    self.finish_check(&id, &session, reply, number, cx);
                }
            }
        }
        let due: Vec<String> = self
            .finishes
            .pending
            .iter()
            .filter(|(_, p)| p.since.elapsed() >= HOLD && (p.read.is_some() || p.since.elapsed() >= CHECK_WAIT))
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            if let Some(pending) = self.finishes.pending.remove(&id) {
                self.finish_tell(&id, pending, cx);
                changed = true;
            }
        }
        changed
    }

    /// Tell a finish: its report, the journal, the manager, the notification and, for a session
    /// the manager waits to hear from, the chat and the voice.
    fn finish_tell(&mut self, id: &str, pending: Pending, cx: &mut Context<Self>) {
        let Some(session) = self.state.session(id).cloned() else { return };
        let finish = pending.read.unwrap_or_else(|| Finish::done(finish::headline(&pending.reply)));
        let watching = self.watching_session(id);
        let seen = watching || self.finishes.reports.get(id).is_some_and(|r| r.seen);
        self.finishes.reports.insert(id.to_string(), Report { finish: finish.clone(), at_ms: glasscode_core::clock::now_ms(), seen });
        self.finishes.by_prompt.remove(id);
        let key = self.journal_key(id);
        self.journal_note(glasscode_core::journal::session_finished(id, key.as_ref(), session.agent, &finish, pending.by_prompt));
        self.v2_session_finished(id, &finish, &pending.reply);
        let title = self.session_title(&session);
        // Said where the manager waits to hear back (a message it sent), once its answer is in.
        let awaited = self.finishes.awaiting.get(id).is_some_and(|since| pending.began_ms >= *since);
        if awaited && let Some(line) = spoken_of(&title, &finish) {
            self.finishes.awaiting.remove(id);
            self.chat_say_from_code(line, true, cx);
        }
        let wanted = match finish.outcome {
            Outcome::NeedsYou => !self.state.notify.needs_input_off,
            _ => !self.state.notify.finished_off,
        };
        if wanted
            && !watching
            && let Some((heading, body, needs)) = popup_of(session.agent.name(), &title, &finish, pending.by_prompt)
        {
            self.notify_session(id, session.agent, heading, body, needs, cx);
        }
        cx.notify();
    }

    /// The person opened the session: a "done" mark has been seen.
    pub(crate) fn finish_seen(&mut self, id: &str) {
        if let Some(report) = self.finishes.reports.get_mut(id) {
            report.seen = true;
        }
    }

    /// The manager sent the session a message and waits to hear back (`session.send`).
    pub(crate) fn finish_await(&mut self, id: &str) {
        self.finishes.awaiting.insert(id.to_string(), glasscode_core::clock::now_ms());
    }

    /// The icon of the session's last finish (`row_state`): still "working" while the finish
    /// settles.
    pub(crate) fn finish_row_state(&self, id: &str) -> Option<RowState> {
        if self.finishes.pending.get(id).is_some_and(|p| !p.settled) {
            return Some(RowState::Working);
        }
        row_state_of(self.finishes.report(id), glasscode_core::clock::now_ms())
    }

    /// The state word of the session's last finish (`session_state`).
    pub(crate) fn finish_state_word(&self, id: &str) -> Option<&'static str> {
        state_word_of(self.finishes.report(id))
    }

    /// The session's last finish, for `session.show` and `session list --json`.
    pub(crate) fn finish_json(&self, id: &str) -> Value {
        match self.finishes.report(id) {
            Some(r) => json!({
                "outcome": r.finish.outcome.word(),
                "question": r.finish.question,
                "summary": r.finish.summary,
                "seconds_ago": glasscode_core::clock::now_ms().saturating_sub(r.at_ms) / 1000,
                "checked": r.finish.checked,
            }),
            None => Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish(outcome: Outcome, question: &str, summary: &str) -> Finish {
        Finish { outcome, question: question.into(), summary: summary.into(), checked: false }
    }

    fn report(outcome: Outcome, seen: bool) -> Report {
        Report { finish: finish(outcome, "", ""), at_ms: 0, seen }
    }

    #[test]
    fn the_icon_is_orange_for_a_question_and_marked_until_a_finish_is_seen() {
        assert_eq!(row_state_of(Some(&report(Outcome::NeedsYou, true)), 0), Some(RowState::NeedsYou), "a question stays until it is answered");
        assert_eq!(row_state_of(Some(&report(Outcome::Done, false)), 0), Some(RowState::Finished));
        assert_eq!(row_state_of(Some(&report(Outcome::Done, true)), 0), None);
        assert_eq!(row_state_of(Some(&report(Outcome::StillGoing, true)), 60_000), Some(RowState::Working), "its work goes on by itself");
        assert_eq!(row_state_of(Some(&report(Outcome::StillGoing, true)), STILL_GOING_FOR_MS), None, "not green for good");
        assert_eq!(row_state_of(None, 0), None);
        assert_eq!(state_word_of(Some(&report(Outcome::Done, true))), Some("finished"), "the manager hears it is finished, seen or not");
        assert_eq!(state_word_of(Some(&report(Outcome::StillGoing, true))), Some("goes on by itself"));
    }

    #[test]
    fn a_question_always_notifies_and_done_only_for_work_a_prompt_began() {
        let asks = finish(Outcome::NeedsYou, "Merge #347 now?", "Tests pass.");
        assert_eq!(popup_of("Claude", "Voice fix", &asks, false), Some(("Claude needs you".into(), "Voice fix: Merge #347 now?".into(), true)));
        let done = finish(Outcome::Done, "", "Pushed the fix.");
        assert_eq!(popup_of("Codex", "Retry", &done, true), Some(("Codex finished".into(), "Retry: Pushed the fix.".into(), false)));
        assert_eq!(popup_of("Codex", "Retry", &done, false), None, "another session's message began it");
        assert_eq!(popup_of("Claude", "Retry", &finish(Outcome::StillGoing, "", "Waits for CI."), true), None);
        assert_eq!(popup_of("Claude", "Retry", &finish(Outcome::Done, "", ""), true).map(|p| p.1), Some("Retry".into()));
    }

    #[test]
    fn the_manager_says_the_outcome_and_nothing_while_it_goes_on() {
        assert_eq!(spoken_of("Ali's PR work", &finish(Outcome::Done, "", "Ali has no open pull requests")).as_deref(), Some("“Ali's PR work” finished: Ali has no open pull requests."));
        assert_eq!(spoken_of("Voice fix", &finish(Outcome::NeedsYou, "Should I push it?", "")).as_deref(), Some("“Voice fix” asks: Should I push it?"));
        assert_eq!(spoken_of("Voice fix", &finish(Outcome::StillGoing, "", "Waits for CI.")), None);
    }

    #[test]
    fn a_prompt_is_answered_once_it_has_left_the_screen_for_a_while() {
        let mut looks = PromptLooks::default();
        assert!(!looks.look(false), "a prompt the screen never showed says nothing");
        assert!(!looks.look(false));
        assert!(!looks.look(true));
        assert!(!looks.look(false), "a redraw is not an answer");
        assert!(!looks.look(true), "back: it still waits");
        assert!(!looks.look(false) && !looks.look(false));
        assert!(looks.look(false), "gone three looks in a row: answered");
    }

    #[test]
    fn the_hold_covers_a_stop_hook_that_goes_on() {
        assert!(HOLD >= Duration::from_millis(6600), "nine continuations in ten start within 6.6 s");
        assert!(SETTLE < HOLD && SETTLE >= Duration::from_millis(1600), "the dot waits out half the continuations, the notification nine in ten");
        assert!(CHECK_WAIT > HOLD && CHECK_WAIT < finish::CHECK_LIMIT);
    }
}
