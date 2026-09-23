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
//!   reconcile path, on by default for a live mount).
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
    AlertEngine, AlertRuleSet, AlertSink, FiredAlert, ReconAlertFact, SnapshotFacts, WebhookConfig,
    ureq_webhook_sinks,
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

/// What a published `CoreSnapshot` looks like to the rule evaluator.
///
/// ⚠ **This conversion lives HERE, and that is the whole of why `vike-alerting` can be a leaf.**
/// That crate used to take a `CoreSnapshot` directly, which put a `vike-core` edge on the crate
/// whose reason for being split out of `vike-ops` was *"so a watchdog could page without it"*. It
/// now asks for facts and the caller supplies them — and the caller is this file, which already
/// holds the snapshot.
///
/// ⚠ **`drawdown_curve` MUST come from `Portfolio::drawdown_curve`, never from
/// `CoreSnapshot::equity()` or `portfolio.equity_total`, and nothing downstream can check that any
/// more.** The rule follows the core's own latch onto the daemon's OWN equity curve (configured
/// capital + realized + unrealized P&L) rather than the cross-venue equity TOTAL, which on an
/// `Authoritative` block includes the venue's wallet for the whole account the credentials open.
/// Reading the total meant a third party's withdrawal from a SHARED account fired a drawdown alert
/// with no trading behind it, while a real 25% loss on the daemon's own ~9000 of book was 3.6% of a
/// 62647 total and never fired at all (the CI box, 2026-08-17). `Portfolio::pnl_total`'s fold is pinned
/// bit-identical to the engine-side scalar `CoreThread::sweep_drawdown_latch` acts on, so the latch
/// and the alert measure one quantity by construction.
///
/// The two tests below are that guard, and they MOVED here from
/// `crates/vike-alerting/src/eval.rs` with this conversion: once the evaluator takes a plain
/// `f64`, a test there could only prove that the number passed in is the number read back.
struct SnapshotView<'a>(&'a CoreSnapshot);

impl SnapshotFacts for SnapshotView<'_> {
    fn mark(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.0.last_mark(venue, symbol)
    }

    fn drawdown_curve(&self) -> f64 {
        self.0.portfolio.drawdown_curve()
    }

    fn capital_base(&self) -> f64 {
        self.0.portfolio.capital_base
    }

    fn pnl_total(&self) -> f64 {
        self.0.portfolio.pnl_total()
    }

    fn recon_alerts(&self) -> Vec<ReconAlertFact<'_>> {
        self.0
            .recon
            .alerts
            .iter()
            .map(|a| ReconAlertFact {
                kind: a.kind.as_str(),
                detail: a.detail.as_str(),
                proposed_event_count: a.proposed_event_count,
            })
            .collect()
    }
}

impl AlertMount {
    /// Evaluate the snapshot-driven rules against one published `CoreSnapshot` and dispatch what
    /// fired. Called once per summary tick; returns what fired (the daemon ignores it — delivery
    /// already happened inside the engine — but tests assert on it).
    pub fn on_snapshot(&mut self, snap: &CoreSnapshot, now_ms: i64) -> Vec<FiredAlert> {
        self.engine.on_snapshot(&SnapshotView(snap), now_ms)
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

    /// ⚠ **A THIRD PARTY moving money in a shared venue account must not fire a drawdown rule.**
    /// The the CI box shape (2026-08-17): a bybit block that `VIKE_RECONCILE=1` flipped to
    /// `Authoritative` by adopting the SHARED UNIFIED account's 53647.10600813 USDT
    /// `walletBalance`, next to paper mounts. `equity_total` carries that wallet, so a withdrawal
    /// collapses it with no trading behind it at all.
    ///
    /// ⚠ **MOVED here from `crates/vike-alerting/src/eval.rs` on 2026-09-23**, with the conversion
    /// it guards. That crate no longer sees a `CoreSnapshot`, so the distinction this test turns on
    /// — `Portfolio::drawdown_curve` against `equity_total`, two numbers its own aggregation law
    /// computes — cannot be expressed there at all: an evaluator handed a plain `f64` can only be
    /// shown to read back what it was given. Here it is a property of `SnapshotView`.
    ///
    /// This test FAILS if the conversion reads the wrong field: 31000 is 43% off a 54647 peak.
    #[test]
    fn a_venue_wallet_movement_with_no_trading_behind_it_never_fires() {
        let set = AlertRuleSet {
            rules: vec![AlertRule::new("d", RuleTrigger::Drawdown { pct: 0.10 })],
            ..Default::default()
        };
        let mut mount = maybe_mount_with_sinks(Some(set), Vec::new()).expect("one enabled rule");

        // The daemon's own book is FLAT and stays flat: no position, no fill, no fee, no funding.
        let flat = |wallet: f64| {
            let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
            s.portfolio.capital_base = 1_000.0;
            s.portfolio.venues = vec![
                vike_core::snapshot::VenueBlock { venue: "binance".into(), ..Default::default() },
                vike_core::snapshot::VenueBlock {
                    venue: "bybit".into(),
                    // `balance_mode` deliberately unset (`Delta`, the `Default`): `pnl_total` is
                    // MODE-BLIND — that is exactly the property under test, and it means the guard
                    // does not depend on correctly classifying the block.
                    balance: wallet,
                    equity: wallet,
                    ..Default::default()
                },
            ];
            s.portfolio.equity_total = wallet + 1_000.0;
            s
        };

        assert!(mount.on_snapshot(&flat(53_647.10600813), 1).is_empty(), "peak established, flat");
        assert!(
            mount.on_snapshot(&flat(30_000.0), 2).is_empty(),
            "a 23647 wallet withdrawal is 43% off the equity TOTAL and must fire nothing: the \
             daemon traded nothing, so its own curve never moved"
        );
    }

    /// The other half, and the one that must NOT be lost in the process: a real loss on the
    /// daemon's OWN book still fires, at the configured fraction of its own capital — even while a
    /// large adopted wallet sits beside it making the loss look like noise in `equity_total`.
    ///
    /// Also MOVED from `vike-alerting` with the conversion. It FAILS if the conversion reads the
    /// total: a 250 loss is 25% of the daemon's 1000 of book but 0.46% of a 54647 total.
    #[test]
    fn a_real_loss_on_the_daemons_own_book_still_fires_beside_a_large_wallet() {
        let set = AlertRuleSet {
            rules: vec![AlertRule::new("d", RuleTrigger::Drawdown { pct: 0.20 })],
            ..Default::default()
        };
        let mut mount = maybe_mount_with_sinks(Some(set), Vec::new()).expect("one enabled rule");

        let book = |own_pnl: f64| {
            let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
            s.portfolio.capital_base = 1_000.0;
            s.portfolio.venues = vec![vike_core::snapshot::VenueBlock {
                venue: "bybit".into(),
                balance: 53_647.10600813,
                unrealized: own_pnl,
                equity: 53_647.10600813 + own_pnl,
                ..Default::default()
            }];
            s.portfolio.equity_total = 53_647.10600813 + own_pnl;
            s
        };

        assert!(mount.on_snapshot(&book(0.0), 1).is_empty(), "flat: peak = 1000");
        let fired = mount.on_snapshot(&book(-250.0), 2);
        assert_eq!(fired.len(), 1, "a 25% loss of the daemon's own capital must fire a 20% rule");
        assert!(fired[0].body.contains("own-PnL drawdown 25.00%"), "body: {}", fired[0].body);
        assert!(
            fired[0].body.contains("capital_base 1000"),
            "body names the base: {}",
            fired[0].body
        );
    }
}
