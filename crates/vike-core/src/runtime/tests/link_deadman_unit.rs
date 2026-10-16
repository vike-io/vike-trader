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

/// `check` reduced to the `(venue, symbol)` keys, in order, one per tripped WINDOW.
fn keys(dm: &mut LinkDeadMan, now_ms: i64) -> Vec<(String, String)> {
    dm.check(now_ms).into_iter().map(|t| (t.venue, t.symbol)).collect()
}

/// `check` reduced to `(venue, symbol, source)`.
fn trips(dm: &mut LinkDeadMan, now_ms: i64) -> Vec<(String, String, LinkSource)> {
    dm.check(now_ms).into_iter().map(|t| (t.venue, t.symbol, t.source)).collect()
}

/// (a) The core case: a disclosed disconnect, a grace that expires, ONE trip.
#[test]
fn a_disconnect_outlasting_the_grace_trips_once() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe("binance", "BTCUSDT", LinkSource::Venue, FeedStatus::Live, 0);
    dm.observe("binance", "BTCUSDT", LinkSource::Venue, FeedStatus::Disconnected, 100);

    assert!(keys(&mut dm, 1100).is_empty(), "lag == grace is still inside it (strict >)");
    assert_eq!(
        keys(&mut dm, 1101),
        vec![("binance".to_string(), "BTCUSDT".to_string())],
        "one ms past the grace ⇒ trip"
    );
    assert!(keys(&mut dm, 9999).is_empty(), "no re-fire within the same outage");
    assert!(dm.is_tripped("binance", "BTCUSDT"));
}

/// (b) The property the grace exists for: an ORDINARY reconnect is silent. A `Live` inside the
/// window closes it, and no later sweep can trip on that outage.
#[test]
fn a_reconnect_inside_the_grace_is_silent() {
    let mut dm = latch(1000, &["bybit"]);
    dm.observe("bybit", "BTCUSDT", LinkSource::Venue, FeedStatus::Live, 0);
    dm.observe("bybit", "BTCUSDT", LinkSource::Venue, FeedStatus::Disconnected, 100);
    assert!(keys(&mut dm, 500).is_empty());
    // the venue's backoff + re-dial lands well inside the window
    assert_eq!(
        dm.observe("bybit", "BTCUSDT", LinkSource::Venue, FeedStatus::Live, 700),
        LinkObservation::Quiet
    );
    assert!(keys(&mut dm, 10_000).is_empty(), "a closed window cannot trip later");
}

/// (c) ⚠ **The rule the whole switch turns on.** `Stale` moves nothing — it neither opens a
/// window nor closes one — because a weekend on an FX venue is disclosed as exactly this.
#[test]
fn stale_never_trips_and_never_clears() {
    let mut dm = latch(1000, &["oanda"]);
    dm.observe("oanda", "EUR_USD", LinkSource::Venue, FeedStatus::Live, 0);
    for t in [100, 5_000, 100_000, 172_800_000] {
        assert_eq!(
            dm.observe("oanda", "EUR_USD", LinkSource::Venue, FeedStatus::Stale, t),
            LinkObservation::Quiet
        );
        assert!(keys(&mut dm, t).is_empty(), "a 48h weekend of Stale must not trip at t={t}");
    }
    // ...and it did not silently CLOSE an outage either: a real disconnect after all that
    // Stale still opens a window and still trips.
    dm.observe("oanda", "EUR_USD", LinkSource::Venue, FeedStatus::Disconnected, 172_800_000);
    assert!(keys(&mut dm, 172_800_500).is_empty());
    assert_eq!(keys(&mut dm, 172_801_001).len(), 1, "a REAL disconnect still trips after Stale");
}

