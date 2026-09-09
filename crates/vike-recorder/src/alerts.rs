//! `alerts` — where a SILENT SERIES goes. The half `crates/vike-recorder/src/liveness.rs` was
//! missing.
//!
//! ## The gap this closes
//!
//! `liveness` already computed the condition perfectly and then threw the result away: the bin's
//! `report_silence` logged two `tracing::warn!`s and discarded the `Vec<Silent>`. On the CI box that
//! fired **20 times in 24 hours** across 6 series, into a log nothing was reading — and on
//! 2026-08-05 a Polymarket series collapsed from ~250–370k rows/min to **18/min for 19 minutes**
//! (~3.8M rows lost) while the daemon looked completely healthy from every other vantage point.
//! Detection was never the missing piece. Delivery was.
//!
//! ## Why `vike-alerting`, and why that is FREE here
//!
//! `vike-alerting` is the workspace's alerting primitive, and its DEFAULT build has **zero `vike-*`
//! dependencies** — a property with its own CI gate (`scripts/ci_feature_suite.sh`'s
//! `alerting-standalone` lane runs `cargo tree -p vike-alerting -e normal` and fails on any `vike-`
//! line). That crate exists BECAUSE of this exact failure: the latency box crashed, ~6h of Polymarket L2 tape
//! was lost unnoticed, and the wanted fix was something that could page without linking
//! `vike-core -> vike-exec -> vike-model`. So this module takes it with default features and adds
//! nothing to that graph.
//!
//! The trigger is `vike_alerting::RuleTrigger::SeriesStale`, which names a SERIES. The nearest
//! pre-existing variant, `Feed`, is venue-scoped and reports a transport transition — it cannot say
//! *which* of a recorder's series went quiet, and "which" is the entire diagnosis.
//!
//! ## Default ON, and log-only until a target is named
//!
//! An absent `[alerting]` table means DEFAULTS (see `crate::config::Alerting`), so the engine is
//! mounted on every recorder and every silent series produces a real `FiredAlert` that reaches
//! [`LogSink`]. Naming a webhook target escalates that same alert to a pager. A trigger nobody
//! feeds is the defect this work exists to remove, so the wiring is not behind a switch — only its
//! reach is.
//!
//! ## …and its FEED-level twin
//!
//! [`resolve_tick`] is the same three steps (judge, log, alert) for the failure the series watchdog
//! is structurally blind to: a venue that never resolved a symbol at all, so it subscribed nothing,
//! so there is no series for [`watchdog_tick`] to find. Measured on the CI box from a clean install: a
//! Polymarket proxy nothing was listening behind produced ONE `warn!` per tick, forever, while the
//! daemon reported a healthy startup and recorded not one row. It reuses `RuleTrigger::Feed` — a
//! venue-scoped degradation, which is precisely what this is — rather than adding a variant to a
//! crate BELOW this one; the cost is that the alert body cannot carry the resolver's error, so the
//! error rides the adjacent `tracing::error!`.
//!
//! ## Rate limiting lives on the PRODUCER side
//!
//! One rule covers every series (a recorder's series set rotates, so per-series rules are
//! unwritable), and `AlertRule::cooldown_ms` is per RULE — six silent series would page once. The
//! per-series gate is `crate::liveness::SilenceWatch::alertable`, and this module's rule carries no
//! cooldown of its own so the two cannot fight.
//!
//! ## …and delivery is OFF the tick, because the tick also polls the stop flag
//!
//! Rate limiting bounds how MANY alerts a tick raises. It does not bound how LONG raising them
//! takes, and until this module wrapped its sinks that was unbounded in the worst possible place.
//! `AlertEngine::dispatch` is a nested serial loop and `vike_alerting::WebhookSink`'s `deliver` is
//! a blocking HTTP POST whose only bound is the transport's 10 s global timeout; every one of them
//! ran inline on `crates/vike-recorder/src/recorder_cli.rs`'s single `loop { rt.tick(..) … }` — the
//! same loop that observes the stop flag. [`RecorderAlerts::on_silence`] dispatches once per SILENT
//! SERIES and the per-series gate ([`crate::liveness::SilenceWatch::alertable`]) makes every silent
//! series due on the FIRST tick of an outage, over a series set that is one entry per
//! (symbol, stream) — it grows with a profile's family glob. 100 series x 2 webhook targets x 10 s
//! is ~2000 s: **~33 minutes in which a `systemctl stop` reaches its `TimeoutStopSec=` and SIGKILL
//! lands on the writer instead of the flush.**
//!
//! So [`RecorderAlerts::mount`] builds its webhook targets through
//! `vike_alerting::queued_ureq_webhook_sinks`: a bounded queue and one delivery thread per target,
//! so a tick pays an enqueue whatever the endpoint is doing. Three consequences worth carrying:
//!
//! * **An unconfigured recorder spawns no thread.** With no webhook target the only sink is
//!   [`LogSink`], a `tracing` call. The default install is byte-identical.
//! * **A full queue DROPS, counted and logged** (`vike_alerting::QueuedSink`'s overflow policy).
//!   The alternative — blocking when full — hands the stall straight back.
//! * **The threads are stopped inside the daemon's already-capped teardown** via
//!   [`stop_delivery`], never joined unbounded: a worker caught mid-POST would otherwise add the
//!   transport's timeout to a stop budget that a systemd `TimeoutStopSec=` is sized against.
//!
//! ⚠ What this deliberately does NOT do is COALESCE. N silent series in one outage is arguably one
//! incident, and one message naming N series would serve the human on the pager better than N
//! messages — but that is a change of MEANING, not a bound: the argument above (six silent series
//! must read as six, not as one) is this module's, `series_prefix` scoping lives in the RULE, and
//! one coalesced alert on two dead targets is still 20 s of a stop flag going unread. Coalescing
//! stays open on top of the queue, where the incident is known; it is not a substitute for it.

