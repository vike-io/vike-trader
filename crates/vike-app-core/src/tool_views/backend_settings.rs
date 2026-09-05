//! The Connections tool's **Backend settings** section (split-plane REQ-7) — the Backends
//! picker's sibling: a table of the ACTIVE backend's effective settings-file rows, fetched over
//! the node wire (`vike_tradehub_client::settings_show`), rendered here — and, since the WRITE
//! half landed, EDITABLE: each row carries an edit affordance driving [`SettingsEditState`], a
//! pure per-row edit flow whose accepted writes go over the wire as `WireCommand::SetSetting`
//! (`vike_tradehub_client::set_setting`).
//!
//! Same seam as every extracted tool body: the PURE state machines and the renderer live in this
//! CI-tested crate; the I/O (the per-call `settings_show`/`set_setting` connections, the
//! credential lookups for the observe/control keys) stays in `vike-app`, which drains the
//! `refresh` and [`SettingsWriteRequest`] out-slots after the frame — the
//! [`crate::backend_conn::BackendAction`] deferred-mutation idiom.
//!
//! # The policy TYPED-CONFIRM (the REQ-7 ratified contract)
//!
//! A `policy.toml` row's Save stays DISABLED until the operator has TYPED the row's exact dotted
//! key into the confirm box — [`can_save`] is the one decision site, and the confirm box is never
//! pre-filled (pre-filling would reduce the ceremony to a click, which is precisely what the
//! contract exists to prevent). The daemon enforces the same rule server-side
//! (`crates/vike-tradehub/src/server.rs`'s `apply_set_setting`), so this UI cannot be the only
//! thing standing between a click and a ceiling change.
//!
//! # Restart-to-apply (v1)
//!
//! An accepted write answers `restart_required` (always `true` in v1) and the section renders
//! [`SAVED_RESTART_NOTE`] — the running backend keeps its boot-time values until restarted. The
//! binary refreshes the settings fetch after a save, so the table shows the NEW file value (with
//! its origin) alongside that note.

use vike_tradehub_client::wire::{WireSettingsRow, WireSettingsShow};

/// The line rendered against a node whose `Welcome.features` does not advertise
/// `"settings-show"` — the client verb refused CLIENT-side and nothing went on the wire.
pub const PREDATES_SETTINGS_SHOW: &str =
    "server predates settings-show — update the backend daemon to read its settings here";

/// The Backend-settings section's render state — one per ACTIVE backend connection, owned by the
/// binary (an `Arc<Mutex<..>>` slot its fetch thread writes) and handed here per frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BackendSettingsState {
    /// Nothing fetched yet for this backend — the section shows a loading line and the binary
    /// auto-fetches ([`should_fetch_settings`]).
    #[default]
    Idle,
    /// A background fetch is in flight.
    Pending,
    /// The node answered: the rendered rows, verbatim from the wire (already redacted server-side).
    Loaded(WireSettingsShow),
    /// The node does not advertise the capability ([`PREDATES_SETTINGS_SHOW`]).
    Unsupported,
    /// Transport/handshake/server fault, stringified.
    Error(String),
}

/// Fold a finished fetch into the render state — the ONE mapping from the client verb's error
/// vocabulary to the section's: `Unsupported` (the client-side feature refusal) becomes the
/// "server predates settings-show" state; every other fault is rendered as its text.
pub fn settings_fetch_state(result: std::io::Result<WireSettingsShow>) -> BackendSettingsState {
    match result {
        Ok(show) => BackendSettingsState::Loaded(show),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => BackendSettingsState::Unsupported,
        Err(e) => BackendSettingsState::Error(e.to_string()),
    }
}

/// Whether the binary should START a fetch this frame: an active backend whose slot is still
/// [`BackendSettingsState::Idle`] (first sight of this connection). `Pending` guards re-entry;
/// `Loaded`/`Unsupported`/`Error` stay until the operator clicks Refresh or the backend changes
/// (the binary keys its slot by backend addr, so a switch resets to `Idle`).
pub fn should_fetch_settings(has_active_backend: bool, state: &BackendSettingsState) -> bool {
    has_active_backend && *state == BackendSettingsState::Idle
}

