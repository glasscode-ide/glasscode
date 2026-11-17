//! The editor of a file tab (U20): the text of one file as lines, a cursor and a selection,
//! undo and redo, the clipboard and the input method, and saving that never loses text.
//!
//! The parts:
//! - the text: `Text` (lines and the end of each), `Buffer` (a file's text, its byte order mark
//!   and the line end new lines get), `Doc` (the buffer with the selection and the undo history:
//!   every edit goes through it, and it has no window, so the tests drive it);
//! - the file: `decode` (which files can be edited, and why not), `read_for_edit`,
//!   `write_atomic` (a temporary file in the same folder, renamed over the file) and `on_disk`
//!   (what a change on the disk does);
//! - the entity `Editor` and its element, which shapes only the lines on screen.
//!
//! Positions are a line and a byte offset in it. Each key's work is on its line only: moving by
//! characters, by words, the input method's UTF-16 offsets. A line inserted in the middle moves
//! the lines after it in the `Vec` (about a tenth of a millisecond at 2 MB, `a_2_mb_file...`).

use std::collections::VecDeque;
use std::hash::{Hash as _, Hasher as _};
use std::io::Read as _;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font,
    GlobalElementId, Hsla, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ScrollWheelEvent, ShapedLine, SharedString, Style,
    Task, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use crate::file_view::{Stamp, size_words, stamp};
use crate::theme;
use crate::ui;

/// Larger files stay read only.
pub const MAX_EDIT_BYTES: u64 = 2 * 1024 * 1024;
/// A line longer than this (in bytes) keeps its file read only: each key would lay the whole
/// line out again (a minified file is one line of megabytes).
pub const MAX_EDIT_LINE: usize = 20_000;
/// A NUL byte in this first part makes a file binary, as git and the file view decide.
const BINARY_PROBE: usize = 8000;
const BOM: &[u8] = b"\xEF\xBB\xBF";
/// Tab inserts this many spaces; a tab character in the file is drawn to the next stop.
pub const TAB: usize = 4;
/// The undo history keeps at most this many steps and this many bytes of text.
const MAX_STEPS: usize = 1000;
const MAX_HISTORY_BYTES: usize = 32 * 1024 * 1024;

// ----- the text ----------------------------------------------------------------------------------

/// How a line ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eol {
    /// The last line: no end.
    None,
    Lf,
    CrLf,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::None => "",
            Eol::Lf => "\n",
            Eol::CrLf => "\r\n",
        }
    }
}

/// Text as lines, each with its end. Every line but the last ends (`Lf` or `CrLf`); the last
/// one never does, so a file without a final line end has a last line with text, and one with
/// it has an empty last line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    lines: Vec<String>,
    eols: Vec<Eol>,
}

impl Text {
    pub fn empty() -> Text {
        Text { lines: vec![String::new()], eols: vec![Eol::None] }
    }

    /// One line, without an end (`s` has no "\n").
    pub fn line(s: &str) -> Text {
        Text { lines: vec![s.to_string()], eols: vec![Eol::None] }
    }

    /// A file's text, as it is: split at each "\n", and a "\r" before it is part of the end.
    /// Any other "\r" stays in its line as a character.
    pub fn parse(s: &str) -> Text {
        let mut lines = Vec::new();
        let mut eols = Vec::new();
        let mut rest = s;
        while let Some(i) = rest.find('\n') {
            let (line, eol) = match rest[..i].strip_suffix('\r') {
                Some(line) => (line, Eol::CrLf),
                None => (&rest[..i], Eol::Lf),
            };
            lines.push(line.to_string());
            eols.push(eol);
            rest = &rest[i + 1..];
        }
        lines.push(rest.to_string());
        eols.push(Eol::None);
        Text { lines, eols }
    }

    /// Typed or pasted text: each line end ("\r\n", "\n" or a lone "\r") becomes `eol`.
    pub fn typed(s: &str, eol: Eol) -> Text {
        let mut text = Text::parse(&s.replace("\r\n", "\n").replace('\r', "\n"));
        for e in text.eols.iter_mut().filter(|e| **e != Eol::None) {
            *e = eol;
        }
        text
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Its size in bytes, with the line ends.
    pub fn len(&self) -> usize {
        self.lines.iter().map(String::len).sum::<usize>() + self.eols.iter().map(|e| e.as_str().len()).sum::<usize>()
    }

    pub fn write_to(&self, out: &mut String) {
        for (line, eol) in self.lines.iter().zip(&self.eols) {
            out.push_str(line);
            out.push_str(eol.as_str());
        }
    }
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (line, eol) in self.lines.iter().zip(&self.eols) {
            f.write_str(line)?;
            f.write_str(eol.as_str())?;
        }
        Ok(())
    }
}

/// A place in the text: the line, and the byte offset in it (always at a character boundary).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }
}

/// Where `text` ends when it is put at `start`.
fn end_of(start: Pos, text: &Text) -> Pos {
    let last = text.lines.len() - 1;
    if last == 0 { Pos::new(start.line, start.col + text.lines[0].len()) } else { Pos::new(start.line + last, text.lines[last].len()) }
}

/// A file's text, with what saving needs to write it back byte for byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buffer {
    text: Text,
    /// The file began with a UTF-8 byte order mark; saving writes it again.
    bom: bool,
    /// The end a new line break gets where its line has none to copy (the last line): the
    /// file's most common end, LF in a file with none.
    style: Eol,
}

impl Buffer {
    pub fn new(text: &str, bom: bool) -> Buffer {
        let text = Text::parse(text);
        let crlf = text.eols.iter().filter(|e| **e == Eol::CrLf).count();
        let lf = text.eols.iter().filter(|e| **e == Eol::Lf).count();
        Buffer { text, bom, style: if crlf > lf { Eol::CrLf } else { Eol::Lf } }
    }

    pub fn lines(&self) -> usize {
        self.text.lines.len()
    }

    pub fn line(&self, i: usize) -> &str {
        &self.text.lines[i]
    }

    pub fn eol(&self, i: usize) -> Eol {
        self.text.eols[i]
    }

    pub fn end(&self) -> Pos {
        let last = self.lines() - 1;
        Pos::new(last, self.text.lines[last].len())
    }

    /// The bytes to save: the byte order mark if the file had one, then each line and its end.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = String::with_capacity(self.text.len() + 3);
        if self.bom {
            out.push('\u{FEFF}');
        }
        self.text.write_to(&mut out);
        out.into_bytes()
    }

    /// The end of a new line break in line `line`: the line's own end, so a file with mixed ends
    /// keeps them, or the file's most common end on the last line.
    pub fn break_in(&self, line: usize) -> Eol {
        match self.text.eols[line.min(self.lines() - 1)] {
            Eol::None => self.style,
            eol => eol,
        }
    }

    /// The nearest valid place: in the text, at a character boundary.
    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines() - 1);
        let s = &self.text.lines[line];
        let mut col = p.col.min(s.len());
        while !s.is_char_boundary(col) {
            col -= 1;
        }
        Pos::new(line, col)
    }

    pub fn extract(&self, range: Range<Pos>) -> Text {
        let (a, b) = (range.start, range.end);
        if a.line == b.line {
            return Text::line(&self.text.lines[a.line][a.col..b.col]);
        }
        let mut lines = Vec::with_capacity(b.line - a.line + 1);
        let mut eols = Vec::with_capacity(b.line - a.line + 1);
        lines.push(self.text.lines[a.line][a.col..].to_string());
        eols.push(self.text.eols[a.line]);
        for i in a.line + 1..b.line {
            lines.push(self.text.lines[i].clone());
            eols.push(self.text.eols[i]);
        }
        lines.push(self.text.lines[b.line][..b.col].to_string());
        eols.push(Eol::None);
        Text { lines, eols }
    }

    /// Put `new` in place of `range`; where it ends. The last line of `new` takes the end of the
    /// line `range` ends in.
    pub fn replace(&mut self, range: Range<Pos>, new: &Text) -> Pos {
        let (a, b) = (range.start, range.end);
        // Typing: one line changes in place, nothing else moves.
        if a.line == b.line && new.lines.len() == 1 {
            self.text.lines[a.line].replace_range(a.col..b.col, &new.lines[0]);
            return Pos::new(a.line, a.col + new.lines[0].len());
        }
        let tail = self.text.lines[b.line][b.col..].to_string();
        let tail_eol = self.text.eols[b.line];
        let head = &self.text.lines[a.line][..a.col];
        let n = new.lines.len();
        let mut lines = Vec::with_capacity(n);
        let mut eols = Vec::with_capacity(n);
        for (i, (line, eol)) in new.lines.iter().zip(&new.eols).enumerate() {
            let mut s = if i == 0 { head.to_string() } else { String::new() };
            s.push_str(line);
            if i == n - 1 {
                s.push_str(&tail);
                eols.push(tail_eol);
            } else {
                eols.push(*eol);
            }
            lines.push(s);
        }
        self.text.lines.splice(a.line..=b.line, lines);
        self.text.eols.splice(a.line..=b.line, eols);
        end_of(a, new)
    }
}

// ----- moving in a line --------------------------------------------------------------------------

/// The grapheme boundary before `col` (an emoji with its modifier, a flag, a letter with its
/// accent is one step).
pub fn prev_grapheme(s: &str, col: usize) -> usize {
    let mut cursor = GraphemeCursor::new(col, s.len(), true);
    match cursor.prev_boundary(s, 0) {
        Ok(Some(at)) => at,
        Ok(None) => 0,
        Err(_) => s[..col].char_indices().next_back().map_or(0, |(i, _)| i),
    }
}

pub fn next_grapheme(s: &str, col: usize) -> usize {
    let mut cursor = GraphemeCursor::new(col, s.len(), true);
    match cursor.next_boundary(s, 0) {
        Ok(Some(at)) => at,
        Ok(None) => s.len(),
        Err(_) => s[col..].chars().next().map_or(s.len(), |c| col + c.len_utf8()),
    }
}

/// `col` moved back to a grapheme boundary (a click between the parts of an emoji).
pub fn snap_grapheme(s: &str, col: usize) -> usize {
    let mut cursor = GraphemeCursor::new(col, s.len(), true);
    match cursor.is_boundary(s, 0) {
        Ok(true) | Err(_) => col,
        Ok(false) => prev_grapheme(s, col),
    }
}

/// The start of the word before `col` in the line, if there is one.
fn word_start_before(s: &str, col: usize) -> Option<usize> {
    s.unicode_word_indices().rev().find_map(|(i, _)| (i < col).then_some(i))
}

/// The end of the word after `col` in the line, if there is one.
fn word_end_after(s: &str, col: usize) -> Option<usize> {
    s.unicode_word_indices().find_map(|(i, w)| (i + w.len() > col).then_some(i + w.len()))
}

/// The part of the line a double click at `col` selects: a word, a run of spaces, a sign.
fn segment_at(s: &str, col: usize) -> Range<usize> {
    let mut last = 0..0;
    for (i, part) in s.split_word_bound_indices() {
        last = i..i + part.len();
        if col < i + part.len() {
            return last;
        }
    }
    last
}