/// (d) ⚠ **The FIRST link death of a mount trips, with no `Live` ever observed** — the
/// headline outage, and the one a prior-`Live` gate silently excluded. No bridge on this
/// roster discloses `Live` on a first successful connect (`StreamHealth::recover` is a no-op
/// with no gap open), so under that gate a socket that came up, died and never returned
/// produced no window at all. The module doc carries the evidence.
#[test]
fn a_first_link_death_trips_without_any_prior_live() {
    let mut dm = latch(1000, &["okx"]);
    dm.observe("okx", "BTC-USDT-SWAP", LinkSource::Venue, FeedStatus::Disconnected, 0);
    assert!(keys(&mut dm, 500).is_empty(), "still inside the grace");
    assert_eq!(
        keys(&mut dm, 1001),
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
    dm.observe("binance", "BTCUSDT", LinkSource::Venue, FeedStatus::Disconnected, 0);
    for t in [3_000, 6_000, 9_000] {
        dm.observe("binance", "BTCUSDT", LinkSource::Venue, FeedStatus::Disconnected, t); // backoff re-dials
        assert!(keys(&mut dm, t).is_empty());
    }
    dm.observe("binance", "BTCUSDT", LinkSource::Venue, FeedStatus::Live, 23_000); // one full ordinary cycle
    assert!(keys(&mut dm, 1_000_000).is_empty(), "the link came up inside the grace");
}

/// (d3) A `Live` on a link that was never down allocates nothing — the map holds an entry per
/// link that has actually been disclosed DOWN, not one per subscription.
#[test]
fn a_live_on_a_healthy_link_costs_no_state() {
    let mut dm = latch(1000, &["binance"]);
    for s in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
        assert_eq!(
            dm.observe("binance", s, LinkSource::Venue, FeedStatus::Live, 0),
            LinkObservation::Quiet
        );
    }
    assert!(dm.links.is_empty(), "a link that never dropped is not tracked");
}

/// (e) Recovery re-arms, and says so exactly once: the `Live` that clears a TRIPPED outage
/// reports `Recovered`, a second `Live` reports nothing, and a NEW outage trips again.
#[test]
fn recovery_reports_once_and_re_arms() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe("binance", "BTCUSDT", LinkSource::Venue, FeedStatus::Live, 0);
    dm.observe("binance", "BTCUSDT", LinkSource::Venue, FeedStatus::Disconnected, 0);
    assert_eq!(keys(&mut dm, 2000).len(), 1, "first outage trips");

    assert_eq!(
        dm.observe("binance", "BTCUSDT", LinkSource::Venue, FeedStatus::Live, 3000),
        LinkObservation::Recovered
    );
    assert_eq!(
        dm.observe("binance", "BTCUSDT", LinkSource::Venue, FeedStatus::Live, 3100),
        LinkObservation::Quiet,
        "a second Live is not a second recovery"
    );
    assert!(!dm.is_tripped("binance", "BTCUSDT"));

    dm.observe("binance", "BTCUSDT", LinkSource::Venue, FeedStatus::Disconnected, 4000);
    assert_eq!(keys(&mut dm, 5001).len(), 1, "a second outage trips again after the re-arm");
}

/// (f) A venue outside the armed set is invisible — no state, no trip. This is what makes an FX
/// venue mounted beside an armed crypto one free rather than merely harmless.
#[test]
fn an_unarmed_venue_is_never_tracked() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe("ig", "IX.D.FTSE.DAILY.IP", LinkSource::Venue, FeedStatus::Live, 0);
    dm.observe("ig", "IX.D.FTSE.DAILY.IP", LinkSource::Venue, FeedStatus::Disconnected, 0);
    assert!(keys(&mut dm, 10_000_000).is_empty(), "an unarmed venue can never trip");
    assert!(dm.links.is_empty(), "...and costs no state at all");
}

/// (g) A re-disclosed gap does NOT push the deadline out. A bridge that emits a second
/// `Disconnected` mid-outage (a per-token feed reconnecting one shard at a time) must not be
/// able to hold the switch open forever.
#[test]
fn a_second_disconnect_does_not_extend_the_window() {
    let mut dm = latch(1000, &["polymarket"]);
    dm.observe("polymarket", "TOK", LinkSource::Venue, FeedStatus::Live, 0);
    dm.observe("polymarket", "TOK", LinkSource::Venue, FeedStatus::Disconnected, 100);
    for t in [200, 400, 900, 1_050] {
        dm.observe("polymarket", "TOK", LinkSource::Venue, FeedStatus::Disconnected, t);
    }
    assert_eq!(
        keys(&mut dm, 1101).len(),
        1,
        "the window still expires 1000 ms after the FIRST gap"
    );
}

