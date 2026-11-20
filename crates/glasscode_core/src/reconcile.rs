//! The reconciler (design D24's safety net, events-4): hooks are fast edges, and they get lost:
//! a hook held back by the trust dialog, hooks turned off, a Stop that never comes after an
//! interrupt, a spool written while Glasscode was down. So on start, on wake and every 60 s the
//! reconciler compares what is true now (running sessions, their activity, their pull requests,
//! the worktrees) with what the journal implies, and writes a `reconciled.*` event for every gap.
//! Each gap is reported once: the event it writes is itself folded into what the journal implies.
//!
//! It is pure: what the journal implies is folded from events ([`Reconciler::observe`]), and the
//! snapshot is taken by the app or glasscoded from what they hold in memory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::json;

use crate::agents::AgentActivity;
use crate::journal::{Event, Source, kinds};
use crate::paths::ProjectKey;

/// How often the reconciler runs while nothing else asks for it.
pub const EVERY_SECS: u64 = 60;

/// One session as it is now.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionNow {
    pub id: String,
    pub project: Option<ProjectKey>,
    /// Its agent's process runs.
    pub running: bool,
    pub activity: Option<AgentActivity>,
    /// Its pull request: repository, number and state (OPEN, MERGED, CLOSED), when one is known.
    pub pr: Option<(String, u64, String)>,
}

/// What is true now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub sessions: Vec<SessionNow>,
    /// Every worktree of every project, with its project.
    pub worktrees: Vec<(Option<ProjectKey>, PathBuf)>,
}

/// What the journal says of a session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Implied {
    running: Option<bool>,
    activity: Option<String>,
    pr: Option<(String, u64, String)>,
}

fn activity_name(activity: AgentActivity) -> &'static str {
    match activity {
        AgentActivity::Working => "working",
        AgentActivity::Idle => "idle",
        AgentActivity::NeedsInput => "needs_input",
    }
}

/// What the journal implies, and whether the first comparison was made.
#[derive(Clone, Debug, Default)]
pub struct Reconciler {
    sessions: BTreeMap<String, Implied>,
    worktrees: Option<BTreeSet<PathBuf>>,
    /// The project of each worktree, as the last snapshot said (for the archive of a worktree that
    /// goes, M24).
    projects: BTreeMap<PathBuf, ProjectKey>,
}

