//! The Studio's design tokens + tiny shared widgets: ONE accent (the Studio's `WinKind` violet,
//! see vike-app's `WinKind::color`) and ONE status palette (ok/warn/err) so every pane colors the
//! same states the same way (previously two different "error" reds coexisted), plus the three
//! rendering idioms every pane shares — `section_header`, `chip`, `primary` — so the five tool
//! panes read as one product rather than five prototypes. Pure egui; no state, no I/O.

use egui::{Button, Color32, CornerRadius, Frame, Margin, Response, RichText, Ui};

/// The Studio brand accent — the same violet vike-app's `WinKind::Studio` titles bars with.
pub const ACCENT: Color32 = Color32::from_rgb(170, 130, 250);
/// Compile-clean / profit / success.
pub const OK: Color32 = Color32::from_rgb(0x4e, 0xc9, 0x4e);
/// Unsaved / caution.
pub const WARN: Color32 = Color32::from_rgb(0xe0, 0xa8, 0x30);
/// Compile error / run failure / loss. The ONE error red (kills the old second pink-red).
pub const ERR: Color32 = Color32::from_rgb(0xd9, 0x4e, 0x4e);

/// A filled accent button — the "primary action" of a pane (Run, Run Sweep, Compute, Send,
/// Apply). One per view; everything else stays a default button so the eye lands on the verb
/// that matters. Text is pinned white (not the theme's fg color) so the dimmed-violet fill keeps
/// contrast even under egui light visuals, where the default fg is near-black. The rest of this
/// module is dark-theme-first (the app ships dark-only today); a real light theme would need a
/// chip/rail-fill pass here.
pub fn primary(text: impl Into<String>) -> Button<'static> {
    Button::new(RichText::new(text.into()).color(Color32::WHITE))
        .fill(Color32::from_rgb(0x5a, 0x44, 0x9e))
}

/// The shared pane header: accent glyph + strong title, underlined by a separator. Every tool
/// pane opens with exactly this so switching rail tabs doesn't reshuffle the visual anatomy.
pub fn section_header(ui: &mut Ui, glyph: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(glyph).size(16.0).color(ACCENT));
        ui.label(RichText::new(title).strong().size(15.0));
    });
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(4.0);
}

/// A small tinted status chip (compile status, unsaved marker, OOS Sharpe). Background is the
/// status color at low alpha so the chip reads at a glance without shouting; returns the
/// `Response` so callers can hang hover text off it.
pub fn chip(ui: &mut Ui, color: Color32, text: &str) -> Response {
    Frame::new()
        .fill(color.linear_multiply(0.15))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| ui.label(RichText::new(text).size(11.0).color(color)))
        .response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status palette stays three DISTINCT colors and the accent matches the Studio's
    /// `WinKind` violet (170,130,250) — the one cross-crate color contract worth pinning.
    #[test]
    fn palette_is_distinct_and_accent_matches_winkind_violet() {
        assert_eq!(ACCENT, Color32::from_rgb(170, 130, 250));
        let all = [ACCENT, OK, WARN, ERR];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "status colors must be distinct");
            }
        }
    }
}
