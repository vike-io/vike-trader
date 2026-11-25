//! **No execution is lost to a crash or to a journal moved aside** (package 2's execution stream, plan C.1b and C.2),
//! end to end through the publisher:
//!
//! - a core that panics on a fill AFTER journaling its cause and BEFORE the fold (`CoreConfig::panic_on_trade_id`),
//!   its journal cut right after that cause as a crash there would leave it, holds the cause and no report; the next
//!   boot's restore regenerates the report, the next core journals it, and the datahub ends with that fill, the order's
//!   state after it, its account, and its mount (named from the durable `strategy_code` row: no mount of the new core
//!   carries the code);
//! - a journal moved aside is DRAINED before its successor opens a stream: its readable records under its own id,
//!   then one unverified window for what lies past the place its read halts, and only then the successor (the datahub
//!   orders one order's rows across a move by the order it first heard from each journal).
//!
//! The datahub is `crate::support::FakeDatahub`; the core, the restore and the publisher are the shipped ones.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, CoreHandle, JournalConfig, spawn_core};
use vike_datahub_client::execution_stream::ExecutionRecordBodyWire;
use vike_execution::testing::{RecordingClient, engine_on};
use vike_execution::{Command, GatewayEvent, OrderIntent};
use vike_journal::{
    JournalExecutionStream, JournalExecutionStreamPredecessor, JournalFileConfig, JournalRecord,
};
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_model::orders::client_id::{
    OrderClientIdAccountCode, OrderClientIdGenerator, OrderClientIdIntent, OrderClientIdLabels,
    OrderClientIdPlan, OrderClientIdShape, OrderClientIdSource, OrderClientIdStrategyCode,
};
use vike_model::{EventRingCursor, OrderRequest, OrderType};

use crate::support::{
    FakeDatahub, FakeDatahubMode, binance_fill, fill_report, journal_id_of, open_journal,
    start_execution_stream,
};

/// The core's own v2 session, untagged, 5 ids into its life.
const SESSION: &str = "W00000000abcd";

/// The id [`SESSION`] minted at `sequence` for strategy code `code`.
fn minted(sequence: u64, code: OrderClientIdStrategyCode) -> String {
    let mut generator = OrderClientIdGenerator::resume(SESSION.into(), sequence);
    let plan = OrderClientIdPlan::new(OrderClientIdShape::FULL, OrderClientIdAccountCode::of_id(3));
    let labels = OrderClientIdLabels::new(
        OrderClientIdSource::Strategy,
        code,
        OrderClientIdIntent::OpenLong,
    );
    plan.mint(&mut generator, labels)
}

fn journal_at(dir: &Path) -> JournalConfig {
    // a `Snapshot` after every record: the crash leaves exactly its cause after the last one
    JournalConfig {
        dir: dir.to_path_buf(),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 1,
    }
}

/// A core on binance/BTCUSDT whose venue rests every order, in [`SESSION`] at sequence 5.
fn core_config(dir: &Path) -> CoreConfig {
    CoreConfig {
        seed_cash: 1_000_000.0,
        order_client_id_session: Some((SESSION.into(), 5)),
        journal: Some(journal_at(dir)),
        ..CoreConfig::default()
    }
}

fn binance_core(config: CoreConfig) -> CoreHandle {
    spawn_core(engine_on("binance", "BTCUSDT", RecordingClient::default()), config)
}

