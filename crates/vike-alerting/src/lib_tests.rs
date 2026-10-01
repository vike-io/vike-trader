use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// One published mark, as [`SnapshotFacts`] asks for it. Owned, so a helper can return it —
/// see that trait's doc for why the input is a trait rather than a borrowing struct.
struct Mark(f64);
impl SnapshotFacts for Mark {
    fn mark(&self, venue: &str, symbol: &str) -> Option<f64> {
        (venue == "binance" && symbol == "BTCUSDT").then_some(self.0)
    }
    fn drawdown_curve(&self) -> f64 {
        0.0
    }
    fn capital_base(&self) -> f64 {
        0.0
    }
    fn pnl_total(&self) -> f64 {
        0.0
    }
    fn recon_alerts(&self) -> Vec<ReconAlertFact<'_>> {
        Vec::new()
    }
}

fn snap_mark(px: f64) -> Mark {
    Mark(px)
}

fn price_rule(id: &str) -> AlertRule {
    AlertRule::new(
        id,
        RuleTrigger::Price {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            op: Compare::Above,
            level: 100.0,
        },
    )
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
    let mut engine = AlertEngine::new(vec![price_rule("p")]).with_sink(Box::new(inbox.clone()));
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
    let mut rule = price_rule("p");
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
    let mut rule = price_rule("p");
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
    let mut engine = AlertEngine::new(vec![price_rule("p")]);
    engine.on_snapshot(&snap_mark(99.0), 1);
    assert_eq!(engine.on_snapshot(&snap_mark(101.0), 2).len(), 1);
    // without a reset, re-seeding + crossing would need a full down-then-up; a reset clears
    // last_value so the next pair fires cleanly.
    engine.set_rules(vec![price_rule("p")]);
    engine.on_snapshot(&snap_mark(99.0), 3);
    assert_eq!(engine.on_snapshot(&snap_mark(101.0), 4).len(), 1, "reset re-armed the rule");
}
