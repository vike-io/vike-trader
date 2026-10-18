//! The state one lease shares between its threads: the fold-side command channel, the link thread, the
//! reader, the reconcile thread and the boot thread.
//!
//! One rule shapes everything here: **the fold thread only ever does a channel send** (`Shared::tx`).
//! Nothing it can reach takes a lock, serializes or writes a socket; the link thread owns the socket
//! and the ledger, the reader owns the inbound stream, and everything they must tell each other is a
//! `Msg` or one of the small mutexes below (held for a map lookup, never across I/O or a blocking
//! send, except `delivery`, whose whole job is to be held across the blocking send to the core).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::Duration;

use vike_exec::{EventSender, GatewayCursor, GatewayEvent};
use vike_model::AccountTier;
use vike_model::events::{Event, OrderAccepted, OrderCancelRejected, OrderSubmitted};

use super::cursor::{Admit, CursorBook};
use super::disclosure::LinkSignal;
use super::ids::WireIds;
use super::mirror::{
    ended_coid, implied_prefix_ts, resolved_coid, synthetic_cancel_coid, wrapped_fill,
};
use super::told::Told;
use crate::client::{GatewayLink, TradeOpenError, TradeStream};
use crate::proto::{
    FETCH_STALL_FLOOR, GW_TRADE_READ_FLOOR, OrderResolution, REPLY_IDLE_TIMEOUT, ResolveState,
    TradeBye, TradeDown, TradeOpened, TradeUp,
};

/// Lock, ignoring poisoning: a panic on another thread must not turn every later lock into a second
/// panic on the fold thread's neighbours (none of these mutexes guards an invariant a panic breaks).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How many ended coids the lease remembers (W2G-6): a bound, not a policy. An order that ends is rarely asked
/// about again, and the memory only ever suppresses a mark that would have been refused anyway.
const ENDED_MEMORY: usize = 16_384;

/// The non-terminal advisory a bridge's SYNTHETIC cancel becomes when it answers a cancel of an UNKNOWN submit
/// (W2G-1): the bridge cannot tell "cancelled" from "too late, already filled, not found" there.
pub(crate) const SYNTHETIC_CANCEL_ADVISORY: &str = "gatewayhub trade: the bridge answered the cancel of an \
     UNKNOWN submit with its own synthetic cancel (no venue time), which also means \"too late / already \
     filled / not found\"; the order is NOT treated as cancelled until the venue's own word, the lookup or \
     reconcile says so";

/// A bounded memory of strings: the oldest is forgotten first.
#[derive(Default)]
struct Recent {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl Recent {
    fn insert(&mut self, id: &str) {
        if self.set.insert(id.to_string()) {
            self.order.push_back(id.to_string());
            if self.order.len() > ENDED_MEMORY
                && let Some(oldest) = self.order.pop_front()
            {
                self.set.remove(&oldest);
            }
        }
    }

    fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }
}

/// Which account a lease is for. No secret: the keys live in the [`GatewayLink`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeAccount {
    /// The caller's id for the lease, stable across its reconnects (a process). The daemon keys
    /// `received_through_cseq` and the explicit takeover on it.
    pub client_id: String,
    /// A `vike_model::VENUES` slug.
    pub venue: String,
    /// The account label's text; `None` is the unlabelled default account.
    pub label: Option<String>,
    /// The tier the caller believes the account runs at.
    pub tier: AccountTier,
    /// The account's route key as the core spells an account's engine (`account_route_key`: the bare
    /// venue id for a default account). Every cursor this lease hands the core carries it, and a frame
    /// naming another account is never folded in.
    pub route_key: String,
}

/// Every wait the lease makes that the gateway link's own [`crate::client::GatewayTimings`] does not
/// already carry. The defaults ARE the protocol floors and production only RAISES them (the composition
/// root resolves a setting row and passes `max(floor, row)`; [`TradeTimings::raised`] is that rule); a
/// test narrows them so a stall is watched in milliseconds. ⚠ This crate reads no setting and clamps
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeTimings {
    /// The floor of the read deadline: `max(read_floor, 3 x the heartbeat the daemon stated)`.
    pub read_floor: Duration,
    /// How long a request (adapter start, wire ids, resolve, identity, status) may wait for its answer.
    /// An adapter start connects a venue (a JForex sidecar, a cTrader handshake), so this is the
    /// server-bounded class, [`REPLY_IDLE_TIMEOUT`].
    pub request_reply: Duration,
    /// The floor of a reconcile fetch's per-frame idle bound: `max(floor, 3 x ReconProgress.next_in_ms)`.
    pub recon_stall_floor: Duration,
    /// How long `detach` waits for the link thread to flush what is queued and say goodbye.
    pub detach_flush: Duration,
}

