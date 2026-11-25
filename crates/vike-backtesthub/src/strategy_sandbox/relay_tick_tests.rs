//! A TICK read answered from the daemon's tick cache (`crate::bar_cache`), against the answer it was before: the store's
//! own rows as the count and the tick-columns parts of `TICKS_PER_FRAME` rows, a part over the frame cap halved (the
//! relay's `send_ticks` before the cache, kept here as `old_send_ticks`), byte for byte; and (measured) a binance
//! BTCUSDT.P trade window behind a real datahub, the cache off, missing and hit.

use std::cell::RefCell;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_data::{BarDay, HistoryStore, HistoryStoreError, SharedColumns, TimestampRange};
use vike_datahub_client::proto::{
    BOOK_UPDATE_COLUMNS, ColumnRow, ColumnsMagic, QUOTE_COLUMNS, TRADE_COLUMNS, encode_columns_into,
};
use vike_hub_protocol::child_pipe::{read_message, write_message};
use vike_marketdata::{Bar, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;

use crate::bar_cache::CachedHistoryStore;

use super::super::super::wire::{
    ReadAnswerFrame, StoreRead, TICKS_PER_FRAME, columns_part_into, tick_part_into,
};
use super::super::answer;
use super::cached::{Pipe, Served, cpu_ms, one_serve, status_kib};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
/// The day the cache's clock is in: every stored day is past.
const TODAY: i64 = 1_000;

fn day_of(timestamp: i64) -> i64 {
    timestamp.div_euclid(MS_PER_DAY)
}

/// `range` widened to the whole UTC days it touches.
fn whole_days(range: TimestampRange) -> TimestampRange {
    TimestampRange {
        start: range.start.map(|s| day_of(s) * MS_PER_DAY),
        end: range.end.map(|e| (day_of(e) + 1) * MS_PER_DAY - 1),
    }
}

/// `rows` (ascending by `ts`) within `range`.
fn within<T>(rows: &[T], ts: fn(&T) -> i64, range: TimestampRange) -> &[T] {
    let from = rows.partition_point(|row| range.start.is_some_and(|s| ts(row) < s));
    let to = rows.partition_point(|row| range.end.is_none_or(|e| ts(row) <= e));
    &rows[from..to.max(from)]
}

/// The stamps the tick cache keeps days by (the datahub's `SeriesDays` of a tick kind): every day of the range's whole days that
/// holds a row, stamped once and settled.
fn stamped<T>(rows: &[T], ts: fn(&T) -> i64, range: TimestampRange) -> Vec<BarDay> {
    let mut days: Vec<BarDay> = Vec::new();
    for row in within(rows, ts, whole_days(range)) {
        let day = day_of(ts(row));
        if days.last().is_none_or(|last| last.day != day) {
            days.push(BarDay { day, stamp: 1, provisional: false });
        }
    }
    days
}

/// A store of ticks, each kind ascending, with its stamps; its tick reads are counted. With `page` set it hands a
/// read over as columns pages of that many rows (`read_columns`, the datahub's handle standing in), as the cache keeps
/// them; 0 answers rows only.
struct Ticks {
    quotes: Vec<TickQuote>,
    trades: Vec<TickTrade>,
    books: Vec<L2BookUpdate>,
    reads: AtomicUsize,
    page: usize,
}

/// `rows` as columns pages of `page` rows, each a `magic` body of exactly them.
pub(super) fn pages_of<R: ColumnRow>(
    magic: &ColumnsMagic,
    rows: &[R],
    page: usize,
) -> SharedColumns {
    let mut out = SharedColumns::default();
    for chunk in rows.chunks(page) {
        let mut body = Vec::new();
        encode_columns_into(&mut body, magic, chunk);
        out.push(Arc::new(body), 0..chunk.len());
    }
    out
}

impl HistoryStore for Ticks {
    fn load_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        Ok(Vec::new())
    }
    fn scan_quotes(
        &self,
        _venue: &str,
        _symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(within(&self.quotes, |q| q.timestamp, range).to_vec())
    }
    fn scan_trades(
        &self,
        _venue: &str,
        _symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(within(&self.trades, |t| t.timestamp, range).to_vec())
    }
    fn scan_book_updates(
        &self,
        _venue: &str,
        _symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(within(&self.books, |b| b.timestamp, range).to_vec())
    }
    fn series_days(
        &self,
        kind: StoreKindId,
        _venue: &str,
        _source: Option<&str>,
        _symbol: &str,
        _interval: Option<&str>,
        range: TimestampRange,
    ) -> Result<Option<Vec<BarDay>>, HistoryStoreError> {
        Ok(Some(match kind {
            StoreKindId::Quote => stamped(&self.quotes, |q| q.timestamp, range),
            StoreKindId::Trade => stamped(&self.trades, |t| t.timestamp, range),
            StoreKindId::Book => stamped(&self.books, |b| b.timestamp, range),
            other => return Err(HistoryStoreError::Query(format!("no tick read of {other}"))),
        }))
    }
    fn read_columns(
        &self,
        kind: StoreKindId,
        _venue: &str,
        _source: Option<&str>,
        _symbol: &str,
        _interval: Option<&str>,
        range: TimestampRange,
    ) -> Option<Result<SharedColumns, HistoryStoreError>> {
        if self.page == 0 {
            return None;
        }
        self.reads.fetch_add(1, Ordering::Relaxed);
        let page = self.page;
        Some(Ok(match kind {
            StoreKindId::Quote => {
                pages_of(&QUOTE_COLUMNS, within(&self.quotes, |q| q.timestamp, range), page)
            }
            StoreKindId::Trade => {
                pages_of(&TRADE_COLUMNS, within(&self.trades, |t| t.timestamp, range), page)
            }
            StoreKindId::Book => {
                pages_of(&BOOK_UPDATE_COLUMNS, within(&self.books, |b| b.timestamp, range), page)
            }
            _ => return None,
        }))
    }
    vike_data::hist_store_stubs!(inert: writes, scan_symbol_properties, scan_equity,
        scan_exec_fills, scan_exec_orders);
}

