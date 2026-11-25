//! The MODEL DRIFT WATCH (book W9, chapter 54): the off-fold producer of `vike_alerting`'s
//! `RuleTrigger::ModelDrift` readings.
//!
//! A live strategy that publishes forecasts has them journaled by the core and sent to the datahub on the execution
//! stream, which stores them as the `kind=exec_forecast` series. For each enabled `ModelDrift` rule, every [`DRIFT_CHECK_EVERY`], this watch
//! reads the last `window_ms` of that series back (the strategy lane's `StrategyForecasts`, this daemon's lane
//! key: [`ForecastReader`]), compares the values with the forecasts saved in the rule's REFERENCE RUN
//! (`<project>/user_data/runs/<reference_run>/forecasts.json`, a backtest of the model over its training window)
//! with `vike_math::change_detection`'s population stability index and two-sample Kolmogorov–Smirnov test, and, when
//! the rule asks for it, measures the REALISED error (the forecast read as the return over its horizon, priced
//! by the stored bars of the rule's interval) with the CUSUM against the reference run's own mean error. The
//! reading crosses a channel to the summary tick's `crate::alerts::AlertMount`, whose ONE engine latches it and
//! delivers through the sinks every other rule uses.
//!
//! # The contract
//!
//! - **OFF unless configured**: no enabled `ModelDrift` rule in `alerts.json`, no thread, no read
//!   ([`spawn_drift_watch`] answers `None`).
//! - **Off the core fold**: its own named thread; a read that fails or hangs delays only the next reading. A
//!   failure is said at `warn` once per distinct text, never as an alert.
//! - **A rule nothing feeds is said, never taken as watched**: a reading whose live window holds no forecast carries
//!   a note said like a failure. `kind=exec_forecast` is filled by the datahub from this node's execution stream
//!   (decision 0295), which has no switch to forget.
//! - **Point-in-time**: the live window ends at the reading's time, and a forecast's realised return is measured
//!   only once a bar has CLOSED at or after the end of its horizon. The reference run must END (its last forecast
//!   plus its horizon) by the window's start, or the rule is refused in words: never a window compared with itself.
//! - **The reference is read once per rule** (a finished run never changes), and again only after a failed read.
//!   Its realised-error PRICING reads the bars of the reference's whole window, so a failed pricing is answered from
//!   memory, unread, for a backoff that doubles per failure from [`DRIFT_CHECK_EVERY`] to six hours
//!   ([`REFERENCE_RETRY_MAX`], the one ladder `vike_venues::reconnect::ReconnectBackoff`); a success is kept for good.
//! - No duplicate math: the statistics are `vike_math::change_detection`'s, the sums `vike_math::summation::compensated_sum`.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use vike_alerting::{
    AlertRule, AlertSignal, ForecastErrorDrift, ForecastErrorReading, ModelDriftReading,
    RuleTrigger,
};
use vike_data::{ExecForecastRow, SeriesId};
use vike_math::change_detection::{
    cusum_alarms, kolmogorov_smirnov_two_sample, population_stability_index,
};
use vike_math::summation::compensated_sum;
use vike_model::StoreKindId;
use vike_venues::reconnect::ReconnectBackoff;

use super::inputs::InputReader;

/// How often every `ModelDrift` rule is measured. A drift is a slow fault (hours to days of forecasts), and each
/// reading reads the rule's whole window again.
pub(super) const DRIFT_CHECK_EVERY: Duration = Duration::from_secs(300);

/// The bins of the population stability index: the customary ten (deciles of the reference).
const POPULATION_STABILITY_INDEX_BINS: usize = 10;

/// How often the sleeping thread looks at the stop flag.
const STOP_POLL: Duration = Duration::from_millis(200);

