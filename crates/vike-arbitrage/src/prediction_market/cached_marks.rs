//! The screen on a venue's CACHED marks: the buy-all-YES edge of a complete outcome set from the sum of its YES prices.
//!
//! Over a complete set the YES prices must sum to 1 (exactly one outcome resolves YES, paying `$1`). A sum meaningfully
//! BELOW 1 is the buy-side arbitrage: buy every YES leg for less than the `$1` the winner certainly pays. The edge here
//! is NET of a caller-supplied per-leg cost, so the number compared against zero is the one that matters.
//!
//! ⚠ These are the venue's cached marks, not the live top of book, and they ignore depth entirely: a positive number
//! is a SCREEN, never a trade signal. Executing needs the live book of every leg ([`super::outcome_set_lock`]).
//!
//! ⚠ **The sum must be over a COMPLETE set**: a partial set's sum is a guaranteed phantom edge (see [`super`]). The
//! venue's catalog proves completeness and refuses before it hands a sum in; this function takes a number and
//! cannot tell.

/// The NET buy-side edge `1 − Σ P(YES_i) − cost_per_leg · N` of a complete set whose YES prices sum to `yes_price_sum`
/// over `outcomes` outcomes. Positive means buying one YES of every outcome costs less than the `$1` the winner
/// pays, after `cost_per_leg` (fee plus expected slippage, in price units, per leg; pass `0.0` for the gross edge).
pub fn buy_all_yes_edge(yes_price_sum: f64, outcomes: usize, cost_per_leg: f64) -> f64 {
    1.0 - yes_price_sum - cost_per_leg * outcomes as f64
}
