//! `evaluation_signal_rule` over the model drift reading: latched on the rising edge, re-armed by a stable
//! reading, no verdict below the sample floor, and only the rule the reading was measured for.

use super::*;
use crate::rule::{AlertRule, ForecastErrorDrift, RuleTrigger};

fn rule(id: &str) -> AlertRule {
    AlertRule::new(
        id,
        RuleTrigger::ModelDrift {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            mount_id: None,
            reference_run: "1794124800-abc-0".into(),
            window_ms: 86_400_000,
            min_forecasts: 30,
            population_stability_index_above: 0.25,
            kolmogorov_smirnov_p_value_below: 0.01,
            error: None,
        },
    )
}

/// A reading for rule `id` over `forecasts` live forecasts, with the two statistics given.
fn reading(
    id: &str,
    forecasts: usize,
    population_stability_index: f64,
    kolmogorov_smirnov_p_value: f64,
) -> AlertSignal {
    AlertSignal::ModelDrift(Box::new(ModelDriftReading {
        rule_id: id.into(),
        forecasts,
        reference_forecasts: 500,
        population_stability_index,
        kolmogorov_smirnov_statistic: 0.4,
        kolmogorov_smirnov_p_value,
        error: None,
    }))
}

#[test]
fn a_drift_fires_once_and_a_stable_reading_re_arms_it() {
    let rule = rule("d");
    let mut st = RuleState::default();
    let stable = reading("d", 200, 0.02, 0.6);
    let shifted = reading("d", 200, 0.9, 1e-12);
    assert!(evaluation_signal_rule(&rule, &mut st, &stable, 1).is_none(), "stable: no alert");
    let fired = evaluation_signal_rule(&rule, &mut st, &shifted, 2).expect("the shift fires");
    assert!(fired.body.contains("model drift on binance BTCUSDT"), "{}", fired.body);
    assert!(
        fired.body.contains("population stability index 0.900")
            && fired.body.contains("Kolmogorov–Smirnov D 0.400"),
        "{}",
        fired.body
    );
    assert!(fired.body.contains("200 live forecasts against 500"), "{}", fired.body);
    assert!(
        evaluation_signal_rule(&rule, &mut st, &shifted, 3).is_none(),
        "latched: no second page"
    );
    assert!(evaluation_signal_rule(&rule, &mut st, &stable, 4).is_none(), "re-armed, silently");
    assert!(
        evaluation_signal_rule(&rule, &mut st, &shifted, 5).is_some(),
        "a new episode pages again"
    );
}

#[test]
fn either_statistic_alone_is_a_drift_at_its_bound() {
    let rule = rule("d");
    assert!(
        fire_signal(&rule, &reading("d", 200, 0.25, 0.5), 1).is_some(),
        "the population stability index at its bound"
    );
    assert!(
        fire_signal(&rule, &reading("d", 200, 0.0, 0.009), 1).is_some(),
        "the Kolmogorov–Smirnov p-value below its bound"
    );
    assert!(fire_signal(&rule, &reading("d", 200, 0.249, 0.01), 1).is_none(), "both inside");
}

#[test]
fn too_few_forecasts_is_no_verdict_and_keeps_the_latch() {
    let rule = rule("d");
    let mut st = RuleState::default();
    assert!(evaluation_signal_rule(&rule, &mut st, &reading("d", 29, 5.0, 0.0), 1).is_none());
    assert!(!st.latched, "a thin window decides nothing");
    assert!(evaluation_signal_rule(&rule, &mut st, &reading("d", 30, 5.0, 0.0), 2).is_some());
    // A thin STABLE window after the fire must not re-arm it either.
    assert!(evaluation_signal_rule(&rule, &mut st, &reading("d", 3, 0.0, 1.0), 3).is_none());
    assert!(st.latched, "still latched");
    assert!(evaluation_signal_rule(&rule, &mut st, &reading("d", 30, 5.0, 0.0), 4).is_none());
}

#[test]
fn a_reading_fires_only_the_rule_it_was_measured_for_and_nan_is_never_a_drift() {
    assert!(fire_signal(&rule("a"), &reading("b", 200, 5.0, 0.0), 1).is_none(), "another rule's");
    assert!(fire_signal(&rule("a"), &reading("a", 200, f64::NAN, f64::NAN), 1).is_none(), "NaN");
}

#[test]
fn a_realised_error_alarm_alone_fires_and_says_so() {
    let mut rule = rule("e");
    if let RuleTrigger::ModelDrift { error, .. } = &mut rule.trigger {
        *error =
            Some(ForecastErrorDrift { interval: "1m".into(), allowance: 0.001, threshold: 0.01 });
    }
    let with = |alarms: usize| {
        AlertSignal::ModelDrift(Box::new(ModelDriftReading {
            rule_id: "e".into(),
            forecasts: 200,
            reference_forecasts: 500,
            population_stability_index: 0.01,
            kolmogorov_smirnov_statistic: 0.05,
            kolmogorov_smirnov_p_value: 0.9,
            error: Some(ForecastErrorReading {
                errors: 180,
                mean: 0.004,
                reference_mean: 0.001,
                alarms,
            }),
        }))
    };
    assert!(fire_signal(&rule, &with(0), 1).is_none(), "no alarm, stable values: nothing");
    let fired = fire_signal(&rule, &with(2), 1).expect("the error alarm fires");
    assert!(fired.body.contains("realised error CUSUM alarmed 2 time(s)"), "{}", fired.body);
    assert!(
        !fired.body.contains("population stability index"),
        "only the reason that holds: {}",
        fired.body
    );
}
