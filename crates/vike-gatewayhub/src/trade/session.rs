//! The HUB and the per-account SESSION.
//!
//! [`AccountSessions`] is the daemon's one trade state: the lane, the numbers, the epoch and the table of
//! [`AccountSession`]s. [`AccountSession`] is one `(venue, account)`: it owns the venue adapter once the
//! client has started it, the event ring, the threads that drive and drain the adapter and the
//! at-most-one [`TradeLeaseHold`] that is attached to it.
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
//! `gateway_epoch` is random per SESSION (minted in [`AccountSession::new`]) and is NOT ordered: it names one
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
//! `AccountSessions::open` refuses, in this order and with these words: a bad `client_id`; a venue this build
//! carries no adapter for (`FeatureAbsent`); a `paper` tier (a paper account opens no lease); an account
//! whose rows state no tier (the block `account_tier` names) or another tier than the client claims (a label
//! that names no row any more is judged by the `account` row the client names, `TradeOpen.row`, else by the one a
//! session of it recorded: the operator renamed the account while it ran, and the session is then served on that
//! row's keys, `AccountSession::target`);
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
use vike_execution::Ingest;
use vike_gatewayhub_client::proto::{
    AdapterReply, AdapterStart, ArmingTableWire, PROPERTIES_BUSY, PROPERTIES_EXPIRED,
    PROPERTIES_LIST_EMPTY, PROPERTIES_LIST_UNAVAILABLE, PROPERTIES_NO_INSTRUMENT,
    PROPERTIES_NOT_STARTED, PROPERTIES_RATE_UNAVAILABLE, PreflightAnswer, PreflightStep,
    SYMBOL_FORGOTTEN, SymbolPropertiesRefusal, SymbolPropertiesRefusalCode, SymbolPropertiesWire,
    TradeDown, TradeOpened, TradeStatusReport,
};
use vike_hub_protocol::auth::fresh_nonce;
use vike_model::ArmingBlock;
use vike_model::EventRingCursor;
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
use vike_model::events::{Event, OrderCancelRejected, OrderModifyRejected, OrderRejected};
use vike_model::{AccountTier, VenueReadKind};

mod link;
mod venue_read;

#[cfg(test)]
mod label_tests;
#[cfg(test)]
mod rename_tests;
#[cfg(test)]
mod shutdown_tests;
#[cfg(test)]
mod spelling_tests;

use super::TradeConfig;
use super::dedup::DedupSet;
use super::dispatch::{self, ExecutionClientDispatchCommand, ExecutionClientDispatchContext};
use super::lane::{TradeLane, TradeLaneTarget, VenueLinkReport};
use super::lease::{ADAPTER_GONE_REASON, NO_ADAPTER_REASON, TradeLeaseHold, TradeLeaseHoldSeed};
use super::once::{OnceMemory, OnceMemoryRepeats};
use super::recon::{self, ReconcileWorker, ReconcileWorkerContext, ReconcileWorkerJob};
use super::ring::{EventRing, EventRingEntry, EventRingResume};
use super::state_dir::StateDir;
use crate::budget::ledger::Budget;

/// The most `SymbolFacts` lookups the daemon runs at once: a venue call each, on a thread of its own so the
/// lease reader and the exec thread never wait on one; a client asking more is answered a reason.
const MAX_SYMBOL_FACTS_INFLIGHT: usize = 4;

/// One of the `MAX_SYMBOL_FACTS_INFLIGHT` lookup slots, given back when it is dropped (a panic included).
struct SymbolPropertiesLookupSlot(Arc<AccountSessionsShared>);

impl Drop for SymbolPropertiesLookupSlot {
    fn drop(&mut self) {
        self.0.facts_inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// **The code of a properties refusal, read off the daemon's OWN words** (the lookups build every reason from
/// [`PROPERTIES_BUSY`] and its siblings), at the send: the node decides by the code alone and never reads the
/// words. A "not now" word wins over a "gone" one (a sentence that quotes a venue's words may carry both), the
/// first "not now" word in this order wins among them, and a sentence with none is a [`SymbolPropertiesRefusalCode::Verdict`].
pub(crate) fn properties_refusal_code(reason: &str) -> SymbolPropertiesRefusalCode {
    [
        (PROPERTIES_BUSY, SymbolPropertiesRefusalCode::Busy),
        (PROPERTIES_NOT_STARTED, SymbolPropertiesRefusalCode::NotStarted),
        (PROPERTIES_LIST_UNAVAILABLE, SymbolPropertiesRefusalCode::ListUnavailable),
        (PROPERTIES_LIST_EMPTY, SymbolPropertiesRefusalCode::ListEmpty),
        (PROPERTIES_RATE_UNAVAILABLE, SymbolPropertiesRefusalCode::RateUnavailable),
        (PROPERTIES_NO_INSTRUMENT, SymbolPropertiesRefusalCode::NoInstrument),
        (PROPERTIES_EXPIRED, SymbolPropertiesRefusalCode::Expired),
    ]
    .into_iter()
    .find(|(word, _)| reason.contains(word))
    .map_or(SymbolPropertiesRefusalCode::Verdict, |(_, code)| code)
}

/// **Why a node that did not say it books margin modes (`TradeUp::SymbolProperties::modes`, decision 0218) is not
/// answered `facts`, or `None`.** A symbol the venue holds in isolated margin only, or a fully funded spot pair,
/// is answered with `SymbolPropertiesWire::margin_mode`; a node that predates the field would ignore it and book the
/// symbol CROSS, an isolated position modelled as cross being worse than a refusal. So such a node is refused
/// by name, exactly as a node that does not convert is refused a foreign-quoted symbol. A symbol that states no
/// mode (every symbol of every other venue) is never refused here.
pub(crate) fn mode_refusal(
    venue: &str,
    symbol: &str,
    facts: &SymbolPropertiesWire,
    modes: bool,
) -> Option<String> {
    let mode = facts.margin_mode.filter(|_| !modes)?;
    let held = match mode {
        vike_model::MarginMode::Isolated => "in isolated margin only",
        vike_model::MarginMode::Cash => "as a fully funded spot pair",
        vike_model::MarginMode::Cross => return None,
    };
    Some(format!(
        "{venue}: `{symbol}` is held {held}, and the node that asked books every symbol it admits in cross \
         margin (it does not say it books another mode): update the node"
    ))
}

/// The most client ids whose command watermark a session remembers.
const MAX_WATERMARKS: usize = 16;

/// How long a thread join waits before the thread is left to end on its own.
const JOIN_WAIT: Duration = Duration::from_secs(10);

/// How often the reaper looks at the table. The stop is not a tick: it ends the reaper's wait at once.
const REAPER_TICK: Duration = Duration::from_millis(500);

/// A session's key: the venue and the account label's text (`None` is the default account).
pub type AccountSessionKey = (String, Option<String>);

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
pub struct TradeLeaseOpenRequest {
    /// The caller's id for the lease, stable across its reconnects.
    pub client_id: String,
    /// A `vike_model::VENUES` slug.
    pub venue: String,
    /// The account label's text; `None` is the default account.
    pub account: Option<String>,
    /// The tier the caller believes the account runs at.
    pub tier: AccountTier,
    /// The last event the caller folded.
    pub resume: Option<EventRingCursor>,
    /// Take the lease over from its holder.
    pub takeover: bool,
    /// The authenticated key's fingerprint.
    pub key_id: Option<String>,
    /// Forces the connection shut (the adapter's socket).
    pub closer: Arc<dyn Fn() + Send + Sync>,
}

/// An accepted open.
pub struct TradeLeaseOpenAcceptance {
    /// The reply to write.
    pub reply: TradeOpened,
    /// The lease the connection now serves.
    pub lease: Arc<TradeLeaseHold>,
}

/// A refused open: what to answer instead of `TradeOpened`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TradeLeaseOpenRefusal {
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

fn refused(block: ArmingBlock, text: impl Into<String>) -> TradeLeaseOpenRefusal {
    TradeLeaseOpenRefusal::Refused { block_word: word(block), text: text.into() }
}

/// What a session is shared with: the lane, the numbers, the table.
pub(crate) struct AccountSessionsShared {
    pub(crate) lane: Arc<dyn TradeLane>,
    pub(crate) config: TradeConfig,
    pub(crate) budget: Arc<Budget>,
    pub(crate) state: Option<StateDir>,
    pub(crate) token: OnceLock<String>,
    pub(crate) next_lease: AtomicU64,
    pub(crate) leases: AtomicUsize,
    /// `SymbolFacts` lookups running on their own threads (bounded by `MAX_SYMBOL_FACTS_INFLIGHT`).
    pub(crate) facts_inflight: AtomicUsize,
    /// Which operator reads may reach a venue now (`venue_read.rs`).
    pub(crate) venue_reads: Arc<venue_read::VenueReadGate>,
    pub(crate) stopping: AtomicBool,
    /// How long (ms) a supervised bridge has to announce that it learned the venue before the session is held
    /// (`link::LEARN_BUDGET`; `AccountSessions::set_learning_budget`).
    pub(crate) learn_budget_ms: AtomicU64,
    pub(crate) sessions: Mutex<HashMap<AccountSessionKey, Arc<AccountSession>>>,
}

/// The daemon's trade state. See the module doc.
pub struct AccountSessions {
    shared: Arc<AccountSessionsShared>,
    /// The reaper's stop (dropping the sender wakes it) and its thread.
    reaper: Mutex<Option<(Sender<()>, JoinHandle<()>)>>,
}

impl std::fmt::Debug for AccountSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountSessions").finish_non_exhaustive()
    }
}

