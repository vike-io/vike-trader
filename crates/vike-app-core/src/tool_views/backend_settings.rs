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
//! # Restart-to-apply — ⚠ NOT always
//!
//! An accepted write answers `restart_required` and the section renders [`SAVED_RESTART_NOTE`]
//! when it is `true`. **This paragraph said "always `true` in v1" and that has been false since
//! REQ-7 v2**: the flag is decided SERVER-side by `crates/vike-tradehub/src/hot_reload.rs`'s
//! `classify`, the shipped daemon wires the seam, and the hot set is exactly
//! `preferences.log_level` and `preferences.log_file_level` — applied on the summary tick within
//! `HOT_APPLY_DEADLINE`. A timeout, a failed apply or an unwired seam all answer `true`;
//! `policy.toml` answers `Restart` before the table is consulted (the sealed-policy doctrine).
//! The code was always right; only this prose was stale, and a design written from it would have
//! hard-coded a note the daemon does not always ask for.
//!
//! # ⚠ Two things an accepted write does NOT mean, both now rendered
//!
//! 1. **The environment may outrank the file.** `vike_config`'s precedence is
//!    `code defaults -> <project>/settings/*.toml -> env -> CLI` for `config`/`preferences`/
//!    `flags`. Writing the file under an `env:VAR` origin is accepted, returns
//!    `restart_required: true`, and changes nothing the daemon will read — the refetch shows the
//!    same `env:VAR` and the same old value, which looks like the write having failed.
//!    [`env_shadow`] is the computable test, and the editor's Save button relabels itself
//!    accordingly. `policy.*` can never be shadowed ([`env_shadow`]'s own doc says why).
//! 2. **A DEPLOYED daemon cannot perform this write at all.** [`SANDBOX_WRITE_NOTE`] carries the
//!    measurement and the ruling; the note sits beside Save on every row.

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
/// So the caller is [`super::connections::connections_tool_content`], before the tab branch. The
/// SCOPE argument survives intact one level up: the fetch still only starts while the Connections
/// tool is being drawn, and the thing that renders the answer (the chrome) is now the thing that
/// asks the question. Making the digest merely *able* to say "not fetched" was the alternative and
/// is not sufficient on its own — it would replace a plausible sentence with an honest one and
/// still leave the badge permanently blank on the tab an operator is looking at. Both landed:
/// [`BackendDigest::NotFetched`] is the belt, this relocation is the fix.
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

/// The accepted-write banner, rendered when the node answered `restart_required: true`. ⚠ NOT
/// every write: the daemon decides per key (`crates/vike-tradehub/src/hot_reload.rs`'s
/// `classify`), and the two `preferences` log-level keys apply hot.
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
        /// The reply's restart-to-apply signal, decided SERVER-side per key — not always `true`.
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

// ---------------------------------------------------------------------------------------------
// What the panel may SAY — the derivations, each one a fold over rows that are already on screen
// ---------------------------------------------------------------------------------------------

/// The ORIGIN cell's stable machine word, re-derived from the human label the wire carries.
///
/// ⚠ **This is a SECOND parser of a value the producer already computed, and it exists because
/// the wire drops the first.** `vike_config::provenance::Origin::kind()` returns exactly these
/// three words and is the pinned `--json` domain, but `SettingsShowSource::response` projects
/// only `Origin::label()` into [`WireSettingsRow::origin`]. So the panel re-splits the label:
/// `env:VAR` ⇒ `"env"`, the exact word `default` ⇒ `"default"`, anything else is a file name.
/// Widening `WireSettingsRow` with `origin_kind` would delete this function; until then it is the
/// one place the re-derivation happens, so it cannot drift between the badge, the filter and the
/// editor's warning.
#[must_use]
pub fn origin_kind(origin: &str) -> &'static str {
    if origin.starts_with("env:") {
        "env"
    } else if origin == "default" {
        "default"
    } else {
        "file"
    }
}

/// The ENVIRONMENT VARIABLE shadowing this row, if one does — the name after `env:`.
///
/// ⚠ **This is the fact the whole write half turns on.** `vike_config`'s precedence is
/// `code defaults -> <project>/settings/*.toml -> env -> CLI`, so for a `config`/`preferences`/
/// `flags` key whose origin is `env:VAR`, WRITING THE FILE CHANGES NOTHING until `VAR` is unset
/// on the daemon's box and it restarts — the refetch will still show `env:VAR` and the old value,
/// which looks exactly like the write having failed. Nothing on the wire, in the server or in
/// this panel said so before; [`settings_edit_panel`] now does.
///
/// ⚠ A `policy.*` row can NEVER be shadowed and this correctly returns `None` for one: `Policy`
/// implements neither `EnvOverride` nor `CliOverride` (both sealed in
/// `crates/vike-config/src/layers.rs`), every policy row is built with an empty `env` slice, and
/// `vike_config::precedence_line()` renders `(policy: FILE ONLY)`. That is the good half of the
/// asymmetry: a confirmed policy edit always takes effect at the next boot.
#[must_use]
pub fn env_shadow(row: &WireSettingsRow) -> Option<&str> {
    row.origin.strip_prefix("env:")
}

/// Is this row SET — i.e. does some layer name it, rather than it standing at its compiled-in
/// default? Measured exactly the way `vike_config::provenance::describe` measures it: by key
/// PRESENCE in a parsed layer. ⚠ A file line whose value equals the code default still reports
/// its file, and this must not "improve" on that by diffing against a default — the wire does not
/// carry the default (`FileRow::default` is one of the seven fields it drops), so the panel could
/// not do it even if it should.
#[must_use]
pub fn row_is_set(row: &WireSettingsRow) -> bool {
    origin_kind(&row.origin) != "default"
}

