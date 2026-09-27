//! The one-click ticket — the fast market-entry pad for a Polymarket up/down scalp.
//! RUST-NATIVE (no Python twin — the oracle app has no cockpit).
//!
//! A scalper enters at market on a hair-trigger, so the ticket trades a form for two big BUY UP /
//! BUY DOWN buttons (green / red, the live price on the face), quick-size chips that nudge the
//! stake, and a fee-inclusive payout preview. The safety-vs-speed switch is an ARM toggle encoded
//! in the button FILL COLOUR: disarmed the buy buttons are muted and inert; armed they light up
//! and fire. That way the trading mode is unmistakable before a click sends anything.
//!
//! Same seam as the ladder and the chain rail: a stateless-render [`draw`] over caller-owned
//! [`TicketState`] + borrowed [`TicketInputs`], emitting neutral [`TicketAction`]s the app maps to
//! a `vike_exec::Command`. The payout arithmetic ([`payout_preview`]) is a pure, unit-tested core.

use egui::{Color32, Response, RichText, Stroke, Vec2};
// The shared dark trading-terminal palette (one pinned home; this file used to hand-copy the
// const block — GUI audit F3).
use vike_ui_theme::palette::trading::{ACCENT, DOWN, FAINT, MUTED, PANEL, RULE, TXT, UP};

/// Quick-size chips (dollars) — each adds to the current stake.
const SIZE_CHIPS: [f64; 4] = [1.0, 5.0, 25.0, 100.0];
/// Taker fee on net winnings, in basis points. Polymarket charges none today, but the preview is
/// fee-aware so a future non-zero schedule flows through this ONE site.
const FEE_BPS: f64 = 0.0;

/// Cross-frame view state for one ticket (owned by the app). Holds an f64, so no `PartialEq`.
#[derive(Clone, Debug)]
pub struct TicketState {
    /// the safety switch: while `false` the buy buttons are muted and inert
    pub armed: bool,
    /// the stake in dollars the buy buttons will send
    pub size: f64,
}

impl Default for TicketState {
    fn default() -> Self {
        TicketState { armed: false, size: 1.0 }
    }
}

/// Everything the ticket needs to render one frame. Borrowed / by-value — the app owns the storage.
#[derive(Clone, Copy, Debug)]
pub struct TicketInputs {
    /// current Up/YES price (0..1) — the BUY UP face + payout math
    pub up_price: Option<f64>,
    /// current Down/NO price (0..1) — the BUY DOWN face + payout math
    pub dn_price: Option<f64>,
    /// optional override for the Up win-payout preview (per $1 staked); `None` ⇒ derive from price
    pub up_win_payout: Option<f64>,
    /// optional override for the Down win-payout preview (per $1 staked); `None` ⇒ derive from price
    pub dn_win_payout: Option<f64>,
}

/// A trader intent leaving the ticket. The app mints a client-order-id and maps this to a
/// `vike_exec::Command` (`BuyUp`/`BuyDown` → a market taker for `state.size`).
#[derive(Clone, PartialEq, Debug)]
pub enum TicketAction {
    /// Buy the Up/YES outcome at market for the current stake.
    BuyUp,
    /// Buy the Down/NO outcome at market for the current stake.
    BuyDown,
    /// The stake changed (a chip or clear) — new dollar size.
    SetSize(f64),
    /// The arm safety switch was toggled.
    ToggleArm,
}

// ---------------------------------------------------------------------------
// Pure logic (unit-tested)
// ---------------------------------------------------------------------------

