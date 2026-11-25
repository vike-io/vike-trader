//! **The execution stream through the REAL server, a real settings store and the in-memory store double**: no
//! loss across a datahub restart, no duplicate on a resend, the sequence rules, the gaps, first contact and the one
//! stream per journal. A "restart" is a NEW lane over the same store and the same `vike.db`, on a new listener,
//! after a test's crash point stopped the old session where a killed process would have stopped: the cursor and
//! the rows are all the state there is.
//!
//! The helpers are `pub(crate)` for `execution_fills_two_doors.rs`, which streams one journal's fills.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use vike_data::exec_log::OWNERSHIP_OURS;
use vike_data::{ExecFillRow, HistoryStore, HistoryStoreMemory};
use vike_datahub::execution_stream::{
    ExecutionStreamFaultPoint, ExecutionStreamFaults, ExecutionStreamLane,
};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::execution_stream::{
    ExecutionPositionSnapshotAccountWire, ExecutionPositionSnapshotWire, ExecutionRecordBodyWire,
    ExecutionRecordWire, ExecutionReportFillWire, ExecutionReportOrderWire, ExecutionReportWire,
    ExecutionStreamDown, ExecutionStreamUp, ExecutionUnverifiedWire,
};
use vike_datahub_client::proto::{Request, Response};
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_model::AccountPosition;
use vike_model::events::PositionSide;
use vike_model::orders::ExecutionOwnership;
use vike_secrets::execution_streams::{
    GAP_PRUNED, GAP_UNVERIFIED, read_account_positions, read_execution_streams,
};

pub(crate) const JOURNAL: &str = "0123456789abcdef0123456789abcdef";
const OTHER_JOURNAL: &str = "fedcba9876543210fedcba9876543210";
pub(crate) const VENUE: &str = "binance";

/// A datahub's durable state: the store and the settings directory its cursors live in.
pub(crate) struct DatahubState {
    pub(crate) store: Arc<HistoryStoreMemory>,
    pub(crate) settings: tempfile::TempDir,
}

impl DatahubState {
    pub(crate) fn new() -> Self {
        let settings = tempfile::tempdir().expect("temp");
        vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8")))
            .expect("an empty store");
        DatahubState { store: Arc::new(HistoryStoreMemory::new()), settings }
    }

    /// A datahub serving this box's state, with `faults` as its crash points.
    pub(crate) fn serve(&self, faults: Option<ExecutionStreamFaults>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let lane = ExecutionStreamLane::new(self.settings.path().to_path_buf(), None);
        let lane = match faults {
            Some(f) => lane.with_fault_points(f),
            None => lane,
        };
        let store: Arc<dyn HistoryStore + Send + Sync> = self.store.clone();
        thread::spawn(move || {
            let lanes =
                ServerLanes { execution_stream: Some(Arc::new(lane)), ..ServerLanes::default() };
            let timings = ServerTimings::PRODUCTION;
            let _ = serve_with_lanes(listener, store, None, None, None, None, lanes, None, timings);
        });
        addr
    }

    pub(crate) fn fills(&self, symbol: &str) -> Vec<ExecFillRow> {
        self.store.scan_exec_fills(VENUE, symbol).expect("fills")
    }

    fn cursor(&self, journal_id: &str) -> (Option<u64>, Option<u64>) {
        vike_secrets::execution_streams::read_execution_stream(self.settings.path(), journal_id)
            .expect("read")
            .map(|r| (r.stored_through, r.writing_through))
            .unwrap_or_default()
    }
}

/// A crash point that fires once, at `point`.
fn crash_at(point: ExecutionStreamFaultPoint) -> ExecutionStreamFaults {
    let fired = Arc::new(AtomicBool::new(false));
    Arc::new(move |p| p == point && !fired.swap(true, Ordering::SeqCst))
}

/// Open `journal_id`'s stream on `addr`: the cursor the datahub answered (`None`: it holds nothing), and the socket.
pub(crate) fn open(addr: SocketAddr, journal_id: &str) -> (Option<u64>, TcpStream) {
    let client = DatahubClient::connect(addr).expect("handshake");
    let (opened, stream) = client
        .open_execution_stream(
            journal_id,
            "test tradehub",
            1,
            1_000,
            Duration::from_secs(10),
            Duration::from_secs(10),
        )
        .expect("opened");
    stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    (opened.stored_through, stream)
}

