//! Profile-driven anchored WALK-FORWARD — the `[walkforward]` sibling of [`super::run_sweep`]'s
//! `[sweep]`.
//!
//! [`run_walkforward`] is assembly, not new engine code: it resolves the profile's strategy through
//! the SAME [`super::strategy_by_name`] registry, builds the SAME bar-lane [`crate::EngineParams`]
//! and loads the SAME bars as a plain [`super::run_backtest`] of that profile (both through the
//! shared `super::run` helpers, so a walk-forward can never disagree with a single run about what
//! the profile's `[engine]` means), and hands the series to the EXISTING
//! [`crate::walkforward::walk_forward_strategy`] runner.
//!
//! What that buys over the Studio's DTO-shaped walk-forward
//! (`vike_studio_core::run_walkforward_slice`, which the `RunWalkforward` wire verb serves): the
//! whole `[engine]` surface — the `fee` SCHEDULE, `[engine.impact]`, `[engine.resolution]`,
//! `[risk]`, `snap_to_properties`, `attach_funding` — instead of only the three flat
//! `cash`/`fee_rate`/`slippage` scalars a `WireEngineParams` can carry.
//!
//! BAR mode, ONE series, deliberately: [`crate::walkforward::walk_forward_strategy`] splits a
//! single `&[Bar]` series by INDEX. A tick or multi-symbol profile is a clean
//! [`HarnessError::Validation`], never a silent first-symbol fallback.

use std::sync::Arc;

use vike_data::HistStore;

use super::run::{bar_engine_params, load_profile_bars};
use super::{BacktestProfile, DataKind, HarnessError, report::periods_per_year, strategy_by_name};
use crate::StrategyEngine;
use crate::validation::WalkMode;
use crate::walkforward::{WalkForwardReport, walk_forward_strategy};

