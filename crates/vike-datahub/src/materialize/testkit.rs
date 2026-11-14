//! The WAL event and ingest builders the materializer's two test files share.

use vike_execution::{Command, Ingest, OrderIntent};
use vike_model::OrderRequest;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderModified,
    OrderPartiallyFilled, OrderSubmitted, TradeId,
};

/// [`fill_event`]'s execution as a bare `Event::Fill`, with the caller's trade id and 0.1 commission.
pub(super) fn fill(
    trade_id: &'static str,
    order_client_id: &str,
    symbol: &str,
    qty: f64,
    px: f64,
    ts: i64,
) -> Ingest {
    Ingest::Event(Event::Fill(FillEvent {
        trade_id: trade_id.into(),
        commission: 0.1,
        ..fill_event(order_client_id, symbol, qty, px, ts)
    }))
}

pub(super) fn submitted_event(order_client_id: &str, ts: i64) -> Event {
    Event::OrderSubmitted(OrderSubmitted {
        order_client_id: order_client_id.to_string(),
        timestamp: ts,
    })
}

pub(super) fn accepted_event(order_client_id: &str, voi: Option<&str>, ts: i64) -> Event {
    Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.to_string(),
        venue_order_id: voi.map(Into::into),
        timestamp: ts,
    })
}

pub(super) fn modified_event(
    order_client_id: &str,
    qty: Option<f64>,
    px: Option<f64>,
    ts: i64,
) -> Event {
    Event::OrderModified(OrderModified {
        order_client_id: order_client_id.to_string(),
        venue_order_id: None,
        new_qty: qty,
        new_price: px,
        new_trigger_price: None,
        timestamp: ts,
    })
}

pub(super) fn submit(
    order_client_id: &str,
    venue: &str,
    symbol: &str,
    side: i32,
    qty: f64,
) -> Ingest {
    Ingest::Command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    }))))
}

pub(super) fn fill_event(
    order_client_id: &str,
    symbol: &str,
    qty: f64,
    px: f64,
    ts: i64,
) -> FillEvent {
    FillEvent {
        // minted by this helper — same `tr-<ts>` bytes as the `format!` it replaced
        trade_id: TradeId::prefixed("tr-", ts),
        order_client_id: order_client_id.to_string(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: ts,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    }
}

pub(super) fn accepted(order_client_id: &str, voi: &str, ts: i64) -> Ingest {
    Ingest::Event(accepted_event(order_client_id, Some(voi), ts))
}

/// The adapter's own `OrderSubmitted`, emitted BEFORE the venue round trip and journaled on the
/// ingest lane like every other venue event ("Submitted → REST → Accepted|Rejected" — the
/// emitter-split contract `bridge_conformance.rs` machine-checks for every covered venue).
///
/// The WAL-writing tests below carry it because the REAL WAL carries it, and folding through
/// the FSM means it is now load-bearing: `OrderAccepted` is legal only from SUBMITTED. A venue
/// that skipped it would have its accept dropped by the LIVE engine too — which is exactly the
/// agreement this fold buys.
pub(super) fn submitted(order_client_id: &str, ts: i64) -> Ingest {
    Ingest::Event(submitted_event(order_client_id, ts))
}

pub(super) fn order_filled(order_client_id: &str, f: FillEvent, ts: i64) -> Ingest {
    Ingest::Event(Event::OrderFilled(OrderFilled {
        order_client_id: order_client_id.to_string(),
        fill: f,
        timestamp: ts,
    }))
}

pub(super) fn order_partial(order_client_id: &str, f: FillEvent, ts: i64) -> Ingest {
    Ingest::Event(Event::OrderPartiallyFilled(OrderPartiallyFilled {
        order_client_id: order_client_id.to_string(),
        fill: f,
        timestamp: ts,
    }))
}

pub(super) fn canceled(order_client_id: &str, ts: i64) -> Ingest {
    Ingest::Event(Event::OrderCanceled(OrderCanceled {
        order_client_id: order_client_id.to_string(),
        reason: String::new().into(),
        timestamp: ts,
    }))
}

/// `ing` as the trade daemon journals it when the event came over a gateway daemon's trade link
/// (journal v18): the same event inside an `Ingest::GatewayEvent` with a ring cursor. Anything that
/// is not a plain venue event (a submit intent, a watchdog) is returned as it was.
pub(super) fn via_gateway(ing: Ingest, seq: u64) -> Ingest {
    match ing {
        Ingest::Event(event) => Ingest::GatewayEvent(Box::new(vike_execution::GatewayEvent {
            event,
            route_key: "binance".into(),
            cursor: vike_model::EventRingCursor {
                gateway_epoch: 1,
                sequence: seq,
                timestamp_ms: 0,
            },
        })),
        other => other,
    }
}
