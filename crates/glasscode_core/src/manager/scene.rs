//! Pictures the manager explains with: read from the JSON the brain writes, checked, and a
//! few built in.

use std::collections::HashSet;

use super::{Scene, SceneKind, SceneStep};

/// What still draws well in the small space a scene gets.
pub const MAX_STEPS: usize = 12;
pub const MAX_NODES: usize = 14;
pub const MAX_EDGES: usize = 24;
pub const MAX_CODE_LINES: usize = 60;
const MAX_TITLE: usize = 80;
const MAX_LABEL: usize = 40;
const MAX_NOTE: usize = 120;
const MAX_SAY: usize = 280;
const MAX_CODE_WIDTH: usize = 160;
const MAX_SIDE: usize = 600;
const MAX_FILE: usize = 120;
/// How many problems one error names: enough to fix them in one go, not a wall of text.
const MAX_PROBLEMS: usize = 6;

/// Read a scene from JSON and check it (ids exist, indexes in range, at least one step, not
/// too large to draw). The error says what is wrong in words the brain can act on.
///
/// Text around the JSON (a sentence before it, a code fence) is ignored.
pub fn parse(json: &str) -> anyhow::Result<Scene> {
    let text = match (json.find('{'), json.rfind('}')) {
        (Some(start), Some(end)) if start < end => &json[start..=end],
        _ => anyhow::bail!("a scene is one JSON object ({{\"title\": …, \"kind\": …, \"steps\": […]}}), and there is none here"),
    };
    let scene: Scene = serde_json::from_str(text).map_err(|err| anyhow::anyhow!("the scene's JSON cannot be read: {err}. {}", hint(&err.to_string())))?;
    check(&scene)?;
    Ok(scene)
}

/// What usually went wrong when the JSON has the wrong shape.
fn hint(error: &str) -> &'static str {
    if error.contains("unknown variant") {
        "\"kind\" is one of \"flow\", \"sequence\", \"code\", \"compare\"."
    } else if error.contains("expected usize") || error.contains("expected u64") {
        "In a step, \"edges\" are positions in the scene's \"edges\" list and \"lines\" are line numbers of \"code\", both whole numbers from 0 (node ids go in \"nodes\")."
    } else if error.contains("expected a string") {
        "Ids, labels and every line of \"code\" are strings; a step's \"nodes\" are node ids."
    } else if error.contains("expected a sequence") {
        "\"nodes\", \"edges\", \"code\" and \"steps\" are lists."
    } else {
        "Write one plain JSON object, with double quotes and no comments."
    }
}

