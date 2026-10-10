use super::*;
use serde_json::json;
use std::assert_matches;

const V: &str = "hyperliquid";

fn order(coid: &str, coin: &str, side: i32, req_sz: f64) -> SubmittedOrder {
    SubmittedOrder {
        client_order_id: coid.to_string(),
        coin: coin.to_string(),
        side,
        req_sz,
        ts: 1000,
    }
}

// --- 1. /exchange response ---

#[test]
fn response_resting_filled_error_positional() {
    let resp = json!({
        "status": "ok",
        "response": {"type": "order", "data": {"statuses": [
            {"resting": {"oid": 77738308}},
            {"filled": {"totalSz": "0.02", "avgPx": "1891.4", "oid": 77738309}},
            {"error": "Order must have minimum value of $10."}
        ]}}
    });
    let orders = [
        order("c-rest", "ETH", 1, 0.5),
        order("c-fill", "ETH", 1, 0.02), // fully filled
        order("c-err", "PURR/USDC", -1, 3.0),
    ];
    let evs = map_order_response(&resp, V, &orders);
    assert_eq!(evs.len(), 3);

    match &evs[0] {
        Event::OrderAccepted(a) => {
            assert_eq!(a.client_order_id, "c-rest");
            assert_eq!(a.venue_order_id.as_deref(), Some("77738308"));
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
    match &evs[1] {
        Event::OrderFilled(w) => {
            assert_eq!(w.client_order_id, "c-fill");
            assert_eq!(w.fill.trade_id, "77738309"); // oid keys the FSM wrap dedup
            assert_eq!(w.fill.venue, V);
            assert_eq!(w.fill.symbol, "ETH"); // coin carried through verbatim
            assert_eq!(w.fill.side, 1);
            assert_eq!(w.fill.last_qty, 0.02);
            assert_eq!(w.fill.last_px, 1891.4);
        }
        other => panic!("expected OrderFilled, got {other:?}"),
    }
    match &evs[2] {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "c-err");
            assert!(r.reason.contains("minimum value"));
        }
        other => panic!("expected OrderRejected, got {other:?}"),
    }
}

#[test]
fn response_partial_fill_when_totalsz_below_requested() {
    let resp = json!({"status": "ok", "response": {"data": {"statuses": [
        {"filled": {"totalSz": "0.02", "avgPx": "1891.4", "oid": 5}}
    ]}}});
    let orders = [order("c1", "ETH", 1, 0.05)]; // requested 0.05, only 0.02 filled
    let evs = map_order_response(&resp, V, &orders);
    match &evs[0] {
        Event::OrderPartiallyFilled(w) => {
            assert_eq!(w.fill.last_qty, 0.02);
            assert_eq!(w.client_order_id, "c1");
        }
        other => panic!("expected OrderPartiallyFilled, got {other:?}"),
    }
}

#[test]
fn response_top_level_err_rejects_every_order() {
    let resp = json!({"status": "err", "response": "Insufficient margin to place order."});
    let orders = [order("a", "BTC", 1, 1.0), order("b", "ETH", -1, 2.0)];
    let evs = map_order_response(&resp, V, &orders);
    assert_eq!(evs.len(), 2);
    for (ev, coid) in evs.iter().zip(["a", "b"]) {
        match ev {
            Event::OrderRejected(r) => {
                assert_eq!(r.client_order_id, coid);
                assert!(r.reason.contains("Insufficient margin"));
            }
            other => panic!("expected OrderRejected, got {other:?}"),
        }
    }
}

