//! The HUB and the per-account SESSION.
//!
//! [`TradeHub`] is the daemon's one trade state: the lane, the numbers, the epoch and the table of
//! [`AccountSession`]s. [`AccountSession`] is one `(venue, account)`: it owns the venue adapter once the
//! client has started it, the event ring, the threads that drive and drain the adapter and the
//! at-most-one [`Lease`] that is attached to it.
//!
//! # A session outlives its connections
//!
//! A link flap, a tradehub restart and a takeover all end a lease, and none of them ends the session.
//! The adapter keeps running (a resting order is never cancelled because a link dropped: owner ruling
//! D6), the ring keeps collecting what the adapter produces, and the next `TradeOpen` of the account is
//! answered with the cursor it needs. What ends a session is exactly three things: the client's final
//! `Detach` (it is done: the adapter is torn down, the live lock released), an ORPHAN timeout (no lease
//! for `orphan_ttl`: a ghost client does not keep a venue session for ever) and the daemon's stop.
//!
//! # The epoch
//!
//! `gw_epoch` is random per SESSION (minted in [`AccountSession::new`]) and is NOT ordered: it names one
//! ring, one watermark table, one dedup set and one adapter, and all four die with the session, so a
//! session built again after a final `Detach` or an orphan timeout is a NEW epoch even though the daemon
//! process is the same. (A per-process epoch would let a session restart its `seq` at 1 under a cursor
//! `(E, N)` the client still holds: the client's duplicate guard and the core's would drop the new
//! ring's first `N` events, and the empty watermark table would read as "the daemon never received" a
//! Written submit.) Every cursor the ring issues carries it, and a cursor of another epoch is answered
//! as no cursor (`ReplayUnavailable`), never as an old or new one: what it named is gone. A link flap
//! and a takeover keep the session, so they keep the epoch.
//!
//! # Locks
//!
//! Order: the hub's table, then a session's `open_lock`, then its `adapter_lock`, then its `inner`, then
//! a lease's control lock. `inner` is never held across a lane call, a join or a send to another lease.
//!
//! # Opening
//!
//! `TradeHub::open` refuses, in this order and with these words: a bad `client_id`; a venue this build
//! carries no adapter for (`FeatureAbsent`); a `paper` tier (a paper account opens no lease); an account
//! whose rows state no tier (the block `account_tier` names) or another tier than the client claims;
//! (an account table that cannot be READ is none of these: it is a plain error the client retries, never a
//! `TradeRefused`, which it counts);
//! a full lease table; a lease held by another client (`LeaseHeld`, unless it takes over); a live
//! account another process holds (the lock's own `AddrInUse` text). Every refusal and every open is one
//! `TRADE-OPEN:` line.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vike_boot::live_lock::LiveLock;
use vike_config::ArmingBlock;
use vike_exec::Ingest;
use vike_gatewayhub_client::proto::{
    AdapterReply, AdapterStart, ArmingTable, FACTS_BUSY, FACTS_NOT_STARTED, PreflightAnswer,
    PreflightStep, TradeDown, TradeOpened, TradeStatusReport, WireCursor, WireSymbolFacts,
};
use vike_hub_proto::auth::fresh_nonce;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
use vike_model::events::{Event, OrderCancelRejected, OrderModifyRejected, OrderRejected};

mod link;

use super::TradeConfig;
use super::dedup::DedupSet;
use super::dispatch::{self, ExecCmd, ExecCtx};
use super::lane::{TradeLane, TradeTarget};
use super::lease::{ADAPTER_GONE_REASON, Lease, LeaseSeed, NO_ADAPTER_REASON};
use super::recon::{self, ReconCtx, ReconJob, ReconWorker};
use super::ring::{EventRing, Resume, RingEntry};
use super::state_dir::StateDir;
use crate::budget::ledger::Budget;

/// The most `SymbolFacts` lookups the daemon runs at once: a venue call each, on a thread of its own so the
/// lease reader and the exec thread never wait on one; a client asking more is answered a reason.
const MAX_SYMBOL_FACTS_INFLIGHT: usize = 4;

/// One of the `MAX_SYMBOL_FACTS_INFLIGHT` lookup slots, given back when it is dropped (a panic included).
struct FactsSlot(Arc<HubShared>);

impl Drop for FactsSlot {
    fn drop(&mut self) {
        self.0.facts_inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The most client ids whose command watermark a session remembers.
const MAX_WATERMARKS: usize = 16;

/// How long a thread join waits before the thread is left to end on its own.
const JOIN_WAIT: Duration = Duration::from_secs(10);

/// How often the reaper looks at the table.
const REAPER_TICK: Duration = Duration::from_millis(500);

/// A session's key: the venue and the account label's text (`None` is the default account).
pub type AccountKey = (String, Option<String>);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The WIRE spelling of a block: `ArmingBlock::word()`, the only one `ArmingBlock::from_word` reads back (the
/// badge `as_str()` is for a person and is "" for a running row). One function, so every producer of a
/// `block_word` in this file spells it the same way and [`block_runs`] reads the same spelling.
fn word(block: ArmingBlock) -> String {
    block.word().to_string()
}

/// Whether the wire word `word` (`ArmingBlock::word()`: "none" for a clear row) is one under which an account
/// RUNS. An unknown word does not run.
pub fn block_runs(word: &str) -> bool {
    ArmingBlock::from_word(word).is_some_and(ArmingBlock::runs)
}

// ---- opening ---------------------------------------------------------------------------------------

/// A `TradeOpen`, decoded and handed to the hub by the wire adapter.
pub struct OpenRequest {
    /// The caller's id for the lease, stable across its reconnects.
    pub client_id: String,
    /// A `vike_model::VENUES` slug.
    pub venue: String,
    /// The account label's text; `None` is the default account.
    pub account: Option<String>,
    /// The tier the caller believes the account runs at.
    pub tier: AccountTier,
    /// The last event the caller folded.
    pub resume: Option<WireCursor>,
    /// Take the lease over from its holder.
    pub takeover: bool,
    /// The authenticated key's fingerprint.
    pub key_id: Option<String>,
    /// Forces the connection shut (the adapter's socket).
    pub closer: Arc<dyn Fn() + Send + Sync>,
}

/// An accepted open.
pub struct Opened {
    /// The reply to write.
    pub reply: TradeOpened,
    /// The lease the connection now serves.
    pub lease: Arc<Lease>,
}

/// A refused open: what to answer instead of `TradeOpened`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenRefusal {
    /// A positional `Response::Error` (a malformed or unserved request).
    Error(String),
    /// `Response::LeaseHeld`.
    LeaseHeld {
        /// The holder's key id.
        holder_key_id: String,
        /// When the holder took the lease, epoch ms.
        since_ms: i64,
    },
    /// `Response::TradeRefused`: the account's own state.
    Refused {
        /// The `ArmingBlock` word.
        block_word: String,
        /// The words.
        text: String,
    },
}

fn refused(block: ArmingBlock, text: impl Into<String>) -> OpenRefusal {
    OpenRefusal::Refused { block_word: word(block), text: text.into() }
}

/// What a session is shared with: the lane, the numbers, the table.
pub(crate) struct HubShared {
    pub(crate) lane: Arc<dyn TradeLane>,
    pub(crate) cfg: TradeConfig,
    pub(crate) budget: Arc<Budget>,
    pub(crate) state: Option<StateDir>,
    pub(crate) token: OnceLock<String>,
    pub(crate) next_lease: AtomicU64,
    pub(crate) leases: AtomicUsize,
    /// `SymbolFacts` lookups running on their own threads (bounded by `MAX_SYMBOL_FACTS_INFLIGHT`).
    pub(crate) facts_inflight: AtomicUsize,
    pub(crate) stopping: AtomicBool,
    /// How long (ms) a supervised bridge has to announce that it learned the venue before the session is held
    /// (`link::LEARN_BUDGET`; `TradeHub::set_learning_budget`).
    pub(crate) learn_budget_ms: AtomicU64,
    pub(crate) sessions: Mutex<HashMap<AccountKey, Arc<AccountSession>>>,
}

/// The daemon's trade state. See the module doc.
pub struct TradeHub {
    shared: Arc<HubShared>,
    reaper: Mutex<Option<(Arc<AtomicBool>, JoinHandle<()>)>>,
}

impl std::fmt::Debug for TradeHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TradeHub").finish_non_exhaustive()
    }
}

