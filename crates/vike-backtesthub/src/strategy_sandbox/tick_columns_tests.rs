//! **Tick reads cross to the sandboxed child as COLUMNS** (decision 0281's amendment of 2026-11-19, "Tick columns"):
//! quotes, trades, book updates and depth answer as their count, then the datahub codec's columns parts of the read's
//! own magic, then `Done`, and the child decodes each part straight into ONE `Vec` sized by the count. Held here: the
//! rows the child reads are the parent store's own, bit for bit, and the ones the msgpack `Rows` parts carried before;
//! the frames are what the wire's doc says; a part the child cannot decode is refused in words; a child serving one
//! run keeps no tick frame; and a whole tick run through a child answers what this process answers.

use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use serde::Serialize;
use serde::de::DeserializeOwned;
use vike_backtesthub_client::proto::{Request, Response};
use vike_data::{HistoryStore, HistoryStoreError, TimestampRange};
use vike_datahub_client::proto::{QUOTE_COLUMNS, TRADE_COLUMNS};
use vike_hub_protocol::child_pipe;
use vike_marketdata::{L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_model::{OrderBroker, OrderPlacement, Strategy};
use vike_simulator::OrderBrokerSimulator;

use super::pipe_store::PipeHistStore;
use super::relay;
use super::wire::{
    ChildFrame, ROWS_PER_FRAME, ReadAnswerFrame, StoreRead, TICKS_PER_FRAME, push_tick_part,
    row_count, tick_part_into,
};
use super::{loaded_strategy_files, serve_child};
use crate::backtesthub_server::StudyDirs;
use crate::backtesthub_server::dispatch::answer_here;
use crate::named_run::NamedRunLane;
use crate::studio_run::StrategyFileBuild;

/// The build both ends of the pipe state here (a binary hands in `vike_buildinfo::summary`).
const BUILD: &str = "tick_columns_tests";

const SECOND: i64 = 1_000;
const DAY: i64 = 86_400_000;
/// A UTC midnight, where the tapes open.
const S: i64 = 20_000 * DAY;
const VENUE: &str = "binance";

/// One symbol's recorded tape: its quotes, trades, book events and depth events, each ts-ascending.
#[derive(Default, Clone)]
struct Tape {
    quotes: Vec<TickQuote>,
    trades: Vec<TickTrade>,
    books: Vec<L2BookUpdate>,
    depth: Vec<L2BookUpdate>,
}

/// The hub's store: a tape by symbol (any venue), its reads cut to the range asked, both bounds inclusive. A symbol
/// it does not hold answers nothing, and [`REFUSED`] is refused.
#[derive(Default, Clone)]
struct Ticks(HashMap<String, Tape>);

/// The symbol whose every read [`Ticks`] refuses.
const REFUSED: &str = "REFUSED";

impl Ticks {
    fn read<T: Clone>(
        &self,
        symbol: &str,
        range: TimestampRange,
        lane: impl Fn(&Tape) -> &Vec<T>,
        at: impl Fn(&T) -> i64,
    ) -> Result<Vec<T>, HistoryStoreError> {
        if symbol == REFUSED {
            return Err(HistoryStoreError::Query(format!("the planted refusal of {symbol}")));
        }
        let inside =
            |ts: i64| range.start.is_none_or(|s| ts >= s) && range.end.is_none_or(|e| ts <= e);
        let Some(tape) = self.0.get(symbol) else { return Ok(Vec::new()) };
        Ok(lane(tape).iter().filter(|r| inside(at(r))).cloned().collect())
    }
}

impl HistoryStore for Ticks {
    fn scan_quotes(
        &self,
        _: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.read(symbol, range, |t| &t.quotes, |q| q.timestamp)
    }
    fn scan_trades(
        &self,
        _: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.read(symbol, range, |t| &t.trades, |t| t.timestamp)
    }
    fn scan_book_updates(
        &self,
        _: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.read(symbol, range, |t| &t.books, |b| b.timestamp)
    }
    fn scan_depth(
        &self,
        _: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.read(symbol, range, |t| &t.depth, |b| b.timestamp)
    }
    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_symbol_properties, scan_equity,
        scan_exec_fills, scan_exec_orders);
}

