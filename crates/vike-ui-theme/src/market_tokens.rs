// @generated from crates/vike-ui-theme/ui-theme.toml by crates/vike-ui-theme/tests/tokens_gen — do not edit; change the TOML and regenerate (see tests/brand_assets.rs).

/// Depth-bar alpha for "up". The DOM window (deleted) drew bid bars at 48 and ask bars at 46 before it read these tokens. Why the two differ is not recorded, so the token does not decide it.
pub const DEPTH_UP_ALPHA: u8 = 48;
/// Depth-bar alpha for "down". See `DEPTH_UP_ALPHA`.
pub const DEPTH_DOWN_ALPHA: u8 = 46;
/// Volume-bar strength: the chart's volume pane fills with `up_volume`/`down_volume`, or with a colour the user picked at this strength.
pub const VOLUME_FACTOR: f32 = 0.7;

/// `(up, down, up as text, down as text)` of `id`: each set's four ruled colours.
fn base(id: MarketId) -> (Color32, Color32, Color32, Color32) {
    match id {
        MarketId::Classic => (Color32::from_rgb(64, 186, 80), Color32::from_rgb(248, 82, 73), Color32::from_rgb(64, 186, 80), Color32::from_rgb(248, 82, 73)),
        // TradingView's CURRENT chart default; the older `#26A69A`/`#EF5350` is what Lightweight Charts still ships. Both text colours are lifted: down measured 4.19:1 on Graphite's card, and up 4.57:1, inside the rule's margin.
        MarketId::TradingView => (Color32::from_rgb(8, 153, 129), Color32::from_rgb(242, 54, 69), Color32::from_rgb(10, 154, 130), Color32::from_rgb(243, 74, 88)),
        MarketId::Exchange => (Color32::from_rgb(46, 189, 133), Color32::from_rgb(246, 70, 93), Color32::from_rgb(46, 189, 133), Color32::from_rgb(246, 70, 93)),
        // Up measured 4.44:1 on Graphite's card.
        MarketId::ColourBlind => (Color32::from_rgb(59, 130, 246), Color32::from_rgb(245, 158, 11), Color32::from_rgb(65, 134, 246), Color32::from_rgb(245, 158, 11)),
    }
}
