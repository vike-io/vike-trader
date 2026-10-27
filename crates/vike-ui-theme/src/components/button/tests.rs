//! Tests for `button`: the action and icon buttons, their colours, states and accessibility.

use super::*;
use crate::appearance::Appearance;
use crate::components::testing::{
    ctx_with, fills, harness, named, paint, paint_at, strokes, texts,
};
use crate::components::{ON_FILL, Status};
use crate::market::{MarketColors, MarketId};
use crate::metrics::Density;
use crate::theme::{Theme, ThemeId};
use crate::type_scale::TextSize;
use egui::accesskit::{Role, Toggled};
use egui_kittest::kittest::NodeT;

/// The primary fill IS the installed theme's accent on every theme — three of four would fail
/// on a compile-time `palette::ACCENT` — and its label is the on-fill black.
#[test]
fn primary_is_filled_with_each_themes_accent() {
    for id in ThemeId::ALL {
        let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
        let shapes = paint(&ctx, |ui| {
            ui.add(ActionButton::primary("Save"));
        });
        assert!(fills(&shapes).contains(&Theme::of(id).accent), "{id:?}");
        assert!(texts(&shapes).contains(&("Save".to_string(), ON_FILL)), "{id:?}");
    }
}

#[test]
fn buy_and_sell_follow_the_market_set() {
    for m in MarketId::ALL {
        let ctx = ctx_with(&Appearance { market: m, ..Appearance::default() });
        let shapes = paint(&ctx, |ui| {
            ui.add(ActionButton::buy("Buy"));
            ui.add(ActionButton::sell("Sell"));
        });
        let (f, c) = (fills(&shapes), MarketColors::of(m));
        assert!(f.contains(&c.up) && f.contains(&c.down), "{m:?}: {f:?}");
    }
}

#[test]
fn secondary_is_an_outline_and_danger_the_status_red() {
    let ctx = ctx_with(&Appearance::default());
    let t = Tokens::of(&ctx);
    let shapes = paint(&ctx, |ui| {
        ui.add(ActionButton::secondary("Refresh"));
        ui.add(ActionButton::danger("Delete"));
    });
    assert!(fills(&shapes).contains(&t.theme.surface));
    assert!(strokes(&shapes).contains(&t.theme.border));
    assert!(fills(&shapes).contains(&Status::Error.color()));
}

/// Every button — words or icon, at either text size — is the density's control height.
#[test]
fn a_button_is_the_densitys_control_height() {
    for d in Density::ALL {
        for size in TextSize::ALL {
            let ctx =
                ctx_with(&Appearance { density: d, text_size: size, ..Appearance::default() });
            let (mut words, mut icon) = (0.0, 0.0);
            paint(&ctx, |ui| {
                words = ui.add(ActionButton::secondary("Refresh")).rect.height();
                icon = ui.add(IconButton::new(icons::SETTINGS, "Settings")).rect.height();
            });
            assert_eq!(words, d.metrics().control_h, "{d:?} {size:?}");
            assert_eq!(icon, d.metrics().control_h, "icon {d:?} {size:?}");
        }
    }
}

/// A disabled kit button is disabled in the tree; the only way to make one is to give a reason.
#[test]
fn a_disabled_button_is_disabled_and_an_enabled_one_is_not() {
    let h = harness(Appearance::default(), |ui| {
        ui.add(ActionButton::primary("Apply").disabled_because("Nothing changed"));
        ui.add(ActionButton::secondary("Refresh"));
    });
    let node = |name: &str| {
        h.root()
            .children_recursive()
            .find(|n| n.accesskit_node().label().as_deref() == Some(name))
            .unwrap_or_else(|| panic!("{name} is not in the tree"))
    };
    assert!(node("Apply").accesskit_node().is_disabled());
    assert!(!node("Refresh").accesskit_node().is_disabled());
}

/// An icon without a word is named by its tip, not by a private-use character.
#[test]
fn an_icon_button_is_named_by_its_tip() {
    let h = harness(Appearance::default(), |ui| {
        ui.add(IconButton::new(icons::SETTINGS, "Chart settings"));
    });
    let buttons = named(&h, Role::Button);
    assert!(buttons.contains(&"Chart settings".to_string()), "{buttons:?}");
}

