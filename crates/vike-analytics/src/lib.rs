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
//! - [`result`] / [`report`] — [`BacktestResult`] (the run's raw output) and [`BacktestReport`]
//!   (the flat, `Serialize` summary composed from it by `metrics::` calls only).
//! - [`benchmark`] / [`excursions`] / [`periods`] / [`montecarlo`] / [`stability`] / [`overfit`] /
//!   [`validation`] — the analytics port: benchmark-relative stats, MAE/MFE, calendar bucketing,
//!   resampling, rolling stability, PBO/deflated-Sharpe, and the walk-forward/purged-k-fold split
//!   generators.
//! - [`sizing`] — the `PositionSizer` registry (the eight sizers).
//! - [`zero_trade`] — the zero/near-zero-trade cause analyzer over already-collected diagnostics.
//! - [`binutil`] — the pure argv parsers the family's bins share (see its module doc for why
//!   `store_root`, the one env-reading function, deliberately stayed in `vike-backtest`).
//!
//! # Compatibility
//!
//! `vike-backtest` re-exports every module below at its own crate root, so every existing
//! `vike_backtest::metrics::…` / `vike_backtest::BacktestResult` / `crate::report::…` path
//! resolves exactly as before. This extraction moved code, not behaviour.
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
pub mod excursions;
pub mod metrics;
pub mod montecarlo;
pub mod overfit;
pub mod periods;
pub mod report;
pub mod result;
pub mod signal;
pub mod signal_backtest;
pub mod sizing;
pub mod stability;
pub mod stats;
pub mod validation;
pub mod zero_trade;

pub use report::{BacktestReport, DAILY_PERIODS_PER_YEAR, DEFAULT_PERIODS_PER_YEAR};
pub use result::BacktestResult;
pub use zero_trade::{
    ZeroTradeCause, ZeroTradeInputs, ZeroTradeReport, aggregate_denials, rank_causes,
};
