//! Glasscode's terminal: `alacritty_terminal` for emulation, GPUI for drawing and input.

mod colors;
pub mod files;
mod keys;

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use alacritty_terminal::event::{Event as AlacEvent, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::CursorShape;
use gpui::{
    App, AppContext, BorderStyle, Bounds, ClipboardItem, Context, ElementInputHandler,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font, FontWeight, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, Render, ScrollWheelEvent, ShapedLine, SharedString,
    StrikethroughStyle, Styled, Task, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window,
    actions, canvas, div, fill, font, outline, point, px, size,
};


actions!(terminal, [Copy, Paste, ClearScrollback]);

const LINE_HEIGHT_RATIO: f32 = 1.38;

#[derive(Clone)]
struct EventProxy(async_channel::Sender<AlacEvent>);

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        let _ = self.0.try_send(event);
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct GridSize {
    cols: usize,
    lines: usize,
    cell_width: Pixels,
    line_height: Pixels,
}

impl GridSize {
    fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.lines as u16,
            num_cols: self.cols as u16,
            cell_width: f32::from(self.cell_width) as u16,
            cell_height: f32::from(self.line_height) as u16,
        }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// What to run inside a terminal.
#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
}

pub enum TerminalEvent {
    #[allow(dead_code)]
    Exited(Option<i32>),
    TitleChanged,
    Bell,
    /// Something to tell the person (a pasted file could not be sent).
    Notice(String),
}

