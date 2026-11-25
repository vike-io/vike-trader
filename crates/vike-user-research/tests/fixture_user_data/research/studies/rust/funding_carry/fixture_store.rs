//! The synthetic perpetual market `funding_carry`'s tests run on, built from a fixed seed so every run and every
//! process sees the same bars.
//!
//! A spot price that walks, and a perpetual that trades above it by a BASIS that reverts to a positive mean
//! (a first-order autoregression, coefficient 0.9, around 8 basis points). Every eight hours, a few
//! milliseconds after the hour as a venue stamps it, the perpetual pays a funding rate that follows the basis
//! of the bar (one fifth of it, plus noise), so it is positive most of the time and sometimes negative. Spot and perpetual bars are hourly and flat (one price); the funding series is the
//! perpetual's, interval `funding`, one row per settlement with its rate.

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
pub const SPOT: &str = "COIN";
pub const PERPETUAL: &str = "COIN_PERP";
pub const HOUR: i64 = 3_600_000;
pub const BARS: usize = 1200;
/// Bars between settlements, and how many milliseconds after the hour a settlement is stamped.
pub const SETTLEMENT_BARS: usize = 8;
pub const SETTLEMENT_DELAY_MS: i64 = 5;

/// The store and the window that covers its bars.
pub fn store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
    let mut noise = Noise(0x5EED_0002);
    let store = HistoryStoreMemory::new();
    let (mut spot_log, mut basis) = (4.6, 0.0008);
    let (mut spot, mut perpetual, mut funding) = (Vec::new(), Vec::new(), Vec::new());
    for t in 0..BARS {
        spot_log += 0.008 * noise.normal();
        basis = 0.0008 + 0.9 * (basis - 0.0008) + 0.0004 * noise.normal();
        let (spot_price, perpetual_price) = (libm::exp(spot_log), libm::exp(spot_log + basis));
        let at = t as i64 * HOUR;
        spot.push(Bar::ohlcv(at, spot_price, spot_price, spot_price, spot_price, 1.0));
        perpetual.push(Bar::ohlcv(
            at,
            perpetual_price,
            perpetual_price,
            perpetual_price,
            perpetual_price,
            1.0,
        ));
        if t > 0 && t % SETTLEMENT_BARS == 0 {
            let rate = 0.2 * basis + 0.00005 * noise.normal();
            let row = Bar::ohlcv(at + SETTLEMENT_DELAY_MS, 0.0, 0.0, 0.0, 0.0, 0.0);
            funding.push(Bar { funding: Some(rate), ..row });
        }
    }
    // One commit key per append: a key the store has seen makes the append a no-op.
    store.append_bars(VENUE, SPOT, "1h", &spot, Some("fixture-spot")).unwrap();
    store.append_bars(VENUE, PERPETUAL, "1h", &perpetual, Some("fixture-perpetual")).unwrap();
    store.append_bars(VENUE, PERPETUAL, "funding", &funding, Some("fixture-funding")).unwrap();
    (Arc::new(store), TimestampRange::of(0, BARS as i64 * HOUR))
}
