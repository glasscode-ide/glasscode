//! How sessions show and behave around the terminal: their names (chosen by the person, or
//! from their pull request, Claude's own title or their first message), placeholder branches
//! renamed to match before they are pushed, the sidebar's tree (projects, worktrees, sessions),
//! the "In this worktree" tabs, telling the person when a session is done or needs them, and the
//! sidebars' widths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{AnyElement, ClickEvent, Context, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, SharedString, div, prelude::*};
use glasscode_core::agents::AgentActivity;
use glasscode_core::model::{AgentKind, Session};
use glasscode_core::{git, naming};

use crate::center::Center;
use crate::shell::View;
use crate::sidebar::{self, OnClick, PrState, ProjectLine, RowActions, RowState, SessionLine, Tree, TreeProject, TreeRow, TreeSession, WorktreeLine, agent_icon, status_icon, without_number};
use crate::theme;
use crate::workspace::{RIGHT_WIDTH, SIDEBAR_WIDTH, Side, Workspace};

const TITLE_LEN: usize = 70;

impl Workspace {
    // ----- names --------------------------------------------------------------------------

    /// What a session is called in the sidebar and elsewhere.
    pub fn session_title(&self, s: &Session) -> String {
        let number = s.review.as_ref().map(|r| r.number);
        if self.state.auto_names_off || self.state.custom_titles.contains(&s.id) {
            return without_number(&s.title, number);
        }
        let pr = self.prs.map.get(&s.id).and_then(|p| p.pr.as_ref());
        if !naming::is_generated_title(&s.title, s.agent, &s.branch, pr.map(|p| p.title.as_str())) {
            return s.title.clone();
        }
        if let Some(review) = s.review.as_ref().filter(|r| !r.title.is_empty()) {
            return naming::tidy_title(&review.title, TITLE_LEN);
        }
        if let Some(pr) = pr {
            return naming::tidy_title(&pr.title, TITLE_LEN);
        }
        if let Some(title) = self.auto_titles.get(&s.id) {
            return naming::tidy_title(title, TITLE_LEN);
        }
        without_number(&s.title, number)
    }

    /// Every half minute: read what unnamed sessions are about, and give placeholder branches
    /// a name from it (only before they are pushed, and only while nobody works in them).
    pub fn titles_tick(&mut self, cx: &mut Context<Self>) {
        if self.titles_running || self.titles_checked.elapsed() < Duration::from_secs(30) || self.state.auto_names_off {
            return;
        }
        self.titles_checked = Instant::now();
        let jobs: Vec<(String, AgentKind, PathBuf)> = self
            .state
            .sessions
            .iter()
            .filter(|s| s.agent != AgentKind::Shell && s.review.is_none() && s.team_task.is_none() && !self.state.custom_titles.contains(&s.id))
            .filter_map(|s| Some((s.id.clone(), s.agent, s.transcript_path.clone()?)))
            .collect();
        if jobs.is_empty() {
            return;
        }
        self.titles_running = true;
        let task = cx.background_spawn(async move {
            jobs.into_iter().filter_map(|(id, agent, path)| Some((id, glasscode_core::history::session_title(agent, &path)?))).collect::<HashMap<String, String>>()
        });
        cx.spawn(async move |this, cx| {
            let titles = task.await;
            let _ = this.update(cx, |this, cx| {
                this.titles_running = false;
                if this.auto_titles != titles {
                    this.auto_titles = titles;
                    cx.notify();
                }
                this.rename_placeholder_branches(cx);
            });
        })
        .detach();
    }