/// Check a scene that is already read: everything `parse` checks. The error lists what to
/// change, each with the place it is in (`steps[2].nodes`).
pub fn check(scene: &Scene) -> anyhow::Result<()> {
    let mut wrong: Vec<String> = Vec::new();
    let long = |text: &str, max: usize| text.chars().count() > max;

    if long(&scene.title, MAX_TITLE) {
        wrong.push(format!("the title has {} characters; keep it under {MAX_TITLE}", scene.title.chars().count()));
    }

    // Nodes.
    if scene.nodes.len() > MAX_NODES {
        wrong.push(format!("there are {} nodes; a scene draws at most {MAX_NODES}, so merge or drop some", scene.nodes.len()));
    }
    let mut ids: HashSet<&str> = HashSet::new();
    for (i, node) in scene.nodes.iter().enumerate() {
        if node.id.trim().is_empty() {
            wrong.push(format!("nodes[{i}] has no \"id\"; give every node a short id"));
        } else if !ids.insert(node.id.as_str()) {
            wrong.push(format!("nodes[{i}] uses the id \"{}\" a second time; ids must differ", node.id));
        }
        if node.label.trim().is_empty() {
            wrong.push(format!("nodes[{i}] (\"{}\") has no \"label\"; say in a word or two what it is", node.id));
        } else if long(&node.label, MAX_LABEL) {
            wrong.push(format!("nodes[{i}].label has {} characters; keep it under {MAX_LABEL} and put the rest in \"note\"", node.label.chars().count()));
        }
        if long(&node.note, MAX_NOTE) {
            wrong.push(format!("nodes[{i}].note has {} characters; keep it under {MAX_NOTE}", node.note.chars().count()));
        }
    }
    let known = |id: &str| ids.contains(id);
    let node_ids = || if scene.nodes.is_empty() { "there are no nodes".to_string() } else { format!("the node ids are {}", scene.nodes.iter().map(|n| format!("\"{}\"", n.id)).collect::<Vec<_>>().join(", ")) };

    // Edges.
    if scene.edges.len() > MAX_EDGES {
        wrong.push(format!("there are {} edges; a scene draws at most {MAX_EDGES}", scene.edges.len()));
    }
    for (i, edge) in scene.edges.iter().enumerate() {
        for (end, id) in [("from", &edge.from), ("to", &edge.to)] {
            if !known(id) {
                wrong.push(format!("edges[{i}].{end} is \"{id}\", which is not a node; {}", node_ids()));
            }
        }
        if long(&edge.label, MAX_LABEL) {
            wrong.push(format!("edges[{i}].label has {} characters; keep it under {MAX_LABEL}", edge.label.chars().count()));
        }
    }

    // Code and the two sides.
    if scene.code.len() > MAX_CODE_LINES {
        wrong.push(format!("\"code\" has {} lines; show at most {MAX_CODE_LINES}, only the part you talk about", scene.code.len()));
    }
    if let Some((i, line)) = scene.code.iter().enumerate().find(|(_, l)| long(l, MAX_CODE_WIDTH) || l.contains('\n')) {
        wrong.push(if line.contains('\n') { format!("code[{i}] holds a line break; every line of code is its own string") } else { format!("code[{i}] has {} characters; keep lines under {MAX_CODE_WIDTH}", line.chars().count()) });
    }
    if long(&scene.file, MAX_FILE) {
        wrong.push(format!("\"file\" has {} characters; keep it under {MAX_FILE}", scene.file.chars().count()));
    }
    for (side, text) in [("before", &scene.before), ("after", &scene.after)] {
        if long(text, MAX_SIDE) {
            wrong.push(format!("\"{side}\" has {} characters; keep it under {MAX_SIDE}", text.chars().count()));
        }
    }

    // What each kind needs.
    match scene.kind {
        SceneKind::Flow | SceneKind::Sequence => {
            if scene.nodes.is_empty() {
                wrong.push(format!("a {} scene needs \"nodes\" (and \"edges\" between them)", kind_name(scene.kind)));
            }
        }
        SceneKind::Code => {
            if scene.code.is_empty() {
                wrong.push("a code scene needs \"code\": the lines of the file, one string each".into());
            }
        }
        SceneKind::Compare => {
            if scene.before.trim().is_empty() || scene.after.trim().is_empty() {
                wrong.push("a compare scene needs both \"before\" and \"after\"".into());
            }
        }
    }

    // Steps.
    if scene.steps.is_empty() {
        wrong.push("there are no steps; add at least one: \"steps\": [{\"say\": \"one sentence\"}]".into());
    }
    if scene.steps.len() > MAX_STEPS {
        wrong.push(format!("there are {} steps; a scene has at most {MAX_STEPS}, so say more in fewer steps", scene.steps.len()));
    }
    for (i, step) in scene.steps.iter().enumerate() {
        if step.say.trim().is_empty() {
            wrong.push(format!("steps[{i}] has no \"say\"; every step is one sentence of the explanation"));
        } else if long(&step.say, MAX_SAY) {
            wrong.push(format!("steps[{i}].say has {} characters; keep it under {MAX_SAY}, one or two sentences", step.say.chars().count()));
        }
        for id in step.nodes.iter().chain(&step.focus).filter(|id| !known(id)) {
            let place = if step.focus.as_ref() == Some(id) && !step.nodes.contains(id) { "focus" } else { "nodes" };
            wrong.push(format!("steps[{i}].{place} names \"{id}\", which is not a node; {}", node_ids()));
        }
        if let Some(edge) = step.edges.iter().find(|&&e| e >= scene.edges.len()) {
            wrong.push(match scene.edges.len() {
                0 => format!("steps[{i}].edges has {edge}, but the scene has no edges"),
                n => format!("steps[{i}].edges has {edge}, but the scene's edges are numbered 0 to {}", n - 1),
            });
        }
        if let Some(line) = step.lines.iter().find(|&&l| l >= scene.code.len()) {
            wrong.push(match scene.code.len() {
                0 => format!("steps[{i}].lines has {line}, but the scene has no \"code\""),
                n => format!("steps[{i}].lines has {line}, but \"code\" has lines 0 to {}", n - 1),
            });
        }
    }

    if wrong.is_empty() {
        return Ok(());
    }
    let more = wrong.len().saturating_sub(MAX_PROBLEMS);
    wrong.truncate(MAX_PROBLEMS);
    let mut text = format!("the scene cannot be drawn: {}", wrong.join("; "));
    if more > 0 {
        text.push_str(&format!("; and {more} more"));
    }
    anyhow::bail!(text)
}

