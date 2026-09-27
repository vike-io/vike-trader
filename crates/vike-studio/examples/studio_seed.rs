//! Seed a deterministic demo bar series into a hist store so the Studio has something to run
//! against on a fresh checkout — for development and headless QA captures (the VIKE_SHOT flow),
//! NOT production data (real history comes from `vike-backfill`).
//!
//! ```sh
//! cargo run -p vike-studio --example studio_seed            # seeds ./market_data/hist
//! VIKE_HIST_STORE=/tmp/demo cargo run -p vike-studio --example studio_seed
//! ```
//!
//! Writes 600 one-minute `binance/BTCUSDT` bars: a slow sine over a gentle drift, so an SMA
//! crossover (the default editor script) actually trades. Deterministic (no RNG, fixed epoch
//! base) and idempotent per run via the store's commit-key append — re-running replaces nothing
//! and duplicates nothing.

use vike_data::{DataFusionHist, HistStore};
use vike_model::Bar;

fn main() {
    let root = std::env::var("VIKE_HIST_STORE").unwrap_or_else(|_| "market_data/hist".to_string());
    let store = DataFusionHist::open(&root).expect("open hist store");
    // Fixed base ts (2026-01-01 00:00 UTC) so re-runs write the identical series.
    let base_ts: i64 = 1_767_225_600_000;
    let bars: Vec<Bar> = (0..600)
        .map(|i| {
            let t = i as f64;
            // ~90-bar sine (amplitude 400) over a +2/bar drift, around 60k — visibly wavy on a
            // chart and rich in SMA(5)/SMA(20) crossings.
            let close = 60_000.0 + 2.0 * t + 400.0 * (t / 14.3).sin();
            let open = 60_000.0 + 2.0 * (t - 1.0) + 400.0 * ((t - 1.0) / 14.3).sin();
            let (high, low) = (open.max(close) + 25.0, open.min(close) - 25.0);
            Bar {
                ts: base_ts + 60_000 * i as i64,
                open,
                high,
                low,
                close,
                volume: 10.0 + (t / 5.0).cos().abs() * 90.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect();
    store
        .append_bars("binance", "BTCUSDT", "1m", &bars, Some("studio-seed:demo:v1"))
        .expect("append demo bars");
    println!("seeded 600 binance/BTCUSDT 1m demo bars into {root}");
}
