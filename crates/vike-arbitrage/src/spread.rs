//! The spread of a pair, its running signals as DATA, and the rows of a model of its reversion: the book's chapter
//! 37 from the spread on.
//!
//! **The signals are `vike-indicators`' own numbers.** [`SpreadSignals`] holds a spread with its rolling z-score
//! (how far the spread sits from its recent mean, in standard deviations), its rolling half-life (how many bars a
//! deviation takes to decay by half, `NaN` where the window does not mean-revert) and its rolling deviation.
//! [`signals`] is the ONE copy of the glue that makes it: `vike_indicators::series::{spread_zscore, half_life}` and
//! `vike_math::rolling::rolling_std`, called and never repeated, so a strategy that reads the same indicators by
//! name sees the numbers a model was trained on, bit for bit. A caller with its own signals (a Kalman hedge ratio's
//! spread, say) fills a [`SpreadSignals`] itself with [`SpreadSignals::new`]. The Kalman hedge ratio is
//! `vike_indicators::series::kalman_beta`, which a caller reads directly: it is one call.
//!
//! The spread is `first − hedge_ratio · second` WITHOUT an intercept, as the indicators define it: a constant moves
//! neither the z-score (it is relative to the window's own mean) nor the half-life (the regression has an
//! intercept). Pass LOG prices ([`log_prices`]) and the hedge ratio of [`crate::cointegration::engle_granger`] for
//! the spread of a cointegrated pair.
//!
//! # The data of a model of spread reversion
//!
//! A row of features per bar, the label that says what the spread did next, and the profit of trading on a
//! prediction. No model family is added here: the rows go to the workspace's learners
//! (`vike_ml_training::linear::fit_linear`, LightGBM, the Gaussian process), and a fitted model is run in a strategy
//! file by `vike-ml-prediction`.
//!
//! **The question.** A spread that deviates from its mean will, on average, come back. How far, and how fast, differs
//! from bar to bar with how stretched the spread is, how fast it is moving and how fast it has been reverting.
//! The label is the spread's change over the next `horizon` bars in units of its own recent deviation, so a
//! pair of any price scale gives labels on one scale; a model that predicts it is a model of reversion
//! when its prediction has the opposite sign of the z-score.
//!
//! **The features** ([`FEATURE_NAMES`], one row per bar, every one from the spread's trailing window and no
//! later bar):
//!
//! - `zscore` and `absolute_zscore`: how stretched the spread is;
//! - `zscore_change`: its move over the last bar, and `zscore_momentum`: over the last `momentum` bars, so a
//!   model can tell a spread falling back from one still running away;
//! - `reversion_speed`: `1 / half_life`, in reversions per bar, with `0` where the window has no finite
//!   half-life (`NaN` there means "not mean-reverting", which IS a speed of zero, unlike a missing value);
//!
//! and any extra columns the caller supplies (chapter 38 adds the funding rate and the basis). An extra column
//! must itself be point in time: the caller aligns it to the bar the row is for. A row with a missing value
//! anywhere is dropped, never filled.
//!
//! **Point in time, and the same row live.** The z-score and the half-life are the indicators' own, whose value at a
//! bar reads that bar and the ones before it, so [`reversion_features`] run over the last bars of a strategy's window
//! gives the last row the study trained a model on. A label reads `horizon` bars AHEAD by definition: a row's label
//! is known only `horizon` bars after the row, and a walk-forward split must leave that gap (purge and embargo it).
//!
//! **Trading on a prediction** ([`step_pnl`]): one decision per `horizon` bars, never overlapping, each a position
//! in `[-1, 1]` held for the next `horizon` bars. The profit is gross, in the spread's own units (log price when
//! the spread is of log prices), per unit of the first leg's notional; what the trades cost is a backtest's, charged
//! the one cost type (`vike_model::Costs`), never a rate of this crate's.

use vike_indicators::series;
use vike_math::rolling::{WindowSpec, rolling_std};

