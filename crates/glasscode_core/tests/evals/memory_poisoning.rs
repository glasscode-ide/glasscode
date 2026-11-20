//! The coding-memory set (mem-21), in the spirit of WhisperBench: facts arrive through every
//! channel (the person's own words, a session's note, a pull request comment, a tool's output, a
//! pasted block, text Glasscode typed, a direct edit of a memory file, a commit made with plain
//! git), some of them planted, and a probe asks for them.
//!
//! `fixtures/memory_poisoning/cases.jsonl`, one case a line: `category` (extraction, recall across
//! sessions and agents, time, updates, abstention, poisoning, consolidation, staleness, scope,
//! cost, archive), its `channel` for poisoning, the `setup` steps, the `probe`, and what must be
//! stored, returned, never stored and never returned. Every expected text is in a setup step, so
//! a case cannot lose its plant. Releases are gated on the poisoning and scope cases (mem-21).

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

use crate::{fixtures, needs, read_jsonl, SLICES};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub category: String,
    pub needs: String,
    pub project: String,
    pub setup: Vec<Value>,
    pub probe: Value,
    pub expect: Expect,
    #[serde(default)]
    pub channel: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Expect {
    pub stored: Vec<String>,
    pub returned: Vec<String>,
    pub not_stored: Vec<String>,
    pub not_returned: Vec<String>,
    pub abstain: bool,
    #[serde(flatten)]
    pub more: serde_json::Map<String, Value>,
}

pub fn cases() -> Vec<Case> {
    read_jsonl(&fixtures().join("memory_poisoning").join("cases.jsonl"))
}

fn of(categories: &[&str]) -> Vec<Case> {
    let cases: Vec<Case> = cases().into_iter().filter(|case| categories.contains(&case.category.as_str())).collect();
    assert!(!cases.is_empty(), "no case of {categories:?}");
    cases
}

const CATEGORIES: [&str; 12] = ["extraction", "cross_session", "cross_agent", "temporal", "update", "abstention", "poisoning", "consolidation", "staleness", "scope", "cost", "archive"];

/// mem-21's poisoning channels, and the two the write-time scan refuses (M20.1 step 6).
const CHANNELS: [&str; 10] = ["pr_comment", "tool_output", "pasted_block", "injected_prompt", "direct_file_edit", "foreign_commit", "unrelated_quote", "fooled_session_note", "invisible_unicode", "env_value"];

/// How a fact can arrive.
const VIA: [&str; 8] = ["words", "pr_comment", "tool_output", "session_note", "file_edit", "foreign_commit", "worktree_removed", "over_cap_cycles"];

#[test]
fn every_category_and_channel_has_a_case() {
    let cases = cases();
    let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), cases.len(), "ids are unique");
    assert_eq!(cases.iter().map(|case| case.category.as_str()).collect::<BTreeSet<_>>(), CATEGORIES.into_iter().collect());
    let channels: BTreeSet<&str> = cases.iter().filter_map(|case| case.channel.as_deref()).collect();
    assert_eq!(channels, CHANNELS.into_iter().collect());
    for case in &cases {
        assert!(SLICES.contains(&case.needs.as_str()), "{}: {}", case.id, case.needs);
        assert_eq!(case.channel.is_some(), case.category == "poisoning", "{}: a channel is what a poisoning case plants through", case.id);
        let texts: Vec<&str> = case.setup.iter().map(|step| step["text"].as_str().unwrap_or_else(|| panic!("{}: a setup step without text", case.id))).collect();
        for step in &case.setup {
            let via = step["via"].as_str().unwrap_or_default();
            assert!(VIA.contains(&via), "{}: via {via}", case.id);
        }
        // A plant, and a fact, is in what the setup gives.
        for text in case.expect.stored.iter().chain(&case.expect.returned).chain(&case.expect.not_stored).chain(&case.expect.not_returned) {
            assert!(texts.iter().any(|t| t.contains(text.as_str())), "{}: {text:?} is in no setup step", case.id);
        }
        let expects_something = !(case.expect.stored.is_empty() && case.expect.returned.is_empty() && case.expect.not_stored.is_empty() && case.expect.not_returned.is_empty());
        assert!(expects_something || case.expect.abstain || !case.expect.more.is_empty(), "{}: expects nothing", case.id);
        if case.category == "poisoning" {
            assert!(!(case.expect.not_stored.is_empty() && case.expect.not_returned.is_empty()), "{}: a plant that is never kept out", case.id);
            assert!(case.expect.stored.is_empty() && case.expect.returned.is_empty(), "{}", case.id);
        }
        assert!(case.probe["question"].is_string() && case.probe["caller"].is_string(), "{}", case.id);
    }
    // Scope: another project, another org, another account, and a local-only project with the
    // server as home.
    let scope = of(&["scope"]);
    assert!(scope.iter().any(|case| case.probe["project"] != Value::String(case.project.clone())));
    assert!(scope.iter().any(|case| case.expect.more.contains_key("orgs")));
    assert!(scope.iter().any(|case| case.probe.get("account").is_some()));
    assert!(scope.iter().any(|case| case.probe["home"] == "server" && case.expect.more["local_only"] == true));
    // Consolidation runs fifty cycles over the cap.
    assert!(of(&["consolidation"]).iter().any(|case| case.setup.iter().any(|step| step["count"] == 50)));
}