pub(crate) fn send(stream: &mut TcpStream, up: &ExecutionStreamUp) {
    write_frame(stream, up).expect("send");
}

pub(crate) fn down(stream: &mut TcpStream) -> ExecutionStreamDown {
    read_frame(stream).expect("a down frame")
}

/// A fill report of `trade_id` on `symbol`, ours, on account `ALT`.
pub(crate) fn fill_report(symbol: &str, trade_id: &str, at_ms: i64) -> ExecutionRecordBodyWire {
    ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire {
        caused_by_sequence: 0,
        at_ms,
        ownership: ExecutionOwnership::Ours,
        account: Some("ALT".into()),
        venue: VENUE.into(),
        symbol: symbol.into(),
        order_client_id: format!("c-{trade_id}"),
        venue_order_id: Some(format!("v-{trade_id}")),
        mount_id: Some("maker-a".into()),
        order: Some(ExecutionReportOrderWire {
            side: 1,
            qty: 1.0,
            order_type: "MARKET".into(),
            status: "FILLED".into(),
            price: None,
            trigger_price: None,
            filled_qty: 1.0,
            average_fill_price: 100.0,
            reason: None,
        }),
        fill: Some(ExecutionReportFillWire {
            trade_id: trade_id.into(),
            side: 1,
            qty: 1.0,
            price: 100.0,
            commission: 0.1,
            commission_asset: "USDT".into(),
            liquidity_side: "taker".into(),
            mark_price: None,
            position_side: None,
        }),
        fee_correction: None,
    }))
}

/// Records chained from `after` (`None`: the journal's start), one per `(sequence, body)`.
pub(crate) fn chain(
    after: impl Into<Option<u64>>,
    bodies: Vec<(u64, ExecutionRecordBodyWire)>,
) -> Vec<ExecutionRecordWire> {
    let mut previous = after.into();
    bodies
        .into_iter()
        .map(|(sequence, body)| {
            let r = ExecutionRecordWire {
                sequence,
                previous_sequence: previous,
                recorded_ms: 5_000 + sequence as i64,
                body,
            };
            previous = Some(sequence);
            r
        })
        .collect()
}

/// Fills `t<n>` at sequences `seqs`, alternating two symbols when `two_series`.
fn fills(seqs: &[u64], two_series: bool) -> Vec<(u64, ExecutionRecordBodyWire)> {
    seqs.iter()
        .map(|&s| {
            let symbol = if two_series && s % 2 == 0 { "ETHUSDT" } else { "BTCUSDT" };
            (s, fill_report(symbol, &format!("t{s}"), 1_000 + s as i64))
        })
        .collect()
}

/// One row per execution: every trade id once, in the two series together.
fn trade_ids(b: &DatahubState) -> Vec<String> {
    let mut ids: Vec<String> =
        ["BTCUSDT", "ETHUSDT"].iter().flat_map(|s| b.fills(s)).map(|r| r.trade_id).collect();
    ids.sort();
    ids
}

fn expected(seqs: &[u64]) -> Vec<String> {
    let mut ids: Vec<String> = seqs.iter().map(|s| format!("t{s}")).collect();
    ids.sort();
    ids
}

/// Stream `records` and a heartbeat on a datahub that crashes at `point`, restart it, and resend from the cursor
/// it answers (regrouped one record per frame): every case ends with one row per execution and the cursor at the
/// head.
fn crash_then_resend(point: ExecutionStreamFaultPoint, two_series: bool) {
    let b = DatahubState::new();
    let seqs = [3, 5, 6, 9];
    let (from, mut s) = open(b.serve(Some(crash_at(point))), JOURNAL);
    assert_eq!(from, None, "first contact");
    send(&mut s, &ExecutionStreamUp::Records(chain(None, fills(&seqs, two_series))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 9 });
    assert!(read_frame::<_, ExecutionStreamDown>(&mut s).is_err(), "the datahub died at {point:?}");
    drop(s);

    let (from, mut s) = open(b.serve(None), JOURNAL);
    assert_eq!(from, None, "nothing was acknowledged before the crash");
    for record in chain(None, fills(&seqs, two_series)) {
        send(&mut s, &ExecutionStreamUp::Records(vec![record]));
    }
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 9 }, "{point:?}");
    assert_eq!(trade_ids(&b), expected(&seqs), "{point:?}: one row per execution");
    assert_eq!(b.cursor(JOURNAL), (Some(9), Some(9)));

    // ...and a later window is written as usual.
    send(&mut s, &ExecutionStreamUp::Records(chain(9, fills(&[12], two_series))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 12 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 12 });
    assert_eq!(trade_ids(&b), expected(&[3, 5, 6, 9, 12]));
}