// ---------------------------------------------------------------------------------------------
// The WRITE half (split-plane REQ-7): the per-row edit flow
// ---------------------------------------------------------------------------------------------

/// The line rendered against a node whose `Welcome.features` does not advertise
/// `"settings-write"` — the write verb refused CLIENT-side inside
/// `vike_tradehub_client::set_setting`, nothing on the wire.
pub const PREDATES_SETTINGS_WRITE: &str =
    "server predates settings-write — update the backend daemon to edit its settings here";

/// The accepted-write banner (v1: every write is restart-to-apply).
pub const SAVED_RESTART_NOTE: &str = "saved — restart the backend to apply";

/// The settings FILE whose rows demand the typed confirm. Compared against
/// `WireSettingsRow::section`, which the read half renders as the file name.
const POLICY_FILE: &str = "policy.toml";

/// One requested settings write — the out-slot payload the binary drains after the frame (the
/// `BackendAction` idiom) and hands to `vike_tradehub_client::set_setting` against the ACTIVE
/// backend, folding the outcome back in via [`settings_write_state`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsWriteRequest {
    /// The settings file, as the row's `section` renders it (`"config.toml"`).
    pub file: String,
    /// The full dotted key (`"config.tradehub_addr"`).
    pub key: String,
    /// The new value, as the operator typed it.
    pub value: String,
    /// The TYPED confirm for a policy row (must equal `key` — [`can_save`] already required it);
    /// `None` for every other file.
    pub confirm: Option<String>,
}

/// The section's EDIT flow — one row at a time, owned by the UI thread (its text buffers are
/// live-edited every frame) and advanced by pure transitions so the whole
/// edit → confirm → saving → saved/failed walk is CI-testable without a window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SettingsEditState {
    /// No row is being edited.
    #[default]
    Idle,
    /// The operator is editing one row. `value` starts as the row's current effective value;
    /// `confirm` ALWAYS starts empty (the typed-confirm contract — never pre-filled).
    Editing {
        /// The row's file (`section`), deciding whether the confirm box renders.
        file: String,
        /// The row's full dotted key.
        key: String,
        /// The value text buffer.
        value: String,
        /// The confirm text buffer (policy rows only; ignored otherwise).
        confirm: String,
    },
    /// The write is on the wire (the binary's worker owns it); re-entry is guarded.
    Saving {
        /// The key being written.
        key: String,
    },
    /// The node accepted the write. `restart_required` ⇒ render [`SAVED_RESTART_NOTE`].
    Saved {
        /// The key that was written.
        key: String,
        /// The reply's restart-to-apply signal (always `true` in v1).
        restart_required: bool,
    },
    /// The write was refused (the daemon's own message — the loader's text, the confirm
    /// contract, …) or failed in transport; the operator can re-edit from here.
    Failed {
        /// The key whose write failed.
        key: String,
        /// The refusal/fault, rendered verbatim.
        error: String,
    },
}

/// Begin editing one row: the value buffer starts at the row's current rendered value, the
/// confirm buffer EMPTY (the typed-confirm contract — the operator types the key, the UI never
/// types it for them).
pub fn start_edit(row: &WireSettingsRow) -> SettingsEditState {
    SettingsEditState::Editing {
        file: row.section.clone(),
        key: row.key.clone(),
        value: row.value.clone(),
        confirm: String::new(),
    }
}

/// Whether this file's rows demand the typed confirm (`policy.toml` — the risk ceilings).
pub fn is_policy_file(file: &str) -> bool {
    file == POLICY_FILE
}

/// THE ONE Save gate: a non-policy edit may always save; a policy edit only once the typed
/// confirm equals the row's exact dotted key. Anything but `Editing` cannot save.
pub fn can_save(state: &SettingsEditState) -> bool {
    match state {
        SettingsEditState::Editing { file, key, confirm, .. } => {
            can_save_fields(file, key, confirm)
        }
        _ => false,
    }
}

