//! The HOP, measured: how long a venue event takes from the daemon's sink to the datahub-side sink.
//!
//! A scripted venue client emits one event on the gatewayhub's wire sink (the call a venue pump makes);
//! the datahub's `RemoteDataClient` delivers it, on its reader thread, into a sink that stamps the
//! receipt. The difference is the cost of the hop: the daemon's queue, the writer thread, `serde_json`
//! out, the loopback socket, `serde_json` in, the reader. It is the number wave 1 puts in the PR body
//! and gates nothing on (the core-hop gate of `crates/vike-core/tests/runtime_latency.rs` covers only
//! the hop INSIDE the core, and it does not see this one).
//!
//! # Not a gate
//!
//! `#[ignore]`d, in the style of `crates/vike-datahub/tests/wire_latency.rs`'s loopback timing, but
//! asserting NOTHING about time: an uncalibrated ceiling is a flaky gate, and a ceiling means
//! somebody derived one. The one thing it asserts is that a PACED event (depth 1) arrives: a paced hop
//! that loses one is broken, not slow. A short non-ignored run proves the plumbing so a rotted harness
//! is red. It installs no `tracing` subscriber (`vike_log::test_init` turns `debug` on): the hop is
//! measured as a daemon runs it, and only the plumbing test logs.
//!
//! ```text
//! LANE=laneN just the latency box <branch> cargo test -p vike-gatewayhub --test hop_latency --release \
//!     -- --ignored --nocapture
//! ```
//!
//! Run it on a QUIET lane box and say which: the numbers compare only on one box. A `just the latency box` lane is
//! pinned off the latency gate's cores (it runs under `taskset -c 8-27`), which the report line shows
//! (`Cpus_allowed_list`), together with the box's load before and after.
//!
//! # What is measured
//!
//! * **Verb class** - the sink verbs a venue pump actually emits, one cell each: `quote`, `trade`,
//!   `book_delta` (a raw delta), `depth_50` (a conflating 50-per-side snapshot), `book_100` (the folded
//!   book at 100 per side, the largest steady frame), `forming_bar`, `mark`, `status` and `seed_500`
//!   (a 500-bar seed, the largest lossless message).
//! * **Queue depth** - D events emitted back to back, each on its own symbol so nothing is conflated:
//!   the k-th of a burst has up to k-1 frames ahead of it in the daemon's queue. D is 1 (paced:
//!   request/response, the queue empty), 16 and 128. A latency is per EVENT, over all of a cell's rounds.
//! * **The emit cost** - what the venue pump pays for the sink call itself (`emit_*`): the daemon must
//!   never make a pump wait for its consumer (drift register R3).
//! * **A burst the session queue cannot hold** - is reported, not timed. A round whose events stop
//!   landing for [`STALL`] counts the rest `lost=` and ends the cell (`cut=roundR`). Past a session
//!   queue's byte bound (`WIRE_QUEUE_BYTES`, 4 MiB of `Payload::cost`) the daemon evicts the oldest
//!   lossless frames and owes a `Gap` (the hole, disclosed), and more than `LAPSE_BUDGET` (8)
//!   disclosures in a minute end the session (`ByeReason::TooSlow`). An evicted event never lands, and
//!   the rounds after the first loss would time what the overflow left behind, so the cell stops there.
//!   `seed_500` at depth 128 is that case by arithmetic: a `Bar` is 120 bytes on x86-64, a 500-bar seed
//!   costs about 60 KB of queue, and a session holds about 69 of them, not 128. The hole itself is the
//!   twin test's subject (`crates/vike-datahub/tests/gateway_twin.rs`), not this harness's.
//!
//! Each cell is a fresh gateway and client, `GatewayTimings::default()` and `ServerConfig::PRODUCTION`
//! (the shipped numbers), a warm-up that is discarded, then ~10,000 samples. The line per cell is greppable:
//! `GATEWAYHUB-HOP verb=quote depth=16 n=11200 lost=0 cut=none p50_ns=... p99_ns=... p999_ns=... max_ns=...`
//! (`-` for a quantile of a cell cut before it kept a sample).
//! Everything is `Instant` on one clock in one process: the daemon and the datahub side share the box.
#![cfg(feature = "test-support")]

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataSink, SinkCall};
use vike_gatewayhub::testkit::data::ScriptedData;
use vike_gatewayhub::testkit::events::{bar, delta, quote, trade};
use vike_gatewayhub::testkit::gateway::{LoopbackGateway, keys};
use vike_gatewayhub::testkit::rest::ScriptedRest;
use vike_gatewayhub_client::client::GatewayTimings;
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_market_stream::market::RemoteDataClient;
use vike_marketdata::{Bar, BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};

