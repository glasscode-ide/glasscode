//! Syntax colours (U21): a small scanner of our own (decision P6, no dependency) that finds the
//! comments, strings, numbers, keywords and type names in one line of a file, for the file view,
//! the diff and the editor. No window, no clock.
//!
//! A block comment, a string over several lines or fenced code goes on from one line to the
//! next, so each line starts in a `State`: the one the line above ended in. `Starts` keeps the
//! state at the start of each line of a text and brings it up to date after an edit, from the
//! edited line down only until a line starts as it did before. Colouring a line on screen is then
//! one call to `scan_line` with that line's state.
//!
//! It is a scanner, not a parser, and it never fails. Where it guesses wrong the damage stays
//! small: a string that is not closed is coloured to the end of its line (or, for the kinds that
//! go on over lines, until something closes it), and the ranges it gives are always sorted,
//! apart, inside the line and on character boundaries.
//!
//! Besides the five kinds of the plan it has two: `Key` (a JSON object's keys, TOML keys) and
//! `Heading` (Markdown headings, a TOML table's head). In Markdown, fenced and inline code is
//! `String`, an HTML comment `Comment`, and a list's or a quote's mark `Keyword`.
//!
//! Known limits, each confined to the line it is in: a JavaScript regular expression is not
//! known (a quote in one starts a string to the line's end), a template literal ends at the next
//! backtick whatever `${…}` holds, an apostrophe in JSX text starts a string to the line's end.
//! Shell strings and here-documents do not go on over lines here (a string that does is coloured
//! to its first line's end), so a lone apostrophe in a shell file costs one line, not the file.

use std::ops::Range;

/// The languages that get colours. Every other file has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    /// .ts, .tsx, .mts, .cts
    TypeScript,
    /// .js, .jsx, .mjs, .cjs
    JavaScript,
    /// .json and .jsonc: comments are allowed in both.
    Json,
    Toml,
    Markdown,
    Python,
    /// sh, bash and zsh.
    Shell,
    PowerShell,
}

impl Language {
    pub const ALL: [Language; 9] = [
        Language::Rust,
        Language::TypeScript,
        Language::JavaScript,
        Language::Json,
        Language::Toml,
        Language::Markdown,
        Language::Python,
        Language::Shell,
        Language::PowerShell,
    ];

    /// The language of a file, from its name (or a path that ends in it): its extension, or a
    /// few names of their own. None for anything else, which gets no colours.
    pub fn of(name: &str) -> Option<Language> {
        let name = name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(name)
            .to_ascii_lowercase();
        match name.as_str() {
            "cargo.lock" | "poetry.lock" | "uv.lock" | "pipfile" => return Some(Language::Toml),
            ".bashrc" | ".bash_profile" | ".bash_logout" | ".profile" | ".zshrc" | ".zprofile"
            | ".zshenv" | ".zlogin" | "pkgbuild" => {
                return Some(Language::Shell);
            }
            _ => {}
        }
        // ".gitignore" is a name, not an extension.
        let (stem, extension) = name.rsplit_once('.')?;
        if stem.is_empty() {
            return None;
        }
        Some(match extension {
            "rs" => Language::Rust,
            "ts" | "tsx" | "mts" | "cts" => Language::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Language::JavaScript,
            "json" | "jsonc" => Language::Json,
            "toml" => Language::Toml,
            "md" | "markdown" => Language::Markdown,
            "py" | "pyi" | "pyw" => Language::Python,
            "sh" | "bash" | "zsh" => Language::Shell,
            "ps1" | "psm1" | "psd1" => Language::PowerShell,
            _ => return None,
        })
    }
}

/// What a part of a line is; the theme has a colour for each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Comment,
    String,
    Number,
    Keyword,
    Type,
    /// A key of a JSON object or a TOML table.
    Key,
    /// A Markdown heading; a TOML table's head.
    Heading,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::Comment,
        Kind::String,
        Kind::Number,
        Kind::Keyword,
        Kind::Type,
        Kind::Key,
        Kind::Heading,
    ];
}

/// A part of a line with a colour: bytes of the line, on character boundaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub range: Range<usize>,
    pub kind: Kind,
}

/// What a line starts in: plain code, or something the lines above left open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct State {
    mode: Mode,
    /// How deep a Rust block comment is, a raw string's number of #, the quote of three quotes
    /// or of a here-string, the length of a Markdown fence.
    n: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum Mode {
    #[default]
    Code,
    /// A block comment: `/* */` (Rust's nest), `<# #>`, `<!-- -->`.
    Comment,
    /// A Rust string ("…", b"…", c"…"), which goes on over lines.
    Str,
    /// A Rust raw string (r#"…"#).
    Raw,
    /// A JavaScript or TypeScript template literal.
    Template,
    /// Three quotes: Python, TOML.
    Triple,
    /// Markdown fenced code, of backticks or of tildes.
    FenceTick,
    FenceTilde,
    /// A PowerShell here-string (@"…"@).
    Here,
}

impl State {
    const fn new(mode: Mode, n: u8) -> State {
        State { mode, n }
    }

    /// Inside something that goes on over lines: a block comment, a string, fenced code.
    pub fn is_open(self) -> bool {
        self.mode != Mode::Code
    }
}

/// The parts of one line that get a colour, and the state the next line starts in. `text` is
/// the line without its end; `start` is the state the line above ended in (`State::default()`
/// for a file's first line).
pub fn scan_line(language: Language, start: State, text: &str) -> (Vec<Token>, State) {
    let mut tokens = Vec::new();
    let end = run(language, start, text, Some(&mut tokens));
    // The scanner gives ranges that are sorted, apart and on character boundaries. A range that
    // is not would break the shaping of the line, so it would be left out rather than drawn.
    let mut last = 0;
    tokens.retain(|t| {
        let fits = last <= t.range.start
            && t.range.start < t.range.end
            && t.range.end <= text.len()
            && text.is_char_boundary(t.range.start)
            && text.is_char_boundary(t.range.end);
        if fits {
            last = t.range.end;
        }
        fits
    });
    (tokens, end)
}

/// The state the next line starts in, without the ranges (no allocation).
pub fn end_state(language: Language, start: State, text: &str) -> State {
    run(language, start, text, None)
}

fn run(language: Language, start: State, text: &str, out: Option<&mut Vec<Token>>) -> State {
    let mut scan = Scan {
        lang: language,
        s: text.as_bytes(),
        i: 0,
        out,
    };
    if language == Language::Markdown {
        return scan.markdown(start);
    }
    match scan.resume(start) {
        Some(open) => open,
        None => scan.code(),
    }
}

/// The state at the start of each line of a text, kept in step with its edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Starts {
    language: Language,
    states: Vec<State>,
}

