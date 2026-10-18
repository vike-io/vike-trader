//! **Any symbol over the REAL hop** (decisions 0173 and 0174): the control door lets an order name a symbol
//! its engine was not started on, resolves the symbol's facts through the account's OWN gatewayhub trade
//! lease, subscribes a price feed for it, sends `Command::AdmitSymbol` and then the order. The in-crate tests
//! (`src/server/tests/admits_any_symbol.rs`, `src/readmit_tests.rs`) drive that door over a table of fake
//! facts; these drive it over the pieces the table stands in for.
//!
//! # What is real, what is faked
//!
//! Real, on the node: `RemoteBackend` and the lease it opens (`TradeSession::symbol_facts`), `LeaseFacts`, the
//! door (`accept_command` into a real `vike_core` core with a fail-closed engine), `readmit`, the boot restore
//! (`restore_for_boot`), `wire_venue_feeds` (the CEX arm: `StratFeed`, `TapSink`, `PriceTap`, `CexWatcher`).
//! Real, on the other side: the gatewayhub server (`serve_with_trade`), its lease and session, the verb
//! `gw_trade_symbol_facts` and the gateway's `SymbolFacts` (spelling, listing, refusals), and the datahub's
//! strategy lane (`StratHub` over a real server).
//!
//! Faked: the VENUE. Its order book is `hop_rig::World` (the scripted venue under the real `ExecActor`), its
//! instrument catalog is a list of fixture rows (`Catalog`), and the gatewayhub's tick pump is a socket that
//! speaks the tube (`FakePump`: the same fake `crates/vike-datahub/tests/strat_lane.rs` uses, made to tick).
//! The venue is named `binance` because the door's `SYMBOL_RULES` row for it is `Admits`.
//!
//! # Not real
//!
//! * "A gatewayhub without `gw_trade_symbol_facts`" is a scripted wire peer (`fake_trade_peer`): the real server
//!   always advertises the verb. The node's refusal is decided locally from the advertisement, so the peer
//!   proves it by recording that nothing was sent.
//! * The boot is replayed from its public pieces in `live_runtime`'s own order (`Node::boot`): `live_runtime`
//!   itself is private to the binary's tree and takes a `RemoteCtor` no integration test can name.

use std::collections::{HashMap, HashSet};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use vike_core::replay::{RestoreOutcome, restore_for_boot};
use vike_core::{CommandSink, CoreConfig, CoreHandle, JournalConfig};
use vike_data::{DataClient, HistStore, LiveDataSink, MemHistStore};
use vike_datahub::feeds::{ClientBuilder, FeedBroker, Sharing};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub::strat::{StratHub, StratLimits, TicksBuilder};
use vike_exec::{
    Command, CoreSnapshot, ExecutionClient, ExecutionEngine, Ingest, SymbolAdmission, VenueBlock,
};
use vike_gatewayhub::testkit::data::ScriptedData;
use vike_gatewayhub::trade::adapters::symbol_facts::InstrumentList;
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::proto::{FEATURE_GW_TRADE_SYMBOL_FACTS, TradeUp};
use vike_gatewayhub_client::trade::TradeLink;
use vike_hub_proto::auth::HubKeys;
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_market_stream::frame::{read_capped, write_capped};
use vike_market_stream::market::RemoteDataClient;
use vike_market_stream::opener::{DataStream, Opener, StreamLimits, StreamTimings};
use vike_market_stream::wire::{ByeReason, StreamDown, StreamUp, SubOp};
use vike_marketdata::QuoteTick;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{AccountState, Event};
use vike_model::{
    AccountTier, Contract, Instrument, Margining, OrderRequest, OrderStatus, PerpSpec,
    SymbolProperties, TradingState,
};
use vike_tradehub::feeds::{
    FeedCtors, LiveFeeds, ProdFeedCtors, wire_cex_price_feed, wire_hl_price_feed,
    wire_price_only_feeds, wire_venue_feeds,
};
use vike_tradehub::readmit::{Readmitted, Target, readmit, readmit_and_price, targets};
use vike_tradehub::remote_backend::RemoteBackend;
use vike_tradehub::remote_backend::facts::{LeaseFacts, MAX_WATCHED};
use vike_tradehub::remote_backend::restore::BootHints;
use vike_tradehub::remote_backend::reteach::SnapshotCell;
use vike_tradehub::server::control::{
    Accepted, ControlLimits, ControlLimitsConfig, accept_command,
};
use vike_tradehub::server::symbol_admission::{SymbolFacts, admitting_venues};
use vike_tradehub::{CexVenue, HlNetwork, VenuePlan};
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};
use vike_tradehub_core::{MakerConfig, VenueBackend as _};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/hop_rig.rs"]
mod hop_rig;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../vike-gatewayhub-client/tests/support/fake_trade_peer.rs"]
mod fake_trade_peer;

use hop_rig::{
    CoreRig, Fate, Hop, SYMBOL, VENUE, World, accounts, arming, fast_trade_timings, live_lane,
    poll_lane, start_query, trade_account, wait_until,
};

const WAIT: Duration = Duration::from_secs(15);
/// The foreign symbols the tests trade: spot, like the rig's mounted `BTCUSDT`.
const ETH: &str = "ETHUSDT";
const SOL: &str = "SOLUSDT";
/// The datahub's strategy-lane key (`Scope::Trade`).
const LANE_KEY: &[u8] = b"any-symbol-twin-strategy-lane-key";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---- the fake venue catalog ---------------------------------------------------------------------------------

/// A gate on the catalog: while armed, a listing parks until it is released, so a test can act while a facts
/// request is IN FLIGHT at the gatewayhub. Bounded, so a test that forgets to release does not hang.
struct Hold {
    on: AtomicBool,
    entered: Mutex<Option<Sender<()>>>,
}

impl Hold {
    /// Park the next listing; the receiver is told when one is parked.
    fn arm(&self) -> Receiver<()> {
        let (tx, rx) = channel();
        *lock(&self.entered) = Some(tx);
        self.on.store(true, Ordering::SeqCst);
        rx
    }

    fn release(&self) {
        self.on.store(false, Ordering::SeqCst);
    }