/// The sink verbs measured, in the order the report prints them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verb {
    Quote,
    Trade,
    BookDelta,
    Depth50,
    Book100,
    FormingBar,
    Mark,
    Status,
    Seed500,
}

const VERBS: [Verb; 9] = [
    Verb::Quote,
    Verb::Trade,
    Verb::BookDelta,
    Verb::Depth50,
    Verb::Book100,
    Verb::FormingBar,
    Verb::Mark,
    Verb::Status,
    Verb::Seed500,
];

impl Verb {
    fn name(self) -> &'static str {
        match self {
            Verb::Quote => "quote",
            Verb::Trade => "trade",
            Verb::BookDelta => "book_delta",
            Verb::Depth50 => "depth_50",
            Verb::Book100 => "book_100",
            Verb::FormingBar => "forming_bar",
            Verb::Mark => "mark",
            Verb::Status => "status",
            Verb::Seed500 => "seed_500",
        }
    }

    /// The call a venue pump would make for event `i` of `symbol`, with `i` carried in the field the
    /// receiver reads back (see [`Probe`]). Built BEFORE the send stamp is taken.
    fn call(self, symbol: &str, i: u64) -> SinkCall {
        let (venue, symbol) = ("binance".to_string(), symbol.to_string());
        let x = 100.0 + (i % 997) as f64 * 0.01 + 1e-7;
        match self {
            Verb::Quote => SinkCall::Quote { venue, quote: quote(&symbol, i as i64, x), symbol },
            Verb::Trade => SinkCall::Trade { venue, trade: trade(&symbol, i as i64, x), symbol },
            Verb::BookDelta => SinkCall::BookUpdate { venue, update: delta(&symbol, i, x), symbol },
            Verb::Depth50 => SinkCall::L2Snapshot {
                venue,
                tick_size: 0.5,
                bids: (0..50)
                    .map(|k| BookLevel::new(x - 0.5 * f64::from(k), 1.0 + f64::from(k)))
                    .collect(),
                asks: (0..50).map(|k| BookLevel::new(x + 0.5 * f64::from(k + 1), 1.5)).collect(),
                ts: i as i64,
                symbol,
            },
            Verb::Book100 => {
                let mut book = L2Book::new(0.5);
                let bids: Vec<BookLevel> = (0..100)
                    .map(|k| BookLevel::new(x - 0.5 * f64::from(k), 1.0 + f64::from(k)))
                    .collect();
                let asks: Vec<BookLevel> =
                    (0..100).map(|k| BookLevel::new(x + 0.5 * f64::from(k + 1), 1.5)).collect();
                book.apply_snapshot(i, &bids, &asks);
                SinkCall::Book { venue, symbol, book }
            }
            Verb::FormingBar => {
                SinkCall::FormingBar { venue, interval: "1m".into(), bar: bar(i as i64, x), symbol }
            }
            Verb::Mark => SinkCall::MarkTick { venue, px: x, ts: i as i64, symbol },
            Verb::Status => SinkCall::StreamStatus {
                venue,
                stream: "trades".into(),
                status: StreamStatus::GapStart { at_ts_ms: i as i64 },
                symbol,
            },
            Verb::Seed500 => SinkCall::SeedBars {
                venue,
                interval: "1m".into(),
                bars: (0..500_i64).map(|k| bar(if k == 499 { i as i64 } else { k }, x)).collect(),
                symbol,
            },
        }
    }
}

