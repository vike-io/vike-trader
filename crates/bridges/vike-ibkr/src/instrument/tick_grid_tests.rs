//! The exec's grid check over the REAL market rule of `SAP.SMART.EUR` (captured from the paper gateway, 2026-10-30).

#[cfg(feature = "ibkr-socket")]
use vike_model::OrderType;

use super::*;
use crate::instrument::any_symbol::PriceBand;

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
        order_client_id: format!("order-client-id-{symbol}"),
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
fn the_execution_refuses_a_limit_off_the_venues_grid_by_name() {
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

/// **A pegged order's offset goes out on the grid** of the band its symbol's price was read in (the coarsest of its
/// rules there; SAP's 0.05 at 212), rounded toward the PASSIVE side: an aggressive offset down to a whole step, a
/// passive one further down, one on the grid as it is. A non-zero offset that rounds to nothing is refused by name, at
/// the exec's grid check too; a symbol with no table, or no price read for it, keeps its offset (fail-open).
#[test]
fn a_peg_offset_is_put_on_the_grid_toward_passive_and_refused_when_it_rounds_to_nothing() {
    use vike_model::{OrderPeg, OrderPegAnchor, OrderType};
    GRIDS.lock().unwrap().insert("PEG.SMART.EUR".into(), (sap_rules(), Some(212.0)));
    GRIDS.lock().unwrap().insert("PEGNOPRICE.SMART.EUR".into(), (sap_rules(), None));
    let pegged = |symbol: &str, offset| OrderRequest {
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Pegged,
        peg: Some(OrderPeg { anchor: OrderPegAnchor::Mid, offset }),
        ..Default::default()
    };
    let on = |offset| peg_offset_on_grid(&pegged("PEG.SMART.EUR", offset));
    assert_eq!(on(0.12), Ok(Some(0.1)), "aggressive: down to a whole step");
    assert_eq!(on(0.15), Ok(Some(0.15)), "on the grid: as it is");
    assert_eq!(on(-0.12), Ok(Some(-0.15)), "passive: further down");
    assert_eq!(on(-0.1), Ok(Some(-0.1)));
    assert_eq!(on(-0.03), Ok(Some(-0.05)), "a passive offset never rounds to nothing");
    assert_eq!(on(0.0), Ok(None), "an offset of 0 is on every grid");
    let why = on(0.03).expect_err("0.03 rounds to nothing on the 0.05 step");
    for needle in ["0.03", "0.05", "PEG.SMART.EUR", "nothing was sent"] {
        assert!(why.contains(needle), "`{needle}` missing from: {why}");
    }
    assert_eq!(refusal(&pegged("PEG.SMART.EUR", 0.03)), Some(why), "the exec's grid check says it");
    for symbol in ["PEGNOPRICE.SMART.EUR", "NEVERPEGGED.SMART.EUR"] {
        assert_eq!(peg_offset_on_grid(&pegged(symbol, 0.03)), Ok(None), "{symbol}: fail-open");
        assert_eq!(refusal(&pegged(symbol, 0.03)), None, "{symbol}");
    }
    let mut req = pegged("PEG.SMART.EUR", -0.12);
    put_distances_on_grid(&mut req);
    assert_eq!(req.peg.map(|p| p.offset), Some(-0.15), "the order goes out on the grid");
    let mut refused = pegged("PEG.SMART.EUR", 0.03);
    put_distances_on_grid(&mut refused);
    assert_eq!(
        refused.peg.map(|p| p.offset),
        Some(0.03),
        "a refused offset is left for the refusal"
    );
}

/// **A discretionary amount, a trailing order's trail and limit offset, and an adjustment's trail AMOUNT go out on the grid
/// ROUNDED DOWN**
/// (never further than asked: SAP's 0.05 at 212), each refused by name when a non-zero one rounds to nothing; a trail on a
/// kind that does not trail (the backtest-only field) and a percent are left alone; a symbol with no table keeps them
/// (fail-open). **An adjustment's level and its target's stop and limit are prices**, refused off the grid like the
/// order's own.
#[test]
fn the_distances_round_down_onto_the_grid_and_an_adjustments_prices_are_held_to_it() {
    use vike_model::{OrderAttributes, OrderStopAdjust, OrderStopAdjustTo, OrderType};
    GRIDS.lock().unwrap().insert("DIST.SMART.EUR".into(), (sap_rules(), Some(212.0)));
    let base = OrderRequest {
        symbol: "DIST.SMART.EUR".into(),
        side: -1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(213.35),
        ..Default::default()
    };
    let discretion = |d| OrderRequest {
        attributes: Some(OrderAttributes { discretionary_amount: Some(d), ..Default::default() }),
        ..base.clone()
    };
    let trailing = |t| OrderRequest {
        order_type: OrderType::Trailing,
        price: None,
        trail: Some(t),
        ..base.clone()
    };
    let adjustable = |at, to| OrderRequest {
        order_type: OrderType::Stop,
        price: None,
        trigger_price: Some(200.0),
        adjust: Some(OrderStopAdjust { at, to }),
        ..base.clone()
    };
    let to_trail =
        |t| OrderStopAdjustTo::Trailing { stop: 210.0, trail: Some(t), trail_percent: None };

    let mut r = discretion(0.12);
    put_distances_on_grid(&mut r);
    assert_eq!(r.attributes.and_then(|a| a.discretionary_amount), Some(0.1), "down to a step");
    let mut r = trailing(2.53);
    put_distances_on_grid(&mut r);
    assert_eq!(r.trail, Some(2.5));
    let mut r = adjustable(220.0, to_trail(1.07));
    put_distances_on_grid(&mut r);
    assert_eq!(r.adjust.map(|a| a.to), Some(to_trail(1.05)));
    let mut r = trailing(2.5);
    put_distances_on_grid(&mut r);
    assert_eq!(r.trail, Some(2.5), "on the grid: as it is");
    // A `TRAIL LIMIT`'s (and a `TRAIL LIT`'s) limit offset rounds down the same way; one of 0 stays 0.
    let offset =
        |kind, o| OrderRequest { order_type: kind, limit_offset: Some(o), ..trailing(2.5) };
    for kind in [OrderType::TrailingLimit, OrderType::TrailingTakeProfit] {
        let mut r = offset(kind.clone(), 0.27);
        put_distances_on_grid(&mut r);
        assert_eq!(r.limit_offset, Some(0.25), "{kind:?}");
        let mut zero = offset(kind.clone(), 0.0);
        assert_eq!(refusal(&zero), None, "{kind:?}: an offset of 0 is on every grid");
        put_distances_on_grid(&mut zero);
        assert_eq!(zero.limit_offset, Some(0.0));
    }

    for (req, what) in [
        (discretion(0.03), "discretionary amount 0.03"),
        (trailing(0.04), "trail 0.04"),
        (offset(OrderType::TrailingLimit, 0.02), "limit offset 0.02"),
        (adjustable(220.0, to_trail(0.02)), "adjustment's trail 0.02"),
    ] {
        let why = refusal(&req).expect(what);
        assert!(
            why.contains(what) && why.contains("0.05") && why.contains("nothing was sent"),
            "{why}"
        );
        let mut kept = req.clone();
        put_distances_on_grid(&mut kept);
        assert_eq!(kept, req, "a refused distance is left for the refusal");
    }

    let mut backtest = OrderRequest { trail: Some(2.53), ..base.clone() };
    put_distances_on_grid(&mut backtest);
    assert_eq!(backtest.trail, Some(2.53), "a limit's trail is the backtest field, never judged");
    let pct = OrderStopAdjustTo::Trailing { stop: 210.0, trail: None, trail_percent: Some(1.07) };
    let mut r = adjustable(220.0, pct);
    put_distances_on_grid(&mut r);
    assert_eq!(r.adjust.map(|a| a.to), Some(pct), "a percent is no price");
    let mut never = OrderRequest { symbol: "NEVERDIST.SMART.EUR".into(), ..discretion(0.03) };
    assert_eq!(refusal(&never), None, "no table: fail-open");
    put_distances_on_grid(&mut never);
    assert_eq!(never.attributes.and_then(|a| a.discretionary_amount), Some(0.03));

    // The adjustment's prices: the level, the target's stop, the target's limit.
    let on = OrderStopAdjustTo::StopLimit { stop: 210.0, limit: 209.95 };
    assert_eq!(refusal(&adjustable(220.0, on)), None, "every price on the grid");
    for (at, to, off) in [
        (220.03, on, "220.03"),
        (220.0, OrderStopAdjustTo::Stop { stop: 210.02 }, "210.02"),
        (220.0, OrderStopAdjustTo::StopLimit { stop: 210.0, limit: 209.97 }, "209.97"),
    ] {
        let why = refusal(&adjustable(at, to)).expect(off);
        assert!(why.contains(off) && why.contains("0.05"), "{why}");
    }
}

/// A distance goes on the grid by the ONE model rule (`vike_model::snap_limit`, the owner's D11 and G2): one a few
/// ULP under a grid point names it. `0.3 - 0.2` is `0.09999999999999998`, and the old floor sent a trail, a
/// discretionary amount and a peg offset of it a whole step short (0.05 on SAP's 0.05), a passive offset too.
#[test]
fn a_distance_a_few_ulp_under_a_grid_point_is_that_point() {
    use vike_model::{OrderAttributes, OrderPeg, OrderPegAnchor, OrderType};
    GRIDS.lock().unwrap().insert("ULP.SMART.EUR".into(), (sap_rules(), Some(212.0)));
    let tenth = 0.3 - 0.2;
    assert!(tenth < 0.1, "the premise: {tenth:?}");
    let base =
        OrderRequest { symbol: "ULP.SMART.EUR".into(), side: 1, qty: 1.0, ..Default::default() };
    let mut trailing =
        OrderRequest { order_type: OrderType::Trailing, trail: Some(tenth), ..base.clone() };
    put_distances_on_grid(&mut trailing);
    assert_eq!(trailing.trail, Some(0.1));
    let mut discretion = OrderRequest {
        order_type: OrderType::Limit,
        price: Some(212.0),
        attributes: Some(OrderAttributes {
            discretionary_amount: Some(tenth),
            ..Default::default()
        }),
        ..base.clone()
    };
    put_distances_on_grid(&mut discretion);
    assert_eq!(discretion.attributes.and_then(|a| a.discretionary_amount), Some(0.1));
    let pegged = |offset| OrderRequest {
        order_type: OrderType::Pegged,
        peg: Some(OrderPeg { anchor: OrderPegAnchor::Mid, offset }),
        ..base.clone()
    };
    assert_eq!(peg_offset_on_grid(&pegged(tenth)), Ok(Some(0.1)), "aggressive");
    assert_eq!(peg_offset_on_grid(&pegged(-tenth)), Ok(Some(-0.1)), "passive");
}

/// The 10.0 of the first reading of the grid is a band of 50000 and up; the tick of a 213 EUR price is not it.
#[test]
fn the_coarsest_band_of_the_real_rule_is_the_highest_priced_one() {
    let rules = sap_rules();
    assert_eq!(increment_at(&rules[0], 213.0), 0.05);
    assert_eq!(increment_at(&rules[0], 60_000.0), 10.0);
    assert_eq!(increment_at(&rules[0], 0.00005), 0.0001, "below every edge: the first band");
}
