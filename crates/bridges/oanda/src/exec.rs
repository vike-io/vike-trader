//! OANDA exec half — `ExecutionClient` over the v20 orders API.
//!
//! Like FXCM, order I/O runs on ONE dedicated thread (blocking REST off the single-writer core);
//! `submit`/`cancel`/`modify` enqueue, and venue events return through the ingest via
//! [`EventSender`]. Orders carry `clientExtensions.id = client_order_id`, so cancel targets
//! `@{client_order_id}` (no id-tracking map needed).
//!
//! **Modify is OANDA's order REPLACE** ([`build_replace_body`], [`map_replace_response`]): the venue
//! cancels the resting order and creates a new one under a NEW venue order id. The replacement carries
//! the SAME `clientExtensions.id`, so the platform keeps ONE order: `@{client_order_id}` still
//! addresses it, later fills name it, and the new venue id returns on `OrderModified`. The replaced
//! order's `ORDER_CANCEL` is dropped by the stream decode (`crate::stream`) so it never reads as a
//! cancel of the order. Every modify outcome maps to an event, a refusal to the non-terminal
//! `OrderModifyRejected`.
//!
//! MARKET fills come back inline on the POST response; delayed
//! LIMIT/STOP fills + cancels arrive on the transactions stream (a 2nd reader thread — see
//! [`crate::stream`]). Audit A3: a fill/cancel that lands during a transactions-stream reconnect
//! is recovered by backfilling `/transactions/sinceid` on re-open — see [`crate::stream`]. The
//! shared `last_seen` watermark is advanced here (from each POST's `lastTransactionID`) and by the
//! stream (from each transaction `id`), so a resting-order fill during an early reconnect — before
//! any stream transaction — is still bounded to the right floor, not backfilled from account start.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::thread;

use serde_json::json;
use vike_bridge_core::exec_actor::{
    CancelOutcome, ExecActor, ExecCommand, cancel_batch_undeclared, cancel_event,
};
use vike_catalog::to_oanda_instrument;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderModified,
    OrderModifyRejected, OrderRejected, OrderSubmitted, TradeId,
};
use vike_model::{OrderRequest, OrderType, TimeInForce};

use crate::config::OandaConfig;
use crate::rest::OandaRest;

const VENUE: &str = "oanda";

/// Live OANDA exec client. `submit`/`cancel` enqueue onto the REST thread (non-blocking); every
/// venue event returns through the core ingest. Dropping it stops both threads.
pub struct OandaExecutionClient(ExecActor);

impl OandaExecutionClient {
    /// Hand this client the HALT sentinel it watches: the venue's start passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live start calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self(self.0.with_halt_path(path))
    }

    pub fn spawn(config: OandaConfig, events: EventSender) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        // Shared audit-A3 watermark: the highest transaction id seen on EITHER the POST path
        // (below) or the transactions stream. The stream backfills `/transactions/sinceid` from
        // this on reconnect. 0 = nothing seen yet (skip backfill).
        let last_seen = Arc::new(AtomicU64::new(0));
        let actor = ExecActor::spawn("oanda-exec", events.clone(), {
            let (config, events, last_seen) = (config.clone(), events.clone(), last_seen.clone());
            move |rx| run(config, events, rx, last_seen)
        });
        // Background thread: the transactions stream (delayed LIMIT/STOP fills + cancels the order
        // POST can't carry). ExecActor flag-stops + joins it on teardown (deterministic).
        let stream_join = thread::Builder::new()
            .name("oanda-stream".into())
            .spawn({
                let stop = stop.clone();
                move || crate::stream::stream_transactions(config, events, stop, last_seen)
            })
            .expect("spawn oanda-stream thread");
        Self(actor.with_background(stop, stream_join))
    }
}

impl ExecutionClient for OandaExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.0.submit(request)
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.0.cancel(client_order_id)
    }
    /// Native amend = OANDA's order REPLACE (see [`build_replace_body`]). ⚠ It MUST be delegated
    /// like every other method on this wrapper: a newtype that omits it inherits the trait's no-op,
    /// which is exactly how a modify here was once accepted and then never reached the venue.
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.0.modify(order, new_qty, new_price)
    }
    /// Phase-one teardown seam — raise the stop flags, join nothing. See
    /// `crates/vike-exec/src/execution_engine/client.rs`'s `ExecutionClient::begin_detach`.
    /// ⚠ It MUST be delegated like every other method on this wrapper: a newtype that omits it
    /// inherits the trait's no-op, and the core's raise-all phase then skips this venue entirely
    /// while `detach` below still pays its full wind-down.
    fn begin_detach(&mut self) {
        self.0.begin_detach()
    }
    fn detach(&mut self) {
        self.0.detach()
    }
}

