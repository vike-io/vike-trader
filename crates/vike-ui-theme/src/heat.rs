//! The liquidity heatmap's colour ramp — it painted the DOM's Elite time × price strip. ONE ramp,
//! the same in every theme and every market-colour set: it says how MUCH resting liquidity a cell
//! held, which is neither up nor down. It moved here from `crates/vike-panels/src/dom.rs` value for
//! value, so that every colour the app paints is spelled in this crate
//! (`crates/vike-ops/tests/ui_literal_ratchet.rs`).
//!
//! ⚠ **Nothing calls it today, and it is kept on purpose.** The DOM's heatmap was deleted with the
//! DOM window; the heatmap returns as a layer of the Trade window's tick chart pane (the Trade
//! window spec's §3.5), which the Trade window plan defers to its follow-up plan. That layer is this
//! ramp's next caller.

use egui::Color32;

/// `t` in `0.0..=1.0` → cold, nearly transparent violet → amber → hot orange-white.
///
/// `#[inline]`: a heatmap calls it once per CELL — the DOM's called it up to 180 × its visible rows
/// every frame.
#[inline]
pub fn ramp(t: f32) -> Color32 {
    let r = (20.0 + t * 235.0) as u8;
    let g = (15.0 + t * 150.0) as u8;
    let b = (34.0 + t * 18.0) as u8;
    let a = (40.0 + t * 200.0).min(255.0) as u8;
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ramp IS the DOM's old `heat_color`, value for value, at its two ends and its middle.
    #[test]
    fn the_ramp_is_the_doms_old_heat_colour() {
        assert_eq!(ramp(0.0), Color32::from_rgba_unmultiplied(20, 15, 34, 40));
        assert_eq!(ramp(0.5), Color32::from_rgba_unmultiplied(137, 90, 43, 140));
        assert_eq!(ramp(1.0), Color32::from_rgba_unmultiplied(255, 165, 52, 240));
    }

    /// Hotter is never fainter: a fuller cell is at least as opaque as an emptier one.
    #[test]
    fn hotter_is_never_fainter() {
        let a: Vec<u8> = (0..=10).map(|i| ramp(i as f32 / 10.0).a()).collect();
        assert!(a.windows(2).all(|w| w[0] <= w[1]), "{a:?}");
    }
}
