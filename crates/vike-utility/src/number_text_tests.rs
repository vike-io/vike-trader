use super::*;

/// Dropping noise is not the same as dropping precision: a value whose digits carry information
/// keeps all of them, out to [`MAX_RENDER_DECIMALS`].
#[test]
fn rendering_drops_noise_and_nothing_else() {
    for (v, expect) in [
        (1.2000000000000002_f64, "1.2"),
        (0.1 + 0.2, "0.3"),
        (3.0, "3"),
        (42.5, "42.5"),
        (0.0001234, "0.0001234"),
        (1234.56789, "1234.56789"),
        (-0.30000000000000004, "-0.3"),
        (0.0, "0"),
        (1e-10, "0.0000000001"),
        (1e20, "100000000000000000000"),
    ] {
        assert_eq!(format_number(v), expect, "format_number({v})");
    }
    // Non-finite falls through to Display rather than looping to the precision cap.
    assert_eq!(format_number(f64::NAN), "NaN");
    assert_eq!(format_number(f64::INFINITY), "inf");
}