/// Format a price for OANDA (string). NOTE: OANDA enforces per-instrument precision; 5 dp suits
/// the FX majors but JPY/metals differ — precise per-instrument rounding is a refinement.
fn fmt_price(p: f64) -> String {
    format!("{p:.5}")
}

/// Map a vike `TimeInForce` to OANDA's `timeInForce` string.
///
/// The working-order arm consumes this venue's row of the ONE cross-venue TIF authority
/// ([`vike_venues::venue_tif::venue_tif`]) — a future TIF flip is a one-line row edit there.
/// The MARKET arm stays local BY DESIGN: OANDA MARKET orders accept only FOK/IOC, so every
/// non-Ioc TIF coerces to FOK — a genuine venue constraint on the market path, documented as
/// a divergence in `vike_venues::venue_tif`'s module doc rather than modeled in the
/// resting-path table.
fn oanda_tif(tif: TimeInForce, is_market: bool) -> &'static str {
    if is_market {
        match tif {
            TimeInForce::Ioc => "IOC",
            _ => "FOK",
        }
    } else {
        vike_venues::venue_tif::venue_tif(VENUE, tif).wire().unwrap_or("GTC")
    }
}

/// Build the OANDA order-request body from a vike `OrderRequest`.
// `pub` (the `exec` module itself stays private; re-exported `#[doc(hidden)]` at the crate root)
// so `tests/*.rs` integration tests can exercise the real request-body construction directly
// instead of re-deriving it — last resort per the "prefer testing through the public caller"
// rule (there is no public caller: the only call site is the network-calling `run()` below).
// Not venue API.
pub fn build_order_body(req: &OrderRequest) -> serde_json::Value {
    let instrument = to_oanda_instrument(&req.symbol);
    let units = signed_units(req.side, req.qty); // OANDA units: signed base-currency amount
    let mut order = json!({
        "instrument": instrument,
        "units": units.to_string(),
        "clientExtensions": { "id": req.client_order_id },
    });
    let market = !matches!(req.order_type, OrderType::Limit | OrderType::Stop);
    order["timeInForce"] = json!(oanda_tif(req.time_in_force, market));
    match &req.order_type {
        OrderType::Limit => {
            order["type"] = json!("LIMIT");
            if let Some(p) = req.price {
                order["price"] = json!(fmt_price(p));
            }
        }
        OrderType::Stop => {
            order["type"] = json!("STOP");
            if let Some(p) = req.trigger_price {
                order["price"] = json!(fmt_price(p));
            }
        }
        // stop_limit, take_profit and an unrecognised kind are refused by the capability preflight
        // before they get here; this arm is what the old `_ =>` caught
        OrderType::Market
        | OrderType::StopLimit
        | OrderType::TakeProfit
        | OrderType::Unrecognized(_) => {
            order["type"] = json!("MARKET");
        }
    }
    // OANDA GTD needs an explicit gtdTime (epoch seconds, per Accept-Datetime-Format: UNIX).
    if !market
        && req.time_in_force == TimeInForce::Gtd
        && let Some(exp) = req.gtd_expiry
    {
        order["gtdTime"] = json!((exp / 1000).to_string());
    }
    json!({ "order": order })
}

