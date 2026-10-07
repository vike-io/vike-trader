//! [`AlertEngine`]: folds every enabled rule over one input and dispatches what fired.

use indexmap::IndexMap;

use crate::{
    AlertEvent, AlertRule, AlertRuleSet, AlertSignal, AlertSink, FiredAlert, IndicatorSample,
    RuleState, SnapshotFacts, eval,
};

#[cfg(doc)]
use crate::{QueuedSink, WebhookSink};

/// The off-fold alerting consumer: it owns the rule set, the per-rule evaluation state, and the
/// delivery sinks. Feed it the inputs the caller already has — snapshot facts, one event,
/// computed indicator samples, mapped status signals — via the `on_*` methods; each returns the
/// alerts that fired (already dispatched to every sink).
///
/// State (edge/latch/cooldown) lives in a per-rule [`RuleState`] keyed by `AlertRule::id`; an
/// [`IndexMap`] so iteration/fire order is deterministic (repo convention). Rebuilt on
/// [`set_rules`](Self::set_rules), so re-arming every latch on a config change is explicit.
///
/// ⚠ **The whole engine is vike-free.** [`on_snapshot`] and [`on_event`] take [`SnapshotFacts`]
/// and [`AlertEvent`], which this crate owns
/// (`docs/decisions/0085-the-rank-follows-the-declaration-not-the-role.md`).
///
/// [`on_snapshot`]: Self::on_snapshot
/// [`on_event`]: Self::on_event
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

    /// Evaluate every enabled snapshot-driven rule against `facts` (price crossings, drawdown,
    /// recon alerts), dispatch what fired, and return it. `now_ms` is the caller's wall clock.
    pub fn on_snapshot(&mut self, facts: &dyn SnapshotFacts, now_ms: i64) -> Vec<FiredAlert> {
        self.fold(|rule, st| eval::eval_snapshot_rule(rule, st, facts, now_ms))
    }

    /// Evaluate every enabled indicator-threshold rule against one computed [`IndicatorSample`].
    pub fn on_indicator(&mut self, sample: &IndicatorSample, now_ms: i64) -> Vec<FiredAlert> {
        self.fold(|rule, st| eval::eval_indicator_rule(rule, st, sample, now_ms))
    }

    /// Evaluate every enabled event-driven rule against one [`AlertEvent`] (fills, rejects).
    ///
    /// ⚠ No production caller today — the daemon has a real source for `on_snapshot` alone. Kept
    /// because the rule vocabulary already carries the triggers.
    pub fn on_event(&mut self, ev: &AlertEvent<'_>, now_ms: i64) -> Vec<FiredAlert> {
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
    ///
    /// ⚠ This is a nested SERIAL loop on the CALLER's thread, and it is deliberately kept that way:
    /// the engine owns no thread, no queue and no timeout, so what this costs is exactly what the
    /// sinks cost, summed. That is nothing for a log sink and up to a transport timeout PER ALERT
    /// PER TARGET for a raw [`WebhookSink`] — which is how a venue outage once parked a daemon's
    /// tick loop for half an hour. The bound belongs on the SINK, where the blocking happens:
    /// [`QueuedSink`] makes `deliver` an enqueue, and a caller on a loop registers wire-touching
    /// sinks through it (`queued_ureq_webhook_sinks`). Bounding it HERE instead — a per-dispatch
    /// cap, a deadline — would still leave one alert x one dead endpoint on this thread.
    fn dispatch(&self, fired: &[FiredAlert]) {
        for f in fired {
            for sink in &self.sinks {
                sink.deliver(f);
            }
        }
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod engine_tests;
