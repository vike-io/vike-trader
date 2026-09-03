//! The REAL socket backend: `SocketTransport` wraps the vendored blocking `ibapi` client
//! (`ibapi::client::blocking::Client`, `vendor/ibapi` @ v3.2.1, sync-only) and implements the
//! `IbkrTransport` seam. Thin glue ONLY — every order-correctness decision lives in the pure
//! modules (`event_mapper`/`order`/`contract`/`id_registry`); this file just marshals vike types
//! ⇄ ibapi types and pumps ibapi's global order-update stream onto an internal channel that the
//! exec loop drains via `next_recv`.
//!
//! No unit tests here (per the design): the runtime validation is the paper smoke against a live
//! IB Gateway. The lifecycle logic is already proven by the `FakeTransport` over the SAME
//! `run_exec` loop.
//!
//! ⚠ That smoke has now RUN, on the CI box 2026-08-19, against a real IB Gateway — submit → accept →
//! cancel on the paper account, with the coid intact on both terminal events. Until then this file
//! had been compiled and reviewed but never spoken to a gateway, so every mapping decision below
//! was an argument from the vendored source rather than an observation.
//! `docs/ops/ibkr-socket-gateway.md` is the bring-up (IB Gateway is NOT the Client Portal Gateway
//! the cpapi backend talks to, and the two cannot hold the account at the same time).
//!
//! ## Mapping decisions (verified against the vendored source, not the research notes)
//! - Placement uses `Client::submit_order` (fire-and-forget) + a single global
//!   `Client::order_update_stream()` as the event pump — the ibapi-documented pairing. The stream
//!   yields `OrderUpdate::{OrderStatus, ExecutionData, CommissionReport, OpenOrder}` for ALL orders,
//!   which we translate 1:1 into `IbInbound`.
//! - ibapi's `OrderStatus` has NO `orderRef` field, so status inbound resolves by numeric `order_id`
//!   (the exec loop binds `order_id → coid` at submit; dual resolution still covers exec/commission,
//!   which DO carry `order_reference`).
//! - A subscription `Notice` → `IbInbound::Error{code, order_id: 0, ..}` (ibapi's `Notice` type
//!   carries no order id — even a hard order-rejection loses it on the global stream). The mapper
//!   drops advisories via `classify_code`; for an order-rejection-class code it attributes the
//!   id-less reject to the sole still-unacked order so nothing vanishes (see `EventMapper::on_error`
//!   — routing lives in the pure module, not here). A terminal subscription `Err` (connection
//!   reset/shutdown)
//!   → `IbInbound::StreamDead` and stops the pump — this backend has NO auto-reconnect, and the
//!   variant says so. (It used to send `Reconnected`, which reconnected nothing.)
//! - Fill `ts` is stamped at receive time in epoch-millis (vike's `FillEvent.ts` unit, matching the
//!   crypto bridges). ibapi surfaces the exchange fill time only as a preformatted `String`
//!   (`Execution.time`); parsing it to epoch-millis needs the account timezone and is deferred to
//!   the live smoke. ⚠ STILL DEFERRED after that smoke ran: it rests a limit priced far from the
//!   market precisely so it cannot fill, so no `Execution` was ever observed and the timezone
//!   question is untouched. Proving it needs a marketable order on the paper account, which is a
//!   deliberately different risk from anything the smoke does today.
//!
//! ## ⚠ The fill is STILL unobserved after the 2026-08-23 sweep — and what it would take
//!
//! The INFRASTRUCTURE is no longer the blocker: the CI box carries a full IB Gateway + IBC install at
//! `<project>/bin/ibkr-gateway` on port 4102, with its own settings store beside it, and both socket
//! smokes ran green through it on 2026-08-19 (`docs/ops/ibkr-socket-gateway.md` carries the verbatim
//! runs). Three things stand between that and an observed fill:
//!
//! 1. **The account is exclusive, and handing it back is the expensive half.** Starting IB Gateway
//!    EVICTS the Client Portal Gateway session the CI box normally holds — one account, one claimant,
//!    across both products. Recovery is not another cpapi login: the cpapi gateway PROCESS has to be
//!    restarted first, or the login form comes back blank and ends in `NOT AUTHENTICATED within
//!    90s`. That runbook rates it an hour. So this is a scheduled operation, not something to bolt
//!    onto an unrelated sweep — which is why the 2026-08-23 pass left the cpapi session up and
//!    verified that backend instead.
//! 2. **An open session.** A marketable order fills only while the venue trades, and that sweep ran
//!    on a Sunday with US equities shut. `CASH`/IDEALPRO forex has the widest window and is also the
//!    leg that answers the second question below, so it is the better instrument here than a stock.
//! 3. **A marketable order and a flatten.** One unit through the touch, then the opposite side to
//!    leave the account flat — materially more than the resting far-from-market limit the smoke
//!    places today, which is exactly why it has never been folded into that smoke.
//!
//! What such a run would settle, and nothing short of it can: whether `Execution.time` parses to
//! epoch-millis without the account timezone (this file stamps `now_ms()` instead), and whether the
//! forex `symbol` echo reconstructs as `SYMBOL.EXCHANGE.CURRENCY` — `translate` builds that string
//! from `ed.contract.{symbol,exchange,currency}`, and for a `CASH` pair the venue's own spelling of
//! those three has never been seen.
//!
//! ⚠ The cpapi backend cannot stand in for this. Its fill path was measured DEAD on 2026-08-23 (its
//! WS `sor` topic answers nothing — `transport/cpapi/mod.rs`'s module doc), so a fill produced there
//! would emit no event at all.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ibapi::client::blocking::{Client, Subscription};
use ibapi::contracts::{Contract, Currency, Exchange, OptionRight, SecurityType, Symbol};
use ibapi::orders::{
    Action, ExecutionFilter, ExecutionSide, Order, OrderUpdate, TimeInForce as IbTif,
};
use ibapi::subscriptions::SubscriptionItem;

