//! Inbound frame dispatch: quotes, bar seeds, venue errors, execution events, the close-leg fold.

use std::sync::{Arc, Mutex};

use vike_bridge_core::execution_client::order_client_id::OrderVenueIdRegistry;
use vike_bridge_core::transport::VenueApiError;
use vike_data::DataClientSink;
use vike_execution::EventSender;
use vike_marketdata::{Bar, TickQuote};
use vike_model::events::{
    Event, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderFilled, OrderPartiallyFilled,
    OrderRejected,
};
use vike_model::now_ms;

use super::outbound::AMEND_MESSAGE_PREFIX;
use super::session::{PROBE_MESSAGE_ID, REAUTH_MESSAGE_ID};
use super::stream::decode_error;
use super::{CloseState, ConnectionError, PositionsByIdShared, VENUE, decode_payload};
use crate::execution::positions::{self, CloseEmit, CloseFailure};
use crate::feed::decode;
use crate::instrument::symbols::SymbolMap;
use crate::transport::error_codes;
use crate::transport::proto::{
    ProtoMessage, ProtoOaExecutionEvent, ProtoOaExecutionType, ProtoOaGetTrendbarsRes,
    ProtoOaOrderErrorEvent, ProtoOaSpotEvent, ProtoOaSymbolChangedEvent, ProtoOaTrendbarPeriod, pt,
};

#[cfg(doc)]
use super::outbound::{write_command, write_failure_event};
#[cfg(doc)]
use super::reconcile::rebuild_position_book;
#[cfg(doc)]
use crate::execution::positions::CloseTracker;

/// Hand one canonical event to the core, AFTER the split-exit table has folded it: an event of a leg of an exit that
/// spans positions becomes its parent's single lifecycle (or nothing: `crate::execution::exit_legs`), every other event goes out
/// as it is. The ONE place an event leaves this connection for the core, so a leg's event can never reach the engine
/// under an order client id the engine does not know. A poisoned book sends the event unfolded rather than lose it.
pub(super) fn emit_event(events: &EventSender, positions: &PositionsByIdShared, event: Event) {
    let out = match positions.lock() {
        Ok(mut book) => {
            book.note_order_ended(&event);
            book.fold_exit_leg_event(event)
        }
        Err(_) => vec![event],
    };
    for event in out {
        if events.blocking_send(event).is_err() {
            tracing::warn!(target: "ctrader", "core ingest gone; dropping an exec event");
        }
    }
}

