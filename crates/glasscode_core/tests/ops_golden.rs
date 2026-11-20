//! The golden corpus of the registry's parser (M12.1): command lines as the app answered them
//! before the registry, recorded on a scratch home, each with the op or error `ops::parse` gives
//! (`tests/ops_golden/*.json`; their header says how they were recorded).
//!
//! A case passes when `parse` gives exactly its op or error. Where `v1` is there and no `note`
//! says why not, the parse also agrees with what the app said: the same error text and exit code,
//! a read that answered (exit 0), or a change that waited for the person (exit 4).
//!
//! A case that `differs` has only what v1 said: there the registry answers otherwise on purpose,
//! in words of its own that no golden freezes (their unit tests pin them). The parse must not
//! agree with v1 there, and where `agrees_as_person` says so, it does agree for the person.
//!
//! Every word that names an op (each noun and verb with each of its other words, and a verb left
//! out) starts at least one case.

use std::collections::BTreeSet;

use glasscode_core::control::{Args, Kind, Rule};
use glasscode_core::ops::{NOUNS, OPS, Op, OpError, ParseCtx, SessionView, find, parse};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Golden {
    ctx: Ctx,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Ctx {
    today: String,
    sessions: Vec<SessionView>,
    /// The engine switch is v2: the ops only it knows are known.
    #[serde(default)]
    v2: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    /// The line of HELP this case is an example of.
    help: Option<String>,
    words: Vec<String>,
    /// "outside" (the default), "person", or "session:<id>".
    caller: Option<String>,
    op: Option<Value>,
    error: Option<Value>,
    v1: Option<V1>,
    note: Option<String>,
    differs: Option<bool>,
    agrees_as_person: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1 {
    exit: i32,
    says: Option<String>,
    first_line: Option<String>,
}

fn goldens() -> Vec<(String, Golden)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("ops_golden");
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    files.sort();
    assert!(files.len() >= 3, "{files:?}");
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            let golden = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            (path.file_name().unwrap().to_string_lossy().into_owned(), golden)
        })
        .collect()
}

/// The words of a case that name its op, in lower case: its words without options.
fn naming(words: &[String]) -> Vec<String> {
    Args::parse(words).words.iter().map(|w| w.to_lowercase()).collect()
}

fn ctx_for(ctx: &Ctx, caller: Option<&str>) -> ParseCtx {
    let caller = caller.unwrap_or("outside");
    assert!(matches!(caller, "outside" | "person") || caller.starts_with("session:"), "caller {caller}");
    ParseCtx {
        caller_is_person: caller == "person",
        caller_session: caller.strip_prefix("session:").map(str::to_string),
        today: glasscode_core::day::parse_iso(&ctx.today).expect("ctx.today is a day"),
        sessions: ctx.sessions.clone(),
        v2: ctx.v2,
    }
}

#[test]
fn ops_golden() {
    let mut failures = Vec::new();
    let mut seen_ops = BTreeSet::new();
    let mut seen_help = BTreeSet::new();
    let mut spelled: BTreeSet<(Vec<String>, String)> = BTreeSet::new();
    for (file, golden) in goldens() {
        assert!(!golden.cases.is_empty(), "{file}");
        for case in &golden.cases {
            let at = format!("{file} {:?}", case.words);
            let ctx = ctx_for(&golden.ctx, case.caller.as_deref());
            if case.differs == Some(true) {
                differs_from_v1(&at, case, &golden.ctx, &mut failures);
                continue;
            }
            assert!(case.agrees_as_person.is_none(), "{at}: agrees_as_person is for a case that differs");
            let (expected, got) = match (&case.op, &case.error, parse(&case.words, &ctx)) {
                (Some(op), None, result) => (json!({ "op": op }), result.map(|o| json!({ "op": o })).unwrap_or_else(|e| json!({ "error": e }))),
                (None, Some(error), result) => (json!({ "error": error }), result.map(|o| json!({ "op": o })).unwrap_or_else(|e| json!({ "error": e }))),
                _ => panic!("{at}: a case has an op or an error"),
            };
            if expected != got {
                failures.push(format!("{at}\n  expected {expected}\n  got      {got}"));
                continue;
            }
            if let Some(op) = &case.op {
                let name = op["op"].as_str().unwrap().to_string();
                if let Some(help) = &case.help {
                    let spec = find(&name).unwrap();
                    assert!(spec.examples.contains(&help.as_str()), "{at}: “{help}” is not an example of {name}");
                    seen_help.insert((name.clone(), help.clone()));
                }
                spelled.extend(spellings_of(find(&name).unwrap(), &naming(&case.words)));
                seen_ops.insert(name);
            } else {
                assert!(case.help.is_none(), "{at}: an example is an op");
            }
            agrees_with_v1(&at, case, &mut failures);
        }
    }
    assert!(failures.is_empty(), "{} of the golden cases fail:\n{}", failures.len(), failures.join("\n"));
    // Every op, and every example of HELP, has its case.
    for op in OPS {
        assert!(seen_ops.contains(op.name), "no golden case for {}", op.name);
        for example in op.examples {
            assert!(seen_help.contains(&(op.name.to_string(), example.to_string())), "no golden case is the example “{example}” of {}", op.name);
        }
    }
    let unspelled: Vec<String> = every_spelling().into_iter().filter(|s| !spelled.contains(s)).map(|(path, word)| format!("{path:?} {word:?}")).collect();
    assert!(unspelled.is_empty(), "no golden case names an op with these words (the words before, the word):\n{}", unspelled.join("\n"));
}

