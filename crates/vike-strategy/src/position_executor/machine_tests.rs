use super::*;
// vike-model's recording `Broker` double (`test-support` feature): captures every market/limit
// submit and scripts `now`/`price`/`position`.
use vike_model::strategy::MockBroker;

fn bar(open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        ts: 0,
        open,
        high,
        low,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn fill(side: i32, size: f64, price: f64, ts: i64) -> Fill {
    Fill { side, size, price, fee: 0.0, ts, is_maker: false, symbol: String::new() }
}

/// A 1-unit long BTCUSDT market intent with take-profit +10 (target 110 off a 100 entry).
fn long_tp() -> PositionIntent {
    PositionIntent::market(
        "binance",
        "BTCUSDT",
        1,
        1.0,
        TripleBarrier::new(Some(10.0), None, None, None),
    )
}

// --- entry submission (market / limit) ---

#[test]
fn market_entry_submits_market_then_opens_on_full_fill() {
    let mut ex = PositionExecutor::new(long_tp());
    let mut b = MockBroker::default();
    assert_eq!(ex.state(), ExecutorState::Pending);
    ex.start(&mut b);
    assert_eq!(ex.state(), ExecutorState::EntryWorking);
    assert_eq!(b.markets, vec![("BTCUSDT".to_string(), 1, 1.0)]);
    assert!(b.limits.is_empty());
    // full entry fill @100 → Open
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
    assert_eq!(ex.state(), ExecutorState::Open);
}

#[test]
fn limit_entry_submits_limit() {
    let intent = PositionIntent { entry: EntryKind::Limit { price: 99.0 }, ..long_tp() };
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::default();
    ex.start(&mut b);
    assert_eq!(ex.state(), ExecutorState::EntryWorking);
    assert!(b.markets.is_empty());
    assert_eq!(b.limits, vec![("BTCUSDT".to_string(), 1, 1.0, 99.0)]);
}

#[test]
fn start_is_idempotent() {
    let mut ex = PositionExecutor::new(long_tp());
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.start(&mut b); // second call ignored (not Pending)
    assert_eq!(b.markets.len(), 1);
}

// --- full lifecycle, long & short ---

#[test]
fn full_lifecycle_long_take_profit() {
    let mut ex = PositionExecutor::new(long_tp()); // tp @ 110
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open @100
    assert_eq!(ex.state(), ExecutorState::Open);
    // a bar rallying through 110 fires TP → market close, Closing
    ex.on_bar(&mut b, &bar(105.0, 112.0, 104.0, 111.0));
    assert_eq!(ex.state(), ExecutorState::Closing);
    assert_eq!(
        b.markets,
        vec![
            ("BTCUSDT".to_string(), 1, 1.0),  // entry (buy)
            ("BTCUSDT".to_string(), -1, 1.0), // close (sell, opposite side)
        ]
    );
    // close fill @110 → Closed with the terminal outcome
    ex.on_fill(&fill(-1, 1.0, 110.0, 2_000));
    assert_eq!(ex.state(), ExecutorState::Closed);
    assert!(ex.is_terminal());
    let o = ex.outcome().expect("a Closed outcome");
    assert_eq!(o.barrier_hit, Some(BarrierKind::TakeProfit));
    assert_eq!(o.entry_px, 100.0);
    assert_eq!(o.exit_px, 110.0);
    assert_eq!(o.entry_ts, 1_000);
    assert_eq!(o.exit_ts, 2_000);
    assert_eq!(o.realized_pnl, 10.0); // (110-100)*(+1)*1 - 0
}

#[test]
fn full_lifecycle_short_take_profit() {
    // short: entry SELLS, tp exits BELOW entry (100 → 90), close BUYS to cover
    let intent = PositionIntent::market(
        "binance",
        "BTCUSDT",
        -1,
        1.0,
        TripleBarrier::new(Some(10.0), None, None, None),
    );
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::default();
    ex.start(&mut b);
    assert_eq!(b.markets[0], ("BTCUSDT".to_string(), -1, 1.0)); // short entry sells
    ex.on_fill(&fill(-1, 1.0, 100.0, 1_000)); // Open short @100
    assert_eq!(ex.state(), ExecutorState::Open);
    ex.on_bar(&mut b, &bar(95.0, 96.0, 88.0, 89.0)); // dips through 90 → TP
    assert_eq!(ex.state(), ExecutorState::Closing);
    assert_eq!(b.markets[1], ("BTCUSDT".to_string(), 1, 1.0)); // close buys to cover
    ex.on_fill(&fill(1, 1.0, 90.0, 2_000));
    assert_eq!(ex.state(), ExecutorState::Closed);
    let o = ex.outcome().unwrap();
    assert_eq!(o.barrier_hit, Some(BarrierKind::TakeProfit));
    assert_eq!(o.realized_pnl, 10.0); // (90-100)*(-1)*1 = +10 (short profits as price falls)
}

// --- each barrier drives a close ---

#[test]
fn stop_loss_drives_close() {
    let intent = PositionIntent::market(
        "binance",
        "BTCUSDT",
        1,
        1.0,
        TripleBarrier::new(None, Some(5.0), None, None), // sl @ 95
    );
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
    ex.on_bar(&mut b, &bar(98.0, 99.0, 94.0, 94.5)); // dips through 95
    assert_eq!(ex.state(), ExecutorState::Closing);
    ex.on_fill(&fill(-1, 1.0, 95.0, 2_000));
    assert_eq!(ex.state(), ExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::StopLoss));
}