impl Starts {
    /// Scans the whole text once (a file opening: in the background).
    pub fn new<'a>(language: Language, lines: impl IntoIterator<Item = &'a str>) -> Starts {
        let mut states = Vec::new();
        let mut state = State::default();
        for line in lines {
            states.push(state);
            state = end_state(language, state, line);
        }
        Starts { language, states }
    }

    pub fn language(&self) -> Language {
        self.language
    }

    /// The number of lines.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// The state line `line` starts in (plain code past the end).
    pub fn at(&self, line: usize) -> State {
        self.states.get(line).copied().unwrap_or_default()
    }

    /// The coloured parts of line `line`, whose text is `text`.
    pub fn line(&self, line: usize, text: &str) -> Vec<Token> {
        scan_line(self.language, self.at(line), text).0
    }

    /// Lines `first..first + removed` were replaced by `inserted` lines; `line(i)` is line `i` of
    /// the text now. The edited lines are scanned again, and the lines below them only until one
    /// starts in the state it started in before (from there on nothing can differ). The number of
    /// lines scanned: one for a letter typed in a line of code; all below for a `/*` that opens a
    /// comment nothing closes.
    pub fn edited<'a>(
        &mut self,
        first: usize,
        removed: usize,
        inserted: usize,
        line: impl Fn(usize) -> &'a str,
    ) -> usize {
        let first = first.min(self.states.len());
        let removed = removed.min(self.states.len() - first);
        // The lines above did not change, so the first edited line starts as it did.
        let start = self.at(first);
        // A letter typed changes one line into one: nothing moves (the edited lines' states are
        // all scanned again below, whatever they hold now).
        if removed != inserted {
            self.states
                .splice(first..first + removed, std::iter::repeat_n(start, inserted));
        }
        if let Some(state) = self.states.get_mut(first) {
            *state = start;
        }
        let lines = self.states.len();
        let mut scanned = 0;
        let mut i = first;
        while i < lines {
            let end = end_state(self.language, self.states[i], line(i));
            scanned += 1;
            i += 1;
            // Every edited line is scanned; a line below them stops it once it starts as before.
            if i == lines || (i >= first + inserted && self.states[i] == end) {
                break;
            }
            self.states[i] = end;
        }
        scanned
    }
}

// ----- the scanner -------------------------------------------------------------------------------

/// The bytes a UTF-8 character has, from its first byte (1 for a byte that cannot start one).
fn char_len(b: u8) -> usize {
    match b {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xFF => 4,
        _ => 1,
    }
}

/// A letter of a name: ASCII letters, digits, `_`, and every byte of a character beyond ASCII
/// (so a name in any script is one word, and a word never ends inside a character).
fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn is_blank(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r')
}

struct Scan<'a, 'o> {
    lang: Language,
    s: &'a [u8],
    i: usize,
    /// None when only the state at the line's end is wanted.
    out: Option<&'o mut Vec<Token>>,
}

impl Scan<'_, '_> {
    fn len(&self) -> usize {
        self.s.len()
    }

    /// The byte at `i`, or 0 past the end.
    fn at(&self, i: usize) -> u8 {
        self.s.get(i).copied().unwrap_or(0)
    }

    fn starts(&self, i: usize, pattern: &[u8]) -> bool {
        self.s
            .get(i..)
            .is_some_and(|rest| rest.starts_with(pattern))
    }

    /// Only blanks from `i` to the line's end.
    fn rest_blank(&self, i: usize) -> bool {
        self.s
            .get(i..)
            .is_none_or(|rest| rest.iter().all(|b| is_blank(*b)))
    }

    /// Past an escape at `i` and the whole character it escapes.
    fn skip_escaped(&mut self, i: usize) {
        self.i = (i + 1 + char_len(self.at(i + 1))).min(self.len());
    }

    fn push(&mut self, start: usize, end: usize, kind: Kind) {
        let len = self.s.len();
        let Some(out) = self.out.as_deref_mut() else {
            return;
        };
        let end = end.min(len);
        let start = start.max(out.last().map_or(0, |t| t.range.end));
        if start >= end {
            return;
        }
        match out.last_mut() {
            Some(last) if last.kind == kind && last.range.end == start => last.range.end = end,
            _ => out.push(Token {
                range: start..end,
                kind,
            }),
        }
    }

    // ----- what goes on over lines

    /// The line goes on with what the line above left open. None when that closes in this line:
    /// the rest, from `self.i`, is code.
    fn resume(&mut self, state: State) -> Option<State> {
        match state.mode {
            Mode::Comment => self.block_comment(0, 0, state.n),
            Mode::Str if self.lang == Language::Rust => {
                self.string(0, 0, b"\"", b'\\', Some(state))
            }
            Mode::Raw if self.lang == Language::Rust => self.raw(0, 0, state.n),
            Mode::Template if matches!(self.lang, Language::TypeScript | Language::JavaScript) => {
                self.string(0, 0, b"`", b'\\', Some(state))
            }
            Mode::Triple if matches!(self.lang, Language::Python | Language::Toml) => {
                let (close, escape) = triple_close(self.lang, state.n);
                self.string(0, 0, close, escape, Some(state))
            }
            Mode::Here if self.lang == Language::PowerShell => {
                if self.at(0) == state.n && self.at(1) == b'@' {
                    self.push(0, 2, Kind::String);
                    self.i = 2;
                    None
                } else {
                    self.push(0, self.len(), Kind::String);
                    self.i = self.len();
                    Some(state)
                }
            }
            // Plain code, or a state of another language: start as code.
            _ => None,
        }
    }

    /// A block comment that starts at `start` and goes on from `from`, `depth` deep: to its end,
    /// or to the line's end (then the state the next line starts in).
    fn block_comment(&mut self, start: usize, from: usize, depth: u8) -> Option<State> {
        let (open, close): (&[u8], &[u8]) = match self.lang {
            Language::PowerShell => (b"<#", b"#>"),
            Language::Markdown => (b"<!--", b"-->"),
            _ => (b"/*", b"*/"),
        };
        let nests = self.lang == Language::Rust;
        let mut depth = depth.max(1);
        let mut j = from;
        while j < self.len() {
            if self.starts(j, close) {
                j += close.len();
                depth -= 1;
                if depth == 0 {
                    self.push(start, j, Kind::Comment);
                    self.i = j;
                    return None;
                }
            } else if nests && self.starts(j, open) {
                depth = depth.saturating_add(1);
                j += open.len();
            } else {
                j += 1;
            }
        }
        self.push(start, self.len(), Kind::Comment);
        self.i = self.len();
        Some(State::new(Mode::Comment, depth))
    }

    /// Where a string whose text begins at `from` ends (after `close`), or None at the line's end.
    /// `escape` (0 for none) makes the character after it part of the string.
    fn string_end(&self, from: usize, close: &[u8], escape: u8) -> Option<usize> {
        let mut j = from;
        while j < self.len() {
            let b = self.s[j];
            if escape != 0 && b == escape {
                j += 2;
            } else if b == close[0] && self.starts(j, close) {
                return Some(j + close.len());
            } else {
                j += 1;
            }
        }
        None
    }

    /// A string from `start` whose text begins at `from`: coloured to its end. When the line ends
    /// first it is coloured to the line's end, and `open` is the state the next line starts in
    /// (None: this kind of string ends with its line).
    fn string(
        &mut self,
        start: usize,
        from: usize,
        close: &[u8],
        escape: u8,
        open: Option<State>,
    ) -> Option<State> {
        match self.string_end(from, close, escape) {
            Some(end) => {
                self.push(start, end, Kind::String);
                self.i = end;
                None
            }
            None => {
                self.push(start, self.len(), Kind::String);
                self.i = self.len();
                open
            }
        }
    }

