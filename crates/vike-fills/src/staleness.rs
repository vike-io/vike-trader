//! Stale-price wait discipline for the fill path (opt-in; LEAN `FutureFillModel` analog).
//!
//! A market order must not fill against fill-forwarded or stale data; it WAITS for a fresh print.
//! On sparse feeds (Polymarket especially, where an outcome token can go minutes without a trade)
//! a repeated last price is not tradeable, and filling against it manufactures edge.
//!
//! Enabled ONLY by `vike_sim::EngineParams::max_price_staleness_ms`; `None` (the default) never
//! consults this module, so every fill lane is byte-identical to the pre-feature engine (the
//! parity/golden suites r1/r3/r4/r7_gate/engine_kernel_parity/properties_fills run with `None`).
//!
//! ## The three rules
//!
//! 1. **Fresh print** ([`is_fresh_print`]): `volume > 0.0` (a transaction happened) **or** a
//!    two-sided quote (`bid` AND `ask`: liquidity you can cross). A zero-volume, quote-less bar
//!    is the fill-forward shape this discipline refuses.
//! 2. **Which orders defer** ([`defers`]): `Market` and `MarketClose` ONLY, which fill at "the
//!    current price". `Limit` / `LimitClose` / `Stop` / `Trailing` (and the protective-stop lane)
//!    are price-CONDITIONAL: a repeated stale price cannot satisfy a condition it did not already
//!    satisfy, and gating a protective stop would strand live risk.
//! 3. **Stale** ([`is_stale`]): the age of the symbol's last fresh print STRICTLY exceeds the
//!    bound. Age `0` (this event is a print) always fills; a symbol that never printed is stale.
//!
//! A deferred order is not canceled, rejected or lost: it stays in `pending` and fills on the
//! symbol's next fresh print, at that print's price, unless it leaves the way any resting order
//! does. `SimBroker::stale_deferrals` (mirrored onto
//! `vike_analytics::BacktestResult::stale_deferrals`) counts deferrals, so a run that never got a
//! fresh print says so instead of silently trading nothing.
//!
//! ## The clock is EVENT time, never wall time
//!
//! Every age is `event_ts - last_print_ts`, both from the tape (`bar.ts` / `sub.ts` / `tick.ts`),
//! so a replay yields the same deferrals on any machine at any speed, as the golden suites need.
//!
//! ## Which tier this bites
//!
//! - **Bar (`StrategyEngine::run`, incl. the cash-gated lane): the target.** A resampled sparse
//!   series emits flat zero-volume bars. Freshness is recorded per symbol per step from the coarse
//!   bar BEFORE the fill phase, so a bar that is itself a print has age `0`.
//! - **Granular (`fill_pending_granular`): each SUB-BAR is a print**, recorded before its own fill
//!   pass with the age measured at `sub.ts`; recording only the coarse bar would make every later
//!   sub-bar of a fresh step spuriously stale. A quiet stretch *within* a step defers.
//! - **Tick (`StrategyEngine::run_ticks`): inert BY CONSTRUCTION, correctly.** Each fill event IS
//!   the symbol's just-recorded tick (age `0`); a `Tick::Trade` is a print whatever its size (some
//!   venues emit zero-size / index prints that project to `volume == 0.0` with no quote). The path
//!   already waits structurally; the gate stays wired in so the policy has ONE definition.
//! - **Book**: book events `continue` before the price/fill path; nothing to gate.

use vike_model::{Bar, OrderKind};

/// Does `event` carry genuine current price evidence? Rule 1 in the module doc.
#[inline]
pub fn is_fresh_print(event: &Bar) -> bool {
    event.volume > 0.0 || (event.bid.is_some() && event.ask.is_some())
}

/// Is this order kind subject to the wait discipline? Rule 2 in the module doc.
#[inline]
pub fn defers(kind: OrderKind) -> bool {
    matches!(kind, OrderKind::Market | OrderKind::MarketClose)
}

/// Stale iff the last fresh print is OLDER than the bound (rule 3); `last_print == None` (never
/// printed) is stale.
///
/// Strict (`age > bound`), so `max_price_staleness_ms: Some(0)` means "only fill on an event that
/// is ITSELF a fresh print", the tightest useful setting. A negative age (an out-of-order event
/// stamped before the last print) is never stale.
///
/// A NEGATIVE `bound` is clamped to `0`: read literally it would make even age `0` stale, a silent
/// total freeze from a plausible typo. `Some(-1)` therefore behaves as `Some(0)`.
#[inline]
pub fn is_stale(last_print: Option<i64>, now: i64, bound: i64) -> bool {
    let bound = bound.max(0);
    match last_print {
        None => true,
        Some(ts) => now.saturating_sub(ts) > bound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(volume: f64, bid: Option<f64>, ask: Option<f64>) -> Bar {
        Bar {
            ts: 1_000,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume,
            funding: None,
            bid,
            ask,
            symbol: None,
        }
    }

    fn plain(volume: f64) -> Bar {
        mk(volume, None, None)
    }

    #[test]
    fn traded_bar_is_a_print_flat_zero_volume_bar_is_not() {
        assert!(is_fresh_print(&plain(3.0)));
        assert!(!is_fresh_print(&plain(0.0)));
    }

    #[test]
    fn two_sided_quote_is_a_print_even_with_zero_volume() {
        assert!(is_fresh_print(&mk(0.0, Some(0.99), Some(1.01))));
        // one-sided is not crossable — not a print
        assert!(!is_fresh_print(&mk(0.0, Some(0.99), None)));
    }

    #[test]
    fn only_market_kinds_defer() {
        assert!(defers(OrderKind::Market));
        assert!(defers(OrderKind::MarketClose));
        for k in [OrderKind::Limit, OrderKind::LimitClose, OrderKind::Stop, OrderKind::Trailing] {
            assert!(!defers(k), "{k:?} must not defer");
        }
    }

    #[test]
    fn staleness_is_strict_and_unprinted_is_stale() {
        assert!(is_stale(None, 10, 1_000), "never printed = stale");
        assert!(!is_stale(Some(10), 10, 0), "age 0 fills even at the tightest bound");
        assert!(is_stale(Some(0), 10, 0), "age 10 > bound 0");
        assert!(!is_stale(Some(0), 1_000, 1_000), "age == bound is NOT stale (strict)");
        assert!(is_stale(Some(0), 1_001, 1_000));
        assert!(!is_stale(Some(50), 10, 0), "out-of-order event is never stale");
    }

    #[test]
    fn negative_bound_is_clamped_to_zero_not_a_total_freeze() {
        // read literally, `age > -1` would make even age 0 stale => nothing ever fills
        assert!(!is_stale(Some(10), 10, -1), "age 0 still fills under a negative bound");
        assert!(!is_stale(Some(10), 10, i64::MIN));
        // and it is exactly `Some(0)`: anything older than the event itself is still stale
        assert!(is_stale(Some(9), 10, -1));
        assert_eq!(is_stale(Some(9), 10, -1), is_stale(Some(9), 10, 0));
    }
}
