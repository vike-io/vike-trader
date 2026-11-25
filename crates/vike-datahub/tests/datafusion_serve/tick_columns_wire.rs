//! The tick-level scans' binary COLUMNS (`FEATURE_TICK_COLUMNS`) against the REAL store and the REAL server:
//! a `HistoryStoreDataFusion` seeded with quotes, trades, book updates, depth and perp metrics, served on
//! loopback by `vike_datahub::test_support::spawn_server`.
//!
//! - **The columns are the JSON reply.** At every edge (the first row, one past it, a cut mid-range, the last
//!   row, past the end, before the start) and every `limit` (none, one, a split timestamp group, the old page,
//!   past the old page), each kind's columns reply, plain and LZ4, carries the rows its JSON reply carries,
//!   bit for bit (both re-encoded to the same body), and a refusal is the same words.
//! - **`HistoryStoreDatahub` reads every kind as the local store does**, through columns, plain and compressed.
//! - **An OLD client's frame (no `columns` key) is answered JSON**, as before the field.
//!
//! The other direction (a NEW client against an OLD server) is `crates/vike-datahub-client/tests/remote_tick_columns.rs`'s.
//! The timing probe at the end measures bytes and time per million ticks, JSON pages against columns. Run on a
//! lane: `cargo test -p vike-datahub --features datafusion-serve --release --test datafusion_serve -- --ignored --nocapture tick_columns_timing`.
#![cfg(feature = "datafusion-serve")]

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use vike_data::{HistoryStore, PerpMetricRow, TimestampRange};
use vike_datahub::test_support::spawn_server;
use vike_datahub_client::proto::{
    BAR_COLUMNS, BOOK_UPDATE_COLUMNS, BarColumnsBody, ColumnRow, ColumnsMagic, DEPTH_COLUMNS,
    EQUITY_COLUMNS, FEATURE_TICK_COLUMNS, PERP_METRIC_COLUMNS, QUOTE_COLUMNS, Request, Response,
    TRADE_COLUMNS, columns_len, compress_columns, encode_columns_into,
};
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};
use vike_hub_protocol::frame::{read_frame, read_frame_raw, write_frame};
use vike_marketdata::{Bar, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_model::EquitySample;
use vike_store::HistoryStoreDataFusion;

const VENUE: &str = "binance";
const SYMBOL: &str = "WIRETICKUSDT";
/// 2023-11-14T22:13:20Z: the rows cross a UTC day, so a read spans two parts.
const FIRST: i64 = 1_700_000_000_000;

/// Quotes in groups of three per timestamp, so a `limit` can split a group.
fn quotes(n: i64) -> Vec<TickQuote> {
    (0..n)
        .map(|i| TickQuote {
            timestamp: FIRST + (i / 3) * 20,
            local_timestamp: if i % 5 == 0 { 0 } else { FIRST + (i / 3) * 20 + 4 },
            bid: 64_000.0 + (i % 997) as f64 / 3.0,
            ask: 64_000.5 + (i % 997) as f64 / 3.0,
            bid_size: (i % 7) as f64 * 0.125,
            ask_size: 1e-8 * i as f64,
            symbol: Default::default(),
        })
        .collect()
}

fn trades(n: i64) -> Vec<TickTrade> {
    (0..n)
        .map(|i| TickTrade {
            timestamp: FIRST + (i / 2) * 15,
            local_timestamp: if i % 3 == 0 { 0 } else { FIRST + (i / 2) * 15 + 2 },
            price: 64_000.0 + (i % 211) as f64 * 0.1,
            size: 0.001 * (i % 50 + 1) as f64,
            is_buyer_maker: i % 3 == 1,
            symbol: Default::default(),
        })
        .collect()
}

/// A snapshot of `levels` a side every fiftieth event, deltas between, a gap and its recovery among them.
fn books(n: i64, price: f64) -> Vec<L2BookUpdate> {
    (0..n)
        .map(|i| {
            let level = |k: i64| L2BookLevel::new(price + (k % 400) as f64 * 0.1, 1.0 + k as f64);
            let (kind, sides) = match i % 50 {
                0 => (L2BookUpdateKind::Snapshot, (0..200, 200..400)),
                17 => (L2BookUpdateKind::GapStart, (0..0, 0..0)),
                18 => (L2BookUpdateKind::LiveResume, (0..0, 0..0)),
                k => (L2BookUpdateKind::Delta, (k..k + 2, k + 300..k + 301)),
            };
            L2BookUpdate {
                timestamp: FIRST + i * 250,
                local_timestamp: FIRST + i * 250 + 1,
                sequence: i as u64 + 1,
                kind,
                tick_size: 0.1,
                bids: sides.0.map(level).collect(),
                asks: sides.1.map(level).collect(),
                symbol: String::new(),
            }
        })
        .collect()
}

fn perp_metrics(n: i64) -> Vec<PerpMetricRow> {
    (0..n)
        .map(|i| PerpMetricRow {
            ts: FIRST + i * 3_600_000,
            premium: -1.25e-5 * (i % 13) as f64,
            open_interest: (i % 4 != 0).then_some(4_200.5 + i as f64),
        })
        .collect()
}

const QUOTES: i64 = 25_000;
const TRADES: i64 = 24_000;
const BOOKS: i64 = 1_200;
const PERPS: i64 = 600;

/// The store and its server; the `TempDir` keeps the store alive.
fn served() -> (TempDir, Arc<dyn HistoryStore + Send + Sync>, SocketAddr) {
    let dir = tempfile::tempdir().expect("temp store dir");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("open store");
    let n = store.append_quotes(VENUE, SYMBOL, &quotes(QUOTES), Some("q")).unwrap();
    assert_eq!(n, QUOTES as usize, "quotes land");
    let n = store.append_trades(VENUE, SYMBOL, &trades(TRADES), Some("t")).unwrap();
    assert_eq!(n, TRADES as usize, "trades land");
    assert!(store.append_book_updates(VENUE, SYMBOL, &books(BOOKS, 100.0), Some("b")).unwrap() > 0);
    assert!(store.append_depth(VENUE, SYMBOL, &books(BOOKS, 200.0), Some("d")).unwrap() > 0);
    let n = store.append_perp_metrics(VENUE, SYMBOL, &perp_metrics(PERPS), Some("p")).unwrap();
    assert_eq!(n, PERPS as usize, "perp metrics land");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let server = spawn_server(Arc::clone(&store));
    (dir, store, server)
}

/// `rows` as `magic`'s plain body: "the same rows" is these bytes, every field bit for bit.
fn body<R: ColumnRow>(magic: &ColumnsMagic, rows: &[R]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_columns_into(&mut out, magic, rows);
    out
}

/// One kind's JSON verb and its columns request, for [`same_at_every_edge`].
#[expect(clippy::type_complexity, reason = "a verb's closure, spelled where it is read")]
struct Kind<'a, R> {
    magic: &'a ColumnsMagic,
    /// The seeded rows' timestamps, in order.
    at: Vec<i64>,
    json: &'a dyn Fn(&mut DatahubClient, TimestampRange, Option<u32>) -> Result<Vec<R>, String>,
    scan: &'a dyn Fn(TimestampRange, Option<u32>, BarColumnsBody) -> Request,
}