    fn pass(&self) {
        if !self.on.load(Ordering::SeqCst) {
            return;
        }
        if let Some(tx) = lock(&self.entered).as_ref() {
            let _ = tx.send(());
        }
        let parked = Instant::now();
        while self.on.load(Ordering::SeqCst) && parked.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// The venue's public instrument list, as the gatewayhub's real `SymbolFacts` reads it.
struct Catalog {
    rows: Mutex<Vec<Instrument>>,
    hold: Hold,
    /// While set, the venue's listing fails with this text (a network error or a 429).
    fails: Mutex<Option<String>>,
    /// How many listings were asked of the venue.
    fetched: AtomicUsize,
}

fn grid(tick: f64, step: f64) -> SymbolProperties {
    SymbolProperties {
        tick_size: tick,
        step_size: step,
        min_qty: step,
        min_notional: 5.0,
        ..Default::default()
    }
}

fn spot(symbol: &str, p: SymbolProperties) -> Instrument {
    Instrument::new(VENUE, symbol, "BASE", "USDT", Contract::CryptoSpot).with_properties(p)
}

fn perp(symbol: &str, p: SymbolProperties) -> Instrument {
    let spec = PerpSpec {
        margining: Margining::Linear,
        settle: "USDT".into(),
        value_ccy: None,
        funding_interval_ms: None,
        settling_ms: None,
    };
    Instrument::new(VENUE, format!("{symbol}.P"), "BASE", "USDT", Contract::Perp(spec))
        .with_properties(p)
}

/// The fixture rows, with `eth` as ETHUSDT's grid. `NOTICKUSDT` publishes no tick.
fn rows(eth: SymbolProperties) -> Vec<Instrument> {
    vec![
        spot(SYMBOL, grid(0.1, 0.001)),
        spot(ETH, eth),
        spot(SOL, grid(0.001, 0.01)),
        spot("NOTICKUSDT", grid(0.0, 0.1)),
        perp(ETH, grid(0.05, 0.001)),
    ]
}

impl Catalog {
    fn new(rows: Vec<Instrument>) -> Arc<Self> {
        Arc::new(Catalog {
            rows: Mutex::new(rows),
            hold: Hold { on: AtomicBool::new(false), entered: Mutex::new(None) },
            fails: Mutex::new(None),
            fetched: AtomicUsize::new(0),
        })
    }

    /// How many listings the gatewayhub has asked of the venue.
    fn fetches(&self) -> usize {
        self.fetched.load(Ordering::SeqCst)
    }

    /// The venue's listing fails with `why` (or works again with `None`).
    fn fail_with(&self, why: Option<&str>) {
        *lock(&self.fails) = why.map(str::to_string);
    }

    /// The venue re-lists a symbol (a tick or a lot changed).
    fn set(&self, rows: Vec<Instrument>) {
        *lock(&self.rows) = rows;
    }

    fn list(self: &Arc<Self>) -> InstrumentList {
        let me = Arc::clone(self);
        Arc::new(move |_venue: &str| -> Result<Vec<Instrument>, String> {
            me.hold.pass();
            me.fetched.fetch_add(1, Ordering::SeqCst);
            if let Some(why) = lock(&me.fails).clone() {
                return Err(why);
            }
            Ok(lock(&me.rows).clone())
        })
    }
}

/// `x` rounded to the nearest multiple of `step`: what the engine's grid does to a price or a size.
fn snapped(x: f64, step: f64) -> f64 {
    (x / step).round() * step
}

// ---- the price side: a real datahub strategy lane over a fake pump ----------------------------------------

/// The mid of the quote the fake pump ticks for `symbol`.
fn mid_of(symbol: &str) -> f64 {
    match symbol {
        ETH => 3000.5,
        SOL => 150.5,
        _ => 60_000.5,
    }
}

struct PumpSub {
    sid: u64,
    k: u32,
    symbol: String,
    seq: u64,
}

/// Serve one tube connection of the pump: acknowledge each `Ticks` subscription, then tick a quote for it
/// every few milliseconds until it is unsubscribed or the socket closes. Records `ticks SYM TICK` and
/// `unsub SYM` in `log`; a muted symbol is acknowledged and never ticked.
fn serve_pump(
    mut read: TcpStream,
    log: Arc<Mutex<Vec<String>>>,
    mute: Arc<Mutex<HashSet<String>>>,
) {
    let Ok(write) = read.try_clone() else { return };
    let write = Arc::new(Mutex::new(write));
    let subs: Arc<Mutex<Vec<PumpSub>>> = Arc::default();
    let alive = Arc::new(AtomicBool::new(true));
    {
        let (write, subs, alive) = (Arc::clone(&write), Arc::clone(&subs), Arc::clone(&alive));
        let spawned =
            std::thread::Builder::new().name("fake-pump-ticker".into()).spawn(move || {
                while alive.load(Ordering::SeqCst) {
                    {
                        let mut subs = lock(&subs);
                        let mut out = lock(&write);
                        for sub in subs.iter_mut() {
                            if lock(&mute).contains(&sub.symbol) {
                                continue;
                            }
                            sub.seq += 1;
                            let mid = mid_of(&sub.symbol);
                            let quote = QuoteTick {
                                ts: i64::try_from(sub.seq).unwrap_or(i64::MAX),
                                local_ts: 0,
                                bid: mid - 0.5,
                                ask: mid + 0.5,
                                bid_size: 1.0,
                                ask_size: 1.0,
                                symbol: String::new(),
                            };
                            let frame = StreamDown::Quote { k: sub.k, seq: sub.seq, quote };
                            if write_capped(&mut *out, &frame, 1 << 20).is_err() {
                                alive.store(false, Ordering::SeqCst);
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
        if spawned.is_err() {
            return;
        }
    }
    let mut next_k = 0_u32;
    while let Ok(frame) = read_capped::<_, StreamUp>(&mut read, 64 * 1024) {
        match frame {
            StreamUp::Sub { sid, op: SubOp::Ticks { symbol, tick_size }, .. } => {
                lock(&log).push(format!("ticks {symbol} {tick_size}"));
                next_k += 1;
                // The SubOk is written BEFORE the subscription is visible to the ticker, so no quote
                // can overtake it on the wire.
                let ok = StreamDown::SubOk { sid, k: next_k };
                let _ = write_capped(&mut *lock(&write), &ok, 1 << 20);
                lock(&subs).push(PumpSub { sid, k: next_k, symbol, seq: 0 });
            }
            StreamUp::Unsub { sid } => {
                let mut subs = lock(&subs);
                let symbol = subs.iter().find(|s| s.sid == sid).map(|s| s.symbol.clone());
                lock(&log).push(format!("unsub {}", symbol.unwrap_or_else(|| sid.to_string())));
                subs.retain(|s| s.sid != sid);
            }
            StreamUp::Close => {
                let bye = StreamDown::Bye(ByeReason::ClientClosed);
                let _ = write_capped(&mut *lock(&write), &bye, 1 << 20);
                break;
            }
            _ => {}
        }
    }
    alive.store(false, Ordering::SeqCst);
}

/// An opener that dials the fake pump directly (no handshake: the pump is a fake peer).
struct DirectOpener(String);

impl Opener for DirectOpener {
    fn addr(&self) -> &str {
        &self.0
    }
    fn limits(&self) -> StreamLimits {
        StreamLimits {
            up_frame_ceiling_bytes: 64 * 1024,
            down_frame_ceiling_bytes: 8 * 1024 * 1024,
            max_symbol_bytes: 1024,
        }
    }
    fn timings(&self) -> StreamTimings {
        StreamTimings {
            sub_reply: Duration::from_secs(5),
            stream_read_floor: Duration::from_secs(45),
            reconnect_base: Duration::from_millis(50),
            reconnect_max: Duration::from_millis(200),
        }
    }
    fn failure_text(&self, e: &dyn std::fmt::Display) -> String {
        format!("fake pump {}: {e}", self.0)
    }
    fn open(&self, _venue: &str, observe: &dyn Fn(&TcpStream)) -> std::io::Result<DataStream> {
        let stream = TcpStream::connect(&self.0)?;
        observe(&stream);
        Ok(DataStream {
            stream,
            session: 1,
            epoch: 1,
            heartbeat: Duration::from_secs(5),
            delivery_class: true,
        })
    }
}

/// The price side of a node: a REAL datahub (strategy lane over a real `StratHub`) whose tick pump is the
/// fake pump. Its threads live to the end of the test process, like the pump of `strat_lane.rs`.
struct PriceHub {
    /// The datahub's address (what the tradehub's `ProdFeedCtors` dials).
    addr: String,
    /// What the pump was asked, in order: `ticks SYM TICK`, `unsub SYM`.
    asked: Arc<Mutex<Vec<String>>>,
    mute: Arc<Mutex<HashSet<String>>>,
}

impl PriceHub {
    fn start() -> Self {
        PriceHub::start_serving(&[VENUE])
    }

    /// A datahub whose strategy lane advertises exactly `venues` (what `datahub_probe` reads).
    fn start_serving(venues: &[&str]) -> Self {
        let asked: Arc<Mutex<Vec<String>>> = Arc::default();
        let mute: Arc<Mutex<HashSet<String>>> = Arc::default();
        // The fake pump: every connection (one per strategy-lane session) is served on its own thread.
        let pump = TcpListener::bind("127.0.0.1:0").expect("bind the fake pump");
        let pump_addr = pump.local_addr().expect("the pump's port").to_string();
        {
            let (log, mute) = (Arc::clone(&asked), Arc::clone(&mute));
            std::thread::Builder::new()
                .name("fake-pump-accept".into())
                .spawn(move || {
                    while let Ok((stream, _)) = pump.accept() {
                        let (log, mute) = (Arc::clone(&log), Arc::clone(&mute));
                        let _ = std::thread::Builder::new()
                            .name("fake-pump-conn".into())
                            .spawn(move || serve_pump(stream, log, mute));
                    }
                })
                .expect("spawn the fake pump");
        }
        // The datahub: the bar lane the CEX arm subscribes goes to a scripted client behind the real
        // broker; the tick lane goes to the pump.
        let scripted = ScriptedData::new(&[VENUE]);
        let builder: ClientBuilder = Box::new(
            move |venue: &str,
                  sink: Arc<dyn LiveDataSink>|
                  -> Result<Box<dyn DataClient + Send>, String> {
                scripted
                    .build(venue, None, sink)
                    .map(|client| Box::new(client) as Box<dyn DataClient + Send>)
            },
        );
        let broker = FeedBroker::new(builder, |_| Sharing::Shared);
        let ticks: TicksBuilder = Box::new(
            move |venue: &str, sink: Arc<dyn LiveDataSink>| -> Result<RemoteDataClient, String> {
                RemoteDataClient::new(DirectOpener(pump_addr.clone()), venue, sink)
                    .map_err(|e| e.to_string())
            },
        );
        let served = venues.iter().map(|v| (*v).to_string()).collect();
        let hub = StratHub::new(broker, served, Some(ticks), StratLimits::PRODUCTION);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the datahub");
        let addr = listener.local_addr().expect("the datahub's port").to_string();
        let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
        let lanes = ServerLanes { strat: Some(hub), ..ServerLanes::default() };
        let keys = HubKeys::new(b"any-symbol-observe".to_vec(), b"any-symbol-control".to_vec())
            .with_trade(LANE_KEY.to_vec());
        std::thread::Builder::new()
            .name("any-symbol-datahub".into())
            .spawn(move || {
                let _ = serve_with_lanes(
                    listener,
                    store,
                    None,
                    Some(keys),
                    None,
                    None,
                    lanes,
                    None,
                    false,
                    ServerTimings::PRODUCTION,
                );
            })
            .expect("spawn the datahub");
        PriceHub { addr, asked, mute }
    }

    fn asked(&self) -> Vec<String> {
        lock(&self.asked).clone()
    }

    fn count(&self, line: &str) -> usize {
        self.asked().iter().filter(|l| l.as_str() == line).count()
    }

    fn mute(&self, symbol: &str) {
        lock(&self.mute).insert(symbol.to_string());
    }

    fn unmute(&self, symbol: &str) {
        lock(&self.mute).remove(symbol);
    }
}

/// The node's CEX price feed over the real datahub, wired by the tradehub's own `wire_venue_feeds` (the same
/// arm `live_runtime` calls). The first subscribe waits for the dial; a transient refusal while it comes up
/// is retried, as the late arm would.
fn wire_feeds(hub: &PriceHub, core: &CoreHandle) -> LiveFeeds {
    let ctors = ProdFeedCtors::new(hub.addr.clone(), Some(LANE_KEY.to_vec()));
    let cfg = MakerConfig::crypto(VENUE, SYMBOL, 0.1, 0.001);
    let plan = VenuePlan::Cex { venue: CexVenue::Binance, mainnet: false };
    let (live, data_only) = (HashSet::new(), HashSet::new());
    let wrap = |sink: Arc<dyn LiveDataSink>| sink;
    let deadline = Instant::now() + WAIT;
    loop {
        match wire_venue_feeds(
            &plan,
            &[&cfg],
            core,
            &live,
            &[],
            &[AccountLabel::Default],
            &data_only,
            &wrap,
            &ctors,
        ) {
            Ok(feeds) => return feeds,
            Err(why) => {
                assert!(Instant::now() < deadline, "the price feeds never came up: {why}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

// ---- the node: a real core, the real lease, the real door ----------------------------------------------------

/// How the door prices a symbol it admits.
enum Prices<'a> {
    /// The lease for the facts and a fixed price (the tests that are about the facts).
    Fixed(f64),
    /// The lease for the facts and the real price path: the watcher the CEX feed hands out, subscribed
    /// through the datahub to the fake pump.
    Real(&'a PriceHub),
    /// The real price path of a venue NO MOUNT NAMES (the CI box's binance and okx): the price-only feed
    /// `live_runtime` wires for it, so no tick pump is subscribed at boot.
    Unmounted(&'a PriceHub),
    /// A node that wired no price feed for the venue at all (the shape the CI box had before the price-only feed).
    NoFeed,
}

/// `LeaseFacts` for everything but the price.
struct Fixed {
    inner: Arc<LeaseFacts>,
    px: f64,
}

impl SymbolFacts for Fixed {
    fn generation(&self, route_key: &str) -> u64 {
        self.inner.generation(route_key)
    }
    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, String> {
        self.inner.admission(route_key, venue, symbol)
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(self.px))
    }
    fn admitted(&self, route_key: &str, venue: &str, symbol: &str) {
        self.inner.admitted(route_key, venue, symbol);
    }
    fn release(&self, route_key: &str, venue: &str, symbol: &str) {
        self.inner.release(route_key, venue, symbol);
    }
    fn refresh(&self, route_key: &str, venue: &str, symbol: &str) -> Option<SymbolAdmission> {
        self.inner.refresh(route_key, venue, symbol)
    }
}

/// The control door of a node, as the TCP surface calls it: one cloneable value a thread can own.
#[derive(Clone)]
struct Door {
    sink: CommandSink,
    cell: Arc<ArcSwap<CoreSnapshot>>,
    limits: Arc<Mutex<ControlLimits>>,
    facts: Arc<dyn SymbolFacts>,
}

impl Door {
    /// One command through the ONE acceptance path, judged against the engine blocks the core published.
    fn send(&self, cmd: WireCommand) -> Result<String, String> {
        let blocks = self.cell.load().portfolio.venues.clone();
        self.send_with_blocks(cmd, &blocks)
    }

    fn send_with_blocks(&self, cmd: WireCommand, blocks: &[VenueBlock]) -> Result<String, String> {
        let mut limits = lock(&self.limits);
        accept_command(
            cmd,
            None,
            &mut limits,
            &self.sink,
            None,
            &[],
            &[],
            &[],
            blocks,
            &[],
            Some(&*self.facts),
            None,
            None,
        )
        .map(Accepted::into_coid)
        .map_err(|e| e.message())
    }
}

/// The engine as a mount builds it for a `demo` binance account: its tier, the mounted symbol's coarse
/// grid, and fail-closed on a symbol it was never told about.
fn mounted_engine(engine: &mut ExecutionEngine<Box<dyn ExecutionClient + Send>>) {
    engine.tier = AccountTier::Demo;
    engine.gate.limits.tick_size = Some(0.1);
    engine.gate.limits.lot_size = Some(0.001);
    engine.gate.limits.deny_unadmitted_symbols = admitting_venues().iter().any(|v| v == VENUE);
}

/// A balance report: the core publishes its engine blocks (the roster the door judges from) once something
/// happens to it, and the balance clears every margin check below.
fn publish(core: &CoreHandle) {
    let report = Event::AccountState(AccountState {
        venue: VENUE.into(),
        balances: vec![("USDT".to_string(), 1_000_000.0)],
        ts: 1,
        route_key: None,
    });
    core.event_sender().blocking_send(report).expect("the core is alive");
    let cell = core.snapshot_cell();
    assert!(
        wait_until(WAIT, || !cell.load().portfolio.venues.is_empty()),
        "the engine published its block"
    );
}

fn plain_core() -> CoreConfig {
    CoreConfig { seed_cash: 10_000.0, ..CoreConfig::default() }
}

/// Everything a tradehub is on the hop, plus its door. Fields drop in declaration order: the price feeds
/// first, then the core, then the backend (the lease).
struct Node {
    feeds: Option<LiveFeeds>,
    rig: CoreRig,
    /// Held for the lease it keeps open.
    _backend: RemoteBackend,
    _triggers: Receiver<()>,
    door: Door,
    facts: Arc<LeaseFacts>,
    /// What the boot re-admission did (`None` on a fresh start).
    booted: Option<Readmitted>,
}

impl Node {
    /// A fresh node over `hop`: no journal, no restore.
    fn start(hop: &Hop, prices: Prices<'_>) -> Node {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = format!("any-symbol-node-{}", NEXT.fetch_add(1, Ordering::SeqCst));
        Node::boot(hop.link(), &id, plain_core(), BootHints::none(), prices, &[])
    }

    /// The tradehub's boot, in `live_runtime`'s order: the lease and the adapter (`start_adapter`), the core
    /// over its restore, the price feeds, THEN `bind_prices`, the re-admission of `restored` (a fresh
    /// `AdmitSymbol` each, on the core's command lane) and the price subscription of each, all before
    /// anything a reconcile driver would send. A command the driver would send is stood in for by a
    /// `SetTradingState(Active)`, so the journal shows what was in front of it.
    fn boot(
        link: GatewayLink,
        client_id: &str,
        cfg: CoreConfig,
        hints: BootHints,
        prices: Prices<'_>,
        restored: &[Target],
    ) -> Node {
        Node::boot_waiting(link, client_id, cfg, hints, prices, restored, Duration::from_secs(5))
    }

    /// [`Node::boot`] with `config.gatewayhub_connect_wait_ms` = `wait`: the bound of the adapter start and
    /// of every gatewayhub round trip of the boot re-admission.
    fn boot_waiting(
        link: GatewayLink,
        client_id: &str,
        cfg: CoreConfig,
        hints: BootHints,
        prices: Prices<'_>,
        restored: &[Target],
        wait: Duration,
    ) -> Node {
        let cell: SnapshotCell = Arc::default();
        let backend = RemoteBackend::connect(link, accounts(), client_id.to_string(), wait)
            .with_timings(fast_trade_timings())
            .with_boot(hints)
            .with_snapshot_cell(Arc::clone(&cell));
        let (events, source) = live_lane();
        let (trigger, triggers) = channel::<()>();
        let label = AccountLabel::Default;
        let bind = backend
            .start_adapter(start_query(&label), &events, Some(trigger))
            .expect("the adapter starts over the hop");
        let rig = CoreRig::start_tuned(bind.client, source, cfg, mounted_engine);
        assert!(cell.set(rig.core().snapshot_cell()).is_ok(), "the cell is set once");
        publish(rig.core());

        let facts = backend.symbol_facts();
        let mut feeds = None;
        let door_facts: Arc<dyn SymbolFacts> = match prices {
            Prices::Fixed(px) => Arc::new(Fixed { inner: Arc::clone(&facts), px }),
            Prices::Real(hub) => {
                let wired = wire_feeds(hub, rig.core());
                let watcher = wired.price_watcher().expect("a CEX feed hands out a price watcher");
                facts.bind_prices(HashMap::from([(VENUE.to_string(), watcher)]));
                feeds = Some(wired);
                Arc::clone(&facts) as Arc<dyn SymbolFacts>
            }
            Prices::Unmounted(hub) => {
                let ctors = ProdFeedCtors::new(hub.addr.clone(), Some(LANE_KEY.to_vec()));
                let wrap = |sink: Arc<dyn LiveDataSink>| sink;
                let wired = wire_cex_price_feed(&CexVenue::Binance, rig.core(), &wrap, &ctors)
                    .expect("a price-only feed is built without dialling");
                let watcher = wired.price_watcher().expect("a CEX feed hands out a price watcher");
                facts.bind_prices(HashMap::from([(VENUE.to_string(), watcher)]));
                feeds = Some(wired);
                Arc::clone(&facts) as Arc<dyn SymbolFacts>
            }
            Prices::NoFeed => {
                facts.bind_prices(HashMap::new());
                Arc::clone(&facts) as Arc<dyn SymbolFacts>
            }
        };
        let booted = (!restored.is_empty()).then(|| {
            // As `live_runtime` does: the boot window bounds every gatewayhub round trip below.
            let _boot = facts.begin_boot();
            let done = readmit(restored, &*facts, |c| {
                rig.core().send_command(c);
            });
            for t in &done.admitted {
                facts
                    .price(&t.route_key, &t.venue, &t.symbol, true)
                    .expect("the re-admitted symbol's price feed opens");
            }
            rig.core().send_command(Command::SetTradingState(TradingState::Active));
            done
        });
        let door = Door {
            sink: rig.core().command_sink(),
            cell: rig.core().snapshot_cell(),
            limits: Arc::new(Mutex::new(ControlLimits::new(ControlLimitsConfig {
                max_notional: None,
                rate_per_sec: 1e9,
            }))),
            facts: door_facts,
        };
        Node { feeds, rig, _backend: backend, _triggers: triggers, door, facts, booted }
    }

    /// The engine blocks the core has published.
    fn blocks(&self) -> Vec<VenueBlock> {
        self.door.cell.load().portfolio.venues.clone()
    }

    /// Whether the engine trades `symbol` now (a `AdmitSymbol` folded).
    fn trades(&self, symbol: &str) -> bool {
        self.blocks().first().is_some_and(|b| b.trades(symbol))
    }

    /// Stop the way the daemon does: feeds first, then the core (its journal is complete after this).
    fn stop(mut self) {
        drop(self.feeds.take());
        self.rig.shutdown();
    }
}

fn limit(coid: &str, symbol: &str, qty: f64, px: f64) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(px),
        trigger_price: None,
        reduce_only: false,
        account: None,
    })
}

/// The order `coid` as the VENUE was handed it.
fn venue_saw(world: &Arc<World>, coid: &str) -> OrderRequest {
    assert!(
        wait_until(WAIT, || world.seen_submits().iter().any(|c| c == coid)),
        "the venue never saw {coid}: {:?}",
        world.seen_submits()
    );
    world
        .submitted_orders()
        .into_iter()
        .find(|o| o.client_order_id == coid)
        .expect("a submit is recorded whole")
}

fn catalog_hop(world: &Arc<World>, catalog: &Arc<Catalog>) -> Hop {
    Hop::start_with_catalog(Arc::clone(world), arming(false), Some(catalog.list()))
}

fn eth_pairs(n: usize) -> Vec<(String, String)> {
    vec![(VENUE.to_string(), ETH.to_string()); n]
}

// ---- (1) a foreign symbol over the real facts verb -----------------------------------------------------------------

/// **A foreign symbol of an admitting venue goes through the REAL facts verb over the REAL lease**: the
/// gatewayhub answers from its (fake) catalog, the node admits the symbol with THAT symbol's grid and places
/// the order on it, the venue is handed the order on `ETHUSDT` snapped on ETH's own tick and lot (not the
/// mounted `BTCUSDT`'s coarse grid), the fill folds into ETH's position, and a second order asks nothing.
#[test]
fn a_foreign_symbol_is_resolved_over_the_real_lease_and_placed_on_its_own_grid() {
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Fixed(3000.0));
    world.fate("c1", Fate::Fill);

    // 3000.456 @ 0.12346 is off ETH's grid (0.01 / 0.0001) and off the mounted one (0.1 / 0.001).
    let acked = node.door.send(limit("c1", "ethusdt", 0.12346, 3000.456)).expect("accepted");
    assert_eq!(acked, "c1");
    assert_eq!(
        hop.lane.facts_asked(),
        eth_pairs(2),
        "the facts verb ran at the gatewayhub, once, in the venue's spelling"
    );

    let seen = venue_saw(&world, "c1");
    assert_eq!(seen.symbol, ETH, "signed on the symbol the operator named, canonical");
    let px = seen.price.expect("a limit price");
    assert!((px - snapped(3000.456, 0.01)).abs() < 1e-6, "ETH's tick, not the mount's: {seen:?}");
    assert!((seen.qty - snapped(0.12346, 0.0001)).abs() < 1e-9, "ETH's lot: {seen:?}");

    assert!(node.rig.wait_status("c1", OrderStatus::Filled, WAIT), "{:?}", node.rig.orders());
    assert!(node.trades(ETH), "the engine's published block says it trades ETHUSDT now");
    let snap = node.rig.core().snapshot();
    let sizes: Vec<(&str, bool)> =
        snap.positions.iter().map(|p| (p.symbol.as_str(), p.size > 0.0)).collect();
    assert_eq!(sizes, [(ETH, true)], "the fill is ETH's position, never the mounted BTC's");

    // A second order on the admitted symbol asks the gatewayhub nothing and keeps the grid.
    world.fate("c2", Fate::Fill);
    node.door.send(limit("c2", ETH, 0.5, 2999.994)).expect("second order");
    let second = venue_saw(&world, "c2");
    assert!((second.price.expect("price") - snapped(2999.994, 0.01)).abs() < 1e-6, "{second:?}");
    assert_eq!(hop.lane.facts_asked().len(), 2, "one lookup and its commit served two orders");

    // The mounted symbol keeps ITS grid beside it.
    node.door.send(limit("c3", SYMBOL, 1.0, 60_000.04)).expect("mounted order");
    let own = venue_saw(&world, "c3");
    assert!((own.price.expect("price") - 60_000.0).abs() < 1e-6, "mounted tick 0.1: {own:?}");
    assert_eq!(hop.lane.facts_asked().len(), 2, "the mounted symbol is not looked up");
}

// ---- (2) an unknown symbol, refused with its cause ---------------------------------------------------------------

/// **An unknown symbol is refused with the gatewayhub's own cause, over the real hop**; nothing reaches the
/// venue, the engine admits nothing, and the node is exactly as it was (a mounted-symbol order still goes).
#[test]
fn an_unknown_symbol_is_refused_with_its_cause_over_the_real_hop() {
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Fixed(3000.0));

    let unknown = node
        .door
        .send(limit("u1", "nosuchusdt", 1.0, 5.0))
        .expect_err("a symbol the venue does not list is refused");
    assert!(unknown.contains("cannot admit `NOSUCHUSDT`"), "{unknown}");
    assert!(
        unknown.contains(
            "binance: no instrument NOSUCHUSDT in the venue's instrument list (spot and USDS-M \
             perpetuals)"
        ),
        "the gatewayhub's own sentence crossed the hop: {unknown}"
    );
    assert!(unknown.contains("REFUSED"), "{unknown}");

    let no_tick = node
        .door
        .send(limit("u2", "notickusdt", 1.0, 5.0))
        .expect_err("a symbol with no published tick is refused");
    assert!(no_tick.contains("cannot admit `NOTICKUSDT`"), "{no_tick}");
    assert!(no_tick.contains("carries no tick size"), "{no_tick}");

    // The other lane is refused at the door, by name, before the hop is asked.
    let lane = node.door.send(limit("u3", "ethusdt.p", 1.0, 5.0)).expect_err("the perp lane");
    assert!(lane.contains("`ETHUSDT.P` is a perp symbol") && lane.contains("spot lane"), "{lane}");

    assert_eq!(
        hop.lane.facts_asked(),
        [
            (VENUE.to_string(), "NOSUCHUSDT".to_string()),
            (VENUE.to_string(), "NOTICKUSDT".to_string())
        ],
        "the lane refusal never reached the gatewayhub"
    );
    assert!(world.seen_submits().is_empty(), "no refused order reached the venue");
    for symbol in ["NOSUCHUSDT", "NOTICKUSDT", "ETHUSDT.P"] {
        assert!(!node.trades(symbol), "{symbol} was admitted by a refusal");
    }

    node.door.send(limit("m1", SYMBOL, 1.0, 60_000.04)).expect("the mounted symbol still trades");
    let own = venue_saw(&world, "m1");
    assert_eq!(own.symbol, SYMBOL);
    assert!((own.price.expect("price") - 60_000.0).abs() < 1e-6, "{own:?}");
}

// ---- (3) a gatewayhub that predates the verb ----------------------------------------------------------------------

/// **A gatewayhub that does not advertise `gw_trade_symbol_facts` makes the door refuse with the "update it"
/// text**, and sends nothing: no facts frame, no order. The rest of the trade class still works, so the
/// account runs and the mounted symbol trades. (A scripted wire peer: the real server always advertises the
/// verb; the refusal is the client's, decided from the advertisement.)
#[test]
fn a_gatewayhub_without_the_symbol_facts_verb_makes_the_door_say_update_it() {
    let mut config = fake_trade_peer::TradePeerConfig::new();
    config.features.retain(|f| f != FEATURE_GW_TRADE_SYMBOL_FACTS);
    let peer = fake_trade_peer::TradePeer::start(config);
    let node = Node::boot(
        peer.link(),
        "any-symbol-old-daemon",
        plain_core(),
        BootHints::none(),
        Prices::Fixed(3000.0),
        &[],
    );

    let refused = node
        .door
        .send(limit("o1", ETH, 1.0, 3000.0))
        .expect_err("an older gatewayhub cannot say what ETHUSDT is");
    assert!(refused.contains("cannot admit `ETHUSDT`"), "{refused}");
    assert!(
        refused.contains("the gatewayhub does not advertise symbol facts (update it)"),
        "{refused}"
    );
    assert!(refused.contains("REFUSED"), "{refused}");
    let direct = node.facts.admission(VENUE, VENUE, ETH).expect_err("and neither does the source");
    assert!(direct.contains("(update it)"), "{direct}");

    let sent = peer.ups();
    assert!(
        !sent.iter().any(|f| matches!(f, TradeUp::SymbolFacts { .. })),
        "no facts frame went to a daemon that does not serve it: {sent:?}"
    );
    assert!(!sent.iter().any(|f| matches!(f, TradeUp::Submit { .. })), "and no order: {sent:?}");
    assert!(!node.trades(ETH), "nothing was admitted");

    // The older daemon still trades the mounted symbol.
    node.door.send(limit("o2", SYMBOL, 1.0, 60_000.0)).expect("the mounted symbol");
    assert!(
        wait_until(WAIT, || peer.ups().iter().any(|f| matches!(f, TradeUp::Submit { .. }))),
        "the mounted order reached the daemon"
    );
}

// ---- (4) the price path ----------------------------------------------------------------------------------------

/// **The admitted symbol's price path is real end to end.** The door's first committing ask subscribes the
/// symbol's tick pump through the datahub, on the TICK the facts carried over the lease (ETH's 0.01, not the
/// mount's 0.1); the pump's quote comes back through the real `StratFeed` into the node's price tap; the order
/// held for that first price goes through; and a second order opens no second subscription.
#[test]
fn the_admitted_symbols_price_is_subscribed_on_its_own_tick_and_reaches_the_tap() {
    let hub = PriceHub::start();
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Real(&hub));
    assert!(
        wait_until(WAIT, || hub.count("ticks BTCUSDT 0.1") == 1),
        "the mount's own pump: {:?}",
        hub.asked()
    );
    assert_eq!(
        node.facts.price(VENUE, VENUE, ETH, false),
        Ok(None),
        "a read opens nothing and ETH has no price before anyone asked"
    );
    assert_eq!(hub.count("ticks ETHUSDT 0.01"), 0);

    world.fate("p1", Fate::Fill);
    node.door
        .send(limit("p1", "ethusdt", 0.12346, 3000.456))
        .expect("held for the first price, then accepted");
    assert_eq!(
        hub.count("ticks ETHUSDT 0.01"),
        1,
        "subscribed on the facts' tick: {:?}",
        hub.asked()
    );
    assert_eq!(
        node.facts.price(VENUE, VENUE, ETH, false),
        Ok(Some(3000.5)),
        "the pump's quote reached the node's price tap"
    );
    let seen = venue_saw(&world, "p1");
    assert_eq!(seen.symbol, ETH);
    assert!((seen.price.expect("price") - snapped(3000.456, 0.01)).abs() < 1e-6, "{seen:?}");
    assert!(node.rig.wait_status("p1", OrderStatus::Filled, WAIT), "{:?}", node.rig.orders());

    world.fate("p2", Fate::Fill);
    node.door.send(limit("p2", ETH, 0.5, 3000.0)).expect("second order");
    venue_saw(&world, "p2");
    assert_eq!(hub.count("ticks ETHUSDT 0.01"), 1, "no second subscription: {:?}", hub.asked());
    assert!(!hub.asked().iter().any(|l| l.starts_with("unsub")), "{:?}", hub.asked());
}

/// **A venue no mount names still prices a foreign symbol** (the CI box, 2026-10-17: only bybit was mounted, so
/// binance and okx answered `this node runs no price feed for <venue>` to every foreign symbol and the
/// any-symbol drill was refused there). The venue's price-only feed subscribes NOTHING at boot (not even
/// the mounted symbol's pump), and the first order on ETH opens ETH's pump on the facts' tick and goes
/// through on its price.
#[test]
fn a_venue_no_mount_names_prices_a_foreign_symbol_through_its_price_only_feed() {
    let hub = PriceHub::start();
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Unmounted(&hub));
    assert!(hub.asked().is_empty(), "no subscription at boot: {:?}", hub.asked());

    world.fate("u1", Fate::Fill);
    node.door
        .send(limit("u1", "ethusdt", 0.12346, 3000.456))
        .expect("priced by the price-only feed, then accepted");
    assert_eq!(hub.count("ticks ETHUSDT 0.01"), 1, "{:?}", hub.asked());
    assert_eq!(hub.count("ticks BTCUSDT 0.1"), 0, "{:?}", hub.asked());
    assert_eq!(venue_saw(&world, "u1").symbol, ETH);
    assert!(node.rig.wait_status("u1", OrderStatus::Filled, WAIT), "{:?}", node.rig.orders());
    assert!(node.trades(ETH));
}

/// **With no price feed for the venue the door fails CLOSED, in the words the CI box showed**: the symbol is not
/// admitted and nothing reaches the venue.
#[test]
fn a_venue_with_no_price_feed_refuses_a_foreign_symbol_and_sends_nothing() {
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::NoFeed);

    let refused = node
        .door
        .send(limit("n1", ETH, 0.5, 3000.0))
        .expect_err("no price feed, no foreign symbol");
    assert!(
        refused.contains("no price feed for `ETHUSDT`: this node runs no price feed for binance"),
        "{refused}"
    );
    assert!(!node.trades(ETH), "never admitted");
    assert!(world.seen_submits().is_empty(), "nothing was sent");
}

/// **`live_runtime`'s registration, over a real datahub.** The price-only feeds cover exactly the admitting
/// venues that no mount names (bybit is mounted here, so it is left to its mount's feed), each watcher is
/// registered under its venue, a venue the datahub's strategy lane does not advertise is skipped by the probe
/// (aster here: nothing is dialled for it), and Hyperliquid is wired only when a network was decided for it.
#[test]
fn the_price_only_feeds_cover_the_admitting_venues_no_mount_names_and_the_probe_skips_the_rest() {
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::NoFeed);
    let wrap = |sink: Arc<dyn LiveDataSink>| sink;
    let admitting = admitting_venues();
    let sorted = |wired: &vike_tradehub::feeds::PriceOnlyFeeds| {
        let mut keys: Vec<String> = wired.watchers.keys().cloned().collect();
        keys.sort();
        keys
    };

    let hub = PriceHub::start_serving(&["binance", "bybit", "okx", "hyperliquid"]);
    let ctors = ProdFeedCtors::new(hub.addr.clone(), Some(LANE_KEY.to_vec()));
    let mounted = vec!["bybit".to_string()];
    let wired = wire_price_only_feeds(
        &admitting,
        &mounted,
        Some(HlNetwork::Mainnet),
        node.rig.core(),
        &wrap,
        &ctors,
    );
    assert_eq!(
        sorted(&wired),
        ["binance", "hyperliquid", "okx"],
        "bybit is mounted, aster is skipped"
    );
    assert_eq!(wired.feeds.len(), 3);
    assert_eq!(wired.hl_network, Some(HlNetwork::Mainnet));
    assert!(hub.asked().is_empty(), "nothing is subscribed: {:?}", hub.asked());
    drop(wired);

    // Two mounts (bybit and binance), no network decided for Hyperliquid, everything advertised.
    let hub = PriceHub::start_serving(&["binance", "bybit", "okx", "aster", "hyperliquid"]);
    let ctors = ProdFeedCtors::new(hub.addr.clone(), Some(LANE_KEY.to_vec()));
    let mounted = vec!["bybit".to_string(), "binance".to_string()];
    let wired = wire_price_only_feeds(&admitting, &mounted, None, node.rig.core(), &wrap, &ctors);
    assert_eq!(
        sorted(&wired),
        ["aster", "okx"],
        "a mounted venue and an undecided Hyperliquid are left out"
    );
    assert_eq!(wired.hl_network, None);
    drop(wired);
}

/// Records the dial Hyperliquid's price-only feed asks for and every subscription made on it.
#[derive(Default)]
struct HlCtors {
    dials: Mutex<Vec<(String, Option<String>)>>,
    log: Arc<Mutex<Vec<String>>>,
}

struct QuoteLog {
    log: Arc<Mutex<Vec<String>>>,
    next: u64,
}

impl QuoteLog {
    fn id(&mut self, what: String) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        self.next += 1;
        lock(&self.log).push(what);
        Ok(vike_data::SubscriptionId(self.next))
    }
}

impl DataClient for QuoteLog {
    fn subscribe_bars(
        &mut self,
        s: &str,
        i: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        self.id(format!("bars {s}@{i}"))
    }
    fn subscribe_quotes(
        &mut self,
        s: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        self.id(format!("quotes {s}"))
    }
    fn subscribe_trades(
        &mut self,
        s: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        self.id(format!("trades {s}"))
    }
    fn subscribe_book(
        &mut self,
        s: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        self.id(format!("book {s}"))
    }
    fn unsubscribe(&mut self, _id: vike_data::SubscriptionId) {}
    fn shutdown(&mut self) {}
}

impl FeedCtors for HlCtors {
    fn strat(
        &self,
        venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<vike_datahub_client::strat::StratFeed, String> {
        Err(format!("{venue}: hyperliquid dials client(), not strat()"))
    }

    fn client(
        &self,
        venue: &str,
        network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        lock(&self.dials).push((venue.to_string(), network.map(str::to_string)));
        Ok(Box::new(QuoteLog { log: Arc::clone(&self.log), next: 0 }))
    }
}

/// **Hyperliquid's price-only feed dials the network it is given and subscribes nothing until the door
/// asks.** the CI box's two Hyperliquid route keys are `hyperliquid` (a `demo` account: the test network) and
/// `hyperliquid#LIVE` (a `live` one: mainnet); `HlNetwork::for_tier` is the tier mapping the node's choice
/// (`feed_tier`, mainnet when a live account is armed) goes through, and the dial NAMES it
/// (`StratOpen{network}`: none for mainnet, `testnet` for the test network), so a test-network order is
/// never priced off the main book by a feed that was built for the other.
#[test]
fn hyperliquids_price_only_feed_dials_its_network_and_subscribes_a_coin_only_when_asked() {
    assert_eq!(HlNetwork::for_tier(AccountTier::Live), HlNetwork::Mainnet, "hyperliquid#LIVE");
    assert_eq!(HlNetwork::for_tier(AccountTier::Demo), HlNetwork::Testnet, "hyperliquid");
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::NoFeed);
    let wrap = |sink: Arc<dyn LiveDataSink>| sink;
    for (network, dialed) in [
        (HlNetwork::Mainnet, None),
        (HlNetwork::Testnet, Some(vike_datahub_client::proto::STRAT_NETWORK_TESTNET.to_string())),
    ] {
        let ctors = HlCtors::default();
        let feed = wire_hl_price_feed(&network, node.rig.core(), &wrap, &ctors)
            .expect("a price-only feed is built without subscribing");
        assert_eq!(*lock(&ctors.dials), vec![("hyperliquid".to_string(), dialed)], "{network:?}");
        assert!(lock(&ctors.log).is_empty(), "nothing subscribed at wiring");
        let watcher = feed.price_watcher().expect("a Hyperliquid feed hands out a price watcher");
        watcher.subscribe_ticks("SOL", 0.01).expect("the coin's quote lane opens on demand");
        assert_eq!(*lock(&ctors.log), vec!["quotes SOL".to_string()], "{network:?}");
        drop(feed);
    }
}

/// **A symbol the door gives up on (no price within the wait) leaves its pump open for the idle TTL** (the
/// operator's retry finds it; `an_idle_pump_of_a_refused_symbol_is_closed_and_an_admitted_symbols_never`
/// is the close), and is never admitted or sent; once the pump ticks again the same order goes through on
/// the SAME subscription.
#[test]
fn a_symbol_with_no_price_is_refused_and_its_pump_stays_open_for_the_retry() {
    let hub = PriceHub::start();
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Real(&hub));
    hub.mute(SOL);

    let refused = node
        .door
        .send(limit("s1", SOL, 1.0, 150.0))
        .expect_err("a symbol with no price is refused after the wait");
    assert!(refused.contains("`SOLUSDT` has no price on this node yet"), "{refused}");
    assert!(refused.contains("REFUSED"), "{refused}");
    assert_eq!(hub.count("ticks SOLUSDT 0.001"), 1, "{:?}", hub.asked());
    assert_eq!(hub.count("unsub SOLUSDT"), 0, "the pump stays open: {:?}", hub.asked());
    assert!(!node.trades(SOL), "never admitted");
    assert!(world.seen_submits().is_empty(), "nothing was sent");

    hub.unmute(SOL);
    node.door.send(limit("s2", SOL, 1.0, 150.0)).expect("priced now");
    venue_saw(&world, "s2");
    assert_eq!(hub.count("ticks SOLUSDT 0.001"), 1, "the same subscription: {:?}", hub.asked());
    assert!(node.trades(SOL));
}

/// **An order that cannot be lowered costs no price subscription** (it was a BUG: the plan opened the pump
/// first and only the price wait and a refused `AdmitSymbol` gave the entry back, so a bad `order_type` or
/// an empty coid kept an entry in `LeaseFacts` watched set for good and `MAX_WATCHED` of them refused every
/// new symbol). `accept_command` now lowers the order once BEFORE the plan, and `PriceGuard` covers every
/// later `?`. The good order that follows opens the lane once, with no close.
#[test]
fn a_frame_that_cannot_be_lowered_costs_no_subscription_and_the_good_order_after_it_opens_one() {
    let hub = PriceHub::start();
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Real(&hub));

    let WireCommand::Submit(mut req) = limit("b1", SOL, 1.0, 150.0) else { unreachable!() };
    req.order_type = "teleport".into();
    let refused = node.door.send(WireCommand::Submit(req)).expect_err("a bad order type");
    assert!(refused.contains("order_type"), "{refused}");
    assert_eq!(hub.count("ticks SOLUSDT 0.001"), 0, "refused before the plan could subscribe");
    assert!(!node.trades(SOL) && world.seen_submits().is_empty());

    node.door.send(limit("b2", SOL, 1.0, 150.0)).expect("the same symbol, a good order");
    venue_saw(&world, "b2");
    assert_eq!(
        hub.count("ticks SOLUSDT 0.001"),
        1,
        "the good order opened the lane: {:?}",
        hub.asked()
    );
    assert!(!hub.asked().iter().any(|l| l.starts_with("unsub")), "{:?}", hub.asked());
    assert!(node.trades(SOL));
}

/// **The pump of a symbol the door gave up on is closed once it has been idle for the TTL; the pump of an
/// admitted symbol, and the mount's own, never are.** Until then the pump stays (the previous test's retry),
/// and a later order on the closed symbol subscribes afresh and goes through.
#[test]
fn an_idle_pump_of_a_refused_symbol_is_closed_and_an_admitted_symbols_never() {
    let hub = PriceHub::start();
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Real(&hub));
    let ttl = Duration::from_millis(1500);
    node.facts.set_idle_ttl(ttl);

