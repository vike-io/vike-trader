//! Scripted-stream mapping — canned frames through [`map_frame_to_events`] (NO network) prove the
//! cloid→order client id + coin→symbol remaps, the userFills snapshot latch (first-vs-replay) and the
//! first-snapshot history floor's four-way matrix.
use super::*;
use crate::execution::spot::spot_tests::symbology;
use serde_json::json;
use std::time::Duration;
use vike_bridge_core::websocket_session::spec;
use vike_bridge_core::websocket_session::user::USER_DATA_POLL;

/// `spawn_ms == 0` — the no-floor hatch. Every test that predates the floor passes this, so it
/// exercises byte-identical behaviour; the floor's OWN tests pass [`FLOOR`] instead. (A gate every
/// caller opts out of is the shape this repo treats as a lie, hence the block at the bottom.)
const NO_FLOOR: i64 = 0;

/// A real, armed floor. A fixed small epoch-ms value rather than `now_ms()`: the matrix needs a
/// row on EACH side of it, and a wall-clock floor cannot have a row stamped after it without
/// sleeping. `1_000_000` is unambiguously positive, so `is_pre_spawn`'s `ts > 0` clause is never
/// what decides these cases.
const FLOOR: i64 = 1_000_000;

#[test]
fn order_update_remaps_cloid_to_the_framework_order_client_id() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let cloid = registry.register("vike-1"); // exec would have done this at submit
    let seen = AtomicBool::new(false);

    let frame = json!({"channel":"orderUpdates","data":[{
        "order":{"coin":"BTC","side":"B","limitPx":"50000","sz":"0.0","origSz":"0.1",
                 "oid":42,"cloid":cloid,"timestamp":1},
        "status":"open","statusTimestamp":2
    }]});
    let events = map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR);
    assert_eq!(events.len(), 1);
    match &events[0] {
        Event::OrderAccepted(a) => {
            assert_eq!(
                a.order_client_id, "vike-1",
                "cloid remapped to the framework order client id"
            );
            assert_eq!(a.venue_order_id.as_deref(), Some("42"));
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
}

#[test]
fn stale_cancel_of_a_modify_retired_oid_is_suppressed() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let cloid = registry.register("vike-mod");
    let seen = AtomicBool::new(false);

    // A native modify retired oid 42 (exec records this on the shared registry).
    registry.retire_oid(42);

    // HL streams a stale `canceled` for the RETIRED oid 42 (same cloid → the live re-placed
    // order): it must be DROPPED, not folded into an OrderCanceled that terminates the order.
    let stale = json!({"channel":"orderUpdates","data":[{
        "order":{"coin":"BTC","side":"B","limitPx":"50000","sz":"0.1","oid":42,"cloid":cloid,"timestamp":1},
        "status":"canceled","statusTimestamp":2
    }]});
    assert!(
        map_frame_to_events(&stale, &symbology, &registry, &seen, NO_FLOOR).is_empty(),
        "a canceled for a modify-retired oid is suppressed"
    );

    // A cancel for a DIFFERENT, live oid still folds normally (suppression is one-shot + by oid).
    let real = json!({"channel":"orderUpdates","data":[{
        "order":{"coin":"BTC","side":"B","limitPx":"50000","sz":"0.1","oid":99,"cloid":cloid,"timestamp":1},
        "status":"canceled","statusTimestamp":2
    }]});
    match &map_frame_to_events(&real, &symbology, &registry, &seen, NO_FLOOR)[..] {
        [Event::OrderCanceled(c)] => assert_eq!(c.order_client_id, "vike-mod"),
        other => panic!("expected one OrderCanceled, got {other:?}"),
    }
}

