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
//! a box with no shim never opens a session at all. Market orders place through
//! `O2G2::Orders::TrueMarketOpen` and are demo-verified end to end (fill included) by
//! `tests/fxcm_live_smoke.rs`; honoring a precise requested limit price remains a follow-up.
//!
//! The module tree always compiles, and always compiles the SAME code: the crate-local `fxcm` Cargo
//! feature only gates whether `build.rs` BUILDS the shim shared object, `src/shim/fcshim.cpp` into
//! `libfcshim.so` (which links the SDK). [`loader`] opens that at runtime, so no Rust binary
//! references a ForexConnect symbol or carries `DT_NEEDED libForexConnect.so`, and
//! [`sdk_available`] is the runtime answer. The rules this sets are in
//! `crates/bridges/fxcm/CLAUDE.md`'s "The gate is a RUNTIME probe".

#![warn(unreachable_pub)]
#![warn(clippy::undocumented_unsafe_blocks)]
#![warn(clippy::allow_attributes)]

mod catalog;
mod config;
// PUBLIC: the gatewayhub's kline registry (`crates/vike-gatewayhub/src/rest/kline_sources.rs`'s `KLINE_SOURCES`) holds
// `data::FxcmKlines`, FXCM's KEYLESS history: anonymous GETs of the public candle archive, no ForexConnect, no shim, no
// session. It is not the market-data seam this crate lacks (`LiveDataCaps::NONE`): it only ever fetches history.
pub mod data;
// PUBLIC: the gatewayhub's facts lane reads FXCM's account currency, quote currencies and conversion rates
// through these two (decision 0183). `home_conversion` is the PURE judgement of what the shim's
// `fc_system_properties` / `fc_offers` return; `reads` is the registry that reaches an account's running
// reconcile session without a second login.
pub mod home_conversion;
mod loader;
// PUBLIC: the gatewayhub's price stream (`crates/vike-gatewayhub/src/data/fxcm_quotes.rs`) listens on this desk,
// which polls the Offers table of the same running reconcile session (the any-symbol PRICE lane, decision 0174).
pub mod quotes;
pub mod reads;
// PUBLIC because `crates/vike-bridge-core/tests/bridge_conformance.rs` drives it as the fxcm
// `ConformanceBridge`: `event_mapper` is the crate's whole PURE layer — the stateless
// shim-envelope decode plus the request preflight and the placement mapper lifted out of [`exec`].
pub mod event_mapper;
mod exec;
// The venue-adapter contract (`vike_bridge_core::venue_adapter::VenueAdapter`) for FXCM. NOT behind this
// crate's `fxcm` feature, which only gates `build.rs`'s shim compile: the module compiles the same
// code in every build, and `vike-tradehub`'s `fxcm` feature decides whether it is REGISTERED.
pub mod adapter;
pub mod recon_client;
// A session that failed to open is tried again for the life of the mount (the exec thread's pause schedule, and
// the reconcile client that retries behind a start's back).
mod redial;
mod sys;

pub use catalog::{FxcmCatalog, bundled_instruments};
pub use config::{FxcmConfig, load_fxcm_config_from};
pub use exec::FxcmExecutionClient;
pub use recon_client::{
    FxcmReconClient, recon_client, recon_client_for, recon_client_redialing_for,
};

/// The prefix on every reject this adapter synthesizes for a start whose ForexConnect SESSION
/// never opened.
///
/// ⚠ **It exists so a login failure can never be mistaken for a VENUE reject**, and that is not a
/// theoretical distinction: `crates/bridges/fxcm/tests/fxcm_live_smoke.rs`'s `OrderRejected` arm
/// treats a reject as acceptable (FX is closed Fri 22:00 → Sun 22:00 GMT, and a closed market
/// rejects) and PASSES. Once [`FxcmExecutionClient`] answers a dead session with a reject instead
/// of silence, a dead demo account would land in exactly that arm and read green — so the smoke
/// asserts on this prefix and panics with the venue's own words instead.
pub const SESSION_UNAVAILABLE: &str = "fxcm session unavailable";

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter. Declared
/// unconditionally (outside the `fxcm` feature) — the caps are pure data, not the native SDK.
pub const CAPS: vike_venues::VenueCaps = vike_venues::venue_caps::FXCM;

/// Can this PROCESS reach the ForexConnect SDK — i.e. did the shim open?
///
/// It opens `libfcshim.so` on THIS box, once, and reports what happened: a property of the box the
/// binary runs on, never of the box that built it.
///
/// ⚠ This exists so a test can tell apart two outcomes that are otherwise identical from the
/// outside, and telling them apart is the whole point: with credentials present and no session
/// coming back, a box with NO SHIM is a legitimate skip while a box WITH one means the LOGIN
/// FAILED, which must redden — the rule `crates/vike-secrets/src/store_locator.rs`'s store
/// resolution has (an ABSENT store is silent, a present and unusable one is an ERROR), applied to
/// the venue session. [`sdk_unavailable_reason`] is the second half of that rule: it is what says
/// WHICH.
#[must_use]
pub fn sdk_available() -> bool {
    loader::Shim::get().is_some()
}

