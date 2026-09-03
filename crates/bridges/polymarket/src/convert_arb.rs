//! `convert_arb` — a PURE sum-of-set consistency-arbitrage DETECTOR over the LIVE multi-outcome book
//! feed of a neg-risk set. The live-book twin of [`crate::neg_risk_set::NegRiskSet::arb_edge`] (which
//! screens Gamma's CACHED marks and ignores depth entirely): this one reads the real per-leg
//! [`vike_model::L2Book`] the market feed already streams per token, WALKS it for size, and nets the
//! Polymarket p(1−p) taker fee plus the walk-the-book slippage.
//!
//! ## The two locks (both from the neg-risk Σ invariant: exactly one outcome resolves YES = $1)
//! - **BUY-ALL-YES.** Buy one YES of every outcome; the certain winner pays $1/share, the losers $0.
//!   So `size` shares of each of the `N` YES legs pays exactly `size` USDC at resolution. Locked iff
//!   `Σ_i (YES ask_i) · size + fees  <  size`, i.e. per share `Σ_i (YES ask_i) + fee/size < 1`. No
//!   on-chain convert is involved — this leg is realized purely by holding to resolution.
//! - **BUY-ALL-NO + CONVERT.** Buy one NO of every outcome, then `convertPositions` all `N` NO legs
//!   (index set = every bit). Owning all `N` NOs is worth exactly `N−1` at resolution (`N−1`
//!   outcomes resolve NO = $1, the winner's NO resolves $0); `convertPositions` realizes that
//!   immediately as `(N−1)·amount` USDC (see [`crate::split_merge`]'s module doc). Locked iff
//!   `Σ_i (NO ask_i) · size + fees  <  (N−1) · size`, i.e. per share `Σ_i (NO ask_i) + fee/size < N−1`.
//!
//! These are exactly the two gross triggers the brief names: `Σ best YES asks < $1` and
//! `Σ best NO asks < N−1`. This module makes them SIZED and NET.
//!
//! ## Fee (the shared p(1−p) curve, reachable down-only)
//! The per-leg taker fee is [`vike_model::FeeSchedule::commission`]`(false, size, vwap)` — the same
//! `qty · rate · p·(1−p)` curve `vike-backtest`'s `fair_value::fee` uses, but taken from the SHARED
//! [`vike_model::FeeSchedule::ProbabilityScaled`] home a bridge crate can depend on (the backtest
//! crate is a sibling, not below us). The rate is PER-MARKET and rides in the [`ConvertArbConfig`]:
//! neg-risk politics is commonly `0`, sports `0.05` ([`vike_model::POLYMARKET_V2_FEE_CURVE`]), crypto
//! up/down `0.072`. The fee is charged at the leg's VWAP fill price; because `p·(1−p)` is concave,
//! `fee(VWAP)` is an UPPER bound on the exact per-level fee (Jensen), so the netted edge is
//! CONSERVATIVE — never a phantom arb from under-charging the fee. A nonzero on-chain `convertPositions`
//! fee (usually `0` on neg-risk markets) is NOT modelled here — stated, not silently swallowed.
//!
//! ## Completeness is LOAD-BEARING (inherited from `neg_risk_set`)
//! Evaluating an INCOMPLETE set is a phantom-arb generator — dropping a member can only lower a Σ, so
//! a partial set always looks like free money (see [`crate::neg_risk_set`]'s module doc, the live
//! 96¢-phantom Ethiopia example). So [`detect`] REFUSES any set that is not
//! [`crate::neg_risk_set::SetCompleteness::Complete`], and any set whose YES/NO token universe is not
//! fully populated (a leg with no CLOB token id is not tradeable end-to-end). A leg whose live book
//! is not yet seeded (or has no ask) simply makes that lock unavailable this tick, never a false one.
//!
//! Determinism: the per-leg Σ is a naive left-to-right fold in the set's own index order (the order
//! [`crate::neg_risk_set::NegRiskSet::yes_token_ids`]/`no_token_ids` return), so the result is
//! reproducible frame-to-frame.
//!
//! READ-ONLY / NO AUTO-EXECUTION. This module emits sized, net opportunities; nothing here signs,
//! sizes down, or sends. Turning an opportunity into a trade is the caller's job, and the
//! `convertPositions` calldata + gasless-relayer submit it would need are the PLUMBING-ONLY encoders
//! in [`crate::split_merge`] (whose live round-trip is itself still OWED). Nothing in this crate calls
//! [`detect`], so a default build is byte-identical — and the whole crate is behind the `polymarket`
//! feature, so a no-feature build never compiles it at all.

