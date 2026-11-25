use super::*;

use vike_marketdata::test_support::shape_bar;

/// The first barrier `b` hits for a `side` position entered at 100 at t = 0, judged at t = 0 over
/// one `bar`, the trailing extreme seeded from the entry.
fn first_hit(b: TripleBarrier, side: i32, bar: Bar) -> Option<TripleBarrierHit> {
    evaluate_barriers(100.0, side, 0, &b, 0, &bar, &mut None)
}

// --- price helpers (golden) ---

#[test]
fn barrier_prices_long_and_short() {
    // long entered at 100: TP is +10 above, SL is -5 below
    assert_eq!(take_profit_price(100.0, 1, 10.0), 110.0);
    assert_eq!(stop_loss_price(100.0, 1, 5.0), 95.0);
    // short entered at 100: TP is -10 below, SL is +5 above
    assert_eq!(take_profit_price(100.0, -1, 10.0), 90.0);
    assert_eq!(stop_loss_price(100.0, -1, 5.0), 105.0);
}

// --- take-profit ---

#[test]
fn take_profit_hit_long() {
    let b = TripleBarrier::new(Some(10.0), None, None, None); // tp @ 110
    // bar rallies through 110
    let hit = first_hit(b, 1, shape_bar(105.0, 112.0, 104.0, 111.0));
    assert_eq!(
        hit,
        Some(TripleBarrierHit { kind: TripleBarrierKind::TakeProfit, exit_price: 110.0 })
    );
}

#[test]
fn take_profit_hit_short() {
    let b = TripleBarrier::new(Some(10.0), None, None, None); // tp @ 90
    // bar dips through 90
    let hit = first_hit(b, -1, shape_bar(95.0, 96.0, 88.0, 89.0));
    assert_eq!(
        hit,
        Some(TripleBarrierHit { kind: TripleBarrierKind::TakeProfit, exit_price: 90.0 })
    );
}

#[test]
fn take_profit_gap_open_improves_fill_long() {
    // gap UP through the 110 target: a favourable limit fills at the better open (113), not 110
    let b = TripleBarrier::new(Some(10.0), None, None, None);
    let hit = first_hit(b, 1, shape_bar(113.0, 114.0, 112.0, 113.5));
    assert_eq!(
        hit,
        Some(TripleBarrierHit { kind: TripleBarrierKind::TakeProfit, exit_price: 113.0 })
    );
}

// --- stop-loss ---

#[test]
fn stop_loss_hit_long() {
    let b = TripleBarrier::new(None, Some(5.0), None, None); // sl @ 95
    let hit = first_hit(b, 1, shape_bar(98.0, 99.0, 94.0, 94.5));
    assert_eq!(hit, Some(TripleBarrierHit { kind: TripleBarrierKind::StopLoss, exit_price: 95.0 }));
}

#[test]
fn stop_loss_hit_short() {
    let b = TripleBarrier::new(None, Some(5.0), None, None); // sl @ 105
    let hit = first_hit(b, -1, shape_bar(102.0, 106.0, 101.0, 105.5));
    assert_eq!(
        hit,
        Some(TripleBarrierHit { kind: TripleBarrierKind::StopLoss, exit_price: 105.0 })
    );
}

#[test]
fn stop_loss_gap_open_worsens_fill_long() {
    // gap DOWN through the 95 stop: an adverse stop fills at the worse open (92), not 95
    // (mirrors OrderEmulationBook::gap_open_fills_adverse)
    let b = TripleBarrier::new(None, Some(5.0), None, None);
    let hit = first_hit(b, 1, shape_bar(92.0, 93.0, 91.0, 92.5));
    assert_eq!(hit, Some(TripleBarrierHit { kind: TripleBarrierKind::StopLoss, exit_price: 92.0 }));
}

// --- time barrier (the new leg) ---

#[test]
fn time_barrier_hit_boundary() {
    // deadline = 1000 + 500 = 1500: not before, fires AT and after
    assert!(!time_barrier_hit(1000, 500, 1499));
    assert!(time_barrier_hit(1000, 500, 1500));
    assert!(time_barrier_hit(1000, 500, 1600));
}

#[test]
fn time_barrier_via_evaluate_fires_at_deadline() {
    let b = TripleBarrier::new(None, None, Some(500), None); // deadline entry_timestamp+500
    let mut ext = None;
    let quiet = shape_bar(100.0, 101.0, 99.0, 100.5); // no price barrier could fire (none armed)
    // before the deadline: nothing
    assert_eq!(evaluate_barriers(100.0, 1, 1000, &b, 1499, &quiet, &mut ext), None);
    // at the deadline: Time fires, exit_price = current mark (bar.close)
    assert_eq!(
        evaluate_barriers(100.0, 1, 1000, &b, 1500, &quiet, &mut ext),
        Some(TripleBarrierHit { kind: TripleBarrierKind::Time, exit_price: 100.5 }),
    );
}

// --- none-hit ---

#[test]
fn none_hit_within_range() {
    let b = TripleBarrier::new(Some(10.0), Some(5.0), None, None); // tp 110, sl 95
    // bar stays between 95 and 110
    assert_eq!(first_hit(b, 1, shape_bar(100.0, 108.0, 96.0, 102.0)), None);
}

// --- precedence ---

