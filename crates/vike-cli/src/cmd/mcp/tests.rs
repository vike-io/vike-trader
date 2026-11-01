use super::instructions::instructions;
use super::tool_schemas::tools_spec;
use super::venue_gate::{VENUE_CHECK_MOUNTED, VENUE_CHECK_UNVERIFIED};
use super::*;
use crate::cmd::hubkeys;
use vike_tradehub_client::wire::WireCommand;

/// The same no-node, no-credential server `crates/vike-cli/tests/cli_verbs/mcp_transcript.rs` drives,
/// reached through the public constructor rather than re-spelled — a second field list here is
/// a second place to forget a field, and the two would then disagree about what "a fresh
/// server" is.
fn server() -> Server {
    test_server()
}

fn req(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn call(name: &str, args: Value) -> Value {
    server().handle(&req(1, "tools/call", json!({ "name": name, "arguments": args }))).unwrap()
}

fn advertised_names(access: &ToolAccess) -> Vec<String> {
    access
        .advertised()
        .as_array()
        .expect("tools/list is an array")
        .iter()
        .map(|t| t["name"].as_str().expect("every tool is named").to_string())
        .collect()
}

/// A `#[cfg(test)]` helper, not a parser: every command the instructions name is inside
/// backticks, so the spans ARE the commands and nothing has to guess where one ends.
fn backticked_commands(text: &str) -> Vec<&str> {
    text.split('`').skip(1).step_by(2).collect()
}

#[path = "tests/cancellation.rs"]
#[cfg(test)]
mod cancellation;
#[path = "tests/credential_fence.rs"]
#[cfg(test)]
mod credential_fence;
#[path = "tests/data_deleter.rs"]
#[cfg(test)]
mod data_deleter;
#[path = "tests/manifest_and_skills.rs"]
#[cfg(test)]
mod manifest_and_skills;
#[path = "tests/node_lifecycle.rs"]
#[cfg(test)]
mod node_lifecycle;
#[path = "tests/preview_gate.rs"]
#[cfg(test)]
mod preview_gate;
#[path = "tests/protocol_surface.rs"]
#[cfg(test)]
mod protocol_surface;
#[path = "tests/research_tools.rs"]
#[cfg(test)]
mod research_tools;
#[path = "tests/scoping.rs"]
#[cfg(test)]
mod scoping;
#[path = "tests/transcript.rs"]
#[cfg(test)]
mod transcript;
#[path = "tests/venue_check.rs"]
#[cfg(test)]
mod venue_check;
