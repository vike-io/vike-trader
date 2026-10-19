//! OANDA v20 FX adapter — co-located data + exec (pure-Rust Bearer REST, no native SDK).
//!
//! DATA half: [`OandaRest`] (Bearer client) + [`fetch_candles`] (candles → `Bar`) + the LIVE
//! [`market_feed::Feeds`] `DataClient` (split-plane: quotes STREAM over the venue's chunked-HTTP
//! pricing stream, bars POLL the candles REST — the module doc carries the design note; the pure
//! frame decoders live in [`market_data`]).
//! EXEC half: [`OandaExecutionClient`] — `ExecutionClient` over the v20 orders API (MARKET fills inline on
//! the POST; delayed LIMIT/STOP fills + cancels via the transactions stream on a 2nd reader thread).
//! HISTORY half: [`OandaKlines`] — the store-free `KlineSource` (a paging client over the same candles
//! endpoint, its own interval vocabulary and a token PROVIDER instead of a token).
//! RATE half: [`OandaCatalog::home_conversions`] — what one unit of an instrument's quote currency is
//! worth in the account's home currency ([`HomeConversions`], every rate an [`AccountPerQuote`]), for the
//! engine's multi-currency money; the cold path only, wired into no hub.

#![warn(unreachable_pub)]

pub mod adapter;
mod catalog;
mod config;
mod data;
mod exec;
mod history;
mod home_conversion;
mod klines;
pub mod market_data;
pub mod market_feed;
pub mod recon_client;
mod rest;
mod stream;
mod symbols;

pub use catalog::{OandaCatalog, parse_instruments};
pub use config::{
    AdapterTier, OandaConfig, UnreachableLiveTier, adapter_tier, load_oanda_config_from,
    load_oanda_history_token, oanda_hosts,
};
pub use data::{fetch_candles, granularity, parse_candles};
pub use exec::OandaExecutionClient;
pub use home_conversion::{
    AccountPerQuote, HomeConversion, HomeConversions, QuoteRate, RateError, quote_currencies_of,
    quote_currency_of,
};
pub use klines::{HistoryTokenError, HistoryTokenProvider, OandaKlines};
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
pub use exec::{
    build_order_body, build_replace_body, map_order_response, map_replace_response,
    note_last_transaction_id,
};
/// `map_transactions_since`/`max_transaction_id` are already `pub fn` inside the private
/// `history` module (needed by `stream::resync_gap`); re-exported here `#[doc(hidden)]` purely so
/// the A3 resync-backfill contract is reachable from integration tests.
#[doc(hidden)]
pub use history::{map_transactions_since, max_transaction_id};
/// The table the mappers above read to label a fill with the symbol its order was submitted under
/// (`crate::symbols`); a test shares one between a fake command loop and the stream decode.
#[doc(hidden)]
pub use symbols::SubmittedSymbols;

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPS: vike_venues::VenueCaps = vike_venues::venue_caps::OANDA;

#[cfg(test)]
mod caps_test {
    use vike_model::TimeInForce;

    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_venues::caps_for("oanda");
        assert_eq!(super::CAPS, caps);
        // `OandaExecutionClient` wires submit/cancel and modify (order REPLACE, `exec::run`'s `Modify`
        // arm) — `supports_modify` and the `CancelReplace` amend row move together. And `oanda_tif`
        // maps the FULL TIF set, so OANDA is the rich-TIF venue.
        assert!(caps.supports_modify);
        assert_eq!(
            vike_venues::amend_semantics("oanda"),
            vike_venues::AmendSemantics::CancelReplace
        );
        assert!(caps.supported_tifs.contains(&TimeInForce::Day));
        assert!(caps.supported_tifs.contains(&TimeInForce::Gtd));
        assert_eq!(caps.supported_tifs.len(), 5);
        // Expanded axes: `build_order_body` wires `"limit"` → LIMIT and `"stop"` →
        // native STOP (trigger_price as the order level); everything else — take_profit included
        // — is the `_ => MARKET` coercion → unwired. Full TIF set already asserted above (the rich
        // venue: accepted == supported). `margin_mode` never read (shared FX account); no batch.
        use vike_model::MarginMode;
        use vike_venues::TriggerType;
        assert_eq!(caps.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(caps.accepted_tifs, caps.supported_tifs);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
    }
}