#[test]
fn unknown_cloid_is_left_as_is() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new(); // nothing registered
    let seen = AtomicBool::new(false);
    let frame = json!({"channel":"orderUpdates","data":[{
        "order":{"coin":"BTC","side":"B","limitPx":"50000","sz":"0.1","oid":7,"cloid":"0xdeadbeef","timestamp":1},
        "status":"open"
    }]});
    match &map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR)[0] {
        Event::OrderAccepted(a) => {
            assert_eq!(a.order_client_id, "0xdeadbeef", "foreign cloid untouched")
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
}

#[test]
fn user_fill_remaps_spot_coin_to_symbol_and_cloid_to_order_client_id() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let cloid = registry.register("vike-9");
    let seen = AtomicBool::new(false);

    // spot fill on the "@107" coin (HYPE/USDC), isSnapshot=false (an incremental fill).
    let frame = json!({"channel":"userFills","data":{"isSnapshot":false,"fills":[{
        "coin":"@107","px":"1.5","sz":"2.0","side":"B","oid":1,"cloid":cloid,
        "tid":900,"fee":"0.1","feeToken":"USDC","crossed":true,"time":5
    }]}});
    let events = map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR);
    match &events[0] {
        Event::Fill(f) => {
            assert_eq!(f.symbol, "HYPEUSDC", "spot coin @107 remapped to its user name");
            assert_eq!(
                f.order_client_id, "vike-9",
                "cloid remapped to the framework order client id"
            );
            assert_eq!(f.trade_id, "900"); // tid keys the Account dedup
            assert_eq!(f.last_qty, 2.0);
        }
        other => panic!("expected Fill, got {other:?}"),
    }
}

/// **A fill of an order with no `cloid` carries NO client id** (the spot rule, owner 2026-11-11): HL names such an
/// order (placed by hand in its app, or by a tool that sets none) by its `oid`, which would read as a client id.
/// Every order this workspace signs carries a cloid, so the fill is not ours, and the fold keeps a spot fill with
/// no id out of the strategy's position. A cloid this process does not know (another session's order) keeps its
/// hash, as before.
#[test]
fn a_fill_of_an_order_with_no_cloid_carries_no_client_id() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let seen = AtomicBool::new(false);
    let other = "0x00000000000000000000000000000abc";
    let frame = json!({"channel":"userFills","data":{"isSnapshot":false,"fills":[
        {"coin":"@107","px":"1.5","sz":"2.0","side":"A","oid":77,"tid":901,
         "fee":"0.1","feeToken":"USDC","crossed":true,"time":5},
        {"coin":"@107","px":"1.5","sz":"1.0","side":"A","oid":78,"cloid":other,"tid":902,
         "fee":"0.1","feeToken":"USDC","crossed":true,"time":5}
    ]}});
    let events = map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR);
    let order_client_ids: Vec<&str> = events
        .iter()
        .map(|e| match e {
            Event::Fill(f) => f.order_client_id.as_str(),
            other => panic!("expected a Fill, got {other:?}"),
        })
        .collect();
    assert_eq!(order_client_ids, ["", other]);
}

/// One `userFills` snapshot frame carrying `fills` verbatim.
fn snapshot_frame(fills: Value) -> Value {
    json!({"channel":"userFills","data":{"isSnapshot":true,"fills":fills}})
}

/// One BTC perp fill row; `tid` is `Value::Null` to model an untagged fill.
fn fill_row(tid: Value, sz: &str, time: i64) -> Value {
    json!({"coin":"BTC","px":"50000","sz":sz,"side":"B","oid":1,"tid":tid,
               "fee":"0.5","feeToken":"USDC","crossed":true,"time":time})
}

/// What a snapshot of the one FOREIGN row `tid` (nothing registered), stamped `time`, maps to under
/// `floor`; `seen` is the latch (`false`: this frame IS the first snapshot, `true`: it is a replay).
fn foreign_snapshot(tid: i64, time: i64, seen: bool, floor: i64) -> Vec<Event> {
    let frame = snapshot_frame(json!([fill_row(json!(tid), "0.1", time)]));
    map_frame_to_events(
        &frame,
        &symbology(),
        &OrderClientIdVenueRegistry::new(),
        &AtomicBool::new(seen),
        floor,
    )
}

