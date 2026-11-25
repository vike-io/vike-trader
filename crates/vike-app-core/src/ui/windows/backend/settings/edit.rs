//! The WRITE half: the per-row edit flow and its pure transitions.

use vike_hub_protocol::call::HubCallError;
use vike_tradehub_client::UnansweredCommand;
use vike_tradehub_client::wire::SettingsShowRowWire;

#[cfg(doc)]
use super::settings_fetch_state;

// ---------------------------------------------------------------------------------------------
// The WRITE half (split-plane REQ-7): the per-row edit flow
// ---------------------------------------------------------------------------------------------

/// The line rendered against a node whose `Welcome.features` does not advertise
/// `"settings-write"` — the write verb refused CLIENT-side inside
/// `vike_tradehub_client::set_setting`, nothing on the wire.
pub(crate) const PREDATES_SETTINGS_WRITE: &str =
    "server predates settings-write — update the backend daemon to edit its settings here";

/// The accepted-write banner, rendered when the node answered `restart_required: true`. ⚠ NOT
/// every write: the daemon decides per key (`crates/vike-tradehub/src/hot_reload.rs`'s
/// `classify`), and the two `preferences` log-level keys apply hot.
pub(crate) const SAVED_RESTART_NOTE: &str = "saved — restart the backend to apply";

/// One requested settings write — the out-slot payload the binary drains after the frame (the
/// `BackendAction` idiom) and hands to `vike_tradehub_client::set_setting` against the ACTIVE
/// backend, folding the outcome back in via [`settings_write_state`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSettingsWriteRequest {
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
pub enum BackendSettingsEditState {
    /// No row is being edited.
    #[default]
    Idle,
    /// The operator is editing one row. `value` starts as the row's current effective value.
    Editing {
        /// The row's `section` label, carried into [`BackendSettingsWriteRequest::file`].
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
    /// The node accepted the write. `restart_required` ⇒ render `SAVED_RESTART_NOTE`.
    Saved {
        /// The key that was written.
        key: String,
        /// The reply's restart-to-apply signal, decided SERVER-side per key — not always `true`.
        restart_required: bool,
    },
    /// The write was refused (the daemon's own message — the loader's bounds text, a store that
    /// is not sound, …) or failed before it was sent; the operator can re-edit from here.
    Failed {
        /// The key whose write failed.
        key: String,
        /// The refusal/fault, rendered verbatim.
        error: String,
    },
    /// The write went out and its answer was lost (`UnansweredCommand::of_settings_write`):
    /// OUTCOME UNKNOWN, never shown as saved or refused, in `unanswered`'s one wording. The node may
    /// hold the new value, so the table is read again: a read settles it (decision 0303).
    Unanswered {
        /// The key whose write has no answer.
        key: String,
        /// Which unanswered case, and its words.
        unanswered: UnansweredCommand,
    },
}

/// Begin editing one row: the value buffer starts at the row's current rendered value.
pub(crate) fn start_edit(row: &SettingsShowRowWire) -> BackendSettingsEditState {
    BackendSettingsEditState::Editing {
        file: row.section.clone(),
        key: row.key.clone(),
        value: row.value.clone(),
    }
}

/// Take the Save transition: `Editing` → `Saving`, yielding the [`BackendSettingsWriteRequest`] for the
/// binary's out-slot. EVERY row takes it — no key, the policy ceilings included, demands a typed
/// confirm (0086 point 7). Any other state yields `None` and is left unchanged, so a double-click
/// on Save, or a click landing after the outcome folded, cannot enqueue a second write.
pub(crate) fn take_save(
    state: &mut BackendSettingsEditState,
) -> Option<BackendSettingsWriteRequest> {
    match std::mem::take(state) {
        BackendSettingsEditState::Editing { file, key, value } => {
            *state = BackendSettingsEditState::Saving { key: key.clone() };
            Some(BackendSettingsWriteRequest { file, key, value })
        }
        other => {
            *state = other;
            None
        }
    }
}

/// Fold a finished write into the flow state — [`settings_fetch_state`]'s twin for the write
/// verb: `Ok(restart_required)` ⇒ `Saved`; the client-side feature refusal (`NotOffered`) ⇒
/// `Failed` with `PREDATES_SETTINGS_WRITE`; a write whose answer was lost (`OutcomeUnknown`) ⇒
/// `Unanswered`; every other fault ⇒ `Failed` with its text (the daemon's refusals — the loader's
/// message — arrive here verbatim). An error made on this side before the send is `NotSent`, as
/// `crate::backend::registry`'s signers make it.
pub fn settings_write_state(
    key: &str,
    result: Result<bool, HubCallError>,
) -> BackendSettingsEditState {
    match result {
        Ok(restart_required) => {
            BackendSettingsEditState::Saved { key: key.to_string(), restart_required }
        }
        Err(HubCallError::NotOffered { .. }) => BackendSettingsEditState::Failed {
            key: key.to_string(),
            error: PREDATES_SETTINGS_WRITE.to_string(),
        },
        Err(e) => match UnansweredCommand::of_settings_write(&e) {
            Some(unanswered) => {
                BackendSettingsEditState::Unanswered { key: key.to_string(), unanswered }
            }
            None => BackendSettingsEditState::Failed { key: key.to_string(), error: e.to_string() },
        },
    }
}

/// Whether the binary should refresh the settings FETCH after folding a write outcome: a `Saved`
/// flow (the table should show the new row value beside `SAVED_RESTART_NOTE`) and an `Unanswered`
/// one (the row the node now holds is what settles it). A refusal changed no byte, so there is
/// nothing new to fetch.
pub fn should_refetch_after_write(state: &BackendSettingsEditState) -> bool {
    matches!(
        state,
        BackendSettingsEditState::Saved { .. } | BackendSettingsEditState::Unanswered { .. }
    )
}
