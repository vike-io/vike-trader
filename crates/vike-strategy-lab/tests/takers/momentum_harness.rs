//! The momentum file's controller, driven through the library's `ControllerHarness`: the
//! controller-facing tests of `crates/vike-strategy/src/controller_tests.rs` (which exercise the
//! harness with `MomentumController` as its exemplar), copied onto the shipped
//! `strategies/trend/momentum.rs`. The harness's own internals (its executor list, its cooldown
//! field) stay with the harness's tests; the one assert on `cooldown_ms` was dropped here for that
//! reason.

use vike_marketdata::Bar;
use vike_model::strategy::OrderBrokerMock;
use vike_model::{
    Fill, PositionControllerParams, Strategy, StrategyParams, TripleBarrier, TripleBarrierKind,
};
use vike_strategy::ControllerHarness;

use super::momentum::MomentumController;
use super::test_support::untagged_markets;

fn bar(timestamp: i64, symbol: &str, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        timestamp,
        open,
        high,
        low,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some(symbol.into()),
    }
}

fn fill(symbol: &str, side: i32, size: f64, price: f64, timestamp: i64) -> Fill {
    Fill {
        side,
        size,
        price,
        fee: 0.0,
        timestamp,
        is_maker: false,
        symbol: symbol.to_string(),
        tag: None,
    }
}

/// qty 1, threshold 0 (always opens from the 2nd invitation), TP +10.
fn harness() -> ControllerHarness<MomentumController> {
    let control =
        MomentumController::new(1.0, 0.0, TripleBarrier::new(Some(10.0), None, None, None));
    ControllerHarness::new(control, "v", 0)
}

#[test]
fn first_invitation_declines_no_reference() {
    let mut h = harness();
    let mut b = OrderBrokerMock { now: 1, price: 100.0, ..Default::default() };
    // the momentum controller has no reference yet → declines; no executor, no order
    Strategy::on_bar(&mut h, &mut b, &bar(1, "S", 100.0, 100.0, 100.0, 100.0));
    assert_eq!(h.active_count(), 0);
    assert!(untagged_markets(&b).is_empty());
}

#[test]
fn opens_one_executor_on_signal_and_not_twice() {
    let mut h = harness();
    let mut b = OrderBrokerMock { now: 1, price: 100.0, ..Default::default() };
    Strategy::on_bar(&mut h, &mut b, &bar(1, "S", 100.0, 100.0, 100.0, 100.0)); // sets ref 100
    b.now = 2;
    b.price = 105.0; // +5 move ≥ threshold 0 → open long
    Strategy::on_bar(&mut h, &mut b, &bar(2, "S", 105.0, 105.0, 105.0, 105.0));
    assert_eq!(h.active_count(), 1, "one executor opened");
    assert_eq!(untagged_markets(&b), vec![("S".to_string(), 1, 1.0)], "a single long market entry");
    // a further bar while the executor is live must NOT open a second (one per (venue, symbol))
    b.now = 3;
    b.price = 110.0;
    Strategy::on_bar(&mut h, &mut b, &bar(3, "S", 110.0, 110.0, 110.0, 110.0));
    assert_eq!(h.active_count(), 1, "still exactly one executor");
    assert_eq!(untagged_markets(&b).len(), 1, "no second entry while one is live");
}

#[test]
fn full_lifecycle_through_the_harness_records_outcome() {
    let mut h = harness();
    let mut b = OrderBrokerMock { now: 1, price: 100.0, ..Default::default() };
    Strategy::on_bar(&mut h, &mut b, &bar(1, "S", 100.0, 100.0, 100.0, 100.0)); // ref
    b.now = 2;
    b.price = 101.0;
    Strategy::on_bar(&mut h, &mut b, &bar(2, "S", 101.0, 101.0, 101.0, 101.0)); // open long
    assert_eq!(h.active_count(), 1);
    // entry fills @100 → Open (routed by fill.symbol)
    Strategy::on_fill(&mut h, &mut b, &fill("S", 1, 1.0, 100.0, 3));
    // a bar rallying through the TP target (100+10=110) fires TP → close submitted
    b.now = 4;
    Strategy::on_bar(&mut h, &mut b, &bar(4, "S", 105.0, 112.0, 104.0, 111.0));
    assert_eq!(untagged_markets(&b), vec![("S".to_string(), 1, 1.0), ("S".to_string(), -1, 1.0)]);
    // the close fills @110 → Closed → reaped: outcome recorded, executor dropped, cooldown set
    Strategy::on_fill(&mut h, &mut b, &fill("S", -1, 1.0, 110.0, 5));
    assert_eq!(h.active_count(), 0, "closed executor reaped");
    assert_eq!(h.outcomes().len(), 1);
    let o = h.outcomes()[0];
    assert_eq!(o.barrier_hit, Some(TripleBarrierKind::TakeProfit));
    assert_eq!(o.entry_price, 100.0);
    assert_eq!(o.exit_price, 110.0);
    assert_eq!(o.entry_timestamp, 3);
    assert_eq!(o.exit_timestamp, 5);
    assert_eq!(o.realized_pnl, 10.0); // (110-100)*(+1)*1
}