    world.fate("e1", Fate::Fill);
    node.door.send(limit("e1", ETH, 0.5, 3000.0)).expect("ETH is admitted");
    venue_saw(&world, "e1");
    hub.mute(SOL);
    node.door.send(limit("s1", SOL, 1.0, 150.0)).expect_err("no price within the wait");
    assert_eq!(hub.count("ticks SOLUSDT 0.001"), 1, "{:?}", hub.asked());

    assert_eq!(node.facts.close_idle_pumps(), 0, "idle for less than the TTL: still open");
    std::thread::sleep(ttl + Duration::from_millis(200));
    assert_eq!(node.facts.close_idle_pumps(), 1, "only the refused symbol's pump is idle");
    assert!(
        wait_until(WAIT, || hub.count("unsub SOLUSDT") == 1),
        "the refused symbol's pump was closed: {:?}",
        hub.asked()
    );
    assert_eq!(hub.count("unsub ETHUSDT"), 0, "an admitted symbol keeps its pump");
    assert_eq!(hub.count("unsub BTCUSDT"), 0, "the mount's own pump is not this map's");
    assert_eq!(node.facts.close_idle_pumps(), 0, "nothing is left to close");

    hub.unmute(SOL);
    node.door.send(limit("s2", SOL, 1.0, 150.0)).expect("priced now, on a fresh subscription");
    venue_saw(&world, "s2");
    assert_eq!(hub.count("ticks SOLUSDT 0.001"), 2, "subscribed afresh: {:?}", hub.asked());
    assert!(node.trades(SOL));
    assert_eq!(hub.count("unsub ETHUSDT"), 0);
}

