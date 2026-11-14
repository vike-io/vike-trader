use super::*;
use crate::position_executor::PositionExecutorState;
use vike_model::{QuoteStyle, SpreadMakerParams, TripleBarrier, TripleBarrierKind};
// vike-model's recording `OrderBroker` double (`test-support` feature), as in the machine tests;
// `position()` reads the mock's `pos` (0 by default).
use crate::position_executor::machine_tests::untagged_markets;
use vike_model::strategy::OrderBrokerMock;

// The harness's test exemplar, moved here from the library when the shipped strategies became
// files: the strategy is `strategies/trend/momentum.rs`'s `MomentumController` (with its
// `from_params`); this copy keeps only what these tests drive.
/// A minimal REFERENCE [`Controller`] (an example + the harness's test exemplar, NOT a trading
/// recommendation): a price-momentum threshold. Each invitation (only while the pair is flat AND
/// past cooldown) compares the mark ([`OrderBroker::price`]) to the mark at its PREVIOUS invitation: a
/// move `>= threshold` opens LONG, `<= -threshold` SHORT, otherwise it declines. The first-ever
/// invitation only records the mark. Deterministic and read-only, so parity-clean on the bar and
/// tick paths.
#[derive(Debug, Clone)]
pub struct MomentumController {
    /// Position size (units) each opened intent carries.
    pub qty: f64,
    /// Absolute price move (since the last invitation) that arms a long (`>=`) / short (`<= -`).
    /// `0.0` ⇒ always open from the second invitation on (up/flat → long, down → short).
    pub threshold: f64,
    /// The triple barrier every opened position is guarded by (market entry, emulated exits).
    pub barriers: TripleBarrier,
    /// The mark observed at the previous invitation (`None` before the first) — the momentum origin.
    last_ref: Option<f64>,
}

impl MomentumController {
    /// A momentum controller sizing every entry at `qty`, arming on a `threshold` move, guarding each
    /// position with `barriers`. Starts with no reference (declines its first invitation).
    pub fn new(qty: f64, threshold: f64, barriers: TripleBarrier) -> Self {
        MomentumController { qty, threshold, barriers, last_ref: None }
    }
}

impl Controller for MomentumController {
    fn evaluate<B: OrderBroker>(
        &mut self,
        broker: &B,
        venue: &str,
        symbol: &str,
    ) -> Option<PositionExecutorIntent> {
        let px = broker.price(symbol);
        let side = match self.last_ref {
            Some(previous) if px - previous >= self.threshold => 1,
            Some(previous) if px - previous <= -self.threshold => -1,
            _ => 0, // no reference yet, or the move is inside the threshold band
        };
        self.last_ref = Some(px);
        if side == 0 {
            return None;
        }
        Some(PositionExecutorIntent::market(venue, symbol, side, self.qty, self.barriers))
    }

    /// Hot-swap `qty`, `threshold` and `barriers` (`cooldown_ms` is the harness's). `last_ref` is
    /// DELIBERATELY kept: it is evolving STATE, not a tunable, so the new `threshold` applies on the
    /// very next [`evaluate`](Controller::evaluate) without a spurious first-invitation reset.
    fn apply_params(&mut self, params: &PositionControllerParams) {
        self.qty = params.qty;
        self.threshold = params.threshold;
        self.barriers = params.barriers;
    }
}

fn bar(ts: i64, symbol: &str, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        timestamp: ts,
        open,
        high,
        low,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some(symbol.to_string()),
    }
}

