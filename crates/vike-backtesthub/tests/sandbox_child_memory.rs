//! **What the sandboxed child costs in memory for one backtest** of 263,521 one-minute bars (the the CI box reference run
//! of 2026-11-19: six months of binance BTCUSDT). A child PROCESS, as a hub starts one per run, answers the summarized
//! Studio run a client asks through `vike_backtesthub::strategy_sandbox::serve_child` over its stdin and stderr (its
//! stdout is the test harness's), its history piped by this process's relay (`strategy_sandbox::relay_over`), its
//! strategy a compiled-in stand-in for a file. The child prints its peak resident set (`VmHWM`) and its
//! minor page faults (each a fresh 4 KiB page), the two numbers measured on the CI box, and this process samples its
//! anonymous resident set for the peak of its heap (`VmHWM` counts this binary's own pages too); it prints them with a
//! digest of the run's trades and whole equity curve, which a change that must not move a number keeps. A TICK run
//! of one and of seven days is measured the same way (`measure_the_sandbox_tick_child`).
//!
//! Counted from `/proc`, not by a counting allocator: a `GlobalAlloc` is an `unsafe impl`, which the workspace's
//! `unsafe_code = "forbid"` refuses in this crate (`crates/vike-journal/src/writer_tests.rs` counts faults the same
//! way for the same reason).
#![cfg(target_os = "linux")]

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vike_backtesthub::strategy_sandbox::{relay_over, serve_child};
use vike_backtesthub::studio_run::StrategyFileBuild;
use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::wire_studio::{DataSliceWire, ReplayKindWire, StrategySpecWire};
use vike_data::{HistoryStore, HistoryStoreError, HistoryStoreMemory, TimestampRange};
use vike_marketdata::{Bar, TickQuote, TickTrade};
use vike_model::params::Params;
use vike_model::{OrderBroker, OrderPlacement, Strategy};
use vike_simulator::OrderBrokerSimulator;

/// The the CI box reference run's length: binance BTCUSDT 1m, 2026-04-27..2026-10-27.
const BARS: usize = 263_521;

/// The file the run names, and the text the hub read for it: [`Pulse`]'s header.
const FILE: &str = "other/pulse.rs";
const TEXT: &str = "//! Pulse: flips side every `every` bars.\n//!\n//! ```toml\n//! [strategy]\n\
                    //! runs_on = \"backtest\"\n//!\n//! [params.every]\n//! type = \"integer\"\n\
                    //! default = 7\n//! ```\n";

/// An argument no test is named, which tells [`the_child`] that [`measure_the_sandbox_child`] started it.
/// The build both ends of the child's pipe state here (the binary hands in `vike_buildinfo::summary`).
const BUILD: &str = "sandbox_child_memory";
const CHILD_MARK: &str = "vike-sandbox-child-of-the-measurement";

/// What the child prints its two numbers after.
const CHILD_LINE: &str = "sandbox-child-memory:";

/// The strategy the file stands for, compiled in: a market order every `every` bars (on a tick run, every `every`
/// quotes), each the other side, so the run trades all along the walk.
struct Pulse {
    every: usize,
    seen: usize,
    long: bool,
}

impl Pulse {
    /// One more bar or quote of `symbol`: every `every`th turns the position.
    fn step<B: OrderBroker>(&mut self, broker: &mut B, symbol: &str) {
        self.seen += 1;
        if !self.seen.is_multiple_of(self.every) {
            return;
        }
        broker.place(
            symbol,
            "",
            if self.long { -1 } else { 1 },
            1.0,
            OrderPlacement::Market,
            false,
        );
        self.long = !self.long;
    }
}

impl<B: OrderBroker> Strategy<B> for Pulse {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        if let Some(symbol) = bar.symbol.as_deref() {
            self.step(broker, symbol);
        }
    }

    fn on_quote_tick(&mut self, broker: &mut B, quote: &TickQuote) {
        self.step(broker, &quote.symbol);
    }
}

/// The stand-in the child loads in place of the built artifact.
fn the_file() -> StrategyFileBuild {
    Box::new(|params_toml: &str, _facts: &str| {
        let params: Params = toml::from_str(params_toml).map_err(|e| e.to_string())?;
        let every = params.get("every").and_then(|v| v.as_integer()).unwrap_or(7).max(1) as usize;
        Ok(Box::new(Pulse { every, seen: 0, long: false })
            as Box<dyn Strategy<OrderBrokerSimulator>>)
    })
}

