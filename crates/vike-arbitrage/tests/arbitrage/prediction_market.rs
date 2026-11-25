//! The outcome-set lock and the cached-marks screen, moved here from Polymarket's bridge (the numbers are the
//! ones its tests asserted, unchanged): the maths is venue-neutral, the bridge's tests keep the wiring.

use std::collections::HashMap;

use vike_arbitrage::prediction_market::cached_marks::buy_all_yes_edge;
use vike_arbitrage::prediction_market::outcome_set_lock::{
    LockKind, OutcomeSet, OutcomeSetConfig, PayoutTiming, SizePolicy, detect, evaluate_lock,
    sum_best_asks,
};
use vike_marketdata::{L2Book, L2BookLevel};
use vike_model::FeeSchedule;

/// The crypto up/down p(1−p) taker rate (0.072) — a fee-bearing curve for the netting tests.
const CRYPTO_CURVE: FeeSchedule =
    FeeSchedule::ProbabilityScaled { taker_rate: 0.072, maker_rate: 0.0, maker_rebate_share: 0.0 };

/// A venue that converts a full NO set at once, with no fee, sized at the top of book.
fn free_top_of_book() -> OutcomeSetConfig {
    OutcomeSetConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook, PayoutTiming::Immediately)
}

/// A book with a single ask level `(price, qty)` (and no bids — we only buy).
fn ask_book(price: f64, qty: f64) -> L2Book {
    let mut b = L2Book::new(0.01);
    b.apply_snapshot(1, &[], &[L2BookLevel::new(price, qty)]);
    b
}

/// A book with multiple ask levels (best first in the slice; order does not matter to the book).
fn ask_book_levels(levels: &[L2BookLevel]) -> L2Book {
    let mut b = L2Book::new(0.01);
    b.apply_snapshot(1, &[], levels);
    b
}

// ---- sum_best_asks (the gross top-of-book screen) ----

#[test]
fn sum_best_asks_folds_tops_and_none_on_a_missing_side() {
    let a = ask_book(0.30, 10.0);
    let b = ask_book(0.28, 10.0);
    let c = ask_book(0.29, 10.0);
    let legs = [&a, &b, &c];
    let s = sum_best_asks(&legs).unwrap();
    assert!((s - 0.87).abs() < 1e-9, "s={s}");
    // an empty (no-ask) leg makes the whole screen unpriceable
    let empty = L2Book::new(0.01);
    let with_gap = [&a, &empty, &c];
    assert!(sum_best_asks(&with_gap).is_none());
}

// ---- evaluate_lock: the pure netting math ----

#[test]
fn yes_lock_nets_the_fee_at_top_of_book() {
    // Σ YES asks = 0.30+0.30+0.28 = 0.88 < 1 → a 0.12/share gross edge. Thinnest leg = 40 shares.
    let a = ask_book(0.30, 100.0);
    let b = ask_book(0.30, 40.0);
    let c = ask_book(0.28, 100.0);
    let legs = [&a, &b, &c];
    let config = free_top_of_book();
    let lock = evaluate_lock(&legs, 1.0, &config).expect("net-positive YES lock");
    assert!((lock.size - 40.0).abs() < 1e-9, "sizes to the thinnest best-ask depth");
    // gross cost = 40 * 0.88 = 35.2 ; payout = 40 ; net = 4.8
    assert!((lock.gross_cost - 35.2).abs() < 1e-6, "gross={}", lock.gross_cost);
    assert_eq!(lock.fee, 0.0, "Free curve charges nothing");
    assert!((lock.payout - 40.0).abs() < 1e-9);
    assert!((lock.net_profit - 4.8).abs() < 1e-6, "net={}", lock.net_profit);

    // with the crypto p(1−p) curve the fee is subtracted: Σ 40*0.072*p*(1−p) over the three legs.
    let config_fee =
        OutcomeSetConfig::new(CRYPTO_CURVE, SizePolicy::TopOfBook, PayoutTiming::Immediately);
    let locked = evaluate_lock(&legs, 1.0, &config_fee).expect("still net-positive");
    let expect_fee = 40.0 * 0.072 * (0.30 * 0.70 + 0.30 * 0.70 + 0.28 * 0.72);
    assert!((locked.fee - expect_fee).abs() < 1e-6, "fee={} vs {}", locked.fee, expect_fee);
    assert!((locked.net_profit - (4.8 - expect_fee)).abs() < 1e-6, "net={}", locked.net_profit);
    assert!(locked.net_profit < lock.net_profit, "the fee strictly reduces the netted edge");
}

