//! Selecting text in the Chat view, as in a terminal or a browser: drag across messages,
//! replies, code and tool output, double-click for a word, triple-click for a paragraph, the
//! copy key to copy.
//!
//! Every piece of text the view draws is made here (`Selection::text`), in reading order, so a
//! selection is two positions (piece, byte offset) and each piece knows which of its bytes are
//! inside it. The pieces' layouts from the last frame answer where the mouse is.

use std::ops::Range;

use gpui::{HighlightStyle, Hsla, Point, Pixels, SharedString, StyledText, TextLayout};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    piece: usize,
    offset: usize,
}

struct Piece {
    layout: TextLayout,
    text: SharedString,
}

#[derive(Default)]
pub struct Selection {
    pieces: Vec<Piece>,
    anchor: Option<Pos>,
    head: Option<Pos>,
    pub dragging: bool,
}

impl Selection {
    /// Start drawing a frame: the pieces are made again, in order.
    pub fn begin(&mut self) {
        self.pieces.clear();
    }

    /// A piece of text, with its own styling, tinted where it is selected.
    pub fn text(&mut self, text: impl Into<SharedString>, highlights: Vec<(Range<usize>, HighlightStyle)>, mono: Vec<(Range<usize>, SharedString)>) -> StyledText {
        let text: SharedString = text.into();
        let index = self.pieces.len();
        let highlights = match self.range_in(index, text.len()) {
            Some(range) => overlay(highlights, range, HighlightStyle { background_color: Some(tint()), ..Default::default() }),
            None => highlights,
        };
        let styled = StyledText::new(text.clone()).with_highlights(highlights).with_font_family_overrides(mono);
        self.pieces.push(Piece { layout: styled.layout().clone(), text });
        styled
    }

    /// Plain text (one style).
    pub fn plain(&mut self, text: impl Into<SharedString>) -> StyledText {
        self.text(text, Vec::new(), Vec::new())
    }

    fn ordered(&self) -> Option<(Pos, Pos)> {
        let (a, b) = (self.anchor?, self.head?);
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        (start != end).then_some((start, end))
    }

    /// The selected bytes of piece `index` (of `len` bytes).
    fn range_in(&self, index: usize, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.ordered()?;
        if index < start.piece || index > end.piece {
            return None;
        }
        let from = if index == start.piece { start.offset.min(len) } else { 0 };
        let to = if index == end.piece { end.offset.min(len) } else { len };
        (from < to).then_some(from..to)
    }

    /// Debug builds' UI tests: select whole pieces `from` to `to`.
    pub fn select_pieces(&mut self, from: usize, to: usize) {
        let to = to.min(self.pieces.len().saturating_sub(1));
        self.anchor = Some(Pos { piece: from.min(to), offset: 0 });
        self.head = Some(Pos { piece: to, offset: self.pieces.get(to).map(|p| p.text.len()).unwrap_or(0) });
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
        self.dragging = false;
    }

    /// Where a point is in the text: the piece under it, or the nearest one in reading order.
    fn pos_at(&self, at: Point<Pixels>) -> Option<Pos> {
        let laid: Vec<(usize, gpui::Bounds<Pixels>)> = self.pieces.iter().enumerate().filter_map(|(i, p)| p.layout.try_bounds().map(|b| (i, b))).collect();
        if laid.is_empty() {
            return None;
        }
        let offset_in = |i: usize, point: Point<Pixels>| {
            let piece = &self.pieces[i];
            match piece.layout.index_for_position(point) {
                Ok(o) | Err(o) => o.min(piece.text.len()),
            }
        };
        // On a line of a piece: the one under the pointer, or the nearest on that line.
        let on_line: Vec<&(usize, gpui::Bounds<Pixels>)> = laid.iter().filter(|(_, b)| at.y >= b.top() && at.y <= b.bottom()).collect();
        if let Some((i, b)) = on_line.iter().min_by_key(|(_, b)| {
            let dx = if at.x < b.left() { b.left() - at.x } else if at.x > b.right() { at.x - b.right() } else { Pixels::ZERO };
            (f32::from(dx) * 100.) as i64
        }) {
            let x = at.x.max(b.left()).min(b.right());
            return Some(Pos { piece: *i, offset: offset_in(*i, gpui::point(x, at.y)) });
        }
        // Between pieces: the end of the last one above, or the start of the first.
        match laid.iter().filter(|(_, b)| b.bottom() < at.y).max_by_key(|(i, _)| *i) {
            Some((i, _)) => Some(Pos { piece: *i, offset: self.pieces[*i].text.len() }),
            None => Some(Pos { piece: laid[0].0, offset: 0 }),
        }
    }

