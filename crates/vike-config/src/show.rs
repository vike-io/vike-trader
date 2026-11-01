//! **The rendered, redacted settings-row rows** — the ROWS half of `vike-cli config show`, MOVED
//! down from `crates/vike-cli/src/cmd/config.rs` (where its printers still live) so a second
//! disclosure surface could consume the SAME builder instead of a copy.
//!
//! The second surface is the tradehub wire (split-plane REQ-7, the read half):
//! `vike_tradehub` answers `Request::SettingsShow` with exactly these rows, so a GUI's
//! Backend-Settings page and the CLI's table can never disagree about a value, an origin, or —
//! the part that matters — a redaction. The same argument that moved [`crate::redact`]'s shapes
//! down applies to the builder that APPLIES them: a security-relevant transformation duplicated
//! per surface means a rule fixed in one copy and not the other prints a live credential into
//! whichever surface was forgotten. One home, every surface imports it.
//!
//! What deliberately did NOT move: the printers (`crates/vike-cli/src/cmd/config/show_human.rs`'s
//! `print_file_table` and `settings_json`) — rendering is each surface's own job — and the ENV
//! half (`Resolved`, built over `vike_config::settings::SETTINGS`): the registry sat in `vike-ops`,
//! which this crate could not see, until 2026-10-12, and `Resolved` stays in `vike-cli` with its
//! printers.
//!
//! INVARIANT (the reason this module exists): redaction happens ON CONSTRUCTION, in
//! [`resolve_file_row`], so no printer — CLI table, `--json`, wire frame, GUI grid — can leak a
//! secret-shaped value. `a_secret_settings_value_never_enters_the_file_row` below pins it.

use crate::consumed::{self, ReadStatus};
use crate::provenance::{Description, Origin, ResolvedSetting};
use crate::redact::{REDACTED, SET, UNSET, is_secret_key};

/// One settings row, redacted and rendered — the row twin of `vike-cli`'s env-half `Resolved`, and
/// the same invariant for the same reason: redaction happens here, on construction, so no printer
/// can leak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// The dotted key (`config.tradehub_addr`) — its first segment is the [`crate::Settings`]
    /// section.
    pub key: String,
    /// The settings SECTION this key belongs to (`policy`, `config`, …) — carried through from
    /// [`ResolvedSetting::section`].
    ///
    /// ⚠ A bare section word, not a FILE name; the field keeps its name because every consumer
    /// (the CLI table, `--json`, the tradehub wire, the GUI grid) already keys on it.
    pub file: &'static str,
    /// The layer that set it, as one cell (`default`, `db`).
    pub origin: String,
    /// The machine word: `default` / `db`.
    ///
    /// ⚠ **This domain held `"file"` and `"env"` too, until `docs/decisions/0086` retired the
    /// settings-file layer and decision 0111 the environment one.** `Origin::kind` is the authority.
    pub origin_kind: &'static str,

    /// The effective value, rendered — ALREADY redacted when `secret`.
    pub value: String,
    /// The compiled-in default, rendered — ALREADY redacted when `secret`.
    pub default: String,
    /// The effective value is not what the winning layer holds — a later rule moved it. No such
    /// rule exists today (see [`ResolvedSetting::adjusted`]).
    pub adjusted: bool,
    /// The key is credential-shaped ([`is_secret_key`]) and `value`/`default` are the redaction
    /// sentinels, never store bytes.
    pub secret: bool,
    /// What the consumption tables say about the key ([`consumed::read_status`]) — the ONE answer
    /// the four fields below are derived from, kept whole so a surface can tell "nothing reads it"
    /// from "no table says".
    pub read_status: ReadStatus,
    /// Whether a consumption table says something OUTSIDE the settings system reads this key and
    /// acts on it ([`crate::is_consumed`]). ⚠ `false` for an UNKNOWN key as well as an unread one:
    /// a key no table carries is not a consumed key ([`ReadStatus::Unknown`]).
    pub consumed: bool,
    /// WHICH BINARY reads it ([`crate::Consumer::binary`]) — `None` for an unread key, and
    /// also for a library consumer, whose read belongs to every binary that links it, and for a
    /// `policy.*` ceiling.
    pub read_by: Option<&'static str>,
    /// For an unread or unknown key, why nothing reads it (or why the tables say nothing). `None`
    /// when the key IS consumed. Printed under the table rather than as a column — it is a
    /// paragraph, not a cell.
    pub why_unread: Option<&'static str>,
    /// For an unread or unknown key, the ONE-LINE verdict
    /// ([`crate::Reader::verdict`], [`consumed::UNENFORCED_POLICY_VERDICT`] or
    /// [`consumed::UNKNOWN_VERDICT`]) — printed above [`Self::why_unread`], which is a paragraph an
    /// operator may not read to the end of.
    pub unread_verdict: Option<&'static str>,
}

