//! The Price-to-Beat header — the one-glance state of a Polymarket up/down window.
//! RUST-NATIVE (no Python twin — the oracle app has no cockpit).
//!
//! A scalper of a 5-minute up/down market lives off four numbers: the REFERENCE price the window
//! opened at (the "price to beat"), the live spot, the signed distance between them (green when
//! spot is above the reference ⇒ Up is winning, red below), and the time left. This header renders
//! them in that visual hierarchy — the reference loudest, a big MM:SS countdown escalating to red
//! in the final ≤10s — plus the current Up/Down odds.
//!
//! Stateless and near-actionless (it is a readout): [`draw`] takes only borrowed [`PtbInputs`] and
//! paints. The countdown reuses [`crate::chain::fmt_countdown`] so every cockpit clock formats
//! identically. The single arithmetic bit ([`ptb_delta`]) is pure and unit-tested.

use egui::{RichText, Sense, Stroke, Vec2};
// The shared dark trading-terminal palette (one pinned home; this file used to hand-copy the
// const block — GUI audit F3).
use vike_ui_theme::palette::trading::{ACCENT, DOWN, FAINT, MUTED, PANEL2, RULE, TXT, UP, URGENT};

const HEADER_H: f32 = 44.0;
/// The countdown escalates from amber to red inside this many ms of the resolution.
const URGENT_MS: i64 = 10_000;

/// Everything the header needs to render one frame. Borrowed / by-value — the app owns the storage.
#[derive(Clone, Copy, Debug)]
pub struct PtbInputs {
    /// the window-open REFERENCE price (the "price to beat") — the loudest element
    pub reference_price: f64,
    /// the live spot price
    pub spot: f64,
    /// window resolution wall-clock, epoch ms (drives the countdown)
    pub resolution_ts: i64,
    /// current wall-clock, epoch ms
    pub now_ms: i64,
    /// current Up/YES odds (0..1), if known
    pub up_price: Option<f64>,
    /// current Down/NO odds (0..1), if known
    pub dn_price: Option<f64>,
}

// ---------------------------------------------------------------------------
// Pure logic (unit-tested)
// ---------------------------------------------------------------------------

/// Signed distance of spot from the reference (`spot − reference`): positive ⇒ Up is winning
/// (green), negative ⇒ Down (red).
pub fn ptb_delta(spot: f64, reference: f64) -> f64 {
    spot - reference
}

/// Is a remaining duration in the final escalation stretch (the countdown turns red)? True over
/// the last [`URGENT_MS`] down to and including the boundary; an already-past value is not urgent.
fn is_urgent(ms_remaining: i64) -> bool {
    (0..=URGENT_MS).contains(&ms_remaining)
}

/// Fixed two-decimal price. KEPT local, not `vike_ui_theme::fmt`: `fmt_thousands` adds comma
/// grouping ("60,250.50" vs our "60250.50") and `fmt_compact` compacts ("60.25K") — pinned in the
/// tests.
fn fmt_px(p: f64) -> String {
    format!("{p:.2}")
}

fn fmt_signed(d: f64) -> String {
    format!("{d:+.2}")
}

fn fmt_odds(lbl: &str, p: Option<f64>) -> String {
    match p {
        Some(v) => format!("{lbl} {v:.2}"),
        None => format!("{lbl} —"),
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draw one frame of the Price-to-Beat header. Pure readout — no state, no actions.
pub fn draw(ui: &mut egui::Ui, inputs: &PtbInputs) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), HEADER_H), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, PANEL2);
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, RULE));

    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(10.0, 4.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 10.0;

    // reference (price to beat) — the loudest element
    child.label(RichText::new("BEAT").monospace().size(11.0).color(FAINT));
    child.label(
        RichText::new(fmt_px(inputs.reference_price)).monospace().size(22.0).strong().color(TXT),
    );

    // live spot + signed Δ (green up / red down)
    let delta = ptb_delta(inputs.spot, inputs.reference_price);
    let dcol = if delta > 0.0 {
        UP
    } else if delta < 0.0 {
        DOWN
    } else {
        MUTED
    };
    child.label(RichText::new(fmt_px(inputs.spot)).monospace().size(15.0).color(dcol));
    child.label(RichText::new(fmt_signed(delta)).monospace().size(13.0).color(dcol));

    // Up/Down odds
    child.add_space(4.0);
    child.label(RichText::new(fmt_odds("Up", inputs.up_price)).monospace().size(12.0).color(UP));
    child.label(RichText::new(fmt_odds("Dn", inputs.dn_price)).monospace().size(12.0).color(DOWN));

    // big MM:SS countdown, right-aligned, escalating to red in the final ≤10s
    let remaining = inputs.resolution_ts - inputs.now_ms;
    let ccol = if is_urgent(remaining) { URGENT } else { ACCENT };
    child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |child| {
        child.label(
            RichText::new(crate::chain::fmt_countdown(remaining))
                .monospace()
                .size(22.0)
                .strong()
                .color(ccol),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ptb_delta_signs() {
        assert!(ptb_delta(101.0, 100.0) > 0.0); // spot above reference ⇒ Up winning
        assert!(ptb_delta(99.0, 100.0) < 0.0); // spot below ⇒ Down
        assert!((ptb_delta(100.0, 100.0)).abs() < 1e-12); // dead on
        assert!((ptb_delta(60_250.5, 60_000.0) - 250.5).abs() < 1e-9);
    }

    /// Pins the rendered strings, INCLUDING the divergences that keep this formatter local
    /// instead of swapping to a `vike_ui_theme::fmt` helper (GUI audit F7 — a swap would change
    /// rendered text).
    #[test]
    fn fmt_px_is_fixed_two_decimals_ungrouped() {
        assert_eq!(fmt_px(0.0), "0.00");
        assert_eq!(fmt_px(0.62), "0.62");
        assert_eq!(fmt_px(1234.5), "1234.50"); // no comma — fmt_thousands prints "1,234.50"
        assert_eq!(fmt_px(60_250.5), "60250.50"); // no compaction — fmt_compact prints "60.25K"
    }

    #[test]
    fn fmt_signed_keeps_the_sign() {
        assert_eq!(fmt_signed(12.5), "+12.50");
        assert_eq!(fmt_signed(-3.0), "-3.00");
        assert_eq!(fmt_signed(0.0), "+0.00");
    }

    #[test]
    fn fmt_odds_present_and_absent() {
        assert_eq!(fmt_odds("Up", Some(0.62)), "Up 0.62");
        assert_eq!(fmt_odds("Dn", None), "Dn —");
    }

    #[test]
    fn urgent_only_in_final_ten_seconds() {
        assert!(is_urgent(URGENT_MS)); // exactly 10s → urgent
        assert!(is_urgent(0)); // at the boundary → still urgent
        assert!(!is_urgent(URGENT_MS + 1)); // 10.001s → not yet
        assert!(!is_urgent(-1)); // already resolved → not urgent
    }
}