    /// A Rust raw string from `start`, its text from `from`, closed by `"` and `hashes` #.
    fn raw(&mut self, start: usize, from: usize, hashes: u8) -> Option<State> {
        let mut j = from;
        while j < self.len() {
            if self.s[j] == b'"' && (1..=hashes as usize).all(|k| self.at(j + k) == b'#') {
                let end = j + 1 + hashes as usize;
                self.push(start, end, Kind::String);
                self.i = end;
                return None;
            }
            j += 1;
        }
        self.push(start, self.len(), Kind::String);
        self.i = self.len();
        Some(State::new(Mode::Raw, hashes))
    }

    // ----- code

    fn code(&mut self) -> State {
        let lang = self.lang;
        if self.i == 0 {
            match lang {
                Language::Toml => self.toml_line_start(),
                Language::Python => self.decorator(),
                _ => {}
            }
        }
        let slashes = matches!(
            lang,
            Language::Rust | Language::TypeScript | Language::JavaScript | Language::Json
        );
        while self.i < self.len() {
            let i = self.i;
            let b = self.s[i];
            let open = match b {
                b' ' | b'\t' | b'\r' => {
                    self.i += 1;
                    None
                }
                b'0'..=b'9' => {
                    self.number();
                    None
                }
                b'"' | b'\'' | b'`' => self.quote(i, i),
                b'/' if slashes && self.at(i + 1) == b'/' => {
                    self.push(i, self.len(), Kind::Comment);
                    self.i = self.len();
                    None
                }
                b'/' if slashes && self.at(i + 1) == b'*' => self.block_comment(i, i + 2, 1),
                b'#' if self.hash_comment(i) => {
                    self.push(i, self.len(), Kind::Comment);
                    self.i = self.len();
                    None
                }
                b'<' if lang == Language::PowerShell && self.at(i + 1) == b'#' => {
                    self.block_comment(i, i + 2, 1)
                }
                b'@' if lang == Language::PowerShell
                    && matches!(self.at(i + 1), b'"' | b'\'')
                    && self.rest_blank(i + 2) =>
                {
                    let quote = self.at(i + 1);
                    self.push(i, self.len(), Kind::String);
                    self.i = self.len();
                    Some(State::new(Mode::Here, quote))
                }
                b'$' if lang == Language::PowerShell => {
                    self.ps_variable();
                    None
                }
                b'$' if lang == Language::Shell && self.at(i + 1) == b'\'' => {
                    self.string(i, i + 2, b"'", b'\\', None)
                }
                b'-' if lang == Language::Json && self.at(i + 1).is_ascii_digit() => {
                    self.number();
                    None
                }
                b'-' if lang == Language::PowerShell
                    && self.at(i + 1).is_ascii_alphabetic()
                    && self.ps_word_start(i) =>
                {
                    self.ps_operator();
                    None
                }
                b'[' if lang == Language::PowerShell => {
                    self.ps_type();
                    None
                }
                b'\\' if lang == Language::Shell => {
                    self.skip_escaped(i);
                    None
                }
                b if self.word_start(b) => self.word(),
                _ => {
                    self.i += 1;
                    None
                }
            };
            if let Some(state) = open {
                return state;
            }
        }
        State::default()
    }

    /// `#` starts a comment here. In a shell it does only at a word's start (`$#`, `${#x}` and
    /// `a#b` are not comments); Rust, JavaScript and JSON have none.
    fn hash_comment(&self, i: usize) -> bool {
        let before = if i == 0 { b' ' } else { self.s[i - 1] };
        match self.lang {
            Language::Python | Language::Toml => true,
            Language::Shell => {
                is_blank(before) || matches!(before, b';' | b'&' | b'|' | b'(' | b')')
            }
            Language::PowerShell => {
                is_blank(before)
                    || matches!(
                        before,
                        b';' | b'&' | b'|' | b'(' | b')' | b'{' | b'}' | b','
                    )
            }
            _ => false,
        }
    }

    fn word_start(&self, b: u8) -> bool {
        match self.lang {
            Language::TypeScript | Language::JavaScript => is_ident(b) || b == b'$',
            // A shell word holds paths and options: `./do` and `--in` are no keywords.
            Language::Shell => {
                is_ident(b)
                    || matches!(
                        b,
                        b'-' | b'.' | b'/' | b'~' | b'+' | b'%' | b'@' | b':' | b'=' | b','
                    )
            }
            _ => is_ident(b),
        }
    }

    fn word_byte(&self, b: u8) -> bool {
        match self.lang {
            Language::PowerShell => is_ident(b) || b == b'-',
            _ => self.word_start(b),
        }
    }

    /// A word: a keyword, a type, or a string's prefix (Rust `b"…"`, `r#"…"#`; Python `f"…"`).
    fn word(&mut self) -> Option<State> {
        let start = self.i;
        let mut end = start;
        while end < self.len() && self.word_byte(self.s[end]) {
            end += 1;
        }
        let end = end.max(start + 1);
        let w = &self.s[start..end];
        let next = self.at(end);
        match self.lang {
            Language::Rust if matches!(w, b"b" | b"c" | b"r" | b"br" | b"cr") => {
                if w.ends_with(b"r") {
                    let mut k = end;
                    while self.at(k) == b'#' {
                        k += 1;
                    }
                    if self.at(k) == b'"' {
                        return self.raw(start, k + 1, (k - end).min(255) as u8);
                    }
                    if w == b"r" && next == b'#' && is_ident(self.at(end + 1)) {
                        // A raw name (r#type): a name, not a keyword.
                        let mut k = end + 1;
                        while is_ident(self.at(k)) {
                            k += 1;
                        }
                        self.i = k;
                        return None;
                    }
                } else if next == b'"' {
                    return self.quote(start, end);
                } else if w == b"b" && next == b'\'' {
                    self.rust_char(start, end);
                    return None;
                }
            }
            Language::Python
                if w.len() <= 2
                    && w.iter()
                        .all(|c| matches!(c.to_ascii_lowercase(), b'r' | b'b' | b'u' | b'f'))
                    && matches!(next, b'"' | b'\'') =>
            {
                return self.quote(start, end);
            }
            _ => {}
        }
        self.i = end;
        if self.out.is_some() {
            // `x.type`, `.default`: a field or a method, not a keyword (Rust's `.await` is one).
            let after_dot =
                start > 0 && self.s[start - 1] == b'.' && (start < 2 || self.s[start - 2] != b'.');
            if let Some(kind) = classify(self.lang, w, after_dot) {
                self.push(start, end, kind);
            }
        }
        None
    }

    fn number(&mut self) {
        let start = self.i;
        let mut j = start;
        if self.at(j) == b'-' {
            j += 1;
        }
        let hex = self.at(j) == b'0' && matches!(self.at(j + 1), b'x' | b'X');
        loop {
            while j < self.len() && (self.s[j].is_ascii_alphanumeric() || self.s[j] == b'_') {
                j += 1;
            }
            let digit_next = self.at(j + 1).is_ascii_digit();
            match self.at(j) {
                // 1.5, but not the range 1..2 or a method (1.max).
                b'.' if digit_next => j += 1,
                // 1e-5
                b'+' | b'-'
                    if !hex && digit_next && j > start && matches!(self.s[j - 1], b'e' | b'E') =>
                {
                    j += 1
                }
                // TOML's dates and times: 1979-05-27T07:32:00+01:00
                b'-' | b':' | b'+' if self.lang == Language::Toml && digit_next => j += 1,
                _ => break,
            }
        }
        let end = j.max(start + 1).min(self.len());
        self.push(start, end, Kind::Number);
        self.i = end;
    }

