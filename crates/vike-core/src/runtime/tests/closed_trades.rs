//! A live mount's CLOSED TRADES (`crate::runtime::closed_trades`), white-box over the REAL `dispatch` path:
//! every fill delivered to the mount is booked by the simulator's ledger BEFORE its strategy hears it, the rows
//! are that ledger's over the same fills (an entry, an add, a close across two lots, a flip), a fill's bar index
//! is the bar it falls in (the bar in progress taking the next index), an unmounted slot drops its ledger, and a
//! restart holds the restored position as a lot.

use super::*;
use std::sync::Mutex as TestMutex;
use vike_analytics::closed_trades::TradeClosedLedger;
use vike_execution::testing::RecordingClient;
use vike_model::events::{FillEvent, TradeId};
use vike_model::{OrderBroker, OrderPlacement, TradeClosedRow};

use crate::runtime::closed_trades::bar_of;
use crate::runtime::runtime_mount_tests::unmount_command;
use crate::runtime::test_support::core_with;

const H: i64 = 3_600_000;
/// Every fill's commission, in the account's money.
const FEE: f64 = 0.5;

/// What one hook was lent: the hook, its instant, the closed rows.
type Read = (&'static str, i64, Vec<TradeClosedRow>);

/// Records `closed_trades()` at every bar and every fill, and submits at market every `(side, qty)` queued.
struct Ledgered {
    reads: Arc<TestMutex<Vec<Read>>>,
    orders: Arc<TestMutex<Vec<(i32, f64)>>>,
}

impl Ledgered {
    fn read(&self, hook: &'static str, ts: i64, b: &LiveBroker) {
        let rows = b.closed_trades().expect("a live mount always keeps a ledger").to_vec();
        self.reads.lock().unwrap().push((hook, ts, rows));
    }
}

impl Strategy<LiveBroker> for Ledgered {
    fn on_bar(&mut self, b: &mut LiveBroker, bar: &Bar) {
        self.read("bar", bar.timestamp, b);
        for (side, qty) in self.orders.lock().unwrap().drain(..) {
            b.place("", "", side, qty, OrderPlacement::Market, false);
        }
    }
    fn on_fill(&mut self, b: &mut LiveBroker, fill: &Fill) {
        self.read("fill", fill.timestamp, b);
    }
}

struct Rig {
    core: CoreThread<RecordingClient>,
    reads: Arc<TestMutex<Vec<Read>>>,
    orders: Arc<TestMutex<Vec<(i32, f64)>>>,
}

fn rig(restored: Vec<vike_journal::JournalSnapshotMountAttribution>) -> Rig {
    rig_with(restored, Vec::new(), Vec::new())
}

/// [`rig`] handed the journal's closed-trade ledgers and the tail to book on them, as a boot restore hands them.
fn rig_with(
    restored: Vec<vike_journal::JournalSnapshotMountAttribution>,
    closed_trades: Vec<vike_journal::JournalSnapshotClosedTrades>,
    closed_tail: Vec<crate::replay::TradeClosedTail>,
) -> Rig {
    let (reads, orders) = (Arc::default(), Arc::default());
    let strategy = Ledgered { reads: Arc::clone(&reads), orders: Arc::clone(&orders) };
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            strategy: Box::new(strategy),
            symbols: Vec::new(),
            underlying_symbol: None,
            controller_id: Some("ledger".to_string()),
        }),
        mount_attribution: restored,
        closed_trades,
        closed_tail,
        clock: Box::new(|| 0_i64),
        ..CoreConfig::default()
    });
    // What `spawn_core` arms for a core with a mount, so a fill reaches its strategy.
    core.engine.collect_applied_fills = true;
    Rig { core, reads, orders }
}

/// Close the mount's own bar opened at `ts`, after queueing `order` for its decision.
fn decide(rig: &mut Rig, ts: i64, close: f64, order: Option<(i32, f64)>) {
    rig.orders.lock().unwrap().extend(order);
    let bar = Bar::ohlcv(ts, close, close, close, close, 1.0);
    let (venue, symbol, interval) = ("sim".into(), "BTCUSDT".into(), "1h".into());
    rig.core.dispatch(Ingest::BarClose(Box::new(BarUpdate { venue, symbol, interval, bar })));
}

