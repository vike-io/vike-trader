//! `vike-analytics` — the pure performance-analytics cluster, extracted verbatim out of
//! `vike-backtest`.
//!
//! # Why this crate exists
//!
//! `vike-report` (the LIVE tearsheet reader, dissolved 2026-10-12) reimplemented no metric: every number it printed was a
//! call into this catalog. But the catalog used to live inside `vike-backtest`, so vike-report
//! compiled **32,259 lines to reach ~3,900** — 89% of what it built it could not call, including
//! the simulator (`engine`/`sim_broker`), the paper exchange, and the whole gated `harness/` tree.
//! Splitting on the seam that was already there (these modules name `vike_model` and **nothing
//! else** — the only `vike_execution` mention in the whole cluster was a doc comment in
//! [`record`]) lets a consumer take the numbers without the machine that produced them.
//!
//! # The layering rule this crate holds
//!
//! **`vike-model` and the three crates below it (`vike-marketdata`, `vike-math`, `vike-utility`) are the
//! ONLY vike dependencies, with `vike-trade-ledger` (12), the closed-trade ledger that left this crate.**
//! No `vike-execution`, no `vike-core`, no `vike-data`.
//! That is the property the split exists to create, and adding any of the three would silently
//! undo it — a would-be dependency is a signal that the item belongs in `vike-backtest`, not here.
//!
//! # Contents
//!
//! - [`metrics`] — the metrics catalog (sharpe/sortino/drawdown/CAGR/VaR/ES/SQN/…), the ONE home
//!   for every performance number; the live tearsheet door and `vike-backtest` both call it, so a live and a
//!   backtest tearsheet cannot drift.
//! - [`metric_catalog`] — the metric TAXONOMY as data: one row per number this tree can answer,
//!   plus the rows it deliberately cannot and why. The declaration order IS the render order, so a
//!   consumer asks this module for the ordered ids rather than keeping the hand copy that three
//!   consumers had each grown their own of.
//! - [`record`] / [`results`] — [`TradingRecord`] (the run's raw output, which names its
//!   [`TradingRecordSource`]: a backtest, a walk-forward step or a live account) and
//!   [`results::TradingResults`] (the flat, `Serialize` summary composed from it by `metrics::` calls
//!   only), now carrying the long-form [`results::TradingResultsExtendedMetrics`] catalog and the
//!   [`results::TradingResultsHonesty`] that tell an AMBIGUOUS run apart from a clean one.
//! - [`results_comparison`] — [`results_comparison::TradingResultsComparison`], the ONE comparison of two
//!   results (A against B: every catalog row and its `b − a`) that `report --compare`, the Studio's
//!   "Compare all" and a forward test's end render; its HTML is [`render_comparison_html`].
//! - [`walkforward`] — the walk-forward results: each out-of-sample step's [`results::TradingResults`] and
//!   the stitched walk's overall ones, which the Studio renders without linking the harness that
//!   folds them.
//! - [`realism`] — [`RealismStamp`], the cost model a run actually ran under. Two results are
//!   comparable only if they were priced the same way, and a report used to say nothing at all
//!   about which market produced it.
//! - [`benchmark`] / [`excursions`] / [`periods`] / [`montecarlo`] / [`stability`] / [`overfit`] /
//!   [`validation`] — the analytics port: benchmark-relative stats, MAE/MFE, calendar bucketing,
//!   resampling, rolling stability, PBO/deflated-Sharpe, and the walk-forward/purged-k-fold split
//!   generators.
//! - [`permutation`] — the seeded bar-history permutation a Monte Carlo permutation test re-runs a
//!   backtest on (Masters): the first bar, the clock and the return distribution kept, the order gone.
//! - [`labels`] / [`sample_weights`] / [`factor_evaluation`] — supervised-learning research (López de
//!   Prado, *Advances in Financial ML* ch. 3-4): triple-barrier, meta- and trend-scanning labels, label
//!   uniqueness and the sequential bootstrap, the rank Information Coefficient with quantile returns. The
//!   CUSUM event filter and SADF they pair with are pure formulas, `vike_math::change_detection` and
//!   `vike_math::stationarity_tests` (decision 0294), as are the GARCH(1,1) and two-state HMM fits of the
//!   `garch_volatility` and `hmm_regime` rows (`vike_math::garch`, `vike_math::hidden_markov`).
//! - [`model_drift`] — model drift: the per-feature, per-prediction
//!   [`model_drift::model_drift`] over the statistics of `vike_math::change_detection`.
//! - the market-impact model is NOT here: it and its fit (`calibrate_impact`) left this crate's
//!   `market_impact` module for `vike_fills::market_impact` on 2026-11-18, beside the fill price and
//!   the commission, so the paper book that charges it compiles none of this crate.
//! - [`forward_test`] — a forward test held against its backtest (book chapter 22): round trips,
//!   the pairing of the two records' trades with their slippage, and both records on one daily grid;
//!   the length a record needs is [`overfit::minimum_track_record_length`].
//! - closed-trade accounting is NOT here: the ledger and its trades analysis are the crate below,
//!   `vike_trade_ledger` (it left this crate's `closed_trades` module on 2026-11-18, so the live core that
//!   books with it compiles none of this crate). [`record`] and [`results`] carry its `TradeClosedMetrics`
//!   block only when the run arms the mode, and [`html`] renders it.
//! - [`book_export`] — a RECORDED order book as training data for a DeepLOB-style network: rows of
//!   the top levels per side, folded the backtest's way, and a three-class mid-move label `k` events
//!   ahead, point in time; and DeepLOB's input windows of those rows, each scaled by itself.
//! - [`synthetic`] — seeded synthetic market data for a backtest or a study to run on: OHLCV bar paths
//!   from four price models (each bar one simulated intrabar path), a block bootstrap of real bars or
//!   a learned next-step model a study fitted, a Cont-Stoikov-Talreja limit order book as the three
//!   tick streams a replay reads, and the stylized facts that compare a path with its source. The
//!   caller holds the values; `vike_data::HistoryStoreSynthetic` serves them to a backtest.
//! - [`zero_trade`] — the zero/near-zero-trade cause analyzer over already-collected diagnostics.
//! - [`instrument_graph`] — the catalog as a graph (one asset across venues, Polymarket neg-risk
//!   sets, each venue's spot–perpetual–future–option links) and its price features (lead-lag edges
//!   from cross-correlation, net leadership, eigenvector centrality).
//! - [`results_table`] / [`html`] / [`trades`] / [`equity`] / [`mtm`] — the LIVE TEARSHEET's pure
//!   core, moved here out of `vike-report` on 2026-09-28: the [`TradingResultsTable`] document keyed on
//!   the catalog, its self-contained HTML renderer ([`render_html`]), the fill → closed-trade fold
//!   ([`reconstruct_trades`]), the two equity-curve builders and the mark-to-market fold with its
//!   [`MarkToMarketRuntimeStatistics`]. None of it reads a journal or a store — every input is a `vike_model` value
//!   the caller already holds — which is what lets it live under this crate's layering rule and
//!   what lets a consumer that only RENDERS (`vike-cli`'s `backtest show --html`) name it without
//!   linking a journal reader. [`trading_results_table_from_fills`] is the ONE fold from fills to a document
//!   (with [`TradingResultsTableFillOrder`], the explicit "as held" or "by time" choice its callers differ on); the
//!   readers stayed out: the journal READ is `vike_journal::fills_from_journal`, and the live account
//!   report keeps the store's (`crates/vike-client/src/account_report/history.rs`'s `mtm_curve_from_store`).
//!
//! # Compatibility
//!
//! This extraction moved code, not behaviour. Every caller names this crate directly —
//! `vike_analytics::metrics::…`, `vike_analytics::TradingRecord`, `vike_analytics::results::…`.
//! The tearsheet half is named at the CRATE ROOT only (`vike_analytics::TradingResultsTable`,
//! `vike_analytics::render_html`, …): that is the vocabulary `vike-report` exported it under, and
//! every consumer was re-pointed to it in the move rather than to the module paths, so each of
//! those items has one spelling outside this crate.
//! ⚠ `vike-backtest` RE-EXPORTED eight of these modules and `TradingRecord` at its own crate root
//! "so every existing path resolves exactly as before" until 2026-09-27; those second names were
//! retired under the root `CLAUDE.md`'s one-name rule when the simulator split out of it
//! (docs/decisions/0087).
//!
//! # Cross-platform determinism
//!
//! **Every transcendental in this crate is the `libm` CRATE, never `f64`'s inherent method.**
//! `libm::pow`/`libm::log`/`libm::exp`/`libm::erfc` — never `powf`/`ln`/`exp`. IEEE 754 does not
//! require `pow`/`log`/`exp` to be correctly rounded, so `f64::powf` is whatever the PLATFORM's
//! libm does: MSVC's CRT on the Windows desktop, glibc on the Linux prod boxes. The two disagree
//! by up to 1 ulp, and `signal_backtest::equity_from_pnl` — the EQUITY CURVE — was measurably
//! among the casualties, so a backtest's headline number depended on which box ran it.
//! `crates/vike-analytics/tests/libm_platform_probe.rs` is the measurement that established it and
//! `converted_functions_are_platform_invariant` in that same file is the gate that now holds it.
//!
//! ⚠ `sqrt` is DELIBERATELY excluded and stays `f64::sqrt`: IEEE 754 *does* require it to be
//! correctly rounded, so it is already identical on every platform and routing it through `libm`
//! would move values for nothing.
//!
//! ⚠ **`powi` is NOT in that company, though this paragraph used to put it there** — "it lowers to
//! a multiply chain, not a libm call" is false on MSVC in a `dev` build, where `llvm.powi` becomes
//! the CRT's `pow()`. `sqrt`'s exemption rests on the IEEE 754 STANDARD and holds everywhere;
//! `powi`'s rested on a lowering that one toolchain does not perform. It is banned at every arity
//! by `production_code_calls_libm_not_the_platform`, and the cure is a multiply
//! (`crates/vike-math/src/powers.rs`'s `square` / `cube` / `fourth_power`) rather than `libm`.
//!
//! ⚠ What this COSTS, stated because it is a real trade and not a free win: the `libm` crate does
//! not agree with glibc either (the probe measures 0.07%-10% of samples differing per domain), so
//! this moved values on Linux too — where CI and live trading run. It also means these numbers no
//! longer track CPython's platform libm, which is what the retired Python oracle was computed
//! with. That is a deliberate choice of REPRODUCIBILITY over oracle bit-parity, licensed by
//! `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`.
//!
//! PARITY RULES (inherited, unchanged): f64 end-to-end, same expression order, no mul_add /
//! fast-math; `%` on timestamps is Python floor-mod → `rem_euclid`; `IndexMap` wherever Python
//! dict insertion order affects f64 sum order.

