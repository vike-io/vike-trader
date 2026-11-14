//! `scripts/gen_skills.sh` renders `skills/` from what this server serves, and reads it from a
//! committed file so that rendering costs no build: `crates/vike-mcp/tests/fixtures/tool_facts.tsv`.
//! This is that file's drift gate. To regenerate it, in a lane (nothing compiles on the dev box), then
//! read the diff and run `scripts/gen_skills.sh`:
//!
//! ```sh
//! cargo test -p vike-mcp --lib -- --ignored bless_tool_facts
//! ```

use std::fmt::Write as _;

use super::*;
use crate::tool_args::WriteGate;
use crate::tool_schemas::{declared_fields, roster};

const COMMITTED: &str = include_str!("../../tests/fixtures/tool_facts.tsv");

/// The facts, one tab-separated row each, `gen_skills.sh`'s header names the kinds. Every tool's
/// required arguments, then its arguments in declared order, the write gate's last and marked, because
/// a skill's table lists them under one footnote.
fn tool_facts() -> String {
    let mut out = String::new();
    for tool in roster() {
        let spec = &tool.spec;
        let name = spec["name"].as_str().expect("every tool is named");
        let destructive = spec["annotations"]["destructiveHint"] == json!(true);
        let description = spec["description"].as_str().expect("every tool is described");
        writeln!(out, "TOOL\t{name}\t{}\t{description}", u8::from(destructive)).expect("a String");
        let input = &spec["inputSchema"];
        for arg in input["required"].as_array().into_iter().flatten() {
            let arg = arg.as_str().expect("a required argument is named");
            writeln!(out, "REQ\t{name}\t{arg}").expect("a String");
        }
        let gate = if is_write_tool(name) { declared_fields::<WriteGate>() } else { &[] };
        let args = tool.declared.iter().map(|arg| (arg, 0)).chain(gate.iter().map(|arg| (arg, 1)));
        for (arg, shared) in args {
            let property = &input["properties"][*arg];
            let kind = property["type"].as_str().expect("every argument has one type");
            let description = property["description"].as_str().unwrap_or_default();
            writeln!(out, "PROP\t{name}\t{arg}\t{kind}\t{description}\t{shared}")
                .expect("a String");
        }
    }
    for tool in WRITE_TOOLS {
        writeln!(out, "WRITE\t{tool}").expect("a String");
    }
    writeln!(out, "WINDOW\t{}", PREVIEW_WINDOW.as_secs()).expect("a String");
    out
}

#[test]
fn the_committed_tool_facts_are_what_this_server_serves() {
    assert_eq!(
        COMMITTED,
        tool_facts(),
        "tests/fixtures/tool_facts.tsv is not what this server serves. In a lane: cargo test -p \
         vike-mcp --lib -- --ignored bless_tool_facts, then scripts/gen_skills.sh, and read both diffs"
    );
}

/// A row is one line and its fields are split on tabs, so one more in a description would misread
/// every field after it, in `skills/`, with nothing refusing.
#[test]
fn no_tool_fact_carries_a_separator_of_its_own() {
    let facts = tool_facts();
    let widths = [("TOOL", 4), ("REQ", 3), ("PROP", 6), ("WRITE", 2), ("WINDOW", 2)];
    for line in facts.lines() {
        let kind = line.split('\t').next().unwrap_or_default();
        let width = widths.iter().find(|(k, _)| *k == kind).map(|(_, w)| *w);
        assert_eq!(Some(line.split('\t').count()), width, "{line}");
    }
}

/// Every argument the schema advertises is in the declared order, which is what proves the two views
/// of one struct agree, and what would go red on an argument added around the struct.
#[test]
fn the_declared_order_names_exactly_what_the_schema_advertises() {
    for tool in roster() {
        let name = tool.spec["name"].as_str().expect("every tool is named");
        let gate = if is_write_tool(name) { declared_fields::<WriteGate>() } else { &[] };
        let mut declared: Vec<&str> = tool.declared.iter().chain(gate).copied().collect();
        declared.sort_unstable();
        let advertised: Vec<&str> = tool.spec["inputSchema"]["properties"]
            .as_object()
            .expect("a tool's input is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(declared, advertised, "{name}");
    }
}

#[test]
#[ignore = "writes into the source tree: run it deliberately, then read the diff"]
fn bless_tool_facts() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tool_facts.tsv");
    std::fs::write(path, tool_facts()).expect("write the tool facts");
}
