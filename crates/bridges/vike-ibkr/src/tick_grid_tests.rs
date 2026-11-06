//! The exec's grid check over the REAL market rule of `SAP.SMART.EUR` (captured from the paper gateway, 2026-10-30).

#[cfg(feature = "ibkr-socket")]
use vike_model::OrderType;

use super::*;
use crate::any_symbol::PriceBand;

/// Market rule 1874 (also 1943 and 3055: the same table) of `SAP.SMART.EUR`, read off the paper gateway on 2026-10-30
/// 19:57 UTC: `low_edge:increment`. Its coarsest band, 10, starts at 50000.
const SAP_RULE: [(f64, f64); 19] = [
    (0.0, 0.0001),
    (0.1, 0.0001),
    (0.2, 0.0001),
    (0.5, 0.0001),
    (1.0, 0.0002),
    (2.0, 0.0005),
    (5.0, 0.001),
    (10.0, 0.002),
    (20.0, 0.005),
    (50.0, 0.01),
    (100.0, 0.02),
    (200.0, 0.05),
    (500.0, 0.1),
    (1000.0, 0.2),
    (2000.0, 0.5),
    (5000.0, 1.0),
    (10000.0, 2.0),
    (20000.0, 5.0),
    (50000.0, 10.0),
];

fn sap_rules() -> Vec<Vec<PriceBand>> {
    vec![
        SAP_RULE
            .iter()
            .map(|(low_edge, increment)| PriceBand { low_edge: *low_edge, increment: *increment })
            .collect(),
    ]
}

#[cfg(feature = "ibkr-socket")]
fn limit(symbol: &str, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: format!("coid-{symbol}"),
        venue: "ibkr".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(price),
        ..Default::default()
    }
}

/// A price a whole number of the band's increments from the origin is on the grid of that band, and only of it.
#[test]
fn a_price_is_judged_at_its_own_band_of_the_real_sap_rule() {
    let rules = sap_rules();
    for on in [213.35, 213.4, 99.99, 49.37, 0.1234, 51000.0] {
        assert_eq!(off_grid(&rules, on), None, "{on} is on the grid of its band");
    }
    // 213.37 sits in the 0.05 band (200..500): 0.05 does not divide it.
    assert_eq!(off_grid(&rules, 213.37), Some(vec![0.05]));
    // 99.995 is under 100, so it sits in the 0.01 band.
    assert_eq!(off_grid(&rules, 99.995), Some(vec![0.01]));
    // A price that is not a price is nobody's to judge here.
    for odd in [0.0, -1.0, f64::NAN] {
        assert_eq!(off_grid(&rules, odd), None);
    }
}

/// Where the contract names several rules the price is let through when it is on the grid of ANY of them (which
/// exchange SMART routes to is not the adapter's to say), and refused with every increment when on none.
#[test]
fn a_price_on_the_grid_of_any_rule_passes_and_on_none_is_refused_with_each_increment() {
    let fine = vec![PriceBand { low_edge: 0.0, increment: 0.01 }];
    let coarse = vec![PriceBand { low_edge: 0.0, increment: 0.05 }];
    let rules = vec![fine, coarse];
    assert_eq!(off_grid(&rules, 10.01), None, "on the fine rule's grid");
    assert_eq!(off_grid(&rules, 10.05), None);
    assert_eq!(off_grid(&rules, 10.005), Some(vec![0.01, 0.05]));
}

/// THE REFUSAL: a limit price off the grid of the symbol's noted rule is refused by name with the venue's real
/// increment; a price on it, a market order and a symbol never looked up pass (fail-open). (`note` is the socket
/// feature's: the facts source that calls it exists only there.)
#[cfg(feature = "ibkr-socket")]
#[test]
fn the_exec_refuses_a_limit_off_the_venues_grid_by_name() {
    note("SAP.SMART.EUR", &sap_rules(), Some(212.0));
    let why = refusal(&limit("SAP.SMART.EUR", 213.37)).expect("213.37 is off the 0.05 grid");
    assert!(
        why.contains("213.37") && why.contains("0.05") && why.contains("SAP.SMART.EUR"),
        "{why}"
    );
    assert!(why.contains("212"), "the price the facts were read at is named: {why}");
    assert_eq!(refusal(&limit("SAP.SMART.EUR", 213.35)), None, "on the grid");

    let mut market = limit("SAP.SMART.EUR", 213.37);
    market.order_type = OrderType::Market;
    market.price = None;
    assert_eq!(refusal(&market), None, "no price, nothing to hold to the grid");

    assert_eq!(
        refusal(&limit("NEVER.SMART.EUR", 213.37)),
        None,
        "a symbol never looked up is the venue's to judge"
    );

    let mut stop = limit("SAP.SMART.EUR", 100.0);
    stop.price = None;
    stop.trigger_price = Some(213.37);
    assert!(refusal(&stop).is_some(), "a trigger price is held to the grid too");
}

/// The 10.0 of the first reading of the grid is a band of 50000 and up; the tick of a 213 EUR price is not it.
#[test]
fn the_coarsest_band_of_the_real_rule_is_the_highest_priced_one() {
    let rules = sap_rules();
    assert_eq!(increment_at(&rules[0], 213.0), 0.05);
    assert_eq!(increment_at(&rules[0], 60_000.0), 10.0);
    assert_eq!(increment_at(&rules[0], 0.00005), 0.0001, "below every edge: the first band");
}
