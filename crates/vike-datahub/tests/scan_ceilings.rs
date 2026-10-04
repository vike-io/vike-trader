//! **Every range read reads only what it answers** — the seven scan verbs and `ScanExecFills`,
//! driven through the REAL verbs over a loopback socket, in the shape
//! `docs/superpowers/specs/2026-10-02-remaining-whole-range-reads-design.md`'s section 5 names for
//! its PR 2. `crates/vike-datahub/tests/load_bars_bounded.rs` is the same file for `LoadBars`, and
//! this one is built the same way.
//!
//! # What went wrong before this file existed
//!
//! `crates/vike-datahub/src/server.rs`'s handler answered `ScanCohort`, `ScanPerpMetrics` and
//! `ScanEquity` by reading the client's WHOLE range and cutting the reply afterwards — so every page
//! of `RemoteHistStore`'s pager decoded the rest of the range — answered the four tick scans with
//! the whole range whenever no `limit` was sent (`vike-cli data hist get` and `export --addr` send
//! none), and answered `ScanExecFills` with the whole series. A reply the frame could not carry
//! failed at `write_frame`, as a dropped connection, after the full read.
//!
//! # The halves
//!
//! 1. **No verb ever reads the range** — over [`BudgetOnlyStore`], whose unbudgeted reads PANIC and
//!    whose budgeted ones record what they were asked. Every reply within the ceiling is held
//!    BYTE-IDENTICAL to the frame the old handler sent, computed here from the planted rows by
//!    [`old_reply`]; a request with no `limit` over more than the ceiling is refused BY NAME and the
//!    NEXT request on the same connection is answered. The ceilings are injected
//!    (`vike_datahub::server::serve_with_read_ceilings`), so no test plants half a million rows, and
//!    [`the_production_entries_serve_the_production_ceilings`] pins that the entries a daemon runs
//!    pass the constants.
//! 2. **The book ceiling counts LEVELS** — the unit the store's budget counts —
//!    [`the_book_ceiling_counts_stored_levels_not_events`].
//! 3. **Each ceiling's arithmetic is measured, not trusted** —
//!    [`the_smallest_real_row_of_every_kind_is_no_shorter_than_its_ceiling_assumes`] serialises the
//!    shortest real row of each kind with serde, and tries the longer spelling of its fields.
//! 4. **A row ceiling is not a frame, so the BYTES are cut too** (the design's PR 3, `LoadBars`
//!    included): a reply under its row ceiling whose rows are wide enough to pass the frame is cut to
//!    a shorter whole-`ts` page when a `limit` was sent and refused by name when none was, a first
//!    timestamp too wide for any page is a named error and NEVER an empty page, and a reply that
//!    fits is untouched. The frame is injected like the ceilings (`ReadCeilings::frame_bytes`), and
//!    every expected cut is computed here by serialising each candidate page whole —
//!    [`longest_page_that_fits`] — rather than by the server's own byte count.
//!
//! None of it needs an engine or a feature, so it runs in the derived roster lane on every PR.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use vike_data::{
    CohortRow, DataError, ExecFillRow, ExecOrderRow, HistStore, PerpMetricRow, TsRange,
};
use vike_datahub::server::{
    MIN_BOOK_LEVEL_JSON_BYTES, MIN_COHORT_JSON_BYTES, MIN_EQUITY_JSON_BYTES,
    MIN_EXEC_FILL_JSON_BYTES, MIN_PERP_METRIC_JSON_BYTES, MIN_QUOTE_JSON_BYTES,
    MIN_TRADE_JSON_BYTES, ReadCeilings, serve_with_read_ceilings,
};
use vike_datahub_client::{Request, Response, read_frame_raw, write_frame};
use vike_model::{
    Bar, BookLevel, BookUpdate, BookUpdateKind, EquitySample, QuoteTick, SymbolProperties,
    TradeTick,
};

const VENUE: &str = "hyperliquid";
const SYMBOL: &str = "BTC";

/// The injected ceiling of every kind — small enough that every arm is reachable with a handful of
/// rows.
const CEILING: usize = 10;

/// The injected row ceilings, under the production frame.
fn injected() -> ReadCeilings {
    injected_with_frame(ReadCeilings::PRODUCTION.frame_bytes)
}