fn indent_of(s: &str) -> usize {
    s.len() - s.trim_start_matches([' ', '\t']).len()
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// The byte offset of a UTF-16 offset in `s`.
fn col_of_utf16(s: &str, offset: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units >= offset {
            return i;
        }
        units += c.len_utf16();
    }
    s.len()
}

// ----- the undo history --------------------------------------------------------------------------

/// The selection: where it started (`anchor`) and where the cursor is (`head`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sel {
    pub anchor: Pos,
    pub head: Pos,
}

impl Sel {
    pub fn at(p: Pos) -> Sel {
        Sel { anchor: p, head: p }
    }

    pub fn range(&self) -> Range<Pos> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// What made an edit, for joining edits into one undo step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Letters typed (and the input method's text): a run of them is one step, a word each.
    Typing,
    Backspace,
    Delete,
    /// Each one is a step of its own: a paste, a cut, a new line, an indent.
    Other,
}

#[derive(Clone, Debug)]
struct Edit {
    start: Pos,
    removed: Text,
    inserted: Text,
}

#[derive(Debug)]
struct Step {
    id: u64,
    kind: Kind,
    edits: Vec<Edit>,
    before: Sel,
    after: Sel,
    /// Nothing more joins this step: it was saved, undone or redone, or a later step began.
    sealed: bool,
    /// The last character typed in it, to start a new step at each word.
    last_char: Option<char>,
    bytes: usize,
}

/// Steps to undo and redo, bounded in number and in bytes.
#[derive(Debug, Default)]
struct History {
    done: VecDeque<Step>,
    undone: Vec<Step>,
    next: u64,
    /// The revision when no step is left to undo (the id of the last step dropped for room).
    base: u64,
    bytes: usize,
}

impl History {
    fn revision(&self) -> u64 {
        self.done.back().map_or(self.base, |s| s.id)
    }

    fn seal(&mut self) {
        if let Some(last) = self.done.back_mut() {
            last.sealed = true;
        }
    }

    fn push(&mut self, edit: Edit, kind: Kind, before: Sel, after: Sel) {
        for step in self.undone.drain(..) {
            self.bytes -= step.bytes;
        }
        let size = edit.removed.len() + edit.inserted.len();
        let first = edit.inserted.lines[0].chars().next();
        let last_char = edit.inserted.lines.last().and_then(|l| l.chars().next_back());
        let joins = self.done.back().is_some_and(|last| {
            !last.sealed
                && last.kind == kind
                && kind != Kind::Other
                && last.after == before
                // Typing: each word is a step of its own (it starts after a space).
                && !(kind == Kind::Typing && first.is_some_and(|c| !c.is_whitespace()) && last.last_char.is_some_and(char::is_whitespace))
        });
        if joins {
            let last = self.done.back_mut().expect("a step to join");
            match last.edits.last_mut() {
                // Letters typed one after the other become one edit.
                Some(prev)
                    if kind == Kind::Typing
                        && prev.removed.is_empty()
                        && edit.removed.is_empty()
                        && prev.inserted.lines.len() == 1
                        && edit.inserted.lines.len() == 1
                        && end_of(prev.start, &prev.inserted) == edit.start =>
                {
                    prev.inserted.lines[0].push_str(&edit.inserted.lines[0]);
                }
                _ => last.edits.push(edit),
            }
            last.after = after;
            last.bytes += size;
            if last_char.is_some() {
                last.last_char = last_char;
            }
        } else {
            self.seal();
            self.next += 1;
            self.done.push_back(Step { id: self.next, kind, edits: vec![edit], before, after, sealed: false, last_char, bytes: size });
        }
        self.bytes += size;
        while self.done.len() + self.undone.len() > MAX_STEPS || (self.bytes > MAX_HISTORY_BYTES && self.done.len() > 1) {
            let Some(old) = self.done.pop_front() else { break };
            self.base = old.id;
            self.bytes -= old.bytes;
        }
    }
}

// ----- the document ------------------------------------------------------------------------------

/// A file's text with its selection and undo history. Every edit goes through here.
pub struct Doc {
    pub buf: Buffer,
    pub sel: Sel,
    history: History,
    /// The revision last saved (or read from the disk).
    saved: u64,
}

impl Doc {
    pub fn new(buf: Buffer) -> Doc {
        Doc { buf, sel: Sel::default(), history: History::default(), saved: 0 }
    }

    /// Changes since the file was read or saved. Undoing back to the saved text is no change.
    pub fn dirty(&self) -> bool {
        self.history.revision() != self.saved
    }

    pub fn revision(&self) -> u64 {
        self.history.revision()
    }

    /// A save starts: the bytes to write and the revision they are. Typing from here on starts
    /// a new step, so a save that ends after more typing does not count that typing as saved.
    pub fn begin_save(&mut self) -> (u64, Vec<u8>) {
        self.history.seal();
        (self.revision(), self.buf.to_bytes())
    }

    /// The text of `revision` (from `begin_save`) is on the disk now.
    pub fn mark_saved(&mut self, revision: u64) {
        self.saved = revision;
    }

    /// The file was read again (no unsaved change, or the person chose Reload): the text is the
    /// disk's, the history starts over, and the cursor stays where it can.
    pub fn reload(&mut self, buf: Buffer) {
        self.buf = buf;
        self.history = History { next: self.history.next, base: self.history.next + 1, ..History::default() };
        self.history.next += 1;
        self.saved = self.history.revision();
        self.sel = Sel { anchor: self.buf.clamp(self.sel.anchor), head: self.buf.clamp(self.sel.head) };
    }

    pub fn text(&self) -> String {
        String::from_utf8(self.buf.to_bytes()).unwrap_or_default()
    }

    pub fn selected(&self) -> Text {
        self.buf.extract(self.sel.range())
    }

    /// Replace `range` with `new`; the selection becomes `after` (else the cursor after `new`).
    pub fn edit(&mut self, range: Range<Pos>, new: Text, kind: Kind, after: Option<Sel>) {
        let range = self.buf.clamp(range.start)..self.buf.clamp(range.end);
        let removed = self.buf.extract(range.clone());
        if removed.is_empty() && new.is_empty() {
            return;
        }
        let before = self.sel;
        let end = self.buf.replace(range.clone(), &new);
        self.sel = after.unwrap_or(Sel::at(end));
        self.history.push(Edit { start: range.start, removed, inserted: new }, kind, before, self.sel);
    }

    pub fn undo(&mut self) -> bool {
        let Some(mut step) = self.history.done.pop_back() else { return false };
        for edit in step.edits.iter().rev() {
            self.buf.replace(edit.start..end_of(edit.start, &edit.inserted), &edit.removed);
        }
        self.sel = step.before;
        step.sealed = true;
        self.history.seal();
        self.history.undone.push(step);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(mut step) = self.history.undone.pop() else { return false };
        for edit in &step.edits {
            self.buf.replace(edit.start..end_of(edit.start, &edit.removed), &edit.inserted);
        }
        self.sel = step.after;
        step.sealed = true;
        self.history.seal();
        self.history.done.push_back(step);
        true
    }

    /// Text typed (or the input method's): it replaces the selection.
    pub fn type_text(&mut self, s: &str) {
        let line = self.sel.range().start.line;
        let text = Text::typed(s, self.buf.break_in(line));
        let kind = if text.lines.len() == 1 { Kind::Typing } else { Kind::Other };
        self.edit(self.sel.range(), text, kind, None);
    }

    pub fn paste(&mut self, s: &str) {
        let line = self.sel.range().start.line;
        self.edit(self.sel.range(), Text::typed(s, self.buf.break_in(line)), Kind::Other, None);
    }

    /// Cut: the selected text, removed.
    pub fn cut(&mut self) -> Option<String> {
        if self.sel.is_empty() {
            return None;
        }
        let text = self.selected().to_string();
        self.edit(self.sel.range(), Text::empty(), Kind::Other, None);
        Some(text)
    }

    // ----- moving

    pub fn move_to(&mut self, p: Pos, select: bool) {
        let p = self.buf.clamp(p);
        if select { self.sel.head = p } else { self.sel = Sel::at(p) }
    }

    pub fn prev_place(&self, p: Pos) -> Pos {
        if p.col > 0 {
            Pos::new(p.line, prev_grapheme(self.buf.line(p.line), p.col))
        } else if p.line > 0 {
            Pos::new(p.line - 1, self.buf.line(p.line - 1).len())
        } else {
            p
        }
    }

    pub fn next_place(&self, p: Pos) -> Pos {
        let s = self.buf.line(p.line);
        if p.col < s.len() {
            Pos::new(p.line, next_grapheme(s, p.col))
        } else if p.line + 1 < self.buf.lines() {
            Pos::new(p.line + 1, 0)
        } else {
            p
        }
    }

    pub fn left(&mut self, select: bool) {
        let to = if !select && !self.sel.is_empty() { self.sel.range().start } else { self.prev_place(self.sel.head) };
        self.move_to(to, select);
    }

    pub fn right(&mut self, select: bool) {
        let to = if !select && !self.sel.is_empty() { self.sel.range().end } else { self.next_place(self.sel.head) };
        self.move_to(to, select);
    }

    /// The start of the word before the cursor; at a line's start, the end of the line before.
    pub fn word_before(&self, p: Pos) -> Pos {
        match word_start_before(self.buf.line(p.line), p.col) {
            Some(col) => Pos::new(p.line, col),
            None if p.col > 0 => Pos::new(p.line, 0),
            None => self.prev_place(p),
        }
    }

    /// The end of the word after the cursor; at a line's end, the start of the line after.
    pub fn word_after(&self, p: Pos) -> Pos {
        let s = self.buf.line(p.line);
        match word_end_after(s, p.col) {
            Some(col) => Pos::new(p.line, col),
            None if p.col < s.len() => Pos::new(p.line, s.len()),
            None => self.next_place(p),
        }
    }

    pub fn word_left(&mut self, select: bool) {
        self.move_to(self.word_before(self.sel.head), select);
    }

    pub fn word_right(&mut self, select: bool) {
        self.move_to(self.word_after(self.sel.head), select);
    }

    /// Home: the first letter after the indent, then the line's start.
    pub fn home(&mut self, select: bool) {
        let p = self.sel.head;
        let indent = indent_of(self.buf.line(p.line));
        self.move_to(Pos::new(p.line, if p.col == indent { 0 } else { indent }), select);
    }

    pub fn end(&mut self, select: bool) {
        let line = self.sel.head.line;
        self.move_to(Pos::new(line, self.buf.line(line).len()), select);
    }

    pub fn select_all(&mut self) {
        self.sel = Sel { anchor: Pos::default(), head: self.buf.end() };
    }

    /// What a double click selects: the word (or the run of spaces, or the sign) at `p`.
    pub fn word_range(&self, p: Pos) -> Range<Pos> {
        let r = segment_at(self.buf.line(p.line), p.col);
        Pos::new(p.line, r.start)..Pos::new(p.line, r.end)
    }

