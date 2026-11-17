//! Removing worktrees: the ⋯ menu on sidebar rows, and the dialog that removes one worktree
//! or cleans up many. Every worktree is checked first, so nothing unsaved goes silently.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, ClickEvent, ClipboardItem, Context, FontWeight, Pixels, Point, SharedString, Window, anchored,
    deferred, div, prelude::*, px,
};
use glasscode_core::git::{self, Worktree, WorktreeCheck};

use crate::dialogs::Modal;
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

/// The open ⋯ menu of a worktree row (`worktree: Some`) or a project header (`None`).
pub struct RowMenu {
    pub project_id: String,
    pub worktree: Option<PathBuf>,
    pub position: Point<Pixels>,
}

pub struct RemoveForm {
    pub project_id: String,
    /// One worktree from its ⋯ menu, or the clean-up list of all of them.
    pub single: bool,
    pub items: Vec<RemoveItem>,
    pub delete_branches: bool,
    /// The developer changed the branch box, so loading the checks no longer sets it.
    pub branch_touched: bool,
    /// Never offered for deletion: the default branch and the main checkout's branch.
    pub protected_branches: Vec<String>,
    pub busy: bool,
}

pub struct RemoveItem {
    pub worktree: Worktree,
    pub label: String,
    pub check: Option<WorktreeCheck>,
    pub selected: bool,
    /// Glasscode sessions running in this worktree (closed before it is removed).
    pub sessions: usize,
    /// Other running programs whose working folder is inside it (agents, dev servers).
    pub programs: Vec<String>,
    pub result: Option<Result<String, String>>,
}

impl RemoveItem {
    /// Removing it loses uncommitted work or commits, or git needs a force (lock). Unknown
    /// until checked.
    pub fn risky(&self) -> bool {
        self.worktree.locked || self.check.as_ref().is_none_or(|c| !c.is_clean())
    }

    /// Picked by "Select clean": nothing to lose, no local config files, nobody working in it.
    fn clean(&self) -> bool {
        !self.risky()
            && self.sessions == 0
            && self.programs.is_empty()
            && self.check.as_ref().is_some_and(|c| c.env_files.is_empty())
    }

    fn removed(&self) -> bool {
        matches!(self.result, Some(Ok(_)))
    }
}

fn home_relative(path: &Path) -> String {
    let text = path.display().to_string();
    match std::env::var("HOME").ok() {
        Some(home) if text.starts_with(&home) => format!("~{}", &text[home.len()..]),
        _ => text,
    }
}

