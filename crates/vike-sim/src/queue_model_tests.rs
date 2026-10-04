use super::*;
use vike_model::BookLevel;

fn assert_close(got: f64, want: f64) {
    assert!((got - want).abs() < 1e-12, "got {got}, want {want}");
}

// ---- RiskAdverseQueueModel ----

#[test]
fn risk_adverse_trades_consume_the_front() {
    let m = RiskAdverseQueueModel;
    let mut st = m.on_new_order(5.0);
    assert_close(st.front_qty, 5.0);
    assert!(!m.is_front_cleared(&st));
    m.on_trade(&mut st, 2.0);
    assert_close(st.front_qty, 3.0);
    m.on_trade(&mut st, 3.0);
    assert_close(st.front_qty, 0.0);
    assert!(m.is_front_cleared(&st));
    // over-consumption clamps at zero (never negative)
    m.on_trade(&mut st, 1.0);
    assert_close(st.front_qty, 0.0);
}

#[test]
fn risk_adverse_depth_decrease_clamps_and_increase_is_ignored() {
    let m = RiskAdverseQueueModel;
    let mut st = m.on_new_order(10.0);
    // decrease below the current front → clamp
    m.on_depth_change(&mut st, 10.0, 4.0);
    assert_close(st.front_qty, 4.0);
    // increase joins the BACK → front unchanged
    m.on_depth_change(&mut st, 4.0, 20.0);
    assert_close(st.front_qty, 4.0);
    // decrease still above the front → unchanged (min is a no-op)
    m.on_depth_change(&mut st, 20.0, 6.0);
    assert_close(st.front_qty, 4.0);
    // level wiped → cleared
    m.on_depth_change(&mut st, 6.0, 0.0);
    assert!(m.is_front_cleared(&st));
}

#[test]
fn negative_seed_clamps_to_zero() {
    let m = RiskAdverseQueueModel;
    let st = m.on_new_order(-3.0);
    assert_close(st.front_qty, 0.0);
    assert!(m.is_front_cleared(&st));
}

// ---- ProbQueueModel ----

#[test]
fn prob_power1_splits_decrease_pro_rata() {
    // front 10, back 10 (old 20); decrease to 12 (chg 8): prob = 10/(10+10) = 0.5
    // est = 10 − 0.5·8 + min(10 − 4, 0)=0 → 6, clamp [0,12] → 6.
    let m = ProbQueueModel::power(1.0);
    let mut st = m.on_new_order(10.0);
    m.on_depth_change(&mut st, 20.0, 12.0);
    assert_close(st.front_qty, 6.0);
}

#[test]
fn prob_power1_back_overflow_folds_into_front() {
    // front 10, back 2 (old 12); decrease to 1 (chg 11): prob = 2/12 = 1/6
    // est = 10 − (5/6)·11 + min(2 − 11/6, 0)=0 → 10 − 55/6 = 5/6, clamp [0,1] → 5/6.
    let m = ProbQueueModel::power(1.0);
    let mut st = m.on_new_order(10.0);
    m.on_depth_change(&mut st, 12.0, 1.0);
    assert_close(st.front_qty, 10.0 - (5.0 / 6.0) * 11.0);
}

#[test]
fn prob_trades_are_not_double_counted_by_the_book_echo() {
    // Trade 3 consumes the front (10 → 7) and books cum_trade_qty 3. The venue's depth echo
    // (10 → 7) is then FULLY explained by the trade: chg_net = 0, front stays 7.
    let m = ProbQueueModel::power(1.0);
    let mut st = m.on_new_order(10.0);
    m.on_trade(&mut st, 3.0);
    assert_close(st.front_qty, 7.0);
    assert_close(st.cum_trade_qty, 3.0);
    m.on_depth_change(&mut st, 10.0, 7.0);
    assert_close(st.front_qty, 7.0);
    assert_close(st.cum_trade_qty, 0.0);
}

#[test]
fn prob_partially_trade_explained_decrease_attributes_only_the_net() {
    // Trade 2 (front 10 → 8, cum 2); depth 10 → 5 (chg 5, net 3 after the trade echo).
    // front 8, back = old − front = 2: prob = 2/10 = 0.2
    // est = 8 − 0.8·3 + min(2 − 0.6, 0)=0 → 5.6, clamp [0,5] → 5.
    let m = ProbQueueModel::power(1.0);
    let mut st = m.on_new_order(10.0);
    m.on_trade(&mut st, 2.0);
    m.on_depth_change(&mut st, 10.0, 5.0);
    assert_close(st.front_qty, 5.0);
    assert_close(st.cum_trade_qty, 0.0);
}