/// Dispatch point for inbound frames. Handles heartbeats/errors, routes `SPOT_EVENT` (quotes +
/// any embedded live trendbars) and `GET_TRENDBARS_RES` (the F2 historical seed, see
/// `on_get_trendbars_res`) to the `DataClientSink`, and (when exec is wired) routes
/// `EXECUTION_EVENT` to the `EventSender` after updating the order client id→order-id map. `money_digits` is
/// the account's `ProtoOATrader.money_digits` (fetched once at handshake, refreshed on reconnect —
/// see `open_and_handshake`) — threaded into `decode::execution_event_to_events` as the fallback
/// exponent for a fill's signed commission integer, when the deal itself omits its own
/// `money_digits`.
pub(super) fn on_inbound(
    msg: &ProtoMessage,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn DataClientSink,
    events: Option<&EventSender>,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    positions: &PositionsByIdShared,
    closes: &CloseState,
) {
    match msg.payload_type {
        pt::HEARTBEAT_EVENT => {} // peer keepalive — nothing to do
        pt::ERROR_RES => on_error_res(msg, events, orders, closes, positions),
        pt::ORDER_ERROR_EVENT => on_order_error_event(msg, events, orders, closes, positions),
        pt::SPOT_EVENT => match decode_payload::<ProtoOaSpotEvent>(msg) {
            Ok(event_value) => {
                if let Some((symbol, bid, ask)) = decode::spot_to_quote(&event_value, symbols) {
                    let quote = TickQuote {
                        timestamp: event_value.timestamp.unwrap_or_else(vike_model::now_ms),
                        local_timestamp: vike_model::now_ms(),
                        bid,
                        ask,
                        bid_size: 0.0,
                        ask_size: 0.0,
                        symbol: symbol.as_str().into(),
                    };
                    sink.quote(VENUE, &symbol, quote);
                }
                for (symbol, interval, bar) in decode::spot_to_bars(&event_value, symbols) {
                    // A live trendbar riding a SPOT_EVENT is the bar STILL forming (cTrader keeps
                    // pushing updated snapshots of the current period and marks none closed), so it
                    // is a `forming_bar`; the actor's sink closes the period a later snapshot ends
                    // (`crate::feed::bar::TrendbarCloses`).
                    sink.forming_bar(VENUE, &symbol, interval, bar);
                }
            }
            Err(e) => {
                tracing::warn!(target: "ctrader", error = %e, "failed to decode SPOT_EVENT");
            }
        },
        pt::GET_TRENDBARS_RES => match decode_payload::<ProtoOaGetTrendbarsRes>(msg) {
            Ok(res) => on_get_trendbars_res(&res, symbols, sink),
            Err(e) => {
                tracing::warn!(target: "ctrader", error = %e, "failed to decode GET_TRENDBARS_RES");
            }
        },
        pt::EXECUTION_EVENT => {
            // Order flow: decode, update the order client id→order-id correlation map, and push canonical
            // events onto the ingest lane. Only active when an `EventSender` was wired
            // (`connect_and_auth_execution`); with data-only wiring an execution event only ENDS an order.
            let Some(events) = events else {
                // ⚠ A data socket's list is filled by its reconnect's reconcile (`reconcile_after_reconnect`), and its
                // orders end on the exec socket: without this, an order cancelled minutes before stayed listed and
                // every later reconnect warned of it as a gap (the CI box, 2026-11-09, `slp1109a`).
                if let Ok(event_value) = decode_payload::<ProtoOaExecutionEvent>(msg)
                    && decode::execution_event_is_terminal(&event_value)
                {
                    update_order_map(&event_value, orders);
                }
                return;
            };
            match decode_payload::<ProtoOaExecutionEvent>(msg) {
                Ok(event_value) => {
                    // Keep the OPEN-position map fresh (drives the exec client's reduce routing) and
                    // the order client id→orderId map, then map to canonical events — routing a CLOSE-correlated
                    // event through the per-order-client-id aggregator (see `execution_event_to_canonical`).
                    update_position_map(&event_value, positions);
                    update_bound_exits(&event_value, positions);
                    update_order_map(&event_value, orders);
                    for event in
                        execution_event_to_canonical(&event_value, symbols, money_digits, closes)
                    {
                        emit_event(events, positions, event);
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "ctrader", error = %e, "failed to decode EXECUTION_EVENT");
                }
            }
        }
        // The broker changed a symbol (its trading state is what the exec client reads): noted on the map, and the
        // actor re-reads it between two frames (`crate::transport::connection::symbol_state`). Noted HERE, not in the actor loop, so
        // an event that arrived inside another round trip (and was buffered) is not lost.
        pt::SYMBOL_CHANGED_EVENT => match decode_payload::<ProtoOaSymbolChangedEvent>(msg) {
            Ok(event_value) => symbols.note_changed(&event_value.symbol_id),
            Err(e) => {
                tracing::warn!(target: "ctrader", error = %e, "failed to decode SYMBOL_CHANGED_EVENT");
            }
        },
        other => {
            tracing::debug!(target: "ctrader", payload_type = other, "unhandled inbound frame");
        }
    }
}

/// Route a `GET_TRENDBARS_RES` frame (F2 historical seed): resolve the symbol name + interval,
/// map every trendbar entry through `decode::trendbar_to_bar` (skipping incomplete entries per
/// that function's contract), sort ascending by timestamp (the wire order is not documented/
/// guaranteed), and hand the whole batch to `DataClientSink::seed_bars` in ONE call — the dedicated
/// "recent history before live closes start arriving" verb (see that trait method's doc). The
/// answer's still-forming period is held back from it by the actor's sink
/// (`crate::feed::bar::TrendbarCloses`), since a seed is closed bars, and that sink also tells a reconnect's
/// fill from a seed by what the key asked, so an answer with no bar is handed on too (it answers a fill; a seed
/// left empty is never sent on). Best-effort: an unresolvable symbol id/period is a silent no-op (one `warn!`,
/// never per-bar) — the live `SubscribeTrendbar` stream (sent independently, and unconditionally, by
/// `feed::DataClientBusSpecHoldCtrader::ask`) starts regardless of whether this seed succeeds.
fn on_get_trendbars_res(
    res: &ProtoOaGetTrendbarsRes,
    symbols: &SymbolMap,
    sink: &dyn DataClientSink,
) {
    if let Some((name, interval, bars)) = trendbars_of(res, symbols) {
        sink.seed_bars(VENUE, &name, interval, bars);
    }
}

/// A `GET_TRENDBARS_RES`'s USER name (`SymbolMap::user_name_of`, decision 0290), interval and complete bars,
/// ts-ascending (possibly none). `None` for an unresolvable symbol id or period, with one `warn!`.
pub(super) fn trendbars_of(
    res: &ProtoOaGetTrendbarsRes,
    symbols: &SymbolMap,
) -> Option<(String, &'static str, Vec<Bar>)> {
    let Some(symbol_id) = res.symbol_id else {
        tracing::warn!(target: "ctrader", "GET_TRENDBARS_RES missing symbolId; dropping it");
        return None;
    };
    let Some(name) = symbols.user_name_of(symbol_id) else {
        tracing::warn!(
            target: "ctrader",
            symbol_id,
            "GET_TRENDBARS_RES for unknown symbol id; dropping it"
        );
        return None;
    };
    let Ok(period) = ProtoOaTrendbarPeriod::try_from(res.period) else {
        tracing::warn!(target: "ctrader", "GET_TRENDBARS_RES has an unrecognized period; dropping it");
        return None;
    };
    let mut bars: Vec<Bar> = res.trendbar.iter().filter_map(decode::trendbar_to_bar).collect();
    bars.sort_by_key(|b| b.timestamp);
    Some((name, decode::interval_for_trendbar_period(period), bars))
}

