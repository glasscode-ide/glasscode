//! `glasscode manager …`: what its handlers (`ops_run.rs`) share. What the manager's own brain, a
//! session's agent or the person can ask of the manager from a terminal: reading is free; what
//! changes the manager's own data (its memory, follow-ups, drafts, checks) counts as the Today
//! board does in Settings › Automation; showing, drawing and speaking count as changing what the
//! window shows. A message to a teammate is only ever drafted here: allowing one is the person's
//! alone.

use std::path::{Path, PathBuf};
use std::time::Instant;

use glasscode_core::control::{Reply, code};
use glasscode_core::manager::{Check, FollowState, FollowUp, MsgState, OutMessage, PanelTab, Settings, Tier, followups, messages};
use serde_json::{Value, json};

use crate::control_ui::{Caller, Trust};
use crate::workspace::Workspace;

pub(crate) const OFF: &str = "The manager is off. Switch it on in Settings › Manager.";

/// A voice token is good for this long.
pub(crate) const TOKEN_SECS: u64 = 3600;

/// The screens `show --view` knows.
pub(crate) const VIEWS: &[&str] = &["manager", "sessions", "today", "tickets", "usage", "settings"];

/// The parts of a session's right panel `show --section` can point at.
pub(crate) const SECTIONS: &[&str] = &["checks", "preview", "reviews", "comments", "changes", "files", "reply"];

fn usage(text: impl Into<String>) -> Reply {
    Reply::fail(code::USAGE, text)
}

fn failed(text: impl Into<String>) -> Reply {
    Reply::fail(code::FAILED, text)
}

fn denied(text: impl Into<String>) -> Reply {
    Reply::fail(code::DENIED, text)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// At most `max` characters of one line, with "…" when something was cut.
pub(crate) fn clip(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub(crate) fn to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_default()
}

/// "12 Nov" of a Unix time.
pub(crate) fn day_of(unix: u64) -> String {
    glasscode_core::day::short(glasscode_core::day::local(unix as i64))
}

/// A relative path is from where the command was run.
pub(crate) fn expand(path: &str, base: Option<&Path>) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        return glasscode_core::paths::home().join(rest);
    }
    let p = PathBuf::from(path);
    match base {
        Some(base) if p.is_relative() => base.join(p),
        _ => p,
    }
}

/// "30", "30m", "2h", "1d" as minutes.
pub(crate) fn minutes(text: &str) -> Option<u64> {
    let t = text.trim().to_lowercase();
    let (number, times) = match t.chars().last()? {
        'm' => (&t[..t.len() - 1], 1),
        'h' => (&t[..t.len() - 1], 60),
        'd' => (&t[..t.len() - 1], 1440),
        _ => (t.as_str(), 1),
    };
    number.trim().parse::<u64>().ok().filter(|n| *n > 0).map(|n| n * times)
}

/// "every 30 minutes", "every 2 hours".
pub(crate) fn every(secs: u64) -> String {
    match secs / 60 {
        m if m % 1440 == 0 => format!("every {}", plural((m / 1440) as usize, "day", "days")),
        m if m % 60 == 0 => format!("every {}", plural((m / 60) as usize, "hour", "hours")),
        m => format!("every {}", plural(m as usize, "minute", "minutes")),
    }
}

/// Who a note or a follow-up comes from: "you", "manager" or "session:<id>".
pub(crate) fn source_of(caller: &Caller) -> String {
    match (&caller.session, caller.trust) {
        (_, Trust::You) => "you".into(),
        (Some(id), _) => format!("session:{id}"),
        (None, Trust::Session) => "manager".into(),
        (None, Trust::Outside) => "outside".into(),
    }
}

pub(crate) fn tab_of(word: &str) -> Result<PanelTab, Reply> {
    match word.to_lowercase().replace(['-', '_', ' '], "").as_str() {
        "pr" | "pullrequest" | "pull" => Ok(PanelTab::PullRequest),
        "changes" | "diff" => Ok(PanelTab::Changes),
        "files" => Ok(PanelTab::Files),
        _ => Err(usage(format!("“{word}” is not a tab: pr, changes or files."))),
    }
}

/// The tab a part of the panel is on.
pub(crate) fn tab_for(section: &str) -> Option<PanelTab> {
    match section {
        "checks" | "preview" | "reviews" | "comments" => Some(PanelTab::PullRequest),
        "changes" => Some(PanelTab::Changes),
        "files" => Some(PanelTab::Files),
        _ => None,
    }
}

pub(crate) fn follow_up_line(f: &FollowUp) -> String {
    let state = match f.state {
        FollowState::Open => "open",
        FollowState::Fixed => "fixed?",
        FollowState::Done => "done",
        FollowState::Dropped => "dropped",
    };
    let what = if f.number > 0 { format!("{}#{} “{}”", f.repo, f.number, clip(&f.title, 60)) } else { format!("“{}”", clip(if f.title.is_empty() { &f.note } else { &f.title }, 90)) };
    let who = match (f.person.trim(), f.login.trim()) {
        ("", "") => String::new(),
        ("", login) => format!("  · @{login}"),
        (person, _) => format!("  · {person}"),
    };
    let blockers = if f.blockers.is_empty() { String::new() } else { format!("  · {} of {} still open", f.open_blockers(), f.blockers.len()) };
    format!("{}  [{state}] {what}{who}{blockers}", short(&f.id))
}

