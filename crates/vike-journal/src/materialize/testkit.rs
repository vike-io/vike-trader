//! The WAL event and ingest builders the materializer's two test files share.

use vike_exec::{Command, Ingest, OrderIntent};
use vike_model::OrderRequest;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderModified,
    OrderPartiallyFilled, OrderSubmitted, TradeId,
};

/// [`fill_ev`]'s execution as a bare `Event::Fill`, with the caller's trade id and 0.1 commission.
pub(super) fn fill(
    trade_id: &'static str,
    coid: &str,
    symbol: &str,
    qty: f64,
    px: f64,
    ts: i64,
) -> Ingest {
    Ingest::Event(Event::Fill(FillEvent {
        trade_id: trade_id.into(),
        commission: 0.1,
        ..fill_ev(coid, symbol, qty, px, ts)
    }))
}

pub(super) fn submitted_ev(coid: &str, ts: i64) -> Event {
    Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.to_string(), ts })
}

pub(super) fn accepted_ev(coid: &str, voi: Option<&str>, ts: i64) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: voi.map(Into::into),
        ts,
    })
}

pub(super) fn modified_ev(coid: &str, qty: Option<f64>, px: Option<f64>, ts: i64) -> Event {
    Event::OrderModified(OrderModified {
        client_order_id: coid.to_string(),
        venue_order_id: None,
        new_qty: qty,
        new_price: px,
        ts,
    })
}

pub(super) fn submit(coid: &str, venue: &str, symbol: &str, side: i32, qty: f64) -> Ingest {
    Ingest::Command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        client_order_id: coid.to_string(),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side,
        qty,
        order_type: "limit".to_string(),
        price: Some(100.0),
        ..Default::default()
    }))))
}

pub(super) fn fill_ev(coid: &str, symbol: &str, qty: f64, px: f64, ts: i64) -> FillEvent {
    FillEvent {
        // minted by this helper — same `tr-<ts>` bytes as the `format!` it replaced
        trade_id: TradeId::prefixed("tr-", ts),
        client_order_id: coid.to_string(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

pub(super) fn accepted(coid: &str, voi: &str, ts: i64) -> Ingest {
    Ingest::Event(accepted_ev(coid, Some(voi), ts))
}

/// The adapter's own `OrderSubmitted`, emitted BEFORE the venue round trip and journaled on the
/// ingest lane like every other venue event ("Submitted → REST → Accepted|Rejected" — the
/// emitter-split contract `bridge_conformance.rs` machine-checks for every covered venue).
///
/// The WAL-writing tests below carry it because the REAL WAL carries it, and folding through
/// the FSM means it is now load-bearing: `OrderAccepted` is legal only from SUBMITTED. A venue
/// that skipped it would have its accept dropped by the LIVE engine too — which is exactly the
/// agreement this fold buys.
pub(super) fn submitted(coid: &str, ts: i64) -> Ingest {
    Ingest::Event(submitted_ev(coid, ts))
}

pub(super) fn order_filled(coid: &str, f: FillEvent, ts: i64) -> Ingest {
    Ingest::Event(Event::OrderFilled(OrderFilled {
        client_order_id: coid.to_string(),
        fill: f,
        ts,
    }))
}

pub(super) fn order_partial(coid: &str, f: FillEvent, ts: i64) -> Ingest {
    Ingest::Event(Event::OrderPartiallyFilled(OrderPartiallyFilled {
        client_order_id: coid.to_string(),
        fill: f,
        ts,
    }))
}

pub(super) fn canceled(coid: &str, ts: i64) -> Ingest {
    Ingest::Event(Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.to_string(),
        reason: String::new().into(),
        ts,
    }))
}
