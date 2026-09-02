//! The harness's view of the metrics summary (Task 4): [`BacktestReport`] and its annualization
//! constants, plus the ONE piece that genuinely needs the (feature-gated) profile parser —
//! [`periods_per_year`].
//!
//! The report struct itself lives in `crate::report`, OUTSIDE the `hist-replay` feature: its
//! composition needs only `serde` + `crate::metrics`, so gating it here would have forced every
//! DataFusion-free consumer to re-assemble its own copy (which is exactly what `vike-report`'s
//! `LiveTearsheet` used to do). Everything is re-exported below, so `harness::report::*` and
//! `harness::BacktestReport` paths are unchanged for the `backtest` bin and `harness::sweep`.

pub use crate::report::{BacktestReport, DAILY_PERIODS_PER_YEAR, DEFAULT_PERIODS_PER_YEAR};

use super::profile::{BacktestProfile, DataKind};

/// Milliseconds in one 24-hour day — the unit `interval_ms` is counted in below.
const MS_PER_DAY: f64 = 86_400_000.0;

/// The `periods_per_year` to feed [`BacktestReport::from_result`] for a profile — SINGLE source of
/// truth so the single-run bin and the sweep rank the SAME profile on the same Sharpe scale.
///
/// The only profile-coupled part of the report, and therefore the only part that stays behind
/// `hist-replay` (it names the gated [`BacktestProfile`]).
///
/// # Why this is DERIVED from the interval rather than matched against `"1d"`
///
/// Sharpe annualizes by `sqrt(periods_per_year)`, where `periods_per_year` must be the number of
/// RETURN OBSERVATIONS a year produces — one per bar. This used to return the daily 252 for
/// `"1d"` and the same 252 for everything else, so a 1-minute run was annualized as though its
/// bars were daily: 1,440 bars per day counted as one. That understated every intraday Sharpe by
/// `sqrt(1440) ≈ 37.9x` — silently, because the number still looked like a Sharpe. It also made
/// `--rank-by sharpe` incoherent across a sweep whose points differ in interval.
///
/// The daily anchor is PRESERVED exactly (`"1d"` still returns [`DAILY_PERIODS_PER_YEAR`], so no
/// existing daily report moves by a single bit) and every other interval scales off it by how
/// many of that interval fit in a day: `252 · (86_400_000 / interval_ms)`. So `1h` -> 6,048 and
/// `1m` -> 362,880.
///
/// Two deliberate limits, stated rather than hidden:
///
/// * The 252 anchor is the LEAN/tearsheet EQUITY convention (252 trading days). These markets
///   trade 24/7, so a defensible crypto anchor is 365. Changing it would move every existing
///   daily report, which is a separate decision from fixing the intraday scale — so 252 stays and
///   the intraday values inherit it.
/// * A tick profile still gets [`DEFAULT_PERIODS_PER_YEAR`]: a tick stream has no fixed period, so
///   there is no honest observation count to derive. Unchanged.
///
/// An interval [`vike_model::time::interval_ms`] cannot parse (or a non-positive one) falls back
/// to [`DEFAULT_PERIODS_PER_YEAR`] rather than fabricating a scale — this is a reporting knob, not
/// a validation site, and the profile parser is what rejects a malformed interval.
pub fn periods_per_year(profile: &BacktestProfile) -> f64 {
    let DataKind::Bar = profile.data.kind else {
        return DEFAULT_PERIODS_PER_YEAR;
    };
    match vike_model::time::interval_ms(&profile.data.interval) {
        Some(ms) if ms > 0 => DAILY_PERIODS_PER_YEAR * (MS_PER_DAY / ms as f64),
        _ => DEFAULT_PERIODS_PER_YEAR,
    }
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
