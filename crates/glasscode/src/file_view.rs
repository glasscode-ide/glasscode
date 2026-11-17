//! A file tab in the centre: the file's path, then its text with line numbers, or its diff with
//! HEAD when git shows a change (the Diff and File control). A click on a file in Files or
//! Changes, Enter on a file in Jump, opens one; the same file again brings its tab to the front.
//!
//! The parts stay apart, because the editor (U20) replaces the read-only lines:
//! - the model: `FileTabs` and `FileTab` (the path, what was read, Diff or File);
//! - the loading: `load`, which runs off the main thread and applies the limits (512 KB, 4000
//!   lines, the binary check, long lines cut) before anything reaches the window;
//! - the element that draws lines: `lines_element`, a `uniform_list`, so that a long file costs
//!   only the lines on screen.
//!
//! The tabs of a worktree stay in memory while the person is in another worktree, for the run;
//! they are not saved. A tab is read again when it comes to the front and when git or the disk
//! shows a change (one look at the disk every 2 seconds, for the tab in front only); nothing is
//! drawn again unless what was read differs.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, FontWeight, Hsla, ListHorizontalSizingBehavior, Pixels, SharedString, Stateful,
    UniformListScrollHandle, div, prelude::*, uniform_list,
};
use glasscode_core::git;

use crate::icons;
use crate::theme;
use crate::ui;
use crate::workspace::Workspace;

/// At most this much of a file is read and shown.
pub const MAX_BYTES: usize = 512 * 1024;
pub const MAX_LINES: usize = 4000;
/// A NUL byte in this first part makes a file binary, as git decides.
const BINARY_PROBE: usize = 8000;
/// The part of a line that is drawn: a minified file has lines of megabytes, and laying out one
/// of them would cost more than the rest of the window.
pub const LINE_CHARS: usize = 1000;
/// How often the disk is asked whether the file in front changed.
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// The lines' text and height (logical, as at 100 %).
const LINE_TEXT: f32 = 12.5;
const LINE_HEIGHT: f32 = 20.;
/// A digit of the line numbers in the code font, at `LINE_TEXT`.
const DIGIT: f32 = 7.6;
const MISSING: &str = "This file is not in the worktree any more: it was deleted or moved.";

// ----- the model ---------------------------------------------------------------------------------

/// What a file tab shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The diff with HEAD (while git shows a change).
    Diff,
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Added,
    Removed,
    /// A hunk's head (`@@ … @@`), or git's "\ No newline at end of file".
    Hunk,
}

/// A line as it is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Its number in the file (in a diff: in the new file); none for removed lines and hunk heads.
    pub number: Option<usize>,
    pub kind: Kind,
    /// A conflict marker (`<<<<<<<`, `=======`, `>>>>>>>`), shown on red.
    pub conflict: bool,
    pub text: SharedString,
}

/// Text after the limits: its lines, and a sentence for each limit that applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lines {
    pub lines: Arc<Vec<Line>>,
    /// The longest line, whose width the list takes for its sideways scroll.
    pub longest: usize,
    /// The largest line number, which the numbers' column fits.
    pub largest: usize,
    pub notes: Vec<String>,
}

/// What a tab shows in one mode.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Reading,
    Text(Lines),
    /// A NUL byte near its start: there is no text to show. Its size in bytes.
    Binary(u64),
    /// Not in the worktree: deleted or moved.
    Missing,
    Failed(String),
}

/// What the disk says about a file: its length and when it changed. None when it is gone.
pub type Stamp = Option<(u64, SystemTime)>;

/// Git's change for a file, as the Changes tab has it: its status letter and the lines added and
/// removed.
pub type Change = (char, usize, usize);

/// What one load gives.
#[derive(Debug, PartialEq)]
pub struct Loaded {
    pub stamp: Stamp,
    pub file: Content,
    /// None when git showed no change for the file.
    pub diff: Option<Content>,
}

pub struct FileTab {
    pub worktree: PathBuf,
    /// Relative to the worktree, with "/".
    pub path: String,
    pub full: PathBuf,
    /// Diff or File. None until the first read that knew git's change: then the diff if the file
    /// had one, else the file. Later the person's choice, never changed by a new change on disk
    /// (as the old preview kept the view of the click).
    pub mode: Option<Mode>,
    pub file: Content,
    pub diff: Option<Content>,
    /// What the content was read against, to see when to read it again: the disk and git.
    stamp: Stamp,
    pub change: Option<Change>,
    /// Each load has a number; the result of an older one is dropped.
    generation: u64,
    loading: bool,
    checking: bool,
    checked: Option<Instant>,
    file_scroll: UniformListScrollHandle,
    diff_scroll: UniformListScrollHandle,
}