#[test]
fn a_restart_before_the_append_loses_nothing() {
    crash_then_resend(ExecutionStreamFaultPoint::AfterWritingThrough, false);
}

#[test]
fn a_restart_after_some_series_landed_replays_the_same_keys() {
    crash_then_resend(ExecutionStreamFaultPoint::AfterSeries { landed: 1 }, true);
}

#[test]
fn a_restart_after_the_append_before_the_cursor_adds_no_row() {
    crash_then_resend(ExecutionStreamFaultPoint::BeforeStoredThrough, true);
}

/// A resend of a window already stored (the tradehub lost the acknowledgement) is refused by the chain rule and
/// adds no row: the datahub asks for everything since its cursor, and the tradehub resumes from there.
#[test]
fn a_resend_of_a_stored_window_adds_no_row() {
    let b = DatahubState::new();
    let addr = b.serve(None);
    let (_, mut s) = open(addr, JOURNAL);
    send(&mut s, &ExecutionStreamUp::Records(chain(None, fills(&[1, 2, 3], false))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 3 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 3 });
    drop(s);

    let (from, mut s) = open(addr, JOURNAL);
    assert_eq!(from, Some(3), "the cursor is the datahub's");
    // the chain's start again, as a tradehub that lost the acknowledgement believes: refused once anything is stored
    send(&mut s, &ExecutionStreamUp::Records(chain(None, fills(&[1, 2, 3], false))));
    assert_eq!(down(&mut s), ExecutionStreamDown::Resend { after_sequence: Some(3) });
    send(&mut s, &ExecutionStreamUp::Records(chain(3, fills(&[4], false))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 4 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 4 });
    assert_eq!(trade_ids(&b), expected(&[1, 2, 3, 4]));
}

/// A reconnect that groups the same records into other frames and windows adds no row, and a window crashed half
/// way is rewritten exactly even when the records arrive one frame each.
#[test]
fn a_resend_after_a_reconnect_that_regroups_adds_no_row() {
    let b = DatahubState::new();
    let crash = crash_at(ExecutionStreamFaultPoint::AfterSeries { landed: 1 });
    let (_, mut s) = open(b.serve(Some(crash)), JOURNAL);
    send(&mut s, &ExecutionStreamUp::Records(chain(None, fills(&[1, 2, 3, 4], true))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 4 });
    assert!(read_frame::<_, ExecutionStreamDown>(&mut s).is_err(), "crashed");
    drop(s);

    let (_, mut s) = open(b.serve(None), JOURNAL);
    let records = chain(None, fills(&[1, 2, 3, 4, 5, 6], true));
    send(&mut s, &ExecutionStreamUp::Records(records[..1].to_vec()));
    send(&mut s, &ExecutionStreamUp::Records(records[1..5].to_vec()));
    assert_eq!(
        down(&mut s),
        ExecutionStreamDown::Stored { through_sequence: 4 },
        "the pinned window"
    );
    send(&mut s, &ExecutionStreamUp::Records(records[5..].to_vec()));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 6 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 6 });
    assert_eq!(trade_ids(&b), expected(&[1, 2, 3, 4, 5, 6]));
}

/// **`Stored` up to the highest contiguous sequence, `Resend` from the first gap**: a record that does not chain
/// stores the ones before it, drops the rest of its frame and every record in flight, and the resend fills in.
#[test]
fn a_record_that_does_not_chain_stores_the_contiguous_ones_and_asks_for_the_rest() {
    let b = DatahubState::new();
    let (_, mut s) = open(b.serve(None), JOURNAL);
    let mut frame = chain(None, fills(&[1, 2], false));
    frame.extend(chain(3, fills(&[4, 5], false)));
    send(&mut s, &ExecutionStreamUp::Records(frame));
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 2 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Resend { after_sequence: Some(2) });
    send(&mut s, &ExecutionStreamUp::Records(chain(5, fills(&[6], false))));
    send(&mut s, &ExecutionStreamUp::Records(chain(2, fills(&[3, 4, 5, 6], false))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 6 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 6 });
    assert_eq!(trade_ids(&b), expected(&[1, 2, 3, 4, 5, 6]));
}

