//! The state one lease shares between its threads: the fold-side command channel, the link thread, the
//! reader, the reconcile thread and the boot thread.
//!
//! One rule shapes everything here: **the fold thread only ever does a channel send** (`TradeLeaseShared::tx`).
//! Nothing it can reach takes a lock, serializes or writes a socket; the link thread owns the socket
//! and the ledger, the reader owns the inbound stream, and everything they must tell each other is a
//! `TradeLeaseMessage` or one of the small mutexes below (held for a map lookup, never across I/O or a blocking
//! send, except `delivery`, whose whole job is to be held across the blocking send to the core).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use vike_execution::{EventSender, GatewayEvent};
use vike_model::AccountTier;
use vike_model::EventRingCursor;
use vike_model::OrderCancelIntent;
use vike_model::events::{Event, OrderAccepted, OrderCancelRejected, OrderSubmitted};
use vike_utility::mutex::lock;
use vike_utility::recent_set::RecentSet;

use super::cursor::{EventRingCursorAdmit, EventRingCursorTracker};
use super::disclosure::TradeLinkSignal;
use super::ids::WireIds;
use super::mirror::{
    implied_prefix_timestamp, resolved_order_client_id, synthetic_cancel_order_client_id,
    wrapped_fill,
};
use super::told::Told;
use crate::client::{GatewayLink, TradeLeaseOpenError, TradeLeaseStream};
use crate::proto::{
    FETCH_STALL_FLOOR, GATEWAY_TRADE_READ_FLOOR, OrderResolution, REPLY_IDLE_TIMEOUT, ResolveState,
    TradeBye, TradeDown, TradeOpened, TradeUp,
};
use crate::proto::{SymbolPropertiesRefusal, SymbolPropertiesRefusalCode};

/// How many ended order client ids the lease remembers (W2G-6): a bound, not a policy. An order that ends is rarely asked
/// about again, and the memory only ever suppresses a mark that would have been refused anyway.
const ENDED_MEMORY: usize = 16_384;

/// The non-terminal advisory a bridge's SYNTHETIC cancel becomes when it answers a cancel of an UNKNOWN submit
/// (W2G-1): the bridge cannot tell "cancelled" from "too late, already filled, not found" there.
pub(crate) const SYNTHETIC_CANCEL_ADVISORY: &str = "gatewayhub trade: the bridge answered the cancel of an \
     UNKNOWN submit with its own synthetic cancel (no venue time), which also means \"too late / already \
     filled / not found\"; the order is NOT treated as cancelled until the venue's own word, the lookup or \
     reconcile says so";

/// How many of the latest submits the lease keeps in doubt until an event answers them (see
/// [`TradeLeaseShared::with_unanswered`]): an order older than that is reconcile's and the watchdog's, as it always was.
/// Half the daemon's `MAX_FRAME_ITEMS`, because they all go out in one `ResolveOrders` frame.
const UNANSWERED_MEMORY: usize = 4_096;

/// Which account a lease is for. No secret: the keys live in the [`GatewayLink`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeAccount {
    /// The caller's id for the lease, stable across its reconnects (a process). The daemon keys
    /// `received_through_command_sequence` and the explicit takeover on it.
    pub client_id: String,
    /// A `vike_model::VENUES` slug.
    pub venue: String,
    /// The account label's text; `None` is the unlabelled default account.
    pub label: Option<String>,
    /// The tier the caller believes the account runs at.
    pub tier: AccountTier,
    /// The account's route key as the core spells an account's engine (`route_key_of`: the bare
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
    /// The floor of a reconcile fetch's per-frame idle bound: `max(floor, 3 x ReconcileProgress.next_in_ms)`.
    pub reconcile_stall_floor: Duration,
    /// How long `detach` waits for the link thread to flush what is queued and say goodbye.
    pub detach_flush: Duration,
}

impl Default for TradeTimings {
    fn default() -> Self {
        Self {
            read_floor: GATEWAY_TRADE_READ_FLOOR,
            request_reply: REPLY_IDLE_TIMEOUT,
            reconcile_stall_floor: FETCH_STALL_FLOOR,
            detach_flush: Duration::from_secs(5),
        }
    }
}

