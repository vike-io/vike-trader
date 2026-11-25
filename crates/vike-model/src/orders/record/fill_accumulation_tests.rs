use super::modify_tests::limit_order;
use super::*;
use crate::events::{OrderAccepted, OrderFilled, OrderPartiallyFilled, OrderSubmitted};

fn submitted() -> Event {
    Event::OrderSubmitted(OrderSubmitted { order_client_id: "c".into(), timestamp: 0 })
}

fn accepted() -> Event {
    Event::OrderAccepted(OrderAccepted {
        order_client_id: "c".into(),
        venue_order_id: None,
        timestamp: 0,
    })
}

fn fill(trade_id: &'static str, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        order_client_id: "c".into(),
        venue: "v".into(),
        symbol: "s".into(),
        side: 1,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 0,
        mark_price: None,
        position_side: crate::events::PositionSide::Both,
    }
}

fn partially_filled(trade_id: &'static str, qty: f64, px: f64) -> Event {
    Event::OrderPartiallyFilled(OrderPartiallyFilled {
        order_client_id: "c".into(),
        fill: fill(trade_id, qty, px),
        timestamp: 0,
    })
}

fn filled(trade_id: &'static str, qty: f64, px: f64) -> Event {
    Event::OrderFilled(OrderFilled {
        order_client_id: "c".into(),
        fill: fill(trade_id, qty, px),
        timestamp: 0,
    })
}

/// The documented law itself: after N partial fills, `average_fill_price` is the QUANTITY-WEIGHTED
/// mean of every fill price folded so far, and `filled_qty` is their sum. Three fills with
/// DIFFERENT prices AND different quantities -- a single fill, equal quantities, or equal
/// prices cannot distinguish this from `(sum of prices) / count` or any other wrong fold --
/// exercising exactly the running update `accumulate_fill` performs:
/// `(average_fill_price * previous + last_price * last_qty) / new`.
#[test]
fn running_vwap_is_the_quantity_weighted_mean_of_all_fills() {
    let mut o = limit_order(10.0);
    o.apply(&submitted()).unwrap();
    o.apply(&accepted()).unwrap();

    o.apply(&partially_filled("t1", 2.0, 100.0)).unwrap();
    assert_eq!(o.filled_qty, 2.0);
    assert_eq!(o.average_fill_price, 100.0, "first fill: VWAP is just its own price");

    o.apply(&partially_filled("t2", 6.0, 110.0)).unwrap();
    assert_eq!(o.filled_qty, 8.0);
    // (100*2 + 110*6) / 8 = 860 / 8 = 107.5
    assert_eq!(
        o.average_fill_price, 107.5,
        "VWAP must weight by quantity, not average prices flat"
    );

    o.apply(&filled("t3", 2.0, 90.0)).unwrap();
    assert_eq!(o.filled_qty, 10.0, "filled_qty is the running SUM of fill quantities");
    // (107.5*8 + 90*2) / 10 = (860 + 180) / 10 = 1040 / 10 = 104.0
    assert_eq!(o.average_fill_price, 104.0);
    assert_eq!(o.status, OrderStatus::Filled);
}

/// An INVERSE contract's fill VWAP is harmonic (`ContractValue::average_price`): a Deribit
/// `BTC-PERPETUAL` buy of 2,000 USD filled 1,000 at 50,000 and 1,000 at 60,000 paid 54,545.45 a coin,
/// not 55,000 (the arithmetic mean this fold booked before). A first fill is its own price exactly;
/// a linear contract at any unit keeps the arithmetic VWAP, bit for bit.
#[test]
fn an_inverse_orders_fill_vwap_is_harmonic_and_a_linear_one_is_unchanged() {
    let deribit = ContractValue::inverse(1.0);
    let mut o = limit_order(2_000.0);
    o.apply(&submitted()).unwrap();
    o.apply(&accepted()).unwrap();
    o.apply_valued(&partially_filled("t1", 1_000.0, 50_000.0), deribit).unwrap();
    assert_eq!(o.average_fill_price, 50_000.0, "a first fill is its own price");
    o.apply_valued(&filled("t2", 1_000.0, 60_000.0), deribit).unwrap();
    assert!((o.average_fill_price - 54_545.454_545).abs() < 1e-6, "{}", o.average_fill_price);
    assert_eq!(o.status, OrderStatus::Filled);

    let (mut plain, mut es) = (limit_order(10.0), limit_order(10.0));
    for o in [&mut plain, &mut es] {
        o.apply(&submitted()).unwrap();
        o.apply(&accepted()).unwrap();
    }
    for (tid, q, px) in [("t1", 2.0, 100.1), ("t2", 6.0, 110.3), ("t3", 2.0, 90.7)] {
        plain.apply(&partially_filled(tid, q, px)).unwrap();
        es.apply_valued(&partially_filled(tid, q, px), ContractValue::linear(50.0)).unwrap();
    }
    assert_eq!(
        plain.average_fill_price.to_bits(),
        es.average_fill_price.to_bits(),
        "a linear unit never averages"
    );
}

