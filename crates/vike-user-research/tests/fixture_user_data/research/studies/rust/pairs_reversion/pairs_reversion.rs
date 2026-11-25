//! `pairs_reversion` — the book's chapter-37 case: FIND A PAIR, TEST IT, AND LEARN HOW ITS SPREAD REVERTS.
//!
//! On a set of symbols the recipe names, the study cuts the aligned bars in time. On the EARLIER part it does
//! everything that looks at the data to decide something: it clusters the assets by the correlation of their
//! returns, runs the Engle–Granger test on the pairs inside each cluster, keeps the most significant pair and
//! its hedge ratio, and fits a ridge model of how that pair's spread moves next from the spread's own state. On
//! the LATER part, which none of that has seen, it asks whether the pair still reverts and whether the model
//! earns more than the plain rule "fade the z-score".
//!
//! **Point in time, every step.** Discovery reads training rows only. The spread's z-score and half-life are
//! the indicators' own, whose value at a bar reads that bar and the ones before it. A training row's label looks
//! `horizon` bars ahead, so a row is trained on only if its label is known before the test window begins; the
//! rows between are dropped (the purge). Each test decision is one per `horizon` bars, never overlapping
//! (`vike_arbitrage::spread::step_pnl`'s hold rule), and a hold whose end no decision follows leaves the book flat.
//!
//! **Judged as a backtest judges it.** A kept decision holds the spread, `position` of the first leg's equity and
//! `−position · hedge_ratio` of the second's, decided at its bar's close and filled at the next open by the
//! vectorized kernel (`vike_simulator::VectorBacktestEngine::run_over_bars`) from `cash` (10,000), charged the
//! recipe's costs: the backtest's own keys, `commission` and `slippage` with their units
//! (`vike_user_research::costs_of_recipe`), none when absent.
//!
//! Recipe. Required: `venue`, `interval` and `symbols` (an array of at least three; there is no default
//! instrument). Optional: `train_fraction` (0.6), `clustering` (`single_linkage`, the default, or `k_means`),
//! `max_clusters` (4), `significance` (`five_percent`, `one_percent` or `ten_percent`), `lags` (1: the lagged
//! differences of the residual's unit-root test), `window` (48 bars: the z-score and half-life window),
//! `momentum` (6), `horizon` (3), `penalty` (1.0: the ridge penalty), `threshold` (0.1: the least predicted
//! move, in spread deviations, the model trades on), `entry` (1.5: the z-score the baseline rule fades), the
//! costs (`commission`, `slippage` and their units, none by default; a retired `cost` key is refused), `cash`
//! (10,000), `factors` (1: the statistical factors taken
//! out for the residual spreads), and `kalman_delta` (0.0001) and `kalman_observation_variance` (0.001), the
//! dynamic hedge ratio's two knobs. The filter starts from a ratio of zero and may move only as fast as
//! `kalman_delta` lets it: at the default it is still on its way after a thousand bars (0.90 on the test
//! fixture, against the static 1.19), so raise `kalman_delta` to follow faster.
//!
//! Metrics: the universe (`bars`, `train_bars`, `test_bars`, `clusters`, `pairs_tested`, `pairs_found`); the
//! best pair (`pair_first` and `pair_second`, the symbols' positions in the recipe, `hedge_ratio`, `intercept`,
//! `engle_granger_statistic`, `engle_granger_critical_five_percent`, `johansen_rank`, `johansen_trace_zero`,
//! `kalman_hedge_ratio_end_of_training`, `kalman_hedge_ratio_end`); its
//! health on the test window (`test_adf_statistic`, `test_reverting_share`, `test_mean_half_life`); the model
//! (`train_rows`, `test_rows`, `zscore_coefficient`, `prediction_correlation`, `side_hit_rate`); the two rules
//! (`ml_trades` and `baseline_trades`, the decisions that hold a position; `ml_total` and `baseline_total`, the
//! backtest's return on `cash`; `ml_sharpe` and `baseline_sharpe`, its equity's Sharpe ratio over the test
//! window, `vike_analytics::metrics::sharpe`, a year of bars); and the factor residuals (`factor_mean_r_squared`,
//! `factor_reverting_assets`). A universe with no pair stops after `pairs_found` of 0: finding nothing is an
//! answer, not an error.
//!
//! Artifacts: `pairs.tsv` (every pair found), `model.txt` (the fitted model's text, which a strategy file loads
//! with `vike_ml_prediction::any_model::AnyModel::from_text` and feeds the same feature row), and the charts
//! `chart_spread.tsv` (the test window's z-score) and `chart_profit.tsv` (each rule's return on `cash` at every
//! test bar).

