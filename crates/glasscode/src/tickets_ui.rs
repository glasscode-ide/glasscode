//! Tickets: the person's Linear tickets and the ones they review, and the pull requests that
//! ask for their review, each with the sessions working on it (found by the ticket's ID in a
//! branch, one of its pull requests, its remote task, or because it was started from the
//! ticket). From a ticket: read it whole, comment, and start a session with the ticket as its
//! first message. This file has the state and what happens; `tickets_view.rs` draws it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{AnyElement, AppContext as _, ClickEvent, Context, Entity, FontWeight, Hsla, SharedString, Window, deferred, div, prelude::*};
use glasscode_core::git;
use glasscode_core::github::{self, ReviewRequest};
use glasscode_core::github_tickets;
use glasscode_core::linear::{self, Detail, Person, ProjectOption, Snapshot, Ticket};
use glasscode_core::model::{AgentKind, Provider, Session, new_id, now};

use crate::dialogs::Modal;
use crate::shell::View;
use crate::sidebar::RowState;
use crate::text_area::TextArea;
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

/// How often the board is fetched again while Glasscode runs.
const REFRESH_EVERY: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    Mine,
    Review,
    #[default]
    Both,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Board,
    List,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quick {
    #[default]
    All,
    NeedsYou,
    Working,
    NoSession,
    DueSoon,
}

/// A card: a ticket, or a pull request that asks for a review.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Ticket(String),
    Pr(String, u64),
}

pub enum Loaded<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// A session working on a card.
#[derive(Clone)]
pub struct Work {
    /// A session on this Mac, or (with `on_server`) on the server.
    pub id: String,
    pub on_server: bool,
    pub agent: AgentKind,
    pub title: String,
    pub branch: String,
    pub state: RowState,
    pub running: bool,
    /// How it was found ("its branch has ENG-787").
    pub why: String,
}

pub struct TicketsUi {
    pub snapshot: Option<Snapshot>,
    pub reviews: Vec<ReviewRequest>,
    pub reviews_error: Option<String>,
    /// A fetch has come back at least once (so Focus pruning may trust that an item is gone).
    pub fetched_once: bool,
    /// The board's provider is set up: a Linear key is saved, or GitHub has a repository.
    pub connected: bool,
    /// A Linear key is saved (read with each fetch, not in a render: it is in the key store).
    pub linear_key: bool,
    /// Where the board's last fetch came from (`snapshot` is GitHub's when it is GitHub).
    pub fetched_from: Provider,
    pub loading: bool,
    pub error: Option<String>,
    pub last_fetch: Option<Instant>,
    pub scope: Scope,
    pub layout: Layout,
    pub quick: Quick,
    pub query: Entity<TextInput>,
    pub selected: Option<Key>,
    pub expanded: bool,
    /// The keyboard is in the reader while it shows: Esc closes it.
    pub reader_focus: gpui::FocusHandle,
    /// The reader opened (true) or closed (false) since the last draw: the draw, which has the
    /// window, moves the keyboard into it or back to the window.
    pub reader_focus_due: Option<bool>,
    /// The reader's edge is being dragged: where the drag began, and the width it had (at 100 %).
    pub reader_drag: Option<(f32, f32)>,
    /// Done shows as the other columns do (and its part of the list is open) until the person
    /// folds it. It starts open: folded, it is a narrow strip at the board's end that reads as
    /// a column that did not get its width.
    pub done_open: bool,
    /// Where the board is scrolled sideways.
    pub board_scroll: gpui::ScrollHandle,
    /// Each shown column's card list, by the column's name: a wheel over a column that has
    /// nothing to scroll moves the board sideways (`tickets_view.rs`).
    pub column_scrolls: std::cell::RefCell<HashMap<String, gpui::ScrollHandle>>,
    pub details: HashMap<String, Loaded<Detail>>,
    pub comment: Entity<TextArea>,
    pub posting: bool,
    /// For Settings › Integrations › Linear.
    pub people: Option<Vec<Person>>,
    pub labels: Option<Vec<String>>,
    /// The project selector's options for the active provider (Linear teams, or GitHub repos).
    pub project_options: Option<Vec<ProjectOption>>,
    pub picker: Option<&'static str>,
    pub key_input: Entity<TextInput>,
    pub connecting: bool,
    pub connect_error: Option<String>,
    /// Tickets and review requests already seen (notifications are for new ones); the first
    /// fetch after Glasscode starts only learns what there is.
    seen: Option<HashSet<String>>,
    /// The GitHub repository of each project's folder, as the last scan read it (none until it
    /// is back). `github::repo_slug` runs git twice for each project, about 30 ms each, so it
    /// runs in the background (`tickets_repos_tick`), never on the main thread: the tick and the
    /// Board ask for the provider all the time.
    repos: Option<Vec<(PathBuf, Option<String>)>>,
    repos_at: Option<Instant>,
    repos_loading: bool,
    /// Debug builds: `GLASSCODE_DEBUG_OPEN=board…` was read; with `sample` the board is made up
    /// and nothing is fetched (`sample_repo`: its GitHub repository, when it is a GitHub one).
    debug_done: bool,
    sample: bool,
    sample_repo: Option<String>,
    /// Debug builds (`board:…:esc`): the next draw of the Board presses Esc.
    pub debug_escape: bool,
    /// The Pull requests page's own state; the page lists `reviews` with the person's own
    /// pull requests.
    pub page: crate::prs_view::PrsPage,
}

/// Whether the projects' repositories are read again: never read, the projects changed (the
/// scan was for other folders), or the last scan is older than `REFRESH_EVERY`.
fn repos_due(roots: &[PathBuf], scanned: Option<&[(PathBuf, Option<String>)]>, age: Option<Duration>) -> bool {
    let Some(scanned) = scanned else { return true };
    let same_folders = roots.len() == scanned.len() && roots.iter().zip(scanned).all(|(root, (folder, _))| root == folder);
    !same_folders || age.is_none_or(|age| age >= REFRESH_EVERY)
}

/// The repositories of the projects (`roots`, in project order) that the scan found.
fn repos_of(roots: &[PathBuf], scanned: &[(PathBuf, Option<String>)]) -> Vec<String> {
    roots.iter().filter_map(|root| repo_of(root, scanned)).collect()
}

/// The repository the scan found for one project folder; none before the scan reads it.
fn repo_of(root: &std::path::Path, scanned: &[(PathBuf, Option<String>)]) -> Option<String> {
    scanned.iter().find(|(folder, _)| folder == root)?.1.clone()
}

impl TicketsUi {
    pub fn new(cx: &mut Context<Workspace>) -> Self {
        let query = cx.new(|cx| TextInput::new("Filter cards", cx));
        cx.subscribe(&query, |_: &mut Workspace, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        let key_input = cx.new(|cx| TextInput::new("lin_api_…", cx).mono());
        cx.subscribe(&key_input, |this: &mut Workspace, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Submit) {
                this.linear_connect(cx);
            }
        })
        .detach();
        let comment = cx.new(|cx| TextArea::new("Leave a comment…", 3, cx));
        // Esc in the comment box is the box's own key (`text_area::Cancel`): it closes the reader
        // as Esc anywhere else in the reader does.
        cx.subscribe(&comment, |this: &mut Workspace, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Cancel) {
                this.close_reader(cx);
            }
        })
        .detach();
        let snapshot = linear::load_cache();
        Self {
            snapshot,
            reviews: Vec::new(),
            reviews_error: None,
            fetched_once: false,
            connected: linear::key().is_some(),
            linear_key: linear::key().is_some(),
            fetched_from: Provider::Linear,
            loading: false,
            error: None,
            last_fetch: None,
            scope: Scope::default(),
            layout: Layout::default(),
            quick: Quick::default(),
            query,
            selected: None,
            expanded: false,
            reader_focus: cx.focus_handle(),
            reader_focus_due: None,
            reader_drag: None,
            done_open: true,
            board_scroll: gpui::ScrollHandle::new(),
            column_scrolls: Default::default(),
            details: HashMap::new(),
            comment,
            posting: false,
            people: None,
            labels: None,
            project_options: None,
            picker: None,
            key_input,
            connecting: false,
            connect_error: None,
            seen: None,
            repos: None,
            repos_at: None,
            repos_loading: false,
            debug_done: false,
            sample: false,
            sample_repo: None,
            debug_escape: false,
            page: Default::default(),
        }
    }

    pub fn ticket(&self, identifier: &str) -> Option<&Ticket> {
        self.snapshot.as_ref()?.ticket(identifier)
    }

    pub fn review(&self, repo: &str, number: u64) -> Option<&ReviewRequest> {
        self.reviews.iter().find(|r| r.repo.eq_ignore_ascii_case(repo) && r.hit.number == number)
    }

    /// The tickets assigned to the person, and the ones they review (not their own).
    pub fn mine(&self) -> Vec<&Ticket> {
        self.snapshot.as_ref().map(|s| s.mine.iter().collect()).unwrap_or_default()
    }

    pub fn reviewing(&self) -> Vec<&Ticket> {
        let Some(s) = self.snapshot.as_ref() else { return Vec::new() };
        s.reviewing.iter().filter(|t| t.assignee.as_ref().is_none_or(|a| a.id != s.me)).collect()
    }

    /// The person's reviewer label, as last fetched.
    pub fn label(&self) -> Option<&str> {
        self.snapshot.as_ref()?.label.as_deref()
    }
}

impl Workspace {
    /// The GitHub repository of a project's folder, as the last background scan read it (none
    /// before it has run): what the `glasscode` command's `ops::targets` name, never a git call.
    pub(crate) fn tickets_repo_of(&self, root: &std::path::Path) -> Option<String> {
        repo_of(root, self.tickets.repos.as_deref().unwrap_or_default())
    }

