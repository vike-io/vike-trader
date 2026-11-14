//! `tools/call` for everything that is not the preview gate: the offline tools, every error shape,
//! the refusal of a withheld tool, the tools that dial something that is not there.

use serde_json::json;

use super::harness::{FakeDaemon, Golden, Seen, Session, World};

/// The four tools that open no socket, with their error shapes: a bad script is an `ok: false`
/// answer from one and a tool error from another, and neither is the wire's business to unify.
pub(super) fn offline_tools() -> Vec<Golden> {
    let good =
        "let fast = param(\"fast\", 10);\nlet slow = param(\"slow\", 20.5);\nfn on_bar() { }\n";
    let broken = "fn on_bar( {";
    let mut session = Session::start("tools.offline", World::empty(), &[]);
    session.tool("validate_strategy", json!({ "script": good }));
    session.tool("validate_strategy", json!({ "script": broken }));
    session.tool("validate_strategy", json!({ "script": "let x = no_such_function();" }));
    session.tool("validate_strategy", json!({}));
    session.tool("validate_strategy", json!({ "script": 7 }));
    session.tool("discover_params", json!({ "script": good }));
    session.tool("discover_params", json!({ "script": "fn on_bar() { }" }));
    session.tool("discover_params", json!({ "script": broken }));
    session.tool("discover_params", json!({}));
    session.tool("list_templates", json!({}));
    session.tool("list_indicators", json!({}));
    session.tool("list_indicators", json!({ "name": "sma" }));
    session.tool("list_indicators", json!({ "name": "no_such_indicator" }));
    session.tool("list_indicators", json!({ "name": "   " }));
    session.tool("list_indicators", json!({ "category": "no_such_category" }));
    vec![session.finish().what("the offline tools: answers and error shapes").golden()]
}

/// What a bad call answers, for every tool that can be called badly and for no tool at all.
pub(super) fn tool_errors() -> Vec<Golden> {
    let mut session = Session::start("tools.errors", World::empty(), &[]);
    session.tool("no_such_tool", json!({}));
    session.rpc("tools/call", json!({}));
    session.rpc("tools/call", json!({ "name": "validate_strategy", "arguments": 5 }));
    for tool in [
        "submit_order",
        "cancel_order",
        "modify",
        "flatten",
        "market_exit",
        "set_trading_state",
        "mass_cancel",
        "mount_strategy",
        "unmount_strategy",
        "set_setting",
        "delete_series",
    ] {
        session.tool(tool, json!({}));
    }
    let order = |extra: serde_json::Value| {
        let mut args = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
        for (k, v) in extra.as_object().expect("extra arguments are an object") {
            args[k] = v.clone();
        }
        args
    };
    session.tool("submit_order", order(json!({ "side": "buy" })));
    session.tool("submit_order", order(json!({ "qty": 0.0 })));
    session.tool("submit_order", order(json!({ "order_type": "no_such_type" })));
    session.tool("submit_order", order(json!({ "order_type": "limit" })));
    session.tool("submit_order", order(json!({ "account": "not a label!" })));
    session.tool("set_trading_state", json!({ "state": "no_such_state" }));
    let mount = json!({ "venue": "sim", "symbol": "BTCUSDT", "interval": "1m" });
    session.tool("mount_strategy", mount.clone());
    let mut both = mount.clone();
    both["name"] = json!("spread_maker");
    both["rhai"] = json!("a.rhai");
    session.tool("mount_strategy", both);
    let mut bad_params = mount;
    bad_params["name"] = json!("spread_maker");
    bad_params["params"] = json!(5);
    session.tool("mount_strategy", bad_params);
    let delete = json!({ "kind": "bar", "venue": "binance", "produced_by": "klines:" });
    for extra in [
        json!({ "produced_by": "crates/vike-data/src/klines.rs" }),
        json!({ "symbol": "BTCUSDT", "group": "G" }),
        json!({ "group": "G", "interval": "1h" }),
        json!({ "symbol": "BTC*" }),
        json!({ "symbol": "" }),
        json!({ "kind": "no_such_kind" }),
    ] {
        let mut args = delete.clone();
        for (k, v) in extra.as_object().expect("extra arguments are an object") {
            args[k] = v.clone();
        }
        session.tool("delete_series", args);
    }
    vec![session.finish().what("every bad call: `isError` bodies, no preview, no token").golden()]
}

/// The refusal of a tool the operator's launch withheld, by profile and by name, and a resource
/// reached around it.
pub(super) fn withheld_tools() -> Vec<Golden> {
    let order = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
    let mut read_only =
        Session::start("withheld.read-only", World::empty(), &["--profile", "read-only"]);
    read_only.tool("submit_order", order.clone());
    read_only.tool("delete_series", json!({ "kind": "bar", "venue": "binance" }));
    read_only.tool("no_such_tool", json!({}));

    let mut offline = Session::start("withheld.offline", World::empty(), &["--profile", "offline"]);
    offline.tool("submit_order", order.clone());
    offline.tool("run_backtest", json!({ "profile": "x" }));
    offline.tool("list_series", json!({}));
    offline.tool("node_snapshot", json!({}));
    offline.tool("health_node", json!({}));
    offline.tool("report", json!({}));

    let mut denied = Session::start(
        "withheld.deny",
        World::empty(),
        &["--deny-tool", "submit_order", "--deny-tool", "run_backtest"],
    );
    denied.tool("submit_order", order);
    // A deferred tool that is withheld stays inline: refused and answered at once.
    denied.tool("run_backtest", json!({ "profile": "x" }));
    denied.tool("cancel_order", json!({ "order_client_id": "c-1" }));

    let mut profile_and_deny = Session::start(
        "withheld.read-only-and-deny",
        World::empty(),
        &["--profile", "read-only", "--deny-tool", "validate_strategy"],
    );
    profile_and_deny.tool("validate_strategy", json!({ "script": "fn on_bar() { }" }));
    profile_and_deny.tool("submit_order", json!({}));

    [read_only, offline, denied, profile_and_deny]
        .into_iter()
        .map(|s| {
            s.finish().what("a withheld tool is refused by name; no preview, no token").golden()
        })
        .collect()
}