#[test]
fn take_profit_via_tick_path_drives_close() {
    // exercises the on_tick cadence for a price barrier
    let mut ex = PositionExecutor::new(long_tp()); // tp @ 110
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
    ex.on_tick(&mut b, 110.0); // a tick AT the target
    assert_eq!(ex.state(), ExecutorState::Closing);
    ex.on_fill(&fill(-1, 1.0, 110.0, 1_100));
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::TakeProfit));
}

#[test]
fn time_barrier_drives_close() {
    let intent = PositionIntent::market(
        "binance",
        "BTCUSDT",
        1,
        1.0,
        TripleBarrier::new(None, None, Some(500), None), // deadline = entry_ts + 500
    );
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // entry_ts = 1000 → deadline 1500
    // a quiet bar BEFORE the deadline: nothing fires (injected clock = broker.now())
    b.now = 1_499;
    ex.on_bar(&mut b, &bar(100.0, 101.0, 99.0, 100.5));
    assert_eq!(ex.state(), ExecutorState::Open);
    // AT the deadline: Time fires → market close
    b.now = 1_500;
    ex.on_bar(&mut b, &bar(100.0, 101.0, 99.0, 100.5));
    assert_eq!(ex.state(), ExecutorState::Closing);
    ex.on_fill(&fill(-1, 1.0, 100.5, 1_500));
    assert_eq!(ex.state(), ExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::Time));
}

// --- the sticky-close invariant ---

#[test]
fn sticky_close_survives_favorable_tick() {
    // long, tp 110 / sl 95. The stop fires first → Closing. Then price rallies WELL past the
    // take-profit via both a tick and a bar — the executor must NOT re-open or submit anything
    // new; it stays Closing until the close fill, and the recorded barrier is the ORIGINAL stop.
    let intent = PositionIntent::market(
        "binance",
        "BTCUSDT",
        1,
        1.0,
        TripleBarrier::new(Some(10.0), Some(5.0), None, None),
    );
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
    ex.on_bar(&mut b, &bar(98.0, 99.0, 94.0, 94.5)); // SL fires
    assert_eq!(ex.state(), ExecutorState::Closing);
    assert_eq!(b.markets.len(), 2, "entry + one close only");
    // a favourable tick far above entry — would be a TP if still Open — must be IGNORED
    ex.on_tick(&mut b, 120.0);
    // and a favourable bar too
    ex.on_bar(&mut b, &bar(118.0, 125.0, 117.0, 121.0));
    assert_eq!(ex.state(), ExecutorState::Closing, "sticky: never re-opens on a favourable move");
    assert_eq!(b.markets.len(), 2, "no new orders from the favourable ticks/bars");
    // the close finally fills → Closed, still tagged the original stop
    ex.on_fill(&fill(-1, 1.0, 95.0, 2_000));
    assert_eq!(ex.state(), ExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::StopLoss));
}

// --- failure & cancel paths ---

#[test]
fn entry_reject_fails() {
    // long_tp uses the DEFAULT RetryPolicy (max_attempts = 1): the first submit is the only one,
    // so a single reject exhausts the budget → Failed.
    let mut ex = PositionExecutor::new(long_tp());
    let mut b = MockBroker::default();
    ex.start(&mut b);
    assert_eq!(ex.state(), ExecutorState::EntryWorking);
    ex.on_order_rejected(&mut b);
    assert_eq!(ex.state(), ExecutorState::Failed);
    assert!(ex.is_terminal());
    assert_eq!(ex.outcome(), None); // no position was ever held
    assert_eq!(b.markets.len(), 1, "no resubmit under the default no-retry policy");
}

