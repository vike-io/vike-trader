use super::liveness_tests::{expect, live};
use super::*;

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