/// A kit button answers the pointer: hovered, every kind paints another fill than at rest. A
/// fill set on the button itself would paint every state alike.
#[test]
fn a_kit_button_answers_the_pointer() {
    for kind in Kind::ALL {
        let ctx = ctx_with(&Appearance::default());
        let (at_rest, _, _) = kind.colours(&Tokens::of(&ctx));
        let mut rect = egui::Rect::NOTHING;
        let idle = paint(&ctx, |ui| rect = ui.add(ActionButton::new(kind, "Act")).rect);
        assert!(fills(&idle).contains(&at_rest), "{kind:?} at rest");
        // egui styles a button from the state its previous pass ended in: hover twice.
        paint_at(&ctx, rect.center(), |ui| {
            ui.add(ActionButton::new(kind, "Act"));
        });
        let hovered = paint_at(&ctx, rect.center(), |ui| {
            ui.add(ActionButton::new(kind, "Act"));
        });
        assert!(!fills(&hovered).contains(&at_rest), "{kind:?}: hovering changed nothing");
    }
}

/// Each kind's fill, outline, label and fill under the pointer are what the kit painted before
/// they moved to `ui-theme.toml`'s `button` map: primary the accent, secondary the surface
/// outlined in the border with the UI text and the theme's hover fill under the pointer, buy and
/// sell the market set's up and down, danger the status red; every filled one carries the
/// on-fill black and is lifted a step toward the text colour. Changing a row of the table changes
/// a button, and this test is what says so by name.
#[test]
fn a_kind_wears_the_fill_outline_label_and_hover_the_button_map_gives_it() {
    for id in ThemeId::ALL {
        for m in MarketId::ALL {
            let look = Appearance { theme: id, market: m, ..Appearance::default() };
            let t = Tokens::from_appearance(&look);
            let lifted = |fill: Color32| fill.lerp_to_gamma(t.theme.text, alpha::LIFT);
            let hairline = Stroke::new(stroke::HAIRLINE, t.theme.border);
            for (kind, fill, outline, label, lit) in [
                (Kind::Primary, t.theme.accent, Stroke::NONE, ON_FILL, lifted(t.theme.accent)),
                (Kind::Secondary, t.theme.surface, hairline, t.theme.text_ui, t.theme.hover),
                (Kind::Buy, t.market.up, Stroke::NONE, ON_FILL, lifted(t.market.up)),
                (Kind::Sell, t.market.down, Stroke::NONE, ON_FILL, lifted(t.market.down)),
                (
                    Kind::Danger,
                    Status::Error.color(),
                    Stroke::NONE,
                    ON_FILL,
                    lifted(Status::Error.color()),
                ),
            ] {
                let at = format!("{id:?} {m:?} {kind:?}");
                assert_eq!(kind.colours(&t), (fill, outline, label), "{at}");
                assert_eq!(kind.lit(&t), lit, "{at}: under the pointer");
            }
        }
    }
}

/// The map is the kit's own: every kind reads the row of its own name, no two share one, and the
/// one row of the `button` map that no kind reads is the Trade window's chosen look (read by the
/// ticket's `Look`, which `vike-panels` pins).
#[test]
fn every_kind_has_its_own_row_and_the_one_row_no_kind_reads_is_the_chosen_look() {
    for kind in Kind::ALL {
        assert_eq!(
            kind.row().key,
            format!("{kind:?}").to_uppercase(),
            "{kind:?} reads another row"
        );
        assert_eq!(
            Kind::ALL.iter().filter(|o| o.row() == kind.row()).count(),
            1,
            "{kind:?}'s row is shared"
        );
    }
    let mut unread = Vec::new();
    for row in maps::button::ALL {
        if !Kind::ALL.iter().any(|k| k.row() == *row) {
            unread.push(row.key);
        }
    }
    assert_eq!(unread, ["CHOSEN"], "a row of the button map is read by no kind");
}

