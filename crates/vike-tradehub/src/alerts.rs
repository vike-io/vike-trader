//! The HEADLESS alerting mount — `vike_ops::alerting` wired into the daemon so rules keep
//! evaluating on the production box with no GUI attached.
//!
//! The engine itself (rule model, pure evaluator, delivery sinks) is CI-tested in `vike-ops`; until
//! this module nothing in the workspace ever CONSTRUCTED an `AlertEngine`, so a rule could only
//! ever fire inside a running GUI — and die when the window closed. This is the half that matters
//! for an unattended node.
//!
//! ## Strictly OFF-FOLD (the `p99 < 10µs` gate is untouched)
//! Same contract as [`vike_ops::journal_mat`]: the mount consumes the daemon's ALREADY-PUBLISHED
//! state and never enters the `vike-core` fold. Concretely it is driven from the daemon's existing
//! periodic snapshot thread — the one that already reads the lossy `arc-swap` snapshot cell and
//! prints the one-line JSON summary — so alerting adds ONE `AlertEngine::on_snapshot` call per
//! summary tick on a thread the core never waits on. Nothing here holds a lock the fold takes,
//! back-pressures an ingest lane, or runs on a venue callback.
//!
//! ## Default OFF, byte-identical
//! The RULES FILE is the gate (the "absent config is the gate" idiom, not a second env flag):
//! [`maybe_mount`] returns `None` when there is no rules file, when the file has zero rules, and
//! when every rule in it is `enabled: false`. `None` ⇒ no engine, no sink constructed, no webhook
//! target resolved, and the summary thread runs exactly as it did before — see
//! `tests/alerts_mount.rs`.
//!
//! ## What is fed, and what is NOT
//! [`AlertEngine`] accepts four inputs; the daemon has a real source for exactly ONE of them today,
//! and this module deliberately fabricates none of the others:
//!
//! - FED — `on_snapshot` (from the summary tick). Usable triggers: `Price` (mark crossings),
//!   `Drawdown` (the equity-peak latch) and `ReconAlert` (held reconcile divergences, i.e. the
//!   `VIKE_RECONCILE=1` path).
//! - UNFED — `on_event` (`Fill` / `OrderRejected`): the daemon has NO typed `Event` stream
//!   off-fold. `CoreSnapshot::recent_events` is pre-rendered `String`s, and the live venue event
//!   lane is consumed inside `vike_run::build_node`'s forwarder, which feeds the core ingest.
//! - UNFED — `on_indicator` (`Indicator`): a headless daemon computes no streaming indicators;
//!   that value is a GUI/chart-side computation the consumer must supply.
//! - UNFED — `on_signal` (`Feed` / `FillRateBreaker` / `PolymarketResolution` / `SeriesStale`): the
//!   daemon exposes no feed-health, breaker, resolution or series-liveness transition off-fold (its
//!   reconcile mount runs with an EMPTY feed-status map for exactly this reason). `SeriesStale` is
//!   the one whose producer exists and is simply somewhere ELSE —
//!   `crates/vike-recorder/src/alerts.rs` raises it from `vike_recorder::liveness`, and this daemon
//!   depends on no recorder crate at all, so its rules load here and can never fire.
//!
//! Rules of an unfed kind still LOAD and are still counted; they simply never fire here. Wiring the
//! remaining three is follow-up work that needs a new off-fold producer, not a change to this file.
//!
//! ## Delivery
//! [`LogSink`] always (the durable record on the vike-log trace file — STDOUT stays protocol-only),
//! plus every webhook target the workspace credential store configures, built by the engine's own
//! PURE `webhook_configs_from_env` over the map this daemon's `main.rs` already resolved (no second
//! config path invented here, and no store read inside the alerting library). No configured target
//! ⇒ log-only, which is still a real headless notification surface.

use std::path::Path;

use vike_core::CoreSnapshot;
use vike_ops::alerting::{
    ureq_webhook_sinks, AlertEngine, AlertRuleSet, AlertSink, FiredAlert, WebhookConfig,
};

/// The always-registered sink: every fired alert becomes one structured `tracing` record. It rides
/// the vike-log file/stderr layer, NEVER stdout (which stays the newline-JSON daemon protocol).
/// `warn` because an alert is by definition something the operator asked to be told about.
pub struct LogSink;

impl AlertSink for LogSink {
    fn deliver(&self, alert: &FiredAlert) {
        tracing::warn!(
            rule = %alert.rule_id,
            title = %alert.title,
            ts_ms = alert.ts_ms,
            "ALERT: {}",
            alert.body
        );
    }
}

/// The mounted engine. Owned by (and moved into) the daemon's periodic snapshot thread, so the
/// per-rule evaluation state lives entirely on that off-fold thread — no lock is shared with the
/// core.
pub struct AlertMount {
    engine: AlertEngine,
}