/// **The bound is on OPEN pumps.** Thirty-two symbols in use fill the set and the next is refused; once the
/// door has let go of them (released, never admitted) each newcomer closes the oldest idle pump instead of
/// refusing, so across a stream of refused symbols the pump never holds more than the bound open.
#[test]
fn the_open_pumps_of_symbols_nobody_admitted_stay_within_the_bound() {
    let hub = PriceHub::start();
    let world = World::new();
    let mut listed = rows(grid(0.01, 0.0001));
    let extra = MAX_WATCHED + 8;
    listed.extend((0..extra).map(|i| spot(&format!("X{i}USDT"), grid(0.01, 0.001))));
    let catalog = Catalog::new(listed);
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Real(&hub));
    let name = |i: usize| format!("X{i}USDT");
    let count = |prefix: &str| hub.asked().iter().filter(|l| l.starts_with(prefix)).count();

    for i in 0..MAX_WATCHED {
        node.facts.price(VENUE, VENUE, &name(i), true).expect("a place is free");
    }
    let refused = node.facts.price(VENUE, VENUE, &name(MAX_WATCHED), true).expect_err("full");
    assert!(refused.contains("price subscriptions"), "{refused}");

    for i in 0..MAX_WATCHED {
        node.facts.release(VENUE, VENUE, &name(i));
    }
    for i in MAX_WATCHED..extra {
        node.facts
            .price(VENUE, VENUE, &name(i), true)
            .expect("the oldest idle pump gives its place");
        node.facts.release(VENUE, VENUE, &name(i));
    }
    let evicted = extra - MAX_WATCHED;
    assert!(
        wait_until(WAIT, || count("unsub X") == evicted),
        "one pump closed per newcomer: {:?}",
        hub.asked()
    );
    assert_eq!(count("ticks X") - count("unsub X"), MAX_WATCHED, "open pumps stay at the bound");
    assert_eq!(hub.count("unsub X0USDT"), 1, "the oldest idle pump went first");
}

