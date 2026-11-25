//! A tool's `outputSchema` held to its answers. rmcp checks nothing, while a client on the TypeScript SDK
//! (Claude Code, Claude Desktop) fails the call whose `structuredContent` does not match, so this is the check.
//! It holds the one subset `crates/vike-mcp/src/tool_args.rs`'s `output_schema` advertises (`type`,
//! `properties`, `required`, `enum`, `description`), and a keyword outside it is a mismatch too: the trim
//! cannot grow past what is checked here.
//!
//! One file on two paths, as `write_args.rs` is: `crate::protocol`'s `tool_response` holds every in-process
//! test's answer to it, and `crates/vike-mcp/tests/shipped_mcp/mcp_wire_snapshot/output_schemas.rs` every
//! success answer in the goldens.

use serde_json::Value;

/// Every way `value` breaks `schema`, each at its JSON path (empty: the answer itself); none when it fits.
pub fn mismatches(schema: &Value, value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    check("", schema, value, &mut out);
    out
}

fn check(at: &str, schema: &Value, value: &Value, out: &mut Vec<String>) {
    let Some(schema) = schema.as_object() else {
        return out.push(format!("{at}: the schema is no object: {schema}"));
    };
    for (keyword, rule) in schema {
        match keyword.as_str() {
            "description" => {}
            "type" => {
                let kinds =
                    rule.as_array().map_or_else(|| vec![rule], |kinds| kinds.iter().collect());
                if !kinds.iter().any(|kind| kind.as_str().is_some_and(|kind| is_a(kind, value))) {
                    out.push(format!("{at}: {value} is no {rule}"));
                }
            }
            "enum" => {
                if !rule.as_array().is_some_and(|words| words.contains(value)) {
                    out.push(format!("{at}: {value} is none of {rule}"));
                }
            }
            "required" => {
                for key in rule.as_array().into_iter().flatten() {
                    if key.as_str().is_none_or(|key| value.get(key).is_none()) {
                        out.push(format!("{at}: {key} is missing"));
                    }
                }
            }
            "properties" => {
                for (key, property) in rule.as_object().into_iter().flatten() {
                    if let Some(field) = value.get(key) {
                        check(&format!("{at}/{key}"), property, field, out);
                    }
                }
            }
            other => out.push(format!("{at}: `{other}` is a keyword this check does not hold")),
        }
    }
}

/// Whether `value` is of the JSON Schema type `kind`.
fn is_a(kind: &str, value: &Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.0),
        _ => false,
    }
}
