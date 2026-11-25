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
//!   KEY"; the second sends. It disarms after [`BackendProviderKeysView::ARM_WINDOW`] and on any other
//!   click. Nothing is typed (the owner's design, `docs/decisions/0144`'s ADDED note).
//! * Every act is signed with this desktop's device key, and the server's access list must give it
//!   the `admin` role (decision 0288). A desktop that holds none sends nothing and says so.

use std::time::Duration;

use super::super::data_manager::{action, legend};
use crate::backend::provider_keys::{
    BackendProviderKeyMark, BackendProviderKeysView, PROVIDER_KEYS,
};
use crate::backend::registry::BackendRecord;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::input::{self, ThemeInputField};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, section};
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;

/// Why a button is dead, in the order the operator fixes them (after
/// [`crate::backend::registry::no_device_key`]).
const BUSY: &str = "a request is already running";
const NEEDS_A_VALUE: &str = "type the key's value first";
const NOT_SET: &str = "this key is not set on the server, so there is nothing to remove";
const NOT_CHECKED: &str = "press Check first: this key may not be set on the server";

/// Draw the group. `backend` is the connected backend's record (its address is the server the keys
/// go to); `None` draws the explanation instead of the fields.
pub(in crate::ui::windows) fn provider_keys_group(
    ui: &mut egui::Ui,
    view: &mut BackendProviderKeysView,
    backend: Option<&BackendRecord>,
) {
    let address = backend.map(|b| b.address.trim()).filter(|a| !a.is_empty());
    view.poll(address, ui.ctx());
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
        "data_manager_provider_keys",
        "Data providers",
        |_| {},
        |ui| {
            let t = ThemeTokens::of(ui.ctx());
            legend(
                ui,
                "The Finnhub (earnings) and FMP (dividends) keys are stored on the SERVER you \
                 are connected to, not on this PC: its datahub re-reads them every minute, so a \
                 key saved here is in use within about a minute with no restart. This screen \
                 only ever learns whether a key is set; a value you type is sent once and is \
                 never shown again.",
            );
            ui.add_space(space::SM);
            let Some(address) = address else {
                legend(
                    ui,
                    "No backend is connected, so there is no server to store a key on. Connect \
                     one in the Backend destination.",
                );
                return;
            };
            let name = backend.map(|b| b.name.as_str()).unwrap_or_default();
            legend(ui, &format!("Server: {name} ({address})"));
            ui.add_space(space::SM);

            // This desktop's own device key signs (decision 0288: admin is its role on the server's
            // access list); a desktop that holds none sends nothing.
            let have_admin = crate::backend::registry::device_key().is_some();
            let no_key = crate::backend::registry::no_device_key(address);
            let busy = view.busy();
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Signed by")
                        .font(t.font(ThemeTextRole::Strong))
                        .color(t.theme.text),
                );
                legend(
                    ui,
                    if have_admin {
                        "this desktop's device key (it needs the admin role on the server)"
                    } else {
                        "no device key"
                    },
                );
                let mut check = action(None, "Check");
                if !have_admin {
                    check = check.disabled_because(&no_key);
                } else if busy {
                    check = check.disabled_because(BUSY);
                }
                if ui.add(check).clicked() {
                    view.start_check(address, ui.ctx());
                }
            });
            if !have_admin {
                // On its own line: a horizontal row never wraps a label, and this one is long.
                legend(ui, &no_key);
            }
            ui.add_space(space::SM);

            for (i, key) in PROVIDER_KEYS.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(key.label)
                            .font(t.font(ThemeTextRole::Strong))
                            .color(t.theme.text),
                    );
                    let (word, colour) = match view.mark(i) {
                        BackendProviderKeyMark::Set => ("set", ThemeStatus::Ok.color()),
                        BackendProviderKeyMark::NotSet => ("not set", ThemeStatus::Muted.color()),
                        BackendProviderKeyMark::Unknown => {
                            ("unknown — press Check", ThemeStatus::Muted.color())
                        }
                    };
                    ui.label(
                        egui::RichText::new(word).font(t.font(ThemeTextRole::Body)).color(colour),
                    );
                    ui.label(
                        egui::RichText::new(key.serves)
                            .font(t.font(ThemeTextRole::Caption))
                            .color(t.theme.text_tertiary),
                    );
                });
                ui.horizontal(|ui| {
                    input::text(
                        ui,
                        &mut view.values[i],
                        ThemeInputField {
                            hint: key.name,
                            secret: true,
                            ..ThemeInputField::default()
                        },
                    );
                    // Save is the row's primary action (spec §4.3), as the proxy box's Save beneath is.
                    let mut save = ThemeButtonAction::primary("Save");
                    if !have_admin {
                        save = save.disabled_because(&no_key);
                    } else if view.values[i].trim().is_empty() {
                        save = save.disabled_because(NEEDS_A_VALUE);
                    } else if busy {
                        save = save.disabled_because(BUSY);
                    }
                    if ui.add(save).clicked() {
                        saved = Some(i);
                    }

                    if matches!(view.armed, Some((armed, _)) if armed == i) {
                        // The Connections editor's armed Remove, so the two surfaces match: the error
                        // colour on the outlined button, never a filled red one.
                        let label = egui::RichText::new(format!("Confirm remove {}", key.name))
                            .color(ThemeStatus::Error.color());
                        if ui.add(ThemeButtonAction::secondary(label)).clicked() {
                            confirmed = Some(i);
                        }
                    } else {
                        let mut remove = action(None, "Remove");
                        if !have_admin {
                            remove = remove.disabled_because(&no_key);
                        } else if view.mark(i) == BackendProviderKeyMark::Unknown {
                            // A first click would ARM a delete of a key that may not exist.
                            remove = remove.disabled_because(NOT_CHECKED);
                        } else if view.mark(i) == BackendProviderKeyMark::NotSet {
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
                let colour =
                    if *is_error { ThemeStatus::Error.color() } else { ThemeStatus::Ok.color() };
                ui.label(
                    egui::RichText::new(text.as_str())
                        .font(t.font(ThemeTextRole::Body))
                        .color(colour),
                );
            } else if busy {
                legend(ui, "Asking the server...");
            }
        },
    );

    // Acts run after the layout, with the view free: a send, the second click, or a disarm.
    if let (Some(i), Some(address)) = (saved, address) {
        view.start_save(i, address, ui.ctx());
    }
    if let (Some(i), Some(address)) = (confirmed, address) {
        view.start_remove(i, address, ui.ctx());
    } else if view.armed.is_some() && !armed_this_frame && ui.input(|i| i.pointer.any_click()) {
        // Any other click disarms (the owner's two-click design).
        view.armed = None;
        ui.ctx().request_repaint();
    }
}