#[test]
fn cooldown_gates_the_next_open() {
    // cooldown 1000ms: after a close at ts=5 the pair may not re-open until ts>=1005
    let control =
        MomentumController::new(1.0, 0.0, TripleBarrier::new(Some(10.0), None, None, None));
    let mut h = ControllerHarness::new(control, "v", 1000);
    let mut b = OrderBrokerMock { now: 1, price: 100.0, ..Default::default() };
    Strategy::on_bar(&mut h, &mut b, &bar(1, "S", 100.0, 100.0, 100.0, 100.0));
    b.now = 2;
    b.price = 101.0;
    Strategy::on_bar(&mut h, &mut b, &bar(2, "S", 101.0, 101.0, 101.0, 101.0));
    Strategy::on_fill(&mut h, &mut b, &fill("S", 1, 1.0, 100.0, 3));
    b.now = 4;
    Strategy::on_bar(&mut h, &mut b, &bar(4, "S", 105.0, 112.0, 104.0, 111.0)); // TP
    Strategy::on_fill(&mut h, &mut b, &fill("S", -1, 1.0, 110.0, 5)); // close @ts5
    assert_eq!(h.active_count(), 0);
    let entries_after_close = untagged_markets(&b).len();
    // within the cooldown window: no re-open even on a fresh signal
    b.now = 500;
    b.price = 130.0;
    Strategy::on_bar(&mut h, &mut b, &bar(6, "S", 130.0, 130.0, 130.0, 130.0));
    assert_eq!(h.active_count(), 0, "cooldown blocks re-open");
    assert_eq!(untagged_markets(&b).len(), entries_after_close, "no new entry during cooldown");
    // past the cooldown (5 + 1000 = 1005): a signal re-opens
    b.now = 1005;
    b.price = 140.0;
    Strategy::on_bar(&mut h, &mut b, &bar(7, "S", 140.0, 140.0, 140.0, 140.0));
    assert_eq!(h.active_count(), 1, "re-opens once the cooldown elapsed");
    assert_eq!(untagged_markets(&b).len(), entries_after_close + 1);
}

#[test]
fn on_params_updated_hot_swaps_controller_and_bumps_epoch() {
    let mut h = harness(); // qty 1, threshold 0, cooldown 0, TP+10
    assert_eq!(h.params_epoch(), 0, "starts at epoch 0");
    assert_eq!(h.controller().qty, 1.0);
    assert_eq!(h.controller().threshold, 0.0);

    // re-tune: qty 1→3, threshold 0→5, cooldown 0→2000, TP 10→25
    let mut b = OrderBrokerMock::default();
    let p = PositionControllerParams::new(
        2_000,
        3.0,
        TripleBarrier::new(Some(25.0), None, None, None),
        5.0,
    );
    Strategy::on_params_updated(&mut h, &mut b, &StrategyParams::PositionController(p));

    assert_eq!(h.params_epoch(), 1, "epoch bumped once by the applied re-tune");
    assert_eq!(h.controller().qty, 3.0, "controller size hot-swapped");
    assert_eq!(h.controller().threshold, 5.0, "controller threshold hot-swapped");
    assert_eq!(h.controller().barriers.take_profit, Some(25.0), "barriers hot-swapped");

    // the swap takes effect on the NEXT evaluate: with the new threshold 5, a +4 move no longer
    // opens, but a +6 move does — and the entry then uses the new size 3.
    let mut b2 = OrderBrokerMock { now: 1, price: 100.0, ..Default::default() };
    Strategy::on_bar(&mut h, &mut b2, &bar(1, "S", 100.0, 100.0, 100.0, 100.0)); // ref 100
    b2.now = 2;
    b2.price = 104.0; // +4 < new threshold 5 → decline
    Strategy::on_bar(&mut h, &mut b2, &bar(2, "S", 104.0, 104.0, 104.0, 104.0));
    assert_eq!(h.active_count(), 0, "the new threshold gates the open (swap took effect)");
    b2.now = 3;
    b2.price = 110.0; // +6 ≥ new threshold 5 → open long at the new size
    Strategy::on_bar(&mut h, &mut b2, &bar(3, "S", 110.0, 110.0, 110.0, 110.0));
    assert_eq!(h.active_count(), 1);
    assert_eq!(
        untagged_markets(&b2),
        vec![("S".to_string(), 1, 3.0)],
        "entry uses the hot-swapped size"
    );
}
