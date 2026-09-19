//! Executable spread pricing — what a multi-leg spread ACTUALLY costs to trade,
//! as opposed to its mid-price appearance.
//!
//! A pair/spread signal computed on mid prices is systematically optimistic: a
//! round trip crosses the bid-ask of EVERY leg, twice. This module is the
//! arithmetic that makes that cost explicit, so a strategy can refuse a trade
//! whose edge does not survive it. It is pure `f64` with no I/O and no internal
//! crate deps, and lives in vike-model because BOTH `vike-backtest` (the pairs
//! strategy) and `vike-mm` (`xemm.rs`) consume it — vike-model is their common
//! ancestor under the down-only layering rule.
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
//! Prior art: Nautilus's `SpreadQuoteAggregator` gets this convention right
//! while its `SyntheticInstrument` makes exactly that error. Nautilus is
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
pub struct SpreadLeg {
    /// Signed leg ratio: `+` = held long in the spread, `−` = held short.
    pub ratio: f64,
    /// Best bid for this leg.
    pub bid: f64,
    /// Best ask for this leg.
    pub ask: f64,
    /// Taker fee as a fraction of notional, charged PER CROSSING (e.g. `0.0005`).
    pub taker_fee: f64,
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
/// same contract as [`crate::strategy::Broker::quote_vwap`].
pub fn executable_spread(legs: &[SpreadLeg]) -> Option<(f64, f64)> {
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
pub fn spread_roundtrip_cost(legs: &[SpreadLeg]) -> Option<f64> {
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
pub fn spread_edge_clears_cost(edge: f64, legs: &[SpreadLeg]) -> bool {
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
pub fn spread_carry_cost(
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
pub fn spread_total_cost(
    legs: &[SpreadLeg],
    funding_rates: &[f64],
    intervals_held: f64,
) -> Option<f64> {
    Some(spread_roundtrip_cost(legs)? + spread_carry_cost(legs, funding_rates, intervals_held)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leg(ratio: f64, bid: f64, ask: f64, fee: f64) -> SpreadLeg {
        SpreadLeg { ratio, bid, ask, taker_fee: fee }
    }

    /// Buying the spread LIFTS the ask on a long leg and HITS the bid on a short
    /// leg. A naive `bid_A - bid_B` / `ask_A - ask_B` understates the cost.
    #[test]
    fn negative_ratio_leg_uses_the_opposite_side() {
        let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-1.0, 49.0, 51.0, 0.0)];
        let (bid, ask) = executable_spread(&legs).unwrap();
        // buy spread: pay ask_A(101), receive bid_B(49) -> 52
        assert_eq!(ask.to_bits(), 52.0_f64.to_bits());
        // sell spread: receive bid_A(99), pay ask_B(51) -> 48
        assert_eq!(bid.to_bits(), 48.0_f64.to_bits());
    }

    /// The property Nautilus's SyntheticInstrument violates: the executable
    /// spread is NEVER crossed, even when the short leg is much wider.
    #[test]
    fn spread_is_never_crossed_even_with_a_wide_short_leg() {
        let legs = [leg(1.0, 99.9, 100.1, 0.0), leg(-1.0, 40.0, 60.0, 0.0)];
        let (bid, ask) = executable_spread(&legs).unwrap();
        assert!(ask >= bid, "crossed spread: bid {bid} > ask {ask}");
    }

    /// A round trip crosses FOUR half-spreads: both legs' full widths.
    #[test]
    fn roundtrip_cost_is_both_legs_full_width_when_fees_are_zero() {
        let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-1.0, 49.0, 51.0, 0.0)];
        let cost = spread_roundtrip_cost(&legs).unwrap();
        assert_eq!(cost.to_bits(), 4.0_f64.to_bits()); // 2.0 + 2.0
    }

    /// Hedge ratio scales the short leg's contribution to both quote and cost.
    #[test]
    fn hedge_ratio_scales_the_short_leg() {
        let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-2.0, 49.0, 51.0, 0.0)];
        let cost = spread_roundtrip_cost(&legs).unwrap();
        assert_eq!(cost.to_bits(), 6.0_f64.to_bits()); // 2.0 + 2*2.0
    }

    /// Fees are charged on TWO crossings per leg (in and out), on |ratio|*mid.
    #[test]
    fn fees_are_charged_on_two_crossings_per_leg() {
        let legs = [leg(1.0, 100.0, 100.0, 0.001), leg(-1.0, 50.0, 50.0, 0.001)];
        let cost = spread_roundtrip_cost(&legs).unwrap();
        // width 0; fees = 2*(1*100*0.001) + 2*(1*50*0.001) = 0.2 + 0.1
        assert!((cost - 0.3).abs() < 1e-12, "cost {cost}");
    }

