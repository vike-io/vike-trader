//! The server edge's notional cap and rate token (`ControlLimits`): the builders the topics share.
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use super::control::{ControlLimits, ControlLimitsConfig, DEFAULT_CONTROL_RATE};
use super::*;
use vike_tradehub_client::auth::{self, NodeKeys};
use vike_tradehub_client::proto::{
    NODE_PROTO_VERSION, Request, Response, Scope, read_frame, write_frame,
};
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};

fn submit(qty: f64, price: Option<f64>) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: "c1".into(),
        venue: "hyperliquid".into(),
        symbol: "BTC".into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price,
        trigger_price: None,
        reduce_only: false,
        account: None,
    })
}

fn limits(max_notional: Option<f64>, rate: f64) -> ControlLimits {
    // The exact per-connection construction `handle_connection` performs (audit F13) — a fresh
    // full bucket over a caller-owned config, no env involved.
    ControlLimits::new(ControlLimitsConfig { max_notional, rate_per_sec: rate })
}

// Every `vet`/`preview_vet` below that is NOT about the contract multiplier passes `&[], &[]` —
// "this surface knows no engine roster and no order" — which sizes at multiplier 1.0, the
// arithmetic every test in this file used before the ceiling counted one. The multiplier tests
// further down are the ones that publish a roster (and, for a `Modify`, the order it names).

fn bracket(side: i32, qty: f64, entry: Option<f64>, sl: f64, tp: f64) -> WireCommand {
    WireCommand::Bracket(vike_tradehub_client::wire::WireBracketSpec {
        venue: "hyperliquid".into(),
        symbol: "BTC".into(),
        side,
        qty,
        entry_price: entry,
        stop_loss: sl,
        take_profit: tp,
    })
}

/// One published engine block: its routing key, the symbol it was mounted on and — when the
/// multiplier is not 1.0 — a grid row for that symbol, the SPARSE shape `vike_mount`'s
/// `multiplier_grid` builds (a 1.0 multiplier collapses to an empty grid, not to a no-op row).
fn block(route_key: &str, symbol: &str, multiplier: f64) -> vike_core::VenueBlock {
    let mut grid = indexmap::IndexMap::new();
    if multiplier != 1.0 {
        grid.insert(symbol.to_string(), multiplier);
    }
    vike_core::VenueBlock {
        venue: route_key.split('#').next().unwrap_or(route_key).to_string(),
        route_key: route_key.to_string(),
        symbol: symbol.to_string(),
        multipliers: Arc::new(grid),
        ..Default::default()
    }
}

/// One published order, as `CoreSnapshot::build` publishes it: the holding engine's venue, its
/// account (`None` is the default account) and the symbol it rests on, with the status that says
/// whether it is still open. Terms are beside the point — only the instrument is read.
fn published_order(
    coid: &str,
    venue: &str,
    account: Option<&str>,
    symbol: &str,
    status: vike_exec::OrderStatus,
) -> vike_core::OrderView {
    vike_core::OrderView {
        client_order_id: coid.to_string(),
        venue: venue.to_string(),
        account: account.map(|a| {
            vike_model::accounts::account_keys::AccountLabel::parse(a).expect("a valid label")
        }),
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
    }
}

fn modify_to(coid: &str, qty: f64, price: f64) -> WireCommand {
    WireCommand::Modify { client_order_id: coid.into(), new_qty: Some(qty), new_price: Some(price) }
}

#[cfg(test)]
mod bracket;
#[cfg(test)]
mod ceiling;
#[cfg(test)]
mod modify;
#[cfg(test)]
mod multiplier;
#[cfg(test)]
mod tcp_preview;
