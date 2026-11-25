//! The journal `Snapshot`'s two trade-id sets: the FSM's rides as its DIFFERENCE from the account's
//! (`fill_window::difference`) and a restore rebuilds both exactly, ids and fold days, however the two differ and
//! wherever the window cuts each; and what the difference saves on the CI box's measured shape.

use crate::account::fill_window::{self, day_of};
use crate::testing::{RecordingClient, engine_on};
use crate::{Engine, EventBusOutbox, EventHandler};
use vike_model::MS_PER_DAY;
use vike_model::events::{Event, FillEvent, OrderFilled, PositionSide, TradeId};
use vike_model::{OrderRecord, OrderRequest, OrderStatus, OrderType};

/// Day 20,000 of the epoch (2024-10-04).
const T0: i64 = 20_000 * MS_PER_DAY;

type Entries = Vec<(String, u32)>;

fn engine() -> Engine<RecordingClient> {
    engine_on("sim", "BTCUSDT", RecordingClient::default())
}

fn sorted<'a>(entries: impl Iterator<Item = (&'a str, u32)>) -> Entries {
    let mut v: Entries = entries.map(|(id, day)| (id.to_string(), day)).collect();
    v.sort();
    v
}

/// Both sets as the process holds them, every `(id, fold day)`, sorted: what a restore must give back.
fn both_sets(e: &Engine<RecordingClient>) -> (Entries, Entries) {
    let account = sorted(e.account.fill_ledger(None).0.entries.into_iter());
    (account, sorted(e.seen_fsm_trade_ids.iter().map(|(id, &day)| (id, day))))
}

/// `entries` cut to `window` days of their own newest, sorted.
fn cut(entries: &[(String, u32)], window: u32) -> Entries {
    let w = fill_window::windowed(entries.iter().map(|(id, d)| (id.as_str(), *d)), Some(window));
    w.entries.iter().map(|&(id, day)| (id.to_string(), day)).collect()
}

/// An engine whose two sets differ BOTH ways and by a day, over `span` days back from its clock's day: one id in
/// both a day, a bare fill whose wrap has not come (the account's only), a wrap whose bare fill was refused (the
/// FSM's only), and a fill whose wrap folded after midnight (both, a day apart).
fn differing(span: u32) -> Engine<RecordingClient> {
    let mut e = engine();
    let today = day_of(T0) + span;
    e.now_ms = i64::from(today) * MS_PER_DAY + 3_600_000;
    let shared: Entries = (0..=span).map(|d| (format!("both-{d:03}"), today - d)).collect();
    let account_too = [("bare-only".to_string(), today - 1), ("midnight".to_string(), today - 2)];
    e.account.seed_fill_ledger(shared.iter().cloned().chain(account_too), None);
    let fsm_too = [("wrap-only".to_string(), today - 1), ("midnight".to_string(), today - 1)];
    e.seen_fsm_trade_ids = shared.into_iter().chain(fsm_too).collect();
    e
}

/// THE ROUND TRIP. The `Snapshot` writes the account's ids, the FSM's own entries (the wrap-only id and the one a day
/// apart) and the account's ids the FSM lacks; the replay's `from_snapshot` and the boot's `restore_runtime_state`
/// rebuild both sets exactly, every id at its day, and write the same `Snapshot` again.
#[test]
fn the_fsm_set_rides_as_its_difference_and_both_sets_come_back_exactly() {
    let e = differing(3);
    let snap = e.journal_snapshot();
    assert_eq!(snap.seen_trade_ids.len(), 6, "four shared, the bare-only and the midnight ones");
    assert_eq!(
        snap.fsm_only_trade_ids,
        ["midnight", "wrap-only"],
        "its own, and the one a day apart"
    );
    assert_eq!(snap.account_only_trade_ids, ["bare-only"]);
    let today = day_of(e.now_ms);
    assert_eq!(snap.fsm_only_trade_days, [today - 1, today - 1], "each at the FSM's own day");

    let back = Engine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(both_sets(&back), both_sets(&e), "the replay core's two sets");
    assert_eq!(back.journal_snapshot(), snap, "and the same Snapshot again");
    let mut booted = engine();
    booted.restore_runtime_state(&snap);
    assert_eq!(both_sets(&booted), both_sets(&e), "the boot restore's two sets");

    // The full form writes no days, so it compares ids alone and a restore puts every id at one day.
    let full = e.snapshot_state();
    assert_eq!(full.fsm_only_trade_ids, ["wrap-only"]);
    assert_eq!(full.account_only_trade_ids, ["bare-only"]);
    let back = Engine::from_snapshot(&full, RecordingClient::default());
    let ids = |(a, f): (Entries, Entries)| {
        let ids = |v: Entries| v.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
        (ids(a), ids(f))
    };
    assert_eq!(ids(both_sets(&back)), ids(both_sets(&e)));
    assert_eq!(back.snapshot_state(), full);
}

