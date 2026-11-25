//! `funding_spike_effect` — the effect of a funding-rate SPIKE on a perpetual's next-period return,
//! estimated by double machine learning (`vike_ml_training::double_ml::partially_linear`) with the
//! market's own state as the confounders.
//!
//! A funding spike does not arrive at random: it comes with volatile, heavily traded markets, and
//! those move the next return on their own. Comparing the returns after spikes with the returns
//! after quiet fundings (the `naive_effect` metric) mixes the two. Here the confounders, realized
//! volatility and traded volume over the `lookback` bars before each funding time, are partialled
//! out of both the spike and the return by cross-fitted nuisance models; what is left is the
//! spike's own effect (`effect`), its `standard_error` and a `level` interval (`interval_low`,
//! `interval_high`).
//!
//! Recipe keys. Required: `venue`, `symbol` (the funding series is `ctx.bars(venue, symbol,
//! "funding", …)`, the vocabulary `vike-cli data history fetch VENUE:SYMBOL:funding` fills) and
//! `interval` (the price bars of the same symbol). Optional: `lookback` (bars, default 24),
//! `horizon` (bars, default 8), `spike` (an absolute funding rate at or above it is a spike,
//! default 0.0005), `folds` (default 5), `embargo` (events; by default the most events after any
//! one whose confounders read a return its label holds, reported as the `embargo` metric), `nuisance`
//! (`gaussian_process`, the default, or `linear`), `length_scale` (the Gaussian process's, on
//! z-scored confounders, default 1.0) and `level` (default 0.95).
//!
//! Point in time: an event's confounders read only the bars that had CLOSED by its funding time
//! (`Bar::closed_at`: a 1d bar opened before an 08:00 funding closes 16 hours after it, so it is not
//! one of them), and its outcome is the return from the close of the last of them to the close
//! `horizon` bars later. An event's label therefore reads bars `j − 1 ..= j + horizon − 1`, and the
//! folds are `vike_analytics::validation::purged_kfold_label_spans` over those spans in time order: a
//! training label overlapping a test fold's is PURGED (a `horizon` longer than the funding spacing
//! makes labels overlap; shorter, they only touch and nothing is purged, as with point folds), and an
//! embargo follows each test fold. `left_out_of_training` counts, over the folds, the events neither
//! tested nor trained on.
//!
//! The two confounders are z-scored over the whole window before the fit: a SCALE for the kernel,
//! which the outcome never enters, so it moves no label across a fold.
//!
//! The Gaussian-process nuisances take each target's own variance as both the signal and the noise
//! variance (a prior that the confounders explain about half of it), so one `length_scale` serves a
//! return and a 0/1 spike alike.

use vike_analytics::validation::purged_kfold_label_spans;
use vike_marketdata::Bar;
use vike_math::statistics::{mean, mean_variance, z_score};
use vike_ml_training::DEFAULT_POINT;
use vike_ml_training::double_ml::{CrossFitting, DoubleMlData, partially_linear};
use vike_ml_training::gaussian_process::{GaussianProcessLearner, GaussianProcessParams};
use vike_ml_training::linear::{LinearLearner, LinearSpec};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

/// One funding time with its confounders and outcome.
struct Event {
    time: i64,
    /// `j`: the first bar not closed by the funding time.
    bar: usize,
    funding: f64,
    volatility: f64,
    volume: f64,
    next_return: f64,
}

fn required<'a>(params: &'a toml::Value, key: &str) -> Result<&'a str, StudyError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| StudyError::Study(format!("the recipe needs `{key}` (a string)")))
}

fn count(params: &toml::Value, key: &str, default: i64, least: i64) -> Result<usize, StudyError> {
    let v = params.get(key).and_then(|v| v.as_integer()).unwrap_or(default);
    if v < least {
        return Err(StudyError::Study(format!("`{key}` = {v} is below {least}")));
    }
    Ok(v as usize)
}

fn number(params: &toml::Value, key: &str, default: f64) -> f64 {
    params.get(key).and_then(|v| v.as_float()).unwrap_or(default)
}

/// The population variance of `xs`.
fn variance(xs: &[f64]) -> f64 {
    mean_variance(xs.iter().copied(), 0.0).1
}

/// Every funding time with `lookback` bars (plus the one before them) ahead of it and `horizon`
/// bars after: `j` is the first bar NOT closed by the funding time (`Bar::closed_at`, bars `step`
/// long), the confounders read bars `j − lookback .. j`, the outcome is
/// `close[j + horizon − 1] / close[j − 1] − 1`.
fn events(
    funding: &[Bar],
    prices: &[Bar],
    lookback: usize,
    horizon: usize,
    step: i64,
) -> Vec<Event> {
    let mut out = Vec::new();
    for f in funding {
        let Some(rate) = f.funding else { continue };
        let j = prices.partition_point(|b| b.closed_at(step, f.timestamp));
        if j < lookback + 1 || j + horizon > prices.len() {
            continue;
        }
        let window = &prices[j - lookback..j];
        let squares: f64 = (j - lookback..j)
            .map(|i| {
                let r = prices[i].close / prices[i - 1].close - 1.0;
                r * r
            })
            .sum();
        out.push(Event {
            time: f.timestamp,
            bar: j,
            funding: rate,
            volatility: (squares / lookback as f64).sqrt(),
            volume: window.iter().map(|b| b.volume).sum(),
            next_return: prices[j + horizon - 1].close / prices[j - 1].close - 1.0,
        });
    }
    out
}

