//! `vike-alerting` — the alerting primitive: a persisted rule set, a pure evaluator, and a delivery
//! seam, run as a strict OFF-FOLD consumer of the core's published state (exactly like
//! `vike_journal::materialize` is an off-fold consumer of the WAL). It NEVER touches the vike-core hot
//! fold (the `p99 < 10µs` gate): `vike-tradehub` evaluates its rules once per summary tick, on the
//! thread that already loads the lossy `arc-swap` `CoreSnapshot`, through a [`SnapshotFacts`] view
//! of it (`crates/vike-tradehub/src/alerts.rs`'s `SnapshotView`); nothing here back-pressures or
//! blocks the core. The crate's other consumers run no core at all: `vike-recorder` pages on its
//! own series-liveness signals (`crates/vike-recorder/src/alerts.rs`'s `RecorderAlerts`), and
//! `vike-datahub`, which mounts that recorder under its `record` feature, names this crate to build
//! the recorder's webhook targets.
//! ⚠ This said `vike-app` read the snapshot on repaint and fed `AlertEngine` on the GUI thread
//! until 2026-09-28. No GUI mounts an engine: the desktop holds no core, and the `fat` build that
//! held one was deleted on 2026-09-09.
//!
//! **Why it is its own crate — and what having one configuration guarantees.** the latency box crashed and
//! ~6h of recorded Polymarket L2 tape was lost UNNOTICED. The wanted fix is a watchdog that pages
//! (Telegram) when a recorder stops writing — with no business linking the trading core. While this
//! tree lived inside `vike-ops` it would have had to link `vike-ops` -> `vike-core` -> `vike-exec`
//! -> `vike-model` just to POST a message. So the crate was split out on the seam that was already
//! there, and nothing in it needs a vike crate now:
//!
//! | module | feature | dependency reason |
//! |---|---|---|
//! | [`rule`], [`persist`], [`delivery`], [`eval`], [`AlertEngine`] | none — one configuration | serde / serde_json / ureq / tracing / indexmap — **no vike-\* crate at all** |
//!
//! Every build of this crate therefore has **zero vike-\* dependencies** — that is the property the
//! crate exists to hold. An input that would name a vike type is asked for through a type this
//! crate owns instead — [`SnapshotFacts`], [`AlertEvent`] — and the CALLER converts, as
//! `SnapshotView` above does. ⚠ This passage said a `core` feature carried the vike-typed half and
//! that `vike-ops` enabled it; MEASURED 2026-09-25, this crate declares `default = []` and NO other
//! feature, so there is no `core` and the zero-vike property holds for EVERY build rather than for
//! a default one. ⚠ Its table kept a `core` row (`eval_snapshot_rule`, `eval_event_rule`,
//! `on_snapshot` and `on_event`, over `vike_core::CoreSnapshot` and `vike_model::events::Event`),
//! and this paragraph sent a new vike-typed module "behind a named feature", until 2026-09-28; all
//! four take this crate's own types now. ⚠ It also said `vike-ops` re-exports this crate as
//! `vike_ops::alerting`. That re-export and its `vike_app_core` twin are DELETED: consumers name
//! this crate directly.
//!
//! ⚠ **The gate is the manifest, not the feature list.** `crates/vike-ops/tests/layer_gate.rs`'s
//! `VIKE_FREE_CRATES` fails if this crate's `Cargo.toml` declares ANY `vike-*` normal dependency —
//! optional and target-specific ones included — which a compile-only check would never notice and
//! no rank can say (tier 15 still admits the vocabulary floor). It was a CI lane that built this
//! crate alone and ran `cargo tree` over it until 2026-09-28; with no feature left there was
//! nothing for it to build that the roster lane does not, and the manifest answers the tree's
//! question because only a workspace member can depend on a workspace member. `indexmap` is a
//! plain dependency for exactly that reason: it is not a vike crate, so the engine holding one
//! costs the property nothing.
//!
//! **What moved OUT of `core` first, and why it was the whole point.** [`AlertEngine`] used to be
//! gated too, purely because it holds an `IndexMap`. That put the FOLD — the per-rule state, the
//! cooldown gate, the sink dispatch — behind a feature that dragged `vike-core`, so the vike-free
//! consumer the crate exists for could reach the sinks but had to re-implement everything that
//! decides WHEN to touch them. The engine left the feature while two of its four inputs still named
//! a vike type; those two stopped naming one when `core` itself was deleted on 2026-09-23, and
//! `vike-recorder` mounts a real `AlertEngine` over [`RuleTrigger::SeriesStale`] and
//! [`RuleTrigger::SeriesSlow`] (see `vike_recorder::alerts`). ⚠ This said the two inputs "are
//! gated" and the recorder mounts the engine "with default features on" until 2026-09-28.
//!
//! There were two features, and neither survives. The other, `workspace-env`, carried an optional
//! `vike-bridge-core` edge for one convenience function that opened the workspace credential store
//! itself ([`delivery::webhook_configs_from_env`]'s deleted twin) — a LIBRARY reading global
//! configuration its caller could neither see nor substitute, which is exactly what
//! `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets. Its one caller was
//! a binary that already owned a credential map, so the read moved there and both the feature and
//! the dependency went away. ⚠ This opened "`core` is now the ONLY feature" until 2026-09-28,
//! five days after `core` went too; the crate declares no feature at all.
//!
//! Three parts:
//! - [`rule`] — the persisted model ([`AlertRuleSet`]/[`AlertRule`]/[`RuleTrigger`]), saved/loaded by
//!   [`persist`] with the workspace-v2 forward-compat discipline.
//! - [`eval`] — the pure evaluator:
//!   `(rule + per-rule state + one input) -> Option<FiredAlert>`.
//! - [`delivery`] — the sinks: an in-process buffer (the future toast/OS-notification seam) and an
//!   optional Telegram / generic webhook POST over the existing `ureq` + rustls stack, plus
//!   [`QueuedSink`], the bounded-queue-and-one-thread decorator that keeps a POSTing sink off its
//!   caller's thread (the engine's private `AlertEngine::dispatch` is serial and inline, and that
//!   decorator is where its only bound lives). It also owns [`FiredAlert`], the sink payload — the
//!   one type the evaluator and the sinks share, which any consumer can build and deliver. (It was
//!   argued here as the one type "both halves" share, kept in "the default half", while `core`
//!   split the crate in two.)
//!
//! **OFF / byte-identical when unconfigured:** an empty [`AlertRuleSet`] (no `alerts.json`, or one
//! with no rules) means `AlertEngine` evaluates nothing and delivers nothing — every `on_*` method
//! short-circuits to an empty result and touches no sink. A build that never constructs an engine is
//! wholly unaffected: this is additive, opt-in state. Rules live in an `alerts.json` file that
//! [`persist`] loads from a path its caller resolves; no GUI edits them. ⚠ This said the window
//! that edits rules "lives in `vike-app` (compile-checked in CI, never tested) and is an explicit
//! follow-up" until 2026-09-28; the tree holds no such window, and the desktop mounts no engine
//! for one to edit.

