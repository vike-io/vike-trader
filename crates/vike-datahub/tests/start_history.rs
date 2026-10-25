//! The START HISTORY over a real socket: a tradehub-shaped raw client against the real strategy lane, the real
//! broker and a scripted OANDA-shaped venue client (a bar subscription seeds from inside `subscribe_bars` and
//! then closes bars), with `StartHistory` attached over an in-memory store.
//! `crates/vike-datahub/src/strat/history_tests.rs` holds the pieces with no socket; this file holds what
//! only the wire can show:
//!
//! 1. **Shadow delivers nothing different** - the frames the tradehub reads with the holder attached are the
//!    frames it reads without, byte for byte, while the closed bars land in the store and the seed is compared
//!    once.
//! 2. **Served, the start history is the FIRST seed on the wire**, after the `SubOk` and ahead of the venue's
//!    own, and the venue's seed is spliced so that it never shrinks it.
//! 3. **A refused subscription leaves no start history behind** (the held frames are purged with the key).
//! 4. **Md and the recorder still cannot subscribe bars** (R8): attaching the holder opens no new door.
//!
//! The venue is scripted; nothing here reaches a venue, a gatewayhub or a credential.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use vike_data::{
    DataClient, HistStore, LiveDataError, LiveDataSink, MemHistStore, SubscriptionId, TsRange,
};
use vike_datahub::feeds::{ClientBuilder, FeedBroker, Holder, Sharing};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub::strat::history::{BarWriter, GapSource, GatewayGap, StartHistory};
use vike_datahub::strat::{StratHub, StratLimits};
use vike_datahub_client::proto::{Request, Response, SL_FRAME_CEILING_BYTES, write_frame};
use vike_gatewayhub::testkit::data::ScriptedData;
use vike_gatewayhub::testkit::gateway::LoopbackGateway;
use vike_gatewayhub::testkit::rest::{Answer, ScriptedRest};
use vike_gatewayhub_client::proto::FetchWhat;
use vike_hub_proto::auth::{HubKeys, Scope};
use vike_market_stream::frame::read_capped;
use vike_market_stream::wire::{StreamDown, StreamUp, SubOp};
use vike_marketdata::Bar;

mod common;
use common::{authed_stream, exchange, keys};

const TRADE_KEY: &[u8] = b"datahub-strategy-lane-key";
const STEP: i64 = 60_000;
/// A minute boundary.
const BASE: i64 = 1_760_000_040_000;
const WAIT: Duration = Duration::from_secs(5);

fn trade_keys() -> HubKeys {
    keys().with_trade(TRADE_KEY.to_vec())
}

fn bar_at(i: i64) -> Bar {
    let f = i as f64 / 1000.0;
    Bar {
        ts: BASE + i * STEP,
        open: 1.0 + f,
        high: 1.1 + f,
        low: 0.9 + f,
        close: 1.05 + f,
        volume: 10.0 + i as f64,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn bars(from: i64, to: i64) -> Vec<Bar> {
    (from..to).map(bar_at).collect()
}

fn times(bars: &[Bar]) -> Vec<i64> {
    bars.iter().map(|b| (b.ts - BASE) / STEP).collect()
}

/// `k` whole minutes after [`BASE`] and thirty seconds: minute `k - 1` is the newest closed bar.
fn clock_at(k: i64) -> i64 {
    BASE + k * STEP + 30_000
}

// ---- the venue side ---------------------------------------------------------------------------------------

/// An OANDA-shaped bar client: a bar subscription emits the venue's seed from INSIDE `subscribe_bars`, as the
/// bridge's `bars_main` does, and a symbol spelled `REFUSED` is refused.
struct Venue {
    name: String,
    sink: Arc<dyn LiveDataSink>,
    seed: Vec<Bar>,
    next: u64,
}

impl DataClient for Venue {
    fn subscribe_bars(&mut self, s: &str, interval: &str) -> Result<SubscriptionId, LiveDataError> {
        if s == "REFUSED" {
            return Err(LiveDataError::Subscribe("scripted refusal".into()));
        }
        self.next += 1;
        if !self.seed.is_empty() {
            self.sink.seed_bars(&self.name, s, interval, self.seed.clone());
        }
        Ok(SubscriptionId(self.next))
    }
    /// A QUIET quote key: the subscription succeeds and no price ever comes (a closed market, a quiet pair).
    fn subscribe_quotes(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.next += 1;
        Ok(SubscriptionId(self.next))
    }
    fn subscribe_trades(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("this scripted venue serves bars only"))
    }
    fn subscribe_book(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("this scripted venue serves bars only"))
    }
    fn subscribe_depth(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("this scripted venue serves bars only"))
    }
    fn unsubscribe(&mut self, _id: SubscriptionId) {}
    fn begin_shutdown(&mut self) {}
    fn shutdown(&mut self) {}
}

