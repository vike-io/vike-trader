//! OANDA exec half — `ExecutionClient` over the v20 orders API.
//!
//! Like FXCM, order I/O runs on ONE dedicated thread (blocking REST off the single-writer core);
//! `submit`/`cancel`/`modify` enqueue, and venue events return through the ingest via
//! [`EventSender`]. Orders carry `clientExtensions.id = order_client_id`, so cancel targets
//! `@{order_client_id}` (no id-tracking map needed).
//!
//! **Modify is OANDA's order REPLACE** ([`build_replace_body`], [`map_replace_response`]): the venue
//! cancels the resting order and creates a new one under a NEW venue order id. The replacement carries
//! the SAME `clientExtensions.id`, so the platform keeps ONE order: `@{order_client_id}` still
//! addresses it, later fills name it, and the new venue id returns on `OrderModified`. The replaced
//! order's `ORDER_CANCEL` is dropped by the stream decode (`crate::execution::user_data`) so it never reads as a
//! cancel of the order. Every modify outcome maps to an event, a refusal to the non-terminal
//! `OrderModifyRejected`.
//!
//! MARKET fills come back inline on the POST response; delayed
//! LIMIT/STOP/MARKET_IF_TOUCHED fills + cancels arrive on the transactions stream (a 2nd reader thread — see
//! [`crate::execution::user_data`]). Audit A3: a fill/cancel that lands while no transactions stream is open
//! is recovered by backfilling `/transactions/sinceid` on open, from a floor the stream lane keeps to itself:
//! see [`crate::execution::user_data`]. The shared `last_seen` watermark is advanced here (from each POST's `lastTransactionID`) and
//! by the stream (from each transaction `id`), and only an ambiguous submit reads it, as the floor of its scan
//! ([`resolve_submit`]): a POST's id can lie past a resting fill nobody read, so it must not move the stream's floor.
//!
//! **An order is written on its instrument's OWN grid** ([`crate::instrument`]): the price with exactly
//! the instrument's `displayPrecision` decimals (5 for `EUR_USD`, 3 for `USD_JPY`), the units with its
//! `tradeUnitsPrecision`, both read from the account's instrument list on the first order that needs them.
//! An instrument whose grid cannot be read is refused with a sentence (`OrderRejected`, or the non-terminal
//! `OrderModifyRejected` for a replace), never written on a guessed one.
//!
//! **The stream's events for an order wait behind the order's own submit.** A market order's fill reaches
//! the engine twice, on the POST's reply (`Accepted`, then the fill) and on the transactions stream (the
//! fill alone: the stream carries no accept), and OANDA writes the stream line before the reply is read.
//! A fill ahead of the `OrderAccepted` meets a `Submitted` order, which the FSM refuses and the engine
//! counts as a terminal dropped on a live order. So the `Submit` arm of [`run_on`] holds the shared fence
//! (`vike_bridge_core::exec_actor::submit_fence::SubmitFence::enter_with`, owner ruling B-17) before its POST, the
//! stream lane delivers through it, and what it kept back goes out after the arm's own events, whichever way the
//! arm leaves: the engine always sees `Submitted`, `Accepted`, the fill, and the second copy of the fill is the
//! replay it already drops.
//!
//! **A fill carries the symbol its order was SUBMITTED under** ([`crate::execution::submitted_symbols`]): the order goes out as
//! `EUR_USD` but the engine drops a fill whose symbol it does not mount, so every fill producer here
//! ([`fill_events`], [`events_from_transactions`], [`found_events`]) takes the shared [`OrderClientIdSymbols`].
//!
//! **An ambiguous submit is resolved by order client id, never rejected.** A POST that fails AFTER the
//! request may have left (timeout, reset, a gateway 502/503/504/524, an unreadable reply:
//! `VenueApiHttpError::outcome_unknown`; a 2xx with an unparsable body included) asks `GET /orders/@{order_client_id}`
//! ([`resolve_submit`]): found -> `OrderAccepted` with the venue id, and for a FILLED order its fill
//! (`fillingTransactionID`); OANDA's own "order does not exist" on every attempt of a short ladder AND
//! no transaction of that client id since the pre-POST watermark (a filled market order can be gone
//! from the pending list) -> the terminal "venue confirms order absent" reject; a fill / reject /
//! cancel transaction of that id maps to its events (a CANCELLED order too: accept, then the cancel);
//! anything inconclusive, or still unresolved after [`RESOLVE_BUDGET`] -> NOTHING (the order stays
//! `Submitted` for the stuck-order watchdog and reconcile, as IG's does). `ExecutionClient::confirm` (`ExecutionClientActor::with_confirm`, [`confirm_by_order_client_id`]) asks
//! the same order query but NEVER rules absent: its order's POST may still be queued or in flight, so
//! not found / an error is silence for the stuck-order watchdog and reconcile.

pub(crate) mod funding;
pub(crate) mod resync;
pub(crate) mod submitted_symbols;
pub(crate) mod user_data;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::thread;

use serde_json::json;
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::exec_actor::{
    ConfirmFn, ExecutionClientActor, ExecutionClientActorCancelOutcome,
    ExecutionClientActorCommand, ExecutionClientActorLateCloses, cancel_batch_undeclared,
    cancel_event, late_close_size, too_old_to_send,
};
use vike_bridge_core::per_symbol::OrderClientIdSymbols;
use vike_bridge_core::rest::amend_answer_time;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::http_error::VenueApiHttpError;
use vike_bridge_core::venue_link::VenueLink;
use vike_catalog::to_oanda_instrument;
use vike_execution::EventSender;
use vike_model::events::{
    Event, OrderAccepted, OrderCanceled, OrderModified, OrderModifyRejected, OrderRejected,
    OrderSubmitted,
};
use vike_model::{OrderRequest, OrderType, TimeInForce};

use crate::account::modes::AccountHedging;
use crate::config::OandaConfig;
use crate::execution::submitted_symbols::{settle, submitted_symbols};
use crate::instrument::{GridCache, InstrumentGrid};
use crate::transport::{OANDA_HTTP, OandaTransport};

const VENUE: &str = "oanda";

/// This venue's two hooks on an already-running `actor` (`vike_bridge_core::execution_client::hooks`): every
/// submit records the symbol its order was submitted under BEFORE the actor reads the order, so no fill of it
/// can be built first, and `adopt_restored` teaches the symbol a previous session's resting order was restored
/// under (a pure table write that returns at once, the fold-thread contract). `symbols` is the table the exec
/// thread, the transactions stream and the confirm worker stamp every fill from ([`crate::execution::submitted_symbols`]). `pub` and
/// re-exported `#[doc(hidden)]` only so an integration test can mount a fake command loop under the REAL hooks;
/// [`spawn_execution_client_reporting`] is the one production caller.
#[doc(hidden)]
#[must_use]
pub fn with_symbol_hooks(
    actor: ExecutionClientActor,
    symbols: &Arc<OrderClientIdSymbols>,
) -> ExecutionClientActor {
    let (submitted, restored) = (symbols.clone(), symbols.clone());
    actor
        .with_before_submit(Arc::new(move |request: &mut OrderRequest| {
            submitted.record(&request.order_client_id, &request.symbol);
        }))
        .with_adopt_restored(Arc::new(move |orders: &[vike_execution::RestoredOrder]| {
            restored.adopt(orders);
        }))
}

