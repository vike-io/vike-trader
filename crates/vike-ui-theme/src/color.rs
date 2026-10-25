//! Colour MATH — the one place a GUI crate turns stored bytes into a colour, or derives one colour
//! from another (design system spec §7: raw colours and their derivations live in this crate, and
//! `crates/vike-ops/tests/gui/ui_literal_ratchet.rs` counts every one outside it).
//!
//! The colours these act on are tokens, or colours a user picked (a chart's edited candle colour).
//! The NUMBERS a caller passes — an alpha, a strength — are paint parameters, like a line width,
//! and stay named beside the painter that uses them.

use egui::Color32;

/// The tint that does not tint: an image drawn with it keeps its own colours (`Painter::image`'s `tint`). Not a
/// design colour; the name is the decision (an image is meant to be shown as it is).
pub const UNTINTED: Color32 = Color32::WHITE;

/// A colour stored as `[r, g, b]` — the form a chart window persists a colour the user picked.
pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Opaque `c` at alpha `a` (unmultiplied): depth bars, washes, the faint fill under a line. This
/// was `palette::trading::dim`, the trading palette's helper; it is the job of every palette.
pub fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// `c` at `strength` (0–1) of itself, in egui's gamma space: volume bars, a ghost crosshair.
pub fn faded(c: Color32, strength: f32) -> Color32 {
    c.gamma_multiply(strength)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `with_alpha` is the retired `palette::trading::dim`, byte for byte, at every alpha the
    /// cockpit ladder and the DOM paint (28, 30, 46, 48, the 30..=150 depth ramp) and the u8 edges.
    #[test]
    fn with_alpha_is_the_retired_dim() {
        for (r, g, b) in [(46, 189, 133), (246, 70, 93), (64, 186, 80)] {
            for a in [0u8, 8, 28, 30, 46, 48, 90, 150, 255] {
                assert_eq!(
                    with_alpha(Color32::from_rgb(r, g, b), a).to_array(),
                    Color32::from_rgba_unmultiplied(r, g, b, a).to_array(),
                    "({r}, {g}, {b}) at {a}"
                );
            }
        }
    }

    /// `faded` is egui's gamma-space fade — what the volume pane and the ghost crosshair used.
    #[test]
    fn faded_is_eguis_gamma_space_fade() {
        let c = Color32::from_rgb(64, 186, 80);
        for s in [0.0, 0.35, 0.4, 0.7, 1.0] {
            assert_eq!(faded(c, s), c.gamma_multiply(s), "{s}");
        }
    }

    #[test]
    fn rgb_reads_stored_bytes_as_an_opaque_colour() {
        assert_eq!(rgb([13, 17, 23]), Color32::from_rgb(13, 17, 23));
        assert_eq!(rgb([13, 17, 23]).a(), 255);
    }
}
