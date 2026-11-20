//! The retrieval set (land-14): a synthetic memory home and 214 questions, each naming the one
//! section that answers it, scored as recall at 5.
//!
//! `fixtures/retrieval/memory/` is the home, in the layout and entry format of M20.1 (`you/`,
//! `org/<employer>/`, `projects/<key>/` with `PROJECT.md`, `decisions.md`, `knowhow/`, `work/` and
//! `archive/`): six projects of two orgs, two pairs of projects with near names (`app` and
//! `app-web`, `billing` and `billing-api`) and the same headings in both, work files of renamed
//! branches, and archived work. `questions.jsonl` has the questions, by kind: `direct`, `synonym`
//! (answered through the org's `glossary.md`), `identifier` (through a name such as
//! `retryBackoffMs` split into its words), `near_duplicate`, `renamed` (asked by a branch's old
//! name) and `archive`.
//!
//! The reference search here is BM25 over heading sections, plain and with identifiers split and
//! glossary synonyms: the two numbers land-14 asks for. `floor.json` keeps them from their first
//! run, and this test fails below them. M20.2's real search gets its own floor from its first
//! run (`retrieval_recall_at_5_floor`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::fixtures;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub id: String,
    pub question: String,
    /// `<file>#<heading>`.
    pub expect: String,
    pub kind: String,
}

pub fn memory() -> PathBuf {
    fixtures().join("retrieval").join("memory")
}

pub fn questions() -> Vec<Question> {
    crate::read_jsonl(&fixtures().join("retrieval").join("questions.jsonl"))
}

/// A heading section of a memory file.
#[derive(Debug, Clone)]
pub struct Section {
    /// `<file>#<heading>`, the file relative to the home.
    pub id: String,
    pub file: String,
    pub heading: String,
    /// The entries, without their headers.
    pub body: String,
}

