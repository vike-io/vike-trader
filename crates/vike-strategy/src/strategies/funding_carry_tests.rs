use super::*;
use crate::controller::ControllerHarness;
use crate::position_executor::EntryKind;
use vike_model::strategy::MockBroker;
use vike_model::{Bar, Strategy};

// ---- pure helpers ----

fn q(venue: &str, funding_rate: f64, taker_fee: f64) -> FundingQuote {
    FundingQuote::new(venue, funding_rate, taker_fee)
}

fn bar(ts: i64, symbol: &str, close: f64) -> Bar {
    Bar {
        ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some(symbol.to_string()),
    }
}

// ============================================================================================
// Pure decision core — round-trip cost, net edge, ranking, direction, exit
// ============================================================================================

#[test]
fn roundtrip_cost_sums_both_legs_twice() {
    // each leg crosses twice (entry + exit) → 2 × (long_taker + short_taker)
    assert_eq!(roundtrip_taker_cost(0.001, 0.0005), 2.0 * (0.001 + 0.0005));
    assert_eq!(roundtrip_taker_cost(0.0, 0.0), 0.0);
}

#[test]
fn net_edge_amortizes_cost_over_periods() {
    // hold_periods scales the funding collected; the cost is one-time.
    assert_eq!(net_carry_edge(0.001, 0.002, 1.0), 1.0 * 0.001 - 0.002);
    assert_eq!(net_carry_edge(0.001, 0.002, 8.0), 8.0 * 0.001 - 0.002);
    // a single-period differential below the round-trip cost is NEGATIVE (conservative default).
    assert!(net_carry_edge(0.0008, 0.0031, 1.0) < 0.0);
    // holding longer flips the same pair positive.
    assert!(net_carry_edge(0.0008, 0.0031, 10.0) > 0.0);
}

#[test]
fn rank_none_below_two_venues() {
    assert_eq!(rank_best_carry(&[], 1.0), None);
    assert_eq!(rank_best_carry(&[q("binance", 0.001, 0.0005)], 1.0), None);
}

#[test]
fn rank_longs_the_low_shorts_the_high() {
    // binance funding +0.0001, bybit +0.0006 → long binance (low), short bybit (high).
    let quotes = [q("binance", 0.0001, 0.0), q("bybit", 0.0006, 0.0)];
    let c = rank_best_carry(&quotes, 1.0).unwrap();
    assert_eq!(c.long_venue, "binance");
    assert_eq!(c.short_venue, "bybit");
    assert_eq!(c.gross_differential, 0.0006 - 0.0001);
    assert_eq!(c.roundtrip_cost, 0.0);
    assert_eq!(c.net_edge, 0.0006 - 0.0001);
}

#[test]
fn rank_orients_regardless_of_observation_order() {
    // the HIGH-funding venue observed FIRST must still become the short leg.
    let quotes = [q("bybit", 0.0006, 0.0), q("binance", 0.0001, 0.0)];
    let c = rank_best_carry(&quotes, 1.0).unwrap();
    assert_eq!(c.long_venue, "binance", "lower funding is always the long leg");
    assert_eq!(c.short_venue, "bybit", "higher funding is always the short leg");
    assert_eq!(c.gross_differential, 0.0006 - 0.0001);
}

#[test]
fn rank_handles_negative_funding_long_leg() {
    // negative-funding leg (shorts pay longs) is the natural long leg; differential widens.
    let quotes = [q("binance", -0.0002, 0.0), q("bybit", 0.0006, 0.0)];
    let c = rank_best_carry(&quotes, 1.0).unwrap();
    assert_eq!(c.long_venue, "binance");
    assert_eq!(c.short_venue, "bybit");
    assert_eq!(c.gross_differential, 0.0006 - (-0.0002));
}

#[test]
fn rank_picks_max_net_edge_not_max_gross() {
    // A (0.0), B (0.010), C (0.011) funding; C is a HIGH-fee venue.
    //   A-B: gross 0.010, cost 0                 → net 0.010   (best NET)
    //   A-C: gross 0.011, cost 2*0.010 = 0.020   → net -0.009  (best GROSS, worst net)
    //   B-C: gross 0.001, cost 0.020             → net -0.019
    // So the ranking must prefer A-B (max net) over A-C (max gross) — fees change the winner.
    let quotes = [q("A", 0.0, 0.0), q("B", 0.010, 0.0), q("C", 0.011, 0.010)];
    let c = rank_best_carry(&quotes, 1.0).unwrap();
    assert_eq!((c.long_venue.as_str(), c.short_venue.as_str()), ("A", "B"));
    assert!((c.net_edge - 0.010).abs() < 1e-12, "net edge {}", c.net_edge);
}

