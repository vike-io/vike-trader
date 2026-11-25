//! The tick-level scans through `HistoryStoreDatahub` travel as binary COLUMNS where the server answers them
//! (`FEATURE_TICK_COLUMNS`), and as the JSON pages they always were where it does not:
//!
//! - against a server that advertises the columns, every kind (quotes, trades, book updates, depth, perp
//!   metrics) is asked for its columns at `BAR_PAGE_ROWS`, plain, or LZ4 at `THIN_LINK_BAR_PAGE_ROWS` from a
//!   thin-link handle, the read ending on its EMPTY page, and reads back the rows the server encoded;
//! - against a server that does not (an older datahub), every kind is asked for JSON at the old 10 000-row
//!   page, and the client's own columns read is refused locally, sending nothing;
//! - a columns reply that does not decode, or comes under another reply's magic (depth for a book read), is
//!   refused, never read as fewer or other rows;
//! - a reader that KEEPS the columns (`HistoryStore::read_columns`, the backtesthub's cache) is handed each page as
//!   the server sent it, plain, and a server without the columns is not asked for them.
//!
//! A scripted datahub stands in for the server here; the real one, over the real store, is
//! `crates/vike-datahub/tests/datafusion_serve/tick_columns_wire.rs`, which holds the columns to the JSON reply
//! at every edge and the old client's JSON to the new server.

use std::assert_matches;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use vike_data::{HistoryStore, HistoryStoreError, PerpMetricRow, TimestampRange};
use vike_datahub_client::proto::{
    BAR_COLUMNS, BOOK_UPDATE_COLUMNS, BarColumnsBody, ColumnRow, ColumnsMagic, DEPTH_COLUMNS,
    FEATURE_BAR_COLUMNS, FEATURE_SCAN_LIMIT, FEATURE_TICK_COLUMNS, PERP_METRIC_COLUMNS,
    QUOTE_COLUMNS, Request, Response, TRADE_COLUMNS, compress_columns, encode_columns_into,
};
use vike_datahub_client::test_support::{accept_keyless, spawn_fake_peer};
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};
use vike_hub_protocol::frame::{read_frame, write_frame_raw};
use vike_marketdata::{Bar, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_model::StoreKindId;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const FIRST: i64 = 1_700_000_000_000;

fn quotes() -> Vec<TickQuote> {
    (0..7)
        .map(|i| TickQuote {
            timestamp: FIRST + i,
            local_timestamp: FIRST + i + 3,
            bid: 64_000.0 + i as f64 / 3.0,
            ask: 64_000.5 + i as f64 / 3.0,
            bid_size: 0.25,
            ask_size: 0.0,
            symbol: SYMBOL.into(),
        })
        .collect()
}

fn trades() -> Vec<TickTrade> {
    (0..9)
        .map(|i| TickTrade {
            timestamp: FIRST + i / 2,
            local_timestamp: 0,
            price: 64_000.0 + i as f64 * 0.1,
            size: 0.001 * (i + 1) as f64,
            is_buyer_maker: i % 2 == 0,
            symbol: SYMBOL.into(),
        })
        .collect()
}

fn books(price: f64) -> Vec<L2BookUpdate> {
    let level = |k: usize| L2BookLevel::new(price + k as f64 * 0.1, 1.0 + k as f64);
    vec![
        L2BookUpdate {
            timestamp: FIRST,
            local_timestamp: FIRST + 1,
            sequence: 1,
            kind: L2BookUpdateKind::Snapshot,
            tick_size: 0.1,
            bids: (0..50).map(level).collect(),
            asks: (50..90).map(level).collect(),
            symbol: SYMBOL.to_string(),
        },
        L2BookUpdate {
            timestamp: FIRST + 5,
            local_timestamp: 0,
            sequence: 2,
            kind: L2BookUpdateKind::Delta,
            tick_size: 0.1,
            bids: vec![level(3)],
            asks: Vec::new(),
            symbol: SYMBOL.to_string(),
        },
    ]
}

fn perp_metrics() -> Vec<PerpMetricRow> {
    (0..4)
        .map(|i| PerpMetricRow {
            ts: FIRST + i * 3_600_000,
            premium: -1e-5 * i as f64,
            open_interest: (i % 2 == 0).then_some(1e9),
        })
        .collect()
}

/// `rows` as `magic`'s plain body: "the same rows" is these bytes, every field bit for bit.
fn body<R: ColumnRow>(magic: &ColumnsMagic, rows: &[R]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_columns_into(&mut out, magic, rows);
    out
}

/// The reply a columns server gives `request`: its kind's rows from the first page's request (no `start`),
/// none after, as asked (plain or LZ4). `None` for a request that asks no columns.
fn columns_reply(request: &Request) -> Option<Vec<u8>> {
    fn page<R: ColumnRow>(
        magic: &ColumnsMagic,
        start: Option<i64>,
        rows: Vec<R>,
        asked: BarColumnsBody,
    ) -> Vec<u8> {
        let plain = body(magic, if start.is_none() { &rows[..] } else { &[] });
        match asked {
            BarColumnsBody::Plain => plain,
            BarColumnsBody::Lz4 => compress_columns(magic, &plain),
        }
    }
    Some(match request {
        Request::ScanQuotes { start, columns: Some(b), .. } => {
            page(&QUOTE_COLUMNS, *start, quotes(), *b)
        }
        Request::ScanTrades { start, columns: Some(b), .. } => {
            page(&TRADE_COLUMNS, *start, trades(), *b)
        }
        Request::ScanBookUpdates { start, columns: Some(b), .. } => {
            page(&BOOK_UPDATE_COLUMNS, *start, books(100.0), *b)
        }
        Request::ScanDepth { start, columns: Some(b), .. } => {
            page(&DEPTH_COLUMNS, *start, books(200.0), *b)
        }
        Request::ScanPerpMetrics { start, columns: Some(b), .. } => {
            page(&PERP_METRIC_COLUMNS, *start, perp_metrics(), *b)
        }
        _ => return None,
    })
}

/// A scripted datahub advertising `features`, answering every request with `answer`'s raw body (or a JSON
/// refusal), connection after connection, and recording every request.
fn scripted(
    features: Vec<String>,
    answer: impl Fn(&Request) -> Option<Vec<u8>> + Send + 'static,
) -> (String, Arc<Mutex<Vec<Request>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port").to_string();
    let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
    let record = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.expect("accept the client");
            if !accept_keyless(&mut stream, features.clone()) {
                continue;
            }
            while let Ok(request) = read_frame::<_, Request>(&mut stream) {
                record.lock().unwrap().push(request.clone());
                let reply = answer(&request).unwrap_or_else(|| {
                    serde_json::to_vec(&Response::Error("scripted: no such page".into())).unwrap()
                });
                if write_frame_raw(&mut stream, &reply).is_err() {
                    break;
                }
            }
        }
    });
    (addr, seen)
}

