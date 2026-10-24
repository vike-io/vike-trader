//! The REAL socket backend: `SocketTransport` wraps the blocking `ibapi` client
//! (`ibapi::client::blocking::Client`, the registry crate `ibapi` =5.1.0, sync-only) and
//! implements the `IbkrTransport` seam. Thin glue ONLY — every order-correctness decision lives
//! in the pure modules (`event_mapper`/`order`/`contract`/`id_registry`); this file just marshals
//! vike types ⇄ ibapi types and pumps ibapi's global order-update stream onto an internal channel
//! that the exec loop drains via `next_recv`.
//!
//! The ibapi client is reached through the small [`IbSession`] seam (the real one is
//! [`ClientSession`]), so the connect sequence ([`SocketTransport::start_watched`]), the pump and the snapshot
//! bookkeeping are exercised by unit tests over a double: the production call site and the seed it
//! queues are tested, not a copy of them. What no test can reach is ibapi itself and the gateway behind it:
//! the runtime validation is the paper smoke against a live IB Gateway
//! (`docs/ops/ibkr-socket-gateway.md` is the bring-up).
//!
//! ⚠ That smoke has now RUN, on the CI box 2026-08-19, against a real IB Gateway — submit → accept →
//! cancel on the paper account, with the coid intact on both terminal events. Until then this file
//! had been compiled and reviewed but never spoken to a gateway, so every mapping decision below
//! was an argument from `ibapi`'s published source rather than an observation. ⚠ **The connect-time
//! learning below (`reqAllOpenOrders` + `reqExecutions` + their end markers) has NOT yet run against
//! a real gateway**; the ibapi behaviour it relies on was read from the 4.2.0 source and re-read against 5.1.0's (see
//! "What ibapi does") and the first attended run is the IBKR rollout step.
//!
//! ## Mapping decisions (verified against `ibapi`'s source, 4.2.0 and 5.1.0, not the research notes)
//! - Placement uses `Client::submit_order` (fire-and-forget) + a single global
//!   `Client::order_update_stream()` as the event pump — the ibapi-documented pairing. The stream
//!   yields `OrderUpdate::{OrderStatus, ExecutionData, CommissionReport, OpenOrder, OrderBound}` for
//!   ALL orders, which we translate into `IbInbound`.
//! - ibapi's `OrderStatus` has NO `orderRef` field, so status inbound resolves by numeric `order_id`
//!   (the exec loop binds `order_id → coid` at submit; dual resolution still covers exec/commission,
//!   which DO carry `order_reference`).
//! - A subscription `Notice` → `IbInbound::Error{code, order_id: 0, ..}` (ibapi's `Notice` type
//!   carries no order id — even a hard order-rejection loses it on the global stream). The mapper
//!   drops advisories via `classify_code`; for an order-rejection-class code it attributes the
//!   id-less reject to the sole still-unacked order so nothing vanishes (see `EventMapper::on_error`
//!   — routing lives in the pure module, not here). A terminal subscription `Err` (connection
//!   reset/shutdown) → `IbInbound::StreamDead` and stops the pump — this backend has NO
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
//! IB replays nothing on its own, so a fresh connection's coid⇄orderId map is empty: it could neither
//! cancel an order that rested across the outage nor follow its fills. [`socket_connect_seed`] therefore
//! ends with `Connected`, on which the exec loop asks for the open orders; when the venue says that
//! snapshot ended it asks for the day's executions; when those ended the connection announces itself
//! learned (`ExecLink{up: true}`, `exec::run_exec`'s `deliver`). Three properties are this file's:
//! - **Non-blocking.** `Client::all_open_orders` and `Client::executions` WRITE the request and return
//!   (the 4.2.0 and 5.1.0 source: `send_shared_request` / `send_request`, neither waits for a reply), so the exec
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
//! ## What ibapi does (read from the registry source, 4.2.0 first, 5.1.0 after the bump; not from docs.rs)
//! - `process_orders` fans `OpenOrder`, `ExecutionData` and `CommissionsReport` onto the global
//!   `order_update_stream` FIRST (`send_order_update`), then to the per-request channel, so the rows of a
//!   request land in `pump_loop` whether or not its subscription is read.
//! - `Orders`/`Executions` inherit `StreamDecoder::cancel_message`'s `Err(NotImplemented)`, so dropping
//!   the subscription sends no cancel frame; `OpenOrderEnd` / `ExecutionDataEnd` decode to
//!   `Error::EndOfStream`, which `Subscription::next*` reports as `None` — indistinguishable from a
//!   timeout except by the time it took, which [`wait_for_end`] uses.
//! - A subscription dropped before its `ExecutionDataEnd` arrives makes that frame unroutable
//!   (a `warn!` in ibapi, nothing else): the waiter therefore keeps the subscription until the end.
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
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use ibapi::client::blocking::{Client, NoticeStream, Subscription};
use ibapi::contracts::{Contract, Currency, Exchange, OptionRight, SecurityType, Symbol};
use ibapi::orders::{
    Action, ExecutionFilter, ExecutionSide, Order, OrderUpdate, TimeInForce as IbTif,
};
use ibapi::subscriptions::SubscriptionItem;
use vike_marketdata::OptionKind;

