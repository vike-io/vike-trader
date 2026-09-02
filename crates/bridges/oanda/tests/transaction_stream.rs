//! Integration coverage for the OANDA transactions-stream decode (`decode_transaction_events`)
//! and the audit-A3 post-reconnect resync-backfill mapping (`map_transactions_since` /
//! `max_transaction_id`, re-exported `#[doc(hidden)]` from the private `history` module — see
//! `lib.rs` for the promotion rationale). Canned transaction lines below are hand verified
//! against the OANDA v20 Transaction shape (one JSON object per stream line / `sinceid` array
//! entry) — the retired Python app exported no fixture for OANDA, so these payloads are authored
//! here rather than exported, the same convention `tests/candles.rs` states.
//!
//! The A3/dual-publish contract under test (see `stream.rs` doc comments):
//! - every `ORDER_FILL` dual-publishes a bare `Event::Fill` (Account folds position/PnL) AND an
//!   `Event::OrderFilled` wrap (the order FSM applies it) carrying the SAME `FillEvent` — so a
//!   fill replayed by the `sinceid` backfill folds byte-identically to a live one.
//! - `last_qty` on that `FillEvent` is THIS transaction's delta quantity, never a running total —
//!   OANDA's `units` field on an ORDER_FILL transaction is itself a per-fill delta, not a
//!   cumulative position size, so no additional accumulation/subtraction happens in the mapper.
//! - `ORDER_CANCEL` maps to exactly one terminal `Event::OrderCanceled`.
//! - anything else (HEARTBEAT, order-create echoes, unknown transaction types) maps to nothing.

use vike_model::events::Event;
use vike_oanda::{decode_transaction_events, map_transactions_since, max_transaction_id};

fn kind(e: &Event) -> String {
    match e {
        Event::Fill(f) => format!("Fill:{}:{}:{}", f.client_order_id, f.trade_id, f.last_qty),
        Event::OrderFilled(w) => format!("OrderFilled:{}:{}", w.client_order_id, w.fill.trade_id),
        Event::OrderCanceled(c) => format!("OrderCanceled:{}:{}", c.client_order_id, c.reason),
        other => format!("other:{other:?}"),
    }
}

// --- decode_transaction_events (the live stream path) --------------------------------------

#[test]
fn order_fill_dual_publishes_bare_fill_then_wrap_with_the_same_delta_fill() {
    let v = serde_json::json!({
        "type": "ORDER_FILL", "id": "6373", "time": "1478012400.000000000",
        "orderID": "6372", "instrument": "EUR_USD", "units": "-1000", "price": "1.09300",
        "commission": "0.02", "clientExtensions": {"id": "coid-9"}
    });
    let evs = decode_transaction_events(&v);
    assert_eq!(evs.len(), 2, "a fill must emit exactly bare Fill + OrderFilled wrap");
    match &evs[0] {
        Event::Fill(fill) => {
            assert_eq!(fill.client_order_id, "coid-9");
            assert_eq!(fill.trade_id, "6373");
            assert_eq!(fill.side, -1, "negative units -> sell");
            assert_eq!(fill.last_qty, 1000.0, "last_qty is this fill's delta, not a running total");
            assert_eq!(fill.last_px, 1.093);
            assert_eq!(fill.commission, 0.02);
            assert_eq!(fill.ts, 1_478_012_400_000);
            assert_eq!(fill.venue, "oanda");
            assert_eq!(fill.symbol, "EUR_USD");
        }
        other => panic!("expected bare Fill first, got {other:?}"),
    }
    match &evs[1] {
        Event::OrderFilled(of) => {
            assert_eq!(of.client_order_id, "coid-9");
            assert_eq!(
                of.fill.trade_id, "6373",
                "the wrap carries the SAME FillEvent as the bare Fill"
            );
            assert_eq!(of.fill.last_qty, 1000.0);
            assert_eq!(of.ts, 1_478_012_400_000);
        }
        other => panic!("expected OrderFilled wrap second, got {other:?}"),
    }
}

#[test]
fn order_fill_client_order_id_prefers_client_extensions_over_client_order_id_over_order_id() {
    // clientExtensions.id (vike sets it = our client_order_id) wins when present...
    let with_ext = serde_json::json!({
        "type": "ORDER_FILL", "id": "1", "time": "1", "orderID": "50", "units": "1",
        "clientOrderID": "should-not-win", "clientExtensions": {"id": "coid-a"}
    });
    let coid = |evs: &[Event]| match &evs[0] {
        Event::Fill(f) => f.client_order_id.clone(),
        other => panic!("expected Fill, got {other:?}"),
    };
    assert_eq!(coid(&decode_transaction_events(&with_ext)), "coid-a");

    // ...else clientOrderID (OANDA's own echo field)...
    let with_client_order_id = serde_json::json!({
        "type": "ORDER_FILL", "id": "2", "time": "1", "orderID": "51", "units": "1",
        "clientOrderID": "coid-b"
    });
    assert_eq!(coid(&decode_transaction_events(&with_client_order_id)), "coid-b");

    // ...else falls back to the venue orderID...
    let with_order_id_only = serde_json::json!({
        "type": "ORDER_FILL", "id": "3", "time": "1", "orderID": "52", "units": "1"
    });
    assert_eq!(coid(&decode_transaction_events(&with_order_id_only)), "52");

    // ...and if NONE are present, it's the empty string (never panics).
    let with_nothing =
        serde_json::json!({"type": "ORDER_FILL", "id": "4", "time": "1", "units": "1"});
    assert_eq!(coid(&decode_transaction_events(&with_nothing)), "");
}

