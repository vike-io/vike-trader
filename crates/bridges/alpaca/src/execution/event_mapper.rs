//! Pure Alpaca ↔ vike mapping (no I/O but the kept-list read that names a fill of an order nobody recorded,
//! `crate::names::user_of`; the second oracle for the exec path). Order-body construction (under the wire name the
//! caller read off the kept row), POST-response → events, and `/v2/events/trades` SSE object → events.
//! `format_to_step` is the pinned Decimal wire site.

use vike_bridge_core::format::format_to_step;
use vike_bridge_core::per_symbol::OrderClientIdSymbols;
use vike_bridge_core::transport::http_error::VenueApiHttpError;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderExpired,
    OrderRejected, TradeId,
};
use vike_model::{OrderRequest, OrderType, TimeInForce};
use vike_utility::calendar::{MissingZone, parse_rfc3339_ms};

const VENUE: &str = "alpaca";

/// The `reason` of the `OrderCanceled` that ends an order on the trade event `rejected` ([`decode_trade_event`]).
pub(crate) const REFUSED_AFTER_TAKING: &str =
    "rejected: Alpaca refused the order after taking it; no further updates will occur";

/// The `reason` of the `OrderCancelRejected` the trade event `order_cancel_rejected` is ([`decode_trade_event`]).
pub(crate) const CANCEL_REFUSED: &str = "order_cancel_rejected: Alpaca refused the cancel (the order is \
     already filled or closed, or a replace of it is pending)";

/// Trim trailing fractional zeros (and a bare trailing `.`) from a `format_to_step` output.
/// `format_to_step` always pads to the step's full scale (e.g. `"10.000000000"` for a
/// 9-decimal step), but Alpaca (and the test fixture) expect whole-share quantities as bare
/// integers (`"10"`) while still preserving fractional-share/crypto precision (`"0.01"`).
fn trim_trailing_zeros(s: String) -> String {
    if !s.contains('.') {
        return s;
    }
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// This venue's row of the ONE cross-venue TIF authority ([`vike_venues::venue_time_in_force::venue_time_in_force`]),
/// consumed — lowercase wire; Alpaca has no GTD, so the row coerces `Gtd -> "gtc"`. A future TIF
/// flip is a one-line row edit there.
fn time_in_force_str(tif: TimeInForce) -> &'static str {
    vike_venues::venue_time_in_force::venue_time_in_force(VENUE, tif).wire().unwrap_or("gtc")
}

/// Build the `/v1/trading/accounts/{id}/orders` POST body from a vike `OrderRequest` whose USER name the venue spells
/// `wire_symbol` (`BTCUSD` is `BTC/USD`, `AAPL` is `AAPL`: `crate::names::wire_of`, read off the kept row).
///
/// ⚠ **`"0.000000001"` is a PRECISION CAP, not a venue grid, and it is deliberately not the
/// instrument's own step.** Alpaca accepts a fractional quantity to nine decimal places, and this
/// bridge's `SymbolProperties` grid (`crates/bridges/alpaca/src/instrument.rs`'s
/// `parse_asset_properties`) DEFAULTS `step_size` to `1.0` when `/v1/assets` omits
/// `min_trade_increment` — so quantizing the wire quantity on that grid would truncate every
/// fractional-share order, and a sub-share one to `"0"`. The lot snap belongs to
/// `crates/vike-execution/src/risk.rs`'s `RiskGate`, which has already applied it by the time a request
/// reaches here; this call's whole job is to stop an IEEE artifact reaching the wire.
///
/// ⚠ A nine-decimal step is the FINEST in the tree, which made this the first site to meet
/// `crates/vike-bridge-core/src/format.rs`'s `MAX_UNAMBIGUOUS_MULTIPLE` — above a multiple count of
/// `2^52` the grid is finer than the f64 spacing, and the quantizer's grid witness used to return a
/// neighbouring multiple. It rendered a 9,000,000-share order as `"8999999.999999999"`, which
/// Alpaca rejects. `crates/bridges/alpaca/tests/whole_share_quantity.rs` pins the wire string
/// across that window.
pub fn build_order_body(req: &OrderRequest, wire_symbol: &str) -> serde_json::Value {
    let side = if req.side >= 0 { "buy" } else { "sell" };
    let mut body = serde_json::json!({
        "symbol": wire_symbol,
        "qty": trim_trailing_zeros(format_to_step(req.qty.abs(), "0.000000001")),
        "side": side,
        "time_in_force": time_in_force_str(req.time_in_force),
        "client_order_id": req.order_client_id,
    });
    match &req.order_type {
        OrderType::Limit => {
            body["type"] = serde_json::json!("limit");
            if let Some(p) = req.price {
                body["limit_price"] = serde_json::json!(format_to_step(p, "0.01"));
            }
        }
        OrderType::Stop => {
            body["type"] = serde_json::json!("stop");
            if let Some(p) = req.trigger_price {
                body["stop_price"] = serde_json::json!(format_to_step(p, "0.01"));
            }
        }
        OrderType::StopLimit => {
            body["type"] = serde_json::json!("stop_limit");
            if let Some(p) = req.price {
                body["limit_price"] = serde_json::json!(format_to_step(p, "0.01"));
            }
            if let Some(p) = req.trigger_price {
                body["stop_price"] = serde_json::json!(format_to_step(p, "0.01"));
            }
        }
        // take_profit, market_to_limit, the trailing kinds, pegged and an unrecognised kind are refused by
        // the capability preflight before they get here; this arm is what the old `_ =>` caught
        // (market_to_limit, trailing, trailing_limit, pegged and trailing_take_profit were
        // unrecognised words until each had a variant)
        OrderType::Market
        | OrderType::TakeProfit
        | OrderType::MarketToLimit
        | OrderType::Trailing
        | OrderType::TrailingLimit
        | OrderType::Pegged
        | OrderType::TrailingTakeProfit
        | OrderType::Unrecognized(_) => {
            body["type"] = serde_json::json!("market");
        }
    }
    body
}

