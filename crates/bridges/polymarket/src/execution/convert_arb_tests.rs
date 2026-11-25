//! The WIRING's tests: a neg-risk set's completeness gate, its token lists, and the convert `_indexSet`. The maths the
//! wiring calls (each leg's walk, the VWAP, the fee, the two locks) is tested where it lives, in
//! `crates/vike-arbitrage/tests/arbitrage/prediction_market.rs`.

use super::*;
use crate::catalog::neg_risk_set::NegRiskMember;
use std::collections::HashMap;
use vike_marketdata::L2BookLevel;

const MID: &str = "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500";

/// A book with a single ask level `(price, qty)` (and no bids — we only buy).
fn ask_book(price: f64, qty: f64) -> L2Book {
    let mut b = L2Book::new(0.01);
    b.apply_snapshot(1, &[], &[L2BookLevel::new(price, qty)]);
    b
}

/// A minimal [`NegRiskMember`] carrying an index, both token ids and a (Gamma) yes price so the
/// set reads [`crate::catalog::neg_risk_set::SetCompleteness::Complete`].
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

// ---- full_index_set ----

#[test]
fn full_index_set_matches_the_encoder_helper() {
    for n in [1usize, 2, 3, 7, 32, 127, 128] {
        let indices: Vec<u32> = (0..n as u32).collect();
        assert_eq!(
            full_index_set(n),
            crate::settlement::split_merge::index_set_from_indices(&indices).ok(),
            "n={n}"
        );
    }
    assert_eq!(full_index_set(0), None);
    assert_eq!(full_index_set(129), None, "beyond the 128-bit index space");
    assert_eq!(full_index_set(3), Some(0b111));
}

// ---- detect: the neg-risk wiring ----

/// Build the token→book lookup a `detect` call closes over.
fn books(pairs: &[(&str, L2Book)]) -> HashMap<String, L2Book> {
    pairs.iter().map(|(id, b)| (id.to_string(), b.clone())).collect()
}

#[test]
fn detect_flags_a_yes_underpriced_set_without_a_convert() {
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
    let opps = detect(&set, |id| bk.get(id), FeeSchedule::Free, SizePolicy::TopOfBook);
    assert_eq!(opps.len(), 1, "only the YES lock: {opps:?}");
    let o = &opps[0];
    assert_eq!(o.lock.kind, LockKind::BuyAllYes);
    assert_eq!(o.lock.outcomes, 3);
    assert_eq!(o.market_id, MID);
    assert_eq!(o.convert_index_set, None, "YES lock needs no convert");
    // size 100 (min best-ask depth) × (1 − 0.88) per-share edge = 12.0
    assert!((o.lock.net_profit - 12.0).abs() < 1e-6, "net={}", o.lock.net_profit);
    assert!((o.lock.size - 100.0).abs() < 1e-9);
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
    let opps = detect(&set, |id| bk.get(id), FeeSchedule::Free, SizePolicy::TopOfBook);
    assert_eq!(opps.len(), 1, "only the NO+convert lock: {opps:?}");
    let o = &opps[0];
    assert_eq!(o.lock.kind, LockKind::BuyAllNo);
    assert_eq!(o.lock.payout_timing, PayoutTiming::Immediately, "Polymarket converts at once");
    assert_eq!(o.convert_index_set, Some(0b111), "burn all 3 NO legs");
    assert!((o.lock.payout - 2.0 * 80.0).abs() < 1e-9, "payout (N−1)*size");
    // gross = 80 * 1.75 = 140 ; net = 160 − 140 = 20
    assert!((o.lock.net_profit - 20.0).abs() < 1e-6, "net={}", o.lock.net_profit);
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
    assert!(
        detect(&set, |id| bk.get(id), FeeSchedule::Free, SizePolicy::TopOfBook).is_empty(),
        "incomplete set is refused"
    );
}
