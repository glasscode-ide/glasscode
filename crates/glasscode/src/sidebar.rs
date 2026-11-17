//! The left sidebar's tree: each project, its worktrees, and the sessions in each worktree, and
//! every level can fold. [`tree`] makes the rows from plain data, with no window, so that the
//! order and the folds are tested here and a long tree stays cheap to draw; the functions after
//! it draw one kind of row each. The Server group draws its sessions with the same session row.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, App, ClickEvent, Div, FontWeight, Hsla, MouseButton, MouseDownEvent, SharedString, Transformation, Window, div,
    percentage, prelude::*, pulsating_between,
};
use glasscode_core::git::{self, Worktree};
use glasscode_core::model::{AgentKind, Project, Session};

use crate::icons;
use crate::theme;
use crate::ui;

/// What a row says about its sessions, most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowState {
    NeedsYou,
    Working,
    Idle,
}

pub type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
pub type OnRightClick = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

/// The orange that means "needs you".
pub fn attention() -> Hsla {
    theme::attention()
}

/// An agent's mark (Claude, Codex, a terminal for a shell), faded while its program is not
/// running.
pub fn agent_icon(agent: AgentKind, size: f32, running: bool) -> Div {
    div()
        .flex_none()
        .size(theme::sz(size))
        .flex()
        .items_center()
        .justify_center()
        .when(!running, |d| d.opacity(0.6))
        // `brand` takes pixels on the screen, so the scale goes in here.
        .child(icons::brand(icons::agent_brand(agent), (size * 0.82 * theme::scale()).round()))
}

/// Green while working (a slow pulse), orange when it needs you, nothing when idle.
pub fn status_icon(state: RowState, key: &str) -> Option<AnyElement> {
    match state {
        RowState::Working => Some(
            div()
                .size(theme::sz(8.))
                .rounded_full()
                .bg(theme::added())
                .with_animation(SharedString::from(format!("working-{key}")), Animation::new(Duration::from_millis(1400)).repeat().with_easing(pulsating_between(0.35, 1.0)), |d, t| d.opacity(t))
                .into_any_element(),
        ),
        RowState::NeedsYou => Some(
            div()
                .size(theme::sz(15.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(attention())
                .text_color(theme::on_attention())
                .text_size(theme::sz(10.5))
                .font_weight(FontWeight::BOLD)
                .child("!")
                .into_any_element(),
        ),
        RowState::Idle => None,
    }
}

/// A review's "#958 fix: …" without the number, which the row shows in its own column.
pub fn without_number(title: &str, number: Option<u64>) -> String {
    if let Some(n) = number {
        if let Some(rest) = title.strip_prefix(&format!("#{n} ")) {
            return rest.to_string();
        }
    }
    title.to_string()
}

// ----- the rows, from plain data -----------------------------------------------------------------

/// The most characters of a title a row keeps. The widest sidebar shows about 90; a longer title
/// would be shaped on each frame only to be cut.
const TITLE_MAX: usize = 120;

/// A project as the tree reads it.
pub struct TreeProject<'a> {
    pub project: &'a Project,
    /// Git's worktrees, or None before the first refresh has listed them. An empty list: git
    /// listed none, so the folder has no git.
    pub worktrees: Option<&'a [Worktree]>,
}

/// A session as the tree reads it, with what the workspace knows about it now.
pub struct TreeSession<'a> {
    pub session: &'a Session,
    pub state: RowState,
    /// Its program runs (else its mark is faded: it starts when opened).
    pub running: bool,
    /// The pull request of its branch.
    pub pr: Option<(u64, PrState)>,
}

/// A pull request's number shows in the colour of its checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrState {
    /// A check failed.
    Failing,
    /// Open, and no check failed.
    Open,
    /// Merged, closed, or not known yet.
    Other,
}

pub struct Tree<'a> {
    /// In the order of the sidebar (hidden projects left out).
    pub projects: &'a [TreeProject<'a>],
    /// In the state's order: a worktree lists its sessions in this order.
    pub sessions: &'a [TreeSession<'a>],
    /// Pinned worktrees, the first pinned highest.
    pub pinned: &'a [PathBuf],
    /// `Layout.folded`.
    pub folded: &'a [String],
    /// Worktrees being removed.
    pub removing: &'a HashSet<PathBuf>,
    /// Projects whose worktrees without a session are open.
    pub idle_open: &'a HashSet<String>,
    /// The session on screen, in the Sessions view only.
    pub selected: Option<&'a str>,
}

