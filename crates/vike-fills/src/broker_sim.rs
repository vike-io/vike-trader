//! broker_sim — the canonical cost model. ONE definition of fills, fees, and funding.
//! Exact port of `core/broker_sim.py`. Convention: `side_sign` is +1 buy / -1 sell;
//! `multiplier` scales every notional term. Slippage is adverse: buys fill up, sells down.
//!
//! ## The one deliberate deviation from the port: an adverse move may not invert a price
//!
//! [`adverse_fill_price`] is multiplicative — `raw * (1 + side_sign * slippage)` — and the Python
//! original bounded neither operand. An adverse fraction at or past 1.0 on the losing side does
//! not saturate there, it INVERTS: a sell of a positive quote comes back NEGATIVE (raw 100,
//! `side_sign = -1`, slippage 1.5 -> -50). `vike_model::round_to` preserves the sign, and the
//! wrong-signed price then flows on into the notional floor, the fee, the cash flow, the
//! position's average price and the equity curve. Nothing downstream re-checks it.
//!
//! That regime is REACHABLE, not hypothetical. `vike_backtest::SimBroker::slippage_for` returns
//! `slippage + ImpactModel::impact_frac(..)`, and the impact addend is bounded BELOW (a cost is
//! never negative) but never above: `vike_backtest::impact::AlmgrenChriss` crosses 1.0 at
//! roughly 18x the window's mean bar volume once the measured per-bar sigma reaches 20%, and a
//! thin resampled series produces both halves of that routinely. `paper.rs`'s fee-clamp test
//! already states the assumption the rest of the engine is written against — "the sell side
//! multiplies DOWN and cannot cross zero for any `slippage < 1`" — but until this floor nothing
//! made it true.
//!
//! The floor is on the FACTOR ([`MIN_ADVERSE_FACTOR`]) and it lives HERE rather than at any of the
//! sites that produce a slippage number, because this is the only function that holds both
//! operands of the inversion and every producer funnels through it: the flat `slippage` field,
//! `slippage_for`'s impact sum, `StrategyEngine`'s cash pre-check (which must price the fill at
//! exactly what `apply_fill` is about to charge), the vector kernel and the paper exchange. One
//! floor here keeps all of them consistent; a floor upstream would fix one lane and leave four —
//! and a floor inside an `vike_backtest::impact::ImpactModel` could not work at all, since a
//! model never sees the flat `slippage` it is added to, nor the side that decides which way the
//! move points.
//!
//! It SATURATES, deliberately, rather than repairing: a run that reaches the floor has a cost
//! model whose output stopped being a cost, and the fill it then books (1% of the quote) should
//! look ruinous, because it is. `vike_backtest::SimBroker::slippage_saturations` counts the
//! fills where the floor bound, so such a run can say so instead of quietly returning a
//! plausible number.
//!
//! And it floors the FACTOR, not the price: the fill keeps the SIGN of the quote it was priced
//! off, so a genuinely negative quoted price — this workspace ships futures and EOD backfills, and
//! `vike_model::gross_notional` explicitly refuses to `.abs()` such prices away — passes through
//! untouched and stays negative. Only a factor that had already crossed zero is replaced.

/// The smallest factor [`adverse_fill_price`] will ever multiply a quoted price by: the adverse
/// move saturates once it has consumed 99% of the price.
///
/// STRICTLY POSITIVE is the load-bearing property — it is the whole reason the fill cannot change
/// sign. The magnitude is a policy choice, picked so that the floor (a) sits far above any adverse
/// move that could still be economically meaningful, and therefore can only ever bind on an
/// estimate that has already stopped meaning anything, and (b) stays far enough from zero that the
/// saturated fill is a usable double rather than a dust price that would silently re-route through
/// `SimBroker::apply_fill`'s min-notional floor and turn a saturation into a dropped order.
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
/// BYTE-IDENTICAL wherever the floor does not bind, which is every input the frozen path could
/// already price sanely: binding the factor to a local and multiplying by it is the same two f64
/// operations in the same order (Rust never contracts `a * b + c` into an FMA), and the branch is
/// taken only when `1 + side_sign * slippage` is at or below zero — exactly the inputs that used
/// to produce a zero or sign-flipped fill.
///
/// NaN is deliberately NOT caught: `factor <= 0.0` is false for NaN, so a NaN slippage still
/// yields a NaN price, bit-for-bit as before. A NaN is a different defect from an inversion, and
/// swallowing it behind this floor would re-introduce exactly the silent behaviour the floor
/// exists to remove.
#[inline]
pub fn adverse_fill_price(raw_price: f64, side_sign: i32, slippage: f64) -> f64 {
    let factor = adverse_factor(side_sign, slippage);
    let factor = if factor <= 0.0 { MIN_ADVERSE_FACTOR } else { factor };
    raw_price * factor
}

/// Did this `(side_sign, slippage)` pair drive [`adverse_fill_price`] onto its floor — i.e. has the
/// adverse move reached or passed the whole quoted price?
///
/// The engine-facing question, exposed so a caller can COUNT a saturation without re-spelling the
/// factor expression (see `vike_backtest::SimBroker::slippage_saturations`). `false` for a NaN
/// slippage, matching [`adverse_fill_price`]'s NaN pass-through: a NaN did not saturate, it is
/// unmeasurable.
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

    /// The frozen expression, spelled out once so every parity assertion below compares against
    /// the arithmetic that shipped rather than against a paraphrase of it.
    fn frozen(raw_price: f64, side_sign: i32, slippage: f64) -> f64 {
        raw_price * (1.0 + side_sign as f64 * slippage)
    }

    /// The concrete case `vike-ops/tests/duplicate_shape_gate.rs`'s `sim_broker.rs` ALLOWLIST row
    /// cites: sell 2 @ raw 100 with a total slippage of 1.5. It used to price at -50 — a positive
    /// quote sold for a negative price — which is what made the min-notional floor's SIGNED product
    /// unconditionally trip. It now saturates instead.
    #[test]
    fn a_sell_past_full_slippage_no_longer_inverts_the_price() {
        let px = adverse_fill_price(100.0, -1, 1.5);
        assert!(px > 0.0, "a positive quote must not sell for a negative price, got {px}");
        assert_eq!(px.to_bits(), (100.0 * MIN_ADVERSE_FACTOR).to_bits());
        assert!(adverse_move_saturates(-1, 1.5));
        // And the notional `SimBroker::apply_fill`'s below-min floor computes off it — the SIGNED
        // product that row is about — is positive again, so the floor stops tripping on the sign.
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

    /// The mirror case: a BUY charged a large NEGATIVE slippage (price improvement, the only way
    /// the buy side can cross zero) is floored on the same rule. The guard is on the FACTOR, not
    /// on the sell side, so neither direction can invert.
    #[test]
    fn a_buy_with_a_past_full_price_improvement_is_floored_too() {
        assert!(frozen(100.0, 1, -1.5) < 0.0, "the frozen path inverted here");
        assert!(adverse_move_saturates(1, -1.5));
        assert_eq!(adverse_fill_price(100.0, 1, -1.5).to_bits(), 1.0f64.to_bits());
    }

    /// A genuinely NEGATIVE quoted price is legitimate (futures, EOD backfills) and must flow
    /// through completely untouched — the floor is on the factor, so the fill keeps the quote's
    /// sign in BOTH the normal and the saturated case.
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