/// At every edge and `limit`, the columns reply (plain and LZ4) is the JSON reply: the same body re-encoded,
/// or the same refusal.
fn same_at_every_edge<R: ColumnRow>(client: &mut DatahubClient, kind: Kind<'_, R>) {
    let at = |i: usize| kind.at[i.min(kind.at.len() - 1)];
    let last = kind.at.len() - 1;
    let windows = [
        TimestampRange::all(),
        TimestampRange { start: Some(at(0)), end: None },
        TimestampRange { start: Some(at(0) + 1), end: None },
        TimestampRange::of(at(last / 3), at(last / 2)),
        TimestampRange { start: Some(at(last)), end: None },
        TimestampRange { start: Some(at(last) + 1), end: None },
        TimestampRange::of(at(0) - 120_000, at(0) - 1),
    ];
    let mut rows_seen = 0;
    for window in windows {
        for limit in [None, Some(1), Some(2), Some(4), Some(9_999), Some(10_000), Some(30_000)] {
            let json = (kind.json)(client, window, limit);
            for asked in [BarColumnsBody::Plain, BarColumnsBody::Lz4] {
                let mut columns: Vec<R> = Vec::new();
                let got =
                    client.scan_columns_into(&(kind.scan)(window, limit, asked), &mut columns);
                let what = format!("{} {window:?} limit {limit:?} {asked:?}", kind.magic.name);
                match (&json, got) {
                    (Ok(json), Ok(n)) => {
                        assert_eq!(n, json.len(), "{what}: the row count");
                        assert!(body(kind.magic, &columns) == body(kind.magic, json), "{what}");
                        rows_seen += n;
                    }
                    (Err(json), Err(columns)) => assert_eq!(&columns, json, "{what}: the refusal"),
                    (json, got) => {
                        let json = json.as_ref().map(Vec::len);
                        panic!("{what}: JSON {json:?} but columns {got:?}")
                    }
                }
            }
        }
    }
    assert!(rows_seen > 0, "{}: the edges read rows, not only empty pages", kind.magic.name);
}