/// The injected row ceilings, under a frame of `frame_bytes`.
fn injected_with_frame(frame_bytes: usize) -> ReadCeilings {
    ReadCeilings {
        bars: CEILING,
        quotes: CEILING,
        trades: CEILING,
        book_levels: CEILING,
        cohort: CEILING,
        perp_metrics: CEILING,
        equity: CEILING,
        exec_fills: CEILING,
        frame_bytes,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// `LoadBars` — the double answers it too, so the byte cap's tests cover the bar verb as well.
    Bars,
    Quotes,
    Trades,
    Book,
    Depth,
    Cohort,
    Perp,
    Equity,
    ExecFills,
}

/// The seven verbs that carry a range and a `limit`.
const RANGED: [Kind; 7] =
    [Kind::Quotes, Kind::Trades, Kind::Book, Kind::Depth, Kind::Cohort, Kind::Perp, Kind::Equity];

/// Every verb this file covers.
const ALL: [Kind; 8] = [
    Kind::Quotes,
    Kind::Trades,
    Kind::Book,
    Kind::Depth,
    Kind::Cohort,
    Kind::Perp,
    Kind::Equity,
    Kind::ExecFills,
];

/// One planted row: its `ts` and a discriminator unique to it.
type Planted = (i64, i64);

/// 26 rows over 20 timestamps, with groups that straddle the caps below: `2_000` three times,
/// `5_000` twice and `10_000` four times, each copy with its own discriminator so a reply that
/// reordered or dropped one is visible — `load_bars_bounded.rs`'s fixture.
fn planted() -> Vec<Planted> {
    let mut rows = Vec::new();
    for k in 1..=20i64 {
        let copies = match k {
            2 => 3,
            5 => 2,
            10 => 4,
            _ => 1,
        };
        for c in 0..copies {
            rows.push((k * 1_000, k * 10 + c));
        }
    }
    assert_eq!(rows.len(), 26);
    rows
}

fn in_range(rows: &[Planted], range: TsRange) -> Vec<Planted> {
    rows.iter()
        .copied()
        .filter(|(ts, _)| {
            range.start.is_none_or(|s| *ts >= s) && range.end.is_none_or(|e| *ts <= e)
        })
        .collect()
}

/// The reply the OLD handler sent for `limit` over `rows` (the range's whole answer): the wire's
/// whole-`ts` cap, written out here from its three cases rather than taken from the server.
fn old_reply(rows: &[Planted], limit: Option<u32>) -> Vec<Planted> {
    let mut rows = rows.to_vec();
    let Some(cap) = limit.map(|l| l as usize).filter(|l| *l > 0) else { return rows };
    if rows.len() <= cap {
        return rows;
    }
    let straddling = rows[cap - 1].0;
    if rows[cap].0 != straddling {
        rows.truncate(cap);
        return rows;
    }
    let first = rows.iter().position(|r| r.0 == straddling).expect("the group is in the rows");
    if first > 0 {
        rows.truncate(first);
    } else {
        let last = rows.iter().rposition(|r| r.0 == straddling).expect("the group is in the rows");
        rows.truncate(last + 1);
    }
    rows
}

// ---- rows of each kind ---------------------------------------------------------------------------

/// The bar series' interval, for `LoadBars`.
const INTERVAL: &str = "5s";

fn bar(&(ts, d): &Planted) -> Bar {
    Bar {
        ts,
        open: d as f64,
        high: d as f64 + 1.0,
        low: d as f64 - 1.0,
        close: d as f64,
        volume: 3.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn quote(&(ts, d): &Planted) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid: d as f64,
        ask: d as f64 + 0.5,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: SYMBOL.into(),
    }
}

fn trade(&(ts, d): &Planted) -> TradeTick {
    TradeTick {
        ts,
        local_ts: 0,
        price: d as f64,
        size: 1.0,
        is_buyer_maker: false,
        symbol: SYMBOL.into(),
    }
}

/// A book event of `levels` bid levels — so it stands for `levels` stored rows.
fn book(&(ts, d): &Planted, levels: usize) -> BookUpdate {
    BookUpdate {
        ts,
        local_ts: 0,
        seq: d as u64,
        kind: BookUpdateKind::Delta,
        tick_size: 0.01,
        bids: (0..levels).map(|i| BookLevel { price: d as f64 - i as f64, qty: 1.0 }).collect(),
        asks: Vec::new(),
        symbol: SYMBOL.into(),
    }
}

fn cohort(&(ts, d): &Planted) -> CohortRow {
    CohortRow {
        ts,
        asset: SYMBOL.into(),
        axis: "size".into(),
        cohort: format!("c{d}"),
        grading: "realized".into(),
        label_basis: "point_in_time".into(),
        long_usd: d as f64,
        total_usd: d as f64 + 1.0,
    }
}

fn perp(&(ts, d): &Planted) -> PerpMetricRow {
    PerpMetricRow { ts, premium: d as f64, open_interest: None }
}

fn equity(&(ts, d): &Planted) -> EquitySample {
    EquitySample {
        ts,
        venue: SYMBOL.into(),
        equity: d as f64,
        realized: 1.0,
        unrealized: 2.0,
        missing_prices: 0,
    }
}

fn fill(&(ts, d): &Planted) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: format!("t{d}"),
        client_order_id: "c1".into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 0.5,
        px: d as f64,
        commission: 0.01,
        mark_price: None,
        liquidity_side: "maker".into(),
        commission_asset: "USDC".into(),
    }
}

impl Kind {
    /// The reply the wire carries for `rows`, book events `levels` deep.
    fn response(self, rows: &[Planted], levels: usize) -> Response {
        match self {
            Kind::Bars => Response::Bars(rows.iter().map(bar).collect()),
            Kind::Quotes => Response::Quotes(rows.iter().map(quote).collect()),
            Kind::Trades => Response::Trades(rows.iter().map(trade).collect()),
            Kind::Book => Response::BookUpdates(rows.iter().map(|r| book(r, levels)).collect()),
            Kind::Depth => Response::Depth(rows.iter().map(|r| book(r, levels)).collect()),
            Kind::Cohort => Response::Cohort(rows.iter().map(cohort).collect()),
            Kind::Perp => Response::PerpMetrics(rows.iter().map(perp).collect()),
            Kind::Equity => Response::Equity(rows.iter().map(equity).collect()),
            Kind::ExecFills => Response::ExecFills(rows.iter().map(fill).collect()),
        }
    }

    fn request(self, range: TsRange, limit: Option<u32>) -> Request {
        let (venue, symbol, start, end) =
            (VENUE.to_string(), SYMBOL.to_string(), range.start, range.end);
        match self {
            Kind::Bars => {
                Request::LoadBars { venue, symbol, interval: INTERVAL.into(), start, end, limit }
            }
            Kind::Quotes => Request::ScanQuotes { venue, symbol, start, end, limit },
            Kind::Trades => Request::ScanTrades { venue, symbol, start, end, limit },
            Kind::Book => Request::ScanBookUpdates { venue, symbol, start, end, limit },
            Kind::Depth => Request::ScanDepth { venue, symbol, start, end, limit },
            Kind::Cohort => Request::ScanCohort { venue, asset: symbol, start, end, limit },
            Kind::Perp => Request::ScanPerpMetrics { venue, symbol, start, end, limit },
            Kind::Equity => Request::ScanEquity { venue, symbol, start, end, limit },
            Kind::ExecFills => Request::ScanExecFills { venue, symbol },
        }
    }

    /// What the refusal calls this verb's rows, and what it names its ceiling.
    fn unit_and_ceiling(self) -> (&'static str, &'static str) {
        match self {
            Kind::Bars => ("bars", "LOAD_BARS_CEILING"),
            Kind::Quotes => ("quotes", "SCAN_QUOTES_CEILING"),
            Kind::Trades => ("trades", "SCAN_TRADES_CEILING"),
            Kind::Book | Kind::Depth => ("book levels", "SCAN_BOOK_LEVELS_CEILING"),
            Kind::Cohort => ("cohort rows", "SCAN_COHORT_CEILING"),
            Kind::Perp => ("perp-metric rows", "SCAN_PERP_METRICS_CEILING"),
            Kind::Equity => ("equity samples", "SCAN_EQUITY_CEILING"),
            Kind::ExecFills => ("fills", "SCAN_EXEC_FILLS_CEILING"),
        }
    }