pub struct TerminalView {
    term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    focus: FocusHandle,
    size: GridSize,
    origin: Point<Pixels>,
    scroll_remainder: Pixels,
    selecting: bool,
    pub title: String,
    pub exit_code: Option<Option<i32>>,
    /// When the person last typed or pasted here, and when the program last drew: messages
    /// Glasscode types in by itself wait for both to be quiet.
    pub last_typed: Option<std::time::Instant>,
    pub last_output: std::time::Instant,
    /// Where pasted and dropped files go so the program can read them.
    pub files: files::FileTarget,
    /// Text Windows types as characters, and an IME's text not typed yet (see the input handler).
    typed: keys::TypedText,
    _events: Task<()>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

struct Prepared {
    term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    rx: async_channel::Receiver<AlacEvent>,
    size: GridSize,
}

fn prepare(spec: SpawnSpec) -> anyhow::Result<Prepared> {
    // alacritty writes the program into the command line without quotation marks and lets Windows
    // find it there. Windows then tries each piece up to a space as a program first: for
    // `C:\Program Files\nodejs\node.exe` a file `C:\Program.exe` would start in its place. A
    // Windows path has no `"` of its own.
    #[cfg(windows)]
    let spec = if spec.program.contains([' ', '\t']) && !spec.program.starts_with('"') {
        SpawnSpec {
            program: format!("\"{}\"", spec.program),
            ..spec
        }
    } else {
        spec
    };
    // In chat view the terminal is never painted, so it keeps this size (prepaint only
    // resizes it once it is shown). Give it enough rows that a full-screen prompt — Codex's
    // folder-trust selector, onboarding — isn't clipped above the region chat reads back.
    let size = GridSize {
        cols: 100,
        lines: 48,
        cell_width: px(8.),
        line_height: px(18.),
    };
    let (tx, rx) = async_channel::unbounded();
    let proxy = EventProxy(tx);
    let config = Config {
        scrolling_history: 20_000,
        ..Config::default()
    };
    let term = Arc::new(FairMutex::new(Term::new(config, &size, proxy.clone())));

    let mut env = spec.env;
    env.insert("TERM".into(), "xterm-256color".into());
    env.insert("COLORTERM".into(), "truecolor".into());
    env.insert("TERM_PROGRAM".into(), "Glasscode".into());
    let options = tty::Options {
        shell: Some(tty::Shell::new(spec.program, spec.args)),
        working_directory: Some(spec.cwd),
        drain_on_exit: true,
        env,
        // An argument with a space must reach the program as one argument.
        #[cfg(windows)]
        escape_args: true,
    };
    let pty = tty::new(&options, size.window_size(), 0)?;
    let event_loop = EventLoop::new(term.clone(), proxy, pty, true, false)?;
    let notifier = Notifier(event_loop.channel());
    event_loop.spawn();
    Ok(Prepared { term, notifier, rx, size })
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        // Stops the I/O thread; dropping the PTY hangs up the child process.
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

impl TerminalView {
    /// Start `spec` in a new terminal.
    pub fn spawn(spec: SpawnSpec, cx: &mut App) -> anyhow::Result<gpui::Entity<Self>> {
        let prepared = prepare(spec)?;
        Ok(cx.new(|cx| Self::from_prepared(prepared, cx)))
    }

    fn from_prepared(prepared: Prepared, cx: &mut Context<Self>) -> Self {
        let Prepared { term, notifier, rx, size } = prepared;
        let events = cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                let mut batch = vec![event];
                while let Ok(event) = rx.try_recv() {
                    batch.push(event);
                }
                let alive = this.update(cx, |this, cx| {
                    for event in batch {
                        this.handle_event(event, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            term,
            notifier,
            focus: cx.focus_handle(),
            size,
            origin: Point::default(),
            scroll_remainder: px(0.),
            selecting: false,
            title: String::new(),
            exit_code: None,
            last_typed: None,
            last_output: std::time::Instant::now(),
            files: files::FileTarget::Local,
            typed: keys::TypedText::default(),
            _events: events,
        }
    }

    fn handle_event(&mut self, event: AlacEvent, cx: &mut Context<Self>) {
        match event {
            AlacEvent::Wakeup => {
                self.last_output = std::time::Instant::now();
                cx.notify()
            }
            AlacEvent::Title(title) => {
                self.title = title;
                cx.emit(TerminalEvent::TitleChanged);
            }
            AlacEvent::ResetTitle => {
                self.title.clear();
                cx.emit(TerminalEvent::TitleChanged);
            }
            AlacEvent::Bell => cx.emit(TerminalEvent::Bell),
            AlacEvent::PtyWrite(text) => self.notifier.notify(text.into_bytes()),
            AlacEvent::ClipboardStore(_, text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            AlacEvent::ClipboardLoad(_, format) => {
                let text = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .unwrap_or_default();
                self.notifier.notify(format(&text).into_bytes());
            }
            AlacEvent::ColorRequest(index, format) => {
                // Programs ask for the palette (0-255) and for the default foreground (256),
                // background (257) and cursor (258) colors to pick a light or dark theme.
                let term = self.term.lock();
                let color = colors::for_request(index, term.colors());
                drop(term);
                self.notifier.notify(format(color).into_bytes());
            }
            AlacEvent::TextAreaSizeRequest(format) => {
                self.notifier
                    .notify(format(self.size.window_size()).into_bytes());
            }
            AlacEvent::ChildExit(status) => {
                self.exit_code = Some(status.code());
                cx.emit(TerminalEvent::Exited(status.code()));
                cx.notify();
            }
            AlacEvent::Exit => {
                if self.exit_code.is_none() {
                    self.exit_code = Some(None);
                    cx.emit(TerminalEvent::Exited(None));
                }
                cx.notify();
            }
            AlacEvent::MouseCursorDirty | AlacEvent::CursorBlinkingChange => {}
        }
    }

    /// Send raw bytes (keystrokes, pasted text) to the program.
    pub fn write(&mut self, bytes: impl Into<Cow<'static, [u8]>>, cx: &mut Context<Self>) {
        if self.exit_code.is_some() {
            return;
        }
        {
            let mut term = self.term.lock();
            if term.grid().display_offset() != 0 {
                term.scroll_display(Scroll::Bottom);
            }
            term.selection = None;
        }
        self.notifier.notify(bytes);
        cx.notify();
    }

    /// Visible text plus scrollback, used for handoff summaries and search.
    pub fn text_tail(&self, max_lines: usize) -> String {
        self.tail(max_lines, false, false)
    }

    /// For reading what is typed in a program's input box: rows the terminal wrapped (a line
    /// longer than the screen is wide) joined back into one, and dimmed text (a suggestion the
    /// program shows in an empty box) left out.
    pub fn text_tail_typed(&self, max_lines: usize) -> String {
        self.tail(max_lines, true, true)
    }

    fn tail(&self, max_lines: usize, unwrap: bool, skip_dim: bool) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        let top = grid.topmost_line().0;
        let bottom = grid.bottommost_line().0;
        let start = (bottom - max_lines as i32 + 1).max(top);
        let mut out = String::new();
        for line in start..=bottom {
            let row = &grid[Line(line)];
            let mut text = String::new();
            for col in 0..grid.columns() {
                let cell = &row[Column(col)];
                if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    text.push(if skip_dim && cell.flags.contains(Flags::DIM) { ' ' } else { cell.c });
                }
            }
            let wrapped = unwrap && grid.columns() > 0 && row[Column(grid.columns() - 1)].flags.contains(Flags::WRAPLINE);
            if wrapped {
                out.push_str(&text);
            } else {
                out.push_str(text.trim_end());
                out.push('\n');
            }
        }
        out
    }

    fn set_size(&mut self, size: GridSize) {
        if size == self.size {
            return;
        }
        let grid_changed = size.cols != self.size.cols || size.lines != self.size.lines;
        self.size = size;
        if grid_changed {
            self.term.lock().resize(size);
            self.notifier.on_resize(size.window_size());
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // Its characters come to `replace_text_in_range`; writing it here too would double it.
        if keys::typed_by_system(event, cfg!(windows)) {
            return;
        }
        let mode = *self.term.lock().mode();
        if let Some(bytes) = keys::for_key_down(event, mode) {
            self.last_typed = Some(std::time::Instant::now());
            self.write(bytes, cx);
            cx.stop_propagation();
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.term.lock().selection_to_string();
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(unwrap_lines(&text, self.size.cols)));
        }
    }

    /// Cmd+V: copied files and images become their paths (see [`files`]); text is typed.
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else { return };
        let incoming = files::from_clipboard(&item);
        if !incoming.is_empty() {
            self.paste_files(incoming, cx);
        } else if let Some(text) = item.text() {
            self.paste_text(&text, cx);
        }
    }

    /// Files dropped on the terminal (from Finder, a screenshot's thumbnail, …).
    fn drop_paths(&mut self, paths: &gpui::ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.paste_files(paths.paths().iter().cloned().map(files::Incoming::Path).collect(), cx);
    }

    /// Type the paths of pasted or dropped files (saved or sent to the server first).
    pub fn paste_files(&mut self, incoming: Vec<files::Incoming>, cx: &mut Context<Self>) {
        self.last_typed = Some(std::time::Instant::now());
        let target = self.files.clone();
        let task = cx.background_spawn(async move { files::prepare_all(incoming, &target) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(text) => this.paste_text(&text, cx),
                Err(error) => cx.emit(TerminalEvent::Notice(format!("{error:#}"))),
            });
        })
        .detach();
    }

    pub(crate) fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.last_typed = Some(std::time::Instant::now());
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        let payload = if bracketed {
            format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
        } else {
            text.replace("\r\n", "\r").replace('\n', "\r")
        };
        self.write(payload.into_bytes(), cx);
    }

