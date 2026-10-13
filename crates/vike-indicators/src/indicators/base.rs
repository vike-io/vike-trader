//! The 17 indicators of the base set — each a struct implementing [`crate::Indicator`].
//! Faithful f64 extraction of vike-trader-app `core/indicators/base.py` (the
//! `c_*(&Columns) -> Vec<Vec<f64>>` batch functions live here as the `batch_*`
//! kernels; `vectorize` is a thin wrapper over them, so it stays the parity
//! reference).
//!
//! TWO PATHS, ONE TRUTH: `vectorize` is the batch reference; `on_bar` streams with
//! bounded per-bar work — no full-series recompute. Nine indicators run a true
//! O(1) recurrence (EMA, RSI, ATR, ROC, OBV, MACD, PSAR, Keltner, VWAP); the eight
//! window statistics (WMA, Bollinger, Donchian, Stochastic, CCI, Williams, and now
//! SMA and Awesome) keep an O(window) fold and reproduce the batch's exact
//! per-window arithmetic.
//!
//! ⚠ SMA and Awesome MOVED into that second group. They were O(1) sliding sums until
//! `math::sma` was de-accumulated: a sliding `sum += c[i] - c[i-n]` carries rounding
//! error from bar 0, which made every consumer's value change when
//! `hist_indicator!` truncated history. Bit-parity with the batch is the contract, so
//! the streaming mirrors had to follow the batch rather than the other way round. Bit-parity forbids the usual O(1) shortcuts on the window
//! stats — a running (Welford) variance would round differently than a two-pass
//! window std, so Bollinger/CCI re-read the window with the computed mean. Every
//! indicator is gated by `tests/parity.rs`: on_bar folded == vectorize, bitwise.

// index loops (not iterators) are deliberate in the `batch_*` kernels: they mirror
// the Python oracle's `for i in range(len)` windows line-for-line, keeping the
// parity mapping obvious (same convention as vike-sim's engine kernels).
#![allow(clippy::needless_range_loop)]

mod incremental;
mod windowed;

pub(crate) use incremental::{Atr, Ema, Macd, Obv, Psar, Roc, Rsi, Sma};
pub(crate) use windowed::{
    Awesome, Bollinger, Cci, Donchian, Keltner, Stochastic, Vwap, Williams, Wma,
};