use vike_model::{FeeSchedule, L2Book};

use crate::neg_risk_set::NegRiskSet;

/// How [`detect`]/[`evaluate_lock`] sizes a candidate lock across the `N` legs. The same size is
/// bought on every leg (a lock needs one unit of each outcome), so the size is bounded by the
/// THINNEST leg.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SizePolicy {
    /// The largest size fillable at EVERY leg's best ask with no walking — the minimum best-ask
    /// displayed qty across the legs. VWAP then equals the best ask on every leg, so this sized edge
    /// carries ZERO slippage: the conservative "how much can I lock at the quoted top of book?".
    TopOfBook,
    /// Probe a fixed shares-per-leg size, WALKING each leg's ask book for its VWAP
    /// ([`L2Book::avg_px_for_quantity`]). A size past a leg's best-level depth genuinely walks and
    /// pays slippage; a size past a leg's TOTAL displayed ask depth is unlockable (that leg cannot
    /// complete the set), so the whole lock is refused at that size.
    Fixed(f64),
}

/// Inputs to the netting: the per-leg taker fee curve and the size policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConvertArbConfig {
    /// The per-leg taker fee — the p(1−p) curve. Use [`vike_model::POLYMARKET_V2_FEE_CURVE`], a
    /// custom [`FeeSchedule::ProbabilityScaled`] (e.g. `taker_rate: 0.072` for crypto up/down), or
    /// [`FeeSchedule::Free`] for a fee-less neg-risk market / the gross screen.
    pub fee: FeeSchedule,
    /// How to size the lock (see [`SizePolicy`]).
    pub size: SizePolicy,
}

impl ConvertArbConfig {
    /// Construct with an explicit fee curve + size policy.
    pub fn new(fee: FeeSchedule, size: SizePolicy) -> Self {
        ConvertArbConfig { fee, size }
    }
}

/// Which lock an [`ConvertArbOpportunity`] realizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockKind {
    /// Buy one YES of every outcome; the certain winner pays $1/share. No on-chain convert.
    BuyAllYes,
    /// Buy one NO of every outcome, then `convertPositions` all `N` legs → `(N−1)/share` USDC.
    BuyAllNoConvert,
}

/// The pure per-lock netting result — the numbers [`evaluate_lock`] produces for one side, before
/// any set/venue context is attached. All figures are in USDC (collateral), for the whole `size`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizedLock {
    /// Shares bought on EACH leg.
    pub size: f64,
    /// Σ_i (VWAP_i · size) over the legs — the pre-fee buy cost.
    pub gross_cost: f64,
    /// Σ_i taker fee — the p(1−p) fee at each leg's VWAP.
    pub fee: f64,
    /// What the lock pays at resolution/convert: `size` for buy-all-YES, `(N−1)·size` for
    /// buy-all-NO+convert.
    pub payout: f64,
    /// `payout − gross_cost − fee`. A [`SizedLock`] is only ever returned with `net_profit > 0`.
    pub net_profit: f64,
}