#[test]
fn response_waiting_for_trigger_is_accepted() {
    let resp = json!({"status": "ok", "response": {"data": {"statuses": ["waitingForTrigger"]}}});
    let orders = [order("c1", "BTC", 1, 1.0)];
    let evs = map_order_response(&resp, V, &orders);
    match &evs[0] {
        Event::OrderAccepted(a) => {
            assert_eq!(a.client_order_id, "c1");
            assert_eq!(a.venue_order_id, None); // no oid until it rests/triggers
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
}

// --- 2. orderUpdates ---

fn update(status: &str, oid: u64, cloid: &str) -> Value {
    json!({
        "order": {
            "coin": "BTC", "side": "B", "limitPx": "50000.0", "sz": "0.0",
            "origSz": "0.1", "oid": oid, "cloid": cloid, "timestamp": 1234
        },
        "status": status,
        "statusTimestamp": 5678
    })
}

#[test]
fn order_update_open_is_accepted_with_oid() {
    let ev = map_order_update(&update("open", 42, "0xabc"), V).expect("event");
    match ev {
        Event::OrderAccepted(a) => {
            assert_eq!(a.client_order_id, "0xabc"); // cloid carried (caller remaps to coid)
            assert_eq!(a.venue_order_id.as_deref(), Some("42"));
            assert_eq!(a.ts, 5678); // statusTimestamp preferred
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
}

#[test]
fn order_update_filled_is_orderfilled_from_origsz() {
    let ev = map_order_update(&update("filled", 7, "0xdef"), V).expect("event");
    match ev {
        Event::OrderFilled(w) => {
            assert_eq!(w.client_order_id, "0xdef");
            assert_eq!(w.fill.trade_id, "7"); // oid keys the FSM-wrap dedup
            assert_eq!(w.fill.last_qty, 0.1); // origSz, not the (drained) sz=0.0
            assert_eq!(w.fill.last_px, 50000.0); // limitPx (lifecycle approximation)
            assert_eq!(w.fill.side, 1); // "B" = buy
            assert_eq!(w.fill.symbol, "BTC");
        }
        other => panic!("expected OrderFilled, got {other:?}"),
    }
}

#[test]
fn order_update_prefixed_canceled_by_suffix() {
    // a *Canceled variant not spelled plain "canceled"
    let ev = map_order_update(&update("marginCanceled", 9, "0x1"), V).expect("event");
    match ev {
        Event::OrderCanceled(c) => {
            assert_eq!(c.client_order_id, "0x1");
            assert_eq!(c.reason, "marginCanceled");
        }
        other => panic!("expected OrderCanceled, got {other:?}"),
    }
    // the dead-man's-switch cancel that lacks the -ed suffix
    assert_matches!(
        map_order_update(&update("scheduledCancel", 9, "0x1"), V),
        Some(Event::OrderCanceled(_))
    );
}

#[test]
fn order_update_prefixed_rejected_by_suffix() {
    let ev = map_order_update(&update("tickRejected", 3, "0x2"), V).expect("event");
    match ev {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "0x2");
            assert_eq!(r.reason, "tickRejected");
        }
        other => panic!("expected OrderRejected, got {other:?}"),
    }
}

#[test]
fn order_update_triggered() {
    assert_matches!(
        map_order_update(&update("triggered", 1, "0x3"), V),
        Some(Event::OrderTriggered(_))
    );
}

#[test]
fn order_update_unknown_status_is_soft_none() {
    // an unmapped status (the Hummingbot #7689 crash class) must be a soft no-op, not a panic
    assert!(map_order_update(&update("someBrandNewStatus", 1, "0x4"), V).is_none());
}

#[test]
fn order_update_falls_back_to_oid_when_no_cloid() {
    let row = json!({
        "order": {"coin": "BTC", "side": "A", "sz": "1.0", "oid": 555, "timestamp": 1},
        "status": "open"
    });
    match map_order_update(&row, V).expect("event") {
        Event::OrderAccepted(a) => assert_eq!(a.client_order_id, "555"),
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
}

#[test]
fn order_updates_frame_maps_recognized_rows_only() {
    let frame = json!({"channel": "orderUpdates", "data": [
        update("open", 1, "0xa"),
        update("weirdUnknown", 2, "0xb"), // dropped (soft no-op)
        update("filled", 3, "0xc")
    ]});
    let evs = map_order_updates(&frame, V);
    assert_eq!(evs.len(), 2, "the unknown-status row drops out");
    assert_matches!(evs[0], Event::OrderAccepted(_));
    assert_matches!(evs[1], Event::OrderFilled(_));
}

// --- 3. userFills ---

fn fills_frame(is_snapshot: bool) -> Value {
    json!({"channel": "userFills", "data": {
        "isSnapshot": is_snapshot,
        "user": "0xmaster",
        "fills": [
            { // taker BUY
                "coin": "BTC", "px": "50000.0", "sz": "0.1", "side": "B",
                "oid": 100, "cloid": "0xc1", "tid": 900, "fee": "2.5",
                "feeToken": "USDC", "crossed": true, "dir": "Open Long",
                "startPosition": "0.0", "closedPnl": "0.0", "time": 111
            },
            { // maker SELL with a rebate (negative fee)
                "coin": "ETH", "px": "3000.0", "sz": "1.0", "side": "A",
                "oid": 101, "tid": 901, "fee": "-0.3",
                "feeToken": "USDC", "crossed": false, "dir": "Close Long",
                "startPosition": "1.0", "closedPnl": "5.0", "time": 222
            }
        ]
    }})
}

#[test]
fn user_fills_stream_taker_and_maker() {
    let out = map_user_fills(&fills_frame(false), V);
    assert!(!out.is_snapshot);
    assert_eq!(out.fills.len(), 2);

    let taker = &out.fills[0];
    assert_eq!(taker.trade_id, "900"); // tid keys the Account-lane dedup
    assert_eq!(taker.client_order_id, "0xc1"); // cloid carried
    assert_eq!(taker.venue, V);
    assert_eq!(taker.symbol, "BTC");
    assert_eq!(taker.side, 1); // "B" = buy
    assert_eq!(taker.last_qty, 0.1);
    assert_eq!(taker.last_px, 50000.0);
    assert_eq!(taker.commission, 2.5); // positive = cost
    assert_eq!(taker.commission_asset, "USDC");
    assert_eq!(taker.liquidity_side, LiquiditySide::Taker); // crossed = true
    assert_eq!(taker.ts, 111);
    assert_eq!(taker.position_side, PositionSide::Both);

    let maker = &out.fills[1];
    assert_eq!(maker.trade_id, "901");
    assert_eq!(maker.client_order_id, "101"); // no cloid -> oid fallback
    assert_eq!(maker.symbol, "ETH");
    assert_eq!(maker.side, -1); // "A" = sell
    assert_eq!(maker.commission, -0.3); // negative = maker rebate (SIGNED, kept as-is)
    assert_eq!(maker.liquidity_side, LiquiditySide::Maker); // crossed = false
}

#[test]
fn user_fills_exposes_snapshot_flag() {
    let out = map_user_fills(&fills_frame(true), V);
    assert!(out.is_snapshot, "the initial snapshot flag must be surfaced, not dropped");
    assert_eq!(out.fills.len(), 2);
}

#[test]
fn user_fills_accepts_bare_body_without_envelope() {
    // the pump may hand us the inner {isSnapshot, fills} body directly
    let body = json!({
        "isSnapshot": false,
        "fills": [{"coin": "SOL", "px": "150.0", "sz": "2.0", "side": "B",
                   "oid": 1, "tid": 2, "fee": "0.1", "feeToken": "USDC",
                   "crossed": true, "time": 5}]
    });
    let out = map_user_fills(&body, V);
    assert_eq!(out.fills.len(), 1);
    assert_eq!(out.fills[0].symbol, "SOL");
}

// --- 3b. the id-less fill is REFUSED on both lanes ---
//
// These gate the `TradeId::new` handling, not merely the type. Reverting any of the three sites
// to a permissive id (`unwrap_or_default()`, a `""` fallback, or an id synthesized from
// qty/px/ts) reddens them: each asserts the fill/wrap is ABSENT, so a permissive site turns the
// count from 0 back to 1. The hazard being gated is double-booking — `seen_trade_ids` cannot
// hold an id that does not exist, so an admitted id-less fill re-folds on every replay.

/// `userFills` (ACCOUNT lane): a row with `tid` absent, null or `""` yields no fill at all, on
/// an incremental frame AND on a snapshot. A tagged sibling in the same frame is unaffected.
#[test]
fn user_fill_without_a_tid_is_not_emitted() {
    let row = |tid: Value| {
        json!({"coin": "BTC", "px": "50000.0", "sz": "0.1", "side": "B", "oid": 100,
                   "cloid": "0xc1", "tid": tid, "fee": "1.0", "feeToken": "USDC",
                   "crossed": true, "time": 111})
    };
    let mut absent = row(Value::Null);
    absent.as_object_mut().expect("object").remove("tid");

    for (label, bad) in
        [("absent", absent), ("null", row(Value::Null)), ("empty string", row(json!("")))]
    {
        for is_snapshot in [false, true] {
            let frame = json!({"isSnapshot": is_snapshot, "fills": [bad.clone()]});
            let out = map_user_fills(&frame, V);
            assert!(
                out.fills.is_empty(),
                "a `tid`-{label} fill must NOT be emitted (is_snapshot={is_snapshot}): an \
                     un-dedupable fill double-books position and PnL on replay"
            );
        }
    }

    // ...and the drop is per-row, not per-frame.
    let frame = json!({"isSnapshot": false, "fills": [row(json!("")), row(json!(900))]});
    let out = map_user_fills(&frame, V);
    assert_eq!(out.fills.len(), 1, "only the untagged row drops");
    assert_eq!(out.fills[0].trade_id, "900");
}

/// `orderUpdates` (FSM lane): a `filled` row with no `oid` emits NOTHING — no `OrderFilled`
/// wrap, because `oid` is that wrap's dedup key and an un-dedupable wrap re-adds `filled_qty`
/// on every replay. Sibling rows in the same frame still map.
#[test]
fn order_update_filled_without_an_oid_is_not_emitted() {
    let bad = json!({
        "order": {"coin": "BTC", "side": "B", "sz": "1.0", "origSz": "1.0",
                  "limitPx": "50000.0", "cloid": "0xc9", "timestamp": 7},
        "status": "filled", "statusTimestamp": 8
    });
    assert!(
        map_order_update(&bad, V).is_none(),
        "a `filled` row with no `oid` must yield no FSM wrap"
    );

    let frame = json!({"channel": "orderUpdates", "data": [bad, update("filled", 3, "0xc")]});
    let evs = map_order_updates(&frame, V);
    assert_eq!(evs.len(), 1, "only the oid-less row drops out");
    assert_matches!(evs[0], Event::OrderFilled(_));
}

/// `/exchange` (FSM lane): a `filled` status with no `oid` yields no event for THAT slot, and
/// the positional zip keeps every other slot's event — the emitter-split rule (no order
/// silently vanishes) is still served by the other slots plus the exec-side watchdog.
#[test]
fn response_filled_without_an_oid_is_not_emitted() {
    let resp = json!({"status": "ok", "response": {"type": "order", "data": {"statuses": [
        {"filled": {"totalSz": "0.02", "avgPx": "1891.4"}},          // no oid
        {"filled": {"totalSz": "0.02", "avgPx": "1891.4", "oid": 5}} // fine
    ]}}});
    let orders = [order("c-noid", "ETH", 1, 0.02), order("c-ok", "ETH", 1, 0.02)];
    let evs = map_order_response(&resp, V, &orders);
    assert_eq!(evs.len(), 1, "the oid-less slot emits nothing");
    match &evs[0] {
        Event::OrderFilled(w) => {
            assert_eq!(w.client_order_id, "c-ok");
            assert_eq!(w.fill.trade_id, "5");
        }
        other => panic!("expected the tagged OrderFilled, got {other:?}"),
    }
}

// --- 4. cloid determinism ---

#[test]
fn cloid_is_deterministic_and_well_formed() {
    let a = cloid_from_client_order_id("vike-000123");
    let b = cloid_from_client_order_id("vike-000123");
    assert_eq!(a, b, "same coid -> same cloid (idempotent, no mapping table)");
    assert_ne!(a, cloid_from_client_order_id("vike-000124"), "distinct coids -> distinct cloids");
    // "0x" + 16 bytes as 32 lowercase hex chars = 34 chars total
    assert_eq!(a.len(), 34);
    assert!(a.starts_with("0x"));
    assert!(
        a[2..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "lowercase hex only, got {a}"
    );
}
