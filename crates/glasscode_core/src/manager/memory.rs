//! What the manager remembers: short notes, each a fact. Notes are found again by the words
//! they share with a question; nothing is ever removed, a forgotten note is only marked, so
//! the other copy (`store::merge`) forgets it too.

use std::collections::BTreeSet;

use super::{Data, Note};

/// Words that say nothing about what a note is about.
const STOP: &[&str] = &[
    "a", "about", "after", "again", "all", "also", "am", "an", "and", "any", "are", "as", "at", "be", "because", "been", "before", "but", "by", "can", "could", "did", "do", "does", "doing", "done", "for", "from", "had", "has", "have", "he", "her", "here", "him", "his", "how", "i", "if", "in", "into", "is", "it", "its", "just", "me", "my", "no", "not", "of", "on", "or", "our",
    "she", "should", "so", "some", "than", "that", "the", "their", "them", "then", "there", "these", "they", "this", "those", "to", "too", "up", "us", "use", "was", "we", "were", "what", "when", "where", "which", "who", "why", "will", "with", "would", "you", "your", "know", "tell", "remember", "forget", "note", "thing", "please", "aber", "auf", "das", "dem", "den", "der", "die",
    "ein", "eine", "für", "im", "ist", "mit", "nicht", "und", "von", "was", "wie", "zu",
];

/// Words that often start a sentence with a capital letter and are not a name.
const STARTERS: &[&str] = &[
    "always", "never", "every", "each", "only", "keep", "make", "check", "ask", "run", "deploy", "deploys", "prefer", "prefers", "avoid", "review", "reviews", "send", "open", "close", "start", "stop", "wait", "write", "read", "new", "old", "next", "last", "first", "most", "many", "one", "two", "today", "tomorrow", "yesterday", "now", "later", "yes", "ok", "okay", "let",
    "lets", "see", "look", "try", "give", "take", "put", "set", "get", "go", "our", "team", "people", "everyone", "nobody", "someone", "once", "since", "until", "while", "during",
];

/// How many tags a note gets at most.
const MAX_TAGS: usize = 8;

/// Remember a fact; returns the note's id. The same text twice is one note.
pub fn remember(data: &mut Data, text: &str, source: &str, now: u64) -> String {
    let text = one_line(text);
    if text.is_empty() {
        return String::new();
    }
    let key = normal(&text);
    if let Some(note) = data.notes.iter_mut().find(|n| normal(&n.text) == key) {
        if note.deleted {
            // Remembered again after it was forgotten: later than the forgetting, so it wins.
            note.deleted = false;
            note.text = text.clone();
            note.tags = tags_of(&text);
            note.source = source.trim().to_string();
            note.updated = now.max(note.updated + 1);
        }
        return note.id.clone();
    }
    let id = crate::model::new_id();
    data.notes.push(Note { id: id.clone(), tags: tags_of(&text), text, source: source.trim().to_string(), created: now, updated: now, deleted: false });
    id
}

/// Forget the note with this id, or the one that best matches these words. Returns the text
/// forgotten. When the words fit several notes about equally well, nothing is forgotten:
/// better to be asked again than to lose the wrong one.
pub fn forget(data: &mut Data, id_or_words: &str, now: u64) -> Option<String> {
    let wanted = id_or_words.trim();
    if wanted.is_empty() {
        return None;
    }
    let index = to_forget(data, wanted)?;
    let note = &mut data.notes[index];
    note.deleted = true;
    note.updated = now.max(note.updated);
    Some(note.text.clone())
}

/// The notes that match a question best (not the deleted ones), best first.
pub fn recall<'a>(data: &'a Data, query: &str, limit: usize) -> Vec<&'a Note> {
    let asked = terms(query);
    if asked.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut found: Vec<(usize, &Note)> = data.notes.iter().filter(|n| !n.deleted).map(|n| (score(&asked, n), n)).filter(|(s, _)| *s > 0).collect();
    found.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then_with(|| newest_first(a, b)));
    found.into_iter().take(limit).map(|(_, n)| n).collect()
}

/// Every note that is not deleted, newest first.
pub fn all(data: &Data) -> Vec<&Note> {
    let mut notes: Vec<&Note> = data.notes.iter().filter(|n| !n.deleted).collect();
    notes.sort_by(|a, b| newest_first(a, b));
    notes
}