/// `n` book levels from `price` away from the touch, the third quantity of each side NaN when `nan`.
fn levels(n: usize, price: f64, step: f64, nan: bool) -> Vec<L2BookLevel> {
    (0..n)
        .map(|k| {
            let qty = if nan && k == 2 { f64::NAN } else { 1.0 + k as f64 / 4.0 };
            L2BookLevel::new(price + step * k as f64, qty)
        })
        .collect()
}

/// `days` of `symbol`'s tape from [`S`], every awkward value a store can hold: a quote a second and a trade every
/// five, a six-hour GAP on the second day (a stream-health marker before and after it on the book lane), a
/// DUPLICATE timestamp every seventh row, NaN, infinite and negative-zero prices, ABSENT optional fields (no receive
/// stamp, no sizes, the aggressor flag unset, no symbol on every thirteenth row, a status event with no levels), and
/// books of up to three levels a side, one of them a NaN quantity.
fn tape(symbol: &str, days: i64, base: f64) -> Tape {
    let gap = (S + DAY + 6 * 3_600_000)..(S + DAY + 12 * 3_600_000);
    let stamp = |i: i64, every: i64| {
        let ts = S + (i - i64::from(i % 7 == 6)) * every;
        (!gap.contains(&ts)).then_some(ts)
    };
    let named = |i: i64| if i % 13 == 0 { String::new() } else { symbol.to_string() };
    let quotes = (0..days * DAY / SECOND)
        .filter_map(|i| {
            let timestamp = stamp(i, SECOND)?;
            let mid = base + (i % 101) as f64 / 8.0 - (i % 37) as f64 / 3.0;
            Some(TickQuote {
                timestamp,
                local_timestamp: if i % 4 == 0 { 0 } else { timestamp + 2 + i % 5 },
                bid: match i % 1_000 {
                    5 => f64::NAN,
                    11 => -0.0,
                    _ => mid - 0.25,
                },
                ask: if i % 1_000 == 6 { f64::INFINITY } else { mid + 0.25 },
                bid_size: if i % 3 == 0 { 0.0 } else { 1.0 / (i % 9 + 1) as f64 },
                ask_size: if i % 3 == 0 { 0.0 } else { 2.0 / (i % 11 + 1) as f64 },
                symbol: named(i).into(),
            })
        })
        .collect();
    let trades = (0..days * DAY / (5 * SECOND))
        .filter_map(|i| {
            let timestamp = stamp(i, 5 * SECOND)?;
            Some(TickTrade {
                timestamp,
                local_timestamp: if i % 4 == 1 { 0 } else { timestamp + 3 },
                price: if i % 500 == 3 { f64::NAN } else { base + (i % 17) as f64 / 4.0 },
                size: if i % 19 == 0 { -0.0 } else { 0.001 * (i % 23 + 1) as f64 },
                is_buyer_maker: i % 2 == 0,
                symbol: named(i).into(),
            })
        })
        .collect();
    let books: Vec<L2BookUpdate> = (0..days * DAY / (60 * SECOND))
        .filter_map(|i| {
            let timestamp = S + i * 60 * SECOND;
            let marker = if timestamp == gap.start - 60 * SECOND {
                Some(L2BookUpdateKind::GapStart)
            } else if timestamp == gap.end {
                Some(L2BookUpdateKind::LiveResume)
            } else if i % 997 == 500 {
                Some(L2BookUpdateKind::Stale)
            } else {
                None
            };
            if marker.is_none() && gap.contains(&timestamp) {
                return None;
            }
            let anchor = (timestamp - S) % DAY == 0 || timestamp == gap.end + 60 * SECOND;
            let kind = marker.unwrap_or(if anchor {
                L2BookUpdateKind::Snapshot
            } else {
                L2BookUpdateKind::Delta
            });
            let depth = if marker.is_some() { 0 } else { (i % 4) as usize };
            Some(L2BookUpdate {
                timestamp,
                local_timestamp: if i % 5 == 0 { 0 } else { timestamp + 1 },
                sequence: if marker.is_some() { 0 } else { i as u64 + 1 },
                kind,
                tick_size: 0.25,
                bids: levels(depth, base - 0.25, -0.25, i % 11 == 0),
                asks: levels(depth, base + 0.25, 0.25, false),
                symbol: named(i),
            })
        })
        .collect();
    let depth = books.iter().step_by(2).cloned().collect();
    Tape { quotes, trades, books, depth }
}