/// The venue fills the `n`th order sent, whole, at `px` at `ts`.
fn fill(rig: &mut Rig, n: usize, px: f64, ts: i64) {
    let sent = &rig.core.engine.client.submissions[n];
    let order_client_id = sent.order_client_id.clone();
    let event = FillEvent {
        trade_id: TradeId::prefixed("t-", format_args!("{order_client_id}-{ts}")),
        order_client_id,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: sent.side,
        last_qty: sent.qty,
        last_price: px,
        commission: FEE,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: ts,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    };
    rig.core.bus.publish(Event::Fill(event), &mut rig.core.engine);
    rig.core.dispatch_applied_fills();
}

/// The simulator's booking of one fill (`crates/vike-simulator/src/engine/sim_broker/fills.rs`'s `fold_fill`).
fn book(ledger: &mut TradeClosedLedger, (side, size, price, ts): (i32, f64, f64, i64), bar: usize) {
    let fill = Fill {
        side,
        size,
        price,
        fee: FEE,
        timestamp: ts,
        is_maker: false,
        symbol: "BTCUSDT".into(),
        tag: None,
    };
    ledger.apply(&fill, "", bar, vike_model::ContractValue::default(), 1.0);
}

/// **A fill's rows are in the ledger before its strategy hears it, and they are the simulator's**: an entry, an
/// add, a sell across both lots and a flip, each filled at the next bar's open (in the bar in progress, so at the
/// index that bar takes), read at every bar and every fill, equal row for row to the simulator's ledger over the
/// same fills at the same indices.
#[test]
fn a_mounts_closed_trades_are_the_simulators_booked_before_the_fill_is_heard() {
    let mut rig = rig(Vec::new());
    decide(&mut rig, 0, 100.0, Some((1, 2.0)));
    fill(&mut rig, 0, 100.0, H);
    decide(&mut rig, H, 101.0, Some((1, 1.0)));
    fill(&mut rig, 1, 102.0, 2 * H);
    decide(&mut rig, 2 * H, 103.0, Some((-1, 2.5)));
    fill(&mut rig, 2, 104.0, 3 * H);
    decide(&mut rig, 3 * H, 105.0, Some((-1, 1.5)));
    fill(&mut rig, 3, 103.0, 4 * H);
    decide(&mut rig, 4 * H, 106.0, None);

    let mut sim = TradeClosedLedger::default();
    let fills = [
        (1, 2.0, 100.0, H),
        (1, 1.0, 102.0, 2 * H),
        (-1, 2.5, 104.0, 3 * H),
        (-1, 1.5, 103.0, 4 * H),
    ];
    let mut after: Vec<Vec<TradeClosedRow>> = Vec::new();
    for (k, f) in fills.into_iter().enumerate() {
        book(&mut sim, f, k + 1);
        after.push(sim.rows().to_vec());
    }
    let none = Vec::new();
    assert_eq!(
        *rig.reads.lock().unwrap(),
        vec![
            ("bar", 0, none.clone()),
            ("fill", H, after[0].clone()),
            ("bar", H, after[0].clone()),
            ("fill", 2 * H, after[1].clone()),
            ("bar", 2 * H, after[1].clone()),
            ("fill", 3 * H, after[2].clone()),
            ("bar", 3 * H, after[2].clone()),
            ("fill", 4 * H, after[3].clone()),
            ("bar", 4 * H, after[3].clone()),
        ]
    );
    // The sell of 2.5 closed the first lot whole and half the second, the flip the second's rest.
    let rows = sim.rows();
    assert_eq!(rows.len(), 3);
    assert_eq!((rows[0].entry_bar, rows[0].exit_bar, rows[0].qty, rows[0].pnl), (1, 3, 2.0, 8.0));
    assert_eq!((rows[1].entry_price, rows[1].qty), (102.0, 0.5));
    assert_eq!((rows[2].exit_bar, rows[2].exit_price, rows[2].qty), (4, 103.0, 0.5));
    let lots = sim.lots();
    assert_eq!(
        (lots.len(), lots[0].side, lots[0].qty),
        (1, -1, 1.0),
        "the flip opened a short of 1"
    );
}

