//! vike-simulator — the backtest SIMULATOR: one run, no harness.
//!
//! [`StrategyEngine`]/[`OrderBrokerSimulator`] are the ONE event engine (any N symbols), [`VectorBacktestEngine`]
//! the ONE vectorized kernel (any S), and around them the opt-in realism models the engine consults
//! — market impact ([`ImpactModelAlmgrenChriss`]), two-leg order latency ([`LatencyModelKind`]) and queue
//! position ([`QueueModelKind`]) — plus the schedule/timeframe helpers the engine steps with. It
//! holds no strategy: the simulator test fixtures that are coupled to it are test support
//! (`test_support::fixtures`, behind `test-support`). The
//! engines were unified per the 2026-07-04 engine-unification spec; `SingleSymbolEngine` and
//! `fast_backtest` are retired.
//!
//! What this crate is NOT is the harness: profiles, hist replay, parameter sweeps and searches, the
//! backtesthub and the bins all stay in `vike-backtest`, which depends on this crate. This one
//! opens no store, reads no environment and names no venue — the split and its argument are
//! docs/decisions/0087-the-simulator-leaves-the-harness.md.
//!
//! # One name per item — the modules are SEALED
//!
//! Every module below is `pub(crate)`, and the items callers need are named ONCE, at this crate's
//! root (`vike_simulator::OrderBrokerSimulator`, never `vike_simulator::engine::OrderBrokerSimulator`). That is the remedy the root
//! `CLAUDE.md`'s one-name rule prescribes for a name that has already SPLIT, and these had: while the
//! simulator lived in `vike-backtest`, callers spelled both `vike_backtest::OrderBrokerSimulator` and
//! `vike_backtest::engine::OrderBrokerSimulator`, `…::LatencyModelKind` and `…::latency::LatencyModelKind`.
//!
//! ⚠ **Nothing from another crate is re-exported here, and nothing should be.** A run returns a
//! `vike_analytics::BacktestResult`, sizes through `vike_analytics::sizing`, and fills through
//! `vike_fills::{broker_sim, fill_model, fill_resolution, staleness}` — callers name those crates
//! directly, exactly as this crate's own modules do. `vike-backtest` once re-exported all of them
//! (plus a `vike_model` "backward compatibility" block and the paper exchange); those second names
//! were retired with the split rather than carried here.
//!
//! PARITY RULES: see vike-model — f64 end-to-end, same expression order, no mul_add/fast-math;
//! `%` on timestamps is Python floor-mod → `rem_euclid`.

#![warn(unreachable_pub)]

pub(crate) mod bar_buffer;
pub(crate) mod engine;
pub(crate) mod impact; // opt-in market-impact slippage — every lane, charged the terms its price law misses
pub(crate) mod latency; // opt-in two-leg order-latency models for the tick/book replay path
pub(crate) mod queue_model; // opt-in queue-position models for resting maker limits (tick/book replay)
pub(crate) mod schedule;
pub(crate) mod timeframe;
pub(crate) mod vector_engine;

// The bar, tick and calendar builders the crate's own test binaries share, compiled once here rather
// than once per binary (and the unified benchmark schedule vike-backtest's bench shares). Off by default;
// only dev edges (this crate's self dev-dependency, vike-backtest's) turn `test-support` on.
// The one `pub mod`: its items have no second spelling at the root, so the sealing above is intact.
#[cfg(feature = "test-support")]
pub mod test_support;

// ── The crate-root vocabulary ─────────────────────────────────────────────────────────────────
// Only THIS crate's own items. Adding a name here is how an item becomes reachable at all, so an
// item a caller needs goes HERE — never a `pub mod` that would mint the second, module-path
// spelling the sealing exists to prevent.

pub use bar_buffer::BarSeriesBuffer;
pub use engine::{
    ConversionRate, ConversionSeries, DEFAULT_IMPACT_WINDOW, DecideMode, EquitySampling,
    FillModelKind, InBarRules, InputSeries, Inputs, MirrorFunding, OptionExpirySource,
    OrderBrokerSimulator, RESOLUTION_PROBE_SENTINEL, ResolutionSource, SettlementFill,
    SettlementVariation, ShortfallCover, StrategyEngine, StrategyEngineConfig, TickEvent,
    format_instrument,
};
// `AC_DELTA` is deliberately NOT named here: it is the exponent of the paper's liquidity adjustment,
// which this crate does not apply (no shares-outstanding series, undefined for crypto). It stays in
// `impact.rs` for provenance only — see its own doc — and no caller could ever correctly use it.
pub use impact::{
    AC_ALPHA, AC_BETA, AC_ETA, AC_GAMMA, ImpactInputs, ImpactModel, ImpactModelAlmgrenChriss,
    ImpactModelMarketStatistics, ImpactModelTickWindow, ImpactTerms, window_stats,
};
pub use latency::{
    LatencyModel, LatencyModelConstant, LatencyModelInterpolated, LatencyModelKind, LatencyOrder,
    LatencyRow,
};
pub use queue_model::{
    QueueModel, QueueModelKind, QueueModelProbability, QueueModelProbabilityFunction,
    QueueModelRiskAverse, QueueState,
};
pub use schedule::{Schedule, ScheduleDateRule};
pub use timeframe::parse_timeframe;
pub use vector_engine::{Matrix, VectorBacktestEngine, fast_portfolio_backtest};

/// The sources declaring the two backtest-profile tables that ARE this crate's types, verbatim, each
/// beside its repo-relative path: `[engine.in_bar]` is [`InBarRules`] and `[engine.margin_call]` is
/// [`ShortfallCover`]. `vike_backtest::profile_surface` publishes the profile's schema by parsing the
/// sources that declare it, and reads these through the dependency edge, as it reads `[risk]` through
/// `vike_model::risk::surface` (whose doc says why an `include_str!` across a package boundary is not
/// the way). Its parser panics on a field line it cannot read, so a refactor of either struct fails
/// that gate instead of silently shortening a published table.
#[cfg(feature = "profile-schema")]
pub const PROFILE_TABLE_SOURCES: &[(&str, &str)] = &[
    (
        "crates/vike-simulator/src/engine/sim_broker/in_bar.rs",
        include_str!("engine/sim_broker/in_bar.rs"),
    ),
    (
        "crates/vike-simulator/src/engine/sim_broker/knobs.rs",
        include_str!("engine/sim_broker/knobs.rs"),
    ),
];
