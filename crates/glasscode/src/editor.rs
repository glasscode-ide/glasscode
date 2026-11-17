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
use std::path::Path;

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use crate::file_view::{Stamp, size_words, stamp};

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

    /// The text of `revision` is on the disk now. Later typing starts a new step, so the saved
    /// step stays the saved text.
    pub fn mark_saved(&mut self, revision: u64) {
        self.history.seal();
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

/// What the buffer is based on: the disk's stamp and a hash of the bytes it was read from or
/// saved as. A change of the stamp alone (a touch, a checkout of the same text) is no change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Base {
    pub stamp: Stamp,
    pub hash: u64,
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
    let base = Base { stamp, hash: hash(&bytes) };
    decode(&bytes).map(|buf| (buf, base))
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
        d.mark_saved(d.revision());
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
