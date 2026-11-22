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

/// How long a finish waits for the model's check at most; then it is told as done.
const CHECK_WAIT: Duration = Duration::from_secs(25);

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
}

struct Pending {
    since: Instant,
    reply: String,
    /// The report, once a rule or the check decided it.
    read: Option<Finish>,
    /// The check that decides it, while it runs.
    check: Option<u64>,
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

    pub(crate) fn report(&self, session: &str) -> Option<&Report> {
        self.reports.get(session)
    }
}

/// The icon of a session's last finish, when it shows one: orange while its question waits, the
/// "finished" mark until the person looked at it.
pub(crate) fn row_state_of(report: Option<&Report>) -> Option<RowState> {
    let report = report?;
    match report.finish.outcome {
        Outcome::NeedsYou => Some(RowState::NeedsYou),
        Outcome::Done if !report.seen => Some(RowState::Finished),
        _ => None,
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
        let f = &mut self.finishes;
        f.pending.remove(id);
        f.reports.remove(id);
        f.watches.remove(id);
        // Going on after an approval is the same turn.
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
        self.finishes.watches.insert(id.to_string(), TranscriptWatch::new(&path, glasscode_core::clock::now_ms()));
    }

    /// A session's turn ended (`activity_changed`): decide what it left, now by rule or by the
    /// check in the background, and tell it once [`HOLD`] is over (`finish_tick`).
    pub(crate) fn finish_begin(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.state.session(id).cloned() else { return };
        let now_ms = glasscode_core::clock::now_ms();
        let (reply, stopped_ms) = self.finishes.stops.remove(id).unwrap_or_default();
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
        let check = read.is_none().then(|| {
            self.finishes.checks += 1;
            self.finishes.checks
        });
        // The icon at once: what the rules say, else "finished" until the check answers.
        let shown = read.clone().unwrap_or_else(|| Finish::done(finish::headline(&reply)));
        self.finishes.reports.insert(id.to_string(), Report { finish: shown, at_ms: now_ms, seen: self.watching_session(id) });
        self.finishes.pending.insert(id.to_string(), Pending { since: Instant::now(), reply: reply.clone(), read, check, began_ms, by_prompt });
        if let Some(number) = check {
            self.finish_check(id, &session, reply, number, cx);
        }
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

    /// The icon of the session's last finish (`row_state`).
    pub(crate) fn finish_row_state(&self, id: &str) -> Option<RowState> {
        row_state_of(self.finishes.report(id))
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
        assert_eq!(row_state_of(Some(&report(Outcome::NeedsYou, true))), Some(RowState::NeedsYou), "a question stays until it is answered");
        assert_eq!(row_state_of(Some(&report(Outcome::Done, false))), Some(RowState::Finished));
        assert_eq!(row_state_of(Some(&report(Outcome::Done, true))), None);
        assert_eq!(row_state_of(Some(&report(Outcome::StillGoing, false))), None);
        assert_eq!(row_state_of(None), None);
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
    fn the_hold_covers_a_stop_hook_that_goes_on() {
        assert!(HOLD >= Duration::from_millis(6600), "nine continuations in ten start within 6.6 s");
        assert!(CHECK_WAIT > HOLD && CHECK_WAIT < finish::CHECK_LIMIT);
    }
}
