//! The WRITE half: the per-row edit flow and its pure transitions.

use vike_tradehub_client::wire::WireSettingsRow;

#[cfg(doc)]
use super::settings_fetch_state;

// ---------------------------------------------------------------------------------------------
// The WRITE half (split-plane REQ-7): the per-row edit flow
// ---------------------------------------------------------------------------------------------

/// The line rendered against a node whose `Welcome.features` does not advertise
/// `"settings-write"` — the write verb refused CLIENT-side inside
/// `vike_tradehub_client::set_setting`, nothing on the wire.
pub const PREDATES_SETTINGS_WRITE: &str =
    "server predates settings-write — update the backend daemon to edit its settings here";

/// The accepted-write banner, rendered when the node answered `restart_required: true`. ⚠ NOT
/// every write: the daemon decides per key (`crates/vike-tradehub/src/hot_reload.rs`'s
/// `classify`), and the two `preferences` log-level keys apply hot.
pub const SAVED_RESTART_NOTE: &str = "saved — restart the backend to apply";

/// One requested settings write — the out-slot payload the binary drains after the frame (the
/// `BackendAction` idiom) and hands to `vike_tradehub_client::set_setting` against the ACTIVE
/// backend, folding the outcome back in via [`settings_write_state`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsWriteRequest {
    /// The row's `section` LABEL as the wire renders it (`"config"`) — passed through to the
    /// wire's `file`, which the daemon does not consult: it derives the section from `key`, and it
    /// opens no file.
    pub file: String,
    /// The full dotted key (`"config.tradehub_addr"`).
    pub key: String,
    /// The new value, as the operator typed it.
    pub value: String,
}

/// The section's EDIT flow — one row at a time, owned by the UI thread (its text buffer is
/// live-edited every frame) and advanced by pure transitions so the whole
/// edit → saving → saved/failed walk is CI-testable without a window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SettingsEditState {
    /// No row is being edited.
    #[default]
    Idle,
    /// The operator is editing one row. `value` starts as the row's current effective value.
    Editing {
        /// The row's `section` label, carried into [`SettingsWriteRequest::file`].
        file: String,
        /// The row's full dotted key.
        key: String,
        /// The value text buffer.
        value: String,
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
        /// The reply's restart-to-apply signal, decided SERVER-side per key — not always `true`.
        restart_required: bool,
    },
    /// The write was refused (the daemon's own message — the loader's bounds text, a store that
    /// is not sound, …) or failed in transport; the operator can re-edit from here.
    Failed {
        /// The key whose write failed.
        key: String,
        /// The refusal/fault, rendered verbatim.
        error: String,
    },
}

/// Begin editing one row: the value buffer starts at the row's current rendered value.
pub fn start_edit(row: &WireSettingsRow) -> SettingsEditState {
    SettingsEditState::Editing {
        file: row.section.clone(),
        key: row.key.clone(),
        value: row.value.clone(),
    }
}

/// Take the Save transition: `Editing` → `Saving`, yielding the [`SettingsWriteRequest`] for the
/// binary's out-slot. EVERY row takes it — no key, the policy ceilings included, demands a typed
/// confirm (0086 point 7). Any other state yields `None` and is left unchanged, so a double-click
/// on Save, or a click landing after the outcome folded, cannot enqueue a second write.
pub fn take_save(state: &mut SettingsEditState) -> Option<SettingsWriteRequest> {
    match std::mem::take(state) {
        SettingsEditState::Editing { file, key, value } => {
            *state = SettingsEditState::Saving { key: key.clone() };
            Some(SettingsWriteRequest { file, key, value })
        }
        other => {
            *state = other;
            None
        }
    }
}

/// Fold a finished write into the flow state — [`settings_fetch_state`]'s twin for the write
/// verb: `Ok(restart_required)` ⇒ `Saved`; the client-side feature refusal (`Unsupported`) ⇒
/// `Failed` with [`PREDATES_SETTINGS_WRITE`]; every other fault ⇒ `Failed` with its text (the
/// daemon's refusals — the loader's message — arrive here verbatim).
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
/// `Saved` flow — the table should show the new row value beside [`SAVED_RESTART_NOTE`]. A
/// refusal changed no byte, so there is nothing new to fetch.
pub fn should_refetch_after_write(state: &SettingsEditState) -> bool {
    matches!(state, SettingsEditState::Saved { .. })
}
