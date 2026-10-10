//! T1, T2 and the failed-subscribe case: one venue subscription per key, release/linger/reap, the panic path, and no phantom subscription.

use super::*;
use vike_datahub::md::MD_LINGER;

// ------------------------------------------------------------------------------------------------
// T1 — one venue subscription per key, however many subscribers
// ------------------------------------------------------------------------------------------------

/// **The ruling-2 property.** Two sessions on one key are ONE `subscribe_depth`.
///
/// ⚠ Non-vacuity: `calls.len() == 1` ALONE passes against a hub where session B's acquire silently
/// FAILED, and against one that subscribed the wrong symbol. So the assertions are exact call
/// CONTENTS, **and** session B's acquire returning `Ok`, **and** the venue count staying one after a
/// second reconcile. The acceptance assertion is not redundant with the call-count one: it is the
/// only thing separating "correctly shared" from "silently refused".
///
/// Mutation: make `reconcile` subscribe unconditionally instead of only when `sub_id` is `None` →
/// 1 becomes 2, red.
#[test]
fn a_second_subscriber_does_not_resubscribe_the_venue() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Depth);

    let mut a = h.open_session().unwrap();
    h.acquire(a.id(), &s).expect("session A accepted");
    h.reconcile(now());

    let mut b = h.open_session().unwrap();
    h.acquire(b.id(), &s).expect("session B must be ACCEPTED, not silently refused");
    h.reconcile(now());

    let depth: Vec<Call> = log.all().into_iter().filter(|c| matches!(c, Call::Depth(..))).collect();
    assert_eq!(
        depth,
        vec![Call::Depth("binance".into(), "BTCUSDT.P".into())],
        "exactly ONE venue subscription, for exactly that symbol: {depth:?}"
    );
    assert_eq!(log.count(|c| matches!(c, Call::Client(_))), 1, "and ONE venue client");

    // The third leg: one SESSION acquiring the key twice (two Trade windows on one symbol) is still
    // one subscription, and one release does not take the other's ladder away.
    h.acquire(a.id(), &s).expect("re-acquire in the same session");
    h.reconcile(now());
    assert_eq!(log.count(|c| matches!(c, Call::Depth(..))), 1);
    a.release_at(now());
    h.reconcile(now());
    assert_eq!(
        log.count(|c| matches!(c, Call::Unsubscribe(..))),
        0,
        "session B still holds the key — nothing may be unsubscribed"
    );
    b.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// T2 — release, linger, reap; and the panic path
// ------------------------------------------------------------------------------------------------

/// **T2a — a PANICKING writer releases**, because the release is a `Drop` and not a statement.
///
/// ⚠ This leg asserts an ABSENCE (nothing was unsubscribed yet) and would pass against a hub that
/// does nothing at all. `a_reap_past_the_linger_unsubscribes_per_key` is what rescues it: the same
/// key, the same harness, one `reconcile` later, the unsubscribe MUST appear. Neither leg is worth
/// writing without the other.
///
/// ⚠ It also depends on the hub's poison-recovery discipline: the panic below poisons nothing here,
/// but a hub using `.expect("poisoned")` anywhere would make a real writer panic turn every later
/// assertion into a panic, and the failure would read as a broken test rather than a broken
/// teardown.
#[test]
fn a_panicking_writer_releases_every_key_through_drop() {
    let log = Log::default();
    let h = hub(&log);
    let k1 = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let k2 = spec("binance", "BTCUSDT.P", MdLane::Trades);
    let at = now();

    let hub2 = Arc::clone(&h);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let g = hub2.open_session().unwrap();
        hub2.acquire(g.id(), &k1).unwrap();
        hub2.acquire(g.id(), &k2).unwrap();
        hub2.reconcile(at);
        panic!("writer fault");
    }));
    assert!(unwound.is_err(), "the guard: the closure must actually unwind");

    assert_eq!(log.count(|c| matches!(c, Call::Depth(..) | Call::Trades(..))), 2);
    // Both keys are at zero, and NOTHING is torn down — they are in LINGER, not reaped.
    h.reconcile(at + 1);
    assert_eq!(
        log.count(|c| matches!(c, Call::Unsubscribe(..) | Call::BeginShutdown(_))),
        0,
        "a released key lingers; it is not torn down at once: {:?}",
        log.all()
    );
}