impl AccountSessions {
    /// A hub over `lane`. Starts the reaper thread (`gw-trade-reaper`).
    pub fn new(lane: Arc<dyn TradeLane>, config: TradeConfig, budget: Arc<Budget>) -> Arc<Self> {
        let state = config.state_dir.clone().map(StateDir::new);
        let shared = Arc::new(AccountSessionsShared {
            lane,
            config,
            budget,
            state,
            token: OnceLock::new(),
            next_lease: AtomicU64::new(1),
            leases: AtomicUsize::new(0),
            facts_inflight: AtomicUsize::new(0),
            venue_reads: Arc::default(),
            stopping: AtomicBool::new(false),
            learn_budget_ms: AtomicU64::new(
                u64::try_from(link::LEARN_BUDGET.as_millis()).unwrap_or(u64::MAX),
            ),
            sessions: Mutex::new(HashMap::new()),
        });
        let hub = Arc::new(Self { shared: Arc::clone(&shared), reaper: Mutex::new(None) });
        let (stop, stopped) = mpsc::channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("gw-trade-reaper".into())
            .spawn(move || reaper(&shared, &stopped));
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
        let key: AccountSessionKey = (venue.to_string(), account.map(str::to_string));
        lock(&self.shared.sessions).get(&key).map(|s| s.status())
    }

    /// How many repeated `OrderAccepted` / terminal / fill events the account's first-wins memory has dropped since
    /// the session was built (`OnceMemory`): what the retire log line reports as `repeats_accepted`,
    /// `repeats_terminal` and `repeats_fill`.
    pub fn session_repeats(&self, venue: &str, account: Option<&str>) -> Option<OnceMemoryRepeats> {
        let key: AccountSessionKey = (venue.to_string(), account.map(str::to_string));
        lock(&self.shared.sessions).get(&key).map(|s| s.repeats())
    }

