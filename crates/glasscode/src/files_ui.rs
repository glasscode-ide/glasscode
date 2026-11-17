//! The Files tab: the session's worktree as a tree, like an editor's project panel. Folders
//! open one at a time (each read when opened); names carry git's status (modified, added,
//! deleted, conflicted, untracked), folders count the changes inside them, and what git
//! ignores is dimmed and not listed. A filter finds any file, "Changed" shows only the changed
//! ones, and the file the agent edited last is one click away. Clicking a file opens it in a tab
//! of the centre (`file_view.rs`; its diff when it changed), which has Open in editor, Copy path
//! and Mention in session; the row of the file in front is marked.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, AppContext as _, ClickEvent, ClipboardItem, Context, Entity, FontWeight, Hsla, MouseButton, MouseDownEvent, SharedString, Transformation, Window, deferred, div,
    percentage, prelude::*, px,
};
use glasscode_core::git::{self, DirEntry};

use crate::file_view::FileTabs;
use crate::icons;
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::workspace::{RightTab, Workspace};

/// How often the open folders, ignored paths and the agent's last edit are read again.
const REFRESH_EVERY: Duration = Duration::from_secs(4);
/// A filtered tree shows at most this many rows.
const MAX_ROWS: usize = 600;

#[derive(Default)]
pub struct FilesUi {
    /// The worktree this state is about (it starts over when the session changes).
    pub(crate) root: Option<PathBuf>,
    /// Open folders ("" is the worktree itself, always open).
    open: HashSet<String>,
    /// Folders read so far.
    dirs: HashMap<String, Vec<DirEntry>>,
    /// What git ignores (folders end with "/").
    ignored: Vec<String>,
    /// Every file git tracks or would add, for the filter (read when the filter is used).
    all_files: Option<Vec<String>>,
    loading_all: bool,
    refreshed: Option<Instant>,
    refreshing: bool,
    query: Option<Entity<TextInput>>,
    changed_only: bool,
    /// The row whose ⋯ menu is open.
    menu: Option<String>,
    /// The file the session's agent edited last, and when (Unix seconds).
    pub(crate) last_edit: Option<(String, i64)>,
    /// The files open in tabs of the centre, of every worktree (they outlive a start over).
    pub(crate) tabs: FileTabs,
    /// A file `reveal` asked to open; the next tick opens its tab.
    wanted: Option<String>,
}

impl FilesUi {
    /// Show `path` (relative to the worktree) in the tree and open it in a tab of the centre at
    /// the next tick. For the debug hook `code`; a click goes through `files_select`.
    pub fn reveal(&mut self, path: &str) {
        self.show_in_tree(path);
        self.wanted = Some(path.to_string());
    }

    /// Open the folders on the way to `path`.
    fn show_in_tree(&mut self, path: &str) {
        let mut parent = String::new();
        for part in path.split('/').collect::<Vec<_>>().split_last().map(|(_, dirs)| dirs.to_vec()).unwrap_or_default() {
            parent = if parent.is_empty() { part.to_string() } else { format!("{parent}/{part}") };
            self.open.insert(parent.clone());
        }
        self.menu = None;
        // Read the folders on the way at the next tick.
        self.refreshed = None;
    }

    /// Debug screenshots: start with only the changed files.
    pub fn debug_changed_only(&mut self) {
        self.changed_only = true;
    }

    fn query_text(&self, cx: &gpui::App) -> String {
        self.query.as_ref().map(|q| q.read(cx).text().trim().to_string()).unwrap_or_default()
    }
}

/// A row of the tree.
struct Row {
    path: String,
    name: String,
    depth: usize,
    is_dir: bool,
    open: bool,
    ignored: bool,
    /// Git's letter for a file (M, A, D, R, U for a conflict, ? untracked).
    status: Option<char>,
    /// Changes inside a closed folder, and the most urgent one's letter.
    count: usize,
    worst: Option<char>,
    note: Option<&'static str>,
}

pub(crate) fn status_color(status: char) -> Hsla {
    match status {
        'A' | '?' => theme::added(),
        'D' | 'U' => theme::removed(),
        _ => theme::modified(),
    }
}