/// Map a 2xx order-POST response to the events it implies. Alpaca answers a success with the created
/// order object (`id`, `status`); its refusals are a 4xx (`code`/`message`), which the transport returns
/// as an error and never as a `response` here. So a 2xx with no `id` (an empty body reads `null`) is the
/// venue taking the order without saying how: NOTHING is emitted, and the order stays `Submitted` for
/// the trade-event stream, as an unknown-outcome POST does (`VenueRestAlpaca::submit_order`).
pub fn map_order_response(
    order_client_id: &str,
    ts: i64,
    response: &serde_json::Value,
) -> Vec<Event> {
    if response.get("id").is_none() {
        tracing::warn!(
            venue = "alpaca",
            %order_client_id,
            %response,
            "order submit answered 2xx with no order id: outcome unknown, not rejecting; the order stays \
             Submitted for the event stream"
        );
        return Vec::new();
    }
    vec![Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.to_string(),
        venue_order_id: response.get("id").and_then(|i| i.as_str()).map(Into::into),
        timestamp: ts,
    })]
}

/// The order POST's whole answer as the events it implies: a 2xx [`map_order_response`]'s; a failure that may have
/// reached Alpaca ([`VenueApiHttpError::outcome_unknown`]) NOTHING, so the order stays `Submitted` for the trade-event
/// stream, the stuck-order watchdog and reconcile (decision 0303: a terminal reject there could stand over a live
/// order); any other failure the terminal `OrderRejected` in its words, its catalog id by what it was
/// ([`VenueApiHttpError::to_venue_error`]: never sent, or Alpaca's own refusal, the 4xx).
pub(crate) fn map_submit_answer(
    order_client_id: &str,
    ts: i64,
    answer: &Result<serde_json::Value, VenueApiHttpError>,
) -> Vec<Event> {
    match answer {
        Ok(response) => map_order_response(order_client_id, ts, response),
        Err(e) if e.outcome_unknown() => {
            tracing::warn!(
                venue = VENUE,
                %order_client_id,
                error = %e,
                "order submit outcome unknown: not rejecting; the order stays Submitted for the event stream"
            );
            Vec::new()
        }
        Err(e) => {
            // `e` is a transport/HTTP-status error (never carries the Bearer, which ureq/AlpacaTransport never
            // places into an error's Display) — safe to log.
            tracing::warn!(venue = VENUE, %order_client_id, error = %e, "order submit failed");
            vec![Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: order_client_id.to_string(),
                reason: e.to_string().into(),
                timestamp: ts,
                refusal_id: vike_model::events::refusal_id_of(e.to_venue_error().refusal_id_word()),
            })]
        }
    }
}

