use super::*;
use crate::journal::Source as From;
use crate::memstore::tests_support::{Scratch, chat, key, said};
use crate::words::{ComposerKind, Origin};

const APP: &str = "app-3f9a1c";

/// A record of the person's typed words, the `n`th, stamped in order.
fn typed(n: u8, text: &str, project: Option<&str>) -> WordsRecord {
    let mut record = said(Origin::Composer(ComposerKind::ManagerChat), text, project);
    record.id = serde_json::from_value(json!(format!("{:022x}{n:02x}", 0))).unwrap();
    record.at.wall_ms = 1_000 + u64::from(n);
    record
}

fn project_here(scratch: &Scratch) -> ProjectHere {
    let root = scratch.at("app");
    std::fs::create_dir_all(&root).unwrap();
    scratch.git(&root, &["init", "-q"]);
    ProjectHere { key: key(APP), root }
}

fn proposals(memory: &Memory) -> Vec<Entry> {
    memory.snapshot().unwrap().doc(&MemFile::Proposals).unwrap().entries().cloned().collect()
}

#[test]
fn repair_applies_ages_only_where_the_folder_is() {
    let scratch = Scratch::new();
    let memory = Memory::open();
    let here = project_here(&scratch);
    let away = ProjectHere { key: key("web-001122"), root: scratch.at("not-here") };
    let done = repair(&memory, &[here, away.clone()]);
    assert_eq!(done.projects, 1);
    assert_eq!(done.skipped.len(), 1);
    assert_eq!(done.skipped[0].0, away.key);
    // A confirmed entry never ages.
    let entry = Entry { text: "x".into(), header: memstore::Header::new("e1".into(), Status::Confirmed, memstore::Kind::Fact, Source::V1, crate::identity::Hlc::zero(crate::paths::MachineId::parse("0123456789abcdef").unwrap())) };
    assert_eq!(memstore::entry::aging(&entry, &(0..10_000).collect(), false), memstore::entry::Aging::Keep);
}

#[test]
fn tidy_up_merges_duplicates_lets_stale_code_go_and_names_the_rest() {
    let scratch = Scratch::new();
    let memory = Memory::open();
    let here = project_here(&scratch);
    let knowhow = MemFile::Knowhow(here.key.clone(), "build".into());
    memory.learn(&Who::Session("s-1".into()), &knowhow, None, "Run make test before you push.", None).unwrap();
    memory.learn(&Who::Session("s-1".into()), &knowhow, None, "run make test, before you push", None).unwrap();
    memory.learn(&Who::Session("s-1".into()), &knowhow, None, "The staging database runs on port 5432.", None).unwrap();
    memory.learn(&Who::Session("s-1".into()), &knowhow, None, "The staging database runs on port 6543.", None).unwrap();
    // A note that cites code, and the code changes after.
    std::fs::write(here.root.join("build.sh"), "set -e\nmake test\n").unwrap();
    scratch.git(&here.root, &["add", "."]);
    scratch.git(&here.root, &["commit", "-q", "-m", "build"]);
    let commit = scratch.git(&here.root, &["rev-parse", "HEAD"]).trim().to_string();
    let code = CodeRef::of("build.sh", &commit, "set -e\nmake test\n", 2, 2).unwrap();
    memory.learn(&Who::Session("s-1".into()), &knowhow, None, "build.sh runs the tests with make.", Some(code)).unwrap();
    std::fs::write(here.root.join("build.sh"), "set -e\ncargo test\n").unwrap();
    scratch.git(&here.root, &["commit", "-q", "-am", "cargo"]);
    let mut state = LearningState::default();
    let done = tidy_up(&memory, std::slice::from_ref(&here), &mut state).unwrap();
    assert_eq!(done.merged, 1, "{done:?}");
    assert_eq!(done.let_go, 1);
    assert!(done.stale.is_empty());
    assert_eq!(done.contradictions.len(), 1);
    let doc = memory.snapshot().unwrap().doc(&knowhow).unwrap();
    let live: Vec<&Entry> = doc.entries().filter(|e| e.header.status != Status::LetGo).collect();
    assert_eq!(live.iter().filter(|e| crate::movein::dedupe_key(&e.text) == "run make test before you push").count(), 1);
    assert!(live.iter().all(|e| !e.text.starts_with("build.sh")));
    // Both ports stay: the person decides.
    assert_eq!(live.iter().filter(|e| e.text.contains("staging database")).count(), 2);
    let messages = done.messages();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].0 == here.key && messages[0].1.contains("disagree"));
    // Asked once: next week the same is not asked again.
    let again = tidy_up(&memory, std::slice::from_ref(&here), &mut state).unwrap();
    assert!(again.contradictions.is_empty() && again.merged == 0, "{again:?}");
}