#![warn(unreachable_pub)]

pub mod benchmark;
pub mod book_export;
pub mod equity;
pub mod excursions;
pub mod factor_evaluation;
pub mod forward_test;
pub mod html;
pub mod instrument_graph;
pub mod labels;
pub mod metric_catalog;
pub mod metrics;
pub mod model_drift;
pub mod montecarlo;
pub mod mtm;
pub mod overfit;
pub mod periods;
pub mod permutation;
pub mod realism;
pub mod record;
pub mod results;
pub mod results_comparison;
pub mod results_table;
pub mod sample_weights;
pub mod signal;
pub mod signal_backtest;
pub mod stability;
pub mod stats;
pub mod synthetic;
pub mod trades;
pub mod validation;
pub mod walkforward;
pub mod zero_trade;

pub use realism::{RealismDivergence, RealismStamp};
pub use record::{
    TradingPeriod, TradingRecord, TradingRecordSampling, TradingRecordSimulator,
    TradingRecordSource,
};

// The live tearsheet's vocabulary, at the crate root because that is the ONE spelling every
// consumer uses — the same crate-root set `vike-report` exported before these modules moved here,
// so the move changed the crate name at each call site and nothing else.
pub use equity::{equity_curve_from_samples, equity_curve_from_trades};
pub use html::{
    render_comparison_html, render_html, render_html_with_closed_trades, render_html_with_stats,
};
pub use mtm::{
    MarkToMarketPoint, MarkToMarketRuntimeStatistics, mtm_equity_curve, reconstruct_mtm,
};
pub use results_table::{
    INVALID_LABEL, NOT_RECORDED, TRADING_RESULTS_TABLE_SCHEMA,
    TRADING_RESULTS_TABLE_SCHEMA_UNVERSIONED, TradingResultsTable, TradingResultsTableFillOrder,
    TradingResultsTableLeftOutContract, TradingResultsTableLeftOutLedger,
    TradingResultsTableMetricValues, TradingResultsTableOtherAssetFee,
    trading_results_table_contract_label, trading_results_table_foreign_quote_refusal,
    trading_results_table_from_curve, trading_results_table_from_fills,
    trading_results_table_ledger_label, trading_results_table_left_out_label,
    trading_results_table_other_asset_fee_label, without_fees_in_other_assets,
    without_non_default_contracts, without_non_dollar_ledgers,
};
pub use trades::{
    TradesOpeningSnapshot, opening_snapshot, reconstruct_trades, reconstruct_trades_from,
    trades_opening_refusal, trades_opening_words,
};

#[cfg(test)]
mod test_support;
