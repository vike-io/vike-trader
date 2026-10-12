//! Inbound frame dispatch: quotes, bar seeds, venue errors, execution events, the close-leg fold.

use vike_data::LiveDataSink;
use vike_exec::EventSender;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderFilled,
    OrderPartiallyFilled, OrderRejected, TradeId,
};
use vike_model::{Bar, QuoteTick, now_ms};

use super::stream::decode_error;
use super::{CloseState, ConnError, OrderIdMap, PositionMap, VENUE, decode_payload};
use crate::event_mapper;
use crate::positions::{self, CloseEmit, CloseFailure};
use crate::proto::{
    ProtoMessage, ProtoOaExecutionEvent, ProtoOaExecutionType, ProtoOaGetTrendbarsRes,
    ProtoOaOrderErrorEvent, ProtoOaSpotEvent, ProtoOaTrendbarPeriod, pt,
};
use crate::symbols::SymbolMap;

#[cfg(doc)]
use super::outbound::{write_command, write_failure_event};
#[cfg(doc)]
use super::reconcile::rebuild_position_book;
#[cfg(doc)]
use crate::positions::CloseTracker;

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
        pt::ERROR_RES => on_error_res(msg, events, closes),
        pt::ORDER_ERROR_EVENT => on_order_error_event(msg, events, orders),
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
                    update_order_map(&ev, orders);
                    for event in exec_event_to_canonical(&ev, symbols, money_digits, closes) {
                        if events.blocking_send(event).is_err() {
                            tracing::warn!(target: "ctrader", "core ingest gone; dropping exec event");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "ctrader", error = %e, "failed to decode EXECUTION_EVENT");
                }
            }
        }
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
fn on_error_res(msg: &ProtoMessage, events: Option<&EventSender>, closes: &CloseState) {
    let ConnError::Venue { error_code, description } = decode_error(msg) else {
        return; // undecodable error frame — nothing correlatable to synthesize
    };
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
                Event::OrderRejected(OrderRejected {
                    client_order_id: coid.to_string(),
                    reason: reason.into(),
                    ts: now_ms(),
                })
            };
            if events.blocking_send(event).is_err() {
                tracing::warn!(target: "ctrader", "core ingest gone; dropping synthesized reject");
            }
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
fn on_order_error_event(msg: &ProtoMessage, events: Option<&EventSender>, orders: &OrderIdMap) {
    let Some(events) = events else { return };
    let ev = match decode_payload::<ProtoOaOrderErrorEvent>(msg) {
        Ok(ev) => ev,
        Err(e) => {
            tracing::warn!(target: "ctrader", error = %e, "failed to decode ORDER_ERROR_EVENT");
            return;
        }
    };
    let reason = error_reason(&ev.error_code, ev.description.as_deref().unwrap_or_default());
    let from_order = ev.order_id.and_then(|oid| reverse_lookup(orders, oid));
    let event = match from_order {
        // Correlated via a known venue orderId → the order was accepted → non-terminal advisory.
        Some(coid) => Event::OrderCancelRejected(OrderCancelRejected {
            client_order_id: coid,
            reason: reason.into(),
            ts: now_ms(),
        }),
        None => match msg.client_msg_id.as_deref().filter(|s| !s.is_empty()) {
            // Only the envelope coid known (never accepted) → terminal submit reject.
            Some(coid) => Event::OrderRejected(OrderRejected {
                client_order_id: coid.to_string(),
                reason: reason.into(),
                ts: now_ms(),
            }),
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
    if events.blocking_send(event).is_err() {
        tracing::warn!(target: "ctrader", "core ingest gone; dropping synthesized reject");
    }
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
fn exec_event_to_canonical(
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
    match coid {
        Some(coid) => map_close_event(ev, &coid, symbols, money_digits, closes),
        None => event_mapper::exec_event_to_events(ev, symbols, money_digits),
    }
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