/// A history channel that knows a fixed set of bars.
struct Truth(Vec<Bar>);

impl GapSource for Truth {
    fn fetch(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, String> {
        Ok(self.0.iter().filter(|b| b.ts >= start_ms && b.ts <= end_ms).cloned().collect())
    }
}

struct Rig {
    addr: SocketAddr,
    store: Arc<MemHistStore>,
    history: Option<Arc<StartHistory>>,
    broker: Arc<FeedBroker>,
    sinks: Arc<Mutex<Vec<Arc<dyn LiveDataSink>>>>,
}

impl Rig {
    /// The sink the venue's (shared) client was built with: emitting through it is a venue emission.
    fn venue_sink(&self) -> Arc<dyn LiveDataSink> {
        Arc::clone(&self.sinks.lock().unwrap()[0])
    }

    fn history(&self) -> &Arc<StartHistory> {
        self.history.as_ref().expect("this rig has a start history")
    }
}

/// A lane over a fresh store serving "oanda", with a start history attached when `history` says how
/// (`Some(serve)`), whose venue seeds `venue_seed` on every bar subscription and whose history channel knows
/// `truth`.
fn rig(history: Option<bool>, venue_seed: Vec<Bar>, truth: Vec<Bar>, now: i64) -> Rig {
    rig_over(history, venue_seed, Arc::new(Truth(truth)), now)
}

/// [`rig`] over any history channel (the production one, over a scripted gatewayhub, for instance).
fn rig_over(history: Option<bool>, venue_seed: Vec<Bar>, gap: Arc<dyn GapSource>, now: i64) -> Rig {
    let store = Arc::new(MemHistStore::new());
    let sinks: Arc<Mutex<Vec<Arc<dyn LiveDataSink>>>> = Arc::default();
    let s = Arc::clone(&sinks);
    let builder: ClientBuilder = Box::new(
        move |venue: &str,
              sink: Arc<dyn LiveDataSink>|
              -> Result<Box<dyn DataClient + Send>, String> {
            s.lock().unwrap().push(Arc::clone(&sink));
            Ok(Box::new(Venue { name: venue.into(), sink, seed: venue_seed.clone(), next: 0 }))
        },
    );
    let broker = FeedBroker::new(builder, |_| Sharing::Shared);
    let hub = StratHub::new(
        Arc::clone(&broker),
        vec!["oanda".to_string()],
        None,
        StratLimits::PRODUCTION,
    );
    let attached = history.map(|serve| {
        let reader: Arc<dyn HistStore + Send + Sync> = store.clone();
        let writer_store = Arc::clone(&store);
        let writer: BarWriter = Box::new(move |venue, symbol, interval, bar| {
            let key = format!("{venue}:{symbol}:{interval}:{}-{}", bar.ts, bar.ts + STEP - 1);
            writer_store
                .append_bars(venue, symbol, interval, std::slice::from_ref(bar), Some(&key))
                .map_err(|e| e.to_string())
        });
        let h =
            StartHistory::spawn_with_clock(reader, writer, Some(gap), serve, Arc::new(move || now))
                .expect("the worker starts");
        hub.attach_start_history(Arc::clone(&h));
        h
    });
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let served: Arc<dyn HistStore + Send + Sync> = store.clone();
    let lanes = ServerLanes { strat: Some(hub), ..ServerLanes::default() };
    thread::spawn(move || {
        let _ = serve_with_lanes(
            listener,
            served,
            None,
            Some(trade_keys()),
            None,
            None,
            lanes,
            None,
            false,
            ServerTimings::PRODUCTION,
        );
    });
    Rig { addr, store, history: attached, broker, sinks }
}

// ---- the tradehub side -----------------------------------------------------------------------------------------

fn lane(addr: SocketAddr) -> TcpStream {
    let mut s = authed_stream(addr, &trade_keys(), Scope::Trade);
    match exchange(&mut s, &Request::StratOpen { venue: "oanda".to_string(), network: None }) {
        Response::StratOpened { .. } => {}
        other => panic!("expected StratOpened, got {other:?}"),
    }
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s
}

fn up(s: &mut TcpStream, frame: &StreamUp) {
    write_frame(s, frame).expect("a client frame writes");
}

/// The next frame that is not a heartbeat, bounded.
fn next(s: &mut TcpStream) -> StreamDown {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match read_capped::<_, StreamDown>(s, SL_FRAME_CEILING_BYTES).expect("a server frame reads")
        {
            StreamDown::Heartbeat => {
                assert!(Instant::now() < deadline, "only heartbeats for 20 s");
            }
            other => return other,
        }
    }
}

