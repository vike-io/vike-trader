//! The fake peer of the TRADE class: a scripted gatewayhub on loopback that speaks this crate's own
//! protocol, verifies the handshake with the real keys, and RECORDS every frame the client sends, so
//! "nothing was sent" is an empty slice and "the client never re-sent a Written frame" is a list of
//! `Submit` frames with distinct coids. A real daemon cannot prove either: it answers the frame
//! either way.
//!
//! It models exactly the parts of the gatewayhub the client's guarantees lean on:
//!
//! * **the lease**: one holder per peer; a second client is refused with `LeaseHeld` unless it takes
//!   over (the old holder gets `Bye(Takeover)` and a closed socket); the same client reconnecting
//!   fences its own old connection first;
//! * **`received_through_cseq`**: the highest `cseq` of the client the peer has actually READ. A test
//!   makes a frame Written-but-never-received by [`TradePeer::pause_reading`] before the client sends
//!   it, then [`TradePeer::kill_lease`]: the bytes sit unread in the kernel and die with the socket;
//! * **the epoch and the ring**: [`TradePeer::push_event`] assigns the next `seq`, keeps the event and
//!   writes it to the live lease; a `TradeOpen{resume}` is answered by a replay (`replayed: true`) or by
//!   `ReplayUnavailable`; [`TradePeer::restart`] is a gatewayhub restart (a new random epoch, an empty
//!   ring, no watermark);
//! * **scripted answers** for the request frames (`AdapterStart`, `WireIds`, `ResolveOrders`, `Recon`,
//!   ...) and the rpc verbs.
//!
//! Each caller `#[path]`-includes it (a bare `mod` would resolve beside the binary root); every
//! includer uses a different part, so the includer's `mod` declaration carries the one
//! `#[allow(dead_code, clippy::allow_attributes)]`.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vike_exec::{EventSender, Ingest};
use vike_gatewayhub_client::client::{GatewayLink, GatewayTimings};
use vike_gatewayhub_client::proto::{
    AdapterReply, ArmingTable, GATEWAYHUB_DOMAIN, GATEWAYHUB_PROTO_VERSION, KNOWN_FEATURES,
    KNOWN_TRADE_FEATURES, OrderResolution, ReconKind, ReconPayload, Request, ResolveState,
    Response, TradeBye, TradeDown, TradeOpened, TradeStatusReport, TradeUp, WireCursor,
    WireSymbolFacts, md_venue_feature, read_frame_raw, trade_venue_feature, write_frame,
};
use vike_gatewayhub_client::trade::{TradeAccount, TradeTimings};
use vike_hub_proto::auth::{HubKeys, Scope, fresh_nonce};
use vike_model::events::Event;
use vike_model::{AccountTier, SymbolProperties};

/// One thing the peer saw, in arrival order.
// A test log: a few hundred entries at most, and the tests read `Up { frame }` by value.
#[derive(Debug, Clone)]
#[expect(clippy::large_enum_variant)]
pub enum Seen {
    /// The opening `Hello`.
    Hello(u32),
    /// An `Auth`: the scope the client claimed and whether its mac verified.
    Auth(Scope, bool),
    /// A positional request on connection `conn` (anything but `TradeOpen`).
    Rpc { conn: usize, req: Request },
    /// A `TradeOpen` on connection `conn`.
    Open {
        conn: usize,
        client_id: String,
        venue: String,
        account: Option<String>,
        tier: AccountTier,
        resume: Option<WireCursor>,
        takeover: bool,
    },
    /// A lease frame on connection `conn`.
    Up { conn: usize, frame: TradeUp },
}

/// What to do for one request frame.
// A test script builds a handful of these per request; boxing the frame would respell every script.
#[derive(Debug, Clone)]
#[expect(clippy::large_enum_variant)]
pub enum Step {
    /// Write this frame.
    Send(TradeDown),
    /// Wait.
    Sleep(Duration),
    /// Close the connection.
    Close,
}

/// Whether the peer acknowledges command frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    /// A `CmdAck` for every command frame it reads.
    Auto,
    /// Never: the client's frames stay Written-unacknowledged until a test sends the ack.
    Never,
}

/// Request frame in, steps out.
pub type Script = Arc<dyn Fn(&TradeUp) -> Vec<Step> + Send + Sync>;

/// Sees every COMMAND frame (a `Submit`, `Cancel`, `AdoptRestored`, ...) as the peer reads it, before it is
/// acknowledged. Commands never reach the [`Script`] (which answers request frames), so a test that models
/// what the daemon's adapter does with a command observes it here.
pub type CommandObserver = Arc<dyn Fn(&TradeUp) + Send + Sync>;

/// Positional request in, responses out.
pub type RpcScript = Arc<dyn Fn(&Request) -> Vec<Response> + Send + Sync>;

