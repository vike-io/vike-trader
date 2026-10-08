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
use std::sync::Arc;
use std::thread;

use vike_data::{CohortRow, ExecFillRow, HistStore, PerpMetricRow, TsRange};
use vike_datahub::server::{ReadCeilings, serve_with_read_ceilings};
use vike_datahub_client::proto::{Request, Response, read_frame_raw, write_frame};
use vike_model::{Bar, BookLevel, BookUpdate, BookUpdateKind, EquitySample, QuoteTick, TradeTick};

mod common;
use common::{frame_of, ranges};

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

#[path = "scan_ceilings/budget_store.rs"]
mod budget_store;
#[path = "scan_ceilings/ceiling_arithmetic.rs"]
mod ceiling_arithmetic;
#[path = "scan_ceilings/frame_bytes.rs"]
mod frame_bytes;
#[path = "scan_ceilings/no_verb_reads_the_range.rs"]
mod no_verb_reads_the_range;
