//! A bar read pages by [`HistoryStoreDatahub::BAR_PAGE_ROWS`] and travels as binary COLUMNS where the server
//! answers them (`FEATURE_BAR_COLUMNS`), and neither changes a row:
//!
//! - a long range comes back as the store's own read and as a JSON pager of the old 10 000-row page over the
//!   same server, every page asked with the big cap and for columns, the read ending on its EMPTY page;
//! - page by page, at the edges (first row, a limit of one, a page cut mid-range, the last row, past the end),
//!   a columns reply carries exactly the bars the JSON reply carries;
//! - a server that does not advertise columns is asked for JSON and read as before;
//! - a columns body that does not decode, plain or LZ4, is refused, never read as fewer bars;
//! - a handle on a thin link (`with_compressed_bars`) reads the same bars LZ4-compressed.
//!
//! The REAL server (`vike_datahub::test_support::spawn_server`) over the in-memory store, reached through a
//! proxy that relays every frame as bytes and records what crossed, so the page count is counted.

use std::assert_matches;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use vike_data::{HistoryStore, HistoryStoreError, HistoryStoreMemory, TimestampRange};
use vike_datahub::test_support::spawn_server;
use vike_datahub_client::proto::{
    BAR_COLUMNS, BAR_COLUMNS_LZ4_MAGIC, BAR_COLUMNS_MAGIC, BarColumnsBody, FEATURE_BAR_COLUMNS,
    FEATURE_SCAN_LIMIT, Request, Response, compress_columns, decode_bar_columns_into,
    encode_bar_columns_into,
};
use vike_datahub_client::test_support::{accept_keyless, spawn_fake_peer};
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};
use vike_hub_protocol::frame::{
    decode_body, read_frame, read_frame_raw, write_frame, write_frame_raw,
};
use vike_hub_protocol::handshake::HandshakeFrame;
use vike_marketdata::Bar;

const VENUE: &str = "dukascopy";
const SYMBOL: &str = "SPX500.CFD";
const INTERVAL: &str = "1m";
/// The page every bar read paged by before `BAR_PAGE_ROWS`.
const SMALL_PAGE: u32 = 10_000;
const FIRST: i64 = 1_600_000_000_000;

