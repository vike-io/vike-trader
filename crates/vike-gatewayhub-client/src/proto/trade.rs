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
//! * **Commands** carry a `command_sequence`: `Submit`, `SubmitBatch`, `Cancel`, `CancelBatch`, `Modify`, `Confirm`,
//!   `AdoptRestored`. `command_sequence` is per LEASE (a client's commands to one account: each lease numbers its own
//!   from 1), rises by exactly one per frame and never repeats inside a process; one TCP connection preserves
//!   order. The server answers each with [`TradeDown::CommandAck`], a
//!   TRANSPORT acknowledgement (the frame is queued at the gatewayhub), never a venue ack.
//!   `TradeOpened.received_through_command_sequence` is the watermark of everything the server has read, definitive
//!   inside one `gateway_epoch` because a takeover or reconnect is FENCED (the server reads the old
//!   connection to EOF before it answers the new open).
//! * **Requests** carry a `rid` and are answered by a frame that echoes it: `AdapterStart`, `WireIds`, `SymbolProperties`,
//!   `ResolveOrders`, `IdentityRecord`, `Recon`, `TradeStatus`, `VenueRead`. They place nothing and have no order to
//!   lose; a lost one is an error to the caller waiting on it.
//!
//! # Events
//!
//! [`TradeDown::Event`] carries one venue event with its position in the gatewayhub's per-account ring
//! (`gateway_epoch`, `seq`, `timestamp_ms`): the cursor the tradehub journals with the event
//! (`vike_model::EventRingCursor`). `gateway_epoch` is RANDOM per SESSION (a session is one account's ring, watermarks, dedup set and adapter; it is not ordered),
//! `seq` rises by one per event inside an epoch. `replayed` marks an event delivered from the ring
//! after a resume; it takes the same path as a live one.
//!
//! ⚠ **Adding a variant is the same three edits as the rest of the wire** (the variant, every exhaustive
//! table that names it, a golden line in `fixtures/gatewayhub_wire/`): `tests/wire_tag_fixtures.rs`
//! reads this file and refuses a variant no fixture carries.

use serde::{Deserialize, Serialize};
use vike_model::events::Event;
use vike_model::{OrderCancelIntent, OrderRequest, VenueRead, VenueReadKind};

use super::backend_dto::{
    AdapterReply, AdapterStart, ReconcileKind, ReconcilePayload, SymbolPropertiesWire,
    TradeStatusReport,
};

/// The daemon's `SymbolPropertiesReply` sentences that say "not now", never "no": its lookup slots are full,
/// its lookup thread could not start, or the venue's instrument list could not be had. The gatewayhub builds each
/// refusal's reason from these words and gives it its CODE where it builds it ([`SymbolPropertiesRefusal`]); a node
/// decides by the code and never matches the words.
pub const PROPERTIES_BUSY: &str =
    "too many symbol-facts lookups are running at the gatewayhub; ask again";
/// See [`PROPERTIES_BUSY`].
pub const PROPERTIES_NOT_STARTED: &str = "the gatewayhub could not start a symbol-facts lookup";
/// See [`PROPERTIES_BUSY`]. The daemon keeps this failure for 30 s itself.
pub const PROPERTIES_LIST_UNAVAILABLE: &str = "the venue's instrument list could not be fetched";
/// See [`PROPERTIES_BUSY`].
pub const PROPERTIES_LIST_EMPTY: &str = "the venue's instrument list came back empty";
/// See [`PROPERTIES_BUSY`]. The rate that converts a quote currency (decision 0183) could not be had from the
/// venue NOW: the symbol is not admitted this time (no rate, no admission), and nothing is remembered
/// about the symbol itself.
pub const PROPERTIES_RATE_UNAVAILABLE: &str =
    "the venue's quote-currency rate could not be fetched";

/// The daemon's answer to a `SymbolProperties` that asked it to FORGET a symbol (`forget: true`, decision 0205): the
/// reconcile client was told, nothing was looked up: the reason of the refusal whose code is
/// [`SymbolPropertiesRefusalCode::Forgotten`].
pub const SYMBOL_FORGOTTEN: &str = "the gatewayhub's reconcile client no longer reads this symbol";

