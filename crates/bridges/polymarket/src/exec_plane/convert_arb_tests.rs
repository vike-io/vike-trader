use super::*;
use crate::neg_risk_set::NegRiskMember;
use std::collections::HashMap;

const MID: &str = "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500";
/// The crypto up/down p(1−p) taker rate (0.072) — a fee-bearing curve for the netting tests.
const CRYPTO_CURVE: FeeSchedule =
    FeeSchedule::ProbabilityScaled { taker_rate: 0.072, maker_rate: 0.0, maker_rebate_share: 0.0 };

/// A book with a single ask level `(price, qty)` (and no bids — we only buy).
fn ask_book(price: f64, qty: f64) -> L2Book {
    let mut b = L2Book::new(0.01);
    b.apply_snapshot(1, &[], &[BookLevel::new(price, qty)]);
    b
}

/// A book with multiple ask levels (best first in the slice; order does not matter to the book).
fn ask_book_levels(levels: &[BookLevel]) -> L2Book {
    let mut b = L2Book::new(0.01);
    b.apply_snapshot(1, &[], levels);
    b
}

/// A minimal [`NegRiskMember`] carrying an index, both token ids and a (Gamma) yes price so the
/// set reads [`crate::neg_risk_set::SetCompleteness::Complete`].
fn member(index: u32, yes_price: f64) -> NegRiskMember {
    NegRiskMember {
        index: Some(index),
        title: format!("outcome{index}"),
        condition_id: format!("0xcond{index}"),
        question_id: String::new(),
        yes_token_id: Some(format!("yes{index}")),
        no_token_id: Some(format!("no{index}")),
        yes_price: Some(yes_price),
        active: true,
        closed: false,
    }
}

/// A complete `n`-member set (indices `0..n`), Gamma yes prices `yes_prices`.
fn set_of(n: usize, yes_prices: &[f64]) -> NegRiskSet {
    let members = (0..n).map(|i| member(i as u32, yes_prices[i])).collect();
    NegRiskSet {
        market_id: MID.to_string(),
        event_id: String::new(),
        event_slug: String::new(),
        event_title: String::new(),
        members,
    }
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
    let cfg = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    let lock = evaluate_lock(&legs, 1.0, &cfg).expect("net-positive YES lock");
    assert!((lock.size - 40.0).abs() < 1e-9, "sizes to the thinnest best-ask depth");
    // gross cost = 40 * 0.88 = 35.2 ; payout = 40 ; net = 4.8
    assert!((lock.gross_cost - 35.2).abs() < 1e-6, "gross={}", lock.gross_cost);
    assert_eq!(lock.fee, 0.0, "Free curve charges nothing");
    assert!((lock.payout - 40.0).abs() < 1e-9);
    assert!((lock.net_profit - 4.8).abs() < 1e-6, "net={}", lock.net_profit);

    // with the crypto p(1−p) curve the fee is subtracted: Σ 40*0.072*p*(1−p) over the three legs.
    let cfg_fee = ConvertArbConfig::new(CRYPTO_CURVE, SizePolicy::TopOfBook);
    let locked = evaluate_lock(&legs, 1.0, &cfg_fee).expect("still net-positive");
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
    let cfg = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    let lock = evaluate_lock(&legs, 2.0, &cfg).expect("net-positive NO lock");
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
    let cfg = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    assert!(evaluate_lock(&legs, 1.0, &cfg).is_none(), "Σ YES asks 1.06 > 1 → no lock");
}

#[test]
fn a_fee_can_flip_a_thin_gross_edge_to_no_lock() {
    // Σ YES asks = 0.33+0.33+0.33 = 0.99 < 1 → a thin 0.01/share gross edge.
    let a = ask_book(0.33, 100.0);
    let b = ask_book(0.33, 100.0);
    let c = ask_book(0.33, 100.0);
    let legs = [&a, &b, &c];
    // gross: net-positive
    let free = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    assert!(evaluate_lock(&legs, 1.0, &free).is_some(), "gross edge exists");
    // fee: 3 * 100 * 0.072 * 0.33 * 0.67 ≈ 4.77/100sh > the 1.0 gross edge → net negative → None
    let fee = ConvertArbConfig::new(CRYPTO_CURVE, SizePolicy::TopOfBook);
    assert!(evaluate_lock(&legs, 1.0, &fee).is_none(), "the p(1−p) fee eats the thin edge");
}

