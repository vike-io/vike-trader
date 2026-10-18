//! The PRICE hop, measured: what the two hops of the strategy lane add between a venue pump's sink call in
//! the gatewayhub and the tradehub's consumer sink (B.11, M3 of the wave-2 plan).
//!
//! The path under test, every part REAL but the venue: a scripted venue client in a gatewayhub on loopback
//! (`ScriptedData`, the testkit) -> its wire sink, the call a pump makes -> the tube's per-session queue
//! and writer (hop 1, `serde_json` out) -> the datahub's `RemoteDataClient` reader and `FeedBroker` ->
//! `RoutingSink` -> the strategy hub's session queue and writer (hop 2, `serde_json` out again) -> the
//! tradehub's `StratFeed` reader (`serde_json` in) -> its consumer sink, where the harness stamps the
//! landing. Both sockets are loopback; both sides are in this process.
//!
//! # The baselines
//!
//! The same payload with no hop in it, two ways, so the paired difference can be read against either:
//! `*_sink_direct` calls the consumer sink on the sending thread (what a venue pump does today when the
//! mount owns the feed: the cost of the sink call and nothing else), and `*_sink_handoff` sends the payload
//! down a channel to a thread that makes the call (the same, plus one thread wake). The hop's cost is
//! `HOP-LATENCY-DELTA`'s difference of quantiles against each.
//!
//! # Not a gate
//!
//! `#[ignore]`d and print-only, in the style of `crates/vike-gatewayhub/tests/hop_latency.rs`: it asserts
//! NOTHING about time (the owner's ruling, D9: measure and publish, no CI ceiling; the target is +1 ms p99
//! for prices, both hops together). A short, non-ignored run asserts only that every sample of a small cell
//! LANDED, so a rotted harness is red.
//!
//! ```text
//! LANE=laneN just the CI box <branch> cargo test -p vike-datahub --test price_hop --release \
//!     -- --ignored --nocapture
//! ```
//!
//! Numbers compare only on one quiet box (the coordinator's the latency box lane, `crates/vike-core/CLAUDE.md`'s
//! "Measuring a hot-path change honestly"); a shared box's numbers are a smoke.
//!
//! # The legs and shapes
//!
//! Two verbs, `quote` and `trade` (both lossless, so a lag is a longer wait and never a conflated frame),
//! each in five shapes: paced (the queue empty), the open-loop rates of the measured group maxima
//! (`rate2056` is Binance perp trades, `rate3716` is Polymarket, the busiest feed), and a 16- and a
//! 128-deep burst. `trade_gateway_to_strat_shared` repeats the trade cell with the market-data hub's and the
//! recorder's holders subscribed to the SAME keys, so hop 1's session and the venue client's sink are shared
//! with them and `RoutingSink` makes three sink calls per frame (the shared-session case). `emit_*` in
//! the line is what the venue pump's sink call cost (it must never wait for a consumer).
//!
//! Not measured here: an anchor frame of ~44 KB beside the quotes (head of line), and a stalled reader (the
//! slow-consumer drills of `tests/strat_lane.rs`).

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use vike_data::{DataClient, HistStore, LiveDataSink, MemHistStore, RecordingSink};
use vike_datahub::feeds::{ClientBuilder, FeedBroker, Holder, Sharing};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub::strat::{StratHub, StratLimits};
use vike_datahub_client::strat::{StratFeed, StratOpener, StratTimings};
use vike_gatewayhub::testkit::data::ScriptedData;
use vike_gatewayhub::testkit::events::{quote, trade};
use vike_gatewayhub::testkit::gateway::LoopbackGateway;
use vike_gatewayhub::testkit::rest::ScriptedRest;
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_hub_proto::auth::HubKeys;
use vike_market_stream::market::RemoteDataClient;
use vike_marketdata::{Bar, BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};

mod common;
use common::keys;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "price_hop/hop_stats.rs"]
mod hop_stats;

use hop_stats::{Run, Shape, Stamps, box_line, delta_line, drive, line, report};

const VENUE: &str = "binance";
/// Symbols subscribed on every lane: a burst spreads over them.
const SYMBOLS: usize = 16;
/// The `ts` of the priming frames (the pipeline is up when one lands).
const PRIME: i64 = i64::MAX - 1_000;
/// Stamp slots of one rig: every cell of one measurement takes its own range.
const SLOTS: usize = 160_000;

const TRADE_KEY: &[u8] = b"datahub-price-hop-key";

fn trade_keys() -> HubKeys {
    keys().with_trade(TRADE_KEY.to_vec())
}

fn symbol(k: usize) -> String {
    format!("SYM{:03}", k % SYMBOLS)
}

/// The two verbs measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verb {
    Quote,
    Trade,
}