/// **The bar index is the bar the fill falls in**: inside a closed bar its index, after the last close the
/// next one, over an empty series 0, and with no bar length the last closed bar's.
#[test]
fn a_fills_bar_is_the_one_it_falls_in() {
    let closed: Vec<Bar> = [0, H, 2 * H].map(|ts| Bar::ohlcv(ts, 1.0, 1.0, 1.0, 1.0, 1.0)).to_vec();
    assert_eq!(bar_of(&closed, H, 0), 0);
    assert_eq!(bar_of(&closed, H, H - 1), 0);
    assert_eq!(bar_of(&closed, H, H), 1);
    assert_eq!(bar_of(&closed, H, 2 * H + 5), 2, "the last closed bar, before its close");
    assert_eq!(bar_of(&closed, H, 3 * H), 3, "the bar in progress takes the next index");
    assert_eq!(bar_of(&closed, H, 9 * H), 3);
    assert_eq!(bar_of(&[], H, 5), 0);
    assert_eq!(bar_of(&closed, 0, 9 * H), 2, "no bar length: no bar in progress");
}

/// **An unmounted slot drops its ledger**, and a fill of its order arriving after books into nothing.
#[test]
fn an_unmounted_slot_drops_its_closed_trades() {
    let mut rig = rig(Vec::new());
    decide(&mut rig, 0, 100.0, Some((1, 2.0)));
    fill(&mut rig, 0, 100.0, H);
    assert!(rig.core.mount_closed[0].is_some());
    decide(&mut rig, H, 101.0, Some((-1, 2.0)));
    rig.core.dispatch(unmount_command("ledger"));
    assert!(rig.core.mounts[0].is_none(), "unmounted: {:?}", rig.core.recent);
    assert!(rig.core.mount_closed[0].is_none(), "the tombstone drops its ledger");
    fill(&mut rig, 1, 103.0, 2 * H);
    assert!(rig.core.mount_closed[0].is_none(), "a straggler books into nothing");
}

/// **A restart holds the restored position as a lot**: no row from before the restart, and a close after it
/// closes the lot the restored ledger holds (at its average price, with no fee, opened at the boot instant)
/// instead of opening the opposite side.
#[test]
fn a_restart_holds_the_restored_position_as_a_lot() {
    let restored = vike_journal::JournalSnapshotMountAttribution {
        mount_id: "ledger".into(),
        size: 2.0,
        average_price: 100.0,
        ..Default::default()
    };
    let mut rig = rig(vec![restored]);
    let lots = rig.core.mount_closed[0].as_deref().map(|l| l.lots().to_vec()).unwrap_or_default();
    assert_eq!(lots.len(), 1, "the restored position is one lot");
    assert_eq!(
        (lots[0].side, lots[0].qty, lots[0].price, lots[0].entry_fee_left()),
        (1, 2.0, 100.0, 0.0)
    );

    decide(&mut rig, 0, 104.0, Some((-1, 2.0)));
    fill(&mut rig, 0, 105.0, H);
    let reads = rig.reads.lock().unwrap();
    assert!(reads[0].2.is_empty(), "no row from before the restart");
    let rows = &reads[1].2;
    assert_eq!(rows.len(), 1, "the close closed the restored lot: {rows:?}");
    assert_eq!((rows[0].side, rows[0].entry_price, rows[0].exit_price), (1, 100.0, 105.0));
    assert_eq!((rows[0].pnl, rows[0].commission), (10.0, FEE), "the entry fee was charged before");
    assert!(rig.core.mount_closed[0].as_deref().is_some_and(|l| l.lots().is_empty()), "flat");
}