impl TradeHub {
    /// A hub over `lane`. Starts the reaper thread (`gwh-trade-reaper`).
    pub fn new(lane: Arc<dyn TradeLane>, cfg: TradeConfig, budget: Arc<Budget>) -> Arc<Self> {
        let state = cfg.state_dir.clone().map(StateDir::new);
        let shared = Arc::new(HubShared {
            lane,
            cfg,
            budget,
            state,
            token: OnceLock::new(),
            next_lease: AtomicU64::new(1),
            leases: AtomicUsize::new(0),
            facts_inflight: AtomicUsize::new(0),
            stopping: AtomicBool::new(false),
            learn_budget_ms: AtomicU64::new(
                u64::try_from(link::LEARN_BUDGET.as_millis()).unwrap_or(u64::MAX),
            ),
            sessions: Mutex::new(HashMap::new()),
        });
        let hub = Arc::new(Self { shared: Arc::clone(&shared), reaper: Mutex::new(None) });
        let stop = Arc::new(AtomicBool::new(false));
        let spawned = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("gwh-trade-reaper".into())
                .spawn(move || reaper(&shared, &stop))
        };
        match spawned {
            Ok(handle) => *lock(&hub.reaper) = Some((stop, handle)),
            Err(e) => tracing::error!(
                error = %e,
                "TRADE-LINK: could not start the orphan reaper; an unattached session is kept until the daemon stops"
            ),
        }
        hub
    }

    /// How long a supervised venue's bridge (IBKR's socket) has, after its adapter is installed, to announce that
    /// it learned the venue's open orders before the session is HELD with words (`link` module, "Held"); the
    /// default is [`link::LEARN_BUDGET`], past the bridge's own attempts. A test shortens it.
    pub fn set_learning_budget(&self, budget: Duration) {
        self.shared
            .learn_budget_ms
            .store(u64::try_from(budget.as_millis()).unwrap_or(u64::MAX), Ordering::SeqCst);
    }

    /// The venues this build carries an adapter for (`trade_venue=` in `Welcome.features`).
    pub fn venues(&self) -> Vec<String> {
        self.shared.lane.venues()
    }

    /// How many leases are attached now.
    pub fn leases(&self) -> usize {
        self.shared.leases.load(Ordering::SeqCst)
    }

    /// How many sessions exist (attached or orphaned).
    pub fn sessions(&self) -> usize {
        lock(&self.shared.sessions).len()
    }

    /// The daemon's account of one session (its adapter, its ring, its command count), if there is one.
    pub fn session_status(&self, venue: &str, account: Option<&str>) -> Option<TradeStatusReport> {
        let key: AccountKey = (venue.to_string(), account.map(str::to_string));
        lock(&self.shared.sessions).get(&key).map(|s| s.status())
    }

    /// The `ArmingQuery` answer.
    pub fn arming_table(&self) -> ArmingTable {
        self.shared.lane.arming_table()
    }

    /// The `PreflightQuery` answer. `Err` is a refusal in the daemon's words.
    pub fn preflight(
        &self,
        venues: &[String],
        step: PreflightStep,
    ) -> Result<PreflightAnswer, String> {
        const MAX_VENUES: usize = 64;
        if venues.len() > MAX_VENUES {
            return Err(format!(
                "gatewayhub: a preflight of {} venues (at most {MAX_VENUES})",
                venues.len()
            ));
        }
        let served = self.venues();
        if let Some(bad) = venues.iter().find(|v| v.len() > super::lease::MAX_NAME_BYTES) {
            return Err(format!("gatewayhub: a venue name of {} bytes", bad.len()));
        }
        if let Some(bad) = venues.iter().find(|v| !served.contains(v)) {
            return Err(format!(
                "gatewayhub: `{bad}` has no trade adapter in this build (served: [{}]); nothing was probed",
                served.join(", ")
            ));
        }
        Ok(self.shared.lane.preflight(venues, step))
    }

    /// Answer a `TradeOpen`. See the module doc for the refusals, in order.
    pub fn open(&self, req: OpenRequest) -> Result<Opened, OpenRefusal> {
        let out = self.open_inner(req);
        match &out {
            Ok(opened) => tracing::info!(
                lease = opened.lease.id,
                client = %opened.lease.client_id,
                key_id = %opened.lease.key_id,
                epoch = opened.reply.gw_epoch,
                through_cseq = opened.reply.received_through_cseq,
                ring_floor = opened.reply.event_ring_floor,
                "TRADE-OPEN: lease granted"
            ),
            Err(refusal) => tracing::warn!("TRADE-OPEN: refused: {refusal:?}"),
        }
        out
    }

    fn open_inner(&self, req: OpenRequest) -> Result<Opened, OpenRefusal> {
        let sh = &self.shared;
        if sh.stopping.load(Ordering::SeqCst) {
            return Err(OpenRefusal::Error("gatewayhub: the daemon is stopping".to_string()));
        }
        if req.client_id.is_empty() || req.client_id.len() > super::lease::MAX_NAME_BYTES * 2 {
            return Err(OpenRefusal::Error(format!(
                "gatewayhub: a client id of {} bytes (1 to {})",
                req.client_id.len(),
                super::lease::MAX_NAME_BYTES * 2
            )));
        }
        let served = self.venues();
        if !served.contains(&req.venue) {
            return Err(refused(
                ArmingBlock::FeatureAbsent,
                format!(
                    "gatewayhub: no trade adapter for `{}` in this build (trade_venue= advertises: [{}])",
                    req.venue.chars().take(super::lease::MAX_NAME_BYTES).collect::<String>(),
                    served.join(", ")
                ),
            ));
        }
        if let Some(text) = &req.account
            && let Err(e) = AccountLabel::parse(text)
        {
            return Err(OpenRefusal::Error(format!("gatewayhub: a bad account label: {e}")));
        }
        if req.tier == AccountTier::Paper {
            return Err(refused(
                ArmingBlock::PaperTier,
                "gatewayhub: a paper account runs the simulator and opens no trade lease",
            ));
        }
        // A supervised venue session that is down takes no lease: the client's link must stay DOWN (its
        // `ExecLink`, the strategies' `Disconnected`) until the venue is back, and a lease attached now would
        // announce `Live` for a venue that is not there. Asked before the account table is read: it needs
        // neither the store nor the session's locks.
        let known =
            lock(&self.shared.sessions).get(&(req.venue.clone(), req.account.clone())).cloned();
        if let Some(refusal) = known.and_then(|s| s.link_refuses_open()) {
            return Err(refusal);
        }
        // The tier is re-derived from the account table, never taken from the client.
        match sh.lane.account_tier(&req.venue, req.account.as_deref()) {
            // `account_tier` answers `AccountNotInStore` for ONE thing: the account table could not be READ
            // (a locked or unopenable store), which says nothing about the account. A plain error, NOT a
            // `TradeRefused`, for the reason `link_refuses_open` gives: the client counts refusals and closes
            // its lease for good after three of them, stranding a running adapter with no tradehub; an error
            // is retried on the dial schedule. The definitive answers below stay refusals.
            Err(ArmingBlock::AccountNotInStore) => {
                tracing::error!(
                    "TRADE-OPEN: {}: the account table could not be read; the open is not judged",
                    subject(&req.venue, req.account.as_deref())
                );
                return Err(OpenRefusal::Error(format!(
                    "gatewayhub: the account table could not be read just now, so {} was not \
                     judged; open again when the settings database reads",
                    subject(&req.venue, req.account.as_deref())
                )));
            }
            Err(block) => {
                return Err(refused(
                    block,
                    format!(
                        "gatewayhub: {} does not run: {}",
                        subject(&req.venue, req.account.as_deref()),
                        block.as_str()
                    ),
                ));
            }
            Ok(tier) if tier != req.tier => {
                return Err(refused(
                    ArmingBlock::AdapterFailed,
                    format!(
                        "gatewayhub: the account table says {} runs at {tier:?} and the client opened it at {:?}; refused",
                        subject(&req.venue, req.account.as_deref()),
                        req.tier
                    ),
                ));
            }
            Ok(_) => {}
        }
        loop {
            let session = self.session_for(&req);
            match session.attach(&req) {
                Err(Attach::Retired) => continue,
                // Down: the session is being rebuilt. NOT retired by this refusal (it has no adapter, and
                // the supervisor is what brings it back).
                Err(Attach::Down(r)) => return Err(r),
                Err(Attach::Refused(r)) => {
                    session.retire_if("the open was refused", |i| i.adapter.is_none());
                    return Err(r);
                }
                Ok(opened) => return Ok(opened),
            }
        }
    }

    fn session_for(&self, req: &OpenRequest) -> Arc<AccountSession> {
        let key: AccountKey = (req.venue.clone(), req.account.clone());
        let mut table = lock(&self.shared.sessions);
        if let Some(existing) = table.get(&key) {
            return Arc::clone(existing);
        }
        let session = AccountSession::new(Arc::clone(&self.shared), key.clone(), req.tier);
        table.insert(key, Arc::clone(&session));
        session
    }

    /// Stop everything: end every lease with `Bye(ServerStopping)`, tear every adapter down (a resting
    /// order is not cancelled: that is the client's own sweep), stop the reaper. Idempotent.
    pub fn shutdown(&self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        if let Some((stop, handle)) = lock(&self.reaper).take() {
            stop.store(true, Ordering::SeqCst);
            join_bounded(handle, JOIN_WAIT, "gwh-trade-reaper");
        }
        let sessions: Vec<Arc<AccountSession>> =
            lock(&self.shared.sessions).values().cloned().collect();
        for session in &sessions {
            if let Some(lease) = lock(&session.inner).lease.clone() {
                lease.stop();
            }
        }
        for session in sessions {
            // Wait for the connection to say goodbye before the adapter goes: the lease's own thread
            // calls `finish`, which clears the hold.
            let lease = lock(&session.inner).lease.clone();
            if let Some(lease) = lease {
                lease.wait_finished(Duration::from_secs(3));
            }
            session.retire_force("the daemon is stopping");
        }
    }
}