/// A kind is always drawn: it has a fill and a label (never `none`), a filled kind has no
/// outline and carries the on-fill label, and only the outlined kind names a fill for the
/// pointer — a filled kind's is derived in code, so a role on its row would be read by nothing.
#[test]
fn a_kind_always_has_a_fill_and_a_label_and_only_the_outlined_one_names_a_hover_fill() {
    for kind in Kind::ALL {
        let row = kind.row();
        assert_ne!(row.fill, ColourRole::None, "{kind:?}: a button with no fill");
        assert_ne!(row.text, ColourRole::None, "{kind:?}: a button with no label");
        let outlined = row.stroke != ColourRole::None;
        assert_eq!(outlined, kind == Kind::Secondary, "{kind:?}: the outlined kind");
        assert_eq!(row.colour != ColourRole::None, outlined, "{kind:?}: its own hover fill");
        if !outlined {
            assert_eq!(row.text, ColourRole::OnFill, "{kind:?}: a filled kind's label");
        }
    }
}

/// `disabled_because` is the ONLY way to disable a kit button: one put in a disabled `Ui`
/// without a reason would be a dead control that cannot say why, so debug builds refuse it
/// (spec §4.2).
#[test]
#[should_panic(expected = "disabled_because")]
fn a_kit_button_disabled_without_a_reason_is_refused() {
    let ctx = ctx_with(&Appearance::default());
    paint(&ctx, |ui| {
        ui.add_enabled(false, ActionButton::primary("Apply"));
    });
}

#[test]
#[should_panic(expected = "disabled_because")]
fn an_icon_button_disabled_without_a_reason_is_refused() {
    let ctx = ctx_with(&Appearance::default());
    paint(&ctx, |ui| {
        ui.add_enabled(false, IconButton::new(icons::SETTINGS, "Settings"));
    });
}

/// ...and the reason is what hovering the disabled button shows.
#[test]
fn a_disabled_button_says_why_on_hover() {
    let mut h = harness(Appearance::default(), |ui| {
        ui.ctx().all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
        ui.add(ActionButton::primary("Apply").disabled_because("Nothing changed"));
    });
    h.root()
        .children_recursive()
        .find(|n| n.accesskit_node().label().as_deref() == Some("Apply"))
        .expect("the disabled button is in the tree")
        .hover();
    h.run();
    let labels = named(&h, Role::Label);
    assert!(labels.contains(&"Nothing changed".to_string()), "{labels:?}");
}

/// A pressed icon button shows the segmented control's chosen look: outlined in the theme's
/// accent. An unpressed toggle is not, and neither is a plain icon button (the Trade window's
/// title-bar toggles, spec §3.2).
#[test]
fn a_selected_icon_button_is_outlined_in_each_themes_accent() {
    for id in ThemeId::ALL {
        let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
        let button = |on: Option<bool>| {
            let b = IconButton::new(icons::PANEL_BESIDE, "Ticket beside the ladder");
            match on {
                Some(on) => b.selected(on),
                None => b,
            }
        };
        let accent = Theme::of(id).accent;
        let outlined = |on: Option<bool>| {
            strokes(&paint(&ctx, |ui| {
                ui.add(button(on));
            }))
            .contains(&accent)
        };
        assert!(outlined(Some(true)), "{id:?}: the pressed one");
        assert!(!outlined(Some(false)), "{id:?}: an unpressed toggle");
        assert!(!outlined(None), "{id:?}: a plain icon button");
    }
}

/// Only a TOGGLE reports a pressed state. egui's `Button::selected` opts a button into toggle
/// semantics, so calling it on every icon button would have a screen reader announce each close,
/// minimize and toolbar icon as an unpressed toggle. A plain icon button reports no toggled state
/// at all; one given `selected` reports exactly the state it was given.
#[test]
fn only_an_icon_button_given_a_state_is_announced_as_a_toggle() {
    let h = harness(Appearance::default(), |ui| {
        ui.add(IconButton::new(icons::SETTINGS, "Settings"));
        ui.add(IconButton::new(icons::PANEL_BESIDE, "Ticket beside the ladder").selected(true));
        ui.add(IconButton::new(icons::PANEL_UNDER, "Ticket under the ladder").selected(false));
    });
    let toggled = |name: &str| {
        h.root()
            .children_recursive()
            .find(|n| n.accesskit_node().label().as_deref() == Some(name))
            .unwrap_or_else(|| panic!("{name} is not in the tree"))
            .accesskit_node()
            .toggled()
    };
    assert_eq!(toggled("Settings"), None, "a plain icon button is not a toggle");
    assert_eq!(toggled("Ticket beside the ladder"), Some(Toggled::True));
    assert_eq!(toggled("Ticket under the ladder"), Some(Toggled::False));
}