use super::{IbInbound, IbkrTransport};
use crate::config::IbkrConfig;
use crate::contract::{IbkrContract, SecType};
use crate::error::IbkrError;
use crate::event_mapper::{IbCommissionReport, IbExecDetails, IbOrderStatus};
use crate::order::IbOrderSpec;

/// The live socket-backed transport. Owns the shared blocking client (for place/cancel) and the
/// receiving end of the pump channel (for `next_recv`); a background thread owns the ibapi
/// `order_update_stream` subscription and translates each item onto `tx`.
pub struct SocketTransport {
    client: Arc<Client>,
    /// The API client id this connection places orders under — the ONLY filter applied to the
    /// execution replay (`request_executions`), so it never drags in another client's or a manual
    /// TWS trader's executions.
    client_id: i32,
    /// A clone of the pump channel's sender, used ONLY to synthesize an order-rejection inbound when
    /// `submit_order` fails synchronously (so a rejected intent never silently vanishes).
    tx: Sender<IbInbound>,
    inbound: mpsc::Receiver<IbInbound>,
}

impl SocketTransport {
    /// Connect the blocking ibapi client to the Gateway/TWS at `cfg.host:cfg.port` under
    /// `cfg.client_id`, seed the id/account signals, and spawn the order-update pump. Maps any ibapi
    /// connect/stream failure to `IbkrError::Connect` (→ `Unavailable` at the app root → stay paper).
    pub fn connect(cfg: &IbkrConfig) -> Result<SocketTransport, IbkrError> {
        let addr = format!("{}:{}", cfg.host, cfg.port);
        let client = Client::connect(&addr, cfg.client_id).map_err(|e| {
            IbkrError::Connect(format!("{addr} (client_id={}): {e}", cfg.client_id))
        })?;
        let client = Arc::new(client);

        let (tx, rx) = mpsc::channel::<IbInbound>();

        // Seed the exec loop's IdRegistry with IB's server-assigned next valid order id, then flag
        // account readiness (the blocking handshake has completed once `connect` returned). Both are
        // folded by the exec loop; failure to read the server id falls back to the local floor.
        let seed = client.next_valid_order_id().unwrap_or_else(|_| client.next_order_id());
        let _ = tx.send(IbInbound::NextValidId(seed));
        let _ = tx.send(IbInbound::AccountsReady);

        // The ONE global order-update stream = the event pump. Create it before any order so no
        // status/exec/commission is missed.
        let stream = client
            .order_update_stream()
            .map_err(|e| IbkrError::Connect(format!("order_update_stream: {e}")))?;

        let pump_tx = tx.clone();
        thread::Builder::new()
            .name("ibkr-pump".into())
            .spawn(move || pump_loop(stream, pump_tx))
            .map_err(|e| IbkrError::Connect(format!("pump thread spawn: {e}")))?;

        Ok(SocketTransport { client, client_id: cfg.client_id, tx, inbound: rx })
    }
}

