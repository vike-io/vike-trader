//! Executable spread pricing — what a multi-leg spread ACTUALLY costs to trade,
//! as opposed to its mid-price appearance.
//!
//! A pair/spread signal computed on mid prices is systematically optimistic: a
//! round trip crosses the bid-ask of EVERY leg, twice. This module is the
//! arithmetic that makes that cost explicit, so a strategy can refuse a trade
//! whose edge does not survive it. It is pure `f64` with no I/O and no internal
//! crate deps, and is a private child of the pairs strategy (`pairs.rs`), its one
//! consumer: it lived in vike-model until 2026-10-12, when nothing else called it
//! (`vike-mm`'s xEMM pricing keeps its own sanity check and never did).
//!
//! ## The sign rule
//!
//! For a spread `S = Σ ratioᵢ · priceᵢ`, BUYING the spread means buying every
//! positive-ratio leg (lifting its ASK) and selling every negative-ratio leg
//! (hitting its BID). Selling the spread mirrors it. A naive `bid_A − bid_B` /
//! `ask_A − ask_B` construction takes the same side on every leg; that
//! understates crossing cost and produces a CROSSED quote (bid > ask) whenever
//! the short leg is wider than the long leg.
//!
//! Prior art: a reference engine's spread-quote aggregator gets this convention right
//! while its synthetic-instrument type makes exactly that error. That engine is
//! LGPL-3.0 — the convention is reimplemented here from the rule (a fact, not
//! expression); no source text was copied.
//!
//! ## Why this exists
//!
//! Measured on crypto perpetuals, a pairs round trip decomposes as gross
//! `+0.15%`, trading cost `−0.12%`, funding `−0.18%` ⇒ net `−0.15%`. The
//! mean-reversion signal is real and it is small, so the cost terms decide the
//! outcome. [`spread_roundtrip_cost`] is the fixed per-round-trip half of that;
//! the time-proportional carry half is charged separately.

/// One leg of a spread, with its own book and taker fee.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SpreadLeg {
    /// Signed leg ratio: `+` = held long in the spread, `−` = held short.
    pub(super) ratio: f64,
    /// Best bid for this leg.
    pub(super) bid: f64,
    /// Best ask for this leg.
    pub(super) ask: f64,
    /// Taker fee as a fraction of notional, charged PER CROSSING (e.g. `0.0005`).
    pub(super) taker_fee: f64,
}

/// `true` when every leg has a finite, uncrossed book and the set is non-empty.
fn legs_are_sane(legs: &[SpreadLeg]) -> bool {
    !legs.is_empty()
        && legs.iter().all(|l| {
            l.ratio.is_finite()
                && l.bid.is_finite()
                && l.ask.is_finite()
                && l.taker_fee.is_finite()
                && l.ask >= l.bid
        })
}

/// The EXECUTABLE `(bid, ask)` of the spread: what you receive selling it and
/// what you pay buying it, crossing each leg on the correct side.
///
/// `None` when any leg's book is missing, non-finite or crossed — an unknowable
/// price, never a free one. A caller must treat `None` as "do not trade", the
/// same contract as [`vike_model::Broker::quote_vwap`].
pub(super) fn executable_spread(legs: &[SpreadLeg]) -> Option<(f64, f64)> {
    if !legs_are_sane(legs) {
        return None;
    }
    let mut bid = 0.0f64;
    let mut ask = 0.0f64;
    for l in legs {
        if l.ratio > 0.0 {
            ask += l.ratio * l.ask;
            bid += l.ratio * l.bid;
        } else {
            // A short leg is SOLD at the bid when opening and BOUGHT BACK at the
            // ask when closing, so its contribution flips sides. `ratio` is
            // negative here, so these terms subtract.
            ask += l.ratio * l.bid;
            bid += l.ratio * l.ask;
        }
    }
    Some((bid, ask))
}

/// Total cost of a full round trip in and out of the spread, in spread price
/// units: the executable width (every leg's bid-ask, ratio-weighted) plus taker
/// fees on TWO crossings per leg.
///
/// This is the number an entry edge must EXCEED, not merely approach. It is the
/// FIXED half of the cost; carry grows with holding time and is charged
/// separately, so a caller holding a position must add both.
pub(super) fn spread_roundtrip_cost(legs: &[SpreadLeg]) -> Option<f64> {
    let (bid, ask) = executable_spread(legs)?;
    let width = ask - bid;
    let mut fees = 0.0f64;
    for l in legs {
        let mid = (l.bid + l.ask) / 2.0;
        fees += 2.0 * l.ratio.abs() * mid * l.taker_fee;
    }
    Some(width + fees)
}

/// The gate: does `edge` (the expected convergence, in spread price units)
/// strictly exceed a full round trip? `false` whenever the cost is unknowable.
///
/// Note this charges CROSSINGS ONLY. A strategy that holds a position across
/// funding settlements must gate on crossings plus carry instead.
///
/// Test-only since it left vike-model: `pairs.rs` gates on [`spread_total_cost`], so no
/// production code calls this, and a production build would carry it as dead code.
#[cfg(test)]
pub(super) fn spread_edge_clears_cost(edge: f64, legs: &[SpreadLeg]) -> bool {
    match spread_roundtrip_cost(legs) {
        Some(c) => edge.is_finite() && edge > c,
        None => false,
    }
}

/// Time-proportional CARRY on an open spread: per-interval funding paid on each
/// leg's signed notional, summed over `intervals_held`.
///
/// A long leg PAYS positive funding and a short leg RECEIVES it, so a hedged
/// pair's net carry is the DIFFERENTIAL, not either leg's rate. `funding_rates`
/// is one per-interval rate per leg, index-aligned with `legs`.
///
/// **This term is why "hold longer to amortize the crossing cost" backfires.**
/// The crossing cost is fixed per round trip; carry grows linearly with the
/// hold, so the long hold that dilutes one maximizes the other. Measured on
/// crypto perpetuals, carry (`−0.18%`/trade) EXCEEDED trading cost
/// (`−0.12%`/trade) — a cost floor that omits it is not a cost floor. It is also
/// the term essentially every open-source pairs implementation leaves out.
///
/// `None` when the rate slice does not match the legs, or any input is
/// non-finite — unknowable, never free.
pub(super) fn spread_carry_cost(
    legs: &[SpreadLeg],
    funding_rates: &[f64],
    intervals_held: f64,
) -> Option<f64> {
    if !legs_are_sane(legs)
        || funding_rates.len() != legs.len()
        || !intervals_held.is_finite()
        || intervals_held < 0.0
        || funding_rates.iter().any(|r| !r.is_finite())
    {
        return None;
    }
    let mut carry = 0.0f64;
    for (l, rate) in legs.iter().zip(funding_rates.iter()) {
        let mid = (l.bid + l.ask) / 2.0;
        // `ratio` carries the sign: a long leg pays, a short leg receives.
        carry += l.ratio * mid * rate * intervals_held;
    }
    Some(carry)
}

/// The number an entry edge must actually beat: crossings PLUS carry.
///
/// Prefer this over [`spread_roundtrip_cost`] anywhere a position is held across
/// funding settlements. Gating on crossings alone reproduces the exact omission
/// that makes naive pairs backtests look profitable.
pub(super) fn spread_total_cost(
    legs: &[SpreadLeg],
    funding_rates: &[f64],
    intervals_held: f64,
) -> Option<f64> {
    Some(spread_roundtrip_cost(legs)? + spread_carry_cost(legs, funding_rates, intervals_held)?)
}

#[path = "spread_quote_tests.rs"]
#[cfg(test)]
mod spread_quote_tests;
