//! okx's list spec on catalog rows parsed by the bridge's own `catalog::parse_instruments`: the grid it answers for a linear
//! USDT SWAP or dated future is the mounted path's (`vike_catalog::okx_grid_in_base` of the exec row), bit for bit.
//! Moved here with the function from the gatewayhub's `symbol_facts_tests.rs`; the cache-driven okx tests stay there.

use serde_json::{Value, json};

use super::*;

const WORDS: SymbolPropertiesWords = SymbolPropertiesWords {
    not_answered: "not answered",
    no_instrument: ": none ",
    expired: "gone",
};

fn okx_row(
    instrument: &str,
    inst_type: &str,
    contract: (&str, &str),
    grid: (&str, &str, &str, &str),
) -> Value {
    let (ct_val, ct_val_ccy) = contract;
    let (tick, lot, min, max) = grid;
    let uly = instrument.split('-').take(2).collect::<Vec<_>>().join("-");
    let expiry = (vike_model::now_ms() + 90 * 86_400_000).to_string();
    json!({
        "instType": inst_type, "instId": instrument, "uly": uly, "instFamily": uly,
        "baseCcy": "", "quoteCcy": "", "settleCcy": "USDT",
        "ctVal": ct_val, "ctMult": "1", "ctValCcy": ct_val_ccy, "ctType": "linear",
        // a future's delivery, 90 days from the test's clock: far outside the admission's last-day fence
        "expTime": if inst_type == "FUTURES" { expiry } else { String::new() },
        "alias": "", "lever": "50", "tickSz": tick, "lotSz": lot, "minSz": min, "maxMktSz": max,
        "state": "live",
    })
}

/// Three linear USDT SWAPs of different contract sizes and a dated USDT future.
fn okx_payload() -> Value {
    json!({ "code": "0", "msg": "", "data": [
        okx_row("BTC-USDT-SWAP", "SWAP", ("0.01", "BTC"), ("0.1", "0.01", "0.01", "35000")),
        okx_row("ETH-USDT-SWAP", "SWAP", ("0.1", "ETH"), ("0.01", "0.01", "0.01", "15000")),
        okx_row("DOGE-USDT-SWAP", "SWAP", ("1000", "DOGE"), ("0.00001", "1", "1", "100000")),
        okx_row("BTC-USDT-261225", "FUTURES", ("0.01", "BTC"), ("0.1", "0.01", "0.01", "5000")),
    ]})
}

fn answer(instrument: &str) -> SymbolPropertiesListAnswer {
    let rows = crate::catalog::parse_instruments(&okx_payload());
    let row = rows
        .iter()
        .find(|r| r.raw_symbol == instrument)
        .unwrap_or_else(|| panic!("{instrument} parses"));
    SymbolPropertiesListSpecOkx
        .row_answer(instrument, instrument, row, vike_model::now_ms(), &WORDS)
        .unwrap_or_else(|e| panic!("{instrument}: {e}"))
}

/// **The equality that matters.** The gatewayhub reads the CATALOG row (the grid in CONTRACTS, the
/// contract size in `contract_size`); the engine must hold what the MOUNTED path holds for the same
/// instrument - `vike_catalog::okx_grid_in_base` of the exec row - or an admitted instrument
/// would snap and cap on a grid of another unit. Compared bit for bit, per instrument.
#[test]
fn okx_facts_equal_the_mounted_paths_base_unit_grid() {
    let execution_rows = crate::execution::rest::parse_okx_perp_instruments(&okx_payload());
    for instrument in ["BTC-USDT-SWAP", "ETH-USDT-SWAP", "DOGE-USDT-SWAP", "BTC-USDT-261225"] {
        let exec = &execution_rows[instrument];
        let want = vike_catalog::okx_grid_in_base(&exec.properties, exec.ct_val);
        let got = answer(instrument);
        assert_eq!(got.properties, Some(want), "{instrument}");
        assert_eq!(
            got.contract_value, None,
            "{instrument}: a contract value would scale notional twice"
        );
    }
    let eth = answer("ETH-USDT-SWAP").properties.expect("a grid");
    assert_eq!(eth.tick_size, 0.01, "a price grid is not scaled");
    assert!((eth.step_size - 0.001).abs() < 1e-15, "0.01 contracts x ctVal 0.1: {eth:?}");
    assert!((eth.min_qty - 0.001).abs() < 1e-15 && (eth.max_qty - 1500.0).abs() < 1e-9, "{eth:?}");
    assert_eq!(eth.contract_size, 0.0, "already in base units: no second multiplier");
    let doge = answer("DOGE-USDT-SWAP").properties.expect("a grid");
    assert_eq!(doge.step_size, 1000.0, "1 contract of 1000 DOGE");
}