fn status_letter(status: char) -> &'static str {
    match status {
        'A' => "A",
        'D' => "D",
        'R' => "R",
        'U' => "!",
        '?' => "U",
        _ => "M",
    }
}

pub(crate) fn status_name(status: char) -> &'static str {
    match status {
        'A' => "Added",
        'D' => "Deleted",
        'R' => "Renamed",
        'U' => "Conflict",
        '?' => "Untracked",
        _ => "Modified",
    }
}

/// Conflicts first, then deletions, edits, additions, new files.
fn status_rank(status: char) -> u8 {
    match status {
        'U' => 0,
        'D' => 1,
        'M' | 'R' => 2,
        'A' => 3,
        _ => 4,
    }
}

/// A small text badge for a file's type, like an editor's file icons.
fn file_glyph(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    if lower.ends_with(".lock") || lower == "package-lock.json" || lower == "pnpm-lock.yaml" {
        return "LCK";
    }
    if lower.starts_with(".env") {
        return "ENV";
    }
    if lower == ".gitignore" || lower == ".gitattributes" {
        return "GIT";
    }
    if lower == "dockerfile" {
        return "DKR";
    }
    match ext {
        "rs" => "RS",
        "ts" => "TS",
        "tsx" => "TSX",
        "js" | "mjs" | "cjs" => "JS",
        "jsx" => "JSX",
        "json" => "{ }",
        "toml" => "[T]",
        "yml" | "yaml" => "YML",
        "md" | "mdx" => "MD",
        "html" => "</>",
        "css" | "scss" => "CSS",
        "py" => "PY",
        "go" => "GO",
        "swift" => "SW",
        "sh" | "zsh" => "SH",
        "sql" => "SQL",
        "svg" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" => "IMG",
        _ => "",
    }
}

fn file_icon(name: &str, color: Hsla) -> AnyElement {
    let glyph = file_glyph(name);
    if glyph.is_empty() {
        return icons::icon(icons::FILE, theme::sz(14.), theme::sz(14.), color).into_any_element();
    }
    div().font_family(theme::MONO_FONT).text_size(theme::sz(8.)).font_weight(FontWeight::BOLD).text_color(color).child(glyph).into_any_element()
}

fn folder_icon(color: Hsla) -> AnyElement {
    icons::icon(icons::FOLDER, theme::sz(15.), theme::sz(15.), color).into_any_element()
}

/// Sort paths into tree order (folders before files at each level) and turn them into rows,
/// every folder on the way open.
fn tree_rows(paths: &[String], changes: &HashMap<String, char>) -> Vec<Row> {
    // A path's sort key: each folder with a 0 in front, the file with a 1.
    let key = |p: &String| -> Vec<(u8, String)> {
        let parts: Vec<&str> = p.split('/').collect();
        parts.iter().enumerate().map(|(i, part)| (u8::from(i + 1 == parts.len()), part.to_lowercase())).collect()
    };
    let mut sorted: Vec<&String> = paths.iter().collect();
    sorted.sort_by_key(|p| key(p));
    let mut rows: Vec<Row> = Vec::new();
    let mut shown: HashSet<String> = HashSet::new();
    for path in sorted {
        let parts: Vec<&str> = path.split('/').collect();
        let mut parent = String::new();
        for (depth, part) in parts.iter().enumerate().take(parts.len().saturating_sub(1)) {
            parent = if parent.is_empty() { part.to_string() } else { format!("{parent}/{part}") };
            if shown.insert(parent.clone()) {
                rows.push(Row { path: parent.clone(), name: part.to_string(), depth, is_dir: true, open: true, ignored: false, status: None, count: 0, worst: None, note: None });
            }
        }
        rows.push(Row {
            path: path.clone(),
            name: parts.last().map(|s| s.to_string()).unwrap_or_default(),
            depth: parts.len() - 1,
            is_dir: false,
            open: false,
            ignored: false,
            status: changes.get(path).copied(),
            count: 0,
            worst: None,
            note: None,
        });
        if rows.len() >= MAX_ROWS {
            break;
        }
    }
    rows
}

impl Workspace {
    fn files_changes(&self) -> HashMap<String, char> {
        self.changes.iter().map(|c| (c.path.clone(), c.status)).collect()
    }

