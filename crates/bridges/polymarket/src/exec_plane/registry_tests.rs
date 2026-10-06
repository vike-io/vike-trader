use super::*;
use crate::exec_plane::pending_events::DEFAULT_TTL_MS;

fn trade(tag: &str) -> serde_json::Value {
    serde_json::json!({ "event_type": "trade", "id": tag })
}

#[test]
fn accept_lookup_remove_roundtrip() {
    let r = PolymarketRegistry::new();
    assert!(r.on_accept("coid-1", "0xCLOB", 1).is_empty());
    assert_eq!(r.lookup_clob("0xCLOB"), Some(("coid-1".to_string(), 1)));
    assert_eq!(r.coid_to_clob("coid-1"), Some("0xCLOB".to_string()));
    r.remove("coid-1");
    assert_eq!(r.lookup_clob("0xCLOB"), None);
    assert_eq!(r.coid_to_clob("coid-1"), None);
    assert_eq!(r.lookup_clob("0xUNKNOWN"), None);
}

/// The question the bulk-cancel arm asks before it may assert an ACCOUNT-WIDE `cancel-all`:
/// does this batch name every order the mount believes is resting? Extra ids the map never
/// held do not spoil the answer (the arm rejects those separately); a missing one does. A
/// `remove`d order stops counting, because the coid→clob row is what "resting" means here.
#[test]
fn covers_all_live_answers_about_the_whole_map_and_nothing_else() {
    let r = PolymarketRegistry::new();
    assert!(r.covers_all_live(&[]), "an empty map is covered by an empty batch");
    assert!(r.on_accept("c1", "0xA", 1).is_empty());
    assert!(r.on_accept("c2", "0xB", -1).is_empty());

    assert!(r.covers_all_live(&["c1".to_string(), "c2".to_string()]));
    assert!(!r.covers_all_live(&["c1".to_string()]), "a missing order is not covered");
    assert!(
        r.covers_all_live(&["c1".to_string(), "c2".to_string(), "stranger".to_string()]),
        "an id this map never held is the arm's problem, not this answer's"
    );

    r.remove("c1");
    assert!(r.covers_all_live(&["c2".to_string()]), "a removed order stops being resting");
}

#[test]
fn clone_shares_state() {
    let a = PolymarketRegistry::new();
    let b = a.clone();
    assert!(a.on_accept("c", "0xX", -1).is_empty());
    assert_eq!(b.lookup_clob("0xX"), Some(("c".to_string(), -1)));
}

/// The `POLY_PRESUBMIT_REGISTER` idempotency contract (see [`PolymarketRegistry::on_accept`]'s
/// doc): registering the SAME `(coid, clob_id, side)` twice — first pre-submit, then again from
/// the real server ack — is a true no-op. One row each way, the right coid↔clob_id, the right
/// side, no duplication.
#[test]
fn on_accept_is_idempotent_for_the_same_triple() {
    let r = PolymarketRegistry::new();
    // pre-register the derived id, then the real ack re-registers the SAME id + side
    assert!(r.on_accept("coid-9", "0xDEADBEEF", -1).is_empty());
    assert!(r.on_accept("coid-9", "0xDEADBEEF", -1).is_empty(), "second call claims nothing");
    // a WS fill keyed by the derived clob_id resolves to the coid with the right side …
    assert_eq!(r.lookup_clob("0xDEADBEEF"), Some(("coid-9".to_string(), -1)));
    // … and the reverse (cancel) direction is the single expected id
    assert_eq!(r.coid_to_clob("coid-9"), Some("0xDEADBEEF".to_string()));
    // ONE remove fully clears both maps — proving no duplicate/shadow row survived the double
    // register (if a second row existed, one of these would still resolve).
    r.remove("coid-9");
    assert_eq!(r.lookup_clob("0xDEADBEEF"), None);
    assert_eq!(r.coid_to_clob("coid-9"), None);
}

// ---- the park: "not YET ours" ----