/// Build a reject `reason` string from a venue error's code + optional description. Contains ONLY
/// venue-provided error text (never creds/tokens — `decode_error`/the wire error carry neither).
fn error_reason(error_code: &str, description: &str) -> String {
    if description.is_empty() {
        error_code.to_string()
    } else {
        format!("{error_code}: {description}")
    }
}

/// The order client id of the AMEND an envelope `clientMsgId` names (`AMEND_MESSAGE_PREFIX` + order client id), if it names one.
fn amend_order_client_id(msg: &ProtoMessage) -> Option<&str> {
    msg.client_msg_id
        .as_deref()
        .and_then(|id| id.strip_prefix(AMEND_MESSAGE_PREFIX))
        .filter(|c| !c.is_empty())
}

/// The non-terminal event for a failed AMEND: the order is still working, at its old terms when the venue refused it,
/// and worded "modify outcome unknown" when the code leaves the amend open (`error_codes::TAXONOMY`'s
/// `submit_outcome_unknown`).
fn amend_failed(order_client_id: &str, error_code: &str, description: &str) -> Event {
    let err = VenueApiError {
        code: error_codes::code_of(error_code),
        msg: error_reason(error_code, description),
    };
    let unknown = |code| error_codes::TAXONOMY.submit_outcome_unknown(code);
    vike_bridge_core::rest::modify_failed_with(order_client_id, &err, unknown, now_ms())
}

/// **What the venue's refusal of an order request ends**, for the order client id it names: `accepted` when the order
/// map knows it, so the request refused was a cancel. ONE answer for an `ERROR_RES` and an `ORDER_ERROR_EVENT`.
///
/// - A CLOSE the tracker holds ends through [`close_failure_terminal`] (forgotten, Canceled once a leg filled, else
///   Rejected), as the execution layer's rejection of it does: a plain `OrderRejected` after a filled leg was the
///   illegal `PartiallyFilled -> Rejected` edge, and the tracker's `by_position` stayed, so every later reduce of that
///   position was refused "already closing in flight" for the socket's life.
/// - An accepted order's refused cancel is the NON-terminal [`cancel_refused`]: the order still rests.
/// - Anything else is a submit, refused: the terminal `OrderRejected`, its bound exit's row dropped.
///
/// ⚠ **A code that leaves the write open ends nothing** (`error_codes::TAXONOMY`'s `submit_outcome_unknown`: `UNKNOWN_ERROR`,
/// `CANT_ROUTE_REQUEST`, a custom code; decision 0303): the order may have landed, so it is never rejected. A submit
/// stays in flight for its execution events, a reconnect's ask of the venue's lists
/// ([`super::reconcile::reconcile_after_reconnect`]) or the core's stuck-order ladder; a close stays in the tracker
/// for its legs' events or the socket's next death, which strands it for that ask. A cancel's advisory says unknown.
///
/// shortcut: an open close holds its position's reduce gate until its legs answer or the socket next dies; ask the
/// venue's lists at once if a close ever logs it.
fn order_refusal(
    order_client_id: &str,
    accepted: bool,
    error_code: &str,
    description: &str,
    closes: &CloseState,
    positions: &PositionsByIdShared,
) -> Option<Event> {
    let reason = error_reason(error_code, description);
    let open = error_codes::TAXONOMY.submit_outcome_unknown(error_codes::code_of(error_code));
    let is_close = closes.lock().ok().is_some_and(|t| t.is_tracked(order_client_id));
    if accepted && !is_close {
        return Some(cancel_refused(order_client_id.to_string(), reason, positions, open));
    }
    if open {
        tracing::warn!(
            target: "ctrader",
            %order_client_id,
            is_close,
            %reason,
            "the venue's answer to an order request leaves its outcome unknown (it may have landed): no terminal is \
             sent; its events, a reconnect's ask of the venue's lists or the core's stuck-order ladder end it"
        );
        return None;
    }
    if is_close {
        return Some(close_failure_terminal(order_client_id, &reason, closes, now_ms()));
    }
    // A bound exit the venue refused is over: drop its row.
    if let Ok(mut book) = positions.lock() {
        book.unbind_exit(order_client_id);
    }
    Some(Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: order_client_id.to_string(),
        reason: reason.into(),
        timestamp: now_ms(),
        refusal_id: vike_model::events::refusal_id_of(
            vike_model::OrderRefusalId::VenueRefused.word(),
        ),
    }))
}

