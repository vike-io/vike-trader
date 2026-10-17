//! The `trade` socket class: the frames of a LEASE after [`Request::TradeOpen`](super::Request::TradeOpen)
//! is answered by [`Response::TradeOpened`](super::Response::TradeOpened).
//!
//! # The lease
//!
//! One long-lived duplex connection per `(client, account)` under `Scope::Trade`. `TradeOpened` is the
//! LAST positional frame its socket carries (the `stream` class's `DataOpened` rule): after it every
//! client frame is a [`TradeUp`] and every server frame a [`TradeDown`], so a reader's `match` is a
//! complete protocol and anything else is a desync.
//!
//! # Two kinds of client frame, and why only one is an ORDER
//!
//! * **Commands** carry a `cseq`: `Submit`, `SubmitBatch`, `Cancel`, `CancelBatch`, `Modify`, `Confirm`,
//!   `AdoptRestored`. `cseq` is per LEASE (a client's commands to one account: each lease numbers its own
//!   from 1), rises by exactly one per frame and never repeats inside a process; one TCP connection preserves
//!   order. The server answers each with [`TradeDown::CmdAck`], a
//!   TRANSPORT acknowledgement (the frame is queued at the gatewayhub), never a venue ack.
//!   `TradeOpened.received_through_cseq` is the watermark of everything the server has read, definitive
//!   inside one `gw_epoch` because a takeover or reconnect is FENCED (the server reads the old
//!   connection to EOF before it answers the new open).
//! * **Requests** carry a `rid` and are answered by a frame that echoes it: `AdapterStart`, `WireIds`, `SymbolFacts`,
//!   `ResolveOrders`, `IdentityRecord`, `Recon`, `TradeStatus`. They place nothing and have no order to
//!   lose; a lost one is an error to the caller waiting on it.
//!
//! # Events
//!
//! [`TradeDown::Event`] carries one venue event with its position in the gatewayhub's per-account ring
//! (`gw_epoch`, `seq`, `ts_ms`): the cursor the tradehub journals with the event
//! (`vike_exec::GatewayCursor`). `gw_epoch` is RANDOM per SESSION (a session is one account's ring, watermarks, dedup set and adapter; it is not ordered),
//! `seq` rises by one per event inside an epoch. `replayed` marks an event delivered from the ring
//! after a resume; it takes the same path as a live one.
//!
//! ⚠ **Adding a variant is the same three edits as the rest of the wire** (the variant, every exhaustive
//! table that names it, a golden line in `fixtures/gatewayhub_wire/`): `tests/wire_tag_fixtures.rs`
//! reads this file and refuses a variant no fixture carries.

use serde::{Deserialize, Serialize};
use vike_model::OrderRequest;
use vike_model::events::Event;

use super::backend_dto::{
    AdapterReply, AdapterStart, ReconKind, ReconPayload, TradeStatusReport, WireSymbolFacts,
};

/// The daemon's `SymbolFactsReply` sentences that say "not now", never "no": its lookup slots are full,
/// its lookup thread could not start, or the venue's instrument list could not be had. A node remembers
/// a refusal only if it is none of these ([`facts_refusal_is_transient`]); the gatewayhub builds each
/// from these constants, so the two sides cannot drift.
pub const FACTS_BUSY: &str =
    "too many symbol-facts lookups are running at the gatewayhub; ask again";
/// See [`FACTS_BUSY`].
pub const FACTS_NOT_STARTED: &str = "the gatewayhub could not start a symbol-facts lookup";
/// See [`FACTS_BUSY`]. The daemon keeps this failure for 30 s itself.
pub const FACTS_LIST_UNAVAILABLE: &str = "the venue's instrument list could not be fetched";
/// See [`FACTS_BUSY`].
pub const FACTS_LIST_EMPTY: &str = "the venue's instrument list came back empty";

/// Whether a daemon's refusal of a facts request is "not now" ([`FACTS_BUSY`] and its siblings) rather
/// than a definitive answer about the symbol (unknown, no tick size, not linear, not answered).
#[must_use]
pub fn facts_refusal_is_transient(text: &str) -> bool {
    [FACTS_BUSY, FACTS_NOT_STARTED, FACTS_LIST_UNAVAILABLE, FACTS_LIST_EMPTY]
        .iter()
        .any(|mark| text.contains(mark))
}