fn bar(i: i64) -> Bar {
    let close = 4_000.0 + (i % 977) as f64 * 0.25 + 1.0 / 3.0;
    Bar {
        timestamp: FIRST + i * 60_000,
        open: close - 0.5,
        high: close + 1.25,
        low: close - 1.75,
        close,
        volume: (i % 31) as f64 * 0.1,
        funding: (i % 480 == 0).then_some(-1.25e-5),
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// One bar as its bits: the timestamp, the five required fields, the three optional ones, the symbol.
type BarBits = (i64, [u64; 5], [Option<u64>; 3], Option<String>);

/// Every field's bits: "the same rows" is bit for bit, not `f64` equality.
fn bits(rows: &[Bar]) -> Vec<BarBits> {
    rows.iter()
        .map(|b| {
            (
                b.timestamp,
                [b.open, b.high, b.low, b.close, b.volume].map(f64::to_bits),
                [b.funding, b.bid, b.ask].map(|v| v.map(f64::to_bits)),
                b.symbol.map(String::from),
            )
        })
        .collect()
}

fn decode_bar_columns(body: &[u8]) -> Result<Vec<Bar>, String> {
    let mut out = Vec::new();
    decode_bar_columns_into(body, &mut out).map(|_| out)
}

fn served(rows: usize) -> (Arc<dyn HistoryStore + Send + Sync>, SocketAddr) {
    let store = HistoryStoreMemory::new();
    let seed: Vec<Bar> = (0..rows as i64).map(bar).collect();
    assert_eq!(store.append_bars(VENUE, SYMBOL, INTERVAL, &seed, Some("seed")).unwrap(), rows);
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let server = spawn_server(Arc::clone(&store));
    (store, server)
}

/// What crossed the proxy: every request, and the bar count of every bar reply (JSON or columns).
#[derive(Default)]
struct Crossed {
    requests: Vec<Request>,
    bar_pages: Vec<usize>,
    /// How many bar replies crossed LZ4-compressed.
    compressed_pages: usize,
}

/// Relay frames between each client and `upstream` as BYTES, decoding a copy of each to record it.
fn spawn_recording_proxy(upstream: SocketAddr) -> (String, Arc<Mutex<Crossed>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port").to_string();
    let crossed: Arc<Mutex<Crossed>> = Arc::default();
    let seen = Arc::clone(&crossed);
    thread::spawn(move || {
        for client in listener.incoming() {
            let mut client = client.expect("accept the client");
            let mut server = TcpStream::connect(upstream).expect("dial the real server");
            let (mut back_to_client, mut back_from_server) =
                (client.try_clone().unwrap(), server.try_clone().unwrap());
            let replies = Arc::clone(&seen);
            thread::spawn(move || {
                while let Ok(body) = read_frame_raw(&mut back_from_server) {
                    let compressed = body.starts_with(&BAR_COLUMNS_LZ4_MAGIC);
                    let bars = if compressed || body.starts_with(&BAR_COLUMNS_MAGIC) {
                        Some(decode_bar_columns(&body).expect("the server's columns decode").len())
                    } else {
                        match serde_json::from_slice::<Response>(&body) {
                            Ok(Response::Bars(bars)) => Some(bars.len()),
                            _ => None,
                        }
                    };
                    let mut seen = replies.lock().unwrap();
                    seen.bar_pages.extend(bars);
                    seen.compressed_pages += usize::from(compressed);
                    drop(seen);
                    if write_frame_raw(&mut back_to_client, &body).is_err() {
                        return;
                    }
                }
            });
            let requests = Arc::clone(&seen);
            thread::spawn(move || {
                // The handshake's frames (`vike_hub_protocol::handshake`'s, no `Request`) cross as they are.
                while let Ok(body) = read_frame_raw(&mut client) {
                    if decode_body::<HandshakeFrame>(&body).is_ok() {
                        if write_frame_raw(&mut server, &body).is_err() {
                            return;
                        }
                        continue;
                    }
                    let Ok(request) = decode_body::<Request>(&body) else { return };
                    requests.lock().unwrap().requests.push(request.clone());
                    if write_frame(&mut server, &request).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (addr, crossed)
}

/// The old pager by hand over a plain client: JSON, 10 000-row pages from `last + 1` to the first empty one.
fn small_json_page_read(server: SocketAddr, range: TimestampRange) -> Vec<Bar> {
    let mut client = DatahubClient::connect(server).expect("dial the real server");
    let mut out = Vec::new();
    let mut start = range.start;
    loop {
        let window = TimestampRange { start, end: range.end };
        let page = client
            .load_bars(VENUE, SYMBOL, INTERVAL, window, Some(SMALL_PAGE))
            .expect("a small page answers");
        let Some(last) = page.last().map(|b| b.timestamp) else { break };
        out.extend(page);
        start = Some(last + 1);
    }
    out
}

#[test]
fn big_column_pages_return_the_rows_small_json_pages_do_and_end_on_an_empty_page() {
    let page = HistoryStoreDatahub::BAR_PAGE_ROWS as usize;
    let rows = 2 * page + 1_234;
    let (store, server) = served(rows);
    let (proxy, crossed) = spawn_recording_proxy(server);
    let remote = HistoryStoreDatahub::new(proxy);

    let all = TimestampRange::all();
    let big = remote.load_bars(VENUE, SYMBOL, INTERVAL, all).expect("the big-page read");
    assert_eq!(big.len(), rows, "every bar crossed");
    assert!(bits(&big) == bits(&store.load_bars(VENUE, SYMBOL, INTERVAL, all).unwrap()));
    assert!(bits(&big) == bits(&small_json_page_read(server, all)), "the JSON pager's bars");
    let crossed = crossed.lock().unwrap();
    assert_eq!(crossed.bar_pages, [page, page, 1_234, 0], "three pages and the empty one");
    let asked: Vec<(Option<u32>, Option<BarColumnsBody>)> = crossed
        .requests
        .iter()
        .filter_map(|r| match r {
            Request::LoadBars { limit, columns, .. } => Some((*limit, *columns)),
            _ => None,
        })
        .collect();
    let plain = Some(BarColumnsBody::Plain);
    assert_eq!(asked, [(Some(HistoryStoreDatahub::BAR_PAGE_ROWS), plain); 4]);
    drop(crossed);

    // The same read over a thin link: every page asked LZ4 at the thin-link page, answered compressed, the
    // same bars.
    let (proxy, crossed) = spawn_recording_proxy(server);
    let thin = HistoryStoreDatahub::new(proxy).with_compressed_bars();
    let packed = thin.load_bars(VENUE, SYMBOL, INTERVAL, all).expect("the compressed read");
    assert!(bits(&packed) == bits(&big), "the compressed pages carry the same bars");
    let crossed = crossed.lock().unwrap();
    let thin_page = HistoryStoreDatahub::THIN_LINK_BAR_PAGE_ROWS as usize;
    let mut pages = vec![thin_page; rows / thin_page];
    pages.extend([rows % thin_page, 0]);
    assert_eq!(crossed.bar_pages, pages);
    // The empty page is smaller plain than compressed, so the server answers it plain.
    assert_eq!(
        crossed.compressed_pages,
        pages.len() - 1,
        "every page with bars crossed compressed"
    );
    let asked: Vec<_> = crossed
        .requests
        .iter()
        .filter_map(|r| match r {
            Request::LoadBars { columns, .. } => Some(*columns),
            _ => None,
        })
        .collect();
    assert_eq!(asked, vec![Some(BarColumnsBody::Lz4); pages.len()], "every page asked LZ4");
    assert!(
        crossed.requests.iter().all(|r| match r {
            Request::LoadBars { limit, .. } => *limit == Some(thin_page as u32),
            _ => true,
        }),
        "at the thin-link page"
    );
}

#[test]
fn a_columns_page_carries_the_bars_the_json_page_carries_at_every_edge() {
    const ROWS: i64 = 25_000;
    let (_store, server) = served(ROWS as usize);
    let mut client = DatahubClient::connect(server).expect("dial the real server");
    assert!(client.serves_bar_columns(), "every serving build advertises `{FEATURE_BAR_COLUMNS}`");
    let at = |i: i64| bar(i).timestamp;
    let windows = [
        TimestampRange::all(),
        TimestampRange { start: Some(at(0)), end: None },
        TimestampRange { start: Some(at(0) + 1), end: None },
        TimestampRange::of(at(9_999), at(10_000)),
        TimestampRange { start: Some(at(ROWS - 1)), end: None },
        TimestampRange { start: Some(at(ROWS - 1) + 1), end: None },
        TimestampRange::of(at(0) - 120_000, at(0) - 1),
    ];
    for window in windows {
        for limit in [None, Some(1), Some(2), Some(9_999), Some(10_000), Some(SMALL_PAGE * 3)] {
            let json = client.load_bars(VENUE, SYMBOL, INTERVAL, window, limit).expect("JSON");
            for body in [BarColumnsBody::Plain, BarColumnsBody::Lz4] {
                let columns = client
                    .load_bar_columns(VENUE, None, SYMBOL, INTERVAL, window, limit, body)
                    .expect("columns");
                assert!(bits(&columns) == bits(&json), "{window:?} limit {limit:?} {body:?}");
            }
        }
    }
    // A refusal is the JSON arm's, word for word: a lane the server does not declare.
    let lane = |c: &mut DatahubClient, columns: bool| {
        let all = TimestampRange::all();
        if columns {
            c.load_bar_columns(
                VENUE,
                Some("nosuchlane"),
                SYMBOL,
                INTERVAL,
                all,
                Some(5),
                BarColumnsBody::Plain,
            )
        } else {
            c.load_bars_sourced(
                VENUE,
                "nosuchlane",
                SYMBOL,
                INTERVAL,
                TimestampRange::all(),
                Some(5),
            )
        }
    };
    let (json, columns) = (lane(&mut client, false), lane(&mut client, true));
    assert!(json.is_err(), "an undeclared lane is refused: {json:?}");
    assert_eq!(columns.unwrap_err(), json.unwrap_err());
}

/// A datahub from before the columns: `HistoryStoreDatahub` asks it for JSON and reads it as ever, and the
/// client's own columns read is refused locally, sending nothing.
#[test]
fn a_server_without_the_columns_is_asked_for_json() {
    let reply = |r: &Request| match r {
        Request::LoadBars { start: None, .. } => Some(Response::Bars(vec![bar(0), bar(1)])),
        Request::LoadBars { .. } => Some(Response::Bars(Vec::new())),
        _ => None,
    };
    let (addr, received, _) = spawn_fake_peer(vec![FEATURE_SCAN_LIMIT.to_string()], reply);
    {
        let remote = HistoryStoreDatahub::new(addr.to_string());
        let read = remote.load_bars(VENUE, SYMBOL, INTERVAL, TimestampRange::all()).expect("JSON");
        assert!(bits(&read) == bits(&[bar(0), bar(1)]));
    }
    let asked = received.recv().expect("the peer reports what arrived");
    assert_eq!(asked.len(), 2, "the page and the empty one");
    assert!(asked.iter().all(|r| matches!(r, Request::LoadBars { columns: None, .. })));

    let (addr, received, _) = spawn_fake_peer(vec![FEATURE_SCAN_LIMIT.to_string()], reply);
    {
        let mut client = DatahubClient::connect(addr).expect("dial");
        let refused = client
            .load_bar_columns(
                VENUE,
                None,
                SYMBOL,
                INTERVAL,
                TimestampRange::all(),
                None,
                BarColumnsBody::Plain,
            )
            .expect_err("refused locally");
        assert!(refused.contains(FEATURE_BAR_COLUMNS), "{refused}");
    }
    assert!(received.recv().unwrap().is_empty(), "nothing was sent");
}

/// A columns reply that does not decode — cut short, or with a byte after its end — is an error from the
/// read, never fewer bars.
#[test]
fn a_malformed_columns_reply_is_refused() {
    let mut good = Vec::new();
    encode_bar_columns_into(&mut good, &[bar(0), bar(1), bar(2)]);
    let mut longer = good.clone();
    longer.push(7);
    let packed = compress_columns(&BAR_COLUMNS, &good);
    let mut oversized = BAR_COLUMNS_LZ4_MAGIC.to_vec();
    oversized.extend_from_slice(&u32::MAX.to_le_bytes());
    let mut short_block = packed.clone();
    short_block.truncate(packed.len() - 3);
    let mut wrong_length = packed.clone();
    wrong_length[4] = wrong_length[4].wrapping_add(1);
    let bodies = [
        good[..good.len() - 1].to_vec(),
        longer,
        BAR_COLUMNS_MAGIC.to_vec(),
        oversized,
        short_block,
        wrong_length,
        compress_columns(&BAR_COLUMNS, &good[..good.len() - 1]),
    ];
    for body in bodies {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().unwrap().to_string();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept the client");
            let features = vec![FEATURE_SCAN_LIMIT.to_string(), FEATURE_BAR_COLUMNS.to_string()];
            assert!(accept_keyless(&mut stream, features));
            let _request = read_frame::<_, Request>(&mut stream).expect("the read");
            write_frame_raw(&mut stream, &body).expect("the garbled reply");
        });
        let remote = HistoryStoreDatahub::new(addr);
        let failed = remote.load_bars(VENUE, SYMBOL, INTERVAL, TimestampRange::all());
        assert_matches!(
            failed,
            Err(HistoryStoreError::Query(why)) if why.contains("malformed bar-columns reply")
        );
    }
}
