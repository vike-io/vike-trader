//! **A long compute verb whose client leaves stops working** — the compute daemon's half of MCP
//! request cancellation: `crates/vike-backtest/src/compute_server/connection.rs`'s `PeerWatch` sets
//! a peer-gone flag when the client's socket closes, and every watched verb (`is_watched` there:
//! `RunBacktest`, `RunParamscanProfile`, `RunWalkforwardProfile`) reads it at its next unit of work —
//! a search POINT (`StoreEvaluator::with_cancel`), a SERIES load
//! (`harness::run::run_backtest_cancellable`), a walk-forward WINDOW or training-half point
//! (`walkforward::runner`'s `run_walkforward_cancellable` /
//! `run_walkforward_optimized_cancellable`) — so the run runs out of work at once and the verb
//! answers `Response::Error("cancelled: …")` instead of a report.
//!
//! The work is MEASURED rather than inferred: the store double below sleeps [`READ_DELAY`] in every
//! `load_bars` AND every `scan_symbol_properties`, and counts both. A grid point reads bars once; a
//! backtest reads them once per symbol; a walk-forward reads them once and then, under
//! `snap_to_properties = true`, reads the properties grid at least once per engine run (its fill
//! asks `properties_as_of`) — one per window, one per training-half point. So a full run's read
//! count and its minimum wall time are both known in advance, and a cancelled run stops within one
//! watch interval plus the work already running. Each control test proves the full count, so a
//! cancel test can never pass by measuring a run that was never slow.
//!
//! Hermetic: an ephemeral `127.0.0.1:0` listener, no prod store, no network. Default features (no
//! `datafusion-store`): the double hands bars straight to the harness.

use std::io::Read;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use vike_backtest::compute_server::serve;
use vike_data::{DataError, ExecFillRow, ExecOrderRow, HistStore, TsRange};
use vike_datahub_client::proto::{Request, Response, read_frame, write_frame};
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

/// What every `load_bars` costs. Long enough that the grid's runtime is the store's and not the
/// engine's, short enough that the tests take a second or two each.
const READ_DELAY: Duration = Duration::from_millis(100);

/// The grid a cancelled test ships. A full run costs at least this many store reads.
const BIG_GRID: usize = 200;

/// How long a cancelled search may take to stop after its client leaves. The design answer is one
/// `PEER_POLL` (200 ms) plus the points already running (one [`READ_DELAY`] each, per read).
const STOP_BUDGET: Duration = Duration::from_secs(5);

/// A `HistStore` whose `load_bars` sleeps [`READ_DELAY`], counts the call, and hands back one fixed
/// series (for every symbol asked) — `tests/harness_risk_wiring.rs`'s `FixedBarsStore` with a clock
/// and a counter. `scan_symbol_properties` sleeps and counts too and answers "no record", so a
/// `snap_to_properties` run fills unconstrained and pays one read per lookup. Every other method is
/// an inert stub.
struct SlowCountingStore {
    bars: Vec<Bar>,
    reads: Arc<AtomicUsize>,
}

impl SlowCountingStore {
    fn slow_read(&self) {
        thread::sleep(READ_DELAY);
        self.reads.fetch_add(1, Ordering::SeqCst);
    }
}

