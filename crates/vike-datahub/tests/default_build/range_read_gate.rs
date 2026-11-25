//! A RANGE reply whose reader STOPS READING mid-reply: the datahub's idle write bound
//! (`ServerTimings::range_reply_write_timeout`, `RANGE_REPLY_WRITE_TIMEOUT` in production) closes that
//! connection and gives its read's place back, so the reads queued behind it proceed.
//!
//! The stalled readers take every place one key-less loopback client may hold (`RANGE_READS_PER_CLIENT`;
//! they are one client, by address), and the next read of that client can only be served once one of
//! their places comes back. Without the bound it would wait for good, and this test would fail at its
//! hang guard rather than pass slowly.

use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub::server::{LOAD_BARS_CEILING, ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub_client::PROTO_VERSION;
use vike_datahub_client::proto::{
    BarColumnsBody, RANGE_READS_PER_CLIENT, Request, decode_bar_columns_into,
};
use vike_hub_protocol::frame::{read_frame, read_frame_raw, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
use vike_marketdata::Bar;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";
/// How long a stalled reply may make no progress here: a second, so the test waits seconds, not minutes.
const STALL: Duration = Duration::from_secs(1);

fn bar(i: i64) -> Bar {
    Bar {
        timestamp: 1_600_000_000_000 + i * 60_000,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 3.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn spawn() -> SocketAddr {
    let store = HistoryStoreMemory::new();
    let rows: Vec<Bar> = (0..LOAD_BARS_CEILING as i64).map(bar).collect();
    store.append_bars(VENUE, SYMBOL, INTERVAL, &rows, Some("seed")).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let timings = ServerTimings { range_reply_write_timeout: STALL, ..ServerTimings::PRODUCTION };
    thread::spawn(move || {
        let lanes = ServerLanes::default();
        let _ = serve_with_lanes(listener, store, None, None, None, None, lanes, None, timings);
    });
    addr
}

fn connect(addr: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &HandshakeFrame::Hello { proto_version: PROTO_VERSION })
        .expect("hello");
    let welcome = read_frame::<_, HandshakeReply>(&mut stream);
    assert!(matches!(welcome, Ok(HandshakeReply::Welcome { .. })));
    stream
}

fn load_bars(limit: u32) -> Request {
    Request::LoadBars {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        start: None,
        end: None,
        limit: Some(limit),
        source: None,
        columns: Some(BarColumnsBody::Plain),
        resample_to: None,
    }
}

#[test]
fn a_reader_that_stops_reading_mid_reply_gives_its_place_back() {
    let addr = spawn();
    // Each stalled reader asks three of the biggest pages at once (a ~36 MB body each, more than any
    // loopback buffers hold together), reads four bytes of the first, and reads nothing more: its reply
    // has started, so the server holds its place.
    let stalled: Vec<TcpStream> = (0..RANGE_READS_PER_CLIENT)
        .map(|_| {
            let mut stream = connect(addr);
            for _ in 0..3 {
                write_frame(&mut stream, &load_bars(LOAD_BARS_CEILING as u32)).expect("ask");
            }
            let mut prefix = [0u8; 4];
            stream.read_exact(&mut prefix).expect("its reply has started");
            stream
        })
        .collect();

    // The same client's next read queues behind them, and is served once a stalled reply's bound has
    // closed its connection.
    let mut next = connect(addr);
    next.set_read_timeout(Some(STALL * 60)).expect("the hang guard");
    write_frame(&mut next, &load_bars(10)).expect("ask");
    let body = read_frame_raw(&mut next).expect("served once a stalled place came back");
    let mut bars = Vec::new();
    assert_eq!(decode_bar_columns_into(&body, &mut bars).expect("ten bars"), 10);

    // ...and the stalled connections were CLOSED by the server, each one bound's length after it stalled (the
    // wait covers the last of them): read now, each ends, with what it had or with a reset.
    thread::sleep(STALL * 3);
    for mut stream in stalled {
        stream.set_read_timeout(Some(STALL * 30)).expect("the hang guard");
        let mut rest = Vec::new();
        let read = stream.read_to_end(&mut rest);
        assert!(
            read.is_ok() || read.is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionReset),
            "the server closed the stalled connection rather than leaving it open"
        );
        assert!(rest.len() < 3 * (LOAD_BARS_CEILING * 73), "it never sent the replies whole");
    }
}
