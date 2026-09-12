//! The one canonical color palette — now a thin re-export of the shared [`vike_ui_theme::palette`]
//! leaf crate. `theme::UP` / `theme::ACCENT` / `theme::TEXT_UI` / … keep working across `main.rs`
//! (and `install_visuals` builds the egui `Visuals` from these), while the actual values — and the
//! const-equality test that pins them — live in exactly ONE place: `vike_ui_theme::palette`.
//!
//! History (why this file is now a shim): before `vike-ui-theme` this WAS the canonical palette, and
//! the drift that invited already shipped a real bug — the News and Calendar arms spelled the accent
//! `(62,224,137)` instead of the canonical `(62,224,138)`, a 1-bit green drift on a color that is
//! supposed to be identical everywhere. Hand-copies of these constants in `vike-chart`,
//! `vike-connections` and `vike-app-core` were the same hazard one crate boundary further out. The
//! extraction to `vike_ui_theme::palette` (consumed by every GUI crate) ends both.
//!
//! [`install_visuals`]: crate::install_visuals

pub use vike_ui_theme::palette::*;
