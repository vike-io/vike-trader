//! The REAL socket backend: `IbkrTransportSocket` wraps the blocking `ibapi` client
//! (`ibapi::client::blocking::Client`, the registry crate `ibapi` =5.4.0, sync-only) and
//! implements the `IbkrTransport` seam. Thin glue ONLY — every order-correctness decision lives
//! in the pure modules (`event_mapper`/`order`/`contract`/`id_registry`); this file just marshals
//! vike types ⇄ ibapi types and pumps ibapi's global order-update stream onto an internal channel
//! that the exec loop drains via `next_recv`.
//!
//! The ibapi client is reached through the small [`IbkrApiSession`] seam (the real one is
//! [`IbkrApiSessionClient`]), so the connect sequence ([`IbkrTransportSocket::start_watched`]), the pump and the snapshot
//! bookkeeping are exercised by unit tests over a double: the production call site and the seed it
//! queues are tested, not a copy of them. What no test can reach is ibapi itself and the gateway behind it:
//! the runtime validation is the paper smoke against a live IB Gateway
//! (`docs/ops/ibkr-socket-gateway.md` is the bring-up).
//!
//! ⚠ That smoke has now RUN, on the CI box 2026-08-19, against a real IB Gateway — submit → accept →
//! cancel on the paper account, with the order client id intact on both terminal events. Until then this file
//! had been compiled and reviewed but never spoken to a gateway, so every mapping decision below
//! was an argument from `ibapi`'s published source rather than an observation. ⚠ **The connect-time
//! learning below (`reqAllOpenOrders` + `reqExecutions` + their end markers) has NOT yet run against
//! a real gateway**; the ibapi behaviour it relies on was read from the 4.2.0 source and re-read against 5.1.0's and 5.2.0's
//! (see "What ibapi does") and the first attended run is the IBKR rollout step.
//!
//! ## Mapping decisions (verified against `ibapi`'s source, 4.2.0, 5.1.0 and 5.2.0, not the research notes)
//! - Placement uses `Client::submit_order` (fire-and-forget) + a single global
//!   `Client::order_update_stream()` as the event pump — the ibapi-documented pairing. The stream
//!   yields `OrderUpdate::{OrderStatus, ExecutionData, CommissionReport, OpenOrder, OrderBound}` for
//!   ALL orders, which we translate into `IbkrInbound`.
//! - ibapi's `OrderStatus` has NO `orderRef` field, so status inbound resolves by numeric `order_id`
//!   (the exec loop binds `order_id → order_client_id` at submit; dual resolution still covers exec/commission,
//!   which DO carry `order_reference`).
//! - A subscription `Notice` → `IbkrInbound::Error{code, order_id, msg}` ([`polled_notice`]): `order_id` is the
//!   notice's `request_id` (5.2.0 copies only ORDER-bound notices onto this stream, so it names the order; 0 when
//!   absent, as 4.2.0's always was), `msg` is IB's own `[code] text`. The mapper drops advisories via
//!   `classify_code`; an order-rejection-class code ends the order it names, and an id-less one is attributed
//!   to the sole still-unacked order so nothing vanishes (see `EventMapper::on_error` — routing lives in the
//!   pure module, not here). A terminal subscription `Err` (connection
//!   reset/shutdown) → `IbkrInbound::StreamDead` and stops the pump — this backend has NO
//!   auto-reconnect, and the variant says so.
//! - Fill `ts` is stamped at receive time in epoch-millis (vike's `FillEvent.ts` unit, matching the
//!   crypto bridges). ibapi surfaces the exchange fill time only as a preformatted `String`
//!   (`Execution.time`); parsing it to epoch-millis needs the account timezone and is deferred to
//!   the live smoke (STILL deferred: the smoke rests a limit far from the market, so no `Execution`
//!   was ever observed). The forex `symbol` echo (`SYMBOL.EXCHANGE.CURRENCY` rebuilt from
//!   `ed.contract.{symbol,exchange,currency}`) is likewise unobserved for a `CASH` pair, and is NOT what a fill
//!   carries: the exec loop stamps the symbol the order was submitted under (`EventMapper::fill_symbol`), because the engine
//!   drops a fill whose symbol it does not mount in silence.
//!
//! ## Connect-time learning (what a REBUILT connection asks, and why it is bounded)
//! IB replays nothing on its own, so a fresh connection's order client id⇄orderId map is empty: it could neither
//! cancel an order that rested across the outage nor follow its fills. [`socket_connect_seed`] therefore
//! ends with `Connected`, on which the exec loop asks for the open orders; when the venue says that
//! snapshot ended it asks for the day's executions; when those ended the connection announces itself
//! learned (`ExecLink{up: true}`, `execution::run_execution`'s `deliver`). Three properties are this file's:
//! - **Non-blocking.** `Client::all_open_orders` and `Client::executions` WRITE the request and return
//!   (the 4.2.0, 5.1.0 and 5.2.0 source: `send_shared_request` / `send_request`, neither waits for a reply), so the exec
//!   thread, which also services Shutdown and the readiness gate, is never stalled. The rows arrive on
//!   the global stream; the *end* of each request arrives only on the per-request subscription, which a
//!   short-lived thread (`ibkr-snapshot`) reads, bounded by [`SNAPSHOT_DEADLINE`].
//! - **Ordered.** That end marker travels a different queue than the rows, so it is released into the
//!   inbound channel only by the pump, and only when the pump has drained the stream (a poll that found
//!   it empty): every row the venue sent before the end is on the channel before the marker.
//! - **Bounded in failure.** A request that errors, whose subscription ends in an error, or that does not
//!   end in time yields `SnapshotEnd { complete: false }`; the connection is then NOT announced as
//!   learned and the host keeps whatever hold it has (the gatewayhub holds the session).
//!
//! ## What ibapi does (read from the registry source, 4.2.0 first, 5.1.0 and 5.2.0 after each bump; not from docs.rs)
//! - `process_orders` fans `OpenOrder`, `ExecutionData` and `CommissionsReport` onto the global
//!   `order_update_stream` FIRST (`send_order_update`), then to the per-request channel, so the rows of a
//!   request land in `pump_loop` whether or not its subscription is read.
//! - Since 5.2.0 that holds for THIS client's frames only: a frame naming another client id (`is_foreign`, a
//!   decoded `clientId` that is not ours, 0 included) goes only to the request that asked, so the pump never sees
//!   it. A frame with no `clientId` still counts as ours. `translate` keeps its filter: it is what decides for a
//!   frame the library took as ours.
//! - `Orders`/`Executions` inherit `StreamDecoder::cancel_message`'s `Err(NotImplemented)`, so dropping
//!   the subscription sends no cancel frame; `OpenOrderEnd` / `ExecutionDataEnd` decode to
//!   `Error::EndOfStream`, which `Subscription::next*` reports as `None` — indistinguishable from a
//!   timeout except by the time it took, which [`wait_for_end`] uses.
//! - A subscription dropped before its `ExecutionDataEnd` arrives makes that frame unroutable
//!   (a `warn!` in ibapi, nothing else): the waiter therefore keeps the subscription until the end.
//! - **A write that FAILED put no whole frame on the wire** (owner ruling F-5; read in 5.7.0's `transport/sync.rs`
//!   and `connection/sync.rs`, 2026-11-24), so a failed `submit_order`, amend or `cancel_order` is NOT SENT
//!   (decision 0303) and its synthesized answer stands. Every refusal before the socket (`verify_order`, the encode,
//!   the session gate a reconnect holds, `ensure_connected`) writes nothing. The write itself is ONE `write_all` of the
//!   length-prefixed packet (`Connection::write_raw`) under `TcpSocket`'s writer lock, which a reconnect also takes, so
//!   no frame straddles two connections; it is never written again after a failure; and the `TcpStream` blocks with no
//!   send timeout (only `set_read_timeout`), so a send fails only once the connection is broken and carries no byte
//!   more. TWS acts on a whole frame only, and holds a cut one at most. Re-read these on each ibapi bump.
//!
//! ## ⚠ The fill is STILL unobserved — and what it would take
//! 1. **The account is exclusive, and handing it back is the expensive half.** Starting IB Gateway
//!    EVICTS the Client Portal Gateway session the CI box normally holds — one account, one claimant,
//!    across both products (`docs/ops/ibkr-socket-gateway.md`).
//! 2. **An open session.** A marketable order fills only while the venue trades; `CASH`/IDEALPRO forex
//!    has the widest window.
//! 3. **A marketable order and a flatten.** One unit through the touch, then the opposite side.
//!
//! ⚠ The cpapi backend cannot stand in for this. Its fill path was measured DEAD on 2026-08-23 (its
//! WS `sor` topic answers nothing — `transport/cpapi/mod.rs`'s module doc), so a fill produced there
//! would emit no event at all.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use ibapi::client::blocking::{Client, NoticeStream, Subscription};
use ibapi::contracts::{Contract, Currency, Exchange, OptionRight, SecurityType, Symbol};
use ibapi::orders::conditions::{PriceCondition, TriggerMethod};
use ibapi::orders::{
    Action, ExecutionFilter, ExecutionSide, Order, OrderCondition, OrderUpdate,
    TimeInForce as IbTif,
};
use ibapi::subscriptions::SubscriptionItem;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::venue_link::VenueLink;
use vike_marketdata::OptionKind;

use super::gateway_gate::{SettleCount, Settling, gate_for};
use super::link_watch::{Notices, Watch, WatchTimes, Watching};
use super::{
    AmendOutcome, IbkrInbound, IbkrTransport, OwnOrders, SnapshotRequest, socket_connect_seed,
};
use crate::config::IbkrConfig;
use crate::execution::event_mapper::{
    IbkrCommissionReport, IbkrExecutionDetails, IbkrOpenOrder, IbkrOrderStatus,
};
use crate::execution::order::IbkrOrderSpec;
use crate::instrument::contract::{IbkrContract, SecType};
use crate::transport::error_codes::IbkrError;

/// How long a snapshot request (`reqAllOpenOrders`, `reqExecutions`) may take to end before it is
/// reported as NOT complete. Generous: the day's executions of a busy account are a few hundred rows.
pub(crate) const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(10);

/// How long ONE network step of an adapter OPEN (the exec connect, the reconcile connect, the grid
/// pre-fetch) may take before the open gives up on it. `ibapi`'s `Client::connect` dials with no timeout
/// and its handshake / `next_valid_order_id` / `contract_details` wait with none either, so a gateway that
/// accepts the socket but has lost its link to IBKR's servers (2026-10-17: errors 2110/2103) would hold the
/// open, and the tradehub's serial lease boot behind it, for ever. Above [`SNAPSHOT_DEADLINE`] and the
/// reconcile `READ_DEADLINE` (10 s each), so a slow answer the gateway does give still wins.
pub(crate) const OPEN_DEADLINE: Duration = Duration::from_secs(15);