/// A pipe end that counts the bytes through it: what the run's frames cost each way.
struct Counted<P>(P, u64);

impl<W: std::io::Write> std::io::Write for Counted<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.0.write(bytes)?;
        self.1 += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let n = self.0.read(bytes)?;
        self.1 += n as u64;
        Ok(n)
    }
}

/// `field` of `/proc/<process>/status` (`self`, or a pid), in KiB.
fn status_kib(process: &str, field: &str) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{process}/status")).unwrap_or_default();
    let line = status.lines().find(|l| l.starts_with(field)).unwrap_or_default();
    line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// Minor faults so far, field 10 of `/proc/self/stat` (read after the LAST `)`: the comm may hold one).
fn minor_faults() -> u64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let tail = stat.rfind(')').map_or("", |at| &stat[at + 1..]);
    tail.split_whitespace().nth(7).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// The child's half, started by [`measure_the_sandbox_child`] as a process of this binary: one request on stdin, the
/// protocol's frames on stderr, then its two numbers on stdout. Run any other way (among every ignored test), it
/// answers nothing.
#[test]
#[ignore = "the child half of measure_the_sandbox_child, which starts it"]
fn the_child() {
    if !std::env::args().any(|a| a == CHILD_MARK) {
        return;
    }
    let code = serve_child(
        Box::new(std::io::stdin()),
        Box::new(std::io::stderr()),
        BUILD,
        vec![(Ok(the_file()), None)],
    );
    println!("{CHILD_LINE} {code:?} {} {}", status_kib("self", "VmHWM:"), minor_faults());
}

/// This binary started again as [`the_child`], its stdin, stdout and stderr piped.
fn start_the_child() -> std::process::Child {
    let program = std::env::current_exe().expect("this test binary");
    Command::new(program)
        .args(["the_child", "--exact", "--ignored", "--nocapture", CHILD_MARK])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the child")
}

#[test]
#[ignore = "a measurement: cargo test -p vike-backtesthub --test sandbox_child_memory -- --ignored --nocapture --exact measure_the_sandbox_child"]
fn measure_the_sandbox_child() {
    let store = HistoryStoreMemory::new();
    let bars =
        vike_studio_core::test_support::walk::random_walk_bars(BARS, 60_000, Some("BTCUSDT"));
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("seed");
    drop(bars);
    let request = Request::Summarized(Box::new(Request::RunSlice {
        spec: StrategySpecWire::File {
            file: FILE.to_string(),
            params_toml: "every = 5\n".into(),
            sdk: None,
        },
        slice: Box::new(DataSliceWire {
            venue: "binance".to_string(),
            symbols: vec!["BTCUSDT".to_string()],
            interval: "1m".to_string(),
            start: None,
            end: None,
            kind: ReplayKindWire::Bars,
        }),
        params: None,
    }));
    let run = in_a_child(&request, &store);
    assert!(run.trades.len() > 1_000, "the run trades all along the walk");
    assert_eq!(run.curve.equity.len(), BARS, "one curve step a bar");
    run.print(&format!("{BARS} bars"));
}

/// A summarized run answered by a child process, with what it cost.
struct InAChild {
    trades: Vec<vike_model::Trade>,
    curve: vike_runs::RunEquityCurve,
    /// The trades, the metrics and the whole curve, floats by their bits (the trades' through `Debug`'s exact text).
    digest: u64,
    /// The child's heap at its peak (`RssAnon`, KiB), and its shape over the run: the most of each twentieth, MB.
    peak_anon: u64,
    shape: Vec<u64>,
    /// The child's own peak resident set (`VmHWM`, KiB) and its minor faults.
    hwm: u64,
    faults: u64,
    /// The bytes down the pipe (the history) and up it (the reads, the held parts, the answer), and the relay's time.
    down: u64,
    up: u64,
    relayed: Duration,
}

