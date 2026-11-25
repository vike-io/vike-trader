//! The synthetic hourly market `pairs_reversion`'s tests run on, built from a fixed seed so every run and every
//! process sees the same bars.
//!
//! Six symbols (the names are the fixture's own). `ALPHA` and `BRAVO` share a stochastic trend: `BRAVO` is
//! `1.2 · ALPHA` plus a stationary remainder that reverts with a half-life near ten bars (a first-order
//! autoregression, coefficient 0.93), so the pair is cointegrated with a hedge ratio near 1.2 (0.83 when `ALPHA` is
//! regressed on `BRAVO`). `CHARLIE` and `DELTA` are the same with `0.8` and their own trend. `ECHO` and `FOXTROT`
//! each follow a trend of their own, so no combination of them is stationary. A small market factor moves all six.
//! `ECHO` has three bars missing, so the study's alignment is exercised.
//!
//! Each of the 1000 bars is flat (open, high, low and close one price): the study reads closes.

use std::sync::Arc;

use vike_data::{HistoryStore, HistoryStoreMemory, TimestampRange};
use vike_marketdata::Bar;

// The one fixture noise both fixture studies include: a test binary compiling both loads the file twice, on
// purpose (one copy of the LCG, VIKE-MATH §5.3).
#[expect(clippy::duplicate_mod)]
#[path = "../study_fixture/noise.rs"]
mod noise;
use noise::Noise;

pub const VENUE: &str = "fixture";
pub const SYMBOLS: [&str; 6] = ["ALPHA", "BRAVO", "CHARLIE", "DELTA", "ECHO", "FOXTROT"];
pub const HOUR: i64 = 3_600_000;
pub const BARS: usize = 1000;
/// The bars `ECHO` lacks.
pub const GAP: std::ops::Range<usize> = 100..103;

/// The two kinds of series this market is built of, drawn from the shared noise.
impl Noise {
    /// A random walk of `BARS` steps of standard deviation `step`.
    fn walk(&mut self, step: f64) -> Vec<f64> {
        let mut level = 0.0;
        (0..BARS)
            .map(|_| {
                level += step * self.normal();
                level
            })
            .collect()
    }

    /// A first-order autoregression with coefficient `phi` and innovations of standard deviation `step`.
    fn autoregression(&mut self, phi: f64, step: f64) -> Vec<f64> {
        let mut level = 0.0;
        (0..BARS)
            .map(|_| {
                level = phi * level + step * self.normal();
                level
            })
            .collect()
    }
}

/// The store and the window that covers its bars.
pub fn store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
    let mut noise = Noise(0x5EED_0001);
    let market = noise.walk(0.004);
    let (first, second, third, fourth) =
        (noise.walk(0.01), noise.walk(0.01), noise.walk(0.01), noise.walk(0.01));
    let (first_remainder, second_remainder) =
        (noise.autoregression(0.93, 0.003), noise.autoregression(0.93, 0.003));
    let logs: [Vec<f64>; 6] = [
        (0..BARS).map(|t| 4.6 + market[t] + first[t]).collect(),
        (0..BARS).map(|t| 4.2 + 1.2 * (market[t] + first[t]) + first_remainder[t]).collect(),
        (0..BARS).map(|t| 5.0 + market[t] + second[t]).collect(),
        (0..BARS).map(|t| 3.9 + 0.8 * (market[t] + second[t]) + second_remainder[t]).collect(),
        (0..BARS).map(|t| 4.4 + market[t] + third[t]).collect(),
        (0..BARS).map(|t| 4.0 + market[t] + fourth[t]).collect(),
    ];
    let store = HistoryStoreMemory::new();
    for (symbol, log) in SYMBOLS.iter().zip(&logs) {
        let bars: Vec<Bar> = log
            .iter()
            .enumerate()
            .filter(|(t, _)| !(*symbol == "ECHO" && GAP.contains(t)))
            .map(|(t, l)| {
                let price = libm::exp(*l);
                Bar::ohlcv(t as i64 * HOUR, price, price, price, price, 1.0)
            })
            .collect();
        // One commit key per append: a key the store has seen makes the append a no-op.
        store.append_bars(VENUE, symbol, "1h", &bars, Some(&format!("fixture-{symbol}"))).unwrap();
    }
    (Arc::new(store), TimestampRange::of(0, BARS as i64 * HOUR))
}