use std::collections::HashMap;
use std::time::Duration;

use vike_alerting::{
    AlertEngine, AlertRule, AlertSignal, AlertSink, AlertTargets, DEFAULT_QUEUE_CAPACITY,
    FeedState, FiredAlert, QueuedSinkOutcome, QueuedSinkStop, RuleTrigger, WebhookConfig,
    queued_ureq_webhook_sinks,
};
use vike_data::Liveness;

use crate::config::Alerting;
use crate::liveness::{FeedResolve, ResolveWatch, SilenceWatch, Silent};
use crate::runtime::FeedTick;

/// The rule id every recorder-raised alert correlates on. Stable: it is the delivery correlation
/// id an operator greps for.
pub const RULE_ID: &str = "recorder-series-stale";

/// The rule id for the FEED-level alert: a venue that has never resolved a symbol since startup.
///
/// A second rule rather than a second meaning for [`RULE_ID`], because the two are different faults
/// with different fixes — "this series stopped receiving rows" is a venue-side death, "this venue
/// resolved nothing, ever" is a misconfiguration on THIS box — and an operator correlating on a
/// rule id must be able to tell them apart.
pub const VENUE_RULE_ID: &str = "recorder-venue-unresolved";

/// The always-registered sink: every fired alert becomes one structured `tracing` record on the
/// vike-log file/stderr layer, NEVER stdout (which stays the daemon's protocol channel — the same
/// contract `vike_tradehub::alerts::LogSink` keeps).
///
/// `error`, not `warn`: the per-tick `report_silence` warning is the OBSERVATION and it repeats
/// every tick; this is the gated NOTIFICATION, and if it is in the log at all something is wrong
/// that nobody has been told about by any other means.
pub struct LogSink;

impl AlertSink for LogSink {
    fn deliver(&self, alert: &FiredAlert) {
        tracing::error!(
            rule = %alert.rule_id,
            ts_ms = alert.ts_ms,
            "RECORDER ALERT: {}",
            alert.body
        );
    }
}

/// The mounted engine, the repeat window it was configured with, and the stop handles of its
/// off-thread delivery workers.
pub struct RecorderAlerts {
    engine: AlertEngine,
    repeat_ms: i64,
    /// One per configured webhook target — see [`RecorderAlerts::take_delivery_stops`]. EMPTY on
    /// an unconfigured recorder (log-only delivery spawns no thread at all), which is the common
    /// case and the reason this costs a default install nothing.
    delivery_stops: Vec<QueuedSinkStop>,
}