impl HistStore for SlowCountingStore {
    fn load_bars(&self, _v: &str, _s: &str, _i: &str, _r: TsRange) -> Result<Vec<Bar>, DataError> {
        self.slow_read();
        Ok(self.bars.clone())
    }
    fn scan_quotes(&self, _v: &str, _s: &str, _r: TsRange) -> Result<Vec<QuoteTick>, DataError> {
        Ok(Vec::new())
    }
    fn scan_trades(&self, _v: &str, _s: &str, _r: TsRange) -> Result<Vec<TradeTick>, DataError> {
        Ok(Vec::new())
    }
    fn append_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _b: &[Bar],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_quotes(
        &self,
        _v: &str,
        _s: &str,
        _t: &[QuoteTick],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_trades(
        &self,
        _v: &str,
        _s: &str,
        _t: &[TradeTick],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_book_updates(
        &self,
        _v: &str,
        _s: &str,
        _u: &[BookUpdate],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_book_updates(
        &self,
        _v: &str,
        _s: &str,
        _r: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        Ok(Vec::new())
    }
    fn append_symbol_properties(
        &self,
        _v: &str,
        _s: &str,
        _rows: &[(i64, SymbolProperties)],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_symbol_properties(
        &self,
        _v: &str,
        _s: &str,
        _r: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        self.slow_read();
        Ok(Vec::new())
    }
    fn append_equity(
        &self,
        _v: &str,
        _s: &str,
        _rows: &[EquitySample],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_equity(&self, _v: &str, _s: &str, _r: TsRange) -> Result<Vec<EquitySample>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_fills(
        &self,
        _v: &str,
        _s: &str,
        _rows: &[ExecFillRow],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_fills(&self, _v: &str, _s: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_orders(
        &self,
        _v: &str,
        _s: &str,
        _rows: &[ExecOrderRow],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_orders(&self, _v: &str, _s: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        Ok(Vec::new())
    }
    fn resample_quotes_to_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _r: TsRange,
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn resample_trades_to_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _r: TsRange,
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
}

const HOUR: i64 = 3_600_000;

/// Ten flat hourly bars at 100.0.
fn flat_bars() -> Vec<Bar> {
    (0..10)
        .map(|i| Bar {
            ts: i * HOUR,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect()
}

/// `n` hourly bars rising by 0.1 from 100.0, open == high == low == close — what a walk-forward
/// needs: each window's entry bar keeps equity exactly at `cash` (the stitch's `debug_assert`), and
/// a long `buy_hold` earns a finite, size-linear return, so a searched window always has a winner.
fn rising_bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let px = 100.0 + i as f64 * 0.1;
            Bar {
                ts: i as i64 * HOUR,
                open: px,
                high: px,
                low: px,
                close: px,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect()
}

/// Spawn the compute server over a fresh [`SlowCountingStore`] holding [`flat_bars`]; returns its
/// address and the store's read counter.
fn spawn_server() -> (SocketAddr, Arc<AtomicUsize>) {
    spawn_server_with(flat_bars())
}

/// [`spawn_server`] over a store that hands back `bars`.
fn spawn_server_with(bars: Vec<Bar>) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let reads = Arc::new(AtomicUsize::new(0));
    let store: Arc<dyn HistStore + Send + Sync> =
        Arc::new(SlowCountingStore { bars, reads: Arc::clone(&reads) });
    thread::Builder::new()
        .name("cancel-test-serve".into())
        .spawn(move || {
            let _ = serve(listener, store);
        })
        .expect("spawn the test server");
    (addr, reads)
}

/// A bar-mode `buy_hold` profile with a `points`-long `[paramscan]` grid over `size`.
fn paramscan_request(points: usize) -> Request {
    let grid: Vec<String> = (1..=points).map(|i| format!("{i}.0")).collect();
    let profile_toml = format!(
        r#"
name = "cancel_probe"

[data]
venue = "test"
symbols = ["TESTUSDT"]
kind = "bar"
interval = "1h"
from = "0"
to = "36000000"

[engine]
cash = 1000000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "TESTUSDT"

[paramscan]
size = [{}]
"#,
        grid.join(", ")
    );
    Request::RunParamscanProfile { profile_toml, rank_by: None, search: None }
}

/// Poll `reads` until it has not moved for `quiet`, or `deadline` passes. Returns the final count
/// and when it last moved.
fn settle(reads: &AtomicUsize, quiet: Duration, deadline: Instant) -> (usize, Instant) {
    let mut last = reads.load(Ordering::SeqCst);
    let mut moved_at = Instant::now();
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
        let now = reads.load(Ordering::SeqCst);
        if now != last {
            last = now;
            moved_at = Instant::now();
        } else if moved_at.elapsed() >= quiet {
            break;
        }
    }
    (last, moved_at)
}

/// The CONTROL: a client that stays connected gets its report, with the watch armed for the whole
/// search. The grid is eight points over a store that costs [`READ_DELAY`] a read, so the search
/// outlives at least one watch interval and a watch that misread a live client as gone would turn
/// this answer into the cancellation.
#[test]
fn a_client_that_stays_gets_its_report() {
    let (addr, reads) = spawn_server();
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &paramscan_request(8)).expect("send the search");
    match read_frame::<_, Response>(&mut stream).expect("the answer") {
        Response::ParamscanReport(json) => {
            assert!(!json.contains("cancelled"), "a live client's search was cancelled: {json}");
        }
        other => panic!("expected a ParamscanReport, got {other:?}"),
    }
    assert!(reads.load(Ordering::SeqCst) >= 8, "every point reads the store at least once");
    // The connection survives the watched verb: the socket is back in the mode the loop reads in.
    write_frame(&mut stream, &Request::Ping).expect("ping on the same connection");
    match read_frame::<_, Response>(&mut stream).expect("the pong") {
        Response::Pong => {}
        other => panic!("expected Pong after a watched search, got {other:?}"),
    }
}

/// A client that HALF-closes (shuts its write side, keeps reading) is gone as far as the daemon can
/// tell, and is told so: the answer is a `Response::Error` naming the cancellation, it arrives well
/// inside [`STOP_BUDGET`] where the full grid would take five seconds or more, the store stops
/// being read, and then the daemon closes the connection.
#[test]
fn a_half_closed_client_is_answered_cancelled_and_the_search_stops() {
    let (addr, reads) = spawn_server();
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(30))).expect("client read timeout");
    write_frame(&mut stream, &paramscan_request(BIG_GRID)).expect("send the search");
    thread::sleep(Duration::from_millis(400));
    assert!(reads.load(Ordering::SeqCst) > 0, "guard: the search started before the client left");

    let left_at = Instant::now();
    stream.shutdown(Shutdown::Write).expect("half-close");
    match read_frame::<_, Response>(&mut stream).expect("the cancellation answer") {
        Response::Error(msg) => assert!(msg.contains("cancelled"), "names the cancellation: {msg}"),
        other => panic!("expected Response::Error(cancelled), got {other:?}"),
    }
    let answered_in = left_at.elapsed();
    assert!(answered_in < STOP_BUDGET, "answered {answered_in:?} after the client left");

    let (final_reads, _) = settle(&reads, Duration::from_millis(700), left_at + STOP_BUDGET * 2);
    assert!(
        final_reads < BIG_GRID,
        "the search kept reading the store after its client left: {final_reads} reads (a full run \
         is at least {BIG_GRID})"
    );
    let mut rest = Vec::new();
    let n = stream.read_to_end(&mut rest).expect("read to the daemon's close");
    assert_eq!(n, 0, "nothing follows the cancellation; the daemon closes the connection");
}

/// The path an MCP client actually takes: it CLOSES the socket and reads nothing back. The daemon
/// must stop scheduling points within [`STOP_BUDGET`] of the close, and keep serving other clients.
#[test]
fn a_closed_client_stops_the_search_within_the_budget() {
    let (addr, reads) = spawn_server();
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &paramscan_request(BIG_GRID)).expect("send the search");
    thread::sleep(Duration::from_millis(400));
    assert!(reads.load(Ordering::SeqCst) > 0, "guard: the search started before the client left");

    let left_at = Instant::now();
    drop(stream);
    let (final_reads, last_read_at) =
        settle(&reads, Duration::from_millis(700), left_at + STOP_BUDGET * 2);
    let stopped_in = last_read_at.saturating_duration_since(left_at);
    assert!(
        stopped_in < STOP_BUDGET,
        "the store was still being read {stopped_in:?} after the close"
    );
    assert!(
        final_reads < BIG_GRID,
        "the search kept reading the store after its client left: {final_reads} reads (a full run \
         is at least {BIG_GRID})"
    );

    let mut client2 = TcpStream::connect(addr).expect("a second client connects");
    write_frame(&mut client2, &Request::Ping).expect("ping");
    match read_frame::<_, Response>(&mut client2).expect("pong") {
        Response::Pong => {}
        other => panic!("expected Pong, got {other:?}"),
    }
}

// ── RunBacktest: one run, one store scan per symbol ─────────────────────────────────────────

/// A bar-mode `buy_hold` backtest over `symbols` symbols (`S0`, `S1`, …): one `load_bars` per
/// symbol, so a full run costs exactly that many reads, one after another.
fn backtest_request(symbols: usize) -> Request {
    let names: Vec<String> = (0..symbols).map(|i| format!("\"S{i}\"")).collect();
    Request::RunBacktest(format!(
        r#"
name = "cancel_probe_backtest"

[data]
venue = "test"
symbols = [{}]
kind = "bar"
interval = "1h"
from = "0"
to = "36000000"

[engine]
cash = 1000000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "S0"
"#,
        names.join(", ")
    ))
}

/// The CONTROL for `RunBacktest`: a client that stays gets its report, through a watch armed for
/// all eight series loads (eight [`READ_DELAY`]s, four watch intervals), and the connection keeps
/// serving afterwards.
#[test]
fn a_backtest_client_that_stays_gets_its_report() {
    let (addr, reads) = spawn_server();
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &backtest_request(8)).expect("send the backtest");
    match read_frame::<_, Response>(&mut stream).expect("the answer") {
        Response::Report(json) => {
            assert!(!json.contains("cancelled"), "a live client's run was cancelled: {json}");
        }
        other => panic!("expected a Report, got {other:?}"),
    }
    assert!(reads.load(Ordering::SeqCst) >= 8, "every symbol is one store read");
    write_frame(&mut stream, &Request::Ping).expect("ping on the same connection");
    match read_frame::<_, Response>(&mut stream).expect("the pong") {
        Response::Pong => {}
        other => panic!("expected Pong after a watched backtest, got {other:?}"),
    }
}

/// A backtest client that CLOSES mid-load stops the series loads it has not started: a 60-symbol
/// run is 60 reads and six seconds, and the store must go quiet within [`STOP_BUDGET`] of the close
/// having served fewer.
#[test]
fn a_closed_backtest_client_stops_the_series_loads() {
    const SYMBOLS: usize = 60;
    let (addr, reads) = spawn_server();
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &backtest_request(SYMBOLS)).expect("send the backtest");
    thread::sleep(Duration::from_millis(400));
    assert!(reads.load(Ordering::SeqCst) > 0, "guard: the run started before the client left");

    let left_at = Instant::now();
    drop(stream);
    let (final_reads, last_read_at) =
        settle(&reads, Duration::from_millis(700), left_at + STOP_BUDGET * 2);
    let stopped_in = last_read_at.saturating_duration_since(left_at);
    assert!(
        stopped_in < STOP_BUDGET,
        "the store was still being read {stopped_in:?} after the close"
    );
    assert!(
        final_reads < SYMBOLS,
        "the backtest kept loading series after its client left: {final_reads} reads (a full run \
         is {SYMBOLS})"
    );
}

// ── RunWalkforwardProfile: once per window, and once per training-half point ────────────────

/// Bars every walk-forward test here walks: 244 rising hourly bars.
const WF_BARS: usize = 244;

/// A `snap_to_properties` bar walk over [`rising_bars`]: every engine run's fill asks the store's
/// properties grid once, so each window — and each training-half point of a searched walk — is at
/// least one [`READ_DELAY`] read. `search` appends the searched route: `search = "sweep"` with a
/// `points`-long `size` grid ranked by return.
fn walkforward_request(n_splits: usize, search: Option<usize>) -> Request {
    let (search_key, grid) = match search {
        Some(points) => {
            let sizes: Vec<String> = (1..=points).map(|i| format!("{i}.0")).collect();
            (
                "search = \"sweep\"\nrank_by = \"return\"\n".to_string(),
                format!("\n[sweep]\nsize = [{}]\n", sizes.join(", ")),
            )
        }
        None => (String::new(), String::new()),
    };
    let profile_toml = format!(
        r#"
name = "cancel_probe_walkforward"

[data]
venue = "test"
symbols = ["TESTUSDT"]
kind = "bar"
interval = "1h"
from = "0"
to = "900000000"

[engine]
cash = 1000000.0
snap_to_properties = true

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "TESTUSDT"

[walkforward]
n_splits = {n_splits}
{search_key}{grid}"#
    );
    Request::RunWalkforwardProfile { profile_toml }
}

/// The CONTROL for the FIXED walk: a client that stays gets the stitched report, and the run
/// measurably did one read per window — which is what makes the cancel tests below non-vacuous.
#[test]
fn a_walkforward_client_that_stays_gets_its_report() {
    const SPLITS: usize = 8;
    let (addr, reads) = spawn_server_with(rising_bars(WF_BARS));
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &walkforward_request(SPLITS, None)).expect("send the walk");
    match read_frame::<_, Response>(&mut stream).expect("the answer") {
        Response::WalkforwardReport(json) => {
            assert!(json.contains("oos_return"), "a stitched report: {json}");
        }
        other => panic!("expected a WalkforwardReport, got {other:?}"),
    }
    let n = reads.load(Ordering::SeqCst);
    assert!(n > SPLITS, "one bar load plus at least one properties read per window, got {n}");
    write_frame(&mut stream, &Request::Ping).expect("ping on the same connection");
    match read_frame::<_, Response>(&mut stream).expect("the pong") {
        Response::Pong => {}
        other => panic!("expected Pong after a watched walk, got {other:?}"),
    }
}

/// A walk-forward client that HALF-closes is answered with the cancellation, the windows it has not
/// reached run nothing, and the daemon closes the connection. Sixty windows are at least six
/// seconds of reads.
#[test]
fn a_half_closed_walkforward_client_is_answered_cancelled_and_the_walk_stops() {
    const SPLITS: usize = 60;
    let (addr, reads) = spawn_server_with(rising_bars(WF_BARS));
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(30))).expect("client read timeout");
    write_frame(&mut stream, &walkforward_request(SPLITS, None)).expect("send the walk");
    thread::sleep(Duration::from_millis(400));
    assert!(
        reads.load(Ordering::SeqCst) >= 2,
        "guard: the walk was past its bar load and into its windows before the client left"
    );

    let left_at = Instant::now();
    stream.shutdown(Shutdown::Write).expect("half-close");
    match read_frame::<_, Response>(&mut stream).expect("the cancellation answer") {
        Response::Error(msg) => assert!(msg.contains("cancelled"), "names the cancellation: {msg}"),
        other => panic!("expected Response::Error(cancelled), got {other:?}"),
    }
    let answered_in = left_at.elapsed();
    assert!(answered_in < STOP_BUDGET, "answered {answered_in:?} after the client left");

    let (final_reads, _) = settle(&reads, Duration::from_millis(700), left_at + STOP_BUDGET * 2);
    assert!(
        final_reads < SPLITS,
        "the walk kept running windows after its client left: {final_reads} reads (a full walk is \
         more than {SPLITS})"
    );
    let mut rest = Vec::new();
    let n = stream.read_to_end(&mut rest).expect("read to the daemon's close");
    assert_eq!(n, 0, "nothing follows the cancellation; the daemon closes the connection");
}