/// Is this row read by NOTHING? The CLI's own READ cell, verbatim: `"NO"` is the one value that
/// means nothing in the workspace reads this key.
///
/// ⚠ **`read_by` is a fact about the WORKSPACE, not about the node being looked at.** It answers
/// "which binary reads this key", not "does THIS daemon read it" — `config.node_addr` and
/// `config.backtest_addr` are, by `crates/vike-tradehub/src/hot_reload.rs`'s own comments, keys
/// this daemon never reads, and they still arrive with a non-`NO` cell. The panel therefore
/// renders the column under the CLI's own heading and says what it means, rather than rewording
/// it into "in force here".
#[must_use]
pub fn row_read_by_nothing(row: &WireSettingsRow) -> bool {
    row.read_by == "NO"
}

/// The amber finding: a row that is SET and read by NOTHING — a configured key that is a
/// misconfiguration rather than a setting in force. The CLI prints a paragraph per such key under
/// its table; this panel has room for a marker and a count.
///
/// ⚠ **The CLI's paragraph carries a VERDICT this panel cannot have.**
/// `vike_config::Reader::verdict()` distinguishes "the environment variable still works, export
/// it instead" from "neither spelling does anything" — and `unread_verdict`/`why_unread` are two
/// more of the seven `FileRow` fields [`WireSettingsRow`] drops. So the panel names the finding
/// and points at `vike-cli config show` on the node's own box for the reason, rather than
/// inventing one. Reproducing "READ: NO" with no verdict beside it is the exact state that
/// correction removed.
#[must_use]
pub fn row_is_finding(row: &WireSettingsRow) -> bool {
    row_is_set(row) && row_read_by_nothing(row)
}

/// Where the CLI's reason for a `READ: NO` row actually lives — the panel points here instead of
/// guessing, because the wire drops `unread_verdict` and `why_unread`.
pub const UNREAD_VERDICT_POINTER: &str = "`vike-cli config show` on the node's own box prints the reason a key is read by nothing — the \
     wire row does not carry it";

/// ⚠ **THE SANDBOX RESIDUAL, stated in the UI because the button cannot be honest without it.**
///
/// Both shipped units (`deploy/vike-tradehub.service`,
/// `deploy/vike-tradehub-project.service`) run `ProtectSystem=strict` with a single
/// `ReadWritePaths=` naming `settings/state` and nothing above it, so on a shipped deployment the
/// daemon's own mount namespace has `settings/` READ-ONLY: `vike_config::set_setting_within`'s
/// lock file cannot be opened and the write refuses with `EROFS` before a byte is edited.
/// MEASURED on the CI box inside the live daemon's namespace and declared at three sites in the
/// daemon (`SETTINGS_LOCK_BUDGET`, `apply_set_setting`, `SettingsWriteError::Lock`), with
/// `apply_set_setting`'s own doc heading it *"⚠ This arm is UNREACHABLE on the shipped
/// deployment, and that is a ruling, not a bug"*.
///
/// The grant is NOT to be widened to make the button work —
/// `docs/decisions/0054-settings-move-into-one-database.md` rules that it will be given with a
/// hard ceiling folded above it, and that has not landed. Until then the honest UI is a button
/// that says what will happen, which is what this line is for: it sits beside Save, always, so an
/// operator is never told a write landed by a panel that cannot know whether it did.
pub const SANDBOX_WRITE_NOTE: &str = "⚠ a DEPLOYED daemon may refuse this write: the shipped systemd units run ProtectSystem=strict \
     granting only settings/state, so settings/*.toml is read-only inside the daemon's own mount \
     namespace and the write refuses with EROFS having changed nothing. The refusal is surfaced \
     here verbatim. Do not widen the unit's grant to make it land.";

/// The file this write would edit, spelled out: `<settings dir>/<file>`. `None` when the node
/// reported no settings directory — which is the same condition the write half refuses with
/// *"this node resolved no settings directory at boot"*, so the panel can say so before the click
/// rather than after it.
#[must_use]
pub fn write_target(settings_dir: Option<&str>, file: &str) -> Option<String> {
    let dir = settings_dir?;
    let sep = if dir.contains('\\') && !dir.contains('/') { '\\' } else { '/' };
    Some(format!("{}{sep}{file}", dir.trim_end_matches(['/', '\\'])))
}

/// The settings table's row filter — the `set · N` / `all · N` segmented control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsFilter {
    /// Only rows some layer names — [`row_is_set`].
    #[default]
    Set,
    /// Every typed key the node answered with.
    All,
}

