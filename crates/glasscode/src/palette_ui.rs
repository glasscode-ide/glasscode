//! Jump, the command palette (its shortcut, or the search in the title bar): type a few letters
//! of a session, a worktree, a project, a file of the active session's worktree, a command, a
//! ticket or a review request, and Enter (or a click) goes there. ↑ and ↓ move. The results come
//! in groups (Sessions, Worktrees, Projects, Files, Commands, Tickets), each with a cap, so that
//! one group cannot push the others out of the list.
//!
//! `GLASSCODE_DEBUG_OPEN=palette[:light|dark|classic|contrast][:90|100|115|130][:<query>]` opens
//! it in a debug build: the theme and the scale are set as the Themes page sets them (so they are
//! saved), and the other words are typed.

use std::path::PathBuf;

use gpui::{AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FontWeight, Pixels, ScrollHandle, SharedString, Window, deferred, div, point, prelude::*, relative};
use glasscode_core::git;
use glasscode_core::model::{AgentKind, Session, ThemeChoice, UiScale};

use crate::icons;
use crate::keymap;
use crate::shell::View;
use crate::sidebar::RowState;
use crate::text_input::{self, InputEvent, TextInput};
use crate::theme;
use crate::tickets_ui::Key;
use crate::ui;
use crate::workspace::{
    AddProject, GoBack, GoForward, Handoff, NewSession, OpenSettings, PaletteDown, PaletteUp, ToggleLeftPanel, ToggleRightPanel, Workspace,
};

/// The most files of a worktree that Jump holds (git's sorted list, cut there). Each typed key
/// looks at each of them once; the test below types over this many.
const FILES_MAX: usize = 20_000;
/// The most files a query that names the Files group shows ("files").
const FILES_NAMED: usize = 100;
/// Heights in the list before the scale: a group's heading, a row, the padding above and below.
/// They are fixed, so that the place of a row is known without a layout (to keep it in view).
const HEADING_H: f32 = 28.;
const ROW_H: f32 = 36.;
const LIST_PAD: f32 = 6.;
/// The list scrolls when it is higher than this.
const LIST_MAX: f32 = 384.;

pub struct Palette {
    pub input: Entity<TextInput>,
    pub selected: usize,
    scroll: ScrollHandle,
    /// The active session's worktree when Jump opened: its files are rows.
    root: Option<PathBuf>,
    files: Option<FileList>,
    /// The files that match the query, best first, and how many match in all. Found when the
    /// query changes and when the list comes in, never while drawing.
    file_hits: Vec<usize>,
    file_total: usize,
}

/// The files of a worktree, as the Files tab's filter lists them.
struct FileList {
    root: PathBuf,
    paths: Vec<String>,
    /// The same paths in lowercase, made once, for the match.
    lower: Vec<String>,
}

impl FileList {
    /// Runs git: call it on a background thread.
    fn read(root: PathBuf) -> FileList {
        let mut paths = git::listed_files(&root).unwrap_or_else(|error| {
            log::warn!("jump: could not list the files of a worktree: {error:#}");
            Vec::new()
        });
        paths.truncate(FILES_MAX);
        let lower = paths.iter().map(|p| p.to_lowercase()).collect();
        FileList { root, paths, lower }
    }
}

/// The groups, in the order of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Sessions,
    Worktrees,
    Projects,
    Files,
    Commands,
    Tickets,
}

impl Group {
    const ALL: [Group; 6] = [Group::Sessions, Group::Worktrees, Group::Projects, Group::Files, Group::Commands, Group::Tickets];

    fn title(self) -> &'static str {
        match self {
            Group::Sessions => "Sessions",
            Group::Worktrees => "Worktrees",
            Group::Projects => "Projects",
            Group::Files => "Files",
            Group::Commands => "Commands",
            Group::Tickets => "Tickets",
        }
    }

    /// The most rows a group shows. With nothing typed, every command shows, as before Jump. A
    /// query that is the group's name ("commands") shows the group in full, and the list scrolls;
    /// for the files that is the first `FILES_NAMED`, as each row is drawn on each frame.
    fn cap(self, typed: bool, named: Option<Group>) -> usize {
        match self {
            Group::Files if named == Some(self) => FILES_NAMED,
            _ if named == Some(self) => usize::MAX,
            Group::Sessions => 6,
            Group::Worktrees => 5,
            Group::Projects => 4,
            Group::Files => 8,
            Group::Commands if typed => 6,
            Group::Commands => usize::MAX,
            Group::Tickets => 6,
        }
    }
}