fn wait(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// **The journal at `dir` as a crash right after the first record `is_the_cut` picks leaves it** (decision 0279: a
/// crash takes only the tail): the one segment zeroed past that record's frame. The journal was closed cleanly, so no
/// durable copy is left to repair the tail back.
fn cut_after(dir: &Path, is_the_cut: impl Fn(&JournalRecord) -> bool) {
    let segment = segment_of(dir);
    let mut bytes = std::fs::read(&segment).expect("read the segment");
    // The records through the journal's own reader (its frames are msgpack), then their frames by length.
    let cut = vike_journal::Journal::read_all(dir)
        .expect("read the journal")
        .iter()
        .position(|record: &JournalRecord| is_the_cut(record))
        .expect("the record to cut after is in the journal");
    let mut at = 16;
    for _ in 0..=cut {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes")) as usize;
        assert!(len > 0, "one segment holds every record");
        at += 8 + len;
    }
    bytes[at..].fill(0);
    std::fs::write(&segment, &bytes).expect("write the cut segment");
}

#[test]
fn a_fill_the_core_crashed_on_reaches_the_datahub_after_the_restart() {
    // the settings store whose `strategy_code` row names the mount; no core below mounts it
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(settings.path()))
        .expect("a store");
    let code =
        vike_secrets::strategy_code::strategy_code_for(settings.path(), "mount-m", "momentum")
            .expect("a code");
    let order = minted(2, OrderClientIdStrategyCode::new(code).expect("in range"));

    // the first core: the order rests, is accepted, and the core dies on its fill
    let live = tempfile::tempdir().expect("temp");
    let handle = binance_core(CoreConfig {
        panic_on_trade_id: Some("boom".into()),
        ..core_config(live.path())
    });
    let cell = handle.snapshot_cell();
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        order_client_id: order.clone(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    }))));
    wait("the order rests", || {
        cell.load().orders.iter().any(|o| o.order_client_id == order.as_str())
    });
    let events = handle.event_sender();
    events
        .blocking_send(Event::OrderSubmitted(OrderSubmitted {
            order_client_id: order.clone(),
            timestamp: 5,
        }))
        .expect("sent");
    events
        .blocking_send(Event::OrderAccepted(OrderAccepted {
            order_client_id: order.clone(),
            venue_order_id: Some("V1".into()),
            timestamp: 6,
        }))
        .expect("sent");
    events
        .blocking_send_gateway(GatewayEvent {
            event: Event::Fill(binance_fill(&order, "boom", 1.0, 7)),
            route_key: "binance".into(),
            cursor: EventRingCursor { gateway_epoch: 1, sequence: 1, timestamp_ms: 7 },
        })
        .expect("sent");
    wait("the injected fault", || cell.load().fault.is_some());
    handle.shutdown_and_join();
    // the cause journaled, its fold and report never: the journal a crash right there leaves
    let is_the_cause = |r: &JournalRecord| {
        matches!(r, JournalRecord::Command { message, .. }
            if matches!(message.venue_event(), Some(Event::Fill(f)) if f.trade_id == "boom"))
    };
    cut_after(live.path(), is_the_cause);
    let reported = |r: &JournalRecord| {
        matches!(r, JournalRecord::ExecutionReport { report, .. }
            if report.fill.as_ref().is_some_and(|f| f.trade_id == "boom"))
    };
    let crashed = live;
    let before = vike_journal::Journal::read_all(crashed.path()).expect("reads");
    assert!(
        before.iter().any(is_the_cause) && !before.iter().any(reported),
        "the cause, and no report"
    );

    // the restart: the boot restore regenerates the report, and the next core journals it
    let scratch = tempfile::tempdir().expect("temp");
    let restore = vike_core::replay::restore_for_boot(
        Some(&journal_at(crashed.path())),
        scratch.path(),
        vike_model::now_ms(),
        &[],
    )
    .expect("the journal restores");
    let mut config = core_config(crashed.path());
    restore.apply_to(&mut config);
    let handle = binance_core(config);
    let datahub = FakeDatahub::start(None, FakeDatahubMode::Serve);
    let stop = Arc::new(AtomicBool::new(false));
    let publisher = start_execution_stream(
        crashed.path(),
        handle.journal_durable_watch().expect("a journaling core"),
        datahub.addr,
        Some(settings.path().to_path_buf()),
        Arc::clone(&stop),
    );
    let journal_id = journal_id_of(crashed.path());
    let the_fill = |d: &FakeDatahub| {
        d.stream(&journal_id).records.into_iter().find_map(|r| match r.body {
            ExecutionRecordBodyWire::Report(report)
                if report.fill.as_ref().is_some_and(|f| f.trade_id == "boom") =>
            {
                Some(report)
            }
            _ => None,
        })
    };
    datahub.wait_until("the crashed fill reaches the datahub", |d| the_fill(d).is_some());
    let report = the_fill(&datahub).expect("the fill");
    assert_eq!(report.ownership, vike_model::orders::ExecutionOwnership::Ours);
    assert_eq!(report.account.as_deref(), Some("DEFAULT"), "the ring's account");
    assert_eq!(report.mount_id.as_deref(), Some("mount-m"), "the mount its strategy code names");
    assert_eq!(report.venue_order_id.as_deref(), Some("V1"));
    let state = report.order.as_ref().expect("the order after the fill");
    assert_eq!(
        (state.qty, state.price),
        (1.0, Some(100.0)),
        "the order as the OMS holds it: {state:?}"
    );
    stop.store(true, Ordering::Relaxed);
    publisher.join();
    handle.shutdown_and_join();
    // exactly what the restore regenerated and the next core journaled, the removed mount named
    let regenerated = vike_journal::Journal::read_all(crashed.path())
        .expect("reads")
        .into_iter()
        .find_map(|r| match r {
            JournalRecord::ExecutionReport { report, .. }
                if report.fill.as_ref().is_some_and(|f| f.trade_id == "boom") =>
            {
                Some(report)
            }
            _ => None,
        })
        .expect("the regenerated report is journaled");
    assert_eq!(
        (regenerated.mount_id.as_deref(), regenerated.strategy_code.is_some()),
        (None, true)
    );
    let named =
        vike_model::orders::ExecutionReport { mount_id: Some("mount-m".into()), ..regenerated };
    assert_eq!(
        *report,
        vike_datahub_client::execution_stream::ExecutionReportWire::from(&named),
        "the datahub holds the journaled report, field for field"
    );
    // the journal's `Snapshot`s named this install's session: it is kept in the settings store
    let sessions =
        vike_secrets::order_client_id_sessions::read_order_client_id_sessions(settings.path())
            .expect("reads");
    assert!(sessions.iter().any(|(s, n)| s == SESSION && *n >= 5), "{sessions:?}");
}