impl InAChild {
    fn print(&self, what: &str) {
        println!(
            "sandbox child, {what}: peak heap (RssAnon) {:.1} MB, peak resident {:.1} MB, {} minor \
             faults ({:.1} MB of fresh pages)\n\
             heap over the run, MB: {:?}\n\
             pipe: {} B down, {} B up, the run relayed in {:?}\n\
             result: {} trades, {} curve steps, final equity {:?}, digest {:016x}",
            self.peak_anon as f64 / 1024.0,
            self.hwm as f64 / 1024.0,
            self.faults,
            self.faults as f64 * 4.0 / 1024.0,
            self.shape,
            self.down,
            self.up,
            self.relayed,
            self.trades.len(),
            self.curve.equity.len(),
            self.curve.equity.last(),
            self.digest,
        );
    }
}

/// The summarized `request` answered by [`the_child`] in a process of its own, its history relayed from `store`.
fn in_a_child(request: &Request, store: &(dyn HistoryStore + Send + Sync)) -> InAChild {
    let mut child = start_the_child();
    let (mut to_child, mut from_child) = (
        Counted(child.stdin.take().expect("the child's stdin"), 0),
        Counted(child.stderr.take().expect("the child's stderr"), 0),
    );
    // The child's heap at its peak: its anonymous resident set (`RssAnon`, the binary's own pages left out),
    // sampled every millisecond until its answer is in, and its shape over the run: the most of each twentieth.
    let (pid, done) = (child.id().to_string(), AtomicBool::new(false));
    let started = std::time::Instant::now();
    let (answered, samples) = std::thread::scope(|scope| {
        let sampler = scope.spawn(|| {
            let mut samples = Vec::new();
            while !done.load(Ordering::Relaxed) {
                samples.push(status_kib(&pid, "RssAnon:"));
                std::thread::sleep(Duration::from_millis(1));
            }
            samples
        });
        let answered = relay_over(&mut to_child, &mut from_child, BUILD, request, TEXT, store);
        done.store(true, Ordering::Relaxed);
        (answered, sampler.join().expect("the sampler"))
    });
    let relayed = started.elapsed();
    let (down, up) = (to_child.1, from_child.1);
    drop(to_child);
    let mut said = String::new();
    child.stdout.take().expect("the child's stdout").read_to_string(&mut said).expect("read");
    assert!(child.wait().expect("reap").success(), "the child failed: {said}");
    let (response, trades, curve) = answered.expect("the child answered");
    let Response::RunSummary(summary) = response else { panic!("not a summary: {response:?}") };
    let peak_anon = samples.iter().copied().max().unwrap_or(0);
    let shape: Vec<u64> = samples
        .chunks(samples.len().div_ceil(20).max(1))
        .map(|part| part.iter().max().copied().unwrap_or(0) / 1024)
        .collect();

    // Anywhere in its line: a libtest running one thread prints `test the_child ... ` in front of it.
    let numbers = said
        .lines()
        .find_map(|l| l.split_once(CHILD_LINE).map(|(_, numbers)| numbers))
        .expect("the child's line");
    let [_, hwm, faults]: [&str; 3] =
        numbers.split_whitespace().collect::<Vec<_>>().try_into().expect("three words");
    let (hwm, faults): (u64, u64) = (hwm.parse().expect("kib"), faults.parse().expect("faults"));
    let mut digest = DefaultHasher::new();
    format!("{trades:?}").hash(&mut digest);
    summary.rows.hash(&mut digest);
    curve.timestamp.hash(&mut digest);
    curve.equity.iter().for_each(|v| v.to_bits().hash(&mut digest));
    InAChild {
        trades,
        curve,
        digest: digest.finish(),
        peak_anon,
        shape,
        hwm,
        faults,
        down,
        up,
        relayed,
    }
}

/// A tick tape's rate: a quote every [`QUOTE_EVERY_MS`] and a trade every [`TRADE_EVERY_MS`], about a tenth of binance
/// BTCUSDT's book ticker, so a seven-day run fits a lane; both sides of the pipe scale with it alike.
const QUOTE_EVERY_MS: i64 = 200;
const TRADE_EVERY_MS: i64 = 1_000;

/// 2026-04-27T00:00Z, where the tape opens.
const TAPE_START: i64 = 1_777_248_000_000;

