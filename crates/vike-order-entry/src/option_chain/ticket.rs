//! The options order ticket: the prefilled order a chain bid/ask click opens ([`OptionChainTicket`]),
//! and the confirm modal it is edited in ([`order_ticket`]; design system step 7 moved it down out of
//! `vike-desktop`'s `tool_content` onto the component kit, and wave 3 on down beside the chain that
//! opens it). SAFETY, unchanged: the order leaves ONLY on Confirm —
//! never on the chain click that opened the ticket, never on Cancel.
//!
//! Confirm is the kit's buy or sell button, in the market set in force with the on-fill black
//! label; the white label it had measured 2.5:1 and 3.3:1 on the Classic fills.

use egui::RichText;
use vike_marketdata::OptionKind;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::components::button::ThemeButtonAction;

/// A prefilled options order ticket awaiting the user's Confirm. Built from a chain bid/ask cell
/// click (see [`OptionChainOrderClick`](super::OptionChainOrderClick)); the order is submitted to the deribit execution engine
/// ONLY when the user clicks Confirm in the modal — NEVER on the chain click that opened it.
/// `side` is vike's `i32` order side (+1 Buy / −1 Sell); `price`/`quantity` are editable in the modal.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionChainTicket {
    pub instrument: String,
    pub side: i32,
    pub price: f64,
    pub quantity: f64,
    pub kind: OptionKind,
    pub strike: f64,
}

/// What the operator did with the ticket this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionChainTicketChoice {
    Open,
    Confirmed,
    Cancelled,
}

/// The ticket as a centred window. The price and quantity are edited in place.
pub fn order_ticket(
    ctx: &egui::Context,
    ticket: &mut OptionChainTicket,
) -> OptionChainTicketChoice {
    let mut choice = OptionChainTicketChoice::Open;
    let side = vike_model::side_word(ticket.side);
    let cp = ticket.kind.as_cp();
    egui::Window::new(format!("Order · {}", ticket.instrument))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            let gap = ThemeTokens::of(ui.ctx()).metrics.gap;
            let head = format!("{side} {} · strike {} {cp}", ticket.instrument, ticket.strike);
            ui.label(RichText::new(head).strong());
            ui.add_space(gap);
            // Each field is named by the label beside it, so a screen reader says what it edits.
            ui.horizontal(|ui| {
                let name = ui.label("Limit price").id;
                ui.add(egui::DragValue::new(&mut ticket.price).speed(0.0001)).labelled_by(name);
            });
            ui.horizontal(|ui| {
                let name = ui.label("Quantity").id;
                ui.add(
                    egui::DragValue::new(&mut ticket.quantity).speed(0.01).range(0.0..=f64::MAX),
                )
                .labelled_by(name);
            });
            ui.add_space(gap);
            ui.horizontal(|ui| {
                let confirm = if ticket.side > 0 {
                    ThemeButtonAction::buy("Confirm Buy")
                } else {
                    ThemeButtonAction::sell("Confirm Sell")
                };
                if ui.add(confirm).clicked() {
                    choice = OptionChainTicketChoice::Confirmed;
                }
                if ui.add(ThemeButtonAction::secondary("Cancel")).clicked() {
                    choice = OptionChainTicketChoice::Cancelled;
                }
            });
        });
    choice
}