/// The CONTROL for the SEARCHED walk: a client that stays gets a report whose windows carry what
/// they chose, after one read per training-half point and per window.
#[test]
fn a_searched_walkforward_client_that_stays_gets_its_report() {
    const SPLITS: usize = 2;
    const POINTS: usize = 4;
    let (addr, reads) = spawn_server_with(rising_bars(WF_BARS));
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &walkforward_request(SPLITS, Some(POINTS))).expect("send the walk");
    match read_frame::<_, Response>(&mut stream).expect("the answer") {
        Response::WalkforwardReport(json) => {
            assert!(json.contains("chosen_params"), "a searched window records its winner: {json}");
        }
        other => panic!("expected a WalkforwardReport, got {other:?}"),
    }
    let n = reads.load(Ordering::SeqCst);
    assert!(
        n > SPLITS * POINTS + SPLITS,
        "one bar load, a read per training-half point and per validation run, got {n}"
    );
}

/// A SEARCHED walk-forward client that closes stops the per-window search: four windows over a
/// fifty-point grid are at least two hundred training-half reads (five seconds over four workers).
#[test]
fn a_closed_searched_walkforward_client_stops_scoring_points() {
    const SPLITS: usize = 4;
    const POINTS: usize = 50;
    let (addr, reads) = spawn_server_with(rising_bars(WF_BARS));
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &walkforward_request(SPLITS, Some(POINTS))).expect("send the walk");
    thread::sleep(Duration::from_millis(400));
    assert!(reads.load(Ordering::SeqCst) >= 2, "guard: the search started before the client left");

    let left_at = Instant::now();
    drop(stream);
    let (final_reads, last_read_at) =
        settle(&reads, Duration::from_millis(700), left_at + STOP_BUDGET * 2);
    let stopped_in = last_read_at.saturating_duration_since(left_at);
    assert!(
        stopped_in < STOP_BUDGET,
        "the store was still being read {stopped_in:?} after the close"
    );
    assert!(
        final_reads < SPLITS * POINTS,
        "the walk kept scoring points after its client left: {final_reads} reads (a full walk is \
         more than {})",
        SPLITS * POINTS
    );
}

