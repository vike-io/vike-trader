//! Every SUCCESS answer in the committed goldens fits its tool's `outputSchema`, the one the golden
//! `tools_list.full.json` advertises, by `crate::output_schema`'s check: rmcp checks nothing, and a client on
//! the TypeScript SDK (Claude Code, Claude Desktop) fails the call that does not. An `isError` answer is text
//! only and held to nothing; a tool with no `outputSchema` neither (`crates/vike-mcp/src/tool_schemas.rs`'s
//! module doc says which have one). The goldens are the program's answers: every other case here holds them
//! to it.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::harness::{LINES, golden_dir};
use crate::output_schema::mismatches;

#[test]
fn every_success_answer_in_the_goldens_fits_its_tools_output_schema() {
    let tools = golden("tools_list.full.json");
    let schemas: HashMap<&str, &Value> = transcript(&tools)
        .flat_map(|line| line.pointer("/recv/result/tools").and_then(Value::as_array))
        .flatten()
        .filter_map(|tool| Some((tool["name"].as_str()?, tool.get("outputSchema")?)))
        .collect();
    let (mut checked, mut wrong) = (0, Vec::new());
    for entry in std::fs::read_dir(golden_dir()).expect("the golden directory") {
        let path = entry.expect("a golden").path();
        let file = path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_string();
        if !file.ends_with(".json") {
            continue;
        }
        let session = golden(&file);
        let mut called: HashMap<String, &str> = HashMap::new();
        for line in transcript(&session) {
            if line["send"]["method"] == "tools/call" {
                let tool = line["send"]["params"]["name"].as_str().unwrap_or_default();
                called.insert(line["send"]["id"].to_string(), tool);
            }
            let result = &line["recv"]["result"];
            let Some(tool) = called.get(&line["recv"]["id"].to_string()) else { continue };
            if result["isError"] != json!(false) {
                continue;
            }
            let Some(schema) = schemas.get(tool) else { continue };
            checked += 1;
            let found = mismatches(schema, &result["structuredContent"]);
            wrong.extend(found.into_iter().map(|m| format!("{file}: {tool}: {m}")));
        }
    }
    assert_eq!(wrong, Vec::<String>::new(), "a success answer breaks its tool's outputSchema");
    assert!(checked > 0, "no golden holds a success answer of a tool with an outputSchema");
}

/// The check finds what a client refuses: a type, an enum word, a required field, and a keyword it does not
/// hold.
#[test]
fn the_check_finds_what_a_client_refuses() {
    let schema = json!({
        "type": "object",
        "properties": { "n": { "type": "integer", "enum": [1, 2] }, "ok": { "type": "boolean" } },
        "required": ["ok"],
    });
    assert_eq!(
        mismatches(&schema, &json!({ "ok": true, "n": 2, "more": [] })),
        Vec::<String>::new()
    );
    assert_eq!(
        mismatches(&schema, &json!({ "n": 1.5 })),
        ["/n: 1.5 is none of [1,2]", "/n: 1.5 is no \"integer\"", ": \"ok\" is missing"]
    );
    assert_eq!(
        mismatches(&json!({ "type": "object", "items": {} }), &json!({})),
        [": `items` is a keyword this check does not hold"]
    );
}

/// A committed golden, its multi-line strings joined back (the harness splits them so a person can read them).
fn golden(file: &str) -> Value {
    let text = std::fs::read_to_string(golden_dir().join(file)).expect("a committed golden");
    let mut golden: Value = serde_json::from_str(&text).expect("a golden is JSON");
    join_lines(&mut golden);
    golden
}

fn transcript(session: &Value) -> impl Iterator<Item = &Value> {
    session["transcript"].as_array().into_iter().flatten()
}

fn join_lines(value: &mut Value) {
    if let Some(lines) = value.get(LINES).and_then(Value::as_array) {
        let lines: Vec<&str> = lines.iter().filter_map(Value::as_str).collect();
        *value = json!(lines.join("\n"));
        return;
    }
    match value {
        Value::Object(map) => map.values_mut().for_each(join_lines),
        Value::Array(items) => items.iter_mut().for_each(join_lines),
        _ => {}
    }
}
