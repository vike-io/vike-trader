//! **A backtest of an FX venue's symbol runs on the data stored from it, with no venue session and no login** (owner
//! 2026-11-14: "even when a venue is closed, we must be able to BACKTEST a symbol from that venue on the data stored
//! from it"). The run reads the store and nothing else: this crate links no venue bridge and no gatewayhub (the layer
//! gate fences venue crates off it), so no step of it can log in, whatever the day, and the FX week's login gate
//! (`vike_bridge_core::fx_login`, armed only in the gatewayhub) cannot reach it.
//!
//! Each of the five venues `vike_venues::session::session_for` puts on the FX week, on both lanes: 100,000 units of
//! EURUSD bought at 1.10 and marked at 1.20, fee-free, is exactly 10,000 dollars.

use super::*;
use vike_marketdata::test_support::{flat_bar_zero_volume, quote};
use vike_store::HistoryStoreDataFusion;

/// The venues on the FX week.
const FX: [&str; 5] = ["ctrader", "dukascopy", "fxcm", "ig", "oanda"];

fn profile(venue: &str, kind: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!(
        r#"
[data]
venue = "{venue}"
symbols = ["EURUSD"]
kind = "{kind}"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000000.0
[engine.fee]
kind = "free"

[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = 100000.0
symbol = "EURUSD"
"#
    ))
    .unwrap()
    .with_stand_in(vike_strategies_check::stand_in)
}

/// A store holding `venue`'s EURUSD as three daily bars and three quotes at 1.10, 1.10 and 1.20.
fn store_of(venue: &str) -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let px = [1.10, 1.10, 1.20];
    let bars: Vec<_> = (0..3).map(|i| flat_bar_zero_volume(i * 1000, px[i as usize])).collect();
    store.append_bars(venue, "EURUSD", "1d", &bars, None).unwrap();
    let quotes: Vec<_> = (0..3).map(|i| quote(i * 1000, px[i as usize], px[i as usize])).collect();
    store.append_quotes(venue, "EURUSD", &quotes, None).unwrap();
    (dir, store)
}

#[test]
fn a_backtest_of_each_fx_venue_runs_on_its_stored_bars_and_ticks() {
    for venue in FX {
        for kind in ["bar", "tick"] {
            let (_dir, store) = store_of(venue);
            let result = run_backtest(&profile(venue, kind), store).unwrap_or_else(|e| {
                panic!("{venue}/{kind}: a stored-data backtest must run: {e:?}")
            });
            let pnl = result.final_equity - 1_000_000.0;
            assert!((pnl - 10_000.0).abs() < 1e-6, "{venue}/{kind}: 100,000 x 0.10, booked {pnl}");
        }
    }
}