impl RecorderAlerts {
    /// Mount over a profile's `[alerting]` table and the webhook targets the caller resolved from
    /// the credential store.
    ///
    /// `webhooks` is a VALUE, not a thunk (unlike `vike_tradehub::alerts::maybe_mount`'s): there is
    /// no OFF path to protect here — the engine mounts unconditionally — so there is nothing to
    /// defer, and the binary already opened the store for its own resolution.
    ///
    /// A target NAMED in the profile but absent from `webhooks` (no credentials for it) delivers
    /// nowhere, which is the workspace's absent-credentials-is-the-gate idiom and byte-identical to
    /// not naming it. It is logged once at mount so it is not silent.
    ///
    /// ⚠ **A mount with webhook targets SPAWNS one delivery thread per target** (see the module
    /// doc's off-the-tick section), and its owner must stop them: take the handles with
    /// [`take_delivery_stops`](Self::take_delivery_stops) and pass them to [`stop_delivery`]
    /// inside whatever bounded teardown it owns. A mount with NO targets — the default — spawns
    /// nothing, so a log-only recorder is unchanged.
    pub fn mount(cfg: &Alerting, webhooks: Vec<WebhookConfig>) -> Self {
        for want in &cfg.webhooks {
            if !webhooks.iter().any(|c| &c.name == want) {
                tracing::warn!(
                    target_name = %want,
                    "recorder: [alerting].webhooks names a target with no credentials configured \
                     — silent-series alerts will reach the log only"
                );
            }
        }

        let rule = AlertRule {
            id: RULE_ID.to_string(),
            name: "recorder: series stopped receiving rows".to_string(),
            enabled: true,
            trigger: RuleTrigger::SeriesStale { series_prefix: cfg.series_prefix.clone() },
            targets: AlertTargets { in_process: false, webhooks: cfg.webhooks.clone() },
            // Deliberately 0 — see the module doc: the per-SERIES gate is `SilenceWatch::alertable`
            // and a rule-level cooldown here would swallow every series but the first.
            cooldown_ms: 0,
            once: false,
        };

        // The FEED-level rule. `RuleTrigger::Feed` is REUSED rather than a new variant added:
        // `vike-alerting` is a shared home BELOW this crate, and a venue-scoped degradation is
        // exactly what that trigger already means. The cost is accepted and stated — the fired
        // body is the fixed `feed {venue} degraded`, so the DIAGNOSIS (the resolver's own error)
        // rides the adjacent `tracing::error!` in `resolve_tick`. Carrying it in the body would
        // need a new `AlertSignal` variant, i.e. a coordinated PR against a lower crate, not a
        // rider here.
        //
        // ⚠ `series_prefix` deliberately does NOT scope this one: it is a prefix on a SERIES key
        // (`{kind}/{venue}/{symbol}`), and a venue that never resolved has no series to match. A
        // profile that scoped its alerts to one family would otherwise silence the very failure
        // that produced no family at all.
        let venue_rule = AlertRule {
            id: VENUE_RULE_ID.to_string(),
            name: "recorder: venue has never resolved a symbol".to_string(),
            enabled: true,
            trigger: RuleTrigger::Feed { venue: None, state: FeedState::Degraded },
            targets: AlertTargets { in_process: false, webhooks: cfg.webhooks.clone() },
            // 0 for the same reason as the rule above: the gate is `ResolveWatch::alertable`, and
            // a per-RULE cooldown would page for one venue of a three-venue outage.
            cooldown_ms: 0,
            once: false,
        };

        // ⚠ QUEUED, not raw. `vike_alerting::ureq_webhook_sinks` builds sinks whose `deliver` is a
        // BLOCKING HTTP POST bounded only by the transport's 10 s global timeout, and this engine
        // is folded from `crates/vike-recorder/src/recorder_cli.rs`'s `run` — the single loop that
        // also polls the stop flag. One alert per silent series, times one POST per target, times
        // a dead endpoint, parked that loop for tens of minutes on the first tick of a venue
        // outage. `queued_ureq_webhook_sinks` puts a bounded queue and one thread in front of each
        // target, so this daemon's tick pays an enqueue and nothing else; the overflow policy and
        // the shutdown bound are on `vike_alerting::QueuedSink`.
        //
        // With no webhook target configured — the default — this spawns NOTHING: the log sink
        // above is a `tracing` call and needs no thread.
        let mut engine = AlertEngine::new(vec![rule, venue_rule]).with_sink(Box::new(LogSink));
        let (sinks, delivery_stops) = queued_ureq_webhook_sinks(webhooks, DEFAULT_QUEUE_CAPACITY);
        for sink in sinks {
            engine.add_sink(sink);
        }
        RecorderAlerts {
            engine,
            repeat_ms: (cfg.repeat_secs as i64).saturating_mul(1_000),
            delivery_stops,
        }
    }

    /// Take the delivery workers' stop handles, leaving none behind.
    ///
    /// The engine keeps the SINKS — it must, or a stop raised mid-teardown would have nowhere to
    /// deliver — and the caller takes the handles into whatever bounded region it tears down in.
    /// `crates/vike-recorder/src/recorder_cli.rs`'s `run` does exactly that, which is why this is
    /// a take rather than a `shutdown(&mut self)`: that teardown runs inside a `Send + 'static`
    /// closure on another thread, and moving the whole mount there would move the engine with it.
    pub fn take_delivery_stops(&mut self) -> Vec<QueuedSinkStop> {
        std::mem::take(&mut self.delivery_stops)
    }

    /// Register an extra delivery sink. The TEST seam — a recording double stands in for the
    /// webhook transport, so no test touches the network — and the extension point if this daemon
    /// ever grows a second notification surface.
    pub fn with_sink(mut self, sink: Box<dyn AlertSink>) -> Self {
        self.engine.add_sink(sink);
        self
    }

