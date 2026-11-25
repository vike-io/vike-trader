//! The ticket button's and number field's tests: every look in every theme, its words and its
//! accessible name.

use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::components::ON_FILL;
use vike_ui_theme::components::button::ThemeButtonKind;
use vike_ui_theme::market::ThemeMarketColorsId;
use vike_ui_theme::theme::ThemePaletteId;

use super::*;

const LOOKS: [TradeWindowTicketButtonLook; 4] = [
    TradeWindowTicketButtonLook::Plain,
    TradeWindowTicketButtonLook::Chosen,
    TradeWindowTicketButtonLook::Buy,
    TradeWindowTicketButtonLook::Sell,
];

fn tokens(theme: ThemePaletteId, market: ThemeMarketColorsId) -> ThemeTokens {
    ThemeTokens::from_appearance(&ThemeAppearance { theme, market, ..ThemeAppearance::default() })
}

/// Each look's fill (at rest and under the pointer), outline and label are what the ticket
/// painted before they moved to `ui-theme.toml`'s `button` map: plain the surface outlined in the
/// border with the UI text, turning to the theme's hover fill; chosen the card ringed in the
/// accent with the full text colour; Buy and Sell the market set's up and down with no outline
/// and the on-fill black. Changing a row of the table changes the ticket, and this test is what
/// says so by name.
#[test]
fn a_look_wears_the_fill_outline_and_label_the_button_map_gives_it() {
    for id in ThemePaletteId::ALL {
        for m in ThemeMarketColorsId::ALL {
            let t = tokens(id, m);
            let ring = |c: Color32| Stroke::new(stroke::HAIRLINE, c);
            for (look, fill, hot_fill, outline, label) in [
                (
                    TradeWindowTicketButtonLook::Plain,
                    t.theme.surface,
                    t.theme.hover,
                    ring(t.theme.border),
                    t.theme.text_ui,
                ),
                (
                    TradeWindowTicketButtonLook::Chosen,
                    t.theme.card,
                    t.theme.card,
                    ring(t.theme.accent),
                    t.theme.text,
                ),
                (TradeWindowTicketButtonLook::Buy, t.market.up, t.market.up, Stroke::NONE, ON_FILL),
                (
                    TradeWindowTicketButtonLook::Sell,
                    t.market.down,
                    t.market.down,
                    Stroke::NONE,
                    ON_FILL,
                ),
            ] {
                let at = format!("{id:?} {m:?} {look:?}");
                assert_eq!(look.face(false, &t), (fill, outline), "{at}: at rest");
                assert_eq!(look.face(true, &t), (hot_fill, outline), "{at}: under the pointer");
                assert_eq!(
                    TradeWindowTicketButton::new("x").look(look).ink(&t),
                    label,
                    "{at}: the label"
                );
            }
        }
    }
}

/// The ticket reads the kit's table: a plain button is the kit's secondary one, Buy and Sell are
/// the kit's, the chosen look is the one row of the `button` map the kit has no kind for, no two
/// looks share a row, and no row of the map is read by neither the kit nor the ticket.
#[test]
fn every_look_reads_the_row_of_the_kit_it_is_and_every_row_of_the_button_map_has_a_reader() {
    for (look, kind) in [
        (TradeWindowTicketButtonLook::Plain, Some(ThemeButtonKind::Secondary)),
        (TradeWindowTicketButtonLook::Chosen, None),
        (TradeWindowTicketButtonLook::Buy, Some(ThemeButtonKind::Buy)),
        (TradeWindowTicketButtonLook::Sell, Some(ThemeButtonKind::Sell)),
    ] {
        match kind {
            Some(kind) => assert_eq!(look.row(), kind.row(), "{look:?} is the kit's {kind:?}"),
            None => assert_eq!(look.row().key, "CHOSEN", "{look:?}"),
        }
        assert_eq!(
            LOOKS.iter().filter(|o| o.row() == look.row()).count(),
            1,
            "{look:?}'s row is shared"
        );
    }
    for row in maps::button::ALL {
        let kit = ThemeButtonKind::ALL.iter().any(|k| k.row() == *row);
        let ticket = LOOKS.iter().any(|l| l.row() == *row);
        assert!(kit || ticket, "{} is read by neither the kit nor the ticket", row.key);
    }
}

/// Only the plain button turns to another fill under the pointer through its row: the chosen look
/// does not change, and Buy and Sell are lifted by an overlay in `paint_face`, so their face is
/// the same either way.
#[test]
fn only_the_plain_button_names_another_fill_for_the_pointer() {
    for id in ThemePaletteId::ALL {
        let t = tokens(id, ThemeMarketColorsId::default());
        for look in LOOKS {
            let turns = look.face(false, &t) != look.face(true, &t);
            assert_eq!(turns, look == TradeWindowTicketButtonLook::Plain, "{id:?} {look:?}");
        }
    }
}