/// The memory as text for the brain's prompt: what matches the question first, then the
/// newest, within `budget` characters. One note a line, each starting with "- ".
pub fn context(data: &Data, query: &str, budget: usize) -> String {
    let mut picked = recall(data, query, 8);
    for note in all(data) {
        if !picked.iter().any(|p| p.id == note.id) {
            picked.push(note);
        }
    }
    let mut out = String::new();
    let mut used = 0;
    for note in picked {
        let line = format!("- {}", note.text);
        let cost = line.chars().count() + usize::from(!out.is_empty());
        if used + cost <= budget {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&line);
            used += cost;
        } else if out.is_empty() && budget >= 12 {
            // Not even the best note fits: give its beginning rather than nothing.
            out = line.chars().take(budget - 1).collect::<String>() + "…";
            used = budget;
        }
    }
    out
}

// ----- finding ----------------------------------------------------------------------------------

fn newest_first(a: &Note, b: &Note) -> std::cmp::Ordering {
    b.updated.cmp(&a.updated).then(b.created.cmp(&a.created)).then(a.id.cmp(&b.id))
}

/// The note to forget: by id, by its exact text, or the one the words describe.
fn to_forget(data: &Data, wanted: &str) -> Option<usize> {
    let live = || data.notes.iter().enumerate().filter(|(_, n)| !n.deleted);
    if let Some((i, _)) = live().find(|(_, n)| n.id == wanted) {
        return Some(i);
    }
    let key = normal(wanted);
    if let Some((i, _)) = live().find(|(_, n)| normal(&n.text) == key) {
        return Some(i);
    }
    let asked = terms(wanted);
    if asked.is_empty() {
        return None;
    }
    // How well the words describe a note: mostly how many of them are in it, and a little
    // how much of the note they cover.
    let mut fits: Vec<(f64, usize, &Note)> = live()
        .filter_map(|(i, n)| {
            let of_note = note_terms(n);
            let hits = asked.iter().filter(|t| matches(t, &of_note) > 0).count();
            let asked_cover = hits as f64 / asked.len() as f64;
            (asked_cover >= 0.6).then(|| (asked_cover * 0.7 + hits as f64 / of_note.len().max(1) as f64 * 0.3, i, n))
        })
        .collect();
    fits.sort_by(|(sa, _, a), (sb, _, b)| sb.partial_cmp(sa).unwrap_or(std::cmp::Ordering::Equal).then_with(|| newest_first(a, b)));
    match fits.as_slice() {
        [] => None,
        [(_, i, _)] => Some(*i),
        [(best, i, _), (second, _, _), ..] => (best - second >= 0.1).then_some(*i),
    }
}

/// How well a note answers the asked words: ten for each word found, six for a word that
/// only starts the same, and one more when the word is one of the note's tags.
fn score(asked: &BTreeSet<String>, note: &Note) -> usize {
    let of_note = note_terms(note);
    let tags: BTreeSet<String> = note.tags.iter().flat_map(|t| terms(t)).collect();
    asked.iter().map(|t| matches(t, &of_note) + usize::from(tags.contains(t))).sum()
}

fn matches(term: &str, of_note: &BTreeSet<String>) -> usize {
    if of_note.contains(term) {
        10
    } else if term.len() >= 4 && of_note.iter().any(|n| n.len() >= 4 && (n.starts_with(term) || term.starts_with(n.as_str()))) {
        6
    } else {
        0
    }
}

fn note_terms(note: &Note) -> BTreeSet<String> {
    let mut all = terms(&note.text);
    for tag in &note.tags {
        all.extend(terms(tag));
    }
    all
}

// ----- words ------------------------------------------------------------------------------------

/// The text on one line, with single spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text as it is compared: lower case, single spaces, no full stop at the end.
fn normal(text: &str) -> String {
    one_line(text).to_lowercase().trim_end_matches(['.', '!', '?', ' ']).to_string()
}

/// The words that carry meaning, each cut down to its stem.
fn terms(text: &str) -> BTreeSet<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 2 && !STOP.contains(w)).map(stem).collect()
}

fn has_vowel(word: &str) -> bool {
    word.chars().any(|c| "aeiouy".contains(c))
}

