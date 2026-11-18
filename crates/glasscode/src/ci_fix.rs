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

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::Context;
use glasscode_core::agents::AgentActivity;
use glasscode_core::autofix::{self, Enabled, Plan};
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
            if !self.ci_fix_can_take(&id, cx) {
                // A closed session: flag it once, the ask goes out when it is opened.
                let about = if ask.conflicts { ci.merge_key() } else { ci.head.clone() };
                if !self.terminals.contains_key(&id) && self.ci_fix.flagged.insert((id.clone(), about)) {
                    self.attention.insert(id.clone());
                    cx.notify();
                }
                continue;
            }
            let text = autofix::message(&repo, pr.number, &pr.url, &ci, ask);
            if !self.type_into_session(&id, &text, cx) {
                continue;
            }
            autofix::asked(&mut record, &ci, ask, &id, unix_now());
            self.ci_fix.records.insert(key, record);
            changed = true;
            let title = self.state.session(&id).map(|s| s.title.clone()).unwrap_or_default();
            let what = if ask.conflicts {
                format!("PR #{} conflicts with {}: asked the agent in \u{201c}{title}\u{201d} to resolve it.", pr.number, if ci.base.is_empty() { "its base" } else { &ci.base })
            } else {
                format!("CI failed on PR #{}: asked the agent in \u{201c}{title}\u{201d} to fix it.", pr.number)
            };
            self.show_toast(what.clone(), cx);
            let watching = self.window_active && self.center == Center::Session && self.state.active_session.as_deref() == Some(id.as_str());
            if !watching && !self.state.prefs.notify_ci_off {
                if let Some(session) = self.state.session(&id).cloned() {
                    let heading = if ask.conflicts { "Merge conflicts" } else { "CI failed" };
                    self.notify_session(&id, session.agent, heading.to_string(), what, false, cx);
                }
            }
        }
        if changed {
            self.ci_fix.save();
            cx.notify();
        }
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

    /// Type a message into a session's agent and submit it; false when it cannot take one.
    pub fn type_into_session(&mut self, session_id: &str, message: &str, cx: &mut Context<Self>) -> bool {
        let Some(terminal) = self.terminals.get(session_id).cloned() else { return false };
        if glasscode_core::prompt::parse(&terminal.read(cx).text_tail(60)).is_some() {
            return false;
        }
        let line = message.split_whitespace().collect::<Vec<_>>().join(" ");
        terminal.update(cx, |t, cx| t.write(line.into_bytes(), cx));
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            let _ = terminal.update(cx, |t, cx| t.write(b"\r".to_vec(), cx));
        })
        .detach();
        true
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
