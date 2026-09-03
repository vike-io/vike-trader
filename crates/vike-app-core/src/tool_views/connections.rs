//! The Connections tool body — the venues × Sim/Demo/Live credential-status grid with a live
//! per-venue Status column. Moved verbatim from `vike-app`'s `main.rs` (tool-view extraction
//! batch 2); the ONE adaptation is that the workspace-`.env` read moved UP to the binary (the
//! `vars` parameter) — libraries take configuration as parameters, exactly as batch 1 did with the
//! Tearsheet's `VIKE_JOURNAL_DIR` read.

use super::backend_settings::{
    backend_settings_section, BackendSettingsState, SettingsEditState, SettingsWriteRequest,
};
use super::ToolCtx;
use crate::backend_conn::{click_action, picker_rows, BackendAction};
use crate::backend_editor::{self, BackendForm, EditorState, FormError, KeyPresence};
use crate::backend_registry::BackendsFile;
use std::collections::HashMap;

/// The backend picker's read-only inputs (split-plane B1), grouped the way [`ToolCtx`] groups the
/// grid's: the loaded registry, the ACTIVE connection's record (registry entry or the `--observe`
/// synthetic one), and whether switching is available at all
/// (`crate::backend_conn::switching_available` — `false` while a local core runs; B2).
pub struct BackendPicker<'a> {
    /// The loaded `backends.json` registry.
    pub backends: &'a BackendsFile,
    /// The live connection's record, if any.
    pub active: Option<&'a crate::backend_registry::BackendRecord>,
    /// `false` ⇒ the rows render but the buttons are inert (a local core runs — B2 territory).
    pub available: bool,
}

