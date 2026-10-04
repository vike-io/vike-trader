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
//! old → new record the daemon journals after. The arming screen deleted its own ceremony for the
//! same reason (`crates/vike-app-core/src/ui/tool_views/venues.rs`'s `apply_arming`). So every row
//! saves on one click, and this side sends no `confirm`.
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

use vike_tradehub_client::wire::{WireSettingsRow, WireSettingsShow};
use vike_ui_theme::icons;

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

// ⚠ THE TYPED CONFIRM stood here, and is DELETED rather than disabled
// (`docs/decisions/0086-settings-live-only-in-the-database.md` point 7). It was `can_save` /
// `can_save_fields` — the Save gate that held a policy row's button disabled until its exact dotted
// key was retyped — over `demands_typed_confirm`, a policy-plane predicate on
// `vike_config::is_policy_plane_key` (itself the repair of an earlier `is_policy_file`, an exact
// match of the row's `section` against `const POLICY_FILE`), plus a `confirm` buffer on
// `SettingsEditState::Editing` and on `SettingsWriteRequest`. The daemon's half went first —
// `apply_set_setting` never reads the wire's `confirm` — so this half was a ceremony nothing
// downstream enforced. The names stay in this comment because older records cite them.

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

// ---------------------------------------------------------------------------------------------
// What the panel may SAY — the derivations, each one a fold over rows that are already on screen
// ---------------------------------------------------------------------------------------------

/// The ORIGIN cell's stable machine word, re-derived from the human label the wire carries.
///
/// ⚠ **This is a SECOND parser of a value the producer already computed, and it exists because
/// the wire drops the first.** `vike_config::provenance::Origin::kind()` returns exactly this set
/// of words and is the pinned `--json` domain, but `SettingsShowSource::response` projects
/// only `Origin::label()` into [`WireSettingsRow::origin`]. So the panel re-splits the label:
/// `env:VAR` ⇒ `"env"`, the exact word `default` ⇒ `"default"`, the exact word `db` ⇒ `"db"`,
/// anything else is a file name.
/// Widening `WireSettingsRow` with `origin_kind` would delete this function; until then it is the
/// one place the re-derivation happens, so it cannot drift between the badge, the filter and the
/// editor's warning.
///
/// ⚠ **`db` had to be added here the moment `Origin` grew it**, and the reason is the cost of NOT
/// adding it rather than tidiness: the catch-all below is *anything else is a FILE*, so a row the
/// settings database set would have been badged, filtered and explained to an operator as coming
/// from a file they could open and find innocent. That is this panel's own failure class —
/// positive confirmation of something false — and it is the standing hazard of re-deriving a word
/// the producer already computed. **A word added to `Origin::kind` is an arm added here.** The
/// permanent cure is `WireSettingsRow` carrying `origin_kind`, which deletes this function.
#[must_use]
pub fn origin_kind(origin: &str) -> &'static str {
    if origin.starts_with("env:") {
        "env"
    } else if origin == "default" {
        "default"
    } else if origin == "db" {
        // `vike_config::Origin::Db`'s own `label()`. During decision 0057's Phase 1 the FILES still
        // win, so a row reading `db` means the store and the files have diverged — which is
        // precisely the thing an operator must not see rendered as `file`.
        "db"
    } else {
        "file"
    }
}

/// The ENVIRONMENT VARIABLE shadowing this row, if one does — the name after `env:`.
///
/// ⚠ **This is the fact the whole write half turns on.** `vike_config`'s precedence is
/// `env > the settings database > default`, so for a `config`/`preferences`/`flags` key whose
/// origin is `env:VAR`, WRITING THE ROW CHANGES NOTHING until `VAR` is unset on the daemon's box
/// and it restarts — the refetch will still show `env:VAR` and the old value, which looks exactly
/// like the write having failed. Nothing on the wire, in the server or in this panel said so
/// before; [`settings_edit_panel`] now does.
///
/// ⚠ A `policy.*` row can NEVER be shadowed and this correctly returns `None` for one: `Policy`
/// implements neither `EnvOverride` nor `CliOverride` (both sealed in
/// `crates/vike-config/src/layers.rs`), every policy row is built with an empty `env` slice, and
/// `vike_config::precedence_line()` renders `(policy: STORE ONLY)`. That is the good half of the
/// asymmetry: a policy edit always takes effect at the next boot.
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