/// `xs` minus its mean over its standard deviation; a constant column stays centred at zero.
fn z_scores(xs: &[f64]) -> Vec<f64> {
    let (m, population_variance) = mean_variance(xs.iter().copied(), 0.0);
    let sd = population_variance.sqrt();
    xs.iter().map(|&x| if sd > 0.0 { z_score(x, m, sd) } else { 0.0 }).collect()
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let venue = required(params, "venue")?;
    let symbol = required(params, "symbol")?;
    let interval = required(params, "interval")?;
    let lookback = count(params, "lookback", 24, 2)?;
    let horizon = count(params, "horizon", 8, 1)?;
    let fold_count = count(params, "folds", 5, 2)?;
    let stated_embargo =
        params.get("embargo").map(|_| count(params, "embargo", 0, 0)).transpose()?;
    let spike = number(params, "spike", 0.0005);
    let length_scale = number(params, "length_scale", 1.0);
    let level = number(params, "level", 0.95);
    let nuisance = params.get("nuisance").and_then(|v| v.as_str()).unwrap_or("gaussian_process");

    let step =
        vike_marketdata::interval::interval_ms(interval).filter(|ms| *ms > 0).ok_or_else(|| {
            StudyError::Study(format!("`interval` = {interval:?} is not a bar length"))
        })?;
    let funding = ctx.bars(venue, symbol, "funding", ctx.window())?;
    let prices = ctx.bars(venue, symbol, interval, ctx.window())?;
    let events = events(&funding, &prices, lookback, horizon, step);
    if events.len() < 2 * fold_count {
        return Err(StudyError::Study(format!(
            "{} funding events with {lookback} bars before and {horizon} after in the window: \
             {fold_count} folds need at least {}",
            events.len(),
            2 * fold_count
        )));
    }

    let treated: Vec<f64> =
        events.iter().map(|e| if e.funding.abs() >= spike { 1.0 } else { 0.0 }).collect();
    let spikes = treated.iter().filter(|&&d| d == 1.0).count();
    if spikes == 0 || spikes == events.len() {
        return Err(StudyError::Study(format!(
            "{spikes} of {} events are spikes at |funding| >= {spike}: no contrast to estimate",
            events.len()
        )));
    }
    let outcome: Vec<f64> = events.iter().map(|e| e.next_return).collect();
    let volatility = z_scores(&events.iter().map(|e| e.volatility).collect::<Vec<_>>());
    let volume = z_scores(&events.iter().map(|e| e.volume).collect::<Vec<_>>());
    let confounders: Vec<f64> =
        volatility.iter().zip(&volume).flat_map(|(v, q)| [*v, *q]).collect();

    // Each label reads bars `j - 1 ..= j + horizon - 1`: purged by span, so a training label running into a
    // test fold's prices is never fit on (labels that only touch, as with a horizon inside the spacing, stay).
    let spans: Vec<(usize, usize)> =
        events.iter().map(|e| (e.bar - 1, e.bar + horizon - 1)).collect();
    // The default embargo: the most events after one event whose CONFOUNDERS read one of the returns its label
    // holds (bars `j_k - lookback ..` against `.. j_i + horizon - 1`) while their own label does not overlap it
    // (that one is purged): `horizon <= j_k - j_i < lookback + horizon`.
    let embargo = stated_embargo.unwrap_or_else(|| {
        let after = |i: usize| {
            let reach = |e: &Event| e.bar - events[i].bar;
            events[i + 1..]
                .iter()
                .filter(|e| (horizon..lookback + horizon).contains(&reach(e)))
                .count()
        };
        (0..events.len()).map(after).max().unwrap_or(0)
    });
    let folds = purged_kfold_label_spans(&spans, fold_count, embargo);
    let left_out: usize =
        folds.iter().map(|(train, test)| events.len() - train.len() - test.len()).sum();
    let data = DoubleMlData {
        confounders: &confounders,
        column_count: 2,
        treatment: &treated,
        outcome: &outcome,
    };
    let cross = CrossFitting { folds: &folds, point: &DEFAULT_POINT, seed: 0 };
    let estimate = match nuisance {
        "linear" => {
            let ols = LinearLearner { spec: LinearSpec::OrdinaryLeastSquares };
            partially_linear(&data, &ols, &ols, &cross)
        }
        "gaussian_process" => {
            let process = |target: &[f64]| GaussianProcessLearner {
                params: GaussianProcessParams {
                    length_scale,
                    signal_variance: variance(target),
                    noise_variance: variance(target),
                },
            };
            partially_linear(&data, &process(&outcome), &process(&treated), &cross)
        }
        other => {
            return Err(StudyError::Study(format!(
                "`nuisance` = {other:?} is not `gaussian_process` or `linear`"
            )));
        }
    }
    .map_err(StudyError::Study)?;
    let (low, high) = estimate.confidence_interval(level).map_err(StudyError::Study)?;

    let (mut treated_returns, mut quiet_returns) = (Vec::new(), Vec::new());
    for (d, y) in treated.iter().zip(&outcome) {
        if *d == 1.0 { treated_returns.push(*y) } else { quiet_returns.push(*y) }
    }

    let mut out = StudyOutcome::new();
    out.metric("events", events.len() as f64)?;
    out.metric("spikes", spikes as f64)?;
    out.metric("effect", estimate.effect)?;
    out.metric("standard_error", estimate.standard_error)?;
    out.metric("interval_low", low)?;
    out.metric("interval_high", high)?;
    out.metric("naive_effect", mean(treated_returns) - mean(quiet_returns))?;
    out.metric("embargo", embargo as f64)?;
    out.metric("left_out_of_training", left_out as f64)?;

    let mut tsv = String::from("time\tfunding\tspike\tvolatility\tvolume\tnext_return\n");
    for (e, d) in events.iter().zip(&treated) {
        tsv.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            e.time, e.funding, d, e.volatility, e.volume, e.next_return
        ));
    }
    out.artifact("events.tsv", tsv)?;
    Ok(out)
}
