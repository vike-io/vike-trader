//! The ONE pure rule deciding the SLIPPAGE BAND a venue with no native market order prices its
//! emulated market (and stop-market) orders at.
//!
//! The sibling of [`crate::leverage`], and built to the same contract for the same reason: a number
//! that reaches a live order path must be resolved ONCE at the mount, by a pure function that can be
//! tested without I/O, with a bound that cannot be argued away at 3 a.m.
//!
//! ## The divergence this closes
//!
//! Hyperliquid has no native market order (verified against every other bridge on the roster: they
//! all ship a native `MARKET`/`Market`/`market`/`MKT` wire value, and okx/binance/aster use the
//! venue's own sentinel for a triggered stop rather than an arithmetic band). Its adapter therefore
//! EMULATES a market intent as an `Ioc` limit priced `mid * (1 ± band)`, and priced the SAME band
//! off the trigger for a stop-MARKET. That band was a hardcoded `const MARKET_SLIPPAGE: f64 = 0.05`
//! — **5 %, on every market order, on every instrument.**
//!
//! On a deep BTC book that constant never binds: the order fills at the touch. On a thin alt book it
//! binds completely — the order walks the whole 5 % and fills there, with no rejection, no alert,
//! and nothing in the resulting fill that distinguishes "the market moved" from "we authorized
//! this". The right value is per-instrument and an order of magnitude smaller for the majors
//! (0.1–0.5 %); the safe direction is DOWN.
//!
//! ## The contract
//!
//! 1. **Unset ⇒ UNCHANGED.** `None` — no operator value at all — yields the venue's own
//!    `venue_default`, the exact literal it priced with before this function existed. An upgrade must
//!    never silently change how aggressively a live account's market orders are priced, so this is
//!    the property that matters most; it is pinned by a test here AND by one in the venue's
//!    `exec.rs`.
//! 2. **Set ⇒ the operator's number**, verbatim, whenever it is usable
//!    ([`vike_model::market_slippage::is_usable_market_slippage`]).
//! 3. **Out of range ⇒ CLAMPED into range**, warned, naming the requested value and the bound
//!    applied. Never widened past [`MAX_MARKET_SLIPPAGE`], which is itself today's literal — so no
//!    configuration, valid or not, can make any venue more aggressive than the code already was.
//!
//! Rule 3 clamps rather than errors because this is a live mount: a daemon that refuses to start
//! over a settings value stops trading, and stopping is not obviously safer than proceeding at a
//! bound that is by construction no worse than today. The value's real home
//! (`vike_config::Policy::market_slippage`) REJECTS an out-of-range value at the FILE edge, naming
//! the file and the key — so rule 3 is the floor under a programmatically-built value, exactly as
//! [`crate::leverage`]'s rule 3 is unreachable from a loaded profile.
//!
//! ## Why `venue_default` is NOT validated
//!
//! `venue_default` rides through rule 1 unchecked, on purpose. Validating it could change today's
//! behaviour on the unset path, and rule 1 forbids exactly that. The venue owns its own historical
//! literal and pins it in its own tests (see `vike_hyperliquid::exec`'s
//! `the_venue_default_is_within_the_workspace_bounds`), which is where a venue-specific number
//! belongs.
//!
//! ## Per-symbol: the right answer, deliberately not built here
//!
//! One global band is not the ideal shape — a liquid major wants ~0.1 % and a thin alt genuinely
//! needs more, and forcing both through one number means either over-paying on the majors or failing
//! to fill on the alts. But the honest per-symbol source is the venue's own BOOK DEPTH at submit
//! time (walk the resting levels for the requested size and price the band at the level that fills
//! it, with this constant as the outer bound), **not** a hand-maintained per-symbol table — such a
//! table would read as verified fact while being wrong for most rows the day after it is written.
//! That is the same call [`crate::leverage`] makes about venue leverage ceilings, and it needs the
//! L2 book on the exec path, which this adapter does not have today. The shape it would take:
//! `resolve_market_slippage` keeps this signature as the outer bound, and a future
//! `book_bounded_slippage(book, side, qty) -> Option<f64>` feeds its `requested` argument, so the
//! ceiling still applies to a measured band exactly as it does to a configured one.

use vike_model::market_slippage::{
    MAX_MARKET_SLIPPAGE, MIN_MARKET_SLIPPAGE, is_usable_market_slippage,
};

