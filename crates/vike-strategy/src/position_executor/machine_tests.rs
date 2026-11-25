use super::*;
// vike-model's recording `OrderBroker` double (`test-support` feature): captures every market/limit
// submit and scripts `now`/`price`/`position`.
use vike_model::strategy::OrderBrokerMock;

use vike_marketdata::test_support::shape_bar;

/// The UNTAGGED market orders `OrderBrokerMock` heard through the seam, as `(symbol, side, qty)`: the rows
/// its `markets` held when the same orders went by `Broker::submit_market` (the controller and
/// funding-carry tests read them too).
pub(crate) fn untagged_markets(b: &OrderBrokerMock) -> Vec<(String, i32, f64)> {
    let rows = b.placed.iter().filter(|(_, tag, _, _, order, reduce_only)| {
        tag.is_empty() && *order == OrderPlacement::Market && !reduce_only
    });
    rows.map(|(symbol, _, side, qty, ..)| (symbol.clone(), *side, *qty)).collect()
}

/// The UNTAGGED limits `OrderBrokerMock` heard through the seam, as `(symbol, side, qty, price)`: the rows
/// its `limits` held when the same orders went by `Broker::submit_limit`.
pub(crate) fn untagged_limits(b: &OrderBrokerMock) -> Vec<(String, i32, f64, f64)> {
    let rows = b.placed.iter().filter(|(_, tag, ..)| tag.is_empty());
    rows.filter_map(|(symbol, _, side, qty, order, reduce_only)| match order {
        OrderPlacement::Limit { price } if !reduce_only => {
            Some((symbol.clone(), *side, *qty, *price))
        }
        _ => None,
    })
    .collect()
}

fn fill(side: i32, size: f64, price: f64, ts: i64) -> Fill {
    Fill {
        side,
        size,
        price,
        fee: 0.0,
        timestamp: ts,
        is_maker: false,
        symbol: String::new(),
        tag: None,
    }
}

/// A 1-unit long BTCUSDT market intent with take-profit +10 (target 110 off a 100 entry).
fn long_take_profit() -> PositionExecutorIntent {
    PositionExecutorIntent::market(
        "binance",
        "BTCUSDT",
        1,
        1.0,
        TripleBarrier::new(Some(10.0), None, None, None),
    )
}

/// A `size`-unit long BTCUSDT market executor under `barrier`, filled at 100 and then knocked into
/// `Closing` by a bar dipping through a 95 stop.
fn stopped_out(size: f64, barrier: TripleBarrier) -> (PositionExecutor, OrderBrokerMock) {
    let intent = PositionExecutorIntent::market("binance", "BTCUSDT", 1, size, barrier);
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, size, 100.0, 1_000));
    ex.on_bar(&mut b, &shape_bar(98.0, 99.0, 94.0, 94.5));
    (ex, b)
}

// --- entry submission (market / limit) ---

#[test]
fn market_entry_submits_market_then_opens_on_full_fill() {
    let mut ex = PositionExecutor::new(long_take_profit());
    let mut b = OrderBrokerMock::default();
    assert_eq!(ex.state(), PositionExecutorState::Pending);
    ex.start(&mut b);
    assert_eq!(ex.state(), PositionExecutorState::EntryWorking);
    assert_eq!(untagged_markets(&b), vec![("BTCUSDT".to_string(), 1, 1.0)]);
    assert!(untagged_limits(&b).is_empty());
    // full entry fill @100 → Open
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
    assert_eq!(ex.state(), PositionExecutorState::Open);
}

#[test]
fn limit_entry_submits_limit() {
    let intent = PositionExecutorIntent {
        entry: PositionExecutorEntryKind::Limit { price: 99.0 },
        ..long_take_profit()
    };
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    assert_eq!(ex.state(), PositionExecutorState::EntryWorking);
    assert!(untagged_markets(&b).is_empty());
    assert_eq!(untagged_limits(&b), vec![("BTCUSDT".to_string(), 1, 1.0, 99.0)]);
}

#[test]
fn start_is_idempotent() {
    let mut ex = PositionExecutor::new(long_take_profit());
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.start(&mut b); // second call ignored (not Pending)
    assert_eq!(untagged_markets(&b).len(), 1);
}

