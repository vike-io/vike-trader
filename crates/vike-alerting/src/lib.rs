//! `vike-alerting` — the alerting primitive: a persisted rule set, a pure evaluator, and a delivery
//! seam, run as a strict OFF-FOLD consumer of the core's published state (exactly like
//! `vike_journal::materialize` is an off-fold consumer of the WAL). It NEVER touches the vike-core hot
//! fold (the `p99 < 10µs` gate): `vike-tradehub` evaluates its rules once per summary tick, on the
//! thread that already loads the lossy `arc-swap` `CoreSnapshot`, through a [`SnapshotFacts`] view
//! of it (`crates/vike-tradehub/src/alerts.rs`'s `SnapshotView`); nothing here back-pressures or
//! blocks the core. The crate's other consumers run no core at all: `vike-recorder` pages on its
//! own series-liveness signals (`crates/vike-recorder/src/alerts.rs`'s `RecorderAlerts`), and
//! `vike-datahub`, which mounts that recorder under its `record` feature, names this crate to build
//! the recorder's webhook targets. No GUI mounts an engine: the desktop holds no core.
//!
//! **Why it is its own crate.** A watchdog that pages (Telegram) when a recorder stops writing has
//! no business linking the trading core, so every build of this crate has **zero vike-\*
//! dependencies** — that is the property the crate exists to hold — and the crate declares no
//! feature at all (`docs/decisions/0085-the-rank-follows-the-declaration-not-the-role.md`). An
//! input that would name a vike type is asked for through a type this crate owns instead —
//! [`SnapshotFacts`], [`AlertEvent`] — and the CALLER converts, as `SnapshotView` above does.
//! ⚠ **The gate is the manifest, not the feature list:**
//! `crates/vike-ops/tests/architecture/layer_gate/vike_free.rs`'s `VIKE_FREE_CRATES`. The dependency
//! argument is this crate's `Cargo.toml` `[dependencies]` note.
//!
//! [`AlertEngine`] — the FOLD: the per-rule state, the cooldown gate, the sink dispatch — is in
//! that one build, so the vike-free consumer the crate exists for need not re-implement everything
//! that decides WHEN to touch the sinks: `vike-recorder` mounts a real `AlertEngine` over
//! [`RuleTrigger::SeriesStale`] and [`RuleTrigger::SeriesSlow`] (see `vike_recorder::alerts`).
//!
//! It opens no credential store: [`delivery::webhook_configs_from_env`] takes its caller's map. A
//! LIBRARY reading global configuration its caller could neither see nor substitute is exactly what
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets.
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
//!   one type the evaluator and the sinks share, which any consumer can build and deliver.
//!
//! **OFF / byte-identical when unconfigured:** an empty [`AlertRuleSet`] (no `alerts.json`, or one
//! with no rules) means `AlertEngine` evaluates nothing and delivers nothing — every `on_*` method
//! short-circuits to an empty result and touches no sink. A build that never constructs an engine is
//! wholly unaffected: this is additive, opt-in state. Rules live in an `alerts.json` file that
//! [`persist`] loads from a path its caller resolves; no GUI edits them.

#![warn(unreachable_pub)]

pub mod delivery;
pub mod eval;
pub mod persist;
pub mod rule;

mod engine;
#[cfg(test)]
mod testkit;

pub use delivery::{
    AlertSink, DEFAULT_QUEUE_CAPACITY, FiredAlert, InProcessSink, QueuedSink, QueuedSinkOutcome,
    QueuedSinkStop, STOP_POLL, UreqTransport, WEBHOOK_KEYS, WebhookConfig, WebhookKind,
    WebhookSink, WebhookTransport, queued_ureq_webhook_sinks, ureq_webhook_sinks,
    webhook_configs_from_env,
};
pub use engine::AlertEngine;
pub use eval::{
    AlertEvent, AlertSignal, IndicatorSample, ReconAlertFact, RuleState, SnapshotFacts,
};
pub use rule::{AlertRule, AlertRuleSet, AlertTargets, Compare, FeedState, RuleTrigger};