    /// `glasscode/session-9ade` becomes `glasscode/<what the session is about>`, when the branch was never
    /// pushed and none of the worktree's sessions is working.
    fn rename_placeholder_branches(&mut self, cx: &mut Context<Self>) {
        let mut jobs: Vec<(PathBuf, PathBuf, String, String)> = Vec::new();
        for s in &self.state.sessions {
            // Leave hidden projects untouched — no background branch renames on them.
            if self.state.session_hidden(s) {
                continue;
            }
            if !naming::is_placeholder_branch(&s.branch) || self.rename_skip.contains(&s.worktree) || self.state.custom_titles.contains(&s.id) {
                continue;
            }
            if jobs.iter().any(|(w, ..)| *w == s.worktree) {
                continue;
            }
            let busy = self.state.sessions.iter().any(|o| o.worktree == s.worktree && self.activity.get(&o.id) == Some(&AgentActivity::Working));
            let Some(title) = self.auto_titles.get(&s.id) else { continue };
            let Some(branch) = naming::branch_for(&naming::tidy_title(title, TITLE_LEN)) else { continue };
            let Some(root) = self.state.project(&s.project_id).map(|p| p.root.clone()) else { continue };
            if !busy {
                jobs.push((s.worktree.clone(), root, s.branch.clone(), branch));
            }
        }
        if jobs.is_empty() {
            return;
        }
        let task = cx.background_spawn(async move {
            jobs.into_iter()
                .map(|(worktree, root, old, wanted)| {
                    if git::branch_was_pushed(&worktree, &old) {
                        return (worktree, old, Err("pushed".to_string()));
                    }
                    let name = std::iter::once(wanted.clone()).chain((2..10).map(|n| format!("{wanted}-{n}"))).find(|b| !git::branch_exists(&root, b));
                    let result = match name {
                        Some(name) => git::rename_branch(&worktree, &old, &name).map(|()| name).map_err(|e| e.to_string()),
                        None => Err("no free name".into()),
                    };
                    (worktree, old, result)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = task.await;
            let _ = this.update(cx, |this, cx| {
                for (worktree, old, result) in results {
                    match result {
                        Ok(new) => {
                            for s in this.state.sessions.iter_mut().filter(|s| s.worktree == worktree) {
                                s.branch = new.clone();
                            }
                            this.show_toast(format!("Named the branch {new} (it was {old})."), cx);
                        }
                        Err(_) => {
                            this.rename_skip.insert(worktree);
                        }
                    }
                }
                this.save();
                this.refresh_worktrees(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Mark a new session's title as the person's own (returns its id, for building it).
    pub fn claim_custom_title(&mut self, id: String) -> String {
        self.state.custom_titles.insert(id.clone());
        id
    }

    pub fn open_rename(&mut self, session_id: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(session) = self.state.session(session_id).cloned() else { return };
        let current = self.session_title(&session);
        self.modal = Some(crate::dialogs::Modal::rename(session_id, &current, window, cx));
        cx.notify();
    }

    /// The Rename dialog's name: the person's from now on (empty: Glasscode names it again).
    pub fn rename_from_form(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(crate::dialogs::Modal::Rename(form)) = self.modal.as_ref() else { return };
        let (id, name) = (form.session_id.clone(), form.input.read(cx).text().trim().to_string());
        if name.is_empty() {
            self.state.custom_titles.remove(&id);
        } else if let Some(session) = self.state.sessions.iter_mut().find(|s| s.id == id) {
            session.title = name;
            self.state.custom_titles.insert(id);
        }
        self.save();
        self.close_modal(window, cx);
    }

    /// Whether a project's folder is a git repository (else sessions work in the folder).
    pub fn is_git_project(&self, project_id: &str) -> bool {
        self.state.project(project_id).is_none_or(|p| p.root.join(".git").exists())
    }

    /// `git init` and a first commit, for a project added without git.
    pub fn set_up_git(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let Some(project) = self.state.project(project_id).cloned() else { return };
        self.show_toast(format!("Setting up git in {}…", project.name), cx);
        let root = project.root.clone();
        let task = cx.background_spawn(async move { git::init_repository(&root) });
        let id = project_id.to_string();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(secrets) => {
                        let root = this.state.project(&id).map(|p| p.root.clone());
                        for s in this.state.sessions.iter_mut().filter(|s| s.project_id == id && Some(&s.worktree) == root.as_ref()) {
                            s.branch = "main".into();
                        }
                        this.save();
                        this.show_toast(if secrets.is_empty() {
                            format!("{} uses git now (main, with a first commit). New sessions get their own worktree.", project.name)
                        } else {
                            format!("{} uses git now. Kept out of the first commit because they look like secrets: {}.", project.name, secrets.join(", "))
                        }, cx);
                    }
                    Err(error) => this.show_toast(format!("Could not set up git: {error:#}"), cx),
                }
                this.refresh_worktrees(cx);
                this.refresh_git(cx);
                cx.notify();
            });
        })
        .detach();
    }

    // ----- telling the person -------------------------------------------------------------

    /// A session's agent changed what it is doing: say so when it finished its turn or needs
    /// the person, unless they are looking at it.
    pub fn activity_changed(&mut self, id: &str, previous: Option<AgentActivity>, now: AgentActivity, cx: &mut Context<Self>) {
        let finished = previous == Some(AgentActivity::Working) && now == AgentActivity::Idle;
        let needs = now == AgentActivity::NeedsInput && previous != Some(AgentActivity::NeedsInput);
        let wanted = (finished && !self.state.notify.finished_off) || (needs && !self.state.notify.needs_input_off);
        if !wanted {
            return;
        }
        let watching = self.window_active && self.center == Center::Session && self.state.active_session.as_deref() == Some(id);
        if watching {
            return;
        }
        let Some(session) = self.state.session(id).cloned() else { return };
        let title = format!("{} {}", session.agent.name(), if needs { "needs you" } else { "finished" });
        let body = self.session_title(&session);
        self.notify_session(id, session.agent, title, body, needs, cx);
    }

    // ----- the sidebar --------------------------------------------------------------------

    /// The tree of the left sidebar (`sidebar::tree`), then the Server group and "Add project…",
    /// for the scroll area of `shell::render_left`. It runs on each frame, so it reads only what
    /// is in memory: git's worktrees come from the last refresh, not from git.
    pub(crate) fn render_tree(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let projects: Vec<TreeProject> = self.state.visible_projects().map(|project| TreeProject { project, worktrees: self.worktrees.get(&project.id).map(Vec::as_slice) }).collect();
        let sessions: Vec<TreeSession> = self
            .state
            .visible_sessions()
            .map(|session| TreeSession { session, state: self.row_state(session), running: self.terminals.contains_key(&session.id), pr: self.pr_state(&session.id) })
            .collect();
        // A session row is marked only where it is on screen: in the Sessions view.
        let selected = self.state.active_session.as_deref().filter(|_| self.view == View::Sessions && self.center == Center::Session);
        let tree = Tree {
            projects: &projects,
            sessions: &sessions,
            pinned: &self.state.pinned,
            folded: &self.state.layout.folded,
            removing: &self.removing,
            idle_open: &self.idle_worktrees_open,
            selected,
        };
        let rows = sidebar::tree(&tree, |s| self.session_title(s));
        // The GitHub account shows by the project's name when the sidebar has the room (it has
        // at its usual 300 px); in a narrower one it shows with the row's buttons.
        let github_inline = self.sidebar_width() / theme::scale() >= 280.;
        let mut out: Vec<AnyElement> = rows.into_iter().map(|row| self.tree_row(row, github_inline, cx)).collect();
        if projects.is_empty() {
            out.push(
                div()
                    .px(theme::sz(8.))
                    .py(theme::sz(10.))
                    .text_size(theme::sz(12.5))
                    .text_color(theme::muted())
                    .child(if self.state.projects.is_empty() {
                        "No projects yet. Add a git repository to start."
                    } else {
                        "All projects are switched off in Settings › Projects."
                    })
                    .into_any_element(),
            );
        }
        out.extend(self.render_server_sidebar(cx));
        out.push(
            div()
                .id("add-project")
                .flex()
                .items_center()
                .gap(theme::sz(6.))
                .h(theme::sz(28.))
                .mt(theme::sz(6.))
                .px(theme::sz(6.))
                .rounded(theme::sz(6.))
                .cursor_pointer()
                .text_size(theme::sz(12.5))
                .text_color(theme::text_soft())
                .hover(|s| s.bg(theme::hover()))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.prompt_add_project(window, cx)))
                .child(crate::icons::icon(crate::icons::PLUS, theme::sz(14.), theme::sz(14.), theme::muted()))
                .child("Add project…")
                .into_any_element(),
        );
        out
    }

    /// One row of the tree, with what its clicks do.
    fn tree_row(&self, row: TreeRow, github_inline: bool, cx: &mut Context<Self>) -> AnyElement {
        match row {
            TreeRow::Project { id, name, need, worktrees, folded, current } => {
                let github = self.github_account_for(&id).map(|login| {
                    let open: OnClick = Box::new(cx.listener(|this, _: &ClickEvent, window, cx| this.open_settings(crate::settings_ui::Section::Projects, window, cx)));
                    (login, open)
                });
                let menu_open = self.row_menu.as_ref().is_some_and(|m| m.project_id == id && m.worktree.is_none());
                let (fold, add, menu, right) = (id.clone(), id.clone(), id.clone(), id.clone());
                sidebar::project_row(
                    ProjectLine { id, name, need, worktrees, folded, current, github, github_inline, menu_open },
                    RowActions {
                        click: Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| this.fold(sidebar::project_key(&fold), !folded, cx))),
                        add: Some(Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            // Opened, so that the new session is not hidden under the fold.
                            if folded {
                                this.fold(sidebar::project_key(&add), false, cx);
                            }
                            this.open_new_session(Some(add.clone()), None, window, cx)
                        }))),
                        menu: Some((
                            Box::new(cx.listener(move |this, event: &ClickEvent, _, cx| this.open_row_menu(menu.clone(), None, event.position(), cx))),
                            Box::new(cx.listener(move |this, event: &MouseDownEvent, _, cx| this.open_row_menu(right.clone(), None, event.position, cx))),
                        )),
                        close: None,
                    },
                )
            }
            TreeRow::Worktree { project_id, path, label, git, folded, state, sessions, pinned, current, pr } => {
                let key = sidebar::worktree_key(&project_id, &path);
                let menu_open = self.row_menu.as_ref().is_some_and(|m| m.worktree.as_deref() == Some(path.as_path()));
                let (fold, add_key) = (key.clone(), key.clone());
                let (add_project, add_path) = (project_id.clone(), path.clone());
                let (menu_project, menu_path) = (project_id.clone(), path.clone());
                let (right_project, right_path) = (project_id, path);
                let row = sidebar::worktree_row(
                    WorktreeLine { key, label, git, folded, state, sessions, pinned, current, pr, menu_open },
                    RowActions {
                        click: Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| this.fold(fold.clone(), !folded, cx))),
                        add: Some(Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            // Opened, so that the new session is not hidden under the fold.
                            if folded {
                                this.fold(add_key.clone(), false, cx);
                            }
                            this.open_new_session(Some(add_project.clone()), Some(add_path.clone()), window, cx)
                        }))),
                        menu: Some((
                            Box::new(cx.listener(move |this, event: &ClickEvent, _, cx| this.open_row_menu(menu_project.clone(), Some(menu_path.clone()), event.position(), cx))),
                            Box::new(cx.listener(move |this, event: &MouseDownEvent, _, cx| this.open_row_menu(right_project.clone(), Some(right_path.clone()), event.position, cx))),
                        )),
                        close: None,
                    },
                );
                sidebar::indent(1, row)
            }
            TreeRow::Session { id, project_id, worktree, agent, title, state, running, selected } => {
                let detail = self.state.session(&id).and_then(|s| self.account_note(s));
                let (open_id, close_id) = (id.clone(), id.clone());
                let (menu_id, menu_project, menu_path) = (id.clone(), project_id.clone(), worktree.clone());
                let (right_id, right_project, right_path) = (id.clone(), project_id, worktree);
                let row = sidebar::session_row(
                    SessionLine { key: id, agent, running, title, detail, pr: None, state, stopped: false, selected },
                    RowActions {
                        // The person asked for it: the Sessions view shows it (`select_session` is for
                        // background events).
                        click: Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_session(&open_id, window, cx))),
                        add: None,
                        menu: Some((
                            Box::new(cx.listener(move |this, event: &ClickEvent, _, cx| this.open_session_menu(&menu_id, &menu_project, &menu_path, event.position(), cx))),
                            Box::new(cx.listener(move |this, event: &MouseDownEvent, _, cx| this.open_session_menu(&right_id, &right_project, &right_path, event.position, cx))),
                        )),
                        close: Some(Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| this.close_session(&close_id, window, cx)))),
                    },
                );
                sidebar::indent(2, row)
            }
            TreeRow::Removing { path, label } => sidebar::indent(1, sidebar::removing_row(&path.to_string_lossy(), label)),
            TreeRow::MoreWorktrees { project_id, count, open } => {
                let toggle = project_id.clone();
                sidebar::indent(
                    1,
                    sidebar::more_row(
                        &project_id,
                        count,
                        open,
                        Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if !this.idle_worktrees_open.remove(&toggle) {
                                this.idle_worktrees_open.insert(toggle.clone());
                            }
                            cx.notify();
                        })),
                    ),
                )
            }
            TreeRow::IdleWorktree { project_id, path, label, detached } => {
                let menu_open = self.row_menu.as_ref().is_some_and(|m| m.worktree.as_deref() == Some(path.as_path()));
                let key = path.to_string_lossy().into_owned();
                let (new_project, new_path) = (project_id.clone(), path.clone());
                let (menu_project, menu_path) = (project_id.clone(), path.clone());
                let (right_project, right_path) = (project_id, path);
                let row = sidebar::idle_row(
                    &key,
                    label,
                    detached,
                    menu_open,
                    RowActions {
                        click: Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_new_session(Some(new_project.clone()), Some(new_path.clone()), window, cx))),
                        add: None,
                        menu: Some((
                            Box::new(cx.listener(move |this, event: &ClickEvent, _, cx| this.open_row_menu(menu_project.clone(), Some(menu_path.clone()), event.position(), cx))),
                            Box::new(cx.listener(move |this, event: &MouseDownEvent, _, cx| this.open_row_menu(right_project.clone(), Some(right_path.clone()), event.position, cx))),
                        )),
                        close: None,
                    },
                );
                // Under "N more worktrees without a session", which folds them.
                sidebar::indent(2, row)
            }
        }
    }

    /// Fold a project or a worktree of the tree, or open it, and keep it so over a restart.
    fn fold(&mut self, key: String, fold: bool, cx: &mut Context<Self>) {
        sidebar::set_folded(&mut self.state.layout.folded, &key, fold);
        // A project folded by the build before the tree opens for good once opened here.
        if !fold {
            if let Some(project) = self.state.projects.iter_mut().find(|p| p.id == key) {
                project.expanded = true;
            }
        }
        // Keys of removed projects go; a switched-off project keeps its folds.
        let projects: Vec<&str> = self.state.projects.iter().map(|p| p.id.as_str()).collect();
        sidebar::prune_folded(&mut self.state.layout.folded, &projects);
        self.save();
        cx.notify();
    }

    /// The worktree's menu, from one of its sessions: its "Rename…" renames that session.
    fn open_session_menu(&mut self, id: &str, project_id: &str, worktree: &Path, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        self.last_in_worktree.insert(worktree.to_path_buf(), id.to_string());
        self.open_row_menu(project_id.to_string(), Some(worktree.to_path_buf()), position, cx);
    }

    /// The pull request of a session's branch, and how its checks went.
    fn pr_state(&self, session_id: &str) -> Option<(u64, PrState)> {
        let status = self.prs.map.get(session_id)?;
        let pr = status.pr.as_ref()?;
        let state = match status.ci.as_ref() {
            Some(ci) if !ci.failed().is_empty() => PrState::Failing,
            Some(ci) if ci.state == "OPEN" => PrState::Open,
            _ => PrState::Other,
        };
        Some((pr.number, state))
    }

    /// The width the left sidebar shows at (narrower in a narrow window, `shell::fit_panels`).
    pub fn sidebar_width(&self) -> f32 {
        self.shown_width(Side::Left)
    }

    pub fn right_width(&self) -> f32 {
        self.shown_width(Side::Right)
    }

    /// The width a sidebar has as the person left it.
    pub(crate) fn saved_width(&self, side: Side) -> f32 {
        match side {
            Side::Left => self.state.layout.sidebar.unwrap_or(SIDEBAR_WIDTH),
            Side::Right => self.state.layout.right.unwrap_or(RIGHT_WIDTH),
            Side::Tickets => self.state.layout.tickets.unwrap_or(crate::tickets_view::READER_WIDTH),
        }
    }

    pub(crate) fn row_state(&self, session: &Session) -> RowState {
        if self.attention.contains(&session.id) || !self.control.pending_for(&session.id).is_empty() {
            return RowState::NeedsYou;
        }
        match self.activity.get(&session.id) {
            Some(AgentActivity::NeedsInput) => RowState::NeedsYou,
            Some(AgentActivity::Working) => RowState::Working,
            _ => RowState::Idle,
        }
    }

    /// The account a session uses, when it is not its agent's default.
    fn account_note(&self, s: &Session) -> Option<String> {
        let default = self.state.defaults.for_agent(s.agent).map(String::as_str);
        s.account_id.as_deref().filter(|id| Some(*id) != default).and_then(|id| self.state.account(id)).map(|a| a.label.clone())
    }

    fn agent_label(&self, s: &Session) -> String {
        match self.account_note(s) {
            Some(account) => format!("{} · {account}", s.agent.name()),
            None => s.agent.name().to_string(),
        }
    }

    /// Pin a worktree to the top of its project, or unpin it. Newly pinned goes to the bottom of
    /// the pinned group (the ones already pinned keep their places above it).
    pub fn toggle_pin(&mut self, worktree: PathBuf, cx: &mut Context<Self>) {
        if let Some(i) = self.state.pinned.iter().position(|p| *p == worktree) {
            self.state.pinned.remove(i);
        } else {
            self.state.pinned.push(worktree);
        }
        self.save();
        cx.notify();
    }

    /// The sessions that need the person and those at work, by "Workspace" over the tree (a
    /// switched-off project is not in the sidebar, so neither are its sessions).
    pub fn sidebar_counts(&self) -> (usize, usize) {
        let states: Vec<RowState> = self.state.visible_sessions().map(|s| self.row_state(s)).collect();
        (states.iter().filter(|s| **s == RowState::NeedsYou).count(), states.iter().filter(|s| **s == RowState::Working).count())
    }

    // ----- In this worktree ---------------------------------------------------------------

    /// Tabs over the terminal for the sessions that share this worktree, and + Add.
    pub fn worktree_tabs(&self, session: &Session, cx: &mut Context<Self>) -> Option<AnyElement> {
        let siblings: Vec<&Session> = self.state.sessions.iter().filter(|s| s.project_id == session.project_id && s.worktree == session.worktree).collect();
        if siblings.len() < 2 {
            return None;
        }
        let mut tabs: Vec<AnyElement> = Vec::new();
        for s in siblings {
            let on = s.id == session.id;
            let id = s.id.clone();
            let state = self.row_state(s);
            tabs.push(
                div()
                    .id(SharedString::from(format!("wt-tab-{}", s.id)))
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(theme::sz(30.))
                    .px_2p5()
                    .rounded_md()
                    .border_1()
                    .border_color(if on { theme::border_strong() } else { theme::border() })
                    .when(on, |d| d.bg(theme::selected()))
                    .hover(|d| d.bg(theme::hover()))
                    .cursor_pointer()
                    .text_size(theme::sz(12.5))
                    .text_color(if on { theme::text_strong() } else { theme::text_soft() })
                    .when(on, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_session(&id, window, cx)))
                    .child(agent_icon(s.agent, 18., self.terminals.contains_key(&s.id)))
                    .child(self.agent_label(s))
                    .children(status_icon(state, &format!("tab-{}", s.id)))
                    .into_any_element(),
            );
        }
        let (project, worktree) = (session.project_id.clone(), session.worktree.clone());
        Some(
            div()
                .flex()
                .flex_none()
                .flex_wrap()
                .items_center()
                .gap_1p5()
                .px_3p5()
                .py_1p5()
                .bg(theme::header())
                .border_b_1()
                .border_color(theme::border())
                .child(div().text_size(theme::sz(11.5)).text_color(theme::muted()).mr_1().child("In this worktree"))
                .children(tabs)
                .child(
                    div()
                        .id("wt-tab-add")
                        .flex()
                        .items_center()
                        .h(theme::sz(30.))
                        .px_2p5()
                        .rounded_md()
                        .border_1()
                        .border_dashed()
                        .border_color(theme::border_strong())
                        .cursor_pointer()
                        .hover(|d| d.bg(theme::hover()))
                        .text_size(theme::sz(12.5))
                        .text_color(theme::muted())
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_new_session(Some(project.clone()), Some(worktree.clone()), window, cx)))
                        .child("+ Add"),
                )
                .into_any_element(),
        )
    }

    // ----- sidebar widths -----------------------------------------------------------------

    /// The draggable edge of a sidebar (double-click: back to its usual width). Dragged far in,
    /// the sidebar hides; a hidden sidebar keeps its edge, to drag it out again.
    pub fn resize_handle(&self, side: Side, cx: &mut Context<Self>) -> AnyElement {
        let dragging = self.resizing.is_some_and(|(s, _, _)| s == side);
        let focus = self.edge_focus[side as usize].clone();
        div()
            .id(SharedString::from(format!("resize-{side:?}")))
            .flex_none()
            .w(theme::sz(5.))
            .h_full()
            .mx(theme::sz(-2.))
            .cursor_col_resize()
            .when(dragging, |d| d.bg(theme::border_strong()))
            .hover(|d| d.bg(theme::border_strong()))
            // A click focuses the edge; then ← and → move it 16px, Esc goes back to the session.
            .track_focus(&focus)
            .focus(|d| d.bg(theme::border_strong()))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let step = match event.keystroke.key.as_str() {
                    "left" => -16.,
                    "right" => 16.,
                    "escape" => {
                        this.focus_active(window, cx);
                        return;
                    }
                    _ => return,
                };
                this.resize_by(side, step, cx);
                cx.stop_propagation();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let width = match side {
                        _ if this.side_hidden(side) => 0.,
                        Side::Left => this.sidebar_width(),
                        Side::Right => this.right_width(),
                        Side::Tickets => this.tickets_width(),
                    };
                    this.resizing = Some((side, f32::from(event.position.x), width));
                    // Not to the elements under the edge, nor (a drag) its own focus.
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.click_count() >= 2 {
                    match side {
                        Side::Left => this.state.layout.sidebar = None,
                        Side::Right => this.state.layout.right = None,
                        Side::Tickets => this.state.layout.tickets = None,
                    }
                    this.set_hidden(side, false);
                    this.save();
                    cx.notify();
                } else {
                    // The press above stops the focus that a press gives, so a click gives it.
                    window.focus(&focus, cx);
                }
            }))
            .into_any_element()
    }

    /// While an edge is dragged: follow the mouse (within limits); let go to keep it.
    pub fn resize_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((side, start, width)) = self.resizing else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.resize_end(cx);
            return;
        }
        let dx = f32::from(event.position.x) - start;
        self.set_edge(side, width, dx);
        cx.notify();
    }

    /// Move an edge `dx` to the right from where it is (the arrow keys), and keep it. A hidden
    /// sidebar comes back at its width.
    fn resize_by(&mut self, side: Side, dx: f32, cx: &mut Context<Self>) {
        let width = match side {
            Side::Left => self.sidebar_width(),
            Side::Right => self.right_width(),
            Side::Tickets => self.tickets_width(),
        };
        if self.side_hidden(side) {
            self.set_hidden(side, false);
        } else {
            self.set_edge(side, width, dx);
        }
        self.save();
        cx.notify();
    }

    fn side_hidden(&self, side: Side) -> bool {
        match side {
            Side::Left => self.state.layout.left_hidden,
            Side::Right | Side::Tickets => !self.show_right,
        }
    }

    /// `width` is the width when the drag began (0 for a hidden sidebar). Dragged far in, the
    /// sidebar hides and keeps that width, so it comes back as it was, not at the narrowest
    /// width the drag passed on its way.
    fn set_edge(&mut self, side: Side, width: f32, dx: f32) {
        // The drag is in pixels on the screen; the layout keeps the widths at 100 %
        // (`shell::shown_width` shows them at the scale).
        let scale = theme::scale();
        let (width, dx) = (width / scale, dx / scale);
        let new = edge_width(side, width, dx);
        self.set_hidden(side, new.is_none());
        let Some(width) = new.or((width > 0.).then(|| width.round())) else { return };
        match side {
            Side::Left => self.state.layout.sidebar = Some(width),
            Side::Right => self.state.layout.right = Some(width),
            Side::Tickets => self.state.layout.tickets = Some(width),
        }
    }

    pub fn resize_end(&mut self, cx: &mut Context<Self>) {
        if self.resizing.take().is_some() {
            self.save();
            cx.notify();
        }
    }
}