/// (h) Two links on one armed venue trip independently, and one venue's outage leaves another
/// venue's link untouched — the per-key scoping the runtime's venue-scoped cancel rests on.
#[test]
fn links_are_tracked_per_venue_and_symbol() {
    let mut dm = latch(1000, &["binance", "bybit"]);
    for (v, s) in [("binance", "BTCUSDT"), ("binance", "ETHUSDT"), ("bybit", "BTCUSDT")] {
        dm.observe(v, s, LinkSource::Venue, FeedStatus::Live, 0);
    }
    dm.observe("binance", "BTCUSDT", LinkSource::Venue, FeedStatus::Disconnected, 0);
    assert_eq!(
        keys(&mut dm, 2000),
        vec![("binance".to_string(), "BTCUSDT".to_string())],
        "only the link that died trips"
    );
    assert!(!dm.is_tripped("binance", "ETHUSDT"));
    assert!(!dm.is_tripped("bybit", "BTCUSDT"));
}

// ───────────────────────────── one window per SOURCE (the trade link and the strat link) ──────────

use vike_venues::link_deadman::{STRAT_LINK_STREAM, TRADE_LINK_STREAM};

const KEY: (&str, &str) = ("binance", "BTCUSDT");

fn key() -> (String, String) {
    (KEY.0.to_string(), KEY.1.to_string())
}

/// (s1) Sources do not erase each other, in BOTH directions. A strat-link `Live` after a VENUE
/// `Disconnected` leaves the venue window open and it still trips; a trade-link `Live` leaves the
/// strat window open and that one still trips.
#[test]
fn a_live_from_another_source_closes_nothing() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe(KEY.0, KEY.1, LinkSource::Venue, FeedStatus::Disconnected, 0);
    assert_eq!(
        dm.observe(KEY.0, KEY.1, LinkSource::StratLink, FeedStatus::Live, 100),
        LinkObservation::Quiet
    );
    assert_eq!(
        dm.observe(KEY.0, KEY.1, LinkSource::TradeLink, FeedStatus::Live, 200),
        LinkObservation::Quiet
    );
    assert!(
        dm.is_down(KEY.0, KEY.1, LinkSource::Venue),
        "a foreign Live must not close the venue window"
    );
    assert_eq!(
        keys(&mut dm, 1001),
        vec![key()],
        "the venue outage still trips after two foreign Lives"
    );

    let mut dm = latch(1000, &["binance"]);
    dm.observe(KEY.0, KEY.1, LinkSource::StratLink, FeedStatus::Disconnected, 0);
    dm.observe(KEY.0, KEY.1, LinkSource::TradeLink, FeedStatus::Live, 100);
    dm.observe(KEY.0, KEY.1, LinkSource::Venue, FeedStatus::Live, 200);
    assert!(dm.is_down(KEY.0, KEY.1, LinkSource::StratLink));
    assert_eq!(
        keys(&mut dm, 1001),
        vec![key()],
        "the strat outage trips: no foreign Live erased it"
    );
}

/// (s2) The trade link is a source in its own right: its `Disconnected` alone trips, and a `Live` on
/// the SAME reserved word closes it inside the grace.
#[test]
fn the_trade_link_trips_on_its_own_and_its_own_live_closes_it() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe(KEY.0, KEY.1, LinkSource::TradeLink, FeedStatus::Disconnected, 0);
    assert_eq!(keys(&mut dm, 1001), vec![key()]);
    assert!(dm.is_tripped(KEY.0, KEY.1));

    let mut dm = latch(1000, &["binance"]);
    dm.observe(KEY.0, KEY.1, LinkSource::TradeLink, FeedStatus::Disconnected, 0);
    dm.observe(KEY.0, KEY.1, LinkSource::TradeLink, FeedStatus::Live, 500);
    assert!(keys(&mut dm, 5000).is_empty(), "the trade link came back inside the grace");
}

/// (s3) ANY one source's expiry trips.
#[test]
fn any_one_sources_window_past_the_grace_trips() {
    for word in ["book", TRADE_LINK_STREAM, STRAT_LINK_STREAM] {
        let mut dm = latch(1000, &["binance"]);
        dm.observe(KEY.0, KEY.1, LinkSource::of(word), FeedStatus::Disconnected, 0);
        assert!(keys(&mut dm, 1000).is_empty(), "{word}: lag == grace is still inside it");
        assert_eq!(keys(&mut dm, 1001), vec![key()], "{word}: one source is enough");
    }
}