/// Each word that can stand at each place of an op's words: (the words before it, the word). A
/// verb that may be left out is `""`, any other word for it `"*"`.
fn every_spelling() -> BTreeSet<(Vec<String>, String)> {
    let mut out = BTreeSet::new();
    for noun in NOUNS {
        let before: Vec<String> = noun.path[..noun.path.len() - 1].iter().map(|w| w.to_string()).collect();
        for word in std::iter::once(noun.path[noun.path.len() - 1]).chain(noun.also.iter().copied()) {
            out.insert((before.clone(), word.to_string()));
        }
    }
    for op in OPS {
        let before: Vec<String> = op.cli[..op.cli.len() - 1].iter().map(|w| w.to_string()).collect();
        for word in std::iter::once(op.cli[op.cli.len() - 1]).chain(op.aliases.iter().copied()) {
            out.insert((before.clone(), word.to_string()));
        }
    }
    out
}

/// The spellings a case's words use for its op.
fn spellings_of(op: &glasscode_core::ops::OpSpec, words: &[String]) -> Vec<(Vec<String>, String)> {
    let mut out = Vec::new();
    for (depth, canonical) in op.cli.iter().enumerate() {
        let before: Vec<String> = op.cli[..depth].iter().map(|w| w.to_string()).collect();
        let last = depth + 1 == op.cli.len();
        match words.get(depth) {
            None => {
                if last {
                    out.push((before, String::new()));
                }
                break;
            }
            Some(word) if last && *word != *canonical && !op.aliases.contains(&word.as_str()) => out.push((before, "*".to_string())),
            Some(word) => out.push((before, word.clone())),
        }
    }
    out
}

/// What the app said agrees with this parse: the same error and exit code, or a read that
/// answered (exit 0), or a change that waited for a caller other than the person (exit 4), or one
/// Settings › Permissions never lets agents do (exit 5).
fn agrees(result: &Result<Op, OpError>, v1: &V1, person: bool) -> Result<(), String> {
    match result {
        Err(error) => match (v1.exit, v1.says.as_deref()) == (error.exit_code(), Some(error.text.as_str())) {
            true => Ok(()),
            false => Err(format!("v1 said {:?} (exit {}), parse says {:?} (exit {})", v1.says, v1.exit, error.text, error.exit_code())),
        },
        Ok(op) => {
            let wanted = match (person, op.spec.kind_v1) {
                (true, _) | (_, Kind::Read) => 0,
                (false, kind) if kind.default_rule() == Rule::Never => 5,
                _ => 4,
            };
            if v1.exit != wanted {
                return Err(format!("v1 exited {} where {} (kind {}) gives {wanted}", v1.exit, op.name(), op.spec.kind_v1.key()));
            }
            // A session the app showed is the one the parse names.
            if let (Some(line), Some(id)) = (&v1.first_line, op.args["session"].as_str())
                && !line.contains(id)
            {
                return Err(format!("v1 showed {line:?}, parse names {id}"));
            }
            Ok(())
        }
    }
}

/// A case where the registry answers otherwise than v1 on purpose.
fn differs_from_v1(at: &str, case: &Case, ctx: &Ctx, failures: &mut Vec<String>) {
    assert!(case.op.is_none() && case.error.is_none() && case.help.is_none(), "{at}: a case that differs has only v1's answer");
    let (Some(v1), Some(note)) = (&case.v1, &case.note) else { panic!("{at}: a case that differs has v1 and a note") };
    assert!(note.len() > 20, "{at}: say why it differs");
    let person = case.caller.as_deref() == Some("person");
    if agrees(&parse(&case.words, &ctx_for(ctx, case.caller.as_deref())), v1, person).is_ok() {
        failures.push(format!("{at}: parse agrees with v1, where the case says it differs ({note})"));
    }
    if case.agrees_as_person == Some(true)
        && let Err(why) = agrees(&parse(&case.words, &ctx_for(ctx, Some("person"))), v1, person)
    {
        failures.push(format!("{at}: for the person, parse should give what v1 gave: {why}"));
    }
}

/// What the app answered agrees with the parse, unless the case's note says why not.
fn agrees_with_v1(at: &str, case: &Case, failures: &mut Vec<String>) {
    let Some(v1) = &case.v1 else {
        assert!(case.note.is_none(), "{at}: a note explains a difference from v1");
        return;
    };
    if let Some(note) = &case.note {
        assert!(note.len() > 20, "{at}: say why it differs");
        return;
    }
    let result = match (&case.op, &case.error) {
        (Some(op), None) => Ok(serde_json::from_value::<Op>(op.clone()).unwrap()),
        (None, Some(error)) => Err(serde_json::from_value::<OpError>(error.clone()).unwrap()),
        _ => unreachable!(),
    };
    if let Err(why) = agrees(&result, v1, case.caller.as_deref() == Some("person")) {
        failures.push(format!("{at}: {why}"));
    }
}