impl FileTab {
    fn new(worktree: &Path, path: &str) -> Self {
        FileTab {
            worktree: worktree.to_path_buf(),
            path: path.to_string(),
            full: full_path(worktree, path),
            mode: None,
            file: Content::Reading,
            diff: None,
            stamp: None,
            change: None,
            generation: 0,
            loading: false,
            checking: false,
            checked: None,
            file_scroll: UniformListScrollHandle::new(),
            diff_scroll: UniformListScrollHandle::new(),
        }
    }

    /// The diff is on screen: the file has one, and the person did not pick File.
    pub fn shows_diff(&self) -> bool {
        self.diff.is_some() && self.mode != Some(Mode::File)
    }

    /// Take a load's result. True when what shows changed.
    fn take(&mut self, loaded: Loaded, change: Option<Change>) -> bool {
        self.stamp = loaded.stamp;
        self.change = change;
        let changed = self.file != loaded.file || self.diff != loaded.diff;
        self.file = loaded.file;
        self.diff = loaded.diff;
        changed
    }
}

/// The open file tabs of every worktree, and the one in front.
#[derive(Default)]
pub struct FileTabs {
    /// In the order they opened.
    tabs: Vec<FileTab>,
    /// The file tab in front, the session on screen when it came there, and when. Once a session
    /// is shown after that (a row, a tab, Jump, back and forward, a notification, an agent's
    /// command), that session's own tab is in front, also when it is the same session.
    front: Option<(PathBuf, String, Instant)>,
}

impl FileTabs {
    /// The tab of `path` in `worktree`, opened if it is not open: its full path, and whether it is
    /// new.
    pub fn open(&mut self, worktree: &Path, path: &str) -> (PathBuf, bool) {
        let full = full_path(worktree, path);
        if self.tabs.iter().any(|t| t.full == full) {
            return (full, false);
        }
        self.tabs.push(FileTab::new(worktree, path));
        (full, true)
    }

    pub fn get(&self, full: &Path) -> Option<&FileTab> {
        self.tabs.iter().find(|t| t.full == full)
    }

    pub fn get_mut(&mut self, full: &Path) -> Option<&mut FileTab> {
        self.tabs.iter_mut().find(|t| t.full == full)
    }

    /// The open files of a worktree, in the order they opened.
    pub fn paths_in(&self, worktree: &Path) -> Vec<PathBuf> {
        self.tabs.iter().filter(|t| t.worktree == worktree).map(|t| t.full.clone()).collect()
    }

    pub fn bring_to_front(&mut self, full: PathBuf, session: &str) {
        self.front = Some((full, session.to_string(), Instant::now()));
    }

    /// A session's tab is in front.
    pub fn session_to_front(&mut self) {
        self.front = None;
    }

    /// The file tab in front while `session` is on screen and no session was shown since
    /// (`shown`: when one was shown last, `TodayUi::navigated_at`). Either ends it.
    pub fn front(&mut self, session: &str, shown: Option<Instant>) -> Option<PathBuf> {
        if !self.front.as_ref().is_some_and(|(_, over, at)| over == session && shown.is_none_or(|shown| shown <= *at)) {
            self.front = None;
        }
        self.front.as_ref().map(|(full, _, _)| full.clone())
    }

    /// `front` without ending it, for what only draws.
    pub fn front_tab(&self, session: &str) -> Option<&FileTab> {
        self.front.as_ref().filter(|(_, over, _)| over == session).and_then(|(full, _, _)| self.get(full))
    }

    pub fn close(&mut self, full: &Path) {
        self.tabs.retain(|t| t.full != full);
        if self.front.as_ref().is_some_and(|(f, _, _)| f == full) {
            self.front = None;
        }
    }
}

/// A path as the tree and git write it, from what a caller has (git, the tree, a transcript on
/// Windows): "/" between the parts, none at the start. Only the system's own separator is
/// changed: on macOS "\" is a letter of a file name.
pub fn relative(path: &str) -> String {
    let path = path.replace(std::path::MAIN_SEPARATOR, "/");
    path.trim_start_matches("./").trim_start_matches('/').to_string()
}

/// The file on the disk: each part of `path` joined to the worktree, so that a tab has one name
/// for a file however it was opened.
pub fn full_path(worktree: &Path, path: &str) -> PathBuf {
    path.split('/').filter(|p| !p.is_empty() && *p != ".").fold(worktree.to_path_buf(), |full, part| full.join(part))
}

// ----- the loading -------------------------------------------------------------------------------