/// The parent's store of the bit tests: three days of BTCUSDT (several parts of each lane) and one of ETHUSDT.
fn planted() -> Ticks {
    let mut ticks = Ticks::default();
    ticks.0.insert("BTCUSDT".into(), tape("BTCUSDT", 3, 60_000.0));
    ticks.0.insert("ETHUSDT".into(), tape("ETHUSDT", 1, 3_000.0));
    ticks
}

fn quote_bits(q: &TickQuote) -> (i64, i64, [u64; 4], String) {
    let prices = [q.bid, q.ask, q.bid_size, q.ask_size].map(f64::to_bits);
    (q.timestamp, q.local_timestamp, prices, q.symbol.to_string())
}

fn trade_bits(t: &TickTrade) -> (i64, i64, [u64; 2], bool, String) {
    let values = [t.price, t.size].map(f64::to_bits);
    (t.timestamp, t.local_timestamp, values, t.is_buyer_maker, t.symbol.to_string())
}

/// A book event's every field, floats by their bits.
type BookBits = (i64, i64, u64, String, u64, Vec<(u64, u64)>, Vec<(u64, u64)>, String);

fn book_bits(b: &L2BookUpdate) -> BookBits {
    let side = |levels: &[L2BookLevel]| {
        levels.iter().map(|l| (l.price.to_bits(), l.qty.to_bits())).collect::<Vec<_>>()
    };
    let kind = format!("{:?}", b.kind);
    let grid = b.tick_size.to_bits();
    (
        b.timestamp,
        b.local_timestamp,
        b.sequence,
        kind,
        grid,
        side(&b.bids),
        side(&b.asks),
        b.symbol.clone(),
    )
}

/// `rows` as the pipe carried them before 2026-11-19's tick columns: positional msgpack `Rows` parts of
/// [`ROWS_PER_FRAME`] (`child_pipe::write_message`), each read back and appended, as the child's row reader did.
fn through_msgpack<T: Serialize + DeserializeOwned + std::fmt::Debug>(rows: &[T]) -> Vec<T> {
    let mut out = Vec::new();
    for part in rows.chunks(ROWS_PER_FRAME) {
        let mut frame = Vec::new();
        child_pipe::write_message(&mut frame, &ReadAnswerFrame::Rows(part)).expect("a part");
        match child_pipe::read_message(&mut frame.as_slice()).expect("read back") {
            ReadAnswerFrame::Rows(mut rows) => out.append(&mut rows),
            _ => panic!("not a part"),
        }
    }
    out
}

/// The child's store over two OS pipes, opened by `open`, and the parent's relay answering every read from `store`
/// (`relay::answer`), counted.
fn serve(
    store: Ticks,
    open: impl FnOnce(Box<dyn Read + Send>, Box<dyn Write + Send>) -> PipeHistStore,
) -> (PipeHistStore, Arc<AtomicUsize>) {
    let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
    let (child_in, mut to_child) = std::io::pipe().expect("pipe down");
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    thread::spawn(move || {
        while let Ok(ChildFrame::Read(read)) = child_pipe::read_message(&mut from_child) {
            counted.fetch_add(1, Ordering::Relaxed);
            relay::answer(&mut to_child, &store, read).expect("answer");
        }
    });
    (open(Box::new(child_in), Box::new(child_out)), reads)
}