/// Fee-inclusive payout preview for staking `size` dollars on an outcome priced at `price` (0..1).
///
/// Returns `(total, to_win)`: `total` is the dollars returned on a WIN (stake + net winnings) and
/// `to_win` the net profit — the `size/price` shares each redeem at $1, minus the stake and the
/// fee on winnings. A degenerate price (≤0 or ≥1) or non-positive stake yields `(size, 0.0)`
/// (nothing to win). The fee ([`FEE_BPS`]) applies to the gross winnings only, never the stake.
pub fn payout_preview(size: f64, price: f64) -> (f64, f64) {
    if !(price > 0.0 && price < 1.0) || size <= 0.0 {
        return (size.max(0.0), 0.0);
    }
    let shares = size / price;
    let gross_win = shares - size; // payout (shares × $1) minus the stake
    let fee = gross_win.max(0.0) * (FEE_BPS / 10_000.0);
    let to_win = gross_win - fee;
    let total = size + to_win;
    (total, to_win)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draw one frame of the ticket and return the actions produced this frame.
///
/// `state` carries the arm switch + stake across frames; `inputs` is the borrowed live prices.
/// The buy buttons emit [`TicketAction::BuyUp`]/[`TicketAction::BuyDown`] ONLY while armed; the
/// chips/clear emit [`TicketAction::SetSize`]; the ARM toggle emits [`TicketAction::ToggleArm`].
pub fn draw(
    ui: &mut egui::Ui,
    state: &mut TicketState,
    inputs: &TicketInputs,
) -> Vec<TicketAction> {
    let mut actions: Vec<TicketAction> = Vec::new();

    // --- size row: current stake + quick chips + clear ---
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(
            RichText::new(format!("size ${:.0}", state.size)).monospace().size(13.0).color(TXT),
        );
        for chip in SIZE_CHIPS {
            let txt = RichText::new(format!("+${chip:.0}")).monospace().size(11.0).color(MUTED);
            if ui.add(egui::Button::new(txt).fill(PANEL).stroke(Stroke::new(1.0, RULE))).clicked() {
                let ns = (state.size + chip).max(0.0);
                state.size = ns;
                actions.push(TicketAction::SetSize(ns));
            }
        }
        if ui
            .add(
                egui::Button::new(RichText::new("C").monospace().size(11.0).color(FAINT))
                    .fill(PANEL),
            )
            .on_hover_text("Clear stake")
            .clicked()
        {
            state.size = 0.0;
            actions.push(TicketAction::SetSize(0.0));
        }
    });

    // --- arm toggle: the safety-vs-speed switch, encoded in the fill colour ---
    ui.horizontal(|ui| {
        let (lbl, fill, fg) = if state.armed {
            ("● ARMED", ACCENT, Color32::from_rgb(18, 16, 10))
        } else {
            ("ARM", PANEL, MUTED)
        };
        let btn = egui::Button::new(RichText::new(lbl).monospace().size(12.0).color(fg))
            .fill(fill)
            .stroke(Stroke::new(1.0, RULE))
            .min_size(Vec2::new(76.0, 22.0));
        if ui.add(btn).on_hover_text("Arm the one-click buy buttons").clicked() {
            state.armed = !state.armed;
            actions.push(TicketAction::ToggleArm);
        }
    });

    // --- buy buttons (green / red, price on the face; fire only when armed) ---
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if buy_button(ui, "BUY UP", inputs.up_price, UP, state.armed).clicked() && state.armed {
            actions.push(TicketAction::BuyUp);
        }
        if buy_button(ui, "BUY DOWN", inputs.dn_price, DOWN, state.armed).clicked() && state.armed {
            actions.push(TicketAction::BuyDown);
        }
    });

    // --- fee-inclusive payout preview ("To win" / "Total") per available side ---
    if let Some(p) = inputs.up_price {
        preview_line(ui, "Up", state.size, p, inputs.up_win_payout, UP);
    }
    if let Some(p) = inputs.dn_price {
        preview_line(ui, "Dn", state.size, p, inputs.dn_win_payout, DOWN);
    }

    actions
}

/// One BUY button: `label price` on the face, hot fill when armed, muted/inert when disarmed.
fn buy_button(
    ui: &mut egui::Ui,
    label: &str,
    price: Option<f64>,
    col: Color32,
    armed: bool,
) -> Response {
    let face = match price {
        Some(p) => format!("{label}  {p:.2}"),
        None => format!("{label}  —"),
    };
    let fill = if armed { col.linear_multiply(0.28) } else { PANEL };
    let fg = if armed { col } else { MUTED };
    let stroke = Stroke::new(1.5, if armed { col } else { RULE });
    ui.add(
        egui::Button::new(RichText::new(face).monospace().size(13.0).color(fg))
            .fill(fill)
            .stroke(stroke)
            .min_size(Vec2::new(100.0, 28.0)),
    )
}

/// One payout-preview line. Uses the explicit win-payout override when supplied (net per stake),
/// else derives it from the price via [`payout_preview`].
fn preview_line(
    ui: &mut egui::Ui,
    lbl: &str,
    size: f64,
    price: f64,
    override_win: Option<f64>,
    col: Color32,
) {
    let (total, to_win) = match override_win {
        Some(w) => (size + w, w),
        None => payout_preview(size, price),
    };
    ui.label(
        RichText::new(format!("{lbl}  to win ${to_win:.2} · total ${total:.2}"))
            .monospace()
            .size(11.0)
            .color(col.linear_multiply(0.9)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payout_preview_no_fee_clean_numbers() {
        // $10 at 0.50 → 20 shares, payout $20, net win $10, total $20
        let (total, to_win) = payout_preview(10.0, 0.50);
        assert!((to_win - 10.0).abs() < 1e-9, "to_win {to_win}");
        assert!((total - 20.0).abs() < 1e-9, "total {total}");
        // total is always stake + net win
        assert!((total - (10.0 + to_win)).abs() < 1e-12);
    }

    #[test]
    fn payout_preview_cheap_outcome_pays_more() {
        // $10 at 0.20 → 50 shares, payout $50, net win $40
        let (total, to_win) = payout_preview(10.0, 0.20);
        assert!((to_win - 40.0).abs() < 1e-9, "to_win {to_win}");
        assert!((total - 50.0).abs() < 1e-9, "total {total}");
    }

    #[test]
    fn payout_preview_degenerate_inputs_win_nothing() {
        for bad in [0.0, 1.0, -0.1, 1.5] {
            let (total, to_win) = payout_preview(10.0, bad);
            assert!((total - 10.0).abs() < 1e-12, "price {bad}");
            assert!(to_win.abs() < 1e-12, "price {bad}");
        }
        // non-positive stake
        let (total, to_win) = payout_preview(0.0, 0.5);
        assert!(total.abs() < 1e-12 && to_win.abs() < 1e-12);
        let (total, to_win) = payout_preview(-5.0, 0.5);
        assert!(total.abs() < 1e-12 && to_win.abs() < 1e-12);
    }

    #[test]
    fn state_defaults_disarmed_with_unit_stake() {
        let s = TicketState::default();
        assert!(!s.armed);
        assert!((s.size - 1.0).abs() < 1e-12);
    }
}