/// How the peer behaves.
pub struct TradePeerConfig {
    /// The `Welcome.features`.
    pub features: Vec<String>,
    /// The keys it verifies against.
    pub keys: HubKeys,
    /// The heartbeat period it states in `TradeOpened`.
    pub heartbeat_ms: u64,
    /// Acknowledge commands?
    pub ack: Ack,
    /// The ring's account, spelled as the client's route key.
    pub route_key: String,
    /// Request frames.
    pub script: Script,
    /// Command frames, seen as they are read ([`CommandObserver`]).
    pub observe: Option<CommandObserver>,
    /// Positional requests.
    pub rpc: RpcScript,
    /// Does `TradeOpened` carry `event_ring_floor`? `false` is an OLDER daemon, which omits the field.
    pub says_ring_floor: bool,
    /// A RESTORED session (W2G, `crates/vike-gatewayhub/CLAUDE.md`'s restore trap): the first lease of a
    /// new epoch is started at the ring's FIRST event, so what the rebuilt adapter pushed before anyone
    /// attached is replayed right behind `TradeOpened` (after the `ReplayUnavailable` a foreign cursor owes).
    pub replays_the_whole_ring_to_a_new_epoch: bool,
}

impl TradePeerConfig {
    /// A healthy trade daemon for `binance`: every trade string, the test keys, auto acks.
    pub fn new() -> Self {
        let mut features: Vec<String> = KNOWN_FEATURES.iter().map(|s| (*s).to_string()).collect();
        features.extend(KNOWN_TRADE_FEATURES.iter().map(|s| (*s).to_string()));
        features.push(md_venue_feature("binance"));
        features.push(trade_venue_feature("binance"));
        Self {
            features,
            keys: trade_keys(),
            heartbeat_ms: 500,
            ack: Ack::Auto,
            route_key: "binance".to_string(),
            script: default_script(),
            observe: None,
            rpc: Arc::new(default_rpc),
            says_ring_floor: true,
            replays_the_whole_ring_to_a_new_epoch: false,
        }
    }

    /// A daemon that rebuilt the session itself: the new epoch's ring is replayed from its first event to
    /// the first lease that opens on it.
    #[must_use]
    pub fn replaying_a_restored_ring(mut self) -> Self {
        self.replays_the_whole_ring_to_a_new_epoch = true;
        self
    }

    /// An older daemon: its `TradeOpened` omits `event_ring_floor` (the client reads `None`).
    #[must_use]
    pub fn without_ring_floor(mut self) -> Self {
        self.says_ring_floor = false;
        self
    }

    /// Script the request frames.
    #[must_use]
    pub fn with_script(
        mut self,
        f: impl Fn(&TradeUp) -> Vec<Step> + Send + Sync + 'static,
    ) -> Self {
        self.script = Arc::new(f);
        self
    }

    /// Observe the command frames the peer reads.
    #[must_use]
    pub fn with_command_observer(mut self, f: impl Fn(&TradeUp) + Send + Sync + 'static) -> Self {
        self.observe = Some(Arc::new(f));
        self
    }

    /// Script the positional requests.
    #[must_use]
    pub fn with_rpc(
        mut self,
        f: impl Fn(&Request) -> Vec<Response> + Send + Sync + 'static,
    ) -> Self {
        self.rpc = Arc::new(f);
        self
    }

    /// Replace the advertised features.
    #[must_use]
    pub fn with_features(mut self, features: Vec<String>) -> Self {
        self.features = features;
        self
    }

    /// Never acknowledge commands.
    #[must_use]
    pub fn never_ack(mut self) -> Self {
        self.ack = Ack::Never;
        self
    }

    /// State this heartbeat period.
    #[must_use]
    pub fn with_heartbeat_ms(mut self, ms: u64) -> Self {
        self.heartbeat_ms = ms;
        self
    }
}

impl Default for TradePeerConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// The keys the fake verifies and the tests sign with: observe, control and a TRADE key.
pub fn trade_keys() -> HubKeys {
    HubKeys::new(b"observe-key-of-the-tests".to_vec(), b"control-key-of-the-tests".to_vec())
        .with_trade(b"trade-key-of-the-tests".to_vec())
}

/// Keys with NO trade key: a link that cannot sign a Trade connection.
pub fn keys_without_trade() -> HubKeys {
    HubKeys::new(b"observe-key-of-the-tests".to_vec(), b"control-key-of-the-tests".to_vec())
}

/// Gateway timings short enough that a redial is watched in milliseconds.
pub fn fast_gateway_timings() -> GatewayTimings {
    GatewayTimings {
        reply_idle: Duration::from_secs(5),
        lookup_reply: Duration::from_secs(5),
        fetch_stall_floor: Duration::from_millis(600),
        fetch_probe_slice: Duration::from_millis(20),
        stream_read_floor: Duration::from_millis(300),
        sub_reply: Duration::from_millis(600),
        reconnect_base: Duration::from_millis(20),
        reconnect_max: Duration::from_millis(100),
    }
}

/// Trade timings short enough that a dead link is called dead in a third of a second.
pub fn fast_trade_timings() -> TradeTimings {
    TradeTimings {
        read_floor: Duration::from_millis(400),
        request_reply: Duration::from_secs(5),
        recon_stall_floor: Duration::from_millis(600),
        detach_flush: Duration::from_secs(3),
    }
}

/// A link to `addr` with the trade keys and the fast gateway timings.
pub fn link_to(addr: &str) -> GatewayLink {
    GatewayLink::new(addr, trade_keys()).with_timings(fast_gateway_timings())
}

