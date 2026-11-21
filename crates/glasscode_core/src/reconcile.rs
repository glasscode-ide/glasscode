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
    /// The app started its agent a moment ago (within [`STARTING_SECS`]): its start hook may not
    /// have come yet, so a start the journal has not heard of is not a gap until then.
    pub starting: bool,
}

/// How long after the app starts an agent its start hook may still be on its way.
pub const STARTING_SECS: u64 = 60;

/// What is true now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub sessions: Vec<SessionNow>,
    /// Each project's worktrees as git listed them now (None: nothing to say of worktrees at all).
    /// A project whose list is not read (just after a start, or git cannot list it) is no project
    /// without worktrees: compared with what the journal knows, each of its worktrees would be
    /// "gone", and memory archives the work of a worktree that goes (M24). Only its own worktrees
    /// wait; the other projects' are compared as usual.
    pub worktrees: Option<Vec<ProjectWorktrees>>,
}

/// One project's worktrees, as git listed them now.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectWorktrees {
    pub project: Option<ProjectKey>,
    /// Its name, for the line that says git cannot list it.
    pub name: String,
    /// Its worktrees, or why git could not list them (None: not read yet, just after a start).
    pub list: Result<Vec<PathBuf>, Option<String>>,
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
    /// The project of each worktree, as the last snapshot (or the journal) said: for the archive of
    /// a worktree that goes (M24), and to tell whose list a known worktree waits on.
    projects: BTreeMap<PathBuf, ProjectKey>,
    /// The projects whose list git could not read, said once (until it reads again).
    unreadable: BTreeSet<String>,
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
            if let Some(projects) = data.get("projects").and_then(|v| v.as_object()) {
                for (path, key) in projects {
                    if let Some(key) = key.as_str().and_then(ProjectKey::parse) {
                        self.projects.insert(PathBuf::from(path), key);
                    }
                }
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
                // The app is starting it (a restore at the start): its SessionStart is on its way.
                (false, true) if session.starting => {}
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
        let known_after = now.worktrees.as_ref().map(|projects| self.worktrees_run(projects, &mut out));
        out.extend(first_seen);
        for event in &out {
            self.observe(event);
        }
        if let Some(Some((known, projects))) = known_after {
            self.worktrees = Some(known);
            self.projects = projects;
        }
        out
    }

    /// The worktrees' part of a run: each project whose list git read is compared with what the
    /// journal knows of it; a project not read keeps what is known of it, and one git cannot list
    /// is said once (which project, and why). A first baseline waits for every list. Gives what is
    /// known after, when it changed.
    fn worktrees_run(&mut self, now: &[ProjectWorktrees], out: &mut Vec<Event>) -> Option<(BTreeSet<PathBuf>, BTreeMap<PathBuf, ProjectKey>)> {
        let tag = |p: &ProjectWorktrees| p.project.as_ref().map(|k| k.as_str().to_string()).unwrap_or_else(|| p.name.clone());
        for p in now {
            match &p.list {
                Err(Some(why)) if self.unreadable.insert(tag(p)) => {
                    let why = format!("git cannot list the worktrees of {} ({why}); they are not checked until it can", p.name);
                    out.push(crate::journal::source_degraded("worktrees", &why).project(p.project.as_ref()).data(json!({ "source": "worktrees", "project": p.name, "why": why })));
                }
                Ok(_) if self.unreadable.remove(&tag(p)) => out.push(crate::journal::source_recovered("worktrees").project(p.project.as_ref()).data(json!({ "source": "worktrees", "project": p.name }))),
                _ => {}
            }
        }
        let read: Vec<&ProjectWorktrees> = now.iter().filter(|p| p.list.is_ok()).collect();
        let all_read = read.len() == now.len();
        let listed: BTreeSet<PathBuf> = read.iter().flat_map(|p| p.list.as_ref().map(|l| l.clone()).unwrap_or_default()).collect();
        let project_of = |path: &PathBuf| read.iter().find(|p| p.list.as_ref().is_ok_and(|l| l.contains(path))).and_then(|p| p.project.clone());
        let read_keys: BTreeSet<ProjectKey> = read.iter().filter_map(|p| p.project.clone()).collect();
        let Some(known) = self.worktrees.clone() else {
            // The first baseline: of every project, once all are read.
            if !all_read {
                return None;
            }
            let projects: BTreeMap<PathBuf, ProjectKey> = listed.iter().filter_map(|path| project_of(path).map(|k| (path.clone(), k))).collect();
            out.push(Event::new(kinds::RECONCILED_BASELINE, Source::Reconcile, format!("{} worktrees", listed.len())).data(json!({ "worktrees": paths_of(&listed), "projects": projects_of(&projects) })));
            return Some((listed, projects));
        };
        // A known worktree is gone only when its project's list was read (one of a project not
        // known here waits until every list is read).
        let gone: Vec<PathBuf> = known
            .difference(&listed)
            .filter(|path| match self.projects.get(*path) {
                Some(key) => read_keys.contains(key),
                None => all_read,
            })
            .cloned()
            .collect();
        let added: Vec<PathBuf> = listed.difference(&known).cloned().collect();
        let mut after: BTreeSet<PathBuf> = known.iter().filter(|p| !gone.contains(p)).cloned().collect();
        after.extend(added.iter().cloned());
        let mut projects = self.projects.clone();
        for path in &gone {
            projects.remove(path);
        }
        for path in &listed {
            if let Some(key) = project_of(path) {
                projects.insert(path.clone(), key);
            }
        }
        let data = |worktree: &PathBuf| json!({ "worktree": worktree.display().to_string(), "worktrees": paths_of(&after), "projects": projects_of(&projects) });
        for path in &added {
            out.push(Event::new(kinds::RECONCILED_WORKTREE_ADDED, Source::Reconcile, format!("worktree {}", path.display())).data(data(path)).project(project_of(path).as_ref()));
        }
        for path in &gone {
            out.push(Event::new(kinds::RECONCILED_WORKTREE_GONE, Source::Reconcile, format!("worktree {} is gone", path.display())).data(data(path)).project(self.projects.get(path)));
        }
        Some((after, projects))
    }
}

