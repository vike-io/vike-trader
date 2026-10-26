//! Colour arithmetic for the TESTS that derive and check the themes (`crate::theme`).
//!
//! Test-only on purpose. `relative_luminance` needs `x^2.4`, and
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` keeps
//! platform transcendentals out of production code. Production reads the themes as constants; this
//! module is how the tests prove those constants follow the rule.

use egui::Color32;

/// `a` moved toward `b` by `t` (0 = `a`, 1 = `b`), per 8-bit sRGB channel, rounded half away from
/// zero. The result is opaque.
pub(crate) fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

/// WCAG 2 relative luminance of an opaque sRGB colour.
pub(crate) fn relative_luminance(c: Color32) -> f64 {
    let lin = |v: u8| {
        let s = v as f64 / 255.0;
        if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG 2 contrast ratio between two opaque colours: symmetric, from 1.0 to 21.0.
pub(crate) fn contrast_ratio(a: Color32, b: Color32) -> f64 {
    let (x, y) = (relative_luminance(a), relative_luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

mod tests {
    use super::*;

    #[test]
    fn black_on_white_is_twenty_one_to_one_and_a_colour_on_itself_is_one() {
        assert!((contrast_ratio(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 1e-9);
        let c = Color32::from_rgb(13, 17, 23);
        assert!((contrast_ratio(c, c) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn lerp_hits_both_ends_and_rounds_half_away_from_zero() {
        let a = Color32::from_rgb(0, 10, 200);
        let b = Color32::from_rgb(1, 20, 100);
        assert_eq!(lerp(a, b, 0.0), a);
        assert_eq!(lerp(a, b, 1.0), b);
        // 0 + 1 * 0.5 = 0.5 -> 1; 10 + 10 * 0.5 = 15; 200 - 100 * 0.5 = 150.
        assert_eq!(lerp(a, b, 0.5), Color32::from_rgb(1, 15, 150));
    }
}