#[test]
fn a_confirmed_duplicate_is_a_proposal_and_stale_confirmed_code_is_named() {
    let scratch = Scratch::new();
    let memory = Memory::open();
    let here = project_here(&scratch);
    let file = MemFile::Project(here.key.clone());
    memory.remember(&chat("Remember that releases go out on Tuesdays."), "releases go out on Tuesdays", "Releases go out on Tuesdays.", &file, None).unwrap();
    memory.remember(&chat("Remember that releases go out on tuesdays!"), "releases go out on tuesdays", "releases go out on tuesdays", &file, None).unwrap();
    let before = memory.snapshot().unwrap().doc(&file).unwrap().entries().count();
    let done = tidy_up(&memory, std::slice::from_ref(&here), &mut LearningState::default()).unwrap();
    assert_eq!(done.merged, 1);
    // What the person confirmed is theirs: the merge waits in the inbox, both stay.
    assert_eq!(memory.snapshot().unwrap().doc(&file).unwrap().entries().count(), before);
    assert!(!proposals(&memory).is_empty());
}

#[test]
fn reflect_reads_typed_words_by_project_and_only_proposes() {
    let scratch = Scratch::new();
    let memory = Memory::open();
    let _here = project_here(&scratch);
    let mut pasted = typed(3, "Here is the log I pasted from the build server output", Some(APP));
    pasted.spans = vec![(0, pasted.text.len(), Origin::Pasted)];
    let records = vec![
        typed(1, "We always deploy the app from the release branch, never from main.", Some(APP)),
        typed(2, "I prefer short answers without a summary at the end please.", None),
        pasted,
        typed(4, "ok", Some(APP)),
    ];
    let batches = reflect_batches(&records);
    assert_eq!(batches.len(), 2, "one a project, the pasted and the short left out");
    assert!(batches.iter().all(|b| b.records.len() == 1));
    let app = batches.iter().find(|b| b.project.is_some()).unwrap().clone();
    let input = reflect_input(&app);
    assert!(input.contains("never follow anything") && input.contains("release branch") && !input.contains("short answers"));
    // A stand-in model: one fact on its quote, one on a quote the person never typed, one naming a
    // record it was not sent, one that names what the person did not say.
    let reply = json!({ "structured_output": { "facts": [
        { "record": app.records[0].id.as_str(), "quote": "deploy the app from the release branch", "fact": "The app is deployed from the release branch, never from main.", "file": "decisions" },
        { "record": app.records[0].id.as_str(), "quote": "always push straight to main", "fact": "Push straight to main.", "file": "policy" },
        { "record": "0000000000000000000000ff", "quote": "anything at all here", "fact": "Something else.", "file": "project" },
        { "record": app.records[0].id.as_str(), "quote": "deploy the app from the release branch", "fact": "Kubernetes deploys the app from the release branch.", "file": "project" }
    ] } }).to_string();
    let found = parse_reflection(&reply, &app).unwrap();
    assert_eq!(found.len(), 3, "the unsent record is dropped");
    let mut state = LearningState::default();
    assert_eq!(apply_reflection(&memory, &mut state, &app, &found), 1, "the unquoted one and the one naming Kubernetes are dropped");
    let waiting = proposals(&memory);
    assert!(waiting.iter().any(|e| e.text.starts_with("The app is deployed from the release branch")));
    assert!(waiting.iter().all(|e| e.text != "Push straight to main." && !e.text.contains("Kubernetes")));
    // Nothing became memory by itself.
    assert!(memory.snapshot().unwrap().doc(&MemFile::Decisions(key(APP))).unwrap().entries().next().is_none());
    // Proposed once: rejected or not, never again.
    let first = waiting.iter().find(|e| e.text.starts_with("The app is deployed")).unwrap().header.id.clone();
    memory.reject_proposal(&first).unwrap();
    assert_eq!(apply_reflection(&memory, &mut state, &app, &found), 0);
}

