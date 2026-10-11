use std::collections::HashMap;

use vike_data::Liveness;

use super::{SilenceWatch, Silent, silent_series};

pub(super) fn live(pairs: &[(&str, u64, i64)]) -> HashMap<String, Liveness> {
    pairs
        .iter()
        .map(|(k, rows, last_ms)| (k.to_string(), Liveness { rows: *rows, last_ms: *last_ms }))
        .collect()
}
pub(super) fn expect(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn silent(names: &[&str]) -> Vec<Silent> {
    names
        .iter()
        .map(|n| Silent { series: n.to_string(), silent_for_ms: Some(600_000), rows: 7 })
        .collect()
}

#[test]
fn a_flowing_series_is_not_reported() {
    let got = silent_series(
        &expect(&["trade/binance/BTC"]),
        &live(&[("trade/binance/BTC", 9, 990)]),
        1_000,
        60_000,
    );
    assert!(got.is_empty(), "{got:?}");
}

/// **The 95-minute case.** It received rows, then the venue stream went quiet — the loss
/// counters stay zero and nothing errors, so this is the only place it shows.
#[test]
fn a_series_that_stopped_is_reported_with_its_age() {
    let got = silent_series(
        &expect(&["trade/binance/BTC"]),
        &live(&[("trade/binance/BTC", 6_000, 1_000)]),
        1_000 + 95 * 60_000,
        60_000,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].silent_for_ms, Some(95 * 60_000));
    assert_eq!(got[0].rows, 6_000, "it DID receive rows once — that is the diagnosis");
}

/// **The worse case**, and the one the handle alone can never report: a series with no entry at
/// all. `RecorderHandle::liveness` only knows series that received something, so "never
/// started" is only visible by diffing against what was subscribed.
#[test]
fn a_series_that_never_started_is_reported_distinctly() {
    let got = silent_series(&expect(&["depth/binance/BTC"]), &live(&[]), 5_000, 60_000);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].silent_for_ms, None, "never-started is not 'stale for 5s'");
    assert_eq!(got[0].rows, 0);
}

/// A series still inside the threshold is quiet, not silent — a 5-minute-window family can go
/// a minute between rows without being a fault.
#[test]
fn a_pause_shorter_than_the_threshold_is_not_a_fault() {
    let got =
        silent_series(&expect(&["trade/poly/T"]), &live(&[("trade/poly/T", 3, 0)]), 59_000, 60_000);
    assert!(got.is_empty());
}

/// A series the recorder is no longer subscribed to is IGNORED — a rotated-out Polymarket
/// token stops receiving rows by design, and reporting it would bury the real faults.
#[test]
fn an_unsubscribed_series_is_not_reported() {
    let got = silent_series(
        &expect(&["trade/poly/NEW"]),
        &live(&[("trade/poly/OLD", 500, 0), ("trade/poly/NEW", 1, 9_000)]),
        10_000,
        60_000,
    );
    assert!(got.is_empty(), "only OLD is stale, and OLD is no longer expected: {got:?}");
}

#[test]
fn output_is_sorted_so_a_log_line_is_stable() {
    let got = silent_series(&expect(&["b/v/s", "a/v/s", "c/v/s"]), &live(&[]), 0, 0);
    let names: Vec<&str> = got.iter().map(|s| s.series.as_str()).collect();
    assert_eq!(names, vec!["a/v/s", "b/v/s", "c/v/s"]);
}

/// **The false positive that would make this unreadable.** A just-subscribed series has
/// received nothing BY DEFINITION; reporting it immediately fires on every startup and every
/// Polymarket token rotation.
#[test]
fn a_just_subscribed_series_is_not_reported_yet() {
    let mut w = SilenceWatch::new();
    let e = expect(&["trade/binance/BTC"]);
    assert!(w.check(&e, &live(&[]), 0, 60_000).is_empty(), "t=0");
    assert!(w.check(&e, &live(&[]), 59_000, 60_000).is_empty(), "still inside the grace");
}

/// …but once it HAS had its grace and still never received a row, it is a real fault — a wrong
/// stream name the venue accepted anyway.
#[test]
fn a_series_that_never_starts_is_reported_after_the_grace() {
    let mut w = SilenceWatch::new();
    let e = expect(&["trade/binance/BTC"]);
    let _ = w.check(&e, &live(&[]), 0, 60_000);
    let got = w.check(&e, &live(&[]), 61_000, 60_000);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].silent_for_ms, None);
}

/// The grace applies ONLY to never-started series. One that received rows and then stopped is
/// judged on its own age, so a feed dying right after startup is still caught immediately.
#[test]
fn the_grace_does_not_delay_a_series_that_stopped() {
    let mut w = SilenceWatch::new();
    let e = expect(&["trade/binance/BTC"]);
    let got = w.check(&e, &live(&[("trade/binance/BTC", 10, 0)]), 61_000, 60_000);
    assert_eq!(got.len(), 1, "first tick ever, and it is already reported: {got:?}");
    assert_eq!(got[0].silent_for_ms, Some(61_000));
}

