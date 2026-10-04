//! A `Backfill` whose client goes away STOPS, and so does one an OPERATOR cancels — over real
//! loopback sockets, through the real connection loop.
//! `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §6: T5 and T6 (the
//! dropped client), T7 (an operator's cancel from a second connection) and T8 (who may list and who
//! may cancel). T4, the probe on a bare socket, is in `crates/vike-datahub/src/server.rs`'s test
//! module, where the probe is reachable; the registry's own white-box tests are in
//! `crates/vike-datahub/src/backfill.rs`'s; T9, the capability negotiation, is
//! `crates/vike-datahub-client/tests/backfill_cancel_negotiation.rs`.
//!
//! The collector is FAKE and chunked in miniature: [`CHUNKS`] chunks, its stop probe asked at the top
//! of each — the shape of `vike_backfill`'s two chunked ingests, minus the store — and every boundary
//! WAITS for the test's go-ahead. So the test, not the scheduler, decides what the client has done by
//! the time the probe is asked, and what the server answers is the only thing left to observe. No
//! feature is needed: the table and the server entry are both feature-free.
//!
//! ⚠ **Which lane the fake is mounted on decides whether a cancel can stop it**, because the
//! registry asks the LANE (`crate::backfill`'s `stops_at_a_chunk_boundary`), never the collector.
//! `BackfillTable::new` mounts a `Klines` row — a one-batch lane — so the operator-cancel tests mount
//! the fake as `TickBars`, a chunked one, and the one-batch test keeps `new` on purpose.

use std::io::Read;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::backfill::{BackfillFn, BackfillLane, BackfillTable};
use vike_datahub::server::MAX_CONNECTIONS;
use vike_datahub::{serve_authed, serve_with_backfill};
use vike_datahub_client::proto::{FEATURE_BACKFILL_CANCEL, RunningBackfill};
use vike_datahub_client::{
    DatahubClient,
    proto::{Request, Response, write_frame},
    read_frame,
};
use vike_node_proto::auth::{NodeKeys, Scope};

const VENUE: &str = "fakevenue";
/// Chunks in one request of the fake lane.
const CHUNKS: usize = 5;
/// The longest any one step of a test may take before it is a failure rather than a slow box.
const WAIT: Duration = Duration::from_secs(10);
/// Long enough for a byte or a FIN written on loopback to be in the peer's kernel buffer — it is
/// delivered within the sending call; this is margin, not a measurement.
const SETTLE: Duration = Duration::from_millis(200);

/// What the fake lane did at one boundary.
#[derive(Debug, PartialEq)]
enum Lane {
    /// The probe said go on, and chunk `n` was "stored".
    Stored(usize),
    /// The probe said stop at the top of chunk `n`: chunks `0..n` are stored, none after.
    Stopped(usize),
    /// Every chunk was stored.
    Finished,
}

/// The test's hands on one fake lane.
struct Driver {
    go: Sender<()>,
    heard: Receiver<Lane>,
}

impl Driver {
    /// Let the lane reach its next boundary, ask its probe there and act on the answer — and say
    /// what it did.
    fn step(&self) -> Lane {
        self.go.send(()).expect("the lane is waiting at a boundary");
        self.heard.recv_timeout(WAIT).expect("the lane answers within the wait")
    }
}

/// A chunked collector in miniature, and the driver that walks it boundary by boundary.
fn chunked_lane() -> (BackfillFn, Driver) {
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (heard_tx, heard_rx) = mpsc::channel::<Lane>();
    let go_rx = Mutex::new(go_rx);
    let lane: BackfillFn =
        Box::new(move |_: &str, _: &str, _: i64, _: i64, should_stop: &dyn Fn() -> bool| {
            let go = go_rx.lock().expect("one request at a time");
            for chunk in 0..CHUNKS {
                if go.recv_timeout(WAIT).is_err() {
                    return Err("the test stopped driving the lane".to_string());
                }
                if should_stop() {
                    let _ = heard_tx.send(Lane::Stopped(chunk));
                    return Err(format!(
                        "stopped: before chunk {} of {CHUNKS}; {chunk} chunk(s) stored",
                        chunk + 1
                    ));
                }
                let _ = heard_tx.send(Lane::Stored(chunk));
            }
            let _ = heard_tx.send(Lane::Finished);
            Ok(CHUNKS)
        });
    (lane, Driver { go: go_tx, heard: heard_rx })
}