use super::gateway_gate::{SettleCount, Settling, gate_for};
use super::link_watch::{Notices, Watch, WatchTimes, Watching};
use super::{AmendOutcome, IbInbound, IbkrTransport, OwnOrders, Snapshot, socket_connect_seed};
use crate::config::IbkrConfig;
use crate::contract::{IbkrContract, SecType};
use crate::error::IbkrError;
use crate::exec::event_mapper::{IbCommissionReport, IbExecDetails, IbOpenOrder, IbOrderStatus};
use crate::order::IbOrderSpec;

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

/// Run one blocking `ibapi` open step on a throwaway thread and stop waiting for it after `within`:
/// `IbkrError::Connect` naming `what` when the deadline wins. The thread is ABANDONED, not joined (the
/// blocking wait is what is being escaped): it finishes whenever `ibapi` lets go, and whatever it built
/// (a connected client) is dropped there, which frees the gateway's client id. Same pattern as
/// `vike_bridge_core::ws_proxy`'s `call_with_deadline`.
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
    /// A TWS advisory bound to the stream.
    Notice {
        code: i32,
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
    /// Block up to `within` (at least a few milliseconds: see [`ClientSession`]).
    fn poll(&mut self, within: Duration) -> Polled;
}

/// One snapshot request in flight (the per-request subscription of `all_open_orders` /
/// `executions`).
pub(crate) trait SnapshotHandle: Send {
    /// Wait up to `within` for the venue to end the request: `true` = it did, `false` = it failed or
    /// did not end in time.
    fn wait(self: Box<Self>, within: Duration) -> bool;
}

/// What the socket backend needs of an IB API session. The real one is [`ClientSession`]; the unit
/// tests' double scripts the same calls.
pub(crate) trait IbSession: Send + Sync {
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
}

/// The end markers of finished snapshot requests, waiting for the pump to release them in order.
#[derive(Default)]
struct Markers(Mutex<Vec<(Snapshot, bool)>>);

impl Markers {
    fn push(&self, snapshot: Snapshot, complete: bool) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push((snapshot, complete));
    }

    fn take(&self) -> Vec<(Snapshot, bool)> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn pending(&self) -> bool {
        !self.0.lock().unwrap_or_else(PoisonError::into_inner).is_empty()
    }
}

/// The live socket-backed transport. Owns the session (for place/cancel/the snapshot requests) and the
/// receiving end of the pump channel (for `next_recv`); a background thread owns the order-update
/// stream and translates each item onto `tx`.
pub struct SocketTransport {
    session: Arc<dyn IbSession>,
    /// The API client id this connection places orders under: the filter applied to the execution
    /// replay (`request_executions`) and half of [`IbkrTransport::ownership`].
    client_id: i32,
    /// The configured account (`cfg.account`) every order is placed in (`build_order`).
    account: String,
    /// `cfg.extended_hours` (`venue.ibkr.<tier>.extended_hours`): see [`Self::order_for`]. OFF unless
    /// [`Self::with_extended_hours`] set it, which is what keeps every other door of this type as it was.
    extended_hours: bool,
    /// A clone of the pump channel's sender, used ONLY to synthesize an order-rejection inbound when
    /// `submit_order` fails synchronously (so a rejected intent never silently vanishes).
    tx: Sender<IbInbound>,
    inbound: mpsc::Receiver<IbInbound>,
    markers: Arc<Markers>,
    /// How long a snapshot request may take to end ([`SNAPSHOT_DEADLINE`]; a test shortens it).
    snapshot_deadline: Duration,
    /// Set when the transport is dropped; the pump checks it every poll and then ends, which drops its
    /// order-update stream and with it the last hold on the ibapi client (see `Drop` below).
    stop: Arc<AtomicBool>,
    /// The gateway's settling count, when watched: the drop takes a hold on it at once and hands it to the pump,
    /// which lets go after closing the client, so a dial never starts inside the up-to-[`IDLE_POLL`] window
    /// between the drop and the close (the old client id is still held then).
    settle: Option<SettleCount>,
    closing: Closing,
}