impl AlertMount {
    /// Evaluate the snapshot-driven rules against one published `CoreSnapshot` and dispatch what
    /// fired. Called once per summary tick; returns what fired (the daemon ignores it — delivery
    /// already happened inside the engine — but tests assert on it).
    pub fn on_snapshot(&mut self, snap: &CoreSnapshot, now_ms: i64) -> Vec<FiredAlert> {
        self.engine.on_snapshot(snap, now_ms)
    }

    /// How many rules were loaded (ENABLED and disabled alike) — for the mount banner.
    pub fn rule_count(&self) -> usize {
        self.engine.rules().len()
    }

    /// How many of those are enabled, i.e. can actually fire.
    pub fn enabled_count(&self) -> usize {
        self.engine.rules().iter().filter(|r| r.enabled).count()
    }
}

/// Load a rule set from a caller-resolved path — a thin, testable wrapper over
/// [`vike_ops::alerting::persist::load_path`] so the daemon binary owns the path resolution (and
/// therefore the env read). Absent or unparseable ⇒ `None`, the OFF state; a bad file never bricks
/// the daemon.
pub fn load_rules(path: &Path) -> Option<AlertRuleSet> {
    vike_ops::alerting::persist::load_path(path)
}

/// Mount the engine IFF `rules` carries at least one ENABLED rule — the DEFAULT-OFF gate.
///
/// `None` is returned for all three "unconfigured" shapes (no file ⇒ `rules: None`; a file with an
/// empty `rules` array; a file whose every rule is `enabled: false`), and in that case NOTHING is
/// constructed: no engine, no [`LogSink`], and — critically — no `WebhookSink`.
///
/// `webhooks` is a THUNK, not a value, precisely so the gate runs FIRST: an unconfigured daemon
/// never calls it, so it never opens the credential store and no Telegram token is ever loaded into
/// memory on the OFF path. The caller (the binary) owns that I/O when it does happen — the thunk is
/// `|| webhook_configs_from_env(&workspace_credentials())` in `main.rs`, so the store read is at
/// `Layer::Binary` where the settings-registry rule puts it.
pub fn maybe_mount<F>(rules: Option<AlertRuleSet>, webhooks: F) -> Option<AlertMount>
where
    F: FnOnce() -> Vec<WebhookConfig>,
{
    let set = rules?;
    if !set.rules.iter().any(|r| r.enabled) {
        return None;
    }
    let mut sinks: Vec<Box<dyn AlertSink>> = vec![Box::new(LogSink)];
    sinks.extend(ureq_webhook_sinks(webhooks()));
    Some(mount_with_sinks(set, sinks))
}

/// [`maybe_mount`] with the sinks supplied directly — the TEST seam (an `InProcessSink` or a
/// counting double stands in for the real webhook transport, so no test ever touches the network).
/// Applies the SAME default-OFF gate, so a test proving the off path proves the production one.
pub fn maybe_mount_with_sinks(
    rules: Option<AlertRuleSet>,
    sinks: Vec<Box<dyn AlertSink>>,
) -> Option<AlertMount> {
    let set = rules?;
    if !set.rules.iter().any(|r| r.enabled) {
        return None;
    }
    Some(mount_with_sinks(set, sinks))
}

/// Shared tail of the two constructors, past the gate: build the engine and register the sinks.
fn mount_with_sinks(set: AlertRuleSet, sinks: Vec<Box<dyn AlertSink>>) -> AlertMount {
    let mut engine = AlertEngine::from_rule_set(set);
    for sink in sinks {
        engine.add_sink(sink);
    }
    AlertMount { engine }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_ops::alerting::{AlertRule, Compare, RuleTrigger};

    #[test]
    fn the_gate_is_closed_for_every_unconfigured_shape() {
        // no file at all
        assert!(maybe_mount(None, Vec::new).is_none());
        // a file with zero rules
        assert!(maybe_mount(Some(AlertRuleSet::default()), Vec::new).is_none());
        // a file whose only rule is disabled
        let mut disabled = AlertRule::new("p", RuleTrigger::Drawdown { pct: 0.1 });
        disabled.enabled = false;
        let set = AlertRuleSet { rules: vec![disabled], ..Default::default() };
        assert!(maybe_mount(Some(set), Vec::new).is_none());
    }

    #[test]
    fn one_enabled_rule_mounts_and_counts_both_ways() {
        let mut disabled = AlertRule::new("d", RuleTrigger::OrderRejected);
        disabled.enabled = false;
        let set = AlertRuleSet {
            rules: vec![
                AlertRule::new(
                    "p",
                    RuleTrigger::Price {
                        venue: "binance".into(),
                        symbol: "BTCUSDT".into(),
                        op: Compare::Above,
                        level: 100.0,
                    },
                ),
                disabled,
            ],
            ..Default::default()
        };
        let mount = maybe_mount_with_sinks(Some(set), Vec::new()).expect("one enabled rule mounts");
        assert_eq!(mount.rule_count(), 2, "both rules loaded");
        assert_eq!(mount.enabled_count(), 1, "only one can fire");
    }
}
