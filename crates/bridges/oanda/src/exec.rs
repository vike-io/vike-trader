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
//!
//! **The stream's events for an order wait behind the order's own submit.** A market order's fill reaches
//! the engine twice, on the POST's reply (`Accepted`, then the fill) and on the transactions stream (the
//! fill alone: the stream carries no accept), and OANDA writes the stream line before the reply is read.
//! A fill ahead of the `OrderAccepted` meets a `Submitted` order, which the FSM refuses and the engine
//! counts as a terminal dropped on a live order. So the `Submit` arm of [`run`] marks the order before its
//! POST ([`SubmittedSymbols::hold`]), the stream lane keeps back what it decodes for it, and the arm
//! hands that on after its own events ([`SubmittedSymbols::release`]): the engine always sees
//! `Submitted`, `Accepted`, the fill, and the second copy of the fill is the replay it already drops.
//!
//! **A fill carries the symbol its order was SUBMITTED under** ([`crate::symbols`]): the order goes out as
//! `EUR_USD` but the engine drops a fill whose symbol it does not mount, so every fill producer here
//! ([`parse_fill`], [`events_from_transactions`], [`found_events`]) takes the shared [`SubmittedSymbols`].
//!
//! **An ambiguous submit is resolved by client order id, never rejected.** A POST that fails AFTER the
//! request may have left (timeout, reset, a gateway 502/503/504/524, an unreadable reply:
//! `OandaApiError::outcome_unknown`; a 2xx with an unparsable body included) asks `GET /orders/@{coid}`
//! ([`resolve_submit`]): found -> `OrderAccepted` with the venue id, and for a FILLED order its fill
//! (`fillingTransactionID`); OANDA's own "order does not exist" on every attempt of a short ladder AND
//! no transaction of that client id since the pre-POST watermark (a filled market order can be gone
//! from the pending list) -> the terminal "venue confirms order absent" reject; a fill / reject /
//! cancel transaction of that id maps to its events (a CANCELLED order too: accept, then the cancel);
//! anything inconclusive, or still unresolved after [`RESOLVE_BUDGET`] -> NOTHING (the order stays
//! `Submitted` for the stuck-order watchdog and reconcile, as IG's does). `ExecutionClient::confirm` (`ExecActor::with_confirm`, [`confirm_by_coid`]) asks
//! the same order query but NEVER rules absent: its order's POST may still be queued or in flight, so
//! not found / an error is silence for the stuck-order watchdog and reconcile.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::thread;

use serde_json::json;
use vike_bridge_core::exec_actor::{
    CancelOutcome, ConfirmFn, ExecActor, ExecCommand, cancel_batch_undeclared, cancel_event,
};
use vike_bridge_core::transport::VenueApiError;
use vike_catalog::to_oanda_instrument;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderModified,
    OrderModifyRejected, OrderRejected, OrderSubmitted, TradeId,
};
use vike_model::{OrderRequest, OrderType, TimeInForce};

use crate::config::OandaConfig;
use crate::rest::OandaRest;
use crate::symbols::SubmittedSymbols;

const VENUE: &str = "oanda";

/// Live OANDA exec client. `submit`/`cancel` enqueue onto the REST thread (non-blocking); every
/// venue event returns through the core ingest. Dropping it stops both threads.
pub struct OandaExecutionClient {
    actor: ExecActor,
    /// The symbol each order was submitted under, shared with the exec thread, the transactions stream
    /// and the confirm worker: every fill they build is stamped with it (see [`crate::symbols`]).
    symbols: SubmittedSymbols,
}

impl OandaExecutionClient {
    /// Assemble a client from an already-running `actor` and the table its mappers read. `pub` and
    /// re-exported `#[doc(hidden)]` only so an integration test can mount a fake command loop under the
    /// REAL `submit` (which records the submitted symbol); `spawn` is the one production caller.
    #[doc(hidden)]
    pub fn from_parts(actor: ExecActor, symbols: SubmittedSymbols) -> Self {
        Self { actor, symbols }
    }

