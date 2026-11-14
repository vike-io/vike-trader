//! **One account's fills, read through both doors, give one tearsheet.** The journal door
//! (`vike_journal::fills_from_journal`, `TearsheetFillOrder::AsHeld`: the tradehub daemon and the `tearsheet` CLI) and the
//! stored door (the materializer's `kind=exec_fill` rows, read back and folded `TearsheetFillOrder::ByTime`: the desktop) read
//! one journal, so they must not answer two sheets. The fixture is the two ways they used to differ: a fee the venue
//! states after its fill and then revises (decision 0238), and a hedge account's long and short legs of one symbol.

use std::sync::Arc;
use std::time::Duration;

use vike_analytics::{Tearsheet, TearsheetFillOrder, tearsheet_from_fills};
use vike_data::{ExecFillRow, HistoryStore, HistoryStoreMemory};
use vike_datahub::materialize::{JournalMaterializer, JournalMaterializerConfig};
use vike_execution::Ingest;
use vike_journal::{Journal, JournalFileConfig, fills_from_journal};
use vike_model::events::{Event, FeeAdjustmentEvent, FillEvent, PositionSide};

const VENUE: &str = "bybit";
const SYMBOL: &str = "BTCUSDT";
const SEED: f64 = 10_000.0;

fn fill(trade: &'static str, side: i32, qty: f64, px: f64, fee: f64, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: trade.into(),
        order_client_id: format!("c-{trade}"),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        last_qty: qty,
        last_price: px,
        commission: fee,
        commission_asset: "USDT".into(),
        liquidity_side: "taker".into(),
        timestamp: ts,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

fn leg(position_side: PositionSide, f: FillEvent) -> Event {
    Event::Fill(FillEvent { position_side, ..f })
}

/// What the IBKR mapper sends: the total over the fill's own fee, stamped with the fill's own time.
fn correction(trade: &'static str, extra: f64, ts: i64) -> Event {
    Event::FeeAdjustment(FeeAdjustmentEvent {
        trade_id: trade.into(),
        order_client_id: format!("c-{trade}"),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        last_price: 100.0,
        extra_commission: extra,
        commission_asset: "USDT".into(),
        timestamp: ts,
    })
}

/// Append `events` and drain them through the real materializer: one call is one pass, one batch, one store part.
fn journal_and_drain(dir: &std::path::Path, store: &Arc<HistoryStoreMemory>, events: Vec<Event>) {
    let mut j = Journal::open(dir, JournalFileConfig::default()).unwrap();
    for (i, e) in events.into_iter().enumerate() {
        j.append_command(1_000 + i as i64, &Ingest::Event(e)).unwrap();
    }
    j.flush().unwrap();
    drop(j);
    let (_mat, handle) = JournalMaterializer::spawn(
        dir.to_path_buf(),
        store.clone(),
        JournalMaterializerConfig { interval: Duration::from_millis(20) },
    );
    handle.shutdown(); // stop + final drain + join: nothing in flight after this
}

/// Every catalog row of a sheet in its shortest round-trip spelling, which tells any two bit patterns apart and,
/// unlike `==` on an `f64`, holds a metric that is `NaN` on both sides equal.
fn numbers(sheet: &Tearsheet) -> Vec<String> {
    sheet.rows().map(|(spec, v)| format!("{}={v:?}", spec.id)).collect()
}

#[test]
fn one_journal_gives_one_tearsheet_through_both_doors() {
    use PositionSide::{Long, Short};
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreMemory::new());
    // Long 2 @ 100 with its fee not yet known; short 1 @ 110 beside it; the fee is stated.
    journal_and_drain(
        dir.path(),
        &store,
        vec![
            leg(Long, fill("t1", 1, 2.0, 100.0, 0.0, 1_000)),
            leg(Short, fill("t2", -1, 1.0, 110.0, 0.2, 2_000)),
            correction("t1", 1.00, 1_000),
        ],
    );
    // A later pass: the long closes at 130 (+60), the short at 120 (-10), and the venue revises the fee.
    journal_and_drain(
        dir.path(),
        &store,
        vec![
            leg(Long, fill("t3", -1, 2.0, 130.0, 0.3, 3_000)),
            leg(Short, fill("t4", 1, 1.0, 120.0, 0.4, 4_000)),
            correction("t1", 1.25, 1_000),
        ],
    );

    let (journal, journal_equity, _) = tearsheet_from_fills(
        fills_from_journal(dir.path()).unwrap(),
        TearsheetFillOrder::AsHeld,
        SEED,
        252.0,
    );
    let rows = store.scan_exec_fills(VENUE, SYMBOL).unwrap();
    let (stored, stored_equity, _) = tearsheet_from_fills(
        ExecFillRow::fill_events(&rows),
        TearsheetFillOrder::ByTime,
        SEED,
        252.0,
    );

    // The door that always folded both is the fixture's own arithmetic: two trades, the fee as revised.
    assert_eq!(journal.metric("n_trades"), Some(2.0));
    let net = (60.0 - (1.25 + 0.3)) + (-10.0 - (0.2 + 0.4));
    assert!((journal_equity.last().unwrap() - (SEED + net)).abs() < 1e-9, "{journal_equity:?}");

    assert_eq!(
        (stored.metric("n_trades"), stored_equity.last().copied()),
        (journal.metric("n_trades"), journal_equity.last().copied()),
        "trades and final equity: stored door against journal door"
    );
    assert_eq!(numbers(&stored), numbers(&journal), "every row of the sheet");
    assert_eq!(stored, journal);
}
