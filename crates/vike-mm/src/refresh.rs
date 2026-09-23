//! Order-refresh TOLERANCE — the pure "is this re-quote worth the wire?" predicate behind
//! [`SpreadMaker`](crate::SpreadMaker)'s anti-churn gate. Isolated here (like `skew`/`book`) so the
//! whole decision unit-tests without a runtime or a broker.
//!
//! CONTRACT. Given what a side already has RESTING (`price`, `size`) and the target the current
//! tick just computed, [`within_tolerance`] answers "close enough to leave alone". Both axes are
//! RELATIVE, measured in BASIS POINTS of the RESTING value ([`RefreshTolerance`]), which is the one
//! unit that reads sanely on both a 0..1-priced prediction market and a five-figure crypto book
//! without the strategy knowing either venue's tick grid. A skip needs BOTH axes inside tolerance.
//!
//! OFF IS THE DEFAULT. A `None` tolerance bag never reaches this module at all, and an all-zero bag
//! returns `false` from every call — so an unconfigured maker re-prices on every tick exactly as it
//! did before this feature, byte-identical.
//!
//! SAFETY. This predicate is consulted ONLY on the re-price (modify) path. Placing a new quote and
//! PULLING a resting one (the fill-rate breaker's suppression cancel) are never gated by it — a
//! quote that must come off the book always comes off, whatever the tolerance is set to. And the
//! caller does not even ASK when a fill has just invalidated that side's snapshot (`refresh_skips`'
//! `stale` short-circuit): `resting` is the maker's INTENDED quote, so after a PARTIAL fill it
//! overstates what is left at the venue and this predicate would wrongly answer "no change".

use vike_model::RefreshTolerance;

/// `true` when the freshly computed `target` `(price, size)` sits close enough to the side's
/// `resting` `(price, size)` that re-issuing a modify would be pure wire churn — i.e. the caller
/// should SKIP the modify and leave the resting order (and its venue queue position) alone.
///
/// PURE. Both axes must pass:
/// - price: `|target_px − resting_px| ≤ price_bps · 1e-4 · |resting_px|`
/// - size:  `|target_sz − resting_sz| ≤ size_bps  · 1e-4 · |resting_sz|`
///
/// A fully-inert bag (both axes non-positive — including [`RefreshTolerance::default`]) NEVER
/// skips, so turning the knob on with zeros is still today's behavior. Non-finite inputs never
/// skip either (the conservative direction: re-price rather than silently strand a stale quote).
pub(crate) fn within_tolerance(
    resting: (f64, f64),
    target: (f64, f64),
    tol: RefreshTolerance,
) -> bool {
    // An inert bag is treated as OFF: never skip. (`<=` rather than `== 0.0` keeps it float-lint
    // clean and treats a nonsensical negative threshold as "off", mirroring `skew_multipliers`.)
    if tol.price_bps <= 0.0 && tol.size_bps <= 0.0 {
        return false;
    }
    drift_within(resting.0, target.0, tol.price_bps)
        && drift_within(resting.1, target.1, tol.size_bps)
}