/// THE BUG, at the registry seam: an event that arrives before its id is registered is handed
/// back — in full, in order — the instant `on_accept` writes that id.
#[test]
fn events_parked_before_registration_come_back_on_accept() {
    let r = PolymarketRegistry::new();
    assert_eq!(r.rekey_for_decode("0xLATE"), None, "not registered yet");
    r.park(&["0xLATE".to_string()], UserEventKind::Trade, &trade("t1"));
    r.park(&["0xLATE".to_string()], UserEventKind::Trade, &trade("t2"));
    assert_eq!(r.pending_len(), 2);
    assert_eq!(r.pending_stats().parked, 2);

    let claimed = r.on_accept("coid-late", "0xLATE", 1);
    assert_eq!(claimed.len(), 2, "both events replayed: {claimed:?}");
    assert_eq!(claimed[0].frame["id"], "t1", "oldest first");
    assert_eq!(claimed[1].frame["id"], "t2");
    assert_eq!(r.pending_len(), 0, "the park is drained, not copied");
    assert_eq!(r.pending_stats().replayed, 2);
    assert_eq!(r.pending_stats().expired, 0, "nothing was lost");
    // and a second accept of the same id claims nothing (no re-delivery loop)
    assert!(r.on_accept("coid-late", "0xLATE", 1).is_empty());
}

#[test]
fn a_park_for_one_id_is_not_claimed_by_another() {
    let r = PolymarketRegistry::new();
    r.park(&["0xA".to_string()], UserEventKind::Trade, &trade("t1"));
    assert!(r.on_accept("coid-b", "0xB", 1).is_empty(), "0xB must not claim 0xA's event");
    assert_eq!(r.pending_len(), 1);
    assert_eq!(r.on_accept("coid-a", "0xA", 1).len(), 1);
}

/// One frame naming several unresolved ids parks under each, so whichever turns out to be ours
/// triggers the replay.
#[test]
fn a_frame_parks_under_every_id_it_names() {
    let r = PolymarketRegistry::new();
    r.park(&["0xTAKER".to_string(), "0xMAKER".to_string()], UserEventKind::Trade, &trade("t1"));
    assert_eq!(r.pending_len(), 2);
    assert_eq!(r.on_accept("coid-m", "0xMAKER", -1).len(), 1);
    assert_eq!(r.pending_len(), 1, "the other copy stays until it expires");
}

// ---- the park: "not ours" ----

/// A genuinely foreign id is discarded by the TTL, COUNTED, and (see the `warn!` in
/// `expire_pending_at`) never silently.
#[test]
fn a_genuinely_unknown_id_expires_and_is_counted() {
    // ttl 0 ⇒ anything parked is already past it; no sleeping, no clock injection.
    let r = PolymarketRegistry::with_limits(64, 8, 0, DEFAULT_SETTLING_GRACE_MS);
    r.park(&["0xNOTOURS".to_string()], UserEventKind::Trade, &trade("t1"));
    assert_eq!(r.pending_len(), 1);

    assert_eq!(r.expire_pending(), 1, "one event discarded");
    assert_eq!(r.pending_len(), 0);
    assert_eq!(r.pending_stats().expired, 1);
    assert_eq!(r.pending_stats().replayed, 0);
    // …and a later registration of that id finds nothing to replay (it is really gone)
    assert!(r.on_accept("coid-x", "0xNOTOURS", 1).is_empty());
}

#[test]
fn a_still_young_park_is_not_expired() {
    let r = PolymarketRegistry::new(); // 30s TTL
    r.park(&["0xFRESH".to_string()], UserEventKind::Trade, &trade("t1"));
    assert_eq!(r.expire_pending(), 0, "far inside the TTL");
    assert_eq!(r.pending_len(), 1);
    assert_eq!(r.pending_stats().expired, 0);
    assert_eq!(r.on_accept("coid-f", "0xFRESH", 1).len(), 1, "still claimable");
}

#[test]
fn expiring_an_empty_park_is_free_and_silent() {
    let r = PolymarketRegistry::new();
    assert_eq!(r.expire_pending(), 0);
    assert_eq!(r.pending_stats().expired, 0);
}

