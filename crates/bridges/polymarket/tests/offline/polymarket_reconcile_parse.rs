//! Pure parse tests for the Polymarket `ReconClient` report parsers — NO network: feeds synthetic
//! CLOB / data-api JSON straight to `vike_polymarket::recon_client`'s pure `parse_*` functions and
//! asserts the prediction-market-specific bits the module doc calls out: the CLOB-id → coid registry
//! re-keying (orders AND fills), the composite `trade_id = "{id}:{order_id}"` that dedups against the
//! live path's `seen_trade_ids`, the maker-vs-taker fill split using OUR registered side, the
//! fill-progress-derived order status, the one-way (`Both`, `qty >= 0`) position mapping, and the
//! 6-decimal USDC balance division. Mirrors `vike-ibkr`/`alpaca`'s recon parse tests.
//!
//! The bodies use REAL Polymarket wire shapes, not toy placeholders (see the `TOKEN_*`/`CONDITION_*`/
//! order-hash constants below), so a genuine format quirk cannot sail past: outcome token_ids are
//! ~77-digit ERC-1155 decimal strings and CLOB order/condition/trade ids are `0x` + 64 hex. The
//! token_ids and condition hashes are VERBATIM from the live-captured frames pinned in `src/ws.rs`
//! (dated 2026-07-08) and `src/order.rs`; the per-order/-trade hashes are representative 32-byte
//! values (account-specific ids are faithful in FORMAT but never a verbatim commit of a real order).
//!
//! Integration tests compile as their own crate; the whole library is behind the non-default
//! `polymarket` feature, so the file is gated (else a default-feature `cargo test` breaks — audit
//! G2, the same gate every polymarket integration test carries).

use vike_model::events::{LiquiditySide, PositionSide};
use vike_polymarket::PolymarketRegistry;
use vike_polymarket::recon_client::{
    normalize_order_status, parse_balance, parse_fill_reports, parse_order_reports,
    parse_position_reports,
};

// --- real Polymarket wire formats ---------------------------------------------------------------
// Outcome token_ids (the ERC-1155 `asset`/`asset_id`, i.e. the venue "symbol") are ~77-digit
// decimal strings — these five are VERBATIM from the live-captured `book`/`price_change`/
// `last_trade` frames in `src/ws.rs` and the order example in `src/order.rs`. Condition/market ids
// are the `0x` + 64-hex hashes from those same frames. The parsers key on these as opaque strings,
// so a real 77-digit token vs a toy `"77"` is what actually flows into `symbol`/`venue_order_id`.
const TOKEN_YES: &str =
    "71321045679252212594626385532706912750332728571942532289631379312455583992563";
const TOKEN_A: &str =
    "7589880081658059374445882095611024032084645048848237404312287942463647821549";
const TOKEN_B: &str =
    "24395104702642353411948925889097630568855366590528757185881735998149149192479";
const TOKEN_C: &str =
    "20904118177412102896316191568338021059060941327365407257046608077345075967207";
const TOKEN_EXT: &str =
    "93005850938352995663334573245996733794924636935158112548608169054144721737755";
const CONDITION_A: &str = "0x66bbf6d55e0296278858b3147689f3df9259374f158f9f028b608baa322a639c";
const CONDITION_B: &str = "0x418fcd9c72501eea029eee747b522e4da91478dbf52829893aa7a55b36d84984";

// CLOB order ids are the keccak order-hash (`0x` + 64 hex, exactly like the signed-order hash in
// `src/order.rs`). Named by their role in the registry so the re-keying stays readable.
const OUR_BUY: &str = "0xd375be993c3e1a53dd8be58f66fbdb3eca151058abeb239f4ab7131be318179b"; // registry coid-buy, +1
const OUR_SELL: &str = "0x1a8c1030fe43518310d2a0c6dad04f8087eb0bcb989fa5910f34481524c72f4b"; // registry coid-sell, -1
const EXTERNAL: &str = "0xca6aa3896b22eec752556fe1b38ebb8e90d936b27403ca019eb4d53817565fe7"; // not in the registry
const TAKER_OTHER: &str = "0xbb943ae23b4147cd3c2cb1a777aa491f9a1fa1442454992335d78e5ce58d75b9"; // someone else's taker
const MAKER_OTHER: &str = "0xe6228c80657af5a575fa74a7e954abdacedda9676337382193e735e70b026c4e"; // a maker that isn't ours
const MAKER_OTHER2: &str = "0x4f3ebfb47bea4f5062c509b1b0dffbb3b2d8d1ff94f66d2043f499379c8e2c62"; // another maker not ours

