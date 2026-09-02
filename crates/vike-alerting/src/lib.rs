//! `vike-alerting` — the alerting primitive: a persisted rule set, a pure evaluator, and a delivery
//! seam, run as a strict OFF-FOLD consumer of the core's published state (exactly like
//! `vike_ops::journal_mat` is an off-fold consumer of the WAL). It NEVER touches the vike-core hot
//! fold (the `p99 < 10µs` gate): `vike-app` reads the lossy `arc-swap` `CoreSnapshot` on repaint and
//! hands it (plus any typed `Event`s / status signals it already receives) to `AlertEngine` on the
//! GUI thread; nothing here back-pressures or blocks the core.
//!
//! **Why it is its own crate — and what the feature split guarantees.** the latency box crashed and ~6h of
//! recorded Polymarket L2 tape was lost UNNOTICED. The wanted fix is a watchdog that pages
//! (Telegram) when a recorder stops writing — with no business linking the trading core. While this
//! tree lived inside `vike-ops` it would have had to link `vike-ops` -> `vike-core` -> `vike-exec`
//! -> `vike-model` just to POST a message. So the crate is split on the seam that was already
//! there:
//!
//! | module | feature | dependency reason |
//! |---|---|---|
//! | [`rule`], [`persist`], [`delivery`], most of [`eval`], [`AlertEngine`] | DEFAULT | serde / serde_json / ureq / tracing / indexmap — **no vike-\* crate at all** |
//! | [`eval::eval_snapshot_rule`], [`eval::eval_event_rule`], [`AlertEngine::on_snapshot`], [`AlertEngine::on_event`] | `core` | `vike_core::CoreSnapshot`, `vike_model::events::Event` |
//!
//! A DEFAULT build of this crate therefore has **zero vike-\* dependencies** — that is the property
//! the split exists to hold, and a new module needing a vike type goes behind `core` (or a new named
//! feature), never into the default half. `vike-ops` depends on this crate with `core` on and
//! re-exports it as `vike_ops::alerting`, so every historical `vike_ops::alerting::…` /
//! `vike_app_core::alerting::…` path resolves verbatim.
//!
//! ⚠ **The gate is `cargo tree`, not the feature list.** `scripts/ci_feature_suite.sh`'s
//! `alerting-standalone` lane builds the default configuration ALONE and fails if
//! `cargo tree -p vike-alerting -e normal` prints any `vike-` line — a compile-only check would
//! still pass with a non-optional vike dep added. `indexmap` is a plain dependency for exactly that
//! reason: it is not a vike crate, so the engine holding one costs the property nothing.
//!
//! **What moved OUT of `core`, and why it was the whole point.** [`AlertEngine`] used to be gated
//! too, purely because it holds an `IndexMap`. That put the FOLD — the per-rule state, the
//! cooldown gate, the sink dispatch — behind a feature that drags `vike-core`, so the vike-free
//! consumer the crate exists for could reach the sinks but had to re-implement everything that
//! decides WHEN to touch them. Only two of the engine's four inputs name a vike type; those two are
//! gated, the engine is not, and `vike-recorder` now mounts a real `AlertEngine` over
//! [`RuleTrigger::SeriesStale`] with default features on (see `vike_recorder::alerts`).
//!
//! `core` is now the ONLY feature. There was a second, `workspace-env`, carrying an optional
//! `vike-bridge-core` edge for one convenience function that opened the workspace credential store
//! itself ([`delivery::webhook_configs_from_env`]'s deleted twin) — a LIBRARY reading global
//! configuration its caller could neither see nor substitute, which is exactly what
//! `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets. Its one caller was
//! a binary that already owned a credential map, so the read moved there and both the feature and
//! the dependency went away.
//!
//! Three parts:
//! - [`rule`] — the persisted model ([`AlertRuleSet`]/[`AlertRule`]/[`RuleTrigger`]), saved/loaded by
//!   [`persist`] with the workspace-v2 forward-compat discipline.
//! - [`eval`] — the pure evaluator:
//!   `(rule + per-rule state + one input) -> Option<FiredAlert>`.
//! - [`delivery`] — the sinks: an in-process buffer (the future toast/OS-notification seam) and an
//!   optional Telegram / generic webhook POST over the existing `ureq` + rustls stack. It also owns
//!   [`FiredAlert`], the sink payload: that is the ONE type both halves share, so it lives in the
//!   default half where a vike-free consumer can build and deliver one.
//!
//! **OFF / byte-identical when unconfigured:** an empty [`AlertRuleSet`] (no `alerts.json`, or one
//! with no rules) means `AlertEngine` evaluates nothing and delivers nothing — every `on_*` method
//! short-circuits to an empty result and touches no sink. A build that never constructs an engine is
//! wholly unaffected: this is additive, opt-in state. The GUI window that edits rules lives in
//! `vike-app` (compile-checked in CI, never tested) and is an explicit follow-up — this crate is
//! the CI-tested logic under it.