/// A book event of `levels` levels a side.
fn book(timestamp: i64, sequence: u64, levels: usize) -> L2BookUpdate {
    let side = |sign: f64| -> Vec<L2BookLevel> {
        (0..levels).map(|n| L2BookLevel::new(60_000.0 + sign * (n as f64 + 0.5), 1.25)).collect()
    };
    L2BookUpdate {
        timestamp,
        local_timestamp: timestamp + 2,
        sequence,
        kind: if sequence.is_multiple_of(9) {
            L2BookUpdateKind::Snapshot
        } else {
            L2BookUpdateKind::Delta
        },
        tick_size: 0.5,
        bids: side(-1.0),
        asks: side(1.0),
        symbol: SYMBOL.to_string(),
    }
}

/// Forty days of ticks but a three-day gap (days 17 to 19): timestamps in pairs (equal timestamps within a day), a NaN
/// bid, a NaN price and a negative zero now and then, and a symbol left empty on some rows; handed over in pages of
/// `page` rows (0: rows only).
fn planted(page: usize) -> Ticks {
    let days = (0..40_i64).filter(|day| !(17..20).contains(day));
    let (mut quotes, mut trades, mut books) = (Vec::new(), Vec::new(), Vec::new());
    for day in days {
        let at = |i: i64| day * MS_PER_DAY + i / 2 * 1_000;
        let symbol = |i: i64| if i % 97 == 3 { String::new() } else { SYMBOL.to_string() };
        quotes.extend((0..700).map(|i| TickQuote {
            timestamp: at(i),
            local_timestamp: at(i) + 1,
            bid: if i % 211 == 7 { f64::NAN } else { 60_000.0 + (i % 89) as f64 / 4.0 },
            ask: 60_001.0 + (i % 89) as f64 / 4.0,
            bid_size: if i % 101 == 9 { -0.0 } else { 1.5 },
            ask_size: 2.5,
            symbol: symbol(i).into(),
        }));
        trades.extend((0..900).map(|i| TickTrade {
            timestamp: at(i),
            local_timestamp: at(i) + 2,
            price: if i % 307 == 11 { f64::NAN } else { 60_000.0 + (i % 97) as f64 / 8.0 },
            size: 0.001 * (1 + i % 13) as f64,
            is_buyer_maker: i % 3 == 0,
            symbol: symbol(i).into(),
        }));
        books.extend((0..60).map(|i| book(at(i * 5), (day * 60 + i) as u64 + 1, 3)));
    }
    Ticks { quotes, trades, books, reads: AtomicUsize::new(0), page }
}

