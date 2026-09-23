//! `fixture_bars` — the committed fixture study the pipeline test drives.
//!
//! This file is NOT an example for humans — it exists so CI proves the
//! scan→generate→compile→run pipeline on every run, in checkouts that have no real `user_data/`.
//! It follows the entry-file contract exactly: a folder-named file exporting `run`.
//!
//! What it deliberately EXERCISES, one dependency per step, so the manifest's claim to be the
//! user-study API surface is checked by a compiler rather than asserted in prose:
//!   * `vike_user_research::StudyContext` — a store read, through the window it was handed;
//!   * `vike_data::TsRange` — the range type those reads take;
//!   * `vike_model::py_sum` — the compensated fold this workspace requires wherever a sum order
//!     could matter;
//!   * `vike_indicators::feature::ColumnFeature` — a USER-AUTHORED feature, the study↔strategy
//!     seam, checked by the platform's own parity harness from `tests/pipeline.rs`;
//!   * `vike_analytics::signal_backtest` — the number a study exists to produce.

use vike_analytics::signal_backtest::sharpe_from_bar_pnl;
use vike_analytics::DEFAULT_PERIODS_PER_YEAR;
use vike_indicators::feature::{ColumnFeature, WindowFeature, WindowStat};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

/// A feature a user WROTE — how far each close sits above its own trailing mean.
///
/// Composed over `WindowFeature::point_in_time`, the preset whose window EXCLUDES the current row,
/// so the number a model trains on cannot contain the row it is predicting. `reach` delegates to
/// the inner feature: this arithmetic reads the current row and nothing further back than the
/// window does, and an UNDER-declared reach is exactly the study→strategy divergence the harness in
/// `crates/vike-indicators/src/test_support.rs`'s `assert_feature_parity` exists to catch.
pub struct MeanGap {
    inner: WindowFeature,
}

impl MeanGap {
    pub fn new(period: usize) -> Self {
        Self { inner: WindowFeature::point_in_time(WindowStat::Mean, period) }
    }
}

impl ColumnFeature for MeanGap {
    fn vectorize(&self, x: &[f64]) -> Vec<f64> {
        let mean = self.inner.vectorize(x);
        x.iter().zip(mean).map(|(v, mu)| v - mu).collect()
    }
    fn reach(&self) -> usize {
        self.inner.reach()
    }
}

/// Lenient param reading, the same idiom as the strategy tier's `build` and the built-in
/// `from_params` arms: `params.get(..)` plus a default, never a required key.
fn str_param<'a>(params: &'a toml::Value, key: &str, default: &'a str) -> &'a str {
    params.get(key).and_then(|v| v.as_str()).unwrap_or(default)
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let venue = str_param(params, "venue", "binance");
    let symbol = str_param(params, "symbol", "BTCUSDT");
    let interval = str_param(params, "interval", "1m");
    let period = params.get("period").and_then(|v| v.as_integer()).unwrap_or(2).max(1) as usize;

    // `?` straight through: `From<DataError> for StudyError` exists for exactly this.
    let bars = ctx.bars(venue, symbol, interval, ctx.window())?;
    if bars.is_empty() {
        return Err(StudyError::Study(format!(
            "no bars for {venue}/{symbol}/{interval} in the requested window — the study window \
             is not covered by the store"
        )));
    }

    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let mean = py_mean(&closes);
    let gap = MeanGap::new(period).vectorize(&closes);

    // A one-bar-lag return series, the crudest possible signal, so the Sharpe below is a real
    // call into vike-analytics rather than a constant.
    let rets: Vec<f64> = closes.windows(2).map(|w| (w[1] - w[0]) / w[0]).collect();
    let sharpe = sharpe_from_bar_pnl(&rets, DEFAULT_PERIODS_PER_YEAR);

    let mut out = StudyOutcome::new();
    out.metric("bars", bars.len() as f64)?;
    out.metric("mean_close", mean)?;
    out.metric("sharpe", sharpe)?;

    let mut tsv = String::from("ts\tclose\tmean_gap\n");
    for (bar, g) in bars.iter().zip(gap.iter()) {
        tsv.push_str(&format!("{}\t{}\t{}\n", bar.ts, bar.close, g));
    }
    out.artifact("closes.tsv", tsv)?;
    Ok(out)
}

/// `py_sum` because a mean over a column is exactly the fold whose order this workspace pins.
fn py_mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    vike_model::py_sum(xs.iter().copied()) / xs.len() as f64
}