/// THE REGRESSION PIN. HL resends `isSnapshot` on every reconnect, and the fills that executed
/// while the socket was down exist ONLY there. The old `snapshot_seen` latch returned
/// `Vec::new()` for every snapshot after the first, so those fills silently vanished. Both
/// snapshots must still yield every fill they have NOT shown before (the gap fill); the replayed
/// row is left out by the pump's ledger, and the engine's `seen_trade_ids` is the second line (the
/// end-to-end proof of both is `tests/hyperliquid_userdata_reconnect.rs`).
#[test]
fn reconnect_snapshot_still_yields_its_fills() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let seen = AtomicBool::new(false);

    // Session 1's snapshot: one fill, tid 1.
    let first = map_frame_to_events(
        &snapshot_frame(json!([fill_row(json!(1), "0.1", 1)])),
        &symbology,
        &registry,
        &seen,
        NO_FLOOR,
    );
    assert_eq!(first.len(), 1, "the FIRST snapshot is folded (unchanged)");

    // Session 2 (post-reconnect): HL replays tid 1 AND carries tid 2, which executed while the
    // socket was down. Both are emitted — dropping the frame would have lost tid 2 outright.
    let second = map_frame_to_events(
        &snapshot_frame(json!([fill_row(json!(1), "0.1", 1), fill_row(json!(2), "0.3", 9)])),
        &symbology,
        &registry,
        &seen,
        NO_FLOOR,
    );
    assert_eq!(tids(&second), ["2"], "the replayed tid 1 is left out; the gap fill is admitted");
}

/// **THE PROD2 SHAPE: history the first snapshot floored must not come back with the reconnect.**
/// Two foreign pre-spawn rows (the testnet account's two 2026-07-26 BTC fills) are dropped from the
/// first snapshot; every later reconnect replays them, and being unfloored they used to be admitted
/// — into a core that dropped them as UNATTRIBUTED at `warn!` every ~11 minutes (or, on a box with
/// one account on the symbol, folded them: the phantom position the floor exists to stop). A
/// genuinely new row in the same replay still passes, and so does an INCREMENTAL row, which is
/// never filtered by the ledger.
#[test]
fn a_row_the_floor_dropped_is_not_admitted_by_a_later_reconnect_snapshot() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let seen = AtomicBool::new(false);
    let history =
        || vec![fill_row(json!(61), "0.005", FLOOR - 2), fill_row(json!(62), "0.005", FLOOR - 1)];

    let first =
        map_frame_to_events(&snapshot_frame(json!(history())), &symbology, &registry, &seen, FLOOR);
    assert!(first.is_empty(), "foreign AND pre-spawn: floored: {first:?}");

    let mut replay = history();
    replay.push(fill_row(json!(63), "0.1", FLOOR + 5)); // executed while the socket was down
    let second =
        map_frame_to_events(&snapshot_frame(json!(replay)), &symbology, &registry, &seen, FLOOR);
    assert_eq!(tids(&second), ["63"], "the floored history stays out; the gap fill gets in");

    // The ledger never filters an incremental frame: a repeat there is the engine's to collapse.
    let live = json!({"channel":"userFills","data":{"isSnapshot":false,
            "fills":[fill_row(json!(63), "0.1", FLOOR + 5)]}});
    assert_eq!(tids(&map_frame_to_events(&live, &symbology, &registry, &seen, FLOOR)), ["63"]);
}

/// A fill first seen on an INCREMENTAL frame is recorded too, so the replay of it is left out.
#[test]
fn a_live_fill_is_not_replayed_by_the_next_reconnect_snapshot() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let seen = AtomicBool::new(true); // a first snapshot has long been seen
    let live = json!({"channel":"userFills","data":{"isSnapshot":false,
            "fills":[fill_row(json!(71), "0.1", FLOOR + 1)]}});
    assert_eq!(tids(&map_frame_to_events(&live, &symbology, &registry, &seen, FLOOR)), ["71"]);

    let replay = snapshot_frame(json!([fill_row(json!(71), "0.1", FLOOR + 1)]));
    assert!(map_frame_to_events(&replay, &symbology, &registry, &seen, FLOOR).is_empty());
}