/// (s4) ⚠ **EACH source's window acts ONCE for its OWN outage; no other source's trip suppresses it.**
/// The first version deduped across sources ("the set already acted") and that was the defect the
/// review traced: a held trip of one source silenced every later outage of another. Here the second
/// source expiring in a LATER sweep, while the first still holds its trip, IS reported; neither is
/// reported twice for the same outage; each source's own `Live` re-arms only itself, and the
/// recovery says whether the key as a whole is clear.
#[test]
fn each_source_of_one_key_is_reported_once_for_its_own_outage() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe(KEY.0, KEY.1, LinkSource::Venue, FeedStatus::Disconnected, 0);
    dm.observe(KEY.0, KEY.1, LinkSource::StratLink, FeedStatus::Disconnected, 600);
    assert_eq!(
        trips(&mut dm, 1001),
        vec![(KEY.0.to_string(), KEY.1.to_string(), LinkSource::Venue)],
        "the venue window expires first and is reported"
    );
    let second = dm.check(1700);
    assert_eq!(second.len(), 1, "the strat window expires later and is reported too");
    assert_eq!(second[0].source, LinkSource::StratLink);
    assert_eq!(
        second[0].also_tripped,
        vec![LinkSource::Venue],
        "...and the trip says a venue trip is still held"
    );
    for t in [1701, 5000, 99_999] {
        assert!(keys(&mut dm, t).is_empty(), "no window is reported twice for one outage (t={t})");
    }
    assert!(dm.is_tripped(KEY.0, KEY.1));

    // each source recovers on its OWN word; only the LAST one clears the key
    assert_eq!(
        dm.observe(KEY.0, KEY.1, LinkSource::Venue, FeedStatus::Live, 2000),
        LinkObservation::RecoveredWhileAnotherSourceStaysTripped,
        "the strat trip is still outstanding, so the key is NOT clear"
    );
    assert!(dm.is_tripped(KEY.0, KEY.1));
    assert_eq!(
        dm.observe(KEY.0, KEY.1, LinkSource::StratLink, FeedStatus::Live, 2100),
        LinkObservation::Recovered
    );
    assert!(!dm.is_tripped(KEY.0, KEY.1));
    // the venue window re-armed at its own Live, so a NEW venue outage acts again
    dm.observe(KEY.0, KEY.1, LinkSource::TradeLink, FeedStatus::Disconnected, 3000);
    assert_eq!(keys(&mut dm, 4001), vec![key()], "a fresh outage acts afresh");
}

/// (s4a) Two sources of one key expiring in the SAME sweep are two trips (the sweep folds them into one
/// cancel pass per venue), in key order, each naming its source and the one tripped before it.
#[test]
fn two_sources_expiring_in_one_check_are_two_trips() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe(KEY.0, KEY.1, LinkSource::Venue, FeedStatus::Disconnected, 0);
    dm.observe(KEY.0, KEY.1, LinkSource::TradeLink, FeedStatus::Disconnected, 0);
    let got = dm.check(1001);
    assert_eq!(got.len(), 2, "one trip per window");
    assert_eq!((got[0].source, got[1].source), (LinkSource::Venue, LinkSource::TradeLink));
    assert!(got[0].also_tripped.is_empty(), "the first has no sibling tripped yet");
    assert_eq!(got[1].also_tripped, vec![LinkSource::Venue]);
    assert!(keys(&mut dm, 9999).is_empty(), "no re-fire while both stay down");
}

