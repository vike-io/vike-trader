//! OANDA v20 FX adapter — co-located data + exec (pure-Rust Bearer REST, no native SDK).
//!
//! DATA half: [`OandaTransport`] (Bearer client) + [`fetch_candles`] (candles → `Bar`) + the LIVE
//! [`feed::Feeds`] `DataClient` (split-plane: quotes STREAM over the venue's chunked-HTTP
//! pricing stream, bars POLL the candles REST — the module doc carries the design note; the pure
//! frame decoders live in [`feed::decode`]).
//! EXEC half: [`spawn_execution_client`] — the shared `ExecutionClientActor` over the v20 orders API, with this venue's
//! hooks (MARKET fills inline on the POST; delayed LIMIT/STOP fills + cancels via the transactions stream on
//! a 2nd reader thread).
//! HISTORY half: [`KlineSourceOanda`] — the store-free `KlineSource` (a paging client over the same candles
//! endpoint, its own interval vocabulary and a token PROVIDER instead of a token), and
//! [`credentialed_klines`] — the one practice-token read behind that provider and the spec the
//! gatewayhub's credentialed kline engine serves the lane through.
//! GRID: [`InstrumentGrid`] — the price and unit decimals one instrument is written on, from its own
//! `displayPrecision` / `tradeUnitsPrecision`; [`grid_refusal`] is the gatewayhub's admission fence over it.
//! RATE half: [`CatalogProviderOanda::home_conversions`] — what one unit of an instrument's quote currency is
//! worth in the account's home currency ([`HomeConversions`], every rate a
//! [`vike_model::AccountPerQuote`]), for the
//! engine's multi-currency money; the cold path only, read by the gatewayhub's symbol-facts lane.

#![warn(unreachable_pub)]

pub mod adapter;
mod catalog;
mod config;
// The history lane's practice-token read and the spec the gatewayhub's credentialed kline engine runs.
pub mod credentialed_klines;
// What the gatewayhub asks of OANDA to admit any instrument: the account-listed spec and the rate of a quote currency.
mod symbol_properties;
// The docs-shaped instrument rows the grid tests share, here and in the gatewayhub's facts tests.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub(crate) mod account;
mod execution;
pub mod feed;
pub(crate) mod history;
mod instrument;
// An instrument's user name and its wire name, read off its kept row (decision 0290).
mod names;
pub mod reconcile;
// Every refusal this crate makes, its id and its one text (`vike_utility::refusal_enum!`).
pub mod refusals;
mod transport;

pub use account::quote_rate::{
    HomeConversion, HomeConversions, quote_currencies_of, quote_currency_of,
};
pub use catalog::{CatalogProviderOanda, parse_instruments};
pub use config::{
    OandaConfig, demo_only_arm, load_oanda_config_from, load_oanda_history_token, oanda_hosts,
};
pub use execution::spawn_execution_client;
pub use execution::user_data::decode_transaction_events;
pub use history::candles::{fetch_candles, granularity, parse_candles};
pub use history::klines::{
    HistoryTokenError, HistoryTokenErrorId, HistoryTokenProvider, KlineSourceOanda,
};
pub use instrument::{InstrumentGrid, grid_refusal};
pub use reconcile::{ReconcileClientOanda, recon_client};
pub use symbol_properties::{SymbolPropertiesAccountSpecOanda, quoted_rate};
pub use transport::OandaTransport;

/// The stateless financing mapper (`crate::execution::funding`), so a test can fold a `DAILY_FINANCING` line through a
/// real engine; production books through the stream lane's `FinancingLane`.
#[doc(hidden)]
pub use execution::funding::financing_events;
/// `map_transactions_since`/`max_transaction_id` are already `pub fn` inside the private
/// `history` module (needed by `execution::user_data::resync_gap`); re-exported here `#[doc(hidden)]` purely so
/// the A3 resync-backfill contract is reachable from integration tests.
#[doc(hidden)]
pub use execution::resync::{map_transactions_since, max_transaction_id};
/// Builds the table the mappers above read to label a fill with the symbol its order was submitted under
/// (`crate::execution::submitted_symbols`); a test shares one between a fake command loop and the stream decode.
#[doc(hidden)]
pub use execution::submitted_symbols::submitted_symbols;
/// Test-only surface: pure body-construction / response-mapping helpers with no production
/// caller besides the network-calling `execution::run` loop. `exec` stays a private module — these
/// items are `pub fn` only so this re-export can reach them, then re-exported here
/// `#[doc(hidden)]` so `tests/*.rs` integration tests can exercise them directly instead of
/// re-deriving their logic or standing up a network double (last resort — mirrors the
/// `#[doc(hidden)] pub` convention in `vike_execution::lanes::MarketConflation`). Not part of the
/// venue-adapter public API.
#[doc(hidden)]
pub use execution::{
    build_order_body, build_replace_body, map_order_response, map_replace_response,
    note_last_transaction_id, with_symbol_hooks,
};

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::OANDA;

#[cfg(test)]
mod capabilities_test {
    use vike_model::TimeInForce;

    #[test]
    fn declared_capabilities_match_registry() {
        let capabilities = vike_venues::capabilities_for("oanda");
        assert_eq!(super::CAPABILITIES, capabilities);
        // The exec client wires submit/cancel and modify (order REPLACE, `execution::run`'s `Modify`
        // arm) — `supports_modify` and the `CancelReplace` amend row move together. And `oanda_time_in_force`
        // maps the FULL TIF set, so OANDA is the rich-TIF venue.
        assert!(capabilities.supports_modify);
        assert_eq!(
            vike_venues::amend_semantics("oanda"),
            vike_venues::AmendSemantics::CancelReplace
        );
        assert!(capabilities.supported_tifs.contains(&TimeInForce::Day));
        assert!(capabilities.supported_tifs.contains(&TimeInForce::Gtd));
        assert_eq!(capabilities.supported_tifs.len(), 5);
        // Expanded axes: `build_order_body` wires `"limit"` → LIMIT and `"stop"` →
        // native STOP (trigger_price as the order level); a priceless `"take_profit"` →
        // MARKET_IF_TOUCHED is built too but its row stays closed until its demo smoke runs
        // (`oanda_demo_place_and_cancel_far_from_market_mit`); everything else is refused by name
        // (`execution/mod.rs`'s `unbuilt_kind`) → unwired. Full TIF set already
        // asserted above (the rich venue: accepted == supported). `margin_mode` never read (shared FX account); no batch.
        use vike_model::MarginMode;
        use vike_venues::TriggerType;
        assert_eq!(capabilities.supported_order_kinds, &["market", "limit", "stop", "take_profit"]);
        assert_eq!(capabilities.trigger_types, &[TriggerType::StopLoss, TriggerType::TakeProfit]);
        assert_eq!(capabilities.accepted_tifs, capabilities.supported_tifs);
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cross]);
        assert_eq!(capabilities.max_batch, 0);
        assert!(!capabilities.supports_post_only);
    }
}