/// The live OANDA exec client: the shared [`ExecutionClientActor`] itself, with this venue's hooks ([`with_symbol_hooks`]) and
/// its confirm. `submit`/`cancel`/`modify` enqueue onto the REST thread (non-blocking); every venue event returns
/// through the core ingest. Dropping it stops both threads. The HALT sentinel it watches is handed in by
/// [`ExecutionClientActor::with_halt_path`] (`AdapterInputs::process.halt_path`;
/// `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that every live start calls it).
pub fn spawn_execution_client(config: OandaConfig, events: EventSender) -> ExecutionClientActor {
    spawn_execution_client_reporting(config, events, VenueLink::default())
}

/// [`spawn_execution_client`] whose transactions stream states itself on `link`, the account's [`VenueLink`]
/// (`crate::execution::user_data`'s `stream_transactions`): what a venue start passes, so the status verb can say the stream is down.
pub fn spawn_execution_client_reporting(
    config: OandaConfig,
    events: EventSender,
    link: VenueLink,
) -> ExecutionClientActor {
    let stop = Arc::new(AtomicBool::new(false));
    let symbols = submitted_symbols();
    // The exec path's watermark: the highest transaction id seen on EITHER the POST path (below)
    // or the transactions stream, the floor an ambiguous submit scans from. 0 = nothing seen yet
    // (`ensure_watermark` asks). The stream's own backfill never reads it (`crate::execution::user_data`).
    let last_seen = Arc::new(AtomicU64::new(0));
    // Active re-query by order client id (`confirm`): a fresh short-lived client on the confirm
    // worker, so it never queues behind the exec thread's command loop.
    let confirm: ConfirmFn = {
        let (config, symbols) = (config.clone(), symbols.clone());
        Arc::new(move |order_client_id: &str| {
            let rest = OandaTransport::new(config.api_token.expose().clone());
            let evs = confirm_by_order_client_id(
                &rest,
                &config,
                order_client_id,
                vike_model::now_ms(),
                &symbols,
            );
            settle(&symbols, &evs);
            evs
        })
    };
    // The stream's events for an order wait behind its submit (owner ruling B-17): the exec thread's submit arm
    // holds the fence, the transactions stream (live lines and the A3 backfill) delivers through it.
    let fence = Arc::new(SubmitFence::new(events.clone()));
    let actor = ExecutionClientActor::spawn("oanda-exec", events.clone(), {
        let (config, events, last_seen) = (config.clone(), events.clone(), last_seen.clone());
        let (symbols, fence) = (symbols.clone(), Arc::clone(&fence));
        move |rx| {
            run_on(
                config,
                events,
                rx,
                last_seen,
                symbols,
                fence,
                GridCache::default(),
                AccountHedging::default(),
            )
        }
    })
    .with_confirm(confirm);
    // Background thread: the transactions stream (delayed LIMIT/STOP fills + cancels the order
    // POST can't carry). ExecutionClientActor flag-stops + joins it on teardown (deterministic).
    let stream_join = thread::Builder::new()
        .name("oanda-stream".into())
        .spawn({
            let (stop, symbols) = (stop.clone(), symbols.clone());
            move || {
                crate::execution::user_data::stream_transactions(
                    config, events, stop, last_seen, symbols, fence, link,
                )
            }
        })
        .expect("spawn oanda-stream thread");
    with_symbol_hooks(actor.with_background(stop, stream_join), &symbols)
}

/// Format a price for OANDA (string) on the instrument's own grid: exactly its `displayPrecision`
/// decimals (OANDA refuses more: `PRICE_PRECISION_EXCEEDED`), an off-grid price rounded to the passive
/// side for `side` ([`InstrumentGrid::price`]).
fn fmt_price(grid: &InstrumentGrid, side: i32, p: f64) -> String {
    grid.price(side, p)
}

/// Map a vike `TimeInForce` to OANDA's `timeInForce` string.
///
/// The working-order arm consumes this venue's row of the ONE cross-venue TIF authority
/// ([`vike_venues::venue_time_in_force::venue_time_in_force`]) — a future TIF flip is a one-line row edit there.
/// The MARKET arm stays local BY DESIGN: OANDA MARKET orders accept only FOK/IOC, so every
/// non-Ioc TIF coerces to FOK — a genuine venue constraint on the market path, documented as
/// a divergence in `vike_venues::venue_time_in_force`'s module doc rather than modeled in the
/// resting-path table.
fn oanda_time_in_force(tif: TimeInForce, is_market: bool) -> &'static str {
    if is_market {
        match tif {
            TimeInForce::Ioc => "IOC",
            _ => "FOK",
        }
    } else {
        vike_venues::venue_time_in_force::venue_time_in_force(VENUE, tif).wire().unwrap_or("GTC")
    }
}

/// Whether `req` goes out as a LIMIT, a STOP or a MARKET_IF_TOUCHED order (it RESTS at the venue); a
/// market order does not, and every other kind is refused before a body is built ([`unbuilt_kind`]).
/// [`build_order_body_on`]'s one split.
fn rests_at_venue(req: &OrderRequest) -> bool {
    matches!(req.order_type, OrderType::Limit | OrderType::Stop | OrderType::TakeProfit)
}

/// Why `req` is refused before anything is read or sent (a terminal `OrderRejected` with this text): an
/// auction lifetime (v20 has no auction order: a market-on-open would go out as an immediate FOK market, a
/// limit-on-close as a GTC limit), or a kind this bridge builds no order for. Built: `market`, `limit`,
/// `stop`, and a `take_profit` WITHOUT a limit price (market-if-touched, OANDA's `MARKET_IF_TOUCHED`).
/// A `take_profit` with one (limit-if-touched) is refused: OANDA's `priceBound` is a worst-fill bound
/// that cancels the order, not a limit that rests. `stop_limit`, `market_to_limit` and an unrecognised
/// word used to go out as a MARKET order. The capability preflight refuses each first (`vike_venues`'s
/// `venue_caps` row `OANDA`, and its `venue_time_in_force` row); this is the adapter's own guard.
fn unbuilt_kind(req: &OrderRequest) -> Option<String> {
    if req.time_in_force.is_auction() {
        return Some(format!(
            "oanda has no auction order (time in force `{}`); refused, never sent as an immediate \
             market or a resting limit",
            req.time_in_force.as_word()
        ));
    }
    match req.order_type {
        OrderType::Market | OrderType::Limit | OrderType::Stop => None,
        OrderType::TakeProfit if req.price.is_none() => None,
        OrderType::TakeProfit => Some(
            "oanda builds no limit-if-touched order (a `take_profit` with a limit price): its \
             MARKET_IF_TOUCHED `priceBound` cancels the order past the bound instead of resting a \
             limit; refused, never sent as a market-if-touched"
                .to_string(),
        ),
        _ => Some(format!(
            "oanda builds no `{}` order (only market, limit, stop and a market-if-touched \
             take_profit); refused, never sent as a market order",
            req.order_type
        )),
    }
}