/// **T2b — the reap, and the §0 TEARDOWN HAZARD as an assertion.**
///
/// A reap that touches SOME of a venue's keys must call `unsubscribe` per key and **never**
/// `begin_shutdown`: `crates/vike-data/src/live.rs`'s `FeedRegistry::raise_stops` — which is what
/// every venue's `begin_shutdown` IS — raises the stop flag of EVERY subscription that client owns.
/// A partial reap using it would stop the OTHER keys' threads while the registry still held their
/// ids, so the hub would report itself subscribed and deliver nothing. §5.3 specifies exactly that
/// two-phase teardown for a multi-key reap, and this test is why it is not implemented literally.
#[test]
fn a_partial_reap_unsubscribes_per_key_and_never_raises_every_stop() {
    let log = Log::default();
    let h = hub(&log);
    let keep = spec("binance", "ETHUSDT.P", MdLane::Depth);
    let go = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let at = now();

    let mut keeper = h.open_session().unwrap();
    h.acquire(keeper.id(), &keep).unwrap();
    let mut leaver = h.open_session().unwrap();
    h.acquire(leaver.id(), &go).unwrap();
    h.reconcile(at);
    assert_eq!(log.count(|c| matches!(c, Call::Depth(..))), 2);

    leaver.release_at(at);
    let r = h.reconcile(at + MD_LINGER.as_millis() as i64 + 1);
    assert_eq!(r.stopped, 1, "exactly the reaped key: {r:?}");
    assert_eq!(r.clients_dropped, 0, "the venue client stays — another key is still live");
    assert_eq!(log.count(|c| matches!(c, Call::Unsubscribe(..))), 1);
    assert_eq!(
        log.count(|c| matches!(c, Call::BeginShutdown(_) | Call::Shutdown(_))),
        0,
        "⚠ a PARTIAL reap must never raise every stop flag this client owns: {:?}",
        log.all()
    );
    keeper.release_at(at);
}

/// ...and the OTHER half: when the reap set IS the client's whole live set, the two-phase idiom is
/// legitimate and is used — `begin_shutdown` BEFORE `shutdown`, asserted on ORDER, because that is
/// the only place the raise-all-then-join win is available and the only place it is safe.
#[test]
fn a_whole_venue_reap_raises_stops_before_it_joins_and_drops_the_client() {
    let log = Log::default();
    let h = hub(&log);
    let a = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let b = spec("binance", "BTCUSDT.P", MdLane::Trades);
    let at = now();

    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &a).unwrap();
    h.acquire(g.id(), &b).unwrap();
    h.reconcile(at);
    g.release_at(at);
    let r = h.reconcile(at + MD_LINGER.as_millis() as i64 + 1);

    assert_eq!(r.stopped, 2);
    assert_eq!(r.clients_dropped, 1, "the venue client itself is dropped: {r:?}");
    let teardown: Vec<Call> = log
        .all()
        .into_iter()
        .filter(|c| matches!(c, Call::BeginShutdown(_) | Call::Shutdown(_)))
        .collect();
    assert_eq!(
        teardown,
        vec![Call::BeginShutdown("binance".into()), Call::Shutdown("binance".into())],
        "phase one raises every flag, THEN phase two joins: {teardown:?}"
    );
    assert_eq!(
        log.count(|c| matches!(c, Call::Unsubscribe(..))),
        0,
        "a whole-client reap does not also pay a per-key timeout"
    );
}

/// **T2c — re-acquiring inside the linger CLEARS the deadline.**
///
/// Catches a `reap` that stores an ABSOLUTE deadline at release and forgets to clear it on
/// re-acquire: a Trade window toggled off and on then loses its book 60 s later, in the exact series
/// somebody just asked for.
#[test]
fn a_key_reacquired_inside_the_linger_is_never_reaped() {
    let log = Log::default();
    let h = hub(&log);
    let s = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let at = now();

    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &s).unwrap();
    h.reconcile(at);
    g.release_at(at);

    // Halfway through the linger: nothing yet.
    h.reconcile(at + MD_LINGER.as_millis() as i64 / 2);
    assert_eq!(log.count(|c| matches!(c, Call::Unsubscribe(..))), 0);

    // Re-acquire, then walk PAST the original deadline. Still nothing.
    let mut g2 = h.open_session().unwrap();
    h.acquire(g2.id(), &s).unwrap();
    h.reconcile(at + MD_LINGER.as_millis() as i64 + 5_000);
    assert_eq!(
        log.count(|c| matches!(c, Call::Unsubscribe(..) | Call::Shutdown(_))),
        0,
        "the re-acquire must clear the absolute deadline: {:?}",
        log.all()
    );
    // ...and the key was never re-subscribed either: the survivor was left strictly alone.
    assert_eq!(log.count(|c| matches!(c, Call::Depth(..))), 1);
    g2.release_at(at);
}

// ------------------------------------------------------------------------------------------------
// A failed venue subscribe leaves no phantom
// ------------------------------------------------------------------------------------------------

/// A `subscribe_*` that FAILS must leave the key WANTED with no subscription id, so the next pass
/// retries it — never a key the hub believes is live. That is §6.1's "connects, reports healthy,
/// delivers nothing" failure, produced by bookkeeping rather than by a socket.
#[test]
fn a_failed_venue_subscribe_leaves_no_phantom_subscription() {
    let log = Log::default();
    let h = MdHub::new(builder(log.clone(), true), vec!["binance".into()]);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &spec("binance", "BTCUSDT.P", MdLane::Depth)).unwrap();

    let r = h.reconcile(now());
    assert_eq!(r.started, 0, "nothing started: {r:?}");
    assert_eq!(r.failed.len(), 1, "and the failure is REPORTED, not swallowed: {r:?}");

    // The next pass RETRIES: the key is still wanted, so the attempt count grows.
    let r2 = h.reconcile(now());
    assert_eq!(r2.failed.len(), 1, "still wanted, still retried: {r2:?}");
    assert_eq!(log.count(|c| matches!(c, Call::Depth(..))), 2, "two attempts, no phantom");
    g.release_at(now());
}