    /// The tree starts over for another worktree. The filter box stays, emptied, and so do the
    /// file tabs of the centre and a file asked for.
    fn files_start_over(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let files = std::mem::take(&mut self.gh.files);
        self.gh.files = FilesUi { root: Some(root), query: files.query, tabs: files.tabs, wanted: files.wanted, ..Default::default() };
        if let Some(q) = self.gh.files.query.clone() {
            q.update(cx, |q, cx| q.set_text("", cx));
        }
    }

    /// Every tick: the file tab in front follows its file (whatever the right panel shows), and
    /// the tree keeps in step with the worktree while the Files tab shows.
    pub fn files_tick(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.gh.files.wanted.take() {
            self.files_select(path, cx);
        }
        self.file_tabs_tick(cx);
        if !self.show_right || self.right_tab != RightTab::Files {
            return;
        }
        let Some(session) = self.active_session().cloned() else { return };
        match &self.gh.files.root {
            // Another session's worktree: start over.
            Some(root) if *root != session.worktree => self.files_start_over(session.worktree.clone(), cx),
            Some(_) => {}
            // The first time: keep what was revealed before the tab first showed.
            None => self.gh.files.root = Some(session.worktree.clone()),
        }
        if self.gh.files.query.is_none() {
            let input = cx.new(|cx| TextInput::new("Find a file", cx));
            cx.subscribe(&input, |this: &mut Workspace, input, event: &InputEvent, cx| match event {
                InputEvent::Changed => {
                    if !input.read(cx).text().trim().is_empty() {
                        this.files_load_all(cx);
                    }
                    cx.notify();
                }
                InputEvent::Submit => {
                    // Enter opens the first match.
                    let query = input.read(cx).text().trim().to_lowercase();
                    let first = this.gh.files.all_files.as_ref().and_then(|all| all.iter().find(|p| p.to_lowercase().contains(&query)).cloned());
                    if let Some(path) = first {
                        this.files_select(path, cx);
                    }
                }
                InputEvent::Cancel => {
                    input.update(cx, |i, cx| i.set_text("", cx));
                }
            })
            .detach();
            self.gh.files.query = Some(input);
        }
        let stale = self.gh.files.refreshed.is_none_or(|t| t.elapsed() >= REFRESH_EVERY);
        if stale && !self.gh.files.refreshing {
            self.files_refresh(cx);
        }
    }

    /// Read the open folders, what git ignores and the agent's last edit again.
    fn files_refresh(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.active_session().cloned() else { return };
        let root = session.worktree.clone();
        let mut folders: Vec<String> = self.gh.files.open.iter().cloned().collect();
        folders.push(String::new());
        let asked: HashSet<String> = folders.iter().cloned().collect();
        let transcript = session.transcript_path.clone();
        let with_all = self.gh.files.all_files.is_some();
        self.gh.files.refreshing = true;
        self.gh.files.refreshed = Some(Instant::now());
        let task = cx.background_spawn(async move {
            let ignored = git::ignored_paths(&root).unwrap_or_default();
            let dirs: Vec<(String, Vec<DirEntry>)> = folders
                .into_iter()
                .filter(|f| f.is_empty() || !git::is_ignored(&ignored, f))
                .filter_map(|f| git::list_dir(&root, &f).ok().map(|list| (f, list)))
                .collect();
            let all = if with_all { git::listed_files(&root).ok() } else { None };
            let last_edit = transcript.and_then(|t| last_edited_file(&t, &root));
            (root, ignored, dirs, all, last_edit)
        });
        cx.spawn(async move |this, cx| {
            let (root, ignored, dirs, all, last_edit) = task.await;
            let _ = this.update(cx, |this, cx| {
                let files = &mut this.gh.files;
                files.refreshing = false;
                if files.root.as_ref() != Some(&root) {
                    return;
                }
                files.ignored = ignored;
                // Folders that are gone close by themselves; one opened while this read ran (a file
                // shown from Changes or Jump) stays open.
                let read: HashSet<String> = dirs.iter().map(|(f, _)| f.clone()).collect();
                files.open.retain(|f| !asked.contains(f) || read.contains(f) || git::is_ignored(&files.ignored, f));
                for (folder, list) in dirs {
                    files.dirs.insert(folder, list);
                }
                if all.is_some() {
                    files.all_files = all;
                }
                files.last_edit = last_edit;
                cx.notify();
            });
        })
        .detach();
    }