    /// A quote at `at` (`start` is before it when the string has a prefix, as `b"…"`).
    fn quote(&mut self, start: usize, at: usize) -> Option<State> {
        let q = self.at(at);
        let three = self.at(at + 1) == q && self.at(at + 2) == q;
        match (self.lang, q) {
            (Language::Rust, b'"') => {
                self.string(start, at + 1, b"\"", b'\\', Some(State::new(Mode::Str, 0)))
            }
            (Language::Rust, b'\'') => {
                self.rust_char(start, at);
                None
            }
            (Language::TypeScript | Language::JavaScript, b'`') => self.string(
                start,
                at + 1,
                b"`",
                b'\\',
                Some(State::new(Mode::Template, 0)),
            ),
            (Language::TypeScript | Language::JavaScript, _) => {
                self.string(start, at + 1, &[q], b'\\', None)
            }
            (Language::Python | Language::Toml, b'"' | b'\'') if three => {
                let (close, escape) = triple_close(self.lang, q);
                self.string(
                    start,
                    at + 3,
                    close,
                    escape,
                    Some(State::new(Mode::Triple, q)),
                )
            }
            (Language::Python, b'"' | b'\'') | (Language::Toml | Language::Shell, b'"') => {
                self.string(start, at + 1, &[q], b'\\', None)
            }
            // A TOML literal string and a shell's single quotes have no escapes.
            (Language::Toml | Language::Shell | Language::PowerShell, b'\'') => {
                self.string(start, at + 1, b"'", 0, None)
            }
            // PowerShell escapes with a backtick.
            (Language::PowerShell, b'"') => self.string(start, at + 1, b"\"", b'`', None),
            (Language::PowerShell, b'`') => {
                self.skip_escaped(at);
                None
            }
            (Language::Json, b'"') => {
                let open = self.string(start, at + 1, b"\"", b'\\', None);
                // A string before ":" is a key.
                let mut k = self.i;
                while is_blank(self.at(k)) {
                    k += 1;
                }
                if open.is_none() && self.at(k) == b':' {
                    let end = self.i;
                    if let Some(last) = self
                        .out
                        .as_deref_mut()
                        .and_then(|out| out.last_mut())
                        .filter(|t| t.range.end == end && t.kind == Kind::String)
                    {
                        last.kind = Kind::Key;
                    }
                }
                open
            }
            _ => {
                self.i = at + 1;
                None
            }
        }
    }

    /// Rust's `'`: a character ('a', '\n', '\u{1F600}', b'x'), or else a lifetime or a label
    /// ('a, 'static), which has no colour.
    fn rust_char(&mut self, start: usize, at: usize) {
        let end = if self.at(at + 1) == b'\\' {
            (at + 3..(at + 14).min(self.len()))
                .find(|&k| self.s[k] == b'\'')
                .map(|k| k + 1)
        } else if at + 1 < self.len() && self.s[at + 1] != b'\'' {
            let k = at + 1 + char_len(self.s[at + 1]);
            (self.at(k) == b'\'').then_some(k + 1)
        } else {
            None
        };
        match end {
            Some(end) => {
                self.push(start, end, Kind::String);
                self.i = end;
            }
            None => {
                let mut j = at + 1;
                while j < self.len() && is_ident(self.s[j]) {
                    j += 1;
                }
                self.i = j;
            }
        }
    }

