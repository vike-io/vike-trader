//! broker_sim — the canonical cost model: ONE definition of fills, fees and funding.
//! Convention: `side_sign` is +1 buy / -1 sell; `multiplier` scales every notional term.
//! Slippage is adverse: buys fill up, sells down.
//!
//! ## An adverse move may not invert a price
//!
//! [`adverse_fill_price`] is multiplicative — `raw * (1 + side_sign * slippage)` — so an adverse
//! fraction at or past 1.0 on the losing side does not saturate, it INVERTS: a sell of a positive
//! quote comes back NEGATIVE (raw 100, `side_sign = -1`, slippage 1.5 -> -50).
//! `vike_model::round_to` keeps the sign, and nothing downstream re-checks it: the wrong-signed
//! price flows into the notional floor, the fee, the cash flow, the average price and the equity.
//!
//! That regime is REACHABLE: `vike_sim::SimBroker::slippage_for` returns
//! `slippage + ImpactModel::impact_frac(..)`, an addend bounded below (a cost is never negative)
//! but not above; `vike_sim::AlmgrenChriss` crosses 1.0 at roughly 18x the window's mean bar
//! volume once the measured per-bar sigma reaches 20%, and a thin resampled series produces both
//! routinely. `crates/vike-paper/src/fee_schedule_tests.rs`'s
//! `schedule_path_clamps_a_slippage_pushed_price_into_the_probability_domain` states the assumption
//! the engine is written against (the sell side "cannot cross zero for any `slippage < 1`");
//! until [`MIN_ADVERSE_FACTOR`] nothing made it true.
//!
//! The floor lives HERE, not at any site that produces a slippage number, because this is the
//! only function holding both operands of the inversion and every producer funnels through it:
//! the flat `slippage` field, `slippage_for`'s impact sum, `StrategyEngine`'s cash pre-check (which
//! must price exactly what `apply_fill` will charge), the vector kernel and the paper exchange. A
//! floor upstream would fix one lane and leave four; one inside a `vike_sim::ImpactModel` cannot
//! work at all, since a model sees neither the flat `slippage` it is added to nor the side.

/// The smallest factor [`adverse_fill_price`] will ever multiply a quoted price by: the adverse
/// move saturates once it has consumed 99% of the price.
///
/// A floor on the FACTOR, not the price, and STRICTLY POSITIVE (the load-bearing property): the
/// fill keeps its quote's sign, so a genuinely negative quote (futures, EOD backfills;
/// `vike_model::gross_notional` refuses to `.abs()` it away) stays negative. The magnitude is
/// policy: far above any economically meaningful move, so it binds only on an estimate that
/// already stopped meaning anything, yet far enough from zero that the saturated fill is not a
/// dust price `SimBroker::apply_fill`'s min-notional floor would turn into a dropped order.
///
/// It SATURATES rather than repairs: a run that reaches the floor has a cost model whose output
/// stopped being a cost, and its fill (1% of the quote) should look ruinous, because it is.
/// `vike_sim::SimBroker::slippage_saturations` counts those fills so such a run says so.
pub const MIN_ADVERSE_FACTOR: f64 = 0.01;

/// The multiplicative factor an adverse move of `slippage` applies on `side_sign`, BEFORE the
/// [`MIN_ADVERSE_FACTOR`] floor. Private and shared so [`adverse_fill_price`] and
/// [`adverse_move_saturates`] can never disagree about what "the factor" is.
#[inline]
fn adverse_factor(side_sign: i32, slippage: f64) -> f64 {
    1.0 + side_sign as f64 * slippage
}

/// The fill price after adverse slippage: buys (+1) up, sells (-1) down — with the move floored at
/// [`MIN_ADVERSE_FACTOR`] times the quote, so it can never carry the price through zero.
///
/// BYTE-IDENTICAL wherever the floor does not bind: binding the factor to a local and multiplying
/// by it is the same two f64 operations in the same order (Rust never contracts `a * b + c` into
/// an FMA), and the branch is taken only when `1 + side_sign * slippage` is at or below zero —
/// exactly the inputs that used to produce a zero or sign-flipped fill.
///
/// NaN is deliberately NOT caught: `factor <= 0.0` is false for NaN, so a NaN slippage still
/// yields a NaN price. It is a different defect from an inversion, and swallowing it here would
/// re-introduce the silent behaviour the floor exists to remove.
#[inline]
pub fn adverse_fill_price(raw_price: f64, side_sign: i32, slippage: f64) -> f64 {
    let factor = adverse_factor(side_sign, slippage);
    let factor = if factor <= 0.0 { MIN_ADVERSE_FACTOR } else { factor };
    raw_price * factor
}