    /// The list of every file, for the filter.
    fn files_load_all(&mut self, cx: &mut Context<Self>) {
        if self.gh.files.all_files.is_some() || self.gh.files.loading_all {
            return;
        }
        let Some(root) = self.gh.files.root.clone() else { return };
        self.gh.files.loading_all = true;
        let task = cx.background_spawn(async move { git::listed_files(&root).unwrap_or_default() });
        cx.spawn(async move |this, cx| {
            let all = task.await;
            let _ = this.update(cx, |this, cx| {
                this.gh.files.loading_all = false;
                this.gh.files.all_files = Some(all);
                cx.notify();
            });
        })
        .detach();
    }

    fn files_toggle(&mut self, path: String, cx: &mut Context<Self>) {
        let files = &mut self.gh.files;
        files.menu = None;
        if !files.open.remove(&path) {
            files.open.insert(path.clone());
            if !files.dirs.contains_key(&path) && !git::is_ignored(&files.ignored, &path) {
                if let Some(root) = files.root.clone() {
                    let folder = path.clone();
                    let task = cx.background_spawn(async move { git::list_dir(&root, &folder).ok() });
                    cx.spawn(async move |this, cx| {
                        let list = task.await;
                        let _ = this.update(cx, |this, cx| {
                            if let Some(list) = list {
                                this.gh.files.dirs.insert(path, list);
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                }
            }
        }
        cx.notify();
    }

    /// Open a file of the session's worktree in a tab of the centre (its diff when it changed),
    /// or bring its tab to the front, and show it in the tree. A click in Files or Changes,
    /// Enter in the find box or in Jump, and ◎ come here.
    pub fn files_select(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(worktree) = self.active_session().map(|s| s.worktree.clone()) else { return };
        let path = crate::file_view::relative(&path);
        // A tree of another worktree would start over at its next tick and close the folders
        // opened below: it starts over now.
        if self.gh.files.root.as_ref().is_some_and(|root| *root != worktree) {
            self.files_start_over(worktree, cx);
        }
        self.gh.files.show_in_tree(&path);
        self.open_file_tab(&path, cx);
    }

    /// Put "@path " in the session's prompt (no Enter), as typing it would.
    pub(crate) fn files_mention(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.active_session().cloned() else { return };
        let Some(terminal) = self.terminals.get(&session.id).cloned() else {
            self.show_toast("Open the session first; its agent is not running.", cx);
            return;
        };
        // Typing into an open question would answer it.
        if glasscode_core::prompt::parse(&terminal.read(cx).text_tail(60)).is_some() {
            self.show_toast("The agent is asking something in the terminal. Answer it first, then mention the file.", cx);
            return;
        }
        // Pasted, so the agent's own file picker does not open over it.
        terminal.update(cx, |t, cx| t.paste_text(&format!("@{path} "), cx));
        self.gh.files.menu = None;
        // The session comes in front of a file tab, with the keyboard in its prompt.
        self.gh.files.tabs.session_to_front();
        self.focus_active(window, cx);
        self.show_toast(format!("@{path} is in the session's prompt."), cx);
    }

    /// `path` relative to the session's worktree, or its full path, on the clipboard.
    pub(crate) fn files_copy(&mut self, path: &str, absolute: bool, cx: &mut Context<Self>) {
        // The session's worktree: the tree's root is not set before the Files tab first shows.
        let root = self.active_session().map(|s| s.worktree.clone());
        let text = match (root, absolute) {
            (Some(root), true) => crate::file_view::full_path(&root, path).display().to_string(),
            _ => path.to_string(),
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.gh.files.menu = None;
        self.show_toast(format!("Copied {text}"), cx);
    }

    fn files_reveal_in_finder(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(root) = &self.gh.files.root {
            let full = crate::file_view::full_path(root, path);
            // Out of the click handler that asked (`center::start_outside`).
            crate::center::start_or_toast(format!("Could not show {path} in {}", crate::platform::file_manager()), move || glasscode_core::sys::reveal_path(&full), cx);
        }
        self.gh.files.menu = None;
        cx.notify();
    }

    /// The editor "Open in" uses (the same one the session's Open in editor picks).
    pub(crate) fn files_editor_name(&self) -> &'static str {
        self.preferred_editor().map(|(name, _)| name).unwrap_or(crate::platform::file_manager())
    }

    // ----- drawing -------------------------------------------------------------------------

    fn files_rows(&self, query: &str) -> Vec<Row> {
        let files = &self.gh.files;
        let changes = self.files_changes();
        if !query.is_empty() || files.changed_only {
            let q = query.to_lowercase();
            let mut paths: Vec<String> = if files.changed_only {
                changes.keys().cloned().collect()
            } else {
                files.all_files.clone().unwrap_or_default()
            };
            if !q.is_empty() {
                paths.retain(|p| p.to_lowercase().contains(&q));
            }
            return tree_rows(&paths, &changes);
        }
        let mut rows: Vec<Row> = Vec::new();
        self.files_walk("", 0, &changes, &mut rows);
        rows
    }

    fn files_walk(&self, folder: &str, depth: usize, changes: &HashMap<String, char>, rows: &mut Vec<Row>) {
        let files = &self.gh.files;
        let Some(list) = files.dirs.get(folder) else {
            rows.push(Row { path: format!("{folder}/…"), name: String::new(), depth, is_dir: false, open: false, ignored: false, status: None, count: 0, worst: None, note: Some("Reading…") });
            return;
        };
        let join = |name: &str| if folder.is_empty() { name.to_string() } else { format!("{folder}/{name}") };
        let mut entries: Vec<DirEntry> = list.clone();
        // Deleted files are gone from the disk; they still show, struck through.
        for change in self.changes.iter().filter(|c| c.status == 'D') {
            let (parent, name) = change.path.rsplit_once('/').unwrap_or(("", change.path.as_str()));
            if parent == folder && !entries.iter().any(|e| e.name == name) {
                entries.push(DirEntry { name: name.to_string(), is_dir: false });
            }
        }
        git::sort_entries(&mut entries);
        for entry in entries {
            if rows.len() >= MAX_ROWS * 4 {
                return;
            }
            let path = join(&entry.name);
            let ignored = git::is_ignored(&files.ignored, &path);
            if entry.is_dir {
                let open = files.open.contains(&path);
                let prefix = format!("{path}/");
                let inside: Vec<char> = changes.iter().filter(|(p, _)| p.starts_with(&prefix)).map(|(_, s)| *s).collect();
                let worst = inside.iter().copied().min_by_key(|s| status_rank(*s));
                rows.push(Row { path: path.clone(), name: entry.name.clone(), depth, is_dir: true, open, ignored, status: None, count: if open { 0 } else { inside.len() }, worst, note: None });
                if open {
                    if ignored {
                        rows.push(Row { path: format!("{path}/…"), name: String::new(), depth: depth + 1, is_dir: false, open: false, ignored: true, status: None, count: 0, worst: None, note: Some("Ignored by git: not listed") });
                    } else {
                        self.files_walk(&path, depth + 1, changes, rows);
                    }
                }
            } else {
                rows.push(Row { status: changes.get(&path).copied(), path, name: entry.name, depth, is_dir: false, open: false, ignored, count: 0, worst: None, note: None });
            }
        }
    }

    fn files_row(&self, row: Row, editing: Option<&str>, front: Option<&str>, cx: &mut Context<Self>) -> AnyElement {
        let indent = 8. + row.depth as f32 * 14.;
        let guides: Vec<AnyElement> = (0..row.depth).map(|i| div().absolute().top_0().bottom_0().left(theme::sz(14. + i as f32 * 14.)).w(px(1.)).bg(theme::border()).into_any_element()).collect();
        if let Some(note) = row.note {
            return div()
                .relative()
                .h(theme::sz(24.))
                .flex()
                .items_center()
                .pl(theme::sz(indent + 18.))
                .text_size(theme::sz(12.))
                .italic()
                .text_color(theme::faint())
                .children(guides)
                .child(note)
                .into_any_element();
        }
        // The file in front of the centre.
        let selected = !row.is_dir && front == Some(row.path.as_str());
        let color = match (row.status, row.ignored) {
            (Some(s), _) => status_color(s),
            (None, true) => theme::faint(),
            _ => theme::text(),
        };
        let icon_color = if row.ignored { theme::faint() } else { theme::muted() };
        let group = SharedString::from(format!("frow-{}", row.path));
        let path = row.path.clone();
        let is_dir = row.is_dir;
        let menu_open = self.gh.files.menu.as_deref() == Some(row.path.as_str());
        let menu_path = row.path.clone();
        let right_path = row.path.clone();
        let editing_here = editing == Some(row.path.as_str());
        let line = div()
            .id(SharedString::from(format!("frow-{}", row.path)))
            .group(group.clone())
            .relative()
            .flex()
            .items_center()
            .gap_1p5()
            .h(theme::sz(26.))
            .pl(theme::sz(indent))
            .pr(theme::sz(30.))
            .rounded_md()
            .cursor_pointer()
            .when(selected, |d| d.bg(theme::selected()).border_1().border_color(theme::accent()))
            .when(!selected, |d| d.border_1().border_color(gpui::transparent_black()).hover(|s| s.bg(theme::hover())))
            .children(guides)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if is_dir {
                    this.files_toggle(path.clone(), cx);
                } else {
                    this.gh.files.menu = None;
                    this.files_select(path.clone(), cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.gh.files.menu = Some(right_path.clone());
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            // A folder's chevron points down when it is open, right when it is not.
            .child(div().flex_none().w(theme::sz(12.)).flex().justify_center().when(row.is_dir, |d| {
                d.child(icons::icon(icons::CHEVRON_DOWN, theme::sz(10.), theme::sz(10.), theme::faint()).when(!row.open, |s| s.with_transformation(Transformation::rotate(percentage(0.75)))))
            }))
            .child(div().flex_none().w(theme::sz(16.)).h(theme::sz(16.)).flex().items_center().justify_center().child(if row.is_dir { folder_icon(icon_color) } else { file_icon(&row.name, icon_color) }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(theme::sz(13.))
                    .text_color(color)
                    .when(row.status == Some('D'), |d| d.line_through())
                    .when(row.ignored, |d| d.opacity(0.75))
                    .child(row.name.clone()),
            )
            .when(editing_here, |d| d.child(div().flex_none().size(theme::sz(7.)).rounded_full().bg(theme::added())))
            .when(row.count > 0, |d| {
                let c = row.worst.map(status_color).unwrap_or_else(theme::muted);
                d.child(div().flex_none().min_w(theme::sz(16.)).h(theme::sz(16.)).px_1().rounded_full().border_1().border_color(c).flex().items_center().justify_center().text_size(theme::sz(10.)).font_weight(FontWeight::BOLD).text_color(c).child(row.count.to_string()))
            })
            .when_some(row.status, |d, s| d.child(div().flex_none().w(theme::sz(12.)).flex().justify_center().font_family(theme::MONO_FONT).text_size(theme::sz(11.)).font_weight(FontWeight::BOLD).text_color(status_color(s)).child(status_letter(s))))
            .child(
                div()
                    .absolute()
                    .right(theme::sz(3.))
                    .top(theme::sz(2.))
                    .invisible()
                    .group_hover(group, |s| s.visible())
                    .when(menu_open, |d| d.visible())
                    .child(
                        div()
                            .id(SharedString::from(format!("fmenu-{}", row.path)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(theme::sz(22.))
                            .rounded(theme::sz(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .child(icons::icon(icons::MORE, theme::sz(14.), theme::sz(14.), theme::muted()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                let open = this.gh.files.menu.as_deref() == Some(menu_path.as_str());
                                this.gh.files.menu = if open { None } else { Some(menu_path.clone()) };
                                cx.notify();
                            })),
                    ),
            );
        let menu = menu_open.then(|| self.files_menu(&row.path, row.is_dir, cx));
        div().relative().child(line).when_some(menu, |d, m| d.child(m)).into_any_element()
    }

    fn files_menu(&self, path: &str, is_dir: bool, cx: &mut Context<Self>) -> AnyElement {
        let editor = self.files_editor_name();
        let item = |id: &'static str, label: String| {
            div()
                .id(id)
                .px_2p5()
                .py_1p5()
                .rounded_md()
                .text_size(theme::sz(13.))
                .text_color(theme::text())
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(label)
        };
        let (p1, p2, p3, p4, p5) = (path.to_string(), path.to_string(), path.to_string(), path.to_string(), path.to_string());
        deferred(
            div()
                .id("fmenu")
                .occlude()
                .absolute()
                .right(theme::sz(4.))
                .top(theme::sz(27.))
                .w(theme::sz(210.))
                .p_1()
                .flex()
                .flex_col()
                .rounded_lg()
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::dialog())
                .shadow_lg()
                .child(item("fm-open", format!("Open in {editor}")).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.gh.files.menu = None;
                    this.open_in_editor(Some(p1.clone()), cx);
                })))
                .child(item("fm-finder", format!("Reveal in {}", crate::platform::file_manager())).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.files_reveal_in_finder(&p2, cx))))
                .child(item("fm-copy", "Copy path".into()).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.files_copy(&p3, true, cx))))
                .child(item("fm-copy-rel", "Copy relative path".into()).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.files_copy(&p4, false, cx))))
                .when(!is_dir, |d| d.child(item("fm-mention", "Mention in session".into()).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.files_mention(p5.clone(), window, cx))))),
        )
        .with_priority(4)
        .into_any_element()
    }

    /// The Files tab.
    pub fn render_files_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(session) = self.active_session() else {
            return div().p_4().text_color(theme::muted()).child("No session.").into_any_element();
        };
        let files = &self.gh.files;
        let query = files.query_text(cx);
        let root_name = session.worktree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let changed = self.changes.len();
        // The agent's file counts as "being edited" for two minutes.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let recent = files.last_edit.as_ref().filter(|(_, at)| now - at < 120).map(|(p, _)| p.as_str());
        let front = files.tabs.front_tab(&session.id).filter(|t| t.worktree == session.worktree).map(|t| t.path.as_str());
        let rows = self.files_rows(&query);
        let note = if !query.is_empty() {
            let n = rows.iter().filter(|r| !r.is_dir).count();
            Some(if files.all_files.is_none() { "Looking…".to_string() } else if n == 0 { format!("No file matches “{query}”") } else { format!("{n} file{} match", if n == 1 { "" } else { "s" }) })
        } else if files.changed_only {
            Some(if changed == 0 { format!("Nothing changed on {}", session.branch) } else { format!("{changed} changed file{} on {}", if changed == 1 { "" } else { "s" }, session.branch) })
        } else {
            None
        };
        let tool = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .gap_1()
                .h(theme::sz(30.))
                .min_w(theme::sz(30.))
                .px_1p5()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .text_size(theme::sz(12.5))
                .text_color(theme::text_soft())
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(label)
        };
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1p5()
            .px_2p5()
            .pt_2p5()
            .pb_1p5()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .children(files.query.clone())
                    // × empties the search box.
                    .when(!query.is_empty(), |d| {
                        d.child(
                            div()
                                .id("files-clear")
                                .absolute()
                                .right(theme::sz(8.))
                                .top(theme::sz(8.))
                                .size(theme::sz(20.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(theme::sz(10.))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::hover()))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    if let Some(q) = this.gh.files.query.clone() {
                                        q.update(cx, |q, cx| q.set_text("", cx));
                                    }
                                    cx.notify();
                                }))
                                .child(icons::icon(icons::CLOSE, theme::sz(12.), theme::sz(12.), theme::muted())),
                        )
                    }),
            )
            .child(
                tool("files-changed", "±")
                    .when(files.changed_only, |d| d.bg(theme::selected()).border_color(theme::accent()).text_color(theme::text_strong()))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(changed.to_string()))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.gh.files.changed_only = !this.gh.files.changed_only;
                        cx.notify();
                    })),
            )
            .when_some(files.last_edit.clone(), |d, (path, _)| {
                d.child(tool("files-reveal", "◎").text_color(if recent.is_some() { theme::added() } else { theme::text_soft() }).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.files_select(path.clone(), cx))))
            })
            .child(tool("files-collapse", "⊟").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.gh.files.open.clear();
                this.gh.files.menu = None;
                cx.notify();
            })));
        let empty = rows.is_empty() && note.is_none();
        let tree = div()
            .id("files-tree")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(theme::sz(120.))
            .overflow_y_scroll()
            .px_1p5()
            .pb_2p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .h(theme::sz(26.))
                    .px_2()
                    .text_size(theme::sz(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_strong())
                    .child(folder_icon(theme::muted()))
                    .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(root_name))
                    .child(div().flex_none().font_family(theme::MONO_FONT).text_size(theme::sz(11.)).font_weight(FontWeight::NORMAL).text_color(theme::faint()).child(format!("⎇ {}", session.branch))),
            )
            .children(rows.into_iter().map(|row| self.files_row(row, recent, front, cx)))
            .when(empty, |d| d.child(div().px_2p5().py_4().text_size(theme::sz(13.)).text_color(theme::muted()).child("This folder is empty.")));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(toolbar)
            .when_some(note, |d, n| d.child(div().flex_none().px_3p5().pb_1().text_size(theme::sz(12.)).text_color(theme::muted()).child(n)))
            .child(tree)
            .into_any_element()
    }
}