/// **Every tick read crosses bit for bit**: quotes, trades, book updates and depth of two symbols, through the pipe
/// as columns, are the parent store's rows and the rows the msgpack parts carried, field for field (NaN, infinity,
/// `-0.0`, absent fields, duplicates and the gap included), each answer sized once; a window holding no row answers
/// none; a repeat read stays in a child that keeps its answers.
#[test]
fn every_tick_read_crosses_bit_for_bit_as_the_msgpack_parts_carried_it() {
    let store = planted();
    let (pipe, reads) = serve(planted(), PipeHistStore::new);
    let all = TimestampRange::all();
    for symbol in ["BTCUSDT", "ETHUSDT"] {
        let own = store.scan_quotes(VENUE, symbol, all).expect("own");
        let piped = pipe.scan_quotes(VENUE, symbol, all).expect("piped quotes");
        assert!(piped.iter().map(quote_bits).eq(own.iter().map(quote_bits)), "{symbol} quotes");
        let before = through_msgpack(&own);
        assert!(piped.iter().map(quote_bits).eq(before.iter().map(quote_bits)), "{symbol}");
        assert_eq!(piped.capacity(), piped.len(), "{symbol} quotes sized once, by the count");

        let own = store.scan_trades(VENUE, symbol, all).expect("own");
        let piped = pipe.scan_trades(VENUE, symbol, all).expect("piped trades");
        assert!(piped.iter().map(trade_bits).eq(own.iter().map(trade_bits)), "{symbol} trades");
        let before = through_msgpack(&own);
        assert!(piped.iter().map(trade_bits).eq(before.iter().map(trade_bits)), "{symbol}");
        assert_eq!(piped.capacity(), piped.len(), "{symbol} trades sized once");

        for (lane, own, piped) in [
            (
                "books",
                store.scan_book_updates(VENUE, symbol, all),
                pipe.scan_book_updates(VENUE, symbol, all),
            ),
            ("depth", store.scan_depth(VENUE, symbol, all), pipe.scan_depth(VENUE, symbol, all)),
        ] {
            let (own, piped) = (own.expect("own"), piped.expect("piped"));
            assert!(piped.iter().map(book_bits).eq(own.iter().map(book_bits)), "{symbol} {lane}");
            let before = through_msgpack(&own);
            assert!(piped.iter().map(book_bits).eq(before.iter().map(book_bits)), "{symbol}");
            assert_eq!(piped.capacity(), piped.len(), "{symbol} {lane} sized once");
        }
    }
    let window = TimestampRange::of(S + DAY / 2, S + DAY + 8 * 3_600_000);
    let cut = pipe.scan_quotes(VENUE, "BTCUSDT", window).expect("a window into the gap");
    let own = store.scan_quotes(VENUE, "BTCUSDT", window).expect("own");
    assert!(cut.iter().map(quote_bits).eq(own.iter().map(quote_bits)), "a window");
    let before_gap = TimestampRange::of(S - 10 * DAY, S - 1);
    assert!(pipe.scan_trades(VENUE, "BTCUSDT", before_gap).expect("nothing").is_empty());

    let crossed = reads.load(Ordering::Relaxed);
    pipe.scan_quotes(VENUE, "BTCUSDT", all).expect("again");
    assert_eq!(reads.load(Ordering::Relaxed), crossed, "a repeat read stays in a keeping child");

    // The fixture holds what the test claims to cross.
    let btc = &store.0["BTCUSDT"];
    assert!(btc.quotes.len() > 2 * TICKS_PER_FRAME, "several parts");
    assert!(
        btc.quotes.iter().any(|q| q.bid.is_nan()) && btc.quotes.iter().any(|q| q.ask.is_infinite())
    );
    assert!(btc.quotes.iter().any(|q| q.bid.to_bits() == (-0.0f64).to_bits()));
    assert!(btc.quotes.iter().any(|q| q.symbol.is_empty() && q.local_timestamp == 0));
    assert!(btc.quotes.windows(2).any(|w| w[0].timestamp == w[1].timestamp), "duplicates");
    assert!(btc.quotes.windows(2).any(|w| w[1].timestamp - w[0].timestamp > 3_600_000), "a gap");
    assert!(btc.trades.iter().any(|t| t.price.is_nan()));
    assert!(btc.books.iter().any(|b| b.kind == L2BookUpdateKind::GapStart && b.bids.is_empty()));
    assert!(btc.books.iter().any(|b| b.bids.iter().any(|l| l.qty.is_nan())));
}

