use super::*;
use std::sync::Mutex;
use vike_model::events::OrderSubmitted;

/// Audit A3: the supervisor resyncs once per reconnect (gen bump), never without one, and
/// self-exits when the pump drops the gen Arc (live-gate: a dead session never bumps → quiet).
#[test]
fn resync_fires_on_gen_bump_and_exits_with_the_pump() {
    let generation = Arc::new(AtomicU64::new(0));
    let weak = Arc::downgrade(&generation);
    let stop = Arc::new(AtomicBool::new(false));
    let fetches = Arc::new(AtomicU64::new(0));
    let emitted = Arc::new(Mutex::new(Vec::<String>::new()));

    let (stop_t, fetches_t, emitted_t) = (stop.clone(), fetches.clone(), emitted.clone());
    let handle = std::thread::spawn(move || {
        run_resync_supervisor(
            weak,
            0, // the gen as it stood when this test created it
            0, // no history floor — this test covers the gen edge, not the restart law
            &stop_t,
            Duration::from_millis(1),
            Duration::from_millis(0),
            || {
                fetches_t.fetch_add(1, Ordering::Relaxed);
                vec![Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: "resynced".into(),
                    ts: 0,
                })]
            },
            |ev| {
                if let Event::OrderSubmitted(e) = ev {
                    emitted_t.lock().unwrap().push(e.client_order_id);
                }
                true
            },
            None, // Task 13: no reconcile trigger wired — this test covers event-replay only
        );
    });

    // No reconnect yet → no resync.
    std::thread::sleep(Duration::from_millis(15));
    assert_eq!(fetches.load(Ordering::Relaxed), 0, "no resync without a reconnect");

    // Simulate a reconnect.
    generation.fetch_add(1, Ordering::Relaxed);
    // Poll the EMITTED list — the value asserted on — never the `fetches` counter: the fetch closure
    // bumps `fetches` as its FIRST act, before it has even returned the event the supervisor then
    // hands to the emit closure, so a `fetches != 0` poll can win that race by an instant and read
    // an empty list (it did once, under load: `left: [] right: ["resynced"]`). The race is in this
    // test's observation point, not in the supervisor, and its twin
    // `crates/vike-bridge-core/tests/resync_triggers_reconcile.rs`'s
    // `reconcile_trigger_fires_once_after_settle_and_never_on_initial_connect` removed the same one
    // the same way.
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    loop {
        let seen = emitted.lock().unwrap().clone();
        if !seen.is_empty() {
            assert_eq!(seen, vec!["resynced".to_string()]);
            break;
        }
        assert!(Instant::now() < deadline, "resync never fired on the gen bump");
        std::thread::sleep(Duration::from_millis(2));
    }

    // Pump torn down → supervisor must self-exit.
    drop(generation);
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "supervisor did not exit when the pump was gone");
        std::thread::sleep(Duration::from_millis(2));
    }
    handle.join().unwrap();
    assert_eq!(fetches.load(Ordering::Relaxed), 1, "exactly one resync for one reconnect");
}

/// **The losing interleaving, made deterministic — no sleeps, no scheduling luck.** The gen is
/// ALREADY at 1 before the supervisor thread exists: this is exactly the state
/// `spawn_pump_with_resync` can hand it, since the pump is spawned first and can reconnect
/// before the supervisor is scheduled. The baseline the caller sampled at wiring time was 0, so
/// that reconnect must still register.
///
/// While the supervisor sampled its own baseline this was unfixable by waiting: it would read 1,
/// compare 1 == 1, and sit quiet for the life of the session. The test then does not fail slowly
/// — it fails at whatever deadline you pick, which is why widening the deadline from 3 s to 30 s
/// (#1020) produced a 30.083 s failure instead of a 3 s one.
#[test]
fn a_gen_bump_that_predates_the_supervisor_is_not_lost() {
    let generation = Arc::new(AtomicU64::new(1)); // the pump already re-opened once
    let weak = Arc::downgrade(&generation);
    let stop = Arc::new(AtomicBool::new(false));
    let fetches = Arc::new(AtomicU64::new(0));

    let (stop_t, fetches_t) = (stop.clone(), fetches.clone());
    let handle = std::thread::spawn(move || {
        run_resync_supervisor(
            weak,
            0, // ...but the caller's baseline, taken before the pump could bump, was 0
            0, // no history floor — this test covers the gen edge, not the restart law
            &stop_t,
            Duration::from_millis(1),
            Duration::from_millis(0),
            || {
                fetches_t.fetch_add(1, Ordering::Relaxed);
                Vec::new()
            },
            |_ev| true,
            None,
        );
    });

    // Fires with NO further bump — the pre-existing one is what it must not have swallowed.
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    while fetches.load(Ordering::Relaxed) == 0 {
        assert!(
            Instant::now() < deadline,
            "a reconnect that predated the supervisor was swallowed by its own baseline read"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    drop(generation);
    stop.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "supervisor did not exit");
        std::thread::sleep(Duration::from_millis(2));
    }
    handle.join().unwrap();
}