/// **A fresh journal's record 0 is the chain's start**: chained from `None` while nothing is stored, it is stored once
/// and the cursor then says `Some(..)`, never "nothing"; the next open chains from it, and the start sent again is
/// refused with a `Resend` from the cursor, adding no row.
#[test]
fn a_fresh_journals_record_zero_is_stored_once_from_the_chain_start() {
    let b = DatahubState::new();
    let addr = b.serve(None);
    let (from, mut s) = open(addr, JOURNAL);
    assert_eq!(from, None, "nothing stored");
    send(&mut s, &ExecutionStreamUp::Records(chain(None, fills(&[0, 1], false))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 1 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 1 });
    drop(s);
    let (from, mut s) = open(addr, JOURNAL);
    assert_eq!(from, Some(1));
    send(&mut s, &ExecutionStreamUp::Records(chain(None, fills(&[0], false))));
    assert_eq!(down(&mut s), ExecutionStreamDown::Resend { after_sequence: Some(1) });
    send(&mut s, &ExecutionStreamUp::Records(chain(1, fills(&[2], false))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 2 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 2 });
    assert_eq!(trade_ids(&b), expected(&[0, 1, 2]), "record 0 once");
}

/// A `Gap` is recorded once, at ERROR, and the cursor moves past it: the next open asks for nothing it lost. A gap
/// follows something stored (the journal's record 0 here); from nothing stored the tradehub sends none, it starts
/// from its oldest record.
#[test]
fn a_gap_is_recorded_and_the_cursor_moves_past_it() {
    let b = DatahubState::new();
    let addr = b.serve(None);
    let (_, mut s) = open(addr, JOURNAL);
    send(&mut s, &ExecutionStreamUp::Records(chain(None, fills(&[0], false))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 0 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 0 });
    let gap = ExecutionStreamUp::Gap {
        after_sequence: 0,
        resumes_at_sequence: 10,
        lost_from_ms: None,
        lost_to_ms: Some(77),
    };
    send(&mut s, &gap);
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 9 });
    send(&mut s, &ExecutionStreamUp::Records(chain(0, fills(&[10], false))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 10 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 10 });
    drop(s);
    let (from, _s) = open(addr, JOURNAL);
    assert_eq!(from, Some(10));
    let streams = read_execution_streams(b.settings.path()).expect("read");
    let gaps = &streams[0].1;
    assert_eq!(gaps.len(), 1);
    assert_eq!((gaps[0].kind.as_str(), gaps[0].after_sequence), (GAP_PRUNED, 0));
    assert_eq!((gaps[0].resumes_at_sequence, gaps[0].lost_to_ms), (10, Some(77)));
}

/// An `Unverified` record is stored as a gap row of kind `unverified`, and moves the cursor like any record.
#[test]
fn an_unverified_record_becomes_a_gap_row_of_kind_unverified() {
    let b = DatahubState::new();
    let (_, mut s) = open(b.serve(None), JOURNAL);
    let unverified = ExecutionRecordBodyWire::Unverified(ExecutionUnverifiedWire {
        journal_id: JOURNAL.into(),
        after_sequence: 4,
        through_sequence: 8,
        from_ms: Some(1),
        to_ms: Some(2),
        order_client_ids: vec!["c-1".into()],
        reason: "a snapshot-only restore".into(),
    });
    send(&mut s, &ExecutionStreamUp::Records(chain(None, vec![(9, unverified)])));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 9 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 9 });
    let streams = read_execution_streams(b.settings.path()).expect("read");
    let gap = &streams[0].1[0];
    assert_eq!(
        (gap.kind.as_str(), gap.after_sequence, gap.resumes_at_sequence),
        (GAP_UNVERIFIED, 4, 9)
    );
    assert_eq!((gap.lost_from_ms, gap.lost_to_ms), (Some(1), Some(2)));
}

