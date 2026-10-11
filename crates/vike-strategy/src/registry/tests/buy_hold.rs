//! `BuyHold`'s reader and the declared keys that really move a resolved strategy.

use super::*;
use crate::registry::keys::unknown_params;

/// Every declared key must be one the reader ACCEPTS — proven behaviourally for the two arms
/// whose fields are readable from this crate, so the table is not only text-checked by
/// `tests/param_keys_gate.rs` but observed to move something.
#[test]
fn a_declared_key_actually_moves_the_strategy() {
    let params: Value = toml::from_str("size = 7.5\n").unwrap();
    assert_eq!(BuyHold::from_params(&params).size, 7.5);
    assert!(unknown_params("buy_hold", &params).is_empty());

    let g: Value = toml::from_str("band = 9.0\n").unwrap();
    assert_eq!(Grid::from_params(&g).band, 9.0);
    assert!(unknown_params("grid", &g).is_empty());
}

#[test]
fn buy_hold_from_params_reads_size_and_symbol() {
    let params: Value = toml::from_str("size = 2.5\nsymbol = \"ETHUSDT\"\n").unwrap();
    let strat = BuyHold::from_params(&params);
    assert_eq!(strat.size, 2.5);
    assert_eq!(strat.symbol.as_deref(), Some("ETHUSDT"));
}

#[test]
fn buy_hold_from_params_defaults_size_to_one_and_symbol_to_none() {
    let strat = BuyHold::from_params(&empty());
    assert_eq!(strat.size, 1.0);
    assert_eq!(strat.symbol, None);
}

#[test]
fn buy_hold_from_params_accepts_integer_size() {
    let params: Value = toml::from_str("size = 3\n").unwrap();
    assert_eq!(BuyHold::from_params(&params).size, 3.0);
}

#[test]
fn params_reach_the_resolved_strategy() {
    // The concern the per-arm resolve tests in vike-backtest exist for: a typo'd knob silently
    // falling back to a default. Proven here for the arms whose reader is in THIS crate.
    let g: Value = toml::from_str("step = 0.5\nrungs = 4\nsize = 2.0\nband = 3.0\n").unwrap();
    assert!(resolve("grid", &g).is_ok());
    let grid = Grid::from_params(&g);
    assert_eq!(grid.step, 0.5);
    assert_eq!(grid.rungs, 4);

    let m: Value = toml::from_str("qty = 5.0\ntick_size = 0.01\ngamma = 0.2\n").unwrap();
    assert!(resolve("spread_maker", &m).is_ok());
    assert_eq!(SpreadMaker::from_params(&m).unwrap().params().qty, 5.0);
}