/// How long a close that waited may hold the exec thread on its position read ([`IbkrTransport::held_position`]). The
/// answer comes from the gateway's memory, and a cancel may be queued behind it.
const HELD_READ_WAIT: Duration = Duration::from_secs(5);

/// How long one `contract_details` read may hold the exec thread (it has no timeout of its own): the contract id of a
/// trailing order's price condition ([`IbkrTransportSocket::activation_condition`]) and the lot of an iceberg
/// ([`IbkrTransportSocket::lot_refusal`]).
const DETAILS_READ_WAIT: Duration = Duration::from_secs(5);

/// Run one blocking `ibapi` open step on a throwaway thread and stop waiting for it after `within`:
/// `IbkrError::Connect` naming `what` when the deadline wins. The thread is ABANDONED, not joined (the
/// blocking wait is what is being escaped): it finishes whenever `ibapi` lets go, and whatever it built
/// (a connected client) is dropped there, which frees the gateway's client id. Same pattern as
/// `crates/vike-transport/src/dial.rs`'s `call_with_deadline`.
///
/// `settle` is the gateway's count of threads still settling (`GatewayGate::dial` passes it): the abandoned
/// thread is counted until it returns, and the next dial on that gateway waits for it, so a client id the
/// abandoned open still holds is not asked for a second time.
pub(crate) fn bounded<T: Send + 'static>(
    what: &str,
    within: Duration,
    settle: Option<&SettleCount>,
    f: impl FnOnce() -> Result<T, IbkrError> + Send + 'static,
) -> Result<T, IbkrError> {
    bounded_yielding(what, within, settle, || false, f)
}

/// How often [`bounded_yielding`] looks at its `yield_now` between waits for the open.
const YIELD_POLL: Duration = Duration::from_millis(25);

/// [`bounded`] that also stops waiting, at once, when `yield_now` answers `true` (polled every [`YIELD_POLL`]): the
/// DATA lane's dial gives the gateway's turn back to an order-lane connect that arrived while it waited. The open
/// thread is abandoned like a timeout's, and the error names the yield.
pub(crate) fn bounded_yielding<T: Send + 'static>(
    what: &str,
    within: Duration,
    settle: Option<&SettleCount>,
    mut yield_now: impl FnMut() -> bool,
    f: impl FnOnce() -> Result<T, IbkrError> + Send + 'static,
) -> Result<T, IbkrError> {
    // one-shot: at most 1 message (the closure's result).
    let (tx, rx) = mpsc::channel();
    let hold = settle.map(Settling::hold);
    thread::Builder::new()
        .name("ibkr-open".into())
        .spawn(move || {
            let _hold = hold;
            // Receiver gone = the open gave up; the late result is dropped here.
            let _ = tx.send(f());
        })
        .map_err(|e| IbkrError::Connect(format!("{what}: could not start its thread: {e}")))?;
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.min(YIELD_POLL)) {
            Ok(done) => return done,
            Err(mpsc::RecvTimeoutError::Timeout) if left > YIELD_POLL => {
                if yield_now() {
                    return Err(IbkrError::Connect(format!(
                        "{what} gave way to an order-lane connect on this gateway and was abandoned"
                    )));
                }
            }
            // The deadline, or the open thread ended without a result (a panic): the same answer.
            Err(_) => {
                return Err(IbkrError::Connect(format!(
                    "{what} did not answer within {} ms (the gateway took the connection but never \
                     completed it: its link to IBKR's servers may be down)",
                    within.as_millis()
                )));
            }
        }
    }
}

/// One poll of the order-update stream.
pub(crate) enum Polled {
    Update(Box<OrderUpdate>),
    /// A TWS notice bound to the stream: the order it names (0 = none), and IB's words.
    Notice {
        code: i32,
        order_id: i32,
        message: String,
    },
    /// A terminal transport error (connection reset / shutdown).
    Failed(String),
    /// The stream ended cleanly.
    Closed,
    /// Nothing arrived within the poll: the stream is drained at this instant.
    Idle,
}

/// The order-update stream, as the pump reads it.
pub(crate) trait Updates: Send {
    /// Block up to `within` (at least a few milliseconds: see [`IbkrApiSessionClient`]).
    fn poll(&mut self, within: Duration) -> Polled;
}

/// One snapshot request in flight (the per-request subscription of `all_open_orders` /
/// `executions`).
pub(crate) trait SnapshotHandle: Send {
    /// Wait up to `within` for the venue to end the request: `true` = it did, `false` = it failed or
    /// did not end in time.
    fn wait(self: Box<Self>, within: Duration) -> bool;
}

/// What the socket backend needs of an IB API session. The real one is [`IbkrApiSessionClient`]; the unit
/// tests' double scripts the same calls.
pub(crate) trait IbkrApiSession: Send + Sync {
    /// The next id the library itself would allocate (the fallback when IB's is unreadable).
    fn local_next_order_id(&self) -> i32;
    /// IB's `nextValidId` (a request to the server).
    fn next_valid_order_id(&self) -> Result<i32, String>;
    fn submit_order(&self, order_id: i32, contract: &Contract, order: &Order)
    -> Result<(), String>;
    fn cancel_order(&self, order_id: i32) -> Result<(), String>;
    /// `reqAllOpenOrders`: writes the request and returns at once.
    fn all_open_orders(&self) -> Result<Box<dyn SnapshotHandle>, String>;
    /// `reqExecutions` for one client id: writes the request and returns at once.
    fn executions(&self, client_id: i32) -> Result<Box<dyn SnapshotHandle>, String>;
    /// The ONE global order-update stream.
    fn order_updates(&self) -> Result<Box<dyn Updates>, String>;
    /// The client's global notices (connectivity, farm status), bound before the connect so the handshake burst
    /// is in them; taken once. **Default `None`**: a session without them is not watched for its link.
    fn notices(&self) -> Option<Box<dyn Notices>> {
        None
    }
    /// Whether the client is connected to the gateway (`ibapi` reconnects by itself and is not while it does).
    fn connected(&self) -> bool {
        true
    }
    /// Close the client now: the gateway frees its client id and `ibapi` stops reconnecting a zombie that would
    /// take the id back. Idempotent. **Default no-op.**
    fn close(&self) {}
    /// What `account` holds of `contract` now, net and signed ([`IbkrTransport::held_position`]). **Default: an error.**
    fn held_position(&self, contract: &Contract, account: &str) -> Result<f64, String> {
        let _ = (contract, account);
        Err("this session cannot read a position".to_string())
    }
    /// The ONE contract id `contract_details` names for `contract` (a price condition names it). **Default: an error.**
    fn contract_id(&self, contract: &Contract) -> Result<i32, String> {
        let _ = contract;
        Err("this session cannot look a contract id up".to_string())
    }
    /// The lot `contract_details` states for `contract` ([`lot_of`]), `None` when it states none. **Default: an error.**
    fn lot_size(&self, contract: &Contract) -> Result<Option<f64>, String> {
        let _ = contract;
        Err("this session cannot look a lot size up".to_string())
    }
}

/// The lot an iceberg's display must be a multiple of: IB's `suggestedSizeIncrement` (`ContractDetails`), when every row
/// of the details names the same positive one (a stock on SMART answers one row), else none. It is the ROUND LOT, not the
/// order's size step (`minSize`/`sizeIncrement`, 1 share on a US stock): Regulation NMS Rule 600(b)(93) sets a US stock's
/// round lot by its average closing price since 2025-11-03 (100 shares up to $250, 40 up to $1,000, 10 up to $10,000, 1
/// above; reassigned each May and November), and the field moved from the 100 of ibapi 5.2.0's own US-stock fixture to the
/// 40 IB answered for AAPL on paper 2026-11-09. Exchanges take a reserve order's displayed size in round lots. That IB's
/// `[201] Display size should be a multiple of lot size` means this lot is MEASURED (`tests/ibkr_smokes/ibkr_iceberg_lot_smoke.rs`,
/// paper 2026-11-10): AAPL display 40 accepted, 1 and 100 refused by IB with that `[201]`.
fn lot_of(rows: impl IntoIterator<Item = Option<f64>>) -> Option<f64> {
    let mut agreed = None;
    for lot in rows {
        let lot = lot.filter(|l| l.is_finite() && *l > 0.0)?;
        if agreed.is_some_and(|a| a != lot) {
            return None;
        }
        agreed = Some(lot);
    }
    agreed
}

/// The end markers of finished snapshot requests, waiting for the pump to release them in order.
#[derive(Default)]
struct Markers(Mutex<Vec<(SnapshotRequest, bool)>>);

impl Markers {
    fn push(&self, snapshot: SnapshotRequest, complete: bool) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push((snapshot, complete));
    }

    fn take(&self) -> Vec<(SnapshotRequest, bool)> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn pending(&self) -> bool {
        !self.0.lock().unwrap_or_else(PoisonError::into_inner).is_empty()
    }
}

/// The live socket-backed transport. Owns the session (for place/cancel/the snapshot requests) and the
/// receiving end of the pump channel (for `next_recv`); a background thread owns the order-update
/// stream and translates each item onto `tx`.
pub struct IbkrTransportSocket {
    session: Arc<dyn IbkrApiSession>,
    /// The API client id this connection places orders under: the filter applied to the execution
    /// replay (`request_executions`) and half of [`IbkrTransport::ownership`].
    client_id: i32,
    /// The account every order is placed in (`build_order`): [`Self::account`].
    account: String,
    /// `config.extended_hours` (`venue.ibkr.<tier>.extended_hours`): see [`Self::order_for`]. OFF unless
    /// [`Self::with_extended_hours`] set it, which is what keeps every other door of this type as it was.
    extended_hours: bool,
    /// A clone of the pump channel's sender, used ONLY to synthesize an order-rejection inbound when
    /// `submit_order` fails synchronously (so a rejected intent never silently vanishes).
    tx: Sender<IbkrInbound>,
    inbound: mpsc::Receiver<IbkrInbound>,
    markers: Arc<Markers>,
    /// How long a snapshot request may take to end ([`SNAPSHOT_DEADLINE`]; a test shortens it).
    snapshot_deadline: Duration,
    /// How long the exec thread waits for a position read ([`HELD_READ_WAIT`]; a test shortens it).
    held_read_wait: Duration,
    /// The position read still out, abandoned at [`Self::held_read_wait`]: [`IbkrTransport::held_position`] asks no second.
    held_read_out: SettleCount,
    /// Set when the transport is dropped; the pump checks it every poll and then ends, which drops its
    /// order-update stream and with it the last hold on the ibapi client (see `Drop` below).
    stop: Arc<AtomicBool>,
    /// The gateway's settling count, when watched: the drop takes a hold on it at once and hands it to the pump,
    /// which lets go after closing the client, so a dial never starts inside the up-to-[`IDLE_POLL`] window
    /// between the drop and the close (the old client id is still held then).
    settle: Option<SettleCount>,
    closing: Closing,
    /// The link watchdog's word on the server link ([`IbkrTransport::server_link_broken`]), when watched.
    server_down: Option<Arc<AtomicBool>>,
}

