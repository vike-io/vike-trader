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
//! now only gates whether `build.rs` BUILDS the shim shared object; this module tree always
//! compiles, and always compiles the SAME code.
//!
//! ⚠ **THE SDK BECAME A RUNTIME FACT ON 2026-09-09, and the sentence above used to end differently
//! ("degrading to the `#[cfg(not(fcsdk))]` stub in [`sys`]").** There is no stub any more, and no
//! `fcsdk` cfg: `build.rs` compiles `src/shim/fcshim.cpp` into `libfcshim.so`, which links the SDK
//! the ordinary way, and [`loader`] opens THAT at runtime. Nothing in a Rust binary references a
//! ForexConnect symbol, so no binary carries `DT_NEEDED libForexConnect.so` and none dies at exec
//! on a box without the libraries — which is what the separate `vike-tradehub-fxcm` asset existed
//! to avoid. [`sdk_available`] is the runtime answer where `sdk_linked()` was the compile-time one.

mod catalog;
mod config;
mod loader;
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

/// Can this PROCESS reach the ForexConnect SDK — i.e. did the shim open?
///
/// ⚠ **Renamed from `sdk_linked()` on 2026-09-09, and the rename is the change.** That function
/// answered `cfg!(fcsdk)`, a decision `build.rs` made on the build box; this one opens
/// `libfcshim.so` on THIS box, once, and reports what happened. The two answers coincide on a
/// machine that both built and runs the thing, which is exactly why the old name survived so long
/// and exactly why it had to go: a universal binary is built where the SDK is and run where it is
/// not, so "linked" would have been a permanent yes and the mount gate a permanent no-op.
/// No `pub use` shim keeps the old spelling compiling — a second name for a changed meaning is the
/// worst of both.
///
/// ⚠ This exists so a test can tell apart two outcomes that are otherwise identical from the
/// outside, and telling them apart is the whole point: with credentials present and no session
/// coming back, a box with NO SHIM is a legitimate skip while a box WITH one means the LOGIN
/// FAILED, which must redden. Before it existed,
/// `crates/bridges/fxcm/tests/fxcm_live_smoke.rs` logged a warning and returned in BOTH cases, so
/// a venue that could not be reached at all reported a green test — and it did, for as long as the
/// demo credentials were expired.
///
/// That is the shape `crates/vike-secrets/src/dotenv.rs`'s store resolution already has a rule
/// about — an ABSENT store is the ordinary unconfigured state and is silent, a store that is
/// present and unusable is an ERROR — applied to the venue session instead of the credential file.
/// [`sdk_unavailable_reason`] is the second half of that rule: it is what says WHICH.
///
/// ⚠ A FUNCTION rather than a `const`: an `assert!` on a constant is folded by the compiler and
/// `clippy::assertions_on_constants` rejects it under `-D warnings`, which is exactly how the
/// smokes need to spell it. It is also genuinely no longer constant.
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
/// `vike_mount`'s fxcm arm logs this when it refuses a live mount, so the refusal names the paths
/// it tried rather than only its verdict.
#[must_use]
pub fn sdk_unavailable_reason() -> Option<&'static str> {
    loader::Shim::failure()
}

/// WHICH shim this process opened, or `None` when none did.
///
/// The positive twin of [`sdk_unavailable_reason`]. The loader's ladder has three rungs and the
/// last of them is the dynamic loader's own search, so "it worked" does not say which file
/// answered — and on a box with both an installed `<root>/lib/libfcshim.so` and a stale copy on
/// `LD_LIBRARY_PATH` that is exactly the question. `vike_mount`'s fxcm arm logs it at mount, so the
/// journal records the file the session was made through rather than only that one was.
#[must_use]
pub fn sdk_shim_path() -> Option<&'static str> {
    loader::Shim::get().map(|s| s.path.as_str())
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
