//! What the panel may SAY: the row classifiers, the table filter and the digest.

use vike_tradehub_client::wire::WireSettingsRow;

use super::{BackendSettingsState, PREDATES_SETTINGS_SHOW};

#[cfg(doc)]
use super::{
    editor::settings_edit_panel, section::backend_settings_section, should_fetch_settings,
};

// ---------------------------------------------------------------------------------------------
// What the panel may SAY — the derivations, each one a fold over rows that are already on screen
// ---------------------------------------------------------------------------------------------

/// The ORIGIN cell's stable machine word, re-derived from the human label the wire carries.
///
/// ⚠ **This is a SECOND parser of a value the producer already computed, and it exists because
/// the wire drops the first.** `vike_config::Origin::kind()` returns exactly this set
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
/// default? Measured exactly the way `vike_config::describe` measures it: by key
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