/// The end of a segment's written frames: the frame heads chained from the header to the first zero length.
fn end_of_frames(segment: &Path) -> usize {
    let bytes = std::fs::read(segment).expect("read the segment");
    let mut at = 16;
    loop {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes")) as usize;
        if len == 0 {
            return at;
        }
        at += 8 + len;
    }
}

/// The one segment file of the journal at `dir`.
fn segment_of(dir: &Path) -> PathBuf {
    std::fs::read_dir(dir)
        .expect("read")
        .map(|e| e.expect("entry").path())
        .find(|p| p.extension().is_some_and(|x| x == "vjl"))
        .expect("a segment")
}

#[test]
fn a_moved_aside_journal_is_drained_before_its_successor_opens() {
    // the moved-aside journal: two reports inside the stream's hold, then a frame that does not read (where the
    // move found it broken)
    let moved = tempfile::tempdir().expect("temp");
    {
        let now = vike_model::now_ms();
        let mut journal = open_journal(moved.path());
        journal.append_execution_report(now, &fill_report("old1", "p1")).expect("append");
        journal.append_execution_report(now + 1, &fill_report("old1", "p2")).expect("append");
        journal.flush().expect("flush");
    }
    let segment = segment_of(moved.path());
    let end = end_of_frames(&segment);
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::OpenOptions::new().write(true).open(&segment).expect("open");
        file.seek(SeekFrom::Start(end as u64)).expect("seek");
        file.write_all(&[4, 0, 0, 0, 0xde, 0xad, 0xbe, 0xef, 1, 2, 3, 4])
            .expect("a frame that does not read");
    }
    let moved_id = JournalExecutionStream::mint_journal_id();
    JournalExecutionStream {
        journal_id: moved_id.clone(),
        acknowledged_sequence: None,
        acknowledged_ms: None,
        predecessors: Vec::new(),
    }
    .store(moved.path())
    .expect("its stream file");

    // its successor, which the boot that moved it aside gave the predecessor
    let fresh = tempfile::tempdir().expect("temp");
    let mut journal = open_journal(fresh.path());
    journal.append_execution_report(20, &fill_report("new1", "n1")).expect("append");
    journal.flush().expect("flush");
    let fresh_id = JournalExecutionStream::mint_journal_id();
    JournalExecutionStream {
        journal_id: fresh_id.clone(),
        acknowledged_sequence: None,
        acknowledged_ms: None,
        predecessors: vec![JournalExecutionStreamPredecessor {
            journal_id: moved_id.clone(),
            path: moved.path().to_path_buf(),
            acknowledged_sequence: None,
            done: false,
        }],
    }
    .store(fresh.path())
    .expect("its stream file");

    let datahub = FakeDatahub::start(None, FakeDatahubMode::Serve);
    let stop = Arc::new(AtomicBool::new(false));
    let publisher = start_execution_stream(
        fresh.path(),
        journal.durable_watch(),
        datahub.addr,
        None,
        Arc::clone(&stop),
    );
    datahub.wait_until("the successor's record lands", |d| d.stream(&fresh_id).records.len() == 1);

    let opens = datahub.opens();
    let order: Vec<&str> = opens.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        order.first().copied(),
        Some(moved_id.as_str()),
        "the predecessor opens first: {order:?}"
    );
    let (_, at_successor) =
        opens.iter().find(|(id, _)| *id == fresh_id).expect("the successor opened");
    assert_eq!(
        at_successor.get(&moved_id),
        Some(&Some(2)),
        "the predecessor was stored whole, its unverified rest included, before its successor opened"
    );
    let drained = datahub.stream(&moved_id).records;
    assert_eq!(drained.iter().map(|r| r.sequence).collect::<Vec<_>>(), [0, 1, 2]);
    match &drained[2].body {
        ExecutionRecordBodyWire::Unverified(rest) => {
            assert_eq!(rest.journal_id, moved_id);
            assert_eq!((rest.after_sequence, rest.through_sequence), (1, u64::MAX));
            assert_eq!((rest.from_ms, rest.to_ms), (None, None), "nothing past the halt reads");
        }
        other => panic!("the unverified rest: {other:?}"),
    }
    stop.store(true, Ordering::Relaxed);
    publisher.join();
    let file = JournalExecutionStream::load(fresh.path()).expect("reads").expect("written");
    assert!(file.predecessors.iter().all(|p| p.done), "the predecessor is marked done: {file:?}");
}
