//! The venue gate on this door: a write naming a venue the node does not mount is refused, never guessed. The rule
//! is `vike_client::order_gate`'s and is pinned there verb by verb; this holds the tools' arguments and this door's
//! words to it.

use super::write_args::every_write_tools_arguments;
use super::*;
use crate::venue_gate::{NOTHING_PREVIEWED, VENUE_CHECK_NONE, gate_refusal, venue_check_word};
use vike_client::order_gate::{NodeLink, judge};
use vike_tradehub_client::wire::{CoreSnapshotWire, VenueBlockWire};

/// The venue a write NAMES, per wire variant — the input half of the gate.
///
/// ⚠ The two OPTIONAL rows are the ones worth having a test for. `close_all` and
/// `mass_cancel` mean "every engine" when the argument is omitted, so an omission must read as
/// "nothing to check" and never as "an empty venue to compare" — a gate that refused there
/// would take out the panic button, which is a worse failure than the one being closed.
#[test]
fn the_venue_a_write_names_is_read_per_variant() {
    let of = |tool: &str, args: Value| {
        let cmd = tool_verbs::wire_command_for(tool, &args).expect("a valid write");
        cmd.addressed_venue().map(str::to_string)
    };
    assert_eq!(
        of("submit_order", json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0 })),
        Some("sim".to_string())
    );
    assert_eq!(of("flatten", json!({ "venue": "sim", "symbol": "B" })), Some("sim".to_string()));
    assert_eq!(of("close_all", json!({ "venue": "sim" })), Some("sim".to_string()));
    assert_eq!(of("mass_cancel", json!({ "venue": "sim" })), Some("sim".to_string()));
    // ...and the venue-less shapes, each of which must be checked against nothing.
    assert_eq!(of("close_all", json!({})), None, "an omitted venue means EVERY engine");
    assert_eq!(of("mass_cancel", json!({})), None, "an omitted venue means EVERY venue");
    assert_eq!(of("cancel_order", json!({ "order_client_id": "c1" })), None);
    assert_eq!(of("modify_order", json!({ "order_client_id": "c1", "new_qty": 2.0 })), None);
    assert_eq!(of("set_trading_state", json!({ "state": "halted" })), None);
}

/// A frame whose engines are `keys`, one block each.
fn frame_of(keys: &[&str]) -> CoreSnapshotWire {
    let mut frame = CoreSnapshotWire::empty();
    frame.sequence = 1;
    frame.venues = keys
        .iter()
        .map(|key| {
            let mut block: VenueBlockWire = serde_json::from_value(json!({
                "venue": key.split('#').next().unwrap_or(key), "balance": 0.0, "realized_pnl": 0.0,
                "fees_paid": 0.0, "funding_paid": 0.0, "equity": 0.0, "unrealized": 0.0,
                "missing_prices": 0, "margin_used": 0.0, "free_buying_power": 0.0,
                "trading_state": "Active", "positions": [],
            }))
            .expect("a block");
            block.route_key = (*key).into();
            block.account = key.split_once('#').map(|(_, label)| label.to_string());
            block
        })
        .collect();
    frame
}

/// What this door answers `tool` with `args` on a node running `keys`: the `venue_check` word, or its refusal.
fn verdict(tool: &str, args: &Value, keys: &[&str]) -> Result<&'static str, String> {
    let cmd = tool_verbs::wire_command_for(tool, args).expect("a valid write");
    judge(&cmd, Default::default(), Ok(&frame_of(keys)), NodeLink::default())
        .map(|(_, check)| venue_check_word(check))
        .map_err(|refusal| gate_refusal(&refusal, NOTHING_PREVIEWED))
}

/// **A venue the node does not mount is refused, and the refusal names what is mounted, the read tool, and that the
/// two-call gate was not half given away** — the 2026-09-06 model run's `venue: "node"`, which every layer accepted.
#[test]
fn a_venue_the_node_does_not_mount_is_refused_and_the_refusal_names_what_is_mounted() {
    let args = json!({ "venue": "node", "symbol": "B", "side": 1, "qty": 1.0 });
    let err = verdict("submit_order", &args, &["polymarket", "binance"]).expect_err("not mounted");
    assert!(err.starts_with("error [order.edge.venue_not_mounted]: "), "{err}");
    assert!(err.contains("`node`") && err.contains("polymarket, binance"), "{err}");
    assert!(err.contains("node_snapshot") && err.contains("no preview_token was issued"), "{err}");
}

/// **The tool-argument half of the two-account chain**: a protective stop on a venue that mounts a testnet account and
/// a `LIVE` one is placeable by naming the `account`, and refused, naming the books, without one.
#[test]
fn submit_orders_account_argument_resolves_a_two_account_venue() {
    let keys = ["hyperliquid", "hyperliquid#LIVE"];
    let stop = |account: Option<&str>| {
        let mut args = json!({
            "venue": "hyperliquid", "symbol": "ETH", "side": -1, "qty": 0.01,
            "order_type": "stop", "trigger_price": 1800.0, "reduce_only": true,
        });
        if let Some(a) = account {
            args["account"] = json!(a);
        }
        verdict("submit_order", &args, &keys)
    };
    let err = stop(None).expect_err("an account-less order on a two-account venue is refused");
    assert!(err.starts_with("error [order.edge.bare_venue_of_several_accounts]: "), "{err}");
    assert!(err.contains("REFUSED") && err.contains("`account`"), "{err}");
    assert_eq!(stop(Some("LIVE")), Ok(VENUE_CHECK_MOUNTED));
    assert_eq!(stop(Some("DEFAULT")), Ok(VENUE_CHECK_MOUNTED));
    let err = stop(Some("DEMO")).expect_err("DEMO is not mounted here");
    assert!(err.starts_with("error [order.edge.account_not_mounted]: "), "{err}");
}

