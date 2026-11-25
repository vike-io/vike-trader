//! The sink's mapping, one record at a time, and the two store reads it makes: no socket and no cursor.

use std::sync::Arc;

use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub_client::execution_stream::{
    ExecutionForecastWire, ExecutionPositionSnapshotAccountWire, ExecutionPositionSnapshotWire,
    ExecutionReportFeeCorrectionWire, ExecutionReportFillWire, ExecutionReportOrderWire,
    ExecutionUnverifiedWire,
};
use vike_model::AccountPosition;
use vike_model::events::PositionSide;

use super::*;

const JOURNAL: &str = "0123456789abcdef0123456789abcdef";

fn report(order_client_id: &str) -> ExecutionReportWire {
    ExecutionReportWire {
        caused_by_sequence: 40,
        at_ms: 1_000,
        ownership: ExecutionOwnership::Ours,
        account: Some("ALT".into()),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        order_client_id: order_client_id.into(),
        venue_order_id: Some("v-9".into()),
        mount_id: Some("maker-a".into()),
        order: None,
        fill: None,
        fee_correction: None,
    }
}

fn fill() -> ExecutionReportFillWire {
    ExecutionReportFillWire {
        trade_id: "t1".into(),
        side: 1,
        qty: 0.5,
        price: 65_000.0,
        commission: 0.13,
        commission_asset: "BNB".into(),
        liquidity_side: "maker".into(),
        mark_price: Some(65_001.0),
        position_side: Some("LONG".into()),
    }
}

fn order() -> ExecutionReportOrderWire {
    ExecutionReportOrderWire {
        side: 1,
        qty: 1.0,
        order_type: "LIMIT".into(),
        status: "PARTIALLY_FILLED".into(),
        price: Some(65_000.0),
        trigger_price: None,
        filled_qty: 0.5,
        average_fill_price: 65_000.0,
        reason: None,
    }
}

fn record(sequence: u64, body: ExecutionRecordBodyWire) -> ExecutionRecordWire {
    ExecutionRecordWire {
        sequence,
        previous_sequence: sequence.checked_sub(1),
        recorded_ms: 1_005,
        body,
    }
}

fn window(records: &[ExecutionRecordWire]) -> Window<'_> {
    Window {
        journal_id: JOURNAL,
        after: Some(0),
        through: 99,
        records,
        first_contact_through: 0,
        generation: 3,
    }
}

/// **One record alone makes a whole row**: a fill on a known order yields one `exec_fill` row and one `exec_order`
/// row, every field from the record itself; a fee correction is `fee_correction_row`'s shape; a forecast is its
/// row at the record's time; an unverified record is a gap row; and a FOREIGN report carries no mount.
#[test]
fn one_record_alone_makes_a_whole_row() {
    let fee = ExecutionReportFeeCorrectionWire {
        corrects_trade_id: "t1".into(),
        commission: 0.25,
        commission_asset: "BNB".into(),
        price: 65_000.0,
    };
    let records = [
        record(
            41,
            ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire {
                fill: Some(fill()),
                order: Some(order()),
                ..report("c1")
            })),
        ),
        record(
            42,
            ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire {
                fee_correction: Some(fee),
                ownership: ExecutionOwnership::Foreign,
                ..report("c1")
            })),
        ),
        record(
            43,
            ExecutionRecordBodyWire::Forecast(ExecutionForecastWire {
                mount_id: "maker-a".into(),
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                value: 0.7,
                horizon_ms: 60_000,
                confidence: None,
            }),
        ),
        record(
            44,
            ExecutionRecordBodyWire::Unverified(ExecutionUnverifiedWire {
                journal_id: JOURNAL.into(),
                after_sequence: 30,
                through_sequence: 39,
                from_ms: Some(1),
                to_ms: Some(2),
                order_client_ids: vec!["c1".into()],
                reason: "a snapshot-only restore".into(),
            }),
        ),
    ];
    let rows = rows_of(&window(&records), 7);
    let series = ("binance".to_string(), "BTCUSDT".to_string());
    let fills = &rows.fills[&series];
    assert_eq!(
        fills[0],
        ExecFillRow {
            ts: 1_000,
            trade_id: "t1".into(),
            order_client_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 0.5,
            px: 65_000.0,
            commission: 0.13,
            mark_price: Some(65_001.0),
            liquidity_side: "maker".into(),
            commission_asset: "BNB".into(),
            venue_order_id: Some("v-9".into()),
            account: Some("ALT".into()),
            position_side: Some("LONG".into()),
            corrects_trade_id: None,
            correction_seq: None,
            mount_id: Some("maker-a".into()),
            received_ms: None,
            ownership: Some(OWNERSHIP_OURS.into()),
            journal_sequence: Some(41),
            journal_generation: Some(3),
        }
    );
    assert_eq!(
        fills[1],
        ExecFillRow {
            ts: 1_000,
            trade_id: String::new(),
            order_client_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 0,
            qty: 0.0,
            px: 65_000.0,
            commission: 0.25,
            mark_price: None,
            liquidity_side: String::new(),
            commission_asset: "BNB".into(),
            venue_order_id: Some("v-9".into()),
            account: Some("ALT".into()),
            position_side: None,
            corrects_trade_id: Some("t1".into()),
            correction_seq: Some(40),
            mount_id: None,
            received_ms: Some(1_005),
            ownership: Some(OWNERSHIP_FOREIGN.into()),
            journal_sequence: Some(42),
            journal_generation: Some(3),
        },
        "a foreign report's mount is dropped whatever it said"
    );
    assert_eq!(
        rows.orders[&series],
        vec![ExecOrderRow {
            ts: 1_000,
            order_client_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "LIMIT".into(),
            status: "PARTIALLY_FILLED".into(),
            price: Some(65_000.0),
            trigger_price: None,
            venue_order_id: Some("v-9".into()),
            filled_qty: 0.5,
            avg_fill_px: 65_000.0,
            account: Some("ALT".into()),
            mount_id: Some("maker-a".into()),
            ownership: Some(OWNERSHIP_OURS.into()),
            journal_sequence: Some(41),
            journal_generation: Some(3),
            reason: None,
        }]
    );
    assert_eq!(
        rows.forecasts[&series],
        vec![ExecForecastRow {
            ts: 1_005,
            mount_id: "maker-a".into(),
            value: 0.7,
            horizon_ms: 60_000,
            confidence: None,
        }]
    );
    assert_eq!(
        rows.unverified,
        vec![ExecutionStreamGapRow {
            journal_id: JOURNAL.into(),
            kind: GAP_UNVERIFIED.into(),
            after_sequence: 30,
            resumes_at_sequence: 40,
            lost_from_ms: Some(1),
            lost_to_ms: Some(2),
            noted_ms: 7,
            reason: Some("a snapshot-only restore".into()),
            order_client_ids: vec!["c1".into()],
        }]
    );
    assert_eq!(rows.counts, SinkCounts::default());
}