#[test]
fn fixed_size_walks_the_book_and_pays_slippage() {
    // Two legs at 0.30 (deep) and one leg 40@0.30 then 100@0.34. Buying 100 walks the third leg.
    let a = ask_book(0.30, 200.0);
    let b = ask_book(0.30, 200.0);
    let c = ask_book_levels(&[BookLevel::new(0.30, 40.0), BookLevel::new(0.34, 100.0)]);
    let legs = [&a, &b, &c];
    let cfg_free = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    // TopOfBook sizes to the thin 40 @ 0.30 → no slippage, Σ vwap 0.90.
    let top = evaluate_lock(&legs, 1.0, &cfg_free).expect("top lock");
    assert!((top.size - 40.0).abs() < 1e-9);
    assert!((top.gross_cost - 40.0 * 0.90).abs() < 1e-6, "gross={}", top.gross_cost);

    // Fixed 100 walks leg c: 40@0.30 + 60@0.34 → vwap_c = (40*0.30+60*0.34)/100 = 0.324.
    let cfg100 = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::Fixed(100.0));
    let walked = evaluate_lock(&legs, 1.0, &cfg100).expect("100-lot lock");
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
    let cfg = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::Fixed(80.0));
    // 80 > 50 on leg c: the set cannot be completed at 80 shares → no lock, no partial phantom.
    assert!(evaluate_lock(&legs, 1.0, &cfg).is_none());
    // …but 50 (exactly leg c's depth) IS lockable.
    let ok = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::Fixed(50.0));
    assert!(evaluate_lock(&legs, 1.0, &ok).is_some());
}

#[test]
fn evaluate_lock_degenerate_inputs() {
    let a = ask_book(0.30, 10.0);
    let top = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    let zero = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::Fixed(0.0));
    let nan = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::Fixed(f64::NAN));
    assert!(evaluate_lock(&[], 1.0, &top).is_none(), "empty legs");
    // non-positive / NaN fixed size
    assert!(evaluate_lock(&[&a], 1.0, &zero).is_none(), "zero size");
    assert!(evaluate_lock(&[&a], 1.0, &nan).is_none(), "NaN size");
    // a leg with no ask under TopOfBook
    let empty = L2Book::new(0.01);
    assert!(evaluate_lock(&[&a, &empty], 1.0, &top).is_none(), "a leg with no ask");
}

// ---- full_index_set ----

#[test]
fn full_index_set_matches_the_encoder_helper() {
    for n in [1usize, 2, 3, 7, 32, 127, 128] {
        let indices: Vec<u32> = (0..n as u32).collect();
        assert_eq!(
            full_index_set(n),
            crate::exec_plane::settlement::split_merge::index_set_from_indices(&indices).ok(),
            "n={n}"
        );
    }
    assert_eq!(full_index_set(0), None);
    assert_eq!(full_index_set(129), None, "beyond the 128-bit index space");
    assert_eq!(full_index_set(3), Some(0b111));
}

// ---- detect: the full set-level integration ----

/// Build the token→book lookup a `detect` call closes over.
fn books(pairs: &[(&str, L2Book)]) -> HashMap<String, L2Book> {
    pairs.iter().map(|(id, b)| (id.to_string(), b.clone())).collect()
}

#[test]
fn detect_flags_a_yes_underpriced_set() {
    let set = set_of(3, &[0.50, 0.30, 0.15]); // Complete, contiguous, priced
    // YES asks 0.30/0.30/0.28 = 0.88 < 1 → YES lock. NO asks priced consistently (no NO lock).
    let bk = books(&[
        ("yes0", ask_book(0.30, 100.0)),
        ("yes1", ask_book(0.30, 100.0)),
        ("yes2", ask_book(0.28, 100.0)),
        ("no0", ask_book(0.72, 100.0)),
        ("no1", ask_book(0.72, 100.0)),
        ("no2", ask_book(0.74, 100.0)), // Σ NO = 2.18 > 2 → no NO lock
    ]);
    let cfg = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    let opps = detect(&set, |id| bk.get(id), &cfg);
    assert_eq!(opps.len(), 1, "only the YES lock: {opps:?}");
    let o = &opps[0];
    assert_eq!(o.kind, LockKind::BuyAllYes);
    assert_eq!(o.outcomes, 3);
    assert_eq!(o.market_id, MID);
    assert_eq!(o.convert_index_set, None, "YES lock needs no convert");
    // size 100 (min best-ask depth) × (1 − 0.88) per-share edge = 12.0
    assert!((o.net_profit - 12.0).abs() < 1e-6, "net={}", o.net_profit);
    assert!((o.size - 100.0).abs() < 1e-9);
}