/// The order POST's reply, a `status` and its `body`, read as the transport reads a write's
/// (`crate::transport::AlpacaTransport`'s finisher), as [`map_submit_answer`]'s events: the conformance harness's door
/// to Alpaca's refusal BEFORE it takes an order, since the POST itself needs a live host.
pub fn map_order_post_reply(order_client_id: &str, ts: i64, status: u16, body: &str) -> Vec<Event> {
    let answer = crate::transport::AlpacaTransport::finish(status, body, true, &[]);
    map_submit_answer(order_client_id, ts, &answer)
}

fn parse_timestamp_ms(v: &serde_json::Value, key: &str) -> i64 {
    // Alpaca SSE carries a real RFC3339 timestamp (e.g. "2026-07-14T05:39:31.4Z") — try that first
    // via `vike_utility::calendar::parse_rfc3339_ms`: its zone applied, a zone-less stamp refused (RFC 3339
    // requires one, and every stamp Alpaca sends carries `Z`). Fall back to a bare numeric-epoch-ms string
    // (defensive), else 0 (the core stamps its own receive time).
    let Some(s) = v.get(key).and_then(|t| t.as_str()) else { return 0 };
    parse_rfc3339_ms(s, MissingZone::Refuse).or_else(|| s.parse::<i64>().ok()).unwrap_or(0)
}