/// **An unverified record's reason and order ids are readable back** through `ExecutionStreams`, the operator's read.
#[test]
fn an_unverified_records_reason_and_order_ids_are_readable_back() {
    let b = DatahubState::new();
    let addr = b.serve(None);
    let (_, mut s) = open(addr, JOURNAL);
    let unverified = ExecutionRecordBodyWire::Unverified(ExecutionUnverifiedWire {
        journal_id: JOURNAL.into(),
        after_sequence: 4,
        through_sequence: 8,
        from_ms: Some(1),
        to_ms: Some(2),
        order_client_ids: vec!["c-1".into(), "c-2".into()],
        reason: "a watchdog tick in the cut window".into(),
    });
    send(&mut s, &ExecutionStreamUp::Records(chain(None, vec![(9, unverified)])));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 9 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 9 });

    let mut client = DatahubClient::connect(addr).expect("handshake");
    let streams = client.execution_streams().expect("streams");
    let gap = &streams[0].gaps[0];
    assert_eq!(gap.kind, GAP_UNVERIFIED);
    assert_eq!(gap.reason.as_deref(), Some("a watchdog tick in the cut window"));
    assert_eq!(gap.order_client_ids, vec!["c-1".to_string(), "c-2".to_string()]);
}

/// **First contact never lets an old row win**: a row written before the stream (the materializer's, no
/// ownership, no account) never blocks the stream's row of the same execution; both are stored, and the read
/// rule folds the stream's.
#[test]
fn first_contact_stores_the_stream_row_beside_a_materializer_row() {
    let b = DatahubState::new();
    let old = ExecFillRow { account: None, mount_id: None, ownership: None, ..stream_row("t1") };
    b.store
        .append_exec_fills(VENUE, "BTCUSDT", &[old], Some("mat-fill-binance-BTCUSDT-0-1"))
        .unwrap();
    let (_, mut s) = open(b.serve(None), JOURNAL);
    send(
        &mut s,
        &ExecutionStreamUp::Records(chain(None, vec![(1, fill_report("BTCUSDT", "t1", 1_001))])),
    );
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 1 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 1 });
    let rows = b.fills("BTCUSDT");
    assert_eq!(rows.len(), 2, "both are stored");
    let folded = ExecFillRow::fill_events(&rows);
    assert_eq!(folded.len(), 1, "the read folds one");
    assert_eq!(ExecFillRow::superseded(&rows), [true, false]);
}

/// ...and drops ONLY a duplicate STREAM row: a journal re-identified after a copy resends what the old id stored.
#[test]
fn first_contact_drops_only_a_duplicate_stream_row() {
    let b = DatahubState::new();
    let addr = b.serve(None);
    let (_, mut s) = open(addr, JOURNAL);
    send(&mut s, &ExecutionStreamUp::Records(chain(None, fills(&[1, 2], false))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 2 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 2 });
    drop(s);

    let (from, mut s) = open(addr, OTHER_JOURNAL);
    assert_eq!(from, None, "a new journal id is first contact");
    send(&mut s, &ExecutionStreamUp::Records(chain(None, fills(&[1, 2, 3], false))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 3 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 3 });
    assert_eq!(trade_ids(&b), expected(&[1, 2, 3]), "t1 and t2 once, t3 added");
    let orders = b.store.scan_exec_orders(VENUE, "BTCUSDT").unwrap();
    assert_eq!(orders.len(), 3, "the order rows the same way: {orders:?}");
}

/// **First contact survives a reconnect**: the boundary is the head the journal's FIRST open stated, kept in `vike.db`,
/// so a reconnect in the middle of a re-identified journal's catch-up still drops what the old id stored.
#[test]
fn a_reconnect_in_the_middle_of_a_catch_up_still_filters() {
    let b = DatahubState::new();
    let addr = b.serve(None);
    let (_, mut s) = open(addr, JOURNAL);
    send(&mut s, &ExecutionStreamUp::Records(chain(None, fills(&[1, 2, 3], false))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 3 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 3 });
    drop(s);

    let (_, mut s) = open(addr, OTHER_JOURNAL);
    send(&mut s, &ExecutionStreamUp::Records(chain(None, fills(&[1], false))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 1 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 1 });
    drop(s);

    let (from, mut s) = open(addr, OTHER_JOURNAL);
    assert_eq!(from, Some(1), "the reconnect resumes the catch-up");
    send(&mut s, &ExecutionStreamUp::Records(chain(1, fills(&[2, 3, 4], false))));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 4 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 4 });
    assert_eq!(trade_ids(&b), expected(&[1, 2, 3, 4]), "t2 and t3 once, after the reconnect too");
}

