//! Which session the person means, from their words: words of its title, its project, its branch,
//! its pull request or ticket, and what it is about. Spoken words are often misheard ("class code"
//! for glasscode), so a word also counts when it is the start of a longer one, one letter off a
//! long one, or two words said apart that are one ("glass code"). It only ranks: the caller (the
//! voice's talker, the manager) is shown the titles of the best ones and judges the rest.
//!
//! What it searches is what the sidebar holds, sessions. A worktree left without a session is
//! not one; a caller that searches those too does it apart, to say that it is closed.

/// A session (or a worktree) as it is searched.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub id: String,
    /// What the sidebar calls it.
    pub title: String,
    /// The other names it goes by: its own title, its pull request's, its review's.
    pub names: Vec<String>,
    pub project: String,
    pub branch: String,
    /// Its pull request's number, and the one it reviews.
    pub numbers: Vec<u64>,
    /// Its ticket (ENG-42).
    pub ticket: Option<String>,
    /// What it is about in other words: the task it started with, its last reply. Only whole
    /// words and their starts count here (it is long, and a near miss is no sign).
    pub about: String,
}

/// An entry that fits the words.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// Its place in the entries searched.
    pub index: usize,
    pub score: f32,
    /// The person's words it fits, as they said them.
    pub matched: Vec<String>,
}

/// The entries that fit, best first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ranked {
    pub hits: Vec<Hit>,
    /// How many of the words said could name something (the rest are words like "the session").
    pub words: usize,
}

impl Ranked {
    /// The one that stands out: the only one, or well ahead of the next.
    pub fn clear(&self) -> Option<&Hit> {
        match self.hits.as_slice() {
            [one] => Some(one),
            [first, second, ..] if first.score >= second.score * 1.5 && first.score - second.score >= 1.5 => Some(first),
            _ => None,
        }
    }
}

/// Words that never name a session: how a request is put, not what it is about.
const FILLER: &[&str] = &[
    "a", "an", "the", "and", "or", "of", "in", "on", "at", "for", "to", "from", "with", "about", "into", "that", "which", "this", "these", "those", "it", "its", "is", "are", "was",
    "be", "do", "did", "can", "could", "would", "will", "you", "your", "me", "my", "mine", "i", "we", "our", "us", "please", "just", "now", "there", "here", "where", "what", "one",
    "ones", "called", "named", "titled", "title", "session", "sessions", "open", "opened", "show", "go", "goto", "find", "see", "look", "want", "need", "let", "project",
    "worktree", "branch", "pr", "pull", "request", "number", "ticket", "thing", "agent", "claude", "codex", "tab", "window", "ide", "screen",
];

/// The words of a text, in small letters: "#958" is 958, "hub-825" is hub and 825.
pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// The words said that could name something, as said (filler and single letters left out).
fn wanted(said: &str) -> Vec<String> {
    words(said).into_iter().filter(|w| !FILLER.contains(&w.as_str()) && (w.chars().count() > 1)).collect()
}

fn numeric(word: &str) -> bool {
    word.chars().all(|c| c.is_ascii_digit())
}

/// How well a word said fits a word of a field: 1 the same; less for the start of it (`starts`),
/// or a near miss of a long word (`near`); 0 not at all. Numbers fit only as they are.
fn fit(said: &str, field: &str, near: bool, starts: bool) -> f32 {
    if said == field {
        return 1.0;
    }
    if numeric(said) || numeric(field) {
        return 0.0;
    }
    let (s, f) = (said.chars().count(), field.chars().count());
    if starts && ((s >= 3 && field.starts_with(said)) || (f >= 4 && said.starts_with(field))) {
        return 0.8;
    }
    if !near {
        return 0.0;
    }
    let distance = edits(said, field);
    match distance {
        1 if s >= 5 && f >= 5 => 0.7,
        2 if s >= 8 && f >= 8 => 0.5,
        _ => 0.0,
    }
}

/// The letters to change, add or drop to make one word the other.
fn edits(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut last = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let here = row[j + 1];
            row[j + 1] = if ca == cb { last } else { 1 + last.min(here).min(row[j]) };
            last = here;
        }
    }
    row[b.len()]
}

/// An entry's fields as words, each with its weight and whether a near miss counts there.
struct Fields {
    fields: Vec<(Vec<String>, f32, bool)>,
    numbers: Vec<String>,
    id: String,
}

impl Fields {
    fn of(e: &Entry) -> Self {
        let mut project = words(&e.project);
        // "hub2.0" is said "hub two", and "glass code" is the project glasscode.
        project.push(e.project.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase());
        let names: Vec<String> = e.names.iter().flat_map(|n| words(n)).collect();
        Fields {
            fields: vec![
                (words(&e.title), 3.0, true),
                (names, 2.0, true),
                (words(&e.branch), 2.0, true),
                (project, 1.5, true),
                (e.ticket.as_deref().map(words).unwrap_or_default(), 3.0, false),
                (words(&e.about), 1.0, false),
            ],
            numbers: e.numbers.iter().map(u64::to_string).collect(),
            id: e.id.to_lowercase(),
        }
    }