/// A rotated-out token is forgotten, so the map cannot grow without bound on a daemon that
/// rotates every 5 minutes forever.
#[test]
fn departed_series_are_forgotten() {
    let mut w = SilenceWatch::new();
    let _ = w.check(&expect(&["trade/poly/OLD"]), &live(&[]), 0, 60_000);
    let _ = w.check(&expect(&["trade/poly/NEW"]), &live(&[]), 1_000, 60_000);
    assert_eq!(w.first_seen.len(), 1);
    assert!(w.first_seen.contains_key("trade/poly/NEW"));
}

/// A re-subscribed token gets a FRESH grace rather than inheriting the old one — it is a new
/// subscription to a new stream, and judging it by when its predecessor appeared would report
/// it instantly.
#[test]
fn a_returning_series_gets_a_fresh_grace() {
    let mut w = SilenceWatch::new();
    let a = expect(&["trade/poly/A"]);
    let _ = w.check(&a, &live(&[]), 0, 60_000);
    let _ = w.check(&expect(&["trade/poly/B"]), &live(&[]), 10_000, 60_000);
    assert!(w.check(&a, &live(&[]), 70_000, 60_000).is_empty(), "re-subscribed at t=70_000");
}

// ---- The PER-SERIES notification gate: how often a silent series may PAGE, as opposed to how
// often it is logged (every tick, unchanged). -----------------------------------------------------

/// **The reason this gate is not `AlertRule::cooldown_ms`.** the CI box's watchdog fired for SIX
/// series in one episode; a per-RULE cooldown would page for one of them and swallow five,
/// which reads to an operator as a single-series fault.
#[test]
fn every_series_of_one_episode_alerts_not_just_the_first() {
    let mut w = SilenceWatch::new();
    let got = w.alertable(&silent(&["a/v/s", "b/v/s", "c/v/s"]), 1_000, 3_600_000);
    assert_eq!(got.len(), 3, "all three, on one tick, under one rule: {got:?}");
}

/// A still-silent series does not re-page every tick — the watchdog runs every 30s and an
/// outage lasts hours.
#[test]
fn a_still_silent_series_does_not_repage_within_the_repeat_window() {
    let mut w = SilenceWatch::new();
    let s = silent(&["a/v/s"]);
    assert_eq!(w.alertable(&s, 0, 3_600_000).len(), 1, "first tick pages");
    assert!(w.alertable(&s, 30_000, 3_600_000).is_empty(), "30s later: suppressed");
    assert!(w.alertable(&s, 3_599_999, 3_600_000).is_empty(), "just inside the window");
    assert_eq!(w.alertable(&s, 3_600_000, 3_600_000).len(), 1, "the window elapsed → re-pages");
}

/// `repeat_ms = 0` means ONCE per episode: never re-page while it stays silent.
#[test]
fn a_zero_repeat_pages_once_per_episode_and_never_again_while_it_lasts() {
    let mut w = SilenceWatch::new();
    let s = silent(&["a/v/s"]);
    assert_eq!(w.alertable(&s, 0, 0).len(), 1);
    assert!(w.alertable(&s, 86_400_000, 0).is_empty(), "a day later, still one episode");
}

/// …but a NEW episode pages immediately, however the last one ended. Without the recovery
/// sweep, a series that came back and died again would be silently suppressed for a whole
/// repeat window — the failure this whole file exists to make impossible.
#[test]
fn recovery_rearms_so_the_next_episode_pages_at_once() {
    let mut w = SilenceWatch::new();
    let s = silent(&["a/v/s"]);
    assert_eq!(w.alertable(&s, 0, 3_600_000).len(), 1);
    // it recovered: this tick's silent set no longer names it.
    assert!(w.alertable(&[], 1_000, 3_600_000).is_empty());
    // …and it dies again, well inside the repeat window.
    assert_eq!(w.alertable(&s, 2_000, 3_600_000).len(), 1, "a fresh episode is a fresh page");
}

/// A rotated-out Polymarket token leaves the silent set forever; its bookkeeping must go with
/// it or the map grows without bound on a daemon that rotates every 5 minutes.
#[test]
fn departed_series_do_not_accumulate() {
    let mut w = SilenceWatch::new();
    let _ = w.alertable(&silent(&["trade/poly/OLD"]), 0, 0);
    let _ = w.alertable(&silent(&["trade/poly/NEW"]), 1_000, 0);
    assert_eq!(w.last_alerted.len(), 1);
    assert!(w.last_alerted.contains_key("trade/poly/NEW"));
}
