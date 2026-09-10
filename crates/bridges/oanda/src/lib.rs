//! OANDA v20 FX adapter — co-located data + exec (pure-Rust Bearer REST, no native SDK).
//!
//! DATA half: [`OandaRest`] (Bearer client) + [`fetch_candles`] (candles → `Bar`) + the LIVE
//! [`market_feed::Feeds`] `DataClient` (split-plane: quotes STREAM over the venue's chunked-HTTP
//! pricing stream, bars POLL the candles REST — the module doc carries the design note; the pure
//! frame decoders live in [`market_data`]).
//! EXEC half: [`OandaExecutionClient`] — `ExecutionClient` over the v20 orders API (MARKET fills inline on
//! the POST; delayed LIMIT/STOP fills + cancels via the transactions stream on a 2nd reader thread).
//!
//! Moved into crates/bridges/oanda (crate-reorg Phase 3, PR B).

mod catalog;
mod config;
mod data;
mod exec;
mod history;
pub mod market_data;
pub mod market_feed;
pub mod recon_client;
mod rest;
mod stream;

pub use catalog::{OandaCatalog, parse_instruments};
pub use config::{
    MountableTier, OandaConfig, UnreachableLiveTier, load_oanda_config_for_account,
    load_oanda_config_from, mountable_tier, mountable_tier_for_account, oanda_env_var_names,
    oanda_hosts,
};
pub use data::{fetch_candles, granularity, parse_candles, to_oanda_instrument};
pub use exec::OandaExecutionClient;
pub use recon_client::{OandaReconClient, recon_client};
pub use rest::{OandaApiError, OandaRest};
pub use stream::decode_transaction_events;

/// Test-only surface: pure body-construction / response-mapping helpers with no production
/// caller besides the network-calling `exec::run` loop. `exec` stays a private module — these
/// items are `pub fn` only so this re-export can reach them, then re-exported here
/// `#[doc(hidden)]` so `tests/*.rs` integration tests can exercise them directly instead of
/// re-deriving their logic or standing up a network double (last resort — mirrors the
/// `#[doc(hidden)] pub` convention in `vike_exec::lanes::Conflated`). Not part of the
/// venue-adapter public API.
#[doc(hidden)]
pub use exec::{build_order_body, map_order_response, note_last_transaction_id};
/// `map_transactions_since`/`max_transaction_id` are already `pub fn` inside the private
/// `history` module (needed by `stream::resync_gap`); re-exported here `#[doc(hidden)]` purely so
/// the A3 resync-backfill contract is reachable from integration tests.
#[doc(hidden)]
pub use history::{map_transactions_since, max_transaction_id};

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_model::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPS: vike_model::VenueCaps = vike_model::venue_caps::OANDA;

#[cfg(test)]
mod caps_test {
    use vike_model::TimeInForce;

    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_model::caps_for("oanda");
        assert_eq!(super::CAPS, caps);
        // `OandaExecutionClient` wires submit/cancel only — modify is the trait no-op. But
        // `oanda_tif` maps the FULL TIF set, so OANDA is the rich-TIF venue.
        assert!(!caps.supports_modify);
        assert!(caps.supported_tifs.contains(&TimeInForce::Day));
        assert!(caps.supported_tifs.contains(&TimeInForce::Gtd));
        assert_eq!(caps.supported_tifs.len(), 5);
        // Expanded axes (w2-task-5): `build_order_body` wires `"limit"` → LIMIT and `"stop"` →
        // native STOP (trigger_price as the order level); everything else — take_profit included
        // — is the `_ => MARKET` coercion → unwired. Full TIF set already asserted above (the rich
        // venue: accepted == supported). `margin_mode` never read (shared FX account); no batch.
        use vike_model::{MarginMode, TriggerType};
        assert_eq!(caps.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(caps.accepted_tifs, caps.supported_tifs);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
    }
}