    /// The best a word said scores in any field. Two words said as one (`joined`) count only as
    /// a whole word: "voice livekit" is not the start of "voice".
    fn score(&self, said: &str, joined: bool) -> f32 {
        if numeric(said) && self.numbers.iter().any(|n| n == said) {
            return 5.0;
        }
        // The start of its id (four letters at least), as a list showed it.
        if said.chars().count() >= 4 && self.id.starts_with(said) {
            return 10.0;
        }
        let mut best = 0.0f32;
        for (words, weight, near) in &self.fields {
            for w in words {
                best = best.max(weight * fit(said, w, *near, !joined));
            }
        }
        best
    }
}

/// The entries that fit what the person said, best first (ties keep the entries' order).
pub fn rank(said: &str, entries: &[Entry]) -> Ranked {
    let said = wanted(said);
    let mut hits = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let fields = Fields::of(entry);
        let (mut score, mut matched, mut i) = (0.0f32, Vec::new(), 0);
        while i < said.len() {
            let one = fields.score(&said[i], false);
            // Two words said apart that are one word of it ("glass code").
            if let Some(next) = said.get(i + 1).filter(|n| !numeric(n) && !numeric(&said[i])) {
                let both = fields.score(&format!("{}{next}", said[i]), true);
                if both > 0.0 && 2.0 * both > one + fields.score(next, false) {
                    score += 2.0 * both;
                    matched.push(format!("{} {next}", said[i]));
                    i += 2;
                    continue;
                }
            }
            if one > 0.0 {
                score += one;
                matched.push(said[i].clone());
            }
            i += 1;
        }
        if score > 0.0 {
            hits.push(Hit { index, score, matched });
        }
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ranked { hits, words: said.len() }
}

