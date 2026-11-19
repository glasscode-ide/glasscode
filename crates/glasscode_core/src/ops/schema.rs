//! An op's arguments as a JSON schema, for the manager's typed tools (M34) and for help: the
//! shape of the op's own JSON, which `parse` makes from the command line.

use serde_json::{Map, Value, json};

use super::spec::{ArgKind, ArgSpec, OpSpec};
use crate::board::{List, Section};

/// `{"type": "object", "properties": {…}, "required": […]}`, with what the op does as its
/// description. Words only a person types ("fri", "the start of an id") are not in it: a
/// typed caller sends the values `parse` would have made of them.
pub fn json_schema(spec: &OpSpec) -> Value {
    let mut properties = Map::new();
    for arg in spec.args {
        properties.insert(arg.name.to_string(), property(arg));
    }
    let required: Vec<&str> = spec.args.iter().filter(|a| a.required).map(|a| a.name).collect();
    json!({
        "type": "object",
        "description": spec.summary,
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn property(arg: &ArgSpec) -> Value {
    let (mut p, typed) = match arg.kind {
        ArgKind::Word | ArgKind::Text | ArgKind::Project | ArgKind::Task | ArgKind::Every => (json!({ "type": "string" }), None),
        ArgKind::Ticket => (json!({ "type": "string", "pattern": "^[A-Z][A-Z0-9]*-[0-9]+$" }), None),
        ArgKind::Switch => (json!({ "type": "boolean" }), None),
        ArgKind::Count => (json!({ "type": "integer", "minimum": 0 }), None),
        ArgKind::Pr => (json!({ "type": "integer", "minimum": 1 }), Some("its number")),
        ArgKind::Day => (json!({ "type": ["string", "null"], "format": "date" }), Some("YYYY-MM-DD, or null for none")),
        ArgKind::Until => (json!({ "type": "string", "format": "date" }), Some("YYYY-MM-DD, after today")),
        ArgKind::Session => (json!({ "type": "string" }), Some("its whole id")),
        ArgKind::List => (json!({ "type": "string", "enum": [List::Todo, List::Asked, List::Waiting, List::Later] }), None),
        ArgKind::Section => (json!({ "type": "string", "enum": Section::ALL.iter().map(|s| s.key()).collect::<Vec<_>>() }), None),
        ArgKind::Movable => (json!({ "type": "string", "enum": Section::ALL.iter().filter(|s| s.movable()).map(|s| s.key()).collect::<Vec<_>>() }), None),
        ArgKind::Many => (json!({ "type": "array", "items": { "type": "string" } }), None),
    };
    // What a typed caller sends where a person types words ("fri", the start of an id).
    p["description"] = json!(match typed {
        Some(typed) => format!("{}; here {typed}", arg.help),
        None => arg.help.to_string(),
    });
    if let (ArgKind::Count, Some(n)) = (arg.kind, arg.default.and_then(|d| d.parse::<u64>().ok())) {
        p["default"] = json!(n);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{OPS, find};

    #[test]
    fn the_schema_has_every_argument_and_only_them() {
        for op in OPS {
            let schema = json_schema(op);
            let properties = schema["properties"].as_object().unwrap();
            assert_eq!(properties.len(), op.args.len(), "{}", op.name);
            for arg in op.args {
                let p = &properties[arg.name];
                assert!(p["type"].is_string() || p["type"].is_array(), "{}: {}", op.name, arg.name);
                assert!(p["description"].as_str().is_some_and(|d| d.starts_with(arg.help)), "{}: {}", op.name, arg.name);
                assert_eq!(schema["required"].as_array().unwrap().contains(&json!(arg.name)), arg.required, "{}: {}", op.name, arg.name);
            }
            assert_eq!(schema["additionalProperties"], json!(false));
            assert_eq!(schema["description"], json!(op.summary));
        }
    }

    #[test]
    fn values_have_the_types_parse_makes() {
        let add = json_schema(find("board.add").unwrap());
        assert_eq!(add["required"], json!(["title"]));
        assert_eq!(add["properties"]["list"]["enum"], json!(["todo", "asked", "waiting", "later"]));
        assert_eq!(add["properties"]["due"]["type"], json!(["string", "null"]));
        assert_eq!(add["properties"]["links"]["items"], json!({ "type": "string" }));
        let moved = json_schema(find("board.move").unwrap());
        assert_eq!(moved["properties"]["to"]["enum"], json!(["asked", "todo", "reviews", "tickets", "waiting", "later"]));
        let read = json_schema(find("session.read").unwrap());
        assert_eq!(read["properties"]["last"], json!({ "type": "integer", "minimum": 0, "description": "how many of its last messages", "default": 6 }));
        assert_eq!(read["properties"]["session"]["description"], json!("an id, its start, or words from its title (\"this\" is the session the command runs in); here its whole id"));
        assert_eq!(json_schema(find("pr.status").unwrap())["properties"]["number"]["type"], json!("integer"));
    }
}
