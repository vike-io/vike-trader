//! **What the journal `Snapshot`'s engine half weighs as history grows**, by part: an engine that placed `n`
//! orders, each filled at once (one fill each, alternating sides, so every other fill closes a trade), three
//! minutes of engine clock apart, beside three working orders. Prints one line per `n` (`--nocapture`), and
//! holds the journal form (`Engine::journal_snapshot`) to what is live: the working orders always,
//! no ended order older than `ENDED_ORDER_KEEP_MS`, the trade ids of `WIDEST_WINDOW_DAYS` days (an engine nobody
//! sized to its node's read-back), the FSM's as its difference from the account's, so the bytes stop growing once
//! the history is longer than the window.
//!
//! Main before the bound (`snapshot_state` was the `Snapshot`): 1k orders 552 KB, 10k 5.5 MB, 100k 55.5 MB, the
//! registry 95% of it at ~530 bytes an order.

use vike_execution::account::fill_window::WIDEST_WINDOW_DAYS;
use vike_execution::testing::{TestExecutionClient, engine_on, pump};
use vike_execution::{Engine, EngineSnapshot, EventBusOutbox};
use vike_model::MS_PER_DAY;
use vike_model::{OrderRecord, OrderRequest, OrderType};

/// The engine clock of the first order, and the step between two orders.
const T0: i64 = 1_700_000_000_000;
const STEP: i64 = 3 * 60_000;

fn limit(order_client_id: String, side: i32, px: f64, ts: i64) -> OrderRequest {
    OrderRequest {
        order_client_id,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(px),
        timestamp: ts,
        ..Default::default()
    }
}

/// `n` orders placed and filled, then three that rest (never sent: `Initialized` is working).
fn traded(n: usize) -> Engine<TestExecutionClient> {
    let mut e = engine_on("sim", "BTCUSDT", TestExecutionClient::new("sim", 100.0));
    let mut outbox = EventBusOutbox::default();
    for i in 0..n {
        let now = T0 + i as i64 * STEP;
        e.now_ms = now;
        let side = if i % 2 == 0 { 1 } else { -1 };
        let req = limit(format!("vike-c{i:010}"), side, 100.0 + (i % 5) as f64, now);
        e.submit_order(&req, now, &mut outbox);
        pump(&mut e, &mut outbox);
    }
    for k in 0..3 {
        let req = limit(format!("rest-{k}"), 1, 90.0, e.now_ms);
        let mut mo = OrderRecord::new(req);
        mo.created_ms = Some(e.now_ms);
        e.registry.insert(format!("rest-{k}"), mo);
    }
    e
}

/// The `Snapshot`'s engine half as the core journals it.
fn journaled(e: &Engine<TestExecutionClient>) -> EngineSnapshot {
    e.journal_snapshot()
}

fn bytes<T: serde::Serialize>(v: &T) -> usize {
    serde_json::to_vec(v).expect("serializes").len()
}

#[test]
fn the_snap_engine_half_by_part() {
    // At most this many ids of one set: the window's days of three-minute fills, and the newest day's.
    let window_ids = ((WIDEST_WINDOW_DAYS as i64 + 1) * MS_PER_DAY / STEP) as usize;
    let mut totals = Vec::new();
    for n in [1_000, 10_000, 100_000, 200_000] {
        let e = traded(n);
        let s = journaled(&e);
        let total = bytes(&s);
        let registry = bytes(&s.registry);
        let fills = bytes(&s.seen_trade_ids)
            + bytes(&s.fsm_only_trade_ids)
            + bytes(&s.account_only_trade_ids)
            + bytes(&s.seen_liquidation_ids)
            + bytes(&s.seen_trade_days)
            + bytes(&s.fsm_only_trade_days);
        let closed = bytes(&s.account.closed_pnls);
        let positions = bytes(&s.account.positions);
        println!(
            "SNAP-SIZE n={n} total={total} registry={registry} ({} orders, {} ended left out) \
             fill_ids={fills} ({} + {} + {} ids) closed_pnls={closed} ({} rows) positions={positions} \
             rest={}",
            s.registry.len(),
            s.ended_orders_left_out,
            s.seen_trade_ids.len(),
            s.fsm_only_trade_ids.len() + s.account_only_trade_ids.len(),
            s.seen_liquidation_ids.len(),
            s.account.closed_pnls.len(),
            total - registry - fills - closed - positions,
        );
        let working = s.registry.iter().filter(|(_, o)| !o.status.is_terminal()).count();
        assert_eq!(working, 3, "the working orders always ride the Snapshot");
        assert!(s.registry.len() <= 4, "and only the newest ended order: {}", s.registry.len());
        assert_eq!(s.registry.len() as u64 + s.ended_orders_left_out, n as u64 + 3);
        assert!(s.seen_trade_ids.len() <= window_ids.min(n), "{}", s.seen_trade_ids.len());
        assert!(
            s.fsm_only_trade_ids.is_empty() && s.account_only_trade_ids.is_empty(),
            "every fill came bare and wrapped, so the FSM set is the account's and writes nothing of its own"
        );
        assert!(s.account.closed_pnls.len() <= vike_execution::JOURNAL_CLOSED_PNLS);
        totals.push((total, s.seen_trade_ids.len()));
    }
    // 100k and 200k orders are both longer than the window: the Snapshot stopped growing. The ids it keeps are as
    // many (within a day's), and only their spelling grew (`simt99999` to `simt199999`).
    let ((a, ids_a), (b, ids_b)) = (totals[2], totals[3]);
    assert!(
        ids_b <= ids_a + (MS_PER_DAY / STEP) as usize,
        "the window grew: {ids_a} -> {ids_b} ids"
    );
    assert!(b <= a + a / 10, "the Snapshot grew with history: {a} -> {b} bytes");
}
