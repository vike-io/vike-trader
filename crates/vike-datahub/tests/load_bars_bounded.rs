//! **`Request::LoadBars` reads only what it answers** — driven through the REAL verb over a loopback
//! socket, in the shape `docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md`'s
//! section 5 names (its tests 3, 4 and 5).
//!
//! # What went wrong before this file existed
//!
//! `crates/vike-datahub/src/server.rs`'s handler answered `LoadBars` with `HistStore::load_bars`
//! over the client's WHOLE range and only then cut the reply to the client's `limit`. The frame was
//! bounded and the allocation was the range: one request with no `limit` over a long `5s` series
//! decoded gigabytes inside the daemon that also runs the market-data and recorder planes, and
//! `RemoteHistStore`'s pager — every reader's route since decision 0084 — asked for a 10,000-row page
//! by making the server load everything from that page to the end of the range, once per page.
//!
//! # The three halves
//!
//! 1. **The verb never loads the range** — over [`HeadOnlyStore`], whose `load_bars` PANICS and
//!    whose `load_bars_head` records what it was asked. Every reply the verb sends within the
//!    ceiling is held BYTE-IDENTICAL to the frame the old handler sent, computed here from the
//!    planted rows by [`old_reply`]; a request with no `limit` over more than the ceiling is refused
//!    BY NAME and the NEXT request on the same connection is answered. The ceiling is injected
//!    (`vike_datahub::server::serve_with_read_ceilings`), so no test plants half a million rows, and
//!    [`the_production_entries_serve_the_production_ceiling`] pins that the entries a daemon runs pass
//!    the constant. Values alone could not tell a bounded read from one that loads and discards; a
//!    store that cannot be loaded can.
//! 2. **The ceiling's arithmetic is measured, not trusted** —
//!    [`the_smallest_real_bar_is_no_shorter_than_the_ceiling_assumes`] serialises the shortest real
//!    bar with serde. The `const` assertions beside `LOAD_BARS_CEILING` hold the rest of the
//!    derivation at compile time.
//! 3. **End to end over a real store** (`real_store`, behind `serve-datafusion`): `RemoteHistStore`
//!    over a series several pages wide returns exactly the store's own `load_bars` — the bars twin
//!    of `crates/vike-datahub/tests/wire_six_verbs_roundtrip.rs`'s
//!    `a_scan_wider_than_one_page_comes_back_whole`.
//!
//! The first two need no engine and no feature, so they run in the derived roster lane on every PR.

use std::assert_matches;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use vike_data::{DataError, HistStore, TsRange};
use vike_datahub::server::{
    LOAD_BARS_CEILING, MIN_BAR_JSON_BYTES, ReadCeilings, serve_with_read_ceilings,
};
use vike_datahub_client::proto::{Request, Response, read_frame_raw, write_frame};
use vike_model::Bar;

mod common;
use common::{frame_of, ranges};

const VENUE: &str = "oanda";
const SYMBOL: &str = "EUR_USD";
const INTERVAL: &str = "5s";

/// The injected ceiling — small enough that every arm is reachable with a handful of rows.
const CEILING: usize = 10;

fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: close,
        high: close + 1.0,
        low: close - 1.0,
        close,
        volume: 3.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// 26 rows over 20 timestamps, with groups that straddle the caps below: `2_000` three times,
/// `5_000` twice and `10_000` four times, each copy with its own `close` so a reply that reordered
/// or dropped one is visible.
fn planted() -> Vec<Bar> {
    let mut rows = Vec::new();
    for k in 1..=20i64 {
        let copies = match k {
            2 => 3,
            5 => 2,
            10 => 4,
            _ => 1,
        };
        for c in 0..copies {
            rows.push(bar(k * 1_000, (k * 10 + c) as f64));
        }
    }
    assert_eq!(rows.len(), 26);
    rows
}

/// The rows of `rows` inside `range`, inclusive at both ends — what `load_bars` answers.
fn in_range(rows: &[Bar], range: TsRange) -> Vec<Bar> {
    rows.iter()
        .filter(|b| range.start.is_none_or(|s| b.ts >= s) && range.end.is_none_or(|e| b.ts <= e))
        .cloned()
        .collect()
}

