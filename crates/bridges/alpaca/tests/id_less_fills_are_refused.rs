//! An Alpaca fill with no id is refused on BOTH lanes — the live trade-update stream and the
//! reconcile activities read.
//!
//! `event_mapper` used to reach `trade_id` through `unwrap_or_default()` on `execution_id`, and
//! `recon_client::parse_fills` through `.unwrap_or("")` on the activity `id`. Both yield `""`, and an
//! empty `trade_id` skipped dedup entirely rather than deduping badly.
//!
//! The two lanes fail in DIFFERENT directions, which is why both matter:
//! - live lane: an un-dedupable fill re-books commission and realized PnL on every WS reconnect
//!   replay (`crates/bridges/alpaca/src/stream.rs`'s own doc leans on the `seen_trade_ids` guard).
//! - reconcile lane: an id-less `FillReport` can NEVER match `seen_trade_ids`, so it FABRICATES a
//!   `MissingFill` divergence — and `MissingFill` is one of the two kinds the `hybrid` policy
//!   AUTO-APPLIES, booking the fill a second time with no operator in front of it.

use serde_json::json;

// --- the reconcile lane: skip the row, keep the batch --------------------------------------------

fn fill_activity(id: Option<&str>) -> serde_json::Value {
    let mut v = json!({
        "activity_type": "FILL",
        "symbol": "AAPL",
        "side": "buy",
        "qty": "10",
        "price": "150.25",
        "order_id": "o-1",
        "transaction_time": "2026-08-17T12:00:00Z",
    });
    if let Some(id) = id {
        v["id"] = json!(id);
    }
    v
}

#[test]
fn an_id_less_activity_row_is_skipped_and_the_good_rows_survive() {
    // Skipping is the safe direction (no divergence ⇒ nothing auto-applied). Failing the whole
    // request would throw away the good rows with the bad one, so the degradation is PER ROW.
    let body = serde_json::to_string(&json!([
        fill_activity(Some("a-1")),
        fill_activity(None),
        fill_activity(Some("")),
        fill_activity(Some("a-2")),
    ]))
    .unwrap();
    let reports = vike_alpaca::recon_client::parse_fills(&body, "AAPL", "AAPL").unwrap();
    let ids: Vec<&str> = reports.iter().map(|r| r.trade_id.as_str()).collect();
    assert_eq!(ids, ["a-1", "a-2"], "id-less rows skipped, good rows kept: {ids:?}");
}

#[test]
fn an_all_bad_batch_is_empty_rather_than_an_error() {
    let body =
        serde_json::to_string(&json!([fill_activity(None), fill_activity(Some(""))])).unwrap();
    let reports = vike_alpaca::recon_client::parse_fills(&body, "AAPL", "AAPL").unwrap();
    assert!(reports.is_empty(), "a batch of only id-less rows yields no reports, not an Err");
}

#[test]
fn a_well_formed_batch_is_unaffected() {
    // The control — without it the assertions above could pass by parsing nothing at all.
    let body = serde_json::to_string(&json!([fill_activity(Some("a-1"))])).unwrap();
    let reports = vike_alpaca::recon_client::parse_fills(&body, "AAPL", "AAPL").unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].trade_id, "a-1");
    assert_eq!(reports[0].last_qty, 10.0);
    assert_eq!(reports[0].last_px, 150.25);
}

// --- the live trade-update lane: drop the frame --------------------------------------------------

fn trade_event(execution_id: Option<&str>) -> serde_json::Value {
    let mut v = json!({
        "event": "fill",
        "timestamp": "2026-08-17T12:00:00Z",
        "price": "150.25",
        "qty": "10",
        "order": {
            "id": "o-1",
            "client_order_id": "coid-1",
            "symbol": "AAPL",
            "side": "buy",
            "filled_qty": "10",
            "filled_avg_price": "150.25",
        },
    });
    if let Some(id) = execution_id {
        v["execution_id"] = json!(id);
    }
    v
}

#[test]
fn a_trade_update_with_an_execution_id_still_publishes_its_fill() {
    // Control first: `execution_id` is Alpaca's per-fill identity on this stream.
    let evs = vike_alpaca::decode_trade_event(&trade_event(Some("x-1")));
    let f: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            vike_model::events::Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(f.len(), 1, "a fill event must publish its bare Fill: {evs:?}");
    assert_eq!(f[0].trade_id, "x-1");
}

#[test]
fn a_trade_update_without_an_execution_id_publishes_no_fill() {
    // Refused rather than synthesized: `order.id` is shared by every fill of a partially-filled
    // order, so there is no other per-EXECUTION field here to build a replay-stable id from. A
    // coid/order-id-derived id would collapse distinct partial fills into one.
    for id in [None, Some("")] {
        let evs = vike_alpaca::decode_trade_event(&trade_event(id));
        assert!(
            !evs.iter().any(|e| matches!(e, vike_model::events::Event::Fill(_))),
            "an id-less trade update must publish no bare Fill: {evs:?}"
        );
        assert!(
            !evs.iter().any(|e| matches!(e, vike_model::events::Event::OrderFilled(_))),
            "...and no OrderFilled wrap either: {evs:?}"
        );
    }
}