/// The read of the published forecasts of `(venue, symbol)` decided in `[start, end]` (epoch-ms, inclusive).
/// Production: one dial of the datahub's strategy lane (`vike_datahub_client::strat::forecasts`).
pub(super) type ForecastReader =
    Arc<dyn Fn(&str, &str, i64, i64) -> Result<Vec<ExecForecastRow>, String> + Send + Sync>;

/// Where the watch reads: the project's runs root, the forecasts and the bars.
pub(super) struct DriftSources {
    /// `<project>/user_data/runs`, the reference runs' root.
    pub(super) runs: PathBuf,
    pub(super) forecasts: ForecastReader,
    /// The stored bars a realised error is priced by (the inputs' read, `StrategyInputBars`).
    pub(super) bars: InputReader,
}

/// One forecast as the measurement sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ForecastPoint {
    pub(super) ts: i64,
    pub(super) value: f64,
    pub(super) horizon_ms: i64,
}

/// **The production watch**: the enabled `ModelDrift` rules of `rules` over the booted project's runs root and the
/// datahub's strategy lane (this daemon's lane key, read at each read as the inputs read it), or `None` (nothing
/// spawned) when there is no such rule, or no project to read a reference run from (said at `warn`).
pub(super) fn start(rules: &[AlertRule], stop: Arc<AtomicBool>) -> Option<Receiver<AlertSignal>> {
    if drift_rules(rules).is_empty() {
        return None;
    }
    // `<project>/user_data/runs`: the strategies folder's sibling tree, where a `.rs` mount's model run is found too.
    let Some(runs) = crate::mount_factory::project_strategies_dir().and_then(|strategies| {
        let project = strategies.parent()?;
        Some(
            project
                .join(vike_paths::state_path::PROJECT_USER_DATA_DIR)
                .join(vike_paths::state_path::RUNS_SUBDIR),
        )
    }) else {
        tracing::warn!(
            "model drift rules NOT measured: this process declared no project, so no reference run can be read"
        );
        return None;
    };
    let sources = DriftSources {
        runs,
        forecasts: super::live_runtime::datahub_forecast_reader(),
        bars: super::live_runtime::datahub_input_reader(),
    };
    spawn_drift_watch(rules, sources, stop)
}

/// The enabled `ModelDrift` rules of `rules`.
fn drift_rules(rules: &[AlertRule]) -> Vec<AlertRule> {
    rules
        .iter()
        .filter(|r| r.enabled && matches!(r.trigger, RuleTrigger::ModelDrift { .. }))
        .cloned()
        .collect()
}

/// Spawn the watch over the enabled `ModelDrift` rules of `rules`, or `None` (nothing spawned) when there is none.
/// The receiver goes to `crate::alerts::AlertMount::with_drift_readings`. The thread ends when `stop` is raised or
/// the receiver is dropped.
pub(super) fn spawn_drift_watch(
    rules: &[AlertRule],
    sources: DriftSources,
    stop: Arc<AtomicBool>,
) -> Option<Receiver<AlertSignal>> {
    let rules = drift_rules(rules);
    if rules.is_empty() {
        return None;
    }
    let (tx, rx) = channel();
    let spawned = std::thread::Builder::new()
        .name("th-drift".into())
        .spawn(move || run(&rules, &sources, &stop, &tx, DRIFT_CHECK_EVERY));
    match spawned {
        Ok(_) => Some(rx),
        Err(e) => {
            tracing::error!(error = %e, "the model drift watch could not start: no ModelDrift rule can fire");
            None
        }
    }
}

/// The thread body: a pass at once, then one every `every`, until `stop` or a dropped receiver.
fn run(
    rules: &[AlertRule],
    sources: &DriftSources,
    stop: &AtomicBool,
    tx: &Sender<AlertSignal>,
    every: Duration,
) {
    let mut references = HashMap::new();
    let mut said: HashMap<String, String> = HashMap::new();
    while !stop.load(Ordering::Relaxed) {
        for (rule_id, outcome) in
            measure_pass(rules, sources, &mut references, vike_model::now_ms())
        {
            match outcome {
                Ok((reading, error_half)) => {
                    if let Some(why) = error_half {
                        say_once(&mut said, &rule_id, why);
                    }
                    if tx.send(reading).is_err() {
                        return;
                    }
                }
                Err(why) => say_once(&mut said, &rule_id, why),
            }
        }
        let mut slept = Duration::ZERO;
        while slept < every && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(STOP_POLL);
            slept += STOP_POLL;
        }
    }
}