pub(crate) fn message_state(m: &OutMessage) -> &'static str {
    match (m.state, m.tier) {
        (MsgState::Draft, Tier::Never) => "never sent",
        (MsgState::Draft, _) => "waits for the person",
        (MsgState::Approved, _) => "allowed, going out",
        (MsgState::Sent, _) => "sent",
        (MsgState::Rejected, _) => "rejected",
        (MsgState::Failed, _) => "failed",
    }
}

pub(crate) fn message_line(m: &OutMessage) -> String {
    let why = if m.why.trim().is_empty() { String::new() } else { format!("  (why: {})", clip(&m.why, 70)) };
    let error = m.error.as_deref().filter(|e| !e.is_empty()).map(|e| format!("  ({})", clip(e, 70))).unwrap_or_default();
    format!("{}  [{}] to {}: “{}”{why}{error}", short(&m.id), message_state(m), m.to, clip(&m.body, 110))
}

pub(crate) fn check_line(c: &Check) -> String {
    let when = if c.once { format!("once, {} after it was added", every(c.every_secs).trim_start_matches("every ")) } else { every(c.every_secs) };
    format!("{}  {}  · {when}", short(&c.id), clip(&c.what, 100))
}

/// The one thing in a list that `which` names: its id, the start of its id, or words from
/// its text. `Err` says why not (nothing, or several).
pub(crate) fn pick<'a, T>(items: &'a [T], which: &str, id: impl Fn(&T) -> &str, text: impl Fn(&T) -> String, noun: &str) -> Result<&'a T, Reply> {
    let w = which.trim();
    if let Some(exact) = items.iter().find(|i| id(i) == w) {
        return Ok(exact);
    }
    let starts: Vec<&T> = items.iter().filter(|i| w.len() >= 4 && id(i).starts_with(w)).collect();
    if let [one] = starts.as_slice() {
        return Ok(one);
    }
    let words: Vec<String> = w.to_lowercase().split_whitespace().map(str::to_string).collect();
    let hits: Vec<&T> = items
        .iter()
        .filter(|i| {
            let t = text(i).to_lowercase();
            !words.is_empty() && words.iter().all(|x| t.contains(x))
        })
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(failed(format!("No {noun} “{w}”."))),
        many => Err(usage(format!("“{w}” matches {} of them: {}. Use its id.", many.len(), many.iter().take(6).map(|i| short(id(i))).collect::<Vec<_>>().join(", ")))),
    }
}