// --- full lifecycle, long & short ---

#[test]
fn full_lifecycle_long_take_profit() {
    let mut ex = PositionExecutor::new(long_take_profit()); // tp @ 110
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open @100
    assert_eq!(ex.state(), PositionExecutorState::Open);
    // a bar rallying through 110 fires TP → market close, Closing
    ex.on_bar(&mut b, &shape_bar(105.0, 112.0, 104.0, 111.0));
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    assert_eq!(
        untagged_markets(&b),
        vec![
            ("BTCUSDT".to_string(), 1, 1.0),  // entry (buy)
            ("BTCUSDT".to_string(), -1, 1.0), // close (sell, opposite side)
        ]
    );
    // close fill @110 → Closed with the terminal outcome
    ex.on_fill(&fill(-1, 1.0, 110.0, 2_000));
    assert_eq!(ex.state(), PositionExecutorState::Closed);
    assert!(ex.is_terminal());
    let o = ex.outcome().expect("a Closed outcome");
    assert_eq!(o.barrier_hit, Some(TripleBarrierKind::TakeProfit));
    assert_eq!(o.entry_price, 100.0);
    assert_eq!(o.exit_price, 110.0);
    assert_eq!(o.entry_timestamp, 1_000);
    assert_eq!(o.exit_timestamp, 2_000);
    assert_eq!(o.realized_pnl, 10.0); // (110-100)*(+1)*1 - 0
}

#[test]
fn full_lifecycle_short_take_profit() {
    // short: entry SELLS, tp exits BELOW entry (100 → 90), close BUYS to cover
    let intent = PositionExecutorIntent::market(
        "binance",
        "BTCUSDT",
        -1,
        1.0,
        TripleBarrier::new(Some(10.0), None, None, None),
    );
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    assert_eq!(untagged_markets(&b)[0], ("BTCUSDT".to_string(), -1, 1.0)); // short entry sells
    ex.on_fill(&fill(-1, 1.0, 100.0, 1_000)); // Open short @100
    assert_eq!(ex.state(), PositionExecutorState::Open);
    ex.on_bar(&mut b, &shape_bar(95.0, 96.0, 88.0, 89.0)); // dips through 90 → TP
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    assert_eq!(untagged_markets(&b)[1], ("BTCUSDT".to_string(), 1, 1.0)); // close buys to cover
    ex.on_fill(&fill(1, 1.0, 90.0, 2_000));
    assert_eq!(ex.state(), PositionExecutorState::Closed);
    let o = ex.outcome().unwrap();
    assert_eq!(o.barrier_hit, Some(TripleBarrierKind::TakeProfit));
    assert_eq!(o.realized_pnl, 10.0); // (90-100)*(-1)*1 = +10 (short profits as price falls)
}

// --- each barrier drives a close ---

#[test]
fn stop_loss_drives_close() {
    // sl @ 95; the bar dips through it.
    let (mut ex, _b) = stopped_out(1.0, TripleBarrier::new(None, Some(5.0), None, None));
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    ex.on_fill(&fill(-1, 1.0, 95.0, 2_000));
    assert_eq!(ex.state(), PositionExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(TripleBarrierKind::StopLoss));
}

#[test]
fn take_profit_via_tick_path_drives_close() {
    // exercises the on_tick cadence for a price barrier
    let mut ex = PositionExecutor::new(long_take_profit()); // tp @ 110
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
    ex.on_tick(&mut b, 110.0); // a tick AT the target
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    ex.on_fill(&fill(-1, 1.0, 110.0, 1_100));
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(TripleBarrierKind::TakeProfit));
}

#[test]
fn time_barrier_drives_close() {
    let intent = PositionExecutorIntent::market(
        "binance",
        "BTCUSDT",
        1,
        1.0,
        TripleBarrier::new(None, None, Some(500), None), // deadline = entry_timestamp + 500
    );
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // entry_timestamp = 1000 → deadline 1500
    // a quiet bar BEFORE the deadline: nothing fires (injected clock = broker.now())
    b.now = 1_499;
    ex.on_bar(&mut b, &shape_bar(100.0, 101.0, 99.0, 100.5));
    assert_eq!(ex.state(), PositionExecutorState::Open);
    // AT the deadline: Time fires → market close
    b.now = 1_500;
    ex.on_bar(&mut b, &shape_bar(100.0, 101.0, 99.0, 100.5));
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    ex.on_fill(&fill(-1, 1.0, 100.5, 1_500));
    assert_eq!(ex.state(), PositionExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(TripleBarrierKind::Time));
}