// ---- (5) a node restart ------------------------------------------------------------------------------------------

fn admissions(dir: &Path) -> Vec<(usize, SymbolAdmission)> {
    CommandJournal::read_all(dir)
        .expect("the journal reads")
        .iter()
        .enumerate()
        .filter_map(|(at, rec)| match rec {
            JournalRecord::Cmd { msg: Ingest::Command(Command::AdmitSymbol(a)), .. } => {
                Some((at, (**a).clone()))
            }
            _ => None,
        })
        .collect()
}

/// **A restarted node re-admits the symbol with FRESH facts over the real hop and re-subscribes its price,
/// in front of anything a reconcile driver would send.** Run 1 admits ETHUSDT on tick 0.01, fills one order
/// and rests another. The venue then changes ETH's tick to 0.05 (and the lot), and the node restarts over the
/// same journal: the restore carries the OLD grid; the boot (`Node::boot`, `live_runtime`'s order) asks the
/// gatewayhub again, journals a refreshing `AdmitSymbol` carrying the NEW grid, subscribes the pump on the new
/// tick, and only then sends the next command. The next order is snapped on the new grid.
#[test]
fn a_restarted_node_readmits_with_fresh_facts_and_resubscribes_its_price_first() {
    let hub = PriceHub::start();
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let dir = tempfile::tempdir().expect("a journal dir");
    let journal = JournalConfig {
        dir: dir.path().join("journal"),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 5,
    };
    let journaled = || CoreConfig { journal: Some(journal.clone()), ..plain_core() };

    // ---- run 1
    let first = Node::boot(
        hop.link(),
        "any-symbol-run-1",
        journaled(),
        BootHints::none(),
        Prices::Real(&hub),
        &[],
    );
    world.fate("r1", Fate::Fill);
    first.door.send(limit("r1", ETH, 0.12346, 3000.456)).expect("r1");
    assert!(first.rig.wait_status("r1", OrderStatus::Filled, WAIT), "{:?}", first.rig.orders());
    first.door.send(limit("r2", ETH, 0.5, 2900.0)).expect("r2 rests");
    assert!(first.rig.wait_status("r2", OrderStatus::Accepted, WAIT), "{:?}", first.rig.orders());
    assert_eq!(hub.count("ticks ETHUSDT 0.01"), 1);
    first.stop();

    // The venue changes ETH's tick and lot while the node is down.
    catalog.set(rows(grid(0.05, 0.001)));

    // ---- the boot restore, as live_runtime takes it
    let scratch = tempfile::tempdir().expect("restore scratch");
    let now_ms = vike_model::now_ms();
    let restore =
        restore_for_boot(Some(&journal), scratch.path(), now_ms).expect("the journal reads");
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let wanted = {
        let engines = restore.restored_engines();
        let old = engines[0].limits.grid_by_symbol.get(ETH).expect("the restore holds ETH's row");
        assert_eq!(old.tick_size, Some(0.01), "the journal restores the OLD facts");
        targets(engines)
    };
    assert_eq!(
        wanted,
        [Target { route_key: VENUE.into(), venue: VENUE.into(), symbol: ETH.into() }],
        "the restored positions and orders name ETHUSDT as a symbol to re-admit"
    );
    let hints = BootHints::from_restore(&restore, None, now_ms);
    let mut cfg = journaled();
    restore.apply_to(&mut cfg);

    // ---- run 2
    let second =
        Node::boot(hop.link(), "any-symbol-run-2", cfg, hints, Prices::Real(&hub), &wanted);
    let done = second.booted.as_ref().expect("the boot re-admission ran");
    assert_eq!((done.sent, done.failed.len()), (1, 0), "{done:?}");
    assert_eq!(
        hop.lane.facts_asked(),
        eth_pairs(4),
        "the facts were asked again over the new lease"
    );
    assert_eq!(
        hub.count("ticks ETHUSDT 0.05"),
        1,
        "re-subscribed on the NEW tick: {:?}",
        hub.asked()
    );
    assert_eq!(
        hub.count("ticks ETHUSDT 0.01"),
        1,
        "the old run's subscription is the only old one"
    );
    assert!(wait_until(WAIT, || second.trades(ETH)), "the restored engine trades ETHUSDT again");
    assert!(
        wait_until(WAIT, || {
            second.rig.core().snapshot().positions.iter().any(|p| p.symbol == ETH && p.size > 0.0)
        }),
        "the restored position is ETH's: {:?}",
        second.rig.core().snapshot().positions
    );
    assert!(
        wait_until(WAIT, || second.facts.price(VENUE, VENUE, ETH, false) == Ok(Some(3000.5))),
        "the re-subscribed pump ticks into the new node's tap: {:?}",
        second.facts.price(VENUE, VENUE, ETH, false)
    );

    // The next order is snapped on the FRESH grid, with no further lookup.
    world.fate("r3", Fate::Fill);
    second.door.send(limit("r3", ETH, 0.12346, 3000.456)).expect("r3");
    let seen = venue_saw(&world, "r3");
    assert!((seen.price.expect("price") - snapped(3000.456, 0.05)).abs() < 1e-6, "{seen:?}");
    assert!((seen.qty - snapped(0.12346, 0.001)).abs() < 1e-9, "{seen:?}");
    assert!(
        (seen.price.expect("price") - snapped(3000.456, 0.01)).abs() > 1e-6,
        "the old grid would have given a different price: {seen:?}"
    );
    assert_eq!(hop.lane.facts_asked().len(), 4, "no lookup beyond the boot's");
    second.stop();

    // ---- the journal: the refresh carries the new grid and sits in front of the next command
    let journaled_admissions = admissions(&journal.dir);
    let ticks: Vec<Option<f64>> =
        journaled_admissions.iter().map(|(_, a)| a.grid.tick_size).collect();
    assert_eq!(ticks, [Some(0.01), Some(0.05)], "run 1's admission, then the boot's refresh");
    let refresh_at = journaled_admissions[1].0;
    let next_command_at = CommandJournal::read_all(&journal.dir)
        .expect("the journal reads")
        .iter()
        .position(|rec| {
            matches!(
                rec,
                JournalRecord::Cmd { msg: Ingest::Command(Command::SetTradingState(_)), .. }
            )
        })
        .expect("the stand-in for the reconcile driver's first command was journaled");
    assert!(
        refresh_at < next_command_at,
        "the re-admission folded ahead of the next command: {refresh_at} vs {next_command_at}"
    );
}

