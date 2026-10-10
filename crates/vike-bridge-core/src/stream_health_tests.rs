use super::*;

// --- transport half -----------------------------------------------------------------------

#[test]
fn enter_gap_emits_once_then_guards() {
    let mut h = StreamHealth::new(1000);
    assert_eq!(h.enter_gap(100), Some(HealthEvent::Gap { at_ts_ms: 100 }));
    assert!(h.in_gap());
    // a flapping reconnect calls enter_gap again — no nested Gap
    assert_eq!(h.enter_gap(150), None);
    assert_eq!(h.enter_gap(200), None);
}

#[test]
fn recover_closes_the_gap_with_the_matching_start() {
    let mut h = StreamHealth::new(1000);
    h.enter_gap(100);
    assert_eq!(h.recover(), Some(HealthEvent::Live { gap_started_ts_ms: Some(100) }));
    assert!(!h.in_gap());
    // recover again with no open gap → nothing (Live only closes a gap)
    assert_eq!(h.recover(), None);
}

#[test]
fn recover_without_a_gap_is_none() {
    // first successful connect: no prior gap → no Live
    let mut h = StreamHealth::new(1000);
    assert_eq!(h.recover(), None);
}

#[test]
fn second_outage_gets_a_fresh_gap() {
    let mut h = StreamHealth::new(1000);
    h.enter_gap(100);
    h.recover();
    // a NEW outage later → a fresh Gap at the new ts (not guarded by the first)
    assert_eq!(h.enter_gap(500), Some(HealthEvent::Gap { at_ts_ms: 500 }));
    assert_eq!(h.recover(), Some(HealthEvent::Live { gap_started_ts_ms: Some(500) }));
}

// --- freshness half -----------------------------------------------------------------------

// threshold 1000ms. Data ts advances with now → never stale.
#[test]
fn advancing_data_never_trips() {
    let mut h = StreamHealth::new(1000);
    for t in [0i64, 500, 1000, 1500, 2000] {
        h.observe_data(t);
        assert_eq!(h.check_freshness(t), None, "fresh data at t={t} must not trip");
    }
    assert!(!h.is_stale());
}

// transport alive (check keeps being called) but data ts frozen past threshold → exactly one
// Stale.
#[test]
fn frozen_data_trips_once_then_recovers_once() {
    let mut h = StreamHealth::new(1000);
    h.observe_data(0); // newest data ts = 0
    assert_eq!(h.check_freshness(500), None); // lag 500 <= 1000, fresh
    // data freezes; wall clock advances past threshold
    assert_eq!(
        h.check_freshness(1500),
        Some(HealthEvent::Stale { newest_data_ts_ms: 0, now_ms: 1500 }),
        "lag 1500 > 1000 → Stale"
    );
    assert!(h.is_stale());
    // still frozen, later checks do NOT re-emit
    assert_eq!(h.check_freshness(2000), None);
    assert_eq!(h.check_freshness(3000), None);
    // fresh data resumes
    h.observe_data(3100);
    assert_eq!(
        h.check_freshness(3100),
        Some(HealthEvent::Live { gap_started_ts_ms: Some(1500) }),
        "data fresh again → one Live closing the episode at the trip ts"
    );
    assert!(!h.is_stale());
    assert_eq!(h.check_freshness(3200), None); // no double recovery
}

// a sparse-but-CURRENT market: data arrives infrequently but each frame's ts is recent → no
// trip.
#[test]
fn sparse_but_current_does_not_trip() {
    let mut h = StreamHealth::new(1000);
    // one frame every 800ms (< threshold), ts == arrival — always fresh
    for t in [0i64, 800, 1600, 2400, 3200] {
        h.observe_data(t);
        assert_eq!(h.check_freshness(t), None);
        assert_eq!(
            h.check_freshness(t + 700),
            None,
            "still within threshold before the next frame"
        );
    }
    assert!(!h.is_stale());
}

// no data observed yet AND never armed (a fresh `new()`, `reset_freshness` never called) →
// nothing to judge, never trips on an unstarted stream: a persist-across-reconnect feed
// (Polymarket) never calls `reset_freshness`, so only real observed data arms the clock there.
#[test]
fn no_data_yet_never_trips() {
    let mut h = StreamHealth::new(1000);
    assert_eq!(h.check_freshness(999_999), None);
    assert!(!h.is_stale());
}