/// Every answer frame body written into `buffer`, in order.
fn bodies(buffer: &[u8]) -> Vec<Vec<u8>> {
    let mut cursor = std::io::Cursor::new(buffer);
    std::iter::from_fn(|| vike_hub_protocol::frame::read_frame_raw(&mut cursor).ok()).collect()
}

/// **A tick answer on the wire**: ONE count frame, then columns parts of at most [`TICKS_PER_FRAME`] rows under the
/// read's own magic (no msgpack `Rows`), then `Done`; an empty answer is a count of nought and `Done`; a refused read
/// is ONE `Failed`, which reaches the child's verb in the store's words.
#[test]
fn a_tick_answer_is_its_count_then_columns_parts_then_done() {
    let store = planted();
    let all = TimestampRange::all();
    let n = store.scan_quotes(VENUE, "BTCUSDT", all).expect("own").len();
    let mut buffer = Vec::new();
    let read = |symbol: &str| StoreRead::ScanQuotes {
        venue: VENUE.into(),
        symbol: symbol.into(),
        range: all,
    };
    relay::answer(&mut buffer, &store, read("BTCUSDT")).expect("answered");
    let frames = bodies(&buffer);
    assert_eq!(row_count(&frames[0]), Some(n), "the count first");
    let parts = &frames[1..frames.len() - 1];
    assert_eq!(parts.len(), n.div_ceil(TICKS_PER_FRAME));
    assert!(parts.iter().all(|p| p.starts_with(&QUOTE_COLUMNS.plain)), "quote columns parts");
    let done: ReadAnswerFrame<serde::de::IgnoredAny> =
        rmp_serde::from_slice(frames.last().expect("done")).expect("msgpack");
    assert!(matches!(done, ReadAnswerFrame::Done), "{done:?}");

    buffer.clear();
    relay::answer(&mut buffer, &store, read("NOTHING")).expect("answered");
    let frames = bodies(&buffer);
    assert_eq!((frames.len(), row_count(&frames[0])), (2, Some(0)), "a count of nought, then Done");

    buffer.clear();
    relay::answer(&mut buffer, &store, read(REFUSED)).expect("answered");
    let frames = bodies(&buffer);
    let failed: ReadAnswerFrame<serde::de::IgnoredAny> =
        rmp_serde::from_slice(&frames[0]).expect("msgpack");
    assert!(frames.len() == 1 && matches!(failed, ReadAnswerFrame::Failed(_)), "{failed:?}");

    let (pipe, _) = serve(planted(), PipeHistStore::new);
    match pipe.scan_trades(VENUE, REFUSED, all) {
        Err(HistoryStoreError::Query(m)) => assert!(m.contains("planted refusal"), "{m}"),
        other => panic!("the store's refusal must reach the verb: {other:?}"),
    }
}