/// Parse an OANDA `orderFillTransaction` into a vike [`FillEvent`].
///
/// `None` when the transaction carries no `id` — the same refusal, for the same reason, as
/// [`crate::stream::fill_from_transaction`] (which see: OANDA's transaction `id` is this venue's
/// only per-fill identity, so there is nothing to synthesize a replay-stable id from).
///
/// ⚠ This function is a near-duplicate of that one — a law spelled twice, differing only in taking
/// `coid` from the request instead of the transaction's `clientExtensions`. Both had the identical
/// `unwrap_or_default()` defect and both are fixed here; MERGING them is a separate change (the coid
/// provenance genuinely differs) and is deliberately not attempted on a type-change PR.
fn parse_fill(coid: &str, fill: &serde_json::Value) -> Option<FillEvent> {
    let s = |k: &str| fill.get(k).and_then(|v| v.as_str());
    let units: f64 = s("units").and_then(|u| u.parse().ok()).unwrap_or(0.0);
    let price: f64 = s("price").and_then(|p| p.parse().ok()).unwrap_or(0.0);
    let commission: f64 = s("commission").and_then(|c| c.parse().ok()).unwrap_or(0.0);
    let ts = s("time").and_then(|t| t.parse::<f64>().ok()).map_or(0, |secs| (secs * 1000.0) as i64);
    let trade_id = match TradeId::new(s("id").unwrap_or_default()) {
        Ok(t) => t,
        Err(_) => {
            tracing::warn!(
                venue = VENUE,
                %coid,
                "orderFillTransaction carries no `id` — dropping the fill rather than folding an \
                 un-dedupable one"
            );
            return None;
        }
    };
    Some(FillEvent {
        trade_id,
        client_order_id: coid.to_string(),
        venue: VENUE.to_string().into(),
        symbol: s("instrument").unwrap_or_default().to_string().into(),
        side: if units >= 0.0 { 1 } else { -1 },
        last_qty: units.abs(),
        last_px: price,
        commission,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    })
}

/// Map an OANDA order-POST response to the vike events it implies (Accepted [+ Filled], or Rejected).
// `pub` (re-exported `#[doc(hidden)]`) — same rationale as `build_order_body`: the only
// production caller is `run()`'s network path, so integration tests need direct access to prove
// the dual-publish / reject-short-circuit contract without a live REST double.
pub fn map_order_response(coid: &str, ts: i64, resp: &serde_json::Value) -> Vec<Event> {
    let mut out = Vec::new();
    if let Some(rej) = resp.get("orderRejectTransaction") {
        let reason = rej.get("rejectReason").and_then(|r| r.as_str()).unwrap_or("rejected");
        out.push(Event::OrderRejected(OrderRejected {
            client_order_id: coid.to_string(),
            reason: reason.to_string().into(),
            ts,
        }));
        return out;
    }
    if let Some(create) = resp.get("orderCreateTransaction") {
        out.push(Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: create.get("id").and_then(|i| i.as_str()).map(Into::into),
            ts,
        }));
    }
    if let Some(fill) = resp.get("orderFillTransaction") {
        out.extend(fill_events(coid, ts, fill));
    }
    out
}

/// The events one inline `orderFillTransaction` implies — shared by the order-POST and the
/// order-REPLACE replies, so an immediate fill folds identically on either path.
///
/// Dual-publish (crypto contract): bare Fill first so the Account folds position/PnL, then the
/// OrderFilled wrap so the FSM applies it — both the same FillEvent. Bare Fill and wrap stand or
/// fall together — an id-less fill publishes NEITHER (the acceptance before it still stands: the
/// order genuinely was accepted).
fn fill_events(coid: &str, ts: i64, fill: &serde_json::Value) -> Vec<Event> {
    let Some(fill) = parse_fill(coid, fill) else { return Vec::new() };
    vec![
        Event::Fill(fill.clone()),
        Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts }),
    ]
}

/// The projected wire units of an order: the vike quantity rounded to whole OANDA units, signed by
/// side (`build_order_body`'s rule, kept in ONE place so a replace sizes exactly as a submit does).
fn signed_units(side: i32, qty: f64) -> i64 {
    let sign = if side >= 0 { 1 } else { -1 };
    (qty.round() as i64) * sign
}

