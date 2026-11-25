//! `convert_arb` — Polymarket's WIRING of the venue-neutral outcome-set lock to a neg-risk set. The maths is not
//! here: the two locks (BUY-ALL-YES and BUY-ALL-NO), each leg's L2 walk for size and VWAP, and the p(1−p) fee netted
//! through `vike_model::FeeSchedule` are `vike_arbitrage::prediction_market::outcome_set_lock`'s, which any prediction
//! market's bridge can call; a bridge is a connection and holds no arbitrage logic. What stays here is what is
//! Polymarket's:
//!
//! - **The catalog.** The set is a [`NegRiskSet`] (Gamma parsing, the `negRiskMarketID`, the token ids). Its
//!   COMPLETENESS is load-bearing and proved HERE, before the maths is called: evaluating an INCOMPLETE set is a
//!   phantom-arb generator (dropping a member can only lower a Σ; see [`crate::catalog::neg_risk_set`]'s module doc,
//!   the live 96¢-phantom Ethiopia example). So [`detect`] REFUSES any set that is not
//!   [`crate::catalog::neg_risk_set::SetCompleteness::Complete`]; the maths then refuses a set with fewer than two
//!   outcomes and a side whose token universe is not one token per outcome.
//! - **The convert.** Polymarket can CONVERT a full NO set on the spot: `convertPositions` over every outcome pays
//!   `(N−1)·amount` collateral (see [`crate::settlement::split_merge`]'s module doc), so the lock is configured with
//!   `PayoutTiming::Immediately` for the NO side, and a NO lock carries the `_indexSet` bitmap that burns all `N` legs
//!   ([`full_index_set`]) for [`crate::settlement::split_merge::convert_positions_calldata`]. The calldata itself
//!   stays in `settlement::split_merge`.
//! - **The fee.** The rate is PER-MARKET and the caller's: neg-risk politics is commonly `0`, sports `0.05`
//!   ([`vike_venues::POLYMARKET_V2_FEE_CURVE`]), crypto up/down `0.072`. A nonzero on-chain `convertPositions` fee
//!   (usually `0` on neg-risk markets) is NOT modelled, stated and not silently swallowed.
//!
//! READ-ONLY / NO AUTO-EXECUTION. This emits sized, net opportunities; nothing here signs, sizes down, or sends.
//! Turning an opportunity into a trade is the caller's job, and the `convertPositions` calldata plus the gasless-relayer
//! submit it would need are the PLUMBING-ONLY encoders in [`crate::settlement::split_merge`] (whose live round-trip is
//! itself still OWED). Nothing in this crate calls [`detect`], so a default build is byte-identical, and the whole
//! module is behind the `polymarket` feature, so a no-feature build never compiles it at all.

use vike_arbitrage::prediction_market::outcome_set_lock::{
    self, LockKind, OutcomeSet, OutcomeSetConfig, OutcomeSetLock, PayoutTiming, SizePolicy,
};
use vike_marketdata::L2Book;
use vike_model::FeeSchedule;

use crate::catalog::neg_risk_set::NegRiskSet;

/// A sized, net-of-fee lock over one neg-risk set: the venue-neutral [`OutcomeSetLock`] plus the Polymarket context a
/// caller needs to act (or log). READ-ONLY: emitting one triggers nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct ConvertArbOpportunity {
    /// The set's `negRiskMarketID`: the `_marketId` a `convertPositions` for the NO leg would take.
    pub market_id: String,
    /// The lock: its kind, size, costs, payout and locked profit (always `> 0`).
    pub lock: OutcomeSetLock,
    /// For [`LockKind::BuyAllNo`], the `_indexSet` bitmap that burns all `N` NO legs, handed straight to
    /// [`crate::settlement::split_merge::convert_positions_calldata`]. `None` for buy-all-YES (no convert), or when
    /// `N` exceeds the 128-bit index space the encoder can express.
    pub convert_index_set: Option<u128>,
}

/// The convert `_indexSet` that burns ALL `n` NO legs — bits `0..n` set — matching
/// [`crate::settlement::split_merge::index_set_from_indices`]`(&(0..n))`. `None` when `n` exceeds the 128-bit
/// index space [`crate::settlement::split_merge::convert_positions_calldata`] can encode (no live neg-risk set is
/// anywhere near 128 outcomes).
fn full_index_set(n: usize) -> Option<u128> {
    match n {
        1..=127 => Some((1u128 << n) - 1),
        128 => Some(u128::MAX),
        _ => None, // 0 (no set) or > 128 (unencodable)
    }
}

/// Detect the net-positive locks on `set`, reading each leg's live book via `book_for(token_id)`, with the per-leg
/// taker `fee` and the `size` policy. Returns the opportunities found (0, 1, or 2: the YES and NO locks are
/// independent). Emits NOTHING for an incomplete set (the phantom-arb guard, see the module doc), and nothing for
/// what `vike_arbitrage::prediction_market::outcome_set_lock::detect` refuses (fewer than 2 outcomes, a side with a
/// token missing, a leg whose book is missing, a lock whose netted edge is not `> 0`).
///
/// `book_for` is a lookup into whatever the caller holds the live books in (the feed's per-token
/// `TokenState::book`, a snapshot map, …); the returned reference's lifetime `'a` ties the gathered legs to it.
pub fn detect<'a>(
    set: &NegRiskSet,
    book_for: impl Fn(&str) -> Option<&'a L2Book>,
    fee: FeeSchedule,
    size: SizePolicy,
) -> Vec<ConvertArbOpportunity> {
    if !set.completeness().is_complete() {
        return Vec::new();
    }
    let (yes_tokens, no_tokens) = (set.yes_token_ids(), set.no_token_ids());
    let outcomes =
        OutcomeSet { outcomes: set.len(), yes_tokens: &yes_tokens, no_tokens: &no_tokens };
    // Polymarket converts a full NO set on the spot (`convertPositions`), so its NO lock pays immediately.
    let config = OutcomeSetConfig::new(fee, size, PayoutTiming::Immediately);
    outcome_set_lock::detect(&outcomes, book_for, &config)
        .into_iter()
        .map(|lock| ConvertArbOpportunity {
            market_id: set.market_id.clone(),
            convert_index_set: if lock.kind == LockKind::BuyAllNo {
                full_index_set(set.len())
            } else {
                None
            },
            lock,
        })
        .collect()
}

#[path = "convert_arb_tests.rs"]
#[cfg(test)]
mod convert_arb_tests;