/// The BOUND: at capacity the oldest bucket is shed and counted as `evicted` — a distinct
/// counter from `expired`, because it means the bound is too small, not that the event was
/// foreign.
#[test]
fn the_park_bound_sheds_the_oldest_and_counts_it_separately() {
    let r = PolymarketRegistry::with_limits(2, 8, DEFAULT_TTL_MS, DEFAULT_SETTLING_GRACE_MS);
    r.park(&["0xOLD".to_string()], UserEventKind::Trade, &trade("t1"));
    r.park(&["0xMID".to_string()], UserEventKind::Trade, &trade("t2"));
    r.park(&["0xNEW".to_string()], UserEventKind::Trade, &trade("t3"));

    assert_eq!(r.pending_len(), 2, "held at the bound");
    assert_eq!(r.pending_stats().evicted, 1);
    assert_eq!(r.pending_stats().expired, 0, "an eviction is not an expiry");
    assert!(r.on_accept("c", "0xOLD", 1).is_empty(), "the oldest was the one shed");
    assert_eq!(r.on_accept("c", "0xNEW", 1).len(), 1, "the newest survived");
}

// ---- the settling grace: a match that raced its own cancel ----

#[test]
fn a_cancelled_order_still_rekeys_inside_the_settling_grace() {
    let r = PolymarketRegistry::new();
    assert!(r.on_accept("coid-c", "0xC", -1).is_empty());
    r.remove("coid-c");

    // reconcile's view: gone.
    assert_eq!(r.lookup_clob("0xC"), None);
    assert_eq!(r.coid_to_clob("coid-c"), None, "never cancel a dead order twice");
    // the decoder's view: a fill already on the wire still folds, with OUR side.
    assert_eq!(r.rekey_for_decode("0xC"), Some(("coid-c".to_string(), -1)));
    assert_eq!(r.pending_stats().settled, 1);
}

#[test]
fn the_settling_grace_expires() {
    let r = PolymarketRegistry::with_limits(64, 8, DEFAULT_TTL_MS, 0); // zero grace
    assert!(r.on_accept("coid-c", "0xC", 1).is_empty());
    r.remove("coid-c");
    assert_eq!(r.rekey_for_decode("0xC"), None, "past the grace, it is gone for good");
    assert_eq!(r.pending_stats().settled, 0);
}

#[test]
fn re_accepting_a_settling_id_promotes_it_back_to_live() {
    let r = PolymarketRegistry::new();
    assert!(r.on_accept("coid-c", "0xC", 1).is_empty());
    r.remove("coid-c");
    assert!(r.on_accept("coid-c", "0xC", 1).is_empty());
    assert_eq!(r.lookup_clob("0xC"), Some(("coid-c".to_string(), 1)), "live again");
    // resolving now goes through the LIVE map, so the settling counter does not move
    assert_eq!(r.rekey_for_decode("0xC"), Some(("coid-c".to_string(), 1)));
    assert_eq!(r.pending_stats().settled, 0);
}

#[test]
fn removing_an_unknown_coid_is_a_no_op() {
    let r = PolymarketRegistry::new();
    r.remove("never-registered");
    assert_eq!(r.rekey_for_decode("0xANY"), None);
}

#[test]
fn the_settling_map_is_bounded() {
    let r = PolymarketRegistry::new();
    for i in 0..(DEFAULT_SETTLING_CAPACITY + 5) {
        let (coid, clob) = (format!("coid-{i}"), format!("0x{i}"));
        assert!(r.on_accept(&coid, &clob, 1).is_empty());
        r.remove(&coid);
    }
    // the earliest demotions were shed; the most recent are still re-keyable
    assert_eq!(r.rekey_for_decode("0x0"), None);
    let last = DEFAULT_SETTLING_CAPACITY + 4;
    assert_eq!(r.rekey_for_decode(&format!("0x{last}")), Some((format!("coid-{last}"), 1)));
}
