//! Deribit options adapter — JSON-RPC over WS. Order entry rides the
//! authed order WS (no REST/HMAC); fills stream on a second authed socket.
//!
//! EXEC half (behind the default-on `exec` feature — see the seam note at the module list):
//! [`client::DeribitRest`] (JSON-RPC buy/sell/cancel over the authed order
//! transport) + [`transport::DeribitOrderTransport`] (the persistent order WS) +
//! [`user_data`] (private fill-stream pump) + [`history`]/[`reconcile`] (resync +
//! reconcile-on-arm). DATA half: [`chain::DeribitOptionsProvider`] (keyless public options
//! chain — BTC/ETH/SOL book-summary) + [`dvol`] (the keyless
//! public DVOL volatility-index feed → live-seam mark series + opt-in `HistStore` recording) +
//! [`data`] (keyless public candle/OHLCV history over `public/get_tradingview_chart_data` — the
//! historical twin of the other venues' `data.rs`, but a COLUMNAR body over an END-ANCHORED,
//! silently-truncating window, so it pages BACKWARD like okx's) + the LIVE market plane:
//! [`market_data`] (pure decoders — the `book.*` DeltaSync `change_id` chain,
//! trades/quote/chart frames) and [`market_feed`] (the `vike_data::DataClient` `Feeds` on the
//! shared market-pump driver; bars/quotes/trades/book, plus the conflating DOM `depth` lane on the
//! shared depth driver).

// The exec/feeds seam — the shape vike-bybit and vike-okx took under ruling 8 of the datahub
// market-data wire design, given to this crate as a docs/decisions/0094 follow-up: everything that
// authenticates, builds or places an order, pumps the private fill stream, maps that private wire or
// reconciles an account sits behind the default-on `exec` feature, so a `default-features = false`
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
pub mod chain;
#[cfg(feature = "exec")]
pub mod client;
// The combo-book ORDER-ENTRY wire model (`private/create_combo` and the `private/buy`/`sell` params
// it resolves to, quantized through the order-wire formatter): pure, but its one consumer is
// `client`, so it rides `exec` with it.
#[cfg(feature = "exec")]
pub mod combo;
pub mod data;
pub mod dvol;
#[cfg(feature = "exec")]
pub mod event_mapper;
#[cfg(feature = "exec")]
pub mod exec;
#[cfg(feature = "exec")]
pub mod history;
pub mod market_data;
pub mod market_feed;
// The venue-adapter contract impl (docs/decisions/0096): it spawns the exec client and builds the
// reconcile client, so it rides `exec` with them.
#[cfg(feature = "exec")]
pub mod adapter;
pub mod options_feed;
pub mod ratelimit;
#[cfg(feature = "exec")]
pub mod recon_client;
#[cfg(feature = "exec")]
pub mod reconcile;
pub mod rpc;
// The fence that keeps the fill pump's events for an order behind that order's own
// `OrderSubmitted`/`OrderAccepted`: the exec thread and the pump are its two users.
#[cfg(feature = "exec")]
pub mod submit_fence;
#[cfg(feature = "exec")]
pub mod transport;
#[cfg(feature = "exec")]
pub mod user_data;
#[cfg(feature = "exec")]
pub mod ws_auth;

pub use catalog::DeribitCatalog;
pub use dvol::{DvolFeed, DvolRecorder, spawn_deribit_dvol_feed};
#[cfg(feature = "exec")]
pub use exec::{
    DeribitExecutionClient, InstrumentFacts, fetch_deribit_instrument, fetch_deribit_properties,
};
#[cfg(feature = "exec")]
pub use recon_client::{DeribitReconClient, recon_client};

// The live market-DATA seam impl, `market_feed::Feeds`, implements `vike_data::DataClient` directly
// and is reached at `market_feed::Feeds`; `options_feed`'s spawners and their row/handle types are
// reached at `options_feed::…`. Neither has a crate-root re-export.

/// This adapter's DECLARED static capability row. Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPS: vike_venues::VenueCaps = vike_venues::venue_caps::DERIBIT;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_venues::caps_for("deribit");
        assert_eq!(super::CAPS, caps);
        // `DeribitRest` amends in place (`private/edit`, `supports_modify`) but overrides no
        // batch method → no native batch; it DOES build `reduce_only` on the JSON-RPC order.
        assert!(caps.supports_modify);
        assert!(!caps.supports_native_batch);
        assert!(caps.supports_reduce_only);
        // The live market plane: `market_feed::Feeds` serves bars (chart.trades
        // + REST seed), quotes (quote.*), trades (trades.*), the lossless book (book.* — the
        // change_id DeltaSync chain) AND the conflating DOM `depth` lane (the top 200 levels of
        // that same fold, `LiveDataSink::l2_snapshot`). All five verbs, so the datahub's market-data
        // plane can serve this venue's DOM; `Feeds::subscribe_depth` spawns a real feed thread.
        assert!(caps.has_live_data());
        use vike_venues::LiveVerb;
        for (verb, want) in [
            (LiveVerb::Bars, true),
            (LiveVerb::Quotes, true),
            (LiveVerb::Trades, true),
            (LiveVerb::Book, true),
            (LiveVerb::Depth, true),
        ] {
            assert_eq!(caps.live_data.supports(verb), want, "{verb:?}");
        }
        // `vike_deribit::data::DeribitKlines` pages the keyless public `get_tradingview_chart_data`
        // into `append_bars`; the datahub's `Backfill` verb has the gatewayhub fetch it through
        // `crates/vike-gatewayhub/src/rest/kline_sources.rs`'s `KLINE_SOURCES`. Bars yes, ticks no.
        assert!(caps.backfill_bars && !caps.backfill_ticks);
        // Expanded axes: `build_order_params` distinguishes ONLY `"limit"` — every
        // other kind is wire-`"market"`, so a `"stop"`/`"take_profit"` request would fire
        // IMMEDIATELY as market instead of resting (the most dangerous silent coercion in the
        // matrix) → NO trigger kinds declared, so the preflight refuses them. TIF: GTC
        // default + IOC/FOK/Day mapped, GTD denied (no good-till-DATE on Deribit). `post_only`
        // forced false on the wire; fixed subaccount cross margin; no native batch.
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        assert_eq!(caps.supported_order_kinds, &["market", "limit"]);
        assert_eq!(caps.trigger_types, &[] as &[TriggerType]);
        assert_eq!(
            caps.accepted_tifs,
            &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::Day]
        );
        assert!(!caps.accepted_tifs.contains(&TimeInForce::Gtd));
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
    }
}
