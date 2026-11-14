//! `rotation_top_k` through its SHIPPED profile, `profiles/rotation_top_k_multi.toml`, end to end
//! through `run_backtest`: the profile parses, its five symbols load, and the strategy trades.
//!
//! The profile names 24 months of Binance 1m bars, which no test box holds, so the tape here is a
//! synthetic one over the profile's own venue, symbols, interval and start: [`BARS`] aligned 1m bars
//! per symbol, a deterministic walk each. Everything else is the profile's (its cash, fee, slippage
//! and strategy).
//!
//! It is also the DUMP the strategy's byte identity is compared by when its code moves (the move onto
//! the order seam, 2026-11-12): run it with `--nocapture` on the base and on the branch, and the
//! `dump-digest` line must be equal. The dump is every equity point, every closed trade and every
//! symbol's PnL, each `f64` as its hex bits.

use std::path::PathBuf;
use std::sync::Arc;

use vike_backtest::harness::{ProfileBacktest, run_backtest};
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_marketdata::Bar;
use vike_model::f64_to_hex_bits;

/// Bars per symbol: 300 rebalances at the strategy's every-10-bars cadence.
const BARS: usize = 3_000;
const STEP_MS: i64 = 60_000;

fn profile() -> ProfileBacktest {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("profiles/rotation_top_k_multi.toml");
    ProfileBacktest::from_path(&path)
        .expect("the shipped rotation profile parses")
        .with_stand_in(vike_strategies_check::stand_in)
}

/// [`BARS`] aligned 1m bars for every symbol of `p`, from its `data.from`: a multiplicative walk of
/// at most ±0.3% a bar, from a per-symbol xorshift seed, so the ranking changes hands.
fn store(p: &ProfileBacktest) -> Arc<dyn HistoryStore + Send + Sync> {
    let t0: i64 = p.data.from.parse().expect("the profile's start is epoch milliseconds");
    let venue = p.data.venue.as_deref().expect("the profile names its venue");
    let store = HistoryStoreMemory::new();
    for (k, symbol) in p.data.symbols.iter().enumerate() {
        let mut seed: u64 =
            0x9E37_79B9_7F4A_7C15 ^ (k as u64 + 1).wrapping_mul(0xD1B5_4A32_D192_ED03);
        let mut close = 100.0 * (k as f64 + 1.0);
        let bars: Vec<Bar> = (0..BARS)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let open = close;
                close = open * (1.0 + ((seed % 601) as f64 - 300.0) * 1e-5);
                Bar {
                    timestamp: t0 + i as i64 * STEP_MS,
                    open,
                    high: open.max(close),
                    low: open.min(close),
                    close,
                    volume: 1_000.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: None,
                }
            })
            .collect();
        store.append_bars(venue, symbol, "1m", &bars, None).expect("the double stores bars");
    }
    Arc::new(store)
}

/// FNV-1a over the dump's text: one line to compare between two trees.
fn digest(text: &str) -> u64 {
    text.bytes()
        .fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

#[test]
fn the_shipped_rotation_profile_trades_its_universe() {
    let p = profile();
    let result = run_backtest(&p, store(&p)).expect("the rotation profile runs");
    assert_eq!(result.equity_curve.len(), BARS, "one equity point per step");
    let traded = result.per_symbol_pnl.iter().filter(|(_, pnl)| *pnl != 0.0).count();
    assert!(traded >= 2, "it holds two winners at a time: {:?}", result.per_symbol_pnl);

    let mut dump = format!("final_equity {}\n", f64_to_hex_bits(result.final_equity));
    for (i, (eq, ts)) in result.equity_curve.iter().zip(&result.equity_timestamp).enumerate() {
        dump.push_str(&format!("equity {i} {ts} {}\n", f64_to_hex_bits(*eq)));
    }
    for t in &result.trades {
        dump.push_str(&format!(
            "trade {} {} {} {} {} {} {} {}\n",
            t.symbol,
            t.entry_timestamp,
            t.exit_timestamp,
            f64_to_hex_bits(t.entry_price),
            f64_to_hex_bits(t.exit_price),
            f64_to_hex_bits(t.size),
            f64_to_hex_bits(t.pnl),
            f64_to_hex_bits(t.fees),
        ));
    }
    for (symbol, pnl) in &result.per_symbol_pnl {
        dump.push_str(&format!("pnl {symbol} {}\n", f64_to_hex_bits(*pnl)));
    }
    println!("dump-trades {} dump-digest {:016x}", result.trades.len(), digest(&dump));
}