    /// Raise one alert per series that is DUE to page, and return what fired (already delivered).
    ///
    /// `silent` is `SilenceWatch::check`'s whole output — every silent series, every tick.
    /// [`SilenceWatch::alertable`] narrows it to what should page now; the engine folds each through
    /// the real rule, so the profile's `series_prefix` scope is applied by `vike-alerting`, not
    /// re-implemented here.
    pub fn on_silence(
        &mut self,
        watch: &mut SilenceWatch,
        silent: &[Silent],
        now_ms: i64,
    ) -> Vec<FiredAlert> {
        watch
            .alertable(silent, now_ms, self.repeat_ms)
            .iter()
            .flat_map(|s| self.engine.on_signal(&signal_for(s), now_ms))
            .collect()
    }
}

impl RecorderAlerts {
    /// Raise one alert per venue that is DUE to page for having never resolved, and return what
    /// fired (already delivered). The twin of [`on_silence`](Self::on_silence), one level up.
    pub fn on_unresolved(
        &mut self,
        watch: &mut ResolveWatch,
        venues: &[String],
        now_ms: i64,
    ) -> Vec<FiredAlert> {
        watch
            .alertable(venues, now_ms, self.repeat_ms)
            .iter()
            .flat_map(|venue| {
                let sig = AlertSignal::Feed { venue: venue.clone(), degraded: true };
                self.engine.on_signal(&sig, now_ms)
            })
            .collect()
    }
}

/// **One resolve-watchdog tick: judge, log, alert** — the FEED-level twin of [`watchdog_tick`], and
/// in the LIBRARY for the same reason (`tests/resolve_alert.rs` drives this exact function, so
/// deleting the escalation turns a test red instead of turning a pager off).
///
/// ## The escalation ladder, and why it has two rungs
///
/// * A venue that HAS resolved before and is failing now stays a per-tick `tracing::warn!` in the
///   binary's `report` — untouched, verbatim. `crate::runtime::FeedTick::ResolveFailed`'s "this is
///   a retry, not a gap" is CORRECT for that case: the daemon deliberately leaves its live books
///   alone and tries again in 30 seconds, and paging for a Gamma blip is how a pager gets muted.
/// * A venue that has NEVER resolved, past its grace, is a `tracing::error!` **and a real
///   `FiredAlert`**. That state does not fix itself — a proxy nothing is listening behind, a
///   family name matching no market — and today it produced one warning a tick, forever, while the
///   daemon reported success and recorded nothing.
///
/// `grace_secs` is `--silent-secs` (default 300, `0` disables): the same operator knob that turns
/// the series watchdog off, because both answer "this subscription is producing nothing" and an
/// operator who switched that alarm off must get silence rather than a quieter alarm. ⚠ The `--once`
/// dry run is deliberately NOT routed through this: a single tick can never pass a 300-second
/// grace, so `crate::runtime::dry_run_failures` judges the tick's content directly.
///
/// Returns the venues reported this tick (all of them, whether or not they alerted).
pub fn resolve_tick(
    watch: &mut ResolveWatch,
    alerts: &mut RecorderAlerts,
    ticks: &[FeedTick],
    now_ms: i64,
    grace_secs: u64,
) -> Vec<String> {
    if grace_secs == 0 {
        return Vec::new();
    }
    let feeds: Vec<FeedResolve> = ticks
        .iter()
        .map(|t| FeedResolve {
            venue: t.venue().to_string(),
            last_nonempty_ms: t.last_nonempty_ms(),
        })
        .collect();
    let unresolved = watch.check(&feeds, now_ms, grace_secs as i64 * 1_000);

    // The LOG is unconditional and per tick — the forensic column, and the place the DIAGNOSIS
    // lives, since `AlertSignal::Feed`'s body cannot carry it.
    for venue in &unresolved {
        let why = ticks.iter().find(|t| t.venue() == venue).and_then(|t| match t {
            FeedTick::ResolveFailed { error, .. } => Some(error.clone()),
            FeedTick::Reconciled { .. } => None,
        });
        match why {
            Some(error) => tracing::error!(
                %venue,
                %error,
                "recorder: venue has NEVER resolved a symbol since startup — NOTHING is being \
                 recorded for it, and this will not fix itself"
            ),
            // The quieter sibling: `Ok(empty)` every tick. No error to print, so say what it is.
            None => tracing::error!(
                %venue,
                "recorder: venue has NEVER resolved a symbol since startup — it answers with an \
                 EMPTY symbol set, so nothing is subscribed and NOTHING is being recorded for it"
            ),
        }
    }

    // …and the ALERT is gated per venue, so a three-venue outage pages three times and a
    // still-broken venue does not page every 30 seconds for a week.
    alerts.on_unresolved(watch, &unresolved, now_ms);

    unresolved
}