#[test]
fn external_cancel_before_fill_cancels() {
    let mut ex = PositionExecutor::new(long_tp());
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.cancel(&mut b);
    assert_eq!(ex.state(), ExecutorState::Canceled);
    assert!(ex.is_terminal());
    assert_eq!(b.markets.len(), 1, "no close submitted — nothing was filled");
    assert_eq!(ex.outcome(), None);
}

#[test]
fn cancel_while_open_flattens_not_strands() {
    // a controller stop on an OPEN position must flatten (never strand inventory), not Cancel.
    let mut ex = PositionExecutor::new(long_tp());
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open
    ex.cancel(&mut b);
    assert_eq!(ex.state(), ExecutorState::Closing);
    assert_eq!(b.markets.last().unwrap(), &("BTCUSDT".to_string(), -1, 1.0));
    ex.on_fill(&fill(-1, 1.0, 101.0, 2_000));
    assert_eq!(ex.state(), ExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, None); // a manual flatten has no barrier
}

// --- partial entry accumulation + VWAP ---

#[test]
fn partial_entry_fills_accumulate_then_open_at_vwap() {
    let intent = PositionIntent::market("binance", "BTCUSDT", 1, 2.0, TripleBarrier::none());
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // half filled — still working
    assert_eq!(ex.state(), ExecutorState::EntryWorking);
    ex.on_fill(&fill(1, 1.0, 102.0, 1_100)); // now full → Open at VWAP 101
    assert_eq!(ex.state(), ExecutorState::Open);
    // read the VWAP + completing-fill timestamp through a manual flatten's outcome
    ex.cancel(&mut b);
    assert_eq!(b.markets.last().unwrap(), &("BTCUSDT".to_string(), -1, 2.0)); // closes full qty
    ex.on_fill(&fill(-1, 2.0, 101.0, 1_200));
    let o = ex.outcome().unwrap();
    assert_eq!(o.entry_px, 101.0); // (100+102)/2
    assert_eq!(o.entry_ts, 1_100); // the fill that COMPLETED the entry
    assert_eq!(o.realized_pnl, 0.0); // closed at the entry VWAP
}

// --- closing is idempotent to stray fills / ticks ---

#[test]
fn closing_ignores_further_bars_and_needs_full_close_fill() {
    let intent = PositionIntent::market(
        "binance",
        "BTCUSDT",
        1,
        2.0,
        TripleBarrier::new(None, Some(5.0), None, None), // sl @95
    );
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 2.0, 100.0, 1_000)); // Open 2 units
    ex.on_bar(&mut b, &bar(98.0, 99.0, 94.0, 94.5)); // SL → Closing, close 2 units
    assert_eq!(ex.state(), ExecutorState::Closing);
    // a PARTIAL close fill (1 of 2) keeps it Closing
    ex.on_fill(&fill(-1, 1.0, 95.0, 1_500));
    assert_eq!(ex.state(), ExecutorState::Closing);
    // more bars while Closing submit nothing new
    ex.on_bar(&mut b, &bar(94.0, 95.0, 90.0, 91.0));
    assert_eq!(b.markets.len(), 2, "no resubmit in stage 2 — one entry + one close");
    // the remaining close fills → Closed
    ex.on_fill(&fill(-1, 1.0, 95.0, 1_600));
    assert_eq!(ex.state(), ExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::StopLoss));
}

// ========================================================================================
// Refresh + retry + close-reject resubmit.
// ========================================================================================

// --- entry retry on reject (budget + backoff, now_ms-fed) ---