/// The account every test leases unless it says otherwise.
pub fn account(client_id: &str) -> TradeAccount {
    TradeAccount {
        client_id: client_id.to_string(),
        venue: "binance".to_string(),
        label: None,
        tier: AccountTier::Demo,
        route_key: "binance".to_string(),
    }
}

/// An address nothing listens on: a connect is refused at once.
pub fn dead_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("its address");
    drop(listener);
    addr.to_string()
}

/// The default answers of a healthy daemon to the request frames.
pub fn default_script() -> Script {
    Arc::new(|frame| match frame {
        TradeUp::AdapterStart(start) => vec![Step::Send(TradeDown::AdapterReply {
            rid: start.rid,
            reply: Box::new(AdapterReply::Live {
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
                recon: true,
                leverage_cap: None,
                account_ccy: None,
                account_ccy_assumed: false,
            }),
        })],
        TradeUp::WireIds { rid, coids } => vec![Step::Send(TradeDown::WireIdsReply {
            rid: *rid,
            pairs: coids
                .iter()
                .filter(|c| c.starts_with("hl-"))
                .map(|c| (c.clone(), format!("0x{c}")))
                .collect(),
        })],
        TradeUp::ResolveOrders { rid, coids } => vec![Step::Send(TradeDown::OrdersResolved {
            rid: *rid,
            results: coids
                .iter()
                .map(|c| OrderResolution { coid: c.clone(), state: ResolveState::Querying })
                .collect(),
        })],
        TradeUp::IdentityRecord { rid } => vec![Step::Send(TradeDown::IdentityRecorded {
            rid: *rid,
            ok: true,
            note: String::new(),
        })],
        TradeUp::TradeStatus { rid } => vec![Step::Send(TradeDown::TradeStatusIs {
            rid: *rid,
            status: TradeStatusReport { adapter_up: true, ..TradeStatusReport::default() },
        })],
        TradeUp::SymbolFacts { rid, symbol, .. } => vec![Step::Send(TradeDown::SymbolFactsReply {
            rid: *rid,
            facts: if symbol == "BUSYUSDT" {
                Err(vike_gatewayhub_client::proto::FACTS_BUSY.to_string())
            } else if symbol == "SOLUSDT.P" {
                Ok(WireSymbolFacts {
                    properties: SymbolProperties {
                        tick_size: 0.01,
                        step_size: 1.0,
                        ..SymbolProperties::default()
                    },
                    ..WireSymbolFacts::default()
                })
            } else {
                Err(format!("binance: no instrument {symbol} in the venue's instrument list"))
            },
        })],
        TradeUp::Recon { rid, kind } => vec![Step::Send(TradeDown::ReconReply {
            rid: *rid,
            page: 0,
            more: false,
            payload: empty_payload(kind),
        })],
        _ => Vec::new(),
    })
}

/// The empty answer of each reconcile kind.
pub fn empty_payload(kind: &ReconKind) -> ReconPayload {
    match kind {
        ReconKind::Orders { .. } => ReconPayload::Orders(Vec::new()),
        ReconKind::Fills { .. } => ReconPayload::Fills(Vec::new()),
        ReconKind::Positions => ReconPayload::Positions(Vec::new()),
        ReconKind::Balance => ReconPayload::Balance(None),
        ReconKind::FeeRates => ReconPayload::FeeRates(None),
        ReconKind::AccountIdentity => ReconPayload::AccountIdentity(None),
    }
}