/// Walk `profile` forward over its `[walkforward].n_splits` ANCHORED out-of-sample windows and
/// return the stitched [`WalkForwardReport`].
///
/// Each window runs fresh at `engine.cash` and the OOS curves are rebased onto one running equity
/// (the [`walk_forward_strategy`] stitch). The mode is always `Anchored` and the annualization is
/// [`periods_per_year`] — the same two derivations the Studio's walk-forward makes, so a profile
/// cannot disagree with the Studio about what "walk-forward" means.
///
/// Errors: no `[walkforward]` table, a tick-mode or multi-series profile, an unresolvable strategy,
/// a store failure, or an empty bar slice — every one a clean [`HarnessError`], checked BEFORE the
/// window loop (the loop's closure cannot return one).
pub fn run_walkforward(
    profile: &BacktestProfile,
    // The same `Arc<dyn HistStore + Send + Sync>` seam every other harness entry point takes, so
    // this compiles against the trait with no DataFusion (the `hist-replay`/`datafusion-store`
    // split) and any backend can drive it.
    store: Arc<dyn HistStore + Send + Sync>,
) -> Result<WalkForwardReport, HarnessError> {
    let cfg = profile.walkforward.as_ref().ok_or_else(|| {
        HarnessError::Validation(
            "profile has no [walkforward] table — a walk-forward needs a split count, e.g. \
             `[walkforward]\nn_splits = 4`"
                .to_string(),
        )
    })?;
    // `validate` already rejects `n_splits = 0` at load; re-checked so a hand-built profile that
    // skipped `from_toml_str` cannot reach `walk_forward_splits` with a zero count.
    if cfg.n_splits == 0 {
        return Err(HarnessError::Validation(
            "walkforward.n_splits must be >= 1, got 0".to_string(),
        ));
    }
    if profile.data.kind != DataKind::Bar {
        return Err(HarnessError::Validation(
            "walk-forward is bar-mode only: the splitter divides ONE bar series by index, which a \
             tick replay does not produce — set data.kind = \"bar\""
                .to_string(),
        ));
    }

    // Pre-resolve BOTH per-window inputs so a bad strategy name / fee schedule / resolution sidecar
    // fails HERE with a real error rather than inside the window closure, which cannot return one.
    strategy_by_name(&profile.strategy.name, &profile.strategy.params)?;
    bar_engine_params(profile, &store)?;

    let mut series = load_profile_bars(profile, &store)?;
    if series.len() != 1 {
        return Err(HarnessError::Validation(format!(
            "walk-forward runs over ONE bar series — this profile resolves {}; name a single \
             symbol",
            series.len()
        )));
    }
    let (symbol, bars) = series.pop().expect("len == 1 checked above");
    if bars.is_empty() {
        return Err(HarnessError::Data(format!(
            "no bars for {symbol} ({}) in the profile's range",
            profile.data.interval
        )));
    }

    let cash = profile.engine.cash;
    let report = walk_forward_strategy(
        &bars,
        cfg.n_splits,
        WalkMode::Anchored,
        cash,
        periods_per_year(profile),
        |window| {
            // Both rebuilt per window: `EngineParams` is not `Clone` (it can carry a
            // `Box<dyn PositionSizer>` / properties closure) and each window needs a fresh
            // strategy. Both were proven constructible above, so neither `expect` can fire.
            let strategy = strategy_by_name(&profile.strategy.name, &profile.strategy.params)
                .expect("strategy pre-resolved above");
            let params = bar_engine_params(profile, &store).expect("engine params pre-built above");
            StrategyEngine::new(vec![(symbol.clone(), window.to_vec())], strategy, params).run()
        },
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
"#;

    fn profile(extra: &str) -> BacktestProfile {
        BacktestProfile::from_toml_str(&format!("{BASE}{extra}")).unwrap()
    }

    #[test]
    fn the_walkforward_section_parses_and_defaults_to_absent() {
        assert!(profile("").walkforward.is_none(), "no [walkforward] ⇒ None (unchanged profiles)");
        assert_eq!(profile("[walkforward]\nn_splits = 4\n").walkforward.unwrap().n_splits, 4);
    }

    #[test]
    fn a_zero_split_count_is_rejected_at_load() {
        let err = BacktestProfile::from_toml_str(&format!("{BASE}[walkforward]\nn_splits = 0\n"))
            .unwrap_err();
        assert!(matches!(err, HarnessError::Validation(ref m) if m.contains("n_splits")), "{err}");
    }

    #[test]
    fn an_unknown_walkforward_key_is_a_parse_error() {
        // `deny_unknown_fields` on the section: a typo'd knob fails at load rather than silently
        // doing nothing (the same contract every other profile section has).
        let err = BacktestProfile::from_toml_str(&format!(
            "{BASE}[walkforward]\nn_splits = 2\nsplits = 3\n"
        ))
        .unwrap_err();
        assert!(matches!(err, HarnessError::Parse(_)), "{err}");
    }

    /// Every runner test needs SOME `HistStore` handle (even the guards that return before touching
    /// it), and this crate has no DataFusion-free double — `vike_data::MemHistStore` lives behind a
    /// `test-support` feature vike-backtest does not dev-depend on, and hand-rolling one here would
    /// mean stubbing the whole trait. So the runner tests ride the `datafusion-store` lane over an
    /// EMPTY temp-dir store, exactly like `harness::run`'s own store-backed tests; the profile-shape
    /// tests above stay on the trait-only `hist-replay` lane.
    #[cfg(feature = "datafusion-store")]
    fn empty_store() -> (tempfile::TempDir, Arc<dyn HistStore + Send + Sync>) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn HistStore + Send + Sync> =
            Arc::new(vike_data::DataFusionHist::open(dir.path()).unwrap());
        (dir, store)
    }

    #[cfg(feature = "datafusion-store")]
    #[test]
    fn a_profile_without_the_section_is_a_clean_error() {
        let (_dir, store) = empty_store();
        let err = run_walkforward(&profile(""), store).unwrap_err();
        assert!(matches!(err, HarnessError::Validation(ref m) if m.contains("[walkforward]")));
    }

    #[cfg(feature = "datafusion-store")]
    #[test]
    fn a_tick_profile_is_a_clean_error() {
        let (_dir, store) = empty_store();
        let toml = BASE.replace("kind = \"bar\"", "kind = \"tick\"");
        let p = BacktestProfile::from_toml_str(&format!("{toml}[walkforward]\nn_splits = 2\n"))
            .unwrap();
        let err = run_walkforward(&p, store).unwrap_err();
        assert!(matches!(err, HarnessError::Validation(ref m) if m.contains("bar-mode only")));
    }

    /// An empty bar slice is a DATA error, not a silent zero-window report.
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn an_empty_bar_slice_is_a_data_error() {
        let (_dir, store) = empty_store();
        let err = run_walkforward(&profile("[walkforward]\nn_splits = 2\n"), store).unwrap_err();
        assert!(matches!(err, HarnessError::Data(ref m) if m.contains("no bars")), "{err}");
    }

    /// A cross-venue two-series slice cannot be walked forward (the splitter takes ONE series).
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn a_multi_series_profile_is_a_clean_error() {
        let (_dir, store) = empty_store();
        let toml = r#"
[data]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"
[[data.series]]
venue = "binance"
symbol = "BTCUSDT"
[[data.series]]
venue = "binance"
symbol = "ETHUSDT"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
[walkforward]
n_splits = 2
"#;
        let p = BacktestProfile::from_toml_str(toml).unwrap();
        let err = run_walkforward(&p, store).unwrap_err();
        assert!(matches!(err, HarnessError::Validation(ref m) if m.contains("ONE bar series")));
    }

    /// End-to-end over a REAL store: one window per split, a stitched curve, finite summary stats.
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn runs_one_window_per_split_over_a_real_store() {
        use vike_data::DataFusionHist;
        use vike_model::Bar;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let bars: Vec<Bar> = (0..240)
            .map(|i| {
                let close = 100.0 + i as f64 * 0.1;
                Bar {
                    ts: i as i64 * 1000,
                    open: close,
                    high: close,
                    low: close,
                    close,
                    volume: 1.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: None,
                }
            })
            .collect();
        store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

        let p = profile("[walkforward]\nn_splits = 4\n");
        let rep = run_walkforward(&p, store).unwrap();
        assert_eq!(rep.windows.len(), 4, "one OOS window per split");
        assert!(!rep.oos_equity_curve.is_empty(), "the stitched curve is populated");
        assert!(rep.oos_return.is_finite());
        assert!((0.0..=1.0).contains(&rep.wf_consistency));
    }
}