/// (s4c) ⚠ **THE REVIEW'S TRACE 3.** A source that never reports `Live` holds its trip for the life of
/// the process; it must not shadow another source's outage, its recovery, or the next one.
#[test]
fn a_source_that_never_reports_live_shadows_nobody() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe(KEY.0, KEY.1, LinkSource::StratLink, FeedStatus::Disconnected, 0);
    assert_eq!(keys(&mut dm, 1001), vec![key()]);
    // ... the strat link never reports Live again. The venue socket dies, comes back, dies again:
    for (down, up) in [(2000, 2500), (4000, 4500)] {
        dm.observe(KEY.0, KEY.1, LinkSource::Venue, FeedStatus::Disconnected, down);
        assert_eq!(
            trips(&mut dm, down + 1001),
            vec![(KEY.0.to_string(), KEY.1.to_string(), LinkSource::Venue)],
            "venue outage at t={down} trips although the strat trip is still held"
        );
        assert_eq!(
            dm.observe(KEY.0, KEY.1, LinkSource::Venue, FeedStatus::Live, up + 1500),
            LinkObservation::RecoveredWhileAnotherSourceStaysTripped
        );
    }
}

/// (s4d) A down, B down, A up, B up — in BOTH orders of the two sources, and with the expiry before or
/// after the first recovery: every window is reported exactly once, whichever source went first.
#[test]
fn down_down_up_up_reports_each_source_once_in_either_order() {
    for (first, second) in [
        (LinkSource::Venue, LinkSource::StratLink),
        (LinkSource::StratLink, LinkSource::Venue),
        (LinkSource::TradeLink, LinkSource::StratLink),
    ] {
        // both expire, then both recover
        let mut dm = latch(1000, &["binance"]);
        dm.observe(KEY.0, KEY.1, first, FeedStatus::Disconnected, 0);
        dm.observe(KEY.0, KEY.1, second, FeedStatus::Disconnected, 100);
        assert_eq!(trips(&mut dm, 1050).len(), 1, "{first:?} expires first");
        assert_eq!(trips(&mut dm, 1150).len(), 1, "{second:?} expires next");
        assert!(keys(&mut dm, 9000).is_empty());
        assert_eq!(
            dm.observe(KEY.0, KEY.1, first, FeedStatus::Live, 9100),
            LinkObservation::RecoveredWhileAnotherSourceStaysTripped
        );
        assert_eq!(
            dm.observe(KEY.0, KEY.1, second, FeedStatus::Live, 9200),
            LinkObservation::Recovered
        );
        assert!(!dm.is_tripped(KEY.0, KEY.1));

        // the first recovers BEFORE the second expires: the second still acts for its own outage
        // (the review's trace 4: the result no longer depends on which arrived first)
        let mut dm = latch(1000, &["binance"]);
        dm.observe(KEY.0, KEY.1, first, FeedStatus::Disconnected, 0);
        dm.observe(KEY.0, KEY.1, second, FeedStatus::Disconnected, 100);
        assert_eq!(trips(&mut dm, 1050).len(), 1);
        assert_eq!(
            dm.observe(KEY.0, KEY.1, first, FeedStatus::Live, 1100),
            LinkObservation::Recovered
        );
        assert_eq!(trips(&mut dm, 1150).len(), 1, "{second:?} acts for its own outage");
        assert_eq!(
            dm.observe(KEY.0, KEY.1, second, FeedStatus::Live, 1200),
            LinkObservation::Recovered
        );
    }
}

/// (s4e) A tripped source that is re-disclosed `Disconnected` (a producer repeating itself mid-outage)
/// is NOT re-opened and does not act again: only its own `Live` re-arms it.
#[test]
fn a_redisclosed_gap_after_the_trip_does_not_act_again() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe(KEY.0, KEY.1, LinkSource::Venue, FeedStatus::Disconnected, 0);
    assert_eq!(keys(&mut dm, 1001), vec![key()]);
    dm.observe(KEY.0, KEY.1, LinkSource::Venue, FeedStatus::Disconnected, 2000);
    assert!(keys(&mut dm, 9000).is_empty(), "still the same outage");
    dm.observe(KEY.0, KEY.1, LinkSource::Venue, FeedStatus::Live, 9100);
    dm.observe(KEY.0, KEY.1, LinkSource::Venue, FeedStatus::Disconnected, 9200);
    assert_eq!(keys(&mut dm, 10_201), vec![key()], "a new outage after its own Live acts afresh");
}