/// The reverse direction of the order client id↔order-id correlation map: the `order_client_id` a numeric venue
/// `order_id` belongs to, if any. Used to correlate an `ORDER_ERROR_EVENT` (which carries only the
/// `orderId`) back to the order client id the OMS keys on.
fn reverse_lookup(orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>, order_id: i64) -> Option<String> {
    orders.lock().ok()?.order_client_id_of(&order_id).map(str::to_owned)
}

/// Route an `ERROR_RES` (2142) frame. cTrader echoes the request's envelope `clientMsgId` on the
/// error to that request; the order commands set that field to the order's `order_client_id`
/// ([`write_command`]), so a non-empty `clientMsgId` here identifies the failed order request (an amend's carries
/// `AMEND_MESSAGE_PREFIX`), and [`order_refusal`] decides what it ends.
/// An `ERROR_RES` with an EMPTY `clientMsgId`, or the probe's or the re-auth's (`PROBE_MESSAGE_ID`, `REAUTH_MESSAGE_ID`), is
/// not order-correlated — keep the original `warn!`.
/// An auth-flavored `ERROR_RES` reaches here too, ahead of the actor's refresh: it may be the answer to an order.
///
/// ⚠ **An order client id the order map knows was accepted, so the request refused was a cancel**: NON-terminal, the order still
/// rests at the venue. A cancel refused because the account's session lapsed used to read as the terminal reject of a
/// working order.
fn on_error_res(
    msg: &ProtoMessage,
    events: Option<&EventSender>,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    closes: &CloseState,
    positions: &PositionsByIdShared,
) {
    let ConnectionError::Venue { error_code, description } = decode_error(msg) else {
        return; // undecodable error frame — nothing correlatable to synthesize
    };
    // A failed AMEND (a protocol-level refusal of the request) refuses the AMEND: the order is still working
    // at its old terms, so it must not become a terminal reject.
    if let (Some(order_client_id), Some(events)) = (amend_order_client_id(msg), events) {
        // The exit keeps its size (and stops counting the one the refused amend asked for).
        if let Ok(mut book) = positions.lock() {
            book.exit_amend_failed(order_client_id);
        }
        emit_event(events, positions, amend_failed(order_client_id, &error_code, &description));
        return;
    }
    // The bridge's own requests, the probe and the re-auth, carry ids of their own, and no order stands behind either: a
    // refusal of one in words that end no session reaches here, and used to reject an order client id the engine never placed.
    let order_client_id = msg
        .client_msg_id
        .as_deref()
        .filter(|s| !s.is_empty() && ![PROBE_MESSAGE_ID, REAUTH_MESSAGE_ID].contains(s));
    match (order_client_id, events) {
        (Some(order_client_id), Some(events)) => {
            let accepted = orders
                .lock()
                .ok()
                .is_some_and(|map| map.venue_order_id_of(order_client_id).is_some());
            let refused = order_refusal(
                order_client_id,
                accepted,
                &error_code,
                &description,
                closes,
                positions,
            );
            if let Some(event) = refused {
                emit_event(events, positions, event);
            }
        }
        // A stream asked for again that is already on: what a re-auth's `resubscribe` and a swap's `SwapRecheck` expect.
        _ if error_code == "ALREADY_SUBSCRIBED" => {
            tracing::debug!(target: "ctrader", %description, "already subscribed");
        }
        // Empty clientMsgId (not order-correlated) or data-only wiring: log, don't fabricate.
        _ => tracing::warn!(target: "ctrader", %error_code, %description, "venue error frame"),
    }
}

/// The venue refused a cancel of the accepted order `order_client_id`, which still rests: the NON-terminal event, its id
/// the outcome unknown one when the code leaves the cancel `open`. The bridge's own cancel of an orphaned bound exit is
/// given up (no retry loop) and the engine's cancel of the same order let through.
fn cancel_refused(
    order_client_id: String,
    reason: String,
    positions: &PositionsByIdShared,
    open: bool,
) -> Event {
    if let Ok(mut book) = positions.lock() {
        if book.cancel_in_flight(&order_client_id) {
            tracing::error!(
                target: "ctrader",
                order_client_id = %order_client_id,
                %reason,
                "the venue REFUSED the cancel of an exit whose position is gone: it may \
                 still rest and OPEN a position when it triggers - cancel it by hand"
            );
        }
        book.cancel_failed(&order_client_id);
    }
    let refusal = if open {
        vike_model::OrderRefusalId::OutcomeUnknown
    } else {
        vike_model::OrderRefusalId::VenueRefused
    };
    Event::OrderCancelRejected(OrderCancelRejected {
        order_client_id,
        reason: reason.into(),
        timestamp: now_ms(),
        refusal_id: vike_model::events::refusal_id_of(refusal.word()),
    })
}