/// Resolve the slippage band an emulated-market order is priced at.
///
/// * `requested` — the operator's band as a fraction (`0.002` = 0.2 %), or `None` for "no value
///   expressed", the common desktop/paper case and every mount that has not set one.
/// * `venue_default` — the literal THIS venue priced with before this function existed. Returned
///   verbatim on `None`, so an upgrade is a no-op.
///
/// The returned value is always finite and — whenever `requested` was `Some` — always within
/// [`MIN_MARKET_SLIPPAGE`]`..=`[`MAX_MARKET_SLIPPAGE`], so the caller's `1.0 ± band` price
/// arithmetic can neither produce a non-finite price nor price a sell at or below zero. See the
/// module doc for the full contract.
pub fn resolve_market_slippage(requested: Option<f64>, venue_default: f64) -> f64 {
    let Some(band) = requested else {
        // Rule 1 — no operator value: today's literal, byte-identical.
        return venue_default;
    };
    if is_usable_market_slippage(band) {
        // Rule 2 — the operator's own number.
        return band;
    }
    // Rule 3 — clamp into range. NaN and every value below the floor (including negatives, which
    // would inverse the price, and -inf) land on the FLOOR, matching `crate::leverage`'s "nonsense
    // degrades to the conservative end, never to the default": falling back to `venue_default` here
    // would silently hand back the widest band in the workspace, which is the exact failure this
    // module exists to prevent.
    let bounded =
        if band > MAX_MARKET_SLIPPAGE { MAX_MARKET_SLIPPAGE } else { MIN_MARKET_SLIPPAGE };
    // Mount-time only (once per venue mount), so a warn here is bounded and worth the noise: it
    // names a value the config edge would have rejected outright.
    tracing::warn!(
        target: "vike_bridge_core::market_slippage",
        requested = band,
        applied = bounded,
        min = MIN_MARKET_SLIPPAGE,
        max = MAX_MARKET_SLIPPAGE,
        "requested market-slippage band is outside the allowed range (a band is a FRACTION: 0.002 \
         = 0.2%) — clamping. The maximum is the widest band this workspace has ever sent, so no \
         setting can price market orders more aggressively than the code already did"
    );
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE 1, the property that matters most: no operator value ⇒ the venue's historical literal.
    /// An upgrade must not change how aggressively a live account's market orders are priced.
    #[test]
    fn unset_yields_the_venue_default() {
        assert_eq!(resolve_market_slippage(None, 0.05), 0.05);
        // Not special-cased to hyperliquid's number anywhere: any venue's own literal rides through.
        assert_eq!(resolve_market_slippage(None, 0.01), 0.01);
    }

    /// RULE 2: a usable band wins over the venue literal — including both endpoints of the range.
    #[test]
    fn a_usable_band_overrides_the_venue_default() {
        assert_eq!(resolve_market_slippage(Some(0.002), 0.05), 0.002);
        assert_eq!(resolve_market_slippage(Some(0.005), 0.05), 0.005);
        assert_eq!(resolve_market_slippage(Some(MIN_MARKET_SLIPPAGE), 0.05), MIN_MARKET_SLIPPAGE);
        assert_eq!(resolve_market_slippage(Some(MAX_MARKET_SLIPPAGE), 0.05), MAX_MARKET_SLIPPAGE);
    }

    /// RULE 3, the money half: "set it to 50% so orders always fill" is exactly the input this
    /// bound exists to refuse. It clamps to the ceiling — never through it.
    #[test]
    fn a_band_above_the_ceiling_is_clamped_never_honored() {
        for wide in [0.0500001, 0.1, 0.5, 50.0, f64::INFINITY] {
            assert_eq!(
                resolve_market_slippage(Some(wide), 0.05),
                MAX_MARKET_SLIPPAGE,
                "{wide} must clamp to the ceiling"
            );
        }
    }

    /// RULE 3, the other half: a band too tight to cross a normal book would cancel unfilled — on
    /// the stop-MARKET path, a protective exit that did not exit. It clamps up to the floor.
    #[test]
    fn a_band_below_the_floor_is_clamped_up() {
        for tight in [0.0, 1e-9, 0.0009, -0.01, -1.0, f64::NEG_INFINITY] {
            assert_eq!(
                resolve_market_slippage(Some(tight), 0.05),
                MIN_MARKET_SLIPPAGE,
                "{tight} must clamp to the floor"
            );
        }
    }

    /// NaN carries no intent and must never reach the price arithmetic (`mid * NaN` rounds to a
    /// zero price and the order is locally rejected). It degrades to the conservative end — NOT to
    /// `venue_default`, which is the widest band there is.
    #[test]
    fn nan_degrades_to_the_tight_end_not_to_the_default() {
        assert_eq!(resolve_market_slippage(Some(f64::NAN), 0.05), MIN_MARKET_SLIPPAGE);
        assert_ne!(resolve_market_slippage(Some(f64::NAN), 0.05), 0.05);
    }

    /// THE INVARIANT, machine-checked over every shape of input: a resolved band can only ever be
    /// the same as, or TIGHTER than, the venue's own default. The safe direction is down, and no
    /// operator value — valid, absurd, or malformed — can move it up.
    #[test]
    fn a_configured_band_can_only_tighten_never_widen() {
        let venue_default = 0.05; // = MAX_MARKET_SLIPPAGE, hyperliquid's historical literal
        for requested in [
            0.0,
            1e-9,
            0.0009,
            0.001,
            0.002,
            0.005,
            0.01,
            0.05,
            0.0500001,
            0.5,
            50.0,
            -0.01,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            let got = resolve_market_slippage(Some(requested), venue_default);
            assert!(got <= venue_default, "requested {requested} widened the band to {got}");
            assert!(got.is_finite(), "requested {requested} produced a non-finite band {got}");
            assert!(
                is_usable_market_slippage(got),
                "requested {requested} produced an out-of-range band {got}"
            );
        }
    }
}