/// What the Backend segment's badge and the status line may say — and, for every state in which
/// the node has not answered, WHY there is no number instead of a plausible one.
///
/// ⚠ **Five states, not three.** [`BackendSettingsState`] has `Unsupported` and `Error` beside
/// the three obvious ones and both are reachable on day one (an older node; a wrong or absent
/// observe key; a node started with no settings source; a `policy.toml` that stopped loading
/// after an edit — reachable from this panel's OWN write if a hand edit lands between). A digest
/// that modelled three would render a permissions failure as "loading…" forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendDigest {
    /// No backend is connected — there is nothing to ask and no number to give.
    NoBackend,
    /// A backend is connected and **nothing has asked it yet** — [`BackendSettingsState::Idle`].
    ///
    /// ⚠ **This used to be folded into [`Self::Loading`] and that was a lie with a defect behind
    /// it.** The auto-fetch lived inside [`backend_settings_section`], which the two-tab redesign
    /// made conditional on the Backend tab being DRAWN, while the digest is rendered on BOTH tabs
    /// by design — so on the default (Credentials) tab the state stayed `Idle` for ever and the
    /// status line said `reading…` about a fetch nothing had requested. The fetch is now driven by
    /// the TOOL rather than by one tab (see [`should_fetch_settings`]'s doc), and this variant is
    /// the belt: `not read yet` and `reading` are different sentences, so a future re-hiding of
    /// the driver shows up as a digest that stops advancing rather than as a plausible one.
    NotFetched,
    /// A fetch is in flight — [`BackendSettingsState::Pending`].
    Loading,
    /// The node does not advertise `settings-show`.
    Unsupported,
    /// The fetch faulted; the text is the fault verbatim.
    Error(String),
    /// The node answered. Every field below is a fold over the rows it sent.
    Counts {
        /// Rows the node answered with — every typed settings key, unfiltered (`Request::
        /// SettingsShow` is a unit variant: no filter, no page, no cursor).
        keys: usize,
        /// Rows some layer names ([`row_is_set`]).
        set: usize,
        /// Rows that are SET and read by nothing ([`row_is_finding`]).
        findings: usize,
        /// The first finding's key, for the amber line — naming one is more use than a count
        /// alone, and the table's filter is how the rest are found.
        first_finding: Option<String>,
        /// The settings directory the node resolved at boot, as it rendered it. `None` ⇒ the node
        /// runs on compiled-in defaults (no project above its working directory).
        settings_dir: Option<String>,
    },
}

impl BackendDigest {
    /// Fold the render state (plus whether a backend is connected at all) into the digest.
    #[must_use]
    pub fn of(has_backend: bool, state: &BackendSettingsState) -> Self {
        if !has_backend {
            return BackendDigest::NoBackend;
        }
        match state {
            BackendSettingsState::Idle => BackendDigest::NotFetched,
            BackendSettingsState::Pending => BackendDigest::Loading,
            BackendSettingsState::Unsupported => BackendDigest::Unsupported,
            BackendSettingsState::Error(e) => BackendDigest::Error(e.clone()),
            BackendSettingsState::Loaded(show) => {
                let set = show.rows.iter().filter(|r| row_is_set(r)).count();
                let findings: Vec<&WireSettingsRow> =
                    show.rows.iter().filter(|r| row_is_finding(r)).collect();
                BackendDigest::Counts {
                    keys: show.rows.len(),
                    set,
                    findings: findings.len(),
                    first_finding: findings.first().map(|r| r.key.clone()),
                    settings_dir: show.settings_dir.clone(),
                }
            }
        }
    }

    /// The number beside the `Backend` segment's label — `None` in every state where the node has
    /// not answered, so the segment renders no number rather than a plausible one.
    #[must_use]
    pub fn badge_count(&self) -> Option<usize> {
        match self {
            BackendDigest::Counts { set, .. } => Some(*set),
            _ => None,
        }
    }

    /// Whether the segment carries the amber dot: at least one SET row read by nothing.
    #[must_use]
    pub fn has_finding(&self) -> bool {
        matches!(self, BackendDigest::Counts { findings, .. } if *findings > 0)
    }

    /// The one-line digest the status bar renders when the Backend tab is the INACTIVE one.
    ///
    /// ⚠ Every branch that is not `Counts` says what is not known and why — never a zero, never
    /// an ellipsis that outlives the fault.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            BackendDigest::NoBackend => {
                "Backend settings  no backend connected — nothing to read".to_string()
            }
            BackendDigest::NotFetched => "Backend settings  not read yet".to_string(),
            BackendDigest::Loading => "Backend settings  reading…".to_string(),
            BackendDigest::Unsupported => {
                format!("Backend settings  {PREDATES_SETTINGS_SHOW}")
            }
            BackendDigest::Error(e) => format!("Backend settings  unavailable — {e}"),
            BackendDigest::Counts { keys, set, findings, first_finding, .. } => {
                let mut s = format!("Backend settings  {keys} keys · {set} set");
                if *findings > 0 {
                    match first_finding {
                        Some(k) if *findings == 1 => {
                            s.push_str(&format!(" · ⚠ 1 read by nothing: {k}"));
                        }
                        Some(k) => {
                            s.push_str(&format!(" · ⚠ {findings} read by nothing, incl. {k}"));
                        }
                        None => s.push_str(&format!(" · ⚠ {findings} read by nothing")),
                    }
                }
                s
            }
        }
    }
}

