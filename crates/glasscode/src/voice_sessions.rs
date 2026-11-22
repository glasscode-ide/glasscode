//! The sessions as the voice and `session list` tell them: what the sidebar holds. A session is
//! open while it has a row there, whether or not its agent runs ("not running" is one not started
//! since Glasscode opened; it starts when opened). Closing a session takes it out of the state
//! (`close_session`) and leaves its worktree on disk: such a worktree shows under "N more
//! worktrees without a session", is closed, and is never counted or offered as a session.
//!
//! Which one the person means comes from their words (`session_find`): its title, project,
//! branch, pull request, ticket and what it is about.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use glasscode_core::git::Worktree;
use glasscode_core::model::{Project, Session};
use glasscode_core::session_find::{self, Entry, Ranked};

use crate::center::Center;
use crate::shell::View;
use crate::sidebar::{self, RowState, Tree, TreeProject, TreeRow, TreeSession};
use crate::workspace::Workspace;

/// How many sessions a spoken list names; the rest are counted.
const SPOKEN_MAX: usize = 30;
/// How many sessions a search names when several fit alike.
const FITS_MAX: usize = 5;

/// A worktree without a session: closed, not a session.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Leftover {
    pub project_id: String,
    pub path: PathBuf,
    /// Its branch, else its folder (as the sidebar names it).
    pub label: String,
}

/// What the sidebar holds with nothing folded: the ids of the open sessions in its order (pinned
/// worktrees first, then the newest), and the worktrees without a session. It runs the sidebar's
/// own `tree`, so the two never disagree.
pub(crate) fn places(projects: &[&Project], worktrees: &HashMap<String, Vec<Worktree>>, sessions: &[&Session], pinned: &[PathBuf], removing: &HashSet<PathBuf>) -> (Vec<String>, Vec<Leftover>) {
    let open: Vec<Project> = projects.iter().map(|p| Project { expanded: true, ..(*p).clone() }).collect();
    let tree_projects: Vec<TreeProject> = open.iter().map(|project| TreeProject { project, worktrees: worktrees.get(&project.id).map(Vec::as_slice) }).collect();
    let tree_sessions: Vec<TreeSession> = sessions.iter().map(|session| TreeSession { session, state: RowState::Idle, running: false, pr: None }).collect();
    let idle_open: HashSet<String> = open.iter().map(|p| p.id.clone()).collect();
    let last = HashMap::new();
    let tree = Tree { projects: &tree_projects, sessions: &tree_sessions, pinned, folded: &[], removing, idle_open: &idle_open, selected: None, last: &last };
    let (mut ids, mut leftovers) = (Vec::new(), Vec::new());
    for row in sidebar::tree(&tree, |s| s.title.clone()) {
        match row {
            TreeRow::Worktree { project_id, path, .. } => ids.extend(sessions.iter().filter(|s| s.project_id == project_id && s.worktree == path).map(|s| s.id.clone())),
            TreeRow::IdleWorktree { project_id, path, label, .. } => leftovers.push(Leftover { project_id, path, label }),
            _ => {}
        }
    }
    (ids, leftovers)
}

/// A session's state as the voice says it: "not running" is open, only not started yet.
pub(crate) fn spoken_state(state: &str) -> &str {
    match state {
        "not running" => "not started since Glasscode opened (it starts when opened)",
        other => other,
    }
}

/// A session as a line the voice reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Line {
    pub id: String,
    pub title: String,
    pub project: String,
    pub branch: String,
    /// `session_state`'s word.
    pub state: String,
    pub pr: Option<u64>,
    /// It is on the Mac's screen now.
    pub shown: bool,
}