/// The file and, when git shows a change (`status`), its diff with HEAD, with the limits. It reads
/// the disk and runs git, so it runs off the main thread.
pub fn load(worktree: &Path, path: &str, status: Option<char>) -> Loaded {
    let full = full_path(worktree, path);
    let stamp = stamp(&full);
    let file = read_file(&full);
    let diff = status.map(|status| match &file {
        Content::Binary(size) => Content::Binary(*size),
        // A file git does not track yet: all of it is new.
        Content::Text(lines) if status == '?' => Content::Text(all_added(lines)),
        _ => match git::diff_file(worktree, path, status == '?') {
            Ok(diff) => Content::Text(diff_lines(&diff)),
            Err(error) => Content::Failed(error.to_string()),
        },
    });
    Loaded { stamp, file, diff }
}

pub fn stamp(full: &Path) -> Stamp {
    let meta = std::fs::metadata(full).ok()?;
    Some((meta.len(), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
}

/// A file's text, of its first `MAX_BYTES` only.
pub fn read_file(full: &Path) -> Content {
    let file = match std::fs::File::open(full) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Content::Missing,
        Err(error) => return Content::Failed(error.to_string()),
    };
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity((size as usize).min(MAX_BYTES + 1));
    if let Err(error) = file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes) {
        return Content::Failed(error.to_string());
    }
    if bytes.iter().take(BINARY_PROBE).any(|b| *b == 0) {
        return Content::Binary(size);
    }
    Content::Text(text_lines(&bytes, size))
}

/// Builds `Lines` with the limits on the number and the length of lines.
#[derive(Default)]
struct Builder {
    lines: Vec<Line>,
    /// The longest line so far: its index and its length.
    longest: (usize, usize),
    largest: usize,
    cut: usize,
    full: bool,
}

impl Builder {
    /// Add a line; false once `MAX_LINES` are in.
    fn push(&mut self, number: Option<usize>, kind: Kind, text: &str) -> bool {
        if self.lines.len() == MAX_LINES {
            self.full = true;
            return false;
        }
        let (shown, chars, cut) = shown_text(text);
        if cut {
            self.cut += 1;
        }
        if chars > self.longest.1 {
            self.longest = (self.lines.len(), chars);
        }
        let conflict = text.starts_with("<<<<<<<") || text.starts_with("=======") || text.starts_with(">>>>>>>");
        self.largest = self.largest.max(number.unwrap_or(0));
        self.lines.push(Line { number, kind, conflict, text: shown.into() });
        true
    }

    fn finish(self, mut notes: Vec<String>, what: &str) -> Lines {
        if self.full {
            notes.push(format!("Only the first {MAX_LINES} lines of this {what} are shown."));
        }
        if self.cut > 0 {
            let lines = if self.cut == 1 { "1 line is".to_string() } else { format!("{} lines are", self.cut) };
            notes.push(format!("{lines} longer than {LINE_CHARS} characters; the rest of each is not shown."));
        }
        Lines { lines: Arc::new(self.lines), longest: self.longest.0, largest: self.largest, notes }
    }
}

/// A line as it is drawn: a tab is four spaces, another control character shows as �, and only the
/// first `LINE_CHARS` characters are kept. With its length in characters, and whether it was cut.
fn shown_text(line: &str) -> (String, usize, bool) {
    let mut out = String::with_capacity(line.len().min(LINE_CHARS * 4));
    let mut chars = 0;
    for c in line.chars() {
        if chars >= LINE_CHARS {
            out.push('…');
            return (out, chars + 1, true);
        }
        match c {
            '\t' => {
                out.push_str("    ");
                chars += 4;
            }
            c if c.is_control() => {
                out.push('\u{FFFD}');
                chars += 1;
            }
            c => {
                out.push(c);
                chars += 1;
            }
        }
    }
    (out, chars, false)
}

/// The lines of a file's first bytes (`size` is the whole file's), numbered.
pub fn text_lines(bytes: &[u8], size: u64) -> Lines {
    let shown = &bytes[..bytes.len().min(MAX_BYTES)];
    let text = String::from_utf8_lossy(shown);
    let mut builder = Builder::default();
    for (i, line) in text.lines().enumerate() {
        if !builder.push(Some(i + 1), Kind::Plain, line) {
            break;
        }
    }
    let mut notes = Vec::new();
    if bytes.len() > MAX_BYTES || size > MAX_BYTES as u64 {
        notes.push(format!("This file is {}; only its first {} are shown.", size_words(size), size_words(MAX_BYTES as u64)));
    }
    builder.finish(notes, "file")
}

/// A file git does not track yet, as a diff: each line added.
fn all_added(file: &Lines) -> Lines {
    let lines = file.lines.iter().map(|l| Line { kind: Kind::Added, ..l.clone() }).collect();
    Lines { lines: Arc::new(lines), ..file.clone() }
}

