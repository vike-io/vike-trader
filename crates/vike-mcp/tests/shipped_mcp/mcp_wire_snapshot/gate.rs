//! The mandatory preview gate over the wire: every write tool with no node, the same flow against a
//! real paper node, the token's expiry and single use, and `--unattended`.

use std::time::Duration;

use serde_json::{Value, json};

use super::harness::{Golden, Session, World, request};

/// Longer than the server's `PREVIEW_WINDOW`, whose value the preview's own note states.
const PAST_THE_PREVIEW_WINDOW: Duration = Duration::from_secs(61);

const PAPER_VENUE: &str = "polymarket";
const PAPER_TOKEN: &str = "MCP_WIRE_TOKEN";
/// Obviously fake: any bytes work as long as both sides agree.
const OBSERVE_KEY: &str = "DUMMY-observe-key-for-mcp-wire";
const CONTROL_KEY: &str = "DUMMY-control-key-for-mcp-wire";

/// `args` with `extra`'s fields on top.
fn with(args: &Value, extra: Value) -> Value {
    let mut merged = args.clone();
    for (k, v) in extra.as_object().expect("extra arguments are an object") {
        merged[k] = v.clone();
    }
    merged
}

fn preview_token(answer: &Value) -> String {
    match answer["result"]["structuredContent"]["preview_token"].as_str() {
        Some(token) => token.to_string(),
        None => panic!("a preview hands back a token, got {answer}"),
    }
}

/// `(tool, its arguments, a DIFFERENT command of the same tool)` for all eleven write tools.
fn write_tools() -> Vec<(&'static str, Value, Value)> {
    let order = json!({
        "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 0.5,
        "order_type": "limit", "price": 100.0, "reason": "a golden's reason",
    });
    let mount = json!({
        "venue": "sim", "symbol": "BTCUSDT", "interval": "1m",
        "strategy": "spread_maker.rs", "mount_id": "sim__BTCUSDT__1m",
    });
    let delete = json!({
        "kind": "bar", "venue": "binance", "symbol": "BTCUSDT", "interval": "1h",
        "produced_by": "klines:",
    });
    vec![
        ("submit_order", order.clone(), with(&order, json!({ "qty": 50.0 }))),
        ("cancel_order", json!({ "order_client_id": "c-1" }), json!({ "order_client_id": "c-2" })),
        (
            "modify_order",
            json!({ "order_client_id": "c-1", "new_qty": 2.0 }),
            json!({ "order_client_id": "c-1", "new_qty": 3.0 }),
        ),
        (
            "flatten",
            json!({ "venue": "sim", "symbol": "BTCUSDT" }),
            json!({ "venue": "sim", "symbol": "ETHUSDT" }),
        ),
        ("close_all", json!({ "venue": "sim" }), json!({ "venue": "other" })),
        ("set_trading_state", json!({ "state": "halted" }), json!({ "state": "reducing" })),
        ("mass_cancel", json!({ "venue": "sim" }), json!({ "venue": "sim", "symbol": "BTCUSDT" })),
        ("mount_strategy", mount.clone(), with(&mount, json!({ "interval": "5m" }))),
        (
            "unmount_strategy",
            json!({ "mount_id": "sim__BTCUSDT__1m" }),
            json!({ "mount_id": "sim__ETHUSDT__1m" }),
        ),
        (
            "set_setting",
            json!({ "key": "config.tradehub_addr", "value": "127.0.0.1:7879" }),
            json!({ "key": "config.tradehub_addr", "value": "127.0.0.1:7880" }),
        ),
        ("delete_series", delete.clone(), with(&delete, json!({ "produced_by": "panel_bars:" }))),
    ]
}

/// **Every write tool, no node**: call, `confirm` alone, `confirm` with a junk token, a real token
/// without `confirm`, a real token on another command, the right token, the same token again. A
/// confirm the gate admits fails at the connection, which is how "it got past the gate" shows.
pub(super) fn preview_gate_without_a_node() -> Vec<Golden> {
    write_tools()
        .into_iter()
        .map(|(tool, args, other)| {
            let mut s = Session::start(format!("gate.no_node.{tool}"), World::empty(), &[]);
            let spent_on_another_command = preview_token(&s.tool(tool, args.clone()));
            let token = preview_token(&s.tool(tool, with(&args, json!({ "confirm": true }))));
            s.tool(tool, with(&args, json!({ "confirm": true, "preview_token": "pv-junk" })));
            s.tool(tool, with(&args, json!({ "preview_token": token })));
            s.tool(
                tool,
                with(&other, json!({ "confirm": true, "preview_token": spent_on_another_command })),
            );
            let right = with(&args, json!({ "confirm": true, "preview_token": token }));
            s.tool(tool, right.clone());
            s.tool(tool, right);
            s.finish()
                .what(
                    "the preview gate on one write tool with no node: every token shape, in order",
                )
                .golden()
        })
        .collect()
}