/// The daemon's `SymbolPropertiesReply` words for an instrument that is GONE: the venue's own list, read whole, has
/// no row of that name (an empty answer of a per-symbol read says the same). A failed or partial read is never
/// worded this way: it is [`PROPERTIES_LIST_UNAVAILABLE`] or [`PROPERTIES_LIST_EMPTY`].
pub const PROPERTIES_NO_INSTRUMENT: &str = ": no instrument ";
/// See [`PROPERTIES_NO_INSTRUMENT`]. A dated future whose delivery has passed, on a venue whose list still shows it.
pub const PROPERTIES_EXPIRED: &str = "the contract has expired";

/// **Why the daemon answered a [`TradeUp::SymbolProperties`] with no properties**: a CODE the node decides by, and the
/// operator's sentence, which the node shows and never reads (it may quote a venue's own words). The daemon builds the
/// sentence from its own words ([`PROPERTIES_BUSY`] and its siblings) and sets the code in the same place, from the
/// kind of failure it is (decision 0301; `crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`'s `worded` maps a
/// bridge's kind), never from the sentence. [`Display`](std::fmt::Display) writes the sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolPropertiesRefusal {
    /// What kind of "no" it is.
    pub code: SymbolPropertiesRefusalCode,
    /// The operator's sentence; never a credential. Always the BARE text: the id is [`Self::refusal_id`].
    pub reason: String,
    /// The id of the bridge's refusal the sentence is (`ibkr.symbol.…`), `None` for free text and the daemon's own
    /// words. Additive (decision 0112): skipped while empty, so a frame without one is the bytes it always was, and
    /// an older peer's frame reads `None`. A node shows it ONCE, never decides on it (it decides on [`Self::code`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_id: Option<String>,
}

impl TradeDown {
    /// The `VenueReadReply` of request `rid`: the answer, or the refusal's BARE text with its id beside it.
    #[must_use]
    pub fn venue_read_reply(
        rid: u64,
        answer: Result<VenueRead, vike_utility::errors::CarriedRefusal>,
    ) -> Self {
        use vike_utility::errors::Declared as _;
        let refusal_id = answer.as_ref().err().and_then(|why| why.declared_id()).map(str::to_owned);
        TradeDown::VenueReadReply {
            rid,
            answer: answer.map_err(|why| why.text().to_owned()),
            refusal_id,
        }
    }
}

impl SymbolPropertiesRefusal {
    /// The refusal a person reads: the sentence under its bridge id when it has one.
    #[must_use]
    pub fn carried(&self) -> vike_utility::errors::CarriedRefusal {
        vike_utility::errors::CarriedRefusal::new(self.refusal_id.clone(), self.reason.clone())
    }
}

impl std::fmt::Display for SymbolPropertiesRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// The closed set of a [`SymbolPropertiesRefusal`]'s kinds. Five say "not now" ([`Self::is_transient`]: the node asks
/// again and remembers nothing about the symbol), two say the instrument is GONE ([`Self::is_gone`]), one answers a
/// `forget`, and [`Self::Verdict`] is every other definitive answer about the symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolPropertiesRefusalCode {
    /// Not now: the daemon's lookup slots, or the venue's lookup budget at the daemon, are full ([`PROPERTIES_BUSY`]).
    Busy,
    /// Not now: the daemon could not start a lookup thread ([`PROPERTIES_NOT_STARTED`]).
    NotStarted,
    /// Not now: the venue's instrument list could not be fetched ([`PROPERTIES_LIST_UNAVAILABLE`]).
    ListUnavailable,
    /// Not now: the venue's instrument list came back empty ([`PROPERTIES_LIST_EMPTY`]).
    ListEmpty,
    /// Not now: the rate that converts a quote currency could not be had from the venue
    /// ([`PROPERTIES_RATE_UNAVAILABLE`]).
    RateUnavailable,
    /// Gone: the venue's own list, read whole, has no instrument of that name ([`PROPERTIES_NO_INSTRUMENT`]).
    NoInstrument,
    /// Gone: a dated future whose delivery has passed, on a venue whose list still shows it ([`PROPERTIES_EXPIRED`]).
    Expired,
    /// The answer to a request that asked the daemon to FORGET a symbol ([`SYMBOL_FORGOTTEN`]).
    Forgotten,
    /// A definitive answer about the symbol: unknown here, no tick size, not linear, quoted in another currency, a
    /// venue that does not answer it.
    Verdict,
}

