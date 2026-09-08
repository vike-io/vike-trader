//! IG (IG Group) FX/CFD adapter — stateful session-auth REST.
//!
//! DATA half: config + login ([`IgSession`]) + prices (→ `Bar`).
//! EXEC half: [`IgExecutionClient`] — positions (`/positions/otc`) + working orders
//! (`/workingorders/otc`) resolved via `/confirms/{dealRef}`, plus the async fill/terminal lane
//! ([`stream`] — the Lightstreamer trade-update stream, wired into `exec.rs`) for delayed
//! working-order fills. A `reduce_only` MARKET order CLOSES (`/positions/otc` + `_method: DELETE`)
//! rather than opening an opposing deal — `exec`'s module doc is the authority on that routing.
//!
//! MARKET-DATA half: REST history ([`data`]) plus the LIVE feed — [`market_feed::Feeds`], a
//! [`vike_data::DataClient`] over IG's Lightstreamer TLCP protocol spoken raw on the shared
//! tungstenite WS stack. Its pure halves are [`lightstreamer`] (the TLCP codec) and
//! [`market_data`] (the MARKET/CHART field normalizers).
//!
//! Moved into crates/bridges/ig (crate-reorg Phase 3, PR C).

mod catalog;
mod config;
mod data;
mod event_mapper;
mod exec;
pub mod lightstreamer;
pub mod market_data;
pub mod market_feed;
pub mod recon_client;
mod rest;
mod stream;

pub use catalog::{IgCatalog, parse_markets};
pub use config::{
    IgConfig, ig_env_var_names, ig_rest_base, load_ig_config_for_account, load_ig_config_from,
};
pub use data::{fetch_prices, parse_ig_time_utc, parse_prices, resolution};
pub use exec::IgExecutionClient;
pub use market_feed::Feeds;
pub use recon_client::{IgReconClient, recon_client};
pub use rest::{IgApiError, IgSession};

/// Test-only surface: the pure `/confirms` accept/fill/reject mapper (`exec::map_confirm`) and the
/// Lightstreamer trade-update decode (`event_mapper::decode_trade_confirm`). Both modules stay
/// private; these items are `pub` only so this re-export can reach them, then re-exported here
/// `#[doc(hidden)]` so the cross-bridge conformance harness
/// (`vike-bridge-core/tests/bridge_conformance.rs`) can drive IG's REAL mappers through the shared
/// scenario table. Mirrors oanda's `#[doc(hidden)] pub use exec::map_order_response`. Not part of
/// the venue-adapter public API.
#[doc(hidden)]
pub use event_mapper::decode_trade_confirm;
#[doc(hidden)]
pub use exec::map_confirm;

/// Test-only surface: how a submit resolves when IG's `/confirms` never answered. `pub` here so
/// `crates/bridges/ig/tests/confirm_never_terminalizes.rs` can fold its output through the REAL
/// `vike_exec::ManagedOrder` FSM — the same oracle the cross-bridge conformance harness uses — and
/// assert the no-false-terminal contract where the decision actually lives. Not public API.
#[doc(hidden)]
pub use exec::unresolved_confirm;

/// Test-only surface: the two request BUILDERS, so `crates/bridges/ig/tests/ig_close_position_smoke.rs`
/// opens and closes a real demo position through the SAME bytes production sends. A smoke that
/// hand-rolled its own bodies would prove IG's endpoint works and nothing about this bridge. Not
/// public API.
#[doc(hidden)]
pub use exec::{build_close_request, build_request};

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_model::venue_caps`]; re-exported here for discoverability next to the adapter.
pub const CAPS: vike_model::VenueCaps = vike_model::venue_caps::IG;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_model::caps_for("ig");
        assert_eq!(super::CAPS, caps);
        // `IgExecutionClient` wires submit/cancel only (positions + working orders); modify is the
        // trait no-op, no batch.
        assert!(!caps.supports_modify);
        assert!(!caps.supports_native_batch);
        // ⚠ `supports_reduce_only` stays FALSE even though a `reduce_only` order now CLOSES here.
        // That field asks whether the adapter builds a reduce-only FLAG onto the WIRE order; IG has
        // no such field — `reduce_only` selects a different ENDPOINT instead (`exec`'s module doc
        // has the routing table). cTrader, which closes by position id, declares `false` for the
        // same reason. Flipping it would claim a wire field that does not exist.
        assert!(!caps.supports_reduce_only);
        // LIVE DATA (this PR): `market_feed::Feeds` serves bars + quotes over Lightstreamer and
        // NOTHING else — IG publishes a dealer L1 with no ladder behind it and no public tape.
        assert!(caps.has_live_data());
        assert_eq!(
            caps.live_data,
            vike_model::LiveDataCaps {
                bars: true,
                quotes: true,
                trades: false,
                book: false,
                depth: false
            }
        );
        // Expanded axes (w2-task-5): `build_request` wires `"limit"`/`"stop"` as working orders
        // (level ← price/trigger_price) and everything else as a MARKET position open →
        // take_profit unwired. TIF is never read (working orders hardcode GOOD_TILL_CANCELLED), so
        // accepted == &[Gtc]: a non-GTC limit is preflight-refused instead of silently resting GTC
        // (the loud-deny flip this axis exists for). `margin_mode` never read; no batch.
        use vike_model::{MarginMode, TimeInForce, TriggerType};
        assert_eq!(caps.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(caps.accepted_tifs, &[TimeInForce::Gtc]);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
    }
}