impl Drop for TradeHub {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn subject(venue: &str, account: Option<&str>) -> String {
    match account {
        None => venue.to_string(),
        Some(label) => format!("{venue} account `{label}`"),
    }
}

fn reaper(shared: &Arc<HubShared>, stop: &Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(REAPER_TICK);
        let sessions: Vec<Arc<AccountSession>> = lock(&shared.sessions).values().cloned().collect();
        let ttl = shared.cfg.orphan_ttl;
        for session in sessions {
            let reaped =
                session.retire_if("no client has held the lease for the orphan timeout", |i| {
                    // A session whose adapter the supervisor took down is waiting to bring it back: it
                    // is not an orphan until the orphan timeout says so.
                    (i.adapter.is_none() && i.supervised.is_none())
                        || i.detached_since.is_some_and(|t| t.elapsed() >= ttl)
                });
            if reaped {
                tracing::warn!(
                    venue = %session.key.0,
                    "TRADE-LINK: an orphaned session was torn down (no client held its lease)"
                );
            }
        }
    }
}

// ---- the session -------------------------------------------------------------------------------------

/// What [`AccountSession::pull`] found at a `seq`.
pub(crate) enum Pulled {
    /// The event, with the account spelling and epoch its frame carries.
    Event { entry: RingEntry, account: Arc<str>, gw_epoch: u64 },
    /// The ring evicted that `seq`: the oldest held is `floor`.
    Gap { floor: u64 },
    /// Not produced yet.
    Nothing,
}

enum Attach {
    /// The session was retired while the open waited: build a fresh one.
    Retired,
    /// The venue session went down while the open waited (see `link_refuses_open`).
    Down(OpenRefusal),
    Refused(OpenRefusal),
}

struct Adapter {
    start: AdapterStart,
    reply: AdapterReply,
    bound_tier: AccountTier,
    cmd_tx: SyncSender<ExecCmd>,
    recon: Option<ReconWorker>,
    exec: Option<JoinHandle<()>>,
    /// Set by the dispatch thread when the bridge panicked (see `dispatch::run`), and by the supervisor when
    /// it takes the adapter down for a lost venue link: every command read from then on is answered, not
    /// dispatched.
    dead: Arc<AtomicBool>,
    /// What the bridge said when it disclosed its venue link gone (`ExecLink{up:false}` on its event lane,
    /// caught by the forwarder); `None` while it has said nothing. The supervisor reads it: a bridge that
    /// reports its own death is acted on at once (the IBKR bridge does, W14c B), and the supervisor's own
    /// probes remain the fallback for one that never notices.
    reported: Arc<Mutex<Option<String>>>,
    /// What the bridge said about its LEARNING of the venue: `ExecLink{up:true}` (its connection has learned the
    /// venue's open orders) and the coids it announced since it started (`link::BridgeLearning`). A rebuilt
    /// session held for orders it could not manage is released by the first, judged by the second.
    learning: Arc<link::BridgeLearning>,
    forwarder: Option<JoinHandle<()>>,
    trigger: Option<JoinHandle<()>>,
}

struct Inner {
    tier: AccountTier,
    ring: EventRing,
    route_key: Arc<str>,
    watermarks: HashMap<String, u64>,
    lease: Option<Arc<Lease>>,
    adapter: Option<Adapter>,
    live_lock: Option<LiveLock>,
    detached_since: Option<Instant>,
    pending_reconcile: Option<String>,
    commands: u64,
    foreign_ingest_warned: bool,
    /// The state of the supervised venue session (`link` module); `Up` for every venue that has none.
    link: link::LinkState,
    /// The start the supervisor replays when it brings a lost adapter back (set while `link` is `Down`).
    parked: Option<AdapterStart>,
    /// The stop flag of the supervisor thread watching this session's venue link, while one runs.
    supervised: Option<Arc<AtomicBool>>,
    /// The orders this SUPERVISED session sent toward its venue and has not seen finish (`link` module): the
    /// daemon's only knowledge of what may rest at the venue across a rebuild of the adapter.
    resting: link::Resting,
    /// Set when an adapter was rebuilt while `resting` held orders: the words every order verb is refused
    /// with from then on (`link` module). It lasts as long as the session.
    held: Option<String>,
    /// The ledger's coids when the hold was set: what the bridge has to ACCOUNT for before it is lifted.
    held_coids: std::collections::HashSet<String>,
    /// The ledger had overflowed when the hold was set (so it cannot be accounted for).
    held_overflowed: bool,
    /// Set by a restore: the ring is a NEW epoch whose first events were pushed by the rebuilt adapter before
    /// any lease attached; the first lease takes them (see `attach`).
    carry: bool,
    /// An adapter was installed in this session before (so the next one REPLACES a bridge: `link::hold_if_blind`).
    had_adapter: bool,
}

/// One `(venue, account)`. See the module doc.
pub struct AccountSession {
    shared: Arc<HubShared>,
    key: AccountKey,
    open_lock: Mutex<()>,
    adapter_lock: Mutex<()>,
    retired: AtomicBool,
    inner: Mutex<Inner>,
}