    /// The `ArmingQuery` answer.
    pub fn arming_table(&self) -> ArmingTableWire {
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

    /// Answer a `TradeOpen` from an older client, which names no `account` row: [`Self::open_at_row`] with none, so
    /// the open is judged by the label alone (or by the row a running session of it recorded).
    pub fn open(
        &self,
        req: TradeLeaseOpenRequest,
    ) -> Result<TradeLeaseOpenAcceptance, TradeLeaseOpenRefusal> {
        self.open_at_row(req, None)
    }

    /// Answer a `TradeOpen`. See the module doc for the refusals, in order. `row` is the `account` row the caller's
    /// engine started on (`TradeOpen.row`): an open whose label names no row any more is judged by it, and the
    /// session's adapter starts on its keys (the operator renamed the account while it ran).
    pub fn open_at_row(
        &self,
        req: TradeLeaseOpenRequest,
        row: Option<i64>,
    ) -> Result<TradeLeaseOpenAcceptance, TradeLeaseOpenRefusal> {
        let out = self.open_inner(req, row);
        match &out {
            Ok(opened) => tracing::info!(
                lease = opened.lease.id,
                client = %opened.lease.client_id,
                key_id = %opened.lease.key_id,
                epoch = opened.reply.gateway_epoch,
                through_cseq = opened.reply.received_through_command_sequence,
                ring_floor = opened.reply.event_ring_floor,
                "TRADE-OPEN: lease granted"
            ),
            Err(refusal) => tracing::warn!("TRADE-OPEN: refused: {refusal:?}"),
        }
        out
    }

    fn open_inner(
        &self,
        req: TradeLeaseOpenRequest,
        row: Option<i64>,
    ) -> Result<TradeLeaseOpenAcceptance, TradeLeaseOpenRefusal> {
        let sh = &self.shared;
        if sh.stopping.load(Ordering::SeqCst) {
            return Err(TradeLeaseOpenRefusal::Error(
                "gatewayhub: the daemon is stopping".to_string(),
            ));
        }
        if req.client_id.is_empty() || req.client_id.len() > super::lease::MAX_NAME_BYTES * 2 {
            return Err(TradeLeaseOpenRefusal::Error(format!(
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
            return Err(TradeLeaseOpenRefusal::Error(format!(
                "gatewayhub: a bad account label: {e}"
            )));
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
        if let Some(refusal) = known.as_ref().and_then(|s| s.link_refuses_open()) {
            return Err(refusal);
        }
        // The tier is re-derived from the account table, never taken from the client. A label that names no row any
        // more is the account of the row the caller names (`TradeOpen.row`), else of the row a session of it
        // recorded: the operator renamed it while it ran, and that row's answer under its new label is this open's.
        let mut follows: Option<RowNow> = None;
        let answer = match sh.lane.account_tier(&req.venue, req.account.as_deref()) {
            Err(ArmingBlock::NoAccountRow) => {
                let row = row.or_else(|| known.as_ref().and_then(|s| s.recorded_row()));
                match row.and_then(|row| row_now(&*sh.lane, &req.venue, row)) {
                    Some(now) => {
                        let answer = now.answer;
                        follows = Some(now);
                        answer
                    }
                    None => Err(ArmingBlock::NoAccountRow),
                }
            }
            answer => answer,
        };
        match answer {
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
                return Err(TradeLeaseOpenRefusal::Error(format!(
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
            // Before the attach answers, so the adapter start that follows the open reads the account by its row.
            session.note_row(row, follows.as_ref());
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

    fn session_for(&self, req: &TradeLeaseOpenRequest) -> Arc<AccountSession> {
        let key: AccountSessionKey = (req.venue.clone(), req.account.clone());
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
            drop(stop);
            join_bounded(handle, JOIN_WAIT, "gw-trade-reaper");
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

    /// **Test support: what a suite with no socket owes before its hub stops.** Such a suite plays every
    /// lease's connection itself (`TradeLeaseHold::handle`, `TradeLeaseHold::next`), so no `server::trade` guard will finish a
    /// lease it leaves attached and [`Self::shutdown`] would wait its whole bound for that goodbye. This ends
    /// each attached lease as that guard does when a reader leaves (close, then finish).
    #[cfg(any(test, feature = "test-support"))]
    pub fn end_attached_leases(&self) {
        let sessions: Vec<Arc<AccountSession>> =
            lock(&self.shared.sessions).values().cloned().collect();
        for session in sessions {
            let lease = lock(&session.inner).lease.clone();
            if let Some(lease) = lease {
                lease.close();
                lease.finish();
            }
        }
    }
}

impl Drop for AccountSessions {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// What the `account` row `row` answers NOW, under the label it carries now: the account a label that names no row
/// any more is (the operator renamed it while it ran).
struct RowNow {
    row: i64,
    /// The label the row carries now (`None`: the default account).
    label: Option<String>,
    answer: Result<AccountTier, ArmingBlock>,
}

/// [`RowNow`] of `venue`'s row `row` (`TradeLane::row_label`, then `TradeLane::account_tier` of that label), or
/// `None` when no row of `venue` has that id.
fn row_now(lane: &dyn TradeLane, venue: &str, row: i64) -> Option<RowNow> {
    let label = lane.row_label(venue, row)?;
    let answer = lane.account_tier(venue, label.as_deref());
    Some(RowNow { row, label, answer })
}

/// The `account` row a session's adapter signs with, and the label lane calls address it by while the operator has
/// it renamed. A LEAF lock: taken alone, never held across a call or while another lock is taken.
#[derive(Debug, Default)]
struct RowFollow {
    /// The row: the caller's `TradeOpen.row`, else `TradeLane::row_of` at the adapter's start.
    id: Option<i64>,
    /// The label the row carries now, while it is not the session's own: [`AccountSession::target`] addresses
    /// the lane by it, so a start reads THIS row's keys. `None` while the session's label names the account.
    renamed: Option<Option<String>>,
}

fn subject(venue: &str, account: Option<&str>) -> String {
    match account {
        None => venue.to_string(),
        Some(label) => format!("{venue} account `{label}`"),
    }
}

/// Every `REAPER_TICK`, retire what is orphaned, until the hub's stop drops `stop`'s sender: that ends the
/// wait at once (a sleep would hold the stop for the rest of the tick), and no pass runs after it.
fn reaper(shared: &Arc<AccountSessionsShared>, stop: &mpsc::Receiver<()>) {
    while let Err(mpsc::RecvTimeoutError::Timeout) = stop.recv_timeout(REAPER_TICK) {
        let sessions: Vec<Arc<AccountSession>> = lock(&shared.sessions).values().cloned().collect();
        let ttl = shared.config.orphan_ttl;
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
pub(crate) enum EventRingPull {
    /// The event, with the account spelling and epoch its frame carries.
    Event { entry: EventRingEntry, account: Arc<str>, gateway_epoch: u64 },
    /// The ring evicted that `seq`: the oldest held is `floor`.
    Gap { floor: u64 },
    /// Not produced yet.
    Nothing,
}

enum Attach {
    /// The session was retired while the open waited: build a fresh one.
    Retired,
    /// The venue session went down while the open waited (see `link_refuses_open`).
    Down(TradeLeaseOpenRefusal),
    Refused(TradeLeaseOpenRefusal),
}

struct Adapter {
    start: AdapterStart,
    reply: AdapterReply,
    /// When the venue was last asked the account's currency for this adapter (`None`: never, a restored one).
    /// A reply that carries none is asked again at the next start, after
    /// `TradeConfig::account_currency_retry_after` (`retry_account_currency`).
    account_currency_asked: Option<Instant>,
    bound_tier: AccountTier,
    command_tx: SyncSender<ExecutionClientDispatchCommand>,
    reconcile: Option<ReconcileWorker>,
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
    /// venue's open orders) and the order client ids it announced since it started (`link::BridgeLearning`). A rebuilt
    /// session held for orders it could not manage is released by the first, judged by the second.
    learning: Arc<link::BridgeLearning>,
    /// The venue path the bridge owns and heals itself, when the lane names one (`TradeLane::venue_link`): what
    /// the status verb states of it. Read, never acted on.
    link_report: Option<VenueLinkReport>,
    forwarder: Option<JoinHandle<()>>,
    trigger: Option<JoinHandle<()>>,
}

struct Inner {
    tier: AccountTier,
    ring: EventRing,
    /// The first-wins memory in front of `ring` (`once` module): one record per fact. It lives and dies with the
    /// ring's epoch (`link_restore` clears it where it swaps the ring).
    once: OnceMemory,
    route_key: Arc<str>,
    watermarks: HashMap<String, u64>,
    lease: Option<Arc<TradeLeaseHold>>,
    adapter: Option<Adapter>,
    live_lock: Option<LiveLock>,
    detached_since: Option<Instant>,
    pending_reconcile: Option<String>,
    commands: u64,
    foreign_ingest_warned: bool,
    /// The state of the supervised venue session (`link` module); `Up` for every venue that has none.
    link: link::VenueLinkSupervisorState,
    /// The start the supervisor replays when it brings a lost adapter back (set while `link` is `Down`).
    parked: Option<AdapterStart>,
    /// The stop flag of the supervisor thread watching this session's venue link, while one runs.
    supervised: Option<Arc<AtomicBool>>,
    /// The orders this SUPERVISED session sent toward its venue and has not seen finish (`link` module): the
    /// daemon's only knowledge of what may rest at the venue across a rebuild of the adapter.
    resting: link::VenueLinkSupervisorResting,
    /// Set when an adapter was rebuilt while `resting` held orders: the words every order verb is refused
    /// with from then on (`link` module). It lasts as long as the session.
    held: Option<String>,
    /// The ledger's order client ids when the hold was set: what the bridge has to ACCOUNT for before it is lifted.
    held_order_client_ids: std::collections::HashSet<String>,
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
    shared: Arc<AccountSessionsShared>,
    key: AccountSessionKey,
    open_lock: Mutex<()>,
    adapter_lock: Mutex<()>,
    retired: AtomicBool,
    inner: Mutex<Inner>,
    /// The account's row, and its label while renamed ([`RowFollow`]; a leaf lock).
    row: Mutex<RowFollow>,
}

impl AccountSession {
    fn new(
        shared: Arc<AccountSessionsShared>,
        key: AccountSessionKey,
        tier: AccountTier,
    ) -> Arc<Self> {
        // The one spelling. `open_inner` refused a label `AccountLabel::parse` refuses before any session exists,
        // so `Named` here holds a parsed label; `route_key_of` renders any text as this did.
        let label = key.1.clone().map_or(AccountLabel::Default, AccountLabel::Named);
        let route_key: Arc<str> = Arc::from(route_key_of(&key.0, &label));
        let ring = EventRing::new(mint_epoch(), shared.config.ring_events);
        // The Submit dedup set's horizon: an order client id cannot come back as a new order sooner than it is forgotten here.
        let once = OnceMemory::new(shared.config.dedup_order_client_ids);
        Arc::new(Self {
            shared,
            key,
            open_lock: Mutex::new(()),
            adapter_lock: Mutex::new(()),
            retired: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                tier,
                ring,
                once,
                route_key,
                watermarks: HashMap::new(),
                lease: None,
                adapter: None,
                live_lock: None,
                detached_since: None,
                pending_reconcile: None,
                commands: 0,
                foreign_ingest_warned: false,
                link: link::VenueLinkSupervisorState::Up,
                parked: None,
                supervised: None,
                resting: link::VenueLinkSupervisorResting::default(),
                held: None,
                held_order_client_ids: Default::default(),
                held_overflowed: false,
                carry: false,
                had_adapter: false,
            }),
            row: Mutex::new(RowFollow::default()),
        })
    }

    /// The account as the lane is asked for it: the session's label, or, while the operator has the account
    /// renamed ([`RowFollow::renamed`]), the label its row carries now, so a start reads that row's keys.
    fn target(&self, tier: AccountTier) -> TradeLaneTarget {
        let renamed = lock(&self.row).renamed.clone();
        TradeLaneTarget {
            venue: self.key.0.clone(),
            account: renamed.unwrap_or_else(|| self.key.1.clone()),
            tier,
        }
    }

    /// The row this session recorded ([`RowFollow::id`]).
    fn recorded_row(&self) -> Option<i64> {
        lock(&self.row).id
    }

    /// Record what an open found: judged by a row (`follows`: the account is renamed, its lane calls go by the row's
    /// label now), or by the session's own label, with the row the caller named (`given`) when it named one.
    fn note_row(&self, given: Option<i64>, follows: Option<&RowNow>) {
        match follows {
            Some(now) => self.remember(now),
            None => {
                let mut row = lock(&self.row);
                if given.is_some() {
                    row.id = given;
                }
                row.renamed = None;
            }
        }
    }

    /// Remember that the account is row `now.row`, which carries `now.label` now; said at `warn` when that changes
    /// what the lane is asked for.
    fn remember(&self, now: &RowNow) {
        let renamed = (now.label != self.key.1).then(|| now.label.clone());
        let changed = {
            let mut row = lock(&self.row);
            row.id = Some(now.row);
            let changed = row.renamed != renamed;
            row.renamed = renamed;
            changed
        };
        if changed {
            tracing::warn!(
                row = now.row,
                "TRADE-OPEN: {} names no account row any more; row {} it runs on is now {} ({}): the same \
                 account, served on that row's keys",
                subject(&self.key.0, self.key.1.as_deref()),
                now.row,
                subject(&self.key.0, now.label.as_deref()),
                now.answer.map_or_else(ArmingBlock::as_str, AccountTier::as_str)
            );
        }
    }

    /// **What the row this session recorded answers NOW**, under the label it carries now, for a supervised venue
    /// link whose label names no row any more (`link::VenueLinkSupervisor::verdict`): `None` when no row was recorded or the
    /// row is gone. Remembered, so the rebuild that follows starts on that row's keys.
    pub(crate) fn follow_row(&self) -> Option<Result<AccountTier, ArmingBlock>> {
        let now = row_now(&*self.shared.lane, &self.key.0, self.recorded_row()?)?;
        self.remember(&now);
        Some(now.answer)
    }

    /// A RENAMED session (only) re-reads its row ([`Self::follow_row`]), so its lane calls go by the label the row
    /// carries now (renamed back: the session's own). A session never renamed is not touched.
    pub(crate) fn refresh_renamed(&self) {
        if lock(&self.row).renamed.is_some() {
            let _ = self.follow_row();
        }
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
        let want = self.label().ok();
        let wrong_venue = !order.venue.is_empty() && order.venue != self.key.0;
        let wrong_account = order.account.as_ref().is_some_and(|a| want.as_ref() != Some(a));
        (wrong_venue || wrong_account).then(|| {
            format!(
                "gatewayhub: the order names another venue or account than this lease ({})",
                subject(&self.key.0, self.key.1.as_deref())
            )
        })
    }

    /// **Why this lease refuses a NEW order before the venue**: [`Self::identity_conflict`], else a symbol the control
    /// door writes another way on this lease's venue (`vike_catalog::respelled_symbol`, the C-D4 follow-up). An order
    /// already at the venue (a modify, a cancel, a restored order) is never refused for its name.
    pub(crate) fn submit_refusal(&self, order: &vike_model::OrderRequest) -> Option<String> {
        self.identity_conflict(order).or_else(|| {
            vike_catalog::respelled_symbol(&self.key.0, &order.symbol)
                .map(|why| format!("gatewayhub: {why}"))
        })
    }

    /// The session's account. ⚠ A label the grammar refuses names NO account and never the default one, the rule
    /// `crates/vike-client/src/health.rs`'s `route_key` states: borrowing the default's identity would claim its live
    /// lock. [`AccountSessions::open_at_row`] refuses such a label before a session exists; this is the second fence.
    fn label(&self) -> Result<AccountLabel, String> {
        self.key.1.as_deref().map_or(Ok(AccountLabel::Default), |text| {
            AccountLabel::parse(text).map_err(|e| {
                format!(
                    "gatewayhub: {}: a bad account label ({e}); refused",
                    subject(&self.key.0, Some(text))
                )
            })
        })
    }

    /// The live lock's name: the same route key `AdapterStart.route_key` carries. `Err` for a label that does not
    /// parse ([`Self::label`]): no lock is claimed for it.
    fn lock_key(&self) -> Result<String, String> {
        self.label().map(|label| route_key_of(&self.key.0, &label))
    }

    /// Whether this account is an ARMED LIVE route key (the lane's table says it runs at `live`).
    fn armed_live(&self, tier: AccountTier) -> bool {
        if tier != AccountTier::Live {
            return false;
        }
        let table = self.shared.lane.arming_table();
        // The account as the lane knows it: a renamed one by the label its row carries now (`Self::target`).
        let label = self.target(tier).account;
        table.rows.iter().any(|r| {
            r.venue == self.key.0
                && r.label == label
                && r.tier == Some(AccountTier::Live)
                && r.effective == Some(AccountTier::Live)
                && block_runs(&r.block_word)
        })
    }

    fn attach(
        self: &Arc<Self>,
        req: &TradeLeaseOpenRequest,
    ) -> Result<TradeLeaseOpenAcceptance, Attach> {
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
                old.wait_finished(sh.config.fence_drain);
            } else {
                if old.client_id != req.client_id && !req.takeover {
                    return Err(Attach::Refused(TradeLeaseOpenRefusal::LeaseHeld {
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
                if !old.wait_finished(sh.config.fence_drain) {
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
        if sh.leases.load(Ordering::SeqCst) >= sh.config.max_leases
            && lock(&self.inner).lease.is_none()
        {
            return Err(Attach::Refused(TradeLeaseOpenRefusal::Error(format!(
                "gatewayhub: already serving {} trade leases (the daemon's cap)",
                sh.config.max_leases
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
            let route_key = self
                .lock_key()
                .map_err(|why| Attach::Refused(refused(ArmingBlock::AdapterFailed, why)))?;
            match state.claim_live(&route_key) {
                Ok(guard) => lock(&self.inner).live_lock = Some(guard),
                Err(e) => {
                    // A lease refusal, not an account's: the whole live mount is refused (the tradehub
                    // records a `LeaseHeld` into `lease_refusals`; a `TradeRefused` is only this
                    // account's), as the lock's `AddrInUse` did when the tradehub claimed it. The
                    // lock's own sentence rides in the holder field, the one string a `LeaseHeld`
                    // carries.
                    tracing::error!(
                        route_key = %route_key,
                        "TRADE-OPEN: the live-account lock was refused: {e}"
                    );
                    return Err(Attach::Refused(TradeLeaseOpenRefusal::LeaseHeld {
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
        let last = inner.ring.last_sequence();
        let plan = inner.ring.plan_resume(req.resume.as_ref());
        let (mut next_sequence, gap) = match plan {
            EventRingResume::Fresh => (last + 1, None),
            EventRingResume::Replay { from } => (from, None),
            EventRingResume::Unavailable { floor } => (last + 1, Some(floor)),
        };
        // A client that cannot be replayed what it missed reconciles, and may leave an order UNKNOWN: the
        // venue's own row is what resolves that, so no row may be taken for a repeat of one it never got.
        if !matches!(plan, EventRingResume::Replay { .. }) {
            inner.once.clear();
        }
        // The first lease after a restore starts at the new ring's first event: what the rebuilt adapter pushed
        // while nobody was attached (a bridge may replay events the moment it connects) is the first lease's,
        // not skipped as "before it attached". The client's duplicate guards drop what its reconcile also finds.
        if std::mem::take(&mut inner.carry)
            && inner.ring.floor() != 0
            && !matches!(plan, EventRingResume::Replay { .. })
        {
            next_sequence = inner.ring.floor();
        }
        let lease = TradeLeaseHold::new(
            Arc::clone(self),
            TradeLeaseHoldSeed {
                id,
                client_id: req.client_id.clone(),
                key_id: req.key_id.clone().unwrap_or_default(),
                since_ms: now_ms(),
                next_sequence,
                live_from: last + 1,
                control_cap: sh.config.control_queue,
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
            gateway_epoch: inner.ring.gateway_epoch(),
            lease_epoch: id,
            received_through_command_sequence: inner
                .watermarks
                .get(&req.client_id)
                .copied()
                .unwrap_or(0),
            project_token: token,
            heartbeat_ms: u64::try_from(sh.config.heartbeat.as_millis()).unwrap_or(u64::MAX),
            event_ring_floor: Some(inner.ring.floor()),
        };
        let replaced = inner.lease.replace(Arc::clone(&lease)).is_some();
        inner.tier = req.tier;
        inner.detached_since = None;
        drop(inner);
        if !replaced {
            sh.leases.fetch_add(1, Ordering::SeqCst);
        }
        Ok(TradeLeaseOpenAcceptance { reply, lease })
    }

    /// The lease's connection is over: release the hold (unless a newer lease already replaced it).
    pub(crate) fn lease_ended(&self, lease: &TradeLeaseHold) {
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
    pub(crate) fn note_command_sequence(&self, client_id: &str, command_sequence: u64) {
        let mut inner = lock(&self.inner);
        inner.commands += 1;
        if inner.watermarks.len() >= MAX_WATERMARKS && !inner.watermarks.contains_key(client_id) {
            inner.watermarks.clear();
        }
        let slot = inner.watermarks.entry(client_id.to_string()).or_insert(0);
        *slot = (*slot).max(command_sequence);
    }

    /// Queue `cmd` for the adapter's dispatch thread, waiting while the queue is full (back-pressure on
    /// the client's writer) as long as `alive` holds. `Err` hands the command back: there is no adapter,
    /// its thread is gone or the wait was abandoned.
    pub(crate) fn enqueue(
        &self,
        cmd: ExecutionClientDispatchCommand,
        alive: impl Fn() -> bool,
    ) -> Result<(), ExecutionClientDispatchCommand> {
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
                    ExecutionClientDispatchCommand::Submit(order) => {
                        inner.resting.sent(&order.order_client_id, order.qty, &order.symbol)
                    }
                    ExecutionClientDispatchCommand::SubmitBatch(orders) => {
                        for order in orders {
                            inner.resting.sent(&order.order_client_id, order.qty, &order.symbol);
                        }
                    }
                    _ => {}
                }
            }
            inner.adapter.as_ref().map(|a| a.command_tx.clone())
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
    pub(crate) fn synthesize(&self, cmd: ExecutionClientDispatchCommand) {
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
                cause: None,
                order_client_id: order.order_client_id.clone(),
                reason: reason.as_str().into(),
                timestamp: order.timestamp,
            })
        };
        let cancel_rejected = |order_client_id: &str| {
            Event::OrderCancelRejected(OrderCancelRejected {
                order_client_id: order_client_id.to_string(),
                reason: reason.as_str().into(),
                timestamp: 0,
            })
        };
        match cmd {
            ExecutionClientDispatchCommand::Submit(order) => {
                self.emit_event(reject(&order));
            }
            ExecutionClientDispatchCommand::SubmitBatch(orders) => {
                for order in &orders {
                    self.emit_event(reject(order));
                }
            }
            ExecutionClientDispatchCommand::Cancel { order_client_id, .. } => {
                self.emit_event(cancel_rejected(&order_client_id));
            }
            ExecutionClientDispatchCommand::CancelBatch { order_client_ids, .. } => {
                for order_client_id in &order_client_ids {
                    self.emit_event(cancel_rejected(order_client_id));
                }
            }
            ExecutionClientDispatchCommand::Modify { order, .. } => {
                self.emit_event(Event::OrderModifyRejected(OrderModifyRejected {
                    order_client_id: order.order_client_id.clone(),
                    reason: reason.as_str().into(),
                    // stamped when made: `vike_bridge_core::rest::amend_answer_time`'s rule
                    timestamp: now_ms(),
                }));
            }
            // A re-query, a teaching, an answer and a stop carry no order: nothing to owe.
            ExecutionClientDispatchCommand::Confirm(_)
            | ExecutionClientDispatchCommand::Adopt(_)
            | ExecutionClientDispatchCommand::WireIds { .. }
            | ExecutionClientDispatchCommand::Resolve { .. }
            | ExecutionClientDispatchCommand::Stop => {}
        }
    }

    /// Append an event to the ring and wake the attached lease. The `seq` is stamped here, under the
    /// ring's lock, before any eviction.
    ///
    /// **THE choke point of every order fact of this account**: the bridge's pumps (through the forwarder),
    /// the loop's answers, the history replay, a lookup's answer and the daemon's own synthesized rejects all
    /// pass here and nowhere else, before the ring, the wire, the tradehub's journal and `recent`. A repeat
    /// of a fact the ring already carries (`OnceMemory`: an order's accept and terminal, an execution) is dropped
    /// HERE and takes no `seq`; the returned `seq` is then the newest one, unchanged. The bookkeeping that needs
    /// the first report of an event still gets it: the forwarder's `learning.note` runs BEFORE this door and sees
    /// every report, and `resting.finished` runs here on every ADMITTED event (a dropped repeat of a fill adds
    /// nothing to it: its own ledger counts each exec id once).
    pub(crate) fn emit_event(&self, event: Event) -> u64 {
        let ts = now_ms();
        let (seq, lease) = {
            let mut inner = lock(&self.inner);
            // An attached lease whose unread events the ring has already evicted will be told to reconcile; what fell
            // out is recovered by the venue's own redelivery, so the memory forgets BEFORE it judges this event, not
            // when the writer next pulls (the restatement may come first). O(1): the lease's control lock nests
            // under `inner`, in the documented order.
            let floor = inner.ring.floor();
            if inner.lease.as_ref().is_some_and(|l| l.next_sequence() < floor) {
                inner.once.clear();
            }
            if inner.once.admit(&event) {
                inner.resting.finished(&event);
                (inner.ring.push(ts, event), inner.lease.clone())
            } else {
                let newest = inner.ring.last_sequence();
                drop(inner);
                // Logged with the lock released: formatting the event is the logger's work, never the ring's.
                tracing::debug!(
                    venue = %self.key.0,
                    "TRADE-LINK: a repeat of an order fact the ring already carries was dropped: {event:?}"
                );
                return newest;
            }
        };
        if let Some(lease) = lease {
            lease.wake_events();
        }
        seq
    }

    /// The dispatch thread admitted a Submit for `order_client_id`: it is a NEW order from here on, so the first-wins memory
    /// forgets any earlier life of the order client id (the Submit dedup set is per adapter, `OnceMemory::forget`).
    pub(crate) fn forget_order_facts(&self, order_client_id: &str) {
        lock(&self.inner).once.forget(order_client_id);
    }

    /// The repeats the first-wins memory has dropped since the session was built.
    pub(crate) fn repeats(&self) -> OnceMemoryRepeats {
        lock(&self.inner).once.repeats()
    }

    /// The ring entry at `seq`, for a lease's writer.
    pub(crate) fn pull(&self, seq: u64) -> EventRingPull {
        let inner = lock(&self.inner);
        if seq > inner.ring.last_sequence() {
            return EventRingPull::Nothing;
        }
        match inner.ring.get(seq) {
            Some(entry) => EventRingPull::Event {
                entry: entry.clone(),
                account: Arc::clone(&inner.route_key),
                gateway_epoch: inner.ring.gateway_epoch(),
            },
            None => EventRingPull::Gap { floor: inner.ring.floor() },
        }
    }

    /// Ask the attached client for a reconcile pass now (the bridge's `reconcile_trigger`). With no client
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

    pub(crate) fn reconcile_tx(&self) -> Option<Sender<ReconcileWorkerJob>> {
        lock(&self.inner).adapter.as_ref().and_then(|a| a.reconcile.as_ref().map(|r| r.tx.clone()))
    }

    /// The lane's word on this venue's price feed (R31): `Some(reason)` while it is mid-gap.
    pub(crate) fn reconcile_degraded(&self) -> Option<String> {
        self.shared.lane.reconcile_degraded(&self.key.0)
    }

    pub(crate) fn reconcile_unavailable_reason(&self) -> String {
        if lock(&self.inner).adapter.is_some() {
            "the venue adapter has no reconcile handle".to_string()
        } else {
            "no venue adapter has been started for this account yet".to_string()
        }
    }

    /// `IdentityRecord`: through the reconcile worker when there is one, else with no handle.
    pub(crate) fn identity_record(&self, rid: u64, lease: &Arc<TradeLeaseHold>) {
        let (tx, bound) = {
            let inner = lock(&self.inner);
            match inner.adapter.as_ref() {
                None => (None, None),
                Some(a) => (a.reconcile.as_ref().map(|r| r.tx.clone()), Some(a.bound_tier)),
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
            && tx.send(ReconcileWorkerJob::Identity { rid, to: Arc::clone(lease) }).is_ok()
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
    /// tells the account's reconcile client to read the symbol from now on (`ReconcileClient::admit_symbol`);
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
    ///
    /// A symbol the venue holds in a margin mode other than cross (`SymbolPropertiesWire::margin_mode`: an
    /// isolated-only perpetual, a spot pair) is answered only to a node that said it books one (`modes`,
    /// decision 0218); to any other it is REFUSED by name ([`mode_refusal`]), because that node would book the
    /// symbol cross.
    pub(crate) fn symbol_facts(
        self: &Arc<Self>,
        rid: u64,
        symbol: String,
        probe: bool,
        quoted: bool,
        modes: bool,
        lease: &Arc<TradeLeaseHold>,
    ) {
        let refuse = |code: SymbolPropertiesRefusalCode, why: &str| {
            let refusal = SymbolPropertiesRefusal { code, reason: why.to_string() };
            lease.send(TradeDown::SymbolPropertiesReply { rid, properties: Err(refusal) });
        };
        let inflight = &self.shared.facts_inflight;
        if inflight.fetch_add(1, Ordering::SeqCst) >= MAX_SYMBOL_FACTS_INFLIGHT {
            inflight.fetch_sub(1, Ordering::SeqCst);
            return refuse(SymbolPropertiesRefusalCode::Busy, PROPERTIES_BUSY);
        }
        let target = self.target(lock(&self.inner).tier);
        let (session, to) = (Arc::clone(self), Arc::clone(lease));
        let spawned = std::thread::Builder::new().name("gw-properties".into()).spawn(move || {
            // Released by DROP, so a lane that panics cannot leak the slot (four leaks would end every
            // later lookup with "too many running").
            let slot = SymbolPropertiesLookupSlot(Arc::clone(&session.shared));
            let facts = if quoted {
                session.shared.lane.symbol_facts_quoted(&target, &symbol)
            } else {
                session.shared.lane.symbol_facts(&target, &symbol)
            };
            // Before the reconcile cue below: a symbol this node cannot book tells the client nothing.
            let facts =
                facts.and_then(|facts| match mode_refusal(&target.venue, &symbol, &facts, modes) {
                    Some(why) => Err(why),
                    None => Ok(facts),
                });
            // Queued BEFORE the reply goes out: the worker is one FIFO and the node's next `Recon`
            // request can only follow the reply, so no pass ever runs between an answer and its
            // admission. The worker is read NOW, after the lookup (which can take seconds), not before
            // it: an adapter restarted meanwhile has a new worker, and the cue must reach that one.
            // Best effort: with no worker there is no pass left to widen.
            if !probe
                && facts.is_ok()
                && let Some(tx) = session.reconcile_tx()
            {
                let _ = tx.send(ReconcileWorkerJob::Admit { symbol: symbol.clone() });
            }
            // AFTER the reconcile cue, so a venue that is slow to answer about leverage delays only
            // this reply, never the symbol's reading.
            let facts = match facts {
                Ok(facts) if !probe => Ok(session.with_symbol_leverage(&target, &symbol, facts)),
                other => other,
            };
            drop(slot);
            let properties = facts.map_err(|reason| SymbolPropertiesRefusal {
                code: properties_refusal_code(&reason),
                reason,
            });
            to.send(TradeDown::SymbolPropertiesReply { rid, properties });
        });
        if spawned.is_err() {
            inflight.fetch_sub(1, Ordering::SeqCst);
            refuse(SymbolPropertiesRefusalCode::NotStarted, PROPERTIES_NOT_STARTED);
        }
    }

    /// `SymbolProperties` with `forget` (decision 0205): the node's engine GAVE `symbol` BACK, so the account's reconcile
    /// client stops reading it and its place in the client's list is free. Queued on the reconcile worker, the same
    /// FIFO the admission cue rides, so a pass queued after this one no longer reads the symbol and one queued
    /// before it still does. Nothing is looked up and nothing is asked of the venue, so it needs no lookup slot and no
    /// thread: the answer is the refusal coded [`SymbolPropertiesRefusalCode::Forgotten`], the one the node reads as
    /// "forgotten".
    ///
    /// An account whose adapter is not started has no reconcile client to tell, and nothing to forget: the answer is
    /// the same (a client built at the next start is told only what the node admits afterwards).
    pub(crate) fn forget_symbol(
        self: &Arc<Self>,
        rid: u64,
        symbol: String,
        lease: &Arc<TradeLeaseHold>,
    ) {
        if let Some(tx) = self.reconcile_tx() {
            let _ = tx.send(ReconcileWorkerJob::Forget { symbol });
        }
        lease.send(TradeDown::SymbolPropertiesReply {
            rid,
            properties: Err(SymbolPropertiesRefusal {
                code: SymbolPropertiesRefusalCode::Forgotten,
                reason: SYMBOL_FORGOTTEN.to_string(),
            }),
        });
    }

    /// `VenueRead`: an operator's READ of the account at its venue (its balance, or its open orders), on a
    /// short-lived thread (a venue HTTP call the lease reader must not wait on). It needs no started
    /// adapter, folds nothing into the ring or any book, and writes nothing at the venue.
    ///
    /// Bounded before the lane is reached ([`venue_read::VenueReadGate`]: one read in flight per venue, a gap per
    /// account and kind); a refused request is answered in words and never touches the venue. The answer is
    /// the lane's, with one screen of the daemon's own: a figure that is not a finite number would write
    /// as `null` and end the connection that decoded it, so such an answer is an error in words instead.
    pub(crate) fn venue_read(
        self: &Arc<Self>,
        rid: u64,
        kind: VenueReadKind,
        lease: &Arc<TradeLeaseHold>,
    ) {
        let target = self.target(lock(&self.inner).tier);
        let slot = match self.shared.venue_reads.admit(
            &target.venue,
            target.account.as_deref(),
            kind,
            Instant::now(),
        ) {
            Ok(slot) => slot,
            Err(why) => {
                lease.send(TradeDown::VenueReadReply { rid, answer: Err(why) });
                return;
            }
        };
        let (session, to) = (Arc::clone(self), Arc::clone(lease));
        let spawned = std::thread::Builder::new().name("gw-trade-read".into()).spawn(move || {
            let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                session.shared.lane.venue_read(&target, kind)
            }));
            let answer = match asked {
                Ok(Ok(read)) if read.is_finite() => Ok(read),
                Ok(Ok(_)) => Err(format!(
                    "{}: the venue's answer carried a figure that is not a number",
                    target.venue
                )),
                Ok(Err(why)) => Err(why),
                Err(_) => Err(format!("{}: the read failed inside the daemon", target.venue)),
            };
            match &answer {
                Ok(_) => tracing::info!(
                    venue = %target.venue, account = target.account.as_deref().unwrap_or("default"),
                    "{}: an operator's {} read was answered", target.venue, kind.word()
                ),
                Err(why) => tracing::warn!(
                    venue = %target.venue, account = target.account.as_deref().unwrap_or("default"),
                    "{}: an operator's {} read was not answered: {why}", target.venue, kind.word()
                ),
            }
            // The slot goes back BEFORE the reply is written, so a client that asks the venue's other read
            // the moment it hears this one is not told the venue is busy.
            drop(slot);
            to.send(TradeDown::VenueReadReply { rid, answer });
        });
        if spawned.is_err() {
            lease.send(TradeDown::VenueReadReply {
                rid,
                answer: Err("the daemon could not start a thread for this read".to_string()),
            });
        }
    }

    /// `facts` with the leverage the venue confirmed for `symbol`, when the lane has one
    /// ([`TradeLane::symbol_leverage`]): asked of the lane with the RUNNING adapter's own start (its
    /// `account_leverage` is the figure the node asked). No adapter, or a dead one, asks nothing. A
    /// refusal or failure leaves `facts` as the lookup made them and says why in the daemon's log, so the
    /// node leaves the symbol at the engine's rate: never a half-applied or clamped figure.
    fn with_symbol_leverage(
        &self,
        target: &TradeLaneTarget,
        symbol: &str,
        mut facts: SymbolPropertiesWire,
    ) -> SymbolPropertiesWire {
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
            last_sequence: inner.ring.last_sequence(),
            ring_floor: inner.ring.floor(),
            commands_received: inner.commands,
            // Read under the session lock, which the reader takes for a plain value (it never calls back here).
            venue_link: inner
                .adapter
                .as_ref()
                .and_then(|a| a.link_report.as_ref())
                .and_then(|read| read()),
        }
    }

    // ---- the adapter -------------------------------------------------------------------------------

    /// Start (or, for the same request, confirm) the account's venue adapter. Runs on the lease's reader
    /// thread and may take as long as a venue login takes; the writer keeps the heartbeat going.
    pub(crate) fn start_adapter(self: &Arc<Self>, start: AdapterStart) -> AdapterReply {
        // One instrument, one spelling (the C-D4 follow-up): a mount or a leg the control door writes another way
        // is not started, in the grammar's words, rather than signed under a second name.
        if let Some(why) = std::iter::once(&start.symbol)
            .chain(&start.declared_legs)
            .find_map(|symbol| vike_catalog::respelled_symbol(&self.key.0, symbol))
        {
            return AdapterReply::NotRunning {
                block_word: word(ArmingBlock::AdapterFailed),
                refusal_text: format!("gatewayhub: {why}"),
            };
        }
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
            if let AdapterReply::Running {
                leverage_cap,
                account_currency: account_ccy,
                account_currency_assumed: account_currency_assumed_value,
                native_time_release,
                ..
            } = &mut reply
                && !restore
            {
                *leverage_cap = sh.lane.leverage_cap(&self.target(tier), &start);
                // A restore stored `false` (it asked nothing); the answer is the lane's, no venue call, so ask again.
                *native_time_release = sh.lane.native_time_release(&self.target(tier), &start);
                // ... and so is the account's currency, when the first ask missed: kept for the adapter's life,
                // a miss would leave the node refusing every symbol quoted in another currency until restart.
                // A RESTORE asks the venue for no currency (its reply goes to nobody), so the reply it stored
                // states none, and replaying that to a node that asked for the currency would leave its ledger
                // unanchored after any nightly re-login: the same throttled ask answers both. A venue whose
                // statement is paid for out of a budget (`currency_read_is_budgeted`: ibkr's data lane, ig's
                // allowance) is asked only by a start that asks for it (`read_account_currency`).
                if (account_ccy.is_none() || *account_currency_assumed_value)
                    && (start.read_account_currency
                        || !sh.lane.currency_read_is_budgeted(&self.target(tier)))
                    && let Some((currency, assumed)) =
                        self.retry_account_currency(&sh, tier, &start)
                {
                    *account_ccy = Some(currency);
                    *account_currency_assumed_value = assumed;
                }
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
        let (tx, mut rx) = vike_execution::event_channel(sh.config.event_channel);
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
        // them and stamps a replayed fill with the instrument IB echoes (`AAPL.IBEOS.USD`, `EUR.IDEALPRO.USD`),
        // which never equals the symbol the engine mounted; the engine drops such a fill in silence and the loss
        // is permanent (IB replays only today's executions, and the bridge's `emitted` map blocks a second
        // delivery). The call is made even when there is nothing to teach: a bridge that holds its replay for its
        // host (the IBKR socket bridge) lets it go on this first teaching. Teaches only what THIS daemon sent; the
        // orders only the tradehub knew arrive with its own `adopt_restored`, later, and the bridge drops the
        // replay of an order it was not taught (the reconcile reports the fill).
        // Only for a SUPERVISED venue (the one whose bridge is rebuilt after a loss) or when there is something to
        // teach: every other venue's client is left exactly as it was (a refactor changes no working behaviour).
        let watch = (!restore).then(|| sh.lane.link_watch(&target, &start)).flatten();
        let link_report = sh.lane.venue_link(&target, &start);
        let taught = lock(&self.inner).resting.symbols();
        if restore || watch.is_some() || !taught.is_empty() {
            client.adopt_restored(&taught);
        }
        let has_reconcile = started.reconcile.is_some();
        // The venue's word on the account's stated leverage (decision 0181): a restore's reply goes to
        // nobody, so it asks the venue nothing; a lane with nothing to confirm answers `None`.
        let leverage_cap = if restore { None } else { sh.lane.leverage_cap(&target, &start) };
        // The currency the venue says the account's money is in (decision 0183, item 1): the node anchors
        // the engine's ledger to it. A restore's reply goes to nobody, so it asks the venue nothing.
        let (account_ccy, account_currency_assumed_value) =
            if restore { (None, false) } else { sh.lane.account_currency_stated(&target, &start) };
        // Whether the venue holds a time-only release itself (decision 0267's 2.6); a restore's reply goes to nobody.
        let native_time_release =
            if restore { false } else { sh.lane.native_time_release(&target, &start) };
        // The length of the broker code the bridge's start just stamped into its ids (decision 0270), asked of the
        // same generation, so a reply replayed to a later link still states the code this adapter runs with.
        let broker_code_len = sh.lane.broker_code_len(&target, &start);
        let reply = AdapterReply::Running {
            bound_tier: started.bound_tier,
            grid: started.grid,
            contract_value: started.contract_value,
            margin_mode: started.margin_mode,
            leg_grids: started.leg_grids,
            reconcile: has_reconcile,
            leverage_cap,
            account_currency: account_ccy,
            account_currency_assumed: account_currency_assumed_value,
            native_time_release,
            broker_code_len,
        };
        let (command_tx, command_rx) =
            mpsc::sync_channel::<ExecutionClientDispatchCommand>(sh.config.command_queue.max(1));
        let dead = Arc::new(AtomicBool::new(false));
        let ctx = ExecutionClientDispatchContext {
            client,
            session: Arc::clone(self),
            venue: self.key.0.clone(),
            symbol: start.symbol.clone(),
            budget: Arc::clone(&sh.budget),
            halt_path: sh.config.halt_path.clone(),
            halt_admit: start.halt_admit,
            resolves_by_order_client_id: sh.lane.resolves_by_order_client_id(&self.key.0),
            dedup: DedupSet::new(sh.config.dedup_order_client_ids),
            dead: Arc::clone(&dead),
        };
        let exec = match std::thread::Builder::new()
            .name("gw-trade-exec".into())
            .spawn(move || dispatch::run(command_rx, ctx))
        {
            Ok(h) => h,
            Err(e) => {
                return self.start_failed(format!("could not start the dispatch thread: {e}"));
            }
        };
        let worker = started.reconcile.map(|client| {
            recon::spawn(ReconcileWorkerContext {
                client: Arc::new(Mutex::new(client)),
                lane: Arc::clone(&sh.lane),
                target: target.clone(),
                bound_tier: started.bound_tier,
                progress: sh.config.reconcile_progress,
                page_bytes: sh.config.reconcile_page_bytes,
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
        // The row whose keys this adapter signs with, for a client that named none (`target` already addresses a
        // renamed account by its row's label, so this is the same row). Asked before any lock: no lane call under one.
        if let Some(id) = sh.lane.row_of(&target.venue, target.account.as_deref(), target.tier) {
            lock(&self.row).id = Some(id);
        }
        let blind = {
            let mut inner = lock(&self.inner);
            inner.route_key = Arc::from(start.route_key.as_str());
            if !restore {
                inner.link = link::VenueLinkSupervisorState::Up;
                inner.parked = None;
            }
            inner.adapter = Some(Adapter {
                start,
                reply: reply.clone(),
                // A restore asked the venue nothing, so a later start is due to ask at once.
                account_currency_asked: (!restore).then(Instant::now),
                bound_tier: target.tier,
                command_tx,
                reconcile: recon,
                exec: Some(exec),
                dead,
                reported,
                learning: Arc::clone(&learning),
                link_report,
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
        tracing::info!(venue = %self.key.0, recon = has_reconcile, "TRADE-OPEN: the venue adapter is up");
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

    /// **Ask the venue the account's currency AGAIN** for the running adapter whose reply carries none (the first
    /// ask missed, or a restore asked nothing): `Some` when it answers, remembered in the adapter's own reply so
    /// the next start is answered from it; `None` when it is not due or the venue still states none (the reply
    /// stays as it was). Bounded: not sooner than `TradeConfig::account_currency_retry_after` after the last
    /// ask, which is marked BEFORE the venue is asked so a slow or failing ask is not repeated by the next
    /// start; and single-flight, because the caller holds the session's adapter lock for the whole start.
    fn retry_account_currency(
        &self,
        sh: &AccountSessionsShared,
        tier: AccountTier,
        start: &AdapterStart,
    ) -> Option<(String, bool)> {
        let after = sh.config.account_currency_retry_after;
        {
            let mut inner = lock(&self.inner);
            let adapter = inner.adapter.as_mut()?;
            if adapter.account_currency_asked.is_some_and(|asked| asked.elapsed() < after) {
                return None;
            }
            adapter.account_currency_asked = Some(Instant::now());
        }
        // The STATED form: a venue that fills one in under a ruling (fxcm's assumed USD) says so, and the flag is
        // kept beside the currency in the stored reply.
        let (currency, assumed) = sh.lane.account_currency_stated(&self.target(tier), start);
        let currency = currency?;
        if let Some(adapter) = lock(&self.inner).adapter.as_mut()
            && let AdapterReply::Running {
                account_currency: account_ccy,
                account_currency_assumed: account_currency_assumed_value,
                ..
            } = &mut adapter.reply
        {
            // An ASSUMED reply being asked again (the owner's USD default) that is assumed again learned nothing.
            if assumed
                && *account_currency_assumed_value
                && account_ccy.as_deref() == Some(currency.as_str())
            {
                return None;
            }
            *account_ccy = Some(currency.clone());
            *account_currency_assumed_value = assumed;
        }
        tracing::info!(
            venue = %self.key.0,
            "TRADE-OPEN: the account's currency ({currency}) was read on a later start; the node anchors to it"
        );
        Some((currency, assumed))
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
        let mut stop = ExecutionClientDispatchCommand::Stop;
        loop {
            match adapter.command_tx.try_send(stop) {
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
        if let Some(worker) = adapter.reconcile.take() {
            let _ = worker.tx.send(ReconcileWorkerJob::Stop);
            join_bounded(worker.handle, JOIN_WAIT, "gw-trade-recon");
        }
        if let Some(exec) = adapter.exec.take() {
            join_bounded(exec, JOIN_WAIT, "gw-trade-exec");
        }
        // The pumps are gone with the client: the lane's senders dropped, the channels closed.
        if let Some(h) = adapter.forwarder.take() {
            join_bounded(h, Duration::from_secs(2), "gw-trade-events");
        }
        if let Some(h) = adapter.trigger.take() {
            join_bounded(h, Duration::from_secs(2), "gw-trade-trigger");
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
        let repeats = self.repeats();
        tracing::info!(
            venue = %self.key.0,
            repeats_accepted = repeats.accepted,
            repeats_terminal = repeats.terminal,
            repeats_fill = repeats.fill,
            "TRADE-LINK: session retired: {why}"
        );
    }
}

/// A fresh random, non-zero `gateway_epoch` for one session (`0` means "none" on the wire).
fn mint_epoch() -> u64 {
    let epoch =
        fresh_nonce()[..8].iter().fold(0u64, |accumulator, b| (accumulator << 8) | u64::from(*b));
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
    std::thread::Builder::new().name("gw-trade-events".into()).spawn(move || {
        while let Some(ingest) = next() {
            match ingest {
                Ingest::Event(event) => {
                    // The order client ids the bridge announces are what a hold is judged against (`link` module).
                    learning.note(&event);
                    session.emit_event(event);
                }
                // The adapter disclosing its venue link (the IBKR bridge does both, W14c B). DOWN: the
                // supervisor tears the adapter down and rebuilds it, without waiting for a probe. UP: the
                // bridge has LEARNED the venue (its open orders are rebound, the day's executions
                // replayed), which releases a session held because a rebuilt bridge could not manage the
                // orders of the one before it. The daemon consumes both here and forwards neither: a core
                // would read them as a trade link to defer its watchdog for.
                Ingest::TradeLink(update) => {
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
        Ingest::TradeLink(_) => "TradeLink",
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
        Ingest::FundingRate(_) => "FundingRate",
        Ingest::Accounts(_) => "Accounts",
        Ingest::MountReply(_) => "MountReply",
    }
}

/// The trigger relay: a bridge's reconcile trigger becomes a `ReconcileNow` for the attached client.
fn spawn_trigger(
    rx: mpsc::Receiver<()>,
    session: Arc<AccountSession>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name("gw-trade-trigger".into()).spawn(move || {
        while rx.recv().is_ok() {
            session.reconcile_now("the venue adapter asked for a reconcile pass");
        }
    })
}