// /data/trades trade ids (`0x` + 64 hex, the on-chain match id).
const TRADE_1: &str = "0x39bd1a6cff56b2f61b7fd6e07089d387802bd34b6b929bd3a01f3970b1f38f5b";
const TRADE_2: &str = "0xfd28a98287ffda9e5c9e895a1ffb567c6363c8e9312da3bf377403d3116d3a61";
const TRADE_3: &str = "0xcfba5940426639fa8424331ad06e40834c0690c439bcad8750a191eb477a1074";

/// A registry pre-seeded with two of OUR orders: a BUY taker (`OUR_BUY`) and a SELL maker
/// (`OUR_SELL`). The `lookup` closure the parsers take is this registry's `lookup_clob`.
fn seeded_registry() -> PolymarketRegistry {
    let reg = PolymarketRegistry::new();
    let _ = reg.on_accept("coid-buy", OUR_BUY, 1);
    let _ = reg.on_accept("coid-sell", OUR_SELL, -1);
    reg
}

// --- parse_order_reports ------------------------------------------------------------------------

#[test]
fn order_rekeyed_to_coid_all_fields() {
    let reg = seeded_registry();
    let body = serde_json::json!([{
        "id": OUR_BUY, "status": "LIVE", "market": CONDITION_A, "asset_id": TOKEN_YES,
        "side": "BUY", "original_size": "100", "size_matched": "0", "price": "0.42",
        "order_type": "GTC", "created_at": 1_700_000_000
    }]);
    let r = parse_order_reports(&body, |id| reg.lookup_clob(id)).unwrap();
    assert_eq!(r.len(), 1);
    let o = &r[0];
    assert_eq!(o.venue, "polymarket");
    assert_eq!(o.symbol, TOKEN_YES, "asset_id → symbol (the outcome token_id)");
    assert_eq!(o.venue_order_id.as_str(), OUR_BUY);
    assert_eq!(o.client_order_id.as_deref(), Some("coid-buy"), "CLOB id re-keyed to coid");
    assert_eq!(o.side, 1, "BUY → +1");
    assert_eq!(o.order_type, "gtc", "lower-cased");
    assert_eq!(o.qty, 100.0);
    assert_eq!(o.filled_qty, 0.0);
    assert_eq!(o.avg_px, 0.0);
    assert_eq!(o.status, "ACCEPTED", "untouched active order");
    assert_eq!(o.ts, 1_700_000_000);
}

#[test]
fn order_external_maps_client_id_none() {
    let reg = seeded_registry();
    // an order id the registry does not know → client_order_id None → UnknownOrder downstream
    let body = serde_json::json!([{
        "id": EXTERNAL, "status": "LIVE", "asset_id": TOKEN_EXT,
        "side": "SELL", "original_size": "5", "size_matched": "0", "order_type": "GTC"
    }]);
    let o = &parse_order_reports(&body, |id| reg.lookup_clob(id)).unwrap()[0];
    assert_eq!(o.client_order_id, None);
    assert_eq!(o.side, -1, "SELL → -1");
}

#[test]
fn order_partial_progress_is_partially_filled() {
    let reg = seeded_registry();
    let body = serde_json::json!([{
        "id": OUR_BUY, "status": "LIVE", "asset_id": TOKEN_YES,
        "side": "BUY", "original_size": "100", "size_matched": "40", "order_type": "GTC"
    }]);
    let o = &parse_order_reports(&body, |id| reg.lookup_clob(id)).unwrap()[0];
    assert_eq!(o.status, "PARTIALLY_FILLED");
    assert_eq!(o.filled_qty, 40.0);
}

