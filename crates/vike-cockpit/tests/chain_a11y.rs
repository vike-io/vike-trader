//! The window-chain rail in the accessibility tree (design system spec §4.2: a selected item is a
//! Label, not a Button). Each card is its own widget, named by what a trader reads on it: the asset
//! and the countdown. The picked card is a Label, every other card a Button, and clicking a Button
//! picks it.
//!
//! ⚠ `Harness::run` may run several frames and only the first sees a click, so [`Fixture::emitted`]
//! ACCUMULATES and each test drains it before interacting.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use vike_cockpit::{ChainRailAction, ChainRailInputs, ChainRailState, WindowCard};

/// The window length, in seconds (`vike_model::fair::UPDOWN_WINDOW_SECS`).
const WIN: i64 = 300;
/// 42 s into a window: 4:18 to its close, and 4:18 and 9:18 to the next two opens.
const NOW: i64 = 1_724_000_100_000 + 42_000;
/// The three cards' names, left to right.
const CARDS: [&str; 3] = ["BTC 4:18", "BTC +4:18", "BTC +9:18"];

struct Fixture {
    rail: ChainRailState,
    cards: Vec<WindowCard>,
    emitted: Vec<ChainRailAction>,
}

fn harness(selected: Option<usize>) -> Harness<'static, Fixture> {
    let cards = vike_cockpit::chain_windows(NOW, WIN, 2)
        .iter()
        .map(|w| WindowCard { open_ms: w.open_ms, up_price: None, dn_price: None, volume: None })
        .collect();
    let fixture = Fixture { rail: ChainRailState { selected }, cards, emitted: Vec::new() };
    Harness::builder().with_size(egui::vec2(600.0, 200.0)).build_ui_state(
        |ui, f: &mut Fixture| {
            // The countdown draws in a named weight, which egui's default fonts do not bind.
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            let inputs =
                ChainRailInputs { asset: "BTC", cards: &f.cards, window_secs: WIN, now_ms: NOW };
            let actions = vike_cockpit::draw_chain_rail(ui, &mut f.rail, &inputs);
            f.emitted.extend(actions);
        },
        fixture,
    )
}

fn role(h: &Harness<'static, Fixture>, name: &str) -> Role {
    h.get_by_label(name).accesskit_node().role()
}

#[test]
fn with_nothing_picked_every_card_is_a_button() {
    let mut h = harness(None);
    h.run();
    for name in CARDS {
        assert_eq!(role(&h, name), Role::Button, "{name}");
    }
}

#[test]
fn clicking_a_card_picks_it_and_it_becomes_a_label() {
    let mut h = harness(None);
    h.run();
    h.state_mut().emitted.clear();
    h.get_by_label(CARDS[1]).click();
    h.run();
    assert_eq!(h.state().emitted, vec![ChainRailAction::SelectWindow(1)]);
    assert_eq!(h.state().rail.selected, Some(1));
    assert_eq!(role(&h, CARDS[1]), Role::Label);
    for name in [CARDS[0], CARDS[2]] {
        assert_eq!(role(&h, name), Role::Button, "{name}");
    }
}