    /// Hand this client the HALT sentinel it watches: the venue's start passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live start calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self { actor: self.actor.with_halt_path(path), symbols: self.symbols }
    }

    pub fn spawn(config: OandaConfig, events: EventSender) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let symbols = SubmittedSymbols::default();
        // Shared audit-A3 watermark: the highest transaction id seen on EITHER the POST path
        // (below) or the transactions stream. The stream backfills `/transactions/sinceid` from
        // this on reconnect. 0 = nothing seen yet (skip backfill).
        let last_seen = Arc::new(AtomicU64::new(0));
        // Active re-query by client order id (`confirm`): a fresh short-lived client on the confirm
        // worker, so it never queues behind the exec thread's command loop.
        let confirm: ConfirmFn = {
            let (config, symbols) = (config.clone(), symbols.clone());
            Arc::new(move |coid: &str| {
                let rest = OandaRest::new(config.api_token.clone());
                let evs = confirm_by_coid(&rest, &config, coid, vike_model::now_ms(), &symbols);
                symbols.settle(&evs);
                evs
            })
        };
        let actor = ExecActor::spawn("oanda-exec", events.clone(), {
            let (config, events, last_seen) = (config.clone(), events.clone(), last_seen.clone());
            let symbols = symbols.clone();
            move |rx| run(config, events, rx, last_seen, symbols)
        })
        .with_confirm(confirm);
        // Background thread: the transactions stream (delayed LIMIT/STOP fills + cancels the order
        // POST can't carry). ExecActor flag-stops + joins it on teardown (deterministic).
        let stream_join = thread::Builder::new()
            .name("oanda-stream".into())
            .spawn({
                let (stop, symbols) = (stop.clone(), symbols.clone());
                move || crate::stream::stream_transactions(config, events, stop, last_seen, symbols)
            })
            .expect("spawn oanda-stream thread");
        Self::from_parts(actor.with_background(stop, stream_join), symbols)
    }
}