/// The mount's ledger's open lots as `(side, qty, price)`.
fn lots_of(rig: &Rig) -> Vec<(i32, f64, f64)> {
    let lots = rig.core.mount_closed[0].as_deref().map(|l| l.lots().to_vec()).unwrap_or_default();
    lots.iter().map(|l| (l.side, l.qty, l.price)).collect()
}

/// The mount's ledger's closed rows.
fn rows_of(rig: &Rig) -> Vec<TradeClosedRow> {
    rig.core.mount_closed[0].as_deref().map(|l| l.rows().to_vec()).unwrap_or_default()
}

/// A venue fill of the order `order_client_id`, as the journal's tail holds it.
fn tail_fill(order_client_id: &str, (side, qty, px, ts): (i32, f64, f64, i64)) -> FillEvent {
    FillEvent {
        trade_id: TradeId::prefixed("t-", format_args!("{order_client_id}")),
        order_client_id: order_client_id.to_string(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_price: px,
        commission: FEE,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: ts,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    }
}

/// **A restart resumes the `Snapshot`'s ledger and books the journal's TAIL on it** (the module doc's "A restart"):
/// the `Snapshot` held one lot and a closed row; a fill of the mount's order after it (journaled as a fill, nothing
/// else) is booked at assembly at the bar its clock counts, so the mount comes up with the row and BOTH lots, where
/// its restored position alone would be one lot at the average. The next close books against them as a run with
/// no restart does: the same rows as the simulator's ledger over every fill.
#[test]
fn a_restart_resumes_the_snaps_ledger_and_books_the_tail_on_it() {
    // The run before the restart, on the simulator's ledger: buy 2 at 100 (bar 1), sell 1 at 101 (bar 2), buy 1
    // at 102 (bar 3, after the Snapshot).
    let mut before = TradeClosedLedger::default();
    book(&mut before, (1, 2.0, 100.0, H), 1);
    book(&mut before, (-1, 1.0, 101.0, 2 * H), 2);
    let snap = vike_journal::JournalSnapshotClosedTrades {
        mount_id: "ledger".into(),
        archived: 0,
        rows: before.rows().to_vec(),
        lots: before.lots().iter().map(snap_lot).collect(),
        bar_clock: Some(vike_journal::JournalSnapshotBarClock {
            timestamp: 2 * H,
            index: 2,
            bar_ms: H,
        }),
    };
    let tail = vec![crate::replay::TradeClosedTail::Fill {
        fill: tail_fill("after-snap", (1, 1.0, 102.0, 3 * H)),
        owner: Some("ledger".into()),
        margin_close: false,
    }];
    book(&mut before, (1, 1.0, 102.0, 3 * H), 3);
    let attr = vike_journal::JournalSnapshotMountAttribution {
        mount_id: "ledger".into(),
        size: 2.0,
        average_price: 101.0,
        ..Default::default()
    };
    let mut rig = rig_with(vec![attr], vec![snap], tail);
    assert_eq!(
        lots_of(&rig),
        vec![(1, 1.0, 100.0), (1, 1.0, 102.0)],
        "both lots, not their average"
    );
    assert_eq!(rows_of(&rig), before.rows().to_vec(), "the Snapshot's row");
    assert!(rig.core.pending_closed.is_empty(), "the mount took its ledger");
    let lot = &rig.core.mount_closed[0].as_deref().expect("a ledger").lots()[1];
    assert_eq!(
        (lot.entry_bar, lot.entry_fee_left()),
        (3, FEE),
        "the tail's bar and fee, as booked live"
    );

    decide(&mut rig, 3 * H, 103.0, Some((-1, 2.0)));
    fill(&mut rig, 0, 104.0, 4 * H);
    book(&mut before, (-1, 2.0, 104.0, 4 * H), 1);
    assert_eq!(
        rows_of(&rig),
        before.rows().to_vec(),
        "the close books as a run with no restart does"
    );
    assert_eq!(rows_of(&rig).len(), 3, "one row per lot it reached");
    let snapped = rig.core.closed_trades_for_snap();
    assert_eq!(
        (snapped.len(), snapped[0].rows.len(), snapped[0].lots.len()),
        (1, 3, 0),
        "the next Snapshot"
    );
}

/// An open lot as a `Snapshot` carries it.
fn snap_lot(l: &vike_analytics::closed_trades::OpenLot) -> vike_journal::JournalSnapshotOpenLot {
    vike_journal::JournalSnapshotOpenLot {
        symbol: l.symbol.clone(),
        tag: l.tag.clone(),
        side: l.side,
        entry_timestamp: l.entry_timestamp,
        entry_bar: l.entry_bar,
        price: l.price,
        qty: l.qty,
        fee_per_unit: l.entry_fee_per_unit(),
        value: l.value(),
    }
}

/// The rig's sole engine liquidated at `px`: the venue's own `PositionLiquidated` of the whole one-way position.
fn liquidate(rig: &mut Rig, px: f64, fee: f64, ts: i64) {
    let event = vike_model::events::PositionLiquidated {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        position_side: vike_model::events::PositionSide::Both,
        qty: 0.0,
        liquidation_price: px,
        fee,
        timestamp: ts,
        trade_id: "liq-1".into(),
        route_key: None,
        fee_currency: None,
    };
    rig.core.dispatch(Ingest::Event(Event::PositionLiquidated(event)));
}

/// **A LIQUIDATION of the mount's position enters its ledger** (the module doc's "What is booked"): the venue's
/// own, which reaches no strategy as a fill, closes the lots at the liquidation price with its fee, as the
/// simulator's ledger books its own liquidation, so the ledger and the account's position agree.
#[test]
fn a_venue_liquidation_closes_the_mounts_lots() {
    let mut rig = rig(Vec::new());
    decide(&mut rig, 0, 100.0, Some((1, 2.0)));
    fill(&mut rig, 0, 100.0, H);
    liquidate(&mut rig, 90.0, 1.0, 2 * H);
    assert!(lots_of(&rig).is_empty(), "the liquidated position holds no lot");
    let rows = rows_of(&rig);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].side, rows[0].entry_price, rows[0].exit_price, rows[0].qty),
        (1, 100.0, 90.0, 2.0)
    );
    assert_eq!((rows[0].pnl, rows[0].commission), (-20.0, FEE + 1.0), "the fee is its exit fee");
}