/// **The same gate against a real paper node** (no venue is reachable from one): the node's verdict
/// on a preview, a confirm the node accepts, the single use of its token, a venue it does not mount,
/// and the same order once the venue check has evidence.
///
/// An idle paper node has not folded, so the first preview's `venue_check` is `unverified` after the
/// server's two ~2 s waits; after the first order it is `mounted`. The wait below is what makes that
/// second half deterministic: the server has SEEN the order on its own frame before the next call.
pub(super) fn preview_gate_with_a_paper_node() -> Vec<Golden> {
    let (_mount, addr) =
        crate::common::spawn_paper_node(PAPER_VENUE, PAPER_TOKEN, OBSERVE_KEY, CONTROL_KEY);
    let node = addr.to_string();
    // This box's device key is the one the node lists at the role `control`.
    let world = World::with_device(CONTROL_KEY).scrub(&node, "<node-addr>");
    let mut s = Session::start("gate.paper_node", world, &["--node", &node]);

    let order = json!({ "venue": PAPER_VENUE, "symbol": PAPER_TOKEN, "side": 1, "qty": 1.0 });
    let preview = s.tool("submit_order", order.clone());
    let token = preview_token(&preview);
    let resting =
        preview["result"]["structuredContent"]["wire_command"]["Submit"]["order_client_id"]
            .as_str()
            .expect("the preview shows the minted id")
            .to_string();
    let confirm = with(&order, json!({ "confirm": true, "preview_token": token }));
    s.tool("submit_order", confirm.clone());

    // Through the server's OWN read, and unrecorded: its frame is the one the next call judges, and
    // how many reads the wait took is the one thing here that varies.
    let frame_read = request(
        "unrecorded-wait",
        "tools/call",
        json!({ "name": "node_snapshot", "arguments": {} }),
    );
    let order_is_resting = || {
        let frame = s.unrecorded(frame_read.clone());
        let orders = frame["result"]["structuredContent"]["orders"].as_array();
        orders.is_some_and(|orders| !orders.is_empty())
    };
    assert!(
        vike_log::poll::wait_until(Duration::from_secs(30), order_is_resting),
        "the confirmed order never reached the frame this server holds"
    );
    s.note("the confirmed order is on the frame this server holds");

    s.tool("submit_order", confirm);
    s.tool("submit_order", with(&order, json!({ "venue": "not-a-mounted-venue" })));
    s.tool("submit_order", order);
    let cancel = json!({ "order_client_id": resting });
    let cancel_token = preview_token(&s.tool("cancel_order", cancel.clone()));
    s.tool(
        "cancel_order",
        with(&cancel, json!({ "confirm": true, "preview_token": cancel_token })),
    );
    s.tool("set_trading_state", json!({ "state": "active" }));
    s.tool("set_setting", json!({ "key": "config.tradehub_addr", "value": "127.0.0.1:7879" }));
    vec![s.finish().what("the preview gate against a real paper node").golden()]
}

/// **The one order gate against a real paper node, this box's `policy.max_notional_per_order` at 1** (the owner's
/// D1-D5 of 2026-11-24): what is refused gets no `preview_token` and what passes is previewed, nothing confirmed but
/// the two orders the rows stand on (a market warm-up, so the node publishes the book and its value, and one resting
/// limit under the cap for the modify). `crates/vike-cli/tests/shipped_cli/cli_mcp_parity.rs`'s `GATE_ROWS` holds the
/// shipped CLI to every answer here, row by row, so a call's arguments are that table's, byte for byte.
pub(super) fn order_gate() -> Vec<Golden> {
    let (_mount, addr) =
        crate::common::spawn_paper_node(PAPER_VENUE, PAPER_TOKEN, OBSERVE_KEY, CONTROL_KEY);
    let node = addr.to_string();
    let world = World::with_device(CONTROL_KEY).scrub(&node, "<node-addr>");
    vike_config::write_setting_row(
        &world.settings(),
        "policy.max_notional_per_order",
        "1",
        Duration::from_secs(5),
    )
    .expect("this box's ceiling row");
    let mut s = Session::start("order_gate", world, &["--node", &node]);
    let frame_read = request(
        "unrecorded-wait",
        "tools/call",
        json!({ "name": "node_snapshot", "arguments": {} }),
    );
    let confirmed = |s: &mut Session, args: Value, wait: &dyn Fn(&Value) -> bool| {
        let token = preview_token(&s.tool("submit_order", args.clone()));
        s.tool("submit_order", with(&args, json!({ "confirm": true, "preview_token": token })));
        let seen = || wait(&s.unrecorded(frame_read.clone()));
        assert!(vike_log::poll::wait_until(Duration::from_secs(30), seen), "never on the frame");
    };
    let orders = |frame: &Value| frame["result"]["structuredContent"]["orders"].clone();
    confirmed(
        &mut s,
        json!({ "venue": PAPER_VENUE, "symbol": PAPER_TOKEN, "side": 1, "qty": 1.0 }),
        &|frame| orders(frame).as_array().is_some_and(|o| !o.is_empty()),
    );
    s.note("the warm-up is on the frame: the node publishes its book");
    confirmed(
        &mut s,
        json!({ "venue": PAPER_VENUE, "symbol": PAPER_TOKEN, "side": 1, "qty": 1.0,
                "order_type": "limit", "price": 0.5, "order_client_id": "Rest1" }),
        &|frame| orders(frame).to_string().contains("Rest1"),
    );
    s.note("the resting limit Rest1 is on the frame");
    for (tool, arguments) in ORDER_GATE_CALLS {
        s.tool(tool, serde_json::from_str(arguments).expect("a row's arguments are JSON"));
    }
    vec![
        s.finish().what("the one order gate at a ceiling of 1, against a real paper node").golden(),
    ]
}

