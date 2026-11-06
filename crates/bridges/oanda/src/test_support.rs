//! The docs-shaped `GET /v3/accounts/{id}/instruments` rows the instrument-grid tests share: this crate's
//! (`instrument_grid_tests.rs`) and the gatewayhub's facts tests
//! (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts_oanda_grid_tests.rs`), which reach them through a
//! dev edge with `test-support`. They follow OANDA's documentation of the endpoint and of `Instrument`; no
//! practice-account payload has been recorded. Gated `#[cfg(any(test, feature = "test-support"))]`, so no
//! shipped build compiles it.

use serde_json::{Value, json};

/// One docs-shaped `Instrument` of the list: its OANDA `type`, `displayPrecision` and `tradeUnitsPrecision`
/// as the JSON values given (so a garbage one can be a string, negative or absent).
pub fn row(name: &str, kind: &str, display: Option<Value>, units: Value) -> Value {
    let mut row = json!({
        "name": name,
        "type": kind,
        "displayName": name.replace('_', "/"),
        "pipLocation": -4,
        "tradeUnitsPrecision": units,
        "minimumTradeSize": "1",
        "maximumOrderUnits": "100000000",
        "marginRate": "0.03",
    });
    if let Some(display) = display {
        row["displayPrecision"] = display;
    }
    row
}

/// A well-formed `CURRENCY` row priced to `display` decimals, traded in whole units.
pub fn currency(name: &str, display: i64) -> Value {
    row(name, "CURRENCY", Some(json!(display)), json!(0))
}
