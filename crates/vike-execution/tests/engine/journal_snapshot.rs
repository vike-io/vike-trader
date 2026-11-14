//! `Engine::journal_snapshot`, the journal `Snapshot`'s form, is bounded by what is LIVE and loses nothing a
//! restart needs: every order that has not ended rides it whatever its age, an ended one only near the newest
//! order; the trade ids ride as a window of days with a FLOOR, and a ledger restored from it refuses every
//! re-delivery of a fill the window dropped while it books every new fill and every fill a reconcile pass may
//! still report (`vike_execution::account::fill_window`). The money moves never twice.

use vike_execution::account::fill_window::{DAY_MS, FILL_DEDUP_WINDOW_DAYS, day_of, floor_ms};
use vike_execution::testing::{RecordingClient, engine_on};
use vike_execution::{
    ENDED_ORDER_KEEP_MS, Engine, EngineSnapshot, EventBusOutbox, EventHandler, FillDrop,
};
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::{OrderRecord, OrderRequest, OrderStatus, OrderType};

/// Day 20,000 of the epoch (2024-10-04), the first fold.
const T0: i64 = 20_000 * DAY_MS;

fn engine() -> Engine<RecordingClient> {
    engine_on("sim", "BTCUSDT", RecordingClient::default())
}

fn order(order_client_id: &str, status: OrderStatus, created_ms: Option<i64>) -> OrderRecord {
    let req = OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    };
    let mut mo = OrderRecord::new(req);
    mo.status = status;
    mo.created_ms = created_ms;
    mo
}

