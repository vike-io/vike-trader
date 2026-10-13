use super::*;

fn cfg(grace_ms: u64, venues: &[&str]) -> LinkDeadManConfig {
    LinkDeadManConfig {
        grace: Duration::from_millis(grace_ms),
        action: DeadManAction::CancelAllAndHalt,
        venues: venues.iter().map(|v| (*v).to_string()).collect(),
        halt_file: None,
    }
}

fn latch(grace_ms: u64, venues: &[&str]) -> LinkDeadMan {
    LinkDeadMan::new(&cfg(grace_ms, venues))
}

/// (a) The core case: a disclosed disconnect, a grace that expires, ONE trip.
#[test]
fn a_disconnect_outlasting_the_grace_trips_once() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe("binance", "BTCUSDT", FeedStatus::Live, 0);
    dm.observe("binance", "BTCUSDT", FeedStatus::Disconnected, 100);

    assert!(dm.check(1100).is_empty(), "lag == grace is still inside it (strict >)");
    assert_eq!(
        dm.check(1101),
        vec![("binance".to_string(), "BTCUSDT".to_string())],
        "one ms past the grace ⇒ trip"
    );
    assert!(dm.check(9999).is_empty(), "no re-fire within the same outage");
    assert!(dm.is_tripped("binance", "BTCUSDT"));
}

/// (b) The property the grace exists for: an ORDINARY reconnect is silent. A `Live` inside the
/// window closes it, and no later sweep can trip on that outage.
#[test]
fn a_reconnect_inside_the_grace_is_silent() {
    let mut dm = latch(1000, &["bybit"]);
    dm.observe("bybit", "BTCUSDT", FeedStatus::Live, 0);
    dm.observe("bybit", "BTCUSDT", FeedStatus::Disconnected, 100);
    assert!(dm.check(500).is_empty());
    // the venue's backoff + re-dial lands well inside the window
    assert_eq!(dm.observe("bybit", "BTCUSDT", FeedStatus::Live, 700), LinkObservation::Quiet);
    assert!(dm.check(10_000).is_empty(), "a closed window cannot trip later");
}

/// (c) ⚠ **The rule the whole switch turns on.** `Stale` moves nothing — it neither opens a
/// window nor closes one — because a weekend on an FX venue is disclosed as exactly this.
#[test]
fn stale_never_trips_and_never_clears() {
    let mut dm = latch(1000, &["oanda"]);
    dm.observe("oanda", "EUR_USD", FeedStatus::Live, 0);
    for t in [100, 5_000, 100_000, 172_800_000] {
        assert_eq!(dm.observe("oanda", "EUR_USD", FeedStatus::Stale, t), LinkObservation::Quiet);
        assert!(dm.check(t).is_empty(), "a 48h weekend of Stale must not trip at t={t}");
    }
    // ...and it did not silently CLOSE an outage either: a real disconnect after all that
    // Stale still opens a window and still trips.
    dm.observe("oanda", "EUR_USD", FeedStatus::Disconnected, 172_800_000);
    assert!(dm.check(172_800_500).is_empty());
    assert_eq!(dm.check(172_801_001).len(), 1, "a REAL disconnect still trips after Stale");
}

/// (d) ⚠ **The FIRST link death of a mount trips, with no `Live` ever observed** — the
/// headline outage, and the one a prior-`Live` gate silently excluded. No bridge on this
/// roster discloses `Live` on a first successful connect (`StreamHealth::recover` is a no-op
/// with no gap open), so under that gate a socket that came up, died and never returned
/// produced no window at all. The module doc carries the evidence.
#[test]
fn a_first_link_death_trips_without_any_prior_live() {
    let mut dm = latch(1000, &["okx"]);
    dm.observe("okx", "BTC-USDT-SWAP", FeedStatus::Disconnected, 0);
    assert!(dm.check(500).is_empty(), "still inside the grace");
    assert_eq!(
        dm.check(1001),
        vec![("okx".to_string(), "BTC-USDT-SWAP".to_string())],
        "a link disclosed down and never disclosed up is exactly what this switch is for"
    );
}