/// The settling hold a dropped transport hands its pump (see [`IbkrTransportSocket::settle`]). `Done` is the pump
/// saying it has already ended, so a drop after that holds nothing (a hold nobody would release).
#[derive(Default)]
enum ClosingState {
    #[default]
    Running,
    /// Held for its DROP only (the count goes down when the state is replaced).
    Holding(#[expect(dead_code)] Settling),
    Done,
}

type Closing = Arc<Mutex<ClosingState>>;

/// ⚠ The pump owns the order-update `Subscription`, which holds an `Arc` of ibapi's message bus. ibapi's
/// `Client::drop` only half-closes the socket and does not wake that stream, so a pump that waits for its
/// receiver to fail (it only notices on a send, and an idle stream sends nothing) keeps the bus, and so the
/// TCP connection and the gateway's client id, alive after the session was retired. Setting the flag lets
/// the pump end within one poll ([`IDLE_POLL`]) and release them.
impl Drop for IbkrTransportSocket {
    fn drop(&mut self) {
        // The hold first, then the flag: the pump cannot finish before the hold exists.
        if let Some(settle) = &self.settle {
            let mut state = self.closing.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(*state, ClosingState::Running) {
                *state = ClosingState::Holding(Settling::hold(settle));
            }
        }
        self.stop.store(true, Ordering::Release);
    }
}

impl IbkrTransportSocket {
    /// Connect the blocking ibapi client to the Gateway/TWS at `config.host:config.port` under
    /// `config.client_id`, seed the id/account signals, and spawn the order-update pump. Maps any ibapi
    /// connect/stream failure to `IbkrError::Connect` (→ `Unavailable` at the app root → the account does not run).
    /// `link`: the account's [`VenueLink`], which the link watch writes (`None`: nothing is reported).
    pub fn connect(
        config: &IbkrConfig,
        link: Option<VenueLink>,
    ) -> Result<IbkrTransportSocket, IbkrError> {
        Self::open(config, OPEN_DEADLINE, link)
    }

    /// [`Self::connect`] under a `within` bound ([`bounded`]), reporting to no link: the dial, the handshake and the
    /// next-valid-id read of [`Self::start_watched`] are all `ibapi` waits with no timeout of their own. The tests' door.
    #[cfg(test)]
    pub(crate) fn connect_within(
        config: &IbkrConfig,
        within: Duration,
    ) -> Result<IbkrTransportSocket, IbkrError> {
        Self::open(config, within, None)
    }

    fn open(
        config: &IbkrConfig,
        within: Duration,
        link: Option<VenueLink>,
    ) -> Result<IbkrTransportSocket, IbkrError> {
        // The MASTER client (id 0) is bound by TWS to every order made by hand, and a hand-typed `Order Ref`
        // on one passes the ownership test of this bridge: an exec account may not use it (review 18, R4).
        refuse_master_client(config.client_id)?;
        let config = config.clone();
        let gate = gate_for(&format!("{}:{}", config.host, config.port));
        // A gateway whose link to IBKR was lost is rebuilt on a spaced schedule: a refused attempt dials nothing.
        gate.permit_execution()?;
        let watching = Watching { gate: Arc::clone(&gate), times: WatchTimes::PRODUCTION, link };
        let (opened, timed_out) = gate.dial("exec connect", within, move || {
            let addr = format!("{}:{}", config.host, config.port);
            let (client, notices) = Client::builder()
                .address(addr.clone())
                .client_id(config.client_id)
                .connect_with_notice_stream()
                .map_err(|e| {
                    IbkrError::Connect(format!("{addr} (client_id={}): {e}", config.client_id))
                })?;
            // The login named its accounts at the handshake: a stored `ACCOUNT` must be one of them,
            // and an account that stores none trades the login's one (refused: the client drops here).
            let account = crate::config::account_from_gateway(
                &config.account,
                client.handshake_managed_accounts(),
            )
            .map_err(|why| IbkrError::Unsupported(why.to_string()))?;
            Self::start_watched(
                Arc::new(IbkrApiSessionClient::new(client, notices)),
                config.client_id,
                &account,
                Some(watching),
            )
            .map(|transport| transport.with_extended_hours(config.extended_hours))
        });
        if opened.is_err() {
            gate.execution_failed(timed_out);
        }
        opened
    }

    /// [`Self::start_watched`] with no link watchdog: the unit tests' door (a double has no notices).
    #[cfg(test)]
    pub(crate) fn start(
        session: Arc<dyn IbkrApiSession>,
        client_id: i32,
        account: &str,
    ) -> Result<IbkrTransportSocket, IbkrError> {
        Self::start_watched(session, client_id, account, None)
    }

    /// Everything `connect` does once the session exists. Separate so the unit tests drive THE
    /// production sequence (the seed, the stream, the pump) over a double. `watching` is the link watchdog
    /// the pump runs beside the stream (`link_watch`); `None` = unwatched.
    pub(crate) fn start_watched(
        session: Arc<dyn IbkrApiSession>,
        client_id: i32,
        account: &str,
        watching: Option<Watching>,
    ) -> Result<IbkrTransportSocket, IbkrError> {
        let (tx, rx) = mpsc::channel::<IbkrInbound>();

        // Seed the exec loop: IB's server-assigned next valid order id (the local floor if it is
        // unreadable), the account readiness (the blocking handshake has completed once the session
        // exists) and `Connected`, which makes the exec loop LEARN the venue. All three are folded by
        // the exec loop before it takes a command.
        let seed = session.next_valid_order_id().unwrap_or_else(|_| session.local_next_order_id());
        for inbound in socket_connect_seed(seed) {
            let _ = tx.send(inbound);
        }

        // The ONE global order-update stream = the event pump. Create it before any order so no
        // status/exec/commission is missed.
        let updates = session
            .order_updates()
            .map_err(|e| IbkrError::Connect(format!("order_update_stream: {e}")))?;

        let markers = Arc::new(Markers::default());
        let stop = Arc::new(AtomicBool::new(false));
        let settle = watching.as_ref().map(|w| Arc::clone(w.gate.settling()));
        let closing = Closing::default();
        let watch = watching.map(|w| Watch::new(w, session.notices(), Instant::now()));
        let server_down = watch.as_ref().map(Watch::server_link_flag);
        let pump = InboundReader {
            closing: Arc::clone(&closing),
            updates,
            tx: tx.clone(),
            markers: Arc::clone(&markers),
            own: OwnOrders { client_id, account: account.to_string() },
            stop: Arc::clone(&stop),
            session: Arc::clone(&session),
            watch,
            settle: settle.clone(),
        };
        thread::Builder::new()
            .name("ibkr-pump".into())
            .spawn(move || pump.run())
            .map_err(|e| IbkrError::Connect(format!("pump thread spawn: {e}")))?;

        Ok(IbkrTransportSocket {
            session,
            client_id,
            account: account.to_string(),
            extended_hours: false,
            tx,
            inbound: rx,
            markers,
            snapshot_deadline: SNAPSHOT_DEADLINE,
            held_read_wait: HELD_READ_WAIT,
            held_read_out: SettleCount::default(),
            stop,
            settle,
            closing,
            server_down,
        })
    }

    /// The account every order is placed in: the stored `ACCOUNT`, or the one the gateway's login named at
    /// the connect ([`crate::config::account_from_gateway`]).
    pub(crate) fn account(&self) -> &str {
        &self.account
    }

    /// This transport with the account's `extended_hours` switch applied ([`Self::order_for`]). Said once,
    /// loudly, when on: it changes which orders IBKR may fill outside the regular session.
    pub(crate) fn with_extended_hours(mut self, on: bool) -> Self {
        if on {
            tracing::warn!(
                account = %self.account,
                "ibkr: EXTENDED HOURS is ON for this account (`venue.ibkr.<tier>.extended_hours`): every \
                 US-stock LIMIT order is sent with outsideRth and includeOvernight, so it may fill \
                 pre-market, after-hours and in the overnight session, and a GTC one goes out as DAY (IBKR \
                 refuses GTC with includeOvernight: error 10052); an IOC or FOK one is flagged for the \
                 pre-market and after-hours only (outsideRth alone: IBKR refuses includeOvernight with \
                 them); market and stop orders are not flagged"
            );
        }
        self.extended_hours = on;
        self
    }

    /// The `ibapi` order `place_order` and `modify_order` both hand the session. **Only a LIMIT order on a
    /// STOCK is ever flagged `outside_rth` + `include_overnight`, and only when the account's switch is on**
    /// (of the kinds the node sends only a limit order is eligible outside the regular session, and the
    /// overnight session is US stocks and ETFs): a market, stop or stop-limit (`STP LMT`) order, and a forex
    /// or futures order, are built exactly as with the switch off. A limit-on-open (`LMT` with TIF `OPG`) is
    /// not flagged either: it takes part in the opening auction and nothing else, so no session flag means
    /// anything to it (and `extended_hours_flags` would send it `DAY`).
    /// The amend re-places the order under its own id, so it goes through here and keeps its flags.
    fn order_for(&self, spec: &IbkrOrderSpec, contract: &IbkrContract) -> Order {
        build_order(spec, &self.account, self.flagged(spec, contract))
    }

    /// Whether [`Self::order_for`] flags this order for extended hours.
    fn flagged(&self, spec: &IbkrOrderSpec, contract: &IbkrContract) -> bool {
        self.extended_hours
            && spec.order_type == "LMT"
            && spec.tif != "OPG"
            && contract.sec_type == SecType::Stk
    }

    /// Why this order is not sent: a flagged one that goes out with `includeOvernight` (a GTC or DAY one,
    /// [`extended_hours_flags`]) carrying an attribute IB takes on no order it may route to its overnight venue.
    /// MEASURED on the paper gateway 2026-11-09 with the switch on, each ACCEPTED and then ended by IB with `[201] Order
    /// rejected - reason:<kind> orders not supported for this combination of exchange and security type`: an iceberg, an
    /// all-or-none, a sweep-to-fill, a discretionary and a good-after order. A `hidden` one was accepted (IB's row:
    /// `OVERNIGHT + DAY`) and stays sent. An IOC or FOK goes out with `outsideRth` alone and is not judged here.
    fn overnight_refusal(&self, spec: &IbkrOrderSpec, contract: &IbkrContract) -> Option<String> {
        if !(self.flagged(spec, contract) && extended_hours_flags(spec.tif).2) {
            return None;
        }
        let (_, what) = [
            (spec.display_size.is_some(), "an iceberg (`displaySize`)"),
            (spec.all_or_none, "an all-or-none order (`allOrNone`)"),
            (spec.sweep_to_fill, "a sweep-to-fill order (`sweepToFill`)"),
            (
                spec.discretionary_amt.is_some_and(|d| d != 0.0),
                "a discretionary amount (`discretionaryAmt`)",
            ),
            (spec.good_after_time.is_some(), "a time release (`goodAfterTime`)"),
        ]
        .into_iter()
        .find(|(asked, _)| *asked)?;
        Some(format!(
            "ibkr: this account's extended-hours switch (`venue.ibkr.<tier>.extended_hours`) sends a stock limit \
             with includeOvernight, and IB refuses {what} there (paper, 2026-11-09: [201] \"... not supported for \
             this combination of exchange and security type\"); place it without that term or with the switch \
             off; nothing was sent"
        ))
    }