/// `days` of binance BTCUSDT quotes and trades from [`TAPE_START`] at the tape's rate: a seeded walk, each row with its
/// receive stamp, the sizes moving, a trade's side alternating by the walk. Priced near 100, so the slice's default
/// cash buys the one unit [`Pulse`] trades.
fn tape(days: i64) -> (Vec<TickQuote>, Vec<TickTrade>) {
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut step = move || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as f64 / f64::from(1u32 << 31) - 0.5
    };
    let span = days * 86_400_000;
    let mut mid = 100.0;
    let quotes: Vec<TickQuote> = (0..span / QUOTE_EVERY_MS)
        .map(|i| {
            mid += step() * 0.01;
            let timestamp = TAPE_START + i * QUOTE_EVERY_MS;
            TickQuote {
                timestamp,
                local_timestamp: timestamp + 3,
                bid: mid - 0.005,
                ask: mid + 0.005,
                bid_size: 1.0 + step().abs(),
                ask_size: 1.0 + step().abs(),
                symbol: "BTCUSDT".into(),
            }
        })
        .collect();
    let trades: Vec<TickTrade> = (0..span / TRADE_EVERY_MS)
        .map(|i| {
            let quote = &quotes[(i * TRADE_EVERY_MS / QUOTE_EVERY_MS) as usize];
            let is_buyer_maker = step() < 0.0;
            TickTrade {
                timestamp: quote.timestamp + 1,
                local_timestamp: quote.timestamp + 4,
                price: if is_buyer_maker { quote.bid } else { quote.ask },
                size: 0.001 + step().abs() / 10.0,
                is_buyer_maker,
                symbol: "BTCUSDT".into(),
            }
        })
        .collect();
    (quotes, trades)
}

/// A hub's store holding one binance BTCUSDT tick tape.
struct Tape {
    quotes: Vec<TickQuote>,
    trades: Vec<TickTrade>,
}

/// `rows` whose stamp `at` is inside `range` (both bounds inclusive), as a store answers them.
fn within<T: Clone>(rows: &[T], range: TimestampRange, at: impl Fn(&T) -> i64) -> Vec<T> {
    let inside = |ts: i64| range.start.is_none_or(|s| ts >= s) && range.end.is_none_or(|e| ts <= e);
    rows.iter().filter(|r| inside(at(r))).cloned().collect()
}

impl HistoryStore for Tape {
    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        let ours = venue == "binance" && symbol == "BTCUSDT";
        Ok(if ours { within(&self.quotes, range, |q| q.timestamp) } else { Vec::new() })
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        let ours = venue == "binance" && symbol == "BTCUSDT";
        Ok(if ours { within(&self.trades, range, |t| t.timestamp) } else { Vec::new() })
    }

    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// **What the sandboxed child costs for a TICK run**: a summarized Studio tick run of one day and of seven days of
/// binance BTCUSDT quotes and trades ([`tape`]), its rows piped by this process's relay. The child's heap at its peak,
/// its fresh pages, the bytes each way and the relay's time, with the result's digest, which the tick rows' codec
/// must not move.
#[test]
#[ignore = "a measurement: cargo test -p vike-backtesthub --release --test sandbox_child_memory -- --ignored --nocapture --exact measure_the_sandbox_tick_child"]
fn measure_the_sandbox_tick_child() {
    for days in [1, 7] {
        let (quotes, trades) = tape(days);
        let rows = (quotes.len(), trades.len());
        let store = Tape { quotes, trades };
        let request = Request::Summarized(Box::new(Request::RunSlice {
            spec: StrategySpecWire::File {
                file: FILE.to_string(),
                params_toml: "every = 500\n".into(),
                sdk: None,
            },
            slice: Box::new(DataSliceWire {
                venue: "binance".to_string(),
                symbols: vec!["BTCUSDT".to_string()],
                interval: "1m".to_string(),
                start: None,
                end: None,
                kind: ReplayKindWire::Ticks,
            }),
            params: None,
        }));
        let run = in_a_child(&request, &store);
        run.print(&format!("{days}-day tick run, {} quotes and {} trades", rows.0, rows.1));
        assert!(!run.trades.is_empty(), "the run trades along the tape");
    }
}