#[test]
fn no_lock_pays_n_minus_one_and_nets_correctly() {
    // N=3 → payout (N−1)=2/share. Σ NO asks = 0.60+0.60+0.55 = 1.75 < 2 → 0.25/share gross edge.
    let a = ask_book(0.60, 50.0);
    let b = ask_book(0.60, 50.0);
    let c = ask_book(0.55, 50.0);
    let legs = [&a, &b, &c];
    let lock = evaluate_lock(&legs, 2.0, &free_top_of_book()).expect("net-positive NO lock");
    assert!((lock.size - 50.0).abs() < 1e-9);
    // gross = 50 * 1.75 = 87.5 ; payout = 2 * 50 = 100 ; net = 12.5
    assert!((lock.gross_cost - 87.5).abs() < 1e-6, "gross={}", lock.gross_cost);
    assert!((lock.payout - 100.0).abs() < 1e-9);
    assert!((lock.net_profit - 12.5).abs() < 1e-6, "net={}", lock.net_profit);
}

#[test]
fn a_consistent_set_side_nets_nothing() {
    // A spread-bearing consistent set: YES asks sum to 1.06 (> 1). No YES lock.
    let a = ask_book(0.52, 100.0);
    let b = ask_book(0.32, 100.0);
    let c = ask_book(0.22, 100.0);
    let legs = [&a, &b, &c];
    assert!(
        evaluate_lock(&legs, 1.0, &free_top_of_book()).is_none(),
        "Σ YES asks 1.06 > 1 → no lock"
    );
}

#[test]
fn a_fee_can_flip_a_thin_gross_edge_to_no_lock() {
    // Σ YES asks = 0.33+0.33+0.33 = 0.99 < 1 → a thin 0.01/share gross edge.
    let a = ask_book(0.33, 100.0);
    let b = ask_book(0.33, 100.0);
    let c = ask_book(0.33, 100.0);
    let legs = [&a, &b, &c];
    // gross: net-positive
    assert!(evaluate_lock(&legs, 1.0, &free_top_of_book()).is_some(), "gross edge exists");
    // fee: 3 * 100 * 0.072 * 0.33 * 0.67 ≈ 4.77/100sh > the 1.0 gross edge → net negative → None
    let fee = OutcomeSetConfig::new(CRYPTO_CURVE, SizePolicy::TopOfBook, PayoutTiming::Immediately);
    assert!(evaluate_lock(&legs, 1.0, &fee).is_none(), "the p(1−p) fee eats the thin edge");
}

#[test]
fn fixed_size_walks_the_book_and_pays_slippage() {
    // Two legs at 0.30 (deep) and one leg 40@0.30 then 100@0.34. Buying 100 walks the third leg.
    let a = ask_book(0.30, 200.0);
    let b = ask_book(0.30, 200.0);
    let c = ask_book_levels(&[L2BookLevel::new(0.30, 40.0), L2BookLevel::new(0.34, 100.0)]);
    let legs = [&a, &b, &c];
    // TopOfBook sizes to the thin 40 @ 0.30 → no slippage, Σ vwap 0.90.
    let top = evaluate_lock(&legs, 1.0, &free_top_of_book()).expect("top lock");
    assert!((top.size - 40.0).abs() < 1e-9);
    assert!((top.gross_cost - 40.0 * 0.90).abs() < 1e-6, "gross={}", top.gross_cost);

    // Fixed 100 walks leg c: 40@0.30 + 60@0.34 → vwap_c = (40*0.30+60*0.34)/100 = 0.324.
    let config100 = OutcomeSetConfig::new(
        FeeSchedule::Free,
        SizePolicy::Fixed(100.0),
        PayoutTiming::Immediately,
    );
    let walked = evaluate_lock(&legs, 1.0, &config100).expect("100-lot lock");
    assert!((walked.size - 100.0).abs() < 1e-9);
    let vwap_c = (40.0 * 0.30 + 60.0 * 0.34) / 100.0;
    let gross = 100.0 * (0.30 + 0.30 + vwap_c);
    assert!((walked.gross_cost - gross).abs() < 1e-6, "gross={} vs {}", walked.gross_cost, gross);
    // the walk cost is real: per-share edge shrank vs the no-slippage top-of-book slice
    assert!(
        walked.net_profit / walked.size < top.net_profit / top.size,
        "walking to worse levels lowers the per-share edge"
    );
}

#[test]
fn fixed_size_beyond_displayed_depth_is_unlockable() {
    let a = ask_book(0.30, 100.0);
    let b = ask_book(0.30, 100.0);
    let c = ask_book(0.28, 50.0); // only 50 available on this leg
    let legs = [&a, &b, &c];
    let at = |size: f64| {
        OutcomeSetConfig::new(FeeSchedule::Free, SizePolicy::Fixed(size), PayoutTiming::Immediately)
    };
    // 80 > 50 on leg c: the set cannot be completed at 80 shares → no lock, no partial phantom.
    assert!(evaluate_lock(&legs, 1.0, &at(80.0)).is_none());
    // …but 50 (exactly leg c's depth) IS lockable.
    assert!(evaluate_lock(&legs, 1.0, &at(50.0)).is_some());
}