impl Default for TradeTimings {
    fn default() -> Self {
        Self {
            read_floor: GW_TRADE_READ_FLOOR,
            request_reply: REPLY_IDLE_TIMEOUT,
            recon_stall_floor: FETCH_STALL_FLOOR,
            detach_flush: Duration::from_secs(5),
        }
    }
}

impl TradeTimings {
    /// The defaults, raised to `read_floor` and `recon_stall_floor` where those are ABOVE the protocol
    /// floors: the rule a production root applies to its settings rows. A value below a floor is the
    /// floor.
    #[must_use]
    pub fn raised(read_floor: Duration, recon_stall_floor: Duration) -> Self {
        let floors = Self::default();
        Self {
            read_floor: floors.read_floor.max(read_floor),
            recon_stall_floor: floors.recon_stall_floor.max(recon_stall_floor),
            ..floors
        }
    }
}

/// Why a lease request did not produce its answer. Every text is a complete `gatewayhub <addr>: ...`
/// sentence; none carries a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TradeError {
    /// The daemon does not advertise the string the verb needs. Nothing was sent.
    Unsupported(String),
    /// The request was NOT sent: the link is down or the lease is closed.
    LinkDown(String),
    /// The request was sent and the link was lost before the answer.
    LinkLost(String),
    /// The answer did not come inside its bound.
    Timeout(String),
    /// The daemon answered with a frame this verb cannot have (a desync) or a refusal.
    Protocol(String),
}

impl TradeError {
    /// The sentence.
    pub fn text(&self) -> &str {
        match self {
            TradeError::Unsupported(t)
            | TradeError::LinkDown(t)
            | TradeError::LinkLost(t)
            | TradeError::Timeout(t)
            | TradeError::Protocol(t) => t,
        }
    }
}

impl std::fmt::Display for TradeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

impl std::error::Error for TradeError {}

/// Why a symbol-facts request produced no facts: what a node may REMEMBER (30 s) and what it must ask
/// again. Each carries the operator's sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolFactsError {
    /// A definitive answer about the symbol or the gatewayhub: unknown instrument, no tick size, a
    /// venue that does not answer, a gatewayhub without the verb.
    Refused(String),
    /// Not an answer about the symbol: the link was down or lost, the answer timed out, the daemon was
    /// restarting or busy, or the venue's list could not be fetched. Never remembered.
    Unavailable(String),
}

impl SymbolFactsError {
    /// The sentence.
    pub fn text(&self) -> &str {
        match self {
            SymbolFactsError::Refused(t) | SymbolFactsError::Unavailable(t) => t,
        }
    }
}

impl std::fmt::Display for SymbolFactsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

impl std::error::Error for SymbolFactsError {}

/// What the link thread is told. Command frames arrive with a placeholder `cseq` (`0`): the link thread
/// numbers a frame when it WRITES it, so a number is never burned by a frame that never left.
#[derive(Debug)]
pub(crate) enum Msg {
    /// A command frame (an order, a cancel, a modify, a confirm, the teaching).
    Frame(Box<TradeUp>),
    /// A request frame; its waiter was registered under its `rid` before this was sent.
    Ask(Box<TradeUp>),
    /// The daemon acknowledged command `cseq` on connection `conn`.
    Acked { conn: u64, cseq: u64 },
    /// The daemon answered an `AdapterStart` on connection `conn` (the adapter is up, or it said why not).
    ///
    /// `hold` is the caller's word (read from `Shared::hold_teaching`) that it will teach the new adapter
    /// and call `TradeSession::release_held` itself: the held cancels then wait for that.
    AdapterAnswered { conn: u64, hold: bool },
    /// The caller finished teaching the restarted adapter: release the held cancels
    /// (`TradeSession::release_held`). FIFO behind every `Frame` the caller sent before it, which is
    /// the whole point.
    ReleaseHeld {
        /// Give up on the adapter: release even if it never answered.
        anyway: bool,
    },
    /// The reader of connection `conn` ended.
    ReaderClosed { conn: u64, why: String },
    /// The daemon said goodbye on connection `conn`.
    Bye { conn: u64, bye: TradeBye },
    /// A reconnect attempt finished.
    Dialed(Result<TradeStream, TradeOpenError>),
    /// Phase one or two of a teardown.
    Detach(crate::proto::DetachPhase),
    /// Flush, say goodbye, exit.
    Stop,
}