/// Play one call into `sink`, MOVING its payload (the testkit's `emit` clones the call, and a clone is
/// not what a pump pays).
fn play(sink: &dyn LiveDataSink, call: SinkCall) {
    match call {
        SinkCall::SeedBars { venue, symbol, interval, bars } => {
            sink.seed_bars(&venue, &symbol, &interval, bars);
        }
        SinkCall::CloseBar { venue, symbol, interval, bar } => {
            sink.close_bar(&venue, &symbol, &interval, bar);
        }
        SinkCall::FormingBar { venue, symbol, interval, bar } => {
            sink.forming_bar(&venue, &symbol, &interval, bar);
        }
        SinkCall::MarkTick { venue, symbol, px, ts } => sink.mark_tick(&venue, &symbol, px, ts),
        SinkCall::BarCloseTick { venue, symbol, px, ts } => {
            sink.bar_close_tick(&venue, &symbol, px, ts);
        }
        SinkCall::L2Snapshot { venue, symbol, tick_size, bids, asks, ts } => {
            sink.l2_snapshot(&venue, &symbol, tick_size, bids, asks, ts);
        }
        SinkCall::Quote { venue, symbol, quote } => sink.quote(&venue, &symbol, quote),
        SinkCall::Trade { venue, symbol, trade } => sink.trade(&venue, &symbol, trade),
        SinkCall::Book { venue, symbol, book } => sink.book(&venue, &symbol, Arc::new(book)),
        SinkCall::BookUpdate { venue, symbol, update } => sink.book_update(&venue, &symbol, update),
        SinkCall::StreamStatus { venue, symbol, stream, status } => {
            sink.stream_status(&venue, &symbol, &stream, status);
        }
        SinkCall::DataDelay { venue, symbol, stream, delay } => {
            sink.data_delay(&venue, &symbol, &stream, delay);
        }
    }
}

/// The datahub-side sink: stamps the instant each event lands, under the index the event carries.
/// It takes no lock and allocates nothing on the hot path, so it does not add to what it measures.
struct Probe {
    /// The verb being measured: the only one that counts (see [`Probe::land`]).
    verb: Verb,
    base: Instant,
    /// When event `i` was handed to the daemon's sink, in ns since `base` (+1, so 0 means "not yet").
    sent: Vec<AtomicU64>,
    /// When event `i` landed here, same unit.
    landed: Vec<AtomicU64>,
    delivered: AtomicUsize,
}

impl Probe {
    fn new(verb: Verb, events: usize) -> Arc<Self> {
        let slots = |n: usize| (0..n).map(|_| AtomicU64::new(0)).collect::<Vec<_>>();
        Arc::new(Self {
            verb,
            base: Instant::now(),
            sent: slots(events),
            landed: slots(events),
            delivered: AtomicUsize::new(0),
        })
    }

    fn now_ns(&self) -> u64 {
        self.base.elapsed().as_nanos() as u64 + 1
    }

    fn stamp_sent(&self, i: usize) {
        self.sent[i].store(self.now_ns(), Ordering::Release);
    }

    /// Event `i` of `verb` landed. Only the measured verb counts, and each event once: the client
    /// also delivers statuses of its own (a `GapStart` and a `Live` for every hole the daemon
    /// discloses), and those are not events this harness sent.
    fn land(&self, verb: Verb, i: u64) {
        if verb != self.verb {
            return;
        }
        let at = self.now_ns();
        let Some(slot) = self.landed.get(i as usize) else { return };
        if slot.compare_exchange(0, at, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            self.delivered.fetch_add(1, Ordering::Release);
        }
    }

    fn delivered(&self) -> usize {
        self.delivered.load(Ordering::Acquire)
    }

    /// Latency of event `i`, if it landed.
    fn latency(&self, i: usize) -> Option<u64> {
        let (sent, landed) =
            (self.sent[i].load(Ordering::Acquire), self.landed[i].load(Ordering::Acquire));
        (sent != 0 && landed >= sent).then(|| landed - sent)
    }
}

