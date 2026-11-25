//! `PositionSide::signed_size`, moved with its tests from vike-app-core's `orders/dom_math.rs`.

use super::PositionSide;

// A one-way / paper position always reports position_side `Both` with direction in the SIGN of
// size; the window must NOT strip that sign (the bug: a short rendered as a long, P/L inverted, and
// Close/Reverse picked the wrong side).
#[test]
fn one_way_both_keeps_the_signed_size() {
    assert_eq!(PositionSide::Both.signed_size(-0.01), -0.01); // short stays short
    assert_eq!(PositionSide::Both.signed_size(0.01), 0.01); // long stays long
    assert_eq!(PositionSide::Both.signed_size(0.0), 0.0); // flat
}

#[test]
fn hedge_mode_side_overrides_the_magnitude() {
    // hedge perps carry a magnitude size + direction in the side
    assert_eq!(PositionSide::Short.signed_size(0.01), -0.01);
    assert_eq!(PositionSide::Long.signed_size(0.01), 0.01);
    // a magnitude that arrives signed is still read by the side
    assert_eq!(PositionSide::Short.signed_size(-0.01), -0.01);
    assert_eq!(PositionSide::Long.signed_size(-0.01), 0.01);
}

// The Close/Reverse exit side is derived from the signed size: long closes by SELL, short by BUY.
#[test]
fn exit_side_is_opposite_of_the_held_direction() {
    let exit = crate::closing_side;
    assert_eq!(exit(PositionSide::Both.signed_size(-0.01)), 1); // short -> BUY to close
    assert_eq!(exit(PositionSide::Both.signed_size(0.01)), -1); // long -> SELL to close
}
