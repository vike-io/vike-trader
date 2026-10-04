//! **A series goes silent and an ALERT is produced.** The deliverable, driven through the real
//! production tick (`vike_recorder::alerts::watchdog_tick`) over the crate's PUBLIC surface — the
//! same call the daemon binary makes, with the same arguments.
//!
//! ## What this exists to stop happening again
//!
//! The watchdog computed exactly the right verdict and then threw it away: two `tracing::warn!`s
//! and a dropped `Vec<Silent>`. On the CI box it fired **20 times in 24 hours** across 6 series into a
//! log nothing was reading, and on 2026-08-05 a Polymarket `book` family collapsed to **782 rows
//! over eight minutes against ~100,000/min, five of those minutes at exactly zero** — with nothing
//! said. So the assertion that matters here is not "the condition was detected" (the unit tests in
//! `liveness` already pin that): it is that a `FiredAlert` reached a SINK.
//!
//! ⚠ **This paragraph read "from ~250–370k rows/min to 18/min for 19 minutes — ~3.8M rows lost"
//! and that figure is a MISREADING**, corrected in `crates/vike-recorder/src/alerts.rs`'s module
//! doc, which carries the whole argument: it is the `18388` cell of
//! `docs/ops/onfailure-notify.md`'s replay table read as a per-minute number when the row is a
//! FIVE-MINUTE bucket, and it describes the latency box's separate Polymarket L2 recorder rather than this
//! store. The nineteen-minutes-of-SILENCE shape this file's own fixture models was never true on
//! either tape — it is a fine fixture for the RECENCY rule, which is what this file tests, and it
//! is not evidence about the family rule.
//!
//! The two inputs are the two the binary passes: `expected` is
//! `RecorderRuntime::expected_series()` and `live` is `RecorderHandle::liveness()`. Both are plain
//! data, so this test needs no store, no venue, no network and no clock — `now_ms` is a parameter
//! all the way down.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vike_alerting::{AlertSink, FiredAlert};
use vike_data::Liveness;
use vike_recorder::Alerting;
use vike_recorder::alerts::{RecorderAlerts, watchdog_tick};
use vike_recorder::liveness::SilenceWatch;

/// A sink that records what was delivered. Deliberately blind to `AlertTargets` (unlike
/// `vike_alerting::InProcessSink`): the recorder's rule sets `in_process: false` because a headless
/// daemon has no toast surface, so an `InProcessSink` would legitimately see nothing here and would
/// prove nothing about the wiring.
#[derive(Clone, Default)]
struct Pager(Arc<Mutex<Vec<FiredAlert>>>);

impl AlertSink for Pager {
    fn deliver(&self, alert: &FiredAlert) {
        self.0.lock().unwrap().push(alert.clone());
    }
}

impl Pager {
    fn delivered(&self) -> Vec<FiredAlert> {
        self.0.lock().unwrap().clone()
    }
}

fn mounted(cfg: &Alerting) -> (RecorderAlerts, Pager) {
    let pager = Pager::default();
    (RecorderAlerts::mount(cfg, Vec::new()).with_sink(Box::new(pager.clone())), pager)
}