/// (d2) ...and the startup dial-up the retired gate was reaching for is absorbed by the GRACE
/// instead: a venue that fails its first connects and then comes up inside the window cancels
/// nothing. This is the property that makes (d) safe rather than trigger-happy.
#[test]
fn a_startup_dial_that_succeeds_inside_the_grace_is_silent() {
    let mut dm = latch(30_000, &["binance"]);
    // a failed first dial is disclosed as a gap by the depth driver's `enter_gap`
    dm.observe("binance", "BTCUSDT", FeedStatus::Disconnected, 0);
    for t in [3_000, 6_000, 9_000] {
        dm.observe("binance", "BTCUSDT", FeedStatus::Disconnected, t); // backoff re-dials
        assert!(dm.check(t).is_empty());
    }
    dm.observe("binance", "BTCUSDT", FeedStatus::Live, 23_000); // one full ordinary cycle
    assert!(dm.check(1_000_000).is_empty(), "the link came up inside the grace");
}

/// (d3) A `Live` on a link that was never down allocates nothing — the map holds an entry per
/// link that has actually been disclosed DOWN, not one per subscription.
#[test]
fn a_live_on_a_healthy_link_costs_no_state() {
    let mut dm = latch(1000, &["binance"]);
    for s in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
        assert_eq!(dm.observe("binance", s, FeedStatus::Live, 0), LinkObservation::Quiet);
    }
    assert!(dm.links.is_empty(), "a link that never dropped is not tracked");
}

/// (e) Recovery re-arms, and says so exactly once: the `Live` that clears a TRIPPED outage
/// reports `Recovered`, a second `Live` reports nothing, and a NEW outage trips again.
#[test]
fn recovery_reports_once_and_re_arms() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe("binance", "BTCUSDT", FeedStatus::Live, 0);
    dm.observe("binance", "BTCUSDT", FeedStatus::Disconnected, 0);
    assert_eq!(dm.check(2000).len(), 1, "first outage trips");

    assert_eq!(
        dm.observe("binance", "BTCUSDT", FeedStatus::Live, 3000),
        LinkObservation::Recovered
    );
    assert_eq!(
        dm.observe("binance", "BTCUSDT", FeedStatus::Live, 3100),
        LinkObservation::Quiet,
        "a second Live is not a second recovery"
    );
    assert!(!dm.is_tripped("binance", "BTCUSDT"));

    dm.observe("binance", "BTCUSDT", FeedStatus::Disconnected, 4000);
    assert_eq!(dm.check(5001).len(), 1, "a second outage trips again after the re-arm");
}

/// (f) A venue outside the armed set is invisible — no state, no trip. This is what makes an FX
/// venue mounted beside an armed crypto one free rather than merely harmless.
#[test]
fn an_unarmed_venue_is_never_tracked() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe("ig", "IX.D.FTSE.DAILY.IP", FeedStatus::Live, 0);
    dm.observe("ig", "IX.D.FTSE.DAILY.IP", FeedStatus::Disconnected, 0);
    assert!(dm.check(10_000_000).is_empty(), "an unarmed venue can never trip");
    assert!(dm.links.is_empty(), "...and costs no state at all");
}

/// (g) A re-disclosed gap does NOT push the deadline out. A bridge that emits a second
/// `Disconnected` mid-outage (a per-token feed reconnecting one shard at a time) must not be
/// able to hold the switch open forever.
#[test]
fn a_second_disconnect_does_not_extend_the_window() {
    let mut dm = latch(1000, &["polymarket"]);
    dm.observe("polymarket", "TOK", FeedStatus::Live, 0);
    dm.observe("polymarket", "TOK", FeedStatus::Disconnected, 100);
    for t in [200, 400, 900, 1_050] {
        dm.observe("polymarket", "TOK", FeedStatus::Disconnected, t);
    }
    assert_eq!(dm.check(1101).len(), 1, "the window still expires 1000 ms after the FIRST gap");
}

/// (h) Two links on one armed venue trip independently, and one venue's outage leaves another
/// venue's link untouched — the per-key scoping the runtime's venue-scoped cancel rests on.
#[test]
fn links_are_tracked_per_venue_and_symbol() {
    let mut dm = latch(1000, &["binance", "bybit"]);
    for (v, s) in [("binance", "BTCUSDT"), ("binance", "ETHUSDT"), ("bybit", "BTCUSDT")] {
        dm.observe(v, s, FeedStatus::Live, 0);
    }
    dm.observe("binance", "BTCUSDT", FeedStatus::Disconnected, 0);
    assert_eq!(
        dm.check(2000),
        vec![("binance".to_string(), "BTCUSDT".to_string())],
        "only the link that died trips"
    );
    assert!(!dm.is_tripped("binance", "ETHUSDT"));
    assert!(!dm.is_tripped("bybit", "BTCUSDT"));
}
