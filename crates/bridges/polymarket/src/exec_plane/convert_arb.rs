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
//!   immediately as `(N−1)·amount` USDC (see [`crate::exec_plane::settlement::split_merge`]'s module doc). Locked iff
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
//! in [`crate::exec_plane::settlement::split_merge`] (whose live round-trip is itself still OWED). Nothing in this crate calls
//! [`detect`], so a default build is byte-identical — and the whole crate is behind the `polymarket`
//! feature, so a no-feature build never compiles it at all.

use vike_model::BookLevel;
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
    /// straight to [`crate::exec_plane::settlement::split_merge::convert_positions_calldata`]. `None` for buy-all-YES (no
    /// convert), or when `N` exceeds the 128-bit index space the encoder can express.
    pub convert_index_set: Option<u128>,
}

/// Σ of the best ASK across `legs` (top of book) — the raw gross screen the brief names
/// (`Σ best YES asks`, `Σ best NO asks`). `None` if any leg has no ask quoted (an unpriceable set
/// side). Naive left-to-right fold in the given order.
pub fn sum_best_asks(legs: &[&L2Book]) -> Option<f64> {
    let mut sum = 0.0;
    for b in legs {
        let BookLevel { price: ask, .. } = b.best_ask()?;
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
                let BookLevel { qty, .. } = b.best_ask()?;
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
/// [`crate::exec_plane::settlement::split_merge::index_set_from_indices`]`(&(0..n))`. `None` when `n` exceeds the 128-bit
/// index space [`crate::exec_plane::settlement::split_merge::convert_positions_calldata`] can encode (no live neg-risk set is
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
    if yes_ids.len() == n
        && let Some(legs) = yes_ids.iter().map(|&id| book_for(id)).collect::<Option<Vec<_>>>()
        && let Some(lock) = evaluate_lock(&legs, 1.0, cfg)
    {
        out.push(opportunity(set, LockKind::BuyAllYes, lock, None));
    }

    // BUY-ALL-NO + CONVERT: buy one NO of every outcome, convert all N → `(N−1)·size` USDC.
    let no_ids = set.no_token_ids();
    if no_ids.len() == n
        && let Some(legs) = no_ids.iter().map(|&id| book_for(id)).collect::<Option<Vec<_>>>()
        && let Some(lock) = evaluate_lock(&legs, (n - 1) as f64, cfg)
    {
        out.push(opportunity(set, LockKind::BuyAllNoConvert, lock, full_index_set(n)));
    }

    out
}

#[path = "convert_arb_tests.rs"]
#[cfg(test)]
mod convert_arb_tests;
