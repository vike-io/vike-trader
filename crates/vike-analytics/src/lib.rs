//! `vike-analytics` — the pure performance-analytics cluster, extracted verbatim out of
//! `vike-backtest`.
//!
//! # Why this crate exists
//!
//! `vike-report` (the LIVE tearsheet reader) reimplements no metric: every number it prints is a
//! call into this catalog. But the catalog used to live inside `vike-backtest`, so vike-report
//! compiled **32,259 lines to reach ~3,900** — 89% of what it built it could not call, including
//! the simulator (`engine`/`sim_broker`), the paper exchange, and the whole gated `harness/` tree.
//! Splitting on the seam that was already there (these modules name `vike_model` and **nothing
//! else** — the only `vike_exec` mention in the whole cluster was a doc comment in
//! [`result`]) lets a consumer take the numbers without the machine that produced them.
//!
//! # The layering rule this crate holds
//!
//! **`vike-model` is the ONLY vike dependency.** No `vike-exec`, no `vike-core`, no `vike-data`.
//! That is the property the split exists to create, and adding any of the three would silently
//! undo it — a would-be dependency is a signal that the item belongs in `vike-backtest`, not here.
//!
//! # Contents
//!
//! - [`metrics`] — the metrics catalog (sharpe/sortino/drawdown/CAGR/VaR/ES/SQN/…), the ONE home
//!   for every performance number; `vike-report` and `vike-backtest` both call it, so a live and a
//!   backtest tearsheet cannot drift.
//! - [`metric_catalog`] — the metric TAXONOMY as data: one row per number this tree can answer,
//!   plus the rows it deliberately cannot and why. The declaration order IS the render order, so a
//!   consumer asks this module for the ordered ids rather than keeping the hand copy that three
//!   consumers had each grown their own of.
//! - [`result`] / [`report`] — [`BacktestResult`] (the run's raw output) and [`BacktestReport`]
//!   (the flat, `Serialize` summary composed from it by `metrics::` calls only), now carrying the
//!   long-form [`ExtendedMetrics`] catalog and the [`HonestyCounters`] that tell an AMBIGUOUS run
//!   apart from a clean one.
//! - [`realism`] — [`RealismStamp`], the cost model a run actually ran under. Two results are
//!   comparable only if they were priced the same way, and a report used to say nothing at all
//!   about which market produced it.
//! - [`benchmark`] / [`excursions`] / [`periods`] / [`montecarlo`] / [`stability`] / [`overfit`] /
//!   [`validation`] — the analytics port: benchmark-relative stats, MAE/MFE, calendar bucketing,
//!   resampling, rolling stability, PBO/deflated-Sharpe, and the walk-forward/purged-k-fold split
//!   generators.
//! - [`sizing`] — the `PositionSizer` registry (the eight sizers).
//! - [`zero_trade`] — the zero/near-zero-trade cause analyzer over already-collected diagnostics.
//! - [`binutil`] — the pure argv parsers the family's bins share (see its module doc for why
//!   `store_root`, the one env-reading function, deliberately stayed in `vike-backtest`).
//! - [`tearsheet`] / [`html`] / [`trades`] / [`equity`] / [`mtm`] — the LIVE TEARSHEET's pure
//!   core, moved here out of `vike-report` on 2026-09-28: the [`LiveTearsheet`] document keyed on
//!   the catalog, its self-contained HTML renderer ([`render_html`]), the fill → closed-trade fold
//!   ([`reconstruct_trades`]), the two equity-curve builders and the mark-to-market fold with its
//!   [`RuntimeStats`]. None of it reads a journal or a store — every input is a `vike_model` value
//!   the caller already holds — which is what lets it live under this crate's layering rule and
//!   what lets a consumer that only RENDERS (`vike-cli`'s `backtest show --html`) name it without
//!   linking a journal reader. The readers stayed in `vike-report`: `tearsheet_from_journal` (the
//!   journal door, a free function because an inherent `impl` on a type this crate owns cannot be
//!   written there), `equity_curve_from_store` and `mtm_curve_from_store`.
//!
//! # Compatibility
//!
//! This extraction moved code, not behaviour. Every caller names this crate directly —
//! `vike_analytics::metrics::…`, `vike_analytics::BacktestResult`, `vike_analytics::report::…`.
//! The tearsheet half is named at the CRATE ROOT only (`vike_analytics::LiveTearsheet`,
//! `vike_analytics::render_html`, …): that is the vocabulary `vike-report` exported it under, and
//! every consumer was re-pointed to it in the move rather than to the module paths, so each of
//! those items has one spelling outside this crate.
//! ⚠ `vike-backtest` RE-EXPORTED eight of these modules and `BacktestResult` at its own crate root
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
//! (`crates/vike-indicators/src/math.rs`'s `sq` / `cube` / `quart`) rather than `libm`.
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

pub mod benchmark;
pub mod binutil; // shared bin glue: the pure argv parsers (store_root stays in vike-backtest)
pub mod equity;
pub mod excursions;
pub mod html;
pub mod metric_catalog;
pub mod metrics;
pub mod montecarlo;
pub mod mtm;
pub mod overfit;
pub mod periods;
pub mod realism;
pub mod report;
pub mod result;
pub mod signal;
pub mod signal_backtest;
pub mod sizing;
pub mod stability;
pub mod stats;
pub mod tearsheet;
pub mod trades;
pub mod validation;
pub mod zero_trade;

// The selection vocabulary an operator's `--metrics` value parses into, at the crate root because
// every door onto a report needs it and none of them should reach into the module for it.
pub use metric_catalog::{
    MetricHome, MetricSelection, MetricSpec, MetricUnit, metric_list_text, parse_metric_selection,
};
pub use realism::{RealismDivergence, RealismStamp};
pub use report::{
    BacktestReport, DAILY_PERIODS_PER_YEAR, ExtendedMetrics, HonestyCounters,
    periods_per_year_for_interval,
};
pub use result::BacktestResult;
pub use zero_trade::{
    ZeroTradeCause, ZeroTradeInputs, ZeroTradeReport, aggregate_denials, rank_causes,
};

// The live tearsheet's vocabulary, at the crate root because that is the ONE spelling every
// consumer uses — the same crate-root set `vike-report` exported before these modules moved here,
// so the move changed the crate name at each call site and nothing else.
pub use equity::{equity_curve_from_samples, equity_curve_from_trades};
pub use html::{render_html, render_html_with_stats};
pub use mtm::{MtmPoint, RuntimeStats, mtm_equity_curve, reconstruct_mtm};
pub use tearsheet::{
    LiveTearsheet, MetricValues, NOT_RECORDED, TEARSHEET_SCHEMA, TEARSHEET_SCHEMA_UNVERSIONED,
};
pub use trades::reconstruct_trades;

#[cfg(test)]
mod test_support;