#[test]
fn entry_reject_retries_with_backoff_then_fails_after_exhaustion() {
    // 3 total attempts, 100ms backoff. Each reject schedules a resubmit `backoff_ms` later,
    // re-driven by on_refresh against broker.now(); Failed only after the 3rd attempt is rejected.
    let intent = PositionIntent { retry: RetryPolicy::new(3, 100), ..long_tp() };
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::at(1_000);
    ex.start(&mut b); // attempt 1 @ t=1000
    assert_eq!(b.markets.len(), 1);
    assert_eq!(ex.state(), ExecutorState::EntryWorking);

    // reject #1 → schedule retry at 1100 (budget 1 < 3); NOT resubmitted yet (backoff pending).
    ex.on_order_rejected(&mut b);
    assert_eq!(ex.state(), ExecutorState::EntryWorking);
    assert_eq!(b.markets.len(), 1, "backoff not elapsed — no resubmit yet");

    // a refresh BEFORE the backoff deadline does nothing.
    b.now = 1_099;
    ex.on_refresh(&mut b);
    assert_eq!(b.markets.len(), 1);

    // AT the deadline the retry resubmits (attempt 2).
    b.now = 1_100;
    ex.on_refresh(&mut b);
    assert_eq!(b.markets.len(), 2, "retry #1 resubmitted at the backoff deadline");
    assert_eq!(ex.state(), ExecutorState::EntryWorking);

    // reject #2 → retry at 1200 (budget 2 < 3), resubmits at the deadline (attempt 3 = last).
    ex.on_order_rejected(&mut b);
    b.now = 1_200;
    ex.on_refresh(&mut b);
    assert_eq!(b.markets.len(), 3, "retry #2 resubmitted (the final attempt)");

    // reject #3 → budget 3 is NOT < 3 → Failed, no further submit.
    ex.on_order_rejected(&mut b);
    assert_eq!(ex.state(), ExecutorState::Failed);
    assert!(ex.is_terminal());
    assert_eq!(b.markets.len(), 3, "no resubmit after the budget is exhausted");
    assert_eq!(ex.outcome(), None);
}

#[test]
fn zero_backoff_retry_resubmits_immediately_on_reject() {
    // backoff 0 → the retry fires inside the reject handler (already due), without an on_refresh.
    let intent = PositionIntent { retry: RetryPolicy::new(2, 0), ..long_tp() };
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::at(1_000);
    ex.start(&mut b); // attempt 1
    ex.on_order_rejected(&mut b); // 0-backoff → immediate resubmit (attempt 2)
    assert_eq!(b.markets.len(), 2, "0-backoff retry resubmits immediately");
    assert_eq!(ex.state(), ExecutorState::EntryWorking);
}

#[test]
fn entry_retry_resubmit_then_fill_opens_normally() {
    // a successful resubmit after a reject opens the position and runs the lifecycle normally.
    let intent = PositionIntent { retry: RetryPolicy::new(2, 0), ..long_tp() };
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::at(1_000);
    ex.start(&mut b);
    ex.on_order_rejected(&mut b); // immediate resubmit (attempt 2)
    assert_eq!(b.markets.len(), 2);
    // the resubmitted entry fills → Open (no lingering retry state).
    ex.on_fill(&fill(1, 1.0, 100.0, 1_050));
    assert_eq!(ex.state(), ExecutorState::Open);
    // and it closes on the take-profit like any normally-opened position.
    ex.on_bar(&mut b, &bar(105.0, 112.0, 104.0, 111.0));
    assert_eq!(ex.state(), ExecutorState::Closing);
    ex.on_fill(&fill(-1, 1.0, 110.0, 1_100));
    assert_eq!(ex.state(), ExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::TakeProfit));
}

#[test]
fn on_order_event_routes_reject_and_deny_to_retry_and_ignores_the_rest() {
    // the harness-facing dispatcher: Rejected/Denied → retry; Accepted/Canceled/Expired → no-op.
    let intent = PositionIntent { retry: RetryPolicy::new(2, 0), ..long_tp() };
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::default();
    ex.start(&mut b); // attempt 1
    // a RiskGate Denied is a retryable reject → immediate resubmit (attempt 2).
    ex.on_order_event(
        &mut b,
        &OrderLifecycle {
            client_order_id: "c1".into(),
            tag: None,
            kind: OrderEventKind::Denied { reason: "risk".into() },
        },
    );
    assert_eq!(b.markets.len(), 2, "Denied routed to retry");
    // Accepted / Canceled / Expired / Filled are no-ops.
    for kind in [
        OrderEventKind::Accepted,
        OrderEventKind::Canceled { reason: String::new() },
        OrderEventKind::Expired,
        OrderEventKind::Filled,
    ] {
        ex.on_order_event(&mut b, &OrderLifecycle { client_order_id: "c".into(), tag: None, kind });
    }
    assert_eq!(b.markets.len(), 2, "non-reject events do nothing");
    assert_eq!(ex.state(), ExecutorState::EntryWorking);
    // a venue Rejected now exhausts the budget (attempt 2 = max) → Failed.
    ex.on_order_event(
        &mut b,
        &OrderLifecycle {
            client_order_id: "c9".into(),
            tag: None,
            kind: OrderEventKind::Rejected { reason: "no".into() },
        },
    );
    assert_eq!(ex.state(), ExecutorState::Failed);
}

