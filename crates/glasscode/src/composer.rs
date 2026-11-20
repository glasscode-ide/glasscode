//! The Chat view's message box: several lines that wrap, Enter to send, Shift-Enter (or
//! Option-Enter) for a new line, ↑ and ↓ between lines. Built like `text_input` (the same
//! editing and input-method handling), with wrapped text instead of one shaped line.

use std::ops::Range;

use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, Size, Style, TextRun, UTF16Selection,
    UnderlineStyle, Window, WrappedLine, actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme;

actions!(
    composer,
    [
        Backspace, Delete, Left, Right, Up, Down, SelectLeft, SelectRight, SelectUp, SelectDown, SelectAll, Home, End, Paste, Cut, Copy, Submit, SubmitNow, Newline, Cancel,
        WordLeft, WordRight, DeleteWordLeft, DeleteToStart, CycleMode
    ]
);

pub fn bind_keys(cx: &mut App) {
    crate::keymap::bind(cx, Some("Composer"));
}

pub enum ComposerEvent {
    /// Enter.
    Submit,
    /// Cmd+Enter (Ctrl+Enter on Windows): send even while the agent works (Claude queues it, Codex steers with it).
    SubmitNow,
    /// Escape.
    Cancel,
    /// Shift-Tab: the agent's next mode (auto, plan, accept edits…), as in its terminal.
    CycleMode,
    /// ↑ or ↓ in an empty box (an open question's options move with them).
    Up,
    Down,
    Changed,
    /// Paste with an image (or files) on the clipboard: the session attaches it.
    PasteFiles(Vec<crate::terminal::files::Incoming>),
}

/// Lines shown before the box scrolls.
const MAX_LINES: usize = 10;

pub struct Composer {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    /// Where ↑ and ↓ keep the cursor (the x it had before moving between lines).
    goal_x: Option<Pixels>,
    /// The last layout, for the mouse, ↑/↓ and the input method.
    last: Option<Laid>,
    /// How far the text is scrolled up (when longer than `MAX_LINES`).
    scroll: Pixels,
    is_selecting: bool,
    /// ↑ and ↓ go out (as `Up`/`Down`) even with text in the box: the agent is asking
    /// something and they move its options.
    pub arrows_out: bool,
    /// What of the text the person did not type here (pasted, or set by the app), for the words
    /// ledger: only typed text can be the person's words.
    typing: glasscode_core::words::Typed,
    /// A paste is being put in (its text is not typed).
    pasting: bool,
}

/// Shaped text: one wrapped line per paragraph, with where each starts.
struct Laid {
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    lines: Vec<WrappedLine>,
    /// Byte offset of each paragraph, and its top (before scrolling).
    starts: Vec<(usize, Pixels)>,
}

impl Laid {
    /// The paragraph holding `offset`.
    fn paragraph(&self, offset: usize) -> usize {
        self.starts.iter().rposition(|(start, _)| *start <= offset).unwrap_or(0)
    }

    /// Where `offset` is, relative to the text's top-left (before scrolling).
    fn position(&self, offset: usize) -> Point<Pixels> {
        let p = self.paragraph(offset);
        let (start, top) = self.starts[p];
        let local = self.lines[p].position_for_index(offset - start, self.line_height).unwrap_or_default();
        point(local.x, top + local.y)
    }

    /// The offset at a point relative to the text's top-left (before scrolling).
    fn offset_at(&self, at: Point<Pixels>) -> usize {
        let p = self.starts.iter().rposition(|(_, top)| *top <= at.y).unwrap_or(0);
        let (start, top) = self.starts[p];
        let local = point(at.x.max(px(0.)), (at.y - top).max(px(0.)));
        let index = match self.lines[p].closest_index_for_position(local, self.line_height) {
            Ok(i) | Err(i) => i,
        };
        start + index.min(self.lines[p].len())
    }