/// The settling hold a dropped transport hands its pump (see [`SocketTransport::settle`]). `Done` is the pump
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
impl Drop for SocketTransport {
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

impl SocketTransport {
    /// Connect the blocking ibapi client to the Gateway/TWS at `cfg.host:cfg.port` under
    /// `cfg.client_id`, seed the id/account signals, and spawn the order-update pump. Maps any ibapi
    /// connect/stream failure to `IbkrError::Connect` (→ `Unavailable` at the app root → stay paper).
    pub fn connect(cfg: &IbkrConfig) -> Result<SocketTransport, IbkrError> {
        Self::connect_within(cfg, OPEN_DEADLINE)
    }

    /// [`Self::connect`] under a `within` bound ([`bounded`]): the dial, the handshake and the
    /// next-valid-id read of [`Self::start_watched`] are all `ibapi` waits with no timeout of their own.
    pub(crate) fn connect_within(
        cfg: &IbkrConfig,
        within: Duration,
    ) -> Result<SocketTransport, IbkrError> {
        // The MASTER client (id 0) is bound by TWS to every order made by hand, and a hand-typed `Order Ref`
        // on one passes the ownership test of this bridge: an exec account may not use it (review 18, R4).
        refuse_master_client(cfg.client_id)?;
        let cfg = cfg.clone();
        let gate = gate_for(&format!("{}:{}", cfg.host, cfg.port));
        // A gateway whose link to IBKR was lost is rebuilt on a spaced schedule: a refused attempt dials nothing.
        gate.permit_exec()?;
        let watching = Watching { gate: Arc::clone(&gate), times: WatchTimes::PRODUCTION };
        let (opened, timed_out) = gate.dial("exec connect", within, move || {
            let addr = format!("{}:{}", cfg.host, cfg.port);
            let (client, notices) = Client::builder()
                .address(addr.clone())
                .client_id(cfg.client_id)
                .connect_with_notice_stream()
                .map_err(|e| {
                    IbkrError::Connect(format!("{addr} (client_id={}): {e}", cfg.client_id))
                })?;
            Self::start_watched(
                Arc::new(ClientSession::new(client, notices)),
                cfg.client_id,
                &cfg.account,
                Some(watching),
            )
            .map(|transport| transport.with_extended_hours(cfg.extended_hours))
        });
        if opened.is_err() {
            gate.exec_failed(timed_out);
        }
        opened
    }

    /// [`Self::start_watched`] with no link watchdog: the unit tests' door (a double has no notices).
    #[cfg(test)]
    pub(crate) fn start(
        session: Arc<dyn IbSession>,
        client_id: i32,
        account: &str,
    ) -> Result<SocketTransport, IbkrError> {
        Self::start_watched(session, client_id, account, None)
    }

    /// Everything `connect` does once the session exists. Separate so the unit tests drive THE
    /// production sequence (the seed, the stream, the pump) over a double. `watching` is the link watchdog
    /// the pump runs beside the stream (`link_watch`); `None` = unwatched.
    pub(crate) fn start_watched(
        session: Arc<dyn IbSession>,
        client_id: i32,
        account: &str,
        watching: Option<Watching>,
    ) -> Result<SocketTransport, IbkrError> {
        let (tx, rx) = mpsc::channel::<IbInbound>();

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
        let pump = Pump {
            closing: Arc::clone(&closing),
            updates,
            tx: tx.clone(),
            markers: Arc::clone(&markers),
            own: OwnOrders { client_id, account: account.to_string() },
            stop: Arc::clone(&stop),
            session: Arc::clone(&session),
            watch: watching.map(|w| Watch::new(w, session.notices(), Instant::now())),
            settle: settle.clone(),
        };
        thread::Builder::new()
            .name("ibkr-pump".into())
            .spawn(move || pump.run())
            .map_err(|e| IbkrError::Connect(format!("pump thread spawn: {e}")))?;

        Ok(SocketTransport {
            session,
            client_id,
            account: account.to_string(),
            extended_hours: false,
            tx,
            inbound: rx,
            markers,
            snapshot_deadline: SNAPSHOT_DEADLINE,
            stop,
            settle,
            closing,
        })
    }