/// THE WINDOW. Each set is cut at its OWN newest id (here the FSM's is a day later, a wrap folded after the
/// account's last bare fill), so an id one window keeps and the other drops is a difference too; the restore
/// gives back exactly what the two windows kept.
#[test]
fn a_window_cuts_each_set_at_its_own_newest_and_the_difference_restores_both_cuts() {
    let mut e = differing(10);
    let today = day_of(e.now_ms);
    e.seen_fsm_trade_ids.insert("wrap-late", today + 1);
    e.size_fill_window(3_600_000);
    let window = fill_window::window_days(3_600_000);
    let (account, fsm) = both_sets(&e);
    let snap = e.journal_snapshot();
    assert!(snap.fill_floor_ms.is_some(), "the window left ids out");
    assert!(
        snap.account_only_trade_ids.contains(&format!("both-{window:03}")),
        "the account's window keeps a day the FSM's has dropped"
    );
    let want = (cut(&account, window), cut(&fsm, window));
    let back = Engine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(both_sets(&back), want, "the replay core's");
    assert_eq!(back.journal_snapshot(), snap);
    let mut booted = engine();
    booted.restore_runtime_state(&snap);
    assert_eq!(both_sets(&booted), want, "the boot restore's");
}

fn json_len<T: serde::Serialize + ?Sized>(v: &T) -> usize {
    serde_json::to_vec(v).expect("serializes").len()
}

/// A list's bytes as the `Snapshot` writes it: nothing when it is empty and skipped.
fn listed<T: serde::Serialize>(v: &[T]) -> usize {
    if v.is_empty() { 0 } else { json_len(v) }
}

/// THE SIZE PROOF, on the CI box's measured shape (`state_growth_investigation.md`: 2,477 ids in the account's set,
/// 2,476 of them in the FSM's, a bybit execution id of 36 characters, ~610 fills an hour): 2,500 fills, each folded as
/// a bare fill and its wrap, but the last, whose wrap has not come. Prints the `Snapshot`'s seen-id bytes as JSON
/// (`--nocapture`): BEFORE, the FSM set written whole beside the account's as format 33 did, and AFTER, its
/// difference.
#[test]
fn the_snapshot_seen_id_bytes_for_2500_fills_before_and_after() {
    let mut e = engine();
    let mut outbox = EventBusOutbox::default();
    let n = 2_500u64;
    for i in 0..n {
        let now = T0 + i as i64 * 5_900;
        e.now_ms = now;
        let order_client_id = format!("vike-c{i:010}");
        let side = if i % 2 == 0 { 1 } else { -1 };
        let mut order = OrderRecord::new(OrderRequest {
            order_client_id: order_client_id.clone(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side,
            qty: 1.0,
            order_type: OrderType::Limit,
            price: Some(100.0),
            ..Default::default()
        });
        order.status = OrderStatus::Accepted;
        order.created_ms = Some(now);
        e.registry.insert(order_client_id.clone(), order);
        let fill = FillEvent {
            // A bybit `execId`'s shape: 36 characters.
            trade_id: TradeId::new(format!(
                "{:08x}-{:04x}-4000-8000-{:012x}",
                i * 2_654_435_761 % (1 << 32),
                i,
                i
            ))
            .expect("not empty"),
            order_client_id: order_client_id.clone(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side,
            last_qty: 1.0,
            last_price: 100.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "maker".into(),
            timestamp: now,
            mark_price: None,
            position_side: PositionSide::Both,
        };
        e.on_event(&Event::Fill(fill.clone()), &mut outbox);
        if i + 1 < n {
            e.on_event(
                &Event::OrderFilled(OrderFilled { order_client_id, fill, timestamp: now }),
                &mut outbox,
            );
        }
    }
    let snap = e.journal_snapshot();
    assert_eq!(snap.seen_trade_ids.len(), n as usize);
    assert!(snap.fsm_only_trade_ids.is_empty());
    assert_eq!(snap.account_only_trade_ids.len(), 1, "the one wrap still to come");

    let account = json_len(&snap.seen_trade_ids) + listed(&snap.seen_trade_days);
    let fsm = fill_window::windowed(
        e.seen_fsm_trade_ids.iter().map(|(id, &day)| (id, day)),
        Some(e.fill_window_days),
    );
    let (fsm_ids, _) = fill_window::written(&fsm.entries);
    assert_eq!(fsm_ids.len(), n as usize - 1);
    // Format 33 wrote the FSM's days beside it as the account's (none here: every id folded today).
    let whole = json_len(&fsm_ids) + listed(&snap.seen_trade_days);
    let difference = listed(&snap.fsm_only_trade_ids)
        + listed(&snap.fsm_only_trade_days)
        + listed(&snap.account_only_trade_ids);
    let (before, after) = (account + whole, account + difference);
    println!(
        "SEEN-ID BYTES {n} fills (JSON): before {before} (account set {account} + FSM set whole {whole}), \
         after {after} (account set {account} + FSM difference {difference}: {} only in the FSM's, {} only in \
         the account's); {:.1} bytes an id",
        snap.fsm_only_trade_ids.len(),
        snap.account_only_trade_ids.len(),
        account as f64 / n as f64,
    );
    assert!(difference * 100 < whole, "the FSM set's part: {whole} -> {difference} bytes");
}