fn expect(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn live(pairs: &[(&str, u64, i64)]) -> HashMap<String, Liveness> {
    pairs
        .iter()
        .map(|(k, rows, last_ms)| (k.to_string(), Liveness { rows: *rows, last_ms: *last_ms }))
        .collect()
}

/// The five-minute default, in ms — what `--silent-secs`' default resolves to.
const SILENT_SECS: u64 = 300;

/// The FAMILY rule's input, EMPTY on purpose. Every test in this file is about RECENCY, and an
/// empty slice judges no family at all — see `vike_recorder::alerts::watchdog_tick`. Passing it
/// explicitly rather than hiding it behind a wrapper keeps the production signature visible at
/// every call, which is the property that made this file catch the delivery gap in the first place.
const NO_FAMILIES: &[(String, String)] = &[];

/// **The deliverable.** A series that was writing rows stops; one tick later an alert naming it has
/// been delivered.
///
/// Timeline: the series' last row lands at t=1s, the tick runs at t=1s+19min. The silence is nine
/// minutes past the five-minute threshold, so this is the 2026-08-05 shape exactly.
#[test]
fn a_series_that_stops_receiving_rows_delivers_an_alert_naming_it() {
    let (mut alerts, pager) = mounted(&Alerting::default());
    let mut watch = SilenceWatch::new();
    let series = "book/polymarket/0xtok";

    // Healthy first: rows arriving, nothing said.
    let quiet = watchdog_tick(
        &mut watch,
        &mut alerts,
        &expect(&[series]),
        NO_FAMILIES,
        &live(&[(series, 3_800_000, 1_000)]),
        1_000,
        SILENT_SECS,
    );
    assert!(quiet.is_empty(), "a flowing series is not silent: {quiet:?}");
    assert!(pager.delivered().is_empty(), "…and nothing was paged");

    // …then the venue stops sending. No error is raised anywhere; the row count simply stops.
    let silent = watchdog_tick(
        &mut watch,
        &mut alerts,
        &expect(&[series]),
        NO_FAMILIES,
        &live(&[(series, 3_800_000, 1_000)]),
        1_000 + 19 * 60_000,
        SILENT_SECS,
    );
    assert_eq!(silent, vec![series.to_string()], "the tick reports which series went silent");

    let delivered = pager.delivered();
    assert_eq!(delivered.len(), 1, "the silent series produced exactly one alert");
    assert_eq!(delivered[0].rule_id, "recorder-series-stale");
    assert!(
        delivered[0].body.contains(series),
        "the alert must NAME the series — that is the whole diagnosis: {}",
        delivered[0].body
    );
    assert!(
        delivered[0].body.contains("1140s"),
        "…and how long it has been silent: {}",
        delivered[0].body
    );
}

/// The worse case, and the one `RecorderHandle::liveness` alone can never report: a subscription
/// the venue accepted and never served. It has no liveness entry at all, so it is only visible
/// against what was subscribed — and it must reach the pager once its grace has elapsed.
#[test]
fn a_series_that_never_starts_alerts_after_its_grace() {
    let (mut alerts, pager) = mounted(&Alerting::default());
    let mut watch = SilenceWatch::new();
    let series = "depth/binance/BTCUSDT";

    // Inside the grace: a just-subscribed series has received nothing BY DEFINITION.
    assert!(
        watchdog_tick(
            &mut watch,
            &mut alerts,
            &expect(&[series]),
            NO_FAMILIES,
            &live(&[]),
            0,
            SILENT_SECS
        )
        .is_empty()
    );
    assert!(pager.delivered().is_empty(), "no false positive on startup");

    // Past it: the venue accepted a stream name it never serves.
    let silent = watchdog_tick(
        &mut watch,
        &mut alerts,
        &expect(&[series]),
        NO_FAMILIES,
        &live(&[]),
        301_000,
        SILENT_SECS,
    );
    assert_eq!(silent, vec![series.to_string()]);

    let delivered = pager.delivered();
    assert_eq!(delivered.len(), 1);
    assert!(delivered[0].body.contains(series), "{}", delivered[0].body);
    assert!(
        delivered[0].body.contains("NEVER received a row"),
        "a never-started series is a different fault from a stopped one: {}",
        delivered[0].body
    );
}

/// the CI box's episode had SIX silent series at once. All six must page: one rule covers them all (a
/// recorder's series set rotates, so per-series rules are unwritable), and a per-RULE cooldown
/// would have delivered one and swallowed five — which reads as a single-series fault.
#[test]
fn every_silent_series_of_one_episode_reaches_the_pager() {
    let (mut alerts, pager) = mounted(&Alerting::default());
    let mut watch = SilenceWatch::new();
    let names: Vec<String> = (0..6).map(|i| format!("book/polymarket/tok{i}")).collect();
    let rows: Vec<(&str, u64, i64)> =
        names.iter().map(|n| (n.as_str(), 500_u64, 1_000_i64)).collect();

    let silent = watchdog_tick(
        &mut watch,
        &mut alerts,
        &names,
        NO_FAMILIES,
        &live(&rows),
        1_000 + 600_000,
        SILENT_SECS,
    );
    assert_eq!(silent.len(), 6);
    assert_eq!(pager.delivered().len(), 6, "all six, not just the first");
}

/// A rotated-out Polymarket token stops receiving rows BY DESIGN — every 5 minutes, forever. Paging
/// for that would bury the real faults inside an hour, which is the same outcome as not paging.
#[test]
fn a_rotated_out_series_never_pages() {
    let (mut alerts, pager) = mounted(&Alerting::default());
    let mut watch = SilenceWatch::new();

    let silent = watchdog_tick(
        &mut watch,
        &mut alerts,
        &expect(&["book/polymarket/NEW"]),
        NO_FAMILIES,
        // OLD is long dead but is no longer expected; NEW is fresh and flowing.
        &live(&[("book/polymarket/OLD", 900_000, 0), ("book/polymarket/NEW", 12, 9_000)]),
        10_000,
        SILENT_SECS,
    );
    assert!(silent.is_empty(), "{silent:?}");
    assert!(pager.delivered().is_empty());
}

/// `--silent-secs 0` disables the watchdog outright: no judgement, no log, no alert. An operator
/// who turns it off must get silence, not a quieter alarm.
#[test]
fn a_disabled_watchdog_delivers_nothing_at_all() {
    let (mut alerts, pager) = mounted(&Alerting::default());
    let mut watch = SilenceWatch::new();
    let silent = watchdog_tick(
        &mut watch,
        &mut alerts,
        &expect(&["a/v/s"]),
        NO_FAMILIES,
        &live(&[("a/v/s", 5, 0)]),
        86_400_000,
        0,
    );
    assert!(silent.is_empty());
    assert!(pager.delivered().is_empty());
}

/// The profile's prefix scope reaches the delivered alert — proving the `[alerting]` table is wired
/// through the mount into the rule, and that the scope is applied by `vike-alerting`'s evaluator
/// rather than re-implemented on this side.
#[test]
fn the_profile_series_prefix_scopes_what_pages() {
    let cfg = Alerting { series_prefix: Some("book/polymarket/".into()), ..Default::default() };
    let (mut alerts, pager) = mounted(&cfg);
    let mut watch = SilenceWatch::new();

    let silent = watchdog_tick(
        &mut watch,
        &mut alerts,
        &expect(&["book/polymarket/0xtok", "trade/binance/BTCUSDT"]),
        NO_FAMILIES,
        &live(&[("book/polymarket/0xtok", 9, 0), ("trade/binance/BTCUSDT", 9, 0)]),
        600_000,
        SILENT_SECS,
    );
    assert_eq!(silent.len(), 2, "BOTH are silent, and both are still logged/reported");

    let delivered = pager.delivered();
    assert_eq!(delivered.len(), 1, "…but only the in-scope one pages: {delivered:?}");
    assert!(delivered[0].body.contains("polymarket"), "{}", delivered[0].body);
}

/// The pager is rate-limited per series across ticks — the daemon ticks every 30s and an outage
/// lasts hours — while the returned silent set (what `--exit-on-silence` acts on) is NOT.
#[test]
fn a_persisting_outage_pages_once_but_keeps_reporting_every_tick() {
    let (mut alerts, pager) = mounted(&Alerting::default()); // repeat_secs = 3600
    let mut watch = SilenceWatch::new();
    let series = "a/v/s";
    let rows = live(&[(series, 5, 0)]);

    for tick in 0..10 {
        let now = 600_000 + tick * 30_000;
        let silent = watchdog_tick(
            &mut watch,
            &mut alerts,
            &expect(&[series]),
            NO_FAMILIES,
            &rows,
            now,
            SILENT_SECS,
        );
        assert_eq!(silent.len(), 1, "still silent at tick {tick} — always reported");
    }
    assert_eq!(pager.delivered().len(), 1, "…but paged ONCE inside the repeat window");
}