/// One row of the tree, in the order it shows.
#[derive(Clone, Debug, PartialEq)]
pub enum TreeRow {
    Project {
        id: String,
        name: String,
        /// Its sessions that need the person.
        need: usize,
        /// Its worktrees (with a session and without), not those being removed.
        worktrees: usize,
        folded: bool,
        /// It has the session on screen.
        current: bool,
    },
    /// A worktree with sessions.
    Worktree {
        project_id: String,
        path: PathBuf,
        /// The branch, the folder for a detached one, or "Folder · no git".
        label: String,
        git: bool,
        folded: bool,
        /// The most urgent state of its sessions.
        state: RowState,
        sessions: usize,
        pinned: bool,
        current: bool,
        pr: Option<(u64, PrState)>,
    },
    Session {
        id: String,
        project_id: String,
        worktree: PathBuf,
        agent: AgentKind,
        title: String,
        state: RowState,
        running: bool,
        selected: bool,
    },
    /// A worktree whose removal runs in the background.
    Removing { path: PathBuf, label: String },
    /// "N more worktrees without a session", which opens to list them.
    MoreWorktrees { project_id: String, count: usize, open: bool },
    /// A worktree without a session (a click starts one in it).
    IdleWorktree { project_id: String, path: PathBuf, label: String, detached: bool },
}

/// The folded key of a project: its id.
pub fn project_key(project_id: &str) -> String {
    project_id.to_string()
}

/// The folded key of a worktree: its project's id and its path. Paths stay when a branch is
/// renamed, and the "|" ends the id (a project id is a UUID), so a worktree key never equals a
/// project key.
pub fn worktree_key(project_id: &str, path: &Path) -> String {
    let path = path.to_string_lossy();
    // On Windows git lists C:/a/b and a session can keep C:\a\b: one key for both (a Windows
    // name has no "\" of its own).
    let path = if cfg!(windows) { path.replace('\\', "/") } else { path.into_owned() };
    format!("{project_id}|{path}")
}

/// The project a folded key belongs to.
fn key_project(key: &str) -> &str {
    key.split('|').next().unwrap_or(key)
}

/// Whether a project shows folded: its key, or folded by the build before the tree
/// (`Project.expanded`), so an old state loads as it was left.
pub fn project_folded(folded: &[String], project: &Project) -> bool {
    !project.expanded || folded.contains(&project.id)
}

/// Fold `key` or open it; a key is in the list once.
pub fn set_folded(folded: &mut Vec<String>, key: &str, fold: bool) {
    folded.retain(|k| k != key);
    if fold {
        folded.push(key.to_string());
    }
}

/// Keep only the keys of projects that still exist (switched-off projects keep theirs).
pub fn prune_folded(folded: &mut Vec<String>, projects: &[&str]) {
    folded.retain(|k| projects.contains(&key_project(k)));
}

/// A title on one line and not longer than a row can show.
fn row_title(title: &str) -> String {
    let line: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(TITLE_MAX) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line,
    }
}

/// A worktree's name: its branch, else its folder.
fn worktree_label(git_project: bool, branch: &str, path: &Path, repo_name: &str) -> String {
    if !git_project {
        return "Folder · no git".to_string();
    }
    match branch {
        "detached" => git::folder_label(path, repo_name),
        "" | "HEAD" => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        _ => branch.to_string(),
    }
}

/// The sessions of a project that work in `path`.
fn sessions_in<'s>(sessions: &'s [&'s TreeSession<'s>], path: &'s Path) -> impl Iterator<Item = &'s TreeSession<'s>> + 's {
    sessions.iter().copied().filter(move |s| s.session.worktree == path)
}