/// Build the OANDA order-request body from a vike `OrderRequest` on the five-decimal, whole-unit grid
/// of the majors ([`InstrumentGrid::MAJORS`]: `EUR_USD`, `GBP_USD`, `AUD_USD`, `NZD_USD`).
///
/// This is the OFFLINE fixtures' builder: the live loop ([`run_on`]) reads each instrument's own grid and
/// calls [`build_order_body_on`], and refuses an instrument it cannot read one for. Do not call this for
/// an order that goes out.
// `pub` (the `exec` module itself stays private; re-exported `#[doc(hidden)]` at the crate root)
// so `tests/*.rs` integration tests can exercise the real request-body construction directly
// instead of re-deriving it — last resort per the "prefer testing through the public caller"
// rule (there is no public caller: the only call site is the network-calling `run_on()` below).
// Not venue API.
pub fn build_order_body(req: &OrderRequest) -> serde_json::Value {
    build_order_body_on(req, &InstrumentGrid::MAJORS)
}

/// OANDA's `OrderPositionFill` word for "when the Order is filled, only reduce an existing Position": the
/// `positionFill` of every `reduce_only` order this bridge builds, on any account. v20 states the field on the four
/// request types built here (`MarketOrderRequest`, `LimitOrderRequest`, `StopOrderRequest`,
/// `MarketIfTouchedOrderRequest`); without it the venue's `DEFAULT` lets a reduce-only order OPEN a position
/// (`REDUCE_FIRST` on a netting account, `OPEN_ONLY` on a hedging one) once the position it was for is gone.
pub(crate) const REDUCE_ONLY_FILL: &str = "REDUCE_ONLY";

/// [`build_order_body`] on `grid`, the order's instrument's own: the price with exactly
/// `displayPrecision` decimals (a limit's or a market-if-touched's price rounded to the passive side,
/// a stop's trigger away from the market, see [`InstrumentGrid::price`]) and the units with
/// `tradeUnitsPrecision` ([`signed_units`]). A `reduce_only`
/// order carries [`REDUCE_ONLY_FILL`]; any other order's body is the one it always was.
pub(crate) fn build_order_body_on(req: &OrderRequest, grid: &InstrumentGrid) -> serde_json::Value {
    let instrument = to_oanda_instrument(&req.symbol);
    let units = signed_units(grid, req.side, req.qty); // OANDA units: signed base-currency amount
    let mut order = json!({
        "instrument": instrument,
        "units": units,
        "clientExtensions": { "id": req.order_client_id },
    });
    let market = !rests_at_venue(req);
    order["timeInForce"] = json!(oanda_time_in_force(req.time_in_force, market));
    match &req.order_type {
        OrderType::Limit => {
            order["type"] = json!("LIMIT");
            if let Some(p) = req.price {
                order["price"] = json!(fmt_price(grid, req.side, p));
            }
        }
        OrderType::Stop => {
            order["type"] = json!("STOP");
            // A stop's trigger goes AWAY from the market (the model's `snap_stop`: a buy up, a sell
            // down), so it never fires before its level: the limit rule with the side flipped.
            if let Some(p) = req.trigger_price {
                order["price"] = json!(fmt_price(grid, -req.side, p));
            }
        }
        // ⚠ OANDA's MARKET_IF_TOUCHED fires on a cross from EITHER side of the market at creation (its
        // `initialMarketPrice`), so one placed on the stop side behaves as a stop; the venue does not
        // refuse it, and this bridge knows no market price to refuse it with.
        OrderType::TakeProfit if req.price.is_none() => {
            order["type"] = json!("MARKET_IF_TOUCHED");
            if let Some(p) = req.trigger_price {
                order["price"] = json!(fmt_price(grid, req.side, p));
            }
        }
        OrderType::Market => {
            order["type"] = json!("MARKET");
        }
        // Every other kind is refused before a body is built (`unbuilt_kind`); it writes no `type`, so
        // no body of one is a MARKET order (it used to be).
        _ => {}
    }
    // OANDA GTD needs an explicit gtdTime (epoch seconds, per Accept-Datetime-Format: UNIX).
    if !market
        && req.time_in_force == TimeInForce::Gtd
        && let Some(exp) = req.gtd_expiry
    {
        order["gtdTime"] = json!((exp / 1000).to_string());
    }
    if req.reduce_only {
        order["positionFill"] = json!(REDUCE_ONLY_FILL);
    }
    json!({ "order": order })
}

/// Map an OANDA order-POST response to the vike events it implies (Accepted [+ Filled], or Rejected).
// `pub` (re-exported `#[doc(hidden)]`) — same rationale as `build_order_body`: the only
// production caller is `run()`'s network path, so integration tests need direct access to prove
// the dual-publish / reject-short-circuit contract without a live REST double.
pub fn map_order_response(
    order_client_id: &str,
    ts: i64,
    response: &serde_json::Value,
    symbols: &OrderClientIdSymbols,
) -> Vec<Event> {
    let mut out = Vec::new();
    if let Some(reject) = response.get("orderRejectTransaction") {
        let reason = reject.get("rejectReason").and_then(|r| r.as_str()).unwrap_or("rejected");
        out.push(Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: order_client_id.to_string(),
            reason: reason.to_string().into(),
            timestamp: ts,
        }));
        return out;
    }
    if let Some(create) = response.get("orderCreateTransaction") {
        out.push(Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.to_string(),
            venue_order_id: create.get("id").and_then(|i| i.as_str()).map(Into::into),
            timestamp: ts,
        }));
    }
    if let Some(fill) = response.get("orderFillTransaction") {
        out.extend(fill_events(order_client_id, ts, fill, symbols));
    } else if let Some(cancel) = response.get("orderCancelTransaction") {
        // A market order the venue took and cancelled in the same breath (a closed market, no liquidity): nothing
        // rests, and the reply says so. Read here, or it reads Accepted until the stream speaks, if it does.
        let reason = cancel.get("reason").and_then(|r| r.as_str()).unwrap_or_default();
        out.push(Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: reason.to_string().into(),
            timestamp: ts,
        }));
    }
    out
}

/// The events one inline `orderFillTransaction` implies — shared by the order-POST and the
/// order-REPLACE replies, so an immediate fill folds identically on either path.
///
/// Dual-publish (crypto contract, `vike_bridge_core::terminal_events`): bare Fill first so the
/// Account folds position/PnL, then the OrderFilled wrap so the FSM applies it — both the same
/// FillEvent, built by the stream's own [`crate::execution::user_data::fill_from_transaction`] under the request's
/// order client id. Bare Fill and wrap stand or fall together — an id-less fill publishes NEITHER (the
/// acceptance before it still stands: the order genuinely was accepted).
fn fill_events(
    order_client_id: &str,
    ts: i64,
    fill: &serde_json::Value,
    symbols: &OrderClientIdSymbols,
) -> Vec<Event> {
    let Some(fill) = crate::execution::user_data::fill_from_transaction(
        fill,
        order_client_id.to_string(),
        symbols,
    ) else {
        return Vec::new();
    };
    vike_bridge_core::terminal_events(order_client_id.to_string(), fill, ts, true)
}