impl Reconciler {
    /// What a stretch of the journal implies (at the start: the last days of it).
    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a Event>) -> Reconciler {
        let mut reconciler = Reconciler::default();
        for event in events {
            reconciler.observe(event);
        }
        reconciler
    }

    /// Fold one event into what the journal implies.
    pub fn observe(&mut self, event: &Event) {
        let data = &event.data;
        let text = |key: &str| data.get(key).and_then(|v| v.as_str()).map(str::to_string);
        if event.kind == kinds::RECONCILED_WORKTREE_ADDED || event.kind == kinds::RECONCILED_WORKTREE_GONE || event.kind == kinds::RECONCILED_BASELINE {
            if let Some(list) = data.get("worktrees").and_then(|v| v.as_array()) {
                self.worktrees = Some(list.iter().filter_map(|v| v.as_str()).map(PathBuf::from).collect());
            }
        }
        // A quit ends every session it lists: their terminals close with the app (P10). Without
        // this, each one with no end hook of its own (a shell, an agent whose hook came too late)
        // was "gone, and no event said it ended" at the next start, a notice and a wake each time.
        if event.kind == kinds::APP_QUIT {
            for id in data.get("running").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str()) {
                let implied = self.sessions.entry(id.to_string()).or_default();
                implied.running = Some(false);
                implied.activity = None;
            }
            return;
        }
        let Some(session) = event.session.as_deref() else { return };
        let implied = self.sessions.entry(session.to_string()).or_default();
        match event.kind.as_str() {
            "session.started" | "reconciled.session_running" => implied.running = Some(true),
            "session.ended" | "reconciled.session_gone" => {
                implied.running = Some(false);
                implied.activity = None;
            }
            "session.activity" | "reconciled.activity" | "reconciled.missed_stop" => {
                implied.activity = text("activity");
                implied.running = Some(true);
            }
            "session.needs_input" => implied.activity = Some("needs_input".into()),
            "pr.opened" | "pr.changed" | "pr.merged" | "pr.closed" | "reconciled.pr" => {
                let state = match event.kind.as_str() {
                    "pr.merged" => Some("MERGED".to_string()),
                    "pr.closed" => Some("CLOSED".to_string()),
                    "pr.opened" => Some("OPEN".to_string()),
                    _ => text("state"),
                };
                if let (Some(repo), Some(number), Some(state)) = (text("repo"), data.get("number").and_then(|v| v.as_u64()), state) {
                    implied.pr = Some((repo, number, state));
                }
            }
            "reconciled.baseline" => {
                if let Some(state) = data.get("session") {
                    implied.running = state.get("running").and_then(|v| v.as_bool());
                    implied.activity = state.get("activity").and_then(|v| v.as_str()).map(str::to_string);
                }
            }
            _ => {}
        }
    }

    /// Compare `now` with what the journal implies: one `reconciled.*` event for each gap,
    /// already folded in, so the next run reports only new ones. A session the journal never
    /// heard of, and the worktrees at the first run, are a baseline (one event, no gaps).
    pub fn run(&mut self, now: &Snapshot) -> Vec<Event> {
        let mut out = Vec::new();
        let mut first_seen = Vec::new();
        for session in &now.sessions {
            let activity = session.activity.map(activity_name).map(str::to_string);
            let event = |kind, summary: String, data| Event::new(kind, Source::Reconcile, summary).session(Some(&session.id)).project(session.project.as_ref()).data(data);
            let Some(implied) = self.sessions.get(&session.id).filter(|implied| implied.running.is_some() || implied.activity.is_some()) else {
                first_seen.push(event(kinds::RECONCILED_BASELINE, "first seen".into(), json!({ "session": { "running": session.running, "activity": activity } })));
                continue;
            };
            let implied_running = implied.running.unwrap_or(implied.activity.is_some());
            match (implied_running, session.running) {
                (true, false) => out.push(event(kinds::RECONCILED_SESSION_GONE, "its agent is not running, and no event said it ended".into(), json!({ "implied": implied.activity }))),
                (false, true) => out.push(event(kinds::RECONCILED_SESSION_RUNNING, "it runs, and no event said it started".into(), json!({ "activity": activity }))),
                _ => {}
            }
            if session.running && implied.activity != activity && activity.is_some() {
                let missed_stop = implied.activity.as_deref() == Some("working") && activity.as_deref() != Some("working");
                let kind = if missed_stop { kinds::RECONCILED_MISSED_STOP } else { kinds::RECONCILED_ACTIVITY };
                let summary = format!("{} now; the journal said {}", activity.as_deref().unwrap_or("nothing"), implied.activity.as_deref().unwrap_or("nothing"));
                out.push(event(kind, summary, json!({ "activity": activity, "implied": implied.activity })));
            }
            if let Some((repo, number, state)) = &session.pr
                && implied.pr.as_ref() != session.pr.as_ref()
            {
                let summary = format!("{repo}#{number} is {}", state.to_lowercase());
                out.push(event(kinds::RECONCILED_PR, summary, json!({ "repo": repo, "number": number, "state": state, "implied": implied.pr.as_ref().map(|(r, n, s)| json!({ "repo": r, "number": n, "state": s })) })));
            }
        }
        let worktrees: BTreeSet<PathBuf> = now.worktrees.iter().map(|(_, path)| path.clone()).collect();
        let list = |set: &BTreeSet<PathBuf>| set.iter().map(|p| p.display().to_string()).collect::<Vec<_>>();
        match &self.worktrees {
            None => out.push(Event::new(kinds::RECONCILED_BASELINE, Source::Reconcile, format!("{} worktrees", worktrees.len())).data(json!({ "worktrees": list(&worktrees) }))),
            Some(known) => {
                let project_of = |path: &PathBuf| now.worktrees.iter().find(|(_, p)| p == path).and_then(|(project, _)| project.clone());
                for added in worktrees.difference(known) {
                    let data = json!({ "worktree": added.display().to_string(), "worktrees": list(&worktrees) });
                    out.push(Event::new(kinds::RECONCILED_WORKTREE_ADDED, Source::Reconcile, format!("worktree {}", added.display())).data(data).project(project_of(added).as_ref()));
                }
                for gone in known.difference(&worktrees) {
                    let data = json!({ "worktree": gone.display().to_string(), "worktrees": list(&worktrees) });
                    out.push(Event::new(kinds::RECONCILED_WORKTREE_GONE, Source::Reconcile, format!("worktree {} is gone", gone.display())).data(data).project(self.projects.get(gone)));
                }
            }
        }
        out.extend(first_seen);
        for event in &out {
            self.observe(event);
        }
        self.worktrees = Some(worktrees);
        self.projects = now.worktrees.iter().filter_map(|(project, path)| project.clone().map(|project| (path.clone(), project))).collect();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, running: bool, activity: Option<AgentActivity>) -> SessionNow {
        SessionNow { id: id.into(), project: None, running, activity, pr: None }
    }

    fn kinds_of(events: &[Event]) -> Vec<&str> {
        events.iter().map(|e| e.kind.as_str()).collect()
    }

    #[test]
    fn reconciler_reports_missed_stop() {
        // The journal saw the turn start, and nothing after it (the Stop hook was lost).
        let started = crate::journal::activity("s1", None, Some(AgentActivity::Idle), AgentActivity::Working);
        let mut reconciler = Reconciler::from_events([&started]);
        let now = Snapshot { sessions: vec![session("s1", true, Some(AgentActivity::Idle))], worktrees: Vec::new() };
        let events = reconciler.run(&now);
        assert_eq!(kinds_of(&events), ["reconciled.missed_stop", "reconciled.baseline"]);
        let missed = &events[0];
        assert_eq!((missed.session.as_deref(), missed.source), (Some("s1"), Source::Reconcile));
        assert_eq!((missed.data["activity"].as_str(), missed.data["implied"].as_str()), (Some("idle"), Some("working")));
        // Once: the next run finds nothing new.
        assert!(reconciler.run(&now).is_empty());
    }

    #[test]
    fn a_session_gone_or_running_without_an_event() {
        let started = Event::new(kinds::SESSION_STARTED, Source::Hook, "started").session(Some("s1"));
        let ended = Event::new(kinds::SESSION_ENDED, Source::Hook, "ended").session(Some("s2"));
        let mut reconciler = Reconciler::from_events([&started, &ended]);
        let now = Snapshot { sessions: vec![session("s1", false, None), session("s2", true, Some(AgentActivity::Idle)), session("s3", true, None)], worktrees: Vec::new() };
        let events = reconciler.run(&now);
        assert_eq!(kinds_of(&events), ["reconciled.session_gone", "reconciled.session_running", "reconciled.activity", "reconciled.baseline", "reconciled.baseline"]);
        // s3 was never heard of: a baseline, not a gap.
        assert_eq!(events[4].session.as_deref(), Some("s3"));
        assert!(reconciler.run(&now).is_empty());
    }

    /// A quit ended what it listed; a force quit (no `app.quit`) leaves the gap to report.
    #[test]
    fn a_quit_ends_the_sessions_it_lists() {
        let started = |id: &str| Event::new(kinds::SESSION_STARTED, Source::Hook, "started").session(Some(id));
        let working = crate::journal::activity("s2", None, Some(AgentActivity::Idle), AgentActivity::Working);
        let quit = Event::new(kinds::APP_QUIT, Source::App, "Glasscode quit").data(json!({ "running": ["s1", "s2"] }));
        let now = Snapshot { sessions: vec![session("s1", false, None), session("s2", false, None), session("s3", false, None)], ..Snapshot::default() };
        let mut reconciler = Reconciler::from_events([&started("s1"), &working, &started("s3"), &quit]);
        let events = reconciler.run(&now);
        assert_eq!(events.iter().filter(|e| e.kind == kinds::RECONCILED_SESSION_GONE).filter_map(|e| e.session.as_deref()).collect::<Vec<_>>(), ["s3"], "only the one the quit did not list");
        // Killed: no quit in the journal, so both are gaps.
        let mut killed = Reconciler::from_events([&started("s1"), &working]);
        let gone: Vec<_> = killed.run(&now).into_iter().filter(|e| e.kind == kinds::RECONCILED_SESSION_GONE).filter_map(|e| e.session).collect();
        assert_eq!(gone, ["s1", "s2"]);
    }

    #[test]
    fn pull_requests_and_worktrees_the_journal_missed() {
        let opened = Event::new(kinds::PR_OPENED, Source::Forge, "opened").session(Some("s1")).data(json!({ "repo": "acme/app", "number": 7 }));
        let active = crate::journal::activity("s1", None, None, AgentActivity::Idle);
        let mut reconciler = Reconciler::from_events([&opened, &active]);
        let mut now = Snapshot {
            sessions: vec![SessionNow { pr: Some(("acme/app".into(), 7, "OPEN".into())), ..session("s1", true, Some(AgentActivity::Idle)) }],
            worktrees: vec![(None, "/w/a".into())],
        };
        assert_eq!(kinds_of(&reconciler.run(&now)), ["reconciled.baseline"]);
        // Merged while nobody looked, and a worktree came and one went.
        now.sessions[0].pr = Some(("acme/app".into(), 7, "MERGED".into()));
        now.worktrees = vec![(None, "/w/b".into())];
        let events = reconciler.run(&now);
        assert_eq!(kinds_of(&events), ["reconciled.pr", "reconciled.worktree_added", "reconciled.worktree_gone"]);
        assert!(reconciler.run(&now).is_empty());
        // After a restart, the journal's own record of them is a baseline as good.
        let mut again = Reconciler::from_events(events.iter().chain([&opened, &active]).collect::<Vec<_>>());
        again.worktrees = Some(["/w/b".into()].into_iter().collect());
        let replay = Reconciler::from_events(&events);
        assert_eq!(replay.worktrees, again.worktrees);
    }
}
