//! **One golden file, two implementations** (Dukascopy option A, owner 2026-11-11):
//! `crates/bridges/dukascopy/data/position_legs.json` holds legs and the answers that both
//! `vike_model::PositionStatusReport::from_legs` (here) and the sidecar's `NetPosition.java`
//! (`crates/bridges/dukascopy/jforex-bridge/src/test/java/vike/jforex/NetPositionTest.java`) must give,
//! so the Rust netting rule and the Java one cannot drift apart. An edit to the file runs both: this
//! crate's suite and the `jforex-bridge.yml` gate, whose path filter covers `data/`.
//!
//! The tolerance is the Java test's own `1e-12`: the file's decimals are the answers, not their bits.

use serde_json::Value;
use vike_model::events::PositionSide;
use vike_model::{ContractValue, PositionLeg, PositionStatusReport};

const GOLDEN: &str = include_str!("../data/position_legs.json");
const TOLERANCE: f64 = 1e-12;

fn number(v: &Value, key: &str) -> f64 {
    v[key].as_f64().unwrap_or_else(|| panic!("`{key}` is a number in {v}"))
}

fn close(have: f64, want: f64) -> bool {
    (have - want).abs() <= TOLERANCE
}

fn cases() -> Vec<Value> {
    let golden: Value = serde_json::from_str(GOLDEN).expect("position_legs.json is JSON");
    let cases = golden["cases"].as_array().expect("a `cases` array").clone();
    assert!(cases.len() >= 7, "the golden file lost its cases: {}", cases.len());
    cases
}

/// Every case's legs net to exactly the file's rows: side, signed size and price.
#[test]
fn from_legs_gives_the_golden_rows() {
    for case in cases() {
        let name = case["name"].as_str().expect("a case name");
        let legs: Vec<PositionLeg> = case["legs"]
            .as_array()
            .expect("a `legs` array")
            .iter()
            .map(|l| PositionLeg {
                size: number(l, "size"),
                price: number(l, "price"),
                timestamp_ms: 0,
            })
            .collect();
        let have =
            PositionStatusReport::from_legs("dukascopy", "EURUSD", &legs, ContractValue::default());
        let want = case["rows"].as_array().expect("a `rows` array");
        assert_eq!(have.len(), want.len(), "{name}: {have:?}");
        for (row, want) in have.iter().zip(want) {
            let side: PositionSide =
                serde_json::from_value(want["side"].clone()).expect("a side word");
            assert_eq!(row.position_side, side, "{name}: {row:?}");
            assert!(close(row.qty, number(want, "size")), "{name}: {row:?}");
            assert!(close(row.average_price, number(want, "price")), "{name}: {row:?}");
        }
    }
}

/// The file cannot disagree with itself: each case's `net` (what `NetPosition.java` answers over every
/// leg) is the sum of its rows, at the signed-weighted price, flat with no basis when the rows cancel.
#[test]
fn every_net_is_the_sum_of_its_rows() {
    for case in cases() {
        let name = case["name"].as_str().expect("a case name");
        let rows = case["rows"].as_array().expect("a `rows` array");
        let size: f64 = rows.iter().map(|r| number(r, "size")).sum();
        let notional: f64 = rows.iter().map(|r| number(r, "size") * number(r, "price")).sum();
        let net = &case["net"];
        assert!(close(number(net, "size"), size), "{name}: net size {size}");
        let price = if size.abs() <= TOLERANCE { 0.0 } else { notional / size };
        assert!(close(number(net, "price"), price), "{name}: net price {price}");
    }
}