/// The `if new > 0.0` guard's own boundary: a fill that leaves `filled_qty` at exactly zero
/// -- `previous == 0.0` and `fill.last_qty == 0.0` so `new == 0.0` -- must SKIP the VWAP update
/// rather than divide zero by zero. `>` correctly skips it (`0.0 > 0.0` is false); a mutant
/// `>=` would execute the division, compute `0.0 / 0.0 == NaN`, and poison `average_fill_price`
/// forever after (NaN contaminates every later `average_fill_price * previous` term).
#[test]
fn zero_quantity_fill_at_zero_prior_quantity_does_not_divide_by_zero() {
    let mut o = limit_order(10.0);
    o.apply(&submitted()).unwrap();
    o.apply(&accepted()).unwrap();

    o.apply(&partially_filled("t1", 0.0, 555.0)).unwrap();

    assert_eq!(o.filled_qty, 0.0, "a zero-qty fill folds no quantity");
    assert_eq!(
        o.average_fill_price, 0.0,
        "the guard must skip the update, not divide 0.0/0.0 into NaN"
    );
    assert!(!o.average_fill_price.is_nan(), "average_fill_price must never become NaN");
}

/// An accepted BUY of 10 units of a 2:-1 combo (A +2, B -1).
fn combo_order() -> OrderRecord {
    let spec = crate::OrderCombo {
        venue: "v".into(),
        side: 1,
        qty: 10.0,
        legs: vec![
            crate::OrderComboLeg { symbol: "A".into(), ratio: 2, limit: None },
            crate::OrderComboLeg { symbol: "B".into(), ratio: -1, limit: None },
        ],
        net_limit: Some(100.0),
        time_in_force: crate::TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    };
    let mut o = OrderRecord::new(crate::build_combo(&spec, "c").expect("a valid spec"));
    o.apply(&submitted()).unwrap();
    o.apply(&accepted()).unwrap();
    o
}

/// One fill row of the combo on `symbol`, wrapped as the venue's state word says.
fn row(tid: &'static str, symbol: &'static str, qty: f64, px: f64, filled_word: bool) -> Event {
    let fill = FillEvent { symbol: symbol.into(), ..fill(tid, qty, px) };
    let order_client_id = "c".into();
    if filled_word {
        Event::OrderFilled(OrderFilled { order_client_id, fill, timestamp: 0 })
    } else {
        Event::OrderPartiallyFilled(OrderPartiallyFilled { order_client_id, fill, timestamp: 0 })
    }
}

/// DERIBIT's shape: every leg row says "filled" (its own child order's state), in any leg order, plus a row under
/// the combo instrument. `filled_qty` is the units EVERY leg executed, `average_fill_price` the net per unit
/// (`2 x A's VWAP - B's VWAP`); a leg's "filled" closes nothing until every leg is complete.
#[test]
fn a_combo_counts_the_units_every_leg_executed_at_the_net_and_a_legs_filled_closes_nothing() {
    let mut o = combo_order();
    o.apply(&row("b1", "B", 3.0, 90.0, true)).unwrap();
    assert_eq!(
        (o.status, o.filled_qty, o.average_fill_price),
        (OrderStatus::PartiallyFilled, 0.0, 0.0)
    );
    o.apply(&row("a1", "A", 6.0, 100.0, true)).unwrap();
    assert_eq!(
        (o.status, o.filled_qty, o.average_fill_price),
        (OrderStatus::PartiallyFilled, 3.0, 110.0)
    );
    o.apply(&row("n1", "A_B", 3.0, 110.0, false)).unwrap();
    assert_eq!((o.filled_qty, o.average_fill_price), (3.0, 110.0), "the net print counts nothing");
    // the second execution in the other leg order: A's 7 units wait for B's
    o.apply(&row("a2", "A", 14.0, 105.0, true)).unwrap();
    assert_eq!(
        (o.status, o.filled_qty, o.average_fill_price),
        (OrderStatus::PartiallyFilled, 3.0, 117.0)
    );
    o.apply(&row("b2", "B", 7.0, 95.0, true)).unwrap();
    // A: 2 070 / 20 = 103.5, B: 935 / 10 = 93.5
    assert_eq!((o.status, o.filled_qty, o.average_fill_price), (OrderStatus::Filled, 10.0, 113.5));
    assert!(o.apply(&row("n2", "A_B", 7.0, 113.5, true)).is_err(), "the print finds it closed");
}