/// Route an `ORDER_ERROR_EVENT` (2132) frame — an error against a specific order request. Reverse-
/// correlate its `orderId` to an `order_client_id` via the shared [`OrderVenueIdRegistry`]: a hit means the
/// order was accepted (we only learn its `orderId` from an execution event), so the failed request
/// was a cancel (an amend's id says so itself). If the `orderId` is absent/uncorrelatable but the envelope
/// `clientMsgId` carries an order client id, the order was never accepted, or it is a close's. [`order_refusal`]
/// decides what the refusal ends, as for an `ERROR_RES`. With no correlation at all we `warn!` rather
/// than fabricate a wrong order client id. A no-op under data-only wiring (no `EventSender`).
fn on_order_error_event(
    msg: &ProtoMessage,
    events: Option<&EventSender>,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    closes: &CloseState,
    positions: &PositionsByIdShared,
) {
    let Some(events) = events else { return };
    let event_value = match decode_payload::<ProtoOaOrderErrorEvent>(msg) {
        Ok(event_value) => event_value,
        Err(e) => {
            tracing::warn!(target: "ctrader", error = %e, "failed to decode ORDER_ERROR_EVENT");
            return;
        }
    };
    let description = event_value.description.as_deref().unwrap_or_default();
    // The failure of an AMEND request (its id carries the verb): the order keeps working at its old terms, so
    // this is the amend's refusal, not a cancel's.
    if let Some(order_client_id) = amend_order_client_id(msg) {
        if let Ok(mut book) = positions.lock() {
            book.exit_amend_failed(order_client_id);
        }
        let event = amend_failed(order_client_id, &event_value.error_code, description);
        emit_event(events, positions, event);
        return;
    }
    let from_order = event_value.order_id.and_then(|oid| reverse_lookup(orders, oid));
    let envelope = msg.client_msg_id.as_deref().filter(|s| !s.is_empty());
    let (order_client_id, accepted) = match (from_order, envelope) {
        // Correlated via a known venue orderId → the order was accepted.
        (Some(order_client_id), _) => (order_client_id, true),
        // Only the envelope order client id known: a submit never accepted, or a close.
        (None, Some(order_client_id)) => (order_client_id.to_string(), false),
        // No correlation possible — do not fabricate an order client id.
        (None, None) => {
            tracing::warn!(
                target: "ctrader",
                error_code = %event_value.error_code,
                "ORDER_ERROR_EVENT with no correlatable order; dropping"
            );
            return;
        }
    };
    let refused = order_refusal(
        &order_client_id,
        accepted,
        &event_value.error_code,
        description,
        closes,
        positions,
    );
    if let Some(event) = refused {
        emit_event(events, positions, event);
    }
}

/// Record the referenced order's `order_client_id → order_id` mapping so the exec client can later
/// resolve an order client id to the numeric order id cTrader's cancel/amend requests need. Uses the order's
/// dedicated `order_client_id`, falling back to the `label` we set at submit time. No-op when the
/// event carries no order ref or no correlation id.
///
/// A DEFINITELY-TERMINAL execution event (full FILLED / CANCELLED / REJECTED / EXPIRED — classified
/// once by [`decode::execution_event_is_terminal`], not re-derived here) instead PRUNES the order client id from the
/// map. Cancel/amend for a live order resolve from the map before any terminal event arrives, so
/// removal never breaks them; keeping the entry forever, by contrast, would make every reconnect's
/// gap-diff warn for every historically-completed order client id (they never reappear in `RECONCILE_RES`,
/// which lists only PENDING orders) — so after this prune the gap-warn is actionable.
pub(super) fn update_order_map(
    event: &ProtoOaExecutionEvent,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
) {
    if let Some(order) = &event.order {
        let order_client_id =
            order.client_order_id.clone().or_else(|| order.trade_data.label.clone());
        if let Some(order_client_id) = order_client_id
            && !order_client_id.is_empty()
            && let Ok(mut map) = orders.lock()
        {
            if decode::execution_event_is_terminal(event) {
                map.forget(&order_client_id);
            } else {
                map.bind(&order_client_id, order.order_id);
            }
        }
    }
}

