//! `trade order decode`: offline from the id alone, with the names a node frame gives, with the strategy from this box's
//! `strategy_code` rows (read-only, the node's name first), and `--json` as the shared decode's own document.

use serde_json::{Value, json};
use vike_model::orders::client_id::OrderClientIdStrategyCode;
use vike_tradehub_client::wire::{CoreSnapshotWire, MountViewWire};

use super::{DecodeCommand, decode, names_a_strategy, text};

/// The CLI, no strategy, account #11, a plain close of a long, the stop leg of group `q7w2e9r4t1y6`.
const CLI_STOP: &str = "C00000Bq7w2e9r4t1y6FLSL";

/// A frame whose one block is bybit's `ALT` account, account code `0B` (#11).
fn frame() -> CoreSnapshotWire {
    let mut frame = CoreSnapshotWire::empty();
    frame.sequence = 1;
    let mut block: vike_tradehub_client::wire::VenueBlockWire = serde_json::from_value(json!({
        "venue": "bybit", "balance": 0.0, "realized_pnl": 0.0, "fees_paid": 0.0,
        "funding_paid": 0.0, "equity": 0.0, "unrealized": 0.0, "missing_prices": 0,
        "margin_used": 0.0, "free_buying_power": 0.0, "trading_state": "Active", "positions": [],
    }))
    .expect("a block");
    block.account = Some("ALT".into());
    block.route_key = "bybit#ALT".into();
    block.account_code = Some("0B".into());
    frame.venues = vec![block];
    frame
}

/// `trade order decode <args…>` through the plane's one parser.
fn parsed(args: &[&str]) -> DecodeCommand {
    let argv = ["order", "decode"].iter().chain(args).map(|s| s.to_string());
    match crate::cmd::trade::parse(argv).expect("a decode line").sub {
        Some(crate::cmd::trade::Sub::Order {
            verb: crate::cmd::trade::order::OrderVerb::Decode(c),
        }) => c,
        other => panic!("not read as decode: {other:?}"),
    }
}

#[test]
fn offline_the_id_alone_gives_the_codes_and_the_words() {
    let doc = decode(CLI_STOP, None, &[], None);
    assert_eq!((&doc["form"], &doc["source"]), (&json!("v2"), &json!("C")));
    assert_eq!((&doc["account"], &doc["account_number"]), (&json!("0B"), &json!(11)));
    assert_eq!((&doc["intent"], &doc["role"]), (&json!("FL"), &json!("SL")));
    assert_eq!(doc["account_label"], Value::Null, "no frame, no label");
    let words = doc["words"].as_str().expect("words");
    for said in ["CLI (by hand)", "no strategy", "account #11", "stop of group q7w2e9r4t1y6"] {
        assert!(words.contains(said), "{said:?} not in {words}");
    }
    assert_eq!(text(&doc), format!("{CLI_STOP}\n  {words}"), "nothing more to say offline");

    let v1 = decode("deadbeef17", None, &[], None);
    assert_eq!(text(&v1), "deadbeef17\n  minted before v2");
    assert!(text(&decode("EXT-1", None, &[], None)).contains("not a form this platform mints"));
}

#[test]
fn with_a_frame_the_node_names_the_account() {
    let doc = decode(CLI_STOP, Some(&frame()), &[], None);
    assert_eq!((&doc["account_label"], &doc["route_key"]), (&json!("ALT"), &json!("bybit#ALT")));
    let shown = text(&doc);
    assert!(shown.contains("account #11 ALT"), "{shown}");
    assert!(shown.ends_with("\n  account: bybit#ALT"), "{shown}");
}

/// The strategy behind a code from this box's vike.db when the node names none; the node's name when it does. The
/// lookup creates no store where there is none.
#[test]
fn a_local_store_names_the_strategy_the_node_did_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    let none = decode("S00010Bq7w2e9r4t1y6OL", None, &[], Some(dir.path()));
    assert_eq!(none["strategy_name"], Value::Null);
    assert!(!vike_secrets::db_path_in(dir.path()).exists(), "a decode never creates a store");

    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path()))
        .expect("a store");
    let id = vike_secrets::strategy_code::strategy_code_for(
        dir.path(),
        "bybit__BTCUSDT__30m",
        "ema_cross",
    )
    .expect("a code");
    let code = OrderClientIdStrategyCode::new(id).expect("in range");
    let order_client_id = format!("S{}0Bq7w2e9r4t1y6OL", code.as_str());

    let local = decode(&order_client_id, Some(&frame()), &[], Some(dir.path()));
    assert_eq!(local["strategy_name"], "ema_cross");
    assert_eq!(local["mount_id"], "bybit__BTCUSDT__30m");
    let words = local["words"].as_str().expect("words");
    assert!(words.contains(&format!("strategy {} ema_cross", code.as_str())), "{words}");
    assert!(words.contains("account #11 ALT"), "the node's label stays: {words}");
    assert!(text(&local).contains("\n  mount: bybit__BTCUSDT__30m"), "{}", text(&local));

    let mount: MountViewWire = serde_json::from_value(json!({
        "strategy": "spread_maker", "params": "", "live": false, "mount_id": "bybit__BTCUSDT__1m",
        "order_client_id_code": code.as_str(),
    }))
    .expect("a mount row");
    let named = decode(&order_client_id, Some(&frame()), &[mount], Some(dir.path()));
    assert_eq!(named["strategy_name"], "spread_maker", "the node's name first");
    assert_eq!(named["mount_id"], "bybit__BTCUSDT__1m");
}

/// `--node` reads the mount rows only for an id carrying a strategy code other than `0000`.
#[test]
fn only_an_id_with_a_strategy_costs_the_mount_read() {
    assert!(names_a_strategy("S00010Bq7w2e9r4t1y6OL"));
    for id in [CLI_STOP, "deadbeef17", "EXT-1"] {
        assert!(!names_a_strategy(id), "{id}");
    }
}

/// `--json` prints the shared decode's document (the MCP tool's answer): one JSON value, `--node` optional.
#[test]
fn json_is_the_shared_decodes_document() {
    let c = parsed(&[CLI_STOP, "--json"]);
    assert!(c.json && c.node.is_none(), "{c:?}");
    assert_eq!(parsed(&["--node", "h:1", CLI_STOP]).node.as_deref(), Some("h:1"));
    let doc = decode(&c.id, None, &[], None);
    assert_eq!(doc, vike_client::verbs::decode_order_client_id(CLI_STOP, None, &[]));
    let printed = serde_json::to_string_pretty(&doc).expect("serialises");
    assert_eq!(serde_json::from_str::<Value>(&printed).expect("one document"), doc);
}
