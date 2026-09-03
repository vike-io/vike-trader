//! Hyperliquid `userFills` **reconnect gap-repair** — the regression gate for a silent fill loss.
//!
//! THE BUG (fixed here): HL resends `isSnapshot` on every reconnect, and the fills that executed
//! while the socket was down exist ONLY in that snapshot — the pump has no `since`/watermark
//! re-request and HL streams no per-fill catch-up. The original `snapshot_seen` latch was created
//! ONCE per pump thread (outside the reliability loop) and never reset, so every snapshot after the
//! first returned `Vec::new()`: any reconnect-with-activity dropped its executed fills outright,
//! with no trace. Platform position and realized PnL then disagree with the venue silently — the
//! exact failure the venue-adapter contract's "no order may silently vanish" rule exists to stop.
//!
//! Two levels of proof, mirroring `bridges/bybit/tests/offline/fast_execution.rs`:
//!   * PUMP — the REAL `run_user_data_forever` loop driven by a scripted `UserStream` across a
//!     transport death. Session 2's snapshot carries a fill session 1 never saw; it must be
//!     emitted. NO socket, NO network.
//!   * ENGINE — the emitted stream folded through a REAL `ExecutionEngine`: the fill replayed in
//!     both snapshots books ONCE (`seen_trade_ids`, keyed on HL's `tid`) while the gap fill books,
//!     so the net position is the venue's truth and not a double count. This is what makes
//!     admitting the replay safe, and it is asserted rather than assumed.
//!
//! ## The FIRST snapshot is a different object, and it has a floor
//! Everything above is about the SECOND and later snapshots. The FIRST one is an arbitrary suffix of
//! ACCOUNT history, so it gets `map_frame_to_events`'s history floor — dropping a row only when it is
//! FOREIGN (no cloid this process minted) **and** stamped before the pump's spawn. The two tests at
//! the bottom of this file are the ones that arm it: the floor must not eat a gap fill (only the
//! FIRST snapshot is floored, and a gap fill can be stamped anywhere), and the floor must be the
//! THREAD's spawn rather than the instant of the first successful connect. Every other test here
//! passes `NO_FLOOR`, so the gap-repair proof above is byte-identical to before the floor existed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{json, Value};

// The shared scripted user-data stream double (testing-arch Phase 4c, `test-support` feature).
use vike_bridge_core::scripted::ScriptedUserStream as Scripted;
use vike_bridge_core::user_data::{run_user_data_forever, OpenOutcome, StreamError, StreamMsg};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate, RiskLimits,
};
use vike_hyperliquid::exec::CloidRegistry;
use vike_hyperliquid::symbology::Symbology;
use vike_hyperliquid::user_data::map_frame_to_events;
use vike_model::events::Event;

/// `spawn_ms == 0` — the no-floor hatch, so the gap-repair proofs below are exactly the behaviour
/// they were before the first-snapshot floor existed.
const NO_FLOOR: i64 = 0;

/// An ARMED floor. A fixed small epoch-ms value, not `now_ms()`: these tests need fills on both sides
/// of it with no sleeping, and any real wall clock is ~1.7e12, which is what makes a re-sampled floor
/// detectable rather than a rounding question.
const FLOOR: i64 = 1_000_000;

/// Perp-only symbology (`coin == symbol`), enough for BTC fills.
fn symbology() -> Symbology {
    Symbology::from_meta(
        &json!({"universe":[{"name":"BTC","szDecimals":5,"maxLeverage":40}]}),
        &json!({"tokens":[],"universe":[]}),
    )
}

/// One `userFills` fill row: BTC perp buy, `tid` as given.
fn fill(tid: u64, sz: &str, time: i64) -> Value {
    json!({"coin":"BTC","px":"50000","sz":sz,"side":"B","oid":1,"tid":tid,
           "fee":"0.5","feeToken":"USDC","crossed":true,"time":time})
}

/// A `userFills` snapshot frame (what HL sends on EVERY (re)subscribe), as a wire string.
fn snapshot(fills: Vec<Value>) -> String {
    json!({"channel":"userFills","data":{"isSnapshot":true,"fills":fills}}).to_string()
}

// ---- PUMP (the reliability loop across a reconnect) ---------------------------------------------

/// Drive the real loop over two scripted sessions and return every emitted event.
///
/// Session 1: the first-connect snapshot (tid 1), then a transport drop.
/// Session 2: the reconnect snapshot — HL replays tid 1 AND carries tid 2, which executed while the
/// socket was down. The script is then exhausted, which ends the loop.
fn emitted_across_a_reconnect() -> (Vec<Event>, usize) {
    emitted_across_a_reconnect_with_floor(NO_FLOOR, 3)
}

