//! `vike_options::OptionIndex` over REAL `public/get_instruments` rows: this crate's captured
//! fixture, mapped by the catalog's own `parse_instruments`. Moved here from
//! `crates/vike-options/src/chain/index/tests.rs` so the layer-15 maths crate names no venue; the
//! index's venue-free tests stay there. `a_captured_usdc_book_row_indexes_as_a_usd_quoted_sol_option`
//! is new here (2026-10-11), beside the catalog's widening to the USDC book.

use serde_json::{Value, json};
use vike_marketdata::OptionKind;
use vike_model::{ExerciseStyle, Instrument, Margining, OptionSpec};
use vike_options::{OptionAddress, OptionIndex};

/// The deribit crate's captured mainnet payloads (2026-10-04): `future`, `spot`, `option_btc`,
/// `option_eth` and `unfiltered_btc` (which carries the combo rows), plus `option_usdc`, the
/// USDC-settled book captured 2026-10-11.
const FIXTURE: &str = include_str!("../fixtures/deribit_get_instruments.json");

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).expect("the fixture is valid JSON")
}

/// Every payload of the fixture, through the catalog parse, into one index.
fn fixture_index() -> OptionIndex {
    let all = fixture();
    let instruments: Vec<Instrument> =
        ["future", "spot", "option_btc", "option_eth", "unfiltered_btc"]
            .iter()
            .flat_map(|k| vike_deribit::catalog::parse_instruments(&all[*k]))
            .collect();
    OptionIndex::from_instruments(&instruments)
}

/// 2026-10-05 08:00 UTC, the fixture's `5OCT26` expiry instant.
const OCT5_MS: i64 = 1_791_187_200_000;

#[test]
fn a_real_coin_settled_option_row_indexes_its_payload_contract() {
    let index = fixture_index();
    assert_eq!(
        index.option("BTC-5OCT26-74000-C"),
        Some(&OptionSpec {
            right: OptionKind::Call,
            strike: 74_000.0,
            expiry_ms: OCT5_MS,
            style: ExerciseStyle::European,
            margining: Margining::Inverse,
            settle: "BTC".into(),
            value_ccy: Some("BTC".into()),
            underlying: Some("BTC".into()),
        })
    );
    let addr = index.address("BTC-5OCT26-74000-C").expect("an indexed option has an address");
    assert_eq!(
        addr,
        OptionAddress {
            underlying: "BTC",
            expiry_iso: "2026-10-05".into(),
            strike: 74_000.0,
            right: OptionKind::Call,
            usd_quoted: false,
        },
        "coin-settled: the premium is in coin units, scaled by spot downstream"
    );
    let eth = index.address("ETH-5OCT26-2300-C").expect("the ETH book is indexed too");
    assert_eq!((eth.underlying, eth.strike, eth.usd_quoted), ("ETH", 2_300.0, false));
}

#[test]
fn a_usdc_settled_option_row_is_usd_quoted() {
    // The fixture holds no USDC-book option, so one is made from the real BTC row by rewriting
    // the fields a USDC-settled option publishes differently — and it goes through the catalog's
    // own parse, so this is the contract `contract_of` mints for such a row.
    let all = fixture();
    let mut row = all["option_btc"]["result"][0].clone();
    row["instrument_name"] = json!("SOL_USDC-5OCT26-90-P");
    row["base_currency"] = json!("SOL");
    row["quote_currency"] = json!("USDC");
    row["counter_currency"] = json!("USDC");
    row["settlement_currency"] = json!("USDC");
    row["instrument_type"] = json!("linear");
    row["option_type"] = json!("put");
    row["strike"] = json!(90);
    let instruments = vike_deribit::catalog::parse_instruments(&json!({ "result": [row] }));
    let index = OptionIndex::from_instruments(&instruments);
    let addr = index.address("SOL_USDC-5OCT26-90-P").expect("indexed");
    assert_eq!(
        addr,
        OptionAddress {
            underlying: "SOL",
            expiry_iso: "2026-10-05".into(),
            strike: 90.0,
            right: OptionKind::Put,
            usd_quoted: true,
        },
        "USDC-settled: the premium is already USD, the underlying is the payload's base"
    );
    assert_eq!(index.option("SOL_USDC-5OCT26-90-P").unwrap().margining, Margining::Linear);
}

#[test]
fn perpetuals_and_dated_futures_answer_their_underlying() {
    let index = fixture_index();
    assert_eq!(index.delta_one_underlying("BTC-PERPETUAL"), Some("BTC"));
    assert_eq!(index.delta_one_underlying("BTC_USDC-PERPETUAL"), Some("BTC"));
    assert_eq!(index.delta_one_underlying("BTC-9OCT26"), Some("BTC"));
    assert_eq!(index.delta_one_underlying("AVAX_USDC-5OCT26"), Some("AVAX"));
    // A future is not an option: it has no grid address.
    assert_eq!(index.address("BTC-PERPETUAL"), None);
}

#[test]
fn spot_combos_and_unknown_names_are_absent() {
    let index = fixture_index();
    for name in ["BTC_USDC", "BTC-FS-5OCT26_PERP", "BTC-CSR12-6OCT26-86000_86500", "BTC-1JAN30-1-C"]
    {
        assert_eq!(index.address(name), None, "{name} has no option address");
        assert_eq!(index.delta_one_underlying(name), None, "{name} is no delta-one leg");
    }
    assert!(!index.is_empty());
    assert!(OptionIndex::default().is_empty(), "the default index holds nothing");
}

/// The CAPTURED USDC-settled book row (`option_usdc`, `currency=USDC&kind=option`, 2026-10-11) —
/// the book the catalog listing carries since the Greeks tool builds its index from that listing —
/// indexes as a SOL option whose premium is already USD. Without that book every SOL position read
/// as unpriced: SOL has no coin-settled options at all.
#[test]
fn a_captured_usdc_book_row_indexes_as_a_usd_quoted_sol_option() {
    let instruments = vike_deribit::catalog::parse_instruments(&fixture()["option_usdc"]);
    let index = OptionIndex::from_instruments(&instruments);
    let name = "SOL_USDC-12OCT26-96-P";
    let spec = index.option(name).expect("the USDC book's option is indexed");
    assert!(vike_options::premium_is_usd(spec), "USDC-settled: the premium is already USD");
    assert_eq!((spec.margining, spec.settle.as_str()), (Margining::Linear, "USDC"));
    assert_eq!(
        index.address(name),
        Some(OptionAddress {
            underlying: "SOL",
            expiry_iso: "2026-10-12".into(),
            strike: 96.0,
            right: OptionKind::Put,
            usd_quoted: true,
        })
    );
}