fn kind_name(kind: SceneKind) -> &'static str {
    match kind {
        SceneKind::Flow => "flow",
        SceneKind::Sequence => "sequence",
        SceneKind::Code => "code",
        SceneKind::Compare => "compare",
    }
}

/// What the brain is told about writing a scene: the JSON shape, with one small example
/// for each kind. (A test reads every example back through `parse`.)
pub fn guide() -> &'static str {
    r#"A scene is one JSON object the app draws while you talk:
{"title","kind":"flow|sequence|code|compare","nodes":[{"id","label","note"}],"edges":[{"from","to","label","bad"}],"file","code":["line"],"before","after","steps":[{"say","nodes":["id"],"edges":[0],"lines":[0],"focus":"id"}]}
Rules: 1-12 steps, each "say" one sentence. At most 14 nodes with unique ids and short labels. Edges join node ids; "bad":true draws a broken arrow. A step lists the node ids and edge positions (from 0) shown so far; "focus" is the node it is about. code: at most 60 lines, a step lights "lines" (from 0). compare: fill "before" and "after". Leave out what the kind does not use.
flow: {"title":"Login","kind":"flow","nodes":[{"id":"app","label":"App"},{"id":"api","label":"API"}],"edges":[{"from":"app","to":"api","label":"token"}],"steps":[{"say":"The app sends its token.","nodes":["app","api"],"edges":[0],"focus":"api"}]}
sequence: {"title":"Save","kind":"sequence","nodes":[{"id":"ui","label":"UI"},{"id":"db","label":"DB"}],"edges":[{"from":"ui","to":"db","label":"write"},{"from":"db","to":"ui","label":"ok"}],"steps":[{"say":"The UI writes, the DB answers.","nodes":["ui","db"],"edges":[0,1]}]}
code: {"title":"Retry","kind":"code","file":"net.rs","code":["loop {","  send()?;","}"],"steps":[{"say":"It sends in a loop.","lines":[0,1]}]}
compare: {"title":"Cache","kind":"compare","before":"Every call hits the DB.","after":"Calls read the cache.","steps":[{"say":"Reads are cached now."}]}"#
}