fn bars_op(symbol: &str) -> SubOp {
    SubOp::Bars { symbol: symbol.to_string(), interval: "1m".to_string() }
}

/// A frame as the bytes the tradehub reads.
fn wire(frame: &StreamDown) -> String {
    serde_json::to_string(frame).expect("a frame encodes")
}

/// Subscribe bars for `symbol` on `s` and read everything the subscription puts on the wire: the `SubOk`, every
/// seed that follows it, and the join restatement - stopping at the restatement (a `Status`), which is the last
/// thing a first subscription writes.
fn subscribe(s: &mut TcpStream, sid: u64, symbol: &str) -> Vec<StreamDown> {
    up(s, &StreamUp::Sub { sid, op: bars_op(symbol), class: None });
    let mut out = vec![next(s)];
    loop {
        let frame = next(s);
        let done = matches!(frame, StreamDown::Status { .. });
        out.push(frame);
        if done {
            return out;
        }
    }
}

/// Every frame up to and including the first closed bar.
fn until_close(s: &mut TcpStream) -> Vec<StreamDown> {
    let mut out = Vec::new();
    loop {
        let frame = next(s);
        let done = matches!(frame, StreamDown::BarClose { .. });
        out.push(frame);
        if done {
            return out;
        }
    }
}

fn seeds(frames: &[StreamDown]) -> Vec<Vec<Bar>> {
    frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Seed { bars, .. } => Some(bars.clone()),
            _ => None,
        })
        .collect()
}

fn stored(rig: &Rig, symbol: &str) -> Vec<Bar> {
    rig.store.load_bars("oanda", symbol, "1m", TsRange::all()).expect("the double reads")
}

// ---- 1. shadow ---------------------------------------------------------------------------------------------------

