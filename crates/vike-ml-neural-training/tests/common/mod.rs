//! The seeded values every integration test builds its rows from.

use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};

/// `count` values uniform in `[-1, 1)` from `seed` (24 bits of each draw: exact in an `f32`).
pub fn uniform(seed: u64, count: usize) -> Vec<f32> {
    let mut stream = ChaCha8Rng::seed_from_u64(seed);
    (0..count).map(|_| (stream.next_u32() >> 8) as f32 / (1u32 << 23) as f32 - 1.0).collect()
}

/// `count` returns of a MOMENTUM process, `r[t] = 0.5 * r[t - 1] + u[t]` with `u` from [`uniform`]
/// (`seed`): each return predicts the sign of the next, so a position can earn a positive Sharpe
/// ratio, and a network trained on the Sharpe objective has something to find.
#[allow(dead_code, clippy::allow_attributes)]
pub fn momentum_returns(seed: u64, count: usize) -> Vec<f32> {
    let mut previous = 0.0;
    uniform(seed, count)
        .into_iter()
        .map(|innovation| {
            previous = 0.5 * previous + innovation;
            previous
        })
        .collect()
}

/// Per row, in time order: the `steps` returns ending at that row, oldest first (an LSTM's
/// step-major row and a TCN's one channel, both with one feature), and the NEXT return as the label.
/// No row sees its own label.
#[allow(dead_code, clippy::allow_attributes)]
pub fn windows_and_next(returns: &[f32], steps: usize) -> (Vec<f32>, Vec<f32>) {
    let mut windows = Vec::new();
    let mut next = Vec::new();
    for end in steps..returns.len() {
        windows.extend_from_slice(&returns[end - steps..end]);
        next.push(returns[end]);
    }
    (windows, next)
}

/// The per-period (not annualised) Sharpe ratio of holding `positions` over `next` returns: the
/// workspace's one Sharpe (`vike_analytics::signal_backtest::sharpe_from_bar_pnl`).
#[allow(dead_code, clippy::allow_attributes)]
pub fn sharpe_of(positions: &[f32], next: &[f32]) -> f64 {
    let pnl: Vec<f64> =
        positions.iter().zip(next).map(|(&p, &r)| f64::from(p) * f64::from(r)).collect();
    vike_analytics::signal_backtest::sharpe_from_bar_pnl(&pnl, 1.0)
}