use vike_analytics::metrics::sharpe;
use vike_arbitrage::cointegration::critical_values::{JohansenStatistic, Significance};
use vike_arbitrage::cointegration::{JohansenCase, johansen};
use vike_arbitrage::pair_discovery::{
    Clustering, cluster_returns, cointegrated_pairs, factor_residuals,
};
use vike_arbitrage::spread::{
    self, ReversionConfig, log_prices, reversion_dataset, single_series_signals, step_pnl,
};
use vike_indicators::series;
use vike_marketdata::Bar;
use vike_marketdata::interval::interval_ms;
use vike_math::linear_algebra::{Matrix, correlation_matrix};
use vike_math::stationarity_tests::{self, Deterministic};
use vike_ml_prediction::Predictor;
use vike_ml_training::TrainData;
use vike_ml_training::linear::{LinearSpec, fit_linear};
use vike_portfolio::principal_components::eigenportfolios;
use vike_simulator::VectorBacktestEngine;
use vike_user_research::{StudyContext, StudyError, StudyOutcome, costs_of_recipe};

#[cfg(test)]
pub mod fixture_store;

fn refuse(why: impl Into<String>) -> StudyError {
    StudyError::Study(why.into())
}

fn required<'a>(params: &'a toml::Value, key: &str) -> Result<&'a str, StudyError> {
    params
        .get(key)
        .and_then(toml::Value::as_str)
        .ok_or_else(|| refuse(format!("the recipe must name `{key}` (a string)")))
}

fn text<'a>(params: &'a toml::Value, key: &str, default: &'a str) -> &'a str {
    params.get(key).and_then(toml::Value::as_str).unwrap_or(default)
}

fn count(params: &toml::Value, key: &str, default: i64, least: i64) -> Result<usize, StudyError> {
    let value = params.get(key).and_then(toml::Value::as_integer).unwrap_or(default);
    if value < least {
        return Err(refuse(format!("`{key}` = {value} is below {least}")));
    }
    Ok(value as usize)
}

/// A recipe number written as `1.5` or as `2`.
fn number(params: &toml::Value, key: &str, default: f64) -> f64 {
    let value = params.get(key);
    value
        .and_then(toml::Value::as_float)
        .or_else(|| value.and_then(toml::Value::as_integer).map(|i| i as f64))
        .unwrap_or(default)
}

fn symbols(params: &toml::Value) -> Result<Vec<String>, StudyError> {
    let list = params
        .get("symbols")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| refuse("the recipe must name `symbols` (an array of strings)"))?;
    let names: Vec<String> = list.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
    if names.len() != list.len() || names.len() < 3 {
        return Err(refuse("`symbols` must hold at least three strings: a silhouette needs three"));
    }
    Ok(names)
}

fn significance(params: &toml::Value) -> Result<Significance, StudyError> {
    match text(params, "significance", "five_percent") {
        "one_percent" => Ok(Significance::OnePercent),
        "five_percent" => Ok(Significance::FivePercent),
        "ten_percent" => Ok(Significance::TenPercent),
        other => Err(refuse(format!(
            "`significance` = {other:?} is not `one_percent`, `five_percent` or `ten_percent`"
        ))),
    }
}

fn clustering(params: &toml::Value) -> Result<Clustering, StudyError> {
    match text(params, "clustering", "single_linkage") {
        "single_linkage" => Ok(Clustering::SingleLinkage),
        "k_means" => Ok(Clustering::KMeans),
        other => {
            Err(refuse(format!("`clustering` = {other:?} is not `single_linkage` or `k_means`")))
        }
    }
}

/// The bars of every symbol at the open times ALL of them have, with those times.
fn aligned_bars(
    ctx: &StudyContext,
    venue: &str,
    names: &[String],
    interval: &str,
) -> Result<(Vec<i64>, Vec<Vec<Bar>>), StudyError> {
    let mut series = Vec::with_capacity(names.len());
    for name in names {
        let bars = ctx.bars(venue, name, interval, ctx.window())?;
        if bars.is_empty() {
            return Err(refuse(format!(
                "no bars for {venue}/{name}/{interval} in the requested window"
            )));
        }
        series.push(bars);
    }
    let mut common: Vec<i64> = series[0].iter().map(|b| b.timestamp).collect();
    for bars in &series[1..] {
        let mut kept = Vec::with_capacity(common.len());
        let mut at = 0;
        for &time in &common {
            while at < bars.len() && bars[at].timestamp < time {
                at += 1;
            }
            if at < bars.len() && bars[at].timestamp == time {
                kept.push(time);
            }
        }
        common = kept;
    }
    let aligned = series
        .iter()
        .map(|bars| {
            let mut at = 0;
            common
                .iter()
                .map(|&time| {
                    while bars[at].timestamp < time {
                        at += 1;
                    }
                    bars[at].clone()
                })
                .collect()
        })
        .collect();
    Ok((common, aligned))
}