impl LiveDataSink for Probe {
    fn seed_bars(&self, _venue: &str, _symbol: &str, _interval: &str, bars: Vec<Bar>) {
        if let Some(last) = bars.last() {
            self.land(Verb::Seed500, last.ts as u64);
        }
    }
    fn close_bar(&self, _venue: &str, _symbol: &str, _interval: &str, _bar: Bar) {}
    fn forming_bar(&self, _venue: &str, _symbol: &str, _interval: &str, bar: Bar) {
        self.land(Verb::FormingBar, bar.ts as u64);
    }
    fn mark_tick(&self, _venue: &str, _symbol: &str, _px: f64, ts: i64) {
        self.land(Verb::Mark, ts as u64);
    }
    fn bar_close_tick(&self, _venue: &str, _symbol: &str, _px: f64, _ts: i64) {}
    fn l2_snapshot(
        &self,
        _venue: &str,
        _symbol: &str,
        _tick_size: f64,
        _bids: Vec<BookLevel>,
        _asks: Vec<BookLevel>,
        ts: i64,
    ) {
        self.land(Verb::Depth50, ts as u64);
    }
    fn quote(&self, _venue: &str, _symbol: &str, quote: QuoteTick) {
        self.land(Verb::Quote, quote.ts as u64);
    }
    fn trade(&self, _venue: &str, _symbol: &str, trade: TradeTick) {
        self.land(Verb::Trade, trade.ts as u64);
    }
    fn book(&self, _venue: &str, _symbol: &str, book: Arc<L2Book>) {
        self.land(Verb::Book100, book.last_seq);
    }
    fn book_update(&self, _venue: &str, _symbol: &str, update: BookUpdate) {
        self.land(Verb::BookDelta, update.ts as u64);
    }
    fn stream_status(&self, _venue: &str, _symbol: &str, _stream: &str, status: StreamStatus) {
        if let StreamStatus::GapStart { at_ts_ms } = status {
            self.land(Verb::Status, at_ts_ms as u64);
        }
    }
    fn data_delay(&self, _venue: &str, _symbol: &str, _stream: &str, _delay: DataDelay) {}
}

/// One cell's samples.
struct Cell {
    verb: Verb,
    depth: usize,
    /// Per event, ns from the daemon's sink call to the datahub-side sink.
    latency: Vec<u64>,
    /// Per event, ns the daemon's sink call itself took (what the pump pays).
    emit: Vec<u64>,
    /// Events that never landed (all of them in the cut round, see [`measure`]).
    lost: usize,
    /// The round the cell stopped at because its events stopped landing.
    cut: Option<usize>,
}

/// `p`-th percentile of a sorted sample, the core-hop harness's definition (`n * p`, floored).
fn pct(sorted: &[u64], p: f64) -> u64 {
    sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)]
}

/// p50, p99, p99.9 and max of a sample; `None` for an empty one (a cell cut in its warm-up kept none).
fn quantiles(sample: &[u64]) -> Option<[u64; 4]> {
    let mut sorted = sample.to_vec();
    sorted.sort_unstable();
    let max = *sorted.last()?;
    Some([pct(&sorted, 0.50), pct(&sorted, 0.99), pct(&sorted, 0.999), max])
}

/// One quantile of [`quantiles`] as text, `-` when there is no sample, through `unit`.
fn shown(q: Option<[u64; 4]>, k: usize, unit: impl Fn(u64) -> String) -> String {
    q.map_or_else(|| "-".to_string(), |q| unit(q[k]))
}

impl Cell {
    /// `none`, or `roundR` for a cell cut at round R (warm-up rounds included).
    fn cut_word(&self) -> String {
        self.cut.map_or_else(|| "none".to_string(), |round| format!("round{round}"))
    }

    /// The greppable line of the cell.
    fn line(&self) -> String {
        let (l, e) = (quantiles(&self.latency), quantiles(&self.emit));
        let ns = |v: u64| v.to_string();
        format!(
            "GATEWAYHUB-HOP verb={} depth={} n={} lost={} cut={} p50_ns={} p99_ns={} p999_ns={} \
             max_ns={} emit_p50_ns={} emit_p99_ns={} emit_max_ns={}\n",
            self.verb.name(),
            self.depth,
            self.latency.len(),
            self.lost,
            self.cut_word(),
            shown(l, 0, ns),
            shown(l, 1, ns),
            shown(l, 2, ns),
            shown(l, 3, ns),
            shown(e, 0, ns),
            shown(e, 1, ns),
            shown(e, 3, ns),
        )
    }
}

