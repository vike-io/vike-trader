//! Hand-built `SymbolMap`s the mapper tests descale against, the unit tests in
//! `src/event_mapper_tests.rs` and the offline suite `tests/offline/data_mapper.rs` alike.

use crate::proto::{ProtoOaLightSymbol, ProtoOaSymbol};
use crate::symbols::SymbolMap;

/// EURUSD id=1, digits=5 (scale 1e5) — mirrors the fake server's scripted `SymbolById`.
pub fn eurusd_symbol_map() -> SymbolMap {
    one_symbol_map(1, "EURUSD", 5)
}

/// USDJPY id=4, digits=3 — the live-verified regression fixture. cTrader's relative price scale
/// is a FIXED 1e5 for every symbol regardless of `digits`; a `digits=3` symbol is exactly the case
/// the old (wrong) `symbols.scale(id)` (`10^digits`) descale got 100x wrong.
pub fn usdjpy_symbol_map() -> SymbolMap {
    one_symbol_map(4, "USDJPY", 3)
}

/// The map of the one symbol `name`, id `symbol_id`, at `digits`.
fn one_symbol_map(symbol_id: i64, name: &str, digits: i32) -> SymbolMap {
    let light = vec![ProtoOaLightSymbol {
        symbol_id,
        symbol_name: Some(name.to_string()),
        ..Default::default()
    }];
    let full = vec![ProtoOaSymbol { symbol_id, digits, ..Default::default() }];
    SymbolMap::from_symbols(&light, &full)
}
