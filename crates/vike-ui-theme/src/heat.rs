//! The liquidity heatmap's colour ramp — it painted the DOM's Elite time × price strip. ONE ramp,
//! the same in every theme and every market-colour set: it says how MUCH resting liquidity a cell
//! held, which is neither up nor down. It moved here from `crates/vike-panels/src/dom.rs` value for
//! value, so that every colour the app paints is spelled in this crate
//! (`crates/vike-ops/tests/gui/ui_literal_ratchet.rs`).
//!
//! ⚠ **Nothing calls it today, and it is kept on purpose.** The DOM's heatmap was deleted with the
//! DOM window; the heatmap returns as a layer of the Trade window's tick chart pane (the Trade
//! window spec's §3.5), which the Trade window plan defers to its follow-up plan. That layer is this
//! ramp's next caller.

use egui::Color32;

// The ramp's two ends and its opacities: GENERATED from `crates/vike-ui-theme/ui-theme.toml`
// (`[heat]`), each with its doc comment.
include!("heat_tokens.rs");

/// `t` in `0.0..=1.0` → the owner's v3 design's heatmap: cyan below the middle, amber above it,
/// more opaque the fuller the cell (opacity 0.15 at `0`, 0.6 at `1`).
///
/// `#[inline]`: a heatmap calls it once per CELL.
#[inline]
pub fn ramp(t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let (r, g, b) = if t < 0.5 { COOL } else { WARM };
    let a = ((ALPHA_MIN + ALPHA_SPAN * t) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ramp is the design's: cyan in the lighter half, amber in the heavier, opacity from 0.15
    /// to 0.6 (the design's own cells measured 0.16–0.39 in cyan and 0.39–0.56 in amber).
    #[test]
    fn the_ramp_is_the_designs_cyan_to_amber() {
        assert_eq!(ramp(0.0), Color32::from_rgba_unmultiplied(53, 201, 242, 38));
        assert_eq!(ramp(0.49), Color32::from_rgba_unmultiplied(53, 201, 242, 94));
        assert_eq!(ramp(0.5), Color32::from_rgba_unmultiplied(242, 181, 58, 96));
        assert_eq!(ramp(1.0), Color32::from_rgba_unmultiplied(242, 181, 58, 153));
        assert_eq!(ramp(-1.0), ramp(0.0), "clamped");
        assert_eq!(ramp(7.0), ramp(1.0), "clamped");
    }

    /// Hotter is never fainter: a fuller cell is at least as opaque as an emptier one.
    #[test]
    fn hotter_is_never_fainter() {
        let a: Vec<u8> = (0..=10).map(|i| ramp(i as f32 / 10.0).a()).collect();
        assert!(a.windows(2).all(|w| w[0] <= w[1]), "{a:?}");
    }
}