impl AccountSession {
    fn new(shared: Arc<HubShared>, key: AccountKey, tier: AccountTier) -> Arc<Self> {
        let route_key: Arc<str> = match &key.1 {
            None => Arc::from(key.0.as_str()),
            Some(label) => Arc::from(format!("{}#{label}", key.0)),
        };
        let ring = EventRing::new(mint_epoch(), shared.cfg.ring_events);
        Arc::new(Self {
            shared,
            key,
            open_lock: Mutex::new(()),
            adapter_lock: Mutex::new(()),
            retired: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                tier,
                ring,
                route_key,
                watermarks: HashMap::new(),
                lease: None,
                adapter: None,
                live_lock: None,
                detached_since: None,
                pending_reconcile: None,
                commands: 0,
                foreign_ingest_warned: false,
                link: link::LinkState::Up,
                parked: None,
                supervised: None,
                resting: link::Resting::default(),
                held: None,
                held_coids: Default::default(),
                held_overflowed: false,
                carry: false,
                had_adapter: false,
            }),
        })
    }

    fn target(&self, tier: AccountTier) -> TradeTarget {
        TradeTarget { venue: self.key.0.clone(), account: self.key.1.clone(), tier }
    }

    pub(crate) fn hub_stopping(&self) -> bool {
        self.shared.stopping.load(Ordering::SeqCst)
    }

    pub(crate) fn lease_is_current(&self, id: u64) -> bool {
        lock(&self.inner).lease.as_ref().is_some_and(|l| l.id == id)
    }

    /// Whether `order` names another venue or account than this session's. `Some(reason)` = refuse it.
    /// Only a CONFLICT refuses: an empty venue and an order naming no account are the core's ordinary
    /// shapes (the lease IS the routing), so they pass.
    pub(crate) fn identity_conflict(&self, order: &vike_model::OrderRequest) -> Option<String> {
        let want = match &self.key.1 {
            None => AccountLabel::Default,
            Some(text) => AccountLabel::parse(text).unwrap_or(AccountLabel::Default),
        };
        let wrong_venue = !order.venue.is_empty() && order.venue != self.key.0;
        let wrong_account = order.account.as_ref().is_some_and(|a| *a != want);
        (wrong_venue || wrong_account).then(|| {
            format!(
                "gatewayhub: the order names another venue or account than this lease ({})",
                subject(&self.key.0, self.key.1.as_deref())
            )
        })
    }

    /// The live lock's name: the same route key `AdapterStart.route_key` carries.
    fn lock_key(&self) -> String {
        let label = match &self.key.1 {
            None => AccountLabel::Default,
            Some(text) => AccountLabel::parse(text).unwrap_or(AccountLabel::Default),
        };
        route_key_of(&self.key.0, &label)
    }

    /// Whether this account is an ARMED LIVE route key (the lane's table says it runs at `live`).
    fn armed_live(&self, tier: AccountTier) -> bool {
        if tier != AccountTier::Live {
            return false;
        }
        let table = self.shared.lane.arming_table();
        table.rows.iter().any(|r| {
            r.venue == self.key.0
                && r.label == self.key.1
                && r.tier == Some(AccountTier::Live)
                && r.effective == Some(AccountTier::Live)
                && block_runs(&r.block_word)
        })
    }

    fn attach(self: &Arc<Self>, req: &OpenRequest) -> Result<Opened, Attach> {
        let sh = Arc::clone(&self.shared);
        let _open = lock(&self.open_lock);
        if self.retired.load(Ordering::SeqCst) {
            return Err(Attach::Retired);
        }
        // The supervisor takes `open_lock` to put the session down, so an open that waited behind it sees the
        // state it left.
        if let Some(refusal) = self.link_refuses_open() {
            return Err(Attach::Down(refusal));
        }
        // The previous holder, if any, is dealt with BEFORE this open is answered.
        let old = lock(&self.inner).lease.clone();
        if let Some(old) = old {
            if old.is_ended() {
                old.wait_finished(sh.cfg.fence_drain);
            } else {
                if old.client_id != req.client_id && !req.takeover {
                    return Err(Attach::Refused(OpenRefusal::LeaseHeld {
                        holder_key_id: old.key_id.clone(),
                        since_ms: old.since_ms,
                    }));
                }
                if old.client_id != req.client_id {
                    tracing::warn!(
                        old_client = %old.client_id,
                        old_key_id = %old.key_id,
                        by_key_id = req.key_id.as_deref().unwrap_or("-"),
                        "TRADE-LINK: lease taken over; the previous holder is told and fenced"
                    );
                    old.bye(vike_gatewayhub_client::proto::TradeBye::Takeover {
                        by_key_id: req.key_id.clone().unwrap_or_default(),
                    });
                } else {
                    tracing::info!(
                        client = %old.client_id,
                        "TRADE-LINK: the holder reconnected; its previous connection is read to its end first"
                    );
                }
                old.fence();
                if !old.wait_finished(sh.cfg.fence_drain) {
                    tracing::warn!(
                        client = %old.client_id,
                        "TRADE-LINK: the previous connection did not finish inside the fence; closing it"
                    );
                    old.force_close();
                    old.wait_finished(Duration::from_secs(2));
                }
            }
        }
        // A running adapter runs at ONE tier.
        {
            let inner = lock(&self.inner);
            if inner.adapter.is_some() && inner.tier != req.tier {
                return Err(Attach::Refused(refused(
                    ArmingBlock::AdapterFailed,
                    format!(
                        "gatewayhub: the adapter for {} runs at {:?}; it cannot be opened at {:?} until its holder detaches",
                        subject(&self.key.0, self.key.1.as_deref()),
                        inner.tier,
                        req.tier
                    ),
                )));
            }
        }
        // The lease table has a bound.
        if sh.leases.load(Ordering::SeqCst) >= sh.cfg.max_leases
            && lock(&self.inner).lease.is_none()
        {
            return Err(Attach::Refused(OpenRefusal::Error(format!(
                "gatewayhub: already serving {} trade leases (the daemon's cap)",
                sh.cfg.max_leases
            ))));
        }
        // One live process per account.
        let holds_lock = lock(&self.inner).live_lock.is_some();
        let needs_lock = !holds_lock && self.armed_live(req.tier);
        if needs_lock {
            let Some(state) = &sh.state else {
                return Err(Attach::Refused(refused(
                    ArmingBlock::AdapterFailed,
                    "gatewayhub: a live account needs the live-account lock and no project state directory is resolved; refused",
                )));
            };
            match state.claim_live(&self.lock_key()) {
                Ok(guard) => lock(&self.inner).live_lock = Some(guard),
                Err(e) => {
                    // A lease refusal, not an account's: the whole live mount is refused (the tradehub
                    // records a `LeaseHeld` into `lease_refusals`; a `TradeRefused` is only this
                    // account's), as the lock's `AddrInUse` did when the tradehub claimed it. The
                    // lock's own sentence rides in the holder field, the one string a `LeaseHeld`
                    // carries.
                    tracing::error!(
                        route_key = %self.lock_key(),
                        "TRADE-OPEN: the live-account lock was refused: {e}"
                    );
                    return Err(Attach::Refused(OpenRefusal::LeaseHeld {
                        holder_key_id: format!("the live-account lock: {e}"),
                        since_ms: now_ms(),
                    }));
                }
            }
        }
        let token = sh
            .token
            .get_or_init(|| match sh.state.as_ref().map(StateDir::project_token) {
                Some(Ok(token)) => token,
                Some(Err(why)) => {
                    tracing::warn!("TRADE-OPEN: no project token: {why}");
                    String::new()
                }
                None => String::new(),
            })
            .clone();

        let id = sh.next_lease.fetch_add(1, Ordering::SeqCst);
        let mut inner = lock(&self.inner);
        let last = inner.ring.last_seq();
        let plan = inner.ring.plan_resume(req.resume.as_ref());
        let (mut next_seq, gap) = match plan {
            Resume::Fresh => (last + 1, None),
            Resume::Replay { from } => (from, None),
            Resume::Unavailable { floor } => (last + 1, Some(floor)),
        };
        // The first lease after a restore starts at the new ring's first event: what the rebuilt adapter pushed
        // while nobody was attached (a bridge may replay events the moment it connects) is the first lease's,
        // not skipped as "before it attached". The client's duplicate guards drop what its reconcile also finds.
        if std::mem::take(&mut inner.carry)
            && inner.ring.floor() != 0
            && !matches!(plan, Resume::Replay { .. })
        {
            next_seq = inner.ring.floor();
        }
        let lease = Lease::new(
            Arc::clone(self),
            LeaseSeed {
                id,
                client_id: req.client_id.clone(),
                key_id: req.key_id.clone().unwrap_or_default(),
                since_ms: now_ms(),
                next_seq,
                live_from: last + 1,
                control_cap: sh.cfg.control_queue,
                closer: Arc::clone(&req.closer),
            },
        );
        if let Some(floor) = gap {
            lease.send(TradeDown::ReplayUnavailable { floor });
        }
        if let Some(reason) = inner.pending_reconcile.take() {
            lease.send(TradeDown::ReconcileNow { reason });
        }
        let reply = TradeOpened {
            gw_epoch: inner.ring.gw_epoch(),
            lease_epoch: id,
            received_through_cseq: inner.watermarks.get(&req.client_id).copied().unwrap_or(0),
            project_token: token,
            heartbeat_ms: u64::try_from(sh.cfg.heartbeat.as_millis()).unwrap_or(u64::MAX),
            event_ring_floor: Some(inner.ring.floor()),
        };
        let replaced = inner.lease.replace(Arc::clone(&lease)).is_some();
        inner.tier = req.tier;
        inner.detached_since = None;
        drop(inner);
        if !replaced {
            sh.leases.fetch_add(1, Ordering::SeqCst);
        }
        Ok(Opened { reply, lease })
    }

    /// The lease's connection is over: release the hold (unless a newer lease already replaced it).
    pub(crate) fn lease_ended(&self, lease: &Lease) {
        let mut inner = lock(&self.inner);
        if inner.lease.as_ref().is_some_and(|l| l.id == lease.id) {
            inner.lease = None;
            inner.detached_since = Some(Instant::now());
            drop(inner);
            self.shared.leases.fetch_sub(1, Ordering::SeqCst);
            tracing::info!(
                client = %lease.client_id,
                "TRADE-LINK: lease released; the adapter keeps running with no client until one reconnects"
            );
        }
    }

    // ---- inbound -----------------------------------------------------------------------------------

    /// The watermark moves when a command frame is READ, before anything else is done with it.
    pub(crate) fn note_cseq(&self, client_id: &str, cseq: u64) {
        let mut inner = lock(&self.inner);
        inner.commands += 1;
        if inner.watermarks.len() >= MAX_WATERMARKS && !inner.watermarks.contains_key(client_id) {
            inner.watermarks.clear();
        }
        let slot = inner.watermarks.entry(client_id.to_string()).or_insert(0);
        *slot = (*slot).max(cseq);
    }

    /// Queue `cmd` for the adapter's dispatch thread, waiting while the queue is full (back-pressure on
    /// the client's writer) as long as `alive` holds. `Err` hands the command back: there is no adapter,
    /// its thread is gone or the wait was abandoned.
    pub(crate) fn enqueue(&self, cmd: ExecCmd, alive: impl Fn() -> bool) -> Result<(), ExecCmd> {
        let tx = {
            let mut inner = lock(&self.inner);
            // A supervised venue session that is down takes no command: it is answered (below, by
            // `synthesize`) with the words of why, instead of reaching an adapter the daemon tore down.
            if !inner.link.is_up() {
                return Err(cmd);
            }
            // A session whose rebuilt adapter cannot manage the orders of the one before it takes no order
            // verb either (`link` module, "Held").
            if inner.held.is_some() && link::carries_order(&cmd) {
                return Err(cmd);
            }
            if inner.supervised.is_some() {
                match &cmd {
                    ExecCmd::Submit(order) => {
                        inner.resting.sent(&order.client_order_id, order.qty, &order.symbol)
                    }
                    ExecCmd::SubmitBatch(orders) => {
                        for order in orders {
                            inner.resting.sent(&order.client_order_id, order.qty, &order.symbol);
                        }
                    }
                    _ => {}
                }
            }
            inner.adapter.as_ref().map(|a| a.cmd_tx.clone())
        };
        let Some(tx) = tx else { return Err(cmd) };
        let mut cmd = cmd;
        loop {
            match tx.try_send(cmd) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(c)) => return Err(c),
                Err(TrySendError::Full(c)) => {
                    if !alive() {
                        return Err(c);
                    }
                    cmd = c;
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    /// A command that could not reach an adapter: the event the contract owes goes in the ring (in order
    /// with the venue's own). A submit is a TERMINAL reject, so no order silently vanishes.
    pub(crate) fn synthesize(&self, cmd: ExecCmd) {
        let reason: String =
            {
                let inner = lock(&self.inner);
                if let Some(words) = inner.link.reason(&self.key.0) {
                    words
                } else if let Some(held) = inner.held.as_ref().filter(|_| {
                    inner.adapter.as_ref().is_some_and(|a| !a.dead.load(Ordering::SeqCst))
                }) {
                    held.clone()
                } else if inner.adapter.is_some() {
                    ADAPTER_GONE_REASON.to_string()
                } else {
                    NO_ADAPTER_REASON.to_string()
                }
            };
        let reject = |order: &vike_model::OrderRequest| {
            Event::OrderRejected(OrderRejected {
                client_order_id: order.client_order_id.clone(),
                reason: reason.as_str().into(),
                ts: order.ts,
            })
        };
        let cancel_rejected = |coid: &str| {
            Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: coid.to_string(),
                reason: reason.as_str().into(),
                ts: 0,
            })
        };
        match cmd {
            ExecCmd::Submit(order) => {
                self.emit_event(reject(&order));
            }
            ExecCmd::SubmitBatch(orders) => {
                for order in &orders {
                    self.emit_event(reject(order));
                }
            }
            ExecCmd::Cancel { coid, .. } => {
                self.emit_event(cancel_rejected(&coid));
            }
            ExecCmd::CancelBatch { coids, .. } => {
                for coid in &coids {
                    self.emit_event(cancel_rejected(coid));
                }
            }
            ExecCmd::Modify { order, .. } => {
                self.emit_event(Event::OrderModifyRejected(OrderModifyRejected {
                    client_order_id: order.client_order_id.clone(),
                    reason: reason.as_str().into(),
                    ts: order.ts,
                }));
            }
            // A re-query, a teaching, an answer and a stop carry no order: nothing to owe.
            ExecCmd::Confirm(_)
            | ExecCmd::Adopt(_)
            | ExecCmd::WireIds { .. }
            | ExecCmd::Resolve { .. }
            | ExecCmd::Stop => {}
        }
    }

    /// Append an event to the ring and wake the attached lease. The `seq` is stamped here, under the
    /// ring's lock, before any eviction.
    pub(crate) fn emit_event(&self, event: Event) -> u64 {
        let ts = now_ms();
        let (seq, lease) = {
            let mut inner = lock(&self.inner);
            inner.resting.finished(&event);
            (inner.ring.push(ts, event), inner.lease.clone())
        };
        if let Some(lease) = lease {
            lease.wake_events();
        }
        seq
    }

    /// The ring entry at `seq`, for a lease's writer.
    pub(crate) fn pull(&self, seq: u64) -> Pulled {
        let inner = lock(&self.inner);
        if seq > inner.ring.last_seq() {
            return Pulled::Nothing;
        }
        match inner.ring.get(seq) {
            Some(entry) => Pulled::Event {
                entry: entry.clone(),
                account: Arc::clone(&inner.route_key),
                gw_epoch: inner.ring.gw_epoch(),
            },
            None => Pulled::Gap { floor: inner.ring.floor() },
        }
    }

    /// Ask the attached client for a reconcile pass now (the bridge's `recon_trigger`). With no client
    /// attached the request waits for the next one.
    pub(crate) fn reconcile_now(&self, reason: &str) {
        let lease = {
            let mut inner = lock(&self.inner);
            // A lease whose connection is already going away reads nothing more: the request waits for the
            // next one instead of being sent into it and lost.
            match inner.lease.clone().filter(|l| !l.is_ended()) {
                Some(l) => Some(l),
                None => {
                    inner.pending_reconcile = Some(reason.to_string());
                    None
                }
            }
        };
        if let Some(lease) = lease {
            lease.send(TradeDown::ReconcileNow { reason: reason.to_string() });
        }
    }

    pub(crate) fn recon_tx(&self) -> Option<Sender<ReconJob>> {
        lock(&self.inner).adapter.as_ref().and_then(|a| a.recon.as_ref().map(|r| r.tx.clone()))
    }

    /// The lane's word on this venue's price feed (R31): `Some(reason)` while it is mid-gap.
    pub(crate) fn recon_degraded(&self) -> Option<String> {
        self.shared.lane.recon_degraded(&self.key.0)
    }

    pub(crate) fn recon_unavailable_reason(&self) -> String {
        if lock(&self.inner).adapter.is_some() {
            "the venue adapter has no reconcile handle".to_string()
        } else {
            "no venue adapter has been started for this account yet".to_string()
        }
    }

    /// `IdentityRecord`: through the reconcile worker when there is one, else with no handle.
    pub(crate) fn identity_record(&self, rid: u64, lease: &Arc<Lease>) {
        let (tx, bound) = {
            let inner = lock(&self.inner);
            match inner.adapter.as_ref() {
                None => (None, None),
                Some(a) => (a.recon.as_ref().map(|r| r.tx.clone()), Some(a.bound_tier)),
            }
        };
        let Some(bound) = bound else {
            lease.send(TradeDown::IdentityRecorded {
                rid,
                ok: false,
                note: "no venue adapter has been started for this account yet".to_string(),
            });
            return;
        };
        if let Some(tx) = tx
            && tx.send(ReconJob::Identity { rid, to: Arc::clone(lease) }).is_ok()
        {
            return;
        }
        let inner_tier = lock(&self.inner).tier;
        let outcome = self.shared.lane.record_identity(&self.target(inner_tier), bound, None);
        let (ok, note) = match outcome {
            Ok(()) => (true, String::new()),
            Err(why) => (false, why),
        };
        lease.send(TradeDown::IdentityRecorded { rid, ok, note });
    }

    /// `SymbolFacts`: the lane's public venue lookup, on a short-lived thread (it may be a venue HTTP call,
    /// and neither the lease reader nor the exec thread may wait on one). Needs no started adapter.
    ///
    /// A **commit** (`probe` false: the node sent `AdmitSymbol` for the symbol) whose answer is GRANTED also
    /// tells the account's reconcile client to read the symbol from now on (`ReconClient::admit_symbol`);
    /// a probe, a refusal, or an account whose adapter is not started tells it nothing.
    ///
    /// A granted commit is also the one request that may ask the VENUE something
    /// ([`Self::with_symbol_leverage`]): for a venue that posts a leverage, the account's figure is made
    /// real for the admitted symbol and the reply carries the figure the venue was SEEN holding. A look
    /// never does (a Preview must change nothing at the venue), and the lookup slot is held until the
    /// reply is built, so the venue calls are bounded by the same four slots.
    ///
    /// A node that CONVERTS a quote currency (`quoted`, decision 0183) is answered through
    /// `TradeLane::symbol_facts_quoted`: a symbol quoted in another currency than the account's comes back
    /// with its rate instead of being refused; every other node is answered by `TradeLane::symbol_facts`,
    /// whose fence refuses it.
    pub(crate) fn symbol_facts(
        self: &Arc<Self>,
        rid: u64,
        symbol: String,
        probe: bool,
        quoted: bool,
        lease: &Arc<Lease>,
    ) {
        let refuse = |why: &str| {
            lease.send(TradeDown::SymbolFactsReply { rid, facts: Err(why.to_string()) });
        };
        let inflight = &self.shared.facts_inflight;
        if inflight.fetch_add(1, Ordering::SeqCst) >= MAX_SYMBOL_FACTS_INFLIGHT {
            inflight.fetch_sub(1, Ordering::SeqCst);
            return refuse(FACTS_BUSY);
        }
        let target = self.target(lock(&self.inner).tier);
        let (session, to) = (Arc::clone(self), Arc::clone(lease));
        let spawned = std::thread::Builder::new().name("gwh-trade-facts".into()).spawn(move || {
            // Released by DROP, so a lane that panics cannot leak the slot (four leaks would end every
            // later lookup with "too many running").
            let slot = FactsSlot(Arc::clone(&session.shared));
            let facts = if quoted {
                session.shared.lane.symbol_facts_quoted(&target, &symbol)
            } else {
                session.shared.lane.symbol_facts(&target, &symbol)
            };
            // Queued BEFORE the reply goes out: the worker is one FIFO and the node's next `Recon`
            // request can only follow the reply, so no pass ever runs between an answer and its
            // admission. The worker is read NOW, after the lookup (which can take seconds), not before
            // it: an adapter restarted meanwhile has a new worker, and the cue must reach that one.
            // Best effort: with no worker there is no pass left to widen.
            if !probe
                && facts.is_ok()
                && let Some(tx) = session.recon_tx()
            {
                let _ = tx.send(ReconJob::Admit { symbol: symbol.clone() });
            }
            // AFTER the reconcile cue, so a venue that is slow to answer about leverage delays only
            // this reply, never the symbol's reading.
            let facts = match facts {
                Ok(facts) if !probe => Ok(session.with_symbol_leverage(&target, &symbol, facts)),
                other => other,
            };
            drop(slot);
            to.send(TradeDown::SymbolFactsReply { rid, facts });
        });
        if spawned.is_err() {
            inflight.fetch_sub(1, Ordering::SeqCst);
            refuse(FACTS_NOT_STARTED);
        }
    }

    /// `facts` with the leverage the venue confirmed for `symbol`, when the lane has one
    /// ([`TradeLane::symbol_leverage`]): asked of the lane with the RUNNING adapter's own start (its
    /// `account_leverage` is the figure the node asked). No adapter, or a dead one, asks nothing. A
    /// refusal or failure leaves `facts` as the lookup made them and says why in the daemon's log, so the
    /// node leaves the symbol at the engine's rate: never a half-applied or clamped figure.
    fn with_symbol_leverage(
        &self,
        target: &TradeTarget,
        symbol: &str,
        mut facts: WireSymbolFacts,
    ) -> WireSymbolFacts {
        let start = lock(&self.inner)
            .adapter
            .as_ref()
            .filter(|a| !a.dead.load(Ordering::SeqCst))
            .map(|a| a.start.clone());
        let Some(start) = start else { return facts };
        match self.shared.lane.symbol_leverage(target, &start, symbol) {
            None => {}
            Some(Ok(figure)) => {
                tracing::info!(
                    venue = %target.venue, account = target.account.as_deref().unwrap_or("default"),
                    %symbol,
                    "{}: {symbol} is held at {figure}x on the venue (read back): the node may margin \
                     it at that figure",
                    target.venue
                );
                facts.leverage = Some(figure);
            }
            Some(Err(why)) => tracing::warn!(
                venue = %target.venue, account = target.account.as_deref().unwrap_or("default"),
                %symbol,
                "{}: the account's leverage ({}x) is NOT confirmed for {symbol}: {why}. The symbol is \
                 admitted as before and the engine keeps its own rate for it",
                target.venue,
                start.account_leverage.unwrap_or_default()
            ),
        }
        facts
    }

    /// The daemon's account of this session.
    pub(crate) fn status(&self) -> TradeStatusReport {
        let inner = lock(&self.inner);
        TradeStatusReport {
            adapter_up: inner.link.is_up()
                && inner.adapter.as_ref().is_some_and(|a| !a.dead.load(Ordering::SeqCst)),
            last_seq: inner.ring.last_seq(),
            ring_floor: inner.ring.floor(),
            commands_received: inner.commands,
        }
    }

    // ---- the adapter -------------------------------------------------------------------------------

    /// Start (or, for the same request, confirm) the account's venue adapter. Runs on the lease's reader
    /// thread and may take as long as a venue login takes; the writer keeps the heartbeat going.
    pub(crate) fn start_adapter(self: &Arc<Self>, start: AdapterStart) -> AdapterReply {
        // Before the adapter lock: a supervised session that is down is being brought back by the
        // daemon itself, and the supervisor holds that lock while it does.
        if let Some(refusal) = self.link_refuses_start() {
            return refusal;
        }
        let _adapter = lock(&self.adapter_lock);
        self.start_adapter_locked(start, false)
    }

    /// [`Self::start_adapter`] with `adapter_lock` held by the caller. `restore` = the supervisor's own
    /// restart of a lost adapter (`link` module): it asks the lane for no link to watch (it IS the watch) and
    /// leaves the link state and the parked start alone, because the restore marks the session up itself,
    /// after it has changed the epoch. A restore is called WITHOUT `adapter_lock`: it takes it itself, after
    /// the venue's login, to install the adapter (a login can take long and the daemon's stop must not wait
    /// for it).
    fn start_adapter_locked(self: &Arc<Self>, start: AdapterStart, restore: bool) -> AdapterReply {
        let sh = Arc::clone(&self.shared);
        let tier = lock(&self.inner).tier;
        let running_reply = {
            let inner = lock(&self.inner);
            inner
                .adapter
                .as_ref()
                .filter(|running| {
                    !running.dead.load(Ordering::SeqCst) && same_start(&running.start, &start)
                })
                .map(|running| running.reply.clone())
        };
        if let Some(mut reply) = running_reply {
            tracing::info!(
                venue = %self.key.0,
                "TRADE-OPEN: the adapter outlived the link; the same start is answered from the running one"
            );
            // The venue's word on the account's leverage is asked afresh (decision 0181), with no lock
            // held: the row may have been set after this adapter came up, and a cached answer would
            // read "no confirmation" and refuse a figure the venue would confirm.
            if let AdapterReply::Live { leverage_cap, .. } = &mut reply
                && !restore
            {
                *leverage_cap = sh.lane.leverage_cap(&self.target(tier), &start);
            }
            return reply;
        }
        let stale = lock(&self.inner).adapter.as_ref().map(|a| a.dead.load(Ordering::SeqCst));
        if let Some(dead) = stale {
            if dead {
                tracing::warn!(
                    venue = %self.key.0,
                    "TRADE-OPEN: the adapter died (its bridge panicked); it is torn down and a fresh one is started"
                );
            } else {
                tracing::warn!(
                    venue = %self.key.0,
                    "TRADE-OPEN: a different adapter start arrived for a running adapter; the old one is torn down first"
                );
            }
            self.stop_adapter_locked();
            if !restore {
                // The supervisor watched THAT adapter. If the start that replaces it is refused, no adapter
                // is left for it to restore (and a supervised session is exempt from the orphan reaper), so
                // it ends here; a start that comes up supervises afresh.
                self.stop_supervisor();
            }
        }
        let target = self.target(tier);
        let (tx, mut rx) = vike_exec::event_channel(sh.cfg.event_channel);
        let events = tx.routed(&start.route_key);
        drop(tx);
        let reported = Arc::new(Mutex::new(None));
        let learning = Arc::new(link::BridgeLearning::default());
        let forwarder = match spawn_forwarder(
            move || rx.blocking_recv(),
            Arc::clone(self),
            Arc::clone(&reported),
            Arc::clone(&learning),
        ) {
            Ok(h) => h,
            Err(e) => {
                return self.start_failed(format!("could not start the event forwarder: {e}"));
            }
        };
        let (trigger_tx, trigger) = if start.takes_trigger {
            let (ttx, trx) = mpsc::channel::<()>();
            match spawn_trigger(trx, Arc::clone(self)) {
                Ok(h) => (Some(ttx), Some(h)),
                Err(e) => {
                    return self.start_failed(format!("could not start the trigger relay: {e}"));
                }
            }
        } else {
            (None, None)
        };
        let started = match sh.lane.start_adapter(&target, &start, events, trigger_tx) {
            Ok(started) => started,
            Err(refusal) => {
                tracing::warn!(
                    venue = %self.key.0,
                    block = %refusal.block_word,
                    "TRADE-OPEN: the adapter did not start: {}",
                    refusal.refusal_text
                );
                // The forwarder and the trigger relay end by themselves once the lane dropped their
                // senders; nothing here owns them any more.
                return AdapterReply::NotRunning {
                    block_word: refusal.block_word,
                    refusal_text: refusal.refusal_text,
                };
            }
        };
        let mut client = started.client;
        // A restore ran the venue's login (slow, and blocking) WITHOUT the adapter lock, so the daemon's stop
        // (`retire_locked` takes it) never waits for a login. It takes the lock now to install, and a session
        // retired meanwhile has nobody to tear this adapter down: it does it itself.
        let _late = restore.then(|| lock(&self.adapter_lock));
        if restore && (self.retired.load(Ordering::SeqCst) || self.hub_stopping()) {
            client.begin_detach();
            client.detach();
            return AdapterReply::NotRunning {
                block_word: word(ArmingBlock::AdapterFailed),
                refusal_text: format!(
                    "gatewayhub: the {} session was retired while its venue session was being rebuilt",
                    self.key.0
                ),
            };
        }
        if started.bound_tier != tier {
            // The lane's own interlock holds the bound tier equal to the account's; this is the second
            // look. A client bound at another tier never serves an order.
            client.begin_detach();
            client.detach();
            let text = format!(
                "gatewayhub: the {} client bound tier {:?} and the account runs at {tier:?}; refused (tier interlock)",
                self.key.0, started.bound_tier
            );
            tracing::error!("TRADE-OPEN: {text}");
            return AdapterReply::NotRunning {
                block_word: word(ArmingBlock::AdapterFailed),
                refusal_text: text,
            };
        }
        // The HANDOVER (finding 1 of the third IBKR review): the new client is taught, BEFORE its bridge may
        // replay the day's executions, the symbol of every order the ledger holds. A rebuilt bridge is blind to
        // them and stamps a replayed fill with the instrument IB echoes (`AAPL.NASDAQ.USD`, `EUR.IDEALPRO.USD`),
        // which never equals the symbol the engine mounted; the engine drops such a fill in silence and the loss
        // is permanent (IB replays only today's executions, and the bridge's `emitted` map blocks a second
        // delivery). The call is made even when there is nothing to teach: a bridge that holds its replay for its
        // host (the IBKR socket bridge) lets it go on this first teaching. Teaches only what THIS daemon sent; the
        // orders only the tradehub knew arrive with its own `adopt_restored`, later.
        // Only for a SUPERVISED venue (the one whose bridge is rebuilt after a loss) or when there is something to
        // teach: every other venue's client is left exactly as it was (a refactor changes no working behaviour).
        let watch = (!restore).then(|| sh.lane.link_watch(&target, &start)).flatten();
        let taught = lock(&self.inner).resting.symbols();
        if restore || watch.is_some() || !taught.is_empty() {
            client.adopt_restored(&taught);
        }
        let has_recon = started.recon.is_some();
        // The venue's word on the account's stated leverage (decision 0181): a restore's reply goes to
        // nobody, so it asks the venue nothing; a lane with nothing to confirm answers `None`.
        let leverage_cap = if restore { None } else { sh.lane.leverage_cap(&target, &start) };
        // The currency the venue says the account's money is in (decision 0183, item 1): the node anchors
        // the engine's ledger to it. A restore's reply goes to nobody, so it asks the venue nothing.
        let account_ccy = if restore { None } else { sh.lane.account_currency(&target) };
        let reply = AdapterReply::Live {
            bound_tier: started.bound_tier,
            grid: started.grid,
            contract_value: started.contract_value,
            margin_mode: started.margin_mode,
            leg_grids: started.leg_grids,
            recon: has_recon,
            leverage_cap,
            account_ccy,
        };
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<ExecCmd>(sh.cfg.command_queue.max(1));
        let dead = Arc::new(AtomicBool::new(false));
        let ctx = ExecCtx {
            client,
            session: Arc::clone(self),
            venue: self.key.0.clone(),
            symbol: start.symbol.clone(),
            budget: Arc::clone(&sh.budget),
            halt_path: sh.cfg.halt_path.clone(),
            halt_admit: start.halt_admit,
            resolves_by_coid: sh.lane.resolves_by_coid(&self.key.0),
            dedup: DedupSet::new(sh.cfg.dedup_coids),
            dead: Arc::clone(&dead),
        };
        let exec = match std::thread::Builder::new()
            .name("gwh-trade-exec".into())
            .spawn(move || dispatch::run(cmd_rx, ctx))
        {
            Ok(h) => h,
            Err(e) => {
                return self.start_failed(format!("could not start the dispatch thread: {e}"));
            }
        };
        let worker = started.recon.map(|client| {
            recon::spawn(ReconCtx {
                client: Arc::new(Mutex::new(client)),
                lane: Arc::clone(&sh.lane),
                target: target.clone(),
                bound_tier: started.bound_tier,
                progress: sh.cfg.recon_progress,
                page_bytes: sh.cfg.recon_page_bytes,
            })
        });
        let recon = match worker {
            None => None,
            Some(Ok(w)) => Some(w),
            Some(Err(e)) => {
                tracing::error!(
                    "TRADE-OPEN: could not start the reconcile worker: {e}; reconcile answers NOT READY"
                );
                None
            }
        };
        let blind = {
            let mut inner = lock(&self.inner);
            inner.route_key = Arc::from(start.route_key.as_str());
            if !restore {
                inner.link = link::LinkState::Up;
                inner.parked = None;
            }
            inner.adapter = Some(Adapter {
                start,
                reply: reply.clone(),
                bound_tier: target.tier,
                cmd_tx,
                recon,
                exec: Some(exec),
                dead,
                reported,
                learning: Arc::clone(&learning),
                forwarder: Some(forwarder),
                trigger,
            });
            // `learning` is read UNDER the inner lock: a bridge that already announced itself learned
            // while this start was still installing is judged now; one that announces afterwards finds
            // the adapter installed and is judged then (`bridge_learned`).
            link::hold_if_blind(&mut inner, &self.key.0, restore, &learning)
        };
        if let Some(words) = blind {
            tracing::error!(venue = %self.key.0, "TRADE-LINK: HELD: {words}");
        }
        tracing::info!(venue = %self.key.0, recon = has_recon, "TRADE-OPEN: the venue adapter is up");
        if let Some(watch) = watch {
            self.supervise(&target, watch);
        }
        // A supervised venue's bridge announces when it has learned the venue; one that never does (its learning
        // failed after its attempts, or is stuck) is surfaced by a held session, not by a journal line alone.
        if lock(&self.inner).supervised.is_some() {
            self.watch_learning(learning);
        }
        reply
    }

    fn start_failed(&self, why: String) -> AdapterReply {
        tracing::error!("TRADE-OPEN: {why}");
        AdapterReply::NotRunning {
            block_word: word(ArmingBlock::AdapterFailed),
            refusal_text: format!("gatewayhub: {why}"),
        }
    }

    /// Tear the adapter down: every command queued runs first, then `begin_detach` and `detach`, then the
    /// threads are joined. Caller holds `adapter_lock`.
    fn stop_adapter_locked(&self) {
        let Some(mut adapter) = lock(&self.inner).adapter.take() else { return };
        // Queued behind everything already queued. A full queue is waited on for a bounded time; past it
        // the sender is dropped, which closes the channel and ends the loop after the queue drains.
        let deadline = Instant::now() + JOIN_WAIT;
        let mut stop = ExecCmd::Stop;
        loop {
            match adapter.cmd_tx.try_send(stop) {
                Ok(()) | Err(TrySendError::Disconnected(_)) => break,
                Err(TrySendError::Full(back)) => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    stop = back;
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        if let Some(worker) = adapter.recon.take() {
            let _ = worker.tx.send(ReconJob::Stop);
            join_bounded(worker.handle, JOIN_WAIT, "gwh-trade-recon");
        }
        if let Some(exec) = adapter.exec.take() {
            join_bounded(exec, JOIN_WAIT, "gwh-trade-exec");
        }
        // The pumps are gone with the client: the lane's senders dropped, the channels closed.
        if let Some(h) = adapter.forwarder.take() {
            join_bounded(h, Duration::from_secs(2), "gwh-trade-events");
        }
        if let Some(h) = adapter.trigger.take() {
            join_bounded(h, Duration::from_secs(2), "gwh-trade-trigger");
        }
    }

    /// Retire the session if `cond` holds and no lease is attached. `true` = retired.
    fn retire_if(&self, why: &str, cond: impl FnOnce(&Inner) -> bool) -> bool {
        let _open = lock(&self.open_lock);
        {
            let inner = lock(&self.inner);
            if inner.lease.is_some() || !cond(&inner) {
                return false;
            }
        }
        self.retire_locked(why);
        true
    }

    /// Retire the session unless a lease is attached.
    pub(crate) fn retire_if_idle(&self, why: &str) -> bool {
        self.retire_if(why, |_| true)
    }

    /// Retire the session whatever holds it (the daemon is stopping).
    fn retire_force(&self, why: &str) {
        let _open = lock(&self.open_lock);
        self.retire_locked(why);
    }

    fn retire_locked(&self, why: &str) {
        if self.retired.swap(true, Ordering::SeqCst) {
            return;
        }
        self.stop_supervisor();
        {
            let _adapter = lock(&self.adapter_lock);
            self.stop_adapter_locked();
        }
        {
            let mut inner = lock(&self.inner);
            inner.live_lock = None;
            if let Some(lease) = inner.lease.take() {
                self.shared.leases.fetch_sub(1, Ordering::SeqCst);
                drop(inner);
                lease.bye(vike_gatewayhub_client::proto::TradeBye::ServerStopping);
            }
        }
        let mut table = lock(&self.shared.sessions);
        if table.get(&self.key).is_some_and(|s| std::ptr::eq(Arc::as_ptr(s), self)) {
            table.remove(&self.key);
        }
        drop(table);
        tracing::info!(venue = %self.key.0, "TRADE-LINK: session retired: {why}");
    }
}

/// A fresh random, non-zero `gw_epoch` for one session (`0` means "none" on the wire).
fn mint_epoch() -> u64 {
    let epoch = fresh_nonce()[..8].iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
    epoch.max(1)
}

/// Whether two adapter starts ask for the same thing (everything but the request id).
fn same_start(a: &AdapterStart, b: &AdapterStart) -> bool {
    let mut a = a.clone();
    a.rid = b.rid;
    a == *b
}

fn join_bounded(handle: JoinHandle<()>, max: Duration, name: &str) {
    let deadline = Instant::now() + max;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            tracing::warn!(
                thread = name,
                "TRADE-LINK: a thread did not end in time; it is left to finish on its own"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = handle.join();
}

// ---- the two small relays ------------------------------------------------------------------------------

/// The forwarder: every event the adapter's pumps push is stamped into the ring in order.
fn spawn_forwarder(
    mut next: impl FnMut() -> Option<Ingest> + Send + 'static,
    session: Arc<AccountSession>,
    reported: Arc<Mutex<Option<String>>>,
    learning: Arc<link::BridgeLearning>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name("gwh-trade-events".into()).spawn(move || {
        while let Some(ingest) = next() {
            match ingest {
                Ingest::Event(event) => {
                    // The coids the bridge announces are what a hold is judged against (`link` module).
                    learning.note(&event);
                    session.emit_event(event);
                }
                // The adapter disclosing its venue link (the IBKR bridge does both, W14c B). DOWN: the
                // supervisor tears the adapter down and rebuilds it, without waiting for a probe. UP: the
                // bridge has LEARNED the venue (its open orders are rebound, the day's executions
                // replayed), which releases a session held because a rebuilt bridge could not manage the
                // orders of the one before it. The daemon consumes both here and forwards neither: a core
                // would read them as a trade link to defer its watchdog for.
                Ingest::ExecLink(update) => {
                    if update.up {
                        learning.mark_learned();
                        session.bridge_learned(&learning);
                    } else {
                        let mut said = lock(&reported);
                        if said.is_none() {
                            *said = Some(format!(
                                "the {} bridge reported its venue link down",
                                session.key.0
                            ));
                        }
                    }
                }
                other => {
                    let mut inner = lock(&session.inner);
                    if !inner.foreign_ingest_warned {
                        inner.foreign_ingest_warned = true;
                        tracing::warn!(
                            "TRADE-LINK: a venue adapter pushed an ingest that is not an execution event; it is not forwarded ({})",
                            ingest_name(&other)
                        );
                    }
                }
            }
        }
    })
}

fn ingest_name(ingest: &Ingest) -> &'static str {
    match ingest {
        Ingest::Event(_) => "Event",
        Ingest::GatewayEvent(_) => "GatewayEvent",
        Ingest::ExecLink(_) => "ExecLink",
        Ingest::Command(_) => "Command",
        Ingest::Market => "Market",
        Ingest::BarSeed(_) => "BarSeed",
        Ingest::BarClose(_) => "BarClose",
        Ingest::Quote(_) => "Quote",
        Ingest::Trade(_) => "Trade",
        Ingest::Book(_) => "Book",
        Ingest::Watchdog => "Watchdog",
        Ingest::StreamStatus(_) => "StreamStatus",
        Ingest::Flow(_) => "Flow",
        Ingest::DataDelay(_) => "DataDelay",
        Ingest::Accounts(_) => "Accounts",
    }
}

/// The trigger relay: a bridge's reconcile trigger becomes a `ReconcileNow` for the attached client.
fn spawn_trigger(
    rx: mpsc::Receiver<()>,
    session: Arc<AccountSession>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name("gwh-trade-trigger".into()).spawn(move || {
        while rx.recv().is_ok() {
            session.reconcile_now("the venue adapter asked for a reconcile pass");
        }
    })
}
