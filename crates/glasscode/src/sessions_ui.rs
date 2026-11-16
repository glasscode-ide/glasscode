//! How sessions show and behave around the terminal: their names (chosen by the person, or
//! from their pull request, Claude's own title or their first message), placeholder branches
//! renamed to match before they are pushed, the sidebar rows (one per worktree), the
//! "In this worktree" tabs, telling the person when a session is done or needs them, and the
//! sidebars' widths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{AnyElement, ClickEvent, Context, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, SharedString, div, prelude::*, px};
use glasscode_core::agents::AgentActivity;
use glasscode_core::model::{AgentKind, Session};
use glasscode_core::{git, naming};

use crate::center::Center;
use crate::sidebar::{Kid, Row, RowState, agent_icon, session_row, status_icon, without_number};
use crate::theme;
use crate::ui;
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

    pub(crate) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut projects: Vec<AnyElement> = Vec::new();
        for project in &self.state.projects {
            // Projects switched off in Settings › Projects drop out of the sidebar entirely.
            if self.state.is_hidden(&project.id) {
                continue;
            }
            let project_id = project.id.clone();
            let count = self
                .state
                .sessions
                .iter()
                .filter(|s| s.project_id == project.id)
                .count();
            let is_current = self
                .active_session()
                .is_some_and(|s| s.project_id == project.id);
            let toggle_id = project_id.clone();
            let add_id = project_id.clone();
            let header = div()
                .flex()
                .items_center()
                .gap_0p5()
                .child(
                    div()
                        .id(SharedString::from(format!("project-{}", project.id)))
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_1p5()
                        .h(px(30.))
                        .px_1p5()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .text_color(if is_current { theme::text_strong() } else { theme::text_soft() })
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(p) = this.state.projects.iter_mut().find(|p| p.id == toggle_id) {
                                p.expanded = !p.expanded;
                            }
                            this.save();
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_color(theme::faint())
                                .text_size(px(10.))
                                .w(px(10.))
                                .child(if project.expanded { "▾" } else { "▸" }),
                        )
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(project.name.clone()))
                        .child(div().flex_1())
                        // The GitHub account this project's sessions use; a click opens Projects & Git.
                        .when_some(self.github_account_for(&project.id), |d, login| {
                            d.child(
                                div()
                                    .id(SharedString::from(format!("project-gh-{}", project.id)))
                                    .flex_none()
                                    .max_w(px(110.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .h(px(20.))
                                    .px_1p5()
                                    .flex()
                                    .items_center()
                                    .rounded(px(10.))
                                    .border_1()
                                    .border_color(theme::border())
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(theme::muted())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::selected()).text_color(theme::text_strong()))
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.open_settings(crate::settings_ui::Section::Projects, window, cx);
                                    }))
                                    .child(format!("@{login}")),
                            )
                        })
                        .when(count > 0, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(theme::muted())
                                    .child(count.to_string()),
                            )
                        }),
                )
                .child({
                    let menu_id = project_id.clone();
                    ui::icon_button(SharedString::from(format!("project-menu-{}", project.id)), "⋯").on_click(cx.listener(
                        move |this, event: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_row_menu(menu_id.clone(), None, event.position(), cx);
                        },
                    ))
                })
                .child(
                    ui::icon_button(SharedString::from(format!("project-add-{}", project.id)), "+")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_new_session(Some(add_id.clone()), None, window, cx)
                        })),
                );

            let mut body: Vec<AnyElement> = Vec::new();
            let repo_name = project.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if project.expanded {
                body.extend(self.project_rows(&project.id, &repo_name, cx));
                body.extend(self.idle_worktree_rows(&project.id, &repo_name, cx));
            }
            projects.push(
                div()
                    .mb_1p5()
                    .child(header)
                    .children(body)
                    .into_any_element(),
            );
        }

        // Below the rows of the views, in the left sidebar (`shell::render_left`), which has the
        // width, the panel colour and the edge.
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3p5()
                    .pt_3()
                    .pb_1p5()
                    .child(ui::caps_label("PROJECTS"))
                    .child({
                        let (needs, working) = self.sidebar_counts();
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .text_size(px(11.))
                            .text_color(theme::muted())
                            .child(div().flex().items_center().gap_1().child(div().size(px(7.)).rounded_full().bg(crate::sidebar::attention())).child(format!("{needs} need you")))
                            .child(div().flex().items_center().gap_1().child(div().size(px(7.)).rounded_full().bg(theme::added())).child(format!("{working} working")))
                    }),
            )
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_1p5()
                    .pb_2()
                    .children(projects)
                    .when(self.state.visible_projects().next().is_none(), |d| {
                        d.child(
                            div()
                                .p_3()
                                .text_color(theme::muted())
                                .child(if self.state.projects.is_empty() {
                                    "No projects yet. Add a git repository to start."
                                } else {
                                    "All projects are switched off in Settings › Projects."
                                }),
                        )
                    })
                    .when_some(self.render_server_sidebar(cx), |d, server| d.child(server)),
            )
            .child(
                div().p_2().border_t_1().border_color(theme::border()).child(
                    div()
                        .id("add-project")
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(px(32.))
                        .px_2()
                        .rounded_md()
                        .cursor_pointer()
                        .text_color(theme::text_soft())
                        .hover(|s| s.bg(theme::hover()))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.prompt_add_project(window, cx)
                        }))
                        .child("+   Add project…"),
                ),
            )
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

    fn agent_label(&self, s: &Session) -> String {
        let default = self.state.defaults.for_agent(s.agent).map(String::as_str);
        match s.account_id.as_deref().filter(|id| Some(*id) != default).and_then(|id| self.state.account(id)) {
            Some(account) => format!("{} · {}", s.agent.name(), account.label),
            None => s.agent.name().to_string(),
        }
    }

    /// One row per worktree that has sessions. Order: pinned worktrees first (in pin order),
    /// then the rest newest-session-first — so a session or review you just started lands at
    /// the top, below any you've pinned, instead of wherever git happens to list it.
    pub fn project_rows(&self, project_id: &str, repo_name: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        // (worktree, newest session created_at) for every worktree that has sessions.
        let mut rows: Vec<(PathBuf, u64)> = Vec::new();
        for (path, _) in self.project_worktrees(project_id) {
            if self.removing.contains(&path) {
                continue;
            }
            let newest = self
                .state
                .sessions
                .iter()
                .filter(|s| s.project_id == project_id && s.worktree == path)
                .map(|s| s.created_at)
                .max();
            if let Some(newest) = newest {
                rows.push((path, newest));
            }
        }
        // Stable sort, so worktrees that tie keep git's order underneath.
        rows.sort_by(|a, b| match (self.state.pin_index(&a.0), self.state.pin_index(&b.0)) {
            (Some(x), Some(y)) => x.cmp(&y),              // both pinned: by pin order
            (Some(_), None) => std::cmp::Ordering::Less,  // pinned above unpinned
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.1.cmp(&a.1),                // newest first
        });
        let mut out = Vec::new();
        for (path, _) in rows {
            let sessions: Vec<&Session> = self.state.sessions.iter().filter(|s| s.project_id == project_id && s.worktree == path).collect();
            if sessions.is_empty() {
                continue;
            }
            out.push(self.worktree_row(project_id, &path, &sessions, repo_name, cx));
        }
        out
    }

    /// Pin a worktree to the top of the sidebar, or unpin it. Newly pinned goes to the
    /// bottom of the pinned group (the ones already pinned keep their places above it).
    pub fn toggle_pin(&mut self, worktree: PathBuf, cx: &mut Context<Self>) {
        if let Some(i) = self.state.pinned.iter().position(|p| *p == worktree) {
            self.state.pinned.remove(i);
        } else {
            self.state.pinned.push(worktree);
        }
        self.save();
        cx.notify();
    }

    fn worktree_row(&self, project_id: &str, path: &Path, sessions: &[&Session], repo_name: &str, cx: &mut Context<Self>) -> AnyElement {
        // The work is named after its agent session (a shell beside it changes nothing).
        let main = sessions.iter().find(|s| s.agent != AgentKind::Shell).copied().unwrap_or(sessions[0]);
        let state = sessions.iter().map(|s| self.row_state(s)).min().unwrap_or(RowState::Idle);
        let active = self.state.active_session.clone();
        let here = self.center == Center::Session && sessions.iter().any(|s| active.as_deref() == Some(s.id.as_str()));
        let default = self.state.defaults.for_agent(main.agent).map(String::as_str);
        let account = main.account_id.as_deref().filter(|id| Some(*id) != default).and_then(|id| self.state.account(id)).map(|a| a.label.clone());
        let sub = {
            if !self.is_git_project(project_id) {
                "Folder · no git".to_string()
            } else {
                let branch = if main.branch.is_empty() || main.branch == "detached" { git::folder_label(path, repo_name) } else { main.branch.clone() };
                format!("⎇ {branch}")
            }
        };
        let sub = match account {
            Some(a) => format!("{sub} · {a}"),
            _ => sub,
        };
        let pr = self.prs.map.get(&main.id).and_then(|st| {
            let pr = st.pr.as_ref()?;
            let color = match st.ci.as_ref().map(|ci| ci.state.as_str()) {
                _ if st.ci.as_ref().is_some_and(|ci| !ci.failed().is_empty()) => theme::removed(),
                Some("OPEN") => theme::added(),
                _ => theme::muted(),
            };
            Some((format!("#{}", pr.number), color))
        });
        let open_id = self.last_in_worktree.get(path).filter(|id| sessions.iter().any(|s| &s.id == *id)).cloned().unwrap_or_else(|| main.id.clone());
        let kids: Vec<Kid> = if here && sessions.len() > 1 {
            sessions
                .iter()
                .map(|s| {
                    let id = s.id.clone();
                    Kid {
                        key: s.id.clone(),
                        agent: s.agent,
                        label: self.agent_label(s),
                        state: self.row_state(s),
                        selected: active.as_deref() == Some(s.id.as_str()),
                        running: self.terminals.contains_key(&s.id),
                        on_click: Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| this.select_session(&id, window, cx))),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let row = Row {
            key: main.id.clone(),
            title: self.session_title(main),
            agent: main.agent,
            running: sessions.iter().any(|s| self.terminals.contains_key(&s.id)),
            selected: here && sessions.len() == 1,
            state,
            sub,
            tag: None,
            pr,
            count: sessions.len(),
            pinned: self.state.is_pinned(path),
            kids,
        };
        let close = (sessions.len() == 1).then(|| {
            let id = main.id.clone();
            Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| this.close_session(&id, window, cx))) as crate::sidebar::OnClick
        });
        let (menu_project, menu_path) = (project_id.to_string(), path.to_path_buf());
        let (right_project, right_path) = (project_id.to_string(), path.to_path_buf());
        let body = session_row(
            row,
            Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| this.select_session(&open_id, window, cx))),
            close,
            Some((
                Box::new(cx.listener(move |this, event: &ClickEvent, _, cx| this.open_row_menu(menu_project.clone(), Some(menu_path.clone()), event.position(), cx))),
                Box::new(cx.listener(move |this, event: &MouseDownEvent, _, cx| this.open_row_menu(right_project.clone(), Some(right_path.clone()), event.position, cx))),
            )),
        );
        // A selected worktree with several sessions shows a little gap under its list.
        div().when(here && sessions.len() > 1, |d| d.bg(theme::subtle()).rounded_md().pb_1()).child(body).into_any_element()
    }

    /// Worktrees nobody works in: those being removed always show; the others fold away.
    pub fn idle_worktree_rows(&self, project_id: &str, repo_name: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let empty: Vec<(PathBuf, String)> = self
            .project_worktrees(project_id)
            .into_iter()
            .filter(|(path, _)| self.removing.contains(path) || !self.state.sessions.iter().any(|s| s.project_id == project_id && &s.worktree == path))
            .collect();
        let mut out = Vec::new();
        for (path, branch) in empty.iter().filter(|(p, _)| self.removing.contains(p)) {
            let label = if branch == "detached" { git::folder_label(path, repo_name) } else { branch.clone() };
            out.push(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .h(px(28.))
                    .pl(px(40.))
                    .pr_2()
                    .opacity(0.55)
                    .text_size(px(11.5))
                    .text_color(theme::text_soft())
                    .child(div().text_color(theme::faint()).child("⎇"))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().line_through().child(label))
                    .child(div().flex_none().text_size(px(10.5)).text_color(theme::muted()).child("Removing…"))
                    .into_any_element(),
            );
        }
        let idle: Vec<&(PathBuf, String)> = empty.iter().filter(|(p, _)| !self.removing.contains(p)).collect();
        if idle.is_empty() {
            return out;
        }
        let open = self.idle_worktrees_open.contains(project_id);
        let fold_id = project_id.to_string();
        out.push(
            div()
                .id(SharedString::from(format!("idle-wt-{project_id}")))
                .flex()
                .items_center()
                .gap_1p5()
                .h(px(28.))
                .pl(px(12.))
                .pr_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .text_size(px(11.5))
                .text_color(theme::muted())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if !this.idle_worktrees_open.remove(&fold_id) {
                        this.idle_worktrees_open.insert(fold_id.clone());
                    }
                    cx.notify();
                }))
                .child(div().w(px(10.)).text_size(px(10.)).text_color(theme::faint()).child(if open { "▾" } else { "▸" }))
                .child(format!("{} more {} without a session", idle.len(), if idle.len() == 1 { "worktree" } else { "worktrees" }))
                .into_any_element(),
        );
        if open {
            for (index, (path, branch)) in idle.into_iter().enumerate() {
                out.push(self.idle_worktree_row(project_id, path, branch, repo_name, index, cx));
            }
        }
        out
    }

    fn idle_worktree_row(&self, project_id: &str, path: &Path, branch: &str, repo_name: &str, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let detached = branch == "detached";
        let label = if detached { git::folder_label(path, repo_name) } else { branch.to_string() };
        let group = SharedString::from(format!("wt-row-{project_id}-{index}"));
        let menu_open = self.row_menu.as_ref().is_some_and(|m| m.worktree.as_deref() == Some(path));
        let (menu_project, menu_path) = (project_id.to_string(), path.to_path_buf());
        let (right_project, right_path) = (project_id.to_string(), path.to_path_buf());
        let (wt_project, wt_path) = (project_id.to_string(), path.to_path_buf());
        div()
            .id(SharedString::from(format!("wt-{}", path.display())))
            .group(group.clone())
            .flex()
            .items_center()
            .gap_1p5()
            .h(px(28.))
            .pl(px(40.))
            .pr_1()
            .rounded_md()
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .text_size(px(11.5))
            .text_color(theme::muted())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_new_session(Some(wt_project.clone()), Some(wt_path.clone()), window, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, _, cx| this.open_row_menu(right_project.clone(), Some(right_path.clone()), event.position, cx)))
            .child(div().text_color(theme::faint()).child("⎇"))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
            .when(detached, |d| d.child(div().flex_none().text_size(px(10.5)).text_color(theme::faint()).child("detached")))
            .child(
                crate::ui::icon_button(SharedString::from(format!("wt-menu-{}", path.display())), "⋯")
                    .size(px(20.))
                    .when(!menu_open, |b| b.invisible().group_hover(group.clone(), |s| s.visible()))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.open_row_menu(menu_project.clone(), Some(menu_path.clone()), event.position(), cx);
                    })),
            )
            .into_any_element()
    }

    /// "3 need you · 7 working" over the projects.
    pub fn sidebar_counts(&self) -> (usize, usize) {
        let states: Vec<RowState> = self.state.sessions.iter().map(|s| self.row_state(s)).collect();
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
                    .h(px(30.))
                    .px_2p5()
                    .rounded_md()
                    .border_1()
                    .border_color(if on { theme::border_strong() } else { theme::border() })
                    .when(on, |d| d.bg(theme::selected()))
                    .hover(|d| d.bg(theme::hover()))
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .text_color(if on { theme::text_strong() } else { theme::text_soft() })
                    .when(on, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.select_session(&id, window, cx)))
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
                .child(div().text_size(px(11.5)).text_color(theme::muted()).mr_1().child("In this worktree"))
                .children(tabs)
                .child(
                    div()
                        .id("wt-tab-add")
                        .flex()
                        .items_center()
                        .h(px(30.))
                        .px_2p5()
                        .rounded_md()
                        .border_1()
                        .border_dashed()
                        .border_color(theme::border_strong())
                        .cursor_pointer()
                        .hover(|d| d.bg(theme::hover()))
                        .text_size(px(12.5))
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
        div()
            .id(SharedString::from(format!("resize-{side:?}")))
            .flex_none()
            .w(px(5.))
            .h_full()
            .mx(px(-2.))
            .cursor_col_resize()
            .when(dragging, |d| d.bg(theme::border_strong()))
            .hover(|d| d.bg(theme::border_strong()))
            // A click focuses the edge; then ← and → move it 16px, Esc goes back to the session.
            .focusable()
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
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if event.click_count() >= 2 {
                    match side {
                        Side::Left => this.state.layout.sidebar = None,
                        Side::Right => this.state.layout.right = None,
                        Side::Tickets => this.state.layout.tickets = None,
                    }
                    this.set_hidden(side, false);
                    this.save();
                    cx.notify();
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

    fn set_edge(&mut self, side: Side, width: f32, dx: f32) {
        let width = edge_width(side, width, dx);
        self.set_hidden(side, width.is_none());
        let Some(width) = width else { return };
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