    /// A degenerate or crossed input book is UNKNOWABLE, not free.
    #[test]
    fn bad_book_is_none_not_zero() {
        assert!(executable_spread(&[leg(1.0, 101.0, 99.0, 0.0)]).is_none(), "crossed leg");
        assert!(executable_spread(&[leg(1.0, f64::NAN, 99.0, 0.0)]).is_none(), "nan leg");
        assert!(executable_spread(&[]).is_none(), "no legs");
    }

    /// The gate: an edge smaller than the round trip must NOT clear.
    #[test]
    fn edge_must_exceed_the_full_roundtrip_cost() {
        let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-1.0, 49.0, 51.0, 0.0)];
        assert!(!spread_edge_clears_cost(3.9, &legs), "3.9 < 4.0 must not clear");
        assert!(spread_edge_clears_cost(4.1, &legs), "4.1 > 4.0 must clear");
    }

    // --- carry (Task 1b) ---

    /// Carry is charged per leg on signed exposure: a long leg PAYS positive
    /// funding, a short leg RECEIVES it. Net carry on a hedged pair is the
    /// DIFFERENTIAL, not either leg's rate.
    #[test]
    fn carry_is_the_signed_differential_across_legs() {
        let legs = [leg(1.0, 100.0, 100.0, 0.0), leg(-1.0, 100.0, 100.0, 0.0)];
        // identical rates on both legs net to zero carry
        let c = spread_carry_cost(&legs, &[0.0001, 0.0001], 3.0).unwrap();
        assert!(c.abs() < 1e-12, "equal rates must net to zero, got {c}");
        // long leg pays 1bp more than the short receives -> a real cost
        let c = spread_carry_cost(&legs, &[0.0002, 0.0001], 3.0).unwrap();
        assert!((c - 0.03).abs() < 1e-12, "carry {c}"); // 1e-4 * 100 * 3 intervals
    }

    /// Carry scales with holding time — the term that makes "hold longer to
    /// amortize the fees" backfire.
    #[test]
    fn carry_scales_with_intervals_held() {
        let legs = [leg(1.0, 100.0, 100.0, 0.0)];
        let short_hold = spread_carry_cost(&legs, &[0.0001], 1.0).unwrap();
        let long_hold = spread_carry_cost(&legs, &[0.0001], 30.0).unwrap();
        assert!((long_hold - 30.0 * short_hold).abs() < 1e-12);
    }

    /// Total cost = crossings + carry. A caller must never charge only one.
    #[test]
    fn total_cost_is_crossings_plus_carry() {
        let legs = [leg(1.0, 99.0, 101.0, 0.0), leg(-1.0, 49.0, 51.0, 0.0)];
        let rt = spread_roundtrip_cost(&legs).unwrap();
        let carry = spread_carry_cost(&legs, &[0.0002, 0.0001], 3.0).unwrap();
        let total = spread_total_cost(&legs, &[0.0002, 0.0001], 3.0).unwrap();
        assert!((total - (rt + carry)).abs() < 1e-12);
    }

    /// A rate per leg is required; a mismatched slice is unknowable, not free.
    #[test]
    fn mismatched_rate_slice_is_none() {
        let legs = [leg(1.0, 100.0, 100.0, 0.0)];
        assert!(spread_carry_cost(&legs, &[], 1.0).is_none());
        assert!(spread_carry_cost(&legs, &[0.0001, 0.0002], 1.0).is_none());
        assert!(spread_carry_cost(&legs, &[f64::NAN], 1.0).is_none());
        assert!(spread_carry_cost(&legs, &[0.0001], -1.0).is_none(), "negative hold");
    }

    /// Carry can exceed the crossing cost on a long hold — the regime that makes
    /// a crossings-only floor dangerously optimistic.
    #[test]
    fn carry_can_dominate_the_crossing_cost() {
        let legs = [leg(1.0, 99.9, 100.1, 0.0), leg(-1.0, 99.9, 100.1, 0.0)];
        let rt = spread_roundtrip_cost(&legs).unwrap(); // 0.4
        // 3bp/interval differential over 90 intervals (~30 days at 8h funding)
        let carry = spread_carry_cost(&legs, &[0.0003, 0.0], 90.0).unwrap();
        assert!(carry > rt, "carry {carry} should dominate crossings {rt}");
    }
}