    // ----- which provider, which project -------------------------------------------------

    /// GitHub repository slugs of the git projects, in project order, from the last scan.
    fn project_repos(&self) -> Vec<String> {
        let roots: Vec<PathBuf> = self.state.projects.iter().map(|p| p.root.clone()).collect();
        self.tickets.repos.as_deref().map(|scanned| repos_of(&roots, scanned)).unwrap_or_default()
    }

    /// Read the projects' repositories in the background: at the start, when a project comes or
    /// goes, and every `REFRESH_EVERY`. A change fetches the board again, as the provider or its
    /// repository may differ now.
    fn tickets_repos_tick(&mut self, cx: &mut Context<Self>) {
        let roots: Vec<PathBuf> = self.state.projects.iter().map(|p| p.root.clone()).collect();
        if self.tickets.repos_loading || !repos_due(&roots, self.tickets.repos.as_deref(), self.tickets.repos_at.map(|at| at.elapsed())) {
            return;
        }
        self.tickets.repos_loading = true;
        self.tickets.repos_at = Some(Instant::now());
        let task = cx.background_spawn(async move {
            roots.into_iter().map(|root| {
                let repo = root.join(".git").exists().then(|| github::repo_slug(&root)).flatten();
                (root, repo)
            }).collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let repos = task.await;
            let _ = this.update(cx, |this, cx| {
                this.tickets.repos_loading = false;
                if this.tickets.repos.as_ref() != Some(&repos) {
                    this.tickets.repos = Some(repos);
                    this.tickets.last_fetch = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The GitHub repository the board shows: the chosen one (if still a known project), else the
    /// first git project's repository.
    pub(crate) fn tickets_github_repo(&self) -> Option<String> {
        if let Some(sample) = self.tickets.sample_repo.as_ref() {
            return Some(sample.clone());
        }
        if let Some(chosen) = self.state.tickets.github_repo.as_ref() {
            return Some(chosen.clone());
        }
        self.project_repos().into_iter().next()
    }

    /// The active ticket provider: a manual override for the selected GitHub repo wins; with no
    /// Linear key, GitHub is the only provider that can work; otherwise the remembered one.
    pub(crate) fn tickets_provider(&self) -> Provider {
        if let Some(repo) = self.tickets_github_repo() {
            if let Some(p) = self.state.tickets.provider_by_repo.get(&repo) {
                return *p;
            }
        }
        if linear::key().is_none() && self.tickets_github_repo().is_some() {
            return Provider::GitHub;
        }
        self.state.tickets.last_provider
    }

    /// Whether the active provider is set up enough to show a board.
    fn tickets_connected(&self) -> bool {
        match self.tickets_provider() {
            Provider::Linear => linear::key().is_some(),
            Provider::GitHub => self.tickets_github_repo().is_some(),
        }
    }

    pub(crate) fn tickets_pick_provider(&mut self, provider: Provider, cx: &mut Context<Self>) {
        self.state.tickets.last_provider = provider;
        if let Some(repo) = self.tickets_github_repo() {
            self.state.tickets.provider_by_repo.insert(repo, provider);
        }
        self.tickets.picker = None;
        self.tickets.project_options = None;
        self.tickets_reload(cx);
        self.tickets_load_projects(cx);
        self.save();
    }

    pub(crate) fn tickets_pick_project(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        match self.tickets_provider() {
            Provider::Linear => self.state.tickets.linear_team = id,
            Provider::GitHub => {
                self.state.tickets.github_repo = id;
                // A manual repo pick also pins this repo to the GitHub provider.
                if let Some(repo) = self.tickets_github_repo() {
                    self.state.tickets.provider_by_repo.insert(repo, Provider::GitHub);
                }
            }
        }
        self.tickets.picker = None;
        self.tickets_reload(cx);
        self.save();
    }

    /// Reload the board for the current provider and project (clears what the last one showed).
    pub(crate) fn tickets_reload(&mut self, cx: &mut Context<Self>) {
        self.tickets.snapshot = None;
        self.tickets.details.clear();
        self.tickets.selected = None;
        self.tickets.error = None;
        self.tickets.linear_key = linear::key().is_some();
        self.tickets.connected = self.tickets_connected();
        self.tickets.last_fetch = None;
        self.tickets_refresh(cx);
        cx.notify();
    }

    /// Fetch the project selector's options for the active provider (Linear teams, or GitHub
    /// repositories), in the background.
    pub(crate) fn tickets_load_projects(&mut self, cx: &mut Context<Self>) {
        if self.tickets.project_options.is_some() {
            return;
        }
        match self.tickets_provider() {
            Provider::Linear => {
                if linear::key().is_none() {
                    return;
                }
                let task = cx.background_spawn(async move { linear::Client::saved().map(|c| c.teams().unwrap_or_default()) });
                cx.spawn(async move |this, cx| {
                    let teams = task.await;
                    let _ = this.update(cx, |this, cx| {
                        this.tickets.project_options = Some(teams.unwrap_or_default());
                        cx.notify();
                    });
                })
                .detach();
            }
            Provider::GitHub => {
                // Start with the known project repositories (offline), then add gh's repo list.
                let mut options: Vec<ProjectOption> = self.project_repos().into_iter().map(|slug| ProjectOption { label: slug.clone(), detail: "project".into(), id: slug }).collect();
                let env = self.env.clone();
                let task = cx.background_spawn(async move { github_tickets::repos(&env, 100).unwrap_or_default() });
                cx.spawn(async move |this, cx| {
                    let more = task.await;
                    let _ = this.update(cx, |this, cx| {
                        for opt in more {
                            if !options.iter().any(|o| o.id.eq_ignore_ascii_case(&opt.id)) {
                                options.push(opt);
                            }
                        }
                        this.tickets.project_options = Some(options);
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    // ----- fetching ----------------------------------------------------------------------

    pub fn tickets_tick(&mut self, cx: &mut Context<Self>) {
        self.tickets_repos_tick(cx);
        self.prs_tick(cx);
        if cfg!(debug_assertions) && !self.tickets.debug_done {
            self.tickets.debug_done = true;
            if let Some(mode) = std::env::var("GLASSCODE_DEBUG_OPEN").ok().filter(|m| m == "board" || m.starts_with("board:")) {
                self.board_debug_open(&mode, cx);
            }
        }
        // The provider depends on the projects' repositories: the first fetch waits for them.
        let due = self.tickets.repos.is_some() && self.tickets.last_fetch.is_none_or(|t| t.elapsed() >= REFRESH_EVERY);
        if due && !self.tickets.loading && (self.tickets_connected() || !self.state.linear.review_prs_off) {
            self.tickets_refresh(cx);
        }
    }

    /// Fetch the board from Linear and the review requests from GitHub, in the background.
    pub fn tickets_refresh(&mut self, cx: &mut Context<Self>) {
        // Not before the projects' repositories are read (the tick fetches once they are): the
        // provider depends on them, and a first fetch with the wrong one would make the tickets
        // of the second look new (only the first fetch learns what there is, `seen`).
        if self.tickets.loading || self.tickets.repos.is_none() || self.tickets.sample {
            return;
        }
        self.tickets.loading = true;
        self.tickets.last_fetch = Some(Instant::now());
        self.tickets.linear_key = linear::key().is_some();
        self.tickets.connected = self.tickets_connected();
        let settings = self.state.linear.clone();
        let env = self.env.clone();
        let provider = self.tickets_provider();
        self.tickets.fetched_from = provider;
        let team = self.state.tickets.linear_team.clone();
        let github_repo = self.tickets_github_repo();
        let task = cx.background_spawn(async move {
            let snapshot = match provider {
                Provider::Linear if linear::key().is_some() => linear::Client::saved().map(|c| c.snapshot(&settings, now(), team.as_deref())),
                Provider::GitHub => github_repo.map(|repo| github_tickets::snapshot(&repo, &env, now())),
                Provider::Linear => None,
            };
            let reviews = (!settings.review_prs_off).then(|| github::review_requested(&env));
            (snapshot, reviews)
        });
        cx.spawn(async move |this, cx| {
            let (snapshot, reviews) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.tickets.loading = false;
                this.tickets.fetched_once = true;
                let mut fresh: Vec<(Key, String, String)> = Vec::new();
                match snapshot {
                    Some(Ok(snapshot)) => {
                        linear::save_cache(&snapshot);
                        for t in &snapshot.mine {
                            if !t.is_done() {
                                fresh.push((Key::Ticket(t.identifier.clone()), format!("{} is assigned to you", t.identifier), t.title.clone()));
                            }
                        }
                        this.tickets.snapshot = Some(snapshot);
                        this.tickets.error = None;
                    }
                    Some(Err(err)) => this.tickets.error = Some(format!("{err:#}")),
                    None => {}
                }
                match reviews {
                    Some(Ok(list)) => {
                        for r in &list {
                            fresh.push((Key::Pr(r.repo.clone(), r.hit.number), format!("#{} asks for your review", r.hit.number), linear::short_title(&crate::tickets_view::tidy_pr_title(&r.hit.title), 80)));
                        }
                        this.tickets.reviews = list;
                        this.tickets.reviews_error = None;
                    }
                    Some(Err(err)) => this.tickets.reviews_error = Some(format!("{err:#}")),
                    None => this.tickets.reviews.clear(),
                }
                this.tickets_notify_new(fresh, cx);
                // A ticket opened before the board was in gets its comments now.
                if let Some(Key::Ticket(id)) = this.tickets.selected.clone() {
                    this.tickets_load_detail(&id, false, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A notification for each ticket or review request that was not there before (the first
    /// fetch only learns what there is).
    fn tickets_notify_new(&mut self, items: Vec<(Key, String, String)>, cx: &mut Context<Self>) {
        let ids: HashSet<String> = items.iter().map(|(k, _, _)| key_id(k)).collect();
        let Some(seen) = self.tickets.seen.as_mut() else {
            self.tickets.seen = Some(ids);
            return;
        };
        let new: Vec<(Key, String, String)> = items.into_iter().filter(|(k, _, _)| !seen.contains(&key_id(k))).collect();
        seen.extend(ids);
        if self.state.linear.notify_off {
            return;
        }
        for (key, title, body) in new.into_iter().take(3) {
            self.notify_ticket(key, title, body, cx);
        }
    }

    /// A ticket's comments and its team's statuses (when it is opened).
    pub fn tickets_load_detail(&mut self, identifier: &str, force: bool, cx: &mut Context<Self>) {
        if (!force && self.tickets.details.contains_key(identifier)) || self.tickets.sample {
            return;
        }
        let Some(ticket) = self.tickets.ticket(identifier).cloned() else { return };
        self.tickets.details.insert(identifier.to_string(), Loaded::Loading);
        let env = self.env.clone();
        let task = cx.background_spawn(async move {
            match ticket.provider {
                Provider::Linear => linear::Client::saved().map(|c| c.detail(&ticket.id)).unwrap_or_else(|| Err(anyhow::anyhow!("Linear is not connected"))),
                Provider::GitHub => {
                    let number = ticket.id.parse::<u64>().map_err(|_| anyhow::anyhow!("not a GitHub issue number"))?;
                    github_tickets::detail(&ticket.team, number, &env)
                }
            }
        });
        let identifier = identifier.to_string();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.tickets.details.insert(identifier, match result {
                    Ok(detail) => Loaded::Ready(detail),
                    Err(err) => Loaded::Failed(format!("{err:#}")),
                });
                cx.notify();
            });
        })
        .detach();
    }

    // ----- what works on what ------------------------------------------------------------------

    pub(crate) fn work_on_ticket(&self, ticket: &Ticket) -> Vec<Work> {
        let prs = ticket.pull_requests();
        let task = ticket.team_task();
        let mut found: Vec<Work> = Vec::new();
        for s in &self.state.sessions {
            let pr = self.prs.map.get(&s.id).and_then(|p| Some((p.repo.clone()?, p.pr.as_ref()?.number)));
            let why = if self.state.ticket_of.get(&s.id).is_some_and(|id| id.eq_ignore_ascii_case(&ticket.identifier)) {
                Some("started from this ticket".to_string())
            } else if ticket_mentions(ticket, &s.branch) {
                Some(format!("its branch has {}", ticket.identifier))
            } else if let Some((_, n)) = pr.filter(|(repo, n)| prs.iter().any(|(r, m)| r.eq_ignore_ascii_case(repo) && m == n)) {
                Some(format!("its pull request #{n}"))
            } else if let Some(review) = s.review.as_ref().filter(|r| prs.iter().any(|(repo, n)| repo.eq_ignore_ascii_case(&r.repo) && *n == r.number)) {
                Some(format!("reviews its pull request #{}", review.number))
            } else if task.is_some() && s.team_task == task {
                Some(format!("its remote task {}", task.clone().unwrap_or_default()))
            } else {
                None
            };
            if let Some(why) = why {
                found.push(self.local_work(s, why));
            }
        }
        // Everyone else in those worktrees.
        let paths: HashSet<PathBuf> = found.iter().filter_map(|w| self.state.session(&w.id).map(|s| s.worktree.clone())).collect();
        for s in &self.state.sessions {
            if paths.contains(&s.worktree) && !found.iter().any(|w| w.id == s.id) {
                found.push(self.local_work(s, "same worktree".into()));
            }
        }
        if let Some(server) = self.server.state.as_ref() {
            for r in &server.sessions {
                if ticket_mentions(ticket, &r.session.branch) {
                    found.push(Work {
                        id: r.session.id.clone(),
                        on_server: true,
                        agent: r.session.agent,
                        title: r.session.title.clone(),
                        branch: r.session.branch.clone(),
                        state: match r.activity {
                            Some(glasscode_core::agents::AgentActivity::NeedsInput) => RowState::NeedsYou,
                            Some(glasscode_core::agents::AgentActivity::Working) => RowState::Working,
                            _ => RowState::Idle,
                        },
                        running: r.running,
                        why: format!("on the server · its branch has {}", ticket.identifier),
                    });
                }
            }
        }
        found
    }

    pub(crate) fn work_on_pr(&self, repo: &str, number: u64) -> Vec<Work> {
        self.state
            .sessions
            .iter()
            .filter(|s| s.review.as_ref().is_some_and(|r| r.number == number && r.repo.eq_ignore_ascii_case(repo)) || s.branch == format!("review/{number}"))
            .map(|s| self.local_work(s, "your review of it".into()))
            .collect()
    }

    fn local_work(&self, s: &Session, why: String) -> Work {
        Work {
            id: s.id.clone(),
            on_server: false,
            agent: s.agent,
            title: self.session_title(s),
            branch: s.branch.clone(),
            state: self.row_state(s),
            running: self.terminals.contains_key(&s.id),
            why,
        }
    }

    /// Worktrees with no session whose branch names the ticket.
    pub(crate) fn idle_worktrees_for(&self, ticket: &Ticket) -> Vec<(String, PathBuf, String)> {
        let mut out = Vec::new();
        for (project_id, list) in &self.worktrees {
            for w in list {
                if !w.is_main && ticket_mentions(ticket, &w.branch) && !self.state.sessions.iter().any(|s| s.worktree == w.path) {
                    out.push((project_id.clone(), w.path.clone(), w.branch.clone()));
                }
            }
        }
        out
    }

    /// The ticket a session works on, for its header.
    pub fn ticket_for_session(&self, session: &Session) -> Option<&Ticket> {
        if let Some(id) = self.state.ticket_of.get(&session.id) {
            if let Some(t) = self.tickets.ticket(id) {
                return Some(t);
            }
        }
        let snapshot = self.tickets.snapshot.as_ref()?;
        let all = || snapshot.mine.iter().chain(snapshot.reviewing.iter());
        if let Some(t) = all().find(|t| ticket_mentions(t, &session.branch)) {
            return Some(t);
        }
        let pr = self.prs.map.get(&session.id).and_then(|p| Some((p.repo.clone()?, p.pr.as_ref()?.number)));
        all().find(|t| {
            let prs = t.pull_requests();
            pr.as_ref().is_some_and(|(repo, n)| prs.iter().any(|(r, m)| r.eq_ignore_ascii_case(repo) && m == n))
                || session.review.as_ref().is_some_and(|r| prs.iter().any(|(repo, n)| repo.eq_ignore_ascii_case(&r.repo) && *n == r.number))
                || (session.team_task.is_some() && t.team_task() == session.team_task)
        })
    }

    // ----- going places ------------------------------------------------------------------

    pub fn show_tickets(&mut self, cx: &mut Context<Self>) {
        self.enter(View::Board);
        self.menu_open = false;
        self.tickets_load_projects(cx);
        if self.tickets.last_fetch.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
            self.tickets_refresh(cx);
        }
        if let Some(Key::Ticket(id)) = self.tickets.selected.clone() {
            self.tickets_load_detail(&id, false, cx);
        }
        cx.notify();
    }

    /// `GLASSCODE_DEBUG_OPEN=board[:sample][:github][:loading][:syncing][:error][:picker][:list][:expand][:esc][:light|dark|classic|contrast][:90|100|115|130][:<ENG-123 | acme/app#133>]`
    /// (debug builds). `sample` puts made-up tickets and review requests on the Board and fetches
    /// nothing (`github`: as issues of a GitHub repository); `loading` is the first fetch that is
    /// not back yet, `syncing` a later one, `error` one that failed, `picker` has the provider's
    /// list open. A key opens that card in the reader (`expand`: in the place of the board), and
    /// `esc` presses Esc three seconds later: the reader closes when it has the keyboard.
    fn board_debug_open(&mut self, mode: &str, cx: &mut Context<Self>) {
        use glasscode_core::model::ThemeChoice;
        let (mut choice, mut scale, mut key) = (None, None, None);
        let mut words: HashSet<&str> = HashSet::new();
        for word in mode.split(':').skip(1) {
            match word {
                "light" => choice = Some(ThemeChoice::Light),
                "dark" => choice = Some(ThemeChoice::Dark),
                "classic" => choice = Some(ThemeChoice::Classic),
                "contrast" => choice = Some(ThemeChoice::Contrast),
                "sample" | "github" | "loading" | "syncing" | "error" | "picker" | "list" | "expand" | "esc" => {
                    words.insert(word);
                }
                other => match (theme::scale_named(other), other.rsplit_once('#')) {
                    (Some(named), _) => scale = Some(named),
                    (None, Some((repo, n))) => key = n.parse().ok().map(|n| if repo.is_empty() { Key::Ticket(other.to_string()) } else { Key::Pr(repo.to_string(), n) }),
                    (None, None) => key = Some(Key::Ticket(other.to_string())),
                },
            }
        }
        if choice.is_some() || scale.is_some() {
            self.set_appearance(
                |a| {
                    a.theme = choice.unwrap_or(a.theme);
                    a.scale = scale.unwrap_or(a.scale);
                },
                cx,
            );
        }
        if words.contains("sample") {
            let provider = if words.contains("github") { Provider::GitHub } else { Provider::Linear };
            let (snapshot, reviews, details) = sample_board(provider, now());
            let option = |id: &str, label: &str, detail: &str| ProjectOption { id: id.into(), label: label.into(), detail: detail.into() };
            self.tickets.sample = true;
            self.tickets.sample_repo = (provider == Provider::GitHub).then(|| "acme/app".to_string());
            self.state.tickets.last_provider = provider;
            self.tickets.fetched_from = provider;
            self.tickets.connected = true;
            self.tickets.fetched_once = true;
            self.tickets.last_fetch = Some(Instant::now());
            self.tickets.project_options = Some(match provider {
                Provider::GitHub => vec![option("acme/app", "acme/app", "A made-up repository"), option("acme/docs", "acme/docs", "")],
                Provider::Linear => vec![option("ENG", "Engineering", "ENG"), option("DOC", "Documentation", "DOC")],
            });
            if words.contains("loading") {
                self.tickets.snapshot = None;
                self.tickets.reviews.clear();
                self.tickets.loading = true;
            } else {
                self.tickets.snapshot = Some(snapshot);
                self.tickets.reviews = reviews;
                self.tickets.details = details.into_iter().map(|(id, detail)| (id, Loaded::Ready(detail))).collect();
                self.tickets.loading = words.contains("syncing");
            }
            if words.contains("error") {
                self.tickets.error = Some("Could not reach the server: the request timed out after 30 seconds".into());
            }
        }
        if words.contains("list") {
            self.tickets.layout = Layout::List;
        }
        self.tickets.expanded = words.contains("expand");
        match key {
            Some(key) => self.open_ticket(key, cx),
            None => self.show_tickets(cx),
        }
        if words.contains("picker") {
            self.tickets.picker = Some("tickets-provider");
        }
        if words.contains("esc") {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                let _ = this.update(cx, |this, cx| {
                    this.tickets.debug_escape = true;
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Usage or Settings in the main area.
    pub fn show_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.enter(view);
        self.menu_open = false;
        cx.notify();
    }

    pub fn show_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.enter(View::Sessions);
        self.focus_active(window, cx);
        cx.notify();
    }

    pub fn open_ticket(&mut self, key: Key, cx: &mut Context<Self>) {
        if let Key::Ticket(id) = &key {
            let id = id.clone();
            self.tickets_load_detail(&id, false, cx);
        }
        self.tickets.selected = Some(key);
        self.tickets.picker = None;
        // The ticket is read in a panel over the Board, which takes the keyboard.
        self.tickets.reader_focus_due = Some(true);
        self.show_tickets(cx);
    }

    /// Close the ticket being read (its ×, Esc, or a click beside it).
    pub fn close_reader(&mut self, cx: &mut Context<Self>) {
        self.tickets.selected = None;
        self.tickets.expanded = false;
        self.tickets.reader_drag = None;
        self.tickets.reader_focus_due = Some(false);
        cx.notify();
    }

    pub fn open_work(&mut self, work: &Work, window: &mut Window, cx: &mut Context<Self>) {
        if work.on_server {
            self.open_server_session(&work.id, window, cx);
        } else {
            self.center = crate::center::Center::Session;
            self.open_session(&work.id, window, cx);
        }
        cx.notify();
    }

    pub fn ticket_comment(&mut self, cx: &mut Context<Self>) {
        let Some(Key::Ticket(identifier)) = self.tickets.selected.clone() else { return };
        let Some(ticket) = self.tickets.ticket(&identifier).cloned() else { return };
        let body = self.tickets.comment.read(cx).text();
        if body.trim().is_empty() || self.tickets.posting {
            return;
        }
        self.tickets.posting = true;
        let env = self.env.clone();
        let where_posted = match ticket.provider {
            Provider::Linear => "Linear",
            Provider::GitHub => "GitHub",
        };
        let task = cx.background_spawn(async move {
            match ticket.provider {
                Provider::Linear => linear::Client::saved().map(|c| c.comment(&ticket.id, body.trim())).unwrap_or_else(|| Err(anyhow::anyhow!("Linear is not connected"))),
                Provider::GitHub => {
                    let number = ticket.id.parse::<u64>().map_err(|_| anyhow::anyhow!("not a GitHub issue number"))?;
                    github_tickets::comment(&ticket.team, number, &env, body.trim())
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.tickets.posting = false;
                match result {
                    Ok(()) => {
                        this.tickets.comment.update(cx, |c, cx| c.set_text("", cx));
                        this.tickets_load_detail(&identifier, true, cx);
                        this.show_toast(format!("Commented on {identifier} in {where_posted}."), cx);
                    }
                    Err(err) => this.show_toast(format!("Could not comment: {err:#}"), cx),
                }
            });
        })
        .detach();
        cx.notify();
    }

    // ----- connecting ------------------------------------------------------------------------

    pub fn linear_connect(&mut self, cx: &mut Context<Self>) {
        let key = self.tickets.key_input.read(cx).text().trim().to_string();
        if key.is_empty() || self.tickets.connecting {
            return;
        }
        self.tickets.connecting = true;
        self.tickets.connect_error = None;
        let task = cx.background_spawn(async move {
            let (viewer, org) = linear::Client::new(key.clone()).viewer()?;
            linear::save_key(&key)?;
            Ok::<_, anyhow::Error>((viewer, org))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.tickets.connecting = false;
                match result {
                    Ok((viewer, org)) => {
                        this.tickets.connected = true;
                        this.tickets.linear_key = true;
                        this.tickets.key_input.update(cx, |i, cx| i.set_text("", cx));
                        this.tickets.last_fetch = None;
                        this.tickets.people = None;
                        this.tickets.labels = None;
                        this.tickets.project_options = None;
                        this.tickets_refresh(cx);
                        this.linear_load_choices(cx);
                        this.show_toast(format!("Connected to Linear as {} ({org}).", viewer.name), cx);
                    }
                    Err(err) => this.tickets.connect_error = Some(format!("{err:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn linear_disconnect(&mut self, cx: &mut Context<Self>) {
        linear::forget_key();
        linear::forget_cache();
        self.tickets.connected = false;
        self.tickets.linear_key = false;
        self.tickets.snapshot = None;
        self.tickets.details.clear();
        self.tickets.people = None;
        self.tickets.labels = None;
        self.tickets.project_options = None;
        self.tickets.selected = None;
        self.tickets.seen = None;
        self.state.linear.me = None;
        self.state.linear.me_label = None;
        self.save();
        self.show_toast(format!("Signed out of Linear. The key is gone from {}.", glasscode_core::keychain::NAME), cx);
        cx.notify();
    }

    /// The workspace's people and reviewer labels, for Settings.
    pub fn linear_load_choices(&mut self, cx: &mut Context<Self>) {
        if !self.tickets.linear_key || (self.tickets.people.is_some() && self.tickets.labels.is_some()) {
            return;
        }
        let task = cx.background_spawn(async move {
            let client = linear::Client::saved()?;
            Some((client.people().unwrap_or_default(), client.reviewer_labels().unwrap_or_default()))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Some((people, labels)) = result {
                    this.tickets.people = Some(people);
                    this.tickets.labels = Some(labels);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn linear_changed(&mut self, message: String, cx: &mut Context<Self>) {
        self.tickets.picker = None;
        self.save();
        self.tickets.last_fetch = None;
        self.tickets_refresh(cx);
        self.show_toast(message, cx);
        cx.notify();
    }

    // ----- starting a session --------------------------------------------------------------

    pub fn open_start_ticket(&mut self, key: Key, existing: Option<(String, PathBuf, String)>, window: &mut Window, cx: &mut Context<Self>) {
        let ticket = match &key {
            Key::Ticket(id) => self.tickets.ticket(id).cloned(),
            Key::Pr(repo, number) => self.tickets.review(repo, *number).and_then(|r| linear::identifiers_in(&format!("{} {}", r.hit.title, r.hit.head)).into_iter().find_map(|id| self.tickets.ticket(&id).cloned())),
        };
        let reviewing = matches!(key, Key::Pr(..)) || ticket.as_ref().is_some_and(|t| self.tickets.reviewing().iter().any(|r| r.identifier == t.identifier));
        let repo = match &key {
            Key::Pr(repo, _) => Some(repo.clone()),
            Key::Ticket(_) => ticket.as_ref().and_then(|t| t.pull_requests().first().map(|(r, _)| r.clone())),
        };
        let project_id = existing.as_ref().map(|(p, _, _)| p.clone()).unwrap_or_else(|| self.ticket_project(repo.as_deref()));
        // The project's branch template (its .glasscode/settings.json), else the ticket's name.
        let branch_name = ticket.as_ref().map(|t| self.ticket_branch(&project_id, t)).unwrap_or_default();
        let branch = cx.new(|cx| TextInput::new("Branch", cx).mono().with_text(branch_name));
        let instruction = match (&ticket, reviewing) {
            (Some(t), false) => format!("Work on {} from Linear: “{}”. The ticket is below. Read it and its comments, then tell me your plan before you change anything.", t.identifier, t.title),
            _ => String::new(),
        };
        let prompt = cx.new(|cx| {
            let mut area = TextArea::new("What the agent should do first", 4, cx);
            area.set_text(instruction, cx);
            area
        });
        if let Some(t) = &ticket {
            let id = t.identifier.clone();
            self.tickets_load_detail(&id, false, cx);
        }
        // The project's default agent wins over Linear's "who drives a ticket's session".
        let agent = self.default_agent(&project_id, true).0;
        let can_move = !reviewing && ticket.as_ref().is_some_and(|t| matches!(t.status.kind.as_str(), "triage" | "backlog" | "unstarted"));
        self.modal = Some(Modal::StartTicket(StartForm {
            key,
            ticket,
            reviewing,
            agent,
            project_id,
            existing: existing.map(|(_, path, branch)| (path, branch)),
            branch,
            prompt,
            move_status: can_move && !self.state.linear.move_on_start_off,
            can_move,
            busy: false,
            error: None,
        }));
        let _ = window;
        cx.notify();
    }

    /// The project a session for a ticket starts in: the one of its pull request's repository,
    /// else the active session's, else the first in git.
    pub(crate) fn ticket_project(&self, repo: Option<&str>) -> String {
        let projects = self.git_projects();
        repo.and_then(|repo| projects.iter().find(|(_, slug)| slug.as_ref().is_some_and(|s| s.eq_ignore_ascii_case(repo))).map(|(id, _)| id.clone()))
            .or_else(|| self.active_session().map(|s| s.project_id.clone()))
            .or_else(|| projects.first().map(|(id, _)| id.clone()))
            .unwrap_or_default()
    }

    /// Projects in git, with their GitHub repository as the background scan last read it
    /// (`tickets_repos_tick`). `repo_slug` runs git, so it never runs here, on the main thread;
    /// the `.git` look is one file-system check, not git.
    fn git_projects(&self) -> Vec<(String, Option<String>)> {
        let scanned = self.tickets.repos.as_deref().unwrap_or_default();
        self.state.projects.iter().filter(|p| p.root.join(".git").exists()).map(|p| (p.id.clone(), repo_of(&p.root, scanned))).collect()
    }

    pub fn start_from_ticket(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::StartTicket(form)) = self.modal.as_mut() else { return };
        if form.busy {
            return;
        }
        let Some(project) = self.state.project(&form.project_id).cloned() else {
            form.error = Some("Pick a project first.".into());
            cx.notify();
            return;
        };
        let agent = form.agent;
        let account_id = if agent.uses_accounts() { self.state.defaults.for_agent(agent).cloned().or_else(|| self.state.accounts_for(agent).next().map(|a| a.id.clone())) } else { None };
        let Some(Modal::StartTicket(form)) = self.modal.as_mut() else { return };
        if agent.uses_accounts() && account_id.is_none() {
            form.error = Some(format!("Add a {} account first (Accounts → Add).", agent.name()));
            cx.notify();
            return;
        }
        let ticket = form.ticket.clone();
        let detail = ticket.as_ref().and_then(|t| match self.tickets.details.get(&t.identifier) {
            Some(Loaded::Ready(d)) => Some(d.clone()),
            _ => None,
        });
        let Some(Modal::StartTicket(form)) = self.modal.as_mut() else { return };
        let reviewing = form.reviewing;
        let key = form.key.clone();
        let existing = form.existing.clone();
        let branch = form.branch.read(cx).text().trim().replace(' ', "-");
        let instruction = form.prompt.read(cx).text();
        let move_status = form.move_status && form.can_move;
        // A review: its pull request (the request's, else the ticket's first open one).
        let review_pr: Option<(String, u64)> = match &key {
            Key::Pr(repo, n) => Some((repo.clone(), *n)),
            Key::Ticket(_) if reviewing => ticket.as_ref().and_then(|t| t.pull_requests().into_iter().next()),
            _ => None,
        };
        if reviewing && review_pr.is_none() {
            form.error = Some("This ticket has no pull request to review yet.".into());
            cx.notify();
            return;
        }
        if !reviewing && existing.is_none() && branch.is_empty() {
            form.error = Some("Name the branch.".into());
            cx.notify();
            return;
        }
        form.busy = true;
        form.error = None;
        cx.notify();
        let known_hit = review_pr.as_ref().and_then(|(repo, n)| self.tickets.review(repo, *n).map(|r| r.hit.clone()));
        let env = self.env.clone();
        let root = project.root.clone();
        let project_name = project.name.clone();
        let fetch = !self.state.prefs.fresh_main_off;
        let task = cx.background_spawn(async move {
            if let Some((repo, number)) = review_pr {
                let hit = match known_hit {
                    Some(hit) => hit,
                    None => github::search_prs(&repo, &number.to_string(), &env)?.into_iter().next().ok_or_else(|| anyhow::anyhow!("GitHub has no pull request {repo}#{number}"))?,
                };
                let review = glasscode_core::model::ReviewRef { repo: repo.clone(), number: hit.number, title: hit.title.clone(), author: hit.author.clone(), url: hit.url.clone(), head: hit.head.clone(), base: hit.base.clone(), private_review: None };
                git::fetch_branch(&root, &hit.base);
                let checkout = git::checkout_review(&root, &project_name, hit.number)?;
                // Its author's association decides whether the review is quarantined (D27).
                let association = glasscode_core::github_api::pr_association(&repo, hit.number, &env);
                return Ok::<_, anyhow::Error>((checkout.path, checkout.branch, Some(checkout.note), Some((review, association))));
            }
            if let Some((path, branch)) = existing {
                return Ok((path, branch, None, None));
            }
            let fresh = git::start_base(&root, fetch);
            let note = (fetch && !fresh.fetched).then(|| format!("Could not fetch origin (offline?); the branch starts from {} as it is on {}.", fresh.base, crate::platform::this_computer()));
            let path = git::add_worktree(&root, &project_name, &branch, &fresh.base)?;
            Ok((path, branch, note, None))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok((worktree, branch, note, reviewed)) => {
                    let (review, association) = match reviewed {
                        Some((review, association)) => (Some(review), association),
                        None => (None, None),
                    };
                    let title = match (&review, &ticket) {
                        (Some(r), _) => format!("#{} {}", r.number, r.title.chars().take(56).collect::<String>()),
                        (None, Some(t)) => linear::short_title(&t.title, 46),
                        (None, None) => format!("{} · {branch}", agent.name()),
                    };
                    let review_prompt = |r: &glasscode_core::model::ReviewRef| {
                        format!("Review pull request #{} \"{}\" by {} ({} into {}) in {}. Read its diff (git diff origin/{}...HEAD) and the code around the changes, then explain the PR and what is worth a close look.", r.number, r.title, if r.author.is_empty() { "its author" } else { &r.author }, r.head, if r.base.is_empty() { "main" } else { &r.base }, r.repo, if r.base.is_empty() { "main" } else { &r.base })
                    };
                    let first = match (&review, &ticket) {
                        (Some(r), Some(t)) => linear::first_message(&format!("{}\n\nThis pull request is for the Linear ticket below. Also say whether it does what the ticket asks.", review_prompt(r)), t, detail.as_ref()),
                        (Some(r), None) => review_prompt(r),
                        (None, Some(t)) => linear::first_message(&instruction, t, detail.as_ref()),
                        (None, None) => instruction.clone(),
                    };
                    let session = Session {
                        id: new_id(),
                        project_id: project.id.clone(),
                        worktree,
                        branch,
                        title,
                        agent,
                        account_id: account_id.clone(),
                        agent_session_id: Workspace::new_session_id_for(agent),
                        transcript_path: None,
                        lineage: vec![this.state.lineage_tag(agent, account_id.as_deref())],
                        initial_prompt: Some(first).filter(|p| !p.trim().is_empty()),
                        team_task: None,
                        model: None,
                        mode: None,
                        review: review.clone(),
                        created_at: now(),
                    };
                    if let Some(t) = &ticket {
                        this.state.ticket_of.insert(session.id.clone(), t.identifier.clone());
                    }
                    this.modal = None;
                    this.center = crate::center::Center::Session;
                    // A stranger's pull request is reviewed quarantined (D27): before it starts.
                    if let Some(r) = &review {
                        this.control_quarantine_for(&session.id, &r.author, association.as_deref());
                    }
                    this.add_and_open_session(session, window, cx);
                    this.save();
                    this.refresh_worktrees(cx);
                    if let (true, Some(t)) = (move_status, ticket.as_ref()) {
                        this.ticket_move_to_started(t.clone(), detail.clone(), cx);
                    }
                    if let Some(note) = note {
                        this.show_toast(note, cx);
                    }
                }
                Err(err) => {
                    if let Some(Modal::StartTicket(form)) = this.modal.as_mut() {
                        form.busy = false;
                        form.error = Some(format!("Could not start it: {err:#}"));
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Move a ticket to its team's first "started" status (In Progress).
    /// A card dropped on another column: the ticket moves there at once, and in Linear (to the
    /// status of that name in its team); if Linear says no, it goes back.
    pub fn ticket_move(&mut self, identifier: &str, to: &str, cx: &mut Context<Self>) {
        let Some(ticket) = self.tickets.ticket(identifier).cloned() else { return };
        if ticket.status.name == to {
            return;
        }
        let before = ticket.status.clone();
        let provider = ticket.provider;
        let where_moved = provider.label();
        // Optimistic: show the card in the target column at once.
        let optimistic = match provider {
            Provider::Linear => self.tickets.snapshot.as_ref().and_then(|s| s.mine.iter().chain(&s.reviewing).map(|t| &t.status).find(|st| st.name == to).cloned()),
            Provider::GitHub => Some(github_tickets::column_status(to)),
        };
        if let Some(status) = optimistic {
            self.tickets_set_status(identifier, status);
        }
        cx.notify();
        let to = to.to_string();
        let env = self.env.clone();
        let task = cx.background_spawn(async move {
            match provider {
                Provider::Linear => {
                    let client = linear::Client::saved().ok_or_else(|| anyhow::anyhow!("Linear is not connected"))?;
                    let detail = client.detail(&ticket.id)?;
                    let (state_id, status) = detail
                        .states
                        .iter()
                        .find(|(_, st)| st.name.eq_ignore_ascii_case(&to))
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("{} has no \"{to}\" status", if ticket.team.is_empty() { "its team".to_string() } else { format!("team {}", ticket.team) }))?;
                    client.set_status(&ticket.id, &state_id)?;
                    Ok::<_, anyhow::Error>((ticket.identifier, status))
                }
                Provider::GitHub => {
                    let number = ticket.id.parse::<u64>().map_err(|_| anyhow::anyhow!("not a GitHub issue number"))?;
                    github_tickets::set_status(&ticket.team, number, &env, &to, &github_tickets::status_prefix())?;
                    Ok((ticket.identifier, github_tickets::column_status(&to)))
                }
            }
        });
        let identifier = identifier.to_string();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((id, status)) => {
                        this.show_toast(format!("{id} is {} in {where_moved}.", status.name), cx);
                        this.tickets_set_status(&id, status);
                    }
                    Err(err) => {
                        this.tickets_set_status(&identifier, before);
                        this.show_toast(format!("Could not move {identifier} in {where_moved}: {err:#}"), cx);
                    }
                }
                this.tickets.last_fetch = None;
                this.tickets_refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Show a ticket in another status (before Linear's answer, or after it).
    pub(crate) fn tickets_set_status(&mut self, identifier: &str, status: linear::Status) {
        if let Some(snapshot) = self.tickets.snapshot.as_mut() {
            for ticket in snapshot.mine.iter_mut().chain(snapshot.reviewing.iter_mut()).filter(|t| t.identifier == identifier) {
                ticket.status = status.clone();
            }
        }
    }

    fn ticket_move_to_started(&mut self, ticket: Ticket, detail: Option<Detail>, cx: &mut Context<Self>) {
        let provider = ticket.provider;
        let where_moved = provider.label();
        let env = self.env.clone();
        let task = cx.background_spawn(async move {
            match provider {
                Provider::Linear => {
                    let client = linear::Client::saved().ok_or_else(|| anyhow::anyhow!("Linear is not connected"))?;
                    let detail = match detail {
                        Some(d) => d,
                        None => client.detail(&ticket.id)?,
                    };
                    let (state_id, status) = detail.started_state().cloned().ok_or_else(|| anyhow::anyhow!("its team has no started status"))?;
                    client.set_status(&ticket.id, &state_id)?;
                    Ok::<_, anyhow::Error>((ticket.identifier, status.name))
                }
                Provider::GitHub => {
                    let number = ticket.id.parse::<u64>().map_err(|_| anyhow::anyhow!("not a GitHub issue number"))?;
                    github_tickets::set_status(&ticket.team, number, &env, "In Progress", &github_tickets::status_prefix())?;
                    Ok((ticket.identifier, "In Progress".to_string()))
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((id, status)) => this.show_toast(format!("{id} is {status} in {where_moved}."), cx),
                    Err(err) => this.show_toast(format!("Could not move the ticket in {where_moved}: {err:#}"), cx),
                }
                this.tickets.last_fetch = None;
                this.tickets_refresh(cx);
            });
        })
        .detach();
    }
}

fn key_id(key: &Key) -> String {
    match key {
        Key::Ticket(id) => id.clone(),
        Key::Pr(repo, n) => format!("{repo}#{n}"),
    }
}

/// Whether a branch (or title) names this ticket, by the right convention for its provider:
/// Linear's `ENG-787`, or a GitHub issue number (`742-…`, `#742`, `issue-742`).
fn ticket_mentions(ticket: &Ticket, text: &str) -> bool {
    match ticket.provider {
        Provider::Linear => linear::mentions(text, &ticket.identifier),
        Provider::GitHub => ticket.id.parse::<u64>().map(|n| github_tickets::mentions(text, n)).unwrap_or(false),
    }
}

// ----- the start dialog -----------------------------------------------------------------------

pub struct StartForm {
    pub key: Key,
    pub ticket: Option<Ticket>,
    pub reviewing: bool,
    pub agent: AgentKind,
    pub project_id: String,
    /// An existing worktree to start in: (path, branch).
    pub existing: Option<(PathBuf, String)>,
    pub branch: Entity<TextInput>,
    pub prompt: Entity<TextArea>,
    pub move_status: bool,
    pub can_move: bool,
    pub busy: bool,
    pub error: Option<String>,
}

fn with_start_form(ws: &mut Workspace, f: impl FnOnce(&mut StartForm)) {
    if let Some(Modal::StartTicket(form)) = ws.modal.as_mut() {
        f(form);
    }
}

pub fn start_title(form: &StartForm) -> String {
    match (&form.key, &form.ticket, form.reviewing) {
        (Key::Pr(_, n), _, _) => format!("Review #{n}"),
        (_, Some(t), true) => format!("Review {}", t.identifier),
        (_, Some(t), false) => format!("Start a session for {}", t.identifier),
        _ => "Start a session".into(),
    }
}

pub fn start_subtitle(ws: &Workspace, form: &StartForm) -> String {
    match (&form.key, &form.ticket) {
        (_, Some(t)) => format!("{} · {}", t.identifier, t.title),
        (Key::Pr(repo, n), None) => ws.tickets.review(repo, *n).map(|r| format!("{repo}#{n} · {}", r.hit.title)).unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn render_start(ws: &Workspace, form: &StartForm, cx: &mut Context<Workspace>) -> AnyElement {
    let label = |text: &str| ui::caps_label(text.to_uppercase());
    let agents = [AgentKind::Claude, AgentKind::Codex].into_iter().map(|agent| {
        ui::chip(SharedString::from(format!("start-agent-{}", agent.name())), form.agent == agent, agent.name()).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            with_start_form(this, |f| f.agent = agent);
            cx.notify();
        }))
    });
    let projects: Vec<AnyElement> = ws
        .state
        .projects
        .iter()
        .filter(|p| p.root.join(".git").exists())
        .map(|p| {
            let id = p.id.clone();
            ui::chip(SharedString::from(format!("start-project-{}", p.id)), form.project_id == p.id, p.name.clone())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let id = id.clone();
                    with_start_form(this, |f| {
                        f.project_id = id;
                        f.existing = None;
                    });
                    cx.notify();
                }))
                .into_any_element()
        })
        .collect();
    // Where: a new worktree, or one that already has the ticket's branch.
    let mut places: Vec<AnyElement> = vec![ui::chip("start-where-new", form.existing.is_none(), if form.reviewing { "New review worktree" } else { "New worktree from the latest main" })
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            with_start_form(this, |f| f.existing = None);
            cx.notify();
        }))
        .into_any_element()];
    if let (Some(t), false) = (&form.ticket, form.reviewing) {
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut candidates: Vec<(String, PathBuf, String)> = ws.idle_worktrees_for(t);
        for w in ws.work_on_ticket(t).iter().filter(|w| !w.on_server) {
            if let Some(s) = ws.state.session(&w.id) {
                candidates.push((s.project_id.clone(), s.worktree.clone(), s.branch.clone()));
            }
        }
        for (i, (project_id, path, branch)) in candidates.into_iter().enumerate() {
            if !seen.insert(path.clone()) || project_id != form.project_id {
                continue;
            }
            let on = form.existing.as_ref().is_some_and(|(p, _)| *p == path);
            let pick = (path.clone(), branch.clone());
            places.push(
                ui::chip(SharedString::from(format!("start-where-{i}")), on, format!("Worktree {branch}"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let pick = pick.clone();
                        with_start_form(this, |f| f.existing = Some(pick));
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
    }
    let branch_note = match (&form.existing, form.reviewing, &form.ticket) {
        (Some(_), _, _) => "An existing worktree: nothing new is checked out.".to_string(),
        (None, true, _) => "Checked out from the pull request, as Review a pull request does.".to_string(),
        (None, false, Some(t)) => format!("Linear links the pull request to {} by the ID in the branch's name.", t.identifier),
        _ => String::new(),
    };
    let ticket_size = form.ticket.as_ref().map(|t| {
        let detail = match ws.tickets.details.get(&t.identifier) {
            Some(Loaded::Ready(d)) => format!(", {} comments", d.comments.len()),
            Some(Loaded::Loading) => ", comments loading…".into(),
            _ => String::new(),
        };
        format!("The ticket goes with it: its description{detail}, about {} words.", t.description.split_whitespace().count())
    });
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(div().flex().flex_col().gap_2().child(label("Who drives it")).child(div().flex().flex_wrap().gap_2().children(agents)))
        .when(projects.len() > 1, |d| d.child(div().flex().flex_col().gap_2().child(label("Project")).child(div().flex().flex_wrap().gap_2().children(projects))))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(label("Where"))
                .child(div().flex().flex_wrap().gap_2().children(places))
                .when(form.existing.is_none() && !form.reviewing, |d| {
                    d.child(form.branch.clone())
                })
                .child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(branch_note)),
        )
        .when(!form.reviewing, |d| {
            d.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(label("First message"))
                    .child(form.prompt.clone())
                    .when_some(ticket_size, |d, note| d.child(div().text_size(theme::sz(12.)).text_color(theme::muted()).child(note))),
            )
        })
        .when(form.reviewing, |d| {
            d.child(div().text_size(theme::sz(12.5)).text_color(theme::text_soft()).child(
                "Glasscode's review steps start: Explain, then Plan, then a private review. The ticket goes with it, so the agent also checks the pull request against what the ticket asks.",
            ))
        })
        .when(form.can_move, |d| {
            let id = form.ticket.as_ref().map(|t| t.identifier.clone()).unwrap_or_default();
            d.child(
                div()
                    .id("start-move")
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        with_start_form(this, |f| f.move_status = !f.move_status);
                        cx.notify();
                    }))
                    .child(ui::checkbox("start-move-box", form.move_status))
                    .child(format!("Move {id} to In Progress in Linear")),
            )
        })
        .when_some(form.error.clone(), |d, e| d.child(div().text_color(theme::removed()).child(e)))
        .into_any_element()
}

pub fn start_footer(form: &StartForm, cx: &mut Context<Workspace>) -> AnyElement {
    let label = if form.busy {
        "Starting…"
    } else if form.reviewing {
        "Start review"
    } else {
        "Start session"
    };
    div()
        .flex()
        .justify_end()
        .gap_2()
        .child(ui::button("start-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx))))
        .child(if form.busy {
            ui::disabled_button("start-go", label).into_any_element()
        } else {
            ui::primary_button("start-go", label).on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.start_from_ticket(window, cx))).into_any_element()
        })
        .into_any_element()
}

// ----- Settings › Integrations › Linear -------------------------------------------------------

pub(crate) fn picker(id: &'static str, value: String, open: bool, options: Vec<(String, String, bool, Box<dyn Fn(&mut Workspace, &mut Context<Workspace>)>)>, cx: &mut Context<Workspace>) -> AnyElement {
    let rows: Vec<AnyElement> = options
        .into_iter()
        .enumerate()
        .map(|(i, (label, detail, on, pick))| {
            div()
                .id(SharedString::from(format!("{id}-opt-{i}")))
                .flex()
                .items_center()
                .gap_2p5()
                .min_h(theme::sz(34.))
                .px_2p5()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(on, |d| d.bg(theme::selected()))
                .hover(|s| s.bg(theme::hover()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| pick(this, cx)))
                .child(div().flex().flex_col().flex_1().min_w_0().child(label).when(!detail.is_empty(), |d| d.child(div().text_size(theme::sz(11.5)).text_color(theme::muted()).child(detail))))
                .when(on, |d| d.child(div().text_color(theme::accent()).child("✓")))
                .into_any_element()
        })
        .collect();
    div()
        .relative()
        .min_w(theme::sz(240.))
        .child(
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_2()
                .h(theme::sz(32.))
                .px_3()
                .rounded_md()
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::bg())
                .cursor_pointer()
                .hover(|s| s.border_color(theme::faint()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.tickets.picker = if this.tickets.picker == Some(id) { None } else { Some(id) };
                    cx.notify();
                }))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(value))
                .child(div().text_color(theme::muted()).child("▾")),
        )
        .when(open, |d| {
            d.child(
                deferred(
                    div()
                        .id(SharedString::from(format!("{id}-list")))
                        .absolute()
                        .top(theme::sz(36.))
                        .right_0()
                        .w(theme::sz(320.))
                        .max_h(theme::sz(320.))
                        .overflow_y_scroll()
                        .p_1()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::dialog())
                        .shadow_lg()
                        .children(rows),
                )
                .with_priority(2),
            )
        })
        .into_any_element()
}

fn setting_row(label: &str, detail: &str, control: AnyElement) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_4()
        .py_1p5()
        .child(div().flex().flex_col().flex_1().min_w_0().child(label.to_string()).when(!detail.is_empty(), |d| d.child(div().text_size(theme::sz(12.)).text_color(theme::card_soft_text()).child(detail.to_string()))))
        .child(control)
        .into_any_element()
}

/// Linear's part of Settings and of the first run, whose cards name it already (no caption here).
/// Text in `text`, `text_soft` and `card_soft_text`, so that it reads on glass too.
pub fn render_linear_section(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let section = div().flex().flex_col().gap_2();
    // Linear's own key: with a GitHub repository the board is set up (`connected`) without one.
    if !ws.tickets.linear_key {
        return section.child(render_connect(ws, cx)).into_any_element();
    }
    let settings = &ws.state.linear;
    // The board's last fetch, and its error, are Linear's only while the board shows Linear.
    let from_linear = ws.tickets.fetched_from == Provider::Linear;
    let snapshot = ws.tickets.snapshot.as_ref().filter(|_| from_linear);
    let error = ws.tickets.error.clone().filter(|_| from_linear);
    let viewer = snapshot.map(|s| format!("Signed in as {}{} · {}", s.viewer.name, if s.viewer.email.is_empty() { String::new() } else { format!(" ({})", s.viewer.email) }, s.org)).unwrap_or_else(|| {
        if from_linear { "Signed in · fetching…".into() } else { "Signed in. The Board shows this repository's GitHub issues now.".into() }
    });
    let me_value = settings.me_label.clone().or_else(|| snapshot.map(|s| if s.viewer.email.is_empty() { s.viewer.name.clone() } else { s.viewer.email.clone() })).unwrap_or_else(|| "Who the key belongs to".into());
    let me_options: Vec<(String, String, bool, Box<dyn Fn(&mut Workspace, &mut Context<Workspace>)>)> = match &ws.tickets.people {
        Some(people) => people
            .iter()
            .map(|p| {
                let on = settings.me.as_deref().map_or_else(|| snapshot.is_some_and(|s| s.viewer.id == p.id), |me| me == p.id);
                let key_owner = snapshot.is_some_and(|s| s.viewer.id == p.id);
                let person = p.clone();
                let label = if p.email.is_empty() { p.name.clone() } else { p.email.clone() };
                let detail = format!("{}{}", p.name, if key_owner { " · the key's own account" } else { "" });
                let pick: Box<dyn Fn(&mut Workspace, &mut Context<Workspace>)> = Box::new(move |this, cx| {
                    let own = this.tickets.snapshot.as_ref().is_some_and(|s| s.viewer.id == person.id);
                    this.state.linear.me = (!own).then(|| person.id.clone());
                    this.state.linear.me_label = (!own).then(|| if person.email.is_empty() { person.name.clone() } else { person.email.clone() });
                    this.linear_changed(format!("Saved. Assigned to me shows {}’s tickets.", person.name), cx);
                });
                (label, detail, on, pick)
            })
            .collect(),
        None => vec![("Loading the workspace’s people…".into(), String::new(), false, Box::new(|_: &mut Workspace, _: &mut Context<Workspace>| {}) as Box<dyn Fn(&mut Workspace, &mut Context<Workspace>)>)],
    };
    let label_value = ws.tickets.label().map(str::to_string).or_else(|| settings.reviewer_label.clone()).unwrap_or_else(|| "Pick your label".into());
    let label_options: Vec<(String, String, bool, Box<dyn Fn(&mut Workspace, &mut Context<Workspace>)>)> = match &ws.tickets.labels {
        Some(labels) if !labels.is_empty() => labels
            .iter()
            .map(|l| {
                let on = ws.tickets.label() == Some(l.as_str());
                let label = l.clone();
                let pick: Box<dyn Fn(&mut Workspace, &mut Context<Workspace>)> = Box::new(move |this, cx| {
                    this.state.linear.reviewer_label = Some(label.clone());
                    this.linear_changed(format!("Saved. To review shows tickets labelled “{label}”."), cx);
                });
                (l.clone(), String::new(), on, pick)
            })
            .collect(),
        Some(_) => vec![("The workspace has no labels starting with “Reviewer”".into(), String::new(), false, Box::new(|_: &mut Workspace, _: &mut Context<Workspace>| {}) as Box<dyn Fn(&mut Workspace, &mut Context<Workspace>)>)],
        None => vec![("Loading the workspace’s labels…".into(), String::new(), false, Box::new(|_: &mut Workspace, _: &mut Context<Workspace>| {}) as Box<dyn Fn(&mut Workspace, &mut Context<Workspace>)>)],
    };
    let me_picker = picker("linear-me", me_value, ws.tickets.picker == Some("linear-me"), me_options, cx);
    let label_picker = picker("linear-label", label_value, ws.tickets.picker == Some("linear-label"), label_options, cx);
    let check = |id: &'static str, on: bool, label: &'static str, detail: &'static str, flip: fn(&mut glasscode_core::linear::LinearSettings)| {
        div()
            .id(id)
            .flex()
            .items_start()
            .gap_2p5()
            .py_1p5()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                flip(&mut this.state.linear);
                this.linear_changed("Saved.".into(), cx);
            }))
            .child(div().mt_0p5().child(ui::checkbox(SharedString::from(format!("{id}-box")), on)))
            .child(div().flex().flex_col().child(label).when(!detail.is_empty(), |d| d.child(div().text_size(theme::sz(12.)).text_color(theme::card_soft_text()).child(detail))))
    };
    let agent = settings.agent.unwrap_or(AgentKind::Claude);
    let agent_chips = div().flex().gap_2().children([AgentKind::Claude, AgentKind::Codex].into_iter().map(|a| {
        ui::chip(SharedString::from(format!("linear-agent-{}", a.name())), agent == a, a.name()).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.state.linear.agent = Some(a);
            this.save();
            this.show_toast("Saved.", cx);
            cx.notify();
        }))
    }));
    section
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(ui::dot(if error.is_some() { theme::removed() } else { theme::added() }))
                .child(div().flex_1().child(viewer))
                .child(ui::button("linear-signout", "Sign out").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.linear_disconnect(cx)))),
        )
        .when_some(error, |d, e| d.child(div().text_size(theme::sz(12.)).text_color(theme::removed()).child(e)))
        .child(setting_row("You are", "Tickets assigned to this person show under Assigned to me", me_picker))
        .child(setting_row("Your reviewer label", "Your team marks reviewers with a label. Tickets with this one show under To review.", label_picker))
        .child(check("linear-reviews", !settings.review_prs_off, "Also show pull requests that ask for your review", "From GitHub, as your default GitHub account", |s| s.review_prs_off = !s.review_prs_off))
        .child(setting_row("Sessions from tickets start with", "You can still pick each time", agent_chips.into_any_element()))
        .child(check("linear-move", !settings.move_on_start_off, "Move a ticket to In Progress when its session starts", "Only from Triage, Backlog or Todo; you can turn it off each time", |s| s.move_on_start_off = !s.move_on_start_off))
        .child(check("linear-notify", !settings.notify_off, "Tell me when a ticket is assigned to me or a review asks for me", "", |s| s.notify_off = !s.notify_off))
        .into_any_element()
}

/// Paste a personal API key (in Settings, and on the Tickets view before it is connected).
pub fn render_connect(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_color(theme::text_soft()).child(format!("Connect Linear with a personal API key. Glasscode reads your tickets and posts the comments you write here; the key stays in {}.", crate::platform::secret_store())))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(theme::sz(12.))
                .text_color(theme::card_soft_text())
                .child("1. Make a key in Linear: Settings › Security & access › Personal API keys.")
                .child(ui::button("linear-key-page", "Open Linear").on_click(|_: &ClickEvent, _, cx| crate::center::open_url(linear::KEY_PAGE, cx))),
        )
        .child(div().text_size(theme::sz(12.)).text_color(theme::card_soft_text()).child("2. Paste it here."))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().child(ws.tickets.key_input.clone()))
                .child(if ws.tickets.connecting {
                    ui::disabled_button("linear-connect", "Checking…").into_any_element()
                } else {
                    ui::primary_button("linear-connect", "Connect").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.linear_connect(cx))).into_any_element()
                }),
        )
        .when_some(ws.tickets.connect_error.clone(), |d, e| d.child(div().text_size(theme::sz(12.)).text_color(theme::removed()).child(e)))
        .into_any_element()
}

