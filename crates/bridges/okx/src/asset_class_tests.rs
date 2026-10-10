//! The class rides the SAME row the grid came from, named by OKX's own `instType`
//! (`docs/decisions/0061-an-instrument-names-its-kind.md`).
use super::*;
use serde_json::json;

fn row(inst_id: &str, inst_type: Value) -> Value {
    json!({"data": [{"instId": inst_id, "instType": inst_type, "ctValCcy": "BTC",
                         "lotSz": "0.01", "minSz": "0.01", "tickSz": "0.1"}]})
}

fn class_of(inst_id: &str, inst_type: Value) -> Option<AssetClass> {
    parse_okx_perp_instruments(&row(inst_id, inst_type))[inst_id].properties.asset_class
}

/// Each of OKX's four listed instTypes, on a row whose `instId` is DELIBERATELY the wrong
/// shape for the word — a `-SWAP`-tailed id declaring `SPOT`, and a bare pair declaring
/// `SWAP`. The venue's word wins both ways, which is what proves nothing is read off the id.
#[test]
fn each_inst_type_names_its_own_class() {
    assert_eq!(class_of("BTC-USDT-SWAP", json!("SWAP")), Some(AssetClass::CryptoPerp));
    assert_eq!(class_of("BTC-USDT-250926", json!("FUTURES")), Some(AssetClass::CryptoFuture));
    assert_eq!(class_of("BTC-USDT-SWAP", json!("SPOT")), Some(AssetClass::CryptoSpot));
    assert_eq!(class_of("BTC-USDT", json!("SWAP")), Some(AssetClass::CryptoPerp));
    assert_eq!(class_of("BTC-USD-250926-100000-C", json!("OPTION")), Some(AssetClass::Option));
}

/// Absent, empty, null, wrong-typed, wrong-cased and simply UNKNOWN all read as "the venue
/// said nothing" — never the neighbouring variant. `MARGIN` is the live example: OKX lists it
/// today and it is spot-margin, which no variant of this taxonomy claims.
#[test]
fn an_unrecognised_inst_type_is_unknown_not_a_guess() {
    assert_eq!(class_of("NOTYPE-USDT-SWAP", Value::Null), None);
    assert_eq!(class_of("EMPTY-USDT-SWAP", json!("")), None);
    assert_eq!(class_of("MARGIN-USDT", json!("MARGIN")), None);
    assert_eq!(class_of("CASED-USDT-SWAP", json!("swap")), None, "the word is case-sensitive");
    assert_eq!(class_of("NUMERIC-USDT-SWAP", json!(1)), None);
    let missing = json!({"data": [{"instId": "GONE-USDT-SWAP", "ctValCcy": "B"}]});
    assert_eq!(
        parse_okx_perp_instruments(&missing)["GONE-USDT-SWAP"].properties.asset_class,
        None,
        "an absent instType key is nobody-said, exactly as an absent one is"
    );
}
