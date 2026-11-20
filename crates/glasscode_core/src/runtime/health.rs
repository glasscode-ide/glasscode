//! Whether a brain can think, and why not (M50 step 6; brain-14, alt-11). Each state has one plain
//! sentence and a time to try again. A change of state is one journal event
//! (`brain.down` or `brain.up`) and one notification, never one per failed wake. When no brain
//! can run, everything that needs no model keeps working (the journal, turn reports, rules,
//! playbooks with fixed actions, notifications, the chat's answers from code): that is the
//! no-model mode, and the fallback for a CLI that stops serving `-p` on a subscription. There is
//! no hidden-terminal fallback, and no fall-back to a weaker profile (brain-m1).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::AgentKind;

/// A brain's state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BrainHealth {
    Ready,
    /// The CLI is not installed (not on the PATH).
    NotInstalled,
    /// Its account is signed out.
    SignedOut,
    /// Its login expires soon (at this Unix time).
    LoginExpiresSoon { at: i64 },
    /// Its plan is used up until then (Unix time, when the CLI says).
    OutOfUsage { until: Option<i64> },
    /// The vendor is overloaded: it is tried again after the backoff.
    Overloaded { tries: u32 },
    /// The CLI's version is not one the team approved.
    VersionNotApproved { version: String },
    /// The CLI is older than the manager needs (`versions::MIN_CLAUDE`, `MIN_CODEX`): it does not
    /// start on it.
    VersionTooOld { version: String, needs: String },
    /// It stopped by itself.
    Crashed { why: String },
    /// Its tools are not exactly Glasscode's (D4): it was stopped before it could act.
    ToolListWrong { extra: Vec<String>, missing: Vec<String> },
    /// The person switched it off.
    SwitchedOff,
}

impl BrainHealth {
    /// Whether it can take a turn.
    pub fn can_think(&self) -> bool {
        matches!(self, BrainHealth::Ready | BrainHealth::LoginExpiresSoon { .. })
    }

    /// The sentence the chat and the notification say, in the person's words.
    pub fn sentence(&self, agent: AgentKind) -> String {
        let cli = agent.name();
        match self {
            BrainHealth::Ready => format!("The manager thinks with {cli}."),
            BrainHealth::NotInstalled => format!("{cli} is not installed, so the manager cannot think. Rules, playbooks and notifications keep working."),
            BrainHealth::SignedOut => format!("The manager's {cli} account is signed out. Sign in again in Settings › Accounts; until then it only answers from what it knows."),
            BrainHealth::LoginExpiresSoon { .. } => format!("The manager's {cli} login expires soon. Sign in again in Settings › Accounts so that it keeps thinking."),
            BrainHealth::OutOfUsage { until: Some(at) } => format!("The manager's {cli} plan is used up until {}. Rules, playbooks and notifications keep working.", crate::day::clock(*at)),
            BrainHealth::OutOfUsage { until: None } => format!("The manager's {cli} plan is used up for now. Rules, playbooks and notifications keep working."),
            BrainHealth::Overloaded { .. } => format!("{cli} is overloaded; the manager tries again in a moment."),
            BrainHealth::VersionNotApproved { version } => format!("{cli} {version} is not a version your team approved, so the manager does not run on it. Settings › Updates has the approved ones."),
            BrainHealth::VersionTooOld { version, needs } => format!("{cli} {version} is older than the manager needs, so it does not start: it needs {cli} {needs} or newer. Update it in Settings › Updates."),
            BrainHealth::Crashed { why } => format!("The manager's {cli} stopped ({why}). It starts again with the next question."),
            BrainHealth::ToolListWrong { extra, missing } => {
                let mut parts = Vec::new();
                if !extra.is_empty() {
                    parts.push(format!("it had {}", extra.join(", ")));
                }
                if !missing.is_empty() {
                    parts.push(format!("it lacked {}", missing.join(", ")));
                }
                format!("The manager's {cli} did not start with only Glasscode's tools ({}), so it was stopped before it could act.", parts.join("; "))
            }
            BrainHealth::SwitchedOff => "The manager is switched off.".to_string(),
        }
    }

    /// When to look again: none for a state only the person ends (signed out, not installed, the
    /// wrong version or tools, switched off).
    pub fn backoff(&self) -> Option<Duration> {
        match self {
            BrainHealth::Ready | BrainHealth::LoginExpiresSoon { .. } => Some(Duration::ZERO),
            BrainHealth::Overloaded { tries } => Some(Duration::from_secs(15u64.saturating_mul(1 << (*tries).min(6)).min(15 * 60))),
            BrainHealth::OutOfUsage { until: Some(at) } => Some(Duration::from_secs(((*at - crate::clock::now_ms() as i64 / 1000).max(60)) as u64)),
            BrainHealth::OutOfUsage { until: None } => Some(Duration::from_secs(30 * 60)),
            BrainHealth::Crashed { .. } => Some(Duration::from_secs(5)),
            BrainHealth::NotInstalled | BrainHealth::SignedOut | BrainHealth::VersionNotApproved { .. } | BrainHealth::VersionTooOld { .. } | BrainHealth::ToolListWrong { .. } | BrainHealth::SwitchedOff => None,
        }
    }