// --- entry refresh (reprice the resting limit) ---

#[test]
fn entry_refresh_reprices_resting_limit_after_interval_and_is_noop_once_open() {
    // a resting limit @99, refreshed every 5000ms (Reprice). Before the interval: nothing; at the
    // interval: reprice to broker.price(); once Open: a no-op.
    let intent = PositionIntent {
        entry: EntryKind::Limit { price: 99.0 },
        refresh: Some(RefreshPolicy::new(5_000, RefreshMode::Reprice)),
        ..long_tp()
    };
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::at(1_000);
    ex.start(&mut b); // submit_limit @99 at t=1000
    assert_eq!(b.limits, vec![("BTCUSDT".to_string(), 1, 1.0, 99.0)]);

    // before every_ms elapses → no reprice.
    b.now = 5_999;
    b.px = 101.0;
    ex.on_refresh(&mut b);
    assert_eq!(b.limits.len(), 1, "interval not elapsed");

    // at the interval → reprice to the fresh mark (101).
    b.now = 6_000;
    b.px = 101.0;
    ex.on_refresh(&mut b);
    assert_eq!(b.limits.len(), 2);
    assert_eq!(
        b.limits[1],
        ("BTCUSDT".to_string(), 1, 1.0, 101.0),
        "repriced to the current broker.price()"
    );
    assert_eq!(ex.state(), ExecutorState::EntryWorking);

    // fill → Open; a later refresh is a no-op (nothing rests once Open).
    ex.on_fill(&fill(1, 1.0, 101.0, 6_050));
    assert_eq!(ex.state(), ExecutorState::Open);
    b.now = 1_000_000;
    b.px = 200.0;
    ex.on_refresh(&mut b);
    assert_eq!(b.limits.len(), 2, "no reprice once Open");
}

#[test]
fn entry_refresh_cancel_replace_mode_also_reprices() {
    // CancelReplace degrades to the same portable action as Reprice (no cancel verb) — it still
    // reprices the resting limit to the fresh mark.
    let intent = PositionIntent {
        entry: EntryKind::Limit { price: 99.0 },
        refresh: Some(RefreshPolicy::new(5_000, RefreshMode::CancelReplace)),
        ..long_tp()
    };
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::at(0);
    ex.start(&mut b);
    b.now = 5_000;
    b.px = 97.5;
    ex.on_refresh(&mut b);
    assert_eq!(b.limits.len(), 2);
    assert_eq!(b.limits[1], ("BTCUSDT".to_string(), 1, 1.0, 97.5));
}

#[test]
fn market_entry_refresh_is_a_noop() {
    // a market intent with a refresh policy has no resting order to reprice.
    let intent = PositionIntent {
        refresh: Some(RefreshPolicy::new(1_000, RefreshMode::Reprice)),
        ..long_tp()
    };
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::default();
    ex.start(&mut b);
    b.now = 10_000;
    ex.on_refresh(&mut b);
    assert!(b.limits.is_empty());
    assert_eq!(b.markets.len(), 1, "a market entry is never repriced");
}

// --- close-reject sticky resubmit ---

#[test]
fn close_reject_resubmits_and_stays_closing() {
    let mut ex = PositionExecutor::new(long_tp()); // tp @ 110
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open @100
    ex.on_bar(&mut b, &bar(105.0, 112.0, 104.0, 111.0)); // TP → Closing, first close submitted
    assert_eq!(ex.state(), ExecutorState::Closing);
    assert_eq!(b.markets.len(), 2, "entry + first close");

    // the close is REJECTED → resubmit it and STAY Closing (sticky — never re-open).
    ex.on_order_rejected(&mut b);
    assert_eq!(ex.state(), ExecutorState::Closing);
    assert_eq!(b.markets.len(), 3, "close resubmitted");
    assert_eq!(
        b.markets[2],
        ("BTCUSDT".to_string(), -1, 1.0),
        "resubmit the full remaining flatten"
    );

    // the resubmitted close finally fills → Closed, still tagged the ORIGINAL take-profit.
    ex.on_fill(&fill(-1, 1.0, 110.0, 2_000));
    assert_eq!(ex.state(), ExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::TakeProfit));
}

