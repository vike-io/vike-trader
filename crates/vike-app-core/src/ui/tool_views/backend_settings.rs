//! The Connections tool's **Backend settings** section (split-plane REQ-7) — the Backends
//! picker's sibling: a table of the ACTIVE backend's effective settings rows, fetched over
//! the node wire (`vike_tradehub_client::settings_show`), rendered here — and, since the WRITE
//! half landed, EDITABLE: each row carries an edit affordance driving [`SettingsEditState`], a
//! pure per-row edit flow whose accepted writes go over the wire as `WireCommand::SetSetting`
//! (`vike_tradehub_client::set_setting`) and land as ONE ROW of the node's settings database
//! (`docs/decisions/0086-settings-live-only-in-the-database.md` — no file is a source, a fallback
//! or a target).
//!
//! Same seam as every extracted tool body: the PURE state machines and the renderer live in this
//! CI-tested crate; the I/O (the per-call `settings_show`/`set_setting` connections, the
//! credential lookups for the observe/control keys) stays in the shell (`vike-desktop`), which
//! drains the `refresh` and [`SettingsWriteRequest`] out-slots after the frame — the
//! [`crate::backend::backend_conn::BackendAction`] deferred-mutation idiom.
//!
//! # No typed confirm, for any key (0086 point 7)
//!
//! A `policy.toml` row's Save used to stay DISABLED until the operator had retyped the row's exact
//! dotted key. `docs/decisions/0086-settings-live-only-in-the-database.md` point 7 deletes that
//! ceremony for EVERY key, the risk ceilings included (*"confirmation over confirmation … a
//! nightmare"*), and the daemon's half went first: `crates/vike-tradehub/src/server/settings.rs`'s
//! `apply_set_setting` never reads the wire's `confirm`. What guards a live limit is what runs on
//! every write whoever sends it — the loader's bounds check before the row commits, and the
//! old → new record the daemon journals after. So every row saves on one click, and this side
//! sends no `confirm`.
//!
//! # Restart-to-apply — ⚠ NOT always
//!
//! An accepted write answers `restart_required` and the section renders [`SAVED_RESTART_NOTE`]
//! when it is `true`. **This paragraph said "always `true` in v1" and that has been false since
//! REQ-7 v2**: the flag is decided SERVER-side by `crates/vike-tradehub/src/hot_reload.rs`'s
//! `classify`, the shipped daemon wires the seam, and the hot set is exactly
//! `preferences.log_level` and `preferences.log_file_level` — applied on the summary tick within
//! `HOT_APPLY_DEADLINE`. A timeout, a failed apply or an unwired seam all answer `true`;
//! the `policy` section answers `Restart` before the table is consulted (the sealed-policy
//! doctrine). The code was always right; only this prose was stale, and a design written from it would have
//! hard-coded a note the daemon does not always ask for.
//!
//! # ⚠ What an accepted write does NOT mean, rendered
//!
//! **The environment may outrank the row.** `vike_config`'s precedence is
//! `env > the settings database > default` (`vike_config::precedence_line`) for `config`/
//! `preferences`/`flags`. Writing the row under an `env:VAR` origin is accepted, returns
//! `restart_required: true`, and changes nothing the daemon will read — the refetch shows the same
//! `env:VAR` and the same old value, which looks like the write having failed. [`env_shadow`] is
//! the computable test, and the editor's Save button relabels itself accordingly. `policy.*` can
//! never be shadowed ([`env_shadow`]'s own doc says why).
//!
//! ⚠ There were two such things. The second, a `SANDBOX_WRITE_NOTE` beside Save on every row, said
//! a deployed daemon's `settings/*.toml` was read-only and the write would refuse with EROFS. It
//! described a file nothing writes: the daemon lands one row through the `settings/db` grant
//! `deploy/vike-tradehub.service` carries (0086 point 6), so the note is DELETED. A refusal of any
//! kind still reaches the operator verbatim, through [`SettingsEditState::Failed`].

use vike_tradehub_client::wire::WireSettingsShow;

mod edit;
mod editor;
mod rows;
mod section;
mod table;

pub use edit::{
    PREDATES_SETTINGS_WRITE, SAVED_RESTART_NOTE, SettingsEditState, SettingsWriteRequest,
    settings_write_state, should_refetch_after_write, start_edit, take_save,
};
pub use rows::{
    BackendDigest, SettingsFilter, UNREAD_VERDICT_POINTER, env_shadow, origin_kind, row_is_finding,
    row_is_set, row_read_by_nothing, write_target,
};
pub use section::{SECTION_TITLE, backend_settings_section};

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
///
/// # ⚠ Called by the TOOL, not by the section — and that is the fix for a real regression
///
/// This used to be called from inside [`backend_settings_section`] and nowhere else, with the
/// argument that "a fetch only ever starts while the section is actually on screen". That held
/// while the Connections window was four stacked sections on one scroll. The two-tab redesign made
/// the section conditional on `ConnectionsTab::Backend`, whose default is `Credentials` — so on a
/// connected backend, opening the window and staying on the default tab left the slot `Idle` for
/// ever while the status line's digested Backend half and the Backend segment's badge, BOTH drawn
/// on both tabs, described a fetch nothing had asked for.
///
/// So the caller is `super::data`'s `data_body`, before the per-destination dispatch. The SCOPE
/// argument survives intact one level up: the fetch still only starts while a destination that
/// renders backend state is being drawn, and the thing that renders the answer (the chrome) is now
/// the thing that asks the question. Making the digest merely *able* to say "not fetched" was the
/// alternative and
/// is not sufficient on its own — it would replace a plausible sentence with an honest one and
/// still leave the badge permanently blank on the tab an operator is looking at. Both landed:
/// [`BackendDigest::NotFetched`] is the belt, this relocation is the fix.
pub fn should_fetch_settings(has_active_backend: bool, state: &BackendSettingsState) -> bool {
    has_active_backend && *state == BackendSettingsState::Idle
}

// ⚠ THE TYPED CONFIRM stood here, and is DELETED rather than disabled
// (`docs/decisions/0086-settings-live-only-in-the-database.md` point 7). It was `can_save` /
// `can_save_fields` — the Save gate that held a policy row's button disabled until its exact dotted
// key was retyped — over `demands_typed_confirm`, a policy-plane predicate on
// `vike_config::is_policy_plane_key` (itself the repair of an earlier `is_policy_file`, an exact
// match of the row's `section` against `const POLICY_FILE`), plus a `confirm` buffer on
// `SettingsEditState::Editing` and on `SettingsWriteRequest`. The daemon's half went first —
// `apply_set_setting` never reads the wire's `confirm` — so this half was a ceremony nothing
// downstream enforced. The names stay in this comment because older records cite them.

#[path = "backend_settings_tests.rs"]
#[cfg(test)]
mod backend_settings_tests;