    fn clear_scrollback(&mut self, _: &ClearScrollback, _: &mut Window, cx: &mut Context<Self>) {
        self.term.lock().grid_mut().clear_history();
        cx.notify();
    }

    fn grid_point(&self, position: Point<Pixels>) -> (GridPoint, Side) {
        let term = self.term.lock();
        let offset = term.grid().display_offset() as i32;
        let x = f32::from(position.x - self.origin.x).max(0.);
        let y = f32::from(position.y - self.origin.y).max(0.);
        let cell_width = f32::from(self.size.cell_width);
        let col_f = x / cell_width;
        let col = (col_f as usize).min(self.size.cols.saturating_sub(1));
        let side = if col_f.fract() < 0.5 { Side::Left } else { Side::Right };
        let row = ((y / f32::from(self.size.line_height)) as usize)
            .min(self.size.lines.saturating_sub(1));
        (GridPoint::new(Line(row as i32 - offset), Column(col)), side)
    }

    /// Whether a left click goes to the program instead of selecting. Programs that track the
    /// mouse (tmux, which Glasscode runs with `mouse on` for scrolling, and some agents' screens)
    /// would otherwise take every drag, and text could never be selected: a drag always
    /// selects here, and Option+click reaches the program.
    fn mouse_reporting(&self, alt: bool) -> bool {
        alt && self.term.lock().mode().intersects(TermMode::MOUSE_MODE)
    }