/// A rule's failure, said at `warn` when its text differs from the last one said for that rule.
fn say_once(said: &mut HashMap<String, String>, rule_id: &str, why: String) {
    if said.get(rule_id) != Some(&why) {
        tracing::warn!(rule = rule_id, "model drift rule not measured: {why}");
        said.insert(rule_id.to_string(), why);
    }
}

/// What one rule's measurement gave: the reading (with what to say at `warn` about it: the realised-error half's
/// failure, else an empty live window), or why none.
pub(super) type RuleOutcome = Result<(AlertSignal, Option<String>), String>;

/// One measuring pass over `rules` at `now_ms`: each rule's id and its outcome. `references` caches each rule's
/// reference (its run's forecasts of the symbol, and its mean realised error once measured).
pub(super) fn measure_pass(
    rules: &[AlertRule],
    sources: &DriftSources,
    references: &mut HashMap<String, Reference>,
    now_ms: i64,
) -> Vec<(String, RuleOutcome)> {
    rules
        .iter()
        .filter(|rule| matches!(rule.trigger, RuleTrigger::ModelDrift { .. }))
        .map(|rule| (rule.id.clone(), measure_rule(rule, sources, references, now_ms)))
        .collect()
}

/// One `ModelDrift` rule's measurement at `now_ms` (any other rule is no reading).
fn measure_rule(
    rule: &AlertRule,
    sources: &DriftSources,
    references: &mut HashMap<String, Reference>,
    now_ms: i64,
) -> RuleOutcome {
    let RuleTrigger::ModelDrift {
        venue, symbol, mount_id, reference_run, window_ms, error, ..
    } = &rule.trigger
    else {
        return Err("not a model drift rule".to_string());
    };
    let reference = match references.entry(rule.id.clone()) {
        Entry::Occupied(known) => known.into_mut(),
        Entry::Vacant(slot) => slot.insert(Reference {
            points: reference_points(sources, reference_run, symbol)?,
            mean_error: None,
            mean_error_failed: None,
        }),
    };
    let start = now_ms.saturating_sub(*window_ms);
    // The reference must END before the window it is held against starts: a run over the window itself pulls
    // PSI, KS and the reference error toward "no drift", so a model that broke inside it reads as stable.
    let reference_end = reference.points.iter().map(|p| p.ts.saturating_add(p.horizon_ms)).max();
    if let Some(end) = reference_end.filter(|end| *end > start) {
        return Err(format!(
            "reference run {reference_run} reaches {end} ms (its last forecast plus its horizon), after \
             the monitored window starts at {start} ms: a reference must end before the window it is \
             held against, or the window is compared with itself. Name a run that ends by then, or a \
             shorter `window_ms`"
        ));
    }
    let live: Vec<ForecastPoint> = (sources.forecasts)(venue, symbol, start, now_ms)?
        .into_iter()
        .filter(|r| mount_id.as_ref().is_none_or(|m| *m == r.mount_id))
        .map(|r| ForecastPoint { ts: r.ts, value: r.value, horizon_ms: r.horizon_ms })
        .collect();
    let (error_reading, error_failure) = match error {
        None => (None, None),
        Some(spec) => match error_half(sources, venue, symbol, spec, reference, &live, now_ms) {
            Ok(reading) => (Some(reading), None),
            Err(why) => (None, Some(format!("its realised-error check: {why}"))),
        },
    };
    // An empty window is a reading that can never alarm: said, so a rule nothing feeds is not taken as watched.
    let quiet = live.is_empty().then(|| {
        format!(
            "no live forecast of {venue} {symbol} was read in the last {window_ms} ms, so this rule cannot fire: \
             check that the mount publishes forecasts, that this node journals them (the run profile's \
             sinks.forecasts, off by default) and that the datahub records this node's execution stream"
        )
    });
    let reading = drift_reading(&rule.id, &reference.points, &live, error_reading);
    Ok((AlertSignal::ModelDrift(Box::new(reading)), error_failure.or(quiet)))
}