/// **A DENIED report keeps its words through the sink**: the gate's reason is the order row's `reason`, so the
/// operator reads why the core refused it, and a report that states none writes none.
#[test]
fn a_denied_report_keeps_its_words_through_the_sink() {
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    let denied = ExecutionReportOrderWire {
        status: "DENIED".into(),
        filled_qty: 0.0,
        average_fill_price: 0.0,
        reason: Some("RiskGate: max_notional_per_order 500 exceeded".into()),
        ..order()
    };
    let records = [
        record(
            1,
            ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire {
                venue_order_id: None,
                order: Some(denied),
                ..report("c-denied")
            })),
        ),
        record(
            2,
            ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire {
                order: Some(order()),
                ..report("c-plain")
            })),
        ),
    ];
    write_window(store.as_ref(), &window(&records), 0, None, &|_| false).expect("written");
    let reasons: Vec<(String, String, Option<String>)> = store
        .scan_exec_orders("binance", "BTCUSDT")
        .unwrap()
        .into_iter()
        .map(|r| (r.order_client_id, r.status, r.reason))
        .collect();
    assert_eq!(
        reasons,
        vec![
            (
                "c-denied".to_string(),
                "DENIED".to_string(),
                Some("RiskGate: max_notional_per_order 500 exceeded".to_string())
            ),
            ("c-plain".to_string(), "PARTIALLY_FILLED".to_string(), None),
        ]
    );
}