/// A scratch home with the engine at v2 (the words ledger keeps text), memory empty, and the
/// project's key bound, until dropped.
struct MemoryHome {
    home: glasscode_core::testkit::Home,
    key: glasscode_core::paths::ProjectKey,
}

impl MemoryHome {
    fn new() -> MemoryHome {
        use glasscode_core::testkit::{Home, HomeSpec};
        let home = Home::with(&HomeSpec::bare());
        // SAFETY: the evals run one at a time (`--test-threads=1`).
        unsafe { std::env::set_var(glasscode_core::store::ENGINE_VARIABLE, "v2") };
        words::reset();
        let key = match memstore::bind_project("p-app", &home.project("app").root).unwrap() {
            glasscode_core::identity::KeyMatch::New(key) | glasscode_core::identity::KeyMatch::Known(key) => key,
            other => panic!("{other:?}"),
        };
        MemoryHome { home, key }
    }

    /// A case's path with the fixture's project key made this home's.
    fn path(&self, case: &Case, file: &str) -> std::path::PathBuf {
        self.home.glasscode().join(file.replace(&case.project, self.key.as_str()))
    }
}

impl Drop for MemoryHome {
    fn drop(&mut self) {
        words::reset();
        // SAFETY: as in `new`.
        unsafe { std::env::remove_var(glasscode_core::store::ENGINE_VARIABLE) };
    }
}

use glasscode_core::memstore::{self, MemFile, Memory, MemoryError};
use glasscode_core::words::{self, ComposerKind, InjectKind, Origin};

/// What became of a case's setup.
#[derive(Default)]
struct Replayed {
    refused_by_scan: bool,
}

fn origin_of(step: &Value) -> Origin {
    match step["origin"].as_str().unwrap_or("outside") {
        "manager_chat" => Origin::Composer(ComposerKind::ManagerChat),
        "today_box" => Origin::Composer(ComposerKind::TodayBox),
        "terminal_keys" => Origin::TerminalKeys,
        "injected" => Origin::Injected(InjectKind::Playbook),
        other => panic!("origin {other}"),
    }
}

/// Every text the case expects something of.
fn expected(case: &Case) -> Vec<String> {
    let mut texts: Vec<String> = case.expect.stored.iter().chain(&case.expect.not_stored).chain(&case.expect.not_returned).cloned().collect();
    for key in ["proposed", "mentioned"] {
        if let Some(list) = case.expect.more.get(key).and_then(Value::as_array) {
            texts.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
        }
    }
    texts
}