/// The rows of the tree. Under each open project: its worktrees with sessions (pinned first, in
/// pin order, then the one with the newest session first), the worktrees being removed, then
/// "N more worktrees without a session". Under each open worktree: its sessions. It reads no
/// file and runs no git, as it runs on each frame.
pub fn tree(t: &Tree, title: impl Fn(&Session) -> String) -> Vec<TreeRow> {
    let mut by_project: HashMap<&str, Vec<&TreeSession>> = HashMap::new();
    for s in t.sessions {
        by_project.entry(s.session.project_id.as_str()).or_default().push(s);
    }
    let pin = |path: &Path| t.pinned.iter().position(|p| p == path);
    let mut rows = Vec::new();
    for p in t.projects {
        let project = p.project;
        let sessions: &[&TreeSession] = by_project.get(project.id.as_str()).map(Vec::as_slice).unwrap_or(&[]);
        let git_project = p.worktrees.is_none_or(|list| !list.is_empty());
        let repo_name = project.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

        // Git's worktrees (only the root before git has listed them), then the folders of
        // sessions that git does not list.
        let mut list: Vec<(&Path, &str)> = match p.worktrees {
            Some(list) if !list.is_empty() => list.iter().map(|w| (w.path.as_path(), w.branch.as_str())).collect(),
            _ => vec![(project.root.as_path(), "")],
        };
        for s in sessions {
            if !list.iter().any(|(path, _)| *path == s.session.worktree) {
                list.push((s.session.worktree.as_path(), s.session.branch.as_str()));
            }
        }
        // Stable, so worktrees that tie keep git's order.
        let mut busy: Vec<(&Path, u64)> = list
            .iter()
            .filter(|(path, _)| !t.removing.contains(*path))
            .filter_map(|(path, _)| Some((*path, sessions_in(sessions, path).map(|s| s.session.created_at).max()?)))
            .collect();
        busy.sort_by(|a, b| match (pin(a.0), pin(b.0)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.1.cmp(&a.1),
        });
        let idle: Vec<(&Path, &str)> = list.iter().copied().filter(|(path, _)| !t.removing.contains(*path) && sessions_in(sessions, path).next().is_none()).collect();

        let folded = project_folded(t.folded, project);
        rows.push(TreeRow::Project {
            id: project.id.clone(),
            name: project.name.clone(),
            need: sessions.iter().filter(|s| s.state == RowState::NeedsYou).count(),
            worktrees: busy.len() + idle.len(),
            folded,
            current: t.selected.is_some_and(|id| sessions.iter().any(|s| s.session.id == id)),
        });
        if folded {
            continue;
        }

        for (path, _) in busy {
            let here: Vec<&TreeSession> = sessions_in(sessions, path).collect();
            // The work is named after its agent session (a shell beside it changes nothing).
            let main = here.iter().find(|s| s.session.agent != AgentKind::Shell).unwrap_or(&here[0]);
            let folded = t.folded.contains(&worktree_key(&project.id, path));
            rows.push(TreeRow::Worktree {
                project_id: project.id.clone(),
                path: path.to_path_buf(),
                label: worktree_label(git_project, &main.session.branch, path, &repo_name),
                git: git_project,
                folded,
                state: here.iter().map(|s| s.state).min().unwrap_or(RowState::Idle),
                sessions: here.len(),
                pinned: pin(path).is_some(),
                current: t.selected.is_some_and(|id| here.iter().any(|s| s.session.id == id)),
                pr: main.pr,
            });
            if folded {
                continue;
            }
            for s in here {
                rows.push(TreeRow::Session {
                    id: s.session.id.clone(),
                    project_id: project.id.clone(),
                    worktree: path.to_path_buf(),
                    agent: s.session.agent,
                    title: row_title(&title(s.session)),
                    state: s.state,
                    running: s.running,
                    selected: t.selected == Some(s.session.id.as_str()),
                });
            }
        }
        for (path, branch) in list.iter().filter(|(path, _)| t.removing.contains(*path)) {
            rows.push(TreeRow::Removing { path: path.to_path_buf(), label: worktree_label(git_project, branch, path, &repo_name) });
        }
        if !idle.is_empty() {
            let open = t.idle_open.contains(&project.id);
            rows.push(TreeRow::MoreWorktrees { project_id: project.id.clone(), count: idle.len(), open });
            if open {
                for (path, branch) in idle {
                    rows.push(TreeRow::IdleWorktree {
                        project_id: project.id.clone(),
                        path: path.to_path_buf(),
                        label: worktree_label(git_project, branch, path, &repo_name),
                        detached: branch == "detached",
                    });
                }
            }
        }
    }
    rows
}

// ----- drawing the rows --------------------------------------------------------------------------

/// A row's padding at its sides.
const ROW_X: f32 = 6.;
const CHEVRON: f32 = 12.;
/// From a row's left edge to the guide line of its children: under its chevron.
const GUIDE: f32 = ROW_X + CHEVRON / 2.;
/// The width of the folder and branch icons.
const ICON: f32 = 14.;

fn row_height(project: bool) -> f32 {
    match (project, theme::compact()) {
        (true, false) => 30.,
        (true, true) => 26.,
        (false, false) => 28.,
        (false, true) => 24.,
    }
}

/// A row `depth` levels in, with a guide line for each level, as in the prototype.
pub fn indent(depth: usize, row: impl IntoElement) -> AnyElement {
    let mut row = row.into_any_element();
    for _ in 0..depth {
        row = div().ml(theme::sz(GUIDE)).pl(theme::sz(4.)).border_l_1().border_color(theme::border()).child(row).into_any_element();
    }
    row
}

/// The chevron of a row that folds: down when open, right when folded (three quarters of a turn,
/// as `percentage` takes no negative part).
fn chevron(folded: bool) -> impl IntoElement {
    icons::icon(icons::CHEVRON_DOWN, theme::sz(CHEVRON), theme::sz(CHEVRON), theme::faint()).when(folded, |s| s.with_transformation(Transformation::rotate(percentage(0.75))))
}

/// The base of each row: one line, its hover and its group for the hover buttons.
fn row_base(id: SharedString, group: SharedString, project: bool) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .group(group)
        .relative()
        .flex()
        .items_center()
        .gap(theme::sz(6.))
        .h(theme::sz(row_height(project)))
        .px(theme::sz(ROW_X))
        .rounded(theme::sz(6.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()))
}

/// Text that takes the room left in a row and ends in "…" when it does not fit.
fn fill_text(text: impl Into<SharedString>) -> Div {
    div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(text.into())
}

/// A small icon button of a row (the "+", "⋯" and "×" that show on hover).
pub fn row_button(id: SharedString, glyph: &'static [u8], on_click: OnClick) -> AnyElement {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(theme::sz(20.))
        .rounded(theme::sz(5.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::selected()))
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
        .child(icons::icon(glyph, theme::sz(13.), theme::sz(13.), theme::text_soft()))
        .into_any_element()
}