/// The projected wire units of an order: the vike quantity rounded to the instrument's unit grid (whole
/// OANDA units for a currency pair), signed by side (`build_order_body_on`'s rule, kept in ONE place so a
/// replace sizes exactly as a submit does). A quantity that rounds to nothing is `"0"`, which the venue
/// refuses, as it always did.
fn signed_units(grid: &InstrumentGrid, side: i32, qty: f64) -> String {
    grid.units(side, qty).unwrap_or_else(|| "0".to_string())
}

/// Build the body of OANDA's order REPLACE (`PUT /v3/accounts/{id}/orders/@{order_client_id}`): the WHOLE
/// replacing order, because v20 has no partial amend. The venue cancels the resting order and
/// creates this one atomically, under a NEW venue order id.
///
/// ⚠ **The platform identity survives the venue's new id because the replacement carries the SAME
/// `clientExtensions.id`** (`build_order_body` writes `order_client_id` there). That is what keeps
/// `@{order_client_id}` addressing (the next cancel or modify) and the `clientOrderID` echoed on every later
/// fill, so the fill folds into the order the platform already knows. The venue's new order id
/// returns on the `OrderModified` ([`map_replace_response`]).
///
/// `new_qty` / `new_price` are the requested changes (`None` keeps the resting value); the price is
/// the LIMIT price of a limit and the TRIGGER of a stop, the field the order actually rests on
/// (`vike_model::OrderRecord`'s modify arm makes the same choice). ⚠ `new_qty` is the replacement's
/// TOTAL units: the replacement is a fresh order carrying no execution history (the table's
/// `AmendSemantics::CancelReplace`), and a price-only modify re-sends the RESTING request's qty, so
/// it is not for an order that has partly filled.
///
/// `Err` is the reason a modify cannot be expressed on this venue at all — a market (nothing rests)
/// or an unwired kind, a non-finite or non-positive value — reported as `OrderModifyRejected` by the
/// caller, never sent. `pub` and re-exported `#[doc(hidden)]` for the same reason as
/// [`build_order_body`], and like it written on the majors' grid: the live loop calls
/// `build_replace_body_on`.
pub fn build_replace_body(
    order: &OrderRequest,
    new_qty: Option<f64>,
    new_price: Option<f64>,
) -> Result<serde_json::Value, String> {
    build_replace_body_on(order, new_qty, new_price, || Ok(InstrumentGrid::MAJORS))
}

/// [`build_replace_body`] on the grid `grid` yields for the order's instrument, asked only AFTER the
/// local checks that need none (a market or an unwired kind is refused without a grid, and so without a
/// read of the instrument list). `Err(why)` from `grid` is the replace's refusal sentence.
pub(crate) fn build_replace_body_on(
    order: &OrderRequest,
    new_qty: Option<f64>,
    new_price: Option<f64>,
    grid: impl FnOnce() -> Result<InstrumentGrid, String>,
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
    let grid = grid()?;
    let qty = new_qty.unwrap_or(order.qty);
    let price = new_price.or(resting_price);
    if !qty.is_finite() || grid.units(order.side, qty).is_none() {
        return Err(format!("replacement quantity {qty} rounds to zero OANDA units"));
    }
    if !price.is_some_and(|p| p.is_finite() && p > 0.0) {
        return Err(format!("replacement price {price:?} is not a positive number"));
    }
    // `price` is `new_price` or the resting one, so the projection only moves what was asked
    let projected = vike_model::modified_order(order, Some(qty), new_price, None);
    Ok(build_order_body_on(&projected, &grid))
}

/// `OrderModifyRejected` for `order_client_id` — NON-terminal: the order keeps its terms. Stamped with the venue's
/// `stated` time when its reply carries one, else when it is made
/// (`vike_bridge_core::rest::amend_answer_time`).
fn reject_modify(order_client_id: &str, reason: String, stated: Option<i64>) -> Event {
    Event::OrderModifyRejected(OrderModifyRejected {
        order_client_id: order_client_id.to_string(),
        reason: reason.into(),
        timestamp: amend_answer_time(stated),
    })
}

/// The venue's time of a reply's transaction: its `time`, epoch seconds under `Accept-Datetime-Format: UNIX`
/// (`0`, read as unstated, when it carries none).
fn transaction_time(transaction: &serde_json::Value) -> Option<i64> {
    Some(crate::feed::decode::ts_ms(transaction))
}

/// Map an order-REPLACE reply (`201`: `orderCancelTransaction` for the old order,
/// `orderCreateTransaction` for the new one) to the vike events it implies.
///
/// * **`OrderModified`** for the SAME `order_client_id`, carrying the replacement's venue order id and the
///   units/price the venue ECHOED (venue truth, not the request), so the FSM ends on what rests.
///   One logical order: the venue's cancel of the replaced order never becomes an `OrderCanceled`
///   (`crate::execution::user_data::decode_transaction_events` skips a replace-cancel for exactly that reason).
/// * An immediate fill of the replacement (`orderFillTransaction`) dual-publishes after it.
/// * A replacement the venue cancelled at once (`replacingOrderCancelTransaction`, e.g. a GTD time
///   already past) is the order's terminal `OrderCanceled`: the old order is gone and nothing rests.
/// * A reply with no `orderCreateTransaction` is NOT assumed to have worked: `OrderModifyRejected`.
///
/// A non-2xx reply never gets here (`OandaTransport::finish` turns it into an error the caller reports as
/// `OrderModifyRejected` with OANDA's `errorMessage`). `pub`, re-exported `#[doc(hidden)]`.
///
/// The `OrderModified` / `OrderModifyRejected` carry the time of the transaction that answers the
/// replace, else the moment the reply is read (`vike_bridge_core::rest::amend_answer_time`); `ts` stamps
/// the immediate fill and the at-once cancel, as it always did.
pub fn map_replace_response(
    order_client_id: &str,
    ts: i64,
    response: &serde_json::Value,
    symbols: &OrderClientIdSymbols,
) -> Vec<Event> {
    if let Some(reject) = response.get("orderRejectTransaction") {
        let reason = reject.get("rejectReason").and_then(|r| r.as_str()).unwrap_or("rejected");
        return vec![reject_modify(order_client_id, reason.to_string(), transaction_time(reject))];
    }
    let Some(create) = response.get("orderCreateTransaction") else {
        return vec![reject_modify(
            order_client_id,
            format!("no orderCreateTransaction in the replace reply for {order_client_id}"),
            None,
        )];
    };
    let s = |k: &str| create.get(k).and_then(|v| v.as_str());
    let number = |k: &str| s(k).and_then(|v| v.parse::<f64>().ok());
    let mut out = vec![Event::OrderModified(OrderModified {
        order_client_id: order_client_id.to_string(),
        venue_order_id: s("id").map(Into::into),
        new_qty: number("units").map(f64::abs),
        new_price: number("price"),
        new_trigger_price: None,
        timestamp: amend_answer_time(transaction_time(create)),
    })];
    if let Some(fill) = response.get("orderFillTransaction") {
        out.extend(fill_events(order_client_id, ts, fill, symbols));
    }
    if let Some(cancel) = response.get("replacingOrderCancelTransaction") {
        let reason = cancel.get("reason").and_then(|r| r.as_str()).unwrap_or_default();
        out.push(Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: reason.to_string().into(),
            timestamp: ts,
        }));
    }
    if response.get("orderReissueTransaction").is_some() {
        tracing::warn!(
            venue = VENUE,
            %order_client_id,
            "order-replace reply carries an orderReissueTransaction (a partially filled order was \
             replaced); the reissued order is not modelled and reconcile owns it"
        );
    }
    out
}