/// Every kind read through `store`, each as its plain body (the bytes are the rows).
fn read_every_kind(store: &HistoryStoreDatahub) -> [Vec<u8>; 5] {
    let all = TimestampRange::all();
    [
        body(&QUOTE_COLUMNS, &store.scan_quotes(VENUE, SYMBOL, all).expect("quotes")),
        body(&TRADE_COLUMNS, &store.scan_trades(VENUE, SYMBOL, all).expect("trades")),
        body(&BOOK_UPDATE_COLUMNS, &store.scan_book_updates(VENUE, SYMBOL, all).expect("book")),
        body(&DEPTH_COLUMNS, &store.scan_depth(VENUE, SYMBOL, all).expect("depth")),
        body(&PERP_METRIC_COLUMNS, &store.scan_perp_metrics(VENUE, SYMBOL, all).expect("perp")),
    ]
}

fn expected() -> [Vec<u8>; 5] {
    [
        body(&QUOTE_COLUMNS, &quotes()),
        body(&TRADE_COLUMNS, &trades()),
        body(&BOOK_UPDATE_COLUMNS, &books(100.0)),
        body(&DEPTH_COLUMNS, &books(200.0)),
        body(&PERP_METRIC_COLUMNS, &perp_metrics()),
    ]
}

/// What each request asked: its `limit` and its `columns`.
fn asked(request: &Request) -> (Option<u32>, Option<BarColumnsBody>) {
    match request {
        Request::ScanQuotes { limit, columns, .. }
        | Request::ScanTrades { limit, columns, .. }
        | Request::ScanBookUpdates { limit, columns, .. }
        | Request::ScanDepth { limit, columns, .. }
        | Request::ScanPerpMetrics { limit, columns, .. } => (*limit, *columns),
        other => panic!("not a tick-level scan: {other:?}"),
    }
}

#[test]
fn a_columns_server_is_asked_for_every_kind_as_columns_in_big_pages() {
    let features = vec![FEATURE_SCAN_LIMIT.to_string(), FEATURE_TICK_COLUMNS.to_string()];
    for (handle, page, columns) in [
        (false, HistoryStoreDatahub::BAR_PAGE_ROWS, BarColumnsBody::Plain),
        (true, HistoryStoreDatahub::THIN_LINK_BAR_PAGE_ROWS, BarColumnsBody::Lz4),
    ] {
        let (addr, seen) = scripted(features.clone(), columns_reply);
        let store = HistoryStoreDatahub::new(addr);
        let store = if handle { store.with_compressed_bars() } else { store };
        assert!(read_every_kind(&store) == expected(), "{columns:?}: every kind's rows");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 10, "{columns:?}: one page and the empty one, five kinds");
        assert!(
            seen.iter().all(|r| asked(r) == (Some(page), Some(columns))),
            "{columns:?}: every page asked for columns at {page}: {seen:?}"
        );
    }
}

