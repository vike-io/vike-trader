//! `catalog::parse_insts` over one `public/instruments` response per OKX `instType`: every row's
//! typed `Contract` is asserted EXACTLY, with its grid and contract value (decision 0126).
//!
//! The fixtures are `tests/fixtures/instruments/{spot,swap,futures,option}.json`; each names its
//! provenance in a `_provenance` key the parser ignores. Most rows are a keyless live GET of
//! 2026-10-11 transcribed field by field; the linear future (`ETH-USDT-261225`) is "per OKX docs,
//! no captured payload".

use serde_json::Value;
use vike_model::{
    AssetClass, Contract, Delivery, ExerciseStyle, FutureFamily, FutureSpec, Instrument, Margining,
    OptionKind, OptionSpec, PerpSpec, Tenor,
};
use vike_okx::catalog::parse_insts;

fn parse(body: &str) -> Vec<Instrument> {
    let v: Value = serde_json::from_str(body).expect("fixture is JSON");
    parse_insts(&v)
}

fn row<'a>(rows: &'a [Instrument], inst_id: &str) -> &'a Instrument {
    rows.iter().find(|i| i.raw_symbol == inst_id).unwrap_or_else(|| panic!("{inst_id} parsed"))
}

#[test]
fn spot_is_crypto_spot_with_its_own_base_grid() {
    let rows = parse(include_str!("../fixtures/instruments/spot.json"));
    assert_eq!(rows.len(), 1);
    let i = &rows[0];
    assert_eq!(i.contract, Contract::CryptoSpot);
    assert_eq!(i.asset_class(), AssetClass::CryptoSpot);
    assert_eq!((i.base.as_str(), i.quote.as_str()), ("BTC", "USDT"));
    assert_eq!(i.properties.tick_size, 0.1);
    assert_eq!(i.properties.step_size, 0.00000001);
    assert_eq!(i.properties.min_qty, 0.00001);
    // OKX states a SPOT `maxMktSz` in the QUOTE currency, so it is not a base max_qty.
    assert_eq!(i.properties.max_qty, 0.0);
    assert_eq!(i.properties.contract_size, 0.0, "a spot row has no contract value");
    assert_eq!(i.contract_type, None);
}

#[test]
fn swaps_are_perps_whose_margining_is_the_ct_type_field() {
    let rows = parse(include_str!("../fixtures/instruments/swap.json"));
    assert_eq!(rows.len(), 2);

    let linear = row(&rows, "BTC-USDT-SWAP");
    assert_eq!(
        linear.contract,
        Contract::Perp(PerpSpec {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_ccy: Some("BTC".into()),
            funding_interval_ms: None,
            settling_ms: None,
        })
    );
    assert_eq!((linear.base.as_str(), linear.quote.as_str()), ("BTC", "USDT"));
    assert_eq!(linear.settle_asset(), Some("USDT"));
    assert_eq!(linear.contract_type.as_deref(), Some("linear"));
    assert_eq!(linear.properties.contract_size, 0.01, "ctVal 0.01 x ctMult 1, in BTC");
    assert_eq!(linear.properties.tick_size, 0.1);
    assert_eq!(linear.properties.step_size, 0.01, "lotSz counts CONTRACTS");
    assert_eq!(linear.properties.min_qty, 0.01);
    assert_eq!(linear.properties.max_qty, 35000.0);
    assert_eq!(linear.properties.asset_class, Some(AssetClass::CryptoPerp));

    let inverse = row(&rows, "BTC-USD-SWAP");
    assert_eq!(
        inverse.contract,
        Contract::Perp(PerpSpec {
            margining: Margining::Inverse,
            settle: "BTC".into(),
            value_ccy: Some("USD".into()),
            funding_interval_ms: None,
            settling_ms: None,
        })
    );
    assert_eq!((inverse.base.as_str(), inverse.quote.as_str()), ("BTC", "USD"));
    assert_eq!(inverse.contract_type.as_deref(), Some("inverse"));
    assert_eq!(inverse.properties.contract_size, 100.0, "100 USD, not 100 BTC: value_ccy says so");
}

#[test]
fn futures_carry_expiry_and_tenor_from_the_row() {
    let rows = parse(include_str!("../fixtures/instruments/futures.json"));
    assert_eq!(rows.len(), 2);

    assert_eq!(
        row(&rows, "BTC-USD-261030").contract,
        Contract::Future(FutureSpec {
            margining: Margining::Inverse,
            settle: "BTC".into(),
            value_ccy: Some("USD".into()),
            expiry_ms: 1_793_347_200_000,
            tenor: Some(Tenor::Month),
            delivery: Delivery::Unknown,
            family: FutureFamily::Crypto,
        })
    );
    let linear = row(&rows, "ETH-USDT-261225");
    assert_eq!(
        linear.contract,
        Contract::Future(FutureSpec {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_ccy: Some("ETH".into()),
            expiry_ms: 1_798_185_600_000,
            tenor: Some(Tenor::Quarter),
            delivery: Delivery::Unknown,
            family: FutureFamily::Crypto,
        })
    );
    assert_eq!((linear.base.as_str(), linear.quote.as_str()), ("ETH", "USDT"));
    assert_eq!(linear.properties.contract_size, 0.1);
    assert_eq!(linear.asset_class(), AssetClass::CryptoFuture);
}

#[test]
fn options_carry_right_strike_expiry_and_underlying_from_the_row() {
    let rows = parse(include_str!("../fixtures/instruments/option.json"));
    assert_eq!(rows.len(), 2);
    let spec = |right| {
        Contract::Option(OptionSpec {
            right,
            strike: 75_000.0,
            expiry_ms: 1_791_792_000_000,
            style: ExerciseStyle::European,
            margining: Margining::Inverse,
            settle: "BTC".into(),
            value_ccy: Some("BTC".into()),
            underlying: Some("BTC-USD".into()),
        })
    };
    let call = row(&rows, "BTC-USD-261012-75000-C");
    assert_eq!(call.contract, spec(OptionKind::Call));
    assert_eq!(row(&rows, "BTC-USD-261012-75000-P").contract, spec(OptionKind::Put));
    assert_eq!((call.base.as_str(), call.quote.as_str()), ("BTC", "USD"));
    assert_eq!(call.properties.contract_size, 0.01, "ctVal 1 x ctMult 0.01 BTC");
    assert_eq!(call.properties.tick_size, 0.0001);
    assert_eq!(call.asset_class(), AssetClass::Option);
}