/// Why the shim did not open — every rung of the search with the loader's own error against it —
/// or `None` when it did.
///
/// This is the diagnostic half of [`sdk_available`], and it exists because the two states an
/// operator confuses are "FXCM is not configured on this box" and "FXCM is configured and I
/// installed the shim in the wrong place". A bare `false` reads as the first whichever it is.
/// [`adapter::FxcmVenueAdapter`]'s start logs this when it refuses a live start, so the refusal names
/// the paths it tried rather than only its verdict.
#[must_use]
pub fn sdk_unavailable_reason() -> Option<&'static str> {
    loader::Shim::failure()
}

/// WHICH shim this process opened, or `None` when none did.
///
/// The positive twin of [`sdk_unavailable_reason`]. The loader's ladder has three rungs and the
/// last of them is the dynamic loader's own search, so "it worked" does not say which file
/// answered — and on a box with both an installed `<root>/lib/libfcshim.so` and a stale copy on
/// `LD_LIBRARY_PATH` that is exactly the question. [`adapter::FxcmVenueAdapter`]'s start logs it, so
/// the journal records the file the session was made through rather than only that one was.
#[must_use]
pub fn sdk_shim_path() -> Option<&'static str> {
    loader::Shim::get().map(|s| s.path.as_str())
}

/// WHAT THIS BOX'S SHIM CAN DO, as the shim itself reports it — or `None` when none opened.
///
/// `0` means the installed `libfcshim.so` predates `fc_abi_version` and therefore predates
/// `fc_login_ex`: a login failure on this box can only report THAT it failed, never why. The
/// answer is worth having at START rather than at the first failure, because the remedy is an
/// operator action (`just fxcm-package <root> <shim>`) and the moment to learn it is not
/// mid-incident. [`adapter::FxcmVenueAdapter`]'s start logs it beside the shim path.
///
/// ⚠ A shim being OLD is not a reason to refuse a live start and must never become one — it
/// executes orders exactly as it always did. The whole point of binding the new symbols optionally
/// is that a diagnostic can never cost a venue.
#[must_use]
pub fn sdk_abi_version() -> Option<i32> {
    loader::Shim::get().map(|s| s.abi_version)
}

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_venues::caps_for("fxcm");
        assert_eq!(super::CAPS, caps);
        // the exec loop amends a resting entry's PRICE (`EditOrder`, shim ABI 2) and nothing else;
        // no batch, no wire reduce-only, no live DataClient.
        assert!(caps.supports_modify);
        assert!(!caps.supports_native_batch);
        assert!(!caps.supports_reduce_only);
        assert!(!caps.has_live_data());
        // Expanded axes: the exec loop reads `order_type` and routes `"market"` to the shim's
        // true-market placement (`fc_place_market`, `O2G2::Orders::TrueMarketOpen`) and a limit to
        // the resting-LIMIT entry — at the requested price (`place_limit_at`, shim ABI 2), or at the
        // fixed `place_limit_entry` distance when it names none — so those two kinds, and only
        // those two, are wired. No trigger kinds. TIF/margin never read; no batch.
        // LIVE-VERIFIED (market-fill path only): the fcshim.cpp:122 real-SDK build break
        // (getInstrument on IO2GTradeRow, which lives only on IO2GTradeTableRow — the base row is
        // resolved via its offer id now) is fixed, and a demo market-order round-trip (BUY → fill
        // @ 1.14268 with the offer-resolved instrument → net-flat SELL) is green on the FXCM demo —
        // see tests/fxcm_live_smoke.rs::fxcm_market_round_trip. The phantom-cancel fix's other two
        // Orders(Delete) branches — async reject ('R') and venue-initiated cancel ('C') — are STILL
        // unexercised live, and the reason is no longer "pending" but a NAMED condition that could
        // not be produced on a demo account. It is stated once, in `vike_venues::venue_caps`'s FXCM
        // row, beside the LIVE-VERIFIED claims it qualifies — not restated here, because the last
        // three copies of this paragraph in this tree disagreed with each other.
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        assert_eq!(caps.supported_order_kinds, &["market", "limit"]);
        assert_eq!(caps.trigger_types, &[] as &[TriggerType]);
        assert_eq!(caps.accepted_tifs, &[TimeInForce::Gtc]);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
    }
}
