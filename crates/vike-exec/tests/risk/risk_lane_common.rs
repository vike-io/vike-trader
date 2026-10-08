//! What the risk-lane suites share: the engine and order builders and the verdict readers.

use vike_exec::testing::RecordingClient;
use vike_exec::{ExecutionEngine, Outbox, RiskLimits};
use vike_model::OrderRequest;
use vike_model::events::Event;

use crate::support::EngineBuilder;

pub(super) fn engine_with(limits: RiskLimits) -> ExecutionEngine<RecordingClient> {
    EngineBuilder { limits, ..Default::default() }.build()
}

pub(super) fn open_buy(qty: f64, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "c1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(price),
        ts: 1,
        ..Default::default()
    }
}

pub(super) fn denied_reason(outbox: &Outbox) -> Option<String> {
    outbox.0.iter().find_map(|e| match e {
        Event::OrderDenied(d) => Some(d.reason.to_string()),
        _ => None,
    })
}

/// The EXACT shape the panic button lowers a position into: `vike_core`'s
/// `CoreThread::market_exit_flatten_legs` emits one `OrderIntent::Flatten` per non-flat position,
/// and that intent's arm mints a `reduce_only` MARKET order for `|position|` on
/// `vike_model::closing_side`. So this is `is_covered_reduce` in every sense the gate has —
/// direction opposes the position AND `|position| >= |qty|` — with the caller's flag set too.
pub(super) fn flatten_leg(coid: &str, pos: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: vike_model::closing_side(pos),
        qty: pos.abs(),
        order_type: "market".into(),
        reduce_only: true,
        ts: 1,
        ..Default::default()
    }
}

/// Submit `req` at `now_ms` and report the denial reason, `None` when it was admitted
/// (`risk_lane_pricing.rs`'s `exposure_verdict`, over any request and clock stamp).
pub(super) fn verdict_at(
    e: &mut ExecutionEngine<RecordingClient>,
    req: &OrderRequest,
    now_ms: i64,
) -> Option<String> {
    let mut outbox = Outbox::default();
    e.submit_order(req, now_ms, &mut outbox);
    denied_reason(&outbox)
}
