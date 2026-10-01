//! The class rides the SAME row the grid came from, named by bybit's own `contractType`
//! (`docs/decisions/0061-an-instrument-names-its-kind.md`).
use super::*;
use serde_json::json;

fn class_of(contract_type: Value) -> Option<AssetClass> {
    let payload = json!({"result": {"list": [{"symbol": "S", "baseCoin": "B",
                                        "contractType": contract_type}]}});
    parse_bybit_perp_instruments(&payload)["S"].properties.asset_class
}

/// All four words bybit publishes. Linear and inverse answer the SAME class on purpose —
/// settlement mode is not what this taxonomy spells, and the venue's own word for it stays
/// verbatim on `BybitInstrument::contract_type`.
#[test]
fn each_contract_type_names_its_own_class() {
    assert_eq!(class_of(json!("LinearPerpetual")), Some(AssetClass::CryptoPerp));
    assert_eq!(class_of(json!("InversePerpetual")), Some(AssetClass::CryptoPerp));
    assert_eq!(class_of(json!("LinearFutures")), Some(AssetClass::CryptoFuture));
    assert_eq!(class_of(json!("InverseFutures")), Some(AssetClass::CryptoFuture));
}

/// ⚠ The measured case this venue exists to warn about: `?category=linear&symbol=XRPUSD`
/// ANSWERS, with an `InversePerpetual` row. So the request's word says linear while the row's
/// word says inverse — and the class comes from the ROW, which is also the word
/// [`non_linear_perpetual_refusal`] reads, so the stored class and the exec refusal can never
/// disagree about what the instrument is.
#[test]
fn the_rows_word_wins_over_the_requests_word() {
    let payload = json!({"result": {"category": "linear", "list": [{
        "symbol": "XRPUSD", "baseCoin": "XRP", "settleCoin": "XRP",
        "contractType": "InversePerpetual"
    }]}});
    let inst = &parse_bybit_perp_instruments(&payload)["XRPUSD"];
    assert_eq!(inst.properties.asset_class, Some(AssetClass::CryptoPerp));
    assert!(
        crate::exec::non_linear_perpetual_refusal("XRPUSD", inst.contract_type.as_deref())
            .is_some(),
        "the same word that named the class also refuses the order"
    );
}

/// Absent, empty, null, wrong-cased and unknown all read as "the venue said nothing". A SPOT
/// row carries no `contractType` at all, and tagging it `CryptoSpot` from its ABSENCE would be
/// a guess about a row this parser was not given a word for.
#[test]
fn an_unrecognised_contract_type_is_unknown_not_a_guess() {
    assert_eq!(class_of(Value::Null), None);
    assert_eq!(class_of(json!("")), None);
    assert_eq!(class_of(json!("linearperpetual")), None, "the word is case-sensitive");
    assert_eq!(class_of(json!("SomethingNew")), None);
    let spot = json!({"result": {"list": [{"symbol": "BTCUSDT", "baseCoin": "BTC"}]}});
    assert_eq!(
        parse_bybit_perp_instruments(&spot)["BTCUSDT"].properties.asset_class,
        None,
        "an absent contractType is nobody-said"
    );
}