    fn production_ceiling(self) -> usize {
        let p = ReadCeilings::PRODUCTION;
        match self {
            Kind::Bars => p.bars,
            Kind::Quotes => p.quotes,
            Kind::Trades => p.trades,
            Kind::Book | Kind::Depth => p.book_levels,
            Kind::Cohort => p.cohort,
            Kind::Perp => p.perp_metrics,
            Kind::Equity => p.equity,
            Kind::ExecFills => p.exec_fills,
        }
    }
}

// ------------------------------------------------------------------------------------------------
// The double
// ------------------------------------------------------------------------------------------------

/// A `HistStore` that answers every range read's BUDGETED form from planted rows and REFUSES TO BE
/// READ WHOLE — `load_bars_bounded.rs`'s `HeadOnlyStore`, for the eight verbs here and `LoadBars`.
///
/// ⚠ Every unbudgeted read PANICS — and so does a budgeted read handed `None` or `Some(0)`, which the
/// store contract reads as the whole range. A handler that reads its range gets no answer out of
/// this store at all: its connection thread dies and the client sees the socket drop. It counts the
/// attempt first, so a swallowed panic would still leave a nonzero [`Self::loads`].
///
/// Its budgeted reads honour the contract and no more: a complete prefix holding at least `n` stored
/// rows — and [`OVERSHOOT`] rows more where the range has them, the way a real store answers in
/// whole storage blocks — counting a book event as its `levels` stored rows.
struct BudgetOnlyStore {
    rows: Vec<Planted>,
    /// Bid levels per book/depth event.
    levels: usize,
    /// Every budgeted read: `(verb, range, n)`.
    asked: Mutex<Vec<(Kind, TsRange, usize)>>,
    loads: AtomicUsize,
}

const OVERSHOOT: usize = 3;

/// What [`BudgetOnlyStore`] answers for `n` stored rows of `rows` (a range's rows): a complete
/// prefix holding at least `n` stored rows, plus [`OVERSHOOT`] where there are more — each row
/// standing for `per_row` stored rows.
fn head_of(rows: &[Planted], n: usize, per_row: usize) -> Vec<Planted> {
    let want = n.saturating_add(OVERSHOOT);
    let (mut take, mut stored) = (0usize, 0usize);
    while take < rows.len() && stored < want {
        stored += per_row;
        take += 1;
    }
    while take > 0 && take < rows.len() && rows[take].0 == rows[take - 1].0 {
        take += 1; // complete: never cut inside a timestamp
    }
    rows[..take].to_vec()
}

impl BudgetOnlyStore {
    fn new(rows: Vec<Planted>, levels: usize) -> Self {
        Self { rows, levels, asked: Mutex::new(Vec::new()), loads: AtomicUsize::new(0) }
    }
    fn asked(&self) -> Vec<(Kind, TsRange, usize)> {
        self.asked.lock().unwrap().clone()
    }
    fn loads(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }

    fn whole(&self, what: &str) -> ! {
        self.loads.fetch_add(1, Ordering::SeqCst);
        panic!(
            "BudgetOnlyStore: {what} read the WHOLE range — a range verb must ask its budgeted read \
             for what it can answer and never materialise the client's range"
        );
    }

    /// The contract's head: a complete prefix of `range` holding at least `n` stored rows.
    fn head(
        &self,
        kind: Kind,
        venue: &str,
        symbol: &str,
        range: TsRange,
        n: usize,
    ) -> Vec<Planted> {
        assert_eq!((venue, symbol), (VENUE, SYMBOL), "the asked-for series");
        self.asked.lock().unwrap().push((kind, range, n));
        let per_row = match kind {
            Kind::Book | Kind::Depth => self.levels,
            _ => 1,
        };
        head_of(&in_range(&self.rows, range), n, per_row)
    }

    fn budget(&self, kind: Kind, budget: Option<usize>) -> usize {
        budget
            .filter(|b| *b > 0)
            .unwrap_or_else(|| self.whole(&format!("{kind:?} with budget {budget:?}")))
    }
}