impl Verb {
    fn word(self) -> &'static str {
        match self {
            Verb::Quote => "quote",
            Verb::Trade => "trade",
        }
    }
}

/// One sink call, as data: moved into the sink (a clone is not what a pump pays).
enum Msg {
    Quote(String, QuoteTick),
    Trade(String, TradeTick),
}

impl Msg {
    /// The call for sample `index`, which it carries in its `ts`.
    fn new(verb: Verb, index: usize) -> Msg {
        let symbol = symbol(index);
        let x = 100.0 + (index % 997) as f64 * 0.01 + 1e-7;
        match verb {
            Verb::Quote => Msg::Quote(symbol.clone(), quote(&symbol, index as i64, x)),
            Verb::Trade => Msg::Trade(symbol.clone(), trade(&symbol, index as i64, x)),
        }
    }

    /// The priming frame of `verb`.
    fn prime(verb: Verb) -> Msg {
        let symbol = symbol(0);
        match verb {
            Verb::Quote => Msg::Quote(symbol.clone(), quote(&symbol, PRIME, 1.0)),
            Verb::Trade => Msg::Trade(symbol.clone(), trade(&symbol, PRIME, 1.0)),
        }
    }

    /// Make the call a venue pump makes.
    fn play(self, sink: &dyn LiveDataSink) {
        match self {
            Msg::Quote(symbol, q) => sink.quote(VENUE, &symbol, q),
            Msg::Trade(symbol, t) => sink.trade(VENUE, &symbol, t),
        }
    }
}

fn msgs(verb: Verb, base: usize, count: usize) -> Vec<Msg> {
    (base..base + count).map(|index| Msg::new(verb, index)).collect()
}

/// The tradehub-side consumer sink: stamps the landing of the sample a frame carries, takes no lock and
/// allocates nothing, so it does not add to what it measures.
struct Probe {
    stamps: Arc<Stamps>,
    primed: AtomicBool,
}

impl Probe {
    fn new(stamps: Arc<Stamps>) -> Arc<Self> {
        Arc::new(Self { stamps, primed: AtomicBool::new(false) })
    }

    fn hit(&self, ts: i64) {
        if ts == PRIME {
            self.primed.store(true, Ordering::Release);
        } else if ts >= 0 {
            self.stamps.land(ts as usize);
        }
    }
}

impl LiveDataSink for Probe {
    fn seed_bars(&self, _venue: &str, _symbol: &str, _interval: &str, _bars: Vec<Bar>) {}
    fn close_bar(&self, _venue: &str, _symbol: &str, _interval: &str, _bar: Bar) {}
    fn forming_bar(&self, _venue: &str, _symbol: &str, _interval: &str, _bar: Bar) {}
    fn mark_tick(&self, _venue: &str, _symbol: &str, _px: f64, _ts: i64) {}
    fn bar_close_tick(&self, _venue: &str, _symbol: &str, _px: f64, _ts: i64) {}
    fn l2_snapshot(
        &self,
        _venue: &str,
        _symbol: &str,
        _tick_size: f64,
        _bids: Vec<BookLevel>,
        _asks: Vec<BookLevel>,
        _ts: i64,
    ) {
    }
    fn quote(&self, _venue: &str, _symbol: &str, quote: QuoteTick) {
        self.hit(quote.ts);
    }
    fn trade(&self, _venue: &str, _symbol: &str, trade: TradeTick) {
        self.hit(trade.ts);
    }
    fn book(&self, _venue: &str, _symbol: &str, _book: Arc<L2Book>) {}
    fn book_update(&self, _venue: &str, _symbol: &str, _update: BookUpdate) {}
    fn stream_status(&self, _venue: &str, _symbol: &str, _stream: &str, _status: StreamStatus) {}
    fn data_delay(&self, _venue: &str, _symbol: &str, _stream: &str, _delay: DataDelay) {}
}

// ---- the rig: a scripted gatewayhub, a real datahub strategy lane, the tradehub's StratFeed -----------------

/// One measurement rig. ⚠ Field order IS drop order: the feed (the tradehub side) goes first, the
/// datahub-side handles next, the gateway LAST so it outlives every client it serves.
struct Rig {
    feed: StratFeed,
    /// The market-data hub's and the recorder's handles, when the rig shares the venue client with them.
    _holders: Vec<Box<dyn DataClient + Send>>,
    probe: Arc<Probe>,
    stamps: Arc<Stamps>,
    /// The next unused stamp slot.
    next: usize,
    data: ScriptedData,
    _gateway: LoopbackGateway,
}