pub mod delivery;
pub mod eval;
pub mod persist;
pub mod rule;

pub use delivery::{
    ureq_webhook_sinks, webhook_configs_from_env, AlertSink, FiredAlert, InProcessSink,
    UreqTransport, WebhookConfig, WebhookKind, WebhookSink, WebhookTransport,
};
pub use eval::{AlertSignal, IndicatorSample, RuleState};
pub use rule::{AlertRule, AlertRuleSet, AlertTargets, Compare, FeedState, RuleTrigger};

use indexmap::IndexMap;
#[cfg(feature = "core")]
use vike_core::CoreSnapshot;
#[cfg(feature = "core")]
use vike_model::events::Event;

/// The off-fold alerting consumer: it owns the rule set, the per-rule evaluation state, and the
/// delivery sinks. Feed it the inputs the GUI already has each frame — the latest `CoreSnapshot`,
/// any typed `Event`s, computed indicator samples, and mapped status signals — via the `on_*`
/// methods; each returns the alerts that fired (already dispatched to every sink).
///
/// State (edge/latch/cooldown) lives in a per-rule [`RuleState`] keyed by `AlertRule::id`; an
/// [`IndexMap`] so iteration/fire order is deterministic (repo convention). Rebuilt on
/// [`set_rules`](Self::set_rules), so re-arming every latch on a config change is explicit.
///
/// Available in the DEFAULT vike-free build: only [`on_snapshot`](Self::on_snapshot) and
/// [`on_event`](Self::on_event) need feature `core`, because only those two inputs are vike types.
pub struct AlertEngine {
    rules: Vec<AlertRule>,
    state: IndexMap<String, RuleState>,
    sinks: Vec<Box<dyn AlertSink>>,
}

impl AlertEngine {
    /// A new engine over `rules`, with no sinks yet (add them with [`add_sink`](Self::add_sink)).
    pub fn new(rules: Vec<AlertRule>) -> Self {
        AlertEngine { rules, state: IndexMap::new(), sinks: Vec::new() }
    }

    /// A new engine over a whole [`AlertRuleSet`] (the persisted shape).
    pub fn from_rule_set(set: AlertRuleSet) -> Self {
        Self::new(set.rules)
    }

    /// Register a delivery sink (in-process buffer, webhook, …). Order is delivery order.
    pub fn add_sink(&mut self, sink: Box<dyn AlertSink>) {
        self.sinks.push(sink);
    }

    /// Builder form of [`add_sink`](Self::add_sink).
    pub fn with_sink(mut self, sink: Box<dyn AlertSink>) -> Self {
        self.add_sink(sink);
        self
    }

    /// Replace the rule set and RESET all evaluation state (every latch re-arms, every cooldown
    /// clears) — a rule edit is a config change, not a continuation.
    pub fn set_rules(&mut self, rules: Vec<AlertRule>) {
        self.rules = rules;
        self.state.clear();
    }

    /// The current rules (read-only).
    pub fn rules(&self) -> &[AlertRule] {
        &self.rules
    }

    /// Whether any rule is enabled — the cheap "is the engine doing anything?" check. `false` ⇒
    /// every `on_*` is a guaranteed no-op (the OFF state).
    pub fn is_active(&self) -> bool {
        self.rules.iter().any(|r| r.enabled)
    }

    /// Evaluate every enabled snapshot-driven rule against `snap` (price crossings, drawdown,
    /// recon alerts), dispatch what fired, and return it. `now_ms` is the caller's wall clock.
    ///
    /// Feature `core` — `CoreSnapshot` is a vike type.
    #[cfg(feature = "core")]
    pub fn on_snapshot(&mut self, snap: &CoreSnapshot, now_ms: i64) -> Vec<FiredAlert> {
        self.fold(|rule, st| eval::eval_snapshot_rule(rule, st, snap, now_ms))
    }

