//! Scripted-stream mapping — canned frames through [`map_frame_to_events`] (NO network) prove the
//! cloid→coid + coin→symbol remaps, the userFills snapshot latch (first-vs-replay) and the
//! first-snapshot history floor's four-way matrix.
use super::*;
use serde_json::json;

/// `spawn_ms == 0` — the no-floor hatch. Every test that predates the floor passes this, so it
/// exercises byte-identical behaviour; the floor's OWN tests pass [`FLOOR`] instead. (A gate every
/// caller opts out of is the shape this repo treats as a lie, hence the block at the bottom.)
const NO_FLOOR: i64 = 0;

/// A real, armed floor. A fixed small epoch-ms value rather than `now_ms()`: the matrix needs a
/// row on EACH side of it, and a wall-clock floor cannot have a row stamped after it without
/// sleeping. `1_000_000` is unambiguously positive, so `is_pre_spawn`'s `ts > 0` clause is never
/// what decides these cases.
const FLOOR: i64 = 1_000_000;

fn symbology() -> Symbology {
    let meta = json!({"universe":[{"name":"BTC","szDecimals":5,"maxLeverage":40}]});
    let spot = json!({
        "tokens":[
            {"name":"USDC","szDecimals":8,"index":0},
            {"name":"HYPE","szDecimals":2,"index":150}
        ],
        "universe":[{"name":"@107","tokens":[150,0],"index":107}]
    });
    Symbology::from_meta(&meta, &spot)
}

#[test]
fn order_update_remaps_cloid_to_the_framework_coid() {
    let symbology = symbology();
    let registry = CloidRegistry::new();
    let cloid = registry.register("vike-1"); // exec would have done this at submit
    let seen = AtomicBool::new(false);

    let frame = json!({"channel":"orderUpdates","data":[{
        "order":{"coin":"BTC","side":"B","limitPx":"50000","sz":"0.0","origSz":"0.1",
                 "oid":42,"cloid":cloid,"timestamp":1},
        "status":"open","statusTimestamp":2
    }]});
    let evs = map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR);
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        Event::OrderAccepted(a) => {
            assert_eq!(a.client_order_id, "vike-1", "cloid remapped to the framework coid");
            assert_eq!(a.venue_order_id.as_deref(), Some("42"));
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
}

#[test]
fn stale_cancel_of_a_modify_retired_oid_is_suppressed() {
    let symbology = symbology();
    let registry = CloidRegistry::new();
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
        [Event::OrderCanceled(c)] => assert_eq!(c.client_order_id, "vike-mod"),
        other => panic!("expected one OrderCanceled, got {other:?}"),
    }
}