/// (s4b) Different symbols are different windows: the latch is keyed per `(venue, symbol, source)`, never per venue.
#[test]
fn the_window_is_per_key_not_per_venue() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe("binance", "BTCUSDT", LinkSource::Venue, FeedStatus::Disconnected, 0);
    dm.observe("binance", "ETHUSDT", LinkSource::TradeLink, FeedStatus::Disconnected, 0);
    assert_eq!(
        keys(&mut dm, 1001),
        vec![
            ("binance".to_string(), "BTCUSDT".to_string()),
            ("binance".to_string(), "ETHUSDT".to_string())
        ]
    );
}

/// (s5) The gap-closure invariant as the latch sees it: a `Disconnected` disclosed for a key by a
/// source is closed by a `Live` from the SAME source, which is what a producer owes on an
/// unsubscribe during an outage. With the closing `Live` the window is gone and nothing trips
/// later; the debt is per source, so a `Live` from another source does not discharge it.
#[test]
fn a_gap_closed_by_its_own_source_after_an_unsubscribe_cannot_trip() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe(KEY.0, KEY.1, LinkSource::StratLink, FeedStatus::Disconnected, 0);
    dm.observe(KEY.0, KEY.1, LinkSource::StratLink, FeedStatus::Live, 400);
    assert!(!dm.is_down(KEY.0, KEY.1, LinkSource::StratLink));
    assert!(keys(&mut dm, 1_000_000).is_empty());

    let mut dm = latch(1000, &["binance"]);
    dm.observe(KEY.0, KEY.1, LinkSource::StratLink, FeedStatus::Disconnected, 0);
    dm.observe(KEY.0, KEY.1, LinkSource::Venue, FeedStatus::Live, 400);
    assert_eq!(keys(&mut dm, 1001), vec![key()], "an unclosed strat gap still trips");
}

/// (s5b) ⚠ **PINS A KNOWN LIMIT (review F5), not a wanted behaviour.** The latch has no idea which keys the
/// core still mounts: a window opened for a key that is then unsubscribed or exited WITHOUT a closing
/// same-source `Live` stays open and trips after the grace, cancelling the venue's book (and halting)
/// for a key nobody watches any more. The gap-closure invariant is the producers' to keep (the
/// docs name it); nothing here enforces it. When the latch learns to drop windows for dropped keys
/// this test is the one that changes.
#[test]
fn a_window_nobody_closes_trips_even_for_a_key_that_is_gone() {
    let mut dm = latch(1000, &["binance"]);
    dm.observe(KEY.0, KEY.1, LinkSource::StratLink, FeedStatus::Disconnected, 0);
    // ... the key is unsubscribed here; the producer sends no `Live` ...
    assert_eq!(keys(&mut dm, 1_000_000), vec![key()], "a false trip after the grace");
}

/// (s6) `Stale` stays inert for every source, and an unarmed venue tracks nothing for any source.
#[test]
fn stale_is_inert_on_every_source_and_an_unarmed_venue_tracks_nothing() {
    let mut dm = latch(1000, &["binance"]);
    for word in ["book", TRADE_LINK_STREAM, STRAT_LINK_STREAM] {
        assert_eq!(
            dm.observe(KEY.0, KEY.1, LinkSource::of(word), FeedStatus::Stale, 0),
            LinkObservation::Quiet
        );
    }
    assert!(dm.links.is_empty(), "Stale opens no window for any source");
    assert!(keys(&mut dm, 10_000_000).is_empty());

    for word in ["book", TRADE_LINK_STREAM, STRAT_LINK_STREAM] {
        dm.observe("oanda", "EUR_USD", LinkSource::of(word), FeedStatus::Disconnected, 0);
        dm.observe("oanda", "EUR_USD", LinkSource::of(word), FeedStatus::Live, 1);
    }
    assert!(dm.links.is_empty(), "an unarmed venue is dropped before any state is built");
}

/// (s7) A `Live` for a source that never went down allocates nothing, whichever source it names.
#[test]
fn a_live_on_a_source_that_never_dropped_costs_no_state() {
    let mut dm = latch(1000, &["binance"]);
    for word in ["book", TRADE_LINK_STREAM, STRAT_LINK_STREAM] {
        assert_eq!(
            dm.observe(KEY.0, KEY.1, LinkSource::of(word), FeedStatus::Live, 0),
            LinkObservation::Quiet
        );
    }
    assert!(dm.links.is_empty());
}