fn default_rpc(req: &Request) -> Vec<Response> {
    match req {
        Request::ArmingQuery => vec![Response::ArmingTable(Box::<ArmingTable>::default())],
        Request::PreflightQuery { .. } => {
            vec![Response::PreflightLegs { legs: Vec::new(), plan: None }]
        }
        _ => vec![Response::Error("fake trade peer: no script".to_string())],
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
struct RingEntry {
    gw_epoch: u64,
    seq: u64,
    ts_ms: i64,
    event: Event,
}

struct Lease {
    client_id: String,
    key_id: String,
    writer: Arc<Mutex<TcpStream>>,
    done: Arc<AtomicBool>,
    killed: Arc<AtomicBool>,
}

struct Shared {
    log: Mutex<Vec<Seen>>,
    conns: AtomicUsize,
    sockets: Mutex<Vec<TcpStream>>,
    stop: AtomicBool,
    epoch: AtomicU64,
    lease_epoch: AtomicU64,
    received: Mutex<HashMap<String, u64>>,
    lease: Mutex<Option<Lease>>,
    /// The `done` flag of the lease `kill_lease` cut last. `kill_lease` takes the lease out of `lease`, so a
    /// fast redial finds no `old` to fence; `open_lease` waits on this instead, as the real daemon waits
    /// `old.fence()` then `wait_finished(fence_drain)` (`attach` in
    /// `crates/vike-gatewayhub/src/trade/session.rs`): the watermark it reports then counts every frame the
    /// cut connection's handler still reads.
    dying: Mutex<Option<Arc<AtomicBool>>>,
    leases_opened: AtomicUsize,
    ring: Mutex<Vec<RingEntry>>,
    next_seq: AtomicU64,
    read_paused: AtomicBool,
    refuse_open: Mutex<Option<Response>>,
    /// The next `n` opens are answered with the response, then opens succeed again.
    refuse_budget: Mutex<Option<(u32, Response)>>,
    acks_on: AtomicBool,
    /// Held by `open_lease` from the open reply to the end of the replay, and by `push_event` around its
    /// ring append and live send: an event is either in the replay or sent live, never both, never before
    /// the open reply.
    gate: Mutex<()>,
}

/// A running fake gatewayhub for the trade class.
pub struct TradePeer {
    pub addr: SocketAddr,
    shared: Arc<Shared>,
    cfg: Arc<TradePeerConfig>,
    acceptor: Option<JoinHandle<()>>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl TradePeer {
    /// Start a peer and begin accepting.
    pub fn start(config: TradePeerConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("resolve the assigned port");
        listener.set_nonblocking(true).expect("non-blocking accept");
        let shared = Arc::new(Shared {
            log: Mutex::new(Vec::new()),
            conns: AtomicUsize::new(0),
            sockets: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            epoch: AtomicU64::new(0xE0_0001),
            lease_epoch: AtomicU64::new(0),
            received: Mutex::new(HashMap::new()),
            lease: Mutex::new(None),
            dying: Mutex::new(None),
            leases_opened: AtomicUsize::new(0),
            ring: Mutex::new(Vec::new()),
            next_seq: AtomicU64::new(1),
            read_paused: AtomicBool::new(false),
            refuse_open: Mutex::new(None),
            refuse_budget: Mutex::new(None),
            acks_on: AtomicBool::new(config.ack == Ack::Auto),
            gate: Mutex::new(()),
        });
        let cfg = Arc::new(config);
        let handlers: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let (sh, cf, hs) = (Arc::clone(&shared), Arc::clone(&cfg), Arc::clone(&handlers));
        let acceptor = thread::Builder::new()
            .name("fake-tp-accept".into())
            .spawn(move || {
                while !sh.stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            if let Ok(clone) = stream.try_clone() {
                                lock(&sh.sockets).push(clone);
                            }
                            let conn = sh.conns.fetch_add(1, Ordering::SeqCst);
                            let (sh2, cf2) = (Arc::clone(&sh), Arc::clone(&cf));
                            let h = thread::Builder::new()
                                .name("fake-tp-conn".into())
                                .spawn(move || connection(conn, stream, &cf2, &sh2))
                                .expect("spawn a connection handler");
                            lock(&hs).push(h);
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
            .expect("spawn the acceptor");
        Self { addr, shared, cfg, acceptor: Some(acceptor), handlers }
    }

    /// The address as the string a link takes.
    pub fn addr_string(&self) -> String {
        self.addr.to_string()
    }

    /// A link to this peer: the trade keys and the fast gateway timings.
    pub fn link(&self) -> GatewayLink {
        link_to(&self.addr_string())
    }

    /// Every frame seen so far.
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.shared.log).clone()
    }

    /// The lease frames seen so far, across every connection, in arrival order.
    pub fn ups(&self) -> Vec<TradeUp> {
        self.seen()
            .into_iter()
            .filter_map(|s| match s {
                Seen::Up { frame, .. } => Some(frame),
                _ => None,
            })
            .collect()
    }

    /// The `TradeOpen`s seen, as `(conn, client_id, resume, takeover)`.
    pub fn opens(&self) -> Vec<(usize, String, Option<WireCursor>, bool)> {
        self.seen()
            .into_iter()
            .filter_map(|s| match s {
                Seen::Open { conn, client_id, resume, takeover, .. } => {
                    Some((conn, client_id, resume, takeover))
                }
                _ => None,
            })
            .collect()
    }

    /// The `Auth` frames seen: `(claimed scope, mac verified)`.
    pub fn auths(&self) -> Vec<(Scope, bool)> {
        self.seen()
            .into_iter()
            .filter_map(|s| match s {
                Seen::Auth(scope, ok) => Some((scope, ok)),
                _ => None,
            })
            .collect()
    }

    /// The positional requests seen (anything but `TradeOpen`).
    pub fn rpcs(&self) -> Vec<Request> {
        self.seen()
            .into_iter()
            .filter_map(|s| match s {
                Seen::Rpc { req, .. } => Some(req),
                _ => None,
            })
            .collect()
    }

    /// How many `Hello`s arrived.
    pub fn hellos(&self) -> usize {
        self.seen().iter().filter(|s| matches!(s, Seen::Hello(_))).count()
    }

    /// How many leases were opened (a `TradeOpened` written).
    pub fn leases_opened(&self) -> usize {
        self.shared.leases_opened.load(Ordering::SeqCst)
    }

    /// The gateway epoch the peer currently reports.
    pub fn epoch(&self) -> u64 {
        self.shared.epoch.load(Ordering::SeqCst)
    }

    /// The highest `cseq` of `client_id` the peer has READ.
    pub fn received_through(&self, client_id: &str) -> u64 {
        lock(&self.shared.received).get(client_id).copied().unwrap_or(0)
    }

    /// Stop READING client frames (the bytes stay unread in the kernel) until [`Self::resume_reading`].
    pub fn pause_reading(&self) {
        self.shared.read_paused.store(true, Ordering::SeqCst);
    }

    /// Read again.
    pub fn resume_reading(&self) {
        self.shared.read_paused.store(false, Ordering::SeqCst);
    }

    /// Switch command acknowledgement on or off.
    pub fn set_acks(&self, on: bool) {
        self.shared.acks_on.store(on, Ordering::SeqCst);
    }

    /// Answer the next `TradeOpen`s with this response instead of opening (`None` = open again).
    pub fn refuse_open(&self, response: Option<Response>) {
        *lock(&self.shared.refuse_open) = response;
    }

    /// Answer the next `n` `TradeOpen`s with `response`, then open again (a refusal that passes).
    pub fn refuse_next_opens(&self, n: u32, response: Response) {
        *lock(&self.shared.refuse_budget) = Some((n, response));
    }

    /// Wait until `n` leases have been opened.
    pub fn wait_leases(&self, n: usize, within: Duration) -> bool {
        Self::wait_until(within, || self.leases_opened() >= n)
    }

    /// Write one frame to the current lease. `false` = no live lease.
    pub fn send(&self, frame: &TradeDown) -> bool {
        let writer = lock(&self.shared.lease).as_ref().map(|l| Arc::clone(&l.writer));
        let Some(writer) = writer else { return false };
        let mut w = lock(&writer);
        write_frame(&mut *w, frame).is_ok()
    }

    /// Cut the current lease's socket: the client sees a lost link.
    pub fn kill_lease(&self) {
        let lease = lock(&self.shared.lease).take();
        if let Some(lease) = lease {
            // Published BEFORE the socket is cut: the client redials the moment it sees the cut, and that
            // open must already find the flag.
            *lock(&self.shared.dying) = Some(Arc::clone(&lease.done));
            // `killed` first: the handler must stop BEFORE it reads another frame, or a frame the test
            // meant to leave unread would be read (and counted) after all.
            lease.killed.store(true, Ordering::SeqCst);
            let _ = lock(&lease.writer).shutdown(Shutdown::Both);
            Self::wait_until(Duration::from_secs(2), || lease.done.load(Ordering::SeqCst));
        }
    }

    /// A gatewayhub RESTART: a new random epoch, an empty ring, no watermark, the lease cut.
    pub fn restart(&self) {
        // The new process is in place BEFORE the old connection dies, so a fast redial cannot land on the
        // old epoch.
        self.shared.epoch.fetch_add(0x1_0000, Ordering::SeqCst);
        lock(&self.shared.ring).clear();
        lock(&self.shared.received).clear();
        self.shared.next_seq.store(1, Ordering::SeqCst);
        self.kill_lease();
    }

    /// Append an event to the ring and write it to the live lease. Returns its `seq`.
    pub fn push_event(&self, event: Event) -> u64 {
        let _gate = lock(&self.shared.gate);
        let seq = self.shared.next_seq.fetch_add(1, Ordering::SeqCst);
        let gw_epoch = self.epoch();
        let ts_ms = i64::try_from(seq).unwrap_or(0) * 10;
        lock(&self.shared.ring).push(RingEntry { gw_epoch, seq, ts_ms, event: event.clone() });
        self.send(&TradeDown::Event {
            account: self.cfg.route_key.clone(),
            gw_epoch,
            seq,
            ts_ms,
            replayed: false,
            event,
        });
        seq
    }

    /// Append to the ring WITHOUT writing (an event produced while the client was away).
    pub fn push_event_offline(&self, event: Event) -> u64 {
        let _gate = lock(&self.shared.gate);
        let seq = self.shared.next_seq.fetch_add(1, Ordering::SeqCst);
        let gw_epoch = self.epoch();
        let ts_ms = i64::try_from(seq).unwrap_or(0) * 10;
        lock(&self.shared.ring).push(RingEntry { gw_epoch, seq, ts_ms, event });
        seq
    }

    /// Poll `cond` until it holds or `within` passes; whether it held.
    pub fn wait_until(within: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if cond() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for TradePeer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.read_paused.store(false, Ordering::SeqCst);
        if let Some(a) = self.acceptor.take() {
            let _ = a.join();
        }
        for s in lock(&self.shared.sockets).iter() {
            let _ = s.shutdown(Shutdown::Both);
        }
        let handlers: Vec<JoinHandle<()>> = std::mem::take(&mut *lock(&self.handlers));
        for h in handlers {
            let _ = h.join();
        }
    }
}

fn note(shared: &Shared, seen: Seen) {
    lock(&shared.log).push(seen);
}

fn send_to(writer: &Arc<Mutex<TcpStream>>, frame: &impl serde::Serialize) -> bool {
    let mut w = lock(writer);
    write_frame(&mut *w, frame).is_ok()
}

/// One connection. However it ends, the socket is shut.
fn connection(conn: usize, stream: TcpStream, cfg: &Arc<TradePeerConfig>, shared: &Arc<Shared>) {
    let closer = stream.try_clone().ok();
    serve(conn, stream, cfg, shared);
    if let Some(c) = closer {
        let _ = c.shutdown(Shutdown::Both);
    }
}

fn serve(conn: usize, mut stream: TcpStream, cfg: &Arc<TradePeerConfig>, shared: &Arc<Shared>) {
    let _ = stream.set_nodelay(true);
    let Ok(Request::Hello { proto_version }) =
        vike_gatewayhub_client::proto::read_frame::<_, Request>(&mut stream)
    else {
        return;
    };
    note(shared, Seen::Hello(proto_version));
    let nonce = fresh_nonce();
    let welcome = Response::Welcome {
        proto_version: GATEWAYHUB_PROTO_VERSION,
        features: cfg.features.clone(),
        nonce,
    };
    if write_frame(&mut stream, &welcome).is_err() {
        return;
    }
    // The client may hang up here (it refused the plane or a missing string BEFORE signing).
    let Ok(Request::Auth { scope, mac }) =
        vike_gatewayhub_client::proto::read_frame::<_, Request>(&mut stream)
    else {
        return;
    };
    let ok = cfg.keys.verify(GATEWAYHUB_DOMAIN, scope, &nonce, GATEWAYHUB_PROTO_VERSION, &mac);
    note(shared, Seen::Auth(scope, ok));
    if !ok {
        let _ = write_frame(&mut stream, &Response::AuthDenied { reason: "bad mac".into() });
        return;
    }
    if write_frame(&mut stream, &Response::AuthOk { scope }).is_err() {
        return;
    }
    let key_id = cfg.keys.key_id(scope).unwrap_or_else(|| "nk-unknown".to_string());

    loop {
        let Ok(body) = read_frame_raw(&mut stream) else { return };
        let Ok(request) = serde_json::from_slice::<Request>(&body) else { return };
        match request {
            Request::TradeOpen { client_id, venue, account, tier, resume, takeover } => {
                note(
                    shared,
                    Seen::Open {
                        conn,
                        client_id: client_id.clone(),
                        venue,
                        account,
                        tier,
                        resume,
                        takeover,
                    },
                );
                let budgeted = {
                    let mut budget = lock(&shared.refuse_budget);
                    match budget.as_mut() {
                        Some((left, response)) if *left > 0 => {
                            *left -= 1;
                            Some(response.clone())
                        }
                        _ => None,
                    }
                };
                if let Some(response) = budgeted.or_else(|| lock(&shared.refuse_open).clone()) {
                    if write_frame(&mut stream, &response).is_err() {
                        return;
                    }
                    continue;
                }
                // A refusal (LeaseHeld) is answered inside and ends this connection too.
                let _opened =
                    open_lease(conn, stream, &client_id, &key_id, resume, takeover, cfg, shared);
                return;
            }
            Request::Ping => {
                if write_frame(&mut stream, &Response::Pong).is_err() {
                    return;
                }
            }
            other => {
                note(shared, Seen::Rpc { conn, req: other.clone() });
                for response in (cfg.rpc)(&other) {
                    if write_frame(&mut stream, &response).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

/// Answer a `TradeOpen` and run the lease. Returns `false` when it was refused.
#[allow(clippy::too_many_arguments, clippy::allow_attributes)]
fn open_lease(
    conn: usize,
    mut stream: TcpStream,
    client_id: &str,
    key_id: &str,
    resume: Option<WireCursor>,
    takeover: bool,
    cfg: &Arc<TradePeerConfig>,
    shared: &Arc<Shared>,
) -> bool {
    // Fence the lease `kill_lease` cut: its handler may still read one frame it had already peeked, and the
    // watermark below must count it.
    let dying = lock(&shared.dying).clone();
    if let Some(done) = dying {
        TradePeer::wait_until(Duration::from_millis(1_000), || done.load(Ordering::SeqCst));
    }
    // Fence: whoever holds the lease is dealt with BEFORE the new open is answered.
    let old = lock(&shared.lease).take();
    if let Some(old) = old {
        let same_client = old.client_id == client_id;
        if !same_client && !takeover {
            let held = Response::LeaseHeld { holder_key_id: old.key_id.clone(), since_ms: 1 };
            // Put the holder back: it was not displaced.
            *lock(&shared.lease) = Some(old);
            let _ = write_frame(&mut stream, &held);
            return false;
        }
        if !same_client {
            let _ = send_to(
                &old.writer,
                &TradeDown::Bye(TradeBye::Takeover { by_key_id: key_id.to_string() }),
            );
        }
        if same_client {
            // FENCED: the old connection is read to its end BEFORE the new open is answered, so
            // `received_through_cseq` counts everything the old one had already sent.
            TradePeer::wait_until(Duration::from_millis(1_000), || old.done.load(Ordering::SeqCst));
        }
        old.killed.store(true, Ordering::SeqCst);
        let _ = lock(&old.writer).shutdown(Shutdown::Both);
        TradePeer::wait_until(Duration::from_secs(2), || old.done.load(Ordering::SeqCst));
    }

    let gate = lock(&shared.gate);
    let epoch = shared.epoch.load(Ordering::SeqCst);
    let opened = TradeOpened {
        gw_epoch: epoch,
        lease_epoch: shared.lease_epoch.fetch_add(1, Ordering::SeqCst) + 1,
        received_through_cseq: lock(&shared.received).get(client_id).copied().unwrap_or(0),
        project_token: "project-token-of-the-tests".to_string(),
        heartbeat_ms: cfg.heartbeat_ms,
        event_ring_floor: cfg
            .says_ring_floor
            .then(|| lock(&shared.ring).first().map_or(0, |e| e.seq)),
    };
    let writer = match stream.try_clone() {
        Ok(w) => Arc::new(Mutex::new(w)),
        Err(_) => return false,
    };
    let done = Arc::new(AtomicBool::new(false));
    let killed = Arc::new(AtomicBool::new(false));
    {
        // The lease is registered and counted under the SAME lock the open reply is written under, and
        // BEFORE the write: a test that sees the count has the lease (its `send` waits for this lock),
        // and one that acts the moment `open()` returns finds the lease there.
        let mut slot = lock(&shared.lease);
        *slot = Some(Lease {
            client_id: client_id.to_string(),
            key_id: key_id.to_string(),
            writer: Arc::clone(&writer),
            done: Arc::clone(&done),
            killed: Arc::clone(&killed),
        });
        shared.leases_opened.fetch_add(1, Ordering::SeqCst);
        if write_frame(&mut stream, &Response::TradeOpened(opened)).is_err() {
            *slot = None;
            return false;
        }
    }

    // Resume: replay what the client missed, or say the ring cannot.
    if let Some(cursor) = resume {
        let ring = lock(&shared.ring).clone();
        let floor = ring.first().map_or(0, |e| e.seq);
        let replayable = cursor.gw_epoch == epoch && (floor == 0 || cursor.seq + 1 >= floor);
        if replayable {
            for entry in ring.iter().filter(|e| e.gw_epoch == epoch && e.seq > cursor.seq) {
                let frame = TradeDown::Event {
                    account: cfg.route_key.clone(),
                    gw_epoch: entry.gw_epoch,
                    seq: entry.seq,
                    ts_ms: entry.ts_ms,
                    replayed: true,
                    event: entry.event.clone(),
                };
                if !send_to(&writer, &frame) {
                    break;
                }
            }
        } else {
            let _ = send_to(&writer, &TradeDown::ReplayUnavailable { floor });
            if cfg.replays_the_whole_ring_to_a_new_epoch {
                for entry in ring.iter().filter(|e| e.gw_epoch == epoch) {
                    let frame = TradeDown::Event {
                        account: cfg.route_key.clone(),
                        gw_epoch: entry.gw_epoch,
                        seq: entry.seq,
                        ts_ms: entry.ts_ms,
                        replayed: true,
                        event: entry.event.clone(),
                    };
                    if !send_to(&writer, &frame) {
                        break;
                    }
                }
            }
        }
    }

    drop(gate);
    lease_loop(conn, stream, client_id, cfg, shared, &writer, &killed);
    done.store(true, Ordering::SeqCst);
    true
}

fn lease_loop(
    conn: usize,
    mut stream: TcpStream,
    client_id: &str,
    cfg: &Arc<TradePeerConfig>,
    shared: &Arc<Shared>,
    writer: &Arc<Mutex<TcpStream>>,
    killed: &Arc<AtomicBool>,
) {
    loop {
        if killed.load(Ordering::SeqCst) {
            return;
        }
        // Wait for data WITHOUT reading it, so a paused peer leaves the bytes in the kernel.
        let _ = stream.set_read_timeout(Some(Duration::from_millis(10)));
        let mut probe = [0u8; 1];
        match stream.peek(&mut probe) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if shared.stop.load(Ordering::SeqCst) {
                    return;
                }
                continue;
            }
            Err(_) => return,
        }
        if shared.read_paused.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(10));
            if shared.stop.load(Ordering::SeqCst) {
                return;
            }
            continue;
        }
        let _ = stream.set_read_timeout(None);
        // `killed` may have been set while this thread peeked: a frame read now would move the watermark
        // after the cut, which the test meant to be final.
        if killed.load(Ordering::SeqCst) {
            return;
        }
        let Ok(body) = read_frame_raw(&mut stream) else { return };
        let Ok(frame) = serde_json::from_slice::<TradeUp>(&body) else { return };
        // The watermark moves when the frame is READ, before anything else is done with it.
        if let Some(cseq) = cseq_of(&frame) {
            let mut received = lock(&shared.received);
            let slot = received.entry(client_id.to_string()).or_insert(0);
            *slot = (*slot).max(cseq);
        }
        note(shared, Seen::Up { conn, frame: frame.clone() });
        match &frame {
            TradeUp::Ping => {
                if !send_to(writer, &TradeDown::Pong) {
                    return;
                }
            }
            TradeUp::Detach { phase } => {
                if *phase == vike_gatewayhub_client::proto::DetachPhase::Final {
                    let _ = send_to(writer, &TradeDown::Bye(TradeBye::ClientClosed));
                    return;
                }
            }
            other => {
                if let Some(cseq) = cseq_of(other) {
                    if let Some(observe) = &cfg.observe {
                        observe(other);
                    }
                    if shared.acks_on.load(Ordering::SeqCst)
                        && !send_to(writer, &TradeDown::CmdAck { cseq })
                    {
                        return;
                    }
                    continue;
                }
                for step in (cfg.script)(other) {
                    match step {
                        Step::Send(down) => {
                            if !send_to(writer, &down) {
                                return;
                            }
                        }
                        Step::Sleep(d) => thread::sleep(d),
                        Step::Close => return,
                    }
                }
            }
        }
    }
}

/// The `cseq` of a command frame.
pub fn cseq_of(frame: &TradeUp) -> Option<u64> {
    match frame {
        TradeUp::Submit { cseq, .. }
        | TradeUp::SubmitBatch { cseq, .. }
        | TradeUp::Cancel { cseq, .. }
        | TradeUp::CancelBatch { cseq, .. }
        | TradeUp::Modify { cseq, .. }
        | TradeUp::Confirm { cseq, .. }
        | TradeUp::AdoptRestored { cseq, .. } => Some(*cseq),
        _ => None,
    }
}

/// What the core's lane received: a polling reader over the `event_channel` receiver (the closure names
/// the channel's type so this file does not have to).
pub struct Lane {
    poll: Box<dyn FnMut() -> Option<Ingest> + Send>,
}

/// A fresh core lane: the sender the client delivers to (already `routed("binance")`, as a mount makes
/// it) and the reader a test drains.
pub fn lane() -> (EventSender, Lane) {
    let (tx, mut rx) = vike_exec::event_channel(1024);
    (tx.routed("binance"), Lane { poll: Box::new(move || rx.try_recv().ok()) })
}

impl Lane {
    /// Everything that arrived so far.
    pub fn drain(&mut self) -> Vec<Ingest> {
        let mut out = Vec::new();
        while let Some(i) = (self.poll)() {
            out.push(i);
        }
        out
    }

    /// Wait until `n` messages matching `keep` arrived (collected in order), or `within` passes.
    pub fn collect_until(
        &mut self,
        within: Duration,
        n: usize,
        keep: impl Fn(&Ingest) -> bool,
    ) -> Vec<Ingest> {
        let deadline = Instant::now() + within;
        let mut out = Vec::new();
        loop {
            while let Some(i) = (self.poll)() {
                if keep(&i) {
                    out.push(i);
                }
            }
            if out.len() >= n || Instant::now() >= deadline {
                return out;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

// ---- helpers every trade suite uses -----------------------------------------------------------------

/// An order the core would submit.
pub fn order(coid: &str) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        price: Some(100.0),
        ts: 1_700_000_000_000,
        ..Default::default()
    }
}

/// Open a lease for `client_id` against `peer` with the fast timings and a fresh core lane.
pub fn open(
    peer: &TradePeer,
    client_id: &str,
) -> (vike_gatewayhub_client::trade::TradeSession, Lane) {
    open_with(peer, client_id, |link| link)
}

/// As [`open`], letting the test adjust the builder.
pub fn open_with(
    peer: &TradePeer,
    client_id: &str,
    adjust: impl FnOnce(
        vike_gatewayhub_client::trade::TradeLink,
    ) -> vike_gatewayhub_client::trade::TradeLink,
) -> (vike_gatewayhub_client::trade::TradeSession, Lane) {
    let (events, lane) = lane();
    let link =
        vike_gatewayhub_client::trade::TradeLink::new(peer.link(), account(client_id), events)
            .with_timings(fast_trade_timings());
    let session = adjust(link).open().expect("the fake peer opens the lease");
    (session, lane)
}

/// Poll until the peer has seen at least `n` lease frames matching `keep`; the matching frames.
pub fn wait_ups(
    peer: &TradePeer,
    within: Duration,
    n: usize,
    keep: impl Fn(&TradeUp) -> bool,
) -> Vec<TradeUp> {
    let mut got = Vec::new();
    TradePeer::wait_until(within, || {
        got = peer.ups().into_iter().filter(|f| keep(f)).collect();
        got.len() >= n
    });
    got
}

/// The terminal rejects in `ingest`, as `(coid, reason)`.
pub fn rejects(ingest: &[Ingest]) -> Vec<(String, String)> {
    ingest
        .iter()
        .filter_map(|i| match i {
            Ingest::Event(Event::OrderRejected(r)) => {
                Some((r.client_order_id.clone(), r.reason.to_string()))
            }
            _ => None,
        })
        .collect()
}

/// The non-terminal cancel advisories in `ingest`, as `(coid, reason)`.
pub fn cancel_advisories(ingest: &[Ingest]) -> Vec<(String, String)> {
    ingest
        .iter()
        .filter_map(|i| match i {
            Ingest::Event(Event::OrderCancelRejected(r)) => {
                Some((r.client_order_id.clone(), r.reason.to_string()))
            }
            _ => None,
        })
        .collect()
}

/// The non-terminal modify advisories in `ingest`, as `(coid, reason)`.
pub fn modify_advisories(ingest: &[Ingest]) -> Vec<(String, String)> {
    ingest
        .iter()
        .filter_map(|i| match i {
            Ingest::Event(Event::OrderModifyRejected(r)) => {
                Some((r.client_order_id.clone(), r.reason.to_string()))
            }
            _ => None,
        })
        .collect()
}

/// An `Event::OrderAccepted` for `coid` naming the venue's order, as a venue's own live accept does: a
/// plain venue event to push through the ring.
pub fn accepted(coid: &str) -> Event {
    Event::OrderAccepted(vike_model::events::OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: Some(format!("v-{coid}").into()),
        ts: 5,
    })
}

/// The resolver's OPTIMISTIC accept (`vike_bridge_core::resolve_ambiguous_submit`'s `Err` arm): it names no
/// venue order, because the lookup failed and nobody knows one.
pub fn accepted_without_id(coid: &str) -> Event {
    Event::OrderAccepted(vike_model::events::OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: None,
        ts: 5,
    })
}
