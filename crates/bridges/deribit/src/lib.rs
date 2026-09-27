//! Deribit options adapter — JSON-RPC over WS (R6 slices 6-7). Order entry rides the
//! authed order WS (no REST/HMAC); fills stream on a second authed socket.
//!
//! EXEC half: [`client::DeribitRest`] (JSON-RPC buy/sell/cancel over the authed order
//! transport) + [`transport::DeribitOrderTransport`] (the persistent order WS) +
//! [`user_data`] (private fill-stream pump) + [`history`]/[`reconcile`] (audit-A3 resync +
//! reconcile-on-arm). DATA half: [`chain::DeribitOptionsProvider`] (keyless public options
//! chain — BTC/ETH/SOL book-summary, ports `data/options/deribit.py`) + [`dvol`] (the keyless
//! public DVOL volatility-index feed → live-seam mark series + opt-in `HistStore` recording) +
//! [`data`] (keyless public candle/OHLCV history over `public/get_tradingview_chart_data` — the
//! historical twin of the other venues' `data.rs`, but a COLUMNAR body over an END-ANCHORED,
//! silently-truncating window, so it pages BACKWARD like okx's) + the LIVE market plane
//! (split-plane I9): [`market_data`] (pure decoders — the `book.*` DeltaSync `change_id` chain,
//! trades/quote/chart frames) and [`market_feed`] (the `vike_data::DataClient` `Feeds` on the
//! shared market-pump driver; bars/quotes/trades/book, depth refused via the caps row).
//!
//! Moved into crates/bridges/deribit (crate-reorg Phase 3, PR F — carries `chain.rs` + the
//! `vike-options` dependency wholesale, per the vike-options extraction plan's D6).

pub mod catalog;
pub mod chain;
pub mod client;
pub mod combo;
pub mod data;
pub mod dvol;
pub mod event_mapper;
pub mod exec;
pub mod history;
pub mod market_data;
pub mod market_feed;
pub mod options_feed;
pub mod ratelimit;
pub mod recon_client;
pub mod reconcile;
pub mod rpc;
pub mod transport;
pub mod user_data;
pub mod ws_auth;

pub use catalog::DeribitCatalog;
pub use dvol::{DvolFeed, DvolRecorder, DvolTick, spawn_deribit_dvol_feed};
pub use exec::{DeribitExecutionClient, fetch_deribit_properties};
pub use recon_client::{DeribitReconClient, recon_client};

// The live market-DATA seam impl: `market_feed::Feeds` implements `vike_data::DataClient` directly
// (the per-venue `*MarketData` thin-factory wrapper was retired — Phase 1 crate-reorg). `Feeds`
// stays reachable at `market_feed::Feeds`, mirroring okx/bybit, which carry this same note; the
// crate-root re-export this crate ALONE carried was dead at every spelling and is gone.
//
// `options_feed`'s two spawners and their row/handle types are reached the same way, at
// `options_feed::…` — the spelling every consumer already uses (the app-core options tool, this
// crate's two live smokes). Corroborating that the re-export was never the reaching path here:
// `options_feed::MAINNET_WS`, which every one of those same consumers also names, was not in it.

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_model::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPS: vike_model::VenueCaps = vike_model::venue_caps::DERIBIT;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_model::caps_for("deribit");
        assert_eq!(super::CAPS, caps);
        // `DeribitRest` implements `VenueRest` but overrides neither modify_order nor the batch
        // methods → no native modify/batch; it DOES build `reduce_only` on the JSON-RPC order.
        assert!(!caps.supports_modify);
        assert!(!caps.supports_native_batch);
        assert!(caps.supports_reduce_only);
        // The live market plane (split-plane I9): `market_feed::Feeds` serves bars (chart.trades
        // + REST seed), quotes (quote.*), trades (trades.*) and the lossless book (book.* —
        // the change_id DeltaSync chain). The conflating DOM `depth` lane is the one verb
        // honestly refused (`subscribe_depth` routes through this row via `require_live_verb`).
        assert!(caps.has_live_data());
        use vike_model::LiveVerb;
        for (verb, want) in [
            (LiveVerb::Bars, true),
            (LiveVerb::Quotes, true),
            (LiveVerb::Trades, true),
            (LiveVerb::Book, true),
            (LiveVerb::Depth, false),
        ] {
            assert_eq!(caps.live_data.supports(verb), want, "{verb:?}");
        }
        // `vike_backfill::deribit` pages the keyless public `get_tradingview_chart_data` into
        // `append_bars` (the `deribit_backfill` bin, #1030). This row said `false` — and
        // vike-model's own `backfill_matches_vike_backfill` test ASSERTED that falsehood for this
        // venue while never reading vike-backfill at all.
        assert!(caps.backfill_bars && !caps.backfill_ticks);
        // Expanded axes (w2-task-5): `build_order_params` distinguishes ONLY `"limit"` — every
        // other kind is wire-`"market"`, so a `"stop"`/`"take_profit"` request would fire
        // IMMEDIATELY as market instead of resting (the most dangerous silent coercion in the
        // matrix) → NO trigger kinds declared, so the preflight refuses them. TIF FLIPPED: GTC
        // default + IOC/FOK/Day mapped, GTD denied (no good-till-DATE on Deribit). `post_only`
        // forced false on the wire; fixed subaccount cross margin; no native batch.
        use vike_model::{MarginMode, TimeInForce, TriggerType};
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