    /// Why an iceberg is not sent: its display size is not a multiple of the contract's lot ([`lot_of`], one bounded
    /// `contract_details` on the exec thread). MEASURED on the paper gateway 2026-11-09: an AAPL iceberg with a display of 1
    /// was ACCEPTED and ended by IB with `[201] Order rejected - reason:Display size should be a multiple of lot size.`
    /// Refused, never rounded: the bridge's convention for a display size it cannot send as asked
    /// (`crate::execution::order::map_order_request` refuses a display that is not a whole number). A read that fails, or details
    /// that state no lot, send the order: IB judges the display then, and its refusal ends the order in its words.
    fn lot_refusal(&self, spec: &IbkrOrderSpec, contract: &Contract) -> Option<String> {
        let shown = spec.display_size?;
        let (session, asked) = (Arc::clone(&self.session), contract.clone());
        let read = bounded("the lot size of an iceberg", DETAILS_READ_WAIT, None, move || {
            session.lot_size(&asked).map_err(IbkrError::DataUnavailable)
        });
        let lot = match read {
            Ok(lot) => lot?,
            Err(e) => {
                tracing::warn!(
                    order_ref = %spec.order_ref,
                    error = %e,
                    "ibkr: the lot size of an iceberg could not be read; it is sent and IB judges its display"
                );
                return None;
            }
        };
        let lots = f64::from(shown) / lot;
        if (lots - lots.round()).abs() <= 1e-9 {
            return None;
        }
        Some(format!(
            "ibkr: the display size {shown} is not a multiple of {}'s round lot {lot} (IB's `suggestedSizeIncrement` \
             in its contract details; a US stock's round lot is 100, 40, 10 or 1 shares by its price since \
             2025-11-03; on the paper gateway 2026-11-09 IB ended an iceberg of display 1 with [201] \"Display size \
             should be a multiple of lot size\"); show a multiple of {lot}; nothing was sent",
            contract.symbol
        ))
    }

    /// The price condition a trailing order with an activation is held behind (`crate::execution::order`'s module doc): the
    /// contract's own id (read with one bounded `contract_details` when the name pins none), its exchange, the
    /// level, crossed UPWARD for a sell and DOWNWARD for a buy (vike's activation: a sell protects a long, so the
    /// price comes up to it), IB's default trigger method, the condition activating the order (ibapi's defaults:
    /// `conditions_cancel_order` false, `conditions_ignore_rth` false). `Err` is the order's rejection, in words.
    fn activation_condition(
        &self,
        level: f64,
        action: &str,
        contract: &Contract,
    ) -> Result<OrderCondition, String> {
        let contract_id = if contract.contract_id != 0 {
            contract.contract_id
        } else {
            let (session, asked) = (Arc::clone(&self.session), contract.clone());
            bounded("the contract id of a price condition", DETAILS_READ_WAIT, None, move || {
                session.contract_id(&asked).map_err(IbkrError::DataUnavailable)
            })
            .map_err(|e| {
                format!(
                    "ibkr: a trailing order's activation is IB's price condition, which names the \
                     contract id, and none was read ({e}); nothing was sent"
                )
            })?
        };
        Ok(OrderCondition::Price(PriceCondition {
            contract_id,
            exchange: contract.exchange.as_str().to_string(),
            price: level,
            trigger_method: TriggerMethod::Default,
            is_more: action == "SELL",
            is_conjunction: true,
        }))
    }

    /// Read `handle` to its end on its own short-lived thread, and queue the answer for the pump to
    /// release behind the rows.
    fn await_end(&self, snapshot: SnapshotRequest, handle: Box<dyn SnapshotHandle>) {
        let markers = Arc::clone(&self.markers);
        let deadline = self.snapshot_deadline;
        let spawned = thread::Builder::new().name("ibkr-snapshot".into()).spawn({
            let markers = Arc::clone(&markers);
            move || {
                let complete = handle.wait(deadline);
                markers.push(snapshot, complete);
            }
        });
        if let Err(e) = spawned {
            tracing::warn!(?snapshot, error = %e, "ibkr: could not start the snapshot waiter");
            markers.push(snapshot, false);
        }
    }
}

/// The master client id (0) is refused for an exec account: see [`IbkrTransportSocket::connect`].
pub(crate) fn refuse_master_client(client_id: i32) -> Result<(), IbkrError> {
    if client_id == 0 {
        return Err(IbkrError::Connect(
            "client id 0 is TWS's master client (TWS binds every order made by hand to it): this \
             account's CLIENT_ID credential field is 0 - set it to 1 or more (leave it unset for the \
             default 1); DATA_CLIENT_ID and RECON_CLIENT_ID default to the next two ids and must stay \
             unique across every account on the gateway"
                .to_string(),
        ));
    }
    Ok(())
}

impl IbkrTransport for IbkrTransportSocket {
    fn place_order(&mut self, order_id: i32, spec: &IbkrOrderSpec, contract: &IbkrContract) {
        if let Some(msg) = self.overnight_refusal(spec, contract) {
            tracing::warn!(order_id, %msg, "ibkr: order refused before submit_order");
            // The synchronous refusal's carrier, as below: a terminal for the bound order client id.
            let _ = self.tx.send(IbkrInbound::Error { code: 201, order_id, msg });
            return;
        }
        let ibkr_contract = build_contract(contract);
        if let Some(msg) = self.lot_refusal(spec, &ibkr_contract) {
            tracing::warn!(order_id, %msg, "ibkr: iceberg refused before submit_order");
            let _ = self.tx.send(IbkrInbound::Error { code: 201, order_id, msg });
            return;
        }
        let mut ibkr_order = self.order_for(spec, contract);
        if let Some(level) = spec.activation {
            match self.activation_condition(level, spec.action, &ibkr_contract) {
                Ok(condition) => ibkr_order.conditions = vec![condition],
                Err(msg) => {
                    tracing::warn!(order_id, %msg, "ibkr: trailing order refused before submit_order");
                    // The synchronous refusal's carrier, as below: a terminal for the bound order client id.
                    let _ = self.tx.send(IbkrInbound::Error { code: 201, order_id, msg });
                    return;
                }
            }
        }
        // Fire-and-forget: updates arrive on the global order_update_stream, not a per-order handle.
        if let Err(e) = self.session.submit_order(order_id, &ibkr_contract, &ibkr_order) {
            tracing::warn!(order_id, error = %e, "ibkr submit_order failed");
            // A refusal before the write, or a write that failed: NOT SENT either way (a failed write put no whole
            // frame on the wire: "What ibapi does"), so synthesize an order-rejection and the bound order client id
            // gets a terminal (code 201 = "order rejected", classified as OrderRejection by the mapper).
            let _ = self.tx.send(IbkrInbound::Error {
                code: 201,
                order_id,
                msg: format!("submit_order failed: {e}"),
            });
        }
    }

    /// Amend by RE-PLACING the order under its own `order_id`: TWS has no amend verb, it reads a
    /// `placeOrder` with an id it already holds as a modification and answers with an `openOrder` row of the
    /// new terms (`execution::amend::PendingAmends` waits for that row). A write failure is the one thing known
    /// here, and it refuses the amend (non-terminal): unlike `place_order` it must NOT be synthesized as the
    /// order's rejection, because the order is still working at its old terms.
    fn modify_order(
        &mut self,
        order_id: i32,
        spec: &IbkrOrderSpec,
        contract: &IbkrContract,
    ) -> AmendOutcome {
        if let Some(msg) = self.overnight_refusal(spec, contract) {
            return AmendOutcome::Refused(msg, vike_model::OrderRefusalId::VenueCapability);
        }
        let ibkr_contract = build_contract(contract);
        let ibkr_order = self.order_for(spec, contract);
        match self.session.submit_order(order_id, &ibkr_contract, &ibkr_order) {
            Ok(()) => AmendOutcome::Sent,
            Err(e) => {
                tracing::warn!(order_id, error = %e, "ibkr amend (re-place under the same id) failed");
                AmendOutcome::Refused(
                    format!("ibkr: the amend could not be written: {e}"),
                    vike_model::OrderRefusalId::NotSent,
                )
            }
        }
    }

    /// The returned per-request subscription is not needed — the resulting `Cancelled` OrderStatus flows through the
    /// global stream. A cancel that could not be written did not take (owner ruling F-5): it is refused in words, as
    /// `modify_order` refuses an amend, and the exec loop answers it with a non-terminal `OrderCancelRejected`. It used
    /// to be `Ok`, so the cancel was reported sent and nothing ever said it was not. NOT SENT (code `0`): TWS acts on a
    /// whole length-prefixed frame only, and a failed write did not put one on the wire (the module doc's "What ibapi
    /// does").
    fn cancel_order(&mut self, order_id: i32) -> Result<(), VenueApiError> {
        // Returned, not logged: the exec loop's `send_cancel` logs it once, with the order client id.
        self.session.cancel_order(order_id).map_err(|e| VenueApiError {
            code: 0,
            msg: format!("ibkr: the cancel could not be written: {e}"),
        })
    }

    /// One bound over the whole read: it holds the exec thread, and `contract_details` waits with no timeout of its own.
    /// One read at a time: while an abandoned one is out this answers at once and writes nothing, so asking again costs a
    /// gateway that does not answer nothing, and a second `positions()` cannot end on the first one's rows (ibapi hands
    /// every live subscription every frame, end marker included).
    fn held_position(&mut self, contract: &IbkrContract) -> Result<f64, String> {
        if self.held_read_out.load(Ordering::SeqCst) > 0 {
            return Err("the last position read has not come back".to_string());
        }
        let out = Settling::hold(&self.held_read_out);
        let (session, asked, account) =
            (Arc::clone(&self.session), build_contract(contract), self.account.clone());
        bounded("the position read of a close that waited", self.held_read_wait, None, move || {
            // Let go before the answer is handed back, so the read behind this one finds it free.
            let _out = out;
            session.held_position(&asked, &account).map_err(IbkrError::DataUnavailable)
        })
        .map_err(|e| e.to_string())
    }

