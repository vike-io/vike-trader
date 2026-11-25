//! The Price-to-Beat header — the one-glance state of a Polymarket up/down window.
//! RUST-NATIVE (no Python twin — the oracle app has no cockpit).
//!
//! A scalper of a 5-minute up/down market lives off four numbers: the REFERENCE price the window
//! opened at (the "price to beat"), the live spot, the signed distance between them (in the
//! market's up colour when spot is above the reference ⇒ Up is winning, its down colour below), and
//! the time left. This header renders them in a visual hierarchy — the countdown loudest (the Hero
//! role, turning the status red in the final ≤10 s), then the reference — plus the current Up/Down
//! odds.
//!
//! Stateless and near-actionless (it is a readout): [`draw_price_to_beat`] takes only borrowed
//! [`CockpitPriceToBeatInputs`] and paints. The countdown reuses [`super::up_down_window`]'s
//! `format_countdown`, so every cockpit clock formats identically. The single arithmetic bit
//! (`price_to_beat_delta`) is pure and unit-tested.

use egui::{RichText, Sense, Stroke, Vec2};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
use vike_ui_theme::maps;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::cockpit;

/// The countdown turns the status red inside this many ms of the resolution.
const URGENT_MS: i64 = 10_000;

/// Everything the header needs to render one frame. Borrowed / by-value — the app owns the storage.
#[derive(Clone, Copy, Debug)]
pub struct CockpitPriceToBeatInputs {
    /// the window-open REFERENCE price (the "price to beat") — the loudest element
    pub reference_price: f64,
    /// the live spot price
    pub spot: f64,
    /// window resolution wall-clock, epoch ms (drives the countdown)
    pub resolution_timestamp: i64,
    /// current wall-clock, epoch ms
    pub now_ms: i64,
    /// current Up/YES odds (0..1), if known
    pub up_price: Option<f64>,
    /// current Down/NO odds (0..1), if known
    pub down_price: Option<f64>,
}

// ---------------------------------------------------------------------------
// Pure logic (unit-tested)
// ---------------------------------------------------------------------------

/// Signed distance of spot from the reference (`spot − reference`): positive ⇒ Up is winning,
/// negative ⇒ Down.
pub(crate) fn price_to_beat_delta(spot: f64, reference: f64) -> f64 {
    spot - reference
}

/// Is a remaining duration in the final escalation stretch (the countdown turns red)? True over
/// the last [`URGENT_MS`] down to and including the boundary; an already-past value is not urgent.
fn is_urgent(ms_remaining: i64) -> bool {
    (0..=URGENT_MS).contains(&ms_remaining)
}

/// Fixed two-decimal price. KEPT local, not `vike_ui_theme::fmt`: `format_thousands` adds comma
/// grouping ("60,250.50" vs our "60250.50") and `format_compact` compacts ("60.25K") — pinned in the
/// tests.
fn format_price(p: f64) -> String {
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

/// Today's 44 px, or taller when the Hero countdown needs it: its row height plus the strip's
/// vertical insets (`space::SM` above and below its one row of text). Hero is 40 px on both scales,
/// so about 61 px either way (a 52.8 px row).
fn header_height(ui: &egui::Ui, t: &ThemeTokens) -> f32 {
    let hero = t.mono_semibold(ThemeTextRole::Hero);
    let row = ui.ctx().fonts_mut(|f| f.row_height(&hero));
    (row + 2.0 * space::SM).max(cockpit::HEADER_MIN_HEIGHT)
}

/// Draw one frame of the Price-to-Beat header. Pure readout — no state, no actions.
pub fn draw_price_to_beat(ui: &mut egui::Ui, inputs: &CockpitPriceToBeatInputs) {
    let t = ThemeTokens::of(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), header_height(ui, &t)),
        Sense::hover(),
    );
    ui.painter().rect_filled(rect, 0.0, t.theme.background);
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(stroke::HAIRLINE, t.theme.border),
    );

    // The labels are spaced by the style's gap, which `appearance::apply` sets to the density's.
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(t.metrics.padding, space::SM)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );

    // reference (price to beat): "BEAT" is a word, so Inter; the price is a Display number
    child.label(
        RichText::new("BEAT").font(t.font(ThemeTextRole::Caption)).color(t.theme.text_tertiary),
    );
    child.label(
        RichText::new(format_price(inputs.reference_price))
            .font(t.mono_semibold(ThemeTextRole::Display))
            .color(t.theme.text),
    );

    // live spot + signed Δ, in the market set's up / down text colours
    let delta = price_to_beat_delta(inputs.spot, inputs.reference_price);
    let dcol = if delta > 0.0 {
        ThemeSidePair::PositiveNegative.text(true, &t)
    } else if delta < 0.0 {
        ThemeSidePair::PositiveNegative.text(false, &t)
    } else {
        t.theme.text_secondary
    };
    child.label(
        RichText::new(format_price(inputs.spot)).font(t.mono(ThemeTextRole::Title)).color(dcol),
    );
    child.label(RichText::new(fmt_signed(delta)).font(t.mono(ThemeTextRole::Strong)).color(dcol));

    // Up/Down odds
    child.add_space(t.metrics.gap);
    child.label(
        RichText::new(fmt_odds("Up", inputs.up_price))
            .font(t.mono(ThemeTextRole::Body))
            .color(maps::side::OUTCOME_UP.text.resolve(&t)),
    );
    child.label(
        RichText::new(fmt_odds("Dn", inputs.down_price))
            .font(t.mono(ThemeTextRole::Body))
            .color(maps::side::OUTCOME_DOWN.text.resolve(&t)),
    );

    // THE cockpit clock, right-aligned: the Hero role in the text colour, the status red in the
    // final ≤10s — never the accent (spec §2: the accent is a shape, not a number's colour)
    let remaining = inputs.resolution_timestamp - inputs.now_ms;
    let ccol = if is_urgent(remaining) { ThemeStatus::Error.color() } else { t.theme.text };
    child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |child| {
        child.label(
            RichText::new(crate::cockpit::up_down_window::format_countdown(remaining))
                .font(t.mono_semibold(ThemeTextRole::Hero))
                .color(ccol),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_to_beat_delta_signs() {
        assert!(price_to_beat_delta(101.0, 100.0) > 0.0); // spot above reference ⇒ Up winning
        assert!(price_to_beat_delta(99.0, 100.0) < 0.0); // spot below ⇒ Down
        assert!((price_to_beat_delta(100.0, 100.0)).abs() < 1e-12); // dead on
        assert!((price_to_beat_delta(60_250.5, 60_000.0) - 250.5).abs() < 1e-9);
    }

    /// Pins the rendered strings, INCLUDING the divergences that keep this formatter local
    /// instead of swapping to a `vike_ui_theme::fmt` helper (GUI audit F7 — a swap would change
    /// rendered text).
    #[test]
    fn format_price_is_fixed_two_decimals_ungrouped() {
        assert_eq!(format_price(0.0), "0.00");
        assert_eq!(format_price(0.62), "0.62");
        assert_eq!(format_price(1234.5), "1234.50"); // no comma — format_thousands prints "1,234.50"
        assert_eq!(format_price(60_250.5), "60250.50"); // not compact — format_compact prints "60.25K"
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