/// A position in the gatewayhub's per-account event ring, as it crosses the wire. The tradehub's journal
/// stores `vike_exec::GatewayCursor` (the same three numbers); `crate::trade::cursor` converts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCursor {
    /// The gatewayhub SESSION the ring belongs to (random per session, not per process).
    pub gw_epoch: u64,
    /// The per-account, per-epoch sequence number.
    pub seq: u64,
    /// The event's own timestamp, epoch ms.
    pub ts_ms: i64,
}

/// The answer to an accepted [`Request::TradeOpen`](super::Request::TradeOpen).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeOpened {
    /// The gatewayhub session the lease lives in (random per session: a session built again after a final detach or an orphan timeout is a new epoch). A client that sees it change
    /// across a reconnect knows every in-memory state it relied on (the cseq watermark, the ring, the
    /// dedup set, the adapter) is gone.
    pub gw_epoch: u64,
    /// The lease's own generation: a late command from an old connection carries an older one.
    pub lease_epoch: u64,
    /// The highest `cseq` of this LEASE (this client's commands to this account) the gatewayhub has READ,
    /// definitive inside one `gw_epoch`. A daemon keys the watermark by `(client_id, venue, account)`.
    pub received_through_cseq: u64,
    /// A random token the gatewayhub keeps in `<state_dir>/gateway-id`: proof the two processes share a
    /// project folder (the HALT sentinel's home). Compared by the CALLER, which reads its own file.
    #[serde(default)]
    pub project_token: String,
    /// **The server's heartbeat period in milliseconds.** The client arms
    /// `max(GW_TRADE_READ_FLOOR, 3 x heartbeat_ms)` after clamping this into
    /// `GW_TRADE_HEARTBEAT_MIN..=GW_TRADE_HEARTBEAT_MAX`, and pings at the same period.
    pub heartbeat_ms: u64,
    /// The oldest `seq` the ring still holds for this account (`Some(0)` = nothing yet). **`None` is a daemon
    /// that does not say** (an older one omits the field, and it is not sent for `None`): the client cannot
    /// tell an empty ring from a full one, so it never anchors on it and reconciles on a flap before its first
    /// event (W19g), instead of reading the missing field as "empty".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_ring_floor: Option<u64>,
}

/// Why a command stream is being ended or a cancel was issued: the mirror of
/// `vike_exec::CancelIntent` (no serde there), with two EXHAUSTIVE conversions in `crate::trade::mirror`.
///
/// ⚠ `Unspecified` stays the flatten-safe default: an old peer that omits the field never sheds a cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WireCancelIntent {
    /// Could not be classified; never shed.
    #[default]
    Unspecified,
    /// Ladder churn; the only intent a venue may hold back.
    Routine,
    /// A declared risk-off action; never shed.
    RiskOff,
}

/// The phase of a teardown ([`TradeUp::Detach`]), the wire twin of `begin_detach` / `detach`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DetachPhase {
    /// Phase one: raise the stop flags, join nothing. Idempotent with `Final`.
    Begin,
    /// Phase two: the client is done; the daemon may release the lease.
    Final,
}

/// What the gatewayhub knows of a client order after an epoch change, for [`TradeUp::ResolveOrders`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolveState {
    /// The daemon's in-memory tracker has the order: its events flow (or already flowed).
    Tracked,
    /// The daemon launched the venue re-query by client order id (`confirm`); the authoritative event
    /// arrives as an ordinary [`TradeDown::Event`].
    Querying,
    /// The venue has no lookup by client order id: the order stays UNKNOWN until reconcile or the
    /// stuck-order watchdog's last resort.
    NoResolver,
}

/// One order's answer in [`TradeDown::OrdersResolved`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderResolution {
    /// The client order id asked about.
    pub coid: String,
    /// What the daemon did.
    pub state: ResolveState,
}

/// Why the gatewayhub is ending a lease; the last frame on the socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeBye {
    /// Another client took the lease over (explicit and fenced). The old holder must NOT fight back.
    Takeover {
        /// The taker's key id (never a key).
        by_key_id: String,
    },
    /// The daemon is shutting down.
    ServerStopping,
    /// The client could not keep up with its event stream.
    TooSlow,
    /// The answer to [`TradeUp::Detach`] with [`DetachPhase::Final`].
    ClientClosed,
    /// A protocol violation by the client (a frame it may not send), with the daemon's words.
    Protocol(String),
}