/// Written straight to the stderr HANDLE: libtest swallows `eprintln!` of a passing test unless the
/// runner passes `--nocapture`, and a measurement nobody can read is no measurement (the same
/// reason the core-hop harness writes its line this way).
fn report(text: &str) {
    let mut err = std::io::stderr();
    let _ = err.write_all(text.as_bytes());
    let _ = err.flush();
}

/// How long a round may go without one of its events landing before the rest count as lost.
const STALL: Duration = Duration::from_secs(2);

/// Measure `verb` at queue depth `depth`: `rounds` rounds of `depth` back-to-back events, after a
/// discarded warm-up. A round ends when all of its events have landed. When they stop landing for
/// [`STALL`] the rest are `lost` and the cell is `cut` at that round (the module doc's "A burst the
/// session queue cannot hold"): only the whole rounds before it are timed.
fn measure(verb: Verb, depth: usize, rounds: usize, warmup: usize, gap: Duration) -> Cell {
    let total = (warmup + rounds) * depth;
    let probe = Probe::new(verb, total);
    let data = ScriptedData::new(&["binance"]);
    // The gateway is declared first, so it is dropped last (the client closes before the server drains).
    let gateway = LoopbackGateway::start(data.clone(), ScriptedRest::new());
    let link = gateway.link_with(keys(), GatewayTimings::default());
    let mut client = RemoteDataClient::new(
        GatewayOpener::new(link),
        "binance",
        Arc::clone(&probe) as Arc<dyn LiveDataSink>,
    )
    .expect("the client's reader thread");
    let symbols: Vec<String> = (0..depth).map(|j| format!("SYM{j:03}")).collect();
    for symbol in &symbols {
        client.subscribe_quotes(symbol).expect("subscribe");
    }
    let sink = Arc::clone(&data.session(0).sink);

    let mut emit = Vec::with_capacity(total);
    let (mut lost, mut cut) = (0usize, None);
    for round in 0..warmup + rounds {
        // Build the round's events first: the stamp must not include building them.
        let events: Vec<(usize, SinkCall)> = symbols
            .iter()
            .enumerate()
            .map(|(j, symbol)| {
                let i = round * depth + j;
                (i, verb.call(symbol, i as u64))
            })
            .collect();
        for (i, call) in events {
            probe.stamp_sent(i);
            let started = Instant::now();
            play(sink.as_ref(), call);
            let took = started.elapsed().as_nanos() as u64;
            if round >= warmup {
                emit.push(took);
            }
        }
        let want = (round + 1) * depth;
        let (mut seen, mut moved) = (probe.delivered(), Instant::now());
        while seen < want && moved.elapsed() < STALL {
            std::thread::sleep(Duration::from_micros(20));
            let now = probe.delivered();
            if now != seen {
                (seen, moved) = (now, Instant::now());
            }
        }
        if seen < want {
            lost = want - seen;
            cut = Some(round);
            break;
        }
        std::thread::sleep(gap);
    }
    // Only whole rounds are timed: a cut round's survivors are the ones the eviction spared.
    let timed = cut.map_or(total, |round| round * depth);
    let latency: Vec<u64> = (warmup * depth..timed).filter_map(|i| probe.latency(i)).collect();
    drop(client);
    drop(gateway);
    Cell { verb, depth, latency, emit, lost, cut }
}

/// The box this ran on, for the line that goes in the PR body: load before, the cores this process may
/// use, how many.
fn box_line(tag: &str) -> String {
    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_else(|_| "n/a".to_string());
    let allowed = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("Cpus_allowed_list")).map(|l| l.replace('\t', " "))
        })
        .unwrap_or_else(|| "Cpus_allowed_list: n/a".to_string());
    let cpus = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
    format!(
        "GATEWAYHUB-HOP-BOX {tag} loadavg=\"{}\" {allowed} available_parallelism={cpus}\n",
        load.trim()
    )
}

