//! **The execution stream end to end: the tradehub's publisher against the datahub's REAL sink**
//! (`vike_datahub::execution_stream` behind the real server, a real `vike.db` for its cursor and the in-memory store
//! double for its rows), never the fake of `support.rs`: what the two halves agree on is what these tests hold.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_data::{ExecFillRow, ExecOrderRow, HistoryStore, HistoryStoreMemory};
use vike_datahub::execution_stream::{ExecutionStreamFaultPoint, ExecutionStreamLane};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};

use crate::support::{
    DatahubDown, fill_report, journal_id_of, open_journal, start_execution_stream,
};

/// A datahub's durable state: the store its rows land in and the settings directory its cursors live in.
struct RealDatahub {
    store: Arc<HistoryStoreMemory>,
    settings: tempfile::TempDir,
}

impl RealDatahub {
    fn new() -> Self {
        let settings = tempfile::tempdir().expect("temp");
        vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8")))
            .expect("an empty store");
        RealDatahub { store: Arc::new(HistoryStoreMemory::new()), settings }
    }

    /// The real server over this state, with the execution stream's lane, on `listener` (an ephemeral port for
    /// `None`); its session dies once at `crash`, where a killed datahub would have stopped, and the lane serves on.
    fn serve(
        &self,
        listener: Option<TcpListener>,
        crash: Option<ExecutionStreamFaultPoint>,
    ) -> SocketAddr {
        let listener =
            listener.unwrap_or_else(|| TcpListener::bind("127.0.0.1:0").expect("bind the datahub"));
        let addr = listener.local_addr().expect("addr");
        let mut lane = ExecutionStreamLane::new(self.settings.path().to_path_buf(), None);
        if let Some(point) = crash {
            let fired = Arc::new(AtomicBool::new(false));
            lane = lane.with_fault_points(Arc::new(move |p| {
                p == point && !fired.swap(true, Ordering::SeqCst)
            }));
        }
        let store: Arc<dyn HistoryStore + Send + Sync> = self.store.clone();
        std::thread::spawn(move || {
            let lanes =
                ServerLanes { execution_stream: Some(Arc::new(lane)), ..ServerLanes::default() };
            let timings = ServerTimings::PRODUCTION;
            let _ = serve_with_lanes(listener, store, None, None, None, None, lanes, None, timings);
        });
        addr
    }

    fn fills(&self) -> Vec<ExecFillRow> {
        self.store.scan_exec_fills("binance", "BTCUSDT").expect("fills")
    }

    fn orders(&self) -> Vec<ExecOrderRow> {
        self.store.scan_exec_orders("binance", "BTCUSDT").expect("orders")
    }

    /// The datahub's cursor of `journal_id`, as its `vike.db` holds it.
    fn stored_through(&self, journal_id: &str) -> Option<u64> {
        vike_secrets::execution_streams::read_execution_stream(self.settings.path(), journal_id)
            .expect("the cursor reads")
            .and_then(|row| row.stored_through)
    }