/// The mark at the start of a row.
#[derive(Clone, Copy)]
enum Mark {
    Agent(AgentKind),
    Icon(&'static [u8]),
}

type Run = fn(&mut Workspace, &mut Window, &mut Context<Workspace>);

/// What Enter on a row does.
enum Target {
    Command(Run),
    Session(String),
    Worktree { project: String, path: PathBuf },
    Project(String),
    File { root: PathBuf, path: String },
    Ticket(Key),
}

/// One row: what it says, a quiet hint, a shortcut, what it does, and what a query matches.
struct Item {
    group: Group,
    label: String,
    hint: String,
    /// A command's shortcut, from `keymap::label` (empty when it has none).
    keys: String,
    mark: Mark,
    /// A session's state.
    dot: Option<RowState>,
    /// Lowercase: the label, the hint, more words and the group's name. The palette before Jump
    /// matched its label, hint and group, so the same words still find the same rows.
    search: String,
    target: Target,
}

impl Item {
    fn new(group: Group, mark: Mark, label: String, hint: String, more: &str, target: Target) -> Item {
        let search = format!("{label} {hint} {more} {}", group.title()).to_lowercase();
        Item { group, label, hint, keys: String::new(), mark, dot: None, search, target }
    }
}

fn command(label: &str, icon: &'static [u8], hint: &str, keys: String, run: Run) -> Item {
    // A shortcut was the hint before Jump: it stays a word that matches.
    let mut item = Item::new(Group::Commands, Mark::Icon(icon), label.to_string(), hint.to_string(), &keys, Target::Command(run));
    item.keys = keys;
    item
}

fn session_item(id: &str, title: String, agent: AgentKind, branch: &str, project: &str, state: RowState) -> Item {
    // "Claude · ⎇ main" was the hint before Jump.
    let before = format!("{} · ⎇ {branch}", agent.name());
    let mut item = Item::new(Group::Sessions, Mark::Agent(agent), title, format!("{project} · {branch}"), &before, Target::Session(id.to_string()));
    item.dot = Some(state);
    item
}

fn review_item(number: u64, title: &str, repo: &str, key: Key) -> Item {
    // Review requests had a group of their own, "Reviews": the word still finds them.
    Item::new(Group::Tickets, Mark::Icon(icons::PULL_REQUEST), format!("#{number} · {title}"), format!("{repo} · asks for your review"), "Reviews", Target::Ticket(key))
}

/// The words of a query, lowercase.
fn words(query: &str) -> Vec<String> {
    query.to_lowercase().split_whitespace().map(str::to_string).collect()
}

/// The group a query names, when it is one word: the group's name, or the name without its "s"
/// ("command", "sessions", "files").
fn named_group(words: &[String]) -> Option<Group> {
    let [word] = words else { return None };
    Group::ALL.into_iter().find(|group| {
        let title = group.title().to_lowercase();
        *word == title || title.strip_suffix('s') == Some(word.as_str())
    })
}

/// The files to show: those `find_files` finds, or with a query that names the group, the
/// worktree's files from the top.
fn file_hits(lower: &[String], words: &[String]) -> (Vec<usize>, usize) {
    match named_group(words) {
        Some(Group::Files) => ((0..lower.len().min(FILES_NAMED)).collect(), lower.len()),
        named => find_files(lower, words, Group::Files.cap(true, named)),
    }
}

/// How well a row answers the words; lower is better and None leaves it out. A row whose label
/// starts with the first word comes first, then one whose label holds it. With nothing typed,
/// the sessions and the commands show (as before Jump), in the order given.
fn rank(group: Group, label: &str, search: &str, words: &[String]) -> Option<u8> {
    let Some(first) = words.first() else {
        return matches!(group, Group::Sessions | Group::Commands).then_some(0);
    };
    if !words.iter().all(|w| search.contains(w.as_str())) {
        return None;
    }
    let label = label.to_lowercase();
    Some(if label.starts_with(first.as_str()) {
        0
    } else if label.contains(first.as_str()) {
        1
    } else {
        2
    })
}

/// The files that hold every word, best first, at most `cap`, and how many hold them in all. A
/// file whose name starts with the first word comes first, then one whose name holds it, then
/// one whose folders do. One pass that allocates nothing for each file, so a key stays instant.
fn find_files(lower: &[String], words: &[String], cap: usize) -> (Vec<usize>, usize) {
    let Some(first) = words.first() else { return (Vec::new(), 0) };
    let mut best: [Vec<usize>; 3] = Default::default();
    let mut total = 0;
    for (i, path) in lower.iter().enumerate() {
        if !words.iter().all(|w| path.contains(w.as_str())) {
            continue;
        }
        total += 1;
        let name = &path[path.rfind('/').map_or(0, |at| at + 1)..];
        let rank = if name.starts_with(first.as_str()) {
            0
        } else if name.contains(first.as_str()) {
            1
        } else {
            2
        };
        if best[rank].len() < cap {
            best[rank].push(i);
        }
    }
    (best.into_iter().flatten().take(cap).collect(), total)
}

/// A line of the list: a group's heading (with the count of its matches it leaves out), or a row.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Line {
    Heading { group: Group, more: usize },
    Item(usize),
}

/// The lines of the list from each row's group and rank (index = the row's index): the groups in
/// order, each row's best first, each group cut at its cap. The files come found already, at
/// most their cap: `files_total` says how many match in all. `named` is the group the query
/// names, shown in full.
fn arrange(found: &[(Group, Option<u8>)], files_total: usize, typed: bool, named: Option<Group>) -> Vec<Line> {
    let mut lines = Vec::new();
    for group in Group::ALL {
        let mut hits: Vec<(u8, usize)> = found.iter().enumerate().filter(|(_, (g, _))| *g == group).filter_map(|(i, (_, rank))| rank.map(|r| (r, i))).collect();
        if hits.is_empty() {
            continue;
        }
        // Stable: rows that rank the same keep their order (sessions: the most recent first).
        hits.sort_by_key(|(rank, _)| *rank);
        let matched = if group == Group::Files { files_total.max(hits.len()) } else { hits.len() };
        hits.truncate(group.cap(typed, named));
        lines.push(Line::Heading { group, more: matched - hits.len() });
        lines.extend(hits.into_iter().map(|(_, i)| Line::Item(i)));
    }
    lines
}

/// The rows of the list in the order shown (indexes of the rows given to `arrange`).
fn picks(lines: &[Line]) -> Vec<usize> {
    lines
        .iter()
        .filter_map(|line| match line {
            Line::Item(i) => Some(*i),
            Line::Heading { .. } => None,
        })
        .collect()
}

/// The rows of `lines` in `group` and in the groups above it.
fn rows_through(lines: &[Line], group: Group) -> (usize, usize) {
    let (mut above, mut inside, mut current) = (0, 0, None);
    for line in lines {
        match line {
            Line::Heading { group, .. } => current = Some(*group),
            Line::Item(_) if current < Some(group) => above += 1,
            Line::Item(_) if current == Some(group) => inside += 1,
            Line::Item(_) => {}
        }
    }
    (above, inside)
}