/// **Stop every alert delivery worker, bounded, and say what that cost.** The teardown half of
/// `RecorderAlerts::mount`'s queued sinks, called from
/// `crates/vike-recorder/src/recorder_cli.rs`'s `run` inside its already-capped teardown region.
///
/// `budget` is for the WHOLE set, not per target: the handles are stopped against one deadline, so
/// two configured webhooks cannot cost twice what one does. A worker that is idle — the normal
/// case, since a stop follows a tick in which nothing was queued — joins within one
/// `vike_alerting::STOP_POLL` (it re-reads its flag only when its receive poll returns, so the
/// cost is up to a tenth of a second, NOT microseconds), and the handles are stopped one after
/// another, so the normal cost of this call is up to one poll PER TARGET: a few hundred
/// milliseconds for two, well inside the budget its caller hands it. A worker caught inside a
/// POST is ABANDONED when the deadline passes, which is the trade
/// `vike_alerting::QueuedSinkStop::shutdown` documents: at most one un-POSTed page, against a
/// teardown that must not outrun the unit's `TimeoutStopSec=`.
///
/// ⚠ The shared deadline has a corner the disclosure must get right: when an EARLIER target ate
/// the whole budget, every later one is stopped with ~0 left, and an idle worker cannot have woken
/// to see its flag inside ~0 — so it reads as abandoned too. That is a thread left to exit on its
/// own within one poll, NOT a page left on the wire, and the two are logged as different facts:
/// the outcome's `mid_delivery` says which, and only the true one gets the "may not have arrived"
/// line. Floor-per-target was the alternative and was not taken, because it makes the set's cost
/// `budget + (targets - 1) x STOP_POLL` and this doc's first sentence false.
///
/// Every outcome is logged, and an abandonment or a drop is logged at `warn!` — this daemon's whole
/// subject is a failure that is silent by nature, so an alert that never went out may not be a
/// silent fact either.
///
/// An empty `stops` (the unconfigured recorder, which mounts no delivery thread) is a no-op that
/// logs nothing.
pub fn stop_delivery(stops: Vec<QueuedSinkStop>, budget: Duration) {
    if stops.is_empty() {
        return;
    }
    let deadline = std::time::Instant::now() + budget;
    for stop in stops {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match stop.shutdown(left) {
            QueuedSinkOutcome::Joined { name, delivered, dropped: 0 } => {
                tracing::info!(
                    target_name = %name,
                    delivered,
                    "recorder: alert delivery stopped"
                );
            }
            QueuedSinkOutcome::Joined { name, delivered, dropped } => tracing::warn!(
                target_name = %name,
                delivered,
                dropped,
                "recorder: alert delivery stopped — {dropped} alert(s) were never sent to this \
                 target this session"
            ),
            QueuedSinkOutcome::Abandoned {
                name,
                delivered,
                dropped,
                budget,
                mid_delivery: true,
            } => tracing::warn!(
                target_name = %name,
                delivered,
                dropped,
                budget_ms = budget.as_millis() as u64,
                "recorder: alert delivery did not finish inside its stop budget — the thread was \
                 left running and the alert it was sending may not have arrived"
            ),
            // The shared-deadline corner: an earlier target spent the budget and this one, idle,
            // never got the poll it needed to notice the flag. Nothing was in flight and nothing
            // is lost — it exits on its own — so this is NOT the "may not have arrived" line, and
            // it is `warn!` only because a teardown that left a thread unjoined is worth a line.
            QueuedSinkOutcome::Abandoned {
                name,
                delivered,
                dropped,
                budget,
                mid_delivery: false,
            } => tracing::warn!(
                target_name = %name,
                delivered,
                dropped,
                budget_ms = budget.as_millis() as u64,
                "recorder: alert delivery worker was idle but not joined — its stop budget was \
                 spent by an earlier target; nothing was in flight and it exits on its own"
            ),
        }
    }
}

/// A `Silent` as the alerting vocabulary sees it. Both shapes survive the crossing —
/// `silent_for_ms: None` stays "never received a row", which is a different fault from "stopped".
fn signal_for(s: &Silent) -> AlertSignal {
    AlertSignal::SeriesStale {
        series: s.series.clone(),
        silent_for_ms: s.silent_for_ms,
        rows: s.rows,
    }
}