    /// A TOML line's start: a table's head (`[server]`, `[[bin]]`) or a key before its `=`.
    fn toml_line_start(&mut self) {
        let n = self.len();
        let mut j = 0;
        while j < n && is_blank(self.s[j]) {
            j += 1;
        }
        if j == n {
            return;
        }
        if self.s[j] == b'[' {
            // No "=" inside, and only a comment after: an array's element is not a head.
            let Some(close) = (j + 1..n).find(|&k| self.s[k] == b']') else {
                return;
            };
            if self.s[j + 1..close].contains(&b'=') {
                return;
            }
            let end = if self.at(close + 1) == b']' {
                close + 2
            } else {
                close + 1
            };
            let mut k = end;
            while k < n && is_blank(self.s[k]) {
                k += 1;
            }
            if k == n || self.s[k] == b'#' {
                self.push(j, end, Kind::Heading);
                self.i = end;
            }
            return;
        }
        // Bare words, dots and quoted parts, then "=".
        let mut k = j;
        let mut end = j;
        while k < n {
            match self.s[k] {
                b'=' => break,
                b' ' | b'\t' | b'.' => k += 1,
                b'"' | b'\'' => {
                    let q = self.s[k];
                    let Some(close) = (k + 1..n).find(|&m| self.s[m] == q) else {
                        return;
                    };
                    k = close + 1;
                    end = k;
                }
                c if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' => {
                    k += 1;
                    end = k;
                }
                // An array's element or a value that goes on: not a key.
                _ => return,
            }
        }
        if k < n && end > j {
            self.push(j, end, Kind::Key);
            self.i = k;
        }
    }

    /// A Python decorator at the line's start (`@property`, `@app.route`).
    fn decorator(&mut self) {
        let mut j = 0;
        while is_blank(self.at(j)) {
            j += 1;
        }
        if self.at(j) != b'@' || !(self.at(j + 1).is_ascii_alphabetic() || self.at(j + 1) == b'_') {
            return;
        }
        let mut k = j + 1;
        while k < self.len() && (is_ident(self.s[k]) || self.s[k] == b'.') {
            k += 1;
        }
        self.push(j, k, Kind::Keyword);
        self.i = k;
    }

    /// A PowerShell variable: no colour, except `$true`, `$false` and `$null`.
    fn ps_variable(&mut self) {
        let start = self.i;
        let mut j = start + 1;
        if self.at(j) == b'{' {
            while j < self.len() && self.s[j] != b'}' {
                j += 1;
            }
            self.i = (j + 1).min(self.len());
            return;
        }
        while j < self.len() && (is_ident(self.s[j]) || self.s[j] == b':') {
            j += 1;
        }
        let w = &self.s[start + 1..j];
        if [&b"true"[..], b"false", b"null"]
            .iter()
            .any(|k| w.eq_ignore_ascii_case(k))
        {
            self.push(start, j, Kind::Keyword);
        }
        self.i = j.max(start + 1);
    }

    /// Before a PowerShell operator (`-eq`, `-and`): the start of the line or a gap.
    fn ps_word_start(&self, i: usize) -> bool {
        i == 0
            || is_blank(self.s[i - 1])
            || matches!(self.s[i - 1], b'(' | b',' | b'{' | b'=' | b'|')
    }

    fn ps_operator(&mut self) {
        let start = self.i;
        let mut j = start + 1;
        while j < self.len() && (self.s[j].is_ascii_alphanumeric() || self.s[j] == b'_') {
            j += 1;
        }
        let w = &self.s[start + 1..j];
        if PS_OPERATORS
            .iter()
            .any(|k| w.eq_ignore_ascii_case(k.as_bytes()))
        {
            self.push(start, j, Kind::Keyword);
        }
        self.i = j;
    }

    /// A type in brackets: `[string]`, `[System.IO.File]`.
    fn ps_type(&mut self) {
        let start = self.i;
        let mut j = start + 1;
        if self.at(j).is_ascii_alphabetic() {
            while j < self.len()
                && j - start < 128
                && (self.s[j].is_ascii_alphanumeric() || matches!(self.s[j], b'_' | b'.'))
            {
                j += 1;
            }
            if self.at(j) == b']' {
                self.push(start + 1, j, Kind::Type);
                self.i = j + 1;
                return;
            }
        }
        self.i = start + 1;
    }

    // ----- Markdown

    fn markdown(&mut self, state: State) -> State {
        let n = self.len();
        match state.mode {
            Mode::FenceTick | Mode::FenceTilde => {
                let fence = if state.mode == Mode::FenceTick {
                    b'`'
                } else {
                    b'~'
                };
                self.push(0, n, Kind::String);
                self.i = n;
                let j = self.indent();
                let mut k = j;
                while self.at(k) == fence {
                    k += 1;
                }
                let closes = j <= 3 && k - j >= state.n as usize && self.rest_blank(k);
                return if closes { State::default() } else { state };
            }
            Mode::Comment => {
                if let Some(open) = self.block_comment(0, 0, 1) {
                    return open;
                }
            }
            _ => {}
        }
        if self.i == 0 {
            let j = self.indent();
            if j <= 3 {
                let b = self.at(j);
                let mut k = j;
                while self.at(k) == b {
                    k += 1;
                }
                let run = k - j;
                // A fence: three backticks or tildes (a backtick fence's words have none).
                if matches!(b, b'`' | b'~')
                    && run >= 3
                    && (b == b'~' || !self.s[k..].contains(&b'`'))
                {
                    self.push(j, n, Kind::String);
                    self.i = n;
                    return State::new(
                        if b == b'`' {
                            Mode::FenceTick
                        } else {
                            Mode::FenceTilde
                        },
                        run.min(255) as u8,
                    );
                }
                if b == b'#' && run <= 6 && (k == n || is_blank(self.s[k])) {
                    self.push(j, n, Kind::Heading);
                    self.i = n;
                    return State::default();
                }
                let mut mark = 0;
                if b == b'>' {
                    mark = 1;
                } else if matches!(b, b'-' | b'*' | b'+') && is_blank(self.at(j + 1)) {
                    mark = 1;
                } else if b.is_ascii_digit() {
                    let mut d = j;
                    while self.at(d).is_ascii_digit() && d - j < 9 {
                        d += 1;
                    }
                    if matches!(self.at(d), b'.' | b')') && is_blank(self.at(d + 1)) {
                        mark = d + 1 - j;
                    }
                }
                if mark > 0 {
                    self.push(j, j + mark, Kind::Keyword);
                    self.i = j + mark;
                }
            }
        }
        // Inline code (`…`, ``…``) and HTML comments.
        let mut no_closer = [usize::MAX; 64];
        while self.i < n {
            let i = self.i;
            match self.s[i] {
                b'\\' => self.skip_escaped(i),
                b'`' => {
                    let mut k = i;
                    while self.at(k) == b'`' {
                        k += 1;
                    }
                    match self.backtick_closer(k, k - i, &mut no_closer) {
                        Some(end) => {
                            self.push(i, end, Kind::String);
                            self.i = end;
                        }
                        // No closing run: the backticks are only backticks.
                        None => self.i = k,
                    }
                }
                b'<' if self.starts(i, b"<!--") => {
                    if let Some(open) = self.block_comment(i, i + 4, 1) {
                        return open;
                    }
                }
                _ => self.i += 1,
            }
        }
        State::default()
    }

    /// The spaces before a Markdown line's text.
    fn indent(&self) -> usize {
        let mut j = 0;
        while self.at(j) == b' ' {
            j += 1;
        }
        j
    }

    /// The end of a run of exactly `run` backticks from `from`. `none_after[run]` remembers where
    /// a search for that length found none, so a line full of lone backticks costs one pass.
    fn backtick_closer(
        &self,
        from: usize,
        run: usize,
        none_after: &mut [usize; 64],
    ) -> Option<usize> {
        if run >= none_after.len() || none_after[run] <= from {
            return None;
        }
        let mut k = from;
        while k < self.len() {
            if self.s[k] == b'`' {
                let begin = k;
                while self.at(k) == b'`' {
                    k += 1;
                }
                if k - begin == run {
                    return Some(k);
                }
            } else {
                k += 1;
            }
        }
        none_after[run] = from;
        None
    }
}

/// What closes three quotes, and its escape (none in TOML's `'''`).
fn triple_close(lang: Language, quote: u8) -> (&'static [u8], u8) {
    match quote {
        b'\'' => (b"'''", if lang == Language::Toml { 0 } else { b'\\' }),
        _ => (b"\"\"\"", b'\\'),
    }
}

// ----- words -------------------------------------------------------------------------------------

#[rustfmt::skip]
const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "macro_rules", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "type",
    "union", "unsafe", "use", "where", "while", "yield",
];
#[rustfmt::skip]
const RUST_TYPES: &[&str] = &[
    "bool", "char", "str", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64",
    "u128", "usize", "f32", "f64",
];
#[rustfmt::skip]
const JS_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger",
    "default", "delete", "do", "else", "export", "extends", "false", "finally", "for", "from",
    "function", "if", "import", "in", "instanceof", "let", "new", "null", "of", "return", "static",
    "super", "switch", "this", "throw", "true", "try", "typeof", "undefined", "var", "void",
    "while", "with", "yield",
];
#[rustfmt::skip]
const TS_KEYWORDS: &[&str] = &[
    "abstract", "declare", "enum", "implements", "infer", "interface", "is", "keyof", "namespace",
    "private", "protected", "public", "readonly", "satisfies", "type",
];
#[rustfmt::skip]
const TS_TYPES: &[&str] = &[
    "any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "unknown",
];
#[rustfmt::skip]
const PY_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "self", "try",
    "while", "with", "yield",
];
#[rustfmt::skip]
const PY_TYPES: &[&str] = &[
    "bool", "bytearray", "bytes", "complex", "dict", "float", "frozenset", "int", "list", "object",
    "set", "str", "tuple", "type",
];
#[rustfmt::skip]
const SH_KEYWORDS: &[&str] = &[
    "alias", "break", "case", "continue", "declare", "do", "done", "elif", "else", "esac", "eval",
    "exec", "exit", "export", "false", "fi", "for", "function", "if", "in", "local", "readonly",
    "return", "select", "set", "shift", "source", "then", "time", "trap", "true", "typeset",
    "unset", "until", "while",
];
#[rustfmt::skip]
const PS_KEYWORDS: &[&str] = &[
    "begin", "break", "catch", "class", "continue", "data", "do", "dynamicparam", "else", "elseif",
    "end", "enum", "exit", "filter", "finally", "for", "foreach", "function", "hidden", "if", "in",
    "param", "process", "return", "static", "switch", "throw", "trap", "try", "until", "using",
    "while",
];
#[rustfmt::skip]
const PS_OPERATORS: &[&str] = &[
    "eq", "ne", "gt", "ge", "lt", "le", "like", "notlike", "match", "notmatch", "contains",
    "notcontains", "in", "notin", "replace", "split", "join", "and", "or", "not", "xor", "is",
    "isnot", "as", "f", "band", "bor", "bxor", "bnot", "shl", "shr",
];