    fn report_mouse(&mut self, button: u8, position: Point<Pixels>, pressed: bool, cx: &mut Context<Self>) {
        let (point, _) = self.grid_point(position);
        let offset = self.term.lock().grid().display_offset() as i32;
        let line = point.line.0 + offset + 1;
        let col = point.column.0 + 1;
        let suffix = if pressed { 'M' } else { 'm' };
        self.notifier
            .notify(format!("\x1b[<{button};{col};{line}{suffix}").into_bytes());
        cx.notify();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.mouse_reporting(event.modifiers.alt) {
            self.report_mouse(0, event.position, true, cx);
            return;
        }
        let (point, side) = self.grid_point(event.position);
        let ty = match event.click_count {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.term.lock().selection = Some(Selection::new(ty, point, side));
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if self.mouse_reporting(event.modifiers.alt) {
            if self.term.lock().mode().contains(TermMode::MOUSE_DRAG) {
                self.report_mouse(32, event.position, true, cx);
            }
            return;
        }
        if !self.selecting {
            return;
        }
        let (point, side) = self.grid_point(event.position);
        if let Some(selection) = self.term.lock().selection.as_mut() {
            selection.update(point, side);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.mouse_reporting(event.modifiers.alt) {
            self.report_mouse(0, event.position, false, cx);
            return;
        }
        self.selecting = false;
        let mut term = self.term.lock();
        if term.selection.as_ref().is_some_and(|s| s.is_empty()) {
            term.selection = None;
        }
        cx.notify();
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let line_height = self.size.line_height;
        self.scroll_remainder += event.delta.pixel_delta(line_height).y;
        let lines = (self.scroll_remainder / line_height) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= line_height * lines as f32;
        let mode = *self.term.lock().mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            let button = if lines > 0 { 64 } else { 65 };
            for _ in 0..lines.unsigned_abs().min(10) {
                self.report_mouse(button, event.position, true, cx);
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            let key = if lines > 0 { "A" } else { "B" };
            let prefix = if mode.contains(TermMode::APP_CURSOR) { "\x1bO" } else { "\x1b[" };
            let bytes = format!("{prefix}{key}").repeat(lines.unsigned_abs() as usize);
            self.notifier.notify(bytes.into_bytes());
        } else {
            self.term.lock().scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }
}

/// Windows only (see `render`): the text Windows types as characters. That is the text of the keys
/// `on_key_down` leaves to it (see [`keys::typed_by_system`]), `SendInput` characters (a
/// `VK_PACKET` never comes as a key-down), and what an IME, the emoji panel or dictation commits.
/// The terminal holds no text of its own, so there are no ranges to read or select.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // Windows places the IME's windows from this and `bounds_for_range`.
        let end = self
            .typed
            .composing()
            .map_or(0, |text| text.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.typed
            .composing()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.typed.compose("");
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(bytes) = self.typed.commit(text) {
            self.last_typed = Some(std::time::Instant::now());
            self.write(bytes, cx);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.typed.compose(new_text);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The cell of the cursor, where the composed text shows: the IME's candidates open there.
        let term = self.term.lock();
        let cursor = term.grid().cursor.point;
        let row = cursor.line.0 + term.grid().display_offset() as i32;
        Some(Bounds::new(
            point(
                self.origin.x + self.size.cell_width * cursor.column.0 as f32,
                self.origin.y + self.size.line_height * row as f32,
            ),
            size(self.size.cell_width, self.size.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

/// Copied text without the line breaks a program put in only because the line was full (tmux
/// and full-screen programs draw a long link as rows of the terminal's width): a full row that
/// ends in text and is followed by text joins the next. Box-drawing rows stay as they are.
fn unwrap_lines(text: &str, cols: usize) -> String {
    let boxy = |c: char| ('\u{2500}'..='\u{257F}').contains(&c);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = String::with_capacity(text.len());
    for (i, line) in lines.iter().enumerate() {
        out.push_str(line);
        let Some(next) = lines.get(i + 1) else { break };
        let full = line.chars().count() >= cols && cols > 0;
        let joins = full
            && line.chars().last().is_some_and(|c| !c.is_whitespace() && !boxy(c))
            && next.chars().next().is_some_and(|c| !c.is_whitespace() && !boxy(c));
        if !joins {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod copy_tests {
    #[test]
    fn a_long_link_copies_in_one_piece() {
        let url = "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a";
        let (a, b) = url.split_at(30);
        assert_eq!(super::unwrap_lines(&format!("{a}\n{b}"), 30), url);
        assert_eq!(super::unwrap_lines("short line\nnext", 30), "short line\nnext");
        let rule = "─".repeat(30);
        assert_eq!(super::unwrap_lines(&format!("{rule}\nTitle"), 30), format!("{rule}\nTitle"));
    }
}

#[cfg(all(test, windows))]
mod spawn_tests {
    use super::*;

    /// The exit code of the default shell that runs `script` in a terminal, or None when it did
    /// not stop in time.
    fn shell_exit_code(script: &str, cwd: PathBuf, env: HashMap<String, String>) -> Option<i32> {
        shell_exit_code_after(script, cwd, env, Vec::new())
    }

    /// The same, with `input` typed into the terminal as it starts.
    fn shell_exit_code_after(
        script: &str,
        cwd: PathBuf,
        env: HashMap<String, String>,
        input: Vec<u8>,
    ) -> Option<i32> {
        let shell = glasscode_core::sys::default_shell();
        let args = shell
            .command_args
            .into_iter()
            .chain([script.to_string()])
            .collect();
        let spec = SpawnSpec {
            program: shell.program,
            args,
            cwd,
            env,
        };
        exit_code(spec, input)
    }

    /// The exit code of `spec` started in a terminal, as a session starts, with `input` typed as
    /// it starts; None when it did not stop in time.
    fn exit_code(spec: SpawnSpec, input: Vec<u8>) -> Option<i32> {
        let prepared = prepare(spec).expect("the terminal starts");
        if !input.is_empty() {
            prepared.notifier.notify(input);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut code = None;
        while std::time::Instant::now() < deadline {
            match prepared.rx.try_recv() {
                Ok(AlacEvent::ChildExit(status)) => {
                    code = status.code();
                    break;
                }
                Ok(_) => {}
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        let _ = prepared.notifier.0.send(Msg::Shutdown);
        code
    }

    #[test]
    fn the_default_shell_runs_in_a_terminal() {
        assert_eq!(
            shell_exit_code("exit 3", std::env::temp_dir(), HashMap::new()),
            Some(3)
        );
    }

    #[test]
    fn a_shell_starts_in_a_folder_with_a_space_and_knows_the_terminal() {
        // As a worktree below a profile folder such as C:\Users\Ada Lovelace.
        let dir =
            std::env::temp_dir().join(format!("glasscode terminal test {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let here = dir.to_string_lossy().replace('\'', "''");
        let env = HashMap::from([(
            "GLASSCODE_WORKTREE".to_string(),
            dir.to_string_lossy().into_owned(),
        )]);
        // DIAGNOSTIC (temporary): what the shell sees when the folder differs.
        let seen = std::env::temp_dir().join(format!("glasscode-seen-{}.txt", std::process::id()));
        let seen_q = seen.to_string_lossy().replace('\'', "''");
        let script = format!(
            "if ((Get-Location).Path -ne '{here}') {{ Set-Content -LiteralPath '{seen_q}' -Value ((Get-Location).Path + '|' + $env:TEMP); exit 1 }}; \
             if ($env:TERM -ne 'xterm-256color' -or $env:COLORTERM -ne 'truecolor' -or $env:TERM_PROGRAM -ne 'Glasscode') {{ exit 2 }}; \
             if ($env:GLASSCODE_WORKTREE -ne '{here}') {{ exit 4 }}; exit 3"
        );
        let code = shell_exit_code(&script, dir.clone(), env);
        let canonical = std::fs::canonicalize(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        let shell_saw = std::fs::read(&seen).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let _ = std::fs::remove_file(&seen);
        assert_eq!(
            code,
            Some(3),
            "1: work folder, 2: terminal variables, 4: session variable. DIAGNOSTIC temp_dir={:?} here={here:?} canonical={canonical:?} shell_saw(location|TEMP)={shell_saw:?}",
            std::env::temp_dir()
        );
    }

    #[test]
    fn text_windows_types_reaches_the_program_as_typed() {
        // What an IME, the emoji panel and a dead key commit, then Enter from the key-down handler.
        let typed = "日本語 😀 é";
        let mut input = keys::TypedText::default().commit(typed).unwrap();
        input.extend(b"\r");
        let script = format!("if ((Read-Host) -eq '{typed}') {{ exit 3 }} else {{ exit 1 }}");
        assert_eq!(
            shell_exit_code_after(&script, std::env::temp_dir(), HashMap::new(), input),
            Some(3)
        );
    }

    #[test]
    fn a_program_in_a_folder_with_a_space_starts_with_its_arguments_whole() {
        // As `node.exe` in `C:\Program Files\nodejs`, `pwsh.exe` in `C:\Program Files\PowerShell\7`,
        // or an agent below a profile folder such as `C:\Users\Ada Lovelace`.
        let base = std::env::temp_dir().join(format!("glasscode-spawn-{}", std::process::id()));
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        let spec =
            |program: &std::path::Path, args: &[&str], env: HashMap<String, String>| SpawnSpec {
                program: program.to_string_lossy().into_owned(),
                args: args.iter().map(|a| a.to_string()).collect(),
                cwd: base.clone(),
                env,
            };

        // Windows tries `<base>\glasscode.exe` first for an unquoted `<base>\glasscode programs\…`.
        let programs = base.join("glasscode programs");
        std::fs::create_dir_all(&programs).unwrap();
        let cmd = programs.join("cmd.exe");
        std::fs::copy(system.join("cmd.exe"), &cmd).unwrap();
        std::fs::copy(system.join("where.exe"), base.join("glasscode.exe")).unwrap();
        let started = exit_code(spec(&cmd, &["/c", "exit 7"], HashMap::new()), Vec::new());

        // A program reads its own path and each argument as they were given: a copy of this test
        // program runs `write_the_arguments`, which writes what it got.
        let tools = base.join("other tools");
        std::fs::create_dir_all(&tools).unwrap();
        let probe = tools.join("glasscode-probe.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &probe).unwrap();
        let written = base.join("arguments.txt");
        let args = [
            "terminal::spawn_tests::write_the_arguments",
            "--exact",
            "--ignored",
            "two words",
        ];
        let env = HashMap::from([(
            "GLASSCODE_TEST_ARGUMENTS".to_string(),
            written.to_string_lossy().into_owned(),
        )]);
        let probed = exit_code(spec(&probe, &args, env), Vec::new());
        let seen = std::fs::read_to_string(&written).unwrap_or_default();

        let _ = std::fs::remove_dir_all(&base);
        let program = probe.to_string_lossy();
        let expected: Vec<&str> = [program.as_ref()].into_iter().chain(args).collect();
        assert_eq!(
            (started, probed, seen.lines().collect::<Vec<_>>()),
            (Some(7), Some(0), expected),
            "cmd /c exit 7 (2: Windows started <base>\\glasscode.exe in its place); the probe's \
             exit code and the arguments it got"
        );
    }

    /// Run by the test above from a copy of this test program: writes the arguments it got, one
    /// on each line, to the file `GLASSCODE_TEST_ARGUMENTS` names.
    #[test]
    #[ignore = "a helper the test above runs"]
    fn write_the_arguments() {
        if let Some(file) = std::env::var_os("GLASSCODE_TEST_ARGUMENTS") {
            std::fs::write(file, std::env::args().collect::<Vec<_>>().join("\n")).unwrap();
        }
    }
}

/// Everything the paint pass needs, computed once per frame while the grid is locked.
struct Frame {
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<(Bounds<Pixels>, CursorShape, bool)>,
    /// An IME's text not typed yet, over the cursor (only Windows composes; see `TypedText`).
    composing: Option<(Point<Pixels>, ShapedLine)>,
    line_height: Pixels,
}

#[derive(Clone, PartialEq)]
struct RunStyle {
    color: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

fn mono_font(bold: bool, italic: bool) -> Font {
    let mut f = font(crate::theme::MONO_FONT.family());
    if bold {
        f.weight = FontWeight::BOLD;
    }
    if italic {
        f = f.italic();
    }
    f
}

fn prepaint_terminal(
    view: &gpui::Entity<TerminalView>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> Frame {
    let font_size = px(crate::theme::terminal_font_size());
    let text_system = window.text_system().clone();
    let font_id = text_system.resolve_font(&mono_font(false, false));
    let cell_width = text_system
        .advance(font_id, font_size, 'm')
        .map(|advance| advance.width)
        .unwrap_or(px(8.));
    let line_height = px((f32::from(font_size) * LINE_HEIGHT_RATIO).round());
    let cols = ((f32::from(bounds.size.width) / f32::from(cell_width)) as usize).max(10);
    let lines = ((f32::from(bounds.size.height) / f32::from(line_height)) as usize).max(4);
    let focused = view.read(cx).focus.is_focused(window);
    view.update(cx, |view, _| {
        view.origin = bounds.origin;
        view.set_size(GridSize {
            cols,
            lines,
            cell_width,
            line_height,
        });
        // A composition goes on where the keyboard went; it must not show here again later.
        if !focused {
            view.typed.compose("");
        }
    });
    let composing = view.read(cx).typed.composing().map(str::to_string);

    let term = view.read(cx).term.clone();
    let term = term.lock();
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let overrides = content.colors;
    let default_bg = colors::background();
    let show_cursor = content.mode.contains(TermMode::SHOW_CURSOR)
        && content.cursor.shape != CursorShape::Hidden
        && offset == 0;
    let cursor_point = content.cursor.point;
    // The default (block) cursor takes the shape chosen in Themes; a program's own stays.
    let cursor_shape = match (content.cursor.shape, crate::theme::cursor_style()) {
        (CursorShape::Block, glasscode_core::model::CursorStyle::Bar) => CursorShape::Beam,
        (CursorShape::Block, glasscode_core::model::CursorStyle::Underline) => CursorShape::Underline,
        (shape, _) => shape,
    };
    let selection = content.selection;

    let mut backgrounds: Vec<(Bounds<Pixels>, Hsla)> = Vec::new();
    let mut frame_lines = Vec::new();

    // Current run being accumulated for the row: start column, text, style.
    let mut row_runs: Vec<(usize, String, RunStyle)> = Vec::new();
    let mut current_row: Option<i32> = None;

    let flush_row = |row: i32,
                     runs: &mut Vec<(usize, String, RunStyle)>,
                     out: &mut Vec<(Point<Pixels>, ShapedLine)>| {
        for (col, text, style) in runs.drain(..) {
            if text.trim().is_empty() && !style.underline && !style.strike {
                continue;
            }
            let run = TextRun {
                len: text.len(),
                font: mono_font(style.bold, style.italic),
                color: style.color,
                background_color: None,
                underline: style.underline.then(|| UnderlineStyle {
                    thickness: px(1.),
                    color: Some(style.color),
                    wavy: false,
                }),
                strikethrough: style.strike.then(|| StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(style.color),
                }),
            };
            let wide = text.chars().count() == 1
                && text.chars().next().is_some_and(|c| unicode_wide(c));
            let shaped = text_system.shape_line(
                SharedString::from(text),
                font_size,
                &[run],
                if wide { None } else { Some(cell_width) },
            );
            let origin = point(
                bounds.origin.x + cell_width * col as f32,
                bounds.origin.y + line_height * row as f32,
            );
            out.push((origin, shaped));
        }
    };

    for indexed in content.display_iter {
        let row = indexed.point.line.0 + offset;
        let col = indexed.point.column.0;
        if current_row != Some(row) {
            if let Some(prev) = current_row {
                flush_row(prev, &mut row_runs, &mut frame_lines);
            }
            current_row = Some(row);
        }
        let cell = &indexed.cell;
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }

        let mut fg_color = cell.fg;
        if cell.flags.contains(Flags::BOLD) {
            fg_color = colors::brighten(fg_color);
        }
        let mut fg = colors::resolve(fg_color, overrides);
        let mut bg = colors::resolve(cell.bg, overrides);
        if cell.flags.contains(Flags::DIM) {
            fg = colors::dimmed(fg);
        }
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let is_selected = selection.is_some_and(|s| s.contains(indexed.point));
        if is_selected {
            bg = colors::selection();
        }
        let is_cursor = show_cursor && indexed.point == cursor_point;
        if is_cursor && focused && cursor_shape == CursorShape::Block {
            fg = default_bg;
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }

        let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2.0 } else { 1.0 };
        if bg != default_bg || is_selected {
            let cell_bounds = Bounds::new(
                point(
                    bounds.origin.x + cell_width * col as f32,
                    bounds.origin.y + line_height * row as f32,
                ),
                size(cell_width * width, line_height),
            );
            let color = colors::to_hsla(bg);
            match backgrounds.last_mut() {
                Some((last, last_color))
                    if *last_color == color
                        && last.origin.y == cell_bounds.origin.y
                        && (last.origin.x + last.size.width - cell_bounds.origin.x).abs()
                            < px(0.5) =>
                {
                    last.size.width += cell_bounds.size.width;
                }
                _ => backgrounds.push((cell_bounds, color)),
            }
        }

        let style = RunStyle {
            color: colors::to_hsla(fg),
            bold: cell.flags.contains(Flags::BOLD),
            italic: cell.flags.contains(Flags::ITALIC),
            underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
            strike: cell.flags.contains(Flags::STRIKEOUT),
        };
        let mut text = String::new();
        text.push(if cell.c == '\t' { ' ' } else { cell.c });
        if let Some(extra) = cell.zerowidth() {
            text.extend(extra.iter());
        }
        let wide = cell.flags.contains(Flags::WIDE_CHAR);
        match row_runs.last_mut() {
            Some((start, run_text, run_style))
                if !wide
                    && *run_style == style
                    && *start + run_text.chars().filter(|c| !is_zero_width(*c)).count() == col
                    && !run_text.chars().any(unicode_wide) =>
            {
                run_text.push_str(&text);
            }
            _ => row_runs.push((col, text, style)),
        }
    }
    if let Some(row) = current_row {
        flush_row(row, &mut row_runs, &mut frame_lines);
    }

    let cursor = show_cursor.then(|| {
        let row = cursor_point.line.0 + offset;
        let cursor_bounds = Bounds::new(
            point(
                bounds.origin.x + cell_width * cursor_point.column.0 as f32,
                bounds.origin.y + line_height * row as f32,
            ),
            size(cell_width, line_height),
        );
        (cursor_bounds, cursor_shape, focused)
    });
    let composing = composing.map(|text| {
        let color = colors::to_hsla(colors::foreground());
        let run = TextRun {
            len: text.len(),
            font: mono_font(false, false),
            color,
            background_color: None,
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(color),
                wavy: false,
            }),
            strikethrough: None,
        };
        let origin = point(
            bounds.origin.x + cell_width * cursor_point.column.0 as f32,
            bounds.origin.y + line_height * (cursor_point.line.0 + offset) as f32,
        );
        (
            origin,
            text_system.shape_line(SharedString::from(text), font_size, &[run], None),
        )
    });

    Frame {
        backgrounds,
        lines: frame_lines,
        cursor,
        composing,
        line_height,
    }
}

fn is_zero_width(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036f | 0x200b..=0x200f | 0xfe00..=0xfe0f)
}

fn unicode_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115f | 0x2e80..=0xa4cf | 0xac00..=0xd7a3 | 0xf900..=0xfaff |
        0xfe30..=0xfe4f | 0xff00..=0xff60 | 0xffe0..=0xffe6 | 0x1f300..=0x1f64f |
        0x1f900..=0x1f9ff | 0x20000..=0x3fffd)
}

fn paint_terminal(frame: Frame, window: &mut Window, cx: &mut App) {
    for (bounds, color) in &frame.backgrounds {
        window.paint_quad(fill(*bounds, *color));
    }
    let cursor_color = colors::to_hsla(colors::cursor());
    if let Some((bounds, shape, focused)) = frame.cursor {
        if focused && shape == CursorShape::Block {
            window.paint_quad(fill(bounds, cursor_color));
        }
    }
    for (origin, line) in &frame.lines {
        let _ = line.paint(*origin, frame.line_height, TextAlign::Left, None, window, cx);
    }
    if let Some((bounds, shape, focused)) = frame.cursor {
        match shape {
            CursorShape::Beam => {
                let beam = Bounds::new(bounds.origin, size(px(2.), bounds.size.height));
                window.paint_quad(fill(beam, cursor_color));
            }
            CursorShape::Underline => {
                let underline = Bounds::new(
                    point(bounds.origin.x, bounds.origin.y + bounds.size.height - px(2.)),
                    size(bounds.size.width, px(2.)),
                );
                window.paint_quad(fill(underline, cursor_color));
            }
            CursorShape::Block if focused => {}
            _ => window.paint_quad(outline(bounds, cursor_color, BorderStyle::Solid)),
        }
    }
    if let Some((origin, line)) = frame.composing {
        let covered = Bounds::new(origin, size(line.width, frame.line_height));
        window.paint_quad(fill(covered, colors::to_hsla(colors::background())));
        let _ = line.paint(origin, frame.line_height, TextAlign::Left, None, window, cx);
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let input = view.clone();
        let focus = self.focus.clone();
        let bg = colors::to_hsla(colors::background());
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .bg(bg)
            .px_2()
            .pt_1()
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_drop(cx.listener(Self::drop_paths))
            .drag_over::<gpui::ExternalPaths>(|style, _, _, _| style.bg(gpui::Hsla { a: 0.08, ..crate::theme::accent() }))
            .on_action(cx.listener(Self::clear_scrollback))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .child(
                canvas(
                    move |bounds, window, cx| prepaint_terminal(&view, bounds, window, cx),
                    move |bounds, frame, window, cx| {
                        // Windows gives text it types as characters (an IME, the emoji panel, a
                        // dead key, `SendInput`) only to an input handler. macOS types every key
                        // from `on_key_down`, and its press-and-hold accents stay as they were.
                        if cfg!(windows) {
                            window.handle_input(
                                &focus,
                                ElementInputHandler::new(bounds, input),
                                cx,
                            );
                        }
                        paint_terminal(frame, window, cx)
                    },
                )
                .size_full(),
            )
    }
}

impl TerminalView {
    pub fn focus_handle_clone(&self) -> FocusHandle {
        self.focus.clone()
    }
}