/// Build the body of OANDA's order REPLACE (`PUT /v3/accounts/{id}/orders/@{coid}`): the WHOLE
/// replacing order, because v20 has no partial amend. The venue cancels the resting order and
/// creates this one atomically, under a NEW venue order id.
///
/// ⚠ **The platform identity survives the venue's new id because the replacement carries the SAME
/// `clientExtensions.id`** (`build_order_body` writes `client_order_id` there). That is what keeps
/// `@{coid}` addressing (the next cancel or modify) and the `clientOrderID` echoed on every later
/// fill, so the fill folds into the order the platform already knows. The venue's new order id
/// returns on the `OrderModified` ([`map_replace_response`]).
///
/// `new_qty` / `new_price` are the requested changes (`None` keeps the resting value); the price is
/// the LIMIT price of a limit and the TRIGGER of a stop, the field the order actually rests on
/// (`vike_model::ManagedOrder`'s modify arm makes the same choice). ⚠ `new_qty` is the replacement's
/// TOTAL units: the replacement is a fresh order carrying no execution history (the table's
/// `AmendSemantics::CancelReplace`), and a price-only modify re-sends the RESTING request's qty, so
/// it is not for an order that has partly filled.
///
/// `Err` is the reason a modify cannot be expressed on this venue at all — a market (nothing rests)
/// or an unwired kind, a non-finite or non-positive value — reported as `OrderModifyRejected` by the
/// caller, never sent. `pub` and re-exported `#[doc(hidden)]` for the same reason as
/// [`build_order_body`].
pub fn build_replace_body(
    order: &OrderRequest,
    new_qty: Option<f64>,
    new_price: Option<f64>,
) -> Result<serde_json::Value, String> {
    let resting_price = match order.order_type {
        OrderType::Limit => order.price,
        OrderType::Stop => order.trigger_price,
        _ => {
            return Err(format!(
                "only a resting limit or stop order can be replaced, not {}",
                order.order_type.as_str()
            ));
        }
    };
    let qty = new_qty.unwrap_or(order.qty);
    let price = new_price.or(resting_price);
    if !qty.is_finite() || signed_units(order.side, qty) == 0 {
        return Err(format!("replacement quantity {qty} rounds to zero OANDA units"));
    }
    if !price.is_some_and(|p| p.is_finite() && p > 0.0) {
        return Err(format!("replacement price {price:?} is not a positive number"));
    }
    let mut projected = order.clone();
    projected.qty = qty;
    match order.order_type {
        OrderType::Stop => projected.trigger_price = price,
        _ => projected.price = price,
    }
    Ok(build_order_body(&projected))
}

/// `OrderModifyRejected` for `coid` — NON-terminal: the order keeps its terms.
fn modify_rejected(coid: &str, reason: String, ts: i64) -> Event {
    Event::OrderModifyRejected(OrderModifyRejected {
        client_order_id: coid.to_string(),
        reason: reason.into(),
        ts,
    })
}

/// Map an order-REPLACE reply (`201`: `orderCancelTransaction` for the old order,
/// `orderCreateTransaction` for the new one) to the vike events it implies.
///
/// * **`OrderModified`** for the SAME `coid`, carrying the replacement's venue order id and the
///   units/price the venue ECHOED (venue truth, not the request), so the FSM ends on what rests.
///   One logical order: the venue's cancel of the replaced order never becomes an `OrderCanceled`
///   (`crate::stream::decode_transaction_events` skips a replace-cancel for exactly that reason).
/// * An immediate fill of the replacement (`orderFillTransaction`) dual-publishes after it.
/// * A replacement the venue cancelled at once (`replacingOrderCancelTransaction`, e.g. a GTD time
///   already past) is the order's terminal `OrderCanceled`: the old order is gone and nothing rests.
/// * A reply with no `orderCreateTransaction` is NOT assumed to have worked: `OrderModifyRejected`.
///
/// A non-2xx reply never gets here (`OandaRest::finish` turns it into an error the caller reports as
/// `OrderModifyRejected` with OANDA's `errorMessage`). `pub`, re-exported `#[doc(hidden)]`.
pub fn map_replace_response(coid: &str, ts: i64, resp: &serde_json::Value) -> Vec<Event> {
    if let Some(rej) = resp.get("orderRejectTransaction") {
        let reason = rej.get("rejectReason").and_then(|r| r.as_str()).unwrap_or("rejected");
        return vec![modify_rejected(coid, reason.to_string(), ts)];
    }
    let Some(create) = resp.get("orderCreateTransaction") else {
        return vec![modify_rejected(
            coid,
            format!("no orderCreateTransaction in the replace reply for {coid}"),
            ts,
        )];
    };
    let s = |k: &str| create.get(k).and_then(|v| v.as_str());
    let num = |k: &str| s(k).and_then(|v| v.parse::<f64>().ok());
    let mut out = vec![Event::OrderModified(OrderModified {
        client_order_id: coid.to_string(),
        venue_order_id: s("id").map(Into::into),
        new_qty: num("units").map(f64::abs),
        new_price: num("price"),
        ts,
    })];
    if let Some(fill) = resp.get("orderFillTransaction") {
        out.extend(fill_events(coid, ts, fill));
    }
    if let Some(cancel) = resp.get("replacingOrderCancelTransaction") {
        let reason = cancel.get("reason").and_then(|r| r.as_str()).unwrap_or_default();
        out.push(Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.to_string(),
            reason: reason.to_string().into(),
            ts,
        }));
    }
    if resp.get("orderReissueTransaction").is_some() {
        tracing::warn!(
            venue = VENUE,
            %coid,
            "order-replace reply carries an orderReissueTransaction (a partially filled order was \
             replaced); the reissued order is not modelled and reconcile owns it"
        );
    }
    out
}

