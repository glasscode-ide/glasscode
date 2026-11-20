//! Op selection (ops-23): natural requests and the op, with its arguments, that a brain should
//! choose; two or more for each op; and requests that must choose no op (planted text from outside,
//! small talk).
//!
//! `fixtures/op_selection/cases.jsonl`, one case a line, and `context.json`, the projects, sessions,
//! board tasks and CI runs the arguments name. The op names are the plan's (design section 8's
//! groups); the registry slices (M12.1, M12.2, M33.2) register them, or rename them here.
//!
//! Without a model, this suite checks the examples themselves: M12.1 adds that each one parses
//! against its op's schema, and that each op's key nouns lead its description. How often a real
//! brain chooses right is measured by hand before a release (M55), never here.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use glasscode_core::ops;

use crate::{fixtures, read_jsonl, SLICES};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub request: String,
    pub source: String,
    pub expect: Option<Expected>,
    pub needs: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub op: String,
    pub args: serde_json::Map<String, Value>,
}

pub fn cases() -> Vec<Case> {
    read_jsonl(&fixtures().join("op_selection").join("cases.jsonl"))
}

pub fn context() -> Value {
    serde_json::from_slice(&std::fs::read(fixtures().join("op_selection").join("context.json")).unwrap()).unwrap()
}

/// The slices whose ops are registered. The checks against the registry take their cases; each
/// slice that registers ops (M12.2, M14, M20.2, M21.2, M33.2) adds itself. Slices land out of the plan's
/// order, so this is a list, not the last of them.
const LANDED: &[&str] = &["M12.1", "M12.2", "M14", "M30", "M33.2"];

/// The cases whose ops the registry has by now ([`LANDED`]).
fn landed() -> Vec<Case> {
    assert!(LANDED.iter().all(|slice| SLICES.contains(slice)), "{LANDED:?}");
    cases().into_iter().filter(|case| case.expect.is_some() && LANDED.contains(&case.needs.as_str())).collect()
}

/// The channels whose text must never select an op (the adversarial suite's).
const OUTSIDE: [&str; 7] = ["pr_comment", "review_thread", "ticket", "ci_log", "web_page", "message", "session_reply"];

fn op_name(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() >= 2 && parts.iter().all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
}

#[test]
fn every_op_has_examples_and_outside_text_selects_nothing() {
    let cases = cases();
    assert_eq!(cases.iter().map(|case| case.id.as_str()).collect::<BTreeSet<_>>().len(), cases.len(), "ids are unique");
    assert_eq!(cases.iter().map(|case| case.request.to_lowercase()).collect::<BTreeSet<_>>().len(), cases.len(), "requests are unique");
    let mut per_op: BTreeMap<&str, usize> = BTreeMap::new();
    for case in &cases {
        assert!(SLICES.contains(&case.needs.as_str()), "{}", case.id);
        match &case.expect {
            Some(expected) => {
                assert!(op_name(&expected.op), "{}: {}", case.id, expected.op);
                assert_eq!(case.source, "person", "{}: only the person's words select an op", case.id);
                assert!(expected.args.keys().all(|key| key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')), "{}", case.id);
                *per_op.entry(expected.op.as_str()).or_insert(0) += 1;
            }
            None => assert!(OUTSIDE.contains(&case.source.as_str()) || case.source == "person", "{}: {}", case.id, case.source),
        }
    }
    for (op, count) in &per_op {
        assert!(*count >= 2, "{op} has {count} example; ops-23 asks two or three");
    }
    let outside: BTreeSet<&str> = cases.iter().filter(|case| case.expect.is_none() && case.source != "person").map(|case| case.source.as_str()).collect();
    assert_eq!(outside, OUTSIDE.into_iter().collect(), "text from every outside channel selects no op");
    assert!(cases.iter().any(|case| case.expect.is_none() && case.source == "person"), "small talk selects no op");
    assert!(per_op.len() >= 20, "{} ops", per_op.len());
}

/// Arguments name only what the context has: a session, a task, a project, a repository, a run.
#[test]
fn arguments_name_what_the_context_has() {
    let context = context();
    let ids = |list: &str, key: &str| -> BTreeSet<String> { context[list].as_array().unwrap().iter().map(|item| item[key].to_string().trim_matches('"').to_string()).collect() };
    let (sessions, tasks, projects, runs) = (ids("sessions", "id"), ids("board", "id"), ids("projects", "name"), ids("runs", "id"));
    let repos = ids("projects", "repo");
    for case in cases() {
        let Some(expected) = case.expect else { continue };
        for (key, value) in &expected.args {
            let value = value.to_string().trim_matches('"').to_string();
            let known = match key.as_str() {
                "session" => sessions.contains(&value),
                "task" => tasks.contains(&value),
                "project" => projects.contains(&value),
                "repo" => repos.contains(&value),
                "run" => runs.contains(&value),
                _ => true,
            };
            assert!(known, "{}: {key} {value} is not in context.json", case.id);
        }
    }
}

/// The cases of the ops registered by [`LANDED`] and the slices before it: each expected op is in
/// the registry, and its arguments are what the op's schema allows (what a typed caller sends).
#[test]
fn every_example_parses_against_its_op() {
    let cases = landed();
    assert!(LANDED.iter().all(|slice| cases.iter().any(|case| case.needs == *slice)), "each of {LANDED:?} has cases");
    for case in cases {
        let expected = case.expect.expect("landed() keeps the cases with an op");
        let op = ops::typed(&expected.op, Value::Object(expected.args.clone())).unwrap_or_else(|error| panic!("{}: {}", case.id, error.text));
        assert_eq!(op.name(), expected.op);
    }
}

/// As above: the ops registered by [`LANDED`] and the slices before it. An op's key nouns (the
/// words of its name before the verb: `session` of `session.open`) are in the first 200
/// characters of what a typed caller reads of it (ops-23).
#[test]
fn each_ops_description_leads_with_its_key_nouns() {
    let names: BTreeSet<String> = landed().into_iter().filter_map(|case| case.expect.map(|e| e.op)).collect();
    assert!(!names.is_empty());
    for name in names {
        let spec = ops::OPS.iter().find(|op| op.name == name).unwrap();
        let description = ops::description(spec).to_lowercase();
        let lead: String = description.chars().take(200).collect();
        let parts: Vec<&str> = name.split('.').collect();
        for noun in &parts[..parts.len() - 1] {
            let noun = noun.replace('_', "-");
            assert!(lead.contains(&noun), "{name}: “{noun}” is not in the first 200 characters of “{description}”");
        }
    }
}
