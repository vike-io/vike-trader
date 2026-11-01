use super::*;

/// An empty box is a REQUEST for a direct connection, not an absence of opinion — so it stores
/// the sentinel rather than nothing. Writing nothing would leave `egress`'s built-in default
/// (proxy ON at a localhost tunnel) in force, which is wrong for the user this box is for: one
/// who simply is not geo-blocked and would otherwise dial a tunnel that does not exist.
#[test]
fn an_empty_box_stores_the_direct_sentinel() {
    assert_eq!(proxy_to_store(""), PROXY_DIRECT);
    assert_eq!(proxy_to_store("   "), PROXY_DIRECT, "a box cleared to spaces is still cleared");
}

/// A bought proxy arrives as one string WITH credentials in it. It must round-trip byte-for-byte
/// — no lowercasing, no stripping, no re-encoding of the userinfo.
#[test]
fn a_bought_proxy_url_is_stored_verbatim_credentials_and_all() {
    let url = "socks5h://user:hunter2@1.2.3.4:1080";
    assert_eq!(proxy_to_store(url), url);
    assert_eq!(proxy_to_store(&format!("  {url}  ")), url, "surrounding space is trimmed");
    assert_eq!(proxy_display(Some(url)), url, "and it comes back to the box unchanged");
}

/// Nothing stored, and BOTH direct spellings, show as an empty box: "no proxy" must look like no
/// proxy rather than like a literal `none` the operator has to know to delete.
#[test]
fn nothing_and_both_direct_spellings_display_as_an_empty_box() {
    assert_eq!(proxy_display(None), "");
    assert_eq!(proxy_display(Some("")), "");
    assert_eq!(proxy_display(Some(PROXY_DIRECT)), "");
    assert_eq!(proxy_display(Some("direct")), "");
    assert_eq!(proxy_display(Some("NONE")), "", "the sentinel is case-insensitive");
}

/// Clearing a configured proxy must round-trip to direct, which is the path a user takes when
/// they stop being geo-blocked — the one direction an asymmetric pair would silently break.
#[test]
fn clearing_a_configured_proxy_round_trips_to_direct() {
    let stored = proxy_to_store("socks5h://1.2.3.4:1080");
    assert_eq!(proxy_display(Some(&stored)), "socks5h://1.2.3.4:1080");
    let cleared =
        proxy_to_store(&proxy_display(Some(&stored)).replace("socks5h://1.2.3.4:1080", ""));
    assert_eq!(cleared, PROXY_DIRECT);
    assert_eq!(proxy_display(Some(&cleared)), "");
}

/// The row reports the click and performs no I/O — the `BulkAction` contract. Save must hand
/// back the STORED form (so the caller writes it verbatim), and every other frame must be
/// `None`, or a caller driving this each frame would rewrite the credential store continuously.
#[test]
fn save_returns_the_stored_value_once_and_only_when_clicked() {
    let ctx = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 120.0));
    let mut state = ProxyEdit { buf: "socks5h://user:hunter2@1.2.3.4:1080".to_string() };

    // Render with no pointer input at all: the row must be inert.
    let mut saved = Some(String::new());
    let raw = egui::RawInput { screen_rect: Some(screen), time: Some(0.0), ..Default::default() };
    // ⚠ The `FullOutput` must have its texture deltas CLEARED before it drops, or epaint
    // panics with "Dropped TexturesDelta with 1 unapplied deltas" — a real frame would upload
    // them. This is the same trap that made the egui 0.36 / wgpu 30 bump pass every test and
    // then panic on the first GPU run; the sibling module's `run_frame` exists for it.
    let mut out = ctx.run_ui(raw, |ui| {
        saved = polymarket_proxy_ui(ui, &mut state);
    });
    out.textures_delta.clear();
    assert_eq!(saved, None, "rendering alone never saves");

    // The buffer is untouched by rendering, so the value the operator sees is the value they
    // typed — and `proxy_to_store` is what Save would hand back, credentials intact.
    assert_eq!(state.buf, "socks5h://user:hunter2@1.2.3.4:1080");
    assert_eq!(proxy_to_store(&state.buf), "socks5h://user:hunter2@1.2.3.4:1080");
}