/// Decode one `/v2/events/trades` SSE JSON object into vike events.
pub fn decode_trade_event(v: &serde_json::Value, symbols: &OrderClientIdSymbols) -> Vec<Event> {
    let event = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
    let order = v.get("order").cloned().unwrap_or(serde_json::Value::Null);
    let order_client_id =
        order.get("client_order_id").and_then(|c| c.as_str()).unwrap_or_default().to_string();
    let ts = parse_timestamp_ms(v, "timestamp");
    match event {
        "fill" | "partial_fill" => {
            let s = |k: &str| {
                v.get(k).and_then(|x| x.as_str()).or_else(|| order.get(k).and_then(|x| x.as_str()))
            };
            let side =
                if order.get("side").and_then(|x| x.as_str()) == Some("sell") { -1 } else { 1 };
            let positive = |x: f64| x.is_finite() && x > 0.0;
            let last_qty = s("qty").and_then(|x| x.parse::<f64>().ok()).filter(|x| positive(*x));
            let last_price_value = s("price")
                .and_then(|x| x.parse::<f64>().ok())
                .or_else(|| {
                    order
                        .get("filled_avg_price")
                        .and_then(|x| x.as_str())
                        .and_then(|x| x.parse().ok())
                })
                .filter(|x| positive(*x));
            // `execution_id` is Alpaca's per-fill identity on the trade-update stream and the key
            // the engine dedups reconnect replays on. Absent ⇒ refuse the frame: there is no other
            // per-execution field here (`order.id` is shared by every fill of a partially-filled
            // order), so nothing replay-stable to synthesize from.
            let trade_id = match TradeId::new(
                v.get("execution_id").and_then(|x| x.as_str()).unwrap_or(""),
            ) {
                Ok(t) => t,
                Err(_) => {
                    tracing::warn!(
                        venue = VENUE,
                        %order_client_id,
                        "trade update carries no `execution_id` — dropping the fill rather than \
                         folding one that cannot be deduplicated"
                    );
                    return Vec::new();
                }
            };
            // The engine drops only a non-finite fill, so a defaulted 0 would open a position at price 0.
            let (Some(last_qty), Some(last_price_value)) = (last_qty, last_price_value) else {
                tracing::error!(
                    venue = VENUE,
                    %order_client_id,
                    %trade_id,
                    "trade update states no positive `qty` or `price` — refusing the fill rather than \
                     booking it at 0"
                );
                return Vec::new();
            };
            // The symbol the ORDER was submitted under, else the USER name of the venue's spelling (`BTC/USD` is
            // `BTCUSD`, its kept row's), else that spelling: the engine drops a fill whose symbol it neither mounts
            // nor registered the order under.
            let wire = order.get("symbol").and_then(|x| x.as_str()).unwrap_or_default();
            let symbol = symbols
                .symbol_of(&order_client_id)
                .or_else(|| crate::names::user_of(wire))
                .unwrap_or_else(|| wire.to_string());
            // A `partial_fill` leaves the entry: the pieces that follow still need it.
            if event == "fill" {
                symbols.forget(&order_client_id);
            }
            let fill = FillEvent {
                trade_id,
                order_client_id: order_client_id.clone(),
                venue: VENUE.to_string().into(),
                symbol: symbol.into(),
                side,
                last_qty,
                last_price: last_price_value,
                commission: 0.0,
                commission_asset: String::new().into(),
                // Alpaca's fill event carries no maker/taker liquidity flag (confirmed live
                // 2026-07-14 against a real equity fill: the payload has execution_id/price/qty/
                // timestamp but no liquidity field) — left empty, like the data-tick side flag.
                liquidity_side: String::new().into(),
                timestamp: ts,
                mark_price: None,
                position_side: vike_model::events::PositionSide::Both,
            };
            // Dual-publish (crypto contract): bare Fill (Account folds pnl) then OrderFilled (FSM).
            vike_bridge_core::terminal_events(order_client_id, fill, ts, true)
        }
        "canceled" => {
            symbols.forget(&order_client_id);
            vec![Event::OrderCanceled(OrderCanceled {
                order_client_id,
                reason: event.to_string().into(),
                timestamp: ts,
                post_only_would_cross: false,
                refusal_id: None,
            })]
        }
        // An expiry is its own end state, as this venue's reconcile (`crate::reconcile`'s `ORDER_STATUS`) and
        // the other venues' streams say (owner ruling L2).
        "expired" => {
            symbols.forget(&order_client_id);
            vec![Event::OrderExpired(OrderExpired { order_client_id, timestamp: ts })]
        }
        // Alpaca refused an order it had TAKEN (its POST answered 200: the venue created it), "rejected after creation,
        // no further updates". The order table takes no reject after an accept, so the end is a cancel in the venue's
        // words (vike-ibkr's `Inactive` after an accept ends the same way), behind the accept the event implies: legal
        // from `Submitted` (a POST whose answer was lost) and from `Accepted`, where the repeated accept is dropped.
        "rejected" => {
            symbols.forget(&order_client_id);
            vec![
                Event::OrderAccepted(OrderAccepted {
                    order_client_id: order_client_id.clone(),
                    venue_order_id: order.get("id").and_then(|i| i.as_str()).map(Into::into),
                    timestamp: ts,
                }),
                Event::OrderCanceled(OrderCanceled {
                    order_client_id,
                    reason: REFUSED_AFTER_TAKING.into(),
                    timestamp: ts,
                    post_only_would_cross: false,
                    refusal_id: vike_model::events::refusal_id_of(
                        vike_model::OrderRefusalId::VenueRefused.word(),
                    ),
                }),
            ]
        }
        // Alpaca refused a cancel its DELETE had taken (`204`): the order was already filled or closed, or a replace of
        // it is pending. The order lives on, so the answer is the non-terminal advisory in the venue's words.
        "order_cancel_rejected" => vec![Event::OrderCancelRejected(OrderCancelRejected {
            order_client_id,
            reason: CANCEL_REFUSED.into(),
            timestamp: ts,
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::VenueRefused.word(),
            ),
        })],
        "new" | "accepted" => vec![Event::OrderAccepted(OrderAccepted {
            order_client_id,
            venue_order_id: order.get("id").and_then(|i| i.as_str()).map(Into::into),
            timestamp: ts,
        })],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Equivalence gate for the `venue_time_in_force` routing: the five recorded lowercase wire strings
    /// (Gtd coerced to "gtc" — Alpaca has no GTD), asserted BOTH through `time_in_force_str` and against
    /// this venue's row of the cross-venue table (byte-for-byte).
    #[test]
    fn time_in_force_str_matches_the_venue_time_in_force_row() {
        use vike_model::TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
        for (tif, want) in [(Gtc, "gtc"), (Ioc, "ioc"), (Fok, "fok"), (Gtd, "gtc"), (Day, "day")] {
            assert_eq!(time_in_force_str(tif), want, "{tif:?}");
            assert_eq!(
                vike_venues::venue_time_in_force::venue_time_in_force(VENUE, tif).wire(),
                Some(want),
                "table row {tif:?}"
            );
        }
    }

    /// **E8 at the venue: a stamp's offset is APPLIED, and a stamp with no zone is no instant.** The old reader
    /// ignored anything after the seconds, so a `-04:00` stamp landed 4 hours off. A zone-less text reads 0 (the
    /// core stamps its own receive time); the numeric epoch-ms fallback stays.
    #[test]
    fn an_sse_timestamp_applies_its_offset_and_a_zoneless_one_is_refused() {
        let at = |t: &str| parse_timestamp_ms(&serde_json::json!({ "timestamp": t }), "timestamp");
        assert!(at("2026-07-14T05:39:31.4Z") > 0);
        assert_eq!(at("2026-07-14T01:39:31.4-04:00"), at("2026-07-14T05:39:31.4Z"));
        assert_eq!(at("2026-07-14T05:39:31.4"), 0, "no zone, no instant");
        assert_eq!(at("1784007571400"), 1_784_007_571_400, "the numeric fallback");
    }

    fn trade_event(event: &str, status: &str) -> serde_json::Value {
        serde_json::json!({"event": event, "timestamp": "2026-07-14T05:39:31.4Z",
            "order": {"id": "v-1", "client_order_id": "c1", "symbol": "AAPL", "side": "buy", "status": status}})
    }

    fn record_after(events: &[Event]) -> vike_model::OrderRecord {
        let mut record = vike_model::OrderRecord::new(OrderRequest {
            order_client_id: "c1".into(),
            symbol: "AAPL".into(),
            side: 1,
            qty: 1.0,
            ..Default::default()
        });
        for event in events {
            record.apply(event).expect("a legal step");
        }
        record
    }

    /// **AL-1: the trade event `rejected` ends the order wherever the order table holds it, in the venue's words.**
    /// Alpaca sends it after it TOOK the order, and the order table takes no reject after an accept: the old
    /// `OrderRejected` was dropped there and the order read `Accepted` for good. Its implied accept walks an order whose
    /// POST answer was lost (`Submitted`) to the cancel, and is the dropped repeat on an `Accepted` one.
    #[test]
    fn a_rejected_trade_event_ends_the_order_from_submitted_and_from_accepted() {
        use vike_model::OrderStatus;
        use vike_model::events::OrderSubmitted;
        let submitted =
            Event::OrderSubmitted(OrderSubmitted { order_client_id: "c1".into(), timestamp: 1 });
        let accepted = map_order_response("c1", 1, &serde_json::json!({"id": "v-1"}));
        let answered = [vec![submitted.clone()], accepted].concat();
        for (before, steps) in [(vec![submitted], [true, true]), (answered, [false, true])] {
            let symbols = crate::execution::submitted_symbols::submitted_symbols();
            symbols.record("c1", "AAPL");
            let said = decode_trade_event(&trade_event("rejected", "rejected"), &symbols);
            match said.as_slice() {
                [Event::OrderAccepted(a), Event::OrderCanceled(c)] => {
                    assert_eq!(a.venue_order_id.as_deref(), Some("v-1"));
                    assert_eq!(
                        (c.order_client_id.as_str(), c.reason.as_str(), c.refusal_id.as_deref()),
                        ("c1", REFUSED_AFTER_TAKING, Some("order.venue_refused"))
                    );
                }
                other => panic!("expected the implied accept and the cancel, got {other:?}"),
            }
            let mut record = record_after(&before);
            let applied: Vec<bool> = said.iter().map(|e| record.apply(e).is_ok()).collect();
            assert_eq!(applied, steps, "from {before:?}");
            assert_eq!(record.status, OrderStatus::Canceled);
            assert_eq!(symbols.label("c1", "X"), "X", "the ended order's entry left");
        }
    }

    /// **AL-2: the trade event `order_cancel_rejected` is the venue's NON-terminal refusal of the cancel**, with its
    /// refusal id: the order lives on (its entry stays, its record keeps its state), where the old mapper said nothing.
    #[test]
    fn an_order_cancel_rejected_trade_event_is_the_venues_refusal_of_the_cancel() {
        let symbols = crate::execution::submitted_symbols::submitted_symbols();
        symbols.record("c1", "AAPL");
        let said = decode_trade_event(&trade_event("order_cancel_rejected", "filled"), &symbols);
        match said.as_slice() {
            [Event::OrderCancelRejected(r)] => {
                assert_eq!((r.order_client_id.as_str(), r.reason.as_str()), ("c1", CANCEL_REFUSED));
                assert_eq!(
                    r.refusal_id.as_deref(),
                    Some(vike_model::OrderRefusalId::VenueRefused.word())
                );
            }
            other => panic!("expected one OrderCancelRejected, got {other:?}"),
        }
        assert_eq!(symbols.label("c1", "X"), "AAPL", "the order still lives");
        let submitted = Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            order_client_id: "c1".into(),
            timestamp: 1,
        });
        let accepted = map_order_response("c1", 1, &serde_json::json!({"id": "v-1"}));
        let mut record = record_after(&[vec![submitted], accepted].concat());
        record.apply(&said[0]).expect("a cancel refusal is legal on a resting order");
        assert_eq!(record.status, vike_model::OrderStatus::Accepted);
    }
}