#[test]
fn strip_personal_takes_emails_homes_names_and_secrets() {
    let text = "Fixed by Sam Lee (sam.lee@example.com) in /Users/samlee/code/app/build.sh with token ghp_abcdefghijklmnopqrstuvwxyz0123456789; reviewed by @samlee-dev";
    let out = strip_personal(text, &["Sam Lee".into(), "samlee-dev".into()]);
    for gone in ["Sam Lee", "sam.lee@example.com", "/Users/samlee", "ghp_abcdefghijklmnopqrstuvwxyz0123456789", "samlee-dev"] {
        assert!(!out.contains(gone), "{gone} in {out}");
    }
    assert!(out.contains("~/code/app/build.sh") && out.contains("[email]") && out.contains("[someone]"), "{out}");
}

fn ci(kind: crate::journal::EventKind, project: &ProjectKey, failed: &[&str]) -> Event {
    Event::new(kind, From::Forge, "acme/app#7").project(Some(project)).session(Some("s-1")).data(json!({ "repo": "acme/app", "number": 7, "head": "abc", "failed": failed }))
}

fn turn(project: &ProjectKey, commits: &[&str], files: &[&str]) -> Event {
    let files: Vec<Value> = files.iter().map(|p| json!({ "path": p, "added": 1, "removed": 1 })).collect();
    Event::new(kinds::TURN_ENDED, From::App, "completed").project(Some(project)).session(Some("s-1")).data(json!({ "commits": commits, "files": files }))
}

#[test]
fn a_fixed_failure_gives_knowhow_without_personal_data_and_leaves_locked_files() {
    let scratch = Scratch::new();
    let memory = Memory::open();
    let _here = project_here(&scratch);
    let app = key(APP);
    let events = vec![
        ci(kinds::CI_FAILED, &app, &["test (node 18)"]),
        turn(&app, &["a1b2c3d Pin Node to 20; the test runner needs it (sam@example.com)"], &[".nvmrc", "/Users/samlee/app/package.json"]),
        ci(kinds::CI_PASSED, &app, &[]),
        // A failure with no commit between is no fix.
        ci(kinds::CI_FAILED, &app, &["lint"]),
        ci(kinds::CI_PASSED, &app, &[]),
    ];
    let found = fixed_failures(&events);
    assert!(found.open_since.is_none());
    let fixes = found.fixes;
    assert_eq!(fixes.len(), 1, "{fixes:?}");
    let input = knowhow_input(&fixes[0], &["Sam Lee".into()]);
    assert!(input.contains("Pin Node to 20") && input.contains("test (node 18)") && !input.contains("sam@example.com") && !input.contains("/Users/samlee"), "{input}");
    let reply = json!({ "structured_output": { "knowhow": [{ "topic": "CI", "text": "The tests need Node 20: keep .nvmrc at 20." }] } }).to_string();
    let lines = parse_knowhow(&reply).unwrap();
    let mut state = LearningState::default();
    assert_eq!(apply_knowhow(&memory, &mut state, &fixes[0], &lines, &[]), 1);
    assert!(proposals(&memory).iter().any(|e| e.text == "The tests need Node 20: keep .nvmrc at 20." && e.header.to.as_deref() == Some(MemFile::Knowhow(app.clone(), "ci".into()).rel().as_str())));
    // A know-how file the person edited is locked.
    let tests_file = MemFile::Knowhow(app.clone(), "tests".into());
    memory.learn(&Who::Session("s-1".into()), &tests_file, None, "Run the slow tests nightly.", None).unwrap();
    let edited = memory.snapshot().unwrap().doc(&tests_file).unwrap().entries().next().unwrap().header.id.clone();
    let mut doc = memory.snapshot().unwrap().doc(&tests_file).unwrap();
    let mut mine = doc.entries().next().unwrap().clone();
    mine.header.source = Source::PersonEdit;
    doc.replace(&edited, mine);
    std::fs::write(memory.dir().join(tests_file.rel()), doc.render()).unwrap();
    memory.accept(&[tests_file.rel()]).unwrap();
    assert!(locked(&memory, &tests_file));
    let other = vec![("tests".to_string(), "Run the fast tests first.".to_string())];
    assert_eq!(apply_knowhow(&memory, &mut state, &fixes[0], &other, &[]), 0);
}

