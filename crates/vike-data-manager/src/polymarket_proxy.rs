//! The **Polymarket proxy** box: the pure store/display pair, and the row's no-I/O contract.
//!
//! Its own module because the field shares nothing with the catalog grid — it is the one piece of
//! the Data Manager's view that writes rather than reads, and its rules are about a credential-store
//! value, not about series coverage.

use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::input::{self, ThemeInputField};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::data_manager;

/// The edit buffer behind the **Polymarket proxy** box. `buffer` is what the user has typed; seed it
/// with the value already in force (vike-app-core's `seed_polymarket_proxy_box`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataManagerPolymarketProxyEdit {
    pub buffer: String,
}

/// `venue.polymarket.socks_proxy`'s "no proxy" spelling. `vike_polymarket::transport` reads this (and `direct`) as
/// an explicit request for a DIRECT connection that wins over `proxy_enabled`, which is why an empty box writes it
/// rather than writing nothing: writing nothing would leave a box that set `proxy_enabled = true` dialling
/// the tunnel the operator just cleared.
pub(crate) const PROXY_DIRECT: &str = "none";

/// What an empty box means once stored: a box holding only spaces is a user who cleared it, so it
/// stores as [`PROXY_DIRECT`]. (`direct` and `none` both come BACK as an empty box, by the
/// resolver's own precedence.)
pub(crate) fn proxy_to_store(typed: &str) -> String {
    let t = typed.trim();
    if t.is_empty() { PROXY_DIRECT.to_string() } else { t.to_string() }
}

/// The **Polymarket proxy** row: a label, a single-line box, and a Save button.
///
/// Returns `Some(value)` on the frame Save is clicked — the value already run through
/// `proxy_to_store`, so the caller writes it to `venue.polymarket.socks_proxy` verbatim. `None` every other
/// frame. Like [`DataManagerInventoryGridBulkAction`], this reports the click and performs no I/O: the caller owns the
/// credential-store write — vike-app-core's `save_polymarket_proxy`, into the store the binary
/// resolved and handed down as a `vike_account_manager::AccountManagerCredentialWrite`. (This said "the caller
/// (vike-app) … because only a binary may touch that store" until 2026-09-28.)
///
/// The value is shown PLAINLY, embedded credentials included — a SOCKS URL may carry
/// `user:pass@`, and masking a value the operator is here to read and correct would defeat the box.
///
/// [`DataManagerInventoryGridBulkAction`]: crate::DataManagerInventoryGridBulkAction
pub fn polymarket_proxy_ui(
    ui: &mut egui::Ui,
    state: &mut DataManagerPolymarketProxyEdit,
) -> Option<String> {
    let t = ThemeTokens::of(ui.ctx());
    let mut save = None;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Polymarket proxy")
                .font(t.font(ThemeTextRole::Body))
                .color(t.theme.text_secondary),
        );
        ui.scope(|ui| {
            ui.spacing_mut().text_edit_width = data_manager::PROXY_BOX_WIDTH;
            input::text(
                ui,
                &mut state.buffer,
                ThemeInputField { hint: PROXY_HINT, ..ThemeInputField::default() },
            );
        });
        // Save is this row's primary action (spec §4.3).
        if ui.add(ThemeButtonAction::primary("Save")).clicked() {
            save = Some(proxy_to_store(&state.buffer));
        }
    });
    ui.label(
        egui::RichText::new(PROXY_HELP)
            .font(t.font(ThemeTextRole::Caption))
            .color(t.theme.text_tertiary),
    );
    save
}

/// The empty box's placeholder — the exact shape a bought proxy is pasted in as.
pub(crate) const PROXY_HINT: &str = "socks5h://user:pass@host:1080";

/// The line under the box. Both constraints are load-bearing rather than pedantry, which is why
/// they are stated to the operator instead of left in a module doc:
///
/// - **`socks5h`, not `socks5`.** Plain `socks5` resolves the target's DNS LOCALLY, which is
///   exactly what a DNS-based geo-block catches — the tunnel carries the bytes but the lookup
///   already failed. `socks5h` hands the hostname to the proxy to resolve.
/// - **SOCKS5 only.** `vike_transport::websocket_proxy::WebsocketProxy::parse` refuses every other scheme, so a
///   bought HTTP proxy will not work here however valid its credentials.
pub(crate) const PROXY_HELP: &str = "SOCKS5 only, and socks5h:// (not socks5://) so DNS resolves at the proxy. \
     Empty = direct, no proxy. Takes effect after restart.";

#[path = "polymarket_proxy_tests.rs"]
#[cfg(test)]
mod polymarket_proxy_tests;