#[test]
fn detect_flags_a_no_underpriced_set_with_the_convert_index_set() {
    let set = set_of(3, &[0.50, 0.30, 0.15]);
    // NO asks 0.60/0.60/0.55 = 1.75 < 2 → NO+convert lock. YES asks sum > 1 → no YES lock.
    let bk = books(&[
        ("yes0", ask_book(0.55, 100.0)),
        ("yes1", ask_book(0.55, 100.0)),
        ("yes2", ask_book(0.55, 100.0)), // Σ YES = 1.65 > 1 → no YES lock
        ("no0", ask_book(0.60, 80.0)),
        ("no1", ask_book(0.60, 80.0)),
        ("no2", ask_book(0.55, 80.0)),
    ]);
    let cfg = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    let opps = detect(&set, |id| bk.get(id), &cfg);
    assert_eq!(opps.len(), 1, "only the NO+convert lock: {opps:?}");
    let o = &opps[0];
    assert_eq!(o.kind, LockKind::BuyAllNoConvert);
    assert_eq!(o.convert_index_set, Some(0b111), "burn all 3 NO legs");
    assert!((o.payout - 2.0 * 80.0).abs() < 1e-9, "payout (N−1)*size");
    // gross = 80 * 1.75 = 140 ; net = 160 − 140 = 20
    assert!((o.net_profit - 20.0).abs() < 1e-6, "net={}", o.net_profit);
}

#[test]
fn detect_does_not_flag_a_consistent_set() {
    let set = set_of(3, &[0.50, 0.30, 0.20]);
    // Both sides carry the spread: Σ YES asks = 1.06 > 1, Σ NO asks = 2.06 > 2. No lock either way.
    let bk = books(&[
        ("yes0", ask_book(0.52, 100.0)),
        ("yes1", ask_book(0.32, 100.0)),
        ("yes2", ask_book(0.22, 100.0)),
        ("no0", ask_book(0.52, 100.0)),
        ("no1", ask_book(0.72, 100.0)),
        ("no2", ask_book(0.82, 100.0)),
    ]);
    let cfg = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    assert!(detect(&set, |id| bk.get(id), &cfg).is_empty(), "consistent set → no arb");
}

#[test]
fn detect_refuses_an_incomplete_set() {
    // Drop index 1 → a gap → SetCompleteness::IndexGap → never evaluated, however cheap the legs.
    let mut set = set_of(3, &[0.50, 0.30, 0.15]);
    set.members.retain(|m| m.index != Some(1));
    assert_eq!(set.len(), 2);
    let bk = books(&[
        ("yes0", ask_book(0.05, 100.0)), // absurdly cheap — a phantom arb if it were evaluated
        ("yes2", ask_book(0.05, 100.0)),
        ("no0", ask_book(0.05, 100.0)),
        ("no2", ask_book(0.05, 100.0)),
    ]);
    let cfg = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    assert!(detect(&set, |id| bk.get(id), &cfg).is_empty(), "incomplete set is refused");
}

#[test]
fn detect_skips_a_side_whose_leg_book_is_missing() {
    let set = set_of(3, &[0.50, 0.30, 0.15]);
    // YES side underpriced, but yes2's book is not seeded → YES lock unavailable this tick.
    let bk = books(&[
        ("yes0", ask_book(0.30, 100.0)),
        ("yes1", ask_book(0.30, 100.0)),
        // no yes2 entry
        ("no0", ask_book(0.60, 100.0)),
        ("no1", ask_book(0.60, 100.0)),
        ("no2", ask_book(0.55, 100.0)), // NO side IS fully seeded and underpriced → 1 lock
    ]);
    let cfg = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    let opps = detect(&set, |id| bk.get(id), &cfg);
    assert_eq!(opps.len(), 1, "only the fully-seeded NO side is emitted: {opps:?}");
    assert_eq!(opps[0].kind, LockKind::BuyAllNoConvert);
}

#[test]
fn detect_refuses_a_one_member_set() {
    // A 1-outcome "set" is degenerate: (N−1)=0 payout, and it is not a real neg-risk group.
    let set = set_of(1, &[0.20]);
    let bk = books(&[("yes0", ask_book(0.20, 100.0)), ("no0", ask_book(0.20, 100.0))]);
    let cfg = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    assert!(detect(&set, |id| bk.get(id), &cfg).is_empty(), "N<2 is refused");
}

#[test]
fn detect_can_flag_both_sides_at_once() {
    // A genuinely mispriced set where BOTH Σ YES < 1 and Σ NO < N−1 (a wide two-sided edge).
    let set = set_of(3, &[0.40, 0.30, 0.20]);
    let bk = books(&[
        ("yes0", ask_book(0.30, 100.0)),
        ("yes1", ask_book(0.25, 100.0)),
        ("yes2", ask_book(0.20, 100.0)), // Σ YES = 0.75 < 1
        ("no0", ask_book(0.55, 100.0)),
        ("no1", ask_book(0.55, 100.0)),
        ("no2", ask_book(0.55, 100.0)), // Σ NO = 1.65 < 2
    ]);
    let cfg = ConvertArbConfig::new(FeeSchedule::Free, SizePolicy::TopOfBook);
    let opps = detect(&set, |id| bk.get(id), &cfg);
    assert_eq!(opps.len(), 2, "both locks: {opps:?}");
    assert!(opps.iter().any(|o| o.kind == LockKind::BuyAllYes));
    assert!(opps.iter().any(|o| o.kind == LockKind::BuyAllNoConvert));
}