/// What a waiting request receives.
#[derive(Debug)]
pub(crate) enum Reply {
    /// A frame answering the request (a progress or a page frame leaves the waiter registered).
    Frame(Box<TradeDown>),
    /// The request was never sent: the link was down.
    Down,
    /// The request was sent and the link was lost before the answer.
    Lost(String),
}

enum Waiter {
    /// A thread is blocked on this channel.
    Chan(Sender<Reply>),
    /// Nobody waits; the answer is recorded (`ResolveOrders` after an epoch change).
    Resolve,
}

/// The link's state, for observers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// Connected and open.
    Up,
    /// Down; the link thread is redialling.
    Down,
    /// Closed for good (taken over, refused, stopped): no more dials.
    Closed,
}

const STATE_UP: u8 = 0;
const STATE_DOWN: u8 = 1;
const STATE_CLOSED: u8 = 2;

/// Counters of what the readers did, for the status view and the tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TradeLinkStats {
    /// Events delivered to the core.
    pub events_delivered: u64,
    /// Of those, events the ring replayed after a resume.
    pub events_replayed: u64,
    /// Events dropped as duplicates by the cursor guard.
    pub events_duplicate: u64,
    /// Events naming another account, dropped (a daemon bug; logged at `error`).
    pub events_misrouted: u64,
    /// Implied `OrderSubmitted` + `OrderAccepted` pairs sent ahead of the venue's word on an UNKNOWN submit (W2F-D1).
    pub prefixes_synthesized: u64,
    /// Connections established after the first.
    pub reconnects: u64,
    /// Reconnects that landed on a different `gw_epoch` (the gatewayhub restarted, or retired the session).
    pub epoch_changes: u64,
}

#[derive(Default)]
struct Counters {
    events_delivered: AtomicU64,
    events_replayed: AtomicU64,
    events_duplicate: AtomicU64,
    events_misrouted: AtomicU64,
    prefixes_synthesized: AtomicU64,
    reconnects: AtomicU64,
    epoch_changes: AtomicU64,
}

/// What a reader does after a frame.
pub(crate) enum Flow {
    Continue,
    Stop(String),
}

/// The shared state of one lease. See the module doc.
pub(crate) struct Shared {
    pub link: GatewayLink,
    pub account: TradeAccount,
    pub timings: TradeTimings,
    pub halt_path: Option<PathBuf>,
    /// The core's lane for this account (already `routed`): synthesized events and the relay.
    pub events: EventSender,
    pub trigger: Option<Sender<()>>,
    pub signal: Arc<dyn LinkSignal>,
    /// The fold-side door to the link thread.
    pub tx: Sender<Msg>,
    state: AtomicU8,
    rid: AtomicU64,
    pending: Mutex<HashMap<u64, Waiter>>,
    pub wire_ids: RwLock<WireIds>,
    /// The symbols the daemon's reconcile client was told the engine admitted (`told.rs`).
    pub told: Told,
    /// Serializes delivery to the core across the readers of successive connections: held across the
    /// (possibly blocking) send so a late event of an old connection cannot overtake a newer one.
    delivery: Mutex<()>,
    cursor: Mutex<CursorBook>,
    unknown: Mutex<BTreeSet<String>>,
    /// UNKNOWN coids for which a cancel of OURS was Written (W2G-1): a synthetic cancel for one of these is
    /// the bridge's answer to that cancel. Leaves with the coid's UNKNOWN mark.
    cancel_sent: Mutex<BTreeSet<String>>,
    /// Coids whose terminal event this lease delivered (W2G-6): they are never marked UNKNOWN afterwards.
    ended: Mutex<Recent>,
    resolutions: Mutex<HashMap<String, ResolveState>>,
    opened: Mutex<Option<TradeOpened>>,
    features: Mutex<Vec<String>>,
    closed_because: Mutex<Option<String>>,
    core_gone: AtomicBool,
    /// A teardown has begun (`begin_detach` / `detach` / the last handle dropped): nothing new should
    /// be started on this lease (the tradehub's re-teaching worker reads it).
    stopping: AtomicBool,
    /// The next `AdapterStart` answer is one the caller will follow with its own teaching and a
    /// `TradeSession::release_held` (`TradeSession::restart_adapter`): the ledger keeps the held cancels for it.
    hold_teaching: AtomicBool,
    /// A `ReplayUnavailable` arrived and no event has been delivered since: the next jump in `seq` is the one
    /// it announced (see [`CursorBook::skipped`]).
    gap_told: AtomicBool,
    counters: Counters,
}

