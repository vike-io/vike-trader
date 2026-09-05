//! Binance adapter — spot REST (slice 1) + WS-API user data (slice 2) + perp (slice 3).
//!
//! DATA half: [`data`] (REST kline/candlestick history) and [`market_feed::Feeds`] (the live
//! `vike_data::DataClient` kline + trades feed — trades via [`trades::run_trades_feed`], Task
//! B2). EXEC half: [`spot::BinanceSpotRest`] (signed spot REST),
//! [`perp::BinancePerpRest`] (signed USDS-M futures REST, incl. native batch-submit+cancel and
//! native modify), [`user_data`]/[`perp_user_data`] (WS-API/listenKey private-stream sessions),
//! and [`history`] (audit-A3 resync replay). Also owns the R8 HFT public L2/tick track
//! ([`market_data`]).
//!
//! Moved into crates/bridges/binance (crate-reorg Phase 3, PR H). Binance was the cleave DONOR: its
//! own venue-neutral fragments (`WsSocket`/`TungsteniteStream`, `SymbolProperties`/`VenueRest`/
//! `LiveRestClient<R>`, `kline_to_bar`) were relocated into `vike-bridge-core` in PR A so every
//! sibling venue (bybit/okx/deribit/polymarket) kept compiling as it moved out on its own — the
//! remaining module here is self-contained venue code, same template as bybit/deribit/okx.

pub mod catalog;
pub mod data;
pub mod error_codes;
pub mod event_mapper;
pub mod exec;
// The shared Binance-wire-grammar core vike-aster reuses — NOT a venue. Everything Aster forked
// verbatim lives there once and both venues' same-named modules are thin re-exports/callers over
// it: the four pure mappers (`event_mapper`/`perp_mapper`/`history`/`filters_rec`, rung 1, venue as
// a param), the public market-data stack (`market_data`/`trades`/`market_feed`, rung 2, hosts as
// a `UrlTable` param — Binance's row is a const, so nothing here became env-resolved), and the
// public catalog parse (`catalog`, rung 3 — the exchangeInfo URLs stay consts here, only the parse
// is shared). `ratelimit` is deliberately NOT shared: see the family module doc.
pub mod family;
pub mod filters_rec;
pub mod history;
pub mod key_permissions;
pub mod market_data;
pub mod market_feed;
pub mod perp;
pub mod perp_mapper;
pub mod perp_user_data;
pub mod ratelimit;
pub mod recon_client;
pub mod spot;
pub(crate) mod trades;
pub mod user_data;
pub mod ws_auth;

// The live market-DATA seam impl: `market_feed::Feeds` implements `vike_data::DataClient`
// directly (the per-venue `*MarketData` thin-factory wrapper was retired — Phase 1 crate-reorg).
// `Feeds` stays reachable at `market_feed::Feeds` for the GUI's direct use.

// `trades` is `pub(crate)` (its module-internal types like `AggTrade` stay crate-private), but the
// production backfill entry point (SP3 Task 3) is re-exported here so `vike-app` can reach it as
// `vike_binance::agg_trades_backfill_reported(...)` without needing the whole module public — with
// the `BackfillOutcome`/`BackfillStop` it answers with. That stop reason is the point: a REST
// error used to be indistinguishable from end-of-history, so an error-truncated symbol looked
// complete, and only a RETURNED cause lets a caller reopen its run-once spawn guard. The
// `()`-returning `agg_trades_backfill` that predated it discarded exactly that value; it is
// retired, and nothing ever called it.
//
// SP3 Task 4 adds `latest_agg_trade` alongside — a small helper the real-network smoke test
// (`tests/binance_backfill_smoke.rs`) uses to derive a real, always-current `before_id`/
// `earliest_ts` pair; nothing in production calls it.
pub use trades::{BackfillOutcome, BackfillStop, agg_trades_backfill_reported, latest_agg_trade};

// vike-catalog `CatalogProvider` impl (crate-reorg catalog workstream): reachable as
// `vike_binance::BinanceCatalog` — the sole catalog path since Task 12 retired the pre-#269
// `SymbolInfo`/`fetch_spot_catalog` code.
pub use catalog::BinanceCatalog;
pub use exec::{BinanceExecutionClient, fetch_binance_properties};
pub use key_permissions::{
    BinanceKeyPermissionProbe, BinanceKeyProbe, key_permission_probe, parse_api_restrictions,
};
pub use recon_client::{BinanceReconClient, recon_client};

/// This adapter's DECLARED static capability row (audit br6). The values live once in
/// [`vike_model::venue_caps`]; re-exported here so `vike_binance::CAPS` is discoverable next to the
/// adapter and the test below ties it to the adapter's real behavior.
pub const CAPS: vike_model::VenueCaps = vike_model::venue_caps::BINANCE;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry_and_reality() {
        // `caps` from the non-const registry fn so the property asserts aren't const-folded
        // (clippy::assertions_on_constants); the eq pins the re-export to it.
        let caps = vike_model::caps_for("binance");
        assert_eq!(super::CAPS, caps);
        // reality: `BinancePerpRest` overrides `VenueRest::{modify_order, submit_batch,
        // cancel_batch}` with native fapi endpoints (perp.rs), so both flags are TRUE; it also
        // builds `reduceOnly` on every order.
        assert!(caps.supports_modify);
        assert!(caps.supports_native_batch);
        assert!(caps.supports_reduce_only);
        // Expanded axes (w2-task-5), tied to the family builders:
        // - perp `build_perp_order_params` wires STOP_MARKET from `"stop"` (`stopPrice` ←
        //   `trigger_price`); spot has no stop type. `"take_profit"` is unwired (the `_ => MARKET`
        //   fallthrough), so it is NOT a supported kind and NOT a declared trigger.
        use vike_model::{MarginMode, TimeInForce, TriggerType};
        assert_eq!(caps.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss]);
        // TIF: GTC/IOC/FOK honored 1:1; the perp lane additionally wires native fapi GTD, so it is
        // in the core-preflight ADMIT set (lane union) but NOT in the honored/offer set (the
        // single "binance" key can't distinguish lanes; spot denies GTD).
        assert_eq!(caps.supported_tifs, &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok]);
        assert!(caps.accepted_tifs.contains(&TimeInForce::Gtd));
        // margin: the order builder never reads `margin_mode` (account default cross). `max_batch`
        // = the fapi batchOrders chunk cap.
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 5);
        assert!(!caps.supports_post_only);
    }
}
