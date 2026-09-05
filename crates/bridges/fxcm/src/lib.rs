//! FXCM ForexConnect exec adapter.
//!
//! Unlike the crypto venues (HMAC REST + websocket), FXCM speaks the ForexConnect C++ SDK.
//! The [`sys`] module wraps that as a blocking, single-threaded `FxcmSession`; this module bridges
//! it to the single-writer core by owning the session on ONE dedicated OS thread that takes
//! `submit`/`cancel` commands in and pushes venue [`Event`](vike_model::events::Event)s into the
//! core ingest via [`EventSender`](vike_exec::EventSender) — the same inbound shape as the WS
//! user-data pumps, just with a blocking session instead of a socket.
//!
//! SCOPE: places a resting LIMIT entry + cancel through the `ExecutionClient` seam, emitting
//! OrderSubmitted / OrderAccepted / OrderRejected / OrderCanceled. Audit A3: async FILL/terminal
//! events flow via a persistent Trades/Orders listener in the shim (`fcshim.cpp`) that enqueues
//! JSON envelopes; the exec thread drains them with `fc_poll_event` between commands and maps each
//! via the pure [`event_mapper`] (dual-publish Fill + wrap), and after a ForexConnect reconnect the
//! re-surfaced trades replay through the same lane (the core dedups by trade_id). The shim's async
//! lane is SDK-gated (built only with `FCSDK_DIR`) and validated by the live demo smoke, not CI;
//! the stub build's `fc_poll_event` returns nothing, so default builds are unchanged. Market orders
//! now place through `O2G2::Orders::TrueMarketOpen` and are demo-verified end to end (fill included)
//! by `tests/fxcm_live_smoke.rs`; honoring a precise requested limit price remains a follow-up.
//!
//! Moved into crates/bridges/fxcm (crate-reorg Phase 3, PR D) — the crate-local `fxcm` Cargo feature
//! now only gates whether `build.rs` attempts the native ForexConnect compile (unchanged
//! semantics); this module tree always compiles, degrading to the `#[cfg(not(fcsdk))]` stub in
//! [`sys`] whenever the SDK isn't found, so a default (no-feature) build already gives the same
//! stub coverage.

mod catalog;
mod config;
// PUBLIC since the exec-conformance wiring: `event_mapper` is the crate's whole PURE layer — the
// stateless shim-envelope decode plus the request preflight and the placement mapper lifted out of
// [`exec`] — and `crates/vike-bridge-core/tests/bridge_conformance.rs` drives it as the fxcm
// `ConformanceBridge`. It was `mod` (private) for as long as this venue sat in that harness's
// DEFERRED list, and the recorded reason for the deferral was the crate-local `fxcm` feature —
// which gates nothing but `build.rs`. The real obstacle was this one keyword.
pub mod event_mapper;
mod exec;
pub mod recon_client;
mod sys;

pub use catalog::{FxcmCatalog, bundled_instruments};
pub use config::{
    FxcmConfig, fxcm_env_var_names, load_fxcm_config_for_account, load_fxcm_config_from,
};
pub use exec::FxcmExecutionClient;
pub use recon_client::{FxcmReconClient, recon_client};

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_model::venue_caps`]; re-exported here for discoverability next to the adapter. Declared
/// unconditionally (outside the `fxcm` feature) — the caps are pure data, not the native SDK.
pub const CAPS: vike_model::VenueCaps = vike_model::venue_caps::FXCM;

/// Is the vendored ForexConnect SDK actually COMPILED INTO this build?
///
/// ⚠ This exists so a test can tell apart two outcomes that are otherwise identical from the
/// outside, and telling them apart is the whole point: with credentials present and no session
/// coming back, a **stub** build is a legitimate skip (there is no SDK to log in with — CI never
/// vendors one) while a **linked** build means the LOGIN FAILED, which must redden. Before this,
/// `crates/bridges/fxcm/tests/fxcm_live_smoke.rs` logged a warning and returned in BOTH cases, so
/// a venue that could not be reached at all reported a green test — and it did, for as long as the
/// demo credentials were expired.
///
/// That is the shape `crates/vike-secrets/src/dotenv.rs`'s store resolution already has a rule
/// about — an ABSENT store is the ordinary unconfigured state and is silent, a store that is
/// present and unusable is an ERROR — applied to the venue session instead of the credential file.
///
/// `build.rs` sets the `fcsdk` cfg when it finds the SDK (`FCSDK_DIR`, else the platform default
/// under `vendor/fcsdk`), so this is that decision, published.
///
/// ⚠ A FUNCTION rather than a `const`: an `assert!` on a constant is folded by the compiler and
/// `clippy::assertions_on_constants` rejects it under `-D warnings`, which is exactly how the
/// smokes need to spell it.
#[must_use]
pub fn sdk_linked() -> bool {
    cfg!(fcsdk)
}

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_model::caps_for("fxcm");
        assert_eq!(super::CAPS, caps);
        // the exec loop EXPLICITLY no-ops `ExecCommand::Modify` ("no native amend on this venue");
        // submit/cancel only, no batch, no wire reduce-only, no live DataClient.
        assert!(!caps.supports_modify);
        assert!(!caps.supports_native_batch);
        assert!(!caps.supports_reduce_only);
        assert!(!caps.has_live_data());
        // Expanded axes: the exec loop reads `order_type` and routes `"market"` to the shim's
        // true-market placement (`fc_place_market`, `O2G2::Orders::TrueMarketOpen`) and everything
        // else to the fixed resting-LIMIT entry (`place_limit_entry`) — so those two kinds, and
        // only those two, are wired. `"limit"` stays an approximation (the shim computes the
        // resting rate from the live quote; a requested limit price is not honored). No trigger
        // kinds. TIF/margin never read; no batch.
        // LIVE-VERIFIED (market-fill path only): the fcshim.cpp:122 real-SDK build break
        // (getInstrument on IO2GTradeRow, which lives only on IO2GTradeTableRow — the base row is
        // resolved via its offer id now) is fixed, and a demo market-order round-trip (BUY → fill
        // @ 1.14268 with the offer-resolved instrument → net-flat SELL) is green on the FXCM demo —
        // see tests/fxcm_live_smoke.rs::fxcm_market_round_trip. The phantom-cancel fix's other two
        // Orders(Delete) branches — async reject ('R') and venue-initiated cancel ('C') — are STILL
        // unexercised live, and the reason is no longer "pending" but a NAMED condition that could
        // not be produced on a demo account. It is stated once, in `vike_model::venue_caps`'s FXCM
        // row, beside the LIVE-VERIFIED claims it qualifies — not restated here, because the last
        // three copies of this paragraph in this tree disagreed with each other.
        use vike_model::{MarginMode, TimeInForce, TriggerType};
        assert_eq!(caps.supported_order_kinds, &["market", "limit"]);
        assert_eq!(caps.trigger_types, &[] as &[TriggerType]);
        assert_eq!(caps.accepted_tifs, &[TimeInForce::Gtc]);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
    }
}