impl TradeTimings {
    /// The defaults, raised to `read_floor` and `reconcile_stall_floor` where those are ABOVE the protocol
    /// floors: the rule a production root applies to its settings rows. A value below a floor is the
    /// floor.
    #[must_use]
    pub fn raised(read_floor: Duration, reconcile_stall_floor: Duration) -> Self {
        let floors = Self::default();
        Self {
            read_floor: floors.read_floor.max(read_floor),
            reconcile_stall_floor: floors.reconcile_stall_floor.max(reconcile_stall_floor),
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
/// again ([`Self::is_transient`]). Each carries the operator's sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolPropertiesRequestError {
    /// The DAEMON's refusal, with its code ([`SymbolPropertiesRefusalCode`]): the one a node decides by, never
    /// the sentence. A transient code is "not now" (busy, the venue's list or rate not had); any other is an
    /// answer about the symbol.
    Daemon(SymbolPropertiesRefusal),
    /// A definitive answer this node gives itself about the symbol or the gatewayhub: a gatewayhub without
    /// the verb, a quoted answer it cannot book.
    Refused(String),
    /// Not an answer about the symbol: the link was down or lost, the answer timed out. Never remembered.
    Unavailable(String),
}

impl SymbolPropertiesRequestError {
    /// The sentence.
    pub fn text(&self) -> &str {
        match self {
            SymbolPropertiesRequestError::Daemon(refusal) => &refusal.reason,
            SymbolPropertiesRequestError::Refused(t)
            | SymbolPropertiesRequestError::Unavailable(t) => t,
        }
    }

    /// The refusal a person reads: the daemon's sentence under the bridge id it carried
    /// (`SymbolPropertiesRefusal::refusal_id`), this node's own words as free text.
    #[must_use]
    pub fn carried(&self) -> vike_utility::errors::CarriedRefusal {
        match self {
            SymbolPropertiesRequestError::Daemon(refusal) => refusal.carried(),
            SymbolPropertiesRequestError::Refused(t)
            | SymbolPropertiesRequestError::Unavailable(t) => t.as_str().into(),
        }
    }

    /// The daemon's code, when the daemon refused.
    #[must_use]
    pub fn code(&self) -> Option<SymbolPropertiesRefusalCode> {
        match self {
            SymbolPropertiesRequestError::Daemon(refusal) => Some(refusal.code),
            SymbolPropertiesRequestError::Refused(_)
            | SymbolPropertiesRequestError::Unavailable(_) => None,
        }
    }

    /// Whether this says nothing about the symbol ("not now"): never remembered, asked again.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            SymbolPropertiesRequestError::Daemon(refusal) => refusal.code.is_transient(),
            SymbolPropertiesRequestError::Refused(_) => false,
            SymbolPropertiesRequestError::Unavailable(_) => true,
        }
    }
}

impl std::fmt::Display for SymbolPropertiesRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

impl std::error::Error for SymbolPropertiesRequestError {}

/// What the link thread is told. Command frames arrive with a placeholder `cseq` (`0`): the link thread
/// numbers a frame when it WRITES it, so a number is never burned by a frame that never left.
#[derive(Debug)]
pub(crate) enum TradeLeaseMessage {
    /// A command frame (an order, a cancel, a modify, a confirm, the teaching).
    Frame(Box<TradeUp>),
    /// A request frame; its waiter was registered under its `rid` before this was sent.
    Ask(Box<TradeUp>),
    /// The daemon acknowledged command `cseq` on connection `conn`.
    Acked { conn: u64, command_sequence: u64 },
    /// The daemon answered an `AdapterStart` on connection `conn` (the adapter is up, or it said why not).
    ///
    /// `hold` is the caller's word (read from `TradeLeaseShared::hold_teaching`) that it will teach the new adapter
    /// and call `TradeLease::release_held` itself: the held cancels then wait for that.
    AdapterAnswered { conn: u64, hold: bool },
    /// The caller finished teaching the restarted adapter: release the held cancels
    /// (`TradeLease::release_held`). FIFO behind every `Frame` the caller sent before it, which is
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
    Dialed(Result<TradeLeaseStream, TradeLeaseOpenError>),
    /// Phase one or two of a teardown.
    Detach(crate::proto::DetachPhase),
    /// Flush, say goodbye, exit.
    Stop,
}

