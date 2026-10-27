//! The deterministic random-walk bar series the Studio Run tests seed their stores with.

use vike_marketdata::Bar;

/// `n` bars of one deterministic oscillating walk, so an SMA(5/20) cross triggers on it: an LCG
/// seeded `0x1234_5678` steps a price from `100.0`, floored at `1.0`. Bar `i` is stamped
/// `step_ms * (i + 1)` and tagged `symbol`; open = high = low = close, with no volume, funding,
/// bid or ask. Every caller gets bit-identical closes for the same `n`, whatever the step.
///
/// Std and `vike_model` only, no `crate::` path: the `tests/` binaries include this file with
/// `#[path = "../src/test_support/walk.rs"] mod walk;`.
pub fn random_walk_bars(n: usize, step_ms: i64, symbol: Option<&str>) -> Vec<Bar> {
    // deterministic oscillating walk so the cross triggers
    let mut px = 100.0f64;
    let mut seed = 0x1234_5678u64;
    (0..n)
        .map(|i| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            px = (px + ((seed >> 32) as f64 / u32::MAX as f64 - 0.5) * 2.0).max(1.0);
            Bar {
                ts: step_ms * (i as i64 + 1),
                open: px,
                high: px,
                low: px,
                close: px,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: symbol.map(Into::into),
            }
        })
        .collect()
}
