//! The harness's view of the metrics summary (Task 4): [`BacktestReport`] and its annualization
//! constants, plus the ONE piece that genuinely needs the (feature-gated) profile parser —
//! [`periods_per_year`].
//!
//! The report struct itself lives in `crate::report`, OUTSIDE the `hist-replay` feature: its
//! composition needs only `serde` + `crate::metrics`, so gating it here would have forced every
//! DataFusion-free consumer to re-assemble its own copy (which is exactly what `vike-report`'s
//! `LiveTearsheet` used to do). Everything is re-exported below, so `harness::report::*` and
//! `harness::BacktestReport` paths are unchanged for the `backtest` bin and `harness::sweep`.

pub use crate::report::{
    BacktestReport, DAILY_PERIODS_PER_YEAR, DEFAULT_PERIODS_PER_YEAR, periods_per_year_for_interval,
};

use super::profile::{BacktestProfile, DataKind};

/// The `periods_per_year` to feed [`BacktestReport::from_result`] for a profile — SINGLE source of
/// truth so the single-run bin and the sweep rank the SAME profile on the same Sharpe scale.
///
/// The only profile-coupled part of the report, and therefore the only part that stays behind
/// `hist-replay` (it names the gated [`BacktestProfile`]).
///
/// # This is a WRAPPER, not the derivation
///
/// The interval -> observations-per-year scale, the reason it is derived rather than matched
/// against `"1d"`, the preserved daily anchor and the unparseable-interval fallback all live on
/// [`periods_per_year_for_interval`] in vike-analytics — beside the constants they scale, and
/// BELOW both planes that need them. This function adds exactly one thing that genuinely needs
/// the gated profile parser: the TICK branch. A tick stream has no fixed period, so there is no
/// honest observation count to derive and it takes [`DEFAULT_PERIODS_PER_YEAR`] outright — and
/// only a caller holding a `BacktestProfile` knows it is holding ticks.
///
/// ⚠ This doc used to carry the whole derivation AND the claim that the Studio's walk-forward
/// made "the same two derivations". The MODE half was true; the annualization half was false —
/// the Studio passed a bare `252.0`. The cure was the workspace's own: one home below both.
pub fn periods_per_year(profile: &BacktestProfile) -> f64 {
    let DataKind::Bar = profile.data.kind else {
        return DEFAULT_PERIODS_PER_YEAR;
    };
    periods_per_year_for_interval(&profile.data.interval)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal BAR profile at `interval`. Only the two fields `periods_per_year` reads
    /// are meaningful; the rest is the smallest thing that parses.
    fn bar_profile(interval: &str) -> BacktestProfile {
        let toml = format!(
            r#"
name = "ppy"
[data]
kind = "bar"
interval = "{interval}"
from = "0"
to = "1"
venue = "binance"
symbols = ["BTCUSDT"]
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#
        );
        toml::from_str(&toml).expect("fixture profile must parse")
    }

    /// The anchor does not move: a daily run reports exactly what it always did.
    #[test]
    fn a_daily_bar_profile_still_returns_the_daily_constant_exactly() {
        assert_eq!(
            periods_per_year(&bar_profile("1d")).to_bits(),
            DAILY_PERIODS_PER_YEAR.to_bits(),
            "the 1d anchor must be bit-identical to its pre-fix value"
        );
    }

    /// The bug this function exists to prevent: an intraday interval annualized as though daily.
    #[test]
    fn intraday_intervals_scale_off_the_daily_anchor_instead_of_collapsing_onto_it() {
        // 1440 one-minute bars per day, 24 one-hour bars per day.
        assert_eq!(periods_per_year(&bar_profile("1m")), 252.0 * 1440.0);
        assert_eq!(periods_per_year(&bar_profile("1h")), 252.0 * 24.0);
        assert_eq!(periods_per_year(&bar_profile("5m")), 252.0 * 288.0);

        // The regression itself: 1m must NOT equal the daily constant.
        assert_ne!(
            periods_per_year(&bar_profile("1m")),
            DAILY_PERIODS_PER_YEAR,
            "a 1m run annualized at 252 understates Sharpe by sqrt(1440)"
        );
    }

    /// Sharpe scales by sqrt(periods_per_year), so this pins the SIZE of the correction the fix
    /// applies — the number quoted in the doc comment above.
    #[test]
    fn the_one_minute_correction_is_sqrt_1440() {
        let ratio = periods_per_year(&bar_profile("1m")) / DAILY_PERIODS_PER_YEAR;
        assert_eq!(ratio, 1440.0);
        assert!(
            (ratio.sqrt() - 37.947).abs() < 0.001,
            "sharpe was understated by ~37.9x, got {}",
            ratio.sqrt()
        );
    }

    /// Longer-than-daily intervals scale DOWN off the same anchor — the arithmetic is not
    /// intraday-only.
    #[test]
    fn a_weekly_interval_scales_down_off_the_same_anchor() {
        assert_eq!(periods_per_year(&bar_profile("7d")), 252.0 / 7.0);
    }

    /// An unparseable interval must fall back, never fabricate a scale from a bad parse.
    #[test]
    fn an_unparseable_interval_falls_back_rather_than_fabricating_a_scale() {
        for bad in ["", "m", "1x", "xm", "-1m"] {
            assert_eq!(
                periods_per_year(&bar_profile(bad)),
                DEFAULT_PERIODS_PER_YEAR,
                "interval {bad:?} must fall back"
            );
        }
    }
}
