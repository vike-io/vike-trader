//! Order-request and command builders.

use vike_exec::{Command, ParamsUpdate};
use vike_model::{OrderRequest, StrategyParams, TimeInForce};

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
        order_type: vike_model::OrderType::Limit,
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
        order_type: vike_model::OrderType::Limit,
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
        order_type: vike_model::OrderType::Limit,
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
        mount_id: None,
        params,
    }))
}

/// The four configuration-derived fields of a `ReconcileReports` payload, for a test that drives the
/// FOLD by sending `Command::ReconcileReports` straight to the handle (the manager that normally
/// builds the payload is `vike_mount::ReconManager`, which this crate cannot name).
///
/// DEFAULTS: `ReconPolicy::default()`, `generate_missing_orders: false`, `reconcile_balance: false`
/// and `BalanceTol::default()` — the values `vike_mount::ReconConfig::default()` carries for the same
/// four fields.
#[derive(Default)]
pub(crate) struct PassConfig {
    pub(crate) policy: vike_exec::recon::ReconPolicy,
    pub(crate) generate_missing_orders: bool,
    pub(crate) reconcile_balance: bool,
    pub(crate) balance_tol: vike_exec::recon::BalanceTol,
}