/// An untagged fill is dropped on EVERY frame — first connect, replay snapshot and incremental
/// alike — because `FillEvent.trade_id` is a `TradeId` and cannot hold `""`, so
/// `map_user_fills` refuses the row before this module sees it.
///
/// ⚠ This is a deliberate TIGHTENING of the previous behaviour, which admitted an untagged fill
/// on first connect and dropped it only from a replay. That split bought nothing: the engine's
/// dedup key would be absent either way, so the first-connect admission merely deferred the
/// double-count to the first reconnect — and the fill it "saved" was the un-dedupable one. A
/// tagged fill in the same frame is unaffected, which is what keeps the drop per-row.
#[test]
fn an_untagged_fill_is_dropped_on_every_frame_not_just_a_replay() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let seen = AtomicBool::new(false);

    let first = map_frame_to_events(
        &snapshot_frame(json!([fill_row(Value::Null, "0.1", 1)])),
        &symbology,
        &registry,
        &seen,
        NO_FLOOR,
    );
    assert!(
        first.is_empty(),
        "even a FIRST-connect untagged fill is refused — it could never be deduped"
    );

    let second = map_frame_to_events(
        &snapshot_frame(json!([fill_row(Value::Null, "0.1", 1), fill_row(json!(7), "0.2", 5)])),
        &symbology,
        &registry,
        &seen,
        NO_FLOOR,
    );
    match &second[..] {
        [Event::Fill(f)] => assert_eq!(f.trade_id, "7", "only the DEDUPABLE fill survives"),
        other => panic!("expected exactly the tagged fill, got {other:?}"),
    }
}

/// An INCREMENTAL (`isSnapshot:false`) frame is never touched by the snapshot flag, before or
/// after a snapshot has been seen: its TAGGED fills always pass through. Its untagged ones do
/// not — that refusal is the mapper's and is frame-kind blind (see the test above).
#[test]
fn incremental_fills_are_untouched_by_the_snapshot_flag() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let seen = AtomicBool::new(true); // a snapshot has already been seen
    let frame = json!({"channel":"userFills","data":{"isSnapshot":false,
            "fills":[fill_row(json!(3), "0.1", 4), fill_row(Value::Null, "0.2", 5)]}});
    let events = map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR);
    match &events[..] {
        [Event::Fill(f)] => assert_eq!(
            f.trade_id, "3",
            "the tagged incremental fill passes through; the untagged one is refused"
        ),
        other => panic!("expected exactly the tagged fill, got {other:?}"),
    }
}

#[test]
fn non_data_frames_yield_nothing() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let seen = AtomicBool::new(false);
    for frame in [json!({"channel":"pong"}), json!({"channel":"subscriptionResponse","data":{}})] {
        assert!(map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR).is_empty());
    }
}

// ---- the FIRST-snapshot history floor -------------------------------------------------------
//
// Every test above passes [`NO_FLOOR`], so none of them enters the branch below — which is
// exactly why these exist. The floor is a CONJUNCTION (FOREIGN **and** pre-spawn), so pinning it
// takes the four-way matrix, and each case gets its OWN test with ONE row in the frame: a single
// row means a missing half of the guard SUCCEEDS at producing the wrong answer instead of being
// masked by a sibling row or hidden behind a short-circuiting earlier assert. The mixed frame at
// the end then proves the decision is per-ROW.
//
// `fill_row` carries no `cloid`, so `map_one_fill`'s `order_order_client_id` falls back to `oid` and the row
// is FOREIGN by construction against an empty registry. [`fill_row_cloid`] is its `ours` twin.

/// One BTC perp fill row carrying `cloid` — what a fill of an order THIS process submitted looks
/// like on the wire (exec stamps `keccak(order client id)` on every order it places).
fn fill_row_cloid(tid: Value, sz: &str, time: i64, cloid: &str) -> Value {
    json!({"coin":"BTC","px":"50000","sz":sz,"side":"B","oid":1,"tid":tid,"cloid":cloid,
               "fee":"0.5","feeToken":"USDC","crossed":true,"time":time})
}

/// Every emitted fill's `tid`, in frame order (panics on any non-fill event).
fn tids(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Event::Fill(f) => f.trade_id.to_string(),
            other => panic!("userFills must emit only bare fills, got {other:?}"),
        })
        .collect()
}

/// (foreign, pre-spawn) ⇒ **DROPPED**. The one combination that drops: someone else's activity
/// from before this process existed. Folding it would subtract a previous session's commission
/// from a zero-based `balance` and open a phantom position of whatever sign the history suffix
/// happens to start on.
#[test]
fn a_foreign_pre_spawn_fill_is_dropped_from_the_first_snapshot_and_counted() {
    let before = first_snapshot_fills_dropped();
    let events = foreign_snapshot(11, FLOOR - 1, false, FLOOR);
    assert!(events.is_empty(), "foreign AND pre-spawn is the combination that drops: {events:?}");
    // A STRICT advance rather than `== before + 1`: the counter is process-global, so a strict
    // inequality holds whatever else a sibling test in this binary is doing concurrently, while
    // still reddening if the increment is deleted (a drop on the money lane is never silent).
    assert!(first_snapshot_fills_dropped() > before, "the drop must be COUNTED as well as logged");
}