#[test]
fn close_reject_resubmits_only_the_remaining_qty() {
    // a partial close then a reject of the remainder resubmits ONLY what is left to flatten.
    let intent = PositionIntent::market(
        "binance",
        "BTCUSDT",
        1,
        2.0,
        TripleBarrier::new(None, Some(5.0), None, None), // sl @ 95
    );
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 2.0, 100.0, 1_000)); // Open 2 units
    ex.on_bar(&mut b, &bar(98.0, 99.0, 94.0, 94.5)); // SL → Closing, close 2 units
    ex.on_fill(&fill(-1, 1.0, 95.0, 1_500)); // partial close 1 of 2, still Closing
    assert_eq!(ex.state(), ExecutorState::Closing);
    ex.on_order_rejected(&mut b); // remainder rejected → resubmit only the remaining 1 unit
    assert_eq!(b.markets.last().unwrap(), &("BTCUSDT".to_string(), -1, 1.0));
    ex.on_fill(&fill(-1, 1.0, 95.0, 1_600));
    assert_eq!(ex.state(), ExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::StopLoss));
}

// ========================================================================================
// Realized-PnL law — routed through the canonical `vike_model::TradeFold`.
// ========================================================================================

#[test]
fn no_multiplier_realized_pnl_matches_pre_fold_behavior() {
    // default multiplier (1.0), zero fees: (exit_px − entry_px) · signed_qty, the plain VWAP diff.
    let mut ex = PositionExecutor::new(long_tp()); // tp @ 110
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
    ex.on_bar(&mut b, &bar(105.0, 112.0, 104.0, 111.0)); // TP fires
    ex.on_fill(&fill(-1, 1.0, 110.0, 2_000));
    let o = ex.outcome().unwrap();
    assert_eq!(o.realized_pnl, 10.0);
    assert_eq!(o.fees, 0.0);
    assert_eq!(o.net(), 10.0);
}

#[test]
fn realized_pnl_is_multiplier_aware_via_trade_fold() {
    // mult=10: entry 1@100, exit 1@110 -> gross realized = (110-100)*1*10 = 100, not 10.
    let intent = PositionIntent::market("binance", "BTCUSDT", 1, 1.0, TripleBarrier::none());
    let mut ex = PositionExecutor::new(intent).with_multiplier(10.0);
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open @100
    ex.cancel(&mut b); // manual flatten (no barrier) -> Closing
    ex.on_fill(&fill(-1, 1.0, 110.0, 2_000)); // Closed
    let o = ex.outcome().unwrap();
    assert_eq!(o.realized_pnl, 100.0, "gross, multiplier-aware");
    assert_eq!(o.fees, 0.0);
    assert_eq!(o.net(), 100.0);
}

#[test]
fn realized_pnl_is_gross_fees_reported_separately() {
    // fee-bearing round trip: `realized_pnl` stays GROSS (matches `Account`'s convention);
    // `fees` carries the round-trip cost separately and `net()` is the fee-adjusted figure.
    let intent = PositionIntent::market("binance", "BTCUSDT", 1, 1.0, TripleBarrier::none());
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&Fill {
        side: 1,
        size: 1.0,
        price: 100.0,
        fee: 0.4,
        ts: 1_000,
        is_maker: false,
        symbol: String::new(),
    });
    ex.cancel(&mut b);
    ex.on_fill(&Fill {
        side: -1,
        size: 1.0,
        price: 110.0,
        fee: 0.6,
        ts: 2_000,
        is_maker: false,
        symbol: String::new(),
    });
    let o = ex.outcome().unwrap();
    assert_eq!(o.realized_pnl, 10.0, "gross price pnl, fees NOT netted in");
    assert_eq!(o.fees, 1.0);
    assert_eq!(o.net(), 9.0);
}

#[test]
fn overshoot_close_clamps_at_flat_not_a_stray_flip() {
    // a close fill reporting MORE size than the tracked entry qty must still finish exactly
    // flat, with the pnl clamped to the tracked qty — not a stray opposite-side flip this
    // executor would then abandon untracked (the executor's contract is close-to-flat).
    let intent = PositionIntent::market("binance", "BTCUSDT", 1, 1.0, TripleBarrier::none());
    let mut ex = PositionExecutor::new(intent);
    let mut b = MockBroker::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open @100, entry_qty = 1.0
    ex.cancel(&mut b); // -> Closing
    // the close fill reports 1.5 — an over-fill past the tracked 1.0 entry qty.
    ex.on_fill(&fill(-1, 1.5, 110.0, 2_000));
    assert_eq!(ex.state(), ExecutorState::Closed, "closes on the fill that covers entry_qty");
    let o = ex.outcome().unwrap();
    assert_eq!(o.realized_pnl, 10.0, "pnl clamped to the tracked 1.0, not the overshot 1.5");
}
