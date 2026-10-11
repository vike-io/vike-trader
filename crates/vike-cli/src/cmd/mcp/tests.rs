use super::instructions::instructions;
use super::tool_schemas::tools_spec;
use super::venue_gate::{VENUE_CHECK_MOUNTED, VENUE_CHECK_UNVERIFIED};
use super::*;
use crate::cmd::nodekeys;
use vike_tradehub_client::wire::WireCommand;

/// The same no-node, no-credential server `crates/vike-cli/tests/mcp_transcript.rs` drives,
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

/// Arguments broad enough for EVERY write tool at once — the twin of
/// `crates/vike-cli/tests/mcp_transcript.rs`'s helper of the same name, and it has to stay one:
/// the tools take disjoint argument sets and each ignores what it does not name, so the union
/// is what lets a loop walk [`WRITE_TOOLS`] with no per-tool table to fall out of step with it.
///
/// ⚠ **A NEW write tool whose required arguments are not in here fails the loops rather than
/// skipping them**, which is the behaviour to keep: the failure names the tool, and the fix is
/// one line. (`set_setting` takes only `key` and `value` since `docs/decisions/0086`; the `key` is
/// a config one, though a policy key would take the same path in this ATTENDED server — there is
/// no retype gate left for a policy write to meet.)
fn every_write_tools_arguments() -> Value {
    json!({
        "venue": "sim",
        "symbol": "BTCUSDT",
        "side": 1,
        "qty": 1.0,
        "client_order_id": "c1",
        "new_qty": 2.0,
        "state": "halted",
        "interval": "1m",
        "controller_id": "sim__BTCUSDT__1m",
        "name": "spread_maker",
        // …and `delete_series`' own required set. `kind`/`venue` are its SELECTOR (exact,
        // never a substring) and `produced_by` is REQUIRED on this surface for every call —
        // see that tool's doc for why it is unconditional here and conditional on the CLI.
        "kind": "bar",
        "produced_by": "klines:",
        "key": "config.tradehub_addr",
        "value": "127.0.0.1:7879"
    })
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
