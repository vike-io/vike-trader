//! [`MIN_MARKET_SLIPPAGE`] / [`MAX_MARKET_SLIPPAGE`] — the bounds on an EMULATED-MARKET slippage
//! band. The ONE copy, imported by everything that validates or clamps one.
//!
//! ## What a "market slippage band" is, and why it needs a bound at all
//!
//! A few venues have no native market order. **Hyperliquid is the only one on this roster** — every
//! other bridge ships a native wire value (`MARKET`/`Market`/`market`/`MKT`, or JForex's
//! `OrderCommand.BUY`), and the two crypto venues that need a protective price on a *triggered*
//! stop use the venue's OWN sentinel (okx `slOrdPx: "-1"`, binance/aster `STOP_MARKET`) rather than
//! an arithmetic band. Such a venue's adapter must EMULATE a market intent as an immediate-or-cancel
//! LIMIT priced aggressively off a reference price: `ref * (1 + band)` to buy, `ref * (1 - band)` to
//! sell.
//!
//! That band is the **aggression bound on every market order the venue sends**. It is not a
//! prediction of slippage and it is not a fee — it is the worst price the order is ALLOWED to reach.
//! On a deep book the order fills at the touch and the band is never approached; on a thin book the
//! order walks the entire band and fills at the far end **silently** — no rejection, no alert, and
//! nothing in the fill event that says "this was the bound, not the market". A band chosen so that
//! orders always fill is therefore a standing authorization to pay that much on every market order,
//! including protective exits.
//!
//! ## Why the bounds live HERE
//!
//! Two places need them and must never disagree: `vike_config::Policy`, which REJECTS an
//! out-of-range value at the file edge naming the file and the key, and
//! `vike_bridge_core::market_slippage::resolve_market_slippage`, which CLAMPS a value on its way to
//! the wire. A second copy of a bound is exactly the split-brain [`crate::rate_limits`]' own doc
//! warns about, so — like `MIN_UTILIZATION`/`MAX_UTILIZATION` — these numbers are defined once, in
//! the leaf crate both consumers can see.
//!
//! ## What is deliberately NOT here
//!
//! No per-venue and no per-symbol table. The right band genuinely differs per instrument (a liquid
//! major wants ~0.1 %, a thin alt needs more), but that number is a property of the book's DEPTH at
//! order time, not a constant anyone can honestly write down in advance — and a hardcoded per-symbol
//! table would read as verified fact while being wrong for most rows the day after it is written
//! (the capability-map honesty rule; `vike_bridge_core::leverage`'s "what is NOT clamped, and why"
//! makes the same call about venue leverage ceilings). The honest per-symbol source is the venue's
//! own book, measured at submit; see `vike_bridge_core::market_slippage`'s module doc for the shape
//! that would take.

/// Floor of the accepted band, **0.1 %**.
///
/// A floor exists for a reason the ceiling's mirror-image intuition gets wrong: **tighter is not
/// automatically safer.** Below roughly a tick or two of a normal spread the emulated order is no
/// longer marketable, and an `Ioc` that fails to cross simply **cancels with no fill**. On the
/// stop-MARKET path — a tripped protective stop, whose limit is priced off the trigger by this same
/// band — that is a stop-loss which did not exit, and an unfilled exit is an unbounded loss. So the
/// floor sits at the tight end of what still crosses a normal book (0.1 %, the band a liquid major
/// such as BTC or ETH actually wants) rather than at zero.
pub const MIN_MARKET_SLIPPAGE: f64 = 0.001;

/// Ceiling of the accepted band, **5 %** — deliberately EQUAL to the widest band this workspace has
/// ever put on the wire (hyperliquid's historical hardcoded `MARKET_SLIPPAGE`). Setting the ceiling
/// at today's value makes the knob a **one-way ratchet**: it can only ever tighten what a mount
/// already does, never widen it, so no configuration can make any venue more aggressive than the
/// code was before the knob existed.
///
/// The documented reason this is a hard constant rather than another operator number: the failure
/// being prevented is a band raised *because orders were not filling* — "set it to 50 % so it always
/// goes through". That does not fix a liquidity problem, it converts it into an unbounded, silent
/// price concession on every market order and every protective stop exit. A venue whose book cannot
/// absorb an order inside 5 % is a venue on which that order should be worked, split, or refused —
/// not slipped. Raising this number is a code change, and therefore a review.
pub const MAX_MARKET_SLIPPAGE: f64 = 0.05;

// COMPILE-TIME invariants, not tests — a violation fails the BUILD and cannot be skipped, and
// clippy rejects the runtime form when both sides are `const` anyway.
//
// A band is a FRACTION, not a percent or basis points: 5% is `0.05`. A ceiling at or above `1.0`
// would price a sell at or below zero, which is not a worse fill — it is a nonsensical order.
const _: () = assert!(MIN_MARKET_SLIPPAGE > 0.0);
const _: () = assert!(MAX_MARKET_SLIPPAGE > MIN_MARKET_SLIPPAGE);
const _: () = assert!(MAX_MARKET_SLIPPAGE < 1.0);

/// Whether `band` is a usable slippage band: finite and within
/// [`MIN_MARKET_SLIPPAGE`]`..=`[`MAX_MARKET_SLIPPAGE`] inclusive.
///
/// Shared so the two consumers agree on the RANGE TEST as well as on the numbers — the file edge
/// (which rejects, naming the key) and the wire edge (which clamps) must not be able to disagree
/// about which values are in bounds, only about what to do with one that is not.
pub fn is_usable_market_slippage(band: f64) -> bool {
    band.is_finite() && (MIN_MARKET_SLIPPAGE..=MAX_MARKET_SLIPPAGE).contains(&band)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bounds are enforced at COMPILE time beside the constants themselves (the `const _: ()`
    /// block above), which is strictly stronger than a test: a violation fails the build and
    /// cannot be skipped. clippy's `assertions_on_constants` correctly rejects the runtime form
    /// when both sides are `const`. This test only records the intent.
    #[test]
    fn the_bounds_are_the_documented_pair() {
        assert_eq!(MIN_MARKET_SLIPPAGE, 0.001);
        assert_eq!(MAX_MARKET_SLIPPAGE, 0.05);
    }

    /// The ratchet, pinned: the ceiling IS hyperliquid's historical hardcoded band, so the setting
    /// can only tighten. If this ever has to change, it is a deliberate widening of the worst price
    /// every emulated market order in the workspace may reach — read this module's doc first.
    #[test]
    fn the_ceiling_is_todays_hyperliquid_band() {
        assert_eq!(MAX_MARKET_SLIPPAGE, 0.05);
    }

    #[test]
    fn range_test_accepts_the_endpoints_and_rejects_nonsense() {
        assert!(is_usable_market_slippage(MIN_MARKET_SLIPPAGE));
        assert!(is_usable_market_slippage(MAX_MARKET_SLIPPAGE));
        assert!(is_usable_market_slippage(0.002));
        for bad in [0.0, -0.01, 0.0009, 0.0500001, 0.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY]
        {
            assert!(!is_usable_market_slippage(bad), "{bad} must not be usable");
        }
    }
}