/// An order report of `order_client_id` at `status`, with no fill.
fn order_report(order_client_id: &str, status: &str, at_ms: i64) -> ExecutionRecordBodyWire {
    let ExecutionRecordBodyWire::Report(mut report) = fill_report("BTCUSDT", "x", at_ms) else {
        unreachable!("fill_report is a report")
    };
    report.fill = None;
    report.order_client_id = order_client_id.into();
    if let Some(order) = report.order.as_mut() {
        order.status = status.into();
    }
    ExecutionRecordBodyWire::Report(report)
}

/// **An order resting across a journal moved aside ends at its NEW journal's row**: its successor restarts the
/// sequences (3 after 900) and the fill's venue time is earlier than the accept vike recorded, yet the current
/// snapshot is the new journal's, because the successor's first contact came later (its generation).
#[test]
fn an_order_resting_across_a_journal_move_ends_at_its_new_journals_row() {
    let b = DatahubState::new();
    let addr = b.serve(None);
    let (_, mut s) = open(addr, JOURNAL);
    let accepted = order_report("c-rest", "ACCEPTED", 5_000);
    send(&mut s, &ExecutionStreamUp::Records(chain(None, vec![(900, accepted)])));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 900 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 900 });
    drop(s);

    let (_, mut s) = open(addr, OTHER_JOURNAL);
    let filled = order_report("c-rest", "FILLED", 4_000);
    send(&mut s, &ExecutionStreamUp::Records(chain(None, vec![(3, filled)])));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 3 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 3 });

    let rows = b.store.scan_exec_orders(VENUE, "BTCUSDT").expect("orders");
    assert_eq!(rows.len(), 2, "both rows are stored");
    let current = vike_data::execution_index::latest_order_snapshots(rows);
    let seen: Vec<(&str, Option<i64>, Option<i64>)> = current
        .iter()
        .map(|r| (r.status.as_str(), r.journal_generation, r.journal_sequence))
        .collect();
    assert_eq!(seen, vec![("FILLED", Some(2), Some(3))]);
}

/// A second open of one journal WINS: the older socket is shut, and the newer stream stores.
#[test]
fn a_second_open_of_a_journal_wins() {
    let b = DatahubState::new();
    let addr = b.serve(None);
    let (_, mut first) = open(addr, JOURNAL);
    let (_, mut second) = open(addr, JOURNAL);
    assert!(
        read_frame::<_, ExecutionStreamDown>(&mut first).is_err(),
        "the older stream is closed"
    );
    send(&mut second, &ExecutionStreamUp::Records(chain(None, fills(&[1], false))));
    send(&mut second, &ExecutionStreamUp::Heartbeat { head_sequence: 1 });
    assert_eq!(down(&mut second), ExecutionStreamDown::Stored { through_sequence: 1 });

    let mut client = DatahubClient::connect(addr).expect("handshake");
    let streams = client.execution_streams().expect("streams");
    assert_eq!(streams.len(), 1);
    assert_eq!((streams[0].journal_id.as_str(), streams[0].stored_through), (JOURNAL, Some(1)));
    assert!(streams[0].connected, "the second stream is live");
}

/// A malformed journal id is refused positionally, and the connection keeps serving.
#[test]
fn a_malformed_journal_id_is_refused_and_the_connection_survives() {
    let b = DatahubState::new();
    let addr = b.serve(None);
    let mut raw = TcpStream::connect(addr).expect("connect");
    let open = Request::ExecutionStreamOpen {
        journal_id: "NOT-HEX".into(),
        tradehub: "t".into(),
        oldest_sequence: 0,
        head_sequence: 0,
    };
    write_frame(&mut raw, &open).expect("send");
    let refused: Response = read_frame(&mut raw).expect("answer");
    assert!(matches!(&refused, Response::Error(why) if why.contains("journal id")), "{refused:?}");
    write_frame(&mut raw, &Request::Ping).expect("ping");
    assert!(matches!(read_frame::<_, Response>(&mut raw).expect("pong"), Response::Pong));
}

