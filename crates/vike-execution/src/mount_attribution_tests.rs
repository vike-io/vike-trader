use super::*;
use vike_model::ContractValue;
use vike_model::accounts::account_keys::AccountLabel;

/// The fold as `vike_core`'s `dispatch_applied_fills` spelled it inline before it moved here.
fn fold_as_it_was(
    attr: &mut MountAttribution,
    side: i32,
    qty: f64,
    price: f64,
    value: ContractValue,
    fee: f64,
) {
    let out =
        vike_model::compute_fill_value(attr.size, attr.average_price, side, qty, price, value);
    attr.size = out.new_size;
    attr.average_price = out.new_average_price;
    if out.closing_qty > 0.0 {
        attr.realized_pnl += out.realized_pnl;
    }
    attr.fees_paid += fee;
}

fn bits(a: &MountAttribution) -> [u64; 4] {
    [a.size, a.average_price, a.realized_pnl, a.fees_paid].map(f64::to_bits)
}

/// `(side, qty, price, fee)` folded in order, each step compared to the inline fold bit for bit.
fn fold_both(value: ContractValue, fills: &[(i32, f64, f64, f64)]) -> MountAttribution {
    let (mut moved, mut inline) = (MountAttribution::default(), MountAttribution::default());
    for (step, &(side, qty, price, fee)) in fills.iter().enumerate() {
        moved.fold_fill(side, qty, price, value, fee);
        fold_as_it_was(&mut inline, side, qty, price, value, fee);
        assert_eq!(bits(&moved), bits(&inline), "step {step}: {moved:?} vs {inline:?}");
    }
    moved
}

#[test]
fn fold_fill_is_the_inline_fold_bit_for_bit_on_a_linear_contract() {
    // open, add, flip through flat, close; the fee of the third is a maker rebate
    let row = fold_both(
        ContractValue::linear(1.0),
        &[
            (1, 1.0, 60_000.0, 6.0),
            (1, 2.0, 61_000.0, 12.2),
            (-1, 5.0, 62_000.0, -3.1),
            (1, 2.0, 59_000.0, 1.18),
        ],
    );
    assert_eq!(row.size, 0.0, "the close leaves it flat");
    assert!(row.realized_pnl > 0.0 && row.legs.is_empty(), "{row:?}");
}

#[test]
fn fold_fill_is_the_inline_fold_bit_for_bit_on_an_inverse_contract() {
    // open short, add, reduce
    let row = fold_both(
        ContractValue::inverse(100.0),
        &[(-1, 10.0, 2_400.0, 0.5), (-1, 5.0, 2_500.0, 0.25), (1, 7.0, 2_300.0, 0.35)],
    );
    assert_eq!(row.size, -8.0, "the reduce leaves the rest open");
}

/// `valued_at` is `ledger_row_valuation`'s priced arm to the bit, and a flat row reads no price.
#[test]
fn valued_at_is_the_row_valuation_it_came_from() {
    let as_it_was = |row: &MountAttribution, px: f64, value: ContractValue| {
        (
            row.realized_pnl - row.fees_paid,
            value.unrealized_pnl(row.average_price, px, row.size),
            value.gross_notional(row.size, px),
        )
    };
    let tuple_bits = |(r, u, n): (f64, f64, f64)| [r, u, n].map(f64::to_bits);
    for value in
        [ContractValue::linear(1.0), ContractValue::linear(0.01), ContractValue::inverse(100.0)]
    {
        let mut row = MountAttribution::default();
        row.fold_fill(1, 3.0, 2_000.0, value, 0.7);
        row.fold_fill(-1, 1.0, 2_100.0, value, 0.3);
        for px in [1_900.0, 2_000.0, 2_345.67] {
            assert_eq!(
                tuple_bits(row.valued_at(px, value)),
                tuple_bits(as_it_was(&row, px, value))
            );
        }
    }
    let mut flat = MountAttribution { realized_pnl: 12.5, fees_paid: 2.0, ..Default::default() };
    flat.leg_mut("bybit", None, "ETHUSDT").fold_fill(
        1,
        1.0,
        3_000.0,
        ContractValue::default(),
        0.0,
    );
    let (r, u, n) = flat.valued_at(f64::NAN, ContractValue::inverse(100.0));
    assert_eq!([r, u, n].map(f64::to_bits), [10.5, 0.0, 0.0].map(f64::to_bits), "own row only");
}

#[test]
fn leg_mut_keeps_two_accounts_of_one_venue_apart_and_finds_a_row_again() {
    let alt = AccountLabel::parse("ALT").unwrap();
    let mut a = MountAttribution::default();
    a.leg_mut("bybit", None, "BTCUSDT").size = 1.0;
    a.leg_mut("bybit", Some(&alt), "BTCUSDT").size = -2.0;
    a.leg_mut("bybit", None, "BTCUSDT").size += 0.5;
    assert_eq!(a.legs.len(), 2, "{a:?}");
    assert_eq!((a.legs[0].account.as_ref(), a.legs[0].attr.size), (None, 1.5));
    assert_eq!((a.legs[1].account.as_ref(), a.legs[1].attr.size), (Some(&alt), -2.0));
    assert_eq!(a.size, 0.0, "the own row is untouched");
}

#[test]
fn put_row_puts_a_keyless_row_as_the_own_one_and_a_keyed_row_under_its_key() {
    let row = |size: f64| MountAttribution { size, ..Default::default() };
    let mut a = MountAttribution::default();
    a.put_row(Some("bybit".into()), Some("ALT".into()), Some("ETHUSDT".into()), row(2.0));
    a.put_row(None, None, None, row(1.0));
    a.put_row(Some("bybit".into()), Some("DEFAULT".into()), Some("ETHUSDT".into()), row(3.0));
    assert_eq!(a.size, 1.0, "the keyless row is the own one and keeps the legs read before it");
    assert_eq!(a.legs.len(), 2, "{a:?}");
    let alt = AccountLabel::parse("ALT").unwrap();
    assert_eq!(a.leg_mut("bybit", Some(&alt), "ETHUSDT").size, 2.0);
    assert_eq!(a.leg_mut("bybit", Some(&AccountLabel::Default), "ETHUSDT").size, 3.0);
    a.put_row(Some("bybit".into()), Some("ALT".into()), Some("ETHUSDT".into()), row(-4.0));
    assert_eq!((a.legs.len(), a.legs[0].attr.size), (2, -4.0), "a keyed row replaces its own");
}

#[test]
fn realized_net_adds_every_row() {
    let mut a = MountAttribution { realized_pnl: 10.0, fees_paid: 1.0, ..Default::default() };
    assert_eq!(a.realized_net(), 9.0, "no leg: the one expression");
    *a.leg_mut("okx", None, "ETHUSDT") =
        MountAttribution { realized_pnl: -4.0, fees_paid: 0.5, ..Default::default() };
    assert_eq!(a.realized_net(), 9.0 - 4.5);
}
