//! Unit tests of the detail pane's pure helpers and of the tier marks it paints.

use super::*;

/// The FIELDS cell is the schema row's account form, with the venue's application fields beside it
/// and the generic/bespoke verdict — derived, never written down. Field words only: no credential
/// NAME is composed (decision 0156 item 6).
#[test]
fn the_field_family_is_the_schema_rows_fields() {
    assert_eq!(field_family("binance"), "API_KEY · API_SECRET · API_PASSPHRASE (generic)");
    assert_eq!(field_family("dukascopy"), "LOGIN · PASSWORD (bespoke)");
    assert_eq!(
        field_family("ctrader"),
        "ACCESS_TOKEN · REFRESH_TOKEN + app CLIENT_ID · CLIENT_SECRET (bespoke)",
        "the application pair is named beside the account's fields, as the venue's"
    );
    assert!(
        field_family("polymarket").starts_with("PRIVATE_KEY · "),
        "{}",
        field_family("polymarket")
    );
    assert_eq!(field_family("not-a-venue"), "no credential fields");
}

/// `paper` renders as `Paper` in the detail pane's tier column — the tier's own word.
#[test]
fn tier_names_render_in_title_case() {
    assert_eq!(title_tier(AccountTier::Paper), "Paper");
    assert_eq!(title_tier(AccountTier::Demo), "Demo");
    assert_eq!(title_tier(AccountTier::Live), "Live");
}

/// A tier state's mark is the colour and the glyph the rail's dot, the detail pane's tier row and the legend
/// painted before the colours moved to `ui-theme.toml`'s `tier_state` map: configured the status green, not
/// set the status grey, a cell that takes no credential the same grey DIMMED by `NOT_CONFIGURABLE_DIM`, and
/// a store nobody could open the status amber — on every theme. Changing a row of the table changes the
/// mark, and this test is what says so by name.
#[test]
fn a_tier_mark_is_the_colour_and_the_glyph_it_always_was() {
    use vike_ui_theme::appearance::Appearance;
    use vike_ui_theme::theme::ThemeId;
    for id in ThemeId::ALL {
        let t = Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() });
        for (state, colour, glyph) in [
            (TierState::Configured, CONFIGURED_COLOR, "\u{25CF}"),
            (TierState::NotSet, ABSENT_COLOR, "\u{25CB}"),
            (
                TierState::NotConfigurable,
                faded(ABSENT_COLOR, connections::NOT_CONFIGURABLE_DIM),
                "\u{00B7}",
            ),
            (TierState::Unknown, CONNECTING_COLOR, "?"),
        ] {
            assert_eq!(tier_colour(state, &t), colour, "{id:?} {state:?}: the colour");
            assert_eq!(tier_glyph(state), glyph, "{state:?}: the glyph");
        }
    }
}