/// The tick reads a run makes of `range`.
fn reads(range: TimestampRange) -> [StoreRead; 3] {
    let (venue, symbol) = (VENUE.to_string(), SYMBOL.to_string());
    [
        StoreRead::ScanQuotes { venue: venue.clone(), symbol: symbol.clone(), range },
        StoreRead::ScanTrades { venue: venue.clone(), symbol: symbol.clone(), range },
        StoreRead::ScanBookUpdates { venue, symbol, range },
    ]
}

/// The relay's tick answer as it was written before it was encoded from shared runs (`send_ticks` as of
/// `perf/sandbox-tick-columns`' `321009cb0f`): the count, then `magic` columns parts of `TICKS_PER_FRAME` rows, each
/// encoded from its own slice, a part over the cap halved, then `Done`.
fn old_send_ticks<R: ColumnRow>(
    out: &mut impl Write,
    magic: &ColumnsMagic,
    answer: Result<Vec<R>, HistoryStoreError>,
) -> io::Result<()> {
    let rows = match answer {
        Ok(rows) => rows,
        Err(e) => return write_message(out, &ReadAnswerFrame::<()>::Failed(e)),
    };
    write_message(out, &ReadAnswerFrame::<()>::Count(rows.len()))?;
    let (mut frame, mut at, mut size) = (Vec::new(), 0, TICKS_PER_FRAME);
    while at < rows.len() {
        let end = (at + size).min(rows.len());
        match tick_part_into(&mut frame, magic, &rows[at..end]) {
            Ok(()) => {
                out.write_all(&frame)?;
                at = end;
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidData && end - at > 1 => {
                size = (end - at) / 2;
            }
            Err(e) => return Err(e),
        }
    }
    write_message(out, &ReadAnswerFrame::<()>::Done)
}

/// What the relay wrote before for `read`: the store's own answer through [`old_send_ticks`].
fn old_frames(store: &Ticks, read: &StoreRead) -> Vec<u8> {
    let mut frames = Vec::new();
    match read {
        StoreRead::ScanQuotes { venue, symbol, range } => {
            let answer = store.scan_quotes(venue, symbol, *range);
            old_send_ticks(&mut frames, &QUOTE_COLUMNS, answer)
        }
        StoreRead::ScanTrades { venue, symbol, range } => {
            let answer = store.scan_trades(venue, symbol, *range);
            old_send_ticks(&mut frames, &TRADE_COLUMNS, answer)
        }
        StoreRead::ScanBookUpdates { venue, symbol, range } => {
            let answer = store.scan_book_updates(venue, symbol, *range);
            old_send_ticks(&mut frames, &BOOK_UPDATE_COLUMNS, answer)
        }
        other => unreachable!("not a tick read: {other:?}"),
    }
    .expect("encode");
    frames
}

/// The daemon's tick cache over `store`, its clock past every stored day.
fn cache(store: &Arc<Ticks>) -> CachedHistoryStore {
    CachedHistoryStore::new(store.clone(), 0, 1_024, Box::new(|| TODAY * MS_PER_DAY))
}

/// Each read of `windows`, first (the cache fetches and keeps the days) and again (a hit, which reads no store, unless
/// the window lists no day), crosses in the frames the store's own answer was encoded into before.
fn crosses_in_the_old_frames(store: &Arc<Ticks>, windows: &[(&str, TimestampRange, bool)]) {
    for &(what, range, kept) in windows {
        let cached = cache(store);
        for read in reads(range) {
            let old = old_frames(store, &read);
            for pass in ["first read", "again"] {
                let before = store.reads.load(Ordering::Relaxed);
                let mut new = Vec::new();
                answer(&mut new, &cached, read.clone()).expect("answer");
                assert!(
                    new == old,
                    "{what}, {pass}, {read:?}: {} bytes, not {}",
                    new.len(),
                    old.len()
                );
                if pass == "again" && kept {
                    assert_eq!(
                        store.reads.load(Ordering::Relaxed),
                        before,
                        "{what}: a hit reads no store"
                    );
                }
            }
        }
        assert_eq!(cached.kept_tick_bytes() > 0, kept, "{what}: days kept");
    }
}

/// **A tick read served from the cache crosses in the frames the store's answer was encoded into, byte for byte**: a
/// window inside one day, one across whole days and the gap (its parts each joined from several days), one cut at both
/// ends, an empty window inside a kept day and one in the gap (which no day lists, so it is read through); the days
/// kept from the store's rows, and kept from its own pages (333 rows each, so pages straddle the day edges).
#[test]
fn a_cached_tick_read_crosses_in_the_frames_of_the_copy() {
    let at = |day: i64, second: i64| day * MS_PER_DAY + second * 1_000;
    for page in [0, 333] {
        crosses_in_the_old_frames(
            &Arc::new(planted(page)),
            &[
                ("inside one day", TimestampRange::of(at(5, 30), at(5, 300)), true),
                ("across whole days", TimestampRange::of(at(2, 0), at(25, 0) - 1), true),
                ("cut at both ends", TimestampRange::of(at(1, 121), at(30, 279)), true),
                ("empty, inside a kept day", TimestampRange::of(at(3, 600), at(3, 700)), true),
                ("empty, in the gap", TimestampRange::of(at(17, 0), at(20, 0) - 1), false),
            ],
        );
    }
}

/// **A part over the frame cap is halved as before, across the kept days**: book events of 300 levels a side (about
/// 9.6 kB of columns each) make one part of three days' 10,500 events, about 100 MB, over the 64 MiB cap, so the relay
/// halves it at the same rows the copied answer was halved at; the days kept from rows and from pages alike.
#[test]
fn an_oversized_tick_part_is_halved_at_the_same_rows() {
    for page in [0, 1_000] {
        let books = (0..3 * 3_500_i64)
            .map(|i| book(i / 3_500 * MS_PER_DAY + i % 3_500 * 20_000, i as u64 + 1, 300));
        let store = Arc::new(Ticks {
            quotes: Vec::new(),
            trades: Vec::new(),
            books: books.collect(),
            reads: AtomicUsize::new(0),
            page,
        });
        let range = TimestampRange::of(0, 3 * MS_PER_DAY - 1);
        let read = reads(range)[2].clone();
        let old = old_frames(&store, &read);
        let (mut frames, mut at) = (0, 0);
        while at < old.len() {
            let len = u32::from_be_bytes(old[at..at + 4].try_into().expect("a length prefix"));
            (frames, at) = (frames + 1, at + 4 + len as usize);
        }
        assert!(
            frames > 3,
            "the count, 10,500 events halved into two parts, and Done: {frames} frames"
        );
        let cached = cache(&store);
        for _ in ["first read", "again"] {
            let mut new = Vec::new();
            answer(&mut new, &cached, read.clone()).expect("answer");
            assert!(new == old, "pages of {page}: {} bytes, not {}", new.len(), old.len());
        }
    }
}

/// **An over-cap book part is refused from its counted length, never built**: 7,200 events of 300 levels a side (about
/// 69 MB of columns, over the 64 MiB cap) kept as one body and asked as one part are refused with `InvalidData` (so the
/// relay halves the part) and leave the reused frame as small as it was; half of them make a part, the bytes their rows
/// encode to.
#[test]
fn an_over_cap_book_part_is_refused_before_it_is_built() {
    let books: Vec<L2BookUpdate> =
        (0..7_200_i64).map(|i| book(i * 1_000, i as u64 + 1, 300)).collect();
    let mut body = Vec::new();
    vike_datahub_client::proto::encode_columns_into(&mut body, &BOOK_UPDATE_COLUMNS, &books);
    let cap = vike_hub_protocol::frame::MAX_FRAME_LEN as usize;
    assert!(body.len() > cap, "guard: one part of them passes the cap");
    let part = |frame: &mut Vec<u8>, rows: usize| {
        let runs = [(body.as_slice(), 0..rows)];
        columns_part_into::<L2BookUpdate>(frame, &BOOK_UPDATE_COLUMNS, &runs)
    };
    let mut frame = Vec::new();
    let refused = part(&mut frame, books.len()).map_err(|e| e.kind());
    assert_eq!(refused, Err(io::ErrorKind::InvalidData), "an over-cap part is refused");
    assert!(frame.capacity() < 1 << 20, "the frame grew to {} bytes", frame.capacity());
    let half = books.len() / 2;
    part(&mut frame, half).expect("half of them fit");
    let mut old = Vec::new();
    tick_part_into(&mut old, &BOOK_UPDATE_COLUMNS, &books[..half]).expect("encoded");
    assert!(frame == old, "the bytes the rows encode to");
}

/// **A hostile page reaching the relay with the cache OFF is refused in the hub**: a scripted datahub answers a trade
/// read with a page whose head states `u32::MAX` rows (a child reserving its rows by the count frame would ask for
/// ~275 GB), and the relay over the bare datahub handle (the daemon's own when both cache budgets are 0) answers the read
/// `Failed` in the codec's words, before any count frame.
#[test]
fn a_hostile_page_with_the_cache_off_is_refused_in_the_hub() {
    use vike_datahub_client::proto::{
        FEATURE_SCAN_LIMIT, FEATURE_TICK_COLUMNS, PROTO_VERSION, Request,
    };
    use vike_hub_protocol::frame::{read_frame, write_frame, write_frame_raw};
    use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
    let trades = planted(0).trades;
    let mut page = Vec::new();
    encode_columns_into(&mut page, &TRADE_COLUMNS, &trades[..3]);
    page[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let addr = listener.local_addr().expect("the bound address").to_string();
    // A key-less datahub that answers every read with the page.
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            if !matches!(
                read_frame::<_, HandshakeFrame>(&mut stream),
                Ok(HandshakeFrame::Hello { .. })
            ) {
                continue;
            }
            let features = [FEATURE_SCAN_LIMIT, FEATURE_TICK_COLUMNS].map(String::from).to_vec();
            let welcome =
                HandshakeReply::Welcome { proto_version: PROTO_VERSION, features, nonce: None };
            if write_frame(&mut stream, &welcome).is_err() {
                continue;
            }
            while read_frame::<_, Request>(&mut stream).is_ok() {
                if write_frame_raw(&mut stream, &page).is_err() {
                    break;
                }
            }
        }
    });
    let remote = vike_datahub_client::HistoryStoreDatahub::new(addr);
    let [_, read, _] = reads(TimestampRange::of(0, MS_PER_DAY - 1));
    let mut out = Vec::new();
    answer(&mut out, &remote, read).expect("an answer written");
    let first: ReadAnswerFrame<()> = read_message(&mut out.as_slice()).expect("a frame");
    let ReadAnswerFrame::Failed(refused) = first else {
        panic!("refused in the hub, not {first:?}");
    };
    let refused = format!("{refused:?}");
    assert!(refused.contains("malformed trade-columns reply"), "{refused}");
}