    /// `reqAllOpenOrders`. The rows come back on the global stream (`translate` → `VenueOrder`); the
    /// request's END comes back only on its own subscription, read by a short-lived thread and
    /// released by the pump behind the rows as `SnapshotEnd`. A request that cannot even be written
    /// is reported as an incomplete snapshot, the same way.
    fn request_open_orders(&mut self) {
        match self.session.all_open_orders() {
            Ok(handle) => self.await_end(SnapshotRequest::OpenOrders, handle),
            Err(e) => {
                tracing::warn!(error = %e, "ibkr all_open_orders failed");
                self.markers.push(SnapshotRequest::OpenOrders, false);
            }
        }
    }

    /// `reqExecutions`, scoped to THIS client id: the commission-recovery request of the stranded-fill
    /// sweep (`event_mapper` module doc trap 5) and the second half of the connect-time learning. The
    /// rows (`ExecutionData` + `CommissionReport`) arrive on the global stream; replaying pairs whose
    /// fill already emitted is harmless — `EventMapper`'s `emitted` index drops both halves, and
    /// `vike_execution::Engine`'s `seen_trade_ids` is a second net. Its END is reported like the
    /// open orders' (the exec loop ignores the ones it did not wait for).
    fn request_executions(&mut self) {
        match self.session.executions(self.client_id) {
            Ok(handle) => self.await_end(SnapshotRequest::Executions, handle),
            Err(e) => {
                tracing::warn!(client_id = self.client_id, error = %e, "ibkr executions replay failed");
                self.markers.push(SnapshotRequest::Executions, false);
            }
        }
    }

    fn ownership(&self) -> Option<OwnOrders> {
        Some(OwnOrders { client_id: self.client_id, account: self.account.clone() })
    }

    fn next_recv(&mut self, timeout: Duration) -> Option<IbkrInbound> {
        super::recv_inbound(&self.inbound, timeout)
    }

    fn server_link_broken(&self) -> bool {
        self.server_down.as_ref().is_some_and(|down| down.load(Ordering::Relaxed))
    }
}

/// How long the pump waits for an update when nothing else is pending, and while an end marker is
/// waiting for the stream to drain (short: the marker is released at the first empty poll).
const IDLE_POLL: Duration = Duration::from_millis(250);
const DRAIN_POLL: Duration = Duration::from_millis(20);

/// The background pump: drain the order-update stream, translating each item into an `IbkrInbound` on
/// `tx`, and release the end markers of finished snapshot requests behind the rows (an end marker is
/// released only by a poll that found the stream EMPTY). Exits when the stream ends, on a terminal
/// error (after signalling `StreamDead`), or when the receiver is dropped (transport gone) - which an
/// idle stream never shows, so `stop` (set by `IbkrTransportSocket`'s `Drop`) is checked every iteration.
struct InboundReader {
    updates: Box<dyn Updates>,
    tx: Sender<IbkrInbound>,
    markers: Arc<Markers>,
    own: OwnOrders,
    stop: Arc<AtomicBool>,
    /// Closed when the pump ends, whatever ended it: the gateway frees the client id now, and `ibapi` stops
    /// reconnecting a zombie that would take it back (the daemon rebuilds with a NEW client).
    session: Arc<dyn IbkrApiSession>,
    /// The link watchdog (`link_watch`), when this transport is watched.
    watch: Option<Watch>,
    /// The gateway's settling count: a pump that is closing its client holds it, and the next dial waits.
    settle: Option<SettleCount>,
    /// The hold a dropped transport took for this pump (`IbkrTransportSocket`'s `Drop`), let go once the client is closed.
    closing: Closing,
}

/// The words of a pump that panicked (decision 0302: a dead thread is noticed): the stream is as dead as a reset.
const PUMP_PANICKED: &str = "the order-update pump panicked (a bug; the log has its message)";

impl InboundReader {
    fn run(mut self) {
        // A panic in the pump (a bug in a translation) ends the stream exactly as a terminal error does: the link reads
        // lost and the exec loop latches the venue closed. `AssertUnwindSafe`: only the watch and the lane are touched.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.pump())).is_err() {
            tracing::error!("ibkr: {PUMP_PANICKED} — the venue is DEAD");
            if let Some(watch) = &self.watch {
                watch.ended(PUMP_PANICKED);
            }
            let _ = self.tx.send(IbkrInbound::StreamDead { reason: PUMP_PANICKED.to_string() });
        }
        let _closing = self.settle.as_ref().map(Settling::hold);
        // The stream first: it holds an `Arc` of ibapi's bus; then the client itself (joins its dispatcher).
        drop(self.updates);
        self.session.close();
        *self.closing.lock().unwrap_or_else(PoisonError::into_inner) = ClosingState::Done;
    }

    fn pump(&mut self) {
        while !self.stop.load(Ordering::Acquire) {
            let within = if self.markers.pending() { DRAIN_POLL } else { IDLE_POLL };
            match self.updates.poll(within) {
                Polled::Update(update) => {
                    if let Some(inbound) = translate(*update, &self.own)
                        && self.tx.send(inbound).is_err()
                    {
                        break; // receiver dropped
                    }
                }
                Polled::Notice { code, order_id, message } => {
                    // TWS notice bound to the stream — surface as Error; the mapper drops advisories.
                    if self.tx.send(IbkrInbound::Error { code, order_id, msg: message }).is_err() {
                        break;
                    }
                }
                Polled::Idle => {
                    for (snapshot, complete) in self.markers.take() {
                        if let Some(watch) = self.watch.as_mut() {
                            watch.snapshot(complete, Instant::now());
                        }
                        if self.tx.send(IbkrInbound::SnapshotEnd { snapshot, complete }).is_err() {
                            return;
                        }
                    }
                }
                Polled::Failed(e) => {
                    // TERMINAL transport error (connection reset / shutdown). This backend has no
                    // reconnect, so say exactly that: `StreamDead` latches the venue closed in
                    // `execution::run_execution` and every later submit is refused with a synthetic reject.
                    tracing::error!(error = %e, "ibkr order-update stream terminated — venue is DEAD");
                    if let Some(watch) = &self.watch {
                        watch.ended(&e);
                    }
                    let _ = self.tx.send(IbkrInbound::StreamDead { reason: e });
                    break;
                }
                Polled::Closed => {
                    // Clean end of stream. Also terminal for this transport (nothing re-opens the
                    // subscription), so it degrades the venue exactly like the error arm rather than
                    // leaving the exec loop accepting orders against a pump that has stopped.
                    tracing::error!("ibkr order-update stream closed — venue is DEAD");
                    if let Some(watch) = &self.watch {
                        watch.ended("order-update stream closed");
                    }
                    let _ = self.tx.send(IbkrInbound::StreamDead {
                        reason: "order-update stream closed".to_string(),
                    });
                    break;
                }
            }
            // The link watchdog: the gateway can keep the socket open and the stream quiet while it has no
            // link to IBKR at all. That is as dead as a reset, and it says so the same way.
            if let Some(watch) = self.watch.as_mut()
                && let Some(reason) = watch.step(self.session.connected(), Instant::now())
            {
                tracing::error!(%reason, "ibkr: link watchdog — the venue is DEAD");
                let _ = self.tx.send(IbkrInbound::StreamDead { reason });
                break;
            }
        }
    }
}

/// Translate one ibapi `OrderUpdate` into a normalized `IbkrInbound`. An `OpenOrder` becomes a
/// `VenueOrder` row carrying the identity of its owner and its quantity: whether it is OURS is the exec
/// loop's question ([`IbkrTransport::ownership`]), because `reqAllOpenOrders` answers for every client
/// of the login.
fn translate(update: OrderUpdate, own: &OwnOrders) -> Option<IbkrInbound> {
    match update {
        // `reqAllOpenOrders` makes TWS send an `orderStatus` for EVERY client's open order on every
        // connect, and ids are per API client: another client's order whose id equals one of ours must not
        // resolve to our order client id. A message that names ANOTHER client (non-zero, not ours) is dropped here; 0
        // is also what an undecoded field reads, so it is never taken as proof of a stranger (a manual TWS
        // order cannot collide with our ids anyway: they are 0 or negative).
        OrderUpdate::OrderStatus(s) if s.client_id != 0 && s.client_id != own.client_id => None,
        OrderUpdate::ExecutionData(ed)
            if ed.execution.client_id != 0 && ed.execution.client_id != own.client_id =>
        {
            None
        }
        OrderUpdate::OrderStatus(s) => Some(IbkrInbound::OrderStatus(IbkrOrderStatus {
            order_id: s.order_id,
            // ibapi's OrderStatus carries no orderRef; resolve by numeric order_id (bound at submit).
            order_ref: String::new(),
            status: s.status.as_str().to_string(),
            filled: s.filled,
            average_fill_price: s.average_fill_price.unwrap_or(0.0),
            client_id: s.client_id,
        })),
        OrderUpdate::ExecutionData(ed) => {
            // Reconstruct the canonical `SYMBOL.EXCHANGE.CURRENCY` from the exec contract.
            let symbol = format!(
                "{}.{}.{}",
                ed.contract.symbol.0, ed.contract.exchange.0, ed.contract.currency.0
            );
            Some(IbkrInbound::ExecutionDetails(IbkrExecutionDetails {
                order_id: ed.execution.order_id,
                order_ref: ed.execution.order_reference,
                exec_id: ed.execution.execution_id,
                symbol,
                side_buy: matches!(ed.execution.side, ExecutionSide::Bought),
                shares: ed.execution.shares,
                price: ed.execution.price,
                // Receive-time epoch-millis (see module docs — exchange fill-time parse deferred).
                ts: vike_model::now_ms(),
                client_id: ed.execution.client_id,
                recoverable: false,
            }))
        }
        OrderUpdate::CommissionReport(c) => Some(IbkrInbound::Commission(IbkrCommissionReport {
            exec_id: c.execution_id,
            commission: c.commission,
            currency: c.currency,
        })),
        OrderUpdate::OpenOrder(od) => Some(IbkrInbound::VenueOrder(IbkrOpenOrder {
            order_id: od.order_id,
            order_ref: od.order.order_ref,
            client_id: od.order.client_id,
            account: od.order.account,
            total_qty: od.order.total_quantity,
            status: od.order_state.status.as_str().to_string(),
            filled_qty: od.order.filled_quantity,
            // IB echoes the unset `lmtPrice` of a kind with no limit as 0 (an `MIT` amend reported `Some(0.0)`).
            limit_price: od.order.limit_price.filter(|_| !has_no_limit(&od.order.order_type)),
            aux_price: od.order.aux_price,
        })),
        // ibapi 4: TWS binds an order's permanent id to this client's order id. Everything here is
        // keyed on the order client id bound at submit (`order_id` / the order client id in `order_ref`), so the
        // permId changes nothing — dropped, like an advisory the mapper would ignore.
        OrderUpdate::OrderBound(_) => None,
    }
}