/// The settings DATABASE a write lands one row of, spelled out: `<settings dir>/db/vike.db`,
/// joined from `vike_secrets::DB_DIR` and `vike_secrets::DB_FILE` (the one spelling of the store's
/// place under a settings directory). `None` when the node reported no settings directory — which
/// is the same condition the write half refuses with *"this node resolved no settings directory at
/// boot"*, so the panel can say so before the click rather than after it.
///
/// ⚠ It joined the row's FILE name (`<settings dir>/config.toml`) until 0086 was applied to this
/// screen. Nothing reads or writes a settings file, so that target told the operator the save would
/// land somewhere it never does.
#[must_use]
pub fn write_target(settings_dir: Option<&str>) -> Option<String> {
    let dir = settings_dir?;
    let sep = if dir.contains('\\') && !dir.contains('/') { '\\' } else { '/' };
    Some(format!(
        "{}{sep}{}{sep}{}",
        dir.trim_end_matches(['/', '\\']),
        vike_secrets::DB_DIR,
        vike_secrets::DB_FILE
    ))
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
/// observe key; a node started with no settings source; a `policy` row that stopped loading
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
    ///
    /// The line is TEXT and carries no warning glyph: an icon cannot live inside a string (it
    /// would draw in a text family — `vike_ui_theme::icons`' module doc). A renderer leads the
    /// line with `icons::WARNING` when [`Self::has_finding`] is true.
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
                            s.push_str(&format!(" · 1 read by nothing: {k}"));
                        }
                        Some(k) => {
                            s.push_str(&format!(" · {findings} read by nothing, incl. {k}"));
                        }
                        None => s.push_str(&format!(" · {findings} read by nothing")),
                    }
                }
                s
            }
        }
    }
}

/// The section body — the Backends picker's sibling in the Connections tool. Renders the ACTIVE
/// backend's state under a `▸ BACKEND SETTINGS` disclosure header that carries the filter segment
/// and Refresh on its own row ([`SECTION_TITLE`]); `refresh` is the fetch out-slot the binary
/// drains after the frame (set ⇒ start a fetch). ⚠ **The AUTO-fetch is no longer set here** — it
/// moved up to
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
    // ⚠ THE SECTION HEADER — the design's `▸ BACKEND SETTINGS`, with the filter segment and
    // Refresh seated on the SAME row.
    //
    // It was ABSENT: this body went straight from the summary paragraph into a bare
    // `ui.horizontal` holding the two filter segments and Refresh, and nothing on screen said
    // where one section ended and the next began. On the Backend tab that matters more than it
    // sounds — the registry rows, their editor and this table are three stacked blocks, and the
    // only thing separating the last two was a `ui.separator()`.
    //
    // A REAL `CollapsingState`, not a triangle that does nothing: a disclosure glyph that does not
    // disclose is a control lying about being one, which is this window's whole theme. It is
    // id-keyed like every other `CollapsingState`, so the open/closed bit is discarded once on an
    // upgrade that changes the id — the module doc of `super::connections` records that trade;
    // `default_open = true` means the discard restores exactly what ships.
    // ------------------------------------------------------------------------------------------
    let section_id = ui.make_persistent_id(SECTION_ID_SALT);
    let collapsing = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        section_id,
        true,
    );
    let mut toggle = false;
    let header = collapsing.show_header(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        // ⚠ The TITLE toggles too, not only the triangle egui draws to its left. Two reasons, and
        // the second is the one that made it a rule here: a section title that looks like a header
        // and is inert while the 12pt glyph beside it is not is a smaller version of this window's
        // whole defect; and egui's own disclosure control is an `Ui::interact` over a painted
        // triangle with no `WidgetInfo`, so it reaches the accessibility tree as an unlabelled node
        // that a headless test cannot name — a control nothing can gate is a control that will rot.
        // `crates/vike-app-core/tests/connections_tabs.rs`'s
        // `the_backend_settings_section_has_a_titled_collapsible_header` clicks THIS.
        if ui
            .add(
                egui::Button::new(
                    egui::RichText::new(SECTION_TITLE)
                        .monospace()
                        .strong()
                        .size(11.0)
                        .color(vike_ui_theme::palette::TEXT2),
                )
                .frame(false),
            )
            .on_hover_text("show or hide the node's effective settings")
            .clicked()
        {
            toggle = true;
        }
        // ⚠ The filter renders ONLY against rows the node actually sent. Before the answer
        // arrives there is no row to count, and a `set · 0 / all · 0` control would be two
        // invented numbers sitting where two measured ones go — the same fabrication the
        // Backend badge refuses.
        if let BackendSettingsState::Loaded(show) = state {
            ui.add_space(6.0);
            let n_set = show.rows.iter().filter(|r| row_is_set(r)).count();
            filter_segment(ui, filter, SettingsFilter::Set, &format!("set · {n_set}"));
            filter_segment(ui, filter, SettingsFilter::All, &format!("all · {}", show.rows.len()));
        }
        ui.add_space(6.0);
        if ui.small_button("Refresh").clicked() {
            *refresh = true;
        }
    });
    let mut header = header;
    if toggle {
        header.toggle();
    }
    let shown = *filter;
    header.body_unindented(|ui| {
        settings_section_body(ui, name, control_armed, state, edit, write, shown);
    });
}