#[test]
fn precedence_stop_before_target_on_a_wide_bar() {
    // GOLDEN anchor: a bar straddling BOTH the 95 stop and the 110 target must resolve to the
    // STOP (assume the adverse leg hit first — no intrabar look-ahead optimism).
    let b = TripleBarrier::new(Some(10.0), Some(5.0), None, None); // tp 110, sl 95
    let hit = first_hit(b, 1, shape_bar(100.0, 115.0, 90.0, 112.0));
    assert_eq!(hit, Some(TripleBarrierHit { kind: TripleBarrierKind::StopLoss, exit_price: 95.0 }));
}

#[test]
fn precedence_price_barrier_before_time() {
    // past the deadline AND the bar crosses the stop → report the price barrier, not the timeout.
    let b = TripleBarrier::new(None, Some(5.0), Some(500), None); // sl 95, deadline entry_timestamp+500
    let mut ext = None;
    let hit =
        evaluate_barriers(100.0, 1, 1000, &b, 1500, &shape_bar(98.0, 99.0, 94.0, 94.5), &mut ext);
    assert_eq!(hit, Some(TripleBarrierHit { kind: TripleBarrierKind::StopLoss, exit_price: 95.0 }));
}

// --- trailing (ratchet parity vs OrderEmulationBook) ---

/// A trail-5 stop for a `side` position whose extreme is seeded at 100 (entry 100): `ratchet` moves
/// the extreme to `extreme` WITHOUT firing, then `fire` fires the trailing stop at `exit_price`.
fn assert_trailing_ratchets_then_fires(
    side: i32,
    ratchet: Bar,
    extreme: f64,
    fire: Bar,
    exit_px: f64,
) {
    let b = TripleBarrier::new(None, None, None, Some(5.0));
    let mut ext = Some(100.0);
    assert_eq!(evaluate_barriers(100.0, side, 0, &b, 0, &ratchet, &mut ext), None);
    assert_eq!(ext, Some(extreme));
    assert_eq!(
        evaluate_barriers(100.0, side, 0, &b, 0, &fire, &mut ext),
        Some(TripleBarrierHit { kind: TripleBarrierKind::Trailing, exit_price: exit_px }),
    );
}

#[test]
fn trailing_ratchets_in_place_then_fires_long() {
    // Replicates vike-core OrderEmulationBook::trailing_ratchets_in_place_then_fires exactly:
    // long protection, trail 5, extreme seeded 100 → trigger 95. A new high 110 ratchets the
    // extreme (low 101 stays above the OLD trigger 95 → no fire); the trigger is then 105, so a dip
    // to 104 fires at 105 (GOLDEN — identical to OrderEmulationBook).
    assert_trailing_ratchets_then_fires(
        1,
        shape_bar(105.0, 110.0, 101.0, 108.0),
        110.0,
        shape_bar(106.0, 107.0, 104.0, 104.5),
        105.0,
    );
}

#[test]
fn trailing_new_high_bar_cannot_stop_itself_out() {
    // the oracle checks the PRIOR extreme's trigger before ratcheting (mirrors OrderEmulationBook)
    let b = TripleBarrier::new(None, None, None, Some(5.0));
    let mut ext = Some(100.0);
    // high 120 would imply trigger 115 — but the low 103 is compared to the PRIOR trigger 95: no fire
    assert_eq!(
        evaluate_barriers(100.0, 1, 0, &b, 0, &shape_bar(110.0, 120.0, 103.0, 118.0), &mut ext),
        None,
    );
    assert_eq!(ext, Some(120.0));
}

#[test]
fn trailing_lazy_seeds_from_entry_price() {
    // passing &mut None seeds the extreme from entry_price (100) → trigger 95; a dip to 94 fires at 95
    let b = TripleBarrier::new(None, None, None, Some(5.0));
    let hit = first_hit(b, 1, shape_bar(96.0, 97.0, 94.0, 94.5));
    assert_eq!(hit, Some(TripleBarrierHit { kind: TripleBarrierKind::Trailing, exit_price: 95.0 }));
}

#[test]
fn trailing_short_ratchets_down_then_fires() {
    // short protection: buy-stop trailing the LOW. trail 5, extreme seeded 100 → trigger 105. Low
    // 90 ratchets the extreme DOWN (high 96 stays below the OLD trigger 105 → no fire); the trigger
    // is then 95, so a rally to 96 fires at 95.
    assert_trailing_ratchets_then_fires(
        -1,
        shape_bar(95.0, 96.0, 90.0, 94.0),
        90.0,
        shape_bar(94.0, 96.0, 93.0, 95.0),
        95.0,
    );
}

// --- tick convenience ---

#[test]
fn evaluate_at_price_tick_path() {
    // tp 110, sl 95
    let b = TripleBarrier::new(Some(10.0), Some(5.0), None, None);
    // a tick exactly AT the 110 take-profit fills there
    let mut ext = None;
    assert_eq!(
        evaluate_barriers_at_price(100.0, 1, 0, &b, 0, 110.0, &mut ext),
        Some(TripleBarrierHit { kind: TripleBarrierKind::TakeProfit, exit_price: 110.0 }),
    );
    // a tick BELOW the 95 stop fills at the tick itself — on a degenerate (single-price) bar the
    // crossing price IS the fill (same as OrderEmulationBook::check_price: min(95, open=94) = 94)
    let mut ext2 = None;
    assert_eq!(
        evaluate_barriers_at_price(100.0, 1, 0, &b, 0, 94.0, &mut ext2),
        Some(TripleBarrierHit { kind: TripleBarrierKind::StopLoss, exit_price: 94.0 }),
    );
    // a tick between the barriers: nothing
    let mut ext3 = None;
    assert_eq!(evaluate_barriers_at_price(100.0, 1, 0, &b, 0, 100.0, &mut ext3), None);
}
