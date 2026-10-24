// @generated from crates/vike-ui-theme/ui-theme.toml by crates/vike-ui-theme/tests/tokens_gen — do not edit; change the TOML and regenerate (see tests/brand_assets.rs).

/// A healthy state — a live link, a configured credential, a saved write. `#3FBF6E`.
pub const OK: Color32 = Color32::from_rgb(63, 191, 110);
/// Not settled yet — a link dialling or reconnecting, an armed remote-control segment, an account named but not yet saved, a store nobody could open. `#E6B428`.
pub const WARNING: Color32 = Color32::from_rgb(230, 180, 40);
/// A fault — a failed link, a refused write, an inline error. `#F0524A`.
pub const ERROR: Color32 = Color32::from_rgb(240, 82, 74);
/// Information — egui's hyperlink colour. `#57A5FF`.
pub const INFO: Color32 = Color32::from_rgb(87, 165, 255);
/// Nothing live to show — `Disconnected` and `Unknown` alike (only a label tells them apart), an unset credential. Grey 120 is a MARK's colour and sits below the 4.5:1 floor for text. `#787878`.
pub const MUTED: Color32 = Color32::from_rgb(120, 120, 120);
