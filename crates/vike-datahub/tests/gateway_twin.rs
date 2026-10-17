//! The TWIN: the datahub's consumer code run down TWO paths against ONE scripted venue behaviour, and
//! the two answers compared bit for bit (drift register R2 to R4 of the wave-1 plan, decision 0164).
//!
//! * **Path A, the pre-change shape (`Route::InProcess`).** The scripted venue client sits in the feed
//!   broker's own client table, in this process, exactly where a bridge's client sat before the move.
//! * **Path B, the new shape (`Route::Hop`).** The SAME scripted client sits behind a loopback
//!   gatewayhub (`vike_gatewayhub::testkit::gateway::LoopbackGateway`, the real server over scripted
//!   lanes) and the datahub's consumer code is handed `vike_gatewayhub_client`'s remote seams instead
//!   of a client: `RemoteDataClient` under the broker, `RemoteCatalog`, `RemoteOptions` and its feeds,
//!   `RemoteRollingFamily`. Where the build arms the production table
//!   (`vike_datahub::feeds::venues::real_client_table`, every `venue-*` feature of the `datahub-all`
//!   job) path B builds its clients through it; a narrower build builds the same one-line
//!   `RemoteDataClient`, because the table would refuse the venue by name.
//!
//! What is compared, lane by lane (the doc of each `mod` below says what it holds):
//!
//! | lane | consumer code under test | compared |
//! |---|---|---|
//! | `feeds` | `FeedBroker` (Md and Rec holders, refcount, `RoutingSink`), the md hub's frame builder | the recorder sink's calls (`{:?}`, so f64 bits), the md hub's wire frames, the venue client's call log |
//! | `recorded_rows` | the recorder's row writer (`RecorderSink` into `DataFusionHist`) | every scanned row, the series list, the commit keys |
//! | `recording_feeds` | `build_recording_feed`, `RecorderRuntime` | the resolvers' answers, the runtime's tick reports, the venue client's call log |
//! | `catalog_lane` | `real_catalog_table`, `CatalogLane`, the verb | every answer the verb gives a client |
//! | `options_lane` | the chain pass, `record_chain`, the strike window, the feed rows | the served chain, the recorded `kind=chain` rows, the feed rows |
//! | `backfill_lane` | `real_backfill_table` (the klines fold, the funding and tick rows, the credentialed row, the presence probe) against the pre-change rows over in-process sources | each answer (`{:?}`, in request order), what the source was asked, every stored row, the series list, the commit keys; the table's three venue lists; the presence word |
//!
//! The chart seed reaches the same kline collectors through `BackfillTable::get_for_seed` (the
//! `backfill_lane` table test pins which venues it admits), and the history-channels report reads the
//! presence probe that lane's last test holds.
//!
//! # What may differ, and is therefore normalised or asserted as a relation instead of equality
//!
//! * **Clocks.** The md hub stamps a folded `book` frame with its own receipt clock, a stream-status
//!   `Live` row of the recorder is filed under `now`, and a link loss is stamped with each lane's last
//!   delivered frame or, for a lane that delivered none, the instant its subscription was
//!   acknowledged. Those fields are blanked (named at the site), masked, or asserted as a relation (a
//!   `Live` names the gap its `GapStart` opened), never compared across the two paths.
//! * **Conflation.** `forming_bar`, `mark_tick`, `bar_close_tick` and `l2_snapshot` are conflating on the
//!   wire by design: under backlog an intermediate state may be superseded. The corpus plays each such
//!   verb once per key, and `a_conflating_lane_may_skip_but_never_the_latest_and_never_reorders` holds the
//!   law itself.
//! * **Texts of a NEW failure mode** (a gatewayhub that is down) wrap the lane's words; the lane's
//!   own words are asserted to survive inside them.
//! * **A non-finite float** cannot cross JSON: in process it is delivered, over the hop the row is
//!   refused and its absence is disclosed as a hole (`a_non_finite_float_is_not_carried_and_the_hole_is_disclosed`).
//! * **Overflow** has no in-process twin (a function call has no queue): the two tests that end the
//!   `feeds` module hold the new failure mode itself, at the datahub's consumers.
//! * **The flush counters** of the recorder's writer are handed out in a hash order of the final
//!   flush, so `recorded_rows` compares them as a set.
//!
//! # A twin that cannot fail for its stated reason is a defect
//!
//! Every test has a non-vacuity floor (a count the scripted behaviour is known to produce, asserted on
//! BOTH paths), and the comparison is also made against the script itself where the script is the
//! ground truth, so two paths that agree on nothing do not pass. The mutation results are in the
//! task notes.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_data::{
    DataClient, LiveDataError, LiveDataSink, RecordingSink, SinkCall, StreamLabel, SubscriptionId,
};
use vike_datahub::feeds::venues::{real_client_table, sharing_for};
use vike_datahub::feeds::{ClientBuilder, FeedBroker, FeedHandle, Holder};
use vike_datahub::md::mailbox::{Mailbox, Recv};
use vike_datahub::md::{MD_LINGER, MarketClientBuilder, MdHub};
use vike_datahub_client::market::{MdFrame, MdLane, MdSpec};
use vike_datahub_client::proto::{Response, write_frame};
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub::testkit::data::{DataCall, Refusal, ScriptedData, SubVerb, SubscribeCall};
use vike_gatewayhub::testkit::events::{
    canonical_script, delta, exact_line, exact_lines_of, quote, symbol_of, trade, verb_of,
};
use vike_gatewayhub::testkit::gateway::LoopbackGateway;
use vike_gatewayhub::testkit::rest::ScriptedRest;
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_market_stream::market::RemoteDataClient;
use vike_marketdata::{Bar, BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};

/// The reconcile clock every hub in this file runs on (the hub takes its clock as a parameter).
const NOW: i64 = 1_780_387_200_000;

/// When the scripted venue "went down": the stamp a `GapStart` carries and its `Live` names.
const GAP_AT: i64 = 1_780_387_200_123;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ====================================================================================================
// The two routes
// ====================================================================================================

/// Which path a rig runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// Path A: the scripted client in the broker's table, in process.
    InProcess,
    /// Path B: the same scripted client behind a loopback gatewayhub.
    Hop,
}

/// Which venues the production table builds a remote client for in THIS build: the `venue-<v>`
/// feature of each arm (`crates/vike-datahub/src/feeds/venues.rs`'s `build_client`).
const ARMED: [(&str, bool); 7] = [
    ("binance", cfg!(feature = "venue-binance")),
    ("bybit", cfg!(feature = "venue-bybit")),
    ("okx", cfg!(feature = "venue-okx")),
    ("aster", cfg!(feature = "venue-aster")),
    ("hyperliquid", cfg!(feature = "venue-hyperliquid")),
    ("polymarket", cfg!(feature = "venue-polymarket")),
    ("deribit", cfg!(feature = "venue-deribit")),
];

fn production_arms(venue: &str) -> bool {
    ARMED.iter().any(|(v, on)| *on && *v == venue)
}

/// Path A's client table: the scripted client, built in process, with no server in the picture.
fn in_process_builder(data: ScriptedData) -> ClientBuilder {
    Box::new(move |venue, sink| {
        data.build(venue, None, sink).map(|client| Box::new(client) as Box<dyn DataClient + Send>)
    })
}

/// Path B's client table: the datahub's production table where this build arms the venue, the same
/// one-line remote client where it does not.
fn hop_builder(link: &GatewayLink) -> ClientBuilder {
    let production = real_client_table(link);
    let link = link.clone();
    Box::new(move |venue, sink| {
        if production_arms(venue) {
            return production(venue, sink);
        }
        RemoteDataClient::new(GatewayOpener::new(link.clone()), venue, sink)
            .map(|client| Box::new(client) as Box<dyn DataClient + Send>)
            .map_err(|e| format!("the gatewayhub client could not start its reader thread: {e}"))
    })
}

/// Md's client builder over the process's one broker: the two lines of
/// `vike_datahub::md::venues::market_builder` (attach the hub's sink for `Holder::Md`, hand back the
/// broker's handle), with the sink wrapped so the test can tell when a verb has been delivered.
fn md_builder(broker: Arc<FeedBroker>, seen: Arc<AtomicUsize>) -> MarketClientBuilder {
    Box::new(
        move |venue: &str,
              sink: Arc<dyn LiveDataSink>|
              -> Result<Box<dyn DataClient + Send>, String> {
            broker.attach_sink(Holder::Md, Arc::new(Settled::new(sink, Arc::clone(&seen))));
            Ok(Box::new(broker.handle(Holder::Md, venue)))
        },
    )
}

// ====================================================================================================
// The delivery witness
// ====================================================================================================

/// Forwards every verb to `inner`, THEN counts it. The hop delivers on a reader thread, so a test
/// cannot read a result the instant the venue side emitted: it waits for this count, and the count
/// is taken after the consumer's own code ran.
///
/// All twelve verbs are spelled, none inherited: a defaulted verb here would be a delivery that
/// never counts and a test that waits for it forever.
struct Settled {
    inner: Arc<dyn LiveDataSink>,
    seen: Arc<AtomicUsize>,
}

impl Settled {
    fn new(inner: Arc<dyn LiveDataSink>, seen: Arc<AtomicUsize>) -> Self {
        Self { inner, seen }
    }

    fn done(&self) {
        self.seen.fetch_add(1, Ordering::SeqCst);
    }
}

impl LiveDataSink for Settled {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.inner.seed_bars(venue, symbol, interval, bars);
        self.done();
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.close_bar(venue, symbol, interval, bar);
        self.done();
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.forming_bar(venue, symbol, interval, bar);
        self.done();
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.mark_tick(venue, symbol, px, ts);
        self.done();
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.bar_close_tick(venue, symbol, px, ts);
        self.done();
    }
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
        ts: i64,
    ) {
        self.inner.l2_snapshot(venue, symbol, tick_size, bids, asks, ts);
        self.done();
    }
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        self.inner.quote(venue, symbol, quote);
        self.done();
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        self.inner.trade(venue, symbol, trade);
        self.done();
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.inner.book(venue, symbol, book);
        self.done();
    }
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        self.inner.book_update(venue, symbol, update);
        self.done();
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.inner.stream_status(venue, symbol, stream, status);
        self.done();
    }
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: DataDelay) {
        self.inner.data_delay(venue, symbol, stream, delay);
        self.done();
    }
}

/// A sink whose `trade` blocks while the gate is closed: a consumer that has stopped consuming. It
/// parks the hop's reader thread (which delivers inline), so the daemon's queue for the session fills.
struct GatedSink {
    inner: Arc<dyn LiveDataSink>,
    gate: Arc<Gate>,
}

/// The gate a [`GatedSink`] waits at. Opens on drop of its [`GateGuard`], so a failing test cannot
/// leave a reader thread parked for ever.
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn closed() -> Arc<Self> {
        Arc::new(Self { open: Mutex::new(false), changed: Condvar::new() })
    }

    fn open(&self) {
        *lock(&self.open) = true;
        self.changed.notify_all();
    }

    fn pass(&self) {
        let mut open = lock(&self.open);
        while !*open {
            open = self.changed.wait(open).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

struct GateGuard(Arc<Gate>);

impl Drop for GateGuard {
    fn drop(&mut self) {
        self.0.open();
    }
}

impl LiveDataSink for GatedSink {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.inner.seed_bars(venue, symbol, interval, bars);
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.close_bar(venue, symbol, interval, bar);
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.forming_bar(venue, symbol, interval, bar);
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.mark_tick(venue, symbol, px, ts);
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.bar_close_tick(venue, symbol, px, ts);
    }
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
        ts: i64,
    ) {
        self.inner.l2_snapshot(venue, symbol, tick_size, bids, asks, ts);
    }
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        self.inner.quote(venue, symbol, quote);
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        self.gate.pass();
        self.inner.trade(venue, symbol, trade);
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.inner.book(venue, symbol, book);
    }
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        self.inner.book_update(venue, symbol, update);
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.inner.stream_status(venue, symbol, stream, status);
    }
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: DataDelay) {
        self.inner.data_delay(venue, symbol, stream, delay);
    }
}

// ====================================================================================================
// The rig: one broker, one md hub, one recorder sink, on one route
// ====================================================================================================

/// One route's datahub plumbing: the broker both planes take their clients from, the md hub over it,
/// and the recorder plane's sink attached for `Holder::Rec`, the way `crate::recorder::record` attaches
/// it before any feed exists.
///
/// ⚠ Field order IS drop order. `gateway` is last so it outlives every client it serves, and a test
/// declares its handles and session guards AFTER the rig so they drop first.
struct Rig {
    route: Route,
    hub: Arc<MdHub>,
    broker: Arc<FeedBroker>,
    /// What the recorder plane received (unless a test handed in its own inner sink).
    rec: Arc<RecordingSink>,
    md_seen: Arc<AtomicUsize>,
    rec_seen: Arc<AtomicUsize>,
    md_due: AtomicUsize,
    rec_due: AtomicUsize,
    data: ScriptedData,
    /// Never read: it is here to be dropped LAST.
    _gateway: Option<LoopbackGateway>,
}

impl Rig {
    fn new(route: Route, venues: &[&str]) -> Self {
        Self::build(route, venues, ServerConfig::PRODUCTION, None)
    }

    /// `config` is the daemon's (hop only); `rec_inner` replaces the recording sink behind the
    /// recorder plane (the store test hands in a `RecorderSink`).
    fn build(
        route: Route,
        venues: &[&str],
        config: ServerConfig,
        rec_inner: Option<Arc<dyn LiveDataSink>>,
    ) -> Self {
        Self::assemble(route, venues, config, rec_inner, ScriptedRest::new())
    }

    /// [`Rig::build`] with a scripted REST lane behind the hop (the recording lane's resolvers ask the
    /// daemon for a listing or a rolling family).
    fn assemble(
        route: Route,
        venues: &[&str],
        config: ServerConfig,
        rec_inner: Option<Arc<dyn LiveDataSink>>,
        rest: ScriptedRest,
    ) -> Self {
        let data = ScriptedData::new(venues);
        let gateway =
            (route == Route::Hop).then(|| LoopbackGateway::start_with(data.clone(), rest, config));
        let builder = match &gateway {
            Some(gateway) => hop_builder(&gateway.link()),
            None => in_process_builder(data.clone()),
        };
        let broker = FeedBroker::new(builder, sharing_for);
        let rec = Arc::new(RecordingSink::default());
        let rec_seen = Arc::new(AtomicUsize::new(0));
        let rec_inner = rec_inner.unwrap_or_else(|| Arc::clone(&rec) as Arc<dyn LiveDataSink>);
        broker.attach_sink(Holder::Rec, Arc::new(Settled::new(rec_inner, Arc::clone(&rec_seen))));
        let md_seen = Arc::new(AtomicUsize::new(0));
        let hub = MdHub::new(
            md_builder(Arc::clone(&broker), Arc::clone(&md_seen)),
            venues.iter().map(|v| (*v).to_string()).collect(),
        );
        Self {
            route,
            hub,
            broker,
            rec,
            md_seen,
            rec_seen,
            md_due: AtomicUsize::new(0),
            rec_due: AtomicUsize::new(0),
            data,
            _gateway: gateway,
        }
    }

    /// The link to the gatewayhub (the hop route only); only the recording feeds build from one.
    #[cfg(any(feature = "record-polymarket", feature = "record-binance"))]
    fn link(&self) -> GatewayLink {
        self._gateway.as_ref().expect("the hop route has a gateway").link()
    }

    /// The recorder plane's handle on `venue`: an ordinary `DataClient`.
    fn rec_handle(&self, venue: &str) -> FeedHandle {
        self.broker.handle(Holder::Rec, venue)
    }

    /// Emit `script` into the venue client behind `session` as the venue would, on a SHARED venue:
    /// the broker's routing sink fans it out, and the md plane sees every verb.
    fn play_shared(&self, script: &[SinkCall]) {
        self.data.session(0).emit_all(script);
        self.md_due.fetch_add(script.len(), Ordering::SeqCst);
    }

    /// Emit `script` into the md plane's OWN client of a per-holder venue (`session` is its index).
    fn play_md_session(&self, session: usize, script: &[SinkCall]) {
        self.data.session(session).emit_all(script);
        self.md_due.fetch_add(script.len(), Ordering::SeqCst);
    }

    /// Emit `script` into the recorder plane's OWN client of a per-holder venue.
    fn play_rec_session(&self, session: usize, script: &[SinkCall]) {
        self.data.session(session).emit_all(script);
        self.rec_due.fetch_add(script.len(), Ordering::SeqCst);
    }

    /// Wait until every verb played so far has been delivered to its plane, then watch a moment for a
    /// stray: an extra event shows up HERE, not as a puzzling count in the next phase.
    fn settle(&self) {
        self.wait_delivered();
        std::thread::sleep(Duration::from_millis(120));
        self.assert_delivered_exactly();
    }