#[test]
fn orders_accept_data_wrapper_and_numeric_fields() {
    let reg = seeded_registry();
    // the paginated `{data:[..]}` shape, plus number-encoded sizes (not strings)
    let body = serde_json::json!({ "data": [{
        "id": OUR_BUY, "status": "LIVE", "asset_id": TOKEN_YES,
        "side": "BUY", "original_size": 12, "size_matched": 0, "order_type": "gtc"
    }], "next_cursor": "MA==" });
    let r = parse_order_reports(&body, |id| reg.lookup_clob(id)).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].qty, 12.0);
}

#[test]
fn orders_non_array_is_err_not_panic() {
    let reg = seeded_registry();
    assert!(
        parse_order_reports(&serde_json::json!({"error": "x"}), |id| reg.lookup_clob(id)).is_err()
    );
}

// --- parse_fill_reports -------------------------------------------------------------------------

#[test]
fn taker_fill_rekeyed_composite_trade_id() {
    let reg = seeded_registry();
    let body = serde_json::json!([{
        "id": TRADE_1, "status": "MATCHED", "asset_id": TOKEN_YES,
        "side": "BUY", "size": "100", "price": "0.52", "match_time": "1700000005",
        "taker_order_id": OUR_BUY, "maker_orders": []
    }]);
    let r = parse_fill_reports(&body, |id| reg.lookup_clob(id)).unwrap();
    assert_eq!(r.len(), 1);
    let f = &r[0];
    assert_eq!(f.venue, "polymarket");
    assert_eq!(f.symbol, TOKEN_YES);
    assert_eq!(
        f.trade_id.as_str(),
        format!("{TRADE_1}:{OUR_BUY}"),
        "composite id matches the live seen_trade_ids"
    );
    assert_eq!(f.venue_order_id.as_str(), OUR_BUY);
    assert_eq!(f.client_order_id.as_deref(), Some("coid-buy"));
    assert_eq!(f.side, 1);
    assert_eq!(f.last_qty, 100.0);
    assert_eq!(f.last_px, 0.52);
    assert_eq!(f.liquidity_side, LiquiditySide::Taker);
    assert_eq!(f.commission, 0.0);
    assert_eq!(f.ts, 1_700_000_005);
}

#[test]
fn maker_fill_uses_registered_side_and_maker_entry() {
    let reg = seeded_registry();
    // our resting SELL (OUR_SELL, side -1) is hit; the trade's TOP-LEVEL side is the taker's BUY, but
    // our fill must carry OUR registered side and the maker entry's matched_amount/price.
    let body = serde_json::json!([{
        "id": TRADE_2, "status": "CONFIRMED", "asset_id": TOKEN_YES,
        "side": "BUY", "size": "100", "price": "0.99", "match_time": "1700000006",
        "taker_order_id": TAKER_OTHER,
        "maker_orders": [{ "order_id": OUR_SELL, "matched_amount": "40", "price": "0.55" }]
    }]);
    let r = parse_fill_reports(&body, |id| reg.lookup_clob(id)).unwrap();
    assert_eq!(r.len(), 1);
    let f = &r[0];
    assert_eq!(f.trade_id.as_str(), format!("{TRADE_2}:{OUR_SELL}"));
    assert_eq!(f.client_order_id.as_deref(), Some("coid-sell"));
    assert_eq!(f.side, -1, "OUR registered side, not the taker's BUY");
    assert_eq!(f.last_qty, 40.0, "the maker entry's matched_amount");
    assert_eq!(f.last_px, 0.55, "the maker entry's price");
    assert_eq!(f.liquidity_side, LiquiditySide::Maker);
}