/// **A tick part that is not exactly one body of its read's magic is refused in words** and appends nothing: another
/// read's part, a cut part, a msgpack `Rows` part. Through the pipe, such a part fails its read once the answer is
/// read through `Done`, and the next read gets its own answer.
#[test]
fn a_tick_part_the_child_cannot_decode_is_refused_and_keeps_the_channel_in_step() {
    let quotes = planted().scan_quotes(VENUE, "BTCUSDT", TimestampRange::all()).expect("own");
    let mut frame = Vec::new();
    tick_part_into(&mut frame, &QUOTE_COLUMNS, &quotes[..100]).expect("a part");
    let body = &frame[4..];
    let mut out: Vec<TickQuote> = Vec::new();
    push_tick_part(body, &QUOTE_COLUMNS, &mut out).expect("its own read's part");
    assert!(out.iter().map(quote_bits).eq(quotes[..100].iter().map(quote_bits)));

    let mut trades: Vec<TickTrade> = Vec::new();
    let refused = push_tick_part(body, &TRADE_COLUMNS, &mut trades).expect_err("another magic");
    assert!(refused.to_string().contains("trade part it cannot decode"), "{refused}");
    let mut rows: Vec<TickQuote> = Vec::new();
    let cut = push_tick_part(&body[..body.len() - 8], &QUOTE_COLUMNS, &mut rows);
    assert!(cut.is_err() && rows.is_empty(), "a cut part appends nothing: {cut:?}");
    let mut msgpack = Vec::new();
    child_pipe::write_message(&mut msgpack, &ReadAnswerFrame::Rows(&quotes[..3])).expect("a part");
    assert!(push_tick_part(&msgpack[4..], &QUOTE_COLUMNS, &mut rows).is_err() && rows.is_empty());

    let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
    let (child_in, mut to_child) = std::io::pipe().expect("pipe down");
    let store = planted();
    thread::spawn(move || {
        let mut first = true;
        while let Ok(ChildFrame::Read(read)) = child_pipe::read_message(&mut from_child) {
            if std::mem::take(&mut first) {
                child_pipe::write_message(&mut to_child, &ReadAnswerFrame::<()>::Count(3))
                    .expect("count");
                let mut part = Vec::new();
                tick_part_into(&mut part, &QUOTE_COLUMNS, &quotes[..3]).expect("part");
                let cut = &part[4..part.len() - 8];
                to_child.write_all(&(cut.len() as u32).to_be_bytes()).expect("length");
                to_child.write_all(cut).expect("cut part");
                child_pipe::write_message(&mut to_child, &ReadAnswerFrame::<()>::Done)
                    .expect("done");
            } else {
                relay::answer(&mut to_child, &store, read).expect("answer");
            }
        }
    });
    let pipe = PipeHistStore::new(Box::new(child_in), Box::new(child_out));
    match pipe.scan_quotes(VENUE, "BTCUSDT", TimestampRange::all()) {
        Err(HistoryStoreError::Io(m)) => assert!(m.contains("cannot decode"), "{m}"),
        other => panic!("a garbled part must fail its read: {other:?}"),
    }
    let again = pipe.scan_trades(VENUE, "ETHUSDT", TimestampRange::all()).expect("the next read");
    assert_eq!(again.len(), planted().0["ETHUSDT"].trades.len());
}

/// The file the run names, and the text the hub read for it: [`TickFlipper`]'s header.
const TEXT: &str = "//! Tick flipper: turns its position every 97th quote.\n//!\n//! ```toml\n\
                    //! [strategy]\n//! runs_on = \"backtest\"\n//! ```\n";

/// The strategy the file stands for, compiled in: a market order every 97th quote, each the other side, so the run
/// trades all along the tape.
struct TickFlipper {
    seen: usize,
    long: bool,
}

impl Strategy<OrderBrokerSimulator> for TickFlipper {
    fn on_quote_tick(&mut self, broker: &mut OrderBrokerSimulator, quote: &TickQuote) {
        self.seen += 1;
        if self.seen.is_multiple_of(97) {
            let side = if self.long { -1 } else { 1 };
            broker.place(&quote.symbol, "", side, 1.0, OrderPlacement::Market, false);
            self.long = !self.long;
        }
    }
}

/// The stand-in a child (or this process) loads in place of the built artifact.
fn the_file() -> StrategyFileBuild {
    Box::new(|_params_toml: &str, _facts: &str| {
        Ok(Box::new(TickFlipper { seen: 0, long: false })
            as Box<dyn Strategy<OrderBrokerSimulator>>)
    })
}

/// Two days of binance BTCUSDT's quotes and trades, replayed tick by tick.
fn tick_profile() -> String {
    format!(
        "[data]\nkind = \"tick\"\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\ninterval = \"1m\"\n\
         from = \"{S}\"\nto = \"{}\"\n\n[engine]\ncash = 1000000.0\nattach_funding = false\n\n\
         [strategy]\nfile = \"other/tick_flipper.rs\"\n",
        S + 2 * DAY - 1
    )
}

/// A clean two-day BTCUSDT tape the run trades on: a quote a second, a trade every five, every row named.
fn tradable() -> Ticks {
    let mut tape = tape("BTCUSDT", 2, 60_000.0);
    tape.quotes.iter_mut().for_each(|q| {
        q.symbol = "BTCUSDT".into();
        if !(q.bid.is_finite() && q.bid > 0.0 && q.ask.is_finite()) {
            (q.bid, q.ask) = (59_990.0, 60_010.0);
        }
    });
    tape.trades.iter_mut().for_each(|t| {
        t.symbol = "BTCUSDT".into();
        t.price = if t.price.is_finite() { t.price } else { 60_000.0 };
    });
    tape.books.clear();
    tape.depth.clear();
    let mut ticks = Ticks::default();
    ticks.0.insert("BTCUSDT".into(), tape);
    ticks
}