/// A sized, net-of-fee arbitrage opportunity over one neg-risk set — [`SizedLock`] plus the context a
/// caller needs to act (or log). READ-ONLY: emitting one triggers nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct ConvertArbOpportunity {
    /// The set's `negRiskMarketID` — the `_marketId` a `convertPositions` for the NO leg would take.
    pub market_id: String,
    /// Which lock this is.
    pub kind: LockKind,
    /// `N`, the number of mutually-exclusive outcomes in the set.
    pub outcomes: usize,
    /// Shares per leg.
    pub size: f64,
    /// Pre-fee buy cost (USDC).
    pub gross_cost: f64,
    /// Taker fee (USDC).
    pub fee: f64,
    /// Resolution/convert payout (USDC).
    pub payout: f64,
    /// Locked profit net of fee and walk cost (USDC); always `> 0`.
    pub net_profit: f64,
    /// For [`LockKind::BuyAllNoConvert`], the `_indexSet` bitmap that burns all `N` NO legs — hand
    /// straight to [`crate::split_merge::convert_positions_calldata`]. `None` for buy-all-YES (no
    /// convert), or when `N` exceeds the 128-bit index space the encoder can express.
    pub convert_index_set: Option<u128>,
}

/// Σ of the best ASK across `legs` (top of book) — the raw gross screen the brief names
/// (`Σ best YES asks`, `Σ best NO asks`). `None` if any leg has no ask quoted (an unpriceable set
/// side). Naive left-to-right fold in the given order.
pub fn sum_best_asks(legs: &[&L2Book]) -> Option<f64> {
    let mut sum = 0.0;
    for b in legs {
        let (ask, _) = b.best_ask()?;
        sum += ask;
    }
    Some(sum)
}

/// Evaluate ONE lock given the per-leg ASK books already gathered in the set's index order.
/// `payout_per_share` is `1.0` for buy-all-YES and `(N−1)` for buy-all-NO+convert. Returns the sized,
/// net lock ONLY when it is net-positive; `None` when the size cannot be filled on every leg (the set
/// cannot be completed at that size) or the netted edge is not `> 0`.
///
/// Pure: walks each leg's asks with [`L2Book::avg_px_for_quantity`] (side `+1` = buy consumes asks),
/// and charges the p(1−p) fee at each leg's VWAP via [`FeeSchedule::commission`].
pub fn evaluate_lock(
    legs: &[&L2Book],
    payout_per_share: f64,
    cfg: &ConvertArbConfig,
) -> Option<SizedLock> {
    if legs.is_empty() {
        return None;
    }
    let size = match cfg.size {
        SizePolicy::Fixed(q) => {
            if q.is_nan() || q <= 0.0 {
                return None; // non-positive / NaN size is not a lock
            }
            q
        }
        SizePolicy::TopOfBook => {
            // The thinnest leg's best-ask qty. Any leg with no ask ⇒ this side is unpriceable.
            let mut min_qty = f64::INFINITY;
            for b in legs {
                let (_, qty) = b.best_ask()?;
                if qty < min_qty {
                    min_qty = qty;
                }
            }
            if min_qty <= 0.0 {
                return None; // resting levels never carry 0 qty, but never lock a zero size
            }
            min_qty
        }
    };

    let mut gross_cost = 0.0;
    let mut fee = 0.0;
    for b in legs {
        // +1 buy walks asks low→high; None ⇒ displayed depth cannot cover `size` on this leg, so the
        // full set cannot be assembled at this size and there is no lock.
        let vwap = b.avg_px_for_quantity(1, size)?;
        gross_cost += vwap * size;
        fee += cfg.fee.commission(false, size, vwap);
    }
    let payout = payout_per_share * size;
    let net_profit = payout - gross_cost - fee;
    (net_profit > 0.0).then_some(SizedLock { size, gross_cost, fee, payout, net_profit })
}

/// The convert `_indexSet` that burns ALL `n` NO legs — bits `0..n` set — matching
/// [`crate::split_merge::index_set_from_indices`]`(&(0..n))`. `None` when `n` exceeds the 128-bit
/// index space [`crate::split_merge::convert_positions_calldata`] can encode (no live neg-risk set is
/// anywhere near 128 outcomes).
fn full_index_set(n: usize) -> Option<u128> {
    match n {
        1..=127 => Some((1u128 << n) - 1),
        128 => Some(u128::MAX),
        _ => None, // 0 (no set) or > 128 (unencodable)
    }
}

