//! Underlying-mark TRACKER for the cross-symbol routing ("Option B") — the pure state that turns a
//! stream of RTDS underlying-spot marks (a DIFFERENT symbol than the PM outcome token the maker is
//! mounted on) into the `(s_now, s_open, sigma_per_sec)` triple the A-S underlying-anchored fair mid
//! ([`AsParams::underlying_weight`]) and ATM guard ([`AsParams::atm_blackout_scale`]) consume via
//! [`crate::avellaneda::AsState::set_underlying`]. Net-new Rust surface — no Python twin.
//!
//! It mirrors [`AsState`](crate::avellaneda::AsState)'s online-σ̂² STYLE (the [`ewma_alpha`] +
//! [`update_sigma2`] recurrence) but on LOG-RETURNS normalized PER SECOND, because
//! [`vike_model::p_up`] — the model the anchored fair mid calls — reads `sigma` as the per-second
//! log-return stddev. `s_open` is the window-open reference: the first mark at/after the boundary
//! `window_open_ms = resolution_ts − window_secs·1000`, RESET whenever a later `resolution_ts` opens
//! a new window (the 5-minute up/down markets roll). Everything is event-time driven (never
//! wall-clock) and every update is O(1), matching the maker's own estimator discipline.
//!
//! [`AsParams`]: vike_model::AsParams

use crate::avellaneda::{ewma_alpha, update_sigma2};

/// EWMA half-life (in updates) for the per-second log-return variance. A fixed, mount-structural
/// smoother — the maker never re-tunes it (the A-S `sigma_half_life` tracks the PM book, this tracks
/// the underlying). Mirrors the A-S σ̂² default half-life.
const UNDERLYING_SIGMA_HALF_LIFE: f64 = 32.0;

/// Online tracker turning an underlying-spot mark stream into `(s_now, s_open, sigma_per_sec)` for
/// [`AsState::set_underlying`](crate::avellaneda::AsState::set_underlying). Held on [`SpreadMaker`](crate::SpreadMaker)
/// as a plain `&mut self` field (mount-structural, like `own_book` — NOT part of
/// [`SpreadMakerParams`](vike_model::SpreadMakerParams)); the single-writer core means no lock.
pub(crate) struct UnderlyingTracker {
    /// Precomputed EWMA α from [`UNDERLYING_SIGMA_HALF_LIFE`] (`α = 1 − 0.5^(1/half_life)`).
    alpha: f64,
    /// The window-open reference spot `s_open` — the first mark seen at/after the current window's
    /// open boundary. `None` until the first mark inside a window; reset on a window rollover.
    s_open: Option<f64>,
    /// The window-open boundary (epoch-ms) the current `s_open` was captured for — a later
    /// `resolution_ts` moves it forward and triggers the rollover reset. `None` until the first
    /// observe.
    window_open_ms: Option<i64>,
    /// Previous `(log_price, ts)` for the per-second log-return σ̂² increment; `None` until the first
    /// mark.
    last: Option<(f64, i64)>,
    /// Online per-second log-return variance σ̂²; `None` until seeded by the first usable increment.
    sigma2: Option<f64>,
}

impl UnderlyingTracker {
    /// A cold tracker: no window reference, no σ estimate. Constructed in [`SpreadMaker::new`](crate::SpreadMaker::new).
    pub(crate) fn new() -> Self {
        UnderlyingTracker {
            alpha: ewma_alpha(UNDERLYING_SIGMA_HALF_LIFE),
            s_open: None,
            window_open_ms: None,
            last: None,
            sigma2: None,
        }
    }