/// Serve `lane` as [`VENUE`]'s collector over an empty in-memory store, on an ephemeral loopback
/// port, key-less.
fn serve_the_lane(lane: BackfillFn) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let table = BackfillTable::new(vec![(VENUE.to_string(), lane)]);
    thread::spawn(move || {
        let _ = serve_with_backfill(listener, store, Some(table));
    });
    addr
}

fn backfill_request() -> Request {
    Request::Backfill {
        venue: VENUE.to_string(),
        symbol: "EURUSD".to_string(),
        interval: "1h".to_string(),
        start: 0,
        end: 10 * 3_600_000,
    }
}

/// A connection the server has SERVED — a `Ping` answered — so it is counted against
/// [`MAX_CONNECTIONS`] for as long as it stays open; `None` when the server dropped it unanswered.
fn served_connection(addr: SocketAddr) -> Option<TcpStream> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(WAIT)).ok()?;
    write_frame(&mut stream, &Request::Ping).ok()?;
    match read_frame::<_, Response>(&mut stream) {
        Ok(Response::Pong) => Some(stream),
        _ => None,
    }
}

/// **T5.** A client that goes away while its backfill runs stops it at the NEXT boundary: the chunk
/// already done stays done, no chunk after the boundary runs, NOTHING is written back — the server
/// closes without a reply — and the connection's slot is released.
///
/// The client goes away by shutting its WRITE side, which sends the same FIN a dropped socket does
/// and keeps the read side open, so "no reply" is something the test can watch rather than infer.
/// That the half-close cancels is itself the behaviour the probe's doc declares.
///
/// The slot is observed through the cap rather than taken on trust: every other slot is filled with
/// a served, idle connection, so a new connection is REFUSED while the backfill runs and served
/// again once its thread has gone.
#[test]
fn a_dropped_client_stops_its_backfill_at_the_next_boundary_unanswered_and_frees_its_slot() {
    let (lane, driver) = chunked_lane();
    let addr = serve_the_lane(lane);
    let mut client = TcpStream::connect(addr).expect("connect");
    write_frame(&mut client, &backfill_request()).expect("send the Backfill");
    assert_eq!(driver.step(), Lane::Stored(0), "a present client: the first boundary goes on");

    let idle: Vec<TcpStream> = (1..MAX_CONNECTIONS)
        .map(|n| {
            served_connection(addr).unwrap_or_else(|| panic!("idle connection {n} was refused"))
        })
        .collect();
    assert!(
        served_connection(addr).is_none(),
        "guard: with the backfill running and every other slot idle, the server is FULL"
    );

    client.shutdown(Shutdown::Write).expect("the client goes away");
    thread::sleep(SETTLE);
    assert_eq!(
        driver.step(),
        Lane::Stopped(1),
        "the backfill must stop at the first boundary after its client went away"
    );

    client.set_read_timeout(Some(WAIT)).expect("timeout");
    let mut reply = Vec::new();
    client.read_to_end(&mut reply).expect("the server closes the connection");
    assert!(
        reply.is_empty(),
        "a stopped backfill writes NOTHING back — {} byte(s) arrived",
        reply.len()
    );

    let served_again = (0..50).any(|_| {
        if served_connection(addr).is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
        false
    });
    assert!(served_again, "the stopped backfill's connection slot was never released");
    drop(idle);
}

/// **T6.** A PIPELINED frame is not a disconnect. The client sends a `Ping` behind its `Backfill`
/// before any reply has come back, so at every boundary the probe finds unread bytes on the socket —
/// and must read them as a live client. The backfill runs to its end, its `BackfillDone` comes back,
/// and THEN the pipelined `Ping` is answered: the peek consumed none of it.
#[test]
fn a_pipelined_frame_is_not_a_disconnect() {
    let (lane, driver) = chunked_lane();
    let addr = serve_the_lane(lane);
    let mut client = TcpStream::connect(addr).expect("connect");
    write_frame(&mut client, &backfill_request()).expect("send the Backfill");
    write_frame(&mut client, &Request::Ping).expect("pipeline a Ping behind it");
    thread::sleep(SETTLE);

    for chunk in 0..CHUNKS {
        assert_eq!(
            driver.step(),
            Lane::Stored(chunk),
            "a client with a frame in flight is present, at every boundary"
        );
    }
    assert_eq!(driver.heard.recv_timeout(WAIT).expect("the lane ends"), Lane::Finished);

    client.set_read_timeout(Some(WAIT)).expect("timeout");
    match read_frame::<_, Response>(&mut client).expect("the backfill is answered") {
        Response::BackfillDone(done) => {
            assert_eq!(done.rows_written, CHUNKS as u64, "every chunk ran")
        }
        other => panic!("the backfill must be answered with BackfillDone: {other:?}"),
    }
    assert!(
        matches!(read_frame::<_, Response>(&mut client), Ok(Response::Pong)),
        "the pipelined Ping must be answered after the backfill, from bytes the peek left alone"
    );
}