/// Advance the shared A3 watermark from an order-POST response's `lastTransactionID`.
// `pub` (re-exported `#[doc(hidden)]`) — the exec-side half of the A3 watermark contract;
// promoted so its monotonic-max / missing-field behavior is integration-testable directly.
pub fn note_last_transaction_id(last_seen: &Arc<AtomicU64>, resp: &serde_json::Value) {
    if let Some(id) =
        resp.get("lastTransactionID").and_then(|v| v.as_str()).and_then(|s| s.parse::<u64>().ok())
    {
        last_seen.fetch_max(id, Ordering::Relaxed);
    }
}

fn run(
    config: OandaConfig,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    last_seen: Arc<AtomicU64>,
) {
    let rest = OandaRest::new(config.api_token.clone());
    let (acct, base) = (config.account_id.clone(), config.rest_base.clone());

    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecCommand::Submit(req) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: req.client_order_id.clone(),
                    ts: req.ts,
                }));
                let body = build_order_body(&req);
                let path = format!("/v3/accounts/{acct}/orders");
                match rest.post_json(&base, &path, &body) {
                    Ok(resp) => {
                        // A3: advance the shared watermark so a reconnect backfills from AFTER
                        // this order's transactions, not from account start.
                        note_last_transaction_id(&last_seen, &resp);
                        for ev in map_order_response(&req.client_order_id, req.ts, &resp) {
                            let _ = events.blocking_send(ev);
                        }
                    }
                    Err(e) => {
                        let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                            client_order_id: req.client_order_id.clone(),
                            reason: e.to_string().into(),
                            ts: req.ts,
                        }));
                    }
                }
            }
            ExecCommand::Cancel { client_order_id: coid, .. } => {
                // A failed/ambiguous cancel must not vanish (audit A2): map every outcome to an
                // event. A genuine venue-side cancel still confirms via the transactions stream.
                let path = format!("/v3/accounts/{acct}/orders/@{coid}/cancel");
                let outcome = match rest.put_empty(&base, &path) {
                    Ok(resp) if resp.get("orderCancelTransaction").is_some() => {
                        CancelOutcome::Canceled
                    }
                    Ok(_) => {
                        CancelOutcome::Rejected(format!("no orderCancelTransaction for {coid}"))
                    }
                    Err(e) => CancelOutcome::Rejected(e.to_string()),
                };
                let _ = events.blocking_send(cancel_event(&coid, outcome));
            }
            // No bulk-cancel path here, and this venue declares none — so `ExecActor` fanned the
            // batch out into the per-id `Cancel`s above before it ever reached this channel, and
            // this arm is unreachable. It exists because a new command variant is exhaustive; the
            // shared helper refuses every id NON-terminally rather than letting a future mis-wiring
            // drop them silently.
            ExecCommand::CancelBatch { client_order_ids, .. } => {
                cancel_batch_undeclared(&events, &client_order_ids)
            }
            // OANDA's native amend is a REPLACE: `PUT /orders/@{coid}` with the replacing order.
            // Every outcome maps to an event: a refusal (local or venue) is the NON-terminal
            // `OrderModifyRejected` and the resting order keeps its terms; success is
            // `OrderModified`, the SAME client order id carrying the venue's NEW order id.
            ExecCommand::Modify { order, new_qty, new_price } => {
                let coid = order.client_order_id.as_str();
                let evs = match build_replace_body(&order, new_qty, new_price) {
                    Err(reason) => vec![modify_rejected(coid, reason, order.ts)],
                    Ok(body) => {
                        let path = format!("/v3/accounts/{acct}/orders/@{coid}");
                        match rest.put_json(&base, &path, &body) {
                            Ok(resp) => {
                                note_last_transaction_id(&last_seen, &resp);
                                map_replace_response(coid, order.ts, &resp)
                            }
                            Err(e) => vec![modify_rejected(coid, e.to_string(), order.ts)],
                        }
                    }
                };
                for ev in evs {
                    let _ = events.blocking_send(ev);
                }
            }
            ExecCommand::Shutdown => break,
        }
    }
}

#[path = "exec_tests.rs"]
#[cfg(test)]
mod exec_tests;