/// The reply the OLD handler sent for `limit` over `rows` (the range's whole answer): the wire's
/// whole-`ts` cap, written out here from its three cases rather than taken from the server, so a
/// defect there cannot be repeated in the thing that judges it. `None` and `Some(0)` are no cap.
fn old_reply(rows: &[Bar], limit: Option<u32>) -> Vec<Bar> {
    let mut rows = rows.to_vec();
    let Some(cap) = limit.map(|l| l as usize).filter(|l| *l > 0) else { return rows };
    if rows.len() <= cap {
        return rows;
    }
    let straddling = rows[cap - 1].ts;
    if rows[cap].ts != straddling {
        rows.truncate(cap); // the cap falls on a group boundary
        return rows;
    }
    let first = rows.iter().position(|b| b.ts == straddling).expect("the group is in the rows");
    if first > 0 {
        rows.truncate(first); // stop before the straddling group
    } else {
        let last = rows.iter().rposition(|b| b.ts == straddling).expect("the group is in the rows");
        rows.truncate(last + 1); // the FIRST group straddles: take it whole, over the cap
    }
    rows
}

// ------------------------------------------------------------------------------------------------
// The double
// ------------------------------------------------------------------------------------------------

/// A `HistStore` that answers a bar series' HEAD from planted rows and REFUSES TO BE LOADED — the
/// `EdgesOnlyStore` of `crates/vike-datahub/tests/backfill_readback.rs`, one verb over.
///
/// ⚠ Its `load_bars` PANICS: a handler that reads its range with `load_bars` gets no answer out of
/// this store at all — its connection thread dies and the client sees the socket drop. It counts the
/// attempt first, so a swallowed panic would still leave a nonzero [`Self::loads`].
///
/// Its `load_bars_head` honours the contract and no more: a complete prefix of the range holding at
/// least `n` rows — and `OVERSHOOT` rows MORE than that where the range has them, the way a real
/// store answers in whole storage blocks. So a server that trusted the store's row count instead of
/// cutting the answer itself would reply with too much.
struct HeadOnlyStore {
    rows: Vec<Bar>,
    /// Every `load_bars_head` call: `(range, n)`.
    asked: Mutex<Vec<(TsRange, usize)>>,
    /// How many times anything tried to `load_bars`.
    loads: AtomicUsize,
}

/// How many rows past `n` the double answers with, where the range has them.
const OVERSHOOT: usize = 3;

impl HeadOnlyStore {
    fn new(rows: Vec<Bar>) -> Self {
        Self { rows, asked: Mutex::new(Vec::new()), loads: AtomicUsize::new(0) }
    }
    fn asked(&self) -> Vec<(TsRange, usize)> {
        self.asked.lock().unwrap().clone()
    }
    fn loads(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }
}

impl HistStore for HeadOnlyStore {
    fn load_bars(&self, _: &str, _: &str, _: &str, _: TsRange) -> Result<Vec<Bar>, DataError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        panic!(
            "HeadOnlyStore::load_bars was called — the LoadBars verb must ask `load_bars_head` for \
             what it can answer and never materialise the client's range"
        );
    }

    fn load_bars_head(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        n: usize,
    ) -> Result<Vec<Bar>, DataError> {
        assert_eq!((venue, symbol, interval), (VENUE, SYMBOL, INTERVAL), "the asked-for series");
        self.asked.lock().unwrap().push((range, n));
        let mut rows = in_range(&self.rows, range);
        let mut take = n.saturating_add(OVERSHOOT).min(rows.len());
        // Complete: never cut inside a timestamp.
        while take > 0 && take < rows.len() && rows[take].ts == rows[take - 1].ts {
            take += 1;
        }
        rows.truncate(take);
        Ok(rows)
    }

    // ---- inert stubs: this double exists for the bar head and nothing else --------------------
    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

// ------------------------------------------------------------------------------------------------
// Harness
// ------------------------------------------------------------------------------------------------

/// Serve `store` on an ephemeral loopback port under the injected [`CEILING`]. Key-less.
fn spawn(store: Arc<dyn HistStore + Send + Sync>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let _ = serve_with_read_ceilings(
            listener,
            store,
            ReadCeilings { bars: CEILING, ..ReadCeilings::PRODUCTION },
        );
    });
    addr
}

/// One positional connection, spoken in raw frames so a reply is compared as BYTES.
struct Wire(TcpStream);

