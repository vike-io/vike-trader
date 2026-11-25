//! Deribit options adapter — JSON-RPC over WS. Order entry rides the
//! authed order WS (no REST/HMAC); fills stream on a second authed socket.
//!
//! EXEC half (behind the default-on `execution` feature — see the seam note at the module list):
//! [`execution::rest::VenueRestDeribit`] (JSON-RPC buy/sell/cancel over the authed order
//! transport) + [`transport::client::DeribitTransport`] (the persistent order WS) +
//! [`execution::user_data`] (private fill-stream pump) + [`execution::resync`]/[`reconcile`](mod@reconcile) (resync +
//! reconcile-on-arm). DATA half: [`feed::options::chain::DeribitOptionsProvider`] (keyless public options
//! chain over the book summary, for the underlyings the venue's own instrument rows list:
//! [`option_listing`]; served to the gatewayhub's options engine, with the two option feeds and the
//! contract index, by [`options::OptionsSpecDeribit`]) + [`feed::dvol`] (the keyless
//! public DVOL volatility-index feed → live-seam mark series + opt-in `HistoryStore` recording) +
//! [`history`] (keyless public candle/OHLCV history over `public/get_tradingview_chart_data` — the
//! historical twin of the other venues' `history.rs`, but a COLUMNAR body over an END-ANCHORED,
//! silently-truncating window, so it pages BACKWARD like okx's) + the LIVE market plane:
//! [`feed::decode`] (pure decoders — the `book.*` DeltaSync `change_id` chain,
//! trades/quote/chart frames) and [`feed`] (`DataClientSpecDeribit`, the venue half of the
//! shared `DataClientWebsocket` engine, on the shared market-pump driver; bars/quotes/trades/book, plus the conflating DOM `depth` lane on the
//! shared depth driver).

// The exec/feeds seam — the shape vike-bybit and vike-okx took under ruling 8 of the datahub
// market-data wire design, given to this crate as a docs/decisions/0094 follow-up: everything that
// authenticates, builds or places an order, pumps the private fill stream, maps that private wire or
// reconciles an account sits behind the default-on `execution` feature, so a `default-features = false`
// consumer links the FEED surface only — catalog/chain/data/dvol/market_data/market_feed/
// options_feed over the keyless public endpoints, plus the pure `rpc` and `ratelimit` helpers
// (they import nothing exec-plane, though only exec modules call them today). The `bridges-feeds`
// arm of scripts/ci_feature_suite.sh is the gate.
//
// ⚠ Like bybit's and okx's this removes no CRATE from the dependency tree — deribit signs nothing,
// and the one dependency only `exec` names (tungstenite) rides vike-bridge-core, which the
// feeds half needs for its sockets. What the seam removes is COMPILED CODE. Cargo.toml's [features]
// block carries the argument.
pub mod catalog;
// The venue-adapter contract impl (docs/decisions/0096): it spawns the exec client and builds the
// reconcile client, so it rides `exec` with them.
#[cfg(feature = "execution")]
pub mod adapter;
// Decision 0290: a user name and the venue's own instrument name, both ways, off the kept row.
pub mod names;
pub mod option_listing;
// The public option TRADE log (each trade with its IV, mark and index price), the option-trades lane's venue half.
pub mod option_trades;
// The options lane's spec: the gatewayhub's options surface (the provider, the index fetch, the feeds).
pub mod options;
// The loopback book-summary and listing stand-in the options suites count requests on, here and in the
// gatewayhub's; never in a normal build.
#[cfg(any(test, feature = "test-support"))]
pub mod options_fixture;
// What the gatewayhub asks of deribit to admit any symbol (the testnet book's keyless lists): the venue's list spec.
pub mod symbol_properties;
// The crate's own test helpers (the testnet drills' authed socket), for its `tests/` binaries through
// the self dev-dependency; never in a normal build.
#[cfg(all(feature = "execution", any(test, feature = "test-support")))]
pub mod test_support;

#[cfg(feature = "execution")]
pub mod execution;
pub mod feed;
pub mod history;
#[cfg(feature = "execution")]
pub mod reconcile;
// Every refusal this crate makes, its id and its one text (`vike_utility::refusal_enum!`).
pub mod refusals;
pub mod transport;

pub use catalog::CatalogProviderDeribit;
#[cfg(feature = "execution")]
pub use execution::{
    DeribitProperties, fetch_deribit_instrument, fetch_deribit_properties,
    spawn_deribit_execution_client,
};
pub use feed::dvol::{DvolRecorder, FeedDvol, spawn_deribit_feed_dvol};
#[cfg(feature = "execution")]
pub use reconcile::{ReconcileClientDeribit, recon_client};

