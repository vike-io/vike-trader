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
pub use vike_analytics::{
    metrics, overfit, periods, report, result, sizing, validation, zero_trade,
};
// The vike-fills cluster, re-exported on exactly the same terms: the shared fill core (cost
// scalars, fill-price tiers, intrabar resolution, stale-price wait) lives one crate down so the
// paper exchange and this simulator can share ONE definition of a fill, and every
// `vike_backtest::fill_model::…` / `crate::broker_sim::…` path still resolves here.
pub use vike_fills::{broker_sim, fill_model, fill_resolution, staleness};
// The R7 paper exchange, re-exported for the same reason and on the same terms. It moved OUT once
// vike-fills existed: it named the shared fill core, `vike_exec` and `vike_model` — never the
// engine (its only two `crate::engine::…` mentions were DOC links) — so `vike-mount`/`vike-run`,
// which wanted this client and nothing else from this crate, were compiling the whole simulator to
// obtain an `ExecutionClient`. `vike_backtest::paper::…` still resolves, and `tests/r7_gate.rs`
// still runs HERE, because it drives the engine AND the live core and so belongs to neither crate
// alone — which also keeps the backtest == paper equivalence law gated from this side.
pub use vike_paper as paper;

pub mod bar_buffer;
pub mod binutil; // shared bin glue: argv helpers (re-exported from vike-analytics) + the repo-root hist-store default
pub mod cheap_np; // the Polymarket 5m up/down fair-value taker (port of fair_value_bot/cheap_bot.py)
pub mod cheap_np_ask; // cheap_np edge re-scoring at the price a taker can actually obtain
pub mod cheap_np_book; // reconstructing the recorded Polymarket L2 book (shared by the measurement bins)
pub mod engine;
pub mod fair_value; // pure fair-value math (port of fair_value_bot/strategy.py) — no I/O, no engine types
#[cfg(feature = "hist-replay")]
pub mod harness; // BacktestNode-lite: config-driven end-to-end backtest harness (Task 1: BacktestProfile)
#[cfg(feature = "hist-replay")]
pub mod hist_replay; // tick-replay Phase 1: HistStore -> Vec<Tick> -> run_ticks loader
pub mod impact; // opt-in market-impact slippage models for the bar path (no book to walk)
pub mod latency; // opt-in two-leg order-latency models for the tick/book replay path
pub mod objective; // pluggable sweep-ranking objectives over BacktestReport — feature-free like report
pub mod queue_model; // opt-in queue-position models for resting maker limits (tick/book replay)
pub mod ref_strategies;
pub mod runs; // what a run LEAVES BEHIND: the COMMON run manifest + the run directory under user_data/runs
pub mod schedule;
pub mod search; // Euler (successive-halving) parameter search — pure core, feature-free like objective
pub mod sport_taker; // the Polymarket sports/esports copy-trading taker (port of sport_taker/*.py)
pub mod timeframe;
pub mod vector_engine;
pub mod walkforward;
// The `backtest` CLI as a library function, so the bin and the `vike` multicall dispatcher reach
// one copy of the DataFusion closure instead of two. Gated exactly as the bin is
// (`required-features = ["datafusion-store"]`).
// `backtest --fetch` — real public bars into the store. Gated by its own feature: the venue
// bridge it needs brings the blocking transport stack, and this crate sits under vike-cli.
#[cfg(feature = "datafusion-store")]
pub mod backtest_cli;
#[cfg(feature = "venue-fetch")]
pub mod fetch;
// The published starter dataset — real bars over plain HTTPS, for the boxes a venue cannot be
// reached from. Same feature as `fetch`: it is the same question with a different answer.
#[cfg(feature = "venue-fetch")]
pub mod starter;

pub use bar_buffer::BarSeriesBuffer;
pub use broker_sim::{adverse_fill_price, fee, funding_charge};
pub use cheap_np::{CheapNp, CheapNpMode, CheapNpSignal, EntryPrice, TokenId};
pub use engine::{
    format_instrument, EngineParams, EquitySampling, FillModelKind, MirrorFill, MirrorFunding,
    OptionExpirySource, OptionRight, OptionSpec, ResolutionSource, SettlementFill, SimBroker,
    StrategyEngine, Tick, VariationSettlement, DEFAULT_IMPACT_WINDOW, RESOLUTION_PROBE_SENTINEL,
};
pub use fill_model::{BarFillModel, FillModel, L2BookFillModel, TickFillModel};
pub use fill_resolution::resolve_intrabar_fills;
#[cfg(feature = "hist-replay")]
pub use hist_replay::{
    merge_quote_trade, merge_ticks, merge_ticks_by_arrival, properties_source, replay_ticks,
    tick_arrival_ts, tick_local_ts, tick_venue_ts, ReplayError, SeriesKind, SeriesRef,
    TickReplayConfig,
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
    window_stats, AlmgrenChriss, ImpactInputs, ImpactModel, MarketStats, AC_ALPHA, AC_BETA, AC_ETA,
    AC_GAMMA,
};
pub use latency::{
    ConstantLatency, IntpOrderLatency, LatencyModel, LatencyModelKind, LatencyOrder, LatencyRow,
    VENUE_HOLD_POLYMARKET_SPORTS_GAME_MS, VENUE_HOLD_POLYMARKET_UPDOWN_MS,
};
pub use objective::{
    multi_metric, multi_metric_score, trade_count_penalty, MultiMetricParams, Objective,
};
pub use queue_model::{
    ProbFunc, ProbQueueModel, QueueModel, QueueModelKind, QueueState, RiskAdverseQueueModel,
};
pub use schedule::{period_key, DateRule, Schedule};
pub use sport_taker::{
    flat_stake_pnl, market_symbol, signal_symbol, FiredEntry, Signal, SignalTracker, SportTaker,
    WalletBuy,
};
pub use timeframe::{parse_timeframe, resample};
pub use vector_engine::{fast_portfolio_backtest, Matrix, VectorBacktestEngine};
pub use vike_analytics::result::BacktestResult;
pub use vike_model::{consolidate_quotes, consolidate_trades, Strategy};

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
