//! The golden corpus of the registry's parser (M12.1): command lines as the app answered them
//! before the registry, recorded on a scratch home, each with the op or error `ops::parse` gives
//! (`tests/ops_golden/*.json`; their header says how they were recorded).
//!
//! A case passes when `parse` gives exactly its op or error. Where `v1` is there and no `note`
//! says why not, the parse also agrees with what the app said: the same error text and exit code,
//! a read that answered (exit 0), or a change that waited for the person (exit 4).

use std::collections::BTreeSet;

use glasscode_core::control::Kind;
use glasscode_core::ops::{OPS, OpError, ParseCtx, SessionView, find, parse};
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

fn ctx_for(ctx: &Ctx, caller: Option<&str>) -> ParseCtx {
    let caller = caller.unwrap_or("outside");
    assert!(matches!(caller, "outside" | "person") || caller.starts_with("session:"), "caller {caller}");
    ParseCtx {
        caller_is_person: caller == "person",
        caller_session: caller.strip_prefix("session:").map(str::to_string),
        today: glasscode_core::day::parse_iso(&ctx.today).expect("ctx.today is a day"),
        sessions: ctx.sessions.clone(),
    }
}

#[test]
fn ops_golden() {
    let mut failures = Vec::new();
    let mut seen_ops = BTreeSet::new();
    let mut seen_help = BTreeSet::new();
    for (file, golden) in goldens() {
        assert!(!golden.cases.is_empty(), "{file}");
        for case in &golden.cases {
            let at = format!("{file} {:?}", case.words);
            let ctx = ctx_for(&golden.ctx, case.caller.as_deref());
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
    let person = case.caller.as_deref() == Some("person");
    match (&case.op, &case.error) {
        (None, Some(error)) => {
            let error: OpError = serde_json::from_value(error.clone()).unwrap();
            if (v1.exit, v1.says.as_deref()) != (error.exit_code(), Some(error.text.as_str())) {
                failures.push(format!("{at}: v1 said {:?} (exit {}), parse says {:?} (exit {})", v1.says, v1.exit, error.text, error.exit_code()));
            }
        }
        (Some(op), None) => {
            let spec = find(op["op"].as_str().unwrap()).unwrap();
            // The person runs anything; any other caller runs reads, and everything else waits.
            let wanted = if person || spec.kind_v1 == Kind::Read { 0 } else { 4 };
            if v1.exit != wanted {
                failures.push(format!("{at}: v1 exited {} where kind {} gives {wanted}", v1.exit, spec.kind_v1.key()));
            }
            // A session the app showed is the one the parse names.
            if let (Some(line), Some(id)) = (&v1.first_line, op["args"]["session"].as_str()) {
                assert!(line.contains(id), "{at}: v1 showed {line:?}, parse names {id}");
            }
        }
        _ => unreachable!(),
    }
}