// The boundary is strict `>`: lag == threshold is NOT yet stale; lag == threshold+1 is.
#[test]
fn check_at_exact_threshold_is_not_yet_stale() {
    let mut h = StreamHealth::new(1000);
    h.observe_data(0);
    assert_eq!(h.check_freshness(1000), None, "lag == threshold is fresh (strict >)");
    assert_eq!(
        h.check_freshness(1001),
        Some(HealthEvent::Stale { newest_data_ts_ms: 0, now_ms: 1001 }),
        "one ms past threshold → Stale"
    );
}

// observe_data() is monotonic-max: an out-of-order OLDER frame must not drag newest_ts
// backward.
#[test]
fn observe_ignores_out_of_order_older_frames() {
    let mut h = StreamHealth::new(1000);
    h.observe_data(2000);
    h.observe_data(500); // older, out of order — must be ignored
    // freshness is judged from 2000, not 500:
    assert_eq!(h.check_freshness(2500), None, "lag from newest (2000) is 500 <= 1000 → fresh");
    assert_eq!(
        h.check_freshness(3001),
        Some(HealthEvent::Stale { newest_data_ts_ms: 2000, now_ms: 3001 }),
        "newest_ts stayed at 2000, never regressed to 500"
    );
}

// --- gating + reset -----------------------------------------------------------------------

#[test]
fn freshness_check_is_suppressed_while_in_a_transport_gap() {
    let mut h = StreamHealth::new(1000);
    assert_eq!(h.enter_gap(100), Some(HealthEvent::Gap { at_ts_ms: 100 }));

    // data is badly stale, but a transport gap is open — check_freshness must stay silent
    h.observe_data(0);
    assert_eq!(h.check_freshness(1_000_000), None, "gated while in_gap()");
    assert!(!h.is_stale(), "gating must not mark stale internally either");

    assert_eq!(h.recover(), Some(HealthEvent::Live { gap_started_ts_ms: Some(100) }));
    // gate lifted — freshness resumes normal operation and trips as expected
    assert_eq!(
        h.check_freshness(1_000_000),
        Some(HealthEvent::Stale { newest_data_ts_ms: 0, now_ms: 1_000_000 }),
        "after recover(), check_freshness works again"
    );
    assert!(h.is_stale());
}

#[test]
fn reset_freshness_clears_an_open_stale_episode() {
    let mut h = StreamHealth::new(1000);
    h.observe_data(0);
    assert_eq!(
        h.check_freshness(1500),
        Some(HealthEvent::Stale { newest_data_ts_ms: 0, now_ms: 1500 })
    );
    assert!(h.is_stale());

    // reset clears the open stale episode (and re-arms the clock at now = 1500) WITHOUT emitting.
    h.reset_freshness(1500);
    assert!(!h.is_stale(), "reset clears the open stale episode without emitting anything");

    // Checked at / near the 1500 arm floor → within threshold, so no immediate re-trip and — the
    // key guard — no spurious `Live` (the cleared `stale_since` cannot recover): stays silent.
    assert_eq!(h.check_freshness(1500), None);
    assert_eq!(h.check_freshness(2000), None, "age 500 from the 1500 arm floor is still fresh");
    assert!(!h.is_stale());
}

#[test]
fn reset_freshness_does_not_touch_transport_gap_state() {
    let mut h = StreamHealth::new(1000);
    assert_eq!(h.enter_gap(100), Some(HealthEvent::Gap { at_ts_ms: 100 }));
    h.reset_freshness(100);
    assert!(h.in_gap(), "reset_freshness must not clear transport gap state");
    // the gap can still be closed normally afterward
    assert_eq!(h.recover(), Some(HealthEvent::Live { gap_started_ts_ms: Some(100) }));
}

// A session armed at start (the depth per-session `reset_freshness(now)`) that then receives
// ZERO data still ages toward `Stale` from the arm floor (a subscribe that silently succeeds but
// sends nothing behind a live socket). Without the `armed_at` floor a dataless session would trip
// NOTHING.
#[test]
fn armed_session_with_no_data_goes_stale() {
    let mut h = StreamHealth::new(1000);
    h.reset_freshness(0); // armed at session start; observe_data is NEVER called below
    assert_eq!(h.check_freshness(500), None, "within threshold of the arm floor → still fresh");
    assert_eq!(
        h.check_freshness(1001),
        Some(HealthEvent::Stale { newest_data_ts_ms: 0, now_ms: 1001 }),
        "past threshold with no data → the armed floor trips Stale from the session start"
    );
    assert!(h.is_stale());
}

