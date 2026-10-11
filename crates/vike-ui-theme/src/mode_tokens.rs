// @generated from crates/vike-ui-theme/ui-theme.toml by crates/vike-ui-theme/tests/tokens_gen — do not edit; change the TOML and regenerate (see tests/brand_assets.rs).

/// Every account mode: `ui-theme.toml`'s `[[mode]]` rows, one constant each, and `ALL`, the registry the brand book and the tests iterate.
pub mod modes {
    use super::{ColourRole, MarkShape, ModeRow};

    /// Real money, and one of the accent's six shapes (spec §2): a fill in the theme's accent with the on-fill label, and no other mode is filled in the accent (the owner's ruling B of 2026-10-03, over the danger red the Trade window's build had filled it in). The venue chip's mark is a filled disc in the accent.
    pub const LIVE: ModeRow = ModeRow { name: "LIVE", fill: ColourRole::Accent, stroke: ColourRole::None, text: ColourRole::OnFill, mark: MarkShape::Filled, mark_colour: ColourRole::Accent, doc: "Real money, and one of the accent's six shapes (spec §2): a fill in the theme's accent with the on-fill label, and no other mode is filled in the accent (the owner's ruling B of 2026-10-03, over the danger red the Trade window's build had filled it in). The venue chip's mark is a filled disc in the accent." };
    /// A real venue, play money: outlined in the info blue with its label in the same blue, which is the v3 design's own colour (the owner, 2026-10-05: let it be as in design, over the warning amber ruling B had given it). LIVE is told from DEMO by shape, filled against outlined, and on Carbon, whose amber accent sat 4 degrees of hue from that amber, by colour as well. The venue chip's mark is a ring in the info blue.
    pub const DEMO: ModeRow = ModeRow { name: "DEMO", fill: ColourRole::None, stroke: ColourRole::Info, text: ColourRole::Info, mark: MarkShape::Ring, mark_colour: ColourRole::Info, doc: "A real venue, play money: outlined in the info blue with its label in the same blue, which is the v3 design's own colour (the owner, 2026-10-05: let it be as in design, over the warning amber ruling B had given it). LIVE is told from DEMO by shape, filled against outlined, and on Carbon, whose amber accent sat 4 degrees of hue from that amber, by colour as well. The venue chip's mark is a ring in the info blue." };
    /// Nothing leaves the box: outlined in the theme's border grey with its label in the secondary text. The venue chip's mark is a ring in the caption grey.
    pub const PAPER: ModeRow = ModeRow { name: "PAPER", fill: ColourRole::None, stroke: ColourRole::Border, text: ColourRole::Text2, mark: MarkShape::Ring, mark_colour: ColourRole::Text3, doc: "Nothing leaves the box: outlined in the theme's border grey with its label in the secondary text. The venue chip's mark is a ring in the caption grey." };

    /// Every row, in the order the TOML lists them.
    pub const ALL: &[ModeRow] = &[LIVE, DEMO, PAPER];
}