#[test]
fn prob_front_zero_attributes_all_change_to_back() {
    // Cleared front stays cleared through back-side cancellations: prob = f(back)/f(back) = 1.
    let m = ProbQueueModel::power(1.0);
    let mut st = m.on_new_order(0.0);
    m.on_depth_change(&mut st, 8.0, 3.0);
    assert_close(st.front_qty, 0.0);
    assert!(m.is_front_cleared(&st));
}

#[test]
fn prob_empty_level_denominator_guard() {
    // old > 0 with front 0 and f(back) = 0 can only happen at back 0 → denom 0 → prob 1
    // (attribute to the back; the front is already empty). Must not NaN.
    let m = ProbQueueModel::power(1.0);
    let mut st = m.on_new_order(0.0);
    m.on_depth_change(&mut st, 0.0, 0.0);
    assert!(st.front_qty == 0.0);
    // and a genuine 0 → 0 "decrease" is a no-op, not a NaN
    assert!(!st.front_qty.is_nan());
}

#[test]
fn prob_power_exponent_biases_attribution() {
    // front 4, back 8 (old 12), decrease 3 (to 9).
    // n=1: prob = 8/12 = 2/3 → est = 4 − 1 = 3.
    // n=2: prob = 64/80 = 0.8 → est = 4 − 0.6 = 3.4 (bigger back soaks more of the change).
    let m1 = ProbQueueModel::power(1.0);
    let mut st1 = m1.on_new_order(4.0);
    m1.on_depth_change(&mut st1, 12.0, 9.0);
    assert_close(st1.front_qty, 3.0);

    let m2 = ProbQueueModel::power(2.0);
    let mut st2 = m2.on_new_order(4.0);
    m2.on_depth_change(&mut st2, 12.0, 9.0);
    assert_close(st2.front_qty, 4.0 - 0.2 * 3.0);
}

#[test]
fn prob_log_weight() {
    // front 4, back 4 (old 8), decrease 2 (to 6): f = ln(1+x) equal → prob 0.5
    // est = 4 − 1 + min(4 − 1, 0)=0 → 3.
    let m = ProbQueueModel::log();
    let mut st = m.on_new_order(4.0);
    m.on_depth_change(&mut st, 8.0, 6.0);
    assert_close(st.front_qty, 3.0);
}

#[test]
fn prob_increase_never_grows_the_front() {
    let m = ProbQueueModel::log();
    let mut st = m.on_new_order(2.0);
    m.on_depth_change(&mut st, 2.0, 50.0);
    assert_close(st.front_qty, 2.0);
}

#[test]
fn prob_depth_increase_supersedes_the_pending_trade_echo() {
    // Adversarial-review regression. Trade 5 consumes the front (10 → 5) and books
    // cum_trade_qty 5 so the venue's echo of that same trade is not double-counted. But the
    // level then GROWS (10 → 12) — an update reports the level's NET state, so the echo has
    // demonstrably already landed. Retaining the stale 5 would excuse a LATER, unrelated
    // 4-lot cancellation from front attribution entirely (chg_net = 4 − 5 ≤ 0), leaving the
    // front too high and fills too slow.
    let m = ProbQueueModel::power(1.0);
    let mut st = m.on_new_order(10.0);
    m.on_trade(&mut st, 5.0);
    assert_close(st.front_qty, 5.0);
    assert_close(st.cum_trade_qty, 5.0);

    m.on_depth_change(&mut st, 10.0, 12.0);
    assert_close(st.cum_trade_qty, 0.0); // superseded
    assert_close(st.front_qty, 5.0); // an increase still never grows the front

    // The pure cancellation is now attributed: front 5, back = 12 − 5 = 7, chg 4,
    // prob = 7/12 → est = 5 − (5/12)·4 = 5 − 5/3. (With the echo retained it stayed 5.)
    m.on_depth_change(&mut st, 12.0, 8.0);
    assert_close(st.front_qty, 5.0 - 5.0 / 3.0);
}

// ---- level identity ----