/// What run 2 boots from after a run that ADMITTED `ETHUSDT` and ended FLAT: the venue rejected both orders
/// on it, so the journal holds no position and no live order in it, only the admission.
struct FlatAdmitted {
    cfg: CoreConfig,
    hints: BootHints,
    wanted: Vec<Target>,
    _dir: tempfile::TempDir,
}

fn flat_admitted_restart(hub: &PriceHub, world: &Arc<World>, hop: &Hop) -> FlatAdmitted {
    let dir = tempfile::tempdir().expect("a journal dir");
    let journal = JournalConfig {
        dir: dir.path().join("journal"),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 5,
    };
    let journaled = || CoreConfig { journal: Some(journal.clone()), ..plain_core() };
    let first = Node::boot(
        hop.link(),
        "any-symbol-flat-1",
        journaled(),
        BootHints::none(),
        Prices::Real(hub),
        &[],
    );
    for coid in ["f1", "f2"] {
        world.fate(coid, Fate::Reject);
        first.door.send(limit(coid, ETH, 0.5, 3000.0)).expect("the door accepts it");
        assert!(
            first.rig.wait_status(coid, OrderStatus::Rejected, WAIT),
            "{:?}",
            first.rig.orders()
        );
    }
    assert!(first.trades(ETH), "ETHUSDT was admitted");
    first.stop();

    let scratch = tempfile::tempdir().expect("restore scratch");
    let now_ms = vike_model::now_ms();
    let restore =
        restore_for_boot(Some(&journal), scratch.path(), now_ms).expect("the journal reads");
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let engines = restore.restored_engines();
    assert!(engines[0].extra_symbols.iter().any(|s| s == ETH), "the restore lists ETHUSDT");
    assert!(
        engines[0].account.positions.iter().all(|(_, entry)| entry.size == 0.0),
        "the book is flat"
    );
    let wanted = targets(engines);
    let hints = BootHints::from_restore(&restore, None, now_ms);
    let mut cfg = journaled();
    restore.apply_to(&mut cfg);
    FlatAdmitted { cfg, hints, wanted, _dir: dir }
}