fn files(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            files(&path, root, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

fn without_comments(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(open) = rest.find("<!--") {
        out.push_str(&rest[..open]);
        rest = rest[open..].find("-->").map_or("", |close| &rest[open + close + 3..]);
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Every heading section of the home, in the order of its files.
pub fn sections(root: &Path) -> Vec<Section> {
    let mut paths = Vec::new();
    files(root, root, &mut paths);
    let mut out = Vec::new();
    for path in paths {
        let file = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(&path).unwrap();
        let mut current: Option<Section> = None;
        for line in text.lines() {
            if let Some(heading) = line.strip_prefix("## ") {
                out.extend(current.take());
                current = Some(Section { id: format!("{file}#{heading}"), file: file.clone(), heading: heading.to_string(), body: String::new() });
            } else if let Some(section) = current.as_mut() {
                let line = without_comments(line);
                if !line.is_empty() {
                    section.body.push_str(line.trim_start_matches("- "));
                    section.body.push('\n');
                }
            }
        }
        out.extend(current.take());
    }
    out
}

const STOP: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "to", "of", "in", "on", "for", "and", "or", "do", "does", "did", "i", "we", "it", "its", "this", "that", "what", "which", "who", "whose",
    "when", "where", "how", "why", "can", "may", "should", "must", "there", "with", "by", "at", "from", "as", "after", "before", "about", "any", "my", "our", "me", "us", "you", "your", "not",
    "no", "into", "than", "then", "so", "if", "each", "every", "all", "has", "have", "had", "will", "would", "get", "got", "one", "now",
];

/// One word as the index keeps it: lower case, a plural `s` and a `'s` taken off.
fn normal(word: &str) -> String {
    let mut word = word.to_lowercase();
    if let Some(stem) = word.strip_suffix("'s") {
        word = stem.to_string();
    }
    if word.chars().count() > 4 && word.ends_with('s') && !word.ends_with("ss") {
        word.pop();
    }
    word
}

fn keep(word: String) -> Option<String> {
    (!word.is_empty() && !STOP.contains(&word.as_str())).then_some(word)
}

/// Plain BM25's words: runs of letters, digits and `_`.
pub fn plain(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter_map(|w| keep(normal(w))).collect()
}

/// The words of an identifier or a path: at `_`, `-`, `/`, `.` and where camelCase turns.
fn pieces(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            out.push(std::mem::take(&mut current));
            continue;
        }
        let turn = i > 0 && c.is_uppercase() && (chars[i - 1].is_lowercase() || chars.get(i + 1).is_some_and(|n| n.is_lowercase()) && chars[i - 1].is_uppercase());
        if turn {
            out.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    out.push(current);
    out.into_iter().filter(|p| !p.is_empty()).collect()
}

/// BM25's words with identifiers split: the plain words, and the pieces of each identifier.
pub fn split(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in text.split(|c: char| !(c.is_alphanumeric() || "_-./".contains(c))) {
        let whole = plain(chunk);
        let parts: Vec<String> = pieces(chunk).iter().filter_map(|p| keep(normal(p))).collect();
        let more = parts.len() > 1 || whole.len() != 1;
        out.extend(whole.iter().cloned());
        if more {
            out.extend(parts.into_iter().filter(|p| !whole.contains(p)));
        }
    }
    out
}

/// The synonym groups of every `glossary.md` (`- term: a, b`), as the words of each phrase.
pub fn glossary(sections: &[Section]) -> Vec<Vec<Vec<String>>> {
    let mut groups = Vec::new();
    for section in sections.iter().filter(|s| s.file.ends_with("glossary.md")) {
        for line in section.body.lines() {
            let Some((term, rest)) = line.split_once(':') else { continue };
            let group: Vec<Vec<String>> = std::iter::once(term).chain(rest.split(',')).map(split).filter(|p| !p.is_empty()).collect();
            if group.len() > 1 {
                groups.push(group);
            }
        }
    }
    groups
}

fn contains(words: &[String], phrase: &[String]) -> bool {
    !phrase.is_empty() && words.windows(phrase.len()).any(|w| w == phrase)
}

/// The query: its words at weight 1, and the words of a glossary phrase's synonyms at weight ½.
pub fn query(text: &str, tokens: fn(&str) -> Vec<String>, synonyms: &[Vec<Vec<String>>]) -> Vec<(String, f64)> {
    let words = tokens(text);
    let mut out: Vec<(String, f64)> = words.iter().map(|w| (w.clone(), 1.0)).collect();
    for group in synonyms {
        if group.iter().any(|phrase| contains(&words, phrase)) {
            for word in group.iter().flatten() {
                if !out.iter().any(|(w, _)| w == word) {
                    out.push((word.clone(), 0.5));
                }
            }
        }
    }
    out
}

/// BM25 (k1 = 1.2, b = 0.75) over sections: a section is its file's path, its heading and its body.
pub struct Index {
    terms: Vec<HashMap<String, f64>>,
    lengths: Vec<f64>,
    df: HashMap<String, f64>,
    average: f64,
}

impl Index {
    pub fn new(sections: &[Section], tokens: fn(&str) -> Vec<String>) -> Index {
        let mut terms = Vec::new();
        let mut lengths = Vec::new();
        let mut df: HashMap<String, f64> = HashMap::new();
        for section in sections {
            let words = tokens(&format!("{} {} {}", section.file.trim_end_matches(".md"), section.heading, section.body));
            let mut tf: HashMap<String, f64> = HashMap::new();
            for word in &words {
                *tf.entry(word.clone()).or_insert(0.0) += 1.0;
            }
            for word in tf.keys() {
                *df.entry(word.clone()).or_insert(0.0) += 1.0;
            }
            lengths.push(words.len() as f64);
            terms.push(tf);
        }
        let average = lengths.iter().sum::<f64>() / lengths.len().max(1) as f64;
        Index { terms, lengths, df, average }
    }

    /// Section numbers, best first; equal scores in the order of the sections.
    pub fn search(&self, query: &[(String, f64)]) -> Vec<usize> {
        let n = self.terms.len() as f64;
        let mut scored: Vec<(f64, usize)> = (0..self.terms.len())
            .map(|i| {
                let score = query
                    .iter()
                    .map(|(word, weight)| {
                        let Some(tf) = self.terms[i].get(word) else { return 0.0 };
                        let df = self.df[word];
                        let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                        weight * idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * self.lengths[i] / self.average))
                    })
                    .sum::<f64>();
                (score, i)
            })
            .filter(|(score, _)| *score > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| i).collect()
    }
}

/// Recall at `k`, over all questions and by kind.
pub fn recall_at(k: usize, questions: &[Question], answer: impl Fn(&str) -> Vec<String>) -> (f64, BTreeMap<String, (usize, usize)>, Vec<String>) {
    let mut kinds: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut hits = 0;
    let mut missed = Vec::new();
    for question in questions {
        let found = answer(&question.question).iter().take(k).any(|id| *id == question.expect);
        hits += usize::from(found);
        if !found {
            missed.push(question.id.clone());
        }
        let entry = kinds.entry(question.kind.clone()).or_insert((0, 0));
        entry.0 += usize::from(found);
        entry.1 += 1;
    }
    (hits as f64 / questions.len() as f64, kinds, missed)
}

const KINDS: [&str; 6] = ["direct", "synonym", "identifier", "near_duplicate", "renamed", "archive"];

#[test]
fn the_set_has_two_hundred_questions_over_one_home() {
    let sections = sections(&memory());
    let questions = questions();
    assert!(questions.len() >= 200, "{}", questions.len());
    assert_eq!(questions.iter().map(|q| q.id.as_str()).collect::<BTreeSet<_>>().len(), questions.len(), "ids are unique");
    assert_eq!(questions.iter().map(|q| q.question.to_lowercase()).collect::<BTreeSet<_>>().len(), questions.len(), "questions are unique");
    let ids: BTreeSet<&str> = sections.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids.len(), sections.len(), "a heading once per file");
    for question in &questions {
        assert!(ids.contains(question.expect.as_str()), "{}: no section {}", question.id, question.expect);
        assert!(KINDS.contains(&question.kind.as_str()), "{}: {}", question.id, question.kind);
    }
    assert_eq!(questions.iter().map(|q| q.kind.as_str()).collect::<BTreeSet<_>>(), KINDS.into_iter().collect());
    let answered: BTreeSet<&str> = questions.iter().map(|q| q.expect.as_str()).collect();
    assert_eq!(answered.len(), sections.len(), "every section answers a question");
    // The home: several projects of two orgs, near names with the same headings, archived work,
    // renamed branches, and entries in M20.1's format.
    let projects: BTreeSet<&str> = sections.iter().filter_map(|s| s.file.strip_prefix("projects/")).map(|f| f.split('/').next().unwrap()).collect();
    assert!(projects.len() >= 6 && projects.contains("app-web-8b2d10") && projects.contains("billing-api-9d0f44"), "{projects:?}");
    assert!(sections.iter().filter(|s| s.file.starts_with("org/")).map(|s| s.file.split('/').nth(1).unwrap()).collect::<BTreeSet<_>>().len() >= 2);
    let mut headings: BTreeMap<&str, usize> = BTreeMap::new();
    for section in sections.iter().filter(|s| s.file.starts_with("projects/")) {
        *headings.entry(section.heading.as_str()).or_insert(0) += 1;
    }
    assert!(headings.get("Retry with backoff") == Some(&2), "a heading in two projects of near names");
    for question in questions.iter().filter(|q| q.kind == "archive") {
        assert!(question.expect.contains("/archive/"), "{}", question.id);
    }
    for question in questions.iter().filter(|q| q.kind == "renamed") {
        let section = sections.iter().find(|s| s.id == question.expect).unwrap();
        let old = section.body.lines().find_map(|l| l.strip_prefix("Renamed from ")).unwrap().trim_end_matches('.');
        assert!(question.question.contains(old) && !section.file.contains(&old.replace('/', "-")), "{}: asked by the old name {old}", question.id);
    }
    let text = std::fs::read_to_string(memory().join("projects/app-3f9a1c/decisions.md")).unwrap();
    assert!(text.lines().filter(|l| l.starts_with("- ")).all(|l| l.contains("<!-- id=") && l.contains("status=") && l.ends_with("-->")), "M20.1's entry format");
}