/// Keep the shared OPEN-position map in step with every execution event: an event carrying a
/// `position` ref UPSERTS it when the position is live+OPEN (`positions::tracked_position`) or
/// REMOVES it once it is CLOSED/zero-volume. This is the map the exec client reads to decide a
/// reducing submit's close-by-position-id routing, so it must reflect an open as soon as its fill
/// lands and drop it the moment a close flattens it. No-op for an event with no position ref (e.g.
/// a pending-order accept before any position exists).
///
/// ⚠ **A `position` ref this build cannot CLASSIFY invalidates the book's evidence flag**, by the
/// same split [`rebuild_position_book`] makes on the reconcile path
/// (`positions::row_is_known_to_carry_no_exposure`: CLOSED/CREATED is knowledge, everything else is
/// a hole). Without it the completeness discipline stopped at the reconcile — the flag is set ONCE,
/// from one past answer, and the book then drifts forwards on events for the whole life of the
/// socket, so an ERROR-status position ref would silently REMOVE a live row while the book went on
/// calling itself authoritative, and that manufactured absence would refuse the position's own exit
/// under `halt_admit = "verify"`. The entry is still dropped (routing runs on the best guess
/// available); only the claim to be EVIDENCE goes, until a reconcile answers again.
pub(super) fn update_position_map(event: &ProtoOaExecutionEvent, positions: &PositionsByIdShared) {
    let Some(p) = &event.position else { return };
    if let Ok(mut book) = positions.lock() {
        match positions::tracked_position(p) {
            Some((id, tracked)) => book.upsert(id, tracked),
            None => {
                book.remove(p.position_id);
                if !positions::row_is_known_to_carry_no_exposure(p) {
                    book.invalidate();
                    tracing::warn!(
                        target: "ctrader",
                        position_id = p.position_id,
                        position_status = p.position_status,
                        "an execution event carried a position this build cannot classify; the \
                         entry is dropped for routing and the book is no longer EVIDENCE (a halt \
                         under halt_admit=verify ADMITS) until a reconcile answers again"
                    );
                }
            }
        }
    }
}

/// The correlation id of the order an execution event references: its dedicated `order_client_id`,
/// else the `label` set at submit (the key [`update_order_map`] uses).
fn event_order_order_client_id(event: &ProtoOaExecutionEvent) -> Option<String> {
    let order = event.order.as_ref()?;
    order
        .client_order_id
        .clone()
        .or_else(|| order.trade_data.label.clone())
        .filter(|c| !c.is_empty())
}

/// Keep the bound-exit table ([`crate::execution::positions::BoundExit`]) in step with an execution event, AFTER
/// [`update_position_map`] has applied the event's position:
///
/// * a `position` ref that is gone (CLOSED, or the CREATED/zero-volume shape the venue really reports
///   after a close: `positions::row_is_known_to_carry_no_exposure`) ORPHANS every exit bound to it,
///   except the order this very event is about (its own terminal event ends its row below). This is
///   what cancels a bound exit when the position closed by ANY road (a market exit, `close_all`, the
///   venue, the UI): the venue does not, and a stale bound stop that reaches the market OPENS a position;
/// * an event about a bound exit whose position is NOT in the book (it closed between the submit and
///   the venue's accept) orphans that exit too, but ONLY while the book is evidence (`is_fetched`): after
///   an unclassifiable position ref or an unverified rebuild the absence is a hole, not a fact, and the next
///   event about a live exit must not cancel it;
/// * a terminal event of a bound exit drops its row; an `ORDER_REPLACED` records its new volume; an
///   `ORDER_CANCEL_REJECTED` puts a cancelling exit back in its position's cover.
pub(super) fn update_bound_exits(event: &ProtoOaExecutionEvent, positions: &PositionsByIdShared) {
    let own = event_order_order_client_id(event);
    let Ok(mut book) = positions.lock() else { return };
    if let Some(p) = &event.position
        && positions::tracked_position(p).is_none()
        && positions::row_is_known_to_carry_no_exposure(p)
    {
        book.orphan_exits_on(p.position_id, own.as_deref());
    }
    // An orphaned exit the connect-time look flagged ends here when its order does (filled, cancelled by hand).
    if decode::execution_event_is_terminal(event)
        && let Some(order) = &event.order
    {
        book.forget_orphaned_exit(order.order_id);
    }
    let Some(order_client_id) = own else { return };
    let kind = ProtoOaExecutionType::try_from(event.execution_type);
    if decode::execution_event_is_terminal(event) {
        book.unbind_exit(&order_client_id);
    } else if matches!(kind, Ok(ProtoOaExecutionType::OrderCancelRejected)) {
        // The venue refused a cancel of this exit: it still rests and still covers its position. FIRST in
        // the chain on purpose: when its position is gone too, the absence check below would orphan it
        // again and the refusal would never be seen, so the cancel would not be given up, the engine's own
        // would stay swallowed and the stop would go on resting.
        if book.cancel_in_flight(&order_client_id) {
            tracing::error!(
                target: "ctrader",
                order_client_id = %order_client_id,
                "the venue REFUSED the cancel of an exit whose position is gone: it may still rest and \
                 OPEN a position when it triggers - cancel it by hand"
            );
        }
        book.cancel_failed(&order_client_id);
    } else if let Some(b) = book.bound_exit(&order_client_id)
        && book.is_fetched()
        && book.position(b.position_id).is_none()
    {
        book.orphan_exit(&order_client_id);
    } else if matches!(kind, Ok(ProtoOaExecutionType::OrderReplaced))
        && let Some(order) = &event.order
    {
        // An amend changed the exit's volume: the position's room is counted with the new one.
        book.set_exit_volume(&order_client_id, order.trade_data.volume);
    }
}