impl ExecutionClient for OandaExecutionClient {
    /// The submitted symbol is recorded BEFORE the order is queued, so no fill of it can be built first.
    fn submit(&mut self, request: &OrderRequest) {
        self.symbols.record(&request.client_order_id, &request.symbol);
        self.actor.submit(request)
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.actor.cancel(client_order_id)
    }
    /// Native amend = OANDA's order REPLACE (see [`build_replace_body`]). ⚠ It MUST be delegated
    /// like every other method on this wrapper: a newtype that omits it inherits the trait's no-op,
    /// which is exactly how a modify here was once accepted and then never reached the venue.
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.actor.modify(order, new_qty, new_price)
    }
    /// The venue-side answer to the core's `Command::ConfirmOrder` (and the gatewayhub's
    /// `ResolveOrders`): [`confirm_by_coid`] on the actor's confirm worker. ⚠ MUST be delegated like
    /// every other method: the trait default is a no-op and `bridge_trade::COID_LOOKUP` says `true`.
    fn confirm(&mut self, client_order_id: &str) {
        self.actor.confirm(client_order_id)
    }
    /// Phase-one teardown seam — raise the stop flags, join nothing. See
    /// `crates/vike-exec/src/execution_engine/client.rs`'s `ExecutionClient::begin_detach`.
    /// ⚠ It MUST be delegated like every other method on this wrapper: a newtype that omits it
    /// inherits the trait's no-op, and the core's raise-all phase then skips this venue entirely
    /// while `detach` below still pays its full wind-down.
    fn begin_detach(&mut self) {
        self.actor.begin_detach()
    }
    fn detach(&mut self) {
        self.actor.detach()
    }
    /// An order a previous session left resting: the engine restored it under `symbol`, so its later fill
    /// must carry that symbol. Pure table write, returns at once (the fold-thread contract).
    fn adopt_restored(&mut self, orders: &[(String, String)]) {
        for (coid, symbol) in orders {
            self.symbols.record(coid, symbol);
        }
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
fn parse_fill(
    coid: &str,
    fill: &serde_json::Value,
    symbols: &SubmittedSymbols,
) -> Option<FillEvent> {
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
        symbol: symbols.label(coid, s("instrument").unwrap_or_default()).into(),
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
pub fn map_order_response(
    coid: &str,
    ts: i64,
    resp: &serde_json::Value,
    symbols: &SubmittedSymbols,
) -> Vec<Event> {
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
        out.extend(fill_events(coid, ts, fill, symbols));
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
fn fill_events(
    coid: &str,
    ts: i64,
    fill: &serde_json::Value,
    symbols: &SubmittedSymbols,
) -> Vec<Event> {
    let Some(fill) = parse_fill(coid, fill, symbols) else { return Vec::new() };
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
pub fn map_replace_response(
    coid: &str,
    ts: i64,
    resp: &serde_json::Value,
    symbols: &SubmittedSymbols,
) -> Vec<Event> {
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
        out.extend(fill_events(coid, ts, fill, symbols));
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

/// The pauses between a by-client-id lookup's attempts when OANDA does not (yet) show the order: a
/// request that timed out may still be in flight at the venue, so ONE miss is not "absent". Three
/// attempts on the exec thread (ambiguous submits only); each read is bounded by
/// [`crate::rest::LOOKUP_TIMEOUT`] and the whole resolution by [`RESOLVE_BUDGET`].
const LOOKUP_BACKOFF: [std::time::Duration; 2] =
    [std::time::Duration::from_secs(1), std::time::Duration::from_secs(2)];

/// ONE deadline for every step of [`resolve_submit`] (the ladder, the transactions scan, the fill
/// read): a step that STARTS after it is skipped as inconclusive. Unbounded, the steps add up to
/// ~28 s (3 x 5 s order reads + 3 s pauses + a 5 s scan + a 5 s fill read) with a cancel or modify
/// queued behind them; bounded, the worst case is this plus the ONE read in flight at expiry
/// ([`crate::rest::LOOKUP_TIMEOUT`]): ~17 s.
const RESOLVE_BUDGET: std::time::Duration = std::time::Duration::from_secs(12);

/// How long [`ensure_watermark`] leaves a failed `GET /summary` alone before asking again, so a
/// dead summary endpoint costs one 5 s read a minute, not one per submit.
const WATERMARK_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// An order OANDA has, in whatever state: its venue id, its `state` and (when FILLED) the id of the
/// transaction that filled it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FoundOrder {
    id: String,
    state: String,
    filling_txn: Option<String>,
}

/// `GET /v3/accounts/{id}/orders/@{coid}` (the order's `clientExtensions.id`): `Ok(Some(order))`
/// when OANDA has the order in ANY state (pending, filled, cancelled), `Ok(None)` only on OANDA's
/// own "order does not exist", `Err` for everything inconclusive.
fn query_order(
    rest: &OandaRest,
    config: &OandaConfig,
    coid: &str,
) -> Result<Option<FoundOrder>, VenueApiError> {
    let path = format!("/v3/accounts/{}/orders/@{coid}", config.account_id);
    let inconclusive = |msg: String| VenueApiError { code: 0, msg };
    match rest.get_order(&config.rest_base, &path) {
        Ok(None) => Ok(None),
        Ok(Some(resp)) => found_order_of(&resp)
            .map(Some)
            .ok_or_else(|| inconclusive(format!("order lookup for {coid} carries no order id"))),
        Err(e) => Err(inconclusive(e.to_string())),
    }
}

/// The order in an order-lookup reply (`{"order": {"id": .., "state": .., "fillingTransactionID": ..}}`).
fn found_order_of(resp: &serde_json::Value) -> Option<FoundOrder> {
    let order = resp.get("order")?;
    let s = |k: &str| order.get(k).and_then(|v| v.as_str()).map(str::to_string);
    Some(FoundOrder {
        id: s("id")?,
        state: s("state").unwrap_or_default(),
        filling_txn: s("fillingTransactionID"),
    })
}

/// Retry `query` over `pauses` while it does not show the order (`Ok(None)` or `Err`); the first
/// `Ok(Some(_))` wins, otherwise the LAST answer stands. `sleep` is injected so the ladder is
/// assertable without waiting.
fn lookup_with_ladder<T>(
    pauses: &[std::time::Duration],
    mut query: impl FnMut() -> Result<Option<T>, VenueApiError>,
    mut sleep: impl FnMut(std::time::Duration),
) -> Result<Option<T>, VenueApiError> {
    let mut last = query();
    for pause in pauses {
        if matches!(last, Ok(Some(_))) {
            break;
        }
        sleep(*pause);
        last = query();
    }
    last
}

/// The events for an order OANDA reports: `OrderAccepted` with its venue id, and for a `FILLED`
/// order the fill too (read from its `fillingTransactionID`): a market order fills at once and the
/// stream may have delivered nothing while the gateway was down. A fill that cannot be read (or
/// that `deadline` leaves no time for) stays the stream's / reconcile's (the accept alone is still
/// true). A CANCELLED order is NOT mapped here: a replaced order is CANCELLED too and shares the
/// client id of the live replacement, so only [`resolve_submit`] (a FIRST submit, where nothing was
/// replaced) reads its cancel, from the order's own transactions.
fn found_events(
    rest: &OandaRest,
    config: &OandaConfig,
    coid: &str,
    ts: i64,
    found: &FoundOrder,
    deadline: std::time::Instant,
    symbols: &SubmittedSymbols,
) -> Vec<Event> {
    let mut out = vec![Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: Some(found.id.clone().into()),
        ts,
    })];
    if found.state == "FILLED"
        && std::time::Instant::now() < deadline
        && let Some(txn) = &found.filling_txn
    {
        let path = format!("/v3/accounts/{}/transactions/{txn}", config.account_id);
        match rest.get_lookup(&config.rest_base, &path, "") {
            Ok(resp) => match resp.get("transaction") {
                Some(t) if t.get("type").and_then(|x| x.as_str()) == Some("ORDER_FILL") => {
                    out.extend(fill_events(coid, ts, t, symbols));
                }
                _ => tracing::warn!(
                    venue = VENUE, %coid, %txn,
                    "the filling transaction is not an ORDER_FILL; the stream / reconcile carry the fill"
                ),
            },
            Err(e) => tracing::warn!(
                venue = VENUE, %coid, %txn, error = %e,
                "FILLED order's filling transaction could not be read; the stream / reconcile carry the fill"
            ),
        }
    }
    out
}

/// The watchdog's / gatewayhub's re-query by client order id ([`ExecActor::with_confirm`]). It can
/// run while the order's POST is still queued behind a slow ambiguous ladder or in flight, so "not
/// found" proves NOTHING here: found -> the order's events, not found or any error -> NO event (the
/// stuck-order watchdog and reconcile keep the order). The terminal "venue confirms absent" belongs
/// to [`resolve_submit`] alone, where the POST certainly went out.
fn confirm_by_coid(
    rest: &OandaRest,
    config: &OandaConfig,
    coid: &str,
    ts: i64,
    symbols: &SubmittedSymbols,
) -> Vec<Event> {
    match query_order(rest, config, coid) {
        Ok(Some(found)) => found_events(
            rest,
            config,
            coid,
            ts,
            &found,
            std::time::Instant::now() + RESOLVE_BUDGET,
            symbols,
        ),
        Ok(None) => Vec::new(),
        Err(e) => {
            tracing::warn!(venue = VENUE, %coid, error = %e, "confirm lookup inconclusive; reporting nothing");
            Vec::new()
        }
    }
}

/// The transactions after `floor` (`GET /transactions/sinceid`) whose client id is `coid`.
fn transactions_of(
    rest: &OandaRest,
    config: &OandaConfig,
    floor: u64,
    coid: &str,
) -> Result<Vec<serde_json::Value>, crate::rest::OandaApiError> {
    let path = format!("/v3/accounts/{}/transactions/sinceid", config.account_id);
    let resp = rest.get_lookup(&config.rest_base, &path, &format!("id={floor}"))?;
    let all = resp.get("transactions").and_then(|t| t.as_array()).cloned().unwrap_or_default();
    Ok(all.into_iter().filter(|t| transaction_is_ours(t, coid)).collect())
}

fn transaction_is_ours(t: &serde_json::Value, coid: &str) -> bool {
    t.pointer("/clientExtensions/id").and_then(|c| c.as_str()) == Some(coid)
        || t.get("clientOrderID").and_then(|c| c.as_str()) == Some(coid)
}

/// Whether a v20 transaction `type` is the venue REFUSING THE ORDER ITSELF: `MARKET_ORDER_REJECT`,
/// `LIMIT_ORDER_REJECT`, `STOP_ORDER_REJECT`, `MARKET_IF_TOUCHED_ORDER_REJECT`,
/// `TAKE_PROFIT_ORDER_REJECT`, `STOP_LOSS_ORDER_REJECT`, `TRAILING_STOP_LOSS_ORDER_REJECT`,
/// `GUARANTEED_STOP_LOSS_ORDER_REJECT`, `FIXED_PRICE_ORDER_REJECT` all end in `_ORDER_REJECT`. A
/// bare `_REJECT` suffix also matches `ORDER_CANCEL_REJECT` and `*_CLIENT_EXTENSIONS_MODIFY_REJECT`:
/// a refused cancel / extension edit of a LIVE order sharing the client id, which must never read
/// as that order's terminal rejection.
fn is_order_reject(kind: &str) -> bool {
    kind.ends_with("_ORDER_REJECT")
}

/// The events one order's own transactions imply (market orders fill at once and may no longer
/// exist as a pending order, so `/orders/@coid` can miss them): a rejection of the ORDER itself
/// ([`is_order_reject`]) -> the terminal `OrderRejected`; creation / fill / cancel ->
/// `OrderAccepted` (venue id) first, then the fill (dual-published) or the cancel. Empty = none of
/// the transactions says anything about the order.
fn events_from_transactions(
    coid: &str,
    ts: i64,
    txs: &[serde_json::Value],
    symbols: &SubmittedSymbols,
) -> Vec<Event> {
    let s = |t: &serde_json::Value, k: &str| t.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let mut out = Vec::new();
    let mut accepted = false;
    let mut accept = |out: &mut Vec<Event>, id: Option<String>| {
        if !std::mem::replace(&mut accepted, true) {
            out.push(Event::OrderAccepted(OrderAccepted {
                client_order_id: coid.to_string(),
                venue_order_id: id.map(Into::into),
                ts,
            }));
        }
    };
    for t in txs {
        let kind = s(t, "type").unwrap_or_default();
        if is_order_reject(&kind) {
            let reason = s(t, "rejectReason").unwrap_or_else(|| "rejected".to_string());
            return vec![Event::OrderRejected(OrderRejected {
                client_order_id: coid.to_string(),
                reason: reason.into(),
                ts,
            })];
        }
        match kind.as_str() {
            "ORDER_FILL" => {
                accept(&mut out, s(t, "orderID"));
                out.extend(fill_events(coid, ts, t, symbols));
            }
            "ORDER_CANCEL" if !crate::stream::is_replace_cancel(t) => {
                accept(&mut out, s(t, "orderID"));
                out.push(Event::OrderCanceled(OrderCanceled {
                    client_order_id: coid.to_string(),
                    reason: s(t, "reason").unwrap_or_default().into(),
                    ts,
                }));
            }
            k if k.ends_with("_ORDER") => accept(&mut out, s(t, "id")),
            _ => {}
        }
    }
    out
}

/// Resolve an ambiguous submit by client order id, on the exec thread right after its POST failed
/// ambiguously (the POST certainly went out; nothing else is queued ahead of it). `floor` is the
/// account's last transaction id read BEFORE that POST, so every transaction of this order is after
/// it. Found -> the order's events; "order does not exist" on every attempt AND no transaction of
/// this client id since `floor` (a filled market order can be gone from the pending list) -> the
/// terminal "venue confirms order absent" reject; anything inconclusive (including no `floor`, or
/// `budget` spent: every step that would START after it is skipped, or a failed read) -> NOTHING
/// ([`unresolved`]): the order stays `Submitted` for the watchdog and reconcile, never a false
/// terminal and never an id-less accept.
///
/// A FOUND order that is CANCELLED is read from its own transactions ([`events_from_transactions`]:
/// accept, then the cancel and its reason): a market FOK/IOC the venue cancelled (no liquidity,
/// market closed) whose POST reply was lost would otherwise sit `Accepted` forever, because the
/// stream's `ORDER_CANCEL` arrived while the order was still `Submitted` and the FSM dropped it.
/// That is safe HERE and not in [`confirm_by_coid`]: this is the FIRST submit on the serialized
/// exec thread, so no replace of this client id can have happened; `events_from_transactions`
/// still skips a replace-cancel.
fn resolve_submit(
    rest: &OandaRest,
    config: &OandaConfig,
    floor: u64,
    pauses: &[std::time::Duration],
    budget: std::time::Duration,
    coid: &str,
    ts: i64,
    symbols: &SubmittedSymbols,
) -> Vec<Event> {
    use std::time::Instant;
    let deadline = Instant::now() + budget;
    let spent = || Instant::now() >= deadline;
    let found = lookup_with_ladder(
        pauses,
        || {
            if spent() {
                return Err(VenueApiError { code: 0, msg: "resolve budget spent".into() });
            }
            query_order(rest, config, coid)
        },
        |pause| thread::sleep(pause.min(deadline.saturating_duration_since(Instant::now()))),
    );
    match found {
        Ok(Some(order)) => {
            let cancelled = if order.state == "CANCELLED" && floor > 0 && !spent() {
                transactions_of(rest, config, floor, coid)
                    .map(|txs| events_from_transactions(coid, ts, &txs, symbols))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            if cancelled.is_empty() {
                found_events(rest, config, coid, ts, &order, deadline, symbols)
            } else {
                cancelled
            }
        }
        Ok(None) if floor > 0 && !spent() => match transactions_of(rest, config, floor, coid) {
            Ok(txs) => match events_from_transactions(coid, ts, &txs, symbols) {
                evs if !evs.is_empty() => evs,
                _ => vec![vike_bridge_core::resolve_ambiguous_submit(coid, ts, Ok(None))],
            },
            Err(e) => unresolved(coid, &e.to_string()),
        },
        Ok(None) => unresolved(
            coid,
            "cannot rule a fill out (no transaction watermark, or the resolve budget is spent)",
        ),
        Err(e) => unresolved(coid, &e.to_string()),
    }
}

/// An inconclusive resolution emits NOTHING (IG's `submit_outcome` does the same): the order stays
/// `Submitted`, so the stuck-order watchdog and reconcile still see it. An id-less `OrderAccepted`
/// would take a possibly never-received order out of the watchdog's view (it watches only
/// Initialized/Submitted).
fn unresolved(coid: &str, why: &str) -> Vec<Event> {
    tracing::warn!(
        venue = VENUE, %coid, %why,
        "submit outcome unknown and the lookup is inconclusive; emitting nothing, the order stays Submitted"
    );
    Vec::new()
}

/// Make sure the shared A3 watermark is not 0: read the account's last transaction id
/// (`GET /v3/accounts/{id}/summary`) before a submit's POST while it is still 0, so the floor
/// [`resolve_submit`] scans from is real. A failure is logged and leaves it 0 (then nothing is ruled
/// absent). After a failure (or a reply with no id) it asks again only after [`WATERMARK_RETRY`]:
/// `retry_at` is the exec thread's own memory of that, so a dead endpoint does not put a 5 s read
/// in front of every submit.
fn ensure_watermark(
    rest: &OandaRest,
    config: &OandaConfig,
    last_seen: &Arc<AtomicU64>,
    retry_at: &mut Option<std::time::Instant>,
) {
    if last_seen.load(Ordering::Relaxed) != 0
        || retry_at.is_some_and(|t| std::time::Instant::now() < t)
    {
        return;
    }
    let path = format!("/v3/accounts/{}/summary", config.account_id);
    match rest.get_lookup(&config.rest_base, &path, "") {
        Ok(resp) => note_last_transaction_id(last_seen, &resp),
        Err(e) => tracing::warn!(
            venue = VENUE, error = %e,
            "could not read the account's last transaction id; an ambiguous submit cannot be ruled absent"
        ),
    }
    *retry_at = (last_seen.load(Ordering::Relaxed) == 0)
        .then(|| std::time::Instant::now() + WATERMARK_RETRY);
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
    symbols: SubmittedSymbols,
) {
    let rest = OandaRest::new(config.api_token.clone());
    let (acct, base) = (config.account_id.clone(), config.rest_base.clone());
    let mut watermark_retry_at = None;
    // Publish what an arm produced; an order it ENDS leaves the submitted-symbol table.
    let emit = |evs: Vec<Event>| {
        symbols.settle(&evs);
        for ev in evs {
            let _ = events.blocking_send(ev);
        }
    };

    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecCommand::Submit(req) => {
                // The stream's events for this order wait until the arm is done (`release` below):
                // OANDA writes a market order's fill to the stream before this thread has read the
                // POST's reply, and a fill ahead of the `OrderAccepted` is refused by the FSM.
                symbols.hold(&req.client_order_id);
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: req.client_order_id.clone(),
                    ts: req.ts,
                }));
                let body = build_order_body(&req);
                let path = format!("/v3/accounts/{acct}/orders");
                // The floor an ambiguous outcome is ruled absent from: read BEFORE the POST so every
                // transaction of this order is after it (the stream may advance the watermark
                // past the order's own transactions before the lookup runs).
                ensure_watermark(&rest, &config, &last_seen, &mut watermark_retry_at);
                let floor = last_seen.load(Ordering::Relaxed);
                match rest.post_json(&base, &path, &body) {
                    Ok(resp) => {
                        // A3: advance the shared watermark so a reconnect backfills from AFTER
                        // this order's transactions, not from account start.
                        note_last_transaction_id(&last_seen, &resp);
                        emit(map_order_response(&req.client_order_id, req.ts, &resp, &symbols));
                    }
                    // The request may have reached OANDA (timeout / reset after the write, a gateway
                    // 5xx): the order may be LIVE or filled, so ask by client id instead of rejecting.
                    Err(e) if e.outcome_unknown() => {
                        tracing::warn!(
                            venue = VENUE, coid = %req.client_order_id, error = %e,
                            "order submit outcome unknown - resolving by client order id"
                        );
                        emit(resolve_submit(
                            &rest,
                            &config,
                            floor,
                            &LOOKUP_BACKOFF,
                            RESOLVE_BUDGET,
                            &req.client_order_id,
                            req.ts,
                            &symbols,
                        ));
                    }
                    Err(e) => emit(vec![Event::OrderRejected(OrderRejected {
                        client_order_id: req.client_order_id.clone(),
                        reason: e.to_string().into(),
                        ts: req.ts,
                    })]),
                }
                // Whatever the arm produced (an inconclusive resolution produces nothing) is out:
                // the stream's events for the order follow it.
                symbols.release(&req.client_order_id, &emit);
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
                emit(vec![cancel_event(&coid, outcome)]);
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
                                map_replace_response(coid, order.ts, &resp, &symbols)
                            }
                            Err(e) => vec![modify_rejected(coid, e.to_string(), order.ts)],
                        }
                    }
                };
                emit(evs);
            }
            ExecCommand::Shutdown => break,
        }
    }
}

#[path = "exec_tests.rs"]
#[cfg(test)]
mod exec_tests;