#[test]
fn evaluate_lock_degenerate_inputs() {
    let a = ask_book(0.30, 10.0);
    let fixed = |size: f64| {
        OutcomeSetConfig::new(FeeSchedule::Free, SizePolicy::Fixed(size), PayoutTiming::Immediately)
    };
    let top = free_top_of_book();
    assert!(evaluate_lock(&[], 1.0, &top).is_none(), "empty legs");
    // non-positive / NaN fixed size
    assert!(evaluate_lock(&[&a], 1.0, &fixed(0.0)).is_none(), "zero size");
    assert!(evaluate_lock(&[&a], 1.0, &fixed(f64::NAN)).is_none(), "NaN size");
    // a leg with no ask under TopOfBook
    let empty = L2Book::new(0.01);
    assert!(evaluate_lock(&[&a, &empty], 1.0, &top).is_none(), "a leg with no ask");
}

// ---- detect: the set-level lock, venue-neutral ----

/// Build the token→book lookup a `detect` call closes over.
fn books(pairs: &[(&str, L2Book)]) -> HashMap<String, L2Book> {
    pairs.iter().map(|(id, b)| (id.to_string(), b.clone())).collect()
}

const YES: [&str; 3] = ["yes0", "yes1", "yes2"];
const NO: [&str; 3] = ["no0", "no1", "no2"];

fn three() -> OutcomeSet<'static> {
    OutcomeSet { outcomes: 3, yes_tokens: &YES, no_tokens: &NO }
}

#[test]
fn detect_flags_a_yes_underpriced_set() {
    // YES asks 0.30/0.30/0.28 = 0.88 < 1 → YES lock. NO asks priced consistently (no NO lock).
    let bk = books(&[
        ("yes0", ask_book(0.30, 100.0)),
        ("yes1", ask_book(0.30, 100.0)),
        ("yes2", ask_book(0.28, 100.0)),
        ("no0", ask_book(0.72, 100.0)),
        ("no1", ask_book(0.72, 100.0)),
        ("no2", ask_book(0.74, 100.0)), // Σ NO = 2.18 > 2 → no NO lock
    ]);
    let locks = detect(&three(), |id| bk.get(id), &free_top_of_book());
    assert_eq!(locks.len(), 1, "only the YES lock: {locks:?}");
    let o = &locks[0];
    assert_eq!(o.kind, LockKind::BuyAllYes);
    assert_eq!(o.payout_timing, PayoutTiming::AtResolution, "a YES lock is paid at resolution");
    assert_eq!(o.outcomes, 3);
    // size 100 (min best-ask depth) × (1 − 0.88) per-share edge = 12.0
    assert!((o.net_profit - 12.0).abs() < 1e-6, "net={}", o.net_profit);
    assert!((o.size - 100.0).abs() < 1e-9);
}

#[test]
fn detect_flags_a_no_underpriced_set_and_carries_the_venues_payout_timing() {
    // NO asks 0.60/0.60/0.55 = 1.75 < 2 → NO lock. YES asks sum > 1 → no YES lock.
    let bk = books(&[
        ("yes0", ask_book(0.55, 100.0)),
        ("yes1", ask_book(0.55, 100.0)),
        ("yes2", ask_book(0.55, 100.0)), // Σ YES = 1.65 > 1 → no YES lock
        ("no0", ask_book(0.60, 80.0)),
        ("no1", ask_book(0.60, 80.0)),
        ("no2", ask_book(0.55, 80.0)),
    ]);
    let converting = detect(&three(), |id| bk.get(id), &free_top_of_book());
    assert_eq!(converting.len(), 1, "only the NO lock: {converting:?}");
    let o = &converting[0];
    assert_eq!(o.kind, LockKind::BuyAllNo);
    assert_eq!(
        o.payout_timing,
        PayoutTiming::Immediately,
        "a venue that converts frees the capital at once"
    );
    assert!((o.payout - 2.0 * 80.0).abs() < 1e-9, "payout (N−1)*size");
    // gross = 80 * 1.75 = 140 ; net = 160 − 140 = 20
    assert!((o.net_profit - 20.0).abs() < 1e-6, "net={}", o.net_profit);

    // A venue that cannot convert pays the same N−1 at resolution: the numbers do not move, the timing does.
    let at_resolution =
        OutcomeSetConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook, PayoutTiming::AtResolution);
    let waiting = detect(&three(), |id| bk.get(id), &at_resolution);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].payout_timing, PayoutTiming::AtResolution);
    assert_eq!(waiting[0].net_profit.to_bits(), o.net_profit.to_bits());
    assert_eq!(waiting[0].payout.to_bits(), o.payout.to_bits());
}