/// (foreign, post-spawn) ⇒ admitted. **This is the row that reddens if the timestamp half of the
/// conjunction is deleted.** A fill on this address that executed while we were running is live
/// account activity, not history — the floor's subject is WHEN, and authorship alone must never
/// drop anything (HL's `userFills` is account-wide and the engine gates on SYMBOL, not cloid;
/// that was true before this floor existed and is deliberately unchanged).
#[test]
fn a_foreign_post_spawn_fill_is_admitted_from_the_first_snapshot() {
    let events = foreign_snapshot(12, FLOOR + 1, false, FLOOR);
    assert_eq!(
        tids(&events),
        ["12"],
        "a foreign fill AFTER the floor is live activity, not history"
    );
}

/// (ours, pre-spawn) ⇒ admitted. **This is the row that reddens if the cloid half of the
/// conjunction is deleted, and it is the case a clock-only floor gets WRONG.**
///
/// It is reachable: the pump's first SUCCESSFUL connect is unsynchronised with everything —
/// `open_websocket` returns `Transport` for a failed connect AND a failed subscribe send, the backoff
/// doubles to the exec lane's 30 s ceiling, and `crate::execution`'s `run` is meanwhile draining `Submit`
/// and placing real orders over signed REST. Venue-vs-local skew widens the same window from the
/// other side, and on this venue skew is NOT bounded by a `recvWindow` (`crate::signing::hash`'s
/// `NonceManager`: `(T − 2 days, T + 1 day)`). Such a fill exists in this snapshot and NOWHERE
/// ELSE — HL streams no per-fill catch-up and this pump re-requests no window — so dropping it
/// would silently lose a fill, the very defect
/// `tests/hyperliquid_userdata_reconnect.rs`'s
/// `a_fill_that_executed_while_the_socket_was_down_is_not_lost` exists to prevent.
#[test]
fn our_own_pre_spawn_fill_is_admitted_from_the_first_snapshot() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let cloid = registry.register("vike-race"); // exec did this at submit
    let seen = AtomicBool::new(false);

    let events = map_frame_to_events(
        &snapshot_frame(json!([fill_row_cloid(json!(13), "0.1", FLOOR - 1, &cloid)])),
        &symbology,
        &registry,
        &seen,
        FLOOR,
    );
    assert_eq!(tids(&events), ["13"], "a resolving cloid PROVES the order is ours — admit it");
    match &events[0] {
        Event::Fill(f) => assert_eq!(
            f.order_client_id, "vike-race",
            "and it is still remapped to the framework order client id, floor or no floor"
        ),
        other => panic!("expected Fill, got {other:?}"),
    }
}

/// (ours, post-spawn) ⇒ admitted — the ordinary live case, which neither half of the guard may
/// touch.
#[test]
fn our_own_post_spawn_fill_is_admitted_from_the_first_snapshot() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let cloid = registry.register("vike-live");
    let seen = AtomicBool::new(false);

    let events = map_frame_to_events(
        &snapshot_frame(json!([fill_row_cloid(json!(14), "0.1", FLOOR + 1, &cloid)])),
        &symbology,
        &registry,
        &seen,
        FLOOR,
    );
    assert_eq!(tids(&events), ["14"]);
}

/// The whole matrix in ONE frame: the floor decides per ROW, so a snapshot mixing a previous
/// session's history with our own racing fills keeps everything except the history. A per-FRAME
/// drop (the shape the original `snapshot_seen` latch had) would lose tids 22-24.
#[test]
fn the_floor_decides_per_row_not_per_frame() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let cloid = registry.register("vike-mixed");
    let seen = AtomicBool::new(false);

    let events = map_frame_to_events(
        &snapshot_frame(json!([
            fill_row(json!(21), "0.1", FLOOR - 1), // foreign + pre  → dropped
            fill_row(json!(22), "0.2", FLOOR + 1), // foreign + post → admitted
            fill_row_cloid(json!(23), "0.3", FLOOR - 1, &cloid), // ours + pre     → admitted
            fill_row_cloid(json!(24), "0.4", FLOOR + 1, &cloid), // ours + post    → admitted
        ])),
        &symbology,
        &registry,
        &seen,
        FLOOR,
    );
    assert_eq!(tids(&events), ["22", "23", "24"], "exactly the history row is dropped");
}

