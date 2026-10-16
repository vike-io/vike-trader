//! Shared streaming-state helpers — the EMA recurrence reused
//! across the category modules. Each mirrors its `math.rs` batch twin bit-for-bit
//! (the crate-level parity contract). Moved out of the old monolithic
//! `indicators.rs` so every category file can reach them.

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
}
