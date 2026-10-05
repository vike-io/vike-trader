//! **The `Backfill` verb's write-through proof asks for the range's EDGES and never loads its
//! bars** — driven through the REAL verb over a loopback socket, in both halves of what that swap
//! has to be.
//!
//! # What went wrong before this file existed
//!
//! `crates/vike-datahub/src/server/backfill.rs`'s `backfill_verb` reads the requested range back through the
//! served store after the collector returns, to report the range's first and last stored `ts`. It did
//! that with `HistStore::load_bars`, which builds EVERY row of the range — and the range is the
//! CLIENT's. Two decades of 5-second candles is tens of millions of bars, gigabytes, inside a daemon
//! whose memory cap is shared with the market-data and recorder planes: one long request could
//! OOM-kill the process. The read-back asks `HistStore::bar_edges` now.
//!
//! # The two halves
//!
//! 1. [`the_backfill_readback_never_loads_the_range`] — over a double whose `load_bars` PANICS, every
//!    lane of the verb still answers, from the double's `bar_edges`, and the double records the
//!    exact `(venue, symbol, interval, range)` each lane asked about. Values alone could not tell a
//!    bounded read-back from one that quietly loads and then discards the bars; a store that cannot
//!    be loaded can.
//! 2. `real_store::the_verb_reports_the_edges_the_real_store_holds_inside_the_window` — over a real
//!    `DataFusionHist`, `BackfillDone`'s `first_ts`/`last_ts` equal the edges of the store's own
//!    `load_bars` for windows narrower than the data, wider than it, empty, and one bar wide. That is
//!    the "byte-identical for every existing lane" claim, measured at the boundary cases the
//!    happy-path test in `backfill_roundtrip.rs` does not reach.
//!
//! The first half needs no engine and no feature, so it runs in the derived roster lane on every PR;
//! the second is behind `serve-datafusion`, which `ci.yml`'s `hist-datafusion` job runs.

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use vike_data::source::FUNDING_INTERVAL;
use vike_data::{BarEdges, DataError, ExecFillRow, ExecOrderRow, HistStore, TsRange};
use vike_datahub::backfill::{BackfillFn, BackfillLane, BackfillTable};
use vike_datahub::serve_with_backfill;
use vike_datahub_client::DatahubClient;
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

/// One `bar_edges` call as the double saw it: `(venue, symbol, interval, range)`.
type EdgesCall = (String, String, String, TsRange);

fn call(venue: &str, symbol: &str, interval: &str, range: TsRange) -> EdgesCall {
    (venue.to_string(), symbol.to_string(), interval.to_string(), range)
}

// ------------------------------------------------------------------------------------------------
// The double
// ------------------------------------------------------------------------------------------------

/// A `HistStore` that answers a range's EDGES from a script and REFUSES TO BE LOADED.
///
/// ⚠ Its `load_bars` PANICS, and that is the whole instrument: a verb that reads its range back with
/// `load_bars` cannot get an answer out of this store at all, so the tests below can only pass for a
/// verb that asks for the edges. It counts the attempt before it panics, so a panic swallowed
/// somewhere on the way would still leave a nonzero [`Self::loads`] behind.
struct EdgesOnlyStore {
    /// What `bar_edges` answers; `None` makes it FAIL instead.
    edges: Option<BarEdges>,
    /// Every `bar_edges` call, in order.
    asked: Mutex<Vec<EdgesCall>>,
    /// How many times anything tried to `load_bars`.
    loads: AtomicUsize,
}

impl EdgesOnlyStore {
    fn answering(edges: BarEdges) -> Self {
        Self { edges: Some(edges), asked: Mutex::new(Vec::new()), loads: AtomicUsize::new(0) }
    }
    fn failing() -> Self {
        Self { edges: None, asked: Mutex::new(Vec::new()), loads: AtomicUsize::new(0) }
    }
    fn asked(&self) -> Vec<EdgesCall> {
        self.asked.lock().unwrap().clone()
    }
    fn loads(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }
}