/// The section header's title, and the id salt its open/closed bit is keyed on. Named because the
/// header is drawn in one place and asserted in another, and a title spelled twice is a title that
/// can disagree with itself.
pub const SECTION_TITLE: &str = "BACKEND SETTINGS";
const SECTION_ID_SALT: &str = "vike_backend_settings_section";

/// Everything UNDER the section header: the dense summary paragraph, then whichever of the fetch
/// states the node is in. Split out of [`backend_settings_section`] only so the collapsing body
/// closure stays readable — no behaviour of its own.
fn settings_section_body(
    ui: &mut egui::Ui,
    name: &str,
    control_armed: bool,
    state: &BackendSettingsState,
    edit: &mut SettingsEditState,
    write: &mut Option<SettingsWriteRequest>,
    filter: SettingsFilter,
) {
    // ------------------------------------------------------------------------------------------
    // The dense summary readout — ONE wrapped paragraph, not a stack of lines.
    // ------------------------------------------------------------------------------------------
    let digest = BackendDigest::of(true, state);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(egui::RichText::new("effective settings of").monospace().size(11.0).weak());
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
                        Some(k) if *findings == 1 => format!("set and read by nothing: {k} ·"),
                        Some(k) => format!("{findings} set and read by nothing, incl. {k} ·"),
                        None => format!("{findings} set and read by nothing ·"),
                    };
                    ui.label(
                        icons::WARNING.before(
                            ui.style(),
                            egui::RichText::new(text)
                                .monospace()
                                .size(11.0)
                                .color(ui.visuals().warn_fg_color),
                        ),
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
                        ui.label(egui::RichText::new("settings dir").monospace().size(11.0).weak());
                        ui.label(egui::RichText::new(dir.as_str()).monospace().size(11.0).weak());
                    }
                    None => {
                        ui.label(
                            egui::RichText::new(
                                "no settings directory — this node resolved none at boot, runs on \
                                 compiled-in defaults, and has no settings database any write \
                                 could reach",
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
                // Never a finding here: only `Counts` has one, and it is the arm above. The line
                // is drawn as text, so a finding would need `icons::WARNING` before it.
                ui.label(
                    egui::RichText::new(other.line().replace("Backend settings  ", "· "))
                        .monospace()
                        .size(11.0)
                        .color(color),
                );
            }
        }
    });

    ui.add_space(4.0);

    // ⚠ The filter + Refresh row that used to sit HERE is now on the section header's own row —
    // see [`backend_settings_section`]. It moved rather than being duplicated: there is exactly
    // one `filter_segment` pair and one Refresh button in this file.
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
            settings_table(ui, show, filter, control_armed, edit, write);
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
                        // "set" is what would hide the env-outranks-the-row hazard below.
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
                            let mark =
                                icons::WARNING.rich().size(10.0).color(ui.visuals().warn_fg_color);
                            let words =
                                format!("set, and read by nothing — {UNREAD_VERDICT_POINTER}");
                            icons::named(ui.label(mark), &words);
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
                    let panel = settings_edit_panel(
                        ui,
                        edit,
                        write,
                        row,
                        show.settings_dir.as_deref(),
                        control_armed,
                    );
                    // ⚠ The panel opens INSIDE this 300pt scroll area, so beneath a row near its
                    // bottom it opened past the lower edge: a panel cut off mid-line and no Save
                    // (measured on the live Backend tab, 2026-09-28). Bring it into view ONCE per
                    // opening — doing it every frame would pin the table and take the scroll
                    // away from the operator for as long as the editor stays open.
                    let shown = ui.ctx().data(|d| d.get_temp::<String>(editor_shown_id()));
                    if shown.as_deref() != Some(row.key.as_str()) {
                        ui.scroll_to_rect(panel.rect, None);
                        ui.ctx().data_mut(|d| d.insert_temp(editor_shown_id(), row.key.clone()));
                    }
                }
            }
        },
    );
    if matches!(edit, SettingsEditState::Idle) {
        ui.ctx().data_mut(|d| d.remove::<String>(editor_shown_id()));
    }
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