impl SymbolPropertiesRefusalCode {
    /// Whether the refusal says "not now" rather than anything about the symbol: the node asks again and remembers
    /// nothing.
    #[must_use]
    pub fn is_transient(self) -> bool {
        matches!(
            self,
            SymbolPropertiesRefusalCode::Busy
                | SymbolPropertiesRefusalCode::NotStarted
                | SymbolPropertiesRefusalCode::ListUnavailable
                | SymbolPropertiesRefusalCode::ListEmpty
                | SymbolPropertiesRefusalCode::RateUnavailable
        )
    }

    /// Whether the refusal says the instrument no longer exists at the venue, so asking again cannot heal it.
    #[must_use]
    pub fn is_gone(self) -> bool {
        matches!(
            self,
            SymbolPropertiesRefusalCode::NoInstrument | SymbolPropertiesRefusalCode::Expired
        )
    }
}

/// The answer to an accepted [`Request::TradeOpen`](super::Request::TradeOpen).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeOpened {
    /// The gatewayhub session the lease lives in (random per session: a session built again after a final detach or an orphan timeout is a new epoch). A client that sees it change
    /// across a reconnect knows every in-memory state it relied on (the command_sequence watermark, the ring, the
    /// dedup set, the adapter) is gone.
    pub gateway_epoch: u64,
    /// The lease's own generation: a late command from an old connection carries an older one.
    pub lease_epoch: u64,
    /// The highest `command_sequence` of this LEASE (this client's commands to this account) the gatewayhub has READ,
    /// definitive inside one `gateway_epoch`. A daemon keys the watermark by `(client_id, venue, account)`.
    pub received_through_command_sequence: u64,
    /// A random token the gatewayhub keeps in `<state_dir>/gateway-id`: proof the two processes share a
    /// project folder (the HALT sentinel's home). Compared by the CALLER, which reads its own file.
    #[serde(default)]
    pub project_token: String,
    /// **The server's heartbeat period in milliseconds.** The client arms
    /// `max(GATEWAY_TRADE_READ_FLOOR, 3 x heartbeat_ms)` after clamping this into
    /// `GATEWAY_TRADE_HEARTBEAT_MIN..=GATEWAY_TRADE_HEARTBEAT_MAX`, and pings at the same period.
    pub heartbeat_ms: u64,
    /// The oldest `seq` the ring still holds for this account (`Some(0)` = nothing yet). **`None` is a daemon
    /// that does not say** (an older one omits the field, and it is not sent for `None`): the client cannot
    /// tell an empty ring from a full one, so it never anchors on it and reconciles on a flap before its first
    /// event (W19g), instead of reading the missing field as "empty".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_ring_floor: Option<u64>,
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
    /// The daemon launched the venue re-query by order client id (`confirm`); the authoritative event
    /// arrives as an ordinary [`TradeDown::Event`].
    Querying,
    /// The venue has no lookup by order client id: the order stays UNKNOWN until reconcile or the
    /// stuck-order watchdog's last resort.
    NoResolver,
}

