//! What the risk-lane suites share: the engine and order builders and the verdict readers.

use vike_execution::testing::RecordingClient;
use vike_execution::{Engine, EventBusOutbox};
use vike_model::OrderRequest;
use vike_model::RiskLimits;

use vike_execution::testing::{EngineBuilder, denied_reason};

pub(super) fn engine_with(limits: RiskLimits) -> Engine<RecordingClient> {
    EngineBuilder { limits, ..Default::default() }.build()
}

pub(super) fn open_buy(qty: f64, price: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: "c1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        timestamp: 1,
        ..Default::default()
    }
}

/// The EXACT shape the panic button lowers a position into: `vike_core`'s
/// `CoreThread::market_exit_flatten_legs` emits one `OrderIntent::Flatten` per non-flat position,
/// and that intent's arm mints a `reduce_only` MARKET order for `|position|` on
/// `vike_model::closing_side`. So this is `is_covered_reduce` in every sense the gate has —
/// direction opposes the position AND `|position| >= |qty|` — with the caller's flag set too.
pub(super) fn flatten_leg(order_client_id: &str, pos: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: vike_model::closing_side(pos),
        qty: pos.abs(),
        order_type: vike_model::OrderType::Market,
        reduce_only: true,
        timestamp: 1,
        ..Default::default()
    }
}

/// Submit `req` at `now_ms` and report the denial reason, `None` when it was admitted
/// (`risk_lane_pricing.rs`'s `exposure_verdict`, over any request and clock stamp).
pub(super) fn verdict_at(
    e: &mut Engine<RecordingClient>,
    req: &OrderRequest,
    now_ms: i64,
) -> Option<String> {
    let mut outbox = EventBusOutbox::default();
    e.submit_order(req, now_ms, &mut outbox);
    denied_reason(&outbox)
}
