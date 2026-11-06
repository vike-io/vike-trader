//! The `SeriesStale` rule: what its body names, its prefix scope, cross-talk and its cooldown;
//! and the `SeriesSlow` rule's body and scope.

use super::*;
use crate::rule::{AlertRule, FeedState, RuleTrigger};

/// The 2026-08-05 shape: a series that was writing 250k rows/min stops, and the alert NAMES it.
/// A venue-scoped `Feed` rule cannot say which series — that is the whole reason this variant
/// exists.
#[test]
fn an_unscoped_rule_fires_for_any_series_and_names_it() {
    let rule = AlertRule::new("s", RuleTrigger::SeriesStale { series_prefix: None });
    let fired =
        fire_signal(&rule, &stale("book/polymarket/0xtok", Some(19 * 60_000), 3_800_000), 1)
            .expect("an unscoped rule fires for any silent series");
    assert!(fired.body.contains("book/polymarket/0xtok"), "{}", fired.body);
    assert!(fired.body.contains("1140s"), "the silence duration is the diagnosis: {}", fired.body);
    assert!(fired.body.contains("3800000"), "…and so is the row count it had: {}", fired.body);
}

/// Never-started reads differently from stopped: the second is a venue stream that died, the
/// first is a subscription that was accepted and never served.
#[test]
fn a_never_started_series_reads_as_never_started_not_as_stale_for_zero() {
    let rule = AlertRule::new("s", RuleTrigger::SeriesStale { series_prefix: None });
    let fired = fire_signal(&rule, &stale("depth/binance/BTCUSDT", None, 0), 1).unwrap();
    assert!(fired.body.contains("NEVER received a row"), "{}", fired.body);
    assert!(fired.body.contains("stream name"), "{}", fired.body);
}

/// A PREFIX scope, because a Polymarket token id does not exist yet when the rule is written.
#[test]
fn the_scope_is_a_prefix_so_a_rotating_family_is_expressible() {
    let rule = AlertRule::new(
        "s",
        RuleTrigger::SeriesStale { series_prefix: Some("book/polymarket/".into()) },
    );
    // a token minted after the rule was written still matches.
    assert!(fire_signal(&rule, &stale("book/polymarket/9911", Some(600_000), 5), 1).is_some());
    // a different venue does not.
    assert!(fire_signal(&rule, &stale("trade/binance/BTCUSDT", Some(600_000), 5), 2).is_none());
    // …nor a different KIND of the same venue (the prefix carries the kind too).
    assert!(fire_signal(&rule, &stale("trade/polymarket/9911", Some(600_000), 5), 3).is_none());
}

/// Cross-talk in both directions: another signal must not fire a stale rule, and a stale signal
/// must not fire another rule.
#[test]
fn a_series_stale_signal_and_rule_do_not_cross_with_the_other_kinds() {
    let stale_rule = AlertRule::new("s", RuleTrigger::SeriesStale { series_prefix: None });
    assert!(
        fire_signal(&stale_rule, &AlertSignal::Feed { venue: "binance".into(), degraded: true }, 1)
            .is_none()
    );

    let feed_rule =
        AlertRule::new("f", RuleTrigger::Feed { venue: None, state: FeedState::Degraded });
    assert!(fire_signal(&feed_rule, &stale("trade/binance/BTCUSDT", Some(1), 1), 2).is_none());
}

/// The rule-level cooldown gates a series-stale rule like any other discrete signal. ⚠ It is
/// PER RULE, not per series — which is exactly why `vike_recorder::liveness::SilenceWatch` owns
/// the per-series repeat gate instead of leaning on this one: with six series silent at once, a
/// rule cooldown would page for the first and swallow the other five.
#[test]
fn the_rule_cooldown_is_per_rule_which_is_why_the_producer_gates_per_series() {
    let mut rule = AlertRule::new("s", RuleTrigger::SeriesStale { series_prefix: None });
    rule.cooldown_ms = 60_000;
    let mut st = RuleState::default();
    assert!(eval_signal_rule(&rule, &mut st, &stale("a/v/s", Some(1), 1), 1_000).is_some());
    assert!(
        eval_signal_rule(&rule, &mut st, &stale("b/v/s", Some(1), 1), 1_001).is_none(),
        "a DIFFERENT series is still suppressed by the same rule's cooldown"
    );
}

/// `SeriesSlow`'s body is its whole diagnosis, because this series IS receiving rows: what is
/// wrong is the PAIR of rates, observed against declared, and the governor beside them is what
/// tells a broken lane from a dead market. Both rates render to two decimals (an integer would
/// print the broken lane as "0"), the window is named, and the scope is a PREFIX, as for the
/// other two recorder rules.
#[test]
fn a_series_slow_body_names_both_rates_the_window_and_the_governor_and_scopes_by_prefix() {
    let rule = AlertRule::new("s", RuleTrigger::SeriesSlow { series_prefix: None });
    let fired = fire_signal(&rule, &slow("depth/binance/BTCUSDT.P"), 1)
        .expect("an unscoped rule fires for any slow series");
    let b = &fired.body;
    assert!(b.contains("series depth/binance/BTCUSDT.P "), "the series is named: {b}");
    assert!(b.contains("running at 0.42/s"), "the observed rate, to two decimals: {b}");
    assert!(b.contains("an expected 10.00/s"), "the DECLARED rate, to two decimals: {b}");
    assert!(b.contains(" over 900s "), "the window both rates were taken over: {b}");
    assert!(
        b.contains("while trade/binance/BTCUSDT.P ran at 20.30/s"),
        "the governor and ITS rate, to two decimals: {b}"
    );

    let scoped = AlertRule::new(
        "s",
        RuleTrigger::SeriesSlow { series_prefix: Some("depth/binance/".into()) },
    );
    assert!(
        fire_signal(&scoped, &slow("depth/binance/ETHUSDT.P"), 2).is_some(),
        "a series under the prefix matches"
    );
    assert!(
        fire_signal(&scoped, &slow("depth/okx/BTC-USDT-SWAP"), 3).is_none(),
        "a different venue does not"
    );
    assert!(
        fire_signal(&scoped, &slow("trade/binance/BTCUSDT.P"), 4).is_none(),
        "nor a different KIND of the same venue (the prefix carries the kind too)"
    );
}