impl HistStore for EdgesOnlyStore {
    fn load_bars(&self, _: &str, _: &str, _: &str, _: TsRange) -> Result<Vec<Bar>, DataError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        panic!(
            "EdgesOnlyStore::load_bars was called — the Backfill verb's read-back must ask \
             `bar_edges` for the range's ends and never materialise its bars"
        );
    }

    fn bar_edges(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<BarEdges, DataError> {
        self.asked.lock().unwrap().push(call(venue, symbol, interval, range));
        self.edges.ok_or_else(|| DataError::Query("the edges read failed on purpose".to_string()))
    }

    // ---- inert stubs: this double exists for the read-back and nothing else -------------------
    fn scan_quotes(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<QuoteTick>, DataError> {
        Ok(Vec::new())
    }
    fn scan_trades(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<TradeTick>, DataError> {
        Ok(Vec::new())
    }
    fn append_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[Bar],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_quotes(
        &self,
        _: &str,
        _: &str,
        _: &[QuoteTick],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_trades(
        &self,
        _: &str,
        _: &str,
        _: &[TradeTick],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_book_updates(
        &self,
        _: &str,
        _: &str,
        _: &[BookUpdate],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_book_updates(
        &self,
        _: &str,
        _: &str,
        _: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        Ok(Vec::new())
    }
    fn append_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: &[(i64, SymbolProperties)],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Ok(Vec::new())
    }
    fn append_equity(
        &self,
        _: &str,
        _: &str,
        _: &[EquitySample],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_equity(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<EquitySample>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_fills(
        &self,
        _: &str,
        _: &str,
        _: &[ExecFillRow],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_fills(&self, _: &str, _: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_orders(
        &self,
        _: &str,
        _: &str,
        _: &[ExecOrderRow],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_orders(&self, _: &str, _: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        Ok(Vec::new())
    }
    fn resample_quotes_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TsRange,
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn resample_trades_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TsRange,
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
}

// ------------------------------------------------------------------------------------------------
// Harness
// ------------------------------------------------------------------------------------------------

/// A collector that "writes" `rows` rows and touches nothing — the double holds no data, and the
/// count is what `BackfillDone::rows_written` must carry through unchanged.
fn collector_writing(rows: usize) -> BackfillFn {
    Box::new(move |_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| Ok(rows))
}

/// One entry per LANE the verb has: klines, funding, and Dukascopy's tick-resampled bars. Each
/// reports a different row count, so a reply that crossed two lanes' answers would show.
fn one_of_each_lane() -> BackfillTable {
    BackfillTable::new(vec![("binance".to_string(), collector_writing(3))])
        .with("binance", BackfillLane::Funding, collector_writing(2))
        .with("dukascopy", BackfillLane::TickBars, collector_writing(4))
}

/// Serve `store` + `table` on an ephemeral loopback port. Key-less, like `backfill_roundtrip.rs`.
fn spawn(store: Arc<dyn HistStore + Send + Sync>, table: BackfillTable) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let _ = serve_with_backfill(listener, store, Some(table));
    });
    addr
}

// ------------------------------------------------------------------------------------------------
// Half 1 — the verb never loads
// ------------------------------------------------------------------------------------------------

/// Every lane of the verb answers over a store that cannot be loaded, and reports the edges the
/// store gave — not values it could have derived from the request or from the collector's count.
#[test]
fn the_backfill_readback_never_loads_the_range() {
    // Deliberately unrelated to the requested windows and to every collector count below: an answer
    // built from anything but `bar_edges` cannot land on these.
    let scripted = BarEdges { first_ts: Some(7), last_ts: Some(99), rows: 5 };
    let store = Arc::new(EdgesOnlyStore::answering(scripted));
    let addr = spawn(store.clone(), one_of_each_lane());
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");

    let klines = client
        .backfill("binance", "SYM", "1h", 1_000, 2_000_000)
        .expect("the kline lane answers over a store that cannot load");
    assert_eq!(klines.rows_written, 3, "the collector's own count rides through");
    assert_eq!(klines.first_ts, Some(7), "first_ts is the store's, from bar_edges");
    assert_eq!(klines.last_ts, Some(99), "last_ts is the store's, from bar_edges");

    let funding = client
        .backfill("binance", "SYM", FUNDING_INTERVAL, 0, 10)
        .expect("the funding lane answers over a store that cannot load");
    assert_eq!(funding.rows_written, 2);
    assert_eq!((funding.first_ts, funding.last_ts), (Some(7), Some(99)));

    let ticks = client
        .backfill("dukascopy", "EURUSD", "1m", 0, 119_999)
        .expect("the tick lane answers over a store that cannot load");
    assert_eq!(ticks.rows_written, 4);
    assert_eq!((ticks.first_ts, ticks.last_ts), (Some(7), Some(99)));

    assert_eq!(
        store.asked(),
        vec![
            call("binance", "SYM", "1h", TsRange::of(1_000, 2_000_000)),
            call("binance", "SYM", FUNDING_INTERVAL, TsRange::of(0, 10)),
            call("dukascopy", "EURUSD", "1m", TsRange::of(0, 119_999)),
        ],
        "each lane asks for exactly its own series over exactly the window the client named, once"
    );
    assert_eq!(store.loads(), 0, "nothing tried to materialise the range");
}

/// The wording of a failed read-back is unchanged: it names the series, the rows the collector DID
/// write (they are in the store — the caller must not retry blind) and the cause, and it is a clean
/// error rather than a `BackfillDone`.
#[test]
fn a_failing_edges_read_is_a_clean_error_naming_the_rows_written() {
    let store = Arc::new(EdgesOnlyStore::failing());
    let addr = spawn(store.clone(), one_of_each_lane());
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");

    let err = client
        .backfill("binance", "SYM", "1h", 0, 1_000)
        .expect_err("a failed read-back must not answer BackfillDone");
    assert!(err.contains("backfill binance/SYM@1h"), "names the series: {err}");
    assert!(
        err.contains("collector wrote 3 rows but the read-back failed"),
        "says what was written and that the PROOF failed: {err}"
    );
    assert!(err.contains("the edges read failed on purpose"), "carries the cause: {err}");
    assert_eq!(store.loads(), 0, "and a failed edges read does not fall back to loading");
}

// ------------------------------------------------------------------------------------------------
// Half 2 — the verb over a real store
// ------------------------------------------------------------------------------------------------

#[cfg(feature = "serve-datafusion")]
mod real_store {
    use super::*;
    use vike_data::DataFusionHist;

    const VENUE: &str = "binance";
    const SYMBOL: &str = "READBACKUSDT";
    const INTERVAL: &str = "1h";
    const HOUR_MS: i64 = 3_600_000;

    fn bar(ts: i64) -> Bar {
        Bar {
            ts,
            open: 201.25,
            high: 203.5,
            low: 200.125,
            close: 202.75,
            volume: 11.5,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// A `binance` collector that appends hourly bars 1h..=9h through `store` — the SAME handle the
    /// server serves — under ONE commit key, whatever window it is handed. So the first request
    /// writes nine rows and every later one writes none, while the store keeps holding all nine.
    fn nine_hourly_bars(store: Arc<DataFusionHist>) -> BackfillTable {
        BackfillTable::new(vec![(
            "binance".to_string(),
            Box::new(
                move |symbol: &str,
                      interval: &str,
                      _start: i64,
                      _end: i64,
                      _: &dyn Fn() -> bool| {
                    let bars: Vec<Bar> = (1..=9).map(|h| bar(h * HOUR_MS)).collect();
                    store
                        .append_bars(VENUE, symbol, interval, &bars, Some("fake-collector"))
                        .map_err(|e| e.to_string())
                },
            ),
        )])
    }

    /// The reply the verb owes for `[start, end]`, derived the OLD way: the ends of what the store's
    /// own `load_bars` returns for it.
    fn expected(store: &DataFusionHist, start: i64, end: i64) -> (Option<i64>, Option<i64>) {
        let bars = store.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::of(start, end)).unwrap();
        (bars.first().map(|b| b.ts), bars.last().map(|b| b.ts))
    }

    #[test]
    fn the_verb_reports_the_edges_the_real_store_holds_inside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let addr = spawn(store.clone(), nine_hourly_bars(store.clone()));
        let mut client = DatahubClient::connect(addr).expect("handshake on connect");

        // (window, the ends it must report). The absolute numbers are pinned as well as compared
        // with the load-derived ones, so an implementation and an oracle wrong the same way fail.
        let cases = [
            (
                (3 * HOUR_MS, 5 * HOUR_MS),
                (Some(3 * HOUR_MS), Some(5 * HOUR_MS)),
                "narrower than the data, INCLUSIVE at both ends",
            ),
            (
                (0, 100 * HOUR_MS),
                (Some(HOUR_MS), Some(9 * HOUR_MS)),
                "wider than the data: the store's ends, not the window's",
            ),
            ((20 * HOUR_MS, 30 * HOUR_MS), (None, None), "a window after every bar holds none"),
            (
                (2 * HOUR_MS + 1, 3 * HOUR_MS - 1),
                (None, None),
                "between two bars: the part overlaps the window and holds no row in it",
            ),
            ((4 * HOUR_MS, 4 * HOUR_MS), (Some(4 * HOUR_MS), Some(4 * HOUR_MS)), "one bar wide"),
        ];
        for (i, ((start, end), want, why)) in cases.into_iter().enumerate() {
            let done = client
                .backfill(VENUE, SYMBOL, INTERVAL, start, end)
                .unwrap_or_else(|e| panic!("case {i} ({why}): the verb must answer: {e}"));
            assert_eq!((done.first_ts, done.last_ts), want, "case {i}: {why}");
            assert_eq!(
                (done.first_ts, done.last_ts),
                expected(&store, start, end),
                "case {i}: the reply is the load-derived one — {why}"
            );
            // The collector's count is the FIRST request's nine rows and then nothing (one commit
            // key): the proof reports what the store HOLDS, never what this request wrote.
            assert_eq!(done.rows_written, if i == 0 { 9 } else { 0 }, "case {i}: rows_written");
        }
    }
}