/// Drive one real reconnect resync and collect everything that reached the sink.
fn replay_once(spawn_ms: i64, rows: Vec<Event>) -> Vec<Event> {
    let generation = Arc::new(AtomicU64::new(0));
    let weak = Arc::downgrade(&generation);
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::<Event>::new()));
    let fetched = Arc::new(AtomicBool::new(false));

    let (stop_t, seen_t, fetched_t) = (stop.clone(), seen.clone(), fetched.clone());
    let handle = std::thread::spawn(move || {
        let mut rows = Some(rows);
        run_resync_supervisor(
            weak,
            0,
            spawn_ms,
            &stop_t,
            Duration::from_millis(1),
            Duration::from_millis(0),
            move || {
                let out = rows.take().unwrap_or_default();
                fetched_t.store(true, Ordering::Relaxed);
                out
            },
            |ev| {
                seen_t.lock().unwrap().push(ev);
                true
            },
            None,
        );
    });

    generation.fetch_add(1, Ordering::Relaxed); // the WS reconnect this lane fires on
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    while !fetched.load(Ordering::Relaxed) {
        assert!(Instant::now() < deadline, "the reconnect resync never fired");
        std::thread::sleep(Duration::from_millis(2));
    }
    // The emit loop runs after the fetch returns; give it the one hop it needs, then tear down
    // deterministically (drop the gen so the supervisor self-exits and we JOIN before reading).
    std::thread::sleep(Duration::from_millis(20));
    drop(generation);
    stop.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "supervisor did not exit");
        std::thread::sleep(Duration::from_millis(2));
    }
    handle.join().unwrap();

    seen.lock().unwrap().clone()
}

fn trade_ids(evs: &[Event]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.to_string()),
            _ => None,
        })
        .collect()
}

/// **The restart law, second half** (module doc). This lane does NOT replay on first connect
/// (`run_user_data_forever_with_idle`'s `opened_once` guard), which is exactly why it looked
/// safe and was not: it fires on every LATER reconnect, so a restart followed by a WS reconnect
/// hours on replays the row-count-bounded venue history — a window with no time bound at all —
/// into a process whose `seen_trade_ids` dedup is empty. `Account::apply_fill` is not
/// idempotent (`balance -= commission`, `realized_pnl +=`); measured on the CI box, each unexplained
/// equity step equalled the sum of PRIOR sessions' costs exactly.
#[test]
fn a_reconnect_resync_drops_fills_older_than_the_mount() {
    let now = vike_model::time::clock::now_ms();
    let seen = replay_once(
        now,
        vec![
            // a PREVIOUS session's fill — retained by the venue, never placed by this process
            fill_at("c_prev_session", "e_old", now - 3_600_000),
            // ...and the WS-gap fill this lane exists to recover
            fill_at("c_this_session", "e_new", now + 3_600_000),
        ],
    );
    let ids = trade_ids(&seen);
    assert!(
        !ids.iter().any(|t| t == "e_old"),
        "a fill stamped BEFORE the mount is a previous session's — it must never reach the \
             ingest, where apply_fill folds its fee and PnL a second time: {seen:?}"
    );
    assert!(
        ids.iter().any(|t| t == "e_new"),
        "a fill stamped after the mount is the reconnect-gap recovery this lane exists for and \
             must still be delivered: {seen:?}"
    );
}

/// **The half that matters: a legitimately OLD resting order still gets through.** Polymarket
/// is the consumer whose `resync_fetch` reads `get_orders` — live orders whose creation
/// legitimately predates the mount — and erring toward "dropped a live resting order" is far
/// worse than erring toward "re-booked a fill". The audit's answer is that its history path
/// stamps `ts: 0` on every event it can emit (`user_ws::decode_trade` reads only the WS
/// `timestamp` field, absent from the `/data/trades` REST rows, which carry `match_time`), and
/// `exec_actor::is_pre_spawn` lets an unstamped event ride through *by construction*.
///
/// This pins that end to end on the shared loop; `vike-polymarket`'s
/// `offline::resync_history_floor` pins the other end — that the REAL `map_polymarket_history`
/// over a REAL `/data/orders` resting-order row is what produces these shapes.
#[test]
fn an_unstamped_replay_row_rides_through_however_old_the_order_is() {
    let now = vike_model::time::clock::now_ms();
    let seen = replay_once(
        now,
        vec![
            // polymarket's cancel arm: `OrderCanceled { ts: 0 }`, for an order placed long ago
            Event::OrderCanceled(vike_model::events::OrderCanceled {
                client_order_id: "c_old_resting".to_string(),
                reason: String::new().into(),
                ts: 0,
            }),
            // ...and its terminal-fill arm, likewise unstamped
            fill_at("c_old_resting", "0xTRADE:0xORDER", 0),
        ],
    );
    assert!(
        seen.iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "c_old_resting")),
        "an UNSTAMPED event carries no evidence of being historical; dropping it would lose \
             live order state, the one direction that is worse than a double-booked fill: {seen:?}"
    );
    assert!(
        trade_ids(&seen).iter().any(|t| t == "0xTRADE:0xORDER"),
        "an unstamped fill must ride through too — no timestamp is ever fabricated: {seen:?}"
    );
}

/// The escape hatch, and the reason this stays a blanket default rather than a per-venue
/// opt-in: `spawn_ms = 0` is "no floor", so a future consumer that genuinely must replay
/// PRE-MOUNT state opts out AT ITS CALL SITE, visibly, instead of the floor being weakened for
/// the seven consumers that need it. Also the boundary a mutation would flip.
#[test]
fn a_zero_floor_replays_everything() {
    let now = vike_model::time::clock::now_ms();
    let seen = replay_once(0, vec![fill_at("c", "e_ancient", 1)]);
    assert_eq!(trade_ids(&seen), vec!["e_ancient".to_string()], "{seen:?}");
    // ...and the floor itself is exclusive at its own instant (the other mutation boundary).
    let seen = replay_once(now, vec![fill_at("c", "e_at_floor", now)]);
    assert_eq!(trade_ids(&seen), vec!["e_at_floor".to_string()], "{seen:?}");
}