#[test]
fn rank_tie_breaks_on_first_pair() {
    // funding [0.0, 0.005, 0.0, 0.005], zero fees: SEVERAL pairs share the maximal net edge
    // 0.005 (A-B, A-D, C-B, C-D). The FIRST-scanned (A-B, i=0/j=1) must win — strictly-greater
    // replacement means a later equal-edge pair never displaces it.
    let quotes = [q("A", 0.0, 0.0), q("B", 0.005, 0.0), q("C", 0.0, 0.0), q("D", 0.005, 0.0)];
    let c = rank_best_carry(&quotes, 1.0).unwrap();
    assert_eq!((c.long_venue.as_str(), c.short_venue.as_str()), ("A", "B"));
}

#[test]
fn best_carry_to_open_gates_on_threshold() {
    // funding 0.0 vs 0.01, zero taker fees → net edge is EXACTLY 0.01 (1.0*(0.01-0.0) - 0.0),
    // so the boundary compare is bit-exact (no subtraction rounding at the threshold).
    let quotes = [q("binance", 0.0, 0.0), q("bybit", 0.01, 0.0)];
    // below threshold → declined
    assert_eq!(best_carry_to_open(&quotes, 1.0, 0.02), None);
    // exactly at threshold → opened
    assert!(best_carry_to_open(&quotes, 1.0, 0.01).is_some());
    // default zero threshold → opened
    assert!(best_carry_to_open(&quotes, 1.0, 0.0).is_some());
}

#[test]
fn carry_leg_side_maps_venue_to_direction() {
    let c = RankedCarry {
        long_venue: "binance".into(),
        short_venue: "bybit".into(),
        gross_differential: 0.0005,
        roundtrip_cost: 0.0,
        net_edge: 0.0005,
    };
    assert_eq!(carry_leg_side(&c, "binance"), Some(1), "long leg is +1");
    assert_eq!(carry_leg_side(&c, "bybit"), Some(-1), "short leg is -1");
    assert_eq!(carry_leg_side(&c, "okx"), None, "a non-leg venue declines");
}

#[test]
fn close_holds_while_edge_intact() {
    // differential still wide, no target hit → hold.
    assert_eq!(should_close_carry(0.0006, 0.0001, 0.0, Some(100.0)), None);
}

#[test]
fn close_on_compression() {
    // differential compressed to/below the exit threshold (still non-negative) → Compressed.
    assert_eq!(should_close_carry(0.0001, 0.0001, 0.0, None), Some(CarryCloseReason::Compressed));
    assert_eq!(should_close_carry(0.00005, 0.0001, 0.0, None), Some(CarryCloseReason::Compressed));
}

#[test]
fn close_on_flip_takes_precedence_over_compression_and_target() {
    // negative differential = the pair now PAYS → Flipped, even though a compression check and
    // an already-hit profit target would ALSO fire (Flipped is checked first).
    assert_eq!(
        should_close_carry(-0.0001, 0.0005, 999.0, Some(1.0)),
        Some(CarryCloseReason::Flipped)
    );
}

#[test]
fn close_on_profit_target_before_compression() {
    // edge still intact (above exit threshold) but the accrued PnL reached the target → close.
    assert_eq!(
        should_close_carry(0.0006, 0.0001, 100.0, Some(100.0)),
        Some(CarryCloseReason::ProfitTarget)
    );
    // below target with the edge intact → hold.
    assert_eq!(should_close_carry(0.0006, 0.0001, 99.0, Some(100.0)), None);
}

// ============================================================================================
// Controller over the funding book (taker fees pulled from the fee registry)
// ============================================================================================

/// A large-differential carry that clears real binance/bybit taker fees in ONE period:
/// binance funding −0.001 (long leg), bybit +0.005 (short leg).
///   gross = 0.005 − (−0.001) = 0.006
///   cost  = 2 × (binance taker 0.0010 + bybit taker 0.00055) = 0.0031
///   net(1 period) = 0.006 − 0.0031 = 0.0029 > 0  → opens at the default zero threshold.
fn loaded_controller() -> FundingCarryController {
    let mut c = FundingCarryController::new(
        "BTCUSDT",
        1.0,
        TripleBarrier::new(None, Some(50.0), Some(28_800_000), None),
    );
    c.observe_funding("binance", -0.001);
    c.observe_funding("bybit", 0.005);
    c
}