/// The node reads with no `--node`.
pub(super) fn node_reads_without_a_node() -> Vec<Golden> {
    let mut session = Session::start("tools.node_reads_no_node", World::empty(), &[]);
    session.tool("node_snapshot", json!({}));
    session.tool("strategy_status", json!({}));
    session.tool("settings_show", json!({}));
    session.tool("health_node", json!({}));
    session.tool("report", json!({}));
    vec![
        session
            .finish()
            .what("the node reads, no `--node`: an error, never an empty answer")
            .golden(),
    ]
}

/// A `--node` nothing listens on, with a key to present: `health_node` answers it as a RESULT, a
/// `verdict` of FAIL and the rows that say why, and `report` as an error, with its own argument errors
/// answered before any dial.
pub(super) fn node_reads_against_a_dead_node() -> Vec<Golden> {
    let world =
        World::with_store(&[("VIKE_TRADEHUB_OBSERVE_KEY", "DUMMY-observe-key-for-mcp-wire")]);
    let mut session =
        Session::start("tools.node_reads_dead_node", world, &["--node", "127.0.0.1:1"]);
    session.tool("health_node", json!({}));
    session.tool("report", json!({ "cash": 1000.0, "periods_per_year": 252.0 }));
    session.tool("report", json!({ "cash": "a lot" }));
    session.tool("report", json!({ "periods_per_year": [] }));
    vec![
        session
            .finish()
            .what("a dead node: `health_node` is a normal FAIL, `report` an error")
            .golden(),
    ]
}

/// The tools that dial a hub, against a port nothing listens on, and their argument errors, which
/// cost no socket.
pub(super) fn dead_daemons() -> Vec<Golden> {
    let mut session = Session::start("tools.dead_daemons", World::empty(), &[]);
    session.tool("run_backtest", json!({ "profile": "[strategy]\nname = \"buy_hold\"\n" }));
    session.tool("run_backtest", json!({}));
    session.tool("run_sweep", json!({ "profile": "x", "rank_by": "sharpe", "trials": 8 }));
    session.tool("run_sweep", json!({}));
    session.tool("run_sweep", json!({ "profile": "x", "optimizer": 5 }));
    session.tool("run_sweep", json!({ "profile": "x", "trials": [] }));
    session.tool("run_walk_forward", json!({ "profile": "x" }));
    session.tool("run_walk_forward", json!({}));
    session.tool("list_strategies", json!({}));
    session.tool("list_series", json!({}));
    vec![
        session
            .finish()
            .what("the hub tools against a dead port, and their argument errors")
            .golden(),
    ]
}

/// **Every tool that dials a hub dials it keyed**: the box's store holds the datahub pair, the fake
/// verifies each mac under it, and each scope is the one `vike-cli`'s verb asks for.
pub(super) fn keyed_dials() -> Vec<Golden> {
    const OBSERVE: &str = "DUMMY-observe-key-for-mcp-wire";
    const CONTROL: &str = "DUMMY-control-key-for-mcp-wire";
    let daemon = FakeDaemon::keyed(OBSERVE, CONTROL);
    let world = World::with_store(&[
        ("VIKE_DATAHUB_OBSERVE_KEY", OBSERVE),
        ("VIKE_DATAHUB_CONTROL_KEY", CONTROL),
    ])
    .scrub(&daemon.addr, "<daemon-addr>");
    let mut session = Session::start(
        "tools.keyed_dials",
        world,
        &["--addr", &daemon.addr, "--backtest-addr", &daemon.addr],
    );
    session.tool("run_backtest", json!({ "profile": "answer-backtest" }));
    session.tool("run_sweep", json!({ "profile": "answer-sweep" }));
    session.tool("run_walk_forward", json!({ "profile": "answer-walk" }));
    session.tool("list_strategies", json!({}));
    session.tool("list_series", json!({}));
    let golden = session.finish().what("a keyed hub, and a store holding its keys").golden();
    let dials = [
        ("Write", "answer-backtest"),
        ("Write", "answer-sweep"),
        ("Write", "answer-walk"),
        ("Read", "ListStrategies"),
        ("Read", "Inventory"),
    ];
    for (scope, request) in dials {
        assert_eq!(daemon.next_open(), Seen::Authed(scope.to_string()), "{request}");
        assert_eq!(daemon.next_open(), Seen::Request(request.to_string()));
    }
    vec![golden]
}

/// The same hub with no keys on this box: refused at the handshake, naming what to file.
pub(super) fn keyed_hub_and_no_keys() -> Vec<Golden> {
    let daemon = FakeDaemon::keyed("a", "b");
    let world = World::empty().scrub(&daemon.addr, "<daemon-addr>");
    let mut session = Session::start(
        "tools.keyed_hub_no_keys",
        world,
        &["--addr", &daemon.addr, "--backtest-addr", &daemon.addr],
    );
    session.tool("run_backtest", json!({ "profile": "x" }));
    session.tool("run_sweep", json!({ "profile": "x" }));
    session.tool("run_walk_forward", json!({ "profile": "x" }));
    session.tool("list_strategies", json!({}));
    session.tool("list_series", json!({}));
    vec![session.finish().what("a keyed hub, and a store holding no keys").golden()]
}