/// **A tool call that opens or closes on a venue and names no account reaches exactly one account, or it is
/// refused** (decision 0254), over the CI box's four shapes and every such tool; a named account goes through.
#[test]
fn an_unnamed_venue_tool_call_resolves_to_one_account_or_is_refused() {
    let shapes: [(&str, &[&str], &str); 4] = [
        ("deribit", &["deribit"], ""),
        ("binance", &["binance", "binance#PERP"], ""),
        ("hyperliquid", &["hyperliquid", "hyperliquid#LIVE"], "LIVE"),
        ("bybit", &["bybit#DEMO", "bybit#LIVE"], "LIVE"),
    ];
    for (venue, keys, named) in shapes {
        for (tool, args) in [
            ("close_all", json!({ "venue": venue })),
            ("mass_cancel", json!({ "venue": venue })),
            ("flatten", json!({ "venue": venue, "symbol": "BTC" })),
            ("submit_order", json!({ "venue": venue, "symbol": "BTC", "side": 1, "qty": 1.0 })),
        ] {
            if named.is_empty() {
                assert_eq!(verdict(tool, &args, keys), Ok(VENUE_CHECK_MOUNTED), "{tool} {venue}");
                continue;
            }
            let err = verdict(tool, &args, keys).expect_err("an unnamed venue of two accounts");
            assert!(err.contains("REFUSED") && err.contains("nothing was sent"), "{err}");
            for key in keys {
                let book = vike_tradehub_client::wire::book_of_route_key(venue, key);
                assert!(err.contains(&book), "{book} missing from: {err}");
            }
            let mut naming = args.clone();
            naming["account"] = json!(named);
            assert_eq!(
                verdict(tool, &naming, keys),
                Ok(VENUE_CHECK_MOUNTED),
                "{tool} {venue} {named}"
            );
        }
    }
}

/// ⚠ A venue whose name is a PREFIX of another's is not that other's account, and the compare is EXACT: routing
/// compares with `==`, so a case or a space routes to nothing and must be refused.
#[test]
fn the_venue_comparison_is_exact_and_a_prefix_venue_is_no_account() {
    let order = |venue: &str| json!({ "venue": venue, "symbol": "B", "side": 1, "qty": 1.0 });
    assert_eq!(
        verdict("submit_order", &order("binance"), &["binance", "binanceus"]),
        Ok(VENUE_CHECK_MOUNTED)
    );
    for spelled in ["Polymarket", "polymarket "] {
        assert!(verdict("submit_order", &order(spelled), &["polymarket"]).is_err(), "{spelled:?}");
    }
}

/// A preview against a node-less server is UNVERIFIED, not refused — and it SAYS so.
///
/// The end-to-end half of the disposition above, over the shipped router rather than the pure
/// function: `test_server()` has no node, so the mounted set cannot be read, and every write
/// tool must still preview exactly as it did before this gate existed.
#[test]
fn a_preview_with_no_node_reports_the_venue_as_unverified_rather_than_refusing_it() {
    let resp =
        call("submit_order", json!({ "venue": "node", "symbol": "B", "side": 1, "qty": 1.0 }));
    assert_eq!(resp["result"]["isError"], false, "no node = no evidence = no refusal: {resp}");
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["venue_check"], VENUE_CHECK_UNVERIFIED, "{sc}");
    assert!(sc["preview_token"].is_string(), "an unverified preview still mints a token: {sc}");
}

/// Every write tool's preview carries the field, so an agent never has to infer from its
/// absence whether the venue was looked at.
#[test]
fn every_write_tool_preview_reports_its_venue_check() {
    for tool in WRITE_TOOLS {
        let resp = call(tool, every_write_tools_arguments("c1"));
        let sc = &resp["result"]["structuredContent"];
        let got = sc["venue_check"].as_str().unwrap_or_default();
        assert!(
            [VENUE_CHECK_MOUNTED, VENUE_CHECK_NONE, VENUE_CHECK_UNVERIFIED].contains(&got),
            "{tool}: every preview reports one of the three dispositions, got {sc}"
        );
    }
}

/// **A cancel naming an id outside the charset is previewed with the CLI's warning, and still sent** (it names an
/// order that already exists: refusing it could make a live order uncancellable).
#[test]
fn a_cancel_preview_warns_of_an_id_outside_the_charset_and_keeps_its_token() {
    let resp = call("cancel_order", json!({ "order_client_id": "BAD-ID-3" }));
    let sc = &resp["result"]["structuredContent"];
    let note = sc["order_client_id_note"].as_str().expect("the CLI's warning");
    assert!(note.contains("could have minted") && note.contains("sending it anyway"), "{note}");
    assert!(sc["preview_token"].is_string(), "{sc}");
    let clean = call("cancel_order", json!({ "order_client_id": "Good1" }));
    assert!(clean["result"]["structuredContent"].get("order_client_id_note").is_none());
}