/// What a waiting request receives.
#[derive(Debug)]
pub(crate) enum TradeLeaseReply {
    /// A frame answering the request (a progress or a page frame leaves the waiter registered).
    Frame(Box<TradeDown>),
    /// The request was never sent: the link was down.
    Down,
    /// The request was sent and the link was lost before the answer.
    Lost(String),
}

enum Waiter {
    /// A thread is blocked on this channel.
    Chan(Sender<TradeLeaseReply>),
    /// Nobody waits; the answer is recorded (`ResolveOrders` after an epoch change).
    Resolve,
}

/// The link's state, for observers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeLinkState {
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
    /// Reconnects that landed on a different `gateway_epoch` (the gatewayhub restarted, or retired the session).
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
pub(crate) enum TradeLeaseFlow {
    Continue,
    Stop(String),
}

/// The shared state of one lease. See the module doc.
pub(crate) struct TradeLeaseShared {
    pub link: GatewayLink,
    pub account: TradeAccount,
    pub timings: TradeTimings,
    pub halt_path: Option<PathBuf>,
    /// The core's lane for this account (already `routed`): synthesized events and the relay.
    pub events: EventSender,
    /// The reconcile trigger: each poke names this lease's account (its route key), so the manager reconciles that leg.
    pub trigger: Option<Sender<String>>,
    pub signal: Arc<dyn TradeLinkSignal>,
    /// The fold-side door to the link thread.
    pub tx: Sender<TradeLeaseMessage>,
    state: AtomicU8,
    rid: AtomicU64,
    pending: Mutex<HashMap<u64, Waiter>>,
    pub wire_ids: RwLock<WireIds>,
    /// The symbols the daemon's reconcile client was told the engine admitted (`told.rs`).
    pub told: Told,
    /// Serializes delivery to the core across the readers of successive connections: held across the
    /// (possibly blocking) send so a late event of an old connection cannot overtake a newer one.
    delivery: Mutex<()>,
    cursor: Mutex<EventRingCursorTracker>,
    unknown: Mutex<BTreeSet<String>>,
    /// UNKNOWN order client ids for which a cancel of OURS was Written (W2G-1): a synthetic cancel for one of these is
    /// the bridge's answer to that cancel. Leaves with the order client id's UNKNOWN mark.
    cancel_sent: Mutex<BTreeSet<String>>,
    /// Order client ids whose terminal event this lease delivered (W2G-6): they are never marked UNKNOWN afterwards.
    ended: Mutex<RecentSet>,
    /// The submits this lease wrote that no event has answered yet (see [`TradeLeaseShared::with_unanswered`]).
    unanswered: Mutex<RecentSet>,
    /// The cancels that may not be shed this lease wrote and no event has answered yet, with their intent
    /// (see [`TradeLeaseShared::take_cancels_in_doubt`]).
    cancels_in_doubt: Mutex<BTreeMap<String, OrderCancelIntent>>,
    resolutions: Mutex<HashMap<String, ResolveState>>,
    opened: Mutex<Option<TradeOpened>>,
    features: Mutex<Vec<String>>,
    closed_because: Mutex<Option<String>>,
    core_gone: AtomicBool,
    /// A teardown has begun (`begin_detach` / `detach` / the last handle dropped): nothing new should
    /// be started on this lease (the tradehub's re-teaching worker reads it).
    stopping: AtomicBool,
    /// The next `AdapterStart` answer is one the caller will follow with its own teaching and a
    /// `TradeLease::release_held` (`TradeLease::restart_adapter`): the ledger keeps the held cancels for it.
    hold_teaching: AtomicBool,
    /// The node CONVERTS a quote currency for this account (`TradeLease::convert_quotes`): every facts
    /// request says so (`TradeUp::SymbolProperties::quoted`), so the daemon may answer a symbol quoted in
    /// another currency than the account's, with its rate.
    quoted: AtomicBool,
    /// A `ReplayUnavailable` arrived and no event has been delivered since: the next jump in `seq` is the one
    /// it announced (see [`EventRingCursorTracker::skipped`]).
    gap_told: AtomicBool,
    counters: Counters,
}