#[test]
fn order_fill_missing_numeric_fields_default_to_zero_and_side_defaults_positive() {
    let v = serde_json::json!({"type": "ORDER_FILL", "id": "9", "orderID": "1"});
    let evs = decode_transaction_events(&v);
    match &evs[0] {
        Event::Fill(f) => {
            assert_eq!(f.last_qty, 0.0);
            assert_eq!(f.last_px, 0.0);
            assert_eq!(f.commission, 0.0);
            assert_eq!(f.ts, 0);
            assert_eq!(f.side, 1, "units defaults to 0.0, and 0.0 >= 0.0 -> side=+1");
            assert_eq!(f.symbol, ""); // instrument absent -> empty, not a panic
        }
        other => panic!("expected Fill, got {other:?}"),
    }
}

#[test]
fn order_cancel_maps_to_single_terminal_event() {
    let v = serde_json::json!({
        "type": "ORDER_CANCEL", "id": "700", "time": "1", "orderID": "6372", "reason": "CLIENT_REQUEST"
    });
    let evs = decode_transaction_events(&v);
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        Event::OrderCanceled(c) => {
            assert_eq!(c.reason, "CLIENT_REQUEST");
            assert_eq!(c.ts, 1000); // "1" second -> 1000 ms
        }
        other => panic!("expected OrderCanceled, got {other:?}"),
    }
}

#[test]
fn order_cancel_missing_reason_defaults_to_empty_string() {
    let v = serde_json::json!({"type": "ORDER_CANCEL", "id": "1", "time": "1", "orderID": "2"});
    match &decode_transaction_events(&v)[0] {
        Event::OrderCanceled(c) => assert_eq!(c.reason, ""),
        other => panic!("expected OrderCanceled, got {other:?}"),
    }
}

#[test]
fn heartbeat_and_unknown_and_missing_type_all_emit_nothing() {
    assert!(decode_transaction_events(&serde_json::json!({"type": "HEARTBEAT", "time": "1"}))
        .is_empty());
    // an order-create echo (or any other real OANDA transaction type) also maps to nothing here —
    // ORDER_CREATE is observed only via the order-POST response path (exec.rs), not the stream.
    assert!(decode_transaction_events(&serde_json::json!({"type": "ORDER_CREATE", "id": "1"}))
        .is_empty());
    assert!(decode_transaction_events(
        &serde_json::json!({"type": "MARKET_ORDER_REJECT", "id": "1"})
    )
    .is_empty());
    // "type" absent entirely -> None also falls into the catch-all arm, not a panic.
    assert!(decode_transaction_events(&serde_json::json!({"id": "1", "time": "1"})).is_empty());
}

// --- history::map_transactions_since / max_transaction_id (the A3 resync-backfill path) ----

#[test]
fn resync_backfill_replays_each_transaction_through_the_same_decode_path() {
    let resp = serde_json::json!({
        "transactions": [
            {"type": "ORDER_FILL", "id": "101", "time": "2.0", "orderID": "50",
             "instrument": "EUR_USD", "units": "1000", "price": "1.1", "commission": "0.02",
             "clientExtensions": {"id": "c_fill"}},
            {"type": "HEARTBEAT", "id": "102", "time": "3.0"},
            {"type": "ORDER_CANCEL", "id": "103", "time": "4.0", "orderID": "60",
             "reason": "CLIENT_REQUEST", "clientExtensions": {"id": "c_cancel"}}
        ],
        "lastTransactionID": "103"
    });
    let evs = map_transactions_since(&resp);
    let kinds: Vec<String> = evs.iter().map(kind).collect();
    assert_eq!(
        kinds,
        vec![
            "Fill:c_fill:101:1000".to_string(),
            "OrderFilled:c_fill:101".to_string(),
            "OrderCanceled:c_cancel:CLIENT_REQUEST".to_string(),
        ],
        "HEARTBEAT is skipped; the fill dual-publishes; the cancel is a single terminal event"
    );
    assert_eq!(max_transaction_id(&resp), Some(103));
}

#[test]
fn max_transaction_id_prefers_the_explicit_field_over_the_derived_max() {
    // A deliberately inconsistent response: the explicit lastTransactionID (200) is LOWER than
    // the max transaction id actually present (999). The explicit field still wins — it is
    // authoritative per the OANDA API, not a derived fallback.
    let resp = serde_json::json!({
        "transactions": [{"type": "HEARTBEAT", "id": "999", "time": "1"}],
        "lastTransactionID": "200"
    });
    assert_eq!(max_transaction_id(&resp), Some(200));
}

#[test]
fn max_transaction_id_falls_back_to_the_derived_max_when_the_field_is_absent() {
    let resp = serde_json::json!({
        "transactions": [
            {"type": "HEARTBEAT", "id": "5", "time": "1"},
            {"type": "HEARTBEAT", "id": "42", "time": "2"},
            {"type": "HEARTBEAT", "id": "not-a-number", "time": "3"},
        ]
    });
    assert_eq!(max_transaction_id(&resp), Some(42), "the highest parseable id wins");
}

#[test]
fn empty_backfill_yields_no_events_and_none_watermark_when_nothing_present() {
    let resp = serde_json::json!({"transactions": [], "lastTransactionID": "7"});
    assert!(map_transactions_since(&resp).is_empty());
    assert_eq!(max_transaction_id(&resp), Some(7));

    assert!(map_transactions_since(&serde_json::json!({})).is_empty());
    assert_eq!(max_transaction_id(&serde_json::json!({})), None);
}
