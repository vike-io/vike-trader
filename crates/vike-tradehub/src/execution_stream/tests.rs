//! What the publisher says of one journal record ([`super::read::body_of`]): a removed mount named from its durable
//! strategy-code row, and a fill that names no order sent FOREIGN with only its ring's account. And how it reads a
//! journal moved aside: never before the datahub answers a dial, never whole, and no more than the hold.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_datahub_client::execution_stream::ExecutionRecordBodyWire;
use vike_execution::{GatewayEvent, Ingest};
use vike_journal::{
    EXECUTION_STREAM_HOLD_MS, Journal, JournalExecutionStream, JournalExecutionStreamPredecessor,
    JournalFileConfig, JournalRecord, record_sequence,
};
use vike_model::EventRingCursor;
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::orders::client_id::OrderClientIdStrategyCode;
use vike_model::orders::{ExecutionOwnership, ExecutionReport, ExecutionReportFill};

use super::link::Pause;
use super::read::{JournalRecords, MovedAside, READ_LIMIT, StrategyCodeMounts, body_of};
use super::{ExecutionStreamSource, ExecutionStreamTimings, Publisher};

/// **A failed connection is dialled again after these waits, verbatim**: one second doubling to a minute, then a minute
/// for every failure after (production's timings over the one ladder).
#[test]
fn a_failed_connection_redials_after_one_second_doubling_to_a_minute() {
    let mut redial = ExecutionStreamTimings::default().redial();
    let secs: Vec<u64> = (0..8).map(|_| redial.failed().as_secs()).collect();
    assert_eq!(secs, [1, 2, 4, 8, 16, 32, 60, 60]);
}