/// **A restored symbol the engine holds NOTHING in is still re-admitted and priced** (v0.1.66 on the CI box,
/// 2026-10-17: bybit#DEMO's ETHUSDT and SOLUSDT came back from the journal as admitted symbols with no mark,
/// `targets` listed only symbols with a position or a live order, so no pump was ever subscribed and every
/// order was denied `no-reference-price`). The mounted venue's feed subscribes only the mount's own symbol,
/// so the pump of the restored one exists only if the boot opened it. Until its first tick the door refuses an
/// opening order with the honest text, and the same pump (no second subscription) lets it through after.
#[test]
fn a_restored_flat_admitted_symbol_is_priced_by_the_boot_and_refused_only_until_its_first_tick() {
    let hub = PriceHub::start();
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let flat = flat_admitted_restart(&hub, &world, &hop);
    assert_eq!(
        flat.wanted,
        [Target { route_key: VENUE.into(), venue: VENUE.into(), symbol: ETH.into() }],
        "an admitted symbol is a target though nothing is held in it"
    );
    assert_eq!(hub.count("ticks ETHUSDT 0.01"), 1, "run 1's own: {:?}", hub.asked());

    hub.mute(ETH);
    let second = Node::boot(
        hop.link(),
        "any-symbol-flat-2",
        flat.cfg,
        flat.hints,
        Prices::Real(&hub),
        &flat.wanted,
    );
    let done = second.booted.as_ref().expect("the boot re-admission ran");
    assert_eq!((done.sent, done.failed.len()), (1, 0), "{done:?}");
    assert_eq!(hub.count("ticks ETHUSDT 0.01"), 2, "the boot opened the pump: {:?}", hub.asked());
    assert!(wait_until(WAIT, || second.trades(ETH)), "the restored engine trades ETHUSDT");

    let refused =
        second.door.send(limit("o1", ETH, 0.5, 3000.0)).expect_err("no tick yet: fail closed");
    assert!(refused.contains("`ETHUSDT` has no price on this node yet"), "{refused}");
    assert!(refused.contains("REFUSED") && refused.contains("retry"), "{refused}");
    assert!(!world.seen_submits().iter().any(|c| c == "o1"), "{:?}", world.seen_submits());

    hub.unmute(ETH);
    world.fate("o2", Fate::Fill);
    second.door.send(limit("o2", ETH, 0.5, 3000.0)).expect("the first tick prices it");
    venue_saw(&world, "o2");
    assert!(second.rig.wait_status("o2", OrderStatus::Filled, WAIT), "{:?}", second.rig.orders());
    assert_eq!(hub.count("ticks ETHUSDT 0.01"), 2, "one pump for both: {:?}", hub.asked());
    second.stop();
}

/// **The door itself re-asks the price lane for a symbol the engine already trades.** The boot never reached
/// this restored symbol (no re-admission here: the case of a boot that missed it, or of a pump that is gone),
/// so its first order must open the pump, be held for the first tick and go through, and not be denied
/// `no-reference-price` by an engine that has no mark.
#[test]
fn an_order_on_a_restored_symbol_the_boot_never_priced_opens_its_pump() {
    let hub = PriceHub::start();
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let flat = flat_admitted_restart(&hub, &world, &hop);
    assert_eq!(hub.count("ticks ETHUSDT 0.01"), 1, "run 1's own: {:?}", hub.asked());

    let second =
        Node::boot(hop.link(), "any-symbol-flat-3", flat.cfg, flat.hints, Prices::Real(&hub), &[]);
    assert!(
        wait_until(WAIT, || second.trades(ETH)),
        "the restore alone keeps ETHUSDT in the engine"
    );
    assert_eq!(hub.count("ticks ETHUSDT 0.01"), 1, "nothing subscribed it at boot");

    world.fate("o1", Fate::Fill);
    second
        .door
        .send(limit("o1", ETH, 0.5, 3000.0))
        .expect("held for the first tick, then accepted");
    venue_saw(&world, "o1");
    assert!(second.rig.wait_status("o1", OrderStatus::Filled, WAIT), "{:?}", second.rig.orders());
    assert_eq!(
        hub.count("ticks ETHUSDT 0.01"),
        2,
        "the order's ask opened the pump: {:?}",
        hub.asked()
    );
    second.stop();
}

/// **A gatewayhub that does not answer cannot hold the boot.** The facts look of the first restored symbol
/// parks inside the gatewayhub; the boot waits the boot bound (`config.gatewayhub_connect_wait_ms`, here one
/// second, not the lease's five), treats the symbol as NOT admitted, and does not ask that lease again for the
/// second one. Nothing is sent or told to the daemon, and the position the journal restored stays in the
/// engine, which still knows the symbol.
#[test]
fn a_silent_gatewayhub_at_boot_admits_nothing_within_the_boot_bound_and_loses_nothing() {
    let hub = PriceHub::start();
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let dir = tempfile::tempdir().expect("a journal dir");
    let journal = JournalConfig {
        dir: dir.path().join("journal"),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 5,
    };
    let journaled = || CoreConfig { journal: Some(journal.clone()), ..plain_core() };

    let first = Node::boot(
        hop.link(),
        "any-symbol-silent-1",
        journaled(),
        BootHints::none(),
        Prices::Real(&hub),
        &[],
    );
    world.fate("q1", Fate::Fill);
    first.door.send(limit("q1", ETH, 0.12346, 3000.456)).expect("q1");
    assert!(first.rig.wait_status("q1", OrderStatus::Filled, WAIT), "{:?}", first.rig.orders());
    first.stop();

    let scratch = tempfile::tempdir().expect("restore scratch");
    let now_ms = vike_model::now_ms();
    let restore =
        restore_for_boot(Some(&journal), scratch.path(), now_ms).expect("the journal reads");
    let mut wanted = targets(restore.restored_engines());
    wanted.push(Target { route_key: VENUE.into(), venue: VENUE.into(), symbol: SOL.into() });
    assert_eq!(wanted.len(), 2, "{wanted:?}");
    let hints = BootHints::from_restore(&restore, None, now_ms);
    let mut cfg = journaled();
    restore.apply_to(&mut cfg);

    let _parked = catalog.hold.arm();
    let asked_before = hop.lane.facts_asked().len();
    let started = Instant::now();
    let second = Node::boot_waiting(
        hop.link(),
        "any-symbol-silent-2",
        cfg,
        hints,
        Prices::Real(&hub),
        &wanted,
        Duration::from_secs(1),
    );
    let took = started.elapsed();
    catalog.hold.release();

    let done = second.booted.as_ref().expect("the boot re-admission ran");
    assert_eq!((done.sent, done.admitted.len(), done.failed.len()), (0, 0, 2), "{done:?}");
    assert!(done.failed[0].2.contains("no answer within"), "{:?}", done.failed);
    assert!(done.failed[1].2.contains("not asked again"), "{:?}", done.failed);
    assert!(
        took < Duration::from_secs(4),
        "the boot bound, not the lease's five seconds: {took:?}"
    );
    assert_eq!(hop.lane.facts_asked().len() - asked_before, 1, "the second symbol was not asked");
    assert!(second.trades(ETH), "the restored engine still knows ETHUSDT");
    assert!(
        wait_until(WAIT, || {
            second.rig.core().snapshot().positions.iter().any(|p| p.symbol == ETH && p.size > 0.0)
        }),
        "the restored position stays visible: {:?}",
        second.rig.core().snapshot().positions
    );
    assert_eq!(hub.count("ticks ETHUSDT 0.01"), 1, "no pump for a symbol that was not admitted");

    // The boot window is over and the gatewayhub answers again: the retry the boot spawns
    // (`readmit::retry_missed` over `readmit_and_price`) re-admits ETHUSDT at the runtime bound, and the
    // symbol is admitted, told to the daemon and priced with no restart.
    let (healed, still_missed) =
        readmit_and_price(&wanted[..1], &*second.facts, |c| second.rig.core().send_command(c));
    assert_eq!((healed.sent, still_missed.len()), (1, 0), "{healed:?} {still_missed:?}");
    assert_eq!(hub.count("ticks ETHUSDT 0.01"), 2, "the retry opened the pump: {:?}", hub.asked());
    assert!(wait_until(WAIT, || second.trades(ETH)), "the symbol is admitted on the retry");
    second.stop();
}