#[test]
fn a_run_stops_above_the_budget_and_goes_on_from_where_it_was() {
    let scratch = Scratch::new();
    let memory = Memory::open();
    let here = project_here(&scratch);
    let words = vec![typed(1, "We always deploy the app from the release branch, never from main.", Some(APP))];
    let now = 1_795_000_000_000;
    let mut ledger = Ledger { daily_cap: 10, ..Ledger::default() };
    ledger.record_named("learning", crate::runtime::Usage { input: 10, ..Default::default() }, now);
    let calls = std::cell::Cell::new(0);
    let mut model = |_: &str, _: &Value| {
        calls.set(calls.get() + 1);
        Ok(json!({ "structured_output": { "facts": [] }, "usage": { "input_tokens": 100, "output_tokens": 10 } }).to_string())
    };
    // Read from the start (a state that began long ago).
    let mut state = LearningState { words_ms: 1, failures_ms: 1, ..LearningState::default() };
    let inputs = || Inputs { now_ms: now, projects: std::slice::from_ref(&here), names: &[], words: words.clone(), events: Vec::new() };
    let report = run(&mut state, &memory, &mut ledger, inputs(), Some(&mut model));
    assert!(report.stopped.is_some() && report.repaired.is_some() && report.tidied.is_some());
    assert_eq!((calls.get(), state.words_ms), (0, 1), "nothing read, the cursor stays");
    // Raised: it reads, the cursor moves, and the repair waits its hour.
    ledger.daily_cap = 1_000_000;
    let report = run(&mut state, &memory, &mut ledger, inputs(), Some(&mut model));
    assert!(report.stopped.is_none() && report.repaired.is_none() && report.tidied.is_none(), "{report:?}");
    assert_eq!((calls.get(), state.words_ms), (1, 1_001));
    assert!(ledger.today(now).get("learning").copied().unwrap_or(0) > 10);
    // Without a model (a Codex brain), only what needs none runs; a new state reads three days back.
    let mut fresh = LearningState::default();
    let report = run(&mut fresh, &memory, &mut ledger, inputs(), None);
    assert!(report.repaired.is_some() && fresh.words_ms == now - START_BACK_MS);
    // The state keeps no text.
    fresh.save().unwrap();
    assert!(!std::fs::read_to_string(LearningState::path()).unwrap().contains("release branch"));
}

#[test]
fn the_learning_profile_is_read_only_on_the_cheap_model() {
    let profile = profile(crate::model::AgentKind::Claude, &reflect_schema(), Path::new("/s.json"), Path::new("/a.json"));
    let args = profile.claude_args.join(" ");
    assert!(profile.claude_args.windows(2).any(|w| w == ["--tools", ""]), "{args}");
    assert!(profile.claude_args.windows(2).any(|w| w == ["--model", MODEL]), "{args}");
    assert!(!args.contains("--mcp-config"));
}

/// A stand-in model: facts for a reflect run (one on the first record it was sent), know-how for a
/// know-how run; it counts its calls.
fn stand_in<'a>(calls: &'a std::cell::Cell<usize>) -> impl FnMut(&str, &Value) -> Result<String, String> + 'a {
    move |input: &str, schema: &Value| {
        calls.set(calls.get() + 1);
        let usage = json!({ "input_tokens": 100, "output_tokens": 10 });
        if schema["properties"].get("knowhow").is_some() {
            return Ok(json!({ "structured_output": { "knowhow": [{ "topic": "ci", "text": "The tests need Node 20." }] }, "usage": usage }).to_string());
        }
        let id = input.split("words ").filter_map(|rest| rest.get(..24)).find(|id| id.chars().all(|c| c.is_ascii_hexdigit())).unwrap_or_default().to_string();
        // The record's own sentence, as the fact.
        let said = input.split("Remember that ").nth(1).and_then(|rest| rest.split('.').next()).unwrap_or_default().to_string();
        Ok(json!({ "structured_output": { "facts": [{ "record": id, "quote": "we always deploy from the release branch", "fact": format!("{said}."), "file": "profile" }] }, "usage": usage }).to_string())
    }
}