/// Did this `(side_sign, slippage)` pair drive [`adverse_fill_price`] onto its floor — has the
/// adverse move reached or passed the whole quoted price? Lets a caller COUNT a saturation without
/// re-spelling the factor (`vike_sim::SimBroker::slippage_saturations`). `false` for a NaN
/// slippage, matching the price's NaN pass-through: a NaN did not saturate, it is unmeasurable.
#[inline]
pub fn adverse_move_saturates(side_sign: i32, slippage: f64) -> bool {
    adverse_factor(side_sign, slippage) <= 0.0
}

/// Transaction fee = `rate` on the (multiplier-scaled) notional.
#[inline]
pub fn fee(size: f64, price: f64, rate: f64, multiplier: f64) -> f64 {
    size * price * rate * multiplier
}

/// Perp funding cash flow for a held position: longs pay positive funding, shorts receive.
#[inline]
pub fn funding_charge(pos_size: f64, mark_price: f64, funding_rate: f64, multiplier: f64) -> f64 {
    pos_size * mark_price * funding_rate * multiplier
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-floor expression, spelled out once: parity assertions compare against the
    /// arithmetic that shipped, not a paraphrase.
    fn frozen(raw_price: f64, side_sign: i32, slippage: f64) -> f64 {
        raw_price * (1.0 + side_sign as f64 * slippage)
    }

    /// The case `crates/vike-ops/tests/hygiene/duplicate_shape_gate.rs`'s `ALLOWLIST` row for
    /// `sim_broker.rs` records as closed: sell 2 @ raw 100 with a total slippage of 1.5 priced at
    /// -50, which made the min-notional floor's SIGNED product unconditionally trip. It now
    /// saturates instead.
    #[test]
    fn a_sell_past_full_slippage_no_longer_inverts_the_price() {
        let px = adverse_fill_price(100.0, -1, 1.5);
        assert!(px > 0.0, "a positive quote must not sell for a negative price, got {px}");
        assert_eq!(px.to_bits(), (100.0 * MIN_ADVERSE_FACTOR).to_bits());
        assert!(adverse_move_saturates(-1, 1.5));
        // The SIGNED notional `SimBroker::apply_fill`'s below-min floor computes is positive again.
        assert!(2.0 * px > 0.0);
    }

    /// Exactly 100% adverse is the boundary and it SATURATES rather than filling at zero: a
    /// zero-priced fill is its own poison (zero notional, free position, meaningless basis).
    #[test]
    fn the_boundary_at_a_full_hundred_percent_saturates_rather_than_filling_at_zero() {
        assert_eq!(frozen(100.0, -1, 1.0), 0.0, "the frozen path filled AT zero here");
        assert!(adverse_move_saturates(-1, 1.0));
        assert_eq!(adverse_fill_price(100.0, -1, 1.0).to_bits(), 1.0f64.to_bits());
    }

    /// The mirror case: a BUY with a large NEGATIVE slippage (price improvement, the only way a buy
    /// can cross zero) is floored too — the guard is on the FACTOR, not the sell side.
    #[test]
    fn a_buy_with_a_past_full_price_improvement_is_floored_too() {
        assert!(frozen(100.0, 1, -1.5) < 0.0, "the frozen path inverted here");
        assert!(adverse_move_saturates(1, -1.5));
        assert_eq!(adverse_fill_price(100.0, 1, -1.5).to_bits(), 1.0f64.to_bits());
    }

    /// A genuinely NEGATIVE quote (futures, EOD backfills) keeps its sign in BOTH the normal and
    /// the saturated case: the floor is on the factor.
    #[test]
    fn a_negative_quoted_price_is_not_repaired() {
        for &side in &[1, -1] {
            for &slip in &[0.0, 0.0005, 0.01, 0.5, 0.99] {
                let got = adverse_fill_price(-50.0, side, slip);
                assert_eq!(got.to_bits(), frozen(-50.0, side, slip).to_bits());
                assert!(got < 0.0, "sign changed at side={side} slippage={slip}: {got}");
                assert!(!adverse_move_saturates(side, slip));
            }
        }
        // Even ON the floor the quote's own sign survives — the fill is not forced positive.
        let saturated = adverse_fill_price(-50.0, -1, 2.0);
        assert!(saturated < 0.0, "a negative quote must stay negative when floored: {saturated}");
        assert_eq!(saturated.to_bits(), (-50.0 * MIN_ADVERSE_FACTOR).to_bits());
    }

    /// BYTE-PARITY: every input whose factor stayed positive prices bit-for-bit as it did before
    /// the floor existed. Swept over a deterministic xorshift grid rather than a handful of points,
    /// because "no re-association" is the claim being made and one ulp is the failure mode.
    #[test]
    fn every_non_inverting_input_is_bit_for_bit_unchanged() {
        fn next(seed: &mut u64) -> f64 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            (*seed >> 11) as f64 / (1u64 << 53) as f64
        }
        let seed = &mut 0x243F_6A88_85A3_08D3u64;
        for _ in 0..200_000 {
            let raw = next(seed) * 200_000.0 - 100_000.0; // both signs, wide magnitude
            let slip = next(seed) * 0.999_999; // any move short of the whole price
            for side in [-1i32, 1] {
                assert!(!adverse_move_saturates(side, slip));
                assert_eq!(
                    adverse_fill_price(raw, side, slip).to_bits(),
                    frozen(raw, side, slip).to_bits(),
                    "moved at raw={raw} side={side} slippage={slip}"
                );
            }
        }
        // The pinned scalars the r1 golden fixture is built from live in this same regime — every
        // `slippage` it carries is under 0.003 — so the fixture is untouched by construction.
        for &slip in &[0.0, 0.0005, 0.001, 0.002] {
            for side in [-1i32, 1] {
                assert_eq!(
                    adverse_fill_price(46_842.6, side, slip).to_bits(),
                    frozen(46_842.6, side, slip).to_bits()
                );
            }
        }
    }

    /// A NaN slippage stays a NaN price. It is a different defect and the floor must not hide it.
    #[test]
    fn a_nan_slippage_is_not_swallowed_by_the_floor() {
        assert!(adverse_fill_price(100.0, -1, f64::NAN).is_nan());
        assert!(
            !adverse_move_saturates(-1, f64::NAN),
            "a NaN did not saturate, it is unmeasurable"
        );
    }

    /// An infinite adverse move on the losing side is an inversion like any other (`-inf` from a
    /// positive quote); on the winning side it is not, and passes through.
    #[test]
    fn an_infinite_adverse_move_is_floored_only_where_it_inverts() {
        assert!(adverse_move_saturates(-1, f64::INFINITY));
        assert_eq!(adverse_fill_price(100.0, -1, f64::INFINITY).to_bits(), 1.0f64.to_bits());
        assert!(!adverse_move_saturates(1, f64::INFINITY));
        assert_eq!(adverse_fill_price(100.0, 1, f64::INFINITY), f64::INFINITY);
    }

    /// The predicate and the price function read the SAME factor, so the predicate exactly decides
    /// which of the two arms the price took — they cannot drift apart.
    #[test]
    fn the_predicate_decides_exactly_which_arm_the_price_took() {
        for &raw in &[100.0f64, -100.0, 0.25] {
            for side in [-1i32, 1] {
                for &slip in &[-3.0f64, -1.5, -1.0, -0.5, 0.0, 0.5, 0.999, 1.0, 1.5, 3.0] {
                    let got = adverse_fill_price(raw, side, slip);
                    if adverse_move_saturates(side, slip) {
                        assert_eq!(
                            got.to_bits(),
                            (raw * MIN_ADVERSE_FACTOR).to_bits(),
                            "saturated but not floored: raw={raw} side={side} slippage={slip}"
                        );
                    } else {
                        assert_eq!(
                            got.to_bits(),
                            frozen(raw, side, slip).to_bits(),
                            "not saturated but moved: raw={raw} side={side} slippage={slip}"
                        );
                    }
                }
            }
        }
    }
}
