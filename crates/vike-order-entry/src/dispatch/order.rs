//! The single home for turning a UI order intent into a
//! [`vike_model::OrderRequest`]. The **local** preview/safety layer that runs on it BEFORE the
//! command reaches the core command lane is the shared client's [`vike_tradehub_client::order_checks`].
//!
//! This is the CI-testable extraction of the order-write construction that used to live inline,
//! hand-rolled, at each scattered submit site in `vike-app`'s `main.rs` (the DOM ladder actions
//! loop, the options confirm-ticket submit, the options-chain cancel). Those sites each rebuilt an
//! `OrderRequest { .. }` literal and minted an `order_client_id` with an ad-hoc `format!` — untested
//! and impossible to reach in CI (the GUI crate is excluded for wgpu build weight). This module has
//! NO Python twin: it is the vike-native "order-entry" seam, mirroring the CLI/MCP competitor
//! study's preview + local-check + env-caps safety idea.
//!
//! Layering: egui-free (pure logic), depends on `vike-model` only — so it builds and unit-tests on
//! CI like the rest of this crate. The shell (`vike-desktop`; `vike-app` when
//! this was written) reaches `build_order_request` + [`vike_tradehub_client::order_checks::validate`]
//! where it used to construct the literal, through the dispatcher ([`crate::dispatch`]).

use vike_model::{OrderRequest, OrderType};

/// The order kinds a UI ticket offers — the form's choices, in the order it lists them. Every
/// [`OrderEntryDispatchOrder`] constructor builds one of these; the rest of [`OrderType`] (stop_limit,
/// take_profit) is not offered by any ticket.
#[cfg(test)]
const TICKET_ORDER_TYPES: &[OrderType] = &[OrderType::Limit, OrderType::Market, OrderType::Stop];

/// A UI order intent, venue-agnostic. Carries exactly the fields the scattered submit sites set:
/// the ladder's Place shape (limit/stop with `trigger_price` + `reduce_only`) AND the options-ticket
/// shape (a plain deribit limit). Everything else on `OrderRequest` comes from its `Default`.
///
/// `Default`-derived so callers build with struct-update syntax; the [`OrderEntryDispatchOrder::limit`] /
/// [`OrderEntryDispatchOrder::stop`] / [`OrderEntryDispatchOrder::market`] constructors cover the common shapes.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct OrderEntryDispatchOrder {
    pub(crate) venue: String,
    pub(crate) instrument: String,
    /// vike order side: +1 Buy / -1 Sell.
    pub(crate) side: i32,
    pub(crate) quantity: f64,
    pub(crate) price: Option<f64>,
    pub(crate) order_type: OrderType,
    /// stop trigger price (set for [`OrderType::Stop`]; `None` otherwise).
    pub(crate) trigger_price: Option<f64>,
    pub(crate) reduce_only: bool,
}

impl OrderEntryDispatchOrder {
    /// A limit order (`price` = the limit price, no trigger).
    pub(crate) fn limit(
        venue: impl Into<String>,
        instrument: impl Into<String>,
        side: i32,
        quantity: f64,
        price: f64,
    ) -> Self {
        Self {
            venue: venue.into(),
            instrument: instrument.into(),
            side,
            quantity,
            price: Some(price),
            order_type: OrderType::Limit,
            trigger_price: None,
            reduce_only: false,
        }
    }

    /// A stop order (`trigger_price` = the stop trigger; no resting limit `price`) — the shape a
    /// Stop from the Trade window's ladder or ticket takes.
    pub(crate) fn stop(
        venue: impl Into<String>,
        instrument: impl Into<String>,
        side: i32,
        quantity: f64,
        trigger: f64,
    ) -> Self {
        Self {
            venue: venue.into(),
            instrument: instrument.into(),
            side,
            quantity,
            price: None,
            order_type: OrderType::Stop,
            trigger_price: Some(trigger),
            reduce_only: false,
        }
    }

    /// A market order (no price, no trigger).
    pub(crate) fn market(
        venue: impl Into<String>,
        instrument: impl Into<String>,
        side: i32,
        quantity: f64,
    ) -> Self {
        Self {
            venue: venue.into(),
            instrument: instrument.into(),
            side,
            quantity,
            price: None,
            order_type: OrderType::Market,
            trigger_price: None,
            reduce_only: false,
        }
    }
}

/// The ONE `OrderRequest` constructor. Fills the seven live-path fields the UI sets and leaves the
/// rest to `OrderRequest::default()`, so it is byte-identical to the `OrderRequest { .., ..Default::default() }`
/// literals it replaces.
pub(crate) fn build_order_request(
    ticket: &OrderEntryDispatchOrder,
    order_client_id: String,
) -> OrderRequest {
    OrderRequest {
        order_client_id,
        venue: ticket.venue.clone(),
        symbol: ticket.instrument.clone(),
        side: ticket.side,
        qty: ticket.quantity,
        order_type: ticket.order_type.clone(),
        price: ticket.price,
        trigger_price: ticket.trigger_price,
        reduce_only: ticket.reduce_only,
        ..Default::default()
    }
}

#[path = "order_tests.rs"]
#[cfg(test)]
mod order_tests;