// --- the sticky-close invariant ---

#[test]
fn sticky_close_survives_favorable_tick() {
    // long, tp 110 / sl 95. The stop fires first → Closing. Then price rallies WELL past the
    // take-profit via both a tick and a bar — the executor must NOT re-open or submit anything
    // new; it stays Closing until the close fill, and the recorded barrier is the ORIGINAL stop.
    // SL fires.
    let (mut ex, mut b) = stopped_out(1.0, TripleBarrier::new(Some(10.0), Some(5.0), None, None));
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    assert_eq!(untagged_markets(&b).len(), 2, "entry + one close only");
    // a favourable tick far above entry — would be a TP if still Open — must be IGNORED
    ex.on_tick(&mut b, 120.0);
    // and a favourable bar too
    ex.on_bar(&mut b, &shape_bar(118.0, 125.0, 117.0, 121.0));
    assert_eq!(
        ex.state(),
        PositionExecutorState::Closing,
        "sticky: never re-opens on a favourable move"
    );
    assert_eq!(untagged_markets(&b).len(), 2, "no new orders from the favourable ticks/bars");
    // the close finally fills → Closed, still tagged the original stop
    ex.on_fill(&fill(-1, 1.0, 95.0, 2_000));
    assert_eq!(ex.state(), PositionExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(TripleBarrierKind::StopLoss));
}

// --- failure & cancel paths ---

#[test]
fn entry_reject_fails() {
    // long_take_profit uses the DEFAULT PositionExecutorRetryPolicy (max_attempts = 1): the first submit is the only one,
    // so a single reject exhausts the budget → Failed.
    let mut ex = PositionExecutor::new(long_take_profit());
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    assert_eq!(ex.state(), PositionExecutorState::EntryWorking);
    ex.on_order_rejected(&mut b);
    assert_eq!(ex.state(), PositionExecutorState::Failed);
    assert!(ex.is_terminal());
    assert_eq!(ex.outcome(), None); // no position was ever held
    assert_eq!(untagged_markets(&b).len(), 1, "no resubmit under the default no-retry policy");
}

#[test]
fn external_cancel_before_fill_cancels() {
    let mut ex = PositionExecutor::new(long_take_profit());
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.cancel(&mut b);
    assert_eq!(ex.state(), PositionExecutorState::Canceled);
    assert!(ex.is_terminal());
    assert_eq!(untagged_markets(&b).len(), 1, "no close submitted — nothing was filled");
    assert_eq!(ex.outcome(), None);
}

#[test]
fn cancel_while_open_flattens_not_strands() {
    // a controller stop on an OPEN position must flatten (never strand inventory), not Cancel.
    let mut ex = PositionExecutor::new(long_take_profit());
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open
    ex.cancel(&mut b);
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    assert_eq!(untagged_markets(&b).last().unwrap(), &("BTCUSDT".to_string(), -1, 1.0));
    ex.on_fill(&fill(-1, 1.0, 101.0, 2_000));
    assert_eq!(ex.state(), PositionExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, None); // a manual flatten has no barrier
}

// --- partial entry accumulation + VWAP ---

#[test]
fn partial_entry_fills_accumulate_then_open_at_vwap() {
    let intent =
        PositionExecutorIntent::market("binance", "BTCUSDT", 1, 2.0, TripleBarrier::none());
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // half filled — still working
    assert_eq!(ex.state(), PositionExecutorState::EntryWorking);
    ex.on_fill(&fill(1, 1.0, 102.0, 1_100)); // now full → Open at VWAP 101
    assert_eq!(ex.state(), PositionExecutorState::Open);
    // read the VWAP + completing-fill timestamp through a manual flatten's outcome
    ex.cancel(&mut b);
    assert_eq!(untagged_markets(&b).last().unwrap(), &("BTCUSDT".to_string(), -1, 2.0)); // closes full qty
    ex.on_fill(&fill(-1, 2.0, 101.0, 1_200));
    let o = ex.outcome().unwrap();
    assert_eq!(o.entry_price, 101.0); // (100+102)/2
    assert_eq!(o.entry_timestamp, 1_100); // the fill that COMPLETED the entry
    assert_eq!(o.realized_pnl, 0.0); // closed at the entry VWAP
}

