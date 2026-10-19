//! The cooldown/once fire gate, driven through an unscoped fill rule.

use super::*;
use crate::rule::{AlertRule, RuleTrigger};

// ---- cooldown / once gating -------------------------------------------------------------

#[test]
fn cooldown_suppresses_refire_within_the_window() {
    let mut rule = AlertRule::new("f", RuleTrigger::Fill { venue: None, symbol: None });
    rule.cooldown_ms = 1000;
    let mut st = RuleState::default();
    let f = fill_event("binance", "BTCUSDT");
    assert!(eval_event_rule(&rule, &mut st, &f, 1000).is_some(), "first fill fires");
    assert!(eval_event_rule(&rule, &mut st, &f, 1500).is_none(), "within cooldown → suppressed");
    assert!(eval_event_rule(&rule, &mut st, &f, 2000).is_some(), "cooldown elapsed → fires");
}

#[test]
fn once_fires_at_most_a_single_time() {
    let mut rule = AlertRule::new("f", RuleTrigger::Fill { venue: None, symbol: None });
    rule.once = true;
    let mut st = RuleState::default();
    let f = fill_event("binance", "BTCUSDT");
    assert!(eval_event_rule(&rule, &mut st, &f, 1).is_some());
    assert!(eval_event_rule(&rule, &mut st, &f, 2).is_none(), "once ⇒ never again");
    assert!(eval_event_rule(&rule, &mut st, &f, 9_999).is_none());
}