/// The project a name said means: by its name (case, spaces and dots aside), else the one name
/// that fits it best ("class code" is glasscode). None when nothing or several fit.
pub fn project(said: &str, names: &[&str]) -> Option<usize> {
    let flat = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    let wanted = flat(said);
    if wanted.is_empty() {
        return None;
    }
    if let Some(i) = names.iter().position(|n| flat(n) == wanted) {
        return Some(i);
    }
    let entries: Vec<Entry> = names.iter().map(|n| Entry { project: n.to_string(), ..Entry::default() }).collect();
    let ranked = rank(said, &entries);
    ranked.clear().map(|hit| hit.index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, title: &str, project: &str, branch: &str) -> Entry {
        Entry { id: id.into(), title: title.into(), project: project.into(), branch: branch.into(), ..Entry::default() }
    }

    /// The glasscode sessions on the person's Mac the day the voice could not find one.
    fn glasscode() -> Vec<Entry> {
        vec![
            entry("d3aef5a3-1111", "Manager agent harness for the IDE", "glasscode", "glasscode/manager-agent-harness-ide"),
            entry("b4ac1e41-2222", "Mobile and glasses app: talk to the manager", "glasscode", "glasscode/mobile-glasses-manager"),
            Entry { names: vec!["Claude · glasscode/session-95d4".into()], ..entry("03f33216-3333", "Claude · glasscode/session-95d4", "glasscode", "glasscode/ali-s-current-pr-work") },
            entry("02237e60-4444", "Cheap voice pipeline with LiveKit and ElevenLabs", "glasscode", "glasscode/voice-livekit-talker"),
            entry("55de6efd-5555", "Why the finished state and orange icon lag", "glasscode", "glasscode/session-b912"),
            entry("2c23ab93-6666", "Prepare the project for publishing", "glasscode", "glasscode/prepare-for-publishing"),
            entry("cd371184-7777", "Voice agent: find and open the right session", "glasscode", "glasscode/voice-find-open-session"),
        ]
    }

    fn best(said: &str, entries: &[Entry]) -> Option<String> {
        rank(said, entries).clear().map(|h| entries[h.index].id.clone())
    }

    #[test]
    fn finds_a_session_by_words_of_its_title() {
        let all = glasscode();
        assert_eq!(best("open the orange icon lag session", &all).as_deref(), Some("55de6efd-5555"));
        assert_eq!(best("Prepare the project for publishing", &all).as_deref(), Some("2c23ab93-6666"));
        assert_eq!(best("the publishing one", &all).as_deref(), Some("2c23ab93-6666"));
        assert_eq!(best("mobile glasses", &all).as_deref(), Some("b4ac1e41-2222"));
    }

    #[test]
    fn a_misheard_title_still_finds_it() {
        // Said "Why the Phoenix State and Orange Icon": three words fit, the misheard one does not.
        let all = glasscode();
        let ranked = rank("Why the Phoenix State and Orange Icon", &all);
        assert_eq!(all[ranked.clear().unwrap().index].id, "55de6efd-5555");
        assert_eq!(ranked.hits[0].matched, ["why", "state", "orange", "icon"]);
        // One letter off a long word counts; a short word must be right.
        assert_eq!(best("harnes for the manager", &all).as_deref(), Some("d3aef5a3-1111"));
    }

    #[test]
    fn finds_it_by_its_branch_when_its_title_says_nothing() {
        // The title is still the generated one; the branch has the words.
        let all = glasscode();
        assert_eq!(best("Ali's current PR work", &all).as_deref(), Some("03f33216-3333"));
        assert_eq!(best("the livekit talker", &all).as_deref(), Some("02237e60-4444"));
        assert_eq!(best("session b912", &all).as_deref(), Some("55de6efd-5555"));
    }

    #[test]
    fn two_that_fit_alike_are_both_named() {
        // "voice" is in two of them: neither stands out, so the caller asks which.
        let all = glasscode();
        let ranked = rank("the voice session", &all);
        assert!(ranked.clear().is_none());
        let ids: Vec<&str> = ranked.hits.iter().take(2).map(|h| all[h.index].id.as_str()).collect();
        assert!(ids.contains(&"02237e60-4444") && ids.contains(&"cd371184-7777"), "{ids:?}");
        // With more of its words, one does.
        assert_eq!(best("the voice livekit one", &all).as_deref(), Some("02237e60-4444"));
    }

    #[test]
    fn a_project_said_apart_or_misheard_fits_every_session_there() {
        let mut all = glasscode();
        all.push(entry("0c9fc561-8888", "Prod deployment (Scaleway)", "hub2.0", "infra/prod-scw-cluster"));
        for said in ["glass code", "class code", "glasscode"] {
            let ranked = rank(said, &all);
            assert_eq!(ranked.hits.len(), 7, "{said}");
            assert!(ranked.hits.iter().all(|h| all[h.index].project == "glasscode"), "{said}");
        }
        assert_eq!(best("the scaleway deployment in hub", &all).as_deref(), Some("0c9fc561-8888"));
    }

    #[test]
    fn numbers_tickets_and_ids() {
        let all = vec![
            Entry { numbers: vec![1131], ticket: Some("HUB-825".into()), ..entry("9994fa5f-aaaa", "Voice mode", "hub2.0", "feat/hub-825-voice-mode") },
            Entry { numbers: vec![1132], ..entry("1a2b3c4d-bbbb", "Something else", "hub2.0", "feat/other") },
        ];
        assert_eq!(best("pull request 1131", &all).as_deref(), Some("9994fa5f-aaaa"));
        assert_eq!(best("#1132", &all).as_deref(), Some("1a2b3c4d-bbbb"));
        // A number is never near another number.
        assert!(rank("1133", &all).hits.is_empty());
        assert_eq!(best("HUB-825", &all).as_deref(), Some("9994fa5f-aaaa"));
        assert_eq!(best("1a2b3c4d", &all).as_deref(), Some("1a2b3c4d-bbbb"));
    }

    #[test]
    fn what_it_is_about_counts_but_only_whole_words() {
        let all = vec![
            Entry { about: "Write a proper README and record an explainer video".into(), ..entry("a", "Claude · glasscode/session-1", "glasscode", "glasscode/session-1") },
            entry("b", "Claude · glasscode/session-2", "glasscode", "glasscode/session-2"),
        ];
        assert_eq!(best("the readme one", &all).as_deref(), Some("a"));
        assert_eq!(best("the explainer video", &all).as_deref(), Some("a"));
        // A near miss of a word in it is not enough.
        assert!(rank("radme", &all).hits.is_empty());
    }

    #[test]
    fn filler_alone_names_nothing() {
        let ranked = rank("open the session for me please", &glasscode());
        assert_eq!(ranked.words, 0);
        assert!(ranked.hits.is_empty());
    }

    #[test]
    fn a_project_by_its_name_or_how_it_was_heard() {
        let names = ["hub2.0", "glasscode", "whereismypartner", "whereismyuni"];
        assert_eq!(project("Glasscode", &names), Some(1));
        assert_eq!(project("glass code", &names), Some(1));
        assert_eq!(project("class code", &names), Some(1));
        assert_eq!(project("hub 2.0", &names), Some(0));
        assert_eq!(project("hub", &names), Some(0));
        assert_eq!(project("where is my uni", &names), Some(3));
        // "where is my" starts two of them: neither.
        assert_eq!(project("whereismy", &names), None);
        assert_eq!(project("nightshift", &names), None);
        assert_eq!(project("", &names), None);
    }

    #[test]
    fn edits_count_letters() {
        assert_eq!(edits("classcode", "glasscode"), 1);
        assert_eq!(edits("phoenix", "finished"), 8);
        assert_eq!(edits("", "abc"), 3);
    }
}