#[test]
fn unknown_cloid_is_left_as_is() {
    let symbology = symbology();
    let registry = CloidRegistry::new(); // nothing registered
    let seen = AtomicBool::new(false);
    let frame = json!({"channel":"orderUpdates","data":[{
        "order":{"coin":"BTC","side":"B","limitPx":"50000","sz":"0.1","oid":7,"cloid":"0xdeadbeef","timestamp":1},
        "status":"open"
    }]});
    match &map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR)[0] {
        Event::OrderAccepted(a) => {
            assert_eq!(a.client_order_id, "0xdeadbeef", "foreign cloid untouched")
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
}

#[test]
fn user_fill_remaps_spot_coin_to_symbol_and_cloid_to_coid() {
    let symbology = symbology();
    let registry = CloidRegistry::new();
    let cloid = registry.register("vike-9");
    let seen = AtomicBool::new(false);

    // spot fill on the "@107" coin (HYPE/USDC), isSnapshot=false (an incremental fill).
    let frame = json!({"channel":"userFills","data":{"isSnapshot":false,"fills":[{
        "coin":"@107","px":"1.5","sz":"2.0","side":"B","oid":1,"cloid":cloid,
        "tid":900,"fee":"0.1","feeToken":"USDC","crossed":true,"time":5
    }]}});
    let evs = map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR);
    match &evs[0] {
        Event::Fill(f) => {
            assert_eq!(f.symbol, "HYPE/USDC", "spot coin @107 remapped to the unified symbol");
            assert_eq!(f.client_order_id, "vike-9", "cloid remapped to the framework coid");
            assert_eq!(f.trade_id, "900"); // tid keys the Account dedup
            assert_eq!(f.last_qty, 2.0);
        }
        other => panic!("expected Fill, got {other:?}"),
    }
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

/// THE REGRESSION PIN. HL resends `isSnapshot` on every reconnect, and the fills that executed
/// while the socket was down exist ONLY there. The old `snapshot_seen` latch returned
/// `Vec::new()` for every snapshot after the first, so those fills silently vanished. Both
/// snapshots must now yield their fills; the engine's `seen_trade_ids` dedups the overlap (the
/// end-to-end proof of that is `tests/hyperliquid_userdata_reconnect.rs`).
#[test]
fn reconnect_snapshot_still_yields_its_fills() {
    let symbology = symbology();
    let registry = CloidRegistry::new();
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
    let tids: Vec<String> = second
        .iter()
        .map(|e| match e {
            Event::Fill(f) => f.trade_id.to_string(),
            other => panic!("expected Fill, got {other:?}"),
        })
        .collect();
    assert_eq!(tids, ["1", "2"], "the reconnect snapshot is admitted in full, gap fill included");
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
    let registry = CloidRegistry::new();
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
    let registry = CloidRegistry::new();
    let seen = AtomicBool::new(true); // a snapshot has already been seen
    let frame = json!({"channel":"userFills","data":{"isSnapshot":false,
            "fills":[fill_row(json!(3), "0.1", 4), fill_row(Value::Null, "0.2", 5)]}});
    let evs = map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR);
    match &evs[..] {
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
    let registry = CloidRegistry::new();
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
// `fill_row` carries no `cloid`, so `map_one_fill`'s `order_coid` falls back to `oid` and the row
// is FOREIGN by construction against an empty registry. [`fill_row_cloid`] is its `ours` twin.

/// One BTC perp fill row carrying `cloid` — what a fill of an order THIS process submitted looks
/// like on the wire (exec stamps `keccak(coid)` on every order it places).
fn fill_row_cloid(tid: Value, sz: &str, time: i64, cloid: &str) -> Value {
    json!({"coin":"BTC","px":"50000","sz":sz,"side":"B","oid":1,"tid":tid,"cloid":cloid,
               "fee":"0.5","feeToken":"USDC","crossed":true,"time":time})
}

/// Every emitted fill's `tid`, in frame order (panics on any non-fill event).
fn tids(evs: &[Event]) -> Vec<String> {
    evs.iter()
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
    let symbology = symbology();
    let registry = CloidRegistry::new(); // nothing registered ⇒ every row is FOREIGN
    let seen = AtomicBool::new(false); // ⇒ this frame IS the first snapshot
    let before = first_snapshot_fills_dropped();

    let evs = map_frame_to_events(
        &snapshot_frame(json!([fill_row(json!(11), "0.1", FLOOR - 1)])),
        &symbology,
        &registry,
        &seen,
        FLOOR,
    );

    assert!(evs.is_empty(), "foreign AND pre-spawn is the combination that drops: {evs:?}");
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
    let symbology = symbology();
    let registry = CloidRegistry::new();
    let seen = AtomicBool::new(false);

    let evs = map_frame_to_events(
        &snapshot_frame(json!([fill_row(json!(12), "0.1", FLOOR + 1)])),
        &symbology,
        &registry,
        &seen,
        FLOOR,
    );
    assert_eq!(tids(&evs), ["12"], "a foreign fill AFTER the floor is live activity, not history");
}

/// (ours, pre-spawn) ⇒ admitted. **This is the row that reddens if the cloid half of the
/// conjunction is deleted, and it is the case a clock-only floor gets WRONG.**
///
/// It is reachable: the pump's first SUCCESSFUL connect is unsynchronised with everything —
/// `open_ws` returns `Transport` for a failed connect AND a failed subscribe send, the backoff
/// doubles to [`MAX_BACKOFF`], and `crate::exec`'s `run` is meanwhile draining `Submit` and
/// placing real orders over signed REST. Venue-vs-local skew widens the same window from the
/// other side, and on this venue skew is NOT bounded by a `recvWindow` (`crate::signing::hash`'s
/// `NonceManager`: `(T − 2 days, T + 1 day)`). Such a fill exists in this snapshot and NOWHERE
/// ELSE — HL streams no per-fill catch-up and this pump re-requests no window — so dropping it
/// would silently lose a fill, the very defect
/// `tests/hyperliquid_userdata_reconnect.rs`'s
/// `a_fill_that_executed_while_the_socket_was_down_is_not_lost` exists to prevent.
#[test]
fn our_own_pre_spawn_fill_is_admitted_from_the_first_snapshot() {
    let symbology = symbology();
    let registry = CloidRegistry::new();
    let cloid = registry.register("vike-race"); // exec did this at submit
    let seen = AtomicBool::new(false);

    let evs = map_frame_to_events(
        &snapshot_frame(json!([fill_row_cloid(json!(13), "0.1", FLOOR - 1, &cloid)])),
        &symbology,
        &registry,
        &seen,
        FLOOR,
    );
    assert_eq!(tids(&evs), ["13"], "a resolving cloid PROVES the order is ours — admit it");
    match &evs[0] {
        Event::Fill(f) => assert_eq!(
            f.client_order_id, "vike-race",
            "and it is still remapped to the framework coid, floor or no floor"
        ),
        other => panic!("expected Fill, got {other:?}"),
    }
}

/// (ours, post-spawn) ⇒ admitted — the ordinary live case, which neither half of the guard may
/// touch.
#[test]
fn our_own_post_spawn_fill_is_admitted_from_the_first_snapshot() {
    let symbology = symbology();
    let registry = CloidRegistry::new();
    let cloid = registry.register("vike-live");
    let seen = AtomicBool::new(false);

    let evs = map_frame_to_events(
        &snapshot_frame(json!([fill_row_cloid(json!(14), "0.1", FLOOR + 1, &cloid)])),
        &symbology,
        &registry,
        &seen,
        FLOOR,
    );
    assert_eq!(tids(&evs), ["14"]);
}

/// The whole matrix in ONE frame: the floor decides per ROW, so a snapshot mixing a previous
/// session's history with our own racing fills keeps everything except the history. A per-FRAME
/// drop (the shape the original `snapshot_seen` latch had) would lose tids 22-24.
#[test]
fn the_floor_decides_per_row_not_per_frame() {
    let symbology = symbology();
    let registry = CloidRegistry::new();
    let cloid = registry.register("vike-mixed");
    let seen = AtomicBool::new(false);

    let evs = map_frame_to_events(
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
    assert_eq!(tids(&evs), ["22", "23", "24"], "exactly the history row is dropped");
}

/// A REPLAY snapshot is never floored — even a row that is both foreign and pre-spawn. This is
/// the gap-repair frame: the fills that executed while the socket was down exist ONLY in it, they
/// can be stamped anywhere relative to a floor sampled at spawn, and "foreign" says nothing about
/// whether we need them (a fill of an order placed by an EARLIER session of this pump, or an
/// oid-only liquidation, is foreign). Dedup one layer down (`tid`) is what makes admitting it
/// safe, and that has not changed.
#[test]
fn a_replay_snapshot_is_never_floored() {
    let symbology = symbology();
    let registry = CloidRegistry::new();
    let seen = AtomicBool::new(true); // a snapshot has ALREADY been seen ⇒ this one is a replay

    let evs = map_frame_to_events(
        &snapshot_frame(json!([fill_row(json!(31), "0.1", FLOOR - 1)])),
        &symbology,
        &registry,
        &seen,
        FLOOR,
    );
    assert_eq!(
        tids(&evs),
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
    let registry = CloidRegistry::new();
    let seen = AtomicBool::new(false); // no snapshot seen yet, so `!replay` holds
    let frame = json!({"channel":"userFills","data":{"isSnapshot":false,
            "fills":[fill_row(json!(41), "0.1", FLOOR - 1)]}});

    let evs = map_frame_to_events(&frame, &symbology, &registry, &seen, FLOOR);
    assert_eq!(tids(&evs), ["41"], "an incremental fill is live, whatever it is stamped");
}

/// [`NO_FLOOR`] admits everything — the hatch every pre-floor test above rides, asserted here
/// rather than assumed, on the exact row an armed floor drops.
#[test]
fn spawn_ms_zero_is_no_floor_at_all() {
    let symbology = symbology();
    let registry = CloidRegistry::new();
    let seen = AtomicBool::new(false);

    let evs = map_frame_to_events(
        &snapshot_frame(json!([fill_row(json!(51), "0.1", 1)])),
        &symbology,
        &registry,
        &seen,
        NO_FLOOR,
    );
    assert_eq!(tids(&evs), ["51"], "`spawn_ms == 0` is the unfloored/probe configuration");
}

// --- restored orders (decision 0121): a cloid seeded by `ExecutionClient::adopt_restored` resolves
// --- exactly like one registered at submit, but is NOT proof the order is "ours" for the floor.

/// The cloid a previous session put on the wire for `coid`, and the registry a restart rebuilt it in.
fn restored(coid: &str) -> (CloidRegistry, String) {
    let registry = CloidRegistry::new();
    registry.register_restored(coid);
    (registry, crate::event_mapper::cloid_from_client_order_id(coid))
}

/// A restored cloid resolves on the order lane: the lifecycle event of an order a previous session
/// left resting (its `canceled`, here) reaches the core under the REAL coid, not the raw `0x…`.
#[test]
fn a_restored_cloid_resolves_to_its_coid_on_an_order_update() {
    let symbology = symbology();
    let (registry, cloid) = restored("vike-old-1");
    let seen = AtomicBool::new(false);
    let frame = json!({"channel":"orderUpdates","data":[{
        "order":{"coin":"BTC","side":"B","limitPx":"50000","sz":"0.1","oid":5,"cloid":cloid,"timestamp":1},
        "status":"canceled","statusTimestamp":2
    }]});
    match &map_frame_to_events(&frame, &symbology, &registry, &seen, NO_FLOOR)[..] {
        [Event::OrderCanceled(c)] => assert_eq!(c.client_order_id, "vike-old-1"),
        other => panic!("expected one OrderCanceled under the real coid, got {other:?}"),
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
    let evs = map_frame_to_events(
        &snapshot_frame(json!([fill_row_cloid(json!(21), "0.1", FLOOR - 1, &cloid)])),
        &symbology,
        &registry,
        &seen,
        FLOOR,
    );
    assert!(evs.is_empty(), "a previous session's fill is history, not this account's: {evs:?}");

    // A fill that happens now arrives incrementally: admitted, under the real coid.
    let live = json!({"channel":"userFills","data":{"isSnapshot":false,"fills":[
        fill_row_cloid(json!(22), "0.1", FLOOR + 5, &cloid)
    ]}});
    match &map_frame_to_events(&live, &symbology, &registry, &seen, FLOOR)[..] {
        [Event::Fill(f)] => assert_eq!(f.client_order_id, "vike-old-2"),
        other => panic!("a live fill of a restored order must be admitted, got {other:?}"),
    }

    // A reconnect snapshot is gap repair, never floored.
    let evs = map_frame_to_events(
        &snapshot_frame(json!([fill_row_cloid(json!(23), "0.1", FLOOR - 1, &cloid)])),
        &symbology,
        &registry,
        &seen,
        FLOOR,
    );
    assert_eq!(tids(&evs), ["23"], "a replay snapshot is admitted");
}