    /// Evaluate every enabled indicator-threshold rule against one computed [`IndicatorSample`].
    pub fn on_indicator(&mut self, sample: &IndicatorSample, now_ms: i64) -> Vec<FiredAlert> {
        self.fold(|rule, st| eval::eval_indicator_rule(rule, st, sample, now_ms))
    }

    /// Evaluate every enabled event-driven rule against one typed `Event` (fills, rejects).
    ///
    /// Feature `core` — `Event` is a vike type.
    #[cfg(feature = "core")]
    pub fn on_event(&mut self, ev: &Event, now_ms: i64) -> Vec<FiredAlert> {
        self.fold(|rule, st| eval::eval_event_rule(rule, st, ev, now_ms))
    }

    /// Evaluate every enabled status-signal rule against one [`AlertSignal`] (feed health, breaker
    /// trip, Polymarket resolution, a recorded series that stopped receiving rows).
    pub fn on_signal(&mut self, sig: &AlertSignal, now_ms: i64) -> Vec<FiredAlert> {
        self.fold(|rule, st| eval::eval_signal_rule(rule, st, sig, now_ms))
    }

    /// Fold `eval_one` over every ENABLED rule (creating its state lazily), dispatch what fired,
    /// and return it. Fast-paths the OFF state: zero rules ⇒ empty result, no sink touched.
    fn fold<F>(&mut self, mut eval_one: F) -> Vec<FiredAlert>
    where
        F: FnMut(&AlertRule, &mut RuleState) -> Option<FiredAlert>,
    {
        if self.rules.is_empty() {
            return Vec::new();
        }
        let mut fired = Vec::new();
        // Split-borrow rules (shared) + state (mut) — disjoint fields — inside a block so both
        // borrows end before the `&self` dispatch below.
        {
            let AlertEngine { rules, state, .. } = self;
            for rule in rules.iter().filter(|r| r.enabled) {
                let st = state.entry(rule.id.clone()).or_default();
                if let Some(f) = eval_one(rule, st) {
                    fired.push(f);
                }
            }
        }
        self.dispatch(&fired);
        fired
    }

    /// Broadcast each fired alert to every sink (each sink self-filters on the alert's targets).
    /// An empty slice is a no-op — nothing fired, nothing delivered.
    fn dispatch(&self, fired: &[FiredAlert]) {
        for f in fired {
            for sink in &self.sinks {
                sink.deliver(f);
            }
        }
    }
}

/// The engine tests that need NO vike type — they run in the `alerting-standalone` lane, which is
/// the only build that sees the crate's own reason for existing.
#[cfg(test)]
mod default_build_tests {
    use super::*;

    /// **The property the crate was split out for, executed rather than asserted in prose.** A
    /// build with no vike crate in it holds a real `AlertEngine`, folds a real rule, and delivers a
    /// real `FiredAlert` naming the series — the watchdog path, end to end, minus the network.
    #[test]
    fn a_vike_free_build_folds_a_series_stale_rule_all_the_way_to_a_sink() {
        let inbox = InProcessSink::new();
        let rule = AlertRule::new(
            "recorder-series-stale",
            RuleTrigger::SeriesStale { series_prefix: None },
        );
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
        assert!(engine
            .on_signal(
                &AlertSignal::SeriesStale {
                    series: "trade/binance/BTCUSDT".into(),
                    silent_for_ms: None,
                    rows: 0
                },
                1
            )
            .is_empty());
        assert!(inbox.is_empty());
    }
}

// The engine tests that DO name a vike type live behind the same feature those inputs do; the
// `vike-alerting` crate is compiled WITH `core` in the default CI lane (the roster selects
// `-p vike-ops` in the same invocation, and resolver-2 unifies its `core` feature into this
// build), while the `alerting-standalone` suite runs the vike-free default build on its own.
#[cfg(all(test, feature = "core"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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

    fn snap_mark(px: f64) -> CoreSnapshot {
        let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
        s.marks = vec![("binance".to_string(), "BTCUSDT".to_string(), px)];
        s
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
        assert!(engine
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
            .is_empty());
        assert!(engine
            .on_signal(&AlertSignal::Feed { venue: "binance".into(), degraded: true }, 3)
            .is_empty());
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
}
