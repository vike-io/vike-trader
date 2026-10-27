//! Inventory-band → taker-flatten "impulse" leg — the pure core of the Guilbaud–Pham impulse-control
//! idea, isolated so it unit-tests without a runtime. Ports the market-order (impulse) side of
//! Guilbaud & Pham, "Optimal High-Frequency Trading with Limit and Market Orders" (Quantitative
//! Finance 13(1):79–94, 2013; arXiv:1106.5040).
//!
//! The cherry-picked idea: a passive maker earns the spread by RESTING limit orders, but pure limit
//! quoting cannot bound inventory — an adverse run fills one side repeatedly and the position drifts
//! unboundedly. Guilbaud–Pham add an IMPULSE-CONTROL leg on top of the continuous limit-quoting: at
//! discrete moments the agent may cross the book with a MARKET order to instantaneously cut inventory
//! it cannot skew its way out of. The maker here already SKEWS its quote sizes toward flat
//! (`skew::skew_multipliers`) and, near a binary resolution, force-flattens via a reservation shift
//! (`settlement::force_flatten_skew`) — both PASSIVE. What lives HERE is the missing ACTIVE leg: when
//! inventory breaches a hard BAND, fire a taker order to cross the book and cut the excess.
//!
//! ⚠ PRACTITIONER HEURISTIC. The Guilbaud–Pham optimal impulse region is the solution of a
//! quasi-variational HJB inequality with no closed form; the fixed inventory BAND used here is the
//! standard practitioner reduction of that region to one tunable — the simplest defensible shape, and
//! the pinned, testable baseline. A convex/asymmetric band is a caller composition on top.
//!
//! FLATTEN-THE-EXCESS, not full-flatten (design choice): when the band is breached we cut only the
//! amount OVER the band (`|inventory| − band`), leaving the position resting exactly AT the band — the
//! band IS the tolerated inventory the passive skew is meant to manage, so a full-flatten to zero
//! would (a) pay the taker spread/impact on inventory the maker deliberately tolerates and (b) fight
//! the skew, which is already leaning that inventory down for free. Cutting to the band is the minimal
//! taker action that restores the passive regime's invariant (`|inventory| <= band`). A caller wanting
//! a hard flatten-to-zero passes `band = 0`.
//!
//! INERT-WHEN-OFF invariant: the check fires ONLY strictly OVER the band, so a maker whose inventory
//! stays within its band never takes — the impulse leg is dormant and the maker is byte-identical to
//! the passive skew-only path.