#[test]
fn every_kinds_columns_carry_the_rows_its_json_carries_at_every_edge() {
    let (_dir, _store, server) = served();
    let mut client = DatahubClient::connect(server).expect("dial the real server");
    assert!(
        client.serves_tick_columns(),
        "every serving build advertises `{FEATURE_TICK_COLUMNS}`"
    );
    let (v, s) = (VENUE.to_string(), SYMBOL.to_string());
    same_at_every_edge(
        &mut client,
        Kind {
            magic: &QUOTE_COLUMNS,
            at: quotes(QUOTES).iter().map(|r| r.timestamp).collect(),
            json: &|c, w, n| c.scan_quotes(VENUE, SYMBOL, w, n),
            scan: &|w, limit, b| Request::ScanQuotes {
                venue: v.clone(),
                symbol: s.clone(),
                start: w.start,
                end: w.end,
                limit,
                source: None,
                columns: Some(b),
            },
        },
    );
    same_at_every_edge(
        &mut client,
        Kind {
            magic: &TRADE_COLUMNS,
            at: trades(TRADES).iter().map(|r| r.timestamp).collect(),
            json: &|c, w, n| c.scan_trades(VENUE, SYMBOL, w, n),
            scan: &|w, limit, b| Request::ScanTrades {
                venue: v.clone(),
                symbol: s.clone(),
                start: w.start,
                end: w.end,
                limit,
                columns: Some(b),
            },
        },
    );
    let book_at: Vec<i64> = books(BOOKS, 0.0).iter().map(|r| r.timestamp).collect();
    same_at_every_edge(
        &mut client,
        Kind {
            magic: &BOOK_UPDATE_COLUMNS,
            at: book_at.clone(),
            json: &|c, w, n| c.scan_book_updates(VENUE, SYMBOL, w, n),
            scan: &|w, limit, b| Request::ScanBookUpdates {
                venue: v.clone(),
                symbol: s.clone(),
                start: w.start,
                end: w.end,
                limit,
                columns: Some(b),
            },
        },
    );
    same_at_every_edge(
        &mut client,
        Kind {
            magic: &DEPTH_COLUMNS,
            at: book_at,
            json: &|c, w, n| c.scan_depth(VENUE, SYMBOL, w, n),
            scan: &|w, limit, b| Request::ScanDepth {
                venue: v.clone(),
                symbol: s.clone(),
                start: w.start,
                end: w.end,
                limit,
                columns: Some(b),
            },
        },
    );
    same_at_every_edge(
        &mut client,
        Kind {
            magic: &PERP_METRIC_COLUMNS,
            at: perp_metrics(PERPS).iter().map(|r| r.ts).collect(),
            json: &|c, w, n| c.scan_perp_metrics(VENUE, SYMBOL, w, n),
            scan: &|w, limit, b| Request::ScanPerpMetrics {
                venue: v.clone(),
                symbol: s.clone(),
                start: w.start,
                end: w.end,
                limit,
                columns: Some(b),
            },
        },
    );
}