/// One axis: `true` when `target` is within `tol_bps` basis points of `resting`.
///
/// - an EXACTLY unchanged value always passes (nothing to send, whatever the threshold);
/// - a non-positive `tol_bps` therefore means "no tolerance on this axis" — only exact equality
///   passes it;
/// - a zero (or non-finite) `resting` reference has no relative scale to measure against, so it
///   never passes — the conservative direction.
fn drift_within(resting: f64, target: f64, tol_bps: f64) -> bool {
    let delta = (target - resting).abs();
    // `<= 0.0` on an absolute value is exactly "unchanged" (and is false for NaN, which then falls
    // through to the comparison below and fails it — a non-finite drift never skips).
    if delta <= 0.0 {
        return true;
    }
    if tol_bps <= 0.0 {
        return false;
    }
    let scale = resting.abs();
    if scale <= 0.0 {
        return false;
    }
    // Multiplied through instead of dividing (`delta/scale ≤ tol_bps/1e4`): no divide, no
    // reciprocal rounding, and NaN falls out as `false`.
    delta * 10_000.0 <= tol_bps * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bag with only a PRICE tolerance — the common tuning (sizes are usually either constant or
    /// meaningfully different).
    fn price_only(price_bps: f64) -> RefreshTolerance {
        RefreshTolerance { price_bps, size_bps: 0.0 }
    }

    // The inert bag (and the `Default`) never skips — this is what makes the OFF path unchanged.
    #[test]
    fn inert_tolerance_never_skips() {
        let inert = RefreshTolerance::default();
        assert_eq!(inert.price_bps.to_bits(), 0.0_f64.to_bits(), "default price axis is 0");
        assert_eq!(inert.size_bps.to_bits(), 0.0_f64.to_bits(), "default size axis is 0");
        // even a literally identical re-quote is not skipped when the bag is inert
        assert!(!within_tolerance((99.6, 1.0), (99.6, 1.0), inert), "inert ⇒ always re-price");
        assert!(!within_tolerance((99.6, 1.0), (99.6, 1.0), price_only(-5.0)), "negative ⇒ off");
    }

    // Basis points are relative to the RESTING price, so the same threshold behaves sanely on a
    // 0..1 prediction-market price and on a five-figure crypto price.
    #[test]
    fn price_axis_is_relative_basis_points() {
        // 25 bp of 0.41 = 0.001025
        let tol = price_only(25.0);
        assert!(within_tolerance((0.41, 5.0), (0.4110, 5.0), tol), "0.001 drift on 0.41 ≈ 24 bp");
        assert!(!within_tolerance((0.41, 5.0), (0.4120, 5.0), tol), "0.002 drift ≈ 49 bp > 25");
        // 25 bp of 100_000 = 250
        assert!(within_tolerance((100_000.0, 1.0), (100_200.0, 1.0), tol), "200 on 100k = 20 bp");
        assert!(!within_tolerance((100_000.0, 1.0), (100_400.0, 1.0), tol), "400 on 100k = 40 bp");
        // exactly AT the threshold is inside (the comparison is `<=`): 25 bp of 100 is 0.25
        assert!(within_tolerance((100.0, 1.0), (100.25, 1.0), tol), "the boundary is inclusive");
        // direction-agnostic: the drift is an absolute value
        assert!(within_tolerance((100.0, 1.0), (99.75, 1.0), tol), "downward drift too");
    }

    // Both axes must pass: a pinned price with a moved SIZE is still a re-quote.
    #[test]
    fn size_axis_gates_independently() {
        // price-only tuning: any size change at all re-quotes (0 bp ⇒ exact equality required)
        assert!(within_tolerance((99.6, 1.0), (99.6, 1.0), price_only(25.0)), "same size passes");
        assert!(
            !within_tolerance((99.6, 1.0), (99.6, 1.000_001), price_only(25.0)),
            "a size change with no size tolerance re-quotes"
        );
        // with a size tolerance, a small size drift passes but a big one does not
        let both = RefreshTolerance { price_bps: 25.0, size_bps: 100.0 }; // 100 bp = 1%
        assert!(within_tolerance((99.6, 2.0), (99.6, 2.01), both), "0.5% size drift is inside 1%");
        assert!(!within_tolerance((99.6, 2.0), (99.6, 2.05), both), "2.5% size drift is outside");
    }

    // Degenerate references and non-finite drift never skip — the conservative direction.
    #[test]
    fn degenerate_inputs_never_skip() {
        let tol = RefreshTolerance { price_bps: 25.0, size_bps: 100.0 };
        // a zero resting reference has no relative scale (but an EXACTLY equal value still passes)
        assert!(!within_tolerance((0.0, 1.0), (0.000_1, 1.0), tol), "no scale off a 0 reference");
        assert!(within_tolerance((0.0, 1.0), (0.0, 1.0), tol), "exactly unchanged still passes");
        // NaN anywhere ⇒ re-price
        assert!(!within_tolerance((f64::NAN, 1.0), (99.6, 1.0), tol), "NaN resting price");
        assert!(!within_tolerance((99.6, 1.0), (f64::NAN, 1.0), tol), "NaN target price");
        assert!(!within_tolerance((99.6, 1.0), (99.6, f64::NAN), tol), "NaN target size");
        assert!(!within_tolerance((99.6, 1.0), (f64::INFINITY, 1.0), tol), "infinite target");
    }
}