impl Wire {
    fn open(addr: SocketAddr) -> Self {
        Self(TcpStream::connect(addr).expect("connect to the loopback server"))
    }

    /// Send one `LoadBars` and return the reply frame's body bytes.
    fn load_bars(&mut self, range: TsRange, limit: Option<u32>) -> io::Result<Vec<u8>> {
        let request = Request::LoadBars {
            venue: VENUE.to_string(),
            symbol: SYMBOL.to_string(),
            interval: INTERVAL.to_string(),
            start: range.start,
            end: range.end,
            limit,
        };
        write_frame(&mut self.0, &request)?;
        read_frame_raw(&mut self.0)
    }

    fn ping(&mut self) -> io::Result<Response> {
        write_frame(&mut self.0, &Request::Ping)?;
        let body = read_frame_raw(&mut self.0)?;
        Ok(serde_json::from_slice(&body).expect("a Response"))
    }
}

// ------------------------------------------------------------------------------------------------
// Half 1 — the verb never loads the range
// ------------------------------------------------------------------------------------------------

/// A `limit` within the ceiling asks the store for exactly that many rows and answers EXACTLY the
/// frame the old handler sent — over limits that land on a group boundary, inside a straddling
/// group, and inside the FIRST group of a range.
#[test]
fn a_limited_request_reads_a_head_and_answers_the_old_frame() {
    let rows = planted();
    let store = Arc::new(HeadOnlyStore::new(rows.clone()));
    let addr = spawn(store.clone());
    let mut wire = Wire::open(addr);

    let mut expected_asks = Vec::new();
    for range in ranges() {
        for limit in 1..=CEILING as u32 {
            let got = wire.load_bars(range, Some(limit)).expect("a limited LoadBars answers");
            let old = old_reply(&in_range(&rows, range), Some(limit));
            assert_eq!(
                got,
                frame_of(&Response::Bars(old.clone())),
                "range {range:?}, limit {limit}: the reply must be byte-identical to the old \
                 frame ({} rows)",
                old.len()
            );
            expected_asks.push((range, limit as usize));
        }
    }
    assert_eq!(store.asked(), expected_asks, "each request asks for its own limit, once");
    assert_eq!(store.loads(), 0, "nothing tried to materialise a range");
}

/// A `limit` above the ceiling is CLAMPED to it, `u32::MAX` included: the store is asked for the
/// ceiling and the reply is the old frame for a ceiling-sized limit.
#[test]
fn a_limit_above_the_ceiling_is_clamped_to_it() {
    let rows = planted();
    let store = Arc::new(HeadOnlyStore::new(rows.clone()));
    let addr = spawn(store.clone());
    let mut wire = Wire::open(addr);

    for limit in [CEILING as u32 + 1, 25, u32::MAX] {
        let got = wire.load_bars(TsRange::all(), Some(limit)).expect("answers");
        assert_eq!(
            got,
            frame_of(&Response::Bars(old_reply(&rows, Some(CEILING as u32)))),
            "limit {limit} answers what a limit of the ceiling answers"
        );
    }
    assert_eq!(
        store.asked(),
        vec![(TsRange::all(), CEILING); 3],
        "every over-ceiling limit asks the store for the ceiling, never for the limit"
    );
    assert_eq!(store.loads(), 0);
}

/// No `limit` — and `Some(0)`, which the wire has always read as "no cap" — over a range holding no
/// more than the ceiling answers the WHOLE range, byte-identical to the old frame, from a read of
/// `ceiling + 1`.
#[test]
fn a_request_without_a_limit_within_the_ceiling_is_the_old_whole_frame() {
    let rows = planted();
    let store = Arc::new(HeadOnlyStore::new(rows.clone()));
    let addr = spawn(store.clone());
    let mut wire = Wire::open(addr);

    // A window sitting EXACTLY at the ceiling: 1_000 ..= 7_000 is 1 + 3 + 1 + 1 + 2 + 1 + 1 rows.
    let at_ceiling = TsRange::of(1_000, 7_000);
    assert_eq!(in_range(&rows, at_ceiling).len(), CEILING, "the fixture sits AT the ceiling");
    for range in [TsRange::of(4_000, 9_000), at_ceiling, TsRange::of(50_000, 60_000)] {
        for limit in [None, Some(0)] {
            let got = wire.load_bars(range, limit).expect("answers");
            assert_eq!(
                got,
                frame_of(&Response::Bars(in_range(&rows, range))),
                "range {range:?}, limit {limit:?}: the whole range, byte-identical to the old frame"
            );
        }
    }
    assert!(
        store.asked().iter().all(|(_, n)| *n == CEILING + 1),
        "a no-limit request reads ceiling + 1 and no more: {:?}",
        store.asked()
    );
    assert_eq!(store.loads(), 0);
}