/// A REPLAY snapshot is never floored — even a row that is both foreign and pre-spawn. This is
/// the gap-repair frame: the fills that executed while the socket was down exist ONLY in it, they
/// can be stamped anywhere relative to a floor sampled at spawn, and "foreign" says nothing about
/// whether we need them (a fill of an order placed by an EARLIER session of this pump, or an
/// oid-only liquidation, is foreign). Dedup one layer down (`tid`) is what makes admitting it
/// safe, and that has not changed.
#[test]
fn a_replay_snapshot_is_never_floored() {
    let events = foreign_snapshot(31, FLOOR - 1, true, FLOOR);
    assert_eq!(
        tids(&events),
        ["31"],
        "the floor applies to the FIRST snapshot only — a replay carries the socket-down gap"
    );
}

/// An INCREMENTAL frame is never floored either, and this is not the same statement as the test
/// above: `replay` is `false` for every `isSnapshot:false` frame (the `&&` short-circuits before
/// the latch is even read), so dropping the `uf.is_snapshot &&` conjunct would floor EVERY live
/// fill for the life of the process, forever, not just one frame.
#[test]
fn an_incremental_frame_is_never_floored() {
    let symbology = symbology();
    let registry = OrderClientIdVenueRegistry::new();
    let seen = AtomicBool::new(false); // no snapshot seen yet, so `!replay` holds
    let frame = json!({"channel":"userFills","data":{"isSnapshot":false,
            "fills":[fill_row(json!(41), "0.1", FLOOR - 1)]}});

    let events = map_frame_to_events(&frame, &symbology, &registry, &seen, FLOOR);
    assert_eq!(tids(&events), ["41"], "an incremental fill is live, whatever it is stamped");
}

/// [`NO_FLOOR`] admits everything — the hatch every pre-floor test above rides, asserted here
/// rather than assumed, on the exact row an armed floor drops.
#[test]
fn spawn_ms_zero_is_no_floor_at_all() {
    let events = foreign_snapshot(51, 1, false, NO_FLOOR);
    assert_eq!(tids(&events), ["51"], "`spawn_ms == 0` is the unfloored/probe configuration");
}

// --- restored orders (decision 0121): a cloid seeded by `ExecutionClient::adopt_restored` resolves
// --- exactly like one registered at submit, but is NOT proof the order is "ours" for the floor.

/// The cloid a previous session put on the wire for `order_client_id`, and the registry a restart rebuilt it in.
fn restored(order_client_id: &str) -> (OrderClientIdVenueRegistry, String) {
    let registry = OrderClientIdVenueRegistry::new();
    registry.register_restored(order_client_id);
    (registry, crate::execution::event_mapper::cloid_from_order_client_id(order_client_id))
}

/// A restored cloid resolves on the order lane: the lifecycle event of an order a previous session
/// left resting (its `canceled`, here) reaches the core under the REAL order client id, not the raw `0x…`.
#[test]
fn a_restored_cloid_resolves_to_its_order_client_id_on_an_order_update() {
    let symbology = symbology();
    let (registry, cloid) = restored("vike-old-1");
    let seen = AtomicBool::new(false);
    let frame = json!({"channel":"orderUpdates","data":[{
        "order":{"coin":"BTC","side":"B","limitPx":"50000","sz":"0.1","oid":5,"cloid":cloid,"timestamp":1},
        "status":"canceled","statusTimestamp":2
    }]});
    match &map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR)[..] {
        [Event::OrderCanceled(c)] => assert_eq!(c.order_client_id, "vike-old-1"),
        other => panic!("expected one OrderCanceled under the real order client id, got {other:?}"),
    }
}