/// A light stem: plural, "-ed", "-ing" and a final "e" go, so "merges", "merged" and
/// "merging" are the same word. Not a grammar, only enough to find a note again.
fn stem(word: &str) -> String {
    let mut w = word.to_string();
    if !w.is_ascii() || w.chars().any(|c| c.is_ascii_digit()) {
        return w;
    }
    // Plural.
    if w.len() > 4 && w.ends_with("ies") {
        w.truncate(w.len() - 3);
        w.push('y');
    } else if w.len() > 4 && ["ses", "xes", "zes", "ches", "shes"].iter().any(|end| w.ends_with(end)) {
        w.truncate(w.len() - 2);
    } else if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") && !w.ends_with("us") && !w.ends_with("is") {
        w.truncate(w.len() - 1);
    }
    // "-ing", "-ed".
    for end in ["ing", "ed"] {
        if w.len() > end.len() + 2 && w.ends_with(end) && has_vowel(&w[..w.len() - end.len()]) {
            w.truncate(w.len() - end.len());
            let b = w.as_bytes();
            if b.len() > 2 && b[b.len() - 1] == b[b.len() - 2] && !b"lsz".contains(&b[b.len() - 1]) {
                w.truncate(w.len() - 1);
            }
            break;
        }
    }
    if w.len() > 3 && w.ends_with('e') {
        w.truncate(w.len() - 1);
    }
    w
}

/// Tags found in a note: `#words` the person wrote, pull request numbers (`#123`), ticket
/// ids (`ENG-826`) and names (words with a capital letter that are not ordinary words).
fn tags_of(text: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    let mut add = |tag: String| {
        if tags.len() < MAX_TAGS && !tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
            tags.push(tag);
        }
    };
    let mut starts_sentence = true;
    for raw in text.split_whitespace() {
        let ends_sentence = raw.ends_with(['.', '!', '?', ':']);
        let word = raw.trim_matches(|c: char| !(c.is_alphanumeric() || c == '#'));
        let word = word.strip_suffix("'s").or_else(|| word.strip_suffix("’s")).unwrap_or(word);
        let first = starts_sentence;
        starts_sentence = ends_sentence;
        if let Some(rest) = word.strip_prefix('#') {
            if !rest.is_empty() && rest.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
                add(if rest.chars().all(|c| c.is_ascii_digit()) { format!("#{rest}") } else { rest.to_lowercase() });
            }
        } else if is_ticket(word) || is_name(word, first) {
            add(word.to_string());
        }
    }
    tags
}