    /// What a triple click selects: the line with its end.
    pub fn line_range(&self, line: usize) -> Range<Pos> {
        let end = if line + 1 < self.buf.lines() { Pos::new(line + 1, 0) } else { Pos::new(line, self.buf.line(line).len()) };
        Pos::new(line, 0)..end
    }

    // ----- editing keys

    pub fn backspace(&mut self) {
        if !self.sel.is_empty() {
            return self.edit(self.sel.range(), Text::empty(), Kind::Backspace, None);
        }
        let p = self.sel.head;
        let s = self.buf.line(p.line);
        // In the indent, back to the last stop: Tab's four spaces go in one key.
        let from = if p.col > 0 && s[..p.col].bytes().all(|b| b == b' ') {
            Pos::new(p.line, (p.col - 1) / TAB * TAB)
        } else {
            self.prev_place(p)
        };
        self.edit(from..p, Text::empty(), Kind::Backspace, None);
    }

    pub fn delete(&mut self) {
        let range = if self.sel.is_empty() { self.sel.head..self.next_place(self.sel.head) } else { self.sel.range() };
        self.edit(range, Text::empty(), Kind::Delete, None);
    }

    pub fn delete_word_left(&mut self) {
        let range = if self.sel.is_empty() { self.word_before(self.sel.head)..self.sel.head } else { self.sel.range() };
        self.edit(range, Text::empty(), Kind::Other, None);
    }

    pub fn delete_word_right(&mut self) {
        let range = if self.sel.is_empty() { self.sel.head..self.word_after(self.sel.head) } else { self.sel.range() };
        self.edit(range, Text::empty(), Kind::Other, None);
    }

    /// Enter: a line break with the line's own end, and the new line keeps the indent.
    pub fn newline(&mut self) {
        let range = self.sel.range();
        let s = self.buf.line(range.start.line);
        let indent = s[..indent_of(s).min(range.start.col)].to_string();
        let text = Text { lines: vec![String::new(), indent], eols: vec![self.buf.break_in(range.start.line), Eol::None] };
        self.edit(range, text, Kind::Other, None);
    }

    /// Tab: four spaces; over several lines, each line moves in by four.
    pub fn tab(&mut self) {
        let range = self.sel.range();
        if range.start.line == range.end.line {
            return self.edit(range, Text::line(&" ".repeat(TAB)), Kind::Typing, None);
        }
        self.reindent(|s| (" ".repeat(TAB) + s, TAB as isize));
    }

    /// Shift+Tab: the cursor's line (or each selected line) moves out by up to four spaces, or
    /// one tab character.
    pub fn outdent(&mut self) {
        self.reindent(|s| {
            let cut = if s.starts_with('\t') { 1 } else { s.bytes().take(TAB).take_while(|b| *b == b' ').count() };
            (s[cut..].to_string(), -(cut as isize))
        });
    }

    /// Change the start of each line of the selection; `change` gives the new line and how far
    /// its start moved. One undo step.
    fn reindent(&mut self, change: impl Fn(&str) -> (String, isize)) {
        let range = self.sel.range();
        // A selection that ends at a line's start does not take that line.
        let last = if range.end.col == 0 && range.end.line > range.start.line { range.end.line - 1 } else { range.end.line };
        let first = range.start.line;
        let mut lines = Vec::with_capacity(last - first + 1);
        let mut moved = Vec::with_capacity(last - first + 1);
        for i in first..=last {
            let (line, by) = change(self.buf.line(i));
            lines.push(line);
            moved.push(by);
        }
        if moved.iter().all(|m| *m == 0) {
            return;
        }
        // A place at a line's start stays there when the line moves in, so whole lines stay
        // selected; a line that is not changed keeps its places.
        let shift = |p: Pos| match p.line.checked_sub(first).and_then(|i| moved.get(i)) {
            Some(&by) if !(p.col == 0 && by > 0) => Pos::new(p.line, (p.col as isize + by).max(0) as usize),
            _ => p,
        };
        let after = Sel { anchor: shift(self.sel.anchor), head: shift(self.sel.head) };
        let eols = (first..=last).map(|i| if i == last { Eol::None } else { self.buf.eol(i) }).collect();
        let whole = Pos::new(first, 0)..Pos::new(last, self.buf.line(last).len());
        self.edit(whole, Text { lines, eols }, Kind::Other, Some(after));
    }

    // ----- the input method

    /// The input method's offsets count UTF-16 units from the start of this line (where the
    /// selection or the composition starts), so that no key walks the whole file.
    pub fn ime_base(&self, marked: Option<&Range<Pos>>) -> usize {
        let start = self.sel.range().start.line;
        marked.map_or(start, |m| m.start.line.min(start))
    }

    pub fn to_utf16(&self, base: usize, p: Pos) -> usize {
        if p.line < base {
            return 0;
        }
        let before: usize = (base..p.line).map(|l| utf16_len(self.buf.line(l)) + 1).sum();
        before + utf16_len(&self.buf.line(p.line)[..p.col])
    }

    pub fn from_utf16(&self, base: usize, mut offset: usize) -> Pos {
        let mut line = base.min(self.buf.lines() - 1);
        loop {
            let s = self.buf.line(line);
            let len = utf16_len(s);
            if offset <= len || line + 1 == self.buf.lines() {
                return Pos::new(line, col_of_utf16(s, offset.min(len)));
            }
            offset -= len + 1;
            line += 1;
        }
    }

    /// The text between two places, with "\n" between lines, for the input method.
    pub fn plain(&self, range: Range<Pos>) -> String {
        let text = self.buf.extract(range);
        text.lines.join("\n")
    }
}

// ----- the file ----------------------------------------------------------------------------------

/// What the disk has: its stamp and a hash of the bytes (None: no file there). The buffer keeps
/// the one it was read from or saved as; a change of the stamp alone (a touch, a checkout of the
/// same text) is no change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Base {
    pub stamp: Stamp,
    pub hash: Option<u64>,
}

impl Base {
    pub const MISSING: Base = Base { stamp: None, hash: None };
}

pub fn hash(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// A file's bytes as an editable buffer, or the sentence that says why it stays read only.
pub fn decode(bytes: &[u8]) -> Result<Buffer, String> {
    if bytes.len() as u64 > MAX_EDIT_BYTES {
        return Err(format!("This file is {}; Glasscode edits files up to {}, so it stays read only.", size_words(bytes.len() as u64), size_words(MAX_EDIT_BYTES)));
    }
    if bytes.iter().take(BINARY_PROBE).any(|b| *b == 0) {
        return Err("This is a binary file; it stays read only.".to_string());
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err("This file is in UTF-16; Glasscode edits UTF-8 files only, so it stays read only.".to_string());
    }
    let (bom, body) = match bytes.strip_prefix(BOM) {
        Some(body) => (true, body),
        None => (false, bytes),
    };
    let Ok(text) = std::str::from_utf8(body) else {
        return Err("This file is not in UTF-8; Glasscode edits UTF-8 files only, so it stays read only (saving could change its other characters).".to_string());
    };
    if text.split('\n').any(|line| line.len() > MAX_EDIT_LINE) {
        return Err(format!("A line of this file is longer than {MAX_EDIT_LINE} characters; it stays read only."));
    }
    Ok(Buffer::new(text, bom))
}

/// Read a file to edit it: at most `MAX_EDIT_BYTES` and one more byte, in the background.
pub fn read_for_edit(full: &Path) -> Result<(Buffer, Base), String> {
    let link = std::fs::symlink_metadata(full).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    if link {
        return Err("This file is a link to another file; Glasscode does not edit through links, so it stays read only.".to_string());
    }
    let stamp = stamp(full);
    let file = std::fs::File::open(full).map_err(|e| format!("Glasscode could not read this file: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_EDIT_BYTES + 1).read_to_end(&mut bytes).map_err(|e| format!("Glasscode could not read this file: {e}"))?;
    let size = stamp.map_or(bytes.len() as u64, |(len, _)| len);
    if size > MAX_EDIT_BYTES {
        return Err(format!("This file is {}; Glasscode edits files up to {}, so it stays read only.", size_words(size), size_words(MAX_EDIT_BYTES)));
    }
    let base = Base { stamp, hash: Some(hash(&bytes)) };
    decode(&bytes).map(|buf| (buf, base))
}

/// What the disk has now, read again after its stamp changed: the base, and the text when it can
/// be edited (None: the file is gone).
pub struct Seen {
    pub base: Base,
    pub text: Option<Result<Buffer, String>>,
}

pub fn read_seen(full: &Path) -> Seen {
    match std::fs::symlink_metadata(full) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Seen { base: Base::MISSING, text: None },
        _ => {}
    }
    match read_for_edit(full) {
        Ok((buf, base)) => Seen { base, text: Some(Ok(buf)) },
        Err(why) => {
            // Not editable now: its hash still tells whether it changed.
            let stamp = stamp(full);
            let hash = std::fs::File::open(full).ok().and_then(|f| {
                let mut bytes = Vec::new();
                f.take(MAX_EDIT_BYTES + 1).read_to_end(&mut bytes).ok().map(|_| hash(&bytes))
            });
            Seen { base: Base { stamp, hash }, text: Some(Err(why)) }
        }
    }
}

/// What a change of the file on the disk does to an open editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OnDisk {
    /// The same bytes (only the stamp changed), or the version the person chose to overwrite.
    Same,
    /// No unsaved change: read it again, silently.
    Reload,
    /// Unsaved changes: nothing is replaced and nothing is written until the person chooses
    /// (Reload or Keep mine).
    Ask,
    /// No unsaved change, and the file is gone: the editor closes, the tab says so.
    Gone,
    /// No unsaved change, and the file cannot be edited now (binary, too large, not UTF-8).
    ReadOnly(String),
}

/// `was`: the hash the text is based on; `kept`: the one the person chose to overwrite ("Keep
/// mine"); `now`: the disk's (None: gone), and whether its text can be edited.
pub fn on_disk(was: Option<u64>, kept: Option<Option<u64>>, now: Option<u64>, editable: Result<(), String>, dirty: bool) -> OnDisk {
    if now == was || kept == Some(now) {
        OnDisk::Same
    } else if dirty {
        OnDisk::Ask
    } else if now.is_none() {
        OnDisk::Gone
    } else if let Err(why) = editable {
        OnDisk::ReadOnly(why)
    } else {
        OnDisk::Reload
    }
}

/// Why a save wrote nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum SaveError {
    /// The disk has other bytes than the text is based on (an agent wrote the file): the person
    /// decides. What the disk has.
    Changed(Base),
    Failed(String),
}

static SAVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Save `bytes` as `full`, a file of `worktree`, in two steps: `prepare_save` writes a temporary
/// file in the same folder, `finish_save` renames it over the file. The file is never seen half
/// written, and a failure leaves it as it was. Nothing is written when the disk does not have
/// `expect` (the text's base, or the version the person chose to overwrite).
pub fn save_file(full: &Path, worktree: &Path, bytes: &[u8], expect: &Base) -> Result<Base, SaveError> {
    let temp = prepare_save(full, worktree, bytes, expect)?;
    finish_save(&temp, full, bytes)
}

fn failed(why: impl Into<String>) -> SaveError {
    SaveError::Failed(why.into())
}

pub fn prepare_save(full: &Path, worktree: &Path, bytes: &[u8], expect: &Base) -> Result<PathBuf, SaveError> {
    const KEPT: &str = "Your text is kept here.";
    let (Some(folder), Some(name)) = (full.parent(), full.file_name()) else { return Err(failed("This is not a file Glasscode can save.")) };
    // The tab's own path, inside its own worktree: never a path from typed text, never out of it.
    let inside = match (folder.canonicalize(), worktree.canonicalize()) {
        (Ok(folder), Ok(worktree)) => folder.starts_with(worktree),
        _ => return Err(failed(format!("The folder of this file is gone, so Glasscode did not save it. {KEPT}"))),
    };
    if !inside {
        return Err(failed("This file is not inside its worktree, so Glasscode did not save it."));
    }
    let meta = match std::fs::symlink_metadata(full) {
        Ok(meta) => Some(meta),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(failed(format!("Glasscode could not save this file ({error}). {KEPT}"))),
    };
    if meta.as_ref().is_some_and(|m| m.file_type().is_symlink()) {
        return Err(failed("This file is a link; Glasscode does not save through links."));
    }
    // A rename replaces a read-only file on macOS and Linux: ask the file itself first.
    if meta.as_ref().is_some_and(|m| m.permissions().readonly()) {
        return Err(failed(format!("This file is read only, so Glasscode did not save it. {KEPT}")));
    }
    let now = Base { stamp: stamp(full), hash: None };
    if now.stamp != expect.stamp {
        let seen = match now.stamp {
            None => Base::MISSING,
            Some(_) => {
                let mut disk = Vec::new();
                let read = std::fs::File::open(full).and_then(|f| f.take(MAX_EDIT_BYTES + 1).read_to_end(&mut disk));
                if let Err(error) = read {
                    return Err(failed(format!("Glasscode could not read the file before saving it ({error}). {KEPT}")));
                }
                Base { stamp: now.stamp, hash: Some(hash(&disk)) }
            }
        };
        if seen.hash != expect.hash {
            return Err(SaveError::Changed(seen));
        }
    }
    let number = SAVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = folder.join(format!(".{}.{}-{number}.glasscode-save", name.to_string_lossy(), std::process::id()));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        std::io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
        drop(file);
        // The file's permissions (its mode on macOS and Linux) go to the new file.
        if let Some(meta) = &meta {
            std::fs::set_permissions(&temp, meta.permissions())?;
        }
        Ok::<_, std::io::Error>(())
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(failed(format!("Glasscode could not save this file ({error}). {KEPT}")));
    }
    Ok(temp)
}

pub fn finish_save(temp: &Path, full: &Path, bytes: &[u8]) -> Result<Base, SaveError> {
    if let Err(error) = std::fs::rename(temp, full) {
        let _ = std::fs::remove_file(temp);
        return Err(failed(format!("Glasscode could not save this file ({error}). Your text is kept here.")));
    }
    Ok(Base { stamp: stamp(full), hash: Some(hash(bytes)) })
}

// ----- what is drawn ----------------------------------------------------------------------------

/// A line as it is drawn: a tab goes to the next stop of four, another control character shows
/// as �. With the place of each character in both, for the cursor, the selection, a click and
/// the input method (and the colour ranges of U21).
pub struct Shown {
    pub text: SharedString,
    /// (byte in the line, byte in `text`) at each character's start and at the end; None when
    /// they are the same.
    map: Option<Vec<(usize, usize)>>,
}

impl Shown {
    pub fn of(line: &str) -> Shown {
        if !line.chars().any(char::is_control) {
            return Shown { text: SharedString::from(line.to_string()), map: None };
        }
        let mut text = String::with_capacity(line.len() + 8);
        let mut map = Vec::with_capacity(line.len() + 1);
        let mut column = 0;
        for (i, c) in line.char_indices() {
            map.push((i, text.len()));
            match c {
                '\t' => {
                    let spaces = TAB - column % TAB;
                    text.extend(std::iter::repeat_n(' ', spaces));
                    column += spaces;
                }
                c if c.is_control() => {
                    text.push('\u{FFFD}');
                    column += 1;
                }
                c => {
                    text.push(c);
                    column += 1;
                }
            }
        }
        map.push((line.len(), text.len()));
        Shown { text: text.into(), map: Some(map) }
    }

    /// Where byte `col` of the line is drawn.
    pub fn shown(&self, col: usize) -> usize {
        match &self.map {
            None => col,
            Some(map) => map[map.partition_point(|(b, _)| *b < col).min(map.len() - 1)].1,
        }
    }

    /// The byte of the line drawn at `at` (the character it is in).
    pub fn byte(&self, at: usize) -> usize {
        match &self.map {
            None => at.min(self.text.len()),
            Some(map) => map[map.partition_point(|(_, d)| *d <= at).saturating_sub(1)].0,
        }
    }
}

/// A colour for a part of a line (bytes of the line). U21 gives these for syntax colours,
/// through `Editor::spans`.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub range: Range<usize>,
    pub color: Hsla,
}

/// The runs a line is shaped with: the text colour, the spans' colours over it, and the input
/// method's text underlined (`marked`, bytes of the line).
pub fn runs(shown: &Shown, font: &Font, color: Hsla, spans: &[Span], marked: Option<Range<usize>>) -> Vec<TextRun> {
    let len = shown.text.len();
    let mut cuts = vec![0, len];
    let spans: Vec<(Range<usize>, Hsla)> = spans.iter().map(|s| (shown.shown(s.range.start)..shown.shown(s.range.end), s.color)).collect();
    let marked = marked.map(|m| shown.shown(m.start)..shown.shown(m.end));
    for r in spans.iter().map(|(r, _)| r).chain(marked.iter()) {
        cuts.push(r.start.min(len));
        cuts.push(r.end.min(len));
    }
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .filter(|w| w[1] > w[0])
        .map(|w| {
            let color = spans.iter().rev().find(|(r, _)| r.start <= w[0] && w[1] <= r.end).map_or(color, |(_, c)| *c);
            let underline = marked.as_ref().filter(|m| m.start <= w[0] && w[1] <= m.end).map(|_| UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false });
            TextRun { len: w[1] - w[0], font: font.clone(), color, background_color: None, underline, strikethrough: None }
        })
        .collect()
}

// ----- the entity --------------------------------------------------------------------------------

actions!(
    editor,
    [
        Left, Right, Up, Down, WordLeft, WordRight, Home, End, DocStart, DocEnd, PageUp, PageDown, SelectLeft, SelectRight, SelectUp, SelectDown, SelectWordLeft,
        SelectWordRight, SelectHome, SelectEnd, SelectDocStart, SelectDocEnd, SelectPageUp, SelectPageDown, SelectAll, Backspace, Delete, DeleteWordLeft, DeleteWordRight,
        Newline, Tab, Outdent, Undo, Redo, Cut, Copy, Paste, Save
    ]
);

pub fn bind_keys(cx: &mut App) {
    crate::keymap::bind(cx, Some("Editor"));
}

pub enum EditorEvent {
    /// The text changed, was saved or was read again: the tab's dot may change.
    Changed,
    Saved(String),
    /// The file is gone from the disk and there was nothing unsaved.
    Gone,
    /// The file on the disk cannot be edited now, and there was nothing unsaved: why.
    ReadOnly(String),
}

/// What a press and a drag select: characters, words (a double click) or lines (a triple click,
/// or the line numbers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Char,
    Word,
    Line,
}

/// The last layout, for the mouse, the input method and moving between lines.
struct Laid {
    /// The text, right of the line numbers.
    text: Bounds<Pixels>,
    line_height: Pixels,
    scroll: Point<Pixels>,
    /// The lines on screen, from `first`.
    first: usize,
    rows: Vec<(Shown, ShapedLine)>,
}

/// Room above the first line and below the last.
const PAD: f32 = 6.;
/// Room kept between the cursor and the left or right edge when the text scrolls sideways.
const EDGE: f32 = 24.;

pub struct Editor {
    focus: FocusHandle,
    pub full: PathBuf,
    worktree: PathBuf,
    pub doc: Doc,
    /// The input method's text being composed (one line).
    marked: Option<Range<Pos>>,
    /// Up and Down keep the cursor at this x.
    goal_x: Option<Pixels>,
    /// How far the text is scrolled: down, and sideways.
    scroll: Point<Pixels>,
    /// Bring the cursor into view at the next layout.
    reveal: bool,
    laid: Option<Laid>,
    /// A press of the mouse that selects while it moves: by what, and what the press selected.
    drag: Option<(Unit, Range<Pos>)>,
    pointer: Point<Pixels>,
    /// Scrolls while a drag is past the edge of the text.
    autoscroll: Option<Task<()>>,
    /// What the disk had when the text was read or last saved.
    base: Base,
    /// The disk's version the person chose to overwrite ("Keep mine").
    kept: Option<Base>,
    /// The disk changed while the text has unsaved changes: what it has now.
    conflict: Option<Base>,
    /// The last save failed: why.
    error: Option<String>,
    saving: bool,
    checking: bool,
    dirty: bool,
}

impl EventEmitter<EditorEvent> for Editor {}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn font() -> Font {
    gpui::font(theme::MONO_FONT.family())
}

impl Editor {
    pub fn new(full: PathBuf, worktree: PathBuf, buf: Buffer, base: Base, cx: &mut Context<Self>) -> Self {
        Editor {
            focus: cx.focus_handle(),
            full,
            worktree,
            doc: Doc::new(buf),
            marked: None,
            goal_x: None,
            scroll: Point::default(),
            reveal: false,
            laid: None,
            drag: None,
            pointer: Point::default(),
            autoscroll: None,
            base,
            kept: None,
            conflict: None,
            error: None,
            saving: false,
            checking: false,
            dirty: false,
        }
    }

    pub fn dirty(&self) -> bool {
        self.doc.dirty()
    }

    pub fn saving(&self) -> bool {
        self.saving
    }

