//! Arguments for EVERY write tool at once. One file on two paths: a unit test reaches it from
//! `crates/vike-mcp/src/tests.rs`, and the stdio transcript through `#[path]` from
//! `crates/vike-mcp/tests/shipped_mcp/main.rs`, because a test crate cannot see a `cfg(test)` module.

/// Arguments broad enough for EVERY MCP write tool at once (`vike_mcp::WRITE_TOOLS`), each caller
/// passing its own `order_client_id`. The tools take disjoint argument sets and each IGNORES what it
/// does not name, so the union is what lets a loop walk the write tools with no per-tool table to
/// fall out of step with them.
///
/// ⚠ **A NEW write tool whose required arguments are not in here fails the loops rather than
/// skipping them** (its call answers a build error instead of a preview), which is the behaviour
/// to keep: the failure names the tool, and the fix is one line. `set_setting` takes only `key`
/// and `value` since `docs/decisions/0086`; the `key` is a config one, though a policy key would
/// take the same path in this ATTENDED server.
pub fn every_write_tools_arguments(order_client_id: &str) -> serde_json::Value {
    serde_json::json!({
        "venue": "sim",
        "symbol": "BTCUSDT",
        "side": 1,
        "qty": 1.0,
        "order_client_id": order_client_id,
        "new_qty": 2.0,
        "state": "halted",
        // …and the node-lifecycle verbs' own required set.
        "interval": "1m",
        "mount_id": "sim__BTCUSDT__1m",
        "strategy": "spread_maker.rs",
        // …and `delete_series`' own required set. `kind`/`venue` are its SELECTOR (exact, never a
        // substring) and `produced_by` is REQUIRED on this surface for every call, sweep or single
        // series — see that tool's doc for why it is unconditional here and conditional on the CLI.
        "kind": "bar",
        "produced_by": "klines:",
        // …and `submit_order_list`'s: an OTO, a market entry and its protective stop.
        "shape": "oto",
        "stop_price": 0.5,
        // …and `submit_combo`'s: two legs at market (no net, no leg limit).
        "legs": [{ "symbol": "C1", "ratio": 1 }, { "symbol": "C2", "ratio": -1 }],
        "key": "config.tradehub_addr",
        "value": "127.0.0.1:17879"
    })
}