/// A rule's reference: its run's forecasts of the rule's symbol, and their mean realised error once priced.
pub(super) struct Reference {
    points: Vec<ForecastPoint>,
    mean_error: Option<f64>,
    /// The last failed pricing of `mean_error`, held until its backoff ends.
    mean_error_failed: Option<PricingFailed>,
}

/// A failed reference pricing: its words (answered again, unread, until `due_ms`) and the ladder that set it, which
/// doubles per failure from [`DRIFT_CHECK_EVERY`] up to [`REFERENCE_RETRY_MAX`].
struct PricingFailed {
    why: String,
    due_ms: i64,
    ladder: ReconnectBackoff,
}

/// The longest a failed reference pricing waits before the bars are read again: six hours.
const REFERENCE_RETRY_MAX: Duration = Duration::from_millis(6 * vike_model::MS_PER_HOUR_U64);

/// The forecasts the finished run `run_id` saved for `symbol`, in publishing order.
fn reference_points(
    sources: &DriftSources,
    run_id: &str,
    symbol: &str,
) -> Result<Vec<ForecastPoint>, String> {
    let dir = vike_runs::finished_run_dir(&sources.runs, run_id).map_err(|e| e.to_string())?;
    let saved = vike_runs::read_forecasts(&dir).map_err(|e| e.to_string())?;
    let points: Vec<ForecastPoint> = saved
        .forecasts
        .iter()
        .filter(|row| row.symbol == symbol)
        .map(|row| ForecastPoint {
            ts: row.timestamp,
            value: row.forecast.value(),
            horizon_ms: row.forecast.horizon_ms(),
        })
        .collect();
    if points.is_empty() {
        return Err(format!("reference run {run_id} saved no forecast of {symbol}"));
    }
    Ok(points)
}

/// The reading of `live` against `reference`: the population stability index and the two-sample
/// Kolmogorov–Smirnov test over the forecast VALUES, and the
/// realised-error half as measured. Pure; an empty side answers NaN statistics (never a drift).
pub(super) fn drift_reading(
    rule_id: &str,
    reference: &[ForecastPoint],
    live: &[ForecastPoint],
    error: Option<ForecastErrorReading>,
) -> ModelDriftReading {
    let reference_values: Vec<f64> = reference.iter().map(|p| p.value).collect();
    let live_values: Vec<f64> = live.iter().map(|p| p.value).collect();
    let kolmogorov_smirnov = kolmogorov_smirnov_two_sample(&reference_values, &live_values);
    ModelDriftReading {
        rule_id: rule_id.to_string(),
        forecasts: live.len(),
        reference_forecasts: reference.len(),
        population_stability_index: population_stability_index(
            &reference_values,
            &live_values,
            POPULATION_STABILITY_INDEX_BINS,
        ),
        kolmogorov_smirnov_statistic: kolmogorov_smirnov.statistic,
        kolmogorov_smirnov_p_value: kolmogorov_smirnov.p_value,
        error,
    }
}