/// A datahub from before the tick columns: every kind is asked for JSON at the old page and read as ever,
/// and the client's own columns read is refused locally, sending nothing.
#[test]
fn a_server_without_the_tick_columns_is_asked_for_json() {
    let json = |r: &Request| {
        let first = |start: &Option<i64>| start.is_none();
        Some(match r {
            Request::ScanQuotes { start, .. } => {
                Response::Quotes(if first(start) { quotes() } else { Vec::new() })
            }
            Request::ScanTrades { start, .. } => {
                Response::Trades(if first(start) { trades() } else { Vec::new() })
            }
            Request::ScanBookUpdates { start, .. } => {
                Response::BookUpdates(if first(start) { books(100.0) } else { Vec::new() })
            }
            Request::ScanDepth { start, .. } => {
                Response::Depth(if first(start) { books(200.0) } else { Vec::new() })
            }
            Request::ScanPerpMetrics { start, .. } => {
                Response::PerpMetrics(if first(start) { perp_metrics() } else { Vec::new() })
            }
            _ => return None,
        })
    };
    // The bar columns do not stand for the tick ones: a server may answer one and predate the other.
    let older = vec![
        FEATURE_SCAN_LIMIT.to_string(),
        vike_datahub_client::proto::FEATURE_BAR_COLUMNS.to_string(),
    ];
    let (addr, received, _) = spawn_fake_peer(older.clone(), json);
    {
        let store = HistoryStoreDatahub::new(addr.to_string());
        assert!(read_every_kind(&store) == expected(), "every kind's rows, through JSON");
    }
    let sent = received.recv().expect("the peer reports what arrived");
    assert_eq!(sent.len(), 10, "one page and the empty one, five kinds");
    assert!(
        sent.iter().all(|r| asked(r) == (Some(10_000), None)),
        "JSON at the old page: {sent:?}"
    );

    let (addr, received, _) = spawn_fake_peer(older, json);
    {
        let mut client = DatahubClient::connect(addr).expect("dial");
        let scan = Request::ScanTrades {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            start: None,
            end: None,
            limit: Some(5),
            columns: Some(BarColumnsBody::Plain),
        };
        let mut rows: Vec<TickTrade> = Vec::new();
        let refused = client.scan_columns_into(&scan, &mut rows).expect_err("refused locally");
        assert!(refused.contains(FEATURE_TICK_COLUMNS), "{refused}");
    }
    assert!(received.recv().unwrap().is_empty(), "nothing was sent");
}

/// A columns reply that does not decode, or that comes under another reply's magic, is an error from the
/// read, never fewer or other rows.
#[test]
fn a_malformed_or_crossed_tick_reply_is_refused() {
    let features = vec![FEATURE_SCAN_LIMIT.to_string(), FEATURE_TICK_COLUMNS.to_string()];
    let (addr, _) = scripted(features, |r| {
        Some(match r {
            Request::ScanQuotes { .. } => {
                let good = body(&QUOTE_COLUMNS, &quotes());
                good[..good.len() - 1].to_vec()
            }
            // The depth lane's magic on a book read: the very rows, the wrong reply.
            Request::ScanBookUpdates { .. } => body(&DEPTH_COLUMNS, &books(100.0)),
            _ => return None,
        })
    });
    let store = HistoryStoreDatahub::new(addr);
    let all = TimestampRange::all();
    assert_matches!(
        store.scan_quotes(VENUE, SYMBOL, all),
        Err(HistoryStoreError::Query(why)) if why.contains("malformed quote-columns reply")
    );
    assert_matches!(
        store.scan_book_updates(VENUE, SYMBOL, all),
        Err(HistoryStoreError::Query(why)) if why.contains("malformed book-update-columns reply")
    );
}

fn bars() -> Vec<Bar> {
    (0..5).map(|i| Bar::ohlcv(FIRST + i * 60_000, 1.0, 2.0, 0.5, 1.5, i as f64)).collect()
}