pub mod delivery;
pub mod eval;
pub mod persist;
pub mod rule;

pub use delivery::{
    AlertSink, DEFAULT_QUEUE_CAPACITY, FiredAlert, InProcessSink, QueuedSink, QueuedSinkOutcome,
    QueuedSinkStop, STOP_POLL, UreqTransport, WEBHOOK_KEYS, WebhookConfig, WebhookKind,
    WebhookSink, WebhookTransport, queued_ureq_webhook_sinks, ureq_webhook_sinks,
    webhook_configs_from_env,
};
pub use eval::{
    AlertEvent, AlertSignal, IndicatorSample, ReconAlertFact, RuleState, SnapshotFacts,
};
pub use rule::{AlertRule, AlertRuleSet, AlertTargets, Compare, FeedState, RuleTrigger};

use indexmap::IndexMap;

/// The off-fold alerting consumer: it owns the rule set, the per-rule evaluation state, and the
/// delivery sinks. Feed it the inputs the caller already has — snapshot facts, one event,
/// computed indicator samples, mapped status signals — via the `on_*` methods; each returns the
/// alerts that fired (already dispatched to every sink).
///
/// State (edge/latch/cooldown) lives in a per-rule [`RuleState`] keyed by `AlertRule::id`; an
/// [`IndexMap`] so iteration/fire order is deterministic (repo convention). Rebuilt on
/// [`set_rules`](Self::set_rules), so re-arming every latch on a config change is explicit.
///
/// ⚠ **The whole engine is vike-free, and there is no longer a feature saying otherwise.**
/// [`on_snapshot`] and [`on_event`] used to name `vike_core::CoreSnapshot` and
/// `vike_model::events::Event` behind a `core` feature; they now take [`SnapshotFacts`] and
/// [`AlertEvent`], which this crate owns. That feature was this crate's ONLY `vike-*` edge, and
/// carrying it cost the crate a rank far above anything it linked — the half-done state of the
/// split that created this crate *so a watchdog could page without the core*
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
    /// ⚠ No production caller today — the daemon has a real source for `on_snapshot` alone, which
    /// this crate's own mount doc has said since it was written. Kept because the rule vocabulary
    /// already carries the triggers.
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

/// The watchdog path end to end in a build with no vike crate in it — the crate's own reason for
/// existing. The roster lane runs it: with no feature left, there is only the one configuration to
/// build. ⚠ This called a standalone lane "the only build that sees the crate's own reason for
/// existing" until 2026-09-28. That stopped being true when `core` went on 2026-09-23, and the
/// lane — which from then on re-ran this same build beside the roster's — was deleted on
/// 2026-09-28.
#[cfg(test)]
mod default_build_tests;

// The rest of the engine's tests, over the evaluator's own input types (`SnapshotFacts`,
// `AlertEvent`) — no vike type here either. ⚠ This said they named a vike type behind the `core`
// feature, and that the default CI lane compiled this crate WITH `core` because `vike-ops` enabled
// it, until 2026-09-28: the feature went on 2026-09-23, and `vike-ops`' edge on this crate on
// 2026-09-25.
#[cfg(test)]
mod lib_tests;