/// `HistoryStoreDatahub` reads every kind, through columns, as the local store answers it, plain and
/// compressed; and none is empty, or the equality would hold against nothing.
#[test]
fn history_store_datahub_reads_every_kind_through_columns_as_the_store_does() {
    let (_dir, store, server) = served();
    let all = TimestampRange::all();
    let local = [
        body(&QUOTE_COLUMNS, &store.scan_quotes(VENUE, SYMBOL, all).unwrap()),
        body(&TRADE_COLUMNS, &store.scan_trades(VENUE, SYMBOL, all).unwrap()),
        body(&BOOK_UPDATE_COLUMNS, &store.scan_book_updates(VENUE, SYMBOL, all).unwrap()),
        body(&DEPTH_COLUMNS, &store.scan_depth(VENUE, SYMBOL, all).unwrap()),
        body(&PERP_METRIC_COLUMNS, &store.scan_perp_metrics(VENUE, SYMBOL, all).unwrap()),
    ];
    assert!(local.iter().all(|b| b.len() > 1_000), "every kind holds rows");
    for thin in [false, true] {
        let remote = HistoryStoreDatahub::new(server.to_string());
        let remote = if thin { remote.with_compressed_bars() } else { remote };
        let read = [
            body(&QUOTE_COLUMNS, &remote.scan_quotes(VENUE, SYMBOL, all).unwrap()),
            body(&TRADE_COLUMNS, &remote.scan_trades(VENUE, SYMBOL, all).unwrap()),
            body(&BOOK_UPDATE_COLUMNS, &remote.scan_book_updates(VENUE, SYMBOL, all).unwrap()),
            body(&DEPTH_COLUMNS, &remote.scan_depth(VENUE, SYMBOL, all).unwrap()),
            body(&PERP_METRIC_COLUMNS, &remote.scan_perp_metrics(VENUE, SYMBOL, all).unwrap()),
        ];
        for (kind, (remote, local)) in read.iter().zip(&local).enumerate() {
            assert!(remote == local, "kind {kind}, thin link {thin}: the store's rows");
        }
    }
    // The book and the depth lane hold different rows, so a crossed wire would show above.
    assert_ne!(local[2][8..], local[3][8..], "guard: the two book lanes differ");
}

/// A frame an OLD client writes (no `columns` key: the field did not exist) is answered JSON, as ever.
#[test]
fn an_old_clients_scan_is_answered_json_as_before() {
    let (_dir, store, server) = served();
    let mut stream = TcpStream::connect(server).expect("connect");
    let old = |verb: &str| {
        format!(
            r#"{{"{verb}":{{"venue":"{VENUE}","symbol":"{SYMBOL}","start":null,"end":null,"limit":40}}}}"#
        )
    };
    let all = TimestampRange::all();
    for verb in ["ScanQuotes", "ScanTrades", "ScanBookUpdates", "ScanDepth", "ScanPerpMetrics"] {
        let frame = old(verb).into_bytes();
        stream.write_all(&(frame.len() as u32).to_be_bytes()).unwrap();
        stream.write_all(&frame).unwrap();
        let reply = read_frame::<_, Response>(&mut stream).expect("a JSON reply");
        let (got, want) = match reply {
            Response::Quotes(r) => (body(&QUOTE_COLUMNS, &r), {
                let rows = store.scan_quotes(VENUE, SYMBOL, all).unwrap();
                body(&QUOTE_COLUMNS, &rows[..r.len()])
            }),
            Response::Trades(r) => (body(&TRADE_COLUMNS, &r), {
                let rows = store.scan_trades(VENUE, SYMBOL, all).unwrap();
                body(&TRADE_COLUMNS, &rows[..r.len()])
            }),
            Response::BookUpdates(r) => (body(&BOOK_UPDATE_COLUMNS, &r), {
                let rows = store.scan_book_updates(VENUE, SYMBOL, all).unwrap();
                body(&BOOK_UPDATE_COLUMNS, &rows[..r.len()])
            }),
            Response::Depth(r) => (body(&DEPTH_COLUMNS, &r), {
                let rows = store.scan_depth(VENUE, SYMBOL, all).unwrap();
                body(&DEPTH_COLUMNS, &rows[..r.len()])
            }),
            Response::PerpMetrics(r) => (body(&PERP_METRIC_COLUMNS, &r), {
                let rows = store.scan_perp_metrics(VENUE, SYMBOL, all).unwrap();
                body(&PERP_METRIC_COLUMNS, &rows[..r.len()])
            }),
            other => panic!("{verb}: an old client's scan must get its JSON rows, got {other:?}"),
        };
        assert!(got.len() > 20, "{verb}: rows came back");
        assert!(got == want, "{verb}: the store's first rows");
    }
}

