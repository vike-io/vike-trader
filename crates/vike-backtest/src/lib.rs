//! vt-sim — the vike trading core's sim/backtest components (R1 scope).
//!
//! Exact ports (origin/main `023d6e8`) of:
//! - `core/broker_sim.py`      → [`broker_sim`] (the 3 canonical cost scalars)
//! - `core/orders.py`          → `vike_model::fill_trigger` (`order_fill_price` + `_granular`;
//!   moved in accounting-upgrade Phase C — call sites spell `vike_model::`, no re-export here)
//! - `core/fill_model.py`      → [`fill_model`] (Bar/Tick tiers + the net-new L2 book-walk tier)
//! - `core/fill_resolution.py` → [`fill_resolution`] (adverse-first + SL/TP bracket cap)
//!
//! (`core/consolidator.py` moved to `vike-model` — shared with `vike-data`. Its two SERIES
//! consolidators are re-exported here for backward compatibility; the per-tick
//! `quote_tick_to_bar`/`trade_tick_to_bar` projections are NOT — every call site, this crate's
//! own [`engine`] included, already spells `vike_model::`.)
//!
//! The engines are unified per the 2026-07-04 engine-unification spec: [`engine`] holds the ONE
//! event engine (`StrategyEngine`/`SimBroker`), [`vector_engine`] the ONE vectorized kernel
//! (`VectorBacktestEngine`); `SingleSymbolEngine`/`fast_backtest` are retired.
//!
//! The pure ANALYTICS cluster (the metrics catalog, `BacktestResult`/`BacktestReport`, the
//! benchmark/excursions/periods/montecarlo/stability/overfit/validation port, the sizer registry
//! and the zero-trade diagnoser) now lives in [`vike_analytics`] — it named `vike_model` and
//! nothing else, so a consumer that just wants the numbers no longer compiles this simulator to
//! get them. The modules something actually reaches through here are RE-EXPORTED at this crate's
//! root below, so `vike_backtest::metrics::…`, `vike_backtest::BacktestResult` and the in-crate
//! `crate::report::…` paths are unchanged. `benchmark`/`excursions`/`montecarlo`/`stability` are
//! deliberately NOT: no path of either spelling ever resolved through this crate, so re-exporting
//! them minted a second name for nothing. They stay where they live, at `vike_analytics::…`.
//!
//! The shared FILL cluster ([`broker_sim`], [`fill_model`], [`fill_resolution`], [`staleness`])
//! now lives in [`vike_fills`], on exactly the same terms and for the same reason: it named
//! `vike_model` and nothing else, and the R7 [`paper`] exchange needs the identical cost model
//! the engine uses — "backtest == paper == live" is only checkable while there is ONE definition
//! of a fill. Every module is RE-EXPORTED at this crate's root below, so
//! `vike_backtest::fill_model::…`, the flat `vike_backtest::{adverse_fill_price, FillModel, …}`
//! names and the in-crate `crate::broker_sim::…` paths are unchanged.
//!
//! PARITY RULES: see vike-model — f64 end-to-end, same expression order, no mul_add/fast-math;
//! `%` on timestamps is Python floor-mod → `rem_euclid`.