// ---- (6) a restart or a takeover in the middle of an admission -----------------------------------------------

/// **A gatewayhub restart in the middle of an admission leaves no half-admitted symbol and sends no order.**
/// The facts request is parked inside the gatewayhub's catalog, the daemon is restarted under it, and the door
/// refuses with the cause; the engine admitted nothing and the venue saw nothing. Once the node is back on the
/// new daemon the same order admits and places exactly once.
#[test]
fn a_gatewayhub_restart_in_the_middle_of_an_admission_admits_nothing_and_sends_nothing() {
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let mut hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Fixed(3000.0));

    let parked = catalog.hold.arm();
    let door = node.door.clone();
    let asking = std::thread::spawn(move || door.send(limit("h1", ETH, 1.0, 3000.0)));
    parked.recv_timeout(WAIT).expect("the facts request is in flight inside the gatewayhub");
    let first_daemon = Arc::clone(&hop.lane);
    hop.restart_gateway();
    let refused = asking.join().expect("the door thread").expect_err("the admission was cut");
    catalog.hold.release();

    assert!(refused.contains("cannot admit `ETHUSDT`"), "{refused}");
    assert!(refused.contains("REFUSED"), "{refused}");
    assert!(!node.trades(ETH), "no half-admitted symbol");
    assert!(world.seen_submits().is_empty(), "no order was sent");
    assert_eq!(first_daemon.facts_asked(), eth_pairs(1), "the request did reach the first daemon");

    // The node comes back on the new daemon (the adapter is started again), and the retry succeeds once.
    assert!(
        wait_until(WAIT, || hop.lane.inner.starts() == 1),
        "the adapter is started again on the new daemon: {:?}",
        hop.lane.inner.calls()
    );
    world.fate("h2", Fate::Fill);
    node.door.send(limit("h2", ETH, 1.0, 3000.0)).expect("the retry admits and places");
    venue_saw(&world, "h2");
    assert_eq!(world.seen_submits(), ["h2"], "exactly one order, the retry's");
    assert_eq!(hop.lane.facts_asked(), eth_pairs(2), "the new daemon was asked, then committed");
    assert!(wait_until(WAIT, || node.trades(ETH)), "and the symbol is admitted now");
}

/// **A lease takeover in the middle of an admission leaves no half-admitted symbol and sends no order.** A
/// second client takes the account's lease over while the facts request is parked at the gatewayhub: the
/// first node's door refuses with the cause, and keeps refusing (its lease is closed for good), with nothing
/// admitted and nothing sent.
#[test]
fn a_lease_takeover_in_the_middle_of_an_admission_admits_nothing_and_sends_nothing() {
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Fixed(3000.0));

    let parked = catalog.hold.arm();
    let door = node.door.clone();
    let asking = std::thread::spawn(move || door.send(limit("t1", ETH, 1.0, 3000.0)));
    parked.recv_timeout(WAIT).expect("the facts request is in flight inside the gatewayhub");
    let (usurper_events, _usurper_lane) = poll_lane();
    let usurper = TradeLink::new(hop.link(), trade_account("any-symbol-usurper"), usurper_events)
        .with_timings(fast_trade_timings())
        .with_takeover(true)
        .open()
        .expect("the takeover is granted");
    let refused = asking.join().expect("the door thread").expect_err("the admission was cut");
    catalog.hold.release();

    assert!(refused.contains("cannot admit `ETHUSDT`"), "{refused}");
    assert!(refused.contains("REFUSED"), "{refused}");
    let again = node
        .door
        .send(limit("t2", ETH, 1.0, 3000.0))
        .expect_err("a lease that was taken over asks nothing more");
    assert!(again.contains("cannot admit `ETHUSDT`"), "{again}");
    assert!(!node.trades(ETH), "no half-admitted symbol");
    assert!(world.seen_submits().is_empty(), "no order was sent");
    assert_eq!(
        hop.lane.facts_asked(),
        eth_pairs(1),
        "only the parked request ever reached the daemon"
    );
    drop(usurper);
}

// ---- (7) what the node remembers of a refusal ------------------------------------------------------------------

/// **The node remembers a DEFINITIVE refusal and nothing else.** An unknown symbol asked twice reaches the
/// gatewayhub once (the second ask is answered from the node's 30 s memory); a failed listing fetch (a 429)
/// is refused with its cause but NOT remembered by the node, so the second ask reaches the gatewayhub again
/// (which keeps the failure for its own 30 s: the venue is fetched once).
#[test]
fn only_a_definitive_refusal_is_remembered_by_the_node() {
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Fixed(3000.0));

    for id in ["d1", "d2"] {
        let why = node.door.send(limit(id, "nosuchusdt", 1.0, 5.0)).expect_err("unknown");
        assert!(why.contains("no instrument NOSUCHUSDT"), "{why}");
    }
    assert_eq!(hop.lane.facts_asked().len(), 1, "the second ask was answered by the node's memory");

    catalog.fail_with(Some("HTTP 429"));
    let fetches_before = catalog.fetches();
    for id in ["f1", "f2"] {
        let why = node.door.send(limit(id, ETH, 1.0, 3000.0)).expect_err("the listing fails");
        assert!(why.contains("HTTP 429"), "{why}");
    }
    assert_eq!(
        hop.lane.facts_asked().len(),
        3,
        "a failed fetch is not remembered by the node: both asks reached the gatewayhub"
    );
    assert_eq!(catalog.fetches() - fetches_before, 1, "the gatewayhub fetched the venue once");
    assert!(world.seen_submits().is_empty(), "no refused order reached the venue");
}

/// **A link failure in the middle of the node's work is never remembered.** The daemon is down: the order is
/// refused with the link's cause. The daemon is back: the very next ask admits and places, with no wait.
#[test]
fn a_link_failure_is_not_remembered_the_retry_admits_at_once() {
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let mut hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Fixed(3000.0));

    hop.stop_gateway();
    let down = node.door.send(limit("l1", ETH, 1.0, 3000.0)).expect_err("the daemon is down");
    assert!(down.contains("cannot admit `ETHUSDT`"), "{down}");
    assert!(!node.trades(ETH), "no half-admitted symbol");

    hop.start_gateway();
    assert!(
        wait_until(WAIT, || hop.lane.inner.starts() == 1),
        "the adapter is started again on the new daemon: {:?}",
        hop.lane.inner.calls()
    );
    world.fate("l2", Fate::Fill);
    node.door.send(limit("l2", ETH, 1.0, 3000.0)).expect("the retry admits and places at once");
    venue_saw(&world, "l2");
    assert_eq!(world.seen_submits(), ["l2"], "exactly one order, the retry's");
    assert!(wait_until(WAIT, || node.trades(ETH)), "and the symbol is admitted now");
}

// ---- the labelled account ----------------------------------------------------------------------------------------

fn labelled_roster() -> Vec<VenueBlock> {
    vec![VenueBlock {
        venue: VENUE.into(),
        route_key: format!("{VENUE}#ALT"),
        symbol: SYMBOL.into(),
        tier: Some(AccountTier::Demo),
        ..Default::default()
    }]
}

fn labelled_order() -> WireCommand {
    let WireCommand::Submit(mut req) = limit("l1", ETH, 1.0, 3000.0) else { unreachable!() };
    req.account = Some("ALT".into());
    WireCommand::Submit(req)
}

/// A labelled account is no longer refused for being labelled (decision 0174 item 4: the core fans a quote
/// out to it): the door asks for ITS lease. This rig opened none for `binance#ALT`, so the refusal is the
/// lookup's, by name, and nothing is asked of the hop, subscribed or sent. (A labelled account that has a
/// lease is covered by `admits_any_symbol.rs` and the core's `labelled_fanout` wiring tests.)
#[test]
fn a_labelled_account_with_no_lease_is_refused_by_the_lookup_before_the_hop_is_asked() {
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Fixed(3000.0));
    let why = node
        .door
        .send_with_blocks(labelled_order(), &labelled_roster())
        .expect_err("no lease for the labelled account");
    assert!(why.contains("no open trade lease") && why.contains("`binance#ALT`"), "{why}");
    assert!(why.contains("REFUSED"), "{why}");
    assert!(hop.lane.facts_asked().is_empty(), "no lookup for an account with no lease");
    assert!(world.seen_submits().is_empty());
}

/// **That refusal is one plain line** (it was a BUG: `plan_submit_symbol` carried the labelled-account
/// block TWICE, and the copy that ran held literal `\n` escapes, so the operator read the sentence broken
/// across lines with 13 spaces of indentation). One copy is left.
#[test]
fn a_labelled_accounts_refusal_is_one_plain_line() {
    let world = World::new();
    let catalog = Catalog::new(rows(grid(0.01, 0.0001)));
    let hop = catalog_hop(&world, &catalog);
    let node = Node::start(&hop, Prices::Fixed(3000.0));
    let why = node
        .door
        .send_with_blocks(labelled_order(), &labelled_roster())
        .expect_err("a labelled account takes no foreign symbol");
    assert!(!why.contains('\n'), "the refusal an operator reads is one line: {why:?}");
    assert!(!why.contains("  "), "...without a run of indentation inside the sentence: {why:?}");
}
