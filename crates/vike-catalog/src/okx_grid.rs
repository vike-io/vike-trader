//! **An okx contract's grid restated in BASE units**: the ONE contracts-to-base conversion (audit flag OKX-3), so a
//! process that links no bridge (the datahub's properties tape) scales a SWAP or FUTURES row exactly as the bridge's
//! mounted path (`vike_okx::adapter`'s `mount_parts`) and its symbol facts (`vike_okx::symbol_properties`'
//! `okx_in_base`) do.
//!
//! okx fills `step_size`/`min_qty`/`max_qty` from `lotSz`/`minSz`/`maxMktSz`, which are counts of CONTRACTS, each
//! `contract_size` of the base coin (`ctVal`, or the catalog row's `ctVal x ctMult`). The wire wants those raw numbers
//! (`to_contracts` divides a BASE qty by `ctVal` and floors on that contracts grid), but `OrderRequest.qty`, the
//! pre-trade gate and a backtest's fill are BASE: handed the raw grid, the gate floors a base quantity on a contracts
//! step — for BTC-USDT-SWAP (`ctVal` 0.01) 100x too coarse, so a legitimate 0.015 BTC order floors to 0.01.

use vike_model::SymbolProperties;

/// **`properties` (an okx contracts grid) in BASE units**: the three QUANTITY fields times `contract_size`, and
/// `contract_size` 0 (absent) because the scaling is done and a second multiplier would scale a notional twice.
/// ⚠ `tick_size` is quote per base and `min_notional` is quote: both stay, since scaling either would be the same bug
/// in the other direction. A non-finite or non-positive `contract_size` returns `properties` unchanged rather than a
/// grid silently zeroed or made infinite.
#[must_use]
pub fn okx_grid_in_base(properties: &SymbolProperties, contract_size: f64) -> SymbolProperties {
    if !contract_size.is_finite() || contract_size <= 0.0 {
        return *properties;
    }
    SymbolProperties {
        step_size: properties.step_size * contract_size,
        min_qty: properties.min_qty * contract_size,
        max_qty: properties.max_qty * contract_size,
        contract_size: 0.0,
        ..*properties
    }
}

#[path = "okx_grid_tests.rs"]
#[cfg(test)]
mod okx_grid_tests;