// --- closing is idempotent to stray fills / ticks ---

#[test]
fn closing_ignores_further_bars_and_needs_full_close_fill() {
    // Open 2 units, sl @95; SL → Closing, close 2 units.
    let (mut ex, mut b) = stopped_out(2.0, TripleBarrier::new(None, Some(5.0), None, None));
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    // a PARTIAL close fill (1 of 2) keeps it Closing
    ex.on_fill(&fill(-1, 1.0, 95.0, 1_500));
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    // more bars while Closing submit nothing new
    ex.on_bar(&mut b, &shape_bar(94.0, 95.0, 90.0, 91.0));
    assert_eq!(untagged_markets(&b).len(), 2, "no resubmit in stage 2 — one entry + one close");
    // the remaining close fills → Closed
    ex.on_fill(&fill(-1, 1.0, 95.0, 1_600));
    assert_eq!(ex.state(), PositionExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(TripleBarrierKind::StopLoss));
}

// ========================================================================================
// Refresh + retry + close-reject resubmit.
// ========================================================================================

// --- entry retry on reject (budget + backoff, now_ms-fed) ---

#[test]
fn entry_reject_retries_with_backoff_then_fails_after_exhaustion() {
    // 3 total attempts, 100ms backoff. Each reject schedules a resubmit `backoff_ms` later,
    // re-driven by on_refresh against broker.now(); Failed only after the 3rd attempt is rejected.
    let intent = PositionExecutorIntent {
        retry: PositionExecutorRetryPolicy::new(3, 100),
        ..long_take_profit()
    };
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::at(1_000);
    ex.start(&mut b); // attempt 1 @ t=1000
    assert_eq!(untagged_markets(&b).len(), 1);
    assert_eq!(ex.state(), PositionExecutorState::EntryWorking);

    // reject #1 → schedule retry at 1100 (budget 1 < 3); NOT resubmitted yet (backoff pending).
    ex.on_order_rejected(&mut b);
    assert_eq!(ex.state(), PositionExecutorState::EntryWorking);
    assert_eq!(untagged_markets(&b).len(), 1, "backoff not elapsed — no resubmit yet");

    // a refresh BEFORE the backoff deadline does nothing.
    b.now = 1_099;
    ex.on_refresh(&mut b);
    assert_eq!(untagged_markets(&b).len(), 1);

    // AT the deadline the retry resubmits (attempt 2).
    b.now = 1_100;
    ex.on_refresh(&mut b);
    assert_eq!(untagged_markets(&b).len(), 2, "retry #1 resubmitted at the backoff deadline");
    assert_eq!(ex.state(), PositionExecutorState::EntryWorking);

    // reject #2 → retry at 1200 (budget 2 < 3), resubmits at the deadline (attempt 3 = last).
    ex.on_order_rejected(&mut b);
    b.now = 1_200;
    ex.on_refresh(&mut b);
    assert_eq!(untagged_markets(&b).len(), 3, "retry #2 resubmitted (the final attempt)");

    // reject #3 → budget 3 is NOT < 3 → Failed, no further submit.
    ex.on_order_rejected(&mut b);
    assert_eq!(ex.state(), PositionExecutorState::Failed);
    assert!(ex.is_terminal());
    assert_eq!(untagged_markets(&b).len(), 3, "no resubmit after the budget is exhausted");
    assert_eq!(ex.outcome(), None);
}