/// No `limit` over MORE than the ceiling is refused BY NAME — not read, not clamped in silence, not
/// sent to fail at `write_frame` — and the connection stays positional: the next request on it, of
/// either kind, is answered.
#[test]
fn a_request_without_a_limit_over_the_ceiling_is_refused_by_name_and_the_connection_survives() {
    let rows = planted();
    let store = Arc::new(HeadOnlyStore::new(rows.clone()));
    let addr = spawn(store.clone());
    let mut wire = Wire::open(addr);

    for limit in [None, Some(0)] {
        let got = wire.load_bars(TsRange::all(), limit).expect("a refusal is a reply, not a drop");
        let reply: Response = serde_json::from_slice(&got).expect("a Response");
        let Response::Error(why) = &reply else {
            panic!(
                "limit {limit:?}: 26 rows over a ceiling of {CEILING} must be REFUSED: {reply:?}"
            )
        };
        assert!(why.contains(&format!("more than {CEILING} bars")), "names the ceiling: {why}");
        assert!(why.contains("LOAD_BARS_CEILING"), "names the constant: {why}");
        assert!(why.contains(&format!("{VENUE}:{SYMBOL}:{INTERVAL}")), "names the series: {why}");
        assert!(why.contains("`limit`"), "names the remedy: {why}");

        // ...and the SAME connection answers the next request of either kind.
        assert_matches!(wire.ping(), Ok(Response::Pong), "a Ping after the refusal is answered");
        let next = wire.load_bars(TsRange::all(), Some(4)).expect("a LoadBars after it answers");
        assert_eq!(next, frame_of(&Response::Bars(old_reply(&rows, Some(4)))));
    }
    assert_eq!(
        store.asked(),
        vec![
            (TsRange::all(), CEILING + 1),
            (TsRange::all(), 4),
            (TsRange::all(), CEILING + 1),
            (TsRange::all(), 4),
        ],
        "the refusal is decided from a read of ceiling + 1, never of the range"
    );
    assert_eq!(store.loads(), 0);
}

/// The entries a daemon actually runs pass [`LOAD_BARS_CEILING`] — an injected test ceiling that
/// production never received would leave every test above green and the real verb unbounded. The
/// double holds 26 rows, so this asks the production numbers without planting them.
#[test]
fn the_production_entries_serve_the_production_ceiling() {
    let store = Arc::new(HeadOnlyStore::new(planted()));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let served: Arc<dyn HistStore + Send + Sync> = store.clone();
    thread::spawn(move || {
        let _ = vike_datahub::serve(listener, served);
    });
    let mut wire = Wire::open(addr);

    let whole = wire.load_bars(TsRange::all(), None).expect("answers");
    assert_eq!(whole, frame_of(&Response::Bars(planted())), "26 rows are far under the ceiling");
    wire.load_bars(TsRange::all(), Some(u32::MAX)).expect("answers");
    assert_eq!(
        store.asked(),
        vec![(TsRange::all(), LOAD_BARS_CEILING + 1), (TsRange::all(), LOAD_BARS_CEILING)],
        "`serve` reads ceiling + 1 for no limit and clamps a huge limit to the ceiling"
    );
    assert_eq!(store.loads(), 0);
}

// ------------------------------------------------------------------------------------------------
// Half 2 — the ceiling's arithmetic
// ------------------------------------------------------------------------------------------------