/// [`can_save`] over the destructured fields — the same one decision, reachable from inside the
/// renderer's `match` arm (where the state is already mutably borrowed for its text buffers).
///
/// ⚠ `pub(crate)` because it is THE typed-confirm decision for BOTH GUI write surfaces: this one
/// (the wire path) and [`super::venues`] (the Data Manager's LOCAL arming write). Reused rather
/// than forked, so a change to the ceremony moves both at once — and the daemon enforces the same
/// rule server-side in `crates/vike-tradehub/src/server.rs`'s `apply_set_setting`.
pub(crate) fn can_save_fields(file: &str, key: &str, confirm: &str) -> bool {
    !is_policy_file(file) || confirm == key
}

/// Take the Save transition: `Editing` (passing [`can_save`]) → `Saving`, yielding the
/// [`SettingsWriteRequest`] for the binary's out-slot — with `confirm` attached ONLY for a
/// policy row. Any other state (or a policy edit whose confirm is wrong) yields `None` and
/// leaves the state unchanged.
pub fn take_save(state: &mut SettingsEditState) -> Option<SettingsWriteRequest> {
    if !can_save(state) {
        return None;
    }
    let SettingsEditState::Editing { file, key, value, confirm } = std::mem::take(state) else {
        unreachable!("can_save admits only Editing");
    };
    *state = SettingsEditState::Saving { key: key.clone() };
    let confirm = is_policy_file(&file).then_some(confirm);
    Some(SettingsWriteRequest { file, key, value, confirm })
}

/// Fold a finished write into the flow state — [`settings_fetch_state`]'s twin for the write
/// verb: `Ok(restart_required)` ⇒ `Saved`; the client-side feature refusal (`Unsupported`) ⇒
/// `Failed` with [`PREDATES_SETTINGS_WRITE`]; every other fault ⇒ `Failed` with its text (the
/// daemon's refusals — the loader's message, the confirm contract — arrive here verbatim).
pub fn settings_write_state(key: &str, result: std::io::Result<bool>) -> SettingsEditState {
    match result {
        Ok(restart_required) => SettingsEditState::Saved { key: key.to_string(), restart_required },
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => SettingsEditState::Failed {
            key: key.to_string(),
            error: PREDATES_SETTINGS_WRITE.to_string(),
        },
        Err(e) => SettingsEditState::Failed { key: key.to_string(), error: e.to_string() },
    }
}

/// Whether the binary should refresh the settings FETCH after folding a write outcome: only a
/// `Saved` flow — the table should show the new file value beside [`SAVED_RESTART_NOTE`]. A
/// refusal changed no byte, so there is nothing new to fetch.
pub fn should_refetch_after_write(state: &SettingsEditState) -> bool {
    matches!(state, SettingsEditState::Saved { .. })
}