fn ago(time: Option<i64>) -> String {
    let Some(time) = time else { return String::new() };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(time);
    let days = (now - time).max(0) / 86_400;
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=13 => format!("{days} days ago"),
        14..=60 => format!("{} weeks ago", days / 7),
        _ => format!("{} months ago", days / 30),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Short status for a list row: text and color.
fn status_of(item: &RemoveItem) -> (String, gpui::Hsla) {
    if let Some(result) = &item.result {
        return match result {
            Ok(note) => (note.clone(), theme::added()),
            Err(err) => (format!("✕ {err}"), theme::removed()),
        };
    }
    if item.worktree.prunable {
        return ("folder already gone".into(), theme::muted());
    }
    let Some(check) = &item.check else { return ("checking…".into(), theme::faint()) };
    let mut parts = Vec::new();
    if check.changed > 0 {
        parts.push(plural(check.changed, "uncommitted file", "uncommitted files"));
    }
    if check.lost_commits > 0 {
        parts.push(plural(check.lost_commits, "commit only here", "commits only here"));
    }
    if item.worktree.locked {
        parts.push("locked".into());
    }
    if !parts.is_empty() {
        return (parts.join(" · "), theme::removed());
    }
    if item.sessions > 0 {
        return (plural(item.sessions, "open session", "open sessions"), theme::codex());
    }
    if !item.programs.is_empty() {
        return (format!("in use by {}", item.programs.join(", ")), theme::codex());
    }
    if !check.env_files.is_empty() {
        return ("has .env files".into(), theme::modified());
    }
    if check.branch_only_commits > 0 {
        return (format!("clean · {} not pushed", plural(check.branch_only_commits, "commit", "commits")), theme::modified());
    }
    ("clean".into(), theme::added())
}

fn with_form(ws: &mut Workspace, f: impl FnOnce(&mut RemoveForm)) {
    if let Some(Modal::RemoveWorktrees(form)) = ws.modal.as_mut() {
        f(form);
    }
}

impl Workspace {
    // ----- ⋯ menus ---------------------------------------------------------------------------

    pub fn open_row_menu(&mut self, project_id: String, worktree: Option<PathBuf>, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.row_menu = Some(RowMenu { project_id, worktree, position });
        cx.notify();
    }

    pub fn render_row_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.row_menu.as_ref()?;
        let project_id = menu.project_id.clone();
        let mut items: Vec<AnyElement> = Vec::new();
        let item = |id: &'static str, label: String, danger: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(28.))
                .px_2p5()
                .rounded_md()
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(if danger { theme::removed() } else { theme::text() })
                .hover(|s| s.bg(theme::hover()))
                .child(label)
        };
        match &menu.worktree {
            Some(path) => {
                let is_main = self
                    .worktrees
                    .get(&project_id)
                    .and_then(|list| list.iter().find(|w| &w.path == path))
                    .is_some_and(|w| w.is_main);
                let (p1, p2, p3, p4) = (path.clone(), path.clone(), path.clone(), path.clone());
                let (id1, id4) = (project_id.clone(), project_id.clone());
                // Rename the session this worktree opens to.
                let here: Vec<&glasscode_core::model::Session> = self.state.sessions.iter().filter(|s| &s.worktree == path).collect();
                let rename_id = self
                    .last_in_worktree
                    .get(path)
                    .filter(|id| here.iter().any(|s| &&s.id == id))
                    .cloned()
                    .or_else(|| here.iter().find(|s| s.agent != glasscode_core::model::AgentKind::Shell).or(here.first()).map(|s| s.id.clone()));
                if let Some(rename_id) = rename_id {
                    items.push(
                        item("menu-wt-rename", "Rename…".into(), false)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.row_menu = None;
                                this.open_rename(&rename_id, window, cx);
                            }))
                            .into_any_element(),
                    );
                }
                items.push(
                    item("menu-wt-new", "New session here".into(), false)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.row_menu = None;
                            this.open_new_session(Some(id1.clone()), Some(p1.clone()), window, cx);
                        }))
                        .into_any_element(),
                );
                let pinned = self.state.is_pinned(path);
                let pin_path = path.clone();
                items.push(
                    item("menu-wt-pin", if pinned { "Unpin from top".into() } else { "Pin to top".into() }, false)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.row_menu = None;
                            this.toggle_pin(pin_path.clone(), cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("menu-wt-finder", format!("Show in {}", crate::platform::file_manager()), false)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.row_menu = None;
                            if let Err(error) = glasscode_core::sys::open_path(&p2) {
                                log::warn!("could not open {} in the file manager: {error}", p2.display());
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                );
                items.push(
                    item("menu-wt-copy", "Copy path".into(), false)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.row_menu = None;
                            cx.write_to_clipboard(ClipboardItem::new_string(p3.display().to_string()));
                            this.show_toast("Path copied.", cx);
                        }))
                        .into_any_element(),
                );
                items.push(div().h(px(1.)).my_1().bg(theme::border()).into_any_element());
                if is_main {
                    items.push(
                        div()
                            .px_2p5()
                            .py_1p5()
                            .text_size(px(12.))
                            .text_color(theme::faint())
                            .child("The main checkout can't be removed")
                            .into_any_element(),
                    );
                } else {
                    items.push(
                        item("menu-wt-remove", "Remove worktree…".into(), true)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.row_menu = None;
                                this.open_remove_worktrees(id4.clone(), Some(p4.clone()), window, cx);
                            }))
                            .into_any_element(),
                    );
                }
            }
            None => {
                let count = self.worktrees.get(&project_id).map(|l| l.iter().filter(|w| !w.is_main).count()).unwrap_or(0);
                let root = self.state.project(&project_id).map(|p| p.root.clone());
                let (id1, id2) = (project_id.clone(), project_id.clone());
                items.push(
                    item("menu-project-new", "New session…".into(), false)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.row_menu = None;
                            this.open_new_session(Some(id1.clone()), None, window, cx);
                        }))
                        .into_any_element(),
                );
                if let Some(root) = root {
                    items.push(
                        item("menu-project-finder", format!("Show in {}", crate::platform::file_manager()), false)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.row_menu = None;
                                if let Err(error) = glasscode_core::sys::open_path(&root) {
                                    log::warn!("could not open {} in the file manager: {error}", root.display());
                                }
                                cx.notify();
                            }))
                            .into_any_element(),
                    );
                }
                items.extend(self.github_menu_items(&project_id, cx));
                if !self.is_git_project(&project_id) {
                    let git_id = project_id.clone();
                    items.push(
                        item("menu-project-git", "Set up git…".into(), false)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.row_menu = None;
                                this.set_up_git(&git_id, cx);
                            }))
                            .into_any_element(),
                    );
                }
                items.push(div().h(px(1.)).my_1().bg(theme::border()).into_any_element());
                items.push(
                    item("menu-project-cleanup", format!("Clean up worktrees… ({count})"), false)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.row_menu = None;
                            this.open_remove_worktrees(id2.clone(), None, window, cx);
                        }))
                        .into_any_element(),
                );
                let remove_id = project_id.clone();
                items.push(
                    item("menu-project-remove", "Remove project from Glasscode…".into(), true)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.row_menu = None;
                            this.confirm_remove_project(remove_id.clone(), window, cx);
                        }))
                        .into_any_element(),
                );
            }
        }
        Some(
            deferred(
                anchored().position(menu.position).snap_to_window_with_margin(px(8.)).child(
                    div()
                        .id("row-menu")
                        .occlude()
                        .w(px(230.))
                        .p_1()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::dialog())
                        .shadow_lg()
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.row_menu = None;
                            cx.notify();
                        }))
                        .children(items),
                ),
            )
            .with_priority(4)
            .into_any_element(),
        )
    }

    /// Developer aid (GLASSCODE_DEBUG_OPEN=wt-…): step 0 opens, step 1 acts.
    pub fn debug_worktrees(&mut self, mode: &str, step: u8, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_id) = self.active_session().map(|s| s.project_id.clone()).or_else(|| self.state.projects.first().map(|p| p.id.clone())) else { return };
        let (verb, suffix) = mode.split_once(':').unwrap_or((mode, ""));
        let path = self
            .worktrees
            .get(&project_id)
            .and_then(|l| l.iter().find(|w| !suffix.is_empty() && w.path.to_string_lossy().ends_with(suffix)))
            .map(|w| w.path.clone());
        match (verb, step) {
            ("wt-menu", 0) => self.open_row_menu(project_id, path, gpui::point(px(250.), px(200.)), cx),
            ("wt-project-menu", 0) => self.open_row_menu(project_id, None, gpui::point(px(250.), px(130.)), cx),
            ("wt-remove" | "wt-remove-go", 0) => self.open_remove_worktrees(project_id, path, window, cx),
            ("wt-cleanup" | "wt-cleanup-go", 0) => self.open_remove_worktrees(project_id, None, window, cx),
            ("wt-remove-go", 1) => self.remove_selected_worktrees(window, cx),
            ("wt-cleanup-go", 1) => {
                with_form(self, |form| {
                    for item in form.items.iter_mut() {
                        item.selected = item.clean();
                    }
                    form.delete_branches = true;
                });
                self.remove_selected_worktrees(window, cx);
            }
            _ => {}
        }
    }

    // ----- dialog ------------------------------------------------------------------------------

    /// Open the remove dialog for one worktree (`only`) or the clean-up list of a project.
    pub fn open_remove_worktrees(&mut self, project_id: String, only: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.state.project(&project_id).cloned() else { return };
        let repo_name = project.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let list = self.worktrees.get(&project_id).cloned().unwrap_or_default();
        let mut worktrees: Vec<Worktree> = list.iter().filter(|w| !w.is_main).cloned().collect();
        if let Some(path) = &only {
            worktrees.retain(|w| &w.path == path);
            if worktrees.is_empty() {
                // A session folder git no longer lists.
                worktrees.push(Worktree { path: path.clone(), branch: "detached".into(), prunable: !path.exists(), ..Default::default() });
            }
        }
        let items: Vec<RemoveItem> = worktrees
            .into_iter()
            .map(|wt| RemoveItem {
                label: wt.label(&repo_name),
                sessions: self.state.sessions.iter().filter(|s| s.worktree == wt.path).count(),
                worktree: wt,
                check: None,
                selected: only.is_some(),
                result: None,
                programs: Vec::new(),
            })
            .collect();
        let mut protected_branches = vec![git::default_branch(&project.root)];
        protected_branches.extend(list.iter().filter(|w| w.is_main).map(|w| w.branch.clone()));
        self.modal = Some(Modal::RemoveWorktrees(RemoveForm {
            project_id,
            single: only.is_some(),
            items,
            delete_branches: false,
            branch_touched: false,
            protected_branches,
            busy: false,
        }));
        self.load_worktree_checks(cx);
        window.focus(&self.focus_handle_ref().clone(), cx);
        cx.notify();
    }

    /// Check every worktree in the dialog on a few threads; rows update as results arrive
    /// (`git status` in a big repository can take seconds per worktree).
    fn load_worktree_checks(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::RemoveWorktrees(form)) = self.modal.as_ref() else { return };
        let queue: VecDeque<Worktree> = form.items.iter().map(|i| i.worktree.clone()).collect();
        let project_id = form.project_id.clone();
        let workers = queue.len().clamp(1, 8);
        let queue = Arc::new(Mutex::new(queue));
        let (tx, rx) = mpsc::channel::<(PathBuf, WorktreeCheck)>();
        for _ in 0..workers {
            let (queue, tx) = (queue.clone(), tx.clone());
            std::thread::spawn(move || {
                loop {
                    let next = queue.lock().ok().and_then(|mut q| q.pop_front());
                    let Some(wt) = next else { break };
                    // Stops early once the dialog is gone and nobody listens.
                    if tx.send((wt.path.clone(), git::check_worktree(&wt))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let own_pid = std::process::id();
        let programs = cx.background_spawn(async move { git::process_folders() });
        let project = project_id.clone();
        cx.spawn(async move |this, cx| {
            let folders = programs.await;
            let _ = this.update(cx, |this, cx| {
                let Some(Modal::RemoveWorktrees(form)) = this.modal.as_mut() else { return };
                if form.project_id != project {
                    return;
                }
                for item in form.items.iter_mut() {
                    let root = &item.worktree.path;
                    let real = root.canonicalize().unwrap_or_else(|_| root.clone());
                    // Glasscode's own sessions (and what runs in them) end with the removal.
                    let mut names: Vec<String> = folders
                        .iter()
                        .filter(|p| p.dir.starts_with(root) || p.dir.starts_with(&real))
                        .filter(|p| !git::descends_from(&folders, p.pid, own_pid))
                        .map(|p| p.name.clone())
                        .collect();
                    names.sort();
                    names.dedup();
                    item.programs = names;
                }
                cx.notify();
            });
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let mut batch = Vec::new();
                let finished = loop {
                    match rx.try_recv() {
                        Ok(result) => batch.push(result),
                        Err(mpsc::TryRecvError::Empty) => break false,
                        Err(mpsc::TryRecvError::Disconnected) => break true,
                    }
                };
                let open = this.update(cx, |this, cx| {
                    let keep_branch = this.state.prefs.keep_branch_on_remove;
                    let Some(Modal::RemoveWorktrees(form)) = this.modal.as_mut() else { return false };
                    if form.project_id != project_id {
                        return false;
                    }
                    for (path, check) in batch {
                        if let Some(item) = form.items.iter_mut().find(|i| i.worktree.path == path) {
                            item.check = Some(check);
                        }
                    }
                    if form.single && !form.branch_touched && !keep_branch {
                        form.delete_branches = form.items.first().is_some_and(|i| i.check.as_ref().is_some_and(|c| c.branch_only_commits == 0));
                    }
                    cx.notify();
                    true
                });
                if finished || !open.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    fn remove_selected_worktrees(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::RemoveWorktrees(form)) = self.modal.as_mut() else { return };
        if form.busy {
            return;
        }
        let single = form.single;
        let delete_branches = form.delete_branches;
        let protected = form.protected_branches.clone();
        // A worktree whose files have changes not saved in the editor stays (U20): that text is
        // only in Glasscode.
        let unsaved = |path: &PathBuf| !self.gh.files.tabs.unsaved(|w| w == path).is_empty();
        let held: Vec<String> = form.items.iter().filter(|i| i.selected && !i.removed() && unsaved(&i.worktree.path)).map(|i| i.label.clone()).collect();
        let held = (!held.is_empty()).then(|| format!("Not removed: {} (files with changes not saved in the editor; save or discard them first).", held.join(", ")));
        let jobs: Vec<(Worktree, bool, bool)> = form
            .items
            .iter()
            .filter(|i| i.selected && !i.removed() && (i.check.is_some() || i.worktree.prunable))
            .filter(|i| !self.removing.contains(&i.worktree.path) && !unsaved(&i.worktree.path))
            .map(|i| {
                let force = i.risky();
                let branch_ok = !i.worktree.is_detached() && !protected.contains(&i.worktree.branch);
                // One worktree: the developer saw what the branch has and ticked the box.
                // Clean-up: only branches whose commits are all on the remote or elsewhere.
                let delete_branch = delete_branches
                    && branch_ok
                    && (single || i.check.as_ref().is_some_and(|c| c.branch_only_commits == 0));
                (i.worktree.clone(), force, delete_branch)
            })
            .collect();
        if jobs.is_empty() {
            if let Some(held) = held {
                self.show_toast(held, cx);
            }
            return;
        }
        let Some(root) = self.state.project(&form.project_id).map(|p| p.root.clone()) else { return };
        // It goes on in the background: the dialog closes, the sidebar shows "Removing…" on
        // each row, and a toast says how it went.
        self.modal = None;
        let names: Vec<String> = jobs.iter().map(|(w, _, _)| if w.branch.is_empty() { home_relative(&w.path) } else { w.branch.clone() }).collect();
        for (wt, _, _) in &jobs {
            self.removing.insert(wt.path.clone());
        }
        let _ = single;
        let removing = if names.len() == 1 { format!("Removing {} in the background…", names[0]) } else { format!("Removing {} in the background…", plural(names.len(), "worktree", "worktrees")) };
        self.show_toast(held.map_or(removing.clone(), |held| format!("{removing} {held}")), cx);

        // Sessions in these worktrees end first: their terminals hold the folders open.
        let doomed: Vec<String> = self
            .state
            .sessions
            .iter()
            .filter(|s| jobs.iter().any(|(w, _, _)| w.path == s.worktree))
            .map(|s| s.id.clone())
            .collect();
        for id in doomed {
            self.close_session(&id, window, cx);
        }

        let task = cx.background_spawn(async move {
            jobs.into_iter()
                .map(|(wt, force, delete_branch)| {
                    let result = git::remove_worktree(&root, &wt, force).map_err(|e| e.to_string()).map(|()| {
                        if !delete_branch {
                            return "removed".to_string();
                        }
                        match git::delete_branch(&root, &wt.branch) {
                            Ok(()) => "removed with its branch".to_string(),
                            Err(err) => format!("removed (branch kept: {err})"),
                        }
                    });
                    (wt.path, result)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = task.await;
            let _ = this.update(cx, |this, cx| {
                for (path, _) in &results {
                    this.removing.remove(path);
                }
                let removed = results.iter().filter(|(_, r)| r.is_ok()).count();
                let branches = results.iter().filter(|(_, r)| r.as_ref().is_ok_and(|n| n.contains("its branch"))).count();
                let failures: Vec<String> = results.iter().filter_map(|(path, r)| r.as_ref().err().map(|e| format!("{}: {e}", home_relative(path)))).collect();
                this.refresh_worktrees(cx);
                let mut message = if removed == 1 && results.len() == 1 {
                    format!("Removed {}", names[0])
                } else {
                    format!("Removed {}", plural(removed, "worktree", "worktrees"))
                };
                if branches > 0 {
                    message.push_str(&format!(" and {}", plural(branches, "branch", "branches")));
                }
                message.push('.');
                if failures.is_empty() {
                    this.show_toast(message, cx);
                } else if removed == 0 {
                    this.show_toast(format!("Could not remove {}", failures.join("; ")), cx);
                } else {
                    this.show_toast(format!("{message} Could not remove {}", failures.join("; ")), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

// ----- rendering -------------------------------------------------------------------------------

pub fn render_remove(_ws: &Workspace, form: &RemoveForm, cx: &mut Context<Workspace>) -> AnyElement {
    if form.single {
        return render_single(form, cx);
    }
    let clean = form.items.iter().filter(|i| i.clean() && !i.removed()).count();
    let loading = form.items.iter().any(|i| i.check.is_none() && !i.worktree.prunable);
    let mut rows: Vec<AnyElement> = Vec::new();
    for (index, item) in form.items.iter().enumerate() {
        let (status, color) = status_of(item);
        let detail = if item.worktree.is_detached() {
            format!("detached @ {}", &item.worktree.head[..7.min(item.worktree.head.len())])
        } else {
            git::folder_label(&item.worktree.path, "")
        };
        let removed = item.removed();
        rows.push(
            div()
                .id(SharedString::from(format!("rm-{index}")))
                .flex()
                .items_center()
                .gap_2p5()
                .h(px(32.))
                .px_2()
                .rounded_md()
                .when(!removed, |d| d.cursor_pointer().hover(|s| s.bg(theme::hover())))
                .when(item.selected, |d| d.bg(theme::selected()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    with_form(this, |form| {
                        if let Some(item) = form.items.get_mut(index) {
                            if !item.removed() && !form.busy {
                                item.selected = !item.selected;
                            }
                        }
                    });
                    cx.notify();
                }))
                .child(if removed { div().w(px(15.)).text_color(theme::added()).child("✓").into_any_element() } else { ui::checkbox(SharedString::from(format!("rm-check-{index}")), item.selected).into_any_element() })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .overflow_hidden()
                        .child(
                            div()
                                .flex_none()
                                .max_w(px(300.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .font_family(theme::MONO_FONT)
                                .text_size(px(12.))
                                .text_color(if removed { theme::faint() } else { theme::text_strong() })
                                .child(item.label.clone()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(11.5))
                                .text_color(theme::faint())
                                .child(detail),
                        ),
                )
                .child(div().flex_none().text_size(px(11.5)).text_color(color).child(status))
                .child(
                    div()
                        .flex_none()
                        .w(px(84.))
                        .text_size(px(11.5))
                        .text_color(theme::muted())
                        .child(ago(item.check.as_ref().and_then(|c| c.last_commit))),
                )
                .into_any_element(),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div().text_size(px(12.5)).text_color(theme::text_soft()).child(
                "Removing a worktree deletes its folder. Branches stay unless you tick the box below. \
                 Red ones have work that exists nowhere else; they are removed only if you select them.",
            ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    ui::chip("rm-select-clean", false, if loading { "Checking…".to_string() } else { format!("Select clean ones ({clean})") })
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            with_form(this, |form| {
                                for item in form.items.iter_mut() {
                                    item.selected = item.clean() && !item.removed();
                                }
                            });
                            cx.notify();
                        })),
                )
                .child(ui::chip("rm-select-none", false, "Clear").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    with_form(this, |form| form.items.iter_mut().for_each(|i| i.selected = false));
                    cx.notify();
                })))
                .child(
                    div()
                        .ml_auto()
                        .text_size(px(12.))
                        .text_color(theme::muted())
                        .child(format!("{} besides the main checkout", plural(form.items.len(), "worktree", "worktrees"))),
                ),
        )
        .child(div().flex().flex_col().gap_0p5().children(rows))
        .into_any_element()
}

fn render_single(form: &RemoveForm, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(item) = form.items.first() else { return div().into_any_element() };
    let wt = &item.worktree;
    let line = |glyph: &'static str, color: gpui::Hsla, text: String| {
        div()
            .flex()
            .items_start()
            .gap_2()
            .child(div().flex_none().w(px(14.)).text_color(color).child(glyph))
            .child(div().flex_1().text_color(theme::text()).child(text))
            .into_any_element()
    };
    let mut lines: Vec<AnyElement> = Vec::new();
    match (&item.check, wt.prunable) {
        (_, true) => lines.push(line("✓", theme::added(), "The folder is already gone; this only cleans up git's list.".into())),
        (None, _) => lines.push(line("…", theme::faint(), "Checking what is in this worktree…".into())),
        (Some(check), _) => {
            if check.changed > 0 {
                lines.push(line("✕", theme::removed(), format!("{} will be deleted.", plural(check.changed, "uncommitted file", "uncommitted files"))));
            }
            if check.lost_commits > 0 {
                lines.push(line(
                    "✕",
                    theme::removed(),
                    format!("{} on this detached HEAD {} on no branch or remote and will be lost.", plural(check.lost_commits, "commit", "commits"), if check.lost_commits == 1 { "is" } else { "are" }),
                ));
            }
            if wt.locked {
                lines.push(line("!", theme::modified(), "It is locked (git worktree lock), probably by the tool that created it.".into()));
            }
            if !check.env_files.is_empty() {
                let names: Vec<&str> = check.env_files.iter().take(4).map(String::as_str).collect();
                lines.push(line("!", theme::modified(), format!("Local files git ignores are deleted too: {}{}", names.join(", "), if check.env_files.len() > 4 { ", …" } else { "" })));
            }
            if check.is_clean() && !wt.locked {
                lines.push(line("✓", theme::added(), "Nothing is lost: no uncommitted changes, and every commit is on a branch or the remote.".into()));
            }
        }
    }
    if item.sessions > 0 {
        lines.push(line("•", theme::codex(), format!("{} in this worktree will be closed.", plural(item.sessions, "Glasscode session", "Glasscode sessions"))));
    }
    if !item.programs.is_empty() {
        lines.push(line(
            "!",
            theme::modified(),
            format!("Running in this folder right now: {}. Stop them first, or they lose their folder.", item.programs.join(", ")),
        ));
    }
    let offer_branch = !wt.is_detached() && !wt.branch.is_empty() && !form.protected_branches.contains(&wt.branch);
    let branch_only = item.check.as_ref().map(|c| c.branch_only_commits).unwrap_or(0);
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::raised())
                .child(div().font_family(theme::MONO_FONT).text_color(theme::text_strong()).child(item.label.clone()))
                .child(div().mt_1().font_family(theme::MONO_FONT).text_size(px(11.5)).text_color(theme::muted()).child(home_relative(&wt.path)))
                .child(
                    div().mt_1().text_size(px(12.)).text_color(theme::muted()).child(
                        [
                            if wt.is_detached() { format!("detached @ {}", &wt.head[..7.min(wt.head.len())]) } else { format!("branch {}", wt.branch) },
                            item.check.as_ref().and_then(|c| c.last_commit).map(|t| format!("last commit {}", ago(Some(t)))).unwrap_or_default(),
                        ]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · "),
                    ),
                ),
        )
        .child(div().flex().flex_col().gap_1p5().text_size(px(12.5)).children(lines))
        .when(offer_branch, |d| {
            d.child(
                div()
                    .id("rm-branch")
                    .flex()
                    .items_start()
                    .gap_2()
                    .mt_1()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        with_form(this, |form| {
                            form.delete_branches = !form.delete_branches;
                            form.branch_touched = true;
                        });
                        cx.notify();
                    }))
                    .child(ui::checkbox("rm-branch-check", form.delete_branches))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(div().text_color(theme::text()).child(format!("Also delete the local branch {}", wt.branch)))
                            .child(div().text_size(px(12.)).text_color(if branch_only > 0 { theme::removed() } else { theme::muted() }).child(if branch_only > 0 {
                                format!("{} only on this branch and would be lost.", plural(branch_only, "commit exists", "commits exist"))
                            } else {
                                "Its commits are on the remote or another branch, so nothing is lost.".to_string()
                            })),
                    ),
            )
        })
        .when_some(item.result.as_ref().and_then(|r| r.as_ref().err()), |d, err| {
            d.child(div().p_2p5().rounded_md().bg(theme::error_bg()).text_color(theme::error_text()).text_size(px(12.)).child(err.clone()))
        })
        .into_any_element()
}

pub fn remove_footer(form: &RemoveForm, cx: &mut Context<Workspace>) -> AnyElement {
    let selected: Vec<&RemoveItem> = form.items.iter().filter(|i| i.selected && !i.removed()).collect();
    let checking = selected.iter().any(|i| i.check.is_none() && !i.worktree.prunable);
    let risky = selected.iter().filter(|i| i.risky()).count();
    let label = if form.busy {
        "Removing…".to_string()
    } else if checking {
        "Checking…".to_string()
    } else if form.single {
        if risky > 0 { "Remove anyway".into() } else { "Remove worktree".into() }
    } else if risky > 0 {
        format!("Remove {} ({} with unsaved work)", selected.len(), risky)
    } else {
        format!("Remove {}", plural(selected.len(), "worktree", "worktrees"))
    };
    let enabled = !form.busy && !checking && !selected.is_empty();
    let button = if !enabled {
        ui::disabled_button("rm-go", label)
    } else if risky > 0 {
        ui::danger_button("rm-go", label)
    } else {
        ui::primary_button("rm-go", label)
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .when(!form.single, |d| {
            d.child(
                div()
                    .id("rm-branches")
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(theme::text_soft())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        with_form(this, |form| form.delete_branches = !form.delete_branches);
                        cx.notify();
                    }))
                    .child(ui::checkbox("rm-branches-check", form.delete_branches))
                    .child("Also delete their local branches when every commit is on the remote"),
            )
        })
        .child(div().flex_1())
        .child(ui::button("rm-cancel", "Cancel").on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_modal(window, cx))))
        .child(button.font_weight(FontWeight::SEMIBOLD).when(enabled, |b| {
            b.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.remove_selected_worktrees(window, cx)))
        }))
        .into_any_element()
}