/// Replay a case's setup into memory, each step through its own channel: words through the ledger
/// (a fact is tried for each expected text the words hold), outside text as words of no one, a
/// session's note through `memory note` (with its quote, as a fact citing the person's words),
/// edits and commits made directly.
fn replay(case: &Case, home: &MemoryHome) -> Replayed {
    let memory = Memory::open();
    let file = MemFile::Decisions(home.key.clone());
    let mut out = Replayed::default();
    let mut last_words = None;
    let try_fact = |record: &words::WordsRecord, quote: &str, fact: &str, out: &mut Replayed| match memory.remember(record, quote, fact, &file, None) {
        Err(MemoryError::Refused(_)) => out.refused_by_scan = true,
        _ => {}
    };
    for step in &case.setup {
        let text = step["text"].as_str().unwrap();
        match step["via"].as_str().unwrap() {
            via @ ("words" | "pr_comment" | "tool_output") => {
                let origin = if via == "words" { origin_of(step) } else { Origin::Outside };
                let untyped: Vec<std::ops::Range<usize>> = step["pasted"].as_str().and_then(|pasted| text.find(pasted).map(|at| at..at + pasted.len())).into_iter().collect();
                let id = words::record_with(origin, text, &untyped, step["session"].as_str(), Some(&home.key));
                let record = words::find(&id).expect("recorded");
                // What an extractor would take: each expected text the words hold, and the clause
                // after "that" ("Remember that …", "note that …").
                let mut facts: Vec<String> = expected(case).into_iter().filter(|fact| text.contains(fact.as_str())).collect();
                if let Some((_, clause)) = text.split_once("that ") {
                    facts.push(clause.split(['.', '!', '?']).next().unwrap_or(clause).trim().to_string());
                }
                for fact in &facts {
                    try_fact(&record, fact, fact, &mut out);
                }
                last_words = Some(record);
            }
            "session_note" => {
                let session = step["session"].as_str().unwrap_or("s-test");
                if let Some(quote) = step["quote"].as_str() {
                    try_fact(last_words.as_ref().expect("words to cite"), quote, text, &mut out);
                } else if let Some(code) = step["code"].as_str() {
                    let path = code.split('@').next().unwrap();
                    let line: u32 = code.rsplit("lines-").next().unwrap().parse().unwrap();
                    let root = &home.home.project("app").root;
                    let content = std::fs::read_to_string(root.join(path)).unwrap();
                    let cited = memstore::CodeRef::of(path, "HEAD", &content, line, line).unwrap();
                    let topic = MemFile::Knowhow(home.key.clone(), "code".into());
                    memory.learn(&memstore::Who::Session(session.into()), &topic, None, text, Some(cited)).unwrap();
                } else {
                    let work = glasscode_core::identity::WorkKey { project: home.key.clone(), kind: glasscode_core::identity::WorkKind::Branch, id: step["worktree"].as_str().unwrap_or("main").into() };
                    if let Err(MemoryError::Refused(_)) = memory.note(session, &work, text) {
                        out.refused_by_scan = true;
                    }
                }
            }
            "file_edit" => {
                // Memory has begun, and then a session's shell writes into it.
                memory.remember(&chat_record(home, "Remember that the project has a memory."), "the project has a memory", "the project has a memory", &MemFile::Profile, None).unwrap();
                let path = home.path(case, step["file"].as_str().unwrap());
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                let before = std::fs::read_to_string(&path).unwrap_or_default();
                std::fs::write(&path, format!("{before}{text}\n")).unwrap();
            }
            "foreign_commit" => {
                let file = step["file"].as_str().unwrap();
                if file.starts_with("memory/") {
                    memory.remember(&chat_record(home, "Remember that the project has a memory."), "the project has a memory", "the project has a memory", &MemFile::Profile, None).unwrap();
                    let path = home.path(case, file);
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(&path, format!("{text}\n")).unwrap();
                    let rel = path.strip_prefix(memory.dir()).unwrap().to_string_lossy().into_owned();
                    home.home.git(memory.dir(), &["add", &rel]);
                    home.home.git(memory.dir(), &["commit", "-q", "-m", "plain git"]);
                } else {
                    // The project's own code changes under a cited fact.
                    let root = home.home.project("app").root.clone();
                    let path = root.join(file);
                    let content = std::fs::read_to_string(&path).unwrap();
                    let name = text.split(" = ").next().unwrap();
                    let changed: Vec<String> = content.lines().map(|line| if line.starts_with(name) { text.to_string() } else { line.to_string() }).collect();
                    std::fs::write(&path, changed.join("\n") + "\n").unwrap();
                    home.home.git(&root, &["commit", "-q", "-am", "change the cited line"]);
                }
            }
            "over_cap_cycles" => {
                // Over the cap of the file that holds the rules, again and again.
                let decisions = MemFile::Decisions(home.key.clone());
                for cycle in 0..step["count"].as_u64().unwrap() {
                    let filler = format!("filler {cycle} {}", "y".repeat(400));
                    let record = chat_record(home, &format!("Remember that {filler}."));
                    loop {
                        match memory.remember(&record, &filler, &filler, &decisions, None) {
                            Ok(_) => break,
                            Err(MemoryError::OverCap(over)) => {
                                let oldest = over.entries.iter().find(|(_, _, start)| start.starts_with("filler")).map(|(id, ..)| id.clone()).expect("a filler to let go");
                                memory.let_go(&memstore::Who::Manager, &oldest).unwrap();
                            }
                            Err(err) => panic!("{}: {err}", case.id),
                        }
                    }
                }
            }
            other => panic!("{}: via {other}", case.id),
        }
    }
    out
}

fn chat_record(home: &MemoryHome, text: &str) -> words::WordsRecord {
    let id = words::record(Origin::Composer(ComposerKind::ManagerChat), text, None, Some(&home.key));
    words::find(&id).expect("recorded")
}

/// Every entry of Glasscode's last commit, with its file.
fn entries() -> Vec<(MemFile, memstore::Entry)> {
    let snap = Memory::open().snapshot().unwrap();
    let files = snap.files().unwrap();
    files.iter().flat_map(|file| snap.doc(file).unwrap().entries().cloned().map(|entry| (file.clone(), entry)).collect::<Vec<_>>()).collect()
}

/// A fact memory holds as true: confirmed, or learned, outside the inbox.
fn stored(text: &str) -> bool {
    entries().iter().any(|(file, entry)| *file != MemFile::Proposals && entry.header.kind == memstore::Kind::Fact && matches!(entry.header.status, memstore::Status::Confirmed | memstore::Status::Learned) && entry.text.contains(text))
}