/// The realised-error half: the live errors' CUSUM against the reference's mean error (priced once, cached).
fn error_half(
    sources: &DriftSources,
    venue: &str,
    symbol: &str,
    spec: &ForecastErrorDrift,
    reference: &mut Reference,
    live: &[ForecastPoint],
    now_ms: i64,
) -> Result<ForecastErrorReading, String> {
    let interval_ms = vike_marketdata::interval::interval_ms(&spec.interval)
        .ok_or_else(|| format!("`{}` is not a bar interval", spec.interval))?;
    let series = SeriesId::per_symbol(StoreKindId::Bar, venue, symbol, Some(spec.interval.clone()));
    let closes = |points: &[ForecastPoint], end: i64| -> Result<Vec<(i64, f64)>, String> {
        let first = points.iter().map(|p| p.ts).min().unwrap_or(end);
        let bars = (sources.bars)(&series, first.saturating_sub(interval_ms), end)?;
        Ok(bars.iter().map(|b| (b.timestamp + interval_ms, b.close)).collect())
    };
    let reference_mean = match reference.mean_error {
        Some(mean) => mean,
        None => {
            // A failed pricing reads the WHOLE reference window's bars: it is not read again before its backoff.
            if let Some(failed) = reference.mean_error_failed.as_ref().filter(|f| now_ms < f.due_ms)
            {
                return Err(failed.why.clone());
            }
            let last = reference.points.iter().map(|p| p.ts + p.horizon_ms).max().unwrap_or(0);
            let priced = closes(&reference.points, last).and_then(|closes| {
                let errors = realised_errors(&reference.points, &closes, last);
                if errors.is_empty() {
                    return Err(format!(
                        "no reference forecast could be priced on the stored {} bars",
                        spec.interval
                    ));
                }
                Ok(compensated_sum(errors.iter().copied()) / errors.len() as f64)
            });
            match priced {
                Ok(mean) => {
                    reference.mean_error = Some(mean);
                    reference.mean_error_failed = None;
                    mean
                }
                Err(why) => {
                    let mut ladder = reference.mean_error_failed.take().map_or_else(
                        || ReconnectBackoff::new(DRIFT_CHECK_EVERY, REFERENCE_RETRY_MAX),
                        |failed| failed.ladder,
                    );
                    let delay_ms = i64::try_from(ladder.failed().as_millis()).unwrap_or(i64::MAX);
                    let due_ms = now_ms.saturating_add(delay_ms);
                    reference.mean_error_failed =
                        Some(PricingFailed { why: why.clone(), due_ms, ladder });
                    return Err(why);
                }
            }
        }
    };
    let errors = realised_errors(live, &closes(live, now_ms)?, now_ms);
    let mean = if errors.is_empty() {
        f64::NAN
    } else {
        compensated_sum(errors.iter().copied()) / errors.len() as f64
    };
    Ok(ForecastErrorReading {
        errors: errors.len(),
        mean,
        reference_mean,
        alarms: cusum_alarms(&errors, reference_mean, spec.allowance, spec.threshold).len(),
    })
}

/// Each forecast's absolute error against the return that followed it, in order, over `closes` (`(close time,
/// close)` ascending): the price at a time is the close of the last bar CLOSED by then, and a forecast whose
/// horizon ends after `as_of`, or that has no price at either end, is left out. Pure.
///
/// A forecast's `ts` is its DECISION instant (a bar step's close, decision 0271, live and backtest alike), so the
/// start price is the close of the bar it was decided on and the return measured is the one AFTER it, never the
/// bar the forecast was computed from.
pub(super) fn realised_errors(
    points: &[ForecastPoint],
    closes: &[(i64, f64)],
    as_of: i64,
) -> Vec<f64> {
    let price_at = |t: i64| {
        let n = closes.partition_point(|(close_time, _)| *close_time <= t);
        n.checked_sub(1).map(|i| closes[i].1)
    };
    points
        .iter()
        .filter(|p| p.ts + p.horizon_ms <= as_of)
        .filter_map(|p| {
            let start = price_at(p.ts).filter(|&price| price > 0.0)?;
            let end = price_at(p.ts + p.horizon_ms)?;
            Some((p.value - (end / start - 1.0)).abs())
        })
        .collect()
}