/// A human table of the same numbers, in microseconds.
fn table(cells: &[Cell]) -> String {
    let mut out = String::from(
        "GATEWAYHUB-HOP-TABLE verb depth n p50_us p99_us p99.9_us max_us | emit p50_us p99_us max_us \
         | lost cut\n",
    );
    for cell in cells {
        let (l, e) = (quantiles(&cell.latency), quantiles(&cell.emit));
        let us = |ns: u64| format!("{:.1}", ns as f64 / 1_000.0);
        out.push_str(&format!(
            "GATEWAYHUB-HOP-TABLE {:<11} {:>3} {:>6} {:>8} {:>8} {:>8} {:>9} | {:>6} {:>6} {:>8} | {} {}\n",
            cell.verb.name(),
            cell.depth,
            cell.latency.len(),
            shown(l, 0, us),
            shown(l, 1, us),
            shown(l, 2, us),
            shown(l, 3, us),
            shown(e, 0, us),
            shown(e, 1, us),
            shown(e, 3, us),
            cell.lost,
            cell.cut_word(),
        ));
    }
    out
}

/// **The measurement.** Every verb class at every queue depth, ~10,000 samples a cell; one line per
/// cell and a table at the end. Asserts only that events arrived.
#[test]
#[ignore = "a measurement, not a gate: run on a quiet lane, `--ignored --nocapture`, and quote the numbers"]
fn the_hop_from_the_daemons_sink_to_the_datahubs_sink() {
    // (depth, rounds): each cell is ~10,000 samples.
    const SHAPES: [(usize, usize); 3] = [(1, 10_000), (16, 700), (128, 80)];
    report(&box_line("before"));
    let mut cells = Vec::new();
    for verb in VERBS {
        for (depth, rounds) in SHAPES {
            let cell = measure(verb, depth, rounds, rounds / 20 + 5, Duration::from_micros(200));
            report(&cell.line());
            assert!(
                depth > 1 || (cell.lost == 0 && !cell.latency.is_empty()),
                "{} paced (depth 1) lost {} events: a paced hop that loses one is broken, not slow",
                verb.name(),
                cell.lost
            );
            cells.push(cell);
        }
    }
    report(&table(&cells));
    report(&box_line("after"));
}

/// The harness's own plumbing, on a short run that rides every PR: all events of a small cell land,
/// each has a latency, and the line a human or a script reads carries every field; a cell cut before
/// it kept a sample still prints (the line the measurement owes `seed_500` at depth 128). Asserts
/// nothing about how long anything took.
#[test]
fn the_harness_measures_every_event_of_a_short_run() {
    vike_log::test_init();
    for verb in VERBS {
        let cell = measure(verb, 4, 25, 3, Duration::from_micros(100));
        assert_eq!(cell.lost, 0, "{}: events never landed", verb.name());
        assert_eq!(cell.cut, None, "{}: the cell was cut", verb.name());
        assert_eq!(cell.latency.len(), 4 * 25, "{}: an event has no latency", verb.name());
        assert_eq!(cell.emit.len(), 4 * 25);
        let line = cell.line();
        for field in [
            "verb=",
            "depth=4",
            "n=100",
            "lost=0",
            "cut=none",
            "p50_ns=",
            "p99_ns=",
            "p999_ns=",
            "max_ns=",
            "emit_p50_ns=",
        ] {
            assert!(line.contains(field), "{}: the line lacks `{field}`: {line}", verb.name());
        }
        assert!(line.ends_with('\n') && line.starts_with("GATEWAYHUB-HOP "));
    }
    assert!(table(&[measure(Verb::Quote, 2, 10, 2, Duration::ZERO)]).contains("quote"));
    let cut = Cell {
        verb: Verb::Seed500,
        depth: 128,
        latency: Vec::new(),
        emit: vec![2_000, 3_000],
        lost: 59,
        cut: Some(0),
    };
    assert!(
        cut.line().contains(" n=0 lost=59 cut=round0 p50_ns=- p99_ns=- p999_ns=- max_ns=- "),
        "{}",
        cut.line()
    );
    assert!(table(&[cut]).contains("| 59 round0\n"));
    assert!(box_line("test").starts_with("GATEWAYHUB-HOP-BOX test "));
}