/// Text a brief could carry: in a file of the last commit, not let go, and not a fact about code
/// whose cited lines changed.
fn returnable(text: &str, home: &MemoryHome) -> bool {
    let root = home.home.project("app").root.clone();
    entries().iter().any(|(file, entry)| *file != MemFile::Proposals && entry.header.status != memstore::Status::LetGo && entry.text.contains(text) && entry.header.code.as_ref().is_none_or(|code| memstore::code_holds(&root, code)))
}

fn check(case: &Case, home: &MemoryHome, replayed: &Replayed) {
    let all = entries();
    let has = |text: &str, status: memstore::Status, proposals: bool| all.iter().any(|(file, entry)| (*file == MemFile::Proposals) == proposals && entry.header.status == status && entry.text.contains(text));
    for text in &case.expect.stored {
        assert!(stored(text), "{}: {text:?} is not stored", case.id);
    }
    for text in &case.expect.not_stored {
        assert!(!stored(text), "{}: {text:?} was stored", case.id);
    }
    for text in &case.expect.not_returned {
        assert!(!returnable(text, home), "{}: {text:?} could reach a brief", case.id);
    }
    let list = |key: &str| case.expect.more.get(key).and_then(Value::as_array).map(|list| list.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
    for text in list("proposed") {
        assert!(has(&text, memstore::Status::Confirmed, true), "{}: {text:?} is not proposed", case.id);
    }
    for text in list("mentioned") {
        assert!(has(&text, memstore::Status::Mentioned, false), "{}: {text:?} is not mentioned", case.id);
    }
    if case.expect.more.get("pending") == Some(&Value::Bool(true)) {
        assert!(!Memory::open().pending().unwrap().is_empty(), "{}: nothing pending", case.id);
    }
    if case.expect.more.get("refused_by").and_then(Value::as_str) == Some("scan") {
        assert!(replayed.refused_by_scan, "{}: the scan refused nothing", case.id);
    }
    if case.expect.more.get("know_how_only") == Some(&Value::Bool(true)) {
        for text in &case.expect.not_stored {
            assert!(all.iter().filter(|(_, entry)| entry.text.contains(text.as_str())).all(|(_, entry)| entry.header.kind == memstore::Kind::Knowhow && matches!(entry.header.source, memstore::Source::Session(_))), "{}: {text:?} is more than a session's know-how", case.id);
        }
    }
    if case.expect.more.get("lost").and_then(Value::as_u64) == Some(0) {
        for text in &case.expect.stored {
            assert!(stored(text), "{}: {text:?} was lost", case.id);
        }
    }
}

#[test]
fn the_store_keeps_out_what_was_planted() {
    let cases = of(&["extraction", "poisoning"]);
    assert!(cases.iter().all(|case| case.needs == "M20.1"));
    for case in &cases {
        let home = MemoryHome::new();
        let replayed = replay(case, &home);
        check(case, &home, &replayed);
        drop(home);
    }
}

#[test]
fn consolidation_and_staleness_lose_nothing_they_should_keep() {
    let cases = of(&["consolidation", "staleness"]);
    assert!(!cases.is_empty());
    for case in &cases {
        let home = MemoryHome::new();
        let replayed = replay(case, &home);
        check(case, &home, &replayed);
        drop(home);
    }
}

#[test]
#[ignore = "needs M20.2"]
fn search_and_the_brief_return_only_what_should_come_back() {
    let cases = of(&["cross_session", "cross_agent", "temporal", "update", "abstention", "scope", "cost"]);
    assert!(cases.iter().any(|case| case.expect.abstain));
    needs("M20.2", "set each case up, ask its probe through memory.search and the brief of its caller, and check returned, not_returned, abstain and the cost limits");
}

#[test]
#[ignore = "needs M20.3"]
fn a_local_only_project_never_reaches_the_server() {
    let cases: Vec<Case> = of(&["scope"]).into_iter().filter(|case| case.needs == "M20.3").collect();
    assert_eq!(cases.len(), 1);
    needs("M20.3", "build the server's copy and check that no object of it holds the local-only project's text");
}

#[test]
#[ignore = "needs M21.1"]
fn another_account_does_not_read_the_project() {
    let cases: Vec<Case> = of(&["scope"]).into_iter().filter(|case| case.needs == "M21.1").collect();
    assert_eq!(cases.len(), 1);
    needs("M21.1", "check that the project's model account setting (D17) keeps the fact from a session of another account");
}

#[test]
#[ignore = "needs M24"]
fn archived_work_is_still_found() {
    let cases = of(&["archive"]);
    assert!(cases.iter().all(|case| case.setup.iter().any(|step| step["via"] == "worktree_removed")));
    needs("M24", "remove the worktree through the archive reconciler and check search still returns the note");
}