/// The IB order types this bridge sends with no `lmtPrice` (`crate::execution::order::map_order_request`; a `TRAIL LIMIT`'s and a
/// `TRAIL LIT`'s limit is the venue's, off its offset, and an `MTL`'s is its first fill's, so they keep what IB echoes).
fn has_no_limit(order_type: &str) -> bool {
    matches!(
        order_type,
        "MKT"
            | "MOC"
            | "STP"
            | "MIT"
            | "TRAIL"
            | "TRAIL MIT"
            | "PEG BEST"
            | "PEG MID"
            | "PEG MKT"
            | "REL"
    )
}

// ---- the real session ----------------------------------------------------------------------------

/// The real [`IbkrApiSession`]: the blocking ibapi client, and the notice stream bound before its connect (taken once).
struct IbkrApiSessionClient(Arc<Client>, Mutex<Option<NoticeStream>>);

/// The client's notices, drained without blocking.
struct NoticesSocket(NoticeStream);

impl Notices for NoticesSocket {
    fn next(&mut self) -> Option<(i32, String)> {
        self.0.try_next().map(|n| (n.code, n.message))
    }
}

impl IbkrApiSessionClient {
    fn new(client: Client, notices: NoticeStream) -> Self {
        Self(Arc::new(client), Mutex::new(Some(notices)))
    }
}

impl IbkrApiSession for IbkrApiSessionClient {
    fn notices(&self) -> Option<Box<dyn Notices>> {
        let taken = self.1.lock().unwrap_or_else(PoisonError::into_inner).take();
        taken.map(|n| Box::new(NoticesSocket(n)) as Box<dyn Notices>)
    }

    fn connected(&self) -> bool {
        self.0.is_connected()
    }

    fn close(&self) {
        self.0.disconnect();
    }

    fn local_next_order_id(&self) -> i32 {
        self.0.next_order_id()
    }

    fn next_valid_order_id(&self) -> Result<i32, String> {
        self.0.next_valid_order_id().map_err(|e| e.to_string())
    }

    fn submit_order(
        &self,
        order_id: i32,
        contract: &Contract,
        order: &Order,
    ) -> Result<(), String> {
        self.0.submit_order(order_id, contract, order).map_err(|e| e.to_string())
    }

    fn cancel_order(&self, order_id: i32) -> Result<(), String> {
        self.0.cancel_order(order_id, "").map(drop).map_err(|e| e.to_string())
    }

    fn all_open_orders(&self) -> Result<Box<dyn SnapshotHandle>, String> {
        let subscription = self.0.all_open_orders().map_err(|e| e.to_string())?;
        Ok(self.snapshot(move |step| classify(subscription.next_timeout(step))))
    }

    fn executions(&self, client_id: i32) -> Result<Box<dyn SnapshotHandle>, String> {
        let filter = ExecutionFilter { client_id: Some(client_id), ..Default::default() };
        let subscription = self.0.executions(filter).map_err(|e| e.to_string())?;
        Ok(self.snapshot(move |step| classify(subscription.next_timeout(step))))
    }

    fn order_updates(&self) -> Result<Box<dyn Updates>, String> {
        let stream = self.0.order_update_stream().map_err(|e| e.to_string())?;
        Ok(Box::new(UpdatesSocket(stream)))
    }

    // The parsers are `crate::reconcile::socket`'s, which only the two-backend build compiles.
    #[cfg(feature = "ibkr")]
    fn held_position(&self, contract: &Contract, account: &str) -> Result<f64, String> {
        crate::reconcile::socket::held_on(&self.0, contract, account, HELD_READ_WAIT)
    }

    #[cfg(feature = "ibkr")]
    fn contract_id(&self, contract: &Contract) -> Result<i32, String> {
        let details =
            self.0.contract_details(contract).map_err(|e| format!("ibkr contract details: {e}"))?;
        crate::reconcile::socket::resolve_conid(&details, &contract.symbol.to_string())
    }

    fn lot_size(&self, contract: &Contract) -> Result<Option<f64>, String> {
        let details =
            self.0.contract_details(contract).map_err(|e| format!("ibkr contract details: {e}"))?;
        Ok(lot_of(details.iter().map(|d| d.suggested_size_increment)))
    }
}

impl IbkrApiSessionClient {
    /// A snapshot handle over `read`, which also asks THIS client whether it is still connected: ibapi
    /// answers a read of a subscription whose channel was torn down exactly like one that ended, so an
    /// "end" is only believed while the session is up (review 18, R6).
    fn snapshot(
        &self,
        read: impl FnMut(Duration) -> Wait + Send + 'static,
    ) -> Box<dyn SnapshotHandle> {
        let client = Arc::clone(&self.0);
        Box::new(SnapshotRead { read, connected: Arc::new(move || client.is_connected()) })
    }
}

struct UpdatesSocket(Subscription<OrderUpdate>);

impl Updates for UpdatesSocket {
    fn poll(&mut self, within: Duration) -> Polled {
        let began = Instant::now();
        match self.0.next_timeout(within) {
            Some(Ok(SubscriptionItem::Data(update))) => Polled::Update(Box::new(update)),
            Some(Ok(SubscriptionItem::Notice(n))) => polled_notice(n),
            Some(Err(e)) => Polled::Failed(e.to_string()),
            // `None` is a timeout OR the end of the stream, and ibapi does not say which: a stream
            // that ended answers at once.
            None => {
                if ended_early(began, within) {
                    Polled::Closed
                } else {
                    Polled::Idle
                }
            }
        }
    }
}

/// Whether a `next_timeout(within)` that returned `None` returned before its timeout: the library
/// answers a subscription that has ENDED immediately, so that is how an end is told from a quiet wait.
fn ended_early(began: Instant, within: Duration) -> bool {
    began.elapsed() + Duration::from_millis(5) < within
}

/// One notice of the order-update stream as the pump reads it: the order its `request_id` names (0 when it names
/// none) and IB's own words, code included (`Notice`'s `[code] text`), so a refusal reaches the order as IB said it.
pub(crate) fn polled_notice(n: ibapi::Notice) -> Polled {
    Polled::Notice { code: n.code, order_id: n.request_id.unwrap_or(0), message: n.to_string() }
}

/// One snapshot request in flight: `read(step)` is one bounded read of its subscription (the glue of
/// `Subscription<Orders>` / `Subscription<Executions>`), `connected` asks the session whether it is up.
struct SnapshotRead<R: FnMut(Duration) -> Wait + Send> {
    read: R,
    connected: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl<R: FnMut(Duration) -> Wait + Send> SnapshotHandle for SnapshotRead<R> {
    fn wait(mut self: Box<Self>, within: Duration) -> bool {
        wait_for_end(within, &mut self.read, &*self.connected)
    }
}

/// One read of a snapshot subscription, by what it found. `None` is a timeout, the END of the request, or a
/// channel torn down: [`wait_for_end`] tells them apart.
fn classify<I, E>(item: Option<Result<I, E>>) -> Wait {
    match item {
        Some(Ok(_)) => Wait::Row,
        Some(Err(_)) => Wait::Failed,
        None => Wait::NoneYet,
    }
}

/// What one bounded read of a snapshot subscription found.
enum Wait {
    /// A row or a notice: the request is still being answered.
    Row,
    /// The subscription failed (a reset, a TWS error).
    Failed,
    /// Nothing: a timeout, or the end (told apart by [`wait_for_end`] by how long the read took).
    NoneYet,
}

/// Read a snapshot subscription until it ends, fails, or `within` passes. `read(step)` is one bounded
/// read of at most `step`.
fn wait_for_end(
    within: Duration,
    mut read: impl FnMut(Duration) -> Wait,
    connected: impl Fn() -> bool,
) -> bool {
    const STEP: Duration = Duration::from_millis(250);
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        let step = left.min(STEP);
        let began = Instant::now();
        match read(step) {
            Wait::Row => {}
            Wait::Failed => return false,
            Wait::NoneYet => {
                if ended_early(began, step) {
                    // The library answers a subscription whose channel was torn down exactly like one
                    // that ENDED: only an end seen while the session is still up is an end.
                    return connected();
                }
            }
        }
    }
}

/// Build an `ibapi::Contract` from the vike `IbkrContract`.
pub(crate) fn build_contract(c: &IbkrContract) -> Contract {
    Contract {
        symbol: Symbol::from(c.symbol.as_str()),
        security_type: sec_type(&c.sec_type),
        exchange: Exchange::from(c.exchange.as_str()),
        currency: Currency::from(c.currency.as_str()),
        last_trade_date_or_contract_month: c.expiry.clone().unwrap_or_default(),
        strike: c.strike.unwrap_or(0.0),
        right: c.right.map(option_right),
        multiplier: c.multiplier.clone().unwrap_or_default(),
        contract_id: c.con_id.unwrap_or(0) as i32,
        ..Contract::default()
    }
}

/// Build an `ibapi::Order` from the pure `IbkrOrderSpec`. `transmit` defaults to `true` (ibapi's
/// `Order::default`), so the order is live on submit. `account` is the configured `config.account`,
/// stamped on the order whenever it is set: on a login that manages several accounts an order
/// without one goes to the login's default account, and the reconcile client
/// (`crate::reconcile::socket`) reads only `config.account`, so exec and reconcile would see two accounts.
/// `extended_hours` is the caller's verdict ([`IbkrTransportSocket::order_for`]); only then are `outside_rth` and
/// `include_overnight` set, and with it `false` the order is the one this function always built (`ibapi`
/// defaults both flags to `false`). **A flagged GTC goes out as DAY**: IBKR answers `includeOvernight` with GTC by
/// error 10052 "Invalid time in force:GTC" (observed on the paper gateway, 5 of 5, 2026-10-25), and its overnight
/// session takes DAY limit orders. An IOC or FOK is flagged `outsideRth` alone, for the same reason: see
/// [`extended_hours_flags`], the table. The order type and its prices are the spec's as built
/// (`MIT`/`LIT`/`MTL`/`MOC`/`LOC`/`TRAIL`/`TRAIL MIT`/`PEG MID` included, `crate::execution::order::map_order_request`); an iceberg's `displaySize`, a trailing code's `lmtPriceOffset` and
/// `trailingPercent`, the attributes (`hidden`, `allOrNone`, `sweepToFill`, `discretionaryAmt`), the release's
/// `goodAfterTime`, a GTD's `goodTillDate` and the adjustable stop's `triggerPrice` + `adjusted*` fields are set only
/// when the spec carries one, so every other order keeps `Order::default`'s (a trailing order's price condition is
/// `IbkrTransportSocket::place_order`'s).
fn build_order(spec: &IbkrOrderSpec, account: &str, extended_hours: bool) -> Order {
    // The wire time in force and the two flags come out of ONE table when the switch is on, and are the
    // order's own (no flag) when it is off: the byte-identical order the bridge sent before the switch.
    let (wire_time_in_force, outside_rth, include_overnight) =
        if extended_hours { extended_hours_flags(spec.tif) } else { (tif(spec.tif), false, false) };
    let mut order = Order {
        action: if spec.action == "BUY" { Action::Buy } else { Action::Sell },
        total_quantity: spec.total_qty,
        order_type: spec.order_type.to_string(),
        limit_price: spec.lmt_price,
        aux_price: spec.aux_price,
        limit_price_offset: spec.lmt_price_offset, // `Order::default`'s `None` on every order but a TRAIL LIMIT
        tif: wire_time_in_force,
        order_ref: spec.order_ref.clone(),
        account: account.trim().to_string(),
        outside_rth,
        include_overnight,
        // Each of these is `Order::default`'s value when the spec states none.
        hidden: spec.hidden,
        all_or_none: spec.all_or_none,
        sweep_to_fill: spec.sweep_to_fill,
        discretionary_amt: spec.discretionary_amt.unwrap_or_default(),
        trailing_percent: spec.trailing_percent,
        good_after_time: spec.good_after_time.clone().unwrap_or_default(),
        good_till_date: spec.good_till_date.clone().unwrap_or_default(),
        ..Order::default()
    };
    if spec.display_size.is_some() {
        order.display_size = spec.display_size;
    }
    if let Some(adjust) = spec.adjust {
        order.trigger_price = Some(adjust.trigger_price);
        order.adjusted_order_type = adjust.order_type.to_string();
        order.adjusted_stop_price = Some(adjust.stop_price);
        order.adjusted_stop_limit_price = adjust.stop_limit_price;
        order.adjusted_trailing_amount = adjust.trailing_amount;
        order.adjustable_trailing_unit = adjust.trailing_unit;
    }
    order
}