#[test]
fn trade_matching_both_our_sides_emits_two_fills() {
    let reg = seeded_registry();
    // one trade where we are BOTH the taker (OUR_BUY) AND a maker (OUR_SELL) — two distinct fills.
    let body = serde_json::json!([{
        "id": TRADE_3, "status": "MINED", "asset_id": TOKEN_YES,
        "side": "BUY", "size": "10", "price": "0.5",
        "taker_order_id": OUR_BUY,
        "maker_orders": [
            { "order_id": OUR_SELL, "matched_amount": "3", "price": "0.5" },
            { "order_id": MAKER_OTHER, "matched_amount": "7", "price": "0.5" }
        ]
    }]);
    let r = parse_fill_reports(&body, |id| reg.lookup_clob(id)).unwrap();
    assert_eq!(r.len(), 2, "our taker + our maker; the third maker is not ours");
    let ids: Vec<_> = r.iter().map(|f| f.trade_id.to_string()).collect();
    assert!(ids.contains(&format!("{TRADE_3}:{OUR_BUY}")));
    assert!(ids.contains(&format!("{TRADE_3}:{OUR_SELL}")));
}

/// A trade row with no wire `id` is SKIPPED on BOTH legs, rather than reported under the composite
/// `":{order_id}"`. This gates the empty-`id` refusal in `parse_fill_reports`, not the type:
/// reverting it to a permissive composite turns the asserted 1 back into 3.
///
/// The hazard is specific to this side of the fold. An id-less `FillReport` can never match
/// `local.seen_trade_ids`, so it does not merely fail to dedup — it FABRICATES a `MissingFill`
/// divergence, and `MissingFill` is one of the two kinds the `hybrid` reconcile policy
/// AUTO-APPLIES: the invented divergence books the fill a second time with no operator in front of
/// it. A skipped row costs one pass of visibility (the next pass re-reads the same window).
#[test]
fn a_trade_row_without_an_id_is_skipped_on_both_legs() {
    let reg = seeded_registry();
    let leg = |id: serde_json::Value| {
        serde_json::json!({
            "id": id, "status": "MATCHED", "asset_id": TOKEN_YES,
            "side": "BUY", "size": "100", "price": "0.52", "match_time": "1700000005",
            "taker_order_id": OUR_BUY,
            "maker_orders": [{ "order_id": OUR_SELL, "matched_amount": "40", "price": "0.55" }]
        })
    };
    let mut absent = leg(serde_json::Value::Null);
    absent.as_object_mut().expect("object").remove("id");

    for (label, row) in [
        ("absent", absent),
        ("null", leg(serde_json::Value::Null)),
        ("empty", leg(serde_json::json!(""))),
    ] {
        let r = parse_fill_reports(&row_body(row), |id| reg.lookup_clob(id)).unwrap();
        assert!(
            r.is_empty(),
            "an `id`-{label} trade row must report NO fill (taker or maker): {r:?}"
        );
    }

    // ...and the skip is per-row: a good row alongside a bad one still reports.
    let body = serde_json::json!([leg(serde_json::json!("")), leg(serde_json::json!(TRADE_1))]);
    let r = parse_fill_reports(&body, |id| reg.lookup_clob(id)).unwrap();
    assert_eq!(r.len(), 2, "only the id-less row is skipped (the good row has both our legs)");
    let ids: Vec<_> = r.iter().map(|f| f.trade_id.to_string()).collect();
    assert!(ids.contains(&format!("{TRADE_1}:{OUR_BUY}")), "{ids:?}");
    assert!(ids.contains(&format!("{TRADE_1}:{OUR_SELL}")), "{ids:?}");
}

/// One trade row wrapped as the bare array the parser takes.
fn row_body(row: serde_json::Value) -> serde_json::Value {
    serde_json::Value::Array(vec![row])
}