    /// Mouse down: start a selection (2 clicks: the word; 3: the whole piece).
    pub fn press(&mut self, at: Point<Pixels>, clicks: usize, extend: bool) {
        let Some(pos) = self.pos_at(at) else {
            self.clear();
            return;
        };
        self.dragging = true;
        if extend && self.anchor.is_some() {
            self.head = Some(pos);
            return;
        }
        let text = &self.pieces[pos.piece].text;
        let (start, end) = match clicks {
            2 => word_at(text, pos.offset),
            n if n >= 3 => (0, text.len()),
            _ => (pos.offset, pos.offset),
        };
        self.anchor = Some(Pos { piece: pos.piece, offset: start });
        self.head = Some(Pos { piece: pos.piece, offset: end });
    }

    /// Mouse moved with the button down: the selection follows.
    pub fn drag(&mut self, at: Point<Pixels>) -> bool {
        if !self.dragging {
            return false;
        }
        let Some(pos) = self.pos_at(at) else { return false };
        let changed = self.head != Some(pos);
        self.head = Some(pos);
        changed
    }

    /// The selected text, a line per piece (without the spacing put around code).
    pub fn copied(&self) -> Option<String> {
        let (start, end) = self.ordered()?;
        let mut parts = Vec::new();
        for index in start.piece..=end.piece.min(self.pieces.len().saturating_sub(1)) {
            let text = &self.pieces[index].text;
            if let Some(range) = self.range_in(index, text.len()) {
                parts.push(text[range].replace('\u{202F}', ""));
            }
        }
        let joined = parts.join("\n");
        (!joined.trim().is_empty()).then_some(joined)
    }
}

/// The selection's tint: the link blue, see-through.
fn tint() -> Hsla {
    let mut color = theme::link();
    color.a = if theme::is_light() { 0.22 } else { 0.38 };
    color
}

/// The word around `offset` (or just that spot, between words).
fn word_at(text: &str, offset: usize) -> (usize, usize) {
    text.split_word_bound_indices()
        .find(|(start, word)| *start <= offset && offset <= start + word.len() && !word.trim().is_empty())
        .map(|(start, word)| (start, start + word.len()))
        .unwrap_or((offset, offset))
}

/// `highlights` (sorted, not overlapping) with `extra` laid over `range`: the ranges split
/// where the selection starts and ends, so every part keeps its own look plus the tint.
fn overlay(highlights: Vec<(Range<usize>, HighlightStyle)>, range: Range<usize>, extra: HighlightStyle) -> Vec<(Range<usize>, HighlightStyle)> {
    // Cut points: every boundary of a highlight and of the selection.
    let mut cuts: Vec<usize> = highlights.iter().flat_map(|(r, _)| [r.start, r.end]).chain([range.start, range.end]).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        if from >= to {
            continue;
        }
        let base = highlights.iter().find(|(r, _)| r.start <= from && to <= r.end).map(|(_, s)| *s);
        let selected = range.start <= from && to <= range.end;
        let style = match (base, selected) {
            (Some(b), true) => b.highlight(extra),
            (Some(b), false) => b,
            (None, true) => extra,
            (None, false) => continue,
        };
        out.push((from..to, style));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_the_selection_over_highlights() {
        let bold = HighlightStyle { font_weight: Some(gpui::FontWeight::BOLD), ..Default::default() };
        let tint = HighlightStyle { background_color: Some(gpui::black()), ..Default::default() };
        let out = overlay(vec![(2..6, bold)], 4..9, tint);
        let ranges: Vec<Range<usize>> = out.iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(ranges, vec![2..4, 4..6, 6..9]);
        assert_eq!(out[0].1, bold);
        assert_eq!(out[1].1, bold.highlight(tint));
        assert_eq!(out[2].1, tint);
    }

    #[test]
    fn finds_words() {
        assert_eq!(word_at("fix the token refresh", 9), (8, 13));
        assert_eq!(word_at("a  b", 2), (2, 2));
    }
}