/// The selection once the file rows have come in (`old` the lines before them, `new` after): a
/// row below the files keeps its place, so that Enter still runs the row that shows selected.
fn selection_after_files(old: &[Line], new: &[Line], selected: usize) -> usize {
    let (above, before) = rows_through(old, Group::Files);
    let (_, after) = rows_through(new, Group::Files);
    if selected >= above + before { selected + after - before } else { selected }
}

/// The top of each line and the height of the whole list, before the scale.
fn line_tops(lines: &[Line]) -> (Vec<f32>, f32) {
    let mut top = LIST_PAD;
    let mut tops = Vec::with_capacity(lines.len());
    for line in lines {
        tops.push(top);
        top += match line {
            Line::Heading { .. } => HEADING_H,
            Line::Item(_) => ROW_H,
        };
    }
    (tops, top + LIST_PAD)
}

/// The part of the list to keep in view for the `pick`th row: from its group's heading when it is
/// the group's first row (from the top of the list for the first group), to the end of the list
/// for the last row.
fn span(lines: &[Line], pick: usize) -> Option<(f32, f32)> {
    let line = lines.iter().enumerate().filter(|(_, l)| matches!(l, Line::Item(_))).nth(pick)?.0;
    let (tops, total) = line_tops(lines);
    let top = match line.checked_sub(1) {
        Some(0) => 0.,
        Some(above) if matches!(lines[above], Line::Heading { .. }) => tops[above],
        _ => tops[line],
    };
    let bottom = if line + 1 == lines.len() { total } else { tops[line] + ROW_H };
    Some((top, bottom))
}

/// How far down the list must be scrolled so that `top..bottom` shows in a view `view` high
/// that is scrolled `scrolled`: the least move.
fn scroll_to_show(top: f32, bottom: f32, view: f32, scrolled: f32) -> f32 {
    if view <= 0. {
        // Not laid out yet.
        scrolled
    } else if top < scrolled {
        top
    } else if bottom > scrolled + view {
        bottom - view
    } else {
        scrolled
    }
}

/// The theme, the scale and the query of a `GLASSCODE_DEBUG_OPEN=palette…` value.
fn parse_hook(mode: &str) -> (Option<ThemeChoice>, Option<UiScale>, String) {
    let (mut choice, mut scale, mut query) = (None, None, Vec::new());
    for word in mode.split(':').skip(1) {
        match word {
            "light" => choice = Some(ThemeChoice::Light),
            "dark" => choice = Some(ThemeChoice::Dark),
            "classic" => choice = Some(ThemeChoice::Classic),
            "contrast" => choice = Some(ThemeChoice::Contrast),
            other => match theme::scale_named(other) {
                Some(named) => scale = Some(named),
                None => query.push(other),
            },
        }
    }
    (choice, scale, query.join(":"))
}