/// The Connections tool body: the venues × Sim/Demo/Live credential-status grid + a live
/// per-venue Status column, with in-app masked key editing (writes the workspace `.env`; no
/// connect/disconnect controls yet). `vars` is a FRESH workspace-`.env` read done by the caller
/// every frame this tool is visible: a per-frame parse of a small gitignored file is cheap, and it
/// keeps the grid live after a save (or after the user edits `.env` externally and comes back) —
/// simpler than caching on `App` and invalidating on open/refresh/save.
///
/// The live map is built from [`ToolCtx::feed_statuses`] — one entry per already-producing feed
/// (binance/bybit/okx/aster/hyperliquid/polymarket), each a snapshot of that bridge feed's own
/// human-readable status string parsed via `vike_connections::parse_feed_status`. A venue with no
/// live producer (deribit, the FX/broker venues) is simply absent from the map, so `connections_ui`
/// renders it as `Unknown`. Extending coverage means adding that venue's status handle to
/// `App.feed_statuses` where it is assembled (near the feed setup) — this body picks it up
/// automatically.
///
/// ⚠ The grid is per ACCOUNT since multi-account support landed: `vike_connections::AccountGrids`
/// derives one credential grid per account the store holds from the same `vars` map, and the
/// panel's own chip strip picks which one is shown. A box with no labelled account enumerates
/// none, so the derivation is `credential_status(vars)` and the tool renders as it always did —
/// `crates/vike-connections/src/view.rs`'s `account_strip` carries the design argument and the
/// create/remove rules.
///
/// Below the grid sits the BACKEND picker (split-plane B1): the registry rows from
/// [`crate::backend_conn::picker_rows`], each with its Active marker, its control-armed state, and
/// one Connect/Disconnect button. Every decision is a pure `backend_conn` function — this closure
/// only renders and forwards the click into `action` (the OUT slot the App drains after the
/// frame, the same deferred-mutation idiom as every other tool action).
///
/// The Backends section is also where records are MANAGED (split-plane I2): per registry row an
/// Edit and a Delete (are-you-sure) button, plus Add below the list. Every decision — form
/// validation, what a click does, delete-active's disconnect-first, edit-active's deferral — is
/// a pure [`crate::backend_editor`] function; this closure renders the [`EditorState`] and
/// forwards the outcome into `registry` (a second OUT slot beside `action`, drained by the App
/// after the frame through [`backend_editor::apply_registry_update`]). The form's key fields
/// hold credential-store KEY NAMES, never key material ([`backend_editor::KEY_NAME_HINT`] says
/// so in the form), and each carries a presence indicator ([`backend_editor::key_presence`])
/// checked read-only against the same `vars` map the credential grid renders from.
///
/// Below THAT sits its sibling, the **Backend settings** section (split-plane REQ-7, read half):
/// the ACTIVE backend's effective settings-file rows rendered read-only from `settings` (state
/// the binary's fetch thread owns), with `settings_refresh` as the drain-after-frame out-slot —
/// see [`super::backend_settings`] for the state machine and the fetch decisions.
#[allow(clippy::too_many_arguments)] // the tool seam's shape: read-only inputs + one OUT slot each
pub fn connections_tool_content(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut crate::tools::ToolView,
    vars: &HashMap<String, String>,
    picker: &BackendPicker<'_>,
    action: &mut Option<BackendAction>,
    editor: &mut EditorState,
    registry: &mut Option<backend_editor::RegistryUpdate>,
    settings: &BackendSettingsState,
    settings_refresh: &mut bool,
    settings_edit: &mut SettingsEditState,
    settings_write: &mut Option<SettingsWriteRequest>,
) {
    // ONE grid per account the store holds. On a single-account box `AccountGrids::from_vars`
    // enumerates none, so this is `credential_status(vars)` and the tool renders exactly as before.
    let grids = vike_connections::AccountGrids::from_vars(vars);

    let live: HashMap<String, vike_connections::ConnectionState> = ctx
        .feed_statuses
        .iter()
        .map(|(venue, handle)| {
            let s = handle.lock().unwrap().clone();
            (venue.clone(), vike_connections::parse_feed_status(&s))
        })
        .collect();

    // ⚠ `ctx.credentials`, NOT a walk of our own: the Save arm writes the store the BINARY resolved
    // and records into the ledger derived from that same walk. See `ToolCtx::credentials`.
    //
    // ⚠ `take`, so the preselection fires on exactly ONE frame, ever. `ToolView::connections_account`
    // is seeded by the `connections-account` capture arm before the first frame; leaving the value
    // in place would re-select that account every frame and make the chip strip's buttons inert —
    // a click would land and be undone before anything was drawn. On every ordinary launch this is
    // `None` and the call is the call that shipped.
    vike_connections::connections_ui(
        ui,
        &grids,
        &live,
        ctx.credentials,
        tv.connections_account.take(),
    );

    ui.add_space(10.0);
    ui.separator();
    ui.strong("Backends");
    let rows = picker_rows(picker.backends, picker.active);
    if rows.is_empty() {
        ui.weak("no backends configured — Add creates the first record (stored in backends.json beside workspace.json)");
    } else {
        if !picker.available {
            ui.weak("a local trading core is running — backend switching is observe-mode only");
        }
        for row in rows {
            ui.horizontal(|ui| {
                // Active marker + name/addr + the record's control-armed state, render-only.
                ui.label(if row.is_active { "●" } else { " " });
                let name = if row.record.name.is_empty() {
                    "(--observe)"
                } else {
                    row.record.name.as_str()
                };
                ui.strong(name);
                ui.monospace(&row.record.addr);
                ui.weak(if row.record.control { "control armed" } else { "read-only" });
                if !row.listed {
                    ui.weak("(not in registry)");
                }
                let label = if row.is_active { "Disconnect" } else { "Connect" };
                if ui.add_enabled(picker.available, egui::Button::new(label)).clicked() {
                    *action = Some(click_action(row.record, picker.active));
                }
                // Manage buttons (I2) — registry rows only: the synthetic `--observe` record
                // exists for one process's lifetime and is not the file's to edit or delete.
                // NOT gated on `picker.available`: these edit the FILE, not the connection (the
                // one file action that touches a conn — delete-active — asks its disconnect
                // through the drain, and can only arise where a conn exists at all).
                if row.listed {
                    if ui.button("Edit").clicked() {
                        editor.open_edit(row.record);
                    }
                    if ui.button("Delete").clicked() {
                        editor.request_delete(&row.record.name);
                    }
                }
            });
        }
    }
    if ui.button("Add backend").clicked() {
        editor.open_add();
    }
    backend_editor_ui(ui, editor, vars, picker, registry);

    // The Backends section's SIBLING (split-plane REQ-7, read half): the ACTIVE backend's
    // effective settings, read-only. Rendered from state the binary owns (its fetch thread runs
    // `vike_tradehub_client::settings_show`); `settings_refresh` is the out-slot it drains.
    let active_name =
        picker.active.map(|r| if r.name.is_empty() { "(--observe)" } else { r.name.as_str() });
    backend_settings_section(
        ui,
        active_name,
        settings,
        settings_refresh,
        settings_edit,
        settings_write,
    );
}