// ── T7 – T8: THE OPERATOR'S DOOR ─────────────────────────────────────────────────────────────────
//
// `ListBackfills` and `CancelBackfill` against a MOUNTED table — where they answer for real rather
// than with the no-table refusal `crates/vike-datahub/tests/auth_roundtrip.rs`'s sweeps see.

const OBSERVE_KEY: &[u8] = b"datahub-observe-key";
const CONTROL_KEY: &[u8] = b"datahub-control-key";

fn keys() -> NodeKeys {
    NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec())
}

/// Serve `table` over an empty in-memory store on an ephemeral loopback port — key-less when
/// `keys` is `None`, keyed otherwise.
fn serve_table(table: BackfillTable, keys: Option<NodeKeys>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve_authed(listener, store, Some(table), keys, None, None, None);
    });
    addr
}

/// `lane` mounted as [`VENUE`]'s TICK-BARS row — a CHUNKED lane, so a cancel can stop it.
fn stoppable(lane: BackfillFn) -> BackfillTable {
    BackfillTable::new(Vec::new()).with(VENUE, BackfillLane::TickBars, lane)
}

fn ids(rows: &[RunningBackfill]) -> Vec<u64> {
    rows.iter().map(|r| r.id).collect()
}

/// **T7.** An operator's cancel from a SECOND connection stops the first connection's backfill at
/// its next boundary. The second connection is answered at once with the request it flagged; the
/// first — whose client is still there, unlike T5's — is answered with a `Response::Error` naming
/// the cancel and carrying the collector's own account of what is stored, and its connection stays
/// open for the next request. The request then leaves the list.
///
/// ⚠ K6 kills this test: a composed probe that leaves the cancel flag out asks only the peek, the
/// fetcher is present, and boundary 1 reads `Stored(1)` instead of `Stopped(1)`.
#[test]
fn an_operator_cancel_from_another_connection_stops_the_backfill_and_answers_both() {
    let (lane, driver) = chunked_lane();
    let addr = serve_table(stoppable(lane), None);
    let mut fetcher = TcpStream::connect(addr).expect("connect");
    write_frame(&mut fetcher, &backfill_request()).expect("send the Backfill");
    assert_eq!(driver.step(), Lane::Stored(0), "the backfill is running");

    let mut operator = DatahubClient::connect(addr).expect("the operator connects");
    assert!(operator.serves_backfill_cancel(), "{:?}", operator.features());
    let listed = operator.list_backfills().expect("the list is served");
    assert_eq!(listed.len(), 1, "{listed:?}");
    let row = &listed[0];
    assert_eq!(
        (row.venue.as_str(), row.symbol.as_str(), row.interval.as_str()),
        (VENUE, "EURUSD", "1h")
    );
    assert_eq!((row.start, row.end), (0, 10 * 3_600_000));
    assert_eq!(row.lane, "TickBars");
    assert!(row.stoppable && !row.cancelled, "{row:?}");
    assert_eq!(row.boundaries, 1, "one boundary reached: the top of chunk 0");
    assert_eq!(
        row.peer.as_deref(),
        Some(fetcher.local_addr().expect("local").to_string().as_str()),
        "the peer is the fetching connection's"
    );

    let done = operator.cancel_backfill(VENUE, "EURUSD", "1h").expect("the cancel is served");
    assert_eq!(ids(&done.flagged), vec![row.id], "{done:?}");
    assert!(done.flagged[0].cancelled, "the answer shows the raised flag");
    assert!(done.unstoppable.is_empty(), "{done:?}");
    // The cancel did NOT wait: the lane is still parked at its boundary, listed and flagged.
    let listed = operator.list_backfills().expect("list again");
    assert!(listed.len() == 1 && listed[0].cancelled, "{listed:?}");

    assert_eq!(
        driver.step(),
        Lane::Stopped(1),
        "a cancelled backfill must stop at the first boundary after the cancel"
    );
    fetcher.set_read_timeout(Some(WAIT)).expect("timeout");
    match read_frame::<_, Response>(&mut fetcher).expect("the cancelled client is ANSWERED") {
        Response::Error(msg) => {
            assert!(msg.contains("CANCELLED by an operator"), "names the cancel: {msg}");
            assert!(msg.contains("CancelBackfill"), "names the verb that did it: {msg}");
            assert!(msg.contains("resumes"), "says that repeating resumes: {msg}");
            assert!(
                msg.contains("stopped: before chunk 2 of 5; 1 chunk(s) stored"),
                "carries the collector's own account of what is stored: {msg}"
            );
        }
        other => panic!("a cancelled backfill must answer Response::Error, never {other:?}"),
    }
    // ...and the fetcher's connection SURVIVES: an operator's cancel is a refused request, not a
    // dropped connection.
    write_frame(&mut fetcher, &Request::Ping).expect("ping");
    assert!(matches!(read_frame::<_, Response>(&mut fetcher), Ok(Response::Pong)));
    assert!(
        operator.list_backfills().expect("list").is_empty(),
        "a stopped request leaves the registry before its answer is written"
    );
}