/// The row the stream writes for `fill_report("BTCUSDT", trade_id, 1_001)`: what a pre-stream twin copies.
fn stream_row(trade_id: &str) -> ExecFillRow {
    ExecFillRow {
        ts: 1_001,
        trade_id: trade_id.into(),
        order_client_id: format!("c-{trade_id}"),
        venue: VENUE.into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        px: 100.0,
        commission: 0.1,
        mark_price: None,
        liquidity_side: "taker".into(),
        commission_asset: "USDT".into(),
        venue_order_id: Some(format!("v-{trade_id}")),
        account: Some("ALT".into()),
        position_side: None,
        corrects_trade_id: None,
        correction_seq: None,
        mount_id: Some("maker-a".into()),
        received_ms: None,
        ownership: Some(OWNERSHIP_OURS.into()),
        journal_sequence: None,
        journal_generation: None,
    }
}

/// A boot's position snapshot of two accounts: `MAIN` long 2 BTCUSDT at 100, `ALT` unknown.
fn position_snapshot() -> ExecutionRecordBodyWire {
    ExecutionRecordBodyWire::PositionSnapshot(ExecutionPositionSnapshotWire {
        accounts: vec![
            ExecutionPositionSnapshotAccountWire {
                venue: VENUE.into(),
                account: "MAIN".into(),
                positions: vec![AccountPosition {
                    symbol: "BTCUSDT".into(),
                    position_side: PositionSide::Both,
                    size: 2.0,
                    average_price: 100.0,
                }],
                unknown: None,
            },
            ExecutionPositionSnapshotAccountWire {
                venue: VENUE.into(),
                account: "ALT".into(),
                positions: Vec::new(),
                unknown: Some("the journal holds no earlier book".into()),
            },
        ],
    })
}

/// **A position snapshot is stored ONCE, whatever crashes and resends**: a datahub that dies after writing it and
/// before its cursor moves is sent the same window again, and `vike.db` still holds one row per account, under the
/// journal's generation and the record's sequence and time; the fills after it carry their own sequences, which is
/// what puts them after it.
#[test]
fn a_position_snapshot_is_stored_once_across_a_crash_and_a_resend() {
    let b = DatahubState::new();
    let records =
        || chain(None, vec![(2, position_snapshot()), (3, fill_report("BTCUSDT", "t3", 1_003))]);
    let (_, mut s) =
        open(b.serve(Some(crash_at(ExecutionStreamFaultPoint::BeforeStoredThrough))), JOURNAL);
    send(&mut s, &ExecutionStreamUp::Records(records()));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 3 });
    assert!(read_frame::<_, ExecutionStreamDown>(&mut s).is_err(), "the datahub died");
    drop(s);

    let addr = b.serve(None);
    let (from, mut s) = open(addr, JOURNAL);
    assert_eq!(from, None, "nothing was acknowledged before the crash");
    send(&mut s, &ExecutionStreamUp::Records(records()));
    send(&mut s, &ExecutionStreamUp::Heartbeat { head_sequence: 3 });
    assert_eq!(down(&mut s), ExecutionStreamDown::Stored { through_sequence: 3 });
    let stored = read_account_positions(b.settings.path(), i64::MAX).expect("read");
    // ...and a reader asks them over the wire, the latest at or before its window's start.
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let served = client.execution_account_positions(i64::MAX).expect("served");
    let accounts: Vec<_> = served.iter().map(|r| (r.account.as_str(), r.sequence)).collect();
    assert_eq!(accounts, [("ALT", 2), ("MAIN", 2)]);
    assert!(
        client.execution_account_positions(5_001).expect("served").is_empty(),
        "none that early"
    );
    let summary: Vec<_> = stored
        .iter()
        .map(|r| {
            (
                r.account.as_str(),
                r.generation,
                r.sequence,
                r.as_of_ms,
                r.positions.len(),
                r.unknown.is_some(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [("ALT", 1, 2, 5_002, 0, true), ("MAIN", 1, 2, 5_002, 1, false)],
        "{stored:#?}"
    );
    assert_eq!((stored[1].positions[0].size, stored[1].positions[0].average_price), (2.0, 100.0));
    let fill = &b.fills("BTCUSDT")[0];
    assert_eq!((fill.journal_generation, fill.journal_sequence), (Some(1), Some(3)));
    assert!(fill.after_position_snapshot((stored[1].generation, stored[1].sequence as i64)));
}