fn opportunity(
    set: &NegRiskSet,
    kind: LockKind,
    lock: SizedLock,
    convert_index_set: Option<u128>,
) -> ConvertArbOpportunity {
    ConvertArbOpportunity {
        market_id: set.market_id.clone(),
        kind,
        outcomes: set.len(),
        size: lock.size,
        gross_cost: lock.gross_cost,
        fee: lock.fee,
        payout: lock.payout,
        net_profit: lock.net_profit,
        convert_index_set,
    }
}

/// Detect the net-positive consistency-arb locks on `set`, reading each leg's live book via
/// `book_for(token_id)`. Returns the opportunities found (0, 1, or 2 — the YES and NO locks are
/// independent), most-actionable-first is not implied (they are distinct trades). Emits NOTHING for:
///
/// - a set that is not [`crate::neg_risk_set::SetCompleteness::Complete`] (the phantom-arb guard);
/// - a set with fewer than 2 outcomes (not a real neg-risk set — `(N−1)` payout would be ≤ 0);
/// - a side whose token universe is incomplete (a leg with no YES/NO CLOB token id);
/// - a side where any leg's book is missing (`book_for` returns `None`) — that lock is simply
///   unavailable this tick;
/// - a lock whose netted edge is not `> 0`.
///
/// `book_for` is a lookup into whatever the caller holds the live books in (the feed's per-token
/// `TokenState::book`, a snapshot map, …); the returned reference's lifetime `'a` ties the gathered
/// legs to it.
pub fn detect<'a>(
    set: &NegRiskSet,
    book_for: impl Fn(&str) -> Option<&'a L2Book>,
    cfg: &ConvertArbConfig,
) -> Vec<ConvertArbOpportunity> {
    let mut out = Vec::new();
    let n = set.len();
    if n < 2 || !set.completeness().is_complete() {
        return out;
    }

    // BUY-ALL-YES: buy one YES of every outcome; the certain winner pays $1/share → payout `size`.
    let yes_ids = set.yes_token_ids();
    if yes_ids.len() == n {
        if let Some(legs) = yes_ids.iter().map(|&id| book_for(id)).collect::<Option<Vec<_>>>() {
            if let Some(lock) = evaluate_lock(&legs, 1.0, cfg) {
                out.push(opportunity(set, LockKind::BuyAllYes, lock, None));
            }
        }
    }

    // BUY-ALL-NO + CONVERT: buy one NO of every outcome, convert all N → `(N−1)·size` USDC.
    let no_ids = set.no_token_ids();
    if no_ids.len() == n {
        if let Some(legs) = no_ids.iter().map(|&id| book_for(id)).collect::<Option<Vec<_>>>() {
            if let Some(lock) = evaluate_lock(&legs, (n - 1) as f64, cfg) {
                out.push(opportunity(set, LockKind::BuyAllNoConvert, lock, full_index_set(n)));
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neg_risk_set::NegRiskMember;
    use std::collections::HashMap;

    const MID: &str = "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500";
    /// The crypto up/down p(1−p) taker rate (0.072) — a fee-bearing curve for the netting tests.
    const CRYPTO_CURVE: FeeSchedule = FeeSchedule::ProbabilityScaled {
        taker_rate: 0.072,
        maker_rate: 0.0,
        maker_rebate_share: 0.0,
    };

    /// A book with a single ask level `(price, qty)` (and no bids — we only buy).
    fn ask_book(price: f64, qty: f64) -> L2Book {
        let mut b = L2Book::new(0.01);
        b.apply_snapshot(1, &[], &[(price, qty)]);
        b
    }

    /// A book with multiple ask levels (best first in the slice; order does not matter to the book).
    fn ask_book_levels(levels: &[(f64, f64)]) -> L2Book {
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
        let c = ask_book_levels(&[(0.30, 40.0), (0.34, 100.0)]);
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
        assert!(
            (walked.gross_cost - gross).abs() < 1e-6,
            "gross={} vs {}",
            walked.gross_cost,
            gross
        );
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
                crate::split_merge::index_set_from_indices(&indices).ok(),
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
}
