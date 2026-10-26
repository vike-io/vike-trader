// @generated from crates/vike-ui-theme/ui-theme.toml by crates/vike-ui-theme/tests/tokens_gen — do not edit; change the TOML and regenerate (see tests/brand_assets.rs).

/// The cool end of the ramp: the owner's v3 design draws the lighter half of its heatmap in this cyan.
const COOL: (u8, u8, u8) = (53, 201, 242);
/// The warm end: the design's amber, for the heavier half.
const WARM: (u8, u8, u8) = (242, 181, 58);
/// The opacity of the emptiest cell and the added opacity of the fullest, as the design's measured (cyan from 0.16, amber to 0.56 and past it).
const ALPHA_MIN: f32 = 0.15;
/// See `ALPHA_MIN`.
const ALPHA_SPAN: f32 = 0.45;