/// `MIN_BAR_JSON_BYTES` is a floor on what one real bar costs in a `Response::Bars` frame, measured
/// with the encoder `write_frame` uses: the shortest real `ts` (13 digits, 2001-09-09), every price
/// at its shortest JSON spelling, every optional field `null`. If serde ever wrote a bar shorter, a
/// reply of more than `LOAD_BARS_CEILING` bars could fit a frame and the ceiling would refuse
/// something that works — so this measures the figure the compile-time assertions take on trust.
#[test]
fn the_smallest_real_bar_is_no_shorter_than_the_ceiling_assumes() {
    let shortest = Bar {
        ts: 1_000_000_000_000,
        open: 0.0,
        high: 0.0,
        low: 0.0,
        close: 0.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    // One bar's cost INSIDE a reply, its separating comma included: the difference between a reply
    // of two and a reply of one.
    let one = frame_of(&Response::Bars(vec![shortest.clone()])).len();
    let two = frame_of(&Response::Bars(vec![shortest.clone(), shortest.clone()])).len();
    assert_eq!(two - one, MIN_BAR_JSON_BYTES, "the floor is the measured shortest bar, exactly");

    // No price spelling is shorter than the `0.0` above — a NaN is `null`, a big one `1e16`.
    for v in [0.0, 1.0, 9.0, -0.0, 1e16, 1e-7, f64::NAN, f64::MIN_POSITIVE] {
        let spelled = serde_json::to_string(&v).expect("serialize a price");
        assert!(spelled.len() >= 3, "{v:?} is spelled {spelled:?}, shorter than `0.0`");
    }
    // ...and every real `ts` is at least 13 digits.
    let mut later = shortest;
    later.ts = 1_700_000_000_000;
    assert!(frame_of(&Response::Bars(vec![later])).len() >= one, "a later ts costs no fewer bytes");
}

// ------------------------------------------------------------------------------------------------
// Half 3 — end to end over a real store
// ------------------------------------------------------------------------------------------------

#[cfg(feature = "serve-datafusion")]
mod real_store {
    use super::*;

    use vike_data::DataFusionHist;
    use vike_datahub_client::RemoteHistStore;

    /// 2023-11-14 00:00 UTC, so the series' parts fall on real UTC dates.
    const BASE: i64 = 1_699_920_000_000;
    const STEP: i64 = 5_000;
    /// More than two `RemoteHistStore` pages (10,000 rows each), over two UTC days.
    const ROWS: i64 = 25_000;

    /// `RemoteHistStore` over a real series several pages wide returns EXACTLY the store's own
    /// `load_bars` — rows and order — over the whole range, a range starting inside a part, and a
    /// bounded range across the day boundary. The series carries a re-fetched window across the
    /// first page boundary, so every timestamp there is stored twice and the pager's whole-`ts` rule
    /// is exercised on top of the head read. The end-to-end form of the design's section 2 equality.
    #[test]
    fn a_paged_read_of_a_real_series_is_exactly_its_load_bars() {
        let dir = tempfile::tempdir().expect("temp store dir");
        let store = DataFusionHist::open(dir.path()).expect("open store");
        let main: Vec<Bar> = (0..ROWS).map(|i| bar(BASE + i * STEP, 1.0 + i as f64)).collect();
        store.append_bars(VENUE, SYMBOL, INTERVAL, &main, Some("main")).unwrap();
        let refetch: Vec<Bar> =
            (9_990..10_010).map(|i| bar(BASE + i * STEP, 0.5 + i as f64)).collect();
        store.append_bars(VENUE, SYMBOL, INTERVAL, &refetch, Some("refetch")).unwrap();

        let local: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let served = local.clone();
        thread::spawn(move || {
            let _ = vike_datahub::serve(listener, served);
        });
        let remote = RemoteHistStore::new(addr.to_string());

        for range in [
            TsRange::all(),
            TsRange { start: Some(BASE + 123 * STEP + 1), end: None },
            TsRange::of(BASE + 15_000 * STEP, BASE + 20_000 * STEP),
        ] {
            let direct = local.load_bars(VENUE, SYMBOL, INTERVAL, range).expect("direct read");
            let wire = remote.load_bars(VENUE, SYMBOL, INTERVAL, range).expect("paged read");
            assert_eq!(wire.len(), direct.len(), "range {range:?}: every row crosses the wire");
            assert_eq!(wire, direct, "range {range:?}: rows and order, exactly");
        }
        let whole = local.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
        assert_eq!(whole.len(), (ROWS + 20) as usize, "the fixture holds what it says");
        assert!(whole.len() > 2 * 10_000, "more than two pages, or the pager is not exercised");
    }
}