#[test]
fn non_fillable_and_foreign_trades_emit_nothing() {
    let reg = seeded_registry();
    // FAILED/RETRYING never fill; and a trade for no order of ours is skipped.
    let failed = serde_json::json!([{
        "id": TRADE_1, "status": "FAILED", "asset_id": TOKEN_YES, "side": "BUY", "size": "1", "price": "0.5",
        "taker_order_id": OUR_BUY, "maker_orders": []
    }]);
    assert!(parse_fill_reports(&failed, |id| reg.lookup_clob(id)).unwrap().is_empty());
    let foreign = serde_json::json!([{
        "id": TRADE_1, "status": "MATCHED", "asset_id": TOKEN_YES, "side": "BUY", "size": "1", "price": "0.5",
        "taker_order_id": TAKER_OTHER,
        "maker_orders": [{ "order_id": MAKER_OTHER2, "matched_amount": "1", "price": "0.5" }]
    }]);
    assert!(parse_fill_reports(&foreign, |id| reg.lookup_clob(id)).unwrap().is_empty());
}

#[test]
fn trades_non_array_is_err() {
    let reg = seeded_registry();
    assert!(
        parse_fill_reports(&serde_json::json!({"error": "x"}), |id| reg.lookup_clob(id)).is_err()
    );
}

// --- parse_position_reports ---------------------------------------------------------------------

#[test]
fn positions_one_way_holdings() {
    let body = serde_json::json!([
        {"conditionId": CONDITION_A, "asset": TOKEN_A, "size": 100.0, "avgPrice": 0.52, "redeemable": false},
        {"conditionId": CONDITION_B, "asset": TOKEN_B, "size": 0.0, "avgPrice": 0.0, "redeemable": false}
    ]);
    let r = parse_position_reports(&body).unwrap();
    assert_eq!(r.len(), 2, "zero-size rows are kept (stale-position detection)");
    let p = &r[0];
    assert_eq!(p.venue, "polymarket");
    assert_eq!(p.symbol, TOKEN_A, "asset → symbol");
    assert_eq!(p.position_side, PositionSide::Both, "Polymarket holdings are one-way → BOTH");
    assert_eq!(p.qty, 100.0);
    assert_eq!(p.avg_px, 0.52, "avgPrice → avg_px");
    assert_eq!(r[1].qty, 0.0);
}

#[test]
fn positions_skip_malformed_and_accept_wrapper() {
    let body = serde_json::json!({ "data": [
        {"conditionId": CONDITION_A, "size": 5.0},           // missing asset → skipped
        {"asset": TOKEN_C, "size": 7.0, "avgPrice": 0.3}     // ok
    ]});
    let r = parse_position_reports(&body).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].symbol, TOKEN_C);
}

#[test]
fn positions_non_array_is_err() {
    assert!(parse_position_reports(&serde_json::json!({"error": "bad user"})).is_err());
}

// --- parse_balance ------------------------------------------------------------------------------

#[test]
fn balance_base_units_to_usdc() {
    // 6-decimal base units → whole USDC
    assert_eq!(parse_balance(&serde_json::json!({"balance":"1000000"})).unwrap(), Some(1.0));
    assert_eq!(parse_balance(&serde_json::json!({"balance":"12500000"})).unwrap(), Some(12.5));
    // a numeric encoding works too
    assert_eq!(parse_balance(&serde_json::json!({"balance": 2_500_000})).unwrap(), Some(2.5));
}

#[test]
fn balance_absent_is_none_non_object_is_err() {
    assert_eq!(parse_balance(&serde_json::json!({"allowance":"0"})).unwrap(), None);
    assert!(parse_balance(&serde_json::json!([])).is_err());
}

// --- normalize_order_status (the full table) ----------------------------------------------------

#[test]
fn order_status_table() {
    assert_eq!(normalize_order_status("LIVE", 0.0, 100.0), "ACCEPTED");
    assert_eq!(normalize_order_status("LIVE", 40.0, 100.0), "PARTIALLY_FILLED");
    assert_eq!(normalize_order_status("MATCHED", 100.0, 100.0), "FILLED");
    assert_eq!(normalize_order_status("CANCELED", 0.0, 100.0), "CANCELED");
    assert_eq!(normalize_order_status("anything-CANCELLATION", 0.0, 100.0), "CANCELED");
    // an empty book with no progress is still ACCEPTED (never divides by zero)
    assert_eq!(normalize_order_status("LIVE", 0.0, 0.0), "ACCEPTED");
}