/// An answer's text, hashed: the trades, the report and every number in it.
fn digest(response: &Response) -> u64 {
    let mut digest = DefaultHasher::new();
    format!("{response:?}").hash(&mut digest);
    digest.finish()
}

/// **A tick run through a child answers what this process answers**: the profile's tick replay over the store, run
/// in this process (no pipe), and run by `serve_child` with every quote and trade read crossing the pipe as columns,
/// give the same report, digest for digest.
#[test]
fn a_tick_run_through_a_child_answers_what_this_process_answers() {
    let request = Request::RunBacktest(tick_profile());
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(tradable());
    let study = StudyDirs {
        strategies: loaded_strategy_files(
            &request,
            vec![(Ok(the_file()), None)],
            vec![(TEXT.into(), BTreeMap::new())],
        ),
        ..StudyDirs::working_directory()
    };
    let here = answer_here(request.clone(), &store, &study, NamedRunLane::DISARMED, None).0;
    assert!(matches!(&here, Response::Report(_)), "the reference run must run: {here:?}");

    let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
    let (child_in, mut to_child) = std::io::pipe().expect("pipe down");
    let child = thread::spawn(move || {
        let built = vec![(Ok(the_file()), None)];
        let _ = serve_child(Box::new(child_in), Box::new(child_out), BUILD, built);
    });
    let parent = tradable();
    let relayed = relay::relay(
        &mut to_child,
        &mut from_child,
        BUILD,
        &request,
        TEXT,
        &BTreeMap::new(),
        &parent,
        &|| {},
        None,
    );
    drop(to_child);
    child.join().expect("the child's half");
    let piped = relayed.unwrap_or_else(|stopped| panic!("the relay stopped: {}", stopped.why));
    assert_eq!(digest(&piped.response), digest(&here), "the same answer: {:?}", piped.response);
}