// The vike-analytics cluster, re-exported so every `vike_backtest::<module>::…` path (and every
// `crate::<module>::…` path inside this crate) resolves exactly as it did before the extraction.
// These are `pub use`d MODULES, not `pub mod`s: the code lives one crate down.
// ⚠ NOT every module of that crate — `benchmark`/`excursions`/`montecarlo`/`stability` are absent
// on purpose (see the module doc above). Re-adding one is only correct once a call site needs the
// `vike_backtest::` spelling, which none ever has.
// ⚠ `metric_catalog` and `realism` joined this list on the backtest-CLI-parity branch and are
// REMOVED again here. The comment that admitted them asserted that "`vike-cli` depends on THIS
// crate and not on vike-analytics", so the metric-selection parser and the realism stamp were
// "unreachable from the CLI's `--metrics` arm without the `vike_backtest::` spelling". Both halves
// of that are the opposite of the tree: `crates/vike-cli/Cargo.toml` takes `vike-analytics` as a
// NORMAL dependency (its own rationale argues the one external crate that edge adds) and takes
// THIS crate only under `[dev-dependencies]`, so a `vike_backtest::` spelling is not reachable
// from the CLI's shipped code at all. The real call site is
// `crates/vike-cli/src/cmd/runs/show.rs`'s `report_key_order`, which writes
// `vike_analytics::metric_catalog::` fully qualified; and the one in-crate reader bypasses the
// re-export from inside the very crate that declared it — `harness::report::realism_stamp` spells
// `use vike_analytics::realism::RealismStamp`. So neither name ever had a caller at either
// spelling, which is exactly what the condition above forbids. That condition is the half of the
// old comment worth keeping, and it is kept.
pub use vike_analytics::{
    metrics, overfit, periods, report, result, sizing, validation, zero_trade,
};
// The vike-fills cluster, re-exported on exactly the same terms: the shared fill core (cost
// scalars, fill-price tiers, intrabar resolution, stale-price wait) lives one crate down so the
// paper exchange and this simulator can share ONE definition of a fill, and every
// `vike_backtest::fill_model::…` / `crate::broker_sim::…` path still resolves here.
pub use vike_fills::{broker_sim, fill_model, fill_resolution, staleness};
// ⚠ **`pub use vike_paper as paper;` stood here until 2026-09-23 and is DELETED.** It was kept so
// that `vike_backtest::paper::…` "still resolves" after the R7 paper exchange moved out — which is
// word for word the thing the conventions forbid: *a re-export kept so old spellings compile is a
// second name that rots*, and the 2026-09-18 ruling says a symbol has ONE name. The
// public-API-surface exception does not cover it, because this is not this crate's vocabulary: it
// is another crate's, spelled again here.
//
// Nothing outside this crate ever used the alias — `vike-mount` and `vike-run` name `vike-paper`
// directly, which was the point of extracting it — and the only callers were five of this crate's
// own test files, which say `vike_paper::…` now. That makes the edge a DEV one: `tests/r7_gate.rs`
// still runs HERE, because the backtest == paper equivalence law drives the engine AND the live
// core and so belongs to neither crate alone, but a test-only need is a `[dev-dependencies]` need.
// The manifest carries the rest of the argument.