impl Workspace {
    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::new("Jump to a session, worktree, project, file or command", cx).bare());
        cx.subscribe_in(&input, window, |this: &mut Workspace, _, event: &InputEvent, window, cx| match event {
            InputEvent::Submit => this.palette_run(None, window, cx),
            InputEvent::Cancel => this.close_palette(window, cx),
            InputEvent::Changed => this.palette_changed(cx),
        })
        .detach();
        crate::text_input::focus(&input, window, cx);
        self.menu_open = false;
        // A folder without git has no Files tab to open a file in.
        let root = self.active_session().filter(|s| self.is_git_project(&s.project_id)).map(|s| s.worktree.clone());
        self.palette = Some(Palette { input, selected: 0, scroll: ScrollHandle::new(), root: root.clone(), files: None, file_hits: Vec::new(), file_total: 0 });
        if let Some(root) = root {
            self.palette_read_files(root, cx);
        }
        cx.notify();
    }

    /// `GLASSCODE_DEBUG_OPEN=palette…` (debug builds; see the top of this file).
    pub fn open_palette_hook(&mut self, mode: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (choice, scale, query) = parse_hook(mode);
        if choice.is_some() || scale.is_some() {
            self.set_appearance(
                |a| {
                    a.theme = choice.unwrap_or(a.theme);
                    a.scale = scale.unwrap_or(a.scale);
                },
                cx,
            );
        }
        self.open_palette(window, cx);
        if let Some(input) = self.palette.as_ref().map(|p| p.input.clone()).filter(|_| !query.is_empty()) {
            input.update(cx, |input, cx| input.set_text(query, cx));
        }
    }

    pub fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.palette.take().map(|p| p.input.read(cx).focus_handle_clone());
        self.palette_gone(input, window, cx);
        cx.notify();
    }

    /// The palette's box had the keyboard and is gone: it goes to the session on screen, or to
    /// the window, so the window's keys (back, forward, the panels) keep working.
    fn palette_gone(&mut self, input: Option<gpui::FocusHandle>, window: &mut Window, cx: &mut Context<Self>) {
        if !input.is_some_and(|input| input.is_focused(window)) {
            return;
        }
        if self.view == View::Sessions {
            self.focus_active(window, cx);
        } else {
            window.focus(self.focus_handle_ref(), cx);
        }
    }

    /// Read the worktree's files on a background thread, each time Jump opens: the list is fresh,
    /// and no key waits for git.
    fn palette_read_files(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move { FileList::read(root) });
        cx.spawn(async move |this, cx| {
            let list = task.await;
            let _ = this.update(cx, |this, cx| {
                // Jump may be closed, or open again for another session.
                let Some(palette) = this.palette.as_mut().filter(|p| p.root.as_ref() == Some(&list.root)) else { return };
                palette.files = Some(list);
                let (_, old) = this.palette_rows(cx);
                this.palette_find_files(cx);
                let (_, new) = this.palette_rows(cx);
                if let Some(palette) = this.palette.as_mut() {
                    palette.selected = selection_after_files(&old, &new, palette.selected);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn palette_find_files(&mut self, cx: &mut Context<Self>) {
        let Some(palette) = self.palette.as_mut() else { return };
        let words = words(&palette.input.read(cx).text());
        (palette.file_hits, palette.file_total) = match &palette.files {
            Some(files) => file_hits(&files.lower, &words),
            None => (Vec::new(), 0),
        };
    }

    fn palette_changed(&mut self, cx: &mut Context<Self>) {
        self.palette_find_files(cx);
        if let Some(palette) = self.palette.as_mut() {
            palette.selected = 0;
            palette.scroll.set_offset(point(Pixels::ZERO, Pixels::ZERO));
        }
        cx.notify();
    }

    pub fn palette_move(&mut self, down: bool, cx: &mut Context<Self>) {
        let (_, lines) = self.palette_rows(cx);
        let count = picks(&lines).len();
        if let Some(p) = self.palette.as_mut()
            && count > 0
        {
            p.selected = if down { (p.selected + 1) % count } else { (p.selected + count - 1) % count };
            // Keep the selected row in view (the scroll handle's numbers are scaled).
            if let Some((top, bottom)) = span(&lines, p.selected) {
                let scale = theme::scale();
                let view = f32::from(p.scroll.bounds().size.height) / scale;
                let scrolled = -f32::from(p.scroll.offset().y) / scale;
                let to = scroll_to_show(top, bottom, view, scrolled);
                p.scroll.set_offset(point(Pixels::ZERO, theme::sz(-to)));
            }
        }
        cx.notify();
    }

    fn palette_run(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.palette.as_ref().map(|p| index.unwrap_or(p.selected)) else { return };
        let (mut items, lines) = self.palette_rows(cx);
        let Some(&at) = picks(&lines).get(selected) else { return };
        let item = items.swap_remove(at);
        let input = self.palette.take().map(|p| p.input.read(cx).focus_handle_clone());
        self.jump_to(item.target, window, cx);
        // Unless the command gave the keyboard to something else.
        self.palette_gone(input, window, cx);
        cx.notify();
    }

    fn jump_to(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        match target {
            Target::Command(run) => run(self, window, cx),
            Target::Session(id) => {
                self.center = crate::center::Center::Session;
                self.open_session(&id, window, cx);
            }
            // Its most recent session (the one shown last in it, else the newest), or what the
            // row's "+" (and an idle worktree's row) does when it has none: a new session there.
            // A click on the row in the tree only folds it, which Jump has no use for.
            Target::Worktree { project, path } => {
                let sessions: Vec<&Session> = self.state.sessions.iter().filter(|s| s.project_id == project && s.worktree == path).collect();
                let last = self.last_in_worktree.get(&path).filter(|id| sessions.iter().any(|s| &s.id == *id)).cloned();
                let newest = sessions.iter().max_by_key(|s| s.created_at).map(|s| s.id.clone());
                match last.or(newest) {
                    Some(id) => {
                        self.center = crate::center::Center::Session;
                        self.open_session(&id, window, cx);
                    }
                    None => self.open_new_session(Some(project), Some(path), window, cx),
                }
            }
            // As a click on the project in the tree (`fold` in sessions_ui.rs): it folds or opens.
            Target::Project(id) => {
                let Some(project) = self.state.projects.iter_mut().find(|p| p.id == id) else { return };
                let fold = !crate::sidebar::project_folded(&self.state.layout.folded, project);
                crate::sidebar::set_folded(&mut self.state.layout.folded, &crate::sidebar::project_key(&id), fold);
                // A project folded by the build before the tree opens for good once opened.
                if !fold {
                    project.expanded = true;
                }
                self.save();
                cx.notify();
            }
            Target::File { root, path } => self.jump_to_file(root, path, window, cx),
            Target::Ticket(key) => self.open_ticket(key, cx),
        }
    }

    /// A file opens as a click in the Files tab opens it: in a tab of the centre (its diff when
    /// it changed), or its tab comes to the front.
    fn jump_to_file(&mut self, root: PathBuf, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_session().filter(|s| s.worktree == root).map(|s| s.id.clone()) else {
            self.show_toast("Another session is active now. Open Jump again to find its files.", cx);
            return;
        };
        self.open_session(&id, window, cx);
        self.files_select(path, cx);
    }

    /// Every row Jump has for the query, and the lines of its list.
    fn palette_rows(&self, cx: &App) -> (Vec<Item>, Vec<Line>) {
        let Some(palette) = self.palette.as_ref() else { return (Vec::new(), Vec::new()) };
        let words = words(&palette.input.read(cx).text());
        let items = self.palette_items(palette);
        let found: Vec<(Group, Option<u8>)> = items.iter().map(|item| (item.group, rank(item.group, &item.label, &item.search, &words))).collect();
        let lines = arrange(&found, palette.file_total, !words.is_empty(), named_group(&words));
        (items, lines)
    }

    /// Everything Jump can go to (the files: only those found for the query).
    fn palette_items(&self, palette: &Palette) -> Vec<Item> {
        let mut items: Vec<Item> = vec![
            command("New session…", icons::PLUS, "", keymap::label(&NewSession), |ws, window, cx| ws.open_new_session(None, None, window, cx)),
            command("Go to Today", icons::TODAY, "Your day", String::new(), |ws, window, cx| ws.open_today(window, cx)),
            command("Go to the Board", icons::TICKETS, "Tickets from Linear", String::new(), |ws, _, cx| ws.show_tickets(cx)),
            command("Go to Pull requests", icons::PULL_REQUEST, "Yours, and those that ask for your review", String::new(), |ws, window, cx| {
                ws.go(View::PullRequests, window, cx)
            }),
            command("Go to Usage", icons::USAGE, "Claude and Codex", String::new(), |ws, _, cx| ws.show_view(View::Usage, cx)),
            command("Go back", icons::BACK, "", keymap::label(&GoBack), |ws, window, cx| ws.go_back(window, cx)),
            command("Go forward", icons::FORWARD, "", keymap::label(&GoForward), |ws, window, cx| ws.go_forward(window, cx)),
            command("Open Settings", icons::SETTINGS, "", keymap::label(&OpenSettings), |ws, window, cx| ws.go(View::Settings, window, cx)),
            command("Change the theme", icons::THEMES, "Appearance", String::new(), |ws, _, cx| ws.open_appearance(cx)),
            command("Accounts", icons::GEAR, "Settings", String::new(), |ws, window, cx| ws.open_settings(crate::settings_ui::Section::Accounts, window, cx)),
            command("Linear settings", icons::TICKETS, "Who you are, your reviewer label", String::new(), |ws, window, cx| ws.open_linear_settings(window, cx)),
            command("GitHub account for a project", icons::PULL_REQUEST, "Git and projects", String::new(), |ws, window, cx| {
                ws.open_settings(crate::settings_ui::Section::Projects, window, cx)
            }),
            command("Add a project…", icons::FOLDER, "", keymap::label(&AddProject), |ws, window, cx| ws.prompt_add_project(window, cx)),
            command("Hand off this session…", icons::SESSIONS, "", keymap::label(&Handoff), |ws, window, cx| ws.open_handoff(None, None, window, cx)),
            command("Show or hide the left sidebar", icons::PANEL_LEFT, "", keymap::label(&ToggleLeftPanel), |ws, _, cx| ws.toggle_left(cx)),
            command("Show or hide the side panel", icons::PANEL_RIGHT, "", keymap::label(&ToggleRightPanel), |ws, _, cx| ws.toggle_right(cx)),
            command("Browser", icons::GEAR, "Settings", String::new(), |ws, window, cx| ws.open_settings(crate::settings_ui::Section::Browser, window, cx)),
            command("Server…", icons::PLUG, "Your always-on machine", String::new(), |ws, window, cx| ws.open_server_dialog(window, cx)),
            command("Check for CLI updates", icons::REFRESH, "Updates", String::new(), |ws, window, cx| {
                ws.open_settings(crate::settings_ui::Section::Updates, window, cx)
            }),
            // Where the manager is switched on (it has no page until then).
            command("Manager settings", icons::STAR, "One agent above all your sessions", String::new(), |ws, window, cx| {
                ws.open_settings(crate::settings_ui::Section::Manager, window, cx)
            }),
        ];
        // The manager's page, while it is on: after the other views.
        if self.state.manager.enabled {
            let go = command("Go to the manager", icons::STAR, &self.state.manager.name, String::new(), |ws, window, cx| ws.go(View::Manager, window, cx));
            items.insert(5, go);
        }

        // Sessions, the most recent first: those on the back list (the nearest first), then the
        // newest; the one on screen last, as going there changes nothing.
        let back: Vec<&str> = self.history.0.iter().rev().filter_map(|place| place.session.as_deref()).collect();
        let on_screen = self.state.active_session.as_deref().filter(|_| self.view == View::Sessions);
        let mut sessions: Vec<&Session> = self.state.sessions.iter().collect();
        sessions.sort_by_key(|s| (Some(s.id.as_str()) == on_screen, back.iter().position(|id| *id == s.id).unwrap_or(usize::MAX), std::cmp::Reverse(s.created_at)));
        let project_name = |id: &str| self.state.project(id).map(|p| p.name.clone()).unwrap_or_default();
        for s in sessions {
            items.push(session_item(&s.id, self.session_title(s), s.agent, &s.branch, &project_name(&s.project_id), self.row_state(s)));
        }

        for project in self.state.projects.iter().filter(|p| !self.state.is_hidden(&p.id)) {
            let repo = project.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let in_project: Vec<&Session> = self.state.sessions.iter().filter(|s| s.project_id == project.id).collect();
            // The worktrees git listed, and those of the sessions (`project_worktrees` would run
            // git while drawing when the list is not read yet).
            let mut worktrees: Vec<(PathBuf, String)> = self.worktrees.get(&project.id).map(|list| list.iter().map(|w| (w.path.clone(), w.branch.clone())).collect()).unwrap_or_default();
            for s in &in_project {
                if !worktrees.iter().any(|(path, _)| *path == s.worktree) {
                    worktrees.push((s.worktree.clone(), s.branch.clone()));
                }
            }
            for (path, branch) in worktrees.into_iter().filter(|(path, _)| !self.removing.contains(path)) {
                let count = in_project.iter().filter(|s| s.worktree == path).count();
                let label = if branch.is_empty() || branch == "detached" { git::folder_label(&path, &repo) } else { branch };
                let folder = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let hint = format!("{} · {}", project.name, sessions_count(count));
                items.push(Item::new(Group::Worktrees, Mark::Icon(icons::BRANCH), label, hint, &folder, Target::Worktree { project: project.id.clone(), path }));
            }
            items.push(Item::new(Group::Projects, Mark::Icon(icons::FOLDER), project.name.clone(), sessions_count(in_project.len()), &repo, Target::Project(project.id.clone())));
        }

        if let Some(files) = &palette.files {
            for &i in &palette.file_hits {
                let path = &files.paths[i];
                let (folder, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
                let mut item = Item::new(Group::Files, Mark::Icon(icons::FILE), name.to_string(), folder.to_string(), "", Target::File { root: files.root.clone(), path: path.clone() });
                // Its path, and the group's name: these rows are only those `file_hits` found.
                item.search = format!("{} files", files.lower[i]);
                items.push(item);
            }
        }

        if let Some(snapshot) = self.tickets.snapshot.as_ref() {
            for t in snapshot.mine.iter().chain(snapshot.reviewing.iter()) {
                let key = Key::Ticket(t.identifier.clone());
                items.push(Item::new(Group::Tickets, Mark::Icon(icons::TICKETS), format!("{} · {}", t.identifier, t.title), t.status.name.clone(), "", Target::Ticket(key)));
            }
        }
        for r in &self.tickets.reviews {
            let key = Key::Pr(r.repo.clone(), r.hit.number);
            items.push(review_item(r.hit.number, &crate::tickets_view::tidy_pr_title(&r.hit.title), &r.repo, key));
        }
        items
    }

    pub fn render_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let palette = self.palette.as_ref()?;
        let (items, lines) = self.palette_rows(cx);
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut pick = 0;
        for line in &lines {
            match *line {
                Line::Heading { group, more } => rows.push(heading(group, more)),
                Line::Item(at) => {
                    rows.push(row(&items[at], pick, pick == palette.selected, cx));
                    pick += 1;
                }
            }
        }
        let height = if lines.is_empty() { ROW_H + LIST_PAD * 2. } else { line_tops(&lines).1.min(LIST_MAX) };
        let list = div()
            .flex()
            .flex_col()
            .py(theme::sz(LIST_PAD))
            .px(theme::sz(6.))
            .children(rows)
            .when(lines.is_empty(), |d| d.child(div().flex().items_center().h(theme::sz(ROW_H)).px(theme::sz(10.)).text_color(theme::text_soft()).child("Nothing matches. Try other words.")));
        let keys = |labels: Vec<String>, word: &'static str| {
            div().flex().items_center().gap(theme::sz(4.)).children(labels.into_iter().filter(|l| !l.is_empty()).map(ui::kbd)).child(word)
        };
        Some(
            deferred(
                div()
                    .id("palette-overlay")
                    .occlude()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .justify_center()
                    .items_start()
                    .pt(theme::sz(72.))
                    .pb(theme::sz(24.))
                    .bg(theme::overlay())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_palette(window, cx)))
                    .child(
                        // Glass over whatever is on screen. With no backdrop painted its tint alone
                        // shows the text under it through: the dialog colour goes under the glass
                        // (a painted backdrop covers it, as its frames are opaque).
                        crate::backdrop::glass_strong(theme::sz(14.))
                            .bg(theme::dialog())
                            .id("palette")
                            .key_context("Palette")
                            .on_action(cx.listener(|this, _: &PaletteUp, _, cx| this.palette_move(false, cx)))
                            .on_action(cx.listener(|this, _: &PaletteDown, _, cx| this.palette_move(true, cx)))
                            .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
                            .flex()
                            .flex_col()
                            .w(theme::sz(620.))
                            // In a low window (900 x 560 at 130 %) the list gives up height first.
                            .max_h(relative(1.))
                            .shadow_lg()
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .gap(theme::sz(10.))
                                    .px(theme::sz(16.))
                                    .py(theme::sz(4.))
                                    .border_b_1()
                                    .border_color(theme::border())
                                    .child(icons::icon(icons::SEARCH, theme::sz(16.), theme::sz(16.), theme::card_soft_text()))
                                    .child(div().flex_1().min_w_0().child(palette.input.clone())),
                            )
                            .child(div().flex().min_h_0().h(theme::sz(height)).child(ui::scroll_area("palette-list", &palette.scroll, list)))
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .gap(theme::sz(16.))
                                    .px(theme::sz(14.))
                                    .py(theme::sz(8.))
                                    .border_t_1()
                                    .border_color(theme::border())
                                    .text_size(theme::sz(11.5))
                                    .text_color(theme::card_soft_text())
                                    .child(keys(vec![keymap::label(&PaletteUp), keymap::label(&PaletteDown)], "move"))
                                    .child(keys(vec![keymap::label(&text_input::Submit)], "open"))
                                    .child(keys(vec![keymap::label(&text_input::Cancel)], "close")),
                            ),
                    ),
            )
            .with_priority(6)
            .into_any_element(),
        )
    }
}