    fn height(&self) -> Pixels {
        self.starts.last().map(|(_, top)| *top).unwrap_or_default() + self.lines.last().map(|l| l.size(self.line_height).height).unwrap_or(self.line_height)
    }
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: SharedString::default(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            goal_x: None,
            last: None,
            scroll: px(0.),
            is_selecting: false,
            arrows_out: false,
            typing: Default::default(),
            pasting: false,
        }
    }

    pub fn text(&self) -> String {
        self.content.to_string()
    }

    /// What of [`Composer::text`] the person did not type here.
    pub fn typing(&self) -> &glasscode_core::words::Typed {
        &self.typing
    }

    /// Text the app puts in the box (a draft brought back, dictation) counts as not typed by the
    /// person.
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = text.into();
        self.typing.set(self.content.len(), false);
        self.selected_range = self.content.len()..self.content.len();
        self.marked_range = None;
        self.goal_x = None;
        cx.emit(ComposerEvent::Changed);
        cx.notify();
    }

    /// Add text where the cursor is (typing that started in the conversation).
    pub fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, text, window, cx);
    }

    /// What is selected (empty when nothing is).
    pub fn selected_text(&self) -> String {
        self.content[self.selected_range.clone()].to_string()
    }

    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>) {
        self.placeholder = placeholder.into();
    }

    pub fn focus_handle_clone(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    /// The offset one visual line up or down from the cursor, keeping its x.
    fn vertical(&mut self, down: bool) -> usize {
        let cursor = self.cursor_offset();
        let Some(laid) = self.last.as_ref() else { return cursor };
        let at = laid.position(cursor);
        let x = *self.goal_x.get_or_insert(at.x);
        let y = if down { at.y + laid.line_height * 1.5 } else { at.y - laid.line_height * 0.5 };
        if y < px(0.) {
            return 0;
        }
        if y >= laid.height() {
            return self.content.len();
        }
        laid.offset_at(point(x, y))
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.content.is_empty() || self.arrows_out {
            cx.emit(ComposerEvent::Up);
            return;
        }
        let goal = self.goal_x;
        let to = self.vertical(false);
        self.move_to(to, cx);
        self.goal_x = goal.or(self.goal_x);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.content.is_empty() || self.arrows_out {
            cx.emit(ComposerEvent::Down);
            return;
        }
        let goal = self.goal_x;
        let to = self.vertical(true);
        self.move_to(to, cx);
        self.goal_x = goal.or(self.goal_x);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let to = self.vertical(false);
        self.select_to(to, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let to = self.vertical(true);
        self.select_to(to, cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(self.cursor_offset()), cx);
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_word(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete_to_start(&mut self, _: &DeleteToStart, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let line_start = self.content[..self.cursor_offset()].rfind('\n').map(|i| i + 1).unwrap_or(0);
            self.select_to(line_start, cx);
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let line_start = self.content[..self.cursor_offset()].rfind('\n').map(|i| i + 1).unwrap_or(0);
        self.move_to(if line_start == self.cursor_offset() { 0 } else { line_start }, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let line_end = self.content[cursor..].find('\n').map(|i| cursor + i).unwrap_or(self.content.len());
        self.move_to(if line_end == cursor { self.content.len() } else { line_end }, cx);
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(ComposerEvent::Submit);
    }

    fn submit_now(&mut self, _: &SubmitNow, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(ComposerEvent::SubmitNow);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn cycle_mode(&mut self, _: &CycleMode, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(ComposerEvent::CycleMode);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(ComposerEvent::Cancel);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                return;
            }
            self.select_to(prev, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                return;
            }
            self.select_to(next, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        self.is_selecting = true;
        let at = self.index_for_mouse_position(event.position);
        if event.modifiers.shift {
            self.select_to(at, cx);
        } else {
            self.move_to(at, cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else { return };
        let files = crate::terminal::files::from_clipboard(&item);
        if !files.is_empty() {
            cx.emit(ComposerEvent::PasteFiles(files));
            return;
        }
        if let Some(text) = item.text() {
            self.pasting = true;
            self.replace_text_in_range(None, &text.replace("\r\n", "\n"), window, cx);
            self.pasting = false;
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if crate::chat_ui::debug_no_clipboard() {
            eprintln!("[chat-debug] message box copies: {:?}", self.selected_text());
            return;
        }
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.goal_x = None;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed { self.selected_range.start } else { self.selected_range.end }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let Some(laid) = self.last.as_ref() else { return 0 };
        if self.content.is_empty() {
            return 0;
        }
        let local = point(position.x - laid.bounds.left(), position.y - laid.bounds.top() + self.scroll);
        if local.y < px(0.) {
            return 0;
        }
        if local.y >= laid.height() {
            return self.content.len();
        }
        laid.offset_at(local)
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content.grapheme_indices(true).rev().find_map(|(idx, _)| (idx < offset).then_some(idx)).unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content.grapheme_indices(true).find_map(|(idx, _)| (idx > offset).then_some(idx)).unwrap_or(self.content.len())
    }

    fn previous_word(&self, offset: usize) -> usize {
        self.content.unicode_word_indices().rev().find_map(|(idx, _)| (idx < offset).then_some(idx)).unwrap_or(0)
    }

    fn next_word(&self, offset: usize) -> usize {
        self.content.unicode_word_indices().find_map(|(idx, word)| (idx + word.len() > offset).then_some(idx + word.len())).unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for Composer {
    fn text_for_range(&mut self, range_utf16: Range<usize>, actual_range: &mut Option<Range<usize>>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _ignore_disabled_input: bool, _window: &mut Window, _cx: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected_range), reversed: self.selection_reversed })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, _: &mut Window, cx: &mut Context<Self>) {
        let range = range_utf16.as_ref().map(|r| self.range_from_utf16(r)).or(self.marked_range.clone()).unwrap_or(self.selected_range.clone());
        self.typing.edit(range.clone(), new_text.len(), !self.pasting);
        self.content = (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        self.goal_x = None;
        cx.emit(ComposerEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, new_selected_range_utf16: Option<Range<usize>>, _window: &mut Window, cx: &mut Context<Self>) {
        let range = range_utf16.as_ref().map(|r| self.range_from_utf16(r)).or(self.marked_range.clone()).unwrap_or(self.selected_range.clone());
        // An input method's text, while it composes: typed.
        self.typing.edit(range.clone(), new_text.len(), true);
        self.content = (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        self.marked_range = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|new_range| new_range.start + range.start..new_range.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        cx.notify();
    }

    fn bounds_for_range(&mut self, range_utf16: Range<usize>, bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let laid = self.last.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let start = laid.position(range.start);
        let end = laid.position(range.end);
        let top = bounds.top() - self.scroll;
        Some(Bounds::from_corners(point(bounds.left() + start.x, top + start.y), point(bounds.left() + end.x.max(start.x), top + end.y + laid.line_height)))
    }

    fn character_index_for_point(&mut self, at: Point<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        let laid = self.last.as_ref()?;
        let local = point(at.x - laid.bounds.left(), at.y - laid.bounds.top() + self.scroll);
        Some(self.offset_to_utf16(laid.offset_at(local)))
    }
}

/// Shape `text` in paragraphs wrapped at `width`.
fn shape(text: &SharedString, color: gpui::Hsla, marked: Option<&Range<usize>>, width: Pixels, window: &mut Window) -> (Vec<WrappedLine>, Vec<(usize, Pixels)>, Pixels) {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let line_height = window.line_height();
    let run = TextRun { len: text.len(), font: style.font(), color, background_color: None, underline: None, strikethrough: None };
    let runs: Vec<TextRun> = match marked {
        Some(m) => vec![
            TextRun { len: m.start, ..run.clone() },
            TextRun { len: m.end - m.start, underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false }), ..run.clone() },
            TextRun { len: text.len() - m.end, ..run },
        ]
        .into_iter()
        .filter(|r| r.len > 0)
        .collect(),
        None => vec![run],
    };
    let lines: Vec<WrappedLine> = window.text_system().shape_text(text.clone(), font_size, &runs, Some(width), None).map(|l| l.into_vec()).unwrap_or_default();
    let mut starts = Vec::with_capacity(lines.len());
    let (mut offset, mut top) = (0, px(0.));
    for line in &lines {
        starts.push((offset, top));
        offset += line.len() + 1;
        top += line.size(line_height).height;
    }
    (lines, starts, top.max(line_height))
}

struct ComposerElement {
    input: Entity<Composer>,
}

impl IntoElement for ComposerElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct Prepaint {
    laid: Option<Laid>,
    cursor: Option<gpui::PaintQuad>,
    selection: Vec<gpui::PaintQuad>,
    placeholder: bool,
}

impl Element for ComposerElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let input = self.input.read(cx);
        let text = if input.content.is_empty() { input.placeholder.clone() } else { input.content.clone() };
        let line_height = window.line_height();
        let layout = window.request_measured_layout(style, move |known: Size<Option<Pixels>>, available: Size<AvailableSpace>, window, _cx| {
            let width = known.width.unwrap_or(match available.width {
                AvailableSpace::Definite(w) => w,
                _ => theme::sz(600.),
            });
            let (_, _, height) = shape(&text, theme::text(), None, width, window);
            size(width, height.min(line_height * MAX_LINES as f32))
        });
        (layout, ())
    }

    fn prepaint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> Prepaint {
        let input = self.input.read(cx);
        let placeholder = input.content.is_empty();
        // The box lies on glass (the Chat view, Today's start box): its colours are the text
        // colours that read there (plan rule 12).
        let (text, color) = if placeholder { (input.placeholder.clone(), theme::card_soft_text()) } else { (input.content.clone(), theme::text()) };
        let marked = input.marked_range.clone();
        let selected = input.selected_range.clone();
        let cursor_at = input.cursor_offset();
        let mut scroll = input.scroll;
        let (lines, starts, height) = shape(&text, color, marked.as_ref(), bounds.size.width, window);
        let line_height = window.line_height();
        let laid = Laid { bounds, line_height, lines, starts };
        // Keep the cursor in view when the text is taller than the box.
        let cursor = if placeholder { point(px(0.), px(0.)) } else { laid.position(cursor_at) };
        if cursor.y < scroll {
            scroll = cursor.y;
        } else if cursor.y + line_height > scroll + bounds.size.height {
            scroll = cursor.y + line_height - bounds.size.height;
        }
        scroll = scroll.min((height - bounds.size.height).max(px(0.))).max(px(0.));
        let origin = point(bounds.left(), bounds.top() - scroll);
        let caret = selected.is_empty().then(|| fill(Bounds::new(point(origin.x + cursor.x, origin.y + cursor.y), size(px(1.5), line_height)), theme::accent()));
        let mut selection = Vec::new();
        if !selected.is_empty() && !placeholder {
            let mut color = theme::accent();
            color.a = 0.25;
            let (a, b) = (laid.position(selected.start), laid.position(selected.end));
            let row = |y: Pixels, from: Pixels, to: Pixels| fill(Bounds::from_corners(point(origin.x + from, origin.y + y), point(origin.x + to, origin.y + y + line_height)), color);
            if a.y == b.y {
                selection.push(row(a.y, a.x, b.x));
            } else {
                selection.push(row(a.y, a.x, bounds.size.width));
                let mut y = a.y + line_height;
                while y < b.y {
                    selection.push(row(y, px(0.), bounds.size.width));
                    y += line_height;
                }
                selection.push(row(b.y, px(0.), b.x));
            }
        }
        self.input.update(cx, |input, _| input.scroll = scroll);
        Prepaint { laid: Some(laid), cursor: caret, selection, placeholder }
    }

    fn paint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), prepaint: &mut Prepaint, window: &mut Window, cx: &mut App) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let Some(laid) = prepaint.laid.take() else { return };
        let scroll = self.input.read(cx).scroll;
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for quad in prepaint.selection.drain(..) {
                window.paint_quad(quad);
            }
            for (line, (_, top)) in laid.lines.iter().zip(&laid.starts) {
                let _ = line.paint(point(bounds.left(), bounds.top() - scroll + *top), laid.line_height, gpui::TextAlign::Left, None, window, cx);
            }
            if focus_handle.is_focused(window) {
                if let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }
            }
        });
        let _ = prepaint.placeholder;
        self.input.update(cx, |input, _| input.last = Some(laid));
    }
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .w_full()
            .key_context("Composer")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_to_start))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::submit_now))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::cycle_mode))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(ComposerElement { input: cx.entity() })
    }
}

impl Focusable for Composer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