/// `order_gate`'s calls after its two orders, each `(tool, arguments)`: the rows of
/// `crates/vike-cli/tests/shipped_cli/cli_mcp_parity.rs`'s `GATE_ROWS`.
const ORDER_GATE_CALLS: &[(&str, &str)] = &[
    (
        "submit_order",
        r#"{"order_type": "limit", "price": 0.5, "qty": 10.0, "side": 1, "symbol": "MCP_WIRE_TOKEN", "venue": "polymarket"}"#,
    ),
    (
        "submit_order",
        r#"{"order_client_id": "BAD-ID-1", "qty": 1.0, "side": 1, "symbol": "MCP_WIRE_TOKEN", "venue": "polymarket"}"#,
    ),
    (
        "submit_order",
        r#"{"order_type": "limit", "price": 0.5, "qty": 1.0, "side": 1, "symbol": "MCP_WIRE_TOKEN", "venue": "nosuchvenue"}"#,
    ),
    (
        "submit_order_list",
        r#"{"limit_price": 0.5, "qty": 10.0, "shape": "oco", "side": 1, "stop_price": 0.6, "symbol": "MCP_WIRE_TOKEN", "venue": "polymarket"}"#,
    ),
    (
        "submit_order_list",
        r#"{"limit_price": 0.5, "order_client_ids": "BAD-1,Good2", "qty": 1.0, "shape": "oco", "side": 1, "stop_price": 0.6, "symbol": "MCP_WIRE_TOKEN", "venue": "polymarket"}"#,
    ),
    (
        "submit_combo",
        r#"{"legs": [{"limit": 0.5, "ratio": 1, "symbol": "MCP_WIRE_TOKEN"}, {"limit": 0.4, "ratio": -1, "symbol": "OTHER"}], "order_client_id": "BAD-ID-2", "qty": 1.0, "side": 1, "venue": "polymarket"}"#,
    ),
    ("modify_order", r#"{"new_qty": 10.0, "order_client_id": "Rest1"}"#),
    ("modify_order", r#"{"new_price": 2.0, "order_client_id": "Rest1"}"#),
    ("cancel_order", r#"{"order_client_id": "BAD-ID-3"}"#),
    ("flatten", r#"{"symbol": "MCP_WIRE_TOKEN", "venue": "polymarket"}"#),
    ("close_all", r#"{"venue": "polymarket"}"#),
    ("close_all", r#"{}"#),
    ("mass_cancel", r#"{"venue": "polymarket"}"#),
];

/// **The token's expiry, by really waiting past the window**: there is no clock to inject from outside
/// the process. A slow test on purpose, and the only one here. A node command and the deleter are two
/// arms with two sentences; each expired token is then simply gone.
pub(super) fn preview_token_expiry() -> Vec<Golden> {
    let order = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
    let delete = json!({ "kind": "bar", "venue": "binance", "produced_by": "klines:" });
    let mut s = Session::start("gate.expiry", World::empty(), &[]);
    let order_token = preview_token(&s.tool("submit_order", order.clone()));
    let delete_token = preview_token(&s.tool("delete_series", delete.clone()));
    std::thread::sleep(PAST_THE_PREVIEW_WINDOW);
    s.note("waited past the preview window");
    let order = with(&order, json!({ "confirm": true, "preview_token": order_token }));
    let delete = with(&delete, json!({ "confirm": true, "preview_token": delete_token }));
    s.tool("submit_order", order.clone());
    s.tool("submit_order", order);
    s.tool("delete_series", delete.clone());
    s.tool("delete_series", delete);
    vec![s.finish().what("an expired token is refused once and is gone after that").golden()]
}

/// `--unattended`: every `set_setting` refused before a preview, and every other write untouched.
pub(super) fn unattended() -> Vec<Golden> {
    let mut s = Session::start("gate.unattended", World::empty(), &["--unattended"]);
    s.tool("set_setting", json!({ "key": "policy.max_notional_per_order", "value": "250" }));
    s.tool("set_setting", json!({ "key": "config.tradehub_addr", "value": "127.0.0.1:7879" }));
    s.tool("submit_order", json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }));
    s.tool("set_trading_state", json!({ "state": "halted" }));
    vec![s.finish().what("`--unattended`: what it refuses, and what it does not").golden()]
}