fn fill(symbol: &str, side: i32, size: f64, price: f64, ts: i64) -> Fill {
    Fill {
        side,
        size,
        price,
        fee: 0.0,
        timestamp: ts,
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
fn fills_route_by_symbol_across_two_pairs() {
    // two symbols → two independent executors; a fill only advances its own.
    let control =
        MomentumController::new(1.0, 0.0, TripleBarrier::new(Some(10.0), None, None, None));
    let mut h = ControllerHarness::new(control, "v", 0);
    let mut b = OrderBrokerMock { now: 1, price: 100.0, ..Default::default() };
    // prime + open A
    Strategy::on_bar(&mut h, &mut b, &bar(1, "A", 100.0, 100.0, 100.0, 100.0));
    b.now = 2;
    b.price = 101.0;
    Strategy::on_bar(&mut h, &mut b, &bar(2, "A", 101.0, 101.0, 101.0, 101.0));
    // prime + open B (shared controller reference is per-invocation; both open long on the rise)
    Strategy::on_bar(&mut h, &mut b, &bar(2, "B", 100.0, 100.0, 100.0, 100.0));
    b.price = 102.0;
    Strategy::on_bar(&mut h, &mut b, &bar(3, "B", 102.0, 102.0, 102.0, 102.0));
    assert_eq!(h.active_count(), 2, "one executor per symbol");
    // a fill for A only opens A; B stays EntryWorking
    Strategy::on_fill(&mut h, &mut b, &fill("A", 1, 1.0, 100.0, 4));
    let a = h.executors.iter().find(|(k, _)| k.1 == "A").unwrap();
    let b_ex = h.executors.iter().find(|(k, _)| k.1 == "B").unwrap();
    assert_eq!(a.1.state(), PositionExecutorState::Open, "A opened on its fill");
    assert_eq!(b_ex.1.state(), PositionExecutorState::EntryWorking, "B unaffected by A's fill");
}

// ====================================================================================
// Live params (StrategyParams::PositionController + on_params_updated hot-swap)
// ====================================================================================

/// A full [`SpreadMakerParams`] — the FOREIGN variant used to prove the harness ignores it.
fn a_spreadmaker_params() -> SpreadMakerParams {
    SpreadMakerParams {
        qty: 9.0,
        half_spread: 9.0,
        target_inventory: 0.0,
        max_inventory: 1.0,
        skew: 0.0,
        fill_window_ms: 0,
        net_fill_threshold: 0.0,
        suppress_cooldown_ms: 0,
        style: QuoteStyle::Mid,
        depth_levels: 1,
        tick_size: 0.0,
        filter_own: false,
        avellaneda_stoikov: None,
        refresh_tolerance: None,
        ladder: None,
        reward: None,
        toxicity: None,
    }
}

#[test]
fn controller_params_round_trips_serde() {
    let p = PositionControllerParams::new(
        1_500,
        2.5,
        TripleBarrier::new(Some(10.0), Some(4.0), Some(60_000), Some(2.0)),
        3.5,
    );
    let json = serde_json::to_string(&p).unwrap();
    assert_eq!(p, serde_json::from_str::<PositionControllerParams>(&json).unwrap());
    // and wrapped in the journaled StrategyParams union (the shape Command::UpdateParams carries)
    let sp = StrategyParams::PositionController(p);
    let j2 = serde_json::to_string(&sp).unwrap();
    assert_eq!(sp, serde_json::from_str::<StrategyParams>(&j2).unwrap());
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
    assert_eq!(h.cooldown_ms, 2_000, "harness cooldown hot-swapped");

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

#[test]
fn on_params_updated_is_a_noop_for_a_foreign_variant() {
    let mut h = harness(); // qty 1, threshold 0, cooldown 0
    let mut b = OrderBrokerMock::default();
    // a SpreadMaker update is NOT ours: nothing swaps, the epoch does not move.
    Strategy::on_params_updated(
        &mut h,
        &mut b,
        &StrategyParams::SpreadMaker(a_spreadmaker_params()),
    );
    assert_eq!(h.params_epoch(), 0, "a foreign variant does not bump the epoch");
    assert_eq!(h.controller().qty, 1.0, "controller size untouched by a foreign variant");
    assert_eq!(h.controller().threshold, 0.0, "controller threshold untouched");
    assert_eq!(h.cooldown_ms, 0, "harness cooldown untouched");
}

#[test]
fn retune_does_not_tear_down_an_in_flight_executor() {
    let mut h = harness(); // qty 1, threshold 0, cooldown 0, TP+10
    let mut b = OrderBrokerMock { now: 1, price: 100.0, ..Default::default() };
    Strategy::on_bar(&mut h, &mut b, &bar(1, "S", 100.0, 100.0, 100.0, 100.0)); // ref
    b.now = 2;
    b.price = 101.0;
    Strategy::on_bar(&mut h, &mut b, &bar(2, "S", 101.0, 101.0, 101.0, 101.0)); // open long, qty 1
    assert_eq!(h.active_count(), 1);
    assert_eq!(
        untagged_markets(&b),
        vec![("S".to_string(), 1, 1.0)],
        "entry at the ORIGINAL size 1"
    );

    // re-tune mid-flight to a very different size/barrier while the entry is still working.
    let p = PositionControllerParams::new(
        0,
        9.0,
        TripleBarrier::new(Some(50.0), None, None, None),
        0.0,
    );
    Strategy::on_params_updated(&mut h, &mut b, &StrategyParams::PositionController(p));

    // the in-flight executor SURVIVES and KEEPS its original intent — a re-tune only affects
    // FUTURE opens; it is never torn down or retroactively resized.
    assert_eq!(h.active_count(), 1, "in-flight executor not torn down by the re-tune");
    let (_, ex) = h.executors.iter().find(|(k, _)| k.1 == "S").unwrap();
    assert_eq!(ex.state(), PositionExecutorState::EntryWorking, "still working its entry");
    assert_eq!(ex.intent().qty, 1.0, "in-flight keeps its ORIGINAL size (not resized to 9)");
    assert_eq!(
        ex.intent().barriers.take_profit,
        Some(10.0),
        "in-flight keeps its ORIGINAL barriers (not re-armed to +50)"
    );
    assert_eq!(h.params_epoch(), 1, "the re-tune still landed (epoch bumped)");
    assert_eq!(
        untagged_markets(&b).len(),
        1,
        "no extra order minted by the re-tune (broker untouched)"
    );
}