/// [`emitted_across_a_reconnect`] with the first-snapshot floor as a parameter. `stop_after` is how
/// many emitted events end the drive (an armed floor emits FEWER, so the count cannot be a literal).
fn emitted_across_a_reconnect_with_floor(spawn_ms: i64, stop_after: usize) -> (Vec<Event>, usize) {
    let symbology = symbology();
    let registry = CloidRegistry::new();
    // Created OUTSIDE the loop, exactly as `spawn_hyperliquid_user_data` does — this is the state
    // that made the bug survive reconnects, so the test must share it the same way.
    let snapshot_seen = AtomicBool::new(false);

    let stop = AtomicBool::new(false);
    let mut sessions = vec![
        Scripted::new(vec![
            Ok(StreamMsg::Text(snapshot(vec![fill(1, "0.1", 1)]))),
            Err(StreamError::Timeout),
            Err(StreamError::Closed("transport death".into())),
        ]),
        Scripted::new(vec![Ok(StreamMsg::Text(snapshot(vec![
            fill(1, "0.1", 1), // replayed — already folded
            fill(2, "0.3", 9), // executed DURING THE GAP; exists nowhere else
        ])))]),
    ];
    sessions.reverse(); // pop() takes session 1 first

    let mut opens = 0usize;
    let mut out: Vec<Event> = Vec::new();
    let stop_ref = &stop;
    let result = run_user_data_forever(
        || {
            opens += 1;
            match sessions.pop() {
                Some(s) => OpenOutcome::Ready(s),
                None => OpenOutcome::Stopped, // script done — end the loop
            }
        },
        |frame| map_frame_to_events(frame, &symbology, &registry, &snapshot_seen, spawn_ms),
        |event| {
            out.push(event);
            // Stop once the gap fill has landed, so an exhausted session 2 can't spin.
            if out.len() == stop_after {
                stop_ref.store(true, Ordering::Relaxed);
            }
            true
        },
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(4), // tiny backoff so the reconnect sleep is fast
        None,
        || {},
    );
    assert!(result.is_ok(), "no auth error on this venue's keyless user stream");
    (out, opens)
}

#[test]
fn a_fill_that_executed_while_the_socket_was_down_is_not_lost() {
    let (events, opens) = emitted_across_a_reconnect();
    assert_eq!(opens, 2, "transport death must reconnect exactly once");

    let tids: Vec<String> = events
        .iter()
        .map(|e| match e {
            Event::Fill(f) => f.trade_id.to_string(),
            other => panic!("userFills must emit only bare fills, got {other:?}"),
        })
        .collect();

    // The regression: with the old latch this was `["1"]` — session 2's whole frame was discarded,
    // so tid 2 (a REAL executed fill, executed while the socket was down) never existed anywhere in
    // the platform. It must now reach the ingest lane; the replayed tid 1 rides along and is
    // collapsed one layer down (asserted in the ENGINE test below).
    assert_eq!(tids, ["1", "1", "2"], "the reconnect snapshot is admitted in full: {tids:?}");
}

// ---- ENGINE (the replay is idempotent, so admitting it is safe) ---------------------------------

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "hyperliquid", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "hyperliquid",
        "BTC",
    )
}

#[test]
fn the_replayed_fill_books_once_and_the_gap_fill_books_too() {
    let (events, _) = emitted_across_a_reconnect();
    let mut eng = engine();
    let mut outbox = Outbox::default();
    for ev in &events {
        eng.on_event(ev, &mut outbox);
    }

    // tid 1 arrived TWICE (first-connect snapshot + reconnect replay) and books ONCE — the engine's
    // always-on `seen_trade_ids` guard, keyed on HL's `tid`. tid 2 is new and books. So:
    //   0.1 (tid 1, once) + 0.3 (tid 2) = 0.4 — the venue's true position.
    // A double count would read 0.5; the old latch's silent drop would read 0.1.
    assert!(
        (eng.position_size("BOTH") - 0.4).abs() < 1e-12,
        "replay collapsed, gap fill folded: got {}",
        eng.position_size("BOTH")
    );
}

/// The same proof stated as a pure invariant: re-delivering an ALREADY-folded snapshot any number
/// of times never moves the position. This is the property that lets the bridge stop guessing which
/// snapshot is a replay — it can push all of them and let the engine be the single dedup authority.
#[test]
fn re_folding_the_same_snapshot_is_idempotent_in_the_engine() {
    let symbology = symbology();
    let registry = CloidRegistry::new();
    let flag = AtomicBool::new(false);
    let frame: Value =
        serde_json::from_str(&snapshot(vec![fill(1, "0.1", 1), fill(2, "0.3", 9)])).unwrap();

    let mut eng = engine();
    let mut outbox = Outbox::default();
    for _ in 0..5 {
        for ev in &map_frame_to_events(&frame, &symbology, &registry, &flag, NO_FLOOR) {
            eng.on_event(ev, &mut outbox);
        }
    }
    assert!(
        (eng.position_size("BOTH") - 0.4).abs() < 1e-12,
        "five deliveries of the same two fills book exactly once each: got {}",
        eng.position_size("BOTH")
    );
}

