use super::*;

fn props(tick: f64) -> vike_model::SymbolProperties {
    vike_model::SymbolProperties { tick_size: tick, ..Default::default() }
}

/// ⚠ **The grid key must be the SAME BYTES the runtime routes on.**
///
/// `vike_core`'s `resolve_intent_symbol` matches a declared leg with `l.symbol == r` and stamps
/// the RAW declared string onto the `OrderRequest`; `RiskLimits::grid_for` then looks the map up
/// by that string. So a key normalised on the way in is a row the gate can never hit — the
/// wrong-instrument rounding this map exists to prevent, wearing a row that says it was
/// resolved. This asserts the two rules agree on the one input where they could differ.
///
/// Non-vacuous: with the `trim()` this file used to apply, the key would be `"ETHUSDT"` and the
/// lookup below — by the raw declared spelling — would miss.
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

/// An all-whitespace leg is a declaration error, not an instrument — `trim` still DECIDES even
/// though it no longer forms the key.
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

/// The mounted symbol is never re-gridded — it already IS the scalar limits.
#[test]
fn the_primary_symbol_is_skipped() {
    let declared = vec!["BTCUSDT".to_string()];
    let grids = declared_symbol_grids("BTCUSDT", &declared, |_| Some(props(0.05)));
    assert!(grids.is_empty());
}
