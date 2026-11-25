//! The model drift watch (book W9): a reference run on disk, a scripted forecast read and a scripted bar read,
//! measured and evaluated through the REAL `AlertMount`. A synthetic SHIFT of the live forecasts fires one
//! alert; a stable stream drawn from the reference's own distribution fires none; a forecast whose error grew
//! raises the realised-error CUSUM.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;
use std::time::Duration;

use vike_alerting::{
    AlertRule, AlertRuleSet, AlertSignal, ForecastErrorDrift, InProcessSink, RuleTrigger,
};
use vike_data::ExecForecastRow;
use vike_marketdata::Bar;
use vike_model::{Forecast, ForecastRow};

use super::super::drift_watch::{
    DriftSources, ForecastPoint, measure_pass, realised_errors, spawn_drift_watch,
};

const MINUTE: i64 = 60_000;
const REFERENCE_RUN: &str = "1794124800-reference-0";

/// A deterministic uniform draw in `[low, high)` (a 64-bit LCG): no randomness crate, no platform math.
fn uniform(seed: u64, n: usize, low: f64, high: f64) -> Vec<f64> {
    let mut state = seed;
    (0..n)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            low + (high - low) * ((state >> 11) as f64 / (1u64 << 53) as f64)
        })
        .collect()
}

/// A FINISHED run under `runs` whose `forecasts.json` holds `values` for BTCUSDT, one a minute from minute 1.
fn reference_run(runs: &Path, values: &[f64]) {
    let dir = runs.join(REFERENCE_RUN);
    std::fs::create_dir_all(&dir).expect("run dir");
    let forecasts: Vec<ForecastRow> = values
        .iter()
        .enumerate()
        .map(|(k, &value)| ForecastRow {
            timestamp: (k as i64 + 1) * MINUTE,
            symbol: "BTCUSDT".into(),
            forecast: Forecast::new(value, MINUTE, None).expect("a forecast"),
        })
        .collect();
    let saved = vike_runs::RunForecasts {
        schema: vike_runs::FORECASTS_SCHEMA,
        source_len: forecasts.len(),
        forecasts,
    };
    std::fs::write(dir.join(vike_runs::FORECASTS_FILE), serde_json::to_string(&saved).unwrap())
        .expect("forecasts");
    let manifest = format!(
        "{{\"run_id\":\"{REFERENCE_RUN}\",\"kind\":\"backtest\",\"produced_by\":\"backtest\",\"started_at\":\
         \"2026-11-16T00:00:00Z\",\"finished_at\":\"2026-11-16T00:00:01Z\",\"git_sha\":null,\
         \"config\":{{}},\"detail\":{{}}}}"
    );
    std::fs::write(dir.join(vike_runs::MANIFEST_FILE), manifest).expect("manifest");
}

/// The live forecasts `values`, one a minute from minute `first`, as the datahub would answer them.
fn live_rows(first: i64, values: &[f64]) -> Vec<ExecForecastRow> {
    values
        .iter()
        .enumerate()
        .map(|(k, &value)| ExecForecastRow {
            ts: (first + k as i64) * MINUTE,
            mount_id: "m1".into(),
            value,
            horizon_ms: MINUTE,
            confidence: None,
        })
        .collect()
}

