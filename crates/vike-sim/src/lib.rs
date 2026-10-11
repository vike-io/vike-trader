//! vike-sim — the backtest SIMULATOR: one run, no harness.
//!
//! [`StrategyEngine`]/[`SimBroker`] are the ONE event engine (any N symbols), [`VectorBacktestEngine`]
//! the ONE vectorized kernel (any S), and around them the opt-in realism models the engine consults
//! — market impact ([`AlmgrenChriss`]), two-leg order latency ([`LatencyModelKind`]) and queue
//! position ([`QueueModelKind`]) — plus the schedule/timeframe helpers the engine steps with and the
//! reference strategies ([`RotationTopK`], [`BracketPerSymbol`], …) that are coupled to it. The
//! engines were unified per the 2026-07-04 engine-unification spec; `SingleSymbolEngine` and
//! `fast_backtest` are retired.
//!
//! What this crate is NOT is the harness: profiles, hist replay, parameter sweeps and searches, the
//! compute daemon and the bins all stay in `vike-backtest`, which depends on this crate. This one
//! opens no store, reads no environment and names no venue — the split and its argument are
//! docs/decisions/0087-the-simulator-leaves-the-harness.md.
//!
//! # One name per item — the modules are SEALED
//!
//! Every module below is `pub(crate)`, and the items callers need are named ONCE, at this crate's
//! root (`vike_sim::SimBroker`, never `vike_sim::engine::SimBroker`). That is the remedy the root
//! `CLAUDE.md`'s one-name rule prescribes for a name that has already SPLIT, and these had: while the
//! simulator lived in `vike-backtest`, callers spelled both `vike_backtest::SimBroker` and
//! `vike_backtest::engine::SimBroker`, `…::LatencyModelKind` and `…::latency::LatencyModelKind`.
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
pub(crate) mod ref_strategies;
pub(crate) mod schedule;
pub(crate) mod timeframe;
pub(crate) mod vector_engine;

// ── The crate-root vocabulary ─────────────────────────────────────────────────────────────────
// Only THIS crate's own items. Adding a name here is how an item becomes reachable at all, so an
// item a caller needs goes HERE — never a `pub mod` that would mint the second, module-path
// spelling the sealing exists to prevent.

pub use bar_buffer::BarSeriesBuffer;
pub use engine::{
    DEFAULT_IMPACT_WINDOW, DecideMode, EngineParams, EquitySampling, FillModelKind, MirrorFill,
    MirrorFunding, OptionExpirySource, OptionRight, OptionSpec, RESOLUTION_PROBE_SENTINEL,
    ResolutionSource, SettlementFill, SimBroker, StrategyEngine, Tick, VariationSettlement,
    format_instrument,
};
// `AC_DELTA` is deliberately NOT named here: it is the exponent of the paper's liquidity adjustment,
// which this crate does not apply (no shares-outstanding series, undefined for crypto). It stays in
// `impact.rs` for provenance only — see its own doc — and no caller could ever correctly use it.
pub use impact::{
    AC_ALPHA, AC_BETA, AC_ETA, AC_GAMMA, AlmgrenChriss, ImpactInputs, ImpactModel, ImpactTerms,
    MarketStats, TickWindow, window_stats,
};
pub use latency::{
    ConstantLatency, IntpOrderLatency, LatencyModel, LatencyModelKind, LatencyOrder, LatencyRow,
};
pub use queue_model::{
    ProbFunc, ProbQueueModel, QueueModel, QueueModelKind, QueueState, RiskAdverseQueueModel,
};
// The reference strategies had NO root name while they lived in `vike-backtest` — every caller
// (the harness registry, the r4 parity gate, the walk-forward tests) reached them by module path.
// Sealing the module makes the root the only door, so all five are named here.
pub use ref_strategies::{
    BracketPerSymbol, CapsSizersMask, GatedWeights, RotationTopK, TickPairMse,
};
pub use schedule::{DateRule, Schedule};
pub use timeframe::{parse_timeframe, resample};
pub use vector_engine::{Matrix, VectorBacktestEngine, fast_portfolio_backtest};