/// The kept lists a the CI box datahub serves (2026-11-19), rows by venue: 23,799 with the binance row added below.
const KEPT_ROWS: [(&str, usize); 13] = [
    ("aster", 672),
    ("binance", 2_210),
    ("bybit", 1_464),
    ("ctrader", 365),
    ("deribit", 5_233),
    ("dukascopy", 945),
    ("fxcm", 497),
    ("hyperliquid", 588),
    ("ibkr", 1),
    ("ig", 6_514),
    ("oanda", 68),
    ("okx", 5_041),
    ("polymarket", 200),
];

/// **What the kept instrument lists cost the child** in the the CI box reference case: a PROFILE backtest of a strategy
/// file on binance BTCUSDT 1m (precision_confluence's run), its store serving every venue's kept rows in the CI box's
/// numbers. The child's fresh pages, its peak resident set and the run's time, with the result's digest.
#[test]
#[ignore = "a measurement: cargo test -p vike-backtesthub --release --test sandbox_child_memory -- --ignored --nocapture --exact measure_the_kept_rows_a_child_reads"]
fn measure_the_kept_rows_a_child_reads() {
    let store = HistoryStoreMemory::new();
    let bars =
        vike_studio_core::test_support::walk::random_walk_bars(BARS, 60_000, Some("BTCUSDT"));
    let (from, to) = (bars[0].timestamp, bars[BARS - 1].timestamp);
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("seed");
    drop(bars);
    let row = |venue: &str, symbol: String, base: String| {
        vike_model::Instrument::new(venue, symbol, base, "USDT", vike_model::Contract::CryptoSpot)
    };
    let mut rows = vec![row("binance", "BTCUSDT".into(), "BTC".into())];
    for (venue, n) in KEPT_ROWS {
        rows.extend((0..n).map(|i| row(venue, format!("S{i}USDT"), format!("S{i}"))));
    }
    let rows = vike_catalog::spelled(rows);
    let (count, bytes) = (rows.len(), rmp_serde::to_vec(&rows).expect("msgpack").len());
    store.serve_kept_instruments(rows);
    let profile = format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"{from}\"\nto = \"{to}\"\n\n[engine]\ncash = 1000000.0\n\n[strategy]\n\
         file = \"{FILE}\"\n[strategy.params]\nevery = 5\n"
    );
    let request = Request::Summarized(Box::new(Request::RunBacktest(profile)));
    let mut child = start_the_child();
    let (mut to_child, mut from_child) = (
        child.stdin.take().expect("the child's stdin"),
        child.stderr.take().expect("the child's stderr"),
    );
    let started = std::time::Instant::now();
    let answered = relay_over(&mut to_child, &mut from_child, BUILD, &request, TEXT, &store);
    let took = started.elapsed();
    drop(to_child);
    let mut said = String::new();
    child.stdout.take().expect("the child's stdout").read_to_string(&mut said).expect("read");
    assert!(child.wait().expect("reap").success(), "the child failed: {said}");
    let (response, trades, curve) = answered.expect("the child answered");
    assert!(!matches!(response, Response::Error(_)), "the run answered: {response:?}");
    let numbers = said.lines().find_map(|l| l.strip_prefix(CHILD_LINE)).expect("the child's line");
    let [_, hwm, faults]: [&str; 3] =
        numbers.split_whitespace().collect::<Vec<_>>().try_into().expect("three words");
    let (hwm, faults): (u64, u64) = (hwm.parse().expect("kib"), faults.parse().expect("faults"));
    let mut digest = DefaultHasher::new();
    format!("{trades:?}").hash(&mut digest);
    curve.timestamp.hash(&mut digest);
    curve.equity.iter().for_each(|v| v.to_bits().hash(&mut digest));
    println!(
        "kept lists served: {count} rows, {:.1} MB as msgpack. Profile run of {BARS} bars in a child: {faults} \
         minor faults ({:.1} MB of fresh pages), peak resident {:.1} MB, run {took:?}\n\
         result: {} trades, {} curve steps, final equity {:?}, digest {:016x}",
        bytes as f64 / 1e6,
        faults as f64 * 4.0 / 1024.0,
        hwm as f64 / 1024.0,
        trades.len(),
        curve.equity.len(),
        curve.equity.last(),
        digest.finish(),
    );
    assert!(trades.len() > 1_000, "the run trades all along the walk");
}