#[test]
fn controller_builds_quotes_with_registry_taker_fees() {
    let c = loaded_controller();
    let best = c.best_carry().expect("a profitable carry");
    assert_eq!(best.long_venue, "binance", "negative-funding binance is the long leg");
    assert_eq!(best.short_venue, "bybit", "positive-funding bybit is the short leg");
    assert_eq!(best.gross_differential, 0.005 - (-0.001));
    // real registry taker fees: binance 10 bps, bybit 5.5 bps → 2*(0.001 + 0.00055). The rates
    // come from `bps/10_000.0` divisions, so compare within an ulp-tolerant epsilon.
    assert!((best.roundtrip_cost - 2.0 * (0.001 + 0.00055)).abs() < 1e-15);
    assert!(best.net_edge > 0.0);
}

#[test]
fn controller_opens_the_long_leg_on_the_low_funding_venue() {
    let mut c = loaded_controller();
    let b = MockBroker { px: 30_000.0, ..Default::default() }; // book-less → observe_funding only
    // asked about binance (the low-funding leg) → LONG intent at the configured size.
    let intent = Controller::evaluate(&mut c, &b, "binance", "BTCUSDT").expect("opens the leg");
    assert_eq!(intent.venue, "binance");
    assert_eq!(intent.symbol, "BTCUSDT");
    assert_eq!(intent.side, 1, "long the low-funding leg");
    assert_eq!(intent.qty, 1.0);
    assert_eq!(intent.entry, EntryKind::Market);
    assert_eq!(intent.barriers.stop_loss, Some(50.0));
}

#[test]
fn controller_opens_the_short_leg_on_the_high_funding_venue() {
    let mut c = loaded_controller();
    let b = MockBroker { px: 30_000.0, ..Default::default() };
    // asked about bybit (the high-funding leg) → SHORT intent.
    let intent = Controller::evaluate(&mut c, &b, "bybit", "BTCUSDT").expect("opens the leg");
    assert_eq!(intent.side, -1, "short the high-funding leg");
    assert_eq!(intent.venue, "bybit");
}

#[test]
fn controller_declines_a_venue_outside_the_winning_pair() {
    let mut c = loaded_controller();
    c.observe_funding("okx", 0.002); // a third venue, not a leg of the best (binance/bybit) pair
    let b = MockBroker::default();
    // okx sits between the extremes → not in the winning pair → declines.
    assert_eq!(Controller::evaluate(&mut c, &b, "okx", "BTCUSDT"), None);
    // and the winning legs still open.
    assert!(Controller::evaluate(&mut c, &b, "binance", "BTCUSDT").is_some());
    assert!(Controller::evaluate(&mut c, &b, "bybit", "BTCUSDT").is_some());
}

#[test]
fn controller_declines_a_foreign_symbol() {
    let mut c = loaded_controller();
    let b = MockBroker::default();
    assert_eq!(Controller::evaluate(&mut c, &b, "binance", "ETHUSDT"), None);
}

#[test]
fn controller_declines_when_differential_below_fees() {
    // a thin differential that does NOT clear the round-trip taker cost in one period.
    let mut c = FundingCarryController::new("BTCUSDT", 1.0, TripleBarrier::none());
    c.observe_funding("binance", 0.0001);
    c.observe_funding("bybit", 0.0003); // gross 0.0002 ≪ cost 0.0031 → net < 0
    let b = MockBroker::default();
    assert_eq!(c.best_carry(), None, "net edge below zero threshold → no carry");
    assert_eq!(Controller::evaluate(&mut c, &b, "binance", "BTCUSDT"), None);
}

#[test]
fn controller_auto_observes_the_asked_venue_from_bar_funding() {
    // A broker whose latest bar carries funding auto-populates the asked venue's book slot, so a
    // single explicit observe of the OTHER venue is enough to form a pair.
    struct BarBroker {
        bars: Vec<Bar>,
    }
    impl Broker for BarBroker {
        fn submit_market(&mut self, _s: &str, _side: i32, _q: f64) {}
        fn submit_limit(&mut self, _s: &str, _side: i32, _q: f64, _p: f64) {}
        fn position(&self, _s: &str) -> f64 {
            0.0
        }
        fn price(&self, _s: &str) -> f64 {
            30_000.0
        }
        fn equity(&self) -> f64 {
            0.0
        }
        fn bars(&self, _s: &str) -> &[Bar] {
            &self.bars
        }
        fn index(&self) -> usize {
            0
        }
        fn now(&self) -> i64 {
            0
        }
    }
    let mut c = FundingCarryController::new("BTCUSDT", 1.0, TripleBarrier::none());
    c.observe_funding("bybit", 0.005); // the other leg, explicit
    let mut only_bar = bar(1, "BTCUSDT", 30_000.0);
    only_bar.funding = Some(-0.001); // binance's funding arrives via the bar
    let b = BarBroker { bars: vec![only_bar] };
    // evaluate for binance auto-observes -0.001 from the bar, forming the binance/bybit pair.
    let intent = Controller::evaluate(&mut c, &b, "binance", "BTCUSDT").expect("pair formed");
    assert_eq!(intent.side, 1, "binance long via bar-observed funding");
}

