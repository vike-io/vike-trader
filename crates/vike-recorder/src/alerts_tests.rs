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
    let six: Vec<Silent> = (0..6).map(|i| stopped(&format!("book/polymarket/tok{i}"))).collect();

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
    let (queued, stop) = QueuedSink::spawn("slow", DEFAULT_QUEUE_CAPACITY, Box::new(SlowTarget));
    let mut alerts =
        RecorderAlerts::mount(&Alerting::default(), Vec::new()).with_sink(Box::new(queued));
    let mut watch = SilenceWatch::new();
    let six: Vec<Silent> = (0..6).map(|i| stopped(&format!("book/polymarket/tok{i}"))).collect();

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