/// **T7, the one-batch half (Q6).** A request on a ONE-BATCH lane is LISTED, and the cancel names
/// it as unstoppable rather than flagging it — and it then runs to its end and answers
/// `BackfillDone` as if no cancel had been asked. The fake asks its probe at every boundary, which
/// no real one-batch row does, so the test also proves the flag was never raised: an asked probe
/// would have said stop.
#[test]
fn a_one_batch_backfill_is_listed_and_its_cancel_says_it_cannot_stop() {
    let (lane, driver) = chunked_lane();
    // `BackfillTable::new` mounts a `Klines` row: the one-batch lane.
    let addr = serve_table(BackfillTable::new(vec![(VENUE.to_string(), lane)]), None);
    let mut fetcher = TcpStream::connect(addr).expect("connect");
    write_frame(&mut fetcher, &backfill_request()).expect("send the Backfill");
    assert_eq!(driver.step(), Lane::Stored(0));

    let mut operator = DatahubClient::connect(addr).expect("connect");
    let listed = operator.list_backfills().expect("list");
    assert_eq!(listed.len(), 1, "a one-batch request is LISTED: {listed:?}");
    assert!(!listed[0].stoppable, "{listed:?}");
    assert_eq!(listed[0].lane, "Klines");

    let done = operator.cancel_backfill(VENUE, "EURUSD", "1h").expect("the cancel is served");
    assert!(done.flagged.is_empty(), "a one-batch request must not be flagged: {done:?}");
    assert_eq!(ids(&done.unstoppable), ids(&listed));
    assert!(!done.unstoppable[0].cancelled);

    for chunk in 1..CHUNKS {
        assert_eq!(driver.step(), Lane::Stored(chunk), "the cancel did not touch it");
    }
    assert_eq!(driver.heard.recv_timeout(WAIT).expect("the lane ends"), Lane::Finished);
    fetcher.set_read_timeout(Some(WAIT)).expect("timeout");
    match read_frame::<_, Response>(&mut fetcher).expect("answered") {
        Response::BackfillDone(done) => assert_eq!(done.rows_written, CHUNKS as u64),
        other => panic!("an uncancellable backfill runs to its end: {other:?}"),
    }
}

/// **The panic property, on the wire.** A collector that PANICS mid-request takes its connection
/// thread down — and its registry entry with it: the request is listed while it runs and gone once
/// the thread has unwound, with no explicit cleanup having run. A removal written as a statement
/// after the collector call would leave it listed forever.
#[test]
fn a_panicking_collector_leaves_no_entry_in_the_registry() {
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let lane: BackfillFn =
        Box::new(move |_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| {
            let _ = entered_tx.send(());
            let _ = go_rx.lock().expect("one request").recv_timeout(WAIT);
            panic!("a collector panicked mid-request (planted by the test)");
        });
    let addr = serve_table(stoppable(lane), None);
    let mut fetcher = TcpStream::connect(addr).expect("connect");
    write_frame(&mut fetcher, &backfill_request()).expect("send the Backfill");
    entered_rx.recv_timeout(WAIT).expect("the collector is running");

    let mut operator = DatahubClient::connect(addr).expect("connect");
    assert_eq!(operator.list_backfills().expect("list").len(), 1, "guard: it is listed");
    go_tx.send(()).expect("let it panic");

    // The panic drops the connection: the fetcher reads EOF and no reply. The registration is a
    // local of the verb, so it unwinds BEFORE the stream does — once EOF is here, so is its removal.
    fetcher.set_read_timeout(Some(WAIT)).expect("timeout");
    let mut reply = Vec::new();
    let _ = fetcher.read_to_end(&mut reply);
    assert!(reply.is_empty(), "a panicked request answers nothing: {} byte(s)", reply.len());
    assert!(
        operator.list_backfills().expect("the server still serves").is_empty(),
        "the panicking collector LEAKED its registry entry"
    );
}

