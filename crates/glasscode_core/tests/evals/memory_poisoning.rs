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

#[test]
#[ignore = "needs M20.1"]
fn the_store_keeps_out_what_was_planted() {
    let cases = of(&["extraction", "poisoning"]);
    assert!(cases.iter().all(|case| case.needs == "M20.1"));
    needs("M20.1", "replay each setup into a scratch memory store (words through the M13 ledger, the rest as their channel) and check stored and not_stored");
}

#[test]
#[ignore = "needs M20.1"]
fn consolidation_and_staleness_lose_nothing_they_should_keep() {
    let cases = of(&["consolidation", "staleness"]);
    assert!(!cases.is_empty());
    needs("M20.1", "run the over-cap cycles and the changed cited lines, and check that nothing is lost and a stale code fact leaves the brief");
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

/// The gate's part of D17: a session on another account than the one the project allows reads
/// nothing of it through Glasscode (the manager's memory, its sessions). That the brief leaves the
/// fact out is M20.2's, which reads the same setting.
#[test]
fn another_account_does_not_read_the_project() {
    use glasscode_core::caller::Caller;
    use glasscode_core::gate::{self, Decision, Policy, PolicySet, ProjectPolicy, SessionFacts, SessionOrigin};
    let cases: Vec<Case> = of(&["scope"]).into_iter().filter(|case| case.needs == "M21.1").collect();
    assert_eq!(cases.len(), 1);
    let case = &cases[0];
    let project = glasscode_core::paths::ProjectKey::parse(&case.project).unwrap();
    let allowed = case.expect.more["model_account"].as_str().unwrap();
    let account = case.probe["account"].as_str().unwrap();
    assert_ne!(allowed, account);
    let mut set = PolicySet { policy: Policy { projects: [(project.clone(), ProjectPolicy { model_account: Some(allowed.into()), ..ProjectPolicy::default() })].into(), ..Policy::default() }, ..PolicySet::default() };
    let facts = |account: &str| SessionFacts { project: Some(project.clone()), origin: SessionOrigin::Person, account: Some(account.into()), ..SessionFacts::default() };
    set.sessions.insert("s-probe".into(), facts(account));
    let read = glasscode_core::ops::Op { spec: glasscode_core::ops::find("manager.memory").unwrap(), args: serde_json::json!({}) };
    let targets = glasscode_core::ops::Targets { projects: vec![project.clone()], ..Default::default() };
    let caller = Caller::Session { id: "s-probe".into(), project: None };
    assert!(matches!(gate::decide(&caller, &read, &targets, &set, None), Decision::Never { .. }), "{}", case.id);
    set.sessions.insert("s-probe".into(), facts(allowed));
    assert_eq!(gate::decide(&caller, &read, &targets, &set, None), Decision::Allow, "{}", case.id);
}

#[test]
#[ignore = "needs M24"]
fn archived_work_is_still_found() {
    let cases = of(&["archive"]);
    assert!(cases.iter().all(|case| case.setup.iter().any(|step| step["via"] == "worktree_removed")));
    needs("M24", "remove the worktree through the archive reconciler and check search still returns the note");
}