#[test]
fn zero_backoff_retry_resubmits_immediately_on_reject() {
    // backoff 0 → the retry fires inside the reject handler (already due), without an on_refresh.
    let intent = PositionExecutorIntent {
        retry: PositionExecutorRetryPolicy::new(2, 0),
        ..long_take_profit()
    };
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::at(1_000);
    ex.start(&mut b); // attempt 1
    ex.on_order_rejected(&mut b); // 0-backoff → immediate resubmit (attempt 2)
    assert_eq!(untagged_markets(&b).len(), 2, "0-backoff retry resubmits immediately");
    assert_eq!(ex.state(), PositionExecutorState::EntryWorking);
}

#[test]
fn entry_retry_resubmit_then_fill_opens_normally() {
    // a successful resubmit after a reject opens the position and runs the lifecycle normally.
    let intent = PositionExecutorIntent {
        retry: PositionExecutorRetryPolicy::new(2, 0),
        ..long_take_profit()
    };
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::at(1_000);
    ex.start(&mut b);
    ex.on_order_rejected(&mut b); // immediate resubmit (attempt 2)
    assert_eq!(untagged_markets(&b).len(), 2);
    // the resubmitted entry fills → Open (no lingering retry state).
    ex.on_fill(&fill(1, 1.0, 100.0, 1_050));
    assert_eq!(ex.state(), PositionExecutorState::Open);
    // and it closes on the take-profit like any normally-opened position.
    ex.on_bar(&mut b, &shape_bar(105.0, 112.0, 104.0, 111.0));
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    ex.on_fill(&fill(-1, 1.0, 110.0, 1_100));
    assert_eq!(ex.state(), PositionExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(TripleBarrierKind::TakeProfit));
}

#[test]
fn on_order_event_routes_reject_and_deny_to_retry_and_ignores_the_rest() {
    // the harness-facing dispatcher: Rejected/Denied → retry; Accepted/Canceled/Expired → no-op.
    let intent = PositionExecutorIntent {
        retry: PositionExecutorRetryPolicy::new(2, 0),
        ..long_take_profit()
    };
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b); // attempt 1
    // a RiskGate Denied is a retryable reject → immediate resubmit (attempt 2).
    ex.on_order_event(
        &mut b,
        &OrderEvent {
            order_client_id: "c1".into(),
            tag: None,
            kind: OrderEventKind::Denied { reason: "risk".into() },
        },
    );
    assert_eq!(untagged_markets(&b).len(), 2, "Denied routed to retry");
    // Accepted / Canceled / Expired / Filled are no-ops.
    for kind in [
        OrderEventKind::Accepted,
        OrderEventKind::Canceled { reason: String::new() },
        OrderEventKind::Expired,
        OrderEventKind::Filled,
    ] {
        ex.on_order_event(&mut b, &OrderEvent { order_client_id: "c".into(), tag: None, kind });
    }
    assert_eq!(untagged_markets(&b).len(), 2, "non-reject events do nothing");
    assert_eq!(ex.state(), PositionExecutorState::EntryWorking);
    // a venue Rejected now exhausts the budget (attempt 2 = max) → Failed.
    ex.on_order_event(
        &mut b,
        &OrderEvent {
            order_client_id: "c9".into(),
            tag: None,
            kind: OrderEventKind::Rejected { reason: "no".into() },
        },
    );
    assert_eq!(ex.state(), PositionExecutorState::Failed);
}

// --- entry refresh (a refreshable limit entry rests under its tag) ---

/// The tag a refreshable BTCUSDT limit entry rests under.
const ENTRY_TAG: &str = "BTCUSDT:entry";

/// A 1-unit long BTCUSDT limit entry @99, refreshed every 5000ms by `mode`.
fn refreshed(mode: PositionExecutorRefreshMode) -> PositionExecutorIntent {
    PositionExecutorIntent {
        entry: PositionExecutorEntryKind::Limit { price: 99.0 },
        refresh: Some(PositionExecutorRefreshPolicy::new(5_000, mode)),
        ..long_take_profit()
    }
}

/// A `placed` row of the tagged entry: `qty` @ `price`.
fn entry_row(qty: f64, price: f64) -> (String, String, i32, f64, OrderPlacement, bool) {
    ("BTCUSDT".into(), ENTRY_TAG.into(), 1, qty, OrderPlacement::Limit { price }, false)
}

