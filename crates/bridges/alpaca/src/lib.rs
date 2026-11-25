//! Alpaca Broker API venue bridge — OAuth2 Bearer REST exec + market-data WS.
//!
//! Ports the Alpaca Broker API (US equities + crypto) onto vike's `ExecutionClient` (trading) and
//! `DataClient` (live data) seams. Unlike every other bridge, Alpaca authenticates with an OAuth2
//! client-credentials grant exchanged for a short-lived (900 s) Bearer token. A single pinned
//! `account_id` collapses the multi-account Broker API to one-account-per-client.
//!
//! Module map:
//! - `auth` — the OAuth2 client-credentials `TokenSource` that mints and caches the Bearer.
//! - `config` — the account's credential trio → `AlpacaConfig`, and the arm's one tier decision.
//! - `hosts` — the sandbox-vs-live host table for authx, broker, data REST and data WS.
//! - `rest` — `AlpacaTransport`, the Bearer REST client that refreshes and retries once on a 401.
//! - `exec` — `spawn`, the `ExecutionClient` seam over the Broker orders API: the shared exec actor with
//!   this venue's hooks.
//! - `stream` — the exec event lane: the SSE body of `/v2/events/trades` on the shared private-stream pump.
//! - `event_mapper` — the pure Alpaca ↔ vike mapping: order bodies, order responses, SSE events.
//! - `names` — the USER name ↔ the venue's spelling and the class, off the kept row (decision 0290).
//! - `recon_client` — `ReconcileClientAlpaca`, the `ReconcileClient` seam over the same REST client.
//! - `instruments` — `/v1/assets` entry → contract and grid; the startup grid pre-fetch.
//! - `catalog` — `CatalogProviderAlpaca`, the venue's `CatalogProvider` for the cross-venue symbol search.
//! - `adapter` — `VenueAdapterAlpaca`, the venue-adapter contract over the exec and recon clients.
//! - `data` — `DataClientSpecAlpaca`, the market-data WS as a venue of the shared data engine
//!   (`vike_bridge_core::data_client::ws::DataClientWebsocket`, the `vike_data::DataClient` seam).
//! - `sandbox` — sandbox-only account provisioning (create and fake-fund a test account).

#![warn(unreachable_pub)]

pub mod adapter;
mod catalog;
mod config;
// What the gatewayhub asks of alpaca to admit any symbol (one asset, the account's key): the venue's list spec.
pub mod symbol_properties;

mod account;
pub mod execution;
pub mod feed;
mod instrument;
mod names;
pub mod reconcile;
// Every refusal this crate makes, its id and its one text (`vike_utility::refusal_enum!`).
pub mod refusals;
mod transport;

pub use account::{create_test_account, fund_test_account};
pub use catalog::{CatalogProviderAlpaca, parse_assets};
pub use config::{AlpacaConfig, load_alpaca_config_from};
#[doc(hidden)]
pub use execution::event_mapper::{
    build_order_body, decode_trade_event, map_order_post_reply, map_order_response,
};
/// Builds the table the SSE decode reads to label a fill with the symbol its order was submitted under
/// (`crate::execution::submitted_symbols`); a test shares one between a fake command loop and the decode.
#[doc(hidden)]
pub use execution::submitted_symbols::submitted_symbols;
/// The exec event lane's reconnect cursor, the one the SSE open and decode share; `link_conformance` drives it through
/// the shared pump.
#[doc(hidden)]
pub use execution::user_data::TradeEventsCursor;
/// The exec event lane's reconnect ladder and its modes, the one its pump runs; `link_conformance` runs it too.
#[doc(hidden)]
pub use execution::user_data::trade_events_backoff;
pub use instrument::fetch_alpaca_properties;
pub use reconcile::{ReconcileClientAlpaca, recon_client};
pub use transport::AlpacaTransport;
pub use transport::sign::TokenSource;
pub use transport::urls::{AlpacaUrls, urls_for};

/// This adapter's DECLARED static capability row. Values live once in [`vike_venues::venue_caps`];
/// re-exported here for discoverability next to the adapter.
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::ALPACA;

#[cfg(test)]
mod capabilities_test {
    #[test]
    fn declared_capabilities_match_registry() {
        // `capabilities` from the non-const registry fn so the property assert isn't const-folded
        // (clippy::assertions_on_constants); the eq pins the re-export to it.
        let capabilities = vike_venues::capabilities_for("alpaca");
        assert_eq!(super::CAPABILITIES, capabilities);
        assert!(!capabilities.supports_modify);
        // Expanded axes (w2-task-5): `build_order_body` maps `"limit"`/`"stop"`/`"stop_limit"`
        // natively (stop_price ← trigger_price) — the ONE adapter wiring a `"stop_limit"` kind;
        // `"take_profit"` → the `_ => market` coercion → unwired. `accepted_tifs` adds Gtd (coerced
        // to gtc today — live behavior, encoded as-accepted, so the preflight must not refuse it).
        // `margin_mode` never read (shared Reg-T account); no native batch.
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        assert_eq!(capabilities.supported_order_kinds, &["market", "limit", "stop", "stop_limit"]);
        assert_eq!(capabilities.trigger_types, &[TriggerType::StopLoss]);
        assert!(capabilities.accepted_tifs.contains(&TimeInForce::Gtd));
        assert!(!capabilities.supported_tifs.contains(&TimeInForce::Gtd));
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cross]);
        assert_eq!(capabilities.max_batch, 0);
        assert!(!capabilities.supports_post_only);
    }
}
