//! Stale-price wait discipline for the fill path (opt-in; LEAN `FutureFillModel` analog).
//!
//! A market order must not fill against fill-forwarded or otherwise stale data — it should
//! WAIT for a fresh print. On sparse feeds (Polymarket especially, where an outcome token can
//! go minutes without a trade) a repeated last price is not a tradeable price, and filling
//! against it manufactures edge that never existed.
//!
//! Enabled ONLY by `vike_backtest::EngineParams::max_price_staleness_ms`. `None` (the default)
//! means this module is never consulted and every fill lane is byte-identical to the pre-feature
//! engine — the parity/golden suites (r1/r3/r4/r7_gate/engine_kernel_parity/properties_fills)
//! all run with `None`.
//!
//! ## The three rules, stated once
//!
//! 1. **What counts as a fresh print** ([`is_fresh_print`]) — an event carrying genuine current
//!    price evidence: `volume > 0.0` (a real transaction happened in this event/period) **or** a
//!    live two-sided quote (`bid` AND `ask` present — resting liquidity you can actually cross).
//!    A zero-volume, quote-less bar is precisely the fill-forward/no-trade shape, and is the
//!    thing this discipline exists to refuse.
//!
//! 2. **Which orders defer** ([`defers`]) — `Market` and `MarketClose` ONLY. These are the kinds
//!    that resolve at "whatever the current price is", so a stale price silently becomes their
//!    fill price. `Limit` / `LimitClose` / `Stop` / `Trailing` (and the protective-stop lane) are
//!    price-CONDITIONAL: a repeated stale price cannot spuriously satisfy a condition it did not
//!    already satisfy, and gating a protective stop would strand live risk. They pass through
//!    untouched.
//!
//! 3. **When a price is stale** ([`is_stale`]) — the age of the symbol's last fresh print,
//!    measured against the event clock, STRICTLY exceeds the bound. Age `0` (this very event is a
//!    print) therefore always fills. A symbol that has not printed AT ALL yet is stale by
//!    definition (there is no evidence of a tradeable price), so its market orders keep waiting.
//!
//! A deferred order is NOT canceled and NOT rejected, and it is never lost: it stays in
//! `pending` exactly as it was and is retried on the very next event for that symbol, so it
//! fills on the next fresh print, at that print's price. The only way a deferred order stops
//! resting is the way any resting order does — the strategy cancels it, or an opt-in
//! resolution/expiry lane retires it. `SimBroker::stale_deferrals` (mirrored onto
//! `vike_backtest::BacktestResult::stale_deferrals`, so the `harness`/bin entry points can see it
//! too) counts how often a deferral happened, so a run that never got a fresh print says so instead
//! of silently producing zero trades.
//!
//! ## The clock is EVENT time, never wall time
//!
//! Every age is `event_ts - last_print_ts`, both drawn from the tape (`bar.ts` / `sub.ts` /
//! `tick.ts`). Nothing reads a system clock, so a replay of the same tape yields the same
//! deferrals on any machine at any speed — determinism the golden/parity suites depend on.
//!
//! ## Which tier this actually bites
//!
//! - **Bar tier (`StrategyEngine::run`, incl. the cash-gated lane): the target.** Forward-filled
//!   / no-trade bars are real here — a resampled sparse series emits flat zero-volume bars, and
//!   today a market order fills against them. Freshness is recorded once per symbol per step
//!   from that step's coarse bar, immediately BEFORE the fill phase, so a bar that is itself a
//!   print has age `0` and fills on the spot.
//! - **Granular sub-bar lane (`fill_pending_granular`): each SUB-BAR is a first-class print.**
//!   When granular data is supplied it is the finest tier available, so its own volume/quote
//!   evidence is the best print evidence there is; the sub-bar is recorded before its own fill
//!   pass and the age is measured at `sub.ts`. (Recording only the coarse bar while measuring at
//!   `sub.ts` would make every later sub-bar of a fully-fresh step spuriously stale — the age
//!   would grow monotonically across the step.) A fully-printing granular step therefore never
//!   defers; a quiet stretch *within* a step does, which is the point of having the finer tier.
//! - **Tick tier (`StrategyEngine::run_ticks`): inert BY CONSTRUCTION, and that is correct.**
//!   Each fill event there IS the symbol's own just-arrived tick, recorded as a print
//!   immediately before the fill phase, so the age is always `0`. Freshness on that lane is
//!   decided from the tick KIND, not only from the projected bar: a `Tick::Trade` is a
//!   transaction and counts as a print regardless of its reported size (some venues emit
//!   zero-size / index prints, which project to `volume == 0.0` with no quote). The tick path
//!   already waits for fresh data structurally — an order rests in `pending` until the symbol's
//!   next tick arrives. The gate is still wired into that lane so the policy has ONE definition
//!   rather than two.
//! - **Book tier**: book events never reach the price/fill path at all (they `continue` before
//!   it), so there is nothing to gate.

use vike_model::{Bar, OrderKind};

/// Does `event` carry genuine current price evidence? See rule 1 in the module doc.
///
/// `volume > 0.0` = a transaction printed. `bid`+`ask` = crossable two-sided liquidity. A
/// zero-volume quote-less bar is the fill-forward shape and is NOT a print.
#[inline]
pub fn is_fresh_print(event: &Bar) -> bool {
    event.volume > 0.0 || (event.bid.is_some() && event.ask.is_some())
}

/// Is this order kind subject to the wait discipline? See rule 2 in the module doc — takers that
/// resolve at "the current price" (`Market`, `MarketClose`) defer; price-conditional kinds do not.
#[inline]
pub fn defers(kind: OrderKind) -> bool {
    matches!(kind, OrderKind::Market | OrderKind::MarketClose)
}

/// Stale iff the last fresh print is OLDER than the bound (rule 3). `last_print` is `None` when
/// the symbol has never printed — stale, since no tradeable price has been observed at all.
///
/// The comparison is strict (`age > bound`), so `max_price_staleness_ms: Some(0)` means "only
/// fill on an event that is ITSELF a fresh print" — the tightest useful setting. A negative age
/// (an out-of-order event stamped before the last print) is never stale.
///
/// A NEGATIVE `bound` is clamped to `0` rather than taken literally: read literally it would make
/// even age `0` stale, i.e. nothing could ever fill — a silent total freeze from a plausible
/// typo. `Some(0)` is therefore the floor of the setting, and `Some(-1)` behaves as `Some(0)`.
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