#[test]
fn entry_refresh_reprice_amends_the_resting_entry_in_place_and_is_noop_once_open() {
    let mut ex = PositionExecutor::new(refreshed(PositionExecutorRefreshMode::Reprice));
    let mut b = OrderBrokerMock::at(1_000);
    ex.start(&mut b);
    assert_eq!(b.placed, vec![entry_row(1.0, 99.0)], "the entry rests under its tag");

    // before every_ms elapses → nothing.
    b.now = 5_999;
    b.price = 101.0;
    ex.on_refresh(&mut b);
    assert!(b.modified.is_empty(), "interval not elapsed");

    // at the interval → the ONE resting entry is amended to the fresh mark; nothing new is placed.
    b.now = 6_000;
    ex.on_refresh(&mut b);
    assert_eq!(b.modified, vec![(ENTRY_TAG.to_string(), None, Some(101.0))]);
    assert_eq!(b.placed, vec![entry_row(1.0, 99.0)], "no second entry beside the first");
    assert!(b.cancelled.is_empty());
    assert_eq!(ex.state(), PositionExecutorState::EntryWorking);

    // fill → Open; a later refresh is a no-op (nothing rests once Open).
    ex.on_fill(&fill(1, 1.0, 101.0, 6_050));
    assert_eq!(ex.state(), PositionExecutorState::Open);
    b.now = 1_000_000;
    b.price = 200.0;
    ex.on_refresh(&mut b);
    assert_eq!((b.modified.len(), b.placed.len()), (1, 1), "no refresh once Open");
}

#[test]
fn entry_refresh_cancel_replace_pulls_the_entry_and_places_it_afresh_on_the_next_hook() {
    let mut ex = PositionExecutor::new(refreshed(PositionExecutorRefreshMode::CancelReplace));
    let mut b = OrderBrokerMock::at(0);
    ex.start(&mut b);
    b.now = 5_000;
    b.price = 97.5;
    ex.on_refresh(&mut b);
    assert_eq!(b.cancelled, vec![ENTRY_TAG.to_string()], "the stale entry is pulled");
    assert_eq!(b.placed.len(), 1, "never re-placed in the hook its cancel went out in");
    b.now = 5_001;
    b.price = 97.0;
    ex.on_refresh(&mut b);
    assert_eq!(b.placed, vec![entry_row(1.0, 99.0), entry_row(1.0, 97.0)], "afresh at the mark");
    assert!(b.modified.is_empty());
    b.now = 5_002;
    ex.on_refresh(&mut b);
    assert_eq!((b.placed.len(), b.cancelled.len()), (2, 1), "the timer restarts at the new entry");
}

#[test]
fn a_cancel_replace_places_only_what_is_left_after_a_late_partial_fill() {
    let mut ex = PositionExecutor::new(refreshed(PositionExecutorRefreshMode::CancelReplace));
    let mut b = OrderBrokerMock::at(0);
    ex.start(&mut b);
    b.now = 5_000;
    ex.on_refresh(&mut b); // the cancel goes out ...
    ex.on_fill(&fill(1, 0.4, 99.0, 5_000)); // ... and 0.4 of the entry filled before it landed
    b.now = 5_001;
    b.price = 98.0;
    ex.on_refresh(&mut b);
    assert_eq!(b.placed[1..], [entry_row(0.6, 98.0)], "the replacement is the rest, not the whole");
    ex.on_fill(&fill(1, 0.6, 98.0, 5_100));
    assert_eq!(ex.state(), PositionExecutorState::Open);
}

#[test]
fn cancelling_the_executor_pulls_its_resting_tagged_entry() {
    let mut ex = PositionExecutor::new(refreshed(PositionExecutorRefreshMode::Reprice));
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.cancel(&mut b);
    assert_eq!(ex.state(), PositionExecutorState::Canceled);
    assert_eq!(b.cancelled, vec![ENTRY_TAG.to_string()]);
}

#[test]
fn an_entry_with_no_refresh_policy_stays_untagged() {
    // the one entry that can never be amended keeps the untagged lane (and is abandoned, not
    // cancelled, by an executor cancel, as before).
    let intent = PositionExecutorIntent {
        entry: PositionExecutorEntryKind::Limit { price: 99.0 },
        ..long_take_profit()
    };
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    assert_eq!(untagged_limits(&b), vec![("BTCUSDT".to_string(), 1, 1.0, 99.0)]);
    ex.cancel(&mut b);
    assert!(b.cancelled.is_empty());
}