/// The taker-flatten impulse decision — pure. Returns `Some((side_sign, qty))` naming the MARKET
/// order that cuts the band-excess inventory, or `None` when nothing should be taken.
///
/// `inventory` = current SIGNED position (`> 0` long, `< 0` short); `band` = the tolerated inventory
/// half-width the passive skew manages (`>= 0`; a negative value is clamped to `0` = hard
/// flatten-to-zero).
///
/// Fires ONLY when `inventory.abs() > band` (STRICTLY over — exactly at the band is NOT over, so it
/// holds). When it fires:
/// - `side_sign = −inventory.signum()` — long (`inventory > 0`) ⇒ `−1.0` (SELL to reduce), short
///   (`inventory < 0`) ⇒ `+1.0` (BUY to reduce);
/// - `qty = inventory.abs() − band` — the EXCESS over the band only (see the module doc for the
///   flatten-the-excess vs full-flatten rationale); with `band = 0` this is the full `|inventory|`.
///
/// Returns `None` when: `inventory == 0.0` (already flat — `signum()`'s ±1 sign is meaningless and
/// there is nothing to cut), or `inventory.abs() <= band` (within/at the band — the passive regime's
/// invariant already holds). `qty` is therefore always strictly positive whenever `Some`.
pub(crate) fn taker_flatten(inventory: f64, band: f64) -> Option<(f64, f64)> {
    // A negative band is nonsensical; treat it as 0 (hard flatten-to-zero) rather than letting it
    // widen the trigger. Also the guard for the `abs > band` comparison below.
    let band = band.max(0.0);
    let abs = inventory.abs();
    // Flat, or within/at the band → no impulse. `<=` (not `<`) makes "exactly at the band" HOLD:
    // the band is the tolerated inventory, so sitting on it is not "over". `inventory == 0.0` is
    // caught by `abs <= band` whenever `band >= 0` (always, post-clamp), so it needs no separate arm.
    if abs <= band {
        return None;
    }
    // Over the band: cut the EXCESS toward the band (not to zero). Naive subtraction (no mul_add) to
    // match the crate's fold-order convention. `abs > band` here, so `qty > 0` strictly.
    let side_sign = -inventory.signum();
    let qty = abs - band;
    Some((side_sign, qty))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_band_is_none() {
        // Strictly inside the band → nothing to take, either side.
        assert_eq!(taker_flatten(5.0, 10.0), None);
        assert_eq!(taker_flatten(-5.0, 10.0), None);
        // Flat is always None (regardless of band).
        assert_eq!(taker_flatten(0.0, 10.0), None);
        assert_eq!(taker_flatten(0.0, 0.0), None);
    }

    #[test]
    fn long_over_band_sells_the_excess() {
        // Long 15 with a band of 10 → SELL (−1.0) the 5-unit excess.
        let (side, qty) = taker_flatten(15.0, 10.0).expect("over band ⇒ Some");
        assert_eq!(side.to_bits(), (-1.0_f64).to_bits(), "long ⇒ SELL");
        assert!((qty - 5.0).abs() < 1e-12, "cut the excess over the band only");
    }

    #[test]
    fn short_over_band_buys_the_excess() {
        // Short 15 with a band of 10 → BUY (+1.0) the 5-unit excess.
        let (side, qty) = taker_flatten(-15.0, 10.0).expect("over band ⇒ Some");
        assert_eq!(side.to_bits(), 1.0_f64.to_bits(), "short ⇒ BUY");
        assert!((qty - 5.0).abs() < 1e-12, "cut the excess over the band only");
    }

    #[test]
    fn band_zero_flattens_all_but_leaves_nothing_at_flat() {
        // band = 0 is hard flatten-to-zero: qty == |inventory|.
        let (side, qty) = taker_flatten(7.0, 0.0).expect("nonzero inventory over a 0 band ⇒ Some");
        assert_eq!(side.to_bits(), (-1.0_f64).to_bits());
        assert!((qty - 7.0).abs() < 1e-12, "full flatten");
        let (side, qty) = taker_flatten(-7.0, 0.0).expect("nonzero inventory over a 0 band ⇒ Some");
        assert_eq!(side.to_bits(), 1.0_f64.to_bits());
        assert!((qty - 7.0).abs() < 1e-12, "full flatten");
        // ...but exactly flat under a 0 band is still None (nothing to cut).
        assert_eq!(taker_flatten(0.0, 0.0), None);
    }

    #[test]
    fn exactly_at_band_is_none() {
        // abs == band is NOT "over" → hold (the band is the tolerated inventory).
        assert_eq!(taker_flatten(10.0, 10.0), None);
        assert_eq!(taker_flatten(-10.0, 10.0), None);
    }

    #[test]
    fn negative_band_clamped_to_zero() {
        // A negative band behaves as 0 (hard flatten), never as a widened trigger.
        let (side, qty) = taker_flatten(3.0, -5.0).expect("clamped band 0 ⇒ full flatten");
        assert_eq!(side.to_bits(), (-1.0_f64).to_bits());
        assert!((qty - 3.0).abs() < 1e-12, "excess over clamped-0 band == |inventory|");
        // Still flat ⇒ None under a negative (clamped-0) band.
        assert_eq!(taker_flatten(0.0, -5.0), None);
    }
}