/// The numeric `positionId` an execution event references — the deal's source position, else the
/// order's linked position — or `0` when it references none (an ordinary opening order before the
/// position exists). Used to spot a CLOSE-correlated event.
fn event_position_id(event: &ProtoOaExecutionEvent) -> i64 {
    event
        .deal
        .as_ref()
        .map(|d| d.position_id)
        .or_else(|| event.order.as_ref().and_then(|o| o.position_id))
        .unwrap_or(0)
}

/// Map an execution event to canonical `Event`s. When the event belongs to an in-flight CLOSE (its
/// `positionId` is registered in `closes`, keyed by a reducing submit's order client id), it is folded through
/// the per-order-client-id aggregator ([`map_close_event`]) so N FIFO close legs surface as ONE coherent
/// `OrderAccepted → OrderPartiallyFilled* → OrderFilled` lifecycle under that order client id — the closing
/// order carries no `clientOrderId` of ours, so this position-id correlation is the ONLY link back
/// to the submit. Every non-close event falls through to the stateless
/// `decode::execution_event_to_events`.
///
/// ⚠ **An event is the close's only if it carries NO order client id or the close's OWN.** A resting exit
/// BOUND to the same position (`ProtoOaNewOrderReq::position_id`) references that `positionId` too, and
/// its cancel or fill during a `market-exit` would otherwise terminalize the exit's order client id and strand its
/// fills. The closing order's own events carry the close's order client id as their `clientOrderId` (the envelope
/// id; measured 2026-10-26), and a bound exit's carry its own.
pub(super) fn execution_event_to_canonical(
    event: &ProtoOaExecutionEvent,
    symbols: &SymbolMap,
    money_digits: u32,
    closes: &CloseState,
) -> Vec<Event> {
    let position_id = event_position_id(event);
    let order_client_id = if position_id != 0 {
        closes.lock().ok().and_then(|t| t.order_client_id_for(position_id).map(str::to_string))
    } else {
        None
    };
    let own = event.order.as_ref().and_then(|o| o.client_order_id.as_deref());
    let order_client_id = order_client_id
        .filter(|close_order_client_id| own.is_none_or(|own| own == close_order_client_id));
    let mut events = match order_client_id {
        Some(order_client_id) => {
            map_close_event(event, &order_client_id, symbols, money_digits, closes)
        }
        None => decode::execution_event_to_events(event, symbols, money_digits),
    };
    // The swap a CLOSING deal realized rides with its fill (A26): only when this event emitted the fill, once per
    // deal id (`apply_funding` is not idempotent and a funding event has no id of its own), and a poisoned
    // tracker books nothing rather than risk a second time.
    if events.iter().any(|e| matches!(e, Event::Fill(_)))
        && let Some(swap) = decode::deal_swap_funding(event, symbols, money_digits)
        && let Some(deal) = event.deal.as_ref()
        && closes.lock().ok().is_some_and(|mut t| t.first_swap_of_deal(deal.deal_id))
    {
        crate::execution::close_detail::note_unmeasured_booking("swap");
        events.push(Event::Funding(swap));
    }
    events
}

/// The LEGAL terminal event for a failed close order client id. When earlier legs `progressed` the FSM to
/// `PartiallyFilled` (where `OrderRejected` would be an illegal, DROPPED transition) it is
/// `OrderCanceled` carrying the failure's `reason`: the close ends with what its legs filled, as FXCM's
/// does (`OrderPartiallyFilled` per leg fill, then `OrderCanceled`; owner ruling 3, 2026-11-11), so one
/// partial close reads the same on both venues. The filled legs already folded their own bare
/// `Event::Fill`s, so nothing moves money here. Nothing filled yet: a clean `OrderRejected`, under the catalog id
/// `refusal`. The `OrderCanceled` carries an id only when the failure is the VENUE'S refusal of the order it took
/// (`order.venue_refused`). PURE: does not touch the tracker, so it is safe to build at CAPTURE time (before a write is
/// attempted) without a side effect.
pub(super) fn close_terminal_event(
    order_client_id: &str,
    progressed: bool,
    reason: &str,
    ts: i64,
    refusal: vike_model::OrderRefusalId,
) -> Event {
    if progressed {
        let venue_refused = refusal == vike_model::OrderRefusalId::VenueRefused;
        Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: reason.into(),
            timestamp: ts,
            post_only_would_cross: false,
            refusal_id: vike_model::events::refusal_id_of(refusal.word()).filter(|_| venue_refused),
        })
    } else {
        Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: order_client_id.to_string(),
            reason: reason.into(),
            timestamp: ts,
            refusal_id: vike_model::events::refusal_id_of(refusal.word()),
        })
    }
}