// ---- THE ACCOUNT is the dedup authority (not this engine, and not this bridge) ------------------
//
// The three assertions above prove the OUTCOME (position 0.4, not 0.5). They passed while the guard
// lived in `ExecutionEngine::seen_trade_ids`, so they cannot tell you WHERE the replay was stopped —
// and "where" is the whole defect: this bridge is one of a growing set of paths that re-deliver an
// already-folded fill (`exec_actor::run_loop`'s history resync, `user_data::run_resync_supervisor`'s
// replay, this snapshot), each relying on a guard it cannot see. The tests below pin the guard's
// LOCATION by asserting on `Account`'s own refusal counter, so deleting the engine-side check cannot
// leave them green.

/// The replayed `tid` is refused BY THE ACCOUNT — the aggregate that owns the money — and the
/// refusal is visible to an operator instead of being an invisible `Fold::Dropped`.
#[test]
fn the_replayed_snapshot_fill_is_refused_by_the_account_itself() {
    let (events, _) = emitted_across_a_reconnect();
    let mut eng = engine();
    let mut outbox = Outbox::default();
    for ev in &events {
        eng.on_event(ev, &mut outbox);
    }

    assert_eq!(
        eng.account.duplicate_fills_refused, 1,
        "tid 1 arrived twice: the ACCOUNT must be what refused the second copy"
    );
    assert_eq!(
        eng.account.colliding_fills_refused, 0,
        "HL's `tid` is venue-global — a re-delivery, never an id collision"
    );
    // ⚠ This used to assert `untagged_fills_folded == 0` — "every HL fill row carries a `tid`, so
    // nothing rode the undedupable path". That counter is gone: `FillEvent::trade_id` is a `TradeId`
    // that cannot be empty (#1341), so the undedupable path does not exist to ride. The property is
    // stronger now and belongs to the type rather than to a runtime count, so there is nothing left
    // here to assert — and an assertion that can only ever hold is worse than its absence.
    assert_eq!(eng.seen_trade_ids().count(), 2, "the ledger holds tid 1 and tid 2, once each");
}

/// The invariant, stated as an EQUITY DELTA — the number the measured the CI box defect moved. Fold the
/// whole reconnect stream, then fold it again from the top: the second pass is pure re-delivery, so
/// equity must not move by a single bit, and every fill of it must be refused at the account.
#[test]
fn re_delivering_the_whole_reconnect_stream_moves_equity_by_bitwise_zero() {
    let (events, _) = emitted_across_a_reconnect();
    let mut eng = engine();
    let mut outbox = Outbox::default();
    for ev in &events {
        eng.on_event(ev, &mut outbox);
    }
    let equity_before = eng.account.equity_all(10_000.0);
    let refused_before = eng.account.duplicate_fills_refused;

    for ev in &events {
        eng.on_event(ev, &mut outbox);
    }

    assert_eq!(
        eng.account.equity_all(10_000.0).to_bits(),
        equity_before.to_bits(),
        "a whole re-delivered session must move equity by EXACTLY zero"
    );
    assert_eq!(
        eng.account.duplicate_fills_refused - refused_before,
        events.len() as u64,
        "and every one of the {} re-delivered fills must be refused at the ACCOUNT",
        events.len()
    );
}

// ---- THE FIRST-SNAPSHOT FLOOR, driven through the same real loop --------------------------------
//
// Both tests below arm the floor (`FLOOR`, not `NO_FLOOR`). They exist because the floor's hatch is
// `spawn_ms == 0`: every test above opts out of the new branch, so without these the guard would be
// shipped with no drive at all — the "gate that always returns 0" shape.