fn fill(order_client_id: &str) -> FillEvent {
    FillEvent {
        trade_id: TradeId::new("t1").expect("non-empty"),
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 0.5,
        last_price: 100.0,
        commission: 0.01,
        commission_asset: "USDT".into(),
        liquidity_side: "taker".into(),
        timestamp: 77,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

/// A fill report of ours on `binance#A`, carrying strategy code `code` and `mount_id`.
fn report(ownership: ExecutionOwnership, code: &str, mount_id: Option<&str>) -> JournalRecord {
    JournalRecord::ExecutionReport {
        sequence: 9,
        now_ms: 100,
        report: ExecutionReport {
            caused_by_sequence: 8,
            at_ms: 77,
            ownership,
            account: Some("A".into()),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            order_client_id: "SW0001...".into(),
            venue_order_id: Some("V1".into()),
            mount_id: mount_id.map(str::to_string),
            strategy_code: OrderClientIdStrategyCode::parse(code),
            order: None,
            fill: Some(ExecutionReportFill::of_fill(&fill("SW0001..."))),
            fee_correction: None,
            reason: None,
        },
    }
}

fn mount_and_ownership(
    body: Option<ExecutionRecordBodyWire>,
) -> (Option<String>, ExecutionOwnership) {
    match body {
        Some(ExecutionRecordBodyWire::Report(r)) => (r.mount_id.clone(), r.ownership),
        other => panic!("a report: {other:?}"),
    }
}

/// **C.1a: a late event on a mount removed before it keeps its strategy.** The core found no mount carrying the code,
/// so the report names the code alone; the publisher names the mount from the durable `strategy_code` row before it
/// sends. A report that names its mount is sent as it is, and a FOREIGN one never takes a mount, whatever its code.
#[test]
fn a_removed_mounts_code_is_named_from_its_strategy_code_row() {
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(settings.path()))
        .expect("a store");
    let code =
        vike_secrets::strategy_code::strategy_code_for(settings.path(), "mount-m", "momentum")
            .expect("a code");
    let code = OrderClientIdStrategyCode::new(code).expect("in range").to_string();
    let mut mounts = StrategyCodeMounts::new(Some(settings.path().to_path_buf()));

    let removed =
        body_of(&report(ExecutionOwnership::Ours, &code, None), &mut mounts).expect("named");
    assert_eq!(mount_and_ownership(removed), (Some("mount-m".into()), ExecutionOwnership::Ours));

    let named = body_of(&report(ExecutionOwnership::Ours, &code, Some("live-mount")), &mut mounts)
        .expect("as it is");
    assert_eq!(mount_and_ownership(named).0.as_deref(), Some("live-mount"));

    let foreign =
        body_of(&report(ExecutionOwnership::Foreign, &code, None), &mut mounts).expect("as it is");
    assert_eq!(mount_and_ownership(foreign), (None, ExecutionOwnership::Foreign));

    // a code no row holds names nothing, and a box with no store names nothing either
    let unknown =
        body_of(&report(ExecutionOwnership::Ours, "00ZZ", None), &mut mounts).expect("read");
    assert_eq!(mount_and_ownership(unknown).0, None);
    let mut none = StrategyCodeMounts::new(None);
    let bare =
        body_of(&report(ExecutionOwnership::Ours, &code, None), &mut none).expect("no store");
    assert_eq!(mount_and_ownership(bare).0, None);
}

/// **A fill that names no order** (the OMS journals no report for it) is sent from its own `Command`, FOREIGN, with the
/// account whose ring carried it, its fill, and no order or mount. One that names an order is the OMS's to report.
#[test]
fn a_fill_naming_no_order_is_sent_foreign_with_its_rings_account() {
    let ringed = |order_client_id: &str| JournalRecord::Command {
        sequence: 4,
        now_ms: 90,
        message: Ingest::GatewayEvent(Box::new(GatewayEvent {
            event: Event::Fill(fill(order_client_id)),
            route_key: "binance#A".into(),
            cursor: EventRingCursor { gateway_epoch: 1, sequence: 1, timestamp_ms: 1 },
        })),
    };
    let mut mounts = StrategyCodeMounts::new(None);
    match body_of(&ringed(""), &mut mounts).expect("no store read") {
        Some(ExecutionRecordBodyWire::Report(r)) => {
            assert_eq!(r.ownership, ExecutionOwnership::Foreign);
            assert_eq!(r.account.as_deref(), Some("A"));
            assert_eq!((r.caused_by_sequence, r.at_ms), (4, 77));
            assert_eq!((&r.mount_id, &r.order, &r.venue_order_id), (&None, &None, &None));
            assert_eq!(r.fill.as_ref().expect("the fill").trade_id, "t1");
        }
        other => panic!("a foreign report: {other:?}"),
    }
    assert_eq!(body_of(&ringed("op1"), &mut mounts).expect("read"), None, "the OMS reports it");
    let unringed = JournalRecord::Command {
        sequence: 5,
        now_ms: 91,
        message: Ingest::Event(Event::Fill(fill(""))),
    };
    match body_of(&unringed, &mut mounts).expect("read") {
        Some(ExecutionRecordBodyWire::Report(r)) => {
            assert_eq!(r.account, None, "no ring, no account")
        }
        other => panic!("a foreign report: {other:?}"),
    }
}

/// **A boot's position snapshot is sent whole**: every account, its positions and its unknown words, field for
/// field (the venue a position names is the account's, so it does not ride).
#[test]
fn a_position_snapshot_is_sent_whole() {
    use vike_journal::JournalPositionSnapshotAccount;
    let record = JournalRecord::PositionSnapshot {
        sequence: 9,
        now_ms: 100,
        accounts: vec![
            JournalPositionSnapshotAccount {
                venue: "binance".into(),
                account: "A".into(),
                positions: vec![vike_model::AccountVenuePosition {
                    venue: "binance".into(),
                    symbol: "BTCUSDT".into(),
                    position_side: PositionSide::Long,
                    size: 0.5,
                    average_price: 64_000.0,
                }],
                unknown: None,
            },
            JournalPositionSnapshotAccount {
                venue: "binance".into(),
                account: "B".into(),
                positions: Vec::new(),
                unknown: Some("no engine runs the account (its keys are absent)".into()),
            },
        ],
    };
    let mut mounts = StrategyCodeMounts::new(None);
    let Some(ExecutionRecordBodyWire::PositionSnapshot(sent)) =
        body_of(&record, &mut mounts).expect("no store read")
    else {
        panic!("a position snapshot");
    };
    let rows: Vec<_> = sent
        .accounts
        .iter()
        .map(|a| (a.venue.as_str(), a.account.as_str(), a.positions.len(), a.unknown.as_deref()))
        .collect();
    assert_eq!(
        rows,
        [
            ("binance", "A", 1, None),
            ("binance", "B", 0, Some("no engine runs the account (its keys are absent)"))
        ]
    );
    let p = &sent.accounts[0].positions[0];
    assert_eq!(
        (p.symbol.as_str(), p.position_side, p.size, p.average_price),
        ("BTCUSDT", PositionSide::Long, 0.5, 64_000.0)
    );
}

fn small_segments() -> JournalFileConfig {
    JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 64 }
}