/// The lines of git's diff of one file, numbered as in the new file. The file's headers before
/// the first hunk are left out.
pub fn diff_lines(diff: &str) -> Lines {
    let mut builder = Builder::default();
    let mut number = 0usize;
    for line in diff.lines().skip_while(|l| !l.starts_with("@@")) {
        let rest = line.get(1..).unwrap_or("");
        let more = match line.chars().next() {
            Some('@') => {
                number = hunk_start(line).unwrap_or(number);
                builder.push(None, Kind::Hunk, line)
            }
            Some('\\') => builder.push(None, Kind::Hunk, line),
            Some('-') => builder.push(None, Kind::Removed, rest),
            Some('+') => {
                number += 1;
                builder.push(Some(number - 1), Kind::Added, rest)
            }
            _ => {
                number += 1;
                builder.push(Some(number - 1), Kind::Plain, rest)
            }
        };
        if !more {
            break;
        }
    }
    builder.finish(Vec::new(), "diff")
}

/// The first line of a hunk in the new file: 12 in "@@ -10,4 +12,6 @@".
fn hunk_start(head: &str) -> Option<usize> {
    head.split(' ').find_map(|part| part.strip_prefix('+')).and_then(|n| n.split(',').next()).and_then(|n| n.parse().ok())
}

/// "740 KB", "2.3 MB", "120 bytes".
pub fn size_words(bytes: u64) -> String {
    const KB: f64 = 1024.;
    let b = bytes as f64;
    if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{} KB", (b / KB).round() as u64)
    } else if bytes == 1 {
        "1 byte".to_string()
    } else {
        format!("{bytes} bytes")
    }
}

// ----- the element that draws lines ------------------------------------------------------------------

/// The lines of a file or a diff (`marks`: with the column of + and −). Only the lines on screen
/// are laid out. The list is as wide as its longest line, which is cut at `LINE_CHARS`, and
/// scrolls sideways; nothing wraps. The text cannot be selected (the old preview could not
/// either); the editor (U20) comes in its place.
pub fn lines_element(id: impl Into<ElementId>, lines: &Lines, marks: bool, scroll: &UniformListScrollHandle) -> AnyElement {
    let rows = lines.lines.clone();
    let gutter = gutter_width(lines.largest);
    uniform_list(id, rows.len(), move |range, _, _| range.map(|i| line_row(&rows[i], gutter, marks)).collect::<Vec<_>>())
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(Some(lines.longest))
        .track_scroll(scroll)
        .size_full()
        .py(theme::sz(6.))
        .into_any_element()
}

/// The width of the line numbers: room for the largest one's digits, at least two.
fn gutter_width(largest: usize) -> Pixels {
    let digits = largest.max(1).ilog10() as f32 + 1.;
    theme::sz(digits.max(2.) * DIGIT + 22.)
}

fn line_row(line: &Line, gutter: Pixels, marks: bool) -> Div {
    let (bg, mark, mark_color, text_color) = match line.kind {
        Kind::Added => (theme::success_bg(), "+", theme::added(), theme::text()),
        Kind::Removed => (theme::error_bg(), "−", theme::removed(), theme::text()),
        Kind::Hunk => (theme::hover(), "", theme::muted(), theme::muted()),
        Kind::Plain => (gpui::transparent_black(), "", theme::muted(), theme::text()),
    };
    let bg = if line.conflict { theme::error_bg() } else { bg };
    div()
        .flex()
        .items_center()
        .w_full()
        .h(theme::sz(LINE_HEIGHT))
        .bg(bg)
        .whitespace_nowrap()
        .font_family(theme::MONO_FONT)
        .text_size(theme::sz(LINE_TEXT))
        .line_height(theme::sz(LINE_HEIGHT))
        .child(div().flex_none().w(gutter).pr(theme::sz(12.)).flex().justify_end().text_color(theme::faint()).children(line.number.map(|n| n.to_string())))
        .when(marks, |d| d.child(div().flex_none().w(theme::sz(14.)).text_color(mark_color).child(mark)))
        .child(div().flex_none().pr(theme::sz(16.)).text_color(text_color).child(line.text.clone()))
}

// ----- the tab in the window ---------------------------------------------------------------------

/// A small button of the file's head: its icon, if it has one, and its words.
fn head_button(id: &'static str, glyph: Option<&'static [u8]>, label: String) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(theme::sz(6.))
        .h(theme::sz(26.))
        .px(theme::sz(8.))
        .rounded(theme::sz(6.))
        .border_1()
        .border_color(theme::border())
        .text_size(theme::sz(12.5))
        .text_color(theme::text_soft())
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover()).text_color(theme::text()))
        .when_some(glyph, |d, glyph| d.child(icons::icon(glyph, theme::sz(14.), theme::sz(14.), theme::text_soft())))
        .child(label)
}

