//! `eval_indicator_rule` and `eval_event_rule`: indicator thresholds, fills, rejects, denials.

use super::*;
use crate::rule::{AlertRule, Compare, RuleTrigger};

// ---- Indicator threshold ----------------------------------------------------------------

#[test]
fn indicator_rule_matches_instrument_and_output_then_crosses() {
    let rule = AlertRule::new(
        "i",
        RuleTrigger::Indicator {
            venue: "binance".into(),
            symbol: "ETHUSDT".into(),
            indicator: "rsi".into(),
            output: 0,
            params: vec![14.0],
            op: Compare::Below,
            threshold: 30.0,
        },
    );
    let mut st = RuleState::default();
    let s = |v: f64| IndicatorSample {
        venue: "binance".into(),
        symbol: "ETHUSDT".into(),
        indicator: "rsi".into(),
        output: 0,
        value: v,
    };
    // Wrong indicator / output / symbol never fire and never touch state.
    let wrong = IndicatorSample {
        venue: "binance".into(),
        symbol: "ETHUSDT".into(),
        indicator: "macd".into(),
        output: 0,
        value: 5.0,
    };
    assert!(eval_indicator_rule(&rule, &mut st, &wrong, 1).is_none());
    assert_eq!(st.last_value, None);
    // 40 (seed, no prior) then 25: crosses below 30 → fires.
    assert!(eval_indicator_rule(&rule, &mut st, &s(40.0), 2).is_none());
    assert!(eval_indicator_rule(&rule, &mut st, &s(25.0), 3).is_some());
    // still below → no re-fire.
    assert!(eval_indicator_rule(&rule, &mut st, &s(20.0), 4).is_none());
}

// ---- Fill / OrderRejected events --------------------------------------------------------

#[test]
fn fill_rule_scoping_fires_only_for_the_matching_instrument() {
    let mut st = RuleState::default();
    let scoped = AlertRule::new(
        "f",
        RuleTrigger::Fill { venue: Some("binance".into()), symbol: Some("BTCUSDT".into()) },
    );
    assert!(eval_event_rule(&scoped, &mut st, &fill_event("binance", "BTCUSDT"), 1).is_some());
    assert!(
        fire_event(&scoped, &fill_event("okx", "BTCUSDT"), 2).is_none(),
        "wrong venue must not fire"
    );
    assert!(
        fire_event(&scoped, &fill_event("binance", "ETHUSDT"), 3).is_none(),
        "wrong symbol must not fire"
    );
    // Unscoped fires for any fill.
    let any = AlertRule::new("f2", RuleTrigger::Fill { venue: None, symbol: None });
    assert!(fire_event(&any, &fill_event("okx", "SOLUSDT"), 4).is_some());
    // A non-fill event never fires a fill rule.
    let rej = AlertEvent::OrderRejected { client_order_id: "c1", reason: "x" };
    assert!(fire_event(&any, &rej, 5).is_none());
}

#[test]
fn order_rejected_rule_fires_on_both_reject_and_deny() {
    let rule = AlertRule::new("r", RuleTrigger::OrderRejected);
    let rej = AlertEvent::OrderRejected { client_order_id: "c1", reason: "insufficient balance" };
    let den = AlertEvent::OrderDenied { client_order_id: "c2", reason: "risk gate" };
    let fired = fire_event(&rule, &rej, 1).unwrap();
    assert!(fired.body.contains("insufficient balance"));
    assert!(fire_event(&rule, &den, 2).is_some());
    // an accepted fill never fires an order-rejected rule.
    assert!(fire_event(&rule, &fill_event("binance", "BTCUSDT"), 3).is_none());
}