#[test]
fn apply_params_hot_swaps_qty_barriers_threshold_and_preserves_state() {
    let mut c = loaded_controller();
    assert_eq!(c.qty(), 1.0);
    assert_eq!(c.entry_threshold(), 0.0);
    assert_eq!(c.hold_periods(), 1.0, "starts at the default hold horizon");

    let p = ControllerParams::new(
        5_000,                                            // cooldown — the HARNESS's, ignored here
        3.0,                                              // qty 1 → 3
        TripleBarrier::new(Some(20.0), None, None, None), // barriers re-armed
        0.01,                                             // threshold → entry_threshold
    );
    Controller::apply_params(&mut c, &p);

    assert_eq!(c.qty(), 3.0, "size hot-swapped");
    assert_eq!(c.entry_threshold(), 0.01, "entry threshold hot-swapped from ControllerParams");
    assert_eq!(c.barriers().take_profit, Some(20.0), "barriers hot-swapped");
    assert_eq!(c.hold_periods(), 1.0, "hold_periods (not in the bag) preserved");
    // the funding book (evolving STATE) is preserved, so the pair still ranks.
    assert!(c.best_carry().is_none(), "0.006 gross now below the new 0.01 threshold");
    // widen the differential enough to clear the new threshold → still ranks binance/bybit.
    c.observe_funding("bybit", 0.02);
    let best = c.best_carry().expect("clears the new threshold");
    assert_eq!(best.long_venue, "binance");
}

// ============================================================================================
// Harness integration + OFF/default (byte-identical when off) proofs
// ============================================================================================

#[test]
fn harness_opens_one_sided_leg_for_its_venue() {
    // Mount the controller in the EXISTING single-venue harness under "binance": it opens ONLY
    // the binance (long) leg — the documented single-venue-harness behavior.
    let mut c = FundingCarryController::new("BTCUSDT", 2.0, TripleBarrier::none());
    c.observe_funding("binance", -0.001);
    c.observe_funding("bybit", 0.005);
    let mut h = ControllerHarness::new(c, "binance", 0);
    let mut b = MockBroker { now: 1, px: 30_000.0, ..Default::default() };
    Strategy::on_bar(&mut h, &mut b, &bar(1, "BTCUSDT", 30_000.0));
    assert_eq!(h.active_count(), 1, "one leg opened");
    assert_eq!(
        b.markets,
        vec![("BTCUSDT".to_string(), 1, 2.0)],
        "a single long market entry at the configured size"
    );
}

#[test]
fn off_default_unfed_controller_places_no_orders() {
    // OFF / byte-identical-when-off proof: a freshly-constructed controller (no funding observed)
    // mounted in a harness and fed bars places ZERO orders — inert, exactly like a mount with no
    // strategy. Nothing opens until a profitable cross-venue carry is fed in.
    let c = FundingCarryController::new("BTCUSDT", 1.0, TripleBarrier::none());
    let mut h = ControllerHarness::new(c, "binance", 0);
    let mut b = MockBroker { now: 1, px: 30_000.0, ..Default::default() };
    for ts in 1..=5 {
        Strategy::on_bar(&mut h, &mut b, &bar(ts, "BTCUSDT", 30_000.0));
    }
    assert_eq!(h.active_count(), 0, "no executor without a fed carry");
    assert!(b.markets.is_empty(), "no market orders");
    assert!(b.limits.is_empty(), "no limit orders");
}

#[test]
fn off_single_venue_never_opens() {
    // A carry needs a PAIR: with only ONE venue observed the controller stays inert (no second
    // leg to rank against), regardless of how attractive that one venue's funding is.
    let mut c = FundingCarryController::new("BTCUSDT", 1.0, TripleBarrier::none());
    c.observe_funding("binance", -0.05); // a huge negative funding, but still a single venue
    let b = MockBroker::default();
    assert_eq!(c.best_carry(), None, "one venue can never form a delta-neutral pair");
    assert_eq!(Controller::evaluate(&mut c, &b, "binance", "BTCUSDT"), None, "no leg opened");
}