/// **Shadow delivers nothing different.** The same session against a lane with the start history attached and a
/// lane without: the `SubOk`, the seed and the restatement are the same bytes, and so are the frames that
/// follow up to the first close. The attached lane meanwhile saved the closed bar - once, and not the forming
/// snapshot of the same time - and compared the venue's seed once.
#[test]
fn a_shadowed_lane_hands_the_tradehub_exactly_the_frames_a_plain_lane_does() {
    let seed = bars(2, 8);
    let with = rig(Some(false), seed.clone(), Vec::new(), clock_at(9));
    let without = rig(None, seed.clone(), Vec::new(), clock_at(9));
    // The store already holds older bars, which the venue's seed overlaps.
    with.store.append_bars("oanda", "EURUSD", "1m", &bars(0, 6), None).unwrap();

    let mut a = lane(with.addr);
    let mut b = lane(without.addr);
    let (fa, fb) = (subscribe(&mut a, 1, "EURUSD"), subscribe(&mut b, 1, "EURUSD"));
    assert_eq!(fa.iter().map(wire).collect::<Vec<_>>(), fb.iter().map(wire).collect::<Vec<_>>());
    assert_eq!(seeds(&fa), vec![seed], "the tradehub's only seed is the venue's, unchanged");

    for r in [&with, &without] {
        let venue = r.venue_sink();
        venue.forming_bar("oanda", "EURUSD", "1m", bar_at(8));
        venue.close_bar("oanda", "EURUSD", "1m", bar_at(8));
    }
    let (ca, cb) = (until_close(&mut a), until_close(&mut b));
    assert_eq!(ca.iter().map(wire).collect::<Vec<_>>(), cb.iter().map(wire).collect::<Vec<_>>());

    let history = with.history();
    assert!(history.settle(WAIT));
    let saved: Vec<i64> = times(&stored(&with, "EURUSD"));
    assert_eq!(
        saved,
        vec![0, 1, 2, 3, 4, 5, 6, 7, 8],
        "the seed's two bars the store lacked, then the closed bar once; the forming one never"
    );
    assert_eq!(history.stats().saved, 3);
    assert_eq!(history.stats().repaired, 2);
    let reports = history.reports();
    assert_eq!(reports.len(), 1, "one comparison line for the one subscription");
    assert_eq!((reports[0].overlap, reports[0].equal, reports[0].store_behind), (4, 4, 2));
    assert_eq!(reports[0].repaired, 2);
    assert!(stored(&without, "EURUSD").is_empty(), "a lane with no holder saves nothing");
}

// ---- 2. served ---------------------------------------------------------------------------------------------------