    /// This transport with the account's `extended_hours` switch applied ([`Self::order_for`]). Said once,
    /// loudly, when on: it changes which orders IBKR may fill outside the regular session.
    pub(crate) fn with_extended_hours(mut self, on: bool) -> Self {
        if on {
            tracing::warn!(
                account = %self.account,
                "ibkr: EXTENDED HOURS is ON for this account (`venue.ibkr.<tier>.extended_hours`): every \
                 US-stock LIMIT order is sent with outsideRth and includeOvernight, so it may fill \
                 pre-market, after-hours and in the overnight session; market and stop orders are not flagged"
            );
        }
        self.extended_hours = on;
        self
    }

    /// The `ibapi` order `place_order` and `modify_order` both hand the session. **Only a LIMIT order on a
    /// STOCK is ever flagged `outside_rth` + `include_overnight`, and only when the account's switch is on**
    /// (of the kinds the node sends only a limit order is eligible outside the regular session, and the
    /// overnight session is US stocks and ETFs): a market or stop order, and a forex or futures order, are
    /// built exactly as with the switch off.
    /// The amend re-places the order under its own id, so it goes through here and keeps its flags.
    fn order_for(&self, spec: &IbOrderSpec, contract: &IbkrContract) -> Order {
        let flagged =
            self.extended_hours && spec.order_type == "LMT" && contract.sec_type == SecType::Stk;
        build_order(spec, &self.account, flagged)
    }