/// The median and the best of one number over the rounds.
fn spread(runs: &[(Served, Option<u64>)], of: fn(&(Served, Option<u64>)) -> u64) -> (u64, u64) {
    let mut values: Vec<u64> = runs.iter().map(of).collect();
    values.sort_unstable();
    (values[values.len() / 2], values[0])
}

/// **What a 1-day tick window costs the hub, the cache off, missing and hit** ([`measure_a_tick_window`]), three
/// rounds. Run in release, alone.
#[test]
#[ignore = "a measurement: cargo test --release -p vike-backtesthub --lib measure_the_cached_tick_serve -- --ignored --nocapture"]
fn measure_the_cached_tick_serve_of_one_day() {
    measure_a_tick_window(1, 3);
}

/// The same for a 7-day window, one round (a cold serve of it is minutes). Run in release, alone.
#[test]
#[ignore = "a measurement: cargo test --release -p vike-backtesthub --lib measure_the_cached_tick_serve -- --ignored --nocapture"]
fn measure_the_cached_tick_serve_of_seven_days() {
    measure_a_tick_window(7, 1);
}

/// The first day [`measure_a_window`] plants.
const MEASURED_D0: i64 = 20_400;

/// The perpetual the tick windows read.
const PERP: &str = "BTCUSDT.P";