/// A Control connection starts a backfill on a keyed server, in the background, through the real
/// client — so the client's own `backfill` is what reads the answer.
fn backfill_as_control(addr: SocketAddr) -> thread::JoinHandle<Result<u64, String>> {
    thread::spawn(move || {
        let mut client = DatahubClient::connect_authed(addr, &keys(), Scope::Write)
            .map_err(|e| e.to_string())?;
        client.backfill(VENUE, "EURUSD", "1h", 0, 10 * 3_600_000).map(|done| done.rows_written)
    })
}

/// **T8, the keyed server.** An OBSERVE connection SEES the running request and is REFUSED the
/// cancel — on scope, by the server — and the request runs on unflagged; a CONTROL connection is
/// served both, and its cancel stops it. That is 0101 in one test: an Observe key may see what the
/// operator is fetching and may not stop any of it.
#[test]
fn observe_lists_but_cannot_cancel_and_control_does_both() {
    let (lane, driver) = chunked_lane();
    let addr = serve_table(stoppable(lane), Some(keys()));
    let fetch = backfill_as_control(addr);
    assert_eq!(driver.step(), Lane::Stored(0), "the backfill is running");

    let mut observe = DatahubClient::connect_authed(addr, &keys(), Scope::Read).expect("observe");
    assert_eq!(observe.authenticated_scope(), Some(Scope::Read), "guard: really Observe");
    assert!(observe.serves_backfill_cancel(), "a keyed server advertises it too");
    let listed = observe.list_backfills().expect("Observe is SERVED the list");
    assert_eq!(listed.len(), 1, "{listed:?}");
    let err = observe
        .cancel_backfill(VENUE, "EURUSD", "1h")
        .expect_err("Observe must be REFUSED the cancel");
    assert!(err.contains("CancelBackfill requires the Control scope"), "{err}");
    assert!(
        !observe.list_backfills().expect("the connection survived")[0].cancelled,
        "a refused cancel raised no flag"
    );
    assert_eq!(driver.step(), Lane::Stored(1), "and the backfill runs on");

    let mut control = DatahubClient::connect_authed(addr, &keys(), Scope::Write).expect("control");
    assert_eq!(control.list_backfills().expect("Control lists").len(), 1);
    let done = control.cancel_backfill(VENUE, "EURUSD", "1h").expect("Control cancels");
    assert_eq!(ids(&done.flagged), ids(&listed));
    assert_eq!(driver.step(), Lane::Stopped(2), "Control's cancel stops it");

    let answer = fetch.join().expect("the fetching thread");
    let msg = answer.expect_err("the cancelled request is answered with an error");
    assert!(msg.contains("CANCELLED by an operator"), "{msg}");
}

/// **T8, the key-less server.** A key-less LOOPBACK datahub serves BOTH verbs — the cancel is
/// served wherever `Backfill` is (0101's verdict 1), unlike `DeleteSeries`, which a key-less server
/// refuses (`crates/vike-datahub/tests/auth_roundtrip.rs`'s `a_keyless_server_serves_no_delete_verb`).
/// Advertised, listed and cancelled through the plain unauthenticated client.
#[test]
fn a_keyless_loopback_server_serves_both_registry_verbs() {
    let (lane, driver) = chunked_lane();
    let addr = serve_table(stoppable(lane), None);
    let mut operator = DatahubClient::connect(addr).expect("connect");
    assert_eq!(operator.authenticated_scope(), None, "guard: really key-less");
    assert!(
        operator.features().iter().any(|f| f == FEATURE_BACKFILL_CANCEL),
        "{:?}",
        operator.features()
    );
    assert!(operator.list_backfills().expect("served").is_empty(), "nothing is running yet");
    assert_eq!(
        operator.cancel_backfill(VENUE, "EURUSD", "1h").expect("served"),
        Default::default(),
        "a cancel of a series with nothing running is an empty success"
    );

    let mut fetcher = TcpStream::connect(addr).expect("connect");
    write_frame(&mut fetcher, &backfill_request()).expect("send the Backfill");
    assert_eq!(driver.step(), Lane::Stored(0));
    let done = operator.cancel_backfill(VENUE, "EURUSD", "1h").expect("served key-less");
    assert_eq!(done.flagged.len(), 1, "{done:?}");
    assert_eq!(driver.step(), Lane::Stopped(1));
}
