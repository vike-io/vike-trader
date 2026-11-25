//! **Trading is never blocked or slowed by the datahub** (package 2's execution stream, plan C.2): with the datahub
//! DOWN (nothing listens on its address) or STUCK (it accepts and never reads a byte), orders submit and fill on a
//! paper core and its fold finishes within the same bound as a core with no publisher at all; then a datahub appears
//! and every report the OMS journaled lands, in journal order. And a fresh journal's FIRST record (sequence 0) is sent
//! like any other: the read cursor is `None` until the datahub holds something.
//!
//! The datahub is `crate::support::FakeDatahub`, the stream's grammar over loopback; the publisher is the shipped one
//! (`vike_tradehub::execution_stream`) with short waits.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, CoreHandle, JournalConfig, spawn_core};
use vike_datahub_client::execution_stream::ExecutionRecordBodyWire;
use vike_execution::testing::{TestExecutionClient, engine_on};
use vike_execution::{Command, OrderIntent};
use vike_journal::{Journal, JournalFileConfig, JournalRecord};
use vike_model::{OrderRequest, OrderType};

use crate::support::{
    DatahubDown, FakeDatahub, FakeDatahubMode, fill_report, journal_id_of, open_journal,
    start_execution_stream,
};

const ORDERS: usize = 20;
const QTY: f64 = 0.01;
/// What every case's fold must finish within, a datahub configured or not: the paper client fills each submit at once,
/// so this is a loaded lane's slack, and the property is that the datahub's state does not move it.
const FOLD_BOUND: Duration = Duration::from_secs(10);

/// A paper core on binance/BTCUSDT (every submit filled at once at its price) journaling into `dir`.
fn paper_core(dir: &Path) -> CoreHandle {
    let journal = JournalConfig {
        dir: dir.to_path_buf(),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 64,
    };
    spawn_core(
        engine_on("binance", "BTCUSDT", TestExecutionClient::new("binance", 100.0)),
        CoreConfig { seed_cash: 1_000_000.0, journal: Some(journal), ..CoreConfig::default() },
    )
}

/// Submit [`ORDERS`] limit buys and wait until the core has folded every fill: how long that took.
fn trade(handle: &CoreHandle, tag: &str) -> Duration {
    let cell = handle.snapshot_cell();
    let started = Instant::now();
    for i in 0..ORDERS {
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
            order_client_id: format!("{tag}{i}"),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: QTY,
            order_type: OrderType::Limit,
            price: Some(100.0),
            ..Default::default()
        }))));
    }
    let want = QTY * ORDERS as f64;
    loop {
        let held: f64 = cell.load().positions.iter().map(|p| p.size).sum();
        if (held - want).abs() < 1e-9 {
            return started.elapsed();
        }
        assert!(
            started.elapsed() < FOLD_BOUND,
            "{tag}: the core folded {held} of {want} within {FOLD_BOUND:?}: {:?}",
            cell.load().recent_events
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The sequences of every `ExecutionReport` the journal at `dir` holds, in journal order.
fn journaled_reports(dir: &Path) -> Vec<u64> {
    Journal::read_all(dir)
        .expect("the journal reads")
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ExecutionReport { sequence, .. } => Some(*sequence),
            _ => None,
        })
        .collect()
}

#[test]
fn trading_is_not_blocked_while_the_datahub_is_down_and_every_report_lands_after() {
    // the bound a core with no datahub configured folds within
    let alone = tempfile::tempdir().expect("temp");
    let handle = paper_core(alone.path());
    let without = trade(&handle, "alone");
    handle.shutdown_and_join();

    for mode in [None, Some(FakeDatahubMode::Hold)] {
        let dir = tempfile::tempdir().expect("temp");
        let handle = paper_core(dir.path());
        // down: a held port that closes every connection unanswered; stuck: a listener that accepts and never reads
        let stuck = mode.map(|m| FakeDatahub::start(None, m));
        let down = stuck.is_none().then(DatahubDown::start);
        let addr = stuck.as_ref().map(|d| d.addr).or(down.as_ref().map(|d| d.addr));
        let addr = addr.expect("a datahub, down or stuck");
        let stop = Arc::new(AtomicBool::new(false));
        let watch = handle.journal_durable_watch().expect("a journaling core");
        let publisher = start_execution_stream(dir.path(), watch, addr, None, Arc::clone(&stop));
        let with = trade(&handle, "op");
        assert!(with < FOLD_BOUND, "{mode:?}: the fold took {with:?} (alone: {without:?})");

        // a datahub appears: every report the OMS journaled lands, in journal order
        let datahub = match stuck {
            Some(stuck) => {
                stuck.serve();
                stuck
            }
            None => FakeDatahub::start(down.map(DatahubDown::up), FakeDatahubMode::Serve),
        };
        handle.shutdown_and_join();
        let journaled = journaled_reports(dir.path());
        assert!(journaled.len() >= 2 * ORDERS, "{mode:?}: every order reported: {journaled:?}");
        let journal_id = journal_id_of(dir.path());
        let sent_reports = |d: &FakeDatahub| -> Vec<u64> {
            d.stream(&journal_id)
                .records
                .iter()
                .filter(|r| matches!(r.body, ExecutionRecordBodyWire::Report(_)))
                .map(|r| r.sequence)
                .collect()
        };
        datahub.wait_until("every journaled report lands", |d| {
            sent_reports(d).len() >= journaled.len()
        });
        assert_eq!(
            sent_reports(&datahub),
            journaled,
            "{mode:?}: every report, once, in journal order"
        );
        stop.store(true, Ordering::Relaxed);
        publisher.join();
    }
}

/// **A fresh journal's record 0 reaches the datahub**: the first contact reads from the journal's first record, so
/// a report the journal numbered 0 (a fresh journal's first record, say the boot's unverified window after a move
/// aside) is sent, chained from the datahub's `0`.
#[test]
fn a_fresh_journals_record_zero_reaches_the_datahub() {
    let dir = tempfile::tempdir().expect("temp");
    let mut journal = open_journal(dir.path());
    assert_eq!(journal.append_execution_report(10, &fill_report("op1", "t0")).expect("append"), 0);
    journal.append_execution_report(11, &fill_report("op1", "t1")).expect("append");
    journal.flush().expect("flush");
    let datahub = FakeDatahub::start(None, FakeDatahubMode::Serve);
    let stop = Arc::new(AtomicBool::new(false));
    let publisher = start_execution_stream(
        dir.path(),
        journal.durable_watch(),
        datahub.addr,
        None,
        Arc::clone(&stop),
    );
    let journal_id = journal_id_of(dir.path());
    datahub.wait_until("both records land", |d| d.stream(&journal_id).records.len() == 2);
    let records = datahub.stream(&journal_id).records;
    assert_eq!(
        records
            .iter()
            .map(|r| (r.sequence, r.previous_sequence, r.recorded_ms))
            .collect::<Vec<_>>(),
        [(0, None, 10), (1, Some(0), 11)],
        "record 0 first, the chain's start, and record 1 chained from it"
    );
    stop.store(true, Ordering::Relaxed);
    publisher.join();
    let floor =
        vike_journal::JournalExecutionStream::load(dir.path()).expect("reads").expect("written");
    assert_eq!(
        floor.acknowledged_sequence,
        Some(1),
        "the datahub's acknowledgement is the journal's floor"
    );
}