/// What a columns reply of `rows` is on the wire: the one-pass body, or, asked LZ4, that body compressed where
/// LZ4 shrinks it.
fn wire_body<R: ColumnRow>(magic: &ColumnsMagic, rows: &[R], asked: BarColumnsBody) -> Vec<u8> {
    let plain = body(magic, rows);
    let packed = compress_columns(magic, &plain);
    match asked {
        BarColumnsBody::Lz4 if packed.len() < plain.len() => packed,
        _ => plain,
    }
}

/// One request on a raw socket, and its reply's body as the bytes that crossed.
fn raw_reply(stream: &mut TcpStream, request: &Request) -> Vec<u8> {
    write_frame(stream, request).expect("send the request");
    read_frame_raw(stream).expect("the reply's frame")
}

/// **The bytes on the wire**: a bar read's, a tick scan's and a D3 scan's columns reply, plain and LZ4, are
/// byte for byte the one-pass encoding of the rows their JSON reply carries (and its LZ4 block). The codec's
/// `holds_every_way` holds that encoding to the streamed writer the datahub wrote with before, for every kind,
/// so the wire carries what the streamed writer put on it.
#[test]
fn a_columns_reply_is_the_one_pass_encoding_of_its_json_rows_byte_for_byte() {
    let dir = tempfile::tempdir().expect("temp store dir");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("open store");
    let bars: Vec<Bar> = (0..3_000)
        .map(|i| {
            let close = 64_000.0 + (i % 97) as f64 / 3.0;
            Bar::ohlcv(FIRST + i * 60_000, close - 0.5, close + 1.0, close - 1.0, close, 0.1)
        })
        .collect();
    assert_eq!(store.append_bars(VENUE, SYMBOL, "1m", &bars, Some("b")).unwrap(), bars.len());
    store.append_quotes(VENUE, SYMBOL, &quotes(5_000), Some("q")).unwrap();
    let equity: Vec<EquitySample> = (0..2_000)
        .map(|i| EquitySample {
            timestamp: FIRST + i * 1_000,
            venue: VENUE.to_string(),
            equity: 10_000.0 + i as f64 / 7.0,
            realized: (i % 13) as f64,
            unrealized: -(i as f64) / 3.0,
            missing_prices: (i % 3) as u32,
        })
        .collect();
    assert!(store.append_equity(VENUE, SYMBOL, &equity, Some("e")).unwrap() > 0);
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let server = spawn_server(store);
    // A raw key-less socket that names no codec: its replies are JSON unless a request asks for columns.
    let mut stream = TcpStream::connect(server).expect("a raw socket");
    let (v, s) = (VENUE.to_string(), SYMBOL.to_string());
    let json = |stream: &mut TcpStream, request: &Request| -> Response {
        serde_json::from_slice(&raw_reply(stream, request)).expect("a JSON reply")
    };
    for asked in [BarColumnsBody::Plain, BarColumnsBody::Lz4] {
        for limit in [None, Some(1_000)] {
            let what = format!("{asked:?} limit {limit:?}");
            let bars = |columns| Request::LoadBars {
                venue: v.clone(),
                symbol: s.clone(),
                interval: "1m".to_string(),
                start: None,
                end: None,
                limit,
                source: None,
                columns,
                resample_to: None,
            };
            let Response::Bars(rows) = json(&mut stream, &bars(None)) else { panic!("bars") };
            let wire = raw_reply(&mut stream, &bars(Some(asked)));
            assert!(wire == wire_body(&BAR_COLUMNS, &rows, asked), "bars, {what}");
            let quotes = |columns| Request::ScanQuotes {
                venue: v.clone(),
                symbol: s.clone(),
                start: None,
                end: None,
                limit,
                source: None,
                columns,
            };
            let Response::Quotes(rows) = json(&mut stream, &quotes(None)) else { panic!("quotes") };
            let wire = raw_reply(&mut stream, &quotes(Some(asked)));
            assert!(wire == wire_body(&QUOTE_COLUMNS, &rows, asked), "quotes, {what}");
            let equity = |columns| Request::ScanEquity {
                venue: v.clone(),
                symbol: s.clone(),
                start: None,
                end: None,
                limit,
                columns,
            };
            let Response::Equity(rows) = json(&mut stream, &equity(None)) else { panic!("equity") };
            let wire = raw_reply(&mut stream, &equity(Some(asked)));
            assert!(wire == wire_body(&EQUITY_COLUMNS, &rows, asked), "equity, {what}");
            assert!(rows.len() > 500, "guard: the D3 read carried rows, {what}");
        }
    }
}

