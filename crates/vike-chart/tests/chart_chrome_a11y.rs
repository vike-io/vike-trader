//! Accessibility-tree tests for the chart's own chrome on the component kit (GUI design system
//! step 7): the hover nav row, the scale row, the pane controls and the price-axis menu. The kit
//! owns how each looks; these tests pin what the chart promises about them — every icon-only
//! button is named by its words, a selection is a Label, a control at its end says it is
//! disabled.

mod common;
mod dialog_harness;

use egui::accesskit::{Role, Toggled};
use egui_kittest::kittest::NodeT;
use vike_chart::chart::PaneKey;

use dialog_harness::{all_nodes, fixture, harness, label_texts, one, present, settle, toggled};

/// Over the price pane: 45% across, 28% down — left of the price axis, above the bottom rows.
fn over_price() -> egui::Pos2 {
    egui::pos2(1200.0 * 0.45, 1000.0 * 0.28)
}

#[test]
fn the_nav_row_names_every_icon_button_by_its_words() {
    let mut h = harness(fixture());
    settle(&mut h);
    h.hover_at(over_price());
    settle(&mut h);
    let nodes = all_nodes(&h);
    for words in [
        "Zoom out",
        "Zoom in",
        "Earlier",
        "Later",
        "Reset view",
        "Chart settings",
        "Maximize price",
    ] {
        assert!(present(&nodes, Role::Button, words), "{words:?} is not a named nav button");
    }
}

#[test]
fn the_scale_row_reports_the_requested_scale_as_a_label() {
    let mut h = harness(fixture());
    settle(&mut h);
    h.hover_at(over_price());
    settle(&mut h);
    let nodes = all_nodes(&h);
    assert!(label_texts(&nodes).contains(&"Lin".to_string()), "Linear is the selected segment");
    for other in ["Log", "%", "Auto"] {
        assert!(present(&nodes, Role::Button, other), "{other:?} must be a button");
    }
}

#[test]
fn the_pane_move_buttons_are_disabled_at_the_ends() {
    let mut f = fixture();
    f.sub_panes = vec![PaneKey::Volume];
    let mut h = harness(f);
    settle(&mut h);
    // Inside the volume pane at the default shares (price 0.60 : volume 0.16).
    h.hover_at(egui::pos2(600.0, 1000.0 - 120.0));
    settle(&mut h);
    let nodes = all_nodes(&h);
    assert!(present(&nodes, Role::Button, "Remove volume pane"), "the pointer missed the pane");
    for words in ["Move pane up", "Move pane down"] {
        assert!(
            one(&nodes, Role::Button, words).accesskit_node().is_disabled(),
            "{words:?} must be disabled with one sub-pane"
        );
    }
}

#[test]
fn the_price_axis_menu_marks_the_scale_with_a_radio_and_invert_with_a_checkbox() {
    let mut h = harness(fixture());
    settle(&mut h);
    // The right price-axis gutter: 36 px in from the right edge, level with the price pane.
    let gutter = egui::pos2(1200.0 - 36.0, 1000.0 * 0.28);
    h.hover_at(gutter);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: gutter,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    settle(&mut h);
    let nodes = all_nodes(&h);
    assert_eq!(toggled(one(&nodes, Role::RadioButton, "Regular")), Some(Toggled::True));
    for other in ["Logarithmic", "Percent", "Indexed to 100"] {
        assert_eq!(
            toggled(one(&nodes, Role::RadioButton, other)),
            Some(Toggled::False),
            "{other:?}"
        );
    }
    assert_eq!(toggled(one(&nodes, Role::CheckBox, "Invert scale")), Some(Toggled::False));
}