/// A replace whose reply was lost (`lost`), told apart by reading the order back: the transactions stream drops both
/// halves of a replace (`crate::execution::user_data::is_replace_cancel`, and the replacement's create names a known client id), so
/// nothing else would ever say it landed, and the platform would keep the old terms over an order the venue repriced. `before`
/// is the order at its old terms, written on the grid the venue holds it on; `read` is `GET /orders/@{order_client_id}`.
///
/// Terms that differ are `OrderModified`, carrying the venue's own. The old terms, or anything else, are the non-terminal
/// refusal, which then says what is known, because the order may have been replaced without the read being able to show it.
/// Each is stamped when the read is answered (`vike_bridge_core::rest::amend_answer_time`).
fn modify_after_lost_reply(
    order_client_id: &str,
    before: Option<&serde_json::Value>,
    read: Result<Option<serde_json::Value>, VenueApiHttpError>,
    lost: &VenueApiHttpError,
) -> Event {
    let term = |order: &serde_json::Value, k: &str| {
        order.get(k).and_then(|v| v.as_str()).and_then(|v| v.parse::<f64>().ok())
    };
    let read = read.ok().flatten();
    let held = read
        .as_ref()
        .and_then(|response| response.get("order"))
        .filter(|o| o.get("state").and_then(|s| s.as_str()) == Some("PENDING"));
    let before = before.and_then(|body| body.get("order"));
    if let (Some(held), Some(before)) = (held, before) {
        let (units, price) = (term(held, "units"), term(held, "price"));
        if units.is_some() && price.is_some() {
            if units == term(before, "units") && price == term(before, "price") {
                return reject_modify(
                    order_client_id,
                    format!(
                        "oanda: the replace's reply was lost ({lost}) and the venue, read after it, holds the old terms; \
                         the order keeps them"
                    ),
                    None,
                );
            }
            tracing::warn!(
                venue = VENUE, %order_client_id, error = %lost,
                "a replace's reply was lost and the venue holds other terms: it landed"
            );
            return Event::OrderModified(OrderModified {
                order_client_id: order_client_id.to_string(),
                venue_order_id: held.get("id").and_then(|i| i.as_str()).map(Into::into),
                new_qty: units.map(f64::abs),
                new_price: price,
                new_trigger_price: None,
                timestamp: amend_answer_time(None),
            });
        }
    }
    reject_modify(
        order_client_id,
        format!(
            "oanda: the replace's outcome is UNKNOWN ({lost}) and the order could not be read back: the venue may have \
             replaced it; the platform keeps the old terms until a reconcile reads the order"
        ),
        None,
    )
}

/// The pauses between a by-client-id lookup's attempts when OANDA does not (yet) show the order: a
/// request that timed out may still be in flight at the venue, so ONE miss is not "absent". Three
/// attempts on the exec thread (ambiguous submits only); each read is bounded by
/// [`crate::transport::LOOKUP_TIMEOUT`] and the whole resolution by [`RESOLVE_BUDGET`].
const LOOKUP_BACKOFF: [std::time::Duration; 2] =
    [std::time::Duration::from_secs(1), std::time::Duration::from_secs(2)];

/// ONE deadline for every step of [`resolve_submit`] (the ladder, the transactions scan, the fill
/// read): a step that STARTS after it is skipped as inconclusive. Unbounded, the steps add up to
/// ~28 s (3 x 5 s order reads + 3 s pauses + a 5 s scan + a 5 s fill read) with a cancel or modify
/// queued behind them; bounded, the worst case is this plus the ONE read in flight at expiry
/// ([`crate::transport::LOOKUP_TIMEOUT`]): ~17 s.
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
    filling_transaction: Option<String>,
}

/// `GET /v3/accounts/{id}/orders/@{order_client_id}` (the order's `clientExtensions.id`): `Ok(Some(order))`
/// when OANDA has the order in ANY state (pending, filled, cancelled), `Ok(None)` only on OANDA's
/// own "order does not exist", `Err` for everything inconclusive.
fn query_order(
    rest: &OandaTransport,
    config: &OandaConfig,
    order_client_id: &str,
) -> Result<Option<FoundOrder>, VenueApiError> {
    let path = format!("/v3/accounts/{}/orders/@{order_client_id}", config.account_id);
    let inconclusive = |msg: String| VenueApiError { code: 0, msg };
    match rest.get_order(&config.rest_base, &path) {
        Ok(None) => Ok(None),
        Ok(Some(response)) => found_order_of(&response).map(Some).ok_or_else(|| {
            inconclusive(format!("order lookup for {order_client_id} carries no order id"))
        }),
        Err(e) => Err(inconclusive(e.to_string())),
    }
}

/// The order in an order-lookup reply (`{"order": {"id": .., "state": .., "fillingTransactionID": ..}}`).
fn found_order_of(response: &serde_json::Value) -> Option<FoundOrder> {
    let order = response.get("order")?;
    let s = |k: &str| order.get(k).and_then(|v| v.as_str()).map(str::to_string);
    Some(FoundOrder {
        id: s("id")?,
        state: s("state").unwrap_or_default(),
        filling_transaction: s("fillingTransactionID"),
    })
}

/// Retry `query` over `pauses` while it does not show the order (`Ok(None)` or `Err`); the first
/// `Ok(Some(_))` wins, otherwise the LAST answer stands (`vike_utility::retry::retry_on_pauses`).
/// `sleep` is injected so the ladder is assertable without waiting.
fn lookup_with_ladder<T>(
    pauses: &[std::time::Duration],
    mut query: impl FnMut() -> Result<Option<T>, VenueApiError>,
    sleep: impl FnMut(std::time::Duration),
) -> Result<Option<T>, VenueApiError> {
    let shows_the_order = |answer: &Result<Option<T>, VenueApiError>| matches!(answer, Ok(Some(_)));
    vike_utility::retry::retry_on_pauses(pauses, |_| query(), shows_the_order, sleep)
}

/// The events for an order OANDA reports: `OrderAccepted` with its venue id, and for a `FILLED`
/// order the fill too (read from its `fillingTransactionID`): a market order fills at once and the
/// stream may have delivered nothing while the gateway was down. A fill that cannot be read (or
/// that `deadline` leaves no time for) stays the stream's / reconcile's (the accept alone is still
/// true). A CANCELLED order is NOT mapped here: a replaced order is CANCELLED too and shares the
/// client id of the live replacement, so only [`resolve_submit`] (a FIRST submit, where nothing was
/// replaced) reads its cancel, from the order's own transactions.
fn found_events(
    rest: &OandaTransport,
    config: &OandaConfig,
    order_client_id: &str,
    ts: i64,
    found: &FoundOrder,
    deadline: std::time::Instant,
    symbols: &OrderClientIdSymbols,
) -> Vec<Event> {
    let mut out = vec![Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.to_string(),
        venue_order_id: Some(found.id.clone().into()),
        timestamp: ts,
    })];
    if found.state == "FILLED"
        && std::time::Instant::now() < deadline
        && let Some(txn) = &found.filling_transaction
    {
        let path = format!("/v3/accounts/{}/transactions/{txn}", config.account_id);
        match rest.get_lookup(&config.rest_base, &path, "") {
            Ok(response) => match response.get("transaction") {
                Some(t) if t.get("type").and_then(|x| x.as_str()) == Some("ORDER_FILL") => {
                    out.extend(fill_events(order_client_id, ts, t, symbols));
                }
                _ => tracing::warn!(
                    venue = VENUE, %order_client_id, %txn,
                    "the filling transaction is not an ORDER_FILL; the stream / reconcile carry the fill"
                ),
            },
            Err(e) => tracing::warn!(
                venue = VENUE, %order_client_id, %txn, error = %e,
                "FILLED order's filling transaction could not be read; the stream / reconcile carry the fill"
            ),
        }
    }
    out
}

