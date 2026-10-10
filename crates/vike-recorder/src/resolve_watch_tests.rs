use super::*;

fn never(venue: &str) -> FeedResolve {
    FeedResolve { venue: venue.into(), last_nonempty_ms: None }
}
fn worked_at(venue: &str, ms: i64) -> FeedResolve {
    FeedResolve { venue: venue.into(), last_nonempty_ms: Some(ms) }
}

/// **The false positive that would make this unusable.** A feed is unresolved BY DEFINITION on
/// the tick it is mounted; reporting it at once fires on every startup of every box.
#[test]
fn a_just_mounted_venue_is_not_reported_yet() {
    let mut w = ResolveWatch::new();
    assert!(w.check(&[never("polymarket")], 0, 300_000).is_empty(), "t=0");
    assert!(w.check(&[never("polymarket")], 299_000, 300_000).is_empty(), "inside the grace");
}

/// **The measured failure.** Past the grace and it has still never produced a symbol: a proxy
/// nothing is listening behind, a family name that matches no market. It will not fix itself.
#[test]
fn a_venue_that_never_resolves_is_reported_after_the_grace() {
    let mut w = ResolveWatch::new();
    let _ = w.check(&[never("polymarket")], 0, 300_000);
    assert_eq!(w.check(&[never("polymarket")], 301_000, 300_000), vec!["polymarket"]);
}

/// ⚠ **The retry case, which must NEVER escalate.** `crate::runtime`'s module doc is why: a
/// resolution failure changes nothing and is retried next tick, and paging for a Gamma blip is
/// how a pager gets muted.
#[test]
fn a_venue_that_resolved_once_is_never_reported_however_long_it_has_been_failing() {
    let mut w = ResolveWatch::new();
    let _ = w.check(&[worked_at("polymarket", 1_000)], 1_000, 300_000);
    assert!(
        w.check(&[worked_at("polymarket", 1_000)], 86_400_000, 300_000).is_empty(),
        "a day of failing resolves, after ONE success, is still a retry — not a page"
    );
}

/// A three-venue box with one dead venue must name exactly that one, and name it.
#[test]
fn only_the_never_resolved_venues_are_reported_and_the_output_is_sorted() {
    let mut w = ResolveWatch::new();
    let feeds =
        [never("polymarket"), worked_at("binance", 500), never("aster"), worked_at("okx", 500)];
    let _ = w.check(&feeds, 0, 300_000);
    assert_eq!(w.check(&feeds, 301_000, 300_000), vec!["aster", "polymarket"]);
}

/// A venue dropped from the profile is forgotten, so a long-running daemon's map cannot grow
/// without bound — and a re-added venue gets a FRESH grace rather than inheriting the old one.
#[test]
fn departed_venues_are_forgotten_and_a_returning_one_gets_a_fresh_grace() {
    let mut w = ResolveWatch::new();
    let _ = w.check(&[never("polymarket")], 0, 300_000);
    let _ = w.check(&[never("binance")], 1_000, 300_000);
    assert_eq!(w.first_seen.len(), 1);
    assert!(w.first_seen.contains_key("binance"));
    // polymarket comes back at t=1_000's successor: it must get its whole grace again.
    assert!(w.check(&[never("polymarket")], 2_000, 300_000).is_empty());
    assert!(w.check(&[never("polymarket")], 301_000, 300_000).is_empty(), "grace from t=2_000");
    assert_eq!(w.check(&[never("polymarket")], 303_000, 300_000), vec!["polymarket"]);
}

/// Every venue of one episode pages — the reason this gate is not `AlertRule::cooldown_ms`.
#[test]
fn every_unresolved_venue_of_one_episode_alerts_not_just_the_first() {
    let mut w = ResolveWatch::new();
    let all = ["aster".to_string(), "binance".to_string(), "polymarket".to_string()];
    assert_eq!(w.alertable(&all, 1_000, 3_600_000).len(), 3, "all three, on one tick");
}

/// A still-unresolved venue does not re-page every tick — the daemon ticks every 30 s.
#[test]
fn a_still_unresolved_venue_does_not_repage_within_the_repeat_window() {
    let mut w = ResolveWatch::new();
    let one = ["polymarket".to_string()];
    assert_eq!(w.alertable(&one, 0, 3_600_000).len(), 1, "first tick pages");
    assert!(w.alertable(&one, 30_000, 3_600_000).is_empty(), "30s later: suppressed");
    assert_eq!(w.alertable(&one, 3_600_000, 3_600_000).len(), 1, "the window elapsed");
}

/// …and a recovery re-arms, so the NEXT episode pages at once rather than waiting out a window.
#[test]
fn a_recovered_venue_rearms_and_a_fresh_episode_pages_immediately() {
    let mut w = ResolveWatch::new();
    let one = ["polymarket".to_string()];
    assert_eq!(w.alertable(&one, 0, 3_600_000).len(), 1);
    assert!(w.alertable(&[], 1_000, 3_600_000).is_empty(), "it resolved: nothing to page");
    assert_eq!(w.alertable(&one, 2_000, 3_600_000).len(), 1, "a fresh episode is a fresh page");
    assert_eq!(w.last_alerted.len(), 1, "…and the map does not accumulate");
}