impl HistStore for BudgetOnlyStore {
    // ---- the budgeted reads under test --------------------------------------------------------
    fn load_bars_head(
        &self,
        v: &str,
        s: &str,
        interval: &str,
        r: TsRange,
        n: usize,
    ) -> Result<Vec<Bar>, DataError> {
        assert_eq!(interval, INTERVAL, "the asked-for bar series");
        Ok(self.head(Kind::Bars, v, s, r, n).iter().map(bar).collect())
    }
    fn scan_quotes_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<QuoteTick>, DataError> {
        let n = self.budget(Kind::Quotes, b);
        Ok(self.head(Kind::Quotes, v, s, r, n).iter().map(quote).collect())
    }
    fn scan_trades_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<TradeTick>, DataError> {
        let n = self.budget(Kind::Trades, b);
        Ok(self.head(Kind::Trades, v, s, r, n).iter().map(trade).collect())
    }
    fn scan_book_updates_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<BookUpdate>, DataError> {
        let n = self.budget(Kind::Book, b);
        Ok(self.head(Kind::Book, v, s, r, n).iter().map(|p| book(p, self.levels)).collect())
    }
    fn scan_depth_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<BookUpdate>, DataError> {
        let n = self.budget(Kind::Depth, b);
        Ok(self.head(Kind::Depth, v, s, r, n).iter().map(|p| book(p, self.levels)).collect())
    }
    fn scan_cohort_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<CohortRow>, DataError> {
        let n = self.budget(Kind::Cohort, b);
        Ok(self.head(Kind::Cohort, v, s, r, n).iter().map(cohort).collect())
    }
    fn scan_perp_metrics_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        let n = self.budget(Kind::Perp, b);
        Ok(self.head(Kind::Perp, v, s, r, n).iter().map(perp).collect())
    }
    fn scan_equity_capped(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        b: Option<usize>,
    ) -> Result<Vec<EquitySample>, DataError> {
        let n = self.budget(Kind::Equity, b);
        Ok(self.head(Kind::Equity, v, s, r, n).iter().map(equity).collect())
    }
    fn scan_exec_fills_head(
        &self,
        v: &str,
        s: &str,
        r: TsRange,
        n: usize,
    ) -> Result<Vec<ExecFillRow>, DataError> {
        Ok(self.head(Kind::ExecFills, v, s, r, n).iter().map(fill).collect())
    }

    // ---- the unbudgeted reads: each one is the defect this file exists to catch ----------------
    fn scan_quotes(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<QuoteTick>, DataError> {
        self.whole("scan_quotes")
    }
    fn scan_trades(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<TradeTick>, DataError> {
        self.whole("scan_trades")
    }
    fn scan_book_updates(
        &self,
        _: &str,
        _: &str,
        _: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        self.whole("scan_book_updates")
    }
    fn scan_depth(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<BookUpdate>, DataError> {
        self.whole("scan_depth")
    }
    fn scan_cohort(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<CohortRow>, DataError> {
        self.whole("scan_cohort")
    }
    fn scan_perp_metrics(
        &self,
        _: &str,
        _: &str,
        _: TsRange,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        self.whole("scan_perp_metrics")
    }
    fn scan_equity(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<EquitySample>, DataError> {
        self.whole("scan_equity")
    }
    fn scan_exec_fills(&self, _: &str, _: &str) -> Result<Vec<ExecFillRow>, DataError> {
        self.whole("scan_exec_fills")
    }
    fn load_bars(&self, _: &str, _: &str, _: &str, _: TsRange) -> Result<Vec<Bar>, DataError> {
        self.whole("load_bars")
    }

    // ---- inert stubs: this double exists for the range reads and nothing else ------------------
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
    fn append_exec_fills(
        &self,
        _: &str,
        _: &str,
        _: &[ExecFillRow],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
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

/// Serve `store` on an ephemeral loopback port under the injected ceilings. Key-less.
fn spawn(store: Arc<dyn HistStore + Send + Sync>) -> SocketAddr {
    spawn_with(store, injected())
}

/// [`spawn`] under any ceilings — the byte tests inject a frame too.
fn spawn_with(store: Arc<dyn HistStore + Send + Sync>, ceilings: ReadCeilings) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let _ = serve_with_read_ceilings(listener, store, ceilings);
    });
    addr
}

/// One positional connection, spoken in raw frames so a reply is compared as BYTES.
struct Wire(TcpStream);

impl Wire {
    fn open(addr: SocketAddr) -> Self {
        Self(TcpStream::connect(addr).expect("connect to the loopback server"))
    }
    fn ask(&mut self, request: &Request) -> io::Result<Vec<u8>> {
        write_frame(&mut self.0, request)?;
        read_frame_raw(&mut self.0)
    }
    fn ping(&mut self) -> io::Result<Response> {
        let body = self.ask(&Request::Ping)?;
        Ok(serde_json::from_slice(&body).expect("a Response"))
    }
}

/// The frame body the server writes for `response` — `write_frame`'s own encoding.
fn frame_of(response: &Response) -> Vec<u8> {
    serde_json::to_vec(response).expect("serialize a Response")
}

fn ranges() -> [TsRange; 3] {
    [TsRange::all(), TsRange { start: Some(1_500), end: None }, TsRange::of(4_000, 16_000)]
}

// ------------------------------------------------------------------------------------------------
// Half 1 — no verb reads the range
// ------------------------------------------------------------------------------------------------

/// A `limit` within the ceiling asks the store for exactly that many rows and answers EXACTLY the
/// frame the old handler sent — over limits on a group boundary, inside a straddling group, and
/// inside the FIRST group of a range.
#[test]
fn a_limited_request_reads_a_budget_and_answers_the_old_frame() {
    for kind in RANGED {
        let rows = planted();
        let store = Arc::new(BudgetOnlyStore::new(rows.clone(), 1));
        let mut wire = Wire::open(spawn(store.clone()));
        let mut expected_asks = Vec::new();
        for range in ranges() {
            for limit in 1..=CEILING as u32 {
                let got =
                    wire.ask(&kind.request(range, Some(limit))).expect("a limited scan answers");
                let old = old_reply(&in_range(&rows, range), Some(limit));
                assert_eq!(
                    got,
                    frame_of(&kind.response(&old, 1)),
                    "{kind:?}, range {range:?}, limit {limit}: byte-identical to the old frame"
                );
                expected_asks.push((kind, range, limit as usize));
            }
        }
        assert_eq!(
            store.asked(),
            expected_asks,
            "{kind:?}: each request reads its own limit, once"
        );
        assert_eq!(store.loads(), 0, "{kind:?}: nothing read a range");
    }
}

/// A `limit` above the ceiling is CLAMPED to it, `u32::MAX` included.
#[test]
fn a_limit_above_the_ceiling_is_clamped_to_it() {
    for kind in RANGED {
        let rows = planted();
        let store = Arc::new(BudgetOnlyStore::new(rows.clone(), 1));
        let mut wire = Wire::open(spawn(store.clone()));
        for limit in [CEILING as u32 + 1, 25, u32::MAX] {
            let got = wire.ask(&kind.request(TsRange::all(), Some(limit))).expect("answers");
            assert_eq!(
                got,
                frame_of(&kind.response(&old_reply(&rows, Some(CEILING as u32)), 1)),
                "{kind:?}, limit {limit}: answers what a limit of the ceiling answers"
            );
        }
        assert_eq!(store.asked(), vec![(kind, TsRange::all(), CEILING); 3], "{kind:?}");
        assert_eq!(store.loads(), 0, "{kind:?}");
    }
}

/// No `limit` — and `Some(0)`, which the wire has always read as "no cap" — over a range holding no
/// more than the ceiling answers the WHOLE range, byte-identical to the old frame, from a read of
/// `ceiling + 1`. `ScanExecFills` has no range: its series holds exactly the ceiling.
#[test]
fn a_request_without_a_limit_within_the_ceiling_is_the_old_whole_frame() {
    let rows = planted();
    // 1_000 ..= 7_000 is 1 + 3 + 1 + 1 + 2 + 1 + 1 rows: EXACTLY the ceiling.
    let at_ceiling = TsRange::of(1_000, 7_000);
    assert_eq!(in_range(&rows, at_ceiling).len(), CEILING, "the fixture sits AT the ceiling");
    for kind in RANGED {
        let store = Arc::new(BudgetOnlyStore::new(rows.clone(), 1));
        let mut wire = Wire::open(spawn(store.clone()));
        for range in [TsRange::of(4_000, 9_000), at_ceiling, TsRange::of(50_000, 60_000)] {
            for limit in [None, Some(0)] {
                let got = wire.ask(&kind.request(range, limit)).expect("answers");
                assert_eq!(
                    got,
                    frame_of(&kind.response(&in_range(&rows, range), 1)),
                    "{kind:?}, range {range:?}, limit {limit:?}: the whole range, byte-identical"
                );
            }
        }
        assert!(
            store.asked().iter().all(|(_, _, n)| *n == CEILING + 1),
            "{kind:?}: a no-limit request reads ceiling + 1 and no more: {:?}",
            store.asked()
        );
        assert_eq!(store.loads(), 0, "{kind:?}");
    }

    let series = in_range(&rows, at_ceiling);
    let store = Arc::new(BudgetOnlyStore::new(series.clone(), 1));
    let mut wire = Wire::open(spawn(store.clone()));
    let got = wire.ask(&Kind::ExecFills.request(TsRange::all(), None)).expect("answers");
    assert_eq!(got, frame_of(&Kind::ExecFills.response(&series, 1)), "the whole series");
    assert_eq!(store.asked(), vec![(Kind::ExecFills, TsRange::all(), CEILING + 1)]);
    assert_eq!(store.loads(), 0);
}

/// No `limit` over MORE than the ceiling is refused BY NAME — not read, not clamped in silence, not
/// sent to fail at `write_frame` — and the connection stays positional: the next request on it is
/// answered.
#[test]
fn a_request_without_a_limit_over_the_ceiling_is_refused_by_name_and_the_connection_survives() {
    for kind in ALL {
        let rows = planted();
        let store = Arc::new(BudgetOnlyStore::new(rows.clone(), 1));
        let mut wire = Wire::open(spawn(store.clone()));
        let limits: &[Option<u32>] =
            if kind == Kind::ExecFills { &[None] } else { &[None, Some(0)] };
        for &limit in limits {
            let got = wire.ask(&kind.request(TsRange::all(), limit)).expect("a refusal is a reply");
            let reply: Response = serde_json::from_slice(&got).expect("a Response");
            let Response::Error(why) = &reply else {
                panic!(
                    "{kind:?}, limit {limit:?}: 26 rows over a ceiling of {CEILING} must be REFUSED: {reply:?}"
                )
            };
            let (unit, name) = kind.unit_and_ceiling();
            assert!(
                why.contains(&format!("more than {CEILING} {unit}")),
                "{kind:?} names the ceiling: {why}"
            );
            assert!(why.contains(name), "{kind:?} names the constant {name}: {why}");
            assert!(why.contains(&format!("{VENUE}:{SYMBOL}")), "{kind:?} names the series: {why}");
            assert!(why.contains("`limit`"), "{kind:?} names the limit: {why}");

            // ...and the SAME connection answers the next request.
            assert!(
                matches!(wire.ping(), Ok(Response::Pong)),
                "{kind:?}: a Ping after it is answered"
            );
            if kind != Kind::ExecFills {
                let next = wire.ask(&kind.request(TsRange::all(), Some(4))).expect("answers");
                assert_eq!(
                    next,
                    frame_of(&kind.response(&old_reply(&rows, Some(4)), 1)),
                    "{kind:?}"
                );
            }
        }
        assert!(
            store.asked().iter().all(|(_, _, n)| *n == CEILING + 1 || *n == 4),
            "{kind:?}: the refusal is decided from a read of ceiling + 1: {:?}",
            store.asked()
        );
        assert_eq!(store.loads(), 0, "{kind:?}");
    }
}

/// The book ceiling is in the STORE's unit — a stored row per level — so a range of few events but
/// many levels is refused, and the same events one level shallower are answered. The read is still
/// `ceiling + 1`, of levels.
#[test]
fn the_book_ceiling_counts_stored_levels_not_events() {
    for kind in [Kind::Book, Kind::Depth] {
        let rows = planted();
        let store = Arc::new(BudgetOnlyStore::new(rows.clone(), 3));
        let mut wire = Wire::open(spawn(store.clone()));

        // 4_000 ..= 6_000: four events, twelve levels — under the ceiling in events, over it in rows.
        let over = TsRange::of(4_000, 6_000);
        assert_eq!(in_range(&rows, over).len(), 4);
        let reply: Response =
            serde_json::from_slice(&wire.ask(&kind.request(over, None)).expect("answers")).unwrap();
        assert!(
            matches!(&reply, Response::Error(why) if why.contains("book levels")),
            "{kind:?}: 4 events of 3 levels are 12 stored rows, over {CEILING}: {reply:?}"
        );
        // 4_000 ..= 5_000: three events, nine levels — answered whole.
        let under = TsRange::of(4_000, 5_000);
        let got = wire.ask(&kind.request(under, None)).expect("answers");
        assert_eq!(got, frame_of(&kind.response(&in_range(&rows, under), 3)), "{kind:?}");
        assert!(store.asked().iter().all(|(_, _, n)| *n == CEILING + 1), "{kind:?}");
        assert_eq!(store.loads(), 0, "{kind:?}");
    }
}

/// The entries a daemon actually runs pass [`ReadCeilings::PRODUCTION`] — an injected test ceiling
/// that production never received would leave every test above green and the real verbs
/// unbounded. The double holds 26 rows, so this asks the production numbers without planting them.
#[test]
fn the_production_entries_serve_the_production_ceilings() {
    for kind in ALL {
        let store = Arc::new(BudgetOnlyStore::new(planted(), 1));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let served: Arc<dyn HistStore + Send + Sync> = store.clone();
        thread::spawn(move || {
            let _ = vike_datahub::serve(listener, served);
        });
        let mut wire = Wire::open(addr);
        let whole = wire.ask(&kind.request(TsRange::all(), None)).expect("answers");
        assert_eq!(whole, frame_of(&kind.response(&planted(), 1)), "{kind:?}: 26 rows fit");
        let c = kind.production_ceiling();
        let mut expected = vec![(kind, TsRange::all(), c + 1)];
        if kind != Kind::ExecFills {
            wire.ask(&kind.request(TsRange::all(), Some(u32::MAX))).expect("answers");
            expected.push((kind, TsRange::all(), c));
        }
        assert_eq!(
            store.asked(),
            expected,
            "{kind:?}: ceiling + 1 for no limit, a huge limit clamped"
        );
        assert_eq!(store.loads(), 0, "{kind:?}");
    }
}

// ------------------------------------------------------------------------------------------------
// Half 3 — each ceiling's arithmetic
// ------------------------------------------------------------------------------------------------

/// One row's cost INSIDE a reply, its separating comma included: the difference between a reply of
/// two and a reply of one.
fn row_cost<T: Clone>(wrap: fn(Vec<T>) -> Response, row: T) -> usize {
    frame_of(&wrap(vec![row.clone(), row.clone()])).len() - frame_of(&wrap(vec![row])).len()
}

/// Each `MIN_*_JSON_BYTES` is a floor on what one real row of its kind costs in its reply, measured
/// with the encoder `write_frame` uses: the shortest real `ts` (13 digits), every float at its
/// shortest spelling (`0.0`), every `Option<f64>` at `Some(0.0)` (shorter than `null`), every string
/// empty, every integer `0`, every boolean `true`. If serde ever wrote a row shorter, a reply of more
/// than its ceiling could fit a frame and the ceiling would refuse something that works — so this
/// measures the figure each compile-time assertion takes on trust, and then tries the LONGER
/// spelling of each field, which must never come out shorter.
#[test]
fn the_smallest_real_row_of_every_kind_is_no_shorter_than_its_ceiling_assumes() {
    const TS: i64 = 1_000_000_000_000;

    let q = QuoteTick {
        ts: TS,
        local_ts: 0,
        bid: 0.0,
        ask: 0.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    };
    assert_eq!(row_cost(Response::Quotes, q.clone()), MIN_QUOTE_JSON_BYTES);
    for v in [
        QuoteTick { local_ts: TS, ..q.clone() },
        QuoteTick { bid: f64::NAN, ..q.clone() },
        QuoteTick { symbol: "X".into(), ..q.clone() },
    ] {
        assert!(row_cost(Response::Quotes, v.clone()) >= MIN_QUOTE_JSON_BYTES, "{v:?}");
    }

    let t = TradeTick {
        ts: TS,
        local_ts: 0,
        price: 0.0,
        size: 0.0,
        is_buyer_maker: true,
        symbol: String::new(),
    };
    assert_eq!(row_cost(Response::Trades, t.clone()), MIN_TRADE_JSON_BYTES);
    for v in
        [TradeTick { is_buyer_maker: false, ..t.clone() }, TradeTick { price: -0.0, ..t.clone() }]
    {
        assert!(row_cost(Response::Trades, v.clone()) >= MIN_TRADE_JSON_BYTES, "{v:?}");
    }

    // A LEVEL's cost: an event of two levels against the same event of one.
    let one = |price: f64, qty: f64| BookUpdate {
        ts: TS,
        local_ts: 0,
        seq: 0,
        kind: BookUpdateKind::Delta,
        tick_size: 0.0,
        bids: vec![BookLevel { price, qty }],
        asks: Vec::new(),
        symbol: String::new(),
    };
    let level_cost = |price: f64, qty: f64| {
        let mut two = one(price, qty);
        two.bids.push(BookLevel { price, qty });
        frame_of(&Response::BookUpdates(vec![two])).len()
            - frame_of(&Response::BookUpdates(vec![one(price, qty)])).len()
    };
    assert_eq!(level_cost(0.0, 0.0), MIN_BOOK_LEVEL_JSON_BYTES);
    assert!(level_cost(f64::NAN, 1e-7) >= MIN_BOOK_LEVEL_JSON_BYTES);
    // ...and an event of NO levels — one placeholder row in the store — costs far more than a level.
    let mut empty = one(0.0, 0.0);
    empty.bids.clear();
    assert!(row_cost(Response::BookUpdates, empty) > MIN_BOOK_LEVEL_JSON_BYTES);

    let c = CohortRow {
        ts: TS,
        asset: String::new(),
        axis: String::new(),
        cohort: String::new(),
        grading: String::new(),
        label_basis: String::new(),
        long_usd: 0.0,
        total_usd: 0.0,
    };
    assert_eq!(row_cost(Response::Cohort, c.clone()), MIN_COHORT_JSON_BYTES);
    assert!(
        row_cost(Response::Cohort, CohortRow { asset: "BTC".into(), ..c }) >= MIN_COHORT_JSON_BYTES
    );

    let p = PerpMetricRow { ts: TS, premium: 0.0, open_interest: Some(0.0) };
    assert_eq!(row_cost(Response::PerpMetrics, p), MIN_PERP_METRIC_JSON_BYTES);
    let none = PerpMetricRow { open_interest: None, ..p };
    assert_eq!(
        row_cost(Response::PerpMetrics, none),
        MIN_PERP_METRIC_JSON_BYTES + 1,
        "`null` is one byte LONGER than `0.0` — which is why the floor takes Some(0.0)"
    );

    let e = EquitySample {
        ts: TS,
        venue: String::new(),
        equity: 0.0,
        realized: 0.0,
        unrealized: 0.0,
        missing_prices: 0,
    };
    assert_eq!(row_cost(Response::Equity, e.clone()), MIN_EQUITY_JSON_BYTES);
    assert!(
        row_cost(Response::Equity, EquitySample { missing_prices: 7, ..e })
            >= MIN_EQUITY_JSON_BYTES
    );

    let f = ExecFillRow {
        ts: TS,
        trade_id: String::new(),
        client_order_id: String::new(),
        venue: String::new(),
        symbol: String::new(),
        side: 0,
        qty: 0.0,
        px: 0.0,
        commission: 0.0,
        mark_price: Some(0.0),
        liquidity_side: String::new(),
        commission_asset: String::new(),
    };
    assert_eq!(row_cost(Response::ExecFills, f.clone()), MIN_EXEC_FILL_JSON_BYTES);
    for v in [ExecFillRow { mark_price: None, ..f.clone() }, ExecFillRow { side: -1, ..f.clone() }]
    {
        assert!(row_cost(Response::ExecFills, v.clone()) >= MIN_EXEC_FILL_JSON_BYTES, "{v:?}");
    }
}

// ------------------------------------------------------------------------------------------------
// Half 4 — the bytes
// ------------------------------------------------------------------------------------------------

/// Every verb the byte cap guards that carries a `limit`: the seven scans and `LoadBars`.
const BYTE_RANGED: [Kind; 8] = [
    Kind::Bars,
    Kind::Quotes,
    Kind::Trades,
    Kind::Book,
    Kind::Depth,
    Kind::Cohort,
    Kind::Perp,
    Kind::Equity,
];

/// Book events this many levels deep are the FAT rows here: one event's JSON is several levels
/// wide, while four of them (eight stored rows) still sit under the injected row ceiling.
const FAT_LEVELS: usize = 2;

fn levels_of(kind: Kind) -> usize {
    match kind {
        Kind::Book | Kind::Depth => FAT_LEVELS,
        _ => 1,
    }
}

/// The length of `kind`'s reply carrying `rows` — `write_frame`'s own body.
fn reply_len(kind: Kind, rows: &[Planted]) -> usize {
    frame_of(&kind.response(rows, levels_of(kind))).len()
}

/// The page the ROW caps alone give `limit` over the whole series: the double's head of
/// `min(limit, CEILING)`, cut by the wire's whole-`ts` rule — what the server sent before the byte
/// cap existed.
fn row_capped_page(kind: Kind, rows: &[Planted], limit: u32) -> Vec<Planted> {
    let n = (limit as usize).min(CEILING);
    old_reply(&head_of(rows, n, levels_of(kind)), Some(n as u32))
}

/// The oracle for the byte cap: the longest whole-`ts` prefix of `page` whose reply fits `frame`,
/// found by serialising each candidate page WHOLE — never by summing row lengths, which is what the
/// server does, so a defect in its arithmetic cannot be repeated here. `None` when not even the
/// first timestamp's rows fit.
fn longest_page_that_fits(kind: Kind, page: &[Planted], frame: usize) -> Option<Vec<Planted>> {
    let mut best = None;
    for end in 1..=page.len() {
        let whole_ts = end == page.len() || page[end].0 != page[end - 1].0;
        if whole_ts && reply_len(kind, &page[..end]) <= frame {
            best = Some(page[..end].to_vec());
        }
    }
    best
}

/// A server over the planted series (book events [`FAT_LEVELS`] deep) under a frame of `frame`.
fn spawn_framed(kind: Kind, rows: Vec<Planted>, frame: usize) -> (Arc<BudgetOnlyStore>, Wire) {
    let store = Arc::new(BudgetOnlyStore::new(rows, levels_of(kind)));
    let wire = Wire::open(spawn_with(store.clone(), injected_with_frame(frame)));
    (store, wire)
}

/// The window the no-`limit` byte tests read: four rows, so eight stored book rows — under the
/// injected row ceiling for every kind, so only the FRAME can refuse it.
fn no_limit_window() -> TsRange {
    TsRange::of(4_000, 6_000)
}

/// A reply that fits the frame is UNTOUCHED — with a `limit`, and with none — even when it fits
/// with not one byte to spare.
#[test]
fn a_reply_that_fits_the_frame_is_untouched() {
    for kind in BYTE_RANGED {
        let rows = planted();
        let page = row_capped_page(kind, &rows, CEILING as u32);
        let (store, mut wire) = spawn_framed(kind, rows.clone(), reply_len(kind, &page));
        let got = wire.ask(&kind.request(TsRange::all(), Some(CEILING as u32))).expect("answers");
        assert_eq!(got, frame_of(&kind.response(&page, levels_of(kind))), "{kind:?}, with a limit");

        let whole = in_range(&rows, no_limit_window());
        let (_, mut wire) = spawn_framed(kind, rows.clone(), reply_len(kind, &whole));
        let got = wire.ask(&kind.request(no_limit_window(), None)).expect("answers");
        assert_eq!(got, frame_of(&kind.response(&whole, levels_of(kind))), "{kind:?}, no limit");
        assert_eq!(store.loads(), 0, "{kind:?}");
    }
    let series = in_range(&planted(), TsRange::of(1_000, 7_000));
    let (_, mut wire) =
        spawn_framed(Kind::ExecFills, series.clone(), reply_len(Kind::ExecFills, &series));
    let got = wire.ask(&Kind::ExecFills.request(TsRange::all(), None)).expect("answers");
    assert_eq!(got, frame_of(&Kind::ExecFills.response(&series, 1)), "the whole fill series");
}

/// A `limit`ed reply whose rows number no more than the ceiling but whose BYTES pass the frame is
/// cut to the longest whole-`ts` page that fits — shorter than the row caps alone would send, and
/// as legal as any soft-capped page: the pager continues from its last `ts`. Over frames that cut
/// one byte short of the whole page, exactly at a group boundary, one byte past it, and one byte
/// short of the next group — book events fat, so a tick kind is cut by bytes too.
#[test]
fn a_limited_reply_over_the_frame_is_cut_to_a_shorter_whole_ts_page() {
    for kind in BYTE_RANGED {
        let rows = planted();
        let page = row_capped_page(kind, &rows, CEILING as u32);
        assert!(page.len() > 5, "{kind:?}: the page must have groups to cut: {page:?}");
        for frame in [
            reply_len(kind, &page) - 1,
            reply_len(kind, &page[..4]),
            reply_len(kind, &page[..4]) + 1,
            reply_len(kind, &page[..5]) - 1,
        ] {
            let expected = longest_page_that_fits(kind, &page, frame)
                .unwrap_or_else(|| panic!("{kind:?}, frame {frame}: the first ts fits"));
            assert!(
                !expected.is_empty() && expected.len() < page.len(),
                "{kind:?}, frame {frame}: the fixture must make the frame bite"
            );
            let (store, mut wire) = spawn_framed(kind, rows.clone(), frame);
            let got = wire
                .ask(&kind.request(TsRange::all(), Some(CEILING as u32)))
                .unwrap_or_else(|e| panic!("{kind:?}, frame {frame}: a reply, not a drop: {e}"));
            assert!(got.len() <= frame, "{kind:?}, frame {frame}: the reply fits the frame");
            assert_eq!(
                got,
                frame_of(&kind.response(&expected, levels_of(kind))),
                "{kind:?}, frame {frame}: the longest whole-ts page that fits ({} of {} rows)",
                expected.len(),
                page.len()
            );
            assert_eq!(store.asked(), vec![(kind, TsRange::all(), CEILING)], "{kind:?}");
            assert_eq!(store.loads(), 0, "{kind:?}");
        }
    }
}

/// With no `limit`, a reply under its row ceiling whose BYTES pass the frame is REFUSED BY NAME —
/// never cut in silence, never sent to fail at `write_frame` — and the connection stays: the same
/// window asked WITH a `limit` is then answered with the page that fits.
#[test]
fn a_reply_without_a_limit_over_the_frame_is_refused_by_name_and_the_connection_survives() {
    for kind in BYTE_RANGED {
        let rows = planted();
        let whole = in_range(&rows, no_limit_window());
        let frame = reply_len(kind, &whole) - 1;
        let (store, mut wire) = spawn_framed(kind, rows.clone(), frame);
        let got = wire.ask(&kind.request(no_limit_window(), None)).expect("a refusal is a reply");
        let reply: Response = serde_json::from_slice(&got).expect("a Response");
        let Response::Error(why) = &reply else {
            panic!("{kind:?}: a reply one byte over the frame must be REFUSED: {reply:?}")
        };
        let (_, name) = kind.unit_and_ceiling();
        assert!(why.contains("MAX_FRAME_LEN"), "{kind:?} names the frame: {why}");
        assert!(why.contains(&format!("{frame} bytes")), "{kind:?} names its size: {why}");
        assert!(why.contains(name), "{kind:?} says it is under {name}: {why}");
        assert!(why.contains("`limit`"), "{kind:?} names the remedy: {why}");

        assert!(matches!(wire.ping(), Ok(Response::Pong)), "{kind:?}: the connection survives");
        let page = longest_page_that_fits(kind, &whole, frame).expect("the first ts fits");
        let next =
            wire.ask(&kind.request(no_limit_window(), Some(CEILING as u32))).expect("answers");
        assert_eq!(next, frame_of(&kind.response(&page, levels_of(kind))), "{kind:?}");
        assert_eq!(store.loads(), 0, "{kind:?}");
    }

    let series = in_range(&planted(), TsRange::of(1_000, 7_000));
    let frame = reply_len(Kind::ExecFills, &series) - 1;
    let (_, mut wire) = spawn_framed(Kind::ExecFills, series, frame);
    let got = wire.ask(&Kind::ExecFills.request(TsRange::all(), None)).expect("a reply");
    let reply: Response = serde_json::from_slice(&got).expect("a Response");
    assert!(
        matches!(&reply, Response::Error(why) if why.contains("MAX_FRAME_LEN") && why.contains("SCAN_EXEC_FILLS_CEILING")),
        "a fill series one byte over the frame is refused by name: {reply:?}"
    );
    assert!(matches!(wire.ping(), Ok(Response::Pong)));
}

/// ⚠ The rows of the FIRST timestamp alone passing the frame is a NAMED ERROR — never an empty page,
/// which `RemoteHistStore`'s pager reads as the end of the range, losing the rest with no error.
#[test]
fn a_first_timestamp_too_wide_for_any_page_is_an_error_never_an_empty_page() {
    for kind in BYTE_RANGED {
        let rows = planted();
        let from = TsRange { start: Some(2_000), end: None };
        let group = in_range(&rows, TsRange::of(2_000, 2_000));
        assert_eq!(group.len(), 3, "the fixture stores 2_000 three times");
        let frame = reply_len(kind, &group) - 1;
        let (store, mut wire) = spawn_framed(kind, rows.clone(), frame);

        let got = wire.ask(&kind.request(from, Some(CEILING as u32))).expect("a reply, not a drop");
        assert_ne!(
            got,
            frame_of(&kind.response(&[], 1)),
            "{kind:?}: an EMPTY page would end a paged read here in silence"
        );
        let reply: Response = serde_json::from_slice(&got).expect("a Response");
        let Response::Error(why) = &reply else {
            panic!("{kind:?}: a first ts wider than the frame must be a named error: {reply:?}")
        };
        assert!(why.contains("ts 2000"), "{kind:?} names the timestamp: {why}");
        assert!(why.contains("MAX_FRAME_LEN"), "{kind:?} names the frame: {why}");

        // ...the connection survives, and with no `limit` over that one timestamp it is refused too.
        assert!(matches!(wire.ping(), Ok(Response::Pong)), "{kind:?}");
        let got = wire.ask(&kind.request(TsRange::of(2_000, 2_000), None)).expect("a reply");
        let reply: Response = serde_json::from_slice(&got).expect("a Response");
        assert!(
            matches!(&reply, Response::Error(why) if why.contains("MAX_FRAME_LEN")),
            "{kind:?}: {reply:?}"
        );
        assert_eq!(store.loads(), 0, "{kind:?}");
    }
}

/// Production serves the frame `write_frame` enforces — an injected test frame that production
/// never received would leave every byte test above green and the real replies uncut.
#[test]
fn the_production_frame_is_the_wire_frame() {
    assert_eq!(
        ReadCeilings::PRODUCTION.frame_bytes,
        vike_datahub_client::MAX_FRAME_LEN as usize,
        "the byte cap must cut to the frame write_frame refuses to pass"
    );
}