/// The section body — the Backends picker's sibling in the Connections tool. Renders the ACTIVE
/// backend's state; `refresh` is the fetch out-slot the binary drains after the frame (set ⇒
/// start a fetch). ⚠ **The AUTO-fetch is no longer set here** — it moved up to
/// [`super::connections::connections_tool_content`], because this body is drawn on only one of two
/// tabs while the digest it feeds is drawn on both; [`should_fetch_settings`]'s own doc carries the
/// regression that made the move necessary. What this body still sets is the EXPLICIT Refresh
/// click, whatever the state, which belongs to the button that was clicked.
///
/// The WRITE half: `edit` is the per-row edit flow (UI-thread-owned — its text buffers are
/// live), `write` the out-slot an accepted Save fills ([`take_save`]); the binary drains it
/// after the frame, runs the wire write, and folds the outcome back with
/// [`settings_write_state`]. Every decision is a pure function above — this body only renders.
#[allow(clippy::too_many_arguments)] // the tool seam's shape: read-only inputs + one OUT slot each
pub fn backend_settings_section(
    ui: &mut egui::Ui,
    active_backend: Option<&str>,
    control_armed: bool,
    state: &BackendSettingsState,
    refresh: &mut bool,
    edit: &mut SettingsEditState,
    write: &mut Option<SettingsWriteRequest>,
    filter: &mut SettingsFilter,
) {
    let Some(name) = active_backend else {
        ui.label(
            egui::RichText::new(
                "no backend connected — the strip at the foot of this window is where one is \
                 connected, and its settings can only be read over that connection",
            )
            .monospace()
            .size(11.0)
            .weak(),
        );
        return;
    };
    // ------------------------------------------------------------------------------------------
    // The dense summary readout — ONE wrapped paragraph, not a stack of lines.
    // ------------------------------------------------------------------------------------------
    let digest = BackendDigest::of(true, state);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(egui::RichText::new(name).monospace().strong().size(11.0));
        match &digest {
            BackendDigest::Counts { keys, set, findings, first_finding, settings_dir } => {
                let defaulted = keys.saturating_sub(*set);
                ui.label(
                    egui::RichText::new(format!(
                        "· {keys} keys · {set} set from env or file · {defaulted} at their \
                         compiled-in default ·"
                    ))
                    .monospace()
                    .size(11.0)
                    .weak(),
                );
                if *findings > 0 {
                    let text = match first_finding {
                        Some(k) if *findings == 1 => {
                            format!("⚠ set and read by nothing: {k} ·")
                        }
                        Some(k) => {
                            format!("⚠ {findings} set and read by nothing, incl. {k} ·")
                        }
                        None => format!("⚠ {findings} set and read by nothing ·"),
                    };
                    ui.label(
                        egui::RichText::new(text)
                            .monospace()
                            .size(11.0)
                            .color(ui.visuals().warn_fg_color),
                    );
                }
                ui.label(
                    egui::RichText::new(
                        "edits are restart-to-apply unless the node says otherwise ·",
                    )
                    .monospace()
                    .size(11.0)
                    .weak(),
                );
                match settings_dir {
                    Some(dir) => {
                        ui.label(egui::RichText::new(dir.as_str()).monospace().size(11.0).weak());
                    }
                    None => {
                        ui.label(
                            egui::RichText::new(
                                "no settings directory — this node resolved none at boot, runs on \
                                 compiled-in defaults, and has no file any write could reach",
                            )
                            .monospace()
                            .size(11.0)
                            .color(ui.visuals().warn_fg_color),
                        );
                    }
                }
            }
            other => {
                let color = match other {
                    BackendDigest::Error(_) => ui.visuals().warn_fg_color,
                    _ => ui.visuals().weak_text_color(),
                };
                ui.label(
                    egui::RichText::new(other.line().replace("Backend settings  ", "· "))
                        .monospace()
                        .size(11.0)
                        .color(color),
                );
            }
        }
    });

    // ------------------------------------------------------------------------------------------
    // The filter + Refresh row.
    //
    // ⚠ The filter renders ONLY against rows the node actually sent. Before the answer arrives
    // there is no row to count, and a `set · 0 / all · 0` control would be two invented numbers
    // sitting where two measured ones go — the same fabrication the Backend badge refuses.
    // ------------------------------------------------------------------------------------------
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if let BackendSettingsState::Loaded(show) = state {
            let n_set = show.rows.iter().filter(|r| row_is_set(r)).count();
            filter_segment(ui, filter, SettingsFilter::Set, &format!("set · {n_set}"));
            filter_segment(ui, filter, SettingsFilter::All, &format!("all · {}", show.rows.len()));
            ui.add_space(8.0);
        }
        if ui.small_button("Refresh").clicked() {
            *refresh = true;
        }
    });
    ui.add_space(4.0);

    match state {
        // ⚠ Two sentences, not one. `Idle` is *nothing has asked yet* and `Pending` is *a fetch is
        // in flight*, and the same collapse in the digest is what let the tab-conditional
        // auto-fetch hide for a whole design — see [`should_fetch_settings`]. In production this
        // arm is a single frame (`connections_tool_content` requests the fetch before drawing
        // either tab); if it persists, the driver is not running and this says so instead of
        // animating an ellipsis for ever.
        BackendSettingsState::Idle => {
            ui.label(
                egui::RichText::new("the node's settings have not been read yet")
                    .monospace()
                    .size(11.0)
                    .weak(),
            );
        }
        BackendSettingsState::Pending => {
            ui.label(
                egui::RichText::new("reading the node's settings…").monospace().size(11.0).weak(),
            );
        }
        BackendSettingsState::Unsupported => {
            ui.label(egui::RichText::new(PREDATES_SETTINGS_SHOW).monospace().size(11.0).weak());
        }
        BackendSettingsState::Error(e) => {
            // ⚠ WRAPPED, not one line. The daemon's own refusals are paragraphs — the
            // `SettingsWriteError::Lock` text alone is ~600 characters and leads with the
            // namespace question, because `ls -ld` from an ordinary shell answers it wrongly.
            // Rendering it unwrapped is rendering it unread.
            ui.label(
                egui::RichText::new(format!("settings unavailable — {e}"))
                    .monospace()
                    .size(11.0)
                    .color(ui.visuals().warn_fg_color),
            );
        }
        BackendSettingsState::Loaded(show) => {
            settings_table(ui, show, *filter, control_armed, edit, write);
        }
    }
}

