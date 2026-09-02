//! The headless alerting mount's contract: DEFAULT-OFF and byte-identical when unconfigured, and a
//! real fire reaching a real sink when it is.
//!
//! This drives the SAME public `vike_tradehub::alerts` seam the daemon's `main.rs` uses — `load_rules`
//! on a caller-resolved path, then `maybe_mount`/`maybe_mount_with_sinks`, then `on_snapshot` once
//! per summary tick — so what passes here is what the daemon does. Nothing here touches the network
//! (the webhook transport is never constructed; delivery is asserted through the in-process sink and
//! a counting double) and nothing reads process env, so it stays on the FAST CI lane.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use vike_core::CoreSnapshot;
use vike_ops::alerting::{
    AlertRule, AlertRuleSet, AlertSink, Compare, FiredAlert, InProcessSink, RuleTrigger,
};
use vike_tradehub::alerts::{load_rules, maybe_mount, maybe_mount_with_sinks};

/// A sink that only counts deliveries — the "was ANY sink touched?" witness for the OFF path.
#[derive(Clone, Default)]
struct CountingSink {
    n: Arc<AtomicUsize>,
}

impl AlertSink for CountingSink {
    fn deliver(&self, _alert: &FiredAlert) {
        self.n.fetch_add(1, Ordering::Relaxed);
    }
}

/// A snapshot carrying one published mark — the daemon's summary tick reads exactly this shape.
fn snap_mark(px: f64) -> CoreSnapshot {
    let mut s = CoreSnapshot::empty("hyperliquid", "BTC");
    s.marks = vec![("hyperliquid".to_string(), "BTC".to_string(), px)];
    s
}

/// A snapshot carrying an equity level (the drawdown latch's input).
/// A snapshot whose DRAWDOWN CURVE (`Portfolio::drawdown_curve` — the quantity
/// `RuleTrigger::Drawdown` reads) is `curve`: a fixed 1000 capital base plus one venue block
/// carrying the daemon's own realized PnL. `equity_total` is deliberately left at its default and
/// is NOT what feeds the rule — see `vike_alerting::eval`'s `Drawdown` arm for why the cross-venue
/// equity total (which on a live mount includes the venue's whole-account wallet) stopped being the
/// source.
fn snap_equity(curve: f64) -> CoreSnapshot {
    let mut s = CoreSnapshot::empty("hyperliquid", "BTC");
    s.portfolio.capital_base = 1_000.0;
    s.portfolio.venues = vec![vike_core::snapshot::VenueBlock {
        venue: "hyperliquid".into(),
        realized_pnl: curve - 1_000.0,
        ..Default::default()
    }];
    s
}

fn price_rule(id: &str, level: f64) -> AlertRule {
    AlertRule::new(
        id,
        RuleTrigger::Price {
            venue: "hyperliquid".into(),
            symbol: "BTC".into(),
            op: Compare::Above,
            level,
        },
    )
}

// ---- the DEFAULT-OFF gate ---------------------------------------------------------------------

/// No rules file ⇒ no rules ⇒ no mount, and NO sink is ever constructed or touched. This is the
/// byte-identical guarantee: the daemon's summary thread skips its `if let Some(..)` entirely.
#[test]
fn an_absent_rules_file_mounts_nothing_and_touches_no_sink() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("alerts.json");
    assert!(load_rules(&missing).is_none(), "a missing file is the OFF state, never an error");

    let counter = CountingSink::default();
    assert!(
        maybe_mount_with_sinks(load_rules(&missing), vec![Box::new(counter.clone())]).is_none(),
        "no rules ⇒ no engine mounted"
    );
    assert_eq!(counter.n.load(Ordering::Relaxed), 0, "nothing delivered on the OFF path");
}

/// A file that EXISTS but declares zero rules is equally OFF — and so is one whose every rule is
/// disabled. Both are shapes an operator lands on while editing, and neither may spawn an engine.
#[test]
fn an_empty_or_all_disabled_rules_file_mounts_nothing() {
    let dir = tempfile::tempdir().unwrap();

    let empty = dir.path().join("empty.json");
    std::fs::write(&empty, serde_json::to_string(&AlertRuleSet::default()).unwrap()).unwrap();
    let loaded = load_rules(&empty).expect("an empty rule set still parses");
    assert!(loaded.is_empty());
    assert!(maybe_mount(Some(loaded), Vec::new).is_none(), "zero rules ⇒ no engine");

    let off = dir.path().join("off.json");
    let mut rule = price_rule("p", 100.0);
    rule.enabled = false;
    let set = AlertRuleSet { rules: vec![rule], ..Default::default() };
    std::fs::write(&off, serde_json::to_string(&set).unwrap()).unwrap();
    let loaded = load_rules(&off).expect("a disabled rule still parses");
    assert_eq!(loaded.rules.len(), 1);
    assert!(maybe_mount(Some(loaded), Vec::new).is_none(), "no ENABLED rule ⇒ no engine");
}