/// **A datahub that is down costs no read of a moved-aside journal**: every connection dials before it reads. The
/// predecessor here is a FILE where its directory should be, so a pass that read it before dialling would fail on
/// it; each of these passes fails on the dial instead, and nothing walked it.
#[test]
fn a_datahub_that_is_down_costs_no_read_of_a_moved_aside_journal() {
    let live = tempfile::tempdir().expect("temp");
    let journal = Journal::open(live.path(), small_segments()).expect("open");
    let aside = tempfile::tempdir().expect("temp");
    let path = aside.path().join("journal.broken-1");
    std::fs::write(&path, b"no journal directory").expect("write");
    JournalExecutionStream {
        journal_id: JournalExecutionStream::mint_journal_id(),
        acknowledged_sequence: None,
        acknowledged_ms: None,
        predecessors: vec![JournalExecutionStreamPredecessor {
            journal_id: JournalExecutionStream::mint_journal_id(),
            path,
            acknowledged_sequence: None,
            done: false,
        }],
    }
    .store(live.path())
    .expect("the stream file");
    // a port bound and closed at once: nothing listens there
    let addr =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("addr");
    let source = ExecutionStreamSource {
        journal_dir: live.path().to_path_buf(),
        watch: journal.durable_watch(),
        datahub_addr: addr.to_string(),
        tradehub: "test".into(),
        keys: Arc::new(|| None),
        settings_dir: None,
        timings: ExecutionStreamTimings::default(),
    };
    let stop = AtomicBool::new(false);
    let mut publisher = Publisher::new(&source, &stop);
    for attempt in 0..3 {
        let Err(Pause::Failed { why, sent: false }) = publisher.pass() else {
            panic!("attempt {attempt}: a datahub that is down fails the pass, nothing sent")
        };
        assert!(why.starts_with(&format!("datahub {addr}")), "attempt {attempt}: {why}");
        assert!(publisher.moved.is_none(), "attempt {attempt}: nothing walked the moved journal");
    }
}

/// **A moved-aside journal is never held whole, and owes no more than the hold** (plan C.1b's drain): it is read
/// [`READ_LIMIT`] records a page, and the leading run older than `EXECUTION_STREAM_HOLD_MS` is ONE unverified window
/// at the run's last sequence, its span the records it stands for; a cursor inside the run opens the window past it,
/// and a cursor past it sends none.
#[test]
fn a_moved_aside_journal_is_read_a_page_at_a_time_and_owes_no_more_than_the_hold() {
    let JournalRecord::ExecutionReport { report: fill, .. } =
        report(ExecutionOwnership::Ours, "0001", None)
    else {
        unreachable!("a report")
    };
    let aside = tempfile::tempdir().expect("temp");
    let (old, young) = (1_000, 1_000 + EXECUTION_STREAM_HOLD_MS + 10_000);
    {
        let mut journal = Journal::open(aside.path(), small_segments()).expect("open");
        for i in 0..1_100 {
            let at = if i < 3 { old + i } else { young };
            journal.append_execution_report(at, &fill).expect("append");
        }
        journal.flush().expect("flush");
    }
    let id = JournalExecutionStream::mint_journal_id();
    let moved = MovedAside::scan(&id, aside.path(), young + 1).expect("one walk");
    let source = JournalRecords::MovedAside(&moved);
    assert_eq!(source.oldest().expect("held"), Some((0, old)));
    assert_eq!(source.end(), Some(Some(1_099)), "a clean end: no unverified rest");

    let (mut after, mut pages, mut sent) = (None, Vec::new(), Vec::new());
    loop {
        let page = source.read(after, READ_LIMIT).expect("reads");
        let Some(last) = page.last() else { break };
        after = Some(record_sequence(last));
        pages.push(page.len());
        sent.extend(page.iter().map(record_sequence));
    }
    assert_eq!(pages, [READ_LIMIT, READ_LIMIT, 74], "a page at a time");
    assert_eq!(sent, [2].into_iter().chain(3..1_100).collect::<Vec<u64>>());

    let window = |after: Option<u64>| {
        let page = source.read(after, 1).expect("reads");
        match page.first() {
            Some(JournalRecord::ExecutionReportsUnverified {
                sequence,
                journal_id,
                after_sequence,
                through_sequence,
                from_ms,
                to_ms,
                reason,
                ..
            }) => {
                assert_eq!(journal_id, &id);
                assert!(reason.contains("hold"), "{reason}");
                Some((*sequence, *after_sequence, *through_sequence, *from_ms, *to_ms))
            }
            _ => None,
        }
    };
    assert_eq!(window(None), Some((2, 0, 2, Some(old), Some(old + 2))));
    assert_eq!(window(Some(0)), Some((2, 0, 2, Some(old + 1), Some(old + 2))));
    assert_eq!(window(Some(2)), None, "the datahub holds the run");
}