impl Shared {
    pub fn new(
        link: GatewayLink,
        account: TradeAccount,
        timings: TradeTimings,
        halt_path: Option<PathBuf>,
        events: EventSender,
        trigger: Option<Sender<()>>,
        signal: Arc<dyn LinkSignal>,
        resume: Option<GatewayCursor>,
    ) -> (Arc<Self>, mpsc::Receiver<Msg>) {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Self {
            link,
            account,
            timings,
            halt_path,
            events,
            trigger,
            signal,
            tx,
            state: AtomicU8::new(STATE_DOWN),
            rid: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            wire_ids: RwLock::new(WireIds::default()),
            told: Told::default(),
            delivery: Mutex::new(()),
            cursor: Mutex::new(CursorBook::new(resume)),
            unknown: Mutex::new(BTreeSet::new()),
            cancel_sent: Mutex::new(BTreeSet::new()),
            ended: Mutex::new(Recent::default()),
            resolutions: Mutex::new(HashMap::new()),
            opened: Mutex::new(None),
            features: Mutex::new(Vec::new()),
            closed_because: Mutex::new(None),
            core_gone: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            hold_teaching: AtomicBool::new(false),
            gap_told: AtomicBool::new(false),
            counters: Counters::default(),
        });
        (shared, rx)
    }

    // ---- state ------------------------------------------------------------------------------------

    pub fn link_state(&self) -> LinkState {
        match self.state.load(Ordering::Acquire) {
            STATE_UP => LinkState::Up,
            STATE_DOWN => LinkState::Down,
            _ => LinkState::Closed,
        }
    }

    pub fn set_up(&self) {
        // A closed lease never comes back up.
        let _ =
            self.state.compare_exchange(STATE_DOWN, STATE_UP, Ordering::AcqRel, Ordering::Acquire);
    }

    pub fn set_down(&self) {
        let _ =
            self.state.compare_exchange(STATE_UP, STATE_DOWN, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Close the lease for good and remember why.
    pub fn set_closed(&self, why: &str) {
        self.state.store(STATE_CLOSED, Ordering::Release);
        let mut slot = lock(&self.closed_because);
        if slot.is_none() {
            *slot = Some(why.to_string());
        }
    }

    /// Why the lease is closed, if it is.
    pub fn closed_reason(&self) -> Option<String> {
        lock(&self.closed_because).clone()
    }

    /// The sentence a refusal of a command on a closed or unreachable lease carries.
    pub fn unavailable_text(&self) -> String {
        match self.closed_reason() {
            Some(why) => format!("gatewayhub {}: trade lease closed: {why}", self.link.addr()),
            None => format!("gatewayhub {}: trade link unavailable", self.link.addr()),
        }
    }

    /// Whether a delivery found the core gone (the process is shutting down).
    pub fn core_gone(&self) -> bool {
        self.core_gone.load(Ordering::Acquire)
    }

    /// A teardown has begun on this lease.
    pub fn mark_stopping(&self) {
        self.stopping.store(true, Ordering::Release);
    }

    /// Whether a teardown has begun.
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    /// The next `AdapterStart` answer is held for the caller's teaching (see `Shared::hold_teaching`).
    pub fn hold_next_answer(&self) {
        self.hold_teaching.store(true, Ordering::Release);
    }

    pub fn stats(&self) -> TradeLinkStats {
        let c = &self.counters;
        TradeLinkStats {
            events_delivered: c.events_delivered.load(Ordering::Relaxed),
            events_replayed: c.events_replayed.load(Ordering::Relaxed),
            events_duplicate: c.events_duplicate.load(Ordering::Relaxed),
            events_misrouted: c.events_misrouted.load(Ordering::Relaxed),
            prefixes_synthesized: c.prefixes_synthesized.load(Ordering::Relaxed),
            reconnects: c.reconnects.load(Ordering::Relaxed),
            epoch_changes: c.epoch_changes.load(Ordering::Relaxed),
        }
    }

    pub fn count_reconnect(&self, epoch_changed: bool) {
        self.counters.reconnects.fetch_add(1, Ordering::Relaxed);
        if epoch_changed {
            self.counters.epoch_changes.fetch_add(1, Ordering::Relaxed);
        }
    }

    // ---- the open reply and the cursor -------------------------------------------------------------

    pub fn set_opened(&self, opened: TradeOpened, features: Vec<String>) {
        let missed = lock(&self.cursor).opened(opened.gw_epoch, opened.event_ring_floor == Some(0));
        *lock(&self.opened) = Some(opened);
        *lock(&self.features) = features;
        if missed {
            // No cursor names where this client stood (the ring held events when it first opened) and it
            // has delivered nothing since: whatever the daemon produced during the flap is not replayable.
            tracing::warn!(
                "gatewayhub trade: the link flapped before this client heard its first event, and the \
                 ring cannot be replayed from a position it never held; reconcile will find what it missed"
            );
            self.trigger_reconcile();
        }
    }

    pub fn opened(&self) -> Option<TradeOpened> {
        lock(&self.opened).clone()
    }

    pub fn has_feature(&self, feature: &str) -> bool {
        lock(&self.features).iter().any(|f| f == feature)
    }

    /// Refuse locally, naming the string, unless the daemon advertised `feature`.
    pub fn require(&self, feature: &'static str) -> Result<(), TradeError> {
        if self.has_feature(feature) {
            return Ok(());
        }
        Err(TradeError::Unsupported(format!(
            "gatewayhub at {} does not advertise {feature:?} - the daemon predates this verb or was \
             built without the lane; upgrade vike-gatewayhub. Nothing was sent.",
            self.link.addr()
        )))
    }

    pub fn cursor(&self) -> Option<GatewayCursor> {
        lock(&self.cursor).last()
    }

    /// What a redial presents as `TradeOpen.resume`: the delivered cursor, or the anchor of the epoch
    /// (`CursorBook::resume`).
    pub fn resume_cursor(&self) -> Option<GatewayCursor> {
        lock(&self.cursor).resume()
    }

    // ---- UNKNOWN orders ----------------------------------------------------------------------------

    /// Mark `coids` UNKNOWN, except the ones whose terminal event this lease already delivered (W2G-6: an
    /// ended order has nothing left to resolve, and a lookup's re-statement of its ending would earn a prefix
    /// the core refuses). Returns the coids actually marked.
    pub fn mark_unknown(&self, coids: impl IntoIterator<Item = String>) -> Vec<String> {
        let open: Vec<String> = {
            let ended = lock(&self.ended);
            coids.into_iter().filter(|c| !ended.contains(c)).collect()
        };
        lock(&self.unknown).extend(open.iter().cloned());
        open
    }

    /// The UNKNOWN set's lock, held by a test to park whoever marks a coid (`attach` parks in `mark_unknown`).
    #[cfg(test)]
    pub(crate) fn hold_unknown_for_test(&self) -> MutexGuard<'_, BTreeSet<String>> {
        lock(&self.unknown)
    }

    fn is_unknown(&self, coid: &str) -> bool {
        lock(&self.unknown).contains(coid)
    }

    /// A cancel frame is about to be written: remember which UNKNOWN coids it names (W2G-1), so a synthetic
    /// cancel that comes back for one of them is read as the answer to THIS cancel. Returns the coids newly
    /// remembered, for [`Shared::forget_cancel_written`] if the write fails.
    pub fn note_cancel_written(&self, frame: &TradeUp) -> Vec<String> {
        let named: Vec<&String> = if let TradeUp::Cancel { coid, .. } = frame {
            vec![coid]
        } else if let TradeUp::CancelBatch { coids, .. } = frame {
            coids.iter().collect()
        } else {
            return Vec::new();
        };
        let mine: Vec<String> = {
            let unknown = lock(&self.unknown);
            named.into_iter().filter(|c| unknown.contains(c.as_str())).cloned().collect()
        };
        if mine.is_empty() {
            return mine;
        }
        let mut sent = lock(&self.cancel_sent);
        mine.into_iter().filter(|c| sent.insert(c.clone())).collect()
    }

    /// The write that [`Shared::note_cancel_written`] preceded failed: the cancel was never sent.
    pub fn forget_cancel_written(&self, coids: &[String]) {
        let mut sent = lock(&self.cancel_sent);
        for c in coids {
            sent.remove(c);
        }
    }

    pub fn unknown_orders(&self) -> Vec<String> {
        lock(&self.unknown).iter().cloned().collect()
    }

    pub fn resolution_of(&self, coid: &str) -> Option<ResolveState> {
        lock(&self.resolutions).get(coid).copied()
    }

    // ---- requests -----------------------------------------------------------------------------------

    pub fn next_rid(&self) -> u64 {
        self.rid.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Register a waiter for `rid` and hand `frame` to the link thread. The returned receiver gets the
    /// answer frames; drop it (and [`Shared::forget`]) when giving up.
    pub fn ask(&self, rid: u64, frame: TradeUp) -> Result<mpsc::Receiver<Reply>, TradeError> {
        let (tx, rx) = mpsc::channel();
        lock(&self.pending).insert(rid, Waiter::Chan(tx));
        if self.tx.send(Msg::Ask(Box::new(frame))).is_err() {
            lock(&self.pending).remove(&rid);
            return Err(TradeError::LinkDown(self.unavailable_text()));
        }
        Ok(rx)
    }

    /// As [`Shared::ask`] with nobody waiting: the answer is recorded in [`Shared::resolution_of`].
    pub fn ask_resolve(&self, rid: u64, frame: TradeUp) {
        lock(&self.pending).insert(rid, Waiter::Resolve);
        if self.tx.send(Msg::Ask(Box::new(frame))).is_err() {
            lock(&self.pending).remove(&rid);
        }
    }

    /// Stop waiting for `rid`.
    pub fn forget(&self, rid: u64) {
        lock(&self.pending).remove(&rid);
    }

    /// Route an answer frame to its waiter. `terminal` removes the waiter; a progress or a "more pages"
    /// frame keeps it.
    fn complete(&self, rid: u64, down: TradeDown, terminal: bool) {
        let waiter = {
            let mut pending = lock(&self.pending);
            if terminal { pending.remove(&rid) } else { pending.get(&rid).map(clone_waiter) }
        };
        match waiter {
            Some(Waiter::Chan(tx)) => {
                // A receiver that gave up is fine: the answer has nobody to go to.
                let _ = tx.send(Reply::Frame(Box::new(down)));
            }
            Some(Waiter::Resolve) => {
                if let TradeDown::OrdersResolved { results, .. } = down {
                    self.record_resolutions(results);
                }
            }
            None => tracing::debug!(rid, "gatewayhub trade: an answer with no waiter (it gave up)"),
        }
    }

    pub fn record_resolutions(&self, results: Vec<OrderResolution>) {
        let mut seen = lock(&self.resolutions);
        for r in results {
            seen.insert(r.coid, r.state);
        }
    }

    /// Fail every waiter: `sent` requests were lost with the link, unsent ones never left.
    pub fn fail_pending(&self, why: &str) {
        let drained: Vec<Waiter> = lock(&self.pending).drain().map(|(_, w)| w).collect();
        for w in drained {
            if let Waiter::Chan(tx) = w {
                let _ = tx.send(Reply::Lost(why.to_string()));
            }
        }
    }

    /// A request the link thread could not write because the link is down: fail just that waiter.
    pub fn fail_unsent(&self, rid: u64) {
        if let Some(Waiter::Chan(tx)) = lock(&self.pending).remove(&rid) {
            let _ = tx.send(Reply::Down);
        }
    }

    /// A request the link thread wrote and then lost the link on: fail just that waiter.
    pub fn fail_lost(&self, rid: u64, why: &str) {
        if let Some(Waiter::Chan(tx)) = lock(&self.pending).remove(&rid) {
            let _ = tx.send(Reply::Lost(why.to_string()));
        }
    }

    // ---- the reader --------------------------------------------------------------------------------

    /// Handle one frame the reader of connection `conn` read.
    pub fn on_down(&self, conn: u64, frame: TradeDown) -> Flow {
        match frame {
            TradeDown::Event { account, gw_epoch, seq, ts_ms, replayed, event } => self
                .deliver_event(&account, GatewayCursor { gw_epoch, seq, ts_ms }, replayed, event),
            TradeDown::CmdAck { cseq } => {
                // A closed channel means the link thread is gone; the reader ends on its own then.
                let _ = self.tx.send(Msg::Acked { conn, cseq });
                Flow::Continue
            }
            TradeDown::AdapterReply { rid, .. } => {
                // BEFORE the waiter has its answer: the link thread releases the teaching an epoch change
                // held back for the adapter, and everything the waiter sends once it wakes (the
                // re-teaching, `TradeSession::release_held`) must queue behind that. A closed inbox =
                // the link is gone.
                let hold = self.hold_teaching.swap(false, Ordering::AcqRel);
                let _ = self.tx.send(Msg::AdapterAnswered { conn, hold });
                self.complete(rid, frame, true);
                Flow::Continue
            }
            TradeDown::WireIdsReply { rid, .. }
            | TradeDown::OrdersResolved { rid, .. }
            | TradeDown::IdentityRecorded { rid, .. }
            | TradeDown::TradeStatusIs { rid, .. }
            | TradeDown::SymbolFactsReply { rid, .. }
            | TradeDown::ReconFailed { rid, .. }
            | TradeDown::ReconNotReady { rid, .. } => {
                self.complete(rid, frame, true);
                Flow::Continue
            }
            TradeDown::ReconProgress { rid, .. } => {
                self.complete(rid, frame, false);
                Flow::Continue
            }
            TradeDown::ReconReply { rid, more, .. } => {
                self.complete(rid, frame, !more);
                Flow::Continue
            }
            TradeDown::ReconcileNow { reason } => {
                tracing::info!(reason, "gatewayhub trade: reconcile requested by the gatewayhub");
                self.trigger_reconcile();
                Flow::Continue
            }
            TradeDown::ReplayUnavailable { floor } => {
                self.gap_told.store(true, Ordering::Release);
                tracing::warn!(
                    floor,
                    "gatewayhub trade: the ring no longer holds the events this client missed; \
                     reconcile will find them"
                );
                self.trigger_reconcile();
                Flow::Continue
            }
            TradeDown::Heartbeat | TradeDown::Pong => Flow::Continue,
            TradeDown::Bye(bye) => {
                let why = format!("{bye:?}");
                let _ = self.tx.send(Msg::Bye { conn, bye });
                Flow::Stop(format!("the gatewayhub said goodbye ({why})"))
            }
        }
    }

    pub fn trigger_reconcile(&self) {
        if let Some(trigger) = &self.trigger {
            // A closed trigger channel means the reconcile manager is gone.
            let _ = trigger.send(());
        }
    }

    /// Deliver one ring event to the core, in order, exactly once.
    fn deliver_event(
        &self,
        account: &str,
        cursor: GatewayCursor,
        replayed: bool,
        event: vike_model::events::Event,
    ) -> Flow {
        if account != self.account.route_key {
            self.counters.events_misrouted.fetch_add(1, Ordering::Relaxed);
            tracing::error!(
                got = account,
                want = %self.account.route_key,
                "gatewayhub trade: an event for another account arrived on this lease and was NOT \
                 folded in (a daemon bug)"
            );
            return Flow::Continue;
        }
        // Held across the blocking send, on purpose: see `Shared::delivery`.
        let _order = lock(&self.delivery);
        if lock(&self.cursor).admit(cursor) == Admit::Duplicate {
            self.counters.events_duplicate.fetch_add(1, Ordering::Relaxed);
            return Flow::Continue;
        }
        // A hole in `seq` nobody announced is an event the daemon could not write: reconcile finds
        // whatever it said. (`ReplayUnavailable` announces the legitimate jumps and reconciles itself.)
        let announced = self.gap_told.swap(false, Ordering::AcqRel);
        if !announced && lock(&self.cursor).skipped(cursor) {
            tracing::error!(
                epoch = cursor.gw_epoch,
                seq = cursor.seq,
                "gatewayhub trade: the event stream skipped a seq the daemon never announced; an event \
                 was lost in transit and reconcile will find what it said"
            );
            self.trigger_reconcile();
        }
        // **W2G-1: a bridge's SYNTHETIC cancel is its answer to OUR cancel, not the venue's word on the order.**
        // A REST venue reports "too late / already filled / not found" as success (bybit 110001 and 170213,
        // okx 51400-51402) and the shared actor states `OrderCanceled { ts: 0, reason: "" }` for it: for an
        // order whose submit is UNKNOWN that proves nothing, and treated as the venue's terminal word it
        // ended a FILLED order Canceled and flat. It reaches the core as the advisory it is (non-terminal),
        // the coid stays UNKNOWN, and the lookup, the venue's own event or reconcile decides.
        let replacement = synthetic_cancel_coid(&event)
            .filter(|coid| self.is_unknown(coid) && lock(&self.cancel_sent).contains(*coid))
            .map(|coid| {
                tracing::warn!(
                    coid,
                    "gatewayhub trade: the bridge answered the cancel of an UNKNOWN submit with its \
                     synthetic cancel; stated as an advisory, the order stays UNKNOWN"
                );
                Event::OrderCancelRejected(OrderCancelRejected {
                    client_order_id: coid.to_string(),
                    reason: SYNTHETIC_CANCEL_ADVISORY.into(),
                    ts: 0,
                })
            });
        let event = replacement.unwrap_or(event);
        let resolved = resolved_coid(&event).map(str::to_string);
        let ended = ended_coid(&event).map(str::to_string);
        // **W2F-D1: the venue's word on an UNKNOWN submit that only an accepted order can receive.** The
        // order's own `OrderSubmitted`/`OrderAccepted` died with the old daemon's ring, so the core still
        // holds it `Initialized`, and its table (r5-pinned) refuses a fill, cancel or expiry from there:
        // the order would sit until the stuck-order watchdog REJECTED it while the venue holds it ended.
        // The venue has just proved it holds the order, so the two hops the lost prefix would have
        // carried are stated first, with this event's own time, as plain (cursor-less) events: the core drops a
        // `GatewayEvent` at or below its held cursor as a redelivery, so they cannot share the answer's cursor
        // (journaled like any event, so a refold lands the same state). An order the core already holds past `Initialized` refuses both,
        // as it refuses any replayed event. The accept names no venue order (none is known): it is the
        // table's ordinary `Submitted -> Accepted` edge, never the fold's lost-prefix door.
        if let (Some(coid), Some(ts)) = (resolved.as_deref(), implied_prefix_ts(&event))
            && lock(&self.unknown).contains(coid)
        {
            let prefix = [
                Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.to_string(), ts }),
                Event::OrderAccepted(OrderAccepted {
                    client_order_id: coid.to_string(),
                    venue_order_id: None,
                    ts,
                }),
            ];
            for implied in prefix {
                if self.events.blocking_send(implied).is_err() {
                    self.core_gone.store(true, Ordering::Release);
                    return Flow::Stop("the core has exited".to_string());
                }
            }
            // **W2G-2: a FILLED wrap carries the fill it books.** The position, the fee and the balance are the
            // BARE fill's (the dual-publish contract sends both), and a lookup states the wrap alone; the wrap
            // embeds the full `FillEvent`, so the bare fill stated here is exact. The engine's trade-id dedup
            // (`Account::apply_fill`, `seen_fsm_trade_ids`) drops the venue's own copy if it arrives later, and a
            // bare fill that had already been delivered would have cleared UNKNOWN, so none is stated twice.
            if let Some(fill) = wrapped_fill(&event)
                && self.events.blocking_send(Event::Fill(fill.clone())).is_err()
            {
                self.core_gone.store(true, Ordering::Release);
                return Flow::Stop("the core has exited".to_string());
            }
            self.counters.prefixes_synthesized.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                coid,
                "gatewayhub trade: the venue answered an UNKNOWN submit with an event only an accepted \
                 order can receive; the order's lost OrderSubmitted and OrderAccepted were stated first \
                 (and the fill a filled wrap carries)"
            );
        }
        let gw = GatewayEvent { event, route_key: self.account.route_key.as_str().into(), cursor };
        if self.events.blocking_send_gateway(gw).is_err() {
            self.core_gone.store(true, Ordering::Release);
            return Flow::Stop("the core has exited".to_string());
        }
        lock(&self.cursor).delivered(cursor);
        self.counters.events_delivered.fetch_add(1, Ordering::Relaxed);
        if replayed {
            self.counters.events_replayed.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(coid) = ended {
            lock(&self.ended).insert(&coid);
        }
        if let Some(coid) = resolved {
            // The venue spoke about the ORDER (`resolved_coid`): an advisory about a command is not that.
            lock(&self.unknown).remove(&coid);
            lock(&self.cancel_sent).remove(&coid);
        }
        Flow::Continue
    }
}

fn clone_waiter(w: &Waiter) -> Waiter {
    match w {
        Waiter::Chan(tx) => Waiter::Chan(tx.clone()),
        Waiter::Resolve => Waiter::Resolve,
    }
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("account", &self.account)
            .field("state", &self.link_state())
            .finish_non_exhaustive()
    }
}