/// **The adopt-before-the-first-snapshot hazard, closed.** The pass that adopts an order may run
/// BEFORE the pump's first `userFills` snapshot. Were a restored cloid read as "ours", that
/// snapshot's pre-spawn rows of the PREVIOUS session would fold into a fresh `Account` (zero
/// balance, empty dedup ledger) — the phantom position and double-charged commission the floor
/// exists to stop. So a restored cloid is remapped but still FLOORED on the first snapshot, and
/// only there: its live fills (an incremental frame) and a reconnect snapshot are admitted.
#[test]
fn a_restored_orders_history_is_floored_but_its_live_fills_are_admitted() {
    let symbology = symbology();
    let (registry, cloid) = restored("vike-old-2");
    let seen = AtomicBool::new(false);

    // First snapshot: a previous session's fill (pre-spawn) of the restored order is history.
    let events = map_frame_to_events(
        &snapshot_frame(json!([fill_row_cloid(json!(21), "0.1", FLOOR - 1, &cloid)])),
        &symbology,
        &registry,
        &seen,
        FLOOR,
    );
    assert!(
        events.is_empty(),
        "a previous session's fill is history, not this account's: {events:?}"
    );

    // A fill that happens now arrives incrementally: admitted, under the real order client id.
    let live = json!({"channel":"userFills","data":{"isSnapshot":false,"fills":[
        fill_row_cloid(json!(22), "0.1", FLOOR + 5, &cloid)
    ]}});
    match &map_frame_to_events(&live, &symbology, &registry, &seen, FLOOR)[..] {
        [Event::Fill(f)] => assert_eq!(f.order_client_id, "vike-old-2"),
        other => panic!("a live fill of a restored order must be admitted, got {other:?}"),
    }

    // A reconnect snapshot is gap repair, never floored.
    let events = map_frame_to_events(
        &snapshot_frame(json!([fill_row_cloid(json!(23), "0.1", FLOOR - 1, &cloid)])),
        &symbology,
        &registry,
        &seen,
        FLOOR,
    );
    assert_eq!(tids(&events), ["23"], "a replay snapshot is admitted");
}

/// The cadences [`spawn_hyperliquid_user_data`] hands the shared pump, as EFFECTIVE values. The
/// ping and the silent-stall watchdog are the exec lane of `venue_timings::HYPERLIQUID` (read
/// inline at the call), and so is the reconnect ceiling; the poll is `vike_bridge_core::websocket_session::user`'s
/// pump default. A retune of any of them is a deliberate edit to the literal here, in a diff that
/// says so.
#[test]
fn the_pump_runs_on_the_declared_cadences() {
    let lane = venue_timings::HYPERLIQUID.exec();
    assert_eq!(lane.keepalive_every(), Duration::from_secs(30), "app-level ping");
    assert_eq!(lane.idle_threshold(), Duration::from_secs(120), "silent-stall watchdog, 4 x ping");
    assert_eq!(USER_DATA_POLL, Duration::from_secs(1), "stop-poll / socket read timeout");
    assert_eq!(lane.reconnect().max, Duration::from_secs(30), "reconnect ceiling");
}

/// `crate::transport::consts::WEBSOCKET_PING_SECS` no longer feeds this pump but STAYS, for the market pump:
/// `spec::row`'s hyperliquid row declares the same 30 s ping, and its doc cites this
/// constant. One ping cadence, three places that state it, held equal here.
///
/// ⚠ Only the PING is one fact. The exec lane's idle threshold (120 s, 4 x the ping) and the market
/// pump row's (60 s) are two different watchdogs and are deliberately NOT pinned to each other.
#[test]
fn websocket_ping_secs_is_the_execution_lanes_keepalive_and_the_market_pump_rows() {
    let ping = Duration::from_secs(crate::transport::consts::WEBSOCKET_PING_SECS);
    assert_eq!(venue_timings::HYPERLIQUID.exec().keepalive_every(), ping, "exec lane row");
    assert_eq!(spec::row(VENUE).keepalive_every, Some(ping), "market pump row");
}

// --- a perp the engine was not mounted on (decision 0174: the account trades ANY perp) --------------

/// A symbology whose universe holds a case-sensitive coin the mount never traded.
fn symbology_with_kpepe() -> Symbology {
    let meta = json!({"universe":[
        {"name":"BTC","szDecimals":5,"maxLeverage":40},
        {"name":"kPEPE","szDecimals":0,"maxLeverage":10}
    ]});
    Symbology::from_meta(&meta, &json!({}))
}

