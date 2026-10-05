//! An OANDA `ORDER_FILL` with no `id` yields NO events, and a fill report with no `id` is skipped.
//!
//! Both decode paths used to spell `s("id").unwrap_or_default()`, so an absent `id` became `""` —
//! and an empty `trade_id` skipped `ExecutionEngine`'s dedup guard entirely rather than deduping
//! badly, so the fill was applied unconditionally. This venue REPLAYS by design (see
//! `crates/bridges/oanda/src/stream.rs`'s `stream_transactions` audit-A3 note: on reconnect it
//! backfills `/transactions/sinceid` through the SAME decode path), so an un-dedupable fill here
//! re-books its commission and realized PnL on every reconnect.
//!
//! OANDA's transaction `id` is this venue's ONLY per-fill identity — `orderID` is shared by every
//! fill of a multi-fill order — so there is nothing replay-stable to synthesize from, and the chosen
//! policy is to REFUSE the frame.

use serde_json::json;
use vike_model::events::Event;
use vike_oanda::decode_transaction_events;
use vike_oanda::recon_client::parse_fill_reports;

fn order_fill(id: Option<&str>) -> serde_json::Value {
    let mut v = json!({
        "type": "ORDER_FILL",
        "instrument": "EUR_USD",
        "units": "1000",
        "price": "1.0912",
        "commission": "0.05",
        "time": "1700000000.000000000",
        "orderID": "77",
        "clientExtensions": { "id": "coid-1" },
    });
    if let Some(id) = id {
        v["id"] = json!(id);
    }
    v
}

#[test]
fn a_well_formed_order_fill_still_dual_publishes() {
    // The control: without this the "no events" assertions below would pass for the wrong reason.
    let evs = decode_transaction_events(&order_fill(Some("6373")));
    assert_eq!(evs.len(), 2, "bare Fill + OrderFilled wrap: {evs:?}");
    match &evs[0] {
        Event::Fill(f) => assert_eq!(f.trade_id, "6373"),
        other => panic!("expected the bare Fill first, got {other:?}"),
    }
    assert!(matches!(&evs[1], Event::OrderFilled(w) if w.fill.trade_id == "6373"));
}

#[test]
fn an_order_fill_without_an_id_publishes_nothing() {
    assert!(
        decode_transaction_events(&order_fill(None)).is_empty(),
        "an id-less ORDER_FILL must publish NO events — folding it would re-book on every \
         reconnect backfill"
    );
}

#[test]
fn an_order_fill_with_an_empty_id_publishes_nothing() {
    // The shape the old `unwrap_or_default()` produced from an absent field, spelled explicitly by
    // the venue. Both must be refused, or the venue can still choose which door to walk through.
    assert!(decode_transaction_events(&order_fill(Some(""))).is_empty());
}

#[test]
fn neither_half_of_the_dual_publish_escapes_alone() {
    // The bare `Event::Fill` (the Account's copy) and the `OrderFilled` wrap (the FSM's copy) are two
    // views of ONE execution. A refused frame must drop BOTH — publishing the wrap alone would
    // advance the order FSM for a fill the Account never booked.
    for id in [None, Some("")] {
        let evs = decode_transaction_events(&order_fill(id));
        assert!(!evs.iter().any(|e| matches!(e, Event::Fill(_))), "leaked a bare Fill: {evs:?}");
        assert!(
            !evs.iter().any(|e| matches!(e, Event::OrderFilled(_))),
            "leaked an OrderFilled wrap without its bare Fill: {evs:?}"
        );
    }
}

// --- the reconcile lane -------------------------------------------------------------------------

fn fill_txn(id: Option<&str>) -> serde_json::Value {
    let mut v = json!({
        "type": "ORDER_FILL",
        "instrument": "EUR_USD",
        "units": "1000",
        "price": "1.0912",
        "commission": "0.05",
        "time": "1700000000.000000000",
        "orderID": "77",
    });
    if let Some(id) = id {
        v["id"] = json!(id);
    }
    v
}

#[test]
fn an_id_less_fill_report_row_is_skipped_and_the_good_rows_survive() {
    // Why SKIP rather than admit: `vike_exec::recon::diff` matches a report against the engine's
    // `seen_trade_ids`. An empty id can never match, so it does not merely go unrecognised — it
    // FABRICATES a `MissingFill` divergence, and `MissingFill` is one of the two kinds the `hybrid`
    // policy AUTO-APPLIES, i.e. it books the fill a second time with no operator in front of it.
    // Skipping yields no divergence at all, which is the safe direction. Failing the whole batch
    // would instead discard the good rows along with the bad one — hence one row, not the request.
    let body = json!({
        "transactions": [fill_txn(Some("6373")), fill_txn(None), fill_txn(Some("")), fill_txn(Some("6375"))]
    });
    let reports = parse_fill_reports(&body, "EUR_USD", "EURUSD", "USD");
    let ids: Vec<&str> = reports.iter().map(|r| r.trade_id.as_str()).collect();
    assert_eq!(ids, ["6373", "6375"], "id-less rows dropped, good rows kept: {ids:?}");
}

#[test]
fn a_batch_of_only_id_less_rows_is_empty_not_an_error() {
    let body = json!({ "transactions": [fill_txn(None), fill_txn(Some(""))] });
    assert!(parse_fill_reports(&body, "EUR_USD", "EURUSD", "USD").is_empty());
}