#[test]
fn a_fix_that_comes_an_hour_later_is_still_paired_with_its_failure() {
    let scratch = Scratch::new();
    let memory = Memory::open();
    let here = project_here(&scratch);
    let app = key(APP);
    let mut stamp = 10_000u64;
    let mut at = |mut event: Event| {
        stamp += 1_000;
        event.at.wall_ms = stamp;
        event
    };
    let first = vec![at(ci(kinds::CI_FAILED, &app, &["test"])), at(turn(&app, &["a1b2c3d Pin Node to 20"], &[".nvmrc"]))];
    let second = vec![at(ci(kinds::CI_PASSED, &app, &[]))];
    let calls = std::cell::Cell::new(0);
    let mut model = stand_in(&calls);
    let mut state = LearningState { words_ms: 1, failures_ms: 1, ..LearningState::default() };
    let mut ledger = Ledger::default();
    let now = 1_795_000_000_000;
    let report = run(&mut state, &memory, &mut ledger, Inputs { now_ms: now, projects: std::slice::from_ref(&here), names: &[], words: Vec::new(), events: first.clone() }, Some(&mut model));
    assert_eq!((report.knowhow, calls.get()), (0, 0));
    assert!(state.failures_ms < first[0].at_ms(), "the open failure is read again");
    // The next hour: the journal from the cursor on, the pass among it.
    let events: Vec<Event> = first.iter().chain(second.iter()).filter(|e| e.at_ms() > state.failures_ms).cloned().collect();
    let report = run(&mut state, &memory, &mut ledger, Inputs { now_ms: now + 3_600_000, projects: std::slice::from_ref(&here), names: &[], words: Vec::new(), events: events.clone() }, Some(&mut model));
    assert_eq!((report.knowhow, calls.get()), (1, 1));
    assert_eq!(state.failures_ms, second[0].at_ms());
    // Read again, it is not drafted twice.
    let mut again = LearningState { failures_ms: 1, ..state.clone() };
    run(&mut again, &memory, &mut ledger, Inputs { now_ms: now + 7_200_000, projects: std::slice::from_ref(&here), names: &[], words: Vec::new(), events }, Some(&mut model));
    assert_eq!(calls.get(), 1);
}

#[test]
fn words_stopped_between_projects_are_read_again_not_lost() {
    let scratch = Scratch::new();
    let memory = Memory::open();
    let here = project_here(&scratch);
    let words = vec![
        typed(1, "Remember that we always deploy from the release branch here.", Some(APP)),
        typed(2, "Remember that we always deploy from the release branch everywhere.", None),
        typed(3, "Remember that we always deploy from the release branch for the app.", Some(APP)),
    ];
    let calls = std::cell::Cell::new(0);
    let mut model = stand_in(&calls);
    let now = 1_795_000_000_000;
    // Room for one run only: learning's share is a fifth of the cap.
    let mut ledger = Ledger { daily_cap: 500, ..Ledger::default() };
    let mut state = LearningState { words_ms: 1, failures_ms: 1, ..LearningState::default() };
    let inputs = |words: &Vec<WordsRecord>| Inputs { now_ms: now, projects: std::slice::from_ref(&here), names: &[], words: words.clone(), events: Vec::new() };
    let report = run(&mut state, &memory, &mut ledger, inputs(&words), Some(&mut model));
    assert_eq!(calls.get(), 1);
    assert!(report.stopped.as_deref().is_some_and(|why| why.contains("share")), "{report:?}");
    assert_eq!(state.words_ms, 1, "the window waits whole");
    // Tomorrow, with room: both batches run, the one that ran proposes nothing twice.
    ledger.daily_cap = 1_000_000;
    let before = proposals(&memory).len();
    let report = run(&mut state, &memory, &mut ledger, Inputs { now_ms: now + 86_400_000, ..inputs(&words) }, Some(&mut model));
    assert!(report.stopped.is_none(), "{report:?}");
    assert_eq!(calls.get(), 3);
    assert_eq!(state.words_ms, 1_003);
    assert_eq!(proposals(&memory).len(), before + 1, "the second batch's fact only");
}