/// The watchdog's / gatewayhub's re-query by order client id ([`ExecutionClientActor::with_confirm`]). It can
/// run while the order's POST is still queued behind a slow ambiguous ladder or in flight, so "not
/// found" proves NOTHING here: found -> the order's events, not found or any error -> NO event (the
/// stuck-order watchdog and reconcile keep the order). The terminal "venue confirms absent" belongs
/// to [`resolve_submit`] alone, where the POST certainly went out.
fn confirm_by_order_client_id(
    rest: &OandaTransport,
    config: &OandaConfig,
    order_client_id: &str,
    ts: i64,
    symbols: &OrderClientIdSymbols,
) -> Vec<Event> {
    match query_order(rest, config, order_client_id) {
        Ok(Some(found)) => found_events(
            rest,
            config,
            order_client_id,
            ts,
            &found,
            std::time::Instant::now() + RESOLVE_BUDGET,
            symbols,
        ),
        Ok(None) => Vec::new(),
        Err(e) => {
            tracing::warn!(venue = VENUE, %order_client_id, error = %e, "confirm lookup inconclusive; reporting nothing");
            Vec::new()
        }
    }
}

/// The transactions after `floor` (`GET /transactions/sinceid`) whose client id is `order_client_id`. A 200 with no `transactions`
/// array is an answer about nothing, so it is an error: read as an empty list it became "the venue confirms the order absent".
fn transactions_of(
    rest: &OandaTransport,
    config: &OandaConfig,
    floor: u64,
    order_client_id: &str,
) -> Result<Vec<serde_json::Value>, VenueApiHttpError> {
    let path = format!("/v3/accounts/{}/transactions/sinceid", config.account_id);
    let response = rest.get_lookup(&config.rest_base, &path, &format!("id={floor}"))?;
    let all = response.get("transactions").and_then(|t| t.as_array()).ok_or_else(|| {
        OANDA_HTTP.error(
            VenueApiHttpError::STATUS_OUTCOME_UNKNOWN,
            "the transactions read carries no `transactions` array",
        )
    })?;
    Ok(all.iter().filter(|t| transaction_is_ours(t, order_client_id)).cloned().collect())
}

fn transaction_is_ours(t: &serde_json::Value, order_client_id: &str) -> bool {
    t.pointer("/clientExtensions/id").and_then(|c| c.as_str()) == Some(order_client_id)
        || t.get("clientOrderID").and_then(|c| c.as_str()) == Some(order_client_id)
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
/// exist as a pending order, so `/orders/@order_client_id` can miss them): a rejection of the ORDER itself
/// ([`is_order_reject`]) -> the terminal `OrderRejected`; creation / fill / cancel ->
/// `OrderAccepted` (venue id) first, then the fill (dual-published) or the cancel. Empty = none of
/// the transactions says anything about the order.
fn events_from_transactions(
    order_client_id: &str,
    ts: i64,
    txs: &[serde_json::Value],
    symbols: &OrderClientIdSymbols,
) -> Vec<Event> {
    let s = |t: &serde_json::Value, k: &str| t.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let mut out = Vec::new();
    let mut accepted = false;
    let mut accept = |out: &mut Vec<Event>, id: Option<String>| {
        if !std::mem::replace(&mut accepted, true) {
            out.push(Event::OrderAccepted(OrderAccepted {
                order_client_id: order_client_id.to_string(),
                venue_order_id: id.map(Into::into),
                timestamp: ts,
            }));
        }
    };
    for t in txs {
        let kind = s(t, "type").unwrap_or_default();
        if is_order_reject(&kind) {
            let reason = s(t, "rejectReason").unwrap_or_else(|| "rejected".to_string());
            return vec![Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: order_client_id.to_string(),
                reason: reason.into(),
                timestamp: ts,
            })];
        }
        match kind.as_str() {
            "ORDER_FILL" => {
                accept(&mut out, s(t, "orderID"));
                out.extend(fill_events(order_client_id, ts, t, symbols));
            }
            "ORDER_CANCEL" if !crate::execution::user_data::is_replace_cancel(t) => {
                accept(&mut out, s(t, "orderID"));
                out.push(Event::OrderCanceled(OrderCanceled {
                    order_client_id: order_client_id.to_string(),
                    reason: s(t, "reason").unwrap_or_default().into(),
                    timestamp: ts,
                }));
            }
            k if k.ends_with("_ORDER") => accept(&mut out, s(t, "id")),
            _ => {}
        }
    }
    out
}

/// Resolve an ambiguous submit by order client id, on the exec thread right after its POST failed
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
/// That is safe HERE and not in [`confirm_by_order_client_id`]: this is the FIRST submit on the serialized
/// exec thread, so no replace of this client id can have happened; `events_from_transactions`
/// still skips a replace-cancel.
fn resolve_submit(
    rest: &OandaTransport,
    config: &OandaConfig,
    floor: u64,
    pauses: &[std::time::Duration],
    budget: std::time::Duration,
    order_client_id: &str,
    ts: i64,
    symbols: &OrderClientIdSymbols,
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
            query_order(rest, config, order_client_id)
        },
        |pause| thread::sleep(pause.min(deadline.saturating_duration_since(Instant::now()))),
    );
    match found {
        Ok(Some(order)) => {
            let cancelled = if order.state == "CANCELLED" && floor > 0 && !spent() {
                transactions_of(rest, config, floor, order_client_id)
                    .map(|txs| events_from_transactions(order_client_id, ts, &txs, symbols))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            if cancelled.is_empty() {
                found_events(rest, config, order_client_id, ts, &order, deadline, symbols)
            } else {
                cancelled
            }
        }
        Ok(None) if floor > 0 && !spent() => {
            match transactions_of(rest, config, floor, order_client_id) {
                Ok(txs) => match events_from_transactions(order_client_id, ts, &txs, symbols) {
                    evs if !evs.is_empty() => evs,
                    _ => vec![vike_bridge_core::rest::venue_confirms_absent(order_client_id, ts)],
                },
                Err(e) => unresolved(order_client_id, &e.to_string()),
            }
        }
        Ok(None) => unresolved(
            order_client_id,
            "cannot rule a fill out (no transaction watermark, or the resolve budget is spent)",
        ),
        Err(e) => unresolved(order_client_id, &e.to_string()),
    }
}

