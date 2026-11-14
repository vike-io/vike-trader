//! The server edge's notional cap and rate token (`ControlLimits`): the builders the topics share.
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use super::control::{ControlLimits, ControlLimitsConfig, DEFAULT_CONTROL_RATE};
use super::*;
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{NODE_PROTO_VERSION, Request, Response, read_frame, write_frame};
use vike_tradehub_client::wire::{CommandWire, OrderRequestWire};

fn submit(qty: f64, price: Option<f64>) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: "c1".into(),
        venue: "hyperliquid".into(),
        symbol: "BTC".into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price,
        trigger_price: None,
        reduce_only: false,
        account: None,
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

fn limits(max_notional: Option<f64>, rate: f64) -> ControlLimits {
    // The exact per-connection construction `handle_connection` performs (audit F13) — a fresh
    // full bucket over a caller-owned config, no env involved.
    ControlLimits::new(ControlLimitsConfig { max_notional, rate_per_sec: rate })
}

// Every `vet`/`preview_vet` below that is NOT about the contract value passes `&[], &[]` —
// "this surface knows no engine roster and no order" — which sizes at the default value (`Linear`,
// 1.0), the arithmetic every test in this file used before the ceiling counted one. The value tests
// further down are the ones that publish a roster (and, for a `Modify`, the order it names).

fn bracket(
    side: i32,
    qty: f64,
    entry: Option<f64>,
    stop_loss: f64,
    take_profit: f64,
) -> CommandWire {
    CommandWire::Bracket(vike_tradehub_client::wire::OrderBracketWire {
        order_client_id: None,
        venue: "hyperliquid".into(),
        symbol: "BTC".into(),
        side,
        qty,
        entry_price: entry,
        stop_loss,
        take_profit,
        account: None,
    })
}

/// One published engine block: its routing key, the symbol it was mounted on and — when the
/// value is not the default — a grid row for that symbol, the SPARSE shape `vike_tradehub_core`'s
/// `contract_value_grid` builds (a linear 1.0 collapses to an empty grid, not to a no-op row).
fn value_block(
    route_key: &str,
    symbol: &str,
    value: vike_model::ContractValue,
) -> vike_execution::VenueBlock {
    let mut grid = indexmap::IndexMap::new();
    if !value.is_default() {
        grid.insert(symbol.to_string(), value);
    }
    vike_execution::VenueBlock {
        venue: route_key.split('#').next().unwrap_or(route_key).to_string(),
        route_key: route_key.to_string(),
        symbol: symbol.to_string(),
        contract_values: Arc::new(grid),
        ..Default::default()
    }
}

/// [`value_block`] for a LINEAR value with the given per-unit factor (the old `multiplier`).
fn block(route_key: &str, symbol: &str, multiplier: f64) -> vike_execution::VenueBlock {
    value_block(route_key, symbol, vike_model::ContractValue::linear(multiplier))
}

/// [`value_block`] for an INVERSE value (deribit's `BTC-PERPETUAL`: the quantity is dollars).
fn inverse_block(route_key: &str, symbol: &str, per_unit: f64) -> vike_execution::VenueBlock {
    value_block(route_key, symbol, vike_model::ContractValue::inverse(per_unit))
}

/// One published order, as `vike_core::snapshot::build` publishes it: the holding engine's venue, its
/// account (`None` is the default account) and the symbol it rests on, with the status that says
/// whether it is still open. Terms are beside the point — only the instrument is read.
fn published_order(
    order_client_id: &str,
    venue: &str,
    account: Option<&str>,
    symbol: &str,
    status: vike_model::OrderStatus,
) -> vike_execution::OrderView {
    vike_execution::OrderView {
        data_delay: None,
        order_client_id: order_client_id.to_string(),
        venue: venue.to_string(),
        account: account.map(|a| {
            vike_model::accounts::account_keys::AccountLabel::parse(a).expect("a valid label")
        }),
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    }
}

fn modify_to(order_client_id: &str, qty: f64, price: f64) -> CommandWire {
    CommandWire::Modify {
        order_client_id: order_client_id.into(),
        new_qty: Some(qty),
        new_price: Some(price),
        new_trigger_price: None,
    }
}

#[cfg(test)]
mod bracket;
#[cfg(test)]
mod ceiling;

#[cfg(test)]
mod closes;
#[cfg(test)]
mod contract_value;
#[cfg(test)]
mod foreign_pair;
#[cfg(test)]
mod modify;
#[cfg(test)]
mod tcp_preview;