/// One segment of the `set · N` / `all · N` control. The SELECTED one is a label, not a button —
/// the same tree-readable-selection idiom the venue rail and the account strip use.
fn filter_segment(
    ui: &mut egui::Ui,
    filter: &mut SettingsFilter,
    value: SettingsFilter,
    text: &str,
) {
    if *filter == value {
        ui.label(
            egui::RichText::new(text)
                .monospace()
                .size(10.0)
                .strong()
                .color(vike_ui_theme::palette::ACCENT),
        );
    } else if ui.small_button(egui::RichText::new(text).monospace().size(10.0)).clicked() {
        *filter = value;
    }
}

/// Column widths for the settings table. Fixed, because a STICKY header has to be drawn OUTSIDE
/// the scroll area and therefore cannot inherit an `egui::Grid`'s measured columns — the same
/// construction `crates/vike-data-manager/src/view.rs`'s `stored_catalog_grid` uses, and the only
/// sticky-header idiom in this tree.
const W_KEY: f32 = 230.0;
const W_VALUE: f32 = 150.0;
const W_ORIGIN: f32 = 120.0;
const W_READ: f32 = 64.0;
const W_EDIT: f32 = 44.0;
const HEADER_H: f32 = 18.0;
const ROW_H: f32 = 17.0;
/// Inter-column gap, applied in both the header row and the data rows so the sticky header stays
/// registered with the columns under it.
const COL_GAP: f32 = 6.0;
/// How far the columns may shrink before they stop shrinking. Below this the row overflows its
/// clip rect rather than becoming five unreadable slivers — the window's own scroll is then the
/// answer, not a narrower table.
const MIN_COL_SCALE: f32 = 0.55;

/// The columns' widths for THIS width. Fixed proportions, scaled down together so the sticky
/// header — which is drawn outside the scroll area and therefore cannot inherit a measured
/// column — stays registered with the rows at any window width. The design must work at ~400pt,
/// where the unscaled table is half again too wide.
#[derive(Debug, Clone, Copy)]
struct Cols {
    key: f32,
    value: f32,
    origin: f32,
    read: f32,
    edit: f32,
}

impl Cols {
    fn for_width(avail: f32) -> Self {
        let natural = W_KEY + W_VALUE + W_ORIGIN + W_READ + W_EDIT + 4.0 * COL_GAP;
        let k = if natural > 0.0 { (avail / natural).clamp(MIN_COL_SCALE, 1.0) } else { 1.0 };
        Cols {
            key: W_KEY * k,
            value: W_VALUE * k,
            origin: W_ORIGIN * k,
            read: W_READ * k,
            edit: W_EDIT * k,
        }
    }
}

/// One fixed-width cell.
fn cell(ui: &mut egui::Ui, w: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(w, ROW_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(w);
            add(ui);
        },
    );
}

/// A cell whose text ELIDES rather than wrapping or spilling — the key and value columns, which
/// are the two that genuinely overflow a narrow window. The full text stays reachable on hover,
/// so nothing is lost, only folded.
fn elided(ui: &mut egui::Ui, w: f32, text: &str, color: egui::Color32) {
    cell(ui, w, |ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(text).monospace().size(10.0).color(color))
                .truncate(),
        )
        .on_hover_text(text);
    });
}

/// The settings table: a STICKY header row drawn outside the scroll area, then the filtered rows,
/// with the inline editor expanding as a row beneath the one being edited.
fn settings_table(
    ui: &mut egui::Ui,
    show: &WireSettingsShow,
    filter: SettingsFilter,
    control_armed: bool,
    edit: &mut SettingsEditState,
    write: &mut Option<SettingsWriteRequest>,
) {
    let head = |ui: &mut egui::Ui, w: f32, text: &str| {
        cell(ui, w, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .monospace()
                    .strong()
                    .size(9.0)
                    .color(vike_ui_theme::palette::TEXT3),
            );
        });
    };
    // ⚠ ONE `Cols` for the header and the rows both. The header is drawn OUTSIDE the scroll area
    // (that is what makes it sticky), so it cannot inherit a measured column — two independent
    // width computations here would put the labels out of register with their columns at every
    // width but one.
    let cols = Cols::for_width(ui.available_width());
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), HEADER_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = COL_GAP;
            head(ui, cols.key, "KEY");
            head(ui, cols.value, "VALUE");
            head(ui, cols.origin, "ORIGIN");
            head(ui, cols.read, "READ");
            head(ui, cols.edit, "");
        },
    );
    ui.separator();

    let rows: Vec<&WireSettingsRow> =
        show.rows.iter().filter(|r| filter == SettingsFilter::All || row_is_set(r)).collect();
    if rows.is_empty() {
        ui.label(egui::RichText::new("no rows match this filter").monospace().size(11.0).weak());
        return;
    }

    let mut open: Option<SettingsEditState> = None;
    egui::ScrollArea::vertical().max_height(300.0).id_salt("backend-settings-rows").show(
        ui,
        |ui| {
            for row in rows {
                let finding = row_is_finding(row);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = COL_GAP;
                    elided(ui, cols.key, &row.key, vike_ui_theme::palette::TEXT2);
                    // `""` means UNSET on the wire; the CLI prints `-` and so does this.
                    let v = if row.value.is_empty() { "-" } else { row.value.as_str() };
                    elided(ui, cols.value, v, vike_ui_theme::palette::TEXT);
                    cell(ui, cols.origin, |ui| {
                        // ⚠ `env:VAR` and `<file>.toml` are DIFFERENT answers and both are on the
                        // wire; only `default` is painted down. Collapsing the first two into
                        // "set" is what would hide the env-outranks-file hazard below.
                        let color = match origin_kind(&row.origin) {
                            "env" => vike_ui_theme::palette::WARN,
                            "default" => vike_ui_theme::palette::TEXT3,
                            _ => vike_ui_theme::palette::TEXT2,
                        };
                        ui.label(
                            egui::RichText::new(&row.origin).monospace().size(10.0).color(color),
                        );
                    });
                    cell(ui, cols.read, |ui| {
                        // THREE values, and the panel neither reduces them to two nor rewords
                        // them: a binary's short name, `yes` (a library reads it — so every
                        // binary linking it does), or `NO`.
                        let color = if row_read_by_nothing(row) {
                            vike_ui_theme::palette::TEXT3
                        } else {
                            vike_ui_theme::palette::TEXT2
                        };
                        ui.label(
                            egui::RichText::new(&row.read_by).monospace().size(10.0).color(color),
                        )
                        .on_hover_text(READ_COLUMN_HINT);
                    });
                    cell(ui, cols.edit, |ui| {
                        if finding {
                            ui.label(
                                egui::RichText::new("⚠")
                                    .size(10.0)
                                    .color(ui.visuals().warn_fg_color),
                            )
                            .on_hover_text(format!(
                                "set, and read by nothing — {UNREAD_VERDICT_POINTER}"
                            ));
                        }
                        if ui.small_button(egui::RichText::new("edit").size(9.0)).clicked() {
                            open = Some(start_edit(row));
                        }
                    });
                });
                // The editor expands as a row BENEATH the one being edited.
                let editing_this =
                    matches!(edit, SettingsEditState::Editing { key, .. } if key == &row.key);
                let reporting_this = matches!(
                    edit,
                    SettingsEditState::Saving { key }
                        | SettingsEditState::Saved { key, .. }
                        | SettingsEditState::Failed { key, .. }
                    if key == &row.key
                );
                if editing_this || reporting_this {
                    settings_edit_panel(
                        ui,
                        edit,
                        write,
                        row,
                        show.settings_dir.as_deref(),
                        control_armed,
                    );
                }
            }
        },
    );
    if let Some(next) = open {
        *edit = next;
    }
}

