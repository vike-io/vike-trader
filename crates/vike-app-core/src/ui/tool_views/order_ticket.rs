//! The options order ticket (design system step 7 moved it down out of `vike-desktop`'s
//! `tool_content` onto the component kit). SAFETY, unchanged: the order leaves ONLY on Confirm —
//! never on the chain click that opened the ticket, never on Cancel.
//!
//! Confirm is the kit's buy or sell button, in the market set in force with the on-fill black
//! label; the white label it had measured 2.5:1 and 3.3:1 on the Classic fills.

use egui::RichText;
use vike_ui_theme::components::Tokens;
use vike_ui_theme::components::button::ActionButton;

use crate::tools::OptOrderTicket;

/// What the operator did with the ticket this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketChoice {
    Open,
    Confirmed,
    Cancelled,
}

/// The ticket as a centred window. The price and quantity are edited in place.
pub fn order_ticket(ctx: &egui::Context, ticket: &mut OptOrderTicket) -> TicketChoice {
    let mut choice = TicketChoice::Open;
    let side = if ticket.side > 0 { "BUY" } else { "SELL" };
    let cp = if ticket.is_call { "C" } else { "P" };
    egui::Window::new(format!("Order · {}", ticket.instrument))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            let gap = Tokens::of(ui.ctx()).metrics.gap;
            let head = format!("{side} {} · strike {} {cp}", ticket.instrument, ticket.strike);
            ui.label(RichText::new(head).strong());
            ui.add_space(gap);
            ui.horizontal(|ui| {
                ui.label("Limit price");
                ui.add(egui::DragValue::new(&mut ticket.price).speed(0.0001));
            });
            ui.horizontal(|ui| {
                ui.label("Quantity");
                ui.add(egui::DragValue::new(&mut ticket.qty).speed(0.01).range(0.0..=f64::MAX));
            });
            ui.add_space(gap);
            ui.horizontal(|ui| {
                let confirm = if ticket.side > 0 {
                    ActionButton::buy("Confirm Buy")
                } else {
                    ActionButton::sell("Confirm Sell")
                };
                if ui.add(confirm).clicked() {
                    choice = TicketChoice::Confirmed;
                }
                if ui.add(ActionButton::secondary("Cancel")).clicked() {
                    choice = TicketChoice::Cancelled;
                }
            });
        });
    choice
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::caption::test_shapes::{ctx_with, fills, second_pass, texts};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use std::sync::{Arc, Mutex};
    use vike_ui_theme::appearance::Appearance;
    use vike_ui_theme::components::ON_FILL;
    use vike_ui_theme::market::{MarketColors, MarketId};

    fn ticket(side: i32) -> OptOrderTicket {
        OptOrderTicket {
            instrument: "BTC-27DEC26-100000-C".to_string(),
            side,
            price: 0.05,
            qty: 0.1,
            is_call: true,
            strike: 100000.0,
        }
    }

    /// Confirm is the kit's buy or sell button in the market set in force, labelled in the on-fill
    /// black. The old white label measured 2.5:1 on the Classic green and 3.3:1 on its red.
    #[test]
    fn confirm_is_the_market_sets_side_labelled_on_fill() {
        for m in MarketId::ALL {
            for side in [1, -1] {
                let ctx = ctx_with(&Appearance { market: m, ..Appearance::default() });
                // A window FADES IN over `animation_time`, and a fading painter multiplies a rect's
                // fill (a text keeps its colour and carries an opacity factor instead), so on the
                // frames a test runs the fill would be a dimmed market colour. No animation: the
                // window is fully opaque (the same fix `chart_window.rs`'s
                // `the_style_menu_marks_the_selected_style_with_an_accent_edge` uses for its popup).
                ctx.all_styles_mut(|s| s.animation_time = 0.0);
                let mut t = ticket(side);
                let shapes = second_pass(&ctx, |ui| {
                    order_ticket(ui.ctx(), &mut t);
                });
                let c = MarketColors::of(m);
                let (fill, word) =
                    if side > 0 { (c.up, "Confirm Buy") } else { (c.down, "Confirm Sell") };
                assert!(fills(&shapes).contains(&fill), "{m:?} {word}: the fill");
                let label =
                    texts(&shapes).into_iter().find(|(s, _, _)| s == word).map(|(_, _, c)| c);
                assert_eq!(label, Some(ON_FILL), "{m:?} {word}: the label");
            }
        }
    }

    /// The order leaves on Confirm and nothing else: no click keeps the ticket open, and Cancel
    /// drops it.
    #[test]
    fn only_confirm_confirms() {
        for (click, want) in
            [("Confirm Buy", TicketChoice::Confirmed), ("Cancel", TicketChoice::Cancelled)]
        {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&seen);
            let mut t = ticket(1);
            let mut h =
                Harness::builder().with_size(egui::vec2(800.0, 600.0)).build_ui(move |ui| {
                    if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                        return;
                    }
                    sink.lock().unwrap().push(order_ticket(ui.ctx(), &mut t));
                });
            h.run();
            assert!(
                seen.lock().unwrap().iter().all(|c| *c == TicketChoice::Open),
                "no click, no choice"
            );
            h.get_by_label(click).click();
            h.run();
            assert!(seen.lock().unwrap().contains(&want), "{click}");
        }
    }
}