// Once real data lands, `newest_ts` supersedes the `armed_at` floor: freshness ages from the
// DATA ts, not the (older) arm time — so an armed session that DOES receive data is judged
// exactly as it would be without the floor (`newest_ts.or(armed_at)`).
#[test]
fn armed_then_real_data_uses_the_data_ts() {
    let mut h = StreamHealth::new(1000);
    h.reset_freshness(0); // armed at 0…
    h.observe_data(50_000); // …but real data at 50_000 supersedes the floor
    assert_eq!(
        h.check_freshness(50_500),
        None,
        "ages from the data ts (50_000), not the arm floor (0) — else 50_500 would trip"
    );
    assert!(!h.is_stale());
}

// --- the tick-lane map + the transport-only constructor ---------------------------------

/// The 1:1 `HealthEvent` -> `FeedStatus` map, pinned in all three directions. ⚠ The `Gap` row
/// is the load-bearing one: `Disconnected` is the ONE status that opens the connection-state
/// dead-man's grace window, so mapping a gap to anything else would leave the tick-track pumps
/// disclosing a link death the switch cannot act on.
#[test]
fn a_health_event_maps_onto_the_feed_status_the_core_acts_on() {
    use vike_model::FeedStatus;
    assert_eq!(HealthEvent::Gap { at_ts_ms: 7 }.feed_status(), FeedStatus::Disconnected);
    assert_eq!(HealthEvent::Live { gap_started_ts_ms: Some(7) }.feed_status(), FeedStatus::Live);
    assert_eq!(
        HealthEvent::Stale { newest_data_ts_ms: 1, now_ms: 9 }.feed_status(),
        FeedStatus::Stale,
    );
}

/// [`StreamHealth::transport_only`] is the SAME transport machine — one gap per outage, `Live`
/// only to close one — and its freshness half can never speak, however far the clock is
/// advanced. Both halves asserted: a constructor that silently disabled the gap tracking too
/// would pass a test that only checked the second.
#[test]
fn transport_only_keeps_the_gap_half_and_can_never_go_stale() {
    let mut h = StreamHealth::transport_only();
    assert_eq!(h.enter_gap(100), Some(HealthEvent::Gap { at_ts_ms: 100 }));
    assert_eq!(h.enter_gap(200), None, "still one gap per outage");
    assert!(h.in_gap());
    assert_eq!(h.recover(), Some(HealthEvent::Live { gap_started_ts_ms: Some(100) }));
    assert_eq!(h.recover(), None, "Live still only ever CLOSES a gap");

    // …and the freshness half is inert even if somebody calls it: armed at 0, judged a century
    // later, with the threshold at i64::MAX.
    h.reset_freshness(0);
    assert_eq!(h.check_freshness(3_155_760_000_000), None, "no threshold can trip");
    assert!(!h.is_stale());
}

/// The ONE `HealthEvent` → `vike_data::StreamStatus` map every sink-owning feed calls: each case maps
/// name-for-name (`Gap` → `GapStart`) with every field carried verbatim, `None` included.
#[cfg(feature = "full")]
#[test]
fn health_events_map_one_to_one_onto_stream_status() {
    use vike_data::StreamStatus;
    assert_eq!(
        health_to_stream_status(HealthEvent::Gap { at_ts_ms: 7 }),
        StreamStatus::GapStart { at_ts_ms: 7 }
    );
    assert_eq!(
        health_to_stream_status(HealthEvent::Live { gap_started_ts_ms: Some(7) }),
        StreamStatus::Live { gap_started_ts_ms: Some(7) }
    );
    assert_eq!(
        health_to_stream_status(HealthEvent::Live { gap_started_ts_ms: None }),
        StreamStatus::Live { gap_started_ts_ms: None }
    );
    assert_eq!(
        health_to_stream_status(HealthEvent::Stale { newest_data_ts_ms: 5, now_ms: 9 }),
        StreamStatus::Stale { newest_data_ts_ms: 5, now_ms: 9 }
    );
}