/// One order's answer in [`TradeDown::OrdersResolved`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderResolution {
    /// The order client id asked about.
    pub order_client_id: String,
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
    /// One order. Answered by [`TradeDown::CommandAck`]; a duplicate `(account, order client id)` is acked, not re-sent.
    Submit {
        /// The command sequence number.
        command_sequence: u64,
        /// The order, as the core built it.
        order: OrderRequest,
    },
    /// Several orders in one frame (at most `GATEWAY_TRADE_UP_FRAME_CEILING_BYTES`); each still produces its
    /// own event stream.
    SubmitBatch {
        /// The command sequence number.
        command_sequence: u64,
        /// The orders, in the caller's order.
        orders: Vec<OrderRequest>,
    },
    /// Cancel one order.
    Cancel {
        /// The command sequence number.
        command_sequence: u64,
        /// The order client id.
        order_client_id: String,
        /// Why.
        intent: OrderCancelIntent,
    },
    /// Cancel several orders with one intent.
    CancelBatch {
        /// The command sequence number.
        command_sequence: u64,
        /// The order client ids, in the caller's order.
        order_client_ids: Vec<String>,
        /// Why.
        intent: OrderCancelIntent,
    },
    /// Reprice/resize a resting order; absolute terms, idempotent by content.
    Modify {
        /// The command sequence number.
        command_sequence: u64,
        /// The whole resting order.
        order: OrderRequest,
        /// The requested quantity (`None` = keep).
        new_qty: Option<f64>,
        /// The requested price (`None` = keep).
        new_price: Option<f64>,
        /// A stop-limit's or a take-profit's requested TRIGGER (`ExecutionClient::modify_trigger`).
        /// Sent only to a daemon that advertised
        /// [`FEATURE_GATEWAY_TRADE_MODIFY_TRIGGER`](super::FEATURE_GATEWAY_TRADE_MODIFY_TRIGGER) (an older one
        /// would drop the key and move the limit alone); skipped when `None`, so every frame without it
        /// keeps its bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_trigger_price: Option<f64>,
    },
    /// Prod the venue re-query for one order (the stuck-order watchdog's confirm).
    Confirm {
        /// The command sequence number.
        command_sequence: u64,
        /// The order client id.
        order_client_id: String,
    },
    /// Teach the adapter the orders a previous session left resting: `(order client id, unified symbol)` pairs.
    /// Idempotent; fire-and-forget (acked like a command). Built and read through the `trade` module's
    /// `adopt_restored_frame` and `restored_orders` (`crate::trade::mirror`), the one spelling of both directions.
    AdoptRestored {
        /// The command sequence number.
        command_sequence: u64,
        /// The pairs.
        pairs: Vec<(String, String)>,
        /// `(order client id, venue order id)` for the pairs whose venue order id the node holds, so a bridge that keys
        /// its own state by the venue's id rebuilds it (`vike_execution::RestoredOrder::venue_order_id`). Keyed by
        /// order client id, never by position, so a split or a merge of the frame cannot hand one order another's id; a
        /// later entry for an order client id wins. Skipped when empty, so a frame without it keeps its bytes, and an
        /// older daemon (which ignores a key it does not know) is taught exactly what it was before.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        venue_ids: Vec<(String, String)>,
    },
    /// The wire ids of restored order client ids (boot prefetch). Answered by [`TradeDown::WireIdsReply`].
    WireIds {
        /// The request id.
        rid: u64,
        /// The order client ids.
        order_client_ids: Vec<String>,
    },
    /// Query-only resolution of Written-but-unacknowledged submits after an epoch change. Answered by
    /// [`TradeDown::OrdersResolved`].
    ResolveOrders {
        /// The request id.
        rid: u64,
        /// The order client ids.
        order_client_ids: Vec<String>,
    },
    /// Record the venue-confirmed account identity (bookkeeping; a failure is a warning). Answered by
    /// [`TradeDown::IdentityRecorded`].
    IdentityRecord {
        /// The request id.
        rid: u64,
    },
    /// One reconcile fetch. Answered by `ReconcileProgress`* then `ReconcileReply`* (pages), `ReconcileFailed` or
    /// `ReconcileNotReady`.
    Reconcile {
        /// The request id.
        rid: u64,
        /// What to fetch.
        kind: ReconcileKind,
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
    /// [`TradeDown::SymbolPropertiesReply`]; gated by `gateway_trade_symbol_properties`.
    ///
    /// A request is a **probe** (a look: the Preview, the Telegram look, the door's own lookup, which an
    /// order may still be refused after) or a **commit** (the node has sent `AdmitSymbol` for the
    /// symbol, or the boot has). The answer is the same; only a granted commit tells the account's
    /// reconcile client to read the symbol from now on, so a look never takes one of its sixteen slots.
    SymbolProperties {
        /// The request id.
        rid: u64,
        /// The symbol, bounded like every symbol on the wire.
        symbol: String,
        /// `true` = a look. Absent on the wire (a node that predates the field) reads as `false`: a
        /// COMMIT, which is what every request of such a node was before the field existed.
        #[serde(default)]
        probe: bool,
        /// **This node CONVERTS a quote currency** (decision 0183, phase 1): it may be answered a symbol
        /// quoted in a currency other than the account's, with the rate that converts it
        /// ([`SymbolPropertiesWire::quote_rate`]). Absent while `false`, so a node that does not convert sends
        /// the frame it always did, and a daemon that predates the field reads `false`: the symbol is
        /// then refused by the daemon's own fence if it is not quoted in the account's currency. The
        /// daemon honours it for the venues it has a rate source for (oanda, ctrader, ibkr, dukascopy, ig,
        /// fxcm: `TradeLaneBridge::symbol_facts_quoted`) and ignores it elsewhere.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        quoted: bool,
        /// **The node's engine GAVE `symbol` BACK** (decision 0205): the account's reconcile client stops reading
        /// it and its place in the client's list of sixteen is free. NOT a look and NOT a commit: the daemon looks
        /// nothing up and asks the venue nothing, it queues the forget on the reconcile worker and answers
        /// `SymbolPropertiesReply` refused with the code [`SymbolPropertiesRefusalCode::Forgotten`]. Absent while `false`.
        ///
        /// ⚠ **A node sends it with `probe: true`, always.** A daemon from before the field ignores the key and
        /// reads the frame as the look it also is: nothing is told, nothing is forgotten, and the answer is an
        /// ordinary properties reply instead of that code, which is how the node learns the daemon did not
        /// forget and keeps the symbol in the list it claims (`TradeLease::forget_symbol`). That is why this is a
        /// key and not a new frame (a frame the daemon cannot decode ends the lease) and why it needs no feature
        /// string: the reply is the negotiation, and the failure it guards against is harmless.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        forget: bool,

        /// **This node books a symbol the venue holds in a margin mode other than cross** (decision 0218):
        /// it reads [`SymbolPropertiesWire::margin_mode`] into the admission's `margin_mode`, so an isolated-only
        /// perpetual or a spot pair is booked in the pool the venue holds it in. Absent while `false`, so a
        /// node that predates the field sends the frame it always did, and a daemon that predates the field
        /// ignores the key. A daemon that DOES know the field REFUSES such a symbol by name when this is
        /// `false` (the node would book it cross), so a skewed pair keeps the old behaviour and never books
        /// an isolated position wrong. This build always sends `true`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        modes: bool,
    },
    /// **An operator's READ of the lease's account at the venue**: the balance with the per-exchange
    /// allowance where the venue has one, or the venue's own list of the account's open orders.
    /// Read-only: the daemon asks the venue and folds nothing, books nothing and changes nothing at the
    /// venue. Answered by [`TradeDown::VenueReadReply`]
    /// (the answer, or the sentence for why there is none: a bridge that offers no such read says
    /// `not offered by <venue>`); gated by `gateway_trade_venue_read`.
    ///
    /// The daemon bounds it so a loop of them cannot hammer a venue's API: one in flight per venue and a
    /// short minimum gap per account and read; a request over a bound is refused in words and reaches the
    /// venue not at all. It needs no started adapter, and a `live` account is as readable as a `demo`
    /// one (a read writes nothing).
    VenueRead {
        /// The request id.
        rid: u64,
        /// Which read.
        kind: VenueReadKind,
    },
}