    fn name(&self) -> String {
        self.full.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The colours of a line's parts. None yet; U21's scanner gives them here.
    fn spans(&self, _line: usize) -> Vec<Span> {
        Vec::new()
    }

    /// After a change of the text or the selection: the cursor comes into view, and the tab
    /// hears when the dot changes.
    fn changed(&mut self, keep_goal: bool, cx: &mut Context<Self>) {
        if !keep_goal {
            self.goal_x = None;
        }
        self.reveal = true;
        let dirty = self.doc.dirty();
        if dirty != self.dirty {
            self.dirty = dirty;
            cx.emit(EditorEvent::Changed);
        }
        cx.notify();
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.marked = None;
        self.changed(false, cx);
    }

    // ----- places on screen

    fn line_height(&self) -> Pixels {
        theme::sz(crate::file_view::LINE_HEIGHT)
    }

    fn shape(&self, line: usize, window: &mut Window) -> (Shown, ShapedLine) {
        let shown = Shown::of(self.doc.buf.line(line));
        let runs = runs(&shown, &font(), theme::text(), &[], None);
        let shaped = window.text_system().shape_line(shown.text.clone(), theme::sz(crate::file_view::LINE_TEXT), &runs, None);
        (shown, shaped)
    }

    /// The x of a place in its line, from the line's start.
    fn x_of(&self, p: Pos, window: &mut Window) -> Pixels {
        if let Some((shown, shaped)) = self.laid.as_ref().and_then(|l| p.line.checked_sub(l.first).and_then(|i| l.rows.get(i))) {
            return shaped.x_for_index(shown.shown(p.col));
        }
        let (shown, shaped) = self.shape(p.line, window);
        shaped.x_for_index(shown.shown(p.col))
    }

    /// The place in `line` nearest to `x`, at a grapheme boundary.
    fn col_at(&self, line: usize, x: Pixels, window: &mut Window) -> usize {
        let at = |shown: &Shown, shaped: &ShapedLine| snap_grapheme(self.doc.buf.line(line), shown.byte(shaped.closest_index_for_x(x)));
        if let Some((shown, shaped)) = self.laid.as_ref().and_then(|l| line.checked_sub(l.first).and_then(|i| l.rows.get(i))) {
            return at(shown, shaped);
        }
        let (shown, shaped) = self.shape(line, window);
        at(&shown, &shaped)
    }

    /// The place under a point of the window (above the text: the first line; below: the last).
    fn pos_at(&self, position: Point<Pixels>, window: &mut Window) -> Option<Pos> {
        let laid = self.laid.as_ref()?;
        let y = position.y - laid.text.top() - theme::sz(PAD) + laid.scroll.y;
        let line = if y < px(0.) { 0 } else { ((y / laid.line_height) as usize).min(self.doc.buf.lines() - 1) };
        let x = (position.x - laid.text.left() + laid.scroll.x).max(px(0.));
        Some(Pos::new(line, self.col_at(line, x, window)))
    }

    /// The lines that fit on screen.
    fn page(&self) -> usize {
        self.laid.as_ref().map_or(20, |l| ((l.text.size.height / l.line_height) as usize).saturating_sub(1).max(1))
    }

    // ----- keys

    fn vertical(&mut self, rows: isize, select: bool, window: &mut Window, cx: &mut Context<Self>) {
        let head = self.doc.sel.head;
        let x = match self.goal_x {
            Some(x) => x,
            None => self.x_of(head, window),
        };
        let target = head.line as isize + rows;
        let p = if target < 0 {
            Pos::default()
        } else if target as usize >= self.doc.buf.lines() {
            self.doc.buf.end()
        } else {
            Pos::new(target as usize, self.col_at(target as usize, x, window))
        };
        if !select && !self.doc.sel.is_empty() && rows.abs() == 1 {
            // Up and Down leave a selection from its edge, as Left and Right do.
            let edge = if rows < 0 { self.doc.sel.range().start } else { self.doc.sel.range().end };
            self.doc.sel = Sel::at(edge);
            return self.vertical(rows, false, window, cx);
        }
        self.doc.move_to(p, select);
        self.goal_x = Some(x);
        self.changed(true, cx);
    }

    fn page_by(&mut self, down: bool, select: bool, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.page() as isize;
        let lh = self.line_height();
        self.scroll.y = (self.scroll.y + lh * if down { rows as f32 } else { -(rows as f32) }).max(px(0.));
        self.vertical(if down { rows } else { -rows }, select, window, cx);
    }

    fn moved(&mut self, cx: &mut Context<Self>) {
        self.marked = None;
        self.changed(false, cx);
    }

    fn save_key(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        self.save(cx).detach();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.doc.sel.is_empty() {
            // The text as the file has it, line ends included.
            cx.write_to_clipboard(ClipboardItem::new_string(self.doc.selected().to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.doc.cut() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.edited(cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.doc.paste(&text);
            self.edited(cx);
        }
    }

    // ----- the mouse

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let Some(p) = self.pos_at(event.position, window) else { return };
        let in_numbers = self.laid.as_ref().is_some_and(|l| event.position.x < l.text.left());
        let unit = match event.click_count {
            _ if in_numbers => Unit::Line,
            0 | 1 => Unit::Char,
            2 => Unit::Word,
            _ => Unit::Line,
        };
        let origin = match unit {
            Unit::Char => p..p,
            Unit::Word => self.doc.word_range(p),
            Unit::Line => self.doc.line_range(p.line),
        };
        if event.modifiers.shift && unit == Unit::Char {
            self.doc.sel.head = p;
            self.drag = Some((unit, self.doc.sel.anchor..self.doc.sel.anchor));
        } else {
            self.doc.sel = Sel { anchor: origin.start, head: origin.end };
            self.drag = Some((unit, origin));
        }
        self.pointer = event.position;
        self.moved(cx);
    }

    /// The selection while the mouse moves with the button down: from what the press selected to
    /// the unit under the pointer.
    fn drag_to(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((unit, origin)) = self.drag.clone() else { return };
        self.pointer = position;
        let Some(p) = self.pos_at(position, window) else { return };
        let under = match unit {
            Unit::Char => p..p,
            Unit::Word => self.doc.word_range(p),
            Unit::Line => self.doc.line_range(p.line),
        };
        self.doc.sel = if under.start < origin.start { Sel { anchor: origin.end, head: under.start } } else { Sel { anchor: origin.start, head: under.end.max(origin.end) } };
        if unit == Unit::Char {
            self.doc.sel = Sel { anchor: origin.start, head: p };
        }
        let outside = self.laid.as_ref().is_some_and(|l| !l.text.contains(&position));
        if outside && self.autoscroll.is_none() {
            self.autoscroll = Some(cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(40)).await;
                    let more = this.update_in(cx, |this, window, cx| this.autoscroll_step(window, cx)).unwrap_or(false);
                    if !more {
                        break;
                    }
                }
            }));
        }
        self.moved(cx);
    }

    /// One step of the scroll while a drag is past the edge: faster the farther the pointer is.
    fn autoscroll_step(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(text) = self.laid.as_ref().map(|l| l.text).filter(|_| self.drag.is_some()) else {
            self.autoscroll = None;
            return false;
        };
        let p = self.pointer;
        let past = |v: Pixels, low: Pixels, high: Pixels| if v < low { v - low } else if v > high { v - high } else { px(0.) };
        let (dx, dy) = (past(p.x, text.left(), text.right()), past(p.y, text.top(), text.bottom()));
        if dx == px(0.) && dy == px(0.) {
            self.autoscroll = None;
            return false;
        }
        let lh = self.line_height();
        self.scroll.y = (self.scroll.y + dy.clamp(-lh * 3., lh * 3.)).max(px(0.));
        self.scroll.x = (self.scroll.x + dx.clamp(-lh * 3., lh * 3.)).max(px(0.));
        self.drag_to(p, window, cx);
        // The view follows the scroll here, not the cursor.
        self.reveal = false;
        true
    }

    fn mouse_up(&mut self) {
        self.drag = None;
        self.autoscroll = None;
    }

    fn wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(self.line_height());
        let (dx, dy) = if event.modifiers.shift && delta.x == px(0.) { (delta.y, px(0.)) } else { (delta.x, delta.y) };
        self.scroll.y = (self.scroll.y - dy).max(px(0.));
        self.scroll.x = (self.scroll.x - dx).max(px(0.));
        cx.notify();
    }

    // ----- saving

    /// Save the text as it is now. The task gives the sentence of a failure; the text and its
    /// dot stay in every case but success.
    pub fn save(&mut self, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        if self.saving {
            return Task::ready(Err("Glasscode is still saving this file; try again in a moment.".to_string()));
        }
        let (revision, bytes) = self.doc.begin_save();
        let expect = self.kept.clone().unwrap_or_else(|| self.base.clone());
        let (full, worktree) = (self.full.clone(), self.worktree.clone());
        self.saving = true;
        cx.notify();
        let task = cx.background_spawn(async move { save_file(&full, &worktree, &bytes, &expect) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| this.saved(revision, result, cx)).unwrap_or_else(|_| Err("The editor closed before the save ended.".to_string()))
        })
    }

    fn saved(&mut self, revision: u64, result: Result<Base, SaveError>, cx: &mut Context<Self>) -> Result<(), String> {
        self.saving = false;
        let out = match result {
            Ok(base) => {
                self.base = base;
                self.kept = None;
                self.conflict = None;
                self.error = None;
                self.doc.mark_saved(revision);
                cx.emit(EditorEvent::Saved(self.name()));
                Ok(())
            }
            Err(SaveError::Changed(seen)) => {
                self.conflict = Some(seen);
                Err("The file changed on disk since Glasscode read it, so nothing was written.".to_string())
            }
            Err(SaveError::Failed(why)) => {
                self.error = Some(why.clone());
                Err(why)
            }
        };
        self.changed(true, cx);
        out
    }

    // ----- the disk

    /// U13's look at the disk (every 2 seconds, for the tab in front) saw `now`. Only a stamp
    /// that differs from every version known here reads the file again.
    pub fn disk_stamp(&mut self, now: Stamp, cx: &mut Context<Self>) {
        let known = |b: &Option<Base>| b.as_ref().is_some_and(|b| b.stamp == now);
        if self.saving || self.checking || now == self.base.stamp || known(&self.kept) || known(&self.conflict) {
            return;
        }
        self.checking = true;
        let full = self.full.clone();
        let task = cx.background_spawn(async move { read_seen(&full) });
        cx.spawn(async move |this, cx| {
            let seen = task.await;
            let _ = this.update(cx, |this, cx| {
                this.checking = false;
                this.disk_seen(seen, cx);
            });
        })
        .detach();
    }

    fn disk_seen(&mut self, seen: Seen, cx: &mut Context<Self>) {
        let editable = match &seen.text {
            Some(Err(why)) => Err(why.clone()),
            _ => Ok(()),
        };
        match on_disk(self.base.hash, self.kept.as_ref().map(|k| k.hash), seen.base.hash, editable, self.doc.dirty() || self.saving) {
            OnDisk::Same => {
                // A touch, or the version kept: its new stamp is known now.
                if seen.base.hash == self.base.hash {
                    self.base.stamp = seen.base.stamp;
                } else if let Some(kept) = self.kept.as_mut() {
                    kept.stamp = seen.base.stamp;
                }
            }
            OnDisk::Ask => {
                self.conflict = Some(seen.base);
                cx.notify();
            }
            OnDisk::Reload => self.take_disk(seen, cx),
            OnDisk::Gone => cx.emit(EditorEvent::Gone),
            OnDisk::ReadOnly(why) => cx.emit(EditorEvent::ReadOnly(why)),
        }
    }

    /// The disk's text replaces the buffer: no unsaved change, or the person chose Reload. The
    /// cursor and the scroll stay where they can.
    fn take_disk(&mut self, seen: Seen, cx: &mut Context<Self>) {
        match seen.text {
            None => cx.emit(EditorEvent::Gone),
            Some(Err(why)) => cx.emit(EditorEvent::ReadOnly(why)),
            Some(Ok(buf)) => {
                self.doc.reload(buf);
                self.base = seen.base;
                self.kept = None;
                self.conflict = None;
                self.error = None;
                self.marked = None;
                self.changed(false, cx);
                self.reveal = false;
            }
        }
    }

    /// "Reload" in the bar: the person drops the unsaved changes.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let full = self.full.clone();
        let task = cx.background_spawn(async move { read_seen(&full) });
        cx.spawn(async move |this, cx| {
            let seen = task.await;
            let _ = this.update(cx, |this, cx| this.take_disk(seen, cx));
        })
        .detach();
    }

    /// "Keep mine": the text stays, and the next save overwrites the disk's version.
    fn keep_mine(&mut self, cx: &mut Context<Self>) {
        self.kept = self.conflict.take();
        cx.notify();
    }

    // ----- drawing

    /// A line above the text: what changed on the disk, a save that failed.
    fn bars(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mut bars = Vec::new();
        let bar = |bg: Hsla, color: Hsla| {
            div().flex().flex_none().flex_wrap().items_center().gap_x(theme::sz(10.)).gap_y(theme::sz(6.)).px(theme::sz(14.)).py(theme::sz(8.)).bg(bg).text_color(color).text_size(theme::sz(12.5))
        };
        if let Some(seen) = &self.conflict {
            let what = if seen.hash.is_none() { "This file was deleted on disk while it has changes here that are not saved." } else { "This file changed on disk (an agent or another program wrote it) while it has changes here that are not saved." };
            bars.push(
                bar(theme::warn_bg(), theme::text())
                    .child(div().flex_1().min_w(theme::sz(200.)).child(format!("{what} Reload drops your changes and reads the file again. Keep mine keeps your text, and the next save overwrites the file on disk.")))
                    .child(ui::button("editor-reload", "Reload").on_click(cx.listener(|this, _, _, cx| this.reload(cx))))
                    .child(ui::button("editor-keep", "Keep mine").on_click(cx.listener(|this, _, _, cx| this.keep_mine(cx))))
                    .into_any_element(),
            );
        } else if self.kept.is_some() {
            bars.push(bar(theme::warn_bg(), theme::text_soft()).child("Keep mine: the next save overwrites the file on disk.").into_any_element());
        }
        if let Some(error) = &self.error {
            bars.push(bar(theme::error_bg(), theme::error_text()).child(error.clone()).into_any_element());
        }
        bars
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("Editor")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .on_action(cx.listener(|this, _: &Left, _, cx| {
                this.doc.left(false);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &Right, _, cx| {
                this.doc.right(false);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| {
                this.doc.left(true);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| {
                this.doc.right(true);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &WordLeft, _, cx| {
                this.doc.word_left(false);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &WordRight, _, cx| {
                this.doc.word_right(false);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &SelectWordLeft, _, cx| {
                this.doc.word_left(true);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &SelectWordRight, _, cx| {
                this.doc.word_right(true);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &Home, _, cx| {
                this.doc.home(false);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &End, _, cx| {
                this.doc.end(false);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &SelectHome, _, cx| {
                this.doc.home(true);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &SelectEnd, _, cx| {
                this.doc.end(true);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &DocStart, _, cx| {
                this.doc.move_to(Pos::default(), false);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &DocEnd, _, cx| {
                this.doc.move_to(this.doc.buf.end(), false);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &SelectDocStart, _, cx| {
                this.doc.move_to(Pos::default(), true);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &SelectDocEnd, _, cx| {
                this.doc.move_to(this.doc.buf.end(), true);
                this.moved(cx)
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.doc.select_all();
                this.marked = None;
                this.changed(false, cx);
                // Select all keeps the view where it is.
                this.reveal = false;
            }))
            .on_action(cx.listener(|this, _: &Up, window, cx| this.vertical(-1, false, window, cx)))
            .on_action(cx.listener(|this, _: &Down, window, cx| this.vertical(1, false, window, cx)))
            .on_action(cx.listener(|this, _: &SelectUp, window, cx| this.vertical(-1, true, window, cx)))
            .on_action(cx.listener(|this, _: &SelectDown, window, cx| this.vertical(1, true, window, cx)))
            .on_action(cx.listener(|this, _: &PageUp, window, cx| this.page_by(false, false, window, cx)))
            .on_action(cx.listener(|this, _: &PageDown, window, cx| this.page_by(true, false, window, cx)))
            .on_action(cx.listener(|this, _: &SelectPageUp, window, cx| this.page_by(false, true, window, cx)))
            .on_action(cx.listener(|this, _: &SelectPageDown, window, cx| this.page_by(true, true, window, cx)))
            .on_action(cx.listener(|this, _: &Backspace, _, cx| {
                this.doc.backspace();
                this.edited(cx)
            }))
            .on_action(cx.listener(|this, _: &Delete, _, cx| {
                this.doc.delete();
                this.edited(cx)
            }))
            .on_action(cx.listener(|this, _: &DeleteWordLeft, _, cx| {
                this.doc.delete_word_left();
                this.edited(cx)
            }))
            .on_action(cx.listener(|this, _: &DeleteWordRight, _, cx| {
                this.doc.delete_word_right();
                this.edited(cx)
            }))
            .on_action(cx.listener(|this, _: &Newline, _, cx| {
                this.doc.newline();
                this.edited(cx)
            }))
            .on_action(cx.listener(|this, _: &Tab, _, cx| {
                this.doc.tab();
                this.edited(cx)
            }))
            .on_action(cx.listener(|this, _: &Outdent, _, cx| {
                this.doc.outdent();
                this.edited(cx)
            }))
            .on_action(cx.listener(|this, _: &Undo, _, cx| {
                this.doc.undo();
                this.edited(cx)
            }))
            .on_action(cx.listener(|this, _: &Redo, _, cx| {
                this.doc.redo();
                this.edited(cx)
            }))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::save_key))
            .children(self.bars(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .cursor(CursorStyle::IBeam)
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_scroll_wheel(cx.listener(Self::wheel))
                    .child(EditorElement { editor: cx.entity() }),
            )
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(&mut self, range: Range<usize>, actual: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let base = self.doc.ime_base(self.marked.as_ref());
        let r = self.doc.from_utf16(base, range.start)..self.doc.from_utf16(base, range.end);
        actual.replace(self.doc.to_utf16(base, r.start)..self.doc.to_utf16(base, r.end));
        Some(self.doc.plain(r))
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let base = self.doc.ime_base(self.marked.as_ref());
        let r = self.doc.sel.range();
        Some(UTF16Selection { range: self.doc.to_utf16(base, r.start)..self.doc.to_utf16(base, r.end), reversed: self.doc.sel.head < self.doc.sel.anchor })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let base = self.doc.ime_base(self.marked.as_ref());
        self.marked.as_ref().map(|m| self.doc.to_utf16(base, m.start)..self.doc.to_utf16(base, m.end))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(&mut self, range: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        // Only text: a key with Ctrl that nothing binds gives no control character.
        let text: String = text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t')).collect();
        let base = self.doc.ime_base(self.marked.as_ref());
        let range = range.map(|r| self.doc.from_utf16(base, r.start)..self.doc.from_utf16(base, r.end)).or(self.marked.clone()).unwrap_or(self.doc.sel.range());
        if text.is_empty() && range.start == range.end {
            self.marked = None;
            return cx.notify();
        }
        let typed = Text::typed(&text, self.doc.buf.break_in(range.start.line));
        let kind = if typed.lines.len() == 1 { Kind::Typing } else { Kind::Other };
        self.doc.edit(range, typed, kind, None);
        self.edited(cx);
    }

    fn replace_and_mark_text_in_range(&mut self, range: Option<Range<usize>>, text: &str, selected: Option<Range<usize>>, window: &mut Window, cx: &mut Context<Self>) {
        if text.contains(['\n', '\r']) {
            return self.replace_text_in_range(range, text, window, cx);
        }
        let base = self.doc.ime_base(self.marked.as_ref());
        let range = range.map(|r| self.doc.from_utf16(base, r.start)..self.doc.from_utf16(base, r.end)).or(self.marked.clone()).unwrap_or(self.doc.sel.range());
        let start = range.start;
        self.doc.edit(range, Text::line(text), Kind::Typing, None);
        self.marked = (!text.is_empty()).then(|| start..Pos::new(start.line, start.col + text.len()));
        if let Some(selected) = selected {
            let at = |units: usize| Pos::new(start.line, start.col + col_of_utf16(text, units));
            self.doc.sel = Sel { anchor: at(selected.start), head: at(selected.end) };
        }
        self.changed(false, cx);
    }

    fn bounds_for_range(&mut self, range: Range<usize>, _: Bounds<Pixels>, window: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let base = self.doc.ime_base(self.marked.as_ref());
        let (a, b) = (self.doc.from_utf16(base, range.start), self.doc.from_utf16(base, range.end));
        let (x0, x1) = (self.x_of(a, window), if b.line == a.line { self.x_of(b, window) } else { self.x_of(a, window) });
        let laid = self.laid.as_ref()?;
        let origin = point(laid.text.left() - laid.scroll.x + x0, laid.text.top() + theme::sz(PAD) + laid.line_height * a.line as f32 - laid.scroll.y);
        Some(Bounds::new(origin, size((x1 - x0).max(theme::sz(2.)), laid.line_height)))
    }

    fn character_index_for_point(&mut self, at: Point<Pixels>, window: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let p = self.pos_at(at, window)?;
        let base = self.doc.ime_base(self.marked.as_ref());
        Some(self.doc.to_utf16(base, p))
    }
}

// ----- the element -------------------------------------------------------------------------------

/// The text with its line numbers. Only the lines on screen are shaped (as `uniform_list` does
/// for the read-only view), and one element draws them, so the cursor, the selection, a click
/// and the input method share one set of places.
struct EditorElement {
    editor: Entity<Editor>,
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct Frame {
    /// The current line or the selection, under the text.
    under: Vec<PaintQuad>,
    rows: Vec<(Point<Pixels>, ShapedLine)>,
    numbers: Vec<(Point<Pixels>, ShapedLine)>,
    caret: Option<PaintQuad>,
    text: Bounds<Pixels>,
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = Frame;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> Frame {
        let editor = self.editor.read(cx);
        let lines = editor.doc.buf.lines();
        let lh = editor.line_height();
        let pad = theme::sz(PAD);
        let font = font();
        let font_size = theme::sz(crate::file_view::LINE_TEXT);
        let gutter = crate::file_view::gutter_width(lines);
        let text = Bounds::from_corners(point(bounds.left() + gutter, bounds.top()), bounds.bottom_right());
        let height = bounds.size.height;
        let head = editor.doc.sel.head;
        let mut scroll = editor.scroll;
        // Down: the cursor's line into view, then no further than the last line.
        if editor.reveal {
            let top = pad + lh * head.line as f32;
            if top - pad < scroll.y {
                scroll.y = top - pad;
            } else if top + lh + pad > scroll.y + height {
                scroll.y = top + lh + pad - height;
            }
        }
        scroll.y = scroll.y.min((lh * lines as f32 + pad * 2. - height).max(px(0.))).max(px(0.));
        let first = (((scroll.y - pad) / lh).floor().max(0.)) as usize;
        let last = ((((scroll.y - pad + height) / lh).ceil().max(0.)) as usize + 1).min(lines);
        let marked = editor.marked.clone();
        let mut rows = Vec::with_capacity(last.saturating_sub(first));
        for line in first..last {
            let shown = Shown::of(editor.doc.buf.line(line));
            let marked = marked.as_ref().filter(|m| m.start.line == line).map(|m| m.start.col..m.end.col);
            let runs = runs(&shown, &font, theme::text(), &editor.spans(line), marked);
            let shaped = window.text_system().shape_line(shown.text.clone(), font_size, &runs, None);
            rows.push((shown, shaped));
        }
        let x_in = |line: usize, col: usize| rows.get(line.wrapping_sub(first)).map(|(shown, shaped): &(Shown, ShapedLine)| shaped.x_for_index(shown.shown(col)));
        // Sideways: the cursor into view, and no further than the widest line on screen.
        let caret_x = x_in(head.line, head.col);
        let edge = theme::sz(EDGE);
        if let Some(x) = caret_x.filter(|_| editor.reveal) {
            if x - edge < scroll.x {
                scroll.x = x - edge;
            } else if x + edge > scroll.x + text.size.width {
                scroll.x = x + edge - text.size.width;
            }
        }
        let widest = rows.iter().map(|(_, s)| s.width()).fold(caret_x.unwrap_or_default(), Pixels::max);
        scroll.x = scroll.x.min((widest + edge - text.size.width).max(px(0.))).max(px(0.));

        let y_of = |line: usize| text.top() + pad + lh * line as f32 - scroll.y;
        let x0 = text.left() - scroll.x;
        let mut under = Vec::new();
        let sel = editor.doc.sel;
        if sel.is_empty() {
            under.push(fill(Bounds::new(point(text.left(), y_of(head.line)), size(text.size.width, lh)), theme::hover()));
        } else {
            let range = sel.range();
            // The width of a space: a selected line end shows as one.
            let end_width = theme::sz(crate::file_view::DIGIT);
            for line in range.start.line.max(first)..=range.end.line.min(last.saturating_sub(1)) {
                let from = if line == range.start.line { x_in(line, range.start.col).unwrap_or_default() } else { px(0.) };
                let to = if line == range.end.line { x_in(line, range.end.col).unwrap_or_default() } else { x_in(line, editor.doc.buf.line(line).len()).unwrap_or_default() + end_width };
                if to > from {
                    under.push(fill(Bounds::from_corners(point(x0 + from, y_of(line)), point(x0 + to, y_of(line) + lh)), theme::c(theme::term().selection)));
                }
            }
        }
        let caret = caret_x.map(|x| fill(Bounds::new(point(x0 + x, y_of(head.line)), size(theme::sz(1.5), lh)), theme::accent()));
        let numbers = (first..last)
            .map(|line| {
                let label = SharedString::from((line + 1).to_string());
                let color = if line == head.line { theme::text_soft() } else { theme::faint() };
                let run = TextRun { len: label.len(), font: font.clone(), color, background_color: None, underline: None, strikethrough: None };
                let shaped = window.text_system().shape_line(label, font_size, &[run], None);
                (point(bounds.left() + gutter - theme::sz(12.) - shaped.width(), y_of(line)), shaped)
            })
            .collect();
        let rows_painted = rows.iter().enumerate().map(|(i, (_, shaped))| (point(x0, y_of(first + i)), shaped.clone())).collect();
        self.editor.update(cx, |editor, _| {
            editor.scroll = scroll;
            editor.reveal = false;
            editor.laid = Some(Laid { text, line_height: lh, scroll, first, rows });
        });
        Frame { under, rows: rows_painted, numbers, caret, text }
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), frame: &mut Frame, window: &mut Window, cx: &mut App) {
        let focus = self.editor.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.editor.clone()), cx);
        // A drag goes on past the edge of the text and of the window.
        let editor = self.editor.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && event.pressed_button == Some(MouseButton::Left) {
                editor.update(cx, |editor, cx| editor.drag_to(event.position, window, cx));
            }
        });
        let editor = self.editor.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                editor.update(cx, |editor, _| editor.mouse_up());
            }
        });
        let lh = self.editor.read(cx).line_height();
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for (origin, line) in &frame.numbers {
                let _ = line.paint(*origin, lh, gpui::TextAlign::Left, None, window, cx);
            }
            window.with_content_mask(Some(gpui::ContentMask { bounds: frame.text }), |window| {
                for quad in frame.under.drain(..) {
                    window.paint_quad(quad);
                }
                for (origin, line) in &frame.rows {
                    let _ = line.paint(*origin, lh, gpui::TextAlign::Left, None, window, cx);
                }
                // The caret does not blink: a blink would draw the window again on each step.
                if focus.is_focused(window) {
                    if let Some(caret) = frame.caret.take() {
                        window.paint_quad(caret);
                    }
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Doc {
        Doc::new(Buffer::new(text, false))
    }

    fn at(line: usize, col: usize) -> Pos {
        Pos::new(line, col)
    }

    /// Every way a file can end and break its lines comes back byte for byte.
    #[test]
    fn a_file_is_written_back_as_it_was_read() {
        let samples: [&[u8]; 12] = [
            b"",
            b"\n",
            b"\r\n",
            b"\xEF\xBB\xBF",
            b"\xEF\xBB\xBFfn main() {}\r\n",
            b"one\ntwo\nthree\n",
            b"one\r\ntwo\r\nthree\r\n",
            b"one\r\ntwo\nthree\r\nfour",
            b"no final end",
            b"a lone \r in a line\nand \r\r\n",
            b"\ttabs\tand \x07 bells\n",
            "emoji 👋🏽 🇩🇪 e\u{301}\n".as_bytes(),
        ];
        for bytes in samples {
            let buf = decode(bytes).expect("editable");
            assert_eq!(buf.to_bytes(), bytes, "{:?}", String::from_utf8_lossy(bytes));
        }
    }

    #[test]
    fn crlf_stays_crlf_and_mixed_ends_stay_mixed() {
        let mut d = doc("a\r\nb\r\n");
        d.move_to(at(0, 1), false);
        d.newline();
        d.type_text("x");
        assert_eq!(d.text(), "a\r\nx\r\nb\r\n");
        // A mixed file: a new line takes the end of the line it breaks.
        let mut d = doc("crlf\r\ncrlf\r\nlf\nlast");
        d.move_to(at(2, 2), false);
        d.newline();
        d.move_to(at(0, 4), false);
        d.newline();
        assert_eq!(d.text(), "crlf\r\n\r\ncrlf\r\nlf\n\nlast");
        // The last line has no end of its own: the file's most common end (CRLF, two of three).
        d.move_to(d.buf.end(), false);
        d.newline();
        assert_eq!(d.text(), "crlf\r\n\r\ncrlf\r\nlf\n\nlast\r\n");
        // A paste with CRLF in an LF file becomes LF lines.
        let mut d = doc("x\ny\n");
        d.move_to(at(1, 0), false);
        d.paste("p\r\nq\rr\n");
        assert_eq!(d.text(), "x\np\nq\nr\ny\n");
    }

    #[test]
    fn no_final_line_end_stays_so() {
        let mut d = doc("one\ntwo");
        d.move_to(at(1, 3), false);
        d.type_text("!");
        assert_eq!(d.text(), "one\ntwo!");
        d.move_to(at(0, 0), false);
        d.delete();
        assert_eq!(d.text(), "ne\ntwo!");
    }

    #[test]
    fn a_byte_order_mark_stays() {
        let mut d = Doc::new(decode(b"\xEF\xBB\xBFhi\n").unwrap());
        assert_eq!(d.buf.line(0), "hi", "the mark is not text");
        d.move_to(at(0, 0), false);
        d.type_text("oh ");
        assert_eq!(d.buf.to_bytes(), b"\xEF\xBB\xBFoh hi\n");
    }

    #[test]
    fn what_cannot_be_edited_says_why() {
        assert!(decode(&[b'a', 0, b'b']).unwrap_err().contains("binary"));
        assert!(decode(&[0x66, 0x6f, 0xe9, 0x0a]).unwrap_err().contains("not in UTF-8"), "Latin-1");
        assert!(decode(&[0xFF, 0xFE, b'a', 0]).unwrap_err().contains("binary") || decode(&[0xFF, 0xFE, b'a', 0]).unwrap_err().contains("UTF-16"));
        assert!(decode(&[0xFF, 0xFE, b'a', b'b']).unwrap_err().contains("UTF-16"));
        let big = vec![b'x'; MAX_EDIT_BYTES as usize + 1];
        assert!(decode(&big).unwrap_err().contains("up to 2.0 MB"));
        let long = "y".repeat(MAX_EDIT_LINE + 1);
        assert!(decode(long.as_bytes()).unwrap_err().contains("longer than"));
        // Just under the limits: editable.
        let mut under = "z".repeat(MAX_EDIT_LINE);
        under.push('\n');
        assert!(decode(under.as_bytes()).is_ok());
    }

    #[test]
    fn insert_and_delete_at_line_starts_and_ends() {
        let mut d = doc("ab\ncd\nef");
        // The start of the first line.
        d.move_to(at(0, 0), false);
        d.type_text("^");
        d.backspace();
        d.backspace();
        assert_eq!(d.text(), "ab\ncd\nef", "backspace at the very start does nothing");
        // The end of a line: delete joins the next line; backspace at a start joins the last.
        d.move_to(at(0, 2), false);
        d.delete();
        assert_eq!(d.text(), "abcd\nef");
        d.move_to(at(1, 0), false);
        d.backspace();
        assert_eq!((d.text().as_str(), d.sel.head), ("abcdef", at(0, 4)));
        // The end of the last line: delete does nothing; a new line there has no end after it.
        d.end(false);
        d.delete();
        d.newline();
        d.type_text("g");
        assert_eq!(d.text(), "abcdef\ng");
        // Enter at the start of a line moves all of it down.
        d.move_to(at(0, 0), false);
        d.newline();
        assert_eq!((d.text().as_str(), d.sel.head), ("\nabcdef\ng", at(1, 0)));
    }

    #[test]
    fn selection_at_line_starts_and_ends_and_the_first_and_last_line() {
        let mut d = doc("first\nmiddle\nlast");
        d.move_to(at(0, 0), false);
        d.left(true);
        assert!(d.sel.is_empty(), "nothing before the first line");
        d.end(true);
        assert_eq!(d.selected().to_string(), "first");
        d.right(true);
        assert_eq!(d.selected().to_string(), "first\n", "over the line's end");
        d.move_to(d.buf.end(), false);
        d.right(true);
        assert!(d.sel.is_empty(), "nothing after the last line");
        d.left(true);
        d.home(true);
        assert_eq!(d.selected().to_string(), "last");
        d.left(true);
        assert_eq!(d.selected().to_string(), "\nlast");
        // Typing over a selection that spans lines.
        d.type_text("X");
        assert_eq!(d.text(), "first\nmiddleX");
        d.select_all();
        d.backspace();
        assert_eq!((d.text().as_str(), d.sel.head), ("", at(0, 0)));
        // Left and Right with a selection go to its edge.
        let mut d = doc("abc");
        d.move_to(at(0, 1), false);
        d.move_to(at(0, 3), true);
        d.left(false);
        assert_eq!(d.sel, Sel::at(at(0, 1)));
    }

    #[test]
    fn characters_of_more_than_one_byte_and_emoji_are_one_step() {
        // A thumb with a skin tone, a flag, a family joined with zero-width joiners, an e with a
        // combining accent, and Japanese.
        let line = "a👍🏽🇩🇪👨‍👩‍👧e\u{301}日本";
        let mut d = doc(line);
        let mut stops = vec![0];
        while d.sel.head.col < line.len() {
            d.right(false);
            stops.push(d.sel.head.col);
        }
        assert_eq!(stops.len(), 8, "a, thumb, flag, family, é, 日, 本: {stops:?}");
        for stop in &stops {
            assert!(line.is_char_boundary(*stop));
        }
        d.backspace();
        d.backspace();
        assert_eq!(d.buf.line(0), "a👍🏽🇩🇪👨‍👩‍👧e\u{301}");
        d.backspace();
        assert_eq!(d.buf.line(0), "a👍🏽🇩🇪👨‍👩‍👧", "the accent goes with its letter");
        d.backspace();
        assert_eq!(d.buf.line(0), "a👍🏽🇩🇪");
        d.move_to(at(0, 1), false);
        d.delete();
        assert_eq!(d.buf.line(0), "a🇩🇪");
        // A click inside an emoji lands at its start.
        assert_eq!(snap_grapheme("a🇩🇪", 5), 1);
        // The input method counts UTF-16: 🇩🇪 is four units.
        assert_eq!(d.to_utf16(0, at(0, d.buf.line(0).len())), 5);
        assert_eq!(d.from_utf16(0, 5), at(0, 9));
    }

    #[test]
    fn typing_runs_are_undone_by_the_word() {
        let mut d = doc("");
        for c in "hello brave new world".chars() {
            d.type_text(&c.to_string());
        }
        assert!(d.undo());
        assert_eq!(d.text(), "hello brave new ");
        assert!(d.undo());
        assert_eq!(d.text(), "hello brave ");
        assert!(d.redo());
        assert!(d.redo());
        assert_eq!(d.text(), "hello brave new world");
        assert!(!d.redo());
        // A move of the cursor ends a run.
        d.move_to(at(0, 5), false);
        d.type_text(",");
        d.move_to(d.buf.end(), false);
        d.type_text("!");
        d.undo();
        assert_eq!(d.text(), "hello, brave new world");
        d.undo();
        assert_eq!(d.text(), "hello brave new world");
        // Backspaces in a row are one step.
        d.move_to(d.buf.end(), false);
        for _ in 0..5 {
            d.backspace();
        }
        assert_eq!(d.text(), "hello brave new ");
        d.undo();
        assert_eq!((d.text().as_str(), d.sel.head), ("hello brave new world", at(0, 21)));
        // An edit after an undo drops what could be redone.
        d.undo();
        d.type_text("x");
        assert!(!d.redo());
    }

    #[test]
    fn undo_and_redo_across_a_save() {
        let mut d = doc("v1\n");
        assert!(!d.dirty());
        d.move_to(at(0, 2), false);
        d.type_text("a");
        assert!(d.dirty());
        let (revision, bytes) = d.begin_save();
        assert_eq!(bytes, b"v1a\n");
        d.mark_saved(revision);
        assert!(!d.dirty());
        // Typing after the save is a new step even without a pause, so undo goes back to the
        // saved text exactly.
        d.type_text("b");
        assert!(d.dirty());
        d.undo();
        assert_eq!(d.text(), "v1a\n");
        assert!(!d.dirty(), "back at the saved text");
        d.undo();
        assert_eq!(d.text(), "v1\n");
        assert!(d.dirty(), "before the saved text is a change too");
        d.redo();
        assert!(!d.dirty());
        d.redo();
        assert!(d.dirty());
        // Typing while the save runs: the letter is not saved, whatever step it joins.
        let mut d = doc("");
        d.type_text("a");
        d.type_text("b");
        let (revision, _) = d.begin_save();
        d.type_text("c");
        d.mark_saved(revision);
        assert!(d.dirty(), "\"c\" is not on the disk");
        d.undo();
        assert_eq!(d.text(), "ab");
        assert!(!d.dirty());
        // Twenty steps back and forward.
        let mut d = doc("");
        for i in 0..20 {
            d.type_text(&format!("w{i} "));
        }
        let all = d.text();
        for _ in 0..20 {
            assert!(d.undo());
        }
        assert_eq!(d.text(), "");
        assert!(!d.undo());
        for _ in 0..20 {
            assert!(d.redo());
        }
        assert_eq!(d.text(), all);
    }

    #[test]
    fn the_history_is_bounded() {
        let mut d = doc("");
        for i in 0..(MAX_STEPS + 50) {
            d.newline();
            d.type_text(&i.to_string());
        }
        assert!(d.history.done.len() <= MAX_STEPS);
        let mut undone = 0;
        while d.undo() {
            undone += 1;
        }
        assert_eq!(undone, MAX_STEPS);
        assert!(d.dirty(), "the oldest steps are gone, so this is not the text that was read");
    }

    #[test]
    fn tab_home_and_words() {
        let mut d = doc("    let x = 1;\nfn f()");
        d.move_to(at(0, 9), false);
        d.home(false);
        assert_eq!(d.sel.head, at(0, 4), "first the start of the text");
        d.home(false);
        assert_eq!(d.sel.head, at(0, 0), "then the start of the line");
        d.home(false);
        assert_eq!(d.sel.head, at(0, 4));
        d.tab();
        assert_eq!(d.buf.line(0), "        let x = 1;");
        d.backspace();
        assert_eq!(d.buf.line(0), "    let x = 1;", "one key takes the four spaces back");
        d.outdent();
        assert_eq!(d.buf.line(0), "let x = 1;");
        // Over several lines: each line in and out, one undo step each.
        d.select_all();
        d.tab();
        assert_eq!(d.text(), "    let x = 1;\n    fn f()");
        d.outdent();
        assert_eq!(d.text(), "let x = 1;\nfn f()");
        d.undo();
        assert_eq!(d.text(), "    let x = 1;\n    fn f()");
        // Words, and across a line's end.
        let mut d = doc("one two\nthree");
        d.move_to(at(0, 0), false);
        d.word_right(false);
        assert_eq!(d.sel.head, at(0, 3));
        d.word_right(false);
        d.word_right(false);
        assert_eq!(d.sel.head, at(1, 0));
        d.word_left(false);
        assert_eq!(d.sel.head, at(0, 7));
        d.word_left(false);
        assert_eq!(d.sel.head, at(0, 4));
        assert_eq!(d.word_range(at(0, 5)), at(0, 4)..at(0, 7));
        assert_eq!(d.line_range(0), at(0, 0)..at(1, 0));
        assert_eq!(d.line_range(1), at(1, 0)..at(1, 5));
        d.delete_word_left();
        assert_eq!(d.buf.line(0), "two");
    }

    #[test]
    fn the_input_method_counts_from_the_selection_line() {
        let mut d = doc("first\nsé👍\nlast");
        d.move_to(at(1, 1), false);
        assert_eq!(d.ime_base(None), 1);
        assert_eq!(d.to_utf16(1, d.sel.head), 1);
        // A composition: marked text replaced as it changes, then committed, is one step.
        d.edit(at(1, 1)..at(1, 1), Text::line("k"), Kind::Typing, None);
        d.edit(at(1, 1)..at(1, 2), Text::line("か"), Kind::Typing, None);
        d.edit(at(1, 1)..at(1, 4), Text::line("漢"), Kind::Typing, None);
        assert_eq!(d.buf.line(1), "s漢é👍");
        d.undo();
        assert_eq!(d.buf.line(1), "sé👍");
        assert_eq!(d.from_utf16(1, 4), at(1, d.buf.line(1).len()), "é is one unit, 👍 two");
        assert_eq!(d.from_utf16(1, 5), at(2, 0), "past the line's end, the next line");
        assert_eq!(d.plain(at(1, 0)..at(2, 2)), "sé👍\nla");
    }

    /// A 2 MB file opens well under a second, and a line typed or inserted in its middle costs
    /// a fraction of a millisecond (debug build; a release build is several times faster).
    #[test]
    fn a_2_mb_file_opens_fast_and_its_middle_edits_fast() {
        let line = "    let value = compute(alpha, beta, gamma); // a line of code\n";
        let text = line.repeat((MAX_EDIT_BYTES as usize - 1) / line.len());
        assert!(text.len() as u64 <= MAX_EDIT_BYTES && text.len() > 2_000_000);
        let begun = std::time::Instant::now();
        let mut d = Doc::new(decode(text.as_bytes()).expect("just under 2 MB"));
        let opened = begun.elapsed();
        assert!(opened < std::time::Duration::from_secs(1), "{opened:?}");
        let middle = d.buf.lines() / 2;
        d.move_to(at(middle, 10), false);
        let begun = std::time::Instant::now();
        d.newline();
        let newline = begun.elapsed();
        let begun = std::time::Instant::now();
        for c in "typed".chars() {
            d.type_text(&c.to_string());
        }
        let typed = begun.elapsed() / 5;
        // The letters, then the indent the new line copied (one key), then the line break.
        for _ in 0..6 {
            d.backspace();
        }
        assert_eq!(d.sel.head, at(middle + 1, 0));
        let begun = std::time::Instant::now();
        d.backspace();
        let joined = begun.elapsed();
        eprintln!("2 MB, {} lines: open {opened:?}; a new line in the middle {newline:?}; a letter {typed:?}; a backspace that joins two lines {joined:?}", d.buf.lines());
        assert!(newline < std::time::Duration::from_millis(50), "{newline:?}");
        assert!(joined < std::time::Duration::from_millis(50), "{joined:?}");
        assert!(typed < std::time::Duration::from_millis(5), "{typed:?}");
        assert_eq!(d.text(), text, "the same text again");
    }
}