pub mod bar_buffer;
pub mod binutil; // shared bin glue: argv helpers (re-exported from vike-analytics) + the repo-root hist-store default
// What a run is about to READ, answered before it reads it: the `data.explain` plan, the
// `data.require_coverage` gate and the `data.universe` rule, over ONE resolution of the slice
// (`run_fingerprint::planned_series_ids`, which the run ADDRESS already needed). Gated with
// `harness` because it names `BacktestProfile` and the `HistStore` trait — the same level
// `compute_server` sits at, which is what lets the REMOTE route answer these too.
#[cfg(feature = "hist-replay")]
pub mod data_plan;
pub mod engine;
#[cfg(feature = "hist-replay")]
pub mod harness; // BacktestNode-lite: config-driven end-to-end backtest harness (Task 1: BacktestProfile)
#[cfg(feature = "hist-replay")]
pub mod hist_replay; // tick-replay Phase 1: HistStore -> Vec<Tick> -> run_ticks loader
// The COMPUTE daemon `vike-backend backtest --addr` runs (ruling 7): the seven verbs that RUN
// something, over the same node protocol `vike-datahub` serves the store on. Gated with the harness
// it dispatches to — `hist-replay`, not `datafusion-store`, deliberately: the server holds the store
// as `Arc<dyn HistStore>` and names no concrete backend, so the existing `hist-replay` clippy lane
// compiles it and `vike-studio-core` (which takes this crate with `hist-replay`) can name the
// `StudioRunTable` type it fills. Only the DAEMON ARM in `backtest_cli` opens a `DataFusionHist`.
#[cfg(feature = "hist-replay")]
pub mod compute_server;
pub mod impact; // opt-in market-impact slippage — every lane, charged the terms its price law misses
pub mod latency; // opt-in two-leg order-latency models for the tick/book replay path
pub mod objective; // pluggable sweep-ranking objectives over BacktestReport — feature-free like report
// The run profile's TOML schema as DATA, DERIVED from `harness/profile.rs` by `include_str!`
// rather than restated — deliberately NOT behind `hist-replay`, so CI's default roster lane runs
// its gates (it only reads the source as text; it compiles none of the harness).
pub mod profile_surface;
pub mod queue_model; // opt-in queue-position models for resting maker limits (tick/book replay)
pub mod ref_strategies;
#[cfg(feature = "hist-replay")]
pub mod run_fingerprint; // what a run's INPUTS were: the data slice as the store held it, and the address over it
pub mod schedule;
pub mod search; // Euler (successive-halving) parameter search — pure core, feature-free like objective
pub mod timeframe;
// What a parameter SEARCH leaves behind. UNGATED deliberately — these are DOCUMENTS, so a default
// build compiles and tests them without the `hist-replay` harness tree; the RECORDER that writes
// them lives under that gate, at `harness::trials`.
pub mod trial_ledger;
pub mod vector_engine;
pub mod walkforward;
// `BacktestResult` -> the `WireRunResult` DTO. Behind `hist-replay` because that is the feature that
// brings `vike-datahub-client` in; it MOVED here from `vike_studio_core::wire_run` when the named
// run needed the same rendering from a crate that cannot name the Studio (see the module doc).
#[cfg(feature = "hist-replay")]
pub mod wire_result;
// The NAMED RUN's server half (`docs/decisions/0064-a-named-run-carries-no-source.md`) — the ONE
// run path in this crate that does not call `harness::registry::strategy_by_name`, because the
// resolution happens in a crate that cannot name `vike-script`. Same `hist-replay` gate as
// `compute_server`, which is its only caller: it needs the wire DTOs, the store trait and the
// generated user roster, all three of which ride that feature.
#[cfg(feature = "hist-replay")]
pub mod named_run;
// The `backtest` CLI as a library function, so the bin and the `vike` multicall dispatcher reach
// one copy of the DataFusion closure instead of two. Gated exactly as the bin is
// (`required-features = ["datafusion-store"]`).
// The on-chain -> CLOB matching rule for the Polymarket measurement bins: the anchor and its two
// tolerances, split out of `vike_data::cheap_np_book` so the DATA crate carries the archive fold
// and not one study's matching heuristics. Gated with the two bins that are its only callers, for
// the reason its own module doc gives.
#[cfg(feature = "datafusion-store")]
pub mod backtest_cli;
#[cfg(feature = "datafusion-store")]
pub mod cheap_np_match;
// The published starter dataset — real bars over plain HTTPS, for the boxes a venue cannot be
// reached from.
//
// ⚠ **`pub mod fetch` used to sit directly above this line, and its removal is the point of the
// change that deleted it.** That module held this crate's ONE venue call
// (`vike_binance::data::fetch_klines_range`), which made the COMPUTE plane a client of an
// exchange. History is fetched by the data plane, once, into the store; `vike-cli data fetch` asks
// a datahub for it. This module is what is left, and it reaches no venue at all — a plain HTTPS GET
// of a prepared dataset from a GitHub release.
//
// ⚠ So the feature gating it still reads `venue-fetch` and no longer fetches from a venue. That is
// a name that has stopped describing its contents, left deliberately rather than by oversight: the
// rename was raised and DEPRIORITISED by the owner as churn, and it moves four gate rows
// (`crates/vike/Cargo.toml`'s `backtest`, `crates/vike-ops/tests/multicall_gate.rs`'s
// `SHIPPED_TOOL_FEATURES`, and this file) for no behaviour. Whoever renames it next is holding the
// argument for it here.
#[cfg(feature = "venue-fetch")]
pub mod starter;