/// A combo leg averages by its OWN contract value, the one each row's symbol is folded with
/// (`ContractValue::add_quantity_price`): a deribit spread of coin-margined futures pays leg A's harmonic
/// 54,545.45 for 10 at 50,000 and 10 at 60,000, so the net is `2 x 54,545.45 - 49,000`, not the
/// arithmetic `2 x 55,000 - 49,000` this fold booked before. A linear leg at any unit keeps
/// `Σ qty x px`, bit for bit.
#[test]
fn a_combo_leg_on_an_inverse_contract_averages_harmonically_and_a_linear_one_is_unchanged() {
    let deribit = ContractValue::inverse(1.0);
    let mut o = combo_order();
    o.apply_valued(&row("a1", "A", 10.0, 50_000.0, false), deribit).unwrap();
    o.apply_valued(&row("a2", "A", 10.0, 60_000.0, false), deribit).unwrap();
    o.apply_valued(&row("b1", "B", 10.0, 49_000.0, true), deribit).unwrap();
    let harmonic = 20.0 / (10.0 / 50_000.0 + 10.0 / 60_000.0);
    assert!(
        (o.average_fill_price - (2.0 * harmonic - 49_000.0)).abs() < 1e-6,
        "{}",
        o.average_fill_price
    );
    assert!((o.average_fill_price - 61_000.0).abs() > 900.0, "never the arithmetic net");
    assert_eq!((o.status, o.filled_qty), (OrderStatus::Filled, 10.0));

    let rows = [
        row("b1", "B", 3.0, 90.3, true),
        row("a1", "A", 6.0, 100.7, true),
        row("a2", "A", 14.0, 105.1, true),
        row("b2", "B", 7.0, 95.9, true),
    ];
    let (mut plain, mut unit) = (combo_order(), combo_order());
    for r in &rows {
        plain.apply(r).unwrap();
        unit.apply_valued(r, ContractValue::linear(50.0)).unwrap();
    }
    assert_eq!(plain.combo_leg_fills, unit.combo_leg_fills, "a linear unit never averages");
    assert_eq!(plain.average_fill_price.to_bits(), unit.average_fill_price.to_bits());
    let a: f64 = 6.0 * 100.7 + 14.0 * 105.1;
    assert_eq!(plain.combo_leg_fills[0][1].to_bits(), a.to_bits(), "Σ qty x px, as before");
}

/// The combo order's OWN "filled" (a row on no leg: deribit's combo-instrument row) closes it at `qty`.
#[test]
fn a_combo_level_filled_closes_the_combo_at_its_quantity() {
    let mut o = combo_order();
    o.apply(&row("a", "A", 6.0, 100.0, false)).unwrap();
    o.apply(&row("b", "B", 3.0, 90.0, false)).unwrap();
    o.apply(&row("n", "A_B", 10.0, 110.0, true)).unwrap();
    assert_eq!((o.status, o.filled_qty), (OrderStatus::Filled, 10.0));
}

/// PAPER's shape: one row per leg, the last wrapped `OrderFilled`; it lands with every leg complete.
#[test]
fn a_combo_whose_legs_are_complete_is_closed_by_its_last_legs_filled() {
    let mut o = combo_order();
    o.apply(&row("a", "A", 20.0, 100.0, false)).unwrap();
    o.apply(&row("b", "B", 10.0, 90.0, true)).unwrap();
    assert_eq!((o.status, o.filled_qty, o.average_fill_price), (OrderStatus::Filled, 10.0, 110.0));
}