/// The width a sidebar `width` wide gets when its edge moves `dx` to the right, within its
/// limits; None when that leaves it narrower than `shell::HIDE_BELOW`, so it hides. The left
/// sidebar grows as its edge moves right; the panels on the right as it moves left.
fn edge_width(side: Side, width: f32, dx: f32) -> Option<f32> {
    let wanted = if side == Side::Left { width + dx } else { width - dx };
    if wanted < crate::shell::HIDE_BELOW {
        return None;
    }
    let (min, max) = edge_limits(side);
    Some(wanted.clamp(min, max).round())
}

/// The narrowest and the widest a sidebar is dragged to.
pub(crate) fn edge_limits(side: Side) -> (f32, f32) {
    match side {
        Side::Left => (220., 560.),
        Side::Right => (300., 760.),
        Side::Tickets => (400., 900.),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dragged_edge_keeps_its_limits_and_hides_when_far_in() {
        assert_eq!(edge_width(Side::Left, 300., 40.), Some(340.));
        assert_eq!(edge_width(Side::Left, 300., -120.), Some(220.), "narrow, but not hidden yet");
        assert_eq!(edge_width(Side::Left, 300., -161.), None, "below 140 px it hides");
        assert_eq!(edge_width(Side::Left, 300., 900.), Some(560.));
        // A hidden sidebar starts its drag at 0 px.
        assert_eq!(edge_width(Side::Left, 0., 100.), None);
        assert_eq!(edge_width(Side::Left, 0., 150.), Some(220.));
        assert_eq!(edge_width(Side::Right, 420., 60.), Some(360.), "the right panel grows to the left");
        assert_eq!(edge_width(Side::Right, 0., -150.), Some(300.));
        assert_eq!(edge_width(Side::Right, 420., 300.), None);
        assert_eq!(edge_width(Side::Tickets, 540., -1000.), Some(900.));
    }
}
