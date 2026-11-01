//! [`AlertEngine`]'s tests: the vike-free watchdog path end to end, the OFF state, and the fold.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::testkit::{price_rule, snap_mark};
use crate::{
    AlertEngine, AlertRule, AlertSignal, AlertSink, AlertTargets, Compare, FiredAlert,
    InProcessSink, IndicatorSample, RuleTrigger,
};

/// The watchdog path end to end in a build with no vike crate in it — the crate's own reason for
/// existing. The roster lane runs it: with no feature left, there is only the one configuration to
/// build. ⚠ This called a standalone lane "the only build that sees the crate's own reason for
/// existing" until 2026-09-28. That stopped being true when `core` went on 2026-09-23, and the
/// lane — which from then on re-ran this same build beside the roster's — was deleted on
/// 2026-09-28.
/// **The property the crate was split out for, executed rather than asserted in prose.** A
/// build with no vike crate in it holds a real `AlertEngine`, folds a real rule, and delivers a
/// real `FiredAlert` naming the series — the watchdog path, end to end, minus the network.
#[test]
fn a_vike_free_build_folds_a_series_stale_rule_all_the_way_to_a_sink() {
    let inbox = InProcessSink::new();
    let rule =
        AlertRule::new("recorder-series-stale", RuleTrigger::SeriesStale { series_prefix: None });
    let mut engine = AlertEngine::new(vec![rule]).with_sink(Box::new(inbox.clone()));
    assert!(engine.is_active());

    let fired = engine.on_signal(
        &AlertSignal::SeriesStale {
            series: "book/polymarket/0xtok".into(),
            silent_for_ms: Some(19 * 60_000),
            rows: 3_800_000,
        },
        1_000,
    );
    assert_eq!(fired.len(), 1, "one silent series ⇒ one alert");
    assert_eq!(fired[0].rule_id, "recorder-series-stale");

    let delivered = inbox.drain();
    assert_eq!(delivered.len(), 1, "…and it reached the sink, not just the return value");
    assert!(delivered[0].body.contains("book/polymarket/0xtok"), "{}", delivered[0].body);
}

/// The OFF state is unchanged by the engine leaving `core`: no rules ⇒ no evaluation, no sink
/// touched, on the signal path too.
#[test]
fn an_empty_engine_is_still_inert_on_the_signal_path() {
    let inbox = InProcessSink::new();
    let mut engine = AlertEngine::new(Vec::new()).with_sink(Box::new(inbox.clone()));
    assert!(!engine.is_active());
    assert!(
        engine
            .on_signal(
                &AlertSignal::SeriesStale {
                    series: "trade/binance/BTCUSDT".into(),
                    silent_for_ms: None,
                    rows: 0
                },
                1
            )
            .is_empty()
    );
    assert!(inbox.is_empty());
}

// The rest of the engine's tests, over the evaluator's own input types (`SnapshotFacts`,
// `AlertEvent`) — no vike type here either. ⚠ This said they named a vike type behind the `core`
// feature, and that the default CI lane compiled this crate WITH `core` because `vike-ops` enabled
// it, until 2026-09-28: the feature went on 2026-09-23, and `vike-ops`' edge on this crate on
// 2026-09-25.

/// A sink that just counts deliveries — to prove the OFF path never dispatches.
#[derive(Clone, Default)]
struct CountingSink {
    n: Arc<AtomicUsize>,
}
impl AlertSink for CountingSink {
    fn deliver(&self, _alert: &FiredAlert) {
        self.n.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn empty_engine_is_inert_and_touches_no_sink() {
    // The OFF / byte-identical guarantee: no rules ⇒ every input path returns empty AND never
    // dispatches to a sink.
    let counter = CountingSink::default();
    let mut engine = AlertEngine::new(Vec::new()).with_sink(Box::new(counter.clone()));
    assert!(!engine.is_active());
    assert!(engine.on_snapshot(&snap_mark(9_999.0), 1).is_empty());
    assert!(
        engine
            .on_indicator(
                &IndicatorSample {
                    venue: "binance".into(),
                    symbol: "BTCUSDT".into(),
                    indicator: "rsi".into(),
                    output: 0,
                    value: 5.0,
                },
                2
            )
            .is_empty()
    );
    assert!(
        engine
            .on_signal(&AlertSignal::Feed { venue: "binance".into(), degraded: true }, 3)
            .is_empty()
    );
    assert_eq!(counter.n.load(Ordering::Relaxed), 0, "no rules ⇒ nothing delivered");
}

#[test]
fn engine_dispatches_a_crossing_to_the_in_process_sink() {
    let inbox = InProcessSink::new();
    let mut engine = AlertEngine::new(vec![price_rule("p", Compare::Above, 100.0)])
        .with_sink(Box::new(inbox.clone()));
    assert!(engine.is_active());
    // seed below the level (no prior ⇒ no fire), then cross up.
    assert!(engine.on_snapshot(&snap_mark(99.0), 1).is_empty());
    let fired = engine.on_snapshot(&snap_mark(101.0), 2);
    assert_eq!(fired.len(), 1, "the upward crossing fires exactly one alert");
    assert_eq!(fired[0].rule_id, "p");
    // and it reached the in-process buffer (default targets: in_process on).
    let drained = inbox.drain();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].rule_id, "p");
}

#[test]
fn disabled_rule_never_fires() {
    let mut rule = price_rule("p", Compare::Above, 100.0);
    rule.enabled = false;
    let inbox = InProcessSink::new();
    let mut engine = AlertEngine::new(vec![rule]).with_sink(Box::new(inbox.clone()));
    assert!(!engine.is_active());
    engine.on_snapshot(&snap_mark(99.0), 1);
    assert!(engine.on_snapshot(&snap_mark(101.0), 2).is_empty(), "disabled ⇒ never evaluated");
    assert!(inbox.is_empty());
}

#[test]
fn in_process_off_target_is_not_buffered_but_still_returned() {
    // A rule whose targets exclude in-process still FIRES (and is returned) but is not buffered
    // by the in-process sink — routing is per-alert, decided by the sink.
    let mut rule = price_rule("p", Compare::Above, 100.0);
    rule.targets = AlertTargets { in_process: false, webhooks: vec!["telegram".into()] };
    let inbox = InProcessSink::new();
    let mut engine = AlertEngine::new(vec![rule]).with_sink(Box::new(inbox.clone()));
    engine.on_snapshot(&snap_mark(99.0), 1);
    let fired = engine.on_snapshot(&snap_mark(101.0), 2);
    assert_eq!(fired.len(), 1, "it still fires (a webhook target would deliver it)");
    assert!(inbox.is_empty(), "but the in-process sink skips a non-in-process alert");
}

#[test]
fn set_rules_resets_latch_state() {
    // After a crossing fires, resetting the rules must re-arm: the same seed+cross fires again.
    let mut engine = AlertEngine::new(vec![price_rule("p", Compare::Above, 100.0)]);
    engine.on_snapshot(&snap_mark(99.0), 1);
    assert_eq!(engine.on_snapshot(&snap_mark(101.0), 2).len(), 1);
    // without a reset, re-seeding + crossing would need a full down-then-up; a reset clears
    // last_value so the next pair fires cleanly.
    engine.set_rules(vec![price_rule("p", Compare::Above, 100.0)]);
    engine.on_snapshot(&snap_mark(99.0), 3);
    assert_eq!(engine.on_snapshot(&snap_mark(101.0), 4).len(), 1, "reset re-armed the rule");
}