pub use bar_buffer::BarSeriesBuffer;
pub use broker_sim::{adverse_fill_price, fee, funding_charge};
pub use engine::{
    DEFAULT_IMPACT_WINDOW, DecideMode, EngineParams, EquitySampling, FillModelKind, MirrorFill,
    MirrorFunding, OptionExpirySource, OptionRight, OptionSpec, RESOLUTION_PROBE_SENTINEL,
    ResolutionSource, SettlementFill, SimBroker, StrategyEngine, Tick, VariationSettlement,
    format_instrument,
};
pub use fill_model::{BarFillModel, FillModel, L2BookFillModel, TickFillModel};
pub use fill_resolution::resolve_intrabar_fills;
#[cfg(feature = "hist-replay")]
pub use hist_replay::{
    ReplayError, SeriesKind, SeriesRef, TickReplayConfig, merge_quote_trade, merge_ticks,
    merge_ticks_by_arrival, properties_source, replay_ticks, tick_arrival_ts, tick_local_ts,
    tick_venue_ts,
};
// The concrete-`DataFusionHist` streaming loader is behind `datafusion-store` (it names the concrete
// backend), NOT the trait-only `hist-replay` re-export above.
#[cfg(feature = "datafusion-store")]
pub use hist_replay::replay_ticks_streaming;
// `AC_DELTA` is deliberately NOT re-exported: it is the exponent of the paper's liquidity
// adjustment, which this crate does not apply (no shares-outstanding series, undefined for
// crypto). It stays reachable at `impact::AC_DELTA` for provenance, but it is not crate-level API
// a caller could ever correctly use — the term it belongs to does not exist here.
pub use impact::{
    AC_ALPHA, AC_BETA, AC_ETA, AC_GAMMA, AlmgrenChriss, ImpactInputs, ImpactModel, ImpactTerms,
    MarketStats, TickWindow, window_stats,
};
pub use latency::{
    ConstantLatency, IntpOrderLatency, LatencyModel, LatencyModelKind, LatencyOrder, LatencyRow,
    VENUE_HOLD_POLYMARKET_SPORTS_GAME_MS, VENUE_HOLD_POLYMARKET_UPDOWN_MS,
};
pub use objective::{
    MultiMetricParams, Objective, multi_metric, multi_metric_score, trade_count_penalty,
};
pub use queue_model::{
    ProbFunc, ProbQueueModel, QueueModel, QueueModelKind, QueueState, RiskAdverseQueueModel,
};
pub use schedule::{DateRule, Schedule, period_key};
pub use timeframe::{parse_timeframe, resample};
pub use vector_engine::{Matrix, VectorBacktestEngine, fast_portfolio_backtest};
pub use vike_analytics::result::BacktestResult;
pub use vike_model::{Strategy, consolidate_quotes, consolidate_trades};

// ── Flat names deliberately NOT re-exported here ──────────────────────────────────────────────
// Same disposition as `AC_DELTA` above, and recorded so a later sweep does not re-mint them. Each
// was a crate-root alias for a symbol that lives one crate down; every one had ZERO occurrences at
// BOTH the `vike_backtest::NAME` and the in-crate `crate::NAME` spelling, while the module path it
// duplicates is what the real callers already write.
//
// * `fill_model::book_taker_price` — this crate's `SimBroker`, the `cheap_np_askgate` bin and
//   `vike-fills` itself all reach THE taker-price law through the module path, which is also
//   where its doc chain points.
// * `staleness::{defers, is_fresh_print, is_stale}` — `defers` was the one RENAMED name in this
//   file (`staleness_defers`) and had exactly one occurrence in the tracked repo, its own
//   declaration. The engine spells all three `crate::staleness::…`.
// * `vike_analytics::report::{BacktestReport, DAILY_PERIODS_PER_YEAR, DEFAULT_PERIODS_PER_YEAR}`.
//   ⚠ `harness::BacktestReport` is a DIFFERENT path — `harness/report.rs` re-exports the same
//   three off `crate::report` — and IS live, in the bins and in `vike-datahub`. Do not "unify".
// * `vike_analytics::zero_trade::{aggregate_denials, rank_causes, ZeroTradeCause, ZeroTradeInputs,
//   ZeroTradeReport}` — `harness::sweep` and `SimBroker`'s doc chain spell `crate::zero_trade::…`,
//   which the module re-export at the top of this file keeps resolving.
// * `vike_model::{quote_tick_to_bar, trade_tick_to_bar}` — `engine.rs`, the only caller in this
//   crate, already writes `vike_model::`.
// * the whole `hist-replay`-gated `vike_strategy::{funding_capture, grid_dca, pairs,
//   trailing_scalper, AnchorMode, DcaAccumulate, FundingCapture, Grid, PairsZScore,
//   TrailingScalper}` block. Its own comment claimed "no call site moved"; every call site had in
//   fact already moved — `harness::registry`, behind that same gate and the thing that resolves
//   these by name, writes `vike_strategy::Grid::from_params` fully qualified, and so does every
//   test.
//
// Re-adding one is only correct once a call site genuinely needs the `vike_backtest::` spelling.