/// The reference numbers: recall at 5 of plain BM25, and of BM25 with identifiers split and
/// glossary synonyms. They must stay at or above their floor; the second must beat the first on
/// the questions it exists for.
#[test]
fn reference_search_recall_at_5() {
    let sections = sections(&memory());
    let questions = questions();
    let synonyms = glossary(&sections);
    assert!(synonyms.len() >= 9, "{}", synonyms.len());
    let ids: Vec<String> = sections.iter().map(|s| s.id.clone()).collect();
    let plain_index = Index::new(&sections, plain);
    let split_index = Index::new(&sections, split);
    let (plain_recall, plain_kinds, plain_missed) = recall_at(5, &questions, |q| plain_index.search(&query(q, plain, &[])).into_iter().map(|i| ids[i].clone()).collect());
    let (split_recall, split_kinds, split_missed) = recall_at(5, &questions, |q| split_index.search(&query(q, split, &synonyms)).into_iter().map(|i| ids[i].clone()).collect());
    println!("recall at 5 over {} questions and {} sections", questions.len(), sections.len());
    println!("{:<16} {:>12} {:>22}", "kind", "plain BM25", "split and synonyms");
    for kind in KINDS {
        let (a, n) = plain_kinds[kind];
        let (b, _) = split_kinds[kind];
        println!("{kind:<16} {:>12} {:>22}", format!("{a}/{n}"), format!("{b}/{n}"));
    }
    println!("{:<16} {:>12.3} {:>22.3}", "all", plain_recall, split_recall);
    println!("missed by plain BM25: {}", plain_missed.join(" "));
    println!("missed with split and synonyms: {}", split_missed.join(" "));
    let floor: serde_json::Value = serde_json::from_slice(&std::fs::read(fixtures().join("retrieval").join("floor.json")).unwrap()).unwrap();
    let at_least = |key: &str| floor[key].as_f64().unwrap_or_else(|| panic!("floor.json has no {key}"));
    assert!(plain_recall >= at_least("reference_plain_bm25") - 1e-9, "plain BM25 fell below its floor: {plain_recall:.3}");
    assert!(split_recall >= at_least("reference_split_and_synonyms") - 1e-9, "split and synonyms fell below its floor: {split_recall:.3}");
    for kind in ["synonym", "identifier"] {
        assert!(split_kinds[kind].0 >= plain_kinds[kind].0, "{kind}: splitting and synonyms find less than plain BM25");
    }
    assert!(split_recall > plain_recall);
}

