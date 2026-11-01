//! `symbol_grid`'s leg-key tests: a leg is keyed under the exact bytes the runtime routes on.

use super::*;

fn props(tick: f64) -> vike_model::SymbolProperties {
    vike_model::SymbolProperties { tick_size: tick, ..Default::default() }
}

/// ⚠ **The grid key must be the SAME BYTES the runtime routes on.** `vike_core`'s
/// `resolve_intent_symbol` stamps the RAW declared string onto the `OrderRequest` and
/// `RiskLimits::grid_for` looks the map up by it, so a normalised key is a row the gate never
/// hits: the wrong-instrument rounding this map exists to prevent.
///
/// Non-vacuous: with a `trim()` on the key it would be `"ETHUSDT"` and the raw-spelling lookup
/// below would miss.
#[test]
fn a_leg_declared_with_whitespace_is_keyed_under_the_bytes_the_runtime_will_route() {
    let declared = vec![" ETHUSDT".to_string()];
    let grids = declared_symbol_grids("BTCUSDT", &declared, |s| {
        // The venue is asked for whatever was declared; what matters here is the KEY.
        (s == " ETHUSDT").then(|| props(0.05))
    });
    assert!(
        grids.contains_key(" ETHUSDT"),
        "the row must be filed under the declared spelling, not a normalised one: {:?}",
        grids.keys().collect::<Vec<_>>()
    );
}

/// An all-whitespace leg is a declaration error: `trim` still DECIDES, though it forms no key.
#[test]
fn an_all_whitespace_leg_is_dropped_rather_than_gridded() {
    let declared = vec!["   ".to_string()];
    let grids = declared_symbol_grids("BTCUSDT", &declared, |_| Some(props(0.05)));
    assert!(
        grids.is_empty(),
        "a blank leg must not become a row: {:?}",
        grids.keys().collect::<Vec<_>>()
    );
}

/// The mounted symbol is never re-gridded: it IS the scalar limits.
#[test]
fn the_primary_symbol_is_skipped() {
    let declared = vec!["BTCUSDT".to_string()];
    let grids = declared_symbol_grids("BTCUSDT", &declared, |_| Some(props(0.05)));
    assert!(grids.is_empty());
}