/// A client frame on an open lease.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TradeUp {
    /// Start the venue adapter of this lease. Answered by [`TradeDown::AdapterReply`].
    AdapterStart(AdapterStart),
    /// One order. Answered by [`TradeDown::CmdAck`]; a duplicate `(account, coid)` is acked, not re-sent.
    Submit {
        /// The command sequence number.
        cseq: u64,
        /// The order, as the core built it.
        order: OrderRequest,
    },
    /// Several orders in one frame (at most `GW_TRADE_UP_FRAME_CEILING_BYTES`); each still produces its
    /// own event stream.
    SubmitBatch {
        /// The command sequence number.
        cseq: u64,
        /// The orders, in the caller's order.
        orders: Vec<OrderRequest>,
    },
    /// Cancel one order.
    Cancel {
        /// The command sequence number.
        cseq: u64,
        /// The client order id.
        coid: String,
        /// Why.
        intent: WireCancelIntent,
    },
    /// Cancel several orders with one intent.
    CancelBatch {
        /// The command sequence number.
        cseq: u64,
        /// The client order ids, in the caller's order.
        coids: Vec<String>,
        /// Why.
        intent: WireCancelIntent,
    },
    /// Reprice/resize a resting order; absolute terms, idempotent by content.
    Modify {
        /// The command sequence number.
        cseq: u64,
        /// The whole resting order.
        order: OrderRequest,
        /// The requested quantity (`None` = keep).
        new_qty: Option<f64>,
        /// The requested price (`None` = keep).
        new_price: Option<f64>,
    },
    /// Prod the venue re-query for one order (the stuck-order watchdog's confirm).
    Confirm {
        /// The command sequence number.
        cseq: u64,
        /// The client order id.
        coid: String,
    },
    /// Teach the adapter the orders a previous session left resting: `(coid, unified symbol)` pairs.
    /// Idempotent; fire-and-forget (acked like a command).
    AdoptRestored {
        /// The command sequence number.
        cseq: u64,
        /// The pairs.
        pairs: Vec<(String, String)>,
    },
    /// The wire ids of restored client order ids (boot prefetch). Answered by [`TradeDown::WireIdsReply`].
    WireIds {
        /// The request id.
        rid: u64,
        /// The client order ids.
        coids: Vec<String>,
    },
    /// Query-only resolution of Written-but-unacknowledged submits after an epoch change. Answered by
    /// [`TradeDown::OrdersResolved`].
    ResolveOrders {
        /// The request id.
        rid: u64,
        /// The client order ids.
        coids: Vec<String>,
    },
    /// Record the venue-confirmed account identity (bookkeeping; a failure is a warning). Answered by
    /// [`TradeDown::IdentityRecorded`].
    IdentityRecord {
        /// The request id.
        rid: u64,
    },
    /// One reconcile fetch. Answered by `ReconProgress`* then `ReconReply`* (pages), `ReconFailed` or
    /// `ReconNotReady`.
    Recon {
        /// The request id.
        rid: u64,
        /// What to fetch.
        kind: ReconKind,
    },
    /// Liveness: the client pings at the period the server stated. Answered by [`TradeDown::Pong`].
    Ping,
    /// Teardown, in two phases.
    Detach {
        /// Which phase.
        phase: DetachPhase,
    },
    /// The daemon's account of this lease. Answered by [`TradeDown::TradeStatusIs`].
    TradeStatus {
        /// The request id.
        rid: u64,
    },
    /// The venue facts of ANY symbol of the lease's exchange, in the node's engine spelling (binance and
    /// aster: upper case, `.P` = the perpetual lane; bybit: a bare upper-case symbol is the linear
    /// perpetual). Public data answered over the trade key alone. Answered by
    /// [`TradeDown::SymbolFactsReply`]; gated by `gw_trade_symbol_facts`.
    ///
    /// A request is a **probe** (a look: the Preview, the Telegram look, the door's own lookup, which an
    /// order may still be refused after) or a **commit** (the node has sent `AdmitSymbol` for the
    /// symbol, or the boot has). The answer is the same; only a granted commit tells the account's
    /// reconcile client to read the symbol from now on, so a look never takes one of its sixteen slots.
    SymbolFacts {
        /// The request id.
        rid: u64,
        /// The symbol, bounded like every symbol on the wire.
        symbol: String,
        /// `true` = a look. Absent on the wire (a node that predates the field) reads as `false`: a
        /// COMMIT, which is what every request of such a node was before the field existed.
        #[serde(default)]
        probe: bool,
    },
}