/// **What the tick columns cost against the msgpack parts they replaced**, in ONE process: a day and seven days of
/// one symbol's quotes and trades at the sandbox measurement's rate (a quote every 200 ms, a trade a second), each
/// encoded both ways and decoded into one `Vec` the way the child reads it (the msgpack parts appended; the columns
/// parts into rows sized by the count). Bytes, and the median and best of seven rounds of each decode.
#[test]
#[ignore = "a measurement: cargo test -p vike-backtesthub --release --lib -- --ignored --nocapture strategy_sandbox::tick_columns_tests::measure_the_tick_decode"]
fn measure_the_tick_decode() {
    use std::time::{Duration, Instant};

    fn timed(mut run: impl FnMut()) -> (Duration, Duration) {
        let mut took: Vec<Duration> = (0..7)
            .map(|_| {
                let started = Instant::now();
                run();
                started.elapsed()
            })
            .collect();
        took.sort();
        (took[3], took[0])
    }
    fn msgpack_parts<T: Serialize>(rows: &[T]) -> Vec<Vec<u8>> {
        rows.chunks(ROWS_PER_FRAME)
            .map(|part| {
                let mut frame = Vec::new();
                child_pipe::write_message(&mut frame, &ReadAnswerFrame::Rows(part)).expect("part");
                frame.split_off(4)
            })
            .collect()
    }
    fn column_parts<R: vike_datahub_client::proto::ColumnRow>(
        rows: &[R],
        magic: &vike_datahub_client::proto::ColumnsMagic,
    ) -> Vec<Vec<u8>> {
        rows.chunks(TICKS_PER_FRAME)
            .map(|part| {
                let mut frame = Vec::new();
                tick_part_into(&mut frame, magic, part).expect("part");
                frame.split_off(4)
            })
            .collect()
    }
    fn report<R: vike_datahub_client::proto::ColumnRow + Serialize + DeserializeOwned>(
        what: &str,
        rows: &[R],
        magic: &vike_datahub_client::proto::ColumnsMagic,
    ) {
        let (old, new) = (msgpack_parts(rows), column_parts(rows, magic));
        let bytes = |parts: &[Vec<u8>]| parts.iter().map(|p| p.len() + 4).sum::<usize>();
        let (old_median, old_best) = timed(|| {
            let mut out: Vec<R> = Vec::new();
            for body in &old {
                match rmp_serde::from_slice::<ReadAnswerFrame<Vec<R>>>(body).expect("msgpack") {
                    ReadAnswerFrame::Rows(mut part) => out.append(&mut part),
                    _ => panic!("a part"),
                }
            }
            assert_eq!(out.len(), rows.len());
        });
        let (new_median, new_best) = timed(|| {
            let mut out: Vec<R> = Vec::with_capacity(rows.len());
            for body in &new {
                push_tick_part(body, magic, &mut out).expect("columns");
            }
            assert_eq!(out.len(), rows.len());
        });
        println!(
            "{what}: {} rows. msgpack parts {:.1} MB, decode median {old_median:?} / best {old_best:?}; \
             columns parts {:.1} MB, decode median {new_median:?} / best {new_best:?}",
            rows.len(),
            bytes(&old) as f64 / 1e6,
            bytes(&new) as f64 / 1e6,
        );
    }
    for days in [1, 7] {
        let quotes: Vec<TickQuote> = (0..days * DAY / 200)
            .map(|i| TickQuote {
                timestamp: S + i * 200,
                local_timestamp: S + i * 200 + 3,
                bid: 60_000.0 + (i % 1_009) as f64 / 10.0,
                ask: 60_000.1 + (i % 1_009) as f64 / 10.0,
                bid_size: 1.0 + (i % 13) as f64 / 7.0,
                ask_size: 1.0 + (i % 17) as f64 / 7.0,
                symbol: "BTCUSDT".into(),
            })
            .collect();
        let trades: Vec<TickTrade> = (0..days * DAY / SECOND)
            .map(|i| TickTrade {
                timestamp: S + i * SECOND + 1,
                local_timestamp: S + i * SECOND + 4,
                price: 60_000.0 + (i % 1_013) as f64 / 10.0,
                size: 0.001 * (i % 29 + 1) as f64,
                is_buyer_maker: i % 3 == 0,
                symbol: "BTCUSDT".into(),
            })
            .collect();
        report(&format!("{days}-day quotes"), &quotes, &QUOTE_COLUMNS);
        report(&format!("{days}-day trades"), &trades, &TRADE_COLUMNS);
    }
}

/// **A child serving ONE run keeps no tick frame** (`pipe_store::reads_again`): a repeat read crosses the pipe again,
/// bit for bit the first; a child that may read again keeps the frames, and its repeat stays in it.
#[test]
fn a_single_runs_child_keeps_no_tick_frame_and_a_rereading_child_does() {
    let all = TimestampRange::all();
    let single = Request::RunBacktest(tick_profile());
    let (once, once_reads) = serve(planted(), |i, o| PipeHistStore::serving(&single, i, o));
    let (kept, kept_reads) = serve(planted(), |i, o| PipeHistStore::serving(&Request::Ping, i, o));
    let first = once.scan_quotes(VENUE, "BTCUSDT", all).expect("piped");
    let again = once.scan_quotes(VENUE, "BTCUSDT", all).expect("again");
    assert!(again.iter().map(quote_bits).eq(first.iter().map(quote_bits)));
    assert_eq!(once_reads.load(Ordering::Relaxed), 2, "a single run's child kept no tick frame");
    let first = kept.scan_quotes(VENUE, "BTCUSDT", all).expect("piped");
    let again = kept.scan_quotes(VENUE, "BTCUSDT", all).expect("kept");
    assert!(again.iter().map(quote_bits).eq(first.iter().map(quote_bits)));
    assert_eq!(again.capacity(), again.len(), "a repeat read replays the count");
    assert_eq!(kept_reads.load(Ordering::Relaxed), 1, "a rereading child keeps its tick frames");
}