/// binance BTCUSDT.P trades a day at the CI box's rate (4.5 million, the store's mean over 2026-11-12..18).
const TRADES_PER_DAY: i64 = 4_500_000;

/// The bar window's days: 184 days of binance BTCUSDT `1m`, the size of the CI box's reference bar run of 2026-11-19.
const BAR_DAYS: i64 = 184;

/// **What a tick window of `days` costs the hub, the cache off, missing and hit** ([`measure_a_window`]): binance
/// BTCUSDT.P trades at the CI box's rate.
fn measure_a_tick_window(days: i64, rounds: usize) {
    let read = |range: TimestampRange| StoreRead::ScanTrades {
        venue: VENUE.to_string(),
        symbol: PERP.to_string(),
        range,
    };
    let what = format!("{days}-day tick window, {} trades", days * TRADES_PER_DAY);
    let kept = CachedHistoryStore::kept_tick_bytes;
    let rows = days * TRADES_PER_DAY;
    measure_a_window(&what, ("trades", days), rows, read, (0, 8_192), kept, rounds);
}

/// **What a six-month window of `1m` bars costs the hub, the cache off, missing and hit** ([`measure_a_window`]):
/// [`BAR_DAYS`] of binance BTCUSDT `1m`, three rounds. Run in release, alone.
#[test]
#[ignore = "a measurement: cargo test --release -p vike-backtesthub --lib measure_the_cached_bar_window -- --ignored --nocapture"]
fn measure_the_cached_bar_window_of_six_months() {
    let read = |range: TimestampRange| StoreRead::LoadBars {
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        interval: "1m".to_string(),
        range,
    };
    let what = format!("{BAR_DAYS}-day 1m bar window, {} bars", BAR_DAYS * 1_440);
    let kept = CachedHistoryStore::kept_bytes;
    measure_a_window(&what, ("bars", BAR_DAYS), BAR_DAYS * 1_440, read, (8_192, 0), kept, 3);
}