/// `ENG-826`: capital letters, a dash, digits.
fn is_ticket(word: &str) -> bool {
    match word.split_once('-') {
        Some((team, number)) => team.len() >= 2 && team.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) && team.starts_with(|c: char| c.is_ascii_uppercase()) && !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// A word written with a capital letter that is not an ordinary word: "Sam", "GitHub". At
/// the start of a sentence every word has a capital, so more words are ruled out there.
fn is_name(word: &str, starts_sentence: bool) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else { return false };
    if !first.is_uppercase() || word.chars().count() < 2 || !word.chars().all(char::is_alphabetic) || !word.chars().any(char::is_lowercase) {
        return false;
    }
    let low = word.to_lowercase();
    !STOP.contains(&low.as_str()) && !(starts_sentence && STARTERS.contains(&low.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_with(notes: &[(&str, u64)]) -> Data {
        let mut data = Data::default();
        for (text, at) in notes {
            remember(&mut data, text, "you", *at);
        }
        data
    }

    #[test]
    fn remembers_a_fact_with_its_tags() {
        let mut data = Data::default();
        let id = remember(&mut data, "  Sam prefers small pull requests,\n see #123 and ENG-826  #backend ", "you", 100);
        assert!(!id.is_empty());
        let note = &data.notes[0];
        assert_eq!(note.id, id);
        assert_eq!(note.text, "Sam prefers small pull requests, see #123 and ENG-826 #backend");
        assert_eq!(note.tags, vec!["Sam", "#123", "ENG-826", "backend"]);
        assert_eq!((note.source.as_str(), note.created, note.updated, note.deleted), ("you", 100, 100, false));
    }

    #[test]
    fn the_same_text_twice_is_one_note() {
        let mut data = Data::default();
        let first = remember(&mut data, "Deploys happen on Fridays.", "you", 10);
        let again = remember(&mut data, "  deploys   happen on FRIDAYS ", "manager", 20);
        assert_eq!(first, again);
        assert_eq!(data.notes.len(), 1);
        assert_eq!(data.notes[0].updated, 10, "a note that is already there is left as it is");
        assert_eq!(remember(&mut data, "   ", "you", 30), "");
        assert_eq!(data.notes.len(), 1);
        let other = remember(&mut data, "Deploys happen on Mondays.", "you", 40);
        assert_ne!(other, first);
        assert_eq!(data.notes.len(), 2);
    }

    #[test]
    fn tags_are_names_numbers_tickets_and_hash_words() {
        assert_eq!(tags_of("Always ask Mira before touching the billing code"), vec!["Mira"]);
        assert_eq!(tags_of("The staging server is slow."), Vec::<String>::new());
        assert_eq!(tags_of("Mira's team owns GitHub actions; Alex owns WEB-12."), vec!["Mira", "GitHub", "Alex", "WEB-12"]);
        assert_eq!(tags_of("#Release-Notes go out after #42, and #42 again"), vec!["release-notes", "#42"]);
        assert_eq!(tags_of("Use the API. Never push on Friday."), vec!["Friday"]);
        assert_eq!(tags_of("CI is flaky, see PR-7 and A-1").len(), 1, "only PR-7 looks like a ticket");
        assert!(tags_of("One Two Three Four Five Six Seven Eight Nine Ten Eleven Twelve Thirteen").len() <= MAX_TAGS);
    }

    #[test]
    fn stems_bring_word_forms_together() {
        for group in [
            vec!["merge", "merges", "merged", "merging"],
            vec!["deploy", "deploys", "deployed", "deploying"],
            vec!["review", "reviews", "reviewed", "reviewing"],
            vec!["fix", "fixes", "fixed", "fixing"],
            vec!["release", "releases", "released"],
            vec!["run", "runs", "running"],
            vec!["prefer", "prefers", "preferred"],
            vec!["branch", "branches"],
            vec!["reply", "replies"],
            vec!["string", "strings"],
        ] {
            let stems: BTreeSet<String> = group.iter().map(|w| stem(w)).collect();
            assert_eq!(stems.len(), 1, "{group:?} gave {stems:?}");
        }
        assert_eq!(stem("status"), "status");
        assert_eq!(stem("need"), "need");
        assert_eq!(stem("1111"), "1111");
    }

    #[test]
    fn recall_finds_by_shared_words_best_first() {
        let data = data_with(&[
            ("Sam reviews pull requests on Friday mornings", 10),
            ("The staging server is redeployed every night", 20),
            ("Mira wants a ticket for every deployment to production", 30),
            ("Alex is away in December", 40),
        ]);
        let found = recall(&data, "When does Sam review?", 5);
        assert_eq!(found.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), vec!["Sam reviews pull requests on Friday mornings"]);

        // A word that only starts the same still finds it, behind exact words.
        let found = recall(&data, "who deploys to production", 5);
        assert_eq!(found[0].text, "Mira wants a ticket for every deployment to production");

        assert!(recall(&data, "what is the and of it", 5).is_empty(), "only filler words find nothing");
        assert!(recall(&data, "kubernetes", 5).is_empty());
        assert!(recall(&data, "", 5).is_empty());
        assert!(recall(&data, "Sam", 0).is_empty());
    }

    #[test]
    fn recall_prefers_the_newest_on_a_tie_and_respects_the_limit() {
        let data = data_with(&[("Sam likes tabs", 10), ("Sam likes short titles", 30), ("Sam likes rebasing", 20)]);
        let found = recall(&data, "what does Sam like", 2);
        assert_eq!(found.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), vec!["Sam likes short titles", "Sam likes rebasing"]);
    }

    #[test]
    fn recall_finds_a_pull_request_by_its_number() {
        let data = data_with(&[("Do not merge #1111 before the database change is live", 10), ("#42 needs Mira's review", 20)]);
        assert_eq!(recall(&data, "anything about 1111?", 3)[0].text, "Do not merge #1111 before the database change is live");
        assert_eq!(recall(&data, "#42", 3)[0].text, "#42 needs Mira's review");
    }

    #[test]
    fn forgets_by_id_and_keeps_the_note_marked() {
        let mut data = Data::default();
        let id = remember(&mut data, "Alex is away in December", "you", 10);
        assert_eq!(forget(&mut data, &id, 50).as_deref(), Some("Alex is away in December"));
        assert_eq!(data.notes.len(), 1, "never removed, so the other copy forgets it too");
        assert!(data.notes[0].deleted);
        assert_eq!(data.notes[0].updated, 50);
        assert!(all(&data).is_empty());
        assert!(recall(&data, "Alex December", 5).is_empty());
        assert_eq!(forget(&mut data, &id, 60), None, "already forgotten");
    }

    #[test]
    fn forgets_the_note_the_words_describe() {
        let mut data = data_with(&[("Sam reviews pull requests on Friday mornings", 10), ("The staging server is redeployed every night", 20), ("Alex is away in December", 30)]);
        assert_eq!(forget(&mut data, "that Alex is away in december", 40).as_deref(), Some("Alex is away in December"));
        assert_eq!(forget(&mut data, "the staging server thing", 41).as_deref(), Some("The staging server is redeployed every night"));
        assert_eq!(forget(&mut data, "Sam's Friday reviews", 42).as_deref(), Some("Sam reviews pull requests on Friday mornings"));
        assert!(all(&data).is_empty());
    }

    #[test]
    fn does_not_forget_on_a_weak_or_unclear_match() {
        let mut data = data_with(&[("Sam likes tabs", 10), ("Sam likes spaces in YAML", 20), ("The staging server is redeployed every night", 30)]);
        assert_eq!(forget(&mut data, "the production database password", 40), None, "too few shared words");
        assert_eq!(forget(&mut data, "Sam", 40), None, "two notes fit about equally: forget neither");
        assert_eq!(forget(&mut data, "", 40), None);
        assert_eq!(forget(&mut data, "the and of", 40), None);
        assert_eq!(all(&data).len(), 3);
        // Said clearly enough, one of the two goes.
        assert_eq!(forget(&mut data, "Sam likes tabs", 41).as_deref(), Some("Sam likes tabs"));
        assert_eq!(forget(&mut data, "Sam", 42).as_deref(), Some("Sam likes spaces in YAML"), "now only one note is about Sam");
    }

    #[test]
    fn remembering_again_brings_a_forgotten_note_back() {
        let mut data = Data::default();
        let id = remember(&mut data, "Alex is away in December", "you", 10);
        forget(&mut data, &id, 50);
        let again = remember(&mut data, "alex is away in december", "session:s1", 50);
        assert_eq!(again, id);
        assert_eq!(data.notes.len(), 1);
        assert!(!data.notes[0].deleted);
        assert!(data.notes[0].updated > 50, "later than the forgetting, so the other copy takes it");
        assert_eq!(data.notes[0].source, "session:s1");
    }

    #[test]
    fn all_lists_live_notes_newest_first() {
        let mut data = data_with(&[("first", 10), ("third", 30), ("second", 20)]);
        forget(&mut data, "second", 40);
        assert_eq!(all(&data).iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), vec!["third", "first"]);
    }

    #[test]
    fn context_puts_matches_first_and_stays_in_budget() {
        let data = data_with(&[("Sam reviews pull requests on Friday mornings", 10), ("The staging server is redeployed every night", 20), ("Alex is away in December", 30)]);
        let text = context(&data, "when does Sam review", 500);
        assert_eq!(text, "- Sam reviews pull requests on Friday mornings\n- Alex is away in December\n- The staging server is redeployed every night");

        // With no question: the newest first.
        assert!(context(&data, "", 500).starts_with("- Alex is away in December\n"));

        // A tight budget: whole notes only, the best first, shorter ones may still fit.
        let tight = context(&data, "when does Sam review", 75);
        assert_eq!(tight, "- Sam reviews pull requests on Friday mornings\n- Alex is away in December");
        assert!(tight.chars().count() <= 75);

        // Not even one note fits: its beginning.
        let tiny = context(&data, "when does Sam review", 20);
        assert_eq!(tiny.chars().count(), 20);
        assert!(tiny.starts_with("- Sam reviews") && tiny.ends_with('…'));

        assert_eq!(context(&data, "Sam", 0), "");
        assert_eq!(context(&Data::default(), "Sam", 500), "");
    }

    #[test]
    fn context_leaves_out_forgotten_notes() {
        let mut data = data_with(&[("Sam reviews on Fridays", 10), ("Alex is away in December", 20)]);
        forget(&mut data, "Alex is away in December", 30);
        assert_eq!(context(&data, "Alex", 500), "- Sam reviews on Fridays");
    }
}