impl TradeLeaseShared {
    pub fn new(
        link: GatewayLink,
        account: TradeAccount,
        timings: TradeTimings,
        halt_path: Option<PathBuf>,
        events: EventSender,
        trigger: Option<Sender<String>>,
        signal: Arc<dyn TradeLinkSignal>,
        resume: Option<EventRingCursor>,
    ) -> (Arc<Self>, mpsc::Receiver<TradeLeaseMessage>) {
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
            cursor: Mutex::new(EventRingCursorTracker::new(resume)),
            unknown: Mutex::new(BTreeSet::new()),
            cancel_sent: Mutex::new(BTreeSet::new()),
            ended: Mutex::new(RecentSet::default()),
            unanswered: Mutex::new(RecentSet::default()),
            cancels_in_doubt: Mutex::new(BTreeMap::new()),
            resolutions: Mutex::new(HashMap::new()),
            opened: Mutex::new(None),
            features: Mutex::new(Vec::new()),
            closed_because: Mutex::new(None),
            core_gone: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            hold_teaching: AtomicBool::new(false),
            quoted: AtomicBool::new(false),
            gap_told: AtomicBool::new(false),
            counters: Counters::default(),
        });
        (shared, rx)
    }

    /// Say whether the node converts a quote currency for this account (see the field).
    pub fn set_quoted(&self, converts: bool) {
        self.quoted.store(converts, Ordering::Release);
    }

    /// Whether facts requests carry the node's "I convert a quote currency".
    pub fn quoted(&self) -> bool {
        self.quoted.load(Ordering::Acquire)
    }

    // ---- state ------------------------------------------------------------------------------------

    pub fn link_state(&self) -> TradeLinkState {
        match self.state.load(Ordering::Acquire) {
            STATE_UP => TradeLinkState::Up,
            STATE_DOWN => TradeLinkState::Down,
            _ => TradeLinkState::Closed,
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

    /// Close the lease for good and remember why: the reason first, so a lease that reads closed has one.
    pub fn set_closed(&self, why: &str) {
        {
            let mut slot = lock(&self.closed_because);
            if slot.is_none() {
                *slot = Some(why.to_string());
            }
        }
        self.state.store(STATE_CLOSED, Ordering::Release);
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

    /// The next `AdapterStart` answer is held for the caller's teaching (see `TradeLeaseShared::hold_teaching`).
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
        let missed =
            lock(&self.cursor).opened(opened.gateway_epoch, opened.event_ring_floor == Some(0));
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

    pub fn cursor(&self) -> Option<EventRingCursor> {
        lock(&self.cursor).last()
    }

    /// What a redial presents as `TradeOpen.resume`: the delivered cursor, or the anchor of the epoch
    /// (`EventRingCursorTracker::resume`).
    pub fn resume_cursor(&self) -> Option<EventRingCursor> {
        lock(&self.cursor).resume()
    }

    // ---- UNKNOWN orders ----------------------------------------------------------------------------

    /// Mark `order_client_ids` UNKNOWN, except the ones whose terminal event this lease already delivered (W2G-6: an
    /// ended order has nothing left to resolve, and a lookup's re-statement of its ending would earn a prefix
    /// the core refuses). Returns the order client ids actually marked.
    pub fn mark_unknown(&self, order_client_ids: impl IntoIterator<Item = String>) -> Vec<String> {
        let open: Vec<String> = {
            let ended = lock(&self.ended);
            order_client_ids.into_iter().filter(|c| !ended.contains(c)).collect()
        };
        lock(&self.unknown).extend(open.iter().cloned());
        open
    }

    /// The UNKNOWN set's lock, held by a test to park whoever marks an order client id (`attach` parks in `mark_unknown`).
    #[cfg(test)]
    pub(crate) fn hold_unknown_for_test(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        lock(&self.unknown)
    }

    fn is_unknown(&self, order_client_id: &str) -> bool {
        lock(&self.unknown).contains(order_client_id)
    }

    /// A cancel frame is about to be written: remember which UNKNOWN order client ids it names (W2G-1), so a synthetic
    /// cancel that comes back for one of them is read as the answer to THIS cancel. Returns the order client ids newly
    /// remembered, for [`TradeLeaseShared::forget_cancel_written`] if the write fails.
    pub fn note_cancel_written(&self, frame: &TradeUp) -> Vec<String> {
        let named: Vec<&String> = if let TradeUp::Cancel { order_client_id, .. } = frame {
            vec![order_client_id]
        } else if let TradeUp::CancelBatch { order_client_ids, .. } = frame {
            order_client_ids.iter().collect()
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

    /// The write that [`TradeLeaseShared::note_cancel_written`] preceded failed: the cancel was never sent.
    pub fn forget_cancel_written(&self, order_client_ids: &[String]) {
        let mut sent = lock(&self.cancel_sent);
        for c in order_client_ids {
            sent.remove(c);
        }
    }

    /// A submit frame is about to be written (BEFORE the write: the answer can be read the instant the daemon has
    /// the frame): it is in doubt until an event answers it, see [`TradeLeaseShared::with_unanswered`]. A frame whose write
    /// fails gets a reject, which [`TradeLeaseShared::forget_unanswered`] takes back out.
    pub fn note_submit_written(&self, frame: &TradeUp) {
        let mut unanswered = lock(&self.unanswered);
        match frame {
            TradeUp::Submit { order, .. } => {
                unanswered.insert(&order.order_client_id, UNANSWERED_MEMORY);
            }
            TradeUp::SubmitBatch { orders, .. } => {
                for order in orders {
                    unanswered.insert(&order.order_client_id, UNANSWERED_MEMORY);
                }
            }
            _ => {}
        }
    }

    /// `order_client_id` was answered: by the venue, or by a terminal this client synthesized.
    pub fn forget_unanswered(&self, order_client_id: &str) {
        lock(&self.unanswered).remove(order_client_id);
    }

    /// **Nothing more can reach this lease from the daemon that owned these orders** (it closed for good, or a new
    /// epoch means the ring is gone): `written`, the submits it never acknowledged, followed by every submit it
    /// did acknowledge and nothing answered, in the order they were written. An acknowledgement says the daemon
    /// OWNS an order and owes it an answer through its ring; the reject of an order it had no adapter for sits
    /// there, a takeover's `Bye` overtakes it (`Lease::next`) and a daemon's death destroys it. Such an order is in
    /// doubt exactly as a Written one is, so it is named and never synthesized.
    pub fn with_unanswered(&self, written: Vec<String>) -> Vec<String> {
        let unanswered = lock(&self.unanswered).take();
        let mut seen: BTreeSet<String> = written.iter().cloned().collect();
        let mut order_client_ids = written;
        order_client_ids.extend(unanswered.into_iter().filter(|c| seen.insert(c.clone())));
        order_client_ids
    }

    /// A cancel frame that may not be shed is about to be written (BEFORE the write, as
    /// [`TradeLeaseShared::note_submit_written`]): it is IN DOUBT until an event answers it, see
    /// [`TradeLeaseShared::take_cancels_in_doubt`]. Returns the order client ids newly noted, for
    /// [`TradeLeaseShared::forget_cancels_in_doubt`] if the write fails (the ledger then holds the frame itself).
    /// Past `UNANSWERED_MEMORY` a cancel is not noted, and the core's watchdog rung for an unanswered cancel
    /// is its backstop.
    pub fn note_cancel_in_doubt(&self, frame: &TradeUp) -> Vec<String> {
        let (named, intent): (Vec<&String>, OrderCancelIntent) = match frame {
            TradeUp::Cancel { order_client_id, intent, .. } => (vec![order_client_id], *intent),
            TradeUp::CancelBatch { order_client_ids, intent, .. } => {
                (order_client_ids.iter().collect(), *intent)
            }
            _ => return Vec::new(),
        };
        if intent.may_be_shed() {
            return Vec::new();
        }
        let mut in_doubt = lock(&self.cancels_in_doubt);
        let mut noted = Vec::new();
        for c in named {
            if in_doubt.len() < UNANSWERED_MEMORY && !in_doubt.contains_key(c) {
                in_doubt.insert(c.clone(), intent);
                noted.push(c.clone());
            }
        }
        noted
    }

    /// The write that [`TradeLeaseShared::note_cancel_in_doubt`] preceded failed: the cancel was never sent.
    pub fn forget_cancels_in_doubt(&self, order_client_ids: &[String]) {
        let mut in_doubt = lock(&self.cancels_in_doubt);
        for c in order_client_ids {
            in_doubt.remove(c);
        }
    }

    /// **The cancels that may not be shed this lease wrote and no event answered**, with their intent, leaving
    /// none. An acknowledgement says the daemon owes the cancel an answer through its ring; a daemon that dies
    /// before it calls the venue takes the cancel with it and the order keeps resting. So after an epoch change
    /// the link holds them again for the new daemon (a cancel is idempotent: one of an order that is gone is
    /// answered with an advisory), and a lease closed for good names them.
    pub fn take_cancels_in_doubt(&self) -> Vec<(String, OrderCancelIntent)> {
        std::mem::take(&mut *lock(&self.cancels_in_doubt)).into_iter().collect()
    }

    pub fn unknown_orders(&self) -> Vec<String> {
        lock(&self.unknown).iter().cloned().collect()
    }

    pub fn resolution_of(&self, order_client_id: &str) -> Option<ResolveState> {
        lock(&self.resolutions).get(order_client_id).copied()
    }

    // ---- requests -----------------------------------------------------------------------------------

    pub fn next_rid(&self) -> u64 {
        self.rid.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Register a waiter for `rid` and hand `frame` to the link thread. The returned receiver gets the
    /// answer frames; drop it (and [`TradeLeaseShared::forget`]) when giving up.
    pub fn ask(
        &self,
        rid: u64,
        frame: TradeUp,
    ) -> Result<mpsc::Receiver<TradeLeaseReply>, TradeError> {
        let (tx, rx) = mpsc::channel();
        lock(&self.pending).insert(rid, Waiter::Chan(tx));
        if self.tx.send(TradeLeaseMessage::Ask(Box::new(frame))).is_err() {
            lock(&self.pending).remove(&rid);
            return Err(TradeError::LinkDown(self.unavailable_text()));
        }
        Ok(rx)
    }

    /// As [`TradeLeaseShared::ask`] with nobody waiting: the answer is recorded in [`TradeLeaseShared::resolution_of`].
    pub fn ask_resolve(&self, rid: u64, frame: TradeUp) {
        lock(&self.pending).insert(rid, Waiter::Resolve);
        if self.tx.send(TradeLeaseMessage::Ask(Box::new(frame))).is_err() {
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
                let _ = tx.send(TradeLeaseReply::Frame(Box::new(down)));
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
            seen.insert(r.order_client_id, r.state);
        }
    }

    /// Fail every waiter: `sent` requests were lost with the link, unsent ones never left.
    pub fn fail_pending(&self, why: &str) {
        let drained: Vec<Waiter> = lock(&self.pending).drain().map(|(_, w)| w).collect();
        for w in drained {
            if let Waiter::Chan(tx) = w {
                let _ = tx.send(TradeLeaseReply::Lost(why.to_string()));
            }
        }
    }

    /// A request the link thread could not write because the link is down: fail just that waiter.
    pub fn fail_unsent(&self, rid: u64) {
        if let Some(Waiter::Chan(tx)) = lock(&self.pending).remove(&rid) {
            let _ = tx.send(TradeLeaseReply::Down);
        }
    }

    /// A request the link thread wrote and then lost the link on: fail just that waiter.
    pub fn fail_lost(&self, rid: u64, why: &str) {
        if let Some(Waiter::Chan(tx)) = lock(&self.pending).remove(&rid) {
            let _ = tx.send(TradeLeaseReply::Lost(why.to_string()));
        }
    }

    // ---- the reader --------------------------------------------------------------------------------

    /// Handle one frame the reader of connection `conn` read.
    pub fn on_down(&self, conn: u64, frame: TradeDown) -> TradeLeaseFlow {
        match frame {
            TradeDown::Event {
                account,
                gateway_epoch,
                sequence: seq,
                timestamp_ms: ts_ms,
                replayed,
                event,
            } => self.deliver_event(
                &account,
                EventRingCursor { gateway_epoch, sequence: seq, timestamp_ms: ts_ms },
                replayed,
                event,
            ),
            TradeDown::CommandAck { command_sequence: cseq } => {
                // A closed channel means the link thread is gone; the reader ends on its own then.
                let _ = self.tx.send(TradeLeaseMessage::Acked { conn, command_sequence: cseq });
                TradeLeaseFlow::Continue
            }
            TradeDown::AdapterReply { rid, .. } => {
                // BEFORE the waiter has its answer: the link thread releases the teaching an epoch change
                // held back for the adapter, and everything the waiter sends once it wakes (the
                // re-teaching, `TradeLease::release_held`) must queue behind that. A closed inbox =
                // the link is gone.
                let hold = self.hold_teaching.swap(false, Ordering::AcqRel);
                let _ = self.tx.send(TradeLeaseMessage::AdapterAnswered { conn, hold });
                self.complete(rid, frame, true);
                TradeLeaseFlow::Continue
            }
            TradeDown::WireIdsReply { rid, .. }
            | TradeDown::OrdersResolved { rid, .. }
            | TradeDown::IdentityRecorded { rid, .. }
            | TradeDown::TradeStatusIs { rid, .. }
            | TradeDown::SymbolPropertiesReply { rid, .. }
            | TradeDown::VenueReadReply { rid, .. }
            | TradeDown::ReconcileFailed { rid, .. }
            | TradeDown::ReconcileNotReady { rid, .. } => {
                self.complete(rid, frame, true);
                TradeLeaseFlow::Continue
            }
            TradeDown::ReconcileProgress { rid, .. } => {
                self.complete(rid, frame, false);
                TradeLeaseFlow::Continue
            }
            TradeDown::ReconcileReply { rid, more, .. } => {
                self.complete(rid, frame, !more);
                TradeLeaseFlow::Continue
            }
            TradeDown::ReconcileNow { reason } => {
                tracing::info!(reason, "gatewayhub trade: reconcile requested by the gatewayhub");
                self.trigger_reconcile();
                TradeLeaseFlow::Continue
            }
            TradeDown::ReplayUnavailable { floor } => {
                self.gap_told.store(true, Ordering::Release);
                tracing::warn!(
                    floor,
                    "gatewayhub trade: the ring no longer holds the events this client missed; \
                     reconcile will find them"
                );
                self.trigger_reconcile();
                TradeLeaseFlow::Continue
            }
            TradeDown::Heartbeat | TradeDown::Pong => TradeLeaseFlow::Continue,
            TradeDown::Bye(bye) => {
                let why = format!("{bye:?}");
                let _ = self.tx.send(TradeLeaseMessage::Bye { conn, bye });
                TradeLeaseFlow::Stop(format!("the gatewayhub said goodbye ({why})"))
            }
        }
    }

    /// Ask for a reconcile pass of THIS account's leg.
    pub fn trigger_reconcile(&self) {
        if let Some(trigger) = &self.trigger {
            // A closed trigger channel means the reconcile manager is gone.
            let _ = trigger.send(self.account.route_key.clone());
        }
    }

    /// Deliver one ring event to the core, in order, exactly once.
    fn deliver_event(
        &self,
        account: &str,
        cursor: EventRingCursor,
        replayed: bool,
        event: vike_model::events::Event,
    ) -> TradeLeaseFlow {
        if account != self.account.route_key {
            self.counters.events_misrouted.fetch_add(1, Ordering::Relaxed);
            tracing::error!(
                got = account,
                want = %self.account.route_key,
                "gatewayhub trade: an event for another account arrived on this lease and was NOT \
                 folded in (a daemon bug)"
            );
            return TradeLeaseFlow::Continue;
        }
        // Held across the blocking send, on purpose: see `TradeLeaseShared::delivery`.
        let _order = lock(&self.delivery);
        if lock(&self.cursor).admit(cursor) == EventRingCursorAdmit::Duplicate {
            self.counters.events_duplicate.fetch_add(1, Ordering::Relaxed);
            return TradeLeaseFlow::Continue;
        }
        // A hole in `seq` nobody announced is an event the daemon could not write: reconcile finds
        // whatever it said. (`ReplayUnavailable` announces the legitimate jumps and reconciles itself.)
        let announced = self.gap_told.swap(false, Ordering::AcqRel);
        if !announced && lock(&self.cursor).skipped(cursor) {
            tracing::error!(
                epoch = cursor.gateway_epoch,
                seq = cursor.sequence,
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
        // the order client id stays UNKNOWN, and the lookup, the venue's own event or reconcile decides.
        let replacement = synthetic_cancel_order_client_id(&event)
            .filter(|order_client_id| self.is_unknown(order_client_id) && lock(&self.cancel_sent).contains(*order_client_id))
            .map(|order_client_id| {
                tracing::warn!(
                    order_client_id,
                    "gatewayhub trade: the bridge answered the cancel of an UNKNOWN submit with its \
                     synthetic cancel; stated as an advisory, the order stays UNKNOWN"
                );
                Event::OrderCancelRejected(OrderCancelRejected {
                    order_client_id: order_client_id.to_string(),
                    reason: SYNTHETIC_CANCEL_ADVISORY.into(),
                    timestamp: 0,
                    refusal_id: None,
                })
            });
        let event = replacement.unwrap_or(event);
        let resolved = resolved_order_client_id(&event).map(str::to_string);
        let ended = event.closed_order().map(str::to_string);
        // A cancel in doubt is answered by its order's end or by its refusal (`take_cancels_in_doubt`).
        let cancel_answered = match &event {
            Event::OrderCancelRejected(r) => Some(r.order_client_id.clone()),
            _ => ended.clone(),
        };
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
        if let (Some(order_client_id), Some(ts)) =
            (resolved.as_deref(), implied_prefix_timestamp(&event))
            && lock(&self.unknown).contains(order_client_id)
        {
            let prefix = [
                Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: order_client_id.to_string(),
                    timestamp: ts,
                }),
                Event::OrderAccepted(OrderAccepted {
                    order_client_id: order_client_id.to_string(),
                    venue_order_id: None,
                    timestamp: ts,
                }),
            ];
            for implied in prefix {
                if self.events.blocking_send(implied).is_err() {
                    self.core_gone.store(true, Ordering::Release);
                    return TradeLeaseFlow::Stop("the core has exited".to_string());
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
                return TradeLeaseFlow::Stop("the core has exited".to_string());
            }
            self.counters.prefixes_synthesized.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                order_client_id,
                "gatewayhub trade: the venue answered an UNKNOWN submit with an event only an accepted \
                 order can receive; the order's lost OrderSubmitted and OrderAccepted were stated first \
                 (and the fill a filled wrap carries)"
            );
        }
        let gw = GatewayEvent { event, route_key: self.account.route_key.as_str().into(), cursor };
        if self.events.blocking_send_gateway(gw).is_err() {
            self.core_gone.store(true, Ordering::Release);
            return TradeLeaseFlow::Stop("the core has exited".to_string());
        }
        lock(&self.cursor).delivered(cursor);
        self.counters.events_delivered.fetch_add(1, Ordering::Relaxed);
        if replayed {
            self.counters.events_replayed.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(order_client_id) = ended {
            lock(&self.ended).insert(&order_client_id, ENDED_MEMORY);
        }
        if let Some(order_client_id) = cancel_answered {
            lock(&self.cancels_in_doubt).remove(&order_client_id);
        }
        if let Some(order_client_id) = resolved {
            // The venue spoke about the ORDER (`resolved_order_client_id`): an advisory about a command is not that.
            lock(&self.unknown).remove(&order_client_id);
            lock(&self.cancel_sent).remove(&order_client_id);
            self.forget_unanswered(&order_client_id);
        }
        TradeLeaseFlow::Continue
    }
}

fn clone_waiter(w: &Waiter) -> Waiter {
    match w {
        Waiter::Chan(tx) => Waiter::Chan(tx.clone()),
        Waiter::Resolve => Waiter::Resolve,
    }
}

impl std::fmt::Debug for TradeLeaseShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TradeLeaseShared")
            .field("account", &self.account)
            .field("state", &self.link_state())
            .finish_non_exhaustive()
    }
}
