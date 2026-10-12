//! The "Data providers" group of the Providers destination's Event plane: the Finnhub and FMP keys,
//! entered here and stored on the SERVER (`crate::backend::provider_keys` is the client of the
//! node's credential verbs; its module doc carries the whole argument).
//!
//! What this screen can and cannot do, stated once:
//!
//! * It shows each key as **set** or **not set** and never a value. The node answers a PRESENT-NAMES
//!   read; no verb on the wire returns a value.
//! * A typed value is sent once and the buffer is emptied the moment it goes.
//! * **Remove is a two-click**: the first click arms the button, which then reads "Confirm remove
//!   KEY"; the second sends. It disarms after [`ProviderKeysView::ARM_WINDOW`] and on any other
//!   click. Nothing is typed (the owner's design, `docs/decisions/0144`'s ADDED note).
//! * The server's ADMIN key is typed into a masked field for the session and kept nowhere.

use std::time::Duration;

use super::{action, legend};
use crate::backend::backend_registry::BackendRecord;
use crate::backend::provider_keys::{Mark, PROVIDER_KEYS, ProviderKeysView};
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::input::{self, Field};
use vike_ui_theme::components::{Status, Tokens, section};
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;

/// Why the admin-key-dependent buttons are dead, in the order the operator fixes them.
const NEEDS_ADMIN_KEY: &str = "type the server's admin key first";
const BUSY: &str = "a request is already running";
const NEEDS_A_VALUE: &str = "type the key's value first";
const NOT_SET: &str = "this key is not set on the server, so there is nothing to remove";

/// Draw the group. `backend` is the connected backend's record (its address is the server the keys
/// go to); `None` draws the explanation instead of the fields.
pub(super) fn provider_keys_group(
    ui: &mut egui::Ui,
    view: &mut ProviderKeysView,
    backend: Option<&BackendRecord>,
) {
    let addr = backend.map(|b| b.addr.trim()).filter(|a| !a.is_empty());
    view.poll(addr, ui.ctx());
    let now = ui.input(|i| i.time);
    if let Some(left) = view.tick_arm(now) {
        ui.ctx().request_repaint_after(Duration::from_secs_f64(left));
    }
    // The click that arms is itself a click: it must not disarm what it armed.
    let mut armed_this_frame = false;
    let mut confirmed: Option<usize> = None;
    let mut saved: Option<usize> = None;

    section::section(
        ui,
        "dm_provider_keys",
        "Data providers",
        |_| {},
        |ui| {
            let t = Tokens::of(ui.ctx());
            legend(
                ui,
                "The Finnhub (earnings) and FMP (dividends) keys are stored on the SERVER you \
                 are connected to, not on this PC: its datahub re-reads them every minute, so a \
                 key saved here is in use within about a minute with no restart. This screen \
                 only ever learns whether a key is set; a value you type is sent once and is \
                 never shown again.",
            );
            ui.add_space(space::SM);
            let Some(addr) = addr else {
                legend(
                    ui,
                    "No backend is connected, so there is no server to store a key on. Connect \
                     one in the Backend destination.",
                );
                return;
            };
            let name = backend.map(|b| b.name.as_str()).unwrap_or_default();
            legend(ui, &format!("Server: {name} ({addr})"));
            ui.add_space(space::SM);

            let have_admin = !view.admin_key.trim().is_empty();
            let busy = view.busy();
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Admin key")
                        .font(t.font(TextRole::Strong))
                        .color(t.theme.text),
                );
                input::text(
                    ui,
                    &mut view.admin_key,
                    Field {
                        hint: "the server's admin key — typed per session, never stored",
                        secret: true,
                        ..Field::default()
                    },
                );
                let mut check = action(None, "Check");
                if !have_admin {
                    check = check.disabled_because(NEEDS_ADMIN_KEY);
                } else if busy {
                    check = check.disabled_because(BUSY);
                }
                if ui.add(check).clicked() {
                    view.start_check(addr, ui.ctx());
                }
            });
            ui.add_space(space::SM);

            for (i, key) in PROVIDER_KEYS.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(key.label)
                            .font(t.font(TextRole::Strong))
                            .color(t.theme.text),
                    );
                    let (word, colour) = match view.mark(i) {
                        Mark::Set => ("set", Status::Ok.color()),
                        Mark::NotSet => ("not set", Status::Muted.color()),
                        Mark::Unknown => ("unknown — press Check", Status::Muted.color()),
                    };
                    ui.label(egui::RichText::new(word).font(t.font(TextRole::Body)).color(colour));
                    ui.label(
                        egui::RichText::new(key.serves)
                            .font(t.font(TextRole::Caption))
                            .color(t.theme.text3),
                    );
                });
                ui.horizontal(|ui| {
                    input::text(
                        ui,
                        &mut view.values[i],
                        Field { hint: key.name, secret: true, ..Field::default() },
                    );
                    let mut save = action(None, "Save");
                    if !have_admin {
                        save = save.disabled_because(NEEDS_ADMIN_KEY);
                    } else if view.values[i].trim().is_empty() {
                        save = save.disabled_because(NEEDS_A_VALUE);
                    } else if busy {
                        save = save.disabled_because(BUSY);
                    }
                    if ui.add(save).clicked() {
                        saved = Some(i);
                    }

                    if matches!(view.armed, Some((armed, _)) if armed == i) {
                        let label = format!("Confirm remove {}", key.name);
                        if ui.add(ActionButton::danger(label.as_str())).clicked() {
                            confirmed = Some(i);
                        }
                    } else {
                        let mut remove = action(None, "Remove");
                        if !have_admin {
                            remove = remove.disabled_because(NEEDS_ADMIN_KEY);
                        } else if view.mark(i) == Mark::NotSet {
                            remove = remove.disabled_because(NOT_SET);
                        } else if busy {
                            remove = remove.disabled_because(BUSY);
                        }
                        if ui.add(remove).clicked() {
                            view.armed = Some((i, now));
                            armed_this_frame = true;
                        }
                    }
                });
                ui.add_space(space::SM);
            }

            if let Some((is_error, text)) = view.status() {
                let colour = if *is_error { Status::Error.color() } else { Status::Ok.color() };
                ui.label(
                    egui::RichText::new(text.as_str()).font(t.font(TextRole::Body)).color(colour),
                );
            } else if busy {
                legend(ui, "Asking the server...");
            }
        },
    );

    // Acts run after the layout, with the view free: a send, the second click, or a disarm.
    if let (Some(i), Some(addr)) = (saved, addr) {
        view.start_save(i, addr, ui.ctx());
    }
    if let (Some(i), Some(addr)) = (confirmed, addr) {
        view.start_remove(i, addr, ui.ctx());
    } else if view.armed.is_some() && !armed_this_frame && ui.input(|i| i.pointer.any_click()) {
        // Any other click disarms (the owner's two-click design).
        view.armed = None;
        ui.ctx().request_repaint();
    }
}
