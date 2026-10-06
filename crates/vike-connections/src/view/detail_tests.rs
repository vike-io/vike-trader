//! Unit tests of the detail pane's pure helpers and of the tier marks it paints.

use super::*;

/// ⚠ **`key_family`'s own per-venue assertions live in
/// `crates/vike-connections/tests/key_shapes/write_shapes.rs`, not here**, and it is FORCED — the
/// same rule that put `edit_fields`' fixtures there. Spelling a venue key PREFIX in a `src/`
/// test region (`BINANCE_`, `POLY_`, `ASTER_`, `CTRADER_`) makes
/// `crates/vike-ops/tests/settings/settings_registry/directions.rs`'s `every_read_variable_is_declared` harvest it
/// as an env-var-shaped literal and demand a `vike_ops::settings::SETTINGS` row asserting a
/// read this crate does not perform. MEASURED: five such prefixes reddened that gate before
/// the assertions moved. What stays here is only what spells no such literal.
///
/// The prefix-cutting HELPER is private, so its own edge cases have to be tested here — with
/// inputs that are deliberately not venue-shaped.
#[test]
fn the_common_prefix_is_cut_on_an_underscore_boundary() {
    assert_eq!(common_key_prefix(&["ONLY_ONE".to_string()]), "ONLY_");
    assert_eq!(
        common_key_prefix(&["ABC_DEF".to_string(), "ABC_XYZ".to_string()]),
        "ABC_",
        "the cut lands on the shared `_` boundary, never mid-token"
    );
    // No shared `_` boundary at all: the whole first key rather than an empty string.
    assert_eq!(common_key_prefix(&["AB".to_string(), "XY".to_string()]), "AB");
}

/// `SIM` renders as `Sim` in the detail pane's tier column.
#[test]
fn tier_names_render_in_title_case() {
    assert_eq!(title_tier("SIM"), "Sim");
    assert_eq!(title_tier("DEMO"), "Demo");
    assert_eq!(title_tier("LIVE"), "Live");
    assert_eq!(title_tier(""), "");
}

/// A tier state's mark is the colour and the glyph the rail's dot, the detail pane's tier row and the legend
/// painted before the colours moved to `ui-theme.toml`'s `tier_state` map: configured the status green, not
/// set the status grey, a tier that does not exist the same grey DIMMED by `NOT_CONFIGURABLE_DIM`, and a
/// store nobody could open the status amber — on every theme. Changing a row of the table changes the mark,
/// and this test is what says so by name.
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
