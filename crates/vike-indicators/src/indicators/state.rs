//! Shared streaming-state helpers — the EMA/Wilder(RMA)/SMA recurrences reused
//! across the category modules. Each mirrors its `math.rs` batch twin bit-for-bit
//! (the crate-level parity contract). Moved out of the old monolithic
//! `indicators.rs` so every category file can reach them.

use std::collections::VecDeque;

/// Streaming EMA: mirrors `math::ema` bit-for-bit. `push` accumulates a naive
/// left-fold seed over the first `n` inputs, emits the SMA seed at the `n`-th,
/// then the alpha recurrence. Returns `None` while warming up.
#[derive(Clone)]
pub(crate) struct EmaState {
    n: usize,
    alpha: f64,
    count: usize,
    seed_sum: f64,
    prev: f64,
}
impl EmaState {
    pub(crate) fn new(n: usize) -> Self {
        Self { n, alpha: 2.0 / (n as f64 + 1.0), count: 0, seed_sum: 0.0, prev: f64::NAN }
    }
    pub(crate) fn push(&mut self, x: f64) -> Option<f64> {
        self.count += 1;
        if self.count < self.n {
            self.seed_sum += x;
            None
        } else if self.count == self.n {
            self.seed_sum += x;
            self.prev = self.seed_sum / self.n as f64;
            Some(self.prev)
        } else {
            self.prev = self.alpha * x + (1.0 - self.alpha) * self.prev;
            Some(self.prev)
        }
    }
    pub(crate) fn reset(&mut self) {
        self.count = 0;
        self.seed_sum = 0.0;
        self.prev = f64::NAN;
    }
}

/// Streaming Wilder RMA: mirrors `math::rma` (via `math::atr`) bit-for-bit.
#[derive(Clone)]
pub(crate) struct RmaState {
    n: usize,
    count: usize,
    seed_sum: f64,
    prev: f64,
}
impl RmaState {
    pub(crate) fn new(n: usize) -> Self {
        Self { n, count: 0, seed_sum: 0.0, prev: f64::NAN }
    }
    pub(crate) fn push(&mut self, x: f64) -> Option<f64> {
        self.count += 1;
        if self.count < self.n {
            self.seed_sum += x;
            None
        } else if self.count == self.n {
            self.seed_sum += x;
            self.prev = self.seed_sum / self.n as f64;
            Some(self.prev)
        } else {
            self.prev = (self.prev * (self.n as f64 - 1.0) + x) / self.n as f64;
            Some(self.prev)
        }
    }
    pub(crate) fn reset(&mut self) {
        self.count = 0;
        self.seed_sum = 0.0;
        self.prev = f64::NAN;
    }
}

/// Streaming SMA over the last `n` inputs — mirrors `math::sma` bit-for-bit: a naive left fold
/// over the retained window, recomputed on each push. Returns `None` while warming up.
///
/// ⚠ **This carried `sum` and updated it with `sum += x - x_evicted`; it moved to a per-window
/// fold in lockstep with `math::sma`**, which is where the reason lives (a carried sum makes every
/// kernel built on it non-truncation-invariant). The two are ONE change and cannot land
/// separately — `parity.rs`'s `on_bar_equals_vectorize_for_every_indicator` compares this against
/// `math::sma` bit-for-bit, so a lagging mirror fails loudly and immediately.
///
/// The fold runs over `make_contiguous()` rather than the ring iterator deliberately: `math::sma`
/// folds a SLICE, so folding a slice here makes the two the same code path by CONSTRUCTION rather
/// than by an argument about `VecDeque::Iter`'s specialised `fold`.
///
/// O(n) per push instead of O(1). The only user is `base.rs`'s `Awesome` — whose `n` comes from
/// `crates/vike-indicators/src/registry.rs`'s `AWESOME_PARAMS`, range 1..=1000 with defaults 5 and
/// 34, NOT the defaults alone: measured 6.79 ns/push at n = 34 against 412.70 ns at n = 1000. And
/// `Awesome` never trims, so no retention policy bounds it.
#[derive(Clone)]
pub(crate) struct SmaAcc {
    n: usize,
    window: VecDeque<f64>,
}
impl SmaAcc {
    pub(crate) fn new(n: usize) -> Self {
        Self { n, window: VecDeque::new() }
    }
    pub(crate) fn push(&mut self, x: f64) -> Option<f64> {
        self.window.push_back(x);
        if self.window.len() > self.n {
            self.window.pop_front();
        }
        // ⚠ `n == 0` used to reach `pop_front().unwrap()` on an empty deque and PANIC. `math::sma`
        // returns all-NaN for it, so `None` — which every caller maps to NaN — is the answer that
        // AGREES with the batch path: a degenerate param now returns the same value on both sides
        // instead of killing the streaming one.
        if self.n > 0 && self.window.len() == self.n {
            Some(self.window.make_contiguous().iter().sum::<f64>() / self.n as f64)
        } else {
            None
        }
    }
    pub(crate) fn reset(&mut self) {
        self.window.clear();
    }
}