/// **A close of the account-wide margin call is a liquidation too, and an operator's ticket is not**: a fill of
/// an order no mount minted books into the mount's lots only when the margin call released it.
#[test]
fn a_margin_call_close_enters_the_ledger_and_an_operator_fill_does_not() {
    let mut rig = rig(Vec::new());
    decide(&mut rig, 0, 100.0, Some((1, 2.0)));
    fill(&mut rig, 0, 100.0, H);
    // Two reduce-only market sells no mount minted: the operator's, then the margin call's.
    for _ in 0..2 {
        let req = vike_model::OrderRequest {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            order_type: vike_model::OrderType::Market,
            reduce_only: true,
            ..Default::default()
        };
        rig.core.dispatch(Ingest::Command(Command::Order(OrderIntent::Submit(Box::new(req)))));
    }
    let order_client_id = rig.core.engine.client.submissions[2].order_client_id.clone();
    rig.core.liquidation_order_client_ids.insert(order_client_id);
    fill(&mut rig, 1, 95.0, 2 * H);
    assert_eq!(
        lots_of(&rig),
        vec![(1, 2.0, 100.0)],
        "the operator's fill is not the mount's trade"
    );
    fill(&mut rig, 2, 94.0, 3 * H);
    assert_eq!(lots_of(&rig), vec![(1, 1.0, 100.0)], "the margin call's close is");
    let rows = rows_of(&rig);
    assert_eq!((rows.len(), rows[0].exit_price, rows[0].qty), (1, 94.0, 1.0));
}