/// A server frame on an open lease.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TradeDown {
    /// The transport acknowledgement of one command: queued at the gatewayhub. NOT a venue ack.
    CommandAck {
        /// The acknowledged command.
        command_sequence: u64,
    },
    /// One venue event with its ring position.
    Event {
        /// The ring's account, spelled as `AdapterStart.route_key`. A frame naming another account is a
        /// daemon bug and is never folded into this one's engine.
        account: String,
        /// The gatewayhub process the ring belongs to.
        gateway_epoch: u64,
        /// The per-account, per-epoch sequence number.
        sequence: u64,
        /// The event's own timestamp, epoch ms (the resume floor a history replay would start from).
        timestamp_ms: i64,
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
    /// The answer to [`TradeUp::WireIds`]: `(order client id, wire id)` for every order client id whose wire id is not the
    /// order client id itself.
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
        /// One row per order client id asked.
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
    ReconcileProgress {
        /// The echoed request id.
        rid: u64,
        /// The daemon's promise, ms; the client arms `max(floor, 3 x next_in_ms)` against the next frame.
        next_in_ms: u64,
    },
    /// One page of a reconcile answer. A list kind may span pages (`more`); a scalar kind is one page.
    ReconcileReply {
        /// The echoed request id.
        rid: u64,
        /// The page number, from 0, strictly +1.
        page: u32,
        /// Another page follows.
        more: bool,
        /// The rows.
        payload: ReconcilePayload,
        /// **The admitted symbols the daemon's reconcile client READ for this answer** (beyond the mounted
        /// one), in the client's own spelling: the client's `ReconcileClient::admitted_symbols` taken in the
        /// fetch's own critical section, named on page 0 of a LIST kind (orders, fills, positions) by a
        /// daemon that advertises `gateway_trade_reconcile_read`. `None` = the daemon does not say (an older one, a
        /// scalar kind, or a client that keeps no list because its reads span the whole account); `Some`
        /// is a claim and an empty list is one ("read the mounted symbol only"). Never written for
        /// `None`, so a reply that names nothing is byte-identical to the one an older daemon sends, and an
        /// older node ignores the key (a pure addition to a REPLY: decision 0112 verdict 3).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read: Option<Vec<String>>,
    },
    /// The reconcile fetch failed after the venue was asked: an ERROR (the staleness escalation), never
    /// an empty answer.
    ReconcileFailed {
        /// The echoed request id.
        rid: u64,
        /// The error text.
        error: String,
    },
    /// The reconcile fetch was NOT attempted and why (no session yet, the epoch just changed, the
    /// wire-id prefetch failed, a degraded feed): a SKIPPED pass, distinct from an error.
    ReconcileNotReady {
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
    /// The answer to [`TradeUp::SymbolProperties`]: the properties, or the refusal's code and the human reason there are
    /// none (an unknown symbol, a lane the node does not run, a venue that does not answer); never a credential.
    SymbolPropertiesReply {
        /// The echoed request id.
        rid: u64,
        /// The outcome.
        properties: Result<SymbolPropertiesWire, SymbolPropertiesRefusal>,
    },
    /// The answer to [`TradeUp::VenueRead`]: what the venue said, or the operator's sentence for why
    /// there is nothing (`not offered by <venue>`, a busy slot, a timeout, the venue's own refusal);
    /// never a credential.
    VenueReadReply {
        /// The echoed request id.
        rid: u64,
        /// The outcome: the answer, or the operator's sentence, always BARE (its id is `refusal_id`).
        answer: Result<VenueRead, String>,
        /// The id of the bridge's refusal `answer`'s sentence is (`ibkr.venue_read.…`), `None` for an answer, free
        /// text and the daemon's own words. Additive (decision 0112): skipped while empty, and an older peer's frame
        /// reads `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refusal_id: Option<String>,
    },
    /// Run a reconcile pass now (the bridge's `reconcile_trigger`, forwarded).
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
