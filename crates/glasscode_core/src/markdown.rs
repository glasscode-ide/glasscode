//! Just enough Markdown to show an agent's notes or a ticket as a page: headings,
//! paragraphs, lists and checklists, code, quotes, tables, and **bold**, *italic*, `code` and
//! [links](https://…) inside a line.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// Where a link goes.
    pub link: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading { level: u8, text: Vec<Span> },
    Paragraph(Vec<Span>),
    /// A list entry; `number` for "1." lists, `depth` 0 at the left edge.
    Item { number: Option<u32>, depth: u8, text: Vec<Span> },
    /// A checklist entry ("- [x] done").
    Check { done: bool, depth: u8, text: Vec<Span> },
    Code(String),
    Quote(Vec<Span>),
    /// Rows of cells; the first row is the header.
    Table(Vec<Vec<Vec<Span>>>),
    Rule,
}

/// "- x", "* x", "+ x", "1. x", "1) x" → (number, text).
fn list_marker(line: &str) -> Option<(Option<u32>, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            return Some((None, rest));
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = &line[digits..];
    let rest = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))?;
    Some((line[..digits].parse().ok(), rest))
}

fn is_rule(line: &str) -> bool {
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3 && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '_'))
}

pub fn parse(text: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut code: Option<Vec<String>> = None;
    let mut in_item = false;
    let flush = |paragraph: &mut Vec<String>, blocks: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(spans(&paragraph.join(" "))));
            paragraph.clear();
        }
    };
    for raw in text.lines() {
        if let Some(lines) = code.as_mut() {
            if raw.trim_start().starts_with("```") {
                blocks.push(Block::Code(lines.join("\n")));
                code = None;
            } else {
                lines.push(raw.to_string());
            }
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if line.starts_with("```") {
            flush(&mut paragraph, &mut blocks);
            code = Some(Vec::new());
            in_item = false;
            continue;
        }
        if line.is_empty() {
            flush(&mut paragraph, &mut blocks);
            in_item = false;
            continue;
        }
        if let Some(hashes) = line.strip_prefix('#').map(|_| line.chars().take_while(|&c| c == '#').count()) {
            if hashes <= 6 && line[hashes..].starts_with(' ') {
                flush(&mut paragraph, &mut blocks);
                blocks.push(Block::Heading { level: hashes as u8, text: spans(line[hashes..].trim()) });
                in_item = false;
                continue;
            }
        }
        if is_rule(line) && paragraph.is_empty() {
            blocks.push(Block::Rule);
            in_item = false;
            continue;
        }
        if let Some((number, rest)) = list_marker(line) {
            flush(&mut paragraph, &mut blocks);
            let depth = (indent / 2).min(3) as u8;
            let check = rest.strip_prefix("[ ] ").map(|r| (false, r)).or_else(|| rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")).map(|r| (true, r)));
            blocks.push(match check {
                Some((done, rest)) if number.is_none() => Block::Check { done, depth, text: spans(rest) },
                _ => Block::Item { number, depth, text: spans(rest) },
            });
            in_item = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix('>') {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Quote(spans(rest.trim())));
            in_item = false;
            continue;
        }
        if line.starts_with('|') {
            flush(&mut paragraph, &mut blocks);
            let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
            let separator = cells.iter().all(|c| !c.is_empty() && c.trim_matches(':').chars().all(|ch| ch == '-'));
            if !separator {
                let row: Vec<Vec<Span>> = cells.iter().map(|c| spans(c)).collect();
                match blocks.last_mut() {
                    Some(Block::Table(rows)) => rows.push(row),
                    _ => blocks.push(Block::Table(vec![row])),
                }
            }
            in_item = false;
            continue;
        }
        // A wrapped line of the list entry above it.
        if in_item && indent > 0 {
            if let Some(Block::Item { text, .. } | Block::Check { text, .. }) = blocks.last_mut() {
                text.push(Span { text: " ".into(), ..Default::default() });
                text.extend(spans(line));
                continue;
            }
        }
        in_item = false;
        paragraph.push(line.to_string());
    }
    if let Some(lines) = code {
        blocks.push(Block::Code(lines.join("\n")));
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

/// `**bold**`, `*italic*` / `_italic_`, `` `code` ``, `[text](link)` and `![image](link)`.
pub fn spans(text: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut current = String::new();
    let (mut bold, mut italic) = (false, false);
    let chars: Vec<char> = text.chars().collect();
    let push = |out: &mut Vec<Span>, current: &mut String, bold: bool, italic: bool, code: bool| {
        if !current.is_empty() {
            out.push(Span { text: std::mem::take(current), bold, italic, code, link: None });
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|&x| x == '`') {
                push(&mut out, &mut current, bold, italic, false);
                let code: String = chars[i + 1..i + 1 + end].iter().collect();
                out.push(Span { text: code, bold, italic, code: true, link: None });
                i += end + 2;
                continue;
            }
        }
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            push(&mut out, &mut current, bold, italic, false);
            bold = !bold;
            i += 2;
            continue;
        }
        let word_edge = |j: usize| j == 0 || !chars[j - 1].is_alphanumeric();
        if (c == '*' || c == '_') && (italic || (word_edge(i) && chars.get(i + 1).is_some_and(|n| !n.is_whitespace()))) {
            // An underscore inside a word (snake_case) stays as it is.
            let closes = italic && (i + 1 == chars.len() || !chars[i + 1].is_alphanumeric());
            if !italic || closes {
                push(&mut out, &mut current, bold, italic, false);
                italic = !italic;
                i += 1;
                continue;
            }
        }
        let image = c == '!' && chars.get(i + 1) == Some(&'[');
        if c == '[' || image {
            let open = if image { i + 1 } else { i };
            if let Some(close) = chars[open + 1..].iter().position(|&x| x == ']') {
                let after = open + 1 + close + 1;
                if chars.get(after) == Some(&'(') {
                    if let Some(end) = chars[after..].iter().position(|&x| x == ')') {
                        let label: String = chars[open + 1..open + 1 + close].iter().collect();
                        let target: String = chars[after + 1..after + end].iter().collect();
                        let target = target.trim().trim_start_matches('<').trim_end_matches('>').to_string();
                        let strong = label.starts_with("**") && label.ends_with("**") && label.len() > 4;
                        let label = label.replace("**", "");
                        // Linear writes a bare "Acme" as [Acme](http://Acme): that is text.
                        let autolink = target == format!("http://{label}") || target == format!("https://{label}");
                        if autolink || target.is_empty() {
                            push(&mut out, &mut current, bold, italic, false);
                            out.push(Span { text: label, bold: bold || strong, italic, code: false, link: None });
                        } else {
                            push(&mut out, &mut current, bold, italic, false);
                            let text = if image { format!("Image: {}", if label.is_empty() { "open" } else { label.as_str() }) } else { label };
                            out.push(Span { text, bold: bold || strong, italic, code: false, link: Some(target) });
                        }
                        i = after + end + 1;
                        continue;
                    }
                }
            }
        }
        current.push(c);
        i += 1;
    }
    push(&mut out, &mut current, bold, italic, false);
    out
}

/// The plain text of some spans.
pub fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_overview() {
        let md = "## What it does\nA child group takes its\nparent's role.\n\n## Worth a close look\n- **Backfill locks** — holds a lock (`migrations/0042.sql:12`)\n  on big orgs\n1. first\n2) second\n\n```\ncode here\n```\n---\n| a | b |\n|---|---|\n";
        let blocks = parse(md);
        assert_eq!(blocks[0], Block::Heading { level: 2, text: vec![Span { text: "What it does".into(), ..Default::default() }] });
        assert_eq!(plain(match &blocks[1] { Block::Paragraph(s) => s, _ => panic!() }), "A child group takes its parent's role.");
        let Block::Item { number: None, text, .. } = &blocks[3] else { panic!("{:?}", blocks[3]) };
        assert_eq!(plain(text), "Backfill locks — holds a lock (migrations/0042.sql:12) on big orgs");
        assert!(text[0].bold && text.iter().any(|s| s.code && s.text == "migrations/0042.sql:12"));
        assert!(matches!(blocks[4], Block::Item { number: Some(1), .. }));
        assert!(matches!(blocks[5], Block::Item { number: Some(2), .. }));
        assert_eq!(blocks[6], Block::Code("code here".into()));
        assert_eq!(blocks[7], Block::Rule);
        assert_eq!(blocks[8], Block::Table(vec![vec![spans("a"), spans("b")]]));
    }

    #[test]
    fn reads_tickets_markdown() {
        let md = "- [X] Agent picker\n- [ ] Not yet\n\n| Candidate | Priority |\n| -- | -- |\n| **To Do** | higher |\n\nSee [ENG-639](https://linear.app/x) and [**Acme**](<http://Acme>), ![01](https://u/1).";
        let blocks = parse(md);
        assert!(matches!(&blocks[0], Block::Check { done: true, .. }));
        assert!(matches!(&blocks[1], Block::Check { done: false, .. }));
        let Block::Table(rows) = &blocks[2] else { panic!("{:?}", blocks[2]) };
        assert_eq!(rows.len(), 2);
        assert!(rows[1][0][0].bold && plain(&rows[1][0]) == "To Do");
        let Block::Paragraph(text) = &blocks[3] else { panic!() };
        let link = text.iter().find(|s| s.text == "ENG-639").unwrap();
        assert_eq!(link.link.as_deref(), Some("https://linear.app/x"));
        let brand = text.iter().find(|s| s.text == "Acme").unwrap();
        assert!(brand.bold && brand.link.is_none());
        assert!(text.iter().any(|s| s.text == "Image: 01" && s.link.as_deref() == Some("https://u/1")));
    }

    #[test]
    fn keeps_snake_case_and_links() {
        assert_eq!(plain(&spans("see parent_id in [the docs](https://x.y)")), "see parent_id in the docs");
        let s = spans("an *italic* word");
        assert!(s.iter().any(|x| x.italic && x.text == "italic"));
        assert!(spans("a ** b").iter().all(|x| !x.italic));
    }
}