// ── The blind spot, PINNED ───────────────────────────────────────────────────────────────────

/// **A close behind pipelined bytes is NOT seen, and this test says so on purpose.** The client
/// sends a 40-point search, then a `Ping` before the search's answer, then closes. The `Ping` sits
/// unread in the daemon's receive queue, and `PeerWatch`'s non-consuming `peek` returns it every
/// time and never reaches the FIN behind it (`PeerWatch`'s doc argues why std offers no way past),
/// so the search runs to its last point.
///
/// A watch that SAW the close would stop near `(close + PEER_POLL) / READ_DELAY x 4` reads, about
/// twenty. If this test goes red because the read count fell short, the blind spot is gone: flip it
/// into a cancel test rather than loosening it.
#[test]
fn a_close_behind_a_pipelined_request_is_not_seen() {
    const POINTS: usize = 40;
    let (addr, reads) = spawn_server();
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &paramscan_request(POINTS)).expect("send the search");
    write_frame(&mut stream, &Request::Ping).expect("pipeline a second request behind it");
    thread::sleep(Duration::from_millis(300));
    assert!(reads.load(Ordering::SeqCst) > 0, "guard: the search started before the client left");

    let left_at = Instant::now();
    drop(stream);
    let (final_reads, _) = settle(&reads, Duration::from_millis(700), left_at + STOP_BUDGET * 4);
    assert!(
        final_reads >= POINTS,
        "the search stopped after {final_reads} of {POINTS} reads — the daemon SAW a close behind \
         a pipelined request, which `PeerWatch`'s doc says it cannot; update both"
    );
}