fn fill(n: u64, side: i32, px: f64, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::prefixed("t-", n),
        order_client_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: 1.0,
        last_price: px,
        commission: 0.25,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: ts,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

/// Fold `f` on `e` with the engine clock at `now`, as the core's dispatch stamps it.
fn fold(e: &mut Engine<RecordingClient>, f: &FillEvent, now: i64) {
    e.now_ms = now;
    e.on_event(&Event::Fill(f.clone()), &mut EventBusOutbox::default());
}

/// The account's money, bit for bit.
fn money(e: &Engine<RecordingClient>) -> (u64, u64, u64, u64) {
    let a = &e.account;
    let size = a.positions.values().map(|p| p.size).sum::<f64>();
    (a.balance.to_bits(), a.realized_pnl.to_bits(), a.fees_paid.to_bits(), size.to_bits())
}

#[test]
fn the_journal_snapshot_keeps_every_order_that_has_not_ended_and_only_the_recent_ended_ones() {
    let mut e = engine();
    let old = T0;
    let newest = T0 + 100 * DAY_MS;
    for (order_client_id, status, created) in [
        ("old-working", OrderStatus::Accepted, Some(old)),
        ("old-unsent", OrderStatus::Initialized, Some(old)),
        ("old-pending-cancel", OrderStatus::PendingCancel, Some(old)),
        ("old-partial", OrderStatus::PartiallyFilled, Some(old)),
        ("old-filled", OrderStatus::Filled, Some(old)),
        ("old-canceled", OrderStatus::Canceled, Some(old)),
        ("old-rejected", OrderStatus::Rejected, Some(old)),
        ("adopted-live", OrderStatus::Accepted, None),
        ("adopted-ended", OrderStatus::Canceled, None),
        ("edge-filled", OrderStatus::Filled, Some(newest - ENDED_ORDER_KEEP_MS)),
        ("late-filled", OrderStatus::Filled, Some(newest - ENDED_ORDER_KEEP_MS - 1)),
        ("newest-filled", OrderStatus::Filled, Some(newest)),
    ] {
        e.registry.insert(order_client_id.into(), order(order_client_id, status, created));
    }
    let snap = e.journal_snapshot();
    let kept: Vec<&str> = snap.registry.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(
        kept,
        [
            "old-working",
            "old-unsent",
            "old-pending-cancel",
            "old-partial",
            "adopted-live",
            "edge-filled",
            "newest-filled"
        ],
        "every order that has not ended, in registry order, and the ended ones near the newest"
    );
    assert_eq!(snap.ended_orders_left_out, 5);
    let left: Vec<&str> = e.journal_left_out().map(|(c, _)| c).collect();
    assert_eq!(
        left,
        ["old-filled", "old-canceled", "old-rejected", "adopted-ended", "late-filled"]
    );
    assert_eq!(e.snapshot_state().registry.len(), 12, "the whole registry is still the engine's");
    assert_eq!(e.registry.len(), 12, "the engine itself forgets nothing");

    // A snapshot of the engine a replay rebuilds from it is the same bytes, and counts the same orders.
    let back = Engine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(back.journal_snapshot(), snap);
    assert_eq!(back.snapshot_state().ended_orders_left_out, 5);
    // ...and a later order that ends adds one more to the count once it ages out.
    let mut later = back;
    later.registry.insert(
        "much-later".into(),
        order("much-later", OrderStatus::Accepted, Some(newest + 10 * ENDED_ORDER_KEEP_MS)),
    );
    let s = later.journal_snapshot();
    assert_eq!(s.ended_orders_left_out, 7, "edge-filled and newest-filled aged out");
    assert_eq!(s.registry.len() as u64 + s.ended_orders_left_out, 13);
}

#[test]
fn the_journal_snapshot_writes_the_newest_closed_pnl_rows_and_the_full_one_writes_them_all() {
    let mut e = engine();
    let rows = vike_execution::JOURNAL_CLOSED_PNLS + 10;
    for i in 0..2 * rows as u64 {
        let side = if i % 2 == 0 { 1 } else { -1 };
        fold(&mut e, &fill(i, side, 100.0 + (i % 3) as f64, T0), T0);
    }
    assert_eq!(e.snapshot_state().account.closed_pnls.len(), rows);
    let journal = e.journal_snapshot().account.closed_pnls;
    assert_eq!(journal, e.account.closed_pnls[10..], "the newest rows, in order");
    assert_eq!(e.journal_snapshot().account.realized_pnl, e.account.realized_pnl);
    // A replay rebuilt from it keeps the newest again as it folds more.
    let mut back = Engine::from_snapshot(&e.journal_snapshot(), RecordingClient::default());
    for i in 0..4u64 {
        let f = fill(10_000 + i, if i % 2 == 0 { 1 } else { -1 }, 101.0, T0);
        fold(&mut back, &f, T0);
        fold(&mut e, &f, T0);
    }
    assert_eq!(
        back.journal_snapshot(),
        e.journal_snapshot(),
        "live and replay write the same Snapshot"
    );
}

/// One fill a day for 120 days, flat every other day. Returns the engine and the fills.
fn a_hundred_and_twenty_days() -> (Engine<RecordingClient>, Vec<FillEvent>) {
    let mut e = engine();
    let mut fills = Vec::new();
    for d in 0..120u64 {
        let ts = T0 + d as i64 * DAY_MS + 3_600_000;
        let f = fill(d, if d % 2 == 0 { 1 } else { -1 }, 100.0 + (d % 7) as f64, ts);
        fold(&mut e, &f, ts + 5);
        fills.push(f);
    }
    (e, fills)
}

#[test]
fn the_journal_snapshot_keeps_a_window_of_days_and_states_the_floor() {
    let (e, _) = a_hundred_and_twenty_days();
    let full = e.snapshot_state();
    assert_eq!(full.seen_trade_ids.len(), 120);
    assert_eq!(full.fill_floor_ms, None, "the full form leaves nothing out");
    let snap = e.journal_snapshot();
    let newest = day_of(T0) + 119;
    let first = newest - FILL_DEDUP_WINDOW_DAYS;
    assert_eq!(snap.seen_trade_ids.len(), (newest - first + 1) as usize, "the window's days only");
    assert_eq!(snap.seen_trade_days.len(), snap.seen_trade_ids.len(), "a day per id");
    assert!(snap.seen_trade_days.iter().all(|&d| d >= first));
    let mut sorted = snap.seen_trade_ids.clone();
    sorted.sort();
    assert_eq!(sorted, snap.seen_trade_ids, "the ids stay sorted: the canonical form");
    assert_eq!(snap.fill_floor_ms, Some(floor_ms(newest)));
    assert_eq!(snap.fill_floor_ms, Some(i64::from(first + 1) * DAY_MS));
    assert!(
        snap.fill_floor_ms.unwrap() <= T0 + 119 * DAY_MS - u32::MAX as i64,
        "the floor is behind the longest reconcile read-back"
    );
}

/// THE MONEY PROOF. A ledger restored from the bounded `Snapshot` (the replay's `from_snapshot` and the live boot's
/// `restore_runtime_state` alike) refuses every fill the venue re-delivers, those the window kept by their id and
/// those it dropped by the floor, and books a new fill and a fill a reconcile pass reads back at its longest.
#[test]
fn a_ledger_restored_from_the_window_books_no_redelivery_twice_and_every_new_fill_once() {
    let (live, fills) = a_hundred_and_twenty_days();
    let snap = live.journal_snapshot();
    let mut replayed = Engine::from_snapshot(&snap, RecordingClient::default());
    let mut booted = engine();
    booted.restore_runtime_state(&snap);
    let now = T0 + 120 * DAY_MS;
    for e in [&mut replayed, &mut booted] {
        assert_eq!(money(e), money(&live), "the restore holds the live money");
        let before = money(e);
        for f in &fills {
            fold(e, f, now);
        }
        assert_eq!(money(e), before, "no re-delivered fill booked again, in or out of the window");
        assert_eq!(e.account.duplicate_fills_refused, 120, "each one refused");
        assert_eq!(e.fill_drop_reason(&fills[0]), Some(FillDrop::BeforeDedupWindow), "dropped");
        assert_eq!(e.fill_drop_reason(&fills[119]), Some(FillDrop::AlreadyFolded), "kept");

        // A NEW fill books, and so does one a pass reads back at the longest lookback the setting allows.
        let new = fill(500, 1, 101.0, now);
        assert_eq!(e.fill_drop_reason(&new), None);
        fold(e, &new, now);
        let oldest_readback = fill(501, -1, 101.0, now - u32::MAX as i64);
        assert_eq!(e.fill_drop_reason(&oldest_readback), None, "within every lookback");
        fold(e, &oldest_readback, now);
        assert_eq!(e.account.duplicate_fills_refused, 120);
        assert_eq!(
            e.account.realized_pnl, live.account.realized_pnl,
            "a flat round trip at one price"
        );
        let size: f64 = e.account.positions.values().map(|p| p.size).sum();
        assert_eq!(size, live.account.positions.values().map(|p| p.size).sum::<f64>());
        assert!(e.take_unproven_fills().is_empty(), "every refusal above was proven, none parked");
    }
}

/// THE NO-TIMESTAMP PROOF. A venue that re-sends an OLD fill without its timestamp (`ts` 0: a frame that omitted
/// the field) to a ledger restored from the window books NOTHING: no stamp can put it after the floor, and the
/// window no longer holds its id. The fill is parked for the operator (the core holds it as a reconcile row) and
/// a confirm would be refused by the same reason. A fill the window still holds is an ordinary duplicate.
#[test]
fn a_timestamp_less_redelivery_of_an_old_fill_books_nothing_and_is_held_for_the_operator() {
    let (live, fills) = a_hundred_and_twenty_days();
    let snap = live.journal_snapshot();
    let mut replayed = Engine::from_snapshot(&snap, RecordingClient::default());
    let mut booted = engine();
    booted.restore_runtime_state(&snap);
    let now = T0 + 120 * DAY_MS;
    let unstamped = |f: &FillEvent| FillEvent { timestamp: 0, ..f.clone() };
    for e in [&mut replayed, &mut booted] {
        let before = money(e);
        let dropped = unstamped(&fills[0]);
        assert_eq!(e.fill_drop_reason(&dropped), Some(FillDrop::BeforeDedupWindow));
        fold(e, &dropped, now);
        fold(e, &dropped, now);
        fold(e, &unstamped(&fills[119]), now);
        assert_eq!(money(e), before, "nothing booked");
        let parked = e.take_unproven_fills();
        assert_eq!(
            parked.len(),
            1,
            "the dropped one parked once; the kept one is a plain duplicate"
        );
        assert_eq!(parked[0].trade_id, dropped.trade_id);
        assert_eq!(e.account.duplicate_fills_refused, 1, "only the kept id counts as a duplicate");
        assert!(!e.account.has_folded_fill(dropped.trade_id.as_str()), "its id is not burned");
        // The same fill WITH a stamp the window vouches for is new and books.
        let stamped = FillEvent { timestamp: now, ..dropped.clone() };
        assert_eq!(e.fill_drop_reason(&stamped), None);
        fold(e, &stamped, now);
        assert_ne!(money(e), before, "a stamp proves it new");
    }
}

/// ...and a GENUINELY new fill without a timestamp still books wherever no floor applies: a process that never
/// restored a window, and one restored from a `Snapshot` whose ledger left nothing out.
#[test]
fn a_new_timestamp_less_fill_books_when_no_floor_applies() {
    let new = fill(9_000, 1, 100.0, 0);
    let mut fresh = engine();
    fold(&mut fresh, &new, T0);
    assert_eq!(fresh.account.positions.values().map(|p| p.size).sum::<f64>(), 1.0, "booked");

    let mut short = engine();
    for d in 0..10u64 {
        fold(&mut short, &fill(d, 1, 100.0, T0 + d as i64 * DAY_MS), T0 + d as i64 * DAY_MS);
    }
    let snap = short.journal_snapshot();
    assert_eq!(snap.fill_floor_ms, None, "ten days span no window: nothing left out, no floor");
    let mut booted = engine();
    booted.restore_runtime_state(&snap);
    fold(&mut booted, &new, T0 + 10 * DAY_MS);
    assert_eq!(booted.account.positions.values().map(|p| p.size).sum::<f64>(), 11.0, "booked");
    assert!(booted.take_unproven_fills().is_empty());
}

#[test]
fn a_snap_written_before_the_window_restores_every_id_at_its_own_day_and_no_floor() {
    let (live, fills) = a_hundred_and_twenty_days();
    let old = live.snapshot_state();
    assert!(
        old.seen_trade_days.is_empty() && old.fill_floor_ms.is_none(),
        "the full form: old bytes"
    );
    let mut tree = serde_json::to_value(&old).unwrap();
    for key in ["seen_trade_days", "seen_fsm_trade_days", "fill_floor_ms", "ended_orders_left_out"]
    {
        assert!(
            tree.as_object_mut().unwrap().remove(key).is_none(),
            "{key} writes no key when empty"
        );
    }
    let old: EngineSnapshot = serde_json::from_value(tree).unwrap();
    let mut e = engine();
    e.restore_runtime_state(&old);
    assert_eq!(e.account.fill_floor_ms(), None);
    assert_eq!(e.seen_trade_ids().count(), 120, "every id of the old Snapshot");
    let before = money(&e);
    for f in &fills {
        fold(&mut e, f, T0 + 200 * DAY_MS);
    }
    assert_eq!(money(&e), before, "none booked again");
    // The next bounded snapshot stamps them at the old Snapshot's own day, so it keeps them all for a window.
    let next = e.journal_snapshot();
    assert_eq!(next.seen_trade_ids.len(), 120);
    assert!(next.seen_trade_days.iter().all(|&d| d == day_of(old.now_ms)));
}

#[test]
fn the_window_bookkeeping_is_left_out_of_the_state_hash() {
    let (e, _) = a_hundred_and_twenty_days();
    let snap = e.journal_snapshot();
    let mut bare = snap.clone();
    bare.seen_trade_days.clear();
    bare.seen_fsm_trade_days.clear();
    bare.fill_floor_ms = None;
    bare.ended_orders_left_out = 0;
    assert_eq!(vike_execution::state_hash(&[snap]), vike_execution::state_hash(&[bare]));
}

/// A build from before the keys ignores them: the struct takes no `deny_unknown_fields`, so a bounded
/// snapshot's JSON reads into the shape that knew none of them.
#[test]
fn a_build_from_before_the_window_reads_a_bounded_snapshot() {
    #[derive(serde::Deserialize)]
    struct Before {
        seen_trade_ids: Vec<String>,
        registry: Vec<(String, OrderRecord)>,
    }
    let (e, _) = a_hundred_and_twenty_days();
    let snap = e.journal_snapshot();
    let before: Before = serde_json::from_str(&serde_json::to_string(&snap).unwrap()).unwrap();
    assert_eq!(before.seen_trade_ids, snap.seen_trade_ids);
    assert_eq!(before.registry.len(), snap.registry.len());
}