fn is_in(table: &[&str], w: &[u8]) -> bool {
    table.iter().any(|k| k.as_bytes() == w)
}

/// A name in CamelCase, or one capital (`T`): a type, by the custom of Rust, TypeScript and
/// Python. `MAX_LEN` (no small letter) is a constant.
fn camel(w: &[u8]) -> bool {
    w.first().is_some_and(u8::is_ascii_uppercase)
        && (w.len() == 1 || w.iter().any(u8::is_ascii_lowercase))
}

fn classify(lang: Language, w: &[u8], after_dot: bool) -> Option<Kind> {
    let keyword = |table: &[&str]| !after_dot && is_in(table, w);
    match lang {
        Language::Rust if keyword(RUST_KEYWORDS) || (after_dot && w == b"await") => {
            Some(Kind::Keyword)
        }
        Language::Rust if is_in(RUST_TYPES, w) || camel(w) => Some(Kind::Type),
        Language::TypeScript if keyword(JS_KEYWORDS) || keyword(TS_KEYWORDS) => Some(Kind::Keyword),
        Language::TypeScript if (!after_dot && is_in(TS_TYPES, w)) || camel(w) => Some(Kind::Type),
        Language::JavaScript if keyword(JS_KEYWORDS) => Some(Kind::Keyword),
        Language::JavaScript if camel(w) => Some(Kind::Type),
        Language::Python if keyword(PY_KEYWORDS) => Some(Kind::Keyword),
        Language::Python if (!after_dot && is_in(PY_TYPES, w)) || camel(w) => Some(Kind::Type),
        Language::Json if matches!(w, b"true" | b"false" | b"null") => Some(Kind::Keyword),
        Language::Toml if matches!(w, b"true" | b"false") => Some(Kind::Keyword),
        Language::Toml if matches!(w, b"inf" | b"nan") => Some(Kind::Number),
        Language::Shell if is_in(SH_KEYWORDS, w) => Some(Kind::Keyword),
        Language::PowerShell
            if PS_KEYWORDS
                .iter()
                .any(|k| w.eq_ignore_ascii_case(k.as_bytes())) =>
        {
            Some(Kind::Keyword)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each coloured part of a text, line by line, as its text and kind.
    fn parts(lang: Language, text: &str) -> Vec<(String, Kind)> {
        let mut state = State::default();
        let mut out = Vec::new();
        for line in text.split('\n') {
            let (tokens, end) = scan_line(lang, state, line);
            out.extend(
                tokens
                    .into_iter()
                    .map(|t| (line[t.range].to_string(), t.kind)),
            );
            state = end;
        }
        out
    }

    fn expect(lang: Language, text: &str, want: &[(&str, Kind)]) {
        let got = parts(lang, text);
        let want: Vec<(String, Kind)> = want.iter().map(|(s, k)| (s.to_string(), *k)).collect();
        assert_eq!(got, want, "{lang:?}: {text}");
    }

    use Kind::{
        Comment as C, Heading as H, Key as K, Keyword as W, Number as N, String as S, Type as T,
    };

    #[test]
    fn rust_has_its_parts() {
        expect(
            Language::Rust,
            "/// Doc\nfn main() -> Result<u8, Error> {\n    let s = \"a \\\" b\"; // note\n    let c = '\\n'; let l: &'static str = r#\"raw \"q\"\"#;\n    x.await; 1_000u32 + 0xFF + 1.5e-3; 1..2\n}",
            &[
                ("/// Doc", C),
                ("fn", W),
                ("Result", T),
                ("u8", T),
                ("Error", T),
                ("let", W),
                ("\"a \\\" b\"", S),
                ("// note", C),
                ("let", W),
                ("'\\n'", S),
                ("let", W),
                ("str", T),
                ("r#\"raw \"q\"\"#", S),
                ("await", W),
                ("1_000u32", N),
                ("0xFF", N),
                ("1.5e-3", N),
                ("1", N),
                ("2", N),
            ],
        );
        // Block comments nest; a string goes on over lines; b'x' and b"x" are strings.
        expect(
            Language::Rust,
            "/* a /* b */ c */ x\nlet s = \"one\ntwo\"; b'x' b\"y\"",
            &[
                ("/* a /* b */ c */", C),
                ("let", W),
                ("\"one", S),
                ("two\"", S),
                ("b'x'", S),
                ("b\"y\"", S),
            ],
        );
    }

    #[test]
    fn typescript_and_javascript_have_their_parts() {
        expect(
            Language::TypeScript,
            "import { a } from \"b\";\ninterface Point { x: number; type: string }\nconst t = `line\n${x} end`; // c\nlet n = obj.type ?? 42n;",
            &[
                ("import", W),
                ("from", W),
                ("\"b\"", S),
                ("interface", W),
                ("Point", T),
                ("number", T),
                ("type", W),
                ("string", T),
                ("const", W),
                ("`line", S),
                ("${x} end`", S),
                ("// c", C),
                ("let", W),
                ("42n", N),
            ],
        );
        // JavaScript has no TypeScript words; "interface" is a name there.
        expect(
            Language::JavaScript,
            "let interface = 'x' /* c */ + $el",
            &[("let", W), ("'x'", S), ("/* c */", C)],
        );
    }

    #[test]
    fn json_toml_markdown_python_shell_and_powershell_have_their_parts() {
        expect(
            Language::Json,
            "{\n  \"name\": \"glasscode\", // c\n  \"n\" : -1.5e3, \"ok\": true, \"none\": null\n}",
            &[
                ("\"name\"", K),
                ("\"glasscode\"", S),
                ("// c", C),
                ("\"n\"", K),
                ("-1.5e3", N),
                ("\"ok\"", K),
                ("true", W),
                ("\"none\"", K),
                ("null", W),
            ],
        );
        expect(
            Language::Toml,
            "[package] # head\nname = \"glasscode\"\nversion.workspace = true\ndate = 1979-05-27T07:32:00Z\ntext = '''\nline\n'''\n[[bin]]\nlist = [\n  [1, 2],\n]",
            &[
                ("[package]", H),
                ("# head", C),
                ("name", K),
                ("\"glasscode\"", S),
                ("version.workspace", K),
                ("true", W),
                ("date", K),
                ("1979-05-27T07:32:00Z", N),
                ("text", K),
                ("'''", S),
                ("line", S),
                ("'''", S),
                ("[[bin]]", H),
                ("list", K),
                ("1", N),
                ("2", N),
            ],
        );
        expect(
            Language::Markdown,
            "# Title\n- item with `code` and ``a ` b``\n> quote <!-- hidden\nstill --> after\n```rust\nlet x = 1;\n```\n1. one `unclosed",
            &[
                ("# Title", H),
                ("-", W),
                ("`code`", S),
                ("``a ` b``", S),
                (">", W),
                ("<!-- hidden", C),
                ("still -->", C),
                ("```rust", S),
                ("let x = 1;", S),
                ("```", S),
                ("1.", W),
            ],
        );
        expect(
            Language::Python,
            "@dataclass\nclass Point(Base):\n    def f(self, x: int) -> str:  # c\n        s = f'{x}' + r\"\\d\" + \"\"\"doc\n        more\"\"\"\n        return 3.14",
            &[
                ("@dataclass", W),
                ("class", W),
                ("Point", T),
                ("Base", T),
                ("def", W),
                ("self", W),
                ("int", T),
                ("str", T),
                ("# c", C),
                ("f'{x}'", S),
                ("r\"\\d\"", S),
                ("\"\"\"doc", S),
                ("        more\"\"\"", S),
                ("return", W),
                ("3.14", N),
            ],
        );
        expect(
            Language::Shell,
            "#!/bin/sh\nif [ \"$x\" = 'a' ]; then echo $# ${#y} a#b # c\nfi\n./do --in 2>&1",
            &[
                ("#!/bin/sh", C),
                ("if", W),
                ("\"$x\"", S),
                ("'a'", S),
                ("then", W),
                ("# c", C),
                ("fi", W),
                ("2", N),
                ("1", N),
            ],
        );
        expect(
            Language::PowerShell,
            "<# help\n#>\nparam([string]$Path = $null)\nif ($a -eq 1) { Get-Item \"x`\"y\" } # c\n$h = @\"\nbody 'q'\n\"@",
            &[
                ("<# help", C),
                ("#>", C),
                ("param", W),
                ("string", T),
                ("$null", W),
                ("if", W),
                ("-eq", W),
                ("1", N),
                ("\"x`\"y\"", S),
                ("# c", C),
                ("@\"", S),
                ("body 'q'", S),
                ("\"@", S),
            ],
        );
    }

    #[test]
    fn a_string_that_is_not_closed_colours_to_the_line_end() {
        // Single-line strings end with their line; the next line is code again.
        for lang in [
            Language::TypeScript,
            Language::JavaScript,
            Language::Json,
            Language::Python,
            Language::Toml,
            Language::Shell,
            Language::PowerShell,
        ] {
            let (tokens, end) = scan_line(lang, State::default(), "x = \"abc");
            assert_eq!(
                tokens.last().map(|t| t.range.clone()),
                Some(4..8),
                "{lang:?}"
            );
            assert!(!end.is_open(), "{lang:?}");
        }
        // A Rust string goes on to the next line and to the end of the file if nothing closes it.
        let (tokens, end) = scan_line(Language::Rust, State::default(), "let s = \"abc");
        assert_eq!(
            tokens.last().map(|t| (t.range.clone(), t.kind)),
            Some((8..12, S))
        );
        assert!(end.is_open());
        let (tokens, end) = scan_line(Language::Rust, end, "fn x() {}");
        assert_eq!(
            tokens,
            vec![Token {
                range: 0..9,
                kind: S
            }]
        );
        assert!(end.is_open());
        // A lone quote of a lifetime is no string.
        assert_eq!(
            parts(Language::Rust, "fn f<'a>(x: &'a str)"),
            vec![("fn".into(), W), ("str".into(), T)]
        );
    }

    #[test]
    fn an_unknown_language_has_no_colours() {
        for name in [
            "notes.txt",
            "Makefile",
            ".gitignore",
            "image.png",
            "archive.tar.gz",
            "README",
            ".env",
            "a.json5",
        ] {
            assert_eq!(Language::of(name), None, "{name}");
        }
        let known = [
            ("src/main.rs", Language::Rust),
            ("app.tsx", Language::TypeScript),
            ("a.mts", Language::TypeScript),
            ("x.mjs", Language::JavaScript),
            ("x.cjs", Language::JavaScript),
            ("C:\\w\\App.JSX", Language::JavaScript),
            ("tsconfig.json", Language::Json),
            (".vscode/settings.jsonc", Language::Json),
            ("Cargo.toml", Language::Toml),
            ("Cargo.lock", Language::Toml),
            ("README.md", Language::Markdown),
            ("a.pyi", Language::Python),
            ("run.sh", Language::Shell),
            ("x.zsh", Language::Shell),
            (".bashrc", Language::Shell),
            ("build.ps1", Language::PowerShell),
            ("m.psm1", Language::PowerShell),
        ];
        for (name, lang) in known {
            assert_eq!(Language::of(name), Some(lang), "{name}");
        }
    }

    /// A small, fixed sequence of numbers, so the "fuzz" inputs are the same on each run.
    struct Dice(u64);

    impl Dice {
        fn next(&mut self, below: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % below as u64) as usize
        }
    }

    const PIECES: &[&str] = &[
        "\"", "'", "`", "```", "~~~", "/*", "*/", "//", "#", "<#", "#>", "<!--", "-->", "\\",
        "\\\"", "r#\"", "\"#", "b'", "'''", "\"\"\"", "@\"", "\"@", "${", "}", "$", "-", "-eq",
        "[", "]", "=", ":", ".", "..", "0x", "1.5e-3", "42", "fn", "let", "if", "type", "class",
        "Point", "é", "日本", "👍🏽", "🇩🇪", "\u{301}", "\t", " ", "  ", "\r", "a", "_x", "x1", "> ",
        "- ", "1. ", "# ", "@dec", "[t]", "\u{FFFD}", "…",
    ];

    /// Random lines from risky pieces, in each language and from each state a line can start in:
    /// every range is sorted, apart, inside the line and on character boundaries (also before
    /// the last guard of `scan_line`), and the state alone is the state of the full scan.
    #[test]
    fn ranges_stay_on_character_boundaries_whatever_the_line() {
        let mut dice = Dice(0x9E37_79B9_7F4A_7C15);
        for lang in Language::ALL {
            let mut starts = vec![State::default()];
            for round in 0..1500 {
                let pieces = 1 + dice.next(24);
                let line: String = (0..pieces)
                    .map(|_| PIECES[dice.next(PIECES.len())])
                    .collect();
                let start = starts[dice.next(starts.len())];
                let mut raw = Vec::new();
                let end = run(lang, start, &line, Some(&mut raw));
                let mut last = 0;
                for t in &raw {
                    assert!(
                        last <= t.range.start
                            && t.range.start < t.range.end
                            && t.range.end <= line.len(),
                        "{lang:?} {line:?} {raw:?}"
                    );
                    assert!(
                        line.is_char_boundary(t.range.start) && line.is_char_boundary(t.range.end),
                        "{lang:?} {line:?} {raw:?}"
                    );
                    last = t.range.end;
                }
                let (tokens, end_too) = scan_line(lang, start, &line);
                assert_eq!(
                    tokens, raw,
                    "the guard leaves nothing out: {lang:?} {line:?}"
                );
                assert_eq!(end, end_too);
                assert_eq!(end_state(lang, start, &line), end, "{lang:?} {line:?}");
                if end.is_open() && !starts.contains(&end) && round % 3 == 0 {
                    starts.push(end);
                }
            }
        }
    }

    /// A line of a megabyte (a minified file) and a line cut in the middle of a character's
    /// bytes elsewhere never panic, and cost one pass.
    #[test]
    fn very_long_lines_are_scanned_once() {
        let line = "a = \"x\"; /* c */ `t` 'q' 12 # z ".repeat(32_768);
        let begun = std::time::Instant::now();
        for lang in Language::ALL {
            let (tokens, _) = scan_line(lang, State::default(), &line);
            assert!(!tokens.is_empty() || lang == Language::Markdown, "{lang:?}");
        }
        let lone = "`".repeat(5000) + &" x `".repeat(5000);
        let _ = scan_line(Language::Markdown, State::default(), &lone);
        let elapsed = begun.elapsed();
        eprintln!(
            "nine languages over a line of {} bytes, and a line of lone backticks: {elapsed:?}",
            line.len()
        );
        assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
    }

    fn lines_of(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_string).collect()
    }

    fn starts_of(lang: Language, lines: &[String]) -> Starts {
        Starts::new(lang, lines.iter().map(String::as_str))
    }

    /// Replace lines `first..first + removed` with `new`, in the lines and in their states.
    fn edit(
        starts: &mut Starts,
        lines: &mut Vec<String>,
        first: usize,
        removed: usize,
        new: &[&str],
    ) -> usize {
        lines.splice(first..first + removed, new.iter().map(|s| s.to_string()));
        let snapshot = lines.clone();
        let scanned = starts.edited(first, removed, new.len(), |i| snapshot[i].as_str());
        assert_eq!(
            *starts,
            starts_of(starts.language(), lines),
            "the same as a full scan after editing line {first}"
        );
        scanned
    }

    #[test]
    fn opening_a_block_comment_recolours_the_lines_below_and_closing_it_restores_them() {
        let text = "fn main() {\n    let a = 1;\n    let b = \"two\";\n    call(a, b);\n}\n// end";
        let mut lines = lines_of(text);
        let mut starts = starts_of(Language::Rust, &lines);
        let before = starts.clone();
        assert!((0..lines.len()).all(|i| !starts.at(i).is_open()));
        let scanned = edit(&mut starts, &mut lines, 1, 1, &["    /* let a = 1;"]);
        assert_eq!(scanned, lines.len() - 1, "every line below");
        assert!((2..lines.len()).all(|i| starts.at(i).is_open()));
        assert_eq!(
            starts.line(3, &lines[3]),
            vec![Token {
                range: 0..lines[3].len(),
                kind: C
            }],
            "a line below is all comment"
        );
        // Closed on the next line: the lines below it are code again.
        let scanned = edit(&mut starts, &mut lines, 2, 1, &["    let b = \"two\"; */"]);
        assert_eq!(
            scanned,
            lines.len() - 2,
            "the edited line and every line below, which were comment"
        );
        assert!((3..lines.len()).all(|i| !starts.at(i).is_open()));
        // Taking the `/*` away again: only the line it closed changes below it.
        let scanned = edit(&mut starts, &mut lines, 1, 1, &["    let a = 1;"]);
        assert_eq!(scanned, 2, "line 3 starts as before");
        let scanned = edit(&mut starts, &mut lines, 2, 1, &["    let b = \"two\";"]);
        assert_eq!(scanned, 1);
        assert_eq!(starts, before, "the states of before");
    }

    #[test]
    fn the_work_after_a_letter_is_one_line() {
        let text = "fn main() {\n    let a = 1;\n    /* note\n    still */\n    let b = 2;\n}";
        let mut lines = lines_of(text);
        let mut starts = starts_of(Language::Rust, &lines);
        assert_eq!(edit(&mut starts, &mut lines, 1, 1, &["    let ab = 1;"]), 1);
        assert_eq!(
            edit(&mut starts, &mut lines, 3, 1, &["    still x */"]),
            1,
            "inside a comment"
        );
        assert_eq!(edit(&mut starts, &mut lines, 2, 1, &["    /* notes"]), 1);
        // A new line (one line becomes two) scans the two.
        assert_eq!(
            edit(&mut starts, &mut lines, 1, 1, &["    let ab", " = 1;"]),
            2
        );
        // Two lines joined: one.
        assert_eq!(edit(&mut starts, &mut lines, 1, 2, &["    let ab = 1;"]), 1);
    }

    /// After each step of a sequence of edits (a paste of several lines, a join, a deletion,
    /// and back), the states kept are those a full scan gives (checked inside `edit`).
    #[test]
    fn the_states_kept_are_those_of_a_full_scan() {
        for (lang, text) in [
            (
                Language::Rust,
                "fn a() {\n    let s = r#\"x\n\"#;\n}\n/* c\n*/",
            ),
            (
                Language::Python,
                "def f():\n    '''doc\n    more'''\n    return 1",
            ),
            (Language::Markdown, "# T\n```\ncode\n```\n<!-- c\n-->\ntext"),
            (
                Language::TypeScript,
                "const a = `x\ny`;\n/* c */\nlet b = 1;",
            ),
            (Language::PowerShell, "<# a\n#>\n$x = @'\nbody\n'@\n1"),
            (Language::Toml, "a = \"\"\"\nx\n\"\"\"\n[t]\nb = 1"),
        ] {
            let mut lines = lines_of(text);
            let mut starts = starts_of(lang, &lines);
            let n = lines.len();
            edit(
                &mut starts,
                &mut lines,
                1,
                1,
                &["/* paste", "\"\"\" ``` <# '''", "`more"],
            );
            edit(&mut starts, &mut lines, 0, 2, &["joined"]);
            edit(&mut starts, &mut lines, 2, 2, &[]);
            let last = lines.len() - 1;
            edit(
                &mut starts,
                &mut lines,
                last,
                1,
                &["*/ \"\"\" ``` #> ''' `", "end"],
            );
            let all = lines.len();
            edit(
                &mut starts,
                &mut lines,
                0,
                all,
                &text.split('\n').collect::<Vec<_>>(),
            );
            assert_eq!(lines.len(), n);
        }
    }

    /// The measurement for the pull request: a file of 2 MB scanned once, then one letter typed
    /// in its middle and a `/*` that opens a comment on its first line (every line below).
    #[test]
    fn a_2_mb_file_is_scanned_fast_and_a_letter_rescans_one_line() {
        let block = "/// A function.\nfn compute(alpha: u32, beta: &str) -> Result<String, Error> {\n    let text = format!(\"{alpha} and {beta}\"); // note\n    Ok(text)\n}\n";
        let text = block.repeat(2 * 1024 * 1024 / block.len());
        let mut lines = lines_of(&text);
        let begun = std::time::Instant::now();
        let mut starts = starts_of(Language::Rust, &lines);
        let scanned = begun.elapsed();
        let middle = lines.len() / 2 + 2;
        let mut typed = lines[middle].clone();
        typed.insert(8, 'x');
        lines[middle] = typed;
        let begun = std::time::Instant::now();
        let letter_lines = starts.edited(middle, 1, 1, |i| lines[i].as_str());
        let letter = begun.elapsed();
        let begun = std::time::Instant::now();
        let colours: usize = (middle..middle + 40)
            .map(|i| starts.line(i, &lines[i]).len())
            .sum();
        let screen = begun.elapsed();
        lines[0].insert_str(0, "/*");
        let begun = std::time::Instant::now();
        let comment_lines = starts.edited(0, 1, 1, |i| lines[i].as_str());
        let comment = begun.elapsed();
        eprintln!(
            "{} bytes, {} lines: full scan {scanned:?}; a letter {letter:?} ({letter_lines} line); 40 lines on screen coloured {screen:?} ({colours} ranges); a /* on line 1 {comment:?} ({comment_lines} lines)",
            text.len(),
            lines.len()
        );
        assert_eq!(letter_lines, 1);
        assert_eq!(comment_lines, lines.len());
        assert!(scanned < std::time::Duration::from_secs(2), "{scanned:?}");
        assert!(letter < std::time::Duration::from_millis(5), "{letter:?}");
    }
}