/// A venue that is no catalog venue, a symbol the store cannot name and an order row with no order client id are
/// SKIPPED and counted, never the window; a user name is filed under its store name.
#[test]
fn a_record_the_store_cannot_name_is_skipped_and_counted() {
    let records = [
        record(
            1,
            ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire {
                venue: "../etc".into(),
                fill: Some(fill()),
                ..report("c1")
            })),
        ),
        record(
            2,
            ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire {
                symbol: String::new(),
                fill: Some(fill()),
                ..report("c1")
            })),
        ),
        record(
            3,
            ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire {
                order: Some(order()),
                ..report("")
            })),
        ),
        record(
            4,
            ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire {
                venue: "hyperliquid".into(),
                symbol: "XYZ:TSLAUSDC.P".into(),
                fill: Some(fill()),
                ..report("c1")
            })),
        ),
        record(
            5,
            ExecutionRecordBodyWire::PositionSnapshot(ExecutionPositionSnapshotWire {
                accounts: vec![
                    ExecutionPositionSnapshotAccountWire {
                        venue: "../etc".into(),
                        account: "MAIN".into(),
                        positions: Vec::new(),
                        unknown: None,
                    },
                    ExecutionPositionSnapshotAccountWire {
                        venue: "binance".into(),
                        account: "MAIN".into(),
                        positions: vec![AccountPosition {
                            symbol: "BTCUSDT".into(),
                            position_side: PositionSide::Both,
                            size: -1.0,
                            average_price: 99.5,
                        }],
                        unknown: None,
                    },
                ],
            }),
        ),
    ];
    let rows = rows_of(&window(&records), 0);
    assert_eq!(rows.counts.unnamed, 4, "the snapshot's account on no catalog venue too");
    let filed: Vec<_> = rows.fills.keys().cloned().collect();
    assert_eq!(filed, vec![("hyperliquid".to_string(), "XYZ-TSLAUSDC.P".to_string())]);
    assert_eq!(rows.fills[&filed[0]][0].symbol, "XYZ:TSLAUSDC.P", "the row keeps the booked name");
    assert_eq!(
        rows.account_positions,
        vec![ExecutionAccountPositionsRow {
            journal_id: JOURNAL.into(),
            generation: 3,
            sequence: 5,
            as_of_ms: 1_005,
            venue: "binance".into(),
            account: "MAIN".into(),
            unknown: None,
            positions: vec![AccountPosition {
                symbol: "BTCUSDT".into(),
                position_side: PositionSide::Both,
                size: -1.0,
                average_price: 99.5,
            }],
        }],
        "one row per account, under the window's journal and generation, the record's sequence and time"
    );
}

/// **A late fill without a venue order id takes its order's** (a fill after a restart, on an order the OMS no
/// longer holds): from the newest order row of that order in the same series, the window's own first, else the
/// store's. With none it stays `None`, counted.
#[test]
fn a_late_fill_without_a_venue_order_id_takes_its_orders() {
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    let stored = ExecOrderRow {
        ts: 10,
        order_client_id: "c-old".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "LIMIT".into(),
        status: "CANCELED".into(),
        price: Some(1.0),
        trigger_price: None,
        venue_order_id: Some("v-old".into()),
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        account: Some("ALT".into()),
        mount_id: None,
        ownership: Some(OWNERSHIP_OURS.into()),
        journal_sequence: Some(3),
        journal_generation: None,
        reason: None,
    };
    store.append_exec_orders("binance", "BTCUSDT", &[stored], Some("seed")).unwrap();
    let late = |id: &str, trade: &str| ExecutionReportWire {
        venue_order_id: None,
        fill: Some(ExecutionReportFillWire { trade_id: trade.into(), ..fill() }),
        ..report(id)
    };
    let records = [
        record(5, ExecutionRecordBodyWire::Report(Box::new(late("c-old", "t1")))),
        record(
            6,
            ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire {
                venue_order_id: Some("v-new".into()),
                order: Some(order()),
                ..report("c-new")
            })),
        ),
        record(7, ExecutionRecordBodyWire::Report(Box::new(late("c-new", "t2")))),
        record(8, ExecutionRecordBodyWire::Report(Box::new(late("c-gone", "t3")))),
    ];
    let w = Window {
        journal_id: JOURNAL,
        after: Some(4),
        through: 8,
        records: &records,
        first_contact_through: 0,
        generation: 1,
    };
    let written = write_window(store.as_ref(), &w, 0, None, &|_| false).expect("written");
    assert_eq!(written.counts.venue_order_id_unknown, 1, "c-gone has no order row");
    let ids: Vec<(String, Option<String>)> = store
        .scan_exec_fills("binance", "BTCUSDT")
        .unwrap()
        .into_iter()
        .map(|r| (r.trade_id, r.venue_order_id))
        .collect();
    assert_eq!(
        ids,
        vec![
            ("t1".to_string(), Some("v-old".to_string())),
            ("t2".to_string(), Some("v-new".to_string())),
            ("t3".to_string(), None),
        ]
    );
}

/// Below the free-space floor a window stores no forecast and says why; the fills are written as ever.
#[test]
fn below_the_floor_a_window_stores_no_forecast() {
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    let records = [
        record(
            1,
            ExecutionRecordBodyWire::Forecast(ExecutionForecastWire {
                mount_id: "m".into(),
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                value: 1.0,
                horizon_ms: 1,
                confidence: Some(0.5),
            }),
        ),
        record(
            2,
            ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire {
                fill: Some(fill()),
                ..report("c1")
            })),
        ),
    ];
    let floor = || Some("the disk is full".to_string());
    let written = write_window(store.as_ref(), &window(&records), 0, Some(&floor), &|_| false)
        .expect("written");
    assert_eq!(written.forecasts_held_back.as_deref(), Some("the disk is full"));
    assert_eq!(written.counts.forecasts_held_back, 1);
    let all = vike_data::TimestampRange { start: None, end: None };
    assert!(store.scan_forecasts("binance", "BTCUSDT", all).unwrap().is_empty());
    assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap().len(), 1);
}