    /// The word for the journal and the chat's status.
    pub fn word(&self) -> &'static str {
        match self {
            BrainHealth::Ready => "ready",
            BrainHealth::NotInstalled => "not_installed",
            BrainHealth::SignedOut => "signed_out",
            BrainHealth::LoginExpiresSoon { .. } => "login_expires_soon",
            BrainHealth::OutOfUsage { .. } => "out_of_usage",
            BrainHealth::Overloaded { .. } => "overloaded",
            BrainHealth::VersionNotApproved { .. } => "version_not_approved",
            BrainHealth::VersionTooOld { .. } => "version_too_old",
            BrainHealth::Crashed { .. } => "crashed",
            BrainHealth::ToolListWrong { .. } => "tool_list_wrong",
            BrainHealth::SwitchedOff => "switched_off",
        }
    }
}

/// What the chat says when no brain can think (the no-model mode, alt-11).
pub fn thinking_unavailable(health: &BrainHealth, agent: AgentKind) -> String {
    format!("{} I can still answer from what Glasscode knows (sessions, the board, pull requests), and rules and playbooks keep running.", health.sentence(agent))
}

/// The state a brain's failure text says, from the CLI's own words (its stderr, or a result
/// marked as an error); none when it does not say one of them.
pub fn from_failure(text: &str, overloaded_tries: u32) -> Option<BrainHealth> {
    let t = text.to_lowercase();
    if t.contains("/login") || t.contains("not logged in") || t.contains("invalid api key") || t.contains("oauth token has expired") || t.contains("please sign in") || t.contains("401") && t.contains("auth") {
        return Some(BrainHealth::SignedOut);
    }
    if t.contains("usage limit") || t.contains("limit reached") || t.contains("rate_limit") || t.contains("credit balance is too low") || t.contains("quota") {
        return Some(BrainHealth::OutOfUsage { until: None });
    }
    if t.contains("overloaded") || t.contains("529") || t.contains("503") || t.contains("server is busy") {
        return Some(BrainHealth::Overloaded { tries: overloaded_tries + 1 });
    }
    None
}

/// The state Claude's `auth status --json` says (`loggedIn`, and an expiry when it gives one).
pub fn from_claude_auth(status: &Value, now_secs: i64) -> BrainHealth {
    if status["loggedIn"] != Value::Bool(true) {
        return BrainHealth::SignedOut;
    }
    let expires = status["expiresAt"].as_i64().map(|ms| if ms > 10_000_000_000 { ms / 1000 } else { ms });
    match expires {
        Some(at) if at <= now_secs => BrainHealth::SignedOut,
        Some(at) if at - now_secs < 24 * 3600 => BrainHealth::LoginExpiresSoon { at },
        _ => BrainHealth::Ready,
    }
}

/// The state a `rate_limit_event` of the stream says: out of usage when any window's status is not
/// "allowed" (whichever windows the vendor reports, brain-6).
pub fn from_rate_limit(event: &Value) -> Option<BrainHealth> {
    let info = &event["rate_limit_info"];
    let status = info["status"].as_str().or_else(|| event["status"].as_str())?;
    if matches!(status, "allowed" | "allowed_warning") {
        return None;
    }
    let until = info["resetsAt"].as_i64().or_else(|| event["resetsAt"].as_i64());
    Some(BrainHealth::OutOfUsage { until })
}

/// One change of state, once: what the journal and the notification get (none when it did not
/// change kind; a second overloaded try is no news).
pub fn change(before: &BrainHealth, after: &BrainHealth) -> Option<(crate::journal_kinds::EventKind, bool)> {
    if before.word() == after.word() {
        return None;
    }
    let up = *after == BrainHealth::Ready;
    Some((if up { crate::journal_kinds::MANAGER_BRAIN_UP } else { crate::journal_kinds::MANAGER_BRAIN_DOWN }, up))
}

