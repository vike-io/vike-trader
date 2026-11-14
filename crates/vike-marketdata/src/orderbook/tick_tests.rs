//! The index -> price rule: exact for every tick that divides one unit and every other decimal
//! tick, the plain product for a tick that is neither.

use super::TickFraction;

#[test]
fn a_decimal_tick_reads_the_decimal_price_not_the_product() {
    let cent = TickFraction::new(0.01);
    assert_eq!(cent.price_of(35.0), 0.35);
    assert_ne!(35.0 * 0.01, 0.35, "the old product reads 0.35000000000000003");
    assert_eq!(cent.price_of(57.0), 0.57);
    assert_eq!(TickFraction::new(0.1).price_of(1001.0), 100.1);
    assert_eq!(TickFraction::new(0.1).price_of(3.0), 0.3);
    assert_eq!(TickFraction::new(0.001).price_of(350.0), 0.35);
    assert_eq!(TickFraction::new(0.05).price_of(60_001.0), 3000.05);
    assert_eq!(TickFraction::new(0.03).price_of(7.0), 0.21);
    assert_eq!(TickFraction::new(0.3).price_of(3.0), 0.9);
    assert_ne!(3.0 * 0.3, 0.9, "the old product reads 0.8999999999999999");
    assert_eq!(TickFraction::new(0.00000001).price_of(1234.0), 0.00001234);
    assert_eq!(cent.price_of(-35.0), -0.35);
    assert_eq!(cent.price_of(0.0), 0.0);
}

#[test]
fn binary_and_integer_ticks_read_exactly_as_before() {
    for (tick, index) in
        [(0.5, 201.0), (0.25, 4_003.0), (0.03125, 3_205.0), (5.0, 41.0), (2.5, 7.0), (1.0, 99.0)]
    {
        assert_eq!(TickFraction::new(tick).price_of(index), index * tick, "tick {tick}");
    }
    assert_eq!(TickFraction::new(0.03125).price_of(3_205.0), 3_205.0 / 32.0);
}

/// A tick a few ulps off one that divides one unit (an inferred gap, a 1/3) counts the ticks in
/// one unit, as the side-directed order snap always did.
#[test]
fn a_tick_that_divides_one_unit_counts_its_ticks_in_one_unit() {
    let inferred_gap = 100.01 - 100.0; // 0.010000000000005116
    assert_eq!(TickFraction::new(inferred_gap).price_of(9_999.0), 99.99);
    assert_eq!(TickFraction::new(1.0 / 3.0).price_of(7.0), 7.0 / 3.0);
}

#[test]
fn a_tick_that_is_neither_keeps_the_product() {
    let noisy = 0.1 + 0.2; // 0.30000000000000004: no short decimal, and 1/it is no whole number
    assert_eq!(TickFraction::new(noisy).price_of(7.0), 7.0 * noisy);
    let two_thirds = 2.0 / 3.0;
    assert_eq!(TickFraction::new(two_thirds).price_of(7.0), 7.0 * two_thirds);
    assert!(TickFraction::new(f64::NAN).price_of(7.0).is_nan());
    assert!(TickFraction::new(0.01).price_of(f64::NAN).is_nan(), "a NaN count stays NaN");
    assert!(TickFraction::new(0.0).price_of(f64::INFINITY).is_nan(), "inf × 0, as the product");
    assert_eq!(TickFraction::new(-0.5).price_of(3.0), -1.5, "a negative tick: the product");
}