/// The edit flow's own panel, rendered beneath the row it edits, inside the table's scroll area,
/// whenever that row is being edited (or a write is in flight / just finished). Renders
/// [`SettingsEditState`]; every decision (may this save? what does the outcome mean?) is a pure
/// function above. Returns the panel's response so the section can scroll it into view.
fn settings_edit_panel(
    ui: &mut egui::Ui,
    edit: &mut SettingsEditState,
    write: &mut Option<SettingsWriteRequest>,
    row: &WireSettingsRow,
    settings_dir: Option<&str>,
    control_armed: bool,
) -> egui::Response {
    // Clicks are staged and applied AFTER the match: inside an arm the state is mutably borrowed
    // for its live text buffers, so the transitions ([`take_save`], the reset to `Idle`) run once
    // that borrow ends — the same deferred-mutation shape the section's own out-slots use.
    let mut do_save = false;
    let mut dismiss = false;
    let shadow = env_shadow(row).map(str::to_string);
    let target = write_target(settings_dir);
    let panel = egui::Frame::new()
        .fill(vike_ui_theme::palette::CARD)
        .stroke(egui::Stroke::new(1.0, vike_ui_theme::palette::BORDER))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| match edit {
            SettingsEditState::Idle => {}
            SettingsEditState::Editing { file: _, key, value } => {
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
                // ⚠ No confirm line, for any key — the policy ceilings included (0086 point 7; the
                // tombstone above `SettingsWriteRequest` names what stood here).

                // ⚠⚠ THE WARNING THIS WHOLE PANEL EXISTS FOR. When the ENVIRONMENT sets this key,
                // writing the row changes nothing the running daemon will ever read: the write is
                // ACCEPTED, `restart_required: true` comes back, and the refetch shows the row
                // still reading `env:VAR` with the old value — which looks exactly like a failure.
                // Nothing on the wire, in `apply_set_setting`, or in this panel said so before.
                if let Some(var) = &shadow {
                    ui.add_space(3.0);
                    let words = egui::RichText::new(format!(
                        "the ENVIRONMENT sets this key, and env outranks the settings \
                             database. Saving this row changes nothing until {var} is unset on \
                             the daemon's box and it restarts — this row will still read \
                             env:{var} with the old value after the write. Unset the variable \
                             there first."
                    ))
                    .monospace()
                    .size(10.0)
                    .color(ui.visuals().warn_fg_color);
                    ui.label(icons::WARNING.before(ui.style(), words));
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
                    let words = egui::RichText::new(
                        "this backend's write channel is NOT armed — a settings write is a \
                             CONTROL action and the observe key cannot sign it. Arm the record \
                             (Edit it above: a named control key plus `control armed`). The node \
                             must ALSO have flags.tradehub_control = true, which this side cannot \
                             see from here.",
                    )
                    .monospace()
                    .size(10.0)
                    .color(ui.visuals().warn_fg_color);
                    ui.label(icons::WARNING.before(ui.style(), words));
                }

                // What will be written, and where — exactly, before the click.
                ui.add_space(3.0);
                match &target {
                    Some(path) => ui.label(
                        egui::RichText::new(format!(
                            "writes {} = {} as one row of {path} on the backend · takes effect \
                             on restart",
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
                            "this node resolved NO settings directory at boot — there is no \
                             settings database to write and the daemon will refuse this write \
                             outright",
                        )
                        .monospace()
                        .size(10.0)
                        .color(ui.visuals().warn_fg_color),
                    ),
                };

                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    // ⚠ The LABEL is honest about the shadow: "Save anyway" is what the button
                    // does when the environment outranks the row, and a plain "Save" there would
                    // be the panel claiming an effect it cannot deliver. It read "Save to file" /
                    // "Save to file anyway" until 0086 was applied here: the hazard was right, the
                    // FILE never was. Always ENABLED — no key demands a typed confirm.
                    let label = if shadow.is_some() { "Save anyway" } else { "Save" };
                    if ui.button(label).clicked() {
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
                // process reads. The acceptance is about the ROW; say what it is not about.
                if let Some(var) = &shadow {
                    ui.label(
                        egui::RichText::new(format!(
                            "the row was written — the EFFECTIVE value is still {var}'s until \
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
                // ⚠ WRAPPED. The daemon's refusals arrive as its own sentences — the loader's
                // message, verbatim — not as codes. One unwrapped line is one unread line.
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
    panel.response
}

/// Which row's editor the section last scrolled into view — so it does that ONCE per opening.
fn editor_shown_id() -> egui::Id {
    egui::Id::new("backend-settings-editor-shown")
}

#[path = "backend_settings_tests.rs"]
#[cfg(test)]
mod backend_settings_tests;