/// Every body a moved-aside journal at `path` streams, in order, through the publisher's own read and `body_of`.
fn streamed(path: &std::path::Path) -> Vec<ExecutionRecordBodyWire> {
    let moved = MovedAside::scan(&JournalExecutionStream::mint_journal_id(), path, 10_000)
        .expect("one walk");
    let source = JournalRecords::MovedAside(&moved);
    let mut mounts = StrategyCodeMounts::new(None);
    let (mut after, mut bodies) = (None, Vec::new());
    loop {
        let page = source.read(after, READ_LIMIT).expect("reads");
        let Some(last) = page.last() else { break };
        after = Some(record_sequence(last));
        for record in &page {
            bodies.extend(body_of(record, &mut mounts).expect("no store read"));
        }
    }
    bodies
}

/// The records of the two journals below: three fill reports, between which a maker's strategy re-priced an order
/// the engine dropped every re-price of (`with_inert`, the journal before decision 0127's amendment of 2026-11-19).
fn maker_records(with_inert: bool) -> Vec<JournalRecord> {
    let mut out = Vec::new();
    for i in 0..3u64 {
        if with_inert {
            for k in 0..4 {
                out.push(JournalRecord::StrategySubmit {
                    sequence: 0,
                    now_ms: 100,
                    mount_id: "m".into(),
                    intent: vike_execution::OrderIntent::Modify {
                        order_client_id: format!("b{i}"),
                        new_qty: None,
                        new_price: Some(100.0 + f64::from(k)),
                        new_trigger_price: None,
                    },
                    route_key: None,
                });
            }
        }
        out.push(report(ExecutionOwnership::Ours, "0001", Some("m")));
    }
    // each record its own sequence, as a journal numbers them
    for (sequence, record) in out.iter_mut().enumerate() {
        match record {
            JournalRecord::StrategySubmit { sequence: s, .. }
            | JournalRecord::ExecutionReport { sequence: s, .. } => *s = sequence as u64,
            other => unreachable!("{other:?}"),
        }
    }
    out
}

/// **The datahub receives the same executions from the lean journal, the binary one and a moved-aside JSON one**
/// (the research note's 1d and 1e, the owner's "do all" of 2026-11-19). A journal that leaves the inert re-prices out
/// streams the bodies the journal that kept them streams; and a format 33 journal (JSON frames, what the binary step
/// moves aside) is read and streamed by this build's reader, body for body as the binary one.
#[test]
fn the_lean_binary_and_moved_aside_json_journals_stream_the_same_executions() {
    let write = |records: &[JournalRecord]| {
        let dir = tempfile::tempdir().expect("temp");
        let mut journal = Journal::open(dir.path(), small_segments()).expect("open");
        for record in records {
            match record {
                JournalRecord::StrategySubmit { now_ms, mount_id, intent, .. } => {
                    journal.append_strategy_submit(*now_ms, mount_id, intent, None).expect("append")
                }
                JournalRecord::ExecutionReport { now_ms, report, .. } => {
                    journal.append_execution_report(*now_ms, report).expect("append")
                }
                other => unreachable!("{other:?}"),
            };
        }
        journal.flush().expect("flush");
        dir
    };
    let (full, lean) = (write(&maker_records(true)), write(&maker_records(false)));
    let bodies = streamed(full.path());
    assert_eq!(bodies.len(), 3, "the three reports, and nothing of the re-prices");
    assert_eq!(streamed(lean.path()), bodies, "the lean journal streams the same executions");

    // The format a build before the binary step wrote: a v33 header, each record a JSON frame.
    let json = tempfile::tempdir().expect("temp");
    let records = maker_records(true);
    let mut segment = Vec::new();
    segment.extend_from_slice(&0x314C_4A56u32.to_le_bytes()); // "VJL1"
    segment.extend_from_slice(&33u32.to_le_bytes());
    segment.extend_from_slice(&0u64.to_le_bytes());
    for record in &records {
        let payload = serde_json::to_vec(record).expect("a record serializes");
        segment.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        segment.extend_from_slice(&vike_journal::fnv1a32(&payload).to_le_bytes());
        segment.extend_from_slice(&payload);
    }
    segment.resize(64 * 1024, 0);
    std::fs::write(json.path().join("journal-00000000.vjl"), segment).expect("a v33 segment");
    assert_eq!(
        streamed(json.path()),
        bodies,
        "a moved-aside JSON journal streams the same executions"
    );
    assert!(
        Journal::read_all_located(json.path()).is_err(),
        "and is never restored from: the boot refuses it"
    );
}