/// A scene that explains a file's lines step by step, from the lines and which to light when.
///
/// What comes in is made to fit: at most `MAX_CODE_LINES` lines and `MAX_STEPS` steps, line
/// numbers outside the file dropped, long lines and sentences cut. The result always passes
/// `check`.
pub fn code_walk(title: &str, file: &str, lines: Vec<String>, steps: Vec<(String, Vec<usize>)>) -> Scene {
    let mut code: Vec<String> = lines.into_iter().take(MAX_CODE_LINES).map(|line| cut(line.trim_end().replace('\n', " ").replace('\t', "    "), MAX_CODE_WIDTH)).collect();
    if code.is_empty() {
        code.push(String::new());
    }
    let title = cut(title.trim().to_string(), MAX_TITLE);
    let mut walk: Vec<SceneStep> = steps
        .into_iter()
        .filter(|(say, _)| !say.trim().is_empty())
        .take(MAX_STEPS)
        .map(|(say, mut lit)| {
            lit.retain(|&l| l < code.len());
            lit.sort_unstable();
            lit.dedup();
            SceneStep { say: cut(say.trim().to_string(), MAX_SAY), lines: lit, ..Default::default() }
        })
        .collect();
    if walk.is_empty() {
        let say = if file.trim().is_empty() { "Here is the code.".to_string() } else { cut(format!("Here is {}.", file.trim()), MAX_SAY) };
        walk.push(SceneStep { say, ..Default::default() });
    }
    Scene { title, kind: SceneKind::Code, file: cut(file.trim().to_string(), MAX_FILE), code, steps: walk, ..Default::default() }
}

/// The text, cut to `max` characters with "…" when it was longer.
fn cut(text: String, max: usize) -> String {
    if text.chars().count() <= max { text } else { text.chars().take(max - 1).collect::<String>() + "…" }
}

#[cfg(test)]
mod tests {
    use super::super::{SceneEdge, SceneNode};
    use super::*;

    fn flow() -> serde_json::Value {
        serde_json::json!({
            "title": "How a review reaches the session",
            "kind": "flow",
            "nodes": [{ "id": "gh", "label": "GitHub", "note": "where Sam reviews" }, { "id": "mgr", "label": "Manager" }, { "id": "s", "label": "Your session" }],
            "edges": [{ "from": "gh", "to": "mgr", "label": "review" }, { "from": "mgr", "to": "s", "label": "tells it", "bad": false }],
            "steps": [{ "say": "Sam reviews on GitHub.", "nodes": ["gh"], "focus": "gh" }, { "say": "The manager sees it and tells your session.", "nodes": ["gh", "mgr", "s"], "edges": [0, 1], "focus": "s" }]
        })
    }

    /// The error for a flow scene changed by `change`.
    fn error_after(change: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut scene = flow();
        change(&mut scene);
        parse(&scene.to_string()).expect_err("this scene should not pass").to_string()
    }

    #[test]
    fn reads_a_good_scene() {
        let scene = parse(&flow().to_string()).unwrap();
        assert_eq!(scene.kind, SceneKind::Flow);
        assert_eq!(scene.nodes.len(), 3);
        assert_eq!(scene.nodes[0], SceneNode { id: "gh".into(), label: "GitHub".into(), note: "where Sam reviews".into() });
        assert_eq!(scene.edges[1], SceneEdge { from: "mgr".into(), to: "s".into(), label: "tells it".into(), bad: false });
        assert_eq!(scene.steps[1].edges, vec![0, 1]);
        assert_eq!(scene.steps[1].focus.as_deref(), Some("s"));
    }

    #[test]
    fn reads_the_json_out_of_the_text_around_it() {
        let wrapped = format!("Here is the picture:\n```json\n{}\n```\nThat is all.", flow());
        assert_eq!(parse(&wrapped).unwrap(), parse(&flow().to_string()).unwrap());
    }