impl Rig {
    /// A rig whose `StratFeed` holds `verbs` on every symbol; `shared` adds the md and recorder holders
    /// on the same keys.
    fn start(verbs: &[Verb], shared: bool) -> Rig {
        let data = ScriptedData::new(&[VENUE]);
        let gateway = LoopbackGateway::start(data.clone(), ScriptedRest::new());
        let link = gateway.link();
        let builder: ClientBuilder = Box::new(move |venue, sink| {
            RemoteDataClient::new(GatewayOpener::new(link.clone()), venue, sink)
                .map(|client| Box::new(client) as Box<dyn DataClient + Send>)
                .map_err(|e| format!("the gatewayhub client could not start its reader: {e}"))
        });
        let broker = FeedBroker::new(builder, |_| Sharing::Shared);
        let hub = StratHub::new(
            Arc::clone(&broker),
            vec![VENUE.to_string()],
            None,
            StratLimits::PRODUCTION,
        );
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
        let lanes = ServerLanes { strat: Some(Arc::clone(&hub)), ..ServerLanes::default() };
        std::thread::Builder::new()
            .name("hop-datahub".into())
            .spawn(move || {
                let _ = serve_with_lanes(
                    listener,
                    store,
                    None,
                    Some(trade_keys()),
                    None,
                    None,
                    lanes,
                    None,
                    false,
                    ServerTimings::PRODUCTION,
                );
            })
            .expect("spawn the datahub");

        let stamps = Arc::new(Stamps::new(SLOTS));
        let probe = Probe::new(Arc::clone(&stamps));
        let opener = StratOpener::new(addr.to_string(), trade_keys(), StratTimings::default());
        let mut feed = StratFeed::new(opener, VENUE, Arc::clone(&probe) as Arc<dyn LiveDataSink>)
            .expect("the strat feed's reader thread");
        let mut held: Vec<Box<dyn DataClient + Send>> = Vec::new();
        if shared {
            // The GUI's and the recorder's holders, on the keys the strategy lane is about to take.
            broker.attach_sink(Holder::Md, Arc::new(RecordingSink::default()));
            broker.attach_sink(Holder::Rec, Arc::new(RecordingSink::default()));
            held.push(Box::new(broker.handle(Holder::Md, VENUE)));
            held.push(Box::new(broker.handle(Holder::Rec, VENUE)));
        }
        for k in 0..SYMBOLS {
            for verb in verbs {
                let symbol = symbol(k);
                for client in held.iter_mut() {
                    match verb {
                        Verb::Quote => client.subscribe_quotes(&symbol).map(|_| ()),
                        Verb::Trade => client.subscribe_trades(&symbol).map(|_| ()),
                    }
                    .expect("a shared holder subscribes");
                }
                let subscribed = match verb {
                    Verb::Quote => feed.subscribe_quotes(&symbol),
                    Verb::Trade => feed.subscribe_trades(&symbol),
                };
                // The first subscribe waits for the dial; a transient `Err` while it comes up is retried.
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut result = subscribed;
                while result.is_err() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                    result = match verb {
                        Verb::Quote => feed.subscribe_quotes(&symbol),
                        Verb::Trade => feed.subscribe_trades(&symbol),
                    };
                }
                result.expect("the strategy lane takes the subscription");
            }
        }
        Rig { feed, _holders: held, probe, stamps, next: 0, data, _gateway: gateway }
    }

    /// The venue client's wire sink: what a pump of the gatewayhub calls.
    fn venue_sink(&self) -> Arc<dyn LiveDataSink> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(session) = self.data.sessions().last() {
                return Arc::clone(&session.sink);
            }
            assert!(Instant::now() < deadline, "the gatewayhub never built the venue client");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Emit priming frames until one lands at the consumer: the whole path is up.
    fn prime(&self, verb: Verb) {
        let sink = self.venue_sink();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self.probe.primed.swap(false, Ordering::AcqRel) {
            assert!(Instant::now() < deadline, "no priming frame reached the tradehub's sink");
            Msg::prime(verb).play(sink.as_ref());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// One cell over the real path: the wire sink of the gatewayhub's venue client to the consumer sink.
    fn cell(&mut self, verb: Verb, shape: Shape, count: usize, warmup: usize) -> Run {
        assert!(self.next + count <= SLOTS, "the rig is out of stamp slots");
        self.prime(verb);
        let sink = self.venue_sink();
        let base = self.next;
        self.next += count;
        drive(shape, base, warmup, &self.stamps, msgs(verb, base, count), |m| m.play(sink.as_ref()))
    }

    fn is_up(&self) -> bool {
        self.feed.is_up()
    }
}

/// The BASELINE with no hop at all: the consumer sink called on the sending thread.
fn sink_direct(verb: Verb, shape: Shape, count: usize, warmup: usize) -> Run {
    let stamps = Arc::new(Stamps::new(count));
    let probe = Probe::new(Arc::clone(&stamps));
    drive(shape, 0, warmup, &stamps, msgs(verb, 0, count), |m| m.play(probe.as_ref()))
}

/// The BASELINE with one thread wake: the payload goes down a channel to a thread that calls the sink.
fn sink_handoff(verb: Verb, shape: Shape, count: usize, warmup: usize) -> Run {
    let stamps = Arc::new(Stamps::new(count));
    let probe = Probe::new(Arc::clone(&stamps));
    let (tx, rx) = channel::<Msg>();
    let consumer = std::thread::Builder::new()
        .name("hop-handoff".into())
        .spawn(move || {
            while let Ok(m) = rx.recv() {
                m.play(probe.as_ref());
            }
        })
        .expect("spawn the handoff thread");
    let run = drive(shape, 0, warmup, &stamps, msgs(verb, 0, count), |m| {
        let _ = tx.send(m);
    });
    drop(tx);
    let _ = consumer.join();
    run
}

/// `(shape, samples)` of a cell.
const SHAPES: [(Shape, usize); 5] = [
    (Shape::Paced, 4_000),
    (Shape::Rate(2_056), 6_000),
    (Shape::Rate(3_716), 8_000),
    (Shape::Burst(16), 4_000),
    (Shape::Burst(128), 4_096),
];

/// **The measurement.** Both verbs in every shape over the real path and over both baselines, then the
/// trade cells again with the md and recorder holders sharing the venue client. One line per cell and a
/// delta line per pair.
#[test]
#[ignore = "a measurement, not a gate: run on a quiet lane, `--ignored --nocapture`, and quote the numbers"]
fn the_price_hop_from_the_gatewayhubs_sink_to_the_tradehubs_sink() {
    use Verb::{Quote, Trade};
    report(&box_line("before"));
    let mut plain = Rig::start(&[Quote, Trade], false);
    assert!(plain.is_up());
    for verb in [Quote, Trade] {
        for (shape, count) in SHAPES {
            let warmup = count / 20 + 8;
            let leg = format!("{}_gateway_to_strat", verb.word());
            let (direct_leg, handoff_leg) =
                (format!("{}_sink_direct", verb.word()), format!("{}_sink_handoff", verb.word()));
            let through = plain.cell(verb, shape, count, warmup);
            let direct = sink_direct(verb, shape, count, warmup);
            let handoff = sink_handoff(verb, shape, count, warmup);
            report(&line(&leg, shape, &through));
            report(&line(&direct_leg, shape, &direct));
            report(&line(&handoff_leg, shape, &handoff));
            report(&delta_line(&leg, &direct_leg, shape, &through, &direct));
            report(&delta_line(&leg, &handoff_leg, shape, &through, &handoff));
        }
    }
    drop(plain);

    let mut shared = Rig::start(&[Trade], true);
    for (shape, count) in SHAPES {
        let warmup = count / 20 + 8;
        let leg = "trade_gateway_to_strat_shared";
        let through = shared.cell(Trade, shape, count, warmup);
        let handoff = sink_handoff(Trade, shape, count, warmup);
        report(&line(leg, shape, &through));
        report(&delta_line(leg, "trade_sink_handoff", shape, &through, &handoff));
    }
    drop(shared);
    report(&box_line("after"));
}

/// The harness's own plumbing, on a short run that rides every PR: every sample of a small cell of both
/// verbs LANDED over the path, plain and shared, and over both baselines, and the lines carry their
/// fields. Asserts nothing about how long anything took.
#[test]
fn the_harness_lands_every_sample_of_a_short_run() {
    vike_log::test_init();
    let (count, warmup) = (24, 4);
    for shared in [false, true] {
        let verbs: &[Verb] = if shared { &[Verb::Trade] } else { &[Verb::Quote, Verb::Trade] };
        let mut rig = Rig::start(verbs, shared);
        assert!(rig.is_up());
        for &verb in verbs {
            for shape in [Shape::Paced, Shape::Burst(4), Shape::Rate(1_000)] {
                let through = rig.cell(verb, shape, count, warmup);
                let direct = sink_direct(verb, shape, count, warmup);
                let handoff = sink_handoff(verb, shape, count, warmup);
                for run in [&through, &direct, &handoff] {
                    assert_eq!(run.lost, 0, "{} samples never landed", verb.word());
                    assert_eq!(run.cut, None);
                    assert_eq!(run.latency.len(), count - warmup);
                    assert_eq!(run.emit.len(), count - warmup);
                    let text = line("leg", shape, run);
                    for field in [
                        "HOP-LATENCY leg=leg",
                        "n=20",
                        "lost=0",
                        "p50_ns=",
                        "p99_ns=",
                        "p999_ns=",
                        "max_ns=",
                    ] {
                        assert!(text.contains(field), "the line lacks `{field}`: {text}");
                    }
                }
                let delta = delta_line("a", "b", shape, &through, &direct);
                assert!(delta.starts_with("HOP-LATENCY-DELTA leg=a baseline=b shape="), "{delta}");
            }
        }
    }
}