/// **A fill of a perp the engine was never mounted on folds as a bare fill on its own symbol.**
/// `userFills` rows carry no `cloid` on this venue's account-wide stream (the docs list none), so the
/// order key falls back to the `oid` and the fill is keyed by the symbol and the `tid` alone. The
/// trade id stays the BARE `tid`: HL's reconcile read is account-wide and bare too, so a symbol-
/// qualified id (the binance and aster shape) would make every fill a missing fill there. The fill is
/// named by the coin's USER name (`KPEPEUSDC.P`, decision 0290), the string the engine keys the position
/// by; the venue's case-kept `kPEPE` stays on the wire.
#[test]
fn a_foreign_perp_fill_without_a_cloid_folds_on_its_own_symbol_with_the_bare_tid() {
    let symbology = symbology_with_kpepe();
    let registry = OrderClientIdVenueRegistry::new(); // this process minted nothing for the fill
    let seen = AtomicBool::new(false);
    let frame = json!({"channel":"userFills","data":{"isSnapshot":false,"fills":[{
        "coin":"kPEPE","px":"0.0123","sz":"1000","side":"B","oid":77,"tid":4242,
        "fee":"0.02","feeToken":"USDC","crossed":true,"time":5
    }]}});
    let events = map_frame_to_events(&frame, &symbology, &registry, &seen, FLOOR);
    match &events[..] {
        [Event::Fill(f)] => {
            assert_eq!(f.symbol, "KPEPEUSDC.P", "the coin kPEPE under its user name");
            assert_eq!(f.trade_id, "4242", "the bare tid, never `KPEPEUSDC.P:4242`");
            assert_eq!(f.order_client_id, "77", "no cloid: the oid, which no registry resolves");
            assert_eq!((f.last_qty, f.last_price, f.side), (1000.0, 0.0123, 1));
        }
        other => panic!("expected one Fill, got {other:?}"),
    }
}

/// The restart law still holds for a foreign perp: its history in the first snapshot is dropped
/// (nothing of ours, stamped before the spawn), exactly like the mounted symbol's.
#[test]
fn a_foreign_perp_pre_spawn_snapshot_row_is_floored_like_any_other() {
    let symbology = symbology_with_kpepe();
    let registry = OrderClientIdVenueRegistry::new();
    let seen = AtomicBool::new(false);
    let row = json!({"coin":"kPEPE","px":"0.01","sz":"5","side":"B","oid":1,"tid":9,
                     "fee":"0.0","feeToken":"USDC","crossed":true,"time":FLOOR - 1});
    let events =
        map_frame_to_events(&snapshot_frame(json!([row])), &symbology, &registry, &seen, FLOOR);
    assert!(events.is_empty(), "history of a coin we never mounted is not folded: {events:?}");
}

/// The reopen's reconcile nudge waits for the new session's `userFills` snapshot: nothing on the first connect, nothing
/// at the re-open or on a frame before the snapshot, one on the snapshot, and none on a later one of the same session.
#[test]
fn the_reconnect_nudge_follows_the_new_sessions_snapshot_never_the_reopen() {
    let (trigger, nudged) = std::sync::mpsc::channel();
    let nudge = ReconnectNudge::new(Some(trigger));
    let snapshot = snapshot_frame(json!([]));
    nudge.decoded(&snapshot);
    assert_eq!(nudged.try_iter().count(), 0, "the first connect has no gap to reconcile");
    nudge.reopened();
    assert_eq!(
        nudged.try_iter().count(),
        0,
        "nudged at the re-open, before the venue re-sent anything"
    );
    nudge.decoded(&json!({"channel":"orderUpdates","data":[]}));
    nudge.decoded(&json!({"channel":"userFills","data":{"isSnapshot":false,"fills":[]}}));
    assert_eq!(nudged.try_iter().count(), 0, "nudged on a frame that is not the snapshot");
    nudge.decoded(&snapshot);
    assert_eq!(nudged.try_iter().count(), 1, "the re-open's snapshot sends one nudge");
    nudge.decoded(&snapshot);
    assert_eq!(nudged.try_iter().count(), 0, "one re-open, one nudge");
}