/// An inconclusive resolution emits NOTHING (IG's `submit_outcome` does the same): the order stays
/// `Submitted`, so the stuck-order watchdog and reconcile still see it. An id-less `OrderAccepted`
/// would take a possibly never-received order out of the watchdog's view (it watches only
/// Initialized/Submitted).
fn unresolved(order_client_id: &str, why: &str) -> Vec<Event> {
    tracing::warn!(
        venue = VENUE, %order_client_id, %why,
        "submit outcome unknown and the lookup is inconclusive; emitting nothing, the order stays Submitted"
    );
    Vec::new()
}

/// Make sure the exec path's watermark is not 0: read the account's last transaction id
/// (`GET /v3/accounts/{id}/summary`) before a submit's POST while it is still 0, so the floor
/// [`resolve_submit`] scans from is real. A failure is logged and leaves it 0 (then nothing is ruled
/// absent). After a failure (or a reply with no id) it asks again only after [`WATERMARK_RETRY`]:
/// `retry_at` is the exec thread's own memory of that, so a dead endpoint does not put a 5 s read
/// in front of every submit.
fn ensure_watermark(
    rest: &OandaTransport,
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
        Ok(response) => note_last_transaction_id(last_seen, &response),
        Err(e) => tracing::warn!(
            venue = VENUE, error = %e,
            "could not read the account's last transaction id; an ambiguous submit cannot be ruled absent"
        ),
    }
    *retry_at = (last_seen.load(Ordering::Relaxed) == 0)
        .then(|| std::time::Instant::now() + WATERMARK_RETRY);
}

/// Advance the exec path's watermark from an order-POST response's `lastTransactionID`.
// `pub` (re-exported `#[doc(hidden)]`) — the exec-side half of the watermark contract;
// promoted so its monotonic-max / missing-field behavior is integration-testable directly.
pub fn note_last_transaction_id(last_seen: &Arc<AtomicU64>, response: &serde_json::Value) {
    note_max_id(last_seen, response, "lastTransactionID");
}

/// Advance the exec path's watermark to the max of itself and `v[key]`, a decimal-string transaction id
/// (`lastTransactionID` on a POST response, `id` on a streamed transaction); a missing or unreadable id
/// is a no-op.
pub(crate) fn note_max_id(last_seen: &Arc<AtomicU64>, v: &serde_json::Value, key: &str) {
    if let Some(id) = v.get(key).and_then(|x| x.as_str()).and_then(|s| s.parse::<u64>().ok()) {
        last_seen.fetch_max(id, Ordering::Relaxed);
    }
}

/// The account's instrument list, the payload [`GridCache`] reads its grids from. One short-timeout GET
/// ([`crate::transport::LOOKUP_TIMEOUT`]): it runs on the exec thread, ahead of an order that needs a grid.
fn read_instruments(
    rest: &OandaTransport,
    base: &str,
    account: &str,
) -> Result<serde_json::Value, String> {
    rest.get_lookup(base, &format!("/v3/accounts/{account}/instruments"), "")
        .map_err(|e| e.to_string())
}

/// The TEST harness's loop: [`run_on`] with a book that already holds `EUR_USD` on the majors' grid and
/// reads nothing, and a NETTING account, because the scripted venues of the older tests answer the order
/// calls in sequence and nothing else. An instrument beyond it would read the list from the same scripted
/// venue.
#[cfg(test)]
fn run(
    config: OandaConfig,
    events: EventSender,
    rx: Receiver<ExecutionClientActorCommand>,
    last_seen: Arc<AtomicU64>,
    symbols: Arc<OrderClientIdSymbols>,
    fence: Arc<SubmitFence>,
) {
    let book = GridCache::preloaded([("EUR_USD", InstrumentGrid::MAJORS)]);
    run_on(config, events, rx, last_seen, symbols, fence, book, AccountHedging::stated(false))
}

