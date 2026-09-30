//! The status colours (design system spec §3.2) — ok, warning, error, info and muted. They are the
//! one set of colours that is the SAME in every theme: a theme recolours its background, its
//! neutrals and its accent, and must never recolour "connected". So they are not theme tokens, and
//! none of them is the accent (spec §2: "Connected" is the status green, never the accent).
//!
//! They lived in `palette`'s `status` module until design-system step 7 moved them here: `palette`
//! holds Graphite's values and goes once its last caller follows the chosen theme, while these
//! outlive it (spec §9 point 7, owner 2026-09-28). The names are colour ROLES, not connection
//! states — each paints more than one thing (a live link, a credential mark, an inline error, an
//! armed control segment).
//!
//! # Why one set — two surfaces, one state, near-identical-but-different colours
//!
//! The bottom status strip (`crates/vike-app-core/src/ui/status_dot.rs`'s `dot_color_for`) and the
//! Connections tool's live-feed cell (`crates/vike-connections/src/view.rs`'s `venue_detail`)
//! classify the SAME `crates/vike-model/src/feed_status.rs`'s `ConnectionState`, and they used to
//! paint it from two private colour sets a few points apart — one feed, two panels, two ambers:
//!
//! | state | the strip carried | the Connections tool carried |
//! |---|---|---|
//! | `Connected` | the app accent | the app accent |
//! | `Connecting` | `(230,180,40)` | `(224,176,62)` |
//! | `Error` | `(248,82,73)` | `(224,96,96)` |
//! | `Disconnected` / `Unknown` | `from_gray(120)` | `from_gray(110)` |
//!
//! The strip's set won, and not by taste: two of its colours were already canonical constants.
//! `the_superseded_connections_colours_are_gone` keeps the Connections tool's near-copies from
//! coming back. On 2026-09-28 "connected" and "error" stopped being the app accent and the market
//! "down" red: a theme binds its accent, and an alias would have repainted "connected" cyan, violet
//! or amber along with it.

use egui::Color32;

/// A healthy state — a live link, a configured credential, a saved write. `#3FBF6E`.
pub const OK: Color32 = Color32::from_rgb(63, 191, 110);
/// Not settled yet — a link dialling or reconnecting, an armed remote-control segment, an account
/// named but not yet saved, a store nobody could open. `#E6B428`.
pub const WARNING: Color32 = Color32::from_rgb(230, 180, 40);
/// A fault — a failed link, a refused write, an inline error. `#F0524A`.
pub const ERROR: Color32 = Color32::from_rgb(240, 82, 74);
/// Information — egui's hyperlink colour. `#57A5FF`.
pub const INFO: Color32 = Color32::from_rgb(87, 165, 255);
/// Nothing live to show — `Disconnected` and `Unknown` alike (only a label tells them apart), an
/// unset credential. Grey 120 is a MARK's colour and sits below the 4.5:1 floor for text.
pub const MUTED: Color32 = Color32::from_gray(120);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::{MarketColors, MarketId};
    use crate::theme::{Theme, ThemeId};

    fn rgb(c: Color32) -> (u8, u8, u8) {
        let [r, g, b, _] = c.to_array();
        (r, g, b)
    }

    const ALL: [Color32; 5] = [OK, WARNING, ERROR, INFO, MUTED];

    /// Spec §3.2's values, exactly: moving them out of `palette` changed no pixel.
    #[test]
    fn the_status_colours_are_the_specs() {
        assert_eq!(rgb(OK), (0x3F, 0xBF, 0x6E), "ok #3FBF6E");
        assert_eq!(rgb(WARNING), (0xE6, 0xB4, 0x28), "warning #E6B428");
        assert_eq!(rgb(ERROR), (0xF0, 0x52, 0x4A), "error #F0524A");
        assert_eq!(rgb(INFO), (0x57, 0xA5, 0xFF), "info #57A5FF");
        assert_eq!(rgb(MUTED), (120, 120, 120), "muted: grey 120");
        for c in ALL {
            assert_eq!(c.to_array()[3], 255, "status colours are opaque");
        }
    }

    /// Fixed in every theme — so none may be a theme's accent or caption grey, which move with the
    /// theme, or a market set's up or down, which move with the market-colour setting.
    #[test]
    fn no_status_colour_is_a_theme_or_market_colour() {
        for c in ALL {
            for id in ThemeId::ALL {
                let t = Theme::of(id);
                assert_ne!(c, t.accent, "{c:?} is {id:?}'s accent");
                assert_ne!(c, t.text3, "{c:?} is {id:?}'s caption grey");
            }
            for m in MarketId::ALL {
                let mc = MarketColors::of(m);
                assert_ne!(c, mc.up, "{c:?} is {m:?}'s up");
                assert_ne!(c, mc.down, "{c:?} is {m:?}'s down");
            }
        }
        // `palette::WARN` is a far more saturated orange (a missing-price badge); the two were never
        // one colour. This line goes with `palette`.
        assert_ne!(WARNING, crate::palette::WARN);
    }

    /// The Connections tool's three private near-copies (the module doc's table). "Somebody re-adds
    /// the old amber" and "somebody edits the new one" are the same diff; this tells them apart.
    #[test]
    fn the_superseded_connections_colours_are_gone() {
        assert_ne!(rgb(WARNING), (224, 176, 62), "the Connections tool's old amber");
        assert_ne!(rgb(ERROR), (224, 96, 96), "the old red, a near-copy of the market down");
        assert_ne!(rgb(MUTED), (110, 110, 110), "the Connections tool's old grey");
    }
}