/// The section body — the Backends picker's sibling in the Connections tool. Renders the ACTIVE
/// backend's state; `refresh` is the fetch out-slot the binary drains after the frame (set ⇒
/// start a fetch). The section sets it ITSELF for the auto-fetch case
/// ([`should_fetch_settings`]: an `Idle` active backend), so a fetch only ever starts while the
/// section is actually on screen — and on an explicit Refresh click, whatever the state.
///
/// The WRITE half: `edit` is the per-row edit flow (UI-thread-owned — its text buffers are
/// live), `write` the out-slot an accepted Save fills ([`take_save`]); the binary drains it
/// after the frame, runs the wire write, and folds the outcome back with
/// [`settings_write_state`]. Every decision is a pure function above — this body only renders.
pub fn backend_settings_section(
    ui: &mut egui::Ui,
    active_backend: Option<&str>,
    state: &BackendSettingsState,
    refresh: &mut bool,
    edit: &mut SettingsEditState,
    write: &mut Option<SettingsWriteRequest>,
) {
    ui.add_space(10.0);
    ui.separator();
    ui.strong("Backend settings");
    let Some(name) = active_backend else {
        ui.weak("no active backend — connect one above to read its effective settings");
        return;
    };
    if should_fetch_settings(true, state) {
        *refresh = true;
    }
    ui.horizontal(|ui| {
        ui.weak(format!("effective settings of {name} (edits are restart-to-apply)"));
        if ui.small_button("Refresh").clicked() {
            *refresh = true;
        }
    });
    match state {
        BackendSettingsState::Idle | BackendSettingsState::Pending => {
            ui.weak("loading backend settings…");
        }
        BackendSettingsState::Unsupported => {
            ui.weak(PREDATES_SETTINGS_SHOW);
        }
        BackendSettingsState::Error(e) => {
            ui.colored_label(ui.visuals().warn_fg_color, format!("settings unavailable: {e}"));
        }
        BackendSettingsState::Loaded(show) => {
            match show.settings_dir.as_deref() {
                Some(dir) => ui.monospace(format!("settings dir: {dir}")),
                None => ui.weak("no settings directory — the node runs on compiled-in defaults"),
            };
            settings_edit_panel(ui, edit, write);
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                egui::Grid::new("backend-settings-grid").striped(true).show(ui, |ui| {
                    ui.strong("SETTING");
                    ui.strong("VALUE");
                    ui.strong("ORIGIN");
                    ui.strong("READ");
                    ui.strong("");
                    ui.end_row();
                    for row in &show.rows {
                        ui.monospace(&row.key);
                        ui.monospace(if row.value.is_empty() { "-" } else { row.value.as_str() });
                        ui.label(&row.origin);
                        ui.label(&row.read_by);
                        if ui.small_button("edit").clicked() {
                            *edit = start_edit(row);
                        }
                        ui.end_row();
                    }
                });
            });
        }
    }
}