fn timed<T>(what: &str, f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let out = f();
    let took = t.elapsed();
    println!("{what:<56} {:>8.3} s", took.as_secs_f64());
    (out, took)
}

/// A JSON read by hand, the way `HistoryStoreDatahub` paged ticks before the columns: one connection,
/// 10 000-row pages from `last + 1` to the empty one; the bytes are each page's JSON body.
fn json_pages<R: serde::Serialize>(
    server: SocketAddr,
    page: impl Fn(&mut DatahubClient, TimestampRange) -> Result<Vec<R>, String>,
    ts_of: impl Fn(&R) -> i64,
) -> (Vec<R>, usize) {
    let mut client = DatahubClient::connect(server).expect("dial");
    let (mut out, mut start, mut bytes) = (Vec::new(), None, 0);
    loop {
        let rows = page(&mut client, TimestampRange { start, end: None }).expect("a JSON page");
        bytes += serde_json::to_vec(&rows).unwrap().len();
        let Some(last) = rows.last().map(&ts_of) else { break };
        out.extend(rows);
        start = Some(last + 1);
    }
    (out, bytes)
}

/// The columns bytes of `rows` read in pages of `page` rows, plain and LZ4.
fn column_bytes<R: ColumnRow>(magic: &ColumnsMagic, rows: &[R], page: usize) -> (usize, usize) {
    rows.chunks(page).fold((0, 0), |(plain, packed), p| {
        let body = body(magic, p);
        (plain + columns_len(p), packed + compress_columns(magic, &body).len().min(body.len()))
    })
}