/// The editor body under the Backends rows — RENDER ONLY: reads the [`EditorState`], draws the
/// are-you-sure line or the Add/Edit form, and forwards Save/Confirm through the pure
/// `backend_editor` decisions into the `registry` OUT slot. Click flags are collected first and
/// applied after the `match`, because the state transitions need `&mut EditorState` while the
/// arms hold borrows into it.
fn backend_editor_ui(
    ui: &mut egui::Ui,
    editor: &mut EditorState,
    vars: &HashMap<String, String>,
    picker: &BackendPicker<'_>,
    registry: &mut Option<backend_editor::RegistryUpdate>,
) {
    let mut save_clicked = false;
    let mut confirm_clicked = false;
    let mut cancel_clicked = false;
    match editor {
        EditorState::Closed => {}
        EditorState::ConfirmDelete { name } => {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(format!("Delete backend {name:?}?"));
                if backend_editor::delete_disconnects(name, picker.active) {
                    ui.weak("— the ACTIVE backend: deleting disconnects it first");
                }
                confirm_clicked = ui.button("Delete").clicked();
                cancel_clicked = ui.button("Cancel").clicked();
            });
        }
        EditorState::Add { form, errors } => {
            let clicks = backend_form_ui(ui, "Add backend", form, errors, vars, false);
            (save_clicked, cancel_clicked) = clicks;
        }
        EditorState::Edit { original, form, errors } => {
            let defers = backend_editor::edit_defers(original, picker.active);
            let title = format!("Edit backend {original:?}");
            let clicks = backend_form_ui(ui, &title, form, errors, vars, defers);
            (save_clicked, cancel_clicked) = clicks;
        }
    }
    if cancel_clicked {
        editor.cancel();
    } else if save_clicked {
        if let Some(upd) = backend_editor::submit(editor, picker.backends) {
            *registry = Some(upd);
        }
    } else if confirm_clicked {
        if let Some(upd) = backend_editor::confirm_delete(editor, picker.backends, picker.active) {
            *registry = Some(upd);
        }
    }
}

/// The Add/Edit form body: the five fields, the B7 names-not-values hint, the per-key-name
/// presence indicator, the edit-active deferral line, and the previous Save's refusals. Returns
/// `(save_clicked, cancel_clicked)`.
fn backend_form_ui(
    ui: &mut egui::Ui,
    title: &str,
    form: &mut BackendForm,
    errors: &[FormError],
    vars: &HashMap<String, String>,
    defers: bool,
) -> (bool, bool) {
    ui.add_space(6.0);
    ui.strong(title);
    // B7, restated where the operator is typing: NAMES, never key material.
    ui.weak(backend_editor::KEY_NAME_HINT);
    ui.horizontal(|ui| {
        ui.label("name");
        ui.text_edit_singleline(&mut form.name);
    });
    ui.horizontal(|ui| {
        ui.label("addr");
        ui.text_edit_singleline(&mut form.addr);
        ui.weak("host:port");
    });
    ui.horizontal(|ui| {
        ui.label("observe key name");
        ui.text_edit_singleline(&mut form.observe_key);
        key_presence_badge(ui, backend_editor::key_presence(&form.observe_key, vars));
    });
    ui.horizontal(|ui| {
        ui.label("control key name");
        ui.text_edit_singleline(&mut form.control_key);
        ui.weak("(optional)");
        key_presence_badge(ui, backend_editor::key_presence(&form.control_key, vars));
    });
    ui.checkbox(&mut form.control, "control armed (write channel)");
    if backend_editor::armed_without_key(form) {
        ui.weak("armed but no control key named — the write channel can never mount");
    }
    if defers {
        ui.weak(
            "editing the ACTIVE backend — changes take effect on next connect; \
             the live connection is not rewired",
        );
    }
    for e in errors {
        ui.colored_label(egui::Color32::from_rgb(0xe5, 0x6a, 0x6a), e.to_string());
    }
    let mut clicks = (false, false);
    ui.horizontal(|ui| {
        clicks = (ui.button("Save").clicked(), ui.button("Cancel").clicked());
    });
    clicks
}

/// The typo-catcher's badge — presence only, never a value (`backend_editor::key_presence`'s
/// return type has no value-carrying variant, so this CANNOT print one).
fn key_presence_badge(ui: &mut egui::Ui, presence: KeyPresence) {
    match presence {
        KeyPresence::Blank => {}
        KeyPresence::Present => {
            ui.weak("✓ in store");
        }
        KeyPresence::Absent => {
            ui.weak("⚠ not in store");
        }
    }
}