/// What a measured datahub holds: `days` from [`MEASURED_D0`] of `kind`, `trades` ([`TRADES_PER_DAY`] of [`PERP`]) or
/// `bars` (a `1m` bar a minute of [`SYMBOL`]), written a day at a time.
fn plant(store: &vike_store::HistoryStoreDataFusion, kind: &str, days: i64) {
    for day in 0..days {
        let start = (MEASURED_D0 + day) * MS_PER_DAY;
        let key = format!("day:{day}");
        if kind == "trades" {
            let trades: Vec<TickTrade> = (0..TRADES_PER_DAY)
                .map(|i| TickTrade {
                    timestamp: start + i * MS_PER_DAY / TRADES_PER_DAY,
                    local_timestamp: start + i * MS_PER_DAY / TRADES_PER_DAY + 3,
                    price: 90_000.0 + (i % 2_001) as f64 / 10.0,
                    size: 0.001 * (1 + i % 50) as f64,
                    is_buyer_maker: i % 3 == 0,
                    symbol: PERP.into(),
                })
                .collect();
            store.append_trades(VENUE, PERP, &trades, Some(&key)).expect("trades");
        } else {
            let bars: Vec<Bar> = (0..1_440_i64)
                .map(|m| {
                    let open = 90_000.0 + ((day * 1_440 + m) % 2_001) as f64 / 10.0;
                    let bar = Bar::ohlcv(
                        start + m * 60_000,
                        open,
                        open + 5.0,
                        open - 5.0,
                        open + 1.0,
                        3.5,
                    );
                    Bar { symbol: Some(SYMBOL.into()), ..bar }
                })
                .collect();
            store.append_bars(VENUE, SYMBOL, "1m", &bars, Some(&key)).expect("bars");
        }
    }
}

/// **The datahub of a measurement, in a process of its own** (started by [`DatahubProcess::start`], never alone): what
/// to plant comes on stdin as `<kind> <days>` ([`plant`]), into a store of its OWN temp dir, and the address goes out on
/// stdout as `datahub at <addr>`. It serves until its stdin reaches its end (its parent closed it, or died), then
/// removes its store and ends: no store and no datahub outlives the measurement. A run with no such line on stdin (a
/// terminal, or none) does nothing.
#[test]
#[ignore = "the datahub process of measure_a_window, which starts it: never run alone"]
fn serve_a_measured_datahub() {
    use std::io::{BufRead, IsTerminal};
    let stdin = std::io::stdin();
    let mut window = String::new();
    if stdin.is_terminal() || stdin.lock().read_line(&mut window).is_err() {
        return;
    }
    let Some((kind, days)) = window.trim().split_once(' ') else { return };
    let Ok(days) = days.parse::<i64>() else { return };
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = vike_store::HistoryStoreDataFusion::open(dir.path()).expect("the store");
    plant(&store, kind, days);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    println!("datahub at {}", listener.local_addr().expect("the bound address"));
    let served: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    std::thread::spawn(move || {
        let _ = vike_datahub::serve(listener, served);
    });
    let _ = std::io::copy(&mut stdin.lock(), &mut std::io::sink());
    drop(dir);
}

