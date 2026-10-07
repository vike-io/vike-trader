//! Order-request and command builders.

use vike_core::StrategyParams;
use vike_exec::{Command, ParamsUpdate};
use vike_model::{OrderRequest, TimeInForce};

/// A sim/BTCUSDT LIMIT order for `qty` at `px` on `side`.
///
/// DEFAULTS: every field not named here is `OrderRequest::default()` (so `TimeInForce::Gtc`, no
/// expiry).
pub(crate) fn sim_limit(coid: &str, side: i32, qty: f64, px: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty,
        order_type: "limit".into(),
        price: Some(px),
        ..Default::default()
    }
}

/// A resting limit order with an explicit GTD deadline (the terms already on `OrderRequest`).
///
/// DEFAULTS: sim/BTCUSDT, a 1.0 buy at 100.0, `TimeInForce::Gtd` expiring at `expiry_ms`; every
/// other field is `OrderRequest::default()`.
pub(crate) fn gtd_request(coid: &str, expiry_ms: i64) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        time_in_force: TimeInForce::Gtd,
        gtd_expiry: Some(expiry_ms),
        ..Default::default()
    })
}

/// [`gtd_request`]'s DAY twin: the same resting order, expiring at the end of its UTC day.
///
/// DEFAULTS: sim/BTCUSDT, a 1.0 buy at 100.0, `TimeInForce::Day`, no explicit expiry; every other
/// field is `OrderRequest::default()`.
pub(crate) fn day_request(coid: &str) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        time_in_force: TimeInForce::Day,
        ..Default::default()
    })
}

/// A live re-tune of the strategy mounted on binance/BTCUSDT/1m — the mount
/// `crate::kit::engines::binance_mount_config` builds.
pub(crate) fn binance_update_params(params: StrategyParams) -> Command {
    Command::UpdateParams(Box::new(ParamsUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        params,
    }))
}