/// The journal event of a change (`change`): its kind, the sentence, and the state as data.
pub fn event(after: &BrainHealth, agent: AgentKind, lane: crate::caller::Lane) -> crate::journal::Event {
    let kind = if *after == BrainHealth::Ready { crate::journal_kinds::MANAGER_BRAIN_UP } else { crate::journal_kinds::MANAGER_BRAIN_DOWN };
    let mut event = crate::journal::Event::new(kind, crate::journal::Source::App, after.sentence(agent));
    event.data = serde_json::json!({ "agent": agent.name().to_lowercase(), "lane": lane, "health": after });
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn health_states_map_to_sentences() {
        let all = [
            BrainHealth::Ready,
            BrainHealth::NotInstalled,
            BrainHealth::SignedOut,
            BrainHealth::LoginExpiresSoon { at: 1_800_000_000 },
            BrainHealth::OutOfUsage { until: Some(1_800_000_000) },
            BrainHealth::OutOfUsage { until: None },
            BrainHealth::Overloaded { tries: 2 },
            BrainHealth::VersionNotApproved { version: "9.9.9".into() },
            BrainHealth::VersionTooOld { version: "2.0.1".into(), needs: "2.1.322".into() },
            BrainHealth::Crashed { why: "exit 1".into() },
            BrainHealth::ToolListWrong { extra: vec!["Bash".into()], missing: vec![] },
            BrainHealth::SwitchedOff,
        ];
        let mut sentences = std::collections::BTreeSet::new();
        for state in &all {
            for agent in [AgentKind::Claude, AgentKind::Codex] {
                let sentence = state.sentence(agent);
                assert!(sentence.ends_with('.') && !sentence.contains("{"), "{sentence}");
                sentences.insert(sentence);
            }
            // Only the person ends these; the others are tried again.
            let person_ends = matches!(state, BrainHealth::NotInstalled | BrainHealth::SignedOut | BrainHealth::VersionNotApproved { .. } | BrainHealth::VersionTooOld { .. } | BrainHealth::ToolListWrong { .. } | BrainHealth::SwitchedOff);
            assert_eq!(state.backoff().is_none(), person_ends, "{state:?}");
        }
        // Each state says something of its own for each CLI, but "switched off", which names none.
        assert_eq!(sentences.len(), all.len() * 2 - 1);
        assert!(BrainHealth::ToolListWrong { extra: vec!["Bash".into()], missing: vec!["mcp__glasscode__ui_state".into()] }.sentence(AgentKind::Claude).contains("it had Bash; it lacked mcp__glasscode__ui_state"));
        assert!(BrainHealth::Overloaded { tries: 1 }.backoff() < BrainHealth::Overloaded { tries: 4 }.backoff());
        assert!(thinking_unavailable(&BrainHealth::SignedOut, AgentKind::Claude).contains("rules and playbooks keep running"));
    }

    #[test]
    fn failures_and_signals_say_the_state() {
        assert_eq!(from_failure("Invalid API key · Please run /login", 0), Some(BrainHealth::SignedOut));
        assert_eq!(from_failure("Claude AI usage limit reached|1800000000", 0), Some(BrainHealth::OutOfUsage { until: None }));
        assert_eq!(from_failure("API Error: 529 Overloaded", 2), Some(BrainHealth::Overloaded { tries: 3 }));
        assert_eq!(from_failure("something else", 0), None);
        assert_eq!(from_claude_auth(&json!({ "loggedIn": false }), 0), BrainHealth::SignedOut);
        assert_eq!(from_claude_auth(&json!({ "loggedIn": true, "authMethod": "claude.ai" }), 0), BrainHealth::Ready);
        assert_eq!(from_claude_auth(&json!({ "loggedIn": true, "expiresAt": 10_003_600_i64 }), 10_000_000), BrainHealth::LoginExpiresSoon { at: 10_003_600 });
        assert_eq!(from_claude_auth(&json!({ "loggedIn": true, "expiresAt": 9_000_000_i64 }), 10_000_000), BrainHealth::SignedOut, "expired");
        // Every window counts, whichever the vendor reports (the seven-day one on P01's plan).
        assert_eq!(from_rate_limit(&json!({ "type": "rate_limit_event", "rate_limit_info": { "status": "rejected", "rateLimitType": "seven_day", "resetsAt": 1_800_000_000 } })), Some(BrainHealth::OutOfUsage { until: Some(1_800_000_000) }));
        assert_eq!(from_rate_limit(&json!({ "type": "rate_limit_event", "rate_limit_info": { "status": "allowed" } })), None);
    }

    #[test]
    fn a_change_is_one_event() {
        let (down, _) = change(&BrainHealth::Ready, &BrainHealth::SignedOut).unwrap();
        assert_eq!(down, crate::journal_kinds::MANAGER_BRAIN_DOWN);
        assert_eq!(change(&BrainHealth::Overloaded { tries: 1 }, &BrainHealth::Overloaded { tries: 2 }), None, "a second try is no news");
        assert_eq!(change(&BrainHealth::SignedOut, &BrainHealth::Ready), Some((crate::journal_kinds::MANAGER_BRAIN_UP, true)));
        assert_eq!(event(&BrainHealth::SignedOut, AgentKind::Claude, crate::caller::Lane::Chat).data["health"]["state"], json!("signed_out"));
        assert!(change(&BrainHealth::SignedOut, &BrainHealth::OutOfUsage { until: None }).is_some(), "another reason to be down is said");
    }
}