    fn wait_delivered(&self) {
        let (md, rec) = (self.md_due.load(Ordering::SeqCst), self.rec_due.load(Ordering::SeqCst));
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.md_seen.load(Ordering::SeqCst) < md || self.rec_seen.load(Ordering::SeqCst) < rec
        {
            assert!(
                Instant::now() < deadline,
                "{:?}: delivery stalled: md {}/{md}, recorder {}/{rec} events arrived",
                self.route,
                self.md_seen.load(Ordering::SeqCst),
                self.rec_seen.load(Ordering::SeqCst)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn assert_delivered_exactly(&self) {
        let (md, rec) = (self.md_due.load(Ordering::SeqCst), self.rec_due.load(Ordering::SeqCst));
        assert_eq!(
            self.md_seen.load(Ordering::SeqCst),
            md,
            "{:?}: the md plane received events nobody emitted",
            self.route
        );
        if rec > 0 {
            assert_eq!(
                self.rec_seen.load(Ordering::SeqCst),
                rec,
                "{:?}: the recorder plane received events nobody emitted",
                self.route
            );
        }
    }

    /// Wait until the recorder plane has received `n` verbs in all (a SHARED venue's recorder gets a
    /// filtered subset of what was emitted, so a test that knows the subset says how many), then
    /// confirm no more follow.
    fn settle_rec(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.rec_seen.load(Ordering::SeqCst) < n {
            assert!(
                Instant::now() < deadline,
                "{:?}: the recorder plane received {} of {n} events",
                self.route,
                self.rec_seen.load(Ordering::SeqCst)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(
            self.rec_seen.load(Ordering::SeqCst),
            n,
            "{:?}: the recorder plane received more than the {n} events that were routed to it",
            self.route
        );
    }

    /// A round trip on the recorder's client of `venue`, after which every frame sent to that client
    /// before it has been handled by the venue side (one socket, one reader: frames are in order).
    /// In process it is just a subscribe. The throwaway key is unsubscribed again and named so
    /// [`Rig::subscribes`] leaves it out.
    fn barrier(&self, handle: &mut FeedHandle, tag: &str) {
        let id = handle
            .subscribe_trades(&format!("BARRIER-{tag}"))
            .unwrap_or_else(|e| panic!("{:?}: the barrier subscribe failed: {e}", self.route));
        handle.unsubscribe(id);
    }

    /// `symbol`'s subscribe calls the venue side received, in order, by verb.
    fn subscribes(&self, symbol: &str) -> Vec<String> {
        self.data
            .log()
            .into_iter()
            .filter_map(|(_, call)| match call {
                DataCall::Subscribe(SubscribeCall { verb, symbol: s, .. }) if s == symbol => {
                    Some(verb.as_str().to_string())
                }
                _ => None,
            })
            .collect()
    }

    /// The venue side's whole call log, `session: call`, in arrival order.
    fn log_lines(&self) -> Vec<String> {
        self.data
            .log()
            .into_iter()
            .map(|(session, call)| format!("session {session}: {call}"))
            .collect()
    }
}

// ====================================================================================================
// Comparison helpers
// ====================================================================================================

/// Rows keyed by what they belong to, each key's rows in the order they arrived.
type Keyed = BTreeMap<String, Vec<String>>;

/// Every recorded call as its exact (`{:?}`, so bit-exact for an f64) line, filed under its symbol.
fn by_symbol(calls: &[SinkCall]) -> Keyed {
    let mut out = Keyed::new();
    for call in calls {
        out.entry(symbol_of(call).to_string()).or_default().push(exact_line(call));
    }
    out
}

/// Two keyed collections are the same: same keys, same count under each, same line at every position.
/// The failure names the key and the position, which is the order contract.
fn assert_same(what: &str, in_process: &Keyed, hop: &Keyed) {
    assert_eq!(
        in_process.keys().collect::<Vec<_>>(),
        hop.keys().collect::<Vec<_>>(),
        "{what}: the paths saw different keys"
    );
    for (key, want) in in_process {
        let have = &hop[key];
        assert_eq!(
            have.len(),
            want.len(),
            "{what}/{key}: the hop delivered {} events, the in-process path {}",
            have.len(),
            want.len()
        );
        for (i, (w, h)) in want.iter().zip(have).enumerate() {
            assert_eq!(h, w, "{what}/{key}: event {i} differs between the paths");
        }
    }
}

/// The frames of one tick, keyed by the (lane, venue, symbol) they are for, each as the JSON the wire
/// carries. Within a key the order is the delivery order; across keys it is NOT compared (the hub
/// walks its keys in a hash order of its own).
type Frames = Keyed;

/// Decode one framed mailbox payload back into the `MdFrame` it carries.
fn decode(bytes: &[u8]) -> MdFrame {
    assert!(bytes.len() > 4, "a framed payload carries a 4-byte length prefix");
    let response: Response = serde_json::from_slice(&bytes[4..]).expect("decode Response");
    match response {
        Response::Md(frame) => *frame,
        other => panic!("a stream mailbox carries only Response::Md, got {other:?}"),
    }
}

/// Drain a session's mailbox in delivery order, synthesizing the writer's `TapeGap` where
/// `run_market_writer` would.
fn drain(mailbox: &Mailbox) -> Vec<MdFrame> {
    let mut out = Vec::new();
    loop {
        match mailbox.recv_timeout(Duration::from_millis(1)) {
            Recv::Frame { bytes, owed_gap } => {
                if let Some((key, dropped, from_seq, to_seq)) = owed_gap {
                    out.push(MdFrame::TapeGap {
                        venue: key.venue.clone(),
                        symbol: key.symbol.clone(),
                        dropped,
                        from_seq,
                        to_seq,
                    });
                }
                out.push(decode(&bytes));
            }
            _ => return out,
        }
    }
}

/// The hub stamps a folded `book` frame with ITS OWN receipt clock (`BookSnapshot::venue_ts`'s doc), so
/// that one field is blanked before frames are compared. Nothing else in a frame is a clock.
fn normalized(frame: MdFrame) -> MdFrame {
    match frame {
        MdFrame::Book(mut snapshot) => {
            snapshot.venue_ts = 0;
            MdFrame::Book(snapshot)
        }
        other => other,
    }
}

fn frame_key(frame: &MdFrame) -> String {
    match frame {
        MdFrame::Depth(b) => format!("depth/{}/{}", b.venue, b.symbol),
        MdFrame::Book(b) => format!("book/{}/{}", b.venue, b.symbol),
        MdFrame::Trades { venue, symbol, .. } | MdFrame::TapeGap { venue, symbol, .. } => {
            format!("trades/{venue}/{symbol}")
        }
        MdFrame::Status { venue, symbol, lane, .. } => format!("status/{lane:?}/{venue}/{symbol}"),
        MdFrame::Heartbeat | MdFrame::Bye(_) => "control".to_string(),
    }
}

/// The frame as the wire carries it: the JSON body `write_frame` frames, bit for bit.
fn wire_json(frame: &MdFrame) -> String {
    let mut buf = Vec::new();
    write_frame(&mut buf, &Response::Md(Box::new(frame.clone()))).expect("frame a Response::Md");
    String::from_utf8_lossy(&buf[4..]).into_owned()
}

fn frames_of(mailbox: &Mailbox) -> Frames {
    let mut out = Frames::new();
    for frame in drain(mailbox) {
        let frame = normalized(frame);
        out.entry(frame_key(&frame)).or_default().push(wire_json(&frame));
    }
    out
}

fn spec(venue: &str, symbol: &str, lane: MdLane) -> MdSpec {
    MdSpec { venue: venue.into(), symbol: symbol.into(), lane, depth_levels: None }
}

/// One tick lane subscribe by label (the four verbs the broker carries).
fn subscribe(
    client: &mut dyn DataClient,
    symbol: &str,
    lane: StreamLabel,
) -> Result<SubscriptionId, LiveDataError> {
    match lane {
        StreamLabel::Quotes => client.subscribe_quotes(symbol),
        StreamLabel::Trades => client.subscribe_trades(symbol),
        StreamLabel::Book => client.subscribe_book(symbol),
        StreamLabel::Depth => client.subscribe_depth(symbol),
    }
}

fn status(venue: &str, symbol: &str, lane: StreamLabel, status: StreamStatus) -> SinkCall {
    SinkCall::StreamStatus {
        venue: venue.into(),
        symbol: symbol.into(),
        stream: lane.as_str().into(),
        status,
    }
}

/// The venue "going down" and "coming back" on `keys`: a `GapStart` for each, then a `Live` naming it.
fn gap_scripts(venue: &str, keys: &[(&str, StreamLabel)]) -> (Vec<SinkCall>, Vec<SinkCall>) {
    let down = keys
        .iter()
        .map(|(symbol, lane)| {
            status(venue, symbol, *lane, StreamStatus::GapStart { at_ts_ms: GAP_AT })
        })
        .collect();
    let up = keys
        .iter()
        .map(|(symbol, lane)| {
            status(venue, symbol, *lane, StreamStatus::Live { gap_started_ts_ms: Some(GAP_AT) })
        })
        .collect();
    (down, up)
}

// ====================================================================================================
// Lane: live feeds through the broker, the md hub and the recorder's sink
// ====================================================================================================

/// The corpus of this lane. One venue behaviour (the testkit's canonical script, which plays all twelve
/// `LiveDataSink` verbs with floats whose bits are not short decimals, plus the venue going down and
/// coming back, bursts, refusals and a seed emitted inside `subscribe`) is run down both routes through
/// the datahub's own consumer code: `FeedBroker` (the Md and Rec holders, the refcount, the routing
/// sink), the md hub's reconcile and publish tick, and a recorder-plane sink.
mod feeds {
    use super::*;

    const SYMBOLS: [&str; 2] = ["BTCUSDT", "ETHUSDT"];

    /// What the recorder plane holds on binance (a shared venue): BTCUSDT's trades and depth, ETHUSDT's
    /// trades. Quotes and book are NOT held, so the routing sink must withhold them.
    const REC_KEYS: [(&str, StreamLabel); 3] = [
        ("BTCUSDT", StreamLabel::Trades),
        ("BTCUSDT", StreamLabel::Depth),
        ("ETHUSDT", StreamLabel::Trades),
    ];

    /// What the md plane holds on binance, through the hub.
    const MD_KEYS: [(&str, MdLane); 3] =
        [("BTCUSDT", MdLane::Depth), ("BTCUSDT", MdLane::Trades), ("ETHUSDT", MdLane::Depth)];

    /// Every key either plane holds, for the venue-down switch.
    const GAP_KEYS: [(&str, StreamLabel); 4] = [
        ("BTCUSDT", StreamLabel::Trades),
        ("BTCUSDT", StreamLabel::Depth),
        ("ETHUSDT", StreamLabel::Trades),
        ("ETHUSDT", StreamLabel::Depth),
    ];

    /// What one run of the shared-venue scenario observed.
    struct SharedRun {
        script: Vec<SinkCall>,
        /// The recorder plane's calls after the canonical script, per symbol, exact lines.
        rec_phase1: Keyed,
        /// ... and every call in the one order the recorder saw them in, across symbols.
        rec_order: Vec<String>,
        /// ... and after the venue went down and came back.
        rec_final: Keyed,
        /// The md hub's frames after each of the three phases.
        frames: [Frames; 3],
        produced: [u64; 3],
        log: Vec<String>,
    }

    fn run_shared(route: Route) -> SharedRun {
        let rig = Rig::new(route, &["binance"]);
        // The recorder plane first: its filter bit is published before the real subscribe (rule 2 of
        // `crate::feeds`), and its keys are real subscriptions the md plane then shares.
        let mut rec = rig.rec_handle("binance");
        for (symbol, lane) in REC_KEYS {
            subscribe(&mut rec, symbol, lane)
                .unwrap_or_else(|e| panic!("{route:?}: recorder {symbol} {lane:?}: {e}"));
        }
        let guard = rig.hub.open_session().expect("an md session");
        for (symbol, lane) in MD_KEYS {
            rig.hub
                .acquire(guard.id(), &spec("binance", symbol, lane))
                .unwrap_or_else(|e| panic!("{route:?}: md {symbol} {lane:?}: {e:?}"));
        }
        let report = rig.hub.reconcile(NOW);
        assert_eq!(report.started, MD_KEYS.len(), "{route:?}: {report:?}");
        assert!(report.failed.is_empty(), "{route:?}: {report:?}");

        // Phase 1: the canonical script, every verb, both symbols.
        let script = canonical_script("binance", &SYMBOLS);
        rig.play_shared(&script);
        rig.settle();
        let tick = rig.hub.publish_tick();
        let frames1 = frames_of(guard.mailbox());
        let rec_phase1 = by_symbol(&rig.rec.recorded());
        let rec_order: Vec<String> = rig.rec.recorded().iter().map(exact_line).collect();

        // Phase 2 and 3: the venue goes down and comes back.
        let (down, up) = gap_scripts("binance", &GAP_KEYS);
        rig.play_shared(&down);
        rig.settle();
        let tick2 = rig.hub.publish_tick();
        let frames2 = frames_of(guard.mailbox());
        rig.play_shared(&up);
        rig.settle();
        let tick3 = rig.hub.publish_tick();
        let frames3 = frames_of(guard.mailbox());

        SharedRun {
            script,
            rec_phase1,
            rec_order,
            rec_final: by_symbol(&rig.rec.recorded()),
            frames: [frames1, frames2, frames3],
            produced: [tick.frames_produced, tick2.frames_produced, tick3.frames_produced],
            log: rig.log_lines(),
        }
    }

    /// The calls of `script` the recorder plane is entitled to under [`REC_KEYS`], exact lines: a
    /// trade or a depth snapshot of BTCUSDT, a trade of ETHUSDT.
    fn expected_for_recorder(script: &[SinkCall]) -> Keyed {
        let mut out = Keyed::new();
        for call in script {
            let entitled = matches!(
                (symbol_of(call), verb_of(call)),
                ("BTCUSDT", "trade" | "l2_snapshot") | ("ETHUSDT", "trade")
            );
            if entitled {
                out.entry(symbol_of(call).to_string()).or_default().push(exact_line(call));
            }
        }
        out
    }

    /// **The recorder plane gets the same events, bit for bit and in the same order per key.** The
    /// routing sink withholds what the recorder does not hold, the hop carries what it does, and the
    /// answer is also the script's own lines (not only the other path's).
    #[test]
    fn the_recorder_gets_the_same_events_bit_exact_and_in_order_per_key() {
        vike_log::test_init();
        let a = run_shared(Route::InProcess);
        let b = run_shared(Route::Hop);

        // The floors: the recorder holds BTCUSDT trades and depth (3 + 1 events) and ETHUSDT trades (3).
        assert_eq!(a.rec_phase1["BTCUSDT"].len(), 4, "{:?}", a.rec_phase1);
        assert_eq!(a.rec_phase1["ETHUSDT"].len(), 3, "{:?}", a.rec_phase1);
        assert_eq!(
            b.rec_phase1.len(),
            2,
            "the hop must not invent a key: {:?}",
            b.rec_phase1.keys()
        );

        assert_same("recorder", &a.rec_phase1, &b.rec_phase1);
        // One session is one ordered stream: the order across the two symbols survives the hop too.
        assert_eq!(b.rec_order.len(), a.rec_order.len());
        for (i, (want, have)) in a.rec_order.iter().zip(&b.rec_order).enumerate() {
            assert_eq!(
                have, want,
                "the recorder's call {i} (across symbols) differs between the paths"
            );
        }
        let truth = expected_for_recorder(&a.script);
        assert_same("recorder against the script", &truth, &b.rec_phase1);
        assert_same("recorder against the script (in process)", &truth, &a.rec_phase1);
    }

    /// **The md hub publishes the same frames**: byte-equal wire JSON per key, in the same order within
    /// a key. The only field blanked is the book frame's receipt clock.
    #[test]
    fn the_md_hub_publishes_the_same_frames_over_the_hop() {
        vike_log::test_init();
        let a = run_shared(Route::InProcess);
        let b = run_shared(Route::Hop);

        // The floor: three keys produced a frame (a depth frame per symbol, BTCUSDT's tape).
        assert_eq!(
            a.frames[0].keys().map(String::as_str).collect::<Vec<_>>(),
            ["depth/binance/BTCUSDT", "depth/binance/ETHUSDT", "trades/binance/BTCUSDT"],
            "{:?}",
            a.frames[0]
        );
        assert_eq!(a.produced[0], 3, "one frame per dirty key");
        assert_eq!(b.produced[0], 3, "the hop produced a different number of frames");
        assert_eq!(a.frames[0]["trades/binance/BTCUSDT"].len(), 1, "one batch for the tape");
        assert_same("md frames", &a.frames[0], &b.frames[0]);

        // Ground truth: the frames carry the SCRIPT's numbers, not merely the same numbers on both
        // paths (a consumer-side mutation changes both routes alike and only this sees it).
        let json_to_frame = |json: &str| -> MdFrame {
            match serde_json::from_str::<Response>(json).expect("a Response") {
                Response::Md(frame) => *frame,
                other => panic!("not an md frame: {other:?}"),
            }
        };
        let want_prints: Vec<TradeTick> = a
            .script
            .iter()
            .filter_map(|call| match call {
                SinkCall::Trade { symbol, trade: print, .. } if symbol == "BTCUSDT" => {
                    // The hub's tape drops the per-tick symbol: the frame's envelope carries it once.
                    Some(TradeTick { symbol: String::new(), ..print.clone() })
                }
                _ => None,
            })
            .collect();
        match json_to_frame(&b.frames[0]["trades/binance/BTCUSDT"][0]) {
            MdFrame::Trades { ticks, .. } => {
                assert_eq!(want_prints.len(), 3);
                assert_eq!(format!("{ticks:?}"), format!("{want_prints:?}"), "the tape's prints");
            }
            other => panic!("expected the tape: {other:?}"),
        }
        let want_depth = a.script.iter().find_map(|call| match call {
            SinkCall::L2Snapshot { symbol, tick_size, bids, asks, .. } if symbol == "BTCUSDT" => {
                Some((*tick_size, bids.clone(), asks.clone()))
            }
            _ => None,
        });
        let (tick_size, bids, asks) = want_depth.expect("the script has a BTCUSDT depth snapshot");
        match json_to_frame(&b.frames[0]["depth/binance/BTCUSDT"][0]) {
            MdFrame::Depth(snapshot) => {
                assert_eq!(snapshot.tick_size.to_bits(), tick_size.to_bits());
                assert_eq!(format!("{:?}", snapshot.bids), format!("{bids:?}"), "the depth bids");
                assert_eq!(format!("{:?}", snapshot.asks), format!("{asks:?}"), "the depth asks");
            }
            other => panic!("expected the depth frame: {other:?}"),
        }
    }

    /// **The venue-down switch marks the same gaps on both planes**: a `GapStart` on every held key,
    /// then a `Live` naming the gap. The recorder sees them for the lanes it holds, the md hub as
    /// sticky status frames, one per tick.
    #[test]
    fn a_venue_down_switch_marks_the_same_gaps_in_both_planes() {
        vike_log::test_init();
        let a = run_shared(Route::InProcess);
        let b = run_shared(Route::Hop);

        // The floors. md: a status frame per held md key (a status change also republishes the key's
        // book, which is why the depth frames are here too); the recorder: BTCUSDT trades+depth and
        // ETHUSDT trades.
        let statuses = |frames: &Frames| -> Vec<String> {
            frames
                .iter()
                .filter(|(k, _)| k.starts_with("status/"))
                .flat_map(|(_, v)| v.clone())
                .collect()
        };
        for run in [&a, &b] {
            assert_eq!(statuses(&run.frames[1]).len(), 3, "GapStart frames: {:?}", run.frames[1]);
            assert_eq!(statuses(&run.frames[2]).len(), 3, "Live frames: {:?}", run.frames[2]);
            assert!(statuses(&run.frames[1]).iter().all(|f| f.contains("GapStart")));
            assert!(statuses(&run.frames[2]).iter().all(|f| f.contains("Live")));
            assert_eq!(run.rec_final["BTCUSDT"].len(), 4 + 4, "{:?}", run.rec_final["BTCUSDT"]);
            assert_eq!(run.rec_final["ETHUSDT"].len(), 3 + 2, "{:?}", run.rec_final["ETHUSDT"]);
        }
        assert_same("md GapStart frames", &a.frames[1], &b.frames[1]);
        assert_same("md Live frames", &a.frames[2], &b.frames[2]);
        assert_same("recorder after the gap", &a.rec_final, &b.rec_final);
        assert_eq!(a.produced, b.produced);
    }

    /// **Two holders, one subscribe** (the refcount) and the same call log at the venue client: the
    /// recorder's three keys and the md plane's one new key are four subscribes, not six, on both paths.
    #[test]
    fn two_holders_share_one_subscription_per_key_and_the_venue_sees_the_same_calls() {
        vike_log::test_init();
        let a = run_shared(Route::InProcess);
        let b = run_shared(Route::Hop);
        assert_eq!(
            a.log,
            [
                "session 0: open binance",
                "session 0: subscribe_trades BTCUSDT",
                "session 0: subscribe_depth BTCUSDT",
                "session 0: subscribe_trades ETHUSDT",
                "session 0: subscribe_depth ETHUSDT",
            ],
            "the in-process path is the contract: one subscribe per key"
        );
        assert_eq!(b.log, a.log, "the hop must ask the venue client for exactly the same things");
    }

    // ---- the refcount, release and the two-phase shutdown --------------------------------------

    /// Observations of the refcount scenario, one line per stage.
    fn run_refcount(route: Route) -> (Vec<String>, Vec<String>) {
        let rig = Rig::new(route, &["binance"]);
        let mut obs = Vec::new();
        let mut rec = rig.rec_handle("binance");

        // 1. The recorder takes BTCUSDT trades.
        let id = rec.subscribe_trades("BTCUSDT").expect("rec trades");
        obs.push(format!("1 rec takes trades: real={}", rig.broker.real_subscriptions()));

        // 2. The md plane joins the same key: still one real subscription.
        let mut guard = rig.hub.open_session().expect("an md session");
        rig.hub
            .acquire(guard.id(), &spec("binance", "BTCUSDT", MdLane::Trades))
            .expect("md trades");
        rig.hub.reconcile(NOW);
        rig.barrier(&mut rec, "joined");
        obs.push(format!(
            "2 md joins: real={} venue subscribes={:?}",
            rig.broker.real_subscriptions(),
            rig.subscribes("BTCUSDT")
        ));

        // 3. The recorder lets go: the md plane still holds it, so nothing is unsubscribed.
        rec.unsubscribe(id);
        rig.barrier(&mut rec, "rec-left");
        // The shared key is the session's first subscription, so the venue client's id for it is 1.
        let unsubs_of_the_shared_key = |rig: &Rig| {
            rig.data.log().iter().filter(|(_, c)| *c == DataCall::Unsubscribe(1)).count()
        };
        obs.push(format!(
            "3 rec lets go: real={} venue unsubscribes of the shared key={}",
            rig.broker.real_subscriptions(),
            unsubs_of_the_shared_key(&rig)
        ));

        // 4. The recorder begins its shutdown while the md plane still holds the key: it is not the
        // sole holder, so the venue client must not be told (rule 1 of `crate::feeds`).
        let held = rec.subscribe_trades("BTCUSDT").expect("rec re-takes trades");
        rec.begin_shutdown();
        rig.barrier(&mut rec, "not-sole");
        let session = rig.data.session(0);
        obs.push(format!(
            "4 begin_shutdown while shared: venue begin_shutdowns={}",
            session.begin_shutdowns()
        ));
        rec.unsubscribe(held);

        // 5. The md plane releases and the hub reaps after its linger: the key is unsubscribed ONCE,
        // after a begin_shutdown (the hub reaps a venue's whole live set two-phase).
        guard.release_at(NOW);
        rig.hub.reconcile(NOW + MD_LINGER.as_millis() as i64 + 1_000);
        session.wait_for_call(|c| *c == DataCall::Unsubscribe(1));
        let before = session.begin_shutdowns();
        obs.push(format!(
            "5 md releases: real={} unsubscribes of the shared key={} begin_shutdowns>=1: {}",
            rig.broker.real_subscriptions(),
            unsubs_of_the_shared_key(&rig),
            before >= 1
        ));

        // 6. A SOLE holder's begin_shutdown reaches the venue client, and the broker then refuses new
        // keys as transient until the drain ends.
        let depth = rec.subscribe_depth("ETHUSDT").expect("rec depth");
        rec.begin_shutdown();
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.begin_shutdowns() <= before {
            assert!(
                Instant::now() < deadline,
                "{route:?}: a sole holder's begin_shutdown never arrived"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        obs.push(format!(
            "6 sole begin_shutdown: refuses -> {:?}",
            rec.subscribe_trades("SOLUSDT")
        ));
        rec.unsubscribe(depth);
        obs.push(format!(
            "6 drain ended: accepts -> {:?}",
            rec.subscribe_trades("SOLUSDT").map(|_| ())
        ));
        (obs, rig.log_lines())
    }

    /// **Release, the refcount and `begin_shutdown` mean the same on both paths.** One subscribe for
    /// two holders, no unsubscribe while a holder remains, a begin_shutdown that reaches the venue
    /// client only for a sole holder, and the same transient refusal while draining.
    #[test]
    fn release_refcount_and_begin_shutdown_mean_the_same_on_both_paths() {
        vike_log::test_init();
        let (a_obs, a_log) = run_refcount(Route::InProcess);
        let (b_obs, b_log) = run_refcount(Route::Hop);

        // Non-vacuity, read off the in-process path (the contract) as literal expectations.
        assert_eq!(
            a_obs[0..3],
            [
                "1 rec takes trades: real=1",
                "2 md joins: real=1 venue subscribes=[\"trades\"]",
                "3 rec lets go: real=1 venue unsubscribes of the shared key=0",
            ]
        );
        assert!(a_obs[3].ends_with("venue begin_shutdowns=0"), "{a_obs:?}");
        assert_eq!(
            a_obs[4],
            "5 md releases: real=0 unsubscribes of the shared key=1 begin_shutdowns>=1: true"
        );
        assert!(a_obs[5].contains("draining"), "the sole holder's drain must refuse: {a_obs:?}");
        assert!(a_obs[6].contains("Ok"), "the drain ends when the key is released: {a_obs:?}");

        assert_eq!(b_obs, a_obs, "the hop changed what a holder observes");
        assert_eq!(b_log, a_log, "the hop changed what the venue client was asked");
        // The unsubscribe of the shared key is ONE call, and only after the md plane left.
        let unsub1 = a_log.iter().filter(|l| l.ends_with("unsubscribe 1")).count();
        assert_eq!(unsub1, 1, "{a_log:?}");
    }

    // ---- a per-holder venue --------------------------------------------------------------------

    struct PerHolderRun {
        rec: Keyed,
        frames: Frames,
        rec_session: Vec<String>,
        md_session: Vec<String>,
    }

    fn run_per_holder(route: Route) -> PerHolderRun {
        let rig = Rig::new(route, &["polymarket"]);
        let mut rec = rig.rec_handle("polymarket");
        rec.subscribe_book("TOKEN-A").expect("rec book");
        rec.subscribe_trades("TOKEN-A").expect("rec trades");
        let mut guard = rig.hub.open_session().expect("an md session");
        for lane in [MdLane::Book, MdLane::Trades] {
            rig.hub.acquire(guard.id(), &spec("polymarket", "TOKEN-A", lane)).expect("md key");
        }
        let report = rig.hub.reconcile(NOW);
        assert_eq!((report.started, report.failed.len()), (2, 0), "{route:?}: {report:?}");
        assert_eq!(rig.data.opened(), 2, "{route:?}: a per-holder venue is one client per holder");

        // The venue emits on each client; each plane's sink gets its own client's events, unfiltered.
        let script = canonical_script("polymarket", &["TOKEN-A"]);
        rig.play_rec_session(0, &script);
        rig.play_md_session(1, &script);
        rig.settle();
        rig.hub.publish_tick();
        let frames = frames_of(guard.mailbox());
        let rec_lines = by_symbol(&rig.rec.recorded());

        // The md plane lets go and the hub reaps: ITS client unsubscribes, the recorder's never does.
        guard.release_at(NOW);
        rig.hub.reconcile(NOW + MD_LINGER.as_millis() as i64 + 1_000);
        let md_session = rig.data.session(1);
        md_session.wait_for_call(|c| *c == DataCall::Unsubscribe(1));
        md_session.wait_for_call(|c| *c == DataCall::Unsubscribe(2));
        // A round trip on the recorder's client proves nothing is still in flight towards it.
        rec.subscribe_trades("TOKEN-B").expect("rec barrier");
        let rec_session = rig.data.session(0);
        let lines = |s: &vike_gatewayhub::testkit::data::ScriptedSession| {
            s.calls().iter().map(ToString::to_string).collect::<Vec<_>>()
        };
        // The md hub starts and reaps its keys in a hash order of its own, so its client's calls are
        // compared as a set; the recorder's client is driven in program order and is compared as one.
        let mut md_calls = lines(&md_session);
        md_calls.sort();
        PerHolderRun {
            rec: rec_lines,
            frames,
            rec_session: lines(&rec_session),
            md_session: md_calls,
        }
    }

    /// **A per-holder venue (polymarket) is one client per holder over the hop too**: each plane's
    /// events arrive on its own client and nowhere else, the md plane's release unsubscribes only its
    /// own client, and every event is bit-exact.
    #[test]
    fn a_per_holder_venue_gets_one_client_each_and_never_cross_talks() {
        vike_log::test_init();
        let a = run_per_holder(Route::InProcess);
        let b = run_per_holder(Route::Hop);

        // The floors: the recorder's own client delivered all 12 verbs x 1 symbol (+ the md client's
        // are NOT in it), the md hub framed the book and the tape.
        let canon = canonical_script("polymarket", &["TOKEN-A"]);
        assert_eq!(
            a.rec["TOKEN-A"].len(),
            canon.len(),
            "the per-holder recorder sink is unfiltered"
        );
        assert_eq!(
            a.frames.keys().map(String::as_str).collect::<Vec<_>>(),
            ["book/polymarket/TOKEN-A", "trades/polymarket/TOKEN-A"]
        );
        assert_same("per-holder recorder", &a.rec, &b.rec);
        assert_same("per-holder md frames", &a.frames, &b.frames);
        assert_eq!(a.md_session, b.md_session, "the md client's calls");
        assert_eq!(a.rec_session, b.rec_session, "the recorder client's calls");
        assert!(
            a.rec_session.iter().all(|l| !l.starts_with("unsubscribe")),
            "the md plane's release must not touch the recorder's client: {:?}",
            a.rec_session
        );
        assert!(
            a.md_session.iter().any(|l| l == "unsubscribe 1")
                && a.md_session.iter().any(|l| l == "unsubscribe 2"),
            "{:?}",
            a.md_session
        );
    }

    // ---- refusals and a venue that is down -----------------------------------------------------

    fn run_refusals(route: Route) -> (Vec<String>, Keyed) {
        let rig = Rig::new(route, &["binance"]);
        rig.data
            .refuse_all("BADSYM", Refusal::Unsupported("this scripted feed serves no such symbol"));
        rig.data.refuse_all("WORSE", Refusal::Subscribe("the scripted venue rejected it".into()));
        let mut rec = rig.rec_handle("binance");
        let mut obs = Vec::new();
        obs.push(format!("unsupported -> {:?}", rec.subscribe_trades("BADSYM")));
        obs.push(format!("subscribe -> {:?}", rec.subscribe_quotes("WORSE")));
        obs.push(format!("bars -> {:?}", rec.subscribe_bars("BTCUSDT", "1m")));
        obs.push(format!("real after refusals = {}", rig.broker.real_subscriptions()));
        rec.subscribe_trades("BTCUSDT").expect("a good key still subscribes");
        // The refused symbol's filter bit was withdrawn: its trade is not routed to the recorder, the
        // good key's is. (The recorder gets exactly ONE event.)
        let session = rig.data.session(0);
        session.emit(&SinkCall::Trade {
            venue: "binance".into(),
            symbol: "BADSYM".into(),
            trade: trade("BADSYM", 1, 0.1 + 0.2),
        });
        session.emit(&SinkCall::Trade {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trade: trade("BTCUSDT", 2, 0.1 + 0.2),
        });
        rig.settle_rec(1);
        (obs, by_symbol(&rig.rec.recorded()))
    }

    /// **A refusal survives the hop with its variant and its text**: `Unsupported(&'static str)` and
    /// `Subscribe(String)` read the same to the recorder on both paths, a refused key leaves no
    /// refcount behind, and its events are not routed.
    #[test]
    fn refusals_survive_the_hop_with_their_variant_and_text() {
        vike_log::test_init();
        let (a_obs, a_rec) = run_refusals(Route::InProcess);
        let (b_obs, b_rec) = run_refusals(Route::Hop);

        assert_eq!(
            a_obs[0],
            "unsupported -> Err(Unsupported(\"this scripted feed serves no such symbol\"))"
        );
        assert_eq!(a_obs[1], "subscribe -> Err(Subscribe(\"the scripted venue rejected it\"))");
        assert_eq!(
            a_obs[2],
            "bars -> Err(Unsupported(\"the feed broker carries tick lanes only\"))"
        );
        assert_eq!(a_obs[3], "real after refusals = 0");
        assert_eq!(b_obs, a_obs, "a refusal changed on the way through the hop");
        assert_eq!(a_rec.keys().collect::<Vec<_>>(), ["BTCUSDT"], "{a_rec:?}");
        assert_same("recorder after a refusal", &a_rec, &b_rec);
    }

    fn run_down_venue(route: Route) -> (String, usize) {
        let rig = Rig::new(route, &["binance", "bybit"]);
        let mut binance = rig.rec_handle("binance");
        binance.subscribe_trades("BTCUSDT").expect("binance is up");
        rig.data.set_up(false);
        let mut bybit = rig.rec_handle("bybit");
        let down = format!("{:?}", bybit.subscribe_trades("BTCUSDT"));
        rig.data.set_up(true);
        // The recorder retries on its next pass. In process the next pass builds the client; over the
        // hop the reader redials within its backoff.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match bybit.subscribe_trades("BTCUSDT") {
                Ok(_) => break,
                Err(e) => {
                    assert!(
                        matches!(e, LiveDataError::Subscribe(_)),
                        "{route:?}: a down venue must stay a transient Subscribe error: {e:?}"
                    );
                    assert!(Instant::now() < deadline, "{route:?}: the venue never came back: {e}");
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        (down, rig.broker.real_subscriptions())
    }

    /// **A venue that is down is a TRANSIENT `Subscribe` error on both paths, and the next pass
    /// succeeds.** The hop wraps the lane's words in the link's (a new failure mode, disclosed): the
    /// lane's own words survive inside them, the variant is the same, and nothing is `Unsupported`.
    #[test]
    fn a_down_venue_is_a_transient_subscribe_error_on_both_paths_and_recovers() {
        vike_log::test_init();
        let (a_down, a_real) = run_down_venue(Route::InProcess);
        let (b_down, b_real) = run_down_venue(Route::Hop);
        let lane_words = "the venue is down";
        assert!(a_down.starts_with("Err(Subscribe("), "{a_down}");
        assert!(a_down.contains(lane_words), "{a_down}");
        assert!(b_down.starts_with("Err(Subscribe("), "{b_down}");
        assert!(b_down.contains(lane_words), "the lane's words were lost on the hop: {b_down}");
        assert_eq!(a_real, 2);
        assert_eq!(b_real, a_real, "after the venue came back both clients hold their keys");
    }

    // ---- a seed emitted inside subscribe ---------------------------------------------------------

    fn run_seed_in_subscribe(route: Route) -> Vec<String> {
        let rig = Rig::new(route, &["binance"]);
        rig.data.set_default_hook(Arc::new(|sink: &dyn LiveDataSink, call: &SubscribeCall| {
            if call.verb == SubVerb::Trades {
                sink.stream_status(
                    "binance",
                    &call.symbol,
                    StreamLabel::Trades.as_str(),
                    StreamStatus::Live { gap_started_ts_ms: None },
                );
                sink.trade("binance", &call.symbol, trade(&call.symbol, 7, 0.1 + 0.2));
            }
        }));
        let mut rec = rig.rec_handle("binance");
        rec.subscribe_trades("BTCUSDT").expect("rec trades");
        // In process both events are already there; over the hop they follow the ack.
        rig.settle_rec(2);
        rig.rec.recorded().iter().map(exact_line).collect()
    }

    /// **What a venue emits INSIDE `subscribe_*` is not lost**: the recorder's filter bit is published
    /// before the real call (rule 2), so a status and a print emitted from within it reach the recorder
    /// in order on both paths.
    #[test]
    fn what_a_venue_emits_inside_subscribe_reaches_the_recorder_in_order() {
        vike_log::test_init();
        let a = run_seed_in_subscribe(Route::InProcess);
        let b = run_seed_in_subscribe(Route::Hop);
        assert_eq!(a.len(), 2);
        assert!(a[0].contains("StreamStatus") && a[1].contains("Trade"), "{a:?}");
        assert_eq!(b, a, "the hop reordered or altered what the venue emitted inside subscribe");
    }

    // ---- unconflated lanes: a burst, a conflating lane, a paced lane ------------------------------

    /// `n` rounds of a quote, a trade and a raw book delta for `symbol`, ts and seq strictly increasing.
    fn burst_script(venue: &str, symbol: &str, n: i64) -> Vec<SinkCall> {
        let mut out = Vec::new();
        for i in 0..n {
            let x = 0.1 * (i as f64 + 1.0) + 1e-9;
            out.push(SinkCall::Quote {
                venue: venue.into(),
                symbol: symbol.into(),
                quote: quote(symbol, i, x),
            });
            out.push(SinkCall::Trade {
                venue: venue.into(),
                symbol: symbol.into(),
                trade: trade(symbol, i, x),
            });
            out.push(SinkCall::BookUpdate {
                venue: venue.into(),
                symbol: symbol.into(),
                update: delta(symbol, i as u64 + 1, x),
            });
        }
        out
    }

    fn run_burst(route: Route, n: i64) -> (Keyed, Frames) {
        let rig = Rig::new(route, &["binance"]);
        let mut rec = rig.rec_handle("binance");
        for lane in [StreamLabel::Quotes, StreamLabel::Trades, StreamLabel::Book] {
            subscribe(&mut rec, "BTCUSDT", lane).expect("rec lane");
        }
        let guard = rig.hub.open_session().expect("an md session");
        rig.hub
            .acquire(guard.id(), &spec("binance", "BTCUSDT", MdLane::Trades))
            .expect("md trades");
        rig.hub.reconcile(NOW);
        rig.play_shared(&burst_script("binance", "BTCUSDT", n));
        rig.settle();
        rig.hub.publish_tick();
        (by_symbol(&rig.rec.recorded()), frames_of(guard.mailbox()))
    }

    /// **A burst inside the queue is delivered whole, in order, unconflated.** Every quote, trade and
    /// raw book delta of a tight loop reaches the recorder on both paths, and the md tape holds every
    /// print: the shared holder conflates nothing.
    #[test]
    fn a_burst_inside_the_queue_is_delivered_whole_and_in_order() {
        vike_log::test_init();
        const N: i64 = 400;
        let (a_rec, a_frames) = run_burst(Route::InProcess, N);
        let (b_rec, b_frames) = run_burst(Route::Hop, N);
        assert_eq!(a_rec["BTCUSDT"].len(), 3 * N as usize, "the recorder holds all three lanes");
        assert_same("burst, recorder", &a_rec, &b_rec);
        assert_eq!(a_frames.keys().collect::<Vec<_>>(), ["trades/binance/BTCUSDT"]);
        assert!(
            a_frames["trades/binance/BTCUSDT"][0].matches("\"is_buyer_maker\"").count()
                == N as usize
        );
        assert_same("burst, md tape", &a_frames, &b_frames);
    }

    /// `n` depth snapshots of `symbol`, each a different state.
    fn depth_script(venue: &str, symbol: &str, n: i64) -> Vec<SinkCall> {
        (0..n)
            .map(|i| {
                let x = 100.0 + i as f64 * 0.1 + 1e-9;
                SinkCall::L2Snapshot {
                    venue: venue.into(),
                    symbol: symbol.into(),
                    tick_size: 0.5,
                    bids: vec![BookLevel::new(x, 1.0 + i as f64), BookLevel::new(x - 0.5, 2.0)],
                    asks: vec![BookLevel::new(x + 0.5, 1.5)],
                    ts: 1_000 + i,
                }
            })
            .collect()
    }

    /// **A conflating lane may skip, but never the latest and never out of order.** `l2_snapshot` is
    /// latest-wins on the wire: a tight burst may be thinned under backlog, and what the recorder
    /// receives is a SUBSEQUENCE of the in-process path's, ending with the last state, bit-exact. The
    /// md hub's frame (also latest-wins) is therefore the same.
    #[test]
    fn a_conflating_lane_may_skip_but_never_the_latest_and_never_reorders() {
        vike_log::test_init();
        const N: i64 = 300;
        let run = |route: Route| {
            let rig = Rig::new(route, &["binance"]);
            let mut rec = rig.rec_handle("binance");
            rec.subscribe_depth("BTCUSDT").expect("rec depth");
            let guard = rig.hub.open_session().expect("an md session");
            rig.hub.acquire(guard.id(), &spec("binance", "BTCUSDT", MdLane::Depth)).expect("md");
            rig.hub.reconcile(NOW);
            let script = depth_script("binance", "BTCUSDT", N);
            // The md plane receives whatever the hop delivers; the LAST state must arrive. Wait for
            // the recorder to hold it (rec gets its events before md within an event).
            rig.data.session(0).emit_all(&script);
            let last = exact_line(&script[script.len() - 1]);
            let deadline = Instant::now() + Duration::from_secs(20);
            while rig.rec.recorded().last().map(exact_line).as_ref() != Some(&last) {
                assert!(Instant::now() < deadline, "{route:?}: the latest state never arrived");
                std::thread::sleep(Duration::from_millis(5));
            }
            // md processes an event right after the recorder, so wait for its count to settle.
            std::thread::sleep(Duration::from_millis(150));
            rig.hub.publish_tick();
            (by_symbol(&rig.rec.recorded()), frames_of(guard.mailbox()), script)
        };
        let (a_rec, a_frames, script) = run(Route::InProcess);
        let (b_rec, b_frames, _) = run(Route::Hop);

        assert_eq!(a_rec["BTCUSDT"].len(), N as usize, "in process nothing is conflated");
        let want = by_symbol(&script);
        assert_same("depth, in process against the script", &want, &a_rec);
        // The hop's delivery is a subsequence of the script, in order, and ends with the last state.
        let have = &b_rec["BTCUSDT"];
        let mut cursor = 0;
        for line in have {
            let at =
                want["BTCUSDT"][cursor..].iter().position(|w| w == line).unwrap_or_else(|| {
                    panic!("the hop delivered a state out of order or never emitted")
                });
            cursor += at + 1;
        }
        assert_eq!(have.last(), want["BTCUSDT"].last(), "the latest state must always arrive");
        assert!(!have.is_empty() && have.len() <= N as usize);
        assert_same("depth frame (latest wins)", &a_frames, &b_frames);
    }

    /// **A paced conflating lane loses nothing**: each snapshot is awaited before the next is
    /// emitted, so the queue never holds two, and all of them reach the recorder bit-exact on both
    /// paths. (The recorded depth series is written at the venue's own 100 ms cadence in production.)
    #[test]
    fn a_paced_conflating_lane_loses_nothing() {
        vike_log::test_init();
        const N: i64 = 40;
        let run = |route: Route| {
            let rig = Rig::new(route, &["binance"]);
            let mut rec = rig.rec_handle("binance");
            rec.subscribe_depth("BTCUSDT").expect("rec depth");
            let session = rig.data.session(0);
            for (i, call) in depth_script("binance", "BTCUSDT", N).iter().enumerate() {
                session.emit(call);
                rig.rec_due.fetch_add(1, Ordering::SeqCst);
                rig.wait_delivered();
                assert_eq!(rig.rec.recorded().len(), i + 1, "{route:?}: after event {i}");
            }
            by_symbol(&rig.rec.recorded())
        };
        let a = run(Route::InProcess);
        let b = run(Route::Hop);
        assert_eq!(a["BTCUSDT"].len(), N as usize);
        assert_same("paced depth", &a, &b);
    }

    // ---- the one thing JSON cannot carry ----------------------------------------------------------

    fn run_non_finite(route: Route) -> Vec<String> {
        let rig = Rig::new(route, &["binance"]);
        let mut rec = rig.rec_handle("binance");
        rec.subscribe_trades("BTCUSDT").expect("rec trades");
        let session = rig.data.session(0);
        let mut bad = trade("BTCUSDT", 2, 1.0);
        bad.price = f64::NAN;
        for print in [trade("BTCUSDT", 1, 0.1 + 0.2), bad, trade("BTCUSDT", 3, 1.0 / 3.0)] {
            session.emit(&SinkCall::Trade {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                trade: print,
            });
        }
        // In process: three prints. Over the hop: two prints and the disclosure between them.
        rig.settle_rec(if route == Route::InProcess { 3 } else { 4 });
        rig.rec
            .recorded()
            .iter()
            .map(|call| match call {
                SinkCall::Trade { trade: print, .. } => {
                    format!("trade {} {}", print.ts, print.price)
                }
                SinkCall::StreamStatus { stream, status, .. } => {
                    let word = match status {
                        StreamStatus::GapStart { .. } => "GapStart",
                        StreamStatus::Live { .. } => "Live",
                        _ => "other",
                    };
                    format!("{stream} {word}")
                }
                other => verb_of(other).to_string(),
            })
            .collect()
    }

    /// **A non-finite float does not cross, and its absence is disclosed.** JSON writes `NaN` as `null`,
    /// which the client could not decode (and would read as a lost link, then flap), so the daemon refuses
    /// such a row and a lossless lane owes a hole: the one place the hop is NOT bit-exact with the
    /// in-process path, by design. In process the print reaches the recorder as a `NaN`; over the hop the
    /// recorder gets the prints around it, in order, and a `GapStart` and `Live` for the lane between
    /// them. No venue parse in this workspace produces a `NaN` price today; the test pins what would happen.
    #[test]
    fn a_non_finite_float_is_not_carried_and_the_hole_is_disclosed() {
        vike_log::test_init();
        let a = run_non_finite(Route::InProcess);
        let b = run_non_finite(Route::Hop);
        assert_eq!(
            a,
            ["trade 1 0.30000000000000004", "trade 2 NaN", "trade 3 0.3333333333333333"],
            "in process the print is delivered as it is"
        );
        assert_eq!(
            b,
            [
                "trade 1 0.30000000000000004",
                "trades GapStart",
                "trades Live",
                "trade 3 0.3333333333333333"
            ],
            "over the hop: no NaN, a disclosed hole, the good prints in order"
        );
    }

    // ---- overflow and link loss: hop only --------------------------------------------------------
    //
    // A function call has no queue, so there is no in-process twin. These hold the new failure mode
    // itself, at the datahub's consumers: what the recorder and the md hub are told when the daemon
    // cannot keep up with a parked consumer.

    /// A trade whose own symbol field is 8 KiB, so a few hundred of them fill the socket buffers and
    /// then the daemon's queue.
    fn fat_trade(ts: i64) -> TradeTick {
        TradeTick {
            ts,
            local_ts: ts + 1,
            price: 0.1 + 0.2,
            size: 0.3,
            is_buyer_maker: ts % 2 == 0,
            symbol: format!("BTCUSDT{}", "x".repeat(8 * 1024)),
        }
    }

    /// Prints flooded at the daemon's sink while the consumer is parked.
    const FLOOD: i64 = 5_000;

    /// A hop rig whose recorder plane is parked behind a closed gate and whose daemon queues are tiny
    /// (the backpressure suite's numbers), with `lapse_budget` gaps allowed before a session is cut.
    fn parked_rig(lapse_budget: u64) -> (Rig, Arc<RecordingSink>, Arc<Gate>) {
        let mut config = ServerConfig::PRODUCTION;
        config.limits.queue_bytes = 128 * 1024;
        config.limits.queue_frames = 256;
        config.limits.lapse_budget = lapse_budget;
        let gate = Gate::closed();
        let rec_log = Arc::new(RecordingSink::default());
        let gated: Arc<dyn LiveDataSink> = Arc::new(GatedSink {
            inner: Arc::clone(&rec_log) as Arc<dyn LiveDataSink>,
            gate: Arc::clone(&gate),
        });
        (Rig::build(Route::Hop, &["binance"], config, Some(gated)), rec_log, gate)
    }

    /// Flood the daemon's sink with [`FLOOD`] fat prints of BTCUSDT. The venue side must never feel the
    /// consumer: no single call may take long, and the whole flood returns promptly.
    fn flood(rig: &Rig) {
        let session = rig.data.session(0);
        let started = Instant::now();
        let mut worst = Duration::ZERO;
        for ts in 0..FLOOD {
            let at = Instant::now();
            session.sink.trade("binance", "BTCUSDT", fat_trade(ts));
            worst = worst.max(at.elapsed());
        }
        assert!(worst < Duration::from_secs(2), "one sink call took {worst:?}: the pump blocked");
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the flood took {:?}",
            started.elapsed()
        );
    }

    /// Whether `call` is a `GapStart` (`wanted` = "GapStart") or a `Live` (`wanted` = "Live") for
    /// `symbol`'s `lane`.
    fn is_status(call: &SinkCall, symbol: &str, lane: StreamLabel, wanted: &str) -> bool {
        match call {
            SinkCall::StreamStatus { symbol: s, stream, status, .. } => {
                s == symbol
                    && stream == lane.as_str()
                    && match status {
                        StreamStatus::GapStart { .. } => wanted == "GapStart",
                        StreamStatus::Live { .. } => wanted == "Live",
                        _ => false,
                    }
            }
            _ => false,
        }
    }

    /// **An overflow is a disclosed hole at the datahub, never a silent one.** With the consumer
    /// parked and the daemon's queue tiny, a flood
    ///
    /// * never blocks the venue side's sink call;
    /// * reaches the recorder as prints in strictly increasing order, with a `GapStart` (and the `Live`
    ///   that closes it) for the trades lane between any two surviving prints that are not consecutive;
    /// * costs the datahub nothing else: no session is lost (the lapse budget is out of the way), the
    ///   broker never re-subscribes.
    #[test]
    fn an_overflow_is_a_disclosed_hole_between_surviving_prints() {
        vike_log::test_init();
        let (rig, rec_log, gate) = parked_rig(1_000_000);
        let mut rec = rig.rec_handle("binance");
        rec.subscribe_trades("BTCUSDT").expect("rec trades");
        // Declared after the rig and the handle, so it drops first: the gate opens before anything closes.
        let unpark = GateGuard(gate);
        assert_eq!(rig.broker.real_subscriptions(), 1);

        flood(&rig);
        unpark.0.open();
        // The consumer drains what the daemon kept; wait until it has been quiet for a moment.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut quiet_since = Instant::now();
        let mut seen = 0;
        loop {
            let now_seen = rec_log.recorded().len();
            if now_seen != seen {
                seen = now_seen;
                quiet_since = Instant::now();
            } else if seen > 0 && quiet_since.elapsed() > Duration::from_millis(500) {
                break;
            }
            assert!(Instant::now() < deadline, "the consumer never drained ({seen} calls)");
            std::thread::sleep(Duration::from_millis(20));
        }

        let calls = rec_log.recorded();
        // The stream starts at ts 0, so "the previous print" before the first is -1: a first survivor
        // that is not print 0 is a hole at the head (the producer can outrun the daemon's writer
        // before it has written anything), and it too must be preceded by its disclosure.
        let mut last_ts: i64 = -1;
        let (mut survivors, mut holes) = (0usize, 0usize);
        let mut gap_since_last = false;
        for call in &calls {
            match call {
                SinkCall::Trade { trade: print, .. } => {
                    assert!(print.ts > last_ts, "prints out of order: {last_ts} then {}", print.ts);
                    if print.ts != last_ts + 1 {
                        holes += 1;
                        assert!(
                            gap_since_last,
                            "a silent hole between ts {last_ts} and {}",
                            print.ts
                        );
                    }
                    last_ts = print.ts;
                    gap_since_last = false;
                    survivors += 1;
                }
                c if is_status(c, "BTCUSDT", StreamLabel::Trades, "GapStart") => {
                    gap_since_last = true;
                }
                _ => {}
            }
        }
        assert!(
            holes > 0 && survivors > 0 && survivors < FLOOD as usize,
            "the flood never overflowed: {survivors} survivors, {holes} holes"
        );
        let count = |wanted: &str| {
            calls.iter().filter(|c| is_status(c, "BTCUSDT", StreamLabel::Trades, wanted)).count()
        };
        assert!(count("GapStart") >= holes, "{} GapStarts for {holes} holes", count("GapStart"));
        assert_eq!(count("Live"), count("GapStart"), "every disclosed hole is closed by a Live");
        assert_eq!(
            rig.data.opened(),
            1,
            "the datahub kept its session: nothing was lost but frames"
        );
        assert_eq!(rig.broker.real_subscriptions(), 1);
    }

    /// **A session cut off for lagging marks EVERY key the datahub held, then replays and closes the
    /// gaps.** The same flood with `lapse_budget = 0` ends the session at the first disclosed hole:
    /// the datahub's client then marks `GapStart` on the OTHER held key too (ETHUSDT depth, which
    /// nobody flooded), replays both subscriptions on a fresh session under its own ids, and closes
    /// each gap with a `Live` naming it. The recorder sees the marks in that order, the md hub as its
    /// sticky status, and the broker is never asked to subscribe again.
    #[test]
    fn a_session_cut_off_for_lagging_marks_every_held_key_a_gap_then_replays() {
        vike_log::test_init();
        let (rig, rec_log, gate) = parked_rig(0);
        let mut rec = rig.rec_handle("binance");
        rec.subscribe_trades("BTCUSDT").expect("rec trades");
        rec.subscribe_depth("ETHUSDT").expect("rec depth");
        let guard = rig.hub.open_session().expect("an md session");
        rig.hub.acquire(guard.id(), &spec("binance", "ETHUSDT", MdLane::Depth)).expect("md depth");
        rig.hub.reconcile(NOW);
        assert_eq!(rig.broker.real_subscriptions(), 2);
        // Declared after the rig, the handle and the session, so it drops FIRST: the gate opens before
        // anything closes, or the parked reader thread would hold the clients' joins.
        let unpark = GateGuard(gate);

        flood(&rig);
        unpark.0.open();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !(rec_log
            .recorded()
            .iter()
            .any(|c| is_status(c, "ETHUSDT", StreamLabel::Depth, "Live"))
            && rig.data.opened() >= 2)
        {
            assert!(
                Instant::now() < deadline,
                "the held keys were never marked and replayed: {} session(s), {} recorder calls",
                rig.data.opened(),
                rec_log.recorded().len()
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        let calls = rec_log.recorded();
        // ETHUSDT depth was not flooded: it is marked by the session loss alone, GapStart then Live.
        let eth: Vec<&SinkCall> = calls.iter().filter(|c| symbol_of(c) == "ETHUSDT").collect();
        assert_eq!(eth.len(), 2, "{eth:?}");
        assert!(is_status(eth[0], "ETHUSDT", StreamLabel::Depth, "GapStart"), "{eth:?}");
        assert!(is_status(eth[1], "ETHUSDT", StreamLabel::Depth, "Live"), "{eth:?}");
        // ... and the Live names the gap its GapStart opened.
        match (eth[0], eth[1]) {
            (
                SinkCall::StreamStatus { status: StreamStatus::GapStart { at_ts_ms }, .. },
                SinkCall::StreamStatus { status: StreamStatus::Live { gap_started_ts_ms }, .. },
            ) => assert_eq!(*gap_started_ts_ms, Some(*at_ts_ms)),
            other => panic!("not a GapStart/Live pair: {other:?}"),
        }
        // The flooded key: the prints that arrived are in order, some were lost, the lane was marked.
        let prints: Vec<i64> = calls
            .iter()
            .filter_map(|c| match c {
                SinkCall::Trade { trade: print, .. } => Some(print.ts),
                _ => None,
            })
            .collect();
        assert!(prints.windows(2).all(|w| w[0] < w[1]), "prints out of order");
        // (No print need have survived: with the budget at zero the first disclosed hole ends the session,
        // and a producer that outruns the writer from the first call makes that hole the first thing written.)
        assert!(
            prints.len() < FLOOD as usize,
            "{} prints of {FLOOD}; {} recorder calls: {:?}",
            prints.len(),
            calls.len(),
            calls
                .iter()
                .map(|c| format!("{}:{}", verb_of(c), symbol_of(c)))
                .take(12)
                .collect::<Vec<_>>()
        );
        assert!(calls.iter().any(|c| is_status(c, "BTCUSDT", StreamLabel::Trades, "GapStart")));

        // The datahub did not re-subscribe: the client replayed under its own sids on a new session.
        assert_eq!(rig.broker.real_subscriptions(), 2);
        let replay = rig.data.session(1).calls();
        for (verb, symbol) in [(SubVerb::Trades, "BTCUSDT"), (SubVerb::Depth, "ETHUSDT")] {
            assert!(
                replay.contains(&DataCall::Subscribe(SubscribeCall {
                    verb,
                    symbol: symbol.into(),
                    interval: None
                })),
                "{symbol} was not replayed: {replay:?}"
            );
        }
        // The md plane sees the same ETHUSDT depth disclosure as sticky status, ending Live.
        std::thread::sleep(Duration::from_millis(100));
        rig.hub.publish_tick();
        let frames = frames_of(guard.mailbox());
        let status = &frames["status/Depth/binance/ETHUSDT"];
        assert!(status.last().is_some_and(|f| f.contains("Live")), "{status:?}");
    }

    // ---- the corpus itself --------------------------------------------------------------------

    /// The suite's floor (the `the_pin_has_a_non_empty_input` pattern): the canonical script plays all
    /// twelve verbs, so a comparison that "agrees" because nothing was played cannot hide here.
    #[test]
    fn the_corpus_plays_every_verb_for_both_symbols() {
        vike_log::test_init();
        let script = canonical_script("binance", &SYMBOLS);
        let verbs: std::collections::BTreeSet<&str> = script.iter().map(verb_of).collect();
        assert_eq!(verbs.len(), 12, "{verbs:?}");
        for symbol in SYMBOLS {
            assert_eq!(exact_lines_of(&script, symbol).len() * 2, script.len(), "{symbol}");
        }
    }
}

// ====================================================================================================
// Lane: the recorder's row writer (`RecorderSink` into `DataFusionHist`)
// ====================================================================================================

/// The recorder plane's REAL sink: the rows the broker routes to `Holder::Rec` land in a
/// `DataFusionHist` through `RecorderSink`, on each route, and the two stores are compared row for row
/// and commit key for commit key. Compiled with `datafusion-serve` (the `datahub-all` job).
#[cfg(feature = "datafusion-serve")]
mod recorded_rows {
    use super::*;

    use vike_data::{HistStore, TsRange};
    use vike_marketdata::BookUpdateKind;
    use vike_store::{DataFusionHist, RecorderConfig, RecorderSink};

    const TOKENS: [&str; 2] = ["TOKEN-A", "TOKEN-B"];

    /// What a store holds, in a form two stores can be compared by.
    #[derive(Debug, PartialEq)]
    struct StoreView {
        /// Every series, `{:?}` of its id, sorted.
        series: Vec<String>,
        /// Every series' commit keys, in manifest order, as [`shape`]s.
        commits: Keyed,
        /// Every flush counter of the store, sorted.
        flush_seqs: Vec<u64>,
        /// `kind/venue/symbol` to the exact lines of the rows scanned back.
        rows: Keyed,
    }

    /// A stream-health marker row carries the recorder's clock (`local_ts` always, and the `ts` of a
    /// `LiveResume`, which is filed under "now"); blank those so two runs a moment apart compare.
    fn normalized(mut update: BookUpdate) -> BookUpdate {
        if matches!(
            update.kind,
            BookUpdateKind::GapStart | BookUpdateKind::Stale | BookUpdateKind::LiveResume
        ) {
            update.local_ts = 0;
        }
        if update.kind == BookUpdateKind::LiveResume {
            update.ts = 0;
        }
        update
    }

    /// A commit key `live-{venue}-{symbol}-{kind}-{first_ts}-{last_ts}-{flush_seq}` as
    /// `(shape, flush_seq)`. The shape masks any timestamp later than the scripted clock (a
    /// `LiveResume` is filed under "now", and a rollover puts it in a buffer of its own). The writer's
    /// flush counter is set apart: the final flush hands it out walking its series in a hash order, so
    /// what two stores can share is the SET of counters and that no two are alike.
    fn shape(key: &str) -> (String, u64) {
        let mut tail = key.rsplitn(3, '-');
        let seq = tail.next().and_then(|s| s.parse().ok()).expect("a flush counter");
        let last = tail.next().expect("last_ts");
        let (head, first) = tail.next().and_then(|r| r.rsplit_once('-')).expect("first_ts");
        let mask = |ts: &str| {
            if ts.parse::<i64>().is_ok_and(|t| t > GAP_AT) {
                "<now>".to_string()
            } else {
                ts.to_string()
            }
        };
        (format!("{head}-{}-{}", mask(first), mask(last)), seq)
    }

    /// Rows as the exact (`{:?}`, so bit-exact for an f64) lines two stores are compared by.
    fn lines<T: std::fmt::Debug>(rows: Vec<T>) -> Vec<String> {
        rows.iter().map(|r| format!("{r:?}")).collect()
    }

    fn view(store: &DataFusionHist) -> StoreView {
        let mut ids = store.list_series().expect("list_series");
        ids.sort();
        let mut commits = Keyed::new();
        let mut flush_seqs = Vec::new();
        let mut series = Vec::new();
        for id in &ids {
            let label = format!("{id:?}");
            let keys = store.series_commits(id).expect("series_commits");
            let shapes: Vec<(String, u64)> = keys.iter().map(|k| shape(k)).collect();
            flush_seqs.extend(shapes.iter().map(|(_, seq)| *seq));
            commits.insert(label.clone(), shapes.into_iter().map(|(shape, _)| shape).collect());
            series.push(label);
        }
        flush_seqs.sort_unstable();
        let mut rows = Keyed::new();
        for symbol in TOKENS {
            let all = TsRange::all;
            rows.insert(
                format!("quote/polymarket/{symbol}"),
                lines(store.scan_quotes("polymarket", symbol, all()).expect("scan_quotes")),
            );
            rows.insert(
                format!("trade/polymarket/{symbol}"),
                lines(store.scan_trades("polymarket", symbol, all()).expect("scan_trades")),
            );
            let books = store.scan_book_updates("polymarket", symbol, all()).expect("books");
            rows.insert(
                format!("book/polymarket/{symbol}"),
                lines(books.into_iter().map(normalized).collect::<Vec<_>>()),
            );
            let depth = store.scan_depth("polymarket", symbol, all()).expect("depth");
            rows.insert(
                format!("depth/polymarket/{symbol}"),
                lines(depth.into_iter().map(normalized).collect::<Vec<_>>()),
            );
        }
        StoreView { series, commits, flush_seqs, rows }
    }

    fn run(route: Route) -> StoreView {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = Arc::new(DataFusionHist::open(dir.path()).expect("a store"));
        let (sink, handle) = RecorderSink::spawn(Arc::clone(&store), RecorderConfig::default())
            .expect("the recorder's writer thread");
        let rig = Rig::build(
            route,
            &["polymarket"],
            ServerConfig::PRODUCTION,
            Some(sink as Arc<dyn LiveDataSink>),
        );
        let mut rec = rig.rec_handle("polymarket");
        for token in TOKENS {
            for lane in
                [StreamLabel::Quotes, StreamLabel::Trades, StreamLabel::Book, StreamLabel::Depth]
            {
                subscribe(&mut rec, token, lane)
                    .unwrap_or_else(|e| panic!("{route:?}: {token} {lane:?}: {e}"));
            }
        }
        // The venue's behaviour: every verb for both tokens, then the book and depth lanes of TOKEN-A
        // going down and coming back.
        let mut script = canonical_script("polymarket", &TOKENS);
        let (down, up) = gap_scripts(
            "polymarket",
            &[("TOKEN-A", StreamLabel::Book), ("TOKEN-A", StreamLabel::Depth)],
        );
        script.extend(down);
        script.extend(up);
        rig.play_rec_session(0, &script);
        rig.settle();
        // The writer flushes every buffered row on shutdown and the store is then the whole answer.
        handle.shutdown();
        view(&store)
    }

    /// **The recorder's rows are the same rows.** The same venue behaviour, through the broker and the
    /// recorder's own writer into a real store, leaves the same series, the same rows (every float bit
    /// for bit) and the same commit keys on both routes. The only masked fields are the two the
    /// recorder stamps with its own clock for a stream-health marker.
    #[test]
    fn the_recorders_rows_and_commit_keys_are_the_same_over_the_hop() {
        vike_log::test_init();
        let a = run(Route::InProcess);
        let b = run(Route::Hop);

        // The floors: 8 series (quote, trade, book, depth for two tokens), and the rows the script makes.
        assert_eq!(a.series.len(), 8, "{:?}", a.series);
        for token in TOKENS {
            assert_eq!(a.rows[&format!("quote/polymarket/{token}")].len(), 3, "{token}");
            assert_eq!(a.rows[&format!("trade/polymarket/{token}")].len(), 3, "{token}");
        }
        // TOKEN-A's book: three deltas, a GapStart and a LiveResume; TOKEN-B's: its three deltas.
        assert_eq!(a.rows["book/polymarket/TOKEN-A"].len(), 5);
        assert_eq!(a.rows["book/polymarket/TOKEN-B"].len(), 3);
        assert_eq!(a.rows["depth/polymarket/TOKEN-A"].len(), 3);
        assert_eq!(a.rows["depth/polymarket/TOKEN-B"].len(), 1);
        assert!(a.commits.values().all(|keys| !keys.is_empty()), "{:?}", a.commits);

        assert_eq!(b.series, a.series, "the hop recorded a different set of series");
        assert_same("stored rows", &a.rows, &b.rows);
        assert_same("commit keys", &a.commits, &b.commits);
        assert_eq!(b.flush_seqs, a.flush_seqs, "the writers numbered their flushes differently");
        let distinct: std::collections::BTreeSet<_> = a.flush_seqs.iter().collect();
        assert_eq!(
            distinct.len(),
            a.flush_seqs.len(),
            "a flush counter was reused: {:?}",
            a.flush_seqs
        );
    }
}

// ====================================================================================================
// Lane: the recording feeds (resolvers, and the recorder runtime over the broker)
// ====================================================================================================

/// `build_recording_feed`'s two rows and the recorder runtime that ticks them: the resolver (a
/// rolling family, a catalog glob) answers, the runtime reconciles a client to the answer, and the
/// venue client sees the subscribes and unsubscribes. Path A is the resolver double in process (what the
/// bridge's `RollingFamily` and `BinanceCatalog` were); path B is the production row over the hop.
#[cfg(any(feature = "record-polymarket", feature = "record-binance"))]
mod recording_feeds {
    use super::*;

    use vike_recorder::assembled::AssembledFeed;
    use vike_recorder::config::Subscription;
    use vike_recorder::membership::Membership;
    use vike_recorder::runtime::{FeedTick, RecorderRuntime, VenueFeed};
    use vike_recorder::session::Stream;

    /// Wait until the venue side's log has `n` entries (the hop delivers a subscribe or an
    /// unsubscribe after the call returned), then return it.
    fn log_after(rig: &Rig, n: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while rig.data.log().len() < n {
            assert!(
                Instant::now() < deadline,
                "{:?}: the venue client saw {} of {n} calls",
                rig.route,
                rig.data.log().len()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(100));
        rig.log_lines()
    }

    /// The calls a run of ticks must have made at the venue client: its open, every start and every stop.
    fn expected_calls(ticks: &[FeedTick]) -> usize {
        let moves: usize = ticks
            .iter()
            .map(|t| match t {
                FeedTick::Reconciled { report, .. } => report.started.len() + report.stopped.len(),
                FeedTick::ResolveFailed { .. } => 0,
            })
            .sum();
        // The client is built on the first subscribe, so a run that never subscribed never opened one.
        if moves == 0 { 0 } else { 1 + moves }
    }

    // ---- polymarket: a rotating family ---------------------------------------------------------

    #[cfg(feature = "record-polymarket")]
    mod polymarket {
        use super::*;

        use std::collections::BTreeSet;

        use vike_data::live::SymbolResolver;

        const FAMILY: &str = "btc-updown-5m";
        /// 2026-06-02 08:00 UTC, a 5-minute boundary.
        const BASE: i64 = 1_780_387_200_000;
        const WINDOW_MS: i64 = 300_000;

        /// The two outcome tokens of the window `now_ms` falls in; the directory is down for one window.
        fn tokens_at(now_ms: i64) -> Result<Vec<String>, String> {
            let window = now_ms / WINDOW_MS;
            if window == (BASE + 2 * WINDOW_MS) / WINDOW_MS {
                return Err("gamma directory unreachable (scripted)".to_string());
            }
            Ok(vec![format!("TOK-{window}-UP"), format!("TOK-{window}-DOWN")])
        }

        /// Path A's resolver: what `RollingFamily` was, answering from the same function.
        struct InProcessFamily;

        impl SymbolResolver for InProcessFamily {
            fn group(&self) -> Option<&str> {
                Some(FAMILY)
            }
            fn desired(&mut self, now_ms: i64) -> Result<BTreeSet<String>, String> {
                tokens_at(now_ms).map(|tokens| tokens.into_iter().collect())
            }
        }

        struct Run {
            ticks: Vec<FeedTick>,
            log: Vec<String>,
            family: Option<String>,
            expected_series: Vec<String>,
        }

        fn run(route: Route) -> Run {
            let rest = ScriptedRest::new().with_family_by("polymarket", FAMILY, tokens_at);
            let rig =
                Rig::assemble(route, &["polymarket"], ServerConfig::PRODUCTION, None, rest.clone());
            let feed: AssembledFeed = match route {
                Route::InProcess => AssembledFeed::new(
                    "polymarket",
                    true,
                    Box::new(InProcessFamily),
                    Box::new(rig.rec_handle("polymarket")),
                ),
                Route::Hop => {
                    let sub = Subscription {
                        venue: "polymarket".into(),
                        family: Some(FAMILY.into()),
                        symbols: Vec::new(),
                    };
                    vike_datahub::recording::build_recording_feed(&sub, &rig.broker, &rig.link())
                        .unwrap_or_else(|e| panic!("the production row must build: {e}"))
                }
            };
            let family = feed.family().map(str::to_string);
            let mut rt = RecorderRuntime::new(
                Membership::new(),
                vec![Stream::Quotes, Stream::Trades, Stream::Book],
            );
            rt.add_feed(Box::new(feed));
            // Two ticks in one window, the rotation, a window the directory is down for (the
            // subscriptions must be left exactly as they are), and the next window.
            let mut ticks = Vec::new();
            for offset in [0, 30_000, WINDOW_MS, WINDOW_MS + 30_000, 2 * WINDOW_MS, 3 * WINDOW_MS] {
                ticks.extend(rt.tick(BASE + offset));
            }
            let expected_series = rt.expected_series();
            let log = log_after(&rig, expected_calls(&ticks));
            Run { ticks, log, family, expected_series }
        }

        /// **The rolling family rotates the same subscriptions on both routes.** The recorder runtime,
        /// ticked across a rotation and a directory outage, subscribes the new window's tokens (Quotes
        /// dropped beside Book, as polymarket narrows it), unsubscribes the old, leaves everything
        /// alone while the directory is down, and the venue client sees the same calls in the same
        /// order. The family's own failure text arrives unchanged.
        #[test]
        fn a_rotating_family_drives_the_same_subscriptions_over_the_hop() {
            vike_log::test_init();
            let a = run(Route::InProcess);
            let b = run(Route::Hop);

            // The floors, read off the in-process run: 2 tokens x (trades, book) at the first tick, a
            // full swap at the rotation, a failure that keeps the 4 live subscriptions, a swap again.
            let ticks = &a.ticks;
            assert_eq!(ticks.len(), 6);
            assert!(matches!(&ticks[0], FeedTick::Reconciled { live: 4, .. }), "{:?}", ticks[0]);
            assert!(
                matches!(&ticks[2], FeedTick::Reconciled { live: 4, report, .. }
                if report.started.len() == 4 && report.stopped.len() == 4),
                "{:?}",
                ticks[2]
            );
            assert!(
                matches!(&ticks[4], FeedTick::ResolveFailed { error, live: 4, .. }
                    if error == "gamma directory unreachable (scripted)"),
                "{:?}",
                ticks[4]
            );
            assert_eq!(a.family.as_deref(), Some(FAMILY));

            assert_eq!(b.family, a.family, "the group a series is filed under");
            assert_eq!(b.ticks, a.ticks, "the runtime's tick reports differ");
            assert_eq!(b.expected_series, a.expected_series);
            assert_eq!(b.log, a.log, "the venue client was asked different things");
        }

        /// A malformed family is refused when the feed is BUILT, offline, with the grammar check's own
        /// words (the production row), whatever the daemon is doing.
        #[test]
        fn a_family_without_an_interval_is_refused_at_build_with_the_same_words() {
            vike_log::test_init();
            let rig = Rig::new(Route::Hop, &["polymarket"]);
            let sub = Subscription {
                venue: "polymarket".into(),
                family: Some("btc-updown".into()),
                symbols: Vec::new(),
            };
            let refused =
                match vike_datahub::recording::build_recording_feed(&sub, &rig.broker, &rig.link())
                {
                    Ok(_) => panic!("a family with no interval must be refused"),
                    Err(e) => e,
                };
            assert_eq!(refused, vike_catalog::polymarket_family_minutes("btc-updown").unwrap_err());
        }
    }

    // ---- binance: a glob over the venue's listing -------------------------------------------------

    #[cfg(feature = "record-binance")]
    mod binance {
        use super::*;

        use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
        use vike_gatewayhub::testkit::rest::Answer as RestAnswer;
        use vike_gatewayhub_client::proto::WireCatalogMode;
        use vike_model::{AssetClass, Contract, Instrument};
        use vike_recorder::resolve::CatalogGlob;

        const NOW: i64 = 1_780_387_200_000;
        const GLOB: &str = "*USDT";

        fn listing() -> Vec<Instrument> {
            ["BTCUSDT", "ETHUSDT", "BTCEUR", "SOLUSDT", "SOLEUR"]
                .iter()
                .map(|raw| Instrument::new("binance", *raw, "BASE", "QUOTE", Contract::CryptoSpot))
                .collect()
        }

        /// Path A's provider: what `BinanceCatalog` was, in process, counting how often it is asked.
        struct InProcessCatalog {
            outcome: Result<Vec<Instrument>, String>,
            asked: Arc<AtomicUsize>,
        }

        impl CatalogProvider for InProcessCatalog {
            fn venue(&self) -> &str {
                "binance"
            }
            fn asset_classes(&self) -> &[AssetClass] {
                &[AssetClass::CryptoSpot]
            }
            fn mode(&self) -> CatalogMode {
                CatalogMode::Enumerable
            }
            fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
                self.asked.fetch_add(1, Ordering::SeqCst);
                self.outcome.clone().map_err(CatalogError)
            }
        }

        struct Run {
            ticks: Vec<FeedTick>,
            family: Option<String>,
            log: Vec<String>,
            listings_asked: usize,
        }

        fn run(route: Route, outcome: Result<Vec<Instrument>, String>) -> Run {
            let mut rest = ScriptedRest::new().with_catalog(
                "binance",
                vec![AssetClass::CryptoSpot],
                WireCatalogMode::Enumerable,
                outcome.clone().unwrap_or_default(),
            );
            if let Err(text) = &outcome {
                rest = rest.with_catalog_list("binance", RestAnswer::fetch_error(text.clone()));
            }
            let rig =
                Rig::assemble(route, &["binance"], ServerConfig::PRODUCTION, None, rest.clone());
            let asked = Arc::new(AtomicUsize::new(0));
            let feed: AssembledFeed = match route {
                Route::InProcess => AssembledFeed::new(
                    "binance",
                    false,
                    Box::new(CatalogGlob::new(
                        GLOB,
                        Box::new(InProcessCatalog { outcome, asked: Arc::clone(&asked) }),
                    )),
                    Box::new(rig.rec_handle("binance")),
                ),
                Route::Hop => {
                    let sub = Subscription {
                        venue: "binance".into(),
                        family: Some(GLOB.into()),
                        symbols: Vec::new(),
                    };
                    vike_datahub::recording::build_recording_feed(&sub, &rig.broker, &rig.link())
                        .unwrap_or_else(|e| panic!("the production row must build: {e}"))
                }
            };
            let family = feed.family().map(str::to_string);
            let mut rt =
                RecorderRuntime::new(Membership::new(), vec![Stream::Trades, Stream::Depth]);
            rt.add_feed(Box::new(feed));
            let mut ticks = Vec::new();
            for offset in [0, 30_000, 60_000] {
                ticks.extend(rt.tick(NOW + offset));
            }
            let log = log_after(&rig, expected_calls(&ticks));
            let listings_asked = match route {
                Route::InProcess => asked.load(Ordering::SeqCst),
                Route::Hop => rest
                    .fetches()
                    .iter()
                    .filter(|f| {
                        matches!(
                            f.what,
                            vike_gatewayhub_client::proto::FetchWhat::CatalogList { .. }
                        )
                    })
                    .count(),
            };
            Run { ticks, family, log, listings_asked }
        }

        /// **A glob over the venue's listing resolves the same symbols on both routes**, files them
        /// under the same path-safe group, asks the listing once (the glob caches it), and the venue
        /// client sees the same subscribes in the same order.
        #[test]
        fn a_glob_resolves_the_same_symbols_from_the_gatewayhubs_listing() {
            vike_log::test_init();
            let a = run(Route::InProcess, Ok(listing()));
            let b = run(Route::Hop, Ok(listing()));

            // The floors: 3 USDT symbols x (trades, depth) started at the first tick, nothing after.
            assert!(
                matches!(&a.ticks[0], FeedTick::Reconciled { symbols: 3, report, live: 6, .. }
                    if report.started.len() == 6),
                "{:?}",
                a.ticks[0]
            );
            assert!(
                a.ticks[1..]
                    .iter()
                    .all(|t| matches!(t, FeedTick::Reconciled { report, .. } if report.is_quiet()))
            );
            assert_eq!(
                a.family.as_deref(),
                Some("USDT"),
                "the glob is kept, its group is path-safe"
            );
            assert_eq!(a.listings_asked, 1, "the glob caches its listing");

            assert_eq!(b.family, a.family);
            assert_eq!(b.listings_asked, 1, "the hop asked for the listing more than once");
            assert_eq!(b.ticks, a.ticks);
            assert_eq!(b.log, a.log);
        }

        /// **A listing that fails is `Err`, never an empty set**: the same words on both routes, the
        /// subscriptions untouched, and asked again on the next tick.
        #[test]
        fn a_failing_listing_is_an_error_never_an_empty_set_on_both_routes() {
            vike_log::test_init();
            let failure = Err("HTTP 503 from the exchangeInfo endpoint".to_string());
            let a = run(Route::InProcess, failure.clone());
            let b = run(Route::Hop, failure);
            for tick in &a.ticks {
                assert!(
                    matches!(tick, FeedTick::ResolveFailed { error, live: 0, .. }
                        if error == "binance instrument list: catalog error: HTTP 503 from the exchangeInfo endpoint"),
                    "{tick:?}"
                );
            }
            assert_eq!(a.listings_asked, 3, "a failed listing is asked again every tick");
            assert_eq!(
                b.ticks, a.ticks,
                "the failure text or the retry cadence changed on the hop"
            );
            assert_eq!(b.listings_asked, a.listings_asked);
        }
    }
}

// ====================================================================================================
// Lane: the venue catalog (the verb, the table, the memo, the cap)
// ====================================================================================================

/// A datahub server with the catalog lane armed answers `DatahubClient::venue_catalog` over both
/// tables: the in-process closures (the pre-change `real_catalog_table`) and `real_catalog_table` over a
/// gatewayhub. Every answer a client can receive is compared.
#[cfg(feature = "catalog-serve")]
mod catalog_lane {
    use super::*;

    use std::net::{SocketAddr, TcpListener};

    use vike_catalog::CatalogError;
    use vike_data::{HistStore, MemHistStore};
    use vike_datahub::catalog::{
        CATALOG_TTL, CatalogFn, CatalogLane, CatalogTable, real_catalog_table,
    };
    use vike_datahub::serve_authed;
    use vike_datahub_client::DatahubClient;
    use vike_datahub_client::catalog::{CATALOG_MAX_INSTRUMENTS, CatalogOutcome};
    use vike_gatewayhub::testkit::rest::{Answer as RestAnswer, RestCall};
    use vike_gatewayhub_client::catalog::CATALOG_VENUES;
    use vike_gatewayhub_client::proto::{FetchWhat, WireCatalogMode};
    use vike_model::{AssetClass, Contract, Instrument};

    const FAILURE: &str = "HTTP 503 from the exchangeInfo endpoint";

    /// `n` instruments whose properties are floats with awkward bit patterns.
    fn listing(venue: &str, n: usize) -> Vec<Instrument> {
        (0..n)
            .map(|i| {
                let mut instrument = Instrument::new(
                    venue,
                    format!("SYM{i}USDT"),
                    format!("SYM{i}"),
                    "USDT",
                    Contract::CryptoSpot,
                );
                instrument.properties.tick_size = 0.1 + 0.2 + i as f64 * 1e-9;
                instrument.properties.step_size = 1.0 / 3.0;
                instrument.properties.min_qty = f64::MIN_POSITIVE;
                instrument.properties.min_notional = -0.0;
                instrument
            })
            .collect()
    }

    /// What the scripted venues list: binance 40, polymarket past the cap, hyperliquid nothing, okx fails.
    fn universe(venue: &str) -> Option<Result<Vec<Instrument>, String>> {
        match venue {
            "binance" => Some(Ok(listing("binance", 40))),
            "polymarket" => Some(Ok(listing("polymarket", CATALOG_MAX_INSTRUMENTS + 7))),
            "hyperliquid" => Some(Ok(Vec::new())),
            "okx" => Some(Err(FAILURE.to_string())),
            _ => None,
        }
    }

    /// Path A's table: the eight venues in the real order, each an in-process closure that answers
    /// what the pre-change provider seam did (`list_via`: the cap, `CatalogError`'s own `Display`).
    fn in_process_table(asked: Arc<Mutex<Vec<String>>>) -> CatalogTable {
        CatalogTable::new(
            CATALOG_VENUES
                .into_iter()
                .map(|venue| {
                    let asked = Arc::clone(&asked);
                    let entry: CatalogFn = Box::new(move || {
                        lock(&asked).push(venue.to_string());
                        match universe(venue) {
                            Some(Ok(mut list)) => {
                                list.truncate(CATALOG_MAX_INSTRUMENTS);
                                Ok(list)
                            }
                            Some(Err(text)) => Err(CatalogError(text).to_string()),
                            None => {
                                Err(format!("catalog error: no listing is scripted for {venue}"))
                            }
                        }
                    });
                    (venue.to_string(), entry)
                })
                .collect(),
        )
    }

    fn scripted_rest() -> ScriptedRest {
        let mut rest = ScriptedRest::new();
        for venue in ["binance", "polymarket", "hyperliquid", "okx"] {
            let list = universe(venue).expect("scripted");
            rest = rest.with_catalog(
                venue,
                vec![AssetClass::CryptoSpot],
                WireCatalogMode::Enumerable,
                list.clone().unwrap_or_default(),
            );
            if let Err(text) = list {
                rest = rest.with_catalog_list(venue, RestAnswer::fetch_error(text));
            }
        }
        rest
    }

    /// A datahub server over `lane`, on an ephemeral loopback port, for the process's life.
    fn serve(lane: CatalogLane) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("address");
        let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
        let lane = Arc::new(lane);
        std::thread::Builder::new()
            .name("twin-datahub".into())
            .spawn(move || {
                let _ = serve_authed(listener, store, None, None, None, None, Some(lane));
            })
            .expect("spawn the datahub server");
        addr
    }

    /// One answer: a one-line summary, and the instruments as `{:?}` (the bit-exact form).
    struct Reply {
        summary: String,
        body: String,
    }

    fn ask(client: &mut DatahubClient, venue: &str) -> Reply {
        match client.venue_catalog(venue) {
            Ok(listing) => match listing.outcome {
                CatalogOutcome::Listed { instruments, truncated, cached } => Reply {
                    summary: format!(
                        "{venue}: Listed {} truncated={truncated} cached={cached}",
                        instruments.len()
                    ),
                    body: format!("{instruments:?}"),
                },
                other => Reply { summary: format!("{venue}: {other:?}"), body: String::new() },
            },
            Err(e) => Reply { summary: format!("{venue}: Err({e})"), body: String::new() },
        }
    }

    const ASKS: [&str; 8] =
        ["binance", "binance", "okx", "okx", "polymarket", "hyperliquid", "alpaca", "kraken"];

    fn run(route: Route) -> (Vec<Reply>, Vec<String>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        match route {
            Route::InProcess => {
                let addr =
                    serve(CatalogLane::new(in_process_table(Arc::clone(&asked)), CATALOG_TTL));
                let mut client = DatahubClient::connect(addr).expect("connect");
                let replies = ASKS.iter().map(|v| ask(&mut client, v)).collect();
                let asked = lock(&asked).clone();
                (replies, asked)
            }
            Route::Hop => {
                let rest = scripted_rest();
                // Declared first, so it drops last.
                let gateway = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());
                let table = real_catalog_table(&gateway.link(), false);
                let addr = serve(CatalogLane::new(table, CATALOG_TTL));
                let mut client = DatahubClient::connect(addr).expect("connect");
                let replies = ASKS.iter().map(|v| ask(&mut client, v)).collect();
                let listed: Vec<String> = rest
                    .calls()
                    .into_iter()
                    .filter_map(|c| match c {
                        RestCall::Fetch(f) => match f.what {
                            FetchWhat::CatalogList { venue } => Some(venue),
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect();
                (replies, listed)
            }
        }
    }

    /// The index of the first byte two strings differ at, for a failure message that is not 10 MB.
    fn first_difference(a: &str, b: &str) -> Option<usize> {
        a.bytes()
            .zip(b.bytes())
            .position(|(x, y)| x != y)
            .or_else(|| (a.len() != b.len()).then_some(a.len().min(b.len())))
    }

    /// **Every answer the catalog verb gives is the same over the hop**: a listing bit for bit (the
    /// instrument properties are floats with awkward bits), the cap at `CATALOG_MAX_INSTRUMENTS` with
    /// the truncation said, a memo hit reported as cached, a venue's failure with the same words,
    /// an empty universe as an empty listing, a credentialed venue and a stranger refused with the
    /// same `supported` list, and the venue fetched the same number of times in the same order.
    #[test]
    fn every_answer_the_catalog_verb_gives_is_the_same_over_the_hop() {
        vike_log::test_init();
        let (a, a_fetched) = run(Route::InProcess);
        let (b, b_fetched) = run(Route::Hop);

        // The floors, read off the in-process run.
        assert_eq!(a[0].summary, "binance: Listed 40 truncated=false cached=false");
        assert_eq!(
            a[1].summary, "binance: Listed 40 truncated=false cached=true",
            "the memo answers"
        );
        assert!(a[2].summary.contains(FAILURE), "{}", a[2].summary);
        assert!(a[3].summary.contains(FAILURE), "a failure is not memoized: {}", a[3].summary);
        assert_eq!(
            a[4].summary,
            format!("polymarket: Listed {CATALOG_MAX_INSTRUMENTS} truncated=true cached=false")
        );
        assert_eq!(a[5].summary, "hyperliquid: Listed 0 truncated=false cached=false");
        assert!(
            a[6].summary.contains("Refused(") && a[7].summary.contains("Refused("),
            "{} / {}",
            a[6].summary,
            a[7].summary
        );
        assert!(
            a[0].body.contains(&format!("tick_size: {:?}", 0.1 + 0.2)),
            "the fixture's awkward float is in the body"
        );
        assert_eq!(a_fetched, ["binance", "okx", "okx", "polymarket", "hyperliquid"]);

        assert_eq!(a.len(), b.len());
        for (i, (want, have)) in a.iter().zip(&b).enumerate() {
            assert_eq!(have.summary, want.summary, "answer {i}");
            if let Some(at) = first_difference(&want.body, &have.body) {
                panic!("answer {i} ({}): the instruments differ from byte {at}", want.summary);
            }
        }
        assert_eq!(b_fetched, a_fetched, "the venue was fetched differently");
    }
}

// ====================================================================================================
// Lane: the options chain pass, the strike window, the recorder hop and the feed rows
// ====================================================================================================

/// The Deribit options lane: its chain pass (`fetch_options`, the full chain asked for, recorded, then
/// windowed) over a gatewayhub, against the same arithmetic done in process on the same chains, and the
/// two feed streams' rows against the rows pushed straight into the callbacks.
#[cfg(feature = "options-serve")]
mod options_lane {
    use super::*;

    use vike_data::{HistStore, MemHistStore, TsRange};
    use vike_datahub::options::deribit::{
        STRIKE_WINDOW, chain_recorder, chain_snapshot_rows, fetch_options, record_chain,
    };
    use vike_gatewayhub_client::options::{RemoteOptions, RemoteOptionsFeed};
    use vike_gatewayhub_client::proto::{OptionsCall, OptionsFeedSpec};
    use vike_marketdata::{
        Expiry, OptionChain, OptionKind, OptionQuote, OptionTicker, StrikeRow, UnderlyingKind,
    };
    use vike_options::limit_strikes;

    const ASOF: i64 = 1_780_387_200_000;

    fn expiry(date: &str, dte: i64) -> Expiry {
        Expiry { date: date.into(), dte, label: date.into() }
    }

    /// A quote whose every float is an awkward bit pattern, distinct per `i`.
    fn quote_at(name: &str, strike: f64, kind: OptionKind, i: f64) -> OptionQuote {
        OptionQuote {
            bid: Some(0.1 + 0.2 + i * 1e-7),
            ask: Some(1.0 / 3.0 + i * 1e-7),
            mark: Some(-0.0),
            iv: Some(f64::MIN_POSITIVE + i),
            open_interest: Some(120.0 + i / 7.0),
            volume: Some(8.0),
            delta: Some(-0.0),
            gamma: Some(1e-300),
            theta: Some(-30.1),
            vega: Some(55.5 + i / 3.0),
            instrument_name: Some(name.to_string()),
            ..OptionQuote::new(strike, kind)
        }
    }

    /// BTC `date` over `strikes` (a call and a put each), spot in the middle.
    fn chain(date: &str, tag: &str, dte: i64, strikes: &[f64]) -> OptionChain {
        OptionChain {
            underlying: "BTC".to_string(),
            underlying_kind: UnderlyingKind::Crypto,
            underlying_price: Some(1.0 / 3.0 + strikes[strikes.len() / 2]),
            expiry: expiry(date, dte),
            asof_ms: ASOF,
            source: "deribit".to_string(),
            rows: strikes
                .iter()
                .enumerate()
                .map(|(i, &k)| StrikeRow {
                    strike: k,
                    call: Some(quote_at(
                        &format!("BTC-{tag}-{k}-C"),
                        k,
                        OptionKind::Call,
                        i as f64,
                    )),
                    put: Some(quote_at(&format!("BTC-{tag}-{k}-P"), k, OptionKind::Put, i as f64)),
                })
                .collect(),
        }
    }

    /// The front expiry: 100 strikes (the ±30 window keeps 60); the back one: 3.
    fn fixtures() -> Vec<OptionChain> {
        let wide: Vec<f64> = (0..100).map(|i| 80_000.0 + 1_000.0 * f64::from(i)).collect();
        vec![
            chain("2037-06-25", "25JUN37", 3_900, &wide),
            chain("2037-12-24", "24DEC37", 4_100, &[120_000.0, 125_000.0, 130_000.0]),
        ]
    }

    /// **The chain pass serves and records the same rows.** Path A is the pre-change arithmetic done
    /// in process on the same chains (the window trims what is served, the recorder gets the FULL
    /// chain before it); path B is the real pass over a gatewayhub that serves the chains. The
    /// served chains and the recorded `kind=chain` rows are compared bit for bit, and the recorded rows
    /// are also the flattening of the full chain (ground truth), not only the other path's.
    #[test]
    fn the_chain_pass_serves_and_records_the_same_rows_over_the_hop() {
        vike_log::test_init();
        let full = fixtures();

        // Path A.
        let store_a = Arc::new(MemHistStore::new());
        let rec_a =
            chain_recorder(true, Arc::clone(&store_a) as Arc<dyn HistStore + Send + Sync>, 60_000)
                .expect("armed");
        let mut served_a = BTreeMap::new();
        for c in &full {
            record_chain(&rec_a, c);
            served_a.insert(c.expiry.date.clone(), limit_strikes(c.clone(), Some(STRIKE_WINDOW)));
        }

        // Path B.
        let mut rest = ScriptedRest::new()
            .with_options_underlyings(Ok(vec!["BTC".into()]))
            .with_options_expiries("BTC", Ok(full.iter().map(|c| c.expiry.clone()).collect()));
        for c in &full {
            rest = rest.with_options_chain("BTC", &c.expiry.date, Ok(c.clone()));
        }
        let gateway = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());
        let store_b = Arc::new(MemHistStore::new());
        let rec_b =
            chain_recorder(true, Arc::clone(&store_b) as Arc<dyn HistStore + Send + Sync>, 60_000)
                .expect("armed");
        let fetched = fetch_options(&RemoteOptions::new(gateway.link()), Some(rec_b.as_ref()))
            .expect("the pass fetched");
        let bundle = &fetched["BTC"];

        // The floors: the window trimmed the front chain, the recorder got every strike.
        assert!(
            served_a["2037-06-25"].rows.len() < full[0].rows.len()
                && served_a["2037-06-25"].rows.len() > 30,
            "the window must trim the front chain: {} of {}",
            served_a["2037-06-25"].rows.len(),
            full[0].rows.len()
        );
        let rows_a = store_a.scan_chain("deribit", "BTC", TsRange::all()).expect("scan");
        assert_eq!(rows_a.len(), 2 * 100 + 2 * 3, "a call and a put at every strike, unwindowed");

        // Served: the same chains, byte for byte.
        assert_eq!(bundle.chains.keys().collect::<Vec<_>>(), served_a.keys().collect::<Vec<_>>());
        for (date, want) in &served_a {
            assert_eq!(
                format!("{:?}", bundle.chains[date]),
                format!("{want:?}"),
                "the served chain for {date} differs between the paths"
            );
        }
        assert_eq!(bundle.default_expiry, "2037-06-25");
        assert_eq!(
            bundle.expiries.iter().map(|e| e.date.as_str()).collect::<Vec<_>>(),
            ["2037-06-25", "2037-12-24"]
        );

        // Recorded: the same rows, and they are the full chains' own rows.
        let rows_b = store_b.scan_chain("deribit", "BTC", TsRange::all()).expect("scan");
        assert_eq!(format!("{rows_b:?}"), format!("{rows_a:?}"), "the recorded rows differ");
        let truth: Vec<_> = full.iter().flat_map(chain_snapshot_rows).collect();
        assert_eq!(
            format!("{rows_b:?}"),
            format!("{truth:?}"),
            "the recorded rows are not the full chain's"
        );

        // The request: the full chain, never a window.
        let windows: Vec<_> = rest
            .options_calls()
            .into_iter()
            .filter_map(|c| match c {
                OptionsCall::Chain { strike_window, .. } => Some(strike_window),
                _ => None,
            })
            .collect();
        assert!(
            windows.len() == 2 && windows.iter().all(Option::is_none),
            "the datahub must ask for the full chain: {windows:?}"
        );
    }

    fn marks() -> Vec<Vec<OptionTicker>> {
        let row = |i: i64, name: &str| {
            let mut t = OptionTicker::new(ASOF + i, name);
            t.bid = Some(-0.0);
            t.ask = Some(0.1 + 0.2 + i as f64 * 1e-9);
            t.bid_size = Some(1.0 / 3.0);
            t.mark = Some(f64::MIN_POSITIVE);
            t.mark_iv = Some(0.625 + i as f64 * 1e-12);
            t
        };
        vec![
            vec![row(1, "BTC-25JUN37-100000-C"), row(2, "BTC-25JUN37-100000-P")],
            vec![row(3, "BTC-25JUN37-110000-C")],
            vec![
                row(4, "BTC-24DEC37-120000-P"),
                row(5, "BTC-24DEC37-125000-C"),
                row(6, "BTC-24DEC37-130000-P"),
            ],
        ]
    }

    /// **The two feed streams deliver the same rows in the same order.** Path A hands each batch
    /// straight to the callback (what the in-process feed did); path B pushes it through a scripted
    /// daemon feed, over the wire, into `RemoteOptionsFeed`'s callback. The markprice batches and the
    /// ticker rows come out bit for bit and in order.
    #[test]
    fn the_feed_rows_arrive_bit_exact_and_in_order() {
        vike_log::test_init();
        let batches = marks();
        let tickers: Vec<OptionTicker> = batches.iter().flatten().cloned().collect();

        // Path A: straight into the callbacks.
        let a_marks: Vec<OptionTicker> = batches.iter().flatten().cloned().collect();
        let a_tickers = tickers.clone();

        // Path B: through the daemon.
        let rest = ScriptedRest::new().with_options_served();
        let gateway = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());
        let got_marks = Arc::new(Mutex::new(Vec::<OptionTicker>::new()));
        let got_tickers = Arc::new(Mutex::new(Vec::<OptionTicker>::new()));
        let sink = Arc::clone(&got_marks);
        let _markprice =
            RemoteOptionsFeed::markprice(gateway.link(), vec!["btc_usd".into()], move |rows| {
                lock(&sink).extend(rows)
            })
            .expect("the markprice feed");
        let sink = Arc::clone(&got_tickers);
        let _ticker = RemoteOptionsFeed::ticker(
            gateway.link(),
            vec!["BTC-25JUN37-100000-C".into()],
            move |row| lock(&sink).push(row),
        )
        .expect("the ticker feed");
        // The two feeds open on their own threads, so which is first is not ours to say: find each by
        // the spec it was opened with.
        let feeds = [rest.feed(0), rest.feed(1)];
        let by_spec = |wanted_marks: bool| {
            feeds
                .iter()
                .find(|f| matches!(f.spec, OptionsFeedSpec::MarkPrice { .. }) == wanted_marks)
                .unwrap_or_else(|| panic!("no feed was opened with that spec"))
        };
        let (marks_feed, ticker_feed) = (by_spec(true), by_spec(false));
        for batch in &batches {
            marks_feed.sink.marks(batch.clone());
        }
        for row in &tickers {
            ticker_feed.sink.ticker(row.clone());
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while lock(&got_marks).len() < a_marks.len() || lock(&got_tickers).len() < a_tickers.len() {
            assert!(
                Instant::now() < deadline,
                "the feeds delivered {} of {} marks and {} of {} tickers",
                lock(&got_marks).len(),
                a_marks.len(),
                lock(&got_tickers).len(),
                a_tickers.len()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));

        assert_eq!(a_marks.len(), 6);
        assert_eq!(
            format!("{:?}", lock(&got_marks).clone()),
            format!("{a_marks:?}"),
            "markprice rows"
        );
        assert_eq!(
            format!("{:?}", lock(&got_tickers).clone()),
            format!("{a_tickers:?}"),
            "ticker rows"
        );
    }
}

// ====================================================================================================
// Lane: backfill (the klines fold, funding, the tick lane, the credentialed lane, the presence word)
// ====================================================================================================

/// The datahub's backfill table over a gatewayhub (`real_backfill_table`, the production table) against
/// the pre-change shape: the same collectors (`vike_backfill`'s ingests, unchanged) over IN-PROCESS
/// sources, where the bridge's sources used to sit. Both tables are served by a datahub server into a
/// real `DataFusionHist` and driven through `DatahubClient::backfill`, so what is compared is what an
/// operator gets: the answer of each request, every stored row (bit for bit), every commit key, and what
/// the source was asked (venue, symbol, window, in order). Compiled with `backfill-serve`.
#[cfg(feature = "backfill-serve")]
mod backfill_lane {
    use super::*;

    use std::net::{SocketAddr, TcpListener};

    use vike_catalog::HistoryLane;
    use vike_data::source::{
        FUNDING_INTERVAL, FundingRatePoint, FundingRateSource, KlineSource, SourceError,
    };
    use vike_data::{HistStore, SeriesId, StoreKindId, TsRange};
    use vike_datahub::backfill::{
        BackfillTable, credentialed_klines_row, gateway_credential_probe, real_backfill_table,
    };
    use vike_datahub::serve_with_backfill;
    use vike_datahub_client::DatahubClient;
    use vike_datahub_client::history::CredentialPresence;
    use vike_gatewayhub::testkit::rest::Answer as RestAnswer;
    use vike_gatewayhub_client::credential::CredentialPresence as Asked;
    use vike_gatewayhub_client::history::{FUNDING_VENUES, KLINE_VENUES};
    use vike_gatewayhub_client::proto::{FetchFailure, FetchRows, FetchWhat};
    use vike_hub_proto::auth::HubKeys;
    use vike_store::DataFusionHist;

    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 86_400_000;
    /// Tuesday 2024-01-02T00:00:00Z: a settled weekday inside OANDA's dense series.
    const DAY0: i64 = 1_704_153_600_000;

    // ---- what the scripted venue says ---------------------------------------------------------------

    /// Three bars whose floats are not short decimals, at hours 1, 2 and 3.
    fn awkward_bars() -> Vec<Bar> {
        (1..=3_i64)
            .map(|i| {
                let open = 100.0 + 0.1 + 0.2 + i as f64 * 1e-9;
                Bar {
                    ts: i * HOUR,
                    open,
                    high: open + 1.0 / 3.0,
                    low: open - 1.0 / 7.0,
                    close: open + 1e-9 * i as f64,
                    volume: 1.0 / 3.0 + i as f64,
                    funding: (i == 1).then_some(0.0001 + 0.0002),
                    bid: None,
                    ask: None,
                    symbol: None,
                }
            })
            .collect()
    }

    fn funding_points() -> Vec<FundingRatePoint> {
        vec![
            FundingRatePoint { ts_ms: HOUR, rate: 0.0001 + 0.0002, premium: Some(1.0 / 3.0) },
            FundingRatePoint { ts_ms: 9 * HOUR, rate: -0.00005, premium: None },
        ]
    }

    /// Five ticks in two minutes, awkward bids and asks.
    fn ticks() -> Vec<QuoteTick> {
        [(0, 1.10001), (1_000, 1.10010), (2_000, 1.09990), (60_000, 1.10100), (61_500, 1.10080)]
            .into_iter()
            .map(|(ts, bid)| QuoteTick {
                ts,
                local_ts: 0,
                bid,
                ask: bid + 0.1 + 0.2,
                bid_size: 1.0 / 3.0,
                ask_size: 1.0,
                symbol: "EURUSD".to_string(),
            })
            .collect()
    }

    /// The scripted venues of the keyless lanes: binance answers bars and funding, bybit refuses, okx
    /// fails, dukascopy answers ticks.
    fn kline_outcome(venue: &str) -> Option<Result<Vec<Bar>, SourceError>> {
        match venue {
            "binance" => Some(Ok(awkward_bars())),
            "bybit" => Some(Err(SourceError::Refused(
                "bybit refuses this spelling, nothing was asked".into(),
            ))),
            "okx" => Some(Err(SourceError::Fetch("okx answered 429 after three tries".into()))),
            _ => None,
        }
    }

    // ---- path A: in-process sources -----------------------------------------------------------------

    struct InProcessKlines {
        venue: &'static str,
        outcome: Result<Vec<Bar>, SourceError>,
        asked: Arc<Mutex<Vec<String>>>,
    }

    impl KlineSource for InProcessKlines {
        fn venue(&self) -> &str {
            self.venue
        }
        fn fetch(
            &self,
            symbol: &str,
            interval: &str,
            start_ms: i64,
            end_ms: i64,
        ) -> Result<Vec<Bar>, SourceError> {
            lock(&self.asked)
                .push(format!("{} {symbol} {interval} {start_ms} {end_ms}", self.venue));
            self.outcome.clone()
        }
    }

    struct InProcessFunding {
        venue: &'static str,
        asked: Arc<Mutex<Vec<String>>>,
    }

    impl FundingRateSource for InProcessFunding {
        fn venue(&self) -> &str {
            self.venue
        }
        fn fetch(
            &self,
            symbol: &str,
            start_ms: i64,
            end_ms: i64,
        ) -> Result<Vec<FundingRatePoint>, SourceError> {
            lock(&self.asked).push(format!("{} {symbol} {start_ms} {end_ms}", self.venue));
            Ok(funding_points())
        }
    }

    /// The pre-change `real_backfill_table`: the same collectors `real_backfill_table` builds, over the
    /// in-process sources the bridges were. Rows are added in the rosters' order, then dukascopy's
    /// tick lane; the credentialed lane has its own scenario.
    fn in_process_table(
        store: &Arc<DataFusionHist>,
        asked: &Arc<Mutex<Vec<String>>>,
    ) -> BackfillTable {
        let mut table = BackfillTable::new(
            KLINE_VENUES
                .iter()
                .copied()
                .map(|venue| {
                    let (store, asked) = (Arc::clone(store), Arc::clone(asked));
                    let collect: vike_datahub::backfill::BackfillFn =
                        Box::new(move |symbol, interval, start, end, _| {
                            let source = InProcessKlines {
                                venue,
                                outcome: kline_outcome(venue).unwrap_or_else(|| {
                                    Err(SourceError::Fetch("not scripted".into()))
                                }),
                                asked: Arc::clone(&asked),
                            };
                            vike_backfill::kline_source::backfill_kline_source(
                                &store, &source, symbol, interval, start, end,
                            )
                            .map_err(|e| e.to_string())
                        });
                    (venue.to_string(), collect)
                })
                .collect(),
        );
        for venue in FUNDING_VENUES {
            let (store, asked) = (Arc::clone(store), Arc::clone(asked));
            table = table.with(
                venue,
                HistoryLane::Funding,
                Box::new(
                    move |symbol: &str, _: &str, start: i64, end: i64, _: &dyn Fn() -> bool| {
                        let source = InProcessFunding { venue, asked: Arc::clone(&asked) };
                        vike_backfill::funding_rate::backfill_funding_rate(
                            &store, &source, symbol, start, end,
                        )
                        .map(|written| written.rate_rows)
                        .map_err(|e| e.to_string())
                    },
                ),
            );
        }
        let (tick_store, tick_asked) = (Arc::clone(store), Arc::clone(asked));
        let table = table.with(
            vike_backfill::venues::dukascopy::VENUE,
            HistoryLane::TickBars,
            Box::new(
                move |symbol: &str,
                      interval: &str,
                      start: i64,
                      end: i64,
                      should_stop: &dyn Fn() -> bool| {
                    vike_backfill::venues::dukascopy::backfill_quotes_then_bars(
                        &tick_store,
                        symbol,
                        interval,
                        start,
                        end,
                        should_stop,
                        |sym, from, to| {
                            lock(&tick_asked).push(format!("dukascopy {sym} {from} {to}"));
                            Ok(ticks())
                        },
                    )
                    .map_err(|e| e.to_string())
                },
            ),
        );
        // ...and the credentialed row last, appended after dukascopy as the pre-change table appended it.
        let oanda_asked = Arc::clone(asked);
        let (venue, collect) = credentialed_klines_row(
            Arc::clone(store),
            move || InProcessOanda { asked: Arc::clone(&oanda_asked) },
            // The row's own refusals are the datahub's and untouched by the move; the requests of this
            // file are ones they let through.
            |_, _, _, _| None,
        );
        table.with(&venue, HistoryLane::CredentialedKlines, collect)
    }

    // ---- path B: the production table over a scripted gatewayhub -------------------------------------

    fn scripted_rest() -> ScriptedRest {
        let mut rest = ScriptedRest::new();
        for venue in KLINE_VENUES {
            if let Some(outcome) = kline_outcome(venue) {
                rest = rest.with_klines(
                    venue,
                    match outcome {
                        Ok(bars) => RestAnswer::rows(bars),
                        Err(error) => RestAnswer::error(error),
                    },
                );
            }
        }
        rest = rest
            .with_funding("binance", RestAnswer::rows(funding_points()))
            .with_ticks("dukascopy", RestAnswer::rows(ticks()));
        rest
    }

    /// What the daemon was asked, in the form path A's sources record.
    fn asked_of(rest: &ScriptedRest) -> Vec<String> {
        rest.fetches()
            .into_iter()
            .map(|call| match call.what {
                FetchWhat::Klines { venue, symbol, interval, start_ms, end_ms } => {
                    format!("{venue} {symbol} {interval} {start_ms} {end_ms}")
                }
                FetchWhat::Funding { venue, symbol, start_ms, end_ms }
                | FetchWhat::Ticks { venue, symbol, start_ms, end_ms } => {
                    format!("{venue} {symbol} {start_ms} {end_ms}")
                }
                other => panic!("not a history fetch: {other:?}"),
            })
            .collect()
    }

    // ---- the harness ---------------------------------------------------------------------------------

    fn empty_store() -> (tempfile::TempDir, Arc<DataFusionHist>) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = Arc::new(DataFusionHist::open(dir.path()).expect("a store"));
        (dir, store)
    }

    fn serve(store: &Arc<DataFusionHist>, table: BackfillTable) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("address");
        let store = Arc::clone(store);
        std::thread::Builder::new()
            .name("twin-backfill".into())
            .spawn(move || {
                let _ = serve_with_backfill(listener, store, Some(table));
            })
            .expect("spawn the datahub server");
        addr
    }

    /// One request of a scenario.
    type Request = (&'static str, &'static str, &'static str, i64, i64);

    /// What a scenario observed.
    struct Observed {
        /// `{:?}` of each request's answer, in order.
        answers: Vec<String>,
        /// What the source side was asked, in order.
        asked: Vec<String>,
        /// Every series, sorted.
        series: Vec<String>,
        /// Each series' commit keys.
        commits: Keyed,
        /// Each series' rows, exact lines.
        rows: Keyed,
    }

    fn lines<T: std::fmt::Debug>(rows: Vec<T>) -> Vec<String> {
        rows.iter().map(|r| format!("{r:?}")).collect()
    }

    fn view(store: &DataFusionHist) -> (Vec<String>, Keyed, Keyed) {
        let mut ids: Vec<SeriesId> = store.list_series().expect("list_series");
        ids.sort();
        let (mut series, mut commits, mut rows) = (Vec::new(), Keyed::new(), Keyed::new());
        for id in &ids {
            let label = format!("{id:?}");
            commits.insert(label.clone(), store.series_commits(id).expect("series_commits"));
            let all = TsRange::all;
            let body = match id.kind {
                StoreKindId::Bar => lines(
                    store
                        .load_bars(
                            &id.venue,
                            &id.symbol,
                            id.interval.as_deref().unwrap_or(""),
                            all(),
                        )
                        .expect("load_bars"),
                ),
                StoreKindId::Quote => {
                    lines(store.scan_quotes(&id.venue, &id.symbol, all()).expect("scan_quotes"))
                }
                StoreKindId::PerpMetrics => lines(
                    store
                        .scan_perp_metrics(&id.venue, &id.symbol, all())
                        .expect("scan_perp_metrics"),
                ),
                other => vec![format!("a {other:?} series this twin does not scan")],
            };
            rows.insert(label.clone(), body);
            series.push(label);
        }
        (series, commits, rows)
    }

    /// Drive `requests` through a datahub server over `route`'s table into a fresh store.
    fn run(route: Route, requests: &[Request]) -> Observed {
        let (_dir, store) = empty_store();
        let asked = Arc::new(Mutex::new(Vec::new()));
        // Declared first, so it is dropped last (the connections to it close first).
        let rest = scripted_rest();
        let gateway = (route == Route::Hop)
            .then(|| LoopbackGateway::start(ScriptedData::new(&[]), rest.clone()));
        let table = match &gateway {
            Some(gateway) => real_backfill_table(Arc::clone(&store), gateway.link()),
            None => in_process_table(&store, &asked),
        };
        let addr = serve(&store, table);
        let mut client = DatahubClient::connect(addr).expect("connect");
        let answers = requests
            .iter()
            .map(|(venue, symbol, interval, start, end)| {
                format!("{:?}", client.backfill(venue, symbol, interval, *start, *end))
            })
            .collect();
        let asked = match route {
            Route::InProcess => lock(&asked).clone(),
            Route::Hop => asked_of(&rest),
        };
        let (series, commits, rows) = view(&store);
        Observed { answers, asked, series, commits, rows }
    }

    fn assert_twins(what: &str, a: &Observed, b: &Observed) {
        assert_eq!(b.answers, a.answers, "{what}: the answers differ");
        assert_eq!(b.asked, a.asked, "{what}: the source was asked different things");
        assert_eq!(b.series, a.series, "{what}: the stores hold different series");
        assert_same(&format!("{what}, rows"), &a.rows, &b.rows);
        assert_same(&format!("{what}, commit keys"), &a.commits, &b.commits);
    }

    // ---- the lanes -----------------------------------------------------------------------------------

    /// **The klines fold stores the same rows and says the same words.** A request to a roster venue
    /// lands bit-exact bars under the same commit key and a repeat writes nothing; a refusal and a
    /// venue failure keep their variant and text through the ingest's own prefix; a stranger is refused
    /// naming the supported set, in the roster's order. The source is asked for the request's own
    /// window, once, on both routes.
    #[test]
    fn the_klines_fold_stores_the_same_rows_and_answers_the_same_words() {
        vike_log::test_init();
        let requests: [Request; 5] = [
            ("binance", "BACKFILLUSDT", "1h", 0, 4 * HOUR),
            ("binance", "BACKFILLUSDT", "1h", 0, 4 * HOUR),
            ("bybit", "BACKFILLUSDT", "1h", 0, 4 * HOUR),
            ("okx", "BACKFILLUSDT", "1h", 0, 4 * HOUR),
            ("kraken", "BACKFILLUSDT", "1h", 0, 4 * HOUR),
        ];
        let a = run(Route::InProcess, &requests);
        let b = run(Route::Hop, &requests);

        // The floors, read off the in-process run (the contract).
        assert!(a.answers[0].contains("rows_written: 3"), "{}", a.answers[0]);
        assert!(
            a.answers[1].contains("rows_written: 0"),
            "a spent window writes nothing: {}",
            a.answers[1]
        );
        assert!(
            a.answers[2].contains("bybit refuses this spelling, nothing was asked"),
            "{}",
            a.answers[2]
        );
        assert!(
            a.answers[3].contains("okx answered 429 after three tries")
                && a.answers[3].contains("venue fetch"),
            "{}",
            a.answers[3]
        );
        assert!(
            a.answers[4]
                .contains("binance, bybit, okx, aster, deribit, hyperliquid, dukascopy, oanda"),
            "the unknown-venue refusal names the roster in its declared order: {}",
            a.answers[4]
        );
        assert!(a.asked.len() >= 3, "binance, bybit and okx were each asked: {:?}", a.asked);
        assert!(
            a.asked.iter().all(|call| call.ends_with(&format!(" 0 {}", 4 * HOUR))),
            "the request's own window: {:?}",
            a.asked
        );
        assert_eq!(a.series.len(), 1, "only the successful venue stored anything: {:?}", a.series);

        // The ground truth: the stored rows are the script's bars.
        let stored = a.rows.values().next().expect("one series");
        assert_eq!(stored, &lines(awkward_bars()), "the stored bars are not the venue's");

        assert_twins("klines", &a, &b);
    }

    /// **The funding lane stores the same rate and premium rows**, and asks the source the same window.
    #[test]
    fn the_funding_lane_stores_the_same_rate_and_premium_rows() {
        vike_log::test_init();
        let requests: [Request; 2] = [
            ("binance", "BACKFILLUSDT", FUNDING_INTERVAL, 0, 10 * HOUR),
            ("binance", "BACKFILLUSDT", FUNDING_INTERVAL, 0, 10 * HOUR),
        ];
        let a = run(Route::InProcess, &requests);
        let b = run(Route::Hop, &requests);
        assert!(a.answers[0].contains("rows_written: 2"), "{}", a.answers[0]);
        assert!(a.answers[1].contains("rows_written: 0"), "{}", a.answers[1]);
        assert_eq!(a.series.len(), 2, "the rate series and the premium series: {:?}", a.series);
        assert_twins("funding", &a, &b);
    }

    /// **The tick lane stores the same quotes and the same resampled bars** (dukascopy: the fetch is the
    /// only thing that moved; the store-and-resample half is `vike_backfill`'s, run on both routes).
    #[test]
    fn the_tick_lane_stores_the_same_quotes_and_resampled_bars() {
        vike_log::test_init();
        let requests: [Request; 1] = [("dukascopy", "EURUSD", "1m", 0, 119_999)];
        let a = run(Route::InProcess, &requests);
        let b = run(Route::Hop, &requests);
        assert!(a.answers[0].contains("rows_written: 2"), "two 1m buckets: {}", a.answers[0]);
        assert_eq!(a.series.len(), 2, "quotes and bars: {:?}", a.series);
        assert_eq!(a.asked.len(), 1, "one chunk, one fetch: {:?}", a.asked);
        assert_twins("ticks", &a, &b);
    }

    // ---- the credentialed lane ------------------------------------------------------------------------

    /// Three bars at the start of whatever window the source is handed, as a real source answers.
    fn oanda_bars(start_ms: i64) -> Vec<Bar> {
        (0..3_i64)
            .map(|i| Bar {
                ts: start_ms + i * 5_000,
                open: 1.08125,
                high: 1.0815,
                low: 1.081,
                close: 1.08137 + i as f64 * 1e-9,
                volume: 12.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            })
            .collect()
    }

    struct InProcessOanda {
        asked: Arc<Mutex<Vec<String>>>,
    }

    impl KlineSource for InProcessOanda {
        fn venue(&self) -> &str {
            "oanda"
        }
        fn fetch(
            &self,
            symbol: &str,
            interval: &str,
            start_ms: i64,
            end_ms: i64,
        ) -> Result<Vec<Bar>, SourceError> {
            lock(&self.asked).push(format!("oanda {symbol} {interval} {start_ms} {end_ms}"));
            Ok(oanda_bars(start_ms))
        }
    }

    fn run_credentialed(route: Route) -> Observed {
        let (_dir, store) = empty_store();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let rest = ScriptedRest::new()
            // These advertise the lanes; `on_fetch` answers every fetch itself.
            .with_klines("binance", RestAnswer::rows(Vec::new()))
            .with_credentialed_klines("oanda", RestAnswer::rows(Vec::new()))
            .on_fetch(|_, what, _credentialed, _| match what {
                FetchWhat::Klines { start_ms, .. } => Ok(FetchRows::Bars(oanda_bars(*start_ms))),
                other => Err(FetchFailure::Fetch(format!("unscripted fetch: {other:?}"))),
            });
        let gateway = (route == Route::Hop)
            .then(|| LoopbackGateway::start(ScriptedData::new(&[]), rest.clone()));
        let table = match &gateway {
            Some(gateway) => real_backfill_table(Arc::clone(&store), gateway.link()),
            None => in_process_table(&store, &asked),
        };
        let addr = serve(&store, table);
        let mut client = DatahubClient::connect(addr).expect("connect");
        let requests: [Request; 2] = [
            ("oanda", "EUR_USD", "5s", DAY0 + HOUR, DAY0 + DAY + HOUR),
            ("oanda", "EUR_USD", "5s", DAY0 + HOUR, DAY0 + DAY + HOUR),
        ];
        let answers = requests
            .iter()
            .map(|(venue, symbol, interval, start, end)| {
                format!("{:?}", client.backfill(venue, symbol, interval, *start, *end))
            })
            .collect();
        let asked = match route {
            Route::InProcess => lock(&asked).clone(),
            Route::Hop => {
                assert!(
                    rest.fetches().iter().all(|f| f.credentialed),
                    "the credentialed verb alone"
                );
                asked_of(&rest)
            }
        };
        let (series, commits, rows) = view(&store);
        Observed { answers, asked, series, commits, rows }
    }

    /// **The credentialed lane ingests the same days.** A ragged two-day window is asked as two whole UTC
    /// days (the chunked ingest, never the per-window one), each stored under its own day key; a repeat
    /// fetches nothing. On the hop every fetch is the credentialed verb.
    #[test]
    fn the_credentialed_lane_ingests_the_same_whole_days() {
        vike_log::test_init();
        let a = run_credentialed(Route::InProcess);
        let b = run_credentialed(Route::Hop);
        assert!(a.answers[0].contains("rows_written: 6"), "{}", a.answers[0]);
        assert!(a.answers[1].contains("rows_written: 0"), "{}", a.answers[1]);
        assert_eq!(
            a.asked,
            [
                format!("oanda EUR_USD 5s {DAY0} {}", DAY0 + DAY - 1),
                format!("oanda EUR_USD 5s {} {}", DAY0 + DAY, DAY0 + 2 * DAY - 1),
            ],
            "whole UTC days, never the ragged window"
        );
        assert_twins("credentialed", &a, &b);
    }

    // ---- the table's shape, and the presence word -------------------------------------------------------

    fn dead_link(keys: HubKeys) -> vike_gatewayhub_client::client::GatewayLink {
        vike_gatewayhub_client::client::GatewayLink::new("127.0.0.1:1", keys)
    }

    /// **The table has the same venues in the same order.** The supported set is what the unknown-venue
    /// refusal prints, so its order is observable; the funding set and the chart seed's set are the
    /// same two derived lists.
    #[test]
    fn the_table_names_the_same_venues_in_the_same_order() {
        vike_log::test_init();
        let (_dir, store) = empty_store();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let a = in_process_table(&store, &asked);
        let b = real_backfill_table(
            Arc::clone(&store),
            dead_link(HubKeys::new(Vec::new(), Vec::new())),
        );
        assert_eq!(
            b.supported(),
            ["binance", "bybit", "okx", "aster", "deribit", "hyperliquid", "dukascopy", "oanda"],
            "the declared order, written out: the pre-change registry's"
        );
        assert_eq!(b.supported(), a.supported());
        assert_eq!(b.funding_supported(), ["binance", "hyperliquid"]);
        assert_eq!(b.funding_supported(), a.funding_supported());
        assert_eq!(b.seed_supported(), a.seed_supported());
        assert_eq!(
            b.seed_supported(),
            ["binance", "bybit", "okx", "aster", "deribit", "hyperliquid"]
        );
        assert!(b.get_for_seed("oanda").is_none() && b.get_for_seed("dukascopy").is_none());
    }

    /// **The presence word maps one for one, and a daemon that cannot be asked is `NotChecked`.** For each
    /// of the five words the pre-change probe returned directly, the table over a scripted gatewayhub
    /// answers the same word; a dead port, an unkeyed link and a daemon that has stopped all answer
    /// `NotChecked`, never `Absent` (nothing was read, so nothing may be called not stored).
    #[test]
    fn the_presence_word_maps_one_for_one_and_an_unreachable_daemon_is_not_checked() {
        vike_log::test_init();
        let words = [
            (Asked::NotNeeded, CredentialPresence::NotNeeded),
            (Asked::Present, CredentialPresence::Present),
            (Asked::Absent, CredentialPresence::Absent),
            (Asked::Unreadable, CredentialPresence::Unreadable),
            (Asked::NotChecked, CredentialPresence::NotChecked),
        ];
        let (_dir, store) = empty_store();
        for (asked, want) in words {
            // Path A: the probe handed its word straight to the table.
            let a = BackfillTable::new(Vec::new())
                .with(
                    "oanda",
                    HistoryLane::CredentialedKlines,
                    Box::new(|_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| Ok(0)),
                )
                .with_credential_probe("oanda", Box::new(move || want));
            // Path B: the production table asks a scripted gatewayhub.
            let rest = ScriptedRest::new().with_presence("oanda", asked);
            let gateway = LoopbackGateway::start(ScriptedData::new(&[]), rest);
            let b = real_backfill_table(Arc::clone(&store), gateway.link());
            let lane = HistoryLane::CredentialedKlines;
            assert_eq!(a.credential_presence("oanda", lane), want);
            assert_eq!(
                b.credential_presence("oanda", lane),
                want,
                "{asked:?} did not map to its twin"
            );
            assert_eq!(
                b.credential_presence("binance", HistoryLane::Klines),
                CredentialPresence::NotNeeded,
                "a keyless row needs no credential"
            );
        }

        // A daemon that cannot be asked: nothing was read.
        let probe = |link| gateway_credential_probe(link, "oanda")();
        assert_eq!(
            probe(dead_link(vike_gatewayhub::testkit::gateway::keys())),
            CredentialPresence::NotChecked
        );
        assert_eq!(
            probe(dead_link(HubKeys::new(Vec::new(), Vec::new()))),
            CredentialPresence::NotChecked
        );
        let rest = ScriptedRest::new().with_presence("oanda", Asked::Present);
        let mut gateway = LoopbackGateway::start(ScriptedData::new(&[]), rest);
        let link = gateway.link();
        assert_eq!(probe(link.clone()), CredentialPresence::Present);
        gateway.stop().expect("the server stopped");
        assert_eq!(
            probe(link),
            CredentialPresence::NotChecked,
            "a stopped daemon is not an absent key"
        );
    }
}
