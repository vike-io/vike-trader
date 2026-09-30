use super::*;

fn silent(names: &[&str]) -> Vec<Silent> {
    names
        .iter()
        .map(|n| Silent { series: n.to_string(), silent_for_ms: Some(600_000), rows: 7 })
        .collect()
}

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