/// A measured datahub running in a process of its own: this test binary again, as [`serve_a_measured_datahub`]. So this
/// process is the HUB alone: its CPU and its memory are the hub's. Dropped, it closes the datahub's stdin and waits for
/// it to remove its store and end (if this process dies instead, the stdin closes all the same).
struct DatahubProcess {
    child: std::process::Child,
    /// The datahub's stdin, held open while it serves: closing it is what ends it.
    stdin: Option<std::process::ChildStdin>,
    addr: String,
}

impl DatahubProcess {
    /// Start one holding `days` of `kind`, and wait until it serves (planting takes a while).
    fn start(kind: &str, days: i64) -> Self {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let exe = std::env::current_exe().expect("this test binary");
        let mut child = Command::new(exe)
            .args(["serve_a_measured_datahub", "--ignored", "--nocapture", "--test-threads=1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the datahub process");
        let mut stdin = child.stdin.take().expect("its stdin");
        writeln!(stdin, "{kind} {days}").expect("what to plant");
        let mut lines = BufReader::new(child.stdout.take().expect("its stdout")).lines();
        // Anywhere in a line: libtest writes `test <name> ... ` with no newline before the test runs, so the address
        // lands at the END of that line (a prefix match waited for ever, on a lane, beside a datahub serving).
        let addr = lines.by_ref().map_while(Result::ok).find_map(|line| {
            line.split_once("datahub at ").map(|(_, addr)| addr.trim().to_string())
        });
        // Whatever it prints after is drained, so it never writes into a closed pipe.
        std::thread::spawn(move || lines.for_each(drop));
        let process = Self { child, stdin: Some(stdin), addr: addr.unwrap_or_default() };
        assert!(!process.addr.is_empty(), "the datahub process named no address");
        process
    }

    /// Its CPU so far, in milliseconds.
    fn cpu_ms(&self) -> Option<u64> {
        cpu_ms(&self.child.id().to_string())
    }
}

impl Drop for DatahubProcess {
    /// Close its stdin, so it removes its store and ends; a minute later it is killed, and says so (its store may then
    /// stay in `TMPDIR`).
    fn drop(&mut self) {
        drop(self.stdin.take());
        for _ in 0..600 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        eprintln!(
            "the measured datahub did not end with its stdin: killed, its store may stay in TMPDIR"
        );
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// **What a window costs the HUB, the cache off, missing and hit**: `window`'s rows (`(kind, days)`, [`plant`]) in a
/// REAL store (`vike_store::HistoryStoreDataFusion`) behind a REAL datahub (`vike_datahub::serve`) in a process of its
/// own ([`DatahubProcess`]), read by `HistoryStoreDatahub` as the daemon reads it; `read` of the window through a cache
/// of `budgets` (bar, tick MiB), `rounds` rounds each way in turn, printed as median / best: the time to the first byte
/// on the pipe and to the last, the hub's CPU (this process's: the cache, the client and the relay) and the datahub's,
/// the hub's peak resident growth, and a digest of the bytes written (the same every way, and to compare across
/// builds). OFF is a read with no cache, MISS a cold cache (it fetches, cuts and keeps the days), HIT the cache the
/// round's miss filled. Then what the filled cache holds: its kept bytes (`kept`), per row and per day, the days a GiB
/// of budget keeps, and the resident set it still holds once the miss's frees are done (glibc may keep freed pages, so
/// this is an upper bound).
fn measure_a_window(
    what: &str,
    (kind, days): (&str, i64),
    rows: i64,
    read: impl FnOnce(TimestampRange) -> StoreRead,
    budgets: (u32, u32),
    kept_of: fn(&CachedHistoryStore) -> usize,
    rounds: usize,
) {
    let datahub = DatahubProcess::start(kind, days);
    println!("planted {what} in a datahub process of its own ({})", datahub.child.id());
    let remote: Arc<dyn HistoryStore + Send + Sync> =
        Arc::new(vike_datahub_client::HistoryStoreDatahub::new(datahub.addr.clone()));
    let clock = move || (MEASURED_D0 + days + 30) * MS_PER_DAY;
    let new_cache =
        || CachedHistoryStore::new(remote.clone(), budgets.0, budgets.1, Box::new(clock));
    let range = TimestampRange::of(MEASURED_D0 * MS_PER_DAY, (MEASURED_D0 + days) * MS_PER_DAY - 1);
    let read = read(range);
    // The cache a miss filled is kept past its serve (so its frees are not in the serve's time) and is the hit's.
    let filled = RefCell::new(None);
    let off = |pipe: &mut Pipe| answer(pipe, remote.as_ref(), read.clone());
    let miss = |pipe: &mut Pipe| {
        let cold = new_cache();
        let served = answer(pipe, &cold, read.clone());
        *filled.borrow_mut() = Some(cold);
        served
    };
    let hit = |pipe: &mut Pipe| {
        let warm = filled.borrow();
        answer(pipe, warm.as_ref().expect("a miss filled the cache"), read.clone())
    };
    // One serve, with the datahub's CPU over it.
    let serve = |how: &dyn Fn(&mut Pipe) -> io::Result<()>| {
        let before = datahub.cpu_ms();
        let served = one_serve(how);
        (served, datahub.cpu_ms().zip(before).map(|(after, before)| after - before))
    };
    let (mut offs, mut misses, mut hits) = (Vec::new(), Vec::new(), Vec::new());
    let (mut kept, mut held_kib) = (0, None);
    for _ in 0..rounds {
        offs.push(serve(&off));
        filled.borrow_mut().take();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let before = status_kib("VmRSS:");
        misses.push(serve(&miss));
        std::thread::sleep(std::time::Duration::from_millis(500));
        held_kib =
            status_kib("VmRSS:").zip(before).map(|(after, before)| after.saturating_sub(before));
        hits.push(serve(&hit));
        kept = filled.borrow().as_ref().map_or(0, kept_of);
    }
    let every = [&offs, &misses, &hits];
    let digests: Vec<u64> =
        every.iter().flat_map(|runs| runs.iter().map(|run| run.0.digest)).collect();
    assert!(digests.iter().all(|d| *d == digests[0]), "the same bytes every way: {digests:x?}");
    let per_day = kept as f64 / days as f64;
    println!(
        "{what}: kept {:.1} MiB = {:.1} B a row, {:.1} MiB a day, {:.1} days a GiB; resident held after the miss \
         {} MiB; the bytes' digest {:016x}",
        kept as f64 / f64::from(1 << 20),
        kept as f64 / rows as f64,
        per_day / f64::from(1 << 20),
        f64::from(1 << 30) / per_day,
        held_kib.map_or_else(|| "?".to_string(), |kib| (kib / 1024).to_string()),
        digests[0],
    );
    for (what, runs) in [("cache off", offs), ("miss", misses), ("hit", hits)] {
        let (first, first_best) = spread(&runs, |run| run.0.first.as_micros() as u64);
        let (last, last_best) = spread(&runs, |run| run.0.last.as_micros() as u64);
        let (hub, hub_best) = spread(&runs, |run| run.0.cpu_ms.unwrap_or(0));
        let (datahub, _) = spread(&runs, |run| run.1.unwrap_or(0));
        let (peak, _) = spread(&runs, |run| run.0.peak_kib.unwrap_or(0));
        let ms = |micros: u64| micros as f64 / 1_000.0;
        println!(
            "  {what}: first byte {:.1} ms (best {:.1}), last byte {:.1} ms (best {:.1}), hub CPU {hub} ms (best \
             {hub_best}), datahub CPU {datahub} ms, {} bytes, hub peak resident growth {} MiB",
            ms(first),
            ms(first_best),
            ms(last),
            ms(last_best),
            runs[0].0.bytes,
            peak / 1024,
        );
    }
}