// The live market-DATA seam: `feed::DataClientSpecDeribit` is deribit's half of the shared
// `vike_data::DataClient` engine (`vike_bridge_core::data_client::ws::DataClientWebsocket`); `options_feed`'s spawners and their row/handle types are
// reached at `options_feed::…`. Neither has a crate-root re-export.

/// This adapter's DECLARED static capability row. Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::DERIBIT;

#[cfg(test)]
mod capabilities_test {
    #[test]
    fn declared_capabilities_match_registry() {
        let capabilities = vike_venues::capabilities_for("deribit");
        assert_eq!(super::CAPABILITIES, capabilities);
        // `VenueRestDeribit` amends in place (`private/edit`, `supports_modify`) but overrides no
        // batch method → no native batch; it DOES build `reduce_only` on the JSON-RPC order.
        assert!(capabilities.supports_modify);
        assert!(!capabilities.supports_native_batch);
        assert!(capabilities.supports_reduce_only);
        // The live market plane: the engine over `feed::DataClientSpecDeribit` serves bars (chart.trades
        // + REST seed), quotes (quote.*), trades (trades.*), the lossless book (book.* — the
        // change_id DeltaSync chain) AND the conflating DOM `depth` lane (the top 200 levels of
        // that same fold, `DataClientSink::l2_snapshot`). All five verbs, so the datahub's market-data
        // plane can serve this venue's DOM; a `subscribe_depth` spawns a real feed thread.
        assert!(capabilities.has_live_data());
        use vike_venues::DataClientVerb;
        for (verb, want) in [
            (DataClientVerb::Bars, true),
            (DataClientVerb::Quotes, true),
            (DataClientVerb::Trades, true),
            (DataClientVerb::Book, true),
            (DataClientVerb::Depth, true),
        ] {
            assert_eq!(capabilities.live_data.supports(verb), want, "{verb:?}");
        }
        // `vike_deribit::history::KlineSourceDeribit` pages the keyless public `get_tradingview_chart_data`
        // into `append_bars`; the datahub's `Backfill` verb has the gatewayhub fetch it through
        // `crates/vike-gatewayhub/src/rest/kline_sources.rs`'s `KLINE_SOURCES`. Bars yes, ticks no.
        assert!(capabilities.backfill_bars && !capabilities.backfill_ticks);
        // Expanded axes: `execution::rest::order_params_on` builds the native trigger orders (`stop_market`,
        // `stop_limit`, `take_market`/`take_limit` with `trigger_price` and `trigger`), so the
        // trigger kinds are declared; `crates/bridges/deribit/tests/offline/native_stops.rs` runs a
        // real stop through the builder and the same row. TIF: GTC default + IOC/FOK/Day mapped,
        // GTD denied (no good-till-DATE on Deribit). `post_only` forced false on the wire; fixed
        // subaccount cross margin; no native batch. `market_limit` (a `market_to_limit`) and
        // `display_amount` (a `display_qty`) are declared since `deribit_market_limit_on_the_testnet`
        // and `deribit_iceberg_on_the_testnet` ran.
        // `trailing` is the venue's `trailing_stop` (`wire_type`), declared since its testnet drill ran.
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        // `market_to_limit` is the venue's `market_limit` (`execution::rest::wire_type`)
        assert_eq!(
            capabilities.supported_order_kinds,
            &[
                "market",
                "limit",
                "stop",
                "stop_limit",
                "take_profit",
                "market_to_limit",
                "trailing"
            ]
        );
        assert_eq!(capabilities.trigger_types, &[TriggerType::StopLoss, TriggerType::TakeProfit]);
        assert_eq!(
            capabilities.accepted_tifs,
            &[
                TimeInForce::Gtc,
                TimeInForce::Ioc,
                TimeInForce::Fok,
                TimeInForce::Day,
                TimeInForce::PostOnly
            ]
        );
        assert!(!capabilities.accepted_tifs.contains(&TimeInForce::Gtd));
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cross]);
        assert_eq!(capabilities.max_batch, 0);
        assert!(capabilities.supports_post_only, "post_only + reject_post_only");
        // `display_qty` is `display_amount` on a plain limit (`execution::rest::order_params_on`); every other
        // shape is refused by name per order (`execution::rest::display_refusal`)
        assert!(capabilities.supports_display_quantity);
    }
}