    /// Fold one underlying mark and, once warm, yield `(s_now, s_open, sigma_per_sec)` for
    /// [`AsState::set_underlying`](crate::avellaneda::AsState::set_underlying):
    /// - `s_open` is captured as the FIRST mark at/after `window_open_ms = resolution_ts −
    ///   window_secs·1000`, and RESET when a later `resolution_ts` opens a new window (σ̂² re-warms
    ///   on its own across the roll — the underlying's volatility is not window-scoped);
    /// - `sigma_per_sec = √σ̂²`, the EWMA per-second log-return stddev (the [`update_sigma2`] STYLE on
    ///   log-returns normalized by the inter-mark seconds).
    ///
    /// Returns `None` — feeds NOTHING to the A-S state — until it has BOTH a window-open reference AND
    /// a σ estimate, and whenever no window can be defined (`window_secs <= 0`, no `resolution_ts`, or
    /// a non-finite / non-positive price): in every such case the anchored fair mid / ATM guard cannot
    /// fire anyway (they require the time-to-resolution regime), so there is nothing to feed. O(1).
    pub(crate) fn observe(
        &mut self,
        price: f64,
        ts: i64,
        window_secs: f64,
        resolution_ts: Option<i64>,
    ) -> Option<(f64, f64, f64)> {
        // A window must be definable to anchor `s_open`; without one the blend/ATM guard can't fire.
        if window_secs <= 0.0 || !price.is_finite() || price <= 0.0 {
            return None;
        }
        let t_res = resolution_ts?;
        // window-open boundary; a later resolution_ts (a new market/window) moves it forward.
        let window_open_ms = t_res - (window_secs * 1000.0) as i64;
        if self.window_open_ms != Some(window_open_ms) {
            // rolled into a NEW window — drop the stale open reference (σ̂² re-warms on its own).
            self.window_open_ms = Some(window_open_ms);
            self.s_open = None;
        }
        // capture the window-open reference the first time a mark lands at/after the boundary.
        if self.s_open.is_none() && ts >= window_open_ms {
            self.s_open = Some(price);
        }
        // advance the per-second log-return σ̂² (the `AsState::update_sigma` STYLE, on log-returns/sec).
        let logp = libm::log(price);
        if let Some((logp_prev, ts_prev)) = self.last {
            let dt_ms = (ts - ts_prev) as f64;
            if dt_ms > 0.0 {
                let dt_s = dt_ms / 1000.0;
                let d = logp - logp_prev;
                self.sigma2 = Some(match self.sigma2 {
                    Some(prev) => update_sigma2(prev, d, dt_s, self.alpha),
                    None => d * d / dt_s, // seed with the first instantaneous per-second σ̂²
                });
            }
        }
        self.last = Some((logp, ts));
        // ready only once we have BOTH a window-open reference AND a σ estimate.
        match (self.s_open, self.sigma2) {
            (Some(s_open), Some(sigma2)) => Some((price, s_open, sigma2.sqrt())),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 300 s window closing at T = 300_000 ms ⇒ window_open_ms = 0.
    const WINDOW_SECS: f64 = 300.0;
    const T_RES: i64 = 300_000;

    #[test]
    fn observe_captures_s_open_and_warms_a_positive_sigma() {
        let mut u = UnderlyingTracker::new();
        // first mark: captures s_open (ts >= 0), but no σ yet (no previous increment) ⇒ None.
        assert_eq!(u.observe(100.0, 1_000, WINDOW_SECS, Some(T_RES)), None);
        // second mark at a DIFFERENT price seeds σ̂² ⇒ Some, with s_open pinned to the FIRST price.
        let (s_now, s_open, sigma) =
            u.observe(101.0, 2_000, WINDOW_SECS, Some(T_RES)).expect("warm after two marks");
        assert_eq!(s_now.to_bits(), 101.0_f64.to_bits(), "s_now is the latest mark");
        assert_eq!(
            s_open.to_bits(),
            100.0_f64.to_bits(),
            "s_open is the first at/after the boundary"
        );
        assert!(
            sigma > 0.0,
            "a real log-return move produces a positive per-second σ, got {sigma}"
        );
    }

    #[test]
    fn observe_rolls_the_window_open_reference_forward() {
        let mut u = UnderlyingTracker::new();
        u.observe(100.0, 1_000, WINDOW_SECS, Some(T_RES));
        u.observe(101.0, 2_000, WINDOW_SECS, Some(T_RES));
        // still the SAME window ⇒ s_open unchanged at 100.
        let (_, s_open, _) = u.observe(102.0, 3_000, WINDOW_SECS, Some(T_RES)).expect("warm");
        assert_eq!(s_open.to_bits(), 100.0_f64.to_bits(), "same window keeps its open reference");
        // a LATER resolution_ts opens a NEW window (open boundary 300_000): the reference resets and
        // re-captures at the first mark at/after the new boundary.
        let next_res = 600_000;
        let (_, s_open2, sigma2) =
            u.observe(200.0, 301_000, WINDOW_SECS, Some(next_res)).expect("still warm across roll");
        assert_eq!(s_open2.to_bits(), 200.0_f64.to_bits(), "rollover re-captures the new open");
        assert!(sigma2 > 0.0, "σ re-warms across the roll (carried, not reset)");
    }

    #[test]
    fn observe_feeds_nothing_without_a_definable_window() {
        let mut u = UnderlyingTracker::new();
        // no resolution_ts ⇒ nothing to anchor ⇒ None (twice, to prove it never latches).
        assert_eq!(u.observe(100.0, 1_000, WINDOW_SECS, None), None);
        assert_eq!(u.observe(101.0, 2_000, WINDOW_SECS, None), None);
        // window_secs <= 0 ⇒ None even with a resolution_ts.
        assert_eq!(u.observe(100.0, 1_000, 0.0, Some(T_RES)), None);
        // a non-positive / non-finite price ⇒ None (defensive: ln would be undefined).
        assert_eq!(u.observe(0.0, 1_000, WINDOW_SECS, Some(T_RES)), None);
        assert_eq!(u.observe(f64::NAN, 1_000, WINDOW_SECS, Some(T_RES)), None);
    }

    #[test]
    fn a_mark_before_the_window_opens_holds_the_open_reference() {
        // window opens at 300_000 (T_RES 600_000, 300 s window); a mark BEFORE it sets no s_open.
        let mut u = UnderlyingTracker::new();
        assert_eq!(
            u.observe(100.0, 100_000, WINDOW_SECS, Some(600_000)),
            None,
            "pre-window ⇒ no open"
        );
        // σ warms, but still no s_open ⇒ still None…
        assert_eq!(
            u.observe(101.0, 200_000, WINDOW_SECS, Some(600_000)),
            None,
            "warm σ, still no open"
        );
        // …until a mark lands at/after the boundary, which captures s_open and yields the triple.
        let (_, s_open, _) =
            u.observe(105.0, 300_500, WINDOW_SECS, Some(600_000)).expect("open captured now");
        assert_eq!(
            s_open.to_bits(),
            105.0_f64.to_bits(),
            "s_open is the first mark at/after the open"
        );
    }
}