    #[test]
    fn says_when_there_is_no_json_or_it_is_broken() {
        assert!(parse("no picture today").unwrap_err().to_string().contains("one JSON object"));
        assert!(parse("").unwrap_err().to_string().contains("one JSON object"));
        assert!(parse("}{").unwrap_err().to_string().contains("one JSON object"));
        let broken = parse(r#"{"title": "x", "steps": [{"say": "a"},]}"#).unwrap_err().to_string();
        assert!(broken.starts_with("the scene's JSON cannot be read:") && broken.contains("double quotes and no comments"), "{broken}");
    }

    #[test]
    fn says_what_is_wrong_with_the_shape() {
        let kind = error_after(|s| s["kind"] = "graph".into());
        assert!(kind.contains("unknown variant `graph`") && kind.contains("\"kind\" is one of \"flow\", \"sequence\", \"code\", \"compare\""), "{kind}");
        let edges = error_after(|s| s["steps"][1]["edges"] = serde_json::json!(["gh"]));
        assert!(edges.contains("positions in the scene's \"edges\" list"), "{edges}");
        let nodes = error_after(|s| s["steps"][0]["nodes"] = serde_json::json!([1]));
        assert!(nodes.contains("a step's \"nodes\" are node ids"), "{nodes}");
        let steps = error_after(|s| s["steps"] = "first this, then that".into());
        assert!(steps.contains("are lists"), "{steps}");
    }

    #[test]
    fn a_scene_needs_steps_and_not_too_many() {
        assert!(error_after(|s| s["steps"] = serde_json::json!([])).contains("there are no steps; add at least one"));
        assert!(error_after(|s| {
            s.as_object_mut().unwrap().remove("steps");
        })
        .contains("there are no steps"));
        let many = error_after(|s| s["steps"] = (0..13).map(|i| serde_json::json!({ "say": format!("Step {i}.") })).collect());
        assert!(many.contains("there are 13 steps; a scene has at most 12"), "{many}");
        let twelve: serde_json::Value = (0..12).map(|i| serde_json::json!({ "say": format!("Step {i}.") })).collect();
        let mut ok = flow();
        ok["steps"] = twelve;
        assert!(parse(&ok.to_string()).is_ok());
        let silent = error_after(|s| s["steps"][1]["say"] = "  ".into());
        assert!(silent.contains("steps[1] has no \"say\""), "{silent}");
    }

    #[test]
    fn nodes_are_limited_named_and_unique() {
        let many = error_after(|s| s["nodes"] = (0..15).map(|i| serde_json::json!({ "id": format!("n{i}"), "label": format!("Node {i}") })).collect());
        assert!(many.contains("there are 15 nodes; a scene draws at most 14"), "{many}");
        let twice = error_after(|s| s["nodes"][2]["id"] = "gh".into());
        assert!(twice.contains("nodes[2] uses the id \"gh\" a second time"), "{twice}");
        let nameless = error_after(|s| s["nodes"][1]["id"] = "".into());
        assert!(nameless.contains("nodes[1] has no \"id\""), "{nameless}");
        let blank = error_after(|s| s["nodes"][1]["label"] = "".into());
        assert!(blank.contains("nodes[1] (\"mgr\") has no \"label\""), "{blank}");
        let none = error_after(|s| {
            s["nodes"] = serde_json::json!([]);
            s["edges"] = serde_json::json!([]);
            s["steps"] = serde_json::json!([{ "say": "Nothing to see." }]);
        });
        assert!(none.contains("a flow scene needs \"nodes\""), "{none}");
    }

    #[test]
    fn edges_and_steps_must_point_at_what_exists() {
        let edge = error_after(|s| s["edges"][0]["to"] = "manager".into());
        assert!(edge.contains("edges[0].to is \"manager\", which is not a node; the node ids are \"gh\", \"mgr\", \"s\""), "{edge}");
        let node = error_after(|s| s["steps"][0]["nodes"] = serde_json::json!(["github"]));
        assert!(node.contains("steps[0].nodes names \"github\", which is not a node; the node ids are"), "{node}");
        let focus = error_after(|s| s["steps"][1]["focus"] = "session".into());
        assert!(focus.contains("steps[1].focus names \"session\", which is not a node"), "{focus}");
        let index = error_after(|s| s["steps"][1]["edges"] = serde_json::json!([0, 2]));
        assert!(index.contains("steps[1].edges has 2, but the scene's edges are numbered 0 to 1"), "{index}");
        let no_edges = error_after(|s| {
            s["edges"] = serde_json::json!([]);
            s["steps"][1]["edges"] = serde_json::json!([0]);
        });
        assert!(no_edges.contains("steps[1].edges has 0, but the scene has no edges"), "{no_edges}");
        let line = error_after(|s| s["steps"][0]["lines"] = serde_json::json!([3]));
        assert!(line.contains("steps[0].lines has 3, but the scene has no \"code\""), "{line}");
    }

    #[test]
    fn code_scenes_are_checked() {
        let code = |lines: usize, lit: usize| serde_json::json!({ "title": "Retry", "kind": "code", "file": "net.rs", "code": (0..lines).map(|i| format!("line {i}")).collect::<Vec<_>>(), "steps": [{ "say": "Look here.", "lines": [lit] }] }).to_string();
        assert!(parse(&code(60, 59)).is_ok());
        let long = parse(&code(61, 0)).unwrap_err().to_string();
        assert!(long.contains("\"code\" has 61 lines; show at most 60"), "{long}");
        let out = parse(&code(3, 3)).unwrap_err().to_string();
        assert!(out.contains("steps[0].lines has 3, but \"code\" has lines 0 to 2"), "{out}");
        let empty = parse(r#"{"kind":"code","steps":[{"say":"Look."}]}"#).unwrap_err().to_string();
        assert!(empty.contains("a code scene needs \"code\""), "{empty}");
        let wide = parse(&serde_json::json!({ "kind": "code", "code": ["x".repeat(161)], "steps": [{ "say": "Look." }] }).to_string()).unwrap_err().to_string();
        assert!(wide.contains("code[0] has 161 characters"), "{wide}");
        let joined = parse(&serde_json::json!({ "kind": "code", "code": ["a\nb"], "steps": [{ "say": "Look." }] }).to_string()).unwrap_err().to_string();
        assert!(joined.contains("code[0] holds a line break"), "{joined}");
    }

    #[test]
    fn compare_scenes_need_both_sides() {
        assert!(parse(r#"{"kind":"compare","before":"Slow.","after":"Fast.","steps":[{"say":"It got faster."}]}"#).is_ok());
        let half = parse(r#"{"kind":"compare","before":"Slow.","steps":[{"say":"It got faster."}]}"#).unwrap_err().to_string();
        assert!(half.contains("a compare scene needs both \"before\" and \"after\""), "{half}");
        let long = parse(&serde_json::json!({ "kind": "compare", "before": "x".repeat(601), "after": "y", "steps": [{ "say": "Look." }] }).to_string()).unwrap_err().to_string();
        assert!(long.contains("\"before\" has 601 characters"), "{long}");
    }

    #[test]
    fn texts_must_fit() {
        assert!(error_after(|s| s["title"] = "t".repeat(81).into()).contains("the title has 81 characters"));
        assert!(error_after(|s| s["nodes"][0]["label"] = "l".repeat(41).into()).contains("nodes[0].label has 41 characters"));
        assert!(error_after(|s| s["nodes"][0]["note"] = "n".repeat(121).into()).contains("nodes[0].note has 121 characters"));
        assert!(error_after(|s| s["edges"][0]["label"] = "e".repeat(41).into()).contains("edges[0].label has 41 characters"));
        assert!(error_after(|s| s["steps"][0]["say"] = "s".repeat(281).into()).contains("steps[0].say has 281 characters"));
        // Characters, not bytes.
        let mut ok = flow();
        ok["nodes"][0]["label"] = "ü".repeat(40).into();
        assert!(parse(&ok.to_string()).is_ok());
    }

    #[test]
    fn several_problems_are_named_together_but_not_endlessly() {
        let two = error_after(|s| {
            s["edges"][0]["from"] = "x".into();
            s["steps"][1]["edges"] = serde_json::json!([9]);
        });
        assert!(two.starts_with("the scene cannot be drawn: ") && two.contains("edges[0].from is \"x\"") && two.contains("steps[1].edges has 9"), "{two}");
        let many = error_after(|s| s["steps"] = (0..10).map(|_| serde_json::json!({ "say": "" })).collect());
        assert!(many.ends_with("; and 4 more"), "{many}");
        assert_eq!(many.matches("has no \"say\"").count(), 6);
    }

    #[test]
    fn check_works_on_a_scene_built_in_code() {
        let mut scene = parse(&flow().to_string()).unwrap();
        assert!(check(&scene).is_ok());
        scene.steps[0].nodes.push("nobody".into());
        assert!(check(&scene).unwrap_err().to_string().contains("steps[0].nodes names \"nobody\""));
        assert!(check(&Scene::default()).is_err());
    }

    #[test]
    fn the_guide_is_short_and_every_example_in_it_parses() {
        let guide = guide();
        assert!(guide.chars().count() <= 1500, "the guide has {} characters", guide.chars().count());
        let mut kinds = Vec::new();
        for (name, kind) in [("flow", SceneKind::Flow), ("sequence", SceneKind::Sequence), ("code", SceneKind::Code), ("compare", SceneKind::Compare)] {
            let line = guide.lines().find(|l| l.starts_with(&format!("{name}: {{"))).unwrap_or_else(|| panic!("no example for {name}"));
            let scene = parse(&line[name.len() + 2..]).unwrap_or_else(|err| panic!("the {name} example: {err}"));
            assert_eq!(scene.kind, kind);
            kinds.push(scene.kind);
        }
        assert_eq!(kinds.len(), 4);
        // The limits it states are the ones checked.
        assert!(guide.contains(&format!("1-{MAX_STEPS} steps")) && guide.contains(&format!("At most {MAX_NODES} nodes")) && guide.contains(&format!("at most {MAX_CODE_LINES} lines")));
    }

    #[test]
    fn code_walk_builds_a_scene_that_passes() {
        let lines: Vec<String> = ["fn retry() {", "    loop {", "        send()?;", "    }", "}"].iter().map(|l| l.to_string()).collect();
        let scene = code_walk("How retry works", "src/net.rs", lines.clone(), vec![("It loops.".into(), vec![1, 3]), ("Each round sends once.".into(), vec![2])]);
        assert_eq!((scene.title.as_str(), scene.kind, scene.file.as_str()), ("How retry works", SceneKind::Code, "src/net.rs"));
        assert_eq!(scene.code, lines);
        assert_eq!(scene.steps.iter().map(|s| (s.say.as_str(), s.lines.clone())).collect::<Vec<_>>(), vec![("It loops.", vec![1, 3]), ("Each round sends once.", vec![2])]);
        assert!(check(&scene).is_ok());
        // It survives the trip through JSON.
        assert_eq!(parse(&serde_json::to_string(&scene).unwrap()).unwrap(), scene);
    }

    #[test]
    fn code_walk_makes_sloppy_input_fit() {
        let lines: Vec<String> = (0..80).map(|i| if i == 0 { format!("\tlet x = \"{}\";  \n", "y".repeat(300)) } else { format!("line {i}") }).collect();
        let steps: Vec<(String, Vec<usize>)> = (0..20).map(|i| (if i == 1 { "  ".to_string() } else { format!("Step {i}. {}", "z".repeat(if i == 0 { 400 } else { 0 })) }, vec![70, 5, 5, 2, 59])).collect();
        let scene = code_walk(&"T".repeat(200), "", lines, steps);
        assert!(check(&scene).is_ok(), "{:?}", check(&scene));
        assert_eq!(scene.code.len(), MAX_CODE_LINES);
        assert!(scene.code[0].starts_with("    let x") && scene.code[0].ends_with('…') && scene.code[0].chars().count() == 160);
        assert_eq!(scene.steps.len(), MAX_STEPS);
        assert_eq!(scene.steps[0].lines, vec![2, 5, 59], "sorted, once each, only lines that exist");
        assert!(scene.steps[0].say.ends_with('…'));
        assert!(scene.steps.iter().all(|s| !s.say.trim().is_empty()), "the empty step was left out");
        assert_eq!(scene.title.chars().count(), 80);

        // Nothing at all still gives something to show.
        let empty = code_walk("", "src/net.rs", Vec::new(), Vec::new());
        assert!(check(&empty).is_ok());
        assert_eq!(empty.steps[0].say, "Here is src/net.rs.");
        assert_eq!(code_walk("", " ", Vec::new(), Vec::new()).steps[0].say, "Here is the code.");
    }
}