/// A TIMING PROBE, not a gate (`#[ignore]`d; it asserts only that every read returns the same rows): a
/// million quotes and a million trades over ten UTC days in the real store, read through the real datahub on
/// loopback, BEFORE (JSON, 10 000-row pages) and AFTER (columns, `BAR_PAGE_ROWS` pages; and LZ4 at the
/// thin-link page). Bytes are each read's bodies; seconds are wall time on one connection.
#[test]
#[ignore = "a timing probe: run by hand on a lane, --release --nocapture"]
fn tick_columns_timing() {
    const DAYS: i64 = 10;
    const PER_DAY: i64 = 100_000;
    const DAY_MS: i64 = 86_400_000;
    let dir = tempfile::tempdir().expect("temp store dir");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("open store");
    let seeded = Instant::now();
    for day in 0..DAYS {
        // Each day's rows from its midnight (2022-01-01 onwards), never past it: one part a day.
        let shift = |ts: i64| ts - FIRST + 1_640_995_200_000 + day * DAY_MS;
        let mut q = quotes(PER_DAY);
        q.iter_mut().for_each(|r| r.timestamp = shift(r.timestamp));
        let mut t = trades(PER_DAY);
        t.iter_mut().for_each(|r| r.timestamp = shift(r.timestamp));
        store.append_quotes(VENUE, SYMBOL, &q, Some(&format!("q{day}"))).unwrap();
        store.append_trades(VENUE, SYMBOL, &t, Some(&format!("t{day}"))).unwrap();
    }
    println!(
        "seeded {} quotes and trades in {:.1} s",
        DAYS * PER_DAY,
        seeded.elapsed().as_secs_f64()
    );
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let server = spawn_server(Arc::clone(&store));
    let all = TimestampRange::all();
    let plain = HistoryStoreDatahub::new(server.to_string());
    let thin = HistoryStoreDatahub::new(server.to_string()).with_compressed_bars();
    let page = HistoryStoreDatahub::BAR_PAGE_ROWS as usize;
    let thin_page = HistoryStoreDatahub::THIN_LINK_BAR_PAGE_ROWS as usize;
    let _warm = plain.scan_trades(VENUE, SYMBOL, all).unwrap();

    let ((json, json_bytes), _) = timed("quotes, BEFORE: JSON, 10 000-row pages", || {
        let page = |c: &mut DatahubClient, w| c.scan_quotes(VENUE, SYMBOL, w, Some(10_000));
        json_pages(server, page, |r: &TickQuote| r.timestamp)
    });
    let (columns, _) = timed("quotes, AFTER: columns, BAR_PAGE_ROWS pages", || {
        plain.scan_quotes(VENUE, SYMBOL, all).unwrap()
    });
    let (packed, _) = timed("quotes, AFTER: columns LZ4, thin-link pages", || {
        thin.scan_quotes(VENUE, SYMBOL, all).unwrap()
    });
    assert_eq!(columns.len(), (DAYS * PER_DAY) as usize);
    let same = body(&QUOTE_COLUMNS, &json);
    assert!(same == body(&QUOTE_COLUMNS, &columns) && same == body(&QUOTE_COLUMNS, &packed));
    let (plain_bytes, _) = column_bytes(&QUOTE_COLUMNS, &columns, page);
    let (_, lz4_bytes) = column_bytes(&QUOTE_COLUMNS, &columns, thin_page);
    println!(
        "quotes per million: JSON {:.1} MB, columns {:.1} MB, LZ4 {:.1} MB",
        json_bytes as f64 / 1e6,
        plain_bytes as f64 / 1e6,
        lz4_bytes as f64 / 1e6
    );

    let ((json, json_bytes), _) = timed("trades, BEFORE: JSON, 10 000-row pages", || {
        let page = |c: &mut DatahubClient, w| c.scan_trades(VENUE, SYMBOL, w, Some(10_000));
        json_pages(server, page, |r: &TickTrade| r.timestamp)
    });
    let (columns, _) = timed("trades, AFTER: columns, BAR_PAGE_ROWS pages", || {
        plain.scan_trades(VENUE, SYMBOL, all).unwrap()
    });
    let (packed, _) = timed("trades, AFTER: columns LZ4, thin-link pages", || {
        thin.scan_trades(VENUE, SYMBOL, all).unwrap()
    });
    let same = body(&TRADE_COLUMNS, &json);
    assert!(same == body(&TRADE_COLUMNS, &columns) && same == body(&TRADE_COLUMNS, &packed));
    let (plain_bytes, _) = column_bytes(&TRADE_COLUMNS, &columns, page);
    let (_, lz4_bytes) = column_bytes(&TRADE_COLUMNS, &columns, thin_page);
    println!(
        "trades per million: JSON {:.1} MB, columns {:.1} MB, LZ4 {:.1} MB",
        json_bytes as f64 / 1e6,
        plain_bytes as f64 / 1e6,
        lz4_bytes as f64 / 1e6
    );
}
