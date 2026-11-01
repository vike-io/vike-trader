//! Order-request and command builders.

use vike_exec::{Command, ParamsUpdate, SymbolAdmission};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{OrderRequest, OrderType, StrategyParams, SymbolGrid, TimeInForce};

/// A BTCUSDT LIMIT order on `venue` for `qty` at `px` on `side`. The venue is an argument because a
/// file that pins its own (`dispatch_lanes.rs` trades binance) says so at every call site.
///
/// DEFAULTS: every field not named here is `OrderRequest::default()` (so `TimeInForce::Gtc`, no
/// expiry, `ts 0`, no account).
pub(crate) fn limit_on(venue: &str, coid: &str, side: i32, qty: f64, px: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        side,
        qty,
        order_type: OrderType::Limit,
        price: Some(px),
        ..Default::default()
    }
}

/// [`limit_on`] the sim venue.
///
/// DEFAULTS: those of [`limit_on`].
pub(crate) fn sim_limit(coid: &str, side: i32, qty: f64, px: f64) -> OrderRequest {
    limit_on("sim", coid, side, qty, px)
}

/// A binance/BTCUSDT 1.0 buy LIMIT at 100.0 addressed to `account` (`None`: the venue's default
/// account) — the order the hot engine add/stop suites route by account.
///
/// DEFAULTS: those of [`limit_on`], with `ts 1`.
pub(crate) fn binance_limit_for(coid: &str, account: Option<AccountLabel>) -> OrderRequest {
    OrderRequest { ts: 1, account, ..limit_on("binance", coid, 1, 1.0, 100.0) }
}

/// A reduce-only STOP that sells 1.0 binance/BTCUSDT.P when the price falls to `trigger`.
///
/// DEFAULTS: `ts 1`, no limit price; every other field is `OrderRequest::default()`.
pub(crate) fn binance_perp_reduce_only_stop(coid: &str, trigger: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: -1,
        qty: 1.0,
        order_type: OrderType::Stop,
        trigger_price: Some(trigger),
        reduce_only: true,
        ts: 1,
        ..Default::default()
    }
}

/// An `AdmitSymbol` of `symbol` on the sim venue with its own margin rate `im_requirement` (`None`:
/// the engine-wide rate), first marked at `initial_mark` — the per-symbol-margin suites' admission.
///
/// DEFAULTS: route key `"sim"`, tick and lot `0.5` (the rest of the grid `SymbolGrid::default()`), no
/// contract value, no quote currency, rate or rate stamp, `release: false`, no margin mode.
pub(crate) fn sim_admission(
    symbol: &str,
    initial_mark: f64,
    im_requirement: Option<f64>,
) -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: "sim".into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.5), lot_size: Some(0.5), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: Some(initial_mark),
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement,
        release: false,
        margin_mode: None,
    }))
}

/// An `AdmitSymbol` of the yen-quoted `symbol` on the sim venue, stating its quote rate `rate` (yen
/// to the account currency) as of `as_of_ms` — the quote-rate suites' admission.
///
/// DEFAULTS: route key `"sim"`, tick `0.25` and lot `1.0` (the rest of the grid
/// `SymbolGrid::default()`), first marked at `150.0`, quote currency `"JPY"`, no contract value, no
/// margin rate of its own, `release: false`, no margin mode.
pub(crate) fn jpy_admission(symbol: &str, rate: f64, as_of_ms: i64) -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: "sim".into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.25), lot_size: Some(1.0), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: Some(150.0),
        quote_ccy: Some("JPY".into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of_ms),
        im_requirement: None,
        release: false,
        margin_mode: None,
    }))
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
/// builds the payload is `vike_tradehub_core::ReconManager`, which this crate cannot name).
///
/// DEFAULTS: `ReconPolicy::default()`, `generate_missing_orders: false`, `reconcile_balance: false`
/// and `BalanceTol::default()` — the values `vike_tradehub_core::ReconConfig::default()` carries for the same
/// four fields.
#[derive(Default)]
pub(crate) struct PassConfig {
    pub(crate) policy: vike_exec::recon::ReconPolicy,
    pub(crate) generate_missing_orders: bool,
    pub(crate) reconcile_balance: bool,
    pub(crate) balance_tol: vike_exec::recon::BalanceTol,
}
