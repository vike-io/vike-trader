//! Inbound frame dispatch: quotes, bar seeds, venue errors, execution events, the close-leg fold.

use vike_data::LiveDataSink;
use vike_exec::EventSender;
use vike_marketdata::{Bar, QuoteTick};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderFilled,
    OrderModifyRejected, OrderPartiallyFilled, OrderRejected, TradeId,
};
use vike_model::now_ms;

use super::outbound::AMEND_MSG_PREFIX;
use super::stream::decode_error;
use super::{CloseState, ConnError, OrderIdMap, PositionMap, VENUE, decode_payload};
use crate::event_mapper;
use crate::positions::{self, CloseEmit, CloseFailure};
use crate::proto::{
    ProtoMessage, ProtoOaExecutionEvent, ProtoOaExecutionType, ProtoOaGetTrendbarsRes,
    ProtoOaOrderErrorEvent, ProtoOaSpotEvent, ProtoOaSymbolChangedEvent, ProtoOaTrendbarPeriod, pt,
};
use crate::symbols::SymbolMap;

#[cfg(doc)]
use super::outbound::{write_command, write_failure_event};
#[cfg(doc)]
use super::reconcile::rebuild_position_book;
#[cfg(doc)]
use crate::positions::CloseTracker;

/// Hand one canonical event to the core, AFTER the split-exit table has folded it: an event of a leg of an exit that
/// spans positions becomes its parent's single lifecycle (or nothing: `crate::exit_legs`), every other event goes out
/// as it is. The ONE place an event leaves this connection for the core, so a leg's event can never reach the engine
/// under a coid the engine does not know. A poisoned book sends the event unfolded rather than lose it.
pub(super) fn emit_event(events: &EventSender, positions: &PositionMap, event: Event) {
    let out = match positions.lock() {
        Ok(mut book) => book.fold_exit_leg_event(event),
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
/// `on_get_trendbars_res`) to the `LiveDataSink`, and (when exec is wired) routes
/// `EXECUTION_EVENT` to the `EventSender` after updating the coid→order-id map. `money_digits` is
/// the account's `ProtoOATrader.money_digits` (fetched once at handshake, refreshed on reconnect —
/// see `open_and_handshake`) — threaded into `event_mapper::exec_event_to_events` as the fallback
/// exponent for a fill's signed commission integer, when the deal itself omits its own
/// `money_digits`.
pub(super) fn on_inbound(
    msg: &ProtoMessage,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn LiveDataSink,
    events: Option<&EventSender>,
    orders: &OrderIdMap,
    positions: &PositionMap,
    closes: &CloseState,
) {
    match msg.payload_type {
        pt::HEARTBEAT_EVENT => {} // peer keepalive — nothing to do
        pt::ERROR_RES => on_error_res(msg, events, closes, positions),
        pt::ORDER_ERROR_EVENT => on_order_error_event(msg, events, orders, positions),
        pt::SPOT_EVENT => match decode_payload::<ProtoOaSpotEvent>(msg) {
            Ok(ev) => {
                if let Some((symbol, bid, ask)) = event_mapper::spot_to_quote(&ev, symbols) {
                    let quote = QuoteTick {
                        ts: ev.timestamp.unwrap_or_else(vike_model::now_ms),
                        local_ts: vike_model::now_ms(),
                        bid,
                        ask,
                        bid_size: 0.0,
                        ask_size: 0.0,
                        symbol: symbol.clone(),
                    };
                    sink.quote(VENUE, &symbol, quote);
                }
                for (symbol, interval, bar) in event_mapper::spot_to_bars(&ev, symbols) {
                    // A live trendbar riding a SPOT_EVENT is the bar STILL forming (cTrader keeps
                    // pushing updated snapshots of the current period until it closes) — the
                    // conflating lane is the correct sink verb, not `close_bar`.
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
            // Order flow: decode, update the coid→order-id correlation map, and push canonical
            // events onto the ingest lane. Only active when an `EventSender` was wired
            // (`connect_and_auth_exec`); with data-only wiring an execution event is a no-op.
            let Some(events) = events else { return };
            match decode_payload::<ProtoOaExecutionEvent>(msg) {
                Ok(ev) => {
                    // Keep the OPEN-position map fresh (drives the exec client's reduce routing) and
                    // the coid→orderId map, then map to canonical events — routing a CLOSE-correlated
                    // event through the per-coid aggregator (see `exec_event_to_canonical`).
                    update_position_map(&ev, positions);
                    update_bound_exits(&ev, positions);
                    update_order_map(&ev, orders);
                    for event in exec_event_to_canonical(&ev, symbols, money_digits, closes) {
                        emit_event(events, positions, event);
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "ctrader", error = %e, "failed to decode EXECUTION_EVENT");
                }
            }
        }
        // The broker changed a symbol (its trading state is what the exec client reads): noted on the map, and the
        // actor re-reads it between two frames (`crate::conn::symbol_state`). Noted HERE, not in the actor loop, so
        // an event that arrived inside another round trip (and was buffered) is not lost.
        pt::SYMBOL_CHANGED_EVENT => match decode_payload::<ProtoOaSymbolChangedEvent>(msg) {
            Ok(ev) => symbols.note_changed(&ev.symbol_id),
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
/// map every trendbar entry through `event_mapper::trendbar_to_bar` (skipping incomplete entries per
/// that function's contract), sort ascending by timestamp (the wire order is not documented/
/// guaranteed), and hand the whole batch to `LiveDataSink::seed_bars` in ONE call — the dedicated
/// "recent history before live closes start arriving" verb (see that trait method's doc).
/// Best-effort: an unresolvable symbol id/period or an empty/all-incomplete trendbar list is a
/// silent no-op (one `warn!`, never per-bar) — the live `SubscribeTrendbar` stream (sent
/// independently, and unconditionally, by `data::CtraderData::subscribe_bars`) starts regardless
/// of whether this seed succeeds.
fn on_get_trendbars_res(
    res: &ProtoOaGetTrendbarsRes,
    symbols: &SymbolMap,
    sink: &dyn LiveDataSink,
) {
    let Some(symbol_id) = res.symbol_id else {
        tracing::warn!(target: "ctrader", "GET_TRENDBARS_RES missing symbolId; dropping seed");
        return;
    };
    let Some(name) = symbols.name_of(symbol_id) else {
        tracing::warn!(
            target: "ctrader",
            symbol_id,
            "GET_TRENDBARS_RES for unknown symbol id; dropping seed"
        );
        return;
    };
    let Ok(period) = ProtoOaTrendbarPeriod::try_from(res.period) else {
        tracing::warn!(target: "ctrader", "GET_TRENDBARS_RES has an unrecognized period; dropping seed");
        return;
    };
    let mut bars: Vec<Bar> =
        res.trendbar.iter().filter_map(event_mapper::trendbar_to_bar).collect();
    if bars.is_empty() {
        return; // nothing to seed (e.g. a brand-new symbol/period with no history yet) — not an error
    }
    bars.sort_by_key(|b| b.ts);
    sink.seed_bars(VENUE, name, event_mapper::interval_for_trendbar_period(period), bars);
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

/// The coid of the AMEND an envelope `clientMsgId` names (`AMEND_MSG_PREFIX` + coid), if it names one.
fn amend_coid(msg: &ProtoMessage) -> Option<&str> {
    msg.client_msg_id
        .as_deref()
        .and_then(|id| id.strip_prefix(AMEND_MSG_PREFIX))
        .filter(|c| !c.is_empty())
}

/// The non-terminal event for a failed AMEND: the order is still working at its old terms.
fn amend_failed(coid: &str, reason: String) -> Event {
    Event::OrderModifyRejected(OrderModifyRejected {
        client_order_id: coid.to_string(),
        reason: reason.into(),
        ts: now_ms(),
    })
}

/// Reverse the coid→order-id correlation map: find the `client_order_id` a numeric venue
/// `order_id` belongs to, if any. Used to correlate an `ORDER_ERROR_EVENT` (which carries only the
/// `orderId`) back to the coid the OMS keys on.
fn reverse_lookup(orders: &OrderIdMap, order_id: i64) -> Option<String> {
    let map = orders.lock().ok()?;
    map.iter().find(|&(_, &oid)| oid == order_id).map(|(coid, _)| coid.clone())
}

/// Route an `ERROR_RES` (2142) frame. cTrader echoes the request's envelope `clientMsgId` on the
/// error to that request; the order commands set that field to the order's `client_order_id`
/// ([`write_command`]), so a non-empty `clientMsgId` here identifies the failed order request. We
/// synthesize a TERMINAL [`Event::OrderRejected`] for it (the safe default: the verb — submit vs
/// cancel vs amend — is not distinguishable from the envelope alone, and a submit reject is the
/// common case; a cancel/amend failure surfaces instead as an `ORDER_ERROR_EVENT`, handled below).
/// An `ERROR_RES` with an EMPTY `clientMsgId` is not order-correlated — keep the original `warn!`.
/// Auth-flavored `ERROR_RES` never reaches here: the actor loop diverts it to token refresh first.
///
/// If the coid is a CLOSE coid (a `ProtoOAClosePositionReq` the venue rejected at the protocol
/// layer), it is resolved through [`close_failure_terminal`] instead — Filled-if-progressed, else
/// Rejected — and FORGOTTEN from the tracker, so a wire-level close reject can never (a) leave a
/// stale `by_position` entry that misroutes a later leg's fill, nor (b) strand the coid with an
/// illegal `OrderRejected` from `PartiallyFilled`. An ordinary order's coid gets the plain reject.
fn on_error_res(
    msg: &ProtoMessage,
    events: Option<&EventSender>,
    closes: &CloseState,
    positions: &PositionMap,
) {
    let ConnError::Venue { error_code, description } = decode_error(msg) else {
        return; // undecodable error frame — nothing correlatable to synthesize
    };
    // A failed AMEND (a protocol-level refusal of the request) refuses the AMEND: the order is still working
    // at its old terms, so it must not become a terminal reject.
    if let (Some(coid), Some(events)) = (amend_coid(msg), events) {
        let reason = error_reason(&error_code, &description);
        // The exit keeps its size (and stops counting the one the refused amend asked for).
        if let Ok(mut book) = positions.lock() {
            book.exit_amend_failed(coid);
        }
        emit_event(events, positions, amend_failed(coid, reason));
        return;
    }
    let coid = msg.client_msg_id.as_deref().filter(|s| !s.is_empty());
    match (coid, events) {
        (Some(coid), Some(events)) => {
            let reason = error_reason(&error_code, &description);
            // A close coid resolves via the tracker (forget + legal terminal); a normal one is a
            // plain terminal reject.
            let is_close = closes.lock().ok().is_some_and(|t| t.is_tracked(coid));
            let event = if is_close {
                close_failure_terminal(coid, &reason, closes, now_ms())
            } else {
                // A bound exit the venue refused at the protocol layer is over: drop its row.
                if let Ok(mut book) = positions.lock() {
                    book.unbind_exit(coid);
                }
                Event::OrderRejected(OrderRejected {
                    cause: None,
                    client_order_id: coid.to_string(),
                    reason: reason.into(),
                    ts: now_ms(),
                })
            };
            emit_event(events, positions, event);
        }
        // Empty clientMsgId (not order-correlated) or data-only wiring: log, don't fabricate.
        _ => tracing::warn!(target: "ctrader", %error_code, %description, "venue error frame"),
    }
}

/// Route an `ORDER_ERROR_EVENT` (2132) frame — an error against a specific order request. Reverse-
/// correlate its `orderId` to a `client_order_id` via the shared [`OrderIdMap`]: a hit means the
/// order was accepted (we only learn its `orderId` from an execution event), so the failed request
/// was a cancel/amend → NON-terminal [`Event::OrderCancelRejected`] (the order stays live). If the
/// `orderId` is absent/uncorrelatable but the envelope `clientMsgId` carries a coid, the order was
/// never accepted → TERMINAL [`Event::OrderRejected`]. With no correlation at all we `warn!` rather
/// than fabricate a wrong coid. A no-op under data-only wiring (no `EventSender`).
fn on_order_error_event(
    msg: &ProtoMessage,
    events: Option<&EventSender>,
    orders: &OrderIdMap,
    positions: &PositionMap,
) {
    let Some(events) = events else { return };
    let ev = match decode_payload::<ProtoOaOrderErrorEvent>(msg) {
        Ok(ev) => ev,
        Err(e) => {
            tracing::warn!(target: "ctrader", error = %e, "failed to decode ORDER_ERROR_EVENT");
            return;
        }
    };
    let reason = error_reason(&ev.error_code, ev.description.as_deref().unwrap_or_default());
    // The failure of an AMEND request (its id carries the verb): the order keeps working at its old terms, so
    // this is the amend's refusal, not a cancel's.
    if let Some(coid) = amend_coid(msg) {
        if let Ok(mut book) = positions.lock() {
            book.exit_amend_failed(coid);
        }
        emit_event(events, positions, amend_failed(coid, reason));
        return;
    }
    let from_order = ev.order_id.and_then(|oid| reverse_lookup(orders, oid));
    let event = match from_order {
        // Correlated via a known venue orderId → the order was accepted → non-terminal advisory.
        Some(coid) => {
            // The bridge's own cancel of an orphaned bound exit was refused: give up on it (no retry
            // loop) and let the engine's cancel of the same order through.
            if let Ok(mut book) = positions.lock() {
                if book.cancel_in_flight(&coid) {
                    tracing::error!(
                        target: "ctrader",
                        coid = %coid,
                        %reason,
                        "the venue REFUSED the cancel of an exit whose position is gone: it may \
                         still rest and OPEN a position when it triggers - cancel it by hand"
                    );
                }
                book.cancel_failed(&coid);
            }
            Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: coid,
                reason: reason.into(),
                ts: now_ms(),
            })
        }
        None => match msg.client_msg_id.as_deref().filter(|s| !s.is_empty()) {
            // Only the envelope coid known (never accepted) → terminal submit reject.
            Some(coid) => {
                if let Ok(mut book) = positions.lock() {
                    book.unbind_exit(coid);
                }
                Event::OrderRejected(OrderRejected {
                    cause: None,
                    client_order_id: coid.to_string(),
                    reason: reason.into(),
                    ts: now_ms(),
                })
            }
            // No correlation possible — do not fabricate a coid.
            None => {
                tracing::warn!(
                    target: "ctrader",
                    error_code = %ev.error_code,
                    "ORDER_ERROR_EVENT with no correlatable order; dropping"
                );
                return;
            }
        },
    };
    emit_event(events, positions, event);
}

/// Record the referenced order's `client_order_id → order_id` mapping so the exec client can later
/// resolve a coid to the numeric order id cTrader's cancel/amend requests need. Uses the order's
/// dedicated `client_order_id`, falling back to the `label` we set at submit time. No-op when the
/// event carries no order ref or no correlation id.
///
/// A DEFINITELY-TERMINAL execution event (full FILLED / CANCELLED / REJECTED / EXPIRED — classified
/// once by [`event_mapper::exec_event_is_terminal`], not re-derived here) instead PRUNES the coid from the
/// map. Cancel/amend for a live order resolve from the map before any terminal event arrives, so
/// removal never breaks them; keeping the entry forever, by contrast, would make every reconnect's
/// gap-diff warn for every historically-completed coid (they never reappear in `RECONCILE_RES`,
/// which lists only PENDING orders) — so after this prune the gap-warn is actionable.
pub(super) fn update_order_map(ev: &ProtoOaExecutionEvent, orders: &OrderIdMap) {
    if let Some(order) = &ev.order {
        let coid = order.client_order_id.clone().or_else(|| order.trade_data.label.clone());
        if let Some(coid) = coid
            && !coid.is_empty()
            && let Ok(mut map) = orders.lock()
        {
            if event_mapper::exec_event_is_terminal(ev) {
                map.remove(&coid);
            } else {
                map.insert(coid, order.order_id);
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
pub(super) fn update_position_map(ev: &ProtoOaExecutionEvent, positions: &PositionMap) {
    let Some(p) = &ev.position else { return };
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

/// The correlation id of the order an execution event references: its dedicated `client_order_id`,
/// else the `label` set at submit (the key [`update_order_map`] uses).
fn event_order_coid(ev: &ProtoOaExecutionEvent) -> Option<String> {
    let order = ev.order.as_ref()?;
    order
        .client_order_id
        .clone()
        .or_else(|| order.trade_data.label.clone())
        .filter(|c| !c.is_empty())
}

/// Keep the bound-exit table ([`crate::positions::BoundExit`]) in step with an execution event, AFTER
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
pub(super) fn update_bound_exits(ev: &ProtoOaExecutionEvent, positions: &PositionMap) {
    let own = event_order_coid(ev);
    let Ok(mut book) = positions.lock() else { return };
    if let Some(p) = &ev.position
        && positions::tracked_position(p).is_none()
        && positions::row_is_known_to_carry_no_exposure(p)
    {
        book.orphan_exits_on(p.position_id, own.as_deref());
    }
    // An orphaned exit the connect-time look flagged ends here when its order does (filled, cancelled by hand).
    if event_mapper::exec_event_is_terminal(ev)
        && let Some(order) = &ev.order
    {
        book.forget_orphaned_exit(order.order_id);
    }
    let Some(coid) = own else { return };
    let kind = ProtoOaExecutionType::try_from(ev.execution_type);
    if event_mapper::exec_event_is_terminal(ev) {
        book.unbind_exit(&coid);
    } else if matches!(kind, Ok(ProtoOaExecutionType::OrderCancelRejected)) {
        // The venue refused a cancel of this exit: it still rests and still covers its position. FIRST in
        // the chain on purpose: when its position is gone too, the absence check below would orphan it
        // again and the refusal would never be seen, so the cancel would not be given up, the engine's own
        // would stay swallowed and the stop would go on resting.
        if book.cancel_in_flight(&coid) {
            tracing::error!(
                target: "ctrader",
                coid = %coid,
                "the venue REFUSED the cancel of an exit whose position is gone: it may still rest and \
                 OPEN a position when it triggers - cancel it by hand"
            );
        }
        book.cancel_failed(&coid);
    } else if let Some(b) = book.bound_exit(&coid)
        && book.is_fetched()
        && book.position(b.position_id).is_none()
    {
        book.orphan_exit(&coid);
    } else if matches!(kind, Ok(ProtoOaExecutionType::OrderReplaced))
        && let Some(order) = &ev.order
    {
        // An amend changed the exit's volume: the position's room is counted with the new one.
        book.set_exit_volume(&coid, order.trade_data.volume);
    }
}

/// The numeric `positionId` an execution event references — the deal's source position, else the
/// order's linked position — or `0` when it references none (an ordinary opening order before the
/// position exists). Used to spot a CLOSE-correlated event.
fn event_position_id(ev: &ProtoOaExecutionEvent) -> i64 {
    ev.deal
        .as_ref()
        .map(|d| d.position_id)
        .or_else(|| ev.order.as_ref().and_then(|o| o.position_id))
        .unwrap_or(0)
}

/// Map an execution event to canonical `Event`s. When the event belongs to an in-flight CLOSE (its
/// `positionId` is registered in `closes`, keyed by a reducing submit's coid), it is folded through
/// the per-coid aggregator ([`map_close_event`]) so N FIFO close legs surface as ONE coherent
/// `OrderAccepted → OrderPartiallyFilled* → OrderFilled` lifecycle under that coid — the closing
/// order carries no `clientOrderId` of ours, so this position-id correlation is the ONLY link back
/// to the submit. Every non-close event falls through to the stateless
/// `event_mapper::exec_event_to_events`.
///
/// ⚠ **An event is the close's only if it carries NO client order id or the close's OWN.** A resting exit
/// BOUND to the same position (`ProtoOaNewOrderReq::position_id`) references that `positionId` too, and
/// its cancel or fill during a `market-exit` would otherwise terminalize the exit's coid and strand its
/// fills. The closing order's own events carry the close's coid as their `clientOrderId` (the envelope
/// id; measured 2026-10-26), and a bound exit's carry its own.
pub(super) fn exec_event_to_canonical(
    ev: &ProtoOaExecutionEvent,
    symbols: &SymbolMap,
    money_digits: u32,
    closes: &CloseState,
) -> Vec<Event> {
    let position_id = event_position_id(ev);
    let coid = if position_id != 0 {
        closes.lock().ok().and_then(|t| t.coid_for(position_id).map(str::to_string))
    } else {
        None
    };
    let own = ev.order.as_ref().and_then(|o| o.client_order_id.as_deref());
    let coid = coid.filter(|close_coid| own.is_none_or(|own| own == close_coid));
    let mut events = match coid {
        Some(coid) => map_close_event(ev, &coid, symbols, money_digits, closes),
        None => event_mapper::exec_event_to_events(ev, symbols, money_digits),
    };
    // The swap a CLOSING deal realized rides with its fill (A26): only when this event emitted the fill, once per
    // deal id (`apply_funding` is not idempotent and a funding event has no id of its own), and a poisoned
    // tracker books nothing rather than risk a second time.
    if events.iter().any(|e| matches!(e, Event::Fill(_)))
        && let Some(swap) = event_mapper::deal_swap_funding(ev, symbols, money_digits)
        && let Some(deal) = ev.deal.as_ref()
        && closes.lock().ok().is_some_and(|mut t| t.first_swap_of_deal(deal.deal_id))
    {
        crate::close_detail::note_unmeasured_booking("swap");
        events.push(Event::Funding(swap));
    }
    events
}

/// A ZERO-quantity `FillEvent` that gives a close coid a LEGAL FSM terminal without moving money:
/// used to terminalize a partially-filled close (some legs done) as `OrderFilled` when a later leg
/// FAILS — an `OrderRejected` from `PartiallyFilled` is illegal and would be DROPPED, stranding the
/// order. The already-filled legs folded their own bare `Event::Fill` into the Account, so this
/// carries `last_qty = 0.0`, and no bare `Event::Fill` is emitted alongside it (nothing to fold).
///
/// ⚠ The `trade_id` used to be the EMPTY string, deliberately, to BYPASS the engine's
/// `seen_fsm_trade_ids` dedup "so the wrap always applies". That made this the one site in the tree
/// depending on the dedup hole, and it cannot survive a `TradeId` that has no empty value. It is now
/// a deterministic per-coid id, which is not a behaviour regression but the same intent stated
/// safely: the id is a pure function of `coid`, so re-terminalizing the SAME close is the only thing
/// it dedups — and that is a duplicate by definition (the FSM refuses a second terminal from a
/// terminal state anyway). One coid can never need two DISTINCT zero-qty terminals.
fn fsm_terminal_fill(coid: &str, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::prefixed("CTRADER-FSMTERM-", coid),
        client_order_id: coid.to_string(),
        venue: VENUE.into(),
        symbol: "".into(),
        side: 0,
        last_qty: 0.0,
        last_px: 0.0,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: Default::default(),
        ts,
        mark_price: None,
        position_side: Default::default(),
    }
}

/// The LEGAL terminal event for a failed close coid: `OrderFilled` (for the volume already closed)
/// when earlier legs `progressed` the FSM to `PartiallyFilled` — where `OrderRejected` would be an
/// illegal, DROPPED transition — else a clean `OrderRejected`. PURE: does not touch the tracker, so
/// it is safe to build at CAPTURE time (before a write is attempted) without a side effect.
pub(super) fn close_terminal_event(coid: &str, progressed: bool, reason: &str, ts: i64) -> Event {
    if progressed {
        Event::OrderFilled(OrderFilled {
            client_order_id: coid.to_string(),
            fill: fsm_terminal_fill(coid, ts),
            ts,
        })
    } else {
        Event::OrderRejected(OrderRejected {
            cause: None,
            client_order_id: coid.to_string(),
            reason: reason.into(),
            ts,
        })
    }
}

/// Terminalize a close coid on a FAILURE THAT HAS ALREADY HAPPENED (a leg reject or a wire
/// `ERROR_RES`): FORGET it from the tracker and return the legal terminal ([`close_terminal_event`]).
/// The write-failure path does NOT use this — it must not forget at capture time (see
/// [`write_failure_event`]).
fn close_failure_terminal(coid: &str, reason: &str, closes: &CloseState, ts: i64) -> Event {
    let progressed = matches!(
        closes.lock().ok().map(|mut t| t.resolve_failure(coid)),
        Some(CloseFailure::TerminalFilled)
    );
    close_terminal_event(coid, progressed, reason, ts)
}

/// Fold ONE close-correlated execution event for reduce-order `coid` into canonical events via the
/// shared [`CloseTracker`]. A REJECTED closing order terminates the coid via
/// [`close_failure_terminal`] (Filled-if-progressed, else Rejected — never a dropped illegal
/// transition); a CANCELLED one terminates as `OrderCanceled` (legal from `PartiallyFilled`).
/// Otherwise the event is an accept/(partial) fill — the tracker decides whether it emits the single
/// `OrderAccepted`, an intermediate `OrderPartiallyFilled`, or the terminal `OrderFilled` (once the
/// cumulative closed volume reaches the planned reduce total). The fill itself is built by the SAME
/// `event_mapper::deal_fill` the live fill path uses, only with the substituted coid — the closing
/// deal's `tradeSide` is already the reducing direction (a SELL deal closes a LONG), so the folded
/// `FillEvent` nets the position toward flat with no special-casing.
fn map_close_event(
    ev: &ProtoOaExecutionEvent,
    coid: &str,
    symbols: &SymbolMap,
    money_digits: u32,
    closes: &CloseState,
) -> Vec<Event> {
    let ts = event_mapper::lifecycle_ts(ev);
    match ProtoOaExecutionType::try_from(ev.execution_type) {
        Ok(ProtoOaExecutionType::OrderRejected) => {
            let reason = ev.error_code.clone().unwrap_or_default();
            return vec![close_failure_terminal(coid, &reason, closes, ts)];
        }
        Ok(ProtoOaExecutionType::OrderCancelled) => {
            if let Ok(mut t) = closes.lock() {
                t.forget(coid);
            }
            return vec![Event::OrderCanceled(OrderCanceled {
                client_order_id: coid.to_string(),
                reason: ev.error_code.clone().unwrap_or_default().into(),
                ts,
            })];
        }
        _ => {}
    }

    let fill_volume = ev.deal.as_ref().map(|d| d.filled_volume).unwrap_or(0);
    let Some(emit) = closes.lock().ok().map(|mut t| t.on_event(coid, fill_volume)) else {
        return Vec::new();
    };
    let venue_order_id = ev.order.as_ref().map(|o| o.order_id).unwrap_or(0);
    let accepted = |out: &mut Vec<Event>| {
        out.push(Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: Some(venue_order_id.to_string().into()),
            ts,
        }));
    };
    let mut out = Vec::new();
    match emit {
        CloseEmit::AcceptOnly => accepted(&mut out),
        CloseEmit::Partial { accept_first } => {
            if accept_first {
                accepted(&mut out);
            }
            if let Some(fill) = event_mapper::deal_fill(ev, coid, symbols, money_digits) {
                let fts = fill.ts;
                // DUAL-PUBLISH (mirrors `event_mapper::exec_event_to_events` and every crypto venue):
                // the bare `Event::Fill` folds into `Account` (REDUCES the position — the whole point
                // of close-position), the wrap advances the OMS order FSM. The closing deal's side is
                // already the reducing direction (a SELL deal closes a LONG), so the folded fill nets
                // toward flat with no special-casing.
                out.push(Event::Fill(fill.clone()));
                out.push(Event::OrderPartiallyFilled(OrderPartiallyFilled {
                    client_order_id: coid.to_string(),
                    fill,
                    ts: fts,
                }));
            }
        }
        CloseEmit::Final { accept_first } => {
            if accept_first {
                accepted(&mut out);
            }
            if let Some(fill) = event_mapper::deal_fill(ev, coid, symbols, money_digits) {
                let fts = fill.ts;
                // Dual-publish: bare `Event::Fill` (Account fold) THEN the terminal wrap (FSM). See
                // the `Partial` arm above for why the bare fill is load-bearing.
                out.push(Event::Fill(fill.clone()));
                out.push(Event::OrderFilled(OrderFilled {
                    client_order_id: coid.to_string(),
                    fill,
                    ts: fts,
                }));
            }
        }
    }
    out
}