/// One-minute bars from minute 0, the bar of minute `k` closing at `100 + k`.
fn bars(minutes: i64) -> Vec<Bar> {
    (0..minutes)
        .map(|k| Bar {
            timestamp: k * MINUTE,
            open: 100.0 + k as f64,
            high: 100.0 + k as f64,
            low: 100.0 + k as f64,
            close: 100.0 + k as f64,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect()
}

/// Scripted reads over `live` and `bars`, each filtered to the asked window as the datahub's would be.
fn sources(runs: &Path, live: Vec<ExecForecastRow>, bars: Vec<Bar>) -> DriftSources {
    DriftSources {
        runs: runs.to_path_buf(),
        forecasts: Arc::new(move |venue: &str, symbol: &str, start: i64, end: i64| {
            assert_eq!((venue, symbol), ("binance", "BTCUSDT"));
            Ok(live.iter().filter(|r| (start..=end).contains(&r.ts)).cloned().collect())
        }),
        bars: Arc::new(move |_series: &vike_data::SeriesId, start: i64, end: i64| {
            Ok(bars.iter().filter(|b| (start..=end).contains(&b.timestamp)).cloned().collect())
        }),
    }
}

fn drift_rule(error: Option<ForecastErrorDrift>) -> AlertRule {
    AlertRule::new(
        "drift",
        RuleTrigger::ModelDrift {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            mount_id: None,
            reference_run: REFERENCE_RUN.into(),
            window_ms: 200 * MINUTE,
            min_forecasts: 30,
            population_stability_index_above: 0.25,
            kolmogorov_smirnov_p_value_below: 0.01,
            error,
        },
    )
}

/// What the REAL alert mount delivers for the readings of one pass at `now_ms` over `sources`.
fn delivered(rule: &AlertRule, sources: &DriftSources, now_ms: i64) -> Vec<String> {
    let sink = InProcessSink::new();
    let set = AlertRuleSet { rules: vec![rule.clone()], ..Default::default() };
    let (tx, rx) = channel();
    let mut mount = crate::alerts::maybe_mount_with_sinks(Some(set), vec![Box::new(sink.clone())])
        .expect("one enabled rule mounts")
        .with_drift_readings(Some(rx));
    for (_, outcome) in
        measure_pass(std::slice::from_ref(rule), sources, &mut HashMap::new(), now_ms)
    {
        let (reading, error_failure) = outcome.expect("measured");
        assert_eq!(error_failure, None);
        tx.send(reading).expect("the mount holds the receiver");
    }
    mount.on_drift_readings(now_ms);
    sink.drain().into_iter().map(|alert| alert.body).collect()
}

#[test]
fn a_shifted_forecast_stream_fires_and_a_stable_one_does_not() {
    let runs = tempfile::tempdir().expect("runs root");
    reference_run(runs.path(), &uniform(1, 300, -0.01, 0.01));
    let now = 600 * MINUTE;
    let rule = drift_rule(None);

    let stable = sources(runs.path(), live_rows(401, &uniform(2, 200, -0.01, 0.01)), Vec::new());
    assert_eq!(delivered(&rule, &stable, now), Vec::<String>::new(), "the same distribution");

    let shifted = sources(runs.path(), live_rows(401, &uniform(2, 200, 0.02, 0.04)), Vec::new());
    let alerts = delivered(&rule, &shifted, now);
    let [body] = alerts.as_slice() else { panic!("one alert for the shift: {alerts:?}") };
    assert!(body.contains("model drift on binance BTCUSDT"), "{body}");
    assert!(body.contains("200 live forecasts against 300"), "{body}");
}

#[test]
fn a_mount_filter_and_a_missing_reference_are_honoured() {
    let runs = tempfile::tempdir().expect("runs root");
    let mut rule = drift_rule(None);
    let missing = sources(runs.path(), Vec::new(), Vec::new());
    let [(_, outcome)] =
        <[_; 1]>::try_from(measure_pass(&[rule.clone()], &missing, &mut HashMap::new(), MINUTE))
            .expect("one rule, one outcome");
    assert!(outcome.is_err(), "no reference run is no reading, never a silent one");

    reference_run(runs.path(), &uniform(1, 300, -0.01, 0.01));
    if let RuleTrigger::ModelDrift { mount_id, .. } = &mut rule.trigger {
        *mount_id = Some("another-mount".into());
    }
    let shifted = sources(runs.path(), live_rows(401, &uniform(2, 200, 0.02, 0.04)), Vec::new());
    let now = 600 * MINUTE;
    let outcomes = measure_pass(&[rule], &shifted, &mut HashMap::new(), now);
    let Ok((AlertSignal::ModelDrift(reading), _)) = &outcomes[0].1 else {
        panic!("{:?}", outcomes[0].1)
    };
    assert_eq!(reading.forecasts, 0, "another mount's forecasts are not this rule's");
}

#[test]
fn the_realised_error_is_point_in_time_and_its_cusum_alarms_on_a_grown_error() {
    // Minute j's forecast looks one minute ahead: the price then is the close of the bar that closed by then.
    let exact = |j: i64| 1.0 / (99.0 + j as f64);
    let points: Vec<ForecastPoint> = (1..=3)
        .map(|j| ForecastPoint { ts: j * MINUTE, value: exact(j), horizon_ms: MINUTE })
        .collect();
    let closes: Vec<(i64, f64)> =
        bars(10).iter().map(|b| (b.timestamp + MINUTE, b.close)).collect();
    let errors = realised_errors(&points, &closes, 3 * MINUTE);
    assert_eq!(errors.len(), 2, "the third forecast's horizon ends after the reading: {errors:?}");
    assert!(errors.iter().all(|e| *e < 1e-12), "exact forecasts: {errors:?}");

    // The reference forecasted exactly; the live model is off by 0.05 on every forecast.
    let runs = tempfile::tempdir().expect("runs root");
    let reference: Vec<f64> = (1..=100).map(exact).collect();
    reference_run(runs.path(), &reference);
    let live: Vec<f64> = (101..=200).map(|j| exact(j) + 0.05).collect();
    let rule = drift_rule(Some(ForecastErrorDrift {
        interval: "1m".into(),
        allowance: 0.01,
        threshold: 0.1,
    }));
    let watched = sources(runs.path(), live_rows(101, &live), bars(300));
    // The 200-minute window starts at minute 101, where the reference's last forecast ends.
    let now = 301 * MINUTE;
    let outcomes = measure_pass(std::slice::from_ref(&rule), &watched, &mut HashMap::new(), now);
    let Ok((AlertSignal::ModelDrift(reading), None)) = &outcomes[0].1 else {
        panic!("{:?}", outcomes[0].1)
    };
    let error = reading.error.as_ref().expect("the error half was measured");
    assert_eq!(error.errors, 100);
    assert!(error.reference_mean < 1e-12, "{error:?}");
    assert!((error.mean - 0.05).abs() < 1e-9 && error.alarms > 0, "{error:?}");
    let alerts = delivered(&rule, &watched, now);
    assert!(alerts.iter().any(|b| b.contains("realised error CUSUM alarmed")), "{alerts:?}");
}

/// A reference run that reaches into the monitored window is refused in words, never measured: the window would
/// be compared with itself. One that ends exactly where the window starts is measured.
#[test]
fn a_reference_that_does_not_end_before_the_window_is_refused() {
    let runs = tempfile::tempdir().expect("runs root");
    // Forecasts at minutes 1..=300, the last one's horizon ending at minute 301.
    reference_run(runs.path(), &uniform(1, 300, -0.01, 0.01));
    let rule = drift_rule(None);
    let watched = sources(runs.path(), live_rows(401, &uniform(2, 200, -0.01, 0.01)), Vec::new());
    let at =
        |now: i64| measure_pass(std::slice::from_ref(&rule), &watched, &mut HashMap::new(), now);
    // The 200-minute window from minute 300 holds the reference's own last minute.
    let overlapping = at(500 * MINUTE);
    let Err(why) = &overlapping[0].1 else {
        panic!("measured over itself: {:?}", overlapping[0].1)
    };
    assert!(why.contains("a reference must end before the window it is held against"), "{why}");
    assert!(at(501 * MINUTE)[0].1.is_ok(), "the window starts where the reference ends");
}

/// A forecast stamped at its decision bar's CLOSE (as the core and the simulator stamp a bar step's) is held
/// against the bar AFTER it: one that only repeats the return of the bar it was computed from is not exact.
#[test]
fn the_realised_error_never_holds_a_forecast_against_the_bar_it_was_computed_from() {
    let closes: Vec<(i64, f64)> =
        bars(10).iter().map(|b| (b.timestamp + MINUTE, b.close)).collect();
    // Decided at the close of the bar of minute j - 1 (at j minutes), which closed at 99 + j.
    let repeat = |j: i64| (99.0 + j as f64) / (98.0 + j as f64) - 1.0;
    let next = |j: i64| (100.0 + j as f64) / (99.0 + j as f64) - 1.0;
    let at = |value: fn(i64) -> f64| -> Vec<f64> {
        let points: Vec<ForecastPoint> = (2..=5)
            .map(|j| ForecastPoint { ts: j * MINUTE, value: value(j), horizon_ms: MINUTE })
            .collect();
        realised_errors(&points, &closes, 10 * MINUTE)
    };
    assert!(at(next).iter().all(|e| *e < 1e-12), "the bar after the decision: exact");
    assert!(at(repeat).iter().all(|e| *e > 1e-6), "its own bar repeated: an error");
}

#[test]
fn the_watch_spawns_only_for_a_drift_rule_and_measures_at_once() {
    let runs = tempfile::tempdir().expect("runs root");
    reference_run(runs.path(), &uniform(1, 300, -0.01, 0.01));
    let stop = Arc::new(AtomicBool::new(false));
    let price_rule = AlertRule::new(
        "p",
        RuleTrigger::Price {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            comparison: vike_alerting::Compare::Above,
            level: 1.0,
        },
    );
    let none = spawn_drift_watch(
        &[price_rule],
        sources(runs.path(), Vec::new(), Vec::new()),
        Arc::clone(&stop),
    );
    assert!(none.is_none(), "no ModelDrift rule, no thread");

    let rx = spawn_drift_watch(
        &[drift_rule(None)],
        sources(runs.path(), Vec::new(), Vec::new()),
        Arc::clone(&stop),
    )
    .expect("a ModelDrift rule starts the watch");
    let first = rx.recv_timeout(Duration::from_secs(10)).expect("a reading at once");
    assert!(matches!(first, AlertSignal::ModelDrift(ref r) if r.rule_id == "drift"), "{first:?}");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// L2: a rule nothing feeds is SAID, never taken as watched: a pass whose live window holds no forecast answers its
/// reading (no verdict) with a note to say at `warn`, which a fed window does not carry.
#[test]
fn a_rule_nothing_feeds_is_said_and_a_fed_one_is_not() {
    let runs = tempfile::tempdir().expect("runs root");
    reference_run(runs.path(), &uniform(1, 300, -0.01, 0.01));
    let now = 600 * MINUTE;
    let empty = sources(runs.path(), Vec::new(), Vec::new());
    let outcomes = measure_pass(&[drift_rule(None)], &empty, &mut HashMap::new(), now);
    let Ok((AlertSignal::ModelDrift(reading), Some(note))) = &outcomes[0].1 else {
        panic!("{:?}", outcomes[0].1)
    };
    assert_eq!(reading.forecasts, 0);
    assert!(note.contains("no live forecast of binance BTCUSDT"), "{note}");

    let fed = sources(runs.path(), live_rows(401, &uniform(2, 200, -0.01, 0.01)), Vec::new());
    let outcomes = measure_pass(&[drift_rule(None)], &fed, &mut HashMap::new(), now);
    assert!(matches!(&outcomes[0].1, Ok((_, None))), "{:?}", outcomes[0].1);
}

/// L5: a reference whose realised error cannot be priced is NOT read again at every pass. Its bars are the reference's
/// whole window, so a failure is answered from memory, in the same words, until a backoff that doubles per failure.
#[test]
fn a_failed_reference_pricing_is_not_read_again_before_its_backoff() {
    let runs = tempfile::tempdir().expect("runs root");
    reference_run(runs.path(), &uniform(1, 300, -0.01, 0.01));
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    let mut failing =
        sources(runs.path(), live_rows(401, &uniform(2, 200, -0.01, 0.01)), Vec::new());
    failing.bars = Arc::new(move |_series: &vike_data::SeriesId, _start: i64, _end: i64| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err("the datahub refused the read".to_string())
    });
    let rule = drift_rule(Some(ForecastErrorDrift {
        interval: "1m".into(),
        allowance: 0.01,
        threshold: 0.1,
    }));
    let mut references = HashMap::new();
    let every = super::super::drift_watch::DRIFT_CHECK_EVERY.as_millis() as i64;
    let now = 600 * MINUTE;
    let mut pass = |at: i64| {
        let outcomes = measure_pass(std::slice::from_ref(&rule), &failing, &mut references, at);
        let Ok((_, Some(note))) = &outcomes[0].1 else { panic!("{:?}", outcomes[0].1) };
        assert!(note.contains("the datahub refused the read"), "{note}");
        reads.load(std::sync::atomic::Ordering::SeqCst)
    };
    assert_eq!(pass(now), 1, "the first pass reads");
    assert_eq!(pass(now + MINUTE), 1, "inside the backoff: answered from memory");
    assert_eq!(pass(now + every), 2, "the backoff ended: read again");
    assert_eq!(pass(now + 2 * every), 2, "the second failure doubled the backoff");
    assert_eq!(pass(now + 3 * every), 3);
}