/// The natural log of each price (`libm`, decision 0032: a study file cannot name `libm` itself); `NaN` where a
/// price is not a positive finite number.
pub fn log_prices(prices: &[f64]) -> Vec<f64> {
    prices.iter().copied().map(vike_math::time_series::log_of_positive).collect()
}

/// A spread and its rolling signals, one value per bar, `NaN` while a window is not full. Every vector has the
/// spread's length; [`reversion_features`] and [`reversion_dataset`] read them.
#[derive(Clone, Debug, PartialEq)]
pub struct SpreadSignals {
    /// `first − hedge_ratio · second`.
    pub spread: Vec<f64>,
    /// The spread's z-score against its trailing window, the current value included, with the population standard
    /// deviation (`vike_indicators::series::spread_zscore`); `NaN` where the window is flat.
    pub zscore: Vec<f64>,
    /// The bars a deviation takes to halve, `−ln 2 / λ` of the window's regression `Δs = a + λ s_(t-1)`
    /// (`vike_indicators::series::half_life`); `NaN` where the window is not mean-reverting (`λ ≥ 0`). `NaN` here is
    /// an answer, not a gap.
    pub half_life: Vec<f64>,
    /// The spread's standard deviation over the same trailing window, population form
    /// (`vike_math::rolling::rolling_std` with `WindowSpec::indicator`): the scale a label is measured in.
    pub deviation: Vec<f64>,
}

impl SpreadSignals {
    /// The four vectors as one value; `None` unless they all have the same length.
    pub fn new(
        spread: Vec<f64>,
        zscore: Vec<f64>,
        half_life: Vec<f64>,
        deviation: Vec<f64>,
    ) -> Option<Self> {
        let n = spread.len();
        (zscore.len() == n && half_life.len() == n && deviation.len() == n).then_some(Self {
            spread,
            zscore,
            half_life,
            deviation,
        })
    }
}

/// The spread `first − hedge_ratio · second` with its z-score, half-life and deviation over `window` bars, from the
/// spread indicators' own kernels (`series::spread_zscore`, `series::half_life`) and the rolling deviation they
/// share (`vike_math::rolling::rolling_std` with `WindowSpec::indicator`). The ONE copy of this glue: a study and a strategy file
/// both call it.
///
/// `None` when the two series differ in length, or `window` (`2..=200`) or `hedge_ratio` (`-5..=5`) is outside what the
/// indicators accept.
pub fn signals(
    first: &[f64],
    second: &[f64],
    hedge_ratio: f64,
    window: usize,
) -> Option<SpreadSignals> {
    if first.len() != second.len() {
        return None;
    }
    let period = window as f64;
    let spread: Vec<f64> = first.iter().zip(second).map(|(a, b)| a - hedge_ratio * b).collect();
    SpreadSignals::new(
        spread.clone(),
        series::spread_zscore(first, second, period, hedge_ratio).ok()?,
        series::half_life(first, second, period, hedge_ratio).ok()?,
        rolling_std(&spread, WindowSpec::indicator(window)),
    )
}

/// The signals of a spread that is ONE series already, such as the cumulative residual of an asset against its
/// factors ([`crate::pair_discovery::factor_residuals`]): [`signals`] with a zero second leg.
pub fn single_series_signals(spread: &[f64], window: usize) -> Option<SpreadSignals> {
    signals(spread, &vec![0.0; spread.len()], 0.0, window)
}

/// The built-in columns of a feature row, in order.
pub const FEATURE_NAMES: [&str; 5] =
    ["zscore", "zscore_change", "zscore_momentum", "absolute_zscore", "reversion_speed"];

/// The look-back and the horizon of a reversion model. The window of the z-score, the half-life and the
/// deviation is the caller's, in the [`SpreadSignals`] it made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReversionConfig {
    /// The bars `zscore_momentum` looks back over; at least 1.
    pub momentum: usize,
    /// The bars ahead the label looks; at least 1.
    pub horizon: usize,
}

