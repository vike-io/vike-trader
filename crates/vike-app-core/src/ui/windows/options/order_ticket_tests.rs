//! The options order ticket's tests that draw through app-core's `caption::test_shapes`. The ticket
//! itself is `vike_order_entry::option_chain::ticket`'s.

use crate::ui::caption::test_shapes::second_pass;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::sync::{Arc, Mutex};
use vike_order_entry::option_chain::ticket::{
    OptionChainTicket, OptionChainTicketChoice, order_ticket,
};
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::components::ON_FILL;
use vike_ui_theme::market::{ThemeMarketColors, ThemeMarketColorsId};
use vike_ui_theme::painted::{context_with, fills, texts};

fn ticket(side: i32) -> OptionChainTicket {
    OptionChainTicket {
        instrument: "BTC-27DEC26-100000-C".to_string(),
        side,
        price: 0.05,
        quantity: 0.1,
        kind: vike_marketdata::OptionKind::Call,
        strike: 100000.0,
    }
}

/// Confirm is the kit's buy or sell button in the market set in force, labelled in the on-fill
/// black. The old white label measured 2.5:1 on the Classic green and 3.3:1 on its red.
#[test]
fn confirm_is_the_market_sets_side_labelled_on_fill() {
    for m in ThemeMarketColorsId::ALL {
        for side in [1, -1] {
            let context =
                context_with(&ThemeAppearance { market: m, ..ThemeAppearance::default() });
            // A window FADES IN over `animation_time`, and a fading painter multiplies a rect's
            // fill (a text keeps its colour and carries an opacity factor instead), so on the
            // frames a test runs the fill would be a dimmed market colour. No animation: the
            // window is fully opaque (the same fix `chart_window.rs`'s
            // `the_style_menu_marks_the_selected_style_with_an_accent_edge` uses for its popup).
            context.all_styles_mut(|s| s.animation_time = 0.0);
            let mut t = ticket(side);
            let shapes = second_pass(&context, |ui| {
                order_ticket(ui.ctx(), &mut t);
            });
            let c = ThemeMarketColors::of(m);
            let (fill, word) =
                if side > 0 { (c.up, "Confirm Buy") } else { (c.down, "Confirm Sell") };
            assert!(fills(&shapes).contains(&fill), "{m:?} {word}: the fill");
            let label = texts(&shapes).into_iter().find(|(s, _, _)| s == word).map(|(_, _, c)| c);
            assert_eq!(label, Some(ON_FILL), "{m:?} {word}: the label");
        }
    }
}

/// The order leaves on Confirm and nothing else: no click keeps the ticket open, and Cancel
/// drops it.
#[test]
fn only_confirm_confirms() {
    for (click, want) in [
        ("Confirm Buy", OptionChainTicketChoice::Confirmed),
        ("Cancel", OptionChainTicketChoice::Cancelled),
    ] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let mut t = ticket(1);
        let mut h = Harness::builder().with_size(egui::vec2(800.0, 600.0)).build_ui(move |ui| {
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            sink.lock().unwrap().push(order_ticket(ui.ctx(), &mut t));
        });
        h.run();
        assert!(
            seen.lock().unwrap().iter().all(|c| *c == OptionChainTicketChoice::Open),
            "no click, no choice"
        );
        h.get_by_label(click).click();
        h.run();
        assert!(seen.lock().unwrap().contains(&want), "{click}");
    }
}