#[test]
fn same_level_absorbs_ulp_drift_without_merging_ticks() {
    // Adversarial-review regression: a strategy price computed arithmetically (mid − k·tick)
    // accumulates an ulp, which bit-exact matching would read as a different level — seeding
    // `seed_depth` (0.0 = front of queue) instead of the observed depth, silently.
    let drifted = f64::from_bits(99.3f64.to_bits() + 1);
    assert_ne!(drifted, 99.3);
    assert!(same_level(99.3, drifted, Some(0.01)), "tick-grid compare absorbs the drift");
    assert!(same_level(99.3, drifted, None), "so does the bookless epsilon fallback");
    // ADJACENT ticks must still be distinct under both rules
    assert!(!same_level(99.3, 99.31, Some(0.01)));
    assert!(!same_level(99.3, 99.31, None));
    // a nonsense grid falls back to the epsilon rule rather than dividing by zero
    assert!(same_level(99.3, drifted, Some(0.0)));
    assert!(!same_level(99.3, 99.31, Some(0.0)));
}

#[test]
fn bookless_seed_matches_a_drifted_quote_price() {
    use vike_model::QuoteTick;
    let mut tracker = QueueTracker::new(QueueModelKind::RiskAdverse, 7.0, 0, 1);
    tracker.note_quote(
        0,
        &QuoteTick {
            ts: 1,
            local_ts: 0,
            bid: 99.3,
            ask: 101.0,
            bid_size: 4.0,
            ask_size: 2.5,
            symbol: String::new(),
        },
    );
    let drifted = f64::from_bits(99.3f64.to_bits() + 1);
    assert_close(tracker.seed(0, 1, drifted, None).front_qty, 4.0);
    // a genuinely different level still falls through to the configured default
    assert_close(tracker.seed(0, 1, 99.29, None).front_qty, 7.0);
}

#[test]
fn kind_builds_the_matching_model() {
    // observable behavior check: RiskAdverse clamps on decrease, ProbPower(1) splits it.
    let ra = QueueModelKind::RiskAdverse.build();
    let mut st = ra.on_new_order(10.0);
    ra.on_depth_change(&mut st, 20.0, 12.0);
    assert_close(st.front_qty, 10.0); // clamp is a no-op (12 > 10)

    let pp = QueueModelKind::ProbPower(1.0).build();
    let mut st = pp.on_new_order(10.0);
    pp.on_depth_change(&mut st, 20.0, 12.0);
    assert_close(st.front_qty, 6.0); // pro-rata split

    let pl = QueueModelKind::ProbLog.build();
    let mut st = pl.on_new_order(4.0);
    pl.on_depth_change(&mut st, 8.0, 6.0);
    assert_close(st.front_qty, 3.0);
}

// ---- tracker seeding ----

#[test]
fn tracker_seeds_book_then_quote_then_default() {
    use vike_model::QuoteTick;
    let mut tracker = QueueTracker::new(QueueModelKind::RiskAdverse, 7.0, 0, 1);

    // no book, no quote → configured default
    let st = tracker.seed(0, 1, 99.0, None);
    assert_close(st.front_qty, 7.0);

    // matching L1 quote side/price → its size
    tracker.note_quote(
        0,
        &QuoteTick {
            ts: 1,
            local_ts: 0,
            bid: 99.0,
            ask: 101.0,
            bid_size: 4.0,
            ask_size: 2.5,
            symbol: String::new(),
        },
    );
    assert_close(tracker.seed(0, 1, 99.0, None).front_qty, 4.0); // buy @ bid price
    assert_close(tracker.seed(0, -1, 101.0, None).front_qty, 2.5); // sell @ ask price
    assert_close(tracker.seed(0, 1, 98.0, None).front_qty, 7.0); // off-quote price → default

    // a book takes precedence over both
    let mut book = L2Book::new(0.01);
    book.apply_snapshot(1, &[BookLevel::new(99.0, 12.0)], &[BookLevel::new(101.0, 3.0)]);
    assert_close(tracker.seed(0, 1, 99.0, Some(&book)).front_qty, 12.0);
    assert_close(tracker.seed(0, -1, 101.0, Some(&book)).front_qty, 3.0);
    // book present but level absent → trust the book: nothing ahead
    assert_close(tracker.seed(0, 1, 98.0, Some(&book)).front_qty, 0.0);
}