/// The series as the columns of one matrix over the rows `from..to`.
fn matrix_of(columns: &[Vec<f64>], from: usize, to: usize) -> Result<Matrix, StudyError> {
    let values: Vec<f64> =
        (from..to).flat_map(|t| columns.iter().map(move |column| column[t])).collect();
    Matrix::from_row_major(to - from, columns.len(), values)
        .ok_or_else(|| refuse("the series do not form a matrix"))
}

/// The successive differences of the columns of `levels`: one row fewer.
fn differences(levels: &Matrix) -> Result<Matrix, StudyError> {
    let (rows, columns) = (levels.rows(), levels.columns());
    let values: Vec<f64> = (1..rows)
        .flat_map(|t| (0..columns).map(move |j| levels.get(t, j) - levels.get(t - 1, j)))
        .collect();
    Matrix::from_row_major(rows - 1, columns, values)
        .ok_or_else(|| refuse("the returns do not form a matrix"))
}

fn sign_of(value: f64, least: f64) -> f64 {
    if value.abs() >= least { value.signum() } else { 0.0 }
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let venue = required(params, "venue")?;
    let interval = required(params, "interval")?;
    let names = symbols(params)?;
    let level = significance(params)?;
    let method = clustering(params)?;
    let max_clusters = count(params, "max_clusters", 4, 2)?;
    let lags = count(params, "lags", 1, 0)?;
    let window = count(params, "window", 48, 2)?;
    let momentum = count(params, "momentum", 6, 1)?;
    let horizon = count(params, "horizon", 3, 1)?;
    let factors = count(params, "factors", 1, 1)?;
    let train_fraction = number(params, "train_fraction", 0.6);
    let penalty = number(params, "penalty", 1.0);
    let threshold = number(params, "threshold", 0.1);
    let entry = number(params, "entry", 1.5);
    if params.get("cost").is_some() {
        return Err(refuse(
            "`cost` is retired: state the backtest's `commission` and `slippage` (with their units)",
        ));
    }
    let costs = costs_of_recipe(params)?;
    let cash = number(params, "cash", 10_000.0);
    if !(0.1..=0.9).contains(&train_fraction) {
        return Err(refuse(format!("`train_fraction` = {train_fraction} is outside 0.1..=0.9")));
    }
    let bar_ms = interval_ms(interval)
        .ok_or_else(|| refuse(format!("`interval` = {interval:?} is not a bar interval")))?;

    let (times, aligned) = aligned_bars(ctx, venue, &names, interval)?;
    let closes: Vec<Vec<f64>> =
        aligned.iter().map(|bars| bars.iter().map(|bar| bar.close).collect()).collect();
    let bars = times.len();
    let split = (bars as f64 * train_fraction) as usize;
    if split < 4 * window || bars - split < 4 * window {
        return Err(refuse(format!(
            "{bars} aligned bars split at {split} leave fewer than four windows of {window} on a side"
        )));
    }
    let logs: Vec<Vec<f64>> = closes.iter().map(|c| log_prices(c)).collect();
    if logs.iter().any(|column| column.iter().any(|v| !v.is_finite())) {
        return Err(refuse("a close is not a positive finite price"));
    }

    // Discovery: the training rows only.
    let train_logs = matrix_of(&logs, 0, split)?;
    let train_returns = differences(&train_logs)?;
    let clusters = cluster_returns(&train_returns, max_clusters, method).ok_or_else(|| {
        refuse(
            "the training returns have no admissible clustering (a flat series, or too few assets)",
        )
    })?;
    let tested: usize = clusters.iter().map(|c| c.len() * c.len().saturating_sub(1) / 2).sum();
    let pairs = cointegrated_pairs(&train_logs, &clusters, lags, level);

    let mut out = StudyOutcome::new();
    out.metric("bars", bars as f64)?;
    out.metric("train_bars", split as f64)?;
    out.metric("test_bars", (bars - split) as f64)?;
    out.metric("clusters", clusters.len() as f64)?;
    out.metric("pairs_tested", tested as f64)?;
    out.metric("pairs_found", pairs.len() as f64)?;
    let mut table =
        String::from("first\tsecond\thedge_ratio\tintercept\tstatistic\tcritical_five_percent\n");
    for pair in &pairs {
        table.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            names[pair.first],
            names[pair.second],
            pair.hedge_ratio,
            pair.intercept,
            pair.statistic,
            pair.critical_values.five_percent
        ));
    }
    out.artifact("pairs.tsv", table)?;
    let Some(best) = pairs.first().copied() else {
        return Ok(out);
    };

    out.metric("pair_first", best.first as f64)?;
    out.metric("pair_second", best.second as f64)?;
    out.metric("hedge_ratio", best.hedge_ratio)?;
    out.metric("intercept", best.intercept)?;
    out.metric("engle_granger_statistic", best.statistic)?;
    out.metric("engle_granger_critical_five_percent", best.critical_values.five_percent)?;
    let pair_levels = matrix_of(&[logs[best.first].clone(), logs[best.second].clone()], 0, split)?;
    match johansen(&pair_levels, lags, JohansenCase::UnrestrictedConstant) {
        Some(test) => {
            out.metric("johansen_rank", test.rank(JohansenStatistic::Trace, level) as f64)?;
            out.metric("johansen_trace_zero", test.trace_statistics[0])?;
        }
        None => {
            out.metric("johansen_rank", f64::NAN)?;
            out.metric("johansen_trace_zero", f64::NAN)?;
        }
    }
    // The Kalman filter's hedge ratio runs through the whole series, each value from its own bar and the ones
    // before it: beside the static ratio it shows whether the ratio drifts.
    let kalman = series::kalman_beta(
        &logs[best.first],
        &logs[best.second],
        number(params, "kalman_delta", 0.0001),
        number(params, "kalman_observation_variance", 0.001),
    )
    .map_err(|_| refuse("`kalman_delta` or `kalman_observation_variance` is outside its range"))?;
    out.metric("kalman_hedge_ratio_end_of_training", kalman[split - 1])?;
    out.metric("kalman_hedge_ratio_end", kalman[bars - 1])?;

    // The spread over the whole series (each value reads its own bar and the bars before it), judged on the rest.
    let signals = spread::signals(&logs[best.first], &logs[best.second], best.hedge_ratio, window)
        .ok_or_else(|| {
            refuse("the window or the hedge ratio is outside what the spread indicators accept")
        })?;
    let centred: Vec<f64> = signals.spread[split..].iter().map(|s| s - best.intercept).collect();
    out.metric(
        "test_adf_statistic",
        stationarity_tests::augmented_dickey_fuller(&centred, lags, Deterministic::Absent),
    )?;
    let finite: Vec<f64> =
        signals.half_life[split..].iter().copied().filter(|h| h.is_finite()).collect();
    out.metric("test_reverting_share", finite.len() as f64 / (bars - split) as f64)?;
    let mean_half_life =
        if finite.is_empty() { f64::NAN } else { finite.iter().sum::<f64>() / finite.len() as f64 };
    out.metric("test_mean_half_life", mean_half_life)?;

    // The model: ridge on the spread's own state, trained on rows whose label is known before the test starts.
    let config = ReversionConfig { momentum, horizon };
    let dataset = reversion_dataset(&signals, config, &[])
        .ok_or_else(|| refuse("the spread gives no feature rows"))?;
    let columns = dataset.columns;
    let (mut train_x, mut train_y) = (Vec::new(), Vec::new());
    let (mut test_rows, mut test_bars) = (Vec::new(), Vec::new());
    for (row, &bar) in dataset.bars.iter().enumerate() {
        let features = &dataset.features[row * columns..(row + 1) * columns];
        if bar + horizon < split {
            train_x.extend_from_slice(features);
            train_y.push(dataset.labels[row]);
        } else if bar >= split {
            test_rows.push((features, dataset.labels[row]));
            test_bars.push(bar);
        }
    }
    if train_y.len() <= 2 * columns || test_rows.is_empty() {
        return Err(refuse(format!(
            "{} training rows and {} test rows are too few to fit and judge a model",
            train_y.len(),
            test_rows.len()
        )));
    }
    let data = TrainData::new(&train_x, &train_y, columns)
        .map_err(|e| refuse(format!("the training rows were rejected: {e}")))?;
    let model = fit_linear(&data, LinearSpec::Ridge { penalty })
        .map_err(|e| refuse(format!("the fit was refused: {e}")))?;
    let mut predictions = Vec::with_capacity(test_rows.len());
    for (features, _) in &test_rows {
        predictions.push(model.predict_one(features).map_err(refuse)?);
    }
    out.metric("train_rows", train_y.len() as f64)?;
    out.metric("test_rows", test_rows.len() as f64)?;
    out.metric("zscore_coefficient", model.coefficients[0])?;
    let paired: Vec<f64> = predictions
        .iter()
        .zip(&test_rows)
        .flat_map(|(p, (_, label))| [*p, f64::from(*label)])
        .collect();
    let correlation = Matrix::from_row_major(test_rows.len(), 2, paired)
        .and_then(|m| correlation_matrix(&m))
        .map_or(f64::NAN, |m| m.get(0, 1));
    out.metric("prediction_correlation", correlation)?;
    let sides = predictions
        .iter()
        .zip(&test_rows)
        .filter(|(p, (_, label))| p.signum() == f64::from(*label).signum())
        .count();
    out.metric("side_hit_rate", sides as f64 / test_rows.len() as f64)?;

    // The two rules on the test bars: the model's sign when it predicts a move of at least `threshold`
    // spread deviations, against fading the z-score beyond `entry`.
    let ml: Vec<(usize, f64)> =
        test_bars.iter().zip(&predictions).map(|(&bar, &p)| (bar, sign_of(p, threshold))).collect();
    let baseline: Vec<(usize, f64)> =
        test_bars.iter().map(|&bar| (bar, -sign_of(signals.zscore[bar], entry))).collect();
    // The platform's one 365-day year in milliseconds, over one bar.
    let bars_per_year = vike_options::MS_PER_YEAR / bar_ms as f64;
    let legs = [aligned[best.first].as_slice(), aligned[best.second].as_slice()];
    let mut chart = String::from("time\tvalue\tseries\tkind\n");
    for (name, decisions) in [("ml", &ml), ("baseline", &baseline)] {
        // The kept decisions as the two legs' weights (the module doc), flat where a hold ends unless the next
        // kept decision starts there: it comes later in the walk and writes over the flat.
        let kept = step_pnl(&signals.spread, decisions, horizon);
        let mut kept_bars = kept.iter().map(|(bar, _)| *bar).peekable();
        let mut weights = vec![f64::NAN; 2 * bars];
        for &(bar, position) in decisions.iter() {
            if kept_bars.next_if_eq(&bar).is_none() {
                continue;
            }
            weights[2 * bar] = position;
            weights[2 * bar + 1] = -position * best.hedge_ratio;
            let end = bar + horizon;
            weights[2 * end] = 0.0;
            weights[2 * end + 1] = 0.0;
        }
        let run = VectorBacktestEngine::run_over_bars(&legs, &weights, &costs, cash, 1.0)
            .map_err(refuse)?;
        let equity = &run.equity_curve[split..];
        out.metric(
            &format!("{name}_trades"),
            decisions.iter().filter(|(_, p)| *p != 0.0).count() as f64,
        )?;
        out.metric(&format!("{name}_total"), run.final_equity / cash - 1.0)?;
        out.metric(&format!("{name}_sharpe"), sharpe(equity, bars_per_year))?;
        for (time, value) in times[split..].iter().zip(equity) {
            chart.push_str(&format!("{time}\t{}\t{name}\tline\n", value / cash - 1.0));
        }
    }
    out.artifact("chart_profit.tsv", chart)?;
    let mut zscores = String::from("time\tvalue\tseries\tkind\n");
    for (time, z) in times.iter().zip(&signals.zscore).skip(split) {
        if z.is_finite() {
            zscores.push_str(&format!("{time}\t{z}\tzscore\tline\n"));
        }
    }
    out.artifact("chart_spread.tsv", zscores)?;
    out.artifact("model.txt", model.to_text())?;

    // The factor residuals of every asset on the training window: how much the market and the next statistical
    // factors explain, and which residual spreads are mean-reverting at the end of it.
    // The factors are the eigenportfolios of the training returns (the caller's, `vike-arbitrage` takes their returns).
    let portfolios = eigenportfolios(&train_returns, factors)
        .ok_or_else(|| refuse("the principal components of the training returns do not exist"))?;
    let factor_columns: Vec<Vec<f64>> = portfolios.into_iter().map(|p| p.returns).collect();
    let factor_returns = matrix_of(&factor_columns, 0, train_returns.rows())?;
    let residuals = factor_residuals(&train_returns, &factor_returns).ok_or_else(|| {
        refuse("the factor residuals do not exist (too many factors for the assets or rows)")
    })?;
    out.metric(
        "factor_mean_r_squared",
        residuals.r_squared.iter().sum::<f64>() / residuals.r_squared.len() as f64,
    )?;
    let reverting = (0..names.len())
        .filter(|&j| {
            single_series_signals(&residuals.spreads.column(j), window)
                .is_some_and(|s| s.half_life.last().is_some_and(|h| h.is_finite()))
        })
        .count();
    out.metric("factor_reverting_assets", reverting as f64)?;
    Ok(out)
}
