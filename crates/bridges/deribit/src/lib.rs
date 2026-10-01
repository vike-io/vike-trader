//! Deribit options adapter — JSON-RPC over WS (R6 slices 6-7). Order entry rides the
//! authed order WS (no REST/HMAC); fills stream on a second authed socket.
//!
//! EXEC half (behind the default-on `exec` feature — see the seam note at the module list):
//! [`client::DeribitRest`] (JSON-RPC buy/sell/cancel over the authed order
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
// and the one dependency only `exec` names (tungstenite) rides vike-bridge-core's `full`, which the
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
// The venue mount contract impl (docs/decisions/0096): it spawns the exec client and builds the
// reconcile client, so it rides `exec` with them.
#[cfg(feature = "exec")]
pub mod mount;
pub mod options_feed;
pub mod ratelimit;
#[cfg(feature = "exec")]
pub mod recon_client;
#[cfg(feature = "exec")]
pub mod reconcile;
pub mod rpc;
#[cfg(feature = "exec")]
pub mod transport;
#[cfg(feature = "exec")]
pub mod user_data;
#[cfg(feature = "exec")]
pub mod ws_auth;

pub use catalog::DeribitCatalog;
pub use dvol::{DvolFeed, DvolRecorder, DvolTick, spawn_deribit_dvol_feed};
#[cfg(feature = "exec")]
pub use exec::{DeribitExecutionClient, fetch_deribit_properties};
#[cfg(feature = "exec")]
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
        // `vike_deribit::data::DeribitKlines` pages the keyless public `get_tradingview_chart_data`
        // into `append_bars` (landed as the `deribit_backfill` bin, #1030, deleted since; the
        // datahub's `Backfill` verb dispatches it now, through
        // `vike_datahub::backfill::KLINE_SOURCES` — a row this bridge crate has carried since
        // docs/decisions/0094 moved the impl here).
        // This row said `false` — and vike-model's own `backfill_matches_vike_backfill` test
        // ASSERTED that falsehood for this venue while never reading vike-backfill at all.
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
