// @generated from crates/vike-ui-theme/ui-theme.toml by crates/vike-ui-theme/src/tokens_gen — do not edit; change the TOML and regenerate (see tests/brand_assets.rs).

/// Every account mode: `ui-theme.toml`'s `[[mode]]` rows, one constant each, and `ALL`, the registry the brand book and the tests iterate.
pub mod modes {
    use super::{ThemeColourRole, ThemeMarkShape, ThemeAccountTierRow};

    /// Real money, and one of the accent's six shapes (spec §2): a fill in the theme's accent with the on-fill label, and no other mode is filled in the accent (the owner's ruling B of 2026-10-03, over the danger red the Trade window's build had filled it in). The venue chip's mark is a filled disc in the accent.
    pub const LIVE: ThemeAccountTierRow = ThemeAccountTierRow { name: "LIVE", fill: ThemeColourRole::Accent, stroke: ThemeColourRole::None, text: ThemeColourRole::OnFill, mark: ThemeMarkShape::Filled, mark_colour: ThemeColourRole::Accent, doc: "Real money, and one of the accent's six shapes (spec §2): a fill in the theme's accent with the on-fill label, and no other mode is filled in the accent (the owner's ruling B of 2026-10-03, over the danger red the Trade window's build had filled it in). The venue chip's mark is a filled disc in the accent." };
    /// A real venue, play money: outlined in the info blue with its label in the same blue, which is the v3 design's own colour (the owner, 2026-10-05: let it be as in design, over the warning amber ruling B had given it). LIVE is told from DEMO by shape, filled against outlined, and on Carbon, whose amber accent sat 4 degrees of hue from that amber, by colour as well. The venue chip's mark is a ring in the info blue.
    pub const DEMO: ThemeAccountTierRow = ThemeAccountTierRow { name: "DEMO", fill: ThemeColourRole::None, stroke: ThemeColourRole::Info, text: ThemeColourRole::Info, mark: ThemeMarkShape::Ring, mark_colour: ThemeColourRole::Info, doc: "A real venue, play money: outlined in the info blue with its label in the same blue, which is the v3 design's own colour (the owner, 2026-10-05: let it be as in design, over the warning amber ruling B had given it). LIVE is told from DEMO by shape, filled against outlined, and on Carbon, whose amber accent sat 4 degrees of hue from that amber, by colour as well. The venue chip's mark is a ring in the info blue." };
    /// Nothing leaves the box: outlined in the theme's border grey with its label in the secondary text. The venue chip's mark is a ring in the caption grey.
    pub const PAPER: ThemeAccountTierRow = ThemeAccountTierRow { name: "PAPER", fill: ThemeColourRole::None, stroke: ThemeColourRole::Border, text: ThemeColourRole::TextSecondary, mark: ThemeMarkShape::Ring, mark_colour: ThemeColourRole::TextTertiary, doc: "Nothing leaves the box: outlined in the theme's border grey with its label in the secondary text. The venue chip's mark is a ring in the caption grey." };

    /// Every row, in the order the TOML lists them.
    pub const ALL: &[ThemeAccountTierRow] = &[LIVE, DEMO, PAPER];
}
