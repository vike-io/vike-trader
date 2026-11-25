use super::instructions::instructions;
use super::tool_schemas::tools_spec;
use super::venue_gate::{VENUE_CHECK_MOUNTED, VENUE_CHECK_UNVERIFIED};
use super::*;
use vike_client::hubkeys;
use vike_tradehub_client::wire::CommandWire;

/// The same no-node, no-credential server `crates/vike-mcp/tests/shipped_mcp/mcp_transcript.rs` drives,
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

/// A call still naming the order client id under its retired key is REFUSED by name, on an order tool
/// and on an order-list tool's plural alike: an absent id would be minted and a retry placed twice.
#[test]
fn a_retired_order_client_id_key_is_refused_by_name() {
    for (old, new) in vike_model::orders::client_id::RETIRED_ORDER_CLIENT_ID_KEYS {
        for tool in ["submit_order", "decode_order_id"] {
            let mut args = json!({ "venue": "binance", "symbol": "BTCUSDT" });
            args[old] = json!("pinned1");
            let resp = call(tool, args);
            assert_eq!(resp["result"]["isError"], true, "{tool} {old}: {resp}");
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains(&format!("`{old}` is now `{new}`")), "{tool} {old}: {text}");
        }
    }
}

/// Every success answer an in-process test is given, held to its tool's `outputSchema` (`crate::protocol`'s
/// `tool_response` calls this), so a schema no golden answers under (`run_status`'s) is held too. A tool with
/// none is held to nothing.
pub(crate) fn assert_it_fits_its_output_schema(tool: &str, answer: &Value) {
    let spec = tools_spec();
    let schema = spec.as_array().into_iter().flatten().find(|t| t["name"] == tool);
    if let Some(schema) = schema.and_then(|t| t.get("outputSchema")) {
        let wrong = output_schema::mismatches(schema, answer);
        assert_eq!(
            wrong,
            Vec::<String>::new(),
            "{tool}'s answer breaks its outputSchema: {answer}"
        );
    }
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
pub(super) mod cancellation;
#[path = "tests/credential_fence.rs"]
#[cfg(test)]
mod credential_fence;
#[path = "tests/data_deleter.rs"]
#[cfg(test)]
mod data_deleter;
#[path = "tests/dial_patience.rs"]
#[cfg(test)]
mod dial_patience;
#[path = "tests/manifest_and_skills.rs"]
#[cfg(test)]
mod manifest_and_skills;
#[path = "tests/node_lifecycle.rs"]
#[cfg(test)]
mod node_lifecycle;
#[path = "tests/output_schema.rs"]
#[cfg(test)]
mod output_schema;
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
#[path = "tests/tool_facts.rs"]
#[cfg(test)]
mod tool_facts;
#[path = "tests/transcript.rs"]
#[cfg(test)]
mod transcript;
#[path = "tests/venue_check.rs"]
#[cfg(test)]
mod venue_check;
#[path = "tests/write_args.rs"]
#[cfg(test)]
mod write_args;