/// **The time-in-force x extended-hours table**: what a FLAGGED stock limit order (the `extended_hours`
/// switch is on) sends for the time in force it was given, as `(wire time in force, outsideRth,
/// includeOvernight)`. One switch per tier stays the operator's control; the TIF decides which of the
/// two flags IBKR will take with it (measured on the paper gateway, 2026-10-25 and 2026-10-28; the rows
/// and codes are in `docs/ops/ibkr-socket-gateway.md`'s "Extended hours and the overnight session"):
///
/// | asked | sent | outsideRth | includeOvernight | IBKR |
/// |---|---|---|---|---|
/// | `GTC` | `DAY` | yes | yes | accepted (the default order: GTC with `includeOvernight` is refused, 10052) |
/// | `DAY` | `DAY` | yes | yes | accepted (warning 399 outside the session; ends with it) |
/// | `IOC` | `IOC` | yes | **no** | `includeOvernight` + `IOC` is refused 10052; `outsideRth` alone is accepted |
/// | `FOK` | `FOK` | yes | **no** | `includeOvernight` + `FOK` is refused 10052; `outsideRth` alone is accepted or refused 201 by the venue |
/// | `GTD` | `GTD` | yes | **no** | accepted, `goodTillDate` read back in US/Eastern (paper 2026-11-10, the w2 auction smoke's GTD row) |
///
/// So `includeOvernight` rides `DAY` only, and an `IOC`/`FOK` order is flagged for the pre-market and
/// after-hours sessions but NOT the overnight one (where it is accepted and ended by IBKR without a fill,
/// measured). Any other word (a post-only order is refused before the transport; a limit-on-open is not
/// flagged) takes `outsideRth` alone, the same rule: nothing but `DAY` is ever sent with `includeOvernight`.
fn extended_hours_flags(wire: &str) -> (IbTif, bool, bool) {
    match wire {
        "GTC" | "DAY" => (IbTif::Day, true, true),
        other => (tif(other), true, false),
    }
}

fn sec_type(s: &SecType) -> SecurityType {
    match s {
        SecType::Stk => SecurityType::Stock,
        SecType::Opt => SecurityType::Option,
        SecType::Fut => SecurityType::Future,
        SecType::Cash => SecurityType::ForexPair,
        SecType::Ind => SecurityType::Index,
        SecType::Crypto => SecurityType::Crypto,
        SecType::Other(code) => SecurityType::Other(code.clone()),
    }
}

fn option_right(r: OptionKind) -> OptionRight {
    match r {
        OptionKind::Call => OptionRight::Call,
        OptionKind::Put => OptionRight::Put,
    }
}

fn tif(s: &str) -> IbTif {
    match s {
        "GTC" => IbTif::GoodTillCanceled,
        "IOC" => IbTif::ImmediateOrCancel,
        "FOK" => IbTif::FillOrKill,
        "GTD" => IbTif::GoodTillDate,
        "OPG" => IbTif::OnOpen,
        "AUC" => IbTif::Auction,
        _ => IbTif::Day,
    }
}

#[cfg(test)]
mod tests {
    use super::{build_contract, build_order};
    use crate::instrument::contract::parse_simplified;
    use ibapi::contracts::SecurityType;
    use std::assert_matches;

    /// THE WIRE ASSERTION, made on the CONSTRUCTED `ibapi::Contract` rather than on any string:
    /// this is the exact value `IbkrTransportSocket::place_order` hands the `ibapi` client, so what it
    /// says here is what IBKR is asked to trade.
    ///
    /// ⚠ Before 2026-09-16 the first case below produced `SecurityType::Stock` with an EMPTY
    /// `last_trade_date_or_contract_month`, because `parse_simplified` defaulted every
    /// unrecognised canonical to equity. A real E-mini order went out as a stock order.
    #[test]
    fn a_futures_canonical_reaches_the_wire_as_a_futures_contract() {
        let c = build_contract(&parse_simplified("ES.GLOBEX.USD.FUT.20251219").unwrap());
        assert_eq!(c.security_type, SecurityType::Future);
        assert_eq!(c.symbol.as_str(), "ES");
        assert_eq!(c.exchange.as_str(), "GLOBEX");
        assert_eq!(c.currency.as_str(), "USD");
        assert_eq!(c.last_trade_date_or_contract_month, "20251219");
        assert_eq!(c.multiplier, "", "no multiplier claimed → IBKR resolves the listed one");

        let m = build_contract(&parse_simplified("ES.GLOBEX.USD.FUT.202512.50").unwrap());
        assert_eq!(m.last_trade_date_or_contract_month, "202512");
        assert_eq!(m.multiplier, "50");
    }

    #[test]
    fn a_claimed_option_reaches_the_wire_with_its_strike_and_right() {
        let c = build_contract(&parse_simplified("SPY.SMART.USD.OPT.20251219.500.C").unwrap());
        assert_eq!(c.security_type, SecurityType::Option);
        assert_eq!(c.last_trade_date_or_contract_month, "20251219");
        assert_eq!(c.strike, 500.0);
        assert_matches!(c.right, Some(ibapi::contracts::OptionRight::Call));
    }

    /// The equity and forex canonicals this tree actually starts, unchanged on the wire.
    #[test]
    fn the_equity_and_forex_canonicals_are_unchanged_on_the_wire() {
        let a = build_contract(&parse_simplified("AAPL.SMART.USD").unwrap());
        assert_eq!(a.security_type, SecurityType::Stock);
        assert_eq!(a.symbol.as_str(), "AAPL");
        assert_eq!(a.exchange.as_str(), "SMART");
        assert_eq!(a.currency.as_str(), "USD");
        assert_eq!(a.last_trade_date_or_contract_month, "");
        assert_eq!(a.strike, 0.0);
        assert!(a.right.is_none());
        assert_eq!(a.multiplier, "");
        assert_eq!(a.contract_id, 0);

        let f = build_contract(&parse_simplified("EUR.USD.IDEALPRO").unwrap());
        assert_eq!(f.security_type, SecurityType::ForexPair);
        assert_eq!(f.symbol.as_str(), "EUR");
        assert_eq!(f.exchange.as_str(), "IDEALPRO");
        assert_eq!(f.currency.as_str(), "USD");
    }

    /// **The order carries the configured account** (decision 0140 safety review): on a login that
    /// manages several accounts, an order with no account goes to the login's default one while
    /// reconcile reads `config.account` only. The built order — what `place_order` hands ibapi — names
    /// it, trimmed; an unset account leaves it blank (the login's default, as before).
    #[test]
    fn the_built_order_carries_the_configured_account() {
        let spec = crate::execution::order::IbkrOrderSpec {
            action: "BUY",
            total_qty: 1.0,
            order_type: "LMT",
            lmt_price: Some(1.0),
            aux_price: None,
            tif: "DAY",
            order_ref: "order-client-id-1".to_string(),
            display_size: None,
            lmt_price_offset: None,
            activation: None,
            ..Default::default()
        };
        assert_eq!(build_order(&spec, "DU1234567", false).account, "DU1234567");
        assert_eq!(build_order(&spec, " DU1234567 ", false).account, "DU1234567");
        assert_eq!(build_order(&spec, "", false).account, "");
        assert_eq!(build_order(&spec, "DU1234567", false).order_ref, "order-client-id-1");
    }

    /// **A stop-limit reaches the wire as `STP LMT`** with the limit as `lmtPrice` and the trigger as `auxPrice`
    /// (TWS's `Order` fields, which `place_order` hands ibapi as built here).
    #[test]
    fn a_stop_limit_is_built_with_its_limit_and_its_trigger() {
        let spec = crate::execution::order::IbkrOrderSpec {
            action: "SELL",
            total_qty: 2.0,
            order_type: "STP LMT",
            lmt_price: Some(179.5),
            aux_price: Some(180.0),
            tif: "GTC",
            order_ref: "order-client-id-sl".to_string(),
            display_size: None,
            lmt_price_offset: None,
            activation: None,
            ..Default::default()
        };
        let order = build_order(&spec, "DU1234567", false);
        assert_eq!(order.order_type, "STP LMT");
        assert_eq!(order.limit_price, Some(179.5), "lmtPrice is the limit");
        assert_eq!(order.aux_price, Some(180.0), "auxPrice is the trigger");
        assert_eq!(order.tif, ibapi::orders::TimeInForce::GoodTillCanceled);
        assert!(!order.outside_rth && !order.include_overnight);
        assert_eq!(order.display_size, ibapi::orders::Order::default().display_size);
    }