/// **A keeper is handed the pages as the server sent them** (`HistoryStore::read_columns`): each bar and tick read of
/// a columns server comes back as the server's PLAIN body (an LZ4 one decompressed, from a thin-link handle), whole,
/// asked as the typed read asks it and ending on its empty page, with nothing decoded. A body under another reply's
/// magic, a head stating rows no body holds, or a row the decode would refuse, refuses the read where it arrives; a
/// server without the columns is not asked (`None`, nothing sent), so the keeper reads the rows.
#[test]
fn a_keeper_is_handed_the_pages_as_the_server_sent_them() {
    let features =
        [FEATURE_SCAN_LIMIT, FEATURE_TICK_COLUMNS, FEATURE_BAR_COLUMNS].map(String::from);
    let with_bars = |r: &Request| match r {
        Request::LoadBars { start, columns: Some(b), .. } => {
            let rows = if start.is_none() { bars() } else { Vec::new() };
            let plain = body(&BAR_COLUMNS, &rows);
            Some(if *b == BarColumnsBody::Lz4 {
                compress_columns(&BAR_COLUMNS, &plain)
            } else {
                plain
            })
        }
        other => columns_reply(other),
    };
    for (handle, page, columns) in [
        (false, HistoryStoreDatahub::BAR_PAGE_ROWS, BarColumnsBody::Plain),
        (true, HistoryStoreDatahub::THIN_LINK_BAR_PAGE_ROWS, BarColumnsBody::Lz4),
    ] {
        let (addr, seen) = scripted(features.to_vec(), with_bars);
        let store = HistoryStoreDatahub::new(addr);
        let store = if handle { store.with_compressed_bars() } else { store };
        let all = TimestampRange::all();
        for (kind, interval, plain) in [
            (StoreKindId::Bar, Some("1m"), body(&BAR_COLUMNS, &bars())),
            (StoreKindId::Quote, None, body(&QUOTE_COLUMNS, &quotes())),
            (StoreKindId::Trade, None, body(&TRADE_COLUMNS, &trades())),
            (StoreKindId::Book, None, body(&BOOK_UPDATE_COLUMNS, &books(100.0))),
        ] {
            let read = store.read_columns(kind, VENUE, None, SYMBOL, interval, all);
            let read = read.expect("a columns server's pages").expect("read");
            let pages: Vec<_> = read.runs().collect();
            let rows = vike_datahub_client::proto::columns_rows(&plain).unwrap();
            assert_eq!(
                pages,
                [(plain.as_slice(), 0..rows)],
                "{columns:?} {kind:?}: the body, whole"
            );
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 8, "{columns:?}: one page and the empty one, four reads");
        let tick_pages = seen.iter().filter(|r| !matches!(r, Request::LoadBars { .. }));
        assert!(
            tick_pages.clone().all(|r| asked(r) == (Some(page), Some(columns))),
            "{columns:?}: asked as the typed read: {seen:?}"
        );
        assert_eq!(tick_pages.count(), 6);
    }

    // Hostile pages are refused HERE, where they arrive, in the decoder's words: no reader of the answer (the cache, or
    // the sandbox relay reading this handle with the cache off) meets a row count or a row the decode would refuse.
    let (addr, _) = scripted(features.to_vec(), |r| {
        Some(match r {
            // A quote body on a trade read.
            Request::ScanTrades { .. } => body(&QUOTE_COLUMNS, &quotes()),
            // A head stating `u32::MAX` rows: a reader sizing by it would reserve hundreds of GB.
            Request::ScanQuotes { .. } => {
                let mut page = body(&QUOTE_COLUMNS, &quotes());
                page[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
                page
            }
            // A book kind no event has: only a decode of the row (or the check that walks it) sees it.
            Request::ScanBookUpdates { .. } => {
                let rows = books(100.0);
                let mut page = body(&BOOK_UPDATE_COLUMNS, &rows);
                page[8 + 8 * 6 * rows.len()] = 5;
                page
            }
            _ => return None,
        })
    });
    let store = HistoryStoreDatahub::new(addr);
    let read = |kind| store.read_columns(kind, VENUE, None, SYMBOL, None, TimestampRange::all());
    for (kind, words) in [
        (StoreKindId::Trade, "trade-columns magic"),
        (StoreKindId::Quote, "malformed quote-columns reply"),
        (StoreKindId::Book, "a book kind no event has"),
    ] {
        assert_matches!(
            read(kind),
            Some(Err(HistoryStoreError::Query(why))) if why.contains(words),
            "{kind:?}"
        );
    }
    assert!(read(StoreKindId::Depth).is_none(), "a read this does not keep");

    let older = vec![FEATURE_SCAN_LIMIT.to_string()];
    let (addr, received, _) = spawn_fake_peer(older, |_| None);
    {
        let store = HistoryStoreDatahub::new(addr.to_string());
        let all = TimestampRange::all();
        for (kind, interval) in [(StoreKindId::Bar, Some("1m")), (StoreKindId::Trade, None)] {
            assert!(store.read_columns(kind, VENUE, None, SYMBOL, interval, all).is_none());
        }
    }
    assert!(received.recv().unwrap().is_empty(), "nothing was sent");
}