impl Workspace {
    /// `manager status`: also answers while the manager is off.
    pub(crate) fn manager_status_reply(&self) -> Reply {
        let settings = &self.state.manager;
        let ui = &self.manager;
        let home = match settings.home {
            glasscode_core::manager::Home::Mac => "mac",
            glasscode_core::manager::Home::Server => "server",
        };
        if !settings.enabled {
            let data = json!({ "enabled": false, "name": settings.name, "initials": settings.initials(), "state": "idle", "caption": "", "needs_you": 0, "follow_ups_open": 0, "messages_waiting": 0, "home": home, "on_server": false });
            return Reply::ok(OFF, data);
        }
        let state = if ui.thinking {
            "thinking"
        } else if ui.speaking_until.is_some_and(|until| until > Instant::now()) {
            "speaking"
        } else {
            "idle"
        };
        let needs_you = glasscode_core::manager::brief::needs_you(&self.v2_snapshot(), self.follow_ups(), self.manager_outbox());
        let follow_ups = followups::open(self.follow_ups()).len();
        let waiting = messages::waiting_in(self.manager_outbox()).len();
        // Where it acts: the home's lease says (D5, M57).
        let on_server = !self.acts_as_home();
        let mut text = format!(
            "{} is on ({state}), running on {}.\nNeeds you: {needs_you} · follow-ups open: {follow_ups} · messages waiting for your OK: {waiting}.",
            settings.name,
            if on_server { "the server" } else { "this Mac" }
        );
        if !ui.caption.trim().is_empty() {
            text.push_str(&format!("\nLast said: “{}”", clip(&ui.caption, 160)));
        }
        Reply::ok(
            text,
            json!({
                "enabled": true,
                "name": settings.name,
                "initials": settings.initials(),
                "state": state,
                "caption": ui.caption,
                "needs_you": needs_you,
                "follow_ups_open": follow_ups,
                "messages_waiting": waiting,
                "home": home,
                "on_server": on_server,
            }),
        )
    }
    /// A session the manager may show: (its id, its title). One on this Mac, or one it sees on
    /// the server; never one in a project it does not manage.
    pub(crate) fn manager_find_session(&self, word: &str, caller: &Caller, settings: &Settings) -> Result<(String, String), Reply> {
        match self.find_session(word, caller) {
            Ok(id) => {
                let Some(session) = self.state.session(&id) else { return Err(failed("That session is gone.")) };
                let title = self.session_title(session);
                if !settings.manages(&session.project_id) {
                    return Err(denied(format!("“{title}” is in a project {} does not manage (Settings › Manager).", settings.name)));
                }
                Ok((id, title))
            }
            Err(not_here) => {
                let snap = self.v2_snapshot();
                let w = word.trim();
                let words: Vec<String> = w.to_lowercase().split_whitespace().map(str::to_string).collect();
                let hits: Vec<_> = snap
                    .sessions
                    .iter()
                    .filter(|s| {
                        let title = s.title.to_lowercase();
                        s.id == w || (w.len() >= 4 && s.id.starts_with(w)) || (!words.is_empty() && words.iter().all(|x| title.contains(x)))
                    })
                    .collect();
                match hits.as_slice() {
                    [one] => Ok((one.id.clone(), one.title.clone())),
                    _ => Err(not_here),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(trust: Trust, session: Option<&str>) -> Caller {
        Caller { who: "x".into(), trust, channel: glasscode_core::caller::Caller::Ui, session: session.map(str::to_string), session_title: None, agent: None, cwd: None, project: None, drive: None }
    }

    #[test]
    fn times_read_as_minutes() {
        assert_eq!(minutes("30"), Some(30));
        assert_eq!(minutes("45m"), Some(45));
        assert_eq!(minutes("2h"), Some(120));
        assert_eq!(minutes("1d"), Some(1440));
        assert_eq!(minutes("0"), None);
        assert_eq!(minutes("soon"), None);
        assert_eq!(every(30 * 60), "every 30 minutes");
        assert_eq!(every(3600), "every 1 hour");
        assert_eq!(every(2 * 86_400), "every 2 days");
    }

    #[test]
    fn who_a_note_comes_from() {
        assert_eq!(source_of(&caller(Trust::You, None)), "you");
        assert_eq!(source_of(&caller(Trust::Session, Some("s1"))), "session:s1");
        assert_eq!(source_of(&caller(Trust::Session, None)), "manager");
        assert!(caller(Trust::Session, None).is_manager());
        assert!(!caller(Trust::Session, Some("s1")).is_manager() && !caller(Trust::You, None).is_manager() && !caller(Trust::Outside, None).is_manager());
    }

    #[test]
    fn tabs_and_sections() {
        assert_eq!(tab_of("pr").ok(), Some(PanelTab::PullRequest));
        assert_eq!(tab_of("Pull-Request").ok(), Some(PanelTab::PullRequest));
        assert_eq!(tab_of("diff").ok(), Some(PanelTab::Changes));
        assert!(tab_of("terminal").is_err());
        assert_eq!(tab_for("checks"), Some(PanelTab::PullRequest));
        assert_eq!(tab_for("files"), Some(PanelTab::Files));
        assert_eq!(tab_for("reply"), None);
    }

    #[test]
    fn picking_by_id_start_or_words() {
        let checks = vec![
            Check { id: "aaaa1111-x".into(), what: "see whether 1111 was merged".into(), ..Check::default() },
            Check { id: "bbbb2222-x".into(), what: "see whether the deploy went out".into(), ..Check::default() },
        ];
        let by = |which: &str| pick(&checks, which, |c| &c.id, |c| c.what.clone(), "check").map(|c| c.id.clone());
        assert_eq!(by("aaaa1111-x").ok().as_deref(), Some("aaaa1111-x"));
        assert_eq!(by("bbbb").ok().as_deref(), Some("bbbb2222-x"));
        assert_eq!(by("deploy").ok().as_deref(), Some("bbbb2222-x"));
        assert!(by("see whether").is_err());
        assert!(by("nothing like it").is_err());
    }

    #[test]
    fn lines_say_where_things_stand() {
        let draft = OutMessage { id: "m1m1m1m1-x".into(), to: "Sam".into(), body: "The fix is pushed.".into(), why: "he asked".into(), ..OutMessage::default() };
        assert_eq!(message_line(&draft), "m1m1m1m1  [waits for the person] to Sam: “The fix is pushed.”  (why: he asked)");
        assert_eq!(message_state(&OutMessage { tier: Tier::Never, ..OutMessage::default() }), "never sent");
        let follow = FollowUp { id: "f1f1f1f1-x".into(), repo: "acme/shop".into(), number: 12, title: "Retry queue".into(), person: "Mira".into(), ..FollowUp::default() };
        assert_eq!(follow_up_line(&follow), "f1f1f1f1  [open] acme/shop#12 “Retry queue”  · Mira");
        let once = Check { id: "c1c1c1c1-x".into(), what: "look at the deploy".into(), every_secs: 1800, once: true, ..Check::default() };
        assert_eq!(check_line(&once), "c1c1c1c1  look at the deploy  · once, 30 minutes after it was added");
    }
}