/// Terminalize a close order client id on a FAILURE THAT HAS ALREADY HAPPENED (a leg reject, or the venue's
/// refusal as an `ERROR_RES` or an `ORDER_ERROR_EVENT`: [`order_refusal`]): FORGET it from the tracker and return
/// the legal terminal ([`close_terminal_event`]). Either is the venue's answer, so the catalog id is `order.venue_refused`.
/// The write-failure path does NOT use this — it must not forget at capture time (see
/// [`write_failure_event`]).
fn close_failure_terminal(
    order_client_id: &str,
    reason: &str,
    closes: &CloseState,
    ts: i64,
) -> Event {
    let progressed = matches!(
        closes.lock().ok().map(|mut t| t.resolve_failure(order_client_id)),
        Some(CloseFailure::Canceled)
    );
    close_terminal_event(
        order_client_id,
        progressed,
        reason,
        ts,
        vike_model::OrderRefusalId::VenueRefused,
    )
}

/// Fold ONE close-correlated execution event for reduce-order `order_client_id` into canonical events via the
/// shared [`CloseTracker`]. A REJECTED closing order terminates the order client id via
/// [`close_failure_terminal`] (Canceled-if-progressed, else Rejected — never a dropped illegal
/// transition); a CANCELLED one terminates as `OrderCanceled` (legal from `PartiallyFilled`).
/// Otherwise the event is an accept/(partial) fill — the tracker decides whether it emits the single
/// `OrderAccepted`, an intermediate `OrderPartiallyFilled`, or the terminal `OrderFilled` (once the
/// cumulative closed volume reaches the planned reduce total). The fill itself is built by the SAME
/// `decode::deal_fill` the live fill path uses, only with the substituted order client id — the closing
/// deal's `tradeSide` is already the reducing direction (a SELL deal closes a LONG), so the folded
/// `FillEvent` nets the position toward flat with no special-casing.
fn map_close_event(
    event: &ProtoOaExecutionEvent,
    order_client_id: &str,
    symbols: &SymbolMap,
    money_digits: u32,
    closes: &CloseState,
) -> Vec<Event> {
    let ts = decode::lifecycle_timestamp(event);
    match ProtoOaExecutionType::try_from(event.execution_type) {
        Ok(ProtoOaExecutionType::OrderRejected) => {
            let reason = event.error_code.clone().unwrap_or_default();
            return vec![close_failure_terminal(order_client_id, &reason, closes, ts)];
        }
        Ok(ProtoOaExecutionType::OrderCancelled) => {
            if let Ok(mut t) = closes.lock() {
                t.forget(order_client_id);
            }
            return vec![Event::OrderCanceled(OrderCanceled {
                order_client_id: order_client_id.to_string(),
                reason: event.error_code.clone().unwrap_or_default().into(),
                timestamp: ts,
                post_only_would_cross: false,
                refusal_id: None,
            })];
        }
        _ => {}
    }

    let fill_volume = event.deal.as_ref().map(|d| d.filled_volume).unwrap_or(0);
    let Some(emit) = closes.lock().ok().map(|mut t| t.on_event(order_client_id, fill_volume))
    else {
        return Vec::new();
    };
    let venue_order_id = event.order.as_ref().map(|o| o.order_id).unwrap_or(0);
    let accepted = |out: &mut Vec<Event>| {
        out.push(Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.to_string(),
            venue_order_id: Some(venue_order_id.to_string().into()),
            timestamp: ts,
        }));
    };
    let mut out = Vec::new();
    match emit {
        CloseEmit::AcceptOnly => accepted(&mut out),
        CloseEmit::Partial { accept_first } => {
            if accept_first {
                accepted(&mut out);
            }
            if let Some(fill) = decode::deal_fill(event, order_client_id, symbols, money_digits) {
                let fts = fill.timestamp;
                // DUAL-PUBLISH (mirrors `decode::execution_event_to_events` and every crypto venue):
                // the bare `Event::Fill` folds into `Account` (REDUCES the position — the whole point
                // of close-position), the wrap advances the OMS order FSM. The closing deal's side is
                // already the reducing direction (a SELL deal closes a LONG), so the folded fill nets
                // toward flat with no special-casing.
                out.push(Event::Fill(fill.clone()));
                out.push(Event::OrderPartiallyFilled(OrderPartiallyFilled {
                    order_client_id: order_client_id.to_string(),
                    fill,
                    timestamp: fts,
                }));
            }
        }
        CloseEmit::Final { accept_first } => {
            if accept_first {
                accepted(&mut out);
            }
            if let Some(fill) = decode::deal_fill(event, order_client_id, symbols, money_digits) {
                let fts = fill.timestamp;
                // Dual-publish: bare `Event::Fill` (Account fold) THEN the terminal wrap (FSM). See
                // the `Partial` arm above for why the bare fill is load-bearing.
                out.push(Event::Fill(fill.clone()));
                out.push(Event::OrderFilled(OrderFilled {
                    order_client_id: order_client_id.to_string(),
                    fill,
                    timestamp: fts,
                }));
            }
        }
    }
    out
}