#[test]
fn detect_does_not_flag_a_consistent_set() {
    // Both sides carry the spread: Σ YES asks = 1.06 > 1, Σ NO asks = 2.06 > 2. No lock either way.
    let bk = books(&[
        ("yes0", ask_book(0.52, 100.0)),
        ("yes1", ask_book(0.32, 100.0)),
        ("yes2", ask_book(0.22, 100.0)),
        ("no0", ask_book(0.52, 100.0)),
        ("no1", ask_book(0.72, 100.0)),
        ("no2", ask_book(0.82, 100.0)),
    ]);
    assert!(
        detect(&three(), |id| bk.get(id), &free_top_of_book()).is_empty(),
        "consistent set → no arb"
    );
}

#[test]
fn detect_skips_a_side_whose_leg_book_is_missing() {
    // YES side underpriced, but yes2's book is not seeded → YES lock unavailable this tick.
    let bk = books(&[
        ("yes0", ask_book(0.30, 100.0)),
        ("yes1", ask_book(0.30, 100.0)),
        // no yes2 entry
        ("no0", ask_book(0.60, 100.0)),
        ("no1", ask_book(0.60, 100.0)),
        ("no2", ask_book(0.55, 100.0)), // NO side IS fully seeded and underpriced → 1 lock
    ]);
    let locks = detect(&three(), |id| bk.get(id), &free_top_of_book());
    assert_eq!(locks.len(), 1, "only the fully-seeded NO side is emitted: {locks:?}");
    assert_eq!(locks[0].kind, LockKind::BuyAllNo);
}

#[test]
fn detect_refuses_a_one_outcome_set_and_a_side_with_a_token_missing() {
    // A 1-outcome "set" is degenerate: (N−1)=0 payout, and it is not a real outcome set.
    let bk = books(&[("yes0", ask_book(0.20, 100.0)), ("no0", ask_book(0.20, 100.0))]);
    let one = OutcomeSet { outcomes: 1, yes_tokens: &["yes0"], no_tokens: &["no0"] };
    assert!(detect(&one, |id| bk.get(id), &free_top_of_book()).is_empty(), "N<2 is refused");

    // Three outcomes but only two YES tokens: an outcome without a token cannot be bought, so that side is
    // unavailable, however cheap the legs it does have.
    let cheap = books(&[
        ("yes0", ask_book(0.05, 100.0)),
        ("yes2", ask_book(0.05, 100.0)),
        ("no0", ask_book(0.72, 100.0)),
        ("no1", ask_book(0.72, 100.0)),
        ("no2", ask_book(0.74, 100.0)),
    ]);
    let short = OutcomeSet { outcomes: 3, yes_tokens: &["yes0", "yes2"], no_tokens: &NO };
    assert!(detect(&short, |id| cheap.get(id), &free_top_of_book()).is_empty());
}

#[test]
fn detect_can_flag_both_sides_at_once() {
    // A genuinely mispriced set where BOTH Σ YES < 1 and Σ NO < N−1 (a wide two-sided edge).
    let bk = books(&[
        ("yes0", ask_book(0.30, 100.0)),
        ("yes1", ask_book(0.25, 100.0)),
        ("yes2", ask_book(0.20, 100.0)), // Σ YES = 0.75 < 1
        ("no0", ask_book(0.55, 100.0)),
        ("no1", ask_book(0.55, 100.0)),
        ("no2", ask_book(0.55, 100.0)), // Σ NO = 1.65 < 2
    ]);
    let locks = detect(&three(), |id| bk.get(id), &free_top_of_book());
    assert_eq!(locks.len(), 2, "both locks: {locks:?}");
    assert!(locks.iter().any(|o| o.kind == LockKind::BuyAllYes));
    assert!(locks.iter().any(|o| o.kind == LockKind::BuyAllNo));
}

// ---- cached_marks: the screen on the venue's cached marks ----

/// The numbers the bridge's catalog tests asserted: a complete 3-way set priced 0.50 + 0.30 + 0.15 = 0.95 is a 0.05
/// gross buy-side edge; 2c a leg over 3 legs eats 6c, so the net edge is negative; a fairly priced set (0.60 + 0.40)
/// shows none.
#[test]
fn the_cached_marks_edge_by_hand() {
    let gross = buy_all_yes_edge(0.50 + 0.30 + 0.15, 3, 0.0);
    assert!((gross - 0.05).abs() < 1e-12, "gross={gross}");
    let net = buy_all_yes_edge(0.50 + 0.30 + 0.15, 3, 0.02);
    assert!((net - (0.05 - 0.06)).abs() < 1e-12 && net < 0.0, "net={net}");
    assert!(buy_all_yes_edge(0.60 + 0.40, 2, 0.0).abs() < 1e-12);
}