/// A server frame on an open lease.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TradeDown {
    /// The transport acknowledgement of one command: queued at the gatewayhub. NOT a venue ack.
    CmdAck {
        /// The acknowledged command.
        cseq: u64,
    },
    /// One venue event with its ring position.
    Event {
        /// The ring's account, spelled as `AdapterStart.route_key`. A frame naming another account is a
        /// daemon bug and is never folded into this one's engine.
        account: String,
        /// The gatewayhub process the ring belongs to.
        gw_epoch: u64,
        /// The per-account, per-epoch sequence number.
        seq: u64,
        /// The event's own timestamp, epoch ms (the resume floor a history replay would start from).
        ts_ms: i64,
        /// Delivered from the ring after a resume.
        replayed: bool,
        /// The event, bit-exact (`serde_json` with `float_roundtrip`).
        event: Event,
    },
    /// The answer to [`TradeUp::AdapterStart`]. BOXED: the grids make it the widest reply.
    AdapterReply {
        /// The echoed request id.
        rid: u64,
        /// The outcome.
        reply: Box<AdapterReply>,
    },
    /// The answer to [`TradeUp::WireIds`]: `(coid, wire id)` for every coid whose wire id is not the
    /// coid itself.
    WireIdsReply {
        /// The echoed request id.
        rid: u64,
        /// The pairs.
        pairs: Vec<(String, String)>,
    },
    /// The answer to [`TradeUp::ResolveOrders`].
    OrdersResolved {
        /// The echoed request id.
        rid: u64,
        /// One row per coid asked.
        results: Vec<OrderResolution>,
    },
    /// The answer to [`TradeUp::IdentityRecord`].
    IdentityRecorded {
        /// The echoed request id.
        rid: u64,
        /// Whether the record was made.
        ok: bool,
        /// A human note (a warning's words); never a credential.
        #[serde(default)]
        note: String,
    },
    /// A running reconcile fetch's heartbeat: another frame of this `rid` within `next_in_ms`.
    ReconProgress {
        /// The echoed request id.
        rid: u64,
        /// The daemon's promise, ms; the client arms `max(floor, 3 x next_in_ms)` against the next frame.
        next_in_ms: u64,
    },
    /// One page of a reconcile answer. A list kind may span pages (`more`); a scalar kind is one page.
    ReconReply {
        /// The echoed request id.
        rid: u64,
        /// The page number, from 0, strictly +1.
        page: u32,
        /// Another page follows.
        more: bool,
        /// The rows.
        payload: ReconPayload,
    },
    /// The reconcile fetch failed after the venue was asked: an ERROR (the staleness escalation), never
    /// an empty answer.
    ReconFailed {
        /// The echoed request id.
        rid: u64,
        /// The error text.
        error: String,
    },
    /// The reconcile fetch was NOT attempted and why (no session yet, the epoch just changed, the
    /// wire-id prefetch failed, a degraded feed): a SKIPPED pass, distinct from an error.
    ReconNotReady {
        /// The echoed request id.
        rid: u64,
        /// Why.
        reason: String,
    },
    /// The answer to [`TradeUp::TradeStatus`].
    TradeStatusIs {
        /// The echoed request id.
        rid: u64,
        /// The report.
        status: TradeStatusReport,
    },
    /// The answer to [`TradeUp::SymbolFacts`]: the facts, or the human reason there are none (an unknown
    /// symbol, a lane the node does not run, a venue that does not answer); never a credential.
    SymbolFactsReply {
        /// The echoed request id.
        rid: u64,
        /// The outcome.
        facts: Result<WireSymbolFacts, String>,
    },
    /// Run a reconcile pass now (the bridge's `recon_trigger`, forwarded).
    ReconcileNow {
        /// Why.
        reason: String,
    },
    /// A resume asked for events the ring no longer holds (or never did): the client reconciles.
    ReplayUnavailable {
        /// The oldest `seq` the ring still holds.
        floor: u64,
    },
    /// Written after the stated heartbeat period of silence.
    Heartbeat,
    /// The answer to [`TradeUp::Ping`].
    Pong,
    /// The server is ending this lease, and why. The socket closes after it.
    Bye(TradeBye),
}
