use super::*;

/// A 100-long axis and a 20-long chip: the centre may sit anywhere in `10..=90` when it must stay inside.
const AXIS: (f32, f32) = (0.0, 100.0);

#[test]
fn inside_keeps_the_whole_chip_in_the_axis() {
    let at = |s| chip_centre(Fit::Inside, s, 20.0, AXIS.0, AXIS.1);
    assert_eq!(at(50.0), Some(50.0));
    assert_eq!(at(-30.0), Some(10.0));
    assert_eq!(at(140.0), Some(90.0));
    // An axis shorter than the chip leaves nothing to clamp into: the chip stays on its value.
    assert_eq!(chip_centre(Fit::Inside, 4.0, 20.0, 0.0, 12.0), Some(4.0));
}

#[test]
fn pin_outside_moves_only_a_value_outside_the_axis() {
    let at = |s| chip_centre(Fit::PinOutside, s, 20.0, AXIS.0, AXIS.1);
    // On the axis the chip stays on its value, even half off the edge: it is the value's own place.
    assert_eq!(at(3.0), Some(3.0));
    assert_eq!(at(100.0), Some(100.0));
    // Off the axis it sits flush against the nearer edge, wholly inside.
    assert_eq!(at(-44.2), Some(10.0));
    assert_eq!(at(163.0), Some(90.0));
}

#[test]
fn hide_drops_the_chip_in_the_near_strip_and_past_the_far_edge() {
    let at = |s| chip_centre(Fit::Hide { strip: 26.0 }, s, 20.0, AXIS.0, AXIS.1);
    assert_eq!(at(25.9), None);
    assert_eq!(at(26.0), Some(26.0));
    assert_eq!(at(100.0), Some(100.0));
    assert_eq!(at(100.1), None);
    assert_eq!(at(-5.0), None);
}