/// The edit flow's own panel, rendered between the header and the grid whenever a row is being
/// edited (or a write is in flight / just finished). Renders [`SettingsEditState`]; every
/// decision (may this save? what does the outcome mean?) is a pure function above.
fn settings_edit_panel(
    ui: &mut egui::Ui,
    edit: &mut SettingsEditState,
    write: &mut Option<SettingsWriteRequest>,
) {
    // Clicks are staged and applied AFTER the match: inside an arm the state is mutably borrowed
    // for its live text buffers, so the transitions ([`take_save`], the reset to `Idle`) run once
    // that borrow ends — the same deferred-mutation shape the section's own out-slots use.
    let mut do_save = false;
    let mut dismiss = false;
    match edit {
        SettingsEditState::Idle => {}
        SettingsEditState::Editing { file, key, value, confirm } => {
            let policy = is_policy_file(file);
            ui.horizontal(|ui| {
                ui.monospace(key.as_str());
                ui.label("=");
                ui.add(egui::TextEdit::singleline(value).desired_width(220.0));
            });
            if policy {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "policy ceiling — type the key name to confirm:",
                    );
                    // `hint_text` shows the expected spelling; the BUFFER stays empty — the
                    // operator types it (the contract), a hint is not a pre-fill.
                    ui.add(
                        egui::TextEdit::singleline(confirm)
                            .hint_text(key.as_str())
                            .desired_width(220.0),
                    );
                });
            }
            let allowed = can_save_fields(file, key, confirm);
            ui.horizontal(|ui| {
                if ui.add_enabled(allowed, egui::Button::new("Save")).clicked() {
                    do_save = true;
                }
                if ui.small_button("Cancel").clicked() {
                    dismiss = true;
                }
            });
        }
        SettingsEditState::Saving { key } => {
            ui.weak(format!("saving {key}…"));
        }
        SettingsEditState::Saved { key, restart_required } => {
            let note = if *restart_required {
                format!("{key}: {SAVED_RESTART_NOTE}")
            } else {
                format!("{key}: saved")
            };
            ui.colored_label(ui.visuals().strong_text_color(), note);
            if ui.small_button("OK").clicked() {
                dismiss = true;
            }
        }
        SettingsEditState::Failed { key, error } => {
            ui.colored_label(ui.visuals().warn_fg_color, format!("{key}: write refused — {error}"));
            if ui.small_button("Dismiss").clicked() {
                dismiss = true;
            }
        }
    }
    if do_save {
        // `take_save` re-checks the gate and transitions to `Saving`; the binary drains the
        // request after the frame.
        if let Some(req) = take_save(edit) {
            *write = Some(req);
        }
    } else if dismiss {
        *edit = SettingsEditState::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use vike_tradehub_client::wire::WireSettingsRow;

    fn show() -> WireSettingsShow {
        WireSettingsShow {
            settings_dir: Some("/srv/node/settings".into()),
            rows: vec![WireSettingsRow {
                section: "config.toml".into(),
                key: "config.tradehub_addr".into(),
                value: "127.0.0.1:7879".into(),
                origin: "config.toml".into(),
                read_by: "tradehub".into(),
            }],
        }
    }

    /// The three fetch outcomes map onto the three render states — and the feature refusal
    /// (`Unsupported`) is what the section renders as "server predates settings-show".
    #[test]
    fn fetch_outcomes_map_to_render_states() {
        assert_eq!(settings_fetch_state(Ok(show())), BackendSettingsState::Loaded(show()));
        assert_eq!(
            settings_fetch_state(Err(io::Error::new(io::ErrorKind::Unsupported, "no feature"))),
            BackendSettingsState::Unsupported
        );
        let e =
            settings_fetch_state(Err(io::Error::new(io::ErrorKind::PermissionDenied, "bad mac")));
        match e {
            BackendSettingsState::Error(msg) => assert!(msg.contains("bad mac")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// Auto-fetch fires exactly once per backend: only an ACTIVE backend in the `Idle` state —
    /// `Pending` guards re-entry, terminal states wait for an explicit Refresh, and no active
    /// backend means nothing to ask.
    #[test]
    fn auto_fetch_fires_only_for_an_idle_active_backend() {
        assert!(should_fetch_settings(true, &BackendSettingsState::Idle));
        assert!(!should_fetch_settings(false, &BackendSettingsState::Idle));
        assert!(!should_fetch_settings(true, &BackendSettingsState::Pending));
        assert!(!should_fetch_settings(true, &BackendSettingsState::Loaded(show())));
        assert!(!should_fetch_settings(true, &BackendSettingsState::Unsupported));
        assert!(!should_fetch_settings(true, &BackendSettingsState::Error("x".into())));
    }

    // ------------------------------------------------------------------------------------------
    // The WRITE half's edit flow (REQ-7)
    // ------------------------------------------------------------------------------------------

    fn policy_row() -> WireSettingsRow {
        WireSettingsRow {
            section: "policy.toml".into(),
            key: "policy.max_notional_per_order".into(),
            value: "500".into(),
            origin: "policy.toml".into(),
            read_by: "tradehub".into(),
        }
    }

    fn config_row() -> WireSettingsRow {
        WireSettingsRow {
            section: "config.toml".into(),
            key: "config.tradehub_addr".into(),
            value: "127.0.0.1:7879".into(),
            origin: "config.toml".into(),
            read_by: "tradehub".into(),
        }
    }

    /// ⚠ THE TYPED-CONFIRM WALK, end to end as pure transitions: a policy row's edit starts with
    /// an EMPTY confirm (never pre-filled), cannot save until the confirm equals the exact key
    /// (a near-miss stays blocked and `take_save` refuses without losing the buffers), saves
    /// with the confirm ATTACHED, and folds the node's acceptance into
    /// `Saved { restart_required }` — the state that renders [`SAVED_RESTART_NOTE`] and tells
    /// the binary to refetch.
    #[test]
    fn a_policy_edit_walks_edit_confirm_saved_restart_required() {
        let mut edit = start_edit(&policy_row());
        match &edit {
            SettingsEditState::Editing { file, key, value, confirm } => {
                assert_eq!(file, "policy.toml");
                assert_eq!(key, "policy.max_notional_per_order");
                assert_eq!(value, "500", "the value buffer starts at the row's current value");
                assert!(confirm.is_empty(), "the confirm buffer is NEVER pre-filled");
            }
            other => panic!("expected Editing, got {other:?}"),
        }
        assert!(!can_save(&edit), "an unconfirmed policy edit cannot save");

        // A near-miss confirm stays blocked, and take_save refuses WITHOUT resetting the flow.
        if let SettingsEditState::Editing { value, confirm, .. } = &mut edit {
            *value = "250".to_string();
            *confirm = "policy.max_notional".to_string();
        }
        assert!(!can_save(&edit));
        assert_eq!(take_save(&mut edit), None, "a blocked save takes nothing");
        assert!(
            matches!(&edit, SettingsEditState::Editing { value, .. } if value == "250"),
            "the operator's buffers survive a blocked save: {edit:?}"
        );

        // The exact key unlocks it; the request carries the confirm; the flow is Saving.
        if let SettingsEditState::Editing { confirm, .. } = &mut edit {
            *confirm = "policy.max_notional_per_order".to_string();
        }
        assert!(can_save(&edit));
        let req = take_save(&mut edit).expect("the confirmed edit saves");
        assert_eq!(
            req,
            SettingsWriteRequest {
                file: "policy.toml".into(),
                key: "policy.max_notional_per_order".into(),
                value: "250".into(),
                confirm: Some("policy.max_notional_per_order".into()),
            }
        );
        assert!(matches!(&edit, SettingsEditState::Saving { key } if key == &req.key), "{edit:?}");

        // The node accepts ⇒ Saved carries restart-to-apply, and the binary refetches.
        edit = settings_write_state(&req.key, Ok(true));
        assert_eq!(
            edit,
            SettingsEditState::Saved {
                key: "policy.max_notional_per_order".into(),
                restart_required: true,
            }
        );
        assert!(should_refetch_after_write(&edit));
    }

    /// A non-policy row needs NO confirm: editable immediately, and its request carries
    /// `confirm: None` (the daemon ignores the field for non-policy files either way).
    #[test]
    fn a_non_policy_row_saves_without_any_confirm() {
        let mut edit = start_edit(&config_row());
        assert!(can_save(&edit), "no typed confirm demanded outside policy.toml");
        if let SettingsEditState::Editing { value, .. } = &mut edit {
            *value = "0.0.0.0:9000".to_string();
        }
        let req = take_save(&mut edit).expect("saves without a confirm");
        assert_eq!(req.confirm, None);
        assert_eq!(req.file, "config.toml");
        assert_eq!(req.value, "0.0.0.0:9000");
    }

    /// The write outcomes fold like the fetch's: acceptance keeps the reply's restart flag, the
    /// client-side feature refusal renders as "server predates settings-write", any other fault
    /// (the daemon's refusal text — the loader's message, the confirm contract) verbatim — and
    /// only a SAVED flow triggers the refetch (a refusal changed no byte).
    #[test]
    fn write_outcomes_map_to_flow_states() {
        let saved = settings_write_state("k", Ok(false));
        assert_eq!(saved, SettingsEditState::Saved { key: "k".into(), restart_required: false });

        let unsupported = settings_write_state(
            "k",
            Err(io::Error::new(io::ErrorKind::Unsupported, "no feature")),
        );
        assert_eq!(
            unsupported,
            SettingsEditState::Failed { key: "k".into(), error: PREDATES_SETTINGS_WRITE.into() }
        );
        assert!(!should_refetch_after_write(&unsupported));

        let refused = settings_write_state(
            "k",
            Err(io::Error::new(io::ErrorKind::InvalidData, "unknown field `tradehub_adr`")),
        );
        match refused {
            SettingsEditState::Failed { error, .. } => {
                assert!(error.contains("tradehub_adr"), "the daemon's text verbatim: {error}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// `take_save` is a no-op outside `Editing` — a double-click on Save, or a click landing
    /// after the outcome folded, cannot enqueue a second write.
    #[test]
    fn take_save_refuses_outside_editing() {
        for mut state in [
            SettingsEditState::Idle,
            SettingsEditState::Saving { key: "k".into() },
            SettingsEditState::Saved { key: "k".into(), restart_required: true },
            SettingsEditState::Failed { key: "k".into(), error: "e".into() },
        ] {
            let before = state.clone();
            assert_eq!(take_save(&mut state), None);
            assert_eq!(state, before, "a refused take leaves the state alone");
        }
    }
}