/// The colour for a label's dot.
/// A made-up board for the `sample` debug hook: no real organisation, repository or person.
fn sample_board(provider: Provider, now: u64) -> (Snapshot, Vec<ReviewRequest>, Vec<(String, Detail)>) {
    let github = provider == Provider::GitHub;
    let me = Person { id: "u-sam".into(), name: "Sam Example".into(), email: "sam@example.com".into() };
    let kim = Person { id: "u-kim".into(), name: "Kim Example".into(), email: "kim@example.com".into() };
    let status = |name: &str, kind: &str, position: f64| linear::Status { name: name.into(), kind: kind.into(), position };
    let ticket = |n: u64, title: &str, status: linear::Status, priority: u8, labels: &[&str], due: Option<&str>, assignee: &Person| {
        let identifier = if github { format!("#{n}") } else { format!("ENG-{n}") };
        Ticket {
            id: if github { n.to_string() } else { format!("id-{n}") },
            branch: format!("sam/eng-{n}-{}", title.to_lowercase().split_whitespace().take(3).collect::<Vec<_>>().join("-")),
            url: if github { format!("https://github.com/acme/app/issues/{n}") } else { format!("https://linear.app/acme/issue/ENG-{n}") },
            identifier,
            title: title.into(),
            description: "The list jumps when a row comes in.\n\n## What to do\n\n- [x] Find where the height is measured\n- [ ] Keep the scroll place when a row is added\n- [ ] Check it at 130 %\n\nSee `list_view.rs` for the measure.".into(),
            priority,
            due: due.map(str::to_string),
            created_at: "2026-11-10T09:00:00Z".into(),
            updated_at: "2026-11-18T09:00:00Z".into(),
            status,
            assignee: Some(assignee.clone()),
            creator: Some("Kim Example".into()),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            team: if github { "acme/app".into() } else { "ENG".into() },
            provider,
            ..Default::default()
        }
    };
    let (progress, review, todo, backlog, done) = (status("In Progress", "started", 0.), status("In Review", "started", 1.), status("Todo", "unstarted", 0.), status("Backlog", "backlog", 0.), status("Done", "completed", 0.));
    let mut linked = ticket(123, "Keep the scroll place when a row is added", progress.clone(), 2, &["bug", "list"], Some("2026-11-24"), &me);
    linked.attachments = vec![linear::Attachment { title: "fix: keep the scroll place".into(), subtitle: String::new(), url: "https://github.com/acme/app/pull/133".into(), source: "github".into() }];
    let mut finished = ticket(98, "Show the build number in the title bar", done, 4, &[], None, &me);
    finished.completed_at = Some("2026-11-12T16:00:00Z".into());
    let snapshot = Snapshot {
        viewer: me.clone(),
        org: "Acme".into(),
        me: me.id.clone(),
        label: None,
        mine: vec![
            linked,
            ticket(127, "The sidebar forgets its width after a restart", progress, 1, &["bug"], None, &me),
            ticket(131, "A long title that goes over two lines on a card, so that the card shows how it cuts the words at the end", todo.clone(), 3, &["design", "board", "polish"], Some("2026-12-04"), &me),
            ticket(134, "Write the guide for the scratch home", todo.clone(), 0, &["docs"], None, &me),
            ticket(135, "Retry after the network comes back", todo, 3, &[], None, &me),
            ticket(140, "Group the notifications of one session", backlog, 4, &["idea"], None, &me),
            finished,
        ],
        reviewing: vec![ticket(129, "Sort the board columns by status", review, 3, &["board"], None, &kim)],
        fetched_at: now,
    };
    let hit = |number: u64, title: &str, author: &str, head: &str| github::PrHit {
        number,
        title: title.into(),
        url: format!("https://github.com/acme/app/pull/{number}"),
        author: author.into(),
        head: head.into(),
        base: "main".into(),
        same_repo: true,
        state: "OPEN".into(),
        review_decision: "REVIEW_REQUIRED".into(),
        updated_at: "2026-11-18T08:00:00Z".into(),
        additions: 42,
        deletions: 7,
        changed_files: 3,
        ..Default::default()
    };
    let reviews = vec![
        ReviewRequest { repo: "acme/app".into(), hit: hit(133, "fix: keep the scroll place", "kim-example", "kim/eng-123-keep-the-scroll"), created_at: "2026-11-17T12:00:00Z".into(), merge_state: "CLEAN".into(), reviews: Vec::new() },
        ReviewRequest { repo: "acme/app".into(), hit: hit(126, "fix: reconnect after sleep", "kim-example", "reconnect"), created_at: "2026-11-14T09:00:00Z".into(), merge_state: "DIRTY".into(), reviews: vec![("lee-example".into(), "COMMENTED".into())] },
    ];
    let comments = vec![
        linear::Comment { author: "Kim Example".into(), body: "It also jumps when a row goes away. Same cause?".into(), created_at: "2026-11-17T10:00:00Z".into(), reply: false },
        linear::Comment { author: "Sam Example".into(), body: "Yes: the measure runs before the row is in.".into(), created_at: "2026-11-17T11:00:00Z".into(), reply: true },
    ];
    let details = snapshot.mine.iter().chain(snapshot.reviewing.iter()).map(|t| (t.identifier.clone(), Detail { description: String::new(), comments: comments.clone(), states: Vec::new() })).collect();
    (snapshot, reviews, details)
}

