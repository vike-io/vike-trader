//! `evaluation_indicator_rule` and `evaluation_event_rule`: indicator thresholds, fills, rejects, denials.

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
            comparison: Compare::Below,
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
    assert!(evaluation_indicator_rule(&rule, &mut st, &wrong, 1).is_none());
    assert_eq!(st.last_value, None);
    // 40 (seed, no prior) then 25: crosses below 30 → fires.
    assert!(evaluation_indicator_rule(&rule, &mut st, &s(40.0), 2).is_none());
    assert!(evaluation_indicator_rule(&rule, &mut st, &s(25.0), 3).is_some());
    // still below → no re-fire.
    assert!(evaluation_indicator_rule(&rule, &mut st, &s(20.0), 4).is_none());
}

// ---- Fill / OrderRejected events --------------------------------------------------------

#[test]
fn fill_rule_scoping_fires_only_for_the_matching_instrument() {
    let mut st = RuleState::default();
    let scoped = AlertRule::new(
        "f",
        RuleTrigger::Fill { venue: Some("binance".into()), symbol: Some("BTCUSDT".into()) },
    );
    assert!(
        evaluation_event_rule(&scoped, &mut st, &fill_event("binance", "BTCUSDT"), 1).is_some()
    );
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
    let reject = AlertEvent::OrderRejected {
        order_client_id: "c1",
        venue: "okx",
        refusal_id: None,
        reason: "x",
    };
    assert!(fire_event(&any, &reject, 5).is_none());
}

/// The body names the order, its venue and the refusal's words.
#[test]
fn order_rejected_rule_fires_on_both_reject_and_deny() {
    let rule = AlertRule::new("r", RuleTrigger::OrderRejected);
    let reject = AlertEvent::OrderRejected {
        order_client_id: "c1",
        venue: "bybit",
        refusal_id: None,
        reason: "insufficient balance",
    };
    let den = AlertEvent::OrderDenied {
        order_client_id: "c2",
        venue: "okx",
        refusal_id: None,
        reason: "risk gate",
    };
    let fired = fire_event(&rule, &reject, 1).unwrap();
    assert_eq!(fired.body, "order c1 on bybit rejected: insufficient balance");
    assert_eq!(fire_event(&rule, &den, 2).unwrap().body, "order c2 on okx denied: risk gate");
    // a denial's catalog id is shown in brackets before its words
    let den = AlertEvent::OrderDenied {
        order_client_id: "c3",
        venue: "okx",
        refusal_id: Some("order.risk.halted"),
        reason: "halted",
    };
    assert_eq!(
        fire_event(&rule, &den, 3).unwrap().body,
        "order c3 on okx denied [order.risk.halted]: halted"
    );
    // and so is a venue rejection's
    let reject = AlertEvent::OrderRejected {
        order_client_id: "c4",
        venue: "bybit",
        refusal_id: Some("order.venue.rejected"),
        reason: "too small",
    };
    assert_eq!(
        fire_event(&rule, &reject, 5).unwrap().body,
        "order c4 on bybit rejected [order.venue.rejected]: too small"
    );
    // an accepted fill never fires an order-rejected rule.
    assert!(fire_event(&rule, &fill_event("binance", "BTCUSDT"), 4).is_none());
}

/// **A `journal_growth` rule fires on the journal's crossing and on nothing else**, and its body states the size, the
/// segments and the rate, or that the rate is not measured yet; no other rule fires on it.
#[test]
fn journal_growth_rule_fires_on_the_journal_event_alone() {
    let rule = AlertRule::new("j", RuleTrigger::JournalGrowth);
    let flood = AlertEvent::JournalGrowth {
        total_bytes: 2_200 * 1024 * 1024,
        segments: 34,
        bytes_per_hour: Some(400 * 1024 * 1024),
    };
    assert_eq!(
        fire_event(&rule, &flood, 1).unwrap().body,
        "journal at 2200.0 MiB in 34 segment(s), growing 400.0 MiB an hour"
    );
    let early = AlertEvent::JournalGrowth {
        total_bytes: 3 * 1024 * 1024 * 1024,
        segments: 48,
        bytes_per_hour: None,
    };
    assert_eq!(
        fire_event(&rule, &early, 2).unwrap().body,
        "journal at 3072.0 MiB in 48 segment(s), its growth not measured yet"
    );
    assert!(fire_event(&rule, &fill_event("binance", "BTCUSDT"), 3).is_none());
    assert!(fire_event(&AlertRule::new("r", RuleTrigger::OrderRejected), &flood, 4).is_none());
}

/// **A `journal_fence_halt` rule fires on the node's halt and on nothing else**, its body the node's own words.
#[test]
fn journal_fence_halt_rule_fires_on_the_halt_event_alone() {
    let rule = AlertRule::new("halt", RuleTrigger::JournalFenceHalt);
    let halt =
        AlertEvent::JournalFenceHalt { fault: "the journal wrote nothing; restart the node" };
    assert_eq!(
        fire_event(&rule, &halt, 1).unwrap().body,
        "trading HALTED: the journal wrote nothing; restart the node"
    );
    let growth = AlertEvent::JournalGrowth { total_bytes: 1, segments: 1, bytes_per_hour: None };
    assert!(fire_event(&rule, &growth, 2).is_none());
    assert!(fire_event(&AlertRule::new("j", RuleTrigger::JournalGrowth), &halt, 3).is_none());
}