    /// Wait (20 s at most) until `done` holds.
    fn wait_until(&self, what: &str, done: impl Fn(&RealDatahub) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(self) {
            assert!(Instant::now() < deadline, "{what}: fills {:?}", trade_ids(&self.fills()));
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn trade_ids(rows: &[ExecFillRow]) -> Vec<String> {
    rows.iter().map(|r| r.trade_id.clone()).collect()
}

/// A filled market order's report, ours, on account `A` under mount `maker-a`: one fill row and one order row.
fn report(trade_id: &str) -> vike_model::orders::ExecutionReport {
    vike_model::orders::ExecutionReport {
        account: Some("A".into()),
        mount_id: Some("maker-a".into()),
        order: Some(vike_model::orders::ExecutionReportOrder {
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Market,
            status: vike_model::OrderStatus::Filled,
            price: None,
            trigger_price: None,
            filled_qty: 1.0,
            average_fill_price: 100.0,
        }),
        ..fill_report(&format!("c-{trade_id}"), trade_id)
    }
}

/// **A fresh journal's record 0 is stored once, with no resend loop.** Its first record is sequence 0, so the chain
/// starts at `None`: the datahub holds nothing, answers the open with `None`, and takes a record chained from `None`
/// as the start. A datahub that read `0` as "nothing" would ask for a resend from 0 for ever and store nothing.
#[test]
fn a_fresh_journals_record_zero_is_stored_once_by_the_real_sink() {
    let datahub = RealDatahub::new();
    let addr = datahub.serve(None, None);
    let dir = tempfile::tempdir().expect("temp");
    let mut journal = open_journal(dir.path());
    let first = journal.append_execution_report(10, &report("t0")).expect("append");
    assert_eq!(first, 0, "the journal's first record is a report");
    journal.append_execution_report(11, &report("t1")).expect("append");
    journal.flush().expect("flush");
    let stop = Arc::new(AtomicBool::new(false));
    let publisher =
        start_execution_stream(dir.path(), journal.durable_watch(), addr, None, Arc::clone(&stop));
    let journal_id = journal_id_of(dir.path());
    datahub.wait_until("both reports are stored", |d| d.stored_through(&journal_id) == Some(1));
    stop.store(true, Ordering::Relaxed);
    publisher.join();

    assert_eq!(trade_ids(&datahub.fills()), ["t0", "t1"], "record 0 once, then record 1");
    let sequences: Vec<Option<i64>> = datahub.orders().iter().map(|o| o.journal_sequence).collect();
    assert_eq!(sequences, [Some(0), Some(1)], "each record's order row, once");
    let floor =
        vike_journal::JournalExecutionStream::load(dir.path()).expect("reads").expect("written");
    assert_eq!(floor.acknowledged_sequence, Some(1), "the acknowledgement is the journal's floor");
}

/// **End to end through what goes wrong: every report lands ONCE, in journal order, with its account and mount.**
///
/// 1. The datahub is DOWN while the tradehub journals: the publisher only redials.
/// 2. It comes UP and its session DIES in the middle of the first window (the fill series landed, the order series
///    and the cursor did not), where a killed datahub would have stopped; the publisher redials and RESENDS the
///    window from the datahub's cursor, and the rewrite under the same keys adds no second fill row.
/// 3. Reports journaled while connected follow.
/// 4. The tradehub's publisher restarts and resends from the datahub's cursor: nothing stored is sent twice.
#[test]
fn every_report_lands_once_in_order_through_a_datahub_down_a_mid_window_crash_and_a_resend() {
    let dir = tempfile::tempdir().expect("temp");
    let mut journal = open_journal(dir.path());
    let append = |journal: &mut vike_journal::Journal, ids: std::ops::Range<i64>| {
        for i in ids {
            journal.append_execution_report(10 + i, &report(&format!("t{i}"))).expect("append");
        }
        journal.flush().expect("flush");
    };
    append(&mut journal, 0..4);
    let down = DatahubDown::start();
    let addr = down.addr;
    let stop = Arc::new(AtomicBool::new(false));
    let publisher =
        start_execution_stream(dir.path(), journal.durable_watch(), addr, None, Arc::clone(&stop));
    let journal_id = journal_id_of(dir.path());
    std::thread::sleep(Duration::from_millis(300)); // a few redials against a datahub that is down

    let datahub = RealDatahub::new();
    datahub.serve(Some(down.up()), Some(ExecutionStreamFaultPoint::AfterSeries { landed: 1 }));
    datahub.wait_until("the crashed window, resent", |d| d.stored_through(&journal_id) == Some(3));
    append(&mut journal, 4..6);
    datahub.wait_until("the reports journaled while connected", |d| {
        d.stored_through(&journal_id) == Some(5)
    });
    stop.store(true, Ordering::Relaxed);
    publisher.join();

    let restarted = Arc::new(AtomicBool::new(false));
    let publisher = start_execution_stream(
        dir.path(),
        journal.durable_watch(),
        addr,
        None,
        Arc::clone(&restarted),
    );
    append(&mut journal, 6..7);
    datahub
        .wait_until("the report after the restart", |d| d.stored_through(&journal_id) == Some(6));
    restarted.store(true, Ordering::Relaxed);
    publisher.join();

    let want: Vec<String> = (0..7).map(|i| format!("t{i}")).collect();
    let fills = datahub.fills();
    assert_eq!(trade_ids(&fills), want, "every fill once, in journal order");
    let orders = datahub.orders();
    let sequences: Vec<Option<i64>> = orders.iter().map(|o| o.journal_sequence).collect();
    assert_eq!(
        sequences,
        (0..7).map(Some).collect::<Vec<_>>(),
        "every order row once, in journal order"
    );
    let attributed = |account: &Option<String>, mount: &Option<String>| {
        account.as_deref() == Some("A") && mount.as_deref() == Some("maker-a")
    };
    assert!(fills.iter().all(|r| attributed(&r.account, &r.mount_id)), "{fills:?}");
    assert!(orders.iter().all(|r| attributed(&r.account, &r.mount_id)), "{orders:?}");
}