fn sentence(text: impl Into<SharedString>) -> Div {
    div().px(theme::sz(16.)).py(theme::sz(14.)).text_size(theme::sz(13.)).text_color(theme::muted()).child(text.into())
}

impl Workspace {
    /// Git's change for a file of `worktree`, as the Changes tab read it last; the outer None
    /// while the Changes are of another worktree (just after a change of session).
    fn change_of(&self, worktree: &Path, path: &str) -> Option<Option<Change>> {
        (self.changes_for.as_deref() == Some(worktree)).then(|| self.changes.iter().find(|c| c.path == path).map(|c| (c.status, c.added, c.removed)))
    }

    /// The file tab in front of the centre, while a local session is on screen in Sessions.
    pub(crate) fn front_file(&self) -> Option<&FileTab> {
        if self.view != crate::shell::View::Sessions || self.center != crate::center::Center::Session {
            return None;
        }
        let session = self.active_session()?;
        self.gh.files.tabs.front_tab(&session.id).filter(|t| t.worktree == session.worktree)
    }

    /// Open a file of the active session's worktree (`path` relative to it) in a tab of the
    /// centre, or bring its tab to the front.
    pub(crate) fn open_file_tab(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(session) = self.active_session().cloned() else { return };
        let path = relative(path);
        if path.is_empty() {
            return;
        }
        let (full, new) = self.gh.files.tabs.open(&session.worktree, &path);
        self.gh.files.tabs.bring_to_front(full.clone(), &session.id);
        if new {
            self.file_tab_load(&full, cx);
        } else {
            self.file_tab_check(&full, cx);
        }
        cx.notify();
    }

    /// A click on a file tab: it comes to the front, and is read again if the disk changed.
    pub(crate) fn show_file_tab(&mut self, full: PathBuf, cx: &mut Context<Self>) {
        let Some(session) = self.active_session().map(|s| s.id.clone()) else { return };
        self.gh.files.tabs.bring_to_front(full.clone(), &session);
        self.file_tab_check(&full, cx);
        cx.notify();
    }