/// The file the agent edited last, from its transcript (Claude's Edit / Write / MultiEdit
/// tool calls), relative to the worktree, with when.
fn last_edited_file(transcript: &std::path::Path, worktree: &std::path::Path) -> Option<(String, i64)> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(transcript).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(256 * 1024);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = String::new();
    file.take(256 * 1024).read_to_string(&mut tail).ok()?;
    for line in tail.lines().rev() {
        if !line.contains("\"tool_use\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(content) = value["message"]["content"].as_array() else { continue };
        for item in content.iter().rev() {
            let name = item["name"].as_str().unwrap_or_default();
            if item["type"] != "tool_use" || !matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
                continue;
            }
            let Some(path) = item["input"]["file_path"].as_str().or_else(|| item["input"]["notebook_path"].as_str()) else { continue };
            // With "/" as git and the tree write it (Windows paths have "\").
            let relative = crate::file_view::relative(&std::path::Path::new(path).strip_prefix(worktree).ok()?.to_string_lossy());
            let at = value["timestamp"].as_str().and_then(glasscode_core::github::parse_time).unwrap_or(0);
            return Some((relative, at));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_paths_make_a_tree() {
        let paths: Vec<String> = ["services/iam/src/mcp-oauth.ts", "README.md", "services/iam/package.json", "apps/web/page.tsx"].iter().map(|s| s.to_string()).collect();
        let changes: HashMap<String, char> = [("README.md".to_string(), 'M')].into_iter().collect();
        let rows = tree_rows(&paths, &changes);
        let shown: Vec<(usize, &str, bool)> = rows.iter().map(|r| (r.depth, r.name.as_str(), r.is_dir)).collect();
        assert_eq!(
            shown,
            [(0, "apps", true), (1, "web", true), (2, "page.tsx", false), (0, "services", true), (1, "iam", true), (2, "src", true), (3, "mcp-oauth.ts", false), (2, "package.json", false), (0, "README.md", false)]
        );
        assert_eq!(rows.last().unwrap().status, Some('M'));
    }

    #[test]
    fn finds_the_last_edit() {
        let dir = std::env::temp_dir().join(format!("glasscode-files-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("t.jsonl");
        let lines = [
            r#"{"type":"assistant","timestamp":"2026-10-20T10:00:00Z","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/w/src/a.rs"}}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-10-20T10:05:00Z","message":{"content":[{"type":"text","text":"done"},{"type":"tool_use","name":"Write","input":{"file_path":"/w/src/b.rs"}}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-10-20T10:06:00Z","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
        ];
        std::fs::write(&transcript, lines.join("\n")).unwrap();
        let (path, at) = last_edited_file(&transcript, std::path::Path::new("/w")).unwrap();
        assert_eq!(path, "src/b.rs");
        assert_eq!(at, glasscode_core::github::parse_time("2026-10-20T10:05:00Z").unwrap());
        assert!(last_edited_file(&transcript, std::path::Path::new("/elsewhere")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn glyphs_for_types() {
        assert_eq!(file_glyph("mcp-oauth.ts"), "TS");
        assert_eq!(file_glyph("Cargo.lock"), "LCK");
        assert_eq!(file_glyph(".env.local"), "ENV");
        assert_eq!(file_glyph("README.md"), "MD");
        assert_eq!(file_glyph("notes"), "");
    }
}