impl IbkrTransport for SocketTransport {
    fn place_order(&mut self, order_id: i32, spec: &IbOrderSpec, contract: &IbkrContract) {
        let ib_contract = build_contract(contract);
        let ib_order = build_order(spec);
        // Fire-and-forget: updates arrive on the global order_update_stream, not a per-order handle.
        if let Err(e) = self.client.submit_order(order_id, &ib_contract, &ib_order) {
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

    fn cancel_order(&mut self, order_id: i32) {
        // The returned per-request `Subscription<CancelOrder>` is not needed — the resulting
        // `Cancelled` OrderStatus flows through the global stream. Fire and drop.
        if let Err(e) = self.client.cancel_order(order_id, "") {
            tracing::warn!(order_id, error = %e, "ibkr cancel_order failed");
        }
    }

    fn request_open_orders(&mut self) {
        // Reconnect resync (Task 10): `all_open_orders` returns its own per-request `Subscription`,
        // but ibapi's routing ALSO unconditionally forwards every OpenOrder/OrderStatus message onto
        // the global `order_update_stream` we're already pumping (`send_order_update` runs before the
        // per-request/shared-channel dispatch — verified in `transport::sync::process_orders`'s
        // `OrderRoutingStrategy::OrderOrShared` arm), so the returned subscription can be dropped:
        // each row still lands in `pump_loop` → `translate` → `IbInbound::OpenOrder` →
        // `EventMapper::rebind_open_order`.
        if let Err(e) = self.client.all_open_orders() {
            tracing::warn!(error = %e, "ibkr all_open_orders failed");
        }
    }

    /// Commission recovery (`event_mapper` module doc trap 5) — `reqExecutions`, scoped to THIS
    /// client id. Three vendored facts make it work, each verified in `vendor/ibapi` rather than
    /// assumed:
    ///
    /// 1. **It carries the commissions.**
    ///    `crates/bridges/vike-ibkr/vendor/ibapi/src/orders/sync.rs`'s `executions` (on `Client`):
    ///    *"Requests current day's (since midnight) executions matching the filter … Along with the
    ///    `ExecutionData`, the `CommissionReport` will also be returned."* That is exactly the half
    ///    a stranded `pending` entry is missing.
    /// 2. **The replay reaches our pump.** Both message types are fanned onto the global
    ///    `order_update_stream` BEFORE any per-request dispatch — `transport/sync.rs`'s
    ///    `process_orders` calls `send_order_update(&message)` as the first statement of the
    ///    `ExecutionData` arm AND of the `ByExecutionId` (CommissionsReport) arm. So each row still
    ///    lands in `pump_loop` → `translate` → `IbInbound::{ExecDetails, Commission}` → the mapper,
    ///    the same argument `request_open_orders` above already relies on.
    /// 3. **Dropping the returned subscription sends nothing to TWS.** `Executions` inherits
    ///    `StreamDecoder::cancel_message`'s default
    ///    (`crates/bridges/vike-ibkr/vendor/ibapi/src/subscriptions/common.rs`), which returns
    ///    `Err(NotImplemented)`; `Subscription::drop` → `cancel()` guards every branch on
    ///    `if let Ok(message)`, so no cancel frame is written and the replay is not truncated.
    ///
    /// Replaying pairs whose fill already emitted is harmless — `EventMapper`'s `emitted` index
    /// drops both halves, and `vike_exec::ExecutionEngine`'s `seen_trade_ids` is a second net.
    fn request_executions(&mut self) {
        let filter = ExecutionFilter { client_id: Some(self.client_id), ..Default::default() };
        // Fire and drop, exactly like `cancel_order`/`request_open_orders`: the rows arrive on the
        // global stream (fact 2), so the per-request subscription is dead weight.
        if let Err(e) = self.client.executions(filter) {
            tracing::warn!(client_id = self.client_id, error = %e, "ibkr executions replay failed");
        }
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

/// The background pump: drain ibapi's global order-update subscription, translating each item into an
/// `IbInbound` on `tx`. Exits when the stream ends, on a terminal error (after signalling
/// `Reconnected`), or when the receiver is dropped (transport gone).
fn pump_loop(stream: Subscription<OrderUpdate>, tx: Sender<IbInbound>) {
    loop {
        match stream.next() {
            Some(Ok(SubscriptionItem::Data(update))) => {
                if let Some(inbound) = translate(update) {
                    if tx.send(inbound).is_err() {
                        break; // receiver dropped
                    }
                }
            }
            Some(Ok(SubscriptionItem::Notice(n))) => {
                // TWS advisory bound to the stream — surface as Error; the mapper drops advisories.
                if tx.send(IbInbound::Error { code: n.code, order_id: 0, msg: n.message }).is_err()
                {
                    break;
                }
            }
            Some(Err(e)) => {
                // TERMINAL transport error (connection reset / shutdown). This backend has no
                // reconnect, so say exactly that: `StreamDead` latches the venue closed in
                // `exec::run_exec` and every later submit is refused with a synthetic reject.
                //
                // ⚠ This used to send `IbInbound::Reconnected` — a name for a thing that never
                // happened. The exec loop dutifully answered it with `request_open_orders` +
                // `request_executions` into the dead socket, then went on accepting submits forever.
                tracing::error!(error = %e, "ibkr order-update stream terminated — venue is DEAD");
                let _ = tx.send(IbInbound::StreamDead { reason: e.to_string() });
                break;
            }
            None => {
                // Clean end of stream. Also terminal for this transport (nothing re-opens the
                // subscription), so it degrades the venue exactly like the error arm rather than
                // leaving the exec loop accepting orders against a pump that has stopped.
                tracing::error!("ibkr order-update stream closed — venue is DEAD");
                let _ = tx.send(IbInbound::StreamDead {
                    reason: "order-update stream closed".to_string(),
                });
                break;
            }
        }
    }
}

/// Translate one ibapi `OrderUpdate` into a normalized `IbInbound`. `OpenOrder` becomes
/// `IbInbound::OpenOrder` — the reconnect-resync payload (Task 10) `fold_inbound` rebuilds the
/// coid⇄orderId map from via `EventMapper::rebind_open_order`.
fn translate(update: OrderUpdate) -> Option<IbInbound> {
    match update {
        OrderUpdate::OrderStatus(s) => Some(IbInbound::OrderStatus(IbOrderStatus {
            order_id: s.order_id,
            // ibapi's OrderStatus carries no orderRef; resolve by numeric order_id (bound at submit).
            order_ref: String::new(),
            status: s.status.as_str().to_string(),
            filled: s.filled,
            avg_fill_price: s.average_fill_price.unwrap_or(0.0),
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
                ts: vike_model::clock::now_ms(),
            }))
        }
        OrderUpdate::CommissionReport(c) => Some(IbInbound::Commission(IbCommissionReport {
            exec_id: c.execution_id,
            commission: c.commission,
            currency: c.currency,
        })),
        // Reconnect resync (Task 10): one row of the open-order snapshot IB replays in response to
        // `all_open_orders` (`request_open_orders` below). `order.order_ref` IS the coid — this is
        // what `EventMapper::rebind_open_order` rebuilds the coid⇄orderId map from.
        OrderUpdate::OpenOrder(od) => {
            Some(IbInbound::OpenOrder { order_id: od.order_id, order_ref: od.order.order_ref })
        }
    }
}

/// Build an `ibapi::Contract` from the vike `IbkrContract`.
fn build_contract(c: &IbkrContract) -> Contract {
    Contract {
        symbol: Symbol::from(c.symbol.as_str()),
        security_type: sec_type(&c.sec_type),
        exchange: Exchange::from(c.exchange.as_str()),
        currency: Currency::from(c.currency.as_str()),
        last_trade_date_or_contract_month: c.expiry.clone().unwrap_or_default(),
        strike: c.strike.unwrap_or(0.0),
        right: c.right.and_then(option_right),
        multiplier: c.multiplier.clone().unwrap_or_default(),
        contract_id: c.con_id.unwrap_or(0) as i32,
        ..Contract::default()
    }
}

/// Build an `ibapi::Order` from the pure `IbOrderSpec`. `transmit` defaults to `true` (ibapi's
/// `Order::default`), so the order is live on submit.
fn build_order(spec: &IbOrderSpec) -> Order {
    Order {
        action: if spec.action == "BUY" { Action::Buy } else { Action::Sell },
        total_quantity: spec.total_qty,
        order_type: spec.order_type.to_string(),
        limit_price: spec.lmt_price,
        aux_price: spec.aux_price,
        tif: tif(spec.tif),
        order_ref: spec.order_ref.clone(),
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

fn option_right(r: char) -> Option<OptionRight> {
    match r.to_ascii_uppercase() {
        'C' => Some(OptionRight::Call),
        'P' => Some(OptionRight::Put),
        _ => None,
    }
}

fn tif(s: &str) -> IbTif {
    match s {
        "GTC" => IbTif::GoodTilCanceled,
        "IOC" => IbTif::ImmediateOrCancel,
        "FOK" => IbTif::FillOrKill,
        "GTD" => IbTif::GoodTilDate,
        _ => IbTif::Day,
    }
}