#[test]
fn market_entry_refresh_is_a_noop() {
    // a market intent with a refresh policy has no resting order to reprice.
    let intent = PositionExecutorIntent {
        refresh: Some(PositionExecutorRefreshPolicy::new(
            1_000,
            PositionExecutorRefreshMode::Reprice,
        )),
        ..long_take_profit()
    };
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    b.now = 10_000;
    ex.on_refresh(&mut b);
    assert!(untagged_limits(&b).is_empty());
    assert_eq!(untagged_markets(&b).len(), 1, "a market entry is never repriced");
}

// --- close-reject sticky resubmit ---

#[test]
fn close_reject_resubmits_and_stays_closing() {
    let mut ex = PositionExecutor::new(long_take_profit()); // tp @ 110
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open @100
    ex.on_bar(&mut b, &shape_bar(105.0, 112.0, 104.0, 111.0)); // TP → Closing, first close submitted
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    assert_eq!(untagged_markets(&b).len(), 2, "entry + first close");

    // the close is REJECTED → resubmit it and STAY Closing (sticky — never re-open).
    ex.on_order_rejected(&mut b);
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    assert_eq!(untagged_markets(&b).len(), 3, "close resubmitted");
    assert_eq!(
        untagged_markets(&b)[2],
        ("BTCUSDT".to_string(), -1, 1.0),
        "resubmit the full remaining flatten"
    );

    // the resubmitted close finally fills → Closed, still tagged the ORIGINAL take-profit.
    ex.on_fill(&fill(-1, 1.0, 110.0, 2_000));
    assert_eq!(ex.state(), PositionExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(TripleBarrierKind::TakeProfit));
}

#[test]
fn close_reject_resubmits_only_the_remaining_quantity() {
    // a partial close then a reject of the remainder resubmits ONLY what is left to flatten.
    // Open 2 units, sl @ 95; SL → Closing, close 2 units.
    let (mut ex, mut b) = stopped_out(2.0, TripleBarrier::new(None, Some(5.0), None, None));
    ex.on_fill(&fill(-1, 1.0, 95.0, 1_500)); // partial close 1 of 2, still Closing
    assert_eq!(ex.state(), PositionExecutorState::Closing);
    ex.on_order_rejected(&mut b); // remainder rejected → resubmit only the remaining 1 unit
    assert_eq!(untagged_markets(&b).last().unwrap(), &("BTCUSDT".to_string(), -1, 1.0));
    ex.on_fill(&fill(-1, 1.0, 95.0, 1_600));
    assert_eq!(ex.state(), PositionExecutorState::Closed);
    assert_eq!(ex.outcome().unwrap().barrier_hit, Some(TripleBarrierKind::StopLoss));
}

// ========================================================================================
// Realized-PnL law — routed through the canonical `vike_model::TradeFold`.
// ========================================================================================

#[test]
fn no_multiplier_realized_pnl_matches_pre_fold_behavior() {
    // default multiplier (1.0), zero fees: (exit_price − entry_price) · signed_quantity, the plain VWAP diff.
    let mut ex = PositionExecutor::new(long_take_profit()); // tp @ 110
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
    ex.on_bar(&mut b, &shape_bar(105.0, 112.0, 104.0, 111.0)); // TP fires
    ex.on_fill(&fill(-1, 1.0, 110.0, 2_000));
    let o = ex.outcome().unwrap();
    assert_eq!(o.realized_pnl, 10.0);
    assert_eq!(o.fees, 0.0);
    assert_eq!(o.net(), 10.0);
}

