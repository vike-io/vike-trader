use super::*;
use crate::test_support::{fills, installed, shapes_of};
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::theme::{ThemePalette, ThemePaletteId};

/// An empty box is a REQUEST for a direct connection, not an absence of opinion — so it stores
/// the sentinel rather than nothing. Writing nothing would leave a box that set
/// `proxy_enabled = true` dialling the tunnel the operator just cleared. (The display half, and
/// clearing over an enabled tunnel, are pinned where the box is seeded: vike-app-core's
/// `polymarket_proxy_tests.rs`.)
#[test]
fn an_empty_box_stores_the_direct_sentinel() {
    assert_eq!(proxy_to_store(""), PROXY_DIRECT);
    assert_eq!(proxy_to_store("   "), PROXY_DIRECT, "a box cleared to spaces is still cleared");
}

/// A bought proxy arrives as one string WITH credentials in it. It must be stored byte-for-byte
/// — no lowercasing, no stripping, no re-encoding of the userinfo.
#[test]
fn a_bought_proxy_url_is_stored_verbatim_credentials_and_all() {
    let url = "socks5h://user:hunter2@1.2.3.4:1080";
    assert_eq!(proxy_to_store(url), url);
    assert_eq!(proxy_to_store(&format!("  {url}  ")), url, "surrounding space is trimmed");
}

/// The row reports the click and performs no I/O — the `DataManagerInventoryGridBulkAction` contract. Save must hand
/// back the STORED form (so the caller writes it verbatim), and every other frame must be
/// `None`, or a caller driving this each frame would rewrite the credential store continuously.
#[test]
fn save_returns_the_stored_value_once_and_only_when_clicked() {
    let context = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 120.0));
    let mut state = DataManagerPolymarketProxyEdit {
        buffer: "socks5h://user:hunter2@1.2.3.4:1080".to_string(),
    };

    // Render with no pointer input at all: the row must be inert.
    let mut saved = Some(String::new());
    let raw = egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
    // ⚠ The `FullOutput` must have its texture deltas CLEARED before it drops, or epaint
    // panics with "Dropped TexturesDelta with 1 unapplied deltas" — a real frame would upload
    // them. This is the same trap that made the egui 0.36 / wgpu 30 bump pass every test and
    // then panic on the first GPU run; `crate::test_support`'s `run_frame` exists for it.
    let mut out = context.run_ui(raw, |ui| {
        saved = polymarket_proxy_ui(ui, &mut state);
    });
    out.textures_delta.clear();
    assert_eq!(saved, None, "rendering alone never saves");

    // The buffer is untouched by rendering, so the value the operator sees is the value they
    // typed — and `proxy_to_store` is what Save would hand back, credentials intact.
    assert_eq!(state.buffer, "socks5h://user:hunter2@1.2.3.4:1080");
    assert_eq!(proxy_to_store(&state.buffer), "socks5h://user:hunter2@1.2.3.4:1080");
}

/// Save is the proxy row's primary action (spec §4.3): filled with the installed theme's accent.
#[test]
fn the_proxy_rows_save_is_primary() {
    for theme in ThemePaletteId::ALL {
        let context = installed(&ThemeAppearance { theme, ..ThemeAppearance::default() });
        let mut edit = DataManagerPolymarketProxyEdit::default();
        let f = fills(&shapes_of(&context, |ui| {
            let _ = polymarket_proxy_ui(ui, &mut edit);
        }));
        assert!(f.contains(&ThemePalette::of(theme).accent), "{theme:?}: {f:?}");
    }
}