#[test]
fn the_reference_words_split_identifiers_and_paths() {
    assert_eq!(pieces("retryBackoffMs"), ["retry", "Backoff", "Ms"]);
    assert_eq!(pieces("HTTPServer"), ["HTTP", "Server"]);
    assert_eq!(split("`max_upload_size` in src/billing/invoiceExporter.ts"), ["max_upload_size", "max", "upload", "size", "src", "billing", "invoiceexporter", "ts", "invoice", "exporter"]);
    assert_eq!(plain("The tests' fixtures"), ["test", "fixture"]);
    let sections = vec![Section { id: "g#Terms".into(), file: "org/acme/glossary.md".into(), heading: "Terms".into(), body: "CI: build pipeline, continuous integration\n".into() }];
    let groups = glossary(&sections);
    let q = query("What does the build pipeline run?", split, &groups);
    assert!(q.contains(&("ci".to_string(), 0.5)) && q.contains(&("build".to_string(), 1.0)), "{q:?}");
}

/// M20.2's search over the same home: its first run sets `memory_search` in floor.json, and every
/// later run must reach it.
#[test]
fn retrieval_recall_at_5_floor() {
    use glasscode_core::memstore::{MemFile, search};
    let questions = questions();
    assert!(questions.len() >= 200);
    // The home's files as memory's search reads a commit of them: the manager's scope (every
    // project), as the questions are asked of the whole home.
    let mut paths = Vec::new();
    files(&memory(), &memory(), &mut paths);
    let files: Vec<(MemFile, String)> = paths
        .iter()
        .map(|path| {
            let rel = path.strip_prefix(memory()).unwrap().to_string_lossy().replace('\\', "/");
            (MemFile::parse(&rel).unwrap_or_else(|| panic!("{rel} is no memory file")), std::fs::read_to_string(path).unwrap())
        })
        .collect();
    let index = search::Index::build(&files);
    let scope = search::Scope::default();
    let (recall, kinds, missed) = recall_at(5, &questions, |q| index.search(q, &scope, 5).into_iter().map(|hit| hit.section).collect());
    println!("memory.search recall at 5: {recall:.3}");
    for kind in KINDS {
        let (found, n) = kinds[kind];
        println!("{kind:<16} {found}/{n}");
    }
    println!("missed: {}", missed.join(" "));
    let floor: serde_json::Value = serde_json::from_slice(&std::fs::read(fixtures().join("retrieval").join("floor.json")).unwrap()).unwrap();
    let at_least = floor["memory_search"].as_f64().expect("floor.json's memory_search, set by M20.2's first run");
    assert!(recall >= at_least - 1e-9, "memory.search fell below its floor: {recall:.3} < {at_least:.3}");
    // A query over 10 MB of memory (the reference load) within the design's 30 ms p95, in a
    // release build; here only that the index answers.
    assert!(!index.search("retry backoff", &scope, 5).is_empty());
}