/// A corrupt file must never brick the daemon: it loads as `None` (the OFF state), exactly like an
/// absent one. An operator's half-saved JSON cannot stop a trading node from starting.
#[test]
fn a_corrupt_rules_file_is_the_off_state_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("alerts.json");
    std::fs::write(&bad, "{ not valid json ]").unwrap();
    assert!(load_rules(&bad).is_none());
    assert!(maybe_mount(load_rules(&bad), Vec::new).is_none());
}

// ---- the ON path ------------------------------------------------------------------------------

/// The end-to-end headless path: a rules file on disk → `load_rules` → `maybe_mount` → the summary
/// tick's `on_snapshot` → a fired alert in a stubbed sink. No network, no GUI.
#[test]
fn a_rules_file_on_disk_fires_a_crossing_into_the_stubbed_sink() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alerts.json");
    let set = AlertRuleSet { rules: vec![price_rule("btc-100k", 100_000.0)], ..Default::default() };
    std::fs::write(&path, serde_json::to_string_pretty(&set).unwrap()).unwrap();

    let inbox = InProcessSink::new();
    let counter = CountingSink::default();
    let mut mount = maybe_mount_with_sinks(
        load_rules(&path),
        vec![Box::new(inbox.clone()), Box::new(counter.clone())],
    )
    .expect("one enabled rule mounts");
    assert_eq!(mount.rule_count(), 1);
    assert_eq!(mount.enabled_count(), 1);

    // Tick 1 seeds the prior sample (a rule can never fire on its first observation).
    assert!(mount.on_snapshot(&snap_mark(99_000.0), 1_000).is_empty());
    // Tick 2 crosses up → exactly one fire, delivered to BOTH sinks.
    let fired = mount.on_snapshot(&snap_mark(101_000.0), 2_000);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].rule_id, "btc-100k");
    let drained = inbox.drain();
    assert_eq!(drained.len(), 1, "the in-process sink received the alert");
    assert!(drained[0].body.contains("101000"), "the body carries the mark: {}", drained[0].body);
    assert_eq!(counter.n.load(Ordering::Relaxed), 1, "every registered sink saw it once");

    // Tick 3 stays above → edge-triggered, so no re-fire (the daemon ticks continuously).
    assert!(mount.on_snapshot(&snap_mark(102_000.0), 3_000).is_empty());
    assert_eq!(counter.n.load(Ordering::Relaxed), 1);
}

/// The second snapshot-driven trigger an unattended node actually wants: the equity-drawdown latch,
/// fed from the same tick. Proves `on_snapshot` covers more than price crossings.
#[test]
fn the_drawdown_latch_fires_from_the_same_snapshot_tick() {
    let inbox = InProcessSink::new();
    let set = AlertRuleSet {
        rules: vec![AlertRule::new("dd", RuleTrigger::Drawdown { pct: 0.10 })],
        ..Default::default()
    };
    let mut mount = maybe_mount_with_sinks(Some(set), vec![Box::new(inbox.clone())])
        .expect("an enabled drawdown rule mounts");

    assert!(mount.on_snapshot(&snap_equity(1_000.0), 1).is_empty(), "the peak is just seeded");
    assert_eq!(mount.on_snapshot(&snap_equity(880.0), 2).len(), 1, "12% off peak breaches 10%");
    assert!(mount.on_snapshot(&snap_equity(870.0), 3).is_empty(), "latched — no alert storm");
    assert_eq!(inbox.drain().len(), 1);
}

/// HONESTY GATE: a rule whose trigger has NO source in the daemon (a fill alert needs a typed
/// `Event` stream the headless daemon does not expose off-fold) still LOADS and still MOUNTS — it
/// simply never fires from the snapshot tick. This pins the documented "loads but is unfed"
/// behavior so a future PR that wires `on_event` has a test to flip rather than a silent surprise.
#[test]
fn an_unfed_trigger_kind_mounts_but_never_fires_from_snapshots() {
    let counter = CountingSink::default();
    let set = AlertRuleSet {
        rules: vec![
            AlertRule::new("any-fill", RuleTrigger::Fill { venue: None, symbol: None }),
            AlertRule::new("rejects", RuleTrigger::OrderRejected),
        ],
        ..Default::default()
    };
    let mut mount = maybe_mount_with_sinks(Some(set), vec![Box::new(counter.clone())])
        .expect("enabled rules mount even when their input is unfed here");
    assert_eq!(mount.enabled_count(), 2);
    for tick in 1..=5 {
        assert!(
            mount.on_snapshot(&snap_mark(100.0 * tick as f64), tick).is_empty(),
            "event-driven rules cannot fire from a snapshot"
        );
    }
    assert_eq!(counter.n.load(Ordering::Relaxed), 0, "no source ⇒ no delivery, never a fake one");
}