/// The feature rows of a spread: the bars they are for and their values, row after row.
///
/// `None` when `extra` holds a column of another length than the spread, or `momentum` is zero.
pub fn reversion_features(
    signals: &SpreadSignals,
    config: ReversionConfig,
    extra: &[&[f64]],
) -> Option<(Vec<usize>, Vec<f64>)> {
    let n = signals.spread.len();
    if config.momentum == 0 || extra.iter().any(|column| column.len() != n) {
        return None;
    }
    let mut bars = Vec::new();
    let mut features = Vec::new();
    for t in config.momentum..n {
        let (z, previous, earlier) =
            (signals.zscore[t], signals.zscore[t - 1], signals.zscore[t - config.momentum]);
        if !(z.is_finite() && previous.is_finite() && earlier.is_finite()) {
            continue;
        }
        if extra.iter().any(|column| !column[t].is_finite()) {
            continue;
        }
        let half_life = signals.half_life[t];
        let speed = if half_life.is_finite() && half_life > 0.0 { 1.0 / half_life } else { 0.0 };
        bars.push(t);
        features.extend_from_slice(&[z, z - previous, z - earlier, z.abs(), speed]);
        features.extend(extra.iter().map(|column| column[t]));
    }
    Some((bars, features))
}

/// A training set: the feature rows of the bars whose label is known, with the labels.
#[derive(Clone, Debug, PartialEq)]
pub struct ReversionDataset {
    /// The bar each row is for (its decision bar).
    pub bars: Vec<usize>,
    /// Row-major, [`ReversionDataset::columns`] values per row, the order of [`FEATURE_NAMES`] then the extras.
    pub features: Vec<f64>,
    /// The spread's change over the next `horizon` bars divided by the row's `SpreadSignals::deviation` (its
    /// standard deviation over the trailing window, the population form the z-score uses).
    pub labels: Vec<f32>,
    pub columns: usize,
}

/// The feature rows of `signals` with a label each: the rows whose bar has `horizon` more bars after it and a
/// spread deviation that is a positive finite number.
///
/// `None` where [`reversion_features`] is, or when `horizon` is zero.
pub fn reversion_dataset(
    signals: &SpreadSignals,
    config: ReversionConfig,
    extra: &[&[f64]],
) -> Option<ReversionDataset> {
    if config.horizon == 0 {
        return None;
    }
    let (bars, features) = reversion_features(signals, config, extra)?;
    let columns = FEATURE_NAMES.len() + extra.len();
    let n = signals.spread.len();
    let mut dataset =
        ReversionDataset { bars: Vec::new(), features: Vec::new(), labels: Vec::new(), columns };
    for (row, &t) in bars.iter().enumerate() {
        let sigma = signals.deviation[t];
        if t + config.horizon >= n || !sigma.is_finite() || sigma <= 0.0 {
            continue;
        }
        dataset.bars.push(t);
        dataset.features.extend_from_slice(&features[row * columns..(row + 1) * columns]);
        dataset
            .labels
            .push(((signals.spread[t + config.horizon] - signals.spread[t]) / sigma) as f32);
    }
    Some(dataset)
}

/// The gross profit of each decision of a trading rule on a spread, as `(bar, profit)`: `decisions` are
/// `(bar, position)` in time order, the position in `[-1, 1]` held from that bar's close to the close
/// `horizon` bars later, earning `position · (spread[bar + horizon] − spread[bar])`. Nothing is charged: the
/// costs are a backtest's (a study hands the kept decisions to `vike_simulator::VectorBacktestEngine::run`,
/// charged the one cost type, `vike_model::Costs`).
///
/// A decision is skipped (it earns nothing) when it starts before the previous kept decision ended, so holds
/// never overlap, and when `horizon` bars after it lie past the spread's end. A kept decision whose hold ends
/// with no kept decision at that bar leaves the book flat there.
pub fn step_pnl(spread: &[f64], decisions: &[(usize, f64)], horizon: usize) -> Vec<(usize, f64)> {
    let mut profits: Vec<(usize, f64)> = Vec::new();
    let mut free_from = 0;
    for &(bar, position) in decisions {
        if bar < free_from || bar + horizon >= spread.len() {
            continue;
        }
        profits.push((bar, position * (spread[bar + horizon] - spread[bar])));
        free_from = bar + horizon;
    }
    profits
}