    /// Read `handle` to its end on its own short-lived thread, and queue the answer for the pump to
    /// release behind the rows.
    fn await_end(&self, snapshot: Snapshot, handle: Box<dyn SnapshotHandle>) {
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

/// The master client id (0) is refused for an exec account: see [`SocketTransport::connect`].
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

impl IbkrTransport for SocketTransport {
    fn place_order(&mut self, order_id: i32, spec: &IbOrderSpec, contract: &IbkrContract) {
        let ib_contract = build_contract(contract);
        let ib_order = self.order_for(spec, contract);
        // Fire-and-forget: updates arrive on the global order_update_stream, not a per-order handle.
        if let Err(e) = self.session.submit_order(order_id, &ib_contract, &ib_order) {
            tracing::warn!(order_id, error = %e, "ibkr submit_order failed");
            // Synchronous validation failure → synthesize an order-rejection so the bound coid gets a
            // terminal (code 201 = "order rejected", classified as OrderRejection by the mapper).
            let _ = self.tx.send(IbInbound::Error {
                code: 201,
                order_id,
                msg: format!("submit_order failed: {e}"),
            });
        }
    }

    /// Amend by RE-PLACING the order under its own `order_id`: TWS has no amend verb, it reads a
    /// `placeOrder` with an id it already holds as a modification and answers with an `openOrder` row of the
    /// new terms (`exec::amend::PendingAmends` waits for that row). A write failure is the one thing known
    /// here, and it refuses the amend (non-terminal): unlike `place_order` it must NOT be synthesized as the
    /// order's rejection, because the order is still working at its old terms.
    fn modify_order(
        &mut self,
        order_id: i32,
        spec: &IbOrderSpec,
        contract: &IbkrContract,
    ) -> AmendOutcome {
        let ib_contract = build_contract(contract);
        let ib_order = self.order_for(spec, contract);
        match self.session.submit_order(order_id, &ib_contract, &ib_order) {
            Ok(()) => AmendOutcome::Sent,
            Err(e) => {
                tracing::warn!(order_id, error = %e, "ibkr amend (re-place under the same id) failed");
                AmendOutcome::Refused(format!("ibkr: the amend could not be written: {e}"))
            }
        }
    }

    fn cancel_order(&mut self, order_id: i32) {
        // The returned per-request subscription is not needed — the resulting `Cancelled`
        // OrderStatus flows through the global stream. Fire and drop.
        if let Err(e) = self.session.cancel_order(order_id) {
            tracing::warn!(order_id, error = %e, "ibkr cancel_order failed");
        }
    }

    /// `reqAllOpenOrders`. The rows come back on the global stream (`translate` → `VenueOrder`); the
    /// request's END comes back only on its own subscription, read by a short-lived thread and
    /// released by the pump behind the rows as `SnapshotEnd`. A request that cannot even be written
    /// is reported as an incomplete snapshot, the same way.
    fn request_open_orders(&mut self) {
        match self.session.all_open_orders() {
            Ok(handle) => self.await_end(Snapshot::OpenOrders, handle),
            Err(e) => {
                tracing::warn!(error = %e, "ibkr all_open_orders failed");
                self.markers.push(Snapshot::OpenOrders, false);
            }
        }
    }

    /// `reqExecutions`, scoped to THIS client id: the commission-recovery request of the stranded-fill
    /// sweep (`event_mapper` module doc trap 5) and the second half of the connect-time learning. The
    /// rows (`ExecutionData` + `CommissionReport`) arrive on the global stream; replaying pairs whose
    /// fill already emitted is harmless — `EventMapper`'s `emitted` index drops both halves, and
    /// `vike_exec::ExecutionEngine`'s `seen_trade_ids` is a second net. Its END is reported like the
    /// open orders' (the exec loop ignores the ones it did not wait for).
    fn request_executions(&mut self) {
        match self.session.executions(self.client_id) {
            Ok(handle) => self.await_end(Snapshot::Executions, handle),
            Err(e) => {
                tracing::warn!(client_id = self.client_id, error = %e, "ibkr executions replay failed");
                self.markers.push(Snapshot::Executions, false);
            }
        }
    }

    fn ownership(&self) -> Option<OwnOrders> {
        Some(OwnOrders { client_id: self.client_id, account: self.account.clone() })
    }

    fn next_recv(&mut self, timeout: Duration) -> Option<IbInbound> {
        match self.inbound.recv_timeout(timeout) {
            Ok(inbound) => Some(inbound),
            // Timeout: let the exec loop poll its command channel (Shutdown).
            Err(RecvTimeoutError::Timeout) => None,
            // Pump ended (stream closed): report a timeout so the loop keeps servicing commands.
            // Sleep for `timeout` first — recv on an already-disconnected channel returns
            // immediately, so without this the exec loop would busy-spin a core once the pump
            // thread has exited.
            Err(RecvTimeoutError::Disconnected) => {
                thread::sleep(timeout);
                None
            }
        }
    }
}

/// How long the pump waits for an update when nothing else is pending, and while an end marker is
/// waiting for the stream to drain (short: the marker is released at the first empty poll).
const IDLE_POLL: Duration = Duration::from_millis(250);
const DRAIN_POLL: Duration = Duration::from_millis(20);

/// The background pump: drain the order-update stream, translating each item into an `IbInbound` on
/// `tx`, and release the end markers of finished snapshot requests behind the rows (an end marker is
/// released only by a poll that found the stream EMPTY). Exits when the stream ends, on a terminal
/// error (after signalling `StreamDead`), or when the receiver is dropped (transport gone) - which an
/// idle stream never shows, so `stop` (set by `SocketTransport`'s `Drop`) is checked every iteration.
struct Pump {
    updates: Box<dyn Updates>,
    tx: Sender<IbInbound>,
    markers: Arc<Markers>,
    own: OwnOrders,
    stop: Arc<AtomicBool>,
    /// Closed when the pump ends, whatever ended it: the gateway frees the client id now, and `ibapi` stops
    /// reconnecting a zombie that would take it back (the daemon rebuilds with a NEW client).
    session: Arc<dyn IbSession>,
    /// The link watchdog (`link_watch`), when this transport is watched.
    watch: Option<Watch>,
    /// The gateway's settling count: a pump that is closing its client holds it, and the next dial waits.
    settle: Option<SettleCount>,
    /// The hold a dropped transport took for this pump (`SocketTransport`'s `Drop`), let go once the client is closed.
    closing: Closing,
}

impl Pump {
    fn run(mut self) {
        self.pump();
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
                Polled::Notice { code, message } => {
                    // TWS advisory bound to the stream — surface as Error; the mapper drops advisories.
                    if self.tx.send(IbInbound::Error { code, order_id: 0, msg: message }).is_err() {
                        break;
                    }
                }
                Polled::Idle => {
                    for (snapshot, complete) in self.markers.take() {
                        if let Some(watch) = self.watch.as_mut() {
                            watch.snapshot(complete, Instant::now());
                        }
                        if self.tx.send(IbInbound::SnapshotEnd { snapshot, complete }).is_err() {
                            return;
                        }
                    }
                }
                Polled::Failed(e) => {
                    // TERMINAL transport error (connection reset / shutdown). This backend has no
                    // reconnect, so say exactly that: `StreamDead` latches the venue closed in
                    // `exec::run_exec` and every later submit is refused with a synthetic reject.
                    tracing::error!(error = %e, "ibkr order-update stream terminated — venue is DEAD");
                    let _ = self.tx.send(IbInbound::StreamDead { reason: e });
                    break;
                }
                Polled::Closed => {
                    // Clean end of stream. Also terminal for this transport (nothing re-opens the
                    // subscription), so it degrades the venue exactly like the error arm rather than
                    // leaving the exec loop accepting orders against a pump that has stopped.
                    tracing::error!("ibkr order-update stream closed — venue is DEAD");
                    let _ = self.tx.send(IbInbound::StreamDead {
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
                let _ = self.tx.send(IbInbound::StreamDead { reason });
                break;
            }
        }
    }
}

/// Translate one ibapi `OrderUpdate` into a normalized `IbInbound`. An `OpenOrder` becomes a
/// `VenueOrder` row carrying the identity of its owner and its quantity: whether it is OURS is the exec
/// loop's question ([`IbkrTransport::ownership`]), because `reqAllOpenOrders` answers for every client
/// of the login.
fn translate(update: OrderUpdate, own: &OwnOrders) -> Option<IbInbound> {
    match update {
        // `reqAllOpenOrders` makes TWS send an `orderStatus` for EVERY client's open order on every
        // connect, and ids are per API client: another client's order whose id equals one of ours must not
        // resolve to our coid. A message that names ANOTHER client (non-zero, not ours) is dropped here; 0
        // is also what an undecoded field reads, so it is never taken as proof of a stranger (a manual TWS
        // order cannot collide with our ids anyway: they are 0 or negative).
        OrderUpdate::OrderStatus(s) if s.client_id != 0 && s.client_id != own.client_id => None,
        OrderUpdate::ExecutionData(ed)
            if ed.execution.client_id != 0 && ed.execution.client_id != own.client_id =>
        {
            None
        }
        OrderUpdate::OrderStatus(s) => Some(IbInbound::OrderStatus(IbOrderStatus {
            order_id: s.order_id,
            // ibapi's OrderStatus carries no orderRef; resolve by numeric order_id (bound at submit).
            order_ref: String::new(),
            status: s.status.as_str().to_string(),
            filled: s.filled,
            avg_fill_price: s.average_fill_price.unwrap_or(0.0),
            client_id: s.client_id,
        })),
        OrderUpdate::ExecutionData(ed) => {
            // Reconstruct the canonical `SYMBOL.EXCHANGE.CURRENCY` from the exec contract.
            let symbol = format!(
                "{}.{}.{}",
                ed.contract.symbol.0, ed.contract.exchange.0, ed.contract.currency.0
            );
            Some(IbInbound::ExecDetails(IbExecDetails {
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
        OrderUpdate::CommissionReport(c) => Some(IbInbound::Commission(IbCommissionReport {
            exec_id: c.execution_id,
            commission: c.commission,
            currency: c.currency,
        })),
        OrderUpdate::OpenOrder(od) => Some(IbInbound::VenueOrder(IbOpenOrder {
            order_id: od.order_id,
            order_ref: od.order.order_ref,
            client_id: od.order.client_id,
            account: od.order.account,
            total_qty: od.order.total_quantity,
            status: od.order_state.status.as_str().to_string(),
            filled_qty: od.order.filled_quantity,
            limit_price: od.order.limit_price,
            aux_price: od.order.aux_price,
        })),
        // ibapi 4: TWS binds an order's permanent id to this client's order id. Everything here is
        // keyed on the client order id bound at submit (`order_id` / the coid in `order_ref`), so the
        // permId changes nothing — dropped, like an advisory the mapper would ignore.
        OrderUpdate::OrderBound(_) => None,
    }
}

// ---- the real session ----------------------------------------------------------------------------

/// The real [`IbSession`]: the blocking ibapi client, and the notice stream bound before its connect (taken once).
struct ClientSession(Arc<Client>, Mutex<Option<NoticeStream>>);

/// The client's notices, drained without blocking.
struct NoticeFeed(NoticeStream);

impl Notices for NoticeFeed {
    fn next(&mut self) -> Option<(i32, String)> {
        self.0.try_next().map(|n| (n.code, n.message))
    }
}

impl ClientSession {
    fn new(client: Client, notices: NoticeStream) -> Self {
        Self(Arc::new(client), Mutex::new(Some(notices)))
    }
}

impl IbSession for ClientSession {
    fn notices(&self) -> Option<Box<dyn Notices>> {
        let taken = self.1.lock().unwrap_or_else(PoisonError::into_inner).take();
        taken.map(|n| Box::new(NoticeFeed(n)) as Box<dyn Notices>)
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
        Ok(Box::new(UpdateStream(stream)))
    }
}

impl ClientSession {
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

struct UpdateStream(Subscription<OrderUpdate>);

impl Updates for UpdateStream {
    fn poll(&mut self, within: Duration) -> Polled {
        let began = Instant::now();
        match self.0.next_timeout(within) {
            Some(Ok(SubscriptionItem::Data(update))) => Polled::Update(Box::new(update)),
            Some(Ok(SubscriptionItem::Notice(n))) => {
                Polled::Notice { code: n.code, message: n.message }
            }
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

/// Build an `ibapi::Order` from the pure `IbOrderSpec`. `transmit` defaults to `true` (ibapi's
/// `Order::default`), so the order is live on submit. `account` is the configured `cfg.account`,
/// stamped on the order whenever it is set: on a login that manages several accounts an order
/// without one goes to the login's default account, and the reconcile client
/// (`crate::recon::socket`) reads only `cfg.account`, so exec and reconcile would see two accounts.
/// `extended_hours` is the caller's verdict ([`SocketTransport::order_for`]); only then are `outside_rth` and
/// `include_overnight` set, and with it `false` the order is the one this function always built (`ibapi`
/// defaults both flags to `false`).
fn build_order(spec: &IbOrderSpec, account: &str, extended_hours: bool) -> Order {
    Order {
        action: if spec.action == "BUY" { Action::Buy } else { Action::Sell },
        total_quantity: spec.total_qty,
        order_type: spec.order_type.to_string(),
        limit_price: spec.lmt_price,
        aux_price: spec.aux_price,
        tif: tif(spec.tif),
        order_ref: spec.order_ref.clone(),
        account: account.trim().to_string(),
        outside_rth: extended_hours,
        include_overnight: extended_hours,
        ..Order::default()
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
        _ => IbTif::Day,
    }
}

#[cfg(test)]
mod tests {
    use super::{build_contract, build_order};
    use crate::contract::parse_simplified;
    use ibapi::contracts::SecurityType;
    use std::assert_matches;

    /// THE WIRE ASSERTION, made on the CONSTRUCTED `ibapi::Contract` rather than on any string:
    /// this is the exact value `SocketTransport::place_order` hands the `ibapi` client, so what it
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
    /// reconcile reads `cfg.account` only. The built order — what `place_order` hands ibapi — names
    /// it, trimmed; an unset account leaves it blank (the login's default, as before).
    #[test]
    fn the_built_order_carries_the_configured_account() {
        let spec = crate::order::IbOrderSpec {
            action: "BUY",
            total_qty: 1.0,
            order_type: "LMT",
            lmt_price: Some(1.0),
            aux_price: None,
            tif: "DAY",
            order_ref: "coid-1".to_string(),
        };
        assert_eq!(build_order(&spec, "DU1234567", false).account, "DU1234567");
        assert_eq!(build_order(&spec, " DU1234567 ", false).account, "DU1234567");
        assert_eq!(build_order(&spec, "", false).account, "");
        assert_eq!(build_order(&spec, "DU1234567", false).order_ref, "coid-1");
    }
}

#[path = "socket_tests.rs"]
#[cfg(test)]
pub(crate) mod socket_tests;

#[path = "socket_watch_tests.rs"]
#[cfg(test)]
mod socket_watch_tests;