/// The buttons of a row, over its right end while the pointer is on the row (or its menu is
/// open). They cover the counts under them, which show again when the pointer leaves.
fn hover_buttons(group: SharedString, keep: bool, buttons: Vec<AnyElement>) -> Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .right(theme::sz(3.))
        .flex()
        .items_center()
        .gap(theme::sz(2.))
        .pl(theme::sz(4.))
        .rounded(theme::sz(5.))
        .bg(theme::hover())
        .when(!keep, |d| d.invisible().group_hover(group, |s| s.visible()))
        .children(buttons)
}

/// A faint number at the end of a row.
fn count(n: usize) -> Div {
    div().flex_none().text_size(theme::sz(11.)).text_color(theme::faint()).child(n.to_string())
}

/// The orange count of what needs the person.
pub fn need_badge(n: usize) -> Div {
    div()
        .flex_none()
        .min_w(theme::sz(16.))
        .h(theme::sz(16.))
        .px(theme::sz(4.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(attention())
        .text_color(theme::on_attention())
        .text_size(theme::sz(10.))
        .font_weight(FontWeight::BOLD)
        .child(n.to_string())
}

/// The colour of a pull request's number.
pub fn pr_color(state: PrState) -> Hsla {
    match state {
        PrState::Failing => theme::removed(),
        PrState::Open => theme::added(),
        PrState::Other => theme::muted(),
    }
}

pub struct ProjectLine {
    pub id: String,
    pub name: String,
    pub need: usize,
    pub worktrees: usize,
    pub folded: bool,
    pub current: bool,
    /// The GitHub account its sessions use (`@login`); a click opens Settings › Projects.
    pub github: Option<(String, OnClick)>,
    /// The account fits in the row; else it shows with the hover buttons.
    pub github_inline: bool,
    pub menu_open: bool,
}

pub struct RowActions {
    /// A click on the row.
    pub click: OnClick,
    /// "+": a new session here.
    pub add: Option<OnClick>,
    /// "⋯" and the right button: the row's menu.
    pub menu: Option<(OnClick, OnRightClick)>,
    /// "×": close.
    pub close: Option<OnClick>,
}

fn split_actions(key: &str, actions: RowActions) -> (OnClick, Option<OnRightClick>, Vec<AnyElement>) {
    let mut buttons = Vec::new();
    let mut right = None;
    if let Some(add) = actions.add {
        buttons.push(row_button(SharedString::from(format!("{key}-add")), icons::PLUS, add));
    }
    if let Some((menu, menu_right)) = actions.menu {
        buttons.push(row_button(SharedString::from(format!("{key}-menu")), icons::MORE, menu));
        right = Some(menu_right);
    }
    if let Some(close) = actions.close {
        buttons.push(row_button(SharedString::from(format!("{key}-close")), icons::CLOSE, close));
    }
    (actions.click, right, buttons)
}

/// A project: chevron, folder, name, the account, what needs the person, its worktrees.
pub fn project_row(line: ProjectLine, actions: RowActions) -> AnyElement {
    let key = format!("tree-p-{}", line.id);
    let group = SharedString::from(format!("{key}-g"));
    let (click, right, mut buttons) = split_actions(&key, actions);
    let pill = line.github.map(|(login, open)| {
        div()
            .id(SharedString::from(format!("{key}-gh")))
            .flex_none()
            .max_w(theme::sz(96.))
            .h(theme::sz(18.))
            .px(theme::sz(6.))
            .flex()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .rounded_full()
            .border_1()
            .border_color(theme::border())
            .text_size(theme::sz(10.5))
            .text_color(theme::muted())
            .font_weight(FontWeight::NORMAL)
            .cursor_pointer()
            .hover(|s| s.bg(theme::selected()).text_color(theme::text_strong()))
            .on_click(move |event, window, cx| {
                cx.stop_propagation();
                open(event, window, cx);
            })
            .child(format!("@{login}"))
            .into_any_element()
    });
    let mut inline_pill = None;
    if let Some(pill) = pill {
        if line.github_inline {
            inline_pill = Some(pill);
        } else {
            buttons.insert(0, pill);
        }
    }
    row_base(SharedString::from(key), group.clone(), true)
        .mt(theme::sz(4.))
        .text_size(theme::sz(13.))
        .text_color(if line.current { theme::text_strong() } else { theme::text() })
        .on_click(move |event, window, cx| click(event, window, cx))
        .when_some(right, |d, right| d.on_mouse_down(MouseButton::Right, move |event, window, cx| right(event, window, cx)))
        .child(chevron(line.folded))
        .child(icons::icon(icons::FOLDER, theme::sz(ICON), theme::sz(ICON), theme::muted()))
        .child(fill_text(line.name).font_weight(FontWeight::MEDIUM))
        .children(inline_pill)
        .when(line.need > 0, |d| d.child(need_badge(line.need)))
        .when(line.worktrees > 0, |d| d.child(count(line.worktrees)))
        .child(hover_buttons(group, line.menu_open, buttons))
        .into_any_element()
}

pub struct WorktreeLine {
    /// Unique among rows.
    pub key: String,
    pub label: String,
    pub git: bool,
    pub folded: bool,
    pub state: RowState,
    pub sessions: usize,
    pub pinned: bool,
    pub current: bool,
    pub pr: Option<(u64, PrState)>,
    pub menu_open: bool,
}

/// A worktree: chevron, branch, its name in the code font, the pin, the pull request's number,
/// the dot of a folded one that needs the person, and its sessions.
pub fn worktree_row(line: WorktreeLine, actions: RowActions) -> AnyElement {
    let key = format!("tree-w-{}", line.key);
    let group = SharedString::from(format!("{key}-g"));
    let (click, right, buttons) = split_actions(&key, actions);
    row_base(SharedString::from(key), group.clone(), false)
        .text_size(theme::sz(12.))
        .text_color(if line.current { theme::text() } else { theme::text_soft() })
        .on_click(move |event, window, cx| click(event, window, cx))
        .when_some(right, |d, right| d.on_mouse_down(MouseButton::Right, move |event, window, cx| right(event, window, cx)))
        .child(chevron(line.folded))
        .child(icons::icon(if line.git { icons::BRANCH } else { icons::FOLDER }, theme::sz(ICON), theme::sz(ICON), theme::muted()))
        .child(fill_text(line.label).when(line.git, |d| d.font_family(theme::MONO_FONT)))
        .when(line.pinned, |d| d.child(icons::icon(icons::PIN, theme::sz(12.), theme::sz(12.), theme::faint())))
        .when_some(line.pr, |d, (number, state)| d.child(div().flex_none().text_size(theme::sz(11.)).text_color(pr_color(state)).child(format!("#{number}"))))
        .when(line.folded && line.state == RowState::NeedsYou, |d| d.child(ui::status_dot(RowState::NeedsYou, false)))
        .child(count(line.sessions))
        .child(hover_buttons(group, line.menu_open, buttons))
        .into_any_element()
}

pub struct SessionLine {
    /// Unique among rows.
    pub key: String,
    pub agent: AgentKind,
    pub running: bool,
    pub title: String,
    /// A quiet note after the title: the account when it is not the default, or for a server
    /// session its branch or "Review".
    pub detail: Option<String>,
    /// A pull request's number (server sessions; a local one has it on its worktree).
    pub pr: Option<(String, Hsla)>,
    pub state: RowState,
    /// Its program stopped (a server session): an empty ring.
    pub stopped: bool,
    pub selected: bool,
}

/// A session: its agent's mark, its title, a note, and its state as a dot.
pub fn session_row(line: SessionLine, actions: RowActions) -> AnyElement {
    let key = format!("tree-s-{}", line.key);
    let group = SharedString::from(format!("{key}-g"));
    let (click, right, buttons) = split_actions(&key, actions);
    row_base(SharedString::from(key), group.clone(), false)
        .text_size(theme::sz(12.5))
        .text_color(if line.selected { theme::text_strong() } else { theme::text_soft() })
        .when(line.selected, |d| d.bg(theme::selected()))
        .on_click(move |event, window, cx| click(event, window, cx))
        .when_some(right, |d, right| d.on_mouse_down(MouseButton::Right, move |event, window, cx| right(event, window, cx)))
        .child(agent_icon(line.agent, 16., line.running))
        .child(fill_text(line.title).when(line.state == RowState::NeedsYou, |d| d.font_weight(FontWeight::SEMIBOLD)))
        .when_some(line.detail, |d, detail| {
            // Narrow, so that the title keeps most of a narrow row.
            d.child(div().min_w(theme::sz(24.)).max_w(theme::sz(64.)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(11.)).text_color(theme::faint()).child(detail))
        })
        .when_some(line.pr, |d, (text, color)| d.child(div().flex_none().text_size(theme::sz(11.)).text_color(color).child(text)))
        .when(line.stopped, |d| d.child(div().flex_none().text_size(theme::sz(11.)).text_color(theme::faint()).child("stopped")))
        .child(ui::status_dot(line.state, line.stopped))
        .when(!buttons.is_empty(), |d| d.child(hover_buttons(group, false, buttons)))
        .into_any_element()
}

/// A worktree being removed: struck through, "Removing…".
pub fn removing_row(key: &str, label: String) -> AnyElement {
    div()
        .id(SharedString::from(format!("tree-r-{key}")))
        .flex()
        .items_center()
        .gap(theme::sz(6.))
        .h(theme::sz(row_height(false)))
        .px(theme::sz(ROW_X))
        .pl(theme::sz(ROW_X + CHEVRON + 6.))
        .opacity(0.55)
        .text_size(theme::sz(12.))
        .text_color(theme::text_soft())
        .child(icons::icon(icons::BRANCH, theme::sz(ICON), theme::sz(ICON), theme::faint()))
        .child(fill_text(label).line_through().font_family(theme::MONO_FONT))
        .child(div().flex_none().text_size(theme::sz(11.)).text_color(theme::muted()).child("Removing…"))
        .into_any_element()
}

/// "N more worktrees without a session", which opens to list them.
pub fn more_row(project_id: &str, count: usize, open: bool, click: OnClick) -> AnyElement {
    div()
        .id(SharedString::from(format!("tree-m-{project_id}")))
        .flex()
        .items_center()
        .gap(theme::sz(6.))
        .h(theme::sz(row_height(false)))
        .px(theme::sz(ROW_X))
        .rounded(theme::sz(6.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()))
        .text_size(theme::sz(11.5))
        .text_color(theme::muted())
        .on_click(move |event, window, cx| click(event, window, cx))
        .child(chevron(!open))
        .child(fill_text(format!("{count} more {} without a session", if count == 1 { "worktree" } else { "worktrees" })))
        .into_any_element()
}

/// A worktree without a session: a click starts one in it; "⋯" and the right button its menu.
pub fn idle_row(key: &str, label: String, detached: bool, menu_open: bool, actions: RowActions) -> AnyElement {
    let key = format!("tree-i-{key}");
    let group = SharedString::from(format!("{key}-g"));
    let (click, right, buttons) = split_actions(&key, actions);
    row_base(SharedString::from(key), group.clone(), false)
        .pl(theme::sz(ROW_X + CHEVRON + 6.))
        .text_size(theme::sz(12.))
        .text_color(theme::muted())
        .on_click(move |event, window, cx| click(event, window, cx))
        .when_some(right, |d, right| d.on_mouse_down(MouseButton::Right, move |event, window, cx| right(event, window, cx)))
        .child(icons::icon(icons::BRANCH, theme::sz(ICON), theme::sz(ICON), theme::faint()))
        .child(fill_text(label).font_family(theme::MONO_FONT))
        .when(detached, |d| d.child(div().flex_none().text_size(theme::sz(10.5)).text_color(theme::faint()).child("detached")))
        .child(hover_buttons(group, menu_open, buttons))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_titles_lose_their_number() {
        assert_eq!(without_number("#960 fix(core): configure the gateway", Some(960)), "fix(core): configure the gateway");
        assert_eq!(without_number("#960 fix", Some(961)), "#960 fix");
        assert_eq!(without_number("OCR", None), "OCR");
    }

    #[test]
    fn needs_you_is_most_urgent() {
        let mut states = vec![RowState::Idle, RowState::Working, RowState::NeedsYou];
        states.sort();
        assert_eq!(states[0], RowState::NeedsYou);
    }

    fn project(id: &str, root: &str) -> Project {
        Project { id: id.into(), name: id.into(), root: PathBuf::from(root), expanded: true, github_account: None }
    }

    fn session(id: &str, project: &str, worktree: &str, branch: &str, agent: AgentKind, created_at: u64) -> Session {
        Session {
            id: id.into(),
            project_id: project.into(),
            worktree: PathBuf::from(worktree),
            branch: branch.into(),
            title: format!("title {id}"),
            agent,
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

    /// The tree's input, with each session idle and running.
    struct Fixture {
        projects: Vec<Project>,
        worktrees: Vec<(String, Vec<Worktree>)>,
        sessions: Vec<Session>,
        states: HashMap<String, RowState>,
        pinned: Vec<PathBuf>,
        folded: Vec<String>,
        removing: HashSet<PathBuf>,
        idle_open: HashSet<String>,
        selected: Option<String>,
    }

    impl Fixture {
        fn new(projects: Vec<Project>, sessions: Vec<Session>) -> Self {
            Fixture {
                projects,
                worktrees: Vec::new(),
                sessions,
                states: HashMap::new(),
                pinned: Vec::new(),
                folded: Vec::new(),
                removing: HashSet::new(),
                idle_open: HashSet::new(),
                selected: None,
            }
        }

        fn rows(&self) -> Vec<TreeRow> {
            let projects: Vec<TreeProject> = self
                .projects
                .iter()
                .map(|p| TreeProject { project: p, worktrees: self.worktrees.iter().find(|(id, _)| *id == p.id).map(|(_, list)| list.as_slice()) })
                .collect();
            let sessions: Vec<TreeSession> = self
                .sessions
                .iter()
                .map(|s| TreeSession { session: s, state: self.states.get(&s.id).copied().unwrap_or(RowState::Idle), running: true, pr: None })
                .collect();
            let input = Tree {
                projects: &projects,
                sessions: &sessions,
                pinned: &self.pinned,
                folded: &self.folded,
                removing: &self.removing,
                idle_open: &self.idle_open,
                selected: self.selected.as_deref(),
            };
            tree(&input, |s| s.title.clone())
        }
    }

    /// The rows in a short form: "P name", "W label", "S id", "R label", "M n", "I label".
    fn outline(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                TreeRow::Project { name, .. } => format!("P {name}"),
                TreeRow::Worktree { label, .. } => format!("W {label}"),
                TreeRow::Session { id, .. } => format!("S {id}"),
                TreeRow::Removing { label, .. } => format!("R {label}"),
                TreeRow::MoreWorktrees { count, .. } => format!("M {count}"),
                TreeRow::IdleWorktree { label, .. } => format!("I {label}"),
            })
            .collect()
    }

    fn two_projects() -> Fixture {
        let mut f = Fixture::new(
            vec![project("p1", "/code/app"), project("p2", "/code/site")],
            vec![
                session("a", "p1", "/code/app-wt/one", "feat/one", AgentKind::Claude, 10),
                session("b", "p1", "/code/app-wt/two", "feat/two", AgentKind::Codex, 30),
                session("c", "p1", "/code/app-wt/one", "feat/one", AgentKind::Shell, 20),
                session("d", "p2", "/code/site", "main", AgentKind::Shell, 5),
            ],
        );
        f.worktrees = vec![
            ("p1".into(), vec![worktree("/code/app", "main"), worktree("/code/app-wt/one", "feat/one"), worktree("/code/app-wt/two", "feat/two"), worktree("/code/app-wt/old", "old")]),
            ("p2".into(), vec![worktree("/code/site", "main")]),
        ];
        f
    }

    /// The labels of the worktree rows with sessions, in order.
    fn worktree_labels(rows: &[TreeRow]) -> Vec<String> {
        rows.iter().filter_map(|r| if let TreeRow::Worktree { label, .. } = r { Some(label.clone()) } else { None }).collect()
    }

    #[test]
    fn worktrees_go_pinned_first_then_newest_and_list_their_sessions() {
        let mut f = two_projects();
        assert_eq!(outline(&f.rows()), ["P p1", "W feat/two", "S b", "W feat/one", "S a", "S c", "M 2", "P p2", "W main", "S d"]);
        // A pinned worktree goes first, though its sessions are older.
        f.pinned = vec![PathBuf::from("/code/app-wt/one")];
        assert_eq!(worktree_labels(&f.rows()), ["feat/one", "feat/two", "main"]);
        // Two pinned: in pin order.
        f.pinned = vec![PathBuf::from("/code/app-wt/one"), PathBuf::from("/code/app-wt/two")];
        assert_eq!(worktree_labels(&f.rows()), ["feat/one", "feat/two", "main"]);
        f.pinned = vec![PathBuf::from("/code/app-wt/two"), PathBuf::from("/code/app-wt/one")];
        assert_eq!(worktree_labels(&f.rows()), ["feat/two", "feat/one", "main"]);
        let pinned: Vec<bool> = f.rows().iter().filter_map(|r| if let TreeRow::Worktree { pinned, .. } = r { Some(*pinned) } else { None }).collect();
        assert_eq!(pinned, [true, true, false]);
    }

    #[test]
    fn folded_keys_hide_what_is_under_them() {
        let mut f = two_projects();
        f.folded = vec![worktree_key("p1", Path::new("/code/app-wt/one")), project_key("p2")];
        assert_eq!(outline(&f.rows()), ["P p1", "W feat/two", "S b", "W feat/one", "M 2", "P p2"]);
        let folds: Vec<bool> = f
            .rows()
            .iter()
            .filter_map(|r| match r {
                TreeRow::Project { folded, .. } | TreeRow::Worktree { folded, .. } => Some(*folded),
                _ => None,
            })
            .collect();
        assert_eq!(folds, [false, false, true, true]);
        // A worktree key is no project key, and two worktrees have two keys.
        assert_ne!(worktree_key("p1", Path::new("p1")), project_key("p1"));
        assert_ne!(worktree_key("p1", Path::new("/a")), worktree_key("p1", Path::new("/b")));
        assert_ne!(worktree_key("p1", Path::new("/a")), worktree_key("p2", Path::new("/a")));
        assert_eq!(key_project(&worktree_key("p1", Path::new("/a|b"))), "p1");
    }

    #[cfg(windows)]
    #[test]
    fn a_worktree_has_one_key_whichever_way_its_path_is_written() {
        // Git's list and a session's folder, before and after git has listed the worktrees.
        assert_eq!(worktree_key("p1", Path::new(r"C:\code\app-wt\one")), worktree_key("p1", Path::new("C:/code/app-wt/one")));
    }

    #[test]
    fn a_project_folded_by_the_old_build_stays_folded() {
        let mut f = two_projects();
        f.projects[1].expanded = false;
        assert_eq!(outline(&f.rows()).last().map(String::as_str), Some("P p2"));
        assert!(project_folded(&[], &f.projects[1]));
        // Opened in the tree: the key goes and `expanded` comes back.
        f.projects[1].expanded = true;
        assert!(!project_folded(&f.folded, &f.projects[1]));
    }

    #[test]
    fn folding_keeps_one_key_and_prunes_projects_that_are_gone() {
        let mut folded = Vec::new();
        set_folded(&mut folded, "p1", true);
        set_folded(&mut folded, "p1", true);
        set_folded(&mut folded, &worktree_key("p1", Path::new("/x")), true);
        set_folded(&mut folded, &worktree_key("gone", Path::new("/y")), true);
        set_folded(&mut folded, "gone", true);
        assert_eq!(folded.len(), 4);
        prune_folded(&mut folded, &["p1", "p2"]);
        assert_eq!(folded, vec!["p1".to_string(), worktree_key("p1", Path::new("/x"))]);
        set_folded(&mut folded, "p1", false);
        assert_eq!(folded, vec![worktree_key("p1", Path::new("/x"))]);
    }

    #[test]
    fn a_worktree_has_the_most_urgent_state_of_its_sessions() {
        let mut f = two_projects();
        f.states.insert("a".into(), RowState::Working);
        f.states.insert("c".into(), RowState::NeedsYou);
        let rows = f.rows();
        let state_of = |label: &str| {
            rows.iter().find_map(|r| match r {
                TreeRow::Worktree { label: l, state, .. } if l == label => Some(*state),
                _ => None,
            })
        };
        assert_eq!(state_of("feat/one"), Some(RowState::NeedsYou));
        assert_eq!(state_of("feat/two"), Some(RowState::Idle));
        // The project counts the sessions that need the person, and all its worktrees.
        assert!(matches!(rows[0], TreeRow::Project { need: 1, worktrees: 4, .. }));
    }

    #[test]
    fn a_worktree_is_named_after_its_agent_session_and_a_folder_without_git_says_so() {
        let mut f = two_projects();
        // The shell came first in the worktree, the Claude session names it.
        f.sessions[0].branch = "feat/claude".into();
        f.sessions[2].branch = "feat/shell".into();
        f.sessions.swap(0, 2);
        assert!(outline(&f.rows()).contains(&"W feat/claude".to_string()));
        // Git listed nothing: the project's folder has no git.
        f.worktrees[1].1.clear();
        let rows = f.rows();
        assert!(rows.iter().any(|r| matches!(r, TreeRow::Worktree { label, git: false, .. } if label == "Folder · no git")));
    }

    #[test]
    fn a_project_with_no_worktree_listed_shows_its_folder() {
        // Before git has listed the worktrees, and with no session: the root, under "1 more".
        let mut f = Fixture::new(vec![project("p1", "/code/app")], Vec::new());
        assert_eq!(outline(&f.rows()), ["P p1", "M 1"]);
        assert!(matches!(f.rows()[0], TreeRow::Project { worktrees: 1, need: 0, .. }));
        f.idle_open.insert("p1".into());
        assert_eq!(outline(&f.rows()), ["P p1", "M 1", "I app"]);
        // A project with no project folder at all still has its row.
        let f = Fixture::new(vec![project("p1", "")], Vec::new());
        assert_eq!(outline(&f.rows())[0], "P p1");
    }

    #[test]
    fn removed_worktrees_say_so_and_idle_ones_fold_away() {
        let mut f = two_projects();
        f.removing.insert(PathBuf::from("/code/app-wt/two"));
        assert_eq!(outline(&f.rows())[..6], ["P p1", "W feat/one", "S a", "S c", "R feat/two", "M 2"].map(String::from));
        f.idle_open.insert("p1".into());
        assert_eq!(outline(&f.rows())[6..8], ["I main", "I old"].map(String::from));
        // Removing worktrees are not counted.
        assert!(matches!(f.rows()[0], TreeRow::Project { worktrees: 3, .. }));
    }

    #[test]
    fn the_selected_session_marks_its_row_worktree_and_project() {
        let mut f = two_projects();
        f.selected = Some("c".into());
        let rows = f.rows();
        assert!(matches!(&rows[0], TreeRow::Project { current: true, .. }));
        assert!(rows.iter().any(|r| matches!(r, TreeRow::Worktree { label, current: true, .. } if label == "feat/one")));
        assert!(rows.iter().any(|r| matches!(r, TreeRow::Session { id, selected: true, .. } if id == "c")));
        assert!(rows.iter().any(|r| matches!(r, TreeRow::Project { id, current: false, .. } if id == "p2")));
    }

    #[test]
    fn a_long_title_is_cut_on_a_character_and_kept_on_one_line() {
        let long: String = "Überprüfung ".repeat(17); // 204 characters, many of two bytes
        assert_eq!(long.chars().count(), 204);
        let mut f = two_projects();
        f.sessions[3].title = format!("{long}\nsecond line");
        let rows = f.rows();
        let title = rows.iter().find_map(|r| if let TreeRow::Session { id, title, .. } = r { (id == "d").then(|| title.clone()) } else { None }).unwrap();
        assert_eq!(title.chars().count(), TITLE_MAX + 1);
        assert!(title.ends_with('…') && !title.contains('\n'));
        assert_eq!(row_title("short  title"), "short title");
    }

    #[test]
    fn many_rows_build_quickly() {
        // 15 projects with 4 worktrees each and two sessions in each worktree.
        let projects: Vec<Project> = (0..15).map(|p| project(&format!("p{p}"), &format!("/code/p{p}"))).collect();
        let mut sessions = Vec::new();
        for p in 0..15 {
            for w in 0..4 {
                for s in 0..2 {
                    sessions.push(session(&format!("s{p}-{w}-{s}"), &format!("p{p}"), &format!("/code/p{p}-wt/{w}"), &format!("b{w}"), AgentKind::Shell, (p * 10 + w) as u64));
                }
            }
        }
        let f = Fixture::new(projects, sessions);
        let started = std::time::Instant::now();
        let rows = f.rows();
        assert_eq!(rows.len(), 15 * (1 + 4 + 8 + 1));
        // Far above what it takes (well under a millisecond), to fail only on a slow algorithm.
        assert!(started.elapsed() < Duration::from_millis(250), "{:?}", started.elapsed());
    }
}