    /// Read the tab's file (and its diff) in the background. What is shown changes, and the
    /// window draws again, only when the result differs.
    fn file_tab_load(&mut self, full: &Path, cx: &mut Context<Self>) {
        let Some((worktree, path, had)) = self.gh.files.tabs.get(full).map(|t| (t.worktree.clone(), t.path.clone(), t.change)) else { return };
        // Until the Changes of this worktree are read, the change the tab had stands.
        let known = self.change_of(&worktree, &path);
        let change = known.unwrap_or(had);
        let Some(tab) = self.gh.files.tabs.get_mut(full) else { return };
        tab.generation += 1;
        tab.loading = true;
        let generation = tab.generation;
        let full = full.to_path_buf();
        let task = cx.background_spawn(async move { load(&worktree, &path, change.map(|c| c.0)) });
        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            let _ = this.update(cx, |this, cx| {
                let Some(tab) = this.gh.files.tabs.get_mut(&full) else { return };
                if tab.generation != generation {
                    return;
                }
                tab.loading = false;
                let changed = tab.take(loaded, change);
                if tab.mode.is_none() && known.is_some() {
                    tab.mode = Some(if tab.diff.is_some() { Mode::Diff } else { Mode::File });
                }
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Ask the disk, in the background, whether the tab's file changed; read it again if so.
    fn file_tab_check(&mut self, full: &Path, cx: &mut Context<Self>) {
        let Some(tab) = self.gh.files.tabs.get_mut(full) else { return };
        if tab.loading || tab.checking {
            return;
        }
        tab.checking = true;
        tab.checked = Some(Instant::now());
        let full = full.to_path_buf();
        let task = cx.background_spawn({
            let full = full.clone();
            async move { stamp(&full) }
        });
        cx.spawn(async move |this, cx| {
            let now = task.await;
            let _ = this.update(cx, |this, cx| {
                let Some(tab) = this.gh.files.tabs.get_mut(&full) else { return };
                tab.checking = false;
                if tab.stamp != now {
                    this.file_tab_load(&full, cx);
                }
            });
        })
        .detach();
    }

    /// Every tick: the file tab in front is read again when git's change for it differs from
    /// the one it was read with, or the disk says it changed.
    pub(crate) fn file_tabs_tick(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.front_file() else { return };
        if tab.loading {
            return;
        }
        let (full, change, due) = (tab.full.clone(), tab.change, tab.checked.is_none_or(|t| t.elapsed() >= CHECK_EVERY));
        if self.change_of(&tab.worktree, &tab.path).is_some_and(|now| now != change) {
            self.file_tab_load(&full, cx);
        } else if due {
            self.file_tab_check(&full, cx);
        }
    }

    /// The file tab's content: its head (the path, git's status, Diff and File, what can be done
    /// with it), the limits that applied, and the lines on an opaque ground (long text stays sharp
    /// over the backdrop, as in the terminal).
    pub(crate) fn render_file_view(&self, full: &Path, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.gh.files.tabs.get(full) else {
            return div().into_any_element();
        };
        let path = tab.path.clone();
        let show_diff = tab.shows_diff();
        let content = if show_diff { tab.diff.as_ref().unwrap_or(&tab.file) } else { &tab.file };
        let scroll = if show_diff { &tab.diff_scroll } else { &tab.file_scroll };
        let editor = self.files_editor_name();
        // The agent's file counts as being edited for two minutes, as in the tree.
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let editing = self.gh.files.last_edit.as_ref().is_some_and(|(p, at)| *p == path && now - at < 120);

        let mode = |id: &'static str, label: &'static str, on: bool, pick: Mode| {
            let full = full.to_path_buf();
            ui::segment(id, on, label).h(theme::sz(24.)).px_2p5().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if let Some(tab) = this.gh.files.tabs.get_mut(&full) {
                    tab.mode = Some(pick);
                }
                cx.notify();
            }))
        };
        let views = tab.diff.is_some().then(|| ui::segmented().flex_none().child(mode("file-mode-diff", "Diff", show_diff, Mode::Diff)).child(mode("file-mode-file", "File", !show_diff, Mode::File)));
        let status = tab.change.map(|(status, _, _)| {
            div()
                .flex_none()
                .px_1p5()
                .py_0p5()
                .rounded(theme::sz(5.))
                .bg(theme::tag_bg())
                .text_color(crate::files_ui::status_color(status))
                .text_size(theme::sz(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .child(crate::files_ui::status_name(status))
        });
        let (p_open, p_copy, p_mention) = (path.clone(), path.clone(), path.clone());
        let head = div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap_x(theme::sz(8.))
            .gap_y(theme::sz(6.))
            .px(theme::sz(12.))
            .py(theme::sz(8.))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w(theme::sz(140.))
                    .items_center()
                    .gap(theme::sz(8.))
                    .child(icons::icon(icons::FILE, theme::sz(15.), theme::sz(15.), theme::muted()))
                    // The middle gives way, so the folder's start and the file's name stay.
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis_middle().font_family(theme::MONO_FONT).text_size(theme::sz(12.5)).text_color(theme::text()).child(path.clone())),
            )
            .children(status)
            .children(views)
            // The buttons go onto more lines rather than past the edge of a narrow centre.
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .gap(theme::sz(6.))
                    .child(head_button("file-open", Some(icons::OPEN_OUTSIDE), format!("Open in {editor}")).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_in_editor(Some(p_open.clone()), cx))))
                    .child(head_button("file-copy", None, "Copy path".into()).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.files_copy(&p_copy, true, cx))))
                    .child(head_button("file-mention", None, "Mention in session".into()).on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.files_mention(p_mention.clone(), window, cx)))),
            );

        let mut notes: Vec<String> = match content {
            Content::Text(lines) => lines.notes.clone(),
            _ => Vec::new(),
        };
        // A file deleted while its tab is open: git's diff shows what it had, under the sentence.
        if show_diff && tab.file == Content::Missing {
            notes.insert(0, MISSING.to_string());
        }
        let body: AnyElement = match content {
            Content::Reading => sentence("Reading…").into_any_element(),
            Content::Text(lines) if lines.lines.is_empty() => sentence(if show_diff { "git shows no changed lines in this file." } else { "This file is empty." }).into_any_element(),
            Content::Text(lines) => div().flex().flex_col().flex_1().min_h_0().child(lines_element(if show_diff { "file-diff-lines" } else { "file-lines" }, lines, show_diff, scroll)).into_any_element(),
            Content::Binary(size) => sentence(format!("A binary file ({}): it has no text to show.", size_words(*size))).into_any_element(),
            Content::Missing => sentence(MISSING).into_any_element(),
            Content::Failed(error) => sentence(format!("Glasscode could not read this file: {error}")).into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .rounded(theme::sz(14.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .overflow_hidden()
            .child(head)
            .when(editing, |d| {
                d.child(note_line(theme::added(), true, "The agent edited this file in the last two minutes."))
            })
            .children(notes.into_iter().map(|n| note_line(theme::muted(), false, n)))
            .child(body)
            .into_any_element()
    }
}

/// A line under the head: a limit that applied, or the agent's edit (with its dot). It wraps in
/// a narrow centre.
fn note_line(color: Hsla, dot: bool, text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(theme::sz(6.))
        .px(theme::sz(14.))
        .pt(theme::sz(6.))
        .text_size(theme::sz(12.))
        .text_color(color)
        .when(dot, |d| d.child(ui::dot(color)))
        .child(div().flex_1().min_w_0().child(text.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("glasscode-file-view-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn texts(lines: &Lines) -> Vec<&str> {
        lines.lines.iter().map(|l| l.text.as_ref()).collect()
    }

    #[test]
    fn open_select_again_close() {
        let mut tabs = FileTabs::default();
        let (w1, w2) = (Path::new("/w/app"), Path::new("/w/app-login"));
        let (a, new) = tabs.open(w1, "src/main.rs");
        assert!(new);
        let (b, _) = tabs.open(w1, "README.md");
        let (c, _) = tabs.open(w2, "src/main.rs");
        // The same file again, however its path is written: the same tab.
        assert_eq!(tabs.open(w1, &relative(&format!("src{}main.rs", std::path::MAIN_SEPARATOR))), (a.clone(), false));
        assert_eq!(tabs.paths_in(w1), vec![a.clone(), b.clone()]);
        assert_eq!(tabs.paths_in(w2), vec![c.clone()], "a worktree has its own tabs");
        tabs.close(&a);
        assert_eq!(tabs.paths_in(w1), vec![b]);
        assert!(tabs.get(&a).is_none());
    }

    /// A file tab stays in front while its session is on screen; another session on screen, or
    /// the same session shown again (its row clicked), ends that, also when it comes back.
    #[test]
    fn a_session_shown_comes_to_the_front() {
        let mut tabs = FileTabs::default();
        let (a, _) = tabs.open(Path::new("/w/app"), "a.rs");
        let before = Instant::now();
        tabs.bring_to_front(a.clone(), "s1");
        assert_eq!(tabs.front("s1", None), Some(a.clone()));
        assert_eq!(tabs.front("s1", Some(before)), Some(a.clone()), "a session shown before the file came forward");
        assert_eq!(tabs.front_tab("s2").map(|t| t.full.clone()), None);
        assert_eq!(tabs.front("s2", None), None);
        assert_eq!(tabs.front("s1", None), None, "it does not come back");
        tabs.bring_to_front(a.clone(), "s1");
        assert_eq!(tabs.front("s1", Some(Instant::now() + Duration::from_millis(5))), None, "the same session shown again");
        tabs.bring_to_front(a.clone(), "s1");
        tabs.session_to_front();
        assert_eq!(tabs.front("s1", None), None);
        tabs.bring_to_front(a.clone(), "s1");
        tabs.close(&a);
        assert_eq!(tabs.front("s1", None), None, "a closed tab is not in front");
    }

    #[test]
    fn paths_have_one_form() {
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(relative(&format!("src{sep}ui{sep}main.rs")), "src/ui/main.rs");
        assert_eq!(relative("./README.md"), "README.md");
        assert_eq!(full_path(Path::new("/w/app"), "src/main.rs"), Path::new("/w/app").join("src").join("main.rs"));
    }

    #[test]
    fn an_empty_file_has_no_lines() {
        let dir = temp_dir("empty");
        std::fs::write(dir.join("empty.txt"), "").unwrap();
        let loaded = load(&dir, "empty.txt", None);
        assert_eq!(loaded.file, Content::Text(Lines::default()));
        assert_eq!(loaded.diff, None);
        assert_eq!(loaded.stamp.map(|s| s.0), Some(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_binary_file_shows_no_text() {
        let dir = temp_dir("binary");
        std::fs::write(dir.join("logo.png"), [0x89, b'P', b'N', b'G', 0, 0, 0, 13, 1, 2, 3]).unwrap();
        assert_eq!(load(&dir, "logo.png", None).file, Content::Binary(11));
        // Changed: no diff lines either.
        assert_eq!(load(&dir, "logo.png", Some('?')).diff, Some(Content::Binary(11)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_above_the_limit_shows_its_start_with_a_sentence() {
        let dir = temp_dir("large");
        // 700 KB in lines of 99 characters: past both limits.
        let line = "x".repeat(99);
        let text: String = (0..7200).map(|_| format!("{line}\n")).collect();
        std::fs::write(dir.join("big.log"), &text).unwrap();
        let Content::Text(lines) = read_file(&dir.join("big.log")) else { panic!("not text") };
        assert_eq!(lines.lines.len(), MAX_LINES);
        assert_eq!(lines.lines[MAX_LINES - 1].number, Some(MAX_LINES));
        assert_eq!(lines.notes.len(), 2, "{:?}", lines.notes);
        assert!(lines.notes[0].starts_with("This file is 703 KB; only its first 512 KB"), "{:?}", lines.notes);
        assert!(lines.notes[1].contains("first 4000 lines"));
        // The size and long lines: 586 KB in 300 lines of 2000 characters.
        let long: String = (0..300).map(|_| format!("{}\n", "y".repeat(2000))).collect();
        let lines = text_lines(&long.as_bytes()[..MAX_BYTES + 1], long.len() as u64);
        assert_eq!(lines.lines.len(), 263, "the last line is the part before the cut");
        assert_eq!(lines.lines[262].text.len(), 26);
        assert!(lines.notes[0].starts_with("This file is 586 KB"), "{:?}", lines.notes);
        assert!(lines.notes[1].starts_with("262 lines are longer"), "{:?}", lines.notes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_deleted_after_the_tab_opened_says_so() {
        let dir = temp_dir("deleted");
        std::fs::write(dir.join("gone.rs"), "fn main() {}\n").unwrap();
        let first = load(&dir, "gone.rs", None);
        assert!(matches!(first.file, Content::Text(_)));
        std::fs::remove_file(dir.join("gone.rs")).unwrap();
        // The tick's look at the disk sees it, and the read again gives the sentence.
        assert_ne!(stamp(&dir.join("gone.rs")), first.stamp);
        assert_eq!(load(&dir, "gone.rs", None), Loaded { stamp: None, file: Content::Missing, diff: None });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn very_long_lines_are_cut() {
        let long = "a".repeat(250_000);
        let text = format!("short\n{long}\n\tindented\nlast\u{7}\n");
        let lines = text_lines(text.as_bytes(), text.len() as u64);
        assert_eq!(lines.lines.len(), 4);
        assert_eq!(lines.lines[1].text.chars().count(), LINE_CHARS + 1);
        assert!(lines.lines[1].text.ends_with('…'));
        assert_eq!(lines.longest, 1, "the list takes the cut line's width");
        assert_eq!(lines.lines[2].text.as_ref(), "    indented");
        assert_eq!(lines.lines[3].text.as_ref(), "last\u{FFFD}");
        assert_eq!(lines.notes, vec![format!("1 line is longer than {LINE_CHARS} characters; the rest of each is not shown.")]);
        // A character of more than one byte at the cut is not split.
        let wide = "é".repeat(LINE_CHARS + 5);
        assert_eq!(shown_text(&wide).0.chars().count(), LINE_CHARS + 1);
    }

    #[test]
    fn a_diff_is_numbered_as_the_new_file() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -10,4 +10,5 @@ fn main() {\n context\n-old\n+new\n+++plus\n context\n@@ -40 +41 @@\n-x\n+<<<<<<< HEAD\n\\ No newline at end of file\n";
        let lines = diff_lines(diff);
        let shown: Vec<(Option<usize>, Kind, &str)> = lines.lines.iter().map(|l| (l.number, l.kind, l.text.as_ref())).collect();
        assert_eq!(
            shown,
            vec![
                (None, Kind::Hunk, "@@ -10,4 +10,5 @@ fn main() {"),
                (Some(10), Kind::Plain, "context"),
                (None, Kind::Removed, "old"),
                (Some(11), Kind::Added, "new"),
                (Some(12), Kind::Added, "++plus"),
                (Some(13), Kind::Plain, "context"),
                (None, Kind::Hunk, "@@ -40 +41 @@"),
                (None, Kind::Removed, "x"),
                (Some(41), Kind::Added, "<<<<<<< HEAD"),
                (None, Kind::Hunk, "\\ No newline at end of file"),
            ]
        );
        assert!(lines.lines[8].conflict);
        assert_eq!(hunk_start("@@ -0,0 +1 @@"), Some(1));
        assert!(diff_lines("").lines.is_empty());
    }

    #[test]
    fn a_new_file_is_all_added() {
        let dir = temp_dir("untracked");
        std::fs::write(dir.join("new.md"), "# Title\n\ntext\n").unwrap();
        let loaded = load(&dir, "new.md", Some('?'));
        let Some(Content::Text(diff)) = loaded.diff else { panic!("no diff") };
        assert_eq!(texts(&diff), vec!["# Title", "", "text"]);
        assert!(diff.lines.iter().all(|l| l.kind == Kind::Added));
        assert_eq!(diff.lines[2].number, Some(3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sizes_in_words() {
        assert_eq!(size_words(1), "1 byte");
        assert_eq!(size_words(900), "900 bytes");
        assert_eq!(size_words(512 * 1024), "512 KB");
        assert_eq!(size_words(2_400_000), "2.3 MB");
    }

    #[test]
    fn the_gutter_fits_the_numbers() {
        assert_eq!(gutter_width(9), gutter_width(42), "two digits at least");
        assert!(gutter_width(4000) > gutter_width(999));
    }
}