impl Line {
    fn text(&self) -> String {
        let pr = self.pr.map(|n| format!(", pull request #{n}")).unwrap_or_default();
        let shown = if self.shown { " (on the Mac's screen now)" } else { "" };
        format!("id {}: \"{}\" in {}, branch {}, {}{pr}{shown}", self.id, self.title, self.project, self.branch, spoken_state(&self.state))
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The voice's list of open sessions: a count line for each project (or the one asked for), then
/// the sessions, and what is not a session. `projects` are (name, open sessions, worktrees
/// without a session), in the sidebar's order.
pub(crate) fn list_text(lines: &[Line], projects: &[(String, usize, usize)], one_project: bool) -> String {
    let mut out = Vec::new();
    let leftovers: usize = projects.iter().map(|(_, _, l)| l).sum();
    if one_project {
        let (name, open, _) = &projects[0];
        out.push(format!("Count: {name} has {} (each is a row in the sidebar).", plural(*open, "open session", "open sessions")));
    } else {
        let each: Vec<String> = projects.iter().filter(|(_, open, _)| *open > 0).map(|(name, open, _)| format!("{name} {open}")).collect();
        let total: usize = projects.iter().map(|(_, open, _)| open).sum();
        out.push(format!("Count: {} in all (each is a row in the sidebar): {}.", plural(total, "open session", "open sessions"), if each.is_empty() { "none".into() } else { each.join(", ") }));
    }
    if leftovers > 0 {
        out.push(format!(
            "Not sessions: {} {} no session. {} closed: never count, list or open {} as a session.",
            plural(leftovers, "worktree", "worktrees"),
            if leftovers == 1 { "has" } else { "have" },
            if leftovers == 1 { "It is" } else { "They are" },
            if leftovers == 1 { "it" } else { "them" }
        ));
    }
    out.extend(lines.iter().take(SPOKEN_MAX).map(Line::text));
    if lines.len() > SPOKEN_MAX {
        out.push(format!("…and {} more: name a project, or words of the one meant, to narrow it.", lines.len() - SPOKEN_MAX));
    }
    out.join("\n")
}

/// The voice's answer to a search among `lines` (as ranked), with the worktrees without a
/// session that fit (their labels and projects) when no session does.
pub(crate) fn search_text(said: &str, lines: &[Line], ranked: &Ranked, closed: &[(String, String)], within: Option<&str>) -> String {
    let place = within.map(|p| format!(" in {p}")).unwrap_or_default();
    if let Some(best) = ranked.clear() {
        let others = ranked.hits.len() - 1;
        let less = if others > 0 { format!(" {} fit less.", plural(others, "other open session", "other open sessions")) } else { String::new() };
        return format!("Best fit for \"{said}\" (fits: {}): {}.{less}", best.matched.join(", "), lines[best.index].text());
    }
    if !ranked.hits.is_empty() {
        let named: Vec<String> = ranked.hits.iter().take(FITS_MAX).map(|h| lines[h.index].text()).collect();
        let more = ranked.hits.len().saturating_sub(FITS_MAX);
        let more = if more > 0 { format!("\n…and {more} more that fit.") } else { String::new() };
        return format!("{} open sessions{place} fit \"{said}\" alike; ask which one is meant:\n{}{more}", ranked.hits.len(), named.join("\n"));
    }
    let mut out = format!("No open session{place} fits \"{said}\".");
    if !closed.is_empty() {
        let named: Vec<String> = closed.iter().take(3).map(|(label, project)| format!("{label} in {project}")).collect();
        out.push_str(&format!(
            " A worktree without a session fits it ({}): it is closed, so there is no session to open; a new session can be started there.",
            named.join(", ")
        ));
    }
    out
}

/// The `session list` op's last line: what its rows are, and what is not a session.
pub(crate) fn list_footer(shown: usize, open: usize, leftovers: usize, project: Option<&str>, matched: Option<&str>) -> String {
    let place = project.map(|p| format!(" in {p}")).unwrap_or_default();
    let first = match matched {
        Some(words) => format!("Best fit first for “{words}”: {shown} of the {} fit.", plural(open, &format!("open session{place}"), &format!("open sessions{place}"))),
        None => format!("{}: each is open in the sidebar (“not running”: not started since Glasscode opened; it starts when opened).", plural(open, &format!("session{place}"), &format!("sessions{place}"))),
    };
    if leftovers == 0 {
        return first;
    }
    format!(
        "{first} {} {} no session: closed, not sessions (`glasscode worktree list`).",
        plural(leftovers, "worktree", "worktrees"),
        if leftovers == 1 { "has" } else { "have" }
    )
}

impl Workspace {
    /// The open sessions in the sidebar's order, and the worktrees without a session (`places`).
    pub(crate) fn sidebar_places(&self) -> (Vec<&Session>, Vec<Leftover>) {
        let projects: Vec<&Project> = self.state.visible_projects().collect();
        let sessions: Vec<&Session> = self.state.visible_sessions().collect();
        let (ids, leftovers) = places(&projects, &self.worktrees, &sessions, &self.state.pinned, &self.removing);
        (ids.iter().filter_map(|id| self.state.session(id)).collect(), leftovers)
    }

    /// The session on the Mac's screen now (in the Sessions view).
    fn session_on_screen(&self) -> Option<&str> {
        self.state.active_session.as_deref().filter(|_| self.view == View::Sessions && self.center == Center::Session)
    }

    fn project_name(&self, id: &str) -> String {
        self.state.project(id).map(|p| p.name.clone()).unwrap_or_default()
    }

    /// A session as it is searched: what it is called, its project, branch, pull request, ticket,
    /// the task it started with and its last reply.
    pub(crate) fn session_entry(&self, s: &Session) -> Entry {
        let pr = self.prs.map.get(&s.id).and_then(|p| p.pr.as_ref());
        let mut names = vec![s.title.clone()];
        names.extend(pr.map(|p| p.title.clone()));
        names.extend(s.review.as_ref().map(|r| r.title.clone()));
        names.extend(self.auto_titles.get(&s.id).cloned());
        let about: Vec<&str> = [s.initial_prompt.as_deref(), self.round.replies.get(&s.id).map(String::as_str)].into_iter().flatten().collect();
        Entry {
            id: s.id.clone(),
            title: self.session_title(s),
            names,
            project: self.project_name(&s.project_id),
            branch: s.branch.clone(),
            numbers: pr.map(|p| p.number).into_iter().chain(s.review.as_ref().map(|r| r.number)).collect(),
            ticket: self.state.ticket_of.get(&s.id).cloned(),
            about: about.join(" "),
        }
    }

    fn voice_line(&self, s: &Session) -> Line {
        Line {
            id: s.id.clone(),
            title: self.session_title(s),
            project: self.project_name(&s.project_id),
            branch: s.branch.clone(),
            state: self.session_state(s).to_string(),
            pr: self.prs.map.get(&s.id).and_then(|p| p.pr.as_ref()).map(|p| p.number),
            shown: self.session_on_screen() == Some(s.id.as_str()),
        }
    }

    /// A project by what was said of its name ("glass code"): its id, or what to say instead.
    fn voice_project(&self, said: &str) -> Result<String, String> {
        let projects: Vec<&Project> = self.state.visible_projects().collect();
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        session_find::project(said, &names).map(|i| projects[i].id.clone()).ok_or_else(|| format!("There is no project \"{said}\". The projects: {}.", names.join(", ")))
    }

    /// The worktrees without a session whose branch or folder fits the words: (label, project).
    fn closed_fits(&self, said: &str, leftovers: &[Leftover]) -> Vec<(String, String)> {
        let entries: Vec<Entry> = leftovers
            .iter()
            .map(|l| Entry { id: l.path.to_string_lossy().into_owned(), branch: l.label.clone(), title: l.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), ..Entry::default() })
            .collect();
        session_find::rank(said, &entries).hits.iter().map(|h| (leftovers[h.index].label.clone(), self.project_name(&leftovers[h.index].project_id))).collect()
    }

    /// The talker's `list_sessions`: the open sessions (of a project, those that wait for the
    /// person, or the ones that fit words said), with counts, and what is not a session.
    pub(crate) fn voice_sessions(&self, project: &str, search: &str, needs: bool) -> String {
        let within = match project.trim() {
            "" => None,
            said => match self.voice_project(said) {
                Ok(id) => Some(id),
                Err(say) => return say,
            },
        };
        let (sessions, leftovers) = self.sidebar_places();
        let mine = |id: &str| within.as_deref().is_none_or(|p| p == id);
        let sessions: Vec<&Session> = sessions.into_iter().filter(|s| mine(&s.project_id)).filter(|s| !needs || self.session_state(s) == "needs you").collect();
        let leftovers: Vec<Leftover> = leftovers.into_iter().filter(|l| mine(&l.project_id)).collect();
        let place = within.as_deref().map(|id| self.project_name(id));
        let lines: Vec<Line> = sessions.iter().map(|s| self.voice_line(s)).collect();

        let search = search.trim();
        if !search.is_empty() {
            let entries: Vec<Entry> = sessions.iter().map(|s| self.session_entry(s)).collect();
            let ranked = session_find::rank(search, &entries);
            if ranked.words > 0 {
                let closed = if ranked.hits.is_empty() { self.closed_fits(search, &leftovers) } else { Vec::new() };
                return search_text(search, &lines, &ranked, &closed, place.as_deref());
            }
        }
        if needs {
            return if lines.is_empty() { "No session waits for the person.".into() } else { lines.iter().map(Line::text).collect::<Vec<_>>().join("\n") };
        }
        let counts: Vec<(String, usize, usize)> = self
            .state
            .visible_projects()
            .filter(|p| mine(&p.id))
            .map(|p| (p.name.clone(), sessions.iter().filter(|s| s.project_id == p.id).count(), leftovers.iter().filter(|l| l.project_id == p.id).count()))
            .collect();
        if counts.is_empty() {
            return "No projects.".into();
        }
        let mut text = list_text(&lines, &counts, within.is_some());
        if self.session_on_screen().is_none() {
            text.push_str(&format!("\nOn the Mac's screen now: {}.", crate::capabilities::Place::View(self.view).name()));
        }
        text
    }

    /// The session the talker named for `open_place`: its id, the start of it, or words of it
    /// (opened only when one fits clearly). Else what to say instead.
    pub(crate) fn voice_session_for(&self, said: &str) -> Result<String, String> {
        let said = said.trim();
        let (sessions, leftovers) = self.sidebar_places();
        if let Some(s) = sessions.iter().find(|s| s.id == said) {
            return Ok(s.id.clone());
        }
        let starts: Vec<&&Session> = sessions.iter().filter(|s| said.len() >= 4 && s.id.starts_with(said)).collect();
        if let [one] = starts.as_slice() {
            return Ok(one.id.clone());
        }
        if said.is_empty() {
            return Err("Say which session to open.".into());
        }
        let entries: Vec<Entry> = sessions.iter().map(|s| self.session_entry(s)).collect();
        let ranked = session_find::rank(said, &entries);
        if let Some(best) = ranked.clear() {
            return Ok(sessions[best.index].id.clone());
        }
        let lines: Vec<Line> = sessions.iter().map(|s| self.voice_line(s)).collect();
        let closed = if ranked.hits.is_empty() { self.closed_fits(said, &leftovers) } else { Vec::new() };
        Err(format!("Nothing opened. {}", search_text(said, &lines, &ranked, &closed, None)))
    }

    /// The `session list` op's last line for the sessions it lists (`list_footer`).
    pub(crate) fn session_list_footer(&self, shown: usize, project: Option<&str>, matched: Option<&str>) -> String {
        let (sessions, leftovers) = self.sidebar_places();
        let mine = |id: &str| project.is_none_or(|p| p == id);
        let open = sessions.iter().filter(|s| mine(&s.project_id)).count();
        let leftovers = leftovers.iter().filter(|l| mine(&l.project_id)).count();
        let name = project.map(|id| self.project_name(id));
        list_footer(shown, open, leftovers, name.as_deref(), matched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glasscode_core::model::AgentKind;

    fn project(id: &str, root: &str) -> Project {
        Project { id: id.into(), name: id.into(), root: PathBuf::from(root), expanded: true, github_account: None }
    }

    fn session(id: &str, project: &str, worktree: &str, created_at: u64) -> Session {
        Session {
            id: id.into(),
            project_id: project.into(),
            worktree: PathBuf::from(worktree),
            branch: format!("feat/{id}"),
            title: format!("title {id}"),
            agent: AgentKind::Claude,
            account_id: None,
            agent_session_id: None,
            transcript_path: None,
            lineage: Vec::new(),
            initial_prompt: None,
            team_task: None,
            review: None,
            model: None,
            mode: None,
            created_at,
        }
    }

    fn worktree(path: &str, branch: &str) -> Worktree {
        Worktree { path: PathBuf::from(path), branch: branch.into(), ..Default::default() }
    }

    #[test]
    fn open_sessions_and_leftovers_are_what_the_sidebar_shows() {
        // Folded and closed in the window: the voice still sees every project's rows.
        let mut app = project("app", "/code/app");
        app.expanded = false;
        let site = project("site", "/code/site");
        let sessions = [session("a", "app", "/code/app-wt/one", 10), session("b", "app", "/code/app-wt/two", 30), session("c", "app", "/code/app-wt/one", 20), session("d", "site", "/code/site", 5)];
        let refs: Vec<&Session> = sessions.iter().collect();
        let worktrees = HashMap::from([
            ("app".to_string(), vec![worktree("/code/app", "main"), worktree("/code/app-wt/one", "feat/one"), worktree("/code/app-wt/two", "feat/two"), worktree("/code/app-wt/old", "old"), worktree("/code/app-wt/gone", "gone")]),
            ("site".to_string(), vec![worktree("/code/site", "main")]),
        ]);
        let removing = HashSet::from([PathBuf::from("/code/app-wt/gone")]);
        let (ids, leftovers) = places(&[&app, &site], &worktrees, &refs, &[], &removing);
        // Newest worktree first, and both sessions of the worktree with two.
        assert_eq!(ids, ["b", "a", "c", "d"]);
        // The worktrees with no session are not sessions; the one being removed is neither.
        let labels: Vec<&str> = leftovers.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels, ["main", "old"]);
        assert!(leftovers.iter().all(|l| l.project_id == "app"));
        // A pinned worktree comes first, as in the sidebar.
        let (ids, _) = places(&[&app, &site], &worktrees, &refs, &[PathBuf::from("/code/app-wt/one")], &removing);
        assert_eq!(ids, ["a", "c", "b", "d"]);
    }

    fn line(id: &str, title: &str, state: &str) -> Line {
        Line { id: id.into(), title: title.into(), project: "glasscode".into(), branch: format!("glasscode/{id}"), state: state.into(), pr: None, shown: false }
    }

    #[test]
    fn the_list_counts_open_sessions_and_says_what_is_not_one() {
        let lines = vec![line("s1", "Why the finished state and orange icon lag", "working"), Line { shown: true, pr: Some(347), ..line("s2", "Voice talker", "not running") }];
        let text = list_text(&lines, &[("glasscode".into(), 2, 9)], true);
        let mut rows = text.lines();
        assert_eq!(rows.next(), Some("Count: glasscode has 2 open sessions (each is a row in the sidebar)."));
        assert_eq!(rows.next(), Some("Not sessions: 9 worktrees have no session. They are closed: never count, list or open them as a session."));
        assert_eq!(rows.next(), Some("id s1: \"Why the finished state and orange icon lag\" in glasscode, branch glasscode/s1, working"));
        assert_eq!(
            rows.next(),
            Some("id s2: \"Voice talker\" in glasscode, branch glasscode/s2, not started since Glasscode opened (it starts when opened), pull request #347 (on the Mac's screen now)")
        );
        assert_eq!(rows.next(), None);
        // Every project: a count for each, and the total.
        let text = list_text(&lines, &[("hub2.0".into(), 0, 0), ("glasscode".into(), 2, 0)], false);
        assert_eq!(text.lines().next(), Some("Count: 2 open sessions in all (each is a row in the sidebar): glasscode 2."));
        assert!(!text.contains("Not sessions"));
    }

    #[test]
    fn a_long_list_is_cut_and_says_how_many_more() {
        let lines: Vec<Line> = (0..SPOKEN_MAX + 4).map(|i| line(&format!("s{i}"), "t", "idle")).collect();
        let text = list_text(&lines, &[("glasscode".into(), lines.len(), 0)], true);
        assert!(text.ends_with("…and 4 more: name a project, or words of the one meant, to narrow it."), "{text}");
        assert_eq!(text.lines().count(), 1 + SPOKEN_MAX + 1);
    }

    #[test]
    fn a_search_names_the_best_fit_or_asks() {
        let lines = vec![line("s1", "Why the finished state and orange icon lag", "working"), line("s2", "Voice agent: find and open the right session", "idle"), line("s3", "Cheap voice pipeline", "idle")];
        let entries: Vec<Entry> = lines.iter().map(|l| Entry { id: l.id.clone(), title: l.title.clone(), project: l.project.clone(), branch: l.branch.clone(), ..Entry::default() }).collect();

        let ranked = session_find::rank("orange icon lag", &entries);
        let text = search_text("orange icon lag", &lines, &ranked, &[], None);
        assert!(text.starts_with("Best fit for \"orange icon lag\" (fits: orange, icon, lag): id s1:"), "{text}");

        let ranked = session_find::rank("the voice one", &entries);
        let text = search_text("the voice one", &lines, &ranked, &[], Some("glasscode"));
        assert!(text.starts_with("2 open sessions in glasscode fit \"the voice one\" alike; ask which one is meant:"), "{text}");
        assert!(text.contains("id s2:") && text.contains("id s3:"), "{text}");

        // Nothing open fits, a closed worktree does: it is said to be closed.
        let ranked = session_find::rank("codex full auto", &entries);
        let text = search_text("codex full auto", &lines, &ranked, &[("fix/codex-full-auto".into(), "glasscode".into())], None);
        assert_eq!(
            text,
            "No open session fits \"codex full auto\". A worktree without a session fits it (fix/codex-full-auto in glasscode): it is closed, so there is no session to open; a new session can be started there."
        );
    }

    #[test]
    fn the_op_footer_tells_open_from_closed() {
        assert_eq!(
            list_footer(7, 7, 9, Some("glasscode"), None),
            "7 sessions in glasscode: each is open in the sidebar (“not running”: not started since Glasscode opened; it starts when opened). 9 worktrees have no session: closed, not sessions (`glasscode worktree list`)."
        );
        assert_eq!(list_footer(1, 1, 0, None, None), "1 session: each is open in the sidebar (“not running”: not started since Glasscode opened; it starts when opened).");
        assert_eq!(list_footer(2, 7, 1, None, Some("voice")), "Best fit first for “voice”: 2 of the 7 open sessions fit. 1 worktree has no session: closed, not sessions (`glasscode worktree list`).");
    }

    #[test]
    fn not_running_is_said_as_open() {
        assert_eq!(spoken_state("not running"), "not started since Glasscode opened (it starts when opened)");
        assert_eq!(spoken_state("needs you"), "needs you");
    }
}