/// **Served, the start history is the first seed on the wire**: the `SubOk`, then the store's bars with the
/// missing span fetched from the history channel (and only that span), then the venue's own seed spliced over
/// it - so the strategy ends with the whole history, the venue's bar winning its time.
#[test]
fn a_served_lane_puts_the_start_history_on_the_wire_before_the_venues_seed() {
    let mut venue_seed = bars(7, 9);
    venue_seed[1].close = 3.0; // the venue's newest bar differs from the channel's: the venue wins
    let r = rig(Some(true), venue_seed, bars(0, 9), clock_at(9));
    r.store.append_bars("oanda", "EURUSD", "1m", &bars(0, 6), None).unwrap();

    let mut s = lane(r.addr);
    let frames = subscribe(&mut s, 1, "EURUSD");
    assert!(matches!(frames[0], StreamDown::SubOk { sid: 1, .. }), "the SubOk is first");
    let got = seeds(&frames);
    assert_eq!(
        got.len(),
        2,
        "the start history, then the venue's seed spliced over it: {frames:?}"
    );
    assert_eq!(times(&got[0]), (0..9).collect::<Vec<_>>(), "the store's six and the fetched three");
    assert_eq!(
        times(&got[1]),
        (0..9).collect::<Vec<_>>(),
        "the venue's short seed did not shrink it"
    );
    assert_eq!(got[1][8].close, 3.0, "and the venue's bar won its time");
    let seqs: Vec<u64> = frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Seed { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect();
    assert_eq!(seqs, vec![1, 2], "one numbered, lossless seed lane");
    assert!(r.history().settle(WAIT));
}

/// Subscribe QUOTES for `symbol` on `s`: the `SubOk` and the join restatement, nothing more (the key is quiet).
fn subscribe_quotes(s: &mut TcpStream, sid: u64, symbol: &str) {
    up(s, &StreamUp::Sub { sid, op: SubOp::Quotes { symbol: symbol.to_string() }, class: None });
    assert!(matches!(next(s), StreamDown::SubOk { .. }), "the quote subscription is acknowledged");
    assert!(matches!(next(s), StreamDown::Status { .. }), "…and the join is restated");
}

/// **The start history depends on no price.** A bar subscription is a REST poll and its start history comes from
/// the store and the history channel, so a quote key of the same symbol that is quiet (a closed market, a pair
/// that does not tick: the case wave A's rule eight swaps a stream for) delays nothing, in either order of the
/// two subscriptions: the history is on the wire right after the bar subscription's `SubOk`, with no quote sent.
#[test]
fn a_start_history_waits_for_no_price_whatever_order_the_quote_and_bar_keys_join_in() {
    for quotes_first in [true, false] {
        let r = rig(Some(true), bars(7, 9), bars(0, 9), clock_at(9));
        r.store.append_bars("oanda", "EURUSD", "1m", &bars(0, 6), None).unwrap();
        let mut s = lane(r.addr);
        let started = Instant::now();
        if quotes_first {
            subscribe_quotes(&mut s, 1, "EURUSD");
        }
        let frames = subscribe(&mut s, 2, "EURUSD");
        if !quotes_first {
            subscribe_quotes(&mut s, 1, "EURUSD");
        }
        let got = seeds(&frames);
        assert_eq!(got.len(), 2, "quotes_first={quotes_first}: {frames:?}");
        assert_eq!(times(&got[0]), (0..9).collect::<Vec<_>>(), "quotes_first={quotes_first}");
        // THE ORDER, pinned as it is. A key a quote subscription already acknowledged is not held for the bar
        // subscription's `SubOk` (only a NEW key's frames are), so there the start history reaches the wire
        // BEFORE that `SubOk`, exactly as the venue's own seed always has; the receiver files a seed by its
        // key, which the quote subscription bound, and delivers it. For a key the bar subscription opens,
        // the `SubOk` comes first and the history follows it.
        let subok = frames.iter().position(|f| matches!(f, StreamDown::SubOk { .. }));
        let first_seed = frames.iter().position(|f| matches!(f, StreamDown::Seed { .. }));
        if quotes_first {
            assert!(first_seed < subok, "an acknowledged key is not held: {frames:?}");
        } else {
            assert!(subok < first_seed, "a new key is held until its SubOk: {frames:?}");
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "quotes_first={quotes_first}: nothing waited for a price"
        );
    }
}

// ---- 3. a refusal ------------------------------------------------------------------------------------------------

/// **A refused subscription leaves no start history behind**: the preload went into the key's held frames
/// before the venue was asked, and the refusal purges them - the tradehub reads the `SubErr` and nothing else,
/// and the next subscription is unaffected.
#[test]
fn a_refused_subscription_leaves_no_start_history_on_the_wire() {
    let r = rig(Some(true), bars(7, 9), bars(0, 9), clock_at(9));
    r.store.append_bars("oanda", "REFUSED", "1m", &bars(0, 9), None).unwrap();
    let mut s = lane(r.addr);
    up(&mut s, &StreamUp::Sub { sid: 1, op: bars_op("REFUSED"), class: None });
    assert!(matches!(next(&mut s), StreamDown::SubErr { sid: 1, .. }));
    up(&mut s, &StreamUp::Ping);
    assert!(
        matches!(next_raw(&mut s), StreamDown::Heartbeat),
        "after the refusal the next thing on the wire is the heartbeat, not a held seed"
    );
    // The SAME symbol again: a seed the first refusal did not purge would come out ahead of this refusal.
    up(&mut s, &StreamUp::Sub { sid: 2, op: bars_op("REFUSED"), class: None });
    assert!(
        matches!(next(&mut s), StreamDown::SubErr { sid: 2, .. }),
        "the second refusal is the first thing on the wire: no held seed of the first is left"
    );
    // ...and the session carries on: the next key subscribes and is preloaded as usual.
    r.store.append_bars("oanda", "EURUSD", "1m", &bars(0, 9), None).unwrap();
    let frames = subscribe(&mut s, 3, "EURUSD");
    assert_eq!(seeds(&frames).len(), 2);
}

/// A history channel whose SECOND fetch waits for the test: the first answers at once.
struct Gate {
    bars: Vec<Bar>,
    calls: AtomicUsize,
    started: Mutex<mpsc::Sender<()>>,
    go: Mutex<mpsc::Receiver<()>>,
}

impl GapSource for Gate {
    fn fetch(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, String> {
        if self.calls.fetch_add(1, Ordering::SeqCst) >= 1 {
            let _ = self.started.lock().unwrap().send(());
            let _ = self.go.lock().unwrap().recv();
        }
        Ok(self.bars.iter().filter(|b| b.ts >= start_ms && b.ts <= end_ms).cloned().collect())
    }
}

/// **A session that joins a LIVE key is handed its start history before any close of that key.** Its history
/// is prepared (here: held in the channel) before its route is published, so a close the venue emits meanwhile
/// is not queued ahead of the seed - the seed would swallow it, since a seed replaces the series whole. This
/// pins the ORDER only: a close that lands inside the preload window itself (at most one second) reaches
/// neither the joiner's seed nor its route, and a joiner gets no venue seed to cover it (the crate's CLAUDE.md).
#[test]
fn a_session_that_joins_a_live_key_is_handed_its_start_history_before_any_close() {
    let (started_tx, started_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let gate = Arc::new(Gate {
        bars: bars(0, 9),
        calls: AtomicUsize::new(0),
        started: Mutex::new(started_tx),
        go: Mutex::new(go_rx),
    });
    // No venue seed: a seed would have the saver write its bars into the store, and the joiner's history
    // would then have nothing to fetch.
    let r = rig_over(Some(true), Vec::new(), gate, clock_at(9));
    r.store.append_bars("oanda", "EURUSD", "1m", &bars(0, 6), None).unwrap();
    let mut first = lane(r.addr);
    let _ = subscribe(&mut first, 1, "EURUSD"); // the first holder: its fetch answers at once

    let mut joiner = lane(r.addr);
    up(&mut joiner, &StreamUp::Sub { sid: 1, op: bars_op("EURUSD"), class: None });
    started_rx.recv_timeout(WAIT).expect("the joiner's history fetch started");
    r.venue_sink().close_bar("oanda", "EURUSD", "1m", bar_at(9)); // the live key closes a bar meanwhile
    go_tx.send(()).unwrap();
    let mut frames = Vec::new();
    loop {
        let frame = next(&mut joiner);
        let done = matches!(frame, StreamDown::Status { .. });
        frames.push(frame);
        if done {
            break;
        }
    }
    let first_data =
        frames.iter().find(|f| matches!(f, StreamDown::Seed { .. } | StreamDown::BarClose { .. }));
    assert!(
        matches!(first_data, Some(StreamDown::Seed { .. })),
        "the joiner's first data is its start history: {frames:?}"
    );
}

fn next_raw(s: &mut TcpStream) -> StreamDown {
    read_capped::<_, StreamDown>(s, SL_FRAME_CEILING_BYTES).expect("a server frame reads")
}

// ---- 4. no new door ----------------------------------------------------------------------------------------------

/// **R8: attaching the holder opens no door.** Md and the recorder still cannot subscribe a bar lane through the
/// broker (only the strategy lane can), so a window or a recorder can never be what makes the datahub save or
/// read a bar series; and nothing was saved or compared by asking.
#[test]
fn market_data_and_the_recorder_still_cannot_subscribe_bars() {
    let r = rig(Some(true), bars(0, 3), bars(0, 9), clock_at(9));
    for holder in [Holder::Md, Holder::Rec] {
        let mut handle = r.broker.handle(holder, "oanda");
        match handle.subscribe_bars("EURUSD", "1m") {
            Err(LiveDataError::Unsupported(why)) => {
                assert!(why.contains("tick lanes only"), "{why}")
            }
            other => panic!("{holder:?}: expected Unsupported, got {other:?}"),
        }
    }
    let h = r.history();
    assert!(h.settle(WAIT));
    assert!(stored(&r, "EURUSD").is_empty());
    assert_eq!(h.stats(), vike_datahub::strat::history::StartHistoryStats::default());
    assert!(h.reports().is_empty());
}

// ---- 5. the production history channel, over a scripted gatewayhub -----------------------------------------------

/// A scripted gatewayhub rest lane whose credentialed OANDA klines answer `rows`. The kit derives the daemon's
/// advertisement from its scripts, and the real daemon always serves keyless klines beside the credentialed
/// lane (the client's `gw_fetch` check looks for them), so a keyless row is scripted too and never asked.
fn scripted_oanda_history(rows: Vec<Bar>) -> ScriptedRest {
    ScriptedRest::new()
        .with_klines("binance", Answer::rows(Vec::new()))
        .with_credentialed_klines("oanda", Answer::rows(rows))
}

/// The credentialed klines the scripted OANDA history lane received: `(symbol, interval, start_ms, end_ms)`.
fn credentialed_klines(rest: &ScriptedRest) -> Vec<(String, String, i64, i64)> {
    rest.fetches()
        .into_iter()
        .filter(|call| call.credentialed)
        .filter_map(|call| match call.what {
            FetchWhat::Klines { symbol, interval, start_ms, end_ms, .. } => {
                Some((symbol, interval, start_ms, end_ms))
            }
            _ => None,
        })
        .collect()
}

/// **The production gap source asks OANDA by OANDA's own name, for exactly the span, on the credentialed
/// lane, and nobody else**: the strategy's `EURUSD` goes out as `EUR_USD` (the history lane's spelling), the
/// window is the asked span, and a venue it has no line for is refused locally with nothing sent.
#[test]
fn the_gateway_gap_asks_oanda_by_its_own_name_for_exactly_the_span_and_nobody_else() {
    let rest = scripted_oanda_history(bars(7, 10));
    let gateway = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());
    let gap = GatewayGap::new(gateway.link());

    let got = gap.fetch("oanda", "EURUSD", "1m", bar_at(7).ts, bar_at(9).ts).expect("served");
    assert_eq!(times(&got), vec![7, 8, 9]);
    assert_eq!(
        credentialed_klines(&rest),
        vec![("EUR_USD".to_string(), "1m".to_string(), bar_at(7).ts, bar_at(9).ts)],
        "OANDA's own name, the asked span, the credentialed verb"
    );

    let other = gap.fetch("binance", "BTCUSDT", "1m", bar_at(7).ts, bar_at(9).ts);
    assert!(other.as_ref().is_err_and(|e| e.contains("no history channel")), "{other:?}");
    assert_eq!(rest.fetches().len(), 1, "a venue with no line asked nobody");
}

/// **The whole served path with the production channel**: the store holds the first bars, the lane preloads
/// them, the gatewayhub is asked once for ONLY the span after the store's newest bar (and not for what the store
/// has), and the tradehub reads the joined history before the venue's own seed.
#[test]
fn a_served_lane_with_the_production_channel_asks_the_gatewayhub_only_for_the_missing_span() {
    let rest = scripted_oanda_history(bars(6, 9));
    let gateway = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());
    let gap: Arc<dyn GapSource> = Arc::new(GatewayGap::new(gateway.link()));
    let r = rig_over(Some(true), bars(7, 9), gap, clock_at(9));
    r.store.append_bars("oanda", "EURUSD", "1m", &bars(0, 6), None).unwrap();

    let mut s = lane(r.addr);
    let frames = subscribe(&mut s, 1, "EURUSD");
    let got = seeds(&frames);
    assert_eq!(got.len(), 2, "the start history, then the venue's seed joined over it: {frames:?}");
    assert_eq!(times(&got[0]), (0..9).collect::<Vec<_>>(), "six stored, three fetched");
    assert_eq!(
        times(&got[1]),
        (0..9).collect::<Vec<_>>(),
        "the venue's short seed did not shrink it"
    );
    assert_eq!(
        credentialed_klines(&rest),
        vec![("EUR_USD".to_string(), "1m".to_string(), bar_at(6).ts, clock_at(9) - STEP)],
        "one request, for the span after the store's newest bar and nothing the store holds"
    );
}