fn sessions_count(count: usize) -> String {
    match count {
        0 => "no session".to_string(),
        1 => "1 session".to_string(),
        n => format!("{n} sessions"),
    }
}

fn heading(group: Group, more: usize) -> AnyElement {
    div()
        .flex()
        .items_end()
        .justify_between()
        .h(theme::sz(HEADING_H))
        .px(theme::sz(10.))
        .pb(theme::sz(5.))
        .text_size(theme::sz(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::card_soft_text())
        .child(group.title())
        .when(more > 0, |d| d.child(format!("{more} more")))
        .into_any_element()
}

fn mark(mark: Mark) -> AnyElement {
    const SIZE: f32 = 16.;
    match mark {
        // `brand` takes a size in pixels: the scale goes in here.
        Mark::Agent(agent) => icons::brand(icons::agent_brand(agent), SIZE),
        Mark::Icon(data) => icons::icon(data, theme::sz(SIZE), theme::sz(SIZE), theme::card_soft_text()).into_any_element(),
    }
}

/// The `pick`th row of the list.
fn row(item: &Item, pick: usize, on: bool, cx: &Context<Workspace>) -> AnyElement {
    div()
        .id(SharedString::from(format!("palette-{pick}")))
        .flex()
        .flex_none()
        .items_center()
        .gap(theme::sz(10.))
        .h(theme::sz(ROW_H))
        .px(theme::sz(10.))
        .rounded(theme::sz(8.))
        .cursor_pointer()
        .when(on, |d| d.bg(theme::selected()))
        .when(!on, |d| d.hover(|s| s.bg(theme::tint())))
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.palette_run(Some(pick), window, cx)))
        .child(mark(item.mark))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme::text()).child(item.label.clone()))
        .when_some(item.dot, |d, state| d.child(ui::status_dot(state, false)))
        .when(!item.hint.is_empty(), |d| {
            d.child(div().flex_none().max_w(theme::sz(240.)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(theme::sz(12.)).text_color(theme::card_soft_text()).child(item.hint.clone()))
        })
        .when(!item.keys.is_empty(), |d| d.child(ui::kbd(item.keys.clone())))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn item(group: Group, label: &str, hint: &str) -> Item {
        Item::new(group, Mark::Icon(icons::FILE), label.to_string(), hint.to_string(), "", Target::Project(label.to_string()))
    }

    fn found(items: &[Item], query: &str) -> Vec<(Group, Option<u8>)> {
        let words = words(query);
        items.iter().map(|i| (i.group, rank(i.group, &i.label, &i.search, &words))).collect()
    }

    fn headings(lines: &[Line]) -> Vec<(Group, usize)> {
        lines
            .iter()
            .filter_map(|line| match line {
                Line::Heading { group, more } => Some((*group, *more)),
                Line::Item(_) => None,
            })
            .collect()
    }

    #[test]
    fn the_groups_come_in_order() {
        let items = vec![
            item(Group::Commands, "Open the alpha settings", ""),
            item(Group::Tickets, "ENG-1 · Alpha crash", "Todo"),
            item(Group::Projects, "alpha", "2 sessions"),
            item(Group::Files, "alpha.rs", "src"),
            item(Group::Worktrees, "alpha-fix", "alpha · 1 session"),
            item(Group::Sessions, "Fix the alpha login", "alpha · main"),
            item(Group::Sessions, "Nothing to do with it", "beta · main"),
        ];
        let lines = arrange(&found(&items, "alpha"), 1, true, None);
        let order: Vec<Group> = headings(&lines).into_iter().map(|(group, _)| group).collect();
        assert_eq!(order, Group::ALL);
        // Each heading is followed by its one row; the session without the word is not there.
        assert_eq!(picks(&lines), [5, 4, 2, 3, 0, 1]);
    }

    #[test]
    fn a_group_stops_at_its_cap_and_others_still_show() {
        let mut items: Vec<Item> = (0..10).map(|n| item(Group::Sessions, &format!("Fix the build {n}"), "acme · main")).collect();
        items.push(item(Group::Commands, "Build settings", ""));
        let lines = arrange(&found(&items, "build"), 0, true, None);
        assert_eq!(headings(&lines), [(Group::Sessions, 4), (Group::Commands, 0)]);
        // The command's label starts with the word, the sessions' labels only hold it.
        assert_eq!(picks(&lines), [0, 1, 2, 3, 4, 5, 10]);
        // The files group counts every file that matches, not only those it holds.
        let files: Vec<Item> = (0..8).map(|n| item(Group::Files, &format!("build{n}.rs"), "src")).collect();
        assert_eq!(headings(&arrange(&found(&files, "build"), 30, true, None)), [(Group::Files, 22)]);
    }

    #[test]
    fn the_start_of_a_label_goes_first_in_its_group() {
        let items = vec![item(Group::Commands, "Open the theme", ""), item(Group::Commands, "Change", "Themes"), item(Group::Commands, "Theme colours", "")];
        assert_eq!(picks(&arrange(&found(&items, "theme"), 0, true, None)), [2, 0, 1]);
    }

    #[test]
    fn nothing_typed_shows_the_sessions_and_every_command() {
        let mut items: Vec<Item> = (0..3).map(|n| item(Group::Sessions, &format!("Session {n}"), "")).collect();
        items.extend((0..18).map(|n| item(Group::Commands, &format!("Command {n}"), "")));
        items.push(item(Group::Projects, "acme", ""));
        items.push(item(Group::Worktrees, "main", ""));
        items.push(item(Group::Tickets, "ENG-123 · A ticket", ""));
        let lines = arrange(&found(&items, "   "), 0, false, None);
        assert_eq!(headings(&lines), [(Group::Sessions, 0), (Group::Commands, 0)]);
        assert_eq!(picks(&lines), (0..21).collect::<Vec<_>>());
    }

    #[test]
    fn a_query_that_names_a_group_shows_it_in_full() {
        assert_eq!(named_group(&words("commands")), Some(Group::Commands));
        assert_eq!(named_group(&words("Command")), Some(Group::Commands));
        assert_eq!(named_group(&words("sessions")), Some(Group::Sessions));
        assert_eq!(named_group(&words("files")), Some(Group::Files));
        assert_eq!(named_group(&words("comm")), None);
        assert_eq!(named_group(&words("open commands")), None);
        assert_eq!(named_group(&words("")), None);
        // The old palette showed 14 of its 18 commands for "commands"; now all 18 show.
        let items: Vec<Item> = (0..18).map(|n| command(&format!("Command {n}"), icons::GEAR, "", String::new(), |_, _, _| {})).collect();
        let query = words("commands");
        let lines = arrange(&found(&items, "commands"), 0, true, named_group(&query));
        assert_eq!(headings(&lines), [(Group::Commands, 0)]);
        assert_eq!(picks(&lines).len(), 18);
        // Any other word keeps the cap of 6.
        assert_eq!(headings(&arrange(&found(&items, "comm"), 0, true, named_group(&words("comm")))), [(Group::Commands, 12)]);
        // "files" lists the worktree's files from the top, at most FILES_NAMED.
        let lower: Vec<String> = (0..300).map(|i| format!("src/part_{i}.rs")).collect();
        let (hits, total) = file_hits(&lower, &words("files"));
        assert_eq!((hits.len(), hits[0], total), (FILES_NAMED, 0, 300));
        // A file name is still found the usual way, eight at most.
        assert_eq!(file_hits(&lower, &words("part_1")).0.len(), 8);
    }

    #[test]
    fn a_query_with_no_match_shows_nothing() {
        let items = vec![item(Group::Sessions, "Fix the build", "acme · main"), item(Group::Commands, "Open Settings", "")];
        assert!(arrange(&found(&items, "zebra"), 0, true, None).is_empty());
        // Every word must be there.
        assert!(arrange(&found(&items, "fix zebra"), 0, true, None).is_empty());
        assert_eq!(find_files(&["src/main.rs".to_string()], &words("zebra"), 8), (Vec::new(), 0));
    }

    #[test]
    fn the_words_of_the_old_palette_still_match() {
        let items = vec![
            session_item("s1", "Fix the login".into(), AgentKind::Claude, "fix-login", "acme", RowState::Idle),
            command("Go back", icons::BACK, "", "Ctrl+<".into(), |_, _, _| {}),
            command("Server…", icons::PLUG, "Your always-on machine", String::new(), |_, _, _| {}),
            review_item(784, "Add the export", "acme/app", Key::Pr("acme/app".into(), 784)),
            item(Group::Tickets, "ENG-123 · Export fails", "In Progress"),
        ];
        let hits = |query: &str| picks(&arrange(&found(&items, query), 0, true, None));
        assert_eq!(hits("claude"), [0]);
        assert_eq!(hits("⎇ fix-login"), [0]);
        assert_eq!(hits("sessions"), [0]);
        assert_eq!(hits("acme"), [0, 3]);
        assert_eq!(hits("commands"), [1, 2]);
        assert_eq!(hits("ctrl+<"), [1]);
        assert_eq!(hits("always-on"), [2]);
        assert_eq!(hits("reviews"), [3]);
        assert_eq!(hits("asks for your review"), [3]);
        assert_eq!(hits("tickets"), [3, 4]);
        assert_eq!(hits("in progress"), [4]);
    }

    #[test]
    fn a_file_name_goes_before_a_folder() {
        let lower: Vec<String> = ["src/button/mod.rs", "src/widgets/round_button.rs", "src/widgets/button.rs"].map(String::from).to_vec();
        assert_eq!(find_files(&lower, &words("button"), 8), (vec![2, 1, 0], 3));
        assert_eq!(find_files(&lower, &words("button widgets"), 8), (vec![2, 1], 2));
        assert_eq!(find_files(&lower, &words("button"), 2), (vec![2, 1], 3));
        assert_eq!(find_files(&lower, &words(""), 8), (Vec::new(), 0));
    }

    #[test]
    fn typing_stays_instant_with_twenty_thousand_files() {
        let lower: Vec<String> = (0..FILES_MAX).map(|i| format!("crates/part{}/src/module_{}/file_{i}.rs", i % 40, i % 300)).collect();
        let query = "src file_1999";
        let started = Instant::now();
        let mut last = (Vec::new(), 0);
        for end in 1..=query.len() {
            last = find_files(&lower, &words(&query[..end]), 8);
        }
        let took = started.elapsed();
        // A key each, as typed: one look at each file for each key. The bound is far above what
        // a debug build takes, so that a slow machine does not fail it.
        assert!(took < Duration::from_secs(3), "{} keys over {} files took {took:?}", query.len(), lower.len());
        // file_1999 and file_19990 to file_19999 match; the first eight show, in git's order.
        assert_eq!(last.1, 11);
        assert_eq!(last.0, [1999, 19990, 19991, 19992, 19993, 19994, 19995, 19996]);
    }

    #[test]
    fn the_selection_keeps_its_row_when_the_files_come_in() {
        let heading = |group| Line::Heading { group, more: 0 };
        let old = [heading(Group::Sessions), Line::Item(0), heading(Group::Commands), Line::Item(1), Line::Item(2)];
        let new = [heading(Group::Sessions), Line::Item(0), heading(Group::Files), Line::Item(3), Line::Item(4), Line::Item(5), heading(Group::Commands), Line::Item(1), Line::Item(2)];
        // The second command was selected: it still is.
        let selected = selection_after_files(&old, &new, 2);
        assert_eq!(picks(&new)[selected], picks(&old)[2]);
        // A session above the files keeps its place.
        assert_eq!(selection_after_files(&old, &new, 0), 0);
        // No file matched: nothing moves.
        assert_eq!(selection_after_files(&old, &old, 2), 2);
    }

    #[test]
    fn a_moved_selection_stays_in_view() {
        let mut lines = vec![Line::Heading { group: Group::Sessions, more: 0 }];
        lines.extend((0..20).map(Line::Item));
        let (tops, total) = line_tops(&lines);
        assert_eq!(tops[1], LIST_PAD + HEADING_H);
        assert_eq!(total, LIST_PAD * 2. + HEADING_H + ROW_H * 20.);
        // The first row brings its heading and the top of the list.
        assert_eq!(span(&lines, 0), Some((0., LIST_PAD + HEADING_H + ROW_H)));
        // The last row brings the end of the list.
        assert_eq!(span(&lines, 19), Some((tops[20], total)));
        assert_eq!(span(&lines, 20), None);
        // Down past the bottom of the view: the least scroll that shows the row.
        let (top, bottom) = span(&lines, 12).unwrap();
        assert_eq!(scroll_to_show(top, bottom, LIST_MAX, 0.), bottom - LIST_MAX);
        // A row in view does not move it; back up above the view, the row goes to its top.
        assert_eq!(scroll_to_show(top, bottom, LIST_MAX, bottom - LIST_MAX), bottom - LIST_MAX);
        let (top, bottom) = span(&lines, 2).unwrap();
        assert_eq!(scroll_to_show(top, bottom, LIST_MAX, 200.), top);
        // Before the first layout the view has no height.
        assert_eq!(scroll_to_show(top, bottom, 0., 0.), 0.);
    }

    #[test]
    fn the_hook_takes_a_theme_a_scale_and_a_query() {
        assert_eq!(parse_hook("palette"), (None, None, String::new()));
        assert_eq!(parse_hook("palette:dark:130:fix-login"), (Some(ThemeChoice::Dark), Some(UiScale::P130), "fix-login".to_string()));
        assert_eq!(parse_hook("palette:light:button.rs"), (Some(ThemeChoice::Light), None, "button.rs".to_string()));
    }
}