/// **The floor must not be able to eat a gap fill.** Only the FIRST snapshot is floored, and that is
/// the whole reason the two halves are separable: a gap fill's stamp is unconstrained relative to a
/// floor sampled at spawn (the socket can be down for minutes, and the fill can be a previous
/// session's resting order finally filling, hence FOREIGN too), so a floor applied to the reconnect
/// snapshot would delete exactly the row this file exists to protect.
///
/// The script is [`emitted_across_a_reconnect`]'s, unchanged, with both fills stamped BEFORE the
/// floor and neither carrying a cloid — i.e. the worst case for the floor. Session 1's first-connect
/// snapshot loses tid 1 (correctly: pre-start account history). Session 2's REPLAY keeps both, so the
/// gap fill tid 2 still reaches the ingest lane. Extending the floor to replay snapshots reddens this.
#[test]
fn an_armed_floor_still_lets_the_socket_down_fill_through() {
    let before = vike_hyperliquid::user_data::first_snapshot_fills_dropped();
    // The floored drive emits 2 events, not 3 — tid 1's first-connect copy is the dropped row.
    let (events, opens) = emitted_across_a_reconnect_with_floor(FLOOR, 2);
    assert_eq!(opens, 2, "transport death must reconnect exactly once");

    let tids: Vec<String> = events
        .iter()
        .map(|e| match e {
            Event::Fill(f) => f.trade_id.to_string(),
            other => panic!("userFills must emit only bare fills, got {other:?}"),
        })
        .collect();
    assert_eq!(
        tids,
        ["1", "2"],
        "the FIRST snapshot's history row is floored; the REPLAY is admitted in full, gap fill \
         included: {tids:?}"
    );
    // A strict advance rather than an exact count: the counter is process-global. Deleting the
    // increment still reddens this, which is the property being gated (no silent drop).
    assert!(
        vike_hyperliquid::user_data::first_snapshot_fills_dropped() > before,
        "the floored row must be COUNTED, not silently discarded"
    );
}

/// **The floor is the PUMP THREAD's spawn, not the instant of the first successful connect.**
///
/// `open_ws` returns [`OpenOutcome::Transport`] for a failed connect AND for a failed subscribe send,
/// and the loop then sleeps a doubling backoff and retries — so the first snapshot can arrive
/// arbitrarily late, while `vike_hyperliquid::exec`'s `run` has been draining `Submit` and placing
/// real orders over signed REST the whole time. A floor re-sampled at each open would sit AFTER those
/// fills, and on this venue they are in the first snapshot and NOWHERE else.
///
/// Script: open #1 fails at the transport (no socket, no snapshot, so the first-vs-replay latch is
/// still unset); open #2 delivers the FIRST snapshot, carrying a foreign fill stamped just AFTER the
/// spawn floor — the shape of a fill this process caused during the connect-failure window. It must
/// survive. Sampling the floor at the successful open instead reddens this: any real clock reading is
/// ~1.7e12, far above the fill's stamp.
///
/// ⚠ What this pins is the PROPERTY, at the seam a test can reach. The production sampling SITE is
/// `spawn_hyperliquid_user_data`'s thread body (the `let spawn_ms` beside `snapshot_seen`, outside the
/// loop), and covering that literally would need a real socket — there is no offline drive of that
/// function. The closure captures the binding by `Fn`, so re-sampling requires deliberately adding a
/// clock read inside it.
#[test]
fn the_floor_is_the_thread_spawn_not_the_first_successful_connect() {
    let symbology = symbology();
    let registry = CloidRegistry::new(); // nothing registered ⇒ the row is FOREIGN
    let snapshot_seen = AtomicBool::new(false);
    let stop = AtomicBool::new(false);

    // The FIRST snapshot, delivered on the SECOND open attempt, carrying a fill stamped after spawn.
    let mut sessions =
        vec![Scripted::new(vec![Ok(StreamMsg::Text(snapshot(vec![fill(1, "0.1", FLOOR + 1)])))])];

    let mut attempts = 0usize;
    let mut out: Vec<Event> = Vec::new();
    let stop_ref = &stop;
    let result = run_user_data_forever(
        || {
            attempts += 1;
            if attempts == 1 {
                // The connect (or the subscribe send) failed: backoff and retry, latch untouched.
                return OpenOutcome::Transport("connect refused".to_string());
            }
            match sessions.pop() {
                Some(s) => OpenOutcome::Ready(s),
                None => OpenOutcome::Stopped,
            }
        },
        |frame| map_frame_to_events(frame, &symbology, &registry, &snapshot_seen, FLOOR),
        |event| {
            out.push(event);
            stop_ref.store(true, Ordering::Relaxed);
            true
        },
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(4), // tiny backoff so the failed-open retry is fast
        None,
        || {},
    );
    assert!(result.is_ok(), "no auth error on this venue's keyless user stream");
    assert_eq!(attempts, 2, "the first open must have failed at the transport");

    let tids: Vec<String> = out
        .iter()
        .map(|e| match e {
            Event::Fill(f) => f.trade_id.to_string(),
            other => panic!("userFills must emit only bare fills, got {other:?}"),
        })
        .collect();
    assert_eq!(
        tids,
        ["1"],
        "a fill stamped after the THREAD's spawn survives a first snapshot that arrived on a later \
         connect — a floor sampled at that connect would have deleted it: {tids:?}"
    );
}