/// **One watchdog tick: judge, log, alert.** The daemon's whole silence reaction, in the LIBRARY
/// rather than in `src/bin/vike_recorder.rs`.
///
/// It lives here because the alerting call is the entire deliverable of this work, and a line that
/// only exists in a binary is a line no test drives — which is precisely how the previous version
/// of this reaction (compute the verdict, log it, drop it) survived. `tests/silence_alert.rs`
/// exercises this function, so deleting the `on_silence` call below turns a test RED instead of
/// turning a pager off.
///
/// `expected` is `RecorderRuntime::expected_series()` and `live` is `RecorderHandle::liveness()`;
/// the binary supplies both and owns the clock. Returns the silent series (all of them, whether or
/// not they alerted this tick) so the caller can act — `--exit-on-silence`.
///
/// `silent_secs == 0` disables the watchdog outright: no judgement, no log, no alert.
pub fn watchdog_tick(
    watch: &mut SilenceWatch,
    alerts: &mut RecorderAlerts,
    expected: &[String],
    live: &HashMap<String, Liveness>,
    now_ms: i64,
    silent_secs: u64,
) -> Vec<String> {
    if silent_secs == 0 {
        return Vec::new();
    }
    let silent = watch.check(expected, live, now_ms, silent_secs as i64 * 1_000);

    // The LOG is unconditional and per tick — that column is the forensic record, and it is what
    // an operator reads AFTER being paged.
    for s in &silent {
        match s.silent_for_ms {
            // Received rows once, then stopped — usually a venue-side stream death.
            Some(ms) => tracing::warn!(
                series = %s.series,
                silent_secs = ms / 1_000,
                rows = s.rows,
                "recorder: SUBSCRIBED BUT SILENT — this series has stopped receiving rows"
            ),
            // Never received one — usually a wrong stream name, or a subscription the venue
            // accepted and never served.
            None => tracing::warn!(
                series = %s.series,
                "recorder: SUBSCRIBED BUT NEVER RECEIVED A ROW — check the venue's stream name"
            ),
        }
    }

    // …and the ALERT is gated per series, because a pager that fires every 30 seconds for hours is
    // one that gets muted, and a muted pager is where this whole failure started.
    alerts.on_silence(watch, &silent, now_ms);

    silent.into_iter().map(|s| s.series).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use vike_alerting::{QueuedSink, WebhookKind};

    /// A recording sink — the same shape as `vike_alerting::InProcessSink` but blind to
    /// `AlertTargets`, so it observes what the ENGINE dispatched rather than what a target routed.
    /// The production rule sets `in_process: false` (a headless daemon has no toast surface), so an
    /// `InProcessSink` would legitimately see nothing and prove nothing.
    #[derive(Clone, Default)]
    struct Recorded(Arc<Mutex<Vec<FiredAlert>>>);

    impl AlertSink for Recorded {
        fn deliver(&self, alert: &FiredAlert) {
            self.0.lock().unwrap().push(alert.clone());
        }
    }

    impl Recorded {
        fn bodies(&self) -> Vec<String> {
            self.0.lock().unwrap().iter().map(|a| a.body.clone()).collect()
        }
    }

    fn mounted(cfg: &Alerting) -> (RecorderAlerts, Recorded) {
        let tap = Recorded::default();
        let mut a = RecorderAlerts::mount(cfg, Vec::new());
        a.engine.add_sink(Box::new(tap.clone()));
        (a, tap)
    }

    fn stopped(series: &str) -> Silent {
        Silent { series: series.to_string(), silent_for_ms: Some(19 * 60_000), rows: 3_800_000 }
    }

    /// **The deliverable.** A series goes silent and an ALERT is produced — not a `warn!` that
    /// nothing can observe. The 2026-08-05 shape: one Polymarket series, silent 19 minutes.
    #[test]
    fn a_silent_series_produces_an_alert_naming_it() {
        let (mut alerts, tap) = mounted(&Alerting::default());
        let mut watch = SilenceWatch::new();

        let fired = alerts.on_silence(&mut watch, &[stopped("book/polymarket/0xtok")], 1_000);

        assert_eq!(fired.len(), 1, "the silent series raised exactly one alert");
        assert_eq!(fired[0].rule_id, RULE_ID);
        let bodies = tap.bodies();
        assert_eq!(bodies.len(), 1, "…and it was DELIVERED, not just returned");
        assert!(bodies[0].contains("book/polymarket/0xtok"), "the series is named: {}", bodies[0]);
        assert!(bodies[0].contains("1140s"), "…with how long it has been silent: {}", bodies[0]);
    }

    /// A never-started series is the other half of the watchdog's judgment and must survive the
    /// crossing into the alert intact — it is a different fault with a different fix.
    #[test]
    fn a_never_started_series_alerts_as_never_started() {
        let (mut alerts, tap) = mounted(&Alerting::default());
        let mut watch = SilenceWatch::new();
        let never = Silent { series: "depth/binance/BTCUSDT".into(), silent_for_ms: None, rows: 0 };

        assert_eq!(alerts.on_silence(&mut watch, &[never], 1_000).len(), 1);
        assert!(tap.bodies()[0].contains("NEVER received a row"), "{:?}", tap.bodies());
    }

    /// Six series silent at once ⇒ six alerts. A per-RULE cooldown would produce one, which reads
    /// as a single-series fault — see the module doc.
    #[test]
    fn every_silent_series_alerts_not_only_the_first() {
        let (mut alerts, _tap) = mounted(&Alerting::default());
        let mut watch = SilenceWatch::new();
        let six: Vec<Silent> =
            (0..6).map(|i| stopped(&format!("book/polymarket/tok{i}"))).collect();

        assert_eq!(alerts.on_silence(&mut watch, &six, 1_000).len(), 6);
    }

    /// The profile's prefix scope is honored — and honored by the RULE, not by a second filter
    /// here, so the two cannot disagree.
    #[test]
    fn the_profile_prefix_scopes_which_series_alert() {
        let cfg = Alerting { series_prefix: Some("book/polymarket/".into()), ..Default::default() };
        let (mut alerts, tap) = mounted(&cfg);
        let mut watch = SilenceWatch::new();

        let fired = alerts.on_silence(
            &mut watch,
            &[stopped("book/polymarket/0xtok"), stopped("trade/binance/BTCUSDT")],
            1_000,
        );
        assert_eq!(fired.len(), 1, "only the in-scope series alerts: {fired:?}");
        assert!(tap.bodies()[0].contains("polymarket"));
    }

    /// The repeat window is respected across ticks, and a recovery re-arms it. Together with
    /// `liveness`' own tests this pins the whole rate-limit story from the mount's side.
    #[test]
    fn a_still_silent_series_does_not_repage_but_a_new_episode_does() {
        let cfg = Alerting { repeat_secs: 3600, ..Default::default() };
        let (mut alerts, _tap) = mounted(&cfg);
        let mut watch = SilenceWatch::new();
        let s = [stopped("a/v/s")];

        assert_eq!(alerts.on_silence(&mut watch, &s, 0).len(), 1);
        assert!(alerts.on_silence(&mut watch, &s, 30_000).is_empty(), "still inside the window");
        assert!(alerts.on_silence(&mut watch, &[], 60_000).is_empty(), "recovered: nothing fires");
        assert_eq!(
            alerts.on_silence(&mut watch, &s, 90_000).len(),
            1,
            "a fresh episode pages at once, well inside the old repeat window"
        );
    }

    /// Nothing silent ⇒ nothing delivered. The steady state of a healthy recorder is the one this
    /// must not be noisy in.
    #[test]
    fn a_healthy_tick_delivers_nothing() {
        let (mut alerts, tap) = mounted(&Alerting::default());
        let mut watch = SilenceWatch::new();
        assert!(alerts.on_silence(&mut watch, &[], 1_000).is_empty());
        assert!(tap.bodies().is_empty());
    }

    /// A named target with no credentials degrades to log-only rather than failing the mount — the
    /// absent-credentials-is-the-gate idiom. (The warning it logs is asserted by eye, not here;
    /// what matters is that the alert still fires.)
    #[test]
    fn a_named_target_without_credentials_still_alerts_to_the_log() {
        let cfg = Alerting { webhooks: vec!["telegram".into()], ..Default::default() };
        let (mut alerts, tap) = mounted(&cfg);
        let mut watch = SilenceWatch::new();
        assert_eq!(alerts.on_silence(&mut watch, &[stopped("a/v/s")], 1).len(), 1);
        assert_eq!(tap.bodies().len(), 1);
    }

    // ── delivery is OFF the tick ───────────────────────────────────────────────────────────────

    /// How long [`SlowTarget`] takes to answer one alert. The real worst case is `UreqTransport`'s
    /// 10 s global timeout; half a second is enough to make the arithmetic visible in a test that
    /// still finishes in seconds.
    const SLOW_DELIVERY: Duration = Duration::from_millis(500);

    /// An endpoint that takes [`SLOW_DELIVERY`] to answer.
    struct SlowTarget;

    impl AlertSink for SlowTarget {
        fn deliver(&self, _alert: &FiredAlert) {
            std::thread::sleep(SLOW_DELIVERY);
        }
    }

    /// **The defect, at the level it bit.** Six silent series with one slow target is 3 s of
    /// endpoint work; the watchdog tick that raises them must not wait for any of it, because that
    /// tick is the same loop that polls the daemon's stop flag. Unqueued, this is the 33-minute
    /// stall in miniature.
    ///
    /// The ceiling on the tick is ONE delivery ([`SLOW_DELIVERY`], 500 ms): ~50x what six rule
    /// evaluations plus six enqueues cost on a quiet box (under 10 ms), so a scheduler stall of a
    /// few hundred milliseconds on a loaded CI lane passes, while a tick parked for even a single
    /// POST fails. The `fired.len()` assertion is the other half and is not timing-sensitive.
    #[test]
    fn a_slow_delivery_target_does_not_park_the_watchdog_tick() {
        let (queued, stop) =
            QueuedSink::spawn("slow", DEFAULT_QUEUE_CAPACITY, Box::new(SlowTarget));
        let mut alerts =
            RecorderAlerts::mount(&Alerting::default(), Vec::new()).with_sink(Box::new(queued));
        let mut watch = SilenceWatch::new();
        let six: Vec<Silent> =
            (0..6).map(|i| stopped(&format!("book/polymarket/tok{i}"))).collect();

        let began = Instant::now();
        let fired = alerts.on_silence(&mut watch, &six, 1_000);
        let dispatched = began.elapsed();

        assert_eq!(fired.len(), 6, "all six still fire — the queue changes WHEN, not WHETHER");
        assert!(
            dispatched < SLOW_DELIVERY,
            "the tick waited {dispatched:?} on the endpoint — at least one whole delivery \
             ({SLOW_DELIVERY:?} each, 3 s in all); none of that may be the tick's problem"
        );
        // A budget that covers the 3 s drain with room for a loaded box, so the worker is joined
        // rather than abandoned mid-test.
        stop_delivery(vec![stop], Duration::from_secs(10));
    }

    /// The default install pays nothing for this: with no webhook target the only sink is
    /// [`LogSink`], and there is no thread to stop.
    #[test]
    fn an_unconfigured_recorder_mounts_no_delivery_thread() {
        let mut alerts = RecorderAlerts::mount(&Alerting::default(), Vec::new());
        assert!(alerts.take_delivery_stops().is_empty());
        // …and the teardown call is a no-op rather than an error on that path.
        stop_delivery(Vec::new(), Duration::from_secs(1));
    }

    /// A configured target mounts exactly one worker, and the teardown JOINS it — the lifecycle
    /// the daemon's bounded teardown depends on. Nothing fires here, so nothing is POSTed and no
    /// test touches the network.
    #[test]
    fn a_configured_target_mounts_one_worker_that_the_teardown_stops() {
        let cfg = Alerting { webhooks: vec!["webhook".into()], ..Default::default() };
        let targets = vec![WebhookConfig {
            name: "webhook".into(),
            kind: WebhookKind::Generic { url: "https://example.invalid/never-called".into() },
        }];
        let mut alerts = RecorderAlerts::mount(&cfg, targets);

        let stops = alerts.take_delivery_stops();
        assert_eq!(stops.iter().map(|s| s.name()).collect::<Vec<_>>(), ["webhook"]);
        assert!(alerts.take_delivery_stops().is_empty(), "the take leaves none behind");
        stop_delivery(stops, Duration::from_secs(5));
    }

    /// The number `stop_delivery`'s doc states, held: two IDLE targets stopped against one shared
    /// budget cost up to one `STOP_POLL` EACH — a few hundred milliseconds — and both are joined,
    /// well inside the daemon's `ALERT_STOP_BUDGET_SECS`.
    ///
    /// The ceiling asserted is twenty polls (2 s) for two targets — 10x the two polls the doc
    /// states as the worst case — because the cost being measured is two thread WAKE-UPS, and this
    /// runs on a shared CI lane whose load has been measured well past one runnable thread per
    /// core. The budget handed to `stop_delivery` is deliberately far WIDER than the ceiling
    /// (10 s, the transport's global timeout): the regression this refutes is an idle stop that
    /// costs a delivery timeout — a `join()` where a bounded poll belongs — and a budget no wider
    /// than the ceiling would return from the ABANDON path at the ceiling, putting the assertion on
    /// the boundary it is meant to be a verdict on. What it also refutes is the "microseconds"
    /// claim an earlier cut of the docs made.
    #[test]
    fn two_idle_targets_stop_within_a_poll_each() {
        let cfg =
            Alerting { webhooks: vec!["telegram".into(), "webhook".into()], ..Default::default() };
        let targets = vec![
            WebhookConfig {
                name: "telegram".into(),
                kind: WebhookKind::Telegram { token: "t".into(), chat_id: "c".into() },
            },
            WebhookConfig {
                name: "webhook".into(),
                kind: WebhookKind::Generic { url: "https://example.invalid/never-called".into() },
            },
        ];
        let mut alerts = RecorderAlerts::mount(&cfg, targets);
        let stops = alerts.take_delivery_stops();
        assert_eq!(stops.len(), 2);

        let began = Instant::now();
        stop_delivery(stops, Duration::from_secs(10));
        let took = began.elapsed();
        assert!(
            took < vike_alerting::STOP_POLL * 20,
            "two idle workers took {took:?} to stop — the doc says up to one poll each, and the \
             ceiling is ten times that"
        );
    }
}