#[test]
fn realized_pnl_is_multiplier_aware_via_trade_fold() {
    // mult=10: entry 1@100, exit 1@110 -> gross realized = (110-100)*1*10 = 100, not 10.
    let intent =
        PositionExecutorIntent::market("binance", "BTCUSDT", 1, 1.0, TripleBarrier::none());
    let mut ex = PositionExecutor::new(intent).with_contract_value(ContractValue::linear(10.0));
    let mut b = OrderBrokerMock::default();
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
fn an_inverse_contract_realizes_its_pnl_in_the_settle_coin() {
    // 1 contract worth $100, long 1@100, closed 1@110: $10 of PnL, which the coin ledger books at
    // the exit price, 10/110 coin — `TradeFold::apply_value`'s law, the live account's.
    let intent = PositionExecutorIntent::market("bybit", "BTCUSD", 1, 1.0, TripleBarrier::none());
    let mut ex = PositionExecutor::new(intent).with_contract_value(ContractValue::inverse(100.0));
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
    ex.cancel(&mut b);
    ex.on_fill(&fill(-1, 1.0, 110.0, 2_000));
    let o = ex.outcome().unwrap();
    assert_eq!(o.realized_pnl, 10.0 / 110.0, "coins, not the $10 or the linear 10·100");
}

/// An inverse entry filled 1,000 at 50,000 and 1,000 at 60,000 opened at 54,545.45, the harmonic
/// price its PnL fold holds (it read 55,000, so a 5,000 take-profit sat at 60,000, not 59,545.45);
/// the close's VWAP is harmonic too, and a round trip at the entry price realizes nothing.
#[test]
fn an_inverse_entry_and_exit_average_harmonically() {
    let intent = PositionExecutorIntent::market(
        "deribit",
        "BTC-PERPETUAL",
        1,
        2_000.0,
        TripleBarrier::none(),
    );
    let mut ex = PositionExecutor::new(intent).with_contract_value(ContractValue::inverse(1.0));
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1_000.0, 50_000.0, 1_000));
    ex.on_fill(&fill(1, 1_000.0, 60_000.0, 1_100));
    assert_eq!(ex.state(), PositionExecutorState::Open);
    ex.cancel(&mut b);
    ex.on_fill(&fill(-1, 1_000.0, 50_000.0, 1_200));
    ex.on_fill(&fill(-1, 1_000.0, 60_000.0, 1_300));
    let o = ex.outcome().unwrap();
    let harmonic = 2_000.0 / (1_000.0 / 50_000.0 + 1_000.0 / 60_000.0);
    assert!((o.entry_price - harmonic).abs() < 1e-9, "the entry {}", o.entry_price);
    assert!((o.exit_price - harmonic).abs() < 1e-9, "the exit {}", o.exit_price);
    assert!(o.realized_pnl.abs() < 1e-15, "out at the coins it paid: {}", o.realized_pnl);
}

#[test]
fn realized_pnl_is_gross_fees_reported_separately() {
    // fee-bearing round trip: `realized_pnl` stays GROSS (matches `Account`'s convention);
    // `fees` carries the round-trip cost separately and `net()` is the fee-adjusted figure.
    let intent =
        PositionExecutorIntent::market("binance", "BTCUSDT", 1, 1.0, TripleBarrier::none());
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.on_fill(&Fill {
        side: 1,
        size: 1.0,
        price: 100.0,
        fee: 0.4,
        timestamp: 1_000,
        is_maker: false,
        symbol: String::new(),
        tag: None,
    });
    ex.cancel(&mut b);
    ex.on_fill(&Fill {
        side: -1,
        size: 1.0,
        price: 110.0,
        fee: 0.6,
        timestamp: 2_000,
        is_maker: false,
        symbol: String::new(),
        tag: None,
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
    let intent =
        PositionExecutorIntent::market("binance", "BTCUSDT", 1, 1.0, TripleBarrier::none());
    let mut ex = PositionExecutor::new(intent);
    let mut b = OrderBrokerMock::default();
    ex.start(&mut b);
    ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open @100, entry_quantity = 1.0
    ex.cancel(&mut b); // -> Closing
    // the close fill reports 1.5 — an over-fill past the tracked 1.0 entry qty.
    ex.on_fill(&fill(-1, 1.5, 110.0, 2_000));
    assert_eq!(
        ex.state(),
        PositionExecutorState::Closed,
        "closes on the fill that covers entry_qty"
    );
    let o = ex.outcome().unwrap();
    assert_eq!(o.realized_pnl, 10.0, "pnl clamped to the tracked 1.0, not the overshot 1.5");
}