impl FileRow {
    /// The `READ` cell: the consuming BINARY when one program owns the read, `yes` when a library
    /// does (no single honest name), `NO` when nothing reads it at all, and `?` when no table says.
    pub fn read_cell(&self) -> &str {
        match self.read_status {
            ReadStatus::Read { binary: Some(binary) } => binary,
            ReadStatus::Read { binary: None } => "yes",
            ReadStatus::Unread { .. } => "NO",
            ReadStatus::Unknown => "?",
        }
    }
}

/// Render + redact one [`ResolvedSetting`]. `None` (an unset `Option` field) prints
/// empty, which the CLI's human table renders as `-` and machine consumers keep as `""`.
pub fn resolve_file_row(row: &ResolvedSetting) -> FileRow {
    let secret = is_secret_key(&row.key);
    let (value, default) = if secret {
        let set =
            row.origin != Origin::Default && row.value.as_deref().is_some_and(|v| !v.is_empty());
        let default = if row.default.is_none() { String::new() } else { REDACTED.to_string() };
        ((if set { SET } else { UNSET }).to_string(), default)
    } else {
        (row.value.clone().unwrap_or_default(), row.default.clone().unwrap_or_default())
    };
    // ⚠ ONE question, three answers (`consumed::read_status`). This used to ask only
    // `CONSUMPTION` and read "no row" as "consumed" — "a `policy.*` key has its own gate" — so
    // `policy.max_leverage`, which nothing enforced at the time, printed `READ: yes`. A policy key has a
    // shipped verdict now (`consumed::POLICY_VERDICTS`), and a key NO table carries answers
    // `Unknown` and says so, rather than inheriting a "yes".
    let read_status = consumed::read_status(&row.key);
    let (is_read, read_by, why_unread, unread_verdict) = match read_status {
        ReadStatus::Read { binary } => (true, binary, None, None),
        ReadStatus::Unread { verdict, why } => (false, None, Some(why), Some(verdict)),
        ReadStatus::Unknown => {
            (false, None, Some(consumed::UNKNOWN_WHY), Some(consumed::UNKNOWN_VERDICT))
        }
    };
    FileRow {
        key: row.key.clone(),
        file: row.section,
        origin: row.origin.label(),
        origin_kind: row.origin.kind(),
        value,
        default,
        adjusted: row.adjusted,
        secret,
        read_status,
        consumed: is_read,
        read_by,
        why_unread,
        unread_verdict,
    }
}

/// The rows half, filtered and rendered. `filter` is a case-insensitive substring match on the
/// dotted key or the settings section; `changed_only` drops every row nothing configured.
pub fn file_rows(d: &Description, filter: Option<&str>, changed_only: bool) -> Vec<FileRow> {
    let needle = filter.map(str::to_ascii_lowercase);
    d.rows
        .iter()
        .filter(|r| match needle.as_deref() {
            None => true,
            Some(n) => r.key.to_ascii_lowercase().contains(n) || r.section.contains(n),
        })
        .filter(|r| !changed_only || r.origin != Origin::Default)
        .map(resolve_file_row)
        .collect()
}

#[path = "show_tests.rs"]
#[cfg(test)]
mod show_tests;
