//! **One account's fills, read through both doors, give one tearsheet.** The journal door
//! (`vike_journal::fills_from_journal`, `TradingResultsTableFillOrder::AsHeld`: the tradehub daemon and the `tearsheet` CLI) and the
//! stored door (the execution stream's `kind=exec_fill` rows, read back through `ExecFillRow::fill_events` and folded
//! `TradingResultsTableFillOrder::ByTime`: the desktop) read one journal, so they must not answer two sheets (decision 0021). The
//! fixture is the two ways they used to differ: a fee the venue states after its fill and then revises (decision 0238),
//! and a hedge account's long and short legs of one symbol.
//!
//! The journal holds the venue events; the stream carries, for each, the report the OMS states about it: the fill or the
//! fee correction, caused by that event's record. Both doors read the same events.

use vike_analytics::{
    TradingResultsTable, TradingResultsTableFillOrder, trading_results_table_from_fills,
};
use vike_data::ExecFillRow;
use vike_datahub_client::execution_stream::{
    ExecutionRecordBodyWire, ExecutionReportFeeCorrectionWire, ExecutionReportFillWire,
    ExecutionReportWire, ExecutionStreamDown, ExecutionStreamUp,
};
use vike_execution::Ingest;
use vike_journal::{Journal, JournalFileConfig, fills_from_journal};
use vike_model::events::{Event, FeeAdjustmentEvent, FillEvent, PositionSide};
use vike_model::orders::ExecutionOwnership;

use crate::execution_stream_restart::{DatahubState, JOURNAL, VENUE, chain, down, open, send};

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

/// The report the OMS states about `event`, the venue event journaled at `cause`: a foreign one (these ids are no
/// session's), on the account the event's stream carried.
fn report_of(event: &Event, cause: u64, now_ms: i64) -> ExecutionRecordBodyWire {
    let base = |order_client_id: &str, at_ms: i64| ExecutionReportWire {
        caused_by_sequence: cause,
        at_ms,
        ownership: ExecutionOwnership::Foreign,
        account: Some("ALT".into()),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        order_client_id: order_client_id.into(),
        venue_order_id: None,
        mount_id: None,
        order: None,
        fill: None,
        fee_correction: None,
    };
    let report = match event {
        Event::Fill(f) => ExecutionReportWire {
            fill: Some(ExecutionReportFillWire {
                trade_id: f.trade_id.to_string(),
                side: f.side,
                qty: f.last_qty,
                price: f.last_price,
                commission: f.commission,
                commission_asset: f.commission_asset.to_string(),
                liquidity_side: f.liquidity_side.to_string(),
                mark_price: f.mark_price,
                position_side: (f.position_side != PositionSide::Both)
                    .then(|| f.position_side.to_string()),
            }),
            ..base(&f.order_client_id, f.timestamp)
        },
        Event::FeeAdjustment(a) => ExecutionReportWire {
            fee_correction: Some(ExecutionReportFeeCorrectionWire {
                corrects_trade_id: a.trade_id.to_string(),
                commission: a.extra_commission,
                commission_asset: a.commission_asset.to_string(),
                price: a.last_price,
            }),
            ..base(&a.order_client_id, now_ms)
        },
        other => panic!("no report for {other:?}"),
    };
    ExecutionRecordBodyWire::Report(Box::new(report))
}

/// Journal `events` and stream their reports to the datahub as one window, acknowledged.
fn journal_and_stream(
    dir: &std::path::Path,
    stream: &mut std::net::TcpStream,
    after: &mut Option<u64>,
    events: Vec<Event>,
) {
    let mut j = Journal::open(dir, JournalFileConfig::default()).unwrap();
    let mut bodies = Vec::new();
    for (i, e) in events.into_iter().enumerate() {
        let now_ms = 1_000 + i as i64;
        let cause = j.append_command(now_ms, &Ingest::Event(e.clone())).unwrap();
        // The report's own record follows its cause; any rising sequence past it will do here.
        bodies.push((cause * 2 + 1_000_000, report_of(&e, cause, now_ms)));
    }
    j.flush().unwrap();
    drop(j);
    let through = bodies.last().map(|(s, _)| *s).expect("a window holds records");
    send(stream, &ExecutionStreamUp::Records(chain(*after, bodies)));
    send(stream, &ExecutionStreamUp::Heartbeat { head_sequence: through });
    assert_eq!(down(stream), ExecutionStreamDown::Stored { through_sequence: through });
    *after = Some(through);
}

/// Every catalog row of a sheet in its shortest round-trip spelling, which tells any two bit patterns apart and,
/// unlike `==` on an `f64`, holds a metric that is `NaN` on both sides equal.
fn numbers(sheet: &TradingResultsTable) -> Vec<String> {
    sheet.rows().map(|(spec, v)| format!("{}={v:?}", spec.id)).collect()
}

#[test]
fn one_journal_gives_one_tearsheet_through_both_doors() {
    use PositionSide::{Long, Short};
    let dir = tempfile::tempdir().unwrap();
    let datahub = DatahubState::new();
    let (mut after, mut stream) = open(datahub.serve(None), JOURNAL);
    // Long 2 @ 100 with its fee not yet known; short 1 @ 110 beside it; the fee is stated.
    journal_and_stream(
        dir.path(),
        &mut stream,
        &mut after,
        vec![
            leg(Long, fill("t1", 1, 2.0, 100.0, 0.0, 1_000)),
            leg(Short, fill("t2", -1, 1.0, 110.0, 0.2, 2_000)),
            correction("t1", 1.00, 1_000),
        ],
    );
    // A later window: the long closes at 130 (+60), the short at 120 (-10), and the venue revises the fee.
    journal_and_stream(
        dir.path(),
        &mut stream,
        &mut after,
        vec![
            leg(Long, fill("t3", -1, 2.0, 130.0, 0.3, 3_000)),
            leg(Short, fill("t4", 1, 1.0, 120.0, 0.4, 4_000)),
            correction("t1", 1.25, 1_000),
        ],
    );

    let (journal, journal_equity, _) = trading_results_table_from_fills(
        fills_from_journal(dir.path()).unwrap(),
        TradingResultsTableFillOrder::AsHeld,
        SEED,
        252.0,
    );
    let rows = datahub.fills(SYMBOL);
    assert_eq!(rows.len(), 6, "four fills and two corrections, each once");
    let (stored, stored_equity, _) = trading_results_table_from_fills(
        ExecFillRow::fill_events(&rows),
        TradingResultsTableFillOrder::ByTime,
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