/// The exec thread's loop. `grids` is the book each order's instrument grid is read from: an order whose
/// instrument has none is refused with the book's sentence (`OrderRejected`; a replace
/// `OrderModifyRejected`), before anything is sent. `hedging` is what the thread knows of the account's
/// `hedgingEnabled`: asked once before the first command when unknown, and again by a reduce-only submit that
/// finds it unknown; a netting answer makes a close that waited read the position first (`crate::account::modes`).
fn run_on(
    config: OandaConfig,
    events: EventSender,
    rx: Receiver<ExecutionClientActorCommand>,
    last_seen: Arc<AtomicU64>,
    symbols: Arc<OrderClientIdSymbols>,
    fence: Arc<SubmitFence>,
    mut grids: GridCache,
    mut hedging: AccountHedging,
) {
    /// The account's net position in `symbol` now, on the lookup agent (`/positions` omits an instrument never
    /// traded, which reads 0). The read a close asks when it waited (`ExecutionClientActorLateCloses`).
    fn held_position(
        rest: &OandaTransport,
        base: &str,
        account: &str,
        symbol: &str,
    ) -> Result<f64, String> {
        let rows = rest
            .get_lookup(base, &format!("/v3/accounts/{account}/positions"), "")
            .map_err(|e| e.to_string())?;
        let reports =
            crate::reconcile::parse_position_reports(&rows, &to_oanda_instrument(symbol), symbol);
        Ok(reports.iter().map(|row| row.qty).sum())
    }

    let rest = OandaTransport::new(config.api_token.expose().clone());
    let (account, base) = (config.account_id.clone(), config.rest_base.clone());
    hedging.ensure(&rest, &base, &account);
    let mut watermark_retry_at = None;
    let mut late = ExecutionClientActorLateCloses::default();
    // Publish what an arm produced; an order it ENDS leaves the submitted-symbol table.
    let emit = |evs: Vec<Event>| {
        settle(&symbols, &evs);
        for event in evs {
            let _ = events.blocking_send(event);
        }
    };

    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecutionClientActorCommand::Submit(req, queued_at) => {
                // The stream's events for this order wait until the arm is done, whichever way it
                // leaves (the shared fence, owner ruling B-17): OANDA writes a market order's fill to
                // the stream before this thread has read the POST's reply, and a fill ahead of the
                // `OrderAccepted` is refused by the FSM. Each held event settles the symbol table as
                // it goes, as `publish` would have.
                let _fenced = fence.enter_with(&req.order_client_id, |event| {
                    settle(&symbols, std::slice::from_ref(event));
                });
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: req.order_client_id.clone(),
                    timestamp: req.timestamp,
                }));
                // The instrument's own grid, or the sentence the order is refused with: nothing is sent.
                // A reduce-only order says so on the wire (`REDUCE_ONLY_FILL`, in the body). A kind this
                // bridge builds no order for (`unbuilt_kind`), then an order too old to send, is refused
                // first, before it costs a read; a cancel and a close have no age arm. A close that
                // waited, on a netting account, asks the position first (`ExecutionClientActorLateCloses`), as it did before
                // the word went on every account (`crate::account::modes`).
                let grid = match unbuilt_kind(&req).or_else(|| too_old_to_send(&req, queued_at)) {
                    Some(why) => Err(why),
                    None => grids.of(
                        &to_oanda_instrument(&req.symbol),
                        std::time::Instant::now(),
                        || read_instruments(&rest, &base, &account),
                    ),
                };
                let body = grid.and_then(|grid| {
                    if req.reduce_only {
                        hedging.ensure(&rest, &base, &account);
                    }
                    // Asked whatever the account: a late close on a hedging or an unknown one still has
                    // closes behind it, and the setting may be known by then.
                    let must_read = late.must_read(&req, queued_at);
                    Ok(if hedging.nets() && must_read {
                        let held = held_position(&rest, &base, &account, &req.symbol);
                        match late_close_size(&req, held)? {
                            Some(qty) => {
                                build_order_body_on(&OrderRequest { qty, ..(*req).clone() }, &grid)
                            }
                            None => build_order_body_on(&req, &grid),
                        }
                    } else {
                        build_order_body_on(&req, &grid)
                    })
                });
                let body = match body {
                    Ok(body) => body,
                    Err(why) => {
                        emit(vec![Event::OrderRejected(OrderRejected {
                            cause: None,
                            order_client_id: req.order_client_id.clone(),
                            reason: why.into(),
                            timestamp: req.timestamp,
                        })]);
                        continue; // the fence lets the stream's events go as `_fenced` drops
                    }
                };
                let path = format!("/v3/accounts/{account}/orders");
                // The floor an ambiguous outcome is ruled absent from: read BEFORE the POST so every
                // transaction of this order is after it (the stream may advance the watermark
                // past the order's own transactions before the lookup runs).
                ensure_watermark(&rest, &config, &last_seen, &mut watermark_retry_at);
                let floor = last_seen.load(Ordering::Relaxed);
                // Asked again: the reads above can spend longer than the order had left, and the wait bounds the POST.
                if let Some(why) = too_old_to_send(&req, queued_at) {
                    emit(vec![Event::OrderRejected(OrderRejected {
                        cause: None,
                        order_client_id: req.order_client_id.clone(),
                        reason: why.into(),
                        timestamp: req.timestamp,
                    })]);
                    continue; // the fence lets the stream's events go as `_fenced` drops
                }
                match rest.post_json(&base, &path, &body) {
                    Ok(response) => {
                        // The next ambiguous submit scans from AFTER this order's transactions.
                        note_last_transaction_id(&last_seen, &response);
                        emit(map_order_response(
                            &req.order_client_id,
                            req.timestamp,
                            &response,
                            &symbols,
                        ));
                    }
                    // The request may have reached OANDA (timeout / reset after the write, a gateway
                    // 5xx): the order may be LIVE or filled, so ask by client id instead of rejecting.
                    Err(e) if e.outcome_unknown() => {
                        tracing::warn!(
                            venue = VENUE, order_client_id = %req.order_client_id, error = %e,
                            "order submit outcome unknown - resolving by order client id"
                        );
                        emit(resolve_submit(
                            &rest,
                            &config,
                            floor,
                            &LOOKUP_BACKOFF,
                            RESOLVE_BUDGET,
                            &req.order_client_id,
                            req.timestamp,
                            &symbols,
                        ));
                    }
                    Err(e) => emit(vec![Event::OrderRejected(OrderRejected {
                        cause: None,
                        order_client_id: req.order_client_id.clone(),
                        reason: e.to_string().into(),
                        timestamp: req.timestamp,
                    })]),
                }
                // Whatever the arm produced (an inconclusive resolution produces nothing) is out:
                // the stream's events for the order follow it as `_fenced` drops here.
            }
            ExecutionClientActorCommand::Cancel { order_client_id, .. } => {
                // A failed/ambiguous cancel must not vanish (audit A2): map every outcome to an
                // event. A genuine venue-side cancel still confirms via the transactions stream.
                let path = format!("/v3/accounts/{account}/orders/@{order_client_id}/cancel");
                let outcome = match rest.put_empty(&base, &path) {
                    Ok(response) if response.get("orderCancelTransaction").is_some() => {
                        ExecutionClientActorCancelOutcome::Canceled
                    }
                    Ok(_) => ExecutionClientActorCancelOutcome::Rejected(format!(
                        "no orderCancelTransaction for {order_client_id}"
                    )),
                    Err(e) => ExecutionClientActorCancelOutcome::Rejected(e.to_string()),
                };
                emit(vec![cancel_event(&order_client_id, outcome)]);
            }
            // No bulk-cancel path here, and this venue declares none — so `ExecutionClientActor` fanned the
            // batch out into the per-id `Cancel`s above before it ever reached this channel, and
            // this arm is unreachable. It exists because a new command variant is exhaustive; the
            // shared helper refuses every id NON-terminally rather than letting a future mis-wiring
            // drop them silently.
            ExecutionClientActorCommand::CancelBatch { order_client_ids, .. } => {
                cancel_batch_undeclared(&events, &order_client_ids)
            }
            // Unreachable the same way: this venue declares no bulk submit and no queued adopt.
            ExecutionClientActorCommand::SubmitBatch(requests, _) => {
                vike_bridge_core::exec_actor::submit_batch_undeclared(&events, &requests)
            }
            ExecutionClientActorCommand::Adopt(order) => {
                vike_bridge_core::exec_actor::adopt_undeclared(&order)
            }
            // OANDA's native amend is a REPLACE: `PUT /orders/@{order_client_id}` with the replacing order.
            // Every outcome maps to an event: a refusal (local or venue) is the NON-terminal
            // `OrderModifyRejected` and the resting order keeps its terms; success is
            // `OrderModified`, the SAME order client id carrying the venue's NEW order id.
            ExecutionClientActorCommand::Modify { order, new_qty, new_price, .. } => {
                let order_client_id = order.order_client_id.as_str();
                let grid = || {
                    grids.of(&to_oanda_instrument(&order.symbol), std::time::Instant::now(), || {
                        read_instruments(&rest, &base, &account)
                    })
                };
                // The replacement is the WHOLE order again, so a reduce-only one keeps its `positionFill`.
                let replacement = build_replace_body_on(&order, new_qty, new_price, grid);
                let evs = match replacement {
                    Err(reason) => vec![reject_modify(order_client_id, reason, None)],
                    Ok(body) => {
                        let path = format!("/v3/accounts/{account}/orders/@{order_client_id}");
                        match rest.put_json(&base, &path, &body) {
                            Ok(response) => {
                                note_last_transaction_id(&last_seen, &response);
                                map_replace_response(
                                    order_client_id,
                                    order.timestamp,
                                    &response,
                                    &symbols,
                                )
                            }
                            // The replace may have landed: ask the venue what it holds.
                            Err(e) if e.outcome_unknown() => {
                                let before = build_replace_body_on(&order, None, None, || {
                                    grids.of(
                                        &to_oanda_instrument(&order.symbol),
                                        std::time::Instant::now(),
                                        || read_instruments(&rest, &base, &account),
                                    )
                                });
                                let read = rest.get_order(&base, &path);
                                vec![modify_after_lost_reply(
                                    order_client_id,
                                    before.as_ref().ok(),
                                    read,
                                    &e,
                                )]
                            }
                            Err(e) => vec![reject_modify(order_client_id, e.to_string(), None)],
                        }
                    }
                };
                emit(evs);
            }
            ExecutionClientActorCommand::Shutdown => break,
        }
    }
}

#[path = "execution_tests.rs"]
#[cfg(test)]
mod execution_tests;

#[path = "execution_grid_tests.rs"]
#[cfg(test)]
mod execution_grid_tests;