fn paths_of(set: &BTreeSet<PathBuf>) -> Vec<String> {
    set.iter().map(|p| p.display().to_string()).collect()
}

fn projects_of(projects: &BTreeMap<PathBuf, ProjectKey>) -> serde_json::Map<String, serde_json::Value> {
    projects.iter().map(|(path, key)| (path.display().to_string(), json!(key.as_str()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, running: bool, activity: Option<AgentActivity>) -> SessionNow {
        SessionNow { id: id.into(), project: None, running, activity, pr: None, starting: false }
    }

    /// One project (no key yet), its list read.
    fn listed(paths: &[&str]) -> Option<Vec<ProjectWorktrees>> {
        Some(vec![ProjectWorktrees { project: None, name: "app".into(), list: Ok(paths.iter().map(PathBuf::from).collect()) }])
    }

    fn kinds_of(events: &[Event]) -> Vec<&str> {
        events.iter().map(|e| e.kind.as_str()).collect()
    }

    #[test]
    fn reconciler_reports_missed_stop() {
        // The journal saw the turn start, and nothing after it (the Stop hook was lost).
        let started = crate::journal::activity("s1", None, Some(AgentActivity::Idle), AgentActivity::Working);
        let mut reconciler = Reconciler::from_events([&started]);
        let now = Snapshot { sessions: vec![session("s1", true, Some(AgentActivity::Idle))], worktrees: Some(Vec::new()) };
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
        let now = Snapshot { sessions: vec![session("s1", false, None), session("s2", true, Some(AgentActivity::Idle)), session("s3", true, None)], worktrees: Some(Vec::new()) };
        let events = reconciler.run(&now);
        assert_eq!(kinds_of(&events), ["reconciled.session_gone", "reconciled.session_running", "reconciled.activity", "reconciled.baseline", "reconciled.baseline"]);
        // s3 was never heard of: a baseline, not a gap.
        assert_eq!(events[4].session.as_deref(), Some("s3"));
        assert!(reconciler.run(&now).is_empty());
    }

    /// A session the app is starting (restored at a start) is not "running with no start" before
    /// its SessionStart hook comes; once the start window is over, the gap is reported.
    #[test]
    fn a_session_the_app_is_starting_waits_for_its_start_hook() {
        let quit = Event::new(kinds::APP_QUIT, Source::App, "Glasscode quit").data(json!({ "running": ["s1"] }));
        let starting = Snapshot { sessions: vec![SessionNow { starting: true, ..session("s1", true, None) }], ..Snapshot::default() };
        let mut reconciler = Reconciler::from_events([&quit]);
        assert!(reconciler.run(&starting).is_empty(), "restored at the start: its hook is on its way");
        // Its start hook came: nothing to say, then or later.
        reconciler.observe(&Event::new(kinds::SESSION_STARTED, Source::Hook, "started").session(Some("s1")));
        let started = Snapshot { sessions: vec![session("s1", true, None)], ..Snapshot::default() };
        assert!(reconciler.run(&started).is_empty());
        // No hook in the whole window (hooks off): the gap is reported once the window is over.
        let mut silent = Reconciler::from_events([&quit]);
        assert!(silent.run(&starting).is_empty());
        assert_eq!(kinds_of(&silent.run(&started)), ["reconciled.session_running"]);
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
            worktrees: listed(&["/w/a"]),
        };
        assert_eq!(kinds_of(&reconciler.run(&now)), ["reconciled.baseline"]);
        // Merged while nobody looked, and a worktree came and one went.
        now.sessions[0].pr = Some(("acme/app".into(), 7, "MERGED".into()));
        now.worktrees = listed(&["/w/b"]);
        let events = reconciler.run(&now);
        assert_eq!(kinds_of(&events), ["reconciled.pr", "reconciled.worktree_added", "reconciled.worktree_gone"]);
        assert!(reconciler.run(&now).is_empty());
        // After a restart, the journal's own record of them is a baseline as good.
        let mut again = Reconciler::from_events(events.iter().chain([&opened, &active]).collect::<Vec<_>>());
        again.worktrees = Some(["/w/b".into()].into_iter().collect());
        let replay = Reconciler::from_events(&events);
        assert_eq!(replay.worktrees, again.worktrees);
    }

    /// Two projects, one of them git cannot list (its folder unmounted): only that one's worktrees
    /// wait, and it is said once which project and why; the other's are compared as usual. Once
    /// it reads again, so is it.
    #[test]
    fn one_unreadable_project_waits_alone() {
        let (app, shop) = (ProjectKey::parse("app-3f9a1c").unwrap(), ProjectKey::parse("shop-3f9a1c").unwrap());
        let project = |key: &ProjectKey, name: &str, list: Result<Vec<&str>, Option<&str>>| ProjectWorktrees { project: Some(key.clone()), name: name.into(), list: list.map(|l| l.iter().map(PathBuf::from).collect()).map_err(|why| why.map(str::to_string)) };
        let both = |a: Result<Vec<&str>, Option<&str>>, s: Result<Vec<&str>, Option<&str>>| Snapshot { sessions: Vec::new(), worktrees: Some(vec![project(&app, "app", a), project(&shop, "shop", s)]) };
        let mut reconciler = Reconciler::default();
        assert_eq!(kinds_of(&reconciler.run(&both(Ok(vec!["/app", "/wt/app/a"]), Ok(vec!["/shop", "/wt/shop/s"])))), ["reconciled.baseline"]);
        // shop cannot be listed; app lost a worktree and gained one.
        let down = both(Ok(vec!["/app", "/wt/app/b"]), Err(Some("not a git repository")));
        let events = reconciler.run(&down);
        assert_eq!(kinds_of(&events), ["source.degraded", "reconciled.worktree_added", "reconciled.worktree_gone"]);
        assert!(events[0].summary.contains("shop") && events[0].summary.contains("not a git repository"), "{}", events[0].summary);
        assert_eq!(events[0].project.as_ref(), Some(&shop));
        assert_eq!((events[1].data["worktree"].as_str(), events[2].data["worktree"].as_str()), (Some("/wt/app/b"), Some("/wt/app/a")));
        // Still down: said once; shop's worktrees are not gone.
        assert!(reconciler.run(&down).is_empty());
        // After a restart the journal still says whose each worktree is.
        let mut again = Reconciler::from_events(&events);
        assert!(again.run(&down).iter().all(|e| e.kind == kinds::SOURCE_DEGRADED), "nothing of shop's worktrees");
        // shop reads again, one of its worktrees gone meanwhile.
        let up = both(Ok(vec!["/app", "/wt/app/b"]), Ok(vec!["/shop"]));
        let events = reconciler.run(&up);
        assert_eq!(kinds_of(&events), ["source.recovered", "reconciled.worktree_gone"]);
        assert_eq!(events[1].data["worktree"].as_str(), Some("/wt/shop/s"));
        // Not read yet (just after a start): nothing, and nothing said.
        assert!(reconciler.run(&both(Ok(vec!["/app", "/wt/app/b"]), Err(None))).is_empty());
    }

    /// Just after a start the worktree lists are not read yet (and git may fail to list one): an
    /// unread list is not an empty one. Nothing is said of the worktrees until they are read,
    /// so no live worktree is "gone" and memory archives none of their work (M24).
    #[test]
    fn worktrees_not_read_yet_are_not_gone() {
        let baseline = Event::new(kinds::RECONCILED_BASELINE, Source::Reconcile, "2 worktrees").data(json!({ "worktrees": ["/w/a", "/w/b"] }));
        let mut reconciler = Reconciler::from_events([&baseline]);
        let unread = Snapshot { sessions: Vec::new(), worktrees: None };
        assert!(reconciler.run(&unread).is_empty(), "nothing of the worktrees before their lists are read");
        // Read: only what really went.
        let read = Snapshot { sessions: Vec::new(), worktrees: listed(&["/w/a"]) };
        let events = reconciler.run(&read);
        assert_eq!(kinds_of(&events), ["reconciled.worktree_gone"]);
        assert_eq!(events[0].data["worktree"].as_str(), Some("/w/b"));
        // Unread again (git failed this time): still nothing, and what was known stays.
        assert!(reconciler.run(&unread).is_empty());
        assert!(reconciler.run(&read).is_empty());
        // A first start with nothing known yet makes its baseline only once the lists are read.
        let mut fresh = Reconciler::default();
        assert!(fresh.run(&unread).is_empty());
        assert_eq!(kinds_of(&fresh.run(&read)), ["reconciled.baseline"]);
    }
}
