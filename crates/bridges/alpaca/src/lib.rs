//! Alpaca Broker API venue bridge — OAuth2 Bearer REST exec + market-data WS.
//!
//! Ports the Alpaca Broker API (US equities + crypto) onto vike's `ExecutionClient` (trading) and
//! `DataClient` (live data) seams. Unlike every other bridge, Alpaca authenticates with an OAuth2
//! client-credentials grant exchanged for a short-lived (900 s) Bearer token. A single pinned
//! `account_id` collapses the multi-account Broker API to one-account-per-client.
//!
//! Module map:
//! - `auth` — the OAuth2 client-credentials `TokenSource` that mints and caches the Bearer.
//! - `config` — the credential trio → `AlpacaConfig`, and the venue's tier word (`alpaca_tier`).
//! - `hosts` — the sandbox-vs-live host table for authx, broker, data REST and data WS.
//! - `rest` — `AlpacaRest`, the Bearer REST client that refreshes and retries once on a 401.
//! - `exec` — `AlpacaExecutionClient`, the `ExecutionClient` seam over the Broker orders API.
//! - `stream` — the SSE reader for `/v2/events/trades`, the exec event lane.
//! - `event_mapper` — the pure Alpaca ↔ vike mapping: order bodies, order responses, SSE events.
//! - `recon_client` — `AlpacaReconClient`, the `ReconClient` seam over the same REST client.
//! - `instruments` — `/v1/assets` entry → contract and grid; the startup grid pre-fetch.
//! - `catalog` — `AlpacaCatalog`, the venue's `CatalogProvider` for the cross-venue symbol search.
//! - `mount` — `AlpacaVenueMount`, the venue mount contract over the exec and recon clients.
//! - `data` — `AlpacaDataClient`, the market-data WS → `vike_data::DataClient` seam.
//! - `sandbox` — sandbox-only account provisioning (create and fake-fund a test account).

#![warn(unreachable_pub)]

mod auth;
mod catalog;
mod config;
pub mod data;
mod event_mapper;
mod exec;
mod hosts;
mod instruments;
pub mod mount;
pub mod recon_client;
mod rest;
mod sandbox;
mod stream;

pub use auth::TokenSource;
pub use catalog::{AlpacaCatalog, parse_assets};
pub use config::{
    AlpacaConfig, alpaca_tier, load_alpaca_config_for_account, load_alpaca_config_from,
};
pub use data::AlpacaDataClient;
#[doc(hidden)]
pub use event_mapper::{build_order_body, decode_trade_event, map_order_response};
pub use exec::AlpacaExecutionClient;
pub use hosts::{AlpacaEnv, hosts_for};
pub use instruments::fetch_alpaca_properties;
pub use recon_client::{AlpacaReconClient, recon_client};
pub use rest::{AlpacaApiError, AlpacaRest};
pub use sandbox::{create_test_account, fund_test_account};

/// This adapter's DECLARED static capability row. Values live once in [`vike_model::venues::venue_caps`];
/// re-exported here for discoverability next to the adapter.
pub const CAPS: vike_model::VenueCaps = vike_model::venues::venue_caps::ALPACA;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        // `caps` from the non-const registry fn so the property assert isn't const-folded
        // (clippy::assertions_on_constants); the eq pins the re-export to it.
        let caps = vike_model::caps_for("alpaca");
        assert_eq!(super::CAPS, caps);
        assert!(!caps.supports_modify);
        // Expanded axes (w2-task-5): `build_order_body` maps `"limit"`/`"stop"`/`"stop_limit"`
        // natively (stop_price ← trigger_price) — the ONE adapter wiring a `"stop_limit"` kind;
        // `"take_profit"` → the `_ => market` coercion → unwired. `accepted_tifs` adds Gtd (coerced
        // to gtc today — live behavior, encoded as-accepted, so the preflight must not refuse it).
        // `margin_mode` never read (shared Reg-T account); no native batch.
        use vike_model::{MarginMode, TimeInForce, TriggerType};
        assert_eq!(caps.supported_order_kinds, &["market", "limit", "stop", "stop_limit"]);
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss]);
        assert!(caps.accepted_tifs.contains(&TimeInForce::Gtd));
        assert!(!caps.supported_tifs.contains(&TimeInForce::Gtd));
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
    }
}