    /// **The wave-2 kinds reach the wire as `map_order_request` built them**: a real request through the mapper and
    /// `build_order` (what `place_order` hands ibapi) gives `MIT`/`LIT` with the trigger as `auxPrice`, `MTL`
    /// unpriced, a market-on-open as `MKT` with TIF `OPG` (ibapi's `OnOpen`), a limit-on-close as `LOC` sent DAY,
    /// and an iceberg's `displaySize`.
    #[test]
    fn the_wave_two_kinds_are_built_as_mapped() {
        use ibapi::orders::TimeInForce as IbapiTimeInForce;
        use vike_model::{OrderRequest, OrderType, TimeInForce};
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let built = |r: OrderRequest| {
            build_order(
                &crate::execution::order::map_order_request(&r, &c, 1).unwrap(),
                "DU1234567",
                false,
            )
        };
        let base = OrderRequest {
            order_client_id: "w2".into(),
            symbol: "AAPL.SMART.USD".into(),
            side: 1,
            qty: 5.0,
            ..Default::default()
        };
        let mit = built(OrderRequest {
            order_type: OrderType::TakeProfit,
            trigger_price: Some(180.0),
            ..base.clone()
        });
        assert_eq!(
            (mit.order_type.as_str(), mit.limit_price, mit.aux_price),
            ("MIT", None, Some(180.0))
        );
        let lit = built(OrderRequest {
            order_type: OrderType::TakeProfit,
            price: Some(181.0),
            trigger_price: Some(180.0),
            ..base.clone()
        });
        assert_eq!(
            (lit.order_type.as_str(), lit.limit_price, lit.aux_price),
            ("LIT", Some(181.0), Some(180.0))
        );
        let mtl = built(OrderRequest { order_type: OrderType::MarketToLimit, ..base.clone() });
        assert_eq!((mtl.order_type.as_str(), mtl.limit_price, mtl.aux_price), ("MTL", None, None));
        let moo = built(OrderRequest { time_in_force: TimeInForce::AtOpen, ..base.clone() });
        assert_eq!((moo.order_type.as_str(), &moo.tif), ("MKT", &IbapiTimeInForce::OnOpen));
        let loc = built(OrderRequest {
            order_type: OrderType::Limit,
            price: Some(150.0),
            time_in_force: TimeInForce::AtClose,
            ..base.clone()
        });
        assert_eq!(
            (loc.order_type.as_str(), loc.limit_price, &loc.tif),
            ("LOC", Some(150.0), &IbapiTimeInForce::Day)
        );
        let iceberg = built(OrderRequest {
            order_type: OrderType::Limit,
            price: Some(150.0),
            display_quantity: Some(1.0),
            ..base
        });
        assert_eq!((iceberg.order_type.as_str(), iceberg.display_size), ("LMT", Some(1)));
    }

    /// **The trailing kind reaches the wire as mapped**: a `trailing` is `TRAIL` with the trail as `auxPrice` and nothing
    /// else priced, no `trailStopPrice` and no condition (a `trailing_limit` and an activation are refused by the mapper;
    /// the price condition an activation would be is `place_order`'s, `socket_tests.rs`).
    #[test]
    fn the_trailing_kind_is_built_as_mapped() {
        use vike_model::{OrderRequest, OrderType};
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let r = OrderRequest {
            order_client_id: "w3".into(),
            symbol: "AAPL.SMART.USD".into(),
            side: -1,
            qty: 5.0,
            trail: Some(2.5),
            order_type: OrderType::Trailing,
            ..Default::default()
        };
        let trail = build_order(
            &crate::execution::order::map_order_request(&r, &c, 1).unwrap(),
            "DU1234567",
            false,
        );
        assert_eq!(
            (
                trail.order_type.as_str(),
                trail.limit_price,
                trail.aux_price,
                trail.limit_price_offset
            ),
            ("TRAIL", None, Some(2.5), None)
        );
        assert_eq!(trail.trail_stop_price, None);
        assert_eq!(trail.trailing_percent, None);
        assert!(trail.conditions.is_empty());
        assert_eq!(trail.tif, ibapi::orders::TimeInForce::GoodTillCanceled);
    }

    /// **The mid peg reaches the wire as mapped**: `PEG MID` carries the offset (IB's sign) as `auxPrice` and nothing else,
    /// no `lmtPrice`, not `notHeld` (the other anchors are refused by the mapper).
    #[test]
    fn the_mid_peg_is_built_as_mapped() {
        use vike_model::{OrderPeg, OrderPegAnchor, OrderRequest, OrderType};
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let r = OrderRequest {
            order_client_id: "w5".into(),
            symbol: "AAPL.SMART.USD".into(),
            side: 1,
            qty: 5.0,
            order_type: OrderType::Pegged,
            peg: Some(OrderPeg { anchor: OrderPegAnchor::Mid, offset: 0.02 }),
            ..Default::default()
        };
        let order = build_order(
            &crate::execution::order::map_order_request(&r, &c, 1).unwrap(),
            "DU1234567",
            false,
        );
        assert_eq!(
            (order.order_type.as_str(), order.limit_price, order.aux_price),
            ("PEG MID", None, Some(-0.02))
        );
        assert_eq!((order.compete_against_best_offset, order.not_held), (None, false));
    }

    /// **The attributes wave reaches the wire as mapped** (a real request through the mapper and `build_order`, what
    /// `place_order` hands ibapi): `hidden`, `allOrNone`, `sweepToFill`, `discretionaryAmt` on the `LMT`; `TRAIL MIT`
    /// with the trail as `auxPrice` (a `TRAIL LIT` is refused by the mapper); a percent trail as `trailingPercent`
    /// with no `auxPrice`; the adjustable stop's `triggerPrice` and `adjusted*` fields on the `STP`; an opening `MTL` with
    /// TIF `AUC`; a time release as `goodAfterTime`. A plain limit keeps every one of them at `Order::default`'s.
    #[test]
    fn the_attributes_wave_is_built_as_mapped() {
        use ibapi::orders::TimeInForce as IbapiTimeInForce;
        use vike_model::{
            OrderAttributes, OrderRelease, OrderRequest, OrderStopAdjust, OrderStopAdjustTo,
            OrderType, TimeInForce,
        };
        let c = parse_simplified("AAPL.SMART.USD").unwrap();
        let built = |r: OrderRequest| {
            build_order(
                &crate::execution::order::map_order_request(&r, &c, 1).unwrap(),
                "DU1234567",
                false,
            )
        };
        let base = OrderRequest {
            order_client_id: "at".into(),
            symbol: "AAPL.SMART.USD".into(),
            side: 1,
            qty: 5.0,
            order_type: OrderType::Limit,
            price: Some(150.0),
            ..Default::default()
        };
        let plain = built(base.clone());
        let default = ibapi::orders::Order::default();
        assert_eq!(
            (plain.hidden, plain.all_or_none, plain.sweep_to_fill, plain.discretionary_amt),
            (default.hidden, default.all_or_none, default.sweep_to_fill, default.discretionary_amt)
        );
        assert_eq!(
            (&plain.good_after_time, &plain.good_till_date, plain.trailing_percent),
            (&default.good_after_time, &default.good_till_date, default.trailing_percent)
        );
        assert_eq!(
            (&plain.adjusted_order_type, plain.trigger_price, plain.adjustable_trailing_unit),
            (&default.adjusted_order_type, default.trigger_price, default.adjustable_trailing_unit)
        );

        let attributed = built(OrderRequest {
            display_quantity: Some(0.0),
            attributes: Some(OrderAttributes {
                all_or_none: true,
                sweep_to_fill: true,
                discretionary_amount: Some(0.5),
            }),
            ..base.clone()
        });
        assert_eq!(
            (
                attributed.order_type.as_str(),
                attributed.hidden,
                attributed.display_size,
                attributed.all_or_none,
                attributed.sweep_to_fill,
                attributed.discretionary_amt
            ),
            ("LMT", true, default.display_size, true, true, 0.5),
            "a hidden order keeps `Order::default`'s display size"
        );

        let trail = OrderRequest {
            order_type: OrderType::TrailingTakeProfit,
            price: None,
            trail: Some(2.5),
            ..base.clone()
        };
        let mit = built(trail.clone());
        assert_eq!(
            (mit.order_type.as_str(), mit.limit_price, mit.aux_price, mit.limit_price_offset),
            ("TRAIL MIT", None, Some(2.5), None)
        );
        let pct = built(OrderRequest { trail: None, trail_percent: Some(1.5), ..trail });
        assert_eq!(
            (pct.order_type.as_str(), pct.aux_price, pct.trailing_percent),
            ("TRAIL MIT", None, Some(1.5))
        );
        for order in [&mit, &pct] {
            assert_eq!(order.trail_stop_price, None);
            assert!(order.conditions.is_empty());
        }

        let adjustable = built(OrderRequest {
            side: -1,
            order_type: OrderType::Stop,
            price: None,
            trigger_price: Some(180.0),
            adjust: Some(OrderStopAdjust {
                at: 200.0,
                to: OrderStopAdjustTo::Trailing {
                    stop: 195.0,
                    trail: None,
                    trail_percent: Some(1.0),
                },
            }),
            ..base.clone()
        });
        assert_eq!(
            (adjustable.order_type.as_str(), adjustable.aux_price, adjustable.trigger_price),
            ("STP", Some(180.0), Some(200.0))
        );
        assert_eq!(
            (
                adjustable.adjusted_order_type.as_str(),
                adjustable.adjusted_stop_price,
                adjustable.adjusted_stop_limit_price,
                adjustable.adjusted_trailing_amount,
                adjustable.adjustable_trailing_unit
            ),
            ("TRAIL", Some(195.0), None, Some(1.0), 100)
        );

        let auction = built(OrderRequest {
            order_type: OrderType::MarketToLimit,
            price: None,
            time_in_force: TimeInForce::AtOpen,
            ..base.clone()
        });
        assert_eq!(
            (auction.order_type.as_str(), auction.limit_price, &auction.tif),
            ("MTL", None, &IbapiTimeInForce::Auction)
        );

        let released = built(OrderRequest {
            release: Some(OrderRelease { at: 1_794_234_600_000, price: None }),
            ..base
        });
        assert_eq!(
            (released.good_after_time.as_str(), &released.tif),
            ("20261109-14:30:00", &IbapiTimeInForce::GoodTillCanceled)
        );
    }

    /// **A GTD order carries its date** (`goodTillDate`, `crate::execution::order::ibkr_utc`'s spelling) beside TIF `GTD`; `tif`
    /// reads the spec's word, so this holds as soon as the `venue_time_in_force` row maps `GTD`.
    #[test]
    fn a_good_till_date_rides_the_gtd_order() {
        let spec = crate::execution::order::IbkrOrderSpec {
            action: "BUY",
            total_qty: 1.0,
            order_type: "LMT",
            lmt_price: Some(150.0),
            tif: "GTD",
            order_ref: "gtd".into(),
            good_till_date: Some(crate::execution::order::ibkr_utc(1_794_234_600_000)),
            ..Default::default()
        };
        let order = build_order(&spec, "DU1234567", false);
        assert_eq!(order.tif, ibapi::orders::TimeInForce::GoodTillDate);
        assert_eq!(order.good_till_date, "20261109-14:30:00");
    }
}

#[path = "socket_tests.rs"]
#[cfg(test)]
pub(crate) mod socket_tests;

#[path = "socket_watch_tests.rs"]
#[cfg(test)]
mod socket_watch_tests;
