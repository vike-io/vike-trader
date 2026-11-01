//! The class and the typed contract ride the SAME row the grid came from, named by bybit's own
//! `contractType`/`settleCoin`/`deliveryTime`
//! (`docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`),
//! and the exec grid map and the catalog's picker rows decide it ONCE.
use super::*;
use serde_json::json;
use vike_model::{AssetClass, Delivery, FutureFamily, FutureSpec, Margining, PerpSpec};

fn class_of(contract_type: Value) -> Option<AssetClass> {
    let payload = json!({"result": {"list": [{"symbol": "S", "baseCoin": "B", "quoteCoin": "Q",
                                        "settleCoin": "B", "deliveryTime": "1798185600000",
                                        "contractType": contract_type}]}});
    parse_bybit_perp_instruments(&payload)["S"].properties.asset_class
}

/// All four words bybit publishes, projected through the typed contract's class. Linear and
/// inverse answer the SAME class on purpose — settlement mode is the contract's `Margining`, not
/// a class, and the venue's own word stays verbatim on `BybitInstrument::contract_type`.
#[test]
fn each_contract_type_names_its_own_class() {
    assert_eq!(class_of(json!("LinearPerpetual")), Some(AssetClass::CryptoPerp));
    assert_eq!(class_of(json!("InversePerpetual")), Some(AssetClass::CryptoPerp));
    assert_eq!(class_of(json!("LinearFutures")), Some(AssetClass::CryptoFuture));
    assert_eq!(class_of(json!("InverseFutures")), Some(AssetClass::CryptoFuture));
}

/// ⚠ The measured case this venue exists to warn about: `?category=linear&symbol=XRPUSD`
/// ANSWERS, with an `InversePerpetual` row. So the request's word says linear while the row's
/// word says inverse — and the contract comes from the ROW, which is also the word
/// [`non_linear_perpetual_refusal`] reads, so the stored contract and the exec refusal can never
/// disagree about what the instrument is.
#[test]
fn the_rows_word_wins_over_the_requests_word() {
    let payload = json!({"result": {"category": "linear", "list": [{
        "symbol": "XRPUSD", "baseCoin": "XRP", "quoteCoin": "USD", "settleCoin": "XRP",
        "contractType": "InversePerpetual"
    }]}});
    let inst = &parse_bybit_perp_instruments(&payload)["XRPUSD"];
    assert_eq!(inst.properties.asset_class, Some(AssetClass::CryptoPerp));
    assert!(
        matches!(&inst.contract, Some(Contract::Perp(p)) if p.margining == Margining::Inverse),
        "the request said linear; the row said inverse: {:?}",
        inst.contract
    );
    assert!(
        crate::exec::non_linear_perpetual_refusal("XRPUSD", inst.contract_type.as_deref())
            .is_some(),
        "the same word that typed the contract also refuses the order"
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
    let row = &parse_bybit_perp_instruments(&spot)["BTCUSDT"];
    assert_eq!(row.properties.asset_class, None, "an absent contractType is nobody-said");
    assert_eq!(row.contract, None);
}

/// A perpetual whose settle asset is not in the row is no contract — never one with a settle
/// asset guessed from the quote coin. The grid still parses: the exec thread needs it either way.
#[test]
fn a_row_without_a_settle_coin_has_no_contract_but_keeps_its_grid() {
    let payload = json!({"result": {"list": [{"symbol": "S", "baseCoin": "B", "quoteCoin": "USDT",
        "contractType": "LinearPerpetual", "priceFilter": {"tickSize": "0.5"}}]}});
    let row = &parse_bybit_perp_instruments(&payload)["S"];
    assert_eq!(row.contract, None);
    assert_eq!(row.properties.asset_class, None);
    assert_eq!(row.properties.tick_size, 0.5);
    assert_eq!(row.contract_type.as_deref(), Some("LinearPerpetual"));
}

/// The committed `category=inverse` page (REAL, fetched 2026-10-11, trimmed to three rows).
fn inverse_fixture() -> Value {
    serde_json::from_str(include_str!("../tests/fixtures/instruments_info/inverse.json")).unwrap()
}

/// ⚠ **A dated future is typed from its own fields** — `InverseFutures`, `settleCoin` and the
/// `deliveryTime` epoch ms — even though the picker does not offer it (no route; see
/// `crates/bridges/bybit/src/catalog.rs`'s module doc). The row names no tenor and no delivery
/// method, so both are unknown rather than read off `BTCUSDZ26`'s spelling.
#[test]
fn a_real_inverse_future_row_is_a_dated_future() {
    let row = &parse_bybit_perp_instruments(&inverse_fixture())["BTCUSDZ26"];
    assert_eq!(
        row.contract,
        Some(Contract::Future(FutureSpec {
            margining: Margining::Inverse,
            settle: "BTC".into(),
            value_ccy: Some("USD".into()),
            expiry_ms: 1_798_185_600_000,
            tenor: None,
            delivery: Delivery::Unknown,
            family: FutureFamily::Crypto,
        }))
    );
    assert_eq!(row.properties.asset_class, Some(AssetClass::CryptoFuture));
    assert_eq!(row.properties.tick_size, 0.5);
    assert_eq!(row.properties.step_size, 1.0);
    assert_eq!(row.contract_type.as_deref(), Some("InverseFutures"));
}

/// A dated future with no positive `deliveryTime` is no contract: an expiry is what makes it one.
#[test]
fn a_future_without_a_delivery_time_is_no_contract() {
    for delivery in [json!("0"), Value::Null, json!("junk")] {
        let payload = json!({"result": {"list": [{"symbol": "F", "baseCoin": "BTC",
            "quoteCoin": "USD", "settleCoin": "BTC", "contractType": "InverseFutures",
            "deliveryTime": delivery}]}});
        assert_eq!(parse_bybit_perp_instruments(&payload)["F"].contract, None, "{delivery}");
    }
}

/// ⚠ **Identity is decided ONCE**: over the same real page, every picker row the catalog mints
/// carries exactly the contract and grid the exec plane's map holds for the same venue symbol.
#[test]
fn the_picker_row_and_the_exec_grid_are_one_decision() {
    for payload in [
        inverse_fixture(),
        serde_json::from_str(include_str!(
            "../tests/fixtures/instruments_info/linear_btcusdt.json"
        ))
        .unwrap(),
    ] {
        let grid = parse_bybit_perp_instruments(&payload);
        let picker: Vec<_> = crate::catalog::parse_inverse(&payload)
            .into_iter()
            .chain(crate::catalog::parse_perp(&payload))
            .collect();
        assert!(!picker.is_empty());
        for inst in picker {
            let wire = inst.raw_symbol.strip_suffix(".P").expect("a perp row carries .P");
            let row = &grid[wire];
            assert_eq!(row.contract.as_ref(), Some(&inst.contract), "{wire}");
            assert_eq!(row.properties, inst.properties, "{wire}");
            assert_eq!(row.contract_type, inst.contract_type, "{wire}");
        }
    }
}

/// The real linear row: a linear perp settling USDT, valued in the base coin, funding every
/// 480 minutes — and the PerpSpec carries no expiry because a perpetual has none.
#[test]
fn a_real_linear_row_is_a_linear_perp() {
    let payload: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/instruments_info/linear_btcusdt.json"
    ))
    .unwrap();
    assert_eq!(
        parse_bybit_perp_instruments(&payload)["BTCUSDT"].contract,
        Some(Contract::Perp(PerpSpec {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_ccy: Some("BTC".into()),
            funding_interval_ms: Some(480 * 60_000),
            settling_ms: None,
        }))
    );
}