/// What the READ column means, on hover — the CLI's own legend, because "which binary reads this
/// key" and "is this key in force on the node I am looking at" are different questions and only
/// the first one is answered.
const READ_COLUMN_HINT: &str = "which BINARY reads this key, workspace-wide: a binary's short name, `yes` (a library reads \
     it, so every binary linking it does), or `NO` (nothing reads it). It is NOT a claim about \
     this node — a tradehub row can name a key only the GUI reads.";

/// The edit flow's own panel, rendered between the header and the grid whenever a row is being
/// edited (or a write is in flight / just finished). Renders [`SettingsEditState`]; every
/// decision (may this save? what does the outcome mean?) is a pure function above.
fn settings_edit_panel(
    ui: &mut egui::Ui,
    edit: &mut SettingsEditState,
    write: &mut Option<SettingsWriteRequest>,
    row: &WireSettingsRow,
    settings_dir: Option<&str>,
    control_armed: bool,
) {
    // Clicks are staged and applied AFTER the match: inside an arm the state is mutably borrowed
    // for its live text buffers, so the transitions ([`take_save`], the reset to `Idle`) run once
    // that borrow ends — the same deferred-mutation shape the section's own out-slots use.
    let mut do_save = false;
    let mut dismiss = false;
    let shadow = env_shadow(row).map(str::to_string);
    let target = write_target(settings_dir, &row.section);
    egui::Frame::new()
        .fill(vike_ui_theme::palette::CARD)
        .stroke(egui::Stroke::new(1.0, vike_ui_theme::palette::BORDER))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| match edit {
            SettingsEditState::Idle => {}
            SettingsEditState::Editing { file, key, value, confirm } => {
                let policy = is_policy_file(file);
                // EFFECTIVE NOW, with its origin — the value this row actually resolves to today
                // and the layer that set it. Rendered before the control, because "what am I
                // changing from" is the first thing the operator needs and the table row it
                // expanded under scrolls.
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(
                        egui::RichText::new("Effective now")
                            .monospace()
                            .size(9.0)
                            .color(vike_ui_theme::palette::TEXT3),
                    );
                    ui.label(
                        egui::RichText::new(if row.value.is_empty() {
                            "-"
                        } else {
                            row.value.as_str()
                        })
                        .monospace()
                        .size(11.0)
                        .strong(),
                    );
                    ui.label(
                        egui::RichText::new(format!("from {}", row.origin))
                            .monospace()
                            .size(10.0)
                            .weak(),
                    );
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(egui::RichText::new("New value").monospace().size(10.0));
                    ui.add(egui::TextEdit::singleline(value).desired_width(220.0));
                });
                if policy {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            egui::RichText::new("policy ceiling — type the key name to confirm:")
                                .monospace()
                                .size(10.0),
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

                // ⚠⚠ THE WARNING THIS WHOLE PANEL EXISTS FOR. When the ENVIRONMENT sets this key,
                // writing the file changes nothing the running daemon will ever read: the write is
                // ACCEPTED, `restart_required: true` comes back, and the refetch shows the row
                // still reading `env:VAR` with the old value — which looks exactly like a failure.
                // Nothing on the wire, in `apply_set_setting`, or in this panel said so before.
                if let Some(var) = &shadow {
                    ui.add_space(3.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "⚠ the ENVIRONMENT sets this key, and env outranks the file. Writing \
                             {} changes nothing until {var} is unset on the daemon's box and it \
                             restarts — this row will still read env:{var} with the old value \
                             after the write. Unset the variable there first.",
                            row.section
                        ))
                        .monospace()
                        .size(10.0)
                        .color(ui.visuals().warn_fg_color),
                    );
                }

                // ⚠ A write is a CONTROL action and the observe key cannot sign it. This backend
                // record's own arming gate (`BackendRecord::control` — the B9 per-backend gate
                // `backend_registry::resolve_keys` enforces) is knowable BEFORE the click, so an
                // unarmed record says so here rather than answering with a transport refusal.
                // ⚠ It is not the only gate: `flags.tradehub_control` must also be true ON THE
                // NODE, or every `Request::Command` answers "control not enabled on this node" —
                // and this side cannot see that flag, so the sentence names it rather than
                // claiming an armed record is sufficient.
                if !control_armed {
                    ui.add_space(3.0);
                    ui.label(
                        egui::RichText::new(
                            "⚠ this backend's write channel is NOT armed — a settings write is a \
                             CONTROL action and the observe key cannot sign it. Arm the record \
                             (Edit it above: a named control key plus `control armed`). The node \
                             must ALSO have flags.tradehub_control = true, which this side cannot \
                             see from here.",
                        )
                        .monospace()
                        .size(10.0)
                        .color(ui.visuals().warn_fg_color),
                    );
                }

                // What will be written, and where — exactly, before the click.
                ui.add_space(3.0);
                match &target {
                    Some(path) => ui.label(
                        egui::RichText::new(format!(
                            "writes {} = {} to {path} on the backend · takes effect on restart",
                            key,
                            if value.is_empty() { "\"\"" } else { value.as_str() }
                        ))
                        .monospace()
                        .size(10.0)
                        .weak(),
                    ),
                    // The read half reported no settings directory, which is the same condition
                    // the write half refuses with "this node resolved no settings directory at
                    // boot". Say it before the click rather than after it.
                    None => ui.label(
                        egui::RichText::new(
                            "this node resolved NO settings directory at boot — there is no file \
                             to write and the daemon will refuse this write outright",
                        )
                        .monospace()
                        .size(10.0)
                        .color(ui.visuals().warn_fg_color),
                    ),
                };
                ui.label(
                    egui::RichText::new(SANDBOX_WRITE_NOTE)
                        .monospace()
                        .size(10.0)
                        .color(ui.visuals().warn_fg_color),
                );

                let allowed = can_save_fields(file, key, confirm);
                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    // ⚠ The LABEL is honest about the shadow: "Save to file anyway" is what the
                    // button does when the environment outranks the file, and a plain "Save" there
                    // would be the panel claiming an effect it cannot deliver.
                    let label =
                        if shadow.is_some() { "Save to file anyway" } else { "Save to file" };
                    if ui.add_enabled(allowed, egui::Button::new(label)).clicked() {
                        do_save = true;
                    }
                    if ui.small_button("Cancel").clicked() {
                        dismiss = true;
                    }
                });
            }
            SettingsEditState::Saving { key } => {
                ui.label(
                    egui::RichText::new(format!("saving {key}…")).monospace().size(10.0).weak(),
                );
            }
            SettingsEditState::Saved { key, restart_required } => {
                let note = if *restart_required {
                    format!("{key}: {SAVED_RESTART_NOTE}")
                } else {
                    format!("{key}: saved and applied — no restart needed")
                };
                ui.label(
                    egui::RichText::new(note)
                        .monospace()
                        .size(10.0)
                        .color(ui.visuals().strong_text_color()),
                );
                // ⚠ An ACCEPTED write to an env-shadowed key still changes nothing the running
                // process reads. The acceptance is about the FILE; say what it is not about.
                if let Some(var) = &shadow {
                    ui.label(
                        egui::RichText::new(format!(
                            "the file was written — the EFFECTIVE value is still {var}'s until \
                             that variable is unset on the daemon's box",
                        ))
                        .monospace()
                        .size(10.0)
                        .color(ui.visuals().warn_fg_color),
                    );
                }
                if ui.small_button("OK").clicked() {
                    dismiss = true;
                }
            }
            SettingsEditState::Failed { key, error } => {
                // ⚠ WRAPPED. `SettingsWriteError::Lock`'s Display — the EROFS case, the one a
                // deployed box actually hits — is ~600 characters and leads with the namespace
                // question. One unwrapped line is one unread line.
                ui.label(
                    egui::RichText::new(format!("{key}: write refused — {error}"))
                        .monospace()
                        .size(10.0)
                        .color(ui.visuals().warn_fg_color),
                );
                if ui.small_button("Dismiss").clicked() {
                    dismiss = true;
                }
            }
        });
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

    // ------------------------------------------------------------------------------------------
    // The derivations the redesigned panel renders from
    // ------------------------------------------------------------------------------------------

    /// ⚠ The three ORIGIN kinds, re-derived from the LABEL because the wire drops
    /// `Origin::kind()`. `env:VAR` and a file name are DIFFERENT answers and only `default` means
    /// unset — collapsing the first two is what would hide the env-outranks-file hazard.
    #[test]
    fn the_origin_kind_is_re_derived_from_the_label_the_wire_carries() {
        assert_eq!(origin_kind("default"), "default");
        assert_eq!(origin_kind("env:VIKE_RECONCILE"), "env");
        assert_eq!(origin_kind("config.toml"), "file");
        assert_eq!(origin_kind("policy.toml"), "file");
        // ⚠ Only the EXACT word is the default — a file that happened to be called
        // `defaults.toml` is a file.
        assert_eq!(origin_kind("defaults.toml"), "file");
    }

    /// ⚠ THE ENV-SHADOW TEST, and the asymmetry that makes it safe for policy. A `config` row set
    /// from the environment is shadowed; a `policy` row can never be, because `Policy` implements
    /// neither `EnvOverride` nor `CliOverride` (both sealed) and every policy row is built with an
    /// empty env slice.
    #[test]
    fn a_row_set_from_the_environment_is_shadowed_and_a_policy_row_never_is() {
        let shadowed = WireSettingsRow {
            section: "config.toml".into(),
            key: "config.tradehub_addr".into(),
            value: "0.0.0.0:7879".into(),
            origin: "env:VIKE_TRADEHUB_ADDR".into(),
            read_by: "tradehub".into(),
        };
        assert_eq!(env_shadow(&shadowed), Some("VIKE_TRADEHUB_ADDR"));

        assert_eq!(env_shadow(&config_row()), None, "a file origin shadows nothing");
        assert_eq!(env_shadow(&policy_row()), None, "policy has no env layer at all");
        let defaulted = WireSettingsRow { origin: "default".into(), ..config_row() };
        assert_eq!(env_shadow(&defaulted), None);
    }

    /// SET is PRESENCE in a layer, exactly as `vike_config::provenance::describe` measures it —
    /// never a diff against the compiled-in default (which the wire does not even carry).
    #[test]
    fn set_is_presence_in_a_layer_and_the_finding_needs_both_halves() {
        let file = config_row();
        assert!(row_is_set(&file));
        assert!(!row_read_by_nothing(&file));
        assert!(!row_is_finding(&file), "set, but something reads it");

        let unset_unread =
            WireSettingsRow { origin: "default".into(), read_by: "NO".into(), ..config_row() };
        assert!(!row_is_set(&unset_unread));
        assert!(row_read_by_nothing(&unset_unread));
        assert!(
            !row_is_finding(&unset_unread),
            "a key NOBODY configured and nothing reads is not a misconfiguration"
        );

        let finding = WireSettingsRow { read_by: "NO".into(), ..config_row() };
        assert!(row_is_finding(&finding), "set AND read by nothing is the amber row");

        // The third READ value — a library read — is not `NO`.
        let library = WireSettingsRow { read_by: "yes".into(), ..config_row() };
        assert!(!row_read_by_nothing(&library));
    }

    /// The editor says exactly what it will write and WHERE, before the click — and when the node
    /// reported no settings directory it says there is no file, which is the same condition the
    /// daemon's write half refuses with.
    #[test]
    fn the_write_target_is_the_nodes_own_directory_joined_to_the_rows_file() {
        assert_eq!(
            write_target(Some("/srv/vike-<unit>/settings"), "policy.toml").as_deref(),
            Some("/srv/vike-<unit>/settings/policy.toml")
        );
        // A trailing separator does not double up.
        assert_eq!(
            write_target(Some("/srv/vike-<unit>/settings/"), "config.toml").as_deref(),
            Some("/srv/vike-<unit>/settings/config.toml")
        );
        // A Windows-shaped directory keeps its own separator — the node renders the path, this
        // side only joins it, so it must not impose a POSIX spelling on a path it did not make.
        assert_eq!(
            write_target(Some("C:\\vike\\settings"), "flags.toml").as_deref(),
            Some("C:\\vike\\settings\\flags.toml")
        );
        assert_eq!(write_target(None, "policy.toml"), None, "no directory ⇒ no file to name");
    }

    /// ⚠ The SANDBOX note names the mechanism and refuses the fix, because a note that only said
    /// "this may fail" would invite widening the unit's grant — which is the one repair
    /// `apply_set_setting`'s own doc forbids.
    #[test]
    fn the_sandbox_note_names_the_mechanism_and_refuses_the_widening() {
        assert!(SANDBOX_WRITE_NOTE.contains("ProtectSystem=strict"), "{SANDBOX_WRITE_NOTE}");
        assert!(SANDBOX_WRITE_NOTE.contains("settings/state"), "{SANDBOX_WRITE_NOTE}");
        assert!(SANDBOX_WRITE_NOTE.contains("EROFS"), "{SANDBOX_WRITE_NOTE}");
        assert!(
            SANDBOX_WRITE_NOTE.contains("Do not widen"),
            "the note must refuse the repair, not merely warn: {SANDBOX_WRITE_NOTE}"
        );
    }

    /// The digest's five states, and the one that carries numbers is the only one that does.
    #[test]
    fn the_digest_models_every_fetch_state_and_invents_no_count() {
        assert_eq!(
            BackendDigest::of(false, &BackendSettingsState::Loaded(show())),
            BackendDigest::NoBackend
        );
        assert!(BackendDigest::NoBackend.line().contains("no backend connected"));
        assert!(BackendDigest::of(true, &BackendSettingsState::Pending).line().contains("reading"));
        assert!(
            BackendDigest::of(true, &BackendSettingsState::Unsupported)
                .line()
                .contains(PREDATES_SETTINGS_SHOW)
        );
        let err = BackendDigest::of(true, &BackendSettingsState::Error("bad mac".into()));
        assert!(err.line().contains("bad mac"), "{}", err.line());
        assert_eq!(err.badge_count(), None);

        let loaded = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));
        assert_eq!(loaded.badge_count(), Some(1), "the one row, set from config.toml");
        assert!(!loaded.has_finding());
        assert!(loaded.line().contains("1 keys · 1 set"), "{}", loaded.line());
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