pub fn label_color(label: &str) -> Hsla {
    if label.starts_with("Reviewer") {
        theme::codex()
    } else if label.eq_ignore_ascii_case("bug") {
        theme::removed()
    } else if label.eq_ignore_ascii_case("idea") || label.eq_ignore_ascii_case("feature") {
        theme::modified()
    } else {
        theme::muted()
    }
}

pub fn bold(text: impl Into<SharedString>) -> gpui::Div {
    div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text_strong()).child(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample board is made up: no real organisation, repository or person, in both kinds.
    #[test]
    fn the_sample_board_has_no_real_data() {
        for provider in [Provider::Linear, Provider::GitHub] {
            let (snapshot, reviews, details) = sample_board(provider, 0);
            assert_eq!(snapshot.org, "Acme");
            assert!(snapshot.mine.iter().chain(snapshot.reviewing.iter()).all(|t| t.provider == provider && t.url.contains("/acme/")));
            assert!(reviews.iter().all(|r| r.repo == "acme/app" && r.hit.author.ends_with("-example")));
            assert_eq!(details.len(), snapshot.mine.len() + snapshot.reviewing.len(), "each ticket has its comments, so none is fetched");
        }
        assert_eq!(sample_board(Provider::Linear, 0).0.mine[0].identifier, "ENG-123");
        assert_eq!(sample_board(Provider::GitHub, 0).0.mine[0].identifier, "#123");
    }

    fn folders(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    /// The repositories are read once, again when a project comes or goes, and every two
    /// minutes; not on each tick.
    #[test]
    fn the_repositories_are_read_when_the_projects_change() {
        let roots = folders(&["a", "b"]);
        let scanned = vec![(PathBuf::from("a"), Some("acme/app".to_string())), (PathBuf::from("b"), None)];
        assert!(repos_due(&roots, None, None), "never read");
        assert!(!repos_due(&roots, Some(&scanned), Some(Duration::from_secs(1))), "read a second ago");
        assert!(repos_due(&roots, Some(&scanned), Some(REFRESH_EVERY)), "two minutes old");
        assert!(repos_due(&folders(&["a", "b", "c"]), Some(&scanned), Some(Duration::from_secs(1))), "a project was added");
        assert!(repos_due(&folders(&["b", "a"]), Some(&scanned), Some(Duration::from_secs(1))), "the projects changed order");
    }

    /// In project order, only the projects that have a GitHub repository, and only those the
    /// scan read.
    #[test]
    fn the_repositories_follow_the_projects() {
        let scanned = vec![(PathBuf::from("a"), Some("acme/app".to_string())), (PathBuf::from("b"), None), (PathBuf::from("c"), Some("acme/docs".to_string()))];
        assert_eq!(repos_of(&folders(&["c", "b", "a"]), &scanned), vec!["acme/docs".to_string(), "acme/app".to_string()]);
        assert_eq!(repos_of(&folders(&["d"]), &scanned), Vec::<String>::new(), "a project the scan has not read yet");
    }

    /// The start dialog takes a project's repository from the scan, never from git on the main
    /// thread: before the scan, or for a folder with no GitHub origin, there is none.
    #[test]
    fn a_project_repository_comes_from_the_scan() {
        let scanned = vec![(PathBuf::from("a"), Some("acme/app".to_string())), (PathBuf::from("b"), None)];
        assert_eq!(repo_of(std::path::Path::new("a"), &scanned).as_deref(), Some("acme/app"));
        assert_eq!(repo_of(std::path::Path::new("b"), &scanned), None, "no GitHub origin");
        assert_eq!(repo_of(std::path::Path::new("c"), &scanned), None, "not scanned yet");
        assert_eq!(repo_of(std::path::Path::new("a"), &[]), None, "the scan has not come back");
    }
}
